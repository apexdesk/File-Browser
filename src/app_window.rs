//! Menú principal, controles y barra de título de la ventana, y temas.
use super::*;

impl App {
    // ---- menú ----

    fn menu_items(&mut self, ui: &mut egui::Ui) {
        let has_sel = self.sel_count > 0;
        let one_zip = has_sel
            && self.sel_count == 1
            && self.cursor.and_then(|c| self.entries.get(c as usize)).map(|e| crate::archive::detect(Path::new(&*e.name)).is_some()).unwrap_or(false);
        {
            ui.menu_button(t("Archivo"), |ui| {
                ui.set_min_width(250.0);
                if mi(ui, t("Nueva pestaña"), &commands::shortcut_text(Cmd::NewTab), true) {
                    self.new_tab(self.cwd.clone());
                }
                if mi(ui, t("Cerrar pestaña"), &commands::shortcut_text(Cmd::CloseTab), self.tabs.len() > 1) {
                    self.close_tab(self.cur_tab);
                }
                ui.separator();
                if mi(ui, t("Nueva carpeta"), &commands::shortcut_text(Cmd::NewFolder), true) {
                    self.new_folder();
                }
                if mi(ui, t("Nuevo archivo vacío…"), &commands::shortcut_text(Cmd::NewFile), true) {
                    self.dlg = Dlg::NewFile(t("Nuevo archivo.txt").into());
                }
                ui.separator();
                if mi(ui, t("Abrir"), &commands::display("Enter"), has_sel) {
                    if let Some(c) = self.cursor.filter(|&c| self.is_sel(c)) {
                        self.open_index(c);
                    }
                }
                if mi(ui, t("Vista previa"), &commands::shortcut_text(Cmd::Preview), true) {
                    self.toggle_preview();
                }
                if self.sel_count == 1 {
                    self.open_with_menu(ui);
                } else if !crate::vfs::WEB {
                    ui.add_enabled(false, egui::Button::new(t("Abrir con")));
                }
                if mi(ui, t("Propiedades"), &commands::shortcut_text(Cmd::Props), true) {
                    self.open_props();
                }
                ui.separator();
                #[cfg(target_arch = "wasm32")]
                if mi(ui, t("Subir archivos…"), "", crate::vfs::net::writable()) {
                    crate::vfs::net::pick_files(self.ctx.clone());
                }
                if !crate::vfs::WEB && mi(ui, t("Nueva ventana"), &commands::shortcut_text(Cmd::NewWindow), true) {
                    self.new_window();
                }
                if mi(ui, t("Reabrir pestaña cerrada"), &commands::shortcut_text(Cmd::ReopenTab), !self.closed_tabs.is_empty()) {
                    self.reopen_tab();
                }
                ui.separator();
                if commands::available(Cmd::Quit) && mi(ui, t("Salir"), &commands::shortcut_text(Cmd::Quit), true) {
                    self.save_cfg();
                    self.ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            });
            ui.menu_button(t("Editar"), |ui| {
                ui.set_min_width(250.0);
                if mi(ui, t("Deshacer"), &commands::shortcut_text(Cmd::Undo), !self.undo_stack.is_empty()) {
                    self.undo();
                }
                if mi(ui, t("Rehacer"), &commands::shortcut_text(Cmd::Redo), !self.redo_stack.is_empty()) {
                    self.redo();
                }
                ui.separator();
                if mi(ui, t("Cortar"), &commands::shortcut_text(Cmd::Cut), has_sel) {
                    self.do_copy(true);
                }
                if mi(ui, t("Copiar"), &commands::shortcut_text(Cmd::Copy), has_sel) {
                    self.do_copy(false);
                }
                if mi(ui, t("Pegar"), &commands::shortcut_text(Cmd::Paste), true) {
                    self.do_paste();
                }
                if mi(ui, t("Duplicar"), &commands::shortcut_text(Cmd::Duplicate), has_sel) {
                    self.do_duplicate();
                }
                if mi(ui, t("Copiar ruta"), &commands::shortcut_text(Cmd::CopyPath), has_sel) {
                    let t = self.selected_paths().iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join("\n");
                    self.ctx.copy_text(t);
                }
                ui.separator();
                if mi(ui, t("Renombrar"), &commands::shortcut_text(Cmd::Rename), has_sel) {
                    self.start_rename();
                }
                // por `run_cmd`: en la vista Papelera estos dos eliminan definitivamente de ella
                if extras::trash_supported() && mi(ui, t("Mover a la papelera"), &commands::shortcut_text(Cmd::Trash), has_sel) {
                    self.run_cmd(Cmd::Trash);
                }
                if mi(ui, t("Eliminar definitivamente"), &commands::shortcut_text(Cmd::DeletePerm), has_sel) {
                    self.run_cmd(Cmd::DeletePerm);
                }
                ui.separator();
                if mi(ui, t("Seleccionar todo"), &commands::shortcut_text(Cmd::SelectAll), true) {
                    self.select_range(0, self.order.len().saturating_sub(1), false);
                }
                if mi(ui, t("Invertir selección"), &commands::shortcut_text(Cmd::InvertSel), true) {
                    self.invert_selection();
                }
            });
            ui.menu_button(t("Ver"), |ui| {
                ui.set_min_width(250.0);
                if mi_check(ui, t("Cuadrícula"), &commands::shortcut_text(Cmd::GridView), self.cfg.grid) {
                    self.set_view(1);
                }
                if mi_check(ui, t("Lista"), &commands::shortcut_text(Cmd::ListView), !self.cfg.grid && !self.cfg.col_view) {
                    self.set_view(0);
                }
                if mi_check(ui, t("Vista de columnas"), "", self.cfg.col_view) {
                    self.set_view(2);
                }
                ui.separator();
                if mi_check(ui, t("Vista dividida (dos paneles)"), &commands::shortcut_text(Cmd::ToggleSplit), self.other.is_some()) {
                    self.toggle_split();
                }
                if mi_check(ui, t("Paneles apilados (uno encima de otro)"), "", self.cfg.split_stack) {
                    self.cfg.split_stack = !self.cfg.split_stack;
                    self.save_cfg();
                }
                if mi_check(ui, t("Panel de vista previa"), &commands::shortcut_text(Cmd::PreviewPanel), self.cfg.preview_panel) {
                    self.cfg.preview_panel = !self.cfg.preview_panel;
                    self.save_cfg();
                }
                if mi_check(ui, t("Barra lateral"), &commands::shortcut_text(Cmd::ToggleSidebar), self.cfg.show_sidebar) {
                    self.cfg.show_sidebar = !self.cfg.show_sidebar;
                    self.save_cfg();
                }
                if mi_check(ui, t("Mostrar botón de menú (hamburguesa)"), "", self.cfg.show_menu_btn) {
                    self.cfg.show_menu_btn = !self.cfg.show_menu_btn;
                    self.save_cfg();
                }
                if !crate::vfs::WEB && mi_check(ui, t("Terminal"), "F12", self.cfg.term_open) {
                    self.toggle_terminal();
                }
                ui.separator();
                if mi_check(ui, t("Archivos ocultos"), &commands::shortcut_text(Cmd::ToggleHidden), self.cfg.show_hidden) {
                    self.cfg.show_hidden = !self.cfg.show_hidden;
                    self.save_cfg();
                    self.spawn_sort();
                }
                if mi_check(ui, t("Miniaturas"), "", self.cfg.thumbs_on) {
                    self.cfg.thumbs_on = !self.cfg.thumbs_on;
                    self.save_cfg();
                }
                if mi_check(ui, t("Iconos del sistema"), "", self.cfg.sys_icons) {
                    self.cfg.sys_icons = !self.cfg.sys_icons;
                    self.save_cfg();
                }
                if mi_check(ui, t("Tema oscuro"), &commands::shortcut_text(Cmd::Theme), self.cfg.dark) {
                    self.set_dark(!self.cfg.dark);
                }
                ui.separator();
                self.h1_view_menu(ui);
                ui.separator();
                if mi(ui, t("Actualizar"), &commands::shortcut_text(Cmd::Refresh), true) {
                    self.refresh();
                }
            });
            ui.menu_button(t("Ir"), |ui| {
                ui.set_min_width(230.0);
                if mi(ui, t("Atrás"), &commands::shortcut_text(Cmd::Back), !self.back.is_empty()) {
                    self.go_back();
                }
                if mi(ui, t("Adelante"), &commands::shortcut_text(Cmd::Forward), !self.fwd.is_empty()) {
                    self.go_forward();
                }
                if mi(ui, t("Carpeta superior"), &commands::shortcut_text(Cmd::Up), self.cwd.parent().is_some()) {
                    self.go_up();
                }
                if mi(ui, t("Inicio"), &commands::shortcut_text(Cmd::Home), self.home.is_some()) {
                    if let Some(h) = self.home.clone() {
                        self.navigate(h);
                    }
                }
                if mi(ui, t("Ir a la ruta…"), &commands::shortcut_text(Cmd::EditPath), true) {
                    self.addr_select = true;
                    self.addr_edit = Some(self.cwd.display().to_string());
                }
                if extras::trash_supported() && mi(ui, t("Papelera"), &commands::shortcut_text(Cmd::OpenTrash), true) {
                    self.navigate(PathBuf::from(LOC_TRASH));
                }
                self.h1_go_menu(ui);
                ui.separator();
                let bm = self.is_fav(&self.cwd);
                if mi(ui, if bm { t("Quitar de favoritos") } else { t("Añadir a favoritos") }, &commands::shortcut_text(Cmd::Bookmark), virt_mode(&self.cwd).is_none() || bm) {
                    self.toggle_fav(self.cwd.clone());
                }
            });
            ui.menu_button(t("Herramientas"), |ui| {
                ui.set_min_width(260.0);
                if mi(ui, t("Paleta de comandos"), &commands::shortcut_text(Cmd::Palette), true) {
                    self.run_cmd(Cmd::Palette);
                }
                ui.separator();
                if mi(ui, t("Buscar en subcarpetas…"), &commands::shortcut_text(Cmd::Search), true) {
                    self.open_search();
                }
                if mi(ui, t("Filtrar esta carpeta"), &commands::shortcut_text(Cmd::Filter), true) {
                    self.filter_focus = true;
                }
                ui.separator();
                if mi(ui, t("Comprimir…"), &commands::shortcut_text(Cmd::Zip), has_sel) {
                    self.do_zip();
                }
                if mi(ui, t("Calcular hash (MD5, SHA-1, SHA-256)…"), &commands::shortcut_text(Cmd::Hash), self.sel_count == 1) {
                    self.open_hash();
                }
                if mi(ui, t("Renombrar en lote…"), &Some(commands::shortcut_text(Cmd::BatchRename)).filter(|s| !s.is_empty()).unwrap_or_else(|| commands::shortcut_text(Cmd::Rename)), self.sel_count > 1) {
                    self.open_batch();
                }
                if mi(ui, t("Extraer aquí"), &commands::shortcut_text(Cmd::Unzip), one_zip) {
                    self.do_unzip();
                }
                ui.separator();
                if mi(ui, t("Buscar archivos duplicados…"), &commands::shortcut_text(Cmd::Dups), true) {
                    self.open_dups();
                }
                ui.separator();
                let here = self.cwd.clone();
                self.sys_term_menu_item(ui, &here);
                if !crate::vfs::WEB && mi(ui, t("Terminal integrado aquí"), &commands::shortcut_text(Cmd::TerminalHere), true) {
                    self.terminal_here();
                }
                #[cfg(not(target_os = "android"))]
                if !crate::vfs::WEB && mi(ui, t("Compartir esta carpeta…"), "", true) {
                    self.share_folder();
                }
                if mi(ui, t("Calcular tamaño / Propiedades"), &commands::shortcut_text(Cmd::Props), true) {
                    self.open_props();
                }
                ui.separator();
                if mi(ui, t("Opciones…"), &commands::shortcut_text(Cmd::Options), true) {
                    self.dlg = Dlg::Options;
                }
            });
            ui.menu_button(t("Ayuda"), |ui| {
                if mi(ui, t("Atajos de teclado"), &commands::shortcut_text(Cmd::Keys), true) {
                    self.dlg = Dlg::Keys;
                }
                if mi(ui, t("Acerca de"), "", true) {
                    self.dlg = Dlg::About;
                }
            });
        }
    }

    /// Decoración de ventana integrada: zona de arrastre con título + minimizar/maximizar/cerrar.
    fn window_controls(&mut self, ui: &mut egui::Ui, show_title: bool) {
        let pal = self.pal;
        let ctx = self.ctx.clone();
        let maximized = ctx.input(|i| i.viewport().maximized.unwrap_or(false));
        let btn_w = if self.cfg.native_deco { 0.0 } else { 40.0 };
        let w = (ui.available_width() - btn_w * 3.0 - 4.0).max(10.0);
        let (r, resp) = ui.allocate_exact_size(vec2(w, 34.0), Sense::click_and_drag());
        let title = format!(
            "{} — File Browser",
            self.dir_label(&self.cwd)
        );
        if show_title {
            ui.painter().text(r.center(), Align2::CENTER_CENTER, title, FontId::proportional(12.5), pal.subtle);
        }
        resp.context_menu(|ui| {
            if mi_check(ui, t("Mostrar botón de menú (hamburguesa)"), "", self.cfg.show_menu_btn) {
                self.cfg.show_menu_btn = !self.cfg.show_menu_btn;
                self.save_cfg();
            }
            if mi(ui, t("Nueva pestaña"), &commands::shortcut_text(Cmd::NewTab), true) {
                self.new_tab(self.cwd.clone());
            }
        });
        if resp.drag_started() {
            self.begin_window_drag(&ctx, None);
        }
        if resp.double_clicked() {
            ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(!maximized));
        }
        for k in 0..if self.cfg.native_deco { 0 } else { 3 } {
            let (br, bresp) = ui.allocate_exact_size(vec2(btn_w, 28.0), Sense::click());
            let bresp = bresp.on_hover_text(match k {
                0 => t("Minimizar").to_string(),
                1 if maximized => t("Restaurar").to_string(),
                1 => t("Maximizar").to_string(),
                _ => if commands::available(Cmd::Quit) { commands::with_shortcut(t("Cerrar"), Cmd::Quit) } else { t("Cerrar").to_string() },
            });
            let danger = k == 2;
            bresp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, match k {
                0 => t("Minimizar"),
                1 if maximized => t("Restaurar"),
                1 => t("Maximizar"),
                _ => t("Cerrar"),
            }));
            if bresp.hovered() {
                let fill = if danger { pal.danger_fill } else { pal.hover };
                ui.painter().rect_filled(br.shrink2(vec2(3.0, 1.0)), CornerRadius::same(8), fill);
            }
            theme::focus_ring(ui, &pal, &bresp, 8.0);
            let col = if danger && bresp.hovered() { pal.on_danger } else { pal.text };
            let g = match k {
                0 => Glyph::Minimize,
                1 if maximized => Glyph::Restore,
                1 => Glyph::Maximize,
                _ => Glyph::Close,
            };
            theme::draw_glyph(ui.painter(), br.center(), 18.0, g, col);
            if bresp.clicked() {
                match k {
                    0 => ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true)),
                    1 => ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(!maximized)),
                    _ => {
                        self.save_cfg();
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                }
            }
        }
    }

    /// Bordes para redimensionar la ventana sin decoración: 8 pt (10 en táctil; antes 5) a los lados,
    /// donde no pasan del margen de 10 pt que dejan los paneles alrededor del contenido.
    pub(super) fn resize_edges(&self, ctx: &egui::Context) -> Option<egui::ResizeDirection> {
        use egui::{CursorIcon as C, ResizeDirection as D};
        if ctx.input(|i| i.viewport().maximized.unwrap_or(false) || i.viewport().fullscreen.unwrap_or(false)) {
            return None;
        }
        let r = ctx.content_rect();
        let q = ctx.input(|i| i.pointer.hover_pos())?;
        let m = if theme::touch_ui() || self.touch { 10.0 } else { 8.0 };
        // arriba y abajo se queda en 5: la barra de título y la de estado sólo dejan 2-4 pt sobre sus botones
        let (l, rt, t, b) = (q.x < r.left() + m, q.x > r.right() - m, q.y < r.top() + 5.0, q.y > r.bottom() - 5.0);
        let hit = match (l, rt, t, b) {
            (true, _, true, _) => Some((D::NorthWest, C::ResizeNwSe)),
            (_, true, true, _) => Some((D::NorthEast, C::ResizeNeSw)),
            (true, _, _, true) => Some((D::SouthWest, C::ResizeNeSw)),
            (_, true, _, true) => Some((D::SouthEast, C::ResizeNwSe)),
            (true, ..) => Some((D::West, C::ResizeHorizontal)),
            (_, true, ..) => Some((D::East, C::ResizeHorizontal)),
            (_, _, true, _) => Some((D::North, C::ResizeVertical)),
            (_, _, _, true) => Some((D::South, C::ResizeVertical)),
            _ => None,
        };
        if let Some((dir, cur)) = hit {
            ctx.set_cursor_icon(cur);
            if ctx.input(|i| i.pointer.primary_pressed()) {
                return Some(dir);
            }
        }
        None
    }

    /// Fila superior: botón de menú (☰), pestañas (si hay más de una), zona de arrastre y botones de ventana.
    pub(super) fn title_bar(&mut self, ui: &mut egui::Ui) {
        let pal = self.pal;
        ui.horizontal(|ui| {
            ui.set_height(36.0);
            ui.spacing_mut().item_spacing.x = 4.0;
            let resp = if self.cfg.show_menu_btn {
                let (r, resp) = ui.allocate_exact_size(vec2(38.0, 34.0), Sense::click());
                if resp.hovered() || egui::Popup::is_id_open(ui.ctx(), egui::Popup::default_response_id(&resp)) {
                    ui.painter().rect_filled(r, CornerRadius::same(10), pal.hover);
                }
                theme::draw_glyph(ui.painter(), r.center(), 20.0, Glyph::Menu, pal.text);
                resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, t("Menú")));
                theme::focus_ring(ui, &pal, &resp, 10.0);
                resp.on_hover_text(commands::with_shortcut(t("Menú"), Cmd::Menu))
            } else {
                let rr = Rect::from_min_size(ui.cursor().min, vec2(1.0, 1.0));
                ui.interact(rr, ui.id().with("menu_anchor"), Sense::hover())
            };
            self.menu_id = Some(egui::Popup::default_response_id(&resp));
            egui::Popup::menu(&resp).show(|ui| {
                ui.set_min_width(210.0);
                self.menu_items(ui);
            });
            let multi = self.tabs.len() > 1;
            if multi {
                let ctl = if self.cfg.native_deco { 0.0 } else { 124.0 };
                let max_w = (ui.available_width() - ctl - 70.0).max(120.0);
                self.tab_bar(ui, max_w);
            }
            self.window_controls(ui, !multi);
        });
    }

    /// Tema del sistema: el de winit si lo informa, o el detectado por nosotros.
    pub(super) fn system_dark(&self) -> Option<bool> {
        self.ctx.system_theme().map(|t| t == egui::Theme::Dark).or(match self.sys_theme.load(Ordering::Relaxed) {
            1 => Some(false),
            2 => Some(true),
            _ => None,
        })
    }

    pub(super) fn apply_dark(&mut self, dark: bool) {
        self.cfg.dark = dark;
        self.cfg.sync_active();
        // el cambio es gradual: `tick_theme` va mezclando la paleta actual con la nueva
        self.theme_anim = Some((self.pal, theme::resolve(&self.cfg, dark), dark, Instant::now()));
        self.ctx.request_repaint();
    }

    /// Avanza la animación del cambio de tema claro/oscuro.
    pub(super) fn tick_theme(&mut self) {
        let Some((from, to, dark, t0)) = self.theme_anim else { return };
        const DUR: f32 = 0.30;
        let k = (t0.elapsed().as_secs_f32() / DUR).min(1.0);
        let e = k * k * (3.0 - 2.0 * k); // suavizado
        self.pal = from.lerp(&to, e);
        theme::apply_style(&self.ctx, &self.pal, dark);
        if k >= 1.0 {
            self.theme_anim = None;
        } else {
            self.ctx.request_repaint();
        }
    }

    /// Pestaña «Temas» de Opciones: una lista con los dos de serie arriba, los predefinidos y los propios.
    /// Cada tema tiene dos casillas: si hace de tema claro y/o de tema oscuro. Los de serie nunca se pierden
    /// ni se editan; solo los propios tienen colores editables (los demás se duplican).
    pub(super) fn themes_tab(&mut self, ui: &mut egui::Ui, width: f32) -> bool {
        let pal = self.pal;
        let mut changed = false;
        // (nombre, origen 0 serie / 1 predefinido / 2 propio, colores, clave en ranura clara, clave en ranura oscura)
        let mut rows: Vec<(String, u8, [[u8; 3]; 9], String, String)> = Vec::new();
        rows.push((t("Claro").to_string(), 0, theme::palette(false).colors(), String::new(), "@Claro".into()));
        rows.push((t("Oscuro").to_string(), 0, theme::palette(true).colors(), "@Oscuro".into(), String::new()));
        // los propios van justo tras los de serie, para que un tema nuevo se vea sin desplazar la lista
        for th in &self.cfg.custom_themes {
            rows.push((th.name.clone(), 2, th.colors, th.name.clone(), th.name.clone()));
        }
        for th in crate::theme::presets() {
            rows.push((th.name.clone(), 1, th.colors, th.name.clone(), th.name.clone()));
        }
        let mut mode: Option<u8> = None;
        let mut pick: Option<(bool, String)> = None;
        let mut dup: Option<usize> = None;
        let mut del: Option<String> = None;
        let mut edit: Option<String> = None;
        let mut create = false;

        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(t("Modo")).color(pal.subtle));
            for (m, l) in [(0u8, t("Sistema")), (1, t("Claro")), (2, t("Oscuro"))] {
                if ui.radio(self.cfg.theme_mode == m, l).clicked() {
                    mode = Some(m);
                }
            }
        });
        ui.add_space(2.0);
        ui.add(egui::Label::new(egui::RichText::new(t("Elige un tema claro y uno oscuro; «Sistema» alterna entre ellos. Los de serie no se pierden nunca.")).size(12.0).color(pal.subtle)).wrap());
        ui.add_space(8.0);

        // columnas: nombre | claro | oscuro | duplicar | eliminar
        let (cw, iw) = (56.0, 30.0);
        let list_w = width - 14.0;
        let name_w = list_w - 2.0 * cw - 2.0 * iw;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            ui.add_space(name_w);
            for l in [t("Claro"), t("Oscuro")] {
                let (r, _) = ui.allocate_exact_size(vec2(cw, 16.0), egui::Sense::hover());
                ui.painter().text(r.center(), Align2::CENTER_CENTER, l, egui::FontId::proportional(11.5), pal.subtle);
            }
        });
        let cur_slot_light = self.cfg.theme_light.clone();
        let cur_slot_dark = self.cfg.theme_dark.clone();
        egui::ScrollArea::vertical().max_height(300.0).auto_shrink([false, true]).show(ui, |ui| {
            ui.set_width(list_w);
            for i in 0..rows.len() {
                let (name, orig, colors, kl, kd) = rows[i].clone();
                if i > 0 && rows[i - 1].1 != orig {
                    ui.add_space(2.0);
                    ui.painter().hline(ui.available_rect_before_wrap().x_range(), ui.cursor().top(), Stroke::new(1.0, pal.border));
                    ui.add_space(4.0);
                }
                let editing = orig == 2 && self.edit_theme == name;
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
                    let (r, resp) = ui.allocate_exact_size(vec2(name_w, 34.0), egui::Sense::click());
                    resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, true, editing, &name));
                    theme::focus_ring(ui, &pal, &resp, 8.0);
                    if editing {
                        let er = Rect::from_min_max(r.min, pos2(r.max.x + cw * 2.0 + iw * 2.0, r.max.y));
                        ui.painter().rect_filled(er, 8.0, pal.sel);
                        ui.painter().rect_stroke(er, 8.0, Stroke::new(1.0, a11y::sel_ring(&pal)), egui::StrokeKind::Inside);
                    } else if resp.hovered() {
                        ui.painter().rect_filled(Rect::from_min_max(r.min, pos2(r.max.x + cw * 2.0 + iw * 2.0, r.max.y)), 8.0, pal.hover);
                    }
                    let mut x = r.min.x + 10.0;
                    for k in [0usize, 1, 2, 5, 7] {
                        let c = colors[k];
                        let sw = Rect::from_min_size(pos2(x, r.center().y - 7.0), vec2(14.0, 14.0));
                        ui.painter().rect_filled(sw, 4.0, Color32::from_rgb(c[0], c[1], c[2]));
                        ui.painter().rect_stroke(sw, 4.0, Stroke::new(1.0, pal.border), egui::StrokeKind::Inside);
                        x += 16.0;
                    }
                    ui.painter().text(pos2(x + 10.0, r.center().y), Align2::LEFT_CENTER, &name, egui::FontId::proportional(14.0), pal.text);
                    if orig == 2 && resp.clicked() {
                        edit = Some(name.clone());
                    }
                    for (dark, key, cur) in [(false, &kl, &cur_slot_light), (true, &kd, &cur_slot_dark)] {
                        let on = cur == key;
                        let (cr, cresp) = ui.allocate_exact_size(vec2(cw, 34.0), egui::Sense::click());
                        let cresp = cresp.on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_text(if dark { t("Usar en modo oscuro") } else { t("Usar en modo claro") });
                        cresp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::RadioButton, true, on, format!("{name}: {}", if dark { t("Usar en modo oscuro") } else { t("Usar en modo claro") })));
                        let b = Rect::from_center_size(cr.center(), vec2(18.0, 18.0));
                        if on {
                            ui.painter().rect_filled(b, 5.0, pal.accent);
                            let st = Stroke::new(2.0, pal.on_accent);
                            let c = b.center();
                            ui.painter().line_segment([c + vec2(-4.0, 0.5), c + vec2(-1.0, 3.5)], st);
                            ui.painter().line_segment([c + vec2(-1.0, 3.5), c + vec2(4.5, -3.0)], st);
                        } else {
                            ui.painter().rect_stroke(b, 5.0, Stroke::new(1.5, if cresp.hovered() { pal.accent } else { pal.outline }), egui::StrokeKind::Inside);
                        }
                        theme::focus_ring(ui, &pal, &cresp, 6.0);
                        if cresp.clicked() && !on {
                            pick = Some((dark, key.clone()));
                        }
                    }
                    let (dr, _) = ui.allocate_exact_size(vec2(iw, 34.0), egui::Sense::hover());
                    if theme::icon_button(&mut ui.new_child(egui::UiBuilder::new().max_rect(Rect::from_center_size(dr.center(), vec2(28.0, 28.0)))), &pal, theme::Icon::Copy, t("Duplicar")).clicked() {
                        dup = Some(i);
                    }
                    let (er, _) = ui.allocate_exact_size(vec2(iw, 34.0), egui::Sense::hover());
                    if orig == 2
                        && theme::icon_button(&mut ui.new_child(egui::UiBuilder::new().max_rect(Rect::from_center_size(er.center(), vec2(28.0, 28.0)))), &pal, theme::Icon::Trash, t("Eliminar tema")).clicked()
                    {
                        del = Some(name.clone());
                    }
                });
            }
        });
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if theme::dialog_button(ui, &pal, theme::Btn::Plain, t("Nuevo tema")).clicked() {
                create = true;
            }
        });

        // editor del tema propio seleccionado
        if let Some(i) = self.cfg.custom_themes.iter().position(|t| t.name == self.edit_theme) {
            ui.add_space(10.0);
            let mut edited = false;
            let mut rename: Option<String> = None;
            egui::Frame::new().fill(pal.surface).stroke(Stroke::new(1.0, pal.border)).corner_radius(CornerRadius::same(12)).inner_margin(egui::Margin::same(12)).show(ui, |ui| {
                ui.set_width(width - 26.0);
                let others: Vec<String> = self.cfg.custom_themes.iter().enumerate().filter(|(j, _)| *j != i).map(|(_, t)| t.name.clone()).collect();
                let th = &mut self.cfg.custom_themes[i];
                // el nombre se edita en un búfer y se aplica con Intro o al salir del campo; Esc lo descarta
                let bid = egui::Id::new("theme_name_buf");
                ui.horizontal(|ui| {
                    let lbl = ui.label(t("Nombre")).id;
                    let mut n = ui.data(|d| d.get_temp::<(String, String)>(bid)).filter(|(of, _)| *of == th.name).map(|b| b.1).unwrap_or_else(|| th.name.clone());
                    let r = ui.add(egui::TextEdit::singleline(&mut n).desired_width(220.0)).labelled_by(lbl);
                    let clean: String = n.chars().filter(|c| !matches!(c, '|' | '\n' | '\r' | '@')).take(40).collect();
                    let ok = !clean.is_empty() && (clean == th.name || (!others.contains(&clean) && !crate::theme::presets().iter().any(|t| t.name == clean)));
                    if r.lost_focus() {
                        if ok && clean != th.name && !ui.input(|i| i.key_pressed(Key::Escape)) {
                            rename = Some(clean);
                        }
                        ui.data_mut(|d| d.remove::<(String, String)>(bid));
                    } else if r.has_focus() {
                        ui.data_mut(|d| d.insert_temp(bid, (th.name.clone(), n)));
                        if !ok {
                            ui.label(egui::RichText::new(t("Nombre no válido o ya en uso")).color(pal.danger));
                        }
                    }
                });
                ui.add_space(6.0);
                let labels = [t("Fondo"), t("Panel lateral"), t("Superficie"), t("Resaltado"), t("Borde"), t("Texto"), t("Texto secundario"), t("Acento"), t("Fondo del visor")];
                egui::Grid::new("theme_colors").num_columns(3).spacing([18.0, 8.0]).show(ui, |ui| {
                    for (k, l) in labels.iter().enumerate() {
                        ui.horizontal(|ui| {
                            let r = ui.color_edit_button_srgb(&mut th.colors[k]);
                            r.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::ColorButton, true, *l));
                            edited |= r.changed();
                            ui.label(*l);
                        });
                        if k % 3 == 2 {
                            ui.end_row();
                        }
                    }
                });
            });
            if let Some(n) = rename {
                let n: String = n.chars().filter(|c| !matches!(c, '|' | '\n' | '\r' | '@')).take(40).collect();
                let taken = self.cfg.custom_themes.iter().any(|t| t.name == n) || crate::theme::presets().iter().any(|t| t.name == n);
                if !n.is_empty() && !taken {
                    let old = std::mem::replace(&mut self.cfg.custom_themes[i].name, n.clone());
                    if self.cfg.theme_light == old {
                        self.cfg.theme_light = n.clone();
                    }
                    if self.cfg.theme_dark == old {
                        self.cfg.theme_dark = n.clone();
                    }
                    self.edit_theme = n;
                    self.cfg.sync_active();
                    changed = true;
                }
            }
            if edited {
                self.refresh_custom_theme();
                changed = true;
            }
        } else {
            ui.add_space(6.0);
            ui.add(egui::Label::new(egui::RichText::new(t("Los temas de serie y predefinidos no se pueden modificar: duplícalos para editar sus colores.")).size(12.0).color(pal.subtle)).wrap());
        }

        if let Some(m) = mode {
            self.set_theme_mode(m);
        }
        if let Some((dark, key)) = pick {
            // con un modo fijo se pasa al de la casilla para que el tema elegido se vea; con «Sistema» solo se asigna
            if self.cfg.theme_mode != 0 && self.cfg.theme_mode != if dark { 2 } else { 1 } {
                self.cfg.theme_mode = if dark { 2 } else { 1 };
            }
            if dark {
                self.cfg.theme_dark = key;
            } else {
                self.cfg.theme_light = key;
            }
            let show = if self.cfg.theme_mode == 0 { self.cfg.dark } else { dark };
            self.apply_dark(show);
            changed = true;
        }
        let create_from: Option<(Option<usize>, bool)> = if let Some(i) = dup { Some((Some(i), self.cfg.dark)) } else if create { Some((None, self.cfg.dark)) } else { None };
        if let Some((src, side)) = create_from {
            if self.cfg.custom_themes.len() < 32 {
                let (colors, stem) = match src {
                    Some(i) => (rows[i].2, rows[i].0.clone()),
                    None => (self.pal.colors(), t("Tema").to_string()),
                };
                let mut k = 1;
                let name = loop {
                    let n = if src.is_some() { format!("{stem} {}", k + 1) } else { format!("{stem} {}", self.cfg.custom_themes.len() + k) };
                    let taken = self.cfg.custom_themes.iter().any(|t| t.name == n) || crate::theme::presets().iter().any(|t| t.name == n);
                    if !taken {
                        break n;
                    }
                    k += 1;
                };
                self.cfg.custom_themes.push(crate::config::CustomTheme { name: name.clone(), dark: side, colors });
                self.edit_theme = name.clone();
                if side {
                    self.cfg.theme_dark = name;
                } else {
                    self.cfg.theme_light = name;
                }
                let d = self.cfg.dark;
                self.apply_dark(d);
                changed = true;
            }
        }
        if let Some(name) = del {
            if let Some(i) = self.cfg.custom_themes.iter().position(|t| t.name == name) {
                self.cfg.custom_themes.remove(i);
                // las ranuras que lo usaban vuelven al tema de serie
                if self.cfg.theme_light == name {
                    self.cfg.theme_light.clear();
                }
                if self.cfg.theme_dark == name {
                    self.cfg.theme_dark.clear();
                }
                if self.edit_theme == name {
                    self.edit_theme.clear();
                }
                let d = self.cfg.dark;
                self.apply_dark(d);
                changed = true;
            }
        }
        if let Some(n) = edit {
            self.edit_theme = n;
        }
        changed
    }

    /// Edición en vivo de los colores del tema propio activo.
    fn refresh_custom_theme(&mut self) {
        self.theme_anim = None;
        let dark = self.cfg.dark;
        self.pal = theme::resolve(&self.cfg, dark);
        theme::apply_style(&self.ctx, &self.pal, dark);
        self.ctx.request_repaint();
    }

    /// Tema explícito (claro u oscuro).
    pub(super) fn set_dark(&mut self, dark: bool) {
        self.cfg.theme_mode = if dark { 2 } else { 1 };
        self.apply_dark(dark);
        self.save_cfg();
    }

    /// 0 = según el sistema, 1 = claro, 2 = oscuro.
    pub(super) fn set_theme_mode(&mut self, mode: u8) {
        self.cfg.theme_mode = mode;
        let dark = match mode {
            1 => false,
            2 => true,
            _ => self.system_dark().unwrap_or(self.cfg.dark),
        };
        self.apply_dark(dark);
        self.save_cfg();
    }
}
