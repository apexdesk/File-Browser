//! Archivos comprimidos: zip, 7z, rar (sólo lectura), tar y tar.gz/tgz.
//! `list` da un índice rápido (para la vista previa) y `extract_all` descomprime a una carpeta.
#[allow(unused_imports)]
use crate::vfs::{KindV as _, PathV as _};
use crate::fsys::Progress;
use crate::vfs::fs::{self, File};
use std::io::{self, BufReader, Read, Seek, SeekFrom};
use std::path::{Component, Path};
use std::sync::atomic::Ordering as AO;
use std::time::{Duration};
use web_time::{Instant};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Format {
    Zip,
    SevenZ,
    Rar,
    Tar,
    TarGz,
}

pub struct Item {
    pub name: String,
    pub size: u64,
    pub dir: bool,
}

pub struct Listing {
    pub items: Vec<Item>,
    /// El índice se cortó (demasiado grande o lento).
    pub truncated: bool,
}

/// Marcas de error: el comprimido necesita contraseña / la dada no sirve.
pub const NEED_PASS: &str = "\u{1}pass";
pub const BAD_PASS: &str = "\u{1}badpass";

/// Convierte el error de una biblioteca en NEED_PASS / BAD_PASS cuando es de contraseña.
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
fn pw_msg(msg: String, have: bool) -> String {
    let m = msg.to_lowercase();
    if m.contains("password required") || m.contains("password for encrypted") || m.contains("passwordrequired") {
        if have { BAD_PASS.into() } else { NEED_PASS.into() }
    } else if m.contains("wrong password") || m.contains("incorrect") && m.contains("password") || m.contains("bad password") || m.contains("badpassword") {
        BAD_PASS.into()
    } else {
        msg
    }
}

/// Comprueba la cabecera de arranque de un 7z (offset y tamaño de la cabecera principal) contra el
/// tamaño real del archivo, para no fiarse de tamaños declarados que provocarían reservas enormes.
fn sevenz_header_ok(path: &Path) -> Result<(), String> {
    let mut f = File::open(path).map_err(|e| e.to_string())?;
    let len = f.metadata().map_err(|e| e.to_string())?.len();
    let mut h = [0u8; 32];
    f.read_exact(&mut h).map_err(|_| crate::i18n::t("archivo 7z no válido").to_string())?;
    if h[..6] != [b'7', b'z', 0xBC, 0xAF, 0x27, 0x1C] {
        return Err(crate::i18n::t("archivo 7z no válido").into());
    }
    let off = u64::from_le_bytes(h[12..20].try_into().unwrap());
    let size = u64::from_le_bytes(h[20..28].try_into().unwrap());
    if size > 0 && (size > 512 << 20 || off.checked_add(32).and_then(|x| x.checked_add(size)).map(|e| e > len).unwrap_or(true)) {
        return Err(crate::i18n::t("cabecera 7z dañada").into());
    }
    Ok(())
}

/// Formato según el nombre del archivo.
pub fn detect(p: &Path) -> Option<Format> {
    let n = p.file_name()?.to_string_lossy().to_lowercase();
    if n.ends_with(".tar.gz") || n.ends_with(".tgz") {
        Some(Format::TarGz)
    } else if n.ends_with(".tar") {
        Some(Format::Tar)
    } else if n.ends_with(".zip") || n.ends_with(".jar") {
        Some(Format::Zip)
    } else if n.ends_with(".7z") {
        Some(Format::SevenZ)
    } else if n.ends_with(".rar") {
        Some(Format::Rar)
    } else {
        None
    }
}

/// Cuenta los bytes comprimidos leídos, para la barra de progreso.
struct Counting<'a, R> {
    inner: R,
    p: &'a Progress,
}

impl<R: Read> Read for Counting<'_, R> {
    fn read(&mut self, b: &mut [u8]) -> io::Result<usize> {
        if self.p.cancelled() {
            return Err(io::Error::other(crate::i18n::t("cancelado")));
        }
        let n = self.inner.read(b)?;
        self.p.done.fetch_add(n as u64, AO::Relaxed);
        Ok(n)
    }
}

impl<R: Seek> Seek for Counting<'_, R> {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        self.inner.seek(pos)
    }
}

/// Falla con `TimedOut` pasado el plazo (para no quedarse descomprimiendo un tar.gz enorme sólo por listarlo).
struct Deadline<R> {
    inner: R,
    end: Instant,
}

impl<R: Read> Read for Deadline<R> {
    fn read(&mut self, b: &mut [u8]) -> io::Result<usize> {
        if Instant::now() >= self.end {
            return Err(io::Error::new(io::ErrorKind::TimedOut, "plazo"));
        }
        self.inner.read(b)
    }
}

fn safe(rel: &Path) -> bool {
    rel.components().all(|c| matches!(c, Component::Normal(_) | Component::CurDir))
}

/// ¿Puede crearse, dentro de `root` (ya canónica), un enlace simbólico en la carpeta `parent`
/// (canónica) que apunte a `target`? Sólo destinos relativos, con los `..` al principio, que
/// queden dentro de `root`. Como `parent` no tiene enlaces y todos los enlaces creados cumplen lo
/// mismo, seguir cualquier cadena de ellos nunca sale de la carpeta de extracción.
pub(crate) fn link_ok(root: &Path, parent: &Path, target: &Path) -> bool {
    if !parent.starts_with(root) || target.as_os_str().is_empty() {
        return false;
    }
    let mut cur = parent.to_path_buf();
    let mut seen_normal = false;
    for c in target.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir if !seen_normal => {
                if !cur.pop() || !cur.starts_with(root) {
                    return false;
                }
            }
            Component::Normal(n) => {
                seen_normal = true;
                cur.push(n);
            }
            // absoluto, unidad de Windows o `..` tras un nombre (que un enlace intermedio
            // resolvería de otra forma)
            _ => return false,
        }
    }
    cur.starts_with(root)
}

/// ¿Está `p` (que existe) dentro de `root` (canónica) una vez resueltos sus enlaces?
pub(crate) fn inside(root: &Path, p: &Path) -> bool {
    fs::canonicalize(p).is_ok_and(|c| c.starts_with(root))
}

/// Ruta de destino de una entrada tar tal y como la calcula `unpack_in` (sin raíz ni `.`);
/// `None` si contiene `..`.
fn tar_dest(dst: &Path, rel: &Path) -> Option<std::path::PathBuf> {
    let mut out = dst.to_path_buf();
    for c in rel.components() {
        match c {
            Component::Prefix(..) | Component::RootDir | Component::CurDir => {}
            Component::ParentDir => return None,
            Component::Normal(n) => out.push(n),
        }
    }
    Some(out)
}

/// Fuera de la carpeta de destino: no se extrae y se avisa.
pub(crate) fn note_outside(p: &Progress, name: &str) {
    let mut v = p.errors.lock().unwrap();
    if v.len() < 500 {
        v.push(crate::i18n::tf("{}: omitido ({})", &[&name, &crate::i18n::t("fuera de la carpeta de destino")]));
    }
}

/// Índice del contenido sin extraer. Para en `max` entradas o al agotar `budget`.
pub fn list(path: &Path, max: usize, budget: Duration) -> Result<Listing, String> {
    let fmt = detect(path).ok_or(crate::i18n::t("formato no soportado"))?;
    let mut items: Vec<Item> = Vec::new();
    let mut truncated = false;
    match fmt {
        Format::Zip => {
            let f = File::open(path).map_err(|e| e.to_string())?;
            let mut ar = zip::ZipArchive::new(BufReader::new(f)).map_err(|e| e.to_string())?;
            for i in 0..ar.len() {
                if items.len() >= max {
                    truncated = true;
                    break;
                }
                let Ok(z) = ar.by_index_raw(i) else { continue };
                items.push(Item { name: z.name().map(|n| n.into_owned()).unwrap_or_default(), size: z.size(), dir: z.is_dir() });
            }
        }
        Format::SevenZ => {
            sevenz_header_ok(path)?;
            let ar = sevenz_rust2::Archive::open(path).map_err(|e| e.to_string())?;
            for e in &ar.files {
                if items.len() >= max {
                    truncated = true;
                    break;
                }
                items.push(Item { name: e.name().to_string(), size: e.size(), dir: e.is_directory() });
            }
        }
        #[cfg(any(target_os = "android", target_arch = "wasm32"))]
        Format::Rar => return Err(crate::i18n::t("RAR no está soportado en Android").into()),
        #[cfg(not(any(target_os = "android", target_arch = "wasm32")))]
        Format::Rar => {
            let ar = unrar::Archive::new(path).open_for_listing().map_err(|e| e.to_string())?;
            let end = Instant::now() + budget;
            for e in ar {
                let Ok(e) = e else { break };
                if items.len() >= max || Instant::now() >= end {
                    truncated = true;
                    break;
                }
                items.push(Item { name: e.filename.to_string_lossy().into_owned(), size: e.unpacked_size, dir: e.is_directory() });
            }
        }
        Format::Tar | Format::TarGz => {
            let f = File::open(path).map_err(|e| e.to_string())?;
            let r = Deadline { inner: BufReader::new(f), end: Instant::now() + budget };
            let rd: Box<dyn Read> = if fmt == Format::TarGz { Box::new(flate2::read::GzDecoder::new(r).take(MAX_EXPANDED)) } else { Box::new(r.take(MAX_EXPANDED)) };
            let mut ar = tar::Archive::new(rd);
            let ents = ar.entries().map_err(|e| e.to_string())?;
            for e in ents {
                let Ok(e) = e else {
                    truncated = true;
                    break;
                };
                if items.len() >= max {
                    truncated = true;
                    break;
                }
                let name = e.path().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
                items.push(Item { name, size: e.size(), dir: e.header().entry_type().is_dir() });
            }
        }
    }
    Ok(Listing { items, truncated })
}

/// Extrae una sola entrada (`entry`, tal como la da `list`) dentro de `dst` y devuelve su ruta.
/// Rechaza entradas mayores que `max` bytes o con rutas que escapan de `dst`.
pub fn read_entry(src: &Path, entry: &str, dst: &Path, max: u64) -> Result<std::path::PathBuf, String> {
    let fmt = detect(src).ok_or(crate::i18n::t("formato no soportado"))?;
    if !safe(Path::new(entry)) {
        return Err(crate::i18n::t("ruta no válida").into());
    }
    crate::fsys::private_dir(dst);
    let out = dst.join(entry);
    if let Some(d) = out.parent() {
        fs::create_dir_all(d).map_err(|e| e.to_string())?;
    }
    let too_big = || crate::i18n::t("el archivo es demasiado grande para la vista previa").to_string();
    let copy_limited = |r: &mut dyn Read, size: u64| -> Result<(), String> {
        if size > max {
            return Err(too_big());
        }
        let mut f = File::create(&out).map_err(|e| e.to_string())?;
        io::copy(&mut r.take(max), &mut f).map_err(|e| e.to_string())?;
        Ok(())
    };
    match fmt {
        Format::Zip => {
            let f = File::open(src).map_err(|e| e.to_string())?;
            let mut ar = zip::ZipArchive::new(BufReader::new(f)).map_err(|e| e.to_string())?;
            let mut z = ar.by_name(entry).map_err(|e| e.to_string())?;
            let size = z.size();
            copy_limited(&mut z, size)?;
        }
        Format::Tar | Format::TarGz => {
            let f = BufReader::new(File::open(src).map_err(|e| e.to_string())?);
            let rd: Box<dyn Read> = if fmt == Format::TarGz { Box::new(flate2::read::GzDecoder::new(f)) } else { Box::new(f) };
            let mut ar = tar::Archive::new(rd);
            let mut found = false;
            for e in ar.entries().map_err(|e| e.to_string())? {
                let mut e = e.map_err(|e| e.to_string())?;
                if e.path().map(|p| p.to_string_lossy() == entry).unwrap_or(false) {
                    let size = e.size();
                    copy_limited(&mut e, size)?;
                    found = true;
                    break;
                }
            }
            if !found {
                return Err(crate::i18n::t("no se encontró la entrada").into());
            }
        }
        Format::SevenZ => {
            #[cfg(target_arch = "wasm32")]
            return Err(crate::i18n::t("7z no está disponible en la web").into());
            #[cfg(not(target_arch = "wasm32"))]
            {
            sevenz_header_ok(src)?;
            let ar = sevenz_rust2::Archive::open(src).map_err(|e| e.to_string())?;
            if ar.files.iter().any(|f| f.name() == entry && f.size() > max) {
                return Err(too_big());
            }
            let f = File::open(src).map_err(|e| e.to_string())?;
            sevenz_rust2::decompress_with_extract_fn(f, dst, |e, rd, dest| {
                if e.name() == entry {
                    sevenz_rust2::default_entry_extract_fn(e, rd, dest)
                } else {
                    Ok(true)
                }
            })
            .map_err(|e| e.to_string())?;
            }
        }
        #[cfg(any(target_os = "android", target_arch = "wasm32"))]
        Format::Rar => return Err(crate::i18n::t("RAR no está soportado en Android").into()),
        #[cfg(not(any(target_os = "android", target_arch = "wasm32")))]
        Format::Rar => {
            let mut cur = unrar::Archive::new(src).open_for_processing().map_err(|e| e.to_string())?;
            loop {
                let Some(h) = cur.read_header().map_err(|e| e.to_string())? else { return Err(crate::i18n::t("no se encontró la entrada").into()) };
                let e = h.entry();
                if e.filename.to_string_lossy() == entry {
                    if e.is_encrypted() {
                        return Err(crate::i18n::t("el archivo está protegido con contraseña").into());
                    }
                    if e.unpacked_size > max {
                        return Err(too_big());
                    }
                    h.extract_with_base(dst).map_err(|e| e.to_string())?;
                    break;
                }
                cur = h.skip().map_err(|e| e.to_string())?;
            }
        }
    }
    if out.v_is_file() { Ok(out) } else { Err(crate::i18n::t("no se pudo extraer").into()) }
}

/// Extrae todo `src` dentro de `dst`. Devuelve los nombres de primer nivel.
/// Tope de bytes descomprimidos por extracción: por encima se trata como una bomba de descompresión.
pub const MAX_EXPANDED: u64 = 64 << 30;

/// Extrae todo `src` dentro de `dst`. Devuelve los nombres de primer nivel.
///
/// Si falla o se cancela y `dst` no existía, se borra entero: nunca queda una carpeta a medias que
/// parezca una extracción buena (la carpeta es siempre nueva; el reintento con contraseña la vuelve
/// a crear). Si `dst` ya existía, se deja como esté.
pub fn extract_all(src: &Path, dst: &Path, p: &Progress, ctx: &egui::Context, pass: Option<&str>) -> Result<Vec<String>, String> {
    let fresh = fs::symlink_metadata(dst).is_err();
    let r = extract_inner(src, dst, p, ctx, pass);
    if r.is_err() && fresh {
        let _ = fs::remove_dir_all(dst);
    }
    r
}

fn extract_inner(src: &Path, dst: &Path, p: &Progress, ctx: &egui::Context, pass: Option<&str>) -> Result<Vec<String>, String> {
    let fmt = detect(src).ok_or(crate::i18n::t("formato no soportado"))?;
    if fmt == Format::Zip {
        return crate::tools::unzip_to(src, dst, p, ctx, pass);
    }
    fs::create_dir_all(dst).map_err(|e| e.to_string())?;
    let size = fs::metadata(src).map(|m| m.len()).unwrap_or(0);
    p.total.store(size.max(1), AO::Relaxed);
    let open = || File::open(src).map_err(|e| e.to_string());
    match fmt {
        Format::Tar | Format::TarGz => {
            let r = Counting { inner: BufReader::new(open()?), p };
            // tope de bytes descomprimidos: un .tar.gz diminuto no puede llenar el disco
            let rd: Box<dyn Read> = if fmt == Format::TarGz { Box::new(flate2::read::GzDecoder::new(r).take(MAX_EXPANDED)) } else { Box::new(r.take(MAX_EXPANDED)) };
            let mut ar = tar::Archive::new(rd);
            ar.set_preserve_permissions(false);
            let root = fs::canonicalize(dst).map_err(|e| e.to_string())?;
            // las carpetas se crean escribibles y su modo se aplica al final: una carpeta 0555
            // impediría crear lo que contiene
            #[cfg_attr(not(unix), allow(unused_mut, unused_variables))]
            let mut modes: Vec<(std::path::PathBuf, u32)> = Vec::new();
            let mut n = 0u32;
            for e in ar.entries().map_err(|e| e.to_string())? {
                let mut e = e.map_err(|e| e.to_string())?;
                let rel = e.path().map_err(|e| e.to_string())?.into_owned();
                let shown = rel.to_string_lossy().into_owned();
                let Some(out) = tar_dest(dst, &rel) else { continue };
                let kind = e.header().entry_type();
                if kind.is_dir() {
                    if out != dst {
                        if let Some(par) = out.parent() {
                            fs::create_dir_all(par).map_err(|e| format!("{shown}: {e}"))?;
                            if !inside(&root, par) {
                                note_outside(p, &shown);
                                continue;
                            }
                        }
                        if let Err(err) = fs::create_dir(&out) {
                            if !fs::symlink_metadata(&out).is_ok_and(|m| m.v_is_dir()) {
                                return Err(format!("{shown}: {err}"));
                            }
                        }
                        if let Ok(m) = e.header().mode() {
                            modes.push((out, m & 0o777));
                        }
                    }
                } else {
                    if kind.is_symlink() {
                        let target = e.link_name().ok().flatten().map(|t| t.into_owned()).unwrap_or_default();
                        let par = out.parent().map(|q| fs::canonicalize(q).unwrap_or_else(|_| q.to_path_buf())).unwrap_or_default();
                        if !link_ok(&root, &par, &target) {
                            note_outside(p, &shown);
                            continue;
                        }
                    }
                    // unpack_in descarta rutas que escapan de `dst` (también a través de enlaces)
                    e.unpack_in(dst).map_err(|e| e.to_string())?;
                }
                n += 1;
                if n % 64 == 0 {
                    ctx.request_repaint();
                }
            }
            // modos finales, de lo más profundo a la raíz (nunca setuid/setgid: sólo rwx)
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                modes.sort_by_key(|(q, _)| std::cmp::Reverse(q.components().count()));
                for (q, m) in modes {
                    let _ = fs::set_permissions(&q, fs::Permissions::from_mode(m));
                }
            }
        }
        Format::SevenZ => {
            #[cfg(target_arch = "wasm32")]
            return Err(crate::i18n::t("7z no está disponible en la web").into());
            #[cfg(not(target_arch = "wasm32"))]
            {
            sevenz_header_ok(src)?;
            let mut total = 0u64;
            let r = Counting { inner: BufReader::new(open()?), p };
            sevenz_rust2::decompress_with_extract_fn_and_password(r, dst, sevenz_rust2::Password::from(pass.unwrap_or("")), |entry, rd, dest| {
                if p.cancelled() {
                    return Ok(false);
                }
                // un nombre absoluto o con `..` escaparía de `dst` (PathBuf::join lo sustituiría entero)
                if !safe(Path::new(entry.name())) {
                    return Ok(true);
                }
                total = total.saturating_add(entry.size());
                if total > MAX_EXPANDED {
                    return Err(sevenz_rust2::Error::Other(crate::i18n::t("el contenido descomprimido supera el límite permitido").into()));
                }
                sevenz_rust2::default_entry_extract_fn(entry, rd, dest)
            })
            .map_err(|e| match e {
                sevenz_rust2::Error::PasswordRequired => if pass.is_some() { BAD_PASS.to_string() } else { NEED_PASS.to_string() },
                sevenz_rust2::Error::MaybeBadPassword(_) => BAD_PASS.to_string(),
                e => pw_msg(e.to_string(), pass.is_some()),
            })?;
            if p.cancelled() {
                return Err(crate::i18n::t("cancelado").into());
            }
            }
        }
        #[cfg(any(target_os = "android", target_arch = "wasm32"))]
        Format::Rar => return Err(crate::i18n::t("RAR no está soportado en Android").into()),
        #[cfg(not(any(target_os = "android", target_arch = "wasm32")))]
        Format::Rar => {
            let pw = pass.unwrap_or("");
            let arch = if pass.is_some() { unrar::Archive::with_password(src, pw) } else { unrar::Archive::new(src) };
            let mut cur = arch.open_for_processing().map_err(|e| pw_msg(e.to_string(), pass.is_some()))?;
            let mut done = 0u64;
            loop {
                if p.cancelled() {
                    return Err(crate::i18n::t("cancelado").into());
                }
                let Some(h) = cur.read_header().map_err(|e| pw_msg(e.to_string(), pass.is_some()))? else { break };
                let e = h.entry();
                if e.is_encrypted() && pass.is_none() {
                    return Err(NEED_PASS.into());
                }
                let ok = safe(&e.filename);
                done = done.saturating_add(e.unpacked_size);
                if done > MAX_EXPANDED {
                    return Err(crate::i18n::t("el contenido descomprimido supera el límite permitido").into());
                }
                cur = if ok && e.is_file() {
                    h.extract_with_base(dst).map_err(|e| pw_msg(e.to_string(), pass.is_some()))?
                } else if ok && e.is_directory() {
                    let _ = fs::create_dir_all(dst.join(&e.filename));
                    h.skip().map_err(|e| e.to_string())?
                } else {
                    h.skip().map_err(|e| e.to_string())?
                };
                p.done.store(done.min(size), AO::Relaxed);
                ctx.request_repaint();
            }
        }
        Format::Zip => unreachable!(),
    }
    let mut top: Vec<String> = fs::read_dir(dst)
        .map(|rd| rd.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect())
        .unwrap_or_default();
    top.sort();
    Ok(top)
}

// ---------------------------------------------------------------------------
// Caché de extracción para navegar dentro de un comprimido
// ---------------------------------------------------------------------------

/// Carpeta común de las extracciones; dentro, una por proceso (`p<pid>`): cada ventana es un proceso
/// y, con una carpeta compartida, al cerrarse una borraría lo que otra está viendo.
fn cache_base() -> Option<std::path::PathBuf> {
    Some(dirs::cache_dir()?.join("filebrowser-rs").join("archives"))
}

/// Extracciones de este proceso. La carpeta guarda un candado tomado mientras el proceso viva, para
/// que la limpieza de otro (`clean_cache`) no la toque. `None` si no se pudo preparar.
#[cfg(not(target_arch = "wasm32"))]
pub fn cache_root() -> Option<std::path::PathBuf> {
    static ROOT: std::sync::OnceLock<Option<(std::path::PathBuf, std::fs::File)>> = std::sync::OnceLock::new();
    ROOT.get_or_init(|| claim_cache(&cache_base()?, crate::vfs::pid())).as_ref().map(|(d, _)| d.clone())
}

#[cfg(target_arch = "wasm32")]
pub fn cache_root() -> Option<std::path::PathBuf> {
    None
}

#[cfg(not(target_arch = "wasm32"))]
fn claim_cache(base: &Path, pid: u32) -> Option<(std::path::PathBuf, std::fs::File)> {
    let dir = base.join(format!("p{pid}"));
    crate::fsys::private_dir(base);
    crate::fsys::private_dir(&dir);
    let f = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(dir.join(".lock")).ok()?;
    f.try_lock().ok()?;
    Some((dir, f))
}

/// Al arrancar (en un hilo): borra las extracciones de los procesos que ya terminaron (su candado
/// está libre) y las del formato anterior (sin carpeta por proceso) de más de un día.
pub fn clean_cache() {
    if let Some(base) = cache_base() {
        clean_cache_in(&base, &format!("p{}", crate::vfs::pid()), Duration::from_secs(24 * 3600));
    }
}

fn clean_cache_in(base: &Path, own: &str, max_age: Duration) {
    let Ok(rd) = fs::read_dir(base) else { return };
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if name == own {
            continue;
        }
        let p = e.path();
        let is_dir = e.file_type().map(|t| t.v_is_dir()).unwrap_or(false);
        let per_process = is_dir && name.strip_prefix('p').is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()));
        if per_process {
            if lock_free(&p) {
                let _ = fs::remove_dir_all(&p);
            }
            continue;
        }
        let old = e.metadata().and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok()).map(|a| a >= max_age).unwrap_or(false);
        if old {
            let _ = if is_dir { fs::remove_dir_all(&p) } else { fs::remove_file(&p) };
        }
    }
}

/// ¿Nadie tiene tomado el candado de esa carpeta de proceso? (sin candado: de un cierre a medias)
#[cfg(not(target_arch = "wasm32"))]
fn lock_free(dir: &Path) -> bool {
    match std::fs::OpenOptions::new().write(true).open(dir.join(".lock")) {
        Ok(f) => f.try_lock().is_ok(),
        Err(_) => true,
    }
}

#[cfg(target_arch = "wasm32")]
fn lock_free(_dir: &Path) -> bool {
    false
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod cache_tests {
    use super::*;

    #[test]
    fn per_process_cache_and_stale_cleanup() {
        let n = web_time::SystemTime::now().duration_since(web_time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
        let base = crate::vfs::temp_dir().join(format!("fbw3-arch-{}-{n:x}", crate::vfs::pid()));
        // dos «procesos» vivos (candado tomado) y uno que murió sin limpiar
        let (mine, lk) = claim_cache(&base, 11).unwrap();
        let (other, olk) = claim_cache(&base, 22).unwrap();
        assert_eq!(mine, base.join("p11"));
        assert!(claim_cache(&base, 11).is_none(), "el candado es exclusivo");
        let (dead, dlk) = claim_cache(&base, 33).unwrap();
        drop(dlk);
        for d in [&mine, &other, &dead] {
            std::fs::create_dir_all(d.join("abc")).unwrap();
            std::fs::write(d.join("abc.done"), b"").unwrap();
        }
        // carpeta de proceso sin candado (cierre a medias) y restos del formato anterior
        std::fs::create_dir_all(base.join("p44/x")).unwrap();
        std::fs::create_dir_all(base.join("0123456789abcdef")).unwrap();
        std::fs::write(base.join("0123456789abcdef.done"), b"").unwrap();
        std::fs::create_dir_all(base.join("pcfg")).unwrap();
        // limpieza que haría el proceso 11, con todo lo del formato anterior ya caducado
        clean_cache_in(&base, "p11", Duration::ZERO);
        assert!(mine.join("abc").is_dir(), "lo propio no se toca");
        assert!(other.join("abc").is_dir(), "lo de otra ventana viva no se toca");
        assert!(!dead.exists(), "lo de un proceso terminado se borra");
        assert!(!base.join("p44").exists());
        assert!(!base.join("0123456789abcdef").exists() && !base.join("0123456789abcdef.done").exists());
        assert!(!base.join("pcfg").exists(), "no es carpeta de proceso: se trata como resto antiguo");
        // sin caducar, los restos del formato anterior se conservan
        std::fs::create_dir_all(base.join("fresh")).unwrap();
        clean_cache_in(&base, "p11", Duration::from_secs(3600));
        assert!(base.join("fresh").is_dir());
        drop((lk, olk));
        let _ = std::fs::remove_dir_all(&base);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn tmp(name: &str) -> std::path::PathBuf {
        let d = crate::vfs::temp_dir().join(format!("fb-arch-{}-{name}", crate::vfs::pid()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn sample(dir: &Path) {
        fs::create_dir_all(dir.join("sub")).unwrap();
        fs::write(dir.join("a.txt"), "hola").unwrap();
        fs::write(dir.join("sub/b.txt"), "adios").unwrap();
    }

    fn check(arch: &Path) {
        let l = list(arch, 100, Duration::from_secs(5)).unwrap();
        assert!(l.items.iter().any(|i| i.name.ends_with("a.txt") && i.size == 4), "listado de {arch:?}");
        let out = arch.with_extension("out");
        let p = Progress::default();
        extract_all(arch, &out, &p, &egui::Context::default(), None).unwrap();
        assert_eq!(fs::read_to_string(out.join("a.txt")).unwrap(), "hola", "{arch:?}");
        assert_eq!(fs::read_to_string(out.join("sub/b.txt")).unwrap(), "adios", "{arch:?}");
    }

    #[test]
    fn zip_tar_targz_7z() {
        let d = tmp("all");
        let src = d.join("src");
        sample(&src);
        // zip
        let zp = d.join("t.zip");
        {
            let mut zw = zip::ZipWriter::new(File::create(&zp).unwrap());
            let o = zip::write::SimpleFileOptions::default();
            zw.start_file("a.txt", o).unwrap();
            zw.write_all(b"hola").unwrap();
            zw.start_file("sub/b.txt", o).unwrap();
            zw.write_all(b"adios").unwrap();
            zw.finish().unwrap();
        }
        check(&zp);
        // tar y tar.gz
        let tp = d.join("t.tar");
        {
            let mut b = tar::Builder::new(File::create(&tp).unwrap());
            b.append_dir_all(".", &src).unwrap();
            b.finish().unwrap();
        }
        check(&tp);
        let gp = d.join("t.tar.gz");
        {
            let gz = flate2::write::GzEncoder::new(File::create(&gp).unwrap(), flate2::Compression::fast());
            let mut b = tar::Builder::new(gz);
            b.append_dir_all(".", &src).unwrap();
            b.finish().unwrap();
        }
        check(&gp);
        // 7z
        let sp = d.join("t.7z");
        sevenz_rust2::compress_to_path(&src, &sp).unwrap();
        check(&sp);
        let _ = fs::remove_dir_all(&d);
    }
}

#[cfg(test)]
mod hostile_tests {
    use super::*;
    use std::io::Write;

    fn tmp(tag: &str) -> std::path::PathBuf {
        let n = web_time::SystemTime::now().duration_since(web_time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
        let d = crate::vfs::temp_dir().join(format!("fbw1-arch-{tag}-{}-{n:x}", crate::vfs::pid()));
        fs::create_dir_all(&d).unwrap();
        d
    }

    /// Todo lo que hay bajo `d` (sin seguir enlaces), como rutas relativas.
    fn all(d: &Path) -> Vec<String> {
        fn go(base: &Path, d: &Path, out: &mut Vec<String>) {
            for e in fs::read_dir(d).unwrap().flatten() {
                let p = e.path();
                out.push(p.strip_prefix(base).unwrap().to_string_lossy().into_owned());
                if fs::symlink_metadata(&p).is_ok_and(|m| m.is_dir()) {
                    go(base, &p, out);
                }
            }
        }
        let mut v = Vec::new();
        go(d, d, &mut v);
        v.sort();
        v
    }

    fn raw_name(h: &mut tar::Header, name: &[u8]) {
        let n = &mut h.as_old_mut().name;
        n.fill(0);
        n[..name.len()].copy_from_slice(name);
    }

    fn raw_link(h: &mut tar::Header, name: &[u8]) {
        let n = &mut h.as_old_mut().linkname;
        n.fill(0);
        n[..name.len()].copy_from_slice(name);
    }

    #[test]
    fn hostile_zip_stays_inside() {
        let d = tmp("hzip");
        let zp = d.join("h.zip");
        {
            let mut zw = zip::ZipWriter::new(File::create(&zp).unwrap());
            let o = zip::write::SimpleFileOptions::default();
            for name in ["../evil.txt", "/abs_evil.txt", "ok.txt"] {
                zw.start_file(name, o).unwrap();
                zw.write_all(b"x").unwrap();
            }
            // enlace que sale de la carpeta y un archivo escrito «a través» de él
            zw.add_symlink("lnk", "../../outside", o).unwrap();
            zw.start_file("lnk/x.txt", o).unwrap();
            zw.write_all(b"x").unwrap();
            zw.add_symlink("abs", "/etc", o).unwrap();
            // enlace legítimo dentro de la carpeta
            zw.add_symlink("good", "ok.txt", o).unwrap();
            zw.finish().unwrap();
        }
        let out = d.join("box").join("out");
        fs::create_dir_all(out.parent().unwrap()).unwrap();
        let p = Progress::default();
        let _ = extract_all(&zp, &out, &p, &egui::Context::default(), None);
        // nada fuera de `out`
        let mut outside = all(&d);
        outside.retain(|n| !n.starts_with("box/out"));
        assert_eq!(outside, vec!["box", "h.zip"]);
        assert!(out.join("ok.txt").is_file());
        assert!(fs::symlink_metadata(out.join("abs")).is_err(), "enlace absoluto descartado");
        assert!(fs::symlink_metadata(out.join("lnk")).map(|m| !m.file_type().is_symlink()).unwrap_or(true));
        #[cfg(unix)]
        assert_eq!(fs::read_link(out.join("good")).unwrap(), Path::new("ok.txt"));
        assert!(!p.errors.lock().unwrap().is_empty(), "lo omitido se avisa");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn hostile_tar_stays_inside() {
        let d = tmp("htar");
        let tp = d.join("h.tar");
        {
            let mut b = tar::Builder::new(File::create(&tp).unwrap());
            let file = |b: &mut tar::Builder<File>, name: &[u8]| {
                let mut h = tar::Header::new_old();
                raw_name(&mut h, name);
                h.set_size(1);
                h.set_mode(0o644);
                h.set_entry_type(tar::EntryType::Regular);
                h.set_cksum();
                b.append(&h, &b"x"[..]).unwrap();
            };
            file(&mut b, b"../evil.txt");
            file(&mut b, b"ok.txt");
            let link = |b: &mut tar::Builder<File>, name: &[u8], target: &[u8]| {
                let mut h = tar::Header::new_old();
                raw_name(&mut h, name);
                raw_link(&mut h, target);
                h.set_size(0);
                h.set_mode(0o777);
                h.set_entry_type(tar::EntryType::Symlink);
                h.set_cksum();
                b.append(&h, std::io::empty()).unwrap();
            };
            link(&mut b, b"lnk", b"../../outside");
            link(&mut b, b"abs", b"/etc");
            link(&mut b, b"sneaky", b"sub/../../..");
            link(&mut b, b"good", b"ok.txt");
            file(&mut b, b"lnk/x.txt");
            file(&mut b, b"abs/x.txt");
            b.finish().unwrap();
        }
        let out = d.join("box").join("out");
        fs::create_dir_all(out.parent().unwrap()).unwrap();
        let p = Progress::default();
        let _ = extract_all(&tp, &out, &p, &egui::Context::default(), None);
        let mut outside = all(&d);
        outside.retain(|n| !n.starts_with("box/out"));
        assert_eq!(outside, vec!["box", "h.tar"]);
        assert!(out.join("ok.txt").is_file());
        for l in ["lnk", "abs", "sneaky"] {
            assert!(fs::symlink_metadata(out.join(l)).map(|m| !m.file_type().is_symlink()).unwrap_or(true), "{l}");
        }
        #[cfg(unix)]
        assert_eq!(fs::read_link(out.join("good")).unwrap(), Path::new("ok.txt"));
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn link_ok_rules() {
        let r = Path::new("/r");
        assert!(link_ok(r, Path::new("/r/a"), Path::new("../b")));
        assert!(link_ok(r, Path::new("/r/a"), Path::new("x/y")));
        assert!(!link_ok(r, Path::new("/r/a"), Path::new("../../b")));
        assert!(!link_ok(r, Path::new("/r"), Path::new("/etc")));
        assert!(!link_ok(r, Path::new("/r/a"), Path::new("x/../../..")));
        assert!(!link_ok(r, Path::new("/r/a"), Path::new("")));
    }

    /// Una carpeta de sólo lectura (0555) no impide extraer lo que contiene; su modo se aplica al final.
    #[cfg(unix)]
    #[test]
    fn tar_readonly_dir() {
        use std::os::unix::fs::PermissionsExt;
        let d = tmp("ro");
        let tp = d.join("ro.tar");
        {
            let mut b = tar::Builder::new(File::create(&tp).unwrap());
            let mut h = tar::Header::new_gnu();
            h.set_path("ro/").unwrap();
            h.set_entry_type(tar::EntryType::Directory);
            h.set_mode(0o555);
            h.set_size(0);
            h.set_cksum();
            b.append(&h, std::io::empty()).unwrap();
            let mut h = tar::Header::new_gnu();
            h.set_path("ro/sub/f.txt").unwrap();
            h.set_mode(0o444);
            h.set_size(4);
            h.set_cksum();
            b.append(&h, &b"hola"[..]).unwrap();
            b.finish().unwrap();
        }
        let out = d.join("out");
        extract_all(&tp, &out, &Progress::default(), &egui::Context::default(), None).unwrap();
        assert_eq!(fs::read(out.join("ro/sub/f.txt")).unwrap(), b"hola");
        assert_eq!(fs::metadata(out.join("ro")).unwrap().permissions().mode() & 0o777, 0o555);
        let _ = fs::set_permissions(out.join("ro"), fs::Permissions::from_mode(0o755));
        let _ = fs::remove_dir_all(&d);
    }

    /// Una extracción que falla a mitad no deja una carpeta nueva a medio llenar.
    #[test]
    fn failed_extraction_cleans_up() {
        let d = tmp("trunc");
        let tp = d.join("t.tar.gz");
        {
            let gz = flate2::write::GzEncoder::new(File::create(&tp).unwrap(), flate2::Compression::new(1));
            let mut b = tar::Builder::new(gz);
            for i in 0..20 {
                let data: Vec<u8> = (0..50_000u32).map(|x| (x.wrapping_mul(2654435761).wrapping_add(i) >> 13) as u8).collect();
                let mut h = tar::Header::new_gnu();
                h.set_path(format!("f{i}.bin")).unwrap();
                h.set_size(data.len() as u64);
                h.set_mode(0o644);
                h.set_cksum();
                b.append(&h, &data[..]).unwrap();
            }
            b.into_inner().unwrap().finish().unwrap();
        }
        let full = fs::read(&tp).unwrap();
        fs::write(&tp, &full[..full.len() / 2]).unwrap();
        let out = d.join("out");
        assert!(extract_all(&tp, &out, &Progress::default(), &egui::Context::default(), None).is_err());
        assert!(fs::symlink_metadata(&out).is_err(), "carpeta a medias retirada");
        // si la carpeta ya existía, se deja
        fs::create_dir_all(&out).unwrap();
        fs::write(out.join("mio.txt"), b"m").unwrap();
        assert!(extract_all(&tp, &out, &Progress::default(), &egui::Context::default(), None).is_err());
        assert!(out.join("mio.txt").exists());
        let _ = fs::remove_dir_all(&d);
    }
}
