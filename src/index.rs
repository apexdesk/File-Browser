//! Índice de nombres de archivo en memoria para búsquedas instantáneas.
//!
//! Se recorre el disco una vez (en paralelo, en segundo plano) y se guardan solo los nombres,
//! con el padre de cada entrada, en vectores compactos (unos 11 bytes por entrada más el nombre).
//! El índice se guarda en la carpeta de caché para cargarlo al instante en el siguiente arranque
//! y se vuelve a recorrer en segundo plano cuando pasa cierto tiempo.

use crate::tools::{Search, MAX_RESULTS};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering as AO};
use std::sync::{Arc, Condvar, Mutex, RwLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const NO_PARENT: u32 = u32::MAX;
const MAGIC: &[u8; 8] = b"FBIDX001";
/// Pasado este tiempo desde el último recorrido, se vuelve a indexar en segundo plano.
const STALE_SECS: u64 = 20 * 60;

#[derive(Default)]
struct Index {
    arena: Vec<u8>,
    off: Vec<u32>,
    len: Vec<u16>,
    parent: Vec<u32>,
    dir: Vec<bool>,
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

    fn push(&mut self, name: &str, parent: u32, dir: bool) -> u32 {
        let id = self.off.len() as u32;
        let b = name.as_bytes();
        let b = &b[..b.len().min(u16::MAX as usize)];
        self.off.push(self.arena.len() as u32);
        self.len.push(b.len() as u16);
        self.arena.extend_from_slice(b);
        self.parent.push(parent);
        self.dir.push(dir);
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
            if p == NO_PARENT {
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

    /// Busca la carpeta `path` descendiendo desde las raíces indexadas.
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
                if self.parent[i] == cur && self.dir[i] && self.name(i) == want {
                    found = Some(i as u32);
                    break;
                }
            }
            cur = found?;
        }
        Some(cur)
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
        b.extend(self.dir.iter().map(|&d| d as u8));
        if let Some(d) = file.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        let tmp = file.with_extension("tmp");
        if std::fs::write(&tmp, &b).is_ok() {
            let _ = std::fs::rename(&tmp, file);
        }
    }

    fn load(file: &Path) -> Option<Index> {
        let b = std::fs::read(file).ok()?;
        if b.len() < 28 || &b[..8] != MAGIC {
            return None;
        }
        let u64_at = |o: usize| u64::from_le_bytes(b[o..o + 8].try_into().unwrap());
        let u32_at = |o: usize| u32::from_le_bytes(b[o..o + 4].try_into().unwrap());
        let built = u64_at(8);
        let n = u32_at(16) as usize;
        let alen = u32_at(20) as usize;
        if b.len() != 24 + alen + n * 11 {
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
        let dir: Vec<bool> = b[o..o + n].iter().map(|&x| x != 0).collect();
        // comprobación de coherencia: evita pánicos con un fichero dañado
        if (0..n).any(|i| off[i] as usize + len[i] as usize > alen || (parent[i] != NO_PARENT && parent[i] as usize >= n)) {
            return None;
        }
        Some(Index { arena, off, len, parent, dir, built })
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
    dirs::cache_dir().unwrap_or_else(std::env::temp_dir).join("filebrowser").join("index.bin")
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
pub fn disable() {
    GENERATION.fetch_add(1, AO::SeqCst);
    watch::stop();
    if let Ok(mut g) = CURRENT.write() {
        *g = None;
    }
    let _ = std::fs::remove_file(cache_file());
}

/// Activa el índice: carga el guardado (si coincide con las raíces) y recorre de nuevo si hace falta.
pub fn start(roots: Vec<PathBuf>, ctx: egui::Context) {
    if LOADING.swap(true, AO::SeqCst) {
        return;
    }
    let gener = GENERATION.load(AO::SeqCst);
    std::thread::spawn(move || {
        // no compite con el arranque: el primer fotograma y el listado inicial van antes
        std::thread::sleep(Duration::from_millis(2500));
        if let Some(idx) = Index::load(&cache_file()) {
            if idx.roots() == roots && GENERATION.load(AO::SeqCst) == gener {
                PROGRESS.store(idx.count(), AO::Relaxed);
                let idx = Arc::new(idx);
                if let Ok(mut g) = CURRENT.write() {
                    *g = Some(idx.clone());
                }
                watch::restart(idx);
            }
        }
        LOADING.store(false, AO::SeqCst);
        ctx.request_repaint();
        if GENERATION.load(AO::SeqCst) == gener && current().map(|i| now().saturating_sub(i.built) > STALE_SECS).unwrap_or(true) {
            rebuild(roots, ctx);
        }
    });
}

/// Llamar de vez en cuando: refresca en segundo plano un índice que se ha quedado viejo.
pub fn tick(roots: Vec<PathBuf>, ctx: &egui::Context) {
    if BUILDING.load(AO::Relaxed) || LOADING.load(AO::Relaxed) {
        return;
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
    if BUILDING.swap(true, AO::SeqCst) {
        return;
    }
    let gener = GENERATION.load(AO::SeqCst);
    PROGRESS.store(0, AO::Relaxed);
    let started = now_ms();
    std::thread::spawn(move || {
        let mut idx = Index::default();
        let mut stack = Vec::new();
        for r in &roots {
            let id = idx.push(&r.to_string_lossy(), NO_PARENT, true);
            stack.push((id, r.clone()));
        }
        let shared = Arc::new((Mutex::new(Work { stack, active: 0 }), Condvar::new()));
        let idx = Arc::new(Mutex::new(idx));
        let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(2).clamp(2, 4);
        let hs: Vec<_> = (0..threads)
            .map(|_| {
                let (shared, idx, ctx) = (shared.clone(), idx.clone(), ctx.clone());
                std::thread::spawn(move || worker(&shared, &idx, gener, &ctx))
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
            idx.dir.shrink_to_fit();
            idx.save(&cache_file());
            let idx = Arc::new(idx);
            if let Ok(mut g) = CURRENT.write() {
                *g = Some(idx.clone());
            }
            // los cambios anteriores al recorrido ya están reflejados
            if DIRTY.load(AO::Relaxed) < started {
                DIRTY.store(0, AO::Relaxed);
            }
            watch::restart(idx);
        }
        BUILDING.store(false, AO::SeqCst);
        ctx.request_repaint();
    });
}

fn worker(shared: &(Mutex<Work>, Condvar), idx: &Mutex<Index>, gener: u64, ctx: &egui::Context) {
    let (lock, cv) = shared;
    let mut last = std::time::Instant::now();
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
        let mut kids: Vec<(OsString, bool)> = Vec::new();
        if let Ok(rd) = std::fs::read_dir(&dir) {
            for e in rd.flatten() {
                let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
                kids.push((e.file_name(), is_dir));
            }
        }
        let mut subdirs = Vec::new();
        {
            let mut ix = idx.lock().unwrap();
            for (name, is_dir) in &kids {
                let n = name.to_string_lossy();
                let nid = ix.push(&n, id, *is_dir);
                if *is_dir {
                    let p = dir.join(name);
                    if !crate::tools::pseudo_fs(&p) {
                        subdirs.push((nid, p));
                    }
                }
            }
            PROGRESS.store(ix.count(), AO::Relaxed);
        }
        {
            let mut w = lock.lock().unwrap();
            w.stack.extend(subdirs);
            w.active -= 1;
        }
        cv.notify_all();
        if last.elapsed().as_millis() > 300 {
            last = std::time::Instant::now();
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

    pub fn restart(idx: Arc<Index>) {
        let gen_id = WATCH_GEN.fetch_add(1, AO::SeqCst) + 1;
        std::thread::spawn(move || run(idx, gen_id));
    }

    fn run(idx: Arc<Index>, gen_id: u64) {
        let limit = std::fs::read_to_string("/proc/sys/fs/inotify/max_user_watches")
            .ok()
            .and_then(|s| s.trim().parse::<usize>().ok())
            .unwrap_or(8192);
        // dejamos la mitad del cupo del usuario para el resto de programas
        let budget = (limit / 2).min(100_000);
        let fd = unsafe { libc::inotify_init1(libc::IN_NONBLOCK | libc::IN_CLOEXEC) };
        if fd < 0 {
            return;
        }
        // las carpetas más cercanas a la raíz primero: son las que más cambian de interés
        let n = idx.count();
        let mut depth = vec![u8::MAX; n];
        let mut dirs: Vec<(u8, u32)> = Vec::new();
        for i in 0..n {
            if !idx.dir[i] {
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
            if crate::tools::pseudo_fs(&p) {
                continue;
            }
            use std::os::unix::ffi::OsStrExt;
            let Ok(c) = std::ffi::CString::new(p.as_os_str().as_bytes()) else { continue };
            if unsafe { libc::inotify_add_watch(fd, c.as_ptr(), mask) } >= 0 {
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
                    mark_dirty();
                }
            }
        }
        unsafe { libc::close(fd) };
    }
}

/// Rutas del sistema o de herramientas que cambian sin parar y no interesan para buscar.
#[cfg(not(target_os = "linux"))]
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

    static HANDLES: Mutex<Vec<isize>> = Mutex::new(Vec::new());

    pub fn stop() {
        let hs: Vec<isize> = std::mem::take(&mut *HANDLES.lock().unwrap());
        for h in hs {
            // despierta al hilo bloqueado en ReadDirectoryChangesW: sale y cierra el manejador
            unsafe {
                let _ = CancelIoEx(HANDLE(h as *mut _), None);
            }
        }
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
            HANDLES.lock().unwrap().push(raw);
            let base = root.to_string_lossy().into_owned();
            std::thread::spawn(move || {
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
                        mark_dirty();
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
                            if !noisy_path(&format!("{base}{rel}")) {
                                mark_dirty();
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
        std::thread::spawn(move || {
            let (tx, rx) = std::sync::mpsc::channel();
            let mut fs = fsevent_sys::FsEvent::new(roots);
            if fs.observe_async(tx).is_err() {
                return;
            }
            while WATCH_GEN.load(AO::SeqCst) == gen_id {
                match rx.recv_timeout(Duration::from_secs(1)) {
                    Ok(ev) => {
                        if !noisy_path(&ev.path) {
                            mark_dirty();
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

/// Busca en el índice. Devuelve `None` si no hay índice o la carpeta no está en él
/// (el llamador debe entonces recorrer el disco como siempre).
pub fn spawn_search(scope: Option<PathBuf>, needle: String, hidden: bool, ctx: egui::Context) -> Option<Arc<Search>> {
    let idx = current()?;
    let scope_id = match &scope {
        Some(p) => Some(idx.find_dir(p)?),
        None => None,
    };
    let st = Arc::new(Search::default());
    st.scanned.store(idx.count() as u64, AO::Relaxed);
    let s = st.clone();
    std::thread::spawn(move || {
        let needle = needle.to_lowercase();
        let n = idx.count();
        let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(2).clamp(1, 8);
        let chunk = n.div_ceil(threads).max(1);
        let hits: Vec<Vec<u32>> = std::thread::scope(|sc| {
            let hs: Vec<_> = (0..threads)
                .map(|t| {
                    let (idx, needle) = (&idx, &needle);
                    sc.spawn(move || {
                        let (a, b) = ((t * chunk).min(n), ((t + 1) * chunk).min(n));
                        (a..b)
                            .filter(|&i| idx.parent[i] != NO_PARENT && crate::fsys::contains_ci(idx.name(i), needle))
                            .map(|i| i as u32)
                            .collect::<Vec<u32>>()
                    })
                })
                .collect();
            hs.into_iter().map(|h| h.join().unwrap_or_default()).collect()
        });
        let mut found = 0usize;
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
                if idx.name(cur).starts_with('.') {
                    hid = true;
                }
                cur = p as usize;
            }
            if scope_id == Some(i) || !inside || (hid && !hidden) {
                continue;
            }
            s.results.lock().unwrap().push((idx.path_of(i as usize), idx.dir[i as usize]));
            found += 1;
            if found >= MAX_RESULTS {
                break 'o;
            }
        }
        s.done.store(true, AO::Release);
        ctx.request_repaint();
    });
    Some(st)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_search() {
        let d = std::env::temp_dir().join(format!("fbidx{}", std::process::id()));
        std::fs::create_dir_all(d.join("a/b")).unwrap();
        std::fs::create_dir_all(d.join(".oculta")).unwrap();
        std::fs::write(d.join("a/b/Informe.txt"), "x").unwrap();
        std::fs::write(d.join(".oculta/informe2.txt"), "x").unwrap();
        let mut ix = Index::default();
        let r = ix.push(&d.to_string_lossy(), NO_PARENT, true);
        let a = ix.push("a", r, true);
        let b = ix.push("b", a, true);
        ix.push("Informe.txt", b, false);
        let f = d.join("idx.bin");
        ix.save(&f);
        let l = Index::load(&f).unwrap();
        assert_eq!(l.count(), 4);
        assert_eq!(l.path_of(3), d.join("a/b/Informe.txt"));
        assert_eq!(l.find_dir(&d.join("a/b")), Some(2));
        assert_eq!(l.find_dir(&d.join("zz")), None);
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    #[ignore]
    fn bench_home() {
        let ctx = egui::Context::default();
        let t = std::time::Instant::now();
        rebuild(vec![dirs::home_dir().unwrap()], ctx.clone());
        while BUILDING.load(AO::SeqCst) {
            std::thread::sleep(Duration::from_millis(50));
        }
        let ix = current().unwrap();
        eprintln!("{} entradas en {:?}, arena {} MB", ix.count(), t.elapsed(), ix.arena.len() >> 20);
        let t = std::time::Instant::now();
        let s = spawn_search(None, "informe".into(), false, ctx).unwrap();
        while !s.done.load(AO::Acquire) {
            std::thread::sleep(Duration::from_millis(1));
        }
        eprintln!("búsqueda: {} resultados en {:?}", s.results.lock().unwrap().len(), t.elapsed());
        let _ = std::fs::remove_file(cache_file());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn watcher_marks_dirty() {
        let d = std::env::temp_dir().join(format!("fbwatch{}", std::process::id()));
        std::fs::create_dir_all(d.join("sub")).unwrap();
        let mut ix = Index::default();
        let r = ix.push(&d.to_string_lossy(), NO_PARENT, true);
        ix.push("sub", r, true);
        DIRTY.store(0, AO::Relaxed);
        watch::restart(Arc::new(ix));
        std::thread::sleep(Duration::from_millis(500));
        std::fs::write(d.join("sub/nuevo.txt"), "x").unwrap();
        let t = std::time::Instant::now();
        while DIRTY.load(AO::Relaxed) == 0 && t.elapsed() < Duration::from_secs(3) {
            std::thread::sleep(Duration::from_millis(20));
        }
        watch::stop();
        std::fs::remove_dir_all(&d).unwrap();
        assert!(DIRTY.load(AO::Relaxed) != 0);
    }
}
