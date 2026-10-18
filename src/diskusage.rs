//! Uso de disco de una carpeta: árbol de tamaños (con límite de profundidad y de hijos) calculado en un hilo.
#[allow(unused_imports)]
use crate::vfs::{KindV as _, PathV as _};
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
    budget: Option<(u64, web_time::Instant)>,
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
        budget: (limited || crate::vfs::WEB).then(|| (200_000, web_time::Instant::now() + std::time::Duration::from_secs(10))),
        egui: ctx.clone(),
        // se rellena en el hilo: `metadata` sobre un montaje de red caído bloquearía la interfaz
        #[cfg(unix)]
        dev: 0,
    };
    let (done, result) = (scan.done.clone(), scan.result.clone());
    crate::vfs::thread::spawn(move || {
        #[cfg(unix)]
        let c = {
            use std::os::unix::fs::MetadataExt;
            let mut c = c;
            c.dev = crate::vfs::fs::metadata(&path).map(|m| m.dev()).unwrap_or(0);
            c
        };
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
            if self.files.load(AO::Relaxed) >= max || web_time::Instant::now() >= end {
                self.partial.store(true, AO::Relaxed);
                return true;
            }
        }
        false
    }
}

fn file_size(md: &crate::vfs::fs::Metadata) -> u64 {
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
    /// Archivos de la carpeta que se conservan (los mayores); el resto sólo suma a `own`.
    files: Vec<(u64, String)>,
    dropped: u64,
}

/// Recorrido por niveles: se termina de mirar un nivel de carpetas antes de pasar al siguiente, de
/// modo que, si se agota el presupuesto, todas las ramas están igual de exploradas.
fn scan_bfs(root: &Path, name: String, c: &Ctx) -> Node {
    let mut arena: Vec<Ent> = vec![Ent { name, dir: true, own: 0, kids: Vec::new(), files: Vec::new(), dropped: 0 }];
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
            let Ok(rd) = crate::vfs::fs::read_dir(&dir) else { continue };
            for e in rd.flatten() {
                let Ok(ft) = e.file_type() else { continue };
                let n = e.file_name().to_string_lossy().into_owned();
                if ft.v_is_dir() {
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::MetadataExt;
                        // no cruza a otros sistemas de archivos (/proc, montajes…)
                        if e.metadata().map(|m| m.dev() != c.dev).unwrap_or(true) {
                            continue;
                        }
                    }
                    let k = arena.len();
                    arena.push(Ent { name: n, dir: true, own: 0, kids: Vec::new(), files: Vec::new(), dropped: 0 });
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
                        crate::vfs::thread::sleep(std::time::Duration::from_millis(1));
                    }
                    arena[idx].own += sz;
                    // sólo se conservan individualmente los archivos de los anillos visibles
                    // y de cada carpeta sólo los mayores: una carpeta con millones de archivos no
                    // debe llenar la memoria
                    if depth < DEPTH {
                        let d = &mut arena[idx];
                        d.files.push((sz, n));
                        if d.files.len() >= KIDS * 4 {
                            d.files.sort_by(|a, b| b.0.cmp(&a.0));
                            d.dropped += (d.files.len() - KIDS) as u64;
                            d.files.truncate(KIDS);
                        }
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
    // los archivos conservados pasan a ser entradas del árbol
    for i in 0..arena.len() {
        if arena[i].files.is_empty() {
            continue;
        }
        let mut files = std::mem::take(&mut arena[i].files);
        files.sort_by(|a, b| b.0.cmp(&a.0));
        if files.len() > KIDS {
            arena[i].dropped += (files.len() - KIDS) as u64;
            files.truncate(KIDS);
        }
        for (sz, n) in files {
            let k = arena.len();
            arena.push(Ent { name: n, dir: false, own: sz, kids: Vec::new(), files: Vec::new(), dropped: 0 });
            arena[i].kids.push(k);
        }
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
    let mut extra = e.dropped;
    if kids.len() > KIDS {
        extra += (kids.len() - KIDS) as u64;
        kids.truncate(KIDS);
    }
    let kept: u64 = kids.iter().map(|k| k.size).sum();
    let rest = node.size.saturating_sub(kept);
    if extra > 0 && rest > 0 {
        kids.push(Node { name: format!("… ({extra})"), size: rest, dir: false, other: true, kids: Vec::new() });
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn many_files_keep_only_largest_and_totals_match() {
        let d = crate::vfs::temp_dir().join(format!("fb_du_{}", crate::vfs::pid()));
        let _ = crate::vfs::fs::remove_dir_all(&d);
        crate::vfs::fs::create_dir_all(d.join("sub")).unwrap();
        for i in 0..400 {
            crate::vfs::fs::write(d.join(format!("f{i}")), vec![0u8; 5000 + i]).unwrap();
        }
        crate::vfs::fs::write(d.join("sub/x"), vec![0u8; 9000]).unwrap();
        let c = Ctx {
            bytes: Arc::new(AtomicU64::new(0)),
            files: Arc::new(AtomicU64::new(0)),
            cancel: Arc::new(AtomicBool::new(false)),
            partial: Arc::new(AtomicBool::new(false)),
            budget: None,
            egui: egui::Context::default(),
            #[cfg(unix)]
            dev: {
                use std::os::unix::fs::MetadataExt;
                crate::vfs::fs::metadata(&d).unwrap().dev()
            },
        };
        let n = scan_bfs(&d, "r".into(), &c);
        let sum: u64 = n.kids.iter().map(|k| k.size).sum();
        assert_eq!(sum, n.size);
        assert!(n.kids.len() <= KIDS + 2);
        assert!(n.kids.iter().any(|k| k.other));
        let _ = crate::vfs::fs::remove_dir_all(&d);
    }
}
