//! Lector mínimo de TIFF/EXIF: orientación, datos de la toma, GPS y vistas previas JPEG incrustadas
//! (RAW de cámara: CR2, NEF, ARW, DNG, ORF, RW2, PEF…).
use crate::i18n::t;
use std::io::{Cursor, Read, Seek, SeekFrom};
use std::path::Path;

pub struct Ent {
    pub tag: u16,
    pub typ: u16,
    pub count: u32,
    val: [u8; 4],
}

pub struct Tiff<R> {
    r: R,
    le: bool,
    base: u64,
}

fn type_size(t: u16) -> usize {
    match t {
        1 | 2 | 6 | 7 => 1,
        3 | 8 => 2,
        4 | 9 | 11 => 4,
        5 | 10 | 12 => 8,
        _ => 1,
    }
}

impl<R: Read + Seek> Tiff<R> {
    /// Cabecera TIFF en `base` (II/MM + marca; la marca no se comprueba para admitir ORF/RW2/CR2).
    pub fn new(mut r: R, base: u64) -> Option<Self> {
        r.seek(SeekFrom::Start(base)).ok()?;
        let mut h = [0u8; 4];
        r.read_exact(&mut h).ok()?;
        let le = match &h[..2] {
            b"II" => true,
            b"MM" => false,
            _ => return None,
        };
        Some(Tiff { r, le, base })
    }

    fn u16(&self, b: &[u8]) -> u16 {
        if self.le { u16::from_le_bytes([b[0], b[1]]) } else { u16::from_be_bytes([b[0], b[1]]) }
    }

    fn u32(&self, b: &[u8]) -> u32 {
        if self.le { u32::from_le_bytes([b[0], b[1], b[2], b[3]]) } else { u32::from_be_bytes([b[0], b[1], b[2], b[3]]) }
    }

    fn read_at(&mut self, off: u64, n: usize) -> Option<Vec<u8>> {
        self.r.seek(SeekFrom::Start(self.base + off)).ok()?;
        let mut v = vec![0u8; n];
        self.r.read_exact(&mut v).ok()?;
        Some(v)
    }

    pub fn first_ifd(&mut self) -> Option<u32> {
        let b = self.read_at(4, 4)?;
        Some(self.u32(&b))
    }

    /// Entradas de un IFD y el desplazamiento del siguiente.
    pub fn ifd(&mut self, off: u32) -> Option<(Vec<Ent>, u32)> {
        if off < 8 {
            return None;
        }
        let b = self.read_at(off as u64, 2)?;
        let n = self.u16(&b) as usize;
        if n == 0 || n > 512 {
            return None;
        }
        let b = self.read_at(off as u64 + 2, n * 12 + 4)?;
        let ents = (0..n)
            .map(|k| {
                let e = &b[k * 12..k * 12 + 12];
                Ent { tag: self.u16(&e[0..2]), typ: self.u16(&e[2..4]), count: self.u32(&e[4..8]), val: [e[8], e[9], e[10], e[11]] }
            })
            .collect();
        let next = self.u32(&b[n * 12..]);
        Some((ents, next))
    }

    /// Bytes del valor de una entrada (en línea o en su desplazamiento).
    pub fn data(&mut self, e: &Ent, max: usize) -> Option<Vec<u8>> {
        let n = type_size(e.typ).checked_mul(e.count as usize)?;
        if n > max {
            return None;
        }
        if n <= 4 {
            return Some(e.val[..n].to_vec());
        }
        let off = self.u32(&e.val);
        self.read_at(off as u64, n)
    }

    /// Primer valor entero (SHORT o LONG).
    pub fn int(&mut self, e: &Ent) -> Option<u32> {
        let d = self.data(e, 64)?;
        match e.typ {
            3 if d.len() >= 2 => Some(self.u16(&d) as u32),
            4 if d.len() >= 4 => Some(self.u32(&d)),
            1 if !d.is_empty() => Some(d[0] as u32),
            _ => None,
        }
    }

    pub fn ints(&mut self, e: &Ent, max: usize) -> Vec<u32> {
        let Some(d) = self.data(e, max * 4) else { return Vec::new() };
        match e.typ {
            3 => d.chunks_exact(2).map(|c| self.u16(c) as u32).collect(),
            4 => d.chunks_exact(4).map(|c| self.u32(c)).collect(),
            _ => Vec::new(),
        }
    }

    pub fn rationals(&mut self, e: &Ent) -> Vec<(u32, u32)> {
        if e.typ != 5 && e.typ != 10 {
            return Vec::new();
        }
        let Some(d) = self.data(e, 256) else { return Vec::new() };
        d.chunks_exact(8).map(|c| (self.u32(&c[0..4]), self.u32(&c[4..8]))).collect()
    }

    pub fn ascii(&mut self, e: &Ent) -> String {
        let Some(d) = self.data(e, 512) else { return String::new() };
        String::from_utf8_lossy(&d).trim_matches(|c: char| c == '\0' || c.is_whitespace()).to_string()
    }
}

/// Posición de la cabecera TIFF dentro del archivo: 0 para TIFF y RAW, el segmento APP1 en un JPEG.
fn tiff_base(path: &Path, f: &mut std::fs::File) -> Option<u64> {
    let mut head = [0u8; 4];
    f.read_exact(&mut head).ok()?;
    if &head[..2] == b"II" || &head[..2] == b"MM" {
        return Some(0);
    }
    if head[..2] != [0xFF, 0xD8] {
        let _ = path;
        return None;
    }
    let mut pos = 2u64;
    for _ in 0..24 {
        f.seek(SeekFrom::Start(pos)).ok()?;
        let mut m = [0u8; 4];
        f.read_exact(&mut m).ok()?;
        if m[0] != 0xFF {
            return None;
        }
        let len = u16::from_be_bytes([m[2], m[3]]) as u64;
        match m[1] {
            0xE1 => {
                let mut id = [0u8; 6];
                f.read_exact(&mut id).ok()?;
                if &id == b"Exif\0\0" {
                    return Some(pos + 4 + 6);
                }
            }
            0xDA | 0xD9 => return None,
            _ => {}
        }
        pos += 2 + len;
    }
    None
}

fn ifd_entries<R: Read + Seek>(tf: &mut Tiff<R>, off: u32) -> Vec<Ent> {
    tf.ifd(off).map(|x| x.0).unwrap_or_default()
}

fn find(ents: &[Ent], tag: u16) -> Option<&Ent> {
    ents.iter().find(|e| e.tag == tag)
}

/// Orientación EXIF (1..8) de un JPEG o TIFF; 1 si no hay.
pub fn orientation(path: &Path) -> u8 {
    let Ok(mut f) = std::fs::File::open(path) else { return 1 };
    let Some(base) = tiff_base(path, &mut f) else { return 1 };
    let Some(mut tf) = Tiff::new(f, base) else { return 1 };
    let Some(first) = tf.first_ifd() else { return 1 };
    let ents = ifd_entries(&mut tf, first);
    find(&ents, 0x0112).and_then(|e| tf.int(e)).filter(|v| (1..=8).contains(v)).map(|v| v as u8).unwrap_or(1)
}

fn fmt_rat(r: (u32, u32)) -> f64 {
    if r.1 == 0 { 0.0 } else { r.0 as f64 / r.1 as f64 }
}

fn gps(v: &[(u32, u32)], r: &str) -> Option<String> {
    if v.len() < 3 {
        return None;
    }
    let deg = fmt_rat(v[0]) + fmt_rat(v[1]) / 60.0 + fmt_rat(v[2]) / 3600.0;
    Some(format!("{deg:.5}° {r}"))
}

/// Datos de la toma como pares (etiqueta, valor), ya traducidos.
pub fn summary(path: &Path) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let Ok(mut f) = std::fs::File::open(path) else { return out };
    let Some(base) = tiff_base(path, &mut f) else { return out };
    let Some(mut tf) = Tiff::new(f, base) else { return out };
    let Some(first) = tf.first_ifd() else { return out };
    let ents = ifd_entries(&mut tf, first);
    let make = find(&ents, 0x010F).map(|e| tf.ascii(e)).unwrap_or_default();
    let model = find(&ents, 0x0110).map(|e| tf.ascii(e)).unwrap_or_default();
    let cam = if !make.is_empty() && model.to_lowercase().starts_with(&make.to_lowercase()) { model.clone() } else { format!("{make} {model}").trim().to_string() };
    if !cam.is_empty() {
        out.push((t("Cámara").into(), cam));
    }
    if let Some(e) = find(&ents, 0x0131) {
        let s = tf.ascii(e);
        if !s.is_empty() {
            out.push((t("Programa").into(), s));
        }
    }
    let exif = find(&ents, 0x8769).and_then(|e| tf.int(e)).map(|o| ifd_entries(&mut tf, o)).unwrap_or_default();
    if let Some(e) = find(&exif, 0xA434) {
        let s = tf.ascii(e);
        if !s.is_empty() {
            out.push((t("Objetivo").into(), s));
        }
    }
    let date = find(&exif, 0x9003).or_else(|| find(&ents, 0x0132)).map(|e| tf.ascii(e)).unwrap_or_default();
    if !date.is_empty() {
        // 2024:05:01 12:30:00 → 2024-05-01 12:30:00
        let mut d = date.clone();
        if d.len() >= 10 && d.is_char_boundary(4) && d.is_char_boundary(5) && d.is_char_boundary(7) && d.is_char_boundary(8) && d.is_char_boundary(10) {
            d = format!("{}-{}-{}{}", &d[0..4], &d[5..7], &d[8..10], &d[10..]);
        }
        out.push((t("Fecha de la toma").into(), d));
    }
    if let Some(r) = find(&exif, 0x829A).and_then(|e| tf.rationals(e).first().copied()) {
        let s = if r.0 != 0 && r.1 > r.0 { format!("1/{} s", (r.1 as f64 / r.0 as f64).round()) } else { format!("{} s", fmt_rat(r)) };
        out.push((t("Exposición").into(), s));
    }
    if let Some(r) = find(&exif, 0x829D).and_then(|e| tf.rationals(e).first().copied()) {
        out.push((t("Apertura").into(), format!("f/{:.1}", fmt_rat(r))));
    }
    if let Some(v) = find(&exif, 0x8827).and_then(|e| tf.int(e)) {
        out.push(("ISO".into(), v.to_string()));
    }
    if let Some(r) = find(&exif, 0x920A).and_then(|e| tf.rationals(e).first().copied()) {
        out.push((t("Distancia focal").into(), format!("{:.0} mm", fmt_rat(r))));
    }
    let gps_ifd = find(&ents, 0x8825).and_then(|e| tf.int(e)).map(|o| ifd_entries(&mut tf, o)).unwrap_or_default();
    if !gps_ifd.is_empty() {
        let lat_r = find(&gps_ifd, 1).map(|e| tf.ascii(e)).unwrap_or_default();
        let lon_r = find(&gps_ifd, 3).map(|e| tf.ascii(e)).unwrap_or_default();
        let lat = find(&gps_ifd, 2).map(|e| tf.rationals(e)).unwrap_or_default();
        let lon = find(&gps_ifd, 4).map(|e| tf.rationals(e)).unwrap_or_default();
        if let (Some(a), Some(b)) = (gps(&lat, &lat_r), gps(&lon, &lon_r)) {
            out.push((t("Ubicación GPS").into(), format!("{a}, {b}")));
        }
    }
    out
}

/// La mayor vista previa JPEG incrustada en un RAW de cámara o DNG: (desplazamiento, longitud, orientación).
pub fn embedded_jpeg(path: &Path) -> Option<(u64, usize, u8)> {
    let f = std::fs::File::open(path).ok()?;
    let mut tf = Tiff::new(std::io::BufReader::new(f), 0)?;
    let mut best: Option<(u64, usize)> = None;
    let mut todo = vec![tf.first_ifd()?];
    let mut orient = 1u8;
    let mut seen = 0;
    while let Some(off) = todo.pop() {
        seen += 1;
        if seen > 24 {
            break;
        }
        let Some((ents, next)) = tf.ifd(off) else { continue };
        if next != 0 {
            todo.push(next);
        }
        if seen == 1 {
            orient = find(&ents, 0x0112).and_then(|e| tf.int(e)).filter(|v| (1..=8).contains(v)).map(|v| v as u8).unwrap_or(1);
        }
        for tag in [0x014A, 0x8769] {
            if let Some(e) = find(&ents, tag) {
                todo.extend(tf.ints(e, 8));
            }
        }
        let mut cand = Vec::new();
        if let (Some(o), Some(l)) = (find(&ents, 0x0201), find(&ents, 0x0202)) {
            if let (Some(o), Some(l)) = (tf.int(o), tf.int(l)) {
                cand.push((o as u64, l as usize));
            }
        }
        let comp = find(&ents, 0x0103).and_then(|e| tf.int(e));
        if matches!(comp, Some(6 | 7)) {
            if let (Some(o), Some(l)) = (find(&ents, 0x0111), find(&ents, 0x0117)) {
                let (os, ls) = (tf.ints(o, 4), tf.ints(l, 4));
                if os.len() == 1 && ls.len() == 1 {
                    cand.push((os[0] as u64, ls[0] as usize));
                }
            }
        }
        for (o, l) in cand {
            if l > 4096 && best.map(|b| l > b.1).unwrap_or(true) {
                best = Some((o, l));
            }
        }
    }
    let (o, l) = best?;
    let mut f = tf.r;
    f.seek(SeekFrom::Start(o)).ok()?;
    let mut m = [0u8; 3];
    f.read_exact(&mut m).ok()?;
    (m == [0xFF, 0xD8, 0xFF]).then_some((o, l, orient))
}

/// Vista previa JPEG de otros contenedores (CR3 y similares): busca el mayor flujo JPEG del principio del archivo.
pub fn scan_jpeg(path: &Path) -> Option<(u64, usize)> {
    let mut f = std::fs::File::open(path).ok()?;
    let mut buf = Vec::new();
    f.by_ref().take(16 << 20).read_to_end(&mut buf).ok()?;
    let mut best: Option<(u64, usize)> = None;
    let mut i = 0;
    while i + 3 < buf.len() {
        if buf[i] == 0xFF && buf[i + 1] == 0xD8 && buf[i + 2] == 0xFF {
            // fin: el siguiente EOI tras recorrer los segmentos
            let end = jpeg_end(&buf[i..]);
            if let Some(n) = end {
                if best.map(|b| n > b.1).unwrap_or(true) && n > 16 * 1024 {
                    best = Some((i as u64, n));
                }
                i += n;
                continue;
            }
        }
        i += 1;
    }
    best
}

/// Longitud de un JPEG que empieza en `b[0]`, o `None` si está truncado.
fn jpeg_end(b: &[u8]) -> Option<usize> {
    let mut i = 2;
    while i + 4 <= b.len() {
        if b[i] != 0xFF {
            return None;
        }
        let m = b[i + 1];
        if m == 0xD9 {
            return Some(i + 2);
        }
        let len = u16::from_be_bytes([b[i + 2], b[i + 3]]) as usize;
        i += 2 + len;
        if m == 0xDA {
            // datos entrecortados hasta el EOI
            while i + 1 < b.len() {
                if b[i] == 0xFF && b[i + 1] == 0xD9 {
                    return Some(i + 2);
                }
                i += 1;
            }
            return None;
        }
    }
    None
}

pub fn read_range(path: &Path, off: u64, len: usize) -> Option<Vec<u8>> {
    let mut f = std::fs::File::open(path).ok()?;
    // el tamaño viene de la cabecera del archivo: se comprueba contra el real y se acota
    let flen = f.metadata().ok()?.len();
    if len > 64 << 20 || off.checked_add(len as u64)? > flen {
        return None;
    }
    f.seek(SeekFrom::Start(off)).ok()?;
    let mut v = vec![0u8; len];
    f.read_exact(&mut v).ok()?;
    Some(v)
}

#[allow(dead_code)]
pub fn cursor(v: Vec<u8>) -> Cursor<Vec<u8>> {
    Cursor::new(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jpeg_end_finds_eoi() {
        // SOI, APP0 (len 4), SOS (len 2) + datos, EOI
        let b = [0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x04, 1, 2, 0xFF, 0xDA, 0x00, 0x02, 9, 9, 0xFF, 0xD9];
        assert_eq!(jpeg_end(&b), Some(b.len()));
    }

    #[test]
    fn tiff_reads_ascii_and_ints() {
        // II*\0, IFD en 8: 2 entradas (Make "Cam\0" en línea, Orientation=6)
        let mut b = vec![b'I', b'I', 42, 0, 8, 0, 0, 0, 2, 0];
        b.extend([0x0F, 0x01, 2, 0, 4, 0, 0, 0, b'C', b'a', b'm', 0]);
        b.extend([0x12, 0x01, 3, 0, 1, 0, 0, 0, 6, 0, 0, 0]);
        b.extend([0, 0, 0, 0]);
        let mut tf = Tiff::new(Cursor::new(b), 0).unwrap();
        let (ents, next) = tf.ifd(8).unwrap();
        assert_eq!(next, 0);
        assert_eq!(tf.ascii(find(&ents, 0x010F).unwrap()), "Cam");
        assert_eq!(tf.int(find(&ents, 0x0112).unwrap()), Some(6));
    }
}
