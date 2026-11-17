//! Índice de nombres de archivo en memoria para búsquedas instantáneas.
//!
//! Se recorre el disco una vez (en paralelo, en segundo plano) y se guardan solo los nombres,
//! con el padre de cada entrada, en vectores compactos (unos 11 bytes por entrada más el nombre).
//! El índice se guarda en la carpeta de caché para cargarlo al instante en el siguiente arranque
//! y se vuelve a recorrer en segundo plano cuando pasa cierto tiempo.

#[allow(unused_imports)]
use crate::vfs::{KindV as _, PathV as _};
use crate::tools::{Search, MAX_RESULTS};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering as AO};
use std::sync::{Arc, Condvar, Mutex, RwLock};
use std::time::{Duration};
use web_time::{SystemTime, UNIX_EPOCH};

const NO_PARENT: u32 = u32::MAX;
/// 002: un byte de marcas por entrada (carpeta, oculto, sin recorrer) en vez de «es carpeta».
const MAGIC: &[u8; 8] = b"FBIDX002";
/// Marcas de cada entrada.
const F_DIR: u8 = 1;
/// Oculto: empieza por punto o (Windows) tiene el atributo «oculto».
const F_HIDDEN: u8 = 2;
/// Carpeta cuyo contenido no está (montaje de red o virtual, ilegible o tope alcanzado):
/// buscar dentro de ella exige recorrer el disco.
const F_UNWALKED: u8 = 4;
/// Pasado este tiempo desde el último recorrido, se vuelve a indexar en segundo plano.
const STALE_SECS: u64 = 20 * 60;
/// Topes de memoria: con ellos el índice ocupa como mucho unos 175 MB aunque el disco sea enorme
/// (lo que no cabe simplemente no se indexa; la búsqueda normal sigue encontrándolo).
const MAX_ENTRIES: usize = 4_000_000;
const MAX_ARENA: usize = 128 << 20;
/// Las ventanas que no indexan miran cada tanto si la que indexa ha guardado algo más nuevo.
const RELOAD_CHECK_SECS: u64 = 60;

#[derive(Default)]
struct Index {
    arena: Vec<u8>,
    off: Vec<u32>,
    len: Vec<u16>,
    parent: Vec<u32>,
    /// Marcas `F_*`.
    flags: Vec<u8>,
    /// Segundos Unix en los que terminó el recorrido.
    built: u64,
}

impl Index {
    fn count(&self) -> usize {
        self.off.len()
    }

    fn name(&self, i: usize) -> &str {
        let o = self.off[i] as usize;
        std::str::from_utf8(&self.arena[o..o + self.len[i] as usize]).unwrap_or("")
    }

    /// ¿Se ha llegado al tope de entradas o de memoria?
    fn full(&self) -> bool {
        self.off.len() >= MAX_ENTRIES || self.arena.len() >= MAX_ARENA
    }

    fn is_dir(&self, i: usize) -> bool {
        self.flags[i] & F_DIR != 0
    }

    fn push(&mut self, name: &str, parent: u32, flags: u8) -> u32 {
        let id = self.off.len() as u32;
        let b = name.as_bytes();
        let b = &b[..b.len().min(u16::MAX as usize)];
        self.off.push(self.arena.len() as u32);
        self.len.push(b.len() as u16);
        self.arena.extend_from_slice(b);
        self.parent.push(parent);
        self.flags.push(flags);
        id
    }

    fn roots(&self) -> Vec<PathBuf> {
        (0..self.count()).filter(|&i| self.parent[i] == NO_PARENT).map(|i| PathBuf::from(self.name(i))).collect()
    }

    fn path_of(&self, mut i: usize) -> PathBuf {
        let mut parts = Vec::new();
        loop {
            parts.push(i);
            let p = self.parent[i];
            // tope de profundidad: un índice dañado con ciclos de padres no debe colgar la búsqueda
            if p == NO_PARENT || parts.len() > 4096 {
                break;
            }
            i = p as usize;
        }
        let mut out = PathBuf::new();
        for &k in parts.iter().rev() {
            out.push(self.name(k));
        }
        out
    }

    /// Busca la carpeta `path` descendiendo desde las raíces indexadas. `None` también si su
    /// contenido no se indexó (montaje de red, tope…): entonces hay que recorrer el disco.
    fn find_dir(&self, path: &Path) -> Option<u32> {
        let mut best: Option<(u32, usize)> = None;
        for i in 0..self.count() {
            if self.parent[i] == NO_PARENT {
                let r = Path::new(self.name(i));
                if path.starts_with(r) && best.map(|b| r.components().count() > b.1).unwrap_or(true) {
                    best = Some((i as u32, r.components().count()));
                }
            }
        }
        let (mut cur, skip) = best?;
        let comps: Vec<_> = path.components().skip(skip).collect();
        for c in comps {
            let want = c.as_os_str().to_str()?;
            let mut found = None;
            for i in 0..self.count() {
                if self.parent[i] == cur && self.is_dir(i) && self.name(i) == want {
                    found = Some(i as u32);
                    break;
                }
            }
            cur = found?;
        }
        (self.flags[cur as usize] & F_UNWALKED == 0).then_some(cur)
    }

    fn save(&self, file: &Path) {
        let mut b = Vec::with_capacity(self.arena.len() + self.count() * 11 + 64);
        b.extend_from_slice(MAGIC);
        b.extend_from_slice(&self.built.to_le_bytes());
        b.extend_from_slice(&(self.count() as u32).to_le_bytes());
        b.extend_from_slice(&(self.arena.len() as u32).to_le_bytes());
        b.extend_from_slice(&self.arena);
        for v in &self.off {
            b.extend_from_slice(&v.to_le_bytes());
        }
        for v in &self.len {
            b.extend_from_slice(&v.to_le_bytes());
        }
        for v in &self.parent {
            b.extend_from_slice(&v.to_le_bytes());
        }
        b.extend_from_slice(&self.flags);
        if let Some(d) = file.parent() {
            crate::fsys::private_dir(d);
        }
        // otras ventanas leen este archivo: se sustituye de golpe (temporal propio + rename)
        let tmp = file.with_extension(format!("{}.tmp", crate::vfs::pid()));
        if crate::vfs::fs::write(&tmp, &b).is_ok() && crate::vfs::fs::rename(&tmp, file).is_err() {
            let _ = crate::vfs::fs::remove_file(&tmp);
        }
    }

    /// Fecha de construcción de un índice guardado, leyendo sólo la cabecera.
    fn built_on_disk(file: &Path) -> Option<u64> {
        use std::io::Read;
        let mut h = [0u8; 16];
        crate::vfs::fs::File::open(file).ok()?.read_exact(&mut h).ok()?;
        (&h[..8] == MAGIC).then(|| u64::from_le_bytes([h[8], h[9], h[10], h[11], h[12], h[13], h[14], h[15]]))
    }

    fn load(file: &Path) -> Option<Index> {
        let b = crate::vfs::fs::read(file).ok()?;
        if b.len() < 28 || &b[..8] != MAGIC {
            return None;
        }
        let u64_at = |o: usize| u64::from_le_bytes(b[o..o + 8].try_into().unwrap());
        let u32_at = |o: usize| u32::from_le_bytes(b[o..o + 4].try_into().unwrap());
        let built = u64_at(8);
        let n = u32_at(16) as usize;
        let alen = u32_at(20) as usize;
        if n.checked_mul(11).and_then(|x| x.checked_add(24 + alen)) != Some(b.len()) {
            return None;
        }
        let mut o = 24;
        let arena = b[o..o + alen].to_vec();
        o += alen;
        let off: Vec<u32> = (0..n).map(|i| u32_at(o + i * 4)).collect();
        o += n * 4;
        let len: Vec<u16> = (0..n).map(|i| u16::from_le_bytes([b[o + i * 2], b[o + i * 2 + 1]])).collect();
        o += n * 2;
        let parent: Vec<u32> = (0..n).map(|i| u32_at(o + i * 4)).collect();
        o += n * 4;
        let flags: Vec<u8> = b[o..o + n].to_vec();
        // comprobación de coherencia: evita pánicos con un fichero dañado
        if (0..n).any(|i| off[i] as usize + len[i] as usize > alen || (parent[i] != NO_PARENT && parent[i] as usize >= n)) {
            return None;
        }
        // los padres no pueden formar ciclos: un índice manipulado colgaría las búsquedas
        for i in 0..n {
            let (mut cur, mut hops) = (i, 0u32);
            while parent[cur] != NO_PARENT {
                hops += 1;
                if hops > 4096 {
                    return None;
                }
                cur = parent[cur] as usize;
            }
        }
        Some(Index { arena, off, len, parent, flags, built })
    }
}

// ---------------------------------------------------------------------------
// Estado global
// ---------------------------------------------------------------------------

static CURRENT: RwLock<Option<Arc<Index>>> = RwLock::new(None);
static BUILDING: AtomicBool = AtomicBool::new(false);
static PROGRESS: AtomicUsize = AtomicUsize::new(0);
static LOADING: AtomicBool = AtomicBool::new(false);
static GENERATION: AtomicU64 = AtomicU64::new(0);
/// Milisegundos Unix del último cambio detectado en el disco (0 = ninguno pendiente).
static DIRTY: AtomicU64 = AtomicU64::new(0);
/// Cambios más recientes que esto se esperan antes de re-indexar (agrupa ráfagas).
const SETTLE_MS: u64 = 3000;
/// Separación mínima entre dos recorridos provocados por cambios.
#[cfg(target_os = "linux")]
const MIN_GAP_SECS: u64 = 30;
/// En Windows y macOS el sistema genera mucho ruido propio: recorridos más espaciados.
#[cfg(not(target_os = "linux"))]
const MIN_GAP_SECS: u64 = 120;

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// Avisa de que algo cambió en el disco: el índice se refrescará en breve.
pub fn mark_dirty() {
    DIRTY.store(now_ms().max(1), AO::Relaxed);
}

/// Carpetas con cambios aún no recogidos por un recorrido (milisegundos del cambio, carpeta).
/// Las búsquedas las completan leyendo el disco: un archivo recién creado se encuentra ya.
static CHANGED: Mutex<Vec<(u64, PathBuf)>> = Mutex::new(Vec::new());
/// Milisegundos del último desbordamiento (demasiados cambios para apuntarlos uno a uno); 0 = ninguno.
static OVERFLOW: AtomicU64 = AtomicU64::new(0);
const MAX_CHANGED: usize = 512;

/// Como `mark_dirty`, apuntando además la carpeta en la que cambió algo.
#[cfg_attr(not(any(target_os = "linux", windows, target_os = "macos")), allow(dead_code))]
fn mark_changed(dir: PathBuf) {
    let t = now_ms().max(1);
    DIRTY.store(t, AO::Relaxed);
    let mut g = CHANGED.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(e) = g.iter_mut().find(|e| e.1 == dir) {
        e.0 = t;
    } else if g.len() < MAX_CHANGED {
        g.push((t, dir));
    } else {
        OVERFLOW.store(t, AO::Relaxed);
    }
}

/// Cambios sin carpeta conocida (la cola de avisos del sistema se desbordó).
#[cfg_attr(not(any(target_os = "linux", windows, target_os = "macos")), allow(dead_code))]
fn mark_overflow() {
    let t = now_ms().max(1);
    DIRTY.store(t, AO::Relaxed);
    OVERFLOW.store(t, AO::Relaxed);
}

/// Olvida los cambios anteriores a `started`: el recorrido que empezó entonces ya los recoge.
fn forget_changes(started: u64) {
    CHANGED.lock().unwrap_or_else(|e| e.into_inner()).retain(|e| e.0 >= started);
    if OVERFLOW.load(AO::Relaxed) < started {
        OVERFLOW.store(0, AO::Relaxed);
    }
}

/// Nombres de temporales que se crean y desaparecen solos (guardados atómicos, descargas…).
#[cfg_attr(not(any(target_os = "linux", windows, target_os = "macos")), allow(dead_code))]
fn temp_name(n: &str) -> bool {
    n.ends_with(".tmp") || n.ends_with('~') || n.ends_with(".swp") || n.ends_with(".swx") || n.ends_with(".part")
        || n.ends_with(".crdownload") || n.starts_with(".goutputstream-") || n.starts_with(".#")
}

#[derive(Clone, Copy, PartialEq)]
pub enum Status {
    Off,
    /// Recorriendo el disco (y cuántos elementos lleva).
    Building(usize),
    /// Listo: elementos y segundos desde que se construyó.
    Ready(usize, u64),
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn current() -> Option<Arc<Index>> {
    CURRENT.read().ok().and_then(|g| g.clone())
}

fn cache_file() -> PathBuf {
    dirs::cache_dir().unwrap_or_else(crate::vfs::temp_dir).join("filebrowser").join("index.bin")
}

// ---------------------------------------------------------------------------
// Un solo indexador para todas las ventanas
// ---------------------------------------------------------------------------

/// Cerrojo que tiene la ventana (proceso) que recorre el disco y vigila los cambios; las demás
/// sólo cargan el índice que esa guarda. Si se cierra, otra lo toma en su siguiente `tick`.
/// `Some(None)`: sin cerrojo posible (sistema de archivos que no lo admite); se indexa igual.
#[cfg(not(target_arch = "wasm32"))]
static INDEXER: Mutex<Option<Option<std::fs::File>>> = Mutex::new(None);
/// Última comprobación (segundos) del índice guardado por otra ventana.
static LAST_CHECK: AtomicU64 = AtomicU64::new(0);

/// ¿Tiene este proceso el cerrojo de indexador?
fn is_indexer() -> bool {
    #[cfg(target_arch = "wasm32")]
    {
        false
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        INDEXER.lock().map(|g| g.is_some()).unwrap_or(false)
    }
}

/// ¿Es este proceso el indexador? Intenta serlo si nadie lo es.
fn become_indexer() -> bool {
    #[cfg(target_arch = "wasm32")]
    {
        false
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let mut g = INDEXER.lock().unwrap_or_else(|e| e.into_inner());
        if g.is_some() {
            return true;
        }
        let f = cache_file().with_extension("lock");
        if let Some(d) = f.parent() {
            crate::fsys::private_dir(d);
        }
        // sin cerrojo posible, cada ventana se las arregla sola, como antes
        let Ok(h) = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(&f) else {
            *g = Some(None);
            return true;
        };
        match h.try_lock() {
            Ok(()) => {
                *g = Some(Some(h));
                true
            }
            Err(std::fs::TryLockError::WouldBlock) => false,
            Err(std::fs::TryLockError::Error(_)) => {
                *g = Some(None);
                true
            }
        }
    }
}

fn release_indexer() {
    #[cfg(not(target_arch = "wasm32"))]
    {
        *INDEXER.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }
}

/// Carga en segundo plano el índice guardado por otra ventana si es más nuevo que el que hay.
fn reload_shared(roots: Vec<PathBuf>, ctx: egui::Context) {
    if LOADING.swap(true, AO::SeqCst) {
        return;
    }
    let gener = GENERATION.load(AO::SeqCst);
    crate::vfs::thread::spawn(move || {
        if let Some(idx) = Index::load(&cache_file()) {
            let newer = current().map(|c| idx.built > c.built).unwrap_or(true);
            if newer && idx.roots() == roots && GENERATION.load(AO::SeqCst) == gener {
                PROGRESS.store(idx.count(), AO::Relaxed);
                if let Ok(mut g) = CURRENT.write() {
                    *g = Some(Arc::new(idx));
                }
            }
        }
        LOADING.store(false, AO::SeqCst);
        ctx.request_repaint();
    });
}

// ---------------------------------------------------------------------------
// Qué no se recorre
// ---------------------------------------------------------------------------

/// Tipos de sistema de archivos que no se indexan: de red (lentos, y cada ventana los recorrería
/// por la red) y virtuales del sistema.
#[cfg(any(target_os = "linux", target_os = "android", test))]
fn skip_fstype(t: &str) -> bool {
    const NET: &[&str] = &["nfs", "nfs4", "cifs", "smb3", "smbfs", "ncpfs", "afs", "ceph", "glusterfs", "9p", "davfs", "lustre", "gpfs"];
    const PSEUDO: &[&str] = &[
        "proc", "sysfs", "devtmpfs", "devpts", "cgroup", "cgroup2", "securityfs", "debugfs", "tracefs", "pstore", "bpf", "mqueue",
        "hugetlbfs", "configfs", "fusectl", "autofs", "binfmt_misc", "efivarfs", "rpc_pipefs", "nsfs", "ramfs", "tmpfs", "squashfs", "overlay",
    ];
    // FUSE en espacio de usuario (sshfs, rclone, gvfs, portal, appimage…); `fuseblk` (NTFS, exFAT) es local
    NET.contains(&t) || PSEUDO.contains(&t) || t.starts_with("fuse.") || t == "fuse"
}

/// Punto de montaje de /proc/self/mountinfo: los espacios y similares vienen como `\ooo` (octal).
#[cfg(any(target_os = "linux", target_os = "android", test))]
fn unescape_mount(s: &str) -> PathBuf {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\' && i + 3 < b.len() && b[i + 1..i + 4].iter().all(|c| (b'0'..=b'7').contains(c)) {
            let v = u32::from(b[i + 1] - b'0') * 64 + u32::from(b[i + 2] - b'0') * 8 + u32::from(b[i + 3] - b'0');
            out.push(v as u8);
            i += 4;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        PathBuf::from(OsString::from_vec(out))
    }
    #[cfg(not(unix))]
    {
        PathBuf::from(String::from_utf8_lossy(&out).into_owned())
    }
}

/// Puntos de montaje que no se deben recorrer, a partir del texto de /proc/self/mountinfo.
#[cfg(any(target_os = "linux", target_os = "android", test))]
fn skipped_mounts_from(info: &str) -> std::collections::HashSet<PathBuf> {
    let mut out = std::collections::HashSet::new();
    for l in info.lines() {
        // id padre mayor:menor raíz punto opciones [campos opcionales] - tipo origen opciones
        let Some((a, b)) = l.split_once(" - ") else { continue };
        let (Some(mp), Some(ty)) = (a.split(' ').nth(4), b.split(' ').next()) else { continue };
        let mp = unescape_mount(mp);
        if mp != Path::new("/") && skip_fstype(ty) {
            out.insert(mp);
        }
    }
    out
}

pub(crate) fn skipped_mounts() -> std::collections::HashSet<PathBuf> {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        crate::vfs::fs::read_to_string("/proc/self/mountinfo").map(|t| skipped_mounts_from(&t)).unwrap_or_default()
    }
    #[cfg(target_os = "macos")]
    {
        let mut out = std::collections::HashSet::new();
        // SAFETY: getmntinfo devuelve un vector propio de la biblioteca con `n` elementos válidos
        unsafe {
            let mut p: *mut libc::statfs = std::ptr::null_mut();
            let n = libc::getmntinfo(&mut p, libc::MNT_NOWAIT);
            if n > 0 && !p.is_null() {
                for m in std::slice::from_raw_parts(p, n as usize) {
                    let ty = std::ffi::CStr::from_ptr(m.f_fstypename.as_ptr()).to_string_lossy();
                    let local = m.f_flags & (libc::MNT_LOCAL as u32) != 0;
                    if !local || matches!(&*ty, "devfs" | "autofs" | "nullfs") {
                        use std::os::unix::ffi::OsStrExt;
                        let mp = std::ffi::CStr::from_ptr(m.f_mntonname.as_ptr());
                        let mp = PathBuf::from(std::ffi::OsStr::from_bytes(mp.to_bytes()));
                        if mp != Path::new("/") {
                            out.insert(mp);
                        }
                    }
                }
            }
        }
        out
    }
    #[cfg(not(any(target_os = "linux", target_os = "android", target_os = "macos")))]
    {
        Default::default()
    }
}

/// Raíces que de verdad se indexan: fuera las de red o virtuales.
fn effective_roots(roots: Vec<PathBuf>) -> Vec<PathBuf> {
    #[cfg(windows)]
    {
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn GetDriveTypeW(root: *const u16) -> u32;
        }
        use std::os::windows::ffi::OsStrExt;
        roots
            .into_iter()
            .filter(|r| {
                let w: Vec<u16> = r.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
                // 4 = DRIVE_REMOTE (unidad de red)
                unsafe { GetDriveTypeW(w.as_ptr()) != 4 }
            })
            .collect()
    }
    #[cfg(not(windows))]
    {
        let skip = skipped_mounts();
        roots.into_iter().filter(|r| !skip.contains(r)).collect()
    }
}

/// Prioridad baja para el hilo actual (CPU y disco): el índice no debe notarse.
fn low_priority() {
    #[cfg(target_os = "linux")]
    unsafe {
        // en Linux la prioridad es por hilo (who = 0: el hilo que llama)
        libc::setpriority(libc::PRIO_PROCESS, 0, 19);
        libc::syscall(libc::SYS_ioprio_set, 1, 0, 3 << 13); // IOPRIO_WHO_PROCESS, clase «idle»
    }
    #[cfg(windows)]
    {
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn GetCurrentThread() -> isize;
            fn SetThreadPriority(h: isize, prio: i32) -> i32;
        }
        // THREAD_MODE_BACKGROUND_BEGIN: baja también la prioridad de E/S
        unsafe {
            SetThreadPriority(GetCurrentThread(), 0x0001_0000);
        }
    }
}

pub fn status() -> Status {
    if BUILDING.load(AO::Relaxed) || LOADING.load(AO::Relaxed) {
        return Status::Building(PROGRESS.load(AO::Relaxed));
    }
    match current() {
        Some(i) => Status::Ready(i.count(), now().saturating_sub(i.built)),
        None => Status::Off,
    }
}

pub fn ready() -> bool {
    current().is_some()
}

/// Libera el índice de memoria y borra el fichero guardado.
/// Deja de vigilar la unidad `root` (Windows: antes de expulsarla). En otros sistemas no hace nada.
#[cfg(windows)]
pub fn release_drive(root: &Path) {
    watch::release(root);
}

pub fn disable() {
    GENERATION.fetch_add(1, AO::SeqCst);
    watch::stop();
    release_indexer();
    if let Ok(mut g) = CURRENT.write() {
        *g = None;
    }
    let _ = crate::vfs::fs::remove_file(cache_file());
}

/// Activa el índice: carga el guardado (si coincide con las raíces) y recorre de nuevo si hace falta.
pub fn start(roots: Vec<PathBuf>, ctx: egui::Context) {
    // recorrer todo el servidor por la red no tiene sentido: en la web no hay índice
    if cfg!(target_arch = "wasm32") {
        return;
    }
    if LOADING.swap(true, AO::SeqCst) {
        return;
    }
    let gener = GENERATION.load(AO::SeqCst);
    crate::vfs::thread::spawn(move || {
        // no compite con el arranque: el primer fotograma y el listado inicial van antes
        crate::vfs::thread::sleep(Duration::from_millis(2500));
        let eff = effective_roots(roots.clone());
        let indexer = become_indexer();
        if let Some(idx) = Index::load(&cache_file()) {
            if idx.roots() == eff && GENERATION.load(AO::SeqCst) == gener {
                PROGRESS.store(idx.count(), AO::Relaxed);
                let idx = Arc::new(idx);
                if let Ok(mut g) = CURRENT.write() {
                    *g = Some(idx.clone());
                }
                // sólo vigila cambios la ventana que indexa
                if indexer {
                    watch::restart(idx);
                }
            }
        }
        LOADING.store(false, AO::SeqCst);
        ctx.request_repaint();
        // la que no indexa sólo construye si aún no hay nada guardado que cargar
        let stale = current().map(|i| now().saturating_sub(i.built) > STALE_SECS).unwrap_or(true);
        if GENERATION.load(AO::SeqCst) == gener && stale && (indexer || current().is_none()) {
            rebuild(roots, ctx);
        }
    });
}

/// Llamar de vez en cuando: refresca en segundo plano un índice que se ha quedado viejo.
pub fn tick(roots: Vec<PathBuf>, ctx: &egui::Context) {
    if BUILDING.load(AO::Relaxed) || LOADING.load(AO::Relaxed) {
        return;
    }
    if !cfg!(target_arch = "wasm32") && !is_indexer() {
        // las ventanas que no indexan recogen de vez en cuando lo que guarda la que sí
        let t = now();
        if t.saturating_sub(LAST_CHECK.load(AO::Relaxed)) < RELOAD_CHECK_SECS {
            return;
        }
        LAST_CHECK.store(t, AO::Relaxed);
        if become_indexer() {
            // la ventana que indexaba se cerró: ésta toma el relevo
            match current() {
                Some(i) => watch::restart(i),
                None => {
                    rebuild(roots, ctx.clone());
                    return;
                }
            }
        } else {
            let disk = Index::built_on_disk(&cache_file());
            if disk.is_some_and(|b| current().is_none_or(|c| b > c.built)) {
                reload_shared(effective_roots(roots), ctx.clone());
            }
            return;
        }
    }
    if let Some(i) = current() {
        let age = now().saturating_sub(i.built);
        let d = DIRTY.load(AO::Relaxed);
        let changed = d != 0 && now_ms().saturating_sub(d) >= SETTLE_MS && age >= MIN_GAP_SECS;
        if age > STALE_SECS || changed {
            rebuild(roots, ctx.clone());
        } else if d != 0 {
            ctx.request_repaint_after(Duration::from_millis(SETTLE_MS));
        }
    }
}

// ---------------------------------------------------------------------------
// Recorrido en paralelo
// ---------------------------------------------------------------------------

struct Work {
    stack: Vec<(u32, PathBuf)>,
    /// Hilos que están procesando una carpeta ahora mismo.
    active: usize,
}

pub fn rebuild(roots: Vec<PathBuf>, ctx: egui::Context) {
    if cfg!(target_arch = "wasm32") {
        return;
    }
    // otra ventana ya indexa: se recoge lo que ella guarda en vez de recorrer el disco otra vez
    if !become_indexer() {
        reload_shared(effective_roots(roots), ctx);
        return;
    }
    if BUILDING.swap(true, AO::SeqCst) {
        return;
    }
    let gener = GENERATION.load(AO::SeqCst);
    PROGRESS.store(0, AO::Relaxed);
    let started = now_ms();
    crate::vfs::thread::spawn(move || {
        let roots = effective_roots(roots);
        let skip = Arc::new(skipped_mounts());
        let mut idx = Index::default();
        let mut stack = Vec::new();
        for r in &roots {
            let id = idx.push(&r.to_string_lossy(), NO_PARENT, F_DIR);
            stack.push((id, r.clone()));
        }
        let shared = Arc::new((Mutex::new(Work { stack, active: 0 }), Condvar::new()));
        let idx = Arc::new(Mutex::new(idx));
        let threads = crate::vfs::thread::available_parallelism().map(|n| n.get()).unwrap_or(2).clamp(2, 4);
        let hs: Vec<_> = (0..threads)
            .map(|_| {
                let (shared, idx, ctx, skip) = (shared.clone(), idx.clone(), ctx.clone(), skip.clone());
                crate::vfs::thread::spawn(move || {
                    low_priority();
                    worker(&shared, &idx, gener, &ctx, &skip)
                })
            })
            .collect();
        for h in hs {
            let _ = h.join();
        }
        if GENERATION.load(AO::SeqCst) == gener {
            let mut idx = Arc::try_unwrap(idx).ok().map(|m| m.into_inner().unwrap_or_default()).unwrap_or_default();
            idx.built = now();
            idx.arena.shrink_to_fit();
            idx.off.shrink_to_fit();
            idx.len.shrink_to_fit();
            idx.parent.shrink_to_fit();
            idx.flags.shrink_to_fit();
            idx.save(&cache_file());
            let idx = Arc::new(idx);
            if let Ok(mut g) = CURRENT.write() {
                *g = Some(idx.clone());
            }
            // los cambios anteriores al recorrido ya están reflejados
            if DIRTY.load(AO::Relaxed) < started {
                DIRTY.store(0, AO::Relaxed);
            }
            forget_changes(started);
            watch::restart(idx);
        }
        BUILDING.store(false, AO::SeqCst);
        ctx.request_repaint();
    });
}

fn worker(shared: &(Mutex<Work>, Condvar), idx: &Mutex<Index>, gener: u64, ctx: &egui::Context, skip: &std::collections::HashSet<PathBuf>) {
    let (lock, cv) = shared;
    let mut last = web_time::Instant::now();
    loop {
        let (id, dir) = {
            let mut w = lock.lock().unwrap();
            loop {
                if GENERATION.load(AO::Relaxed) != gener {
                    w.stack.clear();
                }
                if let Some(t) = w.stack.pop() {
                    w.active += 1;
                    break t;
                }
                if w.active == 0 {
                    cv.notify_all();
                    return;
                }
                w = cv.wait_timeout(w, Duration::from_millis(50)).unwrap().0;
            }
        };
        let mut kids: Vec<(OsString, u8)> = Vec::new();
        let read = match crate::vfs::fs::read_dir(&dir) {
            Ok(rd) => {
                for e in rd.flatten() {
                    let is_dir = e.file_type().map(|t| t.v_is_dir()).unwrap_or(false);
                    // el atributo «oculto» de Windows viene con la entrada: no cuesta otra llamada
                    let hid = crate::fsys::is_hidden_entry(&e);
                    kids.push((e.file_name(), if is_dir { F_DIR } else { 0 } | if hid { F_HIDDEN } else { 0 }));
                }
                true
            }
            Err(_) => false,
        };
        let mut subdirs = Vec::new();
        let full;
        {
            let mut ix = idx.lock().unwrap();
            let mut cut = !read;
            for (name, fl) in &kids {
                if ix.full() {
                    cut = true;
                    break;
                }
                let n = name.to_string_lossy();
                let mut fl = *fl;
                let p = (fl & F_DIR != 0).then(|| dir.join(name));
                // ni sistemas virtuales ni montajes de red o FUSE: quedan marcadas como no recorridas
                // /run es tmpfs, pero dentro están los discos extraíbles (/run/media): sí se entra
                let walk = p.as_ref().is_some_and(|p| !crate::tools::pseudo_fs(p) && (!skip.contains(p) || p == Path::new("/run")));
                if p.is_some() && !walk {
                    fl |= F_UNWALKED;
                }
                let nid = ix.push(&n, id, fl);
                if let (true, Some(p)) = (walk, p) {
                    subdirs.push((nid, p));
                }
            }
            if cut {
                ix.flags[id as usize] |= F_UNWALKED;
            }
            full = ix.full();
            if full {
                for (nid, _) in &subdirs {
                    ix.flags[*nid as usize] |= F_UNWALKED;
                }
            }
            PROGRESS.store(ix.count(), AO::Relaxed);
        }
        let left = {
            let mut w = lock.lock().unwrap();
            let left = if full {
                // tope alcanzado: se termina con lo que hay
                std::mem::take(&mut w.stack)
            } else {
                w.stack.extend(subdirs);
                Vec::new()
            };
            w.active -= 1;
            left
        };
        if !left.is_empty() {
            let mut ix = idx.lock().unwrap();
            for (nid, _) in left {
                ix.flags[nid as usize] |= F_UNWALKED;
            }
        }
        cv.notify_all();
        if last.elapsed().as_millis() > 300 {
            last = web_time::Instant::now();
            ctx.request_repaint();
        }
    }
}

// ---------------------------------------------------------------------------
// Vigilancia de cambios (inotify en Linux)
// ---------------------------------------------------------------------------

#[cfg(target_os = "linux")]
mod watch {
    use super::*;

    static WATCH_GEN: AtomicU64 = AtomicU64::new(0);

    pub fn stop() {
        WATCH_GEN.fetch_add(1, AO::SeqCst);
    }

    /// Carpetas que no merece vigilar: producen ráfagas constantes de cambios sin interés para buscar.
    fn noisy(name: &str) -> bool {
        matches!(name, ".git" | "node_modules" | "target" | ".cache" | ".cargo" | ".rustup")
    }

    /// Zonas del sistema y de configuración que cambian sin parar (incluida la propia configuración
    /// y caché de esta aplicación): sin vigilarlas, la actividad normal no provoca recorridos.
    /// Lo nuevo en ellas se recoge en el recorrido periódico.
    fn quiet_roots() -> Vec<PathBuf> {
        let mut v: Vec<PathBuf> = ["/tmp", "/var", "/proc", "/sys", "/dev", "/snap"].iter().map(PathBuf::from).collect();
        if let Some(h) = dirs::home_dir() {
            for d in [".config", ".cache", ".local/share", ".local/state", ".var", ".mozilla", ".thunderbird"] {
                v.push(h.join(d));
            }
        }
        v.extend(dirs::config_dir());
        v.extend(dirs::cache_dir());
        v.extend(dirs::data_dir());
        v
    }

    fn quiet(p: &Path, q: &[PathBuf]) -> bool {
        // /run es del sistema, salvo los discos extraíbles de /run/media
        (p.starts_with("/run") && !p.starts_with("/run/media") && p != Path::new("/run")) || q.iter().any(|r| p.starts_with(r))
    }

    pub fn restart(idx: Arc<Index>) {
        let gen_id = WATCH_GEN.fetch_add(1, AO::SeqCst) + 1;
        crate::vfs::thread::spawn(move || run(idx, gen_id));
    }

    fn run(idx: Arc<Index>, gen_id: u64) {
        let limit = crate::vfs::fs::read_to_string("/proc/sys/fs/inotify/max_user_watches")
            .ok()
            .and_then(|s| s.trim().parse::<usize>().ok())
            .unwrap_or(8192);
        // como mucho una cuarta parte del cupo del usuario (y sólo vigila la ventana que indexa)
        let budget = (limit / 4).min(50_000);
        let fd = unsafe { libc::inotify_init1(libc::IN_NONBLOCK | libc::IN_CLOEXEC) };
        if fd < 0 {
            return;
        }
        // las carpetas más cercanas a la raíz primero: son las que más cambian de interés
        let n = idx.count();
        let mut depth = vec![u8::MAX; n];
        let mut dirs: Vec<(u8, u32)> = Vec::new();
        for i in 0..n {
            // las no recorridas (montajes de red, virtuales) no se vigilan
            if !idx.is_dir(i) || idx.flags[i] & F_UNWALKED != 0 {
                continue;
            }
            let (mut d, mut cur, mut skip) = (0u32, i, false);
            while idx.parent[cur] != NO_PARENT {
                if noisy(idx.name(cur)) {
                    skip = true;
                    break;
                }
                d += 1;
                cur = idx.parent[cur] as usize;
            }
            if !skip {
                depth[i] = d.min(254) as u8;
                dirs.push((depth[i], i as u32));
            }
        }
        dirs.sort_unstable();
        let mask = libc::IN_CREATE | libc::IN_DELETE | libc::IN_MOVED_FROM | libc::IN_MOVED_TO | libc::IN_DELETE_SELF | libc::IN_MOVE_SELF;
        let q = quiet_roots();
        let mut wds: std::collections::HashMap<i32, u32> = std::collections::HashMap::new();
        let mut added = 0usize;
        for (_, id) in dirs {
            if WATCH_GEN.load(AO::SeqCst) != gen_id {
                unsafe { libc::close(fd) };
                return;
            }
            if added >= budget {
                break;
            }
            let p = idx.path_of(id as usize);
            if crate::tools::pseudo_fs(&p) || quiet(&p, &q) {
                continue;
            }
            use std::os::unix::ffi::OsStrExt;
            let Ok(c) = std::ffi::CString::new(p.as_os_str().as_bytes()) else { continue };
            let wd = unsafe { libc::inotify_add_watch(fd, c.as_ptr(), mask) };
            if wd >= 0 {
                wds.insert(wd, id);
                added += 1;
            } else if std::io::Error::last_os_error().raw_os_error() == Some(libc::ENOSPC) {
                break;
            }
        }
        let mut buf = vec![0u8; 16 * 1024];
        while WATCH_GEN.load(AO::SeqCst) == gen_id {
            let mut pfd = libc::pollfd { fd, events: libc::POLLIN, revents: 0 };
            let r = unsafe { libc::poll(&mut pfd, 1, 1000) };
            if r > 0 {
                let got = unsafe { libc::read(fd, buf.as_mut_ptr() as *mut _, buf.len()) };
                if got > 0 {
                    on_events(&buf[..got as usize], &idx, &wds);
                }
            }
        }
        unsafe { libc::close(fd) };
    }

    /// Recorre los `inotify_event` leídos: apunta la carpeta de cada cambio que interesa.
    fn on_events(b: &[u8], idx: &Index, wds: &std::collections::HashMap<i32, u32>) {
        let mut off = 0usize;
        // cabecera: wd (i32), mask, cookie, len (u32); luego `len` bytes de nombre terminado en NUL
        while off + 16 <= b.len() {
            let wd = i32::from_ne_bytes(b[off..off + 4].try_into().unwrap());
            let mask = u32::from_ne_bytes(b[off + 4..off + 8].try_into().unwrap());
            let len = u32::from_ne_bytes(b[off + 12..off + 16].try_into().unwrap()) as usize;
            let end = (off + 16 + len).min(b.len());
            let raw = &b[off + 16..end];
            let name = String::from_utf8_lossy(&raw[..raw.iter().position(|&c| c == 0).unwrap_or(raw.len())]);
            off = off + 16 + len;
            if mask & libc::IN_Q_OVERFLOW != 0 {
                mark_overflow();
                continue;
            }
            if mask & libc::IN_IGNORED != 0 || temp_name(&name) || noisy(&name) {
                continue;
            }
            let Some(&id) = wds.get(&wd) else { continue };
            let dir = idx.path_of(id as usize);
            if mask & (libc::IN_DELETE_SELF | libc::IN_MOVE_SELF) != 0 {
                // la propia carpeta desapareció: el cambio está en su padre
                if let Some(p) = dir.parent() {
                    mark_changed(p.to_path_buf());
                }
            } else {
                mark_changed(dir);
            }
        }
    }
}

/// Rutas del sistema o de herramientas que cambian sin parar y no interesan para buscar.
#[cfg(any(windows, target_os = "macos"))]
fn noisy_path(p: &str) -> bool {
    let p = p.replace('\\', "/");
    const BAD: &[&str] = &[
        "/.git/", "/node_modules/", "/target/", "/AppData/", "/$Recycle.Bin/", "/Windows/", "/ProgramData/",
        "/System Volume Information/", "/Library/", "/private/", "/System/", "/.Trash/", "/.Spotlight-V100/", "/.fseventsd/",
        "/.DocumentRevisions-V100/", "/.cache/",
    ];
    let q = format!("{p}/");
    BAD.iter().any(|b| q.contains(b))
}

/// Windows: `ReadDirectoryChangesW` sobre cada unidad, con subárbol, en un hilo por unidad.
#[cfg(windows)]
mod watch {
    use super::*;
    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::Storage::FileSystem::{
        CreateFileW, ReadDirectoryChangesW, FILE_FLAG_BACKUP_SEMANTICS, FILE_LIST_DIRECTORY, FILE_NOTIFY_CHANGE_DIR_NAME,
        FILE_NOTIFY_CHANGE_FILE_NAME, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
    };
    use windows::Win32::System::IO::CancelIoEx;

    static HANDLES: Mutex<Vec<(PathBuf, isize)>> = Mutex::new(Vec::new());

    pub fn stop() {
        let hs: Vec<(PathBuf, isize)> = std::mem::take(&mut *HANDLES.lock().unwrap());
        for (_, h) in hs {
            // despierta al hilo bloqueado en ReadDirectoryChangesW: sale y cierra el manejador
            unsafe {
                let _ = CancelIoEx(HANDLE(h as *mut _), None);
            }
        }
    }

    /// Suelta la vigilancia de las unidades bajo `root` (antes de expulsarla: un manejador
    /// abierto haría que Windows la diera por «en uso»).
    pub fn release(root: &Path) {
        let mut hs = HANDLES.lock().unwrap();
        hs.retain(|(r, h)| {
            if !r.starts_with(root) && !root.starts_with(r) {
                return true;
            }
            unsafe {
                let _ = CancelIoEx(HANDLE(*h as *mut _), None);
            }
            false
        });
    }

    /// Las unidades no cambian entre recorridos: si ya se vigilan, no se hace nada.
    pub fn restart(idx: Arc<Index>) {
        if !HANDLES.lock().unwrap().is_empty() {
            return;
        }
        for root in idx.roots() {
            let wide: Vec<u16> = root.to_string_lossy().encode_utf16().chain(std::iter::once(0)).collect();
            let h = unsafe {
                CreateFileW(
                    windows::core::PCWSTR(wide.as_ptr()),
                    FILE_LIST_DIRECTORY.0,
                    FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                    None,
                    OPEN_EXISTING,
                    FILE_FLAG_BACKUP_SEMANTICS,
                    None,
                )
            };
            let Ok(h) = h else { continue };
            let raw = h.0 as isize;
            HANDLES.lock().unwrap().push((root.clone(), raw));
            let base = root.to_string_lossy().into_owned();
            crate::vfs::thread::spawn(move || {
                let h = HANDLE(raw as *mut _);
                let mut buf = vec![0u32; 16 * 1024]; // alineado a 4 bytes
                loop {
                    let mut got = 0u32;
                    let r = unsafe {
                        ReadDirectoryChangesW(
                            h,
                            buf.as_mut_ptr() as *mut _,
                            (buf.len() * 4) as u32,
                            true,
                            FILE_NOTIFY_CHANGE_FILE_NAME | FILE_NOTIFY_CHANGE_DIR_NAME,
                            Some(&mut got),
                            None,
                            None,
                        )
                    };
                    if r.is_err() {
                        break;
                    }
                    if got == 0 {
                        // desbordó el búfer: hubo cambios, no sabemos cuáles
                        mark_overflow();
                        continue;
                    }
                    let bytes = unsafe { std::slice::from_raw_parts(buf.as_ptr() as *const u8, got as usize) };
                    let mut off = 0usize;
                    while off + 12 <= bytes.len() {
                        let next = u32::from_le_bytes(bytes[off..off + 4].try_into().unwrap()) as usize;
                        let len = u32::from_le_bytes(bytes[off + 8..off + 12].try_into().unwrap()) as usize;
                        if off + 12 + len <= bytes.len() {
                            let w: Vec<u16> = bytes[off + 12..off + 12 + len].chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
                            let rel = String::from_utf16_lossy(&w);
                            let full = PathBuf::from(format!("{base}{rel}"));
                            let tmp = full.file_name().is_some_and(|n| temp_name(&n.to_string_lossy()));
                            if !tmp && !noisy_path(&full.to_string_lossy()) {
                                mark_changed(full.parent().map(Path::to_path_buf).unwrap_or(full));
                            }
                        }
                        if next == 0 {
                            break;
                        }
                        off += next;
                    }
                }
                unsafe {
                    let _ = CloseHandle(h);
                }
            });
        }
    }
}

/// macOS: FSEvents sobre la raíz (eventos a nivel de fichero), en un hilo que vive mientras no cambie la generación.
#[cfg(target_os = "macos")]
mod watch {
    use super::*;

    static WATCH_GEN: AtomicU64 = AtomicU64::new(0);

    pub fn stop() {
        WATCH_GEN.fetch_add(1, AO::SeqCst);
    }

    pub fn restart(idx: Arc<Index>) {
        let gen_id = WATCH_GEN.fetch_add(1, AO::SeqCst) + 1;
        let roots: Vec<String> = idx.roots().iter().map(|p| p.to_string_lossy().into_owned()).collect();
        crate::vfs::thread::spawn(move || {
            let (tx, rx) = std::sync::mpsc::channel();
            let mut fs = fsevent::FsEvent::new(roots);
            if fs.observe_async(tx).is_err() {
                return;
            }
            while WATCH_GEN.load(AO::SeqCst) == gen_id {
                match rx.recv_timeout(Duration::from_secs(1)) {
                    Ok(ev) => {
                        let full = PathBuf::from(&ev.path);
                        let tmp = full.file_name().is_some_and(|n| temp_name(&n.to_string_lossy()));
                        if !tmp && !noisy_path(&ev.path) {
                            mark_changed(full.parent().map(Path::to_path_buf).unwrap_or(full));
                        }
                    }
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                    Err(_) => break,
                }
            }
            fs.shutdown_observe();
        });
    }
}

#[cfg(not(any(target_os = "linux", windows, target_os = "macos")))]
mod watch {
    use super::*;
    pub fn stop() {}
    pub fn restart(_idx: Arc<Index>) {}
}

// ---------------------------------------------------------------------------
// Búsqueda
// ---------------------------------------------------------------------------

/// Busca en el índice. Devuelve `None` sólo si no hay índice (el llamador recorre el disco).
/// Si la carpeta pedida no está indexada (montaje de red, tope…) o ha cambiado desde el último
/// recorrido, el propio hilo de la búsqueda recorre el disco: nada de eso se hace en la interfaz.
pub fn spawn_search(scope: Option<PathBuf>, needle: String, hidden: bool, ctx: egui::Context) -> Option<Arc<Search>> {
    let idx = current()?;
    let st = Arc::new(Search::default());
    st.scanned.store(idx.count() as u64, AO::Relaxed);
    let s = st.clone();
    crate::vfs::thread::spawn(move || {
        let needle = crate::tools::fold(&needle);
        let (changed, overflow) = pending_changes();
        // carpetas que cambiaron desde el último recorrido: se leen del disco además del índice
        let mut extra: Vec<PathBuf> = match &scope {
            Some(p) => changed.into_iter().filter(|c| c.starts_with(p)).collect(),
            None => changed,
        };
        let scope_id = match &scope {
            Some(p) => {
                // otras ventanas no reciben avisos: basta con que la carpeta sea más nueva que el índice
                if newer_than(p, idx.built) && !extra.contains(p) {
                    extra.push(p.clone());
                }
                // demasiados cambios sin apuntar: mejor recorrer la carpeta entera
                match idx.find_dir(p).filter(|_| !overflow) {
                    Some(id) => Some(id),
                    None => {
                        s.scanned.store(0, AO::Relaxed);
                        if crate::tools::live_search(&s, vec![p.clone()], &needle, hidden, &ctx) {
                            s.done.store(true, AO::Release);
                            ctx.request_repaint();
                        }
                        return;
                    }
                }
            }
            None => None,
        };
        let n = idx.count();
        let threads = crate::vfs::thread::available_parallelism().map(|n| n.get()).unwrap_or(2).clamp(1, 8);
        let chunk = n.div_ceil(threads).max(1);
        let hits: Vec<Vec<u32>> = crate::vfs::thread::scope(|sc| {
            let hs: Vec<_> = (0..threads)
                .map(|t| {
                    let (idx, needle) = (&idx, &needle);
                    sc.spawn(move || {
                        let (a, b) = ((t * chunk).min(n), ((t + 1) * chunk).min(n));
                        (a..b)
                            .filter(|&i| idx.parent[i] != NO_PARENT && crate::tools::name_matches(idx.name(i), needle))
                            .map(|i| i as u32)
                            .collect::<Vec<u32>>()
                    })
                })
                .collect();
            hs.into_iter().map(|h| h.join().unwrap_or_default()).collect()
        });
        let mut found = 0usize;
        let mut seen = std::collections::HashSet::new();
        'o: for i in hits.into_iter().flatten() {
            if s.cancel.load(AO::Relaxed) {
                break;
            }
            // recorre los ancestros: dentro de la carpeta pedida y sin componentes ocultos
            let mut cur = i as usize;
            let mut inside = scope_id.is_none();
            let mut hid = false;
            loop {
                if scope_id == Some(cur as u32) {
                    inside = true;
                    break;
                }
                let p = idx.parent[cur];
                if p == NO_PARENT {
                    break;
                }
                if idx.flags[cur] & F_HIDDEN != 0 {
                    hid = true;
                }
                cur = p as usize;
            }
            // lo oculto se descarta antes de contar: no gasta el tope de resultados
            if scope_id == Some(i) || !inside || (hid && !hidden) {
                continue;
            }
            let path = idx.path_of(i as usize);
            if !extra.is_empty() {
                seen.insert(path.clone());
            }
            s.results.lock().unwrap().push((path, idx.is_dir(i as usize)));
            found += 1;
            if found >= MAX_RESULTS {
                break 'o;
            }
        }
        // lo creado desde el último recorrido; las subcarpetas nuevas o cambiadas también se leen
        let mut budget = 2000usize;
        while let Some(dir) = extra.pop() {
            if found >= MAX_RESULTS || budget == 0 || s.cancel.load(AO::Relaxed) {
                break;
            }
            budget -= 1;
            let below = scope.as_deref().and_then(|sc| dir.strip_prefix(sc).ok()).unwrap_or(&dir);
            if !hidden && below.components().any(|c| c.as_os_str().to_string_lossy().starts_with('.')) {
                continue;
            }
            let Ok(rd) = crate::vfs::fs::read_dir(&dir) else { continue };
            for e in rd.flatten() {
                if found >= MAX_RESULTS {
                    break;
                }
                if !hidden && crate::fsys::is_hidden_entry(&e) {
                    continue;
                }
                let p = e.path();
                let d = e.file_type().map(|t| t.v_is_dir()).unwrap_or(false);
                if d && !crate::tools::pseudo_fs(&p) && newer_than(&p, idx.built) {
                    extra.push(p.clone());
                }
                if crate::tools::name_matches(&e.file_name().to_string_lossy(), &needle) && seen.insert(p.clone()) {
                    s.results.lock().unwrap().push((p, d));
                    found += 1;
                }
            }
        }
        s.done.store(true, AO::Release);
        ctx.request_repaint();
    });
    Some(st)
}

/// ¿Se modificó la carpeta `p` en el segundo `built` o después?
fn newer_than(p: &Path, built: u64) -> bool {
    crate::vfs::fs::metadata(p)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .is_some_and(|d| d.as_secs() >= built)
}

/// Carpetas con cambios sin recoger y si hubo un desbordamiento (cambios sin carpeta conocida).
fn pending_changes() -> (Vec<PathBuf>, bool) {
    let v = CHANGED.lock().unwrap_or_else(|e| e.into_inner()).iter().map(|e| e.1.clone()).collect();
    (v, OVERFLOW.load(AO::Relaxed) != 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_search() {
        let d = crate::vfs::temp_dir().join(format!("fbidx{}", crate::vfs::pid()));
        crate::vfs::fs::create_dir_all(d.join("a/b")).unwrap();
        crate::vfs::fs::create_dir_all(d.join(".oculta")).unwrap();
        crate::vfs::fs::write(d.join("a/b/Informe.txt"), "x").unwrap();
        crate::vfs::fs::write(d.join(".oculta/informe2.txt"), "x").unwrap();
        let mut ix = Index::default();
        let r = ix.push(&d.to_string_lossy(), NO_PARENT, F_DIR);
        let a = ix.push("a", r, F_DIR);
        let b = ix.push("b", a, F_DIR);
        ix.push("Informe.txt", b, 0);
        let f = d.join("idx.bin");
        ix.save(&f);
        let l = Index::load(&f).unwrap();
        assert_eq!(l.count(), 4);
        assert_eq!(l.path_of(3), d.join("a/b/Informe.txt"));
        assert_eq!(l.find_dir(&d.join("a/b")), Some(2));
        assert_eq!(l.find_dir(&d.join("zz")), None);
        crate::vfs::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn unwalked_dirs_are_not_found() {
        let mut ix = Index::default();
        let r = ix.push("/", NO_PARENT, F_DIR);
        let m = ix.push("mnt", r, F_DIR);
        ix.push("nas", m, F_DIR | F_UNWALKED);
        assert_eq!(ix.find_dir(Path::new("/mnt")), Some(m));
        assert_eq!(ix.find_dir(Path::new("/mnt/nas")), None);
        assert_eq!(ix.find_dir(Path::new("/mnt/nas/docs")), None);
        #[cfg(target_os = "linux")]
        {
            use crate::tools::pseudo_fs;
            assert!(!pseudo_fs(Path::new("/run")) && !pseudo_fs(Path::new("/run/media")));
            assert!(pseudo_fs(Path::new("/run/user")) && pseudo_fs(Path::new("/proc")));
        }
    }

    #[test]
    fn search_folds_accents_and_finds_new_files() {
        let d = crate::vfs::temp_dir().join(format!("fbidxs{}", crate::vfs::pid()));
        let _ = crate::vfs::fs::remove_dir_all(&d);
        crate::vfs::fs::create_dir_all(d.join("a")).unwrap();
        let set = |built: u64| {
            let mut ix = Index::default();
            let r = ix.push(&d.to_string_lossy(), NO_PARENT, F_DIR);
            let a = ix.push("a", r, F_DIR);
            ix.push("Canción.txt", a, 0);
            ix.push("cancio\u{301}n2.txt", a, 0);
            ix.push(".oculta", a, F_DIR | F_HIDDEN);
            ix.built = built;
            *CURRENT.write().unwrap() = Some(Arc::new(ix));
        };
        set(now() + 5);
        let run = |scope: &Path, q: &str| {
            let s = spawn_search(Some(scope.to_path_buf()), q.into(), false, egui::Context::default()).unwrap();
            while !s.done.load(AO::Acquire) {
                crate::vfs::thread::sleep(Duration::from_millis(2));
            }
            let mut v: Vec<String> = s.results.lock().unwrap().iter().map(|(p, _)| p.file_name().unwrap().to_string_lossy().into_owned()).collect();
            v.sort();
            v
        };
        assert_eq!(run(&d, "CANCION"), ["Canción.txt", "cancio\u{301}n2.txt"]);
        assert!(run(&d, "oculta").is_empty());
        // archivo nuevo en una carpeta modificada después del índice: se lee del disco
        crate::vfs::fs::write(d.join("a/canción3.txt"), "x").unwrap();
        set(1);
        assert_eq!(run(&d, "canción"), ["Canción.txt", "cancio\u{301}n2.txt", "canción3.txt"]);
        // carpeta fuera del índice: recorrido en vivo
        set(now() + 5);
        crate::vfs::fs::create_dir_all(d.join("a/nueva")).unwrap();
        crate::vfs::fs::write(d.join("a/nueva/cancion4.txt"), "x").unwrap();
        assert_eq!(run(&d.join("a/nueva"), "canción"), ["cancion4.txt"]);
        *CURRENT.write().unwrap() = None;
        crate::vfs::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    #[ignore]
    fn bench_home() {
        let ctx = egui::Context::default();
        let t = web_time::Instant::now();
        rebuild(vec![dirs::home_dir().unwrap()], ctx.clone());
        while BUILDING.load(AO::SeqCst) {
            crate::vfs::thread::sleep(Duration::from_millis(50));
        }
        let ix = current().unwrap();
        eprintln!("{} entradas en {:?}, arena {} MB", ix.count(), t.elapsed(), ix.arena.len() >> 20);
        let t = web_time::Instant::now();
        let s = spawn_search(None, "informe".into(), false, ctx).unwrap();
        while !s.done.load(AO::Acquire) {
            crate::vfs::thread::sleep(Duration::from_millis(1));
        }
        eprintln!("búsqueda: {} resultados en {:?}", s.results.lock().unwrap().len(), t.elapsed());
        let _ = crate::vfs::fs::remove_file(cache_file());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn watcher_marks_dirty() {
        // no en /tmp: allí no se vigila (cambia sin parar)
        let d = Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("fbwatch{}", crate::vfs::pid()));
        crate::vfs::fs::create_dir_all(d.join("sub")).unwrap();
        let mut ix = Index::default();
        let r = ix.push(&d.to_string_lossy(), NO_PARENT, F_DIR);
        ix.push("sub", r, F_DIR);
        DIRTY.store(0, AO::Relaxed);
        watch::restart(Arc::new(ix));
        crate::vfs::thread::sleep(Duration::from_millis(500));
        crate::vfs::fs::write(d.join("sub/nuevo.txt"), "x").unwrap();
        let t = web_time::Instant::now();
        while DIRTY.load(AO::Relaxed) == 0 && t.elapsed() < Duration::from_secs(3) {
            crate::vfs::thread::sleep(Duration::from_millis(20));
        }
        watch::stop();
        crate::vfs::fs::remove_dir_all(&d).unwrap();
        assert!(DIRTY.load(AO::Relaxed) != 0);
    }
}

#[cfg(test)]
mod bound_tests {
    use super::*;

    #[test]
    fn network_and_pseudo_mounts_are_skipped() {
        let info = "\
22 1 8:2 / / rw,relatime shared:1 - ext4 /dev/sda2 rw
23 22 0:21 / /proc rw,nosuid shared:5 - proc proc rw
40 22 0:35 / /mnt/nas rw,relatime shared:20 - nfs4 nas:/vol rw
41 22 0:36 / /mnt/my\\040share rw,relatime - cifs //srv/s rw
42 22 0:37 / /run/user/1000/gvfs rw,nosuid - fuse.gvfsd-fuse gvfsd-fuse rw
43 22 8:17 / /media/usb rw,relatime - fuseblk /dev/sdb1 rw
44 22 8:3 / /home rw,relatime - btrfs /dev/sda3 rw
45 22 0:40 / /tmp rw - tmpfs tmpfs rw
";
        let s = skipped_mounts_from(info);
        for p in ["/proc", "/mnt/nas", "/mnt/my share", "/run/user/1000/gvfs", "/tmp"] {
            assert!(s.contains(Path::new(p)), "{p}");
        }
        for p in ["/", "/media/usb", "/home"] {
            assert!(!s.contains(Path::new(p)), "{p}");
        }
        assert!(skip_fstype("fuse.sshfs") && skip_fstype("smb3") && !skip_fstype("ext4") && !skip_fstype("fuseblk"));
    }

    #[test]
    fn index_has_a_cap_and_header_is_readable() {
        let mut ix = Index::default();
        assert!(!ix.full());
        ix.arena.resize(MAX_ARENA, b'a');
        assert!(ix.full());
        let mut ix = Index::default();
        ix.push("/r", NO_PARENT, F_DIR);
        ix.built = 12345;
        let n = web_time::SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
        let f = crate::vfs::temp_dir().join(format!("fbw1-idx-{}-{n:x}.bin", crate::vfs::pid()));
        ix.save(&f);
        assert_eq!(Index::built_on_disk(&f), Some(12345));
        let _ = crate::vfs::fs::remove_file(&f);
        assert_eq!(Index::built_on_disk(&f), None);
    }
}
