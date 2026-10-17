//! Reproductor de audio integrado: `symphonia` decodifica (MP3, WAV, OGG/Vorbis, FLAC, AAC/M4A, ALAC)
//! en un hilo y `cpal` saca el sonido por el dispositivo predeterminado. Sin dependencias externas.
//! En Android todavía no hay salida de sonido.
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use symphonia::core::codecs::{CODEC_TYPE_NULL, DecoderOptions};
use symphonia::core::errors::Error as SymErr;
use symphonia::core::formats::{FormatOptions, FormatReader, SeekMode, SeekTo};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::{MetadataOptions, StandardTagKey};
use symphonia::core::probe::Hint;
use symphonia::core::units::Time;

pub fn supported() -> bool {
    cfg!(not(target_os = "android"))
}

/// Datos de cabecera: duración, etiquetas y carátula.
#[derive(Default)]
struct Meta {
    duration: f64,
    title: String,
    artist: String,
    album: String,
    cover: Option<egui::ColorImage>,
}

fn open_format(path: &Path) -> Result<Box<dyn FormatReader>, String> {
    let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(e) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(e);
    }
    let probed = symphonia::default::get_probe()
        .format(&hint, mss, &FormatOptions::default(), &MetadataOptions::default())
        .map_err(|e| e.to_string())?;
    Ok(probed.format)
}

fn cover_image(data: &[u8]) -> Option<egui::ColorImage> {
    if data.len() > 16 * 1024 * 1024 {
        return None;
    }
    let img = if data.starts_with(&[0xFF, 0xD8]) {
        let mut d = jpeg_decoder::Decoder::new(std::io::Cursor::new(data));
        let px = d.decode().ok()?;
        let i = d.info()?;
        let rgb: Vec<u8> = match i.pixel_format {
            jpeg_decoder::PixelFormat::RGB24 => px,
            jpeg_decoder::PixelFormat::L8 => px.iter().flat_map(|&v| [v, v, v]).collect(),
            _ => return None,
        };
        image::DynamicImage::ImageRgb8(image::RgbImage::from_raw(i.width as u32, i.height as u32, rgb)?)
    } else {
        image::load_from_memory(data).ok()?
    };
    let img = if img.width() > 640 || img.height() > 640 { img.thumbnail(640, 640) } else { img };
    let rgba = img.into_rgba8();
    Some(egui::ColorImage::from_rgba_unmultiplied([rgba.width() as usize, rgba.height() as usize], rgba.as_raw()))
}

fn read_meta(path: &Path) -> Meta {
    let mut m = Meta::default();
    let Ok(file) = std::fs::File::open(path) else { return m };
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(e) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(e);
    }
    let Ok(mut probed) = symphonia::default::get_probe().format(&hint, mss, &FormatOptions::default(), &MetadataOptions::default()) else {
        return m;
    };
    if let Some(t) = probed.format.tracks().iter().find(|t| t.codec_params.codec != CODEC_TYPE_NULL) {
        let p = &t.codec_params;
        if let (Some(n), Some(sr)) = (p.n_frames, p.sample_rate) {
            if sr > 0 {
                m.duration = n as f64 / sr as f64;
            }
        }
    }
    let mut take = |rev: &symphonia::core::meta::MetadataRevision| {
        for tag in rev.tags() {
            let v = tag.value.to_string();
            match tag.std_key {
                Some(StandardTagKey::TrackTitle) if m.title.is_empty() => m.title = v,
                Some(StandardTagKey::Artist) | Some(StandardTagKey::AlbumArtist) if m.artist.is_empty() => m.artist = v,
                Some(StandardTagKey::Album) if m.album.is_empty() => m.album = v,
                _ => {}
            }
        }
        if m.cover.is_none() {
            if let Some(v) = rev.visuals().first() {
                m.cover = cover_image(&v.data);
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
    m
}

/// Estado de una sesión de reproducción (cada arranque o salto crea una nueva).
#[derive(Default)]
struct Session {
    /// fotogramas realmente entregados al dispositivo
    played: AtomicU64,
    rate: AtomicU32,
    ended: AtomicBool,
    error: Mutex<Option<String>>,
}

pub struct AudioPlayer {
    path: PathBuf,
    ctx: egui::Context,
    pub duration: f64,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub tex: Option<egui::TextureHandle>,
    pub error: Option<String>,
    meta: Arc<Mutex<Option<Meta>>>,
    epoch: Arc<AtomicU64>,
    sess: Arc<Session>,
    base: f64,
    started: Option<Instant>,
}

impl AudioPlayer {
    pub fn new(ctx: &egui::Context, path: &Path) -> AudioPlayer {
        let meta = Arc::new(Mutex::new(None));
        {
            let (meta, p, ctx) = (meta.clone(), path.to_path_buf(), ctx.clone());
            std::thread::spawn(move || {
                let m = read_meta(&p);
                *meta.lock().unwrap() = Some(m);
                ctx.request_repaint();
            });
        }
        AudioPlayer {
            path: path.to_path_buf(),
            ctx: ctx.clone(),
            duration: 0.0,
            title: String::new(),
            artist: String::new(),
            album: String::new(),
            tex: None,
            error: if supported() { None } else { Some("Android".into()) },
            meta,
            epoch: Arc::new(AtomicU64::new(0)),
            sess: Arc::new(Session::default()),
            base: 0.0,
            started: None,
        }
    }

    pub fn playing(&self) -> bool {
        self.started.is_some()
    }

    pub fn position(&self) -> f64 {
        let rate = self.sess.rate.load(Ordering::Relaxed);
        let p = if self.started.is_some() && rate > 0 { self.base + self.sess.played.load(Ordering::Relaxed) as f64 / rate as f64 } else { self.base };
        if self.duration > 0.0 { p.min(self.duration) } else { p }
    }

    fn stop(&mut self) {
        self.epoch.fetch_add(1, Ordering::SeqCst);
    }

    fn start(&mut self, pos: f64) {
        self.stop();
        self.error = None;
        let sess = Arc::new(Session::default());
        self.sess = sess.clone();
        let my = self.epoch.load(Ordering::SeqCst);
        let (epoch, path, ctx) = (self.epoch.clone(), self.path.clone(), self.ctx.clone());
        std::thread::spawn(move || {
            if let Err(e) = run(&path, pos, &sess, &epoch, my) {
                *sess.error.lock().unwrap() = Some(e);
            }
            sess.ended.store(true, Ordering::SeqCst);
            ctx.request_repaint();
        });
        self.base = pos;
        self.started = Some(Instant::now());
    }

    pub fn toggle(&mut self) {
        if !supported() {
            return;
        }
        if self.playing() {
            self.base = self.position();
            self.started = None;
            self.stop();
        } else {
            let at = if self.duration > 0.0 && self.base >= self.duration - 0.2 { 0.0 } else { self.base };
            self.start(at);
        }
    }

    pub fn seek_to(&mut self, secs: f64) {
        if !supported() {
            return;
        }
        let max = if self.duration > 0.5 { self.duration - 0.3 } else { f64::MAX };
        self.start(secs.clamp(0.0, max));
    }

    pub fn seek_by(&mut self, d: f64) {
        let p = self.position();
        self.seek_to(p + d);
    }

    /// Recoge las etiquetas y detecta el final de la pista o un error.
    pub fn poll(&mut self) {
        let m = self.meta.lock().unwrap().take();
        if let Some(m) = m {
            self.duration = m.duration;
            self.title = m.title;
            self.artist = m.artist;
            self.album = m.album;
            if let Some(img) = m.cover {
                self.tex = Some(self.ctx.load_texture("cover", img, egui::TextureOptions::LINEAR));
            }
        }
        if self.started.is_some() && self.sess.ended.load(Ordering::SeqCst) {
            if let Some(e) = self.sess.error.lock().unwrap().take() {
                self.error = Some(e);
                self.base = self.position();
            } else {
                self.base = if self.duration > 0.0 { self.duration } else { self.position() };
            }
            self.started = None;
        }
    }
}

impl Drop for AudioPlayer {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(target_os = "android")]
fn run(_path: &Path, _start: f64, _sess: &Session, _epoch: &AtomicU64, _my: u64) -> Result<(), String> {
    Err("sin salida de sonido en Android".into())
}

#[cfg(not(target_os = "android"))]
fn run(path: &Path, start: f64, sess: &Session, epoch: &AtomicU64, my: u64) -> Result<(), String> {
    use std::collections::VecDeque;
    use symphonia::core::audio::SampleBuffer;

    let alive = || epoch.load(Ordering::SeqCst) == my;
    let mut format = open_format(path)?;
    let track = format.tracks().iter().find(|t| t.codec_params.codec != CODEC_TYPE_NULL).ok_or("sin pista de audio")?;
    let (id, params) = (track.id, track.codec_params.clone());
    let mut decoder = symphonia::default::get_codecs().make(&params, &DecoderOptions::default()).map_err(|e| e.to_string())?;
    if start > 0.1 {
        let _ = format.seek(SeekMode::Coarse, SeekTo::Time { time: Time::from(start), track_id: Some(id) });
        decoder.reset();
    }

    // dispositivo de salida
    let (dev, cfg) = output_device()?;
    let (rate_out, ch_out) = (cfg.sample_rate().0, cfg.channels() as usize);
    let ring: Arc<Mutex<VecDeque<f32>>> = Arc::new(Mutex::new(VecDeque::new()));
    let played = Arc::new(AtomicU64::new(0));
    let stream = build_stream(&dev, &cfg, ring.clone(), played.clone())?;
    {
        use cpal::traits::StreamTrait;
        stream.play().map_err(|e| e.to_string())?;
    }
    sess.rate.store(rate_out, Ordering::Relaxed);
    let high = rate_out as usize * ch_out / 2; // ~0,5 s de audio en cola

    let mut sbuf: Option<SampleBuffer<f32>> = None;
    let (mut carry, mut phase): (Option<Vec<f32>>, f64) = (None, 0.0);
    let mut eof = false;
    while alive() {
        // mantiene `played` de la sesión al día con el contador del dispositivo
        sess.played.store(played.load(Ordering::Relaxed), Ordering::Relaxed);
        if ring.lock().map(|q| q.len()).unwrap_or(0) > high {
            std::thread::sleep(std::time::Duration::from_millis(15));
            continue;
        }
        let packet = match format.next_packet() {
            Ok(p) => p,
            Err(SymErr::ResetRequired) => break,
            Err(_) => {
                eof = true;
                break;
            }
        };
        if packet.track_id() != id {
            continue;
        }
        let buf = match decoder.decode(&packet) {
            Ok(b) => b,
            Err(SymErr::DecodeError(_)) => continue,
            Err(_) => break,
        };
        let spec = *buf.spec();
        let (rate_in, ch_in) = (spec.rate, spec.channels.count().max(1));
        if sbuf.is_none() {
            sbuf = Some(SampleBuffer::<f32>::new(buf.capacity() as u64, spec));
        }
        let sb = sbuf.as_mut().unwrap();
        sb.copy_interleaved_ref(buf);
        // canales de entrada -> canales de salida
        let src = sb.samples();
        let mut frames: Vec<f32> = Vec::with_capacity(src.len() / ch_in * ch_out + ch_out);
        if let Some(c) = carry.take() {
            frames.extend_from_slice(&c);
        }
        let had_carry = !frames.is_empty();
        for f in src.chunks_exact(ch_in) {
            for o in 0..ch_out {
                frames.push(map_channel(f, o, ch_out));
            }
        }
        let n_frames = frames.len() / ch_out;
        if n_frames == 0 {
            continue;
        }
        let mut out: Vec<f32> = Vec::new();
        if rate_in == rate_out {
            let skip = if had_carry { ch_out } else { 0 };
            out.extend_from_slice(&frames[skip..]);
            carry = None;
        } else {
            // remuestreo lineal; el último fotograma pasa al siguiente paquete
            let step = rate_in as f64 / rate_out as f64;
            if !had_carry {
                phase = 0.0;
            }
            while (phase.floor() as usize) + 1 < n_frames {
                let i = phase.floor() as usize;
                let fr = (phase - i as f64) as f32;
                for c in 0..ch_out {
                    let a = frames[i * ch_out + c];
                    let b = frames[(i + 1) * ch_out + c];
                    out.push(a + (b - a) * fr);
                }
                phase += step;
            }
            carry = Some(frames[(n_frames - 1) * ch_out..].to_vec());
            phase -= (n_frames - 1) as f64;
        }
        if let Ok(mut q) = ring.lock() {
            q.extend(out);
        }
    }
    // vaciar la cola antes de dar la pista por terminada
    if eof {
        while alive() && ring.lock().map(|q| !q.is_empty()).unwrap_or(false) {
            sess.played.store(played.load(Ordering::Relaxed), Ordering::Relaxed);
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        if alive() {
            std::thread::sleep(std::time::Duration::from_millis(150));
        }
    }
    sess.played.store(played.load(Ordering::Relaxed), Ordering::Relaxed);
    drop(stream);
    Ok(())
}

/// Muestra del canal de salida `o` a partir de un fotograma de entrada.
fn map_channel(f: &[f32], o: usize, ch_out: usize) -> f32 {
    let ch_in = f.len();
    if ch_in == ch_out {
        f[o]
    } else if ch_in == 1 {
        f[0]
    } else if ch_out == 1 {
        f.iter().sum::<f32>() / ch_in as f32
    } else if o < ch_in {
        f[o]
    } else {
        0.0
    }
}

#[cfg(not(target_os = "android"))]
fn output_device() -> Result<(cpal::Device, cpal::SupportedStreamConfig), String> {
    use cpal::traits::{DeviceTrait, HostTrait};
    let dev = cpal::default_host().default_output_device().ok_or("no hay dispositivo de audio")?;
    let cfg = dev.default_output_config().map_err(|e| e.to_string())?;
    Ok((dev, cfg))
}

#[cfg(not(target_os = "android"))]
fn build_stream(
    dev: &cpal::Device,
    cfg: &cpal::SupportedStreamConfig,
    ring: Arc<Mutex<std::collections::VecDeque<f32>>>,
    played: Arc<AtomicU64>,
) -> Result<cpal::Stream, String> {
    use cpal::SampleFormat as F;
    let conf = cfg.config();
    let ch = conf.channels as usize;
    match cfg.sample_format() {
        F::F32 => build::<f32>(dev, &conf, ring, played, ch),
        F::I16 => build::<i16>(dev, &conf, ring, played, ch),
        F::U16 => build::<u16>(dev, &conf, ring, played, ch),
        F::I32 => build::<i32>(dev, &conf, ring, played, ch),
        other => Err(format!("formato de muestra no admitido: {other:?}")),
    }
}

#[cfg(not(target_os = "android"))]
fn build<T>(
    dev: &cpal::Device,
    conf: &cpal::StreamConfig,
    ring: Arc<Mutex<std::collections::VecDeque<f32>>>,
    played: Arc<AtomicU64>,
    ch: usize,
) -> Result<cpal::Stream, String>
where
    T: cpal::SizedSample + cpal::FromSample<f32>,
{
    use cpal::traits::DeviceTrait;
    dev.build_output_stream(
        conf,
        move |data: &mut [T], _| {
            let mut n = 0;
            if let Ok(mut q) = ring.try_lock() {
                n = data.len().min(q.len());
                for (o, s) in data.iter_mut().zip(q.drain(..n)) {
                    *o = T::from_sample(s);
                }
            }
            for o in &mut data[n..] {
                *o = T::EQUILIBRIUM;
            }
            played.fetch_add((n / ch.max(1)) as u64, Ordering::Relaxed);
        },
        |e| eprintln!("audio: {e}"),
        None,
    )
    .map_err(|e| e.to_string())
}
