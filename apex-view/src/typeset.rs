//! Text set into lines: the system's fonts found by family, characters
//! mapped to glyphs (with a fallback face for those a font lacks), lines
//! broken to a width and aligned.

use std::collections::HashMap;
use std::sync::Arc;

use skrifa::instance::{LocationRef, Size};
use skrifa::outline::{DrawSettings, OutlinePen};
use skrifa::raw::TableProvider;
use skrifa::{FontRef, GlyphId, MetadataProvider};
use vello_cpu::kurbo::BezPath;

use crate::draw::{GlyphRun, Item, Rgba};
use crate::fonts::{FaceData, Fonts};
use crate::text::TextRun;

/// Glyph outlines kept per face.
const OUTLINE_CACHE: usize = 4096;

struct Face {
    data: Arc<[u8]>,
    index: u32,
    upem: f32,
    /// Ascent, descent (positive) and line gap, in em.
    ascent: f32,
    descent: f32,
    gap: f32,
    bold: bool,
    italic: bool,
    glyphs: HashMap<char, u32>,
    advances: HashMap<u32, f32>,
    outlines: HashMap<u32, Option<Arc<BezPath>>>,
}

impl Face {
    fn font(&self) -> Option<FontRef<'_>> {
        FontRef::from_index(&self.data, self.index).ok()
    }
}

/// The faces a document's text uses, loaded once.
pub(crate) struct FontCache {
    fonts: Fonts,
    faces: Vec<Face>,
    by_style: HashMap<(String, bool, bool), Option<usize>>,
    by_data: HashMap<(usize, u32), usize>,
    /// Fallback faces for characters, by script group.
    by_char: HashMap<(u8, bool), Option<usize>>,
}

impl FontCache {
    pub fn new(fonts: Fonts) -> FontCache {
        FontCache { fonts, faces: Vec::new(), by_style: HashMap::new(), by_data: HashMap::new(), by_char: HashMap::new() }
    }

    fn add(&mut self, (data, index): FaceData) -> Option<usize> {
        let key = (data.as_ptr() as usize, index);
        if let Some(&i) = self.by_data.get(&key) {
            return Some(i);
        }
        let font = FontRef::from_index(&data, index).ok()?;
        let upem = font.head().ok()?.units_per_em().max(16) as f32;
        let m = font.metrics(Size::unscaled(), LocationRef::default());
        let (ascent, descent, gap) = (m.ascent / upem, -m.descent / upem, m.leading / upem);
        let (bold, italic) = font.os2().map_or((false, false), |o| (o.us_weight_class() >= 600, o.fs_selection().bits() & 1 != 0));
        let ok = |v: f32, d: f32| if v.is_finite() && (0.0..4.0).contains(&v) { v } else { d };
        self.faces.push(Face {
            data: data.clone(),
            index,
            upem,
            ascent: ok(ascent, 0.8),
            descent: ok(descent, 0.2),
            gap: ok(gap, 0.0),
            bold,
            italic,
            glyphs: HashMap::new(),
            advances: HashMap::new(),
            outlines: HashMap::new(),
        });
        let i = self.faces.len() - 1;
        self.by_data.insert(key, i);
        Some(i)
    }

    /// The face for text in `family`.
    pub fn resolve(&mut self, family: &str, bold: bool, italic: bool) -> Option<usize> {
        let key = (family.to_lowercase(), bold, italic);
        if let Some(f) = self.by_style.get(&key) {
            return *f;
        }
        let f = self.fonts.face(family, bold, italic).and_then(|d| self.add(d));
        self.by_style.insert(key, f);
        f
    }

    pub fn upem(&self, face: usize) -> f32 {
        self.faces.get(face).map_or(1000.0, |f| f.upem)
    }

    /// Ascent, descent and line gap of `face` in em.
    pub fn metrics(&self, face: usize) -> (f32, f32, f32) {
        self.faces.get(face).map_or((0.8, 0.2, 0.0), |f| (f.ascent, f.descent, f.gap))
    }

    /// Whether `face` is itself bold / italic.
    pub fn style_of(&self, face: usize) -> (bool, bool) {
        self.faces.get(face).map_or((false, false), |f| (f.bold, f.italic))
    }

    pub fn glyph_in(&mut self, face: usize, ch: char) -> Option<u32> {
        let f = self.faces.get_mut(face)?;
        if let Some(&g) = f.glyphs.get(&ch) {
            return (g != 0).then_some(g);
        }
        let g = FontRef::from_index(&f.data, f.index).ok().and_then(|font| font.charmap().map(ch)).map_or(0, |g| g.to_u32());
        f.glyphs.insert(ch, g);
        (g != 0).then_some(g)
    }

    /// The face and glyph `ch` is drawn with: `face`'s own, else a
    /// fallback face's that has it.
    pub fn glyph(&mut self, face: usize, ch: char, bold: bool) -> Option<(usize, u32)> {
        if let Some(g) = self.glyph_in(face, ch) {
            return Some((face, g));
        }
        let group = script_group(ch);
        let key = (group, bold);
        let fb = match self.by_char.get(&key) {
            Some(f) => *f,
            None => {
                let f = self.fonts.for_char(ch, bold).and_then(|d| self.add(d));
                self.by_char.insert(key, f);
                f
            }
        };
        let fb = fb?;
        self.glyph_in(fb, ch).map(|g| (fb, g))
    }

    /// The advance of glyph `gid` in em.
    pub fn advance(&mut self, face: usize, gid: u32) -> f32 {
        let Some(f) = self.faces.get_mut(face) else { return 0.5 };
        if let Some(&a) = f.advances.get(&gid) {
            return a;
        }
        let a = FontRef::from_index(&f.data, f.index)
            .ok()
            .and_then(|font| font.glyph_metrics(Size::unscaled(), LocationRef::default()).advance_width(GlyphId::new(gid)))
            .map_or(0.5, |a| a / f.upem);
        let a = if a.is_finite() && (0.0..8.0).contains(&a) { a } else { 0.5 };
        f.advances.insert(gid, a);
        a
    }

    /// The outline of glyph `gid` in font units, y up.
    pub fn outline(&mut self, face: usize, gid: u32) -> Option<Arc<BezPath>> {
        let f = self.faces.get_mut(face)?;
        if let Some(p) = f.outlines.get(&gid) {
            return p.clone();
        }
        let path = (|| {
            let font = f.font()?;
            if !crate::font_check::glyph_bounded(&font, GlyphId::new(gid)) {
                return None;
            }
            let glyph = font.outline_glyphs().get(GlyphId::new(gid))?;
            let mut pen = Pen::default();
            glyph.draw(DrawSettings::unhinted(Size::unscaled(), LocationRef::default()), &mut pen).ok()?;
            Some(Arc::new(pen.path))
        })();
        if f.outlines.len() >= OUTLINE_CACHE {
            f.outlines.clear();
        }
        f.outlines.insert(gid, path.clone());
        path
    }
}

/// Characters grouped by the fallback font they likely need.
fn script_group(ch: char) -> u8 {
    match ch as u32 {
        0x1100..=0x11FF | 0x2E80..=0x9FFF | 0xAC00..=0xD7AF | 0xF900..=0xFAFF | 0xFF00..=0xFFEF | 0x20000..=0x2FFFF => 1,
        _ => 0,
    }
}

#[derive(Default)]
struct Pen {
    path: BezPath,
}

impl OutlinePen for Pen {
    fn move_to(&mut self, x: f32, y: f32) {
        self.path.move_to((x as f64, y as f64));
    }
    fn line_to(&mut self, x: f32, y: f32) {
        self.path.line_to((x as f64, y as f64));
    }
    fn quad_to(&mut self, cx0: f32, cy0: f32, x: f32, y: f32) {
        self.path.quad_to((cx0 as f64, cy0 as f64), (x as f64, y as f64));
    }
    fn curve_to(&mut self, cx0: f32, cy0: f32, cx1: f32, cy1: f32, x: f32, y: f32) {
        self.path.curve_to((cx0 as f64, cy0 as f64), (cx1 as f64, cy1 as f64), (x as f64, y as f64));
    }
    fn close(&mut self) {
        self.path.close_path();
    }
}

// --- styled text set into lines ----------------------------------------

/// How a span of text looks.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Style {
    pub family: Arc<str>,
    /// Points.
    pub size: f32,
    pub bold: bool,
    pub italic: bool,
    pub color: Rgba,
    pub underline: bool,
    pub strike: bool,
    pub highlight: Option<Rgba>,
    /// Superscript (> 0) or subscript (< 0).
    pub shift: i8,
    pub caps: bool,
}

impl Default for Style {
    fn default() -> Style {
        Style { family: Arc::from("Calibri"), size: 11.0, bold: false, italic: false, color: Rgba::BLACK, underline: false, strike: false, highlight: None, shift: 0, caps: false }
    }
}

/// Text in one style. Tabs are '\t'; '\n' ends a line.
#[derive(Clone, Debug)]
pub(crate) struct Span {
    pub text: String,
    pub style: Arc<Style>,
    /// A picture set in the line as one character (the text is left out).
    pub object: Option<Object>,
}

/// A picture in a line: its size in points, sitting on the baseline.
#[derive(Clone, Debug)]
pub(crate) struct Object {
    pub w: f32,
    pub h: f32,
    pub key: Arc<str>,
}

impl Span {
    pub fn new(text: impl Into<String>, style: Arc<Style>) -> Span {
        Span { text: text.into(), style, object: None }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub(crate) enum Align {
    #[default]
    Left,
    Center,
    Right,
    Justify,
}

/// How a paragraph is set.
#[derive(Clone, Debug, Default)]
pub(crate) struct ParaFormat {
    pub align: Align,
    /// Indents in points: left, right, and the first line's (from left;
    /// negative hangs).
    pub left: f32,
    pub right: f32,
    pub first: f32,
    /// Line height: a multiple of the font's (`Some(m)`) or exact /
    /// at-least points.
    pub line: LineRule,
    /// Tab stops from the left edge, in order (past them, every 36 pt).
    pub tabs: Vec<Tab>,
}

/// A tab stop: where, how the text after it lines up, and the character
/// filling the space before it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Tab {
    pub pos: f32,
    pub align: TabAlign,
    pub leader: Option<char>,
}

/// Tab stops put in order (one sort shared by every format).
pub(crate) fn sort_tabs(tabs: &mut [Tab]) {
    tabs.sort_by(|a, b| a.pos.total_cmp(&b.pos));
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TabAlign {
    Left,
    Center,
    Right,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum LineRule {
    Multiple(f32),
    Exact(f32),
    AtLeast(f32),
}

impl Default for LineRule {
    fn default() -> LineRule {
        LineRule::Multiple(1.0)
    }
}

/// One glyph set: where it goes on its line and what it draws.
#[derive(Clone, Copy)]
struct Placed {
    ch: char,
    face: usize,
    gid: u32,
    /// From the line's left edge, and its advance.
    x: f32,
    adv: f32,
    span: usize,
    /// Synthetic bold/italic needed.
    embolden: bool,
    skew: bool,
    /// A tab's leader: its glyph and advance.
    lead: Option<(u32, f32)>,
}

/// A line set: glyphs from its left edge (indent and alignment applied).
pub(crate) struct Line {
    glyphs: Vec<Placed>,
    pub width: f32,
    /// Above and below the baseline, and the line's whole height.
    pub ascent: f32,
    pub descent: f32,
    pub height: f32,
}

/// Lines of a paragraph, set in `width` points.
pub(crate) struct Lines {
    pub lines: Vec<Line>,
    spans: Vec<Arc<Style>>,
    objects: Vec<Option<Object>>,
}

impl Lines {
    pub fn height(&self) -> f32 {
        self.lines.iter().map(|l| l.height).sum()
    }

    /// Items to draw line `i` with its top at `(x, y)`, and its text.
    pub fn draw_line(&self, i: usize, x: f32, y: f32, items: &mut Vec<Item>, text: &mut Vec<TextRun>) {
        let Some(line) = self.lines.get(i) else { return };
        // Baseline: the extra height of a tall line goes above the text.
        let base = y + line.height - line.descent;
        draw_glyphs(&line.glyphs, &self.spans, x, base, line, items, text);
        for g in &line.glyphs {
            if let Some(Some(o)) = self.objects.get(g.span)
                && g.ch == '\u{FFFC}'
            {
                items.push(Item::Image { x: x + g.x, y: base - o.h, w: o.w, h: o.h, key: o.key.clone() });
            }
        }
    }
}

fn draw_glyphs(glyphs: &[Placed], spans: &[Arc<Style>], x: f32, base: f32, line: &Line, items: &mut Vec<Item>, text: &mut Vec<TextRun>) {
    let mut i = 0;
    while i < glyphs.len() {
        let g0 = glyphs[i];
        let mut j = i + 1;
        while j < glyphs.len() && glyphs[j].span == g0.span && glyphs[j].face == g0.face {
            j += 1;
        }
        let st = &spans[g0.span];
        let size = if st.shift != 0 { st.size * 0.65 } else { st.size };
        let rise = match st.shift.signum() {
            1 => -st.size * 0.33,
            -1 => st.size * 0.12,
            _ => 0.0,
        };
        let seg = &glyphs[i..j];
        let (x0, x1) = (seg[0].x, seg.last().map_or(seg[0].x, |g| g.x + g.adv));
        if let Some(h) = st.highlight {
            items.push(Item::Rect { x: x + x0, y: base - line.ascent, w: x1 - x0, h: line.ascent + line.descent, color: h });
        }
        let run: Vec<(u32, f32)> = seg.iter().filter(|g| g.gid != 0 && !g.ch.is_whitespace() && g.ch != '\u{FFFC}').map(|g| (g.gid, g.x - x0)).collect();
        if !run.is_empty() {
            items.push(Item::Glyphs(GlyphRun { face: g0.face, size, color: st.color, x: x + x0, y: base + rise, glyphs: run, skew: g0.skew, embolden: g0.embolden }));
        }
        let lw = (st.size / 18.0).max(0.5);
        if st.underline {
            items.push(Item::Line { x0: x + x0, y0: base + st.size * 0.12, x1: x + x1, y1: base + st.size * 0.12, width: lw, color: st.color });
        }
        if st.strike {
            items.push(Item::Line { x0: x + x0, y0: base - st.size * 0.3, x1: x + x1, y1: base - st.size * 0.3, width: lw, color: st.color });
        }
        for g in seg {
            if let Some((gid, adv)) = g.lead.filter(|l| l.1 > 0.1) {
                // Leader glyphs on a grid, so the dots of lines line up.
                let (from, to) = (x + g.x, x + g.x + g.adv - adv * 0.5);
                let mut lx = (from / adv).ceil() * adv;
                let mut dots = Vec::new();
                while lx + adv <= to + 0.01 && dots.len() < 2000 {
                    dots.push((gid, lx - from));
                    lx += adv;
                }
                if !dots.is_empty() {
                    items.push(Item::Glyphs(GlyphRun { face: g.face, size: st.size, color: st.color, x: from, y: base + rise, glyphs: dots, skew: false, embolden: false }));
                }
            }
        }
        let s: String = seg.iter().filter(|g| g.ch != '\u{FFFC}').map(|g| if g.ch == '\t' { ' ' } else { g.ch }).collect();
        if !s.trim().is_empty() {
            text.push(TextRun { text: s, x: x + x0, y: base - st.size * 0.8, width: x1 - x0, height: st.size });
        }
        i = j;
    }
}

/// Whether a line may break after `a` and before `b`.
fn break_after(a: char, b: char) -> bool {
    let cjk = |c: char| matches!(c as u32, 0x2E80..=0x9FFF | 0xAC00..=0xD7AF | 0xF900..=0xFAFF | 0xFF00..=0xFFEF | 0x20000..=0x2FFFF);
    a == ' ' || a == '\t' || a == '\u{200B}' || (a == '-' && b.is_alphanumeric()) || (cjk(a) && !matches!(b, '、' | '。' | '，' | '．' | '）' | '」' | '』' | '！' | '？' | '：' | '；')) || (cjk(b) && !matches!(a, '（' | '「' | '『'))
}

/// Sets `spans` into lines `width` points wide.
pub(crate) fn set(spans: &[Span], fmt: &ParaFormat, width: f32, fonts: &mut FontCache) -> Lines {
    let styles: Vec<Arc<Style>> = spans.iter().map(|s| s.style.clone()).collect();
    let objects: Vec<Option<Object>> = spans.iter().map(|s| s.object.clone()).collect();
    // Every character with its glyph and advance.
    let mut chars: Vec<Placed> = Vec::new();
    for (si, sp) in spans.iter().enumerate() {
        let st = &sp.style;
        if let Some(o) = &sp.object {
            chars.push(Placed { ch: '\u{FFFC}', face: 0, gid: 0, x: 0.0, adv: o.w.max(0.0), span: si, embolden: false, skew: false, lead: None });
            continue;
        }
        let size = if st.shift != 0 { st.size * 0.65 } else { st.size };
        let face = fonts.resolve(&st.family, st.bold, st.italic);
        let (fb, fi) = face.map_or((false, false), |f| fonts.style_of(f));
        for ch in sp.text.chars() {
            let ch = if st.caps { ch.to_uppercase().next().unwrap_or(ch) } else { ch };
            let (face, gid, adv) = match ch {
                '\n' | '\t' => (face.unwrap_or(0), 0, 0.0),
                _ => match face.and_then(|f| fonts.glyph(f, if ch == '\u{A0}' { ' ' } else { ch }, st.bold)) {
                    Some((f, g)) => (f, g, fonts.advance(f, g) * size),
                    None => (face.unwrap_or(0), 0, if ch == ' ' { size * 0.25 } else { size * 0.5 }),
                },
            };
            chars.push(Placed { ch, face, gid, x: 0.0, adv, span: si, embolden: st.bold && !fb, skew: st.italic && !fi, lead: None });
            if chars.len() >= 1_000_000 {
                break;
            }
        }
    }
    let line_metrics = |glyphs: &[Placed], fonts: &FontCache, fallback: &Style| {
        let (mut a, mut d, mut g) = (0f32, 0f32, 0f32);
        let mut any = false;
        for p in glyphs {
            let st = &styles[p.span];
            if p.ch == '\u{FFFC}' {
                if let Some(Some(o)) = objects.get(p.span) {
                    a = a.max(o.h);
                }
                any = true;
                continue;
            }
            let (fa, fd, fg) = fonts.metrics(p.face);
            a = a.max(fa * st.size);
            d = d.max(fd * st.size);
            g = g.max(fg * st.size);
            any = true;
        }
        if !any {
            let (fa, fd, fg) = fonts.resolve_metrics(fallback);
            (fa * fallback.size, fd * fallback.size, fg * fallback.size)
        } else {
            (a, d, g)
        }
    };
    let empty_style = styles.last().cloned().unwrap_or_default();
    let mut lines = Vec::new();
    let mut start = 0;
    let n = chars.len();
    let mut first = true;
    loop {
        let indent = fmt.left + if first { fmt.first } else { 0.0 };
        let avail = (width - indent - fmt.right).max(1.0);
        // Fill the line.
        let mut x = 0.0f32;
        // Widened by a tab whose stop is past the right indent.
        let mut room = avail;
        let mut end = start;
        let mut last_break = None;
        let mut forced = false;
        while end < n {
            let c = chars[end];
            if c.ch == '\n' {
                forced = true;
                break;
            }
            let adv = if c.ch == '\t' {
                // The text up to the next tab, for stops it lines up against.
                let after: f32 = chars[end + 1..].iter().take_while(|c| c.ch != '\t' && c.ch != '\n').take(500).map(|c| c.adv).sum();
                let (w, tab) = tab_width(x + indent, after, &fmt.tabs);
                if let Some(t) = tab {
                    room = room.max((t.pos - indent).min(avail + fmt.right));
                }
                chars[end].lead = tab.and_then(|t| t.leader).and_then(|l| {
                    let f = chars[end].face;
                    let size = styles[c.span].size;
                    fonts.glyph_in(f, l).map(|g| (g, fonts.advance(f, g) * size))
                });
                w
            } else {
                c.adv
            };
            if x + adv > room && end > start && c.ch != ' ' {
                break;
            }
            chars[end].x = x;
            chars[end].adv = adv;
            x += adv;
            end += 1;
            if end < n && break_after(c.ch, chars[end].ch) {
                last_break = Some(end);
            }
        }
        // Back to the last break when the line was cut inside a word.
        let cut = end < n && !forced;
        if cut && let Some(b) = last_break.filter(|&b| b > start) {
            end = b;
        }
        let glyphs: Vec<Placed> = chars[start..end].to_vec();
        let used = glyphs.iter().rev().find(|g| !g.ch.is_whitespace()).map_or(0.0, |g| g.x + g.adv);
        let (a, d, g) = line_metrics(&glyphs, fonts, &empty_style);
        let natural = a + d + g;
        let height = match fmt.line {
            LineRule::Multiple(m) => natural * m.clamp(0.25, 10.0),
            LineRule::Exact(h) => h,
            LineRule::AtLeast(h) => natural.max(h),
        };
        let mut line = Line { glyphs, width: used, ascent: a, descent: d, height: height.max(0.5) };
        let last = !cut;
        align(&mut line, fmt.align, avail, indent, last);
        lines.push(line);
        start = if forced { end + 1 } else { end };
        // Spaces where a line broke are not carried to the next one.
        while cut && start < n && chars[start].ch == ' ' {
            start += 1;
        }
        first = false;
        if start >= n || lines.len() >= 100_000 {
            // A paragraph ended by a line break keeps an empty last line.
            if forced && start >= n {
                let (a, d, g) = line_metrics(&[], fonts, &empty_style);
                lines.push(Line { glyphs: Vec::new(), width: 0.0, ascent: a, descent: d, height: a + d + g });
            }
            break;
        }
    }
    if lines.is_empty() {
        let (a, d, g) = line_metrics(&[], fonts, &empty_style);
        let natural = a + d + g;
        let height = match fmt.line {
            LineRule::Multiple(m) => natural * m.clamp(0.25, 10.0),
            LineRule::Exact(h) => h,
            LineRule::AtLeast(h) => natural.max(h),
        };
        lines.push(Line { glyphs: Vec::new(), width: 0.0, ascent: a, descent: d, height });
    }
    Lines { lines, spans: styles, objects }
}

impl FontCache {
    /// Metrics of the face for `style` (for empty lines).
    fn resolve_metrics(&self, style: &Style) -> (f32, f32, f32) {
        let key = (style.family.to_lowercase(), style.bold, style.italic);
        match self.by_style.get(&key).copied().flatten() {
            Some(f) => self.metrics(f),
            None => self.faces.first().map_or((0.9, 0.25, 0.0), |f| (f.ascent, f.descent, f.gap)),
        }
    }
}

/// The width a tab at `x` takes to its stop, with `after` the width of
/// the text that follows it; and the stop.
fn tab_width(x: f32, after: f32, tabs: &[Tab]) -> (f32, Option<Tab>) {
    match tabs.iter().find(|t| t.pos > x + 0.5) {
        Some(t) => {
            let w = match t.align {
                TabAlign::Left => t.pos - x,
                TabAlign::Center => t.pos - x - after / 2.0,
                TabAlign::Right => t.pos - x - after,
            };
            (w.max(1.0), Some(*t))
        }
        None => ((((x / 36.0).floor() + 1.0) * 36.0 - x).max(1.0), None),
    }
}

/// Places the glyphs of a line for its alignment, the indent included.
fn align(line: &mut Line, align: Align, avail: f32, indent: f32, last: bool) {
    let extra = (avail - line.width).max(0.0);
    let shift = match align {
        Align::Center => extra / 2.0,
        Align::Right => extra,
        _ => 0.0,
    };
    if align == Align::Justify && !last {
        let spaces = line.glyphs.iter().rev().skip_while(|g| g.ch == ' ').filter(|g| g.ch == ' ').count();
        if spaces > 0 && extra < avail * 0.5 {
            let per = extra / spaces as f32;
            let mut add = 0.0;
            let trailing = line.glyphs.iter().rev().take_while(|g| g.ch == ' ').count();
            let k = line.glyphs.len() - trailing;
            for g in &mut line.glyphs[..k] {
                g.x += add;
                if g.ch == ' ' {
                    add += per;
                }
            }
            line.width = avail;
        }
    }
    for g in &mut line.glyphs {
        g.x += indent + shift;
    }
}
