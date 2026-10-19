//! OpenDocument presentations (.odp): one page per slide, notes left out.
//! The content is read at opening; each slide is drawn when asked for: its
//! background, the shapes of its master page, then its own.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use vello_cpu::kurbo::{Affine, BezPath, Ellipse, Point, Rect, RoundedRect, Shape};

use super::{Props, Styles, border, bullet, collapse, color, length, level_style, list_indent, number, para_format, percent, push};
use crate::draw::{self, Item, Page, Rgba};
use crate::geom::preset;
use crate::ooxml::{El, Node, Package, Pictures};
use crate::text::TextRun;
use crate::typeset::{self, FontCache, Lines, ParaFormat, Span, Style, Tab, TabAlign};
use crate::{Error, Fonts, Image, Limits};

/// Slides kept laid out.
const KEEP: usize = 4;
const MAX_SHAPES: usize = 5_000;
const MAX_PARAS: usize = 2_000;

pub(crate) struct Doc {
    pkg: Package,
    st: Styles,
    slides: Vec<El>,
    /// Header, footer and date texts slides name.
    decls: HashMap<String, String>,
    pages: VecDeque<(usize, Page)>,
    fonts: FontCache,
    pictures: Pictures,
    limits: Limits,
}

fn child_mut<'a>(e: &'a mut El, name: &str) -> Option<&'a mut El> {
    e.kids.iter_mut().find_map(|k| match k {
        Node::El(c) if c.name == name => Some(c),
        _ => None,
    })
}

impl Doc {
    pub fn open(data: Vec<u8>, limits: &Limits, fonts: &Fonts) -> Result<Doc, Error> {
        let pkg = Package::open(data, limits)?;
        let (mut content, styles) = super::read_parts(&pkg)?;
        let st = Styles::read(styles.as_ref(), &content);
        let mut slides = Vec::new();
        let mut decls = HashMap::new();
        if let Some(pres) = child_mut(&mut content, "body").and_then(|b| child_mut(b, "presentation")) {
            for k in std::mem::take(&mut pres.kids) {
                let Node::El(e) = k else { continue };
                match e.name.as_str() {
                    "page" if slides.len() < limits.max_pages.max(1) => slides.push(e),
                    "header-decl" | "footer-decl" | "date-time-decl" => {
                        if let Some(n) = e.attr("name") {
                            decls.insert(n.to_string(), e.text());
                        }
                    }
                    _ => {}
                }
            }
        }
        if slides.is_empty() {
            return Err(crate::ooxml::bad("the presentation has no slides"));
        }
        Ok(Doc { pkg, st, slides, decls, pages: VecDeque::new(), fonts: FontCache::new(fonts.clone()), pictures: Pictures::default(), limits: limits.clone() })
    }

    pub fn page_count(&self) -> usize {
        self.slides.len()
    }

    fn master(&self, slide: &El) -> Option<&El> {
        slide.attr("master-page-name").and_then(|m| self.st.master(m)).or(self.st.masters.first())
    }

    fn size(&self, slide: &El) -> (f32, f32) {
        let layout = self.master(slide).and_then(|m| m.attr("page-layout-name")).and_then(|l| self.st.layouts.get(l)).and_then(|l| l.child("page-layout-properties"));
        let d = |k: &str, def: f32| layout.and_then(|p| p.attr(k)).and_then(length).filter(|v| (36.0..=4000.0).contains(v)).unwrap_or(def);
        (d("page-width", 793.7), d("page-height", 446.5))
    }

    pub fn page_size(&self, n: usize) -> Result<(f32, f32), Error> {
        self.slides.get(n).map(|s| self.size(s)).ok_or(Error::NoSuchPage)
    }

    fn page(&mut self, n: usize) -> Result<&Page, Error> {
        if n >= self.slides.len() {
            return Err(Error::NoSuchPage);
        }
        if !self.pages.iter().any(|(i, _)| *i == n) {
            let page = self.draw(n);
            if self.pages.len() >= KEEP {
                self.pages.pop_front();
            }
            self.pages.push_back((n, page));
        }
        self.pages.iter().find(|(i, _)| *i == n).map(|(_, p)| p).ok_or(Error::NoSuchPage)
    }

    fn draw(&mut self, n: usize) -> Page {
        let slide = &self.slides[n];
        let (w, h) = self.size(slide);
        let master = self.master(slide).cloned();
        let dp = self.st.props("drawing-page", slide.attr("style-name"), "drawing-page-properties");
        let flag = |k: &str| dp.get(k) == Some("true");
        let decl = |k: &str| slide.attr(k).and_then(|d| self.decls.get(d)).cloned();
        let fields = Fields {
            number: n + 1,
            count: self.slides.len(),
            footer: if flag("display-footer") { decl("use-footer-name") } else { None },
            header: if flag("display-header") { decl("use-header-name") } else { None },
            date: if flag("display-date-time") { decl("use-date-time-name") } else { None },
            show_number: flag("display-page-number"),
        };
        let mut s = Slide { st: &mut self.st, fonts: &mut self.fonts, fields, items: Vec::new(), text: Vec::new(), shapes: 0 };
        // The background: the slide's own, else its master's.
        let fill = match s.fill(&dp) {
            Fill::None => {
                let mdp = s.st.props("drawing-page", master.as_ref().and_then(|m| m.attr("style-name")), "drawing-page-properties");
                s.fill(&mdp)
            }
            f => f,
        };
        match fill {
            Fill::Color(c) => s.items.push(Item::Rect { x: 0.0, y: 0.0, w, h, color: c }),
            Fill::Image(key) => s.items.push(Item::Image { x: 0.0, y: 0.0, w, h, key }),
            Fill::None => {}
        }
        if let Some(m) = &master {
            s.shapes_in(m, true, 0);
        }
        s.shapes_in(slide, false, 0);
        Page { width: w, height: h, background: Some(Rgba::WHITE), items: s.items, text: s.text }
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

/// What the fields of a slide show.
struct Fields {
    number: usize,
    count: usize,
    footer: Option<String>,
    header: Option<String>,
    date: Option<String>,
    show_number: bool,
}

enum Fill {
    None,
    Color(Rgba),
    Image(Arc<str>),
}

struct Slide<'a> {
    st: &'a mut Styles,
    fonts: &'a mut FontCache,
    fields: Fields,
    items: Vec<Item>,
    text: Vec<TextRun>,
    shapes: usize,
}

/// A paragraph read, to be set at a width.
struct Para {
    fmt: ParaFormat,
    spans: Vec<Span>,
    before: f32,
    after: f32,
}

/// What text in a shape starts from: its style, paragraph properties,
/// list style and, for outlines, the styles of deeper levels.
struct TextCtx {
    base: Style,
    pp: Props,
    list: Option<El>,
    /// "Master-outline" for outline placeholders: level n takes
    /// "Master-outline{n}".
    outline: Option<String>,
}

// --- shapes -------------------------------------------------------------

impl Slide<'_> {
    fn shapes_in(&mut self, parent: &El, master: bool, depth: usize) {
        if depth > 16 {
            return;
        }
        for e in parent.elements() {
            if self.shapes >= MAX_SHAPES {
                return;
            }
            self.shapes += 1;
            // Placeholders of the master page are prompts, not content;
            // its header, footer, date and number show when the slide says.
            if master && let Some(class) = e.attr("class") {
                let shown = match class {
                    "footer" => self.fields.footer.is_some(),
                    "header" => self.fields.header.is_some(),
                    "date-time" => self.fields.date.is_some(),
                    "page-number" => self.fields.show_number,
                    _ => false,
                };
                if !shown {
                    continue;
                }
            }
            if !master && e.attr("placeholder") == Some("true") {
                continue;
            }
            match e.name.as_str() {
                "frame" => self.frame(e),
                "rect" | "ellipse" | "circle" | "custom-shape" | "polygon" | "polyline" | "path" | "line" | "connector" | "regular-polygon" | "caption" => self.shape(e),
                "g" | "a" => self.shapes_in(e, master, depth + 1),
                _ => {}
            }
        }
    }

    /// A shape's style properties of `kind`: presentation styles first,
    /// graphic ones else.
    fn style_of(&mut self, e: &El, kind: &str) -> (Arc<Props>, &'static str) {
        let name = e.attr("style-name");
        let family = if name.is_some_and(|n| self.st.style("presentation", n).is_some()) { "presentation" } else { "graphic" };
        (self.st.props(family, name, kind), family)
    }

    fn fill(&self, p: &Props) -> Fill {
        let alpha = p.get("opacity").and_then(percent).map_or(1.0, |a| a.clamp(0.0, 1.0));
        let with = |mut c: Rgba| {
            c.0[3] = (c.0[3] as f32 * alpha).round() as u8;
            Fill::Color(c)
        };
        let drawn = |kind: &str, attr: &str| p.get(attr).and_then(|n| self.st.drawn.get(&(kind.to_string(), n.to_string())));
        match p.get("fill") {
            Some("solid") => with(p.get("fill-color").and_then(color).unwrap_or(Rgba::rgb(0x729FCF))),
            Some("gradient") => {
                // A gradient shown as the colour halfway.
                let Some(g) = drawn("gradient", "fill-gradient-name") else { return with(p.get("fill-color").and_then(color).unwrap_or(Rgba::WHITE)) };
                let a = g.attr("start-color").and_then(color).unwrap_or(Rgba::BLACK);
                let b = g.attr("end-color").and_then(color).unwrap_or(Rgba::WHITE);
                let mix = |i: usize| ((a.0[i] as u16 + b.0[i] as u16) / 2) as u8;
                with(Rgba([mix(0), mix(1), mix(2), 255]))
            }
            Some("hatch") => {
                if p.get("fill-hatch-solid") == Some("true")
                    && let Some(c) = p.get("fill-color").and_then(color)
                {
                    return with(c);
                }
                let c = drawn("hatch", "fill-hatch-name").and_then(|h| h.attr("color")).and_then(color).unwrap_or(Rgba::BLACK);
                let light = |v: u8| ((v as u16 + 255 * 3) / 4) as u8;
                with(Rgba([light(c.0[0]), light(c.0[1]), light(c.0[2]), 255]))
            }
            Some("bitmap") => match drawn("fill-image", "fill-image-name").and_then(|i| i.attr("href")).map(|h| h.trim_start_matches("./")).filter(|h| !h.is_empty() && !h.contains(':')) {
                Some(h) => Fill::Image(Arc::from(h)),
                None => Fill::None,
            },
            _ => Fill::None,
        }
    }

    fn stroke(&self, p: &Props) -> Option<(f32, Rgba)> {
        match p.get("stroke") {
            Some("solid" | "dash") => {
                let c = p.get("stroke-color").and_then(color).unwrap_or(Rgba::BLACK);
                let w = p.len("stroke-width").unwrap_or(0.0).clamp(0.5, 100.0);
                Some((w, c))
            }
            _ => None,
        }
    }

    /// `path`, in page points, filled and stroked as `p` says.
    fn paint(&mut self, path: BezPath, p: &Props, fill: bool) {
        let path = draw::tidy(path);
        if fill {
            match self.fill(p) {
                Fill::Color(c) => self.items.push(Item::Path { path: path.clone(), fill: Some(c), stroke: None }),
                Fill::Image(key) => {
                    let b = path.bounding_box();
                    let (x, y, w, h) = (b.x0 as f32, b.y0 as f32, b.width() as f32, b.height() as f32);
                    if w > 0.0 && h > 0.0 && w < 1e5 && h < 1e5 {
                        self.items.push(Item::Clip { x, y, w, h });
                        self.items.push(Item::Image { x, y, w, h, key });
                        self.items.push(Item::Unclip);
                    }
                }
                Fill::None => {}
            }
        }
        if let Some(s) = self.stroke(p) {
            self.items.push(Item::Path { path, fill: None, stroke: Some(s) });
        }
    }

    fn frame(&mut self, e: &El) {
        let Some((w, h, a)) = placement(e) else { return };
        let (gp, family) = self.style_of(e, "graphic-properties");
        if let Some(t) = e.child("table") {
            let [_, _, _, _, x, y] = a.as_coeffs();
            self.table(t, x as f32, y as f32, w);
            return;
        }
        if let Some(tb) = e.child("text-box") {
            self.paint(box_path(w, h, a), &gp, true);
            self.text(e, tb, &gp, family, w, h, a);
            return;
        }
        let object = e.child("object").or_else(|| e.child("object-ole")).or_else(|| e.child("plugin"));
        let (x, y) = upright(a, w, h);
        if object.is_some() {
            let grey = Rgba([0xA0, 0xA0, 0xA0, 255]);
            self.items.push(Item::Path { path: box_path(w, h, a), fill: Some(Rgba([0xF4, 0xF4, 0xF4, 255])), stroke: Some((0.75, grey)) });
        }
        // The first picture a viewer can show: frames may hold an SVG
        // with a bitmap after it.
        let images: Vec<&El> = e.children("image").collect();
        let pick = images.iter().find(|i| i.attr("href").is_some_and(raster)).or_else(|| images.iter().find(|i| i.attr("href").is_some_and(|h| !h.ends_with(".svg"))));
        if let Some(href) = pick.and_then(|i| i.attr("href")).map(|h| h.trim_start_matches("./")).filter(|h| !h.is_empty() && !h.contains(':'))
            && w > 0.0
            && h > 0.0
            && w < 1e5
            && h < 1e5
        {
            self.items.push(Item::Image { x, y, w, h, key: Arc::from(href) });
            self.paint(box_path(w, h, a), &gp, false);
        }
    }

    fn shape(&mut self, e: &El) {
        let (gp, family) = self.style_of(e, "graphic-properties");
        let (path, w, h, a, line) = match e.name.as_str() {
            "line" | "connector" => {
                let c = |k: &str| e.attr(k).and_then(length).unwrap_or(0.0) as f64;
                let mut p = BezPath::new();
                p.move_to((c("x1"), c("y1")));
                p.line_to((c("x2"), c("y2")));
                if let Some(t) = e.attr("transform").and_then(transform) {
                    p.apply_affine(t);
                }
                (p, 0.0, 0.0, Affine::IDENTITY, true)
            }
            _ => {
                let Some((w, h, a)) = placement(e) else { return };
                if !(w < 1e5 && h < 1e5) {
                    return;
                }
                let (wd, hd) = (w as f64, h as f64);
                let (mut p, line) = match e.name.as_str() {
                    "rect" => {
                        let r = e.attr("corner-radius").and_then(length).unwrap_or(0.0).max(0.0) as f64;
                        (RoundedRect::new(0.0, 0.0, wd, hd, r.min(wd.min(hd) / 2.0)).to_path(0.1), false)
                    }
                    "ellipse" | "circle" => (Ellipse::new((wd / 2.0, hd / 2.0), (wd / 2.0, hd / 2.0), 0.0).to_path(0.1), false),
                    "polygon" | "polyline" => (scaled(points(e.attr("points").unwrap_or(""), e.name == "polygon"), e.attr("viewBox"), w, h), e.name == "polyline"),
                    "path" => (scaled(BezPath::from_svg(e.attr("d").unwrap_or("")).unwrap_or_default(), e.attr("viewBox"), w, h), false),
                    "regular-polygon" => (regular(e, w, h), false),
                    "custom-shape" => match e.child("enhanced-geometry") {
                        Some(g) => {
                            let (fill, stroke) = enhanced(g, w, h);
                            let mut fill = flip(fill, g, w, h);
                            let mut stroke = flip(stroke, g, w, h);
                            fill.apply_affine(a);
                            stroke.apply_affine(a);
                            self.paint(fill, &gp, true);
                            if let Some(s) = self.stroke(&gp) {
                                self.items.push(Item::Path { path: stroke, fill: None, stroke: Some(s) });
                            }
                            self.text(e, e, &gp, family, w, h, a);
                            return;
                        }
                        None => (preset("rect", w, h), false),
                    },
                    _ => (preset("rect", w, h), false),
                };
                p.apply_affine(a);
                (p, w, h, a, line)
            }
        };
        if line {
            if let Some(s) = self.stroke(&gp).or(Some((0.75, Rgba::BLACK)).filter(|_| gp.get("stroke") != Some("none"))) {
                self.items.push(Item::Path { path, fill: None, stroke: Some(s) });
            }
        } else {
            self.paint(path, &gp, true);
        }
        if w > 0.0 && h > 0.0 {
            self.text(e, e, &gp, family, w, h, a);
        }
    }
}

/// Whether a picture's name says it is one the viewer decodes.
fn raster(h: &str) -> bool {
    let l = h.to_ascii_lowercase();
    [".png", ".jpg", ".jpeg", ".gif", ".bmp", ".webp", ".tif", ".tiff"].iter().any(|x| l.ends_with(x))
}

/// A shape's size and where its box goes: `draw:transform`, else its
/// position.
fn placement(e: &El) -> Option<(f32, f32, Affine)> {
    let w = e.attr("width").and_then(length).unwrap_or(0.0).max(0.0);
    let h = e.attr("height").and_then(length).unwrap_or(0.0).max(0.0);
    let a = match e.attr("transform").and_then(transform) {
        Some(t) => t,
        None => {
            let x = e.attr("x").and_then(length).unwrap_or(0.0);
            let y = e.attr("y").and_then(length).unwrap_or(0.0);
            Affine::translate((x as f64, y as f64))
        }
    };
    (w.is_finite() && h.is_finite()).then_some((w, h, a))
}

/// A `draw:transform` ("rotate (0.5) translate (2cm 3cm)"), each step
/// after the one before it.
fn transform(t: &str) -> Option<Affine> {
    let num = |s: &str| -> Option<f64> {
        if let Some(d) = s.strip_suffix("deg") {
            return d.parse::<f64>().ok().map(f64::to_radians);
        }
        if let Some(r) = s.strip_suffix("rad") {
            return r.parse().ok();
        }
        length(s).map(f64::from)
    };
    let mut a = Affine::IDENTITY;
    let mut rest = t;
    for _ in 0..16 {
        let r = rest.trim_start();
        if r.is_empty() {
            break;
        }
        let open = r.find('(')?;
        let close = open + r[open..].find(')')?;
        let args: Vec<f64> = r[open + 1..close].split(|c: char| c == ',' || c.is_whitespace()).filter(|s| !s.is_empty()).map(num).collect::<Option<_>>()?;
        let op = match (r[..open].trim(), args.as_slice()) {
            ("rotate", [t]) => Affine::rotate(-t),
            ("translate", [x]) => Affine::translate((*x, 0.0)),
            ("translate", [x, y]) => Affine::translate((*x, *y)),
            ("scale", [s]) => Affine::scale(*s),
            ("scale", [x, y]) => Affine::scale_non_uniform(*x, *y),
            ("skewX", [k]) => Affine::skew(k.tan(), 0.0),
            ("skewY", [k]) => Affine::skew(0.0, k.tan()),
            ("matrix", [a, b, c, d, e, f]) => Affine::new([*a, *b, *c, *d, *e, *f]),
            _ => Affine::IDENTITY,
        };
        a = op * a;
        rest = &r[close + 1..];
    }
    a.is_finite().then_some(a)
}

/// Whether `a` turns or flips what it places.
fn turned(a: Affine) -> bool {
    let [m0, m1, m2, m3, _, _] = a.as_coeffs();
    m1.abs() > 1e-3 || m2.abs() > 1e-3 || m0 < 0.0 || m3 < 0.0
}

/// Where a `w` × `h` box placed by `a` sits upright: its top left corner
/// about the same centre.
fn upright(a: Affine, w: f32, h: f32) -> (f32, f32) {
    let c = a * Point::new(w as f64 / 2.0, h as f64 / 2.0);
    (c.x as f32 - w / 2.0, c.y as f32 - h / 2.0)
}

fn box_path(w: f32, h: f32, a: Affine) -> BezPath {
    let mut p = Rect::new(0.0, 0.0, w as f64, h as f64).to_path(0.1);
    p.apply_affine(a);
    p
}

/// Points ("x,y x,y...") as a path.
fn points(s: &str, close: bool) -> BezPath {
    let mut p = BezPath::new();
    for (i, pt) in s.split_whitespace().take(20_000).enumerate() {
        let mut it = pt.split(',').map(|v| v.trim().parse::<f64>().ok());
        let (Some(Some(x)), Some(Some(y))) = (it.next(), it.next()) else { continue };
        if i == 0 {
            p.move_to((x, y));
        } else {
            p.line_to((x, y));
        }
    }
    if close {
        p.close_path();
    }
    p
}

/// `p` in `viewBox` coordinates mapped to a `w` × `h` box.
fn scaled(p: BezPath, view: Option<&str>, w: f32, h: f32) -> BezPath {
    let mut p = draw::tidy(p);
    let v: Vec<f64> = view.unwrap_or("").split_whitespace().filter_map(|n| n.parse().ok()).collect();
    let (x0, y0, vw, vh) = match v.as_slice() {
        [a, b, c, d] if *c > 0.0 && *d > 0.0 => (*a, *b, *c, *d),
        _ => {
            let b = p.bounding_box();
            (b.x0, b.y0, b.width().max(1e-9), b.height().max(1e-9))
        }
    };
    p.apply_affine(Affine::scale_non_uniform(w as f64 / vw, h as f64 / vh) * Affine::translate((-x0, -y0)));
    p
}

fn regular(e: &El, w: f32, h: f32) -> BezPath {
    let n = e.attr("corners").and_then(|c| c.parse::<usize>().ok()).unwrap_or(5).clamp(3, 100);
    let mut p = BezPath::new();
    let (rx, ry) = (w as f64 / 2.0, h as f64 / 2.0);
    for i in 0..n {
        let t = std::f64::consts::TAU * i as f64 / n as f64 - std::f64::consts::FRAC_PI_2;
        let pt = (rx + rx * t.cos(), ry + ry * t.sin());
        if i == 0 {
            p.move_to(pt);
        } else {
            p.line_to(pt);
        }
    }
    p.close_path();
    p
}

/// A custom shape's paths flipped as its geometry says.
fn flip(mut p: BezPath, g: &El, w: f32, h: f32) -> BezPath {
    let fh = g.attr("mirror-horizontal") == Some("true");
    let fv = g.attr("mirror-vertical") == Some("true");
    if fh || fv {
        let (sx, sy) = (if fh { -1.0 } else { 1.0 }, if fv { -1.0 } else { 1.0 });
        let c = (w as f64 / 2.0, h as f64 / 2.0);
        p.apply_affine(Affine::translate(c) * Affine::scale_non_uniform(sx, sy) * Affine::translate((-c.0, -c.1)));
    }
    p
}

// --- custom shapes ------------------------------------------------------

/// The values an enhanced geometry's formulas read: its view box,
/// modifiers and equations.
struct Geo<'a> {
    view: [f64; 4],
    size: (f64, f64),
    mods: Vec<f64>,
    eqs: HashMap<&'a str, (usize, &'a str)>,
    done: RefCell<Vec<Option<f64>>>,
    /// How deep formulas and the equations they name are being worked
    /// out, against loops and deep nesting.
    depth: Cell<usize>,
}

impl Geo<'_> {
    fn eq(&self, name: &str) -> f64 {
        let Some(&(i, f)) = self.eqs.get(name) else { return 0.0 };
        if let Some(Some(v)) = self.done.borrow().get(i) {
            return *v;
        }
        if self.depth.get() > 200 {
            return 0.0;
        }
        self.depth.set(self.depth.get() + 1);
        let v = Formula { s: f.as_bytes(), i: 0, geo: self }.sum();
        self.depth.set(self.depth.get() - 1);
        let v = if v.is_finite() { v } else { 0.0 };
        if let Some(slot) = self.done.borrow_mut().get_mut(i) {
            *slot = Some(v);
        }
        v
    }

    fn name(&self, n: &str) -> f64 {
        let [x, y, w, h] = self.view;
        match n {
            "left" => x,
            "top" => y,
            "right" => x + w,
            "bottom" => y + h,
            "width" => w,
            "height" => h,
            "xstretch" | "ystretch" => 0.0,
            "hasstroke" | "hasfill" => 1.0,
            "pi" => std::f64::consts::PI,
            // The shape's size in hundredths of a millimetre.
            "logwidth" => self.size.0 * 2540.0 / 72.0,
            "logheight" => self.size.1 * 2540.0 / 72.0,
            _ => 0.0,
        }
    }

    /// A value in a path: a number, `?equation` or `$modifier`.
    fn value(&self, t: &str) -> f64 {
        if let Some(n) = t.strip_prefix('?') {
            self.eq(n)
        } else if let Some(m) = t.strip_prefix('$') {
            m.parse::<usize>().ok().and_then(|i| self.mods.get(i)).copied().unwrap_or(0.0)
        } else {
            t.parse().ok().filter(|v: &f64| v.is_finite()).unwrap_or_else(|| self.name(t))
        }
    }
}

/// A formula read left to right.
struct Formula<'a, 'g> {
    s: &'a [u8],
    i: usize,
    geo: &'a Geo<'g>,
}

impl Formula<'_, '_> {
    fn ws(&mut self) {
        while self.s.get(self.i).is_some_and(|c| c.is_ascii_whitespace()) {
            self.i += 1;
        }
    }

    fn eat(&mut self, c: u8) -> bool {
        self.ws();
        if self.s.get(self.i) == Some(&c) {
            self.i += 1;
            true
        } else {
            false
        }
    }

    fn sum(&mut self) -> f64 {
        let mut v = self.product();
        loop {
            if self.eat(b'+') {
                v += self.product();
            } else if self.eat(b'-') {
                v -= self.product();
            } else {
                return v;
            }
        }
    }

    fn product(&mut self) -> f64 {
        let mut v = self.unary();
        loop {
            if self.eat(b'*') {
                v *= self.unary();
            } else if self.eat(b'/') {
                let d = self.unary();
                v = if d == 0.0 { 0.0 } else { v / d };
            } else {
                return v;
            }
        }
    }

    fn unary(&mut self) -> f64 {
        let d = &self.geo.depth;
        if d.get() > 200 {
            self.i = self.s.len();
            return 0.0;
        }
        d.set(d.get() + 1);
        let v = if self.eat(b'-') {
            -self.unary()
        } else if self.eat(b'+') {
            self.unary()
        } else {
            self.atom()
        };
        d.set(d.get() - 1);
        v
    }

    fn args(&mut self) -> Vec<f64> {
        let mut out = Vec::new();
        if !self.eat(b'(') {
            return out;
        }
        if self.eat(b')') {
            return out;
        }
        while out.len() < 8 {
            out.push(self.sum());
            if !self.eat(b',') {
                break;
            }
        }
        self.eat(b')');
        out
    }

    fn atom(&mut self) -> f64 {
        self.ws();
        if self.i >= self.s.len() {
            return 0.0;
        }
        if self.eat(b'(') {
            let v = self.sum();
            self.eat(b')');
            return v;
        }
        let start = self.i;
        let c = self.s[self.i];
        if c == b'?' || c == b'$' {
            self.i += 1;
        }
        while self.s.get(self.i).is_some_and(|c| c.is_ascii_alphanumeric() || *c == b'.' || *c == b'_') {
            self.i += 1;
        }
        if self.i == start {
            // Something not understood: the rest is left.
            self.i = self.s.len();
            return 0.0;
        }
        let tok = std::str::from_utf8(&self.s[start..self.i]).unwrap_or("");
        let f1 = |f: fn(f64) -> f64, a: &[f64]| a.first().map_or(0.0, |v| f(*v));
        match tok {
            "abs" | "sqrt" | "sin" | "cos" | "tan" | "atan" | "atan2" | "min" | "max" | "if" => {
                let a = self.args();
                match tok {
                    "abs" => f1(f64::abs, &a),
                    "sqrt" => f1(|v| v.max(0.0).sqrt(), &a),
                    "sin" => f1(f64::sin, &a),
                    "cos" => f1(f64::cos, &a),
                    "tan" => f1(f64::tan, &a),
                    "atan" => f1(f64::atan, &a),
                    "atan2" => match a.as_slice() {
                        [y, x, ..] => y.atan2(*x),
                        _ => 0.0,
                    },
                    "min" => a.iter().copied().reduce(f64::min).unwrap_or(0.0),
                    "max" => a.iter().copied().reduce(f64::max).unwrap_or(0.0),
                    _ => match a.as_slice() {
                        [c, t, e, ..] => {
                            if *c > 0.0 {
                                *t
                            } else {
                                *e
                            }
                        }
                        _ => 0.0,
                    },
                }
            }
            t => self.geo.value(t),
        }
    }
}

/// An enhanced geometry's paths in a `w` × `h` box: what is filled and
/// what is stroked.
fn enhanced(g: &El, w: f32, h: f32) -> (BezPath, BezPath) {
    let Some(path) = g.attr("enhanced-path").filter(|p| !p.trim().is_empty()) else {
        let p = preset(preset_name(g.attr("type").unwrap_or("rectangle")), w, h);
        return (p.clone(), p);
    };
    let v: Vec<f64> = g.attr("viewBox").unwrap_or("0 0 21600 21600").split_whitespace().filter_map(|n| n.parse().ok()).collect();
    let mut view = match v.as_slice() {
        [a, b, c, d] if [a, b, c, d].iter().all(|v| v.is_finite()) => [*a, *b, *c, *d],
        _ => [0.0, 0.0, 21600.0, 21600.0],
    };
    // An empty view box: the shape's own size in hundredths of a
    // millimetre.
    if view[2] <= 0.0 {
        view[2] = w as f64 * 2540.0 / 72.0;
    }
    if view[3] <= 0.0 {
        view[3] = h as f64 * 2540.0 / 72.0;
    }
    let mods = g.attr("modifiers").unwrap_or("").split_whitespace().take(64).map(|m| m.parse().unwrap_or(0.0)).collect();
    let mut eqs = HashMap::new();
    for (i, q) in g.children("equation").take(2_000).enumerate() {
        if let (Some(n), Some(f)) = (q.attr("name"), q.attr("formula")) {
            eqs.insert(n, (i, f));
        }
    }
    let geo = Geo { view, size: (w as f64, h as f64), mods, done: RefCell::new(vec![None; 2_000]), eqs, depth: Cell::new(0) };
    let kx = if view[2].abs() > 1e-9 { w as f64 / view[2] } else { 0.0 };
    let ky = if view[3].abs() > 1e-9 { h as f64 / view[3] } else { 0.0 };
    let pt = |x: f64, y: f64| Point::new((x - view[0]) * kx, (y - view[1]) * ky);
    let (mut fill, mut stroke) = (BezPath::new(), BezPath::new());
    let mut sub = BezPath::new();
    let (mut no_fill, mut no_stroke) = (false, false);
    let end_sub = |sub: &mut BezPath, nf: &mut bool, ns: &mut bool, fill: &mut BezPath, stroke: &mut BezPath| {
        for el in sub.elements() {
            if !*nf {
                fill.push(*el);
            }
            if !*ns {
                stroke.push(*el);
            }
        }
        *sub = BezPath::new();
        *nf = false;
        *ns = false;
    };
    let toks: Vec<&str> = path.split(|c: char| c.is_whitespace() || c == ',').filter(|t| !t.is_empty()).take(50_000).collect();
    let is_cmd = |t: &str| t.len() == 1 && t.as_bytes()[0].is_ascii_uppercase();
    let mut cur = Point::ZERO;
    let mut start = Point::ZERO;
    let mut cmd = 'M';
    let mut quadrant_x = true;
    let mut i = 0;
    let mut guard = 0;
    while i < toks.len() {
        guard += 1;
        if guard > 100_000 {
            break;
        }
        if is_cmd(toks[i]) {
            cmd = toks[i].chars().next().unwrap_or('N');
            i += 1;
            quadrant_x = cmd == 'X';
            match cmd {
                'Z' => {
                    sub.close_path();
                    cur = start;
                    continue;
                }
                'N' => {
                    end_sub(&mut sub, &mut no_fill, &mut no_stroke, &mut fill, &mut stroke);
                    continue;
                }
                'F' => {
                    no_fill = true;
                    continue;
                }
                'S' => {
                    no_stroke = true;
                    continue;
                }
                _ => {}
            }
        }
        let arity = match cmd {
            'M' | 'L' | 'X' | 'Y' => 2,
            'C' | 'T' | 'U' => 6,
            'Q' | 'G' => 4,
            'A' | 'B' | 'W' | 'V' => 8,
            _ => {
                i += 1;
                continue;
            }
        };
        if i + arity > toks.len() || toks[i..i + arity].iter().any(|t| is_cmd(t)) {
            // Values missing: on to the next command.
            while i < toks.len() && !is_cmd(toks[i]) {
                i += 1;
            }
            continue;
        }
        let a: Vec<f64> = toks[i..i + arity].iter().map(|t| geo.value(t)).collect();
        i += arity;
        match cmd {
            'M' => {
                cur = pt(a[0], a[1]);
                start = cur;
                sub.move_to(cur);
                cmd = 'L';
            }
            'L' => {
                cur = pt(a[0], a[1]);
                line_to(&mut sub, cur);
            }
            'C' => {
                let (p1, p2, p3) = (pt(a[0], a[1]), pt(a[2], a[3]), pt(a[4], a[5]));
                ensure_start(&mut sub, cur);
                sub.curve_to(p1, p2, p3);
                cur = p3;
            }
            'Q' => {
                let (p1, p2) = (pt(a[0], a[1]), pt(a[2], a[3]));
                ensure_start(&mut sub, cur);
                sub.quad_to(p1, p2);
                cur = p2;
            }
            'X' | 'Y' => {
                let end = pt(a[0], a[1]);
                let k = 0.552_284_75;
                let (dx, dy) = (end.x - cur.x, end.y - cur.y);
                ensure_start(&mut sub, cur);
                if quadrant_x {
                    sub.curve_to(Point::new(cur.x + k * dx, cur.y), Point::new(end.x, end.y - k * dy), end);
                } else {
                    sub.curve_to(Point::new(cur.x, cur.y + k * dy), Point::new(end.x - k * dx, end.y), end);
                }
                cur = end;
                quadrant_x = !quadrant_x;
            }
            'T' | 'U' => {
                // Centre, size, then start and end angles in degrees,
                // anticlockwise.
                let c = pt(a[0], a[1]);
                let (rx, ry) = (a[2] * kx / 2.0, a[3] * ky / 2.0);
                let (t0, mut t1) = (a[4], a[5]);
                if !(t0.is_finite() && t1.is_finite()) {
                    continue;
                }
                if t1 <= t0 {
                    t1 += 360.0;
                }
                let at = |t: f64| Point::new(c.x + rx * t.to_radians().cos(), c.y - ry * t.to_radians().sin());
                let first = at(t0);
                if cmd == 'U' || sub.elements().is_empty() {
                    sub.move_to(first);
                    start = first;
                } else {
                    sub.line_to(first);
                }
                let steps = (((t1 - t0) / 10.0).ceil() as usize).clamp(1, 72);
                for s in 1..=steps {
                    cur = at(t0 + (t1 - t0) * s as f64 / steps as f64);
                    sub.line_to(cur);
                }
            }
            'A' | 'B' | 'W' | 'V' => {
                // A box, then the points the arc runs from and to.
                let (p1, p2, p3, p4) = (pt(a[0], a[1]), pt(a[2], a[3]), pt(a[4], a[5]), pt(a[6], a[7]));
                let c = Point::new((p1.x + p2.x) / 2.0, (p1.y + p2.y) / 2.0);
                let (rx, ry) = ((p2.x - p1.x).abs() / 2.0, (p2.y - p1.y).abs() / 2.0);
                if !(rx > 1e-9 && ry > 1e-9) {
                    continue;
                }
                let ang = |p: Point| ((p.y - c.y) / ry).atan2((p.x - c.x) / rx);
                let (s0, mut s1) = (ang(p3), ang(p4));
                let clockwise = matches!(cmd, 'W' | 'V');
                if clockwise && s1 <= s0 {
                    s1 += std::f64::consts::TAU;
                }
                if !clockwise && s1 >= s0 {
                    s1 -= std::f64::consts::TAU;
                }
                let at = |t: f64| Point::new(c.x + rx * t.cos(), c.y + ry * t.sin());
                let first = at(s0);
                if matches!(cmd, 'B' | 'V') || sub.elements().is_empty() {
                    sub.move_to(first);
                    start = first;
                } else {
                    sub.line_to(first);
                }
                let steps = (((s1 - s0).abs().to_degrees() / 10.0).ceil() as usize).clamp(1, 72);
                for s in 1..=steps {
                    cur = at(s0 + (s1 - s0) * s as f64 / steps as f64);
                    sub.line_to(cur);
                }
            }
            'G' => {
                // Radii, start angle and sweep in degrees, from the
                // current point, clockwise.
                let (rx, ry) = (a[0] * kx, a[1] * ky);
                let (st, sw) = (a[2], a[3]);
                if !(st.is_finite() && sw.is_finite()) {
                    continue;
                }
                let (s, c) = st.to_radians().sin_cos();
                let centre = Point::new(cur.x - rx * c, cur.y - ry * s);
                ensure_start(&mut sub, cur);
                let steps = ((sw.abs() / 10.0).ceil() as usize).clamp(1, 72);
                for k in 1..=steps {
                    let t = (st + sw * k as f64 / steps as f64).to_radians();
                    cur = Point::new(centre.x + rx * t.cos(), centre.y + ry * t.sin());
                    sub.line_to(cur);
                }
            }
            _ => {}
        }
    }
    end_sub(&mut sub, &mut no_fill, &mut no_stroke, &mut fill, &mut stroke);
    let ok = |p: &BezPath| p.elements().iter().all(|e| e.end_point().is_none_or(|q| q.x.is_finite() && q.y.is_finite() && q.x.abs() < 1e6 && q.y.abs() < 1e6));
    if !ok(&fill) || !ok(&stroke) {
        return (BezPath::new(), BezPath::new());
    }
    (fill, stroke)
}

fn ensure_start(p: &mut BezPath, cur: Point) {
    if p.elements().is_empty() {
        p.move_to(cur);
    }
}

fn line_to(p: &mut BezPath, to: Point) {
    if p.elements().is_empty() {
        p.move_to(to);
    } else {
        p.line_to(to);
    }
}

/// The preset outline closest to a custom shape type.
fn preset_name(t: &str) -> &str {
    if let Some(o) = t.strip_prefix("ooxml-") {
        return o;
    }
    match t {
        "ellipse" | "circle" | "flowchart-connector" => "ellipse",
        "round-rectangle" | "round-square" | "flowchart-alternate-process" => "roundRect",
        "diamond" | "flowchart-decision" => "diamond",
        "isosceles-triangle" | "flowchart-extract" => "triangle",
        "right-triangle" => "rtTriangle",
        "parallelogram" | "flowchart-data" => "parallelogram",
        "trapezoid" => "trapezoid",
        "pentagon" => "pentagon",
        "hexagon" => "hexagon",
        "octagon" => "octagon",
        "cross" => "plus",
        "right-arrow" => "rightArrow",
        "left-arrow" => "leftArrow",
        "up-arrow" => "upArrow",
        "down-arrow" => "downArrow",
        "chevron" => "chevron",
        "pentagon-right" => "homePlate",
        _ => "rect",
    }
}

// --- text ---------------------------------------------------------------

impl Slide<'_> {
    /// The text of shape `e` (its paragraphs are in `body`) set in its
    /// `w` × `h` box placed by `a`.
    #[allow(clippy::too_many_arguments)]
    fn text(&mut self, e: &El, body: &El, gp: &Props, family: &str, w: f32, h: f32, a: Affine) {
        if !body.elements().any(|k| matches!(k.name.as_str(), "p" | "h" | "list")) || turned(a) {
            // Text set sideways is left out rather than drawn wrong.
            return;
        }
        let name = e.attr("style-name");
        let mut base = Style { family: Arc::from("Liberation Sans"), size: 18.0, ..Style::default() };
        let d = self.st.props("graphic", None, "text-properties");
        base = self.st.text_style(&d, &base);
        let tp = self.st.props(family, name, "text-properties");
        base = self.st.text_style(&tp, &base);
        let mut pp = (*self.st.props(family, name, "paragraph-properties")).clone();
        if let Some(ts) = e.attr("text-style-name") {
            let p = self.st.props("paragraph", Some(ts), "paragraph-properties");
            pp.attrs.extend(p.attrs.iter().map(|(k, v)| (k.clone(), v.clone())));
        }
        let list = gp.kids.get("list-style").cloned();
        let outline = name.filter(|_| family == "presentation").and_then(|n| self.outline_prefix(n));
        let ctx = TextCtx { base, pp, list, outline };
        let mut paras = Vec::new();
        let mut counters = [0i32; 10];
        self.blocks(body, &ctx, None, 0, &mut counters, &mut paras, 0);
        let pad = |k: &str| gp.len(k).or_else(|| gp.len("padding")).unwrap_or(0.0).max(0.0);
        let (pl, pt, pr, pb) = (pad("padding-left"), pad("padding-top"), pad("padding-right"), pad("padding-bottom"));
        let (x, y) = upright(a, w, h);
        let nowrap = gp.get("auto-grow-width") == Some("true");
        let inner = (w - pl - pr).max(1.0);
        let width = if nowrap { 100_000.0 } else { inner };
        let room = h - pt - pb;
        let shrink = matches!(gp.get("fit-to-size"), Some("shrink-to-fit" | "true")) || gp.get("shrink-to-fit") == Some("true");
        let mut scale = 1.0;
        let mut set = self.set(&paras, width, scale);
        let mut total = height(&set);
        for _ in 0..3 {
            if !(shrink && total > room && room > 1.0) {
                break;
            }
            scale *= (room / total).sqrt().clamp(0.3, 0.98);
            set = self.set(&paras, width, scale);
            total = height(&set);
        }
        let mut cy = y
            + pt
            + match gp.get("textarea-vertical-align") {
                Some("middle") => (room - total) / 2.0,
                Some("bottom") => room - total,
                _ => 0.0,
            };
        let widest = set.iter().flat_map(|(_, l, _)| l.lines.iter().map(|l| l.width)).fold(0.0, f32::max);
        let tx = if nowrap && gp.get("textarea-horizontal-align") == Some("center") { x + pl + (inner - widest) / 2.0 } else { x + pl };
        for (before, lines, after) in &set {
            cy += before;
            for i in 0..lines.lines.len() {
                lines.draw_line(i, tx, cy, &mut self.items, &mut self.text);
                cy += lines.lines[i].height;
            }
            cy += after;
        }
    }

    /// The name before "1" of the outline style `name` comes from.
    fn outline_prefix(&self, name: &str) -> Option<String> {
        let mut cur = Some(name.to_string());
        for _ in 0..16 {
            let n = cur?;
            if let Some(p) = n.strip_suffix("outline1") {
                return Some(format!("{p}outline"));
            }
            cur = self.st.style("presentation", &n).and_then(|s| s.attr("parent-style-name")).map(str::to_string);
        }
        None
    }

    fn set(&mut self, paras: &[Para], width: f32, scale: f32) -> Vec<(f32, Lines, f32)> {
        let mut out = Vec::with_capacity(paras.len());
        for p in paras {
            let lines = if scale < 1.0 {
                let spans: Vec<Span> = p.spans.iter().map(|s| Span { style: Arc::new(Style { size: (s.style.size * scale).max(1.0), ..(*s.style).clone() }), ..s.clone() }).collect();
                typeset::set(&spans, &p.fmt, width, self.fonts)
            } else {
                typeset::set(&p.spans, &p.fmt, width, self.fonts)
            };
            out.push((p.before * scale, lines, p.after * scale));
        }
        out
    }

    /// Paragraphs and lists of `parent`; `list` is the list style and
    /// level they are in, and whether the first paragraph takes a label.
    #[allow(clippy::too_many_arguments)]
    fn blocks(&mut self, parent: &El, ctx: &TextCtx, list: Option<(&El, usize, bool)>, level: usize, counters: &mut [i32; 10], out: &mut Vec<Para>, depth: usize) {
        if depth > 16 {
            return;
        }
        let mut first = list.is_some_and(|l| l.2);
        for e in parent.elements() {
            if out.len() >= MAX_PARAS {
                return;
            }
            match e.name.as_str() {
                "p" | "h" => {
                    let p = self.para(e, ctx, list.map(|l| (l.0, l.1)), first, counters);
                    out.push(p);
                    first = false;
                }
                "list" => {
                    let own = e.attr("style-name").and_then(|n| self.st.lists.get(n)).cloned();
                    let ls = own.or_else(|| list.map(|l| l.0.clone())).or_else(|| ctx.list.clone());
                    let lvl = if list.is_some() { (level + 1).min(9) } else { level };
                    for item in e.elements() {
                        let labelled = item.name == "list-item";
                        if labelled {
                            counters[lvl] += 1;
                            for k in counters.iter_mut().skip(lvl + 1) {
                                *k = 0;
                            }
                        }
                        match &ls {
                            Some(ls) => self.blocks(item, ctx, Some((ls, lvl, labelled)), lvl, counters, out, depth + 1),
                            None => self.blocks(item, ctx, None, lvl, counters, out, depth + 1),
                        }
                    }
                    first = false;
                }
                _ => {}
            }
        }
    }

    fn para(&mut self, e: &El, ctx: &TextCtx, list: Option<(&El, usize)>, labelled: bool, counters: &[i32; 10]) -> Para {
        let level = list.map_or(0, |l| l.1);
        // Outline levels below the first take their own style.
        let mut base = ctx.base.clone();
        let mut pp = ctx.pp.clone();
        if let Some(prefix) = &ctx.outline
            && level > 0
        {
            let n = format!("{prefix}{}", level + 1);
            if self.st.style("presentation", &n).is_some() {
                let tp = self.st.props("presentation", Some(&n), "text-properties");
                base = self.st.text_style(&tp, &base);
                let p = self.st.props("presentation", Some(&n), "paragraph-properties");
                pp.attrs.extend(p.attrs.iter().map(|(k, v)| (k.clone(), v.clone())));
            }
        }
        let name = e.attr("style-name");
        if let Some(n) = name {
            let p = self.st.props("paragraph", Some(n), "paragraph-properties");
            pp.attrs.extend(p.attrs.iter().map(|(k, v)| (k.clone(), v.clone())));
            for (k, v) in &p.kids {
                pp.kids.insert(k.clone(), v.clone());
            }
        }
        let tp = self.st.props("paragraph", name, "text-properties");
        let style = Arc::new(self.st.text_style(&tp, &base));
        let mut fmt = para_format(&pp, &style);
        let mut spans = Vec::new();
        if let Some((ls, n)) = list
            && let Some(lvl) = level_style(ls, n)
        {
            if fmt.left == 0.0 && fmt.first == 0.0 {
                let (left, first) = list_indent(lvl);
                fmt.left = left;
                fmt.first = first;
            }
            if labelled && let Some(text) = label(lvl, ls, n, counters) {
                let mut ls = (*style).clone();
                if let Some(t) = lvl.child("text-properties") {
                    if let Some(c) = t.attr("color").and_then(color) {
                        ls.color = c;
                    }
                    if let Some(pc) = t.attr("font-size").and_then(percent) {
                        ls.size = (style.size * pc).clamp(1.0, 1000.0);
                    }
                }
                spans.push(Span::new(format!("{text}\t"), Arc::new(ls)));
            }
        }
        if fmt.first < 0.0 && !fmt.tabs.iter().any(|t| t.pos == fmt.left) {
            fmt.tabs.push(Tab { pos: fmt.left, align: TabAlign::Left, leader: None });
            crate::typeset::sort_tabs(&mut fmt.tabs);
        }
        let mut after_space = true;
        self.inline(e, &style, &mut spans, &mut after_space, 0);
        if let Some(last) = spans.last_mut() {
            let t = last.text.trim_end_matches(' ').len();
            last.text.truncate(t);
        }
        if spans.is_empty() {
            spans.push(Span::new("", style.clone()));
        }
        Para { fmt, spans, before: pp.len("margin-top").unwrap_or(0.0).max(0.0), after: pp.len("margin-bottom").unwrap_or(0.0).max(0.0) }
    }

    fn inline(&mut self, parent: &El, style: &Arc<Style>, spans: &mut Vec<Span>, after_space: &mut bool, depth: usize) {
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
                Node::El(e) => {
                    let field = match e.name.as_str() {
                        "span" | "a" | "meta" => {
                            let st = match e.attr("style-name") {
                                Some(n) => {
                                    let p = self.st.props("text", Some(n), "text-properties");
                                    Arc::new(self.st.text_style(&p, style))
                                }
                                None => style.clone(),
                            };
                            self.inline(e, &st, spans, after_space, depth + 1);
                            continue;
                        }
                        "s" => Some(" ".repeat(e.attr("c").and_then(|v| v.parse::<usize>().ok()).unwrap_or(1).clamp(1, 1000))),
                        "tab" => Some("\t".to_string()),
                        "line-break" => Some("\n".to_string()),
                        "page-number" => Some(self.fields.number.to_string()),
                        "page-count" => Some(self.fields.count.to_string()),
                        "footer" => self.fields.footer.clone(),
                        "header" => self.fields.header.clone(),
                        "date-time" => self.fields.date.clone(),
                        "annotation" | "annotation-end" | "bookmark" | "bookmark-start" | "bookmark-end" | "change" | "change-start" | "change-end" => None,
                        _ => {
                            self.inline(e, style, spans, after_space, depth + 1);
                            continue;
                        }
                    };
                    if let Some(t) = field.filter(|t| !t.is_empty()) {
                        *after_space = t.ends_with([' ', '\t', '\n']);
                        push(spans, t, style);
                    }
                }
            }
        }
    }
}

/// The label of a list item at `level` of list style `ls`.
fn label(lvl: &El, ls: &El, level: usize, counters: &[i32; 10]) -> Option<String> {
    match lvl.name.as_str() {
        "list-level-style-bullet" => {
            let c = lvl.attr("bullet-char")?.chars().next()?;
            Some(bullet(c).to_string())
        }
        "list-level-style-image" => Some("•".into()),
        "list-level-style-number" => {
            let fmt = lvl.attr("num-format").unwrap_or("1");
            let shown = lvl.attr("display-levels").and_then(|v| v.parse::<usize>().ok()).unwrap_or(1).clamp(1, level + 1);
            let start = lvl.attr("start-value").and_then(|v| v.parse::<i32>().ok()).unwrap_or(1).clamp(-1_000_000, 1_000_000) - 1;
            let mut parts = Vec::new();
            for k in level + 1 - shown..=level {
                let f = if k == level { fmt.to_string() } else { level_style(ls, k).and_then(|l| l.attr("num-format")).unwrap_or("1").to_string() };
                parts.push(number((counters[k] + start).max(1), &f));
            }
            let body = parts.join(".");
            if body.is_empty() {
                return None;
            }
            Some(format!("{}{}{}", lvl.attr("num-prefix").unwrap_or(""), body, lvl.attr("num-suffix").unwrap_or("")))
        }
        _ => None,
    }
}

fn height(set: &[(f32, Lines, f32)]) -> f32 {
    set.iter().map(|(b, l, a)| b + l.height() + a).sum()
}

// --- tables -------------------------------------------------------------

impl Slide<'_> {
    fn table(&mut self, t: &El, x: f32, y: f32, w: f32) {
        // Columns, with their repeats.
        let mut cols: Vec<Option<f32>> = Vec::new();
        for c in t.children("table-column") {
            let n = c.attr("number-columns-repeated").and_then(|v| v.parse::<usize>().ok()).unwrap_or(1).clamp(1, 200);
            let width = self.st.props("table-column", c.attr("style-name"), "table-column-properties").len("column-width");
            for _ in 0..n.min(200usize.saturating_sub(cols.len())) {
                cols.push(width);
            }
        }
        let mut rows: Vec<&El> = Vec::new();
        for e in t.elements() {
            match e.name.as_str() {
                "table-row" => rows.push(e),
                "table-header-rows" | "table-rows" | "table-row-group" => rows.extend(e.children("table-row")),
                _ => {}
            }
        }
        rows.truncate(500);
        let cells_in = |r: &El| r.elements().filter(|c| c.name.ends_with("table-cell")).count();
        let n_cols = cols.len().max(rows.iter().map(|r| cells_in(r)).max().unwrap_or(0)).clamp(1, 200);
        cols.resize(n_cols, None);
        let known: f32 = cols.iter().flatten().sum();
        let unknown = cols.iter().filter(|c| c.is_none()).count();
        let each = if unknown > 0 { ((w - known) / unknown as f32).max(10.0) } else { 0.0 };
        let mut col_x = vec![x];
        for c in &cols {
            col_x.push(col_x.last().copied().unwrap_or(x) + c.unwrap_or(each).clamp(0.0, 1e4));
        }
        // The template's cell styles, by where the cell is.
        let template = t.attr("template-name").and_then(|n| self.st.drawn.get(&("table-template".to_string(), n.to_string()))).cloned();
        let use_flag = |k: &str| t.attr(k) == Some("true");
        let n_rows = rows.len();
        let mut row_y = y;
        for (ri, tr) in rows.iter().enumerate() {
            let rp = self.st.props("table-row", tr.attr("style-name"), "table-row-properties");
            let min_h = rp.len("row-height").or_else(|| rp.len("min-row-height")).unwrap_or(0.0).max(0.0);
            let mut cells = Vec::new();
            let mut col = 0usize;
            for tc in tr.elements().filter(|c| c.name.ends_with("table-cell")).take(200) {
                let covered = tc.name == "covered-table-cell";
                let span = tc.attr("number-columns-spanned").and_then(|v| v.parse::<usize>().ok()).unwrap_or(1).clamp(1, 200);
                let at = col.min(n_cols);
                col += if covered { 1 } else { span };
                if covered || at >= n_cols {
                    continue;
                }
                let end = (at + span).min(n_cols);
                let style = tc.attr("style-name").map(str::to_string).or_else(|| {
                    let tm = template.as_ref()?;
                    let pick = |k: &str| tm.child(k).and_then(|e| e.attr("style-name")).map(str::to_string);
                    let when = |c: bool, k: &str| if c { pick(k) } else { None };
                    when(ri == 0 && use_flag("use-first-row-styles"), "first-row")
                        .or_else(|| when(ri + 1 == n_rows && use_flag("use-last-row-styles"), "last-row"))
                        .or_else(|| when(at == 0 && use_flag("use-first-column-styles"), "first-column"))
                        .or_else(|| when(end == n_cols && use_flag("use-last-column-styles"), "last-column"))
                        .or_else(|| when(use_flag("use-banding-rows-styles"), if ri % 2 == 1 { "even-rows" } else { "odd-rows" }))
                        .or_else(|| pick("body"))
                });
                let cw = col_x[end] - col_x[at];
                let cp = self.st.props("table-cell", style.as_deref(), "table-cell-properties");
                let gp = self.st.props("table-cell", style.as_deref(), "graphic-properties");
                let pad = |k: &str| cp.len(k).or_else(|| cp.len("padding")).or_else(|| gp.len(k)).or_else(|| gp.len("padding")).unwrap_or(3.6).max(0.0);
                let ins = [pad("padding-left"), pad("padding-top"), pad("padding-right"), pad("padding-bottom")];
                let tp = self.st.props("table-cell", style.as_deref(), "text-properties");
                let base = self.st.text_style(&tp, &Style { family: Arc::from("Liberation Sans"), size: 18.0, ..Style::default() });
                let pp = (*self.st.props("table-cell", style.as_deref(), "paragraph-properties")).clone();
                let ctx = TextCtx { base, pp, list: None, outline: None };
                let mut paras = Vec::new();
                let mut counters = [0i32; 10];
                self.blocks(tc, &ctx, None, 0, &mut counters, &mut paras, 1);
                let set = self.set(&paras, (cw - ins[0] - ins[2]).max(1.0), 1.0);
                cells.push((at, end, style, ins, set));
            }
            let need = cells.iter().map(|c| height(&c.4) + c.3[1] + c.3[3]).fold(min_h, f32::max);
            for (at, end, style, ins, set) in cells {
                let (cx, cw) = (col_x[at], col_x[end] - col_x[at]);
                let gp = self.st.props("table-cell", style.as_deref(), "graphic-properties");
                let cp = self.st.props("table-cell", style.as_deref(), "table-cell-properties");
                let fill = match self.fill(&gp) {
                    Fill::Color(c) => Some(c),
                    _ => cp.get("background-color").and_then(color),
                };
                if let Some(c) = fill {
                    self.items.push(Item::Rect { x: cx, y: row_y, w: cw, h: need, color: c });
                }
                let b = |k: &str| cp.get(k).or_else(|| cp.get("border")).and_then(border);
                for (k, x0, y0, x1, y1) in [("border-left", cx, row_y, cx, row_y + need), ("border-right", cx + cw, row_y, cx + cw, row_y + need), ("border-top", cx, row_y, cx + cw, row_y), ("border-bottom", cx, row_y + need, cx + cw, row_y + need)] {
                    if let Some((width, color)) = b(k) {
                        self.items.push(Item::Line { x0, y0, x1, y1, width, color });
                    }
                }
                let total = height(&set);
                let room = need - ins[1] - ins[3];
                let mut cy = row_y
                    + ins[1]
                    + match cp.get("vertical-align").or_else(|| gp.get("textarea-vertical-align")) {
                        Some("middle") => (room - total) / 2.0,
                        Some("bottom") => room - total,
                        _ => 0.0,
                    };
                for (before, lines, after) in &set {
                    cy += before;
                    for i in 0..lines.lines.len() {
                        lines.draw_line(i, cx + ins[0], cy, &mut self.items, &mut self.text);
                        cy += lines.lines[i].height;
                    }
                    cy += after;
                }
            }
            row_y += need;
            if row_y > 1e5 {
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transforms_rotate_then_move() {
        let a = transform("rotate (1.5707963) translate (72pt 36pt)").unwrap();
        let p = a * Point::new(10.0, 0.0);
        assert!((p.x - 72.0).abs() < 0.01 && (p.y - 26.0).abs() < 0.01, "{p:?}");
        assert!(transform("rotate (x)").is_none());
    }

    #[test]
    fn formulas_and_paths() {
        let l = Limits::default();
        let g = crate::ooxml::parse("<g viewBox='0 0 100 100' modifiers='25' enhanced-path='M 0 0 L ?f0 0 100 100 Z N'><equation name='f0' formula='$0 * 2 + min(10, width)'/></g>", &l, &[]).unwrap();
        let (fill, _) = enhanced(&g, 200.0, 100.0);
        assert!((fill.bounding_box().x1 - 200.0).abs() < 0.01, "{:?}", fill.bounding_box());
        // A formula naming itself ends.
        let g = crate::ooxml::parse("<g enhanced-path='M ?f0 0 L 1 1 N'><equation name='f0' formula='?f0 + 1'/></g>", &l, &[]).unwrap();
        let _ = enhanced(&g, 10.0, 10.0);
    }
}
