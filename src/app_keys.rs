//! Comandos, atajos, paleta de órdenes y teclado.
use super::*;

impl App {
    // -----------------------------------------------------------------------
    // Comandos, atajos y paleta
    // -----------------------------------------------------------------------

    /// Atajo efectivo de un comando (personalizado o por defecto).
    pub(super) fn binding(&self, cmd: Cmd) -> Option<(Modifiers, Key)> {
        let i = commands::info(cmd);
        let spec = self.cfg.keys.iter().find(|(id, _)| id == i.id).map(|(_, s)| s.as_str()).unwrap_or(i.default);
        if spec.is_empty() { None } else { commands::parse(spec) }
    }

    /// Texto en pantalla del atajo vigente (traducido y con las convenciones de la plataforma).
    pub(super) fn shortcut_text(&self, cmd: Cmd) -> String {
        commands::shortcut_text_in(cmd, &self.cfg.keys)
    }

    /// Cambia un atajo (None = sin atajo). Quien tuviera esa combinación se queda sin ella (comparando
    /// teclas, no textos); el editor de atajos pregunta antes (`request_binding`).
    pub(super) fn set_binding(&mut self, cmd: Cmd, spec: Option<String>) {
        commands::apply_binding(&mut self.cfg.keys, cmd, spec.as_deref());
        commands::set_user_keys(&self.cfg.keys);
        self.save_cfg();
    }

    pub(super) fn run_cmd(&mut self, cmd: Cmd) {
        use Cmd as C;
        // en la papelera, «eliminar» es eliminar definitivamente de ella
        if self.vmode == VMode::Trash && matches!(cmd, C::Trash | C::DeletePerm) {
            self.trash_ask_purge();
            return;
        }
        match cmd {
            C::Palette => self.palette = Some(PaletteState { born: self.ctx.cumulative_frame_nr(), ..Default::default() }),
            C::Back => self.go_back(),
            C::Forward => self.go_forward(),
            C::Up => self.go_up(),
            C::Refresh => self.refresh(),
            C::Home => {
                if let Some(h) = self.home.clone() {
                    self.navigate(h);
                }
            }
            C::EditPath => {
                self.addr_edit = Some(self.cwd.display().to_string());
                self.addr_select = true;
            }
            C::Bookmark => self.toggle_fav(self.cwd.clone()),
            C::OpenTrash => {
                if extras::trash_supported() {
                    self.navigate(PathBuf::from(LOC_TRASH));
                }
            }
            C::SysTerminal => self.sys_terminal_here(),
            C::NewTab => self.new_tab(self.cwd.clone()),
            C::CloseTab => self.close_tab(self.cur_tab),
            C::ReopenTab => self.reopen_tab(),
            C::NextTab => self.step_tab(1),
            C::PrevTab => self.step_tab(-1),
            C::NewWindow => self.new_window(),
            C::NewFolder => self.new_folder(),
            C::NewFile => self.dlg = Dlg::NewFile(t("Nuevo archivo.txt").into()),
            C::Rename => self.start_rename(),
            C::BatchRename => {
                if self.sel_count > 1 {
                    self.open_batch();
                }
            }
            C::Trash => self.do_trash(),
            C::DeletePerm => {
                let v = self.selected_paths();
                if !v.is_empty() {
                    self.confirm_delete = Some(v);
                }
            }
            C::Duplicate => self.do_duplicate(),
            C::CopyPath => {
                let t = self.selected_paths().iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join("\n");
                self.ctx.copy_text(t);
            }
            C::Undo => self.undo(),
            C::Redo => self.redo(),
            C::SelectAll => self.select_range(0, self.order.len().saturating_sub(1), false),
            C::InvertSel => self.invert_selection(),
            C::Props => self.open_props(),
            C::OpenWith => {
                if self.sel_count == 1 {
                    self.open_with();
                }
            }
            C::Hash => {
                if self.sel_count == 1 {
                    self.open_hash();
                }
            }
            C::Dups => self.open_dups(),
            C::Zip => self.do_zip(),
            C::Unzip => self.do_unzip(),
            C::Copy => self.do_copy(false),
            C::Cut => self.do_copy(true),
            C::Paste => self.do_paste(),
            C::Preview => self.toggle_preview(),
            C::ToggleHidden => {
                self.cfg.show_hidden = !self.cfg.show_hidden;
                self.save_cfg();
                self.spawn_sort();
            }
            C::PreviewPanel => {
                self.cfg.preview_panel = !self.cfg.preview_panel;
                self.save_cfg();
            }
            C::ToggleSidebar => {
                self.cfg.show_sidebar = !self.cfg.show_sidebar;
                self.save_cfg();
            }
            C::Terminal => self.toggle_terminal(),
            C::TerminalHere => self.terminal_here(),
            C::ToggleSplit => self.toggle_split(),
            C::SwitchPane => self.a11y_other_pane(),
            C::CopyOther => self.to_other_pane(false),
            C::MoveOther => self.to_other_pane(true),
            C::GridView => self.set_view(1),
            C::ListView => self.set_view(0),
            C::Theme => self.set_dark(!self.cfg.dark),
            C::Search => self.open_search(),
            C::Filter => self.filter_focus = true,
            C::Options => self.dlg = Dlg::Options,
            C::Keys => self.dlg = Dlg::Keys,
            C::ConnectServer => self.connect_uri(String::new()),
            C::Quit | C::Menu | C::ZoomIn | C::ZoomOut | C::ZoomReset | C::IconsBigger | C::IconsSmaller | C::EmptyTrash => self.h1_run(cmd),
            C::OtherPane => self.a11y_other_pane(),
        }
    }

    pub(super) fn palette_window(&mut self, ctx: &egui::Context) {
        let Some(mut ps) = self.palette.take() else { return };
        let pal = self.pal;
        let q = ps.query.trim().to_lowercase();
        let mut items: Vec<(i32, &'static commands::Info)> = commands::ALL
            .iter()
            .filter(|i| commands::available(i.cmd))
            .filter_map(|i| commands::fuzzy(&q, t(i.label)).map(|s| (s, i)))
            .collect();
        items.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| t(a.1.label).cmp(t(b.1.label))));
        let n = items.len();
        let (down, up, enter, esc) = ctx.input_mut(|i| {
            (
                i.consume_key(Modifiers::NONE, Key::ArrowDown),
                i.consume_key(Modifiers::NONE, Key::ArrowUp),
                i.consume_key(Modifiers::NONE, Key::Enter),
                i.consume_key(Modifiers::NONE, Key::Escape),
            )
        });
        if down && n > 0 {
            ps.sel = (ps.sel + 1) % n;
        }
        if up && n > 0 {
            ps.sel = (ps.sel + n - 1) % n;
        }
        ps.sel = ps.sel.min(n.saturating_sub(1));
        let mut run: Option<Cmd> = None;
        let mut close = esc;
        let before = ps.query.clone();
        let resp = egui::Area::new(egui::Id::new("palette"))
            .order(egui::Order::Foreground)
            .anchor(Align2::CENTER_TOP, vec2(0.0, 70.0))
            .show(ctx, |ui| {
                egui::Frame::new()
                    .fill(pal.panel)
                    .stroke(Stroke::new(1.0, pal.border))
                    .corner_radius(CornerRadius::same(16))
                    .inner_margin(egui::Margin::same(12))
                    .shadow(egui::Shadow { offset: [0, 8], blur: 24, spread: 0, color: Color32::from_black_alpha(110) })
                    .show(ui, |ui| {
                        ui.set_width(theme::dialog_width(ctx, 540.0));
                        let te = ui.add(
                            egui::TextEdit::singleline(&mut ps.query)
                                .hint_text(t("Escribe un comando…"))
                                .desired_width(f32::INFINITY)
                                .margin(vec2(8.0, 7.0)),
                        );
                        te.request_focus();
                        ui.add_space(6.0);
                        if n == 0 {
                            ui.label(egui::RichText::new(t("Sin resultados")).color(pal.subtle));
                        }
                        egui::ScrollArea::vertical().max_height(360.0).auto_shrink([false, true]).show(ui, |ui| {
                            for (pos, (_, info)) in items.iter().enumerate() {
                                let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 30.0), Sense::click());
                                let on = pos == ps.sel;
                                resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, true, on, t(info.label)));
                                if on || resp.hovered() {
                                    ui.painter().rect_filled(r, CornerRadius::same(8), if on { pal.sel } else { pal.hover });
                                }
                                if on {
                                    ui.painter().rect_stroke(r, CornerRadius::same(8), Stroke::new(1.0, a11y::sel_ring(&pal)), egui::StrokeKind::Inside);
                                }
                                if on && (down || up) {
                                    ui.scroll_to_rect(r, None);
                                }
                                ui.painter().text(
                                    pos2(r.left() + 10.0, r.center().y),
                                    Align2::LEFT_CENTER,
                                    t(info.label),
                                    FontId::proportional(14.0),
                                    pal.text,
                                );
                                let sc = self.shortcut_text(info.cmd);
                                if !sc.is_empty() {
                                    ui.painter().text(
                                        pos2(r.right() - 10.0, r.center().y),
                                        Align2::RIGHT_CENTER,
                                        sc,
                                        FontId::proportional(12.0),
                                        pal.subtle,
                                    );
                                }
                                if resp.clicked() {
                                    run = Some(info.cmd);
                                }
                            }
                        });
                    });
            });
        if ps.query != before {
            ps.sel = 0;
        }
        if enter {
            run = items.get(ps.sel).map(|(_, i)| i.cmd);
        }
        if ps.born != ctx.cumulative_frame_nr() && ctx.input(|i| i.pointer.primary_clicked()) {
            if let Some(q) = ctx.input(|i| i.pointer.interact_pos()) {
                if !resp.response.rect.contains(q) {
                    close = true;
                }
            }
        }
        if run.is_some() || close {
            if let Some(c) = run {
                self.run_cmd(c);
            }
        } else {
            self.palette = Some(ps);
        }
    }

    // -----------------------------------------------------------------------
    // Teclado
    // -----------------------------------------------------------------------

    pub(super) fn handle_keys(&mut self, ctx: &egui::Context) {
        // Windows: egui-winit convierte Mayús+Supr en «Cortar» sin evento de tecla; fuera de un campo de
        // texto se devuelve como la tecla (atajo «Eliminar definitivamente», y se puede grabar)
        if cfg!(windows) && !ctx.text_edit_focused() {
            ctx.input_mut(|i| {
                let m = i.modifiers;
                if m.shift && !m.command && !m.ctrl && !m.alt {
                    for e in i.events.iter_mut() {
                        if matches!(e, egui::Event::Cut) {
                            *e = egui::Event::Key { key: Key::Delete, physical_key: Some(Key::Delete), pressed: true, repeat: false, modifiers: m };
                        }
                    }
                }
            });
        }
        if self.a11y.bs_hold && !ctx.input(|i| i.key_down(Key::Backspace)) {
            self.a11y.bs_hold = false;
        }
        if let Some(cmd) = self.rebinding {
            let ev = ctx.input(|i| {
                i.events.iter().find_map(|e| match e {
                    egui::Event::Key { key, pressed: true, modifiers, .. } => Some((*key, *modifiers)),
                    _ => None,
                })
            });
            if let Some((key, m)) = ev {
                ctx.input_mut(|i| i.events.retain(|e| !matches!(e, egui::Event::Key { .. } | egui::Event::Text(_))));
                self.rebinding = None;
                if key != Key::Escape {
                    self.set_binding(cmd, Some(commands::format(m, key)));
                }
            }
            return;
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::F12) || i.consume_key(Modifiers::COMMAND, Key::Backtick)) {
            self.toggle_terminal();
            return;
        }
        if self.binding(Cmd::Menu).is_some_and(|(m, key)| ctx.input_mut(|i| consume_exact(i, m, key))) {
            if let Some(id) = self.menu_id {
                egui::Popup::toggle_id(ctx, id);
            }
            return;
        }
        if self.term_focus && self.term.is_some() {
            // salida del terminal con el teclado: «Ir al otro panel» (Ctrl+F6) vuelve a la lista
            let out = self.binding(Cmd::OtherPane).is_some_and(|(m, key)| ctx.input_mut(|i| consume_exact(i, m, key)))
                || ctx.input_mut(|i| consume_exact(i, Modifiers::COMMAND, Key::F6));
            if out {
                self.term_focus = false;
                ctx.memory_mut(|m| {
                    if let Some(id) = m.focused() {
                        m.surrender_focus(id);
                    }
                });
            }
            return; // el teclado es del terminal
        }
        // Esc de un campo de texto (egui ya le quitó el foco), de la paleta, del renombrado o de la
        // barra de dirección: es suyo, la lista no borra el filtro ni la selección
        if self.a11y_esc_taken() && ctx.input(|i| i.key_pressed(Key::Escape)) {
            return;
        }
        // «Ir al otro panel» vale también con el foco en un botón (no en un campo de texto ni en un menú)
        if self.other.is_some() && !ctx.text_edit_focused() && !egui::Popup::is_any_open(ctx) {
            if self.binding(Cmd::OtherPane).is_some_and(|(m, key)| ctx.input_mut(|i| consume_exact(i, m, key))) {
                self.run_cmd(Cmd::OtherPane);
                return;
            }
        }
        // con el foco en el área de archivos (o en ningún control) las teclas son de la lista
        if ctx.egui_wants_keyboard_input() && !self.a11y_files_focused(ctx) {
            return;
        }
        // con un menú abierto las teclas son suyas: no se actúa sobre la lista que hay detrás
        if egui::Popup::is_any_open(ctx) {
            return;
        }
        let (copy, cut, paste, text) = ctx.input(|i| {
            let mut t = String::new();
            for e in &i.events {
                match e {
                    egui::Event::Text(s) => t.push_str(s),
                    // texto compuesto con un método de entrada (chino, japonés, coreano…)
                    egui::Event::Ime(egui::ImeEvent::Commit(s)) => t.push_str(s),
                    _ => {}
                }
            }
            (
                i.events.iter().any(|e| matches!(e, egui::Event::Copy)),
                i.events.iter().any(|e| matches!(e, egui::Event::Cut)),
                i.events.iter().any(|e| matches!(e, egui::Event::Paste(_))),
                t,
            )
        });
        let k = |m: Modifiers, key: Key| ctx.input_mut(|i| consume_exact(i, m, key));

        if let Some((m, key)) = self.binding(Cmd::Palette) {
            if k(m, key) {
                self.run_cmd(Cmd::Palette);
                return;
            }
        }
        if self.preview.is_some() {
            self.preview_keys(ctx);
            return;
        }
        // con el puntero sobre la vista previa lateral y su búsqueda abierta, lo que se escribe es de ella
        let over_side = self.a11y.side_rect.zip(ctx.pointer_hover_pos()).is_some_and(|(r, p)| r.contains(p));
        if over_side && self.side.as_ref().is_some_and(|s| s.typing()) {
            return;
        }
        // Ctrl+C con texto seleccionado en la vista previa lateral copia el texto, no los archivos
        let copy = copy && !(over_side && self.side.as_ref().is_some_and(|s| s.has_text_sel()));

        // Comandos con atajo (configurable): primero los más específicos (más modificadores).
        let mut bound: Vec<(Cmd, Modifiers, Key)> = commands::ALL
            .iter()
            .filter(|i| i.rebind && i.cmd != Cmd::Palette)
            .filter_map(|i| self.binding(i.cmd).map(|(m, key)| (i.cmd, m, key)))
            .collect();
        bound.sort_by_key(|(_, m, _)| std::cmp::Reverse(m.alt as u8 + m.shift as u8 + (m.command || m.ctrl) as u8));
        if !self.filter.is_empty() {
            // con un filtro escrito: Retroceso borra una letra (Espacio sigue siendo la vista previa;
            // sólo se escribe un espacio si se hace clic en el cuadro «Filtrar»)
            if k(Modifiers::NONE, Key::Backspace) {
                self.filter.pop();
                self.spawn_sort();
                // mantener Retroceso pulsado borra el filtro, pero no sigue subiendo de carpeta
                self.a11y.bs_hold = true;
                return;
            }
        }
        // con el puntero sobre la vista previa lateral, sus teclas (desplazar, buscar) son de ella
        for (cmd, m, key) in bound {
            if matches!(cmd, Cmd::SwitchPane | Cmd::CopyOther | Cmd::MoveOther | Cmd::OtherPane) && self.other.is_none() {
                continue;
            }
            if key == Key::Backspace && self.a11y.bs_hold {
                continue;
            }
            if over_side && cmd == Cmd::Filter {
                continue;
            }
            // Tab sólo es de la lista en la vista dividida (ya se sabe que el foco está en ella y que no hay
            // menú ni diálogo); si no, recorre los controles
            let tab = key == Key::Tab && (m.is_none() || m.shift_only());
            if tab && !a11y::tab_switches_pane(self.other.is_some(), self.a11y_files_focused(ctx), false, false) {
                continue;
            }
            if k(m, key) {
                self.run_cmd(cmd);
                if tab {
                    // egui ya ha leído el Tab para mover el foco: se anula
                    ctx.memory_mut(|mem| mem.move_focus(egui::FocusDirection::None));
                }
            }
        }
        if self.a11y_shift_tab(ctx) {
            return;
        }
        if k(Modifiers::NONE, Key::Space) {
            self.toggle_preview();
            return;
        }
        // alias fijos
        if k(Modifiers::COMMAND | Modifiers::SHIFT, Key::Z) {
            self.redo();
        }
        if k(Modifiers::COMMAND, Key::R) {
            self.refresh();
        }
        if k(Modifiers::ALT, Key::ArrowUp) {
            self.go_up();
        }
        let cols = self.cols.max(1) as isize;
        let grid = self.cfg.grid;
        let step_v = if grid { cols } else { 1 };
        let page = self.page_rows.max(1) as isize * step_v;
        for (shift, m) in [(false, Modifiers::NONE), (true, Modifiers::SHIFT)] {
            if k(m, Key::ArrowDown) {
                self.move_cursor(step_v, shift);
            }
            if k(m, Key::ArrowUp) {
                self.move_cursor(-step_v, shift);
            }
            if grid {
                if k(m, Key::ArrowRight) {
                    self.move_cursor(1, shift);
                }
                if k(m, Key::ArrowLeft) {
                    self.move_cursor(-1, shift);
                }
            }
            if over_side {
                continue;
            }
            if k(m, Key::PageDown) {
                self.move_cursor(page, shift);
            }
            if k(m, Key::PageUp) {
                self.move_cursor(-page, shift);
            }
            if k(m, Key::Home) {
                self.move_cursor(-(self.order.len() as isize), shift);
            }
            if k(m, Key::End) {
                self.move_cursor(self.order.len() as isize, shift);
            }
        }
        if k(Modifiers::NONE, Key::Enter) {
            if let Some(c) = self.cursor.filter(|&c| self.is_sel(c)) {
                self.open_index(c);
            }
        }
        if k(Modifiers::NONE, Key::Escape) {
            if !self.filter.is_empty() {
                self.filter.clear();
                self.spawn_sort();
            } else {
                self.clear_sel();
            }
        }
        if copy {
            self.do_copy(false);
        }
        if cut {
            self.do_copy(true);
        }
        if paste {
            self.do_paste();
        }
        // Escribir en la lista filtra la carpeta con esas letras
        let printable: String = text.chars().filter(|c| !c.is_control()).collect();
        let ok = !printable.is_empty()
            && self.palette.is_none()
            && self.rebinding.is_none()
            && printable.trim().len() == printable.len();
        if ok {
            self.filter.push_str(&printable);
            self.spawn_sort();
            self.scroll_to_top();
        }
    }
}
