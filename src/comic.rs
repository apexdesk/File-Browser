//! Cómics en CBZ (zip) y CBR (rar): una página por imagen, en orden natural de nombre.
#[allow(unused_imports)]
use crate::vfs::{KindV as _, PathV as _};
use crate::docview::Pages;
use crate::thumbs::{decode_jpeg_from, Decoded};
use crate::vfs::fs::File;
use std::io::{BufReader, Cursor, Read};
use std::path::{Path, PathBuf};

/// Una imagen mayor que esto no se decodifica.
const MAX_PAGE_BYTES: u64 = 96 << 20;

pub fn is_comic_ext(e: &str) -> bool {
    matches!(e.to_ascii_lowercase().as_str(), "cbz" | "cbr")
}

fn is_image_name(n: &str) -> bool {
    let n = n.to_ascii_lowercase();
    let base = n.rsplit(['/', '\\']).next().unwrap_or(&n);
    !base.starts_with('.')
        && !n.contains("__macosx")
        && matches!(base.rsplit_once('.').map(|x| x.1), Some("jpg" | "jpeg" | "png" | "gif" | "webp" | "bmp" | "tif" | "tiff"))
}

enum Src {
    Zip(zip::ZipArchive<BufReader<File>>),
    /// páginas extraídas a una carpeta temporal (RAR no permite acceso directo)
    #[cfg_attr(any(target_os = "android", target_arch = "wasm32"), allow(dead_code))]
    Dir(PathBuf),
}

pub struct Comic {
    src: Src,
    names: Vec<String>,
}

impl Drop for Comic {
    fn drop(&mut self) {
        if let Src::Dir(d) = &self.src {
            let _ = crate::vfs::fs::remove_dir_all(d);
        }
    }
}

fn is_rar(path: &Path) -> bool {
    let mut m = [0u8; 4];
    File::open(path).and_then(|mut f| f.read_exact(&mut m)).is_ok() && &m == b"Rar!"
}

#[allow(dead_code)]
pub fn open(path: &Path) -> Result<Comic, String> {
    open_inner(path, false, &|| false)
}

/// Como `open`; `cancel` corta la extracción de un CBR cuando el visor ya no hace falta.
pub fn open_cancel(path: &Path, cancel: &dyn Fn() -> bool) -> Result<Comic, String> {
    open_inner(path, false, cancel)
}

fn open_inner(path: &Path, only_first: bool, cancel: &dyn Fn() -> bool) -> Result<Comic, String> {
    let t = crate::i18n::t;
    if is_rar(path) {
        return open_rar(path, only_first, cancel);
    }
    let f = File::open(path).map_err(|_| t("No se pudo abrir").to_string())?;
    let ar = zip::ZipArchive::new(BufReader::new(f)).map_err(|_| t("No se pudo mostrar el documento").to_string())?;
    let mut ar = ar;
    let mut names: Vec<String> = (0..ar.len()).filter_map(|i| ar.by_index_raw(i).ok().and_then(|f| f.name().ok().map(|n| n.into_owned()))).filter(|n| is_image_name(n)).collect();
    names.sort_by(|a, b| crate::fsys::natural_cmp(a, b));
    if names.is_empty() {
        return Err(t("No se pudo mostrar el documento").into());
    }
    Ok(Comic { src: Src::Zip(ar), names })
}

#[cfg(any(target_os = "android", target_arch = "wasm32"))]
fn open_rar(_: &Path, _: bool, _: &dyn Fn() -> bool) -> Result<Comic, String> {
    Err(crate::i18n::t("No se pudo mostrar el documento").into())
}

/// Borra la carpeta de extracción si no se llega a entregar (error, cancelación o pánico).
#[cfg(not(any(target_os = "android", target_arch = "wasm32")))]
struct DirGuard(Option<PathBuf>);

#[cfg(not(any(target_os = "android", target_arch = "wasm32")))]
impl Drop for DirGuard {
    fn drop(&mut self) {
        if let Some(d) = self.0.take() {
            let _ = crate::vfs::fs::remove_dir_all(d);
        }
    }
}

#[cfg(not(any(target_os = "android", target_arch = "wasm32")))]
fn open_rar(path: &Path, only_first: bool, cancel: &dyn Fn() -> bool) -> Result<Comic, String> {
    let fail = || crate::i18n::t("No se pudo mostrar el documento").to_string();
    let mut names: Vec<String> = unrar::Archive::new(path)
        .open_for_listing()
        .map_err(|_| fail())?
        .flatten()
        .filter(|e| e.is_file() && !e.is_encrypted() && is_image_name(&e.filename.to_string_lossy()))
        .map(|e| e.filename.to_string_lossy().into_owned())
        .collect();
    names.sort_by(|a, b| crate::fsys::natural_cmp(a, b));
    if names.is_empty() {
        return Err(fail());
    }
    let stamp = web_time::SystemTime::now().duration_since(web_time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    let dir = dirs::cache_dir().unwrap_or_else(crate::vfs::temp_dir).join("filebrowser").join(format!("comic-{}-{stamp}", crate::vfs::pid()));
    crate::fsys::private_dir(&dir);
    let mut guard = DirGuard(Some(dir.clone()));
    let keep: Vec<String> = if only_first { names[..1].to_vec() } else { names.clone() };
    let mut cur = unrar::Archive::new(path).open_for_processing().map_err(|_| fail())?;
    let mut total = 0u64;
    let mut done = 0usize;
    while let Some(h) = cur.read_header().map_err(|_| fail())? {
        if cancel() {
            return Err(fail());
        }
        let name = h.entry().filename.to_string_lossy().into_owned();
        if let Some(i) = keep.iter().position(|n| *n == name).filter(|_| h.entry().is_file()) {
            // una página que declara un tamaño absurdo (bomba de descompresión) se salta en vez de cargarla entera en memoria
            if h.entry().unpacked_size > MAX_PAGE_BYTES {
                cur = h.skip().map_err(|_| fail())?;
                done += 1;
                if done == keep.len() {
                    break;
                }
                continue;
            }
            let (data, next) = h.read().map_err(|_| fail())?;
            total += data.len() as u64;
            if total > 1 << 30 {
                return Err(fail());
            }
            let k = names.iter().position(|n| *n == keep[i]).unwrap_or(i);
            let _ = crate::vfs::fs::write(dir.join(k.to_string()), data);
            cur = next;
            done += 1;
            if done == keep.len() {
                break;
            }
        } else {
            cur = h.skip().map_err(|_| fail())?;
        }
    }
    guard.0 = None;
    Ok(Comic { src: Src::Dir(dir), names })
}

impl Comic {
    fn bytes(&mut self, page: usize) -> Result<Vec<u8>, String> {
        let t = crate::i18n::t;
        let name = self.names.get(page).ok_or_else(|| t("No se pudo mostrar el documento").to_string())?.clone();
        match &mut self.src {
            Src::Zip(ar) => {
                let f = ar.by_name(&name).map_err(|e| e.to_string())?;
                if f.size() > MAX_PAGE_BYTES {
                    return Err(t("El documento es demasiado grande para mostrarlo").into());
                }
                let mut v = Vec::with_capacity(f.size() as usize);
                f.take(MAX_PAGE_BYTES).read_to_end(&mut v).map_err(|e| e.to_string())?;
                Ok(v)
            }
            Src::Dir(d) => crate::vfs::fs::read(d.join(page.to_string())).map_err(|e| e.to_string()),
        }
    }
}

/// Alto máximo de una página ya escalada (una tira larguísima no puede ser una textura enorme).
pub const PAGE_MAX_H: u32 = 8192;

/// Decodifica los bytes de una imagen con el lado mayor limitado a `width` de ancho.
pub fn decode_bytes(bytes: Vec<u8>, width: u32) -> Option<Decoded> {
    decode_fit(bytes, width, PAGE_MAX_H)
}

/// Decodifica los bytes de una imagen y la reduce para que quepa en `w`×`h`.
pub fn decode_fit(bytes: Vec<u8>, w: u32, h: u32) -> Option<Decoded> {
    let (w, h) = (w.max(1), h.max(1));
    if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        if let Some(d) = decode_jpeg_from(Cursor::new(&bytes), w.max(64).min(h.max(64)), 1) {
            return Some(d);
        }
    }
    let mut r = image::ImageReader::new(Cursor::new(&bytes)).with_guessed_format().ok()?;
    let mut lim = image::Limits::default();
    lim.max_image_width = Some(32768);
    lim.max_image_height = Some(32768);
    lim.max_alloc = Some(256 << 20);
    r.limits(lim);
    let img = r.decode().ok()?;
    let img = if img.width() > w || img.height() > h { img.resize(w, h, image::imageops::FilterType::Triangle) } else { img };
    let rgba = img.into_rgba8();
    Some(Decoded { w: rgba.width() as usize, h: rgba.height() as usize, rgba: rgba.into_raw() })
}

impl Pages for Comic {
    fn count(&self) -> usize {
        self.names.len()
    }

    fn render(&mut self, page: usize, width: u32, _cancel: &dyn Fn() -> bool) -> Result<Decoded, String> {
        let bytes = self.bytes(page)?;
        decode_bytes(bytes, width).ok_or_else(|| crate::i18n::t("No se pudo mostrar el documento").to_string())
    }
}

/// Miniatura: la primera página.
pub fn thumb(path: &Path, max: u32) -> Option<Decoded> {
    if crate::vfs::fs::metadata(path).ok()?.len() > 2 << 30 {
        return None;
    }
    let mut c = open_inner(path, true, &|| false).ok()?;
    let b = c.bytes(0).ok()?;
    decode_fit(b, max, max)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tall_strip_is_capped() {
        let img = image::RgbaImage::from_pixel(100, 20_000, image::Rgba([1, 2, 3, 255]));
        let mut png = Vec::new();
        image::DynamicImage::ImageRgba8(img).write_to(&mut Cursor::new(&mut png), image::ImageFormat::Png).unwrap();
        let d = decode_bytes(png.clone(), 900).unwrap();
        assert!(d.h <= PAGE_MAX_H as usize && d.w <= 100);
        let t = decode_fit(png, 128, 128).unwrap();
        assert!(t.w <= 128 && t.h <= 128);
    }
}
