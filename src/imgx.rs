//! Formatos de imagen que `image` no lee: RAW de cámara (vista previa JPEG incrustada), PSD
//! (imagen compuesta) y miniatura guardada en los archivos .blend.
#[allow(unused_imports)]
use crate::vfs::{KindV as _, PathV as _};
use crate::thumbs::{decode_jpeg_from, Decoded};
use std::io::{Cursor, Read};
use std::path::Path;

pub fn is_raw_ext(e: &str) -> bool {
    matches!(
        e,
        "raw" | "cr2" | "cr3" | "nef" | "nrw" | "arw" | "srf" | "sr2" | "dng" | "orf" | "rw2" | "pef" | "raf" | "srw" | "3fr" | "erf" | "kdc" | "mrw" | "x3f"
    )
}

/// ¿Esta extensión se decodifica aquí?
pub fn handles(e: &str) -> bool {
    is_raw_ext(e) || matches!(e, "psd" | "psb" | "blend" | "blend1")
}

pub fn decode(path: &Path, ext: &str, max: u32) -> Option<Decoded> {
    if is_raw_ext(ext) {
        return decode_raw(path, max);
    }
    match ext {
        "psd" | "psb" => decode_psd(path, max),
        "blend" | "blend1" => blend_thumb(path, max),
        _ => None,
    }
}

fn decode_raw(path: &Path, max: u32) -> Option<Decoded> {
    let (off, len, orient) = match crate::exif::embedded_jpeg(path) {
        Some(x) => x,
        None => {
            let (o, l) = crate::exif::scan_jpeg(path)?;
            (o, l, 1)
        }
    };
    let bytes = crate::exif::read_range(path, off, len)?;
    decode_jpeg_from(Cursor::new(bytes), max, orient)
}

// ---------------------------------------------------------------------------
// PSD: la sección «imagen» guarda la composición ya aplanada.
// ---------------------------------------------------------------------------

fn be16(b: &[u8]) -> usize {
    u16::from_be_bytes([b[0], b[1]]) as usize
}

fn be32(b: &[u8]) -> usize {
    u32::from_be_bytes([b[0], b[1], b[2], b[3]]) as usize
}

fn decode_psd(path: &Path, max: u32) -> Option<Decoded> {
    let md = crate::vfs::fs::metadata(path).ok()?;
    if md.len() > 512 << 20 {
        return None;
    }
    // la cabecera primero: si la imagen es grande se decodifica de una en una entre todos los hilos
    let mut head = [0u8; 26];
    crate::thumbs::open_regular(path)?.read_exact(&mut head).ok()?;
    if &head[..4] != b"8BPS" {
        return None;
    }
    let (hh, hw) = (be32(&head[14..]), be32(&head[18..]));
    let big = md.len() > 48 << 20 || (hw as u64).saturating_mul(hh as u64).saturating_mul(4) > 48 << 20;
    let _heavy = big.then(|| crate::thumbs::lock(&crate::thumbs::HEAVY));
    let data = crate::vfs::fs::read(path).ok()?;
    if data.len() < 30 || &data[..4] != b"8BPS" {
        return None;
    }
    let version = be16(&data[4..]);
    let channels = be16(&data[12..]);
    let (h, w) = (be32(&data[14..]), be32(&data[18..]));
    let depth = be16(&data[22..]);
    let mode = be16(&data[24..]);
    if w == 0 || h == 0 || w.checked_mul(h).is_none_or(|p| p > 100_000_000) || !matches!(depth, 8 | 16) || channels == 0 || channels > 5 {
        return None;
    }
    // memoria real: planos de todos los canales a su profundidad (16 bits duplica)
    if w.checked_mul(h).and_then(|p| p.checked_mul(channels.min(4) * (depth / 8))).map(|b| b > 256 << 20).unwrap_or(true) {
        return None;
    }
    // saltar color mode data, image resources y layer/mask info
    let mut p = 26;
    let n = be32(data.get(p..p + 4)?);
    p = p.checked_add(4)?.checked_add(n)?;
    let n = be32(data.get(p..p + 4)?);
    p = p.checked_add(4)?.checked_add(n)?;
    let n = if version == 2 {
        u64::from_be_bytes(data.get(p..p + 8)?.try_into().ok()?) as usize
    } else {
        be32(data.get(p..p + 4)?)
    };
    p = p.checked_add(if version == 2 { 8 } else { 4 })?.checked_add(n)?;
    let comp = be16(data.get(p..p.checked_add(2)?)?);
    p += 2;
    let bpc = depth / 8;
    let plane = w * h * bpc;
    let ch = channels.min(4);
    let mut planes: Vec<Vec<u8>> = Vec::with_capacity(ch);
    match comp {
        0 => {
            for c in 0..ch {
                planes.push(data.get(p + c * plane..p + (c + 1) * plane)?.to_vec());
            }
        }
        1 => {
            // PackBits: tabla de longitudes por fila y canal
            let rows = h * channels;
            let w_len = if version == 2 { 4 } else { 2 };
            let mut q = p + rows * w_len;
            let row_w = w * bpc;
            for c in 0..channels {
                let mut pl = Vec::with_capacity(plane);
                for r in 0..h {
                    let k = c * h + r;
                    let len = if version == 2 { be32(data.get(p + k * 4..p + k * 4 + 4)?) } else { be16(data.get(p + k * 2..p + k * 2 + 2)?) };
                    let src = data.get(q..q.checked_add(len)?)?;
                    q += len;
                    let start = pl.len();
                    let mut i = 0;
                    while i < src.len() && pl.len() - start < row_w {
                        let c0 = src[i] as i8;
                        i += 1;
                        if c0 >= 0 {
                            let n = c0 as usize + 1;
                            pl.extend_from_slice(src.get(i..i + n)?);
                            i += n;
                        } else if c0 != -128 {
                            let n = 1 - c0 as isize;
                            let b = *src.get(i)?;
                            i += 1;
                            pl.extend(std::iter::repeat(b).take(n as usize));
                        }
                    }
                    pl.resize(start + row_w, 0);
                }
                if c < ch {
                    planes.push(pl);
                }
            }
        }
        _ => return None,
    }
    // a 8 bits por canal (en 16 bits se toma el byte alto)
    let px = |c: usize, i: usize| -> u8 { planes[c][i * bpc] };
    let mut rgba = vec![255u8; w * h * 4];
    for i in 0..w * h {
        let (r, g, b, a) = match (mode, ch) {
            (1, c) if c >= 1 => {
                let v = px(0, i);
                (v, v, v, if c >= 2 { px(1, i) } else { 255 })
            }
            (3, c) if c >= 3 => (px(0, i), px(1, i), px(2, i), if c >= 4 { px(3, i) } else { 255 }),
            (4, 4) => {
                // CMYK invertido en PSD
                let k = px(3, i) as u32;
                ((px(0, i) as u32 * k / 255) as u8, (px(1, i) as u32 * k / 255) as u8, (px(2, i) as u32 * k / 255) as u8, 255)
            }
            _ => return None,
        };
        rgba[i * 4..i * 4 + 4].copy_from_slice(&[r, g, b, a]);
    }
    let img = image::RgbaImage::from_raw(w as u32, h as u32, rgba)?;
    let img = if w as u32 > max || h as u32 > max { image::DynamicImage::ImageRgba8(img).thumbnail(max, max).into_rgba8() } else { img };
    Some(Decoded { w: img.width() as usize, h: img.height() as usize, rgba: img.into_raw() })
}

// ---------------------------------------------------------------------------
// .blend: el bloque «TEST» del principio guarda la miniatura (RGBA, de abajo arriba).
// ---------------------------------------------------------------------------

/// Lado máximo de la miniatura guardada en un .blend (Blender la guarda de 128 px).
const BLEND_MAX_SIDE: usize = 2048;
/// Bytes como mucho que se descomprimen y descartan buscando la miniatura.
const BLEND_SKIP_BUDGET: u64 = 256 << 20;

fn blend_thumb(path: &Path, max: u32) -> Option<Decoded> {
    let mut f = crate::thumbs::open_regular(path)?;
    let mut magic = [0u8; 4];
    f.read_exact(&mut magic).ok()?;
    drop(f);
    let f = crate::thumbs::open_regular(path)?;
    let mut r: Box<dyn Read> = if magic[..2] == [0x1F, 0x8B] {
        Box::new(flate2::read::MultiGzDecoder::new(f))
    } else if magic == [0x28, 0xB5, 0x2F, 0xFD] {
        Box::new(ruzstd::decoding::StreamingDecoder::new(f).ok()?)
    } else {
        Box::new(f)
    };
    let mut head = [0u8; 12];
    r.read_exact(&mut head).ok()?;
    if &head[..7] != b"BLENDER" {
        return None;
    }
    // Formato clásico: «BLENDER» + puntero (_ 4 bytes, - 8) + endianness (v little, V big) + 3 dígitos de versión.
    // Formato nuevo: «BLENDER17-01v0500» (cabecera de 17 bytes, punteros de 8 y tamaños de 64 bits).
    let large = head[7].is_ascii_digit();
    let (ptr, le) = if large {
        let mut more = [0u8; 5];
        r.read_exact(&mut more).ok()?;
        (8usize, more[0] == b'v')
    } else {
        (if head[7] == b'-' { 8 } else { 4 }, head[8] == b'v')
    };
    let u32at = |b: &[u8]| if le { u32::from_le_bytes([b[0], b[1], b[2], b[3]]) } else { u32::from_be_bytes([b[0], b[1], b[2], b[3]]) } as usize;
    let mut skip_left = BLEND_SKIP_BUDGET;
    for _ in 0..64 {
        let mut code = [0u8; 4];
        r.read_exact(&mut code).ok()?;
        let size = if large {
            // sdna(4) + puntero(8) + longitud(8) + cuenta(8)
            let mut b = [0u8; 28];
            r.read_exact(&mut b).ok()?;
            let l: [u8; 8] = b[12..20].try_into().ok()?;
            // en 32 bits un tamaño enorme no se trunca: se toma como inválido
            usize::try_from(if le { u64::from_le_bytes(l) } else { u64::from_be_bytes(l) }).unwrap_or(usize::MAX)
        } else {
            // tamaño(4) + puntero + sdna(4) + cuenta(4)
            let mut s = [0u8; 4];
            r.read_exact(&mut s).ok()?;
            let mut skip = vec![0u8; ptr + 8];
            r.read_exact(&mut skip).ok()?;
            u32at(&s)
        };
        if &code == b"TEST" {
            if size < 8 || size > 16 << 20 {
                return None;
            }
            // primero las dimensiones; el resto sólo si son razonables
            let mut d = [0u8; 8];
            r.read_exact(&mut d).ok()?;
            let (w, h) = (u32at(&d[0..4]), u32at(&d[4..8]));
            if w == 0 || h == 0 || w > BLEND_MAX_SIDE || h > BLEND_MAX_SIDE || w * h * 4 > size - 8 {
                return None;
            }
            let mut src = vec![0u8; w * h * 4];
            r.read_exact(&mut src).ok()?;
            let mut rgba = Vec::with_capacity(w * h * 4);
            for y in (0..h).rev() {
                rgba.extend_from_slice(&src[y * w * 4..(y + 1) * w * 4]);
            }
            let max = max.max(1) as usize;
            if w <= max && h <= max {
                return Some(Decoded { w, h, rgba });
            }
            let img = image::RgbaImage::from_raw(w as u32, h as u32, rgba)?;
            let img = image::DynamicImage::ImageRgba8(img).thumbnail(max as u32, max as u32).into_rgba8();
            return Some(Decoded { w: img.width() as usize, h: img.height() as usize, rgba: img.into_raw() });
        }
        if &code == b"ENDB" || &code == b"DNA1" || size > 1 << 28 {
            return None;
        }
        skip_left = skip_left.checked_sub(size as u64)?;
        // bloques que no interesan: se descartan leyendo
        std::io::copy(&mut r.by_ref().take(size as u64), &mut std::io::sink()).ok()?;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// .blend clásico (punteros de 8, little endian) con un bloque TEST de `w`×`h`.
    fn blend(w: u32, h: u32, size: u32) -> Vec<u8> {
        let mut b = b"BLENDER-v300".to_vec();
        b.extend_from_slice(b"TEST");
        b.extend_from_slice(&size.to_le_bytes());
        b.extend_from_slice(&[0; 16]);
        b.extend_from_slice(&w.to_le_bytes());
        b.extend_from_slice(&h.to_le_bytes());
        let px = (w as usize * h as usize * 4).min(size.saturating_sub(8) as usize).min(64 << 20);
        b.extend((0..px).map(|i| i as u8));
        b
    }

    #[test]
    fn blend_thumbnail_is_validated_and_downscaled() {
        let dir = std::env::temp_dir().join(format!("fb-imgx-blend-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let p = dir.join("x.blend");
        // tamaño normal: se devuelve tal cual
        std::fs::write(&p, blend(128, 128, 8 + 128 * 128 * 4)).unwrap();
        let d = decode(&p, "blend", 256).unwrap();
        assert_eq!((d.w, d.h, d.rgba.len()), (128, 128, 128 * 128 * 4));
        // grande: se reduce al máximo pedido
        std::fs::write(&p, blend(1024, 512, 8 + 1024 * 512 * 4)).unwrap();
        let d = decode(&p, "blend", 64).unwrap();
        assert!(d.w <= 64 && d.h <= 64 && d.rgba.len() == d.w * d.h * 4);
        // dimensiones absurdas o que no caben en el bloque
        std::fs::write(&p, blend(4096, 4096, 16 << 20)).unwrap();
        assert!(decode(&p, "blend", 64).is_none());
        std::fs::write(&p, blend(100, 100, 8 + 100)).unwrap();
        assert!(decode(&p, "blend", 64).is_none());
        std::fs::write(&p, blend(1, 1, u32::MAX)).unwrap();
        assert!(decode(&p, "blend", 64).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn psd_huge_dimensions_are_rejected() {
        let dir = std::env::temp_dir().join(format!("fb-imgx-psd-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let p = dir.join("x.psd");
        let mut b = b"8BPS".to_vec();
        b.extend_from_slice(&1u16.to_be_bytes());
        b.extend_from_slice(&[0; 6]);
        b.extend_from_slice(&3u16.to_be_bytes());
        b.extend_from_slice(&0xFFFF_FFFFu32.to_be_bytes());
        b.extend_from_slice(&0xFFFF_FFFFu32.to_be_bytes());
        b.extend_from_slice(&8u16.to_be_bytes());
        b.extend_from_slice(&3u16.to_be_bytes());
        b.resize(64, 0);
        std::fs::write(&p, &b).unwrap();
        assert!(decode(&p, "psd", 64).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
