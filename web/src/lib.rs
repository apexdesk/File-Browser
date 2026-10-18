//! Cliente web del File Browser: egui en WebAssembly sobre la API de filebrowser-server.
//! El aspecto sale del mismo `theme.rs` que usa el gestor de escritorio.

pub mod config;
pub mod fsys;
pub mod i18n;
#[path = "../../src/theme.rs"]
pub mod theme;

use egui::{vec2, Color32, CornerRadius, Margin, Sense};
use fsys::Kind;
use serde::Deserialize;
use std::sync::{Arc, Mutex};
use theme::{Glyph, Pal};

#[derive(Deserialize, Clone, Debug)]
pub struct Entry {
    pub n: String,
    pub d: bool,
    pub s: u64,
    pub m: i64,
}

#[derive(Deserialize, Clone, Debug, Default)]
pub struct Listing {
    pub path: Vec<String>,
    pub entries: Vec<Entry>,
    #[serde(default)]
    pub truncated: bool,
}

type Slot<T> = Arc<Mutex<Option<Result<T, String>>>>;

#[derive(Clone, Copy, PartialEq)]
enum SortBy {
    Name,
    Size,
    Date,
}

/// Las miniaturas se descargan y decodifican en el navegador: solo de imágenes pequeñas.
const THUMB_MAX: u64 = 3 << 20;

pub struct App {
    /// `/s/<token>/`
    root: String,
    /// `https://host:puerto` (el cargador de imágenes necesita URL absolutas)
    origin: String,
    cur: Listing,
    roots: Vec<String>,
    hist: Vec<Vec<String>>,
    hpos: usize,
    pending: Slot<Listing>,
    loading: bool,
    err: Option<String>,
    sel: Option<String>,
    filter: String,
    grid: bool,
    zoom: f32,
    sort: SortBy,
    desc: bool,
    dark: Option<bool>,
    pal: Pal,
}

pub fn enc(s: &str) -> String {
    let mut o = String::new();
    for &c in s.as_bytes() {
        if c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_' | b'.' | b'~') {
            o.push(c as char);
        } else {
            o.push_str(&format!("%{c:02X}"));
        }
    }
    o
}

fn human(n: u64) -> String {
    const U: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let (mut v, mut i) = (n as f64, 0);
    while v >= 1024.0 && i < U.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 { format!("{n} B") } else { format!("{v:.1} {}", U[i]) }
}

/// `2026-10-18 16:40` (UTC) a partir de segundos Unix; vacío si no hay fecha.
fn short_date(secs: i64) -> String {
    if secs == 0 {
        return String::new();
    }
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02} {:02}:{:02}", rem / 3600, rem % 3600 / 60)
}

fn ext_of(name: &str) -> &str {
    name.rsplit_once('.').map(|(_, e)| e).filter(|e| !e.is_empty() && !name.starts_with('.')).unwrap_or("")
}

fn kind_of(e: &Entry) -> Kind {
    if e.d { Kind::Folder } else { fsys::kind_of_ext(ext_of(&e.n)) }
}

fn toolbar_button(ui: &mut egui::Ui, pal: &Pal, g: Glyph, enabled: bool, active: bool, tip: &str) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(34.0, 34.0), if enabled { Sense::click() } else { Sense::hover() });
    let p = ui.painter();
    if enabled && (resp.hovered() || active) {
        p.rect_filled(rect, CornerRadius::same(10), if active { pal.sel } else { pal.hover });
    }
    let col = if !enabled { pal.bg.lerp_to_gamma(pal.subtle, 0.5) } else if active { pal.accent } else { pal.text };
    theme::draw_glyph(p, rect.center(), 20.0, g, col);
    if enabled { resp.on_hover_text(tip) } else { resp }
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, root: String, origin: String) -> Self {
        egui_extras::install_image_loaders(&cc.egui_ctx);
        let mut a = App {
            root,
            origin,
            cur: Listing::default(),
            roots: Vec::new(),
            hist: vec![Vec::new()],
            hpos: 0,
            pending: Arc::default(),
            loading: false,
            err: None,
            sel: None,
            filter: String::new(),
            grid: true,
            zoom: 112.0,
            sort: SortBy::Name,
            desc: false,
            dark: None,
            pal: theme::palette(true),
        };
        a.fetch(Vec::new(), &cc.egui_ctx);
        a
    }

    fn fetch(&mut self, path: Vec<String>, ctx: &egui::Context) {
        self.loading = true;
        let slot = self.pending.clone();
        let ctx = ctx.clone();
        let url = format!("{}api/list?p={}", self.root, enc(&path.join("/")));
        ehttp::fetch(ehttp::Request::get(url), move |r| {
            let v = match r {
                Ok(resp) if resp.ok => serde_json::from_slice::<Listing>(&resp.bytes).map_err(|e| e.to_string()),
                Ok(resp) => Err(format!("{} {}", resp.status, resp.status_text)),
                Err(e) => Err(e),
            };
            if let Ok(mut g) = slot.lock() {
                *g = Some(v);
            }
            ctx.request_repaint();
        });
    }

    /// Navega a `path` y lo añade al historial.
    fn go(&mut self, path: Vec<String>, ctx: &egui::Context) {
        if path == self.hist[self.hpos] && !self.cur.entries.is_empty() {
            return;
        }
        self.hist.truncate(self.hpos + 1);
        self.hist.push(path.clone());
        self.hpos += 1;
        self.filter.clear();
        self.sel = None;
        self.fetch(path, ctx);
    }

    fn go_hist(&mut self, pos: usize, ctx: &egui::Context) {
        self.hpos = pos;
        self.filter.clear();
        self.sel = None;
        let p = self.hist[pos].clone();
        self.fetch(p, ctx);
    }

    fn file_url(&self, name: &str) -> String {
        let mut u = self.root.clone();
        for c in &self.cur.path {
            u.push_str(&enc(c));
            u.push('/');
        }
        u.push_str(&enc(name));
        u
    }

    fn open(&mut self, e: &Entry, ctx: &egui::Context) {
        if e.d {
            let mut p = self.cur.path.clone();
            p.push(e.n.clone());
            self.go(p, ctx);
        } else {
            ctx.open_url(egui::OpenUrl::new_tab(self.file_url(&e.n)));
        }
    }

    /// Dibuja la miniatura de una imagen pequeña si ya está cargada; `false` si hay que usar el icono.
    fn thumb(&self, ui: &mut egui::Ui, e: &Entry, rect: egui::Rect) -> bool {
        if kind_of(e) != Kind::Image || e.s > THUMB_MAX || !matches!(ext_of(&e.n).to_ascii_lowercase().as_str(), "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp") {
            return false;
        }
        let url = format!("{}{}", self.origin, self.file_url(&e.n));
        let img = egui::Image::new(url);
        match img.load_for_size(ui.ctx(), rect.size()) {
            Ok(egui::load::TexturePoll::Ready { texture }) => {
                let s = texture.size;
                let k = (rect.width() / s.x).min(rect.height() / s.y);
                let r = egui::Rect::from_center_size(rect.center(), vec2(s.x * k, s.y * k));
                ui.painter().rect_filled(r.expand(1.0), CornerRadius::same(6), self.pal.border);
                ui.painter().image(texture.id, r, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), Color32::WHITE);
                true
            }
            _ => false,
        }
    }

    fn visible(&self) -> Vec<Entry> {
        let f = self.filter.to_lowercase();
        let mut v: Vec<Entry> = self.cur.entries.iter().filter(|e| f.is_empty() || e.n.to_lowercase().contains(&f)).cloned().collect();
        v.sort_by(|a, b| {
            b.d.cmp(&a.d).then_with(|| {
                let o = match self.sort {
                    SortBy::Name => a.n.to_lowercase().cmp(&b.n.to_lowercase()),
                    SortBy::Size => a.s.cmp(&b.s),
                    SortBy::Date => a.m.cmp(&b.m),
                };
                if self.desc { o.reverse() } else { o }
            })
        });
        v
    }

    fn poll(&mut self) {
        let done = self.pending.lock().ok().and_then(|mut g| g.take());
        if let Some(r) = done {
            self.loading = false;
            match r {
                Ok(l) => {
                    if l.path.is_empty() {
                        self.roots = l.entries.iter().map(|e| e.n.clone()).collect();
                    }
                    self.cur = l;
                    self.err = None;
                }
                Err(e) => self.err = Some(e),
            }
        }
    }

    fn toolbar(&mut self, ui: &mut egui::Ui) {
        let pal = self.pal;
        let ctx = ui.ctx().clone();
        ui.horizontal_centered(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            if toolbar_button(ui, &pal, Glyph::Back, self.hpos > 0, false, "Atrás").clicked() {
                self.go_hist(self.hpos - 1, &ctx);
            }
            if toolbar_button(ui, &pal, Glyph::Forward, self.hpos + 1 < self.hist.len(), false, "Adelante").clicked() {
                self.go_hist(self.hpos + 1, &ctx);
            }
            if toolbar_button(ui, &pal, Glyph::Up, !self.cur.path.is_empty(), false, "Subir (Retroceso)").clicked() {
                let mut p = self.cur.path.clone();
                p.pop();
                self.go(p, &ctx);
            }
            if toolbar_button(ui, &pal, Glyph::Refresh, true, false, "Actualizar (F5)").clicked() {
                let p = self.cur.path.clone();
                self.fetch(p, &ctx);
            }
            // ruta
            let mut nav: Option<Vec<String>> = None;
            egui::Frame::new().fill(pal.surface).corner_radius(10).inner_margin(Margin::symmetric(12, 6)).show(ui, |ui| {
                ui.set_min_width((ui.available_width() - 440.0).max(160.0));
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    let seg = |ui: &mut egui::Ui, t: &str, last: bool| {
                        let c = if last { pal.text } else { pal.subtle };
                        ui.add(egui::Label::new(egui::RichText::new(t).color(c).size(15.0)).sense(Sense::click())).on_hover_cursor(egui::CursorIcon::PointingHand)
                    };
                    if seg(ui, "/", self.cur.path.is_empty()).clicked() {
                        nav = Some(Vec::new());
                    }
                    for i in 0..self.cur.path.len() {
                        ui.label(egui::RichText::new("›").color(pal.subtle));
                        if seg(ui, &self.cur.path[i], i + 1 == self.cur.path.len()).clicked() {
                            nav = Some(self.cur.path[..=i].to_vec());
                        }
                    }
                });
            });
            if let Some(p) = nav {
                self.go(p, &ctx);
            }
            ui.add(egui::TextEdit::singleline(&mut self.filter).hint_text("Filtrar…  (Ctrl+F)").desired_width(190.0).margin(Margin::symmetric(10, 8)));
            if self.loading {
                ui.spinner();
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if toolbar_button(ui, &pal, Glyph::List, true, !self.grid, "Lista").clicked() {
                    self.grid = false;
                }
                if toolbar_button(ui, &pal, Glyph::Grid, true, self.grid, "Iconos").clicked() {
                    self.grid = true;
                }
            });
        });
    }

    fn sidebar(&mut self, ui: &mut egui::Ui) {
        let pal = self.pal;
        let ctx = ui.ctx().clone();
        ui.add_space(8.0);
        ui.label(egui::RichText::new("CARPETAS COMPARTIDAS").size(12.0).color(pal.subtle));
        ui.add_space(4.0);
        let mut nav: Option<Vec<String>> = None;
        let home = self.cur.path.is_empty();
        if self.side_row(ui, Glyph::Home, "Inicio", home).clicked() {
            nav = Some(Vec::new());
        }
        for r in self.roots.clone() {
            let on = self.cur.path.first() == Some(&r);
            if self.side_row(ui, Glyph::Folder, &r, on).clicked() {
                nav = Some(vec![r]);
            }
        }
        if let Some(p) = nav {
            self.go(p, &ctx);
        }
    }

    fn side_row(&self, ui: &mut egui::Ui, g: Glyph, label: &str, active: bool) -> egui::Response {
        let pal = self.pal;
        let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 36.0), Sense::click());
        if active || resp.hovered() {
            ui.painter().rect_filled(rect, CornerRadius::same(10), if active { pal.sel } else { pal.hover });
        }
        theme::draw_glyph(ui.painter(), egui::pos2(rect.left() + 20.0, rect.center().y), 18.0, g, if active { pal.accent } else { pal.text });
        ui.painter().text(egui::pos2(rect.left() + 42.0, rect.center().y), egui::Align2::LEFT_CENTER, label, egui::FontId::proportional(15.0), pal.text);
        resp.on_hover_cursor(egui::CursorIcon::PointingHand)
    }

    fn content(&mut self, ui: &mut egui::Ui) {
        let pal = self.pal;
        let ctx = ui.ctx().clone();
        let items = self.visible();
        if let Some(e) = &self.err {
            ui.colored_label(Color32::LIGHT_RED, e.as_str());
        }
        let mut open: Option<Entry> = None;
        if self.grid {
            let cell = vec2(self.zoom + 16.0, self.zoom + 48.0);
            let cols = ((ui.available_width() / cell.x).floor() as usize).max(1);
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
                for row in items.chunks(cols) {
                    ui.horizontal(|ui| {
                        for e in row {
                            let (rect, resp) = ui.allocate_exact_size(cell, Sense::click());
                            let on = self.sel.as_deref() == Some(e.n.as_str());
                            if on || resp.hovered() {
                                ui.painter().rect_filled(rect.shrink(2.0), CornerRadius::same(12), if on { pal.sel } else { pal.hover });
                            }
                            let ir = egui::Rect::from_center_size(egui::pos2(rect.center().x, rect.top() + 8.0 + self.zoom * 0.5), vec2(self.zoom * 0.9, self.zoom * 0.9));
                            if e.d {
                                theme::draw_folder(ui.painter(), ir);
                            } else if !self.thumb(ui, e, ir) {
                                theme::draw_file(ui.painter(), ir, kind_of(e), ext_of(&e.n), &pal);
                            }
                            let mut job = egui::text::LayoutJob::simple(e.n.clone(), egui::FontId::proportional(14.0), pal.text, cell.x - 12.0);
                            job.wrap.max_rows = 2;
                            job.wrap.break_anywhere = true;
                            job.halign = egui::Align::Center;
                            let g = ui.painter().layout_job(job);
                            let pos = egui::pos2(rect.center().x - g.size().x / 2.0 + g.size().x / 2.0 - g.rect.width() / 2.0 - g.rect.min.x * 0.0, rect.top() + self.zoom + 14.0);
                            ui.painter().galley(egui::pos2(rect.center().x, pos.y), g, pal.text);
                            if resp.clicked() {
                                self.sel = Some(e.n.clone());
                            }
                            if resp.double_clicked() {
                                open = Some(e.clone());
                            }
                            resp.on_hover_text(format!("{}{}", e.n, if e.d { String::new() } else { format!("  ·  {}", human(e.s)) }));
                        }
                    });
                }
            });
        } else {
            self.list_view(ui, &items, &mut open);
        }
        if let Some(e) = open {
            self.open(&e, &ctx);
        }
        if items.is_empty() && !self.loading && self.err.is_none() {
            ui.centered_and_justified(|ui| ui.label(egui::RichText::new("Carpeta vacía").color(pal.subtle)));
        }
    }

    fn list_view(&mut self, ui: &mut egui::Ui, items: &[Entry], open: &mut Option<Entry>) {
        let pal = self.pal;
        let w = ui.available_width();
        let (wsize, wdate) = (110.0, 150.0);
        ui.horizontal(|ui| {
            let mut head = |ui: &mut egui::Ui, label: &str, by: SortBy, width: f32| {
                let r = ui.add_sized(vec2(width, 26.0), egui::Label::new(egui::RichText::new(label).color(pal.subtle).size(13.0)).sense(Sense::click()));
                if self.sort == by {
                    let c = egui::pos2(r.rect.center().x + label.chars().count() as f32 * 3.6 + 12.0, r.rect.center().y);
                    let (a, b) = if self.desc { (3.5, -3.5) } else { (-3.5, 3.5) };
                    ui.painter().add(egui::Shape::convex_polygon(vec![c + vec2(-4.5, a), c + vec2(4.5, a), c + vec2(0.0, b)], pal.subtle, egui::Stroke::NONE));
                }
                if r.clicked() {
                    if self.sort == by {
                        self.desc = !self.desc;
                    } else {
                        self.sort = by;
                        self.desc = false;
                    }
                }
            };
            head(ui, "Nombre", SortBy::Name, (w - wsize - wdate - 24.0).max(80.0));
            head(ui, "Tamaño", SortBy::Size, wsize);
            head(ui, "Modificado", SortBy::Date, wdate);
        });
        egui::ScrollArea::vertical().auto_shrink([false, false]).show_rows(ui, 32.0, items.len(), |ui, range| {
            for e in &items[range] {
                let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 32.0), Sense::click());
                let on = self.sel.as_deref() == Some(e.n.as_str());
                if on || resp.hovered() {
                    ui.painter().rect_filled(rect, CornerRadius::same(8), if on { pal.sel } else { pal.hover });
                }
                let ir = egui::Rect::from_center_size(egui::pos2(rect.left() + 20.0, rect.center().y), vec2(24.0, 24.0));
                if e.d {
                    theme::draw_folder(ui.painter(), ir);
                } else {
                    theme::draw_file(ui.painter(), ir, kind_of(e), "", &pal);
                }
                let f = egui::FontId::proportional(14.5);
                let name_w = (rect.width() - wsize - wdate - 24.0 - 44.0).max(60.0);
                let mut job = egui::text::LayoutJob::simple_singleline(e.n.clone(), f.clone(), pal.text);
                job.wrap.max_width = name_w;
                job.wrap.max_rows = 1;
                let g = ui.painter().layout_job(job);
                ui.painter().galley(egui::pos2(rect.left() + 44.0, rect.center().y - g.size().y / 2.0), g, pal.text);
                if !e.d {
                    ui.painter().text(egui::pos2(rect.right() - wdate - 12.0, rect.center().y), egui::Align2::RIGHT_CENTER, human(e.s), f.clone(), pal.subtle);
                }
                ui.painter().text(egui::pos2(rect.right() - wdate + 4.0, rect.center().y), egui::Align2::LEFT_CENTER, short_date(e.m), f, pal.subtle);
                if resp.clicked() {
                    self.sel = Some(e.n.clone());
                }
                if resp.double_clicked() {
                    *open = Some(e.clone());
                }
            }
        });
    }

    fn keys(&mut self, ctx: &egui::Context) {
        let (up, enter, f5) = ctx.input(|i| (i.key_pressed(egui::Key::Backspace), i.key_pressed(egui::Key::Enter), i.key_pressed(egui::Key::F5)));
        if ctx.egui_wants_keyboard_input() {
            return;
        }
        if up && !self.cur.path.is_empty() {
            let mut p = self.cur.path.clone();
            p.pop();
            self.go(p, ctx);
        }
        if f5 {
            let p = self.cur.path.clone();
            self.fetch(p, ctx);
        }
        if enter {
            if let Some(e) = self.sel.as_ref().and_then(|n| self.cur.entries.iter().find(|e| &e.n == n)).cloned() {
                self.open(&e, ctx);
            }
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.poll();
        let ctx = ui.ctx().clone();
        let dark = ctx.theme() == egui::Theme::Dark;
        if self.dark != Some(dark) {
            self.dark = Some(dark);
            self.pal = theme::palette(dark);
            theme::apply_style(&ctx, &self.pal, dark);
        }
        let pal = self.pal;
        self.keys(&ctx);
        let bar = |l: i8, t: i8| egui::Frame::new().fill(pal.bg).inner_margin(Margin { left: l, right: l, top: t, bottom: t });
        egui::Panel::top("toolbar").frame(bar(10, 8)).show_separator_line(false).exact_size(52.0).show(ui, |ui| self.toolbar(ui));
        let n = self.visible().len();
        let total = self.cur.entries.len();
        egui::Panel::bottom("status").frame(bar(14, 4)).exact_size(30.0).show_separator_line(false).show(ui, |ui| {
            ui.horizontal_centered(|ui| {
                let t = if n == total { format!("{total} elementos") } else { format!("{n} de {total} elementos") };
                ui.label(egui::RichText::new(t).color(pal.subtle).size(13.0));
                if let Some(e) = self.sel.as_ref().and_then(|s| self.cur.entries.iter().find(|e| &e.n == s)) {
                    let d = if e.d { String::new() } else { format!("  ·  {}", human(e.s)) };
                    ui.label(egui::RichText::new(format!("·  {}{d}", e.n)).color(pal.subtle).size(13.0));
                }
                if self.cur.truncated {
                    ui.label(egui::RichText::new("·  listado recortado").color(pal.subtle).size(13.0));
                }
            });
        });
        if ui.available_width() > 560.0 {
            egui::Panel::left("side").frame(egui::Frame::new().fill(pal.bg).inner_margin(Margin::symmetric(10, 0))).exact_size(230.0).show_separator_line(false).show(ui, |ui| self.sidebar(ui));
        }
        egui::CentralPanel::default().frame(egui::Frame::new().fill(pal.bg).inner_margin(Margin { left: 6, right: 14, top: 0, bottom: 6 })).show_inside(ui, |ui| {
            egui::Frame::new().fill(pal.panel).corner_radius(14).inner_margin(Margin::same(10)).stroke(egui::Stroke::new(1.0, pal.border)).show(ui, |ui| {
                ui.set_min_size(ui.available_size());
                self.content(ui);
            });
        });
    }
}

#[cfg(target_arch = "wasm32")]
mod boot {
    use super::App;
    use wasm_bindgen::prelude::*;
    use wasm_bindgen::JsCast;

    #[wasm_bindgen(start)]
    pub fn start() {
        wasm_bindgen_futures::spawn_local(async {
            let win = web_sys::window().expect("window");
            let doc = win.document().expect("document");
            let canvas = doc.get_element_by_id("fb_canvas").and_then(|e| e.dyn_into::<web_sys::HtmlCanvasElement>().ok()).expect("canvas");
            // la página se sirve en /s/<token>/app/…; la API cuelga de /s/<token>/api/
            let path = win.location().pathname().unwrap_or_default();
            let mut it = path.split('/').filter(|s| !s.is_empty());
            let root = match (it.next(), it.next()) {
                (Some("s"), Some(tok)) => format!("/s/{tok}/"),
                _ => "/".to_string(),
            };
            let origin = win.location().origin().unwrap_or_default();
            let _ = eframe::WebRunner::new()
                .start(canvas, eframe::WebOptions::default(), Box::new(move |cc| Ok(Box::new(App::new(cc, root.clone(), origin.clone())))))
                .await;
        });
    }
}
