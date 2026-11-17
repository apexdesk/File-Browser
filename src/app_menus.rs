// Menús, atajos de teclado y opciones de interfaz de la ventana principal: editor de atajos (con aviso de
// conflictos y búsqueda), tecla Alt/F10 para el menú, Salir y escala de la interfaz como comandos,
// tarjeta de bienvenida del primer arranque y confirmación opcional al mover a la papelera.
// Es un submódulo de `app` (ve sus campos privados); el estado propio vive en `App::h1`.

use super::*;

/// Estado de este módulo dentro de `App` (un solo campo, `h1`).
#[derive(Default)]
#[cfg_attr(target_os = "macos", allow(dead_code))]
pub(super) struct State {
    /// búsqueda del editor de atajos (por nombre o por tecla)
    keys_query: String,
    /// atajo pedido que ya usa otro comando: (comando, atajo guardado, comando que lo tiene)
    keys_ask: Option<(Cmd, String, Cmd)>,
    /// pidiendo confirmación para «Restablecer todos»
    keys_reset_all: bool,
    /// aviso en el editor de atajos (atajo reservado o de un comando fijo)
    keys_note: Option<String>,
    /// Alt pulsada sola: cuándo (segundos de egui) y si se ha usado con otra tecla o el ratón
    alt_down: Option<f64>,
    alt_used: bool,
    alt_prev: bool,
    /// casilla del índice en la bienvenida
    welcome_index: bool,
    /// escala de la interfaz mientras se arrastra el deslizador (se aplica al soltarlo)
    scale_edit: Option<f32>,
}

/// Botón de menú contextual con el atajo vigente del comando a la derecha.
pub(super) fn sc_btn(label: &'static str, cmd: Cmd) -> egui::Button<'static> {
    egui::Button::new(label).shortcut_text(commands::shortcut_text(cmd))
}

/// Escala de la interfaz válida: entre 75 % y 200 %, en pasos de 5 %.
fn clamp_scale(v: f32) -> f32 {
    if !v.is_finite() {
        return 1.0;
    }
    ((v * 20.0).round() / 20.0).clamp(0.75, 2.0)
}

impl App {
    /// Al crear la ventana: atajos publicados, escala, animaciones, vista inicial y bienvenida.
    pub(super) fn h1_init(&mut self) {
        commands::set_user_keys(&self.cfg.keys);
        let ctx = self.ctx.clone();
        // Salir y la escala son comandos propios (configurables): fuera los atajos fijos de egui
        ctx.options_mut(|o| {
            o.quit_shortcuts.clear();
            o.zoom_with_keyboard = false;
        });
        self.cfg.ui_scale = clamp_scale(self.cfg.ui_scale);
        ctx.set_zoom_factor(self.cfg.ui_scale);
        self.apply_motion();
        match self.cfg.start_view {
            1 => {
                self.cfg.grid = true;
                self.cfg.col_view = false;
            }
            2 => {
                self.cfg.grid = false;
                self.cfg.col_view = false;
            }
            3 => {
                self.cfg.grid = false;
                self.cfg.col_view = true;
            }
            _ => {}
        }
        theme::set_touch_ui(self.touch || cfg!(target_os = "android"));
        self.h1.welcome_index = !crate::vfs::WEB;
        // las capturas automáticas no se quedan en la bienvenida (salvo FB_DLG=welcome)
        #[cfg(feature = "selftest")]
        let shot = std::env::var_os("FB_SHOT").is_some();
        #[cfg(not(feature = "selftest"))]
        let shot = false;
        if self.cfg.first_run && !shot {
            self.dlg = Dlg::Welcome;
        }
    }

    /// «Reducir animaciones»: 0 según el sistema, 1 sí, 2 no.
    fn apply_motion(&self) {
        let on = match self.cfg.reduce_motion {
            1 => true,
            2 => false,
            _ => theme::detect_reduced_motion(),
        };
        theme::set_reduced_motion(on);
    }

    /// Cada fotograma, tras los diálogos: interfaz táctil, escala, Alt sola y atajos que valen siempre.
    pub(super) fn h1_frame(&mut self, ctx: &egui::Context) {
        theme::set_touch_ui(self.touch || cfg!(target_os = "android"));
        if self.h1.scale_edit.is_none() && (ctx.zoom_factor() - self.cfg.ui_scale).abs() > 0.001 {
            ctx.set_zoom_factor(self.cfg.ui_scale);
        }
        #[cfg(not(target_os = "macos"))]
        self.alt_menu(ctx);
        // Salir y la escala también con un diálogo abierto o escribiendo en un campo (si no, ya los ha
        // atendido `handle_keys`); el terminal se queda sus teclas
        if self.rebinding.is_some() || (self.term_focus && self.term.is_some()) {
            return;
        }
        for c in [Cmd::Quit, Cmd::ZoomIn, Cmd::ZoomOut, Cmd::ZoomReset] {
            if !commands::available(c) {
                continue;
            }
            if let Some((m, key)) = self.binding(c) {
                if ctx.input_mut(|i| consume_exact(i, m, key)) {
                    self.h1_run(c);
                }
            }
        }
        // Ctrl+= también amplía (en muchos teclados «+» es Mayús+=)
        if self.binding(Cmd::ZoomIn) == commands::parse("Ctrl+Plus") && ctx.input_mut(|i| consume_exact(i, Modifiers::COMMAND, Key::Equals)) {
            self.h1_run(Cmd::ZoomIn);
        }
    }

    /// Alt pulsada y soltada sola abre o cierra el menú (como en Windows y muchas aplicaciones de Linux).
    #[cfg(not(target_os = "macos"))]
    fn alt_menu(&mut self, ctx: &egui::Context) {
        let (alt, alt_only, other, now, focused) = ctx.input(|i| {
            let m = i.modifiers;
            let other = i.pointer.any_down()
                || i.events.iter().any(|e| {
                    matches!(e, egui::Event::Key { .. } | egui::Event::Text(_) | egui::Event::PointerButton { .. } | egui::Event::MouseWheel { .. })
                });
            (m.alt, m.alt && !m.ctrl && !m.shift && !m.command && !m.mac_cmd, other, i.time, i.viewport().focused.unwrap_or(true))
        });
        let idle = matches!(self.dlg, Dlg::None)
            && self.palette.is_none()
            && !self.touch
            && !self.phone
            && focused
            && self.rebinding.is_none()
            && !(self.term_focus && self.term.is_some())
            && !ctx.egui_wants_keyboard_input();
        if alt_only && !self.h1.alt_prev {
            self.h1.alt_down = Some(now);
            self.h1.alt_used = other || !idle;
        } else if alt && (other || !alt_only || !idle) {
            self.h1.alt_used = true;
        }
        if !alt && self.h1.alt_prev {
            if let Some(t0) = self.h1.alt_down.take() {
                if !self.h1.alt_used && idle && now - t0 < 1.0 {
                    if let Some(id) = self.menu_id {
                        egui::Popup::toggle_id(ctx, id);
                    }
                }
            }
        }
        self.h1.alt_prev = alt;
    }

    /// Comandos de este módulo (Salir, menú, escala, iconos y vaciar la papelera).
    pub(super) fn h1_run(&mut self, cmd: Cmd) {
        match cmd {
            Cmd::Quit => {
                if commands::available(Cmd::Quit) {
                    self.save_cfg();
                    self.ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            }
            Cmd::Menu => {
                if let Some(id) = self.menu_id {
                    egui::Popup::toggle_id(&self.ctx, id);
                }
            }
            Cmd::ZoomIn => self.set_ui_scale(self.cfg.ui_scale + 0.1),
            Cmd::ZoomOut => self.set_ui_scale(self.cfg.ui_scale - 0.1),
            Cmd::ZoomReset => self.set_ui_scale(1.0),
            Cmd::IconsBigger | Cmd::IconsSmaller => {
                let d = if cmd == Cmd::IconsBigger { 16.0 } else { -16.0 };
                self.cfg.zoom = (self.cfg.zoom + d).clamp(64.0, 200.0);
                self.save_cfg();
            }
            Cmd::EmptyTrash => {
                if commands::available(Cmd::EmptyTrash) {
                    self.confirm_purge = Some(PurgeReq::All);
                }
            }
            _ => {}
        }
    }

    /// Cambia la escala de la interfaz (se guarda y se avisa del valor).
    fn set_ui_scale(&mut self, v: f32) {
        let v = clamp_scale(v);
        self.h1.scale_edit = None;
        if (v - self.cfg.ui_scale).abs() > 0.001 || (self.ctx.zoom_factor() - v).abs() > 0.001 {
            self.cfg.ui_scale = v;
            self.ctx.set_zoom_factor(v);
            self.save_cfg();
        }
        self.say(tf("Escala de la interfaz: {} %", &[&((v * 100.0).round() as i32)]), false);
    }

    // ---- menú principal ----

    /// Entradas añadidas al menú «Ver»: escala de la interfaz y tamaño de los iconos.
    pub(super) fn h1_view_menu(&mut self, ui: &mut egui::Ui) {
        for c in [Cmd::ZoomIn, Cmd::ZoomOut, Cmd::ZoomReset, Cmd::IconsBigger, Cmd::IconsSmaller] {
            let on = match c {
                Cmd::ZoomIn => self.cfg.ui_scale < 2.0,
                Cmd::ZoomOut => self.cfg.ui_scale > 0.75,
                Cmd::ZoomReset => (self.cfg.ui_scale - 1.0).abs() > 0.001,
                Cmd::IconsBigger => self.cfg.zoom < 200.0,
                _ => self.cfg.zoom > 64.0,
            };
            if mi(ui, t(commands::info(c).label), &commands::shortcut_text(c), on) {
                self.h1_run(c);
            }
        }
    }

    /// Entradas añadidas al menú «Ir»: vaciar la papelera y desmontar recursos de red.
    pub(super) fn h1_go_menu(&mut self, ui: &mut egui::Ui) {
        if commands::available(Cmd::EmptyTrash) && mi(ui, t("Vaciar papelera"), &commands::shortcut_text(Cmd::EmptyTrash), true) {
            self.h1_run(Cmd::EmptyTrash);
        }
        #[cfg(target_os = "linux")]
        {
            let mounts: Vec<(String, PathBuf)> = self.network.iter().filter_map(|n| n.path.clone().map(|p| (n.name.clone(), p))).collect();
            if !mounts.is_empty() {
                let mut go: Option<PathBuf> = None;
                ui.menu_button(t("Desmontar"), |ui| {
                    for (name, p) in mounts {
                        if mi(ui, &name, "", true) {
                            go = Some(p);
                        }
                    }
                });
                if let Some(p) = go {
                    self.unmount(p);
                }
            }
        }
    }

    // ---- editor de atajos ----

    /// Pide un atajo para `cmd`: si es de otro comando reasignable se pregunta; si es de un comando fijo
    /// o una tecla que hace falta para escribir, se avisa y no se cambia.
    fn request_binding(&mut self, cmd: Cmd, spec: Option<String>) {
        self.h1.keys_note = None;
        let Some(s) = spec.filter(|s| !s.is_empty()) else {
            self.set_binding(cmd, None);
            return;
        };
        if commands::reserved_plain(&s) {
            self.h1.keys_note = Some(tf("{} no se puede usar sola: hace falta para escribir y moverse por la lista", &[&commands::display(&s)]));
            return;
        }
        match commands::conflict_in(&s, &self.cfg.keys, Some(cmd)) {
            Some(o) if !commands::info(o).rebind => {
                self.h1.keys_note = Some(tf("{} está reservado para «{}»", &[&commands::display(&s), &t(commands::info(o).label)]));
            }
            Some(o) => self.h1.keys_ask = Some((cmd, s, o)),
            None => self.set_binding(cmd, Some(s)),
        }
    }

    /// Diálogo «Atajos de teclado». Devuelve si sigue abierto.
    pub(super) fn keys_dialog(&mut self, ctx: &egui::Context) -> bool {
        let pal = self.pal;
        // esperando una combinación: la primera tecla pulsada es el atajo (Esc cancela). Se captura aquí
        // porque con un diálogo abierto no se atiende el teclado de la ventana.
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
                    self.request_binding(cmd, Some(commands::format(m, key)));
                }
            }
        }
        let mut open = true;
        let mut start: Option<Cmd> = None;
        let mut reset: Option<Cmd> = None;
        let mut clear: Option<Cmd> = None;
        // texto de la pregunta del pie (conflicto o restablecer todos)
        let ask = self.h1.keys_ask.as_ref().map(|(cmd, s, o)| {
            let (lc, lo) = (t(commands::info(*cmd).label), t(commands::info(*o).label));
            tf("{} ya es el atajo de «{}». ¿Se lo asignas a «{}»? «{}» se quedará sin atajo.", &[&commands::display(s), &lo, &lc, &lo])
        });
        let reset_all = self.h1.keys_reset_all;
        let act = theme::card_window_with_footer(
            ctx,
            &pal,
            t("Atajos de teclado"),
            600.0,
            &mut open,
            |ui| {
                ui.add(egui::Label::new(egui::RichText::new(t("Pulsa un atajo para cambiarlo. Con Esc se cancela.")).color(pal.subtle)).wrap());
                ui.add_space(6.0);
                ui.add(egui::TextEdit::singleline(&mut self.h1.keys_query).hint_text(t("Buscar por nombre o por tecla…")).desired_width(f32::INFINITY));
                if let Some(n) = &self.h1.keys_note {
                    ui.add_space(4.0);
                    ui.add(egui::Label::new(egui::RichText::new(n.as_str()).color(pal.warn)).wrap());
                }
                ui.add_space(8.0);
                let q = self.h1.keys_query.clone();
                let rows: Vec<&'static commands::Info> = commands::ALL
                    .iter()
                    .filter(|i| commands::available(i.cmd))
                    .filter(|i| commands::matches_query(&q, t(i.label), &self.shortcut_text(i.cmd), &commands::spec_in(i.cmd, &self.cfg.keys)))
                    .collect();
                if rows.is_empty() {
                    ui.label(egui::RichText::new(t("Sin resultados")).color(pal.subtle));
                }
                let wide = ui.available_width() >= 520.0;
                let bw = if wide { 170.0 } else { (ui.available_width() - 150.0).clamp(90.0, 170.0) };
                // una fila: atajo (botón para cambiarlo) + Quitar + Restablecer
                let mut controls = |ui: &mut egui::Ui, info: &'static commands::Info| {
                    let sc = self.shortcut_text(info.cmd);
                    if !info.rebind {
                        ui.label(egui::RichText::new(if sc.is_empty() { "—".to_string() } else { sc }).color(pal.subtle));
                        return;
                    }
                    let waiting = self.rebinding == Some(info.cmd);
                    let txt = if waiting {
                        t("Pulsa una combinación…").to_string()
                    } else if sc.is_empty() {
                        "—".to_string()
                    } else {
                        sc.clone()
                    };
                    if ui.add_sized([bw, 24.0], egui::Button::new(txt).selected(waiting)).clicked() {
                        start = Some(info.cmd);
                    }
                    if ui.add_enabled(!sc.is_empty(), egui::Button::new(t("Quitar")).small()).clicked() {
                        clear = Some(info.cmd);
                    }
                    let custom = self.cfg.keys.iter().any(|(k, _)| k == info.id);
                    if ui.add_enabled(custom, egui::Button::new(t("Restablecer")).small()).clicked() {
                        reset = Some(info.cmd);
                    }
                };
                if wide {
                    egui::Grid::new("keys").num_columns(2).spacing([18.0, 6.0]).show(ui, |ui| {
                        for info in &rows {
                            ui.label(t(info.label));
                            ui.horizontal(|ui| controls(ui, info));
                            ui.end_row();
                        }
                    });
                } else {
                    // pantalla estrecha: nombre encima y controles debajo
                    for info in &rows {
                        ui.label(t(info.label));
                        ui.horizontal(|ui| controls(ui, info));
                        ui.add_space(6.0);
                    }
                }
                ui.add_space(8.0);
                let hint = tf("Fijos: {} abre · clic central: carpeta en pestaña nueva · escribir filtra la lista", &[&commands::display("Enter")]);
                ui.add(egui::Label::new(egui::RichText::new(hint).color(pal.subtle).size(12.0)).wrap());
            },
            |ui| {
                if let Some(q) = &ask {
                    ui.add(egui::Label::new(q.as_str()).wrap());
                    ui.add_space(8.0);
                    (1u8, theme::DialogButtons::new(t("Cancelar"), t("Reasignar")).show(ui, &pal))
                } else if reset_all {
                    ui.add(egui::Label::new(t("¿Restablecer todos los atajos de teclado? Se perderán los que hayas cambiado.")).wrap());
                    ui.add_space(8.0);
                    (2, theme::DialogButtons::new(t("Cancelar"), t("Restablecer todos")).danger().show(ui, &pal))
                } else {
                    (0, theme::DialogButtons::close_only(t("Cerrar")).extras(&[t("Restablecer todos")]).show(ui, &pal))
                }
            },
        );
        if let Some(c) = start {
            self.rebinding = Some(c);
            self.h1.keys_note = None;
            self.h1.keys_ask = None;
        }
        if let Some(c) = clear {
            self.h1.keys_note = None;
            self.set_binding(c, None);
        }
        if let Some(c) = reset {
            let d = commands::info(c).default;
            self.request_binding(c, if d.is_empty() { None } else { Some(d.to_string()) });
        }
        match act {
            Some((1, theme::DialogAction::Primary)) => {
                if let Some((c, s, _)) = self.h1.keys_ask.take() {
                    self.set_binding(c, Some(s));
                }
            }
            Some((1, theme::DialogAction::Cancel)) => {
                self.h1.keys_ask = None;
                open = true;
            }
            Some((2, theme::DialogAction::Primary)) => {
                self.h1.keys_reset_all = false;
                self.cfg.keys.clear();
                commands::set_user_keys(&self.cfg.keys);
                self.save_cfg();
            }
            Some((2, theme::DialogAction::Cancel)) => {
                self.h1.keys_reset_all = false;
                open = true;
            }
            Some((0, theme::DialogAction::Extra(0))) => {
                self.h1.keys_reset_all = true;
                self.h1.keys_ask = None;
            }
            Some((0, theme::DialogAction::Cancel)) => open = false,
            _ => {}
        }
        if !open {
            self.rebinding = None;
            self.h1.keys_ask = None;
            self.h1.keys_reset_all = false;
            self.h1.keys_note = None;
            self.h1.keys_query.clear();
        }
        open
    }

    // ---- bienvenida ----

    /// Tarjeta del primer arranque: idioma, tema y si se indexan los nombres de archivo. Una sola
    /// pantalla; «Omitir» deja todo como está y el índice apagado. No vuelve a salir.
    pub(super) fn welcome_dialog(&mut self, ctx: &egui::Context) -> bool {
        let pal = self.pal;
        let mut open = true;
        let mut lang_change: Option<String> = None;
        let mut theme_change: Option<u8> = None;
        let act = theme::card_window_with_footer(
            ctx,
            &pal,
            t("Te damos la bienvenida"),
            460.0,
            &mut open,
            |ui| {
                ui.add(egui::Label::new(egui::RichText::new(t("Elige cómo quieres empezar. Todo se puede cambiar después en Opciones.")).color(pal.subtle)).wrap());
                ui.add_space(4.0);
                let lbl = theme::section_r(ui, &pal, t("Idioma")).id;
                let auto = self.cfg.lang == "auto";
                let cur = crate::i18n::current();
                let shown = if auto { t("Usar el idioma del sistema") } else { crate::i18n::LANGS[cur].1 };
                let _ = egui::ComboBox::from_id_salt("welcome_lang").selected_text(shown).width(ui.available_width().min(260.0)).show_ui(ui, |ui| {
                    if ui.selectable_label(auto, t("Usar el idioma del sistema")).clicked() {
                        lang_change = Some("auto".into());
                    }
                    for (i, (code, name)) in crate::i18n::LANGS.iter().enumerate() {
                        if ui.selectable_label(!auto && i == cur, *name).clicked() {
                            lang_change = Some(code.to_string());
                        }
                    }
                }).response.labelled_by(lbl);
                theme::section(ui, &pal, t("Tema"));
                ui.horizontal_wrapped(|ui| {
                    for (m, l) in [(0u8, t("Sistema")), (1, t("Claro")), (2, t("Oscuro"))] {
                        if ui.selectable_label(self.cfg.theme_mode == m, l).clicked() {
                            theme_change = Some(m);
                        }
                    }
                });
                // en la web no hay índice (en Android sí)
                if !crate::vfs::WEB {
                    theme::section(ui, &pal, t("Búsqueda"));
                    ui.checkbox(&mut self.h1.welcome_index, t("Búsqueda instantánea: indexa los nombres de tus archivos en este equipo"));
                    ui.add(egui::Label::new(egui::RichText::new(t("El índice sólo guarda nombres y rutas, no sale de este equipo y se puede apagar en Opciones.")).color(pal.subtle).size(12.0)).wrap());
                }
            },
            |ui| theme::DialogButtons::new(t("Omitir"), t("Empezar")).show(ui, &pal),
        );
        if let Some(l) = lang_change {
            self.cfg.lang = l;
            let li = crate::i18n::resolve(&self.cfg.lang);
            crate::i18n::set(li);
            crate::i18n::install_fonts(ctx, li);
            self.save_cfg();
        }
        if let Some(m) = theme_change {
            self.set_theme_mode(m);
        }
        let done = match act {
            Some(theme::DialogAction::Primary) => {
                if !crate::vfs::WEB && self.h1.welcome_index {
                    self.cfg.index_enabled = true;
                    index::start(tools::all_roots(), ctx.clone());
                }
                true
            }
            Some(theme::DialogAction::Cancel) => true,
            _ => !open,
        };
        if done {
            self.cfg.first_run = false;
            self.save_cfg();
            return false;
        }
        true
    }

    // ---- papelera ----

    /// Confirmación antes de mover a la papelera (sólo con «Confirmar al mover a la papelera»).
    pub(super) fn trash_ask_dialog(&mut self, ctx: &egui::Context, v: &[PathBuf]) -> bool {
        let pal = self.pal;
        let mut decision: Option<bool> = None;
        let text = match v {
            [one] => tf("«{}» se moverá a la papelera.", &[&one.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| one.display().to_string())]),
            _ => crate::i18n::tn("Se moverá {} elemento a la papelera.|Se moverán {} elementos a la papelera.", v.len(), &[]),
        };
        theme::modal_card(
            ctx,
            &pal,
            theme::Tone::Warn,
            t("¿Mover a la papelera?"),
            400.0,
            |ui| {
                ui.add(egui::Label::new(egui::RichText::new(text).color(pal.subtle)).wrap());
            },
            |ui| {
                if theme::dialog_button(ui, &pal, theme::Btn::Primary, t("Mover a la papelera")).clicked() {
                    decision = Some(true);
                }
                if theme::dialog_button(ui, &pal, theme::Btn::Plain, t("Cancelar")).clicked() {
                    decision = Some(false);
                }
            },
        );
        // teclado: nunca pisa lo que ya decidió un botón; Intro sólo sin foco en un botón (si no, lo
        // pulsa a él) y no en el fotograma en que se abre el diálogo
        if decision.is_none() {
            if theme::dialog_esc(ctx) && ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)) {
                decision = Some(false);
            } else if !theme::dialog_fresh(ctx, egui::Id::new("trash_ask_dialog")) && ctx.memory(|m| m.focused()).is_none() && ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Enter)) {
                decision = Some(true);
            }
        }
        match decision {
            Some(true) => {
                self.start_op(Op::Trash(v.to_vec()), OpMeta::Trash { replay: false });
                false
            }
            Some(false) => false,
            None => true,
        }
    }

    // ---- Opciones ----

    /// Filas nuevas de «Comportamiento» en Opciones. Devuelve si algo cambió (quien llama guarda).
    pub(super) fn h1_opts_behaviour(&mut self, ui: &mut egui::Ui) -> bool {
        let mut changed = ui.checkbox(&mut self.cfg.confirm_trash, t("Confirmar al mover a la papelera")).changed();
        let names = [t("Recordar la última"), t("Cuadrícula"), t("Lista"), t("Vista de columnas")];
        ui.horizontal(|ui| {
            let lbl = ui.label(t("Vista inicial")).id;
            let cur = self.cfg.start_view.min(3) as usize;
            let _ = egui::ComboBox::from_id_salt("start_view").selected_text(names[cur]).show_ui(ui, |ui| {
                for (i, n) in names.iter().enumerate() {
                    if ui.selectable_label(i == cur, *n).clicked() {
                        self.cfg.start_view = i as u8;
                        changed = true;
                    }
                }
            }).response.labelled_by(lbl);
        });
        changed
    }

    /// Filas nuevas de «Vista» en Opciones: escala de la interfaz, animaciones y botón de menú.
    pub(super) fn h1_opts_view(&mut self, ui: &mut egui::Ui) -> bool {
        let mut changed = false;
        let mut pct = self.h1.scale_edit.unwrap_or(self.cfg.ui_scale) * 100.0;
        let mut apply: Option<f32> = None;
        ui.horizontal(|ui| {
            let lbl = ui.label(t("Escala de la interfaz")).id;
            let r = ui.add(egui::Slider::new(&mut pct, 75.0..=200.0).step_by(5.0).suffix(" %")).labelled_by(lbl);
            if r.changed() {
                self.h1.scale_edit = Some(pct / 100.0);
            }
            // se aplica al soltar: cambiar la escala mientras se arrastra movería el deslizador bajo el ratón
            if let Some(v) = self.h1.scale_edit {
                if !r.dragged() {
                    apply = Some(v);
                }
            }
            let reset = format!("100 %{}", Some(commands::shortcut_text(Cmd::ZoomReset)).filter(|s| !s.is_empty()).map(|s| format!(" ({s})")).unwrap_or_default());
            if ui.add_enabled((self.cfg.ui_scale - 1.0).abs() > 0.001, egui::Button::new(reset).small()).clicked() {
                apply = Some(1.0);
            }
        });
        if let Some(v) = apply {
            let v = clamp_scale(v);
            self.h1.scale_edit = None;
            if (v - self.cfg.ui_scale).abs() > 0.001 {
                self.cfg.ui_scale = v;
                self.ctx.set_zoom_factor(v);
                changed = true;
            }
        }
        let names = [t("Según el sistema"), t("Sí"), t("No")];
        ui.horizontal(|ui| {
            let lbl = ui.label(t("Reducir animaciones")).id;
            let cur = self.cfg.reduce_motion.min(2) as usize;
            let _ = egui::ComboBox::from_id_salt("reduce_motion").selected_text(names[cur]).show_ui(ui, |ui| {
                for (i, n) in names.iter().enumerate() {
                    if ui.selectable_label(i == cur, *n).clicked() {
                        self.cfg.reduce_motion = i as u8;
                        changed = true;
                    }
                }
            }).response.labelled_by(lbl);
        });
        if changed {
            self.apply_motion();
        }
        if !self.phone {
            changed |= ui.checkbox(&mut self.cfg.show_menu_btn, t("Mostrar botón de menú (hamburguesa)")).changed();
        }
        changed
    }

    // ---- autotest (FB_DLG) ----

    /// Abre los diálogos de este módulo con contenido de ejemplo para las capturas automáticas.
    #[cfg(feature = "selftest")]
    pub(super) fn h1_selftest(&mut self, name: &str) {
        match name {
            "welcome" => self.dlg = Dlg::Welcome,
            "keys_ask" => {
                // «Carpeta superior» quiere Alt+⏴, que es de «Atrás»
                self.h1.keys_ask = Some((Cmd::Up, "Alt+ArrowLeft".into(), Cmd::Back));
                self.dlg = Dlg::Keys;
            }
            "keys_reset" => {
                self.h1.keys_reset_all = true;
                self.dlg = Dlg::Keys;
            }
            "keys_find" => {
                self.h1.keys_query = "ctrl+t".into();
                self.dlg = Dlg::Keys;
            }
            "trash_ask" => {
                let mut v: Vec<PathBuf> = self.order.iter().take(3).filter_map(|&i| self.entries.get(i as usize)).map(|e| self.cwd.join(&*e.name)).collect();
                if v.is_empty() {
                    v.push(self.cwd.join("ejemplo.txt"));
                }
                self.dlg = Dlg::TrashAsk(v);
            }
            _ => {}
        }
    }
}
