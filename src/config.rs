//! Ajustes persistentes en un fichero de texto `clave=valor` (sin serde).
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
    pub show_sidebar: bool,
    pub term_open: bool,
    pub term_h: f32,
    /// Ancho del panel de vista previa acoplado.
    pub preview_w: f32,
    pub shell: String,
    pub native_deco: bool,
    pub show_menu_btn: bool,
    pub lang: String,
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
            show_sidebar: true,
            term_open: false,
            term_h: 240.0,
            preview_w: 360.0,
            shell: String::new(),
            native_deco: false,
            show_menu_btn: true,
            lang: "auto".into(),
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

fn file() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("filebrowser-rs").join("config.txt"))
}

impl Config {
    pub fn load() -> Config {
        let mut c = Config::default();
        let Some(txt) = file().and_then(|f| std::fs::read_to_string(f).ok()) else {
            return c;
        };
        let mut has_theme = false;
        for line in txt.lines() {
            let Some((k, v)) = line.split_once('=') else { continue };
            match k {
                "bookmark" => c.bookmarks.push(PathBuf::from(v)),
                "recent" => {
                    if c.recent.len() < 40 {
                        c.recent.push(PathBuf::from(v));
                    }
                }
                "clip_panel" => c.clip_panel = v != "0",
                "col_view" => c.col_view = v == "1",
                k if k.starts_with("colw.") => {
                    if let Ok(w) = v.parse::<f32>() {
                        c.col_w.push((k["colw.".len()..].to_string(), w.clamp(40.0, 400.0)));
                    }
                }
                "grid" => c.grid = v == "1",
                "zoom" => c.zoom = v.parse().unwrap_or(120.0f32).clamp(64.0, 200.0),
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
                "sidebar" => c.show_sidebar = v != "0",
                "term_open" => c.term_open = v == "1",
                "preview_w" => c.preview_w = v.parse().unwrap_or(360.0f32).clamp(220.0, 760.0),
                "term_h" => c.term_h = v.parse().unwrap_or(240.0f32).clamp(100.0, 900.0),
                "menu_btn" => c.show_menu_btn = v != "0",
                "lang" => c.lang = v.to_string(),
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
                "split_ratio" => c.split_ratio = v.parse().unwrap_or(0.5f32).clamp(0.2, 0.8),
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
                "last_dir" => c.last_dir = Some(PathBuf::from(v)),
                "win" => {
                    if let Some((w, h)) = v.split_once('x') {
                        if let (Ok(w), Ok(h)) = (w.parse::<f32>(), h.parse::<f32>()) {
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

    pub fn save(&self) {
        let Some(f) = file() else { return };
        if let Some(p) = f.parent() {
            let _ = std::fs::create_dir_all(p);
        }
        let mut s = String::new();
        for b in &self.bookmarks {
            s.push_str(&format!("bookmark={}\n", b.display().to_string().replace(['\n', '\r'], " ")));
        }
        s.push_str(&format!("grid={}\n", self.grid as u8));
        s.push_str(&format!("zoom={}\n", self.zoom));
        s.push_str(&format!("hidden={}\n", self.show_hidden as u8));
        s.push_str(&format!("dark={}\n", self.dark as u8));
        s.push_str(&format!("theme={}\n", self.theme_mode));
        s.push_str(&format!("theme_light={}\n", self.theme_light.replace(['\n', '\r'], " ")));
        s.push_str(&format!("theme_dark={}\n", self.theme_dark.replace(['\n', '\r'], " ")));
        for th in &self.custom_themes {
            let hex: Vec<String> = th.colors.iter().map(|c| format!("{:02x}{:02x}{:02x}", c[0], c[1], c[2])).collect();
            s.push_str(&format!("ctheme={}|{}|{}\n", th.name, th.dark as u8, hex.join(",")));
        }
        s.push_str(&format!("sort={}\n", self.sort.as_str()));
        s.push_str(&format!("asc={}\n", self.asc as u8));
        if let Some(d) = &self.last_dir {
            s.push_str(&format!("last_dir={}\n", d.display().to_string().replace(['\n', '\r'], " ")));
        }
        s.push_str(&format!("sys_icons={}\n", self.sys_icons as u8));
        s.push_str(&format!("thumbs={}\n", self.thumbs_on as u8));
        s.push_str(&format!("sidebar={}\n", self.show_sidebar as u8));
        s.push_str(&format!("term_open={}\n", self.term_open as u8));
        s.push_str(&format!("term_h={}\n", self.term_h));
        s.push_str(&format!("preview_w={}\n", self.preview_w));
        s.push_str(&format!("menu_btn={}\n", self.show_menu_btn as u8));
        s.push_str(&format!("lang={}\n", self.lang));
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
        for r in &self.recent {
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
        if let Some(d) = &self.split_dir {
            s.push_str(&format!("split_dir={}\n", d.display().to_string().replace(['\n', '\r'], " ")));
        }
        for t in &self.tabs {
            s.push_str(&format!("tab={}\n", t.display().to_string().replace(['\n', '\r'], " ")));
        }
        for (i, t) in &self.tabs2 {
            s.push_str(&format!("tab2={}={}\n", i, t.display().to_string().replace(['\n', '\r'], " ")));
        }
        s.push_str(&format!("cur_tab={}\n", self.cur_tab));
        s.push_str(&format!("native_deco={}\n", self.native_deco as u8));
        s.push_str(&format!("shell={}\n", self.shell.replace(['\n', '\r'], " ")));
        s.push_str(&format!("win={}x{}\n", self.win.0, self.win.1));
        // escritura atómica: un corte a medias no deja la configuración vacía
        let tmp = f.with_extension("tmp");
        if std::fs::write(&tmp, s).is_ok() && std::fs::rename(&tmp, &f).is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
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
