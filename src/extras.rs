//! Utilidades extra: «Abrir con…», papelera, hashes y búsqueda de duplicados.
#[allow(unused_imports)]
use crate::vfs::{KindV as _, PathV as _};
use std::collections::HashMap;
use crate::vfs::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

// ---------------------------------------------------------------------------
// Abrir con…
// ---------------------------------------------------------------------------

#[derive(Clone, Default)]
pub struct App {
    pub name: String,
    /// Línea de órden (Linux: campo Exec; macOS: nombre de la app; Windows: vacío).
    pub exec: String,
    /// Linux: campo Icon (para `%i`).
    pub icon: String,
    /// Linux: el propio archivo .desktop (para `%k`).
    pub desktop: PathBuf,
    /// Identificador estable: id del .desktop (Linux), ruta del ejecutable o del manejador (Windows),
    /// ruta del paquete .app (macOS).
    pub id: String,
    /// Texto secundario (GenericName o Comment del .desktop).
    pub generic: String,
    /// Linux: `Terminal=true` (se ejecuta dentro del terminal del sistema).
    pub terminal: bool,
    /// Es la aplicación predeterminada para el tipo del archivo consultado.
    pub default: bool,
}

/// Aplicaciones registradas para abrir `path` (la predeterminada primero). Bloquea.
pub fn apps_for(path: &Path) -> Vec<App> {
    crate::sysapps::choices_for(path).recommended
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
pub(crate) fn windows_apps(path: &Path) -> Vec<App> {
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
    cmds.into_iter().map(|(name, exec)| App { name, exec, ..Default::default() }).collect()
}

/// Lanza una línea de órden del registro sustituyendo %1/%L por el archivo.
#[cfg(windows)]
fn launch_registry_cmd(line: &str, path: &Path) -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    let line = line.trim();
    let (exe, rest) = if let Some(r) = line.strip_prefix('"') {
        match r.find('"') {
            Some(i) => (r[..i].to_string(), r[i + 1..].to_string()),
            None => return Err(crate::i18n::t("orden mal formada").into()),
        }
    } else {
        match line.find(' ') {
            Some(i) => (line[..i].to_string(), line[i..].to_string()),
            None => (line.to_string(), String::new()),
        }
    };
    let q = format!("\"{}\"", path.display());
    // si el comando ya entrecomilla «%1», no se duplican las comillas
    // una sola pasada: la ruta ya insertada no se vuelve a interpretar (un nombre con «%L» inyectaría argumentos)
    let mut args = String::with_capacity(rest.len() + q.len());
    let mut i = 0;
    'scan: while i < rest.len() {
        for tok in ["%1", "%L", "%l", "%*", "%0"] {
            let quoted = format!("\"{tok}\"");
            if rest[i..].starts_with(&quoted) {
                args.push_str(&q);
                i += quoted.len();
                continue 'scan;
            }
            if rest[i..].starts_with(tok) {
                args.push_str(&q);
                i += tok.len();
                continue 'scan;
            }
        }
        let c = rest[i..].chars().next().unwrap();
        args.push(c);
        i += c.len_utf8();
    }
    if !rest.contains('%') {
        args.push(' ');
        args.push_str(&q);
    }
    std::process::Command::new(exe).raw_arg(args.trim()).creation_flags(0x0800_0000).spawn().map(|_| ()).map_err(|e| e.to_string())
}

/// Convierte el campo Exec de un .desktop en la lista de argumentos, según la especificación
/// Desktop Entry: primero los escapes generales (`\s`, `\n`, `\t`, `\r`, `\\`), luego las
/// comillas dobles (dentro, `\"`, `` \` ``, `\$` y `\\` son literales) y por último los códigos
/// de campo fuera de comillas: `%f %F %u %U` → la ruta (siempre un argumento propio, sin pasar por
/// ningún intérprete), `%i` → `--icon Icon`, `%c` → el nombre, `%k` → el .desktop, `%%` → `%`.
/// Los obsoletos (`%d %D %n %N %v %m`) y los que caen dentro de comillas se quitan. Si no hay
/// código de archivo, la ruta se añade al final. `None` si la línea está mal formada o vacía.
#[cfg_attr(any(windows, target_arch = "wasm32"), allow(dead_code))]
pub(crate) fn exec_argv(exec: &str, path: &Path, name: &str, icon: &str, desktop: &Path) -> Option<Vec<std::ffi::OsString>> {
    use std::ffi::OsString;
    // 1) escapes generales de los valores de tipo cadena
    let mut line = String::with_capacity(exec.len());
    let mut it = exec.chars();
    while let Some(c) = it.next() {
        if c == '\\' {
            match it.next() {
                Some('s') => line.push(' '),
                Some('n') => line.push('\n'),
                Some('t') => line.push('\t'),
                Some('r') => line.push('\r'),
                Some('\\') => line.push('\\'),
                Some(o) => {
                    line.push('\\');
                    line.push(o);
                }
                None => line.push('\\'),
            }
        } else {
            line.push(c);
        }
    }
    // 2) palabras, comillas y códigos de campo
    let mut out: Vec<OsString> = Vec::new();
    let mut word = OsString::new();
    let mut has_word = false;
    let mut in_q = false;
    let mut used = false;
    let mut it = line.chars();
    while let Some(c) = it.next() {
        if in_q {
            match c {
                '"' => in_q = false,
                '\\' => match it.next() {
                    Some(e @ ('"' | '`' | '$' | '\\')) => word.push(e.to_string()),
                    Some(o) => {
                        word.push("\\");
                        word.push(o.to_string());
                    }
                    None => return None,
                },
                // dentro de comillas los códigos de campo no valen: se quitan (salvo `%%`)
                '%' => {
                    if it.clone().next() == Some('%') {
                        it.next();
                        word.push("%");
                    } else {
                        it.next();
                    }
                }
                o => word.push(o.to_string()),
            }
            continue;
        }
        match c {
            ' ' | '\t' | '\n' | '\r' => {
                if has_word {
                    out.push(std::mem::take(&mut word));
                    has_word = false;
                }
            }
            '"' => {
                in_q = true;
                has_word = true;
            }
            '%' => match it.next() {
                Some('f' | 'F' | 'u' | 'U') => {
                    word.push(path.as_os_str());
                    has_word = true;
                    used = true;
                }
                Some('%') => {
                    word.push("%");
                    has_word = true;
                }
                Some('c') => {
                    word.push(name);
                    has_word = true;
                }
                Some('k') => {
                    word.push(desktop.as_os_str());
                    has_word = true;
                }
                Some('i') => {
                    // sólo como argumento propio y si hay icono
                    if !has_word && !icon.is_empty() {
                        out.push("--icon".into());
                        out.push(icon.into());
                    }
                }
                // obsoletos o desconocidos: se quitan
                _ => {}
            },
            o => {
                word.push(o.to_string());
                has_word = true;
            }
        }
    }
    if in_q {
        return None;
    }
    if has_word {
        out.push(word);
    }
    if out.is_empty() {
        return None;
    }
    if !used {
        out.push(path.as_os_str().to_os_string());
    }
    Some(out)
}

/// Lanza un programa sin heredar la consola y recoge su estado al terminar (sin zombis); devuelve
/// el error de arranque (programa inexistente, sin permiso…).
/// `quiet`: sin entrada ni salida heredadas (no para un shell en consola nueva de Windows).
pub(crate) fn spawn_detached(mut cmd: std::process::Command, quiet: bool) -> Result<(), String> {
    use std::process::Stdio;
    if quiet {
        cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    }
    let mut child = cmd.spawn().map_err(|e| e.to_string())?;
    let _ = crate::vfs::thread::Builder::new().name("reap".into()).spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

/// Lanza `app` (o el comando personalizado `custom`) sobre `path`.
pub fn launch(app: Option<&App>, custom: &str, path: &Path) {
    let _ = launch_app(app, custom, path, "");
}

/// Como `launch`, devolviendo el error de arranque. `sys_term`: terminal del sistema configurado
/// (para las aplicaciones con `Terminal=true`). Nunca pasa por `sh -c`: la orden personalizada se
/// parte en argumentos como un campo Exec (`%f` marca dónde va el archivo; si no, va al final).
pub fn launch_app(app: Option<&App>, custom: &str, path: &Path, sys_term: &str) -> Result<(), String> {
    let _ = sys_term;
    #[cfg(windows)]
    {
        if let Some(a) = app {
            if a.exec.is_empty() && !a.id.is_empty() {
                return crate::sysapps::win_invoke(&a.id, path);
            }
            return launch_registry_cmd(&a.exec, path);
        }
        if custom.trim().is_empty() {
            return crate::sysapps::win_open_with_dialog(path);
        }
        use std::os::windows::process::CommandExt;
        let argv = crate::systerm::split_words(custom.trim()).filter(|v| !v.is_empty()).ok_or(crate::i18n::t("orden vacía"))?;
        let mut c = std::process::Command::new(&argv[0]);
        c.args(&argv[1..]).arg(path).creation_flags(0x0800_0000);
        return spawn_detached(c, true);
    }
    #[cfg(target_os = "macos")]
    {
        if let Some(a) = app {
            let mut c = std::process::Command::new("open");
            c.arg("-a").arg(if a.id.is_empty() { &a.exec } else { &a.id }).arg(path);
            return spawn_detached(c, true);
        }
    }
    #[cfg(not(any(windows, target_arch = "wasm32")))]
    {
        let argv = match app {
            #[cfg(not(target_os = "macos"))]
            Some(a) => exec_argv(&a.exec, path, &a.name, &a.icon, &a.desktop),
            _ => exec_argv(custom.trim(), path, "", "", Path::new("")),
        };
        let argv = argv.ok_or(crate::i18n::t("orden vacía o mal formada"))?;
        let dir = path.parent().filter(|d| d.is_dir()).map(|d| d.to_path_buf()).or_else(dirs::home_dir).unwrap_or_else(|| PathBuf::from("/"));
        if app.is_some_and(|a| a.terminal) {
            return crate::systerm::run_in(sys_term, &dir, &argv);
        }
        let l = crate::systerm::Launch { program: argv[0].clone(), args: argv[1..].to_vec(), cwd: dir, new_console: false };
        return crate::systerm::spawn(&l);
    }
    #[allow(unreachable_code)]
    {
        let _ = (app, custom, path);
        Err(crate::i18n::t("no disponible").into())
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
    #[cfg(target_arch = "wasm32")]
    {
        let _ = p;
        Err(crate::i18n::t("la papelera no está disponible en la web").into())
    }
    #[cfg(not(any(target_os = "android", target_arch = "wasm32")))]
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
            web_time::SystemTime::now().duration_since(web_time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0),
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
        let dir = trash_dir().ok_or(crate::i18n::t("sin carpeta personal"))?;
        let rd = fs::read_dir(&dir).map_err(|e| crate::i18n::tf("{} (¿falta el permiso de acceso a la papelera?)", &[&e]))?;
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
                size: md.as_ref().filter(|m| m.v_is_file()).map(|m| m.len()).unwrap_or(0),
                dir: md.as_ref().map(|m| m.v_is_dir()).unwrap_or(false),
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
                Ok(m) if m.v_is_dir() => fs::remove_dir_all(&r.path),
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
    Err(crate::i18n::t("no soportado").into())
}

#[cfg(any(target_os = "linux", windows))]
pub fn trash_restore(rows: &[TrashRow]) -> Result<(), String> {
    // «gemelos» (misma ruta original): `restore_all` no restaura ninguno si los hay juntos. Primero
    // el más reciente de cada ruta y después los demás, que chocarán y irán con otro nombre.
    let mut sorted: Vec<&TrashRow> = rows.iter().collect();
    sorted.sort_by(|a, b| b.time.cmp(&a.time));
    let mut seen = std::collections::HashSet::new();
    let (mut first, mut later) = (Vec::new(), Vec::new());
    for r in sorted {
        if seen.insert(r.item.original_path()) {
            first.push(r.item.clone());
        } else {
            later.push(r.item.clone());
        }
    }
    let mut errs = Vec::new();
    restore_items(first, &mut errs);
    for it in later {
        restore_items(vec![it], &mut errs);
    }
    if errs.is_empty() { Ok(()) } else { Err(errs.join("\n")) }
}

/// Restaura `items`; uno que choque con algo que ya existe se restaura con un nombre libre
/// («x (2)», como en macOS) y se sigue con los demás.
#[cfg(any(target_os = "linux", windows))]
fn restore_items(mut items: Vec<trash::TrashItem>, errs: &mut Vec<String>) {
    while !items.is_empty() {
        match trash::os_limited::restore_all(items) {
            Ok(()) => return,
            Err(trash::Error::RestoreCollision { path, mut remaining_items }) => {
                // los anteriores al que choca ya están restaurados; `remaining_items` empieza por él
                let k = remaining_items.iter().position(|it| it.original_path() == path).unwrap_or(0);
                if k < remaining_items.len() {
                    let it = remaining_items.remove(k);
                    if let Err(e) = restore_renamed(&it) {
                        errs.push(e);
                    }
                }
                items = remaining_items;
            }
            Err(trash::Error::RestoreTwins { items: twins, .. }) => {
                // no debería pasar (se separan antes): uno a uno
                for it in twins {
                    restore_items(vec![it], errs);
                }
                return;
            }
            Err(e) => {
                errs.push(e.to_string());
                return;
            }
        }
    }
}

/// Restaura un elemento de la papelera junto a su sitio original, con un nombre libre.
#[cfg(target_os = "linux")]
fn restore_renamed(it: &trash::TrashItem) -> Result<(), String> {
    // papelera freedesktop: `id` es el .trashinfo; el contenido está en «files» con el mismo nombre
    let info = Path::new(&it.id);
    let (Some(dir), Some(stem)) = (info.parent().and_then(Path::parent), info.file_stem()) else {
        return Err(crate::i18n::tf("{}: ya existe", &[&it.original_path().display()]));
    };
    let file = dir.join("files").join(stem);
    let _ = fs::create_dir_all(&it.original_parent);
    let dst = crate::fsys::unique_path_os(&it.original_parent, &it.name);
    fs::rename(&file, &dst).map_err(|e| format!("{}: {e}", it.name.to_string_lossy()))?;
    let _ = fs::remove_file(info);
    Ok(())
}

/// Windows: la papelera del sistema sólo restaura a la ruta original; si está ocupada, se avisa.
#[cfg(windows)]
fn restore_renamed(it: &trash::TrashItem) -> Result<(), String> {
    Err(crate::i18n::tf("{}: ya existe", &[&it.original_path().display()]))
}
#[cfg(target_os = "macos")]
pub fn trash_restore(rows: &[TrashRow]) -> Result<(), String> {
    mac_trash::restore(rows)
}
#[cfg(not(any(target_os = "linux", windows, target_os = "macos")))]
pub fn trash_restore(_rows: &[TrashRow]) -> Result<(), String> {
    Err(crate::i18n::t("no soportado").into())
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
    Err(crate::i18n::t("no soportado").into())
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
        return Err(crate::i18n::t("no encontrado en la papelera").into());
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
    crate::vfs::thread::spawn(move || {
        let res = (|| -> Result<[String; 3], String> {
            crate::sftp::ensure_local_with(&path, &j.cancel, &mut |_| {})?;
            let mut f = File::open(&path).map_err(|e| e.to_string())?;
            j.total.store(f.metadata().map(|m| m.len()).unwrap_or(0).max(1), Ordering::Relaxed);
            let (mut m, mut s1, mut s2) = (md5::Md5::new(), sha1::Sha1::new(), sha2::Sha256::new());
            let mut buf = vec![0u8; 1 << 20];
            loop {
                if j.cancel.load(Ordering::Relaxed) {
                    return Err(crate::i18n::t("cancelado").into());
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

/// Identidad de un archivo en disco (dispositivo, nodo): dos enlaces duros al mismo archivo no
/// son duplicados (borrar uno no libera nada).
type FileId = (u64, u64);

fn walk(dir: &Path, out: &mut HashMap<u64, Vec<PathBuf>>, seen: &mut std::collections::HashSet<FileId>, j: &DupJob, ctx: &egui::Context) {
    let Ok(rd) = fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        if j.cancel.load(Ordering::Relaxed) {
            return;
        }
        let Ok(ft) = e.file_type() else { continue };
        let p = e.path();
        if ft.v_is_dir() {
            walk(&p, out, seen, j, ctx);
        } else if ft.v_is_file() {
            if let Ok(md) = e.metadata() {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::MetadataExt;
                    if md.nlink() > 1 && !seen.insert((md.dev(), md.ino())) {
                        continue;
                    }
                }
                // en Windows la identidad pide abrir el archivo: se mira después, sólo entre los del mismo tamaño
                #[cfg(not(unix))]
                let _ = &seen;
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

/// Quita de `files` los enlaces duros a un archivo ya visto (`id_of` da la identidad sólo si el
/// archivo tiene más de un enlace).
#[cfg_attr(not(windows), allow(dead_code))]
fn drop_hard_links(files: Vec<PathBuf>, seen: &mut std::collections::HashSet<FileId>, id_of: impl Fn(&Path) -> Option<FileId>) -> Vec<PathBuf> {
    files.into_iter().filter(|p| id_of(p).is_none_or(|id| seen.insert(id))).collect()
}

/// Windows: (número de serie del volumen, índice del archivo) si el archivo tiene más de un enlace duro.
#[cfg(windows)]
fn win_link_id(p: &Path) -> Option<FileId> {
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::Storage::FileSystem::{BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle};
    // sin pedir lectura (sólo atributos) y compartido: sirve aunque otro lo tenga abierto
    let f = std::fs::OpenOptions::new().access_mode(0).share_mode(7).open(p).ok()?;
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    unsafe { GetFileInformationByHandle(HANDLE(f.as_raw_handle()), &mut info) }.ok()?;
    (info.nNumberOfLinks > 1).then(|| (info.dwVolumeSerialNumber as u64, ((info.nFileIndexHigh as u64) << 32) | info.nFileIndexLow as u64))
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
    crate::vfs::thread::spawn(move || {
        let mut by_size: HashMap<u64, Vec<PathBuf>> = HashMap::new();
        walk(&root, &mut by_size, &mut Default::default(), &j, &ctx);
        #[cfg(windows)]
        let mut seen_win = std::collections::HashSet::new();
        for (size, files) in by_size.into_iter().filter(|(_, v)| v.len() > 1) {
            if j.cancel.load(Ordering::Relaxed) {
                break;
            }
            #[cfg(windows)]
            let files = drop_hard_links(files, &mut seen_win, win_link_id);
            if files.len() < 2 {
                continue;
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
        j.groups.lock().unwrap().sort_by_key(|g| std::cmp::Reverse(g.0.saturating_mul(g.1.len() as u64)));
        j.finished.store(true, Ordering::Release);
        ctx.request_repaint();
    });
    job
}

#[cfg(test)]
mod exec_dup_tests {
    use super::*;

    fn argv(exec: &str, path: &str) -> Vec<String> {
        exec_argv(exec, Path::new(path), "Mi App", "mi-icono", Path::new("/usr/share/applications/mi.desktop"))
            .unwrap()
            .into_iter()
            .map(|a| a.into_string().unwrap())
            .collect()
    }

    #[test]
    fn exec_field_codes() {
        assert_eq!(argv("gedit %U", "/t/a b.txt"), ["gedit", "/t/a b.txt"]);
        assert_eq!(argv("vlc --started-from-file %f", "/x"), ["vlc", "--started-from-file", "/x"]);
        assert_eq!(argv("app", "/x"), ["app", "/x"], "sin código: la ruta al final");
        assert_eq!(argv("app --file=%f", "/x"), ["app", "--file=/x"]);
        assert_eq!(argv("app %i %c %k %f", "/x"), ["app", "--icon", "mi-icono", "Mi App", "/usr/share/applications/mi.desktop", "/x"]);
        assert_eq!(argv("app 100%% %d %D %n %N %v %m %F", "/x"), ["app", "100%", "/x"]);
        assert_eq!(argv("  app\t\t%f  ", "/x"), ["app", "/x"]);
    }

    #[test]
    fn exec_quoting_and_escapes() {
        // comillas y comillas escapadas (tal y como quedan tras leer el .desktop: `\\"` → `\"`)
        assert_eq!(argv(r#""/opt/My App/bin" --title "say \\"hi\\"" %f"#, "/x"), ["/opt/My App/bin", "--title", "say \"hi\"", "/x"]);
        assert_eq!(argv(r#"sh -c "echo \\$HOME \\`id\\` \\\\" %f"#, "/x"), ["sh", "-c", "echo $HOME `id` \\", "/x"]);
        // escapes generales: se aplican antes de partir en palabras (`\s` fuera de comillas separa)
        assert_eq!(argv(r"app\sname %f", "/x"), ["app", "name", "/x"]);
        assert_eq!(argv(r#""my\sapp" %f"#, "/x"), ["my app", "/x"]);
        // argumento vacío
        assert_eq!(argv(r#"app "" %f"#, "/x"), ["app", "", "/x"]);
        // dentro de comillas los códigos de campo se quitan (la ruta no se cuela en un argumento de shell)
        assert_eq!(argv(r#"sh -c "open %f" %f"#, "/x"), ["sh", "-c", "open ", "/x"]);
        // mal formado
        assert!(exec_argv(r#"app "abierta"#, Path::new("/x"), "", "", Path::new("")).is_none());
        assert!(exec_argv("   ", Path::new("/x"), "", "", Path::new("")).is_none());
    }

    #[test]
    fn exec_hostile_names_are_single_args() {
        for name in ["/t/$(rm -rf ~).txt", "/t/a\"b'c`d`.txt", "/t/; reboot", "/t/x %f y", "/t/-rf"] {
            let v = argv("app %f", name);
            assert_eq!(v, ["app", name], "{name}");
        }
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt;
            let raw = std::ffi::OsStr::from_bytes(b"/t/caf\xe9");
            let v = exec_argv("app %f", Path::new(raw), "", "", Path::new("")).unwrap();
            assert_eq!(v, [std::ffi::OsString::from("app"), raw.to_os_string()]);
        }
    }

    /// La parte común con Windows: la segunda ruta con la misma identidad sobra; sin identidad
    /// (un solo enlace) nunca se quita nada.
    #[test]
    fn drop_hard_links_by_id() {
        let ids = |p: &Path| match p.file_name().and_then(|n| n.to_str()) {
            Some("a" | "a_link" | "a_link2") => Some((7, 1)),
            Some("b") => Some((8, 1)),
            Some("b_otro_volumen") => Some((9, 1)),
            _ => None,
        };
        let mut seen = std::collections::HashSet::new();
        let v: Vec<PathBuf> = ["a", "x", "a_link", "y", "a_link2"].iter().map(PathBuf::from).collect();
        assert_eq!(drop_hard_links(v, &mut seen, ids), ["a", "x", "y"].iter().map(PathBuf::from).collect::<Vec<_>>());
        // otro grupo de tamaño: comparte el conjunto, pero el volumen distingue
        let v: Vec<PathBuf> = ["b", "b_otro_volumen"].iter().map(PathBuf::from).collect();
        assert_eq!(drop_hard_links(v, &mut seen, ids).len(), 2);
    }

    #[cfg(unix)]
    #[test]
    fn duplicates_skip_hard_links() {
        let n = web_time::SystemTime::now().duration_since(web_time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
        let d = crate::vfs::temp_dir().join(format!("fbw1-dups-{}-{n:x}", crate::vfs::pid()));
        fs::create_dir_all(d.join("sub")).unwrap();
        fs::write(d.join("a.bin"), b"mismo contenido").unwrap();
        fs::hard_link(d.join("a.bin"), d.join("sub/a_link.bin")).unwrap();
        fs::write(d.join("c.bin"), b"otro contenido!").unwrap();
        fs::hard_link(d.join("c.bin"), d.join("c_link.bin")).unwrap();
        fs::write(d.join("c_copy.bin"), b"otro contenido!").unwrap();
        let j = spawn_dups(d.clone(), egui::Context::default());
        let t0 = web_time::Instant::now();
        while !j.finished.load(Ordering::Acquire) {
            assert!(t0.elapsed().as_secs() < 30);
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let g = j.groups.lock().unwrap().clone();
        let _ = fs::remove_dir_all(&d);
        // a.bin y su enlace duro no son duplicados; c.bin (o su enlace) y c_copy.bin sí
        assert_eq!(g.len(), 1, "{g:?}");
        assert_eq!(g[0].1.len(), 2, "{g:?}");
        assert!(g[0].1.iter().any(|p| p.ends_with("c_copy.bin")));
    }
}
