//! Datos del archivo para el panel de información de la vista previa: foto (EXIF), audio (etiquetas
//! y formato), vídeo (contenedor MP4/MKV) y fuentes.
use crate::fsys::Kind;
use crate::i18n::t;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

pub type Rows = Vec<(String, String)>;

fn secs(s: f64) -> String {
    let s = s.max(0.0) as u64;
    if s >= 3600 { format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60) } else { format!("{}:{:02}", s / 60, s % 60) }
}

fn push(rows: &mut Rows, label: &str, v: impl Into<String>) {
    let v = v.into();
    if !v.trim().is_empty() {
        rows.push((label.to_string(), v));
    }
}

fn bitrate(size: u64, dur: f64) -> Option<String> {
    (dur > 1.0).then(|| format!("{} kb/s", (size as f64 * 8.0 / dur / 1000.0).round() as u64))
}

pub fn collect(path: &Path, kind: Kind, size: u64) -> Rows {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    match kind {
        _ if crate::fontview::is_font_ext(&ext) => crate::fontview::info(path),
        Kind::Image => image(path),
        Kind::Audio => audio(path, size),
        Kind::Video => video(path, &ext, size),
        _ => Vec::new(),
    }
}

fn image(path: &Path) -> Rows {
    let mut rows = Rows::new();
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    if !crate::imgx::handles(&ext) && ext != "svg" {
        if let Ok(r) = image::ImageReader::open(path).and_then(|r| r.with_guessed_format()) {
            if let Ok((w, h)) = r.into_dimensions() {
                push(&mut rows, t("Dimensiones"), format!("{w} × {h}"));
            }
        }
    }
    rows.extend(crate::exif::summary(path));
    rows
}

fn audio(path: &Path, size: u64) -> Rows {
    use symphonia::core::codecs::CODEC_TYPE_NULL;
    use symphonia::core::formats::FormatOptions;
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::{MetadataOptions, StandardTagKey as K};
    use symphonia::core::probe::Hint;
    let mut rows = Rows::new();
    let Ok(file) = std::fs::File::open(path) else { return rows };
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(e) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(e);
    }
    let Ok(mut probed) = symphonia::default::get_probe().format(&hint, mss, &FormatOptions::default(), &MetadataOptions::default()) else { return rows };
    let mut tags: [String; 6] = Default::default();
    let mut take = |rev: &symphonia::core::meta::MetadataRevision| {
        for tag in rev.tags() {
            let v = tag.value.to_string();
            let slot = match tag.std_key {
                Some(K::TrackTitle) => 0,
                Some(K::Artist) => 1,
                Some(K::Album) => 2,
                Some(K::Date | K::ReleaseDate | K::OriginalDate) => 3,
                Some(K::Genre) => 4,
                Some(K::TrackNumber) => 5,
                _ => continue,
            };
            if tags[slot].is_empty() {
                tags[slot] = v;
            }
        }
    };
    if let Some(md) = probed.metadata.get() {
        if let Some(rev) = md.current() {
            take(rev);
        }
    }
    if let Some(rev) = probed.format.metadata().current() {
        take(rev);
    }
    for (i, label) in [t("Título"), t("Artista"), t("Álbum"), t("Año"), t("Género"), t("Pista")].into_iter().enumerate() {
        let v = if i == 3 { tags[i].chars().take(4).collect() } else { tags[i].clone() };
        push(&mut rows, label, v);
    }
    if let Some(tr) = probed.format.tracks().iter().find(|t| t.codec_params.codec != CODEC_TYPE_NULL) {
        let p = &tr.codec_params;
        let mut dur = 0.0;
        if let (Some(n), Some(sr)) = (p.n_frames, p.sample_rate) {
            if sr > 0 {
                dur = n as f64 / sr as f64;
                push(&mut rows, t("Duración"), secs(dur));
            }
        }
        if let Some(d) = symphonia::default::get_codecs().get_codec(p.codec) {
            push(&mut rows, t("Códec"), d.short_name);
        }
        if let Some(sr) = p.sample_rate {
            push(&mut rows, t("Frecuencia de muestreo"), format!("{} Hz", sr));
        }
        if let Some(c) = p.channels {
            push(&mut rows, t("Canales"), c.count().to_string());
        }
        if let Some(b) = p.bits_per_sample {
            push(&mut rows, t("Bits por muestra"), b.to_string());
        }
        if let Some(b) = bitrate(size, dur) {
            push(&mut rows, t("Tasa de bits"), b);
        }
    }
    rows
}

// ---------------------------------------------------------------------------
// Vídeo
// ---------------------------------------------------------------------------

#[derive(Default)]
struct Vid {
    dur: f64,
    w: u32,
    h: u32,
    fps: f64,
    vcodec: String,
    acodec: String,
    rate: u32,
    ch: u32,
}

fn video(path: &Path, ext: &str, size: u64) -> Rows {
    let mut v = Vid::default();
    let ok = match ext {
        "mkv" | "webm" | "mka" => mkv(path, &mut v),
        "mp4" | "m4v" | "mov" | "3gp" => mp4(path, &mut v),
        _ => false,
    };
    let mut rows = Rows::new();
    if !ok {
        return rows;
    }
    push(&mut rows, t("Duración"), if v.dur > 0.0 { secs(v.dur) } else { String::new() });
    if v.w > 0 {
        push(&mut rows, t("Resolución"), format!("{} × {}", v.w, v.h));
    }
    push(&mut rows, t("Códec de vídeo"), v.vcodec);
    if v.fps > 1.0 && v.fps < 1000.0 {
        push(&mut rows, t("Fotogramas/s"), format!("{:.2}", v.fps).trim_end_matches('0').trim_end_matches('.').to_string());
    }
    push(&mut rows, t("Códec de audio"), v.acodec);
    if v.rate > 0 {
        push(&mut rows, t("Frecuencia de muestreo"), format!("{} Hz", v.rate));
    }
    if v.ch > 0 {
        push(&mut rows, t("Canales"), v.ch.to_string());
    }
    if let Some(b) = bitrate(size, v.dur) {
        push(&mut rows, t("Tasa de bits"), b);
    }
    rows
}

fn be32(b: &[u8]) -> u32 {
    u32::from_be_bytes([b[0], b[1], b[2], b[3]])
}

fn be64(b: &[u8]) -> u64 {
    u64::from_be_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]])
}

/// Hijos de una caja MP4: (tipo, contenido).
fn children(b: &[u8]) -> Vec<([u8; 4], &[u8])> {
    let mut out = Vec::new();
    let mut i = 0;
    while i + 8 <= b.len() {
        let mut size = be32(&b[i..]) as usize;
        let ty = [b[i + 4], b[i + 5], b[i + 6], b[i + 7]];
        let mut head = 8;
        if size == 1 && i + 16 <= b.len() {
            size = be64(&b[i + 8..]) as usize;
            head = 16;
        } else if size == 0 {
            size = b.len() - i;
        }
        if size < head || i + size > b.len() {
            break;
        }
        out.push((ty, &b[i + head..i + size]));
        i += size;
    }
    out
}

fn find<'a>(c: &[([u8; 4], &'a [u8])], ty: &[u8; 4]) -> Option<&'a [u8]> {
    c.iter().find(|x| &x.0 == ty).map(|x| x.1)
}

fn vcodec_name(f: &[u8]) -> Option<&'static str> {
    Some(match f {
        b"avc1" | b"avc3" => "H.264",
        b"hvc1" | b"hev1" => "H.265",
        b"av01" => "AV1",
        b"vp09" => "VP9",
        b"vp08" => "VP8",
        b"mp4v" => "MPEG-4",
        b"apch" | b"apcn" | b"apcs" | b"apco" | b"ap4h" => "ProRes",
        b"jpeg" | b"mjpa" => "MJPEG",
        _ => return None,
    })
}

fn acodec_name(f: &[u8]) -> Option<&'static str> {
    Some(match f {
        b"mp4a" => "AAC",
        b"ac-3" => "AC-3",
        b"ec-3" => "E-AC-3",
        b"Opus" | b"opus" => "Opus",
        b"alac" => "ALAC",
        b"fLaC" => "FLAC",
        b".mp3" => "MP3",
        b"sowt" | b"twos" | b"lpcm" => "PCM",
        _ => return None,
    })
}

fn mp4(path: &Path, v: &mut Vid) -> bool {
    let Ok(mut f) = std::fs::File::open(path) else { return false };
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    let mut pos = 0u64;
    // el índice (moov) puede estar al principio o al final: se salta mdat sin leerlo
    let mut moov = None;
    for _ in 0..64 {
        if pos + 8 > len || f.seek(SeekFrom::Start(pos)).is_err() {
            break;
        }
        let mut h = [0u8; 16];
        let n = f.read(&mut h).unwrap_or(0);
        if n < 8 {
            break;
        }
        let mut size = be32(&h) as u64;
        let mut head = 8;
        if size == 1 && n >= 16 {
            size = be64(&h[8..]);
            head = 16;
        } else if size == 0 {
            size = len - pos;
        }
        if size < head {
            break;
        }
        if &h[4..8] == b"moov" {
            if size > 128 << 20 {
                return false;
            }
            let mut b = vec![0u8; (size - head) as usize];
            if f.seek(SeekFrom::Start(pos + head)).is_err() || f.read_exact(&mut b).is_err() {
                return false;
            }
            moov = Some(b);
            break;
        }
        pos += size;
    }
    let Some(moov) = moov else { return false };
    let top = children(&moov);
    if let Some(m) = find(&top, b"mvhd") {
        let (ts, d) = if m[0] == 1 && m.len() >= 32 { (be32(&m[20..]) as f64, be64(&m[24..]) as f64) } else if m.len() >= 20 { (be32(&m[12..]) as f64, be32(&m[16..]) as f64) } else { (0.0, 0.0) };
        if ts > 0.0 {
            v.dur = d / ts;
        }
    }
    for (ty, trak) in &top {
        if ty != b"trak" {
            continue;
        }
        let tc = children(trak);
        let Some(mdia) = find(&tc, b"mdia") else { continue };
        let mc = children(mdia);
        let handler = find(&mc, b"hdlr").filter(|h| h.len() >= 12).map(|h| [h[8], h[9], h[10], h[11]]);
        let Some(minf) = find(&mc, b"minf") else { continue };
        let Some(stbl) = find(&children(minf), b"stbl") else { continue };
        let sc = children(stbl);
        let entry = find(&sc, b"stsd").filter(|s| s.len() >= 24).map(|s| (&s[12..16], &s[16..]));
        match handler.as_ref().map(|h| &h[..]) {
            Some(b"vide") if v.vcodec.is_empty() => {
                if let Some((f4, e)) = entry {
                    v.vcodec = vcodec_name(f4).map(|s| s.to_string()).unwrap_or_else(|| String::from_utf8_lossy(f4).into_owned());
                    // VisualSampleEntry: ancho y alto a 24..28 desde el inicio de la entrada
                    if e.len() >= 28 {
                        v.w = u16::from_be_bytes([e[24], e[25]]) as u32;
                        v.h = u16::from_be_bytes([e[26], e[27]]) as u32;
                    }
                }
                if let Some(t) = find(&tc, b"tkhd") {
                    let off = if t[0] == 1 { 88 } else { 76 };
                    if t.len() >= off + 8 && be32(&t[off..]) >> 16 > 0 {
                        v.w = be32(&t[off..]) >> 16;
                        v.h = be32(&t[off + 4..]) >> 16;
                    }
                }
                let ts = find(&mc, b"mdhd").map(|m| if m[0] == 1 && m.len() >= 32 { (be32(&m[20..]) as f64, be64(&m[24..]) as f64) } else if m.len() >= 20 { (be32(&m[12..]) as f64, be32(&m[16..]) as f64) } else { (0.0, 0.0) });
                let count = find(&sc, b"stsz").filter(|s| s.len() >= 12).map(|s| be32(&s[8..]) as f64);
                if let (Some((ts, d)), Some(n)) = (ts, count) {
                    if ts > 0.0 && d > 0.0 {
                        v.fps = n / (d / ts);
                    }
                }
            }
            Some(b"soun") if v.acodec.is_empty() => {
                if let Some((f4, e)) = entry {
                    v.acodec = acodec_name(f4).map(|s| s.to_string()).unwrap_or_else(|| String::from_utf8_lossy(f4).into_owned());
                    if e.len() >= 28 {
                        v.ch = u16::from_be_bytes([e[16], e[17]]) as u32;
                        v.rate = u16::from_be_bytes([e[24], e[25]]) as u32;
                    }
                }
            }
            _ => {}
        }
    }
    true
}

// ---- Matroska / WebM ----

fn vint(b: &[u8], keep: bool) -> Option<(u64, usize)> {
    let first = *b.first()?;
    let len = first.leading_zeros() as usize + 1;
    if len > 8 || b.len() < len {
        return None;
    }
    let mut v = if keep { first as u64 } else { (first & (0xFFu16 >> len) as u8) as u64 };
    for x in &b[1..len] {
        v = (v << 8) | *x as u64;
    }
    Some((v, len))
}

/// Elementos hermanos de un bloque EBML: (id, contenido). Tamaño desconocido = hasta el final.
fn ebml(b: &[u8]) -> Vec<(u64, &[u8])> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let Some((id, a)) = vint(&b[i..], true) else { break };
        let Some((size, s)) = vint(&b[i + a..], false) else { break };
        let start = i + a + s;
        let unknown = size == (1u64 << (7 * s)) - 1;
        let end = if unknown { b.len() } else { (start + size as usize).min(b.len()) };
        out.push((id, &b[start.min(end)..end]));
        i = end;
    }
    out
}

fn uint(b: &[u8]) -> u64 {
    b.iter().fold(0u64, |a, x| (a << 8) | *x as u64)
}

fn float(b: &[u8]) -> f64 {
    match b.len() {
        4 => f32::from_be_bytes([b[0], b[1], b[2], b[3]]) as f64,
        8 => f64::from_be_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]),
        _ => 0.0,
    }
}

fn mkv_codec(id: &str) -> String {
    match id {
        "V_MPEG4/ISO/AVC" => "H.264",
        "V_MPEGH/ISO/HEVC" => "H.265",
        "V_VP9" => "VP9",
        "V_VP8" => "VP8",
        "V_AV1" => "AV1",
        "A_AAC" => "AAC",
        "A_OPUS" => "Opus",
        "A_VORBIS" => "Vorbis",
        "A_FLAC" => "FLAC",
        "A_AC3" => "AC-3",
        "A_EAC3" => "E-AC-3",
        "A_MPEG/L3" => "MP3",
        s if s.starts_with("A_DTS") => "DTS",
        s => return s.trim_start_matches(['V', 'A']).trim_start_matches('_').to_string(),
    }
    .to_string()
}

fn mkv(path: &Path, v: &mut Vid) -> bool {
    let Ok(f) = std::fs::File::open(path) else { return false };
    let mut buf = Vec::new();
    if f.take(4 << 20).read_to_end(&mut buf).is_err() {
        return false;
    }
    let top = ebml(&buf);
    let Some(seg) = top.iter().find(|e| e.0 == 0x18538067) else { return false };
    let mut scale = 1_000_000.0;
    for (id, body) in ebml(seg.1) {
        match id {
            0x1549A966 => {
                for (i, b) in ebml(body) {
                    match i {
                        0x2AD7B1 => scale = uint(b) as f64,
                        0x4489 => v.dur = float(b),
                        _ => {}
                    }
                }
                v.dur = v.dur * scale / 1e9;
            }
            0x1654AE6B => {
                for (tid, tb) in ebml(body) {
                    if tid != 0xAE {
                        continue;
                    }
                    let (mut ty, mut codec, mut dd) = (0u64, String::new(), 0u64);
                    let (mut w, mut h, mut rate, mut ch) = (0, 0, 0.0, 0);
                    for (i, b) in ebml(tb) {
                        match i {
                            0x83 => ty = uint(b),
                            0x86 => codec = String::from_utf8_lossy(b).trim_end_matches('\0').to_string(),
                            0x23E383 => dd = uint(b),
                            0xE0 => {
                                for (j, x) in ebml(b) {
                                    match j {
                                        0xB0 => w = uint(x) as u32,
                                        0xBA => h = uint(x) as u32,
                                        _ => {}
                                    }
                                }
                            }
                            0xE1 => {
                                for (j, x) in ebml(b) {
                                    match j {
                                        0xB5 => rate = float(x),
                                        0x9F => ch = uint(x) as u32,
                                        _ => {}
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                    if ty == 1 && v.vcodec.is_empty() {
                        v.vcodec = mkv_codec(&codec);
                        (v.w, v.h) = (w, h);
                        if dd > 0 {
                            v.fps = 1e9 / dd as f64;
                        }
                    } else if ty == 2 && v.acodec.is_empty() {
                        v.acodec = mkv_codec(&codec);
                        v.rate = rate as u32;
                        v.ch = ch;
                    }
                }
            }
            0x1F43B675 => break,
            _ => {}
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ebml_and_boxes() {
        // elemento 0x4489 (Duration) float32 = 2.0 dentro de Info
        let b = [0x44, 0x89, 0x84, 0x40, 0x00, 0x00, 0x00];
        let e = ebml(&b);
        assert_eq!(e[0].0, 0x4489);
        assert_eq!(float(e[0].1), 2.0);
        let mut m = vec![0, 0, 0, 12, b'f', b'r', b'e', b'e', 1, 2, 3, 4];
        m.extend([0, 0, 0, 9, b'm', b'd', b'a', b't', 7]);
        let c = children(&m);
        assert_eq!(c.len(), 2);
        assert_eq!(c[1].1, &[7]);
    }
}
