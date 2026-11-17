//! Integración con el sistema: argumentos de línea de órdenes, instancia única y registro como
//! gestor de archivos (escritorio).
#[allow(unused_imports)]
use crate::vfs::{KindV as _, PathV as _};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

/// Rutas pedidas en la línea de órdenes (ya absolutas).
static TARGETS: OnceLock<Vec<PathBuf>> = OnceLock::new();

pub fn initial_targets() -> Vec<PathBuf> {
    #[allow(unused_mut)]
    let mut v = TARGETS.get().cloned().unwrap_or_default();
    // macOS: lo que se abrió con la aplicación al lanzarla llega como evento, no como argumento
    #[cfg(target_os = "macos")]
    v.extend(mac_open::take_pending());
    v
}

/// Convierte `file:///a%20b`, `~/x` o rutas relativas en una ruta absoluta.
pub fn parse_target(arg: &str) -> Option<PathBuf> {
    parse_target_os(OsStr::new(arg))
}

/// Como `parse_target`, para un argumento que puede no ser UTF-8 (nombres en Latin-1, etc.).
pub fn parse_target_os(arg: &OsStr) -> Option<PathBuf> {
    let b = arg.as_encoded_bytes();
    let p = if let Some(rest) = b.strip_prefix(b"file://") {
        // file://host/ruta o file:///ruta
        let path = rest.iter().position(|&c| c == b'/').map(|i| &rest[i..]).unwrap_or(rest);
        bytes_to_path(pct_decode(path))
    } else {
        let p = Path::new(arg);
        // «~» y «~/x» (por componentes: «~pepe» no es la carpeta personal)
        match p.strip_prefix("~") {
            Ok(rest) if rest.as_os_str().is_empty() => dirs::home_dir()?,
            Ok(rest) => dirs::home_dir()?.join(rest),
            Err(_) => p.to_path_buf(),
        }
    };
    if p.as_os_str().is_empty() {
        return None;
    }
    let p = if p.is_absolute() { p } else { std::env::current_dir().ok()?.join(p) };
    Some(clean(&p))
}

#[cfg(unix)]
fn bytes_to_path(b: Vec<u8>) -> PathBuf {
    use std::os::unix::ffi::OsStringExt;
    PathBuf::from(std::ffi::OsString::from_vec(b))
}

#[cfg(not(unix))]
fn bytes_to_path(b: Vec<u8>) -> PathBuf {
    // los URI van en UTF-8 codificado con «%»
    PathBuf::from(String::from_utf8_lossy(&b).into_owned())
}

fn pct_decode(b: &[u8]) -> Vec<u8> {
    let hex = |c: u8| (c as char).to_digit(16).map(|v| v as u8);
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2])) {
                out.push(h << 4 | l);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    out
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
    // args_os: un argumento que no sea UTF-8 es una ruta válida, no motivo para abortar
    for a in std::env::args_os().skip(1) {
        match a.to_str() {
            Some("-h" | "--help") => {
                cli_print(HELP, false);
                return Cli::Exit(0);
            }
            Some("-V" | "--version") => {
                cli_print(&format!("filebrowser {}", env!("CARGO_PKG_VERSION")), false);
                return Cli::Exit(0);
            }
            Some("-n" | "--new-window") => new_window = true,
            Some(s @ ("--install" | "--uninstall")) => {
                let r = if s == "--install" { install() } else { uninstall() };
                return match r {
                    Ok(m) => {
                        cli_print(&m, false);
                        Cli::Exit(0)
                    }
                    Err(e) => {
                        cli_print(&e, true);
                        Cli::Exit(1)
                    }
                };
            }
            Some(s) if s.starts_with('-') && s.len() > 1 => {}
            _ => {
                if let Some(p) = parse_target_os(&a) {
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

/// Escribe la respuesta a una opción de la línea de órdenes. En Windows (versión final, subsistema
/// gráfico) el programa no tiene consola: se usa la de quien lo lanzó y, si no hay (doble clic,
/// acceso directo), un cuadro de mensaje.
fn cli_print(msg: &str, err: bool) {
    #[cfg(all(windows, not(debug_assertions)))]
    if !win_attach_console() {
        message_box(msg, err);
        return;
    }
    if err {
        eprintln!("{msg}");
    } else {
        println!("{msg}");
    }
}

#[cfg(all(windows, not(debug_assertions)))]
fn win_attach_console() -> bool {
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn AttachConsole(pid: u32) -> i32;
    }
    // SAFETY: sin precondiciones; u32::MAX es ATTACH_PARENT_PROCESS ((DWORD)-1).
    let ok = unsafe { AttachConsole(u32::MAX) } != 0;
    if ok {
        // el intérprete ya ha vuelto a mostrar su indicador: se empieza en línea nueva
        println!();
    }
    ok
}

/// Cuadro de mensaje modal sin ventana dueña (Windows): avisos cuando no hay consola ni ventana.
#[cfg(windows)]
pub fn message_box(msg: &str, err: bool) {
    use windows::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONERROR, MB_ICONINFORMATION, MB_OK, MB_SETFOREGROUND};
    let text: Vec<u16> = msg.encode_utf16().chain([0]).collect();
    let title: Vec<u16> = "File Browser".encode_utf16().chain([0]).collect();
    let icon = if err { MB_ICONERROR } else { MB_ICONINFORMATION };
    // SAFETY: cadenas UTF-16 terminadas en 0 que viven durante toda la llamada; sin ventana dueña.
    let _ = unsafe {
        MessageBoxW(None, windows::core::PCWSTR(text.as_ptr()), windows::core::PCWSTR(title.as_ptr()), MB_OK | icon | MB_SETFOREGROUND)
    };
}

/// Al salir: deja de anunciarse como la instancia en marcha.
pub fn shutdown() {
    #[cfg(all(unix, not(target_os = "android")))]
    if let Some(p) = OWN_SOCKET.get() {
        let _ = crate::vfs::fs::remove_file(p);
    }
    #[cfg(windows)]
    win_ipc::shutdown();
}

// ---------------------------------------------------------------------------
// Lectura acotada de los mensajes de otra instancia
// ---------------------------------------------------------------------------

/// Tamaño máximo de un mensaje (lista de rutas) de otra instancia.
#[cfg(any(windows, all(unix, not(target_os = "android")), test))]
const MAX_MSG: usize = 1 << 20;

/// Tiempo máximo para recibir un mensaje entero.
#[cfg(any(windows, all(unix, not(target_os = "android")), test))]
const MSG_TIME: std::time::Duration = std::time::Duration::from_secs(3);

#[cfg(any(windows, all(unix, not(target_os = "android")), test))]
trait Timed: std::io::Read {
    fn set_to(&self, d: std::time::Duration) -> std::io::Result<()>;
}

#[cfg(any(windows, all(unix, not(target_os = "android")), test))]
impl Timed for std::net::TcpStream {
    fn set_to(&self, d: std::time::Duration) -> std::io::Result<()> {
        self.set_read_timeout(Some(d))
    }
}

#[cfg(all(unix, not(target_os = "android")))]
impl Timed for std::os::unix::net::UnixStream {
    fn set_to(&self, d: std::time::Duration) -> std::io::Result<()> {
        self.set_read_timeout(Some(d))
    }
}

/// Lee hasta el final (o hasta un salto de línea si `line`) sin pasar de `cap` bytes ni de `end`.
/// `None` si se pasa, si caduca o si hay un error.
#[cfg(any(windows, all(unix, not(target_os = "android")), test))]
fn read_capped<S: Timed>(s: &mut S, cap: usize, end: std::time::Instant, line: bool) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let mut b = [0u8; 4096];
    loop {
        let left = end.checked_duration_since(std::time::Instant::now()).filter(|d| !d.is_zero())?;
        s.set_to(left).ok()?;
        let want = if line { 1 } else { b.len() };
        let n = match s.read(&mut b[..want]) {
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return None,
        };
        if n == 0 {
            return if line { None } else { Some(out) };
        }
        if line && b[0] == b'\n' {
            return Some(out);
        }
        if out.len().saturating_add(n) > cap {
            return None;
        }
        out.extend_from_slice(&b[..n]);
    }
}

// ---------------------------------------------------------------------------
// Instancia única (unix de escritorio)
// ---------------------------------------------------------------------------

/// Socket que hemos creado (se borra al salir).
#[cfg(all(unix, not(target_os = "android")))]
static OWN_SOCKET: OnceLock<PathBuf> = OnceLock::new();

/// ¿Es `d` una carpeta (no un enlace) de `uid` a la que nadie más puede entrar?
#[cfg(all(unix, not(target_os = "android")))]
fn private_to(d: &Path, uid: u32) -> bool {
    use std::os::unix::fs::MetadataExt;
    std::fs::symlink_metadata(d).map(|m| m.is_dir() && m.uid() == uid && m.mode() & 0o077 == 0).unwrap_or(false)
}

/// Carpeta privada para el socket: XDG_RUNTIME_DIR o, si no hay, `<temp>/filebrowser-rs-<uid>` con
/// permisos 0700 (en /tmp otro usuario podría haber creado antes el socket o la carpeta).
#[cfg(all(unix, not(target_os = "android")))]
fn socket_dir_in(runtime: Option<PathBuf>, tmp: &Path, uid: u32) -> Option<PathBuf> {
    if let Some(d) = runtime.filter(|d| private_to(d, uid)) {
        return Some(d);
    }
    use std::os::unix::fs::DirBuilderExt;
    let d = tmp.join(format!("filebrowser-rs-{uid}"));
    let _ = std::fs::DirBuilder::new().mode(0o700).create(&d);
    private_to(&d, uid).then_some(d)
}

#[cfg(all(unix, not(target_os = "android")))]
fn socket_path() -> Option<PathBuf> {
    // SAFETY: getuid no tiene precondiciones y no falla.
    let uid = unsafe { libc::getuid() };
    let dir = socket_dir_in(std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from), &crate::vfs::temp_dir(), uid)?;
    Some(dir.join(format!("filebrowser-rs-{uid}.sock")))
}

/// Usuario del otro extremo del socket.
#[cfg(all(unix, not(target_os = "android")))]
fn peer_uid(s: &std::os::unix::net::UnixStream) -> Option<u32> {
    use std::os::fd::AsRawFd;
    let fd = s.as_raw_fd();
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        // SAFETY: ucred es una estructura C sin invariantes (todo ceros es válido); getsockopt escribe
        // como mucho `len` bytes en ella y `fd` es un socket abierto mientras dure `s`.
        let mut c: libc::ucred = unsafe { std::mem::zeroed() };
        let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
        let r = unsafe { libc::getsockopt(fd, libc::SOL_SOCKET, libc::SO_PEERCRED, (&mut c as *mut libc::ucred).cast(), &mut len) };
        (r == 0).then_some(c.uid)
    }
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    {
        let (mut uid, mut gid) = (0, 0);
        // SAFETY: `fd` es un socket abierto mientras dure `s`; los punteros son a variables locales.
        let r = unsafe { libc::getpeereid(fd, &mut uid, &mut gid) };
        (r == 0).then_some(uid)
    }
}

#[cfg(all(unix, not(target_os = "android")))]
fn forward(targets: &[PathBuf]) -> bool {
    use std::io::Write;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::net::UnixStream;
    let Some(path) = socket_path() else { return false };
    let Ok(mut s) = UnixStream::connect(path) else { return false };
    let _ = s.set_write_timeout(Some(MSG_TIME));
    let mut buf = Vec::new();
    for t in targets {
        buf.extend_from_slice(t.as_os_str().as_bytes());
        buf.push(b'\n');
    }
    // token de activación de quien nos lanzó (Wayland; DESKTOP_STARTUP_ID en X11): con él la
    // instancia en marcha puede ponerse delante
    let token = std::env::var("XDG_ACTIVATION_TOKEN").or_else(|_| std::env::var("DESKTOP_STARTUP_ID")).ok();
    if let (Some(tok), Some(last)) = (token.filter(|t| valid_token(t)), targets.last()) {
        let dir = if last.v_is_dir() { last.as_path() } else { last.parent().unwrap_or(last) };
        buf.extend_from_slice(&token_line(dir, &tok));
    }
    s.write_all(&buf).is_ok()
}

/// Marca de la línea con el token de activación. Una ruta no puede tener NUL, así que no se
/// confunde con ninguna; la línea es `<carpeta del último destino>/\0fb-activation-token\0<token en hex>`
/// para que una versión anterior la tome por un archivo inexistente de una carpeta que ya iba a
/// abrir (no selecciona nada ni abre otra pestaña).
#[cfg(all(unix, not(target_os = "android")))]
const TOKEN_MARK: &[u8] = b"\0fb-activation-token\0";

#[cfg(all(unix, not(target_os = "android")))]
fn valid_token(t: &str) -> bool {
    !t.is_empty() && t.len() <= 4096 && !t.bytes().any(|b| b == b'\n' || b == 0)
}

#[cfg(all(unix, not(target_os = "android")))]
fn token_line(dir: &Path, tok: &str) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt;
    let mut l = dir.as_os_str().as_bytes().to_vec();
    if !l.ends_with(b"/") {
        l.push(b'/');
    }
    l.extend_from_slice(TOKEN_MARK);
    // en hexadecimal: el token puede llevar «/» y la línea dejaría de ser un nombre dentro de `dir`
    for b in tok.bytes() {
        l.extend_from_slice(format!("{b:02x}").as_bytes());
    }
    l.push(b'\n');
    l
}

/// Rutas de un mensaje: una por línea (las líneas con NUL son datos, no rutas).
#[cfg(all(unix, not(target_os = "android")))]
fn split_paths(buf: &[u8]) -> Vec<PathBuf> {
    use std::os::unix::ffi::OsStrExt;
    buf.split(|&b| b == b'\n').filter(|l| !l.is_empty() && !l.contains(&0)).map(|l| PathBuf::from(OsStr::from_bytes(l))).collect()
}

/// Token de activación que viene en un mensaje, si lo hay.
#[cfg(all(unix, not(target_os = "android")))]
fn msg_token(buf: &[u8]) -> Option<String> {
    buf.split(|&b| b == b'\n').find_map(|l| {
        let at = l.windows(TOKEN_MARK.len()).position(|w| w == TOKEN_MARK)?;
        let hex = &l[at + TOKEN_MARK.len()..];
        if hex.len() % 2 != 0 {
            return None;
        }
        let bytes = hex
            .chunks(2)
            .map(|c| std::str::from_utf8(c).ok().and_then(|c| u8::from_str_radix(c, 16).ok()))
            .collect::<Option<Vec<u8>>>()?;
        let t = String::from_utf8(bytes).ok()?;
        valid_token(&t).then_some(t)
    })
}

/// Último token de activación recibido de otra invocación (o por D-Bus), para ponerse delante.
static ACTIVATION: Mutex<Option<String>> = Mutex::new(None);

#[allow(dead_code)]
fn set_activation_token(t: Option<String>) {
    if let Some(t) = t {
        *crate::thumbs::lock(&ACTIVATION) = Some(t);
    }
}

/// Saca el token de activación pendiente (se usa una sola vez).
pub fn take_activation_token() -> Option<String> {
    crate::thumbs::lock(&ACTIVATION).take()
}

// ---------------------------------------------------------------------------
// Instancia única (Windows): TCP en 127.0.0.1 con desafío-respuesta
// ---------------------------------------------------------------------------

/// Protocolo entre instancias por TCP local. El puerto y un secreto están en un archivo de la
/// carpeta del usuario; el secreto nunca viaja: cada lado demuestra conocerlo con
/// SHA-256(secreto, papel, reto del otro) antes de que se envíen o acepten rutas.
///
/// 1. cliente → `reto_c`            2. servidor → `reto_s prueba(s, reto_c)`
/// 3. cliente → `prueba(c, reto_s)` y las rutas, y cierra su lado     4. servidor → `ok`
#[cfg(any(windows, test))]
mod ipc {
    use super::*;
    use std::io::Write;
    use std::net::TcpStream;
    use std::time::Instant;

    pub fn random_hex() -> String {
        use std::hash::{BuildHasher, Hasher};
        // RandomState toma sus claves del generador del sistema
        let a = std::collections::hash_map::RandomState::new().build_hasher().finish();
        let b = std::collections::hash_map::RandomState::new().build_hasher().finish();
        format!("{a:016x}{b:016x}")
    }

    fn proof(token: &str, role: &str, nonce: &str) -> String {
        use sha2::Digest as _;
        let mut h = sha2::Sha256::new();
        for part in [token, role, nonce] {
            h.update(part.as_bytes());
            h.update([0u8]);
        }
        h.finalize().iter().map(|b| format!("{b:02x}")).collect()
    }

    fn line(s: &mut TcpStream, end: Instant) -> Option<String> {
        String::from_utf8(read_capped(s, 256, end, true)?).ok()
    }

    /// Envía `payload` a la instancia que escucha en `s`. `true` si la confirmó.
    pub fn client(s: &mut TcpStream, token: &str, payload: &[u8]) -> bool {
        let end = Instant::now() + MSG_TIME;
        let _ = s.set_write_timeout(Some(MSG_TIME));
        let nc = random_hex();
        if s.write_all(format!("{nc}\n").as_bytes()).is_err() {
            return false;
        }
        let Some(l) = line(s, end) else { return false };
        let Some((ns, p)) = l.split_once(' ') else { return false };
        // si quien escucha en el puerto no conoce el secreto, no recibe las rutas
        if p != proof(token, "s", &nc) || ns.len() != 32 {
            return false;
        }
        let mut msg = format!("{}\n", proof(token, "c", ns)).into_bytes();
        msg.extend_from_slice(payload);
        if s.write_all(&msg).is_err() || s.shutdown(std::net::Shutdown::Write).is_err() {
            return false;
        }
        line(s, end).as_deref() == Some("ok")
    }

    /// Atiende a una instancia que se conecta. Devuelve su mensaje si demostró conocer el secreto.
    pub fn server(s: &mut TcpStream, token: &str) -> Option<Vec<u8>> {
        let end = Instant::now() + MSG_TIME;
        let _ = s.set_write_timeout(Some(MSG_TIME));
        let nc = line(s, end)?;
        if nc.len() != 32 {
            return None;
        }
        let ns = random_hex();
        s.write_all(format!("{ns} {}\n", proof(token, "s", &nc)).as_bytes()).ok()?;
        if line(s, end)? != proof(token, "c", &ns) {
            return None;
        }
        let msg = read_capped(s, MAX_MSG, end, false)?;
        let _ = s.write_all(b"ok\n");
        Some(msg)
    }

    /// Rutas como UTF-16 en hexadecimal, una por línea: sin pérdidas aunque no sean Unicode válido.
    pub fn encode(targets: &[PathBuf]) -> Vec<u8> {
        let mut out = String::new();
        for t in targets {
            for u in os_units(t.as_os_str()) {
                out.push_str(&format!("{u:04x}"));
            }
            out.push('\n');
        }
        out.into_bytes()
    }

    pub fn decode(buf: &[u8]) -> Vec<PathBuf> {
        let txt = String::from_utf8_lossy(buf);
        txt.lines()
            .filter(|l| !l.is_empty() && l.len() % 4 == 0)
            .filter_map(|l| {
                let units: Option<Vec<u16>> = (0..l.len()).step_by(4).map(|i| l.get(i..i + 4).and_then(|h| u16::from_str_radix(h, 16).ok())).collect();
                units.map(|u| PathBuf::from(os_from_units(&u)))
            })
            .collect()
    }

    #[cfg(windows)]
    fn os_units(s: &OsStr) -> Vec<u16> {
        use std::os::windows::ffi::OsStrExt;
        s.encode_wide().collect()
    }
    #[cfg(windows)]
    fn os_from_units(u: &[u16]) -> std::ffi::OsString {
        use std::os::windows::ffi::OsStringExt;
        std::ffi::OsString::from_wide(u)
    }
    #[cfg(not(windows))]
    fn os_units(s: &OsStr) -> Vec<u16> {
        s.to_string_lossy().encode_utf16().collect()
    }
    #[cfg(not(windows))]
    fn os_from_units(u: &[u16]) -> std::ffi::OsString {
        std::ffi::OsString::from(String::from_utf16_lossy(u))
    }
}

#[cfg(windows)]
mod win_ipc {
    use super::*;
    use std::net::{TcpListener, TcpStream};
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Contenido que escribimos en el archivo de estado (para borrarlo sólo si sigue siendo nuestro).
    static OWN: OnceLock<(PathBuf, String)> = OnceLock::new();

    fn state_file() -> Option<PathBuf> {
        dirs::data_local_dir().map(|d| d.join("filebrowser-rs").join("instance"))
    }

    fn read_state() -> Option<(u16, String)> {
        let txt = crate::vfs::fs::read_to_string(state_file()?).ok()?;
        let mut it = txt.split_whitespace();
        Some((it.next()?.parse().ok()?, it.next()?.to_string()))
    }

    fn connect(port: u16, ms: u64) -> Option<TcpStream> {
        TcpStream::connect_timeout(&std::net::SocketAddr::from(([127, 0, 0, 1], port)), std::time::Duration::from_millis(ms)).ok()
    }

    pub fn forward(targets: &[PathBuf]) -> bool {
        let Some((port, token)) = read_state() else { return false };
        let Some(mut s) = connect(port, 500) else { return false };
        ipc::client(&mut s, &token, &ipc::encode(targets))
    }

    pub fn listen(wake: impl Fn() + Send + Sync + 'static) -> Inbox {
        let inbox: Inbox = Arc::new(Mutex::new(Vec::new()));
        let Some(file) = state_file() else { return inbox };
        // ¿otra instancia viva? (que además conozca el secreto: un puerto reutilizado no cuenta)
        if let Some((port, token)) = read_state() {
            if let Some(mut s) = connect(port, 300) {
                if ipc::client(&mut s, &token, b"") {
                    return inbox;
                }
            }
        }
        let Ok(l) = TcpListener::bind(("127.0.0.1", 0)) else { return inbox };
        let Ok(addr) = l.local_addr() else { return inbox };
        let token = ipc::random_hex();
        if let Some(d) = file.parent() {
            let _ = crate::vfs::fs::create_dir_all(d);
        }
        let txt = format!("{} {}", addr.port(), token);
        if crate::vfs::fs::write(&file, &txt).is_ok() {
            let _ = OWN.set((file, txt));
        }
        let q = inbox.clone();
        let wake = Arc::new(wake);
        let token = Arc::new(token);
        crate::vfs::thread::spawn(move || {
            static BUSY: AtomicUsize = AtomicUsize::new(0);
            for c in l.incoming() {
                let Ok(mut c) = c else { continue };
                // un hilo por conexión (una lenta no bloquea a las demás), con un máximo
                if BUSY.fetch_add(1, Ordering::SeqCst) >= 8 {
                    BUSY.fetch_sub(1, Ordering::SeqCst);
                    continue;
                }
                let (q, wake, token) = (q.clone(), wake.clone(), token.clone());
                crate::vfs::thread::spawn(move || {
                    if let Some(msg) = ipc::server(&mut c, &token) {
                        let paths = ipc::decode(&msg);
                        if !paths.is_empty() {
                            q.lock().unwrap().extend(paths);
                            wake();
                        }
                    }
                    BUSY.fetch_sub(1, Ordering::SeqCst);
                });
            }
        });
        inbox
    }

    pub fn shutdown() {
        if let Some((f, txt)) = OWN.get() {
            if crate::vfs::fs::read_to_string(f).ok().as_deref() == Some(txt.as_str()) {
                let _ = crate::vfs::fs::remove_file(f);
            }
        }
    }
}

#[cfg(windows)]
fn forward(targets: &[PathBuf]) -> bool {
    win_ipc::forward(targets)
}

#[cfg(not(any(windows, all(unix, not(target_os = "android")))))]
fn forward(_: &[PathBuf]) -> bool {
    false
}

/// Cola de rutas que llegan desde otras invocaciones del programa.
pub type Inbox = Arc<Mutex<Vec<PathBuf>>>;

/// Empieza a escuchar a otras invocaciones; `wake` despierta a la interfaz.
#[cfg(all(unix, not(target_os = "android")))]
pub fn listen(wake: impl Fn() + Send + Sync + 'static) -> Inbox {
    use std::os::unix::net::{UnixListener, UnixStream};
    let wake = Arc::new(wake);
    let inbox: Inbox = Arc::new(Mutex::new(Vec::new()));
    // «Mostrar en la carpeta» por D-Bus, aunque otra instancia tenga el socket (puede no haberlo
    // registrado: otra sesión de D-Bus, versión antigua…); si el nombre ya tiene dueño, no hace nada
    #[cfg(target_os = "linux")]
    {
        let (q, w) = (inbox.clone(), wake.clone());
        crate::vfs::thread::spawn(move || dbus_service(q, w));
    }
    let Some(path) = socket_path() else { return inbox };
    // Otra instancia viva conserva el socket; si está muerto, se sustituye.
    if UnixStream::connect(&path).is_ok() {
        return inbox;
    }
    let _ = crate::vfs::fs::remove_file(&path);
    let Ok(l) = UnixListener::bind(&path) else { return inbox };
    let _ = OWN_SOCKET.set(path);
    let q = inbox.clone();
    crate::vfs::thread::spawn(move || {
        // SAFETY: getuid no tiene precondiciones y no falla.
        let me = unsafe { libc::getuid() };
        for c in l.incoming() {
            let Ok(mut c) = c else { continue };
            // sólo el mismo usuario puede pedirnos que abramos rutas
            if peer_uid(&c) != Some(me) {
                continue;
            }
            let Some(buf) = read_capped(&mut c, MAX_MSG, std::time::Instant::now() + MSG_TIME, false) else { continue };
            let paths = split_paths(&buf);
            if paths.is_empty() {
                continue;
            }
            // el token antes que las rutas: drain_inbox lo recoge al ver rutas
            set_activation_token(msg_token(&buf));
            q.lock().unwrap().extend(paths);
            wake();
        }
    });
    inbox
}
/// `org.freedesktop.FileManager1`: es lo que usan navegadores, mensajería y demás para «Mostrar en la carpeta».
#[cfg(target_os = "linux")]
fn dbus_service(inbox: Inbox, wake: Arc<dyn Fn() + Send + Sync>) {
    struct Fm(Inbox, Arc<dyn Fn() + Send + Sync>);
    impl Fm {
        fn push(&self, uris: Vec<String>, startup_id: String) {
            // el startup_id de FileManager1 es el token de activación de quien llama
            set_activation_token(Some(startup_id).filter(|t| valid_token(t)));
            let mut g = self.0.lock().unwrap();
            g.extend(uris.iter().filter_map(|u| parse_target(u)));
            drop(g);
            (self.1)();
        }
    }
    #[zbus::interface(name = "org.freedesktop.FileManager1")]
    impl Fm {
        fn show_folders(&self, uris: Vec<String>, startup_id: String) {
            self.push(uris, startup_id);
        }
        fn show_items(&self, uris: Vec<String>, startup_id: String) {
            self.push(uris, startup_id);
        }
        fn show_item_properties(&self, uris: Vec<String>, startup_id: String) {
            self.push(uris, startup_id);
        }
    }
    let Ok(conn) = zbus::blocking::connection::Builder::session()
        .and_then(|b| b.name("org.freedesktop.FileManager1"))
        .and_then(|b| b.serve_at("/org/freedesktop/FileManager1", Fm(inbox, wake)))
        .and_then(|b| b.build())
    else {
        return;
    };
    // la conexión debe seguir viva mientras dure el proceso
    std::mem::forget(conn);
}

#[cfg(windows)]
pub fn listen(wake: impl Fn() + Send + Sync + 'static) -> Inbox {
    win_ipc::listen(wake)
}

#[cfg(not(any(windows, all(unix, not(target_os = "android")))))]
pub fn listen(_wake: impl Fn() + Send + 'static) -> Inbox {
    Arc::new(Mutex::new(Vec::new()))
}

// ---------------------------------------------------------------------------
// macOS: «Abrir con» / soltar sobre el icono llegan como eventos de Apple, no como argumentos
// ---------------------------------------------------------------------------

#[cfg(target_os = "macos")]
mod mac_open {
    use super::*;
    use objc2::ffi::class_addMethod;
    use objc2::runtime::{AnyClass, AnyObject, Sel};
    use objc2::{msg_send, sel};
    use std::ffi::{c_char, CStr};

    /// Rutas llegadas antes de que la app tenga cola (las del arranque: llegan antes de
    /// applicationDidFinishLaunching, y la app se crea dentro de éste).
    static PENDING: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());
    static STATE: OnceLock<(Inbox, Box<dyn Fn() + Send + Sync>)> = OnceLock::new();

    /// # Safety
    /// `s` debe ser nulo o un NSString vivo.
    unsafe fn push(s: *mut AnyObject) {
        if s.is_null() {
            return;
        }
        // SAFETY: `s` es un NSString (precondición); UTF8String devuelve una cadena C que vive mientras el
        // objeto (o el autorelease pool en curso) y se copia enseguida.
        let c: *const c_char = unsafe { msg_send![s, UTF8String] };
        if c.is_null() {
            return;
        }
        let p = PathBuf::from(unsafe { CStr::from_ptr(c) }.to_string_lossy().into_owned());
        match STATE.get() {
            Some((q, wake)) => {
                q.lock().unwrap().push(p);
                wake();
            }
            None => PENDING.lock().unwrap().push(p),
        }
    }

    extern "C" fn open_files(_this: *mut AnyObject, _cmd: Sel, _app: *mut AnyObject, files: *mut AnyObject) {
        // SAFETY: AppKit llama con `files` = NSArray<NSString> (application:openFiles:); índices < count.
        unsafe {
            let n: usize = msg_send![files, count];
            for i in 0..n {
                let s: *mut AnyObject = msg_send![files, objectAtIndex: i];
                push(s);
            }
        }
    }

    extern "C" fn open_urls(_this: *mut AnyObject, _cmd: Sel, _app: *mut AnyObject, urls: *mut AnyObject) {
        // SAFETY: AppKit llama con `urls` = NSArray<NSURL> (application:openURLs:); índices < count; `path`
        // devuelve un NSString o nil (que `push` descarta).
        unsafe {
            let n: usize = msg_send![urls, count];
            for i in 0..n {
                let u: *mut AnyObject = msg_send![urls, objectAtIndex: i];
                let s: *mut AnyObject = msg_send![u, path];
                push(s);
            }
        }
    }

    /// Antes de crear el bucle de eventos: añade `application:openFiles:` y `application:openURLs:` a
    /// NSObject. La clase del delegado de winit aún no existe (se registra al crear el bucle) y los
    /// eventos del arranque llegan antes de que la app exista; como el delegado hereda de NSObject,
    /// AppKit ve que responde a ellos desde el principio. Ninguna otra clase recibe esos mensajes.
    pub fn install() {
        let Some(cls) = AnyClass::get("NSObject") else { return };
        let cls = cls as *const AnyClass as *mut AnyClass;
        type Imp = unsafe extern "C" fn();
        // SAFETY: NSObject existe (se acaba de buscar); las funciones tienen la firma que declara la
        // cadena de tipos «v@:@@» (void, self, _cmd, dos objetos); transmutar a `Imp` es como
        // class_addMethod espera recibir cualquier IMP. Si el método ya existiera no se toca.
        unsafe {
            let f: extern "C" fn(*mut AnyObject, Sel, *mut AnyObject, *mut AnyObject) = open_files;
            class_addMethod(cls.cast(), sel!(application:openFiles:).as_ptr(), Some(std::mem::transmute::<_, Imp>(f)), c"v@:@@".as_ptr());
            let g: extern "C" fn(*mut AnyObject, Sel, *mut AnyObject, *mut AnyObject) = open_urls;
            class_addMethod(cls.cast(), sel!(application:openURLs:).as_ptr(), Some(std::mem::transmute::<_, Imp>(g)), c"v@:@@".as_ptr());
        }
    }

    pub fn take_pending() -> Vec<PathBuf> {
        std::mem::take(&mut *PENDING.lock().unwrap())
    }

    pub fn connect(inbox: Inbox, wake: Box<dyn Fn() + Send + Sync>) {
        if STATE.set((inbox, wake)).is_err() {
            return;
        }
        // lo que llegara entre `initial_targets` y aquí
        let late = take_pending();
        if !late.is_empty() {
            if let Some((q, wake)) = STATE.get() {
                q.lock().unwrap().extend(late);
                wake();
            }
        }
    }
}

/// macOS: prepara la recepción de «Abrir con» / soltar sobre el icono. Va antes de `run_native`.
#[cfg(target_os = "macos")]
pub fn install_open_files() {
    mac_open::install();
}

/// macOS: desde ahora las carpetas y archivos que macOS abra con la aplicación van a `inbox`.
#[cfg(target_os = "macos")]
pub fn hook_open_files(inbox: Inbox, wake: impl Fn() + Send + Sync + 'static) {
    mac_open::connect(inbox, Box::new(wake));
}

// ---------------------------------------------------------------------------
// Registro como gestor de archivos
// ---------------------------------------------------------------------------

/// ¿Corremos dentro de Flatpak? Ahí el ejecutable no es una ruta del sistema: el registro lo
/// hace el propio Flatpak con el .desktop de la aplicación.
#[cfg(target_os = "linux")]
fn in_flatpak() -> bool {
    std::env::var_os("FLATPAK_ID").is_some() || Path::new("/.flatpak-info").exists()
}

/// ¿Hay un mecanismo de registro en esta plataforma?
pub fn can_install() -> bool {
    #[cfg(target_os = "linux")]
    return !in_flatpak();
    #[cfg(not(target_os = "linux"))]
    return cfg!(windows);
}

/// Primera línea de los archivos de D-Bus que escribimos: sólo tocamos los que la llevan.
#[cfg(any(target_os = "linux", test))]
const DBUS_MARK: &str = "# filebrowser-rs";

/// Ruta entre comillas para la clave `Exec` de un .desktop: dentro de las comillas se escapan
/// `"` `` ` `` `$` `\`, el `%` se dobla (es un código de campo) y, por último, el escape de cadena
/// del formato (`\` → `\\`).
#[cfg(any(target_os = "linux", test))]
fn desktop_exec_quote(s: &str) -> String {
    let mut q = String::from("\"");
    for c in s.chars() {
        match c {
            '"' | '`' | '$' | '\\' => {
                q.push('\\');
                q.push(c);
            }
            '%' => q.push_str("%%"),
            _ => q.push(c),
        }
    }
    q.push('"');
    q.replace('\\', "\\\\")
}

/// Ruta entre comillas para el `Exec` de un .service de D-Bus (reglas de la shell, sin `%`).
#[cfg(any(target_os = "linux", test))]
fn dbus_exec_quote(s: &str) -> String {
    let mut q = String::from("\"");
    for c in s.chars() {
        if matches!(c, '"' | '`' | '$' | '\\') {
            q.push('\\');
        }
        q.push(c);
    }
    q.push('"');
    q
}

/// ¿Es nuestro este .service? (los de versiones anteriores no llevaban la marca pero sí el ejecutable)
#[cfg(any(target_os = "linux", test))]
fn dbus_is_ours(txt: &str) -> bool {
    txt.starts_with(DBUS_MARK) || txt.lines().any(|l| l.starts_with("Exec=") && l.contains("filebrowser"))
}

/// Quita `filebrowser-rs.desktop` de `inode/directory` en un mimeapps.list. `None` si no estaba.
#[cfg(any(target_os = "linux", test))]
fn strip_mime_default(txt: &str) -> Option<String> {
    let mut changed = false;
    let mut out = String::with_capacity(txt.len());
    let mut section = String::new();
    for l in txt.split_inclusive('\n') {
        let body = l.trim_end_matches(['\n', '\r']);
        let t = body.trim();
        if t.starts_with('[') {
            section = t.to_string();
        }
        let ours_section = section == "[Default Applications]" || section == "[Added Associations]";
        if let Some(v) = t.strip_prefix("inode/directory").map(str::trim_start).and_then(|r| r.strip_prefix('=')).filter(|_| ours_section) {
            let rest: Vec<&str> = v.split(';').map(str::trim).filter(|a| !a.is_empty()).collect();
            if rest.contains(&"filebrowser-rs.desktop") {
                changed = true;
                let rest: Vec<&str> = rest.into_iter().filter(|a| *a != "filebrowser-rs.desktop").collect();
                if !rest.is_empty() {
                    out.push_str(&format!("inode/directory={};", rest.join(";")));
                    out.push_str(&l[body.len()..]);
                }
                continue;
            }
        }
        out.push_str(l);
    }
    changed.then_some(out)
}

#[cfg(target_os = "linux")]
pub fn install() -> Result<String, String> {
    if in_flatpak() {
        return Err(crate::i18n::t("Dentro de Flatpak el registro lo hace el propio Flatpak").into());
    }
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let exe = exe.to_str().filter(|s| !s.contains(['\n', '\r'])).ok_or(crate::i18n::t("la ruta del programa no se puede escribir en un archivo .desktop"))?.to_string();
    let data = dirs::data_dir().ok_or(crate::i18n::t("sin carpeta de datos"))?;
    let apps = data.join("applications");
    let icons = data.join("icons/hicolor/256x256/apps");
    crate::vfs::fs::create_dir_all(&apps).map_err(|e| e.to_string())?;
    crate::vfs::fs::create_dir_all(&icons).map_err(|e| e.to_string())?;
    crate::vfs::fs::write(icons.join("filebrowser-rs.png"), include_bytes!("../assets/icon-256.png")).map_err(|e| e.to_string())?;
    let exec = desktop_exec_quote(&exe);
    let desktop = format!(
        "[Desktop Entry]\nType=Application\nName=File Browser\nComment=Fast, lightweight file manager\n\
         Exec={exec} %U\nIcon=filebrowser-rs\nTerminal=false\nCategories=System;FileTools;FileManager;\n\
         MimeType=inode/directory;\nStartupWMClass=filebrowser-rs\nKeywords=files;folders;explorer;\n"
    );
    let f = apps.join("filebrowser-rs.desktop");
    crate::vfs::fs::write(&f, desktop).map_err(|e| e.to_string())?;
    let run = |prog: &str, args: &[&str]| {
        std::process::Command::new(prog)
            .args(args)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    };
    // «Mostrar en la carpeta» de otras aplicaciones (D-Bus FileManager1); el de otro gestor de
    // archivos no se pisa
    let svc = data.join("dbus-1/services");
    let svc_file = svc.join("org.freedesktop.FileManager1.service");
    let mut note = "";
    match crate::vfs::fs::read_to_string(&svc_file) {
        Ok(t) if !dbus_is_ours(&t) => note = "; another file manager owns the D-Bus FileManager1 service file, left untouched",
        _ => {
            if crate::vfs::fs::create_dir_all(&svc).is_ok() {
                let _ = crate::vfs::fs::write(&svc_file, format!("{DBUS_MARK}\n[D-BUS Service]\nName=org.freedesktop.FileManager1\nExec={}\n", dbus_exec_quote(&exe)));
            }
        }
    }
    run("update-desktop-database", &[&apps.display().to_string()]);
    run("gtk-update-icon-cache", &["-q", "-t", &data.join("icons/hicolor").display().to_string()]);
    let ok = run("xdg-mime", &["default", "filebrowser-rs.desktop", "inode/directory"]);
    Ok(format!(
        "Installed {}{}{note}",
        f.display(),
        if ok { "; set as the default application for folders" } else { " (xdg-mime not found: set the default for folders manually)" }
    ))
}

#[cfg(target_os = "linux")]
pub fn uninstall() -> Result<String, String> {
    let data = dirs::data_dir().ok_or(crate::i18n::t("sin carpeta de datos"))?;
    let mut n = 0;
    for f in [data.join("applications/filebrowser-rs.desktop"), data.join("icons/hicolor/256x256/apps/filebrowser-rs.png")] {
        if crate::vfs::fs::remove_file(&f).is_ok() {
            n += 1;
        }
    }
    let svc = data.join("dbus-1/services/org.freedesktop.FileManager1.service");
    if crate::vfs::fs::read_to_string(&svc).is_ok_and(|t| dbus_is_ours(&t)) && crate::vfs::fs::remove_file(&svc).is_ok() {
        n += 1;
    }
    // ya no somos la aplicación para carpetas
    if let Some(m) = dirs::config_dir().map(|d| d.join("mimeapps.list")) {
        if let Some(t) = crate::vfs::fs::read_to_string(&m).ok().and_then(|t| strip_mime_default(&t)) {
            if crate::vfs::fs::write(&m, t).is_ok() {
                n += 1;
            }
        }
    }
    let _ = std::process::Command::new("update-desktop-database").arg(data.join("applications")).status();
    Ok(format!("Removed {n} file(s) or entries"))
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
    Err(crate::i18n::t("No disponible en esta plataforma: usa el paquete de la aplicación").into())
}

#[cfg(not(any(target_os = "linux", windows)))]
pub fn uninstall() -> Result<String, String> {
    Err(crate::i18n::t("No disponible en esta plataforma").into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn targets() {
        assert_eq!(parse_target("file:///tmp/a%20b"), Some(PathBuf::from("/tmp/a b")));
        assert_eq!(parse_target("/tmp/x/../y/./z"), Some(PathBuf::from("/tmp/y/z")));
        assert!(parse_target("").is_none());
        assert_eq!(pct_decode(b"100%"), b"100%");
        assert_eq!(pct_decode(b"%zz%41"), b"%zzA");
        assert_eq!(pct_decode(b"%4"), b"%4");
    }

    #[cfg(unix)]
    #[test]
    fn targets_not_utf8() {
        use std::os::unix::ffi::{OsStrExt, OsStringExt};
        let raw = OsStr::from_bytes(b"/tmp/caf\xe9/x");
        assert_eq!(parse_target_os(raw), Some(PathBuf::from(raw)));
        // el URI conserva los bytes tal cual (no se cambian por U+FFFD)
        let want = PathBuf::from(std::ffi::OsString::from_vec(b"/tmp/caf\xe9".to_vec()));
        assert_eq!(parse_target_os(OsStr::new("file:///tmp/caf%E9")), Some(want));
        // «~pepe» no es la carpeta personal
        let p = parse_target_os(OsStr::new("~pepe")).unwrap();
        assert!(p.ends_with("~pepe"));
    }

    #[test]
    fn exec_quoting() {
        assert_eq!(desktop_exec_quote("/usr/bin/fb"), "\"/usr/bin/fb\"");
        assert_eq!(desktop_exec_quote("/opt/a b/fb"), "\"/opt/a b/fb\"");
        // `$` y `` ` `` se escapan dentro de las comillas; el `%` se dobla
        assert_eq!(desktop_exec_quote("/x/$HOME/`id`/100%"), "\"/x/\\\\$HOME/\\\\`id\\\\`/100%%\"");
        // `\` y `"`: escape de comillas y luego escape de cadena
        assert_eq!(desktop_exec_quote("/x\\y\"z"), "\"/x\\\\\\\\y\\\\\"z\"");
        assert_eq!(dbus_exec_quote("/x/$a `b` \"c\" \\d"), "\"/x/\\$a \\`b\\` \\\"c\\\" \\\\d\"");
        assert!(dbus_is_ours(&format!("{DBUS_MARK}\n[D-BUS Service]\n")));
        assert!(dbus_is_ours("[D-BUS Service]\nName=org.freedesktop.FileManager1\nExec=\"/home/u/bin/filebrowser\"\n"));
        assert!(!dbus_is_ours("[D-BUS Service]\nName=org.freedesktop.FileManager1\nExec=/usr/bin/nautilus --gapplication-service\n"));
    }

    #[test]
    fn mimeapps() {
        let t = "[Default Applications]\ntext/plain=gedit.desktop\ninode/directory=filebrowser-rs.desktop\n\n[Added Associations]\ninode/directory=filebrowser-rs.desktop;org.gnome.Nautilus.desktop;\n";
        let out = strip_mime_default(t).unwrap();
        assert_eq!(out, "[Default Applications]\ntext/plain=gedit.desktop\n\n[Added Associations]\ninode/directory=org.gnome.Nautilus.desktop;\n");
        assert!(strip_mime_default(&out).is_none());
        // otras secciones no se tocan
        assert!(strip_mime_default("[Removed Associations]\ninode/directory=filebrowser-rs.desktop\n").is_none());
    }

    #[test]
    fn ipc_handshake() {
        use std::net::{TcpListener, TcpStream};
        let l = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = l.local_addr().unwrap().port();
        let srv = std::thread::spawn(move || {
            let mut got = Vec::new();
            for _ in 0..2 {
                let (mut c, _) = l.accept().unwrap();
                got.push(ipc::server(&mut c, "secreto"));
            }
            got
        });
        let paths = vec![PathBuf::from("/tmp/a b"), PathBuf::from("/tmp/ñ")];
        let mut c = TcpStream::connect(("127.0.0.1", port)).unwrap();
        assert!(ipc::client(&mut c, "secreto", &ipc::encode(&paths)));
        // con otro secreto no se envía nada ni se acepta
        let mut c = TcpStream::connect(("127.0.0.1", port)).unwrap();
        assert!(!ipc::client(&mut c, "otro", &ipc::encode(&paths)));
        let got = srv.join().unwrap();
        assert_eq!(ipc::decode(got[0].as_ref().unwrap()), paths);
        assert!(got[1].is_none());
    }

    #[test]
    fn ipc_capped() {
        use std::io::Write;
        use std::net::{TcpListener, TcpStream};
        let l = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = l.local_addr().unwrap().port();
        let t = std::thread::spawn(move || {
            let mut c = TcpStream::connect(("127.0.0.1", port)).unwrap();
            let _ = c.write_all(&vec![b'a'; 300]);
            // sin cerrar: el servidor no debe quedarse esperando para siempre
            std::thread::sleep(std::time::Duration::from_millis(600));
        });
        let (mut s, _) = l.accept().unwrap();
        let t0 = std::time::Instant::now();
        assert!(read_capped(&mut s, 100, t0 + std::time::Duration::from_secs(5), false).is_none());
        t.join().unwrap();
        // un mensaje que no termina caduca
        let l = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = l.local_addr().unwrap().port();
        let _c = TcpStream::connect(("127.0.0.1", port)).unwrap();
        let (mut s, _) = l.accept().unwrap();
        let t0 = std::time::Instant::now();
        assert!(read_capped(&mut s, 100, t0 + std::time::Duration::from_millis(200), false).is_none());
        assert!(t0.elapsed() < std::time::Duration::from_secs(3));
    }

    #[cfg(all(unix, not(target_os = "android")))]
    #[test]
    fn unix_socket() {
        use std::io::Write;
        use std::os::unix::ffi::OsStrExt;
        use std::os::unix::fs::PermissionsExt;
        use std::os::unix::net::{UnixListener, UnixStream};
        let tmp = std::env::temp_dir().join(format!("fb_sock_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        // SAFETY: getuid no tiene precondiciones.
        let uid = unsafe { libc::getuid() };
        // un XDG_RUNTIME_DIR abierto a otros no vale: se usa la carpeta privada
        let open = tmp.join("abierta");
        std::fs::create_dir(&open).unwrap();
        std::fs::set_permissions(&open, std::fs::Permissions::from_mode(0o777)).unwrap();
        let d = socket_dir_in(Some(open.clone()), &tmp, uid).unwrap();
        assert_eq!(d, tmp.join(format!("filebrowser-rs-{uid}")));
        assert_eq!(std::fs::metadata(&d).unwrap().permissions().mode() & 0o777, 0o700);
        // si la de /tmp es de otros permisos (la creó otro), no se usa
        std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(socket_dir_in(None, &tmp, uid).is_none());
        std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o700)).unwrap();
        // ida y vuelta por el socket, con el uid del otro extremo
        let sock = d.join("s.sock");
        let l = UnixListener::bind(&sock).unwrap();
        let mut c = UnixStream::connect(&sock).unwrap();
        c.write_all(b"/tmp/a\n/tmp/caf\xe9\n").unwrap();
        drop(c);
        let (mut s, _) = l.accept().unwrap();
        assert_eq!(peer_uid(&s), Some(uid));
        let buf = read_capped(&mut s, MAX_MSG, std::time::Instant::now() + MSG_TIME, false).unwrap();
        assert_eq!(split_paths(&buf), vec![PathBuf::from("/tmp/a"), PathBuf::from(OsStr::from_bytes(b"/tmp/caf\xe9"))]);
        assert_eq!(msg_token(&buf), None);
        // con token de activación: no es una ruta y vuelve igual (aunque lleve «/»)
        let mut buf = b"/tmp/a\n".to_vec();
        buf.extend_from_slice(&token_line(Path::new("/tmp"), "gnome-shell/x/12_TIME3"));
        assert_eq!(split_paths(&buf), vec![PathBuf::from("/tmp/a")]);
        assert_eq!(msg_token(&buf).as_deref(), Some("gnome-shell/x/12_TIME3"));
        // una versión anterior la vería como un archivo dentro de /tmp
        let old = PathBuf::from(OsStr::from_bytes(buf.split(|&b| b == b'\n').nth(1).unwrap()));
        assert_eq!(old.parent(), Some(Path::new("/tmp")));
        assert!(!valid_token("a\nb") && !valid_token(""));
        // demasiado grande: se descarta
        let mut c = UnixStream::connect(&sock).unwrap();
        c.write_all(&[b'x'; 200]).unwrap();
        drop(c);
        let (mut s, _) = l.accept().unwrap();
        assert!(read_capped(&mut s, 100, std::time::Instant::now() + MSG_TIME, false).is_none());
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
