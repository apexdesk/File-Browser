//! A worksheet read (its top-left area, within bounds) and drawn as one
//! page: the grid, fills, cell text as formatted, borders, merged cells
//! and pictures. Other spreadsheet formats read into the same [`Sheet`].

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use super::numfmt::Locale;
use super::styles::{HAlign, Styles, VAlign, Xf};
use crate::draw::{Item, Page, Rgba};
use crate::ooxml::xml::{Ev, Reader};
use crate::ooxml::Package;
use crate::text::TextRun;
use crate::typeset::{self, Align, FontCache, LineRule, ParaFormat, Span, Style};
use crate::{Error, Limits};

/// The area of a sheet shown: rows and columns from the top-left.
pub(crate) const MAX_ROWS: u32 = 200;
pub(crate) const MAX_COLS: u32 = 50;
/// The least area shown, so a small sheet still looks like one.
const MIN_ROWS: u32 = 20;
const MIN_COLS: u32 = 8;
const GRID: Rgba = Rgba([0xD9, 0xD9, 0xD9, 255]);
const PAD: f32 = 2.0;

pub(crate) enum Val {
    Num(f64),
    Text(String),
    Bool(bool),
    Err(String),
    /// Text as the file has it shown, and whether it is a number.
    Shown(String, bool),
}

pub(crate) struct Cell {
    pub row: u32,
    pub col: u32,
    pub xf: usize,
    pub val: Option<Val>,
}

/// What the workbook gives every sheet.
pub(crate) struct Book<'a> {
    pub pkg: &'a Package,
    pub shared: &'a [String],
    pub styles: &'a Styles,
    pub date1904: bool,
    pub locale: Locale,
}

/// How numbers are written: the workbook's date system and the locale.
pub(crate) struct Numbers {
    pub date1904: bool,
    pub locale: Locale,
}

/// A sheet read: its cells (within [`MAX_ROWS`] × [`MAX_COLS`]), sizes,
/// merged areas and pictures.
pub(crate) struct Sheet {
    pub cells: Vec<Cell>,
    /// Column widths and row heights in points (0 when hidden); others
    /// are the default.
    pub cols: HashMap<u32, f32>,
    pub rows: HashMap<u32, f32>,
    pub default_w: f32,
    pub default_h: f32,
    pub grid: bool,
    /// First and last row and column of each.
    pub merges: Vec<(u32, u32, u32, u32)>,
    pub drawing: Option<String>,
    pub anchors: Vec<Anchor>,
}

impl Sheet {
    pub fn new(default_w: f32, default_h: f32) -> Sheet {
        Sheet { cells: Vec::new(), cols: HashMap::new(), rows: HashMap::new(), default_w, default_h, grid: true, merges: Vec::new(), drawing: None, anchors: Vec::new() }
    }
}

/// A picture (or a chart, with no key) placed on a sheet: from a cell's
/// corner and an offset in points, to another's or for a size.
pub(crate) struct Anchor {
    pub from: Option<(u32, u32)>,
    pub off: (f32, f32),
    pub to: Option<(u32, u32, f32, f32)>,
    pub size: (f32, f32),
    pub key: Option<Arc<str>>,
}

/// A cell reference ("B12") as a zero-based row and column.
fn cell_ref(r: &str) -> Option<(u32, u32)> {
    let r = r.trim().trim_start_matches('$');
    let split = r.find(|c: char| c.is_ascii_digit())?;
    let (letters, digits) = r.split_at(split);
    let mut col = 0u32;
    for c in letters.trim_end_matches('$').chars() {
        if !c.is_ascii_alphabetic() {
            return None;
        }
        col = col.checked_mul(26)?.checked_add(c.to_ascii_uppercase() as u32 - 'A' as u32 + 1)?;
    }
    let row: u32 = digits.parse().ok()?;
    (col >= 1 && row >= 1).then(|| (row - 1, col - 1))
}

/// A width in characters of the default font as points.
pub(crate) fn chars_to_pt(w: f32) -> f32 {
    // Excel's maximum digit width for Calibri 11 is 7 pixels.
    let px = ((256.0 * w + (128.0f32 / 7.0).trunc()) / 256.0 * 7.0).trunc();
    px * 0.75
}

fn read_sheet(book: &Book, part: &str, limits: &Limits) -> Result<Sheet, Error> {
    let src = book.pkg.zip()?.read_string(part)?;
    if src.len() as u64 > limits.max_part_bytes {
        return Err(Error::TooLarge);
    }
    let mut sh = Sheet::new(chars_to_pt(8.43), 15.0);
    let mut r = Reader::new(&src);
    let mut row = 0u32;
    let mut next_row = 0u32;
    let mut next_col = 0u32;
    // The cell being read: its place, type and format.
    let mut cur: Option<(u32, u32, String, usize)> = None;
    let mut value: Option<String> = None;
    let mut inline = String::new();
    let mut drawing_id = None;
    // Read as a stream: nesting needs no bound, nothing recurses.
    loop {
        let ev = r.next();
        let empty = matches!(ev, Ev::Empty(_));
        match ev {
            Ev::Start(t) | Ev::Empty(t) => match t.local() {
                "sheetView" => {
                    if t.attr("showGridLines").is_some_and(|v| v == "0" || v == "false") {
                        sh.grid = false;
                    }
                }
                "sheetFormatPr" => {
                    if let Some(w) = t.attr("defaultColWidth").and_then(|v| v.parse::<f32>().ok()) {
                        sh.default_w = chars_to_pt(w.clamp(0.0, 255.0));
                    } else if let Some(w) = t.attr("baseColWidth").and_then(|v| v.parse::<f32>().ok()) {
                        sh.default_w = chars_to_pt(w.clamp(0.0, 255.0) + 0.71);
                    }
                    if let Some(h) = t.attr("defaultRowHeight").and_then(|v| v.parse::<f32>().ok()) {
                        sh.default_h = h.clamp(0.0, 409.0);
                    }
                }
                "col" => {
                    let n = |k: &str| t.attr(k).and_then(|v| v.parse::<f32>().ok());
                    let (Some(min), Some(max)) = (n("min"), n("max")) else { continue };
                    let hidden = t.attr("hidden").is_some_and(|v| v == "1" || v == "true");
                    let w = if hidden { 0.0 } else { n("width").map_or(sh.default_w, |w| chars_to_pt(w.clamp(0.0, 255.0))) };
                    let (min, max) = (min.max(1.0) as u32 - 1, (max.max(1.0) as u32).min(MAX_COLS));
                    for c in min..max {
                        sh.cols.insert(c, w);
                    }
                }
                "row" => {
                    row = t.attr("r").and_then(|v| v.parse::<u32>().ok()).map_or(next_row, |v| v.saturating_sub(1));
                    next_row = row + 1;
                    next_col = 0;
                    if row >= MAX_ROWS {
                        // The rest of the cells are not shown: on to what
                        // follows them.
                        let at = r.pos();
                        match memchr::memmem::find(&src.as_bytes()[at..], b"</sheetData>").or_else(|| memchr::memmem::find(&src.as_bytes()[at..], b"sheetData>")) {
                            Some(k) => {
                                r = Reader::new(&src[at + k..]);
                                continue;
                            }
                            None => break,
                        }
                    }
                    let hidden = t.attr("hidden").is_some_and(|v| v == "1" || v == "true");
                    if hidden {
                        sh.rows.insert(row, 0.0);
                    } else if let Some(h) = t.attr("ht").and_then(|v| v.parse::<f32>().ok()) {
                        sh.rows.insert(row, h.clamp(0.0, 409.0));
                    }
                }
                "c" => {
                    let [rf, ty, s] = t.attrs_of(["r", "t", "s"]);
                    let (rr, cc) = rf.as_deref().and_then(cell_ref).unwrap_or((row, next_col));
                    next_col = cc + 1;
                    let xf = s.and_then(|v| v.parse::<usize>().ok()).unwrap_or(0);
                    let ty = ty.map(|v| v.into_owned()).unwrap_or_default();
                    if rr < MAX_ROWS && cc < MAX_COLS {
                        if empty {
                            sh.cells.push(Cell { row: rr, col: cc, xf, val: None });
                        } else {
                            cur = Some((rr, cc, ty, xf));
                            value = None;
                            inline.clear();
                        }
                    }
                }
                "v" if cur.is_some() => value = Some(r.text()),
                "t" if cur.is_some() => inline.push_str(&r.text()),
                "rPh" => r.skip(),
                "f" if cur.is_some() => r.skip(),
                "mergeCell" => {
                    if let Some(rf) = t.attr("ref")
                        && let Some((a, b)) = rf.split_once(':')
                        && let (Some((r0, c0)), Some((r1, c1))) = (cell_ref(a), cell_ref(b))
                        && sh.merges.len() < 10_000
                        && r0 < MAX_ROWS
                        && c0 < MAX_COLS
                    {
                        sh.merges.push((r0, c0, r1.max(r0), c1.max(c0)));
                    }
                }
                "drawing" => drawing_id = t.attr("id").map(|v| v.into_owned()),
                _ => {}
            },
            Ev::End(name) => {
                if crate::ooxml::xml::local(name) == "c"
                    && let Some((rr, cc, ty, xf)) = cur.take()
                {
                    let val = match ty.as_str() {
                        "s" => value.as_deref().and_then(|v| v.trim().parse::<usize>().ok()).and_then(|i| book.shared.get(i)).map(|s| Val::Text(s.clone())),
                        "inlineStr" => Some(Val::Text(std::mem::take(&mut inline))),
                        "str" | "d" => value.take().map(Val::Text),
                        "b" => value.as_deref().map(|v| Val::Bool(v.trim() == "1")),
                        "e" => value.take().map(Val::Err),
                        _ => value.as_deref().and_then(|v| v.trim().parse::<f64>().ok()).filter(|n| n.is_finite()).map(Val::Num),
                    };
                    sh.cells.push(Cell { row: rr, col: cc, xf, val });
                }
            }
            Ev::Eof => break,
            Ev::Text(_) => {}
        }
    }
    if let Some(id) = drawing_id {
        sh.drawing = book.pkg.rels(part).get(&id).filter(|r| !r.external).map(|r| r.target.clone());
    }
    Ok(sh)
}

/// An anchor placed on the sheet, in points from its top-left.
struct Pic {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    key: Option<Arc<str>>,
}

fn emu(v: f32) -> f32 {
    v / 12_700.0
}

fn anchors(pkg: &Package, part: &str) -> Vec<Anchor> {
    let Ok(root) = pkg.xml(part, &[]) else { return Vec::new() };
    let rels = pkg.rels(part);
    let mut out = Vec::new();
    for a in root.elements().take(500) {
        let corner = |name: &str| -> Option<(u32, u32, f32, f32)> {
            let m = a.child(name)?;
            let n = |k: &str| m.child(k).and_then(|e| e.text().trim().parse::<f32>().ok()).unwrap_or(0.0);
            Some((n("row").clamp(0.0, 1e7) as u32, n("col").clamp(0.0, 1e7) as u32, emu(n("colOff")), emu(n("rowOff"))))
        };
        let ext = |e: Option<&crate::ooxml::El>| (e.and_then(|e| e.num("cx")).map_or(0.0, emu), e.and_then(|e| e.num("cy")).map_or(0.0, emu));
        let anchor = match a.name.as_str() {
            "twoCellAnchor" => {
                let (Some(f), Some(t)) = (corner("from"), corner("to")) else { continue };
                Anchor { from: Some((f.0, f.1)), off: (f.2, f.3), to: Some(t), size: (0.0, 0.0), key: None }
            }
            "oneCellAnchor" => {
                let Some(f) = corner("from") else { continue };
                Anchor { from: Some((f.0, f.1)), off: (f.2, f.3), to: None, size: ext(a.child("ext")), key: None }
            }
            "absoluteAnchor" => {
                let pos = a.child("pos");
                let n = |k: &str| pos.and_then(|e| e.num(k)).map_or(0.0, emu);
                Anchor { from: None, off: (n("x"), n("y")), to: None, size: ext(a.child("ext")), key: None }
            }
            _ => continue,
        };
        let key = a.find("blip").and_then(|b| b.attr("embed")).and_then(|id| rels.get(id)).filter(|r| !r.external).map(|r| Arc::from(r.target.as_str()));
        if key.is_some() || a.find("chart").is_some() {
            out.push(Anchor { key, ..anchor });
        }
    }
    out
}

/// Sheet `part` laid out as a page.
pub(crate) fn page(book: &Book, part: &str, fonts: &mut FontCache, limits: &Limits) -> Result<Page, Error> {
    let mut sh = read_sheet(book, part, limits)?;
    if let Some(d) = sh.drawing.take() {
        sh.anchors = anchors(book.pkg, &d);
    }
    Ok(draw(&sh, book.styles, &Numbers { date1904: book.date1904, locale: book.locale.clone() }, fonts))
}

/// A sheet drawn as a page.
pub(crate) fn draw(sh: &Sheet, styles: &Styles, numbers: &Numbers, fonts: &mut FontCache) -> Page {
    let shows = |c: &Cell| c.val.is_some() || styles.xf(c.xf).fill.is_some() || styles.xf(c.xf).borders.iter().any(Option::is_some);
    let mut last_row = sh.cells.iter().filter(|c| shows(c)).map(|c| c.row).max().unwrap_or(0);
    let mut last_col = sh.cells.iter().filter(|c| shows(c)).map(|c| c.col).max().unwrap_or(0);
    for &(_, _, r1, c1) in &sh.merges {
        last_row = last_row.max(r1);
        last_col = last_col.max(c1);
    }
    let col_w = |c: u32| sh.cols.get(&c).copied().unwrap_or(sh.default_w);
    let row_h = |r: u32| sh.rows.get(&r).copied().unwrap_or(sh.default_h);
    // Where a cell's top-left corner is, for any row and column.
    let mut col_x: Vec<f32> = vec![0.0];
    for c in 0..MAX_COLS + 1 {
        col_x.push(col_x[c as usize] + col_w(c));
    }
    let mut row_y: Vec<f32> = vec![0.0];
    for r in 0..MAX_ROWS + 1 {
        row_y.push(row_y[r as usize] + row_h(r));
    }
    let at = |r: u32, c: u32| -> (f32, f32) {
        let x = if (c as usize) < col_x.len() { col_x[c as usize] } else { col_x[col_x.len() - 1] + (c as usize + 1 - col_x.len()) as f32 * sh.default_w };
        let y = if (r as usize) < row_y.len() { row_y[r as usize] } else { row_y[row_y.len() - 1] + (r as usize + 1 - row_y.len()) as f32 * sh.default_h };
        (x, y)
    };
    let pics: Vec<Pic> = sh
        .anchors
        .iter()
        .filter_map(|a| {
            let (x0, y0) = a.from.map_or((0.0, 0.0), |(r, c)| at(r, c));
            let (x, y) = (x0 + a.off.0, y0 + a.off.1);
            let (w, h) = match a.to {
                Some((r, c, dx, dy)) => {
                    let (x1, y1) = at(r, c);
                    (x1 + dx - x, y1 + dy - y)
                }
                None => a.size,
            };
            (w > 0.5 && h > 0.5 && w < 20_000.0 && h < 20_000.0).then(|| Pic { x, y, w, h, key: a.key.clone() })
        })
        .collect();
    let last_col = last_col.clamp(MIN_COLS - 1, MAX_COLS - 1);
    // Rows shown down to a page's proportions, so the sheet reads as one.
    let tallest = (col_x[last_col as usize + 1] * 1.5).max(400.0);
    let mut last_row = last_row.clamp(MIN_ROWS - 1, MAX_ROWS - 1);
    while last_row >= MIN_ROWS && row_y[last_row as usize + 1] > tallest {
        last_row -= 1;
    }
    let mut width = col_x[last_col as usize + 1];
    let mut height = row_y[last_row as usize + 1];
    for p in &pics {
        width = width.max(p.x + p.w);
        height = height.max((p.y + p.h).min(tallest));
    }
    let width = width.min(col_x[MAX_COLS as usize]).max(36.0);
    let height = height.min(row_y[MAX_ROWS as usize]).max(36.0);
    let (nr, nc) = (last_row + 1, last_col + 1);

    let mut items = Vec::new();
    let mut text = Vec::new();
    // Merged areas by their top-left cell; the cells they cover.
    let mut merged_at: HashMap<(u32, u32), (u32, u32)> = HashMap::new();
    let mut covered: HashSet<(u32, u32)> = HashSet::new();
    for &(r0, c0, r1, c1) in &sh.merges {
        let (r1, c1) = (r1.min(MAX_ROWS), c1.min(MAX_COLS));
        merged_at.insert((r0, c0), (r1, c1));
        for r in r0..=r1 {
            for c in c0..=c1 {
                if (r, c) != (r0, c0) && covered.len() < 200_000 {
                    covered.insert((r, c));
                }
            }
        }
    }
    let filled: HashSet<(u32, u32)> = sh.cells.iter().filter(|c| c.val.is_some()).map(|c| (c.row, c.col)).collect();
    // Grid lines.
    if sh.grid {
        for c in 0..=nc {
            let x = col_x[c as usize];
            items.push(Item::Line { x0: x, y0: 0.0, x1: x, y1: height, width: 0.5, color: GRID });
        }
        for r in 0..=nr {
            let y = row_y[r as usize];
            items.push(Item::Line { x0: 0.0, y0: y, x1: width, y1: y, width: 0.5, color: GRID });
        }
    }
    let rect = |r: u32, c: u32| -> (f32, f32, f32, f32) {
        let (r1, c1) = merged_at.get(&(r, c)).copied().unwrap_or((r, c));
        let (x0, y0) = at(r, c);
        let (x1, y1) = at(r1 + 1, c1 + 1);
        (x0, y0, x1 - x0, y1 - y0)
    };
    // Merged areas hide the grid inside them.
    for (&(r, c), _) in &merged_at {
        let (x, y, w, h) = rect(r, c);
        items.push(Item::Rect { x: x + 0.25, y: y + 0.25, w: (w - 0.5).max(0.0), h: (h - 0.5).max(0.0), color: Rgba::WHITE });
    }
    for cell in &sh.cells {
        if covered.contains(&(cell.row, cell.col)) || cell.row > last_row || cell.col > last_col {
            continue;
        }
        if let Some(f) = styles.xf(cell.xf).fill {
            let (x, y, w, h) = rect(cell.row, cell.col);
            items.push(Item::Rect { x, y, w, h, color: f });
        }
    }
    for p in pics.iter().filter(|p| p.key.is_none()) {
        // A chart: its place shown.
        items.push(Item::Rect { x: p.x, y: p.y, w: p.w, h: p.h, color: Rgba([0xF4, 0xF4, 0xF4, 255]) });
        items.push(Item::Line { x0: p.x, y0: p.y, x1: p.x + p.w, y1: p.y, width: 0.75, color: Rgba([0xA0, 0xA0, 0xA0, 255]) });
        items.push(Item::Line { x0: p.x, y0: p.y + p.h, x1: p.x + p.w, y1: p.y + p.h, width: 0.75, color: Rgba([0xA0, 0xA0, 0xA0, 255]) });
        items.push(Item::Line { x0: p.x, y0: p.y, x1: p.x, y1: p.y + p.h, width: 0.75, color: Rgba([0xA0, 0xA0, 0xA0, 255]) });
        items.push(Item::Line { x0: p.x + p.w, y0: p.y, x1: p.x + p.w, y1: p.y + p.h, width: 0.75, color: Rgba([0xA0, 0xA0, 0xA0, 255]) });
    }
    for (i, cell) in sh.cells.iter().enumerate() {
        if i % 4096 == 4095 && items.len() > 2_000_000 {
            break;
        }
        let Some(val) = &cell.val else { continue };
        if covered.contains(&(cell.row, cell.col)) || cell.row > last_row || cell.col > last_col {
            continue;
        }
        let xf = styles.xf(cell.xf);
        let (x, y, w, h) = rect(cell.row, cell.col);
        if w <= 0.5 || h <= 0.5 {
            continue;
        }
        let (shown, color, number) = shown(val, xf, numbers);
        if shown.is_empty() {
            continue;
        }
        let style = match color {
            Some(c) => Arc::new(Style { color: c, ..(*xf.style).clone() }),
            None => xf.style.clone(),
        };
        let align = match xf.h {
            HAlign::General if number => HAlign::Right,
            HAlign::General if matches!(val, Val::Bool(_) | Val::Err(_)) => HAlign::Center,
            HAlign::General => HAlign::Left,
            a => a,
        };
        let merged = merged_at.contains_key(&(cell.row, cell.col));
        cell_text(fonts, &mut items, &mut text, CellText { shown, style, xf, align, number, x, y, w, h, merged, row: cell.row, col: cell.col }, &filled, &col_x, nc);
    }
    for cell in &sh.cells {
        if covered.contains(&(cell.row, cell.col)) || cell.row > last_row || cell.col > last_col {
            continue;
        }
        let [top, right, bottom, left] = styles.xf(cell.xf).borders;
        let (x, y, w, h) = rect(cell.row, cell.col);
        let mut line = |b: Option<(f32, Rgba)>, x0: f32, y0: f32, x1: f32, y1: f32| {
            if let Some((lw, c)) = b {
                items.push(Item::Line { x0, y0, x1, y1, width: lw, color: c });
            }
        };
        line(top, x, y, x + w, y);
        line(bottom, x, y + h, x + w, y + h);
        line(left, x, y, x, y + h);
        line(right, x + w, y, x + w, y + h);
    }
    for p in &pics {
        if let Some(key) = &p.key {
            items.push(Item::Image { x: p.x, y: p.y, w: p.w, h: p.h, key: key.clone() });
        }
    }
    Page { width, height, background: None, items, text }
}

/// A cell's text as shown, its colour from the format, and whether it is
/// a number.
fn shown(val: &Val, xf: &Xf, numbers: &Numbers) -> (String, Option<Rgba>, bool) {
    match val {
        Val::Num(n) => {
            let n = if numbers.date1904 && xf.num.is_date() { n + 1462.0 } else { *n };
            let (s, color) = xf.num.format_number(n, &numbers.locale);
            (s, color.map(Rgba::rgb), true)
        }
        Val::Shown(s, number) => (s.clone(), None, *number),
        Val::Text(s) => (s.clone(), None, false),
        Val::Bool(b) => ((if *b { "TRUE" } else { "FALSE" }).to_string(), None, false),
        Val::Err(e) => (e.clone(), None, false),
    }
}

struct CellText<'a> {
    shown: String,
    style: Arc<Style>,
    xf: &'a Xf,
    align: HAlign,
    number: bool,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    merged: bool,
    row: u32,
    col: u32,
}

#[allow(clippy::too_many_arguments)]
fn cell_text(fonts: &mut FontCache, items: &mut Vec<Item>, text: &mut Vec<TextRun>, c: CellText, filled: &HashSet<(u32, u32)>, col_x: &[f32], nc: u32) {
    let inner = (c.w - 2.0 * PAD - c.xf.indent).max(1.0);
    let wrap = c.xf.wrap && !c.number;
    // Unwrapped text is set from the left and placed below.
    let fmt = ParaFormat {
        align: match c.align {
            _ if !wrap => Align::Left,
            HAlign::Center => Align::Center,
            HAlign::Right => Align::Right,
            _ => Align::Left,
        },
        line: LineRule::Multiple(1.0),
        ..ParaFormat::default()
    };
    let shown = if c.align == HAlign::Fill && !c.shown.is_empty() { c.shown.repeat(64) } else { c.shown };
    let spans = [Span::new(shown.replace('\r', ""), c.style.clone())];
    let set_width = if wrap { inner } else { 100_000.0 };
    let mut lines = typeset::set(&spans, &fmt, set_width, fonts);
    let mut used = lines.lines.iter().map(|l| l.width).fold(0.0, f32::max);
    // Numbers too wide for the cell are shown as ####.
    if c.number && used > inner + 0.5 && !c.merged {
        let hashes = [Span::new("#".repeat(((inner / (c.style.size * 0.55)).floor() as usize).clamp(1, 255)), c.style.clone())];
        lines = typeset::set(&hashes, &fmt, 100_000.0, fonts);
        used = lines.lines.iter().map(|l| l.width).fold(0.0, f32::max);
    }
    // Text spills into empty cells to its right (left aligned) or left
    // (right aligned).
    let (mut left, mut right) = (c.x, c.x + c.w);
    if !wrap && !c.number && used > inner {
        match c.align {
            HAlign::Right => {
                let mut k = c.col;
                while k > 0 && !filled.contains(&(c.row, k - 1)) && c.x + c.w - left < used + 2.0 * PAD {
                    k -= 1;
                    left = col_x[k as usize];
                }
            }
            HAlign::Center => {}
            _ => {
                let mut k = c.col + 1;
                while k < nc.max(c.col + 1) + 40 && (k as usize) < col_x.len() - 1 && !filled.contains(&(c.row, k)) && right - c.x < used + 2.0 * PAD {
                    right = col_x[k as usize + 1];
                    k += 1;
                }
            }
        }
    }
    let total: f32 = lines.lines.iter().map(|l| l.height).sum();
    let ty = match c.xf.v {
        VAlign::Top => c.y + 1.0,
        VAlign::Center => c.y + (c.h - total) / 2.0,
        VAlign::Bottom => c.y + c.h - total - 1.0,
    };
    let tx = match c.align {
        _ if wrap => c.x + PAD + if fmt.align == Align::Left { c.xf.indent } else { 0.0 },
        HAlign::Right => c.x + c.w - PAD - used - c.xf.indent,
        HAlign::Center => c.x + (c.w - used) / 2.0,
        _ => c.x + PAD + c.xf.indent,
    };
    let clip = used > right - left - 2.0 * PAD + 0.5 || total > c.h + 0.5 || wrap;
    if clip {
        items.push(Item::Clip { x: left, y: c.y, w: right - left, h: c.h });
    }
    let mut ly = ty;
    for i in 0..lines.lines.len() {
        lines.draw_line(i, tx, ly, items, text);
        ly += lines.lines[i].height;
        if ly > c.y + c.h + 400.0 {
            break;
        }
    }
    if clip {
        items.push(Item::Unclip);
    }
}

/// Shared strings of the workbook, rich text joined, phonetic hints left
/// out.
pub(crate) fn shared_strings(pkg: &Package, part: Option<&str>, limits: &Limits) -> Result<Vec<String>, Error> {
    let Some(part) = part else { return Ok(Vec::new()) };
    let Ok(src) = pkg.zip()?.read_string(part) else { return Ok(Vec::new()) };
    if src.len() as u64 > limits.max_part_bytes {
        return Err(Error::TooLarge);
    }
    let mut out = Vec::new();
    let mut r = Reader::new(&src);
    let mut cur: Option<String> = None;
    loop {
        match r.next() {
            Ev::Start(t) => match t.local() {
                "si" => cur = Some(String::new()),
                "t" => {
                    let s = r.text();
                    if let Some(c) = &mut cur {
                        c.push_str(&s);
                    }
                }
                "rPh" | "phoneticPr" => r.skip(),
                _ => {}
            },
            Ev::Empty(t) if t.local() == "si" => out.push(String::new()),
            Ev::End(name) if crate::ooxml::xml::local(name) == "si" => {
                if let Some(c) = cur.take() {
                    out.push(c);
                }
                if out.len() > 4_000_000 {
                    return Err(Error::TooLarge);
                }
            }
            Ev::Eof => break,
            _ => {}
        }
    }
    Ok(out)
}
