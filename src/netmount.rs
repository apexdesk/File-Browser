//! Conexión a servidores de red en Windows y macOS usando lo que trae el sistema (sin dependencias):
//! SMB y WebDAV. Windows: `WNetAddConnection2`; macOS: `mount volume` de AppleScript.
//! En Linux se usa GVFS (`gio`), ver `app.rs`.
#[allow(unused_imports)]
use crate::vfs::{KindV as _, PathV as _};
#[allow(unused_imports)]
use crate::i18n::{t, tf};
#[cfg(any(windows, target_os = "macos", target_os = "linux"))]
use std::path::PathBuf;

pub struct Login {
    pub uri: String,
    pub user: String,
    pub domain: String,
    pub pass: String,
    pub anon: bool,
}

#[derive(Debug, PartialEq)]
pub enum Kind {
    Smb,
    Sftp,
    Dav { tls: bool },
    Unsupported(String),
}

#[derive(Debug)]
pub struct Parsed {
    pub kind: Kind,
    pub host: String,
    pub port: Option<u16>,
    pub user: Option<String>,
    /// contraseña escrita en la dirección («usuario:clave@»): no debe ir a ningún otro sitio
    pub pass: Option<String>,
    /// segmentos de ruta sin vacíos (para SMB, el primero es el recurso)
    pub path: Vec<String>,
}

/// Codifica todo salvo los caracteres «no reservados» de una URL.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn pct_encode(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
            o.push(b as char);
        } else {
            o.push_str(&format!("%{b:02X}"));
        }
    }
    o
}

fn pct_decode(s: &str) -> String {
    String::from_utf8_lossy(&pct_decode_bytes(s)).into_owned()
}

/// Como `pct_decode` pero sin pasar a texto: un «%FF» es el byte 0xFF (nombres SFTP que no son UTF-8).
fn pct_decode_bytes(s: &str) -> Vec<u8> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && i + 2 < b.len()
            && let Ok(v) = u8::from_str_radix(if b[i + 1..i + 3].iter().all(u8::is_ascii_hexdigit) { std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or("zz") } else { "zz" }, 16)
        {
            out.push(v);
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    out
}

/// Ruta de una dirección («sftp://h/a/b%FF» -> «/a/b\xFF») con los bytes tal cual.
#[cfg_attr(not(any(windows, target_os = "macos", target_os = "linux")), allow(dead_code))]
fn raw_path(uri: &str) -> Vec<u8> {
    let rest = uri.trim().split_once("://").map(|(_, r)| r).unwrap_or("");
    let mut out = Vec::new();
    for seg in rest.split('/').skip(1).filter(|s| !s.is_empty()) {
        out.push(b'/');
        out.extend_from_slice(&pct_decode_bytes(seg));
    }
    out
}

pub fn parse(uri: &str) -> Option<Parsed> {
    let uri = uri.trim();
    let (scheme, rest) = uri.split_once("://")?;
    let kind = match scheme.to_lowercase().as_str() {
        "smb" | "cifs" => Kind::Smb,
        "sftp" | "ssh" => Kind::Sftp,
        "dav" | "http" | "webdav" => Kind::Dav { tls: false },
        "davs" | "https" | "webdavs" => Kind::Dav { tls: true },
        other => Kind::Unsupported(other.to_string()),
    };
    let mut parts = rest.split('/');
    let auth = parts.next()?;
    let (user, pass, hostport) = match auth.rsplit_once('@') {
        Some((u, h)) => match u.split_once(':') {
            Some((u, pw)) => (Some(pct_decode(u)), Some(pct_decode(pw)), h),
            None => (Some(pct_decode(u)), None, h),
        },
        None => (None, None, auth),
    };
    let (host, port) = split_host_port(hostport);
    if host.is_empty() {
        return None;
    }
    let path = parts.filter(|s| !s.is_empty()).map(pct_decode).collect();
    Some(Parsed { kind, host: host.to_string(), port, user, pass, path })
}

/// «servidor:puerto» -> (servidor, puerto). IPv6 entre corchetes («[::1]:22» -> «::1»); sin
/// corchetes y con varios «:» es una IPv6 sin puerto («fe80::1»).
pub fn split_host_port(hp: &str) -> (&str, Option<u16>) {
    if let Some(rest) = hp.strip_prefix('[') {
        return match rest.split_once(']') {
            Some((h, tail)) => (h, tail.strip_prefix(':').and_then(|p| p.parse().ok())),
            None => (rest, None),
        };
    }
    match hp.split_once(':') {
        Some((h, p)) if !p.contains(':') && !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()) => (h, p.parse().ok()),
        Some((h, "")) => (h, None),
        _ => (hp, None),
    }
}

/// Quita la contraseña de una dirección («sftp://ana:clave@h/x» -> «sftp://ana@h/x») y la devuelve
/// aparte (descodificada): la dirección se enseña, se guarda y se pasa a otros programas.
pub fn strip_pass(uri: &str) -> (String, Option<String>) {
    let Some((scheme, rest)) = uri.split_once("://") else { return (uri.to_string(), None) };
    let (auth, path) = match rest.find('/') {
        Some(i) => rest.split_at(i),
        None => (rest, ""),
    };
    match auth.rsplit_once('@') {
        Some((ui, host)) => match ui.split_once(':') {
            Some((u, pw)) => (format!("{scheme}://{u}@{host}{path}"), Some(pct_decode(pw))),
            None => (uri.to_string(), None),
        },
        None => (uri.to_string(), None),
    }
}

/// Conexiones hechas desde aquí (Windows y macOS): (dirección, carpeta). Se enseñan en «Red» con
/// «Desconectar».
#[cfg(any(windows, target_os = "macos"))]
static CONNS: std::sync::Mutex<Vec<(String, PathBuf)>> = std::sync::Mutex::new(Vec::new());

#[cfg(any(windows, target_os = "macos"))]
fn remember(uri: &str, p: &std::path::Path) {
    let mut g = CONNS.lock().unwrap_or_else(|e| e.into_inner());
    if !g.iter().any(|(_, q)| q == p) {
        g.push((strip_pass(uri).0, p.to_path_buf()));
    }
}

#[cfg(any(windows, target_os = "macos"))]
pub fn connections() -> Vec<(String, PathBuf)> {
    CONNS.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// SFTP con el `ssh` del sistema (ver `sftp.rs`) para una dirección sftp:// / ssh://.
/// En Linux sólo se usa cuando GVFS monta el servidor pero no expone una carpeta local.
#[cfg(target_os = "linux")]
pub fn connect_sftp(l: &Login) -> Result<PathBuf, String> {
    let p = parse(&l.uri).ok_or_else(|| t("Dirección no válida").to_string())?;
    sftp_connect(l, &p)
}

/// SFTP con el `ssh` del sistema (ver `sftp.rs`): devuelve la carpeta espejo del servidor.
#[cfg(any(windows, target_os = "macos", target_os = "linux"))]
fn sftp_connect(l: &Login, p: &Parsed) -> Result<PathBuf, String> {
    let user = if l.anon { String::new() } else if l.user.is_empty() { p.user.clone().unwrap_or_default() } else { l.user.clone() };
    let user = if user.is_empty() { std::env::var("USERNAME").or_else(|_| std::env::var("USER")).unwrap_or_default() } else { user };
    let pass = if l.pass.is_empty() { p.pass.clone().unwrap_or_default() } else { l.pass.clone() };
    let conn = crate::sftp::Conn {
        host: p.host.clone(),
        port: p.port,
        user,
        pass: if pass.is_empty() || l.anon { None } else { Some(pass) },
    };
    // la ruta sale de la dirección con sus bytes (`p.path` es texto: perdería los que no son UTF-8)
    crate::sftp::connect(conn, &raw_path(&l.uri))
}

#[cfg(windows)]
mod imp {
    use super::*;
    use std::os::windows::ffi::OsStrExt;

    #[repr(C)]
    struct NetResource {
        scope: u32,
        kind: u32,
        display: u32,
        usage: u32,
        local: *mut u16,
        remote: *mut u16,
        comment: *mut u16,
        provider: *mut u16,
    }

    #[link(name = "mpr")]
    unsafe extern "system" {
        fn WNetAddConnection2W(res: *const NetResource, pass: *const u16, user: *const u16, flags: u32) -> u32;
    }

    fn wide(s: &str) -> Vec<u16> {
        std::ffi::OsStr::new(s).encode_wide().chain(std::iter::once(0)).collect()
    }

    fn message(code: u32) -> String {
        match code {
            53 | 67 => t("No se encuentra el servidor o el recurso compartido").into(),
            86 | 1326 => t("Usuario o contraseña incorrectos").into(),
            1219 => t("Ya hay una conexión con ese servidor con otras credenciales: desconéctala (en «Red», o con «net use \\\\servidor /delete») y vuelve a intentarlo").into(),
            1244 | 5 => t("Acceso denegado").into(),
            1222 => t("No hay red").into(),
            1231 => t("No se puede llegar al servidor").into(),
            1203 | 1208 | 66 | 1214 => t("El servicio de WebDAV (WebClient) no está disponible o la dirección no es válida").into(),
            n => tf("Error de Windows {}", &[&n]),
        }
    }

    pub fn connect(l: &Login) -> Result<PathBuf, String> {
        let p = parse(&l.uri).ok_or_else(|| t("Dirección no válida").to_string())?;
        if p.kind == Kind::Sftp {
            return sftp_connect(l, &p);
        }
        let remote = match &p.kind {
            Kind::Smb => {
                if p.path.is_empty() {
                    return Err(t("Indica también el recurso compartido: smb://servidor/recurso").into());
                }
                format!(r"\\{}\{}", p.host, p.path.join(r"\"))
            }
            Kind::Dav { tls } => {
                // \\host@SSL@puerto\DavWWWRoot\ruta
                let mut srv = p.host.clone();
                if *tls {
                    srv.push_str("@SSL");
                }
                if let Some(port) = p.port {
                    srv.push_str(&format!("@{port}"));
                }
                let mut s = format!(r"\\{srv}\DavWWWRoot");
                for seg in &p.path {
                    s.push('\\');
                    s.push_str(seg);
                }
                s
            }
            Kind::Sftp => unreachable!(),
            Kind::Unsupported(sc) => return Err(tf("«{}» no está disponible en este sistema", &[sc])),
        };
        // ya accesible (sesión previa): no hace falta reconectar
        if crate::vfs::fs::read_dir(&remote).is_ok() {
            remember(&l.uri, std::path::Path::new(&remote));
            return Ok(PathBuf::from(remote));
        }
        let user = if l.anon {
            "guest".to_string()
        } else {
            let u = if l.user.is_empty() { p.user.clone().unwrap_or_default() } else { l.user.clone() };
            let d = l.domain.trim();
            if !u.is_empty() && !u.contains('\\') && !u.contains('@') && !d.is_empty() && !d.eq_ignore_ascii_case("WORKGROUP") && p.kind == Kind::Smb {
                format!(r"{d}\{u}")
            } else {
                u
            }
        };
        let mut rname = wide(&remote);
        let res = NetResource {
            scope: 0,
            kind: 1, // RESOURCETYPE_DISK
            display: 0,
            usage: 0,
            local: std::ptr::null_mut(),
            remote: rname.as_mut_ptr(),
            comment: std::ptr::null_mut(),
            provider: std::ptr::null_mut(),
        };
        // invitado: sin contraseña (no se manda lo que hubiera quedado escrito en el campo)
        let pass = if l.anon { String::new() } else if l.pass.is_empty() { p.pass.clone().unwrap_or_default() } else { l.pass.clone() };
        let (wu, wp) = (wide(&user), wide(&pass));
        let have_user = !user.is_empty();
        // SAFETY: `res` apunta a `rname`, y las cadenas `wu`/`wp` (anchas, terminadas en NUL) viven
        // hasta después de la llamada; los punteros nulos son opcionales según la API.
        let code = unsafe {
            WNetAddConnection2W(
                &res,
                if have_user { wp.as_ptr() } else { std::ptr::null() },
                if have_user { wu.as_ptr() } else { std::ptr::null() },
                0,
            )
        };
        if code != 0 {
            return Err(message(code));
        }
        remember(&l.uri, std::path::Path::new(&remote));
        Ok(PathBuf::from(remote))
    }

    pub fn disconnect(path: &std::path::Path) -> Result<(), String> {
        #[link(name = "mpr")]
        unsafe extern "system" {
            fn WNetCancelConnection2W(name: *const u16, flags: u32, force: i32) -> u32;
        }
        let w = wide(&path.display().to_string());
        // SAFETY: cadena ancha terminada en NUL que vive durante la llamada.
        let code = unsafe { WNetCancelConnection2W(w.as_ptr(), 0, 1) };
        if code == 0 || code == 2250 {
            // 2250: ya no estaba conectada
            CONNS.lock().unwrap_or_else(|e| e.into_inner()).retain(|(_, q)| q != path);
            Ok(())
        } else {
            Err(message(code))
        }
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use super::*;
    use std::process::{Command, Stdio};

    fn volumes() -> Vec<String> {
        crate::vfs::fs::read_dir("/Volumes")
            .map(|rd| rd.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect())
            .unwrap_or_default()
    }

    pub fn connect(l: &Login) -> Result<PathBuf, String> {
        let p = parse(&l.uri).ok_or_else(|| t("Dirección no válida").to_string())?;
        if p.kind == Kind::Sftp {
            return sftp_connect(l, &p);
        }
        let user = if l.anon { String::new() } else if l.user.is_empty() { p.user.clone().unwrap_or_default() } else { l.user.clone() };
        let host = if p.host.contains(':') { format!("[{}]", p.host) } else { p.host.clone() };
        // los segmentos ya vienen descodificados: se vuelven a codificar (espacios en el recurso…)
        let segs = p.path.iter().map(|s| pct_encode(s)).collect::<Vec<_>>().join("/");
        let (url, want) = match &p.kind {
            Kind::Smb => {
                if p.path.is_empty() {
                    return Err(t("Indica también el recurso compartido: smb://servidor/recurso").into());
                }
                // el dominio va en la dirección: smb://DOMINIO;usuario@servidor/recurso
                let d = l.domain.trim();
                let auth = if !user.is_empty() && !d.is_empty() && !d.eq_ignore_ascii_case("WORKGROUP") { format!("{};{}@", pct_encode(d), pct_encode(&user)) } else { String::new() };
                (format!("smb://{auth}{host}/{segs}"), p.path[0].clone())
            }
            Kind::Dav { tls } => {
                let mut u = format!("{}://{host}", if *tls { "https" } else { "http" });
                if let Some(port) = p.port {
                    u.push_str(&format!(":{port}"));
                }
                if !segs.is_empty() {
                    u.push('/');
                    u.push_str(&segs);
                }
                (u, p.path.last().cloned().unwrap_or_else(|| p.host.clone()))
            }
            Kind::Sftp => unreachable!(),
            Kind::Unsupported(sc) => return Err(tf("«{}» no está disponible en este sistema", &[sc])),
        };
        let before = volumes();
        let mut cmd = Command::new("osascript");
        if user.is_empty() {
            cmd.args(["-e", "on run argv", "-e", "mount volume (item 1 of argv)", "-e", "end run", &url]);
        } else {
            cmd.args([
                "-e",
                "on run argv",
                "-e",
                "mount volume (item 1 of argv) as user name (item 2 of argv) with password (system attribute \"FB_MNT_PW\")",
                "-e",
                "end run",
                &url,
                &user,
            ]);
            // la contraseña va por el entorno, no por la línea de órdenes (visible con `ps`)
            cmd.env("FB_MNT_PW", if l.pass.is_empty() { p.pass.clone().unwrap_or_default() } else { l.pass.clone() });
        }
        let out = cmd.stdin(Stdio::null()).output().map_err(|e| e.to_string())?;
        if !out.status.success() {
            let m = String::from_utf8_lossy(&out.stderr).trim().to_string();
            return Err(m.rsplit(':').next().unwrap_or(&m).trim().to_string());
        }
        let after = volumes();
        let w = want.to_lowercase();
        let pick = after
            .iter()
            .find(|v| v.to_lowercase() == w)
            .or_else(|| after.iter().find(|v| !before.contains(v)))
            .or_else(|| after.iter().find(|v| v.to_lowercase().starts_with(&w)));
        match pick {
            Some(v) => {
                let dir = PathBuf::from("/Volumes").join(v);
                remember(&l.uri, &dir);
                Ok(dir)
            }
            None => Ok(PathBuf::from("/Volumes")),
        }
    }

    pub fn disconnect(path: &std::path::Path) -> Result<(), String> {
        let o = Command::new("diskutil").arg("unmount").arg(path).stdin(Stdio::null()).output().map_err(|e| e.to_string())?;
        if o.status.success() || !path.v_exists() {
            CONNS.lock().unwrap_or_else(|e| e.into_inner()).retain(|(_, q)| q != path);
            Ok(())
        } else {
            let e = String::from_utf8_lossy(&o.stderr).trim().to_string();
            Err(if e.is_empty() { "diskutil".into() } else { e })
        }
    }
}

#[cfg(any(windows, target_os = "macos"))]
pub use imp::{connect, disconnect};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_uris() {
        let p = parse("smb://nas/media/pelis").unwrap();
        assert_eq!(p.kind, Kind::Smb);
        assert_eq!(p.host, "nas");
        assert_eq!(p.path, vec!["media", "pelis"]);
        let p = parse("https://user@cloud.example.com:8443/remote.php/webdav").unwrap();
        assert_eq!(p.kind, Kind::Dav { tls: true });
        assert_eq!(p.port, Some(8443));
        assert_eq!(p.user.as_deref(), Some("user"));
        assert_eq!(p.path, vec!["remote.php", "webdav"]);
        assert_eq!(parse("sftp://h").unwrap().kind, Kind::Sftp);
        assert!(matches!(parse("nfs://h").unwrap().kind, Kind::Unsupported(_)));
        assert!(parse("sin-esquema").is_none());
        assert_eq!(parse("smb://nas/My%20Share").unwrap().path, vec!["My Share"]);
        // contraseña en la dirección: aparte
        let p = parse("sftp://ana:se%40creto@h:2222/x").unwrap();
        assert_eq!((p.user.as_deref(), p.pass.as_deref(), p.port), (Some("ana"), Some("se@creto"), Some(2222)));
        assert_eq!(strip_pass("sftp://ana:se%40creto@h:2222/x"), ("sftp://ana@h:2222/x".to_string(), Some("se@creto".to_string())));
        assert_eq!(strip_pass("smb://nas/a:b"), ("smb://nas/a:b".to_string(), None));
        // IPv6
        let p = parse("sftp://[::1]:2222/srv").unwrap();
        assert_eq!((p.host.as_str(), p.port), ("::1", Some(2222)));
        let p = parse("sftp://u@fe80::1/srv").unwrap();
        assert_eq!((p.host.as_str(), p.port), ("fe80::1", None));
        assert_eq!(pct_encode("My Share"), "My%20Share");
        // ruta SFTP con bytes que no son UTF-8
        assert_eq!(raw_path("sftp://ana@h:22/home/ana/caf%E9/50%25"), b"/home/ana/caf\xE9/50%".to_vec());
        assert_eq!(raw_path("sftp://h"), Vec::<u8>::new());
    }
}
