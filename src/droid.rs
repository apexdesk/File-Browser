//! Audio y vídeo en Android sin bibliotecas añadidas: los códecs del sistema (`AMediaExtractor` +
//! `AMediaCodec`, con hardware si el móvil lo tiene) y la salida de sonido `AAudio`. Todo viene en
//! el NDK (`libmediandk`, `libaaudio`), así que el APK no crece.
use std::collections::VecDeque;
use std::ffi::{c_int, c_void, CStr};
use std::os::fd::AsRawFd;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

type Status = c_int;

#[repr(C)]
#[derive(Default, Clone, Copy)]
struct BufInfo {
    offset: i32,
    size: i32,
    pts: i64,
    flags: u32,
}

const EOS: u32 = 4;
const TRY_AGAIN: isize = -1;
const FORMAT_CHANGED: isize = -2;
const YUV_420_888: i32 = 0x23;
const SEEK_PREVIOUS_SYNC: c_int = 0;

#[link(name = "mediandk")]
unsafe extern "C" {
    fn AMediaExtractor_new() -> *mut c_void;
    fn AMediaExtractor_delete(e: *mut c_void) -> Status;
    fn AMediaExtractor_setDataSourceFd(e: *mut c_void, fd: c_int, offset: i64, length: i64) -> Status;
    fn AMediaExtractor_getTrackCount(e: *mut c_void) -> usize;
    fn AMediaExtractor_getTrackFormat(e: *mut c_void, i: usize) -> *mut c_void;
    fn AMediaExtractor_selectTrack(e: *mut c_void, i: usize) -> Status;
    fn AMediaExtractor_readSampleData(e: *mut c_void, buf: *mut u8, cap: usize) -> isize;
    fn AMediaExtractor_getSampleTime(e: *mut c_void) -> i64;
    fn AMediaExtractor_getSampleTrackIndex(e: *mut c_void) -> c_int;
    fn AMediaExtractor_advance(e: *mut c_void) -> bool;
    fn AMediaExtractor_seekTo(e: *mut c_void, us: i64, mode: c_int) -> Status;

    fn AMediaFormat_delete(f: *mut c_void) -> Status;
    fn AMediaFormat_setInt32(f: *mut c_void, name: *const std::ffi::c_char, v: i32);
    fn AMediaFormat_getInt32(f: *mut c_void, name: *const std::ffi::c_char, out: *mut i32) -> bool;
    fn AMediaFormat_getInt64(f: *mut c_void, name: *const std::ffi::c_char, out: *mut i64) -> bool;
    fn AMediaFormat_getString(f: *mut c_void, name: *const std::ffi::c_char, out: *mut *const std::ffi::c_char) -> bool;

    fn AMediaCodec_createDecoderByType(mime: *const std::ffi::c_char) -> *mut c_void;
    fn AMediaCodec_delete(c: *mut c_void) -> Status;
    fn AMediaCodec_configure(c: *mut c_void, fmt: *const c_void, surface: *mut c_void, crypto: *mut c_void, flags: u32) -> Status;
    fn AMediaCodec_start(c: *mut c_void) -> Status;
    fn AMediaCodec_stop(c: *mut c_void) -> Status;
    fn AMediaCodec_getInputBuffer(c: *mut c_void, idx: usize, size: *mut usize) -> *mut u8;
    fn AMediaCodec_getOutputBuffer(c: *mut c_void, idx: usize, size: *mut usize) -> *mut u8;
    fn AMediaCodec_dequeueInputBuffer(c: *mut c_void, timeout_us: i64) -> isize;
    fn AMediaCodec_queueInputBuffer(c: *mut c_void, idx: usize, offset: i64, size: usize, time: u64, flags: u32) -> Status;
    fn AMediaCodec_dequeueOutputBuffer(c: *mut c_void, info: *mut BufInfo, timeout_us: i64) -> isize;
    fn AMediaCodec_getOutputFormat(c: *mut c_void) -> *mut c_void;
    fn AMediaCodec_releaseOutputBuffer(c: *mut c_void, idx: usize, render: bool) -> Status;

    fn AImageReader_new(w: i32, h: i32, format: i32, max_images: i32, out: *mut *mut c_void) -> Status;
    fn AImageReader_delete(r: *mut c_void);
    fn AImageReader_getWindow(r: *mut c_void, out: *mut *mut c_void) -> Status;
    fn AImageReader_acquireLatestImage(r: *mut c_void, out: *mut *mut c_void) -> Status;
    fn AImage_delete(i: *mut c_void);
    fn AImage_getWidth(i: *const c_void, out: *mut i32) -> Status;
    fn AImage_getHeight(i: *const c_void, out: *mut i32) -> Status;
    fn AImage_getPlaneData(i: *const c_void, plane: c_int, data: *mut *mut u8, len: *mut c_int) -> Status;
    fn AImage_getPlaneRowStride(i: *const c_void, plane: c_int, out: *mut i32) -> Status;
    fn AImage_getPlanePixelStride(i: *const c_void, plane: c_int, out: *mut i32) -> Status;
}

#[link(name = "aaudio")]
unsafe extern "C" {
    fn AAudio_createStreamBuilder(b: *mut *mut c_void) -> c_int;
    fn AAudioStreamBuilder_delete(b: *mut c_void) -> c_int;
    fn AAudioStreamBuilder_setDirection(b: *mut c_void, d: c_int);
    fn AAudioStreamBuilder_setFormat(b: *mut c_void, f: c_int);
    fn AAudioStreamBuilder_setChannelCount(b: *mut c_void, n: i32);
    fn AAudioStreamBuilder_setSampleRate(b: *mut c_void, r: i32);
    fn AAudioStreamBuilder_setPerformanceMode(b: *mut c_void, m: c_int);
    fn AAudioStreamBuilder_setDataCallback(b: *mut c_void, cb: extern "C" fn(*mut c_void, *mut c_void, *mut c_void, i32) -> c_int, user: *mut c_void);
    fn AAudioStreamBuilder_openStream(b: *mut c_void, out: *mut *mut c_void) -> c_int;
    fn AAudioStream_requestStart(s: *mut c_void) -> c_int;
    fn AAudioStream_requestStop(s: *mut c_void) -> c_int;
    fn AAudioStream_close(s: *mut c_void) -> c_int;
    fn AAudioStream_getSampleRate(s: *mut c_void) -> i32;
    fn AAudioStream_getChannelCount(s: *mut c_void) -> i32;
}

// ---------------------------------------------------------------------------
// Salida de sonido
// ---------------------------------------------------------------------------

pub type Ring = Arc<Mutex<VecDeque<f32>>>;

struct CbState {
    ring: Ring,
    played: Arc<AtomicU64>,
    ch: usize,
}

extern "C" fn audio_cb(_s: *mut c_void, user: *mut c_void, data: *mut c_void, frames: i32) -> c_int {
    // SAFETY: `user` es el `CbState` que `Sink` mantiene vivo hasta cerrar el flujo.
    let st = unsafe { &*(user as *const CbState) };
    let out = unsafe { std::slice::from_raw_parts_mut(data as *mut f32, frames.max(0) as usize * st.ch) };
    let mut n = 0;
    if let Ok(mut q) = st.ring.try_lock() {
        n = out.len().min(q.len());
        for (o, s) in out.iter_mut().zip(q.drain(..n)) {
            *o = s;
        }
    }
    for o in &mut out[n..] {
        *o = 0.0;
    }
    st.played.fetch_add((n / st.ch.max(1)) as u64, Ordering::Relaxed);
    0 // seguir
}

/// Flujo de salida AAudio que consume `ring` (f32 entrelazado). `played` cuenta los fotogramas
/// realmente entregados.
pub struct Sink {
    stream: *mut c_void,
    _state: Box<CbState>,
    pub rate: u32,
    pub ch: usize,
}

impl Sink {
    /// `rate` 0 = la nativa del dispositivo.
    pub fn open(rate: u32, ch: usize, ring: Ring, played: Arc<AtomicU64>) -> Result<Sink, String> {
        let mut state = Box::new(CbState { ring, played, ch });
        unsafe {
            let mut b = std::ptr::null_mut();
            if AAudio_createStreamBuilder(&mut b) != 0 || b.is_null() {
                return Err("AAudio no disponible".into());
            }
            AAudioStreamBuilder_setDirection(b, 0);
            AAudioStreamBuilder_setFormat(b, 2); // PCM_FLOAT
            AAudioStreamBuilder_setChannelCount(b, ch as i32);
            if rate > 0 {
                AAudioStreamBuilder_setSampleRate(b, rate as i32);
            }
            AAudioStreamBuilder_setPerformanceMode(b, 10); // NONE: menos consumo
            AAudioStreamBuilder_setDataCallback(b, audio_cb, &mut *state as *mut CbState as *mut c_void);
            let mut s = std::ptr::null_mut();
            let r = AAudioStreamBuilder_openStream(b, &mut s);
            AAudioStreamBuilder_delete(b);
            if r != 0 || s.is_null() {
                return Err(format!("no se pudo abrir la salida de audio ({r})"));
            }
            let (rate, got_ch) = (AAudioStream_getSampleRate(s).max(1) as u32, AAudioStream_getChannelCount(s).max(1) as usize);
            if got_ch != ch {
                AAudioStream_close(s);
                return Err("canales de salida distintos a los pedidos".into());
            }
            if AAudioStream_requestStart(s) != 0 {
                AAudioStream_close(s);
                return Err("no se pudo iniciar el audio".into());
            }
            Ok(Sink { stream: s, _state: state, rate, ch })
        }
    }
}

impl Drop for Sink {
    fn drop(&mut self) {
        unsafe {
            AAudioStream_requestStop(self.stream);
            AAudioStream_close(self.stream);
        }
    }
}

// ---------------------------------------------------------------------------
// Códecs
// ---------------------------------------------------------------------------

struct Extractor(*mut c_void);
impl Drop for Extractor {
    fn drop(&mut self) {
        unsafe { AMediaExtractor_delete(self.0) };
    }
}

struct Codec(*mut c_void);
impl Drop for Codec {
    fn drop(&mut self) {
        unsafe {
            AMediaCodec_stop(self.0);
            AMediaCodec_delete(self.0);
        }
    }
}

struct Reader(*mut c_void);
impl Drop for Reader {
    fn drop(&mut self) {
        unsafe { AImageReader_delete(self.0) };
    }
}

struct Fmt(*mut c_void);
impl Fmt {
    fn i32(&self, k: &CStr) -> Option<i32> {
        let mut v = 0;
        unsafe { AMediaFormat_getInt32(self.0, k.as_ptr(), &mut v) }.then_some(v)
    }
    fn i64(&self, k: &CStr) -> Option<i64> {
        let mut v = 0;
        unsafe { AMediaFormat_getInt64(self.0, k.as_ptr(), &mut v) }.then_some(v)
    }
    fn string(&self, k: &CStr) -> Option<String> {
        let mut p = std::ptr::null();
        if unsafe { AMediaFormat_getString(self.0, k.as_ptr(), &mut p) } && !p.is_null() {
            Some(unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned())
        } else {
            None
        }
    }
}
impl Drop for Fmt {
    fn drop(&mut self) {
        unsafe { AMediaFormat_delete(self.0) };
    }
}

struct Track {
    index: usize,
    mime: std::ffi::CString,
    fmt: Fmt,
}

fn open_extractor(file: &std::fs::File) -> Result<(Extractor, Option<Track>, Option<Track>, i64), String> {
    let len = file.metadata().map_err(|e| e.to_string())?.len() as i64;
    unsafe {
        let ex = Extractor(AMediaExtractor_new());
        if ex.0.is_null() {
            return Err("sin extractor de medios".into());
        }
        if AMediaExtractor_setDataSourceFd(ex.0, file.as_raw_fd(), 0, len) != 0 {
            return Err("formato no admitido por el sistema".into());
        }
        let (mut video, mut audio, mut dur) = (None, None, 0i64);
        for i in 0..AMediaExtractor_getTrackCount(ex.0) {
            let f = Fmt(AMediaExtractor_getTrackFormat(ex.0, i));
            if f.0.is_null() {
                continue;
            }
            let Some(mime) = f.string(c"mime") else { continue };
            dur = dur.max(f.i64(c"durationUs").unwrap_or(0));
            let Ok(cm) = std::ffi::CString::new(mime.clone()) else { continue };
            if mime.starts_with("video/") && video.is_none() {
                video = Some(Track { index: i, mime: cm, fmt: f });
            } else if mime.starts_with("audio/") && audio.is_none() {
                audio = Some(Track { index: i, mime: cm, fmt: f });
            }
        }
        Ok((ex, video, audio, dur))
    }
}

pub struct Info {
    pub duration: f64,
    pub has_video: bool,
    pub has_audio: bool,
}

/// Duración y pistas, sin decodificar nada.
pub fn probe(path: &Path) -> Option<Info> {
    let file = std::fs::File::open(path).ok()?;
    let (_ex, v, a, d) = open_extractor(&file).ok()?;
    Some(Info { duration: d as f64 / 1e6, has_video: v.is_some(), has_audio: a.is_some() })
}

fn make_codec(t: &Track, surface: *mut c_void) -> Result<Codec, String> {
    unsafe {
        let c = AMediaCodec_createDecoderByType(t.mime.as_ptr());
        if c.is_null() {
            return Err(format!("el móvil no tiene decodificador para {}", t.mime.to_string_lossy()));
        }
        let c = Codec(c);
        if AMediaCodec_configure(c.0, t.fmt.0, surface, std::ptr::null_mut(), 0) != 0 || AMediaCodec_start(c.0) != 0 {
            return Err(format!("no se pudo iniciar el decodificador de {}", t.mime.to_string_lossy()));
        }
        Ok(c)
    }
}

/// Convierte una imagen YUV_420_888 a RGBA reduciéndola a como mucho `max_w` de ancho.
unsafe fn image_to_rgba(img: *mut c_void, max_w: usize) -> Option<(usize, usize, Vec<u8>)> {
    unsafe {
        let (mut w, mut h) = (0i32, 0i32);
        AImage_getWidth(img, &mut w);
        AImage_getHeight(img, &mut h);
        let (w, h) = (w.max(0) as usize, h.max(0) as usize);
        if w < 2 || h < 2 {
            return None;
        }
        let mut pl = [(std::ptr::null_mut::<u8>(), 0usize, 0usize, 0usize); 3]; // datos, longitud, fila, píxel
        for (i, p) in pl.iter_mut().enumerate() {
            let (mut d, mut l, mut rs, mut ps) = (std::ptr::null_mut(), 0, 0, 0);
            if AImage_getPlaneData(img, i as c_int, &mut d, &mut l) != 0
                || AImage_getPlaneRowStride(img, i as c_int, &mut rs) != 0
                || AImage_getPlanePixelStride(img, i as c_int, &mut ps) != 0
                || d.is_null()
            {
                return None;
            }
            *p = (d, l.max(0) as usize, rs.max(0) as usize, ps.max(1) as usize);
        }
        convert(pl, w, h, max_w)
    }
}

type Plane = (*mut u8, usize, usize, usize); // datos, longitud, bytes por fila, bytes por píxel

/// YUV 4:2:0 (tres planos) a RGBA, reduciendo a como mucho `max_w` de ancho.
unsafe fn convert(pl: [Plane; 3], w: usize, h: usize, max_w: usize) -> Option<(usize, usize, Vec<u8>)> {
    unsafe {
        if w < 2 || h < 2 {
            return None;
        }
        // que ningún acceso se salga del búfer
        let (cw, ch) = (w / 2, h / 2);
        let need = |p: usize, rows: usize, cols: usize| (rows - 1) * pl[p].2 + (cols - 1) * pl[p].3 < pl[p].1;
        if !need(0, h, w) || !need(1, ch, cw) || !need(2, ch, cw) {
            return None;
        }
        let step = w.div_ceil(max_w.max(2));
        let (ow, oh) = (w / step, h / step);
        let hd = w >= 1280;
        let mut out = Vec::with_capacity(ow * oh * 4);
        for y in 0..oh {
            let sy = y * step;
            for x in 0..ow {
                let sx = x * step;
                let yy = *pl[0].0.add(sy * pl[0].2 + sx * pl[0].3) as i32 - 16;
                let u = *pl[1].0.add((sy / 2).min(ch - 1) * pl[1].2 + (sx / 2).min(cw - 1) * pl[1].3) as i32 - 128;
                let v = *pl[2].0.add((sy / 2).min(ch - 1) * pl[2].2 + (sx / 2).min(cw - 1) * pl[2].3) as i32 - 128;
                let c = 298 * yy.max(0);
                let (r, g, b) = if hd {
                    ((c + 459 * v + 128) >> 8, (c - 55 * u - 136 * v + 128) >> 8, (c + 541 * u + 128) >> 8)
                } else {
                    ((c + 409 * v + 128) >> 8, (c - 100 * u - 208 * v + 128) >> 8, (c + 516 * u + 128) >> 8)
                };
                out.extend_from_slice(&[r.clamp(0, 255) as u8, g.clamp(0, 255) as u8, b.clamp(0, 255) as u8, 255]);
            }
        }
        Some((ow, oh, out))
    }
}

/// Fotograma en un búfer de salida del códec (sin superficie): NV12 o I420 con el stride y las
/// alturas que declara el formato de salida.
unsafe fn buffer_to_rgba(p: *mut u8, len: usize, f: &Fmt, max_w: usize) -> Result<Option<(usize, usize, Vec<u8>)>, String> {
    unsafe {
        let w = f.i32(c"width").unwrap_or(0).max(0) as usize;
        let h = f.i32(c"height").unwrap_or(0).max(0) as usize;
        let stride = f.i32(c"stride").unwrap_or(w as i32).max(w as i32) as usize;
        let sh = f.i32(c"slice-height").unwrap_or(h as i32).max(h as i32) as usize;
        let cf = f.i32(c"color-format").unwrap_or(21);
        let (left, top) = (f.i32(c"crop-left").unwrap_or(0).max(0) as usize, f.i32(c"crop-top").unwrap_or(0).max(0) as usize);
        let cw = f.i32(c"crop-right").map(|r| r as usize + 1).unwrap_or(w).saturating_sub(left);
        let chh = f.i32(c"crop-bottom").map(|r| r as usize + 1).unwrap_or(h).saturating_sub(top);
        if w == 0 || h == 0 || p.is_null() || len < stride * sh {
            return Ok(None);
        }
        let ybase = stride * sh;
        let y: Plane = (p.add(top * stride + left), len - (top * stride + left), stride, 1);
        let (u, v): (Plane, Plane) = match cf {
            // semiplanar (NV12): UVUV…; 0x7F420888 («flexible») casi siempre llega así
            21 | 0x7F42_0888 => {
                let (off, rs) = (ybase + (top / 2) * stride + (left / 2) * 2, stride);
                ((p.add(off), len.saturating_sub(off), rs, 2), (p.add(off + 1), len.saturating_sub(off + 1), rs, 2))
            }
            // planar (I420)
            19 => {
                let cs = stride / 2;
                let uoff = ybase + (top / 2) * cs + left / 2;
                let voff = ybase + cs * (sh / 2) + (top / 2) * cs + left / 2;
                ((p.add(uoff), len.saturating_sub(uoff), cs, 1), (p.add(voff), len.saturating_sub(voff), cs, 1))
            }
            other => return Err(format!("formato de color de vídeo no admitido ({other:#x})")),
        };
        Ok(convert([y, u, v], cw, chh, max_w))
    }
}

/// Gira un RGBA `deg` grados (90, 180, 270) en sentido horario.
fn rotate(w: usize, h: usize, px: Vec<u8>, deg: i32) -> (usize, usize, Vec<u8>) {
    let mut out = vec![0u8; px.len()];
    let (nw, nh) = if deg == 180 { (w, h) } else { (h, w) };
    for y in 0..h {
        for x in 0..w {
            let (nx, ny) = match deg {
                90 => (h - 1 - y, x),
                180 => (w - 1 - x, h - 1 - y),
                _ => (y, w - 1 - x),
            };
            let (s, d) = ((y * w + x) * 4, (ny * nw + nx) * 4);
            out[d..d + 4].copy_from_slice(&px[s..s + 4]);
        }
    }
    (nw, nh, out)
}

pub struct Opts {
    /// reproducir también el audio del vídeo
    pub audio: bool,
    /// al ritmo del reloj (reproductor) o lo más rápido posible (miniatura)
    pub realtime: bool,
    pub max_w: usize,
    /// salida en búfer en vez de superficie (respaldo si la superficie no entrega imágenes)
    pub bytes: bool,
}

/// Error que pide repetir con `Opts::bytes`.
pub const NO_SURFACE: &str = "sin imágenes de la superficie";

/// Decodifica `path` desde `start_us`. Cada fotograma pasa a `on_frame` (devuelve `false` para parar).
/// `Ok(true)` si se llegó al final del archivo.
pub fn run(path: &Path, start_us: i64, o: &Opts, alive: &dyn Fn() -> bool, on_frame: &mut dyn FnMut(usize, usize, Vec<u8>) -> bool) -> Result<bool, String> {
    let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let (ex, video, audio, _dur) = open_extractor(&file)?;
    let audio = if o.audio { audio } else { None };
    if video.is_none() && audio.is_none() {
        return Err("sin pistas reproducibles".into());
    }
    unsafe {
        let mut reader = None;
        let mut vcodec = None;
        if let Some(v) = &video {
            AMediaExtractor_selectTrack(ex.0, v.index);
            let w = v.fmt.i32(c"width").unwrap_or(0);
            let h = v.fmt.i32(c"height").unwrap_or(0);
            if w <= 0 || h <= 0 || w > 8192 || h > 8192 {
                return Err("tamaño de vídeo no válido".into());
            }
            if o.bytes {
                // salida en búfer: se pide un formato plano/semiplano de 8 bits
                AMediaFormat_setInt32(v.fmt.0, c"color-format".as_ptr(), 0x7F42_0888);
                vcodec = Some(make_codec(v, std::ptr::null_mut())?);
            } else {
                let mut r = std::ptr::null_mut();
                if AImageReader_new(w, h, YUV_420_888, 3, &mut r) != 0 || r.is_null() {
                    return Err("no se pudo crear el lector de imágenes".into());
                }
                let r = Reader(r);
                let mut win = std::ptr::null_mut();
                if AImageReader_getWindow(r.0, &mut win) != 0 || win.is_null() {
                    return Err("sin superficie de salida".into());
                }
                vcodec = Some(make_codec(v, win)?);
                reader = Some(r);
            }
        }
        let mut acodec = None;
        if let Some(a) = &audio {
            AMediaExtractor_selectTrack(ex.0, a.index);
            acodec = Some(make_codec(a, std::ptr::null_mut())?);
        }
        if start_us > 0 {
            AMediaExtractor_seekTo(ex.0, start_us, SEEK_PREVIOUS_SYNC);
        }
        let rot = video.as_ref().and_then(|v| v.fmt.i32(c"rotation-degrees")).unwrap_or(0).rem_euclid(360);
        let vidx = video.as_ref().map(|v| v.index as c_int).unwrap_or(-2);
        let aidx = audio.as_ref().map(|a| a.index as c_int).unwrap_or(-2);

        let ring: Ring = Arc::new(Mutex::new(VecDeque::new()));
        let played = Arc::new(AtomicU64::new(0));
        let mut sink: Option<Sink> = None;
        let (mut a_rate, mut a_ch, mut a_float) = (0u32, 0usize, false);

        let (mut file_end, mut v_eos, mut a_eos) = (false, false, false);
        let (mut v_done, mut a_done) = (video.is_none(), audio.is_none());
        let mut audio_off = false;
        let mut pending: Option<(usize, i64, BufInfo)> = None; // fotograma de vídeo a la espera de su hora
        let mut anchor: Option<(Instant, i64)> = None; // reloj sin audio: (instante, pts)
        let mut shown_any = false;
        let mut released = 0u32;
        let mut idle_rounds = 0u32;

        let clock = |sink: &Option<Sink>, anchor: &Option<(Instant, i64)>| -> i64 {
            if let Some(s) = sink {
                start_us + (played.load(Ordering::Relaxed) as f64 / s.rate as f64 * 1e6) as i64
            } else if let Some((t, p)) = anchor {
                p + t.elapsed().as_micros() as i64
            } else {
                start_us
            }
        };

        while alive() && !(v_done && a_done) {
            let mut busy = false;

            // --- entrada: la siguiente muestra va al códec de su pista
            for _ in 0..8 {
                if file_end {
                    break;
                }
                let t = AMediaExtractor_getSampleTrackIndex(ex.0);
                let codec = if t == vidx && vidx >= 0 {
                    vcodec.as_ref()
                } else if t == aidx && aidx >= 0 {
                    acodec.as_ref()
                } else {
                    None
                };
                if t < 0 {
                    file_end = true;
                    break;
                }
                let codec = if audio_off && t == aidx { None } else { codec };
                let Some(c) = codec else {
                    AMediaExtractor_advance(ex.0); // pista que no usamos
                    continue;
                };
                // con el audio ya en cola de sobra se espera
                if t == aidx && ring.lock().map(|q| q.len()).unwrap_or(0) > a_rate as usize * a_ch.max(1) {
                    break;
                }
                let i = AMediaCodec_dequeueInputBuffer(c.0, 0);
                if i < 0 {
                    break;
                }
                let mut cap = 0usize;
                let buf = AMediaCodec_getInputBuffer(c.0, i as usize, &mut cap);
                if buf.is_null() {
                    break;
                }
                let n = AMediaExtractor_readSampleData(ex.0, buf, cap);
                if n < 0 {
                    // se devuelve el búfer vacío y se da el archivo por terminado
                    AMediaCodec_queueInputBuffer(c.0, i as usize, 0, 0, 0, 0);
                    file_end = true;
                    break;
                }
                let pts = AMediaExtractor_getSampleTime(ex.0).max(0) as u64;
                AMediaCodec_queueInputBuffer(c.0, i as usize, 0, n as usize, pts, 0);
                AMediaExtractor_advance(ex.0);
                busy = true;
            }

            // fin del archivo: aviso de fin a cada códec en cuanto tenga un búfer libre
            if file_end {
                for (c, sent) in [(vcodec.as_ref(), &mut v_eos), (acodec.as_ref(), &mut a_eos)] {
                    if let (Some(c), false) = (c, *sent) {
                        let i = AMediaCodec_dequeueInputBuffer(c.0, 0);
                        if i >= 0 {
                            AMediaCodec_queueInputBuffer(c.0, i as usize, 0, 0, 0, EOS);
                            *sent = true;
                        }
                    }
                }
            }

            // --- salida de audio
            if let Some(c) = &acodec {
                if !a_done && !audio_off {
                    let mut info = BufInfo::default();
                    let i = AMediaCodec_dequeueOutputBuffer(c.0, &mut info, 0);
                    if i == FORMAT_CHANGED || (i >= 0 && sink.is_none()) {
                        let f = Fmt(AMediaCodec_getOutputFormat(c.0));
                        if !f.0.is_null() {
                            a_rate = f.i32(c"sample-rate").unwrap_or(44100).max(8000) as u32;
                            a_ch = (f.i32(c"channel-count").unwrap_or(2).max(1) as usize).min(2);
                            a_float = f.i32(c"pcm-encoding") == Some(4);
                        }
                        if sink.is_none() && a_rate > 0 && !audio_off {
                            match Sink::open(a_rate, a_ch, ring.clone(), played.clone()) {
                                Ok(s) => sink = Some(s),
                                // sin salida de sonido el vídeo sigue, mudo
                                Err(_) => {
                                    audio_off = true;
                                    a_done = true;
                                }
                            }
                        }
                    }
                    if i >= 0 {
                        busy = true;
                        let (src_ch, bytes) = {
                            let f = Fmt(AMediaCodec_getOutputFormat(c.0));
                            let ch = if f.0.is_null() { a_ch } else { f.i32(c"channel-count").unwrap_or(a_ch as i32).max(1) as usize };
                            let mut cap = 0usize;
                            let p = AMediaCodec_getOutputBuffer(c.0, i as usize, &mut cap);
                            let (off, sz) = (info.offset.max(0) as usize, info.size.max(0) as usize);
                            (ch, if p.is_null() || off + sz > cap { &[][..] } else { std::slice::from_raw_parts(p.add(off), sz) })
                        };
                        // muestras anteriores al punto pedido (el salto cae en un punto previo) se descartan
                        let skip_frames = if info.pts < start_us && a_rate > 0 { ((start_us - info.pts) as f64 / 1e6 * a_rate as f64) as usize } else { 0 };
                        let samples: Vec<f32> = if a_float {
                            bytes.chunks_exact(4).map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect()
                        } else {
                            bytes.chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0).collect()
                        };
                        if let Ok(mut q) = ring.lock() {
                            for fr in samples.chunks_exact(src_ch).skip(skip_frames) {
                                for o in 0..a_ch {
                                    q.push_back(if src_ch == a_ch {
                                        fr[o]
                                    } else if src_ch == 1 {
                                        fr[0]
                                    } else if a_ch == 1 {
                                        fr.iter().sum::<f32>() / src_ch as f32
                                    } else {
                                        fr[o.min(src_ch - 1)]
                                    });
                                }
                            }
                        }
                        AMediaCodec_releaseOutputBuffer(c.0, i as usize, false);
                        if info.flags & EOS != 0 {
                            a_done = true;
                        }
                    }
                }
            }

            // --- salida de vídeo
            if let Some(c) = &vcodec {
                if !v_done {
                    if pending.is_none() {
                        let mut info = BufInfo::default();
                        let i = AMediaCodec_dequeueOutputBuffer(c.0, &mut info, 0);
                        if i >= 0 {
                            busy = true;
                            if info.flags & EOS != 0 && info.size == 0 {
                                AMediaCodec_releaseOutputBuffer(c.0, i as usize, false);
                                v_done = true;
                            } else if info.pts < start_us - 20_000 {
                                AMediaCodec_releaseOutputBuffer(c.0, i as usize, false); // antes del punto pedido
                            } else {
                                pending = Some((i as usize, info.pts, info));
                            }
                        } else if i != TRY_AGAIN && i != FORMAT_CHANGED && i != -3 {
                            return Err(format!("error del decodificador de vídeo ({i})"));
                        }
                    }
                    if let Some((i, pts, info)) = pending {
                        let due = !o.realtime || (sink.is_none() && anchor.is_none()) || pts <= clock(&sink, &anchor) + 8_000;
                        // sin audio todavía en marcha, el primer fotograma sale de inmediato
                        if due {
                            pending = None;
                            busy = true;
                            if !shown_any && sink.is_none() {
                                anchor = Some((Instant::now(), pts));
                            }
                            let mut frame = None;
                            if let Some(r) = &reader {
                                AMediaCodec_releaseOutputBuffer(c.0, i, true);
                                released += 1;
                                // el fotograma tarda un instante en llegar al lector
                                let mut img = std::ptr::null_mut();
                                for _ in 0..40 {
                                    if AImageReader_acquireLatestImage(r.0, &mut img) == 0 && !img.is_null() {
                                        break;
                                    }
                                    img = std::ptr::null_mut();
                                    std::thread::sleep(Duration::from_millis(2));
                                }
                                if !img.is_null() {
                                    frame = image_to_rgba(img, o.max_w);
                                    AImage_delete(img);
                                }
                                if !shown_any && frame.is_none() && released >= 8 {
                                    return Err(NO_SURFACE.into());
                                }
                            } else {
                                let mut cap = 0usize;
                                let p = AMediaCodec_getOutputBuffer(c.0, i, &mut cap);
                                let (off, sz) = (info.offset.max(0) as usize, info.size.max(0) as usize);
                                let f = Fmt(AMediaCodec_getOutputFormat(c.0));
                                let r = if p.is_null() || f.0.is_null() || off + sz > cap { Ok(None) } else { buffer_to_rgba(p.add(off), sz, &f, o.max_w) };
                                AMediaCodec_releaseOutputBuffer(c.0, i, false);
                                frame = r?;
                            }
                            if let Some((w, h, px)) = frame {
                                let (w, h, px) = if matches!(rot, 90 | 180 | 270) { rotate(w, h, px, rot) } else { (w, h, px) };
                                shown_any = true;
                                if !on_frame(w, h, px) {
                                    return Ok(false);
                                }
                            }
                        }
                    }
                }
            }

            // el audio sin pista de vídeo termina cuando se ha vaciado la cola
            if a_done && !v_done && video.is_none() {
                v_done = true;
            }
            if v_done && a_done {
                // deja sonar lo que queda en cola
                while alive() && ring.lock().map(|q| !q.is_empty()).unwrap_or(false) && o.realtime {
                    std::thread::sleep(Duration::from_millis(20));
                }
                break;
            }
            if busy {
                idle_rounds = 0;
            } else {
                idle_rounds += 1;
                if !o.realtime && idle_rounds > 1500 {
                    return Err("tiempo agotado decodificando".into());
                }
                std::thread::sleep(Duration::from_millis(3));
            }
        }
        Ok(alive())
    }
}

/// Un fotograma representativo (a ~10 % de la duración, o el primero) como RGBA, para las miniaturas.
pub fn frame(path: &Path, max: u32) -> Option<crate::thumbs::Decoded> {
    let info = probe(path)?;
    if !info.has_video {
        return None;
    }
    let grab = |start_us: i64, bytes: bool| -> Option<crate::thumbs::Decoded> {
        let o = Opts { audio: false, realtime: false, max_w: max.max(64) as usize, bytes };
        let mut got = None;
        let t0 = Instant::now();
        let alive = || t0.elapsed() < Duration::from_secs(20);
        let _ = run(path, start_us, &o, &alive, &mut |w, h, px| {
            got = Some(crate::thumbs::Decoded { w, h, rgba: px });
            false
        });
        got
    };
    let start = ((info.duration * 0.1).min(30.0) * 1e6) as i64;
    grab(start, false).or_else(|| grab(start, true)).or_else(|| if start > 0 { grab(0, true) } else { None })
}
