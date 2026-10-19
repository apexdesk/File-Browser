//! The cell formats of a workbook: fonts, fills, borders, number formats
//! and alignment, with theme and indexed colours resolved.

use std::collections::HashMap;
use std::sync::Arc;

use super::numfmt::{NumFormat, builtin_numfmt};
use crate::draw::Rgba;
use crate::ooxml::{El, Package};
use crate::typeset::Style;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum HAlign {
    General,
    Left,
    Center,
    Right,
    Fill,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum VAlign {
    Top,
    Center,
    Bottom,
}

/// A cell format.
#[derive(Clone)]
pub(crate) struct Xf {
    pub style: Arc<Style>,
    pub fill: Option<Rgba>,
    /// Top, right, bottom, left: width in points and colour.
    pub borders: [Option<(f32, Rgba)>; 4],
    pub num: Arc<NumFormat>,
    pub h: HAlign,
    pub v: VAlign,
    pub wrap: bool,
    pub indent: f32,
}

pub(crate) struct Styles {
    pub xfs: Vec<Xf>,
    default: Xf,
}

impl Styles {
    /// Formats `xfs`, others shown as `default`.
    pub fn new(xfs: Vec<Xf>, default: Xf) -> Styles {
        Styles { xfs, default }
    }

    pub fn xf(&self, i: usize) -> &Xf {
        self.xfs.get(i).unwrap_or(&self.default)
    }
}

impl Xf {
    /// The plain format: text in `style`, general numbers, no fill.
    pub fn plain(style: Arc<Style>) -> Xf {
        Xf { style, fill: None, borders: [None; 4], num: Arc::new(NumFormat::general()), h: HAlign::General, v: VAlign::Bottom, wrap: false, indent: 0.0 }
    }
}

/// The 64 colours of the legacy palette.
pub(crate) const INDEXED: [u32; 64] = [
    0x000000, 0xFFFFFF, 0xFF0000, 0x00FF00, 0x0000FF, 0xFFFF00, 0xFF00FF, 0x00FFFF, 0x000000, 0xFFFFFF, 0xFF0000, 0x00FF00, 0x0000FF, 0xFFFF00, 0xFF00FF, 0x00FFFF, 0x800000, 0x008000, 0x000080, 0x808000, 0x800080, 0x008080, 0xC0C0C0, 0x808080, 0x9999FF, 0x993366, 0xFFFFCC, 0xCCFFFF, 0x660066, 0xFF8080, 0x0066CC, 0xCCCCFF, 0x000080, 0xFF00FF, 0xFFFF00, 0x00FFFF, 0x800080, 0x800000, 0x008080, 0x0000FF, 0x00CCFF, 0xCCFFFF, 0xCCFFCC, 0xFFFF99, 0x99CCFF, 0xFF99CC, 0xCC99FF, 0xFFCC99, 0x3366FF, 0x33CCCC, 0x99CC00, 0xFFCC00, 0xFF9900, 0xFF6600, 0x666699, 0x969696, 0x003366, 0x339966, 0x003300, 0x333300, 0x993300, 0x993366, 0x333399, 0x333333,
];

/// Theme colours in the order cells index them: light 1, dark 1, light
/// 2, dark 2, six accents, hyperlink, followed hyperlink.
pub(crate) fn theme_colors(pkg: &Package, workbook: &str) -> Vec<u32> {
    let mut out = vec![0xFFFFFF, 0x000000, 0xE7E6E6, 0x44546A, 0x4472C4, 0xED7D31, 0xA5A5A5, 0xFFC000, 0x5B9BD5, 0x70AD47, 0x0563C1, 0x954F72];
    let Some(part) = pkg.rel_of(workbook, "theme") else { return out };
    let Ok(theme) = pkg.xml(&part, &[]) else { return out };
    let Some(scheme) = theme.find("clrScheme") else { return out };
    let names = ["lt1", "dk1", "lt2", "dk2", "accent1", "accent2", "accent3", "accent4", "accent5", "accent6", "hlink", "folHlink"];
    for (i, n) in names.iter().enumerate() {
        let Some(e) = scheme.child(n) else { continue };
        let rgb = e.child("srgbClr").and_then(|c| c.attr("val")).or_else(|| e.child("sysClr").and_then(|c| c.attr("lastClr")));
        if let Some(v) = rgb.and_then(|v| u32::from_str_radix(v, 16).ok()) {
            out[i] = v & 0xFF_FFFF;
        }
    }
    out
}

/// A `<color>` element's colour.
pub(crate) fn color(e: &El, theme: &[u32]) -> Option<Rgba> {
    if e.attr("auto").is_some_and(|v| v == "1" || v == "true") {
        return None;
    }
    let rgb = if let Some(v) = e.attr("rgb") {
        let v = u32::from_str_radix(v.trim(), 16).ok()?;
        // ARGB with no alpha is shown opaque.
        v & 0xFF_FFFF
    } else if let Some(t) = e.num("theme") {
        *theme.get(t as usize)?
    } else if let Some(i) = e.num("indexed") {
        match i as usize {
            64 => return None,
            i => *INDEXED.get(i)?,
        }
    } else {
        return None;
    };
    let tint = e.num("tint").unwrap_or(0.0).clamp(-1.0, 1.0);
    let t = |c: u32| -> u8 {
        let c = c as f32;
        (if tint < 0.0 { c * (1.0 + tint) } else { c + (255.0 - c) * tint }).round().clamp(0.0, 255.0) as u8
    };
    Some(Rgba([t((rgb >> 16) & 255), t((rgb >> 8) & 255), t(rgb & 255), 255]))
}

fn border_side(e: Option<&El>, theme: &[u32]) -> Option<(f32, Rgba)> {
    let e = e?;
    let w = match e.attr("style")? {
        "none" => return None,
        "medium" | "mediumDashed" | "mediumDashDot" | "mediumDashDotDot" | "slantDashDot" => 1.5,
        "thick" | "double" => 2.25,
        "hair" => 0.5,
        _ => 0.75,
    };
    Some((w, e.child("color").and_then(|c| color(c, theme)).unwrap_or(Rgba::BLACK)))
}

pub(crate) fn read(pkg: &Package, part: Option<&str>, theme: &[u32]) -> Styles {
    let default = Xf {
        style: Arc::new(Style { family: Arc::from("Calibri"), size: 11.0, ..Style::default() }),
        fill: None,
        borders: [None; 4],
        num: Arc::new(NumFormat::general()),
        h: HAlign::General,
        v: VAlign::Bottom,
        wrap: false,
        indent: 0.0,
    };
    let Some(root) = part.and_then(|p| pkg.xml(p, &[]).ok()) else { return Styles { xfs: Vec::new(), default } };
    let mut codes: HashMap<u32, Arc<NumFormat>> = HashMap::new();
    if let Some(nf) = root.child("numFmts") {
        for f in nf.children("numFmt").take(4096) {
            if let (Some(id), Some(code)) = (f.num("numFmtId"), f.attr("formatCode")) {
                codes.insert(id as u32, Arc::new(NumFormat::parse(code)));
            }
        }
    }
    let fonts: Vec<Arc<Style>> = root
        .child("fonts")
        .map(|f| {
            f.children("font")
                .take(65_536)
                .map(|f| {
                    Arc::new(Style {
                        family: Arc::from(f.val("name").unwrap_or("Calibri")),
                        size: f.child("sz").and_then(|s| s.num("val")).unwrap_or(11.0).clamp(1.0, 409.0),
                        bold: f.flag("b").unwrap_or(false),
                        italic: f.flag("i").unwrap_or(false),
                        underline: f.child("u").is_some_and(|u| u.attr("val") != Some("none")),
                        strike: f.flag("strike").unwrap_or(false),
                        color: f.child("color").and_then(|c| color(c, theme)).unwrap_or(Rgba::BLACK),
                        shift: match f.val("vertAlign") {
                            Some("superscript") => 1,
                            Some("subscript") => -1,
                            _ => 0,
                        },
                        ..Style::default()
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let fills: Vec<Option<Rgba>> = root
        .child("fills")
        .map(|f| {
            f.children("fill")
                .take(65_536)
                .map(|f| {
                    if let Some(p) = f.child("patternFill") {
                        match p.attr("patternType") {
                            None | Some("none") => None,
                            Some("solid") => p.child("fgColor").and_then(|c| color(c, theme)),
                            // Patterns shown as their foreground over white,
                            // lightened.
                            Some(_) => p.child("fgColor").and_then(|c| color(c, theme)).map(|Rgba([r, g, b, _])| {
                                let l = |c: u8| ((c as u16 + 255 * 2) / 3) as u8;
                                Rgba([l(r), l(g), l(b), 255])
                            }),
                        }
                    } else {
                        f.child("gradientFill").and_then(|g| g.find("color")).and_then(|c| color(c, theme))
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    let borders: Vec<[Option<(f32, Rgba)>; 4]> = root
        .child("borders")
        .map(|b| {
            b.children("border")
                .take(65_536)
                .map(|b| {
                    let side = |a: &str, alt: &str| border_side(b.child(a).or_else(|| b.child(alt)), theme);
                    [side("top", "top"), side("right", "end"), side("bottom", "bottom"), side("left", "start")]
                })
                .collect()
        })
        .unwrap_or_default();
    let mut xfs = Vec::new();
    if let Some(cx) = root.child("cellXfs") {
        for x in cx.children("xf").take(200_000) {
            let id = |k: &str| x.num(k).map_or(0, |v| v.max(0.0) as usize);
            let num_id = id("numFmtId") as u32;
            let num = codes.get(&num_id).cloned().unwrap_or_else(|| Arc::new(NumFormat::parse(builtin_numfmt(num_id).unwrap_or("General"))));
            let al = x.child("alignment");
            let h = match al.and_then(|a| a.attr("horizontal")) {
                Some("left") => HAlign::Left,
                Some("center" | "centerContinuous" | "distributed") => HAlign::Center,
                Some("right") => HAlign::Right,
                Some("fill") => HAlign::Fill,
                Some("justify") => HAlign::Left,
                _ => HAlign::General,
            };
            let v = match al.and_then(|a| a.attr("vertical")) {
                Some("top") => VAlign::Top,
                Some("center" | "distributed" | "justify") => VAlign::Center,
                _ => VAlign::Bottom,
            };
            xfs.push(Xf {
                style: fonts.get(id("fontId")).cloned().unwrap_or_else(|| default.style.clone()),
                fill: fills.get(id("fillId")).copied().flatten(),
                borders: borders.get(id("borderId")).copied().unwrap_or([None; 4]),
                num,
                h,
                v,
                wrap: al.and_then(|a| a.attr("wrapText")).is_some_and(|v| v == "1" || v == "true"),
                indent: al.and_then(|a| a.num("indent")).unwrap_or(0.0).clamp(0.0, 250.0) * 9.0,
            });
        }
    }
    let default = Xf { style: fonts.first().cloned().unwrap_or(default.style), ..default };
    Styles { xfs, default }
}
