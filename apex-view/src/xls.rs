//! Excel 97-2003 and Excel 5.0/95 workbooks (.xls, BIFF8 and BIFF5): one
//! page per visible worksheet, drawn like a workbook's. Opening reads the
//! workbook's globals (sheets, strings and formats); each sheet's cells
//! are read when it is asked for. Formulas show the result the file
//! keeps for them.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use crate::codepage::codepage;
use crate::draw::{self, Page, Rgba};
use crate::ole::Ole;
use crate::typeset::{FontCache, Style};
use crate::xlsx::numfmt::{NumFormat, builtin_numfmt};
use crate::xlsx::sheet::{self, Cell, MAX_COLS, MAX_ROWS, Numbers, Sheet, Val, chars_to_pt};
use crate::xlsx::styles::{HAlign, INDEXED, Styles, VAlign, Xf};
use crate::{Error, Fonts, Image, Limits, TextRun};

const KEEP: usize = 4;
const MAX_CELLS: usize = 200_000;

pub(crate) struct Doc {
    stream: Vec<u8>,
    /// Visible worksheets: where their records start.
    sheets: Vec<usize>,
    styles: Styles,
    strings: Vec<String>,
    biff5: bool,
    cp: u16,
    date1904: bool,
    pages: VecDeque<(usize, Page)>,
    fonts: FontCache,
    limits: Limits,
}

impl Doc {
    pub fn open(data: Vec<u8>, limits: &Limits, fonts: &Fonts) -> Result<Doc, Error> {
        let ole = Ole::open(&data, limits)?;
        let stream = ole.stream("Workbook").or_else(|| ole.stream("Book")).ok_or(Error::Unsupported)?;
        drop(ole);
        // The low byte of the version varies (0x0500, 0x0580...).
        let biff5 = match stream.get(..6) {
            Some([0x09, 0x08, _, _, _, 0x06]) => false,
            Some([0x09, 0x08, _, _, _, 0x05]) => true,
            // Other versions (some programs write 0): BIFF8 has the longer BOF.
            Some([0x09, 0x08, len, 0, _, _]) => *len < 16,
            _ => return Err(Error::Unsupported),
        };
        let g = globals(&stream, biff5)?;
        let mut sheets = Vec::new();
        for &(pos, hidden, kind) in &g.sheets {
            if kind == 0 && !hidden && !sheets.contains(&pos) && sheets.len() < limits.max_pages.max(1) {
                sheets.push(pos);
            }
        }
        if sheets.is_empty() {
            return Err(Error::Malformed("the workbook has no worksheet to show".into()));
        }
        let styles = g.styles();
        Ok(Doc { stream, sheets, styles, strings: g.strings, biff5, cp: g.cp, date1904: g.date1904, pages: VecDeque::new(), fonts: FontCache::new(fonts.clone()), limits: limits.clone() })
    }

    pub fn page_count(&self) -> usize {
        self.sheets.len()
    }

    fn page(&mut self, n: usize) -> Result<&Page, Error> {
        let &pos = self.sheets.get(n).ok_or(Error::NoSuchPage)?;
        if !self.pages.iter().any(|(i, _)| *i == n) {
            let sh = read_sheet(&self.stream, pos, &self.strings, self.biff5, self.cp);
            let numbers = Numbers { date1904: self.date1904, locale: crate::xlsx::locale() };
            let page = sheet::draw(&sh, &self.styles, &numbers, &mut self.fonts);
            if self.pages.len() >= KEEP {
                self.pages.pop_front();
            }
            self.pages.push_back((n, page));
        }
        self.pages.iter().find(|(i, _)| *i == n).map(|(_, p)| p).ok_or(Error::NoSuchPage)
    }

    pub fn page_size(&mut self, n: usize) -> Result<(f32, f32), Error> {
        self.page(n).map(|p| (p.width, p.height))
    }

    pub fn render(&mut self, n: usize, width: u32, cancel: &dyn Fn() -> bool) -> Result<Image, Error> {
        self.page(n)?;
        let page = self.pages.iter().find(|(i, _)| *i == n).map(|(_, p)| p).ok_or(Error::NoSuchPage)?;
        draw::render(page, width, &mut self.fonts, &mut |_| None, &self.limits, cancel)
    }

    pub fn runs(&mut self, n: usize) -> Result<Vec<TextRun>, Error> {
        self.page(n).map(Page::runs)
    }
}

// --- records ------------------------------------------------------------------------

/// The records from `at`: type, data and where the next one starts.
struct Records<'a> {
    d: &'a [u8],
    p: usize,
}

impl<'a> Iterator for Records<'a> {
    type Item = (u16, &'a [u8]);

    fn next(&mut self) -> Option<(u16, &'a [u8])> {
        let ty = u16_at(self.d, self.p)?;
        let len = u16_at(self.d, self.p + 2)? as usize;
        let start = self.p + 4;
        let data = self.d.get(start..(start + len).min(self.d.len()))?;
        self.p = start + len;
        Some((ty, data))
    }
}

fn u16_at(d: &[u8], p: usize) -> Option<u16> {
    Some(u16::from_le_bytes(d.get(p..p.checked_add(2)?)?.try_into().ok()?))
}

fn u32_at(d: &[u8], p: usize) -> Option<u32> {
    Some(u32::from_le_bytes(d.get(p..p.checked_add(4)?)?.try_into().ok()?))
}

/// A reader of a record's fields.
struct Cur<'a> {
    d: &'a [u8],
    p: usize,
}

impl<'a> Cur<'a> {
    fn new(d: &'a [u8]) -> Cur<'a> {
        Cur { d, p: 0 }
    }

    fn bytes(&mut self, n: usize) -> Option<&'a [u8]> {
        let b = self.d.get(self.p..self.p.checked_add(n)?)?;
        self.p += n;
        Some(b)
    }

    fn u8(&mut self) -> Option<u8> {
        self.bytes(1).map(|b| b[0])
    }

    fn u16(&mut self) -> Option<u16> {
        self.bytes(2).map(|b| u16::from_le_bytes([b[0], b[1]]))
    }

    fn u32(&mut self) -> Option<u32> {
        self.bytes(4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn left(&self) -> usize {
        self.d.len().saturating_sub(self.p)
    }

    /// `n` characters, 8-bit (Latin-1) or 16-bit.
    fn chars(&mut self, n: usize, wide: bool) -> Option<String> {
        if wide {
            let b = self.bytes(n.checked_mul(2)?)?;
            Some(char::decode_utf16(b.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]]))).map(|c| c.unwrap_or('\u{FFFD}')).collect())
        } else {
            Some(self.bytes(n)?.iter().map(|&b| b as char).collect())
        }
    }
}

/// A string with an 8- or 16-bit length: BIFF8's `ShortXLUnicodeString`
/// and `XLUnicodeString`, or BIFF5's bytes in the code page.
fn text(c: &mut Cur, len16: bool, biff5: bool, cp: u16) -> Option<String> {
    let n = if len16 { c.u16()? as usize } else { c.u8()? as usize };
    if biff5 {
        let b = c.bytes(n.min(c.left()))?;
        Some(codepage(i32::from(cp)).unwrap_or(encoding_rs::WINDOWS_1252).decode_without_bom_handling(b).0.into_owned())
    } else {
        let wide = c.u8()? & 1 != 0;
        let most = c.left() / if wide { 2 } else { 1 };
        c.chars(n.min(most), wide)
    }
}

// --- the workbook's globals -----------------------------------------------------------

struct Font {
    name: String,
    /// Twips.
    size: u16,
    bold: bool,
    italic: bool,
    strike: bool,
    underline: bool,
    shift: i8,
    color: u16,
}

struct Globals {
    /// Each sheet's place, whether it's hidden, and its kind (0 a
    /// worksheet).
    sheets: Vec<(usize, bool, u8)>,
    strings: Vec<String>,
    formats: HashMap<u16, Arc<NumFormat>>,
    fonts: Vec<Font>,
    xfs: Vec<Vec<u8>>,
    palette: [u32; 64],
    date1904: bool,
    cp: u16,
}

/// A BIFF5 `XF` in BIFF8's layout.
fn xf_from_biff5(x: &[u8]) -> Vec<u8> {
    if x.len() < 16 {
        return x.to_vec();
    }
    let u32_at = |i: usize| u32::from_le_bytes([x[i], x[i + 1], x[i + 2], x[i + 3]]);
    let align = u16::from_le_bytes([x[6], x[7]]);
    let (w8, w12) = (u32_at(8), u32_at(12));
    let b1 = (w12 >> 3 & 7) | (w12 >> 6 & 7) << 4 | (w12 & 7) << 8 | (w8 >> 22 & 7) << 12 | (w12 >> 16 & 0x7F) << 16 | (w12 >> 23 & 0x7F) << 23;
    let b2 = (w12 >> 9 & 0x7F) | (w8 >> 25 & 0x7F) << 7 | (w8 >> 16 & 0x3F) << 26;
    let fill = (w8 & 0x3FFF) as u16;
    let mut out = x[..6].to_vec();
    out.extend_from_slice(&[(align & 0x7F) as u8, 0, 0, 0]);
    out.extend_from_slice(&b1.to_le_bytes());
    out.extend_from_slice(&b2.to_le_bytes());
    out.extend_from_slice(&fill.to_le_bytes());
    out
}

fn globals(stream: &[u8], biff5: bool) -> Result<Globals, Error> {
    let mut g = Globals { sheets: Vec::new(), strings: Vec::new(), formats: HashMap::new(), fonts: Vec::new(), xfs: Vec::new(), palette: INDEXED, date1904: false, cp: 1252 };
    let mut recs = Records { d: stream, p: 0 }.peekable();
    while let Some((ty, d)) = recs.next() {
        let mut c = Cur::new(d);
        match ty {
            0x000A => break,
            0x002F => return Err(Error::Encrypted),
            0x0022 => g.date1904 = c.u16() == Some(1),
            0x0042 => {
                g.cp = match c.u16().unwrap_or(1252) {
                    0x8000 => 10000,
                    0x8001 | 1200 | 0 => 1252,
                    v => v,
                }
            }
            0x0085 if g.sheets.len() < 10_000 => {
                let pos = c.u32().unwrap_or(u32::MAX) as usize;
                let hidden = c.u8().unwrap_or(0) & 3 != 0;
                let kind = c.u8().unwrap_or(0);
                g.sheets.push((pos, hidden, kind));
            }
            0x00FC if !biff5 => {
                // The shared strings, going on in CONTINUE records where
                // a string's characters start again with their flags byte.
                let mut parts = vec![d];
                while let Some(&(0x003C, more)) = recs.peek() {
                    parts.push(more);
                    recs.next();
                }
                g.strings = sst(&parts);
            }
            0x041E | 0x001E => {
                if let (Some(id), Some(code)) = (c.u16(), text(&mut c, !biff5, biff5, g.cp))
                    && g.formats.len() < 4096
                {
                    g.formats.insert(id, Arc::new(NumFormat::parse(&code)));
                }
            }
            0x0031 if g.fonts.len() < 4096 => {
                let f = (|| {
                    let size = c.u16()?;
                    let flags = c.u16()?;
                    let color = c.u16()?;
                    let weight = c.u16()?;
                    let script = c.u16()?;
                    let underline = c.u8()? != 0;
                    c.bytes(3)?;
                    let name = text(&mut c, false, biff5, g.cp)?;
                    let shift = match script {
                        1 => 1,
                        2 => -1,
                        _ => 0,
                    };
                    Some(Font { name, size, bold: weight >= 600, italic: flags & 2 != 0, strike: flags & 8 != 0, underline, shift, color })
                })();
                if let Some(f) = f {
                    g.fonts.push(f);
                }
            }
            0x00E0 if g.xfs.len() < 65_536 => g.xfs.push(if biff5 { xf_from_biff5(d) } else { d.to_vec() }),
            0x0092 => {
                let n = c.u16().unwrap_or(0) as usize;
                for i in 0..n.min(56) {
                    if let Some(b) = c.bytes(4) {
                        g.palette[8 + i] = u32::from(b[0]) << 16 | u32::from(b[1]) << 8 | u32::from(b[2]);
                    }
                }
            }
            _ => {}
        }
    }
    Ok(g)
}

/// The shared string table, from its record and its continuations.
fn sst<'a>(parts: &[&'a [u8]]) -> Vec<String> {
    let mut out = Vec::new();
    let Some(first) = parts.first() else { return out };
    let n = u32_at(first, 4).unwrap_or(0) as usize;
    let mut part = 0;
    let mut c = Cur { d: first, p: 8 };
    // The next part when this one is used up.
    let next = |part: &mut usize, c: &mut Cur<'a>| -> bool {
        *part += 1;
        match parts.get(*part) {
            Some(d) => {
                *c = Cur::new(d);
                true
            }
            None => false,
        }
    };
    'strings: for _ in 0..n.min(4_000_000) {
        if c.left() == 0 && !next(&mut part, &mut c) {
            break;
        }
        let (Some(len), Some(flags)) = (c.u16(), c.u8()) else { break };
        let runs = if flags & 8 != 0 { c.u16().unwrap_or(0) as usize } else { 0 };
        let ext = if flags & 4 != 0 { c.u32().unwrap_or(0) as usize } else { 0 };
        let mut wide = flags & 1 != 0;
        let mut left = len as usize;
        let mut s = String::new();
        while left > 0 {
            if c.left() == 0 {
                if !next(&mut part, &mut c) {
                    out.push(s);
                    break 'strings;
                }
                wide = c.u8().unwrap_or(0) & 1 != 0;
            }
            let take = (c.left() / if wide { 2 } else { 1 }).min(left);
            if take == 0 {
                break 'strings;
            }
            if let Some(t) = c.chars(take, wide) {
                s.push_str(&t);
            }
            left -= take;
        }
        // Formatting runs and phonetic data, maybe in the next parts.
        let mut skip = runs * 4 + ext;
        while skip > 0 {
            let k = skip.min(c.left());
            c.p += k;
            skip -= k;
            if skip > 0 && !next(&mut part, &mut c) {
                break;
            }
        }
        out.push(s);
    }
    out
}

impl Globals {
    fn color(&self, i: u16) -> Option<Rgba> {
        self.palette.get(i as usize).map(|&c| Rgba::rgb(c))
    }

    fn font(&self, i: u16) -> Option<&Font> {
        // Index 4 doesn't exist.
        self.fonts.get(if i >= 4 { i as usize - 1 } else { i as usize })
    }

    fn style_of(&self, f: &Font) -> Style {
        Style {
            family: Arc::from(f.name.as_str()),
            size: (f32::from(f.size) / 20.0).clamp(1.0, 409.0),
            bold: f.bold,
            italic: f.italic,
            strike: f.strike,
            underline: f.underline,
            shift: f.shift,
            color: self.color(f.color).unwrap_or(Rgba::BLACK),
            ..Style::default()
        }
    }

    fn styles(&self) -> Styles {
        let base = Arc::new(self.fonts.first().map_or_else(|| Style { family: Arc::from("Arial"), size: 10.0, ..Style::default() }, |f| self.style_of(f)));
        let mut fonts: HashMap<u16, Arc<Style>> = HashMap::new();
        let mut xfs = Vec::with_capacity(self.xfs.len());
        for x in &self.xfs {
            let mut c = Cur::new(x);
            let (Some(font), Some(fmt), Some(_), Some(align), Some(_), Some(indent), Some(_), Some(b1), Some(b2), Some(fill)) =
                (c.u16(), c.u16(), c.u16(), c.u8(), c.u8(), c.u8(), c.u8(), c.u32(), c.u32(), c.u16())
            else {
                xfs.push(Xf::plain(base.clone()));
                continue;
            };
            let style = match fonts.get(&font) {
                Some(s) => s.clone(),
                None => {
                    let s = self.font(font).map_or_else(|| base.clone(), |f| Arc::new(self.style_of(f)));
                    fonts.insert(font, s.clone());
                    s
                }
            };
            let num = self.formats.get(&fmt).cloned().unwrap_or_else(|| Arc::new(NumFormat::parse(builtin_numfmt(u32::from(fmt)).unwrap_or("General"))));
            let line = |kind: u32, color: u32| {
                let w = match kind {
                    0 => return None,
                    2 | 8 | 10 | 12 => 1.5,
                    5 | 6 => 2.25,
                    7 => 0.5,
                    _ => 0.75,
                };
                Some((w, self.color(color as u16).filter(|_| color != 0x40 && color != 0x7F).unwrap_or(Rgba::BLACK)))
            };
            let borders = [line((b1 >> 8) & 15, b2 & 0x7F), line((b1 >> 4) & 15, (b1 >> 23) & 0x7F), line((b1 >> 12) & 15, (b2 >> 7) & 0x7F), line(b1 & 15, (b1 >> 16) & 0x7F)];
            let fill = match b2 >> 26 {
                0 => None,
                1 => self.color(fill & 0x7F),
                // Patterns shown as their foreground over white, lightened.
                _ => self.color(fill & 0x7F).map(|Rgba([r, g, b, _])| {
                    let l = |c: u8| ((c as u16 + 255 * 2) / 3) as u8;
                    Rgba([l(r), l(g), l(b), 255])
                }),
            };
            xfs.push(Xf {
                style,
                fill,
                borders,
                num,
                h: match align & 7 {
                    1 => HAlign::Left,
                    2 | 6 => HAlign::Center,
                    3 => HAlign::Right,
                    4 => HAlign::Fill,
                    5 | 7 => HAlign::Left,
                    _ => HAlign::General,
                },
                v: match (align >> 4) & 7 {
                    0 => VAlign::Top,
                    1 | 3 | 4 => VAlign::Center,
                    _ => VAlign::Bottom,
                },
                wrap: align & 8 != 0,
                indent: f32::from(indent & 15) * 9.0,
            });
        }
        Styles::new(xfs, Xf::plain(base))
    }
}

// --- sheets ---------------------------------------------------------------------------

fn error_text(code: u8) -> &'static str {
    match code {
        0x00 => "#NULL!",
        0x07 => "#DIV/0!",
        0x0F => "#VALUE!",
        0x17 => "#REF!",
        0x1D => "#NAME?",
        0x24 => "#NUM!",
        _ => "#N/A",
    }
}

fn rk(v: u32) -> f64 {
    let n = if v & 2 != 0 { ((v as i32) >> 2) as f64 } else { f64::from_bits(u64::from(v & 0xFFFF_FFFC) << 32) };
    if v & 1 != 0 { n / 100.0 } else { n }
}

/// The sheet whose records start at `pos`, within the area shown.
fn read_sheet(stream: &[u8], pos: usize, strings: &[String], biff5: bool, cp: u16) -> Sheet {
    let mut sh = Sheet::new(chars_to_pt(8.43), 12.75);
    let mut recs = Records { d: stream, p: pos };
    // Its own BOF.
    if !matches!(recs.next(), Some((0x0809, _))) {
        return sh;
    }
    let mut depth = 0usize;
    // The formula whose text result comes in the next STRING record.
    let mut want_string: Option<usize> = None;
    let put = |sh: &mut Sheet, row: u16, col: u16, xf: u16, val: Option<Val>| {
        if u32::from(row) < MAX_ROWS && u32::from(col) < MAX_COLS && sh.cells.len() < MAX_CELLS {
            sh.cells.push(Cell { row: u32::from(row), col: u32::from(col), xf: xf as usize, val });
            true
        } else {
            false
        }
    };
    let mut n = 0usize;
    while let Some((ty, d)) = recs.next() {
        n += 1;
        if n > 2_000_000 {
            break;
        }
        let mut c = Cur::new(d);
        // Embedded charts' substreams are skipped.
        match ty {
            0x0809 => depth += 1,
            0x000A if depth == 0 => break,
            0x000A => depth -= 1,
            _ if depth > 0 => {}
            0x0203 => {
                if let (Some(row), Some(col), Some(xf), Some(v)) = (c.u16(), c.u16(), c.u16(), c.bytes(8)) {
                    let v = f64::from_le_bytes(v.try_into().unwrap_or([0; 8]));
                    put(&mut sh, row, col, xf, Some(Val::Num(v)));
                }
            }
            0x027E | 0x007E => {
                if let (Some(row), Some(col), Some(xf), Some(v)) = (c.u16(), c.u16(), c.u16(), c.u32()) {
                    put(&mut sh, row, col, xf, Some(Val::Num(rk(v))));
                }
            }
            0x00BD => {
                if let (Some(row), Some(first)) = (c.u16(), c.u16()) {
                    let n = c.left().saturating_sub(2) / 6;
                    for i in 0..n.min(256) {
                        if let (Some(xf), Some(v)) = (c.u16(), c.u32()) {
                            put(&mut sh, row, first.saturating_add(i as u16), xf, Some(Val::Num(rk(v))));
                        }
                    }
                }
            }
            0x00FD => {
                if let (Some(row), Some(col), Some(xf), Some(i)) = (c.u16(), c.u16(), c.u16(), c.u32()) {
                    let v = strings.get(i as usize).filter(|s| !s.is_empty()).map(|s| Val::Text(s.clone()));
                    put(&mut sh, row, col, xf, v);
                }
            }
            0x0204 | 0x00D6 => {
                if let (Some(row), Some(col), Some(xf)) = (c.u16(), c.u16(), c.u16())
                    && u32::from(row) < MAX_ROWS
                    && u32::from(col) < MAX_COLS
                    && let Some(s) = text(&mut c, true, biff5, cp)
                {
                    put(&mut sh, row, col, xf, (!s.is_empty()).then_some(Val::Text(s)));
                }
            }
            0x0205 | 0x0005 => {
                if let (Some(row), Some(col), Some(xf), Some(v), Some(err)) = (c.u16(), c.u16(), c.u16(), c.u8(), c.u8()) {
                    let v = if err != 0 { Val::Err(error_text(v).to_string()) } else { Val::Bool(v != 0) };
                    put(&mut sh, row, col, xf, Some(v));
                }
            }
            0x0201 => {
                if let (Some(row), Some(col), Some(xf)) = (c.u16(), c.u16(), c.u16()) {
                    put(&mut sh, row, col, xf, None);
                }
            }
            0x00BE => {
                if let (Some(row), Some(first)) = (c.u16(), c.u16()) {
                    let n = c.left().saturating_sub(2) / 2;
                    for i in 0..n.min(256) {
                        if let Some(xf) = c.u16() {
                            put(&mut sh, row, first.saturating_add(i as u16), xf, None);
                        }
                    }
                }
            }
            0x0006 | 0x0406 => {
                want_string = None;
                if let (Some(row), Some(col), Some(xf), Some(v)) = (c.u16(), c.u16(), c.u16(), c.bytes(8)) {
                    let (val, text_next) = if v[6] == 0xFF && v[7] == 0xFF {
                        match v[0] {
                            0 => (None, true),
                            1 => (Some(Val::Bool(v[2] != 0)), false),
                            2 => (Some(Val::Err(error_text(v[2]).to_string())), false),
                            _ => (None, false),
                        }
                    } else {
                        (Some(Val::Num(f64::from_le_bytes(v.try_into().unwrap_or([0; 8])))), false)
                    };
                    if put(&mut sh, row, col, xf, val) && text_next {
                        want_string = Some(sh.cells.len() - 1);
                    }
                }
            }
            0x0207 => {
                if let Some(i) = want_string.take()
                    && let Some(s) = text(&mut c, true, biff5, cp)
                    && !s.is_empty()
                {
                    sh.cells[i].val = Some(Val::Text(s));
                }
            }
            0x00E5 => {
                let n = c.u16().unwrap_or(0);
                for _ in 0..n {
                    let (Some(r1), Some(r2), Some(c1), Some(c2)) = (c.u16(), c.u16(), c.u16(), c.u16()) else { break };
                    if u32::from(r1) < MAX_ROWS && u32::from(c1) < MAX_COLS && r2 >= r1 && c2 >= c1 && sh.merges.len() < 10_000 {
                        sh.merges.push((u32::from(r1), u32::from(c1), u32::from(r2), u32::from(c2)));
                    }
                }
            }
            0x007D => {
                if let (Some(first), Some(last), Some(w), Some(_), Some(flags)) = (c.u16(), c.u16(), c.u16(), c.u16(), c.u16()) {
                    for col in u32::from(first)..=u32::from(last).min(MAX_COLS - 1) {
                        sh.cols.insert(col, if flags & 1 != 0 { 0.0 } else { chars_to_pt(f32::from(w) / 256.0).clamp(0.0, 2000.0) });
                    }
                }
            }
            0x0055 => {
                if let Some(w) = c.u16() {
                    sh.default_w = chars_to_pt((f32::from(w) + 0.71).min(255.0));
                }
            }
            0x0225 => {
                if let (Some(flags), Some(h)) = (c.u16(), c.u16()) {
                    sh.default_h = if flags & 2 != 0 { 0.0 } else { (f32::from(h) / 20.0).clamp(0.0, 409.0) };
                }
            }
            0x0208 => {
                if d.len() >= 14 {
                    let row = u32::from(u16::from_le_bytes([d[0], d[1]]));
                    let h = u16::from_le_bytes([d[6], d[7]]) & 0x7FFF;
                    let flags = u16::from_le_bytes([d[12], d[13]]);
                    if row < MAX_ROWS {
                        if flags & 0x20 != 0 {
                            sh.rows.insert(row, 0.0);
                        } else if h > 0 {
                            sh.rows.insert(row, (f32::from(h) / 20.0).min(409.0));
                        }
                    }
                }
            }
            0x023E => sh.grid = u16_at(d, 0).is_none_or(|f| f & 2 != 0),
            _ => {}
        }
    }
    sh
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_strings_across_records() {
        // Two strings: "abc" 8-bit, then "déf" 16-bit split across a
        // CONTINUE that starts again with its flags byte.
        let first = [&[0, 0, 0, 0, 2, 0, 0, 0][..], &[3, 0, 0], b"abc", &[3, 0, 1], &[b'd', 0, 0xE9, 0]].concat();
        let more = [1u8, b'f', 0];
        let s = sst(&[&first, &more]);
        assert_eq!(s, vec!["abc".to_string(), "déf".to_string()]);
        assert_eq!(rk(0x0000_0192), 100.0);
        assert_eq!(rk(0x3FF0_0001), 0.01);
    }
}
