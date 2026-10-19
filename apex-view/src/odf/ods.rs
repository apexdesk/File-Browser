//! OpenDocument spreadsheets (.ods): one page per visible sheet, drawn
//! like a workbook's. Sheets are read as a stream when the file is
//! opened, each within the area shown; cells keep the text the file
//! shows for them.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use super::{Styles, border, collapse, color, length};
use crate::draw::{self, Page};
use crate::ooxml::xml::{Ev, Reader, local};
use crate::ooxml::{El, Node, Package, Pictures};
use crate::typeset::{FontCache, Style};
use crate::xlsx::sheet::{self, Anchor, Cell, MAX_COLS, MAX_ROWS, Numbers, Sheet, Val};
use crate::xlsx::styles::{HAlign, Styles as CellStyles, VAlign, Xf};
use crate::{Error, Fonts, Image, Limits, TextRun};

const KEEP: usize = 4;

pub(crate) struct Doc {
    pkg: Package,
    sheets: Vec<Sheet>,
    styles: CellStyles,
    pages: VecDeque<(usize, Page)>,
    fonts: FontCache,
    pictures: Pictures,
    limits: Limits,
}

impl Doc {
    pub fn open(data: Vec<u8>, limits: &Limits, fonts: &Fonts) -> Result<Doc, Error> {
        let pkg = Package::open(data, limits)?;
        let (sheets, styles) = read(&pkg, limits)?;
        if sheets.is_empty() {
            return Err(crate::ooxml::bad("the spreadsheet has no sheet to show"));
        }
        Ok(Doc { pkg, sheets, styles, pages: VecDeque::new(), fonts: FontCache::new(fonts.clone()), pictures: Pictures::default(), limits: limits.clone() })
    }

    pub fn page_count(&self) -> usize {
        self.sheets.len()
    }

    fn page(&mut self, n: usize) -> Result<&Page, Error> {
        let sh = self.sheets.get(n).ok_or(Error::NoSuchPage)?;
        if !self.pages.iter().any(|(i, _)| *i == n) {
            let numbers = Numbers { date1904: false, locale: crate::xlsx::locale() };
            let page = sheet::draw(sh, &self.styles, &numbers, &mut self.fonts);
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
        let (pkg, pictures) = (&self.pkg, &mut self.pictures);
        draw::render(page, width, &mut self.fonts, &mut |key| pictures.get(pkg, key), &self.limits, cancel)
    }

    pub fn runs(&mut self, n: usize) -> Result<Vec<TextRun>, Error> {
        self.page(n).map(Page::runs)
    }
}

/// Cell formats made from the document's cell styles, by name.
struct Formats {
    st: Styles,
    base: Style,
    by_name: HashMap<String, usize>,
    xfs: Vec<Xf>,
}

impl Formats {
    fn xf(&mut self, name: Option<&str>) -> usize {
        let name = name.unwrap_or("Default");
        if let Some(&i) = self.by_name.get(name) {
            return i;
        }
        let cp = self.st.props("table-cell", Some(name), "table-cell-properties");
        let pp = self.st.props("table-cell", Some(name), "paragraph-properties");
        let tp = self.st.props("table-cell", Some(name), "text-properties");
        let all = cp.get("border").and_then(border);
        let side = |k: &str| match cp.get(k) {
            Some(v) => border(v),
            None => all,
        };
        let h = match pp.get("text-align") {
            _ if cp.get("text-align-source") == Some("value-type") => HAlign::General,
            Some("center") => HAlign::Center,
            Some("end" | "right") => HAlign::Right,
            Some("start" | "left" | "justify") => HAlign::Left,
            _ => HAlign::General,
        };
        let v = match cp.get("vertical-align") {
            Some("top") => VAlign::Top,
            Some("middle") => VAlign::Center,
            _ => VAlign::Bottom,
        };
        let xf = Xf {
            style: Arc::new(self.st.text_style(&tp, &self.base)),
            fill: cp.get("background-color").and_then(color),
            borders: [side("border-top"), side("border-right"), side("border-bottom"), side("border-left")],
            wrap: cp.get("wrap-option") == Some("wrap"),
            indent: pp.len("margin-left").unwrap_or(0.0).clamp(0.0, 500.0),
            h,
            v,
            ..Xf::plain(Arc::new(self.base.clone()))
        };
        let i = self.xfs.len();
        if i < 100_000 {
            self.xfs.push(xf);
            self.by_name.insert(name.to_string(), i);
        }
        i
    }
}

fn read(pkg: &Package, limits: &Limits) -> Result<(Vec<Sheet>, CellStyles), Error> {
    if let Ok(m) = pkg.read("META-INF/manifest.xml")
        && memchr::memmem::find(&m, b"encryption-data").is_some()
    {
        return Err(Error::Encrypted);
    }
    let src = pkg.zip()?.read_string("content.xml")?;
    if src.len() as u64 > limits.max_part_bytes {
        return Err(Error::TooLarge);
    }
    // Styles come before the body: that part is read whole.
    let body_at = memchr::memmem::find(src.as_bytes(), b"<office:body").unwrap_or(src.len());
    let head = crate::ooxml::parse(&src[..body_at], limits, &[]).unwrap_or_default();
    let styles = pkg.xml("styles.xml", &[]).ok();
    let st = Styles::read(styles.as_ref(), &head);
    let mut f = Formats { st, base: Style { family: Arc::from("Liberation Sans"), size: 10.0, ..Style::default() }, by_name: HashMap::new(), xfs: Vec::new() };
    let defaults = f.st.props("table-cell", None, "text-properties");
    f.base = f.st.text_style(&defaults, &f.base);
    let default_xf = f.xf(Some("Default"));
    let mut sheets = Vec::new();
    let rest = &src[body_at..];
    let mut r = Reader::new(rest);
    loop {
        match r.next() {
            Ev::Start(t) if t.local() == "table" => {
                let hidden = t.attr("style-name").is_some_and(|s| f.st.props("table", Some(&s), "table-properties").get("display") == Some("false"));
                let sh = read_table(&mut r, &mut f, default_xf);
                if !hidden && sheets.len() < limits.max_pages.max(1) {
                    sheets.push(sh);
                }
            }
            Ev::Eof => break,
            _ => {}
        }
    }
    let default = f.xfs.get(default_xf).cloned().unwrap_or_else(|| Xf::plain(Arc::new(f.base.clone())));
    Ok((sheets, CellStyles::new(f.xfs, default)))
}

/// A sheet read from after its `<table:table>` to past its end.
fn read_table(r: &mut Reader, f: &mut Formats, default_xf: usize) -> Sheet {
    let mut sh = Sheet::new(64.0, 12.8);
    let mut col_styles: Vec<Option<String>> = Vec::new();
    let mut col = 0u32;
    let mut row = 0u32;
    let mut depth = 1usize;
    let mut row_style: Option<String> = None;
    let mut row_repeat = 1u32;
    let mut row_cells_at = 0usize;
    let cell_limits = Limits::default();
    loop {
        let ev = r.next();
        let empty = matches!(ev, Ev::Empty(_));
        match ev {
            Ev::Start(t) | Ev::Empty(t) => {
                if !empty {
                    depth += 1;
                }
                match t.local() {
                    "table-column" => {
                        let n = t.attr("number-columns-repeated").and_then(|v| v.parse::<u32>().ok()).unwrap_or(1).clamp(1, 16_384);
                        let w = t.attr("style-name").and_then(|s| f.st.props("table-column", Some(&s), "table-column-properties").len("column-width"));
                        let hidden = t.attr("visibility").is_some_and(|v| v != "visible");
                        let cs = t.attr("default-cell-style-name").map(|s| s.into_owned());
                        for _ in 0..n {
                            let c = col_styles.len() as u32;
                            if c >= MAX_COLS {
                                break;
                            }
                            if hidden {
                                sh.cols.insert(c, 0.0);
                            } else if let Some(w) = w {
                                sh.cols.insert(c, w.clamp(0.0, 2000.0));
                            }
                            col_styles.push(cs.clone());
                        }
                    }
                    "table-row" => {
                        col = 0;
                        row_repeat = t.attr("number-rows-repeated").and_then(|v| v.parse::<u32>().ok()).unwrap_or(1).clamp(1, 1_048_576);
                        row_style = t.attr("default-cell-style-name").map(|s| s.into_owned());
                        row_cells_at = sh.cells.len();
                        if row >= MAX_ROWS {
                            // The rest of the sheet is not shown.
                            skip_table(r);
                            return sh;
                        }
                        let hidden = t.attr("visibility").is_some_and(|v| v != "visible");
                        let h = t.attr("style-name").and_then(|s| f.st.props("table-row", Some(&s), "table-row-properties").len("row-height"));
                        for k in 0..row_repeat.min(MAX_ROWS) {
                            if row + k >= MAX_ROWS {
                                break;
                            }
                            if hidden {
                                sh.rows.insert(row + k, 0.0);
                            } else if let Some(h) = h {
                                sh.rows.insert(row + k, h.clamp(0.0, 1000.0));
                            }
                        }
                        if empty {
                            row = row.saturating_add(row_repeat);
                        }
                    }
                    "table-cell" | "covered-table-cell" => {
                        let covered = t.local() == "covered-table-cell";
                        let n = t.attr("number-columns-repeated").and_then(|v| v.parse::<u32>().ok()).unwrap_or(1).clamp(1, 16_384);
                        let style = t.attr("style-name").map(|s| s.into_owned()).or_else(|| row_style.clone()).or_else(|| col_styles.get(col as usize).cloned().flatten());
                        let kind = t.attr("value-type").map(|s| s.into_owned());
                        let span = |k: &str| t.attr(k).and_then(|v| v.parse::<u32>().ok()).unwrap_or(1).clamp(1, 100_000);
                        let (cs, rs) = (span("number-columns-spanned"), span("number-rows-spanned"));
                        let body = if empty { None } else { Some(r.raw_element()) };
                        if !empty {
                            depth -= 1;
                        }
                        if col >= MAX_COLS {
                            col = col.saturating_add(n);
                            continue;
                        }
                        let el = body.map(|b| crate::ooxml::parse(&format!("<c>{b}"), &cell_limits, super::TEXT).unwrap_or_default());
                        let text = el.as_ref().map(cell_text).unwrap_or_default();
                        if let Some(e) = &el {
                            frames(e, &mut sh);
                        }
                        let xf = if covered { default_xf } else { f.xf(style.as_deref()) };
                        let number = matches!(kind.as_deref(), Some("float" | "percentage" | "currency" | "date" | "time"));
                        let val = (!text.is_empty()).then(|| match kind.as_deref() {
                            Some("boolean") => Val::Text(text.clone()),
                            _ => Val::Shown(text.clone(), number),
                        });
                        if !covered && (cs > 1 || rs > 1) && sh.merges.len() < 10_000 {
                            sh.merges.push((row, col, row.saturating_add(rs - 1), col.saturating_add(cs - 1)));
                        }
                        let styled = xf != default_xf;
                        for k in 0..n {
                            let c = col + k;
                            if c >= MAX_COLS {
                                break;
                            }
                            if !covered && (val.is_some() || styled) && sh.cells.len() < 200_000 {
                                sh.cells.push(Cell { row, col: c, xf, val: val.as_ref().map(clone_val) });
                            }
                        }
                        col = col.saturating_add(n);
                    }
                    "shapes" if !empty => {
                        let body = r.raw_element();
                        depth -= 1;
                        let el = crate::ooxml::parse(&format!("<c>{body}"), &cell_limits, &[]).unwrap_or_default();
                        frames(&el, &mut sh);
                    }
                    "table" if !empty => {
                        // A table inside: skipped.
                        r.skip();
                        depth -= 1;
                    }
                    _ => {}
                }
            }
            Ev::End(name) => {
                depth -= 1;
                if local(name) == "table-row" {
                    // A repeated row with cells: its copies below.
                    if row_repeat > 1 && sh.cells.len() > row_cells_at {
                        let n = sh.cells.len();
                        for k in 1..row_repeat {
                            if row + k >= MAX_ROWS || sh.cells.len() > 200_000 {
                                break;
                            }
                            for i in row_cells_at..n {
                                let c = &sh.cells[i];
                                let copy = Cell { row: row + k, col: c.col, xf: c.xf, val: c.val.as_ref().map(clone_val) };
                                sh.cells.push(copy);
                            }
                        }
                    }
                    row = row.saturating_add(row_repeat);
                }
                if depth == 0 {
                    return sh;
                }
            }
            Ev::Eof => return sh,
            Ev::Text(_) => {}
        }
    }
}

/// Past the end of the table being read. Tables inside it are in its
/// cells, which are skipped whole: the next end of a table is its own.
fn skip_table(r: &mut Reader) {
    loop {
        match r.next() {
            Ev::Start(t) if t.local() == "table-cell" || t.local() == "covered-table-cell" => r.skip(),
            Ev::End(name) if local(name) == "table" => return,
            Ev::Eof => return,
            _ => {}
        }
    }
}

fn clone_val(v: &Val) -> Val {
    match v {
        Val::Num(n) => Val::Num(*n),
        Val::Text(s) => Val::Text(s.clone()),
        Val::Bool(b) => Val::Bool(*b),
        Val::Err(s) => Val::Err(s.clone()),
        Val::Shown(s, n) => Val::Shown(s.clone(), *n),
    }
}

/// The text a cell shows: its paragraphs, one per line.
fn cell_text(e: &El) -> String {
    let mut out = String::new();
    for p in e.children("p").take(1000) {
        if !out.is_empty() {
            out.push('\n');
        }
        let mut sp = true;
        inline(p, &mut out, &mut sp, 0);
        let t = out.trim_end_matches(' ').len();
        out.truncate(t);
    }
    out
}

fn inline(e: &El, out: &mut String, sp: &mut bool, depth: usize) {
    if depth > 16 || out.len() > 32_768 {
        return;
    }
    for k in &e.kids {
        match k {
            Node::Text(t) => out.push_str(&collapse(t, sp)),
            Node::El(c) => match c.name.as_str() {
                "s" => {
                    let n = c.attr("c").and_then(|v| v.parse::<usize>().ok()).unwrap_or(1).clamp(1, 1000);
                    out.push_str(&" ".repeat(n));
                    *sp = true;
                }
                "tab" => out.push('\t'),
                "line-break" => {
                    out.push('\n');
                    *sp = true;
                }
                "annotation" | "frame" | "custom-shape" => {}
                _ => inline(c, out, sp, depth + 1),
            },
        }
    }
}

/// Pictures and charts in `e`, placed where they are on the sheet.
fn frames(e: &El, sh: &mut Sheet) {
    for fr in e.elements().filter(|k| k.name == "frame").take(200) {
        let n = |k: &str| fr.attr(k).and_then(length).unwrap_or(0.0);
        let key = fr.children("image").find_map(|i| i.attr("href")).filter(|h| !h.contains(':')).map(|h| Arc::from(h.trim_start_matches("./")));
        let chart = fr.child("object").is_some();
        if (key.is_some() || chart) && sh.anchors.len() < 500 {
            sh.anchors.push(Anchor { from: None, off: (n("x"), n("y")), to: None, size: (n("width"), n("height")), key });
        }
    }
}
