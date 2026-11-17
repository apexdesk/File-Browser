//! Barra lateral y ubicaciones (favoritos, unidades, red, montajes gvfs), vigilancia de la carpeta y barras de estado.
use super::*;

#[derive(Clone, Copy, PartialEq)]
pub(super) enum SideKind {
    Fav,
    Drive,
}

/// Lo que se ha elegido en el menú contextual de la barra lateral (se aplica al acabar de dibujarla).
pub(super) enum SideAct {
    NewTab(PathBuf),
    NewWin(PathBuf),
    CopyPath(PathBuf),
    Props(PathBuf),
    #[cfg_attr(not(windows), allow(dead_code))]
    Native(PathBuf),
    ToggleFav(PathBuf),
    RestoreFavs,
    Move(usize, isize),
    Rename(PathBuf),
    Icon(PathBuf),
    Go(PathBuf),
    #[cfg_attr(not(any(target_os = "linux", target_os = "macos", windows)), allow(dead_code))]
    Eject(PathBuf),
}

/// Qué hacer con archivos de un servidor SFTP cuando acaba su descarga (`Op::Fetch`).
#[derive(PartialEq)]
pub(super) enum FetchThen {
    Open(PathBuf),
    Clip(Vec<PathBuf>, bool),
    Drag,
}

/// ¿Es una dirección de red (sftp://, smb://…) y no una ruta? (escrita en la barra o en un favorito)
pub(super) fn is_net_uri(s: &str) -> bool {
    s.split_once("://").is_some_and(|(sch, _)| {
        matches!(sch.to_ascii_lowercase().as_str(), "smb" | "cifs" | "sftp" | "ssh" | "ftp" | "ftps" | "dav" | "davs" | "webdav" | "webdavs" | "nfs" | "afp")
    })
}

/// Disco extraíble que se puede expulsar desde el menú (Linux: /media, /run/media; macOS: /Volumes;
/// Windows: unidades extraíbles, CD/DVD, discos USB y unidades de red asignadas).
fn ejectable(p: &Path) -> bool {
    #[cfg(target_os = "linux")]
    return (p.starts_with("/media") || p.starts_with("/run/media")) && p.components().count() > 2;
    #[cfg(target_os = "macos")]
    return p.starts_with("/Volumes") && p.components().count() > 2;
    #[cfg(windows)]
    return tools::win_eject::ejectable(p);
    #[allow(unreachable_code)]
    {
        let _ = p;
        false
    }
}

/// Menú contextual común de los accesos de la barra lateral (favoritos y discos).
pub(super) fn side_menu(ui: &mut egui::Ui, path: &Path, kind: SideKind, idx: usize, len: usize, fav: bool, can_restore: bool, act: &mut Option<SideAct>) {
    ui.set_min_width(200.0);
    let pick = |ui: &mut egui::Ui, label: &str, enabled: bool| -> bool {
        let c = ui.add_enabled(enabled, egui::Button::new(label)).clicked();
        if c {
            ui.close();
        }
        c
    };
    if pick(ui, t("Abrir"), true) {
        *act = Some(SideAct::Go(path.to_path_buf()));
    }
    if pick(ui, t("Abrir en una pestaña nueva"), true) {
        *act = Some(SideAct::NewTab(path.to_path_buf()));
    }
    if !crate::vfs::WEB && pick(ui, t("Abrir en una ventana nueva"), true) {
        *act = Some(SideAct::NewWin(path.to_path_buf()));
    }
    ui.separator();
    if pick(ui, if fav { t("Quitar de favoritos") } else { t("Añadir a favoritos") }, true) {
        *act = Some(SideAct::ToggleFav(path.to_path_buf()));
    }
    if kind == SideKind::Fav {
        if pick(ui, t("Cambiar nombre…"), true) {
            *act = Some(SideAct::Rename(path.to_path_buf()));
        }
        if pick(ui, t("Cambiar icono…"), true) {
            *act = Some(SideAct::Icon(path.to_path_buf()));
        }
        if pick(ui, t("Subir"), idx > 0) {
            *act = Some(SideAct::Move(idx, -1));
        }
        if pick(ui, t("Bajar"), idx + 1 < len) {
            *act = Some(SideAct::Move(idx, 1));
        }
    }
    if kind == SideKind::Fav && can_restore && pick(ui, t("Restaurar favoritos"), true) {
        *act = Some(SideAct::RestoreFavs);
    }
    ui.separator();
    if pick(ui, t("Copiar ruta"), true) {
        *act = Some(SideAct::CopyPath(path.to_path_buf()));
    }
    if pick(ui, t("Propiedades"), true) {
        *act = Some(SideAct::Props(path.to_path_buf()));
    }
    if kind == SideKind::Drive && ejectable(path) {
        ui.separator();
        if pick(ui, t("Expulsar"), true) {
            *act = Some(SideAct::Eject(path.to_path_buf()));
        }
    }
    #[cfg(windows)]
    if pick(ui, t("Más opciones de Windows…"), true) {
        *act = Some(SideAct::Native(path.to_path_buf()));
    }
}

pub(super) fn standard_places() -> Vec<Place> {
    let mut v = Vec::new();
    let mut add = |name: &str, p: Option<PathBuf>, g: Glyph, sys: &'static str| {
        if let Some(p) = p {
            if p.v_is_dir() {
                v.push(Place { name: name.to_string(), path: p, glyph: g, sys });
            }
        }
    };
    add(t("Inicio"), dirs::home_dir(), Glyph::Home, "home");
    // en la web, las carpetas que comparte el servidor
    if crate::vfs::WEB && let Ok(rd) = crate::vfs::fs::read_dir("/") {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if e.file_type().is_ok_and(|t| t.is_dir()) {
                add(&name, Some(e.path()), Glyph::Folder, "");
            }
        }
    }
    #[cfg(target_os = "android")]
    {
        // Android no define los directorios XDG: usamos los habituales del almacenamiento
        let h = PathBuf::from("/storage/emulated/0");
        add(t("Descargas"), Some(h.join("Download")), Glyph::Download, "downloads");
        add(t("Imágenes"), Some(h.join("Pictures")), Glyph::Picture, "pictures");
        add("DCIM", Some(h.join("DCIM")), Glyph::Picture, "pictures");
        add(t("Documentos"), Some(h.join("Documents")), Glyph::Document, "documents");
        add(t("Música"), Some(h.join("Music")), Glyph::Music, "music");
        add(t("Vídeos"), Some(h.join("Movies")), Glyph::Video, "videos");
    }
    add(t("Escritorio"), dirs::desktop_dir(), Glyph::Desktop, "desktop");
    add(t("Documentos"), dirs::document_dir(), Glyph::Document, "documents");
    add(t("Descargas"), dirs::download_dir(), Glyph::Download, "downloads");
    add(t("Imágenes"), dirs::picture_dir(), Glyph::Picture, "pictures");
    add(t("Música"), dirs::audio_dir(), Glyph::Music, "music");
    add(t("Vídeos"), dirs::video_dir(), Glyph::Video, "videos");
    v
}

/// Sitio de red conocido: ya montado (`path`) o solo conocido por su dirección (`uri`).
pub(super) struct NetPlace {
    pub(super) name: String,
    pub(super) path: Option<PathBuf>,
    pub(super) uri: Option<String>,
}

#[cfg(target_os = "linux")]
pub(super) fn gvfs_dir() -> Option<PathBuf> {
    // sin tocar el disco: un gvfs colgado bloquearía cada fotograma; quien lo use ya falla si no existe
    std::env::var_os("XDG_RUNTIME_DIR").map(|d| PathBuf::from(d).join("gvfs"))
}

/// Nombre legible de un montaje gvfs («smb-share:server=nas,share=media» -> «media en nas»).
#[cfg(target_os = "linux")]
pub(super) fn gvfs_label(dir_name: &str) -> String {
    let (scheme, rest) = dir_name.split_once(':').unwrap_or((dir_name, ""));
    let get = |k: &str| rest.split(',').find_map(|kv| kv.strip_prefix(&format!("{k}=")).map(|v| v.to_string()));
    match scheme {
        "smb-share" => match (get("share"), get("server")) {
            (Some(sh), Some(sv)) => tf("{} en {}", &[&sh, &sv]),
            _ => dir_name.to_string(),
        },
        "sftp" | "ftp" | "dav" | "davs" | "nfs" | "mtp" | "gphoto2" => {
            format!("{} ({})", get("host").unwrap_or_else(|| rest.to_string()).replace("%20", " "), scheme.to_uppercase())
        }
        _ => dir_name.to_string(),
    }
}

#[cfg(target_os = "linux")]
fn pct_decode(s: &str) -> String {
    let b = s.as_bytes();
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
    String::from_utf8_lossy(&out).into_owned()
}

/// Dirección (smb://servidor/recurso) -> directorio gvfs correspondiente, si está montado.
/// Tolera alias de esquema (ssh/sftp, davs/dav, ftps/ftp), mayúsculas y %20. El usuario y el puerto
/// tienen que coincidir si los dos lados los indican; con subcarpeta (sftp, ftp, smb) se entra en
/// ella si existe.
#[cfg(target_os = "linux")]
fn uri_to_gvfs(uri: &str) -> Option<PathBuf> {
    let base = gvfs_dir()?;
    let (scheme, rest) = uri.split_once("://")?;
    let scheme = scheme.to_lowercase();
    let want = match scheme.as_str() {
        "smb" => "smb-share",
        "ssh" | "sftp" => "sftp",
        "dav" | "davs" => "dav",
        "ftp" | "ftps" => "ftp",
        other => other,
    }
    .to_string();
    let auth = rest.split('/').next()?;
    let (user, hp) = match auth.rsplit_once('@') {
        Some((u, h)) => (Some(pct_decode(u.split(':').next().unwrap_or(u)).to_lowercase()), h),
        None => (None, auth),
    };
    let (host, port) = crate::netmount::split_host_port(hp);
    let host = pct_decode(host).to_lowercase();
    let share = pct_decode(rest.split('/').nth(1).unwrap_or("")).to_lowercase();
    // puerto por omisión de cada protocolo: gvfs no lo pone en el nombre
    let def_port = match scheme.as_str() {
        "ssh" | "sftp" => Some(22),
        "ftp" => Some(21),
        "dav" => Some(80),
        "davs" => Some(443),
        _ => None,
    };
    let port = port.filter(|p| Some(*p) != def_port);
    // subcarpeta pedida (sin el recurso en smb)
    let skip = if want == "smb-share" { 2 } else { 1 };
    let sub: PathBuf = if matches!(want.as_str(), "sftp" | "ftp" | "smb-share") {
        rest.split('/').skip(skip).filter(|s| !s.is_empty() && *s != "." && *s != "..").map(pct_decode).collect()
    } else {
        PathBuf::new()
    };
    for e in crate::vfs::fs::read_dir(base).ok()?.flatten() {
        let n = e.file_name().to_string_lossy().into_owned();
        let Some((sch, props)) = n.split_once(':') else { continue };
        if sch.to_lowercase() != want {
            continue;
        }
        let get = |k: &str| props.split(',').find_map(|kv| kv.strip_prefix(&format!("{k}=")).map(|v| pct_decode(v).to_lowercase()));
        let hk = if want == "smb-share" { "server" } else { "host" };
        let gh = get(hk).map(|h| h.trim_start_matches('[').trim_end_matches(']').to_string());
        if gh.as_deref() != Some(host.trim_start_matches('[').trim_end_matches(']')) {
            continue;
        }
        if want == "smb-share" && !share.is_empty() && get("share").as_deref() != Some(share.as_str()) {
            continue;
        }
        if let (Some(u), Some(gu)) = (&user, get("user")) {
            if !u.is_empty() && *u != gu {
                continue;
            }
        }
        if port.map(|p| p.to_string()) != get("port").filter(|p| def_port.map(|d| d.to_string()) != Some(p.clone())) {
            continue;
        }
        let root = e.path();
        let deep = root.join(&sub);
        return Some(if !sub.as_os_str().is_empty() && deep.v_is_dir() { deep } else { root });
    }
    None
}

/// Ruta local de una dirección ya montada según gvfs (`gio info`), aunque no aparezca por nombre en
/// el directorio gvfs. No monta nada; con límite de tiempo por si el servidor no responde.
#[cfg(target_os = "linux")]
fn gio_local_path(uri: &str) -> Option<PathBuf> {
    let o = std::process::Command::new("timeout")
        .args(["4", "gio", "info", "-a", "standard::name", uri])
        .env("LC_ALL", "C")
        .stdin(std::process::Stdio::null())
        .output()
        .ok()?;
    if !o.status.success() {
        return None;
    }
    String::from_utf8_lossy(&o.stdout)
        .lines()
        .find_map(|l| l.trim().strip_prefix("local path:").map(|p| PathBuf::from(p.trim())))
        .filter(|p| p.v_exists())
}

/// Texto en minúsculas de `gio mount -l` (montajes conocidos por gvfs), para saber si merece consultar.
#[cfg(target_os = "linux")]
fn gio_mounts_text() -> String {
    std::process::Command::new("timeout")
        .args(["4", "gio", "mount", "-l"])
        .env("LC_ALL", "C")
        .stdin(std::process::Stdio::null())
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).to_lowercase())
        .unwrap_or_default()
}

/// Tema del sistema en Linux (winit no lo informa en Wayland): portal XDG, gsettings, KDE y GTK.
#[cfg(target_os = "linux")]
pub(super) fn detect_system_dark() -> Option<bool> {
    use std::process::Command;
    let run = |prog: &str, args: &[&str]| -> Option<String> {
        let o = Command::new(prog).args(args).output().ok()?;
        o.status.success().then(|| String::from_utf8_lossy(&o.stdout).trim().to_string())
    };
    // portal: (<<uint32 1>>,) 1 = oscuro, 2 = claro, 0 = sin preferencia
    if let Some(o) = run(
        "gdbus",
        &["call", "--session", "--dest", "org.freedesktop.portal.Desktop", "--object-path", "/org/freedesktop/portal/desktop",
          "--method", "org.freedesktop.portal.Settings.Read", "org.freedesktop.appearance", "color-scheme"],
    ) {
        if let Some(n) = o.rsplit("uint32").next().and_then(|r| r.trim().chars().find(|c| c.is_ascii_digit())) {
            match n {
                '1' => return Some(true),
                '2' => return Some(false),
                _ => {}
            }
        }
    }
    if let Some(o) = run("gsettings", &["get", "org.gnome.desktop.interface", "color-scheme"]) {
        if o.contains("prefer-dark") {
            return Some(true);
        }
        if o.contains("prefer-light") {
            return Some(false);
        }
    }
    let home = dirs::home_dir()?;
    let cfg = dirs::config_dir().unwrap_or_else(|| home.join(".config"));
    if let Ok(txt) = crate::vfs::fs::read_to_string(cfg.join("kdeglobals")) {
        if let Some(l) = txt.lines().find(|l| l.starts_with("ColorScheme=")) {
            return Some(l.to_lowercase().contains("dark"));
        }
    }
    if let Ok(txt) = crate::vfs::fs::read_to_string(cfg.join("gtk-3.0/settings.ini")) {
        for l in txt.lines() {
            let l = l.trim();
            if let Some(v) = l.strip_prefix("gtk-application-prefer-dark-theme=") {
                if v.trim() == "1" || v.trim() == "true" {
                    return Some(true);
                }
            }
            if let Some(v) = l.strip_prefix("gtk-theme-name=") {
                return Some(v.to_lowercase().contains("dark"));
            }
        }
    }
    if let Ok(v) = std::env::var("GTK_THEME") {
        return Some(v.to_lowercase().contains("dark"));
    }
    None
}

#[cfg(target_os = "linux")]
struct GioCred {
    user: String,
    domain: String,
    pass: String,
    anon: bool,
}

/// Error de `run_gio`: no hay `gio` en el sistema.
#[cfg(target_os = "linux")]
const GIO_MISSING: &str = "\u{1}gio-missing";
/// Error de `run_gio`: gio hace una pregunta con opciones (p. ej. la clave de un servidor SSH nuevo).
#[cfg(target_os = "linux")]
const GIO_ASK: &str = "\u{1}gio-ask";

/// Ejecuta `gio <args> <uri>` respondiendo a sus preguntas de usuario, dominio y contraseña.
#[cfg(target_os = "linux")]
fn run_gio(args: &[&str], uri: &str, c: &GioCred) -> Result<(), String> {
    use std::io::{Read, Write};
    use std::process::{Command, Stdio};
    let mut cmd = Command::new("gio");
    cmd.args(args);
    // preguntas en inglés: se reconocen por texto y, en otro idioma, gio se quedaba esperando
    cmd.env("LC_ALL", "C").env("LANGUAGE", "C");
    if c.anon {
        cmd.arg("-a");
    }
    cmd.arg("--").arg(uri).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(GIO_MISSING.into()),
        Err(e) => return Err(format!("gio: {e}")),
    };
    let (tx, rx) = std::sync::mpsc::channel::<Vec<u8>>();
    let outs: [Option<Box<dyn Read + Send>>; 2] = [
        child.stdout.take().map(|o| Box::new(o) as Box<dyn Read + Send>),
        child.stderr.take().map(|o| Box::new(o) as Box<dyn Read + Send>),
    ];
    for mut src in outs.into_iter().flatten() {
        let tx = tx.clone();
        crate::vfs::thread::spawn(move || {
            let mut b = [0u8; 512];
            while let Ok(n) = src.read(&mut b) {
                if n == 0 || tx.send(b[..n].to_vec()).is_err() {
                    break;
                }
            }
        });
    }
    drop(tx);
    let mut stdin = child.stdin.take();
    let (mut buf, mut log) = (String::new(), String::new());
    let t0 = Instant::now();
    let mut sent_pass = false;
    loop {
        match rx.recv_timeout(Duration::from_millis(250)) {
            Ok(chunk) => {
                let txt = String::from_utf8_lossy(&chunk).into_owned();
                buf.push_str(&txt);
                log.push_str(&txt);
                let last = buf.rsplit('\n').next().unwrap_or("").trim_end().to_lowercase();
                // pregunta con opciones («Choice:»): la de la clave de un servidor SSH nuevo; no se
                // contesta a ciegas (gio se quedaba esperando hasta agotar el tiempo)
                if last.starts_with("choice") {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(GIO_ASK.into());
                }
                if last.ends_with(':') || last.ends_with("]:") {
                    let ans = if last.contains("anonymous") || last.contains("anónim") {
                        Some("n")
                    } else if last.contains("password") || last.contains("passphrase") || last.contains("contraseña") {
                        // una contraseña rechazada no se reenvía: podría bloquear la cuenta
                        if sent_pass {
                            let _ = child.kill();
                            let _ = child.wait();
                            return Err(t("Contraseña incorrecta").to_string());
                        }
                        sent_pass = true;
                        Some(c.pass.as_str())
                    } else if last.contains("domain") || last.contains("dominio") {
                        Some(c.domain.as_str())
                    } else if last.contains("user") || last.contains("usuario") {
                        Some(c.user.as_str())
                    } else {
                        None
                    };
                    if let (Some(a), Some(w)) = (ans, stdin.as_mut()) {
                        let _ = writeln!(w, "{a}");
                        let _ = w.flush();
                        buf.push('\n');
                    }
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                if t0.elapsed() > Duration::from_secs(90) {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(t("Tiempo de espera agotado").to_string());
                }
            }
            Err(_) => break,
        }
    }
    drop(stdin);
    match child.wait() {
        Ok(st) if st.success() => Ok(()),
        _ => Err(log.lines().rev().map(|l| l.trim()).find(|l| !l.is_empty() && !l.to_lowercase().ends_with(':')).unwrap_or("gio").to_string()),
    }
}

fn list_network() -> Vec<NetPlace> {
    #[allow(unused_mut)]
    let mut v: Vec<NetPlace> = Vec::new();
    #[cfg(target_os = "linux")]
    {
        // montajes de red del sistema (cifs, nfs, sshfs…)
        if let Ok(txt) = crate::vfs::fs::read_to_string("/proc/self/mounts") {
            for l in txt.lines() {
                let mut it = l.split_whitespace();
                let (Some(dev), Some(mp), Some(fs)) = (it.next(), it.next(), it.next()) else { continue };
                if matches!(fs, "cifs" | "smb3" | "smbfs" | "nfs" | "nfs4" | "fuse.sshfs" | "davfs" | "fuse.davfs" | "ceph" | "afs") {
                    let mp = mp.replace("\\040", " ");
                    v.push(NetPlace { name: dev.trim_start_matches('/').to_string(), path: Some(PathBuf::from(mp)), uri: None });
                }
            }
        }
        // montajes gvfs (smb, sftp, ftp, dav, mtp…)
        let mut mounted: Vec<PathBuf> = Vec::new();
        let gio_text = gio_mounts_text();
        if let Some(g) = gvfs_dir() {
            if let Ok(rd) = crate::vfs::fs::read_dir(&g) {
                for e in rd.flatten() {
                    let n = e.file_name().to_string_lossy().into_owned();
                    mounted.push(e.path());
                    v.push(NetPlace { name: gvfs_label(&n), path: Some(e.path()), uri: None });
                }
            }
        }
        // cada servidor SMB con recursos montados sigue disponible para conectar otros
        let mut servers: Vec<String> = Vec::new();
        for p in &mounted {
            let n = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            if let Some(props) = n.strip_prefix("smb-share:") {
                if let Some(h) = props.split(',').find_map(|kv| kv.strip_prefix("server=")) {
                    if !servers.iter().any(|x| x.eq_ignore_ascii_case(h)) {
                        servers.push(h.to_string());
                    }
                }
            }
        }
        for h in &servers {
            v.push(NetPlace { name: pct_decode(h).to_uppercase(), path: None, uri: Some(format!("smb://{h}/")) });
        }
        // sitios conocidos: marcadores GTK con direcciones de red
        if let Some(h) = dirs::home_dir() {
            if let Ok(txt) = crate::vfs::fs::read_to_string(h.join(".config/gtk-3.0/bookmarks")) {
                for l in txt.lines() {
                    let (uri, label) = l.split_once(' ').map(|(a, b)| (a, Some(b))).unwrap_or((l, None));
                    let Some((scheme, _)) = uri.split_once("://") else { continue };
                    if !matches!(scheme, "smb" | "sftp" | "ftp" | "ftps" | "dav" | "davs" | "nfs" | "ssh") {
                        continue;
                    }
                    let host = uri.split_once("://").and_then(|(_, r)| r.split('/').next()).and_then(|a| a.rsplit('@').next()).map(|h| pct_decode(h.split(':').next().unwrap_or(h)).to_lowercase()).unwrap_or_default();
                    let server_only = scheme == "smb" && uri.split_once("://").map(|(_, r)| r.trim_end_matches('/').split('/').nth(1).is_none()).unwrap_or(false);
                    // un servidor sin recurso sigue ofreciéndose aunque tenga recursos montados: sirve para añadir más
                    let path = if server_only { None } else { uri_to_gvfs(uri).or_else(|| if !host.is_empty() && gio_text.contains(&host) { gio_local_path(uri) } else { None }) };
                    if path.as_ref().map(|p| mounted.contains(p)).unwrap_or(false) {
                        continue; // ya listado como montado
                    }
                    if server_only && servers.iter().any(|x| x.eq_ignore_ascii_case(&host)) {
                        v.retain(|p| p.path.is_some() || p.uri.as_deref().map(|u| !u.to_lowercase().contains(&format!("//{host}/"))).unwrap_or(true));
                    }
                    let name = label.map(|s| s.to_string()).unwrap_or_else(|| uri.to_string());
                    v.push(NetPlace { name, path, uri: Some(uri.to_string()) });
                }
            }
        }
    }
    #[cfg(windows)]
    {
        if let Some(a) = dirs::config_dir() {
            // «Entorno de red»: accesos a recursos compartidos que el usuario ha abierto
            let d = a.join("Microsoft").join("Windows").join("Network Shortcuts");
            if let Ok(rd) = crate::vfs::fs::read_dir(d) {
                for e in rd.flatten() {
                    v.push(NetPlace { name: e.file_name().to_string_lossy().into_owned(), path: Some(e.path()), uri: None });
                }
            }
        }
    }
    v
}

pub(super) fn list_drives() -> Vec<Place> {
    #[allow(unused_mut)]
    let mut v = Vec::new();
    #[cfg(windows)]
    for c in b'A'..=b'Z' {
        let p = PathBuf::from(format!("{}:\\", c as char));
        if p.v_exists() {
            let name = match crate::winshell::volume_label(&p) {
                Some(l) => format!("{l} ({}:)", c as char),
                None => tf("Disco ({}:)", &[&(c as char)]),
            };
            v.push(Place { name, path: p, glyph: Glyph::Drive, sys: "drive" });
        }
    }
    #[cfg(target_os = "linux")]
    {
        v.push(Place { name: t("Sistema").into(), path: PathBuf::from("/"), glyph: Glyph::Drive, sys: "drive" });
        if let Ok(txt) = crate::vfs::fs::read_to_string("/proc/self/mounts") {
            for l in txt.lines() {
                let mut it = l.split_whitespace();
                let (Some(dev), Some(mp)) = (it.next(), it.next()) else { continue };
                let mp = mp.replace("\\040", " ");
                if dev.starts_with("/dev/")
                    && (mp.starts_with("/media") || mp.starts_with("/run/media") || mp.starts_with("/mnt"))
                {
                    let name = Path::new(&mp).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or(mp.clone());
                    v.push(Place { name, path: PathBuf::from(mp), glyph: Glyph::Drive, sys: "drive" });
                }
            }
        }
    }
    #[cfg(target_os = "macos")]
    {
        v.push(Place { name: "Macintosh HD".into(), path: PathBuf::from("/"), glyph: Glyph::Drive, sys: "drive" });
        if let Ok(rd) = crate::vfs::fs::read_dir("/Volumes") {
            for e in rd.flatten() {
                let p = e.path();
                if p != Path::new("/Volumes/Macintosh HD") {
                    v.push(Place { name: e.file_name().to_string_lossy().into_owned(), path: p, glyph: Glyph::Drive, sys: "drive" });
                }
            }
        }
    }
    v
}

/// Fecha de una carpeta vigilada; `UNIX_EPOCH` si ya no existe (otro error: `None`, sin cambios).
fn watch_stamp(p: &Path) -> Option<SystemTime> {
    match crate::vfs::fs::metadata(p) {
        Ok(m) => m.modified().ok(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Some(SystemTime::UNIX_EPOCH),
        Err(_) => None,
    }
}

/// Número del último intento de conexión (ver `start_mount`).
static NET_GEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

impl App {
    /// Hace `work` (E/S que puede tardar, p. ej. en una carpeta de red) en un hilo y, al terminar,
    /// ejecuta `done` con su resultado en el hilo de la interfaz.
    pub(super) fn spawn_io<T: Send + 'static>(&self, work: impl FnOnce() -> T + Send + 'static, done: impl FnOnce(&mut App, T) + Send + 'static) {
        let (q, ctx) = (self.io_done.clone(), self.ctx.clone());
        crate::vfs::thread::spawn(move || {
            let r = work();
            crate::thumbs::lock(&q).push(Box::new(move |a: &mut App| done(a, r)));
            ctx.request_repaint();
        });
    }

    /// Abre un archivo con su aplicación. Si es de un servidor SFTP, primero se descarga como una
    /// tarea más (con avance, cancelar y errores) y se abre una copia de sólo lectura.
    pub(super) fn open_ext(&mut self, p: &Path) {
        if !crate::sftp::is_remote(p) || p.v_is_dir() {
            fsys::open_external(p);
            return;
        }
        if !crate::sftp::is_live(p) {
            self.say(t("Copia local de un servidor SFTP que ya no está conectado").into(), true);
            return;
        }
        if crate::sftp::tree_fetched(p) {
            self.open_remote_copy(p);
            return;
        }
        let p = p.to_path_buf();
        self.fetch_first(std::slice::from_ref(&p), FetchThen::Open(p.clone()));
    }

    /// Abre la copia local (ya descargada) de un archivo de un servidor SFTP, protegida contra escritura.
    fn open_remote_copy(&mut self, p: &Path) {
        static NOTED: AtomicBool = AtomicBool::new(false);
        crate::sftp::set_read_only(p);
        if !NOTED.swap(true, Ordering::Relaxed) {
            self.say(t("Se abre una copia de sólo lectura: los cambios no se guardan en el servidor").into(), false);
        }
        fsys::open_external(p);
    }

    /// Si hay archivos de un servidor SFTP sin descargar en `v`, empieza su descarga (una sola vez
    /// para lo mismo) y al acabar hará `then`. Devuelve si hay que esperar a la descarga.
    pub(super) fn fetch_first(&mut self, v: &[PathBuf], then: FetchThen) -> bool {
        if v.iter().any(|p| crate::sftp::is_remote(p) && !crate::sftp::is_live(p)) {
            self.say(t("Copia local de un servidor SFTP que ya no está conectado").into(), true);
            return true;
        }
        if !v.iter().any(|p| crate::sftp::is_live(p) && !crate::sftp::tree_fetched(p)) {
            return false;
        }
        let same = |m: &OpMeta| matches!(m, OpMeta::Fetch(t) if *t == then);
        if (self.op.is_some() && same(&self.op_meta)) || self.queue.iter().any(|(_, m)| same(m)) {
            return true;
        }
        self.start_op(Op::Fetch(v.to_vec()), OpMeta::Fetch(then));
        true
    }

    /// Termina lo pendiente de una descarga de `fetch_first` que acabó bien.
    pub(super) fn fetch_done(&mut self, then: FetchThen) {
        match then {
            FetchThen::Open(p) => self.open_remote_copy(&p),
            FetchThen::Clip(v, cut) => {
                // sólo si lo copiado sigue siendo eso
                if self.clipboard.as_ref().is_some_and(|(a, c)| *a == v && *c == cut) {
                    self.clip_sys = tools::clip_set_files(&v, cut);
                    if !self.clip_sys {
                        let text = v.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join("\n");
                        self.ctx.copy_text(text);
                    }
                    self.say(t("Descargado: ya se puede pegar en otras aplicaciones").into(), false);
                }
            }
            FetchThen::Drag => self.say(t("Descargado: ya se puede arrastrar fuera de la ventana").into(), false),
        }
    }

    /// Pide las miniaturas que han querido los paneles en este fotograma (más cerca del centro primero).
    pub(super) fn flush_thumbs(&mut self) {
        if std::mem::take(&mut self.thumb_any) {
            let mut w = std::mem::take(&mut self.thumb_want);
            w.sort_by_key(|(d, _)| *d);
            self.thumbs.set_wanted(w.into_iter().map(|(_, r)| r).collect());
        }
    }

    /// Hilo único que vigila la fecha de las carpetas de los paneles (`watch_paths`) y deja el
    /// resultado en `watch_res` sólo cuando cambia. Termina cuando se suelta `watch_wake`.
    #[cfg(not(target_arch = "wasm32"))]
    fn start_watcher(&mut self) {
        let (tx, rx) = channel::<()>();
        self.watch_wake = Some(tx);
        let (paths, slot, vis, ctx) = (self.watch_paths.clone(), self.watch_res.clone(), self.vis_state.clone(), self.ctx.clone());
        let r = std::thread::Builder::new().name("fb-watch".into()).spawn(move || {
            let mut last: Vec<(PathBuf, Option<SystemTime>)> = Vec::new();
            loop {
                // con foco cada 2 s, sin foco cada 6 s; minimizada, nada (despierta al volver)
                let v = vis.load(Ordering::Relaxed);
                let wait = match v {
                    2 => 2,
                    1 => 6,
                    _ => 3600,
                };
                if let Err(std::sync::mpsc::RecvTimeoutError::Disconnected) = rx.recv_timeout(Duration::from_secs(wait)) {
                    break;
                }
                if vis.load(Ordering::Relaxed) == 0 {
                    continue;
                }
                let ps = crate::thumbs::lock(&paths).clone();
                let r: Vec<_> = ps.into_iter().map(|p| {
                    let m = watch_stamp(&p);
                    (p, m)
                }).collect();
                if r != last {
                    last = r.clone();
                    *crate::thumbs::lock(&slot) = r;
                    ctx.request_repaint();
                }
            }
        });
        if r.is_err() {
            self.watch_wake = None;
        }
    }

    /// Vigilancia de cambios de las carpetas de los paneles: recarga sin mover la vista.
    pub(super) fn watch_tick(&mut self, ctx: &egui::Context, focused: bool) {
        let minimized = ctx.input(|i| i.viewport().minimized.unwrap_or(false));
        let vis = if minimized { 0 } else if focused { 2 } else { 1 };
        let was = self.vis_state.swap(vis, Ordering::Relaxed);
        let mut want = vec![self.cwd.clone()];
        if let Some(o) = self.other.as_deref() {
            if o.cwd != self.cwd {
                want.push(o.cwd.clone());
            }
        }
        let changed = {
            let mut p = crate::thumbs::lock(&self.watch_paths);
            if *p != want {
                *p = want;
                true
            } else {
                false
            }
        };
        #[cfg(not(target_arch = "wasm32"))]
        {
            let _ = was;
            if self.watch_wake.is_none() && self.last_check.elapsed() > Duration::from_secs(5) {
                // (si no se pudo crear el hilo, se reintenta de vez en cuando)
                self.last_check = Instant::now();
                self.start_watcher();
            }
            if changed || (vis != 0 && was == 0) || (vis == 2 && was != 2) {
                if let Some(tx) = &self.watch_wake {
                    let _ = tx.send(());
                }
            }
        }
        #[cfg(target_arch = "wasm32")]
        {
            // sin hilos: se consulta aquí, cada 3 s y sólo con foco
            let _ = was;
            if focused && (changed || self.last_check.elapsed() > Duration::from_secs(3)) {
                self.last_check = Instant::now();
                let paths = crate::thumbs::lock(&self.watch_paths).clone();
                let r: Vec<_> = paths.into_iter().map(|p| {
                    let m = watch_stamp(&p);
                    (p, m)
                }).collect();
                *crate::thumbs::lock(&self.watch_res) = r;
            }
            if focused {
                ctx.request_repaint_after(Duration::from_secs(3));
            }
        }
        let seen: Vec<_> = crate::thumbs::lock(&self.watch_res).clone();
        for (path, m) in seen {
            let Some(m) = m else { continue };
            // la carpeta ya no existe (borrada o renombrada fuera): a la más cercana que quede
            if m == SystemTime::UNIX_EPOCH {
                for other in [false, true] {
                    if other {
                        if !self.other.as_deref().map(|o| o.cwd == path).unwrap_or(false) {
                            continue;
                        }
                        self.swap_pane();
                    }
                    if self.cwd == path && self.vmode == VMode::Dir && !self.loading && !path.v_exists() {
                        let up = path.ancestors().skip(1).find(|a| a.v_is_dir()).map(Path::to_path_buf);
                        let to = up.or_else(|| self.home.clone()).unwrap_or_else(|| PathBuf::from("/"));
                        self.say(t("La carpeta ya no existe").into(), true);
                        self.navigate(to);
                    }
                    if other {
                        self.swap_pane();
                    }
                }
                continue;
            }
            // cada panel por su lado: los dos pueden estar en la misma carpeta
            for other in [false, true] {
                if other {
                    if !self.other.as_deref().map(|o| o.cwd == path).unwrap_or(false) {
                        continue;
                    }
                    self.swap_pane();
                }
                if self.cwd == path && self.vmode == VMode::Dir && !self.loading {
                    match self.dir_mtime {
                        // primera fecha conocida de esta carpeta: es la base, no un cambio
                        None => self.dir_mtime = Some(m),
                        Some(old) if old != m && self.op.is_none() && self.rename.is_none() && self.preview.is_none() => {
                            index::mark_dirty();
                            self.refresh_keep();
                        }
                        _ => {}
                    }
                }
                if other {
                    self.swap_pane();
                }
            }
        }
    }

    /// Saca a los dos paneles de `root` (a la carpeta personal) si están dentro.
    pub(super) fn leave(&mut self, root: &Path) {
        let home = self.home.clone().unwrap_or_else(|| PathBuf::from("/"));
        if self.cwd.starts_with(root) {
            self.navigate(home.clone());
        }
        // el panel inactivo se recarga de verdad: si no, la lista y la carpeta quedarían incoherentes
        if self.other.as_deref().map(|o| o.cwd.starts_with(root)).unwrap_or(false) {
            self.swap_pane();
            self.navigate(home);
            self.swap_pane();
        }
    }

    /// Desconecta un servidor: SFTP integrado (`sftp`) o un recurso conectado en Windows/macOS.
    pub(super) fn disconnect(&mut self, root: PathBuf, sftp: bool) {
        if sftp {
            // no puede fallar: se sale antes para no recargar una carpeta que se está borrando
            self.leave(&root);
            self.spawn_io(move || crate::sftp::disconnect(&root), |a, ()| {
                a.say(t("Desconectado").into(), false);
                a.refresh_network();
            });
            return;
        }
        #[cfg(any(windows, target_os = "macos"))]
        {
            let r2 = root.clone();
            self.spawn_io(move || crate::netmount::disconnect(&r2), move |a, r| {
                match r {
                    Ok(()) => {
                        // sólo si se desconectó: si no, se sigue donde se estaba
                        a.leave(&root);
                        a.say(t("Desconectado").into(), false);
                    }
                    Err(e) => a.say(tf("No se pudo desconectar: {}", &[&e]), true),
                }
                a.refresh_network();
                a.refresh_drives();
            });
        }
        #[cfg(not(any(windows, target_os = "macos")))]
        let _ = root;
    }

    /// Expulsa un disco extraíble (Linux: `gio mount -e`; macOS: `diskutil eject`; Windows: retirada
    /// segura del dispositivo, expulsión del medio o desconexión de la unidad de red).
    pub(super) fn eject(&mut self, path: PathBuf) {
        #[cfg(windows)]
        {
            self.say(t("Expulsando…").into(), false);
            let p2 = path.clone();
            let remote = tools::win_eject::is_remote(&path);
            self.spawn_io(
                move || {
                    // la vigilancia del índice tiene abierta la raíz: soltarla y dar tiempo al hilo a cerrarla
                    crate::index::release_drive(&p2);
                    std::thread::sleep(std::time::Duration::from_millis(200));
                    tools::win_eject::eject(&p2)
                },
                move |a, r| {
                    match r {
                        Ok(()) => {
                            a.leave(&path);
                            a.say(if remote { t("Desconectado") } else { t("Expulsado: ya se puede retirar") }.into(), false);
                        }
                        Err(e) => a.say(tf("No se pudo expulsar: {}", &[&e]), true),
                    }
                    a.refresh_drives();
                    if remote {
                        a.refresh_network();
                    }
                },
            );
        }
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            self.say(t("Expulsando…").into(), false);
            let p2 = path.clone();
            self.spawn_io(
                move || -> Result<(), String> {
                    #[cfg(target_os = "linux")]
                    let o = std::process::Command::new("timeout").arg("60").args(["gio", "mount", "-e"]).arg(&p2).stdin(std::process::Stdio::null()).output();
                    #[cfg(target_os = "macos")]
                    let o = std::process::Command::new("diskutil").arg("eject").arg(&p2).stdin(std::process::Stdio::null()).output();
                    let o = o.map_err(|e| e.to_string())?;
                    if o.status.success() {
                        return Ok(());
                    }
                    let e = String::from_utf8_lossy(&o.stderr).trim().to_string();
                    Err(if e.is_empty() { String::from_utf8_lossy(&o.stdout).trim().to_string() } else { e })
                },
                move |a, r| {
                    match r {
                        Ok(()) => {
                            a.leave(&path);
                            a.say(t("Expulsado: ya se puede retirar").into(), false);
                        }
                        Err(e) => a.say(tf("No se pudo expulsar: {}", &[&e]), true),
                    }
                    a.refresh_drives();
                },
            );
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
        let _ = path;
    }

    /// Desmonta un recurso de red (gvfs o montaje del sistema) en un hilo.
    #[cfg(target_os = "linux")]
    pub(super) fn unmount(&mut self, path: PathBuf) {
        self.say(t("Desmontando…").into(), false);
        let is_gvfs = gvfs_dir().map(|g| path.starts_with(g)).unwrap_or(false);
        let p2 = path.clone();
        self.spawn_io(
            move || -> Result<(), String> {
                use std::process::{Command, Stdio};
                let run = |prog: &str, args: &[&std::ffi::OsStr]| -> Result<(), String> {
                    let o = Command::new("timeout")
                        .arg("30")
                        .arg(prog)
                        .args(args)
                        .stdin(Stdio::null())
                        .output()
                        .map_err(|e| e.to_string())?;
                    if o.status.success() {
                        Ok(())
                    } else {
                        let e = String::from_utf8_lossy(&o.stderr).trim().to_string();
                        Err(if e.is_empty() { prog.to_string() } else { e })
                    }
                };
                let p = p2.as_os_str();
                if is_gvfs {
                    let r = run("gio", &[std::ffi::OsStr::new("mount"), std::ffi::OsStr::new("-u"), p]);
                    if r.is_ok() {
                        return r;
                    }
                    // por dirección, reconstruida del nombre del directorio gvfs
                    let n = p2.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    let (sch, props) = n.split_once(':').unwrap_or((&n, ""));
                    let get = |k: &str| props.split(',').find_map(|kv| kv.strip_prefix(&format!("{k}=")).map(String::from));
                    let uri = match sch {
                        "smb-share" => format!("smb://{}/{}", get("server").unwrap_or_default(), get("share").unwrap_or_default()),
                        "dav" | "davs" => format!("{}://{}/", if get("ssl").as_deref() == Some("true") { "davs" } else { "dav" }, get("host").unwrap_or_default()),
                        other => format!("{other}://{}/", get("host").unwrap_or_default()),
                    };
                    return run("gio", &[std::ffi::OsStr::new("mount"), std::ffi::OsStr::new("-u"), std::ffi::OsStr::new(&uri)]).or(r);
                }
                // montaje del sistema: gio (udisks), luego fusermount y umount
                run("gio", &[std::ffi::OsStr::new("mount"), std::ffi::OsStr::new("-u"), p])
                    .or_else(|_| run("fusermount", &[std::ffi::OsStr::new("-u"), p]))
                    .or_else(|_| run("umount", &[p]))
            },
            move |a, r| {
                match r {
                    Ok(()) => {
                        // se sale sólo si se desmontó: si falla, se sigue donde se estaba
                        a.leave(&path);
                        a.say(t("Desmontado").into(), false);
                    }
                    Err(e) => a.say(tf("No se pudo desmontar: {}", &[&e]), true),
                }
                a.refresh_network();
            },
        );
    }

    /// Recalcula unidades y raíces del índice en un hilo (evita bloquear la interfaz con unidades dormidas o de red).
    pub(super) fn refresh_drives(&mut self) {
        if self.drives_busy.swap(true, Ordering::Relaxed) {
            return;
        }
        let (slot, flag, ctx) = (self.drives_slot.clone(), self.drives_busy.clone(), self.ctx.clone());
        crate::vfs::thread::spawn(move || {
            let v = (list_drives(), tools::all_roots());
            *crate::thumbs::lock(&slot) = Some(v);
            flag.store(false, Ordering::Relaxed);
            ctx.request_repaint();
        });
    }

    /// Recalcula la lista de sitios de red en un hilo (evita bloquear la interfaz).
    pub(super) fn refresh_network(&mut self) {
        if self.net_refreshing.swap(true, Ordering::Relaxed) {
            return;
        }
        let (slot, flag, ctx) = (self.net_list.clone(), self.net_refreshing.clone(), self.ctx.clone());
        crate::vfs::thread::spawn(move || {
            let v = list_network();
            *crate::thumbs::lock(&slot) = Some(v);
            flag.store(false, Ordering::Relaxed);
            ctx.request_repaint();
        });
    }

    /// Abre el diálogo de conexión para una dirección (vacía = el usuario la escribe). Una contraseña
    /// escrita en la dirección pasa al campo «Contraseña» y no se enseña ni se guarda.
    pub(super) fn connect_uri(&mut self, uri: String) {
        let (uri, pass) = crate::netmount::strip_pass(uri.trim());
        let auth = uri.split_once("://").map(|(_, r)| r.split('/').next().unwrap_or("").to_string()).unwrap_or_default();
        let (user_in_uri, hp) = match auth.rsplit_once('@') {
            Some((u, h)) => (Some(u.to_string()), h.to_string()),
            None => (None, auth),
        };
        let host = crate::netmount::split_host_port(&hp).0.to_string();
        let user = user_in_uri.or_else(|| self.net_users.get(&host).cloned()).unwrap_or_default();
        self.dlg = Dlg::NetLogin(NetLogin { uri, user, domain: "WORKGROUP".into(), pass: pass.unwrap_or_default(), anon: false });
    }

    /// Deja de esperar la conexión en curso: su resultado, cuando llegue, se descarta.
    pub(super) fn cancel_mount(&mut self) {
        NET_GEN.fetch_add(1, Ordering::Relaxed);
        self.net_busy.store(false, Ordering::Relaxed);
        self.say(t("Conexión cancelada").into(), false);
    }

    /// Monta con `gio mount`; si la dirección no indica recurso, monta cada recurso compartido.
    pub(super) fn start_mount(&mut self, l: &NetLogin) {
        // la contraseña no viaja en la dirección (se ve en `ps` y se guardaría en `net_last`)
        let (uri, upass) = crate::netmount::strip_pass(l.uri.trim());
        let mut l = l.clone();
        l.uri = uri;
        if l.pass.is_empty() {
            l.pass = upass.unwrap_or_default();
        }
        let l = &l;
        self.net_last = Some(l.clone());
        // cada intento tiene su número: el resultado de uno cancelado o sustituido se ignora
        let try_n = NET_GEN.fetch_add(1, Ordering::Relaxed) + 1;
        #[cfg(target_os = "linux")]
        {
            let uri = l.uri.clone();
            let cred = GioCred { user: l.user.clone(), domain: l.domain.clone(), pass: l.pass.clone(), anon: l.anon };
            if let Some((_, r)) = uri.split_once("://") {
                let hp = r.split('/').next().unwrap_or("").rsplit('@').next().unwrap_or("");
                self.net_users.insert(crate::netmount::split_host_port(hp).0.to_string(), l.user.clone());
            }
            self.net_busy.store(true, Ordering::Relaxed);
            self.say(tf("Conectando con {}…", &[&uri]), false);
            let (slot, err_slot, busy, ctx) = (self.net_nav.clone(), self.net_err.clone(), self.net_busy.clone(), self.ctx.clone());
            let shares_slot = self.net_shares.clone();
            let login = NetLogin { uri: uri.clone(), user: l.user.clone(), domain: l.domain.clone(), pass: l.pass.clone(), anon: l.anon };
            crate::vfs::thread::spawn(move || {
                let is_sftp = uri.starts_with("sftp://") || uri.starts_with("ssh://");
                let sftp = |login: &NetLogin| {
                    let l = crate::netmount::Login { uri: login.uri.clone(), user: login.user.clone(), domain: String::new(), pass: login.pass.clone(), anon: login.anon };
                    crate::netmount::connect_sftp(&l).map(Some)
                };
                let res = (|| -> Result<Option<PathBuf>, String> {
                    // ya montado (por otra aplicación o sesión anterior): sin volver a autenticar
                    let server_only = uri.starts_with("smb://") && uri.trim_end_matches('/').split('/').nth(3).is_none();
                    if !server_only {
                        if let Some(p) = uri_to_gvfs(&uri).or_else(|| gio_local_path(&uri)) {
                            return Ok(Some(p));
                        }
                    }
                    // «ya montada» (el mensaje sale en el idioma del sistema): si gio puede listarla, vale.
                    // Un servidor ya montado en modo exploración no tiene ruta local: se montan sus recursos.
                    if let Err(m) = run_gio(&["mount"], &uri, &cred) {
                        if m == GIO_MISSING || m == GIO_ASK {
                            return Err(m);
                        }
                        let listable = std::process::Command::new("timeout")
                            .args(["15", "gio", "list", &uri])
                            .stdin(std::process::Stdio::null())
                            .output()
                            .map(|o| o.status.success())
                            .unwrap_or(false);
                        if !listable {
                            return Err(m);
                        }
                    }
                    if !server_only {
                        if let Some(p) = uri_to_gvfs(&uri) {
                            return Ok(Some(p));
                        }
                    }
                    // sólo un servidor SMB sin recurso tiene «recursos» entre los que elegir; en otro caso la
                    // lista sería la de carpetas y elegir una volvería a listar la siguiente
                    // (un recurso ya elegido que no se puede abrir da error, no otra lista)
                    if !server_only {
                        if is_sftp {
                            return sftp(&login);
                        }
                        return Err(t("El servidor se ha montado pero no tiene carpeta local (¿falta gvfs-fuse?)").to_string());
                    }
                    // servidor sin recurso: gvfs no lo expone como carpeta; se ofrecen sus recursos
                    // compartidos para elegir uno (sólo se monta ése)
                    let base = uri.trim_end_matches('/').to_string();
                    let out = std::process::Command::new("timeout")
                        .args(["20", "gio", "list"])
                        .arg(format!("{base}/"))
                        .stdin(std::process::Stdio::null())
                        .output()
                        .map_err(|e| e.to_string())?;
                    let shares: Vec<String> = String::from_utf8_lossy(&out.stdout)
                        .lines()
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty() && !s.ends_with('$'))
                        .collect();
                    if shares.is_empty() {
                        return Err(t("No hay recursos compartidos accesibles").to_string());
                    }
                    *crate::thumbs::lock(&shares_slot) = Some(NetShares { base, shares, login: login.clone() });
                    Ok(None)
                })();
                // sin gio, o con la pregunta de la clave de un servidor SSH nuevo, SFTP va por el cliente
                // integrado (que enseña la huella para confirmarla)
                let res = match res {
                    Err(m) if (m == GIO_MISSING || m == GIO_ASK) && is_sftp => sftp(&login),
                    Err(m) if m == GIO_MISSING => Err(t("Falta «gio» (gvfs): instálalo para conectar con este tipo de servidor").to_string()),
                    Err(m) if m == GIO_ASK => Err(t("El servidor hace una pregunta que no se puede contestar aquí: conecta una vez desde el gestor de archivos del sistema").to_string()),
                    r => r,
                };
                // cancelado o sustituido por otro intento: no se navega ni se avisa
                if NET_GEN.load(Ordering::Relaxed) != try_n {
                    crate::thumbs::lock(&shares_slot).take();
                    return;
                }
                match res {
                    Ok(Some(p)) => *crate::thumbs::lock(&slot) = Some(p),
                    Ok(None) => {}
                    Err(m) => *crate::thumbs::lock(&err_slot) = Some(m),
                }
                busy.store(false, Ordering::Relaxed);
                ctx.request_repaint();
            });
        }
        #[cfg(any(windows, target_os = "macos"))]
        {
            let uri = l.uri.clone();
            self.net_busy.store(true, Ordering::Relaxed);
            self.say(tf("Conectando con {}…", &[&uri]), false);
            let (slot, err_slot, busy, ctx) = (self.net_nav.clone(), self.net_err.clone(), self.net_busy.clone(), self.ctx.clone());
            let login = crate::netmount::Login { uri, user: l.user.clone(), domain: l.domain.clone(), pass: l.pass.clone(), anon: l.anon };
            crate::vfs::thread::spawn(move || {
                let r = crate::netmount::connect(&login);
                if NET_GEN.load(Ordering::Relaxed) != try_n {
                    return;
                }
                match r {
                    Ok(p) => *crate::thumbs::lock(&slot) = Some(p),
                    Err(m) => *crate::thumbs::lock(&err_slot) = Some(m),
                }
                busy.store(false, Ordering::Relaxed);
                ctx.request_repaint();
            });
        }
        #[cfg(not(any(target_os = "linux", windows, target_os = "macos")))]
        {
            let _ = (l, try_n);
            self.say(t("Conectar a servidores no está disponible en este sistema").into(), true);
        }
    }

    pub(super) fn sidebar(&mut self, ui: &mut egui::Ui) {
        let pal = self.pal;
        let ptr = ui.input(|i| i.pointer.hover_pos());
        let dragging = self.drag_items;
        self.side_drop = None;
        self.bm_drop = false;
        let mut go: Option<PathBuf> = None;
        let mut side_act: Option<SideAct> = None;
        let mut connect: Option<String> = None;
        #[cfg_attr(not(target_os = "linux"), allow(unused_mut))]
        let mut unmount: Option<PathBuf> = None;
        let mut disconnect: Option<(PathBuf, bool)> = None;
        let mut cancel_conn = false;
        let mut show_recent = false;
        let mut show_trash = false;
        let mut clear_recent = false;
        let mut empty_trash = false;
        let cwd = self.cwd.clone();
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            let head = |ui: &mut egui::Ui, t: &str| {
                ui.add_space(8.0);
                ui.label(egui::RichText::new(t.to_uppercase()).size(11.0).color(pal.subtle).strong());
                ui.add_space(2.0);
            };
            head(ui, t("Favoritos"));
            let sys_on = self.cfg.sys_icons;
            self.sidebar_favs(ui, ptr, dragging, &cwd, &mut go, &mut side_act);
            let r = side_item(ui, &pal, Glyph::Clock, None, t("Recientes"), cwd == Path::new(LOC_RECENT), false);
            if r.clicked() {
                show_recent = true;
            }
            r.context_menu(|ui| {
                if ui.add_enabled(!self.cfg.recent.is_empty(), egui::Button::new(t("Vaciar recientes"))).clicked() {
                    clear_recent = true;
                    ui.close();
                }
            });
            if extras::trash_supported() {
                let r = side_item(ui, &pal, Glyph::Trash, None, t("Papelera"), cwd == Path::new(LOC_TRASH), false);
                if r.clicked() {
                    show_trash = true;
                }
                r.context_menu(|ui| {
                    if ui.button(t("Vaciar papelera")).clicked() {
                        empty_trash = true;
                        ui.close();
                    }
                });
            }
            if !self.drives.is_empty() {
                head(ui, t("Dispositivos"));
            }
            for d in &self.drives {
                // el icono propio de la unidad (Cryptomator, Dropbox…) y, si no, el genérico
                let tex = if sys_on { self.sys.for_path(&self.ctx, &d.path).or_else(|| self.sys.folder(&self.ctx, Some(d.sys))) } else { None };
                let r = side_item(ui, &pal, d.glyph, tex, &d.name, d.path == cwd, false);
                if dragging && ptr.map(|q| r.rect.contains(q)).unwrap_or(false) {
                    self.side_drop = Some(d.path.clone());
                }
                if r.clicked() {
                    go = Some(d.path.clone());
                }
                let bm = self.is_fav(&d.path);
                r.context_menu(|ui| side_menu(ui, &d.path, SideKind::Drive, 0, 0, bm, false, &mut side_act));
            }
            head(ui, t("Red"));
            for n in &self.network {
                let active = n.path.as_ref().map(|p| *p == cwd).unwrap_or(false);
                let r = side_item(ui, &pal, Glyph::Network, None, &n.name, active, false);
                let tip = match (&n.path, &n.uri) {
                    (Some(p), _) => p.display().to_string(),
                    (None, Some(u)) => u.clone(),
                    _ => String::new(),
                };
                let r = r.on_hover_text(tip);
                #[cfg(target_os = "linux")]
                if let Some(p) = &n.path {
                    r.context_menu(|ui| {
                        if ui.button(t("Desmontar")).clicked() {
                            unmount = Some(p.clone());
                            ui.close();
                        }
                    });
                }
                if r.clicked() {
                    match (&n.path, &n.uri) {
                        (Some(p), _) => go = Some(p.clone()),
                        (None, Some(u)) => connect = Some(u.clone()),
                        _ => {}
                    }
                }
            }
            // servidores SFTP conectados con el cliente integrado
            for (name, root) in crate::sftp::live_sessions() {
                let active = cwd.starts_with(&root);
                let r = side_item(ui, &pal, Glyph::Network, None, &format!("{name} (SFTP)"), active, false).on_hover_text(format!("sftp://{name}/"));
                if r.clicked() {
                    go = Some(root.clone());
                }
                r.context_menu(|ui| {
                    if ui.button(t("Desconectar")).clicked() {
                        disconnect = Some((root.clone(), true));
                        ui.close();
                    }
                });
            }
            // recursos conectados desde aquí (Windows: net use; macOS: montajes)
            #[cfg(any(windows, target_os = "macos"))]
            for (uri, p) in crate::netmount::connections() {
                if crate::sftp::is_live(&p) || self.network.iter().any(|n| n.path.as_deref() == Some(p.as_path())) {
                    continue;
                }
                let r = side_item(ui, &pal, Glyph::Network, None, &uri, cwd.starts_with(&p), false).on_hover_text(p.display().to_string());
                if r.clicked() {
                    go = Some(p.clone());
                }
                r.context_menu(|ui| {
                    if ui.button(t("Desconectar")).clicked() {
                        disconnect = Some((p.clone(), false));
                        ui.close();
                    }
                });
            }
            if self.net_busy.load(Ordering::Relaxed) {
                ui.horizontal(|ui| {
                    ui.add_space(8.0);
                    ui.spinner();
                    let host = self.net_last.as_ref().map(|l| l.uri.clone()).unwrap_or_default();
                    ui.label(egui::RichText::new(tf("Conectando con {}…", &[&host])).color(pal.subtle)).on_hover_text(host);
                    if ui.small_button(t("Cancelar")).clicked() {
                        cancel_conn = true;
                    }
                });
            }
            if !crate::vfs::WEB {
                let r = side_item(ui, &pal, Glyph::Plus, None, t("Conectar a un servidor…"), false, false);
                if r.clicked() {
                    connect = Some(String::new());
                }
            }
        });
        if show_recent {
            self.navigate(PathBuf::from(LOC_RECENT));
        }
        if show_trash {
            self.navigate(PathBuf::from(LOC_TRASH));
        }
        if clear_recent {
            self.cfg.recent.clear();
            self.save_cfg();
            if self.vmode == VMode::Recent {
                self.refresh();
            }
        }
        if empty_trash {
            self.confirm_purge = Some(PurgeReq::All);
        }
        if cancel_conn {
            self.cancel_mount();
        }
        if let Some((p, sftp)) = disconnect {
            self.disconnect(p, sftp);
        }
        if let Some(u) = connect {
            self.connect_uri(u);
        }
        #[cfg(target_os = "linux")]
        if let Some(p) = unmount {
            self.unmount(p);
        }
        #[cfg(not(target_os = "linux"))]
        let _ = unmount;
        if let Some(a) = side_act {
            self.side_action(a);
        }
        if let Some(p) = go {
            self.navigate(p);
        }
    }

    /// Menú contextual del Explorador de Windows (con las extensiones del sistema) para esas rutas.
    #[cfg(windows)]
    pub(super) fn native_menu(&mut self, paths: Vec<PathBuf>, background: bool) {
        if !crate::winshell::context_menu(&paths, background) {
            self.say(t("No se pudo mostrar el menú de Windows").into(), true);
        }
        // el menú pudo cambiar archivos (comprimir, renombrar…)
        self.refresh();
    }

    /// Aplica una acción del menú contextual de la barra lateral.
    fn side_action(&mut self, a: SideAct) {
        match a {
            SideAct::Go(p) => self.navigate(p),
            SideAct::NewTab(p) => self.new_tab_bg(p),
            SideAct::NewWin(p) => self.new_window_at(&p),
            SideAct::CopyPath(p) => self.ctx.copy_text(p.display().to_string()),
            SideAct::Props(p) => self.open_props_paths(vec![p]),
            SideAct::Native(p) => {
                #[cfg(windows)]
                self.native_menu(vec![p], false);
                #[cfg(not(windows))]
                let _ = p;
            }
            SideAct::ToggleFav(p) => self.toggle_fav(p),
            SideAct::RestoreFavs => self.restore_favs(),
            SideAct::Move(i, d) => {
                let j = i as isize + d;
                if j >= 0 && (j as usize) < self.places.len() {
                    self.move_fav(i, j as usize);
                }
            }
            SideAct::Rename(p) => self.open_fav_label(p),
            SideAct::Icon(p) => self.open_folder_icon(p),
            SideAct::Eject(p) => self.eject(p),
        }
    }

    pub(super) fn status_bar(&mut self, ui: &mut egui::Ui) {
        let pal = self.pal;
        ui.horizontal_centered(|ui| {
            // «19 elementos (1 oculto, 2 filtrados)»
            let mut s = if self.loading { crate::i18n::tn("{} elemento|{} elementos", self.order.len(), &[]) } else { self.ux_status_count() };
            if self.loading {
                s.push_str(t("  ·  cargando…"));
            }
            if self.sel_count > 0 {
                s.push_str(&tf("  ·  {} seleccionados ({})", &[&self.sel_count, &fsys::human_size(self.sel_size)]));
            }
            ui.label(egui::RichText::new(s).size(12.5).color(pal.subtle));
            if let Some(n) = &self.hover_name {
                ui.label(egui::RichText::new(format!("  ·  {n}")).size(12.5).color(pal.text));
            }
            // los mensajes (`say`) salen como avisos apilados: ver `ux_toasts`
            if self.phone {
                return;
            }
            ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                let label = egui::RichText::new(t("Terminal")).size(12.0);
                if !crate::vfs::WEB && ui.selectable_label(self.cfg.term_open, label).on_hover_text(commands::with_shortcut(t("Mostrar/ocultar terminal"), Cmd::Terminal)).clicked() {
                    self.toggle_terminal();
                }
                if self.cfg.grid {
                    ui.spacing_mut().slider_width = 130.0;
                    let r = ui.add(egui::Slider::new(&mut self.cfg.zoom, 64.0..=200.0).show_value(false));
                    if r.drag_stopped() {
                        self.save_cfg();
                    }
                }
                if let Some(op) = self.op.clone() {
                    let tot = op.total.load(Ordering::Relaxed).max(1);
                    let d = op.done.load(Ordering::Relaxed).min(tot);
                    let scanning = crate::thumbs::lock(&op.label).as_str() == t("Calculando…");
                    let frac = if scanning { 0.0 } else { d as f32 / tot as f32 };
                    let mut txt = self.op_desc.clone();
                    if !scanning {
                        txt.push_str(&format!("  {:.0}%", frac * 100.0));
                    }
                    if !self.queue.is_empty() {
                        txt.push_str(&format!("  +{}", self.queue.len()));
                    }
                    let r = ui.add(egui::ProgressBar::new(frac).desired_width(220.0).text(egui::RichText::new(txt).size(12.0)).corner_radius(6));
                    let r = ui.interact(r.rect, egui::Id::new("tasks_btn"), Sense::click()).on_hover_text(t("Tareas en curso"));
                    if r.clicked() {
                        self.tasks_open = !self.tasks_open;
                    }
                    // sólo encima: forzar «Default» en cada fotograma pisaría el cursor que haya puesto otro widget
                    if r.hovered() {
                        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                    }
                }
            });
        });
    }

    /// Panel del portapapeles: lo copiado/cortado, pegar aquí, vaciar y ocultar.
    pub(super) fn clip_bar(&mut self, ui: &mut egui::Ui) {
        let pal = self.pal;
        let Some((paths, cut)) = self.clipboard.clone() else { return };
        let mut paste = false;
        let mut clear = false;
        let mut hide = false;
        ui.horizontal_centered(|ui| {
            ui.label(egui::RichText::new(if cut { t("Cortado:") } else { t("Copiado:") }).size(12.5).color(pal.accent));
            let mut shown = 0;
            let mut used = 0.0;
            let room = (ui.available_width() - 260.0).max(80.0);
            for p in &paths {
                let n = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| p.display().to_string());
                let g = ui.painter().layout_no_wrap(n.clone(), FontId::proportional(12.5), pal.text);
                if used + g.size().x + 14.0 > room && shown > 0 {
                    break;
                }
                used += g.size().x + 14.0;
                shown += 1;
                egui::Frame::new().fill(pal.hover).corner_radius(CornerRadius::same(6)).inner_margin(egui::Margin::symmetric(6, 1)).show(ui, |ui| {
                    ui.label(egui::RichText::new(n).size(12.5).color(pal.text));
                });
            }
            if paths.len() > shown {
                ui.label(egui::RichText::new(format!("+{}", paths.len() - shown)).size(12.5).color(pal.subtle));
            }
            ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                if ui.small_button(t("Cerrar")).clicked() {
                    hide = true;
                }
                if ui.small_button(t("Vaciar")).clicked() {
                    clear = true;
                }
                if ui.small_button(t("Pegar aquí")).clicked() {
                    paste = true;
                }
            });
        });
        if paste {
            self.do_paste();
        }
        if clear {
            self.clipboard = None;
        }
        if hide {
            self.clip_hidden = true;
        }
    }
}

#[cfg(all(test, target_os = "linux"))]
mod gvfs_tests {
    use super::*;

    #[test]
    fn uri_matches_gvfs_names() {
        let d = crate::vfs::temp_dir().join(format!("fbgvfs{}", crate::vfs::pid()));
        let g = d.join("gvfs");
        for n in ["smb-share:server=NAS,share=My Share", "sftp:host=pi.local,user=pi", "dav:host=cloud.x.org,ssl=true,prefix=%2Fremote.php"] {
            crate::vfs::fs::create_dir_all(g.join(n)).unwrap();
        }
        unsafe { std::env::set_var("XDG_RUNTIME_DIR", &d) };
        assert!(uri_to_gvfs("smb://nas/My%20Share").is_some());
        assert!(uri_to_gvfs("smb://user@nas/my share").is_some());
        assert!(uri_to_gvfs("smb://nas").is_some());
        assert!(uri_to_gvfs("smb://nas/otro").is_none());
        assert!(uri_to_gvfs("ssh://pi@pi.local:22/home").is_some());
        assert!(uri_to_gvfs("davs://cloud.x.org/remote.php").is_some());
        assert!(uri_to_gvfs("sftp://otro").is_none());
        // otro usuario u otro puerto es otra conexión
        assert!(uri_to_gvfs("sftp://root@pi.local").is_none());
        assert!(uri_to_gvfs("sftp://pi.local:2222").is_none());
        // subcarpeta existente: se entra en ella
        crate::vfs::fs::create_dir_all(g.join("sftp:host=pi.local,user=pi/home/pi")).unwrap();
        assert_eq!(uri_to_gvfs("sftp://pi@pi.local/home/pi"), Some(g.join("sftp:host=pi.local,user=pi/home/pi")));
        assert_eq!(uri_to_gvfs("smb://nas/My%20Share/no"), Some(g.join("smb-share:server=NAS,share=My Share")));
        let _ = crate::vfs::fs::remove_dir_all(d);
    }
}
