//! Pictures inside documents decoded into RGBA pixels, within the limits:
//! JPEG, PNG, GIF, BMP and WebP.

use std::io::Cursor;

use crate::Limits;

/// Pixels kept of a decoded picture: larger ones are shrunk (a page is
/// never drawn that sharp).
const KEEP_PIXELS: u64 = 8_000_000;

/// The picture in `data`: its size and RGBA pixels (not premultiplied).
pub(crate) fn decode(data: &[u8], limits: &Limits) -> Option<(u32, u32, Vec<u8>)> {
    let (w, h, rgba) = if data.starts_with(&[0xFF, 0xD8]) { jpeg(data, limits)? } else { other(data, limits)? };
    Some(shrink(w, h, rgba))
}

fn jpeg(data: &[u8], limits: &Limits) -> Option<(u32, u32, Vec<u8>)> {
    use zune_jpeg::zune_core::colorspace::ColorSpace;
    use zune_jpeg::zune_core::options::DecoderOptions;
    let side = limits.max_image_pixels.min(u32::MAX as u64).isqrt().clamp(1, 65_535) as usize;
    let opts = DecoderOptions::default().jpeg_set_out_colorspace(ColorSpace::RGBA).set_max_width(side.max(16_384)).set_max_height(side.max(16_384));
    let mut dec = zune_jpeg::JpegDecoder::new_with_options(Cursor::new(data), opts);
    dec.decode_headers().ok()?;
    let info = dec.info()?;
    let (w, h) = (info.width as u32, info.height as u32);
    if w == 0 || h == 0 || w as u64 * h as u64 > limits.max_image_pixels {
        return None;
    }
    let px = dec.decode().ok()?;
    (px.len() >= w as usize * h as usize * 4).then_some((w, h, px))
}

fn other(data: &[u8], limits: &Limits) -> Option<(u32, u32, Vec<u8>)> {
    // Sized from the header first, as the decoder sets no bound of its own
    // but memory; through the entry point a program using `image` most
    // likely has already, so its decoders are not built in twice.
    let (w, h) = header_size(data)?;
    if w == 0 || h == 0 || w as u64 * h as u64 > limits.max_image_pixels {
        return None;
    }
    let img = image::load_from_memory(data).ok()?.into_rgba8();
    let (w, h) = img.dimensions();
    (w as u64 * h as u64 <= limits.max_image_pixels).then(|| (w, h, img.into_raw()))
}

/// Width and height a PNG, GIF, BMP or WebP header gives.
fn header_size(d: &[u8]) -> Option<(u32, u32)> {
    let u16le = |i: usize| d.get(i..i + 2).map(|b| u32::from(u16::from_le_bytes([b[0], b[1]])));
    let u24le = |i: usize| d.get(i..i + 3).map(|b| u32::from_le_bytes([b[0], b[1], b[2], 0]));
    let u32be = |i: usize| d.get(i..i + 4).map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]));
    let i32le = |i: usize| d.get(i..i + 4).map(|b| i32::from_le_bytes([b[0], b[1], b[2], b[3]]).unsigned_abs());
    if d.starts_with(b"\x89PNG") {
        Some((u32be(16)?, u32be(20)?))
    } else if d.starts_with(b"GIF8") {
        Some((u16le(6)?, u16le(8)?))
    } else if d.starts_with(b"BM") {
        if u32::from_le_bytes(d.get(14..18)?.try_into().ok()?) == 12 { Some((u16le(18)?, u16le(20)?)) } else { Some((i32le(18)?, i32le(22)?)) }
    } else if d.starts_with(b"RIFF") && d.get(8..12) == Some(b"WEBP") {
        match d.get(12..16)? {
            b"VP8 " => Some((u16le(26)? & 0x3FFF, u16le(28)? & 0x3FFF)),
            b"VP8L" => {
                let b = u32::from_le_bytes(d.get(21..25)?.try_into().ok()?);
                Some(((b & 0x3FFF) + 1, ((b >> 14) & 0x3FFF) + 1))
            }
            b"VP8X" => Some((u24le(24)? + 1, u24le(27)? + 1)),
            _ => None,
        }
    } else {
        None
    }
}

/// Pixels shrunk by a whole factor (averaging boxes) to at most
/// [`KEEP_PIXELS`].
fn shrink(w: u32, h: u32, rgba: Vec<u8>) -> (u32, u32, Vec<u8>) {
    let px = w as u64 * h as u64;
    if px <= KEEP_PIXELS {
        return (w, h, rgba);
    }
    let k = ((px as f64 / KEEP_PIXELS as f64).sqrt().ceil() as u32).max(2);
    let (nw, nh) = ((w / k).max(1), (h / k).max(1));
    let mut out = vec![0u8; nw as usize * nh as usize * 4];
    for y in 0..nh {
        for x in 0..nw {
            let mut sum = [0u32; 4];
            for dy in 0..k {
                let row = (y * k + dy) as usize * w as usize;
                for dx in 0..k {
                    let i = (row + (x * k + dx) as usize) * 4;
                    for c in 0..4 {
                        sum[c] += rgba[i + c] as u32;
                    }
                }
            }
            let o = (y as usize * nw as usize + x as usize) * 4;
            for c in 0..4 {
                out[o + c] = (sum[c] / (k * k)) as u8;
            }
        }
    }
    (nw, nh, out)
}

/// A BMP file from a device-independent bitmap.
pub(crate) fn bmp_from_dib(dib: &[u8]) -> Option<Vec<u8>> {
    let header = u32::from_le_bytes(dib.get(..4)?.try_into().ok()?) as usize;
    let bits = u16::from_le_bytes(dib.get(14..16)?.try_into().ok()?) as usize;
    let used = dib.get(32..36).map_or(0, |b| u32::from_le_bytes([b[0], b[1], b[2], b[3]])) as usize;
    let colors = if used > 0 { used.min(256) } else if bits <= 8 { 1 << bits } else { 0 };
    let offset = 14 + header.min(1024) + colors * 4;
    let mut out = b"BM".to_vec();
    out.extend_from_slice(&((14 + dib.len()) as u32).to_le_bytes());
    out.extend_from_slice(&[0; 4]);
    out.extend_from_slice(&(offset as u32).to_le_bytes());
    out.extend_from_slice(dib);
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encoded(format: image::ImageFormat) -> Vec<u8> {
        let img = image::RgbaImage::from_pixel(5, 3, image::Rgba([10, 20, 30, 255]));
        let mut out = Cursor::new(Vec::new());
        img.write_to(&mut out, format).unwrap();
        out.into_inner()
    }

    #[test]
    fn sizes_from_headers() {
        for f in [image::ImageFormat::Png, image::ImageFormat::Bmp, image::ImageFormat::Gif, image::ImageFormat::WebP] {
            let d = encoded(f);
            assert_eq!(header_size(&d), Some((5, 3)), "{f:?}");
            let (w, h, px) = decode(&d, &Limits::default()).unwrap();
            assert_eq!((w, h, px.len()), (5, 3, 60));
        }
        let mut small = Limits::default();
        small.max_image_pixels = 10;
        assert!(decode(&encoded(image::ImageFormat::Png), &small).is_none());
    }
}
