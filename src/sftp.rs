//! SFTP sin dependencias: cliente del protocolo SFTP v3 sobre el `ssh` del sistema (`ssh -s … sftp`).
//!
//! Los servidores se ven como una carpeta espejo en la caché: cada carpeta remota que se abre se
//! lista y se refleja con archivos «marcadores» (dispersos, con el tamaño y la fecha reales); el
//! contenido se descarga cuando hace falta (abrir, copiar, vista previa). Es de sólo lectura.
use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

const T_INIT: u8 = 1;
const T_VERSION: u8 = 2;
const T_OPEN: u8 = 3;
const T_CLOSE: u8 = 4;
const T_READ: u8 = 5;
const T_OPENDIR: u8 = 11;
const T_READDIR: u8 = 12;
const T_STAT: u8 = 17;
const T_REALPATH: u8 = 16;
const T_STATUS: u8 = 101;
const T_HANDLE: u8 = 102;
const T_DATA: u8 = 103;
const T_NAME: u8 = 104;
const T_ATTRS: u8 = 105;

const CHUNK: u32 = 32 * 1024;
const WINDOW: usize = 16;

#[derive(Clone, Debug, Default)]
pub struct Attrs {
    pub size: u64,
    pub perm: u32,
    pub mtime: i64,
    pub has_perm: bool,
}

impl Attrs {
    pub fn is_dir(&self) -> bool {
        self.has_perm && self.perm & 0o170000 == 0o040000
    }
    pub fn is_link(&self) -> bool {
        self.has_perm && self.perm & 0o170000 == 0o120000
    }
}

#[derive(Clone, Debug)]
pub struct RemoteEnt {
    pub name: String,
    pub attrs: Attrs,
}

struct Rd<'a> {
    b: &'a [u8],
    i: usize,
}

impl<'a> Rd<'a> {
    fn u8(&mut self) -> Result<u8, String> {
        let v = *self.b.get(self.i).ok_or("respuesta incompleta")?;
        self.i += 1;
        Ok(v)
    }
    fn u32(&mut self) -> Result<u32, String> {
        let s = self.b.get(self.i..self.i + 4).ok_or("respuesta incompleta")?;
        self.i += 4;
        Ok(u32::from_be_bytes(s.try_into().unwrap()))
    }
    fn u64(&mut self) -> Result<u64, String> {
        let s = self.b.get(self.i..self.i + 8).ok_or("respuesta incompleta")?;
        self.i += 8;
        Ok(u64::from_be_bytes(s.try_into().unwrap()))
    }
    fn bytes(&mut self) -> Result<&'a [u8], String> {
        let n = self.u32()? as usize;
        let s = self.b.get(self.i..self.i.checked_add(n).ok_or("respuesta incompleta")?).ok_or("respuesta incompleta")?;
        self.i += n;
        Ok(s)
    }
    fn string(&mut self) -> Result<String, String> {
        Ok(String::from_utf8_lossy(self.bytes()?).into_owned())
    }
    fn attrs(&mut self) -> Result<Attrs, String> {
        let flags = self.u32()?;
        let mut a = Attrs::default();
        if flags & 0x1 != 0 {
            a.size = self.u64()?;
        }
        if flags & 0x2 != 0 {
            self.u32()?;
            self.u32()?;
        }
        if flags & 0x4 != 0 {
            a.perm = self.u32()?;
            a.has_perm = true;
        }
        if flags & 0x8 != 0 {
            self.u32()?;
            a.mtime = self.u32()? as i64;
        }
        if flags & 0x8000_0000 != 0 {
            for _ in 0..self.u32()? {
                self.bytes()?;
                self.bytes()?;
            }
        }
        Ok(a)
    }
}

fn put_str(v: &mut Vec<u8>, s: &[u8]) {
    v.extend_from_slice(&(s.len() as u32).to_be_bytes());
    v.extend_from_slice(s);
}

pub struct Client {
    r: Box<dyn Read + Send>,
    w: Box<dyn Write + Send>,
    child: Option<std::process::Child>,
    next: u32,
    err: Arc<Mutex<String>>,
}

impl Drop for Client {
    fn drop(&mut self) {
        if let Some(c) = &mut self.child {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

impl Client {
    /// Handshake sobre un par de flujos ya abiertos.
    pub fn from_streams(r: Box<dyn Read + Send>, w: Box<dyn Write + Send>, child: Option<std::process::Child>, err: Arc<Mutex<String>>) -> Result<Client, String> {
        let mut c = Client { r, w, child, next: 1, err };
        let mut body = vec![];
        body.extend_from_slice(&3u32.to_be_bytes());
        c.send(T_INIT, &body).map_err(|e| c.fail(e))?;
        let (ty, _) = c.recv()?;
        if ty != T_VERSION {
            return Err("el servidor no respondió como SFTP".into());
        }
        Ok(c)
    }

    fn fail(&self, e: impl std::fmt::Display) -> String {
        let s = self.err.lock().unwrap().trim().to_string();
        match s.lines().rev().find(|l| !l.trim().is_empty()) {
            Some(l) => l.trim().to_string(),
            None => e.to_string(),
        }
    }

    fn send(&mut self, ty: u8, body: &[u8]) -> io::Result<()> {
        let mut p = Vec::with_capacity(body.len() + 5);
        p.extend_from_slice(&((body.len() + 1) as u32).to_be_bytes());
        p.push(ty);
        p.extend_from_slice(body);
        self.w.write_all(&p)?;
        self.w.flush()
    }

    fn recv(&mut self) -> Result<(u8, Vec<u8>), String> {
        let mut len = [0u8; 4];
        if let Err(e) = self.r.read_exact(&mut len) {
            return Err(self.fail(e));
        }
        let n = u32::from_be_bytes(len) as usize;
        if n == 0 || n > 8 << 20 {
            return Err("respuesta no válida del servidor".into());
        }
        let mut buf = vec![0u8; n];
        if let Err(e) = self.r.read_exact(&mut buf) {
            return Err(self.fail(e));
        }
        let ty = buf[0];
        buf.remove(0);
        Ok((ty, buf))
    }

    fn id(&mut self) -> u32 {
        self.next = self.next.wrapping_add(1);
        self.next
    }

    /// Una petición y su respuesta. Devuelve (tipo, cuerpo sin el id).
    fn request(&mut self, ty: u8, f: impl FnOnce(&mut Vec<u8>)) -> Result<(u8, Vec<u8>), String> {
        let id = self.id();
        let mut body = id.to_be_bytes().to_vec();
        f(&mut body);
        self.send(ty, &body).map_err(|e| self.fail(e))?;
        loop {
            let (rt, rb) = self.recv()?;
            if rb.len() < 4 {
                return Err("respuesta no válida del servidor".into());
            }
            if u32::from_be_bytes(rb[..4].try_into().unwrap()) == id {
                return Ok((rt, rb[4..].to_vec()));
            }
        }
    }

    fn status_err(body: &[u8]) -> String {
        let mut r = Rd { b: body, i: 0 };
        let code = r.u32().unwrap_or(4);
        let msg = r.string().unwrap_or_default();
        match code {
            2 => "no existe".into(),
            3 => "permiso denegado".into(),
            _ if !msg.is_empty() => msg,
            _ => format!("error SFTP {code}"),
        }
    }

    pub fn realpath(&mut self, path: &str) -> Result<String, String> {
        let (ty, b) = self.request(T_REALPATH, |v| put_str(v, path.as_bytes()))?;
        match ty {
            T_NAME => {
                let mut r = Rd { b: &b, i: 0 };
                r.u32()?;
                r.string()
            }
            T_STATUS => Err(Self::status_err(&b)),
            _ => Err("respuesta inesperada".into()),
        }
    }

    pub fn stat(&mut self, path: &str) -> Result<Attrs, String> {
        let (ty, b) = self.request(T_STAT, |v| put_str(v, path.as_bytes()))?;
        match ty {
            T_ATTRS => Rd { b: &b, i: 0 }.attrs(),
            T_STATUS => Err(Self::status_err(&b)),
            _ => Err("respuesta inesperada".into()),
        }
    }

    fn open(&mut self, ty: u8, path: &str) -> Result<Vec<u8>, String> {
        let (rt, b) = self.request(ty, |v| {
            put_str(v, path.as_bytes());
            if ty == T_OPEN {
                v.extend_from_slice(&1u32.to_be_bytes()); // SSH_FXF_READ
                v.extend_from_slice(&0u32.to_be_bytes()); // sin atributos
            }
        })?;
        match rt {
            T_HANDLE => Rd { b: &b, i: 0 }.bytes().map(|h| h.to_vec()),
            T_STATUS => Err(Self::status_err(&b)),
            _ => Err("respuesta inesperada".into()),
        }
    }

    fn close(&mut self, h: &[u8]) {
        let _ = self.request(T_CLOSE, |v| put_str(v, h));
    }

    /// Contenido de una carpeta remota (sin «.» ni «..»); los enlaces se resuelven con `stat`.
    pub fn list(&mut self, path: &str) -> Result<Vec<RemoteEnt>, String> {
        let h = self.open(T_OPENDIR, path)?;
        let mut out = Vec::new();
        let res = (|| -> Result<(), String> {
            loop {
                let (ty, b) = self.request(T_READDIR, |v| put_str(v, &h))?;
                match ty {
                    T_NAME => {
                        let mut r = Rd { b: &b, i: 0 };
                        for _ in 0..r.u32()? {
                            let name = r.string()?;
                            r.string()?; // longname
                            let attrs = r.attrs()?;
                            // un servidor hostil podría devolver rutas («../x») para escribir fuera de la carpeta destino
                            let bad = name.is_empty() || name.contains(['/', '\0']) || (cfg!(windows) && name.contains('\\'));
                            if name != "." && name != ".." && !bad {
                                out.push(RemoteEnt { name, attrs });
                            }
                        }
                    }
                    T_STATUS => {
                        if (Rd { b: &b, i: 0 }).u32()? == 1 {
                            return Ok(());
                        }
                        return Err(Self::status_err(&b));
                    }
                    _ => return Err("respuesta inesperada".into()),
                }
            }
        })();
        self.close(&h);
        res?;
        let base = path.trim_end_matches('/');
        for e in out.iter_mut() {
            if e.attrs.is_link() {
                match self.stat(&format!("{base}/{}", e.name)) {
                    Ok(a) => e.attrs = a,
                    Err(_) => e.attrs.perm = 0o100000, // enlace roto: se trata como archivo
                }
            }
        }
        Ok(out)
    }

    /// Descarga `remote` en `local` con varias lecturas en vuelo. `cancel` se consulta entre bloques.
    pub fn download(&mut self, remote: &str, local: &Path, size: u64, cancel: &AtomicBool, progress: &mut dyn FnMut(u64)) -> Result<(), String> {
        use std::io::{Seek, SeekFrom};
        let h = self.open(T_OPEN, remote)?;
        let tmp = {
            let mut n = local.file_name().map(|n| n.to_os_string()).unwrap_or_default();
            n.push(".fb_part");
            local.with_file_name(n)
        };
        let res = (|| -> Result<(), String> {
            let mut f = std::fs::File::create(&tmp).map_err(|e| e.to_string())?;
            let mut pending: HashMap<u32, (u64, u32)> = HashMap::new();
            let mut next_off = 0u64;
            let mut got = 0u64;
            let mut eof = false;
            loop {
                if cancel.load(Ordering::Relaxed) {
                    return Err("cancelado".into());
                }
                while !eof && pending.len() < WINDOW && next_off < size {
                    let len = (size - next_off).min(CHUNK as u64) as u32;
                    let id = self.id();
                    let mut body = id.to_be_bytes().to_vec();
                    put_str(&mut body, &h);
                    body.extend_from_slice(&next_off.to_be_bytes());
                    body.extend_from_slice(&len.to_be_bytes());
                    self.send(T_READ, &body).map_err(|e| self.fail(e))?;
                    pending.insert(id, (next_off, len));
                    next_off += len as u64;
                }
                if pending.is_empty() {
                    break;
                }
                let (ty, b) = self.recv()?;
                if b.len() < 4 {
                    return Err("respuesta no válida del servidor".into());
                }
                let id = u32::from_be_bytes(b[..4].try_into().unwrap());
                let Some((off, len)) = pending.remove(&id) else { continue };
                match ty {
                    T_DATA => {
                        let mut r = Rd { b: &b[4..], i: 0 };
                        let data = r.bytes()?;
                        // un servidor hostil no puede enviar más de lo pedido ni vaciar el bloque para repetirlo sin fin
                        if data.len() as u64 > len as u64 || (data.is_empty() && off < size) {
                            return Err("respuesta no válida del servidor".into());
                        }
                        f.seek(SeekFrom::Start(off)).map_err(|e| e.to_string())?;
                        f.write_all(data).map_err(|e| e.to_string())?;
                        got += data.len() as u64;
                        progress(got);
                        if (data.len() as u32) < len && off + (data.len() as u64) < size {
                            // lectura corta: se pide el resto de ese bloque
                            let rest = len - data.len() as u32;
                            let id = self.id();
                            let mut body = id.to_be_bytes().to_vec();
                            put_str(&mut body, &h);
                            body.extend_from_slice(&(off + data.len() as u64).to_be_bytes());
                            body.extend_from_slice(&rest.to_be_bytes());
                            self.send(T_READ, &body).map_err(|e| self.fail(e))?;
                            pending.insert(id, (off + data.len() as u64, rest));
                        }
                    }
                    T_STATUS => {
                        if (Rd { b: &b[4..], i: 0 }).u32()? == 1 {
                            eof = true; // fin de archivo (tamaño menor que el anunciado)
                        } else {
                            return Err(Self::status_err(&b[4..]));
                        }
                    }
                    _ => return Err("respuesta inesperada".into()),
                }
            }
            f.flush().map_err(|e| e.to_string())
        })();
        self.close(&h);
        match res {
            Ok(()) => std::fs::rename(&tmp, local).map_err(|e| e.to_string()),
            Err(e) => {
                let _ = std::fs::remove_file(&tmp);
                Err(e)
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Conexión con el `ssh` del sistema
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct Conn {
    pub host: String,
    pub port: Option<u16>,
    pub user: String,
    pub pass: Option<String>,
}

/// Cuando `ssh` pide la contraseña llama a este mismo programa (SSH_ASKPASS): se responde y se sale.
/// Debe llamarse al arrancar, antes de abrir ninguna ventana.
pub fn askpass_main() {
    if let Ok(pw) = std::env::var("FB_ASKPASS_PW") {
        let prompt = std::env::args().nth(1).unwrap_or_default().to_lowercase();
        // la clave del servidor solo la acepta el usuario desde el diálogo, nunca esta respuesta automática
        if prompt.contains("(yes/no") || prompt.contains("continue connecting") {
            println!("no");
        } else {
            println!("{pw}");
        }
        std::process::exit(0);
    }
}

fn spawn_ssh(c: &Conn) -> Result<Client, String> {
    use std::process::{Command, Stdio};
    let mut cmd = Command::new("ssh");
    cmd.args(["-s", "-o", "StrictHostKeyChecking=yes", "-o", "ConnectTimeout=15", "-o", "ServerAliveInterval=20", "-o", "NumberOfPasswordPrompts=1"]);
    if let Some(p) = c.port {
        cmd.arg("-p").arg(p.to_string());
    }
    match &c.pass {
        Some(pw) => {
            let me = std::env::current_exe().map_err(|e| e.to_string())?;
            cmd.env("SSH_ASKPASS", me).env("SSH_ASKPASS_REQUIRE", "force").env("FB_ASKPASS_PW", pw);
            if std::env::var_os("DISPLAY").is_none() {
                cmd.env("DISPLAY", ":0");
            }
            cmd.args(["-o", "PreferredAuthentications=password,keyboard-interactive", "-o", "PubkeyAuthentication=no"]);
        }
        None => {
            cmd.args(["-o", "BatchMode=yes"]);
        }
    }
    let target = if c.user.is_empty() { c.host.clone() } else { format!("{}@{}", c.user, c.host) };
    cmd.arg("--").arg(target).arg("sftp");
    cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    let mut child = cmd.spawn().map_err(|e| format!("no se pudo ejecutar ssh: {e}"))?;
    let (w, r, mut er) = (child.stdin.take().unwrap(), child.stdout.take().unwrap(), child.stderr.take().unwrap());
    let err = Arc::new(Mutex::new(String::new()));
    let e2 = err.clone();
    std::thread::spawn(move || {
        let mut s = Vec::new();
        let mut buf = [0u8; 1024];
        while let Ok(n) = er.read(&mut buf) {
            if n == 0 {
                break;
            }
            s.extend_from_slice(&buf[..n]);
            s.truncate(8192);
            *e2.lock().unwrap() = String::from_utf8_lossy(&s).into_owned();
        }
    });
    Client::from_streams(Box::new(r), Box::new(w), Some(child), err)
}

// ---------------------------------------------------------------------------
// Carpeta espejo
// ---------------------------------------------------------------------------

struct Node {
    remote: String,
    size: u64,
    mtime: i64,
    dir: bool,
    fetched: bool,
}

pub struct Session {
    root: PathBuf,
    conn: Mutex<Conn>,
    client: Mutex<Option<Client>>,
    nodes: Mutex<HashMap<PathBuf, Node>>,
}

static SESSIONS: OnceLock<Mutex<Vec<Arc<Session>>>> = OnceLock::new();

fn sessions() -> &'static Mutex<Vec<Arc<Session>>> {
    SESSIONS.get_or_init(|| Mutex::new(Vec::new()))
}

fn cache_root() -> PathBuf {
    dirs::cache_dir().unwrap_or_else(std::env::temp_dir).join("filebrowser").join("sftp")
}

/// ¿Está `p` dentro de la carpeta espejo de algún servidor?
pub fn is_remote(p: &Path) -> bool {
    session_for(p).is_some()
}

fn session_for(p: &Path) -> Option<Arc<Session>> {
    let g = SESSIONS.get()?.lock().unwrap();
    g.iter().find(|s| p.starts_with(&s.root)).cloned()
}

fn local_name(n: &str) -> String {
    #[cfg(windows)]
    {
        n.chars().map(|c| if "<>:\"|?*\\".contains(c) { '_' } else { c }).collect()
    }
    #[cfg(not(windows))]
    {
        n.replace('/', "_")
    }
}

impl Session {
    fn with_client<T>(&self, mut f: impl FnMut(&mut Client) -> Result<T, String>) -> Result<T, String> {
        let mut g = self.client.lock().unwrap();
        for attempt in 0..2 {
            if g.is_none() {
                *g = Some(spawn_ssh(&self.conn.lock().unwrap().clone())?);
            }
            match f(g.as_mut().unwrap()) {
                Ok(v) => return Ok(v),
                // un error de E/S suele ser conexión caída: se reconecta una vez
                Err(e) if attempt == 0 && !e.starts_with("no existe") && !e.starts_with("permiso") && e != "cancelado" => {
                    *g = None;
                }
                Err(e) => return Err(e),
            }
        }
        Err("conexión perdida".into())
    }

    fn remote_of(&self, local: &Path) -> Option<String> {
        if local == self.root {
            return Some("/".into());
        }
        self.nodes.lock().unwrap().get(local).map(|n| n.remote.clone())
    }

    fn sync(&self, dir: &Path) -> Result<(), String> {
        let Some(remote) = self.remote_of(dir) else { return Ok(()) };
        let ents = self.with_client(|c| c.list(&remote))?;
        let _ = std::fs::create_dir_all(dir);
        let mut keep: Vec<String> = Vec::new();
        let base = remote.trim_end_matches('/');
        let mut nodes = self.nodes.lock().unwrap();
        for e in ents {
            let ln = local_name(&e.name);
            let lp = dir.join(&ln);
            let rp = format!("{base}/{}", e.name);
            keep.push(ln);
            if e.attrs.is_dir() {
                let _ = std::fs::create_dir_all(&lp);
                nodes.insert(lp, Node { remote: rp, size: 0, mtime: e.attrs.mtime, dir: true, fetched: true });
            } else {
                let same = nodes.get(&lp).map(|n| !n.dir && n.size == e.attrs.size && n.mtime == e.attrs.mtime).unwrap_or(false)
                    && std::fs::symlink_metadata(&lp).is_ok();
                if !same {
                    if let Ok(f) = std::fs::File::create(&lp) {
                        let _ = f.set_len(e.attrs.size);
                        let _ = f.set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(e.attrs.mtime.max(0) as u64));
                    }
                    nodes.insert(lp, Node { remote: rp, size: e.attrs.size, mtime: e.attrs.mtime, dir: false, fetched: false });
                }
            }
        }
        // lo que ya no está en el servidor
        if let Ok(rd) = std::fs::read_dir(dir) {
            for e in rd.flatten() {
                let n = e.file_name().to_string_lossy().into_owned();
                if !keep.contains(&n) && !n.ends_with(".fb_part") {
                    let p = e.path();
                    nodes.retain(|k, _| !k.starts_with(&p));
                    if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                        let _ = std::fs::remove_dir_all(&p);
                    } else {
                        let _ = std::fs::remove_file(&p);
                    }
                }
            }
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Clave del servidor (known_hosts): se muestra la huella y el usuario decide
// ---------------------------------------------------------------------------

/// Texto de error con el que `connect` avisa de que falta confirmar la clave del servidor.
pub const HOSTKEY: &str = "HOSTKEY";

/// Clave de un servidor nuevo, a la espera de que el usuario compruebe la huella.
#[derive(Clone)]
pub struct PendingKey {
    pub host: String,
    pub port: Option<u16>,
    /// Huellas legibles («256 SHA256:… (ED25519)»).
    pub fingerprints: Vec<String>,
    /// Líneas de `known_hosts` que se guardan si el usuario confía.
    scan: String,
}

impl PendingKey {
    /// Datos de ejemplo para las capturas de prueba.
    pub fn sample() -> PendingKey {
        PendingKey {
            host: "servidor.ejemplo.org".into(),
            port: Some(2222),
            fingerprints: vec!["256 SHA256:RCgIfsoyy+AtpGpjF4bsr9HfIc1MMo0N26m101FB8hc (ED25519)".into(), "3072 SHA256:u2kXr0p9mYvC1nQeLzTb5aVhWd8JsGfN4oRiE7yKcXA (RSA)".into()],
            scan: String::new(),
        }
    }
}

static PENDING: Mutex<Option<PendingKey>> = Mutex::new(None);

pub fn take_pending_key() -> Option<PendingKey> {
    PENDING.lock().ok()?.take()
}

fn hide_console(cmd: &mut std::process::Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    let _ = cmd;
}

fn known_hosts_path() -> Option<PathBuf> {
    Some(dirs::home_dir()?.join(".ssh").join("known_hosts"))
}

/// ¿Está ya el servidor en `known_hosts`? Si no, deja su clave pendiente y devuelve `HOSTKEY`.
fn check_host(conn: &Conn) -> Result<(), String> {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let entry = match conn.port {
        Some(p) if p != 22 => format!("[{}]:{p}", conn.host),
        _ => conn.host.clone(),
    };
    let mut c = Command::new("ssh-keygen");
    c.args(["-F", &entry]).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null());
    hide_console(&mut c);
    if let Ok(o) = c.output() {
        if o.status.success() && !o.stdout.is_empty() {
            return Ok(());
        }
    }
    // servidor nuevo: se pide su clave pública y se calcula la huella para enseñársela al usuario
    let mut k = Command::new("ssh-keyscan");
    k.args(["-T", "10"]);
    if let Some(p) = conn.port {
        k.arg("-p").arg(p.to_string());
    }
    k.arg("--").arg(&conn.host).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null());
    hide_console(&mut k);
    let scan = k.output().map_err(|e| format!("ssh-keyscan: {e}"))?;
    let scan = String::from_utf8_lossy(&scan.stdout).lines().filter(|l| !l.starts_with('#')).collect::<Vec<_>>().join("\n");
    if scan.trim().is_empty() {
        return Err("no se pudo obtener la clave del servidor".into());
    }
    let mut f = Command::new("ssh-keygen");
    f.args(["-l", "-f", "-"]).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
    hide_console(&mut f);
    let mut child = f.spawn().map_err(|e| format!("ssh-keygen: {e}"))?;
    if let Some(mut si) = child.stdin.take() {
        let _ = si.write_all(scan.as_bytes());
        let _ = si.write_all(b"\n");
    }
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    let fingerprints: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| {
            // «256 SHA256:abc host (ED25519)» -> sin el nombre del host
            let mut p: Vec<&str> = l.split_whitespace().collect();
            if p.len() >= 4 {
                p.remove(2);
            }
            p.join(" ")
        })
        .filter(|l| !l.is_empty())
        .take(6)
        .collect();
    if fingerprints.is_empty() {
        return Err("no se pudo calcular la huella del servidor".into());
    }
    *PENDING.lock().unwrap() = Some(PendingKey { host: conn.host.clone(), port: conn.port, fingerprints, scan });
    Err(HOSTKEY.into())
}

/// El usuario ha comprobado la huella: se guarda la clave en `known_hosts`.
pub fn trust_host(k: &PendingKey) -> Result<(), String> {
    use std::io::Write;
    let path = known_hosts_path().ok_or("no se encontró la carpeta personal")?;
    if let Some(d) = path.parent() {
        crate::fsys::private_dir(d);
    }
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(&path).map_err(|e| e.to_string())?;
    // si el archivo no acaba en salto de línea, se añade para no pegar la clave a la anterior
    let needs_nl = std::fs::read(&path).map(|b| !b.is_empty() && b.last() != Some(&b'\n')).unwrap_or(false);
    if needs_nl {
        f.write_all(b"\n").map_err(|e| e.to_string())?;
    }
    f.write_all(k.scan.as_bytes()).and_then(|_| f.write_all(b"\n")).map_err(|e| e.to_string())
}

/// Conecta con un servidor y devuelve la carpeta local que refleja su carpeta personal (o `path`).
pub fn connect(conn: Conn, path: &str) -> Result<PathBuf, String> {
    check_host(&conn)?;
    let key = format!("{}@{}{}", if conn.user.is_empty() { "_" } else { &conn.user }, conn.host, conn.port.map(|p| format!("_{p}")).unwrap_or_default());
    let root = cache_root().join(local_name(&key));
    let sess = {
        let mut g = sessions().lock().unwrap();
        match g.iter().find(|s| s.root == root) {
            Some(s) => {
                // nuevas credenciales: se reabre la conexión
                *s.conn.lock().unwrap() = conn.clone();
                *s.client.lock().unwrap() = None;
                s.clone()
            }
            None => {
                let s = Arc::new(Session { root: root.clone(), conn: Mutex::new(conn.clone()), client: Mutex::new(None), nodes: Mutex::new(HashMap::new()) });
                g.push(s.clone());
                s
            }
        }
    };
    crate::fsys::private_dir(&root);
    let start = if path.is_empty() || path == "/" {
        sess.with_client(|c| c.realpath("."))?
    } else {
        path.to_string()
    };
    // se abre la ruta carpeta a carpeta para tener sus nodos registrados
    let mut cur = root.clone();
    sess.sync(&cur)?;
    for part in start.split('/').filter(|s| !s.is_empty()) {
        // un «..» (del servidor o de la ruta escrita) sacaría el espejo de su carpeta y
        // la sincronización podría truncar o borrar archivos locales
        if part == ".." {
            return Err("ruta no válida".into());
        }
        if part == "." {
            continue;
        }
        cur = cur.join(local_name(part));
        // el remoto se calcula antes de tomar el candado: remote_of lo toma también
        let remote = format!("{}/{}", sess.remote_of(cur.parent().unwrap()).unwrap_or_default().trim_end_matches('/'), part);
        let mut nodes = sess.nodes.lock().unwrap();
        if !nodes.contains_key(&cur) {
            nodes.insert(cur.clone(), Node { remote, size: 0, mtime: 0, dir: true, fetched: true });
        }
        drop(nodes);
        let _ = std::fs::create_dir_all(&cur);
    }
    sess.sync(&cur)?;
    Ok(cur)
}

/// Refleja la carpeta remota correspondiente a `dir` (si está en un espejo). Bloquea: usar en hilos.
pub fn sync_dir(dir: &Path) -> Result<(), String> {
    match session_for(dir) {
        Some(s) => s.sync(dir),
        None => Ok(()),
    }
}

/// Refleja `p` y todas sus subcarpetas (para conocer el tamaño real antes de copiar).
pub fn sync_tree(p: &Path) {
    let Some(s) = session_for(p) else { return };
    if !p.is_dir() {
        return;
    }
    let _ = s.sync(p);
    if let Ok(rd) = std::fs::read_dir(p) {
        for e in rd.flatten() {
            if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                sync_tree(&e.path());
            }
        }
    }
}

/// Asegura que el archivo `p` tiene su contenido real (lo descarga si sólo es un marcador).
pub fn ensure_local(p: &Path) -> Result<(), String> {
    ensure_local_with(p, &AtomicBool::new(false), &mut |_| {})
}

pub fn ensure_local_with(p: &Path, cancel: &AtomicBool, progress: &mut dyn FnMut(u64)) -> Result<(), String> {
    let Some(s) = session_for(p) else { return Ok(()) };
    let (remote, size, mtime) = {
        let g = s.nodes.lock().unwrap();
        match g.get(p) {
            Some(n) if !n.dir && !n.fetched => (n.remote.clone(), n.size, n.mtime),
            _ => return Ok(()),
        }
    };
    s.with_client(|c| c.download(&remote, p, size, cancel, progress))?;
    let _ = std::fs::File::options().write(true).open(p).and_then(|f| f.set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(mtime.max(0) as u64)));
    if let Some(n) = s.nodes.lock().unwrap().get_mut(p) {
        n.fetched = true;
    }
    Ok(())
}

/// Parte de la ruta remota de `p` (para mostrar).
pub fn remote_label(p: &Path) -> Option<String> {
    let s = session_for(p)?;
    let r = s.remote_of(p).or_else(|| p.parent().and_then(|d| s.remote_of(d)))?;
    let c = s.conn.lock().unwrap();
    Some(format!("sftp://{}@{}{}", c.user, c.host, r))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::{channel, Receiver, Sender};

    struct PipeR {
        rx: Receiver<Vec<u8>>,
        buf: Vec<u8>,
    }
    impl Read for PipeR {
        fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
            while self.buf.is_empty() {
                match self.rx.recv() {
                    Ok(v) => self.buf = v,
                    Err(_) => return Ok(0),
                }
            }
            let n = out.len().min(self.buf.len());
            out[..n].copy_from_slice(&self.buf[..n]);
            self.buf.drain(..n);
            Ok(n)
        }
    }
    struct PipeW(Sender<Vec<u8>>);
    impl Write for PipeW {
        fn write(&mut self, b: &[u8]) -> io::Result<usize> {
            self.0.send(b.to_vec()).map_err(|_| io::Error::from(io::ErrorKind::BrokenPipe))?;
            Ok(b.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn pkt(ty: u8, body: &[u8]) -> Vec<u8> {
        let mut v = ((body.len() + 1) as u32).to_be_bytes().to_vec();
        v.push(ty);
        v.extend_from_slice(body);
        v
    }

    fn attrs(size: u64, perm: u32, mtime: u32) -> Vec<u8> {
        let mut v = (0x1u32 | 0x4 | 0x8).to_be_bytes().to_vec();
        v.extend_from_slice(&size.to_be_bytes());
        v.extend_from_slice(&perm.to_be_bytes());
        v.extend_from_slice(&0u32.to_be_bytes());
        v.extend_from_slice(&mtime.to_be_bytes());
        v
    }

    /// Servidor SFTP mínimo en memoria: una carpeta con un archivo grande y una subcarpeta.
    fn mock(content: Vec<u8>) -> (Box<dyn Read + Send>, Box<dyn Write + Send>) {
        let (c2s_tx, c2s_rx) = channel::<Vec<u8>>();
        let (s2c_tx, s2c_rx) = channel::<Vec<u8>>();
        std::thread::spawn(move || {
            let mut r = PipeR { rx: c2s_rx, buf: vec![] };
            let mut listed = false;
            loop {
                let mut l = [0u8; 4];
                if r.read_exact(&mut l).is_err() {
                    return;
                }
                let n = u32::from_be_bytes(l) as usize;
                let mut b = vec![0u8; n];
                r.read_exact(&mut b).unwrap();
                let ty = b[0];
                let body = &b[1..];
                let out = match ty {
                    T_INIT => pkt(T_VERSION, &3u32.to_be_bytes()),
                    _ => {
                        let id = &body[..4];
                        let mut rd = Rd { b: body, i: 4 };
                        let mut o = id.to_vec();
                        match ty {
                            T_REALPATH => {
                                o.extend_from_slice(&1u32.to_be_bytes());
                                put_str(&mut o, b"/home/u");
                                put_str(&mut o, b"/home/u");
                                o.extend_from_slice(&attrs(0, 0o040755, 0));
                                pkt(T_NAME, &o)
                            }
                            T_OPENDIR => {
                                listed = false;
                                put_str(&mut o, b"D");
                                pkt(T_HANDLE, &o)
                            }
                            T_READDIR => {
                                if listed {
                                    o.extend_from_slice(&1u32.to_be_bytes());
                                    put_str(&mut o, b"EOF");
                                    put_str(&mut o, b"");
                                    pkt(T_STATUS, &o)
                                } else {
                                    listed = true;
                                    o.extend_from_slice(&3u32.to_be_bytes());
                                    for (name, a) in [
                                        ("big.bin", attrs(content.len() as u64, 0o100644, 1_700_000_000)),
                                        ("sub", attrs(0, 0o040755, 1_700_000_001)),
                                        (".", attrs(0, 0o040755, 0)),
                                    ] {
                                        put_str(&mut o, name.as_bytes());
                                        put_str(&mut o, b"");
                                        o.extend_from_slice(&a);
                                    }
                                    pkt(T_NAME, &o)
                                }
                            }
                            T_OPEN => {
                                put_str(&mut o, b"F");
                                pkt(T_HANDLE, &o)
                            }
                            T_READ => {
                                rd.bytes().unwrap();
                                let off = rd.u64().unwrap() as usize;
                                let len = rd.u32().unwrap() as usize;
                                if off >= content.len() {
                                    o.extend_from_slice(&1u32.to_be_bytes());
                                    put_str(&mut o, b"EOF");
                                    put_str(&mut o, b"");
                                    pkt(T_STATUS, &o)
                                } else {
                                    // lecturas cortas a propósito (16 KiB como mucho)
                                    let end = (off + len.min(16 * 1024)).min(content.len());
                                    put_str(&mut o, &content[off..end]);
                                    pkt(T_DATA, &o)
                                }
                            }
                            T_CLOSE => {
                                o.extend_from_slice(&0u32.to_be_bytes());
                                put_str(&mut o, b"");
                                put_str(&mut o, b"");
                                pkt(T_STATUS, &o)
                            }
                            _ => {
                                o.extend_from_slice(&8u32.to_be_bytes());
                                put_str(&mut o, b"no soportado");
                                put_str(&mut o, b"");
                                pkt(T_STATUS, &o)
                            }
                        }
                    }
                };
                if s2c_tx.send(out).is_err() {
                    return;
                }
            }
        });
        (Box::new(PipeR { rx: s2c_rx, buf: vec![] }), Box::new(PipeW(c2s_tx)))
    }

    #[test]
    fn list_and_download_against_mock_server() {
        let content: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
        let (r, w) = mock(content.clone());
        let mut c = Client::from_streams(r, w, None, Arc::new(Mutex::new(String::new()))).unwrap();
        assert_eq!(c.realpath(".").unwrap(), "/home/u");
        let l = c.list("/home/u").unwrap();
        assert_eq!(l.len(), 2);
        assert!(l.iter().any(|e| e.name == "sub" && e.attrs.is_dir()));
        let big = l.iter().find(|e| e.name == "big.bin").unwrap();
        assert_eq!(big.attrs.size, 300_000);
        assert_eq!(big.attrs.mtime, 1_700_000_000);
        let dst = std::env::temp_dir().join(format!("fb_sftp_{}.bin", std::process::id()));
        let mut last = 0;
        c.download("/home/u/big.bin", &dst, 300_000, &AtomicBool::new(false), &mut |n| last = n).unwrap();
        assert_eq!(std::fs::read(&dst).unwrap(), content);
        assert_eq!(last, 300_000);
        let _ = std::fs::remove_file(&dst);
    }
}
