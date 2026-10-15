//! Uso de disco de una carpeta: árbol de tamaños (con límite de profundidad y de hijos) calculado en un hilo.
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering as AO};
use std::sync::{Arc, Mutex};

/// Profundidad máxima que se conserva (anillos del gráfico).
pub const DEPTH: usize = 4;
/// Hijos que se conservan por nodo; el resto se agrupa en «otros».
const KIDS: usize = 40;

pub struct Node {
    pub name: String,
    pub size: u64,
    pub dir: bool,
    /// Agrupa los elementos pequeños que no se muestran por separado.
    pub other: bool,
    pub kids: Vec<Node>,
}

pub struct Scan {
    pub bytes: Arc<AtomicU64>,
    pub files: Arc<AtomicU64>,
    pub done: Arc<AtomicBool>,
    pub cancel: Arc<AtomicBool>,
    /// el análisis se cortó por el presupuesto: los tamaños son aproximados
    pub partial: Arc<AtomicBool>,
    pub result: Arc<Mutex<Option<Node>>>,
}

impl Drop for Scan {
    fn drop(&mut self) {
        self.cancel.store(true, AO::Relaxed);
    }
}

struct Ctx {
    bytes: Arc<AtomicU64>,
    files: Arc<AtomicU64>,
    cancel: Arc<AtomicBool>,
    partial: Arc<AtomicBool>,
    /// (archivos, instante límite); `None` = sin límite
    budget: Option<(u64, std::time::Instant)>,
    egui: egui::Context,
    #[cfg(unix)]
    dev: u64,
}

/// `limited`: corta a los ~200 000 archivos o 10 s y marca el resultado como parcial.
pub fn start(path: PathBuf, ctx: &egui::Context, limited: bool) -> Scan {
    let scan = Scan {
        bytes: Arc::new(AtomicU64::new(0)),
        files: Arc::new(AtomicU64::new(0)),
        done: Arc::new(AtomicBool::new(false)),
        cancel: Arc::new(AtomicBool::new(false)),
        partial: Arc::new(AtomicBool::new(false)),
        result: Arc::new(Mutex::new(None)),
    };
    let c = Ctx {
        bytes: scan.bytes.clone(),
        files: scan.files.clone(),
        cancel: scan.cancel.clone(),
        partial: scan.partial.clone(),
        budget: limited.then(|| (200_000, std::time::Instant::now() + std::time::Duration::from_secs(10))),
        egui: ctx.clone(),
        #[cfg(unix)]
        dev: {
            use std::os::unix::fs::MetadataExt;
            std::fs::metadata(&path).map(|m| m.dev()).unwrap_or(0)
        },
    };
    let (done, result) = (scan.done.clone(), scan.result.clone());
    std::thread::spawn(move || {
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| path.display().to_string());
        let node = scan_bfs(&path, name, &c);
        if !c.cancel.load(AO::Relaxed) {
            *result.lock().unwrap() = Some(node);
        }
        done.store(true, AO::Release);
        c.egui.request_repaint();
    });
    scan
}

impl Ctx {
    fn stop(&self) -> bool {
        if self.cancel.load(AO::Relaxed) {
            return true;
        }
        if let Some((max, end)) = self.budget {
            if self.files.load(AO::Relaxed) >= max || std::time::Instant::now() >= end {
                self.partial.store(true, AO::Relaxed);
                return true;
            }
        }
        false
    }
}

fn file_size(md: &std::fs::Metadata) -> u64 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        md.blocks() * 512
    }
    #[cfg(not(unix))]
    {
        md.len()
    }
}

/// Entrada del árbol en construcción (los hijos siempre tienen índice mayor que el padre).
struct Ent {
    name: String,
    dir: bool,
    own: u64,
    kids: Vec<usize>,
}

/// Recorrido por niveles: se termina de mirar un nivel de carpetas antes de pasar al siguiente, de
/// modo que, si se agota el presupuesto, todas las ramas están igual de exploradas.
fn scan_bfs(root: &Path, name: String, c: &Ctx) -> Node {
    let mut arena: Vec<Ent> = vec![Ent { name, dir: true, own: 0, kids: Vec::new() }];
    let mut level: Vec<(usize, PathBuf)> = vec![(0, root.to_path_buf())];
    let mut depth = 0usize;
    let mut cut = false;
    'outer: while !level.is_empty() {
        let mut next: Vec<(usize, PathBuf)> = Vec::new();
        for (idx, dir) in level {
            if c.stop() {
                cut = true;
                break 'outer;
            }
            let Ok(rd) = std::fs::read_dir(&dir) else { continue };
            for e in rd.flatten() {
                let Ok(ft) = e.file_type() else { continue };
                let n = e.file_name().to_string_lossy().into_owned();
                if ft.is_dir() {
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::MetadataExt;
                        // no cruza a otros sistemas de archivos (/proc, montajes…)
                        if e.metadata().map(|m| m.dev() != c.dev).unwrap_or(true) {
                            continue;
                        }
                    }
                    let k = arena.len();
                    arena.push(Ent { name: n, dir: true, own: 0, kids: Vec::new() });
                    arena[idx].kids.push(k);
                    next.push((k, e.path()));
                } else {
                    let sz = e.metadata().map(|m| file_size(&m)).unwrap_or(0);
                    c.bytes.fetch_add(sz, AO::Relaxed);
                    let cnt = c.files.fetch_add(1, AO::Relaxed);
                    if cnt % 2000 == 0 {
                        c.egui.request_repaint();
                    }
                    if cnt % 512 == 511 {
                        // baja prioridad: deja respirar al disco y a la CPU
                        std::thread::sleep(std::time::Duration::from_millis(1));
                    }
                    arena[idx].own += sz;
                    // sólo se conservan individualmente los archivos de los anillos visibles
                    if depth < DEPTH {
                        let k = arena.len();
                        arena.push(Ent { name: n, dir: false, own: sz, kids: Vec::new() });
                        arena[idx].kids.push(k);
                    }
                }
            }
        }
        level = next;
        depth += 1;
    }
    if cut {
        c.partial.store(true, AO::Relaxed);
    }
    // totales: de las hojas hacia la raíz (los hijos tienen índice mayor)
    let mut total: Vec<u64> = arena.iter().map(|e| e.own).collect();
    let mut parent = vec![usize::MAX; arena.len()];
    for (i, e) in arena.iter().enumerate() {
        for &k in &e.kids {
            parent[k] = i;
        }
    }
    for i in (1..arena.len()).rev() {
        if parent[i] != usize::MAX && arena[i].dir {
            let t = total[i];
            total[parent[i]] += t;
        }
    }
    build(&arena, &total, 0, 0)
}

fn build(arena: &[Ent], total: &[u64], i: usize, depth: usize) -> Node {
    let e = &arena[i];
    let mut node = Node { name: e.name.clone(), size: if e.dir { total[i] } else { e.own }, dir: e.dir, other: false, kids: Vec::new() };
    if depth >= DEPTH || e.kids.is_empty() {
        return node;
    }
    let mut kids: Vec<Node> = e.kids.iter().map(|&k| build(arena, total, k, depth + 1)).collect();
    kids.sort_by(|a, b| b.size.cmp(&a.size));
    if kids.len() > KIDS {
        let rest: u64 = kids[KIDS..].iter().map(|k| k.size).sum();
        let n = kids.len() - KIDS;
        kids.truncate(KIDS);
        kids.push(Node { name: format!("… ({n})"), size: rest, dir: false, other: true, kids: Vec::new() });
    }
    node.kids = kids;
    node
}

impl Node {
    pub fn at(&self, path: &[usize]) -> &Node {
        let mut n = self;
        for &i in path {
            match n.kids.get(i) {
                Some(k) => n = k,
                None => break,
            }
        }
        n
    }
}
