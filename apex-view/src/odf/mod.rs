//! OpenDocument packages: their styles (with what each inherits),
//! lengths, colours and fonts, shared by text documents, spreadsheets and
//! presentations.

#[cfg(feature = "odp")]
pub(crate) mod odp;
#[cfg(feature = "ods")]
pub(crate) mod ods;
#[cfg(feature = "odt")]
pub(crate) mod odt;

use std::collections::HashMap;
use std::sync::Arc;

use crate::draw::Rgba;
use crate::ooxml::{El, Package};
use crate::typeset::{Align, LineRule, ParaFormat, Span, Style, Tab, TabAlign};
use crate::Error;

/// Elements whose whitespace is text.
pub(crate) const TEXT: &[&str] = &["p", "h", "span", "a", "meta", "ruby-base", "date", "time", "page-number", "page-count", "author-name", "title", "subject", "file-name", "sequence", "variable-get", "user-field-get", "chapter", "sender-firstname", "text-input"];

/// Properties of a style merged down its chain of parents: attributes by
/// local name, and child elements (tab stops, images...) by name.
#[derive(Clone, Default, Debug)]
pub(crate) struct Props {
    pub attrs: HashMap<String, String>,
    pub kids: HashMap<String, El>,
}

impl Props {
    pub fn get(&self, k: &str) -> Option<&str> {
        self.attrs.get(k).map(String::as_str)
    }

    pub fn len(&self, k: &str) -> Option<f32> {
        self.get(k).and_then(length)
    }

    /// Lays `p`'s properties over these. A font size in percent is taken
    /// of the size so far.
    fn over(&mut self, p: &El) {
        for (k, v) in &p.attrs {
            if (k == "font-size" || k == "font-size-asian" || k == "font-size-complex")
                && let Some(pc) = v.trim().strip_suffix('%').and_then(|n| n.trim().parse::<f32>().ok())
            {
                let base = self.len(k).unwrap_or(12.0);
                self.attrs.insert(k.clone(), format!("{}pt", base * pc / 100.0));
                continue;
            }
            self.attrs.insert(k.clone(), v.clone());
        }
        for e in p.elements() {
            self.kids.insert(e.name.clone(), e.clone());
        }
    }
}

/// The styles of a document: named and automatic ones by family and
/// name, defaults by family, font faces, list styles and page masters.
#[derive(Default)]
pub(crate) struct Styles {
    named: HashMap<(String, String), El>,
    defaults: HashMap<String, El>,
    pub fonts: HashMap<String, String>,
    pub lists: HashMap<String, El>,
    pub layouts: HashMap<String, El>,
    pub masters: Vec<El>,
    pub outline: Option<El>,
    /// Gradients, fill images, hatches and table templates by kind and
    /// name.
    pub drawn: HashMap<(String, String), El>,
    cache: HashMap<(String, String, String), Arc<Props>>,
}

impl Styles {
    /// Styles from `styles.xml` (common, automatic and master) and those
    /// of `content`, which come after.
    pub fn read(styles: Option<&El>, content: &El) -> Styles {
        let mut s = Styles::default();
        for root in styles.into_iter().chain(Some(content)) {
            for group in root.elements() {
                match group.name.as_str() {
                    "font-face-decls" => {
                        for f in group.children("font-face").take(4096) {
                            if let (Some(n), Some(fam)) = (f.attr("name"), f.attr("font-family")) {
                                s.fonts.insert(n.to_string(), fam.trim_matches(['\'', '"']).to_string());
                            }
                        }
                    }
                    "styles" | "automatic-styles" => {
                        for e in group.elements().take(100_000) {
                            match e.name.as_str() {
                                "style" => {
                                    if let Some(n) = e.attr("name") {
                                        s.named.insert((e.attr("family").unwrap_or("").to_string(), n.to_string()), e.clone());
                                    }
                                }
                                "default-style" => {
                                    s.defaults.insert(e.attr("family").unwrap_or("").to_string(), e.clone());
                                }
                                "list-style" => {
                                    if let Some(n) = e.attr("name") {
                                        s.lists.insert(n.to_string(), e.clone());
                                    }
                                }
                                "page-layout" => {
                                    if let Some(n) = e.attr("name") {
                                        s.layouts.insert(n.to_string(), e.clone());
                                    }
                                }
                                "outline-style" => s.outline = Some(e.clone()),
                                "gradient" | "fill-image" | "hatch" | "table-template" => {
                                    if let Some(n) = e.attr("name") {
                                        s.drawn.insert((e.name.clone(), n.to_string()), e.clone());
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                    "master-styles" => s.masters.extend(group.children("master-page").take(1000).cloned()),
                    _ => {}
                }
            }
        }
        s
    }

    pub fn style(&self, family: &str, name: &str) -> Option<&El> {
        self.named.get(&(family.to_string(), name.to_string()))
    }

    pub fn master(&self, name: &str) -> Option<&El> {
        self.masters.iter().find(|m| m.attr("name") == Some(name))
    }

    /// The `kind` properties (`text-properties`, `paragraph-properties`...)
    /// of style `name` of `family`, with what it inherits.
    pub fn props(&mut self, family: &str, name: Option<&str>, kind: &str) -> Arc<Props> {
        let key = (family.to_string(), name.unwrap_or("").to_string(), kind.to_string());
        if let Some(p) = self.cache.get(&key) {
            return p.clone();
        }
        let mut chain = Vec::new();
        let mut cur = name.map(str::to_string);
        while let Some(n) = cur.take() {
            if chain.len() > 16 {
                break;
            }
            let Some(st) = self.style(family, &n) else { break };
            chain.push(st);
            cur = st.attr("parent-style-name").map(str::to_string);
        }
        let mut p = Props::default();
        if let Some(d) = self.defaults.get(family).and_then(|d| d.child(kind)) {
            p.over(d);
        }
        for st in chain.iter().rev() {
            if let Some(k) = st.child(kind) {
                p.over(k);
            }
        }
        let p = Arc::new(p);
        if self.cache.len() < 50_000 {
            self.cache.insert(key, p.clone());
        }
        p
    }

    /// An attribute of style `name` itself or one it inherits from
    /// (`master-page-name`, `list-style-name`...).
    pub fn attr(&self, family: &str, name: &str, attr: &str) -> Option<String> {
        let mut cur = Some(name.to_string());
        for _ in 0..16 {
            let st = self.style(family, cur.as_deref()?)?;
            if let Some(v) = st.attr(attr) {
                return Some(v.to_string());
            }
            cur = st.attr("parent-style-name").map(str::to_string);
        }
        None
    }

    /// A text style from text properties over `base`.
    pub fn text_style(&self, p: &Props, base: &Style) -> Style {
        let mut s = base.clone();
        if let Some(f) = p.get("font-name").and_then(|n| self.fonts.get(n)).map(String::as_str).or_else(|| p.get("font-family").map(|f| f.trim_matches(['\'', '"']))) {
            s.family = Arc::from(f);
        }
        if let Some(v) = p.get("font-size") {
            if let Some(pc) = v.trim().strip_suffix('%').and_then(|n| n.trim().parse::<f32>().ok()) {
                s.size = (base.size * pc / 100.0).clamp(1.0, 1000.0);
            } else if let Some(l) = length(v) {
                s.size = l.clamp(1.0, 1000.0);
            }
        }
        if let Some(w) = p.get("font-weight") {
            s.bold = matches!(w, "bold" | "600" | "700" | "800" | "900");
        }
        if let Some(i) = p.get("font-style") {
            s.italic = matches!(i, "italic" | "oblique");
        }
        if let Some(c) = p.get("color").and_then(color) {
            s.color = c;
        }
        if p.get("use-window-font-color") == Some("true") {
            s.color = Rgba::BLACK;
        }
        if let Some(u) = p.get("text-underline-style") {
            s.underline = u != "none";
        }
        if let Some(u) = p.get("text-line-through-style") {
            s.strike = u != "none";
        }
        if let Some(b) = p.get("background-color") {
            s.highlight = color(b);
        }
        if let Some(t) = p.get("text-position") {
            let first = t.split_whitespace().next().unwrap_or("0");
            s.shift = match first {
                "super" => 1,
                "sub" => -1,
                v => match v.trim_end_matches('%').parse::<f32>() {
                    Ok(n) if n > 0.0 => 1,
                    Ok(n) if n < 0.0 => -1,
                    _ => 0,
                },
            };
        }
        if let Some(t) = p.get("text-transform") {
            s.caps = t == "uppercase";
        }
        if p.get("font-variant") == Some("small-caps") {
            s.caps = true;
        }
        if p.get("display") == Some("none") {
            s.size = 0.01;
        }
        s
    }
}

/// A length ("2.5cm", "10pt", "0.5in"...) in points.
pub(crate) fn length(v: &str) -> Option<f32> {
    let v = v.trim();
    let split = v.find(|c: char| c.is_ascii_alphabetic() || c == '%').unwrap_or(v.len());
    let (n, unit) = v.split_at(split);
    let n: f32 = n.trim().parse().ok()?;
    let k = match unit {
        "pt" | "" => 1.0,
        "cm" => 72.0 / 2.54,
        "mm" => 72.0 / 25.4,
        "in" | "inch" => 72.0,
        "pc" => 12.0,
        "px" => 0.75,
        "em" => 12.0,
        _ => return None,
    };
    let r = n * k;
    r.is_finite().then_some(r.clamp(-20_000.0, 20_000.0))
}

/// A percentage ("80%") as a fraction.
pub(crate) fn percent(v: &str) -> Option<f32> {
    v.trim().strip_suffix('%')?.trim().parse::<f32>().ok().filter(|p| p.is_finite()).map(|p| p / 100.0)
}

/// A colour ("#RRGGBB"); none for "transparent".
pub(crate) fn color(v: &str) -> Option<Rgba> {
    let h = v.trim().strip_prefix('#')?;
    if h.len() == 3 {
        let n = u32::from_str_radix(h, 16).ok()?;
        let d = |c: u32| ((c & 15) * 17) as u8;
        return Some(Rgba([d(n >> 8), d(n >> 4), d(n), 255]));
    }
    (h.len() == 6).then(|| u32::from_str_radix(h, 16).ok().map(Rgba::rgb)).flatten()
}

/// A border ("0.5pt solid #000000"): width and colour; none for "none".
pub(crate) fn border(v: &str) -> Option<(f32, Rgba)> {
    let v = v.trim();
    if v.is_empty() || v == "none" || v.contains("hidden") {
        return None;
    }
    let mut w = 0.75;
    let mut c = Rgba::BLACK;
    let mut double = false;
    for part in v.split_whitespace() {
        if let Some(col) = color(part) {
            c = col;
        } else if let Some(l) = length(part) {
            w = l;
        } else if part == "double" {
            double = true;
        } else if part == "thin" {
            w = 0.5;
        } else if part == "thick" {
            w = 2.0;
        }
    }
    if double {
        w = w.max(1.5);
    }
    (w > 0.0).then_some((w.clamp(0.1, 20.0), c))
}

/// The parts of a package read: `content.xml`, and `styles.xml` when
/// there is one, with whitespace kept inside text.
pub(crate) fn read_parts(pkg: &Package) -> Result<(El, Option<El>), Error> {
    // A package saved with a password: its parts are encrypted.
    if let Ok(m) = pkg.read("META-INF/manifest.xml")
        && memchr::memmem::find(&m, b"encryption-data").is_some()
    {
        return Err(Error::Encrypted);
    }
    let content = pkg.xml("content.xml", TEXT)?;
    let styles = pkg.xml("styles.xml", TEXT).ok();
    Ok((content, styles))
}

/// Text inside paragraph-like elements with its whitespace collapsed as
/// OpenDocument says: runs of spaces, tabs and newlines become one space.
pub(crate) fn collapse(s: &str, after_space: &mut bool) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if matches!(c, ' ' | '\t' | '\n' | '\r') {
            if !*after_space {
                out.push(' ');
                *after_space = true;
            }
        } else {
            out.push(c);
            *after_space = false;
        }
    }
    out
}

/// A list number in an OpenDocument number format ("1", "a", "I"...).
pub(crate) fn number(n: i32, fmt: &str) -> String {
    let n = n.max(0);
    match fmt {
        "a" | "A" => {
            if n == 0 {
                return "0".into();
            }
            let i = (n - 1) as u32;
            let c = (b'a' + (i % 26) as u8) as char;
            let s = c.to_string().repeat((i / 26) as usize + 1);
            if fmt == "A" { s.to_uppercase() } else { s }
        }
        "i" | "I" => {
            let mut out = String::new();
            let mut k = n;
            if !(1..4000).contains(&k) {
                return n.to_string();
            }
            for (v, s) in [(1000, "m"), (900, "cm"), (500, "d"), (400, "cd"), (100, "c"), (90, "xc"), (50, "l"), (40, "xl"), (10, "x"), (9, "ix"), (5, "v"), (4, "iv"), (1, "i")] {
                while k >= v {
                    out.push_str(s);
                    k -= v;
                }
            }
            if fmt == "I" { out.to_uppercase() } else { out }
        }
        "" => String::new(),
        _ => n.to_string(),
    }
}

pub(crate) fn push(spans: &mut Vec<Span>, text: String, style: &Arc<Style>) {
    if let Some(last) = spans.last_mut()
        && last.object.is_none()
        && Arc::ptr_eq(&last.style, style)
    {
        last.text.push_str(&text);
        return;
    }
    spans.push(Span::new(text, style.clone()));
}

/// The style of `level` (from 0) of a list style.
pub(crate) fn level_style(list: &El, level: usize) -> Option<&El> {
    let want = (level + 1).to_string();
    list.elements().find(|l| l.attr("level") == Some(want.as_str()))
}

/// Where a list level puts its text (left indent) and its label (the
/// first line's indent, negative).
pub(crate) fn list_indent(lvl: &El) -> (f32, f32) {
    let Some(p) = lvl.child("list-level-properties") else { return (0.0, 0.0) };
    if p.attr("list-level-position-and-space-mode") == Some("label-alignment")
        && let Some(a) = p.child("list-level-label-alignment")
    {
        let left = a.attr("margin-left").and_then(length).unwrap_or(0.0);
        let first = a.attr("text-indent").and_then(length).unwrap_or(0.0);
        return (left, first);
    }
    let before = p.attr("space-before").and_then(length).unwrap_or(0.0);
    let min = p.attr("min-label-width").and_then(length).unwrap_or(0.0);
    (before + min, -min)
}

/// A bullet as text fonts have it.
pub(crate) fn bullet(c: char) -> char {
    match c as u32 {
        0xE000..=0xF8FF => '•',
        _ => c,
    }
}

pub(crate) fn para_format(pp: &Props, style: &Style) -> ParaFormat {
    let align = match pp.get("text-align") {
        Some("center") => Align::Center,
        Some("end" | "right") => Align::Right,
        Some("justify") => Align::Justify,
        _ => Align::Left,
    };
    let line = if let Some(v) = pp.get("line-height") {
        match percent(v) {
            Some(p) => LineRule::Multiple(p.clamp(0.1, 10.0)),
            None => length(v).map_or(LineRule::Multiple(1.0), |l| LineRule::Exact(l.max(1.0))),
        }
    } else if let Some(l) = pp.len("line-height-at-least") {
        LineRule::AtLeast(l)
    } else if let Some(l) = pp.len("line-spacing") {
        LineRule::AtLeast((style.size * 1.17 + l).max(1.0))
    } else {
        LineRule::Multiple(1.0)
    };
    let left = pp.len("margin-left").unwrap_or(0.0);
    let mut tabs = Vec::new();
    if let Some(ts) = pp.kids.get("tab-stops") {
        for t in ts.children("tab-stop").take(64) {
            let Some(pos) = t.attr("position").and_then(length) else { continue };
            let align = match t.attr("type") {
                Some("right") => TabAlign::Right,
                Some("center") => TabAlign::Center,
                _ => TabAlign::Left,
            };
            let leader = t.attr("leader-text").and_then(|s| s.chars().next()).filter(|c| *c != ' ').or_else(|| match t.attr("leader-style") {
                Some("dotted") => Some('.'),
                Some("dash") => Some('-'),
                Some("solid") => Some('_'),
                _ => None,
            });
            // Stops are from the paragraph's indent.
            tabs.push(Tab { pos: pos + left, align, leader });
        }
        crate::typeset::sort_tabs(&mut tabs);
    }
    ParaFormat { align, left, right: pp.len("margin-right").unwrap_or(0.0), first: pp.len("text-indent").unwrap_or(0.0), line, tabs }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lengths_colours_and_borders() {
        assert_eq!(length("1in"), Some(72.0));
        assert!((length("2.54cm").unwrap() - 72.0).abs() < 0.01);
        assert_eq!(length("12pt"), Some(12.0));
        assert_eq!(color("#ff0000"), Some(Rgba([255, 0, 0, 255])));
        assert_eq!(color("transparent"), None);
        assert_eq!(border("0.5pt solid #00ff00"), Some((0.5, Rgba([0, 255, 0, 255]))));
        assert_eq!(border("none"), None);
        assert_eq!(number(4, "I"), "IV");
        assert_eq!(number(28, "a"), "bb");
        let mut sp = false;
        assert_eq!(collapse("a  \n b", &mut sp), "a b");
    }
}
