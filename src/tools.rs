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
// Portapapeles de archivos del sistema (Linux: wl-copy/xclip; resto: no disponible)
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

#[cfg(not(target_os = "linux"))]
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
