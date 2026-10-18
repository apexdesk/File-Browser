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
    #[cfg(windows)]
    {
        windows_apps(path)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    {
        let _ = path;
        Vec::new()
    }
}

/// Valores de texto de una clave del registro (`reg query`): (nombre, dato). El nombre «(Default)»
/// aparece localizado según el idioma, por eso sólo se fía de la columna REG_*.
#[cfg(windows)]
fn reg_values(key: &str) -> Vec<(String, String)> {
    use std::os::windows::process::CommandExt;
    let Ok(o) = std::process::Command::new("reg").args(["query", key]).creation_flags(0x0800_0000).output() else {
        return Vec::new();
    };
    let txt = String::from_utf8_lossy(&o.stdout).into_owned();
    let mut out = Vec::new();
    for l in txt.lines() {
        let l = l.trim_start();
        if l.is_empty() || l.to_ascii_uppercase().starts_with("HKEY_") {
            continue;
        }
        for ty in ["REG_EXPAND_SZ", "REG_SZ"] {
            if let Some(i) = l.find(&format!("    {ty}    ")) {
                let name = l[..i].trim().to_string();
                let data = l[i + ty.len() + 8..].trim().to_string();
                out.push((name, data));
                break;
            }
        }
    }
    out
}

#[cfg(windows)]
fn reg_default(key: &str) -> Option<String> {
    use std::os::windows::process::CommandExt;
    let o = std::process::Command::new("reg").args(["query", key, "/ve"]).creation_flags(0x0800_0000).output().ok()?;
    let txt = String::from_utf8_lossy(&o.stdout).into_owned();
    for l in txt.lines() {
        for ty in ["REG_EXPAND_SZ", "REG_SZ"] {
            if let Some(i) = l.find(&format!("    {ty}    ")) {
                let v = l[i + ty.len() + 8..].trim().to_string();
                if !v.is_empty() {
                    return Some(v);
                }
            }
        }
    }
    None
}

#[cfg(windows)]
fn expand_env(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(i) = rest.find('%') {
        out.push_str(&rest[..i]);
        let tail = &rest[i + 1..];
        match tail.find('%') {
            Some(j) if j > 0 && std::env::var(&tail[..j]).is_ok() => {
                out.push_str(&std::env::var(&tail[..j]).unwrap());
                rest = &tail[j + 1..];
            }
            _ => {
                out.push('%');
                rest = tail;
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(windows)]
fn windows_apps(path: &Path) -> Vec<App> {
    let Some(ext) = path.extension().map(|e| e.to_string_lossy().to_lowercase()) else { return Vec::new() };
    if ext.is_empty() || ext.contains(['"', '\\', ' ']) {
        return Vec::new();
    }
    let mut cmds: Vec<(String, String)> = Vec::new(); // (nombre, línea de órden)
    let mut add = |name: String, cmd: String| {
        let exe = cmd.to_lowercase();
        if !cmds.iter().any(|(_, c)| c.to_lowercase() == exe) {
            cmds.push((name, cmd));
        }
    };
    // programas registrados por nombre de ejecutable
    let mut exes: Vec<String> = Vec::new();
    for key in [
        format!(r"HKCU\Software\Microsoft\Windows\CurrentVersion\Explorer\FileExts\.{ext}\OpenWithList"),
        format!(r"HKCR\.{ext}\OpenWithList"),
    ] {
        for (n, v) in reg_values(&key) {
            if n.len() == 1 && v.to_lowercase().ends_with(".exe") && !exes.iter().any(|e| e.eq_ignore_ascii_case(&v)) {
                exes.push(v);
            }
        }
    }
    for exe in exes {
        let key = format!(r"HKCR\Applications\{exe}\shell\open\command");
        if let Some(c) = reg_default(&key) {
            let name = exe.trim_end_matches(".exe").trim_end_matches(".EXE").to_string();
            add(name, expand_env(&c));
        }
    }
    // programas asociados por ProgID
    let mut progids: Vec<String> = reg_values(&format!(r"HKCR\.{ext}\OpenWithProgids")).into_iter().map(|(n, _)| n).collect();
    if let Some(d) = reg_default(&format!(r"HKCR\.{ext}")) {
        progids.push(d);
    }
    for id in progids {
        if id.is_empty() {
            continue;
        }
        if let Some(c) = reg_default(&format!(r"HKCR\{id}\shell\open\command")) {
            let name = reg_default(&format!(r"HKCR\{id}")).unwrap_or_else(|| id.clone());
            add(name, expand_env(&c));
        }
    }
    cmds.into_iter().map(|(name, exec)| App { name, exec }).collect()
}

/// Lanza una línea de órden del registro sustituyendo %1/%L por el archivo.
#[cfg(windows)]
fn launch_registry_cmd(line: &str, path: &Path) {
    use std::os::windows::process::CommandExt;
    let line = line.trim();
    let (exe, rest) = if let Some(r) = line.strip_prefix('"') {
        match r.find('"') {
            Some(i) => (r[..i].to_string(), r[i + 1..].to_string()),
            None => return,
        }
    } else {
        match line.find(' ') {
            Some(i) => (line[..i].to_string(), line[i..].to_string()),
            None => (line.to_string(), String::new()),
        }
    };
    let q = format!("\"{}\"", path.display());
    // si el comando ya entrecomilla «%1», no se duplican las comillas
    let mut args = rest.clone();
    for tok in ["%1", "%L", "%l", "%*", "%0"] {
        args = args.replace(&format!("\"{tok}\""), &q).replace(tok, &q);
    }
    if !rest.contains('%') {
        args.push(' ');
        args.push_str(&q);
    }
    let _ = std::process::Command::new(exe).raw_arg(args.trim()).creation_flags(0x0800_0000).spawn();
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
                    let raw = path.display().to_string();
                    let mut s = String::new();
                    let mut it = a.exec.chars().peekable();
                    let mut used = false;
                    let (mut in_dq, mut in_sq) = (false, false);
                    while let Some(c) = it.next() {
                        if c == '%' {
                            match it.next() {
                                Some('f' | 'F' | 'u' | 'U') => {
                                    // la ruta se cita según el contexto en que cae (un nombre con `$(…)` no debe ejecutarse)
                                    if in_dq {
                                        for ch in raw.chars() {
                                            if matches!(ch, '\\' | '$' | '`' | '"') {
                                                s.push('\\');
                                            }
                                            s.push(ch);
                                        }
                                    } else if in_sq {
                                        s.push_str(&raw.replace('\'', "'\\''"));
                                    } else {
                                        s.push_str(&q);
                                    }
                                    used = true;
                                }
                                Some('%') => s.push('%'),
                                _ => {}
                            }
                        } else {
                            match c {
                                '"' if !in_sq => in_dq = !in_dq,
                                '\'' if !in_dq => in_sq = !in_sq,
                                _ => {}
                            }
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
        if let Some(a) = app {
            launch_registry_cmd(&a.exec, path);
            return;
        }
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
    cfg!(any(target_os = "linux", windows, target_os = "macos"))
}

#[derive(Clone)]
pub struct TrashRow {
    pub name: String,
    pub parent: PathBuf,
    pub time: i64,
    pub size: u64,
    pub dir: bool,
    #[cfg(any(target_os = "linux", windows))]
    pub item: trash::TrashItem,
    /// macOS: ubicación actual dentro de ~/.Trash.
    #[cfg(target_os = "macos")]
    pub path: PathBuf,
}

/// Envía un elemento a la papelera. En macOS el sistema no guarda de dónde venía (sólo Finder lo
/// sabe), así que se anota aparte para poder listarlo y restaurarlo igual que en Linux y Windows.
pub fn trash_delete(p: &Path) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    mac_trash::note(p);
    #[cfg(not(target_os = "android"))]
    {
        trash::delete(p).map_err(|e| e.to_string())
    }
    #[cfg(target_os = "android")]
    {
        crate::trash::delete(p)
    }
}

#[cfg(target_os = "macos")]
mod mac_trash {
    use super::*;
    use std::os::unix::fs::MetadataExt;

    fn log_path() -> Option<PathBuf> {
        Some(dirs::data_dir()?.join("filebrowser").join("trash_origins.tsv"))
    }

    pub fn trash_dir() -> Option<PathBuf> {
        Some(dirs::home_dir()?.join(".Trash"))
    }

    pub fn note(orig: &Path) {
        let (Some(f), Some(name)) = (log_path(), orig.file_name()) else { return };
        let Ok(abs) = std::path::absolute(orig) else { return };
        let _ = fs::create_dir_all(f.parent().unwrap());
        let line = format!(
            "{}\t{}\t{}\n",
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0),
            name.to_string_lossy().replace(['\t', '\n'], " "),
            abs.to_string_lossy().replace(['\t', '\n'], " ")
        );
        use std::io::Write;
        if let Ok(mut h) = fs::OpenOptions::new().create(true).append(true).open(f) {
            let _ = h.write_all(line.as_bytes());
        }
    }

    /// Nombre sin el sufijo « HH.MM.SS» que Finder añade cuando ya existía uno igual en la papelera.
    fn base_name(n: &str) -> String {
        let (stem, ext) = match n.rfind('.') {
            Some(i) if i > 0 => (&n[..i], &n[i..]),
            _ => (n, ""),
        };
        let b = stem.as_bytes();
        if b.len() > 9 && stem.is_char_boundary(b.len() - 9) {
            let t = &stem[b.len() - 9..];
            let ok = t.as_bytes().iter().enumerate().all(|(i, c)| match i {
                0 => *c == b' ',
                3 | 6 => *c == b'.',
                _ => c.is_ascii_digit(),
            });
            if ok {
                return format!("{}{}", &stem[..b.len() - 9], ext);
            }
        }
        n.to_string()
    }

    pub fn list() -> Result<Vec<TrashRow>, String> {
        let dir = trash_dir().ok_or("sin carpeta personal")?;
        let rd = fs::read_dir(&dir).map_err(|e| format!("{e} (¿falta el permiso de acceso a la papelera?)"))?;
        // registros: nombre → (hora, ruta original), el más reciente primero
        let mut recs: Vec<(u64, String, PathBuf)> = log_path()
            .and_then(|f| fs::read_to_string(f).ok())
            .map(|t| {
                t.lines()
                    .filter_map(|l| {
                        let mut it = l.splitn(3, '\t');
                        Some((it.next()?.parse().ok()?, it.next()?.to_string(), PathBuf::from(it.next()?)))
                    })
                    .collect()
            })
            .unwrap_or_default();
        recs.sort_by(|a, b| b.0.cmp(&a.0));
        let mut used = vec![false; recs.len()];
        let home = dirs::home_dir().unwrap_or_default();
        let mut out = Vec::new();
        let mut ents: Vec<_> = rd.flatten().collect();
        ents.sort_by_key(|e| std::cmp::Reverse(e.metadata().map(|m| m.ctime()).unwrap_or(0)));
        for e in ents {
            let name = e.file_name().to_string_lossy().into_owned();
            if name == ".DS_Store" {
                continue;
            }
            let base = base_name(&name);
            let hit = recs.iter().enumerate().position(|(i, r)| !used[i] && (r.1 == name || r.1 == base));
            let parent = match hit {
                Some(i) => {
                    used[i] = true;
                    recs[i].2.parent().map(|p| p.to_path_buf()).unwrap_or_else(|| home.join("Desktop"))
                }
                None => home.join("Desktop"),
            };
            let md = e.metadata().ok();
            out.push(TrashRow {
                name: if hit.is_some() { base } else { name },
                parent,
                time: md.as_ref().map(|m| m.ctime()).unwrap_or(0),
                size: md.as_ref().filter(|m| m.is_file()).map(|m| m.len()).unwrap_or(0),
                dir: md.as_ref().map(|m| m.is_dir()).unwrap_or(false),
                path: e.path(),
            });
        }
        Ok(out)
    }

    pub fn restore(rows: &[TrashRow]) -> Result<(), String> {
        let mut errs = Vec::new();
        for r in rows {
            let _ = fs::create_dir_all(&r.parent);
            let mut dst = r.parent.join(&r.name);
            if fs::symlink_metadata(&dst).is_ok() {
                let (stem, ext) = match r.name.rfind('.') {
                    Some(i) if i > 0 => (r.name[..i].to_string(), r.name[i..].to_string()),
                    _ => (r.name.clone(), String::new()),
                };
                let mut n = 2;
                loop {
                    dst = r.parent.join(format!("{stem} ({n}){ext}"));
                    if fs::symlink_metadata(&dst).is_err() {
                        break;
                    }
                    n += 1;
                }
            }
            if let Err(e) = fs::rename(&r.path, &dst) {
                errs.push(format!("{}: {e}", r.name));
            }
        }
        if errs.is_empty() { Ok(()) } else { Err(errs.join("\n")) }
    }

    pub fn purge(rows: &[TrashRow]) -> Result<(), String> {
        let mut errs = Vec::new();
        for r in rows {
            let res = match fs::symlink_metadata(&r.path) {
                Ok(m) if m.is_dir() => fs::remove_dir_all(&r.path),
                Ok(_) => fs::remove_file(&r.path),
                Err(e) => Err(e),
            };
            if let Err(e) = res {
                errs.push(format!("{}: {e}", r.name));
            }
        }
        if errs.is_empty() { Ok(()) } else { Err(errs.join("\n")) }
    }
}

#[cfg(any(target_os = "linux", windows))]
pub fn trash_list() -> Result<Vec<TrashRow>, String> {
    let mut v: Vec<TrashRow> = trash::os_limited::list()
        .map_err(|e| e.to_string())?
        .into_iter()
        .map(|it| {
            let (size, dir) = match trash::os_limited::metadata(&it) {
                Ok(m) => match m.size {
                    trash::TrashItemSize::Bytes(b) => (b, false),
                    trash::TrashItemSize::Entries(_) => (0, true),
                },
                Err(_) => (0, false),
            };
            TrashRow {
                name: it.name.to_string_lossy().into_owned(),
                parent: it.original_parent.clone(),
                time: it.time_deleted,
                size,
                dir,
                item: it,
            }
        })
        .collect();
    v.sort_by(|a, b| b.time.cmp(&a.time));
    Ok(v)
}
#[cfg(target_os = "macos")]
pub fn trash_list() -> Result<Vec<TrashRow>, String> {
    mac_trash::list()
}
#[cfg(not(any(target_os = "linux", windows, target_os = "macos")))]
pub fn trash_list() -> Result<Vec<TrashRow>, String> {
    Err("no soportado".into())
}

#[cfg(any(target_os = "linux", windows))]
pub fn trash_restore(rows: &[TrashRow]) -> Result<(), String> {
    trash::os_limited::restore_all(rows.iter().map(|r| r.item.clone())).map_err(|e| e.to_string())
}
#[cfg(target_os = "macos")]
pub fn trash_restore(rows: &[TrashRow]) -> Result<(), String> {
    mac_trash::restore(rows)
}
#[cfg(not(any(target_os = "linux", windows, target_os = "macos")))]
pub fn trash_restore(_rows: &[TrashRow]) -> Result<(), String> {
    Err("no soportado".into())
}

#[cfg(any(target_os = "linux", windows))]
pub fn trash_purge(rows: &[TrashRow]) -> Result<(), String> {
    trash::os_limited::purge_all(rows.iter().map(|r| r.item.clone())).map_err(|e| e.to_string())
}
#[cfg(target_os = "macos")]
pub fn trash_purge(rows: &[TrashRow]) -> Result<(), String> {
    mac_trash::purge(rows)
}
#[cfg(not(any(target_os = "linux", windows, target_os = "macos")))]
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
            crate::sftp::ensure_local_with(&path, &j.cancel, &mut |_| {})?;
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
    crate::sftp::ensure_local(p).ok()?;
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
