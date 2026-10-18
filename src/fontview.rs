//! Muestra de una fuente (TTF/OTF/TTC/OTC) para la vista previa: nombre y texto de prueba
//! dibujados con la propia fuente, más sus datos.
#[allow(unused_imports)]
use crate::vfs::{KindV as _, PathV as _};
use crate::i18n::t;
use crate::thumbs::Decoded;
use skrifa::instance::{LocationRef, Size};
use skrifa::outline::{DrawSettings, OutlinePen};
use skrifa::raw::TableProvider;
use skrifa::string::StringId;
use skrifa::{FontRef, MetadataProvider};
use std::path::Path;

pub fn is_font_ext(e: &str) -> bool {
    matches!(e.to_ascii_lowercase().as_str(), "ttf" | "otf" | "ttc" | "otc")
}

struct Pen {
    pb: tiny_skia::PathBuilder,
    x: f32,
    y: f32,
}

impl OutlinePen for Pen {
    fn move_to(&mut self, x: f32, y: f32) {
        self.pb.move_to(self.x + x, self.y - y);
    }
    fn line_to(&mut self, x: f32, y: f32) {
        self.pb.line_to(self.x + x, self.y - y);
    }
    fn quad_to(&mut self, cx0: f32, cy0: f32, x: f32, y: f32) {
        self.pb.quad_to(self.x + cx0, self.y - cy0, self.x + x, self.y - y);
    }
    fn curve_to(&mut self, cx0: f32, cy0: f32, cx1: f32, cy1: f32, x: f32, y: f32) {
        self.pb.cubic_to(self.x + cx0, self.y - cy0, self.x + cx1, self.y - cy1, self.x + x, self.y - y);
    }
    fn close(&mut self) {
        self.pb.close();
    }
}

fn open(data: &[u8]) -> Option<FontRef<'_>> {
    FontRef::from_index(data, 0).ok()
}

/// Ancho de una línea de texto en píxeles al tamaño dado.
fn width(font: &FontRef, text: &str, px: f32) -> f32 {
    let cmap = font.charmap();
    let gm = font.glyph_metrics(Size::new(px), LocationRef::default());
    text.chars().filter_map(|c| cmap.map(c)).map(|g| gm.advance_width(g).unwrap_or(px * 0.5)).sum()
}

/// Dibuja `text` con la base en `y`; devuelve el ancho dibujado.
fn line(pix: &mut tiny_skia::Pixmap, font: &FontRef, text: &str, x: f32, y: f32, px: f32) -> f32 {
    let cmap = font.charmap();
    let gm = font.glyph_metrics(Size::new(px), LocationRef::default());
    let outlines = font.outline_glyphs();
    let mut pen = Pen { pb: tiny_skia::PathBuilder::new(), x, y };
    for c in text.chars() {
        let Some(g) = cmap.map(c) else {
            pen.x += px * 0.5;
            continue;
        };
        if let Some(o) = outlines.get(g) {
            let _ = o.draw(DrawSettings::unhinted(Size::new(px), LocationRef::default()), &mut pen);
        }
        pen.x += gm.advance_width(g).unwrap_or(px * 0.5);
    }
    let w = pen.x - x;
    if let Some(path) = pen.pb.finish() {
        let mut paint = tiny_skia::Paint::default();
        paint.set_color_rgba8(24, 24, 28, 255);
        paint.anti_alias = true;
        pix.fill_path(&path, &paint, tiny_skia::FillRule::Winding, tiny_skia::Transform::identity(), None);
    }
    w
}

/// Parte `text` en líneas que quepan en `max` píxeles.
fn wrap(font: &FontRef, text: &str, px: f32, max: f32) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for w in text.split(' ') {
        let cand = if cur.is_empty() { w.to_string() } else { format!("{cur} {w}") };
        if !cur.is_empty() && width(font, &cand, px) > max {
            out.push(std::mem::take(&mut cur));
            cur = w.to_string();
        } else {
            cur = cand;
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

pub fn specimen(path: &Path) -> Result<Decoded, String> {
    let fail = || t("No se pudo mostrar el documento").to_string();
    if crate::vfs::fs::metadata(path).map(|m| m.len() > 64 << 20).unwrap_or(true) {
        return Err(t("El documento es demasiado grande para mostrarlo").into());
    }
    let data = crate::vfs::fs::read(path).map_err(|_| fail())?;
    let font = open(&data).ok_or_else(fail)?;
    let name = font.localized_strings(StringId::FULL_NAME).english_or_first().map(|s| s.to_string()).unwrap_or_default();
    let w = 1100.0f32;
    let pad = 40.0;
    let inner = w - 2.0 * pad;
    let title = if name.is_empty() { path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default() } else { name };
    let blocks: [(&str, f32); 8] = [
        (&title, 60.0),
        ("ABCDEFGHIJKLMNOPQRSTUVWXYZ", 42.0),
        ("abcdefghijklmnopqrstuvwxyz", 42.0),
        ("0123456789  .,;:!?¿¡&@#%()[]{}+-=/*", 38.0),
        ("ÁÉÍÓÚ ÑÇ àèìòù äëïöü ß æ œ € £ ¥ © ®", 32.0),
        ("El veloz murciélago hindú comía feliz cardillo y kiwi. La cigüeña tocaba el saxofón detrás del palenque de paja.", 26.0),
        ("The quick brown fox jumps over the lazy dog. Pack my box with five dozen liquor jugs.", 20.0),
        ("Portez ce vieux whisky au juge blond qui fume. Falsches Üben von Xylophonmusik quält jeden größeren Zwerg.", 15.0),
    ];
    let mut lines: Vec<(String, f32)> = Vec::new();
    for (txt, px) in blocks {
        for l in wrap(&font, txt, px, inner).into_iter().take(if px > 30.0 { 2 } else { 4 }) {
            lines.push((l, px));
        }
    }
    let h: f32 = lines.iter().map(|(_, px)| px * 1.45).sum::<f32>() + 2.0 * pad;
    let mut pix = tiny_skia::Pixmap::new(w as u32, h.ceil() as u32).ok_or_else(fail)?;
    pix.fill(tiny_skia::Color::WHITE);
    let mut y = pad;
    for (txt, px) in &lines {
        y += px * 1.1;
        line(&mut pix, &font, txt, pad, y, *px);
        y += px * 0.35;
    }
    Ok(Decoded { w: pix.width() as usize, h: pix.height() as usize, rgba: pix.take() })
}

/// Datos de la fuente como pares (etiqueta, valor).
pub fn info(path: &Path) -> Vec<(String, String)> {
    let mut out = Vec::new();
    if crate::vfs::fs::metadata(path).map(|m| m.len() > 64 << 20).unwrap_or(true) {
        return out;
    }
    let Ok(data) = crate::vfs::fs::read(path) else { return out };
    let Some(font) = open(&data) else { return out };
    let s = |id: StringId| font.localized_strings(id).english_or_first().map(|x| x.to_string()).filter(|x| !x.is_empty());
    for (label, id) in [
        (t("Familia"), StringId::FAMILY_NAME),
        (t("Estilo"), StringId::SUBFAMILY_NAME),
        (t("Versión"), StringId::VERSION_STRING),
        (t("Diseñador"), StringId::DESIGNER),
        (t("Licencia"), StringId::LICENSE_DESCRIPTION),
        ("Copyright", StringId::COPYRIGHT_NOTICE),
    ] {
        if let Some(v) = s(id) {
            out.push((label.to_string(), v.chars().take(160).collect()));
        }
    }
    if let Ok(m) = font.maxp() {
        out.push((t("Glifos").into(), m.num_glyphs().to_string()));
    }
    let axes = font.axes().len();
    if axes > 0 {
        out.push((t("Ejes variables").into(), axes.to_string()));
    }
    out
}
