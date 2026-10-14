//! Integración con el sistema: argumentos de línea de órdenes, instancia única y registro como
//! gestor de archivos (escritorio).
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

/// Rutas pedidas en la línea de órdenes (ya absolutas).
static TARGETS: OnceLock<Vec<PathBuf>> = OnceLock::new();

pub fn initial_targets() -> Vec<PathBuf> {
    TARGETS.get().cloned().unwrap_or_default()
}

/// Convierte `file:///a%20b`, `~/x` o rutas relativas en una ruta absoluta.
pub fn parse_target(arg: &str) -> Option<PathBuf> {
    let p = if let Some(rest) = arg.strip_prefix("file://") {
        // file://host/ruta o file:///ruta
        let path = rest.find('/').map(|i| &rest[i..]).unwrap_or(rest);
        PathBuf::from(pct_decode(path))
    } else if let Some(rest) = arg.strip_prefix("~/") {
        dirs::home_dir()?.join(rest)
    } else if arg == "~" {
        dirs::home_dir()?
    } else {
        PathBuf::from(arg)
    };
    if p.as_os_str().is_empty() {
        return None;
    }
    let p = if p.is_absolute() { p } else { std::env::current_dir().ok()?.join(p) };
    Some(clean(&p))
}

fn pct_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
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

/// Quita `.` y `..` sin tocar el disco (no resuelve enlaces simbólicos).
fn clean(p: &Path) -> PathBuf {
    use std::path::Component as C;
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            C::CurDir => {}
            C::ParentDir => {
                out.pop();
            }
            c => out.push(c.as_os_str()),
        }
    }
    out
}

pub enum Cli {
    Run,
    Exit(i32),
}

const HELP: &str = "File Browser — fast, lightweight file manager

USAGE:
    filebrowser [OPTIONS] [PATH|URI]...

Folders open in tabs; files open their folder with the file selected.
If File Browser is already running, the paths are opened there.

OPTIONS:
    -n, --new-window   always start a new window
        --install      register as a file manager for this user (desktop entry, icon, default for folders)
        --uninstall    remove that registration
    -V, --version      print the version
    -h, --help         print this help";

/// Procesa los argumentos. Devuelve `Exit` si ya no hay que abrir ventana.
pub fn start() -> Cli {
    let mut targets: Vec<PathBuf> = Vec::new();
    let mut new_window = false;
    for a in std::env::args().skip(1) {
        match a.as_str() {
            "-h" | "--help" => {
                println!("{HELP}");
                return Cli::Exit(0);
            }
            "-V" | "--version" => {
                println!("filebrowser {}", env!("CARGO_PKG_VERSION"));
                return Cli::Exit(0);
            }
            "-n" | "--new-window" => new_window = true,
            "--install" | "--uninstall" => {
                let r = if a == "--install" { install() } else { uninstall() };
                return match r {
                    Ok(m) => {
                        println!("{m}");
                        Cli::Exit(0)
                    }
                    Err(e) => {
                        eprintln!("{e}");
                        Cli::Exit(1)
                    }
                };
            }
            s if s.starts_with('-') && s.len() > 1 => {}
            s => {
                if let Some(p) = parse_target(s) {
                    targets.push(p);
                }
            }
        }
    }
    // Con rutas y una instancia en marcha, se le pasan y salimos.
    if !new_window && !targets.is_empty() && forward(&targets) {
        return Cli::Exit(0);
    }
    let _ = TARGETS.set(targets);
    Cli::Run
}

// ---------------------------------------------------------------------------
// Instancia única (unix de escritorio)
// ---------------------------------------------------------------------------

#[cfg(all(unix, not(target_os = "android")))]
fn socket_path() -> PathBuf {
    let dir = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).filter(|p| p.is_dir()).unwrap_or_else(std::env::temp_dir);
    let uid = unsafe { libc::getuid() };
    dir.join(format!("filebrowser-rs-{uid}.sock"))
}

#[cfg(all(unix, not(target_os = "android")))]
fn forward(targets: &[PathBuf]) -> bool {
    use std::io::Write;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::net::UnixStream;
    let Ok(mut s) = UnixStream::connect(socket_path()) else { return false };
    let mut buf = Vec::new();
    for t in targets {
        buf.extend_from_slice(t.as_os_str().as_bytes());
        buf.push(b'\n');
    }
    s.write_all(&buf).is_ok()
}

#[cfg(not(all(unix, not(target_os = "android"))))]
fn forward(_: &[PathBuf]) -> bool {
    false
}

/// Cola de rutas que llegan desde otras invocaciones del programa.
pub type Inbox = Arc<Mutex<Vec<PathBuf>>>;

/// Empieza a escuchar a otras invocaciones; `wake` despierta a la interfaz.
#[cfg(all(unix, not(target_os = "android")))]
pub fn listen(wake: impl Fn() + Send + 'static) -> Inbox {
    use std::io::Read;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::net::{UnixListener, UnixStream};
    let inbox: Inbox = Arc::new(Mutex::new(Vec::new()));
    let path = socket_path();
    // Otra instancia viva conserva el socket; si está muerto, se sustituye.
    if UnixStream::connect(&path).is_ok() {
        return inbox;
    }
    let _ = std::fs::remove_file(&path);
    let Ok(l) = UnixListener::bind(&path) else { return inbox };
    let q = inbox.clone();
    std::thread::spawn(move || {
        for c in l.incoming() {
            let Ok(mut c) = c else { continue };
            let _ = c.set_read_timeout(Some(std::time::Duration::from_secs(2)));
            let mut buf = Vec::new();
            let _ = c.read_to_end(&mut buf);
            let mut g = q.lock().unwrap();
            for line in buf.split(|&b| b == b'\n').filter(|l| !l.is_empty()) {
                g.push(PathBuf::from(std::ffi::OsStr::from_bytes(line)));
            }
            drop(g);
            wake();
        }
    });
    inbox
}

#[cfg(not(all(unix, not(target_os = "android"))))]
pub fn listen(_wake: impl Fn() + Send + 'static) -> Inbox {
    Arc::new(Mutex::new(Vec::new()))
}

// ---------------------------------------------------------------------------
// Registro como gestor de archivos
// ---------------------------------------------------------------------------

/// ¿Hay un mecanismo de registro en esta plataforma?
pub fn can_install() -> bool {
    cfg!(any(target_os = "linux", windows))
}

#[cfg(target_os = "linux")]
pub fn install() -> Result<String, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let data = dirs::data_dir().ok_or("no data directory")?;
    let apps = data.join("applications");
    let icons = data.join("icons/hicolor/256x256/apps");
    std::fs::create_dir_all(&apps).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&icons).map_err(|e| e.to_string())?;
    std::fs::write(icons.join("filebrowser-rs.png"), include_bytes!("../assets/icon-256.png")).map_err(|e| e.to_string())?;
    let exec = exe.display().to_string().replace('\\', "\\\\").replace('"', "\\\"");
    let desktop = format!(
        "[Desktop Entry]\nType=Application\nName=File Browser\nComment=Fast, lightweight file manager\n\
         Exec=\"{exec}\" %U\nIcon=filebrowser-rs\nTerminal=false\nCategories=System;FileTools;FileManager;\n\
         MimeType=inode/directory;\nStartupWMClass=filebrowser-rs\nKeywords=files;folders;explorer;\n"
    );
    let f = apps.join("filebrowser-rs.desktop");
    std::fs::write(&f, desktop).map_err(|e| e.to_string())?;
    let run = |prog: &str, args: &[&str]| {
        std::process::Command::new(prog)
            .args(args)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    };
    run("update-desktop-database", &[&apps.display().to_string()]);
    run("gtk-update-icon-cache", &["-q", "-t", &data.join("icons/hicolor").display().to_string()]);
    let ok = run("xdg-mime", &["default", "filebrowser-rs.desktop", "inode/directory"]);
    Ok(format!(
        "Installed {}{}",
        f.display(),
        if ok { "; set as the default application for folders" } else { " (xdg-mime not found: set the default for folders manually)" }
    ))
}

#[cfg(target_os = "linux")]
pub fn uninstall() -> Result<String, String> {
    let data = dirs::data_dir().ok_or("no data directory")?;
    let mut n = 0;
    for f in [data.join("applications/filebrowser-rs.desktop"), data.join("icons/hicolor/256x256/apps/filebrowser-rs.png")] {
        if std::fs::remove_file(&f).is_ok() {
            n += 1;
        }
    }
    let _ = std::process::Command::new("update-desktop-database").arg(data.join("applications")).status();
    Ok(format!("Removed {n} file(s)"))
}

#[cfg(windows)]
fn reg(args: &[&str]) -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    let st = std::process::Command::new("reg").args(args).creation_flags(0x0800_0000).status().map_err(|e| e.to_string())?;
    if st.success() { Ok(()) } else { Err(format!("reg {} failed", args.first().unwrap_or(&""))) }
}

#[cfg(windows)]
const WIN_KEYS: [(&str, &str); 2] = [
    (r"HKCU\Software\Classes\Directory\shell\FileBrowser", "%1"),
    (r"HKCU\Software\Classes\Directory\Background\shell\FileBrowser", "%V"),
];

#[cfg(windows)]
pub fn install() -> Result<String, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?.display().to_string();
    for (key, arg) in WIN_KEYS {
        reg(&["add", key, "/ve", "/d", "Open in File Browser", "/f"])?;
        reg(&["add", key, "/v", "Icon", "/d", &format!("\"{exe}\""), "/f"])?;
        reg(&["add", &format!(r"{key}\command"), "/ve", "/d", &format!("\"{exe}\" \"{arg}\""), "/f"])?;
    }
    Ok("Added \"Open in File Browser\" to the folder context menu".into())
}

#[cfg(windows)]
pub fn uninstall() -> Result<String, String> {
    for (key, _) in WIN_KEYS {
        let _ = reg(&["delete", key, "/f"]);
    }
    Ok("Removed the folder context menu entries".into())
}

#[cfg(not(any(target_os = "linux", windows)))]
pub fn install() -> Result<String, String> {
    Err("Not available on this platform: use the application bundle / package".into())
}

#[cfg(not(any(target_os = "linux", windows)))]
pub fn uninstall() -> Result<String, String> {
    Err("Not available on this platform".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn targets() {
        assert_eq!(parse_target("file:///tmp/a%20b"), Some(PathBuf::from("/tmp/a b")));
        assert_eq!(parse_target("/tmp/x/../y/./z"), Some(PathBuf::from("/tmp/y/z")));
        assert!(parse_target("").is_none());
        assert_eq!(pct_decode("100%"), "100%");
        assert_eq!(pct_decode("%zz%41"), "%zzA");
    }
}
