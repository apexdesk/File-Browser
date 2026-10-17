//! Arrastrar archivos fuera de la ventana hacia otras aplicaciones.
//!
//! winit no ofrece «origen de arrastre», así que se implementa por plataforma:
//! * Wayland: `wl_data_device.start_drag` sobre la conexión de winit (el número de serie del botón se
//!   obtiene con un `wl_pointer` propio) y, a la inversa, recepción de archivos soltados.
//! * X11: protocolo XDND desde un hilo con su propia conexión.
//! * Windows: `DoDragDrop` con un objeto de datos del shell.
use std::path::PathBuf;

pub struct DragOut {
    imp: imp::Imp,
}

impl DragOut {
    pub fn new(frame: &eframe::Frame, ctx: &egui::Context) -> Option<DragOut> {
        imp::Imp::new(frame, ctx).map(|imp| DragOut { imp })
    }

    /// Atiende los eventos pendientes; llamar en cada fotograma.
    pub fn pump(&mut self) {
        self.imp.pump();
    }

    /// Inicia el arrastre del sistema con el botón principal pulsado. `false` si no es posible.
    pub fn start(&mut self, paths: &[PathBuf]) -> bool {
        !paths.is_empty() && self.imp.start(paths)
    }

    /// ¿Hay un arrastre del sistema en curso?
    pub fn active(&self) -> bool {
        self.imp.active()
    }

    /// ¿Ha terminado un arrastre desde la última consulta?
    pub fn take_done(&mut self) -> bool {
        self.imp.take_done()
    }

    /// Archivos soltados desde otras aplicaciones (solo Wayland: winit no los entrega).
    pub fn take_dropped(&mut self) -> Vec<PathBuf> {
        self.imp.take_dropped()
    }
}

/// `file:///ruta%20codificada` por archivo, separados por CRLF (text/uri-list).
pub(crate) fn uri_list(paths: &[PathBuf]) -> Vec<u8> {
    let mut out = String::new();
    for p in paths {
        out.push_str("file://");
        #[cfg(unix)]
        let bytes = {
            use std::os::unix::ffi::OsStrExt;
            p.as_os_str().as_bytes().to_vec()
        };
        #[cfg(not(unix))]
        let bytes = p.to_string_lossy().replace('\\', "/").into_bytes();
        for b in bytes {
            if b.is_ascii_alphanumeric() || b"-_.~/".contains(&b) {
                out.push(b as char);
            } else {
                out.push_str(&format!("%{b:02X}"));
            }
        }
        out.push_str("\r\n");
    }
    out.into_bytes()
}

/// Rutas de un `text/uri-list` recibido.
#[allow(dead_code)]
pub(crate) fn parse_uri_list(data: &[u8]) -> Vec<PathBuf> {
    String::from_utf8_lossy(data)
        .lines()
        .filter(|l| !l.starts_with('#'))
        .filter_map(|l| l.trim().strip_prefix("file://"))
        .map(|rest| {
            let path = rest.find('/').map(|i| &rest[i..]).unwrap_or(rest);
            let b = path.as_bytes();
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
            PathBuf::from(String::from_utf8_lossy(&out).into_owned())
        })
        .collect()
}

#[cfg(all(unix, not(any(target_os = "macos", target_os = "android"))))]
mod imp {
    pub use crate::dragout_linux::Imp;
}

#[cfg(windows)]
mod imp {
    pub use crate::dragout_windows::Imp;
}

#[cfg(target_os = "macos")]
mod imp {
    pub use crate::dragout_macos::Imp;
}

#[cfg(not(any(windows, target_os = "macos", all(unix, not(any(target_os = "macos", target_os = "android"))))))]
mod imp {
    use super::*;
    pub struct Imp;
    impl Imp {
        pub fn new(_: &eframe::Frame, _: &egui::Context) -> Option<Imp> {
            None
        }
        pub fn pump(&mut self) {}
        pub fn start(&mut self, _: &[PathBuf]) -> bool {
            false
        }
        pub fn active(&self) -> bool {
            false
        }
        pub fn take_done(&mut self) -> bool {
            false
        }
        pub fn take_dropped(&mut self) -> Vec<PathBuf> {
            Vec::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uri_roundtrip() {
        let v = vec![PathBuf::from("/tmp/a b/ñ#1.txt"), PathBuf::from("/x")];
        let s = uri_list(&v);
        assert_eq!(String::from_utf8(s.clone()).unwrap(), "file:///tmp/a%20b/%C3%B1%231.txt\r\nfile:///x\r\n");
        assert_eq!(parse_uri_list(&s), v);
    }
}
