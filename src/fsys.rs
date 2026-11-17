//! Capa de sistema de archivos: listado en streaming, ordenación y operaciones.
#[allow(unused_imports)]
use crate::vfs::{KindV as _, PathV as _};
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
        "mp4" | "mkv" | "avi" | "mov" | "webm" | "wmv" | "flv" | "m4v" | "mpg" | "mpeg" => {
            Kind::Video
        }
        "mp3" | "flac" | "wav" | "ogg" | "opus" | "m4a" | "aac" | "wma" | "aiff" => Kind::Audio,
        "txt" | "md" | "rs" | "toml" | "json" | "yaml" | "yml" | "xml" | "html" | "htm" | "css"
        | "js" | "jsx" | "tsx" | "py" | "c" | "h" | "cpp" | "hpp" | "cc" | "java" | "go" | "sh"
        | "bat" | "ini" | "cfg" | "conf" | "log" | "csv" | "sql" | "lua" | "php" | "rb" | "cs"
        | "kt" | "swift" | "lock" | "gitignore" | "env" | "tex" | "srt"
        // «.ts» es TypeScript salvo que el contenido diga MPEG-TS (ver `kind_of_file`)
        | "ts" => Kind::Text,
        "zip" | "rar" | "7z" | "tar" | "gz" | "bz2" | "xz" | "zst" | "iso" | "deb" | "rpm"
        | "tgz" | "jar" => Kind::Archive,
        "pdf" | "doc" | "docx" | "odt" | "xls" | "xlsx" | "ods" | "ppt" | "pptx" | "odp" | "rtf"
        | "epub" | "cbz" | "cbr" => Kind::Doc,
        _ => Kind::Other,
    }
}

/// Como `kind_of_ext`, mirando el contenido sólo cuando la extensión es ambigua: «.ts» puede ser
/// TypeScript o vídeo MPEG-TS.
pub fn kind_of_file(path: &Path, ext: &str, size: u64) -> Kind {
    if ext.eq_ignore_ascii_case("ts") && is_mpeg_ts(path, size) {
        return Kind::Video;
    }
    kind_of_ext(ext)
}

/// ¿Es un flujo MPEG-TS? Paquetes de 188 bytes que empiezan por el byte de sincronía 0x47.
pub fn is_mpeg_ts(path: &Path, size: u64) -> bool {
    use std::io::Read;
    if size < 189 {
        return false;
    }
    let Ok(f) = fs::File::open(path) else { return false };
    let mut b = Vec::with_capacity(377);
    if f.take(377).read_to_end(&mut b).is_err() {
        return false;
    }
    is_mpeg_ts_bytes(&b)
}

fn is_mpeg_ts_bytes(b: &[u8]) -> bool {
    b.len() >= 189 && b[0] == 0x47 && b[188] == 0x47 && (b.len() < 377 || b[376] == 0x47)
}

// ---------------------------------------------------------------------------
// Carga en streaming
// ---------------------------------------------------------------------------

pub enum LoadMsg {
    Chunk(u64, Vec<Entry>),
    Done(u64, Option<String>),
    /// aviso que no impide listar (servidor sin conexión: se ve lo que había en la caché)
    Warn(u64, String),
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

/// Para los recorridos (vista recursiva, búsquedas): oculto si empieza por punto y, en Windows,
/// también si tiene el atributo «oculto» (sin pedir más datos: vienen con la propia entrada).
pub fn is_hidden_entry(de: &fs::DirEntry) -> bool {
    if de.file_name().to_string_lossy().starts_with('.') {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if de.metadata().is_ok_and(|m| m.file_attributes() & 0x2 != 0) {
            return true;
        }
    }
    false
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
            let _ = tx.send(LoadMsg::Warn(generation, crate::i18n::tf("Sin conexión con el servidor ({}): se muestra lo que había en la caché", &[&e])));
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
                    Some((s, e)) if !s.is_empty() => kind_of_file(&de.path(), e, md_eff.len()),
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
            Some((s, e)) if !s.is_empty() => kind_of_file(path, e, md_eff.len()),
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
                if !hidden && is_hidden_entry(&de) {
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
            // en búsquedas y vistas recursivas el nombre es una ruta: se filtra sólo por el último trozo
            let base = if cfg!(windows) { e.name.rsplit(['/', '\\']).next() } else { e.name.rsplit('/').next() };
            (spec.show_hidden || !e.is_hidden()) && (needle.is_empty() || contains_ci(base.unwrap_or(&e.name), &needle))
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
    /// Cada archivo copiado se lleva al disco (fsync) antes de darlo por bueno: se activa cuando
    /// después se va a borrar algo (mover, reemplazar), para no perder datos si se va la luz.
    pub durable: AtomicBool,
    /// Destinos que se reemplazaron: (ruta, `true` si lo anterior está en la papelera y `false` si
    /// se borró definitivamente). Deshacer sólo puede recuperar los primeros.
    pub replaced: Mutex<Vec<(PathBuf, bool)>>,
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
    /// Carpetas: se combinan recursivamente y sólo se reemplazan los archivos que coinciden.
    /// Con un archivo (o si alguno de los dos no es carpeta) equivale a `Replace`.
    Merge,
}

pub enum Op {
    Copy { srcs: Vec<PathBuf>, dst: PathBuf, mv: bool, policy: Policy },
    Trash(Vec<PathBuf>),
    Delete(Vec<PathBuf>),
    Compress { srcs: Vec<PathBuf>, out: PathBuf, opts: crate::tools::CompressOpts },
    Unzip { src: PathBuf, dst: PathBuf, pass: Option<String> },
    /// Descarga de un servidor SFTP (archivos o carpetas enteras) antes de dárselos a otro programa.
    Fetch(Vec<PathBuf>),
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
    tree_size(p, None)
}

/// Como `size_of_tree`, pero deja de recorrer en cuanto se cancela (lo medido ya no importa).
fn tree_size(p: &Path, cancel: Option<&AtomicBool>) -> u64 {
    if cancel.is_some_and(|c| c.load(AO::Relaxed)) {
        return 0;
    }
    match fs::symlink_metadata(p) {
        Ok(m) if m.v_is_dir() => fs::read_dir(p)
            .map(|rd| rd.flatten().fold(0u64, |a, e| a.saturating_add(tree_size(&e.path(), cancel))))
            .unwrap_or(0)
            .saturating_add(1),
        Ok(m) => m.len().max(1),
        Err(_) => 0,
    }
}

/// Volumen de una ruta, para saber sin recorrer nada si un mover será un simple `rename`. `None`
/// si no se sabe (entonces se mide, por si hay que copiar).
fn volume_of(p: &Path) -> Option<u64> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        fs::symlink_metadata(p).ok().map(|m| m.dev())
    }
    #[cfg(windows)]
    {
        // la unidad (C:, \\servidor\recurso…): una carpeta montada dentro de otra unidad se
        // detectará al fallar el `rename`, y se copiará igual
        use std::hash::{Hash, Hasher};
        let abs = std::path::absolute(p).ok()?;
        let pre = abs.components().next()?.as_os_str().to_string_lossy().to_lowercase();
        let mut h = std::collections::hash_map::DefaultHasher::new();
        pre.hash(&mut h);
        Some(h.finish())
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = p;
        None
    }
}

/// Cuenta los elementos de un árbol (sin seguir enlaces), para el progreso de un borrado.
fn count_tree(p: &Path, cancel: &AtomicBool) -> u64 {
    if cancel.load(AO::Relaxed) {
        return 0;
    }
    match fs::symlink_metadata(p) {
        Ok(m) if m.v_is_dir() => fs::read_dir(p)
            .map(|rd| rd.flatten().fold(0u64, |a, e| a.saturating_add(count_tree(&e.path(), cancel))))
            .unwrap_or(0)
            .saturating_add(1),
        Ok(_) => 1,
        Err(_) => 0,
    }
}

/// Borra definitivamente un árbol elemento a elemento, contando el progreso y atendiendo a la
/// cancelación (lo ya borrado no vuelve). Sin seguir enlaces.
fn delete_tree(p: &Path, prog: &Progress, ctx: &egui::Context) -> std::io::Result<()> {
    if prog.cancelled() {
        return Err(std::io::Error::new(std::io::ErrorKind::Interrupted, crate::i18n::t("cancelado")));
    }
    let md = fs::symlink_metadata(p)?;
    if md.v_is_dir() {
        *prog.label.lock().unwrap() = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        for e in fs::read_dir(p)? {
            delete_tree(&e?.path(), prog, ctx)?;
        }
        fs::remove_dir(p)?;
    } else {
        #[cfg(windows)]
        let r = {
            use std::os::windows::fs::FileTypeExt;
            if md.file_type().is_symlink_dir() { fs::remove_dir(p) } else { fs::remove_file(p) }
        };
        #[cfg(not(windows))]
        let r = fs::remove_file(p);
        match r {
            // Windows: un archivo de sólo lectura no se deja borrar sin quitarle el atributo
            #[cfg(windows)]
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied && md.permissions().readonly() => {
                let mut pm = md.permissions();
                pm.set_readonly(false);
                fs::set_permissions(p, pm)?;
                fs::remove_file(p)?;
            }
            r => r?,
        }
    }
    if prog.done.fetch_add(1, AO::Relaxed) % 256 == 0 {
        ctx.request_repaint();
    }
    Ok(())
}

/// Nombre temporal junto al destino para copiar antes de reemplazar. Corto (no depende del nombre
/// original, que podría rozar NAME_MAX) e impredecible: nadie puede dejar ahí un enlace a la espera.
fn tmp_beside(dir: &Path) -> PathBuf {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let n = web_time::SystemTime::now().duration_since(web_time::UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(0);
    let k = SEQ.fetch_add(1, AO::Relaxed);
    for i in 0..100u64 {
        let p = dir.join(format!("{}fbrep-{:x}-{n:x}-{:x}", crate::vfs::TMP_LEAD, crate::vfs::pid(), k.wrapping_add(i << 32)));
        if fs::symlink_metadata(&p).is_err() {
            return p;
        }
    }
    dir.join(format!("{}fbrep-{:x}-{n:x}-{k:x}-x", crate::vfs::TMP_LEAD, crate::vfs::pid()))
}

/// ¿`b` es el mismo elemento que `a` escrito con otras mayúsculas? Pasa en FAT/exFAT/SMB/NTFS/APFS,
/// donde «foo.txt» y «Foo.txt» son el mismo archivo: renombrar entre ellos no pisa nada.
pub fn case_alias(a: &Path, b: &Path) -> bool {
    let (Some(na), Some(nb)) = (a.file_name(), b.file_name()) else { return false };
    if a.parent() != b.parent() || na == nb || na.to_string_lossy().to_lowercase() != nb.to_string_lossy().to_lowercase() {
        return false;
    }
    #[cfg(unix)]
    {
        // mismo inodo y un único nombre (dos enlaces duros «foo» y «Foo» sí son nombres distintos)
        use std::os::unix::fs::MetadataExt;
        matches!((fs::symlink_metadata(a), fs::symlink_metadata(b)), (Ok(x), Ok(y)) if x.dev() == y.dev() && x.ino() == y.ino() && (x.nlink() == 1 || x.is_dir()))
    }
    #[cfg(not(unix))]
    {
        matches!((fs::canonicalize(a), fs::canonicalize(b)), (Ok(x), Ok(y)) if x == y)
    }
}

/// Cambia sólo las mayúsculas del nombre (`case_alias`): pasa por un nombre temporal, porque algunos
/// sistemas (SMB, FUSE) no aceptan renombrar un archivo sobre sí mismo o no cambian nada.
pub fn rename_case(a: &Path, b: &Path) -> std::io::Result<()> {
    let tmp = tmp_beside(a.parent().unwrap_or(Path::new(".")));
    fs::rename(a, &tmp)?;
    if let Err(e) = fs::rename(&tmp, b) {
        let _ = fs::rename(&tmp, a);
        return Err(e);
    }
    Ok(())
}

pub fn unique_path(dir: &Path, name: &str) -> PathBuf {
    unique_path_os(dir, std::ffi::OsStr::new(name))
}

/// Como `unique_path`, para nombres que pueden no ser UTF-8 (se conservan byte a byte).
pub fn unique_path_os(dir: &Path, name: &std::ffi::OsStr) -> PathBuf {
    let first = dir.join(name);
    if fs::symlink_metadata(&first).is_err() {
        return first;
    }
    let (stem, ext): (std::ffi::OsString, std::ffi::OsString) = match name.to_str() {
        Some(n) => match n.rsplit_once('.') {
            Some((s, e)) if !s.is_empty() => (s.into(), format!(".{e}").into()),
            _ => (n.into(), Default::default()),
        },
        None => {
            let p = Path::new(name);
            match (p.file_stem(), p.extension()) {
                (Some(s), Some(e)) => {
                    let mut x = std::ffi::OsString::from(".");
                    x.push(e);
                    (s.to_os_string(), x)
                }
                _ => (name.to_os_string(), Default::default()),
            }
        }
    };
    for n in 2u64.. {
        let mut c = stem.clone();
        c.push(format!(" ({n})"));
        c.push(&ext);
        let p = dir.join(c);
        if fs::symlink_metadata(&p).is_err() {
            return p;
        }
    }
    unreachable!()
}

/// ¿Es un error de «otro sistema de archivos»? Sólo entonces un mover se hace copiando y borrando.
fn cross_device(e: &std::io::Error) -> bool {
    #[cfg(target_arch = "wasm32")]
    {
        // el servidor responde 400 entre carpetas compartidas distintas
        e.kind() == std::io::ErrorKind::Other
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        #[cfg(unix)]
        let raw = Some(libc::EXDEV);
        #[cfg(windows)]
        let raw = Some(17); // ERROR_NOT_SAME_DEVICE
        #[cfg(not(any(unix, windows)))]
        let raw: Option<i32> = None;
        e.kind() == std::io::ErrorKind::CrossesDevices || (raw.is_some() && e.raw_os_error() == raw)
    }
}

/// Fsync que no falla donde el sistema de archivos no lo admite (algunos FUSE/red).
fn sync_soft(r: std::io::Result<()>) -> std::io::Result<()> {
    match r {
        Err(e) if matches!(e.kind(), std::io::ErrorKind::Unsupported | std::io::ErrorKind::InvalidInput) => Ok(()),
        r => r,
    }
}

/// Borra un archivo, enlace o carpeta entera (sin seguir enlaces).
fn remove_any(p: &Path) -> std::io::Result<()> {
    match fs::symlink_metadata(p) {
        Ok(m) if m.v_is_dir() => fs::remove_dir_all(p),
        Ok(_) => fs::remove_file(p),
        Err(e) => Err(e),
    }
}

/// Copia un archivo por bloques, avisando del progreso en bytes y atendiendo a la cancelación.
/// Permisos y fecha se copian "si se puede": muchos sistemas de red/FAT no los admiten y eso no
/// debe convertir una copia correcta en un error.
fn copy_file(src: &Path, dst: &Path, prog: &Progress, buf: &mut Vec<u8>) -> std::io::Result<()> {
    use std::io::{Read, Write};
    // archivo de un servidor SFTP: primero se descarga
    let mut got = 0u64;
    if let Err(e) = crate::sftp::ensure_local_with(src, &prog.cancel, &mut |n| {
        prog.done.fetch_add(n.saturating_sub(got), AO::Relaxed);
        got = got.max(n);
    }) {
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
            return Err(std::io::Error::new(std::io::ErrorKind::Interrupted, crate::i18n::t("cancelado")));
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
    // los errores de escritura diferidos (disco lleno, red) salen aquí: en la web, `flush` es el envío
    w.flush()?;
    if prog.durable.load(AO::Relaxed) {
        sync_soft(w.sync_all())?;
    }
    let _ = w.set_permissions(md.permissions());
    if let Ok(m) = md.modified() {
        let _ = w.set_modified(m);
    }
    // NFS/SMB pueden informar del fallo de escritura al cerrar: se cierra a mano y se mira
    #[cfg(unix)]
    {
        use std::os::fd::IntoRawFd;
        let fd = w.into_raw_fd();
        if unsafe { libc::close(fd) } != 0 {
            let e = std::io::Error::last_os_error();
            // EINTR: en Linux el descriptor ya está cerrado
            if e.kind() != std::io::ErrorKind::Interrupted {
                return Err(e);
            }
        }
    }
    #[cfg(not(unix))]
    drop(w);
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
        return Err(std::io::Error::new(std::io::ErrorKind::Interrupted, crate::i18n::t("cancelado")));
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
                        return Err(std::io::Error::new(std::io::ErrorKind::Interrupted, crate::i18n::t("cancelado")));
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
                // se copia el enlace, no lo que apunta (puede requerir el modo de desarrollador;
                // si no se puede crear, se anota el fallo en vez de seguirlo)
                use std::os::windows::fs::FileTypeExt;
                if md.file_type().is_symlink_dir() {
                    std::os::windows::fs::symlink_dir(target, dst)
                } else {
                    std::os::windows::fs::symlink_file(target, dst)
                }
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

/// Copia o mueve `src` a `dst`, que no existe. Mover es un `rename`; sólo si el destino está en otro
/// sistema de archivos se copia y, cuando la copia está completa y cerrada, se borra el origen.
/// Si falla o se cancela, no queda nada a medias en `dst` y el origen sigue intacto.
/// `xdev` fuerza el camino de copia (pruebas).
fn transfer(src: &Path, dst: &Path, mv: bool, prog: &Progress, buf: &mut Vec<u8>, cost: u64, xdev: bool) -> std::io::Result<()> {
    if !mv {
        // copiar a un sitio libre: lo que sí se copió se queda (los fallos ya están anotados uno a uno,
        // como al combinar); sólo la cancelación retira la copia a medias
        return match copy_tree(src, dst, prog, buf) {
            Ok(0) => Ok(()),
            Ok(_) if fs::symlink_metadata(dst).is_ok() => Ok(()),
            Ok(_) => Err(std::io::Error::other(t("la copia no se completó"))),
            Err(e) => {
                let _ = remove_any(dst);
                Err(e)
            }
        };
    }
    if stage(src, dst, mv, prog, buf, cost, xdev)? {
        return Ok(());
    }
    if mv {
        remove_any(src)?;
    }
    Ok(())
}

/// Pone una copia completa de `src` en `dst` (que no existe). Devuelve `true` si fue un `rename`
/// (el origen ya no está) y `false` si se copió (el origen sigue ahí).
fn stage(src: &Path, dst: &Path, mv: bool, prog: &Progress, buf: &mut Vec<u8>, cost: u64, xdev: bool) -> std::io::Result<bool> {
    if mv && !xdev {
        match fs::rename(src, dst) {
            Ok(()) => {
                prog.done.fetch_add(cost, AO::Relaxed);
                return Ok(true);
            }
            // sólo «otro sistema de archivos» justifica copiar; cualquier otro fallo se informa tal cual
            Err(e) if !cross_device(&e) => return Err(e),
            Err(_) => {}
        }
    }
    match copy_tree(src, dst, prog, buf) {
        Ok(0) => Ok(false),
        Ok(_) => {
            // copia incompleta: se retira y se conserva el origen
            let _ = remove_any(dst);
            Err(std::io::Error::other(t("la copia no se completó")))
        }
        Err(e) => {
            let _ = remove_any(dst);
            Err(e)
        }
    }
}

/// Sustituye `old` por `src` (copiado o movido) sin destruir nada hasta que lo nuevo está completo
/// junto a él con un nombre temporal. Lo anterior va a la papelera si la hay; si no, se aparta con
/// otro nombre temporal y sólo se borra cuando lo nuevo ya ocupa su sitio. Ante cualquier fallo el
/// destino queda como estaba y el origen se conserva.
fn replace_with(src: &Path, old: &Path, mv: bool, prog: &Progress, buf: &mut Vec<u8>, cost: u64, xdev: bool) -> std::io::Result<()> {
    let dir = old.parent().unwrap_or(Path::new("."));
    let tmp = tmp_beside(dir);
    let renamed = stage(src, &tmp, mv, prog, buf, cost, xdev)?;
    // deshace la preparación: el contenido nuevo vuelve a su origen o se descarta la copia
    let unstage = || {
        if renamed {
            let _ = fs::rename(&tmp, src);
        } else {
            let _ = remove_any(&tmp);
        }
    };
    let retired = retire_to_trash(old);
    if let Retire::Failed(e) = &retired {
        // se prometió que iría a la papelera: si no la acepta, no se borra sin preguntar
        unstage();
        return Err(std::io::Error::other(tf("no se pudo mover a la papelera lo que había: {e}", &[&e])));
    }
    if retired == Retire::Done {
        // lo anterior está en la papelera (recuperable); falta colocar lo nuevo
        if let Err(e) = fs::rename(&tmp, old) {
            // se saca de la papelera para dejar el destino como estaba
            if let Err(e2) = unretire(old) {
                note_error(prog, old, &std::io::Error::other(tf("sigue en la papelera: {e}", &[&e2])));
            }
            unstage();
            return Err(e);
        }
        crate::thumbs::lock(&prog.replaced).push((old.to_path_buf(), true));
    } else {
        let bak = tmp_beside(dir);
        if let Err(e) = fs::rename(old, &bak) {
            unstage();
            return Err(e);
        }
        if let Err(e) = fs::rename(&tmp, old) {
            let _ = fs::rename(&bak, old);
            unstage();
            return Err(e);
        }
        // lo nuevo ya está en su sitio: ahora sí se borra lo anterior (deshacer no podrá recuperarlo)
        crate::thumbs::lock(&prog.replaced).push((old.to_path_buf(), false));
        if let Err(e) = remove_any(&bak) {
            note_error(prog, &bak, &e);
        }
    }
    if mv && !renamed {
        remove_any(src)?;
    }
    Ok(())
}

#[derive(PartialEq, Eq)]
enum Retire {
    /// está en la papelera
    Done,
    /// aquí no hay papelera (Android, navegador): se aparta y se borra
    NoTrash,
    /// hay papelera pero no lo aceptó
    Failed(String),
}

/// Manda `p` a la papelera. En las pruebas nunca se toca la papelera real del usuario: sólo una
/// papelera falsa registrada para la carpeta temporal de la prueba (`test_trash`); sin ella, como
/// si no hubiera papelera.
fn retire_to_trash(p: &Path) -> Retire {
    #[cfg(test)]
    {
        test_trash::retire(p)
    }
    #[cfg(not(test))]
    {
        if !crate::extras::trash_supported() || crate::vfs::WEB {
            return Retire::NoTrash;
        }
        match crate::extras::trash_delete(p) {
            Ok(()) => Retire::Done,
            Err(e) => Retire::Failed(e),
        }
    }
}

/// Saca `p` de la papelera (lo contrario de `retire_to_trash`).
fn unretire(p: &Path) -> Result<(), String> {
    #[cfg(test)]
    {
        test_trash::restore(p)
    }
    #[cfg(not(test))]
    {
        crate::extras::restore_paths(&[p.to_path_buf()])
    }
}

/// Papelera falsa para las pruebas: lo que se «tira» bajo una raíz registrada se mueve a su carpeta.
#[cfg(test)]
pub(crate) mod test_trash {
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;

    static BINS: Mutex<Vec<(PathBuf, PathBuf)>> = Mutex::new(Vec::new());

    /// A partir de ahora, lo que se tire bajo `root` va a `bin` (que debe existir).
    pub(crate) fn register(root: &Path, bin: &Path) {
        crate::thumbs::lock(&BINS).push((root.to_path_buf(), bin.to_path_buf()));
    }

    pub(crate) fn unregister(root: &Path) {
        crate::thumbs::lock(&BINS).retain(|(r, _)| r != root);
    }

    /// Lo tirado (ruta original, sitio en la papelera falsa), para poder restaurarlo.
    static GONE: Mutex<Vec<(PathBuf, PathBuf)>> = Mutex::new(Vec::new());

    pub(super) fn retire(p: &Path) -> super::Retire {
        let bin = crate::thumbs::lock(&BINS).iter().find(|(r, _)| p.starts_with(r)).map(|(_, b)| b.clone());
        let (Some(bin), Some(name)) = (bin, p.file_name()) else { return super::Retire::NoTrash };
        if !bin.is_dir() {
            return super::Retire::Failed("sin papelera".into());
        }
        let to = super::unique_path_os(&bin, name);
        match std::fs::rename(p, &to) {
            Ok(()) => {
                crate::thumbs::lock(&GONE).push((p.to_path_buf(), to));
                super::Retire::Done
            }
            Err(e) => super::Retire::Failed(e.to_string()),
        }
    }

    pub(super) fn restore(p: &Path) -> Result<(), String> {
        let mut g = crate::thumbs::lock(&GONE);
        let i = g.iter().rposition(|(o, _)| o == p).ok_or(crate::i18n::t("no encontrado en la papelera"))?;
        let (o, at) = g.remove(i);
        std::fs::rename(at, o).map_err(|e| e.to_string())
    }
}

/// «Combinar»: vuelca el contenido de la carpeta `src` en la carpeta existente `dst`. Lo que no
/// existe se copia o mueve entero; las subcarpetas presentes en ambos lados se combinan a su vez; y
/// sólo los elementos que coinciden (archivo con archivo, o tipos distintos) se reemplazan, con la
/// misma seguridad que «Reemplazar». Devuelve los pares (origen, destino) de lo que se colocó, para
/// deshacer elemento a elemento (nunca la carpeta de destino entera, que ya tenía contenido propio).
/// Al mover, las carpetas de origen que quedan vacías se eliminan.
fn merge_tree(src: &Path, dst: &Path, mv: bool, prog: &Progress, buf: &mut Vec<u8>, xdev: bool) -> std::io::Result<Vec<(PathBuf, PathBuf)>> {
    let mut placed = Vec::new();
    let mut failed = 0usize;
    merge_into(src, dst, mv, prog, buf, xdev, &mut placed, &mut failed)?;
    if failed > 0 && placed.is_empty() {
        return Err(std::io::Error::other(t("la copia no se completó")));
    }
    Ok(placed)
}

#[allow(clippy::too_many_arguments)]
fn merge_into(src: &Path, dst: &Path, mv: bool, prog: &Progress, buf: &mut Vec<u8>, xdev: bool, placed: &mut Vec<(PathBuf, PathBuf)>, failed: &mut usize) -> std::io::Result<()> {
    if prog.cancelled() {
        return Err(std::io::Error::new(std::io::ErrorKind::Interrupted, crate::i18n::t("cancelado")));
    }
    let rd = match fs::read_dir(src) {
        Ok(r) => r,
        Err(e) => {
            note_error(prog, src, &e);
            *failed += 1;
            return Ok(());
        }
    };
    prog.done.fetch_add(1, AO::Relaxed);
    for e in rd {
        let e = match e {
            Ok(e) => e,
            Err(e) => {
                note_error(prog, src, &e);
                *failed += 1;
                continue;
            }
        };
        let (sp, dp) = (e.path(), dst.join(e.file_name()));
        // sólo cuenta si se mueve con `rename` (al copiar, el progreso lo dan los bytes copiados); no
        // se mide cada subárbol: el total de la tarea tampoco lo incluye cuando mover es renombrar
        let cost = 0;
        let r = match fs::symlink_metadata(&dp) {
            Err(_) => transfer(&sp, &dp, mv, prog, buf, cost, xdev).map(|()| placed.push((sp.clone(), dp.clone()))),
            Ok(dm) if dm.v_is_dir() && fs::symlink_metadata(&sp).is_ok_and(|m| m.v_is_dir()) => {
                merge_into(&sp, &dp, mv, prog, buf, xdev, placed, failed)?;
                continue;
            }
            Ok(_) => replace_with(&sp, &dp, mv, prog, buf, cost, xdev).map(|()| placed.push((sp.clone(), dp.clone()))),
        };
        match r {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => return Err(e),
            Err(e) => {
                note_error(prog, &sp, &e);
                *failed += 1;
            }
        }
    }
    if mv {
        // sólo si quedó vacía (lo que no se pudo mover sigue en su sitio)
        let _ = fs::remove_dir(src);
    }
    Ok(())
}

pub fn run_op(op: Op, ctx: egui::Context) -> Arc<Progress> {
    let prog = Arc::new(Progress::default());
    let p = prog.clone();
    crate::vfs::thread::spawn(move || {
        match op {
            Op::Copy { srcs, dst, mv, policy } => {
                *p.label.lock().unwrap() = t("Calculando…").into();
                // si después se borra algo (origen, destino reemplazado), cada copia se lleva antes al disco
                p.durable.store(mv || matches!(policy, Policy::Replace | Policy::Merge), AO::Relaxed);
                for s in &srcs {
                    if p.cancelled() {
                        break;
                    }
                    // una carpeta del servidor que no se pudo listar quedaría vacía sin decir nada
                    let errs = crate::sftp::sync_tree(s, &p.cancel);
                    p.errors.lock().unwrap().extend(errs);
                }
                // mover dentro del mismo volumen es un `rename`: no hace falta recorrer el árbol
                let dst_vol = if mv { volume_of(&dst) } else { None };
                let costs: Vec<u64> = srcs
                    .iter()
                    .map(|s| if dst_vol.is_some() && volume_of(s) == dst_vol { 1 } else { tree_size(s, Some(&p.cancel)) })
                    .collect();
                // lo que hay que bajar de un servidor SFTP cuenta además de la copia
                let fetch = crate::sftp::missing_bytes(&srcs);
                p.total.store(costs.iter().fold(fetch, |a, &c| a.saturating_add(c)).max(1), AO::Relaxed);
                let mut buf: Vec<u8> = Vec::new();
                for (src, cost) in srcs.into_iter().zip(costs) {
                    if p.cancelled() {
                        break;
                    }
                    // el nombre real (puede no ser UTF-8) para las rutas; `name` sólo para los mensajes
                    let Some(oname) = src.file_name().map(|n| n.to_os_string()) else {
                        continue;
                    };
                    let name = oname.to_string_lossy().into_owned();
                    if mv && src.parent() == Some(dst.as_path()) {
                        continue;
                    }
                    let canon = |p: &Path| fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
                    if src.v_is_dir() && canon(&dst).starts_with(canon(&src)) {
                        p.errors.lock().unwrap().push(tf("{name}: no se puede copiar dentro de sí misma", &[&name]));
                        continue;
                    }
                    let same_dir = src.parent() == Some(dst.as_path());
                    let mut res: Option<std::io::Result<Vec<(PathBuf, PathBuf)>>> = None;
                    let target = match policy {
                        Policy::Rename => unique_path_os(&dst, &oname),
                        _ if same_dir => unique_path_os(&dst, &oname),
                        Policy::Skip => {
                            let t = dst.join(&oname);
                            if fs::symlink_metadata(&t).is_ok() {
                                p.done.fetch_add(cost, AO::Relaxed);
                                continue;
                            }
                            t
                        }
                        Policy::Replace | Policy::Merge => {
                            let t = dst.join(&oname);
                            if let Ok(tm) = fs::symlink_metadata(&t) {
                                // nunca reemplazar el propio origen ni una carpeta que lo contiene
                                if canon(&src).starts_with(canon(&t)) {
                                    p.errors.lock().unwrap().push(tf("{name}: no se puede reemplazar el propio origen o una carpeta que lo contiene", &[&name]));
                                    continue;
                                }
                                let both_dirs = tm.v_is_dir() && fs::symlink_metadata(&src).is_ok_and(|m| m.v_is_dir());
                                res = Some(if policy == Policy::Merge && both_dirs {
                                    merge_tree(&src, &t, mv, &p, &mut buf, false)
                                } else {
                                    // nada se destruye hasta que lo nuevo está completo junto al destino
                                    replace_with(&src, &t, mv, &p, &mut buf, cost, false).map(|()| vec![(src.clone(), t.clone())])
                                });
                            }
                            t
                        }
                    };
                    let tname = target.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    let res = match res {
                        Some(r) => r,
                        None => transfer(&src, &target, mv, &p, &mut buf, cost, false).map(|()| vec![(src.clone(), target.clone())]),
                    };
                    match res {
                        Ok(pairs) => {
                            p.created.lock().unwrap().push(tname);
                            p.pairs.lock().unwrap().extend(pairs);
                        }
                        // cancelado: `transfer`/`replace_with` ya han retirado lo que dejaron a medias
                        Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
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
                let total: u64 = srcs.iter().fold(0u64, |a, s| a.saturating_add(tree_size(s, Some(&p.cancel))));
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
            Op::Fetch(srcs) => {
                *p.label.lock().unwrap() = t("Calculando…").into();
                let errs = crate::sftp::fetch_all(&srcs, &p.cancel, &p.total, &p.done, &p.label);
                p.errors.lock().unwrap().extend(errs);
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
                // en la web cada elemento es una petición: se borra cada carpeta de una vez
                if crate::vfs::WEB {
                    p.total.store(list.len() as u64, AO::Relaxed);
                } else {
                    *p.label.lock().unwrap() = t("Calculando…").into();
                    let n = list.iter().fold(0u64, |a, it| a.saturating_add(count_tree(it, &p.cancel)));
                    p.total.store(n.max(1), AO::Relaxed);
                    ctx.request_repaint();
                }
                for it in list {
                    if p.cancelled() {
                        break;
                    }
                    *p.label.lock().unwrap() = it.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    let r = if crate::vfs::WEB {
                        let r = if it.v_is_dir() && !fs::symlink_metadata(&it).map(|m| m.file_type().v_is_symlink()).unwrap_or(false) {
                            fs::remove_dir_all(&it)
                        } else {
                            fs::remove_file(&it)
                        };
                        p.done.fetch_add(1, AO::Relaxed);
                        r
                    } else {
                        delete_tree(&it, &p, &ctx)
                    };
                    match r {
                        Ok(()) => {}
                        Err(e) if e.kind() == std::io::ErrorKind::Interrupted && p.cancelled() => {}
                        Err(e) => p.errors.lock().unwrap().push(format!("{}: {e}", it.display())),
                    }
                    ctx.request_repaint();
                }
            }
        }
        p.finished.store(true, AO::Release);
        ctx.request_repaint();
    });
    prog
}

/// Un archivo que el sistema no pudo abrir: se avisa en la interfaz (`take_open_fails`).
pub struct OpenFail {
    pub path: PathBuf,
    pub msg: String,
    /// no hay aplicación asociada: conviene ofrecer «Abrir con…»
    pub pick: bool,
}

static OPEN_FAILS: Mutex<Vec<OpenFail>> = Mutex::new(Vec::new());
static OPEN_CTX: std::sync::OnceLock<egui::Context> = std::sync::OnceLock::new();

/// La interfaz da su contexto para que un fallo al abrir (que llega desde otro hilo) se vea en seguida.
pub fn set_open_ctx(ctx: &egui::Context) {
    if OPEN_CTX.get().is_none() {
        let _ = OPEN_CTX.set(ctx.clone());
    }
}

/// Los fallos al abrir con la aplicación del sistema desde la última llamada.
pub fn take_open_fails() -> Vec<OpenFail> {
    std::mem::take(&mut *crate::thumbs::lock(&OPEN_FAILS))
}

#[allow(dead_code)]
fn open_failed(path: &Path, msg: String, pick: bool) {
    crate::thumbs::lock(&OPEN_FAILS).push(OpenFail { path: path.to_path_buf(), msg, pick: pick && path.v_is_file() });
    if let Some(c) = OPEN_CTX.get() {
        c.request_repaint();
    }
}

pub fn open_external(path: &Path) {
    #[cfg(target_arch = "wasm32")]
    {
        if !path.v_is_dir() {
            crate::vfs::net::open_path(&path.to_string_lossy());
        }
        return;
    }
    #[cfg(not(target_arch = "wasm32"))]
    open_external_native(path);
}

#[cfg(not(target_arch = "wasm32"))]
fn open_external_native(path: &Path) {
    // archivo de un servidor SFTP: se descarga y luego se abre
    // (ya descargado se abre sin más: si no, la llamada de abajo volvería aquí sin fin)
    if crate::sftp::is_remote(path) && !path.v_is_dir() && !crate::sftp::tree_fetched(path) {
        let p = path.to_path_buf();
        crate::vfs::thread::spawn(move || {
            if crate::sftp::ensure_local(&p).is_ok() {
                open_external_native(&p);
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
        match std::process::Command::new(prog).arg(path).stdin(std::process::Stdio::null()).spawn() {
            Ok(mut c) => {
                let p = path.to_path_buf();
                // sin esperar al proceso quedaría zombi; su código dice si hubo aplicación que lo abriera
                crate::vfs::thread::spawn(move || {
                    let t0 = Instant::now();
                    let Ok(st) = c.wait() else { return };
                    // xdg-open puede quedarse esperando a la aplicación y devolver su código al cerrarla:
                    // sólo cuenta un fallo inmediato
                    if st.success() || t0.elapsed() > Duration::from_secs(20) {
                        return;
                    }
                    let code = st.code().unwrap_or(-1);
                    // xdg-open: 2 = no existe, 3 = falta una herramienta, 4 = la acción falló (sin aplicación)
                    // open (macOS): 1 = ninguna aplicación lo abre
                    let pick = if cfg!(target_os = "macos") { code == 1 } else { code == 3 || code == 4 };
                    let msg = if pick { t("no hay ninguna aplicación asociada").to_string() } else { tf("{} terminó con el código {}", &[&prog, &code]) };
                    open_failed(&p, msg, pick);
                });
            }
            Err(e) => open_failed(path, format!("{prog}: {e}"), false),
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
        // un valor de 32 o menos es un error; 31 (SE_ERR_NOASSOC): ninguna aplicación asociada
        let r = unsafe { ShellExecuteW(0, op.as_ptr(), file.as_ptr(), std::ptr::null(), std::ptr::null(), 1) };
        if r <= 32 {
            // los demás valores coinciden con los códigos de error de Windows (2, 3, 5, 8…)
            let msg = if r == 31 { t("no hay ninguna aplicación asociada").to_string() } else { std::io::Error::from_raw_os_error(r as i32).to_string() };
            open_failed(path, msg, r == 31);
        }
    }
    #[cfg(target_os = "android")]
    if let Err(e) = crate::android_open(path) {
        open_failed(path, e, false);
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows, target_os = "android")))]
    let _ = path;
}

/// Tamaño legible en el idioma de la interfaz (separador decimal y unidades del idioma).
pub fn human_size(n: u64) -> String {
    human_size_in(crate::i18n::current(), n)
}

/// Como `human_size`, en el idioma `lang` (índice de `i18n::LANGS`). Base 1024, un decimal.
pub fn human_size_in(lang: usize, n: u64) -> String {
    let code = crate::i18n::LANGS.get(lang).map(|l| l.0).unwrap_or("en");
    let u: [&str; 5] = match code {
        "fr" => ["o", "Ko", "Mo", "Go", "To"],
        "ru" => ["Б", "КБ", "МБ", "ГБ", "ТБ"],
        _ => ["B", "KB", "MB", "GB", "TB"],
    };
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i < 4 {
        v /= 1024.0;
        i += 1;
    }
    // 1023,96 KB se redondearía a «1024,0 KB»: pasa a la unidad siguiente
    if i > 0 && i < 4 && (v * 10.0).round() >= 10240.0 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        return format!("{n} {}", u[0]);
    }
    let s = format!("{v:.1}");
    let s = if decimal_comma(code) { s.replace('.', ",") } else { s };
    format!("{s} {}", u[i])
}

/// ¿El idioma usa coma decimal?
fn decimal_comma(code: &str) -> bool {
    matches!(code, "es" | "fr" | "de" | "pt" | "it" | "ru")
}

/// Formato de fechas elegido en Opciones.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DateStyle {
    /// el del idioma, con «hoy» / «ayer» en las listas
    Auto,
    /// AAAA-MM-DD HH:MM
    Iso,
    /// numérico corto, en el orden del idioma
    Short,
    /// con el nombre del mes
    Long,
}

impl DateStyle {
    pub const ALL: [DateStyle; 4] = [DateStyle::Auto, DateStyle::Iso, DateStyle::Short, DateStyle::Long];

    /// Valor guardado en la configuración.
    pub fn code(self) -> &'static str {
        match self {
            DateStyle::Auto => "auto",
            DateStyle::Iso => "iso",
            DateStyle::Short => "short",
            DateStyle::Long => "long",
        }
    }

    pub fn from_code(s: &str) -> DateStyle {
        DateStyle::ALL.into_iter().find(|d| d.code() == s.trim()).unwrap_or(DateStyle::Auto)
    }

    /// Nombre en Opciones (clave de traducción).
    pub fn label(self) -> &'static str {
        match self {
            DateStyle::Auto => "Automático (según el idioma)",
            DateStyle::Iso => "ISO 8601 (AAAA-MM-DD)",
            DateStyle::Short => "Corto",
            DateStyle::Long => "Largo (con el nombre del mes)",
        }
    }
}

static DATE_STYLE: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

/// Fija el formato de fecha de toda la interfaz.
pub fn set_date_style(s: DateStyle) {
    DATE_STYLE.store(DateStyle::ALL.iter().position(|d| *d == s).unwrap_or(0) as u8, AO::Relaxed);
}

pub fn date_style() -> DateStyle {
    DateStyle::ALL.get(DATE_STYLE.load(AO::Relaxed) as usize).copied().unwrap_or(DateStyle::Auto)
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

/// Año, mes y día de un número de días desde 1970-01-01 (Howard Hinnant, civil from days).
fn civil(days: i64) -> (i64, i64, i64) {
    let z = days.saturating_add(719468);
    let era = z.div_euclid(146097);
    let doe = z.rem_euclid(146097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe.saturating_add(era.saturating_mul(400));
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y.saturating_add(1) } else { y };
    (y, m, d)
}

/// Segundos Unix pasados a hora local (segundos «de reloj» desde 1970-01-01).
fn local_secs(secs: i64) -> i64 {
    secs.saturating_add(local_offset(secs))
}

/// Fecha local "AAAA-MM-DD HH:MM:SS" (la que entiende `parse_time`; para campos editables y nombres).
pub fn fmt_time_iso(secs: i64) -> String {
    if secs <= 0 {
        return String::new();
    }
    let secs = local_secs(secs);
    let (y, m, d) = civil(secs.div_euclid(86400));
    let rem = secs.rem_euclid(86400);
    format!("{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02}", rem / 3600, (rem % 3600) / 60, rem % 60)
}

/// Fecha local para listas y cabeceras: en el formato elegido y el idioma de la interfaz, sin segundos
/// (con el formato automático, «Hoy 14:05» / «Ayer 09:30»).
pub fn fmt_time(secs: i64) -> String {
    if secs <= 0 {
        return String::new();
    }
    let now = local_secs(web_time::SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0));
    fmt_time_in(crate::i18n::current(), date_style(), local_secs(secs), Some(now.div_euclid(86400)), false)
}

/// Fecha local en el estilo `style` (sin segundos ni «hoy»), p. ej. para enseñar un ejemplo de cada formato.
pub fn fmt_time_style(secs: i64, style: DateStyle) -> String {
    fmt_time_in(crate::i18n::current(), style, local_secs(secs), None, false)
}

/// Fecha local completa (con segundos), para Propiedades.
pub fn fmt_time_full(secs: i64) -> String {
    if secs <= 0 {
        return String::new();
    }
    fmt_time_in(crate::i18n::current(), date_style(), local_secs(secs), None, true)
}

/// Formatea `local` (segundos de reloj local desde 1970) en el idioma `lang` y el estilo `style`.
/// `today` (días locales desde 1970) activa «Hoy» / «Ayer» con el estilo automático; `full` añade los segundos.
pub fn fmt_time_in(lang: usize, style: DateStyle, local: i64, today: Option<i64>, full: bool) -> String {
    let code = crate::i18n::LANGS.get(lang).map(|l| l.0).unwrap_or("en");
    let days = local.div_euclid(86400);
    let rem = local.rem_euclid(86400);
    let (y, m, d) = civil(days);
    let (hh, mi, ss) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    let sec = |s: String| if full { format!("{s}:{ss:02}") } else { s };
    // hora: 24 h salvo en inglés (12 h con AM/PM)
    let clock = if code == "en" && style != DateStyle::Iso {
        let h12 = if hh % 12 == 0 { 12 } else { hh % 12 };
        format!("{} {}", sec(format!("{h12}:{mi:02}")), if hh < 12 { "AM" } else { "PM" })
    } else {
        sec(format!("{hh:02}:{mi:02}"))
    };
    if style == DateStyle::Auto {
        if let Some(td) = today {
            if days == td {
                return crate::i18n::t_in(lang, "Hoy {}").replacen("{}", &clock, 1);
            }
            if days.checked_add(1) == Some(td) {
                return crate::i18n::t_in(lang, "Ayer {}").replacen("{}", &clock, 1);
            }
        }
    }
    let mi12 = (m.clamp(1, 12) - 1) as usize;
    let date = match style {
        DateStyle::Iso => format!("{y:04}-{m:02}-{d:02}"),
        DateStyle::Auto | DateStyle::Short => match code {
            "en" => format!("{m:02}/{d:02}/{y:04}"),
            "de" | "ru" => format!("{d:02}.{m:02}.{y:04}"),
            "zh" | "ja" => format!("{y:04}/{m:02}/{d:02}"),
            "ko" => format!("{y:04}. {m:02}. {d:02}."),
            _ => format!("{d:02}/{m:02}/{y:04}"),
        },
        DateStyle::Long => {
            let mon = month_abbr(code)[mi12];
            match code {
                "en" => format!("{mon} {d}, {y},"),
                "de" => format!("{d}. {mon} {y},"),
                "fr" => format!("{d} {mon} {y}"),
                "zh" | "ja" => format!("{y}年{m}月{d}日"),
                "ko" => format!("{y}년 {m}월 {d}일"),
                _ => format!("{d} {mon} {y},"),
            }
        }
    };
    format!("{date} {clock}")
}

/// Meses abreviados por idioma (ruso en genitivo, como se escribe tras el día).
fn month_abbr(code: &str) -> [&'static str; 12] {
    match code {
        "es" => ["ene", "feb", "mar", "abr", "may", "jun", "jul", "ago", "sept", "oct", "nov", "dic"],
        "fr" => ["janv.", "févr.", "mars", "avr.", "mai", "juin", "juil.", "août", "sept.", "oct.", "nov.", "déc."],
        "de" => ["Jan.", "Feb.", "März", "Apr.", "Mai", "Juni", "Juli", "Aug.", "Sept.", "Okt.", "Nov.", "Dez."],
        "pt" => ["jan.", "fev.", "mar.", "abr.", "mai.", "jun.", "jul.", "ago.", "set.", "out.", "nov.", "dez."],
        "it" => ["gen", "feb", "mar", "apr", "mag", "giu", "lug", "ago", "set", "ott", "nov", "dic"],
        "ru" => ["янв.", "февр.", "мар.", "апр.", "мая", "июн.", "июл.", "авг.", "сент.", "окт.", "нояб.", "дек."],
        // zh/ja/ko no los usan (fecha numérica con 年月日)
        _ => ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"],
    }
}

#[cfg(test)]
mod time_tests {
    use super::*;

    #[test]
    fn parse_time_roundtrip() {
        for secs in [86_400i64 * 365, 1_700_000_000, 1_760_000_123] {
            assert_eq!(parse_time(&fmt_time_iso(secs)), Some(secs));
        }
        assert_eq!(parse_time("2026-13-01 00:00"), None);
        assert!(parse_time("2026-10-12 10:30").is_some());
    }

    fn lang(code: &str) -> usize {
        crate::i18n::index_of(code).unwrap()
    }

    /// 2026-11-17 14:05:09 en hora local (segundos de reloj desde 1970).
    const T: i64 = 20774 * 86400 + 14 * 3600 + 5 * 60 + 9;

    #[test]
    fn civil_conocido() {
        assert_eq!(civil(0), (1970, 1, 1));
        assert_eq!(civil(20774), (2026, 11, 17));
        assert_eq!(civil(-1), (1969, 12, 31));
        assert_eq!(civil(11016), (2000, 2, 29));
    }

    #[test]
    fn fechas_en_los_diez_idiomas() {
        let want: [(&str, &str, &str, &str); 10] = [
            // (idioma, corto, largo, corto con segundos)
            ("es", "17/11/2026 14:05", "17 nov 2026, 14:05", "17/11/2026 14:05:09"),
            ("en", "11/17/2026 2:05 PM", "Nov 17, 2026, 2:05 PM", "11/17/2026 2:05:09 PM"),
            ("fr", "17/11/2026 14:05", "17 nov. 2026 14:05", "17/11/2026 14:05:09"),
            ("de", "17.11.2026 14:05", "17. Nov. 2026, 14:05", "17.11.2026 14:05:09"),
            ("pt", "17/11/2026 14:05", "17 nov. 2026, 14:05", "17/11/2026 14:05:09"),
            ("it", "17/11/2026 14:05", "17 nov 2026, 14:05", "17/11/2026 14:05:09"),
            ("ru", "17.11.2026 14:05", "17 нояб. 2026, 14:05", "17.11.2026 14:05:09"),
            ("zh", "2026/11/17 14:05", "2026年11月17日 14:05", "2026/11/17 14:05:09"),
            ("ja", "2026/11/17 14:05", "2026年11月17日 14:05", "2026/11/17 14:05:09"),
            ("ko", "2026. 11. 17. 14:05", "2026년 11월 17일 14:05", "2026. 11. 17. 14:05:09"),
        ];
        for (code, short, long, full) in want {
            let l = lang(code);
            assert_eq!(fmt_time_in(l, DateStyle::Short, T, None, false), short, "{code} corto");
            assert_eq!(fmt_time_in(l, DateStyle::Long, T, None, false), long, "{code} largo");
            assert_eq!(fmt_time_in(l, DateStyle::Short, T, None, true), full, "{code} completo");
            // automático sin «hoy»: igual que el corto
            assert_eq!(fmt_time_in(l, DateStyle::Auto, T, Some(20000), false), short, "{code} auto");
            // ISO igual en todos, en 24 h
            assert_eq!(fmt_time_in(l, DateStyle::Iso, T, Some(20774), false), "2026-11-17 14:05", "{code} iso");
            assert_eq!(fmt_time_in(l, DateStyle::Iso, T, None, true), "2026-11-17 14:05:09", "{code} iso completo");
            // hoy / ayer sólo con el automático y sin segundos en la lista
            let hoy = fmt_time_in(l, DateStyle::Auto, T, Some(20774), false);
            let ayer = fmt_time_in(l, DateStyle::Auto, T, Some(20775), false);
            assert!(!hoy.contains("2026") && !ayer.contains("2026") && hoy != ayer, "{code}: {hoy} / {ayer}");
            assert!(hoy.contains(if code == "en" { "2:05 PM" } else { "14:05" }), "{code}: {hoy}");
            assert_eq!(fmt_time_in(l, DateStyle::Auto, T, Some(20776), false), short, "{code} anteayer");
        }
        assert_eq!(fmt_time_in(lang("es"), DateStyle::Auto, T, Some(20774), false), "Hoy 14:05");
        assert_eq!(fmt_time_in(lang("en"), DateStyle::Auto, T, Some(20775), false), "Yesterday 2:05 PM");
        // medianoche y mediodía en 12 h
        assert_eq!(fmt_time_in(lang("en"), DateStyle::Short, 20774 * 86400, None, false), "11/17/2026 12:00 AM");
        assert_eq!(fmt_time_in(lang("en"), DateStyle::Short, 20774 * 86400 + 12 * 3600, None, false), "11/17/2026 12:00 PM");
    }

    #[test]
    fn estilo_guardado() {
        for s in DateStyle::ALL {
            assert_eq!(DateStyle::from_code(s.code()), s);
        }
        assert_eq!(DateStyle::from_code("raro"), DateStyle::Auto);
    }

    #[test]
    fn tamanos_en_los_diez_idiomas() {
        let want: [(&str, &str, &str, &str); 10] = [
            ("es", "512 B", "1,5 KB", "2,0 GB"),
            ("en", "512 B", "1.5 KB", "2.0 GB"),
            ("fr", "512 o", "1,5 Ko", "2,0 Go"),
            ("de", "512 B", "1,5 KB", "2,0 GB"),
            ("pt", "512 B", "1,5 KB", "2,0 GB"),
            ("it", "512 B", "1,5 KB", "2,0 GB"),
            ("ru", "512 Б", "1,5 КБ", "2,0 ГБ"),
            ("zh", "512 B", "1.5 KB", "2.0 GB"),
            ("ja", "512 B", "1.5 KB", "2.0 GB"),
            ("ko", "512 B", "1.5 KB", "2.0 GB"),
        ];
        for (code, b, k, g) in want {
            let l = lang(code);
            assert_eq!(human_size_in(l, 512), b, "{code}");
            assert_eq!(human_size_in(l, 1536), k, "{code}");
            assert_eq!(human_size_in(l, 2 << 30), g, "{code}");
        }
        let en = lang("en");
        // sin «1024.0 KB»: pasa a MB
        assert_eq!(human_size_in(en, 1024 * 1024 - 1), "1.0 MB");
        assert_eq!(human_size_in(en, 0), "0 B");
        assert_eq!(human_size_in(en, u64::MAX), "16777216.0 TB");
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

#[cfg(test)]
mod safe_tests {
    use super::*;

    /// Carpeta temporal propia de cada prueba.
    fn tmpd(tag: &str) -> PathBuf {
        let n = web_time::SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
        let d = crate::vfs::temp_dir().join(format!("fbw1-{tag}-{}-{n:x}", crate::vfs::pid()));
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn names(d: &Path) -> Vec<String> {
        let mut v: Vec<String> = fs::read_dir(d).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        v.sort();
        v
    }

    fn run_pol(src: &Path, dst: &Path, mv: bool, policy: Policy) -> (Vec<String>, Vec<(PathBuf, PathBuf)>) {
        let p = run_op(Op::Copy { srcs: vec![src.to_path_buf()], dst: dst.to_path_buf(), mv, policy }, egui::Context::default());
        let t0 = Instant::now();
        while !p.finished.load(AO::Acquire) {
            assert!(t0.elapsed() < Duration::from_secs(60), "timeout");
            crate::vfs::thread::sleep(Duration::from_millis(10));
        }
        let e = p.errors.lock().unwrap().clone();
        let pairs = p.pairs.lock().unwrap().clone();
        (e, pairs)
    }

    /// Un FIFO dentro de una carpeta hace que su copia quede incompleta (simula un fallo a mitad).
    #[cfg(unix)]
    fn fifo(p: &Path) {
        use std::os::unix::ffi::OsStrExt;
        let c = std::ffi::CString::new(p.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
    }

    #[test]
    fn move_replace_same_fs_file_and_dir() {
        let b = tmpd("mvrep");
        let (s, d) = (b.join("s"), b.join("d"));
        fs::create_dir_all(s.join("dir")).unwrap();
        fs::create_dir_all(d.join("dir")).unwrap();
        fs::write(s.join("f.txt"), b"nuevo").unwrap();
        fs::write(d.join("f.txt"), b"viejo").unwrap();
        fs::write(s.join("dir/n.txt"), b"n").unwrap();
        fs::write(d.join("dir/o.txt"), b"o").unwrap();
        let (e, pairs) = run_pol(&s.join("f.txt"), &d, true, Policy::Replace);
        assert!(e.is_empty(), "{e:?}");
        assert_eq!(pairs, vec![(s.join("f.txt"), d.join("f.txt"))]);
        let (e, _) = run_pol(&s.join("dir"), &d, true, Policy::Replace);
        assert!(e.is_empty(), "{e:?}");
        assert_eq!(fs::read(d.join("f.txt")).unwrap(), b"nuevo");
        assert_eq!(names(&d.join("dir")), vec!["n.txt"]);
        assert_eq!(names(&d), vec!["dir", "f.txt"], "sin temporales sobrantes");
        assert!(names(&s).is_empty(), "el origen se ha movido");
        let _ = fs::remove_dir_all(&b);
    }

    #[test]
    fn move_replace_cross_device_ok() {
        let b = tmpd("xdevok");
        let (s, d) = (b.join("s"), b.join("d"));
        fs::create_dir_all(s.join("t")).unwrap();
        fs::create_dir_all(d.join("t")).unwrap();
        fs::write(s.join("t/a.txt"), b"nuevo").unwrap();
        fs::write(d.join("t/viejo.txt"), b"viejo").unwrap();
        let prog = Progress::default();
        let mut buf = vec![0u8; 4096];
        replace_with(&s.join("t"), &d.join("t"), true, &prog, &mut buf, 1, true).unwrap();
        assert_eq!(names(&d), vec!["t"]);
        assert_eq!(names(&d.join("t")), vec!["a.txt"]);
        assert!(!s.join("t").exists(), "tras copiar entero, el origen se borra");
        let _ = fs::remove_dir_all(&b);
    }

    /// La copia falla a mitad (otro sistema de archivos): el destino queda intacto y el origen también.
    #[cfg(unix)]
    #[test]
    fn move_replace_cross_device_failure_loses_nothing() {
        let b = tmpd("xdevfail");
        let (s, d) = (b.join("s"), b.join("d"));
        fs::create_dir_all(s.join("t")).unwrap();
        fs::create_dir_all(d.join("t")).unwrap();
        fs::write(s.join("t/a.txt"), b"nuevo").unwrap();
        fifo(&s.join("t/pipe"));
        fs::write(d.join("t/viejo.txt"), b"viejo").unwrap();
        let prog = Progress::default();
        let mut buf = vec![0u8; 4096];
        assert!(replace_with(&s.join("t"), &d.join("t"), true, &prog, &mut buf, 1, true).is_err());
        assert_eq!(names(&d), vec!["t"], "sin temporales sobrantes");
        assert_eq!(fs::read(d.join("t/viejo.txt")).unwrap(), b"viejo");
        assert_eq!(names(&s.join("t")), vec!["a.txt", "pipe"]);
        // lo mismo con un mover simple (sin destino previo)
        assert!(transfer(&s.join("t"), &d.join("u"), true, &prog, &mut buf, 1, true).is_err());
        assert!(fs::symlink_metadata(d.join("u")).is_err());
        assert_eq!(names(&s.join("t")), vec!["a.txt", "pipe"]);
        let _ = fs::remove_dir_all(&b);
    }

    /// Copiar y reemplazar con una copia que falla tampoco toca el destino.
    #[cfg(unix)]
    #[test]
    fn copy_replace_failure_keeps_destination() {
        let b = tmpd("cpfail");
        let (s, d) = (b.join("s"), b.join("d"));
        fs::create_dir_all(s.join("t")).unwrap();
        fs::create_dir_all(d.join("t")).unwrap();
        fifo(&s.join("t/pipe"));
        fs::write(d.join("t/viejo.txt"), b"viejo").unwrap();
        let (e, _) = run_pol(&s.join("t"), &d, false, Policy::Replace);
        assert!(!e.is_empty());
        assert_eq!(names(&d), vec!["t"]);
        assert_eq!(names(&d.join("t")), vec!["viejo.txt"]);
        let _ = fs::remove_dir_all(&b);
    }

    fn merge_fixture(b: &Path) -> (PathBuf, PathBuf) {
        let (s, d) = (b.join("s").join("top"), b.join("d").join("top"));
        fs::create_dir_all(s.join("sub")).unwrap();
        fs::create_dir_all(d.join("sub")).unwrap();
        fs::write(s.join("a.txt"), b"nuevo").unwrap();
        fs::write(s.join("sub/b.txt"), b"nuevo").unwrap();
        fs::write(s.join("sub/c.txt"), b"c").unwrap();
        fs::write(d.join("a.txt"), b"viejo").unwrap();
        fs::write(d.join("keep.txt"), b"k").unwrap();
        fs::write(d.join("sub/b.txt"), b"viejo").unwrap();
        fs::write(d.join("sub/d.txt"), b"d").unwrap();
        (s, d)
    }

    fn check_merged(d: &Path) {
        assert_eq!(names(d), vec!["a.txt", "keep.txt", "sub"]);
        assert_eq!(names(&d.join("sub")), vec!["b.txt", "c.txt", "d.txt"]);
        assert_eq!(fs::read(d.join("a.txt")).unwrap(), b"nuevo");
        assert_eq!(fs::read(d.join("sub/b.txt")).unwrap(), b"nuevo");
    }

    #[test]
    fn merge_copy_and_move() {
        let b = tmpd("merge");
        let (s, d) = merge_fixture(&b);
        let (e, pairs) = run_pol(&s, d.parent().unwrap(), false, Policy::Merge);
        assert!(e.is_empty(), "{e:?}");
        check_merged(&d);
        assert_eq!(names(&s), vec!["a.txt", "sub"], "copiar no toca el origen");
        // se deshace elemento a elemento, nunca la carpeta de destino entera
        assert!(pairs.iter().all(|(_, t)| t != &d));
        assert_eq!(pairs.len(), 3);
        let _ = fs::remove_dir_all(&b);

        let b = tmpd("mergemv");
        let (s, d) = merge_fixture(&b);
        let (e, _) = run_pol(&s, d.parent().unwrap(), true, Policy::Merge);
        assert!(e.is_empty(), "{e:?}");
        check_merged(&d);
        assert!(fs::symlink_metadata(&s).is_err(), "al mover, las carpetas vaciadas se quitan");
        let _ = fs::remove_dir_all(&b);

        // y por el camino de copiar y borrar (otro sistema de archivos)
        let b = tmpd("mergexdev");
        let (s, d) = merge_fixture(&b);
        let prog = Progress::default();
        let mut buf = vec![0u8; 4096];
        let pairs = merge_tree(&s, &d, true, &prog, &mut buf, true).unwrap();
        assert_eq!(pairs.len(), 3);
        check_merged(&d);
        assert!(fs::symlink_metadata(&s).is_err());
        let _ = fs::remove_dir_all(&b);
    }

    #[cfg(unix)]
    #[test]
    fn copy_keeps_symlinks_as_links() {
        let b = tmpd("symcp");
        let s = b.join("s");
        fs::create_dir_all(s.join("real")).unwrap();
        fs::write(s.join("real/x.txt"), b"x").unwrap();
        std::os::unix::fs::symlink("real", s.join("ldir")).unwrap();
        std::os::unix::fs::symlink("/nonexistent/abc", s.join("lbroken")).unwrap();
        let d = b.join("d");
        fs::create_dir_all(&d).unwrap();
        let (e, _) = run_pol(&s, &d, false, Policy::Rename);
        assert!(e.is_empty(), "{e:?}");
        let c = d.join("s");
        assert!(fs::symlink_metadata(c.join("ldir")).unwrap().file_type().is_symlink());
        assert_eq!(fs::read_link(c.join("ldir")).unwrap(), Path::new("real"));
        assert_eq!(fs::read_link(c.join("lbroken")).unwrap(), Path::new("/nonexistent/abc"));
        let _ = fs::remove_dir_all(&b);
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_names_survive() {
        use std::os::unix::ffi::OsStrExt;
        let b = tmpd("nonutf8");
        let raw = std::ffi::OsStr::from_bytes(b"caf\xe9.txt");
        let s = b.join("s");
        fs::create_dir_all(&s).unwrap();
        fs::write(s.join(raw), b"x").unwrap();
        let d = b.join("d");
        fs::create_dir_all(&d).unwrap();
        // dos veces con «Renombrar»: la segunda crea «caf\xe9 (2).txt» con los bytes originales
        for _ in 0..2 {
            let (e, _) = run_pol(&s.join(raw), &d, false, Policy::Rename);
            assert!(e.is_empty(), "{e:?}");
        }
        assert!(d.join(raw).exists());
        assert!(d.join(std::ffi::OsStr::from_bytes(b"caf\xe9 (2).txt")).exists());
        // y mover una carpeta con ese nombre
        let rd = std::ffi::OsStr::from_bytes(b"dir\xff");
        fs::create_dir_all(s.join(rd)).unwrap();
        let (e, _) = run_pol(&s.join(rd), &d, true, Policy::Rename);
        assert!(e.is_empty(), "{e:?}");
        assert!(d.join(rd).is_dir() && !s.join(rd).exists());
        let _ = fs::remove_dir_all(&b);
    }

    #[test]
    fn replace_temp_names_are_short() {
        let b = tmpd("tmpname");
        let t = tmp_beside(&b);
        let n = t.file_name().unwrap().to_string_lossy().into_owned();
        assert!(n.starts_with(".fbrep-") && n.len() < 48, "{n}");
        assert_ne!(t, tmp_beside(&b));
        let _ = fs::remove_dir_all(&b);
    }

    fn run_rep(src: &Path, dst: &Path, mv: bool, policy: Policy) -> (Vec<String>, Vec<(PathBuf, bool)>) {
        let p = run_op(Op::Copy { srcs: vec![src.to_path_buf()], dst: dst.to_path_buf(), mv, policy }, egui::Context::default());
        let t0 = Instant::now();
        while !p.finished.load(AO::Acquire) {
            assert!(t0.elapsed() < Duration::from_secs(60), "timeout");
            crate::vfs::thread::sleep(Duration::from_millis(10));
        }
        let e = p.errors.lock().unwrap().clone();
        let r = p.replaced.lock().unwrap().clone();
        (e, r)
    }

    /// Copiar a un sitio libre con un fallo a mitad: lo copiado se queda y el fallo se informa.
    #[cfg(unix)]
    #[test]
    fn copy_partial_keeps_what_was_copied() {
        let b = tmpd("cppart");
        let (s, d) = (b.join("s"), b.join("d"));
        fs::create_dir_all(s.join("t")).unwrap();
        fs::create_dir_all(&d).unwrap();
        fs::write(s.join("t/a.txt"), b"a").unwrap();
        fifo(&s.join("t/pipe"));
        let (e, pairs) = run_pol(&s.join("t"), &d, false, Policy::Rename);
        assert!(!e.is_empty());
        assert_eq!(fs::read(d.join("t/a.txt")).unwrap(), b"a");
        assert_eq!(pairs, vec![(s.join("t"), d.join("t"))], "deshacer puede retirar la copia parcial");
        let _ = fs::remove_dir_all(&b);
    }

    /// Si la papelera no acepta lo que había, no se borra: el reemplazo falla y nada cambia.
    #[test]
    fn replace_trash_failure_keeps_old() {
        let b = tmpd("reptf");
        let (s, d) = (b.join("s"), b.join("d"));
        fs::create_dir_all(&s).unwrap();
        fs::create_dir_all(&d).unwrap();
        fs::write(s.join("f.txt"), b"nuevo").unwrap();
        fs::write(d.join("f.txt"), b"viejo").unwrap();
        // papelera registrada pero inexistente: la «papelera» falla
        test_trash::register(&d, &b.join("no-existe"));
        let (e, r) = run_rep(&s.join("f.txt"), &d, true, Policy::Replace);
        test_trash::unregister(&d);
        assert!(!e.is_empty());
        assert!(r.is_empty());
        assert_eq!(fs::read(d.join("f.txt")).unwrap(), b"viejo");
        assert_eq!(fs::read(s.join("f.txt")).unwrap(), b"nuevo");
        assert_eq!(names(&d), vec!["f.txt"], "sin temporales sobrantes");
        let _ = fs::remove_dir_all(&b);
    }

    #[test]
    fn delete_counts_every_entry() {
        let b = tmpd("del");
        for i in 0..3 {
            fs::create_dir_all(b.join(format!("x/d{i}"))).unwrap();
            for j in 0..5 {
                fs::write(b.join(format!("x/d{i}/f{j}")), b"z").unwrap();
            }
        }
        fs::write(b.join("y.txt"), b"y").unwrap();
        let p = run_op(Op::Delete(vec![b.join("x"), b.join("y.txt")]), egui::Context::default());
        let t0 = Instant::now();
        while !p.finished.load(AO::Acquire) {
            assert!(t0.elapsed() < Duration::from_secs(60), "timeout");
            crate::vfs::thread::sleep(Duration::from_millis(10));
        }
        assert!(p.errors.lock().unwrap().is_empty());
        assert_eq!(p.total.load(AO::Relaxed), 1 + 3 + 15 + 1);
        assert_eq!(p.done.load(AO::Relaxed), p.total.load(AO::Relaxed));
        assert!(names(&b).is_empty());
        let _ = fs::remove_dir_all(&b);
    }

    #[test]
    fn ts_is_text_unless_mpeg_ts() {
        assert_eq!(kind_of_ext("ts"), Kind::Text);
        let mut v = vec![0u8; 188 * 3];
        for k in 0..3 {
            v[k * 188] = 0x47;
        }
        assert!(is_mpeg_ts_bytes(&v));
        assert!(!is_mpeg_ts_bytes(b"export const x: number = 1;\n"));
        let b = tmpd("ts");
        fs::write(b.join("v.ts"), &v).unwrap();
        fs::write(b.join("c.ts"), "let a = 1;\n".repeat(60)).unwrap();
        assert_eq!(entry_from_path(&b.join("v.ts"), "v.ts".into()).unwrap().kind, Kind::Video);
        assert_eq!(entry_from_path(&b.join("c.ts"), "c.ts".into()).unwrap().kind, Kind::Text);
        let _ = fs::remove_dir_all(&b);
    }

    #[test]
    fn case_alias_needs_same_file() {
        let b = tmpd("case");
        fs::write(b.join("foo.txt"), b"1").unwrap();
        assert!(!case_alias(&b.join("foo.txt"), &b.join("Foo.txt")), "no existe «Foo.txt»");
        assert!(!case_alias(&b.join("foo.txt"), &b.join("bar.txt")));
        // en un sistema que distingue mayúsculas son dos archivos distintos
        fs::write(b.join("Foo.txt"), b"2").unwrap();
        if fs::read(b.join("foo.txt")).unwrap() == b"1" {
            assert!(!case_alias(&b.join("foo.txt"), &b.join("Foo.txt")));
        }
        let _ = fs::remove_dir_all(&b);
    }

    /// Lo reemplazado queda anotado, y se distingue lo que está en la papelera (recuperable al
    /// deshacer) de lo que se borró definitivamente. Papelera falsa: nunca la del usuario.
    #[test]
    fn replace_records_trashed_or_deleted() {
        let b = tmpd("reprec");
        let (s, d, bin) = (b.join("s"), b.join("d"), b.join("bin"));
        for x in [&s, &d, &bin] {
            fs::create_dir_all(x).unwrap();
        }
        fs::write(s.join("f.txt"), b"nuevo").unwrap();
        fs::write(d.join("f.txt"), b"viejo").unwrap();
        // sin papelera: se borra definitivamente
        let (e, r) = run_rep(&s.join("f.txt"), &d, false, Policy::Replace);
        assert!(e.is_empty(), "{e:?}");
        assert_eq!(r, vec![(d.join("f.txt"), false)]);
        // con papelera (falsa, sólo para esta carpeta): lo anterior va a ella
        fs::write(d.join("f.txt"), b"viejo").unwrap();
        test_trash::register(&d, &bin);
        let (e, r) = run_rep(&s.join("f.txt"), &d, false, Policy::Replace);
        assert!(e.is_empty(), "{e:?}");
        assert_eq!(r, vec![(d.join("f.txt"), true)]);
        assert_eq!(fs::read(d.join("f.txt")).unwrap(), b"nuevo");
        assert_eq!(fs::read(bin.join("f.txt")).unwrap(), b"viejo");
        // «Combinar»: sólo los archivos que chocan, con su ruta interior
        fs::create_dir_all(s.join("m/sub")).unwrap();
        fs::create_dir_all(d.join("m/sub")).unwrap();
        fs::write(s.join("m/sub/x.txt"), b"nuevo").unwrap();
        fs::write(d.join("m/sub/x.txt"), b"viejo").unwrap();
        fs::write(s.join("m/solo.txt"), b"s").unwrap();
        fs::write(d.join("m/propio.txt"), b"p").unwrap();
        let (e, r) = run_rep(&s.join("m"), &d, true, Policy::Merge);
        test_trash::unregister(&d);
        assert!(e.is_empty(), "{e:?}");
        assert_eq!(r, vec![(d.join("m/sub/x.txt"), true)]);
        assert_eq!(fs::read(d.join("m/sub/x.txt")).unwrap(), b"nuevo");
        assert!(d.join("m/propio.txt").exists() && d.join("m/solo.txt").exists());
        assert_eq!(fs::read(bin.join("x.txt")).unwrap(), b"viejo");
        let _ = fs::remove_dir_all(&b);
    }
}
