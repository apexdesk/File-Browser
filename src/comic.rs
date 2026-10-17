//! Cómics en CBZ (zip) y CBR (rar): una página por imagen, en orden natural de nombre.
use crate::docview::Pages;
use crate::thumbs::{decode_jpeg_from, Decoded};
use std::fs::File;
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
    Dir(PathBuf),
}

pub struct Comic {
    src: Src,
    names: Vec<String>,
}

impl Drop for Comic {
    fn drop(&mut self) {
        if let Src::Dir(d) = &self.src {
            let _ = std::fs::remove_dir_all(d);
        }
    }
}

fn is_rar(path: &Path) -> bool {
    let mut m = [0u8; 4];
    File::open(path).and_then(|mut f| f.read_exact(&mut m)).is_ok() && &m == b"Rar!"
}

pub fn open(path: &Path) -> Result<Comic, String> {
    open_inner(path, false)
}

fn open_inner(path: &Path, only_first: bool) -> Result<Comic, String> {
    let t = crate::i18n::t;
    if is_rar(path) {
        return open_rar(path, only_first);
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

#[cfg(target_os = "android")]
fn open_rar(_: &Path, _: bool) -> Result<Comic, String> {
    Err(crate::i18n::t("No se pudo mostrar el documento").into())
}

#[cfg(not(target_os = "android"))]
fn open_rar(path: &Path, only_first: bool) -> Result<Comic, String> {
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
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    let dir = dirs::cache_dir().unwrap_or_else(std::env::temp_dir).join("filebrowser").join(format!("comic-{}-{stamp}", std::process::id()));
    std::fs::create_dir_all(&dir).map_err(|_| fail())?;
    let keep: Vec<String> = if only_first { names[..1].to_vec() } else { names.clone() };
    let mut cur = unrar::Archive::new(path).open_for_processing().map_err(|_| fail())?;
    let mut total = 0u64;
    let mut done = 0usize;
    while let Some(h) = cur.read_header().map_err(|_| fail())? {
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
                let _ = std::fs::remove_dir_all(&dir);
                return Err(fail());
            }
            let k = names.iter().position(|n| *n == keep[i]).unwrap_or(i);
            let _ = std::fs::write(dir.join(k.to_string()), data);
            cur = next;
            done += 1;
            if done == keep.len() {
                break;
            }
        } else {
            cur = h.skip().map_err(|_| fail())?;
        }
    }
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
            Src::Dir(d) => std::fs::read(d.join(page.to_string())).map_err(|e| e.to_string()),
        }
    }
}

/// Decodifica los bytes de una imagen con el lado mayor limitado a `width` de ancho.
pub fn decode_bytes(bytes: Vec<u8>, width: u32) -> Option<Decoded> {
    if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        if let Some(d) = decode_jpeg_from(Cursor::new(&bytes), width.max(64), 1) {
            return Some(d);
        }
    }
    let img = image::load_from_memory(&bytes).ok()?;
    let img = if img.width() > width { img.resize(width, u32::MAX, image::imageops::FilterType::Triangle) } else { img };
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
    if std::fs::metadata(path).ok()?.len() > 2 << 30 {
        return None;
    }
    let mut c = open_inner(path, true).ok()?;
    let b = c.bytes(0).ok()?;
    decode_bytes(b, max)
}
