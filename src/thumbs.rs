//! Servicio de miniaturas: cola con prioridad "lo visible primero", pool de hilos,
//! decodificación JPEG con escalado DCT, y caché LRU de texturas.
#[allow(unused_imports)]
use crate::vfs::{KindV as _, PathV as _};
use std::collections::{HashMap, HashSet};
use crate::vfs::fs::File;
use std::io::{BufReader, Read};
use std::process::{Command, Stdio};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock};

pub const THUMB: u32 = 160;
const CACHE_CAP: usize = 700;
/// Miniaturas fallidas que se recuerdan (pasado el tope se olvidan y se reintentan una vez).
const FAILED_CAP: usize = 50_000;
/// Plazo de una miniatura de documento (las demás no se pueden interrumpir).
const THUMB_DEADLINE: std::time::Duration = std::time::Duration::from_secs(8);
/// Pila de los hilos que decodifican archivos ajenos: un desbordamiento de pila no se puede atrapar.
pub const DECODE_STACK: usize = 16 << 20;

// ---------------------------------------------------------------------------
// Contención de fallos: un decodificador que entra en pánico con un archivo raro sólo pierde ese
// archivo; la aplicación y el hilo siguen.
// ---------------------------------------------------------------------------

thread_local! {
    static GUARDED: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

/// Ejecuta `f` (leer o decodificar un archivo ajeno) atrapando un pánico: `None` si falló.
/// En la web (`panic = "abort"`) no atrapa nada.
pub fn guarded<T>(f: impl FnOnce() -> T) -> Option<T> {
    install_panic_hook();
    let _ = GUARDED.try_with(|g| g.set(g.get() + 1));
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
    let _ = GUARDED.try_with(|g| g.set(g.get().saturating_sub(1)));
    r.ok()
}

/// Gancho de pánico: dentro de `guarded` sólo deja una línea (unas pocas veces, sin inundar la
/// salida); fuera, avisa como siempre y aborta, igual que con `panic = "abort"`.
pub fn install_panic_hook() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let prev = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            if GUARDED.try_with(|g| g.get()).unwrap_or(0) > 0 {
                static SHOWN: AtomicU32 = AtomicU32::new(0);
                if SHOWN.fetch_add(1, Ordering::Relaxed) < 8 {
                    use std::io::Write;
                    let at = info.location().map(|l| format!("{}:{}", l.file(), l.line())).unwrap_or_default();
                    let _ = writeln!(std::io::stderr(), "File Browser: un archivo no se pudo decodificar ({at}); se omite");
                }
                return;
            }
            prev(info);
            #[cfg(not(test))]
            std::process::abort();
        }));
    });
}

/// Bloquea un mutex aunque otro hilo haya entrado en pánico con él cogido.
pub fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Lanza un hilo de decodificación con pila amplia.
pub fn spawn_decoder<F: FnOnce() + Send + 'static>(name: &str, f: F) {
    if let Err(e) = crate::vfs::thread::Builder::new().name(name.to_string()).stack_size(DECODE_STACK).spawn(f) {
        use std::io::Write;
        let _ = writeln!(std::io::stderr(), "File Browser: no se pudo lanzar el hilo {name}: {e}");
    }
}

/// Abre sólo un archivo normal: una tubería con nombre o un dispositivo (p. ej. `x.md` que sea un
/// FIFO) dejarían bloqueado para siempre el hilo que lo lee.
pub fn open_regular(path: &Path) -> Option<File> {
    if !crate::vfs::fs::metadata(path).map(|m| m.v_is_file()).unwrap_or(false) {
        return None;
    }
    // sin bloquear al abrir: el archivo puede cambiar por un FIFO entre la comprobación y la apertura
    #[cfg(all(unix, not(target_arch = "wasm32")))]
    let f = {
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::OpenOptions::new().read(true).custom_flags(libc::O_NONBLOCK).open(path).ok()?
    };
    #[cfg(not(all(unix, not(target_arch = "wasm32"))))]
    let f = File::open(path).ok()?;
    f.metadata().ok().filter(|m| m.v_is_file()).map(|_| f)
}

/// Resultado de una miniatura.
enum Out {
    Done(Decoded),
    Failed,
    /// ya no se quería (se pedirá de nuevo si vuelve a verse)
    Cancelled,
}

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
    /// claves de la última petición (las que se están decodificando incluidas)
    wanted: HashSet<u64>,
}

struct Shared {
    st: Mutex<State>,
    cv: Condvar,
}

pub struct Thumbs {
    shared: Arc<Shared>,
    rx: Receiver<(u64, Out)>,
    cache: HashMap<u64, (egui::TextureHandle, u64)>,
    failed: HashSet<u64>,
    frame: u64,
    /// sin hilos (navegador): las miniaturas se decodifican aquí, unas pocas por fotograma
    #[cfg(target_arch = "wasm32")]
    tx: Sender<(u64, Out)>,
}

fn is_video_ext(e: &str) -> bool {
    matches!(e, "mp4" | "mkv" | "avi" | "mov" | "webm" | "wmv" | "flv" | "m4v" | "mpg" | "mpeg" | "ts" | "3gp" | "ogv")
}

fn have_cmd(name: &'static str, cell: &'static OnceLock<bool>) -> bool {
    *cell.get_or_init(|| {
        let path = std::env::var_os("PATH").unwrap_or_default();
        crate::vfs::split_paths(&path).any(|d| {
            d.join(name).v_is_file() || d.join(format!("{name}.exe")).v_is_file()
        })
    })
}

#[cfg(not(target_os = "android"))]
fn have_ffmpeg() -> bool {
    static C: OnceLock<bool> = OnceLock::new();
    have_cmd("ffmpeg", &C)
}

fn have_magick() -> bool {
    static C: OnceLock<bool> = OnceLock::new();
    have_cmd("magick", &C)
}

fn run_capture(cmd: Command) -> Option<Vec<u8>> {
    run_capture_in(cmd, Stdio::null())
}

fn run_capture_in(mut cmd: Command, stdin: Stdio) -> Option<Vec<u8>> {
    cmd.stdin(stdin).stdout(Stdio::piped()).stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    // un archivo corrupto puede dejar colgado a ffmpeg/magick: se mata pasado el plazo
    let mut child = cmd.spawn().ok()?;
    let mut so = child.stdout.take()?;
    let reader = crate::vfs::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = std::io::Read::read_to_end(&mut std::io::Read::take(&mut so, 64 << 20), &mut buf);
        // lo que sobre se descarta para que el proceso no se quede bloqueado escribiendo
        let _ = std::io::copy(&mut so, &mut std::io::sink());
        buf
    });
    let end = web_time::Instant::now() + std::time::Duration::from_secs(25);
    let ok = loop {
        match child.try_wait() {
            Ok(Some(st)) => break st.success(),
            Ok(None) if web_time::Instant::now() < end => crate::vfs::thread::sleep(std::time::Duration::from_millis(15)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break false;
            }
        }
    };
    let out = reader.join().ok()?;
    if ok && !out.is_empty() { Some(out) } else { None }
}

fn from_png_bytes(bytes: &[u8], max: u32) -> Option<Decoded> {
    let img = image::load_from_memory(bytes).ok()?;
    let img = if img.width() > max || img.height() > max { img.thumbnail(max, max) } else { img };
    let rgba = img.into_rgba8();
    Some(Decoded { w: rgba.width() as usize, h: rgba.height() as usize, rgba: rgba.into_raw() })
}

#[allow(unreachable_code)]
fn ffmpeg_frame(path: &Path, max: u32, video: bool) -> Option<Decoded> {
    #[cfg(target_os = "android")]
    {
        // sin ffmpeg: los códecs del sistema dan el fotograma
        return if video { crate::droid::frame(path, max) } else { None };
    }
    #[cfg(not(target_os = "android"))]
    if !have_ffmpeg() {
        return None;
    }
    let scale = format!("scale='min({max},iw)':'min({max},ih)':force_original_aspect_ratio=decrease");
    for seek in if video { vec!["1", "0"] } else { vec!["0"] } {
        let mut c = Command::new("ffmpeg");
        c.args(["-v", "error", "-nostdin", "-ss", seek, "-protocol_whitelist", "file,pipe", "-i"]).arg(path);
        c.args(["-frames:v", "1", "-vf", &scale, "-f", "image2pipe", "-c:v", "png", "-"]);
        if let Some(b) = run_capture(c) {
            if let Some(d) = from_png_bytes(&b, max) {
                return Some(d);
            }
        }
    }
    None
}

/// Formato de ImageMagick para las extensiones que de verdad lo necesitan. Las demás no se le pasan:
/// adivinaría el formato por el contenido, y un «PNG» que fuera PostScript/MVG/MSL llegaría a sus
/// delegados.
fn magick_coder(ext: &str) -> Option<&'static str> {
    Some(match ext {
        "tif" | "tiff" => "TIFF",
        "psd" => "PSD",
        "psb" => "PSB",
        "heic" | "heif" => "HEIC",
        "avif" => "AVIF",
        "xcf" => "XCF",
        "cr2" => "CR2",
        "cr3" => "CR3",
        "nef" => "NEF",
        "nrw" => "NRW",
        "arw" => "ARW",
        "srf" => "SRF",
        "sr2" => "SR2",
        "dng" => "DNG",
        "orf" => "ORF",
        "rw2" => "RW2",
        "pef" => "PEF",
        "raf" => "RAF",
        "srw" => "SRW",
        "3fr" => "3FR",
        "erf" => "ERF",
        "kdc" => "KDC",
        "mrw" => "MRW",
        "x3f" => "X3F",
        _ => return None,
    })
}

/// Argumentos de `magick`: límites de recursos, formato fijado y la imagen por la entrada estándar
/// (el nombre del archivo nunca se interpreta: ni `@lista`, ni comodines, ni `formato:`).
fn magick_args(coder: &str, max: u32) -> Vec<String> {
    let mut a: Vec<String> = Vec::new();
    for (k, v) in [("memory", "256MiB"), ("map", "512MiB"), ("disk", "1GiB"), ("area", "128MP"), ("width", "32768"), ("height", "32768"), ("time", "20"), ("thread", "2")] {
        a.extend(["-limit".to_string(), k.to_string(), v.to_string()]);
    }
    a.push(format!("{coder}:-[0]"));
    a.extend(["-auto-orient".to_string(), "-thumbnail".to_string(), format!("{max}x{max}>"), "png:-".to_string()]);
    a
}

fn magick_frame(path: &Path, max: u32) -> Option<Decoded> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    let coder = magick_coder(&ext)?;
    if !have_magick() {
        return None;
    }
    let f = open_regular(path)?;
    let mut c = Command::new("magick");
    c.args(magick_args(coder, max));
    #[cfg(not(target_arch = "wasm32"))]
    let stdin = Stdio::from(f);
    #[cfg(target_arch = "wasm32")]
    let stdin = {
        drop(f);
        Stdio::null()
    };
    // sigue valiendo el plazo de 25 s de `run_capture_in`
    from_png_bytes(&run_capture_in(c, stdin)?, max)
}

fn decode_svg(path: &Path, max: u32) -> Option<Decoded> {
    let md = crate::vfs::fs::metadata(path).ok()?;
    if md.len() > 4 * 1024 * 1024 {
        return None;
    }
    let data = crate::vfs::fs::read(path).ok()?;
    // un SVG no puede leer otros archivos del disco (<image href="/ruta">) para pintarlos en la miniatura
    let mut opt = resvg::usvg::Options::default();
    opt.image_href_resolver.resolve_string = Box::new(|_, _| None);
    let tree = resvg::usvg::Tree::from_data(&data, &opt).ok()?;
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
    exif_orientation_in(&buf[..n])
}

/// Orientación EXIF a partir del principio de un JPEG.
fn exif_orientation_in(b: &[u8]) -> u8 {
    let n = b.len();
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
                let s = b.get(o..o.checked_add(2)?)?;
                Some(if le { u16::from_le_bytes([s[0], s[1]]) } else { u16::from_be_bytes([s[0], s[1]]) })
            };
            let u32_at = |o: usize| -> Option<u32> {
                let s = b.get(o..o.checked_add(4)?)?;
                Some(if le { u32::from_le_bytes([s[0], s[1], s[2], s[3]]) } else { u32::from_be_bytes([s[0], s[1], s[2], s[3]]) })
            };
            let Some(ifd) = u32_at(t + 4) else { return 1 };
            // desplazamiento del archivo: en 32 bits (Android armv7, web) la suma desbordaría
            let Some(base) = usize::try_from(ifd).ok().and_then(|v| t.checked_add(v)) else { return 1 };
            let Some(cnt) = u16_at(base) else { return 1 };
            for k in 0..cnt as usize {
                let Some(e) = base.checked_add(2 + k * 12) else { return 1 };
                if u16_at(e) == Some(0x0112) {
                    return e.checked_add(8).and_then(u16_at).filter(|v| (1..=8).contains(v)).map(|v| v as u8).unwrap_or(1);
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

/// ¿Este archivo tiene miniatura propia (imagen, vídeo, documento, libro o cómic)?
pub fn thumbable(kind: crate::fsys::Kind, ext: &str) -> bool {
    use crate::fsys::Kind;
    match kind {
        Kind::Image | Kind::Video => true,
        Kind::Doc => crate::docview::is_doc_ext(ext) || crate::comic::is_comic_ext(ext) || ext.eq_ignore_ascii_case("epub"),
        _ => false,
    }
}

/// Decodifica una imagen/SVG/vídeo a RGBA limitando el lado mayor a `max`.
/// Lector de imágenes sobre la capa `vfs` (en la web, el archivo está en el servidor).
pub fn open_image(path: &Path) -> std::io::Result<image::ImageReader<BufReader<File>>> {
    image::ImageReader::new(BufReader::new(File::open(path)?)).with_guessed_format()
}

pub fn decode_image(path: &Path, max: u32) -> Option<Decoded> {
    decode_image_cancel(path, max, &|| false)
}

/// Como `decode_image`; `cancel` interrumpe lo que se puede interrumpir (documentos).
pub fn decode_image_cancel(path: &Path, max: u32, cancel: &dyn Fn() -> bool) -> Option<Decoded> {
    let ext = path.extension()?.to_string_lossy().to_ascii_lowercase();
    // una tubería con nombre o un dispositivo dejarían el hilo bloqueado
    if !crate::vfs::fs::metadata(path).map(|m| m.v_is_file()).unwrap_or(false) {
        return None;
    }
    match ext.as_str() {
        "jpg" | "jpeg" | "jpe" => {
            if let Some(d) = decode_jpeg(path, max) {
                return Some(d);
            }
        }
        "svg" => return decode_svg(path, max),
        e if crate::docview::is_doc_ext(e) => return crate::docview::thumb_cancel(path, max, cancel),
        e if crate::comic::is_comic_ext(e) => return crate::comic::thumb(path, max),
        "epub" => return crate::rich::epub_cover(path, max),
        e if is_video_ext(e) => return ffmpeg_frame(path, max, true),
        e if crate::imgx::handles(e) => return crate::imgx::decode(path, e, max).or_else(|| magick_frame(path, max)),
        "png" | "gif" | "webp" | "bmp" | "tif" | "tiff" | "ico" => {}
        _ => return ffmpeg_frame(path, max, false).or_else(|| magick_frame(path, max)),
    }
    let md = crate::vfs::fs::metadata(path).ok()?;
    if md.len() > 256 * 1024 * 1024 {
        return None;
    }
    let Some(img) = decode_limited(path, max) else { return magick_frame(path, max) };
    let img = if img.width() > max || img.height() > max { img.thumbnail(max, max) } else { img };
    let rgba = img.into_rgba8();
    Some(Decoded { w: rgba.width() as usize, h: rgba.height() as usize, rgba: rgba.into_raw() })
}

/// Imágenes grandes: de una en una entre todos los hilos (la memoria de 6 a la vez no cabe).
pub(crate) static HEAVY: Mutex<()> = Mutex::new(());

/// Decodifica con `image` acotando la memoria (menos para miniaturas que para la vista previa).
fn decode_limited(path: &Path, max: u32) -> Option<image::DynamicImage> {
    let (w, h) = open_image(path).ok()?.into_dimensions().ok()?;
    let mut lim = image::Limits::default();
    lim.max_image_width = Some(32768);
    lim.max_image_height = Some(32768);
    lim.max_alloc = Some(if max <= 1024 { 256 << 20 } else { 512 << 20 });
    let _heavy = (w as u64 * h as u64 * 4 > 48 << 20).then(|| lock(&HEAVY));
    let mut r = open_image(path).ok()?;
    r.limits(lim);
    r.decode().ok()
}

fn decode_jpeg(path: &Path, max: u32) -> Option<Decoded> {
    let f = File::open(path).ok()?;
    decode_jpeg_from(BufReader::with_capacity(64 * 1024, f), max, exif_orientation(path))
}

/// JPEG desde cualquier lector (también las vistas previas incrustadas en RAW), con la orientación dada.
pub fn decode_jpeg_from<R: std::io::Read>(r: R, max: u32, orientation: u8) -> Option<Decoded> {
    let mut dec = jpeg_decoder::Decoder::new(r);
    dec.read_info().ok()?;
    let info = dec.info()?;
    let (w0, h0) = (info.width as u32, info.height as u32);
    // sin tope, un JPEG diminuto que declare 65535×65535 pediría gigas de memoria
    if w0 as u64 * h0 as u64 > 100_000_000 {
        return None;
    }
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
    let rgba = orient(rgba, orientation);
    Some(Decoded { w: rgba.width() as usize, h: rgba.height() as usize, rgba: rgba.into_raw() })
}

impl Thumbs {
    pub fn new(ctx: &egui::Context) -> Self {
        install_panic_hook();
        let shared = Arc::new(Shared { st: Mutex::new(State::default()), cv: Condvar::new() });
        let (tx, rx) = channel();
        let n = crate::vfs::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).clamp(2, 6);
        #[cfg(target_arch = "wasm32")]
        let n = {
            let _ = (n, ctx);
            0
        };
        for _ in 0..n {
            let (sh, tx, ctx): (Arc<Shared>, Sender<_>, egui::Context) = (shared.clone(), tx.clone(), ctx.clone());
            spawn_decoder("miniaturas", move || loop {
                let req = {
                    let mut st = lock(&sh.st);
                    loop {
                        if let Some(r) = st.queue.pop() {
                            st.inflight.insert(r.key);
                            break r;
                        }
                        st = sh.cv.wait(st).unwrap_or_else(|e| e.into_inner());
                    }
                };
                let out = thumb_job(&sh, &req);
                if tx.send((req.key, out)).is_err() {
                    return;
                }
                ctx.request_repaint();
            });
        }
        Thumbs { shared, rx, cache: HashMap::new(), failed: HashSet::new(), frame: 0, #[cfg(target_arch = "wasm32")] tx }
    }

    /// Recoge resultados y los sube a la GPU (acotado por frame).
    pub fn pump(&mut self, ctx: &egui::Context) {
        self.frame += 1;
        #[cfg(target_arch = "wasm32")]
        self.decode_some(ctx);
        let mut n = 0;
        while n < 24 {
            let Ok((key, out)) = self.rx.try_recv() else { break };
            lock(&self.shared.st).inflight.remove(&key);
            match out {
                Out::Done(d) => {
                    let img = color_image(ctx, d.w, d.h, &d.rgba);
                    let tex = ctx.load_texture("t", img, egui::TextureOptions::LINEAR);
                    self.cache.insert(key, (tex, self.frame));
                }
                Out::Failed => {
                    if self.failed.len() >= FAILED_CAP {
                        self.failed.clear();
                    }
                    self.failed.insert(key);
                }
                Out::Cancelled => {}
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

    /// Decodifica un par de miniaturas pendientes (en el navegador no hay hilos que lo hagan).
    #[cfg(target_arch = "wasm32")]
    fn decode_some(&mut self, ctx: &egui::Context) {
        for _ in 0..2 {
            let req = {
                let mut st = lock(&self.shared.st);
                let Some(r) = st.queue.pop() else { return };
                st.inflight.insert(r.key);
                r
            };
            // una imagen grande habría que bajarla entera para una miniatura
            let big = crate::vfs::fs::metadata(&req.path).map(|m| m.len() > 4 << 20).unwrap_or(true);
            let out = if big { None } else { guarded(|| decode_image(&req.path, THUMB)).flatten() };
            let _ = self.tx.send((req.key, out.map(Out::Done).unwrap_or(Out::Failed)));
            ctx.request_repaint();
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
        let mut st = lock(&self.shared.st);
        st.wanted.clear();
        st.wanted.extend(wanted.iter().map(|r| r.key));
        wanted.retain(|r| !st.inflight.contains(&r.key));
        wanted.reverse();
        let had = st.queue.len();
        st.queue = wanted;
        if !st.queue.is_empty() || had > 0 {
            self.shared.cv.notify_all();
        }
    }
}

/// Una miniatura en un hilo del grupo: con plazo, cancelable si deja de quererse y con los pánicos
/// del decodificador contenidos (el archivo queda como fallido y no se reintenta).
fn thumb_job(sh: &Shared, req: &Request) -> Out {
    // los archivos de un servidor SFTP no se descargan sólo para la miniatura
    if crate::sftp::is_remote(&req.path) {
        return Out::Failed;
    }
    let key = req.key;
    let end = web_time::Instant::now() + THUMB_DEADLINE;
    let gone = || !lock(&sh.st).wanted.contains(&key);
    let cancel = || web_time::Instant::now() > end || gone();
    job_outcome(guarded(|| decode_image_cancel(&req.path, THUMB, &cancel)), gone)
}

/// Fallida si el decodificador falló o entró en pánico; cancelada si ya no se quería.
fn job_outcome(r: Option<Option<Decoded>>, gone: impl Fn() -> bool) -> Out {
    match r {
        Some(Some(d)) => Out::Done(d),
        Some(None) if gone() => Out::Cancelled,
        _ => Out::Failed,
    }
}

/// Imagen para subir como textura: si un lado supera el máximo de la GPU (egui lo comprueba con un
/// `assert` y cerraría la aplicación) se reduce proporcionalmente.
pub fn color_image(ctx: &egui::Context, w: usize, h: usize, rgba: &[u8]) -> egui::ColorImage {
    let max = ctx.input(|i| i.max_texture_side).max(512);
    if w <= max && h <= max || w == 0 || h == 0 {
        return egui::ColorImage::from_rgba_unmultiplied([w, h], rgba);
    }
    match image::RgbaImage::from_raw(w as u32, h as u32, rgba.to_vec()) {
        Some(img) => {
            let small = image::DynamicImage::ImageRgba8(img).thumbnail(max as u32, max as u32).into_rgba8();
            egui::ColorImage::from_rgba_unmultiplied([small.width() as usize, small.height() as usize], small.as_raw())
        }
        None => egui::ColorImage::from_rgba_unmultiplied([1, 1], &[0, 0, 0, 0]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exif_ifd_offset_cannot_overflow() {
        // JPEG con APP1 «Exif» cuyo IFD apunta casi al final del espacio de direcciones
        for ifd in [0xFFFF_FFF0u32, 0xFFFF_FFFF, 0x7FFF_FFFF, 8] {
            let mut b = vec![0xFF, 0xD8, 0xFF, 0xE1, 0x00, 0x20];
            b.extend_from_slice(b"Exif\0\0II*\0");
            b.extend_from_slice(&ifd.to_le_bytes());
            b.resize(64, 0);
            assert_eq!(exif_orientation_in(&b), 1);
        }
        // un IFD válido sí se lee: una entrada Orientation = 6
        let mut b = vec![0xFF, 0xD8, 0xFF, 0xE1, 0x00, 0x30];
        b.extend_from_slice(b"Exif\0\0II*\0");
        b.extend_from_slice(&8u32.to_le_bytes());
        b.extend_from_slice(&[1, 0, 0x12, 0x01, 3, 0, 1, 0, 0, 0, 6, 0, 0, 0, 0, 0, 0, 0]);
        b.resize(80, 0);
        assert_eq!(exif_orientation_in(&b), 6);
    }

    #[test]
    fn magick_only_for_listed_formats_with_pinned_coder() {
        for e in ["png", "jpg", "gif", "svg", "ps", "eps", "pdf", "mvg", "msl", "txt", "blend", "raw", ""] {
            assert_eq!(magick_coder(e), None, "{e}");
        }
        assert_eq!(magick_coder("tif"), Some("TIFF"));
        assert_eq!(magick_coder("heic"), Some("HEIC"));
        let a = magick_args("TIFF", 160);
        let input = a.iter().position(|x| x == "TIFF:-[0]").expect("entrada con formato fijado");
        // los límites van antes de la entrada
        let limits: Vec<&str> = a.iter().enumerate().filter(|(_, x)| *x == "-limit").map(|(i, _)| a[i + 1].as_str()).collect();
        for k in ["memory", "map", "disk", "area", "width", "height", "time"] {
            assert!(limits.contains(&k), "{k}");
        }
        assert!(a.iter().rposition(|x| x == "-limit").unwrap() < input);
        assert_eq!(a.last().map(|s| s.as_str()), Some("png:-"));
        assert!(a.contains(&"160x160>".to_string()));
        // ningún argumento lleva una ruta
        assert!(a.iter().all(|x| !x.contains('/')));
    }

    #[test]
    fn panicking_job_is_failed_and_worker_survives() {
        assert!(guarded(|| -> u8 { panic!("decodificador roto") }).is_none());
        assert_eq!(guarded(|| 7), Some(7));
        assert!(matches!(job_outcome(guarded(|| -> Option<Decoded> { panic!("x") }), || false), Out::Failed));
        assert!(matches!(job_outcome(Some(None), || true), Out::Cancelled));
        assert!(matches!(job_outcome(Some(None), || false), Out::Failed));
        // un hilo de trabajo que recibe un trabajo que entra en pánico sigue con los siguientes
        let (tx, rx) = channel::<u32>();
        let (otx, orx) = channel::<(u32, bool)>();
        spawn_decoder("prueba", move || {
            while let Ok(j) = rx.recv() {
                let r = guarded(|| if j % 2 == 0 { panic!("trabajo {j}") } else { j });
                if otx.send((j, r.is_some())).is_err() {
                    return;
                }
            }
        });
        for j in 0..6 {
            tx.send(j).unwrap();
        }
        let got: Vec<(u32, bool)> = (0..6).map(|_| orx.recv_timeout(std::time::Duration::from_secs(10)).unwrap()).collect();
        assert_eq!(got, (0..6).map(|j| (j, j % 2 == 1)).collect::<Vec<_>>());
        // un mutex envenenado por un pánico se sigue pudiendo usar
        let m = Arc::new(Mutex::new(1));
        let m2 = m.clone();
        let _ = guarded(move || {
            let _g = m2.lock().unwrap();
            panic!("con el mutex cogido");
        });
        assert!(m.is_poisoned());
        *lock(&m) += 1;
        assert_eq!(*lock(&m), 2);
    }

    #[cfg(all(unix, not(target_arch = "wasm32")))]
    #[test]
    fn fifo_is_refused_without_blocking() {
        let dir = std::env::temp_dir().join(format!("fb-thumbs-fifo-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let p = dir.join("x.png");
        let c = std::ffi::CString::new(p.to_str().unwrap()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
        let t = std::time::Instant::now();
        assert!(open_regular(&p).is_none());
        assert!(decode_image(&p, 64).is_none());
        assert!(t.elapsed() < std::time::Duration::from_secs(2));
        let ok = dir.join("y.txt");
        std::fs::write(&ok, b"hola").unwrap();
        assert!(open_regular(&ok).is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
