//! Recorrido recursivo acotado para ZIP y búsqueda. No entra en enlaces simbólicos
//! (evita ciclos y escapes): los entrega marcados (`link`) para que quien llama decida, y
//! respeta las mismas reglas de nombres que el resto.

use crate::jail::valid_component;
use std::path::{Path, PathBuf};

pub struct Item {
    /// Componentes relativos a la carpeta de partida.
    pub rel: Vec<String>,
    pub path: PathBuf,
    pub is_dir: bool,
    /// Enlace simbólico (sin seguir: `is_dir` es falso y `size` 0).
    pub link: bool,
    pub size: u64,
    pub mtime: i64,
}

/// Lo que el recorrido no pudo ver: carpetas ilegibles (rutas relativas, como mucho `MAX_NOTED`) y si
/// se cortó por el tope de trabajo o de profundidad.
#[derive(Default)]
pub struct Missed {
    pub unreadable: Vec<String>,
    pub unreadable_count: usize,
    pub cut: bool,
}

const MAX_DEPTH: usize = 64;
pub const MAX_NOTED: usize = 1000;

/// Recorre `start` en profundidad. `f` devuelve `false` para parar. `max_visited` acota el trabajo total.
pub fn walk(start: &Path, hidden: bool, max_visited: usize, f: &mut dyn FnMut(Item) -> bool) -> Missed {
    let mut stack: Vec<(PathBuf, Vec<String>)> = vec![(start.to_path_buf(), Vec::new())];
    let mut visited = 0usize;
    let mut missed = Missed::default();
    while let Some((dir, rel)) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            missed.unreadable_count += 1;
            if missed.unreadable.len() < MAX_NOTED {
                missed.unreadable.push(if rel.is_empty() { ".".into() } else { rel.join("/") });
            }
            continue;
        };
        let mut subdirs = Vec::new();
        for e in rd.flatten() {
            visited += 1;
            if visited > max_visited {
                missed.cut = true;
                return missed;
            }
            let Ok(name) = e.file_name().into_string() else { continue };
            if !valid_component(&name) || (!hidden && name.starts_with('.')) {
                continue;
            }
            let Ok(md) = e.path().symlink_metadata() else { continue };
            let is_dir = md.is_dir();
            let link = md.file_type().is_symlink();
            if !is_dir && !md.is_file() && !link {
                continue; // dispositivos, sockets…
            }
            let mut r = rel.clone();
            r.push(name);
            let mtime = md.modified().map(crate::timefmt::unix).unwrap_or(0);
            let item = Item { rel: r.clone(), path: e.path(), is_dir, link, size: if md.is_file() { md.len() } else { 0 }, mtime };
            if is_dir {
                if r.len() < MAX_DEPTH {
                    subdirs.push((e.path(), r));
                } else {
                    missed.cut = true;
                }
            }
            if !f(item) {
                return missed;
            }
        }
        subdirs.reverse();
        stack.extend(subdirs);
    }
    missed
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn recorre_sin_ocultos_ni_enlaces() {
        let d = std::env::temp_dir().join(format!("fbs-walk-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(d.join("a/b")).unwrap();
        fs::write(d.join("a/b/f"), "x").unwrap();
        fs::write(d.join(".o"), "x").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&d, d.join("a/ciclo")).unwrap();
        let mut v = Vec::new();
        walk(&d, false, 1000, &mut |i| {
            if !i.link {
                v.push(i.rel.join("/"));
            }
            true
        });
        v.sort();
        assert_eq!(v, ["a", "a/b", "a/b/f"]);
        // el enlace se entrega marcado, sin entrar en él
        #[cfg(unix)]
        {
            let mut links = Vec::new();
            walk(&d, false, 1000, &mut |i| {
                if i.link {
                    links.push(i.rel.join("/"));
                }
                true
            });
            assert_eq!(links, ["a/ciclo"]);
        }
        let mut n = 0;
        walk(&d, true, 2, &mut |_| {
            n += 1;
            true
        });
        assert!(n <= 2);
        let _ = fs::remove_dir_all(&d);
    }
}
