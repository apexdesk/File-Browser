//! Acceso a archivos y a hilos que sirve igual en escritorio y en el navegador.
//!
//! En escritorio son `std::fs` y `std::thread` sin más. En WebAssembly no hay disco ni hilos: los
//! archivos son los del equipo que sirve la página (`filebrowser-server`, preguntándole por XHR
//! síncrono) y el trabajo que iría a otro hilo se hace en el momento.
use std::io;
use std::path::{Path, PathBuf};

#[cfg(not(target_arch = "wasm32"))]
pub use std::{fs, thread};

#[cfg(target_arch = "wasm32")]
pub mod fs;
#[cfg(target_arch = "wasm32")]
pub mod net;
#[cfg(target_arch = "wasm32")]
pub mod thread;

/// Carpeta de temporales.
#[cfg(not(target_arch = "wasm32"))]
pub fn temp_dir() -> PathBuf {
    std::env::temp_dir()
}
#[cfg(target_arch = "wasm32")]
pub use fs::temp_dir;

/// Las carpetas de una variable como PATH (en el navegador no hay: `std` se niega a trocearla).
#[cfg(not(target_arch = "wasm32"))]
pub fn split_paths(v: &std::ffi::OsStr) -> std::env::SplitPaths<'_> {
    std::env::split_paths(v)
}
#[cfg(target_arch = "wasm32")]
pub fn split_paths(_v: &std::ffi::OsStr) -> std::vec::IntoIter<PathBuf> {
    Vec::new().into_iter()
}

/// Número de proceso (en el navegador no hay: sirve un número fijo).
#[cfg(not(target_arch = "wasm32"))]
pub fn pid() -> u32 {
    std::process::id()
}
#[cfg(target_arch = "wasm32")]
pub fn pid() -> u32 {
    1
}

/// Los métodos de `Path` que tocan el disco, con nombre propio para no caer en los de `std`
/// (que en el navegador siempre fallan).
pub trait PathV {
    fn v_exists(&self) -> bool;
    fn v_metadata(&self) -> io::Result<fs::Metadata>;
    fn v_symlink_metadata(&self) -> io::Result<fs::Metadata>;
    fn v_read_dir(&self) -> io::Result<fs::ReadDir>;
    fn v_canonicalize(&self) -> io::Result<PathBuf>;
}

impl PathV for Path {
    fn v_exists(&self) -> bool {
        fs::metadata(self).is_ok()
    }
    fn v_metadata(&self) -> io::Result<fs::Metadata> {
        fs::metadata(self)
    }
    fn v_symlink_metadata(&self) -> io::Result<fs::Metadata> {
        fs::symlink_metadata(self)
    }
    fn v_read_dir(&self) -> io::Result<fs::ReadDir> {
        fs::read_dir(self)
    }
    fn v_canonicalize(&self) -> io::Result<PathBuf> {
        fs::canonicalize(self)
    }
}

impl PathV for PathBuf {
    fn v_exists(&self) -> bool {
        self.as_path().v_exists()
    }
    fn v_metadata(&self) -> io::Result<fs::Metadata> {
        self.as_path().v_metadata()
    }
    fn v_symlink_metadata(&self) -> io::Result<fs::Metadata> {
        self.as_path().v_symlink_metadata()
    }
    fn v_read_dir(&self) -> io::Result<fs::ReadDir> {
        self.as_path().v_read_dir()
    }
    fn v_canonicalize(&self) -> io::Result<PathBuf> {
        self.as_path().v_canonicalize()
    }
}

/// `is_dir`, `is_file` e `is_symlink` de rutas y de lo que devuelve el sistema de archivos.
pub trait KindV {
    fn v_is_dir(&self) -> bool;
    fn v_is_file(&self) -> bool;
    fn v_is_symlink(&self) -> bool;
}

impl KindV for Path {
    fn v_is_dir(&self) -> bool {
        fs::metadata(self).is_ok_and(|m| m.is_dir())
    }
    fn v_is_file(&self) -> bool {
        fs::metadata(self).is_ok_and(|m| m.is_file())
    }
    fn v_is_symlink(&self) -> bool {
        fs::symlink_metadata(self).is_ok_and(|m| m.is_symlink())
    }
}

impl KindV for PathBuf {
    fn v_is_dir(&self) -> bool {
        self.as_path().v_is_dir()
    }
    fn v_is_file(&self) -> bool {
        self.as_path().v_is_file()
    }
    fn v_is_symlink(&self) -> bool {
        self.as_path().v_is_symlink()
    }
}

impl KindV for fs::Metadata {
    fn v_is_dir(&self) -> bool {
        self.is_dir()
    }
    fn v_is_file(&self) -> bool {
        self.is_file()
    }
    fn v_is_symlink(&self) -> bool {
        self.is_symlink()
    }
}

impl KindV for fs::FileType {
    fn v_is_dir(&self) -> bool {
        self.is_dir()
    }
    fn v_is_file(&self) -> bool {
        self.is_file()
    }
    fn v_is_symlink(&self) -> bool {
        self.is_symlink()
    }
}
