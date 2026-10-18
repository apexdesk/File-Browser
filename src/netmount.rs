//! Conexión a servidores de red en Windows y macOS usando lo que trae el sistema (sin dependencias):
//! SMB y WebDAV. Windows: `WNetAddConnection2`; macOS: `mount volume` de AppleScript.
//! En Linux se usa GVFS (`gio`), ver `app.rs`.
#[cfg(any(windows, target_os = "macos"))]
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
    /// segmentos de ruta sin vacíos (para SMB, el primero es el recurso)
    pub path: Vec<String>,
}

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
    let (user, hostport) = match auth.rsplit_once('@') {
        Some((u, h)) => (Some(pct_decode(u)), h),
        None => (None, auth),
    };
    let (host, port) = match hostport.rsplit_once(':') {
        Some((h, p)) if !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()) => (h, p.parse().ok()),
        _ => (hostport, None),
    };
    if host.is_empty() {
        return None;
    }
    let path = parts.filter(|s| !s.is_empty()).map(pct_decode).collect();
    Some(Parsed { kind, host: host.to_string(), port, user, path })
}

/// SFTP con el `ssh` del sistema (ver `sftp.rs`): devuelve la carpeta espejo del servidor.
#[cfg(any(windows, target_os = "macos"))]
fn sftp_connect(l: &Login, p: &Parsed) -> Result<PathBuf, String> {
    let user = if l.anon { String::new() } else if l.user.is_empty() { p.user.clone().unwrap_or_default() } else { l.user.clone() };
    let user = if user.is_empty() { std::env::var("USERNAME").or_else(|_| std::env::var("USER")).unwrap_or_default() } else { user };
    let conn = crate::sftp::Conn {
        host: p.host.clone(),
        port: p.port,
        user,
        pass: if l.pass.is_empty() || l.anon { None } else { Some(l.pass.clone()) },
    };
    let path = if p.path.is_empty() { String::new() } else { format!("/{}", p.path.join("/")) };
    crate::sftp::connect(conn, &path)
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
            53 | 67 => "no se encuentra el servidor o el recurso compartido".into(),
            86 | 1326 => "usuario o contraseña incorrectos".into(),
            1219 => "ya hay una conexión con otras credenciales; ciérrala antes (net use /delete)".into(),
            1244 | 5 => "acceso denegado".into(),
            1222 => "no hay red".into(),
            1231 => "no se puede llegar al servidor".into(),
            1203 | 1208 | 66 | 1214 => "el servicio de WebDAV (WebClient) no está disponible o la dirección no es válida".into(),
            n => format!("error de Windows {n}"),
        }
    }

    pub fn connect(l: &Login) -> Result<PathBuf, String> {
        let p = parse(&l.uri).ok_or_else(|| "dirección no válida".to_string())?;
        if p.kind == Kind::Sftp {
            return sftp_connect(l, &p);
        }
        let remote = match &p.kind {
            Kind::Smb => {
                if p.path.is_empty() {
                    return Err("indica el recurso compartido: smb://servidor/recurso".into());
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
            Kind::Unsupported(sc) => return Err(format!("«{sc}» no está disponible en este sistema")),
        };
        // ya accesible (sesión previa): no hace falta reconectar
        if std::fs::read_dir(&remote).is_ok() {
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
        let (wu, wp) = (wide(&user), wide(&l.pass));
        let have_user = !user.is_empty();
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
        Ok(PathBuf::from(remote))
    }

    pub fn disconnect(path: &std::path::Path) {
        #[link(name = "mpr")]
        unsafe extern "system" {
            fn WNetCancelConnection2W(name: *const u16, flags: u32, force: i32) -> u32;
        }
        let w = wide(&path.display().to_string());
        unsafe {
            WNetCancelConnection2W(w.as_ptr(), 0, 1);
        }
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use super::*;
    use std::process::{Command, Stdio};

    fn volumes() -> Vec<String> {
        std::fs::read_dir("/Volumes")
            .map(|rd| rd.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect())
            .unwrap_or_default()
    }

    pub fn connect(l: &Login) -> Result<PathBuf, String> {
        let p = parse(&l.uri).ok_or_else(|| "dirección no válida".to_string())?;
        if p.kind == Kind::Sftp {
            return sftp_connect(l, &p);
        }
        let (url, want) = match &p.kind {
            Kind::Smb => {
                if p.path.is_empty() {
                    return Err("indica el recurso compartido: smb://servidor/recurso".into());
                }
                (format!("smb://{}/{}", p.host, p.path.join("/")), p.path[0].clone())
            }
            Kind::Dav { tls } => {
                let mut u = format!("{}://{}", if *tls { "https" } else { "http" }, p.host);
                if let Some(port) = p.port {
                    u.push_str(&format!(":{port}"));
                }
                for s in &p.path {
                    u.push('/');
                    u.push_str(s);
                }
                (u, p.path.last().cloned().unwrap_or_else(|| p.host.clone()))
            }
            Kind::Sftp => unreachable!(),
            Kind::Unsupported(sc) => return Err(format!("«{sc}» no está disponible en este sistema")),
        };
        let before = volumes();
        let user = if l.anon { String::new() } else if l.user.is_empty() { p.user.clone().unwrap_or_default() } else { l.user.clone() };
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
            cmd.env("FB_MNT_PW", &l.pass);
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
            Some(v) => Ok(PathBuf::from("/Volumes").join(v)),
            None => Ok(PathBuf::from("/Volumes")),
        }
    }

    pub fn disconnect(path: &std::path::Path) {
        let _ = Command::new("diskutil").arg("unmount").arg(path).stdin(Stdio::null()).output();
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
    }
}
