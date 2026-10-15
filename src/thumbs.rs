//! Servicio de miniaturas: cola con prioridad "lo visible primero", pool de hilos,
//! decodificación JPEG con escalado DCT, y caché LRU de texturas.
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{BufReader, Read};
use std::process::{Command, Stdio};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex, OnceLock};

pub const THUMB: u32 = 160;
const CACHE_CAP: usize = 700;

pub struct Decoded {
    pub w: usize,
    pub h: usize,
    pub rgba: Vec<u8>,
}

pub struct Request {
    pub key: u64,
    pub path: PathBuf,
}

#[derive(Default)]
struct State {
    queue: Vec<Request>, // el último es el de mayor prioridad
    inflight: HashSet<u64>,
}

struct Shared {
    st: Mutex<State>,
    cv: Condvar,
}

pub struct Thumbs {
    shared: Arc<Shared>,
    rx: Receiver<(u64, Option<Decoded>)>,
    cache: HashMap<u64, (egui::TextureHandle, u64)>,
    failed: HashSet<u64>,
    frame: u64,
}

fn is_video_ext(e: &str) -> bool {
    matches!(e, "mp4" | "mkv" | "avi" | "mov" | "webm" | "wmv" | "flv" | "m4v" | "mpg" | "mpeg" | "ts" | "3gp" | "ogv")
}

fn have_cmd(name: &'static str, cell: &'static OnceLock<bool>) -> bool {
    *cell.get_or_init(|| {
        let path = std::env::var_os("PATH").unwrap_or_default();
        std::env::split_paths(&path).any(|d| {
            d.join(name).is_file() || d.join(format!("{name}.exe")).is_file()
        })
    })
}

fn have_ffmpeg() -> bool {
    static C: OnceLock<bool> = OnceLock::new();
    have_cmd("ffmpeg", &C)
}

fn have_magick() -> bool {
    static C: OnceLock<bool> = OnceLock::new();
    have_cmd("magick", &C)
}

fn run_capture(mut cmd: Command) -> Option<Vec<u8>> {
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    let out = cmd.output().ok()?;
    if out.status.success() && !out.stdout.is_empty() { Some(out.stdout) } else { None }
}

fn from_png_bytes(bytes: &[u8], max: u32) -> Option<Decoded> {
    let img = image::load_from_memory(bytes).ok()?;
    let img = if img.width() > max || img.height() > max { img.thumbnail(max, max) } else { img };
    let rgba = img.into_rgba8();
    Some(Decoded { w: rgba.width() as usize, h: rgba.height() as usize, rgba: rgba.into_raw() })
}

fn ffmpeg_frame(path: &Path, max: u32, video: bool) -> Option<Decoded> {
    if !have_ffmpeg() {
        return None;
    }
    let scale = format!("scale='min({max},iw)':'min({max},ih)':force_original_aspect_ratio=decrease");
    for seek in if video { vec!["1", "0"] } else { vec!["0"] } {
        let mut c = Command::new("ffmpeg");
        c.args(["-v", "error", "-nostdin", "-ss", seek, "-i"]).arg(path);
        c.args(["-frames:v", "1", "-vf", &scale, "-f", "image2pipe", "-c:v", "png", "-"]);
        if let Some(b) = run_capture(c) {
            if let Some(d) = from_png_bytes(&b, max) {
                return Some(d);
            }
        }
    }
    None
}

fn magick_frame(path: &Path, max: u32) -> Option<Decoded> {
    if !have_magick() {
        return None;
    }
    let mut c = Command::new("magick");
    let mut src = path.as_os_str().to_os_string();
    src.push("[0]");
    c.arg(src).args(["-auto-orient", "-thumbnail", &format!("{max}x{max}>"), "png:-"]);
    from_png_bytes(&run_capture(c)?, max)
}

fn decode_svg(path: &Path, max: u32) -> Option<Decoded> {
    let md = std::fs::metadata(path).ok()?;
    if md.len() > 8 * 1024 * 1024 {
        return None;
    }
    let data = std::fs::read(path).ok()?;
    let tree = resvg::usvg::Tree::from_data(&data, &resvg::usvg::Options::default()).ok()?;
    let sz = tree.size();
    let target = max.min(2048) as f32;
    let s = target / sz.width().max(sz.height()).max(1.0);
    let (w, h) = (((sz.width() * s).round() as u32).max(1), ((sz.height() * s).round() as u32).max(1));
    let mut pm = resvg::tiny_skia::Pixmap::new(w, h)?;
    resvg::render(&tree, resvg::tiny_skia::Transform::from_scale(s, s), &mut pm.as_mut());
    let mut rgba = Vec::with_capacity((w * h * 4) as usize);
    for p in pm.pixels() {
        let c = p.demultiply();
        rgba.extend_from_slice(&[c.red(), c.green(), c.blue(), c.alpha()]);
    }
    Some(Decoded { w: w as usize, h: h as usize, rgba })
}

/// Orientación EXIF (1..8) de un JPEG; 1 si no existe.
fn exif_orientation(path: &Path) -> u8 {
    let mut buf = vec![0u8; 65536];
    let Ok(mut f) = File::open(path) else { return 1 };
    let n = f.read(&mut buf).unwrap_or(0);
    let b = &buf[..n];
    if n < 12 || b[0] != 0xFF || b[1] != 0xD8 {
        return 1;
    }
    let mut i = 2;
    while i + 4 < n {
        if b[i] != 0xFF {
            return 1;
        }
        let marker = b[i + 1];
        let len = u16::from_be_bytes([b[i + 2], b[i + 3]]) as usize;
        if marker == 0xE1 && i + 10 < n && &b[i + 4..i + 10] == b"Exif\0\0" {
            let t = i + 10;
            if t + 8 > n {
                return 1;
            }
            let le = &b[t..t + 2] == b"II";
            let u16_at = |o: usize| -> Option<u16> {
                let s = b.get(o..o + 2)?;
                Some(if le { u16::from_le_bytes([s[0], s[1]]) } else { u16::from_be_bytes([s[0], s[1]]) })
            };
            let u32_at = |o: usize| -> Option<u32> {
                let s = b.get(o..o + 4)?;
                Some(if le { u32::from_le_bytes([s[0], s[1], s[2], s[3]]) } else { u32::from_be_bytes([s[0], s[1], s[2], s[3]]) })
            };
            let Some(ifd) = u32_at(t + 4) else { return 1 };
            let base = t + ifd as usize;
            let Some(cnt) = u16_at(base) else { return 1 };
            for k in 0..cnt as usize {
                let e = base + 2 + k * 12;
                if u16_at(e) == Some(0x0112) {
                    return u16_at(e + 8).filter(|v| (1..=8).contains(v)).map(|v| v as u8).unwrap_or(1);
                }
            }
            return 1;
        }
        if marker == 0xDA {
            return 1;
        }
        i += 2 + len;
    }
    1
}

fn orient(img: image::RgbaImage, o: u8) -> image::RgbaImage {
    use image::imageops::{flip_horizontal, flip_vertical, rotate180, rotate270, rotate90};
    match o {
        2 => flip_horizontal(&img),
        3 => rotate180(&img),
        4 => flip_vertical(&img),
        5 => flip_horizontal(&rotate90(&img)),
        6 => rotate90(&img),
        7 => flip_horizontal(&rotate270(&img)),
        8 => rotate270(&img),
        _ => img,
    }
}

/// Decodifica una imagen/SVG/vídeo a RGBA limitando el lado mayor a `max`.
pub fn decode_image(path: &Path, max: u32) -> Option<Decoded> {
    let ext = path.extension()?.to_string_lossy().to_ascii_lowercase();
    match ext.as_str() {
        "jpg" | "jpeg" | "jpe" => {
            if let Some(d) = decode_jpeg(path, max) {
                return Some(d);
            }
        }
        "svg" => return decode_svg(path, max),
        e if is_video_ext(e) => return ffmpeg_frame(path, max, true),
        "png" | "gif" | "webp" | "bmp" => {}
        _ => return ffmpeg_frame(path, max, false).or_else(|| magick_frame(path, max)),
    }
    let md = std::fs::metadata(path).ok()?;
    if md.len() > 256 * 1024 * 1024 {
        return None;
    }
    let img = image::ImageReader::open(path)
        .ok()
        .and_then(|r| r.with_guessed_format().ok())
        .and_then(|r| r.decode().ok());
    let Some(img) = img else { return magick_frame(path, max) };
    let img = if img.width() > max || img.height() > max { img.thumbnail(max, max) } else { img };
    let rgba = img.into_rgba8();
    Some(Decoded { w: rgba.width() as usize, h: rgba.height() as usize, rgba: rgba.into_raw() })
}

fn decode_jpeg(path: &Path, max: u32) -> Option<Decoded> {
    let f = File::open(path).ok()?;
    let mut dec = jpeg_decoder::Decoder::new(BufReader::with_capacity(64 * 1024, f));
    dec.read_info().ok()?;
    let info = dec.info()?;
    let (w0, h0) = (info.width as u32, info.height as u32);
    if w0 > max || h0 > max {
        // escalado por DCT (1/2, 1/4, 1/8): mucho más rápido que decodificar completo
        let s = (max as f32 / w0.max(h0) as f32).min(1.0);
        let _ = dec.scale((w0 as f32 * s).ceil().max(1.0) as u16, (h0 as f32 * s).ceil().max(1.0) as u16);
    }
    let px = dec.decode().ok()?;
    let info = dec.info()?;
    let (w, h) = (info.width as u32, info.height as u32);
    let rgb: Vec<u8> = match info.pixel_format {
        jpeg_decoder::PixelFormat::RGB24 => px,
        jpeg_decoder::PixelFormat::L8 => px.iter().flat_map(|&v| [v, v, v]).collect(),
        jpeg_decoder::PixelFormat::CMYK32 => px
            .chunks_exact(4)
            .flat_map(|c| {
                let k = c[3] as u32;
                [(c[0] as u32 * k / 255) as u8, (c[1] as u32 * k / 255) as u8, (c[2] as u32 * k / 255) as u8]
            })
            .collect(),
        _ => return None,
    };
    let img = image::RgbImage::from_raw(w, h, rgb)?;
    let img = if w > max || h > max {
        let s = max as f32 / w.max(h) as f32;
        let (nw, nh) = (((w as f32 * s).round() as u32).max(1), ((h as f32 * s).round() as u32).max(1));
        image::imageops::thumbnail(&img, nw, nh)
    } else {
        img
    };
    let rgba = image::DynamicImage::ImageRgb8(img).into_rgba8();
    let rgba = orient(rgba, exif_orientation(path));
    Some(Decoded { w: rgba.width() as usize, h: rgba.height() as usize, rgba: rgba.into_raw() })
}

impl Thumbs {
    pub fn new(ctx: &egui::Context) -> Self {
        let shared = Arc::new(Shared { st: Mutex::new(State::default()), cv: Condvar::new() });
        let (tx, rx) = channel();
        let n = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).clamp(2, 6);
        for _ in 0..n {
            let (sh, tx, ctx): (Arc<Shared>, Sender<_>, egui::Context) = (shared.clone(), tx.clone(), ctx.clone());
            std::thread::spawn(move || loop {
                let req = {
                    let mut st = sh.st.lock().unwrap();
                    loop {
                        if let Some(r) = st.queue.pop() {
                            st.inflight.insert(r.key);
                            break r;
                        }
                        st = sh.cv.wait(st).unwrap();
                    }
                };
                // los archivos de un servidor SFTP no se descargan sólo para la miniatura
                let out = if crate::sftp::is_remote(&req.path) { None } else { decode_image(&req.path, THUMB) };
                if tx.send((req.key, out)).is_err() {
                    return;
                }
                ctx.request_repaint();
            });
        }
        Thumbs { shared, rx, cache: HashMap::new(), failed: HashSet::new(), frame: 0 }
    }

    /// Recoge resultados y los sube a la GPU (acotado por frame).
    pub fn pump(&mut self, ctx: &egui::Context) {
        self.frame += 1;
        let mut n = 0;
        while n < 24 {
            let Ok((key, out)) = self.rx.try_recv() else { break };
            self.shared.st.lock().unwrap().inflight.remove(&key);
            match out {
                Some(d) => {
                    let img = egui::ColorImage::from_rgba_unmultiplied([d.w, d.h], &d.rgba);
                    let tex = ctx.load_texture("t", img, egui::TextureOptions::LINEAR);
                    self.cache.insert(key, (tex, self.frame));
                }
                None => {
                    self.failed.insert(key);
                }
            }
            n += 1;
        }
        if n == 24 {
            ctx.request_repaint();
        }
        if self.cache.len() > CACHE_CAP + 100 {
            let mut v: Vec<(u64, u64)> = self.cache.iter().map(|(k, (_, f))| (*k, *f)).collect();
            v.sort_unstable_by_key(|&(_, f)| f);
            let excess = self.cache.len() - CACHE_CAP;
            for (k, _) in v.iter().take(excess) {
                self.cache.remove(k);
            }
        }
    }

    pub fn get(&mut self, key: u64) -> Option<&egui::TextureHandle> {
        let f = self.frame;
        self.cache.get_mut(&key).map(|(t, last)| {
            *last = f;
            &*t
        })
    }

    pub fn is_failed(&self, key: u64) -> bool {
        self.failed.contains(&key)
    }

    pub fn has(&self, key: u64) -> bool {
        self.cache.contains_key(&key)
    }

    /// Sustituye la cola de pendientes. `wanted` ordenado de mayor a menor prioridad.
    pub fn set_wanted(&self, mut wanted: Vec<Request>) {
        let mut st = self.shared.st.lock().unwrap();
        wanted.retain(|r| !st.inflight.contains(&r.key));
        wanted.reverse();
        let had = st.queue.len();
        st.queue = wanted;
        if !st.queue.is_empty() || had > 0 {
            self.shared.cv.notify_all();
        }
    }
}
