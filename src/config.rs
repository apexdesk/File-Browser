//! Ajustes persistentes en un fichero de texto `clave=valor` (sin serde).
#[allow(unused_imports)]
use crate::vfs::{KindV as _, PathV as _};
use crate::fsys::SortKey;
use std::path::PathBuf;

/// Tema de color creado por el usuario. `colors`: fondo, panel, superficie, resaltado, borde,
/// texto, texto secundario, acento y fondo del visor.
#[derive(Clone, PartialEq)]
pub struct CustomTheme {
    pub name: String,
    pub dark: bool,
    pub colors: [[u8; 3]; 9],
}

pub struct Config {
    pub custom_themes: Vec<CustomTheme>,
    /// Nombre del tema (propio o predefinido) activo ahora ('' = el claro/oscuro de serie); se deriva de las dos ranuras.
    pub theme_custom: String,
    /// Tema elegido para el modo claro y para el oscuro ('' = el de serie). «Sistema» alterna entre ambos.
    pub theme_light: String,
    pub theme_dark: String,
    pub bookmarks: Vec<PathBuf>,
    /// Favoritos de serie que el usuario ha ocultado y orden elegido para los visibles.
    pub fav_hidden: Vec<PathBuf>,
    pub fav_order: Vec<PathBuf>,
    /// Lista completa de favoritos (de serie y del usuario) en su orden; `None` = configuración antigua.
    pub favs: Option<Vec<PathBuf>>,
    /// Nombres personalizados de favoritos.
    pub fav_labels: Vec<(PathBuf, String)>,
    /// Icono elegido para una carpeta (`foldericons::Spec` codificado).
    pub folder_icons: Vec<(PathBuf, String)>,
    /// Leer los iconos personalizados de carpetas del sistema (GNOME/KDE, desktop.ini, Finder).
    pub folder_icons_os: bool,
    pub grid: bool,
    pub zoom: f32,
    pub show_hidden: bool,
    pub dark: bool,
    /// 0 = según el sistema, 1 = claro, 2 = oscuro.
    pub theme_mode: u8,
    pub sort: SortKey,
    pub asc: bool,
    pub last_dir: Option<PathBuf>,
    pub win: (f32, f32),
    pub sys_icons: bool,
    pub thumbs_on: bool,
    /// Escala de toda la interfaz (zoom de egui): 0.75 a 2.0.
    pub ui_scale: f32,
    /// Pedir confirmación antes de mover a la papelera.
    pub confirm_trash: bool,
    /// Vista al arrancar: 0 = la última usada, 1 = cuadrícula, 2 = lista, 3 = columnas.
    pub start_view: u8,
    /// Reducir animaciones: 0 = según el sistema, 1 = sí, 2 = no.
    pub reduce_motion: u8,
    /// Primer arranque: falta responder a la bienvenida. Sólo se guarda mientras es verdad (si se cierra
    /// sin responder, vuelve a salir); los archivos sin la clave no son primer arranque.
    pub first_run: bool,
    pub show_sidebar: bool,
    pub term_open: bool,
    pub term_h: f32,
    /// Ancho del panel de vista previa acoplado.
    pub preview_w: f32,
    pub shell: String,
    /// Terminal del sistema («Abrir en el terminal del sistema»): orden con `{dir}` opcional; vacío = automático.
    pub sys_term: String,
    pub native_deco: bool,
    pub show_menu_btn: bool,
    pub lang: String,
    /// Formato de las fechas: "auto", "iso", "short" o "long" (`fsys::DateStyle`).
    pub date_fmt: String,
    pub restore_tabs: bool,
    pub index_enabled: bool,
    pub preview_window: bool,
    pub preview_panel: bool,
    /// Columnas de la vista de lista (aparte del nombre), de izquierda a derecha: date, size, kind.
    pub columns: Vec<String>,
    /// Anchuras personalizadas de columnas de la lista: (id, ancho).
    pub col_w: Vec<(String, f32)>,
    /// Mostrar el panel del portapapeles cuando hay algo copiado.
    pub clip_panel: bool,
    /// Vista de columnas (jerarquía horizontal); excluye la vista dividida.
    pub col_view: bool,
    /// Archivos abiertos recientemente (el más reciente primero).
    pub recent: Vec<PathBuf>,
    /// Abrir con un solo clic/toque; `None` = según la plataforma (sí en Android, no en PC).
    pub single_click: Option<bool>,
    pub split: bool,
    pub keys: Vec<(String, String)>,
    pub split_stack: bool,
    pub split_ratio: f32,
    pub split_dir: Option<PathBuf>,
    pub tabs: Vec<PathBuf>,
    pub tabs2: Vec<(usize, PathBuf)>,
    pub cur_tab: usize,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            custom_themes: Vec::new(),
            theme_custom: String::new(),
            theme_light: String::new(),
            theme_dark: String::new(),
            bookmarks: Vec::new(),
            fav_hidden: Vec::new(),
            fav_order: Vec::new(),
            favs: None,
            fav_labels: Vec::new(),
            folder_icons: Vec::new(),
            folder_icons_os: true,
            grid: true,
            zoom: 120.0,
            show_hidden: false,
            dark: true,
            theme_mode: 0,
            sort: SortKey::Name,
            asc: true,
            last_dir: None,
            win: (1180.0, 740.0),
            sys_icons: true,
            thumbs_on: true,
            ui_scale: 1.0,
            confirm_trash: false,
            start_view: 0,
            reduce_motion: 0,
            first_run: false,
            show_sidebar: true,
            term_open: false,
            term_h: 240.0,
            preview_w: 360.0,
            shell: String::new(),
            sys_term: String::new(),
            native_deco: false,
            show_menu_btn: true,
            lang: "auto".into(),
            date_fmt: "auto".into(),
            restore_tabs: true,
            index_enabled: true,
            preview_window: true,
            preview_panel: false,
            columns: vec!["date".into(), "size".into(), "kind".into()],
            col_w: Vec::new(),
            clip_panel: true,
            col_view: false,
            recent: Vec::new(),
            single_click: None,
            split: false,
            keys: Vec::new(),
            split_stack: false,
            split_ratio: 0.5,
            split_dir: None,
            tabs: Vec::new(),
            tabs2: Vec::new(),
            cur_tab: 0,
        }
    }
}

/// Modo portable: si junto al ejecutable hay un archivo `portable.txt`, la configuración se guarda en
/// `data/config.txt` a su lado en vez de en la carpeta de configuración del usuario.
#[cfg(not(any(target_os = "android", target_arch = "wasm32")))]
fn portable_dir() -> Option<PathBuf> {
    let dir = std::env::current_exe().ok()?.parent()?.to_path_buf();
    dir.join("portable.txt").is_file().then(|| dir.join("data"))
}

#[cfg(any(target_os = "android", target_arch = "wasm32"))]
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
fn portable_dir() -> Option<PathBuf> {
    None
}

#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
fn file() -> Option<PathBuf> {
    if let Some(d) = portable_dir() {
        return Some(d.join("config.txt"));
    }
    dirs::config_dir().map(|d| d.join("filebrowser-rs").join("config.txt"))
}

/// Lo que este proceso cargó o guardó por última vez (en el formato de `serialize`): con ello se
/// sabe qué claves ha cambiado él y cuáles, al guardar, hay que respetar tal y como las dejó otra
/// ventana (cada ventana es un proceso con su propia copia de los ajustes).
static BASE: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// Claves que se repiten (una línea por elemento) y cuyos cambios de dos ventanas se suman
/// elemento a elemento; el resto de grupos se sustituye entero por el de quien lo cambió.
const SET_KEYS: &[&str] = &["bookmark", "fav_hidden", "ctheme", "fav_label", "folder_icon"];

/// Agrupa las líneas por clave conservando el orden de aparición.
fn groups(text: &str) -> Vec<(String, Vec<String>)> {
    let mut out: Vec<(String, Vec<String>)> = Vec::new();
    for l in text.lines() {
        let Some((k, _)) = l.split_once('=') else { continue };
        match out.iter_mut().find(|(g, _)| g == k) {
            Some((_, v)) => v.push(l.to_string()),
            None => out.push((k.to_string(), vec![l.to_string()])),
        }
    }
    out
}

/// Combina a tres bandas: `ours` (lo que este proceso quiere guardar), `base` (lo que creía que había)
/// y `disk` (lo que hay ahora). Los grupos que este proceso no ha tocado se quedan como en el disco.
fn merge_text(ours: &str, base: Option<&str>, disk: Option<&str>) -> String {
    let (Some(base), Some(disk)) = (base, disk) else { return ours.to_string() };
    let (og, bg, dg) = (groups(ours), groups(base), groups(disk));
    let get = |v: &[(String, Vec<String>)], k: &str| v.iter().find(|(g, _)| g == k).map(|(_, l)| l.clone());
    let mut out: Vec<String> = Vec::new();
    for (k, mine) in &og {
        let before = get(&bg, k).unwrap_or_default();
        let now = get(&dg, k);
        if *mine == before {
            // sin cambios aquí: manda lo que haya en el disco (puede haberlo cambiado otra ventana)
            out.extend(now.unwrap_or_default());
        } else if SET_KEYS.contains(&k.as_str()) {
            // altas y bajas propias sobre lo que hay en el disco
            let mut v: Vec<String> = now.unwrap_or_default().into_iter().filter(|l| !before.contains(l) || mine.contains(l)).collect();
            for l in mine {
                if !before.contains(l) && !v.contains(l) {
                    v.push(l.clone());
                }
            }
            out.extend(v);
        } else {
            out.extend(mine.iter().cloned());
        }
    }
    // grupos que sólo están en el disco: si este proceso los tenía, los ha quitado él; si no, son de otra ventana
    for (k, lines) in &dg {
        if og.iter().any(|(g, _)| g == k) {
            continue;
        }
        if get(&bg, k).is_some() {
            continue;
        }
        out.extend(lines.iter().cloned());
    }
    let mut s = out.join("\n");
    s.push('\n');
    s
}

/// Escritura atómica y duradera: temporal con nombre único junto al destino, fsync y `rename`.
/// Bajo un cerrojo (`config.lock`) se relee el archivo y se combina con lo que este proceso cambió.
#[cfg(not(target_arch = "wasm32"))]
fn save_text_to(f: &std::path::Path, ours: &str, base: &std::sync::Mutex<Option<String>>) -> std::io::Result<()> {
    use std::io::Write;
    let dir = f.parent().unwrap_or(std::path::Path::new("."));
    crate::vfs::fs::create_dir_all(dir)?;
    let lock = crate::vfs::fs::OpenOptions::new().create(true).truncate(false).write(true).open(f.with_extension("lock"));
    // sin cerrojo (sistema de archivos que no lo admite) se guarda igual
    if let Ok(l) = &lock {
        let _ = l.lock();
    }
    let mut b = base.lock().unwrap_or_else(|e| e.into_inner());
    let disk = crate::vfs::fs::read_to_string(f).ok();
    let text = merge_text(ours, b.as_deref(), disk.as_deref());
    let n = web_time::SystemTime::now().duration_since(web_time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    let name = f.file_name().map(|x| x.to_string_lossy().into_owned()).unwrap_or_default();
    let tmp = dir.join(format!(".{name}.{}.{n:x}.tmp", crate::vfs::pid()));
    let r = (|| {
        let mut w = crate::vfs::fs::OpenOptions::new().write(true).create_new(true).open(&tmp)?;
        w.write_all(text.as_bytes())?;
        w.sync_all()?;
        drop(w);
        crate::vfs::fs::rename(&tmp, f)?;
        // la entrada del directorio también al disco (si se puede abrir una carpeta)
        #[cfg(unix)]
        if let Ok(d) = crate::vfs::fs::File::open(dir) {
            let _ = d.sync_all();
        }
        Ok(())
    })();
    if r.is_err() {
        let _ = crate::vfs::fs::remove_file(&tmp);
    } else {
        *b = Some(ours.to_string());
    }
    r
}

impl Config {
    pub fn load() -> Config {
        let c = Self::load_inner();
        if let Ok(mut b) = BASE.lock() {
            *b = Some(c.serialize());
        }
        c
    }

    fn load_inner() -> Config {
        #[cfg(target_arch = "wasm32")]
        let txt = crate::vfs::net::store_get("filebrowser.config");
        #[cfg(not(target_arch = "wasm32"))]
        let txt = file().and_then(|f| crate::vfs::fs::read_to_string(f).ok());
        let Some(txt) = txt else {
            return Self::first_run_default();
        };
        Self::from_text(&txt)
    }

    /// Ajustes del primer arranque: los de serie, sin índice de nombres hasta que el usuario lo elija
    /// en la bienvenida.
    pub fn first_run_default() -> Config {
        let mut c = Config::default();
        c.first_run = true;
        c.index_enabled = false;
        c
    }

    /// Ajustes a partir del texto guardado (`serialize`); las claves que falten quedan como de serie.
    pub fn from_text(txt: &str) -> Config {
        Self::parse_into(Config::default(), txt)
    }

    /// Lee los ajustes de un texto `clave=valor` sobre `c`.
    fn parse_into(mut c: Config, txt: &str) -> Config {
        let mut has_theme = false;
        for line in txt.lines() {
            let Some((k, v)) = line.split_once('=') else { continue };
            match k {
                "bookmark" => c.bookmarks.push(PathBuf::from(v)),
                "fav_hidden" => c.fav_hidden.push(PathBuf::from(v)),
                "fav_order" => c.fav_order.push(PathBuf::from(v)),
                "fav" => {
                    let l = c.favs.get_or_insert_with(Vec::new);
                    if l.len() < 1000 {
                        l.push(PathBuf::from(v));
                    }
                }
                "fav_label" => {
                    if let Some(x) = crate::favs::split_pair(v).filter(|_| c.fav_labels.len() < 1000) {
                        c.fav_labels.push(x);
                    }
                }
                "folder_icon" => {
                    if let Some(x) = crate::favs::split_pair(v).filter(|_| c.folder_icons.len() < 5000) {
                        c.folder_icons.retain(|(p, _)| *p != x.0);
                        c.folder_icons.push(x);
                    }
                }
                "folder_icons_os" => c.folder_icons_os = v != "0",
                "recent" => {
                    if c.recent.len() < 40 {
                        c.recent.push(PathBuf::from(v));
                    }
                }
                "clip_panel" => c.clip_panel = v != "0",
                "col_view" => c.col_view = v == "1",
                k if k.starts_with("colw.") => {
                    if let Some(w) = v.parse::<f32>().ok().filter(|x| x.is_finite()) {
                        c.col_w.push((k["colw.".len()..].to_string(), w.clamp(40.0, 400.0)));
                    }
                }
                "grid" => c.grid = v == "1",
                "zoom" => c.zoom = v.parse().ok().filter(|x: &f32| x.is_finite()).unwrap_or(120.0f32).clamp(64.0, 200.0),
                "hidden" => c.show_hidden = v == "1",
                "dark" => {
                    c.dark = v != "0";
                    if !has_theme {
                        c.theme_mode = if c.dark { 2 } else { 1 };
                    }
                }
                "theme" => {
                    has_theme = true;
                    c.theme_mode = v.parse().unwrap_or(0u8).min(2);
                }
                "theme_custom" => c.theme_custom = v.to_string(),
                "theme_light" => c.theme_light = v.to_string(),
                "theme_dark" => c.theme_dark = v.to_string(),
                "ctheme" => {
                    // nombre|0/1|rrggbb,… (9 colores)
                    let mut it = v.splitn(3, '|');
                    if let (Some(n), Some(d), Some(cs)) = (it.next(), it.next(), it.next()) {
                        let mut colors = [[0u8; 3]; 9];
                        let hex: Vec<&str> = cs.split(',').collect();
                        if hex.len() == 9 && !n.is_empty() {
                            let mut ok = true;
                            for (i, h) in hex.iter().enumerate() {
                                match u32::from_str_radix(h, 16) {
                                    Ok(x) if h.len() == 6 => colors[i] = [(x >> 16) as u8, (x >> 8) as u8, x as u8],
                                    _ => ok = false,
                                }
                            }
                            if ok && c.custom_themes.len() < 32 {
                                c.custom_themes.push(CustomTheme { name: n.to_string(), dark: d == "1", colors });
                            }
                        }
                    }
                }
                "sort" => c.sort = SortKey::from_str(v),
                "asc" => c.asc = v != "0",
                "sys_icons" => c.sys_icons = v != "0",
                "thumbs" => c.thumbs_on = v != "0",
                "ui_scale" => c.ui_scale = v.parse().ok().filter(|x: &f32| x.is_finite()).unwrap_or(1.0f32).clamp(0.75, 2.0),
                "confirm_trash" => c.confirm_trash = v == "1",
                "start_view" => c.start_view = v.parse().unwrap_or(0u8).min(3),
                "reduce_motion" => c.reduce_motion = v.parse().unwrap_or(0u8).min(2),
                "first_run" => c.first_run = v == "1",
                "sidebar" => c.show_sidebar = v != "0",
                "term_open" => c.term_open = v == "1",
                "preview_w" => c.preview_w = v.parse().ok().filter(|x: &f32| x.is_finite()).unwrap_or(360.0f32).clamp(220.0, 760.0),
                "term_h" => c.term_h = v.parse().ok().filter(|x: &f32| x.is_finite()).unwrap_or(240.0f32).clamp(100.0, 900.0),
                "menu_btn" => c.show_menu_btn = v != "0",
                "lang" => c.lang = v.to_string(),
                "date_fmt" => c.date_fmt = crate::fsys::DateStyle::from_code(v).code().to_string(),
                "restore_tabs" => c.restore_tabs = v != "0",
                "index_enabled" => c.index_enabled = v == "1",
                "preview_window" => c.preview_window = v != "0",
                "preview_panel" => c.preview_panel = v == "1",
                "single_click" => c.single_click = Some(v == "1"),
                "columns" => {
                    let known = ["size", "date", "kind", "ext", "perms", "owner", "group", "items", "created", "accessed", "mode", "link"];
                    let mut cols: Vec<String> = Vec::new();
                    for x in v.split(',') {
                        if known.contains(&x) && !cols.iter().any(|c| c == x) {
                            cols.push(x.to_string());
                        }
                    }
                    c.columns = cols
                }
                "split" => c.split = v == "1",
                k if k.starts_with("key.") => c.keys.push((k[4..].to_string(), v.to_string())),
                "split_stack" => c.split_stack = v == "1",
                "split_ratio" => c.split_ratio = v.parse().ok().filter(|x: &f32| x.is_finite()).unwrap_or(0.5f32).clamp(0.2, 0.8),
                "split_dir" => c.split_dir = Some(PathBuf::from(v)),
                "tab" => c.tabs.push(PathBuf::from(v)),
                "tab2" => {
                    if let Some((i, p)) = v.split_once('=') {
                        if let Ok(i) = i.parse() {
                            c.tabs2.push((i, PathBuf::from(p)));
                        }
                    }
                }
                "cur_tab" => c.cur_tab = v.parse().unwrap_or(0),
                "native_deco" => c.native_deco = v == "1",
                "shell" => c.shell = v.to_string(),
                "sys_term" => c.sys_term = v.to_string(),
                "last_dir" => c.last_dir = Some(PathBuf::from(v)),
                "win" => {
                    if let Some((w, h)) = v.split_once('x') {
                        if let (Some(w), Some(h)) = (w.parse::<f32>().ok().filter(|x| x.is_finite()), h.parse::<f32>().ok().filter(|x| x.is_finite())) {
                            c.win = (w.clamp(640.0, 8000.0), h.clamp(420.0, 6000.0));
                        }
                    }
                }
                _ => {}
            }
        }
        // versiones anteriores guardaban un único tema propio que sustituía al modo: pasa a su ranura
        if !c.theme_custom.is_empty() && c.theme_light.is_empty() && c.theme_dark.is_empty() {
            if let Some(th) = c.theme_named(&c.theme_custom) {
                if th.dark {
                    c.theme_dark = th.name;
                } else {
                    c.theme_light = th.name;
                }
            }
        }
        c.sync_active();
        c
    }

    /// Tema por nombre: primero los propios, luego los predefinidos.
    pub fn theme_named(&self, name: &str) -> Option<CustomTheme> {
        if name.is_empty() {
            return None;
        }
        // los temas de serie en la ranura contraria: «@Claro» / «@Oscuro»
        if let Some(b) = name.strip_prefix('@') {
            let dark = match b {
                "Claro" => false,
                "Oscuro" => true,
                _ => return None,
            };
            return Some(CustomTheme { name: name.to_string(), dark, colors: crate::theme::palette(dark).colors() });
        }
        self.custom_themes.iter().find(|t| t.name == name).cloned().or_else(|| crate::theme::presets().iter().find(|t| t.name == name).cloned())
    }

    /// Hace que `theme_custom` sea el tema de la ranura del modo vigente (`dark`), si existe y su base coincide.
    pub fn sync_active(&mut self) {
        let name = if self.dark { &self.theme_dark } else { &self.theme_light };
        self.theme_custom = self.theme_named(name).map(|t| t.name).unwrap_or_default();
    }

    /// Guarda sin informar de errores (ver `try_save`).
    pub fn save(&self) {
        let _ = self.try_save();
    }

    /// Guarda los ajustes. Sólo se escriben los grupos de claves que este proceso ha cambiado; los
    /// demás se conservan como estén en el disco (otra ventana puede haberlos cambiado).
    pub fn try_save(&self) -> Result<(), String> {
        let s = self.serialize();
        #[cfg(target_arch = "wasm32")]
        {
            let mut b = BASE.lock().unwrap_or_else(|e| e.into_inner());
            let disk = crate::vfs::net::store_get("filebrowser.config");
            crate::vfs::net::store_set("filebrowser.config", &merge_text(&s, b.as_deref(), disk.as_deref()));
            *b = Some(s);
            Ok(())
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            let Some(f) = file() else { return Err(crate::i18n::t("sin carpeta de configuración").into()) };
            save_text_to(&f, &s, &BASE).map_err(|e| format!("{}: {e}", f.display()))
        }
    }

    fn serialize(&self) -> String {
        // la carpeta espejo de un servidor SFTP cambia en cada ejecución: se guarda su dirección
        // sftp:// (ver `sftp::persist_path`), y lo de un servidor ya desconectado no se guarda
        fn portable(p: &std::path::Path) -> Option<PathBuf> {
            let q = crate::sftp::persist_path(p);
            (!crate::sftp::is_remote(&q)).then_some(q)
        }
        let mut s = String::new();
        for b in &self.bookmarks {
            s.push_str(&format!("bookmark={}\n", b.display().to_string().replace(['\n', '\r'], " ")));
        }
        for (k, v) in [("fav_hidden", &self.fav_hidden), ("fav_order", &self.fav_order)] {
            for b in v {
                s.push_str(&format!("{k}={}\n", b.display().to_string().replace(['\n', '\r'], " ")));
            }
        }
        for b in self.favs.iter().flatten() {
            s.push_str(&format!("fav={}\n", b.display().to_string().replace(['\n', '\r'], " ")));
        }
        for (p, l) in &self.fav_labels {
            s.push_str(&format!("fav_label={}\n", crate::favs::pair(p, l)));
        }
        for (p, v) in &self.folder_icons {
            s.push_str(&format!("folder_icon={}\n", crate::favs::pair(p, v)));
        }
        s.push_str(&format!("folder_icons_os={}\n", self.folder_icons_os as u8));
        s.push_str(&format!("grid={}\n", self.grid as u8));
        s.push_str(&format!("zoom={}\n", self.zoom));
        s.push_str(&format!("hidden={}\n", self.show_hidden as u8));
        s.push_str(&format!("dark={}\n", self.dark as u8));
        s.push_str(&format!("theme={}\n", self.theme_mode));
        s.push_str(&format!("theme_light={}\n", self.theme_light.replace(['\n', '\r'], " ")));
        s.push_str(&format!("theme_dark={}\n", self.theme_dark.replace(['\n', '\r'], " ")));
        for th in &self.custom_themes {
            let hex: Vec<String> = th.colors.iter().map(|c| format!("{:02x}{:02x}{:02x}", c[0], c[1], c[2])).collect();
            s.push_str(&format!("ctheme={}|{}|{}\n", th.name.replace(['\n', '\r'], " "), th.dark as u8, hex.join(",")));
        }
        s.push_str(&format!("sort={}\n", self.sort.as_str()));
        s.push_str(&format!("asc={}\n", self.asc as u8));
        if let Some(d) = self.last_dir.as_deref().and_then(portable) {
            s.push_str(&format!("last_dir={}\n", d.display().to_string().replace(['\n', '\r'], " ")));
        }
        s.push_str(&format!("sys_icons={}\n", self.sys_icons as u8));
        s.push_str(&format!("thumbs={}\n", self.thumbs_on as u8));
        s.push_str(&format!("ui_scale={}\n", self.ui_scale));
        s.push_str(&format!("confirm_trash={}\n", self.confirm_trash as u8));
        s.push_str(&format!("start_view={}\n", self.start_view));
        s.push_str(&format!("reduce_motion={}\n", self.reduce_motion));
        if self.first_run {
            s.push_str("first_run=1\n");
        }
        s.push_str(&format!("sidebar={}\n", self.show_sidebar as u8));
        s.push_str(&format!("term_open={}\n", self.term_open as u8));
        s.push_str(&format!("term_h={}\n", self.term_h));
        s.push_str(&format!("preview_w={}\n", self.preview_w));
        s.push_str(&format!("menu_btn={}\n", self.show_menu_btn as u8));
        s.push_str(&format!("lang={}\n", self.lang));
        s.push_str(&format!("date_fmt={}\n", self.date_fmt));
        s.push_str(&format!("restore_tabs={}\n", self.restore_tabs as u8));
        s.push_str(&format!("index_enabled={}\n", self.index_enabled as u8));
        s.push_str(&format!("preview_window={}\n", self.preview_window as u8));
        s.push_str(&format!("preview_panel={}\n", self.preview_panel as u8));
        if let Some(v) = self.single_click {
            s.push_str(&format!("single_click={}\n", v as u8));
        }
        s.push_str(&format!("columns={}\n", self.columns.join(",")));
        s.push_str(&format!("clip_panel={}\n", self.clip_panel as u8));
        s.push_str(&format!("col_view={}\n", self.col_view as u8));
        let mut seen = std::collections::HashSet::new();
        for r in self.recent.iter().filter_map(|r| portable(r)).filter(|r| seen.insert(r.clone())) {
            s.push_str(&format!("recent={}\n", r.display().to_string().replace(['\n', '\r'], " ")));
        }
        for (id, w) in &self.col_w {
            s.push_str(&format!("colw.{id}={w}\n"));
        }
        s.push_str(&format!("split={}\n", self.split as u8));
        for (id, spec) in &self.keys {
            s.push_str(&format!("key.{id}={spec}\n"));
        }
        s.push_str(&format!("split_stack={}\n", self.split_stack as u8));
        s.push_str(&format!("split_ratio={}\n", self.split_ratio));
        if let Some(d) = self.split_dir.as_deref().and_then(portable) {
            s.push_str(&format!("split_dir={}\n", d.display().to_string().replace(['\n', '\r'], " ")));
        }
        // una pestaña de un servidor ya desconectado no se puede guardar: se quita sin descolocar los
        // índices de `tab2` y `cur_tab`
        let mut idx = Vec::with_capacity(self.tabs.len());
        let (mut cur, mut kept) = (0, 0);
        for (n, t) in self.tabs.iter().enumerate() {
            match portable(t) {
                Some(t) => {
                    if n <= self.cur_tab {
                        cur = kept;
                    }
                    idx.push(Some(kept));
                    kept += 1;
                    s.push_str(&format!("tab={}\n", t.display().to_string().replace(['\n', '\r'], " ")));
                }
                None => idx.push(None),
            }
        }
        for (i, t) in &self.tabs2 {
            let (Some(Some(i)), Some(t)) = (idx.get(*i), portable(t)) else { continue };
            s.push_str(&format!("tab2={}={}\n", i, t.display().to_string().replace(['\n', '\r'], " ")));
        }
        s.push_str(&format!("cur_tab={}\n", if self.tabs.is_empty() { self.cur_tab } else { cur }));
        s.push_str(&format!("native_deco={}\n", self.native_deco as u8));
        s.push_str(&format!("shell={}\n", self.shell.replace(['\n', '\r'], " ")));
        s.push_str(&format!("sys_term={}\n", self.sys_term.replace(['\n', '\r'], " ")));
        s.push_str(&format!("win={}x{}\n", self.win.0, self.win.1));
        s
    }
}

#[cfg(test)]
mod theme_slot_tests {
    use super::*;

    #[test]
    fn slots_follow_mode_and_base() {
        let mut c = Config::default();
        c.theme_dark = "Nord".into();
        c.theme_light = "".into(); // vacío: el de serie
        c.dark = true;
        c.sync_active();
        assert_eq!(c.theme_custom, "Nord");
        c.dark = false;
        c.sync_active();
        assert_eq!(c.theme_custom, "");
        c.theme_light = "@Oscuro".into(); // cualquier tema vale en cualquier ranura
        c.sync_active();
        assert_eq!(c.theme_custom, "@Oscuro");
        c.theme_light = "Paper".into();
        c.sync_active();
        assert_eq!(c.theme_custom, "Paper");
        assert!(crate::theme::presets().iter().all(|t| t.colors.len() == 9));
    }

    #[test]
    fn old_single_custom_theme_migrates_to_its_slot() {
        let mut c = Config::default();
        c.custom_themes.push(CustomTheme { name: "Mio".into(), dark: true, colors: [[1, 2, 3]; 9] });
        c.theme_custom = "Mio".into();
        let th = c.theme_named(&c.theme_custom).unwrap();
        assert!(th.dark);
    }
}

#[cfg(test)]
mod save_tests {
    use super::*;

    #[test]
    fn merge_keeps_other_windows_changes() {
        let base = "dark=1\nzoom=1.0\nbookmark=/a\n";
        // otra ventana cambió `zoom` y añadió un marcador
        let disk = "dark=1\nzoom=1.5\nbookmark=/a\nbookmark=/b\n";
        // ésta cambió `dark` y quitó /a y añadió /c
        let ours = "dark=0\nzoom=1.0\nbookmark=/c\n";
        let m = merge_text(ours, Some(base), Some(disk));
        let lines: Vec<&str> = m.lines().collect();
        assert!(lines.contains(&"dark=0"));
        assert!(lines.contains(&"zoom=1.5"), "{m}");
        assert!(lines.contains(&"bookmark=/b") && lines.contains(&"bookmark=/c") && !lines.contains(&"bookmark=/a"), "{m}");
        // sin base o sin disco, manda lo propio
        assert_eq!(merge_text(ours, None, Some(disk)), ours);
        assert_eq!(merge_text(ours, Some(base), None), ours);
        // una clave nueva de otra ventana se conserva; una que esta ventana quitó, no
        let m = merge_text("dark=1\n", Some("dark=1\nold=1\n"), Some("dark=1\nold=1\nnew=1\n"));
        assert_eq!(m, "dark=1\nnew=1\n");
    }

    /// Varias «ventanas» guardando a la vez: cada una conserva lo de las demás y nunca queda un
    /// archivo a medias ni temporales sueltos.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn concurrent_saves_do_not_clobber() {
        let n = web_time::SystemTime::now().duration_since(web_time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
        let dir = crate::vfs::temp_dir().join(format!("fbw1-cfg-{}-{n:x}", crate::vfs::pid()));
        crate::vfs::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("config.txt");
        let initial = "dark=1\nbookmark=/a\n";
        crate::vfs::fs::write(&f, initial).unwrap();
        let hs: Vec<_> = (0..8)
            .map(|i| {
                let f = f.clone();
                std::thread::spawn(move || {
                    // cada hilo es una ventana con su propia base
                    let base = std::sync::Mutex::new(Some(initial.to_string()));
                    let ours = format!("dark=1\nk{i}={i}\nbookmark=/a\nbookmark=/w{i}\n");
                    for _ in 0..5 {
                        save_text_to(&f, &ours, &base).unwrap();
                    }
                })
            })
            .collect();
        for h in hs {
            h.join().unwrap();
        }
        let txt = crate::vfs::fs::read_to_string(&f).unwrap();
        for i in 0..8 {
            assert!(txt.lines().any(|l| l == format!("k{i}={i}")), "falta k{i}: {txt}");
            assert!(txt.lines().any(|l| l == format!("bookmark=/w{i}")), "falta /w{i}: {txt}");
        }
        let left: Vec<_> = crate::vfs::fs::read_dir(&dir).unwrap().flatten().map(|e| e.file_name()).filter(|n| n.to_string_lossy().ends_with(".tmp")).collect();
        assert!(left.is_empty(), "{left:?}");
        let _ = crate::vfs::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod h1_settings_tests {
    use super::*;

    #[test]
    fn new_settings_round_trip_and_defaults() {
        // archivo de una versión anterior: sin las claves nuevas, valen las de serie y no es primer arranque
        let old = Config::from_text("grid=0\nzoom=100\nindex_enabled=1\n");
        assert_eq!(old.ui_scale, 1.0);
        assert!(!old.confirm_trash);
        assert_eq!(old.start_view, 0);
        assert_eq!(old.reduce_motion, 0);
        assert!(!old.first_run);
        assert!(old.index_enabled);
        let mut c = Config::default();
        c.ui_scale = 1.25;
        c.confirm_trash = true;
        c.start_view = 3;
        c.reduce_motion = 2;
        let back = Config::from_text(&c.serialize());
        assert_eq!(back.ui_scale, 1.25);
        assert!(back.confirm_trash);
        assert_eq!(back.start_view, 3);
        assert_eq!(back.reduce_motion, 2);
        assert!(!back.first_run);
        // bienvenida sin responder: sigue pendiente tras guardar; respondida, la clave desaparece
        let mut f = Config::first_run_default();
        let pending = Config::from_text(&f.serialize());
        assert!(pending.first_run && !pending.index_enabled);
        f.first_run = false;
        assert!(!f.serialize().contains("first_run"));
        assert!(!Config::from_text(&f.serialize()).first_run);
        // valores fuera de rango o rotos
        let bad = Config::from_text("ui_scale=9\nstart_view=7\nreduce_motion=x\n");
        assert_eq!(bad.ui_scale, 2.0);
        assert_eq!(bad.start_view, 3);
        assert_eq!(bad.reduce_motion, 0);
        let nan = Config::from_text("ui_scale=NaN\n");
        assert_eq!(nan.ui_scale, 1.0);
        // sin archivo: primer arranque y sin índice hasta que se elija
        let f = Config::first_run_default();
        assert!(f.first_run && !f.index_enabled);
    }
}

#[cfg(test)]
mod fav_tests {
    use super::*;

    #[test]
    fn old_config_with_bookmarks_round_trips() {
        let old = "bookmark=/w/a\nbookmark=/w/b\nfav_hidden=/h/Pics\nfav_order=/h/Docs\nfav_order=/h\ngrid=1\n";
        let c = Config::parse_into(Config::default(), old);
        assert_eq!(c.favs, None);
        assert_eq!(c.bookmarks.len(), 2);
        assert!(c.folder_icons_os);
        let builtins: Vec<PathBuf> = ["/h", "/h/Docs", "/h/Pics"].iter().map(PathBuf::from).collect();
        let list = crate::favs::merged(&builtins, c.favs.as_deref(), &c.fav_hidden, &c.fav_order, &c.bookmarks);
        assert_eq!(list, ["/h/Docs", "/h", "/w/a", "/w/b"].iter().map(PathBuf::from).collect::<Vec<_>>());
        // se guarda en el formato nuevo (y el antiguo) y se lee igual
        let mut c = c;
        let (f, o, b) = crate::favs::store(&list, &builtins);
        c.favs = Some(f);
        c.fav_order = o;
        c.bookmarks = b;
        crate::favs::set_label(&mut c.fav_labels, std::path::Path::new("/w/a"), "Trabajo | 2026");
        c.folder_icons.push((PathBuf::from("/w/b"), "drawn:2:3".into()));
        c.folder_icons_os = false;
        let txt = c.serialize();
        assert!(txt.contains("bookmark=/w/a\n") && txt.contains("fav=/w/a\n") && txt.contains("fav_order=/h/Docs\n"), "{txt}");
        let d = Config::parse_into(Config::default(), &txt);
        assert_eq!(d.favs, c.favs);
        assert_eq!(d.fav_labels, c.fav_labels);
        assert_eq!(d.folder_icons, c.folder_icons);
        assert!(!d.folder_icons_os);
        assert_eq!(crate::favs::merged(&builtins, d.favs.as_deref(), &d.fav_hidden, &d.fav_order, &d.bookmarks), list);
        // la última línea de icono de una ruta manda
        let e = Config::parse_into(Config::default(), "folder_icon=/x|drawn:1:1\nfolder_icon=/x|theme:folder-code\n");
        assert_eq!(e.folder_icons, vec![(PathBuf::from("/x"), "theme:folder-code".to_string())]);
    }

    #[test]
    fn stale_sftp_mirror_tabs_are_dropped() {
        // copia de un servidor de otra ejecución: ya no vale, y los índices de las demás se ajustan
        let stale = dirs::cache_dir().unwrap_or_else(crate::vfs::temp_dir).join("filebrowser/sftp/p1/u@h/x");
        let c = Config {
            tabs: vec![PathBuf::from("/a"), stale.clone(), PathBuf::from("/b")],
            tabs2: vec![(2, PathBuf::from("/c")), (1, PathBuf::from("/d"))],
            cur_tab: 2,
            recent: vec![stale, PathBuf::from("sftp://u@h/f.txt"), PathBuf::from("sftp://u@h/f.txt")],
            ..Config::default()
        };
        let e = Config::parse_into(Config::default(), &c.serialize());
        assert_eq!(e.tabs, vec![PathBuf::from("/a"), PathBuf::from("/b")]);
        assert_eq!(e.tabs2, vec![(1, PathBuf::from("/c"))]);
        assert_eq!(e.cur_tab, 1);
        assert_eq!(e.recent, vec![PathBuf::from("sftp://u@h/f.txt")]);
    }
}
