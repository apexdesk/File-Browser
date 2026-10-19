//! A slide drawn: its background, the shapes of its master and layout,
//! then its own; placeholders take their place and text styles from the
//! layout and master, colours and fonts from the theme.

use std::collections::HashMap;
use std::sync::Arc;

use vello_cpu::kurbo::{Affine, BezPath, Point, Shape};

use crate::draw::{Item, Page, Rgba};
use crate::geom::preset;
use crate::ooxml::{El, Package, Rel};
use crate::text::TextRun;
use crate::typeset::{self, Align, FontCache, LineRule, ParaFormat, Span, Style, Tab, TabAlign};
use crate::Error;

const EMU: f32 = 12_700.0;
const MAX_SHAPES: usize = 5_000;

/// The theme of a master: its colours by name and its fonts.
struct Theme {
    colors: HashMap<String, u32>,
    major: String,
    minor: String,
    major_ea: String,
    minor_ea: String,
}

fn theme(pkg: &Package, part: Option<&str>) -> Theme {
    let mut t = Theme { colors: HashMap::new(), major: "Calibri Light".into(), minor: "Calibri".into(), major_ea: String::new(), minor_ea: String::new() };
    for (k, v) in [("dk1", 0x000000), ("lt1", 0xFFFFFF), ("dk2", 0x44546A), ("lt2", 0xE7E6E6), ("accent1", 0x4472C4), ("accent2", 0xED7D31), ("accent3", 0xA5A5A5), ("accent4", 0xFFC000), ("accent5", 0x5B9BD5), ("accent6", 0x70AD47), ("hlink", 0x0563C1), ("folHlink", 0x954F72)] {
        t.colors.insert(k.into(), v);
    }
    let Some(root) = part.and_then(|p| pkg.xml(p, &[]).ok()) else { return t };
    if let Some(cs) = root.find("clrScheme") {
        for e in cs.elements() {
            let v = e.child("srgbClr").and_then(|c| c.attr("val")).or_else(|| e.child("sysClr").and_then(|c| c.attr("lastClr")));
            if let Some(v) = v.and_then(|v| u32::from_str_radix(v, 16).ok()) {
                t.colors.insert(e.name.clone(), v & 0xFF_FFFF);
            }
        }
    }
    if let Some(fs) = root.find("fontScheme") {
        let face = |a: &str, b: &str| fs.child(a).and_then(|s| s.child(b)).and_then(|l| l.attr("typeface")).filter(|f| !f.is_empty()).map(str::to_string);
        if let Some(f) = face("majorFont", "latin") {
            t.major = f;
        }
        if let Some(f) = face("minorFont", "latin") {
            t.minor = f;
        }
        t.major_ea = face("majorFont", "ea").unwrap_or_default();
        t.minor_ea = face("minorFont", "ea").unwrap_or_default();
    }
    t
}

/// A part of the slide's chain: its XML and relationships.
struct Part {
    root: El,
    rels: HashMap<String, Rel>,
}

impl Part {
    fn read(pkg: &Package, name: &str) -> Option<Part> {
        Some(Part { root: pkg.xml(name, &[]).ok()?, rels: pkg.rels(name) })
    }

    fn tree(&self) -> Option<&El> {
        self.root.path(&["cSld", "spTree"])
    }

    fn rel(&self, kind: &str) -> Option<&Rel> {
        self.rels.values().find(|r| r.kind == kind && !r.external)
    }
}

/// Where the shapes of a part map to on the slide: EMU to points, through
/// the groups they are in.
#[derive(Clone, Copy)]
struct Xform {
    sx: f32,
    sy: f32,
    dx: f32,
    dy: f32,
}

impl Xform {
    fn pt(&self, x: f32, y: f32) -> (f32, f32) {
        (x * self.sx + self.dx, y * self.sy + self.dy)
    }
}

/// A shape's box in EMU, its rotation (degrees) and flips.
#[derive(Clone, Copy, Default, Debug)]
struct Frame {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    rot: f32,
    flip_h: bool,
    flip_v: bool,
}

fn frame_of(xfrm: Option<&El>) -> Option<Frame> {
    let x = xfrm?;
    let off = x.child("off")?;
    let ext = x.child("ext")?;
    Some(Frame {
        x: off.num("x").unwrap_or(0.0),
        y: off.num("y").unwrap_or(0.0),
        w: ext.num("cx").unwrap_or(0.0).max(0.0),
        h: ext.num("cy").unwrap_or(0.0).max(0.0),
        rot: x.num("rot").unwrap_or(0.0) / 60_000.0,
        flip_h: x.attr("flipH").is_some_and(|v| v == "1" || v == "true"),
        flip_v: x.attr("flipV").is_some_and(|v| v == "1" || v == "true"),
    })
}

/// Placeholders by what they are: their type and index.
#[derive(Clone, Debug)]
struct Ph {
    kind: String,
    idx: Option<String>,
}

fn ph_of(shape: &El) -> Option<Ph> {
    let nv = shape.elements().find(|e| e.name.starts_with("nv"))?;
    let ph = nv.child("nvPr")?.child("ph")?;
    Some(Ph { kind: ph.attr("type").unwrap_or("body").to_string(), idx: ph.attr("idx").map(str::to_string) })
}

fn title_kind(k: &str) -> bool {
    matches!(k, "title" | "ctrTitle")
}

/// The placeholder in `tree` matching `ph`: by index, else by type.
fn find_ph<'a>(tree: Option<&'a El>, ph: &Ph, by_type_only: bool) -> Option<&'a El> {
    let tree = tree?;
    let shapes = || tree.elements().filter(|e| e.name == "sp");
    if !by_type_only
        && let Some(idx) = &ph.idx
        && let Some(s) = shapes().find(|s| ph_of(s).is_some_and(|p| p.idx.as_ref() == Some(idx)))
    {
        return Some(s);
    }
    let same = |k: &str| k == ph.kind || (title_kind(k) && title_kind(&ph.kind)) || (matches!(k, "body" | "obj") && matches!(ph.kind.as_str(), "body" | "obj" | "subTitle"));
    shapes().find(|s| ph_of(s).is_some_and(|p| same(&p.kind)))
}

pub(crate) struct Slide<'a> {
    pkg: &'a Package,
    theme: Theme,
    clr_map: HashMap<String, String>,
    fonts: &'a mut FontCache,
    items: Vec<Item>,
    text: Vec<TextRun>,
    shapes: usize,
}

/// Slide `part` drawn on a page `size` points.
pub(crate) fn page(pkg: &Package, part: &str, size: (f32, f32), fonts: &mut FontCache) -> Result<Page, Error> {
    let slide = Part::read(pkg, part).ok_or_else(|| crate::ooxml::bad("a slide cannot be read"))?;
    let layout = slide.rel("slideLayout").and_then(|r| Part::read(pkg, &r.target));
    let master = layout.as_ref().and_then(|l| l.rel("slideMaster")).and_then(|r| Part::read(pkg, &r.target));
    let theme_part = master.as_ref().and_then(|m| m.rel("theme")).map(|r| r.target.clone());
    let mut clr_map: HashMap<String, String> = HashMap::new();
    for (k, v) in [("bg1", "lt1"), ("tx1", "dk1"), ("bg2", "lt2"), ("tx2", "dk2")] {
        clr_map.insert(k.into(), v.into());
    }
    if let Some(m) = master.as_ref().and_then(|m| m.root.child("clrMap")) {
        for (k, v) in &m.attrs {
            clr_map.insert(k.clone(), v.clone());
        }
    }
    for p in [layout.as_ref(), Some(&slide)].into_iter().flatten() {
        if let Some(o) = p.root.path(&["clrMapOvr", "overrideClrMapping"]) {
            for (k, v) in &o.attrs {
                clr_map.insert(k.clone(), v.clone());
            }
        }
    }
    let mut s = Slide { pkg, theme: theme(pkg, theme_part.as_deref()), clr_map, fonts, items: Vec::new(), text: Vec::new(), shapes: 0 };
    let (w, h) = size;
    let base = Xform { sx: 1.0 / EMU, sy: 1.0 / EMU, dx: 0.0, dy: 0.0 };
    // The background: the slide's, else its layout's, else its master's.
    let mut bg = Some(Rgba::WHITE);
    for p in [Some(&slide), layout.as_ref(), master.as_ref()].into_iter().flatten() {
        if let Some(b) = p.root.path(&["cSld", "bg"]) {
            s.background(b, p, w, h, &mut bg);
            break;
        }
    }
    let chain = Chain { slide: &slide, layout: layout.as_ref(), master: master.as_ref() };
    let show_master = |p: Option<&Part>| p.is_none_or(|p| p.root.attr("showMasterSp") != Some("0"));
    if let Some(m) = &master
        && show_master(Some(&slide))
        && show_master(layout.as_ref())
        && let Some(tree) = m.tree()
    {
        s.tree(tree, m, base, &chain, Level::Master, 0);
    }
    if let Some(l) = &layout
        && show_master(Some(&slide))
        && let Some(tree) = l.tree()
    {
        s.tree(tree, l, base, &chain, Level::Layout, 0);
    }
    if let Some(tree) = slide.tree() {
        s.tree(tree, &slide, base, &chain, Level::Slide, 0);
    }
    let mut items = std::mem::take(&mut s.items);
    if let Some(c) = bg.filter(|c| *c != Rgba::WHITE) {
        items.insert(0, Item::Rect { x: 0.0, y: 0.0, w, h, color: c });
    }
    Ok(Page { width: w, height: h, background: Some(Rgba::WHITE), items, text: std::mem::take(&mut s.text) })
}

struct Chain<'a> {
    slide: &'a Part,
    layout: Option<&'a Part>,
    master: Option<&'a Part>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Level {
    Master,
    Layout,
    Slide,
}

// --- colours ------------------------------------------------------------

fn hsl(rgb: [f32; 3]) -> [f32; 3] {
    let [r, g, b] = rgb;
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let l = (max + min) / 2.0;
    if (max - min).abs() < 1e-6 {
        return [0.0, 0.0, l];
    }
    let d = max - min;
    let s = if l > 0.5 { d / (2.0 - max - min) } else { d / (max + min) };
    let h = if max == r {
        (g - b) / d + if g < b { 6.0 } else { 0.0 }
    } else if max == g {
        (b - r) / d + 2.0
    } else {
        (r - g) / d + 4.0
    };
    [h / 6.0, s, l]
}

fn rgb_of_hsl([h, s, l]: [f32; 3]) -> [f32; 3] {
    if s <= 0.0 {
        return [l, l, l];
    }
    let q = if l < 0.5 { l * (1.0 + s) } else { l + s - l * s };
    let p = 2.0 * l - q;
    let f = |mut t: f32| {
        if t < 0.0 {
            t += 1.0;
        }
        if t > 1.0 {
            t -= 1.0;
        }
        if t < 1.0 / 6.0 {
            p + (q - p) * 6.0 * t
        } else if t < 0.5 {
            q
        } else if t < 2.0 / 3.0 {
            p + (q - p) * (2.0 / 3.0 - t) * 6.0
        } else {
            p
        }
    };
    [f(h + 1.0 / 3.0), f(h), f(h - 1.0 / 3.0)]
}

impl Slide<'_> {
    /// A colour element (`srgbClr`, `schemeClr`...) with its changes;
    /// `ph` stands for `phClr`.
    fn color(&self, e: &El, ph: Option<Rgba>) -> Option<Rgba> {
        let base: Rgba = match e.name.as_str() {
            "srgbClr" => Rgba::rgb(u32::from_str_radix(e.attr("val")?, 16).ok()?),
            "sysClr" => Rgba::rgb(e.attr("lastClr").and_then(|v| u32::from_str_radix(v, 16).ok()).unwrap_or(if e.attr("val") == Some("window") { 0xFFFFFF } else { 0 })),
            "schemeClr" => {
                let name = e.attr("val")?;
                if name == "phClr" {
                    ph?
                } else {
                    let mapped = self.clr_map.get(name).map(String::as_str).unwrap_or(name);
                    Rgba::rgb(*self.theme.colors.get(mapped).or_else(|| self.theme.colors.get(name))?)
                }
            }
            "prstClr" => Rgba::rgb(match e.attr("val")? {
                "white" => 0xFFFFFF,
                "red" => 0xFF0000,
                "green" => 0x008000,
                "blue" => 0x0000FF,
                "yellow" => 0xFFFF00,
                "gray" | "grey" => 0x808080,
                "orange" => 0xFFA500,
                _ => 0x000000,
            }),
            "scrgbClr" => {
                let c = |k: &str| (e.num(k).unwrap_or(0.0) / 100_000.0 * 255.0).clamp(0.0, 255.0) as u8;
                Rgba([c("r"), c("g"), c("b"), 255])
            }
            "hslClr" => {
                let h = e.num("hue").unwrap_or(0.0) / 21_600_000.0;
                let [r, g, b] = rgb_of_hsl([h, e.num("sat").unwrap_or(0.0) / 100_000.0, e.num("lum").unwrap_or(0.0) / 100_000.0]);
                Rgba([(r * 255.0) as u8, (g * 255.0) as u8, (b * 255.0) as u8, 255])
            }
            _ => return None,
        };
        let mut c = [base.0[0] as f32 / 255.0, base.0[1] as f32 / 255.0, base.0[2] as f32 / 255.0];
        let mut alpha = base.0[3] as f32 / 255.0;
        for m in e.elements() {
            let v = m.num("val").unwrap_or(100_000.0) / 100_000.0;
            match m.name.as_str() {
                "lumMod" => {
                    let mut h = hsl(c);
                    h[2] = (h[2] * v).clamp(0.0, 1.0);
                    c = rgb_of_hsl(h);
                }
                "lumOff" => {
                    let mut h = hsl(c);
                    h[2] = (h[2] + v).clamp(0.0, 1.0);
                    c = rgb_of_hsl(h);
                }
                "satMod" => {
                    let mut h = hsl(c);
                    h[1] = (h[1] * v).clamp(0.0, 1.0);
                    c = rgb_of_hsl(h);
                }
                "tint" => c = c.map(|x| x + (1.0 - x) * (1.0 - v)),
                "shade" => c = c.map(|x| x * v),
                "alpha" => alpha = v.clamp(0.0, 1.0),
                _ => {}
            }
        }
        let b = |x: f32| (x * 255.0).round().clamp(0.0, 255.0) as u8;
        Some(Rgba([b(c[0]), b(c[1]), b(c[2]), b(alpha)]))
    }

    /// The colour inside `parent` (its first colour element).
    fn color_in(&self, parent: &El, ph: Option<Rgba>) -> Option<Rgba> {
        parent.elements().find_map(|e| self.color(e, ph))
    }
}

/// What a fill gives: a colour, a picture, or nothing.
enum Paint {
    None,
    Color(Rgba),
    Picture(Arc<str>),
}

impl Slide<'_> {
    /// The fill of a shape's properties; `Err` when none is said.
    fn paint(&self, props: &El, part: &Part, ph: Option<Rgba>) -> Result<Paint, ()> {
        for e in props.elements() {
            match e.name.as_str() {
                "noFill" => return Ok(Paint::None),
                "solidFill" => return Ok(self.color_in(e, ph).map_or(Paint::None, Paint::Color)),
                "gradFill" => {
                    // A gradient shown as its middle stop's colour.
                    let stops: Vec<&El> = e.child("gsLst").map(|l| l.children("gs").collect()).unwrap_or_default();
                    let mid = stops.get(stops.len() / 2).or(stops.first());
                    return Ok(mid.and_then(|s| self.color_in(s, ph)).map_or(Paint::None, Paint::Color));
                }
                "pattFill" => return Ok(e.child("fgClr").and_then(|c| self.color_in(c, ph)).map_or(Paint::None, Paint::Color)),
                "blipFill" => {
                    let key = e.child("blip").and_then(|b| b.attr("embed")).and_then(|id| part.rels.get(id)).filter(|r| !r.external).map(|r| Arc::from(r.target.as_str()));
                    return Ok(key.map_or(Paint::None, Paint::Picture));
                }
                "grpFill" => return Ok(Paint::None),
                _ => {}
            }
        }
        Err(())
    }

    fn background(&mut self, bg: &El, part: &Part, w: f32, h: f32, out: &mut Option<Rgba>) {
        if let Some(p) = bg.child("bgPr") {
            match self.paint(p, part, None) {
                Ok(Paint::Color(c)) => *out = Some(c),
                Ok(Paint::Picture(key)) => self.items.push(Item::Image { x: 0.0, y: 0.0, w, h, key }),
                _ => {}
            }
        } else if let Some(r) = bg.child("bgRef") {
            if let Some(c) = self.color_in(r, None) {
                *out = Some(c);
            }
        }
    }
}

// --- shapes -------------------------------------------------------------

impl Slide<'_> {
    fn tree(&mut self, tree: &El, part: &Part, xf: Xform, chain: &Chain, level: Level, depth: usize) {
        if depth > 16 {
            return;
        }
        for e in tree.elements() {
            if self.shapes >= MAX_SHAPES {
                return;
            }
            self.shapes += 1;
            match e.name.as_str() {
                "sp" | "cxnSp" => {
                    // Placeholders of the master and layout are prompts,
                    // not content.
                    if level != Level::Slide && ph_of(e).is_some() {
                        continue;
                    }
                    self.shape(e, part, xf, chain);
                }
                "pic" => self.picture(e, part, xf, chain),
                "grpSp" => {
                    let Some(gx) = e.child("grpSpPr").and_then(|p| p.child("xfrm")) else {
                        self.tree(e, part, xf, chain, level, depth + 1);
                        continue;
                    };
                    let f = frame_of(Some(gx)).unwrap_or_default();
                    let (cx, cy) = gx.child("chOff").map_or((f.x, f.y), |c| (c.num("x").unwrap_or(0.0), c.num("y").unwrap_or(0.0)));
                    let (cw, ch) = gx.child("chExt").map_or((f.w, f.h), |c| (c.num("cx").unwrap_or(0.0), c.num("cy").unwrap_or(0.0)));
                    let kx = if cw > 0.0 { f.w / cw } else { 1.0 };
                    let ky = if ch > 0.0 { f.h / ch } else { 1.0 };
                    // child point p maps to f.x + (p - cx) * kx, then xf.
                    let inner = Xform { sx: xf.sx * kx, sy: xf.sy * ky, dx: xf.dx + xf.sx * (f.x - cx * kx), dy: xf.dy + xf.sy * (f.y - cy * ky) };
                    self.tree(e, part, inner, chain, level, depth + 1);
                }
                "graphicFrame" => self.graphic_frame(e, part, xf, chain),
                "AlternateContent" => {
                    if let Some(c) = e.child("Choice").or_else(|| e.child("Fallback")) {
                        self.tree(c, part, xf, chain, level, depth + 1);
                    }
                }
                _ => {}
            }
        }
    }

    /// The box of a shape, from its own properties or its placeholder's.
    fn frame(&self, e: &El, props: Option<&El>, chain: &Chain) -> Option<Frame> {
        if let Some(f) = frame_of(props.and_then(|p| p.child("xfrm"))) {
            return Some(f);
        }
        let ph = ph_of(e)?;
        let from = |p: Option<&Part>, by_type: bool| find_ph(p.and_then(Part::tree), &ph, by_type).and_then(|s| frame_of(s.child("spPr").and_then(|p| p.child("xfrm"))));
        from(chain.layout, false).or_else(|| from(chain.master, true))
    }

    fn shape(&mut self, e: &El, part: &Part, xf: Xform, chain: &Chain) {
        let props = e.child("spPr");
        let Some(f) = self.frame(e, props, chain) else { return };
        let style = e.child("style");
        let (x, y) = xf.pt(f.x, f.y);
        let (w, h) = (f.w * xf.sx, f.h * xf.sy);
        if !(w.is_finite() && h.is_finite()) || w > 1e5 || h > 1e5 {
            return;
        }
        let geom = props.and_then(|p| p.child("prstGeom")).and_then(|g| g.attr("prst")).unwrap_or(if e.name == "cxnSp" { "line" } else { "rect" });
        let cust = props.and_then(|p| p.child("custGeom"));
        let path = match cust {
            Some(c) => custom(c, w, h),
            None => preset(geom, w, h),
        };
        let transform = shape_transform(x, y, w, h, f);
        let mut path = path;
        path.apply_affine(transform);
        // Fill: own, else the style's reference.
        let ref_color = |name: &str| style.and_then(|s| s.child(name)).filter(|r| r.num("idx").unwrap_or(0.0) > 0.0).and_then(|r| self.color_in(r, None));
        let fill = match props.map(|p| self.paint(p, part, None)) {
            Some(Ok(p)) => p,
            _ => ref_color("fillRef").map_or(Paint::None, Paint::Color),
        };
        let line = props.and_then(|p| p.child("ln"));
        let line_color = match line {
            Some(l) if l.child("noFill").is_some() => None,
            Some(l) => match self.paint(l, part, None) {
                Ok(Paint::Color(c)) => Some(c),
                Ok(_) => None,
                Err(()) => ref_color("lnRef"),
            },
            None => ref_color("lnRef"),
        };
        let line_w = line.and_then(|l| l.num("w")).map_or(0.75, |w| (w / EMU).clamp(0.1, 100.0));
        let line_only = matches!(geom, "line" | "straightConnector1" | "bentConnector2" | "bentConnector3" | "curvedConnector3") && cust.is_none();
        match fill {
            Paint::Color(c) if !line_only => self.items.push(Item::Path { path: path.clone(), fill: Some(c), stroke: None }),
            Paint::Picture(key) if !line_only => {
                let b = crate::draw::tidy(path.clone()).bounding_box();
                self.items.push(Item::Clip { x: b.x0 as f32, y: b.y0 as f32, w: b.width() as f32, h: b.height() as f32 });
                self.items.push(Item::Image { x, y, w, h, key });
                self.items.push(Item::Unclip);
            }
            _ => {}
        }
        if let Some(c) = line_color {
            self.items.push(Item::Path { path, fill: None, stroke: Some((line_w, c)) });
        }
        if let Some(body) = e.child("txBody") {
            let font_ref = style.and_then(|s| s.child("fontRef")).and_then(|r| self.color_in(r, None));
            self.text_body(e, body, (x, y, w, h), f, chain, font_ref, None);
        }
    }

    fn picture(&mut self, e: &El, part: &Part, xf: Xform, chain: &Chain) {
        let props = e.child("spPr");
        let Some(f) = self.frame(e, props, chain) else { return };
        let (x, y) = xf.pt(f.x, f.y);
        let (w, h) = (f.w * xf.sx, f.h * xf.sy);
        let Some(fill) = e.child("blipFill") else { return };
        let Some(key) = fill.child("blip").and_then(|b| b.attr("embed")).and_then(|id| part.rels.get(id)).filter(|r| !r.external).map(|r| Arc::<str>::from(r.target.as_str())) else { return };
        if !(w > 0.0 && h > 0.0 && w < 1e5 && h < 1e5) {
            return;
        }
        // A cropped picture: drawn larger, clipped to its box.
        let crop = fill.child("srcRect");
        let c = |k: &str| crop.and_then(|s| s.num(k)).unwrap_or(0.0) / 100_000.0;
        let (l, t, r, b) = (c("l"), c("t"), c("r"), c("b"));
        let (fw, fh) = ((1.0 - l - r).max(0.01), (1.0 - t - b).max(0.01));
        let (iw, ih) = (w / fw, h / fh);
        let cropped = l != 0.0 || t != 0.0 || r != 0.0 || b != 0.0;
        if cropped {
            self.items.push(Item::Clip { x, y, w, h });
        }
        self.items.push(Item::Image { x: x - l * iw, y: y - t * ih, w: iw, h: ih, key });
        if cropped {
            self.items.push(Item::Unclip);
        }
        if let Some(l) = props.and_then(|p| p.child("ln"))
            && let Ok(Paint::Color(c)) = self.paint(l, part, None)
        {
            let lw = l.num("w").map_or(0.75, |w| (w / EMU).clamp(0.1, 100.0));
            let mut p = preset("rect", w, h);
            p.apply_affine(Affine::translate((x as f64, y as f64)));
            self.items.push(Item::Path { path: p, fill: None, stroke: Some((lw, c)) });
        }
    }

    fn graphic_frame(&mut self, e: &El, part: &Part, xf: Xform, chain: &Chain) {
        let Some(f) = frame_of(e.child("xfrm")) else { return };
        let (x, y) = xf.pt(f.x, f.y);
        let (w, h) = (f.w * xf.sx, f.h * xf.sy);
        let Some(data) = e.path(&["graphic", "graphicData"]) else { return };
        if let Some(t) = data.child("tbl") {
            self.table(t, x, y, xf, part, chain);
        } else if data.child("relIds").is_some() {
            // SmartArt: the drawing Office saves beside it, when there is one.
            let drawing = data.child("relIds").and_then(|r| r.attr("dm")).and_then(|dm| part.rels.get(dm)).and_then(|dm| {
                let data_root = self.pkg.xml(&dm.target, &[]).ok()?;
                let id = data_root.find("dataModelExt").and_then(|d| d.attr("relId"))?.to_string();
                part.rels.get(&id).map(|r| r.target.clone())
            });
            let drawing = drawing.or_else(|| {
                let mut ds = part.rels.values().filter(|r| r.kind == "diagramDrawing");
                let first = ds.next()?;
                ds.next().is_none().then(|| first.target.clone())
            });
            if let Some(d) = drawing
                && let Some(p) = Part::read(self.pkg, &d)
                && let Some(tree) = p.root.child("spTree")
            {
                let inner = Xform { sx: xf.sx, sy: xf.sy, dx: x, dy: y };
                let no_chain = Chain { slide: chain.slide, layout: None, master: None };
                self.tree(tree, &p, inner, &no_chain, Level::Slide, 1);
            }
        } else if let Some(pic) = data.find("oleObj").and_then(|o| o.find("pic")) {
            // An embedded object: the picture saved to stand for it.
            self.picture(pic, part, xf, chain);
        } else if data.child("chart").is_some() || data.find("chart").is_some() {
            let grey = Rgba([0xA0, 0xA0, 0xA0, 255]);
            let mut p = preset("rect", w, h);
            p.apply_affine(Affine::translate((x as f64, y as f64)));
            self.items.push(Item::Path { path: p, fill: Some(Rgba([0xF4, 0xF4, 0xF4, 255])), stroke: Some((0.75, grey)) });
        }
    }
}

/// The transform of a shape's path drawn in a `w` × `h` box at the
/// origin: flips and rotation about its centre, then to `(x, y)`.
fn shape_transform(x: f32, y: f32, w: f32, h: f32, f: Frame) -> Affine {
    let (cx, cy) = (w as f64 / 2.0, h as f64 / 2.0);
    let mut a = Affine::translate((-cx, -cy));
    if f.flip_h {
        a = Affine::scale_non_uniform(-1.0, 1.0) * a;
    }
    if f.flip_v {
        a = Affine::scale_non_uniform(1.0, -1.0) * a;
    }
    if f.rot != 0.0 {
        a = Affine::rotate((f.rot as f64).to_radians()) * a;
    }
    Affine::translate((x as f64 + cx, y as f64 + cy)) * a
}

/// A free-form shape's paths scaled into a `w` × `h` box.
fn custom(g: &El, w: f32, h: f32) -> BezPath {
    let mut out = BezPath::new();
    let Some(list) = g.child("pathLst") else { return out };
    for p in list.children("path").take(256) {
        let pw = p.num("w").filter(|v| *v > 0.0).unwrap_or(w * EMU);
        let ph = p.num("h").filter(|v| *v > 0.0).unwrap_or(h * EMU);
        let (kx, ky) = (w as f64 / pw as f64, h as f64 / ph as f64);
        let pt = |e: &El| -> Point { Point::new(e.num("x").unwrap_or(0.0) as f64 * kx, e.num("y").unwrap_or(0.0) as f64 * ky) };
        let mut cur = Point::ZERO;
        let mut n = 0usize;
        for c in p.elements() {
            n += 1;
            if n > 20_000 {
                break;
            }
            match c.name.as_str() {
                "moveTo" => {
                    if let Some(q) = c.child("pt") {
                        cur = pt(q);
                        out.move_to(cur);
                    }
                }
                "lnTo" => {
                    if let Some(q) = c.child("pt") {
                        cur = pt(q);
                        out.line_to(cur);
                    }
                }
                "cubicBezTo" => {
                    let v: Vec<Point> = c.children("pt").map(pt).collect();
                    if v.len() == 3 {
                        out.curve_to(v[0], v[1], v[2]);
                        cur = v[2];
                    }
                }
                "quadBezTo" => {
                    let v: Vec<Point> = c.children("pt").map(pt).collect();
                    if v.len() == 2 {
                        out.quad_to(v[0], v[1]);
                        cur = v[1];
                    }
                }
                "arcTo" => {
                    let wr = c.num("wR").unwrap_or(0.0) as f64 * kx;
                    let hr = c.num("hR").unwrap_or(0.0) as f64 * ky;
                    let st = (c.num("stAng").unwrap_or(0.0) / 60_000.0) as f64;
                    let sw = (c.num("swAng").unwrap_or(0.0) / 60_000.0) as f64;
                    // The arc's centre from its start point and angle.
                    let (s0, c0) = st.to_radians().sin_cos();
                    let centre = Point::new(cur.x - wr * c0, cur.y - hr * s0);
                    let steps = ((sw.abs() / 10.0).ceil() as usize).clamp(1, 72);
                    for i in 1..=steps {
                        let a = (st + sw * i as f64 / steps as f64).to_radians();
                        cur = Point::new(centre.x + wr * a.cos(), centre.y + hr * a.sin());
                        out.line_to(cur);
                    }
                }
                "close" => out.close_path(),
                _ => {}
            }
        }
    }
    out
}

// --- text ---------------------------------------------------------------

/// Paragraph and run properties found up the chain of styles.
struct Levels<'e> {
    /// Sources of level properties, lowest priority first (`lstStyle`
    /// like elements).
    lists: Vec<&'e El>,
}

impl<'e> Levels<'e> {
    /// The level-`lvl` paragraph properties, lowest priority first.
    fn chain(&self, lvl: usize) -> Vec<&'e El> {
        let name = format!("lvl{}pPr", lvl + 1);
        self.lists.iter().filter_map(|l| l.child(&name)).collect()
    }
}

impl Slide<'_> {
    fn font_name(&self, f: &str) -> String {
        match f {
            "+mj-lt" => self.theme.major.clone(),
            "+mn-lt" => self.theme.minor.clone(),
            "+mj-ea" => self.theme.major_ea.clone(),
            "+mn-ea" => self.theme.minor_ea.clone(),
            f => f.to_string(),
        }
    }

    /// A run's style from its properties over those of its level.
    fn run_style(&self, chain: &[&El], rpr: Option<&El>, scale: f32, font_ref: Option<Rgba>) -> Style {
        let defs: Vec<&El> = chain.iter().filter_map(|p| p.child("defRPr")).chain(rpr).collect();
        let attr = |k: &str| defs.iter().rev().find_map(|d| d.attr(k));
        let flag = |k: &str| attr(k).map(|v| v == "1" || v == "true");
        let size = attr("sz").and_then(|v| v.parse::<f32>().ok()).map_or(18.0, |v| v / 100.0).clamp(1.0, 4000.0) * scale;
        let color = defs.iter().rev().find_map(|d| d.child("solidFill").and_then(|f| self.color_in(f, None))).or(font_ref).unwrap_or(Rgba::BLACK);
        let family = defs.iter().rev().find_map(|d| d.child("latin").and_then(|l| l.attr("typeface"))).map(|f| self.font_name(f)).filter(|f| !f.is_empty()).unwrap_or_else(|| self.theme.minor.clone());
        let highlight = defs.iter().rev().find_map(|d| d.child("highlight").and_then(|h| self.color_in(h, None)));
        Style {
            family: Arc::from(family.as_str()),
            size,
            bold: flag("b").unwrap_or(false),
            italic: flag("i").unwrap_or(false),
            color,
            underline: attr("u").is_some_and(|u| u != "none"),
            strike: attr("strike").is_some_and(|s| s != "noStrike"),
            highlight,
            shift: match attr("baseline").and_then(|v| v.parse::<f32>().ok()) {
                Some(b) if b > 0.0 => 1,
                Some(b) if b < 0.0 => -1,
                _ => 0,
            },
            caps: attr("cap").is_some_and(|c| c == "all"),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn text_body(&mut self, shape: &El, body: &El, (x, y, w, h): (f32, f32, f32, f32), f: Frame, chain: &Chain, font_ref: Option<Rgba>, table_cell: Option<[f32; 4]>) {
        // The style lists that apply, master first.
        let ph = ph_of(shape);
        let master_tx = chain.master.and_then(|m| m.root.child("txStyles"));
        let mut lists: Vec<&El> = Vec::new();
        let mut body_prs: Vec<&El> = Vec::new();
        if table_cell.is_none() {
            if let Some(t) = master_tx {
                let which = match &ph {
                    Some(p) if title_kind(&p.kind) => "titleStyle",
                    Some(p) if !matches!(p.kind.as_str(), "dt" | "ftr" | "sldNum") => "bodyStyle",
                    _ => "otherStyle",
                };
                if let Some(s) = t.child(which) {
                    lists.push(s);
                }
            }
            if let Some(p) = &ph {
                for (part, by_type) in [(chain.master, true), (chain.layout, false)] {
                    if let Some(s) = find_ph(part.and_then(Part::tree), p, by_type)
                        && let Some(b) = s.child("txBody")
                    {
                        if let Some(l) = b.child("lstStyle") {
                            lists.push(l);
                        }
                        if let Some(bp) = b.child("bodyPr") {
                            body_prs.push(bp);
                        }
                    }
                }
            }
        }
        if let Some(l) = body.child("lstStyle") {
            lists.push(l);
        }
        if let Some(bp) = body.child("bodyPr") {
            body_prs.push(bp);
        }
        let levels = Levels { lists };
        let bp_attr = |k: &str| body_prs.iter().rev().find_map(|b| b.attr(k));
        let ins = |k: &str, d: f32| bp_attr(k).and_then(|v| v.parse::<f32>().ok()).map_or(d, |v| v / EMU);
        let [l_ins, t_ins, r_ins, b_ins] = table_cell.unwrap_or([ins("lIns", 7.2), ins("tIns", 3.6), ins("rIns", 7.2), ins("bIns", 3.6)]);
        let vertical = bp_attr("vert").is_some_and(|v| v != "horz");
        let rotated = (f.rot - 90.0).abs() < 1.0 || (f.rot - 270.0).abs() < 1.0;
        if vertical || rotated {
            // Text set sideways is left out rather than drawn wrong.
            return;
        }
        let wrap = bp_attr("wrap") != Some("none");
        let anchor = bp_attr("anchor").unwrap_or("t");
        let autofit = body_prs.iter().rev().find_map(|b| b.child("normAutofit"));
        let scale = autofit.and_then(|a| a.num("fontScale")).map_or(1.0, |s| (s / 100_000.0).clamp(0.1, 1.0));
        let spacing_cut = autofit.and_then(|a| a.num("lnSpcReduction")).map_or(0.0, |s| (s / 100_000.0).clamp(0.0, 0.9));
        let inner_w = (w - l_ins - r_ins).max(1.0);
        let set_w = if wrap { inner_w } else { 100_000.0 };
        // Paragraphs set, then placed.
        let mut paras = Vec::new();
        let mut counters: HashMap<(usize, String), i32> = HashMap::new();
        for (pi, p) in body.children("p").enumerate().take(2_000) {
            let ppr = p.child("pPr");
            let lvl = ppr.and_then(|p| p.num("lvl")).map_or(0, |v| v.clamp(0.0, 8.0) as usize);
            let mut pchain = levels.chain(lvl);
            if let Some(pp) = ppr {
                pchain.push(pp);
            }
            let pattr = |k: &str| pchain.iter().rev().find_map(|e| e.attr(k));
            let pchild = |k: &str| pchain.iter().rev().find_map(|e| e.child(k));
            let align = match pattr("algn") {
                Some("ctr") => Align::Center,
                Some("r") => Align::Right,
                Some("just" | "dist") => Align::Justify,
                _ => Align::Left,
            };
            let mar_l = pattr("marL").and_then(|v| v.parse::<f32>().ok()).map_or(0.0, |v| v / EMU);
            let indent = pattr("indent").and_then(|v| v.parse::<f32>().ok()).map_or(0.0, |v| v / EMU);
            let mut spans: Vec<Span> = Vec::new();
            let mut first_style: Option<Arc<Style>> = None;
            for r in p.elements() {
                match r.name.as_str() {
                    "r" | "fld" => {
                        let st = Arc::new(self.run_style(&pchain, r.child("rPr"), scale, font_ref));
                        first_style.get_or_insert_with(|| st.clone());
                        let t = r.child("t").map(|t| t.text()).unwrap_or_default();
                        if !t.is_empty() {
                            spans.push(Span::new(t.replace(['\u{b}', '\r'], "\n"), st));
                        }
                    }
                    "br" => {
                        let st = Arc::new(self.run_style(&pchain, r.child("rPr"), scale, font_ref));
                        spans.push(Span::new("\n", st));
                    }
                    _ => {}
                }
            }
            let end_style = Arc::new(self.run_style(&pchain, p.child("endParaRPr"), scale, font_ref));
            // A bullet before text.
            if !spans.is_empty() && pchild("buNone").is_none() {
                let bstyle = first_style.clone().unwrap_or_else(|| end_style.clone());
                let mut b = (*bstyle).clone();
                if let Some(c) = pchild("buClr").and_then(|c| self.color_in(c, None)) {
                    b.color = c;
                }
                if let Some(pct) = pchild("buSzPct").and_then(|s| s.num("val")) {
                    b.size = (b.size * pct / 100_000.0).clamp(1.0, 4000.0);
                }
                let label = if let Some(c) = pchild("buChar") {
                    if let Some(face) = pchild("buFont").and_then(|f| f.attr("typeface")).filter(|f| !f.starts_with('+')) {
                        b.family = Arc::from(face);
                    }
                    let ch = c.attr("char").unwrap_or("•");
                    Some(symbol_bullet(ch, &b.family))
                } else if let Some(a) = pchild("buAutoNum") {
                    let kind = a.attr("type").unwrap_or("arabicPeriod").to_string();
                    let start = a.num("startAt").map_or(1, |v| v as i32);
                    let n = counters.entry((lvl, kind.clone())).or_insert(start - 1);
                    *n += 1;
                    Some(auto_number(*n, &kind))
                } else {
                    None
                };
                if let Some(label) = label {
                    if matches!(&*b.family, "Symbol" | "Wingdings" | "Wingdings 2" | "Wingdings 3") {
                        b.family = bstyle.family.clone();
                    }
                    spans.insert(0, Span::new(format!("{label}\t"), Arc::new(b)));
                }
            } else {
                counters.retain(|(l, _), _| *l < lvl);
            }
            if spans.is_empty() {
                spans.push(Span::new("", end_style.clone()));
            }
            let size = first_style.as_ref().unwrap_or(&end_style).size;
            let spacing = |k: &str| -> Option<f32> {
                let e = pchild(k)?;
                if let Some(p) = e.child("spcPts").and_then(|p| p.num("val")) {
                    Some(p / 100.0)
                } else {
                    e.child("spcPct").and_then(|p| p.num("val")).map(|p| p / 100_000.0 * size * 1.2)
                }
            };
            let line = match pchild("lnSpc") {
                Some(l) if l.child("spcPts").is_some() => LineRule::Exact(l.child("spcPts").and_then(|p| p.num("val")).map_or(size * 1.2, |v| v / 100.0).max(1.0)),
                Some(l) => LineRule::Multiple((l.child("spcPct").and_then(|p| p.num("val")).map_or(1.0, |v| v / 100_000.0) * (1.0 - spacing_cut)).clamp(0.3, 5.0)),
                None => LineRule::Multiple((1.0 - spacing_cut).max(0.3)),
            };
            let mut tabs: Vec<Tab> = pchild("tabLst")
                .map(|t| {
                    t.children("tab")
                        .filter_map(|t| {
                            let pos = t.num("pos")? / EMU;
                            let align = match t.attr("algn") {
                                Some("r" | "dec") => TabAlign::Right,
                                Some("ctr") => TabAlign::Center,
                                _ => TabAlign::Left,
                            };
                            Some(Tab { pos, align, leader: None })
                        })
                        .take(32)
                        .collect()
                })
                .unwrap_or_default();
            if indent < 0.0 {
                tabs.push(Tab { pos: mar_l, align: TabAlign::Left, leader: None });
            }
            crate::typeset::sort_tabs(&mut tabs);
            let fmt = ParaFormat { align, left: mar_l, right: 0.0, first: indent, line, tabs };
            let lines = typeset::set(&spans, &fmt, set_w, self.fonts);
            let before = if pi == 0 { 0.0 } else { spacing("spcBef").unwrap_or(0.0) * (1.0 - spacing_cut) };
            let after = spacing("spcAft").unwrap_or(0.0) * (1.0 - spacing_cut);
            paras.push((before, lines, after));
        }
        let total: f32 = paras.iter().map(|(b, l, a)| b + l.height() + a).sum();
        let room = h - t_ins - b_ins;
        let mut cy = y
            + t_ins
            + match anchor {
                "ctr" => (room - total) / 2.0,
                "b" => room - total,
                _ => 0.0,
            };
        // Text not wrapped and centred sits about the box's centre.
        let tx = if !wrap { x + l_ins + (inner_w - paras.iter().flat_map(|(_, l, _)| l.lines.iter().map(|l| l.width)).fold(0.0, f32::max)).min(0.0) / 2.0 } else { x + l_ins };
        for (before, lines, after) in &paras {
            cy += before;
            for i in 0..lines.lines.len() {
                lines.draw_line(i, tx, cy, &mut self.items, &mut self.text);
                cy += lines.lines[i].height;
            }
            cy += after;
        }
    }
}

/// A bullet character as text fonts have it.
fn symbol_bullet(ch: &str, family: &str) -> String {
    let c = ch.chars().next().unwrap_or('•');
    let symbolic = matches!(family, "Symbol" | "Wingdings" | "Wingdings 2" | "Wingdings 3") || ('\u{F000}'..='\u{F0FF}').contains(&c);
    if !symbolic {
        return c.to_string();
    }
    let c = char::from_u32(c as u32 & 0xFF).unwrap_or('•');
    match (family, c) {
        ("Wingdings", 'Ø' | 'è') => "➢".into(),
        ("Wingdings", '§' | 'n') => "▪".into(),
        ("Wingdings", 'q' | 'o') => "□".into(),
        ("Wingdings", 'ü') => "✓".into(),
        ("Wingdings", 'v') => "❖".into(),
        _ => "•".into(),
    }
}

fn auto_number(n: i32, kind: &str) -> String {
    let roman = |n: i32| {
        let mut out = String::new();
        let mut n = n.clamp(1, 3999);
        for (v, s) in [(1000, "m"), (900, "cm"), (500, "d"), (400, "cd"), (100, "c"), (90, "xc"), (50, "l"), (40, "xl"), (10, "x"), (9, "ix"), (5, "v"), (4, "iv"), (1, "i")] {
            while n >= v {
                out.push_str(s);
                n -= v;
            }
        }
        out
    };
    let letter = |n: i32| ((b'a' + ((n.max(1) - 1) % 26) as u8) as char).to_string();
    let body = if kind.starts_with("alphaLc") {
        letter(n)
    } else if kind.starts_with("alphaUc") {
        letter(n).to_uppercase()
    } else if kind.starts_with("romanLc") {
        roman(n)
    } else if kind.starts_with("romanUc") {
        roman(n).to_uppercase()
    } else {
        n.to_string()
    };
    if kind.ends_with("ParenBoth") {
        format!("({body})")
    } else if kind.ends_with("ParenR") {
        format!("{body})")
    } else if kind.ends_with("Plain") {
        body
    } else {
        format!("{body}.")
    }
}

// --- tables -------------------------------------------------------------

impl Slide<'_> {
    fn table(&mut self, t: &El, x: f32, y: f32, xf: Xform, part: &Part, chain: &Chain) {
        let cols: Vec<f32> = t.child("tblGrid").map(|g| g.children("gridCol").filter_map(|c| c.num("w")).map(|w| w * xf.sx).take(200).collect()).unwrap_or_default();
        if cols.is_empty() {
            return;
        }
        let tpr = t.child("tblPr");
        let flag = |k: &str| tpr.and_then(|p| p.attr(k)).is_some_and(|v| v == "1" || v == "true");
        let styled = tpr.and_then(|p| p.child("tableStyleId")).is_some() || t.child("tblPr").is_some();
        let (first_row, band_row) = (flag("firstRow"), flag("bandRow"));
        let accent = self.theme.colors.get("accent1").copied().unwrap_or(0x4472C4);
        let mut col_x = vec![x];
        for w in &cols {
            col_x.push(col_x.last().copied().unwrap_or(x) + w);
        }
        let rows: Vec<&El> = t.children("tr").take(500).collect();
        let mut row_y = y;
        for (ri, tr) in rows.iter().enumerate() {
            let min_h = tr.num("h").map_or(0.0, |h| h * xf.sy);
            // Cells set first, for the row's height.
            let mut cells = Vec::new();
            let mut col = 0usize;
            let mut need = min_h;
            for tc in tr.children("tc").take(200) {
                let span = tc.num("gridSpan").map_or(1, |v| v.clamp(1.0, 200.0) as usize);
                let skip = tc.attr("hMerge").is_some() || tc.attr("vMerge").is_some();
                let at = col.min(cols.len());
                let end = (at + span).min(cols.len());
                col += span;
                cells.push((tc, at, end, skip));
                if skip || at >= end {
                    continue;
                }
                let tcpr = tc.child("tcPr");
                let m = |k: &str, d: f32| tcpr.and_then(|p| p.num(k)).map_or(d, |v| v / EMU);
                let w = col_x[end] - col_x[at];
                let h = self.measure_cell(tc, w - m("marL", 7.2) - m("marR", 7.2), chain);
                need = need.max(h + m("marT", 3.6) + m("marB", 3.6));
            }
            let h = need;
            for (tc, at, end, skip) in cells {
                if skip || at >= end {
                    continue;
                }
                let (cx, cw) = (col_x[at], col_x[end] - col_x[at]);
                let tcpr = tc.child("tcPr");
                let header = first_row && ri == 0;
                let fill = match tcpr.map(|p| self.paint(p, part, None)) {
                    Some(Ok(Paint::Color(c))) => Some(c),
                    Some(Ok(_)) => None,
                    _ if styled && header => Some(Rgba::rgb(accent)),
                    _ if styled && band_row && (ri - first_row as usize) % 2 == 0 => Some(tint(accent, 0.8)),
                    _ if styled => Some(tint(accent, 0.9)),
                    _ => None,
                };
                if let Some(c) = fill {
                    self.items.push(Item::Rect { x: cx, y: row_y, w: cw, h, color: c });
                }
                for (k, x0, y0, x1, y1) in [("lnL", cx, row_y, cx, row_y + h), ("lnR", cx + cw, row_y, cx + cw, row_y + h), ("lnT", cx, row_y, cx + cw, row_y), ("lnB", cx, row_y + h, cx + cw, row_y + h)] {
                    let line = tcpr.and_then(|p| p.child(k));
                    let color = match line.map(|l| self.paint(l, part, None)) {
                        Some(Ok(Paint::Color(c))) => Some(c),
                        Some(Ok(_)) => None,
                        _ if styled => Some(Rgba::WHITE),
                        _ => None,
                    };
                    if let Some(c) = color {
                        let lw = line.and_then(|l| l.num("w")).map_or(1.0, |w| (w / EMU).clamp(0.1, 20.0));
                        self.items.push(Item::Line { x0, y0, x1, y1, width: lw, color: c });
                    }
                }
                if let Some(body) = tc.child("txBody") {
                    let m = |k: &str, d: f32| tcpr.and_then(|p| p.num(k)).map_or(d, |v| v / EMU);
                    let font = if styled && header { Some(Rgba::WHITE) } else { None };
                    let anchor_h = h;
                    let ins = [m("marL", 7.2), m("marT", 3.6), m("marR", 7.2), m("marB", 3.6)];
                    // Cell text styled by the cell's own anchor.
                    let mut body = body.clone();
                    if let Some(a) = tcpr.and_then(|p| p.attr("anchor")) {
                        let mut bp = body.child("bodyPr").cloned().unwrap_or(El { name: "bodyPr".into(), ..El::default() });
                        bp.attrs.retain(|(k, _)| k != "anchor");
                        bp.attrs.push(("anchor".into(), a.into()));
                        body.kids.retain(|k| !matches!(k, crate::ooxml::Node::El(e) if e.name == "bodyPr"));
                        body.kids.insert(0, crate::ooxml::Node::El(bp));
                    }
                    let shape = El::default();
                    let bold_header = styled && header;
                    let before = self.items.len();
                    self.text_body(&shape, &body, (cx, row_y, cw, anchor_h), Frame::default(), chain, font, Some([ins[0], ins[1], ins[2], ins[3]]));
                    if bold_header {
                        // Header text is bold in the default style.
                        for it in &mut self.items[before..] {
                            if let Item::Glyphs(g) = it {
                                g.embolden = true;
                            }
                        }
                    }
                }
            }
            row_y += h;
            if row_y > 1e5 {
                break;
            }
        }
    }

    fn measure_cell(&mut self, tc: &El, w: f32, _chain: &Chain) -> f32 {
        let Some(body) = tc.child("txBody") else { return 0.0 };
        let mut total = 0.0;
        for p in body.children("p").take(500) {
            let mut spans = Vec::new();
            for r in p.children("r") {
                let size = r.child("rPr").and_then(|r| r.num("sz")).map_or(18.0, |v| v / 100.0);
                spans.push(Span::new(r.child("t").map(|t| t.text()).unwrap_or_default(), Arc::new(Style { size, family: Arc::from(self.theme.minor.as_str()), ..Style::default() })));
            }
            if spans.is_empty() {
                let size = p.child("endParaRPr").and_then(|r| r.num("sz")).map_or(18.0, |v| v / 100.0);
                spans.push(Span::new("", Arc::new(Style { size, ..Style::default() })));
            }
            total += typeset::set(&spans, &ParaFormat::default(), w.max(1.0), self.fonts).height();
        }
        total
    }
}

fn tint(rgb: u32, k: f32) -> Rgba {
    let t = |c: u32| ((c as f32) + (255.0 - c as f32) * k).round().clamp(0.0, 255.0) as u8;
    Rgba([t((rgb >> 16) & 255), t((rgb >> 8) & 255), t(rgb & 255), 255])
}
