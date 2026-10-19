#![cfg_attr(not(feature = "docx"), allow(dead_code))]

//! A .docx read into what its pages show: sections with their page setup,
//! headers and footers; paragraphs with their resolved formatting, list
//! labels and pictures; and tables.

use std::collections::HashMap;
use std::sync::Arc;

use crate::draw::Rgba;
use crate::ooxml::{El, Package, Rel};
use crate::typeset::{Align, LineRule, Object, ParaFormat, Span, Style, Tab, TabAlign};
use crate::Error;

/// Character standing for the current page number, and for the count.
pub(crate) const PAGE: char = '\u{E000}';
pub(crate) const PAGES: char = '\u{E001}';
pub(crate) const SECTION_PAGES: char = '\u{E002}';

pub(crate) enum Block {
    Para(Para),
    Table(Table),
}

pub(crate) struct Para {
    pub fmt: ParaFormat,
    pub spans: Vec<Span>,
    /// Points.
    pub before: f32,
    pub after: f32,
    pub page_break_before: bool,
    /// Breaks the page after this paragraph's text (a page break run at
    /// its end), and the paragraph ending a section.
    pub break_after: bool,
    pub shading: Option<Rgba>,
    /// Space between paragraphs of one style left out.
    pub contextual: bool,
    pub style: String,
    /// Pictures placed on the page, not in the line.
    pub floats: Vec<Float>,
    /// The paragraph is a section's last: what follows starts this one.
    pub section_end: Option<usize>,
}

/// A picture anchored to the page, a margin or the paragraph.
pub(crate) struct Float {
    pub w: f32,
    pub h: f32,
    /// Offsets in points and what they are from.
    pub x: f32,
    pub y: f32,
    pub x_from: From,
    pub y_from: From,
    pub key: Arc<str>,
    pub behind: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum From {
    Page,
    Margin,
    Para,
}

pub(crate) struct Table {
    /// Column widths in points.
    pub cols: Vec<f32>,
    pub rows: Vec<Row>,
    /// Left indent in points.
    pub indent: f32,
    pub align: Align,
}

pub(crate) struct Row {
    pub cells: Vec<Cell>,
    /// Minimum (or exact) height in points.
    pub height: f32,
    pub exact: bool,
    pub header: bool,
}

pub(crate) struct Cell {
    pub span: usize,
    /// Merged into the cell above.
    pub merged: bool,
    pub blocks: Vec<Block>,
    pub fill: Option<Rgba>,
    /// Top, left, bottom, right: width in points and colour.
    pub borders: [Option<(f32, Rgba)>; 4],
    pub margins: [f32; 4],
    pub valign: u8,
}

pub(crate) struct Section {
    pub width: f32,
    pub height: f32,
    /// Top, right, bottom, left.
    pub margins: [f32; 4],
    pub header_dist: f32,
    pub footer_dist: f32,
    /// Headers and footers: default, then first page's. A section
    /// without one has its previous section's.
    pub header: Option<Arc<Vec<Block>>>,
    pub footer: Option<Arc<Vec<Block>>>,
    pub first_header: Option<Arc<Vec<Block>>>,
    pub first_footer: Option<Arc<Vec<Block>>>,
    /// Page numbers start again here.
    pub page_start: Option<usize>,
    pub title_page: bool,
    /// Starts on a new page (else continues the page).
    pub new_page: bool,
}

pub(crate) struct DocModel {
    pub body: Vec<Block>,
    pub sections: Vec<Section>,
    pub background: Option<Rgba>,
}

// --- styles -------------------------------------------------------------

/// Run properties, each set or inherited.
#[derive(Clone, Default, Debug)]
struct RProps {
    font: Option<String>,
    east_asia: Option<String>,
    size: Option<f32>,
    bold: Option<bool>,
    italic: Option<bool>,
    color: Option<Rgba>,
    underline: Option<bool>,
    strike: Option<bool>,
    highlight: Option<Option<Rgba>>,
    shift: Option<i8>,
    caps: Option<bool>,
    hidden: Option<bool>,
}

impl RProps {
    fn over(&mut self, o: &RProps) {
        macro_rules! take {
            ($($f:ident),*) => { $( if o.$f.is_some() { self.$f = o.$f.clone(); } )* };
        }
        take!(font, east_asia, size, bold, italic, color, underline, strike, highlight, shift, caps, hidden);
    }
}

#[derive(Clone, Default, Debug)]
struct PProps {
    align: Option<Align>,
    left: Option<f32>,
    right: Option<f32>,
    first: Option<f32>,
    before: Option<f32>,
    after: Option<f32>,
    line: Option<LineRule>,
    /// Tab stops set and cleared (None), merged with the style's.
    tabs: Option<Vec<(f32, Option<Tab>)>>,
    page_break_before: Option<bool>,
    shading: Option<Option<Rgba>>,
    contextual: Option<bool>,
    num: Option<(String, usize)>,
}

impl PProps {
    fn over(&mut self, o: &PProps) {
        macro_rules! take {
            ($($f:ident),*) => { $( if o.$f.is_some() { self.$f = o.$f.clone(); } )* };
        }
        take!(align, left, right, first, before, after, line, page_break_before, shading, contextual, num);
        if let Some(own) = &o.tabs {
            let mut v = self.tabs.take().unwrap_or_default();
            for t in own {
                v.retain(|(p, _)| (p - t.0).abs() > 0.5);
                v.push(*t);
            }
            v.sort_by(|a, b| a.0.total_cmp(&b.0));
            v.truncate(64);
            self.tabs = Some(v);
        }
    }
}

struct StyleDef {
    based_on: Option<String>,
    r: RProps,
    p: PProps,
    /// Table styles: borders of the whole table (top, left, bottom,
    /// right, inside horizontal, inside vertical).
    table_borders: Option<[Option<(f32, Rgba)>; 6]>,
}

struct Level {
    format: String,
    text: String,
    start: i32,
    p: PProps,
    r: RProps,
}

struct Ctx<'a> {
    pkg: &'a Package,
    styles: HashMap<String, StyleDef>,
    default_para: Option<String>,
    default_table: Option<String>,
    defaults_r: RProps,
    defaults_p: PProps,
    /// Theme fonts: minor and major Latin, minor and major East Asian.
    theme: [String; 4],
    /// Numbering: num id → abstract levels (with overrides of start).
    nums: HashMap<String, Arc<Vec<Level>>>,
    counters: HashMap<String, Vec<i32>>,
    /// Relationships of the part being read, and its folder.
    rels: HashMap<String, Rel>,
    /// Paragraphs read, against documents of millions.
    paras: usize,
    depth: usize,
}

const MAX_PARAS: usize = 400_000;

fn twips(v: f32) -> f32 {
    v / 20.0
}

fn emu(v: f32) -> f32 {
    v / 12_700.0
}

fn color_of(v: &str) -> Option<Rgba> {
    if v.len() == 6 && v != "auto" {
        u32::from_str_radix(v, 16).ok().map(Rgba::rgb)
    } else {
        None
    }
}

fn highlight(name: &str) -> Option<Rgba> {
    let rgb = match name {
        "yellow" => 0xFFFF00,
        "green" => 0x00FF00,
        "cyan" => 0x00FFFF,
        "magenta" => 0xFF00FF,
        "blue" => 0x0000FF,
        "red" => 0xFF0000,
        "darkBlue" => 0x000080,
        "darkCyan" => 0x008080,
        "darkGreen" => 0x008000,
        "darkMagenta" => 0x800080,
        "darkRed" => 0x800000,
        "darkYellow" => 0x808000,
        "darkGray" => 0x808080,
        "lightGray" => 0xC0C0C0,
        "black" => 0x000000,
        "white" => 0xFFFFFF,
        _ => return None,
    };
    Some(Rgba::rgb(rgb))
}

fn r_props(rpr: &El, theme: &[String; 4]) -> RProps {
    let mut r = RProps::default();
    if let Some(f) = rpr.child("rFonts") {
        let pick = |k: &str, tk: &str| -> Option<String> {
            if let Some(t) = f.attr(tk) {
                return Some(if t.starts_with("major") { if tk == "eastAsiaTheme" { &theme[3] } else { &theme[1] } } else if tk == "eastAsiaTheme" { &theme[2] } else { &theme[0] }.clone());
            }
            f.attr(k).map(str::to_string)
        };
        r.font = pick("ascii", "asciiTheme").or_else(|| pick("hAnsi", "hAnsiTheme"));
        r.east_asia = pick("eastAsia", "eastAsiaTheme");
    }
    r.size = rpr.child("sz").and_then(|e| e.num("val")).map(|v| (v / 2.0).clamp(1.0, 1638.0));
    r.bold = rpr.flag("b");
    r.italic = rpr.flag("i");
    r.strike = rpr.flag("strike").or(rpr.flag("dstrike"));
    r.caps = rpr.flag("caps");
    r.hidden = rpr.flag("vanish");
    if let Some(c) = rpr.val("color") {
        r.color = Some(color_of(c).unwrap_or(Rgba::BLACK));
    }
    if let Some(u) = rpr.child("u") {
        r.underline = Some(!matches!(u.attr("val"), Some("none" | "0")));
    }
    if let Some(h) = rpr.val("highlight") {
        r.highlight = Some(highlight(h));
    } else if let Some(s) = rpr.child("shd").and_then(|s| s.attr("fill")) {
        r.highlight = Some(color_of(s));
    }
    if let Some(v) = rpr.val("vertAlign") {
        r.shift = Some(match v {
            "superscript" => 1,
            "subscript" => -1,
            _ => 0,
        });
    }
    r
}

fn p_props(ppr: &El) -> PProps {
    let mut p = PProps::default();
    if let Some(j) = ppr.val("jc") {
        p.align = Some(match j {
            "center" => Align::Center,
            "right" | "end" => Align::Right,
            "both" | "distribute" => Align::Justify,
            _ => Align::Left,
        });
    }
    if let Some(ind) = ppr.child("ind") {
        p.left = ind.num("left").or(ind.num("start")).map(twips);
        p.right = ind.num("right").or(ind.num("end")).map(twips);
        if let Some(h) = ind.num("hanging") {
            p.first = Some(-twips(h));
        } else if let Some(f) = ind.num("firstLine") {
            p.first = Some(twips(f));
        }
    }
    if let Some(sp) = ppr.child("spacing") {
        p.before = sp.num("before").map(twips);
        p.after = sp.num("after").map(twips);
        if sp.attr("beforeAutospacing").is_some_and(|v| v == "1" || v == "true") {
            p.before = Some(14.0);
        }
        if sp.attr("afterAutospacing").is_some_and(|v| v == "1" || v == "true") {
            p.after = Some(14.0);
        }
        if let Some(l) = sp.num("line") {
            p.line = Some(match sp.attr("lineRule") {
                Some("exact") => LineRule::Exact(twips(l).max(1.0)),
                Some("atLeast") => LineRule::AtLeast(twips(l)),
                _ => LineRule::Multiple(l / 240.0),
            });
        }
    }
    if let Some(tabs) = ppr.child("tabs") {
        let mut v: Vec<(f32, Option<Tab>)> = tabs
            .children("tab")
            .filter(|t| t.attr("val") != Some("bar"))
            .filter_map(|t| {
                let pos = twips(t.num("pos")?);
                if t.attr("val") == Some("clear") {
                    return Some((pos, None));
                }
                let align = match t.attr("val") {
                    Some("right" | "end" | "decimal") => TabAlign::Right,
                    Some("center") => TabAlign::Center,
                    _ => TabAlign::Left,
                };
                let leader = match t.attr("leader") {
                    Some("dot") => Some('.'),
                    Some("hyphen") => Some('-'),
                    Some("underscore" | "heavy") => Some('_'),
                    Some("middleDot") => Some('·'),
                    _ => None,
                };
                Some((pos, Some(Tab { pos, align, leader })))
            })
            .take(64)
            .collect();
        v.sort_by(|a, b| a.0.total_cmp(&b.0));
        p.tabs = Some(v);
    }
    p.page_break_before = ppr.flag("pageBreakBefore");
    p.contextual = ppr.flag("contextualSpacing");
    if let Some(s) = ppr.child("shd") {
        p.shading = Some(s.attr("fill").and_then(color_of));
    }
    if let Some(n) = ppr.child("numPr") {
        let id = n.val("numId").unwrap_or("0").to_string();
        let lvl = n.val("ilvl").and_then(|v| v.parse().ok()).unwrap_or(0usize).min(8);
        p.num = Some((id, lvl));
    }
    p
}

fn border(e: Option<&El>) -> Option<(f32, Rgba)> {
    let e = e?;
    match e.attr("val") {
        None | Some("nil" | "none") => None,
        _ => {
            let w = e.num("sz").map_or(0.5, |s| s / 8.0).clamp(0.25, 12.0);
            Some((w, e.attr("color").and_then(color_of).unwrap_or(Rgba::BLACK)))
        }
    }
}

impl<'a> Ctx<'a> {
    fn new(pkg: &'a Package, main: &str) -> Ctx<'a> {
        let mut c = Ctx {
            pkg,
            styles: HashMap::new(),
            default_para: None,
            default_table: None,
            defaults_r: RProps::default(),
            defaults_p: PProps::default(),
            theme: ["Calibri".into(), "Calibri Light".into(), String::new(), String::new()],
            nums: HashMap::new(),
            counters: HashMap::new(),
            rels: HashMap::new(),
            paras: 0,
            depth: 0,
        };
        let rels = pkg.rels(main);
        if let Some(t) = rels.values().find(|r| r.kind == "theme")
            && let Ok(theme) = pkg.xml(&t.target, &[])
            && let Some(fs) = theme.find("fontScheme")
        {
            for (i, (scheme, script)) in [("minorFont", "latin"), ("majorFont", "latin"), ("minorFont", "ea"), ("majorFont", "ea")].iter().enumerate() {
                if let Some(face) = fs.child(scheme).and_then(|s| s.child(script)).and_then(|l| l.attr("typeface")).filter(|f| !f.is_empty()) {
                    c.theme[i] = face.to_string();
                }
            }
        }
        if let Some(s) = rels.values().find(|r| r.kind == "styles")
            && let Ok(st) = pkg.xml(&s.target, &[])
        {
            c.read_styles(&st);
        }
        if let Some(n) = rels.values().find(|r| r.kind == "numbering")
            && let Ok(num) = pkg.xml(&n.target, &[" "])
        {
            c.read_numbering(&num);
        }
        c
    }

    fn read_styles(&mut self, st: &El) {
        if let Some(d) = st.child("docDefaults") {
            if let Some(r) = d.path(&["rPrDefault", "rPr"]) {
                self.defaults_r = r_props(r, &self.theme);
            }
            if let Some(p) = d.path(&["pPrDefault", "pPr"]) {
                self.defaults_p = p_props(p);
            }
        }
        for s in st.children("style").take(20_000) {
            let Some(id) = s.attr("styleId") else { continue };
            let kind = s.attr("type").unwrap_or("paragraph");
            let default = matches!(s.attr("default"), Some("1" | "true"));
            if default && kind == "paragraph" {
                self.default_para = Some(id.to_string());
            }
            if default && kind == "table" {
                self.default_table = Some(id.to_string());
            }
            let table_borders = s.path(&["tblPr", "tblBorders"]).map(|b| {
                ["top", "left", "bottom", "right", "insideH", "insideV"].map(|k| border(b.child(k).or_else(|| b.child(if k == "left" { "start" } else if k == "right" { "end" } else { k }))))
            });
            self.styles.insert(
                id.to_string(),
                StyleDef {
                    based_on: s.val("basedOn").map(str::to_string),
                    r: s.child("rPr").map(|r| r_props(r, &self.theme)).unwrap_or_default(),
                    p: s.child("pPr").map(p_props).unwrap_or_default(),
                    table_borders,
                },
            );
        }
    }

    fn read_numbering(&mut self, num: &El) {
        let mut abstracts: HashMap<String, Vec<Level>> = HashMap::new();
        for a in num.children("abstractNum").take(5_000) {
            let Some(id) = a.attr("abstractNumId") else { continue };
            let mut levels: Vec<Level> = (0..9).map(|_| Level { format: "decimal".into(), text: String::new(), start: 1, p: PProps::default(), r: RProps::default() }).collect();
            for l in a.children("lvl") {
                let i = l.attr("ilvl").and_then(|v| v.parse::<usize>().ok()).unwrap_or(0);
                let Some(lv) = levels.get_mut(i) else { continue };
                lv.format = l.val("numFmt").unwrap_or("decimal").to_string();
                lv.text = l.val("lvlText").unwrap_or("").to_string();
                lv.start = l.val("start").and_then(|v| v.parse().ok()).unwrap_or(1);
                lv.p = l.child("pPr").map(p_props).unwrap_or_default();
                lv.r = l.child("rPr").map(|r| r_props(r, &self.theme)).unwrap_or_default();
            }
            abstracts.insert(id.to_string(), levels);
        }
        for n in num.children("num").take(20_000) {
            let (Some(id), Some(abs)) = (n.attr("numId"), n.val("abstractNumId")) else { continue };
            let Some(levels) = abstracts.get(abs) else { continue };
            let mut levels: Vec<Level> = levels.iter().map(|l| Level { format: l.format.clone(), text: l.text.clone(), start: l.start, p: l.p.clone(), r: l.r.clone() }).collect();
            for o in n.children("lvlOverride") {
                let i = o.attr("ilvl").and_then(|v| v.parse::<usize>().ok()).unwrap_or(0);
                if let (Some(lv), Some(s)) = (levels.get_mut(i), o.val("startOverride").and_then(|v| v.parse().ok())) {
                    lv.start = s;
                }
            }
            self.nums.insert(id.to_string(), Arc::new(levels));
        }
    }

    /// The properties of style `id` with those it is based on.
    fn style_props(&self, id: &str) -> (RProps, PProps) {
        let mut chain = Vec::new();
        let mut cur = Some(id.to_string());
        while let Some(c) = cur {
            if chain.len() > 20 || chain.contains(&c) {
                break;
            }
            cur = self.styles.get(&c).and_then(|s| s.based_on.clone());
            chain.push(c);
        }
        let (mut r, mut p) = (RProps::default(), PProps::default());
        for c in chain.iter().rev() {
            if let Some(s) = self.styles.get(c) {
                r.over(&s.r);
                p.over(&s.p);
            }
        }
        (r, p)
    }

    fn table_borders(&self, id: Option<&str>) -> [Option<(f32, Rgba)>; 6] {
        let mut cur = id.map(str::to_string).or_else(|| self.default_table.clone());
        let mut n = 0;
        while let Some(c) = cur {
            let Some(s) = self.styles.get(&c) else { break };
            if let Some(b) = s.table_borders {
                return b;
            }
            cur = s.based_on.clone();
            n += 1;
            if n > 20 {
                break;
            }
        }
        [None; 6]
    }

    /// The list label of a paragraph numbered `num`, advancing its count.
    fn label(&mut self, num: &(String, usize)) -> Option<(String, PProps, RProps)> {
        let levels = self.nums.get(&num.0)?.clone();
        let lv = levels.get(num.1)?;
        let counts = self.counters.entry(num.0.clone()).or_insert_with(|| vec![i32::MIN; 9]);
        for (i, c) in counts.iter_mut().enumerate() {
            if i > num.1 {
                *c = i32::MIN;
            }
        }
        counts[num.1] = if counts[num.1] == i32::MIN { lv.start } else { counts[num.1].saturating_add(1) };
        // Levels above that never counted show their start.
        for (i, c) in counts.iter_mut().enumerate().take(num.1) {
            if *c == i32::MIN {
                *c = levels.get(i).map_or(1, |l| l.start);
            }
        }
        let counts = counts.clone();
        let text = if lv.format == "bullet" {
            bullet(&lv.text)
        } else if lv.format == "none" {
            String::new()
        } else {
            let mut out = String::new();
            let mut chars = lv.text.chars().peekable();
            while let Some(c) = chars.next() {
                if c == '%'
                    && let Some(d) = chars.peek().and_then(|d| d.to_digit(10))
                {
                    chars.next();
                    let i = (d as usize).saturating_sub(1).min(8);
                    let fmt = levels.get(i).map_or("decimal", |l| l.format.as_str());
                    out.push_str(&number(counts[i].max(0), fmt));
                } else {
                    out.push(c);
                }
            }
            out
        };
        Some((text, lv.p.clone(), lv.r.clone()))
    }
}

/// A bullet's character as text fonts have it (Symbol and Wingdings
/// bullets are private-use characters).
fn bullet(text: &str) -> String {
    match text.chars().next() {
        Some('\u{F0B7}' | '\u{F06C}' | '\u{F09F}') => "•".into(),
        Some('\u{F0A7}' | '\u{F0FC}') => "▪".into(),
        Some('\u{F0D8}' | '\u{F0E8}') => "➢".into(),
        Some('o') => "◦".into(),
        Some(c) if ('\u{F000}'..='\u{F0FF}').contains(&c) => "•".into(),
        Some(_) => text.to_string(),
        None => "•".into(),
    }
}

/// `n` written in a list format.
pub(crate) fn number(n: i32, fmt: &str) -> String {
    match fmt {
        "lowerLetter" | "upperLetter" => {
            if n <= 0 {
                return n.to_string();
            }
            let n = n as u32 - 1;
            let c = (b'a' + (n % 26) as u8) as char;
            let s = c.to_string().repeat((n / 26) as usize + 1);
            if fmt == "upperLetter" { s.to_uppercase() } else { s }
        }
        "lowerRoman" | "upperRoman" => {
            let s = roman(n);
            if fmt == "upperRoman" { s.to_uppercase() } else { s }
        }
        "decimalZero" => format!("{n:02}"),
        _ => n.to_string(),
    }
}

fn roman(mut n: i32) -> String {
    if !(1..4000).contains(&n) {
        return n.to_string();
    }
    let mut out = String::new();
    for (v, s) in [(1000, "m"), (900, "cm"), (500, "d"), (400, "cd"), (100, "c"), (90, "xc"), (50, "l"), (40, "xl"), (10, "x"), (9, "ix"), (5, "v"), (4, "iv"), (1, "i")] {
        while n >= v {
            out.push_str(s);
            n -= v;
        }
    }
    out
}

/// The style a run is drawn in.
fn style_of(r: &RProps) -> Style {
    Style {
        family: Arc::from(r.font.as_deref().unwrap_or("Calibri")),
        size: r.size.unwrap_or(10.0),
        bold: r.bold.unwrap_or(false),
        italic: r.italic.unwrap_or(false),
        color: r.color.unwrap_or(Rgba::BLACK),
        underline: r.underline.unwrap_or(false),
        strike: r.strike.unwrap_or(false),
        highlight: r.highlight.flatten(),
        shift: r.shift.unwrap_or(0),
        caps: r.caps.unwrap_or(false),
    }
}

// --- the body -----------------------------------------------------------

impl Ctx<'_> {
    fn blocks(&mut self, parent: &El, out: &mut Vec<Block>, sections: &mut Vec<Section>) {
        if self.depth > 24 {
            return;
        }
        self.depth += 1;
        for e in parent.elements() {
            if self.paras > MAX_PARAS {
                break;
            }
            match e.name.as_str() {
                "p" => {
                    self.paras += 1;
                    let mut p = self.para(e);
                    if let Some(sp) = e.path(&["pPr", "sectPr"]) {
                        sections.push(self.section(sp));
                        p.section_end = Some(sections.len() - 1);
                    }
                    out.push(Block::Para(p));
                }
                "tbl" => out.push(Block::Table(self.table(e, sections))),
                // Content controls and inserted text: their content.
                "sdt" => {
                    if let Some(c) = e.child("sdtContent") {
                        self.blocks(c, out, sections);
                    }
                }
                "ins" | "customXml" | "smartTag" => self.blocks(e, out, sections),
                _ => {}
            }
        }
        self.depth -= 1;
    }

    fn para(&mut self, e: &El) -> Para {
        let ppr = e.child("pPr");
        let own = ppr.map(p_props).unwrap_or_default();
        let style = ppr.and_then(|p| p.val("pStyle")).map(str::to_string).or_else(|| self.default_para.clone()).unwrap_or_default();
        let (sr, sp) = self.style_props(&style);
        let mut p = self.defaults_p.clone();
        p.over(&sp);
        let num = own.num.clone().or(sp.num.clone());
        let label = num.as_ref().filter(|n| n.0 != "0").and_then(|n| self.label(n));
        if let Some((_, lp, _)) = &label {
            p.over(lp);
        }
        p.over(&own);
        let mut base_r = self.defaults_r.clone();
        base_r.over(&sr);
        // The paragraph mark's own properties don't change its text.
        let mut spans = Vec::new();
        if let Some((text, _, lr)) = label
            && !text.is_empty()
        {
            let mut r = base_r.clone();
            r.over(&lr);
            spans.push(Span::new(format!("{text}\t"), Arc::new(style_of(&r))));
        }
        let mut floats = Vec::new();
        let mut field = FieldState::default();
        let label_spans = spans.len();
        let mut brk = None;
        self.runs(e, &base_r, &mut spans, &mut floats, &mut field, &mut brk, 0);
        // A page break before the text puts the paragraph on a new page;
        // after some, what follows the paragraph starts one.
        let break_before = brk.is_some_and(|k| k <= label_spans);
        let break_after = brk.is_some() && !break_before;
        let fmt = ParaFormat {
            align: p.align.unwrap_or_default(),
            left: p.left.unwrap_or(0.0),
            right: p.right.unwrap_or(0.0),
            first: p.first.unwrap_or(0.0),
            line: p.line.unwrap_or_default(),
            tabs: p.tabs.iter().flatten().filter_map(|t| t.1).collect(),
        };
        // With a label, a hanging indent's first tab goes to the indent.
        let mut fmt = fmt;
        if fmt.first < 0.0 && !fmt.tabs.iter().any(|t| t.pos == fmt.left) {
            fmt.tabs.push(Tab { pos: fmt.left, align: TabAlign::Left, leader: None });
            crate::typeset::sort_tabs(&mut fmt.tabs);
        }
        Para {
            fmt,
            spans,
            before: p.before.unwrap_or(0.0),
            after: p.after.unwrap_or(0.0),
            page_break_before: p.page_break_before.unwrap_or(false) || break_before,
            break_after,
            shading: p.shading.flatten(),
            contextual: p.contextual.unwrap_or(false),
            style,
            floats,
            section_end: None,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn runs(&mut self, parent: &El, base: &RProps, spans: &mut Vec<Span>, floats: &mut Vec<Float>, field: &mut FieldState, brk: &mut Option<usize>, depth: usize) {
        if depth > 16 {
            return;
        }
        for e in parent.elements() {
            match e.name.as_str() {
                "r" => self.run(e, base, spans, floats, field, brk),
                "hyperlink" | "ins" | "smartTag" | "customXml" | "fldSimple" | "bdo" | "dir" => {
                    if e.name == "fldSimple" {
                        let instr = e.attr("instr").unwrap_or("").trim().to_ascii_uppercase();
                        if let Some(c) = page_field(&instr) {
                            let st = self.run_style(None, base);
                            spans.push(Span::new(c.to_string(), st));
                            continue;
                        }
                    }
                    self.runs(e, base, spans, floats, field, brk, depth + 1)
                }
                "sdt" => {
                    if let Some(c) = e.child("sdtContent") {
                        self.runs(c, base, spans, floats, field, brk, depth + 1);
                    }
                }
                "oMath" | "oMathPara" => {
                    let st = self.run_style(None, base);
                    let t = e.text();
                    if !t.is_empty() {
                        spans.push(Span::new(t, st));
                    }
                }
                _ => {}
            }
        }
    }

    fn run_style(&self, rpr: Option<&El>, base: &RProps) -> Arc<Style> {
        let mut r = base.clone();
        if let Some(rpr) = rpr {
            if let Some(cs) = rpr.val("rStyle") {
                let (cr, _) = self.style_props(cs);
                r.over(&cr);
            }
            r.over(&r_props(rpr, &self.theme));
        }
        Arc::new(style_of(&r))
    }

    fn run(&mut self, r: &El, base: &RProps, spans: &mut Vec<Span>, floats: &mut Vec<Float>, field: &mut FieldState, brk: &mut Option<usize>) {
        let rpr = r.child("rPr");
        let hidden = rpr.and_then(|p| p.flag("vanish")).unwrap_or(false);
        let st = self.run_style(rpr, base);
        let mut text = String::new();
        let flush = |text: &mut String, spans: &mut Vec<Span>| {
            if !text.is_empty() {
                spans.push(Span::new(std::mem::take(text), st.clone()));
            }
        };
        for k in r.elements() {
            match k.name.as_str() {
                "fldChar" => match k.attr("fldCharType") {
                    Some("begin") => {
                        field.depth += 1;
                        field.instr.clear();
                        field.showing = false;
                    }
                    Some("separate") => {
                        field.showing = true;
                        if let Some(c) = page_field(&field.instr.trim().to_ascii_uppercase()) {
                            text.push(c);
                            field.skip_result = true;
                        }
                    }
                    Some("end") => {
                        field.depth = field.depth.saturating_sub(1);
                        field.showing = false;
                        field.skip_result = false;
                    }
                    _ => {}
                },
                "instrText" => field.instr.push_str(&k.text()),
                _ if field.depth > 0 && (!field.showing || field.skip_result) => {}
                _ if hidden => {}
                "t" => text.push_str(&k.text()),
                "tab" | "ptab" => text.push('\t'),
                "br" | "cr" => match k.attr("type") {
                    Some("page") => {
                        flush(&mut text, spans);
                        brk.get_or_insert(spans.len());
                    }
                    Some("column") => text.push('\n'),
                    _ => text.push('\n'),
                },
                "noBreakHyphen" => text.push('\u{2011}'),
                "softHyphen" => {}
                "sym" => {
                    let c = k.attr("char").and_then(|c| u32::from_str_radix(c, 16).ok()).and_then(char::from_u32).unwrap_or('•');
                    text.push(if ('\u{F000}'..='\u{F0FF}').contains(&c) { '•' } else { c });
                }
                "drawing" => {
                    flush(&mut text, spans);
                    self.drawing(k, &st, spans, floats);
                }
                "pict" | "object" => {
                    flush(&mut text, spans);
                    self.vml(k, &st, spans);
                }
                "AlternateContent" => {
                    // The Office 2010 form when it is a drawing, else the
                    // fallback.
                    let pick = k.child("Choice").filter(|c| c.find("drawing").is_some()).or_else(|| k.child("Fallback"));
                    if let Some(c) = pick {
                        flush(&mut text, spans);
                        for d in c.elements() {
                            match d.name.as_str() {
                                "drawing" => self.drawing(d, &st, spans, floats),
                                "pict" => self.vml(d, &st, spans),
                                _ => {}
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        // A page break as the run's last thing breaks after the text;
        // inside a run it breaks there (the rest goes to the next page).
        flush(&mut text, spans);
    }

    fn picture_key(&mut self, rid: &str) -> Option<Arc<str>> {
        let rel = self.rels.get(rid)?;
        if rel.external {
            return None;
        }
        Some(Arc::from(rel.target.as_str()))
    }

    fn drawing(&mut self, d: &El, st: &Arc<Style>, spans: &mut Vec<Span>, floats: &mut Vec<Float>) {
        for shape in d.elements() {
            let ext = shape.child("extent");
            let (w, h) = (ext.and_then(|e| e.num("cx")).map_or(0.0, emu), ext.and_then(|e| e.num("cy")).map_or(0.0, emu));
            if !(w > 0.0 && h > 0.0 && w < 20_000.0 && h < 20_000.0) {
                continue;
            }
            let blip = shape.find("blip").and_then(|b| b.attr("embed")).map(str::to_string);
            let key = blip.and_then(|id| self.picture_key(&id));
            // Text boxes: their text as paragraphs after the picture.
            let txbx = shape.find("txbxContent");
            match shape.name.as_str() {
                "inline" => {
                    if let Some(key) = key {
                        spans.push(Span { text: String::new(), style: st.clone(), object: Some(Object { w, h, key }) });
                    } else if let Some(t) = txbx {
                        let text = t.text();
                        if !text.trim().is_empty() {
                            spans.push(Span::new(text, st.clone()));
                        }
                    }
                }
                "anchor" => {
                    let pos = |name: &str| -> (f32, From) {
                        let Some(p) = shape.child(name) else { return (0.0, From::Para) };
                        let from = match p.attr("relativeFrom") {
                            Some("page") => From::Page,
                            Some("margin" | "column" | "insideMargin" | "leftMargin" | "rightMargin" | "topMargin" | "bottomMargin") => From::Margin,
                            _ => From::Para,
                        };
                        let off = p.child("posOffset").and_then(|o| o.text().trim().parse::<f32>().ok()).map_or(0.0, emu);
                        let off = match p.child("align").map(|a| a.text()) {
                            Some(a) if a == "center" => f32::NAN,
                            Some(a) if a == "right" || a == "bottom" => f32::INFINITY,
                            _ => off,
                        };
                        (off, from)
                    };
                    let (x, x_from) = pos("positionH");
                    let (y, y_from) = pos("positionV");
                    let behind = shape.attr("behindDoc").is_some_and(|v| v == "1" || v == "true");
                    if let Some(key) = key {
                        floats.push(Float { w, h, x, y, x_from, y_from, key, behind });
                    } else if let Some(t) = txbx {
                        let text = t.text();
                        if !text.trim().is_empty() {
                            spans.push(Span::new(format!("{text}\n"), st.clone()));
                        }
                    }
                }
                _ => {}
            }
        }
    }

    /// An old VML picture (`w:pict`, `w:object`): its image, sized by its
    /// style.
    fn vml(&mut self, p: &El, st: &Arc<Style>, spans: &mut Vec<Span>) {
        let Some(img) = p.find("imagedata") else {
            if let Some(t) = p.find("txbxContent") {
                let text = t.text();
                if !text.trim().is_empty() {
                    spans.push(Span::new(text, st.clone()));
                }
            }
            return;
        };
        let Some(id) = img.attr("id").map(str::to_string) else { return };
        let style = p.find("shape").and_then(|s| s.attr("style")).unwrap_or("");
        let dim = |k: &str| -> Option<f32> {
            let v = style.split(';').find_map(|kv| kv.split_once(':').filter(|(a, _)| a.trim() == k).map(|(_, b)| b.trim().to_string()))?;
            let (num, unit) = v.split_at(v.find(|c: char| c.is_ascii_alphabetic()).unwrap_or(v.len()));
            let n: f32 = num.parse().ok()?;
            Some(match unit {
                "in" => n * 72.0,
                "cm" => n * 72.0 / 2.54,
                "mm" => n * 72.0 / 25.4,
                "px" => n * 0.75,
                _ => n,
            })
        };
        let (Some(w), Some(h)) = (dim("width"), dim("height")) else { return };
        if !(w > 0.0 && h > 0.0 && w < 20_000.0 && h < 20_000.0) {
            return;
        }
        if let Some(key) = self.picture_key(&id) {
            spans.push(Span { text: String::new(), style: st.clone(), object: Some(Object { w, h, key }) });
        }
    }

    fn table(&mut self, t: &El, sections: &mut Vec<Section>) -> Table {
        let tpr = t.child("tblPr");
        let style = tpr.and_then(|p| p.val("tblStyle"));
        let sb = self.table_borders(style);
        let own = tpr.and_then(|p| p.child("tblBorders"));
        let tb: [Option<(f32, Rgba)>; 6] = match own {
            Some(b) => ["top", "left", "bottom", "right", "insideH", "insideV"].map(|k| {
                let alt = match k {
                    "left" => "start",
                    "right" => "end",
                    k => k,
                };
                match b.child(k).or_else(|| b.child(alt)) {
                    Some(e) => border(Some(e)),
                    None => None,
                }
            }),
            None => sb,
        };
        let tb = if own.is_some() {
            let mut m = sb;
            for (i, v) in tb.iter().enumerate() {
                let k = ["top", "left", "bottom", "right", "insideH", "insideV"][i];
                if own.and_then(|b| b.child(k)).is_some() {
                    m[i] = *v;
                }
            }
            m
        } else {
            tb
        };
        let cell_mar = tpr.and_then(|p| p.child("tblCellMar"));
        let mar = |k: &str, d: f32| cell_mar.and_then(|m| m.child(k)).and_then(|e| e.num("w")).map_or(d, twips);
        let default_mar = [mar("top", 0.0), mar("left", mar("start", 5.4)), mar("bottom", 0.0), mar("right", mar("end", 5.4))];
        let mut cols: Vec<f32> = t.path(&["tblGrid"]).map(|g| g.children("gridCol").filter_map(|c| c.num("w")).map(twips).collect()).unwrap_or_default();
        let indent = tpr.and_then(|p| p.child("tblInd")).and_then(|i| i.num("w")).map_or(0.0, twips);
        let align = match tpr.and_then(|p| p.val("jc")) {
            Some("center") => Align::Center,
            Some("right" | "end") => Align::Right,
            _ => Align::Left,
        };
        let mut rows = Vec::new();
        for tr in t.children("tr").take(20_000) {
            let trpr = tr.child("trPr");
            let hr = trpr.and_then(|p| p.child("trHeight"));
            let mut cells = Vec::new();
            for tc in tr.elements().filter(|e| e.name == "tc" || e.name == "sdt").take(512) {
                let tc = if tc.name == "sdt" { match tc.path(&["sdtContent", "tc"]) { Some(c) => c, None => continue } } else { tc };
                let tcpr = tc.child("tcPr");
                let span = tcpr.and_then(|p| p.val("gridSpan")).and_then(|v| v.parse::<usize>().ok()).unwrap_or(1).clamp(1, 64);
                let merged = tcpr.and_then(|p| p.child("vMerge")).is_some_and(|m| m.attr("val") != Some("restart"));
                let fill = tcpr.and_then(|p| p.child("shd")).and_then(|s| s.attr("fill")).and_then(color_of);
                let cb = tcpr.and_then(|p| p.child("tcBorders"));
                let own = |k: &str, alt: &str| cb.and_then(|b| b.child(k).or_else(|| b.child(alt)));
                let pick = |k: &str, alt: &str, d: Option<(f32, Rgba)>| match own(k, alt) {
                    Some(e) => border(Some(e)),
                    None => d,
                };
                let borders = [pick("top", "top", tb[0].or(tb[4])), pick("left", "start", tb[1].or(tb[5])), pick("bottom", "bottom", tb[2].or(tb[4])), pick("right", "end", tb[3].or(tb[5]))];
                let cm = tcpr.and_then(|p| p.child("tcMar"));
                let m = |k: &str, i: usize| cm.and_then(|m| m.child(k)).and_then(|e| e.num("w")).map_or(default_mar[i], twips);
                let margins = [m("top", 0), m("left", 1), m("bottom", 2), m("right", 3)];
                let valign = match tcpr.and_then(|p| p.val("vAlign")) {
                    Some("center") => 1,
                    Some("bottom") => 2,
                    _ => 0,
                };
                let mut blocks = Vec::new();
                self.blocks(tc, &mut blocks, sections);
                cells.push(Cell { span, merged, blocks, fill, borders, margins, valign });
            }
            rows.push(Row {
                cells,
                height: hr.and_then(|h| h.num("val")).map_or(0.0, twips),
                exact: hr.and_then(|h| h.attr("hRule")) == Some("exact"),
                header: trpr.and_then(|p| p.flag("tblHeader")).unwrap_or(false),
            });
        }
        // A grid missing or too short: columns from the cells.
        let need = rows.iter().map(|r| r.cells.iter().map(|c| c.span).sum::<usize>()).max().unwrap_or(0);
        if cols.len() < need {
            let w = tpr.and_then(|p| p.child("tblW")).filter(|w| w.attr("type") == Some("dxa")).and_then(|w| w.num("w")).map_or(468.0, twips);
            let have: f32 = cols.iter().sum();
            let each = ((w - have) / (need - cols.len()) as f32).max(18.0);
            cols.resize(need, each);
        }
        for c in &mut cols {
            *c = c.clamp(0.0, 2000.0);
        }
        Table { cols, rows, indent, align }
    }

    fn section(&mut self, s: &El) -> Section {
        let pg = s.child("pgSz");
        let mar = s.child("pgMar");
        let m = |k: &str, d: f32| mar.and_then(|m| m.num(k)).map_or(d, twips);
        let width = pg.and_then(|p| p.num("w")).map_or(612.0, twips).clamp(72.0, 4000.0);
        let height = pg.and_then(|p| p.num("h")).map_or(792.0, twips).clamp(72.0, 4000.0);
        let mut margins = [m("top", 72.0).abs(), m("right", 72.0), m("bottom", 72.0).abs(), m("left", 72.0)];
        // Margins that leave no room are shrunk.
        if margins[1] + margins[3] > width - 36.0 {
            margins[1] = 18.0;
            margins[3] = 18.0;
        }
        if margins[0] + margins[2] > height - 36.0 {
            margins[0] = 18.0;
            margins[2] = 18.0;
        }
        let mut hf = |kind: &str, which: &str| -> Option<Arc<Vec<Block>>> {
            let r = s.children(kind).find(|r| r.attr("type").unwrap_or("default") == which)?;
            let rel = self.rels.get(r.attr("id")?)?.target.clone();
            let root = self.pkg.xml(&rel, &["t"]).ok()?;
            let saved = std::mem::replace(&mut self.rels, self.pkg.rels(&rel));
            let mut blocks = Vec::new();
            let mut ignored = Vec::new();
            self.blocks(&root, &mut blocks, &mut ignored);
            self.rels = saved;
            Some(Arc::new(blocks))
        };
        let header = hf("headerReference", "default");
        let footer = hf("footerReference", "default");
        let first_header = hf("headerReference", "first");
        let first_footer = hf("footerReference", "first");
        Section {
            width,
            height,
            margins,
            header_dist: m("header", 36.0),
            footer_dist: m("footer", 36.0),
            header,
            footer,
            first_header,
            first_footer,
            page_start: s.child("pgNumType").and_then(|n| n.num("start")).map(|v| v.clamp(0.0, 1e6) as usize),
            title_page: s.flag("titlePg").unwrap_or(false),
            new_page: !matches!(s.val("type"), Some("continuous")),
        }
    }
}

#[derive(Default)]
struct FieldState {
    depth: usize,
    instr: String,
    showing: bool,
    /// The field is a page number, drawn by the layout: its result left out.
    skip_result: bool,
}

fn page_field(instr: &str) -> Option<char> {
    let name = instr.split_whitespace().next()?;
    match name {
        "PAGE" => Some(PAGE),
        "NUMPAGES" => Some(PAGES),
        "SECTIONPAGES" => Some(SECTION_PAGES),
        _ => None,
    }
}

/// Reads the document of a .docx package. Pictures are keyed by their
/// package part.
pub(crate) fn read(pkg: &Package) -> Result<DocModel, Error> {
    let main = pkg.rel_of("", "officeDocument").filter(|m| pkg.has(m)).unwrap_or_else(|| "word/document.xml".to_string());
    let root = pkg.xml(&main, &["t", "instrText", "delText"])?;
    let mut cx = Ctx::new(pkg, &main);
    cx.rels = pkg.rels(&main);
    let body = root.child("body").ok_or_else(|| crate::ooxml::bad("the document has no body"))?;
    let mut blocks = Vec::new();
    let mut sections = Vec::new();
    cx.blocks(body, &mut blocks, &mut sections);
    let last = match body.child("sectPr") {
        Some(s) => cx.section(s),
        None => cx.section(&El::default()),
    };
    sections.push(last);
    for i in 1..sections.len() {
        let (before, rest) = sections.split_at_mut(i);
        let (prev, s) = (&before[i - 1], &mut rest[0]);
        s.header = s.header.take().or_else(|| prev.header.clone());
        s.footer = s.footer.take().or_else(|| prev.footer.clone());
        s.first_header = s.first_header.take().or_else(|| prev.first_header.clone());
        s.first_footer = s.first_footer.take().or_else(|| prev.first_footer.clone());
    }
    let background = root.child("background").and_then(|b| b.attr("color")).and_then(color_of);
    Ok(DocModel { body: blocks, sections, background })
}
