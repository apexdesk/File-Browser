//! Herramientas auxiliares: zip/unzip, estadísticas de carpeta, búsqueda recursiva, portapapeles de archivos.
#[allow(unused_imports)]
use crate::vfs::{KindV as _, PathV as _};
use crate::fsys::Progress;
use crate::vfs::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering as AO};
use std::sync::{Arc, Mutex};

// ---------------------------------------------------------------------------
// ZIP
// ---------------------------------------------------------------------------

/// Formato de salida al comprimir.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ArchFmt {
    Zip,
    TarGz,
    SevenZ,
}

impl ArchFmt {
    pub fn ext(self) -> &'static str {
        match self {
            ArchFmt::Zip => "zip",
            ArchFmt::TarGz => "tar.gz",
            ArchFmt::SevenZ => "7z",
        }
    }

    /// ¿Admite contraseña? (tar.gz no tiene cifrado)
    pub fn can_encrypt(self) -> bool {
        self != ArchFmt::TarGz
    }
}

/// Opciones de compresión: nivel 0 (solo almacenar) a 9; la contraseña cifra con AES-256.
#[derive(Clone)]
pub struct CompressOpts {
    pub fmt: ArchFmt,
    pub level: u32,
    pub password: Option<String>,
}

pub fn compress_paths(srcs: &[PathBuf], out: &Path, o: &CompressOpts, p: &Progress, ctx: &egui::Context) -> Result<(), String> {
    match o.fmt {
        ArchFmt::Zip => zip_paths(srcs, out, o, p, ctx),
        ArchFmt::TarGz => targz_paths(srcs, out, o, p, ctx),
        ArchFmt::SevenZ => sevenz_paths(srcs, out, o, p, ctx),
    }
}

/// Lector que cuenta los bytes leídos (progreso) y corta si se cancela la operación.
struct ProgRead<'a, R> {
    inner: R,
    p: &'a Progress,
}

impl<R: Read> Read for ProgRead<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.p.cancelled() {
            return Err(io::Error::other(crate::i18n::t("cancelado")));
        }
        let n = self.inner.read(buf)?;
        self.p.done.fetch_add(n as u64, AO::Relaxed);
        Ok(n)
    }
}

/// Umbral a partir del cual una entrada ZIP se escribe como ZIP64 (sin él, la biblioteca aborta
/// con «Large file option has not been set»). Con margen bajo los 4 GiB: lo comprimido y el
/// cifrado pueden ocupar algo más que el original.
const ZIP64_AT: u64 = 0xF000_0000;

/// Opciones de una entrada ZIP según el tamaño del archivo.
fn zip_entry_opts<'k, 'n>(base: zip::write::FileOptions<'k, 'n, ()>, len: u64) -> zip::write::FileOptions<'k, 'n, ()> {
    base.large_file(len >= ZIP64_AT)
}

/// Lo que no cabe en el comprimido (archivos especiales; enlaces en 7z) no se calla: se anota en
/// los errores de la operación con su motivo.
fn note_skipped(p: &Progress, name: &str, what: &str) {
    let mut v = p.errors.lock().unwrap();
    if v.len() < 500 {
        v.push(crate::i18n::tf("{}: omitido ({})", &[&name, &what]));
    }
}

/// Nombre de un hijo dentro del comprimido (zip y 7z sólo admiten texto: un nombre que no es
/// UTF-8 se guarda con el carácter de sustitución; el archivo se lee siempre por su ruta real).
fn child_name(parent: &str, e: &fs::DirEntry) -> String {
    format!("{parent}/{}", e.file_name().to_string_lossy())
}

fn zip_paths(srcs: &[PathBuf], out: &Path, o: &CompressOpts, p: &Progress, ctx: &egui::Context) -> Result<(), String> {
    let f = File::create(out).map_err(|e| e.to_string())?;
    let mut zw = zip::ZipWriter::new(io::BufWriter::new(f));
    let mut opts = zip::write::SimpleFileOptions::default();
    opts = if o.level == 0 {
        opts.compression_method(zip::CompressionMethod::Stored)
    } else {
        opts.compression_method(zip::CompressionMethod::Deflated).compression_level(Some(o.level as i64))
    };
    let opts = match &o.password {
        Some(pw) => opts.with_aes_encryption(zip::AesMode::Aes256, pw),
        None => opts,
    };
    let mut buf = vec![0u8; 256 * 1024];
    for s in srcs {
        let Some(name) = s.file_name().map(|n| n.to_string_lossy().into_owned()) else { continue };
        add_tree(&mut zw, s, &name, opts, p, ctx, &mut buf)?;
    }
    let w = zw.finish().map_err(|e| e.to_string())?;
    // el último trozo (y en la web, el envío entero) puede fallar aquí
    w.into_inner().map_err(|e| e.to_string())?.flush().map_err(|e| e.to_string())?;
    Ok(())
}

fn targz_paths(srcs: &[PathBuf], out: &Path, o: &CompressOpts, p: &Progress, ctx: &egui::Context) -> Result<(), String> {
    let f = File::create(out).map_err(|e| e.to_string())?;
    let gz = flate2::write::GzEncoder::new(io::BufWriter::new(f), flate2::Compression::new(o.level.min(9)));
    let mut b = tar::Builder::new(gz);
    // los enlaces se guardan como enlaces, nunca se sigue lo que apuntan
    b.follow_symlinks(false);
    for s in srcs {
        let Some(name) = s.file_name() else { continue };
        tar_tree(&mut b, s, Path::new(name), p, ctx)?;
    }
    let gz = b.into_inner().map_err(|e| e.to_string())?;
    let w = gz.finish().map_err(|e| e.to_string())?;
    w.into_inner().map_err(|e| e.to_string())?.flush().map_err(|e| e.to_string())?;
    Ok(())
}

/// Cabecera tar con los datos de `md` (en la web no hay `std::fs::Metadata`).
fn tar_header(md: &fs::Metadata) -> tar::Header {
    let mut h = tar::Header::new_gnu();
    #[cfg(not(target_arch = "wasm32"))]
    h.set_metadata(md);
    #[cfg(target_arch = "wasm32")]
    {
        h.set_size(md.len());
        h.set_mode(0o644);
        h.set_mtime(md.modified().ok().and_then(|t| t.duration_since(web_time::UNIX_EPOCH).ok()).map_or(0, |d| d.as_secs()));
    }
    h
}

/// `name` es una ruta (no texto): tar guarda los nombres byte a byte, también los que no son UTF-8.
fn tar_tree<W: Write>(b: &mut tar::Builder<W>, path: &Path, name: &Path, p: &Progress, ctx: &egui::Context) -> Result<(), String> {
    let shown = || name.to_string_lossy().into_owned();
    if p.cancelled() {
        return Err(crate::i18n::t("cancelado").into());
    }
    let md = fs::symlink_metadata(path).map_err(|e| format!("{}: {e}", shown()))?;
    if md.v_is_dir() {
        b.append_dir(name, path).map_err(|e| format!("{}: {e}", shown()))?;
        p.done.fetch_add(1, AO::Relaxed);
        let rd = fs::read_dir(path).map_err(|e| format!("{}: {e}", shown()))?;
        for e in rd.flatten() {
            tar_tree(b, &e.path(), &name.join(e.file_name()), p, ctx)?;
        }
    } else if md.v_is_symlink() {
        let target = fs::read_link(path).map_err(|e| format!("{}: {e}", shown()))?;
        let mut h = tar_header(&md);
        h.set_entry_type(tar::EntryType::Symlink);
        h.set_size(0);
        b.append_link(&mut h, name, &target).map_err(|e| format!("{}: {e}", shown()))?;
        p.done.fetch_add(1, AO::Relaxed);
    } else if md.v_is_file() {
        let mut h = tar_header(&md);
        // archivo de un servidor SFTP: sin descargar sería un marcador vacío
        crate::sftp::ensure_local_with(path, &p.cancel, &mut |_| {}).map_err(|e| format!("{}: {e}", shown()))?;
        let f = File::open(path).map_err(|e| format!("{}: {e}", shown()))?;
        b.append_data(&mut h, name, ProgRead { inner: f, p }).map_err(|e| format!("{}: {e}", shown()))?;
        p.done.fetch_add(1, AO::Relaxed);
        ctx.request_repaint();
    } else {
        // FIFO, socket, dispositivo: abrirlo bloquearía o no tiene sentido guardarlo
        note_skipped(p, &shown(), crate::i18n::t("archivo especial"));
        p.done.fetch_add(1, AO::Relaxed);
    }
    Ok(())
}

fn sevenz_paths(srcs: &[PathBuf], out: &Path, o: &CompressOpts, p: &Progress, ctx: &egui::Context) -> Result<(), String> {
    use sevenz_rust2::{encoder_options::{AesEncoderOptions, Lzma2Options}, ArchiveWriter, EncoderConfiguration, EncoderMethod, Password};
    let f = File::create(out).map_err(|e| e.to_string())?;
    let mut w = ArchiveWriter::new(io::BufWriter::new(f)).map_err(|e| e.to_string())?;
    let main: EncoderConfiguration = if o.level == 0 { EncoderMethod::COPY.into() } else { Lzma2Options::from_level(o.level.min(9)).into() };
    let mut methods = Vec::new();
    if let Some(pw) = &o.password {
        methods.push(AesEncoderOptions::new(Password::from(pw.as_str())).into());
    }
    methods.push(main);
    w.set_content_methods(methods);
    for s in srcs {
        let Some(name) = s.file_name().map(|n| n.to_string_lossy().into_owned()) else { continue };
        sz_tree(&mut w, s, &name, p, ctx)?;
    }
    let mut bw = w.finish().map_err(|e| e.to_string())?;
    bw.flush().map_err(|e| e.to_string())?;
    Ok(())
}

fn sz_tree<W: Write + io::Seek>(w: &mut sevenz_rust2::ArchiveWriter<W>, path: &Path, name: &str, p: &Progress, ctx: &egui::Context) -> Result<(), String> {
    use sevenz_rust2::ArchiveEntry;
    if p.cancelled() {
        return Err(crate::i18n::t("cancelado").into());
    }
    let md = fs::symlink_metadata(path).map_err(|e| format!("{name}: {e}"))?;
    if md.v_is_dir() {
        w.push_archive_entry(ArchiveEntry::new_directory(name), None::<File>).map_err(|e| format!("{name}: {e}"))?;
        p.done.fetch_add(1, AO::Relaxed);
        let rd = fs::read_dir(path).map_err(|e| format!("{name}: {e}"))?;
        for e in rd.flatten() {
            sz_tree(w, &e.path(), &child_name(name, &e), p, ctx)?;
        }
    } else if md.v_is_symlink() {
        // 7z no guarda enlaces simbólicos de forma portable: se avisa en vez de seguirlo
        note_skipped(p, name, crate::i18n::t("Enlace simbólico"));
        p.done.fetch_add(1, AO::Relaxed);
    } else if md.v_is_file() {
        // archivo de un servidor SFTP: sin descargar sería un marcador vacío
        crate::sftp::ensure_local_with(path, &p.cancel, &mut |_| {}).map_err(|e| format!("{name}: {e}"))?;
        let f = File::open(path).map_err(|e| format!("{name}: {e}"))?;
        w.push_archive_entry(ArchiveEntry::from_path(path, name.to_string()), Some(ProgRead { inner: f, p })).map_err(|e| format!("{name}: {e}"))?;
        p.done.fetch_add(1, AO::Relaxed);
        ctx.request_repaint();
    } else {
        note_skipped(p, name, crate::i18n::t("archivo especial"));
        p.done.fetch_add(1, AO::Relaxed);
    }
    Ok(())
}

fn add_tree<W: Write + io::Seek>(
    zw: &mut zip::ZipWriter<W>,
    path: &Path,
    name: &str,
    opts: zip::write::FileOptions<'_, '_, ()>,
    p: &Progress,
    ctx: &egui::Context,
    buf: &mut [u8],
) -> Result<(), String> {
    if p.cancelled() {
        return Err(crate::i18n::t("cancelado").into());
    }
    let md = fs::symlink_metadata(path).map_err(|e| format!("{name}: {e}"))?;
    if md.v_is_dir() {
        zw.add_directory(format!("{name}/"), opts).map_err(|e| e.to_string())?;
        p.done.fetch_add(1, AO::Relaxed);
        let rd = fs::read_dir(path).map_err(|e| format!("{name}: {e}"))?;
        for e in rd.flatten() {
            add_tree(zw, &e.path(), &child_name(name, &e), opts, p, ctx, buf)?;
        }
    } else if md.v_is_symlink() {
        // se guarda el enlace (modo S_IFLNK + destino), no lo que apunta
        let target = fs::read_link(path).map_err(|e| format!("{name}: {e}"))?;
        zw.add_symlink(name, target.to_string_lossy().replace('\\', "/"), opts).map_err(|e| format!("{name}: {e}"))?;
        p.done.fetch_add(1, AO::Relaxed);
    } else if md.v_is_file() {
        zw.start_file(name, zip_entry_opts(opts, md.len())).map_err(|e| e.to_string())?;
        // archivo de un servidor SFTP: sin descargar sería un marcador vacío
        crate::sftp::ensure_local_with(path, &p.cancel, &mut |_| {}).map_err(|e| format!("{name}: {e}"))?;
        let mut f = File::open(path).map_err(|e| format!("{name}: {e}"))?;
        loop {
            let n = f.read(buf).map_err(|e| format!("{name}: {e}"))?;
            if n == 0 {
                break;
            }
            zw.write_all(&buf[..n]).map_err(|e| e.to_string())?;
            p.done.fetch_add(n as u64, AO::Relaxed);
        }
        p.done.fetch_add(1, AO::Relaxed);
        ctx.request_repaint();
    } else {
        note_skipped(p, name, crate::i18n::t("archivo especial"));
        p.done.fetch_add(1, AO::Relaxed);
    }
    Ok(())
}

/// Extrae `src` dentro de `dst` (carpeta ya creada). Devuelve los nombres de primer nivel.
pub fn unzip_to(src: &Path, dst: &Path, p: &Progress, ctx: &egui::Context, pass: Option<&str>) -> Result<Vec<String>, String> {
    let f = File::open(src).map_err(|e| e.to_string())?;
    let mut ar = zip::ZipArchive::new(io::BufReader::new(f)).map_err(|e| e.to_string())?;
    p.total.store(ar.len() as u64, AO::Relaxed);
    fs::create_dir_all(dst).map_err(|e| e.to_string())?;
    let root = fs::canonicalize(dst).map_err(|e| e.to_string())?;
    let mut top: Vec<String> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    // última carpeta comprobada (las entradas suelen venir agrupadas por carpeta)
    let mut checked: Option<PathBuf> = None;
    let mut written = 0u64;
    for i in 0..ar.len() {
        if p.cancelled() {
            return Err(crate::i18n::t("cancelado").into());
        }
        let bad = |e: String| -> String {
            let m = e.to_lowercase();
            if m.contains("password required") {
                if pass.is_some() { crate::archive::BAD_PASS.into() } else { crate::archive::NEED_PASS.into() }
            } else if m.contains("password") && m.contains("incorrect") {
                crate::archive::BAD_PASS.into()
            } else {
                e
            }
        };
        let mut zf = match pass {
            Some(pw) => ar.by_index_decrypt(i, pw.as_bytes()),
            None => ar.by_index(i),
        }
        .map_err(|e| bad(e.to_string()))?;
        let encrypted = zf.encrypted();
        let Some(rel) = zf.enclosed_name() else { continue }; // descarta rutas peligrosas (../)
        if let Some(first) = rel.components().next() {
            let n = first.as_os_str().to_string_lossy().into_owned();
            if seen.insert(n.clone()) {
                top.push(n);
            }
        }
        let shown = rel.display().to_string();
        let target = dst.join(&rel);
        if zf.is_dir() {
            fs::create_dir_all(&target).map_err(|e| e.to_string())?;
        } else {
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent).map_err(|e| e.to_string())?;
                // un enlace de una entrada anterior no puede llevar la escritura fuera de `dst`
                if checked.as_deref() != Some(parent) {
                    if !crate::archive::inside(&root, parent) {
                        crate::archive::note_outside(p, &shown);
                        continue;
                    }
                    checked = Some(parent.to_path_buf());
                }
            }
            // nombre repetido sobre un enlace ya extraído: se sustituye el enlace, no se sigue
            if fs::symlink_metadata(&target).is_ok_and(|m| m.v_is_symlink()) {
                let _ = fs::remove_file(&target);
            }
            if zf.is_symlink() {
                let mut t = String::new();
                if let Err(e) = (&mut zf).take(4096).read_to_string(&mut t) {
                    return Err(if encrypted && pass.is_some() { crate::archive::BAD_PASS.into() } else { format!("{shown}: {e}") });
                }
                #[cfg(unix)]
                {
                    let par = target.parent().and_then(|q| fs::canonicalize(q).ok()).unwrap_or_default();
                    if crate::archive::link_ok(&root, &par, Path::new(&t)) {
                        std::os::unix::fs::symlink(&t, &target).map_err(|e| format!("{shown}: {e}"))?;
                    } else {
                        crate::archive::note_outside(p, &shown);
                    }
                }
                #[cfg(not(unix))]
                note_skipped(p, &shown, crate::i18n::t("Enlace simbólico"));
            } else {
                let mut out = File::create(&target).map_err(|e| format!("{shown}: {e}"))?;
                // el tamaño declarado puede mentir: se cuenta lo que realmente se escribe
                let room = crate::archive::MAX_EXPANDED.saturating_sub(written);
                let r = io::copy(&mut (&mut zf).take(room.saturating_add(1)), &mut out).and_then(|n| out.flush().map(|()| n));
                if let Ok(n) = &r {
                    written = written.saturating_add(*n);
                    if written > crate::archive::MAX_EXPANDED {
                        return Err(crate::i18n::t("el contenido descomprimido supera el límite permitido").into());
                    }
                }
                if let Err(e) = r {
                    // ZipCrypto con contraseña errónea sólo falla al comprobar el CRC
                    return Err(if encrypted && pass.is_some() { crate::archive::BAD_PASS.into() } else { format!("{shown}: {e}") });
                }
            }
        }
        p.done.fetch_add(1, AO::Relaxed);
        if i % 32 == 0 {
            ctx.request_repaint();
        }
    }
    Ok(top)
}

// ---------------------------------------------------------------------------
// Estadísticas de carpeta (propiedades)
// ---------------------------------------------------------------------------

#[derive(Default)]
pub struct Stats {
    pub bytes: AtomicU64,
    pub files: AtomicU64,
    pub dirs: AtomicU64,
    pub done: AtomicBool,
    pub cancel: AtomicBool,
}

pub fn spawn_stats(paths: Vec<PathBuf>, ctx: egui::Context) -> Arc<Stats> {
    let st = Arc::new(Stats::default());
    let s = st.clone();
    crate::vfs::thread::spawn(move || {
        let mut last = web_time::Instant::now();
        let mut stack = paths;
        while let Some(p) = stack.pop() {
            if s.cancel.load(AO::Relaxed) {
                return;
            }
            let Ok(md) = fs::symlink_metadata(&p) else { continue };
            if md.v_is_dir() {
                s.dirs.fetch_add(1, AO::Relaxed);
                if let Ok(rd) = fs::read_dir(&p) {
                    stack.extend(rd.flatten().map(|e| e.path()));
                }
            } else {
                s.files.fetch_add(1, AO::Relaxed);
                s.bytes.fetch_add(md.len(), AO::Relaxed);
            }
            if last.elapsed().as_millis() > 120 {
                last = web_time::Instant::now();
                ctx.request_repaint();
            }
        }
        s.done.store(true, AO::Release);
        ctx.request_repaint();
    });
    st
}

// ---------------------------------------------------------------------------
// Búsqueda recursiva por nombre
// ---------------------------------------------------------------------------

pub const MAX_RESULTS: usize = 5000;

#[derive(Default)]
pub struct Search {
    pub results: Mutex<Vec<(PathBuf, bool)>>,
    pub scanned: AtomicU64,
    pub done: AtomicBool,
    pub cancel: AtomicBool,
    /// Resultados ya leídos (`stat`) en un hilo aparte, listos para la lista (ver `stat_pending`).
    pub entries: Mutex<Vec<crate::fsys::Entry>>,
    /// Cuántos de `results` ha leído ya `stat_pending`.
    pub statted: std::sync::atomic::AtomicUsize,
    /// Hay un lote leyéndose ahora mismo.
    pub statting: AtomicBool,
}

/// Lee (`stat`) el siguiente lote de resultados de `st` en otro hilo y deja las entradas en
/// `st.entries`: en una carpeta de red cada `stat` puede tardar y no debe parar la interfaz.
/// Un solo lote a la vez; al terminar pide un fotograma, en el que se vuelve a llamar.
pub fn stat_pending(st: &Arc<Search>, max: usize, ctx: &egui::Context) {
    if st.statting.swap(true, AO::AcqRel) {
        return;
    }
    let from = st.statted.load(AO::Acquire);
    let batch: Vec<PathBuf> = {
        let res = crate::thumbs::lock(&st.results);
        let end = res.len().min(from.saturating_add(max.max(1)));
        res.get(from..end).map(|v| v.iter().map(|(p, _)| p.clone()).collect()).unwrap_or_default()
    };
    if batch.is_empty() {
        st.statting.store(false, AO::Release);
        return;
    }
    let (s, ctx) = (st.clone(), ctx.clone());
    crate::vfs::thread::spawn(move || {
        let mut ents = Vec::with_capacity(batch.len());
        for p in &batch {
            if s.cancel.load(AO::Relaxed) {
                break;
            }
            if let Some(e) = crate::fsys::entry_from_path(p, p.to_string_lossy().into_owned()) {
                ents.push(e);
            }
        }
        crate::thumbs::lock(&s.entries).extend(ents);
        s.statted.store(from + batch.len(), AO::Release);
        s.statting.store(false, AO::Release);
        ctx.request_repaint();
    });
}

/// ¿Ha terminado la búsqueda y se han leído todos sus resultados? (puede quedar algo en `entries`)
pub fn stat_finished(st: &Search) -> bool {
    st.done.load(AO::Acquire) && !st.statting.load(AO::Acquire) && st.statted.load(AO::Acquire) >= crate::thumbs::lock(&st.results).len()
}

/// Trabajo de E/S en segundo plano con avance (elementos hechos) y cancelación.
#[derive(Default)]
pub struct Work {
    pub done: AtomicU64,
    pub cancel: AtomicBool,
}

impl Work {
    /// Cuenta un elemento; error «cancelado» si se ha pedido parar.
    pub fn step(&self) -> io::Result<()> {
        if self.cancel.load(AO::Relaxed) {
            return Err(io::Error::new(io::ErrorKind::Interrupted, crate::i18n::t("cancelado")));
        }
        self.done.fetch_add(1, AO::Relaxed);
        Ok(())
    }
}

/// Raíces para «todo el equipo».
pub fn all_roots() -> Vec<PathBuf> {
    #[cfg(windows)]
    {
        (b'A'..=b'Z').map(|c| PathBuf::from(format!("{}:\\", c as char))).filter(|p| p.exists()).collect()
    }
    #[cfg(all(not(windows), not(target_os = "android")))]
    {
        vec![PathBuf::from("/")]
    }
    // Android: sólo el almacenamiento compartido y las tarjetas SD (el resto no es legible ni útil)
    #[cfg(target_os = "android")]
    {
        let mut v = vec![PathBuf::from("/storage/emulated/0")];
        if let Ok(rd) = crate::vfs::fs::read_dir("/storage") {
            for e in rd.flatten() {
                let n = e.file_name().to_string_lossy().into_owned();
                if n != "emulated" && n != "self" && e.path().v_is_dir() {
                    v.push(e.path());
                }
            }
        }
        v
    }
}

pub fn pseudo_fs(p: &Path) -> bool {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        // de /run sólo interesan los discos extraíbles (/run/media/<usuario> en Fedora, Arch…)
        matches!(p.to_str(), Some("/proc" | "/sys" | "/dev" | "/snap")) || (p.parent() == Some(Path::new("/run")) && !p.ends_with("media"))
    }
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    {
        let _ = p;
        false
    }
}

pub fn spawn_search(roots: Vec<PathBuf>, needle: String, hidden: bool, ctx: egui::Context) -> Arc<Search> {
    let st = Arc::new(Search::default());
    let s = st.clone();
    crate::vfs::thread::spawn(move || {
        if live_search(&s, roots, &fold(&needle), hidden, &ctx) {
            s.done.store(true, AO::Release);
            ctx.request_repaint();
        }
    });
    st
}

/// Recorre `roots` buscando `needle` (ya pasado por `fold`) y deja lo hallado en `s`.
/// Devuelve `false` si se canceló (sin marcar `done`).
pub(crate) fn live_search(s: &Search, roots: Vec<PathBuf>, needle: &str, hidden: bool, ctx: &egui::Context) -> bool {
    let mut stack = roots;
    let mut last = web_time::Instant::now();
    let mut found = 0usize;
    'outer: while let Some(dir) = stack.pop() {
        let Ok(rd) = fs::read_dir(&dir) else { continue };
        for e in rd.flatten() {
            if s.cancel.load(AO::Relaxed) {
                return false;
            }
            let name = e.file_name();
            let name = name.to_string_lossy();
            if !hidden && crate::fsys::is_hidden_entry(&e) {
                continue;
            }
            s.scanned.fetch_add(1, AO::Relaxed);
            let is_dir = e.file_type().map(|t| t.v_is_dir()).unwrap_or(false);
            if name_matches(&name, needle) {
                s.results.lock().unwrap().push((e.path(), is_dir));
                found += 1;
                if found >= MAX_RESULTS {
                    break 'outer;
                }
            }
            if is_dir && !pseudo_fs(&e.path()) {
                stack.push(e.path());
            }
            if last.elapsed().as_millis() > 150 {
                last = web_time::Instant::now();
                ctx.request_repaint();
            }
        }
    }
    true
}

/// Minúsculas y sin tildes ni diacríticos latinos, en forma compuesta o descompuesta (macOS
/// guarda los nombres en NFD): «Canción», «cancion» y «cancio\u{301}n» se comparan igual.
pub fn fold(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars().flat_map(char::to_lowercase) {
        let b = match c {
            // marcas combinantes (la tilde de una «ó» descompuesta)
            '\u{300}'..='\u{36f}' => continue,
            'à'..='å' | 'ā' | 'ă' | 'ą' => 'a',
            'ç' | 'ć' | 'ĉ' | 'ċ' | 'č' => 'c',
            'ď' | 'đ' => 'd',
            'è'..='ë' | 'ē' | 'ĕ' | 'ė' | 'ę' | 'ě' => 'e',
            'ĝ' | 'ğ' | 'ġ' | 'ģ' => 'g',
            'ĥ' | 'ħ' => 'h',
            'ì'..='ï' | 'ĩ' | 'ī' | 'ĭ' | 'į' | 'ı' => 'i',
            'ĵ' => 'j',
            'ķ' => 'k',
            'ĺ' | 'ļ' | 'ľ' | 'ŀ' | 'ł' => 'l',
            'ñ' | 'ń' | 'ņ' | 'ň' => 'n',
            'ò'..='ö' | 'ø' | 'ō' | 'ŏ' | 'ő' => 'o',
            'ŕ' | 'ŗ' | 'ř' => 'r',
            'ś' | 'ŝ' | 'ş' | 'š' | 'ș' => 's',
            'ţ' | 'ť' | 'ŧ' | 'ț' => 't',
            'ù'..='ü' | 'ũ' | 'ū' | 'ŭ' | 'ů' | 'ű' | 'ų' => 'u',
            'ŵ' => 'w',
            'ý' | 'ÿ' | 'ŷ' => 'y',
            'ź' | 'ż' | 'ž' => 'z',
            c => c,
        };
        out.push(b);
    }
    out
}

/// ¿Contiene el nombre `hay` el texto `needle` (ya pasado por `fold`)? Rápido si es ASCII.
pub fn name_matches(hay: &str, needle: &str) -> bool {
    if hay.is_ascii() {
        crate::fsys::contains_ci(hay, needle)
    } else {
        fold(hay).contains(needle)
    }
}

// ---------------------------------------------------------------------------
// Portapapeles de archivos del sistema (Linux: wl-copy/xclip; Windows: CF_HDROP; macOS: NSPasteboard)
// ---------------------------------------------------------------------------

#[cfg(target_os = "linux")]
mod clip_impl {
    use super::*;
    use std::process::{Command, Stdio};
    use std::time::Duration;

    const MIME: &str = "x-special/gnome-copied-files";

    fn have(bin: &str) -> bool {
        std::env::var_os("PATH")
            .map(|p| crate::vfs::split_paths(&p).any(|d| d.join(bin).v_is_file()))
            .unwrap_or(false)
    }

    fn wayland() -> bool {
        std::env::var_os("WAYLAND_DISPLAY").map(|v| !v.is_empty()).unwrap_or(false)
    }

    fn encode(p: &Path) -> String {
        use std::os::unix::ffi::OsStrExt;
        let mut s = String::from("file://");
        for &b in p.as_os_str().as_bytes() {
            if b.is_ascii_alphanumeric() || b"/-_.~".contains(&b) {
                s.push(b as char);
            } else {
                s.push_str(&format!("%{b:02X}"));
            }
        }
        s
    }

    fn decode(uri: &str) -> Option<PathBuf> {
        use std::os::unix::ffi::OsStringExt;
        let rest = uri.strip_prefix("file://")?;
        let rest = rest.strip_prefix("localhost").unwrap_or(rest);
        let b = rest.as_bytes();
        let mut out = Vec::with_capacity(b.len());
        let mut i = 0;
        while i < b.len() {
            if b[i] == b'%' && i + 2 < b.len() {
                if let Ok(v) = u8::from_str_radix(if b[i + 1..i + 3].iter().all(u8::is_ascii_hexdigit) { std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or("zz") } else { "zz" }, 16) {
                    out.push(v);
                    i += 3;
                    continue;
                }
            }
            out.push(b[i]);
            i += 1;
        }
        Some(PathBuf::from(std::ffi::OsString::from_vec(out)))
    }

    pub fn set_files(paths: &[PathBuf], cut: bool) -> bool {
        let tool: (&str, Vec<&str>) = if wayland() && have("wl-copy") {
            ("wl-copy", vec!["--type", MIME])
        } else if have("xclip") {
            ("xclip", vec!["-selection", "clipboard", "-t", MIME, "-i"])
        } else {
            return false;
        };
        let mut payload = String::from(if cut { "cut" } else { "copy" });
        for p in paths {
            payload.push('\n');
            payload.push_str(&encode(p));
        }
        let Ok(mut child) = Command::new(tool.0)
            .args(&tool.1)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        else {
            return false;
        };
        if let Some(mut si) = child.stdin.take() {
            let _ = si.write_all(payload.as_bytes());
        }
        // wl-copy/xclip se quedan en segundo plano sirviendo el portapapeles
        crate::vfs::thread::spawn(move || {
            let _ = child.wait();
        });
        true
    }

    fn capture(bin: &str, args: &[&str]) -> Option<String> {
        let mut child = Command::new(bin)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        let mut out = child.stdout.take()?;
        let (tx, rx) = std::sync::mpsc::channel();
        crate::vfs::thread::spawn(move || {
            let mut s = String::new();
            let _ = out.read_to_string(&mut s);
            let _ = tx.send(s);
        });
        match rx.recv_timeout(Duration::from_millis(600)) {
            Ok(s) => {
                let _ = child.wait();
                Some(s)
            }
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                None
            }
        }
    }

    pub fn get_files() -> Option<(Vec<PathBuf>, bool)> {
        let txt = if wayland() && have("wl-paste") {
            capture("wl-paste", &["--no-newline", "--type", MIME])
                .filter(|s| !s.trim().is_empty())
                .or_else(|| capture("wl-paste", &["--no-newline", "--type", "text/uri-list"]))
        } else if have("xclip") {
            capture("xclip", &["-selection", "clipboard", "-o", "-t", MIME])
                .filter(|s| !s.trim().is_empty())
                .or_else(|| capture("xclip", &["-selection", "clipboard", "-o", "-t", "text/uri-list"]))
        } else {
            None
        }?;
        let mut cut = false;
        let mut v = Vec::new();
        for l in txt.lines() {
            let l = l.trim();
            match l {
                "cut" => cut = true,
                "copy" | "" => {}
                _ => {
                    if let Some(p) = decode(l) {
                        if fs::symlink_metadata(&p).is_ok() {
                            v.push(p);
                        }
                    }
                }
            }
        }
        if v.is_empty() { None } else { Some((v, cut)) }
    }
}

/// Windows: CF_HDROP + «Preferred DropEffect» (1 copiar, 2 mover), como el Explorador.
#[cfg(windows)]
mod clip_impl {
    use super::*;
    use std::ffi::c_void;
    use std::os::windows::ffi::{OsStrExt, OsStringExt};

    #[link(name = "user32")]
    unsafe extern "system" {
        fn OpenClipboard(hwnd: isize) -> i32;
        fn CloseClipboard() -> i32;
        fn EmptyClipboard() -> i32;
        fn SetClipboardData(fmt: u32, h: *mut c_void) -> *mut c_void;
        fn GetClipboardData(fmt: u32) -> *mut c_void;
        fn IsClipboardFormatAvailable(fmt: u32) -> i32;
        fn RegisterClipboardFormatW(name: *const u16) -> u32;
        fn GetActiveWindow() -> isize;
        fn GetForegroundWindow() -> isize;
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GlobalAlloc(flags: u32, bytes: usize) -> *mut c_void;
        fn GlobalLock(h: *mut c_void) -> *mut c_void;
        fn GlobalUnlock(h: *mut c_void) -> i32;
        fn GlobalFree(h: *mut c_void) -> *mut c_void;
    }
    #[link(name = "shell32")]
    unsafe extern "system" {
        fn DragQueryFileW(hdrop: *mut c_void, i: u32, buf: *mut u16, cch: u32) -> u32;
    }

    const CF_HDROP: u32 = 15;
    const GHND: u32 = 0x0042; // GMEM_MOVEABLE | GMEM_ZEROINIT

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn effect_format() -> u32 {
        unsafe { RegisterClipboardFormatW(wide("Preferred DropEffect").as_ptr()) }
    }

    /// Abre el portapapeles (con reintentos: otra aplicación puede tenerlo un instante).
    fn open() -> bool {
        unsafe {
            let mut hwnd = GetActiveWindow();
            if hwnd == 0 {
                hwnd = GetForegroundWindow();
            }
            for _ in 0..10 {
                if OpenClipboard(hwnd) != 0 {
                    return true;
                }
                crate::vfs::thread::sleep(std::time::Duration::from_millis(20));
            }
            false
        }
    }

    unsafe fn global_from(bytes: &[u8]) -> *mut c_void {
        unsafe {
            let h = GlobalAlloc(GHND, bytes.len().max(1));
            if h.is_null() {
                return h;
            }
            let p = GlobalLock(h) as *mut u8;
            if p.is_null() {
                GlobalFree(h);
                return std::ptr::null_mut();
            }
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), p, bytes.len());
            GlobalUnlock(h);
            h
        }
    }

    pub fn set_files(paths: &[PathBuf], cut: bool) -> bool {
        if paths.is_empty() || !open() {
            return false;
        }
        unsafe {
            // DROPFILES: pFiles (20), pt (0,0), fNC (0), fWide (1); después, rutas UTF-16 y doble NUL
            let mut buf: Vec<u8> = Vec::new();
            buf.extend_from_slice(&20u32.to_le_bytes());
            buf.extend_from_slice(&[0u8; 12]);
            buf.extend_from_slice(&1u32.to_le_bytes());
            for p in paths {
                for u in p.as_os_str().encode_wide() {
                    buf.extend_from_slice(&u.to_le_bytes());
                }
                buf.extend_from_slice(&[0, 0]);
            }
            buf.extend_from_slice(&[0, 0]);
            let h = global_from(&buf);
            let h2 = global_from(&(if cut { 2u32 } else { 1u32 }).to_le_bytes());
            let mut ok = false;
            if !h.is_null() {
                EmptyClipboard();
                ok = !SetClipboardData(CF_HDROP, h).is_null();
                if !ok {
                    GlobalFree(h);
                }
                if !h2.is_null() && ok {
                    if SetClipboardData(effect_format(), h2).is_null() {
                        GlobalFree(h2);
                    }
                }
            }
            CloseClipboard();
            ok
        }
    }

    pub fn get_files() -> Option<(Vec<PathBuf>, bool)> {
        unsafe {
            if IsClipboardFormatAvailable(CF_HDROP) == 0 || !open() {
                return None;
            }
            let mut out: Vec<PathBuf> = Vec::new();
            let mut cut = false;
            let h = GetClipboardData(CF_HDROP);
            if !h.is_null() {
                let n = DragQueryFileW(h, 0xFFFF_FFFF, std::ptr::null_mut(), 0);
                for i in 0..n {
                    let len = DragQueryFileW(h, i, std::ptr::null_mut(), 0) as usize;
                    let mut b = vec![0u16; len + 1];
                    let got = DragQueryFileW(h, i, b.as_mut_ptr(), (len + 1) as u32) as usize;
                    out.push(PathBuf::from(std::ffi::OsString::from_wide(&b[..got.min(len)])));
                }
            }
            let fmt = effect_format();
            if fmt != 0 && IsClipboardFormatAvailable(fmt) != 0 {
                let h2 = GetClipboardData(fmt);
                if !h2.is_null() {
                    let p = GlobalLock(h2) as *const u32;
                    if !p.is_null() {
                        cut = *p & 2 != 0;
                        GlobalUnlock(h2);
                    }
                }
            }
            CloseClipboard();
            if out.is_empty() {
                None
            } else {
                Some((out, cut))
            }
        }
    }
}

/// macOS: NSPasteboard a través de JavaScript for Automation (`osascript -l JavaScript`); el
/// Finder no tiene «cortar» en el portapapeles, así que sólo se comparte la lista de archivos.
#[cfg(target_os = "macos")]
mod clip_impl {
    use super::*;
    use std::process::{Command, Stdio};

    const SET: &str = r#"function run(argv) {
  ObjC.import('AppKit');
  var pb = $.NSPasteboard.generalPasteboard;
  pb.clearContents;
  var arr = $.NSMutableArray.alloc.init;
  for (var i = 0; i < argv.length; i++) { arr.addObject($.NSURL.fileURLWithPath(argv[i])); }
  return pb.writeObjects(arr) ? "ok" : "fail";
}"#;

    const GET: &str = r#"function run(argv) {
  ObjC.import('AppKit');
  var pb = $.NSPasteboard.generalPasteboard;
  var opts = $({ NSPasteboardURLReadingFileURLsOnlyKey: true });
  var urls = pb.readObjectsForClassesOptions($([$.NSURL]), opts);
  var out = [];
  if (urls) { for (var i = 0; i < urls.count; i++) { out.push(ObjC.unwrap(urls.objectAtIndex(i).path)); } }
  return out.join("\n");
}"#;

    pub fn set_files(paths: &[PathBuf], _cut: bool) -> bool {
        if paths.is_empty() {
            return false;
        }
        Command::new("osascript")
            .args(["-l", "JavaScript", "-e", SET])
            .args(paths)
            .stdin(Stdio::null())
            .output()
            .map(|o| o.status.success() && String::from_utf8_lossy(&o.stdout).trim() == "ok")
            .unwrap_or(false)
    }

    pub fn get_files() -> Option<(Vec<PathBuf>, bool)> {
        let o = Command::new("osascript")
            .args(["-l", "JavaScript", "-e", GET])
            .stdin(Stdio::null())
            .output()
            .ok()?;
        if !o.status.success() {
            return None;
        }
        let v: Vec<PathBuf> = String::from_utf8_lossy(&o.stdout)
            .lines()
            .map(|l| l.trim())
            .filter(|l| !l.is_empty())
            .map(PathBuf::from)
            .collect();
        if v.is_empty() {
            None
        } else {
            Some((v, false))
        }
    }
}

#[cfg(not(any(target_os = "linux", windows, target_os = "macos")))]
mod clip_impl {
    use super::*;
    pub fn set_files(_p: &[PathBuf], _cut: bool) -> bool {
        false
    }
    pub fn get_files() -> Option<(Vec<PathBuf>, bool)> {
        None
    }
}

pub use clip_impl::{get_files as clip_get_files, set_files as clip_set_files};

// ---------------------------------------------------------------------------
// Expulsar unidades (Windows)
// ---------------------------------------------------------------------------

/// Windows: «Expulsar» de la barra lateral. Unidades USB (también discos USB «fijos»): se pide la
/// retirada segura del dispositivo a Plug and Play (`CM_Request_Device_EjectW`, lo mismo que «Quitar
/// hardware de forma segura»: vacía cachés, desmonta y se niega si algo lo está usando). CD/DVD y
/// lectores de tarjetas sin dispositivo extraíble: se bloquea, desmonta y expulsa el medio. Unidades
/// de red asignadas: se desconectan (`WNetCancelConnection2W`). Todo bloquea: llamar desde un hilo.
#[cfg(windows)]
pub mod win_eject {
    use crate::i18n::{t, tf};
    use std::ffi::c_void;
    use std::path::Path;
    use std::sync::Mutex;
    use std::time::{Duration, Instant};
    use windows::core::GUID;

    type Handle = *mut c_void;

    const DRIVE_REMOVABLE: u32 = 2;
    const DRIVE_FIXED: u32 = 3;
    const DRIVE_REMOTE: u32 = 4;
    const DRIVE_CDROM: u32 = 5;

    const GENERIC_READ: u32 = 0x8000_0000;
    const GENERIC_WRITE: u32 = 0x4000_0000;
    const FILE_SHARE_READ: u32 = 1;
    const FILE_SHARE_WRITE: u32 = 2;
    const OPEN_EXISTING: u32 = 3;

    const FSCTL_LOCK_VOLUME: u32 = 0x0009_0018;
    const FSCTL_DISMOUNT_VOLUME: u32 = 0x0009_0020;
    const IOCTL_STORAGE_MEDIA_REMOVAL: u32 = 0x002D_4804;
    const IOCTL_STORAGE_EJECT_MEDIA: u32 = 0x002D_4808;
    const IOCTL_STORAGE_GET_DEVICE_NUMBER: u32 = 0x002D_1080;
    const IOCTL_STORAGE_QUERY_PROPERTY: u32 = 0x002D_1400;

    const FILE_DEVICE_CD_ROM: u32 = 2;
    const FILE_DEVICE_DVD: u32 = 0x33;
    const GUID_DEVINTERFACE_DISK: GUID = GUID::from_u128(0x53f56307_b6bf_11d0_94f2_00a0c91efb8b);
    const GUID_DEVINTERFACE_CDROM: GUID = GUID::from_u128(0x53f56308_b6bf_11d0_94f2_00a0c91efb8b);

    const CR_SUCCESS: u32 = 0;
    const CR_BUFFER_SMALL: u32 = 26;
    const CM_GET_DEVICE_INTERFACE_LIST_PRESENT: u32 = 0;
    const CM_DRP_SERVICE: u32 = 0x05;
    const CM_DRP_CAPABILITIES: u32 = 0x10;
    const CM_DEVCAP_REMOVABLE: u32 = 0x04;
    const DEVPROP_TYPE_STRING: u32 = 0x12;

    #[repr(C)]
    struct DevPropKey {
        fmtid: GUID,
        pid: u32,
    }
    const DEVPKEY_DEVICE_INSTANCEID: DevPropKey = DevPropKey { fmtid: GUID::from_u128(0x78c34fc8_104a_4aca_9ea4_524d52996e57), pid: 256 };

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetDriveTypeW(root: *const u16) -> u32;
        fn QueryDosDeviceW(name: *const u16, target: *mut u16, max: u32) -> u32;
        fn CreateFileW(name: *const u16, access: u32, share: u32, sa: *const c_void, disp: u32, flags: u32, template: Handle) -> Handle;
        fn DeviceIoControl(h: Handle, code: u32, inb: *const c_void, insz: u32, outb: *mut c_void, outsz: u32, ret: *mut u32, ov: *mut c_void) -> i32;
        fn CloseHandle(h: Handle) -> i32;
    }
    #[link(name = "cfgmgr32")]
    unsafe extern "system" {
        fn CM_Get_Device_Interface_List_SizeW(len: *mut u32, guid: *const GUID, dev: *const u16, flags: u32) -> u32;
        fn CM_Get_Device_Interface_ListW(guid: *const GUID, dev: *const u16, buf: *mut u16, len: u32, flags: u32) -> u32;
        fn CM_Get_Device_Interface_PropertyW(iface: *const u16, key: *const DevPropKey, ty: *mut u32, buf: *mut u8, size: *mut u32, flags: u32) -> u32;
        fn CM_Locate_DevNodeW(dev: *mut u32, id: *const u16, flags: u32) -> u32;
        fn CM_Get_Parent(parent: *mut u32, dev: u32, flags: u32) -> u32;
        fn CM_Get_DevNode_Registry_PropertyW(dev: u32, prop: u32, ty: *mut u32, buf: *mut c_void, len: *mut u32, flags: u32) -> u32;
        fn CM_Request_Device_EjectW(dev: u32, veto: *mut i32, name: *mut u16, namelen: u32, flags: u32) -> u32;
        fn CM_MapCrToWin32Err(cr: u32, default: u32) -> u32;
    }
    #[link(name = "mpr")]
    unsafe extern "system" {
        fn WNetCancelConnection2W(name: *const u16, flags: u32, force: i32) -> u32;
    }

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn os_err(code: u32) -> String {
        std::io::Error::from_raw_os_error(code as i32).to_string()
    }

    fn last_err() -> String {
        std::io::Error::last_os_error().to_string()
    }

    fn in_use() -> String {
        t("La unidad está en uso: cierra los archivos y programas que la usan y vuelve a intentarlo").into()
    }

    /// Manejador de Win32 que se cierra al soltarlo.
    struct H(Handle);
    impl Drop for H {
        fn drop(&mut self) {
            // SAFETY: manejador válido devuelto por CreateFileW, cerrado una sola vez.
            unsafe { CloseHandle(self.0) };
        }
    }

    /// Abre un dispositivo (`\\.\E:` o la ruta de una interfaz); `access` 0 sólo permite consultas.
    fn open(w: &[u16], access: u32) -> Option<H> {
        // SAFETY: `w` termina en NUL; sin atributos de seguridad ni plantilla.
        let h = unsafe { CreateFileW(w.as_ptr(), access, FILE_SHARE_READ | FILE_SHARE_WRITE, std::ptr::null(), OPEN_EXISTING, 0, std::ptr::null_mut()) };
        // INVALID_HANDLE_VALUE es -1
        (!h.is_null() && h as isize != -1).then_some(H(h))
    }

    fn volume(l: char) -> Vec<u16> {
        wide(&format!(r"\\.\{l}:"))
    }

    /// DeviceIoControl síncrono: bytes devueltos o el código de error de Windows.
    fn ioctl(h: &H, code: u32, inb: &[u8], out: &mut [u8]) -> Result<u32, u32> {
        let mut got = 0u32;
        let inp = if inb.is_empty() { std::ptr::null() } else { inb.as_ptr() as *const c_void };
        let outp = if out.is_empty() { std::ptr::null_mut() } else { out.as_mut_ptr() as *mut c_void };
        // SAFETY: los búferes viven durante la llamada y sus tamaños son los pasados; sin OVERLAPPED.
        let ok = unsafe { DeviceIoControl(h.0, code, inp, inb.len() as u32, outp, out.len() as u32, &mut got, std::ptr::null_mut()) };
        if ok != 0 {
            Ok(got)
        } else {
            Err(std::io::Error::last_os_error().raw_os_error().unwrap_or(0) as u32)
        }
    }

    fn u32_at(b: &[u8], off: usize) -> u32 {
        u32::from_le_bytes(b[off..off + 4].try_into().unwrap())
    }

    /// (tipo de dispositivo, número de disco) de un volumen o de un disco (STORAGE_DEVICE_NUMBER).
    fn device_number(h: &H) -> Option<(u32, u32)> {
        let mut out = [0u8; 12];
        let n = ioctl(h, IOCTL_STORAGE_GET_DEVICE_NUMBER, &[], &mut out).ok()?;
        (n >= 8).then(|| (u32_at(&out, 0), u32_at(&out, 4)))
    }

    /// Bus del disco (STORAGE_DEVICE_DESCRIPTOR.BusType: USB 7, 1394 4, SD 12, MMC 13…).
    fn bus_type(h: &H) -> Option<u32> {
        // STORAGE_PROPERTY_QUERY { StorageDeviceProperty, PropertyStandardQuery }
        let q = [0u8; 12];
        let mut out = [0u8; 1024];
        let n = ioctl(h, IOCTL_STORAGE_QUERY_PROPERTY, &q, &mut out).ok()?;
        (n >= 32).then(|| u32_at(&out, 28))
    }

    /// Letra de una raíz de unidad («E:\», «e:»…); nada si no es la raíz.
    fn letter(root: &Path) -> Option<char> {
        let s = root.to_str()?;
        let b = s.as_bytes();
        let ok = (b.len() == 2 || (b.len() == 3 && matches!(b[2], b'\\' | b'/'))) && b[0].is_ascii_alphabetic() && b[1] == b':';
        ok.then(|| (b[0] as char).to_ascii_uppercase())
    }

    fn drive_type(l: char) -> u32 {
        let w = wide(&format!("{l}:\\"));
        // SAFETY: cadena terminada en NUL.
        unsafe { GetDriveTypeW(w.as_ptr()) }
    }

    fn system_drive() -> Option<char> {
        std::env::var("SystemDrive").ok()?.chars().next().map(|c| c.to_ascii_uppercase())
    }

    /// Disquetera clásica: su número de dispositivo no es de la serie de los discos.
    fn is_floppy(l: char) -> bool {
        let name = wide(&format!("{l}:"));
        let mut buf = [0u16; 512];
        // SAFETY: `name` termina en NUL; `buf` tiene el tamaño indicado.
        let n = unsafe { QueryDosDeviceW(name.as_ptr(), buf.as_mut_ptr(), buf.len() as u32) } as usize;
        n > 0 && String::from_utf16_lossy(&buf[..n.min(buf.len())]).to_ascii_lowercase().contains("\\floppy")
    }

    fn hotplug_bus(b: u32) -> bool {
        matches!(b, 4 | 7 | 12 | 13)
    }

    fn compute_ejectable(l: char) -> bool {
        if system_drive() == Some(l) {
            return false;
        }
        match drive_type(l) {
            DRIVE_REMOVABLE | DRIVE_CDROM | DRIVE_REMOTE => true,
            // disco USB o FireWire que Windows presenta como fijo
            DRIVE_FIXED => open(&volume(l), 0).and_then(|h| bus_type(&h)).is_some_and(hotplug_bus),
            _ => false,
        }
    }

    static CACHE: Mutex<Vec<(char, Instant, bool)>> = Mutex::new(Vec::new());

    /// ¿Ofrecer «Expulsar» para esta raíz? Rápido (se llama al dibujar el menú): se recuerda un rato.
    pub fn ejectable(root: &Path) -> bool {
        let Some(l) = letter(root) else { return false };
        let mut c = CACHE.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(&(_, at, v)) = c.iter().find(|e| e.0 == l) {
            if at.elapsed() < Duration::from_secs(5) {
                return v;
            }
        }
        let v = compute_ejectable(l);
        c.retain(|e| e.0 != l);
        c.push((l, Instant::now(), v));
        v
    }

    /// ¿Es una unidad de red asignada? (para el aviso: «Desconectado» en vez de «Expulsado»)
    pub fn is_remote(root: &Path) -> bool {
        letter(root).is_some_and(|l| drive_type(l) == DRIVE_REMOTE)
    }

    pub fn eject(root: &Path) -> Result<(), String> {
        let l = letter(root).ok_or_else(|| t("Esta unidad no se puede expulsar").to_string())?;
        CACHE.lock().unwrap_or_else(|e| e.into_inner()).retain(|e| e.0 != l);
        // la carpeta actual del proceso dentro de la unidad la mantendría abierta
        if std::env::current_dir().ok().and_then(|d| letter(Path::new(d.to_str()?.get(..2)?))) == Some(l) {
            let safe = std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\".into());
            let _ = std::env::set_current_dir(safe);
        }
        match drive_type(l) {
            DRIVE_REMOTE => disconnect(l),
            DRIVE_CDROM => eject_media(l, GENERIC_READ),
            _ if is_floppy(l) => eject_media(l, GENERIC_READ | GENERIC_WRITE),
            _ => match removable_devnode(l) {
                Some(dev) => request_eject(dev),
                // lector de tarjetas interno y similares: sólo se puede sacar el medio
                None => eject_media(l, GENERIC_READ | GENERIC_WRITE),
            },
        }
    }

    fn disconnect(l: char) -> Result<(), String> {
        const CONNECT_UPDATE_PROFILE: u32 = 1;
        let w = wide(&format!("{l}:"));
        // sin forzar: con archivos abiertos falla (y se avisa) en vez de perder datos
        // SAFETY: cadena terminada en NUL que vive durante la llamada.
        let code = unsafe { WNetCancelConnection2W(w.as_ptr(), CONNECT_UPDATE_PROFILE, 0) };
        match code {
            // 2250: ya no estaba conectada
            0 | 2250 => Ok(()),
            // ERROR_OPEN_FILES, ERROR_DEVICE_IN_USE
            2401 | 2404 => Err(in_use()),
            n => Err(os_err(n)),
        }
    }

    /// Bloquea, desmonta y expulsa el medio (bandeja del CD, tarjeta de un lector interno).
    fn eject_media(l: char, access: u32) -> Result<(), String> {
        let h = open(&volume(l), access).ok_or_else(last_err)?;
        // el bloqueo falla mientras haya archivos abiertos: se reintenta un poco
        let mut locked = false;
        for i in 0..10 {
            if ioctl(&h, FSCTL_LOCK_VOLUME, &[], &mut []).is_ok() {
                locked = true;
                break;
            }
            if i < 9 {
                std::thread::sleep(Duration::from_millis(300));
            }
        }
        if !locked {
            return Err(in_use());
        }
        ioctl(&h, FSCTL_DISMOUNT_VOLUME, &[], &mut []).map_err(os_err)?;
        // PREVENT_MEDIA_REMOVAL { PreventMediaRemoval: FALSE }
        let _ = ioctl(&h, IOCTL_STORAGE_MEDIA_REMOVAL, &[0u8], &mut []);
        ioctl(&h, IOCTL_STORAGE_EJECT_MEDIA, &[], &mut []).map_err(os_err)?;
        // al cerrar `h` se libera el bloqueo
        Ok(())
    }

    /// Rutas (terminadas en NUL) de las interfaces presentes de una clase.
    fn interfaces(guid: &GUID) -> Vec<Vec<u16>> {
        for _ in 0..3 {
            let mut len = 0u32;
            // SAFETY: punteros válidos; sin filtro de dispositivo.
            if unsafe { CM_Get_Device_Interface_List_SizeW(&mut len, guid, std::ptr::null(), CM_GET_DEVICE_INTERFACE_LIST_PRESENT) } != CR_SUCCESS || len <= 1 {
                return Vec::new();
            }
            let mut buf = vec![0u16; len as usize];
            // SAFETY: `buf` tiene `len` caracteres.
            let cr = unsafe { CM_Get_Device_Interface_ListW(guid, std::ptr::null(), buf.as_mut_ptr(), len, CM_GET_DEVICE_INTERFACE_LIST_PRESENT) };
            if cr == CR_BUFFER_SMALL {
                // llegó un dispositivo entre las dos llamadas
                continue;
            }
            if cr != CR_SUCCESS {
                return Vec::new();
            }
            return buf.split(|&c| c == 0).filter(|s| !s.is_empty()).map(|s| s.iter().copied().chain(std::iter::once(0)).collect()).collect();
        }
        Vec::new()
    }

    /// Nodo de dispositivo (devnode) de una interfaz.
    fn devnode_of(iface: &[u16]) -> Option<u32> {
        let mut ty = 0u32;
        let mut buf = [0u8; 2048];
        let mut size = buf.len() as u32;
        // SAFETY: `iface` termina en NUL; `buf` tiene `size` bytes.
        let cr = unsafe { CM_Get_Device_Interface_PropertyW(iface.as_ptr(), &DEVPKEY_DEVICE_INSTANCEID, &mut ty, buf.as_mut_ptr(), &mut size, 0) };
        if cr != CR_SUCCESS || ty != DEVPROP_TYPE_STRING {
            return None;
        }
        let mut id: Vec<u16> = buf[..(size as usize).min(buf.len())].chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).take_while(|&c| c != 0).collect();
        id.push(0);
        let mut dev = 0u32;
        // SAFETY: `id` termina en NUL. CM_LOCATE_DEVNODE_NORMAL = 0.
        (unsafe { CM_Locate_DevNodeW(&mut dev, id.as_ptr(), 0) } == CR_SUCCESS).then_some(dev)
    }

    fn reg_u32(dev: u32, prop: u32) -> Option<u32> {
        let (mut ty, mut v, mut len) = (0u32, 0u32, 4u32);
        // SAFETY: `v` tiene los 4 bytes indicados.
        (unsafe { CM_Get_DevNode_Registry_PropertyW(dev, prop, &mut ty, &mut v as *mut u32 as *mut c_void, &mut len, 0) } == CR_SUCCESS).then_some(v)
    }

    fn reg_string(dev: u32, prop: u32) -> String {
        let (mut ty, mut buf, mut len) = (0u32, [0u16; 256], 512u32);
        // SAFETY: `buf` tiene `len` bytes.
        if unsafe { CM_Get_DevNode_Registry_PropertyW(dev, prop, &mut ty, buf.as_mut_ptr() as *mut c_void, &mut len, 0) } != CR_SUCCESS {
            return String::new();
        }
        let n = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        String::from_utf16_lossy(&buf[..n])
    }

    /// El dispositivo extraíble que contiene la unidad `l`: el disco o el antepasado más cercano
    /// marcado como extraíble (el dispositivo USB), sin pasar nunca de un concentrador USB (que
    /// arrastraría todo lo conectado a él).
    fn removable_devnode(l: char) -> Option<u32> {
        let (dtype, dnum) = device_number(&open(&volume(l), 0)?)?;
        let guid = if matches!(dtype, FILE_DEVICE_CD_ROM | FILE_DEVICE_DVD) { GUID_DEVINTERFACE_CDROM } else { GUID_DEVINTERFACE_DISK };
        let iface = interfaces(&guid).into_iter().find(|i| open(i, 0).and_then(|h| device_number(&h)) == Some((dtype, dnum)))?;
        let mut dev = devnode_of(&iface)?;
        for _ in 0..4 {
            if reg_u32(dev, CM_DRP_CAPABILITIES).is_some_and(|c| c & CM_DEVCAP_REMOVABLE != 0) {
                return Some(dev);
            }
            let mut parent = 0u32;
            // SAFETY: devnode válido; `parent` recibe el resultado.
            if unsafe { CM_Get_Parent(&mut parent, dev, 0) } != CR_SUCCESS {
                return None;
            }
            if reg_string(parent, CM_DRP_SERVICE).to_ascii_lowercase().starts_with("usbhub") {
                return None;
            }
            dev = parent;
        }
        None
    }

    /// Retirada segura: Plug and Play pregunta a todos los que usan el dispositivo y lo desmonta,
    /// o dice quién lo impide (veto).
    fn request_eject(dev: u32) -> Result<(), String> {
        let mut last = Err(in_use());
        for i in 0..3 {
            let mut veto = 0i32;
            let mut name = [0u16; 512];
            // SAFETY: `name` tiene el tamaño indicado; con nombre de veto Windows no enseña su propio aviso.
            let cr = unsafe { CM_Request_Device_EjectW(dev, &mut veto, name.as_mut_ptr(), name.len() as u32, 0) };
            let n = name.iter().position(|&c| c == 0).unwrap_or(name.len());
            let who = String::from_utf16_lossy(&name[..n]);
            last = match veto {
                // PNP_VetoTypeUnknown: sin veto
                0 if cr == CR_SUCCESS => return Ok(()),
                0 => {
                    // SAFETY: función pura de traducción de códigos.
                    let w = unsafe { CM_MapCrToWin32Err(cr, 31) };
                    Err(os_err(w))
                }
                // PNP_VetoAlreadyRemoved
                13 => return Ok(()),
                // PNP_VetoWindowsApp / PNP_VetoWindowsService: el nombre es el programa o el servicio
                3 | 4 if !who.is_empty() => {
                    let who = Path::new(&who).file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or(who);
                    Err(tf("La unidad está en uso por «{}»: ciérralo y vuelve a intentarlo", &[&who]))
                }
                // PendingClose, WindowsApp, WindowsService, OutstandingOpen, Device, Driver
                2..=7 => Err(in_use()),
                // PNP_VetoInsufficientRights
                12 => Err(t("Acceso denegado").into()),
                v => Err(tf("Windows no permitió expulsar la unidad (motivo {})", &[&v])),
            };
            if i < 2 {
                std::thread::sleep(Duration::from_millis(500));
            }
        }
        last
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn letras_de_raiz() {
            assert_eq!(letter(Path::new("e:\\")), Some('E'));
            assert_eq!(letter(Path::new("F:")), Some('F'));
            assert_eq!(letter(Path::new("F:/")), Some('F'));
            assert_eq!(letter(Path::new("F:\\x")), None);
            assert_eq!(letter(Path::new("\\\\srv\\c")), None);
        }
    }
}

#[cfg(test)]
mod compress_tests {
    use super::*;

    fn tree(tag: &str) -> PathBuf {
        let d = crate::vfs::temp_dir().join(format!("fb_cmp_{}_{tag}", crate::vfs::pid()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(d.join("src/sub")).unwrap();
        fs::write(d.join("src/a.txt"), b"hola mundo ".repeat(500)).unwrap();
        fs::write(d.join("src/sub/b.bin"), (0..=255u8).cycle().take(70_000).collect::<Vec<_>>()).unwrap();
        d
    }

    fn same(a: &Path, b: &Path) {
        assert_eq!(fs::read(a.join("a.txt")).unwrap(), fs::read(b.join("a.txt")).unwrap());
        assert_eq!(fs::read(a.join("sub/b.bin")).unwrap(), fs::read(b.join("sub/b.bin")).unwrap());
    }

    #[test]
    fn roundtrip_all_formats_and_levels() {
        let d = tree("rt");
        let ctx = egui::Context::default();
        for fmt in [ArchFmt::Zip, ArchFmt::TarGz, ArchFmt::SevenZ] {
            for level in [0u32, 1, 6, 9] {
                let out = d.join(format!("o_{level}.{}", fmt.ext()));
                let o = CompressOpts { fmt, level, password: None };
                compress_paths(&[d.join("src")], &out, &o, &Progress::default(), &ctx).unwrap();
                let dst = d.join(format!("x_{level}_{}", fmt.ext()));
                crate::archive::extract_all(&out, &dst, &Progress::default(), &ctx, None).unwrap();
                same(&d.join("src"), &dst.join("src"));
            }
        }
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn encrypted_zip_and_7z() {
        let d = tree("enc");
        let ctx = egui::Context::default();
        let o = CompressOpts { fmt: ArchFmt::Zip, level: 6, password: Some("clave".into()) };
        let zp = d.join("e.zip");
        compress_paths(&[d.join("src")], &zp, &o, &Progress::default(), &ctx).unwrap();
        let mut ar = zip::ZipArchive::new(File::open(&zp).unwrap()).unwrap();
        assert!(ar.by_name("src/a.txt").is_err() || ar.by_name("src/a.txt").unwrap().read_to_end(&mut Vec::new()).is_err());
        let mut v = Vec::new();
        ar.by_name_decrypt("src/a.txt", b"clave").unwrap().read_to_end(&mut v).unwrap();
        assert_eq!(v, fs::read(d.join("src/a.txt")).unwrap());

        let o = CompressOpts { fmt: ArchFmt::SevenZ, level: 6, password: Some("clave".into()) };
        let sp = d.join("e.7z");
        compress_paths(&[d.join("src")], &sp, &o, &Progress::default(), &ctx).unwrap();
        let dst = d.join("x7");
        sevenz_rust2::decompress_file_with_password(&sp, &dst, "clave".into()).unwrap();
        same(&d.join("src"), &dst.join("src"));
        assert!(sevenz_rust2::decompress_file_with_password(&sp, d.join("bad"), "otra".into()).is_err());
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn extract_asks_for_password() {
        use crate::archive::{extract_all, BAD_PASS, NEED_PASS};
        let d = tree("pw");
        let ctx = egui::Context::default();
        for fmt in [ArchFmt::Zip, ArchFmt::SevenZ] {
            let o = CompressOpts { fmt, level: 6, password: Some("clave".into()) };
            let out = d.join(format!("p.{}", fmt.ext()));
            compress_paths(&[d.join("src")], &out, &o, &Progress::default(), &ctx).unwrap();
            let run = |pw: Option<&str>, n: &str| extract_all(&out, &d.join(n), &Progress::default(), &ctx, pw);
            assert_eq!(run(None, "n").unwrap_err(), NEED_PASS, "{}", fmt.ext());
            assert_eq!(run(Some("otra"), "m").unwrap_err(), BAD_PASS, "{}", fmt.ext());
            let ok = format!("ok_{}", fmt.ext());
            run(Some("clave"), &ok).unwrap();
            same(&d.join("src"), &d.join(&ok).join("src"));
        }
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn cancel_stops() {
        let d = tree("cancel");
        let p = Progress::default();
        p.cancel.store(true, AO::Relaxed);
        let o = CompressOpts { fmt: ArchFmt::TarGz, level: 6, password: None };
        assert!(compress_paths(&[d.join("src")], &d.join("c.tar.gz"), &o, &p, &egui::Context::default()).is_err());
        let _ = fs::remove_dir_all(&d);
    }
}

#[cfg(test)]
mod archive_link_tests {
    use super::*;

    #[cfg(unix)]
    fn tmpd(tag: &str) -> PathBuf {
        let n = web_time::SystemTime::now().duration_since(web_time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
        let d = crate::vfs::temp_dir().join(format!("fbw1-tools-{tag}-{}-{n:x}", crate::vfs::pid()));
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn zip64_only_for_large_files() {
        let o = zip::write::SimpleFileOptions::default();
        assert_eq!(zip_entry_opts(o, 0), o.large_file(false));
        assert_eq!(zip_entry_opts(o, ZIP64_AT - 1), o.large_file(false));
        assert_eq!(zip_entry_opts(o, ZIP64_AT), o.large_file(true));
        assert_eq!(zip_entry_opts(o, 5 << 30), o.large_file(true));
        assert_eq!(zip_entry_opts(o, u64::MAX), o.large_file(true));
        // margen: el umbral queda por debajo de 4 GiB
        const { assert!(ZIP64_AT < u32::MAX as u64) };
    }

    /// Los enlaces se guardan como enlaces en zip y tar.gz (nunca lo que apuntan), vuelven como
    /// enlaces al extraer y los que salen de la carpeta se descartan avisando. Un FIFO se omite y
    /// se avisa (abrirlo colgaría la compresión).
    #[cfg(unix)]
    #[test]
    fn symlinks_and_special_files_in_archives() {
        let d = tmpd("links");
        let src = d.join("src");
        fs::create_dir_all(src.join("real")).unwrap();
        fs::write(src.join("real/x.txt"), b"x").unwrap();
        std::os::unix::fs::symlink("real/x.txt", src.join("inner")).unwrap();
        std::os::unix::fs::symlink("real", src.join("dirlink")).unwrap();
        std::os::unix::fs::symlink("/etc/hostname", src.join("outer")).unwrap();
        {
            use std::os::unix::ffi::OsStrExt;
            let c = std::ffi::CString::new(src.join("pipe").as_os_str().as_bytes()).unwrap();
            assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
        }
        let ctx = egui::Context::default();
        for fmt in [ArchFmt::Zip, ArchFmt::TarGz] {
            let out = d.join(format!("o.{}", fmt.ext()));
            let p = Progress::default();
            compress_paths(&[src.clone()], &out, &CompressOpts { fmt, level: 6, password: None }, &p, &ctx).unwrap();
            let errs = p.errors.lock().unwrap().clone();
            assert!(errs.iter().any(|e| e.contains("pipe")), "{}: {errs:?}", fmt.ext());
            if fmt == ArchFmt::Zip {
                let mut ar = zip::ZipArchive::new(File::open(&out).unwrap()).unwrap();
                assert!(ar.by_name("src/inner").unwrap().is_symlink());
                assert!(ar.by_name("src/dirlink").unwrap().is_symlink());
                assert!(ar.by_name("src/dirlink/x.txt").is_err(), "no se sigue el enlace a carpeta");
            }
            let x = d.join(format!("x_{}", fmt.ext()));
            let p = Progress::default();
            crate::archive::extract_all(&out, &x, &p, &ctx, None).unwrap();
            let e = x.join("src");
            assert_eq!(fs::read_link(e.join("inner")).unwrap(), Path::new("real/x.txt"), "{}", fmt.ext());
            assert_eq!(fs::read_link(e.join("dirlink")).unwrap(), Path::new("real"), "{}", fmt.ext());
            assert_eq!(fs::read(e.join("inner")).unwrap(), b"x");
            assert!(fs::symlink_metadata(e.join("outer")).is_err(), "{}: enlace absoluto descartado", fmt.ext());
            assert!(p.errors.lock().unwrap().iter().any(|m| m.contains("outer")), "{}", fmt.ext());
        }
        let _ = fs::remove_dir_all(&d);
    }

    /// Nombres que no son UTF-8: tar los conserva byte a byte.
    #[cfg(unix)]
    #[test]
    fn tar_keeps_non_utf8_names() {
        use std::os::unix::ffi::OsStrExt;
        let d = tmpd("nonutf8");
        let raw = std::ffi::OsStr::from_bytes(b"caf\xe9.txt");
        fs::create_dir_all(d.join("src")).unwrap();
        fs::write(d.join("src").join(raw), b"x").unwrap();
        let ctx = egui::Context::default();
        let out = d.join("o.tar.gz");
        compress_paths(&[d.join("src")], &out, &CompressOpts { fmt: ArchFmt::TarGz, level: 1, password: None }, &Progress::default(), &ctx).unwrap();
        let x = d.join("x");
        crate::archive::extract_all(&out, &x, &Progress::default(), &ctx, None).unwrap();
        assert_eq!(fs::read(x.join("src").join(raw)).unwrap(), b"x");
        let _ = fs::remove_dir_all(&d);
    }
}

#[cfg(test)]
mod search_stat_tests {
    use super::*;

    /// Los `stat` de los resultados se hacen en otro hilo, por lotes, y la búsqueda sólo se da por
    /// terminada cuando todo está leído; lo que ya no existe se descarta.
    #[test]
    fn stat_pending_reads_in_batches() {
        let n = web_time::SystemTime::now().duration_since(web_time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
        let d = crate::vfs::temp_dir().join(format!("fbw3-stat-{}-{n:x}", crate::vfs::pid()));
        fs::create_dir_all(&d).unwrap();
        let st = Arc::new(Search::default());
        for i in 0..5 {
            let p = d.join(format!("f{i}"));
            fs::write(&p, b"x").unwrap();
            st.results.lock().unwrap().push((p, false));
        }
        st.results.lock().unwrap().push((d.join("borrado"), false));
        let ctx = egui::Context::default();
        let mut got = Vec::new();
        let t0 = web_time::Instant::now();
        loop {
            assert!(t0.elapsed().as_secs() < 30);
            stat_pending(&st, 2, &ctx);
            got.extend(std::mem::take(&mut *st.entries.lock().unwrap()));
            if stat_finished(&st) && st.entries.lock().unwrap().is_empty() {
                break;
            }
            // aún sin `done` nunca se da por terminada; después se marca
            if !st.done.load(AO::Acquire) && st.statted.load(AO::Acquire) >= 6 {
                assert!(!stat_finished(&st));
                st.done.store(true, AO::Release);
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        let mut names: Vec<String> = got.iter().map(|e| Path::new(&*e.name).file_name().unwrap().to_string_lossy().into_owned()).collect();
        names.sort();
        assert_eq!(names, ["f0", "f1", "f2", "f3", "f4"]);
        let _ = fs::remove_dir_all(&d);
    }
}
