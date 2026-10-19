//! RTF read into the model Word documents are laid out from: sections
//! with their page setup, headers and footers; paragraphs with their
//! formatting (list labels are the text Word writes for them); tables;
//! and PNG and JPEG pictures in the line.

use std::collections::HashMap;
use std::sync::Arc;

use encoding_rs::Encoding;

use crate::codepage::codepage;
use crate::docx::read::{Block, Cell, DocModel, PAGE, PAGES, Para, Row, Section, Table};
use crate::docx::{Doc, Source};
use crate::draw::Rgba;
use crate::typeset::{Align, LineRule, Object, ParaFormat, Span, Style, Tab, TabAlign};
use crate::{Error, Fonts, Limits};

/// Twips in a point.
const TW: f32 = 20.0;
const MAX_BLOCKS: usize = 200_000;
/// Groups nested deeper are left out.
const MAX_DEPTH: usize = 400;
const MAX_FONTS: usize = 4096;
const MAX_COLORS: usize = 4096;
const MAX_CELLS: usize = 256;
const MAX_SECTIONS: usize = 10_000;

pub(crate) fn open(data: Vec<u8>, limits: &Limits, fonts: &Fonts) -> Result<Doc, Error> {
    let (model, blobs) = read(&data, limits)?;
    Ok(Doc::laid_out(Source::Blobs(blobs), &model, limits, fonts))
}

// --- tokens --------------------------------------------------------------

enum Tok<'a> {
    Open,
    Close,
    Word(&'a str, Option<i32>),
    /// A control symbol (`\~`, `\*`...).
    Sym(u8),
    /// A byte of text in the document's code page (`\'hh` or plain).
    Byte(u8),
    Bin,
    Eof,
}

struct Lexer<'a> {
    d: &'a [u8],
    i: usize,
}

impl<'a> Lexer<'a> {
    fn next(&mut self) -> Tok<'a> {
        loop {
            let Some(&b) = self.d.get(self.i) else { return Tok::Eof };
            self.i += 1;
            return match b {
                b'{' => Tok::Open,
                b'}' => Tok::Close,
                b'\r' | b'\n' | 0 => continue,
                b'\\' => self.control(),
                _ => Tok::Byte(b),
            };
        }
    }

    fn control(&mut self) -> Tok<'a> {
        let d = self.d;
        let Some(&c) = d.get(self.i) else { return Tok::Eof };
        if !c.is_ascii_alphabetic() {
            self.i += 1;
            return match c {
                b'\'' => {
                    let h = d.get(self.i..self.i + 2).and_then(|h| std::str::from_utf8(h).ok()).and_then(|h| u8::from_str_radix(h, 16).ok());
                    if h.is_some() {
                        self.i += 2;
                    }
                    h.map_or(Tok::Sym(b'?'), Tok::Byte)
                }
                b'\\' | b'{' | b'}' => Tok::Byte(c),
                b'\r' | b'\n' => Tok::Word("par", None),
                b'\t' => Tok::Word("tab", None),
                _ => Tok::Sym(c),
            };
        }
        let s = self.i;
        while d.get(self.i).is_some_and(u8::is_ascii_alphabetic) && self.i - s < 32 {
            self.i += 1;
        }
        let word = std::str::from_utf8(&d[s..self.i]).unwrap_or("");
        let neg = d.get(self.i) == Some(&b'-') && d.get(self.i + 1).is_some_and(u8::is_ascii_digit);
        if neg {
            self.i += 1;
        }
        let mut num: Option<i64> = None;
        while let Some(&c) = d.get(self.i).filter(|c| c.is_ascii_digit()) {
            num = Some((num.unwrap_or(0) * 10 + (c - b'0') as i64).min(i32::MAX as i64));
            self.i += 1;
        }
        let num = num.map(|n| if neg { -n } else { n } as i32);
        if d.get(self.i) == Some(&b' ') {
            self.i += 1;
        }
        if word == "bin" {
            let n = num.unwrap_or(0).max(0) as usize;
            self.i = self.i.saturating_add(n).min(d.len());
            return Tok::Bin;
        }
        Tok::Word(word, num)
    }
}

// --- state -----------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Dest {
    Text,
    Skip,
    FontTbl,
    ColorTbl,
    FldInst,
    FldRslt,
    Pict,
    /// A header or footer: 0 header, 1 footer, 2 and 3 the first page's.
    Hf(u8),
}

#[derive(Clone, PartialEq)]
struct Chr {
    font: i32,
    /// Half points.
    fs: i32,
    bold: bool,
    italic: bool,
    underline: bool,
    strike: bool,
    color: i32,
    highlight: i32,
    shift: i8,
    caps: bool,
    hidden: bool,
}

impl Default for Chr {
    fn default() -> Chr {
        Chr { font: -1, fs: 24, bold: false, italic: false, underline: false, strike: false, color: 0, highlight: 0, shift: 0, caps: false, hidden: false }
    }
}

#[derive(Clone, Default)]
struct Pp {
    align: Align,
    li: i32,
    ri: i32,
    fi: i32,
    sb: i32,
    sa: i32,
    sl: i32,
    slmult: bool,
    tabs: Vec<Tab>,
    tab_align: Option<TabAlign>,
    leader: Option<char>,
    intbl: bool,
    pagebb: bool,
    shading: i32,
}

#[derive(Clone)]
struct Group {
    dest: Dest,
    chr: Chr,
    pp: Pp,
    uc: usize,
    /// Text in the group is dropped (shape properties), but pictures in it
    /// are kept.
    hide: bool,
}

type Border = Option<(f32, Rgba)>;

#[derive(Clone, Default)]
struct CellDef {
    right: i32,
    hmerge: bool,
    vmerge: bool,
    fill: i32,
    valign: u8,
    borders: [Border; 4],
    bordered: bool,
}

#[derive(Clone, Default)]
struct RowDef {
    cells: Vec<CellDef>,
    left: i32,
    align: Align,
    height: i32,
    header: bool,
    gap: Option<i32>,
    borders: [Border; 4],
}

/// Where `\brdrw`, `\brdrcf` and `\brdrnone` go: a cell's or a row's side.
#[derive(Clone, Copy)]
enum Edge {
    Cell(usize),
    Row(usize),
}

/// A body or header being read.
#[derive(Default)]
struct Story {
    blocks: Vec<Block>,
    spans: Vec<Span>,
    page_break: bool,
    /// The current table: rows and their cells' contents.
    rows: Vec<(RowDef, Vec<Vec<Block>>)>,
    cells: Vec<Vec<Block>>,
    cell: Vec<Block>,
}

struct FieldState {
    depth: usize,
    instr: String,
    /// 1 page number, 2 page count.
    kind: u8,
}

#[derive(Default)]
struct Pict {
    data: Vec<u8>,
    half: Option<u8>,
    known: bool,
    w: i32,
    h: i32,
    wgoal: i32,
    hgoal: i32,
    sx: i32,
    sy: i32,
}

struct FontDef {
    name: Arc<str>,
    enc: Option<&'static Encoding>,
    /// 1 Symbol, 2 Wingdings: their bytes are pictures, not letters.
    symbol: u8,
}

#[derive(Clone)]
struct Sect {
    width: f32,
    height: f32,
    margins: [f32; 4],
    header_dist: f32,
    footer_dist: f32,
    hf: [Option<Arc<Vec<Block>>>; 4],
    page_start: Option<usize>,
    restart: bool,
    title_page: bool,
    new_page: bool,
}

impl Default for Sect {
    fn default() -> Sect {
        Sect { width: 612.0, height: 792.0, margins: [72.0, 90.0, 72.0, 90.0], header_dist: 36.0, footer_dist: 36.0, hf: Default::default(), page_start: None, restart: false, title_page: false, new_page: true }
    }
}

struct Reader {
    stack: Vec<Group>,
    /// Groups opened past `MAX_DEPTH`, left out.
    deep: usize,
    stories: Vec<Story>,
    fonts: HashMap<i32, FontDef>,
    font_num: i32,
    font_name: String,
    font_charset: i32,
    deff: i32,
    colors: Vec<Option<Rgba>>,
    color: [Option<u8>; 3],
    enc: &'static Encoding,
    /// Bytes of text waiting to be decoded, and their encoding.
    bytes: Vec<u8>,
    bytes_enc: Option<&'static Encoding>,
    bytes_symbol: u8,
    /// `\uN` fallback characters left to skip.
    skip: usize,
    high: Option<u16>,
    star: bool,
    doc_sect: Sect,
    sect: Sect,
    sections: Vec<Section>,
    row: RowDef,
    cell_def: CellDef,
    edge: Option<Edge>,
    fields: Vec<FieldState>,
    pict: Option<Pict>,
    blobs: HashMap<Arc<str>, Vec<u8>>,
    blob_bytes: u64,
    blocks: usize,
    style: Option<(Chr, Arc<Style>)>,
    max_part: u64,
    max_total: u64,
}

/// The encoding of a font's `\fcharset` (none: the document's).
fn charset(cs: i32) -> Option<&'static Encoding> {
    codepage(match cs {
        77 => 10000,
        128 => 932,
        129 => 949,
        134 => 936,
        136 => 950,
        161 => 1253,
        162 => 1254,
        163 => 1258,
        177 => 1255,
        178 => 1256,
        186 => 1257,
        204 => 1251,
        222 => 874,
        238 => 1250,
        _ => return None,
    })
}

/// A Symbol or Wingdings character as the nearest Unicode one.
fn symbol(kind: u8, b: u8) -> char {
    match (kind, b) {
        (_, 0xB7) => '•',
        (2, 0x6C) => '●',
        (2, 0x6E | 0xA7) => '■',
        (2, 0x71 | 0x6F) => '□',
        (2, 0x76) => '❖',
        (2, 0xD8) => '➢',
        (2, 0xFC) => '✓',
        (2, 0xFB) => '✗',
        (2, 0xE0 | 0xE8) => '➔',
        (2, 0x9F) => '•',
        (2, _) => '•',
        (_, 0xA7) => '♣',
        (_, 0xA8) => '♦',
        (_, 0xA9) => '♥',
        (_, 0xAA) => '♠',
        (_, 0xAE) => '→',
        (_, 0xB0) => '°',
        (_, 0xB1) => '±',
        (_, 0xB4) => '×',
        (_, 0xB8) => '÷',
        (_, 0xA3) => '≤',
        (_, 0xB3) => '≥',
        (_, 0xB9) => '≠',
        (_, 0xD6) => '√',
        (_, 0xA5) => '∞',
        (_, 0x61..=0x7A) => {
            const GREEK: &str = "αβχδεφγηιϕκλμνοπθρστυϖωξψζ";
            GREEK.chars().nth((b - 0x61) as usize).unwrap_or('?')
        }
        _ => b as char,
    }
}

impl Reader {
    fn top(&mut self) -> &mut Group {
        if self.stack.is_empty() {
            self.stack.push(Group { dest: Dest::Text, chr: Chr::default(), pp: Pp::default(), uc: 1, hide: false });
        }
        let n = self.stack.len() - 1;
        &mut self.stack[n]
    }

    fn story(&mut self) -> &mut Story {
        if self.stories.is_empty() {
            self.stories.push(Story::default());
        }
        let n = self.stories.len() - 1;
        &mut self.stories[n]
    }

    fn color(&self, i: i32) -> Option<Rgba> {
        usize::try_from(i).ok().and_then(|i| self.colors.get(i)).copied().flatten()
    }

    fn style(&mut self) -> Arc<Style> {
        let c = self.top().chr.clone();
        if let Some((k, s)) = &self.style
            && *k == c
        {
            return s.clone();
        }
        let family = self.fonts.get(&if c.font >= 0 { c.font } else { self.deff }).map(|f| f.name.clone()).filter(|n| !n.is_empty()).unwrap_or_else(|| Arc::from("Times New Roman"));
        let s = Arc::new(Style {
            family,
            size: c.fs as f32 / 2.0,
            bold: c.bold,
            italic: c.italic,
            color: if c.color > 0 { self.color(c.color).unwrap_or(Rgba::BLACK) } else { Rgba::BLACK },
            underline: c.underline,
            strike: c.strike,
            highlight: if c.highlight > 0 { self.color(c.highlight) } else { None },
            shift: c.shift,
            caps: c.caps,
        });
        self.style = Some((c, s.clone()));
        s
    }

    /// The encoding and symbol kind of the current font.
    fn font_enc(&mut self) -> (Option<&'static Encoding>, u8) {
        let f = self.top().chr.font;
        let f = if f >= 0 { f } else { self.deff };
        self.fonts.get(&f).map_or((None, 0), |f| (f.enc, f.symbol))
    }

    fn byte(&mut self, b: u8) {
        if self.skip > 0 {
            self.skip -= 1;
            return;
        }
        let dest = self.top().dest;
        match dest {
            Dest::Skip => return,
            Dest::Pict => {
                if let Some(p) = &mut self.pict {
                    let v = match b {
                        b'0'..=b'9' => b - b'0',
                        b'a'..=b'f' => b - b'a' + 10,
                        b'A'..=b'F' => b - b'A' + 10,
                        _ => return,
                    };
                    if p.data.len() as u64 >= self.max_part {
                        return;
                    }
                    match p.half.take() {
                        Some(h) => p.data.push(h << 4 | v),
                        None => p.half = Some(v),
                    }
                }
                return;
            }
            _ => {}
        }
        let (enc, sym) = if dest == Dest::FontTbl { (None, 0) } else { self.font_enc() };
        if !self.bytes.is_empty() && (self.bytes_enc != enc || self.bytes_symbol != sym) {
            self.flush();
        }
        self.bytes_enc = enc;
        self.bytes_symbol = sym;
        if self.bytes.len() < 1 << 20 {
            self.bytes.push(b);
        }
    }

    /// Decodes the bytes of text waiting.
    fn flush(&mut self) {
        if self.bytes.is_empty() {
            return;
        }
        let bytes = std::mem::take(&mut self.bytes);
        let s = if self.bytes_symbol > 0 {
            bytes.iter().map(|&b| symbol(self.bytes_symbol, b)).collect::<String>()
        } else {
            self.bytes_enc.unwrap_or(self.enc).decode_without_bom_handling(&bytes).0.into_owned()
        };
        self.bytes = bytes;
        self.bytes.clear();
        self.text(&s);
    }

    fn text(&mut self, s: &str) {
        let g = self.top().clone_head();
        match g.0 {
            Dest::Text | Dest::Hf(_) | Dest::FldRslt => {}
            Dest::FontTbl => {
                for c in s.chars() {
                    if c == ';' {
                        self.end_font();
                    } else if self.font_name.len() < 256 {
                        self.font_name.push(c);
                    }
                }
                return;
            }
            Dest::ColorTbl => {
                for c in s.chars() {
                    if c == ';' && self.colors.len() < MAX_COLORS {
                        let [r, g, b] = self.color;
                        self.colors.push((r.is_some() || g.is_some() || b.is_some()).then(|| Rgba([r.unwrap_or(0), g.unwrap_or(0), b.unwrap_or(0), 255])));
                        self.color = [None; 3];
                    }
                }
                return;
            }
            Dest::FldInst => {
                if let Some(f) = self.fields.last_mut()
                    && f.instr.len() < 4096
                {
                    f.instr.push_str(s);
                }
                return;
            }
            _ => return,
        }
        if g.1 || g.2 {
            return;
        }
        if g.0 == Dest::FldRslt && self.fields.last().is_some_and(|f| f.kind != 0) {
            return;
        }
        self.push(s);
    }

    fn push(&mut self, s: &str) {
        let style = self.style();
        let st = self.story();
        if let Some(last) = st.spans.last_mut()
            && last.object.is_none()
            && Arc::ptr_eq(&last.style, &style)
        {
            if last.text.len() < 1 << 20 {
                last.text.push_str(s);
            }
            return;
        }
        if st.spans.len() < 100_000 {
            st.spans.push(Span::new(s, style));
        }
    }

    fn put_char(&mut self, c: char) {
        self.flush();
        let mut b = [0u8; 4];
        self.text(c.encode_utf8(&mut b));
    }

    fn unicode(&mut self, n: i32) {
        self.flush();
        let u = if n < 0 { (n + 65536).max(0) as u32 } else { n as u32 };
        self.skip = self.top().uc;
        if (0xD800..0xDC00).contains(&u) {
            self.high = Some(u as u16);
            return;
        }
        let c = match self.high.take() {
            Some(h) if (0xDC00..0xE000).contains(&u) => char::decode_utf16([h, u as u16]).next().and_then(Result::ok),
            _ if (0xF020..0xF100).contains(&u) && self.font_enc().1 > 0 => Some(symbol(self.font_enc().1, (u - 0xF000) as u8)),
            _ => char::from_u32(u),
        };
        if let Some(c) = c {
            let mut b = [0u8; 4];
            self.text(c.encode_utf8(&mut b));
        }
    }

    fn end_font(&mut self) {
        let name = std::mem::take(&mut self.font_name);
        let name = name.trim();
        if self.fonts.len() < MAX_FONTS && !(name.is_empty() && self.fonts.contains_key(&self.font_num)) {
            let lower = name.to_ascii_lowercase();
            let symbol = if lower.starts_with("wingdings") || lower.starts_with("webdings") {
                2
            } else if self.font_charset == 2 || lower == "symbol" {
                1
            } else {
                0
            };
            self.fonts.insert(self.font_num, FontDef { name: Arc::from(name), enc: charset(self.font_charset), symbol });
        }
        self.font_charset = 0;
    }

    /// Ends the paragraph being read (and the cell, when `cell`).
    fn end_para(&mut self, cell: bool) {
        self.flush();
        let pp = self.top().pp.clone();
        let style = self.style();
        let shading = if pp.shading > 0 { self.color(pp.shading) } else { None };
        let mut spans = std::mem::take(&mut self.story().spans);
        if spans.is_empty() {
            spans.push(Span::new("", style));
        }
        if self.blocks >= MAX_BLOCKS {
            return;
        }
        self.blocks += 1;
        let st = self.story();
        let line = match pp.sl {
            0 => LineRule::Multiple(1.0),
            n if pp.slmult => LineRule::Multiple((n.unsigned_abs() as f32 / 240.0).clamp(0.25, 10.0)),
            n if n < 0 => LineRule::Exact(n.unsigned_abs() as f32 / TW),
            n => LineRule::AtLeast(n as f32 / TW),
        };
        let page_break = std::mem::take(&mut st.page_break);
        let mut fmt = ParaFormat { align: pp.align, left: pp.li as f32 / TW, right: pp.ri as f32 / TW, first: pp.fi as f32 / TW, line, tabs: pp.tabs };
        // A hanging indent's first tab goes to the indent.
        if fmt.first < 0.0 && !fmt.tabs.iter().any(|t| t.pos == fmt.left) {
            fmt.tabs.push(Tab { pos: fmt.left, align: TabAlign::Left, leader: None });
            crate::typeset::sort_tabs(&mut fmt.tabs);
        }
        let para = Para {
            fmt,
            spans,
            before: (pp.sb as f32 / TW).max(0.0),
            after: (pp.sa as f32 / TW).max(0.0),
            page_break_before: pp.pagebb || page_break,
            break_after: false,
            shading,
            contextual: false,
            style: String::new(),
            floats: Vec::new(),
            section_end: None,
        };
        if pp.intbl || cell {
            st.cell.push(Block::Para(para));
            if cell {
                let c = std::mem::take(&mut st.cell);
                st.cells.push(c);
            }
        } else {
            self.flush_table();
            self.story().blocks.push(Block::Para(para));
        }
    }

    /// Turns the rows read into a table.
    fn flush_table(&mut self) {
        let row = self.row.clone();
        let story = self.story();
        if !story.cells.is_empty() || !story.cell.is_empty() {
            // A row without `\row`.
            let mut cells = std::mem::take(&mut story.cells);
            if !story.cell.is_empty() {
                cells.push(std::mem::take(&mut story.cell));
            }
            story.rows.push((row, cells));
        }
        if story.rows.is_empty() {
            return;
        }
        let rows = std::mem::take(&mut story.rows);
        let align = rows[0].0.align;
        // Column edges from all the rows.
        let mut edges: Vec<i32> = Vec::new();
        for (d, _) in &rows {
            edges.push(d.left);
            edges.extend(d.cells.iter().map(|c| c.right));
        }
        edges.sort_unstable();
        edges.dedup_by(|a, b| (*a - *b).abs() < 30);
        if edges.len() < 2 {
            let n = rows.iter().map(|r| r.1.len()).max().unwrap_or(1).clamp(1, MAX_CELLS) as i32;
            edges = (0..=n).map(|i| i * 1440).collect();
        }
        edges.truncate(MAX_CELLS + 1);
        let ncols = edges.len() - 1;
        let col_of = |x: i32| {
            let i = edges.partition_point(|e| *e < x);
            if i > 0 && (i == edges.len() || x - edges[i - 1] <= edges[i] - x) { i - 1 } else { i.min(edges.len() - 1) }
        };
        let colors = &self.colors;
        let color = |i: i32| usize::try_from(i).ok().and_then(|i| colors.get(i)).copied().flatten();
        let mut out = Vec::with_capacity(rows.len());
        for (d, cells) in rows {
            let gap = d.gap.unwrap_or(108).clamp(0, 1440) as f32 / TW;
            let mut cells = cells.into_iter();
            let mut row: Vec<Cell> = Vec::new();
            let mut x = d.left;
            let mut at = 0;
            let defs = if d.cells.is_empty() { vec![CellDef { right: d.left + 1440, ..Default::default() }; cells.len().clamp(1, MAX_CELLS)] } else { d.cells.clone() };
            for (i, c) in defs.iter().enumerate() {
                let right = if d.cells.is_empty() { x + 1440 } else { c.right };
                let (c0, c1) = (col_of(x).max(at), col_of(right));
                x = right;
                let blocks = cells.next().unwrap_or_default();
                if c0 >= ncols {
                    continue;
                }
                let c1 = c1.max(c0 + 1).min(ncols);
                if c0 > at {
                    // Space before the cell: an empty cell of none.
                    row.push(Cell { span: c0 - at, merged: false, blocks: Vec::new(), fill: None, borders: [None; 4], margins: [0.0; 4], valign: 0 });
                }
                at = c1;
                if c.hmerge && i > 0
                    && let Some(prev) = row.last_mut()
                {
                    prev.span += c1 - c0;
                    continue;
                }
                let borders = if c.bordered { c.borders } else { d.borders };
                row.push(Cell { span: c1 - c0, merged: c.vmerge, blocks, fill: color(c.fill), borders, margins: [0.0, gap, 0.0, gap], valign: c.valign });
            }
            if row.is_empty() {
                continue;
            }
            out.push(Row { cells: row, height: d.height.unsigned_abs().min(31_680) as f32 / TW, exact: d.height < 0, header: d.header });
        }
        if out.is_empty() {
            return;
        }
        let indent = (edges[0] as f32 / TW).clamp(-720.0, 720.0);
        let table = Table { cols: edges.windows(2).map(|w| ((w[1] - w[0]) as f32 / TW).max(1.0)).collect(), rows: out, indent, align };
        self.story().blocks.push(Block::Table(table));
    }

    /// The section read so far, and where it ends.
    fn end_section(&mut self) {
        if self.stories.len() != 1 || self.sections.len() >= MAX_SECTIONS {
            return;
        }
        if !self.story().spans.is_empty() {
            self.end_para(false);
        }
        self.flush_table();
        let end = self.sections.len();
        let s = self.section();
        self.sections.push(s);
        let blocks = &mut self.stories[0].blocks;
        match blocks.last_mut() {
            Some(Block::Para(p)) if p.section_end.is_none() => p.section_end = Some(end),
            _ => {
                let mut marker = Para {
                    fmt: ParaFormat::default(),
                    spans: vec![Span::new("", Arc::new(Style { size: 0.5, ..Style::default() }))],
                    before: 0.0,
                    after: 0.0,
                    page_break_before: false,
                    break_after: false,
                    shading: None,
                    contextual: false,
                    style: String::new(),
                    floats: Vec::new(),
                    section_end: Some(end),
                };
                marker.fmt.line = LineRule::Exact(0.1);
                blocks.push(Block::Para(marker));
            }
        }
    }

    fn section(&mut self) -> Section {
        let s = &mut self.sect;
        let (mut width, mut height) = (s.width, s.height);
        if !(width > 72.0 && width < 14_400.0) {
            width = 612.0;
        }
        if !(height > 72.0 && height < 14_400.0) {
            height = 792.0;
        }
        let mut m = s.margins.map(|m| m.clamp(0.0, 1440.0));
        if m[1] + m[3] > width - 72.0 {
            m[1] = 18.0;
            m[3] = 18.0;
        }
        if m[0] + m[2] > height - 72.0 {
            m[0] = 18.0;
            m[2] = 18.0;
        }
        let page_start = if s.restart { Some(s.page_start.unwrap_or(1)) } else { None };
        s.restart = false;
        let [header, footer, first_header, first_footer] = s.hf.clone();
        Section {
            width,
            height,
            margins: m,
            header_dist: s.header_dist.clamp(0.0, 720.0),
            footer_dist: s.footer_dist.clamp(0.0, 720.0),
            header,
            footer,
            first_header: if s.title_page { first_header.or(Some(Arc::new(Vec::new()))) } else { None },
            first_footer: if s.title_page { first_footer.or(Some(Arc::new(Vec::new()))) } else { None },
            page_start,
            title_page: s.title_page,
            new_page: s.new_page,
        }
    }

    /// Sets the side of a border being described.
    fn border(&mut self, f: impl FnOnce(&mut Border)) {
        let side = match self.edge {
            Some(Edge::Cell(i)) => self.cell_def.borders.get_mut(i),
            Some(Edge::Row(i)) => self.row.borders.get_mut(i),
            None => None,
        };
        if let Some(b) = side {
            f(b);
        }
    }

    fn word(&mut self, w: &str, n: Option<i32>) {
        let star = std::mem::take(&mut self.star);
        let v = n.unwrap_or(1);
        let on = n != Some(0);
        let dest = self.top().dest;
        if dest == Dest::Skip {
            return;
        }
        if w != "u" {
            if self.skip > 0 {
                self.skip -= 1;
                return;
            }
            self.flush();
        }
        // Destinations.
        let new_dest = match w {
            "fonttbl" => Some(Dest::FontTbl),
            "colortbl" => Some(Dest::ColorTbl),
            "pict" => {
                self.pict = Some(Pict { sx: 100, sy: 100, ..Default::default() });
                Some(Dest::Pict)
            }
            "shppict" | "shp" | "shpinst" | "shptxt" | "listtext" | "pntext" | "result" | "object" | "upr" => Some(dest),
            "sv" => {
                self.top().hide = true;
                None
            }
            "field" => {
                let depth = self.stack.len();
                if self.fields.len() < 64 {
                    self.fields.push(FieldState { depth, instr: String::new(), kind: 0 });
                }
                None
            }
            "fldinst" => Some(Dest::FldInst),
            "fldrslt" => Some(Dest::FldRslt),
            "header" | "headerr" | "footer" | "footerr" | "headerf" | "footerf" if self.stories.len() == 1 => {
                self.stories.push(Story::default());
                Some(Dest::Hf(match w {
                    "header" | "headerr" => 0,
                    "footer" | "footerr" => 1,
                    "headerf" => 2,
                    _ => 3,
                }))
            }
            "info" | "stylesheet" | "listtable" | "listoverridetable" | "footnote" | "headerl" | "footerl" | "header" | "footer" | "headerr" | "footerr" | "headerf" | "footerf" | "nonshppict" | "shprslt" | "sn" | "pn" | "fldtype" | "datafield" | "formfield" | "themedata"
            | "colorschememapping" | "latentstyles" | "datastore" | "xmlnstbl" | "rsidtbl" | "generator" | "mmathPr" | "pgdsctbl" | "listpicture" | "revtbl" | "annotation" | "atnid" | "atnauthor" | "xe" | "tc" | "objdata" | "nesttableprops" | "userprops" | "docvar"
            | "wgrffmtfilter" | "filetbl" | "pnseclvl" | "template" | "ftnsep" | "ftnsepc" | "aftnsep" | "aftnsepc" | "do" | "background" | "bkmkstart" | "bkmkend" | "comment" | "txe" | "private" => Some(Dest::Skip),
            _ => None,
        };
        if let Some(d) = new_dest {
            let g = self.top();
            g.dest = d;
            if matches!(d, Dest::Hf(_)) {
                g.pp = Pp::default();
                g.chr = Chr::default();
            }
            return;
        }
        if star {
            // An unknown optional destination.
            self.top().dest = Dest::Skip;
            return;
        }
        match dest {
            Dest::FontTbl => {
                match w {
                    "f" => {
                        if !self.font_name.trim().is_empty() {
                            self.end_font();
                        }
                        self.font_num = v;
                        self.font_name.clear();
                        self.font_charset = 0;
                    }
                    "fcharset" => self.font_charset = v,
                    "u" => self.unicode(v),
                    _ => {}
                }
                return;
            }
            Dest::ColorTbl => {
                let i = match w {
                    "red" => 0,
                    "green" => 1,
                    "blue" => 2,
                    _ => return,
                };
                self.color[i] = Some(v.clamp(0, 255) as u8);
                return;
            }
            Dest::Pict => {
                if let Some(p) = &mut self.pict {
                    match w {
                        "pngblip" | "jpegblip" => p.known = true,
                        "picw" => p.w = v,
                        "pich" => p.h = v,
                        "picwgoal" => p.wgoal = v,
                        "pichgoal" => p.hgoal = v,
                        "picscalex" => p.sx = v,
                        "picscaley" => p.sy = v,
                        _ => {}
                    }
                }
                return;
            }
            Dest::Text | Dest::Hf(_) | Dest::FldRslt => {}
            _ => {
                if w == "u" {
                    self.unicode(v);
                }
                return;
            }
        }
        match w {
            // Document.
            "ansicpg" => self.enc = codepage(v).unwrap_or(self.enc),
            "mac" => self.enc = encoding_rs::MACINTOSH,
            "deff" => self.deff = v,
            "paperw" => self.doc_sect.width = v as f32 / TW,
            "paperh" => self.doc_sect.height = v as f32 / TW,
            "margl" => self.doc_sect.margins[3] = v as f32 / TW,
            "margr" => self.doc_sect.margins[1] = v as f32 / TW,
            "margt" => self.doc_sect.margins[0] = v as f32 / TW,
            "margb" => self.doc_sect.margins[2] = v as f32 / TW,
            "uc" => self.top().uc = v.clamp(0, 10) as usize,
            "u" => self.unicode(v),
            // Sections.
            "sectd" => {
                let hf = self.sect.hf.clone();
                self.sect = Sect { hf, ..self.doc_sect.clone() };
            }
            "pgwsxn" => self.sect.width = v as f32 / TW,
            "pghsxn" => self.sect.height = v as f32 / TW,
            "marglsxn" => self.sect.margins[3] = v as f32 / TW,
            "margrsxn" => self.sect.margins[1] = v as f32 / TW,
            "margtsxn" => self.sect.margins[0] = v as f32 / TW,
            "margbsxn" => self.sect.margins[2] = v as f32 / TW,
            "headery" => self.sect.header_dist = v as f32 / TW,
            "footery" => self.sect.footer_dist = v as f32 / TW,
            "titlepg" => self.sect.title_page = on,
            "sbknone" => self.sect.new_page = false,
            "sbkpage" | "sbkodd" | "sbkeven" | "sbkcol" => self.sect.new_page = true,
            "pgnrestart" => self.sect.restart = true,
            "pgnstarts" => self.sect.page_start = Some(v.clamp(0, 1_000_000) as usize),
            "sect" => self.end_section(),
            // Special characters.
            "par" => self.end_para(false),
            "line" => self.put_char('\n'),
            "tab" => self.put_char('\t'),
            "page" => {
                if !self.story().spans.is_empty() {
                    self.end_para(false);
                }
                self.story().page_break = true;
            }
            "emdash" => self.put_char('—'),
            "endash" => self.put_char('–'),
            "emspace" => self.put_char('\u{2003}'),
            "enspace" => self.put_char('\u{2002}'),
            "qmspace" => self.put_char('\u{2005}'),
            "bullet" => self.put_char('•'),
            "lquote" => self.put_char('‘'),
            "rquote" => self.put_char('’'),
            "ldblquote" => self.put_char('“'),
            "rdblquote" => self.put_char('”'),
            "chpgn" => self.put_char(PAGE),
            // Tables.
            "trowd" => {
                self.row = RowDef::default();
                self.cell_def = CellDef::default();
                self.edge = None;
            }
            "trleft" => self.row.left = v,
            "trqc" => self.row.align = Align::Center,
            "trqr" => self.row.align = Align::Right,
            "trql" => self.row.align = Align::Left,
            "trhdr" => self.row.header = true,
            "trrh" => self.row.height = v,
            "trgaph" => self.row.gap = Some(v),
            "trbrdrt" => self.edge = Some(Edge::Row(0)),
            "trbrdrl" => self.edge = Some(Edge::Row(1)),
            "trbrdrb" => self.edge = Some(Edge::Row(2)),
            "trbrdrr" => self.edge = Some(Edge::Row(3)),
            "clbrdrt" | "clbrdrl" | "clbrdrb" | "clbrdrr" => {
                let i = match w {
                    "clbrdrt" => 0,
                    "clbrdrl" => 1,
                    "clbrdrb" => 2,
                    _ => 3,
                };
                self.cell_def.bordered = true;
                self.cell_def.borders[i] = None;
                self.edge = Some(Edge::Cell(i));
            }
            "brdrs" | "brdrth" | "brdrdb" | "brdrsh" | "brdrdot" | "brdrdash" | "brdrhair" | "brdrtriple" | "brdrthtnsg" | "brdrtnthsg" | "brdrdashsm" => self.border(|b| *b = Some(b.unwrap_or((0.5, Rgba::BLACK)))),
            "brdrnone" | "brdrnil" | "brdrtbl" => self.border(|b| *b = None),
            "brdrw" => self.border(|b| {
                if let Some(b) = b {
                    b.0 = (v as f32 / TW).clamp(0.25, 6.0);
                }
            }),
            "brdrcf" => {
                let c = self.color(v).unwrap_or(Rgba::BLACK);
                self.border(|b| {
                    if let Some(b) = b {
                        b.1 = c;
                    }
                });
            }
            "clmrg" => self.cell_def.hmerge = true,
            "clvmrg" => self.cell_def.vmerge = true,
            "clcbpat" => self.cell_def.fill = v,
            "clvertalc" => self.cell_def.valign = 1,
            "clvertalb" => self.cell_def.valign = 2,
            "clvertalt" => self.cell_def.valign = 0,
            "cellx" => {
                let mut d = std::mem::take(&mut self.cell_def);
                d.right = v;
                self.edge = None;
                if self.row.cells.len() < MAX_CELLS {
                    self.row.cells.push(d);
                }
            }
            "cell" => self.end_para(true),
            "nestcell" => {
                self.top().pp.intbl = true;
                self.end_para(false);
            }
            "row" => {
                let row = self.row.clone();
                let st = self.story();
                if !st.cell.is_empty() {
                    let c = std::mem::take(&mut st.cell);
                    st.cells.push(c);
                }
                let cells = std::mem::take(&mut st.cells);
                st.rows.push((row, cells));
            }
            "intbl" => self.top().pp.intbl = true,
            "itap" => self.top().pp.intbl = v > 0,
            // Paragraphs.
            "pard" => {
                self.top().pp = Pp::default();
                if matches!(self.edge, Some(Edge::Row(_))) {
                    self.edge = None;
                }
            }
            "ql" => self.top().pp.align = Align::Left,
            "qc" => self.top().pp.align = Align::Center,
            "qr" => self.top().pp.align = Align::Right,
            "qj" | "qd" => self.top().pp.align = Align::Justify,
            "li" | "lin" => self.top().pp.li = v.clamp(-31_680, 31_680),
            "ri" | "rin" => self.top().pp.ri = v.clamp(-31_680, 31_680),
            "fi" => self.top().pp.fi = v.clamp(-31_680, 31_680),
            "sb" => self.top().pp.sb = v.clamp(0, 31_680),
            "sa" => self.top().pp.sa = v.clamp(0, 31_680),
            "sl" => self.top().pp.sl = n.unwrap_or(0).clamp(-31_680, 31_680),
            "slmult" => self.top().pp.slmult = on,
            "pagebb" => self.top().pp.pagebb = on,
            "cbpat" => self.top().pp.shading = v,
            "tqc" => self.top().pp.tab_align = Some(TabAlign::Center),
            "tqr" | "tqdec" => self.top().pp.tab_align = Some(TabAlign::Right),
            "tldot" | "tlmdot" => self.top().pp.leader = Some('.'),
            "tlhyph" => self.top().pp.leader = Some('-'),
            "tlul" | "tlth" => self.top().pp.leader = Some('_'),
            "tleq" => self.top().pp.leader = Some('='),
            "tx" | "tb" => {
                let pp = &mut self.top().pp;
                let tab = Tab { pos: v.clamp(0, 31_680) as f32 / TW, align: pp.tab_align.take().unwrap_or(TabAlign::Left), leader: pp.leader.take() };
                if pp.tabs.len() < 64 && w == "tx" {
                    let at = pp.tabs.partition_point(|t| t.pos < tab.pos);
                    pp.tabs.insert(at, tab);
                }
            }
            // Characters.
            "plain" => self.top().chr = Chr::default(),
            "b" => self.top().chr.bold = on,
            "i" => self.top().chr.italic = on,
            "ul" | "ulw" | "uldb" | "ulth" | "uld" | "uldash" | "uldashd" | "ulwave" | "ulthd" | "ulthdash" | "uldashdd" | "ulhwave" | "ululdbwave" | "ulldash" | "ulthldash" | "ulthdashd" | "ulthdashdd" => self.top().chr.underline = on,
            "ulnone" => self.top().chr.underline = false,
            "strike" | "striked" => self.top().chr.strike = on,
            "super" => self.top().chr.shift = if on { 1 } else { 0 },
            "sub" => self.top().chr.shift = if on { -1 } else { 0 },
            "nosupersub" => self.top().chr.shift = 0,
            "up" => self.top().chr.shift = if v > 0 { 1 } else { 0 },
            "dn" => self.top().chr.shift = if v > 0 { -1 } else { 0 },
            "caps" | "scaps" => self.top().chr.caps = on,
            "v" | "deleted" => self.top().chr.hidden = on,
            "fs" => self.top().chr.fs = v.clamp(2, 3276),
            "f" => self.top().chr.font = v,
            "cf" => self.top().chr.color = v,
            "highlight" | "chcbpat" | "cb" => self.top().chr.highlight = v,
            _ => {}
        }
    }

    fn sym(&mut self, c: u8) {
        if c == b'*' {
            self.star = true;
            return;
        }
        if self.skip > 0 {
            self.skip -= 1;
            return;
        }
        match c {
            b'~' => self.put_char('\u{A0}'),
            b'_' => self.put_char('\u{2011}'),
            _ => {}
        }
    }

    fn open(&mut self) {
        self.flush();
        self.star = false;
        self.skip = 0;
        if self.stack.len() >= MAX_DEPTH || self.deep > 0 {
            self.deep += 1;
            return;
        }
        let g = self.top().clone();
        self.stack.push(g);
    }

    /// Closes a group, finishing what its destination was reading.
    fn close(&mut self) {
        self.flush();
        self.star = false;
        self.skip = 0;
        if self.deep > 0 {
            self.deep -= 1;
            return;
        }
        if self.stack.len() < 2 {
            return;
        }
        let Some(g) = self.stack.pop() else { return };
        let parent = self.top().dest;
        match g.dest {
            Dest::FontTbl if parent == Dest::FontTbl => {
                if !self.font_name.trim().is_empty() || !self.fonts.contains_key(&self.font_num) {
                    self.end_font();
                }
            }
            Dest::FldInst if parent != Dest::FldInst => {
                if let Some(f) = self.fields.last_mut() {
                    let first = f.instr.split_whitespace().next().unwrap_or("").to_ascii_uppercase();
                    f.kind = match first.as_str() {
                        "PAGE" => 1,
                        "NUMPAGES" | "SECTIONPAGES" => 2,
                        _ => 0,
                    };
                }
            }
            Dest::Pict if parent != Dest::Pict => {
                if let Some(p) = self.pict.take() {
                    self.picture(p);
                }
            }
            Dest::Hf(slot) if parent != Dest::Hf(slot) => {
                self.stack.push(g);
                if !self.story().spans.is_empty() {
                    self.end_para(false);
                }
                self.flush_table();
                self.stack.pop();
                if self.stories.len() > 1
                    && let Some(st) = self.stories.pop()
                    && let Some(s) = self.sect.hf.get_mut(slot as usize)
                {
                    *s = Some(Arc::new(st.blocks));
                    if slot >= 2 {
                        self.sect.title_page = true;
                    }
                }
            }
            _ => {}
        }
        // The end of a field group.
        if self.fields.last().is_some_and(|f| f.depth >= self.stack.len() + 1) {
            if let Some(f) = self.fields.pop() {
                match f.kind {
                    1 => self.put_char(PAGE),
                    2 => self.put_char(PAGES),
                    _ => {}
                }
            }
        }
    }

    fn picture(&mut self, p: Pict) {
        if !p.known || p.data.is_empty() {
            return;
        }
        let (pw, ph) = pixel_size(&p.data).unwrap_or((p.w.max(0) as u32, p.h.max(0) as u32));
        let sx = p.sx.clamp(1, 1000) as f32 / 100.0;
        let sy = p.sy.clamp(1, 1000) as f32 / 100.0;
        let w = if p.wgoal > 0 { p.wgoal as f32 / TW } else { pw as f32 * 0.75 } * sx;
        let h = if p.hgoal > 0 { p.hgoal as f32 / TW } else { ph as f32 * 0.75 } * sy;
        if !(w > 0.5 && h > 0.5 && w < 20_000.0 && h < 20_000.0) {
            return;
        }
        let size = p.data.len() as u64;
        if self.blob_bytes + size > self.max_total {
            return;
        }
        self.blob_bytes += size;
        let key: Arc<str> = Arc::from(format!("pict{}", self.blobs.len()));
        self.blobs.insert(key.clone(), p.data);
        let style = self.style();
        let st = self.story();
        if st.spans.len() < 100_000 {
            st.spans.push(Span { text: String::new(), style, object: Some(Object { w, h, key }) });
        }
    }
}

impl Group {
    /// The destination and whether text in it is dropped.
    fn clone_head(&self) -> (Dest, bool, bool) {
        (self.dest, self.hide, self.chr.hidden)
    }
}

/// The pixel size of a PNG or JPEG picture.
fn pixel_size(d: &[u8]) -> Option<(u32, u32)> {
    if d.starts_with(b"\x89PNG") {
        let w = u32::from_be_bytes(d.get(16..20)?.try_into().ok()?);
        let h = u32::from_be_bytes(d.get(20..24)?.try_into().ok()?);
        return Some((w.min(100_000), h.min(100_000)));
    }
    if d.starts_with(&[0xFF, 0xD8]) {
        let mut i = 2;
        while i + 9 < d.len() {
            if d[i] != 0xFF {
                i += 1;
                continue;
            }
            let m = d[i + 1];
            let len = u16::from_be_bytes([d[i + 2], d[i + 3]]) as usize;
            if matches!(m, 0xC0..=0xCF) && !matches!(m, 0xC4 | 0xC8 | 0xCC) {
                let h = u16::from_be_bytes([d[i + 5], d[i + 6]]) as u32;
                let w = u16::from_be_bytes([d[i + 7], d[i + 8]]) as u32;
                return Some((w, h));
            }
            i += 2 + len.max(2);
        }
    }
    None
}

/// Reads an RTF document: its model and its pictures' bytes.
fn read(data: &[u8], limits: &Limits) -> Result<(DocModel, HashMap<Arc<str>, Vec<u8>>), Error> {
    if !data.trim_ascii_start().starts_with(b"{\\rtf") {
        return Err(Error::Unsupported);
    }
    let mut r = Reader {
        stack: Vec::new(),
        deep: 0,
        stories: vec![Story::default()],
        fonts: HashMap::new(),
        font_num: 0,
        font_name: String::new(),
        font_charset: 0,
        deff: 0,
        colors: Vec::new(),
        color: [None; 3],
        enc: encoding_rs::WINDOWS_1252,
        bytes: Vec::new(),
        bytes_enc: None,
        bytes_symbol: 0,
        skip: 0,
        high: None,
        star: false,
        doc_sect: Sect::default(),
        sect: Sect::default(),
        sections: Vec::new(),
        row: RowDef::default(),
        cell_def: CellDef::default(),
        edge: None,
        fields: Vec::new(),
        pict: None,
        blobs: HashMap::new(),
        blob_bytes: 0,
        blocks: 0,
        style: None,
        max_part: limits.max_part_bytes,
        max_total: limits.max_unpacked_bytes,
    };
    r.top();
    let mut lx = Lexer { d: data.trim_ascii_start(), i: 0 };
    // The first section's page is the document's until a section says
    // otherwise: what `\paperw` and the like set before any `\sectd`.
    let mut started = false;
    loop {
        match lx.next() {
            Tok::Eof => break,
            Tok::Open => r.open(),
            Tok::Close => r.close(),
            Tok::Byte(b) => {
                if r.deep == 0 {
                    if !started && r.top().dest == Dest::Text {
                        started = true;
                        let hf = r.sect.hf.clone();
                        r.sect = Sect { hf, ..r.doc_sect.clone() };
                    }
                    r.byte(b);
                }
            }
            Tok::Word(w, n) => {
                if r.deep == 0 {
                    if !started && matches!(w, "par" | "sectd" | "pard" | "trowd" | "pict" | "header" | "footer" | "headerr" | "footerr" | "headerf" | "footerf") {
                        started = true;
                        let hf = r.sect.hf.clone();
                        r.sect = Sect { hf, ..r.doc_sect.clone() };
                    }
                    r.word(w, n);
                }
            }
            Tok::Sym(c) => {
                if r.deep == 0 {
                    r.sym(c);
                }
            }
            Tok::Bin => r.flush(),
        }
    }
    r.flush();
    if !started {
        let hf = r.sect.hf.clone();
        r.sect = Sect { hf, ..r.doc_sect.clone() };
    }
    // What the last groups left open.
    r.stories.truncate(1);
    r.stack.truncate(1);
    r.top().dest = Dest::Text;
    if !r.story().spans.is_empty() || r.stories[0].blocks.is_empty() && r.story().rows.is_empty() && r.story().cells.is_empty() && r.story().cell.is_empty() {
        r.end_para(false);
    }
    r.flush_table();
    let last = r.section();
    r.sections.push(last);
    // A section without a header has its previous section's.
    for i in 1..r.sections.len() {
        let (before, rest) = r.sections.split_at_mut(i);
        let (prev, s) = (&before[i - 1], &mut rest[0]);
        s.header = s.header.take().or_else(|| prev.header.clone());
        s.footer = s.footer.take().or_else(|| prev.footer.clone());
    }
    let body = std::mem::take(&mut r.stories[0].blocks);
    Ok((DocModel { body, sections: r.sections, background: None }, r.blobs))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(rtf: &str) -> String {
        let (m, _) = read(rtf.as_bytes(), &Limits::default()).unwrap();
        let mut out = String::new();
        for b in &m.body {
            if let Block::Para(p) = b {
                for s in &p.spans {
                    out.push_str(&s.text);
                }
                out.push('|');
            }
        }
        out
    }

    #[test]
    fn reads_text_and_code_pages() {
        assert_eq!(text(r"{\rtf1\ansi\ansicpg1251{\fonttbl{\f0 Arial;}}\f0 \'cf\'f0\'e8\par x\u8364?y\par}"), "При|x€y|");
        assert_eq!(text(r"{\rtf1{\fonttbl{\f0\fcharset2 Symbol;}{\f1 Arial;}}{\f0 \'b7}\tab {\f1 a}\par}"), "•\ta|");
        assert_eq!(text(r"{\rtf1 {\field{\*\fldinst PAGE}{\fldrslt 3}} of {\field{\*\fldinst NUMPAGES}{\fldrslt 9}}}"), "\u{E000} of \u{E001}|");
    }

    #[test]
    fn reads_tables() {
        let (m, _) = read(br"{\rtf1\trowd\cellx1000\cellx2000\intbl a\cell b\cell\row\pard after\par}", &Limits::default()).unwrap();
        assert!(matches!(&m.body[0], Block::Table(t) if t.rows.len() == 1 && t.rows[0].cells.len() == 2 && t.cols.len() == 2));
        assert!(matches!(&m.body[1], Block::Para(p) if p.spans[0].text == "after"));
    }
}
