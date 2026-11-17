//! Arrastre de la ventana, barra de herramientas, barras del teléfono y barra de direcciones.
use super::*;

impl App {
    /// Pide al sistema mover (`dir == None`) o redimensionar la ventana. En Windows, si el sistema no
    /// responde (le pasa a algunas instalaciones), `window_drag_tick` lo hace a mano.
    pub(super) fn begin_window_drag(&mut self, ctx: &egui::Context, dir: Option<egui::ResizeDirection>) {
        use egui::ViewportCommand as V;
        ctx.send_viewport_cmd(match dir {
            Some(d) => V::BeginResize(d),
            None => V::StartDrag,
        });
        if cfg!(windows) {
            let (outer, inner) = ctx.input(|i| (i.viewport().outer_rect, i.viewport().inner_rect));
            if let (Some(c), Some(o), Some(inn)) = (screen_cursor(ctx.pixels_per_point()), outer, inner) {
                self.win_drag = Some(WinDrag { dir, cursor0: c, outer0: o, inner0: inn, manual: false });
            }
        }
    }

    pub(super) fn window_drag_tick(&mut self, ctx: &egui::Context) {
        use egui::{ResizeDirection as D, ViewportCommand as V};
        let Some(wd) = self.win_drag.as_mut() else { return };
        if !ctx.input(|i| i.pointer.primary_down()) {
            self.win_drag = None;
            return;
        }
        let Some(cur) = screen_cursor(ctx.pixels_per_point()) else {
            self.win_drag = None;
            return;
        };
        let d = cur - wd.cursor0;
        if !wd.manual {
            let moved = ctx
                .input(|i| i.viewport().outer_rect)
                .map(|o| (o.min - wd.outer0.min).length() > 0.5 || (o.size() - wd.outer0.size()).length() > 0.5)
                .unwrap_or(true);
            if moved {
                // el sistema está moviendo/redimensionando la ventana: nada que hacer
                self.win_drag = None;
            } else if d.length() > 8.0 {
                wd.manual = true;
            }
            return;
        }
        let mut r = wd.outer0;
        match wd.dir {
            None => r = r.translate(d),
            Some(dir) => {
                let (w, e, n, s) = (
                    matches!(dir, D::West | D::NorthWest | D::SouthWest),
                    matches!(dir, D::East | D::NorthEast | D::SouthEast),
                    matches!(dir, D::North | D::NorthWest | D::NorthEast),
                    matches!(dir, D::South | D::SouthWest | D::SouthEast),
                );
                let (min_w, min_h) = (640.0, 420.0);
                if w {
                    r.min.x = (r.min.x + d.x).min(r.max.x - min_w);
                }
                if e {
                    r.max.x = (r.max.x + d.x).max(r.min.x + min_w);
                }
                if n {
                    r.min.y = (r.min.y + d.y).min(r.max.y - min_h);
                }
                if s {
                    r.max.y = (r.max.y + d.y).max(r.min.y + min_h);
                }
                let frame = wd.outer0.size() - wd.inner0.size();
                ctx.send_viewport_cmd(V::InnerSize(r.size() - frame));
            }
        }
        ctx.send_viewport_cmd(V::OuterPosition(r.min));
        ctx.request_repaint();
    }

    pub(super) fn toolbar(&mut self, ui: &mut egui::Ui) {
        let pal = self.pal;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            if icon_button(ui, &pal, Glyph::Back, !self.back.is_empty(), false, &commands::with_shortcut(t("Atrás"), Cmd::Back)).clicked() {
                self.go_back();
            }
            if icon_button(ui, &pal, Glyph::Forward, !self.fwd.is_empty(), false, &commands::with_shortcut(t("Adelante"), Cmd::Forward)).clicked() {
                self.go_forward();
            }
            if icon_button(ui, &pal, Glyph::Up, self.cwd.parent().is_some(), false, &commands::with_shortcut(t("Carpeta superior"), Cmd::Up)).clicked() {
                self.go_up();
            }
            if icon_button(ui, &pal, Glyph::Refresh, true, false, &commands::with_shortcut(t("Actualizar"), Cmd::Refresh)).clicked() {
                self.refresh();
            }
            ui.add_space(4.0);
            ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                // rueda dentada: ordenar, ocultos y tema
                let gear = icon_button(ui, &pal, Glyph::Gear, true, false, t("Ajustes de vista"));
                self.gear_id = Some(egui::Popup::default_response_id(&gear));
                egui::Popup::menu(&gear).show(|ui| {
                    ui.set_width(190.0);
                    ui.spacing_mut().item_spacing.y = 2.0;
                    menu_head(ui, &pal, t("Ordenar"));
                    for (k, l) in [
                        (SortKey::Name, t("Nombre")),
                        (SortKey::Date, t("Fecha de modificación")),
                        (SortKey::Size, t("Tamaño")),
                        (SortKey::Type, t("Tipo")),
                    ] {
                        let cur = self.cfg.sort == k;
                        if menu_row(ui, &pal, l, cur, Some(self.cfg.asc)) {
                            self.sort_by(k);
                            ui.close();
                        }
                    }
                    ui.separator();
                    menu_head(ui, &pal, t("Tema"));
                    for (m, l) in [(0u8, t("Sistema")), (1, t("Claro")), (2, t("Oscuro"))] {
                        if menu_row(ui, &pal, l, self.cfg.theme_mode == m, None) {
                            self.set_theme_mode(m);
                            ui.close();
                        }
                    }
                    ui.separator();
                    menu_head(ui, &pal, t("Mostrar"));
                    if menu_row(ui, &pal, t("Ocultos"), self.cfg.show_hidden, None) {
                        self.cfg.show_hidden = !self.cfg.show_hidden;
                        self.save_cfg();
                        self.spawn_sort();
                    }
                    // sin botón de menú, aquí se puede volver a mostrar (también con F10 / Alt)
                    if !self.phone && menu_row(ui, &pal, t("Mostrar botón de menú"), self.cfg.show_menu_btn, None) {
                        self.cfg.show_menu_btn = !self.cfg.show_menu_btn;
                        self.save_cfg();
                    }
                });
                if toggle_button(ui, &pal, Glyph::List, !self.cfg.grid && !self.cfg.col_view, &commands::with_shortcut(t("Vista de lista"), Cmd::ListView)).clicked() {
                    self.set_view(0);
                }
                if toggle_button(ui, &pal, Glyph::Grid, self.cfg.grid, &commands::with_shortcut(t("Vista de cuadrícula"), Cmd::GridView)).clicked() {
                    self.set_view(1);
                }
                if !self.phone && toggle_button(ui, &pal, Glyph::Columns, self.cfg.col_view, t("Vista de columnas")).clicked() {
                    self.set_view(2);
                }
                ui.add_space(4.0);
                // filtro
                let hint = self.ux_filter_hint();
                let te = egui::TextEdit::singleline(&mut self.filter)
                    .id(a11y::filter_id())
                    .hint_text(hint)
                    .desired_width(170.0)
                    // a la derecha queda sitio para el botón de borrar
                    .margin(egui::Margin { left: 10, right: 30, top: 8, bottom: 8 });
                let r = ui.add(te);
                if self.filter_focus {
                    r.request_focus();
                    self.filter_focus = false;
                }
                if r.changed() {
                    self.spawn_sort();
                }
                if !self.filter.is_empty() && self.ux_clear_button(ui, &r) {
                    self.filter.clear();
                    self.spawn_sort();
                }
                ui.add_space(6.0);
                if icon_button(ui, &pal, Glyph::FolderPlus, true, false, &commands::with_shortcut(t("Nueva carpeta"), Cmd::NewFolder)).clicked() {
                    self.new_folder();
                }
                if self.touch {
                    // selección múltiple táctil: con el modo activo, tocar alterna elementos
                    if toggle_button(ui, &pal, Glyph::Check, self.sel_mode, t("Seleccionar")).clicked() {
                        self.sel_mode = !self.sel_mode;
                        if !self.sel_mode {
                            self.clear_sel();
                        }
                    }
                }
                ui.add_space(6.0);
                // barra de dirección: ocupa el resto
                let w = ui.available_width().max(80.0);
                self.address_bar(ui, w);
            });
        });
    }

    /// Barra superior del móvil, en dos líneas: arriba los iconos (favoritos y atrás a la izquierda,
    /// selección múltiple y menú a la derecha) y debajo sólo la ruta, a todo el ancho.
    pub(super) fn phone_bar(&mut self, ui: &mut egui::Ui) {
        let pal = self.pal;
        let screen = self.ctx.content_rect();
        ui.spacing_mut().item_spacing.y = 6.0;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            // favoritos y lugares (la antigua barra lateral)
            let (r, places) = ui.allocate_exact_size(vec2(48.0, 48.0), Sense::click());
            places.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, t("Favoritos")));
            theme::focus_ring(ui, &pal, &places, 12.0);
            if places.hovered() || egui::Popup::is_id_open(ui.ctx(), egui::Popup::default_response_id(&places)) {
                ui.painter().rect_filled(r, CornerRadius::same(12), pal.hover);
            }
            theme::draw_glyph(ui.painter(), r.center(), 22.0, Glyph::Bookmark, pal.text);
            egui::Popup::menu(&places).show(|ui| {
                let w = (screen.width() - 24.0).min(300.0);
                ui.set_width(w);
                ui.set_height((screen.height() - 90.0).min(520.0));
                let gen0 = self.generation;
                self.sidebar(ui);
                if self.generation != gen0 {
                    ui.close();
                }
            });
            // atrás
            let can_back = !self.back.is_empty();
            let (r, back) = ui.allocate_exact_size(vec2(48.0, 48.0), Sense::click());
            back.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, can_back, t("Atrás")));
            theme::focus_ring(ui, &pal, &back, 12.0);
            if can_back && back.hovered() {
                ui.painter().rect_filled(r, CornerRadius::same(12), pal.hover);
            }
            theme::draw_glyph(ui.painter(), r.center(), 22.0, Glyph::Back, if can_back { pal.text } else { pal.disabled });
            if can_back && back.clicked() {
                self.go_back();
            }
            ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                // menú
                let (r, more) = ui.allocate_exact_size(vec2(48.0, 48.0), Sense::click());
                more.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, t("Menú")));
                theme::focus_ring(ui, &pal, &more, 12.0);
                if more.hovered() || egui::Popup::is_id_open(ui.ctx(), egui::Popup::default_response_id(&more)) {
                    ui.painter().rect_filled(r, CornerRadius::same(12), pal.hover);
                }
                theme::draw_glyph(ui.painter(), r.center(), 22.0, Glyph::More, pal.text);
                let max_h = (screen.height() - 90.0).max(200.0);
                egui::Popup::menu(&more).show(|ui| {
                    egui::ScrollArea::vertical().max_height(max_h).show(ui, |ui| self.phone_menu(ui));
                });
                // filtrar por nombre: abre la barra de filtro bajo ésta
                let pf_id = egui::Id::new("phone_filter");
                let pf_on = ui.ctx().data(|d| d.get_temp::<u8>(pf_id)).unwrap_or(0) > 0;
                let (r, flt) = ui.allocate_exact_size(vec2(48.0, 48.0), Sense::click());
                flt.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Checkbox, true, pf_on, t("Filtrar carpeta")));
                theme::focus_ring(ui, &pal, &flt, 12.0);
                if flt.hovered() || pf_on || !self.filter.is_empty() {
                    ui.painter().rect_filled(r, CornerRadius::same(12), if pf_on { pal.sel } else { pal.hover });
                }
                if pf_on {
                    ui.painter().rect_stroke(r, CornerRadius::same(12), Stroke::new(1.0, a11y::sel_ring(&pal)), egui::StrokeKind::Inside);
                }
                theme::draw_glyph(ui.painter(), r.center(), 22.0, Glyph::Search, if pf_on || !self.filter.is_empty() { pal.accent } else { pal.text });
                if flt.clicked() {
                    if pf_on {
                        self.filter.clear();
                        self.spawn_sort();
                    }
                    // 12 fotogramas de foco forzado al abrir: el teclado del móvil quita el foco de forma transitoria
                    ui.ctx().data_mut(|d| d.insert_temp(pf_id, if pf_on { 0u8 } else { 12u8 }));
                }
                // selección múltiple
                let (r, sel) = ui.allocate_exact_size(vec2(48.0, 48.0), Sense::click());
                sel.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Checkbox, true, self.sel_mode, t("Seleccionar")));
                theme::focus_ring(ui, &pal, &sel, 12.0);
                if sel.hovered() || self.sel_mode {
                    ui.painter().rect_filled(r, CornerRadius::same(12), if self.sel_mode { pal.sel } else { pal.hover });
                }
                if self.sel_mode {
                    ui.painter().rect_stroke(r, CornerRadius::same(12), Stroke::new(1.0, a11y::sel_ring(&pal)), egui::StrokeKind::Inside);
                }
                theme::draw_glyph(ui.painter(), r.center(), 22.0, Glyph::Check, if self.sel_mode { pal.accent } else { pal.text });
                if sel.clicked() {
                    self.sel_mode = !self.sel_mode;
                    if !self.sel_mode {
                        self.clear_sel();
                    }
                }
                // pestañas abiertas (con más de una): cuadro con el número, abre la hoja de pestañas
                let ntabs = self.tabs.len();
                if ntabs > 1 && ui.available_width() >= 48.0 {
                    let (r, tb) = ui.allocate_exact_size(vec2(48.0, 48.0), Sense::click());
                    tb.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, crate::i18n::tn("{} pestaña abierta|{} pestañas abiertas", ntabs, &[])));
                    if tb.hovered() || self.h2.tabs_sheet {
                        ui.painter().rect_filled(r, CornerRadius::same(12), if self.h2.tabs_sheet { pal.sel } else { pal.hover });
                    }
                    let sq = Rect::from_center_size(r.center(), vec2(22.0, 22.0));
                    ui.painter().rect_stroke(sq, CornerRadius::same(5), Stroke::new(1.8, pal.text), egui::StrokeKind::Inside);
                    let num = if ntabs > 99 { "99+".to_string() } else { ntabs.to_string() };
                    ui.painter().text(sq.center(), Align2::CENTER_CENTER, num, FontId::proportional(if ntabs > 9 { 10.5 } else { 13.0 }), pal.text);
                    theme::focus_ring(ui, &pal, &tb, 12.0);
                    if tb.clicked() {
                        self.h2.tabs_sheet = true;
                    }
                }
            });
        });
        // segunda línea: sólo la ruta (tocar una carpeta navega; tocar el resto la edita)
        let w = ui.available_width().max(80.0);
        self.address_bar(ui, w);
    }

    /// Menú del móvil: sólo lo imprescindible.
    fn phone_menu(&mut self, ui: &mut egui::Ui) {
        let pal = self.pal;
        ui.set_width(250.0);
        ui.spacing_mut().interact_size.y = 48.0;
        ui.spacing_mut().item_spacing.y = 2.0;
        // historial y pestañas (en el móvil no hay atajos de teclado ni barra de pestañas)
        if !self.undo_stack.is_empty() && menu_row(ui, &pal, t("Deshacer"), false, None) {
            self.undo();
            ui.close();
        }
        if !self.redo_stack.is_empty() && menu_row(ui, &pal, t("Rehacer"), false, None) {
            self.redo();
            ui.close();
        }
        if !self.fwd.is_empty() && menu_row(ui, &pal, t("Adelante"), false, None) {
            self.go_forward();
            ui.close();
        }
        if menu_row(ui, &pal, &tf("Pestañas ({})…", &[&self.tabs.len()]), false, None) {
            self.h2.tabs_sheet = true;
            ui.close();
        }
        ui.separator();
        let (lbl, to_grid) = if self.cfg.grid { (t("Vista de lista"), false) } else { (t("Vista de cuadrícula"), true) };
        if menu_row(ui, &pal, lbl, false, None) {
            self.cfg.grid = to_grid;
            self.save_cfg();
            ui.close();
        }
        let bm = self.is_fav(&self.cwd);
        if menu_row(ui, &pal, if bm { t("Quitar de favoritos") } else { t("Añadir a favoritos") }, bm, None) {
            self.toggle_fav(self.cwd.clone());
            ui.close();
        }
        if menu_row(ui, &pal, t("Nueva carpeta"), false, None) {
            self.new_folder();
            ui.close();
        }
        if menu_row(ui, &pal, t("Nuevo archivo vacío…"), false, None) {
            self.dlg = Dlg::NewFile(t("Nuevo archivo.txt").into());
            ui.close();
        }
        if self.clipboard.is_some() && menu_row(ui, &pal, t("Pegar"), false, None) {
            self.do_paste();
            ui.close();
        }
        if menu_row(ui, &pal, t("Seleccionar todo"), false, None) {
            self.sel_mode = true;
            self.run_cmd(Cmd::SelectAll);
            ui.close();
        }
        if menu_row(ui, &pal, t("Buscar en subcarpetas"), false, None) {
            self.run_cmd(Cmd::Search);
            ui.close();
        }
        // todas las demás órdenes, buscándolas por su nombre
        if menu_row(ui, &pal, t("Paleta de comandos"), false, None) {
            ui.close();
            self.run_cmd(Cmd::Palette);
        }
        if self.cwd.parent().is_some() && menu_row(ui, &pal, t("Carpeta superior"), false, None) {
            self.go_up();
            ui.close();
        }
        if menu_row(ui, &pal, t("Actualizar"), false, None) {
            self.refresh();
            ui.close();
        }
        ui.separator();
        menu_head(ui, &pal, t("Ordenar"));
        for (k, l) in [
            (SortKey::Name, t("Nombre")),
            (SortKey::Date, t("Fecha de modificación")),
            (SortKey::Size, t("Tamaño")),
            (SortKey::Type, t("Tipo")),
        ] {
            if menu_row(ui, &pal, l, self.cfg.sort == k, Some(self.cfg.asc)) {
                self.sort_by(k);
                ui.close();
            }
        }
        ui.separator();
        menu_head(ui, &pal, t("Mostrar"));
        if menu_row(ui, &pal, t("Ocultos"), self.cfg.show_hidden, None) {
            self.cfg.show_hidden = !self.cfg.show_hidden;
            self.save_cfg();
            self.spawn_sort();
            ui.close();
        }
        ui.separator();
        menu_head(ui, &pal, t("Tema"));
        for (m, l) in [(0u8, t("Sistema")), (1, t("Claro")), (2, t("Oscuro"))] {
            if menu_row(ui, &pal, l, self.cfg.theme_mode == m, None) {
                self.set_theme_mode(m);
                ui.close();
            }
        }
        ui.separator();
        if !crate::vfs::WEB && menu_row(ui, &pal, t("Conectar a un servidor…"), false, None) {
            self.connect_uri(String::new());
            ui.close();
        }
        if menu_row(ui, &pal, t("Opciones"), false, None) {
            self.dlg = Dlg::Options;
            ui.close();
        }
        // ayuda: en el móvil sólo «Acerca de» (los atajos de teclado no aplican)
        if menu_row(ui, &pal, t("Acerca de"), false, None) {
            self.dlg = Dlg::About;
            ui.close();
        }
    }

    fn address_bar(&mut self, ui: &mut egui::Ui, w: f32) {
        let pal = self.pal;
        let (rect, resp) = ui.allocate_exact_size(vec2(w, if self.phone { 48.0 } else { 34.0 }), Sense::click());
        let ak = a11y::ak_on(ui.ctx(), resp.id);
        if self.addr_edit.is_none() {
            // con el teclado (Intro/Espacio sobre la barra con foco) se edita la ruta
            resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, format!("{}: {}", t("Editar ruta"), self.cwd.display())));
        }
        let p = ui.painter().clone();
        p.rect_filled(rect, CornerRadius::same(10), pal.surface);
        theme::focus_ring(ui, &pal, &resp, 10.0);
        let rect = if self.phone {
            rect
        } else {
        // estrella de favoritos dentro de la barra, a la derecha
        let bm = self.is_fav(&self.cwd);
        let br = Rect::from_center_size(pos2(rect.right() - 20.0, rect.center().y), vec2(30.0, 30.0));
        let bresp = ui.interact(br, ui.id().with("addr_bm"), Sense::click());
        bresp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Checkbox, true, bm, t("Favoritos")));
        if bresp.hovered() || bm {
            p.rect_filled(br, CornerRadius::same(8), if bm { pal.sel } else { pal.hover });
        }
        theme::focus_ring(ui, &pal, &bresp, 8.0);
        theme::draw_glyph(
            &p,
            br.center(),
            18.0,
            if bm { Glyph::StarFilled } else { Glyph::Star },
            if bm { pal.accent } else { pal.subtle },
        );
        let label = if bm { t("Quitar de favoritos") } else { t("Añadir a favoritos") };
        let keys = commands::shortcut_text(Cmd::Bookmark);
        let tip = if keys.is_empty() { label.to_string() } else { format!("{label} ({keys})") };
        if bresp.on_hover_text(tip).clicked() {
            self.toggle_fav(self.cwd.clone());
        }
            Rect::from_min_max(rect.min, pos2(rect.right() - 38.0, rect.bottom()))
        };
        if self.addr_edit.is_some() {
            let mut buf = self.addr_edit.take().unwrap();
            let sep = std::path::MAIN_SEPARATOR;
            // Sugerencias: subcarpetas que empiezan por el fragmento tecleado.
            let expanded = match (buf.strip_prefix('~'), &self.home) {
                (Some(rest), Some(h)) => format!("{}{rest}", h.display()),
                _ => buf.clone(),
            };
            let (dir_s, frag) = match expanded.rfind(['/', '\\']) {
                Some(i) => (expanded[..=i].to_string(), expanded[i + 1..].to_string()),
                None => (String::new(), expanded.clone()),
            };
            if self.addr_cache.0 != dir_s {
                // leer la carpeta puede tardar (red): en un hilo; las sugerencias llegan después
                self.addr_cache = (dir_s.clone(), Vec::new());
                if !dir_s.is_empty() {
                    let d = dir_s.clone();
                    self.spawn_io(
                        move || {
                            let mut names: Vec<String> = Vec::new();
                            if let Ok(rd) = crate::vfs::fs::read_dir(&d) {
                                for e in rd.flatten() {
                                    let ft = e.file_type();
                                    if ft.as_ref().map(|t| t.v_is_dir()).unwrap_or(false)
                                        || (ft.map(|t| t.v_is_symlink()).unwrap_or(false) && crate::vfs::fs::metadata(e.path()).map(|m| m.v_is_dir()).unwrap_or(false))
                                    {
                                        names.push(e.file_name().to_string_lossy().into_owned());
                                    }
                                }
                            }
                            names.sort_by(|a, b| fsys::natural_cmp(a, b));
                            (d, names)
                        },
                        |a, (d, names)| {
                            if a.addr_cache.0 == d {
                                a.addr_cache.1 = names;
                            }
                        },
                    );
                }
            }
            let fl = frag.to_lowercase();
            let sugg: Vec<String> = self
                .addr_cache
                .1
                .iter()
                .filter(|n| if fl.is_empty() { !n.starts_with('.') } else { n.to_lowercase().starts_with(&fl) })
                .filter(|n| n.to_lowercase() != fl)
                .take(8)
                .cloned()
                .collect();
            let prefix_len = buf.rfind(['/', '\\']).map(|i| i + 1).unwrap_or(0);
            let complete = |buf: &mut String, name: &str| {
                buf.truncate(prefix_len);
                buf.push_str(name);
                buf.push(sep);
            };
            let mut enter_pick: Option<String> = None;
            if !sugg.is_empty() {
                if ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::ArrowDown)) {
                    self.addr_hl = Some(self.addr_hl.map(|h| (h + 1).min(sugg.len() - 1)).unwrap_or(0));
                }
                if ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::ArrowUp)) {
                    self.addr_hl = Some(self.addr_hl.map(|h| h.saturating_sub(1)).unwrap_or(0));
                }
                if ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Tab)) {
                    let n = &sugg[self.addr_hl.unwrap_or(0).min(sugg.len() - 1)];
                    complete(&mut buf, n);
                    self.addr_hl = None;
                }
                if let Some(h) = self.addr_hl {
                    if ui.input(|i| i.key_pressed(Key::Enter)) {
                        enter_pick = sugg.get(h).cloned();
                    }
                }
            }
            if let Some(n) = enter_pick {
                complete(&mut buf, &n);
                self.addr_hl = None;
                self.navigate(PathBuf::from(buf.trim()));
                return;
            }
            let te = egui::TextEdit::singleline(&mut buf)
                // id fijo: con el teclado del móvil cambian los márgenes y un id automático podría variar,
                // perdiendo el foco (y ocultando el teclado) y la selección
                .id(egui::Id::new("addr_edit"))
                .font(FontId::proportional(14.0))
                .desired_width(rect.width() - 8.0)
                .min_size(vec2(rect.width() - 8.0, rect.height()))
                .vertical_align(Align::Center)
                .margin(egui::Margin::symmetric(8, 0))
                .frame(egui::Frame::NONE);
            let r = ui.put(rect.shrink2(vec2(2.0, 0.0)), te);
            if r.changed() {
                self.addr_hl = None;
            }
            if !r.has_focus() && !r.lost_focus() {
                r.request_focus();
            }
            if std::mem::take(&mut self.addr_select) {
                self.addr_select_n = 12;
            }
            if self.addr_select_n > 0 {
                if r.changed() {
                    self.addr_select_n = 0;
                } else if r.has_focus() {
                    self.addr_select_n -= 1;
                    let mut st = egui::TextEdit::load_state(ui.ctx(), r.id).unwrap_or_default();
                    let end = egui::text::CCursor::new(buf.chars().count());
                    st.cursor.set_char_range(Some(egui::text::CCursorRange::two(egui::text::CCursor::new(0), end)));
                    st.store(ui.ctx(), r.id);
                }
                ui.ctx().request_repaint();
            }
            let mut sugg_rect = None;
            if !sugg.is_empty() {
                let hl = self.addr_hl;
                let mut picked: Option<String> = None;
                let area = egui::Area::new(egui::Id::new("addr_sugg"))
                    .order(egui::Order::Foreground)
                    .fixed_pos(rect.left_bottom() + vec2(0.0, 4.0))
                    .show(ui.ctx(), |ui| {
                        egui::Frame::popup(ui.style()).show(ui, |ui| {
                            ui.set_min_width(rect.width() - 16.0);
                            for (i, n) in sugg.iter().enumerate() {
                                if ui.selectable_label(hl == Some(i), n).clicked() {
                                    picked = Some(n.clone());
                                }
                            }
                        });
                    });
                sugg_rect = Some(area.response.rect);
                if let Some(n) = picked {
                    complete(&mut buf, &n);
                    self.addr_hl = None;
                    self.addr_edit = Some(buf);
                    return;
                }
            }
            self.addr_sugg_rect = sugg_rect;
            let enter = ui.input(|i| i.key_pressed(Key::Enter));
            let esc = ui.input(|i| i.key_pressed(Key::Escape));
            let in_sugg = ui
                .input(|i| i.pointer.interact_pos())
                .map(|q| self.addr_sugg_rect.map(|r| r.contains(q)).unwrap_or(false))
                .unwrap_or(false);
            if enter {
                let t = buf.trim().to_string();
                let t = match (t.strip_prefix('~'), &self.home) {
                    (Some(rest), Some(h)) => format!("{}{rest}", h.display()),
                    _ => t,
                };
                self.addr_hl = None;
                let p = PathBuf::from(&t);
                if virt_mode(&p).is_none() && !t.contains("://") && matches!(app_nav::quick_stat(&p), Some((_, false))) {
                    self.say(tf("No existe: {}", &[&p.display()]), true);
                    self.addr_edit = Some(buf);
                    self.addr_select = true;
                } else {
                    self.navigate(p);
                }
            } else if esc || (r.lost_focus() && !in_sugg && (!self.phone || ui.input(|i| i.pointer.any_pressed()))) {
                // en el móvil el teclado puede quitar el foco al aparecer o recolocarse: sólo se cierra al tocar fuera
                self.addr_hl = None;
            } else {
                self.addr_edit = Some(buf);
            }
            return;
        }
        // migas de pan
        let mut segs: Vec<(String, PathBuf)> = Vec::new();
        let push_segs = |segs: &mut Vec<(String, PathBuf)>, path: &Path, base: Option<&Path>| {
            let mut acc = base.map(|b| b.to_path_buf()).unwrap_or_default();
            for c in path.components() {
                acc.push(c.as_os_str());
                match c {
                    Component::Prefix(pf) => segs.push((pf.as_os_str().to_string_lossy().into_owned(), acc.clone())),
                    Component::RootDir => {
                        if let Some(l) = segs.last_mut() {
                            l.1 = acc.clone();
                        } else {
                            segs.push(("/".into(), acc.clone()));
                        }
                    }
                    Component::Normal(n) => segs.push((n.to_string_lossy().into_owned(), acc.clone())),
                    _ => {}
                }
            }
        };
        if let Some((root, archive)) = self.arch_of(&self.cwd) {
            // dentro de un comprimido: ruta virtual «carpeta/archivo.zip/subcarpeta»
            push_segs(&mut segs, &archive, None);
            if let Some(l) = segs.last_mut() {
                l.1 = root.clone();
            }
            let rel = self.cwd.strip_prefix(&root).unwrap_or(Path::new(""));
            push_segs(&mut segs, rel, Some(&root));
        } else if virt_mode(&self.cwd).is_some() {
            segs.push((self.dir_label(&self.cwd), self.cwd.clone()));
        } else if let Some((mount, label)) = self.gvfs_mount_of(&self.cwd) {
            // carpeta de red montada: un solo tramo legible en vez de /run/user/1000/gvfs/smb-share:…
            segs.push((label, mount.clone()));
            let rel = self.cwd.strip_prefix(&mount).unwrap_or(Path::new(""));
            push_segs(&mut segs, rel, Some(&mount));
        } else {
            push_segs(&mut segs, &self.cwd, None);
        }
        let font = FontId::proportional(14.0);
        let widths: Vec<f32> = segs
            .iter()
            .map(|(s, _)| p.layout_no_wrap(s.clone(), font.clone(), pal.text).size().x + 18.0)
            .collect();
        let avail = rect.width() - 16.0;
        let sep = 14.0;
        let mut start = segs.len();
        let mut used = 0.0;
        while start > 0 {
            let add = widths[start - 1] + if start < segs.len() { sep } else { 0.0 };
            if used + add > avail - 28.0 && start < segs.len() {
                break;
            }
            used += add;
            start -= 1;
        }
        let mut x = rect.left() + 8.0;
        let ptr = ui.input(|i| i.pointer.hover_pos());
        if let Some(q) = ui.input(|i| if i.pointer.primary_pressed() { i.pointer.interact_pos() } else { None }) {
            self.addr_press = Some(q);
        }
        // al soltar el dedo no hay posición de puntero: usamos la del toque
        let click_pos = ptr.or(self.addr_press);
        // clic «de teclado» (Intro/Espacio con el foco en la barra): no hay puntero, se edita la ruta
        let kb_click = resp.clicked() && !ui.input(|i| i.pointer.any_released());
        let mut clicked_path: Option<PathBuf> = None;
        if start > 0 {
            p.text(pos2(x + 8.0, rect.center().y), Align2::LEFT_CENTER, "…", font.clone(), pal.subtle);
            x += 24.0;
        }
        for i in start..segs.len() {
            let r = Rect::from_min_size(pos2(x, rect.top() + 4.0), vec2(widths[i], rect.height() - 8.0));
            let hot = ptr.map(|q| r.contains(q) && resp.hovered()).unwrap_or(false);
            let last = i + 1 == segs.len();
            if hot {
                p.rect_filled(r, CornerRadius::same(8), pal.hover);
            }
            p.text(r.center(), Align2::CENTER_CENTER, &segs[i].0, font.clone(), if last { pal.text } else { pal.subtle });
            if ak {
                // tramo de la ruta para el lector de pantalla (sólo «pasa por encima»: el clic es de la barra)
                let h = ui.interact(r, ui.id().with(("crumb", i)), Sense::hover());
                h.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Link, true, &segs[i].0));
            }
            if resp.clicked() && !kb_click && click_pos.map(|q| r.contains(q)).unwrap_or(false) {
                clicked_path = Some(segs[i].1.clone());
            }
            x += widths[i];
            if !last {
                let c = pos2(x + sep / 2.0, rect.center().y);
                p.add(egui::Shape::line(
                    vec![c + vec2(-2.5, -4.0), c + vec2(1.5, 0.0), c + vec2(-2.5, 4.0)],
                    Stroke::new(1.3, pal.subtle),
                ));
                x += sep;
            }
        }
        if let Some(path) = clicked_path {
            self.navigate(path);
        } else if resp.clicked() {
            self.addr_edit = Some(self.cwd.display().to_string());
            self.addr_select = true;
        }
    }
}
