//! Pages of office documents drawn: what a page shows as a list of
//! items in points from its top-left corner, painted into pixels.

use std::sync::Arc;

use vello_cpu::color::{AlphaColor, Srgb};
use vello_cpu::kurbo::{Affine, BezPath, Rect, Shape, Stroke};
use vello_cpu::peniko::{Extend, Fill, ImageQuality, ImageSampler};
use vello_cpu::{Image as VImage, ImageSource, PaintType, Pixmap, RenderContext, RenderSettings, Resources};

use crate::text::TextRun;
use crate::typeset::FontCache;
use crate::{Error, Image};

/// An RGBA colour.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Rgba(pub [u8; 4]);

impl Rgba {
    pub const BLACK: Rgba = Rgba([0, 0, 0, 255]);
    pub const WHITE: Rgba = Rgba([255, 255, 255, 255]);

    pub fn rgb(rgb: u32) -> Rgba {
        Rgba([(rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8, 255])
    }

    fn color(self) -> AlphaColor<Srgb> {
        let [r, g, b, a] = self.0;
        AlphaColor::new([r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0, a as f32 / 255.0])
    }
}

/// Glyphs of one face at one size on one baseline.
#[derive(Clone, Debug)]
pub(crate) struct GlyphRun {
    pub face: usize,
    pub size: f32,
    pub color: Rgba,
    /// Baseline origin of the run.
    pub x: f32,
    pub y: f32,
    /// Glyph ids and their offsets along the baseline.
    pub glyphs: Vec<(u32, f32)>,
    /// Slanted by hand (no italic face found).
    pub skew: bool,
    /// Drawn heavier by hand (no bold face found).
    pub embolden: bool,
}

/// What a page shows, in points.
#[derive(Clone, Debug)]
pub(crate) enum Item {
    Rect { x: f32, y: f32, w: f32, h: f32, color: Rgba },
    Line { x0: f32, y0: f32, x1: f32, y1: f32, width: f32, color: Rgba },
    Path { path: BezPath, fill: Option<Rgba>, stroke: Option<(f32, Rgba)> },
    Glyphs(GlyphRun),
    /// A picture, by its key in the document (decoded when drawn).
    Image { x: f32, y: f32, w: f32, h: f32, key: Arc<str> },
    /// Later items clipped to this box until the matching `Unclip`.
    Clip { x: f32, y: f32, w: f32, h: f32 },
    Unclip,
}

/// A page laid out: its size in points, what it shows and its text.
#[derive(Clone, Debug, Default)]
pub(crate) struct Page {
    pub width: f32,
    pub height: f32,
    pub background: Option<Rgba>,
    pub items: Vec<Item>,
    /// Text in points, turned into page fractions when asked for.
    pub text: Vec<TextRun>,
}

impl Page {
    pub fn runs(&self) -> Vec<TextRun> {
        let (w, h) = (self.width.max(1.0), self.height.max(1.0));
        self.text.iter().map(|r| TextRun { text: r.text.clone(), x: r.x / w, y: r.y / h, width: r.width / w, height: r.height / h }).collect()
    }
}

/// Draws `page` `width` pixels wide.
pub(crate) fn render(page: &Page, width: u32, fonts: &mut FontCache, pictures: &mut dyn FnMut(&str) -> Option<Arc<Pixmap>>, limits: &crate::Limits, cancel: &dyn Fn() -> bool) -> Result<Image, Error> {
    let scale = width as f64 / page.width.max(1.0) as f64;
    let height = ((page.height as f64 * scale).round() as u32).clamp(1, 16_000);
    let width = width.clamp(1, 16_000);
    if width as u64 * height as u64 > limits.max_render_pixels {
        return Err(Error::TooLarge);
    }
    let mut ctx = RenderContext::new_with(width as u16, height as u16, RenderSettings { num_threads: 0, ..Default::default() });
    let base = Affine::scale(scale);
    ctx.set_paint(page.background.unwrap_or(Rgba::WHITE).color());
    ctx.fill_rect(&Rect::new(0.0, 0.0, width as f64, height as f64));
    let mut clips = 0usize;
    for (i, item) in page.items.iter().enumerate() {
        if i % 512 == 511 && cancel() {
            return Err(Error::Cancelled);
        }
        ctx.set_transform(base);
        match item {
            Item::Rect { x, y, w, h, color } => {
                ctx.set_paint(color.color());
                ctx.fill_rect(&Rect::new(*x as f64, *y as f64, (*x + *w) as f64, (*y + *h) as f64));
            }
            Item::Line { x0, y0, x1, y1, width: lw, color } => {
                let mut p = BezPath::new();
                p.move_to((*x0 as f64, *y0 as f64));
                p.line_to((*x1 as f64, *y1 as f64));
                // At least a pixel wide, as a viewer shows hairlines.
                ctx.set_stroke(Stroke::new((*lw as f64).max(1.0 / scale)));
                ctx.set_paint(color.color());
                ctx.stroke_path(&p);
            }
            Item::Path { path, fill, stroke } => {
                let tidied;
                let path = if starts_closed(path) {
                    tidied = tidy(path.clone());
                    &tidied
                } else {
                    path
                };
                if let Some(c) = fill {
                    ctx.set_fill_rule(Fill::NonZero);
                    ctx.set_paint(c.color());
                    ctx.fill_path(path);
                }
                if let Some((lw, c)) = stroke {
                    ctx.set_stroke(Stroke::new((*lw as f64).max(1.0 / scale)));
                    ctx.set_paint(c.color());
                    ctx.stroke_path(path);
                }
            }
            Item::Glyphs(run) => {
                // Text smaller than a pixel is not worth its outlines.
                if run.size as f64 * scale < 1.0 {
                    continue;
                }
                ctx.set_paint(run.color.color());
                ctx.set_fill_rule(Fill::NonZero);
                for &(gid, dx) in &run.glyphs {
                    let Some(path) = fonts.outline(run.face, gid) else { continue };
                    let s = run.size as f64 / fonts.upem(run.face) as f64;
                    let skew = if run.skew { 0.2 } else { 0.0 };
                    let m = Affine::new([s, 0.0, skew * s, -s, (run.x + dx) as f64, run.y as f64]);
                    ctx.set_transform(base * m);
                    ctx.fill_path(&path);
                    if run.embolden {
                        ctx.set_stroke(Stroke::new(fonts.upem(run.face) as f64 / 40.0));
                        ctx.stroke_path(&path);
                    }
                }
            }
            Item::Image { x, y, w, h, key } => {
                let Some(image) = pictures(key) else { continue };
                let (iw, ih) = (image.width() as f64, image.height() as f64);
                if iw < 1.0 || ih < 1.0 || *w <= 0.0 || *h <= 0.0 {
                    continue;
                }
                ctx.set_transform(base * Affine::new([*w as f64 / iw, 0.0, 0.0, *h as f64 / ih, *x as f64, *y as f64]));
                let sampler = ImageSampler { x_extend: Extend::Pad, y_extend: Extend::Pad, quality: ImageQuality::Medium, alpha: 1.0 };
                ctx.set_paint(PaintType::Image(VImage { image: ImageSource::Pixmap(image.clone()), sampler }));
                ctx.fill_rect(&Rect::new(0.0, 0.0, iw, ih));
            }
            Item::Clip { x, y, w, h } => {
                if clips < 64 {
                    ctx.push_clip_path(&Rect::new(*x as f64, *y as f64, (*x + *w) as f64, (*y + *h) as f64).to_path(0.1));
                }
                clips += 1;
            }
            Item::Unclip => {
                if clips > 0 {
                    clips -= 1;
                    if clips < 64 {
                        ctx.pop_clip_path();
                    }
                }
            }
        }
    }
    for _ in 0..clips.min(64) {
        ctx.pop_clip_path();
    }
    let mut pix = Pixmap::new(width as u16, height as u16);
    ctx.flush();
    ctx.render(&mut pix, &mut Resources::new());
    // The background is opaque: the pixels are as they are.
    Ok(Image { width, height, rgba: pix.data_as_u8_slice().to_vec() })
}

/// RGBA pixels (not premultiplied) as a pixmap to paint.
pub(crate) fn pixmap(w: u32, h: u32, rgba: &[u8]) -> Option<Arc<Pixmap>> {
    use vello_cpu::color::PremulRgba8;
    if w == 0 || h == 0 || w > u16::MAX as u32 || h > u16::MAX as u32 || rgba.len() < w as usize * h as usize * 4 {
        return None;
    }
    let mut transparent = false;
    let px: Vec<PremulRgba8> = rgba
        .chunks_exact(4)
        .take(w as usize * h as usize)
        .map(|p| {
            let a = p[3] as u16;
            transparent |= a != 255;
            let m = |c: u8| ((c as u16 * a + 127) / 255) as u8;
            PremulRgba8 { r: m(p[0]), g: m(p[1]), b: m(p[2]), a: p[3] }
        })
        .collect();
    let mut pm = Pixmap::from_parts(px, w as u16, h as u16);
    pm.set_may_have_transparency(transparent);
    Some(Arc::new(pm))
}

fn starts_closed(p: &BezPath) -> bool {
    p.elements().first() == Some(&vello_cpu::kurbo::PathEl::ClosePath)
}

/// `p` without the closings that come before anything to close, which
/// paths cannot start with.
pub(crate) fn tidy(p: BezPath) -> BezPath {
    if !starts_closed(&p) {
        return p;
    }
    let mut out = BezPath::new();
    for el in p.elements().iter().skip_while(|e| **e == vello_cpu::kurbo::PathEl::ClosePath) {
        out.push(*el);
    }
    out
}
