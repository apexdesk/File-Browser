//! SFTP sin dependencias: cliente del protocolo SFTP v3 sobre el `ssh` del sistema (`ssh -s … sftp`).
//!
//! Los servidores se ven como una carpeta espejo en la caché: cada carpeta remota que se abre se
//! lista y se refleja con archivos «marcadores» (dispersos, con el tamaño y la fecha reales); el
//! contenido se descarga cuando hace falta (abrir, copiar, vista previa). Es de sólo lectura.
#[allow(unused_imports)]
use crate::vfs::{KindV as _, PathV as _};
use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
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

/// Ruta o nombre remoto tal cual: en SFTP v3 son cadenas de bytes, no necesariamente UTF-8. Se
/// guardan así y así se mandan en cada petición; sólo se convierten a texto para enseñarlos.
type RPath = Vec<u8>;

/// Texto legible de una ruta remota (los bytes que no son UTF-8 se ven como «�»).
fn show(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

/// «base/nombre» (sin duplicar la barra si `base` ya acaba en una).
fn rjoin(base: &[u8], name: &[u8]) -> RPath {
    let mut v = base.to_vec();
    while v.last() == Some(&b'/') {
        v.pop();
    }
    v.push(b'/');
    v.extend_from_slice(name);
    v
}

#[derive(Clone, Debug)]
pub struct RemoteEnt {
    pub name: RPath,
    pub attrs: Attrs,
    /// enlace simbólico (ya resuelto en `attrs`)
    pub link: bool,
}

struct Rd<'a> {
    b: &'a [u8],
    i: usize,
}

impl<'a> Rd<'a> {
    fn u32(&mut self) -> Result<u32, String> {
        let s = self.b.get(self.i..self.i + 4).ok_or(crate::i18n::t("respuesta incompleta"))?;
        self.i += 4;
        Ok(u32::from_be_bytes(s.try_into().unwrap()))
    }
    fn u64(&mut self) -> Result<u64, String> {
        let s = self.b.get(self.i..self.i + 8).ok_or(crate::i18n::t("respuesta incompleta"))?;
        self.i += 8;
        Ok(u64::from_be_bytes(s.try_into().unwrap()))
    }
    fn bytes(&mut self) -> Result<&'a [u8], String> {
        let n = self.u32()? as usize;
        let s = self.b.get(self.i..self.i.checked_add(n).ok_or(crate::i18n::t("respuesta incompleta"))?).ok_or(crate::i18n::t("respuesta incompleta"))?;
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

/// Mensaje legible a partir de lo que `ssh` escribió en stderr (`None` si no dijo nada).
fn explain_ssh_err(s: &str) -> Option<String> {
    use crate::i18n::{t, tf};
    if s.contains("REMOTE HOST IDENTIFICATION HAS CHANGED") || s.contains("Host key verification failed") {
        return Some(t("La clave del servidor no coincide con la guardada: podría ser una suplantación. Si el servidor ha cambiado de clave de verdad, borra la antigua con «ssh-keygen -R servidor» y vuelve a conectar.").into());
    }
    let last = s.lines().rev().map(str::trim).find(|l| !l.is_empty())?;
    if last.contains("Permission denied") && last.contains("publickey") {
        return Some(tf("{} — si tu clave tiene frase de contraseña, añádela al agente (ssh-add) o escríbela en «Contraseña»", &[&last]));
    }
    Some(last.to_string())
}

/// Copias abiertas en otros programas, puestas de sólo lectura (se deshace al borrar el espejo).
static READ_ONLY: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

/// Quita el «sólo lectura» de una copia descargada (para reemplazarla o borrarla).
fn make_writable(p: &Path) {
    let Ok(m) = crate::vfs::fs::symlink_metadata(p) else { return };
    if !m.permissions().readonly() {
        return;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o600));
    }
    #[cfg(not(unix))]
    {
        let mut perm = m.permissions();
        #[allow(clippy::permissions_set_readonly_false)]
        perm.set_readonly(false);
        let _ = crate::vfs::fs::set_permissions(p, perm);
    }
}

/// Pone de sólo lectura la copia local de un archivo remoto que se va a abrir en otro programa: los
/// cambios no se suben al servidor, así el editor avisa al guardar en vez de perderlos sin decir nada.
pub fn set_read_only(p: &Path) {
    if session_for(p).is_none() {
        return;
    }
    let Ok(m) = crate::vfs::fs::metadata(p) else { return };
    if m.v_is_dir() {
        return;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o400));
    }
    #[cfg(not(unix))]
    {
        let mut perm = m.permissions();
        perm.set_readonly(true);
        let _ = crate::vfs::fs::set_permissions(p, perm);
    }
    let mut g = READ_ONLY.lock().unwrap();
    if !g.iter().any(|x| x == p) {
        g.push(p.to_path_buf());
    }
}

/// Devuelve a escribibles las copias de sólo lectura bajo `root` (Windows no borra las de sólo lectura).
fn release_read_only(root: &Path) {
    let mut g = READ_ONLY.lock().unwrap();
    g.retain(|p| {
        if p.starts_with(root) {
            make_writable(p);
            false
        } else {
            true
        }
    });
}

/// Proceso `ssh` de una conexión: compartido para poder matarlo al cancelar (desbloquea `recv`).
type Child = Arc<Mutex<Option<std::process::Child>>>;

pub struct Client {
    r: Box<dyn Read + Send>,
    w: Box<dyn Write + Send>,
    child: Child,
    next: u32,
    err: Arc<Mutex<String>>,
}

impl Drop for Client {
    fn drop(&mut self) {
        kill_child(&self.child);
    }
}

fn kill_child(c: &Child) {
    if let Some(mut c) = c.lock().unwrap_or_else(|e| e.into_inner()).take() {
        let _ = c.kill();
        let _ = c.wait();
    }
}

impl Client {
    /// Handshake sobre un par de flujos ya abiertos.
    pub fn from_streams(r: Box<dyn Read + Send>, w: Box<dyn Write + Send>, child: Option<std::process::Child>, err: Arc<Mutex<String>>) -> Result<Client, String> {
        Self::handshake(r, w, Arc::new(Mutex::new(child)), err)
    }

    fn handshake(r: Box<dyn Read + Send>, w: Box<dyn Write + Send>, child: Child, err: Arc<Mutex<String>>) -> Result<Client, String> {
        let mut c = Client { r, w, child, next: 1, err };
        let mut body = vec![];
        body.extend_from_slice(&3u32.to_be_bytes());
        c.send(T_INIT, &body).map_err(|e| c.fail(e))?;
        let (ty, _) = c.recv()?;
        if ty != T_VERSION {
            return Err(crate::i18n::t("el servidor no respondió como SFTP").into());
        }
        Ok(c)
    }

    fn fail(&self, e: impl std::fmt::Display) -> String {
        let s = self.err.lock().unwrap().clone();
        explain_ssh_err(&s).unwrap_or_else(|| e.to_string())
    }

    /// Olvida lo que `ssh` dijo antes (aviso de bienvenida, peticiones anteriores).
    fn clear_err(&self) {
        self.err.lock().unwrap().clear();
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
            return Err(crate::i18n::t("respuesta no válida del servidor").into());
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
        self.clear_err();
        self.send(ty, &body).map_err(|e| self.fail(e))?;
        loop {
            let (rt, rb) = self.recv()?;
            if rb.len() < 4 {
                return Err(crate::i18n::t("respuesta no válida del servidor").into());
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
            2 => crate::i18n::t("no existe").into(),
            3 => crate::i18n::t("permiso denegado").into(),
            _ if !msg.is_empty() => msg,
            _ => crate::i18n::tf("error SFTP {}", &[&code]),
        }
    }

    pub fn realpath(&mut self, path: &[u8]) -> Result<RPath, String> {
        let (ty, b) = self.request(T_REALPATH, |v| put_str(v, path))?;
        match ty {
            T_NAME => {
                let mut r = Rd { b: &b, i: 0 };
                r.u32()?;
                r.bytes().map(<[u8]>::to_vec)
            }
            T_STATUS => Err(Self::status_err(&b)),
            _ => Err(crate::i18n::t("respuesta inesperada").into()),
        }
    }

    pub fn stat(&mut self, path: &[u8]) -> Result<Attrs, String> {
        let (ty, b) = self.request(T_STAT, |v| put_str(v, path))?;
        match ty {
            T_ATTRS => Rd { b: &b, i: 0 }.attrs(),
            T_STATUS => Err(Self::status_err(&b)),
            _ => Err(crate::i18n::t("respuesta inesperada").into()),
        }
    }

    fn open(&mut self, ty: u8, path: &[u8]) -> Result<Vec<u8>, String> {
        let (rt, b) = self.request(ty, |v| {
            put_str(v, path);
            if ty == T_OPEN {
                v.extend_from_slice(&1u32.to_be_bytes()); // SSH_FXF_READ
                v.extend_from_slice(&0u32.to_be_bytes()); // sin atributos
            }
        })?;
        match rt {
            T_HANDLE => Rd { b: &b, i: 0 }.bytes().map(|h| h.to_vec()),
            T_STATUS => Err(Self::status_err(&b)),
            _ => Err(crate::i18n::t("respuesta inesperada").into()),
        }
    }

    fn close(&mut self, h: &[u8]) {
        let _ = self.request(T_CLOSE, |v| put_str(v, h));
    }

    /// Contenido de una carpeta remota (sin «.» ni «..»); los enlaces se resuelven con `stat`.
    pub fn list(&mut self, path: &[u8]) -> Result<Vec<RemoteEnt>, String> {
        let h = self.open(T_OPENDIR, path)?;
        let mut out = Vec::new();
        let res = (|| -> Result<(), String> {
            loop {
                let (ty, b) = self.request(T_READDIR, |v| put_str(v, &h))?;
                match ty {
                    T_NAME => {
                        let mut r = Rd { b: &b, i: 0 };
                        for _ in 0..r.u32()? {
                            let name = r.bytes()?.to_vec();
                            r.bytes()?; // longname
                            let attrs = r.attrs()?;
                            // un servidor hostil podría devolver rutas («../x») para escribir fuera de la carpeta destino
                            let bad = name.is_empty() || name.iter().any(|&b| b == b'/' || b == 0 || (cfg!(windows) && b == b'\\'));
                            if name != b"." && name != b".." && !bad {
                                let link = attrs.is_link();
                                out.push(RemoteEnt { name, attrs, link });
                            }
                        }
                    }
                    T_STATUS => {
                        if (Rd { b: &b, i: 0 }).u32()? == 1 {
                            return Ok(());
                        }
                        return Err(Self::status_err(&b));
                    }
                    _ => return Err(crate::i18n::t("respuesta inesperada").into()),
                }
            }
        })();
        self.close(&h);
        res?;
        for e in out.iter_mut() {
            if e.attrs.is_link() {
                match self.stat(&rjoin(path, &e.name)) {
                    Ok(a) => e.attrs = a,
                    Err(_) => e.attrs.perm = 0o100000, // enlace roto: se trata como archivo
                }
            }
        }
        Ok(out)
    }

    /// Descarga `remote` en `local` con varias lecturas en vuelo. `cancel` se consulta entre bloques.
    pub fn download(&mut self, remote: &[u8], local: &Path, size: u64, cancel: &AtomicBool, progress: &mut dyn FnMut(u64)) -> Result<(), String> {
        use std::io::{Seek, SeekFrom};
        self.clear_err();
        let h = self.open(T_OPEN, remote)?;
        let tmp = {
            let mut n = local.file_name().map(|n| n.to_os_string()).unwrap_or_default();
            n.push(".fb_part");
            local.with_file_name(n)
        };
        let res = (|| -> Result<(), String> {
            let mut f = crate::vfs::fs::File::create(&tmp).map_err(|e| e.to_string())?;
            let mut pending: HashMap<u32, (u64, u32)> = HashMap::new();
            let mut next_off = 0u64;
            let mut got = 0u64;
            let mut eof = false;
            loop {
                if cancel.load(Ordering::Relaxed) {
                    return Err(crate::i18n::t("Operación cancelada").into());
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
                    return Err(crate::i18n::t("respuesta no válida del servidor").into());
                }
                let id = u32::from_be_bytes(b[..4].try_into().unwrap());
                let Some((off, len)) = pending.remove(&id) else { continue };
                match ty {
                    T_DATA => {
                        let mut r = Rd { b: &b[4..], i: 0 };
                        let data = r.bytes()?;
                        // un servidor hostil no puede enviar más de lo pedido ni vaciar el bloque para repetirlo sin fin
                        if data.len() as u64 > len as u64 || (data.is_empty() && off < size) {
                            return Err(crate::i18n::t("respuesta no válida del servidor").into());
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
                    _ => return Err(crate::i18n::t("respuesta inesperada").into()),
                }
            }
            f.flush().map_err(|e| e.to_string())
        })();
        self.close(&h);
        match res {
            Ok(()) => {
                // la copia anterior pudo quedar de sólo lectura (abierta en otro programa)
                make_writable(local);
                crate::vfs::fs::rename(&tmp, local).map_err(|e| e.to_string())
            }
            Err(e) => {
                let _ = crate::vfs::fs::remove_file(&tmp);
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
///
/// La contraseña no viaja en el entorno (lo heredarían `ProxyCommand` y compañía): está en un
/// archivo privado que se lee y se borra en la primera respuesta. Sólo se responde a la pregunta de
/// contraseña del usuario y servidor esperados y, una vez, a la frase de una clave cifrada (se usa en
/// local); a cualquier otra (clave de otro salto, código de verificación, «yes/no» de la huella…) no
/// se responde nada y `ssh` la da por fallida.
pub fn askpass_main() {
    let Some(file) = std::env::var_os("FB_ASKPASS_FILE") else { return };
    // args_os: un argumento que no sea UTF-8 no debe hacer abortar al programa
    let prompt = std::env::args_os().nth(1).map(|a| a.to_string_lossy().into_owned()).unwrap_or_default();
    let user = std::env::var("FB_ASKPASS_USER").unwrap_or_default();
    let hosts: Vec<String> = std::env::var("FB_ASKPASS_HOSTS").unwrap_or_default().split(' ').filter(|h| !h.is_empty()).map(String::from).collect();
    let plain = std::env::var_os("FB_ASKPASS_PLAIN").is_some();
    let mut code = 1;
    if prompt_matches(&prompt, &user, &hosts, plain) {
        if let Some(pw) = take_secret(Path::new(&file)) {
            println!("{pw}");
            code = 0;
        }
    } else if is_passphrase_prompt(&prompt) {
        // frase de una clave: se descifra en local, no se envía a nadie (y sólo una vez)
        if let Some(pw) = std::env::var_os("FB_ASKPASS_KEYFILE").and_then(|f| take_secret(Path::new(&f))) {
            println!("{pw}");
            code = 0;
        }
    }
    std::process::exit(code);
}

/// ¿Es `prompt` la pregunta de contraseña de `user` en alguno de `hosts`? Formatos de OpenSSH:
/// «usuario@host's password: » (método password) y «(usuario@host) Password: » (keyboard-interactive
/// desde OpenSSH 8.x). `plain` acepta además un «Password:» a secas (keyboard-interactive de un ssh
/// antiguo); sólo se usa cuando no hay saltos intermedios que puedan preguntar lo mismo.
fn prompt_matches(prompt: &str, user: &str, hosts: &[String], plain: bool) -> bool {
    let p = prompt.trim().to_lowercase();
    if plain && p == "password:" {
        return true;
    }
    if user.is_empty() {
        return false;
    }
    let u = user.to_lowercase();
    hosts.iter().map(|h| h.to_lowercase()).filter(|h| !h.is_empty()).any(|h| {
        p == format!("{u}@{h}'s password:") || p == format!("({u}@{h}) password:")
    })
}

/// «Enter passphrase for key '…': » de OpenSSH.
fn is_passphrase_prompt(prompt: &str) -> bool {
    prompt.trim().to_lowercase().starts_with("enter passphrase for key ")
}

/// Archivo con la contraseña para el ayudante de `askpass_main`; se borra al soltarse.
struct Secret(PathBuf);

impl Drop for Secret {
    fn drop(&mut self) {
        let _ = crate::vfs::fs::remove_file(&self.0);
    }
}

fn write_secret(dir: &Path, pw: &str) -> Result<Secret, String> {
    use std::sync::atomic::AtomicU64;
    static N: AtomicU64 = AtomicU64::new(0);
    let path = dir.join(format!(".askpass-{}-{}", crate::vfs::pid(), N.fetch_add(1, Ordering::Relaxed)));
    let mut o = crate::vfs::fs::OpenOptions::new();
    o.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    let _ = crate::vfs::fs::remove_file(&path);
    let mut f = o.open(&path).map_err(|e| e.to_string())?;
    let s = Secret(path);
    f.write_all(pw.as_bytes()).map_err(|e| e.to_string())?;
    Ok(s)
}

/// Lee la contraseña y borra el archivo: sólo sirve una vez.
fn take_secret(p: &Path) -> Option<String> {
    let pw = crate::vfs::fs::read_to_string(p).ok();
    let _ = crate::vfs::fs::remove_file(p);
    pw
}

/// Lo que `ssh -G` resuelve de la configuración (alias de ssh_config, ProxyJump…).
#[derive(Debug, Default, Clone)]
struct Resolved {
    user: String,
    hostname: String,
    port: u16,
    alias: Option<String>,
    /// el servidor se alcanza a través de otro (ProxyJump / ProxyCommand)
    proxied: bool,
    /// archivos known_hosts del usuario y del sistema
    known: Vec<PathBuf>,
    /// los del usuario (UserKnownHostsFile): donde se guarda una clave aceptada
    user_known: Vec<PathBuf>,
}

fn expand_home(f: &str, home: Option<&Path>) -> Option<PathBuf> {
    if let Some(rest) = f.strip_prefix("~/").or_else(|| f.strip_prefix("%d/")) {
        return Some(home?.join(rest));
    }
    if let Some(rest) = f.strip_prefix("__PROGRAMDATA__") {
        let pd = std::env::var_os("PROGRAMDATA")?;
        return Some(PathBuf::from(format!("{}{rest}", pd.to_string_lossy())));
    }
    Some(PathBuf::from(f))
}

/// Interpreta la salida de `ssh -G`.
fn parse_resolved(out: &str, home: Option<&Path>) -> Resolved {
    let mut r = Resolved::default();
    for l in out.lines() {
        let Some((k, v)) = l.trim().split_once(' ') else { continue };
        let v = v.trim();
        match k.to_ascii_lowercase().as_str() {
            "user" => r.user = v.to_string(),
            "hostname" => r.hostname = v.to_string(),
            "port" => r.port = v.parse().unwrap_or(0),
            "hostkeyalias" if v != "none" => r.alias = Some(v.to_string()),
            "proxyjump" | "proxycommand" if v != "none" => r.proxied = true,
            "userknownhostsfile" | "globalknownhostsfile" => {
                let files: Vec<PathBuf> = v.split_whitespace().filter(|f| *f != "none").filter_map(|f| expand_home(f, home)).collect();
                if k.eq_ignore_ascii_case("userknownhostsfile") {
                    r.user_known.extend(files.iter().cloned());
                }
                r.known.extend(files);
            }
            _ => {}
        }
    }
    r
}

fn resolve(c: &Conn) -> Resolved {
    use std::process::{Command, Stdio};
    let home = dirs::home_dir();
    let mut cmd = Command::new("ssh");
    cmd.arg("-G");
    if let Some(p) = c.port {
        cmd.arg("-p").arg(p.to_string());
    }
    cmd.arg("--").arg(ssh_target(c)).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null());
    hide_console(&mut cmd);
    let mut r = match cmd.output() {
        Ok(o) if o.status.success() => parse_resolved(&String::from_utf8_lossy(&o.stdout), home.as_deref()),
        _ => Resolved::default(),
    };
    if r.user.is_empty() {
        r.user = c.user.clone();
    }
    if r.hostname.is_empty() {
        r.hostname = c.host.clone();
    }
    if r.port == 0 {
        r.port = c.port.unwrap_or(22);
    }
    if r.known.is_empty() {
        if let Some(h) = &home {
            r.known.push(h.join(".ssh").join("known_hosts"));
            r.known.push(h.join(".ssh").join("known_hosts2"));
            r.user_known.push(h.join(".ssh").join("known_hosts"));
        }
        r.known.push(PathBuf::from("/etc/ssh/ssh_known_hosts"));
        r.known.push(PathBuf::from("/etc/ssh/ssh_known_hosts2"));
    }
    r
}

/// «usuario@servidor» para `ssh` (una IPv6 va sin corchetes: el puerto va aparte con -p).
fn ssh_target(c: &Conn) -> String {
    let h = c.host.trim_start_matches('[').trim_end_matches(']');
    if c.user.is_empty() { h.to_string() } else { format!("{}@{h}", c.user) }
}

/// Versión (mayor, menor) de OpenSSH en la salida de `ssh -V`: «OpenSSH_9.6p1 Ubuntu…»,
/// «OpenSSH_for_Windows_8.1p1, LibreSSL 3.0.2».
fn parse_ssh_version(s: &str) -> Option<(u32, u32)> {
    let rest = &s[s.find("OpenSSH_")? + "OpenSSH_".len()..];
    let rest = rest.strip_prefix("for_Windows_").unwrap_or(rest);
    let mut it = rest.split(|c: char| !c.is_ascii_digit());
    Some((it.next()?.parse().ok()?, it.next()?.parse().ok()?))
}

/// Versión del `ssh` del sistema (`None` si no se sabe). Se pregunta una vez.
fn ssh_version() -> Option<(u32, u32)> {
    use std::process::{Command, Stdio};
    static V: OnceLock<Option<(u32, u32)>> = OnceLock::new();
    *V.get_or_init(|| {
        let mut cmd = Command::new("ssh");
        cmd.arg("-V").stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
        hide_console(&mut cmd);
        let o = cmd.output().ok()?;
        // la versión sale por stderr
        parse_ssh_version(&String::from_utf8_lossy(&o.stderr)).or_else(|| parse_ssh_version(&String::from_utf8_lossy(&o.stdout)))
    })
}

/// SSH_ASKPASS_REQUIRE (obligar a usar el ayudante aunque haya consola) llegó en OpenSSH 8.4: antes,
/// `ssh` sólo pregunta al ayudante si no tiene terminal o consola donde preguntar.
fn askpass_require_unsupported(v: Option<(u32, u32)>) -> bool {
    v.is_some_and(|v| v < (8, 4))
}

/// Tiempo máximo para entrar con contraseña: si `ssh` se queda preguntando donde nadie ve (una
/// consola oculta), se corta en vez de dejar la conexión colgada.
const LOGIN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(45);

fn login_timeout_msg(version: Option<(u32, u32)>) -> String {
    match version {
        Some((a, b)) if askpass_require_unsupported(version) => crate::i18n::tf(
            "ssh no pudo pedir la contraseña: la versión de OpenSSH instalada ({}) es anterior a la 8.4. Usa una clave SSH (añádela al agente con ssh-add) o actualiza OpenSSH",
            &[&format!("{a}.{b}")],
        ),
        _ => crate::i18n::t("El servidor no completó el inicio de sesión a tiempo").into(),
    }
}

fn spawn_ssh(c: &Conn, secret_dir: &Path) -> Result<Client, String> {
    use std::process::{Command, Stdio};
    let version = if c.pass.is_some() { ssh_version() } else { None };
    // un ssh anterior a 8.4 no admite SSH_ASKPASS_REQUIRE=force: en Windows, con una consola (aunque
    // esté oculta) pregunta en ella y se queda esperando; sin consola (DETACHED_PROCESS) usa SSH_ASKPASS
    let detached = cfg!(windows) && c.pass.is_some() && askpass_require_unsupported(version);
    let mut cmd = Command::new("ssh");
    cmd.args(["-s", "-o", "StrictHostKeyChecking=yes", "-o", "ConnectTimeout=15", "-o", "ServerAliveInterval=20", "-o", "NumberOfPasswordPrompts=1"]);
    if let Some(p) = c.port {
        cmd.arg("-p").arg(p.to_string());
    }
    // se borran al acabar esta función, haya ido bien o no (y ya en la primera respuesta del ayudante)
    let mut _secret = None;
    let mut _key_secret = None;
    match &c.pass {
        Some(pw) => {
            let me = std::env::current_exe().map_err(|e| e.to_string())?;
            let r = resolve(c);
            let mut hosts = vec![r.hostname.clone(), c.host.clone()];
            hosts.extend(r.alias.clone());
            hosts.dedup();
            let s = write_secret(secret_dir, pw)?;
            // lo escrito sirve también, una vez, como frase de una clave cifrada que no está en el agente
            // (se usa en local para descifrarla: no sale hacia ningún servidor)
            let k = write_secret(secret_dir, pw)?;
            cmd.env("SSH_ASKPASS", me).env("SSH_ASKPASS_REQUIRE", "force").env("FB_ASKPASS_FILE", &s.0).env("FB_ASKPASS_KEYFILE", &k.0);
            cmd.env("FB_ASKPASS_USER", &r.user).env("FB_ASKPASS_HOSTS", hosts.join(" "));
            if !r.proxied {
                cmd.env("FB_ASKPASS_PLAIN", "1");
            }
            _secret = Some(s);
            _key_secret = Some(k);
            if std::env::var_os("DISPLAY").is_none() {
                cmd.env("DISPLAY", ":0");
            }
            cmd.args(["-o", "PreferredAuthentications=publickey,password,keyboard-interactive"]);
        }
        None => {
            cmd.args(["-o", "BatchMode=yes"]);
        }
    }
    cmd.arg("--").arg(ssh_target(c)).arg("sftp");
    cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    console_flags(&mut cmd, detached);
    let mut child = cmd.spawn().map_err(|e| crate::i18n::tf("No se pudo ejecutar ssh: {}", &[&e]))?;
    let (w, r, mut er) = (child.stdin.take().unwrap(), child.stdout.take().unwrap(), child.stderr.take().unwrap());
    let err = Arc::new(Mutex::new(String::new()));
    let e2 = err.clone();
    crate::vfs::thread::spawn(move || {
        let mut buf = [0u8; 1024];
        while let Ok(n) = er.read(&mut buf) {
            if n == 0 {
                break;
            }
            // se guarda lo último (lo primero suele ser el aviso de bienvenida); `clear_err` lo vacía
            let mut g = e2.lock().unwrap();
            g.push_str(&String::from_utf8_lossy(&buf[..n]));
            if g.len() > 8192 {
                let mut cut = g.len() - 8192;
                while !g.is_char_boundary(cut) {
                    cut += 1;
                }
                g.drain(..cut);
            }
        }
    });
    let child: Child = Arc::new(Mutex::new(Some(child)));
    // con contraseña, un vigilante corta el inicio de sesión si `ssh` se queda esperando una respuesta
    let timed_out = Arc::new(AtomicBool::new(false));
    let watchdog = c.pass.is_some().then(|| {
        let (tx, rx) = std::sync::mpsc::channel::<()>();
        let (ch, to) = (child.clone(), timed_out.clone());
        crate::vfs::thread::spawn(move || {
            if let Err(std::sync::mpsc::RecvTimeoutError::Timeout) = rx.recv_timeout(LOGIN_TIMEOUT) {
                to.store(true, Ordering::SeqCst);
                kill_child(&ch);
            }
        });
        tx
    });
    let res = Client::handshake(Box::new(r), Box::new(w), child, err);
    drop(watchdog);
    if timed_out.load(Ordering::SeqCst) {
        return Err(login_timeout_msg(version));
    }
    res
}

// ---------------------------------------------------------------------------
// Carpeta espejo
// ---------------------------------------------------------------------------

struct Node {
    /// ruta remota tal cual (bytes)
    remote: RPath,
    size: u64,
    mtime: i64,
    dir: bool,
    /// archivo: contenido descargado; carpeta: ya listada
    fetched: bool,
    /// enlace simbólico en el servidor: al recorrer árboles no se entra (podría ser un ciclo)
    link: bool,
}

/// Conexión abierta y la generación de credenciales con la que se abrió.
type Slot = Mutex<Option<(u64, Client)>>;

pub struct Session {
    root: PathBuf,
    conn: Mutex<Conn>,
    /// sube cada vez que cambian las credenciales: las conexiones viejas se rehacen al usarse
    epoch: std::sync::atomic::AtomicU64,
    /// navegación (listar, `realpath`)
    client: Slot,
    /// descargas: una descarga larga no bloquea la navegación
    xfer: Slot,
    nodes: Mutex<HashMap<PathBuf, Node>>,
}

static SESSIONS: OnceLock<Mutex<Vec<Arc<Session>>>> = OnceLock::new();

fn sessions() -> &'static Mutex<Vec<Arc<Session>>> {
    SESSIONS.get_or_init(|| Mutex::new(Vec::new()))
}

/// Carpeta común de los espejos; dentro, una por proceso (`p<pid>`).
fn cache_base() -> &'static Path {
    static BASE: OnceLock<PathBuf> = OnceLock::new();
    BASE.get_or_init(|| dirs::cache_dir().unwrap_or_else(crate::vfs::temp_dir).join("filebrowser").join("sftp"))
}

/// Espejos de este proceso. Se borran al salir (`shutdown`) o, si el proceso murió sin limpiar, en el
/// siguiente arranque (`cleanup_stale`): tras reiniciar, sus marcadores ya no serían «remotos» y se
/// abrirían como si fueran el archivo de verdad.
fn cache_root() -> PathBuf {
    cache_base().join(format!("p{}", crate::vfs::pid()))
}

/// Prepara la carpeta de este proceso y toma su candado (lo mantiene hasta salir). `None` si no se pudo.
#[cfg(not(target_arch = "wasm32"))]
fn mirror_root() -> Option<PathBuf> {
    static ROOT: OnceLock<Option<(PathBuf, std::fs::File)>> = OnceLock::new();
    ROOT.get_or_init(|| {
        let dir = cache_root();
        crate::fsys::private_dir(cache_base());
        crate::fsys::private_dir(&dir);
        let f = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(dir.join(".lock")).ok()?;
        f.try_lock().ok()?;
        Some((dir, f))
    })
    .as_ref()
    .map(|(d, _)| d.clone())
}

#[cfg(target_arch = "wasm32")]
fn mirror_root() -> Option<PathBuf> {
    None
}

/// Borra los espejos que ya no usa nadie: los de procesos que terminaron (su candado está libre) y
/// los de versiones anteriores (sin candado). Lento con espejos grandes: llamar en un hilo aparte.
#[cfg(not(target_arch = "wasm32"))]
pub fn cleanup_stale() {
    let base = cache_base();
    let own = format!("p{}", crate::vfs::pid());
    let Ok(rd) = std::fs::read_dir(base) else { return };
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if name == own || !e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        let p = e.path();
        let per_process = name.strip_prefix('p').is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()));
        if per_process {
            // candado tomado = el proceso dueño sigue vivo
            let free = match std::fs::OpenOptions::new().write(true).open(p.join(".lock")) {
                Ok(f) => f.try_lock().is_ok(),
                Err(_) => true,
            };
            if !free {
                continue;
            }
        }
        let _ = std::fs::remove_dir_all(&p);
    }
}

#[cfg(target_arch = "wasm32")]
pub fn cleanup_stale() {}

/// Al salir: cierra las conexiones y borra los espejos de este proceso.
pub fn shutdown() {
    let list = SESSIONS.get().map(|m| std::mem::take(&mut *m.lock().unwrap())).unwrap_or_default();
    for s in &list {
        // una descarga en curso tiene su conexión tomada: ssh termina solo al cerrarse nuestro proceso
        for slot in [&s.client, &s.xfer] {
            if let Ok(mut g) = slot.try_lock() {
                g.take();
            }
        }
    }
    drop(list);
    if mirror_root().is_some() {
        release_read_only(&cache_root());
        let _ = crate::vfs::fs::remove_dir_all(cache_root());
    }
}

/// ¿Está `p` dentro de la carpeta espejo de algún servidor? También es «remota» una copia que quedó
/// de otra ejecución: sus marcadores no tienen el contenido real y no deben abrirse como archivos.
pub fn is_remote(p: &Path) -> bool {
    session_for(p).is_some() || p.starts_with(cache_base())
}

fn session_for(p: &Path) -> Option<Arc<Session>> {
    let g = SESSIONS.get()?.lock().unwrap();
    g.iter().find(|s| p.starts_with(&s.root)).cloned()
}

/// ¿Admite el sistema de archivos local nombres que no son UTF-8? Linux y los demás Unix sí (el
/// nombre local es el mismo de bytes); Windows (UTF-16) y macOS (APFS/HFS+ exigen UTF-8) no: ahí
/// los bytes que no son UTF-8 se escriben como «%XX».
const RAW_NAMES: bool = cfg!(all(unix, not(target_vendor = "apple")));

/// Nombre local de un nombre remoto (bytes). Ver `encode_name`.
fn local_name(n: &[u8]) -> OsString {
    os_from_bytes(encode_name(n, cfg!(windows), RAW_NAMES))
}

/// Nombre remoto de un nombre local del espejo: la inversa de `local_name` (salvo el «~2» que añade
/// `local_names` a las coincidencias de mayúsculas; esos nombres se conocen por su nodo).
fn remote_name(local: &OsStr) -> RPath {
    decode_name(&os_bytes(local))
}

#[cfg(unix)]
fn os_from_bytes(v: Vec<u8>) -> OsString {
    use std::os::unix::ffi::OsStringExt;
    OsString::from_vec(v)
}

#[cfg(not(unix))]
fn os_from_bytes(v: Vec<u8>) -> OsString {
    // sin RAW_NAMES `encode_name` sólo produce UTF-8
    String::from_utf8(v).unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned()).into()
}

#[cfg(unix)]
fn os_bytes(s: &OsStr) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt;
    s.as_bytes().to_vec()
}

#[cfg(not(unix))]
fn os_bytes(s: &OsStr) -> Vec<u8> {
    s.to_string_lossy().into_owned().into_bytes()
}

fn push_esc(out: &mut Vec<u8>, x: u8) {
    out.extend_from_slice(format!("%{x:02X}").as_bytes());
}

/// Codifica un nombre remoto como nombre local. Es inyectivo y reversible (`decode_name`): lo que el
/// sistema de archivos no admite se escribe como «%XX» (con `win`, también el punto o espacio final y
/// los nombres de dispositivo; sin `raw`, los bytes que no son UTF-8), y un «%» sólo se escapa si va
/// delante de dos cifras hexadecimales (así «50%» se ve tal cual). Sin `raw` el resultado es UTF-8.
/// Las coincidencias que sólo difieren en mayúsculas (Windows, macOS) las resuelve `sync`.
fn encode_name(n: &[u8], win: bool, raw: bool) -> Vec<u8> {
    let hex2 = |i: usize| n.get(i + 1..i + 3).is_some_and(|b| b.iter().all(u8::is_ascii_hexdigit));
    let bad = |c: char| c == '/' || c == '\0' || (win && ("<>:\"|?*\\".contains(c) || (c as u32) < 32));
    let mut out = Vec::with_capacity(n.len());
    let mut i = 0;
    for chunk in n.utf8_chunks() {
        for c in chunk.valid().chars() {
            let mut b = [0u8; 4];
            let enc = c.encode_utf8(&mut b).as_bytes();
            if bad(c) || (c == '%' && hex2(i)) {
                enc.iter().for_each(|&x| push_esc(&mut out, x));
            } else {
                out.extend_from_slice(enc);
            }
            i += enc.len();
        }
        for &x in chunk.invalid() {
            if raw {
                out.push(x);
            } else {
                push_esc(&mut out, x);
            }
            i += 1;
        }
    }
    if win {
        // Win32 quita el punto o espacio final («a.» sería «a»)
        if let Some(&last) = out.last().filter(|c| matches!(c, b'.' | b' ')) {
            out.pop();
            push_esc(&mut out, last);
        }
        // CON, NUL, COM1… (con o sin extensión) son dispositivos
        let stem = out.split(|&b| b == b'.').next().unwrap_or(&[]).to_ascii_uppercase();
        let dev = matches!(stem.as_slice(), b"CON" | b"PRN" | b"AUX" | b"NUL")
            || (stem.len() == 4 && (stem.starts_with(b"COM") || stem.starts_with(b"LPT")) && stem[3].is_ascii_digit());
        if dev {
            let first = out.remove(0);
            let mut e = Vec::new();
            push_esc(&mut e, first);
            out.splice(0..0, e);
        }
    }
    out
}

/// Inversa de `encode_name`: cada «%XX» vuelve a ser su byte; un «%» que no va delante de dos cifras
/// hexadecimales se queda como está.
fn decode_name(l: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(l.len());
    let mut i = 0;
    while i < l.len() {
        if l[i] == b'%' && l.get(i + 1..i + 3).is_some_and(|b| b.iter().all(u8::is_ascii_hexdigit)) {
            let h = std::str::from_utf8(&l[i + 1..i + 3]).ok().and_then(|h| u8::from_str_radix(h, 16).ok());
            if let Some(x) = h {
                out.push(x);
                i += 3;
                continue;
            }
        }
        out.push(l[i]);
        i += 1;
    }
    out
}

/// Nombres locales para los de una carpeta remota: `local_name` y, si dos coinciden en un sistema
/// de archivos que no distingue mayúsculas, «nombre~2.ext», «nombre~3.ext»…
fn local_names(names: &[&[u8]], fold_case: bool) -> Vec<OsString> {
    local_names_with(names, fold_case, cfg!(windows), RAW_NAMES).into_iter().map(os_from_bytes).collect()
}

fn local_names_with(names: &[&[u8]], fold_case: bool, win: bool, raw: bool) -> Vec<Vec<u8>> {
    let fold = |s: &[u8]| if fold_case { String::from_utf8_lossy(s).to_lowercase().into_bytes() } else { s.to_vec() };
    // orden estable: el mismo listado da siempre los mismos nombres
    let mut order: Vec<usize> = (0..names.len()).collect();
    order.sort_by(|&a, &b| names[a].cmp(names[b]));
    let mut used = std::collections::HashSet::new();
    let mut out = vec![Vec::new(); names.len()];
    for i in order {
        let mut ln = encode_name(names[i], win, raw);
        if !used.insert(fold(&ln)) {
            let (stem, ext) = match ln.iter().rposition(|&b| b == b'.') {
                Some(d) if d > 0 => (ln[..d].to_vec(), ln[d..].to_vec()),
                _ => (ln.clone(), Vec::new()),
            };
            let mut k = 2u32;
            loop {
                let mut cand = stem.clone();
                cand.extend_from_slice(format!("~{k}").as_bytes());
                cand.extend_from_slice(&ext);
                if used.insert(fold(&cand)) {
                    ln = cand;
                    break;
                }
                k += 1;
            }
        }
        out[i] = ln;
    }
    out
}

/// Crea el marcador de un archivo remoto: vacío pero con el tamaño y la fecha reales (el listado los
/// lee del disco). Es disperso: no ocupa espacio. En Windows hay que marcarlo como disperso antes de
/// fijar el tamaño (si no, NTFS reserva el tamaño entero); si el volumen no lo admite (FAT) se deja
/// vacío y se verá con tamaño 0 hasta descargarlo.
fn placeholder(lp: &Path, size: u64, mtime: i64) {
    if let Ok(f) = crate::vfs::fs::File::create(lp) {
        #[cfg(windows)]
        let sparse = set_sparse(&f);
        #[cfg(not(windows))]
        let sparse = true;
        if sparse {
            let _ = f.set_len(size);
        }
        let _ = f.set_modified(web_time::UNIX_EPOCH + std::time::Duration::from_secs(mtime.max(0) as u64));
    }
}

#[cfg(windows)]
fn set_sparse(f: &std::fs::File) -> bool {
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::System::IO::DeviceIoControl;
    const FSCTL_SET_SPARSE: u32 = 0x0009_00C4;
    let mut ret = 0u32;
    // SAFETY: el HANDLE es el del archivo abierto `f`, válido durante la llamada; sin búferes de
    // entrada ni salida (FSCTL_SET_SPARSE sin argumento = marcar disperso) y sin E/S solapada.
    unsafe { DeviceIoControl(HANDLE(f.as_raw_handle()), FSCTL_SET_SPARSE, None, 0, None, 0, Some(&mut ret), None).is_ok() }
}

impl Session {
    fn new(root: PathBuf, conn: Conn) -> Session {
        Session {
            root,
            conn: Mutex::new(conn),
            epoch: std::sync::atomic::AtomicU64::new(0),
            client: Mutex::new(None),
            xfer: Mutex::new(None),
            nodes: Mutex::new(HashMap::new()),
        }
    }

    /// Ejecuta `f` con la conexión de `slot` (la abre o la rehace si hace falta). Sólo se toma el
    /// candado de esa conexión: nunca el de la lista de sesiones.
    ///
    /// Con `cancel`, un vigilante mata el `ssh` en cuanto se cancela: una lectura bloqueada en un
    /// servidor que no responde vuelve enseguida (y entonces no se reconecta). Un servidor colgado sin
    /// cancelar lo corta el propio `ssh` (ServerAliveInterval × ServerAliveCountMax, ~1 min).
    fn with_slot<T>(&self, slot: &Slot, cancel: Option<&AtomicBool>, mut f: impl FnMut(&mut Client) -> Result<T, String>) -> Result<T, String> {
        let cancelled = || cancel.is_some_and(|c| c.load(Ordering::Relaxed));
        let mut g = slot.lock().unwrap_or_else(|e| e.into_inner());
        for attempt in 0..2 {
            if cancelled() {
                return Err(crate::i18n::t("Operación cancelada").into());
            }
            let epoch = self.epoch.load(Ordering::SeqCst);
            if g.as_ref().map(|(g0, _)| *g0 != epoch).unwrap_or(true) {
                *g = None;
                let conn = self.conn.lock().unwrap().clone();
                let dir = self.root.parent().map(Path::to_path_buf).unwrap_or_else(cache_root);
                *g = Some((epoch, spawn_ssh(&conn, &dir)?));
            }
            let client = &mut g.as_mut().unwrap().1;
            let r = match cancel {
                None => f(client),
                Some(flag) => {
                    let child = client.child.clone();
                    let done = AtomicBool::new(false);
                    std::thread::scope(|sc| {
                        let w = sc.spawn(|| {
                            while !done.load(Ordering::Relaxed) {
                                if flag.load(Ordering::Relaxed) {
                                    kill_child(&child);
                                    break;
                                }
                                std::thread::park_timeout(std::time::Duration::from_millis(150));
                            }
                        });
                        let r = f(client);
                        done.store(true, Ordering::Relaxed);
                        w.thread().unpark();
                        r
                    })
                }
            };
            match r {
                Ok(v) => return Ok(v),
                Err(_) if cancelled() => {
                    *g = None;
                    return Err(crate::i18n::t("Operación cancelada").into());
                }
                // un error de E/S suele ser conexión caída: se reconecta una vez
                Err(e) if attempt == 0 && !e.starts_with(crate::i18n::t("no existe")) && !e.starts_with(crate::i18n::t("permiso denegado")) => {
                    *g = None;
                }
                Err(e) => return Err(e),
            }
        }
        Err(crate::i18n::t("Se perdió la conexión con el servidor").into())
    }

    fn with_client<T>(&self, f: impl FnMut(&mut Client) -> Result<T, String>) -> Result<T, String> {
        self.with_slot(&self.client, None, f)
    }

    fn remote_of(&self, local: &Path) -> Option<RPath> {
        if local == self.root {
            return Some(b"/".to_vec());
        }
        self.nodes.lock().unwrap().get(local).map(|n| n.remote.clone())
    }

    fn sync(&self, dir: &Path) -> Result<(), String> {
        let Some(remote) = self.remote_of(dir) else { return Ok(()) };
        let ents = self.with_client(|c| c.list(&remote))?;
        let _ = crate::vfs::fs::create_dir_all(dir);
        let names: Vec<&[u8]> = ents.iter().map(|e| e.name.as_slice()).collect();
        let lnames = local_names(&names, cfg!(any(windows, target_os = "macos")));
        let mut nodes = self.nodes.lock().unwrap();
        for (e, ln) in ents.iter().zip(&lnames) {
            let lp = dir.join(ln);
            let rp = rjoin(&remote, &e.name);
            if e.attrs.is_dir() {
                let _ = crate::vfs::fs::create_dir_all(&lp);
                // ya listada antes: se conserva (su contenido sigue en el espejo)
                let listed = nodes.get(&lp).is_some_and(|n| n.dir && n.remote == rp && n.fetched);
                nodes.insert(lp, Node { remote: rp, size: 0, mtime: e.attrs.mtime, dir: true, fetched: listed, link: e.link });
            } else {
                let same = nodes.get(&lp).map(|n| !n.dir && n.remote == rp && n.size == e.attrs.size && n.mtime == e.attrs.mtime).unwrap_or(false)
                    && crate::vfs::fs::symlink_metadata(&lp).is_ok();
                if !same {
                    make_writable(&lp);
                    placeholder(&lp, e.attrs.size, e.attrs.mtime);
                    nodes.insert(lp, Node { remote: rp, size: e.attrs.size, mtime: e.attrs.mtime, dir: false, fetched: false, link: e.link });
                }
            }
        }
        if let Some(n) = nodes.get_mut(dir) {
            n.fetched = true;
        }
        // lo que ya no está en el servidor
        if let Ok(rd) = crate::vfs::fs::read_dir(dir) {
            for e in rd.flatten() {
                let n = e.file_name();
                if !lnames.contains(&n) && !n.to_string_lossy().ends_with(".fb_part") {
                    let p = e.path();
                    nodes.retain(|k, _| !k.starts_with(&p));
                    release_read_only(&p);
                    if e.file_type().map(|t| t.v_is_dir()).unwrap_or(false) {
                        let _ = crate::vfs::fs::remove_dir_all(&p);
                    } else {
                        let _ = crate::vfs::fs::remove_file(&p);
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
    /// archivo donde se guardan (el primero de UserKnownHostsFile según `ssh -G`)
    file: Option<PathBuf>,
}

impl PendingKey {
    /// Datos de ejemplo para las capturas de prueba.
    #[cfg(feature = "selftest")]
    pub fn sample() -> PendingKey {
        PendingKey {
            host: "servidor.ejemplo.org".into(),
            port: Some(2222),
            fingerprints: vec!["256 SHA256:RCgIfsoyy+AtpGpjF4bsr9HfIc1MMo0N26m101FB8hc (ED25519)".into(), "3072 SHA256:u2kXr0p9mYvC1nQeLzTb5aVhWd8JsGfN4oRiE7yKcXA (RSA)".into()],
            scan: String::new(),
            file: None,
        }
    }
}

static PENDING: Mutex<Option<PendingKey>> = Mutex::new(None);

pub fn take_pending_key() -> Option<PendingKey> {
    PENDING.lock().ok()?.take()
}

fn hide_console(cmd: &mut std::process::Command) {
    console_flags(cmd, false);
}

/// En Windows: sin ventana de consola (CREATE_NO_WINDOW) o, con `detached`, sin consola ninguna
/// (DETACHED_PROCESS). En otros sistemas no hace nada.
fn console_flags(cmd: &mut std::process::Command, detached: bool) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        cmd.creation_flags(if detached { DETACHED_PROCESS } else { CREATE_NO_WINDOW });
    }
    let _ = (cmd, detached);
}

fn known_hosts_path() -> Option<PathBuf> {
    Some(dirs::home_dir()?.join(".ssh").join("known_hosts"))
}

/// Nombre con el que `ssh` busca el servidor en known_hosts: el HostKeyAlias tal cual o el nombre
/// real, con «[nombre]:puerto» si el puerto no es el 22.
fn known_entry(r: &Resolved) -> String {
    match &r.alias {
        Some(a) => a.clone(),
        None if r.port != 22 && r.port != 0 => format!("[{}]:{}", r.hostname, r.port),
        None => r.hostname.clone(),
    }
}

/// Cambia el nombre de cada línea de `ssh-keyscan` por `entry`.
fn rename_scan(scan: &str, entry: &str) -> String {
    scan.lines()
        .filter(|l| !l.starts_with('#') && !l.trim().is_empty())
        .filter_map(|l| l.split_once(' ').map(|(_, rest)| format!("{entry} {rest}")))
        .collect::<Vec<_>>()
        .join("\n")
}

/// ¿Está ya el servidor en algún known_hosts (del usuario o del sistema)? Si no, deja su clave
/// pendiente y devuelve `HOSTKEY`. Alias, HostName, Port y HostKeyAlias de ssh_config se resuelven
/// con `ssh -G`, igual que hará `ssh` al conectar.
fn check_host(conn: &Conn) -> Result<(), String> {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let r = resolve(conn);
    let entry = known_entry(&r);
    for f in r.known.iter().filter(|f| f.v_is_file()) {
        let mut c = Command::new("ssh-keygen");
        c.arg("-F").arg(&entry).arg("-f").arg(f).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null());
        hide_console(&mut c);
        if let Ok(o) = c.output()
            && o.status.success()
            && !o.stdout.is_empty()
        {
            return Ok(());
        }
    }
    // servidor nuevo: se pide su clave pública y se calcula la huella para enseñársela al usuario
    let mut k = Command::new("ssh-keyscan");
    k.args(["-T", "10"]);
    k.arg("-p").arg(r.port.to_string());
    k.arg("--").arg(r.hostname.trim_start_matches('[').trim_end_matches(']')).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    hide_console(&mut k);
    let out = k.output().map_err(|e| format!("ssh-keyscan: {e}"))?;
    let scan = rename_scan(&String::from_utf8_lossy(&out.stdout), &entry);
    if scan.trim().is_empty() {
        if r.proxied {
            // detrás de un salto no se llega directamente: la clave tiene que aceptarse con ssh
            return Err(crate::i18n::t("No se pudo obtener la clave del servidor (se llega a través de otro): conecta una vez con ssh desde un terminal para aceptarla").into());
        }
        // sin respuesta: casi siempre el servidor no existe, no responde o no tiene SSH en ese puerto
        let why = String::from_utf8_lossy(&out.stderr).lines().map(str::trim).rfind(|l| !l.is_empty() && !l.starts_with('#')).map(|l| format!(" ({l})")).unwrap_or_default();
        return Err(crate::i18n::tf("No se puede llegar al servidor {}:{}: no existe, no responde o no tiene SSH en ese puerto{}", &[&r.hostname, &r.port, &why]));
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
        return Err(crate::i18n::t("No se pudo calcular la huella del servidor").into());
    }
    let file = r.user_known.first().cloned();
    *PENDING.lock().unwrap() = Some(PendingKey { host: conn.host.clone(), port: conn.port, fingerprints, scan, file });
    Err(HOSTKEY.into())
}

/// El usuario ha comprobado la huella: se guarda la clave en el known_hosts del usuario que usa
/// `ssh` (el primero de UserKnownHostsFile; si no se sabe, ~/.ssh/known_hosts).
pub fn trust_host(k: &PendingKey) -> Result<(), String> {
    use std::io::Write;
    let path = k.file.clone().or_else(known_hosts_path).ok_or(crate::i18n::t("No se encontró la carpeta personal"))?;
    if let Some(d) = path.parent() {
        crate::fsys::private_dir(d);
    }
    let mut f = crate::vfs::fs::OpenOptions::new().create(true).append(true).open(&path).map_err(|e| e.to_string())?;
    // si el archivo no acaba en salto de línea, se añade para no pegar la clave a la anterior
    let needs_nl = crate::vfs::fs::read(&path).map(|b| !b.is_empty() && b.last() != Some(&b'\n')).unwrap_or(false);
    if needs_nl {
        f.write_all(b"\n").map_err(|e| e.to_string())?;
    }
    f.write_all(k.scan.as_bytes()).and_then(|_| f.write_all(b"\n")).map_err(|e| e.to_string())
}

/// Conecta con un servidor y devuelve la carpeta local que refleja su carpeta personal (o `path`).
/// `path` es la ruta remota tal cual (bytes; vacía o «/»: la carpeta personal).
pub fn connect(conn: Conn, path: &[u8]) -> Result<PathBuf, String> {
    check_host(&conn)?;
    let base = mirror_root().ok_or(crate::i18n::t("No se pudo preparar la carpeta local del servidor"))?;
    let key = format!("{}@{}{}", if conn.user.is_empty() { "_" } else { &conn.user }, conn.host, conn.port.map(|p| format!("_{p}")).unwrap_or_default());
    let root = base.join(local_name(key.as_bytes()));
    let sess = {
        let mut g = sessions().lock().unwrap();
        match g.iter().find(|s| s.root == root) {
            Some(s) => s.clone(),
            None => {
                let s = Arc::new(Session::new(root.clone(), conn.clone()));
                g.push(s.clone());
                s
            }
        }
    }; // la lista se suelta aquí: nunca se espera a una conexión con ella tomada
    // nuevas credenciales: las conexiones abiertas (aunque estén ocupadas descargando) se rehacen al
    // volver a usarse, sin esperarlas aquí
    *sess.conn.lock().unwrap() = conn.clone();
    sess.epoch.fetch_add(1, Ordering::SeqCst);
    crate::fsys::private_dir(&root);
    let start = if path.is_empty() || path == b"/" {
        sess.with_client(|c| c.realpath(b"."))?
    } else {
        path.to_vec()
    };
    // se abre la ruta carpeta a carpeta para tener sus nodos registrados
    let mut cur = root.clone();
    sess.sync(&cur)?;
    for part in start.split(|&b| b == b'/').filter(|s| !s.is_empty()) {
        // un «..» (del servidor o de la ruta escrita) sacaría el espejo de su carpeta y
        // la sincronización podría truncar o borrar archivos locales
        if part == b".." {
            return Err(crate::i18n::t("Ruta no válida").into());
        }
        if part == b"." {
            continue;
        }
        // el remoto se calcula antes de tomar el candado: remote_of lo toma también
        let remote = rjoin(&sess.remote_of(&cur).unwrap_or_default(), part);
        let mut nodes = sess.nodes.lock().unwrap();
        // la carpeta puede tener ya nombre local (de una sincronización, quizá con «~2»)
        let known = nodes.iter().find(|(k, n)| n.dir && n.remote == remote && k.parent() == Some(cur.as_path())).map(|(k, _)| k.clone());
        let next = match known {
            Some(k) => k,
            None => {
                let k = cur.join(local_name(part));
                nodes.entry(k.clone()).or_insert(Node { remote, size: 0, mtime: 0, dir: true, fetched: false, link: false });
                k
            }
        };
        drop(nodes);
        cur = next;
        let _ = crate::vfs::fs::create_dir_all(&cur);
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

/// Refleja `p` y todas sus subcarpetas (para conocer el tamaño real antes de copiar). Devuelve un
/// error por cada carpeta que no se pudo listar (quedaría vacía en la copia) o que no se recorre: un
/// enlace a una carpeta que la contiene sería un ciclo sin fin.
pub fn sync_tree(p: &Path, cancel: &AtomicBool) -> Vec<String> {
    let mut errs = Vec::new();
    let Some(s) = session_for(p) else { return errs };
    if !p.v_is_dir() {
        return errs;
    }
    let Some(remote) = s.remote_of(p) else { return errs };
    // ruta real (sin enlaces) para reconocer los ciclos
    let real = s.with_client(|c| c.realpath(&remote)).unwrap_or(remote);
    s.sync_rec(p, &real, 0, cancel, &mut errs);
    errs
}

impl Session {
    fn sync_rec(&self, dir: &Path, real: &[u8], depth: usize, cancel: &AtomicBool, errs: &mut Vec<String>) {
        if cancel.load(Ordering::Relaxed) {
            return;
        }
        let shown = || self.remote_of(dir).map(|r| show(&r)).unwrap_or_else(|| dir.display().to_string());
        if let Err(e) = self.sync(dir) {
            errs.push(format!("{}: {e}", shown()));
            return;
        }
        if depth >= 64 {
            errs.push(crate::i18n::tf("{}: demasiados niveles de carpetas", &[&shown()]));
            return;
        }
        let Ok(rd) = crate::vfs::fs::read_dir(dir) else { return };
        let mut subs: Vec<PathBuf> = rd.flatten().filter(|e| e.file_type().map(|t| t.v_is_dir()).unwrap_or(false)).map(|e| e.path()).collect();
        subs.sort();
        for sub in subs {
            let (remote, link) = match self.nodes.lock().unwrap().get(&sub) {
                Some(n) => (n.remote.clone(), n.link),
                None => continue,
            };
            let sub_real = if link {
                match self.with_client(|c| c.realpath(&remote)) {
                    Ok(r) => r,
                    Err(e) => {
                        errs.push(format!("{}: {e}", show(&remote)));
                        continue;
                    }
                }
            } else {
                rjoin(real, remote.rsplit(|&b| b == b'/').next().unwrap_or(&[]))
            };
            // ¿`real` es `sub_real` o está dentro?
            if link && (real == sub_real.as_slice() || real.starts_with(&rjoin(&sub_real, b""))) {
                errs.push(crate::i18n::tf("{}: enlace a una carpeta que lo contiene; no se recorre", &[&show(&remote)]));
                continue;
            }
            self.sync_rec(&sub, &sub_real, depth + 1, cancel, errs);
        }
    }
}

/// Asegura que el archivo `p` tiene su contenido real (lo descarga si sólo es un marcador).
pub fn ensure_local(p: &Path) -> Result<(), String> {
    ensure_local_with(p, &AtomicBool::new(false), &mut |_| {})
}

pub fn ensure_local_with(p: &Path, cancel: &AtomicBool, progress: &mut dyn FnMut(u64)) -> Result<(), String> {
    let Some(s) = session_for(p) else {
        // marcador que quedó de otra ejecución: no tiene el contenido real
        if p.starts_with(cache_base()) && !p.v_is_dir() {
            return Err(crate::i18n::t("Copia local de un servidor SFTP que ya no está conectado").into());
        }
        return Ok(());
    };
    let (remote, size, mtime) = {
        let g = s.nodes.lock().unwrap();
        match g.get(p) {
            Some(n) if !n.dir && !n.fetched => (n.remote.clone(), n.size, n.mtime),
            Some(_) => return Ok(()),
            // sin nodo: no se sabe qué archivo remoto es (no hay contenido real que dar)
            None if p == s.root || p.v_is_dir() => return Ok(()),
            None => return Err(crate::i18n::t("Archivo del servidor desconocido: vuelve a abrir la carpeta").into()),
        }
    };
    // conexión propia de las descargas: navegar mientras tanto no espera a que termine
    s.with_slot(&s.xfer, Some(cancel), |c| c.download(&remote, p, size, cancel, progress))?;
    let _ = crate::vfs::fs::File::options().write(true).open(p).and_then(|f| f.set_modified(web_time::UNIX_EPOCH + std::time::Duration::from_secs(mtime.max(0) as u64)));
    if let Some(n) = s.nodes.lock().unwrap().get_mut(p) {
        n.fetched = true;
    }
    Ok(())
}

/// ¿Tiene `p` (archivo, o carpeta con todo su contenido) los datos reales? Lo que no es de un
/// servidor SFTP siempre los tiene.
pub fn tree_fetched(p: &Path) -> bool {
    let Some(s) = session_for(p) else { return !p.starts_with(cache_base()) };
    let g = s.nodes.lock().unwrap();
    match g.get(p) {
        Some(n) if !n.dir => n.fetched,
        Some(n) if !n.fetched => false,
        _ => !g.iter().any(|(k, n)| k.starts_with(p) && k != p && !n.fetched),
    }
}

/// Descarga lo que falte de `srcs` (archivos o carpetas enteras) con avance en bytes en `done` sobre
/// `total`. Devuelve un error por elemento fallido; con `cancel` se detiene enseguida.
pub fn fetch_all(srcs: &[PathBuf], cancel: &AtomicBool, total: &std::sync::atomic::AtomicU64, done: &std::sync::atomic::AtomicU64, label: &Mutex<String>) -> Vec<String> {
    let mut errs = Vec::new();
    let mut files: Vec<(PathBuf, u64)> = Vec::new();
    for src in srcs {
        let Some(s) = session_for(src) else { continue };
        errs.extend(sync_tree(src, cancel));
        let g = s.nodes.lock().unwrap();
        files.extend(g.iter().filter(|(k, n)| k.starts_with(src) && !n.dir && !n.fetched).map(|(k, n)| (k.clone(), n.size)));
    }
    files.sort();
    files.dedup();
    total.store(files.iter().fold(0u64, |a, (_, n)| a.saturating_add(*n)).max(1), Ordering::Relaxed);
    let mut base = 0u64;
    for (f, size) in files {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        *label.lock().unwrap() = f.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        if let Err(e) = ensure_local_with(&f, cancel, &mut |n| done.store(base + n.min(size), Ordering::Relaxed)) {
            if cancel.load(Ordering::Relaxed) {
                break;
            }
            let shown = remote_label(&f).unwrap_or_else(|| f.display().to_string());
            errs.push(format!("{shown}: {e}"));
        }
        base = base.saturating_add(size);
        done.store(base, Ordering::Relaxed);
    }
    errs
}

/// Bytes que faltan por descargar de `srcs` (lo ya reflejado; ver `sync_tree`).
pub fn missing_bytes(srcs: &[PathBuf]) -> u64 {
    let mut n = 0u64;
    for src in srcs {
        let Some(s) = session_for(src) else { continue };
        let g = s.nodes.lock().unwrap();
        n = g.iter().filter(|(k, v)| k.starts_with(src) && !v.dir && !v.fetched).fold(n, |a, (_, v)| a.saturating_add(v.size));
    }
    n
}

/// Ruta remota para una dirección sftp://: «%» como «%25» y los bytes que no son UTF-8 como «%XX»
/// (`netmount` los descodifica a los mismos bytes al volver a conectar).
fn pct_path(b: &[u8]) -> String {
    let mut out = String::with_capacity(b.len());
    for chunk in b.utf8_chunks() {
        out.push_str(&chunk.valid().replace('%', "%25"));
        for x in chunk.invalid() {
            out.push_str(&format!("%{x:02X}"));
        }
    }
    out
}

/// Dirección «sftp://usuario@servidor:puerto/ruta» de `p`, si está en el espejo de un servidor
/// conectado: es lo que se enseña y lo que se guarda (la carpeta espejo cambia en cada ejecución).
pub fn remote_label(p: &Path) -> Option<String> {
    let s = session_for(p)?;
    let r = match s.remote_of(p) {
        Some(r) => r,
        None => {
            let parent = s.remote_of(p.parent()?)?;
            rjoin(&parent, &remote_name(p.file_name()?))
        }
    };
    let c = s.conn.lock().unwrap();
    Some(format!("sftp://{}{}", authority(&c), pct_path(&r)))
}

/// «usuario@servidor:puerto» de una dirección (IPv6 entre corchetes).
fn authority(c: &Conn) -> String {
    let host = if c.host.contains(':') && !c.host.starts_with('[') { format!("[{}]", c.host) } else { c.host.clone() };
    let user = if c.user.is_empty() { String::new() } else { format!("{}@", c.user.replace('%', "%25").replace('@', "%40").replace(':', "%3A")) };
    format!("{user}{host}{}", c.port.map(|p| format!(":{p}")).unwrap_or_default())
}

/// Ruta para guardar (favoritos, recientes, pestañas): la dirección sftp:// si `p` es de un servidor.
pub fn persist_path(p: &Path) -> PathBuf {
    remote_label(p).map(PathBuf::from).unwrap_or_else(|| p.to_path_buf())
}

/// Servidores conectados ahora: («usuario@servidor», carpeta espejo de su raíz).
pub fn live_sessions() -> Vec<(String, PathBuf)> {
    let Some(m) = SESSIONS.get() else { return Vec::new() };
    m.lock().unwrap().iter().map(|s| (authority(&s.conn.lock().unwrap()), s.root.clone())).collect()
}

/// ¿Es `p` de un servidor conectado ahora (no una copia vieja)?
pub fn is_live(p: &Path) -> bool {
    session_for(p).is_some()
}

/// Raíz del espejo del servidor de `p` y su nombre («usuario@servidor (SFTP)») para la ruta y el título.
pub fn session_root(p: &Path) -> Option<(PathBuf, String)> {
    let s = session_for(p)?;
    let a = authority(&s.conn.lock().unwrap());
    Some((s.root.clone(), format!("{a} (SFTP)")))
}

/// Cierra la conexión con un servidor y borra su carpeta espejo.
pub fn disconnect(root: &Path) {
    let gone = {
        let Some(m) = SESSIONS.get() else { return };
        let mut g = m.lock().unwrap();
        let i = g.iter().position(|s| s.root == root);
        i.map(|i| g.remove(i))
    };
    let Some(s) = gone else { return };
    for slot in [&s.client, &s.xfer] {
        // una descarga en curso tiene su conexión tomada: termina con error al borrarse su carpeta
        if let Ok(mut g) = slot.try_lock() {
            g.take();
        }
    }
    s.nodes.lock().unwrap().clear();
    release_read_only(root);
    let _ = crate::vfs::fs::remove_dir_all(root);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::{channel, Receiver, Sender};

    #[test]
    fn askpass_prompts() {
        let hosts = vec!["srv.example.org".to_string(), "srv".to_string()];
        assert!(prompt_matches("ana@srv.example.org's password: ", "ana", &hosts, false));
        assert!(prompt_matches("(ana@srv) Password: ", "ana", &hosts, false));
        assert!(prompt_matches("Ana@SRV's password:", "ana", &hosts, false));
        // otro usuario u otro servidor (p. ej. el salto de ProxyJump): no se responde
        assert!(!prompt_matches("root@srv.example.org's password: ", "ana", &hosts, false));
        assert!(!prompt_matches("ana@bastion's password: ", "ana", &hosts, false));
        // frase de una clave, código de verificación, aceptar la huella
        assert!(!prompt_matches("Enter passphrase for key '/home/ana/.ssh/id_ed25519': ", "ana", &hosts, true));
        assert!(!prompt_matches("Verification code: ", "ana", &hosts, true));
        assert!(!prompt_matches("Are you sure you want to continue connecting (yes/no/[fingerprint])? ", "ana", &hosts, true));
        // «Password:» a secas sólo sin saltos intermedios
        assert!(prompt_matches("Password: ", "ana", &hosts, true));
        assert!(!prompt_matches("Password: ", "ana", &hosts, false));
        assert!(is_passphrase_prompt("Enter passphrase for key '/home/ana/.ssh/id_ed25519': "));
        assert!(!is_passphrase_prompt("ana@srv's password: "));
        assert!(!prompt_matches("@'s password: ", "", &[String::new()], false));
    }

    #[test]
    fn askpass_secret_once() {
        let dir = std::env::temp_dir().join(format!("fb_askpass_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let s = write_secret(&dir, "clave secreta").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&s.0).unwrap().permissions().mode() & 0o777, 0o600);
        }
        assert_eq!(take_secret(&s.0).as_deref(), Some("clave secreta"));
        // sólo sirve una vez
        assert!(take_secret(&s.0).is_none());
        let s2 = write_secret(&dir, "x").unwrap();
        let p2 = s2.0.clone();
        drop(s2);
        assert!(!p2.exists());
        drop(s);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ssh_g_parse() {
        let out = "user ana\nhostname 10.0.0.5\nport 2222\nhostkeyalias none\nproxyjump bastion\nuserknownhostsfile ~/.ssh/known_hosts ~/.ssh/known_hosts2\nglobalknownhostsfile /etc/ssh/ssh_known_hosts\n";
        let r = parse_resolved(out, Some(Path::new("/h")));
        assert_eq!((r.user.as_str(), r.hostname.as_str(), r.port), ("ana", "10.0.0.5", 2222));
        assert!(r.alias.is_none() && r.proxied);
        assert_eq!(r.known, vec![PathBuf::from("/h/.ssh/known_hosts"), PathBuf::from("/h/.ssh/known_hosts2"), PathBuf::from("/etc/ssh/ssh_known_hosts")]);
        assert_eq!(r.user_known, vec![PathBuf::from("/h/.ssh/known_hosts"), PathBuf::from("/h/.ssh/known_hosts2")]);
        assert_eq!(known_entry(&r), "[10.0.0.5]:2222");
        let r = parse_resolved("hostname h\nport 22\nhostkeyalias mialias\nproxycommand none\n", None);
        assert!(!r.proxied);
        assert_eq!(known_entry(&r), "mialias");
        assert_eq!(known_entry(&parse_resolved("hostname h\nport 22\n", None)), "h");
        assert_eq!(rename_scan("# h:22 SSH-2.0\nh ssh-ed25519 AAAA\n\n10.0.0.5 ssh-rsa BBBB", "[x]:2"), "[x]:2 ssh-ed25519 AAAA\n[x]:2 ssh-rsa BBBB");
    }

    #[test]
    fn local_names_injective() {
        let ln = |n: &str| String::from_utf8(encode_name(n.as_bytes(), cfg!(windows), RAW_NAMES)).unwrap();
        let names = ["a_b", "a/b", "a%2Fb", "a%b", "%", "%25", "x", "x\0"];
        let lns: Vec<String> = names.iter().map(|n| ln(n)).collect();
        let set: std::collections::HashSet<&String> = lns.iter().collect();
        assert_eq!(set.len(), names.len(), "{lns:?}");
        assert!(lns.iter().all(|n| !n.contains('/') && !n.contains('\0')));
        assert_eq!(ln("normal.txt"), "normal.txt");
        // un «%» suelto se ve tal cual
        assert_eq!(ln("50% off"), "50% off");
        assert_eq!(ln("%41"), "%2541");
        assert_eq!(local_name(b"normal.txt"), OsString::from("normal.txt"));
        let w = |n: &str| encode_name(n.as_bytes(), true, false);
        assert_ne!(w("a."), w("a"));
        assert_ne!(w("con.txt").to_ascii_uppercase(), b"CON.TXT".to_vec());
        assert_ne!(w("a:b"), w("a_b"));
        // sin distinguir mayúsculas: «A.txt» y «a.txt» no pueden ser el mismo archivo local
        let v = local_names(&[b"a.txt", b"A.txt", b"b"], true);
        // (orden por nombre: «A.txt» va antes y conserva su nombre)
        assert_eq!(v, vec![OsString::from("a~2.txt"), OsString::from("A.txt"), OsString::from("b")]);
        let v = local_names(&[b"a.txt", b"A.txt"], false);
        assert_eq!(v, vec![OsString::from("a.txt"), OsString::from("A.txt")]);
    }

    /// Nombres que no son UTF-8 (SFTP v3 los da como bytes): ida y vuelta exacta en todos los modos
    /// (Linux: bytes tal cual; macOS: sin bytes sueltos; Windows: además caracteres y nombres prohibidos).
    #[test]
    fn local_names_bytes_round_trip() {
        let names: Vec<&[u8]> = vec![
            b"caf\xe9.txt",                // Latin-1
            b"\xff\xfe",
            b"\x82\xa0\x82\xa2.txt",      // Shift-JIS
            b"a%FF",                       // «%FF» de verdad, no el byte 0xFF
            b"a\xff",
            b"%",
            b"%%2F",
            b"%4\xff",
            b"%A%",
            b"x%",
            b"a/b",
            b"a%2Fb",
            b"con.txt",
            b"COM1",
            b"a.",
            b"a ",
            b"a:b*?\\",
            b"\xc3",                        // UTF-8 cortado
            "ñandú €".as_bytes(),
            b"normal",
        ];
        for (win, raw) in [(false, true), (false, false), (true, false)] {
            let enc: Vec<Vec<u8>> = names.iter().map(|n| encode_name(n, win, raw)).collect();
            for (n, e) in names.iter().zip(&enc) {
                assert_eq!(&decode_name(e), n, "win={win} raw={raw} {e:?}");
                assert!(!e.contains(&b'/') && !e.contains(&0));
                if !raw {
                    assert!(std::str::from_utf8(e).is_ok(), "{e:?}");
                }
                if win {
                    let s = std::str::from_utf8(e).unwrap();
                    assert!(!s.ends_with(['.', ' ']) && !s.contains(['<', '>', ':', '"', '|', '?', '*', '\\']));
                }
            }
            let set: std::collections::HashSet<&Vec<u8>> = enc.iter().collect();
            assert_eq!(set.len(), names.len(), "win={win} raw={raw}");
            // sin coincidencias de mayúsculas, `local_names_with` no añade nada
            assert_eq!(local_names_with(&names, false, win, raw), enc);
        }
        // Linux: el nombre local son los mismos bytes
        assert_eq!(encode_name(b"caf\xe9", false, true), b"caf\xe9".to_vec());
        assert_eq!(encode_name(b"caf\xe9", true, false), b"caf%E9".to_vec());
        // en esta plataforma: local_name y remote_name son inversas
        for n in &names {
            assert_eq!(&remote_name(&local_name(n)), n);
        }
        // rutas remotas y su dirección sftp://
        assert_eq!(rjoin(b"/", b"x"), b"/x".to_vec());
        assert_eq!(rjoin(b"/home/u/", b"\xff"), b"/home/u/\xff".to_vec());
        assert_eq!(pct_path(b"/a/50%/caf\xe9"), "/a/50%25/caf%E9");
    }

    #[test]
    fn ssh_versions() {
        assert_eq!(parse_ssh_version("OpenSSH_for_Windows_8.1p1, LibreSSL 3.0.2\r\n"), Some((8, 1)));
        assert_eq!(parse_ssh_version("OpenSSH_9.6p1 Ubuntu-3ubuntu13.5, OpenSSL 3.0.13 30 Jan 2024"), Some((9, 6)));
        assert_eq!(parse_ssh_version("OpenSSH_8.4p1 Debian-5, OpenSSL 1.1.1n"), Some((8, 4)));
        assert_eq!(parse_ssh_version("ssh: command not found"), None);
        assert!(askpass_require_unsupported(Some((8, 1))));
        assert!(askpass_require_unsupported(Some((7, 9))));
        assert!(!askpass_require_unsupported(Some((8, 4))));
        assert!(!askpass_require_unsupported(Some((10, 0))));
        assert!(!askpass_require_unsupported(None));
        assert!(login_timeout_msg(Some((8, 1))).contains("8.1"));
    }

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
        crate::vfs::thread::spawn(move || {
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
                                    o.extend_from_slice(&4u32.to_be_bytes());
                                    for (name, a) in [
                                        (&b"big.bin"[..], attrs(content.len() as u64, 0o100644, 1_700_000_000)),
                                        (b"sub", attrs(0, 0o040755, 1_700_000_001)),
                                        (b"caf\xe9.txt", attrs(3, 0o100644, 0)),
                                        (b".", attrs(0, 0o040755, 0)),
                                    ] {
                                        put_str(&mut o, name);
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
        assert_eq!(c.realpath(b".").unwrap(), b"/home/u");
        let l = c.list(b"/home/u").unwrap();
        assert_eq!(l.len(), 3);
        assert!(l.iter().any(|e| e.name == b"sub" && e.attrs.is_dir()));
        // nombre que no es UTF-8: llega con sus bytes
        assert!(l.iter().any(|e| e.name == b"caf\xe9.txt"));
        let big = l.iter().find(|e| e.name == b"big.bin").unwrap();
        assert_eq!(big.attrs.size, 300_000);
        assert_eq!(big.attrs.mtime, 1_700_000_000);
        let dst = crate::vfs::temp_dir().join(format!("fb_sftp_{}.bin", crate::vfs::pid()));
        let mut last = 0;
        c.download(b"/home/u/big.bin", &dst, 300_000, &AtomicBool::new(false), &mut |n| last = n).unwrap();
        assert_eq!(crate::vfs::fs::read(&dst).unwrap(), content);
        assert_eq!(last, 300_000);
        // los mensajes de ssh: sólo la última línea, con explicación si hace falta
        assert_eq!(explain_ssh_err("Bienvenido\nssh: connect to host x port 22: Connection refused\n").as_deref(), Some("ssh: connect to host x port 22: Connection refused"));
        assert!(explain_ssh_err("@@@ WARNING: REMOTE HOST IDENTIFICATION HAS CHANGED! @@@\nHost key verification failed.\n").unwrap().contains("ssh-keygen -R"));
        assert!(explain_ssh_err("").is_none());
        let _ = crate::vfs::fs::remove_file(&dst);
    }
}
