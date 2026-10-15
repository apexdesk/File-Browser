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
    egui: egui::Context,
    #[cfg(unix)]
    dev: u64,
}

pub fn start(path: PathBuf, ctx: &egui::Context) -> Scan {
    let scan = Scan {
        bytes: Arc::new(AtomicU64::new(0)),
        files: Arc::new(AtomicU64::new(0)),
        done: Arc::new(AtomicBool::new(false)),
        cancel: Arc::new(AtomicBool::new(false)),
        result: Arc::new(Mutex::new(None)),
    };
    let c = Ctx {
        bytes: scan.bytes.clone(),
        files: scan.files.clone(),
        cancel: scan.cancel.clone(),
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
        let node = walk(&path, name, 0, &c);
        if !c.cancel.load(AO::Relaxed) {
            *result.lock().unwrap() = Some(node);
        }
        done.store(true, AO::Release);
        c.egui.request_repaint();
    });
    scan
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

fn walk(path: &Path, name: String, depth: usize, c: &Ctx) -> Node {
    let mut node = Node { name, size: 0, dir: true, other: false, kids: Vec::new() };
    let Ok(rd) = std::fs::read_dir(path) else { return node };
    let mut kids: Vec<Node> = Vec::new();
    for e in rd.flatten() {
        if c.cancel.load(AO::Relaxed) {
            break;
        }
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
            let sub = walk(&e.path(), n, depth + 1, c);
            node.size += sub.size;
            kids.push(sub);
        } else {
            let sz = e.metadata().map(|m| file_size(&m)).unwrap_or(0);
            node.size += sz;
            c.bytes.fetch_add(sz, AO::Relaxed);
            if c.files.fetch_add(1, AO::Relaxed) % 2000 == 0 {
                c.egui.request_repaint();
            }
            kids.push(Node { name: n, size: sz, dir: false, other: false, kids: Vec::new() });
        }
    }
    if depth >= DEPTH {
        return node; // sin detalle por debajo del último anillo
    }
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
