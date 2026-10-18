//! Capa de sistema de archivos: listado en streaming, ordenación y operaciones.
#[allow(unused_imports)]
use crate::vfs::{KindV as _, PathV as _};
#[cfg(target_os = "android")]
use crate::trash;
use crate::i18n::{t, tf};
use std::cmp::Ordering;
use crate::vfs::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering as AO};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::{Duration};
use web_time::{Instant, UNIX_EPOCH};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum Kind {
    Folder,
    Image,
    Video,
    Audio,
    Text,
    Archive,
    Doc,
    Other,
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Kind::Folder => t("Carpeta"),
            Kind::Image => t("Imagen"),
            Kind::Video => t("Vídeo"),
            Kind::Audio => t("Audio"),
            Kind::Text => t("Texto / código"),
            Kind::Archive => t("Comprimido"),
            Kind::Doc => t("Documento"),
            Kind::Other => t("Archivo"),
        }
    }
}

pub const F_DIR: u8 = 1;
pub const F_HIDDEN: u8 = 2;
pub const F_LINK: u8 = 4;

#[derive(Clone)]
pub struct Entry {
    pub name: Box<str>,
    pub size: u64,
    pub mtime: i64,
    pub kind: Kind,
    pub flags: u8,
}

impl Entry {
    #[inline]
    pub fn is_dir(&self) -> bool {
        self.flags & F_DIR != 0
    }
    #[inline]
    pub fn is_hidden(&self) -> bool {
        self.flags & F_HIDDEN != 0
    }
    pub fn ext(&self) -> &str {
        match self.name.rsplit_once('.') {
            Some((stem, e)) if !stem.is_empty() && e.len() <= 8 => e,
            _ => "",
        }
    }
}

pub fn kind_of_ext(ext: &str) -> Kind {
    let mut buf = [0u8; 8];
    if ext.is_empty() || ext.len() > 8 {
        return Kind::Other;
    }
    for (i, b) in ext.bytes().enumerate() {
        buf[i] = b.to_ascii_lowercase();
    }
    let e = std::str::from_utf8(&buf[..ext.len()]).unwrap_or("");
    match e {
        "jpg" | "jpeg" | "jpe" | "png" | "gif" | "webp" | "bmp" | "ico" | "tif" | "tiff" | "svg"
        | "avif" | "heic" | "heif" | "raw" | "cr2" | "cr3" | "nef" | "nrw" | "arw" | "srf" | "sr2" | "dng" | "orf" | "rw2"
        | "pef" | "raf" | "srw" | "3fr" | "erf" | "kdc" | "mrw" | "x3f" | "psd" | "psb" | "xcf" | "blend" | "blend1" => {
            Kind::Image
        }
        "mp4" | "mkv" | "avi" | "mov" | "webm" | "wmv" | "flv" | "m4v" | "mpg" | "mpeg" | "ts" => {
            Kind::Video
        }
        "mp3" | "flac" | "wav" | "ogg" | "opus" | "m4a" | "aac" | "wma" | "aiff" => Kind::Audio,
        "txt" | "md" | "rs" | "toml" | "json" | "yaml" | "yml" | "xml" | "html" | "htm" | "css"
        | "js" | "jsx" | "tsx" | "py" | "c" | "h" | "cpp" | "hpp" | "cc" | "java" | "go" | "sh"
        | "bat" | "ini" | "cfg" | "conf" | "log" | "csv" | "sql" | "lua" | "php" | "rb" | "cs"
        | "kt" | "swift" | "lock" | "gitignore" | "env" | "tex" | "srt" => Kind::Text,
        "zip" | "rar" | "7z" | "tar" | "gz" | "bz2" | "xz" | "zst" | "iso" | "deb" | "rpm"
        | "tgz" | "jar" => Kind::Archive,
        "pdf" | "doc" | "docx" | "odt" | "xls" | "xlsx" | "ods" | "ppt" | "pptx" | "odp" | "rtf"
        | "epub" | "cbz" | "cbr" => Kind::Doc,
        _ => Kind::Other,
    }
}

// ---------------------------------------------------------------------------
// Carga en streaming
// ---------------------------------------------------------------------------

pub enum LoadMsg {
    Chunk(u64, Vec<Entry>),
    Done(u64, Option<String>),
}

#[cfg(windows)]
fn is_hidden_meta(_name: &str, md: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    md.file_attributes() & 0x2 != 0
}
#[cfg(not(windows))]
fn is_hidden_meta(name: &str, _md: &fs::Metadata) -> bool {
    name.starts_with('.')
}

pub fn spawn_load(
    dir: PathBuf,
    generation: u64,
    current: Arc<AtomicU64>,
    tx: Sender<LoadMsg>,
    ctx: egui::Context,
) {
    crate::vfs::thread::spawn(move || {
        // carpeta espejo de un servidor SFTP: se refleja antes de leerla
        if let Err(e) = crate::sftp::sync_dir(&dir) {
            if !dir.v_exists() {
                let _ = tx.send(LoadMsg::Done(generation, Some(e)));
                ctx.request_repaint();
                return;
            }
        }
        let rd = match fs::read_dir(&dir) {
            Ok(r) => r,
            Err(e) => {
                let _ = tx.send(LoadMsg::Done(generation, Some(e.to_string())));
                ctx.request_repaint();
                return;
            }
        };
        // Convención freedesktop: los nombres listados en `.hidden` se ocultan.
        // solo un archivo normal y pequeño (una FIFO o /dev/zero bloquearía o agotaría la memoria)
        let hidden_list: std::collections::HashSet<String> = {
            use std::io::Read;
            let hp = dir.join(".hidden");
            match fs::metadata(&hp) {
                Ok(m) if m.v_is_file() => {
                    let mut s = String::new();
                    let _ = fs::File::open(&hp).and_then(|f| f.take(1 << 20).read_to_string(&mut s));
                    s.lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect()
                }
                _ => Default::default(),
            }
        };
        let mut buf: Vec<Entry> = Vec::with_capacity(1024);
        let mut last = Instant::now();
        let mut target = 400usize;
        for de in rd {
            if current.load(AO::Relaxed) != generation {
                return;
            }
            let Ok(de) = de else { continue };
            let name = de.file_name().to_string_lossy().into_owned();
            let Ok(md) = de.metadata() else {
                continue;
            };
            let mut flags = 0u8;
            let mut md_eff = md;
            if md_eff.file_type().v_is_symlink() {
                flags |= F_LINK;
                if let Ok(m) = fs::metadata(de.path()) {
                    md_eff = m;
                }
            }
            if md_eff.v_is_dir() {
                flags |= F_DIR;
            }
            if is_hidden_meta(&name, &md_eff) || (!hidden_list.is_empty() && hidden_list.contains(&name)) {
                flags |= F_HIDDEN;
            }
            let kind = if flags & F_DIR != 0 {
                Kind::Folder
            } else {
                match name.rsplit_once('.') {
                    Some((s, e)) if !s.is_empty() => kind_of_ext(e),
                    _ => Kind::Other,
                }
            };
            let mtime = md_eff
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            buf.push(Entry {
                name: name.into_boxed_str(),
                size: if flags & F_DIR != 0 { 0 } else { md_eff.len() },
                mtime,
                kind,
                flags,
            });
            if buf.len() >= target || (last.elapsed() > Duration::from_millis(40) && !buf.is_empty())
            {
                target = 20_000;
                let chunk = std::mem::replace(&mut buf, Vec::with_capacity(4096));
                if tx.send(LoadMsg::Chunk(generation, chunk)).is_err() {
                    return;
                }
                ctx.request_repaint();
                last = Instant::now();
            }
        }
        if !buf.is_empty() {
            let _ = tx.send(LoadMsg::Chunk(generation, buf));
        }
        let _ = tx.send(LoadMsg::Done(generation, None));
        ctx.request_repaint();
    });
}

/// Construye una entrada a partir de una ruta; `name` es lo que se guarda como nombre
/// (ruta relativa en la vista recursiva, ruta absoluta en los resultados de búsqueda).
pub fn entry_from_path(path: &Path, name: String) -> Option<Entry> {
    let md = fs::symlink_metadata(path).ok()?;
    let mut flags = 0u8;
    let mut md_eff = md;
    if md_eff.file_type().v_is_symlink() {
        flags |= F_LINK;
        if let Ok(m) = fs::metadata(path) {
            md_eff = m;
        }
    }
    if md_eff.v_is_dir() {
        flags |= F_DIR;
    }
    let base = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    if is_hidden_meta(&base, &md_eff) {
        flags |= F_HIDDEN;
    }
    let kind = if flags & F_DIR != 0 {
        Kind::Folder
    } else {
        match base.rsplit_once('.') {
            Some((s, e)) if !s.is_empty() => kind_of_ext(e),
            _ => Kind::Other,
        }
    };
    let mtime = md_eff
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    Some(Entry {
        name: name.into_boxed_str(),
        size: if flags & F_DIR != 0 { 0 } else { md_eff.len() },
        mtime,
        kind,
        flags,
    })
}

pub const DEEP_MAX: usize = 300_000;

/// Listado recursivo: todos los elementos de `dir` y sus subcarpetas, con nombres relativos.
pub fn spawn_load_deep(dir: PathBuf, hidden: bool, generation: u64, current: Arc<AtomicU64>, tx: Sender<LoadMsg>, ctx: egui::Context) {
    crate::vfs::thread::spawn(move || {
        let mut stack: Vec<PathBuf> = vec![dir.clone()];
        let mut buf: Vec<Entry> = Vec::with_capacity(1024);
        let mut total = 0usize;
        let mut last = Instant::now();
        let mut target = 400usize;
        'outer: while let Some(d) = stack.pop() {
            let Ok(rd) = fs::read_dir(&d) else { continue };
            for de in rd.flatten() {
                if current.load(AO::Relaxed) != generation {
                    return;
                }
                let name = de.file_name().to_string_lossy().into_owned();
                if !hidden && name.starts_with('.') {
                    continue;
                }
                let p = de.path();
                let Ok(rel) = p.strip_prefix(&dir) else { continue };
                let Some(e) = entry_from_path(&p, rel.to_string_lossy().replace('\\', "/")) else { continue };
                if de.file_type().map(|t| t.v_is_dir()).unwrap_or(false) {
                    stack.push(p);
                }
                buf.push(e);
                total += 1;
                if total >= DEEP_MAX {
                    break 'outer;
                }
                if buf.len() >= target || (last.elapsed() > Duration::from_millis(60) && !buf.is_empty()) {
                    target = 10_000;
                    let chunk = std::mem::replace(&mut buf, Vec::with_capacity(4096));
                    if tx.send(LoadMsg::Chunk(generation, chunk)).is_err() {
                        return;
                    }
                    ctx.request_repaint();
                    last = Instant::now();
                }
            }
        }
        if !buf.is_empty() {
            let _ = tx.send(LoadMsg::Chunk(generation, buf));
        }
        let _ = tx.send(LoadMsg::Done(generation, None));
        ctx.request_repaint();
    });
}

// ---------------------------------------------------------------------------
// Ordenación / filtro (en hilo aparte, sólo índices u32)
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SortKey {
    Name,
    Date,
    Size,
    Type,
}

impl SortKey {
    pub fn from_str(s: &str) -> SortKey {
        match s {
            "date" => SortKey::Date,
            "size" => SortKey::Size,
            "type" => SortKey::Type,
            _ => SortKey::Name,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            SortKey::Name => "name",
            SortKey::Date => "date",
            SortKey::Size => "size",
            SortKey::Type => "type",
        }
    }
}

/// Comparación "natural" insensible a mayúsculas (file2 < file10).
pub fn natural_cmp(a: &str, b: &str) -> Ordering {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        let (ca, cb) = (a[i], b[j]);
        if ca.is_ascii_digit() && cb.is_ascii_digit() {
            let si = i;
            while i < a.len() && a[i] == b'0' {
                i += 1;
            }
            let sj = j;
            while j < b.len() && b[j] == b'0' {
                j += 1;
            }
            let (ni, nj) = (i, j);
            while i < a.len() && a[i].is_ascii_digit() {
                i += 1;
            }
            while j < b.len() && b[j].is_ascii_digit() {
                j += 1;
            }
            let (la, lb) = (i - ni, j - nj);
            if la != lb {
                return la.cmp(&lb);
            }
            match a[ni..i].cmp(&b[nj..j]) {
                Ordering::Equal => {}
                o => return o,
            }
            let _ = (si, sj);
        } else {
            let (la, lb) = (ca.to_ascii_lowercase(), cb.to_ascii_lowercase());
            if la != lb {
                return la.cmp(&lb);
            }
            i += 1;
            j += 1;
        }
    }
    (a.len() - i).cmp(&(b.len() - j))
}

/// Contiene `needle` (ya en minúsculas) ignorando mayúsculas, sin reservar memoria en ASCII.
pub fn contains_ci(hay: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return true;
    }
    if hay.is_ascii() && needle.is_ascii() {
        let (h, n) = (hay.as_bytes(), needle.as_bytes());
        if n.len() > h.len() {
            return false;
        }
        'o: for s in 0..=h.len() - n.len() {
            for k in 0..n.len() {
                if h[s + k].to_ascii_lowercase() != n[k] {
                    continue 'o;
                }
            }
            return true;
        }
        false
    } else {
        hay.to_lowercase().contains(needle)
    }
}

pub struct SortSpec {
    pub key: SortKey,
    pub asc: bool,
    pub show_hidden: bool,
    pub filter: String,
}

pub fn build_order(entries: &[Entry], spec: &SortSpec) -> Vec<u32> {
    let needle = spec.filter.to_lowercase();
    let mut order: Vec<u32> = (0..entries.len() as u32)
        .filter(|&i| {
            let e = &entries[i as usize];
            (spec.show_hidden || !e.is_hidden()) && (needle.is_empty() || contains_ci(&e.name, &needle))
        })
        .collect();
    let asc = spec.asc;
    let key = spec.key;
    order.sort_unstable_by(|&ia, &ib| {
        let (a, b) = (&entries[ia as usize], &entries[ib as usize]);
        // Carpetas siempre primero
        match b.is_dir().cmp(&a.is_dir()) {
            Ordering::Equal => {}
            o => return o,
        }
        let o = match key {
            SortKey::Name => natural_cmp(&a.name, &b.name),
            SortKey::Date => a.mtime.cmp(&b.mtime).then_with(|| natural_cmp(&a.name, &b.name)),
            SortKey::Size => a.size.cmp(&b.size).then_with(|| natural_cmp(&a.name, &b.name)),
            SortKey::Type => (a.kind as u8)
                .cmp(&(b.kind as u8))
                .then_with(|| a.ext().to_ascii_lowercase().cmp(&b.ext().to_ascii_lowercase()))
                .then_with(|| natural_cmp(&a.name, &b.name)),
        };
        if asc { o } else { o.reverse() }
    });
    order
}

// ---------------------------------------------------------------------------
// Operaciones de archivos en segundo plano
// ---------------------------------------------------------------------------

#[derive(Default)]
pub struct Progress {
    pub total: AtomicU64,
    pub done: AtomicU64,
    pub finished: AtomicBool,
    pub label: Mutex<String>,
    pub errors: Mutex<Vec<String>>,
    pub created: Mutex<Vec<String>>,
    pub cancel: AtomicBool,
    /// (origen, destino) de cada elemento procesado con éxito; sirve para deshacer.
    pub pairs: Mutex<Vec<(PathBuf, PathBuf)>>,
}

impl Progress {
    pub fn cancelled(&self) -> bool {
        self.cancel.load(AO::Relaxed)
    }
}

/// Qué hacer cuando el destino ya existe.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Policy {
    Rename,
    Replace,
    Skip,
}

pub enum Op {
    Copy { srcs: Vec<PathBuf>, dst: PathBuf, mv: bool, policy: Policy },
    Trash(Vec<PathBuf>),
    Delete(Vec<PathBuf>),
    Compress { srcs: Vec<PathBuf>, out: PathBuf, opts: crate::tools::CompressOpts },
    Unzip { src: PathBuf, dst: PathBuf, pass: Option<String> },
}

/// Crea una carpeta de datos privada (caché, índice): en unix solo la lee su usuario.
pub fn private_dir(p: &Path) {
    let _ = fs::create_dir_all(p);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(p, fs::Permissions::from_mode(0o700));
    }
}

pub(crate) fn size_of_tree(p: &Path) -> u64 {
    match fs::symlink_metadata(p) {
        Ok(m) if m.v_is_dir() => fs::read_dir(p)
            .map(|rd| rd.flatten().fold(0u64, |a, e| a.saturating_add(size_of_tree(&e.path()))))
            .unwrap_or(0)
            .saturating_add(1),
        Ok(m) => m.len().max(1),
        Err(_) => 0,
    }
}

/// Nombre temporal junto al destino para copiar antes de reemplazar.
fn continue_with_tmp(dir: &Path, name: &str) -> PathBuf {
    // nombre impredecible: nadie puede dejar ahí un enlace simbólico a la espera
    let n = web_time::SystemTime::now().duration_since(web_time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    dir.join(format!(".fbrep-{}-{n:x}-{name}", crate::vfs::pid()))
}

pub fn unique_path(dir: &Path, name: &str) -> PathBuf {
    let first = dir.join(name);
    if fs::symlink_metadata(&first).is_err() {
        return first;
    }
    let (stem, ext) = match name.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() => (s, format!(".{e}")),
        _ => (name, String::new()),
    };
    for n in 2.. {
        let p = dir.join(format!("{stem} ({n}){ext}"));
        if fs::symlink_metadata(&p).is_err() {
            return p;
        }
    }
    unreachable!()
}

/// Copia un archivo por bloques, avisando del progreso en bytes y atendiendo a la cancelación.
/// Permisos y fecha se copian "si se puede": muchos sistemas de red/FAT no los admiten y eso no
/// debe convertir una copia correcta en un error.
fn copy_file(src: &Path, dst: &Path, prog: &Progress, buf: &mut Vec<u8>) -> std::io::Result<()> {
    use std::io::{Read, Write};
    // archivo de un servidor SFTP: primero se descarga
    if let Err(e) = crate::sftp::ensure_local_with(src, &prog.cancel, &mut |_| {}) {
        return Err(std::io::Error::other(e));
    }
    let mut r = fs::File::open(src)?;
    let md = r.metadata()?;
    let mut w = fs::File::create(dst)?;
    // En Linux se prueba primero copy_file_range (copia en el kernel o en el servidor en NFS/SMB);
    // si el sistema de archivos no lo admite (FUSE/GVFS…) se cae a un bucle con búfer grande.
    let _ = &buf;
    let mut copied = 0u64;
    #[cfg(target_os = "linux")]
    let mut kernel = true;
    let mut chunk: Vec<u8> = Vec::new();
    loop {
        if prog.cancelled() {
            return Err(std::io::Error::new(std::io::ErrorKind::Interrupted, "cancelado"));
        }
        #[cfg(target_os = "linux")]
        if kernel {
            use std::os::fd::AsRawFd;
            let n = unsafe {
                libc::copy_file_range(r.as_raw_fd(), std::ptr::null_mut(), w.as_raw_fd(), std::ptr::null_mut(), 64 << 20, 0)
            };
            if n > 0 {
                copied += n as u64;
                prog.done.fetch_add(n as u64, AO::Relaxed);
                continue;
            } else if n == 0 && copied > 0 {
                break;
            }
            // 0 sin haber copiado nada o error: se usa la copia normal desde la posición actual
            kernel = false;
            if n < 0 {
                let e = std::io::Error::last_os_error();
                if e.kind() == std::io::ErrorKind::Interrupted {
                    kernel = true;
                    continue;
                }
            }
        }
        if chunk.is_empty() {
            chunk.resize(4 << 20, 0);
        }
        let n = match r.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        w.write_all(&chunk[..n])?;
        copied += n as u64;
        prog.done.fetch_add(n as u64, AO::Relaxed);
    }
    if copied == 0 {
        prog.done.fetch_add(1, AO::Relaxed);
    }
    let _ = w.set_permissions(md.permissions());
    if let Ok(m) = md.modified() {
        let _ = w.set_modified(m);
    }
    Ok(())
}

/// Registra un fallo de un elemento concreto sin detener el resto de la copia.
fn note_error(prog: &Progress, path: &Path, e: &std::io::Error) {
    let mut v = prog.errors.lock().unwrap();
    if v.len() < 500 {
        v.push(format!("{}: {e}", path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| path.display().to_string())));
    }
}

/// Copia un árbol. Un fallo en un elemento se anota y se sigue con el resto; devuelve cuántos
/// elementos fallaron. Sólo la cancelación (`Interrupted`) corta la copia.
fn copy_tree(src: &Path, dst: &Path, prog: &Progress, buf: &mut Vec<u8>) -> std::io::Result<usize> {
    if prog.cancelled() {
        return Err(std::io::Error::new(std::io::ErrorKind::Interrupted, "cancelado"));
    }
    let md = match fs::symlink_metadata(src) {
        Ok(m) => m,
        Err(e) => {
            note_error(prog, src, &e);
            return Ok(1);
        }
    };
    let mut failed = 0;
    if md.v_is_dir() {
        if let Err(e) = fs::create_dir_all(dst) {
            note_error(prog, dst, &e);
            // sin la carpeta no se puede copiar su contenido; se da por hecho su tamaño
            prog.done.fetch_add(size_of_tree(src), AO::Relaxed);
            return Ok(1);
        }
        match fs::read_dir(src) {
            Ok(rd) => {
                // Los archivos pequeños se copian en paralelo: en red lo que cuesta es la latencia
                // de cada archivo, no el ancho de banda.
                let mut small: Vec<(PathBuf, PathBuf)> = Vec::new();
                for e in rd {
                    match e {
                        Ok(e) => {
                            let sp = e.path();
                            let dp = dst.join(e.file_name());
                            let is_small = e.file_type().map(|t| t.v_is_file()).unwrap_or(false)
                                && e.metadata().map(|m| m.len() < (8 << 20)).unwrap_or(false);
                            if is_small {
                                small.push((sp, dp));
                            } else {
                                failed += copy_tree(&sp, &dp, prog, buf)?;
                            }
                        }
                        Err(e) => {
                            note_error(prog, src, &e);
                            failed += 1;
                        }
                    }
                }
                if small.len() < 4 {
                    for (sp, dp) in &small {
                        failed += copy_tree(sp, dp, prog, buf)?;
                    }
                } else {
                    let next = std::sync::atomic::AtomicUsize::new(0);
                    let fails = std::sync::atomic::AtomicUsize::new(0);
                    let cancelled = std::sync::atomic::AtomicBool::new(false);
                    crate::vfs::thread::scope(|sc| {
                        for _ in 0..small.len().min(6) {
                            sc.spawn(|| {
                                let mut b: Vec<u8> = Vec::new();
                                loop {
                                    let i = next.fetch_add(1, AO::Relaxed);
                                    let Some((sp, dp)) = small.get(i) else { break };
                                    match copy_tree(sp, dp, prog, &mut b) {
                                        Ok(f) => {
                                            fails.fetch_add(f, AO::Relaxed);
                                        }
                                        Err(_) => {
                                            cancelled.store(true, AO::Relaxed);
                                            break;
                                        }
                                    }
                                }
                            });
                        }
                    });
                    if cancelled.load(AO::Relaxed) {
                        return Err(std::io::Error::new(std::io::ErrorKind::Interrupted, "cancelado"));
                    }
                    failed += fails.load(AO::Relaxed);
                }
            }
            Err(e) => {
                note_error(prog, src, &e);
                failed += 1;
            }
        }
        // permisos y fecha de la carpeta, al final (si es de sólo lectura no estorba a su contenido)
        let _ = fs::set_permissions(dst, md.permissions());
        prog.done.fetch_add(1, AO::Relaxed);
    } else if md.file_type().v_is_symlink() {
        let r: std::io::Result<()> = fs::read_link(src).and_then(|target| {
            #[cfg(unix)]
            {
                std::os::unix::fs::symlink(target, dst)
            }
            #[cfg(windows)]
            {
                let _ = target;
                fs::copy(src, dst).map(|_| ())
            }
            #[cfg(not(any(unix, windows)))]
            {
                let _ = (target, dst);
                Err(std::io::Error::from(std::io::ErrorKind::Unsupported))
            }
        });
        if let Err(e) = r {
            note_error(prog, src, &e);
            failed += 1;
        }
        prog.done.fetch_add(1, AO::Relaxed);
    } else if cfg!(unix) && !md.file_type().v_is_file() {
        // FIFO, sockets y dispositivos: abrirlos bloquearía la copia para siempre
        note_error(prog, src, &std::io::Error::other(t("archivo especial")));
        failed += 1;
        prog.done.fetch_add(1, AO::Relaxed);
    } else {
        *prog.label.lock().unwrap() = src.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        if let Err(e) = copy_file(src, dst, prog, buf) {
            if e.kind() == std::io::ErrorKind::Interrupted {
                return Err(e);
            }
            note_error(prog, src, &e);
            let _ = fs::remove_file(dst);
            failed += 1;
            prog.done.fetch_add(md.len().max(1), AO::Relaxed);
        }
    }
    Ok(failed)
}

pub fn run_op(op: Op, ctx: egui::Context) -> Arc<Progress> {
    let prog = Arc::new(Progress::default());
    let p = prog.clone();
    crate::vfs::thread::spawn(move || {
        match op {
            Op::Copy { srcs, dst, mv, policy } => {
                *p.label.lock().unwrap() = t("Calculando…").into();
                for s in &srcs {
                    crate::sftp::sync_tree(s);
                }
                let costs: Vec<u64> = srcs.iter().map(|s| size_of_tree(s)).collect();
                p.total.store(costs.iter().fold(0u64, |a, &c| a.saturating_add(c)).max(1), AO::Relaxed);
                let mut buf: Vec<u8> = Vec::new();
                for (src, cost) in srcs.into_iter().zip(costs) {
                    if p.cancelled() {
                        break;
                    }
                    let Some(name) = src.file_name().map(|n| n.to_string_lossy().into_owned()) else {
                        continue;
                    };
                    if mv && src.parent() == Some(dst.as_path()) {
                        continue;
                    }
                    let canon = |p: &Path| fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
                    if src.v_is_dir() && canon(&dst).starts_with(canon(&src)) {
                        p.errors.lock().unwrap().push(tf("{name}: no se puede copiar dentro de sí misma", &[&name]));
                        continue;
                    }
                    let same_dir = src.parent() == Some(dst.as_path());
                    // «Reemplazar» al copiar: se copia a un temporal y sólo si sale bien se cambia por el original
                    let mut replace_old: Option<PathBuf> = None;
                    let target = match policy {
                        Policy::Rename => unique_path(&dst, &name),
                        _ if same_dir => unique_path(&dst, &name),
                        Policy::Skip => {
                            let t = dst.join(&name);
                            if fs::symlink_metadata(&t).is_ok() {
                                p.done.fetch_add(cost, AO::Relaxed);
                                continue;
                            }
                            t
                        }
                        Policy::Replace => {
                            let t = dst.join(&name);
                            if fs::symlink_metadata(&t).is_ok() {
                                // nunca reemplazar el propio origen ni una carpeta que lo contiene
                                if canon(&src).starts_with(canon(&t)) {
                                    p.errors.lock().unwrap().push(tf("{name}: no se puede reemplazar el propio origen o una carpeta que lo contiene", &[&name]));
                                    continue;
                                }
                                if !mv {
                                    replace_old = Some(t.clone());
                                    continue_with_tmp(&dst, &name)
                                } else {
                                // El elemento reemplazado va a la papelera para poder recuperarlo.
                                if let Err(e) = crate::extras::trash_delete(&t) {
                                    // sin papelera (Android…): se sobrescribe, que es lo que pide «Reemplazar»
                                    let gone = if t.v_is_dir() && !t.v_is_symlink() { fs::remove_dir_all(&t) } else { fs::remove_file(&t) };
                                    if let Err(e2) = gone {
                                        p.errors.lock().unwrap().push(format!("{name}: {e} ({e2})"));
                                        continue;
                                    }
                                }
                                t
                                }
                            } else {
                                t
                            }
                        }
                    };
                    let final_target = replace_old.clone().unwrap_or_else(|| target.clone());
                    let tname = final_target.file_name().unwrap().to_string_lossy().into_owned();
                    let res = if mv {
                        match fs::rename(&src, &target) {
                            Ok(()) => {
                                p.done.fetch_add(cost, AO::Relaxed);
                                Ok(())
                            }
                            // otro sistema de archivos: se copia y, sólo si todo salió bien, se borra el origen
                            Err(_) => copy_tree(&src, &target, &p, &mut buf).and_then(|failed| {
                                if failed > 0 {
                                    // copia incompleta: se conserva el origen y se avisa del fallo
                                    let _ = if target.v_is_dir() { fs::remove_dir_all(&target) } else { fs::remove_file(&target) };
                                    Err(std::io::Error::other(t("la copia no se completó")))
                                } else if src.v_is_dir() {
                                    fs::remove_dir_all(&src)
                                } else {
                                    fs::remove_file(&src)
                                }
                            }),
                        }
                    } else if let Some(old) = &replace_old {
                        copy_tree(&src, &target, &p, &mut buf).and_then(|failed| {
                            if failed > 0 {
                                let _ = if target.v_is_dir() { fs::remove_dir_all(&target) } else { fs::remove_file(&target) };
                                return Err(std::io::Error::other(t("la copia no se completó")));
                            }
                            // copia completa: ahora sí se retira lo anterior (papelera si hay) y se coloca la copia
                            if crate::extras::trash_delete(old).is_err() {
                                if old.v_is_dir() && !old.v_is_symlink() { fs::remove_dir_all(old)? } else { fs::remove_file(old)? }
                            }
                            fs::rename(&target, old)
                        })
                    } else {
                        copy_tree(&src, &target, &p, &mut buf).map(|_| ())
                    };
                    match res {
                        Ok(()) => {
                            p.created.lock().unwrap().push(tname);
                            p.pairs.lock().unwrap().push((src.clone(), final_target.clone()));
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {
                            let _ = if target.v_is_dir() { fs::remove_dir_all(&target) } else { fs::remove_file(&target) };
                        }
                        Err(e) => p.errors.lock().unwrap().push(format!("{name}: {e}")),
                    }
                    ctx.request_repaint();
                }
            }
            Op::Trash(list) => {
                p.total.store(list.len() as u64, AO::Relaxed);
                for it in list {
                    if p.cancelled() {
                        break;
                    }
                    match crate::extras::trash_delete(&it) {
                        Ok(()) => p.pairs.lock().unwrap().push((it.clone(), it.clone())),
                        Err(e) => p.errors.lock().unwrap().push(format!("{}: {e}", it.display())),
                    }
                    p.done.fetch_add(1, AO::Relaxed);
                    ctx.request_repaint();
                }
            }
            Op::Compress { srcs, out, opts } => {
                let total: u64 = srcs.iter().fold(0u64, |a, s| a.saturating_add(size_of_tree(s)));
                p.total.store(total.max(1), AO::Relaxed);
                *p.label.lock().unwrap() = t("Comprimiendo…").into();
                match crate::tools::compress_paths(&srcs, &out, &opts, &p, &ctx) {
                    Ok(()) => {
                        p.created.lock().unwrap().push(out.file_name().unwrap().to_string_lossy().into_owned());
                        p.pairs.lock().unwrap().push((out.clone(), out.clone()));
                    }
                    Err(e) => {
                        let _ = fs::remove_file(&out);
                        p.errors.lock().unwrap().push(e);
                    }
                }
            }
            Op::Unzip { src, dst, pass } => {
                *p.label.lock().unwrap() = t("Extrayendo…").into();
                match crate::archive::extract_all(&src, &dst, &p, &ctx, pass.as_deref()) {
                    Ok(top) => {
                        p.created.lock().unwrap().extend(top);
                        p.pairs.lock().unwrap().push((dst.clone(), dst.clone()));
                    }
                    Err(e) => p.errors.lock().unwrap().push(e),
                }
            }
            Op::Delete(list) => {
                p.total.store(list.len() as u64, AO::Relaxed);
                for it in list {
                    if p.cancelled() {
                        break;
                    }
                    let r = if it.v_is_dir() && !fs::symlink_metadata(&it).map(|m| m.file_type().v_is_symlink()).unwrap_or(false) {
                        fs::remove_dir_all(&it)
                    } else {
                        fs::remove_file(&it)
                    };
                    if let Err(e) = r {
                        p.errors.lock().unwrap().push(format!("{}: {e}", it.display()));
                    }
                    p.done.fetch_add(1, AO::Relaxed);
                    ctx.request_repaint();
                }
            }
        }
        p.finished.store(true, AO::Release);
        ctx.request_repaint();
    });
    prog
}

pub fn open_external(path: &Path) {
    // archivo de un servidor SFTP: se descarga y luego se abre
    if crate::sftp::is_remote(path) && !path.v_is_dir() {
        let p = path.to_path_buf();
        crate::vfs::thread::spawn(move || {
            if crate::sftp::ensure_local(&p).is_ok() {
                open_external(&p);
            }
        });
        return;
    }
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        #[cfg(target_os = "linux")]
        let prog = "xdg-open";
        #[cfg(target_os = "macos")]
        let prog = "open";
        if let Ok(mut c) = std::process::Command::new(prog).arg(path).spawn() {
            // sin esperar al proceso quedaría zombi
            crate::vfs::thread::spawn(move || {
                let _ = c.wait();
            });
        }
    }
    #[cfg(windows)]
    {
        // ShellExecute: respeta la asociación del sistema y no pasa por cmd (&, ^, % en nombres)
        use std::os::windows::ffi::OsStrExt;
        #[link(name = "shell32")]
        unsafe extern "system" {
            fn ShellExecuteW(hwnd: isize, op: *const u16, file: *const u16, params: *const u16, dir: *const u16, show: i32) -> isize;
        }
        let wide = |s: &std::ffi::OsStr| -> Vec<u16> { s.encode_wide().chain(std::iter::once(0)).collect() };
        let (op, file) = (wide("open".as_ref()), wide(path.as_os_str()));
        unsafe {
            ShellExecuteW(0, op.as_ptr(), file.as_ptr(), std::ptr::null(), std::ptr::null(), 1);
        }
    }
    #[cfg(target_os = "android")]
    let _ = crate::android_open(path);
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows, target_os = "android")))]
    let _ = path;
}

pub fn human_size(n: u64) -> String {
    const U: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i < 4 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 { format!("{n} B") } else { format!("{v:.1} {}", U[i]) }
}

/// Desfase (segundos) de la zona horaria local respecto a UTC en el instante `t`.
#[cfg(unix)]
fn local_offset(t: i64) -> i64 {
    unsafe {
        let tt = t as libc::time_t;
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&tt, &mut tm).is_null() {
            return 0;
        }
        tm.tm_gmtoff as i64
    }
}

#[cfg(target_arch = "wasm32")]
fn local_offset(t: i64) -> i64 {
    -(js_sys::Date::new(&wasm_bindgen::JsValue::from_f64(t as f64 * 1000.0)).get_timezone_offset() as i64) * 60
}

#[cfg(windows)]
fn local_offset(_t: i64) -> i64 {
    #[repr(C)]
    struct SystemTime([u16; 8]);
    #[repr(C)]
    struct Tzi {
        bias: i32,
        std_name: [u16; 32],
        std_date: SystemTime,
        std_bias: i32,
        dst_name: [u16; 32],
        dst_date: SystemTime,
        dst_bias: i32,
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetTimeZoneInformation(tzi: *mut Tzi) -> u32;
    }
    unsafe {
        let mut z: Tzi = std::mem::zeroed();
        let id = GetTimeZoneInformation(&mut z);
        let extra = match id {
            1 => z.std_bias,
            2 => z.dst_bias,
            _ => 0,
        };
        -((z.bias + extra) as i64) * 60
    }
}

/// Interpreta "AAAA-MM-DD HH:MM[:SS]" (hora local) y devuelve segundos Unix.
pub fn parse_time(text: &str) -> Option<i64> {
    let text = text.trim();
    let (d, t) = text.split_once([' ', 'T']).unwrap_or((text, "00:00:00"));
    let mut dp = d.split('-');
    let (y, m, day): (i64, i64, i64) = (dp.next()?.parse().ok()?, dp.next()?.parse().ok()?, dp.next()?.parse().ok()?);
    if dp.next().is_some() || !(1..=12).contains(&m) || !(1..=31).contains(&day) || !(1970..=9999).contains(&y) {
        return None;
    }
    let mut tp = t.split(':');
    let h: i64 = tp.next()?.parse().ok()?;
    let mi: i64 = tp.next().unwrap_or("0").parse().ok()?;
    let se: i64 = tp.next().unwrap_or("0").parse().ok()?;
    if tp.next().is_some() || !(0..24).contains(&h) || !(0..60).contains(&mi) || !(0..60).contains(&se) {
        return None;
    }
    // days from civil (Howard Hinnant)
    let y2 = if m <= 2 { y - 1 } else { y };
    let era = y2.div_euclid(400);
    let yoe = y2 - era * 400;
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    let naive = days * 86400 + h * 3600 + mi * 60 + se;
    let off = local_offset(naive - local_offset(naive));
    Some(naive - off)
}

/// Cambia la fecha de modificación de un archivo o carpeta.
pub fn set_mtime(path: &Path, secs: i64) -> std::io::Result<()> {
    let t = web_time::UNIX_EPOCH + std::time::Duration::from_secs(secs.max(0) as u64);
    let mut o = crate::vfs::fs::OpenOptions::new();
    o.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        // necesario para abrir carpetas; escritura para poder fijar la fecha
        o.write(true).custom_flags(0x0200_0000);
    }
    o.open(path)?.set_modified(t)
}

/// Fecha local "AAAA-MM-DD HH:MM" sin dependencias pesadas.
pub fn fmt_time(secs: i64) -> String {
    if secs <= 0 {
        return String::new();
    }
    let secs = secs + local_offset(secs);
    let days = secs.div_euclid(86400);
    let rem = secs.rem_euclid(86400);
    // civil from days (Howard Hinnant)
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z.rem_euclid(146097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02}", rem / 3600, (rem % 3600) / 60, rem % 60)
}

#[cfg(test)]
mod time_tests {
    use super::*;

    #[test]
    fn parse_time_roundtrip() {
        for secs in [86_400i64 * 365, 1_700_000_000, 1_760_000_123] {
            assert_eq!(parse_time(&fmt_time(secs)), Some(secs));
        }
        assert_eq!(parse_time("2026-13-01 00:00"), None);
        assert!(parse_time("2026-10-12 10:30").is_some());
    }
}

/// Datos extra de un elemento para las columnas opcionales de la lista (se piden sólo si se muestran).
#[derive(Clone, Default)]
pub struct ExtraMeta {
    pub created: i64,
    pub accessed: i64,
    /// Bits de permisos (`rwxrwxrwx`), si la plataforma los tiene.
    pub mode: Option<u32>,
    pub owner: String,
    pub group: String,
    /// Número de elementos de una carpeta.
    pub items: Option<usize>,
    /// Destino de un enlace simbólico.
    pub link: String,
}

fn secs(t: std::io::Result<web_time::SystemTime>) -> i64 {
    t.ok()
        .and_then(|t| t.duration_since(web_time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(unix)]
fn name_of(id: u32, group: bool) -> String {
    use std::ffi::CStr;
    let mut buf = vec![0u8; 1024];
    unsafe {
        if group {
            let mut gr: libc::group = std::mem::zeroed();
            let mut res: *mut libc::group = std::ptr::null_mut();
            if libc::getgrgid_r(id, &mut gr, buf.as_mut_ptr() as *mut _, buf.len(), &mut res) == 0 && !res.is_null() {
                return CStr::from_ptr(gr.gr_name).to_string_lossy().into_owned();
            }
        } else {
            let mut pw: libc::passwd = std::mem::zeroed();
            let mut res: *mut libc::passwd = std::ptr::null_mut();
            if libc::getpwuid_r(id, &mut pw, buf.as_mut_ptr() as *mut _, buf.len(), &mut res) == 0 && !res.is_null() {
                return CStr::from_ptr(pw.pw_name).to_string_lossy().into_owned();
            }
        }
    }
    id.to_string()
}

pub fn extra_meta(p: &Path, is_dir: bool) -> ExtraMeta {
    let mut x = ExtraMeta::default();
    if let Ok(l) = crate::vfs::fs::symlink_metadata(p) {
        if l.file_type().v_is_symlink() {
            x.link = crate::vfs::fs::read_link(p).map(|t| t.to_string_lossy().into_owned()).unwrap_or_default();
        }
    }
    let Ok(md) = crate::vfs::fs::metadata(p) else { return x };
    x.created = secs(md.created());
    x.accessed = secs(md.accessed());
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        x.mode = Some(md.mode() & 0o777);
        x.owner = name_of(md.uid(), false);
        x.group = name_of(md.gid(), true);
    }
    #[cfg(not(unix))]
    {
        x.mode = Some(if md.permissions().readonly() { 0o444 } else { 0o666 });
    }
    if is_dir {
        x.items = crate::vfs::fs::read_dir(p).ok().map(|d| d.count());
    }
    x
}

/// "rwxr-xr-x" a partir de los bits de permisos.
pub fn mode_str(m: u32) -> String {
    let mut s = String::with_capacity(9);
    for i in (0..3).rev() {
        let b = (m >> (i * 3)) & 7;
        s.push(if b & 4 != 0 { 'r' } else { '-' });
        s.push(if b & 2 != 0 { 'w' } else { '-' });
        s.push(if b & 1 != 0 { 'x' } else { '-' });
    }
    s
}

#[cfg(test)]
mod copy_tests {
    use super::*;

    fn run(src: &Path, dst: &Path) -> Vec<String> {
        let p = run_op(Op::Copy { srcs: vec![src.to_path_buf()], dst: dst.to_path_buf(), mv: false, policy: Policy::Rename }, egui::Context::default());
        let t0 = Instant::now();
        while !p.finished.load(AO::Acquire) {
            assert!(t0.elapsed() < Duration::from_secs(120), "timeout");
            crate::vfs::thread::sleep(Duration::from_millis(20));
        }
        let e = p.errors.lock().unwrap().clone();
        e
    }

    fn run_pol(src: &Path, dst: &Path, mv: bool, policy: Policy) -> Vec<String> {
        let p = run_op(Op::Copy { srcs: vec![src.to_path_buf()], dst: dst.to_path_buf(), mv, policy }, egui::Context::default());
        let t0 = Instant::now();
        while !p.finished.load(AO::Acquire) {
            assert!(t0.elapsed() < Duration::from_secs(60), "timeout");
            crate::vfs::thread::sleep(Duration::from_millis(20));
        }
        let e = p.errors.lock().unwrap().clone();
        e
    }

    #[test]
    fn replace_copies_then_swaps() {
        let base = crate::vfs::temp_dir().join(format!("fbrep{}", crate::vfs::pid()));
        let (a, b) = (base.join("a"), base.join("b"));
        fs::create_dir_all(&a).unwrap();
        fs::create_dir_all(&b).unwrap();
        fs::write(a.join("f.txt"), b"nuevo").unwrap();
        fs::write(b.join("f.txt"), b"viejo").unwrap();
        let errs = run_pol(&a.join("f.txt"), &b, false, Policy::Replace);
        assert!(errs.is_empty(), "{errs:?}");
        assert_eq!(fs::read(b.join("f.txt")).unwrap(), b"nuevo");
        assert_eq!(fs::read_dir(&b).unwrap().count(), 1, "sin temporales sobrantes");
        // reemplazar una carpeta que contiene al origen se rechaza y no toca nada
        let errs = run_pol(&b.join("f.txt"), base.as_path(), false, Policy::Replace);
        let _ = errs;
        assert!(b.join("f.txt").exists());
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn copy_many_files() {
        let base = crate::vfs::temp_dir().join(format!("fbcopy{}", crate::vfs::pid()));
        let src = base.join("src");
        for d in 0..40 {
            let dir = src.join(format!("d{d}")).join("sub");
            fs::create_dir_all(&dir).unwrap();
            for f in 0..500 {
                fs::write(dir.join(format!("f{f}.txt")), b"hola").unwrap();
            }
        }
        let dst = base.join("dst");
        fs::create_dir_all(&dst).unwrap();
        let errs = run(&src, &dst);
        assert!(errs.is_empty(), "{:?}", &errs[..errs.len().min(3)]);
        let n = walk_count(&dst.join("src"));
        let _ = fs::remove_dir_all(&base);
        assert_eq!(n, 20000 + 40 + 40);
    }

    fn walk_count(p: &Path) -> usize {
        let mut n = 0;
        if let Ok(rd) = fs::read_dir(p) {
            for e in rd.flatten() {
                n += 1;
                if e.path().v_is_dir() {
                    n += walk_count(&e.path());
                }
            }
        }
        n
    }
}
