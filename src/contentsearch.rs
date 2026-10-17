//! Búsqueda por contenido: texto plano y documentos (PDF, Office, OpenDocument…) que contienen una frase.
//! No hay índice: se recorre la carpeta en el momento con varios hilos, así que no cuesta nada en reposo.
use crate::tools::{pseudo_fs, Search, MAX_RESULTS};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering as AO;
use std::sync::mpsc::sync_channel;
use std::sync::{Arc, Mutex};

/// Texto plano: se lee como mucho esto de cada archivo.
const TEXT_MAX: u64 = 16 << 20;
/// Documentos: tamaño máximo y páginas que se examinan.
const DOC_MAX: u64 = 40 << 20;
const DOC_PAGES: usize = 300;

pub fn spawn(roots: Vec<PathBuf>, needle: String, hidden: bool, ctx: egui::Context) -> Arc<Search> {
    let st = Arc::new(Search::default());
    let s = st.clone();
    std::thread::spawn(move || {
        let needle = needle.to_lowercase();
        let n = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(2).clamp(1, 6);
        let (tx, rx) = sync_channel::<PathBuf>(256);
        let rx = Arc::new(Mutex::new(rx));
        let found = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut workers = Vec::new();
        for _ in 0..n {
            let (rx, s, needle, found, ctx) = (rx.clone(), s.clone(), needle.clone(), found.clone(), ctx.clone());
            workers.push(std::thread::spawn(move || loop {
                let next = rx.lock().unwrap().recv();
                let Ok(p) = next else { return };
                if s.cancel.load(AO::Relaxed) || found.load(AO::Relaxed) >= MAX_RESULTS {
                    continue;
                }
                s.scanned.fetch_add(1, AO::Relaxed);
                if file_matches(&p, &needle) {
                    s.results.lock().unwrap().push((p, false));
                    found.fetch_add(1, AO::Relaxed);
                    ctx.request_repaint();
                }
            }));
        }
        let mut stack = roots;
        'walk: while let Some(dir) = stack.pop() {
            let Ok(rd) = fs::read_dir(&dir) else { continue };
            for e in rd.flatten() {
                if s.cancel.load(AO::Relaxed) || found.load(AO::Relaxed) >= MAX_RESULTS {
                    break 'walk;
                }
                if !hidden && e.file_name().to_string_lossy().starts_with('.') {
                    continue;
                }
                let Ok(ft) = e.file_type() else { continue };
                if ft.is_dir() {
                    if !pseudo_fs(&e.path()) {
                        stack.push(e.path());
                    }
                } else if ft.is_file() && tx.send(e.path()).is_err() {
                    break 'walk;
                }
            }
        }
        drop(tx);
        for w in workers {
            let _ = w.join();
        }
        s.done.store(true, AO::Release);
        ctx.request_repaint();
    });
    st
}

fn file_matches(p: &Path, needle: &str) -> bool {
    let Ok(md) = fs::metadata(p) else { return false };
    if crate::docview::is_doc_path(p) {
        return md.len() <= DOC_MAX && doc_contains(p, needle);
    }
    if md.len() == 0 {
        return false;
    }
    let Ok(f) = fs::File::open(p) else { return false };
    let mut buf = Vec::new();
    if f.take(TEXT_MAX).read_to_end(&mut buf).is_err() {
        return false;
    }
    // binario: hay bytes nulos al principio
    if buf[..buf.len().min(4096)].contains(&0) {
        return false;
    }
    crate::fsys::contains_ci(&String::from_utf8_lossy(&buf), needle)
}

fn doc_contains(p: &Path, needle: &str) -> bool {
    let Ok(mut doc) = apex_view::Document::open_path(p) else { return false };
    for page in 0..doc.page_count().min(DOC_PAGES) {
        let Ok(runs) = doc.page_runs(page) else { continue };
        let text = runs.iter().map(|r| r.text.as_str()).collect::<Vec<_>>().join(" ");
        if crate::fsys::contains_ci(&text, needle) {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_text_and_skips_binary() {
        let d = std::env::temp_dir().join(format!("fb_cs_{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(d.join("sub")).unwrap();
        fs::write(d.join("a.txt"), "Hola Mundo cruel").unwrap();
        fs::write(d.join("sub/b.rs"), "fn main() { /* mundo */ }").unwrap();
        fs::write(d.join("c.bin"), b"\0\0mundo\0").unwrap();
        fs::write(d.join("d.txt"), "nada").unwrap();
        let st = spawn(vec![d.clone()], "MUNDO".into(), false, egui::Context::default());
        while !st.done.load(AO::Acquire) {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let mut names: Vec<String> = st.results.lock().unwrap().iter().map(|(p, _)| p.file_name().unwrap().to_string_lossy().into_owned()).collect();
        names.sort();
        assert_eq!(names, ["a.txt", "b.rs"]);
        let _ = fs::remove_dir_all(&d);
    }
}
