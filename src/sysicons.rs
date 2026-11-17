//! Iconos del sistema: tema de iconos freedesktop en Linux, Shell (SHGetFileInfo / lista de imágenes)
//! en Windows, NSWorkspace (sólo iconos por ruta) en macOS. En otras plataformas devuelve `None` y se
//! usan los iconos vectoriales propios.
#[allow(unused_imports)]
use crate::vfs::{KindV as _, PathV as _};
use crate::thumbs::Decoded;
use egui::{ColorImage, Context, TextureHandle, TextureId, TextureOptions};
use std::collections::HashMap;

pub struct SysIcons {
    cache: HashMap<String, Option<TextureHandle>>,
    budget: u32,
    /// Iconos propios de cada archivo (.exe, .lnk, discos…): los resuelve un hilo aparte (Windows).
    #[cfg(any(windows, target_os = "macos"))]
    by_path: Option<PathIcons>,
    /// El tema de iconos se carga en segundo plano para no retrasar el arranque.
    #[cfg(target_os = "linux")]
    lin: Option<linux::Lookup>,
    #[cfg(target_os = "linux")]
    lin_rx: Option<std::sync::mpsc::Receiver<linux::Lookup>>,
}

impl SysIcons {
    pub fn new() -> SysIcons {
        SysIcons {
            cache: HashMap::new(),
            budget: 0,
            #[cfg(any(windows, target_os = "macos"))]
            by_path: None,
            #[cfg(target_os = "linux")]
            lin: None,
            #[cfg(target_os = "linux")]
            lin_rx: {
                let (tx, rx) = std::sync::mpsc::channel();
                crate::vfs::thread::spawn(move || {
                    let _ = tx.send(linux::Lookup::new());
                });
                Some(rx)
            },
        }
    }

    /// `true` cuando ya se puede resolver iconos (en Linux, cuando terminó de leerse el tema).
    fn ready(&mut self, ctx: &Context) -> bool {
        #[cfg(target_os = "linux")]
        {
            if self.lin.is_none() {
                match self.lin_rx.as_ref().map(|r| r.try_recv()) {
                    Some(Ok(l)) => {
                        self.lin = Some(l);
                        self.lin_rx = None;
                        ctx.request_repaint();
                    }
                    Some(Err(std::sync::mpsc::TryRecvError::Empty)) => {
                        ctx.request_repaint_after(std::time::Duration::from_millis(20));
                        return false;
                    }
                    // el hilo del tema terminó sin resultado (pánico): listo, sin iconos del tema
                    // (si no, «Icono del tema…» esperaría para siempre redibujando)
                    _ => self.lin_rx = None,
                }
            }
        }
        let _ = ctx;
        true
    }

    /// Limita cuántos iconos nuevos se resuelven por fotograma (la búsqueda toca disco).
    pub fn begin_frame(&mut self) {
        self.budget = 3;
    }

    fn upload(&mut self, ctx: &Context, key: String, d: Option<Decoded>) -> Option<TextureId> {
        let tex = d.map(|d| {
            ctx.load_texture(
                "sysicon",
                ColorImage::from_rgba_unmultiplied([d.w, d.h], &d.rgba),
                TextureOptions::LINEAR,
            )
        });
        let id = tex.as_ref().map(|t| t.id());
        self.cache.insert(key, tex);
        id
    }

    /// `special`: nombre en minúsculas de carpeta de usuario ("documents", "downloads"…).
    pub fn folder(&mut self, ctx: &Context, special: Option<&str>) -> Option<TextureId> {
        let key = format!("dir:{}", special.unwrap_or(""));
        if let Some(t) = self.cache.get(&key) {
            return t.as_ref().map(|t| t.id());
        }
        if !self.ready(ctx) {
            return None;
        }
        if self.budget == 0 {
            ctx.request_repaint();
            return None;
        }
        self.budget -= 1;
        let d = self.load_folder(special);
        self.upload(ctx, key, d)
    }

    pub fn file(&mut self, ctx: &Context, ext: &str) -> Option<TextureId> {
        let key = format!("ext:{}", ext.to_ascii_lowercase());
        if let Some(t) = self.cache.get(&key) {
            return t.as_ref().map(|t| t.id());
        }
        if !self.ready(ctx) {
            return None;
        }
        if self.budget == 0 {
            ctx.request_repaint();
            return None;
        }
        self.budget -= 1;
        let d = self.load_file(&ext.to_ascii_lowercase());
        self.upload(ctx, key, d)
    }

    /// Icono propio de un archivo o unidad (el del programa al que apunta un acceso directo, el de un disco,
    /// el de una carpeta con icono personalizado…). Se resuelve en segundo plano; mientras tanto (y fuera
    /// de Windows y macOS) devuelve `None`.
    #[allow(unused_variables)]
    pub fn for_path(&mut self, ctx: &Context, path: &std::path::Path) -> Option<TextureId> {
        #[cfg(any(windows, target_os = "macos"))]
        {
            return self.by_path.get_or_insert_with(PathIcons::new).get(ctx, path);
        }
        #[cfg(not(any(windows, target_os = "macos")))]
        None
    }

    /// Icono del tema por nombre (el primero de `names` que exista), p. ej. el `custom-icon-name`
    /// que GNOME guarda para una carpeta. Fuera de Linux, `None`.
    #[allow(unused_variables)]
    pub fn named(&mut self, ctx: &Context, names: &[&str]) -> Option<TextureId> {
        let key = format!("name:{}", names.first()?);
        if let Some(t) = self.cache.get(&key) {
            return t.as_ref().map(|t| t.id());
        }
        #[cfg(target_os = "linux")]
        {
            if !self.ready(ctx) {
                return None;
            }
            if self.budget == 0 {
                ctx.request_repaint();
                return None;
            }
            self.budget -= 1;
            if self.cache.len() > 4000 {
                self.cache.retain(|k, _| !k.starts_with("name:"));
            }
            let d = self.lin.as_mut()?.load_any(names);
            return self.upload(ctx, key, d);
        }
        #[cfg(not(target_os = "linux"))]
        None
    }

    /// Carpetas del tema actual (y los que hereda) con iconos de lugares y carpetas, para buscar
    /// «Icono del tema…». `None` mientras se carga el tema; vacío fuera de Linux.
    pub fn theme_dirs(&mut self, ctx: &Context) -> Option<Vec<std::path::PathBuf>> {
        #[cfg(target_os = "linux")]
        {
            if !self.ready(ctx) {
                return None;
            }
            return Some(self.lin.as_ref().map(|l| l.place_dirs()).unwrap_or_default());
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = ctx;
            Some(Vec::new())
        }
    }

    #[cfg(target_os = "linux")]
    fn load_folder(&mut self, special: Option<&str>) -> Option<Decoded> {
        let names: &[&str] = match special {
            Some("documents") => &["folder-documents", "folder"],
            Some("downloads") => &["folder-download", "folder-downloads", "folder"],
            Some("music") => &["folder-music", "folder"],
            Some("pictures") => &["folder-pictures", "folder"],
            Some("videos") => &["folder-videos", "folder"],
            Some("desktop") => &["user-desktop", "folder"],
            Some("home") => &["user-home", "folder"],
            Some("projects") => &["folder-projects", "folder-development", "folder-code", "folder"],
            Some("templates") => &["folder-templates", "folder"],
            Some("publicshare") => &["folder-publicshare", "folder-public", "folder"],
            Some("drive") => &["drive-harddisk", "drive-removable-media", "folder"],
            _ => &["folder"],
        };
        self.lin.as_mut()?.load_any(names)
    }
    #[cfg(target_os = "linux")]
    fn load_file(&mut self, ext: &str) -> Option<Decoded> {
        let lin = self.lin.as_mut()?;
        let names = lin.names_for_ext(ext);
        let refs: Vec<&str> = names.iter().map(|s| s.as_str()).collect();
        lin.load_any(&refs)
    }

    #[cfg(windows)]
    fn load_folder(&mut self, _special: Option<&str>) -> Option<Decoded> {
        win::icon_for("folder", true)
    }
    #[cfg(windows)]
    fn load_file(&mut self, ext: &str) -> Option<Decoded> {
        win::icon_for(&format!("file.{ext}"), false)
    }

    #[cfg(not(any(target_os = "linux", windows)))]
    fn load_folder(&mut self, _special: Option<&str>) -> Option<Decoded> {
        None
    }
    #[cfg(not(any(target_os = "linux", windows)))]
    fn load_file(&mut self, _ext: &str) -> Option<Decoded> {
        None
    }
}

#[cfg(any(windows, target_os = "macos"))]
struct PathIcons {
    tx: std::sync::mpsc::Sender<String>,
    rx: std::sync::mpsc::Receiver<(String, Option<Decoded>)>,
    cache: HashMap<String, Option<TextureHandle>>,
    pending: std::collections::HashSet<String>,
}

#[cfg(any(windows, target_os = "macos"))]
impl PathIcons {
    fn new() -> PathIcons {
        let (tx, req) = std::sync::mpsc::channel::<String>();
        let (res_tx, rx) = std::sync::mpsc::channel();
        crate::vfs::thread::spawn(move || {
            // el shell necesita COM en el hilo que pide los iconos
            // SAFETY: se inicializa COM una vez en este hilo propio, que vive hasta el final.
            #[cfg(windows)]
            unsafe {
                let _ = windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_APARTMENTTHREADED);
            }
            while let Ok(p) = req.recv() {
                #[cfg(windows)]
                let d = win::icon_for_path(&p);
                #[cfg(target_os = "macos")]
                let d = crate::thumbs::guarded(|| mac::icon_for_path(&p)).flatten();
                if res_tx.send((p, d)).is_err() {
                    break;
                }
            }
        });
        PathIcons { tx, rx, cache: HashMap::new(), pending: Default::default() }
    }

    fn get(&mut self, ctx: &Context, path: &std::path::Path) -> Option<TextureId> {
        while let Ok((k, d)) = self.rx.try_recv() {
            self.pending.remove(&k);
            let tex = d.map(|d| ctx.load_texture("pathicon", ColorImage::from_rgba_unmultiplied([d.w, d.h], &d.rgba), TextureOptions::LINEAR));
            self.cache.insert(k, tex);
            ctx.request_repaint();
        }
        let key = path.to_string_lossy().into_owned();
        if let Some(t) = self.cache.get(&key) {
            return t.as_ref().map(|t| t.id());
        }
        if self.cache.len() > 3000 {
            self.cache.clear();
        }
        if self.pending.insert(key.clone()) {
            let _ = self.tx.send(key);
        }
        None
    }
}

// ---------------------------------------------------------------------------
#[cfg(target_os = "linux")]
mod linux {
    #[allow(unused_imports)]
    use crate::vfs::{KindV as _, PathV as _};
    use super::Decoded;
    use std::collections::HashMap;
    use std::path::PathBuf;

    struct Dir {
        path: PathBuf,
        size: i32,
        min: i32,
        max: i32,
        kind: u8, // 0 fixed, 1 scalable, 2 threshold
        threshold: i32,
        symbolic: bool,
    }

    pub struct Lookup {
        themes: Vec<Vec<Dir>>,
        globs: HashMap<String, String>,
        generic: HashMap<String, String>,
        found: HashMap<String, Option<PathBuf>>,
    }

    fn base_dirs() -> Vec<PathBuf> {
        let mut v = Vec::new();
        if let Some(h) = dirs::home_dir() {
            v.push(h.join(".local/share/icons"));
            v.push(h.join(".icons"));
        }
        if let Some(d) = dirs::data_dir() {
            v.push(d.join("icons"));
        }
        let xdg = std::env::var("XDG_DATA_DIRS").unwrap_or_else(|_| "/usr/local/share:/usr/share".into());
        for d in xdg.split(':').filter(|s| !s.is_empty()) {
            v.push(PathBuf::from(d).join("icons"));
        }
        v.push(PathBuf::from("/usr/share/icons"));
        v.push(PathBuf::from("/usr/share/pixmaps"));
        v.dedup();
        v.into_iter().filter(|p| p.v_is_dir()).collect()
    }

    fn current_theme() -> String {
        if let Ok(o) = std::process::Command::new("gsettings")
            .args(["get", "org.gnome.desktop.interface", "icon-theme"])
            .output()
        {
            let s = String::from_utf8_lossy(&o.stdout).trim().trim_matches('\'').to_string();
            if o.status.success() && !s.is_empty() {
                return s;
            }
        }
        if let Some(c) = dirs::config_dir() {
            for f in ["gtk-3.0/settings.ini", "gtk-4.0/settings.ini"] {
                if let Ok(t) = crate::vfs::fs::read_to_string(c.join(f)) {
                    for l in t.lines() {
                        if let Some(v) = l.strip_prefix("gtk-icon-theme-name") {
                            let v = v.trim_start_matches([' ', '=']).trim().to_string();
                            if !v.is_empty() {
                                return v;
                            }
                        }
                    }
                }
            }
        }
        "Adwaita".into()
    }

    /// Lee index.theme: devuelve (Inherits, directorios).
    fn parse_theme(base: &PathBuf, name: &str) -> Option<(Vec<String>, Vec<Dir>)> {
        // un nombre de tema o carpeta con «..» o ruta absoluta saldría de la carpeta de iconos
        let safe = |s: &str| !s.is_empty() && !s.starts_with('/') && !s.split('/').any(|c| c == "..");
        if !safe(name) {
            return None;
        }
        let root = base.join(name);
        let txt = crate::vfs::fs::read_to_string(root.join("index.theme")).ok()?;
        let mut inherits = Vec::new();
        let mut dirs_list: Vec<String> = Vec::new();
        let mut sections: HashMap<String, HashMap<String, String>> = HashMap::new();
        let mut cur = String::new();
        for l in txt.lines() {
            let l = l.trim();
            if l.starts_with('[') && l.ends_with(']') {
                cur = l[1..l.len() - 1].to_string();
            } else if let Some((k, v)) = l.split_once('=') {
                if cur == "Icon Theme" {
                    match k {
                        "Inherits" => inherits = v.split(',').map(|s| s.trim().to_string()).collect(),
                        "Directories" => dirs_list = v.split(',').map(|s| s.trim().to_string()).collect(),
                        _ => {}
                    }
                } else {
                    sections.entry(cur.clone()).or_default().insert(k.to_string(), v.to_string());
                }
            }
        }
        let mut out = Vec::new();
        for d in dirs_list {
            if !safe(&d) {
                continue;
            }
            let Some(sec) = sections.get(&d) else { continue };
            let num = |k: &str| sec.get(k).and_then(|v| v.parse::<i32>().ok());
            if num("Scale").unwrap_or(1) != 1 {
                continue;
            }
            let size = num("Size").unwrap_or(0);
            let kind = match sec.get("Type").map(|s| s.as_str()) {
                Some("Fixed") => 0,
                Some("Scalable") => 1,
                _ => 2,
            };
            out.push(Dir {
                path: root.join(&d),
                size,
                min: num("MinSize").unwrap_or(size),
                max: num("MaxSize").unwrap_or(size),
                kind,
                threshold: num("Threshold").unwrap_or(2),
                symbolic: d.contains("symbolic"),
            });
        }
        Some((inherits, out))
    }

    impl Lookup {
        pub fn new() -> Lookup {
            let bases = base_dirs();
            let mut chain = vec![current_theme()];
            let mut themes = Vec::new();
            let mut i = 0;
            while i < chain.len() && i < 12 {
                let name = chain[i].clone();
                for b in &bases {
                    if let Some((inh, d)) = parse_theme(b, &name) {
                        for p in inh {
                            if !chain.contains(&p) {
                                chain.push(p);
                            }
                        }
                        themes.push(d);
                        break;
                    }
                }
                i += 1;
                if i == chain.len() && !chain.iter().any(|c| c == "hicolor") {
                    chain.push("hicolor".into());
                }
            }
            let mut globs = HashMap::new();
            if let Ok(t) = crate::vfs::fs::read_to_string("/usr/share/mime/globs2") {
                for l in t.lines() {
                    let mut it = l.splitn(3, ':');
                    let (_w, Some(mime), Some(glob)) = (it.next(), it.next(), it.next()) else { continue };
                    if let Some(ext) = glob.strip_prefix("*.") {
                        if !ext.contains(['*', '?', '[', '.']) {
                            globs.entry(ext.to_ascii_lowercase()).or_insert_with(|| mime.to_string());
                        }
                    }
                }
            }
            let mut generic = HashMap::new();
            if let Ok(t) = crate::vfs::fs::read_to_string("/usr/share/mime/generic-icons") {
                for l in t.lines() {
                    if let Some((m, i)) = l.split_once(':') {
                        generic.insert(m.to_string(), i.to_string());
                    }
                }
            }
            Lookup { themes, globs, generic, found: HashMap::new() }
        }

        pub fn names_for_ext(&self, ext: &str) -> Vec<String> {
            let mut v = Vec::new();
            if let Some(mime) = self.globs.get(ext) {
                v.push(mime.replace('/', "-"));
                if let Some(g) = self.generic.get(mime) {
                    v.push(g.clone());
                }
                if let Some((media, _)) = mime.split_once('/') {
                    v.push(format!("{media}-x-generic"));
                }
            }
            v.push("text-x-generic".into());
            v.push("unknown".into());
            v
        }

        fn find(&mut self, name: &str, want: i32) -> Option<PathBuf> {
            if let Some(r) = self.found.get(name) {
                return r.clone();
            }
            let mut res = None;
            'themes: for dirs in &self.themes {
                let mut cands: Vec<(i64, &Dir)> = dirs
                    .iter()
                    .filter(|d| !d.symbolic)
                    .map(|d| {
                        let (want, dsize, dmin, dmax, dthr) = (want as i64, d.size as i64, d.min as i64, d.max as i64, d.threshold as i64);
                        let dist = match d.kind {
                            0 => (dsize - want).abs(),
                            1 => {
                                if want < dmin { dmin - want } else if want > dmax { want - dmax } else { 0 }
                            }
                            _ => {
                                if want < dsize - dthr { dsize - dthr - want }
                                else if want > dsize + dthr { want - dsize - dthr }
                                else { 0 }
                            }
                        };
                        // preferir tamaños algo mayores (más nitidez al reducir)
                        (dist * 2 + if dsize < want && d.kind != 1 { 1 } else { 0 }, d)
                    })
                    .collect();
                cands.sort_by_key(|c| c.0);
                for (_, d) in cands {
                    for ext in ["svg", "png"] {
                        let p = d.path.join(format!("{name}.{ext}"));
                        if p.v_is_file() {
                            res = Some(p);
                            break 'themes;
                        }
                    }
                }
            }
            if res.is_none() {
                for b in ["/usr/share/pixmaps"] {
                    for ext in ["svg", "png"] {
                        let p = PathBuf::from(b).join(format!("{name}.{ext}"));
                        if p.v_is_file() {
                            res = Some(p);
                        }
                    }
                }
            }
            self.found.insert(name.to_string(), res.clone());
            res
        }

        /// Carpetas no simbólicas de «places» (iconos de carpetas), del tema y los que hereda.
        pub fn place_dirs(&self) -> Vec<PathBuf> {
            let mut v: Vec<PathBuf> = Vec::new();
            for t in &self.themes {
                for d in t {
                    if !d.symbolic && d.path.components().any(|c| c.as_os_str() == "places") && !v.contains(&d.path) {
                        v.push(d.path.clone());
                    }
                }
            }
            v
        }

        pub fn load_any(&mut self, names: &[&str]) -> Option<Decoded> {
            for n in names {
                if let Some(p) = self.find(n, 96) {
                    if let Some(d) = crate::thumbs::decode_image(&p, 128) {
                        return Some(d);
                    }
                }
            }
            None
        }
    }
}

// ---------------------------------------------------------------------------
// macOS: icono que el Finder muestra para una ruta (también el personalizado de una carpeta).
// No se puede compilar en Linux: está escrito para objc2 0.5 / objc2-app-kit 0.2 sin probar.
#[cfg(target_os = "macos")]
mod mac {
    use super::Decoded;
    use objc2_app_kit::{NSImage, NSWorkspace};
    use objc2_foundation::{NSSize, NSString};

    pub fn icon_for_path(path: &str) -> Option<Decoded> {
        objc2::rc::autoreleasepool(|_| {
            #[allow(unused_unsafe)]
            // SAFETY: llamadas de AppKit seguras en cualquier hilo (NSWorkspace e NSImage lo son);
            // los objetos devueltos son `Retained` y se liberan solos.
            let bytes: Vec<u8> = unsafe {
                let ws = NSWorkspace::sharedWorkspace();
                let img: objc2::rc::Retained<NSImage> = ws.iconForFile(&NSString::from_str(path));
                img.setSize(NSSize::new(128.0, 128.0));
                let tiff = img.TIFFRepresentation()?;
                tiff.bytes().to_vec()
            };
            if bytes.len() > 64 << 20 {
                return None;
            }
            let im = image::load_from_memory(&bytes).ok()?;
            let im = if im.width() > 128 || im.height() > 128 { im.thumbnail(128, 128) } else { im };
            let rgba = im.into_rgba8();
            Some(Decoded { w: rgba.width() as usize, h: rgba.height() as usize, rgba: rgba.into_raw() })
        })
    }
}

// ---------------------------------------------------------------------------
#[cfg(windows)]
mod win {
    use super::Decoded;
    use std::ffi::c_void;

    #[repr(C)]
    struct ShFileInfoW {
        icon: isize,
        index: i32,
        attrs: u32,
        display: [u16; 260],
        type_name: [u16; 80],
    }
    #[repr(C)]
    struct IconInfo {
        is_icon: i32,
        xh: u32,
        yh: u32,
        mask: isize,
        color: isize,
    }
    #[repr(C)]
    struct Bitmap {
        ty: i32,
        w: i32,
        h: i32,
        stride: i32,
        planes: u16,
        bpp: u16,
        bits: *mut c_void,
    }
    #[repr(C)]
    struct BitmapInfoHeader {
        size: u32,
        w: i32,
        h: i32,
        planes: u16,
        bpp: u16,
        compression: u32,
        size_image: u32,
        xppm: i32,
        yppm: i32,
        used: u32,
        important: u32,
    }
    #[repr(C)]
    struct Guid(u32, u16, u16, [u8; 8]);

    #[link(name = "shell32")]
    unsafe extern "system" {
        fn SHGetFileInfoW(path: *const u16, attrs: u32, psfi: *mut ShFileInfoW, cb: u32, flags: u32) -> usize;
        fn SHGetImageList(list: i32, riid: *const Guid, ppv: *mut *mut c_void) -> i32;
    }
    #[link(name = "user32")]
    unsafe extern "system" {
        fn GetIconInfo(icon: isize, info: *mut IconInfo) -> i32;
        fn DestroyIcon(icon: isize) -> i32;
    }
    #[link(name = "gdi32")]
    unsafe extern "system" {
        fn GetObjectW(h: isize, cb: i32, out: *mut Bitmap) -> i32;
        fn GetDIBits(dc: isize, bmp: isize, start: u32, lines: u32, bits: *mut c_void, bi: *mut BitmapInfoHeader, usage: u32) -> i32;
        fn CreateCompatibleDC(dc: isize) -> isize;
        fn DeleteDC(dc: isize) -> i32;
        fn DeleteObject(h: isize) -> i32;
    }

    const SHGFI_ICON: u32 = 0x100;
    const SHGFI_SYSICONINDEX: u32 = 0x4000;
    const SHGFI_USEFILEATTRIBUTES: u32 = 0x10;

    unsafe fn hicon_to_rgba(icon: isize) -> Option<Decoded> {
        // SAFETY: `icon` es un HICON válido del llamante; GetIconInfo crea dos mapas de bits que se
        // borran aquí; `buf` tiene w*h*4 bytes, justo lo que GetDIBits escribe a 32 bpp.
        unsafe {
            let mut ii = IconInfo { is_icon: 0, xh: 0, yh: 0, mask: 0, color: 0 };
            if GetIconInfo(icon, &mut ii) == 0 {
                return None;
            }
            let mut bm = Bitmap { ty: 0, w: 0, h: 0, stride: 0, planes: 0, bpp: 0, bits: std::ptr::null_mut() };
            let mut out = None;
            if ii.color != 0 && GetObjectW(ii.color, std::mem::size_of::<Bitmap>() as i32, &mut bm) != 0 && bm.w > 0 && bm.h > 0 {
                let (w, h) = (bm.w as usize, bm.h as usize);
                let mut buf = vec![0u8; w * h * 4];
                let mut bi = BitmapInfoHeader {
                    size: std::mem::size_of::<BitmapInfoHeader>() as u32,
                    w: w as i32,
                    h: -(h as i32),
                    planes: 1,
                    bpp: 32,
                    compression: 0,
                    size_image: 0,
                    xppm: 0,
                    yppm: 0,
                    used: 0,
                    important: 0,
                };
                let dc = CreateCompatibleDC(0);
                let n = GetDIBits(dc, ii.color, 0, h as u32, buf.as_mut_ptr() as *mut c_void, &mut bi, 0);
                DeleteDC(dc);
                if n != 0 {
                    let opaque_missing = buf.chunks_exact(4).all(|p| p[3] == 0);
                    for p in buf.chunks_exact_mut(4) {
                        p.swap(0, 2);
                        if opaque_missing {
                            p[3] = 255;
                        }
                    }
                    out = Some(Decoded { w, h, rgba: buf });
                }
            }
            if ii.color != 0 {
                DeleteObject(ii.color);
            }
            if ii.mask != 0 {
                DeleteObject(ii.mask);
            }
            out
        }
    }

    pub fn icon_for(name: &str, dir: bool) -> Option<Decoded> {
        icon_query(name, if dir { 0x10 } else { 0x80 }, SHGFI_USEFILEATTRIBUTES)
    }

    /// Icono del archivo o unidad real (no el genérico de su extensión).
    pub fn icon_for_path(path: &str) -> Option<Decoded> {
        icon_query(path, 0, 0)
    }

    fn icon_query(name: &str, attrs: u32, extra: u32) -> Option<Decoded> {
        // SAFETY: `wide` termina en NUL y vive durante las llamadas; las estructuras son #[repr(C)] con
        // la disposición de SHFILEINFOW; cada HICON obtenido se destruye una sola vez.
        unsafe {
            let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
            let mut fi: ShFileInfoW = std::mem::zeroed();
            // Primero el índice en la lista de imágenes del sistema (hasta 256 px).
            let r = SHGetFileInfoW(
                wide.as_ptr(),
                attrs,
                &mut fi,
                std::mem::size_of::<ShFileInfoW>() as u32,
                SHGFI_SYSICONINDEX | extra,
            );
            if r != 0 {
                // IID_IImageList
                let iid = Guid(0x46EB5926, 0x582E, 0x4017, [0x9F, 0xDF, 0xE8, 0x99, 0x8D, 0xAA, 0x09, 0x50]);
                for list in [4, 2] {
                    // SHIL_JUMBO (256) y SHIL_EXTRALARGE (48)
                    let mut il: *mut c_void = std::ptr::null_mut();
                    if SHGetImageList(list, &iid, &mut il) == 0 && !il.is_null() {
                        // SAFETY: `il` es un IImageList válido (S_OK y no nulo) con una referencia nuestra;
                        // su vtable empieza por IUnknown (Release = nº 2) y GetIcon es el método nº 10.
                        let vtbl = *(il as *mut *const usize);
                        let get_icon: unsafe extern "system" fn(*mut c_void, i32, u32, *mut isize) -> i32 =
                            std::mem::transmute(*vtbl.add(10));
                        let release: unsafe extern "system" fn(*mut c_void) -> u32 = std::mem::transmute(*vtbl.add(2));
                        let mut hicon: isize = 0;
                        let d = if get_icon(il, fi.index, 0, &mut hicon) == 0 && hicon != 0 {
                            let d = hicon_to_rgba(hicon);
                            DestroyIcon(hicon);
                            d
                        } else {
                            None
                        };
                        // SHGetImageList entrega la interfaz con una referencia que hay que soltar
                        release(il);
                        if d.is_some() {
                            return d;
                        }
                    }
                }
            }
            // Alternativa: icono grande clásico (32 px).
            let mut fi: ShFileInfoW = std::mem::zeroed();
            let r = SHGetFileInfoW(
                wide.as_ptr(),
                attrs,
                &mut fi,
                std::mem::size_of::<ShFileInfoW>() as u32,
                SHGFI_ICON | extra,
            );
            if r != 0 && fi.icon != 0 {
                let d = hicon_to_rgba(fi.icon);
                DestroyIcon(fi.icon);
                return d;
            }
            None
        }
    }
}
