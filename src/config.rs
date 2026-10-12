//! Ajustes persistentes en un fichero de texto `clave=valor` (sin serde).
use crate::fsys::SortKey;
use std::path::PathBuf;

pub struct Config {
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
    pub shell: String,
    pub native_deco: bool,
    pub show_menu_btn: bool,
    pub lang: String,
    pub restore_tabs: bool,
    pub preview_window: bool,
    /// Columnas de la vista de lista (aparte del nombre), de izquierda a derecha: date, size, kind.
    pub columns: Vec<String>,
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
            shell: String::new(),
            native_deco: false,
            show_menu_btn: true,
            lang: "auto".into(),
            restore_tabs: true,
            preview_window: true,
            columns: vec!["date".into(), "size".into(), "kind".into()],
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
                "sort" => c.sort = SortKey::from_str(v),
                "asc" => c.asc = v != "0",
                "sys_icons" => c.sys_icons = v != "0",
                "thumbs" => c.thumbs_on = v != "0",
                "sidebar" => c.show_sidebar = v != "0",
                "term_open" => c.term_open = v == "1",
                "term_h" => c.term_h = v.parse().unwrap_or(240.0f32).clamp(100.0, 900.0),
                "menu_btn" => c.show_menu_btn = v != "0",
                "lang" => c.lang = v.to_string(),
                "restore_tabs" => c.restore_tabs = v != "0",
                "preview_window" => c.preview_window = v != "0",
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
        c
    }

    pub fn save(&self) {
        let Some(f) = file() else { return };
        if let Some(p) = f.parent() {
            let _ = std::fs::create_dir_all(p);
        }
        let mut s = String::new();
        for b in &self.bookmarks {
            s.push_str(&format!("bookmark={}\n", b.display()));
        }
        s.push_str(&format!("grid={}\n", self.grid as u8));
        s.push_str(&format!("zoom={}\n", self.zoom));
        s.push_str(&format!("hidden={}\n", self.show_hidden as u8));
        s.push_str(&format!("dark={}\n", self.dark as u8));
        s.push_str(&format!("theme={}\n", self.theme_mode));
        s.push_str(&format!("sort={}\n", self.sort.as_str()));
        s.push_str(&format!("asc={}\n", self.asc as u8));
        if let Some(d) = &self.last_dir {
            s.push_str(&format!("last_dir={}\n", d.display()));
        }
        s.push_str(&format!("sys_icons={}\n", self.sys_icons as u8));
        s.push_str(&format!("thumbs={}\n", self.thumbs_on as u8));
        s.push_str(&format!("sidebar={}\n", self.show_sidebar as u8));
        s.push_str(&format!("term_open={}\n", self.term_open as u8));
        s.push_str(&format!("term_h={}\n", self.term_h));
        s.push_str(&format!("menu_btn={}\n", self.show_menu_btn as u8));
        s.push_str(&format!("lang={}\n", self.lang));
        s.push_str(&format!("restore_tabs={}\n", self.restore_tabs as u8));
        s.push_str(&format!("preview_window={}\n", self.preview_window as u8));
        if let Some(v) = self.single_click {
            s.push_str(&format!("single_click={}\n", v as u8));
        }
        s.push_str(&format!("columns={}\n", self.columns.join(",")));
        s.push_str(&format!("split={}\n", self.split as u8));
        for (id, spec) in &self.keys {
            s.push_str(&format!("key.{id}={spec}\n"));
        }
        s.push_str(&format!("split_stack={}\n", self.split_stack as u8));
        s.push_str(&format!("split_ratio={}\n", self.split_ratio));
        if let Some(d) = &self.split_dir {
            s.push_str(&format!("split_dir={}\n", d.display()));
        }
        for t in &self.tabs {
            s.push_str(&format!("tab={}\n", t.display()));
        }
        for (i, t) in &self.tabs2 {
            s.push_str(&format!("tab2={}={}\n", i, t.display()));
        }
        s.push_str(&format!("cur_tab={}\n", self.cur_tab));
        s.push_str(&format!("native_deco={}\n", self.native_deco as u8));
        s.push_str(&format!("shell={}\n", self.shell));
        s.push_str(&format!("win={}x{}\n", self.win.0, self.win.1));
        let _ = std::fs::write(f, s);
    }
}
