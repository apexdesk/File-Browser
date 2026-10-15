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

pub fn zip_paths(srcs: &[PathBuf], out: &Path, p: &Progress, ctx: &egui::Context) -> Result<(), String> {
    let f = File::create(out).map_err(|e| e.to_string())?;
    let mut zw = zip::ZipWriter::new(io::BufWriter::new(f));
    let opts = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    let mut buf = vec![0u8; 256 * 1024];
    for s in srcs {
        let Some(name) = s.file_name().map(|n| n.to_string_lossy().into_owned()) else { continue };
        add_tree(&mut zw, s, &name, opts, p, ctx, &mut buf)?;
    }
    zw.finish().map_err(|e| e.to_string())?;
    Ok(())
}

fn add_tree<W: Write + io::Seek>(
    zw: &mut zip::ZipWriter<W>,
    path: &Path,
    name: &str,
    opts: zip::write::SimpleFileOptions,
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
pub fn unzip_to(src: &Path, dst: &Path, p: &Progress, ctx: &egui::Context) -> Result<Vec<String>, String> {
    let f = File::open(src).map_err(|e| e.to_string())?;
    let mut ar = zip::ZipArchive::new(io::BufReader::new(f)).map_err(|e| e.to_string())?;
    p.total.store(ar.len() as u64, AO::Relaxed);
    fs::create_dir_all(dst).map_err(|e| e.to_string())?;
    let mut top: Vec<String> = Vec::new();
    for i in 0..ar.len() {
        if p.cancelled() {
            return Err("cancelado".into());
        }
        let mut zf = ar.by_index(i).map_err(|e| e.to_string())?;
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
            io::copy(&mut zf, &mut out).map_err(|e| format!("{}: {e}", rel.display()))?;
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
    #[cfg(not(windows))]
    {
        vec![PathBuf::from("/")]
    }
}

fn pseudo_fs(p: &Path) -> bool {
    #[cfg(target_os = "linux")]
    {
        matches!(p.to_str(), Some("/proc" | "/sys" | "/dev" | "/run" | "/snap"))
    }
    #[cfg(not(target_os = "linux"))]
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
                if let Ok(v) = u8::from_str_radix(&rest[i + 1..i + 3], 16) {
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
