//! A read .docx laid out into pages: paragraphs set into lines and broken
//! across pages, tables row by row (a row is never split), headers and
//! footers with their page numbers.

use vello_cpu::kurbo::Affine;

use super::read::{Block, DocModel, Float, From, PAGE, PAGES, Para, SECTION_PAGES, Section, Table};
use crate::draw::{Item, Page};
use crate::text::TextRun;
use crate::typeset::{self, Align, FontCache, Span};

#[derive(Clone, Copy)]
struct Geom {
    w: f32,
    h: f32,
    /// Top, right, bottom, left.
    m: [f32; 4],
    new_page: bool,
}

impl Geom {
    fn of(s: &Section) -> Geom {
        Geom { w: s.width, h: s.height, m: s.margins, new_page: s.new_page }
    }
}

/// A page laid out, with the section it belongs to.
struct Out {
    page: Page,
    sec: usize,
    first_of_sec: bool,
}

/// Where blocks are laid out: the pages of the body, or one box with no
/// bottom (a table cell, a header).
struct Frame {
    paged: bool,
    geoms: Vec<Geom>,
    /// The page's geometry, for pictures placed from the page.
    page: Option<Geom>,
    sec: usize,
    first_of_sec: bool,
    pages: Vec<Out>,
    max_pages: usize,
    full: bool,
    items: Vec<Item>,
    text: Vec<TextRun>,
    x: f32,
    width: f32,
    y: f32,
    top: f32,
    bottom: f32,
    /// The paragraph before: its space after, style, and whether it
    /// leaves out space between paragraphs of its style.
    prev: Option<(f32, String, bool)>,
    /// Page number, page count and the section's page count, shown by
    /// page fields.
    numbers: [usize; 3],
    depth: usize,
}

impl Frame {
    fn plain(x: f32, width: f32, y: f32, page: Option<Geom>, numbers: [usize; 3], depth: usize) -> Frame {
        Frame {
            paged: false,
            geoms: Vec::new(),
            page,
            sec: 0,
            first_of_sec: false,
            pages: Vec::new(),
            max_pages: 0,
            full: false,
            items: Vec::new(),
            text: Vec::new(),
            x,
            width,
            y,
            top: y,
            bottom: f32::INFINITY,
            prev: None,
            numbers,
            depth,
        }
    }

    fn body(geoms: Vec<Geom>, max_pages: usize) -> Frame {
        let mut f = Frame::plain(0.0, 0.0, 0.0, None, [1, 1, 1], 0);
        f.paged = true;
        f.geoms = geoms;
        f.max_pages = max_pages.max(1);
        f.set_geometry(0);
        f.first_of_sec = true;
        f
    }

    fn set_geometry(&mut self, sec: usize) {
        let g = self.geoms.get(sec).copied().unwrap_or(Geom { w: 612.0, h: 792.0, m: [72.0; 4], new_page: true });
        self.sec = sec;
        self.page = Some(g);
        self.x = g.m[3];
        self.width = (g.w - g.m[1] - g.m[3]).max(36.0);
        self.top = g.m[0];
        self.bottom = (g.h - g.m[2]).max(self.top + 36.0);
        self.y = self.top;
    }

    /// The page in progress closed and kept.
    fn close_page(&mut self) {
        let g = self.page.unwrap_or(Geom { w: 612.0, h: 792.0, m: [72.0; 4], new_page: true });
        let page = Page { width: g.w, height: g.h, background: None, items: std::mem::take(&mut self.items), text: std::mem::take(&mut self.text) };
        self.pages.push(Out { page, sec: self.sec, first_of_sec: self.first_of_sec });
        if self.pages.len() >= self.max_pages {
            self.full = true;
        }
    }

    /// Goes on to a new page of section `sec`.
    fn new_page_of(&mut self, sec: usize, first: bool) {
        if !self.paged {
            return;
        }
        self.close_page();
        self.set_geometry(sec);
        self.first_of_sec = first;
        self.prev = None;
        self.numbers[0] = self.pages.len() + 1;
    }

    fn new_page(&mut self) {
        let s = self.sec;
        self.new_page_of(s, false);
    }

    fn at_top(&self) -> bool {
        self.y <= self.top + 0.01
    }
}

/// Lays out the document into its pages.
pub(crate) fn pages(model: &DocModel, fonts: &mut FontCache, max_pages: usize) -> Vec<Page> {
    let geoms: Vec<Geom> = model.sections.iter().map(Geom::of).collect();
    let mut f = Frame::body(geoms, max_pages);
    blocks(fonts, &mut f, &model.body);
    f.close_page();
    let n = f.pages.len();
    let mut per_section = vec![0usize; model.sections.len() + 1];
    for o in &f.pages {
        per_section[o.sec.min(model.sections.len())] += 1;
    }
    let mut out = Vec::with_capacity(n);
    let mut number = 0usize;
    for o in f.pages {
        let mut page = o.page;
        page.background = model.background;
        let s = model.sections.get(o.sec);
        number = match s.and_then(|s| s.page_start) {
            Some(start) if o.first_of_sec => start,
            _ => number + 1,
        };
        if let Some(s) = s {
            decorate(fonts, &mut page, s, o.first_of_sec, [number, n, per_section[o.sec]]);
        }
        out.push(page);
    }
    out
}

/// The header and footer of a page added to it.
fn decorate(fonts: &mut FontCache, page: &mut Page, s: &Section, first: bool, numbers: [usize; 3]) {
    let g = Geom::of(s);
    let (header, footer) = if s.title_page && first { (&s.first_header, &s.first_footer) } else { (&s.header, &s.footer) };
    let width = (g.w - g.m[1] - g.m[3]).max(36.0);
    let mut items = Vec::new();
    let mut text = Vec::new();
    if let Some(h) = header {
        let mut f = Frame::plain(g.m[3], width, s.header_dist, Some(g), numbers, 0);
        blocks(fonts, &mut f, h);
        items.append(&mut f.items);
        text.append(&mut f.text);
    }
    if let Some(ft) = footer {
        let mut f = Frame::plain(g.m[3], width, 0.0, Some(g), numbers, 0);
        blocks(fonts, &mut f, ft);
        let dy = g.h - s.footer_dist - f.y;
        translate(&mut f.items, &mut f.text, 0.0, dy);
        items.append(&mut f.items);
        text.append(&mut f.text);
    }
    if !items.is_empty() {
        items.append(&mut page.items);
        page.items = items;
    }
    text.append(&mut page.text);
    page.text = text;
}

fn blocks(fonts: &mut FontCache, f: &mut Frame, blocks: &[Block]) {
    for b in blocks {
        if f.full {
            return;
        }
        match b {
            Block::Para(p) => {
                para(fonts, f, p);
                if f.paged
                    && let Some(i) = p.section_end
                {
                    let next = i + 1;
                    if next < f.geoms.len() {
                        if f.geoms[next].new_page {
                            f.new_page_of(next, true);
                        } else {
                            // A continuous section goes on in the page.
                            f.sec = next;
                        }
                    }
                }
            }
            Block::Table(t) => table(fonts, f, t),
        }
    }
}

/// Page fields given their numbers.
fn numbered(spans: &[Span], [page, pages, section]: [usize; 3]) -> Vec<Span> {
    spans
        .iter()
        .map(|s| {
            if s.text.contains([PAGE, PAGES, SECTION_PAGES]) {
                let mut s = s.clone();
                s.text = s.text.replace(PAGE, &page.to_string()).replace(PAGES, &pages.to_string()).replace(SECTION_PAGES, &section.to_string());
                s
            } else {
                s.clone()
            }
        })
        .collect()
}

fn para(fonts: &mut FontCache, f: &mut Frame, p: &Para) {
    if p.page_break_before && f.paged && !(f.at_top() && f.items.is_empty()) {
        f.new_page();
    }
    let spans = if p.spans.iter().any(|s| s.text.contains([PAGE, PAGES, SECTION_PAGES])) { numbered(&p.spans, f.numbers) } else { p.spans.clone() };
    let same = |style: &str| f.prev.as_ref().is_some_and(|(_, s, _)| s == style);
    let mut space = match &f.prev {
        Some((after, _, ctx)) if !(*ctx && same(&p.style)) => *after,
        _ => 0.0,
    };
    if !(p.contextual && same(&p.style)) {
        space += p.before;
    }
    // Space before is left out at the top of a page after the first.
    if f.paged && f.at_top() && !f.pages.is_empty() {
        space = 0.0;
    }
    f.y += space.clamp(0.0, 1000.0);
    let lines = typeset::set(&spans, &p.fmt, f.width, fonts);
    let mut top = f.y;
    for (i, line) in lines.lines.iter().enumerate() {
        if f.paged && f.y + line.height > f.bottom && !f.at_top() {
            f.new_page();
            if f.full {
                return;
            }
            if i == 0 {
                top = f.y;
            }
        }
        if let Some(c) = p.shading {
            f.items.push(Item::Rect { x: f.x + p.fmt.left, y: f.y, w: (f.width - p.fmt.left - p.fmt.right).max(0.0), h: line.height, color: c });
        }
        lines.draw_line(i, f.x, f.y, &mut f.items, &mut f.text);
        f.y += line.height;
    }
    for fl in &p.floats {
        float(f, fl, top);
    }
    f.prev = Some((p.after, p.style.clone(), p.contextual));
    if p.break_after && f.paged {
        f.new_page();
    }
}

/// A picture placed by its anchor.
fn float(f: &mut Frame, fl: &Float, para_top: f32) {
    let page = f.page.filter(|_| f.paged || f.depth == 0);
    let place = |off: f32, from: From, along_x: bool, size: f32| -> f32 {
        let (start, len) = match (from, page, along_x) {
            (From::Page, Some(g), true) => (0.0, g.w),
            (From::Page, Some(g), false) => (0.0, g.h),
            (From::Margin, Some(g), true) => (g.m[3], g.w - g.m[1] - g.m[3]),
            (From::Margin, Some(g), false) => (g.m[0], g.h - g.m[0] - g.m[2]),
            (_, _, true) => (f.x, f.width),
            (_, _, false) => (para_top, 0.0),
        };
        if off.is_nan() {
            start + (len - size) / 2.0
        } else if off.is_infinite() {
            start + len - size
        } else {
            start + off
        }
    };
    let x = place(fl.x, fl.x_from, true, fl.w);
    let y = place(fl.y, fl.y_from, false, fl.h);
    let item = Item::Image { x, y, w: fl.w, h: fl.h, key: fl.key.clone() };
    if fl.behind {
        f.items.insert(0, item);
    } else {
        f.items.push(item);
    }
}

/// A cell laid out on its own: its items from y = 0, and its height.
struct CellBox {
    items: Vec<Item>,
    text: Vec<TextRun>,
    height: f32,
    /// Rows it spans down.
    rows: usize,
}

fn table(fonts: &mut FontCache, f: &mut Frame, t: &Table) {
    if f.depth > 12 || t.cols.is_empty() {
        return;
    }
    let total: f32 = t.cols.iter().sum();
    let x0 = f.x
        + match t.align {
            Align::Center => (f.width - total) / 2.0,
            Align::Right => f.width - total,
            _ => t.indent,
        };
    let mut col_x = vec![x0];
    for w in &t.cols {
        col_x.push(col_x.last().copied().unwrap_or(x0) + w);
    }
    let ncols = t.cols.len();
    // Each cell's first grid column; and which columns continue a merge.
    let grid: Vec<Vec<usize>> = t
        .rows
        .iter()
        .map(|r| {
            let mut c = 0;
            r.cells
                .iter()
                .map(|cell| {
                    let at = c.min(ncols);
                    c += cell.span;
                    at
                })
                .collect()
        })
        .collect();
    let continues = |ri: usize, col: usize| -> bool {
        t.rows.get(ri).is_some_and(|r| r.cells.iter().zip(&grid[ri]).any(|(c, &at)| at == col && c.merged))
    };
    // Cells laid out, and the rows' heights.
    let mut boxes: Vec<Vec<Option<CellBox>>> = Vec::with_capacity(t.rows.len());
    let mut heights: Vec<f32> = Vec::with_capacity(t.rows.len());
    for (ri, row) in t.rows.iter().enumerate() {
        let mut need = 0f32;
        let mut cells = Vec::with_capacity(row.cells.len());
        for (ci, cell) in row.cells.iter().enumerate() {
            if cell.merged {
                cells.push(None);
                continue;
            }
            let at = grid[ri][ci];
            let end = (at + cell.span).min(ncols);
            let (cx, cw) = (col_x[at], col_x[end] - col_x[at]);
            let [mt, ml, mb, mr] = cell.margins;
            let mut cf = Frame::plain(cx + ml, (cw - ml - mr).max(4.0), 0.0, None, f.numbers, f.depth + 1);
            blocks(fonts, &mut cf, &cell.blocks);
            let height = cf.y + mt + mb;
            let mut rows = 1;
            while ri + rows < t.rows.len() && continues(ri + rows, at) {
                rows += 1;
            }
            if rows == 1 {
                need = need.max(height);
            }
            cells.push(Some(CellBox { items: cf.items, text: cf.text, height, rows }));
        }
        let h = if row.exact { row.height } else { need.max(row.height) };
        heights.push(h.max(1.0));
        boxes.push(cells);
    }
    // Cells spanning rows make their last row taller when they need it.
    for ri in 0..t.rows.len() {
        for b in boxes[ri].iter().flatten() {
            if b.rows > 1 {
                let last = (ri + b.rows - 1).min(heights.len() - 1);
                let have: f32 = heights[ri..=last].iter().sum();
                if b.height > have && !t.rows[last].exact {
                    heights[last] += b.height - have;
                }
            }
        }
    }
    // Rows marked as headers at the top are drawn again on each page.
    let header_rows = t.rows.iter().take_while(|r| r.header).count();
    for ri in 0..t.rows.len() {
        let h = heights[ri];
        if f.paged && f.y + h > f.bottom && !f.at_top() {
            f.new_page();
            if ri >= header_rows && header_rows < t.rows.len() {
                let total: f32 = heights[..header_rows].iter().sum();
                if total < (f.bottom - f.top) / 2.0 {
                    for hr in 0..header_rows {
                        draw_row(f, t, hr, &heights, &grid, &col_x, &mut boxes[hr], true);
                    }
                }
            }
        }
        if f.full {
            return;
        }
        draw_row(f, t, ri, &heights, &grid, &col_x, &mut boxes[ri], ri < header_rows);
    }
    f.prev = None;
}

/// Row `ri` of a table drawn at the frame's `y`, which it moves down.
/// `keep` leaves the cells' items in place to draw again.
#[allow(clippy::too_many_arguments)]
fn draw_row(f: &mut Frame, t: &Table, ri: usize, heights: &[f32], grid: &[Vec<usize>], col_x: &[f32], boxes: &mut [Option<CellBox>], keep: bool) {
    let ncols = t.cols.len();
    let continues = |ri: usize, col: usize| -> bool { t.rows.get(ri).is_some_and(|r| r.cells.iter().zip(&grid[ri]).any(|(c, &at)| at == col && c.merged)) };
    let row = &t.rows[ri];
    let h = heights[ri];
    let y = f.y;
    for (ci, cell) in row.cells.iter().enumerate() {
        let at = grid[ri][ci];
        if at >= ncols {
            continue;
        }
        let end = (at + cell.span).min(ncols);
        let (cx, cw) = (col_x[at], col_x[end] - col_x[at]);
        if let Some(c) = cell.fill {
            f.items.push(Item::Rect { x: cx, y, w: cw, h, color: c });
        }
        if let Some(Some(b)) = boxes.get_mut(ci) {
            let span_h: f32 = heights[ri..(ri + b.rows).min(heights.len())].iter().sum();
            let dy = y
                + cell.margins[0]
                + match cell.valign {
                    1 => ((span_h - b.height) / 2.0).max(0.0),
                    2 => (span_h - b.height).max(0.0),
                    _ => 0.0,
                };
            let (mut items, mut text) = if keep { (b.items.clone(), b.text.clone()) } else { (std::mem::take(&mut b.items), std::mem::take(&mut b.text)) };
            translate(&mut items, &mut text, 0.0, dy);
            let clip = row.exact && b.height > h;
            if clip {
                f.items.push(Item::Clip { x: cx, y, w: cw, h });
            }
            f.items.append(&mut items);
            if clip {
                f.items.push(Item::Unclip);
            }
            f.text.append(&mut text);
        }
        let [top, left, bottom, right] = cell.borders;
        let mut line = |b: Option<(f32, crate::draw::Rgba)>, x0: f32, y0: f32, x1: f32, y1: f32| {
            if let Some((w, c)) = b {
                f.items.push(Item::Line { x0, y0, x1, y1, width: w, color: c });
            }
        };
        if !cell.merged {
            line(top, cx, y, cx + cw, y);
        }
        if !continues(ri + 1, at) {
            line(bottom, cx, y + h, cx + cw, y + h);
        }
        line(left, cx, y, cx, y + h);
        line(right, cx + cw, y, cx + cw, y + h);
    }
    f.y += h;
}

/// Items and text moved by `(dx, dy)`.
fn translate(items: &mut [Item], text: &mut [TextRun], dx: f32, dy: f32) {
    for it in items {
        match it {
            Item::Rect { x, y, .. } | Item::Image { x, y, .. } | Item::Clip { x, y, .. } => {
                *x += dx;
                *y += dy;
            }
            Item::Line { x0, y0, x1, y1, .. } => {
                *x0 += dx;
                *x1 += dx;
                *y0 += dy;
                *y1 += dy;
            }
            Item::Path { path, .. } => path.apply_affine(Affine::translate((dx as f64, dy as f64))),
            Item::Glyphs(g) => {
                g.x += dx;
                g.y += dy;
            }
            Item::Unclip => {}
        }
    }
    for t in text {
        t.x += dx;
        t.y += dy;
    }
}
