//! Búsqueda por contenido: texto plano y documentos (PDF, Office, OpenDocument…) que contienen una frase.
//! No hay índice: se recorre la carpeta en el momento con varios hilos, así que no cuesta nada en reposo.
//!
//! En el espejo de un servidor SFTP conectado los archivos son marcadores vacíos: cada carpeta se
//! refleja al recorrerla y cada archivo candidato se descarga antes de examinarlo (queda en la caché
//! del espejo). Para no traerse el servidor entero hay dos topes: `REMOTE_FILE_MAX` por archivo y
//! `REMOTE_TOTAL_MAX` de descarga por búsqueda; lo que no se examina por ellos se cuenta y se avisa al
//! terminar (`take_note`). Lo ya descargado no gasta del tope. Vídeo, audio, comprimidos e imágenes
//! (salvo SVG) del servidor ni se descargan: no tienen texto que buscar.
#[allow(unused_imports)]
use crate::vfs::{KindV as _, PathV as _};
use crate::tools::{pseudo_fs, Search, MAX_RESULTS};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize};
use crate::vfs::fs;
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
/// En la web la búsqueda se hace de golpe en el hilo de la interfaz: archivos examinados como máximo.
const WEB_MAX_FILES: u64 = 3000;
/// Servidor SFTP: tamaño máximo de un archivo que se descarga para examinarlo (8 MB: cubre de sobra
/// código, registros y documentos normales; uno mayor por una conexión lenta tardaría demasiado).
const REMOTE_FILE_MAX: u64 = 8 << 20;
/// Servidor SFTP: descarga total como máximo en una búsqueda (128 MB).
const REMOTE_TOTAL_MAX: u64 = 128 << 20;

/// Descargas de una búsqueda en un servidor SFTP: lo gastado del tope y lo que se dejó sin examinar.
#[derive(Default)]
struct Remote {
    used: AtomicU64,
    skipped: AtomicUsize,
}

/// Aviso pendiente de cada búsqueda (por la dirección de su `Search`) que dejó archivos sin examinar.
static NOTES: Mutex<Vec<(usize, std::sync::Weak<Search>, usize)>> = Mutex::new(Vec::new());

/// Aviso (ya traducido) de la búsqueda `st` si dejó archivos del servidor sin examinar; sólo una vez.
pub fn take_note(st: &Arc<Search>) -> Option<String> {
    let mut g = NOTES.lock().unwrap_or_else(|e| e.into_inner());
    let key = Arc::as_ptr(st) as usize;
    let i = g.iter().position(|(k, w, _)| *k == key && w.upgrade().is_some_and(|w| Arc::ptr_eq(&w, st)))?;
    let n = g.remove(i).2;
    Some(crate::i18n::tf("{} archivos del servidor no se examinaron: más de 8 MB cada uno o más de 128 MB descargados en total", &[&n]))
}

pub fn spawn(roots: Vec<PathBuf>, needle: String, hidden: bool, ctx: egui::Context) -> Arc<Search> {
    let st = Arc::new(Search::default());
    let s = st.clone();
    crate::vfs::thread::spawn(move || {
        let needle = crate::tools::fold(&needle);
        let n = crate::vfs::thread::available_parallelism().map(|n| n.get()).unwrap_or(2).clamp(1, 6);
        let (tx, rx) = sync_channel::<PathBuf>(256);
        let rx = Arc::new(Mutex::new(rx));
        let found = Arc::new(AtomicUsize::new(0));
        let remote = Arc::new(Remote::default());
        let mut workers = Vec::new();
        // sin hilos (web) no hay trabajadores esperando en el canal: se examina en el propio recorrido
        for _ in 0..if crate::vfs::WEB { 0 } else { n } {
            let (rx, s, needle, found, ctx, remote) = (rx.clone(), s.clone(), needle.clone(), found.clone(), ctx.clone(), remote.clone());
            workers.push(crate::vfs::thread::spawn(move || loop {
                let next = rx.lock().unwrap().recv();
                let Ok(p) = next else { return };
                if s.cancel.load(AO::Relaxed) || found.load(AO::Relaxed) >= MAX_RESULTS {
                    continue;
                }
                s.scanned.fetch_add(1, AO::Relaxed);
                if file_matches(&p, &needle, &s.cancel, &remote) {
                    s.results.lock().unwrap().push((p, false));
                    found.fetch_add(1, AO::Relaxed);
                    ctx.request_repaint();
                }
            }));
        }
        // montajes de red o FUSE dentro de lo pedido: leer cada archivo por la red sería eterno
        let skip = crate::index::skipped_mounts();
        // un espejo de un servidor que ya no está conectado sólo tiene marcadores sin contenido
        let mut stack: Vec<PathBuf> = roots.into_iter().filter(|r| remote_ok(r)).collect();
        'walk: while let Some(dir) = stack.pop() {
            if s.cancel.load(AO::Relaxed) {
                break;
            }
            // carpeta de un servidor SFTP: se refleja antes de leerla (si falla, lo que haya en la caché)
            if crate::sftp::is_remote(&dir) {
                let _ = crate::sftp::sync_dir(&dir);
            }
            let Ok(rd) = fs::read_dir(&dir) else { continue };
            for e in rd.flatten() {
                if s.cancel.load(AO::Relaxed) || found.load(AO::Relaxed) >= MAX_RESULTS {
                    break 'walk;
                }
                if !hidden && crate::fsys::is_hidden_entry(&e) {
                    continue;
                }
                let Ok(ft) = e.file_type() else { continue };
                if ft.v_is_dir() {
                    let p = e.path();
                    if !pseudo_fs(&p) && !skip.contains(&p) && remote_ok(&p) {
                        stack.push(p);
                    }
                } else if ft.v_is_file() {
                    if crate::vfs::WEB {
                        // cada archivo es una petición al servidor: tope para no dejar la página colgada
                        if s.scanned.fetch_add(1, AO::Relaxed) >= WEB_MAX_FILES {
                            break 'walk;
                        }
                        let p = e.path();
                        if file_matches(&p, &needle, &s.cancel, &remote) {
                            s.results.lock().unwrap().push((p, false));
                            found.fetch_add(1, AO::Relaxed);
                        }
                    } else if tx.send(e.path()).is_err() {
                        break 'walk;
                    }
                }
            }
        }
        drop(tx);
        for w in workers {
            let _ = w.join();
        }
        let skipped = remote.skipped.load(AO::Relaxed);
        if skipped > 0 && !s.cancel.load(AO::Relaxed) {
            let mut g = NOTES.lock().unwrap_or_else(|e| e.into_inner());
            g.retain(|(_, w, _)| w.strong_count() > 0);
            g.push((Arc::as_ptr(&s) as usize, Arc::downgrade(&s), skipped));
        }
        s.done.store(true, AO::Release);
        ctx.request_repaint();
    });
    st
}

/// ¿Se puede recorrer? Lo de un servidor SFTP sólo si sigue conectado.
fn remote_ok(p: &Path) -> bool {
    !crate::sftp::is_remote(p) || crate::sftp::is_live(p)
}

/// ¿Tiene este archivo del servidor algo de texto que buscar? (por la extensión, sin descargarlo)
fn remote_worth(p: &Path) -> bool {
    let ext = p.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
    match crate::fsys::kind_of_ext(&ext) {
        crate::fsys::Kind::Video | crate::fsys::Kind::Audio | crate::fsys::Kind::Archive => false,
        crate::fsys::Kind::Image => ext == "svg",
        _ => true,
    }
}

/// Descarga (si hace falta) un archivo del servidor para examinarlo, dentro de los topes.
fn fetch_remote(p: &Path, len: u64, cancel: &AtomicBool, remote: &Remote) -> bool {
    if crate::sftp::tree_fetched(p) {
        return true;
    }
    if len == 0 || !remote_worth(p) {
        return false;
    }
    if len > REMOTE_FILE_MAX {
        remote.skipped.fetch_add(1, AO::Relaxed);
        return false;
    }
    // se reserva antes de descargar: varios hilos comparten el tope
    if remote.used.fetch_add(len, AO::Relaxed).saturating_add(len) > REMOTE_TOTAL_MAX {
        remote.used.fetch_sub(len, AO::Relaxed);
        remote.skipped.fetch_add(1, AO::Relaxed);
        return false;
    }
    crate::sftp::ensure_local_with(p, cancel, &mut |_| {}).is_ok() && !cancel.load(AO::Relaxed)
}

fn file_matches(p: &Path, needle: &str, cancel: &AtomicBool, remote: &Remote) -> bool {
    let Ok(md) = fs::metadata(p) else { return false };
    if crate::sftp::is_remote(p) && !fetch_remote(p, md.len(), cancel, remote) {
        return false;
    }
    if crate::docview::is_doc_path(p) {
        return md.len() <= DOC_MAX && doc_contains(p, needle, cancel);
    }
    if md.len() == 0 || (crate::vfs::WEB && md.len() > TEXT_MAX / 8) {
        return false;
    }
    let Ok(f) = fs::File::open(p) else { return false };
    let mut f = f.take(TEXT_MAX);
    // primero el principio: un binario (bytes nulos) se descarta sin leer el resto
    let mut buf = vec![0u8; 4096];
    let mut got = 0;
    while got < buf.len() {
        match f.read(&mut buf[got..]) {
            Ok(0) => break,
            Ok(n) => got += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => return false,
        }
    }
    buf.truncate(got);
    if buf.contains(&0) || cancel.load(AO::Relaxed) {
        return false;
    }
    if f.read_to_end(&mut buf).is_err() {
        return false;
    }
    crate::tools::name_matches(&String::from_utf8_lossy(&buf), needle)
}

fn doc_contains(p: &Path, needle: &str, cancel: &AtomicBool) -> bool {
    let Ok(mut doc) = crate::docview::open_doc(p) else { return false };
    for page in 0..doc.page_count().min(DOC_PAGES) {
        if cancel.load(AO::Relaxed) {
            return false;
        }
        let Ok(runs) = doc.page_runs(page) else { continue };
        let text = runs.iter().map(|r| r.text.as_str()).collect::<Vec<_>>().join(" ");
        if crate::tools::name_matches(&text, needle) {
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
        let d = crate::vfs::temp_dir().join(format!("fb_cs_{}", crate::vfs::pid()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(d.join("sub")).unwrap();
        fs::write(d.join("a.txt"), "Hola Mundo cruel").unwrap();
        fs::write(d.join("sub/b.rs"), "fn main() { /* mundo */ }").unwrap();
        fs::write(d.join("c.bin"), b"\0\0mundo\0").unwrap();
        fs::write(d.join("d.txt"), "nada").unwrap();
        let st = spawn(vec![d.clone()], "MUNDO".into(), false, egui::Context::default());
        while !st.done.load(AO::Acquire) {
            crate::vfs::thread::sleep(std::time::Duration::from_millis(5));
        }
        let mut names: Vec<String> = st.results.lock().unwrap().iter().map(|(p, _)| p.file_name().unwrap().to_string_lossy().into_owned()).collect();
        names.sort();
        assert_eq!(names, ["a.txt", "b.rs"]);
        let _ = fs::remove_dir_all(&d);
    }
}
