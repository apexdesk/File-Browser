//! Utilidades extra: «Abrir con…», papelera, hashes y búsqueda de duplicados.
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

// ---------------------------------------------------------------------------
// Abrir con…
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct App {
    pub name: String,
    /// Línea de órden (Linux: campo Exec; macOS: nombre de la app; Windows: vacío).
    pub exec: String,
}

/// Aplicaciones candidatas para abrir `path`.
pub fn apps_for(path: &Path) -> Vec<App> {
    #[cfg(target_os = "linux")]
    {
        linux_apps(path)
    }
    #[cfg(target_os = "macos")]
    {
        let _ = path;
        let mut v = Vec::new();
        for dir in ["/Applications", "/System/Applications"] {
            if let Ok(rd) = fs::read_dir(dir) {
                for e in rd.flatten() {
                    let n = e.file_name().to_string_lossy().into_owned();
                    if let Some(app) = n.strip_suffix(".app") {
                        v.push(App { name: app.to_string(), exec: app.to_string() });
                    }
                }
            }
        }
        v.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
        v
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = path;
        Vec::new()
    }
}

#[cfg(target_os = "linux")]
fn mime_of(path: &Path) -> Option<String> {
    let o = std::process::Command::new("xdg-mime").args(["query", "filetype"]).arg(path).output().ok()?;
    let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
    if s.is_empty() { None } else { Some(s) }
}

#[cfg(target_os = "linux")]
fn app_dirs() -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Some(d) = dirs::data_dir() {
        v.push(d.join("applications"));
    }
    v.push("/usr/local/share/applications".into());
    v.push("/usr/share/applications".into());
    v.push("/var/lib/flatpak/exports/share/applications".into());
    if let Some(h) = dirs::home_dir() {
        v.push(h.join(".local/share/flatpak/exports/share/applications"));
    }
    v
}

#[cfg(target_os = "linux")]
fn linux_apps(path: &Path) -> Vec<App> {
    let Some(mime) = mime_of(path) else { return Vec::new() };
    let mut ids: Vec<String> = Vec::new();
    for d in app_dirs() {
        if let Ok(txt) = fs::read_to_string(d.join("mimeinfo.cache")) {
            for l in txt.lines() {
                if let Some((m, list)) = l.split_once('=') {
                    if m == mime {
                        for id in list.split(';').filter(|x| !x.is_empty()) {
                            if !ids.iter().any(|x| x == id) {
                                ids.push(id.to_string());
                            }
                        }
                    }
                }
            }
        }
    }
    let mut out = Vec::new();
    for id in ids {
        for d in app_dirs() {
            let f = d.join(&id);
            let Ok(txt) = fs::read_to_string(&f) else { continue };
            let (mut name, mut exec, mut hide) = (None, None, false);
            let mut in_entry = false;
            for l in txt.lines() {
                if l.starts_with('[') {
                    in_entry = l == "[Desktop Entry]";
                } else if in_entry {
                    if let Some(v) = l.strip_prefix("Name=") {
                        name.get_or_insert(v.to_string());
                    } else if let Some(v) = l.strip_prefix("Exec=") {
                        exec.get_or_insert(v.to_string());
                    } else if l == "NoDisplay=true" || l == "Hidden=true" {
                        hide = true;
                    }
                }
            }
            if let (Some(name), Some(exec), false) = (name, exec, hide) {
                out.push(App { name, exec });
            }
            break;
        }
    }
    out
}

fn sh_quote(p: &Path) -> String {
    format!("'{}'", p.display().to_string().replace('\'', "'\\''"))
}

/// Lanza `app` (o el comando personalizado `custom`) sobre `path`.
pub fn launch(app: Option<&App>, custom: &str, path: &Path) {
    #[cfg(unix)]
    {
        let line = match app {
            Some(a) => {
                #[cfg(target_os = "macos")]
                {
                    format!("open -a {} {}", sh_quote(Path::new(&a.exec)), sh_quote(path))
                }
                #[cfg(not(target_os = "macos"))]
                {
                    let q = sh_quote(path);
                    let mut s = String::new();
                    let mut it = a.exec.chars().peekable();
                    let mut used = false;
                    while let Some(c) = it.next() {
                        if c == '%' {
                            match it.next() {
                                Some('f' | 'F' | 'u' | 'U') => {
                                    s.push_str(&q);
                                    used = true;
                                }
                                Some('%') => s.push('%'),
                                _ => {}
                            }
                        } else {
                            s.push(c);
                        }
                    }
                    if !used {
                        s.push(' ');
                        s.push_str(&q);
                    }
                    s
                }
            }
            None => format!("{} {}", custom, sh_quote(path)),
        };
        let _ = std::process::Command::new("sh").arg("-c").arg(format!("{line} >/dev/null 2>&1 &")).spawn();
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let _ = app;
        if custom.trim().is_empty() {
            let _ = std::process::Command::new("rundll32.exe")
                .arg("shell32.dll,OpenAs_RunDLL")
                .arg(path)
                .creation_flags(0x0800_0000)
                .spawn();
        } else {
            let _ = std::process::Command::new(custom.trim()).arg(path).creation_flags(0x0800_0000).spawn();
        }
    }
}

// ---------------------------------------------------------------------------
// Papelera
// ---------------------------------------------------------------------------

pub fn trash_supported() -> bool {
    cfg!(any(target_os = "linux", windows))
}

#[derive(Clone)]
pub struct TrashRow {
    pub name: String,
    pub parent: PathBuf,
    pub time: i64,
    #[cfg(any(target_os = "linux", windows))]
    pub item: trash::TrashItem,
}

#[cfg(any(target_os = "linux", windows))]
pub fn trash_list() -> Result<Vec<TrashRow>, String> {
    let mut v: Vec<TrashRow> = trash::os_limited::list()
        .map_err(|e| e.to_string())?
        .into_iter()
        .map(|it| TrashRow {
            name: it.name.to_string_lossy().into_owned(),
            parent: it.original_parent.clone(),
            time: it.time_deleted,
            item: it,
        })
        .collect();
    v.sort_by(|a, b| b.time.cmp(&a.time));
    Ok(v)
}
#[cfg(not(any(target_os = "linux", windows)))]
pub fn trash_list() -> Result<Vec<TrashRow>, String> {
    Err("no soportado".into())
}

#[cfg(any(target_os = "linux", windows))]
pub fn trash_restore(rows: &[TrashRow]) -> Result<(), String> {
    trash::os_limited::restore_all(rows.iter().map(|r| r.item.clone())).map_err(|e| e.to_string())
}
#[cfg(not(any(target_os = "linux", windows)))]
pub fn trash_restore(_rows: &[TrashRow]) -> Result<(), String> {
    Err("no soportado".into())
}

#[cfg(any(target_os = "linux", windows))]
pub fn trash_purge(rows: &[TrashRow]) -> Result<(), String> {
    trash::os_limited::purge_all(rows.iter().map(|r| r.item.clone())).map_err(|e| e.to_string())
}
#[cfg(not(any(target_os = "linux", windows)))]
pub fn trash_purge(_rows: &[TrashRow]) -> Result<(), String> {
    Err("no soportado".into())
}

/// Restaura de la papelera los elementos cuya ruta original está en `items` (el más reciente de cada ruta).
pub fn restore_paths(items: &[PathBuf]) -> Result<(), String> {
    let all = trash_list()?;
    let mut pick: HashMap<PathBuf, TrashRow> = HashMap::new();
    for r in all {
        let orig = r.parent.join(&r.name);
        if items.contains(&orig) {
            let e = pick.entry(orig).or_insert_with(|| r.clone());
            if r.time > e.time {
                *e = r;
            }
        }
    }
    if pick.is_empty() {
        return Err("no encontrado en la papelera".into());
    }
    let rows: Vec<TrashRow> = pick.into_values().collect();
    trash_restore(&rows)
}

// ---------------------------------------------------------------------------
// Hashes
// ---------------------------------------------------------------------------

#[derive(Default)]
pub struct HashJob {
    pub total: AtomicU64,
    pub done: AtomicU64,
    pub finished: AtomicBool,
    pub cancel: AtomicBool,
    pub result: Mutex<Option<Result<[String; 3], String>>>,
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

pub fn spawn_hash(path: PathBuf, ctx: egui::Context) -> Arc<HashJob> {
    use md5::Digest as _;
    let job = Arc::new(HashJob::default());
    let j = job.clone();
    std::thread::spawn(move || {
        let res = (|| -> Result<[String; 3], String> {
            let mut f = File::open(&path).map_err(|e| e.to_string())?;
            j.total.store(f.metadata().map(|m| m.len()).unwrap_or(0).max(1), Ordering::Relaxed);
            let (mut m, mut s1, mut s2) = (md5::Md5::new(), sha1::Sha1::new(), sha2::Sha256::new());
            let mut buf = vec![0u8; 1 << 20];
            loop {
                if j.cancel.load(Ordering::Relaxed) {
                    return Err("cancelado".into());
                }
                let n = f.read(&mut buf).map_err(|e| e.to_string())?;
                if n == 0 {
                    break;
                }
                m.update(&buf[..n]);
                s1.update(&buf[..n]);
                s2.update(&buf[..n]);
                j.done.fetch_add(n as u64, Ordering::Relaxed);
                ctx.request_repaint();
            }
            Ok([hex(&m.finalize()), hex(&s1.finalize()), hex(&s2.finalize())])
        })();
        *j.result.lock().unwrap() = Some(res);
        j.finished.store(true, Ordering::Release);
        ctx.request_repaint();
    });
    job
}

// ---------------------------------------------------------------------------
// Duplicados
// ---------------------------------------------------------------------------

#[derive(Default)]
pub struct DupJob {
    pub scanned: AtomicU64,
    pub finished: AtomicBool,
    pub cancel: AtomicBool,
    /// Grupos de archivos idénticos (tamaño, rutas).
    pub groups: Mutex<Vec<(u64, Vec<PathBuf>)>>,
}

fn walk(dir: &Path, out: &mut HashMap<u64, Vec<PathBuf>>, j: &DupJob, ctx: &egui::Context) {
    let Ok(rd) = fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        if j.cancel.load(Ordering::Relaxed) {
            return;
        }
        let Ok(ft) = e.file_type() else { continue };
        let p = e.path();
        if ft.is_dir() {
            walk(&p, out, j, ctx);
        } else if ft.is_file() {
            if let Ok(md) = e.metadata() {
                if md.len() > 0 {
                    out.entry(md.len()).or_default().push(p);
                }
            }
            if j.scanned.fetch_add(1, Ordering::Relaxed) % 500 == 0 {
                ctx.request_repaint();
            }
        }
    }
}

fn quick_hash(p: &Path, full: bool) -> Option<[u8; 32]> {
    use sha2::Digest as _;
    let mut f = File::open(p).ok()?;
    let mut h = sha2::Sha256::new();
    let mut buf = vec![0u8; if full { 1 << 20 } else { 64 * 1024 }];
    loop {
        let n = f.read(&mut buf).ok()?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
        if !full {
            break;
        }
    }
    Some(h.finalize().into())
}

pub fn spawn_dups(root: PathBuf, ctx: egui::Context) -> Arc<DupJob> {
    let job = Arc::new(DupJob::default());
    let j = job.clone();
    std::thread::spawn(move || {
        let mut by_size: HashMap<u64, Vec<PathBuf>> = HashMap::new();
        walk(&root, &mut by_size, &j, &ctx);
        for (size, files) in by_size.into_iter().filter(|(_, v)| v.len() > 1) {
            if j.cancel.load(Ordering::Relaxed) {
                break;
            }
            // 1) prefijo de 64 KB, 2) archivo completo
            let mut by_head: HashMap<[u8; 32], Vec<PathBuf>> = HashMap::new();
            for f in files {
                if let Some(h) = quick_hash(&f, false) {
                    by_head.entry(h).or_default().push(f);
                }
            }
            for (_, cand) in by_head.into_iter().filter(|(_, v)| v.len() > 1) {
                let mut by_full: HashMap<[u8; 32], Vec<PathBuf>> = HashMap::new();
                for f in cand {
                    if j.cancel.load(Ordering::Relaxed) {
                        break;
                    }
                    if let Some(h) = quick_hash(&f, true) {
                        by_full.entry(h).or_default().push(f);
                    }
                }
                for (_, g) in by_full.into_iter().filter(|(_, v)| v.len() > 1) {
                    let mut g = g;
                    g.sort();
                    j.groups.lock().unwrap().push((size, g));
                    ctx.request_repaint();
                }
            }
        }
        j.groups.lock().unwrap().sort_by(|a, b| (b.0 * b.1.len() as u64).cmp(&(a.0 * a.1.len() as u64)));
        j.finished.store(true, Ordering::Release);
        ctx.request_repaint();
    });
    job
}
