//! `std::fs` en el navegador: los archivos son los del equipo que sirve la página, y se piden a
//! `filebrowser-server` con XHR síncrono (el programa espera la respuesta, como con un disco).
//! Las rutas son las virtuales del servidor: `/` lista las carpetas compartidas.
use std::cell::RefCell;
use std::collections::HashMap;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;
use web_time::{SystemTime, UNIX_EPOCH};

fn err(kind: io::ErrorKind, msg: &str) -> io::Error {
    io::Error::new(kind, msg.to_string())
}

fn not_found() -> io::Error {
    err(io::ErrorKind::NotFound, "no existe")
}

// ---------- tipos ----------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FileType {
    dir: bool,
}

impl FileType {
    pub fn is_dir(&self) -> bool {
        self.dir
    }
    pub fn is_file(&self) -> bool {
        !self.dir
    }
    pub fn is_symlink(&self) -> bool {
        false
    }
}

#[derive(Clone, Debug)]
pub struct Permissions {
    readonly: bool,
}

impl Permissions {
    pub fn readonly(&self) -> bool {
        self.readonly
    }
    pub fn set_readonly(&mut self, r: bool) {
        self.readonly = r;
    }
}

#[derive(Clone, Debug)]
pub struct Metadata {
    dir: bool,
    len: u64,
    mtime: i64,
}

impl Metadata {
    pub fn len(&self) -> u64 {
        self.len
    }
    pub fn is_dir(&self) -> bool {
        self.dir
    }
    pub fn is_file(&self) -> bool {
        !self.dir
    }
    pub fn is_symlink(&self) -> bool {
        false
    }
    pub fn file_type(&self) -> FileType {
        FileType { dir: self.dir }
    }
    pub fn permissions(&self) -> Permissions {
        Permissions { readonly: !crate::vfs::net::writable() }
    }
    pub fn modified(&self) -> io::Result<SystemTime> {
        if self.mtime <= 0 {
            return Err(err(io::ErrorKind::Unsupported, "sin fecha"));
        }
        Ok(UNIX_EPOCH + Duration::from_secs(self.mtime as u64))
    }
    pub fn accessed(&self) -> io::Result<SystemTime> {
        self.modified()
    }
    pub fn created(&self) -> io::Result<SystemTime> {
        Err(err(io::ErrorKind::Unsupported, "sin fecha de creación"))
    }
}

pub struct DirEntry {
    path: PathBuf,
    md: Metadata,
}

impl DirEntry {
    pub fn path(&self) -> PathBuf {
        self.path.clone()
    }
    pub fn file_name(&self) -> std::ffi::OsString {
        self.path.file_name().unwrap_or_default().to_os_string()
    }
    pub fn metadata(&self) -> io::Result<Metadata> {
        Ok(self.md.clone())
    }
    pub fn file_type(&self) -> io::Result<FileType> {
        Ok(self.md.file_type())
    }
}

pub struct ReadDir(std::vec::IntoIter<DirEntry>);

impl Iterator for ReadDir {
    type Item = io::Result<DirEntry>;
    fn next(&mut self) -> Option<Self::Item> {
        self.0.next().map(Ok)
    }
}

// ---------- carpeta local en memoria ----------

/// `/.fb`: temporales, caché y ajustes del propio navegador. Nunca sale al servidor.
pub const LOCAL: &str = ".fb";

#[derive(Default)]
struct Mem {
    files: std::collections::BTreeMap<String, (Vec<u8>, i64)>,
    dirs: std::collections::BTreeSet<String>,
}

thread_local! {
    static MEM: RefCell<Mem> = RefCell::new(Mem::default());
}

fn is_local(v: &str) -> bool {
    v == LOCAL || v.strip_prefix(LOCAL).is_some_and(|r| r.starts_with('/'))
}

fn now_secs() -> i64 {
    (crate::vfs::net::now()) as i64
}

fn mem_meta(v: &str) -> Option<Metadata> {
    MEM.with_borrow(|m| {
        if v == LOCAL || m.dirs.contains(v) {
            Some(Metadata { dir: true, len: 0, mtime: 0 })
        } else {
            m.files.get(v).map(|(d, t)| Metadata { dir: false, len: d.len() as u64, mtime: *t })
        }
    })
}

fn mem_list(v: &str) -> Vec<(String, Metadata)> {
    MEM.with_borrow(|m| {
        let mut out = Vec::new();
        for d in &m.dirs {
            if parent_of(d) == v {
                out.push((d.rsplit('/').next().unwrap_or("").to_string(), Metadata { dir: true, len: 0, mtime: 0 }));
            }
        }
        for (f, (data, t)) in &m.files {
            if parent_of(f) == v {
                out.push((f.rsplit('/').next().unwrap_or("").to_string(), Metadata { dir: false, len: data.len() as u64, mtime: *t }));
            }
        }
        out
    })
}

fn mem_op(op: &str, a: &str, b: Option<&str>) -> io::Result<()> {
    MEM.with_borrow_mut(|m| match op {
        "mkdir" => {
            if m.dirs.contains(a) || m.files.contains_key(a) {
                return Err(err(io::ErrorKind::AlreadyExists, "ya existe"));
            }
            m.dirs.insert(a.to_string());
            Ok(())
        }
        "mkdirs" => {
            let mut cur = String::new();
            for c in a.split('/') {
                if !cur.is_empty() {
                    cur.push('/');
                }
                cur.push_str(c);
                if cur != LOCAL {
                    m.dirs.insert(cur.clone());
                }
            }
            Ok(())
        }
        "rm" => m.files.remove(a).map(|_| ()).ok_or_else(not_found),
        "rmdir" | "rmrf" => {
            let pre = format!("{a}/");
            m.files.retain(|k, _| !k.starts_with(&pre));
            m.dirs.retain(|k| k != a && !k.starts_with(&pre));
            Ok(())
        }
        "mv" | "cp" => {
            let b = b.ok_or_else(not_found)?;
            let data = m.files.get(a).cloned().ok_or_else(not_found)?;
            m.files.insert(b.to_string(), (data.0, now_secs()));
            if op == "mv" {
                m.files.remove(a);
            }
            Ok(())
        }
        _ => Err(err(io::ErrorKind::Unsupported, "operación desconocida")),
    })
}

/// Carpeta de temporales (en memoria).
pub fn temp_dir() -> PathBuf {
    let _ = mem_op("mkdirs", ".fb/tmp", None);
    PathBuf::from("/.fb/tmp")
}

// ---------- listados con caché ----------

struct Listing {
    at: f64,
    rows: Vec<(String, Metadata)>,
}

thread_local! {
    static LISTINGS: RefCell<HashMap<String, Listing>> = RefCell::new(HashMap::new());
}

/// Cuánto vale un listado ya pedido (segundos): los `metadata` de cada entrada salen de él.
const TTL: f64 = 3.0;

/// La ruta tal y como la entiende el servidor: sin la barra inicial ni barras dobles.
fn virt(p: &Path) -> String {
    let s = p.to_string_lossy();
    s.split('/').filter(|c| !c.is_empty() && *c != ".").collect::<Vec<_>>().join("/")
}

fn forget(path: &str) {
    // cualquier cambio invalida el listado de la carpeta y el de la de arriba
    LISTINGS.with_borrow_mut(|m| {
        m.remove(path);
        m.remove(parent_of(path));
    });
}

fn parent_of(path: &str) -> &str {
    path.rsplit_once('/').map(|(a, _)| a).unwrap_or("")
}

fn listing(path: &str) -> io::Result<Vec<(String, Metadata)>> {
    if is_local(path) {
        return Ok(mem_list(path));
    }
    let now = crate::vfs::net::now();
    if let Some(rows) = LISTINGS.with_borrow(|m| m.get(path).filter(|l| now - l.at < TTL).map(|l| l.rows.clone())) {
        return Ok(rows);
    }
    let rows = crate::vfs::net::list(path)?;
    let rows: Vec<(String, Metadata)> = rows.into_iter().map(|(n, d, s, m)| (n, Metadata { dir: d, len: s, mtime: m })).collect();
    LISTINGS.with_borrow_mut(|m| m.insert(path.to_string(), Listing { at: now, rows: rows.clone() }));
    Ok(rows)
}

// ---------- funciones ----------

pub fn metadata<P: AsRef<Path>>(p: P) -> io::Result<Metadata> {
    let v = virt(p.as_ref());
    if v.is_empty() {
        return Ok(Metadata { dir: true, len: 0, mtime: 0 });
    }
    if is_local(&v) {
        return mem_meta(&v).ok_or_else(not_found);
    }
    let name = v.rsplit('/').next().unwrap_or("").to_string();
    listing(parent_of(&v))?.into_iter().find(|(n, _)| *n == name).map(|(_, m)| m).ok_or_else(not_found)
}

pub fn symlink_metadata<P: AsRef<Path>>(p: P) -> io::Result<Metadata> {
    metadata(p)
}

pub fn read_dir<P: AsRef<Path>>(p: P) -> io::Result<ReadDir> {
    let path = p.as_ref();
    let v = virt(path);
    if !metadata(path)?.is_dir() {
        return Err(err(io::ErrorKind::NotADirectory, "no es una carpeta"));
    }
    let rows = listing(&v)?;
    let base = Path::new("/").join(&v);
    Ok(ReadDir(rows.into_iter().map(|(n, md)| DirEntry { path: base.join(n), md }).collect::<Vec<_>>().into_iter()))
}

pub fn canonicalize<P: AsRef<Path>>(p: P) -> io::Result<PathBuf> {
    let v = virt(p.as_ref());
    metadata(Path::new(&v))?;
    Ok(Path::new("/").join(v))
}

pub fn read<P: AsRef<Path>>(p: P) -> io::Result<Vec<u8>> {
    let mut f = File::open(p)?;
    let mut v = Vec::with_capacity(f.size as usize);
    f.read_to_end(&mut v)?;
    Ok(v)
}

pub fn read_to_string<P: AsRef<Path>>(p: P) -> io::Result<String> {
    String::from_utf8(read(p)?).map_err(|_| err(io::ErrorKind::InvalidData, "no es UTF-8"))
}

pub fn read_link<P: AsRef<Path>>(_p: P) -> io::Result<PathBuf> {
    Err(err(io::ErrorKind::InvalidInput, "no es un enlace"))
}

pub fn write<P: AsRef<Path>, C: AsRef<[u8]>>(p: P, contents: C) -> io::Result<()> {
    let v = virt(p.as_ref());
    store(&v, contents.as_ref())?;
    Ok(())
}

/// Guarda un archivo entero: en memoria si es de la carpeta local, y si no, en el servidor.
fn store(v: &str, data: &[u8]) -> io::Result<()> {
    if is_local(v) {
        if !MEM.with_borrow(|m| m.dirs.contains(parent_of(v)) || parent_of(v) == LOCAL) {
            return Err(not_found());
        }
        MEM.with_borrow_mut(|m| m.files.insert(v.to_string(), (data.to_vec(), now_secs())));
        return Ok(());
    }
    crate::vfs::net::put(v, data)?;
    forget(v);
    Ok(())
}

fn op(name: &str, from: &Path, to: Option<&Path>) -> io::Result<()> {
    let (a, b) = (virt(from), to.map(virt));
    if is_local(&a) {
        mem_op(name, &a, b.as_deref())?;
        return Ok(());
    }
    crate::vfs::net::op(name, &a, b.as_deref())?;
    forget(&a);
    if let Some(b) = b {
        forget(&b);
    }
    Ok(())
}

pub fn create_dir<P: AsRef<Path>>(p: P) -> io::Result<()> {
    op("mkdir", p.as_ref(), None)
}

pub fn create_dir_all<P: AsRef<Path>>(p: P) -> io::Result<()> {
    if metadata(p.as_ref()).is_ok_and(|m| m.is_dir()) {
        return Ok(());
    }
    op("mkdirs", p.as_ref(), None)
}

pub fn remove_file<P: AsRef<Path>>(p: P) -> io::Result<()> {
    op("rm", p.as_ref(), None)
}

pub fn remove_dir<P: AsRef<Path>>(p: P) -> io::Result<()> {
    op("rmdir", p.as_ref(), None)
}

pub fn remove_dir_all<P: AsRef<Path>>(p: P) -> io::Result<()> {
    op("rmrf", p.as_ref(), None)
}

pub fn rename<P: AsRef<Path>, Q: AsRef<Path>>(from: P, to: Q) -> io::Result<()> {
    op("mv", from.as_ref(), Some(to.as_ref()))
}

pub fn copy<P: AsRef<Path>, Q: AsRef<Path>>(from: P, to: Q) -> io::Result<u64> {
    let len = metadata(from.as_ref())?.len();
    op("cp", from.as_ref(), Some(to.as_ref()))?;
    Ok(len)
}

pub fn set_permissions<P: AsRef<Path>>(_p: P, _perm: Permissions) -> io::Result<()> {
    Ok(())
}

// ---------- File ----------

/// Un archivo del servidor. Se lee por trozos (`Range`); lo que se escribe se guarda en memoria y
/// se envía entero al cerrarlo.
pub struct File {
    path: String,
    size: u64,
    pos: u64,
    cache: (u64, Vec<u8>),
    out: Option<Vec<u8>>,
    /// contenido ya en memoria (archivos de la carpeta local)
    mem: Option<Vec<u8>>,
}

const CHUNK: u64 = 512 * 1024;

impl File {
    pub fn open<P: AsRef<Path>>(p: P) -> io::Result<File> {
        let md = metadata(p.as_ref())?;
        if md.is_dir() {
            return Err(err(io::ErrorKind::IsADirectory, "es una carpeta"));
        }
        let path = virt(p.as_ref());
        let mem = is_local(&path).then(|| MEM.with_borrow(|m| m.files.get(&path).map(|f| f.0.clone()))).flatten();
        Ok(File { path, size: md.len(), pos: 0, cache: (0, Vec::new()), out: None, mem })
    }

    pub fn create<P: AsRef<Path>>(p: P) -> io::Result<File> {
        let path = virt(p.as_ref());
        if !is_local(&path) && !crate::vfs::net::writable() {
            return Err(err(io::ErrorKind::PermissionDenied, "el servidor es de solo lectura"));
        }
        Ok(File { path, size: 0, pos: 0, cache: (0, Vec::new()), out: Some(Vec::new()), mem: None })
    }

    pub fn options() -> OpenOptions {
        OpenOptions::new()
    }

    pub fn metadata(&self) -> io::Result<Metadata> {
        Ok(Metadata { dir: false, len: self.out.as_ref().map(|o| o.len() as u64).unwrap_or(self.size), mtime: 0 })
    }

    pub fn set_len(&self, _n: u64) -> io::Result<()> {
        Err(err(io::ErrorKind::Unsupported, "no disponible"))
    }

    pub fn set_modified(&self, _t: SystemTime) -> io::Result<()> {
        Ok(())
    }

    pub fn set_permissions(&self, _p: Permissions) -> io::Result<()> {
        Ok(())
    }

    pub fn sync_all(&self) -> io::Result<()> {
        Ok(())
    }

    pub fn try_clone(&self) -> io::Result<File> {
        Err(err(io::ErrorKind::Unsupported, "no disponible"))
    }

    fn send(&mut self) -> io::Result<()> {
        if let Some(buf) = self.out.take() {
            let r = store(&self.path, &buf);
            self.out = Some(buf);
            r?;
        }
        Ok(())
    }
}

impl Drop for File {
    fn drop(&mut self) {
        let _ = self.send();
    }
}

impl Read for File {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() || self.pos >= self.size {
            return Ok(0);
        }
        if let Some(m) = &self.mem {
            let off = (self.pos as usize).min(m.len());
            let n = buf.len().min(m.len() - off);
            buf[..n].copy_from_slice(&m[off..off + n]);
            self.pos += n as u64;
            return Ok(n);
        }
        let (start, ref data) = self.cache;
        if self.pos < start || self.pos >= start + data.len() as u64 {
            let len = CHUNK.max(buf.len() as u64).min(self.size - self.pos);
            self.cache = (self.pos, crate::vfs::net::range(&self.path, self.pos, len)?);
        }
        let (start, ref data) = self.cache;
        let off = (self.pos - start) as usize;
        let n = buf.len().min(data.len().saturating_sub(off));
        buf[..n].copy_from_slice(&data[off..off + n]);
        self.pos += n as u64;
        Ok(n)
    }
}

impl Read for &File {
    fn read(&mut self, _buf: &mut [u8]) -> io::Result<usize> {
        Err(err(io::ErrorKind::Unsupported, "no disponible"))
    }
}

impl Seek for File {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        let end = self.out.as_ref().map(|o| o.len() as u64).unwrap_or(self.size);
        let p = match to {
            SeekFrom::Start(n) => n as i128,
            SeekFrom::End(d) => end as i128 + d as i128,
            SeekFrom::Current(d) => self.pos as i128 + d as i128,
        };
        if p < 0 {
            return Err(err(io::ErrorKind::InvalidInput, "posición negativa"));
        }
        self.pos = p as u64;
        Ok(self.pos)
    }
}

impl Write for File {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let Some(out) = self.out.as_mut() else {
            return Err(err(io::ErrorKind::PermissionDenied, "abierto solo para leer"));
        };
        out.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[derive(Clone, Debug, Default)]
pub struct OpenOptions {
    write: bool,
}

impl OpenOptions {
    pub fn new() -> OpenOptions {
        OpenOptions::default()
    }
    pub fn read(&mut self, _r: bool) -> &mut OpenOptions {
        self
    }
    pub fn write(&mut self, w: bool) -> &mut OpenOptions {
        self.write |= w;
        self
    }
    pub fn append(&mut self, a: bool) -> &mut OpenOptions {
        self.write |= a;
        self
    }
    pub fn create(&mut self, _c: bool) -> &mut OpenOptions {
        self
    }
    pub fn create_new(&mut self, _c: bool) -> &mut OpenOptions {
        self
    }
    pub fn truncate(&mut self, _t: bool) -> &mut OpenOptions {
        self
    }
    pub fn open<P: AsRef<Path>>(&self, p: P) -> io::Result<File> {
        if self.write { File::create(p) } else { File::open(p) }
    }
}

impl symphonia::core::io::MediaSource for File {
    fn is_seekable(&self) -> bool {
        true
    }

    fn byte_len(&self) -> Option<u64> {
        Some(self.size)
    }
}
