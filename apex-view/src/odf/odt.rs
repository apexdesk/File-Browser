//! OpenDocument text (.odt) read into the model Word documents are laid
//! out from: master pages as sections, paragraphs with their styles and
//! list labels, tables, pictures in the line and anchored ones.

use std::collections::HashMap;
use std::sync::Arc;

use super::{Props, Styles, border, bullet, collapse, color, length, level_style, list_indent, number, para_format, push};
use crate::docx::read::{Block, Cell, DocModel, Float, From, PAGE, PAGES, Para, Row, Section, Table};
use crate::docx::{Doc, Source};
use crate::ooxml::{El, Node, Package};
use crate::typeset::{Align, LineRule, Object, ParaFormat, Span, Style, Tab, TabAlign};
use crate::{Error, Fonts, Limits};

const MAX_BLOCKS: usize = 200_000;

pub(crate) fn open(data: Vec<u8>, limits: &Limits, fonts: &Fonts) -> Result<Doc, Error> {
    let pkg = Package::open(data, limits)?;
    let model = read(&pkg)?;
    Ok(Doc::laid_out(Source::Package(pkg), &model, limits, fonts))
}

/// A list being read: its style and how deep it is.
#[derive(Clone)]
struct ListCtx {
    style: Option<String>,
    level: usize,
    /// The item's first paragraph takes the label.
    first: bool,
    /// Header of the item: no label (`text:list-header`).
    header: bool,
}

struct Reader {
    st: Styles,
    base: Style,
    /// List counters by list style and level.
    counters: HashMap<String, [i32; 10]>,
    blocks: usize,
    /// Pictures anchored to the page, waiting for a paragraph.
    pending: Vec<Float>,
    /// Text boxes' contents, put after the paragraph holding them.
    boxes: Vec<Block>,
    /// Paragraph style pages are set by: when one names a master page,
    /// a section ends.
    master: String,
    sections: Vec<Section>,
    page_start: Option<usize>,
}

fn read(pkg: &Package) -> Result<DocModel, Error> {
    let (content, styles) = super::read_parts(pkg)?;
    let st = Styles::read(styles.as_ref(), &content);
    let body = content.path(&["body", "text"]).ok_or_else(|| crate::ooxml::bad("the document has no text"))?;
    let mut base = Style { family: Arc::from("Liberation Serif"), size: 12.0, ..Style::default() };
    let mut r = Reader { st, base: base.clone(), counters: HashMap::new(), blocks: 0, pending: Vec::new(), boxes: Vec::new(), master: String::new(), sections: Vec::new(), page_start: None };
    let defaults = r.st.props("paragraph", None, "text-properties");
    base = r.st.text_style(&defaults, &base);
    r.base = base;
    r.master = r.st.masters.first().and_then(|m| m.attr("name")).unwrap_or("Standard").to_string();
    if r.st.master("Standard").is_some() {
        r.master = "Standard".into();
    }
    let mut out = Vec::new();
    r.blocks(body, &mut out, None, 0);
    let last = r.section(&r.master.clone());
    r.sections.push(last);
    let n = r.sections.len();
    for i in 1..n {
        let (before, rest) = r.sections.split_at_mut(i);
        let (prev, s) = (&before[i - 1], &mut rest[0]);
        s.header = s.header.take().or_else(|| prev.header.clone());
        s.footer = s.footer.take().or_else(|| prev.footer.clone());
    }
    let background = r.st.master(&r.master.clone()).and_then(|m| m.attr("page-layout-name")).and_then(|l| r.st.layouts.get(l)).and_then(|l| l.child("page-layout-properties")).and_then(|p| p.attr("background-color")).and_then(color);
    Ok(DocModel { body: out, sections: r.sections, background })
}

impl Reader {
    fn blocks(&mut self, parent: &El, out: &mut Vec<Block>, list: Option<&ListCtx>, depth: usize) {
        if depth > 32 {
            return;
        }
        for e in parent.elements() {
            if self.blocks > MAX_BLOCKS {
                return;
            }
            match e.name.as_str() {
                "p" | "h" => {
                    self.blocks += 1;
                    let is_first = list.is_some_and(|l| l.first);
                    let ctx = list.map(|l| ListCtx { first: is_first, ..l.clone() });
                    let p = self.para(e, ctx.as_ref(), out.is_empty() && depth == 0);
                    if let Some(p) = p {
                        out.push(Block::Para(p));
                    }
                    out.append(&mut self.boxes);
                }
                "list" => self.list(e, out, list, depth),
                "table" => {
                    self.blocks += 1;
                    if let Some(t) = self.table(e, depth) {
                        out.push(Block::Table(t));
                    }
                }
                "section" | "index-body" | "table-of-content" | "alphabetical-index" | "illustration-index" | "user-index" | "bibliography" | "object-index" | "table-index" | "deletion" => {
                    if e.name != "deletion" {
                        self.blocks(e, out, list, depth + 1);
                    }
                }
                "frame" => {
                    let mut spans = Vec::new();
                    let mut floats = Vec::new();
                    let st = Arc::new(self.base.clone());
                    self.frame(e, &st, &mut spans, &mut floats, depth);
                    self.pending.append(&mut floats);
                    out.append(&mut self.boxes);
                }
                _ => {}
            }
        }
    }

    fn list(&mut self, e: &El, out: &mut Vec<Block>, outer: Option<&ListCtx>, depth: usize) {
        let style = e.attr("style-name").map(str::to_string).or_else(|| outer.and_then(|o| o.style.clone()));
        let level = outer.map_or(0, |o| o.level + 1).min(9);
        if level == 0
            && let Some(s) = &style
            && e.attr("continue-numbering") != Some("true")
            && e.attr("continue-list").is_none()
        {
            self.counters.remove(s);
        }
        for item in e.elements() {
            let header = item.name == "list-header";
            if item.name != "list-item" && !header {
                continue;
            }
            let ctx = ListCtx { style: style.clone(), level, first: !header, header };
            if !header
                && let Some(s) = &style
            {
                let start = item.attr("start-value").and_then(|v| v.parse::<i32>().ok());
                let c = self.counters.entry(s.clone()).or_insert([0; 10]);
                let begin = self.st.lists.get(s).and_then(|l| level_style(l, level)).and_then(|l| l.attr("start-value")).and_then(|v| v.parse::<i32>().ok()).unwrap_or(1);
                c[level] = match start {
                    Some(v) => v,
                    None if c[level] == 0 => begin,
                    None => c[level] + 1,
                };
                for k in c.iter_mut().skip(level + 1) {
                    *k = 0;
                }
            }
            let mut first = ctx;
            for kid in item.elements() {
                match kid.name.as_str() {
                    "p" | "h" => {
                        self.blocks += 1;
                        if let Some(p) = self.para(kid, Some(&first), false) {
                            out.push(Block::Para(p));
                        }
                        out.append(&mut self.boxes);
                        first.first = false;
                    }
                    "list" => {
                        self.list(kid, out, Some(&first), depth + 1);
                        first.first = false;
                    }
                    _ => {
                        let mut tmp = Vec::new();
                        self.blocks(&wrap(kid), &mut tmp, Some(&first), depth + 1);
                        out.append(&mut tmp);
                    }
                }
                if self.blocks > MAX_BLOCKS || depth > 32 {
                    return;
                }
            }
        }
    }

    /// The page setup of master page `name`.
    fn section(&mut self, name: &str) -> Section {
        let master = self.st.master(name).cloned().unwrap_or_default();
        let layout = master.attr("page-layout-name").and_then(|l| self.st.layouts.get(l)).cloned().unwrap_or_default();
        let p = layout.child("page-layout-properties").cloned().unwrap_or_default();
        let n = |k: &str, d: f32| p.attr(k).and_then(length).unwrap_or(d);
        let width = n("page-width", 595.3).clamp(72.0, 4000.0);
        let height = n("page-height", 841.9).clamp(72.0, 4000.0);
        let all = p.attr("margin").and_then(length);
        let m = |k: &str| p.attr(k).and_then(length).or(all).unwrap_or(56.7).max(0.0);
        let mut margins = [m("margin-top"), m("margin-right"), m("margin-bottom"), m("margin-left")];
        let hf_props = |k: &str| layout.child(k).and_then(|h| h.child("header-footer-properties")).cloned();
        let read_hf = |this: &mut Reader, kind: &str| -> Option<Arc<Vec<Block>>> {
            let e = master.child(kind)?;
            if e.attr("display") == Some("false") {
                return None;
            }
            let mut out = Vec::new();
            this.blocks(e, &mut out, None, 1);
            Some(Arc::new(out))
        };
        let header = read_hf(self, "header");
        let footer = read_hf(self, "footer");
        let first_header = read_hf(self, "header-first");
        let first_footer = read_hf(self, "footer-first");
        let (header_dist, footer_dist) = (margins[0], margins[2]);
        // The body starts below the header and ends above the footer.
        if header.is_some() {
            let hp = hf_props("header-style");
            let h = hp.as_ref().and_then(|h| h.attr("min-height").or(h.attr("height"))).and_then(length).unwrap_or(14.0);
            let gap = hp.as_ref().and_then(|h| h.attr("margin-bottom")).and_then(length).unwrap_or(7.0);
            margins[0] += (h + gap).max(14.0);
        }
        if footer.is_some() {
            let fp = hf_props("footer-style");
            let h = fp.as_ref().and_then(|h| h.attr("min-height").or(h.attr("height"))).and_then(length).unwrap_or(14.0);
            let gap = fp.as_ref().and_then(|h| h.attr("margin-top")).and_then(length).unwrap_or(7.0);
            margins[2] += (h + gap).max(14.0);
        }
        if margins[1] + margins[3] > width - 36.0 {
            margins[1] = 18.0;
            margins[3] = 18.0;
        }
        if margins[0] + margins[2] > height - 36.0 {
            margins[0] = 18.0;
            margins[2] = 18.0;
        }
        let title_page = first_header.is_some() || first_footer.is_some();
        Section {
            width,
            height,
            margins,
            header_dist,
            footer_dist,
            first_header: if title_page { first_header.or(Some(Arc::new(Vec::new()))) } else { None },
            first_footer: if title_page { first_footer.or(Some(Arc::new(Vec::new()))) } else { None },
            header,
            footer,
            page_start: self.page_start.take(),
            title_page,
            new_page: true,
        }
    }

    fn para(&mut self, e: &El, list: Option<&ListCtx>, first_of_body: bool) -> Option<Para> {
        let name = e.attr("style-name").map(str::to_string);
        // A heading in the outline takes the outline's numbering.
        let outline = (e.name == "h").then(|| e.attr("outline-level").and_then(|v| v.parse::<usize>().ok()).unwrap_or(1).clamp(1, 10));
        let pp = self.st.props("paragraph", name.as_deref(), "paragraph-properties");
        let tp = self.st.props("paragraph", name.as_deref(), "text-properties");
        let style = Arc::new(self.st.text_style(&tp, &self.base));
        // A master page named starts a new section.
        let master = name.as_deref().and_then(|n| self.st.style("paragraph", n)).and_then(|s| s.attr("master-page-name")).filter(|m| !m.is_empty()).map(str::to_string);
        let mut page_break_before = pp.get("break-before") == Some("page");
        let mut section_change = None;
        if let Some(m) = master {
            if let Some(n) = pp.get("page-number").and_then(|v| v.parse::<usize>().ok()) {
                self.page_start = Some(n.min(1_000_000));
            }
            if first_of_body && self.sections.is_empty() {
                self.master = m;
            } else {
                section_change = Some(m);
                page_break_before = false;
            }
        }
        let mut fmt = para_format(&pp, &style);
        let mut spans: Vec<Span> = Vec::new();
        // The list label.
        let list_style = list.and_then(|l| l.style.clone()).or_else(|| name.as_deref().and_then(|n| self.st.attr("paragraph", n, "list-style-name")).filter(|s| !s.is_empty()));
        if let (Some(l), Some(ls)) = (list, list_style.as_deref())
            && let Some(lst) = self.st.lists.get(ls).cloned()
            && let Some(lvl) = level_style(&lst, l.level)
        {
            let (left, first) = list_indent(lvl);
            fmt.left += left;
            fmt.first = first;
            if l.first && !l.header {
                let label = self.label(lvl, ls, l.level);
                if let Some(label) = label {
                    let lstyle = lvl.child("text-properties").map(|t| {
                        let p = Props { attrs: t.attrs.iter().cloned().collect(), kids: HashMap::new() };
                        let mut s = self.st.text_style(&p, &style);
                        if let Some(f) = lvl.attr("bullet-char").and(t.attr("font-name")).and_then(|n| self.st.fonts.get(n)) {
                            if matches!(f.as_str(), "OpenSymbol" | "Symbol" | "Wingdings") {
                                s.family = style.family.clone();
                            }
                        }
                        Arc::new(s)
                    });
                    spans.push(Span::new(format!("{label}\t"), lstyle.unwrap_or_else(|| style.clone())));
                }
            }
        } else if let Some(level) = outline
            && let Some(o) = self.st.outline.clone()
            && let Some(lvl) = level_style(&o, level - 1)
            && lvl.attr("num-format").is_some_and(|f| !f.is_empty())
        {
            let c = self.counters.entry("\u{0}outline".into()).or_insert([0; 10]);
            c[level - 1] += 1;
            for k in c.iter_mut().skip(level) {
                *k = 0;
            }
            if let Some(label) = self.label(lvl, "\u{0}outline", level - 1) {
                let (left, first) = list_indent(lvl);
                if left > 0.0 || first < 0.0 {
                    fmt.left += left;
                    fmt.first = first;
                }
                spans.push(Span::new(format!("{label}\t"), style.clone()));
            }
        }
        if fmt.first < 0.0 && !fmt.tabs.iter().any(|t| t.pos == fmt.left) {
            fmt.tabs.push(Tab { pos: fmt.left, align: TabAlign::Left, leader: None });
            crate::typeset::sort_tabs(&mut fmt.tabs);
        }
        let mut floats = std::mem::take(&mut self.pending);
        let mut after_space = true;
        self.inline(e, &style, &mut spans, &mut floats, &mut after_space, 0);
        // Trailing space at the paragraph's end is not shown.
        if let Some(last) = spans.last_mut()
            && last.object.is_none()
        {
            let t = last.text.trim_end_matches(' ').len();
            last.text.truncate(t);
        }
        if spans.is_empty() {
            spans.push(Span::new("", style.clone()));
        }
        let shading = pp.get("background-color").and_then(color);
        let para = Para {
            fmt,
            spans,
            before: pp.len("margin-top").or_else(|| pp.len("margin")).unwrap_or(0.0).max(0.0),
            after: pp.len("margin-bottom").or_else(|| pp.len("margin")).unwrap_or(0.0).max(0.0),
            page_break_before,
            break_after: pp.get("break-after") == Some("page"),
            shading,
            contextual: pp.get("contextual-spacing") == Some("true"),
            style: name.unwrap_or_default(),
            floats,
            section_end: None,
        };
        if let Some(m) = section_change {
            // The section before ends: a paragraph with nothing ends it.
            let prev = std::mem::replace(&mut self.master, m);
            let s = self.section(&prev);
            self.sections.push(s);
            let end = self.sections.len() - 1;
            let mut marker = Para { fmt: ParaFormat::default(), spans: vec![Span::new("", Arc::new(Style { size: 0.5, ..self.base.clone() }))], before: 0.0, after: 0.0, page_break_before: false, break_after: false, shading: None, contextual: false, style: String::new(), floats: Vec::new(), section_end: Some(end) };
            marker.fmt.line = LineRule::Exact(0.1);
            self.boxes.insert(0, Block::Para(para));
            return Some(marker);
        }
        Some(para)
    }

    /// The label of a list item at `level` of list style `ls`.
    fn label(&mut self, lvl: &El, ls: &str, level: usize) -> Option<String> {
        match lvl.name.as_str() {
            "list-level-style-bullet" => Some(lvl.attr("bullet-char").unwrap_or("•").chars().next().map(bullet).unwrap_or('•').to_string()),
            "list-level-style-number" | "outline-level-style" => {
                let fmt = lvl.attr("num-format").unwrap_or("1");
                let shown = lvl.attr("display-levels").and_then(|v| v.parse::<usize>().ok()).unwrap_or(1).clamp(1, level + 1);
                let c = self.counters.get(ls).copied().unwrap_or([0; 10]);
                let mut parts = Vec::new();
                for k in level + 1 - shown..=level {
                    let f = if k == level { fmt.to_string() } else { self.st.lists.get(ls).or(self.st.outline.as_ref()).and_then(|l| level_style(l, k)).and_then(|l| l.attr("num-format")).unwrap_or("1").to_string() };
                    parts.push(number(c[k].max(1), &f));
                }
                let body = parts.join(".");
                if body.is_empty() && fmt.is_empty() {
                    return None;
                }
                Some(format!("{}{}{}", lvl.attr("num-prefix").unwrap_or(""), body, lvl.attr("num-suffix").unwrap_or("")))
            }
            _ => None,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn inline(&mut self, parent: &El, style: &Arc<Style>, spans: &mut Vec<Span>, floats: &mut Vec<Float>, after_space: &mut bool, depth: usize) {
        if depth > 24 {
            return;
        }
        for k in &parent.kids {
            match k {
                Node::Text(t) => {
                    let s = collapse(t, after_space);
                    if !s.is_empty() {
                        push(spans, s, style);
                    }
                }
                Node::El(e) => match e.name.as_str() {
                    "span" | "a" | "meta" | "ruby-base" => {
                        let st = match e.attr("style-name") {
                            Some(n) => {
                                let p = self.st.props("text", Some(n), "text-properties");
                                Arc::new(self.st.text_style(&p, style))
                            }
                            None => style.clone(),
                        };
                        self.inline(e, &st, spans, floats, after_space, depth + 1);
                    }
                    "s" => {
                        let n = e.attr("c").and_then(|v| v.parse::<usize>().ok()).unwrap_or(1).clamp(1, 1000);
                        push(spans, " ".repeat(n), style);
                        *after_space = true;
                    }
                    "tab" => {
                        push(spans, "\t".into(), style);
                        *after_space = true;
                    }
                    "line-break" => {
                        push(spans, "\n".into(), style);
                        *after_space = true;
                    }
                    "page-number" => {
                        push(spans, PAGE.to_string(), style);
                        *after_space = false;
                    }
                    "page-count" => {
                        push(spans, PAGES.to_string(), style);
                        *after_space = false;
                    }
                    "note" => {
                        if let Some(c) = e.child("note-citation") {
                            let st = Arc::new(Style { shift: 1, ..(**style).clone() });
                            push(spans, c.text(), &st);
                            *after_space = false;
                        }
                    }
                    "frame" => self.frame(e, style, spans, floats, depth),
                    "ruby" => {
                        if let Some(b) = e.child("ruby-base") {
                            self.inline(b, style, spans, floats, after_space, depth + 1);
                        }
                    }
                    "annotation" | "bookmark" | "bookmark-start" | "bookmark-end" | "reference-mark" | "reference-mark-start" | "reference-mark-end" | "change" | "change-start" | "change-end" | "soft-page-break" | "annotation-end" | "toc-mark" | "alphabetical-index-mark" | "custom-shape" | "rect" | "ellipse" | "line" | "g" => {}
                    _ => self.inline(e, style, spans, floats, after_space, depth + 1),
                },
            }
        }
    }

    /// A frame: its picture in the line or anchored, its text box's
    /// paragraphs after the paragraph.
    fn frame(&mut self, e: &El, style: &Arc<Style>, spans: &mut Vec<Span>, floats: &mut Vec<Float>, depth: usize) {
        let w = e.attr("width").and_then(length).unwrap_or(0.0);
        let h = e.attr("height").or(e.attr("min-height")).and_then(length).unwrap_or(0.0);
        let anchor = e.attr("anchor-type").unwrap_or("paragraph");
        if let Some(tb) = e.child("text-box") {
            if depth < 8 {
                let mut out = Vec::new();
                self.blocks(tb, &mut out, None, depth + 4);
                self.boxes.append(&mut out);
            }
            return;
        }
        let Some(img) = e.children("image").find(|i| i.attr("href").is_some_and(|h| !h.contains(':'))) else { return };
        let href = img.attr("href").unwrap_or("").trim_start_matches("./");
        if href.is_empty() || !(w > 0.5 && h > 0.5 && w < 20_000.0 && h < 20_000.0) {
            return;
        }
        let key: Arc<str> = Arc::from(href);
        if anchor == "as-char" {
            spans.push(Span { text: String::new(), style: style.clone(), object: Some(Object { w, h, key }) });
            return;
        }
        let gp = self.st.props("graphic", e.attr("style-name"), "graphic-properties");
        let x_from = match (anchor, gp.get("horizontal-rel")) {
            ("page", _) | (_, Some("page" | "page-content" | "page-start-margin" | "page-end-margin")) => From::Page,
            _ => From::Margin,
        };
        let y_from = if anchor == "page" || gp.get("vertical-rel").is_some_and(|r| r.starts_with("page")) { From::Page } else { From::Para };
        let x = match gp.get("horizontal-pos") {
            Some("center") => f32::NAN,
            Some("right" | "outside") => f32::INFINITY,
            Some("left" | "inside") => 0.0,
            _ => e.attr("x").and_then(length).unwrap_or(0.0),
        };
        let y = match gp.get("vertical-pos") {
            Some("middle") => f32::NAN,
            Some("bottom") => f32::INFINITY,
            Some("top") => 0.0,
            _ => e.attr("y").and_then(length).unwrap_or(0.0),
        };
        let behind = gp.get("run-through") == Some("background");
        floats.push(Float { w, h, x, y, x_from, y_from, key, behind });
    }

    fn table(&mut self, t: &El, depth: usize) -> Option<Table> {
        if depth > 12 {
            return None;
        }
        let tp = self.st.props("table", t.attr("style-name"), "table-properties");
        let mut cols = Vec::new();
        let mut rows = Vec::new();
        let mut down = Vec::new();
        self.table_parts(t, &mut cols, &mut rows, &mut down, false, depth);
        if rows.is_empty() {
            return None;
        }
        let ncols = rows.iter().map(|r: &Row| r.cells.iter().map(|c| c.span).sum::<usize>()).max().unwrap_or(1).max(1);
        // Columns with no width share what the table leaves.
        let total = tp.len("width").unwrap_or(0.0);
        cols.resize(ncols, None);
        let known: f32 = cols.iter().flatten().sum();
        let unknown = cols.iter().filter(|c| c.is_none()).count();
        let share = if unknown > 0 { ((total - known).max(0.0) / unknown as f32).max(if total > 0.0 { 0.0 } else { 72.0 }) } else { 0.0 };
        let mut widths: Vec<f32> = cols.iter().map(|c| c.unwrap_or(share).max(2.0)).collect();
        widths.truncate(ncols);
        let align = match tp.get("align") {
            Some("center") => Align::Center,
            Some("right") => Align::Right,
            _ => Align::Left,
        };
        Some(Table { cols: widths, rows, indent: tp.len("margin-left").unwrap_or(0.0), align })
    }

    /// Columns and rows of a table; `down` holds, by column, the rows a
    /// cell above still spans and its width (0 inside it).
    #[allow(clippy::too_many_arguments)]
    fn table_parts(&mut self, parent: &El, cols: &mut Vec<Option<f32>>, rows: &mut Vec<Row>, down: &mut Vec<(usize, usize)>, header: bool, depth: usize) {
        for e in parent.elements() {
            match e.name.as_str() {
                "table-column" => {
                    let n = e.attr("number-columns-repeated").and_then(|v| v.parse::<usize>().ok()).unwrap_or(1).clamp(1, 1024);
                    let cp = self.st.props("table-column", e.attr("style-name"), "table-column-properties");
                    let w = cp.len("column-width");
                    for _ in 0..n {
                        if cols.len() < 1024 {
                            cols.push(w);
                        }
                    }
                }
                "table-columns" | "table-column-group" | "table-header-columns" => self.table_parts(e, cols, rows, down, header, depth),
                "table-header-rows" => self.table_parts(e, cols, rows, down, true, depth),
                "table-rows" | "table-row-group" => self.table_parts(e, cols, rows, down, header, depth),
                "table-row" => {
                    let n = e.attr("number-rows-repeated").and_then(|v| v.parse::<usize>().ok()).unwrap_or(1).clamp(1, 100);
                    let rp = self.st.props("table-row", e.attr("style-name"), "table-row-properties");
                    let (height, exact) = match (rp.len("row-height"), rp.len("min-row-height")) {
                        (Some(h), _) if rp.get("use-optimal-row-height") != Some("true") => (h, true),
                        (_, Some(h)) => (h, false),
                        (Some(h), None) => (h, false),
                        _ => (0.0, false),
                    };
                    let cells = self.row_cells(e, down, depth);
                    for d in down.iter_mut() {
                        d.0 = d.0.saturating_sub(1);
                    }
                    // A repeated row: copies of it after.
                    for _ in 1..n {
                        if rows.len() > 5_000 {
                            return;
                        }
                        rows.push(Row { cells: cells.iter().map(clone_cell).collect(), height, exact, header });
                    }
                    rows.push(Row { cells, height, exact, header });
                }
                _ => {}
            }
        }
    }

    fn row_cells(&mut self, row: &El, down: &mut Vec<(usize, usize)>, depth: usize) -> Vec<Cell> {
        let mut cells = Vec::new();
        let mut col = 0usize;
        for e in row.elements() {
            let covered = e.name == "covered-table-cell";
            if e.name != "table-cell" && !covered {
                continue;
            }
            let rep = e.attr("number-columns-repeated").and_then(|v| v.parse::<usize>().ok()).unwrap_or(1).clamp(1, 1024);
            if covered {
                // Under a cell spanning rows: merged into it. Beside one
                // spanning columns: left out.
                for _ in 0..rep {
                    if let Some(&(left, span)) = down.get(col)
                        && left > 0
                        && span > 0
                    {
                        cells.push(Cell { span, merged: true, blocks: Vec::new(), fill: None, borders: [None; 4], margins: [0.0; 4], valign: 0 });
                    }
                    col += 1;
                }
                continue;
            }
            let span = e.attr("number-columns-spanned").and_then(|v| v.parse::<usize>().ok()).unwrap_or(1).clamp(1, 1024);
            let row_span = e.attr("number-rows-spanned").and_then(|v| v.parse::<usize>().ok()).unwrap_or(1).clamp(1, 10_000);
            let cp = self.st.props("table-cell", e.attr("style-name"), "table-cell-properties");
            let all = cp.get("border").and_then(border);
            let side = |k: &str| match cp.get(k) {
                Some(v) => border(v),
                None => all,
            };
            let pad_all = cp.len("padding").unwrap_or(2.0);
            let pad = |k: &str| cp.len(k).unwrap_or(pad_all).max(0.0);
            let mut blocks = Vec::new();
            if depth < 12 {
                self.blocks(e, &mut blocks, None, depth + 2);
            }
            let cell = Cell {
                span,
                merged: false,
                blocks,
                fill: cp.get("background-color").and_then(color),
                borders: [side("border-top"), side("border-left"), side("border-bottom"), side("border-right")],
                margins: [pad("padding-top"), pad("padding-left"), pad("padding-bottom"), pad("padding-right")],
                valign: match cp.get("vertical-align") {
                    Some("middle") => 1,
                    Some("bottom") => 2,
                    _ => 0,
                },
            };
            for i in 0..rep.min(64) {
                if col + span > 1024 {
                    break;
                }
                if down.len() < col + span {
                    down.resize(col + span, (0, 0));
                }
                if row_span > 1 {
                    down[col] = (row_span, span);
                    for d in &mut down[col + 1..col + span] {
                        *d = (row_span, 0);
                    }
                }
                let _ = i;
                cells.push(clone_cell(&cell));
                col += span;
            }
        }
        cells
    }
}

fn clone_cell(c: &Cell) -> Cell {
    Cell { span: c.span, merged: c.merged, blocks: c.blocks.iter().map(clone_block).collect(), fill: c.fill, borders: c.borders, margins: c.margins, valign: c.valign }
}

fn clone_block(b: &Block) -> Block {
    match b {
        Block::Para(p) => Block::Para(Para {
            fmt: p.fmt.clone(),
            spans: p.spans.clone(),
            before: p.before,
            after: p.after,
            page_break_before: p.page_break_before,
            break_after: p.break_after,
            shading: p.shading,
            contextual: p.contextual,
            style: p.style.clone(),
            floats: Vec::new(),
            section_end: None,
        }),
        Block::Table(t) => Block::Table(Table { cols: t.cols.clone(), rows: t.rows.iter().map(|r| Row { cells: r.cells.iter().map(clone_cell).collect(), height: r.height, exact: r.exact, header: r.header }).collect(), indent: t.indent, align: t.align }),
    }
}

/// An element with only `e` inside, to read it as blocks.
fn wrap(e: &El) -> El {
    El { name: String::new(), attrs: Vec::new(), kids: vec![Node::El(e.clone())] }
}
