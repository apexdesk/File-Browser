//! Iconos de carpeta: el que elige el usuario dentro de la aplicación (se guarda en la configuración,
//! nunca en la carpeta) y los personalizados del sistema:
//! - Linux/BSD: metadatos de GVFS (`metadata::custom-icon` y `custom-icon-name`, lo que escribe
//!   «Archivos» de GNOME al cambiar el icono en Propiedades), archivos `.directory` de KDE y las
//!   carpetas XDG de `user-dirs.dirs` (incluida la de proyectos).
//! - Windows: carpetas con desktop.ini o atributo Sistema/Sólo lectura → icono propio por ruta.
//! - macOS: carpetas con icono personalizado («Icon\r») → icono de NSWorkspace por ruta.
//! - Android y web: sólo los elegidos en la aplicación y los de las carpetas conocidas.
//!
//! Lo del sistema se consulta en un hilo, sólo para las carpetas que se ven, con caché acotada.
#[allow(unused_imports)]
use crate::vfs::{KindV as _, PathV as _};
use crate::thumbs::Decoded;
use egui::{ColorImage, Context, TextureHandle, TextureId, TextureOptions};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};

// ---------------------------------------------------------------------------
// Icono elegido en la aplicación

#[derive(Clone, Debug, PartialEq)]
pub enum Spec {
    /// Carpeta dibujada por la aplicación: color (`COLORS`) y emblema (`EMBLEMS`).
    Drawn { color: u8, emblem: u8 },
    /// Imagen PNG/SVG/ICO… del disco.
    Image(PathBuf),
    /// Icono del tema del sistema por nombre (Linux).
    Theme(String),
}

impl Spec {
    pub fn encode(&self) -> String {
        match self {
            Spec::Drawn { color, emblem } => format!("drawn:{color}:{emblem}"),
            Spec::Image(p) => format!("img:{}", p.display()),
            Spec::Theme(n) => format!("theme:{n}"),
        }
    }

    pub fn decode(s: &str) -> Option<Spec> {
        if let Some(r) = s.strip_prefix("drawn:") {
            let (c, e) = r.split_once(':')?;
            let (color, emblem) = (c.parse::<u8>().ok()?, e.parse::<u8>().ok()?);
            if (color as usize) < COLORS.len() && (emblem as usize) < EMBLEMS.len() {
                return Some(Spec::Drawn { color, emblem });
            }
            return None;
        }
        if let Some(p) = s.strip_prefix("img:") {
            return (!p.is_empty()).then(|| Spec::Image(PathBuf::from(p)));
        }
        if let Some(n) = s.strip_prefix("theme:") {
            return theme_name_ok(n).then(|| Spec::Theme(n.to_string()));
        }
        None
    }
}

/// Nombre de icono del tema razonable (sin rutas ni caracteres raros).
pub fn theme_name_ok(n: &str) -> bool {
    !n.is_empty() && n.len() <= 128 && !n.starts_with('.') && n.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '+'))
}

/// Color de la parte delantera de la carpeta (la trasera y la pestaña se derivan).
pub const COLORS: [[u8; 3]; 8] = [
    [247, 196, 92],  // ámbar (el de serie)
    [104, 160, 232], // azul
    [110, 194, 126], // verde
    [232, 106, 96],  // rojo
    [168, 128, 220], // violeta
    [84, 190, 186],  // turquesa
    [236, 130, 178], // rosa
    [160, 166, 176], // gris
];

/// Emblemas que se pueden poner sobre la carpeta (el primero, ninguno).
pub const EMBLEMS: [&str; 16] = ["", "star", "heart", "code", "music", "picture", "video", "download", "document", "book", "game", "cloud", "lock", "home", "share", "desktop"];

/// Nombre (clave de traducción) de cada emblema, para la ayuda emergente.
pub const EMBLEM_LABELS: [&str; 16] = [
    "Sin emblema",
    "Estrella",
    "Corazón",
    "Código",
    "Música",
    "Imágenes",
    "Vídeos",
    "Descargas",
    "Documentos",
    "Libros",
    "Juegos",
    "Nube",
    "Privado",
    "Inicio",
    "Compartido",
    "Escritorio",
];

fn emblem_index(name: &str) -> u8 {
    EMBLEMS.iter().position(|e| *e == name).unwrap_or(0) as u8
}

/// Aspecto dibujado de una carpeta conocida (Documentos, Descargas…), sin iconos del sistema.
pub fn special_drawn(kind: &str) -> Option<(u8, u8)> {
    let e = match kind {
        "documents" => "document",
        "downloads" => "download",
        "music" => "music",
        "pictures" => "picture",
        "videos" => "video",
        "desktop" => "desktop",
        "home" => "home",
        "projects" => "code",
        "publicshare" => "share",
        "templates" => "document",
        _ => return None,
    };
    Some((0, emblem_index(e)))
}

fn mul(c: [u8; 3], k: f32) -> [u8; 3] {
    let f = |x: u8| (x as f32 * k).round().clamp(0.0, 255.0) as u8;
    [f(c[0]), f(c[1]), f(c[2])]
}

fn hex(c: [u8; 3]) -> String {
    format!("#{:02x}{:02x}{:02x}", c[0], c[1], c[2])
}

/// Trazos del emblema en una rejilla de 24 unidades (relleno `F`, trazo `S`).
fn emblem_svg(name: &str) -> &'static str {
    match name {
        "star" => r#"<polygon class="F" points="12,2.5 14.9,8.6 21.5,9.3 16.6,13.8 18,20.3 12,17 6,20.3 7.4,13.8 2.5,9.3 9.1,8.6"/>"#,
        "heart" => r#"<path class="F" d="M12 20.5C5 16 2.5 12.5 2.5 8.8 2.5 6 4.7 3.8 7.4 3.8 9.4 3.8 11 5 12 6.6 13 5 14.6 3.8 16.6 3.8 19.3 3.8 21.5 6 21.5 8.8 21.5 12.5 19 16 12 20.5Z"/>"#,
        "code" => r#"<path class="S" d="M8 6 2.5 12 8 18M16 6 21.5 12 16 18M13.6 4 10.4 20"/>"#,
        "music" => r#"<path class="S" d="M9 18V5.5L20 3.5V16"/><circle class="F" cx="6.5" cy="18" r="2.7"/><circle class="F" cx="17.5" cy="16" r="2.7"/>"#,
        "picture" => r#"<rect class="S" x="3" y="4" width="18" height="16" rx="2.5"/><path class="S" d="M3.5 17 9 11.5 13 15.5 16 12.5 20.5 17"/><circle class="F" cx="16.3" cy="8.4" r="1.7"/>"#,
        "video" => r#"<rect class="S" x="2.5" y="6" width="13" height="12" rx="2.5"/><polygon class="F" points="16,10.5 21.5,7 21.5,17 16,13.5"/>"#,
        "download" => r#"<path class="S" d="M12 3V15M7 10 12 15 17 10M4 20.5H20"/>"#,
        "document" => r#"<path class="S" d="M6 2.5H14L19 7.5V21.5H6ZM14 2.5V7.5H19M9 12.5H16M9 16.5H16"/>"#,
        "book" => r#"<path class="S" d="M3.5 5C6.3 3.8 9.4 3.9 12 5.6 14.6 3.9 17.7 3.8 20.5 5V19.5C17.7 18.4 14.6 18.4 12 20 9.4 18.4 6.3 18.4 3.5 19.5ZM12 5.6V20"/>"#,
        "game" => r#"<path class="S" d="M7 7H17C19.8 7 21.5 9.5 21.5 13 21.5 16 19.6 17.4 17.6 16.4L15 14.8H9L6.4 16.4C4.4 17.4 2.5 16 2.5 13 2.5 9.5 4.2 7 7 7ZM7.5 9.5V13.5M5.5 11.5H9.5"/><circle class="F" cx="15.5" cy="10.5" r="1.3"/><circle class="F" cx="18" cy="12.8" r="1.3"/>"#,
        "cloud" => r#"<path class="F" d="M7 19.5H17.5C20.3 19.5 22 17.6 22 15.2 22 12.9 20.2 11 17.9 10.9 17.2 7.5 14.6 5 11.3 5 8.2 5 5.6 7.3 5.2 10.4 3.2 11 2 12.8 2 15 2 17.5 4.1 19.5 7 19.5Z"/>"#,
        "lock" => r#"<rect class="F" x="4.5" y="10.5" width="15" height="11" rx="2.5"/><path class="S" d="M8 10.5V7.5C8 5.3 9.8 3.5 12 3.5 14.2 3.5 16 5.3 16 7.5V10.5"/>"#,
        "home" => r#"<path class="S" d="M3 11.5 12 3.5 21 11.5M5.5 9.5V20.5H18.5V9.5M10 20.5V14.5H14V20.5"/>"#,
        "share" => r#"<path class="S" d="M8.2 10.8 15.8 6.7M8.2 13.2 15.8 17.3"/><circle class="F" cx="6" cy="12" r="3"/><circle class="F" cx="18" cy="5.5" r="3"/><circle class="F" cx="18" cy="18.5" r="3"/>"#,
        "desktop" => r#"<rect class="S" x="2.5" y="3.5" width="19" height="13" rx="2.5"/><path class="S" d="M8 21H16M12 16.5V21"/>"#,
        _ => "",
    }
}

/// SVG de la carpeta dibujada (misma silueta que `theme::draw_folder`), 96×96.
pub fn folder_svg(color: u8, emblem: u8) -> String {
    let front = COLORS.get(color as usize).copied().unwrap_or(COLORS[0]);
    let body = mul(front, 0.89);
    let tab = mul(front, 0.85);
    // emblema oscuro sobre colores claros y blanco sobre los oscuros
    let lum = 0.2126 * front[0] as f32 + 0.7152 * front[1] as f32 + 0.0722 * front[2] as f32;
    let ink = if lum > 150.0 { hex(mul(front, 0.45)) } else { "#ffffff".to_string() };
    let em = EMBLEMS.get(emblem as usize).map(|e| emblem_svg(e)).unwrap_or("");
    format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="96" height="96" viewBox="0 0 96 96"><style>.F{{fill:{ink};stroke:none}}.S{{fill:none;stroke:{ink};stroke-width:2;stroke-linecap:round;stroke-linejoin:round}}</style><rect x="3.8" y="7.7" width="37" height="19.2" rx="9.6" fill="{}"/><rect x="3.8" y="16.3" width="88.4" height="71" rx="9.6" fill="{}"/><rect x="3.8" y="30.5" width="88.4" height="56.9" rx="9.6" fill="{}"/><g transform="translate(34 41) scale(1.17)">{em}</g></svg>"#,
        hex(tab),
        hex(body),
        hex(front),
    )
}

/// Pinta un SVG propio (de confianza) a `px` de lado.
fn render_svg(svg: &str, px: u32) -> Option<Decoded> {
    let tree = resvg::usvg::Tree::from_data(svg.as_bytes(), &resvg::usvg::Options::default()).ok()?;
    let sz = tree.size();
    let s = px as f32 / sz.width().max(sz.height()).max(1.0);
    let mut pm = resvg::tiny_skia::Pixmap::new(px, px)?;
    resvg::render(&tree, resvg::tiny_skia::Transform::from_scale(s, s), &mut pm.as_mut());
    let mut rgba = Vec::with_capacity((px * px * 4) as usize);
    for p in pm.pixels() {
        let c = p.demultiply();
        rgba.extend_from_slice(&[c.red(), c.green(), c.blue(), c.alpha()]);
    }
    Some(Decoded { w: px as usize, h: px as usize, rgba })
}

// ---------------------------------------------------------------------------
// Carpetas conocidas

/// Lee `user-dirs.dirs`: (clase, carpeta) con clase = "desktop", "downloads", "documents", "music",
/// "pictures", "videos", "templates", "publicshare", "projects" (XDG_PROJECTS_DIR, xdg-user-dirs 0.19).
/// Una carpeta igual a `home` significa «desactivada» y se omite.
pub fn parse_user_dirs(txt: &str, home: &Path) -> Vec<(&'static str, PathBuf)> {
    let mut out = Vec::new();
    for l in txt.lines().take(200) {
        let l = l.trim();
        if l.starts_with('#') {
            continue;
        }
        let Some((k, v)) = l.split_once('=') else { continue };
        let kind = match k.trim() {
            "XDG_DESKTOP_DIR" => "desktop",
            "XDG_DOWNLOAD_DIR" => "downloads",
            "XDG_TEMPLATES_DIR" => "templates",
            "XDG_PUBLICSHARE_DIR" => "publicshare",
            "XDG_DOCUMENTS_DIR" => "documents",
            "XDG_MUSIC_DIR" => "music",
            "XDG_PICTURES_DIR" => "pictures",
            "XDG_VIDEOS_DIR" => "videos",
            "XDG_PROJECTS_DIR" => "projects",
            _ => continue,
        };
        let v = v.trim();
        let v = v.strip_prefix('"').and_then(|x| x.strip_suffix('"')).unwrap_or(v);
        let p = if let Some(r) = v.strip_prefix("$HOME") {
            let r = r.trim_start_matches('/');
            if r.is_empty() { home.to_path_buf() } else { home.join(r) }
        } else if v.starts_with('/') {
            PathBuf::from(v)
        } else {
            continue;
        };
        if p == home || p.components().any(|c| matches!(c, std::path::Component::ParentDir)) {
            continue;
        }
        out.push((kind, p));
    }
    out
}

/// Carpetas conocidas de este sistema → clase (para el icono especial).
pub fn known_folders() -> HashMap<PathBuf, &'static str> {
    let mut m: HashMap<PathBuf, &'static str> = HashMap::new();
    let home = dirs::home_dir();
    #[cfg(all(unix, not(any(target_os = "macos", target_os = "android", target_os = "ios"))))]
    if let (Some(h), Some(c)) = (&home, dirs::config_dir()) {
        if let Ok(t) = crate::vfs::fs::read_to_string(c.join("user-dirs.dirs")) {
            for (k, p) in parse_user_dirs(&t, h) {
                m.entry(p).or_insert(k);
            }
        }
    }
    #[cfg(target_os = "android")]
    {
        let h = PathBuf::from("/storage/emulated/0");
        for (n, k) in [("Download", "downloads"), ("Pictures", "pictures"), ("DCIM", "pictures"), ("Documents", "documents"), ("Music", "music"), ("Movies", "videos")] {
            m.insert(h.join(n), k);
        }
    }
    for (p, k) in [
        (dirs::document_dir(), "documents"),
        (dirs::download_dir(), "downloads"),
        (dirs::audio_dir(), "music"),
        (dirs::picture_dir(), "pictures"),
        (dirs::video_dir(), "videos"),
        (dirs::desktop_dir(), "desktop"),
        (dirs::template_dir(), "templates"),
        (dirs::public_dir(), "publicshare"),
    ] {
        if let Some(p) = p {
            if Some(&p) != home.as_ref() {
                m.entry(p).or_insert(k);
            }
        }
    }
    if let Some(h) = home {
        m.insert(h, "home");
    }
    m
}

/// `Icon=` de la sección `[Desktop Entry]` de un `.directory` de KDE.
pub fn parse_kde_directory(txt: &str) -> Option<String> {
    let mut in_entry = false;
    for l in txt.lines().take(400) {
        let l = l.trim();
        if l.starts_with('[') {
            in_entry = l == "[Desktop Entry]";
            continue;
        }
        if !in_entry {
            continue;
        }
        if let Some((k, v)) = l.split_once('=') {
            if k.trim() == "Icon" {
                let v = v.trim();
                return (!v.is_empty()).then(|| v.to_string());
            }
        }
    }
    None
}

/// Icono que el sistema da a una carpeta.
#[derive(Clone, Debug, PartialEq)]
pub enum PlatIcon {
    None,
    /// nombre del tema de iconos
    Theme(String),
    /// imagen del disco
    Image(PathBuf),
    /// pedir al sistema el icono de esa ruta (Windows, macOS)
    PerPath,
}

/// Valor de icono de GVFS o de KDE: URI `file://`, ruta (absoluta o relativa a la carpeta) o nombre del tema.
pub fn icon_value(v: &str, folder: &Path) -> PlatIcon {
    let v = v.trim();
    if let Some(r) = v.strip_prefix("file://") {
        // file:///ruta o file://localhost/ruta, con %XX
        let r = r.strip_prefix("localhost").unwrap_or(r);
        if !r.starts_with('/') {
            return PlatIcon::None;
        }
        return match percent_decode(r) {
            Some(p) => PlatIcon::Image(PathBuf::from(p)),
            None => PlatIcon::None,
        };
    }
    if v.starts_with('/') {
        return PlatIcon::Image(PathBuf::from(v));
    }
    if v.contains('/') || v.contains('\\') || v.contains("://") {
        let rel = Path::new(v);
        if v.contains("://") || rel.components().any(|c| !matches!(c, std::path::Component::Normal(_))) {
            return PlatIcon::None;
        }
        return PlatIcon::Image(folder.join(rel));
    }
    if theme_name_ok(v) {
        // «icono.png» sin ruta en KDE: archivo junto a la carpeta, no un nombre del tema
        let lower = v.to_ascii_lowercase();
        if [".png", ".svg", ".svgz", ".ico", ".xpm"].iter().any(|e| lower.ends_with(e)) {
            return PlatIcon::Image(folder.join(v));
        }
        return PlatIcon::Theme(v.to_string());
    }
    PlatIcon::None
}

fn percent_decode(s: &str) -> Option<String> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            let h = b.get(i + 1..i + 3)?;
            let v = u8::from_str_radix(std::str::from_utf8(h).ok()?, 16).ok()?;
            out.push(v);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    let s = String::from_utf8(out).ok()?;
    (!s.contains('\0')).then_some(s)
}

// ---------------------------------------------------------------------------
// Almacén de metadatos de GVFS (~/.local/share/gvfs-metadata): árbol binario + diario.
// Formato de gvfs/metadata/metatree.c, todo en big-endian:
//   cabecera del árbol: "\xda\x1ameta", mayor 1, menor, rotated u32, random_tag u32, root u32,
//     attributes u32, time_t_base u64.
//   entrada de carpeta: name u32, children u32, metadata u32, last_changed u32 (16 bytes).
//   hijos: num u32 + entradas; datos: num u32 + (key u32, value u32); key con el bit alto = lista.
//   cadenas: terminadas en NUL; lista de cadenas: num u32 + desplazamientos.
//   diario «<árbol>-<random_tag %08x>.log»: "\xda\x1ajour", mayor 1, menor, random_tag, file_size,
//     num_entries; entradas desde el byte 20: size u32, crc32 u32 (de los bytes 8..size), mtime u64,
//     tipo u8, ruta\0, datos, relleno a 4 y size u32 al final.
// Todo se lee con límites: un archivo dañado o malicioso sólo hace que no haya icono.

pub mod gvfs {
    pub const KEY_ICON: &str = "custom-icon";
    pub const KEY_ICON_NAME: &str = "custom-icon-name";
    /// Tamaño máximo que se lee de cada archivo.
    pub const MAX_FILE: u64 = 32 << 20;
    const MAX_STR: usize = 4096;
    const LIST: u32 = 1 << 31;

    fn be32(d: &[u8], off: u32) -> Option<u32> {
        let o = off as usize;
        let b = d.get(o..o.checked_add(4)?)?;
        Some(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn cstr(d: &[u8], off: u32) -> Option<&str> {
        let o = off as usize;
        let rest = d.get(o..)?;
        let rest = &rest[..rest.len().min(MAX_STR)];
        let n = rest.iter().position(|&c| c == 0)?;
        std::str::from_utf8(&rest[..n]).ok()
    }

    pub struct Tree {
        data: Vec<u8>,
        root: u32,
        attrs: Vec<String>,
        pub tag: u32,
        pub rotated: bool,
    }

    impl Tree {
        pub fn parse(data: Vec<u8>) -> Option<Tree> {
            if data.len() < 32 || &data[..6] != b"\xda\x1ameta" || data[6] != 1 {
                return None;
            }
            let rotated = be32(&data, 8)? != 0;
            let tag = be32(&data, 12)?;
            let root = be32(&data, 16)?;
            let aoff = be32(&data, 20)?;
            let n = be32(&data, aoff)?;
            if n as usize > data.len() / 4 || n > 4096 {
                return None;
            }
            let mut attrs = Vec::with_capacity(n as usize);
            for i in 0..n {
                let so = be32(&data, aoff.checked_add(4)?.checked_add(i.checked_mul(4)?)?)?;
                attrs.push(cstr(&data, so)?.to_string());
            }
            // la raíz debe caber entera
            be32(&data, root.checked_add(12)?)?;
            Some(Tree { data, root, attrs, tag, rotated })
        }

        fn child(&self, dirent: u32, name: &str) -> Option<u32> {
            let d = &self.data;
            let ch = be32(d, dirent.checked_add(4)?)?;
            if ch == 0 {
                return None;
            }
            let n = be32(d, ch)?;
            if n as usize > d.len() / 16 {
                return None;
            }
            // ordenados por nombre (strcmp): búsqueda binaria acotada
            let (mut lo, mut hi) = (0u32, n);
            let mut steps = 0;
            while lo < hi && steps < 64 {
                steps += 1;
                let mid = lo + (hi - lo) / 2;
                let e = ch.checked_add(4)?.checked_add(mid.checked_mul(16)?)?;
                let en = cstr(d, be32(d, e)?)?;
                match en.as_bytes().cmp(name.as_bytes()) {
                    std::cmp::Ordering::Equal => return Some(e),
                    std::cmp::Ordering::Less => lo = mid + 1,
                    std::cmp::Ordering::Greater => hi = mid,
                }
            }
            None
        }

        /// Valor (cadena) de `key` para `path` ("/Proyectos/x", relativo a la raíz del árbol).
        pub fn get(&self, path: &str, key: &str) -> Option<String> {
            let mut de = self.root;
            for (i, comp) in path.split('/').filter(|c| !c.is_empty()).enumerate() {
                if i > 256 {
                    return None;
                }
                de = self.child(de, comp)?;
            }
            let d = &self.data;
            let md = be32(d, de.checked_add(8)?)?;
            if md == 0 {
                return None;
            }
            let n = be32(d, md)?;
            if n as usize > d.len() / 8 {
                return None;
            }
            for i in 0..n {
                let e = md.checked_add(4)?.checked_add(i.checked_mul(8)?)?;
                let k = be32(d, e)?;
                if k & LIST != 0 {
                    continue;
                }
                if self.attrs.get(k as usize).map(|s| s.as_str()) == Some(key) {
                    return cstr(d, be32(d, e.checked_add(4)?)?).map(|s| s.to_string());
                }
            }
            None
        }
    }

    #[derive(Debug, PartialEq)]
    pub enum Op {
        Set { path: String, key: String, value: String },
        Unset { path: String, key: String },
        /// el valor pasó a ser una lista (para estas claves equivale a quitarlo)
        SetList { path: String, key: String },
        Copy { dst: String, src: String },
        Remove { path: String },
    }

    pub struct Journal {
        pub ops: Vec<Op>,
    }

    pub fn crc32(b: &[u8]) -> u32 {
        let mut c = !0u32;
        for &x in b {
            c ^= x as u32;
            for _ in 0..8 {
                c = if c & 1 != 0 { (c >> 1) ^ 0xEDB8_8320 } else { c >> 1 };
            }
        }
        !c
    }

    impl Journal {
        /// `tag`: el random_tag del árbol (un diario de otro árbol no vale).
        pub fn parse(d: &[u8], tag: u32) -> Option<Journal> {
            if d.len() < 20 || &d[..6] != b"\xda\x1ajour" || d[6] != 1 || be32(d, 8)? != tag {
                return None;
            }
            let n = be32(d, 16)?;
            let mut ops = Vec::new();
            let mut off: usize = 20;
            for _ in 0..n.min(1_000_000) {
                let Some(size) = be32(d, off as u32) else { break };
                let size = size as usize;
                if size < 24 || size % 4 != 0 {
                    break;
                }
                let Some(e) = off.checked_add(size).and_then(|end| d.get(off..end)) else { break };
                let crc = u32::from_be_bytes([e[4], e[5], e[6], e[7]]);
                let tail = u32::from_be_bytes([e[size - 4], e[size - 3], e[size - 2], e[size - 1]]) as usize;
                if tail != size || crc32(&e[8..]) != crc {
                    break;
                }
                let ty = e[16];
                let body = &e[17..size - 4];
                let mut parts = body.split(|&c| c == 0).map(|s| std::str::from_utf8(s).ok().map(|s| s.to_string()));
                let mut next = || parts.next().flatten();
                let op = match ty {
                    0 => match (next(), next(), next()) {
                        (Some(path), Some(key), Some(value)) => Some(Op::Set { path, key, value }),
                        _ => None,
                    },
                    1 => match (next(), next()) {
                        (Some(path), Some(key)) => Some(Op::SetList { path, key }),
                        _ => None,
                    },
                    2 => match (next(), next()) {
                        (Some(path), Some(key)) => Some(Op::Unset { path, key }),
                        _ => None,
                    },
                    3 => match (next(), next()) {
                        (Some(dst), Some(src)) => Some(Op::Copy { dst, src }),
                        _ => None,
                    },
                    4 => next().map(|path| Op::Remove { path }),
                    _ => None,
                };
                if let Some(op) = op {
                    ops.push(op);
                }
                off += size;
            }
            Some(Journal { ops })
        }
    }

    fn under(prefix: &str, path: &str) -> Option<usize> {
        let p = prefix.trim_end_matches('/');
        if path == p || p.is_empty() {
            return Some(p.len());
        }
        (path.starts_with(p) && path.as_bytes().get(p.len()) == Some(&b'/')).then_some(p.len())
    }

    /// Valor de `key` para `path` teniendo en cuenta el diario (lo más reciente manda) y luego el árbol.
    pub fn lookup(tree: Option<&Tree>, journal: Option<&Journal>, path: &str, key: &str) -> Option<String> {
        let mut path = path.to_string();
        if let Some(j) = journal {
            for op in j.ops.iter().rev() {
                match op {
                    Op::Set { path: p, key: k, value } if *p == path && k == key => return Some(value.clone()),
                    Op::Unset { path: p, key: k } | Op::SetList { path: p, key: k } if *p == path && k == key => return None,
                    Op::Remove { path: p } if under(p, &path).is_some() => return None,
                    Op::Copy { dst, src } => {
                        if let Some(n) = under(dst, &path) {
                            // antes de la copia, lo que ahora es `path` era lo de la ruta de origen
                            path = format!("{}{}", src.trim_end_matches('/'), &path[n..]);
                        }
                    }
                    _ => {}
                }
            }
        }
        tree?.get(&path, key)
    }
}

/// Lo que se lee del almacén de GVFS (un árbol con su diario), recargado si cambian los archivos.
#[cfg(all(unix, not(any(target_os = "macos", target_os = "android", target_os = "ios", target_arch = "wasm32"))))]
struct GvfsDb {
    name: &'static str,
    stamp: Option<(u64, Option<std::time::SystemTime>, u64, Option<std::time::SystemTime>)>,
    tree: Option<gvfs::Tree>,
    journal: Option<gvfs::Journal>,
}

#[cfg(all(unix, not(any(target_os = "macos", target_os = "android", target_os = "ios", target_arch = "wasm32"))))]
impl GvfsDb {
    fn refresh(&mut self, dir: &Path) {
        let read = |p: &Path| -> Option<Vec<u8>> {
            let md = std::fs::metadata(p).ok()?;
            if !md.is_file() || md.len() > gvfs::MAX_FILE {
                return None;
            }
            std::fs::read(p).ok()
        };
        let tp = dir.join(self.name);
        let tm = std::fs::metadata(&tp).ok();
        let tag = self.tree.as_ref().map(|t| t.tag);
        let jp = tag.map(|t| dir.join(format!("{}-{t:08x}.log", self.name)));
        let jm = jp.as_ref().and_then(|p| std::fs::metadata(p).ok());
        let stamp = (tm.as_ref().map_or(0, |m| m.len()), tm.and_then(|m| m.modified().ok()), jm.as_ref().map_or(0, |m| m.len()), jm.and_then(|m| m.modified().ok()));
        if self.stamp == Some(stamp) && !self.tree.as_ref().is_some_and(|t| t.rotated) {
            return;
        }
        self.stamp = Some(stamp);
        self.tree = read(&tp).and_then(gvfs::Tree::parse);
        self.journal = self.tree.as_ref().and_then(|t| {
            let j = read(&dir.join(format!("{}-{:08x}.log", self.name, t.tag)))?;
            gvfs::Journal::parse(&j, t.tag)
        });
        if self.tree.as_ref().map(|t| t.tag) != tag {
            // el diario depende del árbol: la próxima vez se compara con el nuevo
            self.stamp = None;
        }
    }

    fn get(&self, path: &str, key: &str) -> Option<String> {
        gvfs::lookup(self.tree.as_ref(), self.journal.as_ref(), path, key)
    }
}

/// Consulta del sistema hecha en el hilo de iconos.
#[cfg(not(target_arch = "wasm32"))]
struct Prober {
    #[cfg(all(unix, not(any(target_os = "macos", target_os = "android", target_os = "ios"))))]
    gvfs: Option<(PathBuf, Vec<GvfsDb>, std::time::Instant)>,
    #[cfg(all(unix, not(any(target_os = "macos", target_os = "android", target_os = "ios"))))]
    home: Option<PathBuf>,
}

#[cfg(not(target_arch = "wasm32"))]
impl Prober {
    fn new() -> Prober {
        Prober {
            #[cfg(all(unix, not(any(target_os = "macos", target_os = "android", target_os = "ios"))))]
            gvfs: dirs::data_dir().map(|d| (d.join("gvfs-metadata"), vec![GvfsDb { name: "home", stamp: None, tree: None, journal: None }, GvfsDb { name: "root", stamp: None, tree: None, journal: None }], std::time::Instant::now())),
            #[cfg(all(unix, not(any(target_os = "macos", target_os = "android", target_os = "ios"))))]
            home: dirs::home_dir(),
        }
    }

    #[allow(unused_variables)]
    fn probe(&mut self, path: &Path, fresh: bool) -> PlatIcon {
        #[cfg(all(unix, not(any(target_os = "macos", target_os = "android", target_os = "ios"))))]
        {
            if let Some(v) = self.gvfs_icon(path, fresh) {
                return v;
            }
            // .directory de KDE (no en montajes de red de GVFS: cada apertura sería lenta)
            let remote = path.to_str().is_some_and(|s| s.contains("/gvfs/"));
            if !remote {
                let f = path.join(".directory");
                if let Ok(md) = std::fs::metadata(&f) {
                    if md.is_file() && md.len() <= 64 * 1024 {
                        if let Some(v) = std::fs::read(&f).ok().and_then(|b| parse_kde_directory(&String::from_utf8_lossy(&b))) {
                            let r = icon_value(&v, path);
                            if r != PlatIcon::None {
                                return r;
                            }
                        }
                    }
                }
            }
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            // FILE_ATTRIBUTE_READONLY | FILE_ATTRIBUTE_SYSTEM: el Explorador sólo lee desktop.ini con uno de ellos
            if std::fs::metadata(path).is_ok_and(|m| m.file_attributes() & 0x5 != 0) || path.join("desktop.ini").is_file() {
                return PlatIcon::PerPath;
            }
        }
        #[cfg(target_os = "macos")]
        {
            // icono personalizado del Finder: archivo «Icon\r» oculto dentro de la carpeta
            if std::fs::symlink_metadata(path.join("Icon\r")).is_ok() {
                return PlatIcon::PerPath;
            }
        }
        PlatIcon::None
    }

    #[cfg(all(unix, not(any(target_os = "macos", target_os = "android", target_os = "ios"))))]
    fn gvfs_icon(&mut self, path: &Path, fresh: bool) -> Option<PlatIcon> {
        let (dir, dbs, checked) = self.gvfs.as_mut()?;
        if fresh || checked.elapsed() > std::time::Duration::from_secs(2) || dbs.iter().all(|d| d.stamp.is_none()) {
            *checked = std::time::Instant::now();
            if dir.is_dir() {
                for d in dbs.iter_mut() {
                    d.refresh(dir);
                }
            }
        }
        // árbol «home» para lo que cuelga de la carpeta personal (ruta relativa a ella), «root» para el resto
        let (db, rel) = match self.home.as_ref().and_then(|h| path.strip_prefix(h).ok()) {
            Some(r) => (0, format!("/{}", r.to_str()?)),
            None => {
                use std::os::unix::fs::MetadataExt;
                // sólo en el mismo sistema de archivos que «/» (otros montajes tienen su propio árbol)
                let same = std::fs::metadata(path).ok()?.dev() == std::fs::metadata("/").ok()?.dev();
                if !same {
                    return None;
                }
                (1, path.to_str()?.to_string())
            }
        };
        let db = dbs.get(db)?;
        if let Some(v) = db.get(&rel, gvfs::KEY_ICON) {
            let r = icon_value(&v, path);
            if r != PlatIcon::None {
                return Some(r);
            }
        }
        let n = db.get(&rel, gvfs::KEY_ICON_NAME)?;
        theme_name_ok(&n).then(|| PlatIcon::Theme(n))
    }
}

/// Hilo que resuelve los iconos del sistema de las carpetas visibles (lo más reciente primero).
#[cfg(not(target_arch = "wasm32"))]
struct Worker {
    tx: Sender<(PathBuf, bool)>,
    rx: Receiver<(PathBuf, PlatIcon)>,
}

#[cfg(not(target_arch = "wasm32"))]
impl Worker {
    fn new(ctx: Context) -> Option<Worker> {
        let (tx, req) = channel::<(PathBuf, bool)>();
        let (res_tx, rx) = channel();
        crate::vfs::thread::Builder::new()
            .name("fb-foldericons".into())
            .spawn(move || {
                let mut pr = Prober::new();
                while let Ok(first) = req.recv() {
                    let mut batch = vec![first];
                    while let Ok(x) = req.try_recv() {
                        batch.push(x);
                    }
                    // si se desplazó deprisa, sólo interesa lo último que se pidió
                    if batch.len() > 512 {
                        let drop = batch.len() - 512;
                        for (p, _) in batch.drain(..drop) {
                            if res_tx.send((p, PlatIcon::None)).is_err() {
                                return;
                            }
                        }
                    }
                    for (p, fresh) in batch.into_iter().rev() {
                        let r = crate::thumbs::guarded(|| pr.probe(&p, fresh)).unwrap_or(PlatIcon::None);
                        if res_tx.send((p, r)).is_err() {
                            return;
                        }
                    }
                    ctx.request_repaint();
                }
            })
            .ok()?;
        Some(Worker { tx, rx })
    }
}

// ---------------------------------------------------------------------------
// Imágenes elegidas como icono (o indicadas por GVFS/KDE): se decodifican en un hilo, acotadas.

const IMG_PX: u32 = 128;
const IMG_MAX_BYTES: u64 = 8 << 20;
const IMG_CACHE: usize = 128;

fn decode_icon_image(p: &Path) -> Option<Decoded> {
    let ext = p.extension()?.to_string_lossy().to_ascii_lowercase();
    if !matches!(ext.as_str(), "png" | "svg" | "ico" | "jpg" | "jpeg" | "gif" | "webp" | "bmp") {
        return None;
    }
    let md = crate::vfs::fs::metadata(p).ok()?;
    if !md.v_is_file() || md.len() > IMG_MAX_BYTES {
        return None;
    }
    crate::thumbs::guarded(|| crate::thumbs::decode_image(p, IMG_PX)).flatten().map(square)
}

/// Centra la imagen en un cuadrado transparente (los iconos se dibujan en un hueco cuadrado).
fn square(d: Decoded) -> Decoded {
    if d.w == d.h || d.w == 0 || d.h == 0 || d.rgba.len() != d.w * d.h * 4 {
        return d;
    }
    let n = d.w.max(d.h);
    let (ox, oy) = ((n - d.w) / 2, (n - d.h) / 2);
    let mut out = vec![0u8; n * n * 4];
    for y in 0..d.h {
        let src = &d.rgba[y * d.w * 4..(y + 1) * d.w * 4];
        let at = ((y + oy) * n + ox) * 4;
        out[at..at + d.w * 4].copy_from_slice(src);
    }
    Decoded { w: n, h: n, rgba: out }
}

struct Images {
    tex: HashMap<PathBuf, Option<TextureHandle>>,
    pending: HashSet<PathBuf>,
    tx: Option<Sender<PathBuf>>,
    rx: Option<Receiver<(PathBuf, Option<Decoded>)>>,
}

impl Images {
    fn new() -> Images {
        Images { tex: HashMap::new(), pending: HashSet::new(), tx: None, rx: None }
    }

    fn get(&mut self, ctx: &Context, p: &Path) -> Option<TextureId> {
        if let Some(rx) = &self.rx {
            while let Ok((k, d)) = rx.try_recv() {
                self.pending.remove(&k);
                if self.tex.len() >= IMG_CACHE {
                    self.tex.clear();
                }
                let t = d.map(|d| ctx.load_texture("foldericon", ColorImage::from_rgba_unmultiplied([d.w, d.h], &d.rgba), TextureOptions::LINEAR));
                self.tex.insert(k, t);
                ctx.request_repaint();
            }
        }
        if let Some(t) = self.tex.get(p) {
            return t.as_ref().map(|t| t.id());
        }
        if crate::vfs::WEB {
            // sin hilos: se decodifica aquí, una vez
            let d = decode_icon_image(p);
            if self.tex.len() >= IMG_CACHE {
                self.tex.clear();
            }
            let t = d.map(|d| ctx.load_texture("foldericon", ColorImage::from_rgba_unmultiplied([d.w, d.h], &d.rgba), TextureOptions::LINEAR));
            let id = t.as_ref().map(|t| t.id());
            self.tex.insert(p.to_path_buf(), t);
            return id;
        }
        if self.pending.len() > 64 || !self.pending.insert(p.to_path_buf()) {
            return None;
        }
        if self.tx.is_none() {
            let (tx, req) = channel::<PathBuf>();
            let (res_tx, rx) = channel();
            let c = ctx.clone();
            crate::thumbs::spawn_decoder("fb-foldericon-img", move || {
                while let Ok(p) = req.recv() {
                    let d = decode_icon_image(&p);
                    if res_tx.send((p, d)).is_err() {
                        break;
                    }
                    c.request_repaint();
                }
            });
            self.tx = Some(tx);
            self.rx = Some(rx);
        }
        if let Some(tx) = &self.tx {
            let _ = tx.send(p.to_path_buf());
        }
        None
    }
}

// ---------------------------------------------------------------------------
// Estado de la aplicación

/// Lo que guarda `App` para los iconos de carpeta y la lista de favoritos.
pub struct IconState {
    /// carpetas conocidas → clase ("documents", "projects"…)
    known: HashMap<PathBuf, &'static str>,
    /// iconos elegidos en la aplicación
    app: HashMap<PathBuf, Spec>,
    plat: HashMap<PathBuf, PlatIcon>,
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    pending: HashSet<PathBuf>,
    #[cfg(not(target_arch = "wasm32"))]
    worker: Option<Worker>,
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    worker_tried: bool,
    images: Images,
    drawn: HashMap<(u8, u8), TextureHandle>,
    /// generación del listado vista por última vez (al recargar se vuelven a leer los de esa carpeta)
    last_gen: u64,
    last_os: bool,
    /// carpetas de favoritos que no existen (se comprueba en un hilo)
    missing: std::sync::Arc<std::sync::Mutex<HashSet<PathBuf>>>,
    missing_busy: std::sync::Arc<std::sync::atomic::AtomicBool>,
    missing_at: Option<web_time::Instant>,
    /// arrastre para reordenar favoritos: índice de origen
    pub fav_drag: Option<usize>,
}

/// Máximo de carpetas recordadas (una carpeta con 50 000 subcarpetas no llena la memoria).
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
const PLAT_CACHE: usize = 20_000;

impl IconState {
    pub fn new() -> IconState {
        IconState {
            known: known_folders(),
            app: HashMap::new(),
            plat: HashMap::new(),
            pending: HashSet::new(),
            #[cfg(not(target_arch = "wasm32"))]
            worker: None,
            worker_tried: false,
            images: Images::new(),
            drawn: HashMap::new(),
            last_gen: 0,
            last_os: true,
            missing: Default::default(),
            missing_busy: Default::default(),
            missing_at: None,
            fav_drag: None,
        }
    }

    /// Carga los iconos elegidos (`cfg.folder_icons`).
    pub fn set_app(&mut self, list: &[(PathBuf, String)]) {
        self.app = list.iter().filter_map(|(p, v)| Spec::decode(v).map(|s| (p.clone(), s))).collect();
    }

    pub fn app_spec(&self, p: &Path) -> Option<&Spec> {
        self.app.get(p)
    }

    pub fn known(&self, p: &Path) -> Option<&'static str> {
        self.known.get(p).copied()
    }

    /// Llamar una vez por fotograma (o antes de pedir iconos): al recargar la carpeta `cwd`
    /// (`generation` cambia) se olvida lo leído de sus subcarpetas; si se activa o desactiva la
    /// lectura del sistema (`os`), todo.
    pub fn tick(&mut self, generation: u64, cwd: &Path, os: bool) {
        if os != self.last_os {
            self.last_os = os;
            self.plat.clear();
        }
        if generation != self.last_gen {
            self.last_gen = generation;
            self.plat.retain(|p, _| p.parent() != Some(cwd));
            self.plat.remove(cwd);
        }
    }

    /// Icono del sistema de la carpeta `p` (None mientras se consulta o si no tiene).
    pub fn plat(&mut self, ctx: &Context, p: &Path) -> Option<PlatIcon> {
        #[cfg(not(target_arch = "wasm32"))]
        {
            if let Some(w) = &self.worker {
                while let Ok((k, v)) = w.rx.try_recv() {
                    self.pending.remove(&k);
                    if self.plat.len() >= PLAT_CACHE {
                        self.plat.clear();
                    }
                    self.plat.insert(k, v);
                }
            }
            if let Some(v) = self.plat.get(p) {
                return (*v != PlatIcon::None).then(|| v.clone());
            }
            if !p.is_absolute() || cfg!(target_os = "android") {
                return None;
            }
            if self.worker.is_none() && !self.worker_tried {
                self.worker_tried = true;
                self.worker = Worker::new(ctx.clone());
            }
            if self.pending.len() < 4096 && self.pending.insert(p.to_path_buf()) {
                if let Some(w) = &self.worker {
                    // la primera vez tras recargar, el almacén de GVFS se revisa ya
                    let _ = w.tx.send((p.to_path_buf(), false));
                }
            }
            None
        }
        #[cfg(target_arch = "wasm32")]
        {
            let _ = (ctx, p);
            None
        }
    }

    /// Textura de la carpeta dibujada con ese color y emblema.
    pub fn drawn_tex(&mut self, ctx: &Context, color: u8, emblem: u8) -> Option<TextureId> {
        if let Some(t) = self.drawn.get(&(color, emblem)) {
            return Some(t.id());
        }
        let d = render_svg(&folder_svg(color, emblem), 96)?;
        let t = ctx.load_texture(format!("folder-{color}-{emblem}"), ColorImage::from_rgba_unmultiplied([d.w, d.h], &d.rgba), TextureOptions::LINEAR);
        let id = t.id();
        self.drawn.insert((color, emblem), t);
        Some(id)
    }

    /// Textura de una imagen del disco (None mientras se decodifica o si no vale).
    pub fn image_tex(&mut self, ctx: &Context, p: &Path) -> Option<TextureId> {
        self.images.get(ctx, p)
    }

    /// ¿La carpeta de favoritos `p` no existe? `all`: las que hay que vigilar (se revisan cada 10 s en un hilo).
    pub fn missing(&mut self, ctx: &Context, all: &[PathBuf], p: &Path) -> bool {
        use std::sync::atomic::Ordering;
        if crate::vfs::WEB {
            return false;
        }
        let due = self.missing_at.is_none_or(|t| t.elapsed() > std::time::Duration::from_secs(10));
        if due && !self.missing_busy.swap(true, Ordering::AcqRel) {
            self.missing_at = Some(web_time::Instant::now());
            let (list, out, busy, c) = (all.to_vec(), self.missing.clone(), self.missing_busy.clone(), ctx.clone());
            crate::vfs::thread::spawn(move || {
                let gone: HashSet<PathBuf> = list.into_iter().filter(|p| !p.v_is_dir()).collect();
                let changed = {
                    let mut m = crate::thumbs::lock(&out);
                    let ch = *m != gone;
                    *m = gone;
                    ch
                };
                busy.store(false, Ordering::Release);
                if changed {
                    c.request_repaint();
                }
            });
        }
        crate::thumbs::lock(&self.missing).contains(p)
    }

    /// Vuelve a comprobar ya qué favoritos existen (tras añadir o quitar).
    pub fn recheck_missing(&mut self) {
        self.missing_at = None;
    }
}

impl Default for IconState {
    fn default() -> Self {
        Self::new()
    }
}

/// Nombres de iconos de las carpetas de un tema (para buscar «Icono del tema…»), sin repetir, acotado.
pub fn list_theme_names(dirs: &[PathBuf]) -> Vec<String> {
    let mut set: HashSet<String> = HashSet::new();
    for d in dirs.iter().take(400) {
        let Ok(rd) = crate::vfs::fs::read_dir(d) else { continue };
        for e in rd.flatten().take(10_000) {
            let n = e.file_name().to_string_lossy().into_owned();
            let stem = n.strip_suffix(".svg").or_else(|| n.strip_suffix(".png"));
            if let Some(s) = stem {
                if theme_name_ok(s) && !s.ends_with("-symbolic") {
                    set.insert(s.to_string());
                }
            }
            if set.len() >= 20_000 {
                break;
            }
        }
    }
    let mut v: Vec<String> = set.into_iter().collect();
    v.sort();
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spec_round_trip() {
        for s in [Spec::Drawn { color: 3, emblem: 5 }, Spec::Image(PathBuf::from("/a b/ícono.png")), Spec::Theme("folder-code".into())] {
            assert_eq!(Spec::decode(&s.encode()), Some(s));
        }
        assert_eq!(Spec::decode("drawn:99:1"), None);
        assert_eq!(Spec::decode("drawn:1:99"), None);
        assert_eq!(Spec::decode("theme:../x"), None);
        assert_eq!(Spec::decode("img:"), None);
        assert_eq!(Spec::decode("otro"), None);
    }

    #[test]
    fn every_drawn_folder_renders() {
        for c in 0..COLORS.len() as u8 {
            for e in 0..EMBLEMS.len() as u8 {
                let d = render_svg(&folder_svg(c, e), 24).expect("svg");
                assert_eq!((d.w, d.h, d.rgba.len()), (24, 24, 24 * 24 * 4));
                assert!(d.rgba.chunks(4).any(|p| p[3] > 0));
            }
        }
        assert_eq!(EMBLEMS.len(), EMBLEM_LABELS.len());
        for k in ["documents", "downloads", "music", "pictures", "videos", "desktop", "home", "projects", "publicshare", "templates"] {
            assert!(special_drawn(k).is_some_and(|(_, e)| e > 0), "{k}");
        }
    }

    #[test]
    fn user_dirs() {
        let home = Path::new("/home/u");
        let txt = "# comentario\nXDG_DESKTOP_DIR=\"$HOME/Escritorio\"\nXDG_PROJECTS_DIR=\"$HOME/Proyectos\"\nXDG_TEMPLATES_DIR=\"$HOME/\"\nXDG_PUBLICSHARE_DIR=\"/srv/pub\"\nXDG_MUSIC_DIR=relativo\nXDG_VIDEOS_DIR=\"$HOME/../x\"\nOTRA=\"$HOME/y\"\n  XDG_DOWNLOAD_DIR = \"$HOME/Descargas\"\n";
        let v = parse_user_dirs(txt, home);
        assert_eq!(
            v,
            vec![
                ("desktop", PathBuf::from("/home/u/Escritorio")),
                ("projects", PathBuf::from("/home/u/Proyectos")),
                ("publicshare", PathBuf::from("/srv/pub")),
                ("downloads", PathBuf::from("/home/u/Descargas")),
            ]
        );
    }

    #[test]
    fn kde_directory() {
        assert_eq!(parse_kde_directory("[Desktop Entry]\nIcon=folder-red\n"), Some("folder-red".into()));
        assert_eq!(parse_kde_directory("[Other]\nIcon=x\n[Desktop Entry]\nName=y\nIcon[es]=z\n Icon = /a/b.png \n"), Some("/a/b.png".into()));
        assert_eq!(parse_kde_directory("[Other]\nIcon=x\n"), None);
        assert_eq!(parse_kde_directory("[Desktop Entry]\nIcon=\n"), None);
    }

    #[test]
    fn icon_values() {
        let f = Path::new("/home/u/P");
        assert_eq!(icon_value("folder-code", f), PlatIcon::Theme("folder-code".into()));
        assert_eq!(icon_value("file:///home/u/Im%C3%A1genes/a%20b.png", f), PlatIcon::Image(PathBuf::from("/home/u/Imágenes/a b.png")));
        assert_eq!(icon_value("file://localhost/x.svg", f), PlatIcon::Image(PathBuf::from("/x.svg")));
        assert_eq!(icon_value("file://host/x.svg", f), PlatIcon::None);
        assert_eq!(icon_value("file:///x%2", f), PlatIcon::None);
        assert_eq!(icon_value("/abs/i.png", f), PlatIcon::Image(PathBuf::from("/abs/i.png")));
        assert_eq!(icon_value("sub/i.png", f), PlatIcon::Image(PathBuf::from("/home/u/P/sub/i.png")));
        assert_eq!(icon_value("../i.png", f), PlatIcon::None);
        assert_eq!(icon_value("i.png", f), PlatIcon::Image(PathBuf::from("/home/u/P/i.png")));
        assert_eq!(icon_value("smb://x/y", f), PlatIcon::None);
        assert_eq!(icon_value("a b", f), PlatIcon::None);
    }

    // ---- GVFS: archivos sintéticos con el formato de metatree.c

    struct B {
        d: Vec<u8>,
    }
    impl B {
        fn u32(&mut self, v: u32) -> usize {
            let o = self.d.len();
            self.d.extend_from_slice(&v.to_be_bytes());
            o
        }
        fn set(&mut self, at: usize, v: u32) {
            self.d[at..at + 4].copy_from_slice(&v.to_be_bytes());
        }
        fn s(&mut self, v: &str) -> u32 {
            let o = self.d.len() as u32;
            self.d.extend_from_slice(v.as_bytes());
            self.d.push(0);
            while self.d.len() % 4 != 0 {
                self.d.push(0);
            }
            o
        }
    }

    /// Árbol: / → {Música, Proyectos{sub}}; Proyectos con custom-icon-name, sub con custom-icon.
    fn tree_bytes(tag: u32) -> Vec<u8> {
        let mut b = B { d: Vec::new() };
        b.d.extend_from_slice(b"\xda\x1ameta\x01\x00");
        b.u32(0); // rotated
        b.u32(tag);
        let root_at = b.u32(0);
        let attrs_at = b.u32(0);
        b.u32(0);
        b.u32(0); // time_t_base
        // atributos
        let names = [b.s("metadata-x"), b.s("custom-icon-name"), b.s("custom-icon"), b.s("nautilus-icon-position")];
        let av = b.u32(names.len() as u32) as u32;
        for n in names {
            b.u32(n);
        }
        b.set(attrs_at, av);
        // datos de Proyectos: custom-icon-name y una lista (que se ignora)
        let v1 = b.s("folder-code");
        let md1 = b.u32(2) as u32;
        b.u32(1);
        b.u32(v1);
        b.u32(3 | (1 << 31));
        b.u32(v1);
        // datos de sub: custom-icon
        let v2 = b.s("file:///tmp/i.png");
        let md2 = b.u32(1) as u32;
        b.u32(2);
        b.u32(v2);
        // hijos de Proyectos: sub
        let n_sub = b.s("sub");
        let ch_p = b.u32(1) as u32;
        b.u32(n_sub);
        b.u32(0);
        b.u32(md2);
        b.u32(0);
        // hijos de la raíz (ordenados): Música, Proyectos
        let n_mus = b.s("Música");
        let n_pro = b.s("Proyectos");
        let ch_r = b.u32(2) as u32;
        b.u32(n_mus);
        b.u32(0);
        b.u32(0);
        b.u32(0);
        b.u32(n_pro);
        b.u32(ch_p);
        b.u32(md1);
        b.u32(0);
        let n_root = b.s("/");
        let root = b.u32(n_root) as u32;
        b.u32(ch_r);
        b.u32(0);
        b.u32(0);
        b.set(root_at, root);
        b.d
    }

    fn entry(ty: u8, strs: &[&str]) -> Vec<u8> {
        let mut e = vec![0u8; 8];
        e.extend_from_slice(&7u64.to_be_bytes());
        e.push(ty);
        for s in strs {
            e.extend_from_slice(s.as_bytes());
            e.push(0);
        }
        while e.len() % 4 != 0 {
            e.push(0);
        }
        let len = (e.len() + 4) as u32;
        e.extend_from_slice(&len.to_be_bytes());
        e[..4].copy_from_slice(&len.to_be_bytes());
        let crc = gvfs::crc32(&e[8..]);
        e[4..8].copy_from_slice(&crc.to_be_bytes());
        e
    }

    fn journal_bytes(tag: u32, entries: &[Vec<u8>]) -> Vec<u8> {
        let mut d = b"\xda\x1ajour\x01\x00".to_vec();
        d.extend_from_slice(&tag.to_be_bytes());
        d.extend_from_slice(&32768u32.to_be_bytes());
        d.extend_from_slice(&(entries.len() as u32).to_be_bytes());
        for e in entries {
            d.extend_from_slice(e);
        }
        d.resize(d.len() + 64, 0);
        d
    }

    #[test]
    fn gvfs_tree_lookup() {
        let t = gvfs::Tree::parse(tree_bytes(0x264c6c17)).expect("árbol");
        assert_eq!(t.tag, 0x264c6c17);
        assert_eq!(t.get("/Proyectos", gvfs::KEY_ICON_NAME).as_deref(), Some("folder-code"));
        assert_eq!(t.get("/Proyectos", gvfs::KEY_ICON), None);
        assert_eq!(t.get("/Proyectos/sub", gvfs::KEY_ICON).as_deref(), Some("file:///tmp/i.png"));
        assert_eq!(t.get("/Música", gvfs::KEY_ICON_NAME), None);
        assert_eq!(t.get("/Nada", gvfs::KEY_ICON_NAME), None);
        assert_eq!(t.get("//Proyectos/", gvfs::KEY_ICON_NAME).as_deref(), Some("folder-code"));
        assert_eq!(icon_value(&t.get("/Proyectos/sub", gvfs::KEY_ICON).unwrap(), Path::new("/x")), PlatIcon::Image(PathBuf::from("/tmp/i.png")));
    }

    #[test]
    fn gvfs_journal_overrides_tree() {
        let tag = 0xabcdef01;
        let t = gvfs::Tree::parse(tree_bytes(tag)).unwrap();
        let j = journal_bytes(
            tag,
            &[
                entry(0, &["/Música", "custom-icon-name", "folder-music"]),
                entry(2, &["/Proyectos", "custom-icon-name"]),
                entry(3, &["/Copia", "/Proyectos"]),
                entry(4, &["/Música"]),
                entry(0, &["/Nuevo", "custom-icon-name", "folder-red"]),
            ],
        );
        let j = gvfs::Journal::parse(&j, tag).expect("diario");
        assert_eq!(j.ops.len(), 5);
        let look = |p: &str, k: &str| gvfs::lookup(Some(&t), Some(&j), p, k);
        assert_eq!(look("/Proyectos", gvfs::KEY_ICON_NAME), None);
        assert_eq!(look("/Música", gvfs::KEY_ICON_NAME), None);
        assert_eq!(look("/Nuevo", gvfs::KEY_ICON_NAME).as_deref(), Some("folder-red"));
        // la copia se hizo después de quitar el icono de Proyectos, pero su subcarpeta conserva el suyo
        assert_eq!(look("/Copia", gvfs::KEY_ICON_NAME), None);
        assert_eq!(look("/Copia/sub", gvfs::KEY_ICON).as_deref(), Some("file:///tmp/i.png"));
        // un diario de otro árbol no vale
        assert!(gvfs::Journal::parse(&journal_bytes(1, &[]), tag).is_none());
    }

    #[test]
    fn gvfs_damaged_input_is_harmless() {
        let good = tree_bytes(5);
        // cada truncado y cada byte alterado: nunca un pánico, como mucho sin resultado
        for n in 0..good.len() {
            if let Some(t) = gvfs::Tree::parse(good[..n].to_vec()) {
                let _ = t.get("/Proyectos/sub", gvfs::KEY_ICON);
            }
        }
        for i in 0..good.len() {
            for v in [0u8, 0xff, 0x7f] {
                let mut d = good.clone();
                d[i] = v;
                if let Some(t) = gvfs::Tree::parse(d) {
                    let _ = t.get("/Proyectos/sub", gvfs::KEY_ICON);
                    let _ = t.get("/Música", gvfs::KEY_ICON_NAME);
                }
            }
        }
        let j = journal_bytes(5, &[entry(0, &["/a", "custom-icon", "x"]), entry(4, &["/a"])]);
        for n in 0..j.len() {
            let _ = gvfs::Journal::parse(&j[..n], 5);
        }
        for i in 0..j.len() {
            let mut d = j.clone();
            d[i] ^= 0x5a;
            if let Some(x) = gvfs::Journal::parse(&d, 5) {
                let _ = gvfs::lookup(None, Some(&x), "/a", "custom-icon");
            }
        }
        // una entrada con CRC malo corta el diario ahí (como hace GVFS)
        let mut bad = entry(0, &["/a", "custom-icon", "x"]);
        bad[5] ^= 1;
        let jj = journal_bytes(5, &[bad, entry(0, &["/b", "custom-icon", "y"])]);
        assert_eq!(gvfs::Journal::parse(&jj, 5).unwrap().ops.len(), 0);
    }

    #[test]
    fn theme_names_listing() {
        let n = web_time::SystemTime::now().duration_since(web_time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
        let dir = crate::vfs::temp_dir().join(format!("fbj-icons-{}-{n:x}", crate::vfs::pid()));
        crate::vfs::fs::create_dir_all(&dir).unwrap();
        for f in ["folder-code.svg", "folder-red.png", "folder-symbolic.svg", "readme.txt", ".hidden.svg"] {
            crate::vfs::fs::write(dir.join(f), b"x").unwrap();
        }
        assert_eq!(list_theme_names(&[dir.clone()]), vec!["folder-code".to_string(), "folder-red".to_string()]);
        let _ = crate::vfs::fs::remove_dir_all(&dir);
    }
}
