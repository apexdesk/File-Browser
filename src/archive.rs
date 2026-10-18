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
    f.read_exact(&mut h).map_err(|_| "archivo 7z no válido".to_string())?;
    if h[..6] != [b'7', b'z', 0xBC, 0xAF, 0x27, 0x1C] {
        return Err("archivo 7z no válido".into());
    }
    let off = u64::from_le_bytes(h[12..20].try_into().unwrap());
    let size = u64::from_le_bytes(h[20..28].try_into().unwrap());
    if size > 0 && (size > 512 << 20 || off.checked_add(32).and_then(|x| x.checked_add(size)).map(|e| e > len).unwrap_or(true)) {
        return Err("cabecera 7z dañada".into());
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
            return Err(io::Error::other("cancelado"));
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

/// Índice del contenido sin extraer. Para en `max` entradas o al agotar `budget`.
pub fn list(path: &Path, max: usize, budget: Duration) -> Result<Listing, String> {
    let fmt = detect(path).ok_or("formato no soportado")?;
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
        Format::Rar => return Err("RAR no está soportado en Android".into()),
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
    let fmt = detect(src).ok_or("formato no soportado")?;
    if !safe(Path::new(entry)) {
        return Err("ruta no válida".into());
    }
    crate::fsys::private_dir(dst);
    let out = dst.join(entry);
    if let Some(d) = out.parent() {
        fs::create_dir_all(d).map_err(|e| e.to_string())?;
    }
    let too_big = || "el archivo es demasiado grande para la vista previa".to_string();
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
                return Err("no se encontró la entrada".into());
            }
        }
        Format::SevenZ => {
            #[cfg(target_arch = "wasm32")]
            return Err("7z no está disponible en la web".into());
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
        Format::Rar => return Err("RAR no está soportado en Android".into()),
        #[cfg(not(any(target_os = "android", target_arch = "wasm32")))]
        Format::Rar => {
            let mut cur = unrar::Archive::new(src).open_for_processing().map_err(|e| e.to_string())?;
            loop {
                let Some(h) = cur.read_header().map_err(|e| e.to_string())? else { return Err("no se encontró la entrada".into()) };
                let e = h.entry();
                if e.filename.to_string_lossy() == entry {
                    if e.is_encrypted() {
                        return Err("el archivo está protegido con contraseña".into());
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
    if out.v_is_file() { Ok(out) } else { Err("no se pudo extraer".into()) }
}

/// Extrae todo `src` dentro de `dst`. Devuelve los nombres de primer nivel.
/// Tope de bytes descomprimidos por extracción: por encima se trata como una bomba de descompresión.
pub const MAX_EXPANDED: u64 = 64 << 30;

pub fn extract_all(src: &Path, dst: &Path, p: &Progress, ctx: &egui::Context, pass: Option<&str>) -> Result<Vec<String>, String> {
    let fmt = detect(src).ok_or("formato no soportado")?;
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
            let mut n = 0u32;
            for e in ar.entries().map_err(|e| e.to_string())? {
                let mut e = e.map_err(|e| e.to_string())?;
                // unpack_in descarta rutas que escapan de `dst`
                e.unpack_in(dst).map_err(|e| e.to_string())?;
                n += 1;
                if n % 64 == 0 {
                    ctx.request_repaint();
                }
            }
        }
        Format::SevenZ => {
            #[cfg(target_arch = "wasm32")]
            return Err("7z no está disponible en la web".into());
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
                    return Err(sevenz_rust2::Error::Other("el contenido descomprimido supera el límite permitido".into()));
                }
                sevenz_rust2::default_entry_extract_fn(entry, rd, dest)
            })
            .map_err(|e| match e {
                sevenz_rust2::Error::PasswordRequired => if pass.is_some() { BAD_PASS.to_string() } else { NEED_PASS.to_string() },
                sevenz_rust2::Error::MaybeBadPassword(_) => BAD_PASS.to_string(),
                e => pw_msg(e.to_string(), pass.is_some()),
            })?;
            if p.cancelled() {
                return Err("cancelado".into());
            }
            }
        }
        #[cfg(any(target_os = "android", target_arch = "wasm32"))]
        Format::Rar => return Err("RAR no está soportado en Android".into()),
        #[cfg(not(any(target_os = "android", target_arch = "wasm32")))]
        Format::Rar => {
            let pw = pass.unwrap_or("");
            let arch = if pass.is_some() { unrar::Archive::with_password(src, pw) } else { unrar::Archive::new(src) };
            let mut cur = arch.open_for_processing().map_err(|e| pw_msg(e.to_string(), pass.is_some()))?;
            let mut done = 0u64;
            loop {
                if p.cancelled() {
                    return Err("cancelado".into());
                }
                let Some(h) = cur.read_header().map_err(|e| pw_msg(e.to_string(), pass.is_some()))? else { break };
                let e = h.entry();
                if e.is_encrypted() && pass.is_none() {
                    return Err(NEED_PASS.into());
                }
                let ok = safe(&e.filename);
                done = done.saturating_add(e.unpacked_size);
                if done > MAX_EXPANDED {
                    return Err("el contenido descomprimido supera el límite permitido".into());
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
