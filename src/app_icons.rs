//! Favoritos de la barra lateral (una sola lista: los de serie y las carpetas del usuario) e iconos
//! de carpeta (elegidos en la aplicación o leídos del sistema). Parte de `App`.
use super::*;
use crate::favs;
use crate::foldericons::{self, IconState, PlatIcon, Spec};

/// Estado propio de `App` para favoritos e iconos de carpeta.
pub(super) struct JState {
    pub(super) icons: IconState,
    /// favoritos de serie que existen en este equipo, en su orden natural
    builtin: Vec<Place>,
    /// nombres de iconos del tema (para «Icono del tema…»), leídos en un hilo la primera vez
    theme_names: Option<Arc<Vec<String>>>,
    theme_rx: Option<Receiver<Vec<String>>>,
}

/// Diálogos de favoritos e iconos (variante `Dlg::Fav`).
pub(super) enum JDlg {
    Icon(IconDlg),
    Label { path: PathBuf, text: String },
}

pub(super) struct IconDlg {
    path: PathBuf,
    sel: Option<Spec>,
    color: u8,
    show_img: bool,
    img: String,
    cands: Option<Vec<PathBuf>>,
    show_theme: bool,
    query: String,
}

fn copy_place(p: &Place) -> Place {
    Place { name: p.name.clone(), path: p.path.clone(), glyph: p.glyph, sys: p.sys }
}

/// Imágenes que hay directamente dentro de la carpeta (para elegirlas como icono), acotado.
fn image_candidates(dir: &Path) -> Vec<PathBuf> {
    let mut v = Vec::new();
    // una carpeta de red montada podría tardar mucho en listarse
    if dir.to_str().is_some_and(|s| s.contains("/gvfs/")) {
        return v;
    }
    if let Ok(rd) = crate::vfs::fs::read_dir(dir) {
        for e in rd.flatten().take(2000) {
            let p = e.path();
            let ok = p.extension().map(|x| x.to_string_lossy().to_ascii_lowercase()).is_some_and(|x| matches!(x.as_str(), "png" | "svg" | "ico"));
            if ok && e.file_type().is_ok_and(|t| t.is_file()) {
                v.push(p);
                if v.len() >= 12 {
                    break;
                }
            }
        }
    }
    v.sort();
    v
}

const UV: Rect = Rect { min: Pos2 { x: 0.0, y: 0.0 }, max: Pos2 { x: 1.0, y: 1.0 } };

impl JState {
    pub(super) fn init(cfg: &Config) -> (Vec<Place>, JState) {
        let mut icons = IconState::new();
        icons.set_app(&cfg.folder_icons);
        let j = JState { icons, builtin: standard_places(), theme_names: None, theme_rx: None };
        let places = j.build(cfg);
        (places, j)
    }

    fn builtin_paths(&self) -> Vec<PathBuf> {
        self.builtin.iter().map(|p| p.path.clone()).collect()
    }

    pub(super) fn is_builtin(&self, p: &Path) -> bool {
        self.builtin.iter().any(|b| b.path == p)
    }

    /// Lista visible a partir de la configuración (también migra los marcadores antiguos).
    fn build(&self, cfg: &Config) -> Vec<Place> {
        let b = self.builtin_paths();
        let list = favs::merged(&b, cfg.favs.as_deref(), &cfg.fav_hidden, &cfg.fav_order, &cfg.bookmarks);
        list.into_iter()
            .map(|p| {
                let mut pl = match self.builtin.iter().find(|x| x.path == p) {
                    Some(x) => copy_place(x),
                    None => {
                        let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| p.display().to_string());
                        let sys = self.icons.known(&p).unwrap_or("");
                        Place { name, path: p, glyph: Glyph::Folder, sys }
                    }
                };
                if let Some(l) = favs::label_of(&cfg.fav_labels, &pl.path) {
                    pl.name = l.to_string();
                }
                pl
            })
            .collect()
    }
}

impl App {
    // ---------------------------------------------------------------- favoritos

    pub(super) fn is_fav(&self, p: &Path) -> bool {
        // una carpeta de un servidor SFTP se guarda por su dirección (la carpeta espejo cambia)
        let q = crate::sftp::persist_path(p);
        self.places.iter().any(|pl| pl.path == p || pl.path == q)
    }

    fn fav_paths(&self) -> Vec<PathBuf> {
        self.places.iter().map(|pl| pl.path.clone()).collect()
    }

    /// Guarda la lista (formato nuevo y antiguo) y rehace la barra lateral.
    fn fav_commit(&mut self, list: Vec<PathBuf>) {
        let (f, o, b) = favs::store(&list, &self.j.builtin_paths());
        self.cfg.favs = Some(f);
        self.cfg.fav_order = o;
        self.cfg.bookmarks = b;
        self.places = self.j.build(&self.cfg);
        self.j.icons.recheck_missing();
        self.save_cfg();
    }

    /// «Añadir a favoritos» / «Quitar de favoritos» (Ctrl+D, estrella de la barra de direcciones…).
    pub(super) fn toggle_fav(&mut self, p: PathBuf) {
        if self.is_fav(&p) {
            self.remove_fav(&crate::sftp::persist_path(&p));
        } else {
            self.add_favs(vec![p]);
        }
    }

    pub(super) fn remove_fav(&mut self, p: &Path) {
        let mut list = self.fav_paths();
        let b = self.j.builtin_paths();
        favs::remove(&mut list, &mut self.cfg.fav_hidden, &b, p);
        self.fav_commit(list);
    }

    /// Añade carpetas al final de los favoritos; devuelve cuántas eran nuevas.
    pub(super) fn add_favs(&mut self, paths: Vec<PathBuf>) -> usize {
        let mut list = self.fav_paths();
        let mut n = 0;
        for p in paths {
            let p = crate::sftp::persist_path(&p);
            if virt_mode(&p).is_none() && favs::add(&mut list, &mut self.cfg.fav_hidden, &p) {
                n += 1;
            }
        }
        if n > 0 {
            self.fav_commit(list);
        }
        n
    }

    pub(super) fn move_fav(&mut self, from: usize, to: usize) {
        let mut list = self.fav_paths();
        if favs::move_item(&mut list, from, to) {
            self.fav_commit(list);
        }
    }

    pub(super) fn restore_favs(&mut self) {
        self.cfg.fav_hidden.clear();
        let list = favs::restore(&self.fav_paths(), &self.j.builtin_paths());
        self.fav_commit(list);
    }

    /// ¿Hay favoritos de serie quitados que «Restaurar favoritos» devolvería?
    pub(super) fn can_restore_favs(&self) -> bool {
        self.j.builtin.iter().any(|b| !self.is_fav(&b.path))
    }

    pub(super) fn open_fav_label(&mut self, p: PathBuf) {
        let text = self.places.iter().find(|pl| pl.path == p).map(|pl| pl.name.clone()).unwrap_or_default();
        self.dlg = Dlg::Fav(JDlg::Label { path: p, text });
    }

    pub(super) fn open_folder_icon(&mut self, p: PathBuf) {
        let sel = self.j.icons.app_spec(&p).cloned();
        let color = match &sel {
            Some(Spec::Drawn { color, .. }) => *color,
            _ => 0,
        };
        let img = match &sel {
            Some(Spec::Image(i)) => i.display().to_string(),
            _ => String::new(),
        };
        let (show_img, show_theme) = (matches!(sel, Some(Spec::Image(_))), matches!(sel, Some(Spec::Theme(_))));
        self.dlg = Dlg::Fav(JDlg::Icon(IconDlg { path: p, sel, color, show_img, img, cands: None, show_theme, query: String::new() }));
    }

    fn set_folder_icon(&mut self, p: &Path, s: Option<Spec>) {
        self.cfg.folder_icons.retain(|(q, _)| q != p);
        if let Some(s) = s {
            self.cfg.folder_icons.push((p.to_path_buf(), s.encode()));
        }
        self.j.icons.set_app(&self.cfg.folder_icons);
        self.save_cfg();
    }

    /// Sección «Favoritos» de la barra lateral: filas, arrastre para reordenar y zona para soltar carpetas.
    pub(super) fn sidebar_favs(&mut self, ui: &mut egui::Ui, ptr: Option<Pos2>, dragging: bool, cwd: &Path, go: &mut Option<PathBuf>, act: &mut Option<SideAct>) {
        let pal = self.pal;
        let can_restore = self.can_restore_favs();
        let n = self.places.len();
        let users: Vec<PathBuf> = self.places.iter().filter(|p| !self.j.is_builtin(&p.path)).map(|p| p.path.clone()).collect();
        // en pantallas táctiles arrastrar es desplazar la barra: se reordena con «Subir»/«Bajar»
        let reorder = !self.touch && !dragging;
        let mut rows: Vec<Rect> = Vec::with_capacity(n);
        for i in 0..n {
            let path = self.places[i].path.clone();
            let (glyph, name) = (self.places[i].glyph, self.places[i].name.clone());
            // una dirección de red (sftp://…) no es una carpeta local: al pulsarla se conecta
            let missing = !self.j.is_builtin(&path) && !super::app_places::is_net_uri(&path.to_string_lossy()) && self.j.icons.missing(&self.ctx, &users, &path);
            let tex = if missing { None } else { self.fav_tex(i) };
            let r = side_item(ui, &pal, glyph, tex, &name, path == cwd, false);
            let r = if reorder { r.interact(Sense::drag()) } else { r };
            rows.push(r.rect);
            if missing {
                // carpeta que ya no existe (disco quitado, carpeta borrada): atenuada, se puede quitar
                ui.painter().rect_filled(r.rect, CornerRadius::same(10), pal.panel.gamma_multiply(0.6));
            }
            if dragging && !missing && ptr.is_some_and(|q| r.rect.contains(q)) {
                self.side_drop = Some(path.clone());
            }
            if r.drag_started() {
                self.j.icons.fav_drag = Some(i);
            }
            if r.clicked() {
                if missing {
                    self.say(format!("{}: {}", t("No disponible"), path.display()), true);
                } else {
                    *go = Some(path.clone());
                }
            }
            let tip = if missing { format!("{}\n{}", t("No disponible"), path.display()) } else { path.display().to_string() };
            r.on_hover_text(tip).context_menu(|ui| side_menu(ui, &path, SideKind::Fav, i, n, true, can_restore, act));
        }
        if let Some(from) = self.j.icons.fav_drag {
            let (down, pos) = ui.input(|i| (i.pointer.primary_down(), i.pointer.interact_pos()));
            let to = pos.map(|q| rows.iter().position(|r| q.y < r.center().y).unwrap_or(rows.len()));
            if let (Some(to), Some(first), Some(last)) = (to, rows.first(), rows.last()) {
                if to != from && to != from + 1 {
                    let y = if to < rows.len() { rows[to].top() } else { last.bottom() };
                    ui.painter().hline(first.x_range(), y, Stroke::new(2.0, pal.accent));
                }
            }
            if !down {
                self.j.icons.fav_drag = None;
                if let Some(to) = to {
                    let to = if to > from { to - 1 } else { to };
                    if to != from {
                        self.move_fav(from, to);
                    }
                }
            }
        }
        if dragging {
            let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 34.0), Sense::hover());
            let hot = ptr.map(|q| r.contains(q)).unwrap_or(false);
            self.bm_drop = hot;
            let p = ui.painter();
            p.rect_filled(r, CornerRadius::same(10), if hot { pal.sel } else { pal.hover.gamma_multiply(0.5) });
            p.rect_stroke(r, CornerRadius::same(10), Stroke::new(1.2, if hot { pal.accent } else { pal.border }), egui::StrokeKind::Inside);
            p.text(r.center(), Align2::CENTER_CENTER, t("Suelta aquí para añadir a favoritos"), FontId::proportional(12.0), if hot { pal.accent } else { pal.subtle });
        }
        if self.places.is_empty() && can_restore {
            let r = side_item(ui, &pal, Glyph::Plus, None, t("Restaurar favoritos"), false, false);
            if r.clicked() {
                *act = Some(SideAct::RestoreFavs);
            }
        }
    }

    // ---------------------------------------------------------------- iconos

    fn spec_tex(&mut self, s: &Spec) -> Option<egui::TextureId> {
        match s {
            Spec::Drawn { color, emblem } => self.j.icons.drawn_tex(&self.ctx, *color, *emblem),
            Spec::Image(p) => self.j.icons.image_tex(&self.ctx, p),
            Spec::Theme(n) => self.sys.named(&self.ctx, &[n.as_str()]),
        }
    }

    /// Icono propio de la carpeta: el elegido en la aplicación y, si no, el del sistema
    /// (GVFS/KDE en Linux, desktop.ini en Windows, «Icon\r» en macOS). `None` si no tiene o aún se lee.
    pub(super) fn custom_folder_tex(&mut self, path: &Path) -> Option<egui::TextureId> {
        self.j.icons.tick(self.generation, &self.cwd, self.cfg.folder_icons_os);
        if let Some(s) = self.j.icons.app_spec(path).cloned() {
            return self.spec_tex(&s);
        }
        if !self.cfg.folder_icons_os || virt_mode(path).is_some() {
            return None;
        }
        match self.j.icons.plat(&self.ctx, path)? {
            PlatIcon::Theme(n) => self.sys.named(&self.ctx, &[n.as_str()]),
            PlatIcon::Image(p) => self.j.icons.image_tex(&self.ctx, &p),
            PlatIcon::PerPath => self.sys.for_path(&self.ctx, path),
            PlatIcon::None => None,
        }
    }

    /// Icono de una carpeta del listado: el propio, el de carpeta conocida (Documentos, Proyectos…) o `None`
    /// (se dibuja la carpeta de siempre).
    pub(super) fn folder_tex_at(&mut self, path: &Path) -> Option<egui::TextureId> {
        if let Some(t) = self.custom_folder_tex(path) {
            return Some(t);
        }
        let k = self.j.icons.known(path)?;
        if self.cfg.sys_icons {
            if cfg!(any(windows, target_os = "macos")) {
                if let Some(t) = self.sys.for_path(&self.ctx, path) {
                    return Some(t);
                }
            }
            return self.sys.folder(&self.ctx, Some(k));
        }
        let (c, e) = foldericons::special_drawn(k)?;
        self.j.icons.drawn_tex(&self.ctx, c, e)
    }

    /// Icono de un favorito de la barra lateral (`None`: su glifo).
    fn fav_tex(&mut self, i: usize) -> Option<egui::TextureId> {
        let (path, sys) = (self.places[i].path.clone(), self.places[i].sys);
        if let Some(t) = self.custom_folder_tex(&path) {
            return Some(t);
        }
        if self.cfg.sys_icons {
            return self.sys.folder(&self.ctx, Some(sys));
        }
        if self.j.is_builtin(&path) {
            return None;
        }
        let (c, e) = foldericons::special_drawn(sys)?;
        self.j.icons.drawn_tex(&self.ctx, c, e)
    }

    /// Dibuja el icono propio o especial de una carpeta del listado; `false` si no tiene.
    pub(super) fn paint_folder_custom(&mut self, p: &egui::Painter, r: Rect, e: &Entry) -> bool {
        if !e.is_dir() || self.vmode == VMode::Trash {
            return false;
        }
        let path = self.cwd.join(&*e.name);
        match self.folder_tex_at(&path) {
            Some(id) => {
                p.image(id, r, UV, Color32::WHITE);
                true
            }
            None => false,
        }
    }

    /// Icono de la carpeta de la pestaña `i`.
    pub(super) fn tab_folder_tex(&mut self, i: usize) -> Option<egui::TextureId> {
        let (mode, cwd) = if i == self.cur_tab { (&self.vmode, &self.cwd) } else { (&self.tabs.get(i)?.vmode, &self.tabs.get(i)?.cwd) };
        if !matches!(mode, VMode::Dir | VMode::Deep) {
            return None;
        }
        let cwd = cwd.clone();
        self.folder_tex_at(&cwd)
    }

    fn theme_names(&mut self) -> Option<Arc<Vec<String>>> {
        if let Some(n) = &self.j.theme_names {
            return Some(n.clone());
        }
        if let Some(rx) = &self.j.theme_rx {
            return match rx.try_recv() {
                Ok(v) => {
                    let v = Arc::new(v);
                    self.j.theme_names = Some(v.clone());
                    self.j.theme_rx = None;
                    Some(v)
                }
                Err(_) => {
                    self.ctx.request_repaint_after(Duration::from_millis(50));
                    None
                }
            };
        }
        let Some(dirs) = self.sys.theme_dirs(&self.ctx) else {
            self.ctx.request_repaint_after(Duration::from_millis(50));
            return None;
        };
        let (tx, rx) = channel();
        let c = self.ctx.clone();
        crate::vfs::thread::spawn(move || {
            let _ = tx.send(foldericons::list_theme_names(&dirs));
            c.request_repaint();
        });
        self.j.theme_rx = Some(rx);
        None
    }

    // ---------------------------------------------------------------- diálogos

    /// Dibuja el diálogo; `false` cuando se cierra.
    pub(super) fn j_dialog(&mut self, ctx: &egui::Context, d: &mut JDlg) -> bool {
        match d {
            JDlg::Icon(d) => self.icon_dialog(ctx, d),
            JDlg::Label { path, text } => self.label_dialog(ctx, path, text),
        }
    }

    fn label_dialog(&mut self, ctx: &egui::Context, path: &Path, text: &mut String) -> bool {
        let pal = self.pal;
        let mut open = true;
        let mut enter = false;
        let res = theme::card_window_with_footer(
            ctx,
            &pal,
            t("Nombre del favorito"),
            theme::dialog_width(ctx, 420.0),
            &mut open,
            |ui| {
                ui.label(egui::RichText::new(path.display().to_string()).color(pal.subtle));
                ui.add_space(6.0);
                let r = ui.add(egui::TextEdit::singleline(text).desired_width(f32::INFINITY));
                if !r.has_focus() && !r.lost_focus() {
                    r.request_focus();
                }
                enter = r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
            },
            |ui| theme::DialogButtons::new(t("Cancelar"), t("Aceptar")).show(ui, &pal),
        );
        let act = res.unwrap_or(theme::DialogAction::None);
        if enter || act == theme::DialogAction::Primary {
            // vacío o igual al nombre de la carpeta: sin nombre propio
            let base = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let builtin = self.j.builtin.iter().find(|b| b.path == path).map(|b| b.name.clone());
            let v = if text.trim() == base || Some(text.trim().to_string()) == builtin { "" } else { text.as_str() };
            favs::set_label(&mut self.cfg.fav_labels, path, v);
            self.places = self.j.build(&self.cfg);
            self.save_cfg();
            return false;
        }
        open && act != theme::DialogAction::Cancel
    }

    fn icon_dialog(&mut self, ctx: &egui::Context, d: &mut IconDlg) -> bool {
        let pal = self.pal;
        let mut open = true;
        let can_apply = d.sel.is_some();
        let extras = [t("Restablecer")];
        let res = theme::card_window_with_footer(
            ctx,
            &pal,
            t("Icono de carpeta"),
            theme::dialog_width(ctx, 520.0),
            &mut open,
            |ui| {
                ui.horizontal(|ui| {
                    let tex = match d.sel.clone() {
                        Some(s) => self.spec_tex(&s),
                        None => self.folder_tex_at(&d.path),
                    };
                    let (r, _) = ui.allocate_exact_size(vec2(64.0, 64.0), Sense::hover());
                    match tex {
                        Some(id) => {
                            ui.painter().image(id, r, UV, Color32::WHITE);
                        }
                        None => theme::draw_folder(ui.painter(), r),
                    }
                    ui.add_space(8.0);
                    ui.add(egui::Label::new(egui::RichText::new(d.path.display().to_string()).color(pal.subtle)).wrap());
                });
                theme::section(ui, &pal, t("Color"));
                ui.horizontal_wrapped(|ui| {
                    for (i, c) in foldericons::COLORS.iter().enumerate() {
                        let (r, resp) = ui.allocate_exact_size(vec2(30.0, 30.0), Sense::click());
                        ui.painter().circle_filled(r.center(), 11.0, Color32::from_rgb(c[0], c[1], c[2]));
                        if d.color == i as u8 {
                            ui.painter().circle_stroke(r.center(), 13.5, Stroke::new(2.0, pal.accent));
                        } else if resp.hovered() {
                            ui.painter().circle_stroke(r.center(), 13.5, Stroke::new(1.0, pal.outline));
                        }
                        if resp.clicked() {
                            d.color = i as u8;
                            let emblem = match d.sel {
                                Some(Spec::Drawn { emblem, .. }) => emblem,
                                _ => 0,
                            };
                            d.sel = Some(Spec::Drawn { color: d.color, emblem });
                        }
                    }
                });
                theme::section(ui, &pal, t("Emblema"));
                ui.horizontal_wrapped(|ui| {
                    for e in 0..foldericons::EMBLEMS.len() as u8 {
                        let tex = self.j.icons.drawn_tex(ctx, d.color, e);
                        let on = d.sel == Some(Spec::Drawn { color: d.color, emblem: e });
                        let (r, resp) = ui.allocate_exact_size(vec2(48.0, 48.0), Sense::click());
                        if on {
                            ui.painter().rect_filled(r, CornerRadius::same(8), pal.sel);
                            ui.painter().rect_stroke(r, CornerRadius::same(8), Stroke::new(1.5, pal.accent), egui::StrokeKind::Inside);
                        } else if resp.hovered() {
                            ui.painter().rect_filled(r, CornerRadius::same(8), pal.hover);
                        }
                        if let Some(id) = tex {
                            ui.painter().image(id, r.shrink(6.0), UV, Color32::WHITE);
                        }
                        if resp.on_hover_text(t(foldericons::EMBLEM_LABELS[e as usize])).clicked() {
                            d.sel = Some(Spec::Drawn { color: d.color, emblem: e });
                        }
                    }
                });
                ui.add_space(8.0);
                if ui.selectable_label(d.show_img, t("Elegir imagen…")).clicked() {
                    d.show_img = !d.show_img;
                }
                if d.show_img {
                    ui.horizontal(|ui| {
                        ui.label(t("Ruta de la imagen"));
                        let r = ui.add(egui::TextEdit::singleline(&mut d.img).hint_text("/…/icono.png").desired_width(f32::INFINITY));
                        if r.changed() {
                            let s = d.img.trim();
                            let p = PathBuf::from(s);
                            let p = if p.is_absolute() { p } else { d.path.join(p) };
                            d.sel = (!s.is_empty()).then_some(Spec::Image(p));
                        }
                    });
                    let cands = d.cands.get_or_insert_with(|| image_candidates(&d.path)).clone();
                    if !cands.is_empty() {
                        ui.label(egui::RichText::new(t("Imágenes de esta carpeta")).color(pal.subtle));
                        ui.horizontal_wrapped(|ui| {
                            for c in cands {
                                let tex = self.j.icons.image_tex(ctx, &c);
                                let on = d.sel == Some(Spec::Image(c.clone()));
                                let (r, resp) = ui.allocate_exact_size(vec2(48.0, 48.0), Sense::click());
                                if on {
                                    ui.painter().rect_filled(r, CornerRadius::same(8), pal.sel);
                                } else if resp.hovered() {
                                    ui.painter().rect_filled(r, CornerRadius::same(8), pal.hover);
                                }
                                if let Some(id) = tex {
                                    ui.painter().image(id, r.shrink(6.0), UV, Color32::WHITE);
                                }
                                let name = c.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                                if resp.on_hover_text(name).clicked() {
                                    d.img = c.display().to_string();
                                    d.sel = Some(Spec::Image(c));
                                }
                            }
                        });
                    }
                }
                if cfg!(target_os = "linux") {
                    if ui.selectable_label(d.show_theme, t("Icono del tema…")).clicked() {
                        d.show_theme = !d.show_theme;
                    }
                    if d.show_theme {
                        ui.add(egui::TextEdit::singleline(&mut d.query).hint_text(t("Buscar")).desired_width(f32::INFINITY));
                        match self.theme_names() {
                            None => {
                                ui.spinner();
                            }
                            Some(names) => {
                                let q = d.query.trim().to_lowercase();
                                let hits: Vec<&String> = names.iter().filter(|n| if q.is_empty() { n.starts_with("folder") } else { n.contains(&q) }).take(60).collect();
                                if hits.is_empty() {
                                    ui.label(egui::RichText::new(t("Sin resultados")).color(pal.subtle));
                                }
                                ui.horizontal_wrapped(|ui| {
                                    for n in hits {
                                        let tex = self.sys.named(ctx, &[n.as_str()]);
                                        let on = d.sel == Some(Spec::Theme(n.clone()));
                                        let (r, resp) = ui.allocate_exact_size(vec2(48.0, 48.0), Sense::click());
                                        if on {
                                            ui.painter().rect_filled(r, CornerRadius::same(8), pal.sel);
                                        } else if resp.hovered() {
                                            ui.painter().rect_filled(r, CornerRadius::same(8), pal.hover);
                                        }
                                        if let Some(id) = tex {
                                            ui.painter().image(id, r.shrink(6.0), UV, Color32::WHITE);
                                        }
                                        if resp.on_hover_text(n.as_str()).clicked() {
                                            d.sel = Some(Spec::Theme(n.clone()));
                                        }
                                    }
                                });
                            }
                        }
                    }
                }
            },
            |ui| theme::DialogButtons::new(t("Cancelar"), t("Aplicar")).extras(&extras).primary_enabled(can_apply).show(ui, &pal),
        );
        match res.unwrap_or(theme::DialogAction::None) {
            theme::DialogAction::Primary => {
                let (p, s) = (d.path.clone(), d.sel.clone());
                self.set_folder_icon(&p, s);
                false
            }
            theme::DialogAction::Extra(_) => {
                let p = d.path.clone();
                self.set_folder_icon(&p, None);
                false
            }
            theme::DialogAction::Cancel => false,
            theme::DialogAction::None => open,
        }
    }
}
