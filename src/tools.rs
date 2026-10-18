//! Herramientas auxiliares: zip/unzip, estadísticas de carpeta, búsqueda recursiva, portapapeles de archivos.
use crate::fsys::Progress;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering as AO};
use std::sync::{Arc, Mutex};

// ---------------------------------------------------------------------------
// ZIP
// ---------------------------------------------------------------------------

/// Formato de salida al comprimir.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ArchFmt {
    Zip,
    TarGz,
    SevenZ,
}

impl ArchFmt {
    pub fn ext(self) -> &'static str {
        match self {
            ArchFmt::Zip => "zip",
            ArchFmt::TarGz => "tar.gz",
            ArchFmt::SevenZ => "7z",
        }
    }

    /// ¿Admite contraseña? (tar.gz no tiene cifrado)
    pub fn can_encrypt(self) -> bool {
        self != ArchFmt::TarGz
    }
}

/// Opciones de compresión: nivel 0 (solo almacenar) a 9; la contraseña cifra con AES-256.
#[derive(Clone)]
pub struct CompressOpts {
    pub fmt: ArchFmt,
    pub level: u32,
    pub password: Option<String>,
}

pub fn compress_paths(srcs: &[PathBuf], out: &Path, o: &CompressOpts, p: &Progress, ctx: &egui::Context) -> Result<(), String> {
    match o.fmt {
        ArchFmt::Zip => zip_paths(srcs, out, o, p, ctx),
        ArchFmt::TarGz => targz_paths(srcs, out, o, p, ctx),
        ArchFmt::SevenZ => sevenz_paths(srcs, out, o, p, ctx),
    }
}

/// Lector que cuenta los bytes leídos (progreso) y corta si se cancela la operación.
struct ProgRead<'a, R> {
    inner: R,
    p: &'a Progress,
}

impl<R: Read> Read for ProgRead<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.p.cancelled() {
            return Err(io::Error::other("cancelado"));
        }
        let n = self.inner.read(buf)?;
        self.p.done.fetch_add(n as u64, AO::Relaxed);
        Ok(n)
    }
}

fn zip_paths(srcs: &[PathBuf], out: &Path, o: &CompressOpts, p: &Progress, ctx: &egui::Context) -> Result<(), String> {
    let f = File::create(out).map_err(|e| e.to_string())?;
    let mut zw = zip::ZipWriter::new(io::BufWriter::new(f));
    let mut opts = zip::write::SimpleFileOptions::default();
    opts = if o.level == 0 {
        opts.compression_method(zip::CompressionMethod::Stored)
    } else {
        opts.compression_method(zip::CompressionMethod::Deflated).compression_level(Some(o.level as i64))
    };
    let opts = match &o.password {
        Some(pw) => opts.with_aes_encryption(zip::AesMode::Aes256, pw),
        None => opts,
    };
    let mut buf = vec![0u8; 256 * 1024];
    for s in srcs {
        let Some(name) = s.file_name().map(|n| n.to_string_lossy().into_owned()) else { continue };
        add_tree(&mut zw, s, &name, opts, p, ctx, &mut buf)?;
    }
    zw.finish().map_err(|e| e.to_string())?;
    Ok(())
}

fn targz_paths(srcs: &[PathBuf], out: &Path, o: &CompressOpts, p: &Progress, ctx: &egui::Context) -> Result<(), String> {
    let f = File::create(out).map_err(|e| e.to_string())?;
    let gz = flate2::write::GzEncoder::new(io::BufWriter::new(f), flate2::Compression::new(o.level.min(9)));
    let mut b = tar::Builder::new(gz);
    for s in srcs {
        let Some(name) = s.file_name().map(|n| n.to_string_lossy().into_owned()) else { continue };
        tar_tree(&mut b, s, &name, p, ctx)?;
    }
    let gz = b.into_inner().map_err(|e| e.to_string())?;
    let mut w = gz.finish().map_err(|e| e.to_string())?;
    w.flush().map_err(|e| e.to_string())?;
    Ok(())
}

fn tar_tree<W: Write>(b: &mut tar::Builder<W>, path: &Path, name: &str, p: &Progress, ctx: &egui::Context) -> Result<(), String> {
    if p.cancelled() {
        return Err("cancelado".into());
    }
    let md = fs::symlink_metadata(path).map_err(|e| format!("{name}: {e}"))?;
    if md.is_dir() {
        b.append_dir(name, path).map_err(|e| format!("{name}: {e}"))?;
        p.done.fetch_add(1, AO::Relaxed);
        let rd = fs::read_dir(path).map_err(|e| format!("{name}: {e}"))?;
        for e in rd.flatten() {
            let child = format!("{name}/{}", e.file_name().to_string_lossy());
            tar_tree(b, &e.path(), &child, p, ctx)?;
        }
    } else if md.is_file() {
        let mut h = tar::Header::new_gnu();
        h.set_metadata(&md);
        // archivo de un servidor SFTP: sin descargar sería un marcador vacío
        crate::sftp::ensure_local_with(path, &p.cancel, &mut |_| {}).map_err(|e| format!("{name}: {e}"))?;
        let f = File::open(path).map_err(|e| format!("{name}: {e}"))?;
        b.append_data(&mut h, name, ProgRead { inner: f, p }).map_err(|e| format!("{name}: {e}"))?;
        p.done.fetch_add(1, AO::Relaxed);
        ctx.request_repaint();
    }
    Ok(())
}

fn sevenz_paths(srcs: &[PathBuf], out: &Path, o: &CompressOpts, p: &Progress, ctx: &egui::Context) -> Result<(), String> {
    use sevenz_rust2::{encoder_options::{AesEncoderOptions, Lzma2Options}, ArchiveWriter, EncoderConfiguration, EncoderMethod, Password};
    let f = File::create(out).map_err(|e| e.to_string())?;
    let mut w = ArchiveWriter::new(io::BufWriter::new(f)).map_err(|e| e.to_string())?;
    let main: EncoderConfiguration = if o.level == 0 { EncoderMethod::COPY.into() } else { Lzma2Options::from_level(o.level.min(9)).into() };
    let mut methods = Vec::new();
    if let Some(pw) = &o.password {
        methods.push(AesEncoderOptions::new(Password::from(pw.as_str())).into());
    }
    methods.push(main);
    w.set_content_methods(methods);
    for s in srcs {
        let Some(name) = s.file_name().map(|n| n.to_string_lossy().into_owned()) else { continue };
        sz_tree(&mut w, s, &name, p, ctx)?;
    }
    w.finish().map_err(|e| e.to_string())?;
    Ok(())
}

fn sz_tree<W: Write + io::Seek>(w: &mut sevenz_rust2::ArchiveWriter<W>, path: &Path, name: &str, p: &Progress, ctx: &egui::Context) -> Result<(), String> {
    use sevenz_rust2::ArchiveEntry;
    if p.cancelled() {
        return Err("cancelado".into());
    }
    let md = fs::symlink_metadata(path).map_err(|e| format!("{name}: {e}"))?;
    if md.is_dir() {
        w.push_archive_entry(ArchiveEntry::new_directory(name), None::<File>).map_err(|e| format!("{name}: {e}"))?;
        p.done.fetch_add(1, AO::Relaxed);
        let rd = fs::read_dir(path).map_err(|e| format!("{name}: {e}"))?;
        for e in rd.flatten() {
            let child = format!("{name}/{}", e.file_name().to_string_lossy());
            sz_tree(w, &e.path(), &child, p, ctx)?;
        }
    } else if md.is_file() {
        // archivo de un servidor SFTP: sin descargar sería un marcador vacío
        crate::sftp::ensure_local_with(path, &p.cancel, &mut |_| {}).map_err(|e| format!("{name}: {e}"))?;
        let f = File::open(path).map_err(|e| format!("{name}: {e}"))?;
        w.push_archive_entry(ArchiveEntry::from_path(path, name.to_string()), Some(ProgRead { inner: f, p })).map_err(|e| format!("{name}: {e}"))?;
        p.done.fetch_add(1, AO::Relaxed);
        ctx.request_repaint();
    }
    Ok(())
}

fn add_tree<W: Write + io::Seek>(
    zw: &mut zip::ZipWriter<W>,
    path: &Path,
    name: &str,
    opts: zip::write::FileOptions<'_, '_, ()>,
    p: &Progress,
    ctx: &egui::Context,
    buf: &mut [u8],
) -> Result<(), String> {
    if p.cancelled() {
        return Err("cancelado".into());
    }
    let md = fs::symlink_metadata(path).map_err(|e| format!("{name}: {e}"))?;
    if md.is_dir() {
        zw.add_directory(format!("{name}/"), opts).map_err(|e| e.to_string())?;
        p.done.fetch_add(1, AO::Relaxed);
        let rd = fs::read_dir(path).map_err(|e| format!("{name}: {e}"))?;
        for e in rd.flatten() {
            let child = format!("{name}/{}", e.file_name().to_string_lossy());
            add_tree(zw, &e.path(), &child, opts, p, ctx, buf)?;
        }
    } else if md.is_file() {
        zw.start_file(name, opts).map_err(|e| e.to_string())?;
        // archivo de un servidor SFTP: sin descargar sería un marcador vacío
        crate::sftp::ensure_local_with(path, &p.cancel, &mut |_| {}).map_err(|e| format!("{name}: {e}"))?;
        let mut f = File::open(path).map_err(|e| format!("{name}: {e}"))?;
        loop {
            let n = f.read(buf).map_err(|e| format!("{name}: {e}"))?;
            if n == 0 {
                break;
            }
            zw.write_all(&buf[..n]).map_err(|e| e.to_string())?;
            p.done.fetch_add(n as u64, AO::Relaxed);
        }
        p.done.fetch_add(1, AO::Relaxed);
        ctx.request_repaint();
    }
    Ok(())
}

/// Extrae `src` dentro de `dst` (carpeta ya creada). Devuelve los nombres de primer nivel.
pub fn unzip_to(src: &Path, dst: &Path, p: &Progress, ctx: &egui::Context, pass: Option<&str>) -> Result<Vec<String>, String> {
    let f = File::open(src).map_err(|e| e.to_string())?;
    let mut ar = zip::ZipArchive::new(io::BufReader::new(f)).map_err(|e| e.to_string())?;
    p.total.store(ar.len() as u64, AO::Relaxed);
    fs::create_dir_all(dst).map_err(|e| e.to_string())?;
    let mut top: Vec<String> = Vec::new();
    let mut written = 0u64;
    for i in 0..ar.len() {
        if p.cancelled() {
            return Err("cancelado".into());
        }
        let bad = |e: String| -> String {
            let m = e.to_lowercase();
            if m.contains("password required") {
                if pass.is_some() { crate::archive::BAD_PASS.into() } else { crate::archive::NEED_PASS.into() }
            } else if m.contains("password") && m.contains("incorrect") {
                crate::archive::BAD_PASS.into()
            } else {
                e
            }
        };
        let mut zf = match pass {
            Some(pw) => ar.by_index_decrypt(i, pw.as_bytes()),
            None => ar.by_index(i),
        }
        .map_err(|e| bad(e.to_string()))?;
        let encrypted = zf.encrypted();
        let Some(rel) = zf.enclosed_name() else { continue }; // descarta rutas peligrosas (../)
        if let Some(first) = rel.components().next() {
            let n = first.as_os_str().to_string_lossy().into_owned();
            if !top.contains(&n) {
                top.push(n);
            }
        }
        let target = dst.join(&rel);
        if zf.is_dir() {
            fs::create_dir_all(&target).map_err(|e| e.to_string())?;
        } else {
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            let mut out = File::create(&target).map_err(|e| format!("{}: {e}", rel.display()))?;
            // el tamaño declarado puede mentir: se cuenta lo que realmente se escribe
            let room = crate::archive::MAX_EXPANDED.saturating_sub(written);
            let r = io::copy(&mut (&mut zf).take(room.saturating_add(1)), &mut out);
            if let Ok(n) = &r {
                written += n;
                if written > crate::archive::MAX_EXPANDED {
                    return Err("el contenido descomprimido supera el límite permitido".into());
                }
            }
            if let Err(e) = r {
                // ZipCrypto con contraseña errónea sólo falla al comprobar el CRC
                return Err(if encrypted && pass.is_some() { crate::archive::BAD_PASS.into() } else { format!("{}: {e}", rel.display()) });
            }
        }
        p.done.fetch_add(1, AO::Relaxed);
        if i % 32 == 0 {
            ctx.request_repaint();
        }
    }
    Ok(top)
}

// ---------------------------------------------------------------------------
// Estadísticas de carpeta (propiedades)
// ---------------------------------------------------------------------------

#[derive(Default)]
pub struct Stats {
    pub bytes: AtomicU64,
    pub files: AtomicU64,
    pub dirs: AtomicU64,
    pub done: AtomicBool,
    pub cancel: AtomicBool,
}

pub fn spawn_stats(paths: Vec<PathBuf>, ctx: egui::Context) -> Arc<Stats> {
    let st = Arc::new(Stats::default());
    let s = st.clone();
    std::thread::spawn(move || {
        let mut last = std::time::Instant::now();
        let mut stack = paths;
        while let Some(p) = stack.pop() {
            if s.cancel.load(AO::Relaxed) {
                return;
            }
            let Ok(md) = fs::symlink_metadata(&p) else { continue };
            if md.is_dir() {
                s.dirs.fetch_add(1, AO::Relaxed);
                if let Ok(rd) = fs::read_dir(&p) {
                    stack.extend(rd.flatten().map(|e| e.path()));
                }
            } else {
                s.files.fetch_add(1, AO::Relaxed);
                s.bytes.fetch_add(md.len(), AO::Relaxed);
            }
            if last.elapsed().as_millis() > 120 {
                last = std::time::Instant::now();
                ctx.request_repaint();
            }
        }
        s.done.store(true, AO::Release);
        ctx.request_repaint();
    });
    st
}

// ---------------------------------------------------------------------------
// Búsqueda recursiva por nombre
// ---------------------------------------------------------------------------

pub const MAX_RESULTS: usize = 5000;

#[derive(Default)]
pub struct Search {
    pub results: Mutex<Vec<(PathBuf, bool)>>,
    pub scanned: AtomicU64,
    pub done: AtomicBool,
    pub cancel: AtomicBool,
}

/// Raíces para «todo el equipo».
pub fn all_roots() -> Vec<PathBuf> {
    #[cfg(windows)]
    {
        (b'A'..=b'Z').map(|c| PathBuf::from(format!("{}:\\", c as char))).filter(|p| p.exists()).collect()
    }
    #[cfg(all(not(windows), not(target_os = "android")))]
    {
        vec![PathBuf::from("/")]
    }
    // Android: sólo el almacenamiento compartido y las tarjetas SD (el resto no es legible ni útil)
    #[cfg(target_os = "android")]
    {
        let mut v = vec![PathBuf::from("/storage/emulated/0")];
        if let Ok(rd) = std::fs::read_dir("/storage") {
            for e in rd.flatten() {
                let n = e.file_name().to_string_lossy().into_owned();
                if n != "emulated" && n != "self" && e.path().is_dir() {
                    v.push(e.path());
                }
            }
        }
        v
    }
}

pub fn pseudo_fs(p: &Path) -> bool {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        matches!(p.to_str(), Some("/proc" | "/sys" | "/dev" | "/run" | "/snap"))
    }
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    {
        let _ = p;
        false
    }
}

pub fn spawn_search(roots: Vec<PathBuf>, needle: String, hidden: bool, ctx: egui::Context) -> Arc<Search> {
    let st = Arc::new(Search::default());
    let s = st.clone();
    std::thread::spawn(move || {
        let needle = needle.to_lowercase();
        let mut stack = roots;
        let mut last = std::time::Instant::now();
        let mut found = 0usize;
        'outer: while let Some(dir) = stack.pop() {
            let Ok(rd) = fs::read_dir(&dir) else { continue };
            for e in rd.flatten() {
                if s.cancel.load(AO::Relaxed) {
                    return;
                }
                let name = e.file_name();
                let name = name.to_string_lossy();
                if !hidden && name.starts_with('.') {
                    continue;
                }
                s.scanned.fetch_add(1, AO::Relaxed);
                let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
                if crate::fsys::contains_ci(&name, &needle) {
                    s.results.lock().unwrap().push((e.path(), is_dir));
                    found += 1;
                    if found >= MAX_RESULTS {
                        break 'outer;
                    }
                }
                if is_dir && !pseudo_fs(&e.path()) {
                    stack.push(e.path());
                }
                if last.elapsed().as_millis() > 150 {
                    last = std::time::Instant::now();
                    ctx.request_repaint();
                }
            }
        }
        s.done.store(true, AO::Release);
        ctx.request_repaint();
    });
    st
}

// ---------------------------------------------------------------------------
// Portapapeles de archivos del sistema (Linux: wl-copy/xclip; Windows: CF_HDROP; macOS: NSPasteboard)
// ---------------------------------------------------------------------------

#[cfg(target_os = "linux")]
mod clip_impl {
    use super::*;
    use std::process::{Command, Stdio};
    use std::time::Duration;

    const MIME: &str = "x-special/gnome-copied-files";

    fn have(bin: &str) -> bool {
        std::env::var_os("PATH")
            .map(|p| std::env::split_paths(&p).any(|d| d.join(bin).is_file()))
            .unwrap_or(false)
    }

    fn wayland() -> bool {
        std::env::var_os("WAYLAND_DISPLAY").map(|v| !v.is_empty()).unwrap_or(false)
    }

    fn encode(p: &Path) -> String {
        use std::os::unix::ffi::OsStrExt;
        let mut s = String::from("file://");
        for &b in p.as_os_str().as_bytes() {
            if b.is_ascii_alphanumeric() || b"/-_.~".contains(&b) {
                s.push(b as char);
            } else {
                s.push_str(&format!("%{b:02X}"));
            }
        }
        s
    }

    fn decode(uri: &str) -> Option<PathBuf> {
        use std::os::unix::ffi::OsStringExt;
        let rest = uri.strip_prefix("file://")?;
        let rest = rest.strip_prefix("localhost").unwrap_or(rest);
        let b = rest.as_bytes();
        let mut out = Vec::with_capacity(b.len());
        let mut i = 0;
        while i < b.len() {
            if b[i] == b'%' && i + 2 < b.len() {
                if let Ok(v) = u8::from_str_radix(if b[i + 1..i + 3].iter().all(u8::is_ascii_hexdigit) { std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or("zz") } else { "zz" }, 16) {
                    out.push(v);
                    i += 3;
                    continue;
                }
            }
            out.push(b[i]);
            i += 1;
        }
        Some(PathBuf::from(std::ffi::OsString::from_vec(out)))
    }

    pub fn set_files(paths: &[PathBuf], cut: bool) -> bool {
        let tool: (&str, Vec<&str>) = if wayland() && have("wl-copy") {
            ("wl-copy", vec!["--type", MIME])
        } else if have("xclip") {
            ("xclip", vec!["-selection", "clipboard", "-t", MIME, "-i"])
        } else {
            return false;
        };
        let mut payload = String::from(if cut { "cut" } else { "copy" });
        for p in paths {
            payload.push('\n');
            payload.push_str(&encode(p));
        }
        let Ok(mut child) = Command::new(tool.0)
            .args(&tool.1)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        else {
            return false;
        };
        if let Some(mut si) = child.stdin.take() {
            let _ = si.write_all(payload.as_bytes());
        }
        // wl-copy/xclip se quedan en segundo plano sirviendo el portapapeles
        std::thread::spawn(move || {
            let _ = child.wait();
        });
        true
    }

    fn capture(bin: &str, args: &[&str]) -> Option<String> {
        let mut child = Command::new(bin)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        let mut out = child.stdout.take()?;
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut s = String::new();
            let _ = out.read_to_string(&mut s);
            let _ = tx.send(s);
        });
        match rx.recv_timeout(Duration::from_millis(600)) {
            Ok(s) => {
                let _ = child.wait();
                Some(s)
            }
            Err(_) => {
                let _ = child.kill();
                None
            }
        }
    }

    pub fn get_files() -> Option<(Vec<PathBuf>, bool)> {
        let txt = if wayland() && have("wl-paste") {
            capture("wl-paste", &["--no-newline", "--type", MIME])
                .filter(|s| !s.trim().is_empty())
                .or_else(|| capture("wl-paste", &["--no-newline", "--type", "text/uri-list"]))
        } else if have("xclip") {
            capture("xclip", &["-selection", "clipboard", "-o", "-t", MIME])
                .filter(|s| !s.trim().is_empty())
                .or_else(|| capture("xclip", &["-selection", "clipboard", "-o", "-t", "text/uri-list"]))
        } else {
            None
        }?;
        let mut cut = false;
        let mut v = Vec::new();
        for l in txt.lines() {
            let l = l.trim();
            match l {
                "cut" => cut = true,
                "copy" | "" => {}
                _ => {
                    if let Some(p) = decode(l) {
                        if fs::symlink_metadata(&p).is_ok() {
                            v.push(p);
                        }
                    }
                }
            }
        }
        if v.is_empty() { None } else { Some((v, cut)) }
    }
}

/// Windows: CF_HDROP + «Preferred DropEffect» (1 copiar, 2 mover), como el Explorador.
#[cfg(windows)]
mod clip_impl {
    use super::*;
    use std::ffi::c_void;
    use std::os::windows::ffi::{OsStrExt, OsStringExt};

    #[link(name = "user32")]
    unsafe extern "system" {
        fn OpenClipboard(hwnd: isize) -> i32;
        fn CloseClipboard() -> i32;
        fn EmptyClipboard() -> i32;
        fn SetClipboardData(fmt: u32, h: *mut c_void) -> *mut c_void;
        fn GetClipboardData(fmt: u32) -> *mut c_void;
        fn IsClipboardFormatAvailable(fmt: u32) -> i32;
        fn RegisterClipboardFormatW(name: *const u16) -> u32;
        fn GetActiveWindow() -> isize;
        fn GetForegroundWindow() -> isize;
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GlobalAlloc(flags: u32, bytes: usize) -> *mut c_void;
        fn GlobalLock(h: *mut c_void) -> *mut c_void;
        fn GlobalUnlock(h: *mut c_void) -> i32;
        fn GlobalFree(h: *mut c_void) -> *mut c_void;
    }
    #[link(name = "shell32")]
    unsafe extern "system" {
        fn DragQueryFileW(hdrop: *mut c_void, i: u32, buf: *mut u16, cch: u32) -> u32;
    }

    const CF_HDROP: u32 = 15;
    const GHND: u32 = 0x0042; // GMEM_MOVEABLE | GMEM_ZEROINIT

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn effect_format() -> u32 {
        unsafe { RegisterClipboardFormatW(wide("Preferred DropEffect").as_ptr()) }
    }

    /// Abre el portapapeles (con reintentos: otra aplicación puede tenerlo un instante).
    fn open() -> bool {
        unsafe {
            let mut hwnd = GetActiveWindow();
            if hwnd == 0 {
                hwnd = GetForegroundWindow();
            }
            for _ in 0..10 {
                if OpenClipboard(hwnd) != 0 {
                    return true;
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            false
        }
    }

    unsafe fn global_from(bytes: &[u8]) -> *mut c_void {
        let h = GlobalAlloc(GHND, bytes.len().max(1));
        if h.is_null() {
            return h;
        }
        let p = GlobalLock(h) as *mut u8;
        if p.is_null() {
            GlobalFree(h);
            return std::ptr::null_mut();
        }
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), p, bytes.len());
        GlobalUnlock(h);
        h
    }

    pub fn set_files(paths: &[PathBuf], cut: bool) -> bool {
        if paths.is_empty() || !open() {
            return false;
        }
        unsafe {
            // DROPFILES: pFiles (20), pt (0,0), fNC (0), fWide (1); después, rutas UTF-16 y doble NUL
            let mut buf: Vec<u8> = Vec::new();
            buf.extend_from_slice(&20u32.to_le_bytes());
            buf.extend_from_slice(&[0u8; 12]);
            buf.extend_from_slice(&1u32.to_le_bytes());
            for p in paths {
                for u in p.as_os_str().encode_wide() {
                    buf.extend_from_slice(&u.to_le_bytes());
                }
                buf.extend_from_slice(&[0, 0]);
            }
            buf.extend_from_slice(&[0, 0]);
            let h = global_from(&buf);
            let h2 = global_from(&(if cut { 2u32 } else { 1u32 }).to_le_bytes());
            let mut ok = false;
            if !h.is_null() {
                EmptyClipboard();
                ok = !SetClipboardData(CF_HDROP, h).is_null();
                if !ok {
                    GlobalFree(h);
                }
                if !h2.is_null() && ok {
                    if SetClipboardData(effect_format(), h2).is_null() {
                        GlobalFree(h2);
                    }
                }
            }
            CloseClipboard();
            ok
        }
    }

    pub fn get_files() -> Option<(Vec<PathBuf>, bool)> {
        unsafe {
            if IsClipboardFormatAvailable(CF_HDROP) == 0 || !open() {
                return None;
            }
            let mut out: Vec<PathBuf> = Vec::new();
            let mut cut = false;
            let h = GetClipboardData(CF_HDROP);
            if !h.is_null() {
                let n = DragQueryFileW(h, 0xFFFF_FFFF, std::ptr::null_mut(), 0);
                for i in 0..n {
                    let len = DragQueryFileW(h, i, std::ptr::null_mut(), 0) as usize;
                    let mut b = vec![0u16; len + 1];
                    let got = DragQueryFileW(h, i, b.as_mut_ptr(), (len + 1) as u32) as usize;
                    out.push(PathBuf::from(std::ffi::OsString::from_wide(&b[..got.min(len)])));
                }
            }
            let fmt = effect_format();
            if fmt != 0 && IsClipboardFormatAvailable(fmt) != 0 {
                let h2 = GetClipboardData(fmt);
                if !h2.is_null() {
                    let p = GlobalLock(h2) as *const u32;
                    if !p.is_null() {
                        cut = *p & 2 != 0;
                        GlobalUnlock(h2);
                    }
                }
            }
            CloseClipboard();
            if out.is_empty() {
                None
            } else {
                Some((out, cut))
            }
        }
    }
}

/// macOS: NSPasteboard a través de JavaScript for Automation (`osascript -l JavaScript`); el
/// Finder no tiene «cortar» en el portapapeles, así que sólo se comparte la lista de archivos.
#[cfg(target_os = "macos")]
mod clip_impl {
    use super::*;
    use std::process::{Command, Stdio};

    const SET: &str = r#"function run(argv) {
  ObjC.import('AppKit');
  var pb = $.NSPasteboard.generalPasteboard;
  pb.clearContents;
  var arr = $.NSMutableArray.alloc.init;
  for (var i = 0; i < argv.length; i++) { arr.addObject($.NSURL.fileURLWithPath(argv[i])); }
  return pb.writeObjects(arr) ? "ok" : "fail";
}"#;

    const GET: &str = r#"function run(argv) {
  ObjC.import('AppKit');
  var pb = $.NSPasteboard.generalPasteboard;
  var opts = $({ NSPasteboardURLReadingFileURLsOnlyKey: true });
  var urls = pb.readObjectsForClassesOptions($([$.NSURL]), opts);
  var out = [];
  if (urls) { for (var i = 0; i < urls.count; i++) { out.push(ObjC.unwrap(urls.objectAtIndex(i).path)); } }
  return out.join("
");
}"#;

    pub fn set_files(paths: &[PathBuf], _cut: bool) -> bool {
        if paths.is_empty() {
            return false;
        }
        Command::new("osascript")
            .args(["-l", "JavaScript", "-e", SET])
            .args(paths)
            .stdin(Stdio::null())
            .output()
            .map(|o| o.status.success() && String::from_utf8_lossy(&o.stdout).trim() == "ok")
            .unwrap_or(false)
    }

    pub fn get_files() -> Option<(Vec<PathBuf>, bool)> {
        let o = Command::new("osascript")
            .args(["-l", "JavaScript", "-e", GET])
            .stdin(Stdio::null())
            .output()
            .ok()?;
        if !o.status.success() {
            return None;
        }
        let v: Vec<PathBuf> = String::from_utf8_lossy(&o.stdout)
            .lines()
            .map(|l| l.trim())
            .filter(|l| !l.is_empty())
            .map(PathBuf::from)
            .collect();
        if v.is_empty() {
            None
        } else {
            Some((v, false))
        }
    }
}

#[cfg(not(any(target_os = "linux", windows, target_os = "macos")))]
mod clip_impl {
    use super::*;
    pub fn set_files(_p: &[PathBuf], _cut: bool) -> bool {
        false
    }
    pub fn get_files() -> Option<(Vec<PathBuf>, bool)> {
        None
    }
}

pub use clip_impl::{get_files as clip_get_files, set_files as clip_set_files};

#[cfg(test)]
mod compress_tests {
    use super::*;

    fn tree(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("fb_cmp_{}_{tag}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(d.join("src/sub")).unwrap();
        fs::write(d.join("src/a.txt"), b"hola mundo ".repeat(500)).unwrap();
        fs::write(d.join("src/sub/b.bin"), (0..=255u8).cycle().take(70_000).collect::<Vec<_>>()).unwrap();
        d
    }

    fn same(a: &Path, b: &Path) {
        assert_eq!(fs::read(a.join("a.txt")).unwrap(), fs::read(b.join("a.txt")).unwrap());
        assert_eq!(fs::read(a.join("sub/b.bin")).unwrap(), fs::read(b.join("sub/b.bin")).unwrap());
    }

    #[test]
    fn roundtrip_all_formats_and_levels() {
        let d = tree("rt");
        let ctx = egui::Context::default();
        for fmt in [ArchFmt::Zip, ArchFmt::TarGz, ArchFmt::SevenZ] {
            for level in [0u32, 1, 6, 9] {
                let out = d.join(format!("o_{level}.{}", fmt.ext()));
                let o = CompressOpts { fmt, level, password: None };
                compress_paths(&[d.join("src")], &out, &o, &Progress::default(), &ctx).unwrap();
                let dst = d.join(format!("x_{level}_{}", fmt.ext()));
                crate::archive::extract_all(&out, &dst, &Progress::default(), &ctx, None).unwrap();
                same(&d.join("src"), &dst.join("src"));
            }
        }
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn encrypted_zip_and_7z() {
        let d = tree("enc");
        let ctx = egui::Context::default();
        let o = CompressOpts { fmt: ArchFmt::Zip, level: 6, password: Some("clave".into()) };
        let zp = d.join("e.zip");
        compress_paths(&[d.join("src")], &zp, &o, &Progress::default(), &ctx).unwrap();
        let mut ar = zip::ZipArchive::new(File::open(&zp).unwrap()).unwrap();
        assert!(ar.by_name("src/a.txt").is_err() || ar.by_name("src/a.txt").unwrap().read_to_end(&mut Vec::new()).is_err());
        let mut v = Vec::new();
        ar.by_name_decrypt("src/a.txt", b"clave").unwrap().read_to_end(&mut v).unwrap();
        assert_eq!(v, fs::read(d.join("src/a.txt")).unwrap());

        let o = CompressOpts { fmt: ArchFmt::SevenZ, level: 6, password: Some("clave".into()) };
        let sp = d.join("e.7z");
        compress_paths(&[d.join("src")], &sp, &o, &Progress::default(), &ctx).unwrap();
        let dst = d.join("x7");
        sevenz_rust2::decompress_file_with_password(&sp, &dst, "clave".into()).unwrap();
        same(&d.join("src"), &dst.join("src"));
        assert!(sevenz_rust2::decompress_file_with_password(&sp, d.join("bad"), "otra".into()).is_err());
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn extract_asks_for_password() {
        use crate::archive::{extract_all, BAD_PASS, NEED_PASS};
        let d = tree("pw");
        let ctx = egui::Context::default();
        for fmt in [ArchFmt::Zip, ArchFmt::SevenZ] {
            let o = CompressOpts { fmt, level: 6, password: Some("clave".into()) };
            let out = d.join(format!("p.{}", fmt.ext()));
            compress_paths(&[d.join("src")], &out, &o, &Progress::default(), &ctx).unwrap();
            let run = |pw: Option<&str>, n: &str| extract_all(&out, &d.join(n), &Progress::default(), &ctx, pw);
            assert_eq!(run(None, "n").unwrap_err(), NEED_PASS, "{}", fmt.ext());
            assert_eq!(run(Some("otra"), "m").unwrap_err(), BAD_PASS, "{}", fmt.ext());
            let ok = format!("ok_{}", fmt.ext());
            run(Some("clave"), &ok).unwrap();
            same(&d.join("src"), &d.join(&ok).join("src"));
        }
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn cancel_stops() {
        let d = tree("cancel");
        let p = Progress::default();
        p.cancel.store(true, AO::Relaxed);
        let o = CompressOpts { fmt: ArchFmt::TarGz, level: 6, password: None };
        assert!(compress_paths(&[d.join("src")], &d.join("c.tar.gz"), &o, &p, &egui::Context::default()).is_err());
        let _ = fs::remove_dir_all(&d);
    }
}
