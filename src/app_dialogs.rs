//! Pila de tareas, superposición de progreso y diálogos modales.
use super::*;

impl App {
    // ---- diálogos ----

    /// Pila de tareas (escritorio): la operación en curso y las que esperan en la cola.
    fn tasks_popup(&mut self, ctx: &egui::Context) {
        let Some(op) = self.op.clone() else {
            self.tasks_open = false;
            return;
        };
        ctx.request_repaint_after(Duration::from_millis(120));
        if !self.tasks_open {
            return;
        }
        if ctx.input(|i| i.key_pressed(Key::Escape)) {
            self.tasks_open = false;
            return;
        }
        let pal = self.pal;
        let elapsed = self.op_started.elapsed().as_secs_f64();
        let total = op.total.load(Ordering::Relaxed).max(1);
        let done = op.done.load(Ordering::Relaxed).min(total);
        let label = crate::thumbs::lock(&op.label).clone();
        let scanning = label == t("Calculando…");
        let bytes = matches!(self.op_meta, OpMeta::Copy { .. } | OpMeta::Replace { .. } | OpMeta::MoveBack { .. } | OpMeta::Generated);
        let detail = if scanning {
            String::new()
        } else if bytes {
            let speed = done as f64 / elapsed.max(0.001);
            let mut d = format!("{} / {}  ·  {}/s", fsys::human_size(done), fsys::human_size(total), fsys::human_size(speed as u64));
            if speed > 1.0 && done > 0 {
                let eta = ((total - done) as f64 / speed) as u64;
                let hms = if eta >= 3600 { format!("{}:{:02}:{:02}", eta / 3600, eta / 60 % 60, eta % 60) } else { format!("{}:{:02}", eta / 60, eta % 60) };
                d.push_str(&format!("  ·  {}", tf("quedan {}", &[&hms])));
            }
            d
        } else {
            tf("{} de {}", &[&done, &total])
        };
        let ppp = ctx.pixels_per_point();
        let inset_b = self.insets[3] / ppp;
        let width = (ctx.content_rect().width() - 28.0).clamp(240.0, 400.0);
        let mut cancel_cur = false;
        let mut drop_q: Option<usize> = None;
        let mut close = false;
        let desc = self.op_desc.clone();
        let queued: Vec<String> = self.queue.iter().map(|(o, _)| op_desc(o)).collect();
        let area = egui::Area::new(egui::Id::new("tasks_popup"))
            .order(egui::Order::Foreground)
            .anchor(Align2::RIGHT_BOTTOM, vec2(-14.0, -(36.0 + inset_b)))
            .show(ctx, |ui| {
                egui::Frame::new()
                    .fill(pal.panel)
                    .stroke(Stroke::new(1.0, pal.border))
                    .corner_radius(CornerRadius::same(12))
                    .inner_margin(egui::Margin::same(12))
                    .shadow(egui::epaint::Shadow { offset: [0, 4], blur: 16, spread: 0, color: Color32::from_black_alpha(90) })
                    .show(ui, |ui| {
                        ui.set_width(width);
                        // cabecera de la familia de diálogos: título y cerrar (= ocultar; las tareas siguen)
                        let mut open = true;
                        theme::dialog_header(ui, &pal, t("Tareas en curso"), Some(&mut open));
                        close |= !open;
                        ui.add_space(6.0);
                        ui.horizontal(|ui| {
                            ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                                if ui.add(egui::Button::new(t("Cancelar")).min_size(vec2(0.0, theme::min_target()))).clicked() {
                                    cancel_cur = true;
                                }
                                ui.with_layout(egui::Layout::left_to_right(Align::Center), |ui| {
                                    ui.add(egui::Label::new(egui::RichText::new(&desc).strong()).truncate());
                                });
                            });
                        });
                        if scanning {
                            ui.add(egui::ProgressBar::new(0.0).desired_width(width).text(t("Calculando…")).animate(true).corner_radius(6));
                        } else {
                            ui.add(egui::ProgressBar::new(done as f32 / total as f32).desired_width(width).show_percentage().corner_radius(6));
                            ui.add_space(4.0);
                            ui.label(egui::RichText::new(detail).size(12.0).color(pal.subtle));
                            if !label.is_empty() {
                                ui.add(egui::Label::new(egui::RichText::new(label).size(12.0).color(pal.text)).truncate());
                            }
                        }
                        if !queued.is_empty() {
                            ui.add_space(8.0);
                            ui.separator();
                            ui.label(egui::RichText::new(t("En cola")).size(12.0).color(pal.subtle));
                            for (i, q) in queued.iter().enumerate() {
                                ui.horizontal(|ui| {
                                    ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                                        if ui.add(egui::Button::new(t("Cancelar")).min_size(vec2(0.0, theme::min_target()))).clicked() {
                                            drop_q = Some(i);
                                        }
                                        ui.with_layout(egui::Layout::left_to_right(Align::Center), |ui| {
                                            ui.add(egui::Label::new(egui::RichText::new(format!("{}.  {q}", i + 1)).size(12.5)).truncate());
                                        });
                                    });
                                });
                            }
                        }
                    });
            });
        // clic fuera de la pila: se cierra (el botón de la barra la alterna por su cuenta)
        let outside = ctx.input(|i| i.pointer.primary_clicked())
            && ctx.input(|i| i.pointer.interact_pos()).map(|p| !area.response.rect.contains(p) && p.y < ctx.content_rect().bottom() - 34.0).unwrap_or(false);
        if close || outside {
            self.tasks_open = false;
        }
        if let Some(i) = drop_q {
            self.queue.remove(i);
        }
        if cancel_cur {
            op.cancel.store(true, Ordering::Relaxed);
        }
    }

    /// Panel flotante con el progreso de la operación en curso (copiar, mover, borrar…).
    pub(super) fn op_overlay(&mut self, ctx: &egui::Context) {
        if !self.phone {
            return self.tasks_popup(ctx);
        }
        let Some(op) = self.op.clone() else {
            self.op_card_hidden = false;
            return;
        };
        ctx.request_repaint_after(Duration::from_millis(120));
        if self.op_card_hidden {
            return;
        }
        let elapsed = self.op_started.elapsed().as_secs_f64();
        if elapsed < 0.4 {
            return; // las operaciones rápidas no necesitan panel
        }
        let pal = self.pal;
        let bytes = matches!(self.op_meta, OpMeta::Copy { .. } | OpMeta::Replace { .. } | OpMeta::MoveBack { .. } | OpMeta::Generated);
        let title = match self.op_meta {
            OpMeta::Copy { mv: false, .. } | OpMeta::Replace { mv: false, .. } => t("Copiando"),
            OpMeta::Copy { mv: true, .. } | OpMeta::Replace { mv: true, .. } | OpMeta::MoveBack { .. } => t("Moviendo"),
            OpMeta::Trash { .. } => t("Moviendo a la papelera"),
            _ => t("Trabajando…"),
        };
        let total = op.total.load(Ordering::Relaxed).max(1);
        let done = op.done.load(Ordering::Relaxed).min(total);
        let label = crate::thumbs::lock(&op.label).clone();
        let scanning = label == t("Calculando…");
        let frac = if scanning { 0.0 } else { done as f32 / total as f32 };
        let detail = if scanning {
            String::new()
        } else if bytes {
            let speed = done as f64 / elapsed.max(0.001);
            let mut d = format!("{} / {}  ·  {}/s", fsys::human_size(done), fsys::human_size(total), fsys::human_size(speed as u64));
            if speed > 1.0 && done > 0 {
                let eta = ((total - done) as f64 / speed) as u64;
                let hms = if eta >= 3600 { format!("{}:{:02}:{:02}", eta / 3600, eta / 60 % 60, eta % 60) } else { format!("{}:{:02}", eta / 60, eta % 60) };
                d.push_str(&format!("  ·  {}", tf("quedan {}", &[&hms])));
            }
            d
        } else {
            tf("{} de {}", &[&done, &total])
        };
        let ppp = ctx.pixels_per_point();
        let inset_b = self.insets[3] / ppp;
        let width = (ctx.content_rect().width() - 28.0).clamp(220.0, 380.0);
        let mut cancel = false;
        let mut hide = false;
        egui::Area::new(egui::Id::new("op_overlay"))
            .order(egui::Order::Foreground)
            .anchor(Align2::RIGHT_BOTTOM, vec2(-14.0, -(42.0 + inset_b)))
            .show(ctx, |ui| {
                egui::Frame::new()
                    .fill(pal.panel)
                    .stroke(Stroke::new(1.0, pal.border))
                    .corner_radius(CornerRadius::same(12))
                    .inner_margin(egui::Margin::same(12))
                    .shadow(egui::epaint::Shadow { offset: [0, 4], blur: 16, spread: 0, color: Color32::from_black_alpha(90) })
                    .show(ui, |ui| {
                        ui.set_width(width);
                        ui.horizontal(|ui| {
                            ui.label(egui::RichText::new(title).strong());
                            if !self.queue.is_empty() {
                                ui.label(egui::RichText::new(tf("(+{} en cola)", &[&self.queue.len()])).size(12.0).color(pal.subtle));
                            }
                            ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                                // botones táctiles (≥ 44 pt) en el móvil
                                let bh = vec2(0.0, theme::MIN_TARGET_TOUCH);
                                if ui.add(egui::Button::new(t("Cancelar")).min_size(bh)).clicked() {
                                    cancel = true;
                                }
                                if ui.add(egui::Button::new(t("Ocultar")).min_size(bh)).clicked() {
                                    hide = true;
                                }
                            });
                        });
                        ui.add_space(6.0);
                        if scanning {
                            ui.add(egui::ProgressBar::new(0.0).desired_width(width).text(t("Calculando…")).animate(true).corner_radius(6));
                        } else {
                            ui.add(egui::ProgressBar::new(frac).desired_width(width).show_percentage().corner_radius(6));
                            ui.add_space(4.0);
                            ui.label(egui::RichText::new(detail).size(12.0).color(pal.subtle));
                            if !label.is_empty() {
                                ui.add(egui::Label::new(egui::RichText::new(label).size(12.0).color(pal.text)).truncate());
                            }
                        }
                    });
            });
        if hide {
            self.op_card_hidden = true;
        }
        if cancel {
            op.cancel.store(true, Ordering::Relaxed);
        }
    }

    pub(super) fn dialogs(&mut self, ctx: &egui::Context) {
        let pal = self.pal;
        self.tabs_sheet(ctx);
        if !matches!(self.dlg, Dlg::Batch(_)) && !self.batch_names.is_empty() {
            self.batch_names.clear();
        }
        // cerrar con tareas en curso: se pide confirmación
        // (también vaciar o restaurar la papelera, que va en su propio hilo)
        if ctx.input(|i| i.viewport().close_requested()) && !self.quit_ok && (self.op.is_some() || !self.queue.is_empty() || self.trash_job.is_some()) {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.quit_ask = true;
        }
        if self.quit_ask {
            let n = (self.queue.len() + self.op.is_some() as usize + self.trash_job.is_some() as usize).max(1);
            let mut decision: Option<bool> = None;
            theme::modal_card(
                ctx,
                &pal,
                theme::Tone::Warn,
                t("Hay tareas en curso"),
                420.0,
                |ui| {
                    ui.label(egui::RichText::new(crate::i18n::tn("Hay {} tarea sin terminar. Si cierras ahora se interrumpirá.|Hay {} tareas sin terminar. Si cierras ahora se interrumpirán.", n, &[])).color(pal.subtle));
                },
                |ui| {
                    if theme::dialog_button(ui, &pal, theme::Btn::Danger, t("Cerrar de todos modos")).clicked() {
                        decision = Some(true);
                    }
                    if theme::dialog_button(ui, &pal, theme::Btn::Plain, t("Seguir trabajando")).clicked() {
                        decision = Some(false);
                    }
                },
            );
            if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
                decision = Some(false);
            }
            match decision {
                Some(true) => {
                    self.quit_ask = false;
                    self.quit_ok = true;
                    self.save_cfg();
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
                Some(false) => self.quit_ask = false,
                None => {}
            }
        }
        let mut dlg = std::mem::replace(&mut self.dlg, Dlg::None);
        let mut keep = true;
        match &mut dlg {
            Dlg::Fav(d) => keep = self.j_dialog(ctx, d),
            Dlg::None => keep = false,
            Dlg::Props(d) => {
                let dirty = d.name != d.name_orig || d.time != d.time_orig || d.perm != d.perm_orig;
                let can = dirty && d.job.is_none();
                let buttons = theme::DialogButtons::new(t("Cerrar"), t("Aplicar")).primary_enabled(can);
                let act = app_h2::std_dialog(ctx, &pal, t("Propiedades"), 420.0, buttons, app_h2::enter_primary(can), |ui| {
                        ui.add(egui::Label::new(egui::RichText::new(&d.title).size(16.0).strong()).wrap());
                        ui.add_space(6.0);
                        let st = &d.stats;
                        let bytes = st.bytes.load(Ordering::Relaxed);
                        let done = st.done.load(Ordering::Acquire);
                        // ancho de los campos: lo que quede a la derecha de las etiquetas (pantallas estrechas)
                        let fw = (ui.available_width() - 120.0).clamp(120.0, 260.0);
                        egui::Grid::new("props").num_columns(2).spacing([16.0, 6.0]).show(ui, |ui| {
                            let row = |ui: &mut egui::Ui, k: &str, v: String| {
                                ui.label(egui::RichText::new(k).color(pal.subtle));
                                ui.add(egui::Label::new(v).wrap());
                                ui.end_row();
                            };
                            row(ui, t("Tipo"), d.kind.clone());
                            row(ui, t("Ubicación"), d.location.clone());
                            row(
                                ui,
                                t("Tamaño"),
                                tf("{} ({} bytes){}", &[&fsys::human_size(bytes), &bytes, &(if done { "" } else { t("  · calculando…") })]),
                            );
                            let (f, dr) = (st.files.load(Ordering::Relaxed), st.dirs.load(Ordering::Relaxed));
                            let dirs_inside = if d.paths.len() == 1 && d.kind == t("Carpeta") { dr.saturating_sub(1) } else { dr };
                            if d.paths.len() > 1 || d.kind == t("Carpeta") {
                                row(ui, t("Contenido"), format!("{}, {}", crate::i18n::tn("{} archivo|{} archivos", f, &[]), crate::i18n::tn("{} carpeta|{} carpetas", dirs_inside, &[])));
                            }
                            if d.mtime > 0 {
                                row(ui, t("Modificado"), fsys::fmt_time_full(d.mtime));
                            }
                            if !d.mode.is_empty() {
                                row(ui, t("Permisos"), d.mode.clone());
                            }
                        });
                        ui.add_space(8.0);
                        ui.separator();
                        theme::section(ui, &pal, t("Editar"));
                        // la copia de un servidor SFTP no se puede cambiar: el servidor no se enteraría
                        if d.paths.iter().any(|p| crate::sftp::is_remote(p)) {
                            ui.label(egui::RichText::new(t("Los servidores SFTP son de sólo lectura")).color(pal.subtle));
                            ui.disable();
                        }
                        egui::Grid::new("props_edit").num_columns(2).spacing([16.0, 8.0]).show(ui, |ui| {
                            if d.paths.len() == 1 {
                                ui.label(t("Nombre"));
                                ui.add(egui::TextEdit::singleline(&mut d.name).desired_width(fw));
                                ui.end_row();
                            }
                            ui.label(t("Modificado"));
                            ui.add(egui::TextEdit::singleline(&mut d.time).hint_text(t("AAAA-MM-DD HH:MM:SS")).desired_width(fw));
                            ui.end_row();
                            #[cfg(unix)]
                            {
                                ui.label(t("Permisos"));
                                egui::Grid::new("props_perm").num_columns(4).spacing([14.0, 4.0]).show(ui, |ui| {
                                    ui.label("");
                                    ui.label(egui::RichText::new(t("Lectura")).color(pal.subtle));
                                    ui.label(egui::RichText::new(t("Escritura")).color(pal.subtle));
                                    ui.label(egui::RichText::new(t("Ejecución")).color(pal.subtle));
                                    ui.end_row();
                                    for (who, sh) in [(t("Propietario"), 6u32), (t("Grupo"), 3), (t("Otros"), 0)] {
                                        ui.label(who);
                                        for (bit, what) in [(4u32, t("Lectura")), (2, t("Escritura")), (1, t("Ejecución"))] {
                                            let m = bit << sh;
                                            let mut on = d.perm & m != 0;
                                            let r = ui.checkbox(&mut on, "");
                                            // sin texto visible: el lector de pantalla oye «Propietario: Lectura»
                                            r.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Checkbox, true, on, format!("{who}: {what}")));
                                            if r.changed() {
                                                if on { d.perm |= m } else { d.perm &= !m }
                                            }
                                        }
                                        ui.end_row();
                                    }
                                });
                                ui.end_row();
                            }
                            #[cfg(not(unix))]
                            {
                                ui.label(t("Permisos"));
                                let mut ro = d.perm & 0o200 == 0;
                                if ui.checkbox(&mut ro, t("Sólo lectura")).changed() {
                                    d.perm = if ro { 0o444 } else { 0o666 };
                                }
                                ui.end_row();
                            }
                        });
                        if d.has_dir {
                            ui.add_space(4.0);
                            ui.checkbox(&mut d.recursive, t("Aplicar también al contenido"));
                        }
                        if let Some(job) = d.job.clone() {
                            ui.add_space(4.0);
                            ui.horizontal_wrapped(|ui| {
                                ui.spinner();
                                ui.label(tf("Aplicando cambios… {}", &[&job.done.load(Ordering::Relaxed)]));
                                if ui.button(t("Cancelar")).clicked() {
                                    job.cancel.store(true, Ordering::Relaxed);
                                }
                            });
                            ui.ctx().request_repaint_after(Duration::from_millis(200));
                        } else if !d.err.is_empty() {
                            ui.add_space(4.0);
                            ui.colored_label(pal.danger, &d.err);
                        } else if d.ok {
                            ui.add_space(4.0);
                            ui.colored_label(pal.accent, t("Cambios aplicados"));
                        }
                    });
                match act {
                    theme::DialogAction::Primary if can => self.apply_props(d),
                    theme::DialogAction::Cancel => keep = false,
                    _ => {}
                }
            }
            Dlg::NetLogin(l) => {
                let smb = l.uri.trim().starts_with("smb://") || l.uri.trim().is_empty();
                let can = !l.uri.trim().is_empty();
                let buttons = theme::DialogButtons::new(t("Cancelar"), t("Conectar")).primary_enabled(can);
                let act = app_h2::std_dialog(ctx, &pal, t("Conectar a un servidor"), 420.0, buttons, app_h2::enter_primary(can), |ui| {
                        let fw = (ui.available_width() - 110.0).clamp(120.0, 260.0);
                        egui::Grid::new("netlogin").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                            ui.label(t("Dirección"));
                            let r = ui.add(
                                egui::TextEdit::singleline(&mut l.uri).hint_text(t("smb://nas/recurso   sftp://servidor")).desired_width(fw),
                            );
                            if l.uri.is_empty() && !r.has_focus() && !r.lost_focus() {
                                r.request_focus();
                            }
                            ui.end_row();
                            ui.add_enabled_ui(!l.anon, |ui| {
                                ui.label(t("Usuario"));
                            });
                            ui.add_enabled(!l.anon, egui::TextEdit::singleline(&mut l.user).desired_width(fw));
                            ui.end_row();
                            if smb {
                                ui.add_enabled_ui(!l.anon, |ui| {
                                    ui.label(t("Dominio"));
                                });
                                ui.add_enabled(!l.anon, egui::TextEdit::singleline(&mut l.domain).desired_width(fw));
                                ui.end_row();
                            }
                            ui.add_enabled_ui(!l.anon, |ui| {
                                ui.label(t("Contraseña"));
                            });
                            ui.add_enabled(!l.anon, egui::TextEdit::singleline(&mut l.pass).password(true).desired_width(fw));
                            ui.end_row();
                        });
                        ui.checkbox(&mut l.anon, t("Acceso anónimo (invitado)"));
                    });
                if act == theme::DialogAction::Primary && !l.uri.trim().is_empty() {
                    let snapshot = NetLogin {
                        uri: l.uri.clone(),
                        user: l.user.clone(),
                        domain: l.domain.clone(),
                        pass: l.pass.clone(),
                        anon: l.anon,
                    };
                    self.start_mount(&snapshot);
                }
                keep = act == theme::DialogAction::None;
            }
            Dlg::Errors(list) => {
                let title = tf("Errores: {}", &[&list.len()]);
                let act = app_h2::std_dialog(ctx, &pal, &title, 480.0, theme::DialogButtons::close_only(t("Cerrar")), Some(theme::DialogAction::Cancel), |ui| {
                        for e in list.iter() {
                            ui.add(egui::Label::new(egui::RichText::new(e).size(12.5)).wrap());
                        }
                    });
                keep = act == theme::DialogAction::None;
            }
            Dlg::NetShares(d) => {
                let mut pick: Option<usize> = None;
                let host = d.base.split_once("://").map(|(_, r)| r.to_string()).unwrap_or_else(|| d.base.clone());
                let title = tf("Recursos de {}", &[&host]);
                let row_h = app_h2::touch_h(self.phone).max(32.0);
                let act = app_h2::std_dialog(ctx, &pal, &title, 380.0, theme::DialogButtons::close_only(t("Cancelar")), None, |ui| {
                        ui.add(egui::Label::new(egui::RichText::new(t("Elige el recurso que quieres montar")).color(pal.subtle)).wrap());
                        ui.add_space(6.0);
                        for (i, s) in d.shares.iter().enumerate() {
                            if ui.add(egui::Button::new(s).min_size(vec2(ui.available_width(), row_h))).clicked() {
                                pick = Some(i);
                            }
                        }
                    });
                if let Some(i) = pick {
                    let l = NetLogin {
                        uri: format!("{}/{}", d.base, d.shares[i]),
                        user: d.login.user.clone(),
                        domain: d.login.domain.clone(),
                        pass: d.login.pass.clone(),
                        anon: d.login.anon,
                    };
                    self.start_mount(&l);
                }
                keep = pick.is_none() && act == theme::DialogAction::None;
            }
            Dlg::Rename(old, text) => {
                let ok = !text.trim().is_empty();
                let buttons = theme::DialogButtons::new(t("Cancelar"), t("Renombrar")).primary_enabled(ok);
                let act = app_h2::std_dialog(ctx, &pal, t("Renombrar"), 380.0, buttons, app_h2::enter_primary(ok), |ui| {
                        let r = ui.add(egui::TextEdit::singleline(text).desired_width(ui.available_width()));
                        if !r.has_focus() && !r.lost_focus() {
                            r.request_focus();
                        }
                    });
                if act == theme::DialogAction::Primary && !text.trim().is_empty() {
                    if let Some(idx) = self.entries.iter().position(|e| *e.name == **old) {
                        let new = text.clone();
                        self.commit_rename(idx as u32, new);
                    }
                }
                keep = act == theme::DialogAction::None;
            }
            Dlg::NewFile(name) => {
                // nombre no válido: se avisa en el diálogo, que sigue abierto con lo escrito
                let bad = !name.trim().is_empty() && bad_name(name.trim());
                let ok = !name.trim().is_empty() && !bad;
                let buttons = theme::DialogButtons::new(t("Cancelar"), t("Crear")).primary_enabled(ok);
                let act = app_h2::std_dialog(ctx, &pal, t("Nuevo archivo"), 380.0, buttons, app_h2::enter_primary(ok), |ui| {
                        let r = ui.add(egui::TextEdit::singleline(name).desired_width(ui.available_width()));
                        if !r.has_focus() && !r.lost_focus() {
                            r.request_focus();
                        }
                        if bad {
                            ui.label(egui::RichText::new(t("Nombre no válido")).color(pal.danger));
                        }
                    });
                let mut keep_bad = false;
                if act == theme::DialogAction::Primary {
                    let n = name.trim().to_string();
                    if bad_name(&n) {
                        self.say(t("Nombre no válido").into(), true);
                        keep_bad = true;
                    } else if self.remote_ro(&self.cwd.clone()) {
                    } else {
                        let cwd = self.cwd.clone();
                        self.say(t("Creando archivo…").into(), false);
                        self.spawn_io(
                            move || {
                                // create_new: si aparece un archivo con ese nombre entre medias, no se vacía
                                let mut p = fsys::unique_path(&cwd, &n);
                                for _ in 0..8 {
                                    match crate::vfs::fs::OpenOptions::new().write(true).create_new(true).open(&p) {
                                        Ok(_) => return Ok(p),
                                        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => p = fsys::unique_path(&cwd, &n),
                                        Err(e) => return Err(e),
                                    }
                                }
                                Err(std::io::Error::new(std::io::ErrorKind::AlreadyExists, crate::i18n::t("ya existe")))
                            },
                            |a, r| match r {
                                Ok(p) => {
                                    a.msg = None;
                                    a.push_undo(Undo::Created(vec![(p.clone(), false)]), true);
                                    if p.parent() == Some(a.cwd.as_path()) {
                                        a.pending_select = vec![p.file_name().unwrap().to_string_lossy().into_owned()];
                                        a.refresh();
                                    }
                                }
                                Err(e) => a.say(tf("No se pudo crear el archivo: {e}", &[&e]), true),
                            },
                        );
                    }
                }
                keep = act == theme::DialogAction::None || keep_bad;
            }
            Dlg::Batch(d) => {
                let re = if d.regex && !d.find.is_empty() {
                    if d.re_cache.as_ref().map(|c| c.0 != d.find).unwrap_or(true) {
                        let r = regex::RegexBuilder::new(&d.find).size_limit(1 << 20).build().ok();
                        d.re_cache = Some((d.find.clone(), r));
                    }
                    d.re_cache.as_ref().and_then(|c| c.1.clone())
                } else {
                    None
                };
                let bad_re = d.regex && !d.find.is_empty() && re.is_none();
                let new: Vec<String> = d.names.iter().enumerate().map(|(i, n)| batch_name(n, d, i, re.as_ref())).collect();
                let olds: HashSet<&str> = d.names.iter().map(|s| s.as_str()).collect();
                let mut seen: HashSet<&str> = HashSet::new();
                // por fila: ¿tiene problema? (vacío, repetido, con barra o ya existente)
                let mut bad: Vec<bool> = Vec::with_capacity(new.len());
                let (dir, exists) = (&d.dir, &mut d.exists);
                // con la carpeta ya listada se mira en la lista (sin tocar el disco en cada tecla)
                let fold = |s: &str| if cfg!(any(windows, target_os = "macos")) { s.to_lowercase() } else { s.to_string() };
                let use_list = self.cwd == *dir && self.vmode == VMode::Dir && !self.loading;
                let mut listed: Option<HashSet<String>> = None;
                let entries = &self.entries;
                for (o, n) in d.names.iter().zip(&new) {
                    // cambiar sólo mayúsculas no choca con el propio original (sistemas sin distinción)
                    let b = bad_name(n) || !seen.insert(n.as_str()) || (n != o && fold(n) != fold(o) && !olds.contains(n.as_str()) && *exists.entry(n.clone()).or_insert_with(|| {
                        if use_list {
                            listed.get_or_insert_with(|| entries.iter().map(|e| fold(&e.name)).collect()).contains(&fold(n))
                        } else {
                            dir.join(n).v_symlink_metadata().is_ok() && !crate::fsys::case_alias(&dir.join(o), &dir.join(n))
                        }
                    }));
                    bad.push(b);
                }
                let conflict = bad.iter().any(|b| *b) || bad_re;
                let changes = d.names.iter().zip(&new).filter(|(o, n)| o != n).count();
                let mut act = 0;
                let mut open = true;
                // nombres nuevos en la vista de carpeta mientras «Vista previa» esté activa
                self.batch_names.clear();
                if d.preview && self.cwd == d.dir {
                    for (o, n) in d.names.iter().zip(&new) {
                        if o != n {
                            self.batch_names.insert(o.clone(), n.clone());
                        }
                    }
                }
                let example = d.names.iter().zip(&new).find(|(o, n)| o != n).or_else(|| d.names.first().zip(new.first())).map(|(o, n)| format!("{o}  ->  {n}")).unwrap_or_default();
                let ex_bad = bad.iter().any(|b| *b);
                let rounded = self.transparent;
                let edges = |a: &Self, c: &egui::Context| a.resize_edges(c);
                // ventana aparte y movible, sin decoración del sistema, como la vista previa
                ctx.show_viewport_immediate(
                    egui::ViewportId::from_hash_of("batch_window"),
                    egui::ViewportBuilder::default()
                        .with_title(t("Renombrar en lote"))
                        .with_decorations(false)
                        .with_transparent(rounded)
                        .with_inner_size([560.0, 440.0])
                        .with_min_inner_size([400.0, 340.0]),
                    |ui, class| {
                        let emb = class == egui::ViewportClass::EmbeddedWindow;
                        let c = ui.ctx().clone();
                        // no anidar `memory` dentro de `input`: el bloqueo del contexto no es reentrante
                        if c.input(|i| i.viewport().close_requested()) || (c.input(|i| i.key_pressed(egui::Key::Escape)) && c.memory(|m| m.focused().is_none())) {
                            open = false;
                            return;
                        }
                        if let (false, Some(dir)) = (emb, edges(self, &c)) {
                            c.send_viewport_cmd(egui::ViewportCommand::BeginResize(dir));
                        }
                        let screen = c.content_rect();
                        let maxed = c.input(|i| i.viewport().maximized.unwrap_or(false));
                        let rad = if rounded && !maxed { egui::CornerRadius::same(14) } else { egui::CornerRadius::ZERO };
                        let p = c.layer_painter(egui::LayerId::background());
                        if !emb {
                        p.rect_filled(screen, rad, pal.panel);
                        p.rect_stroke(screen, rad, Stroke::new(1.0, pal.border), egui::StrokeKind::Inside);
                        // cabecera propia: arrastra la ventana, doble clic maximiza
                        let head = Rect::from_min_size(screen.min, vec2(screen.width(), 46.0));
                        let close_r = Rect::from_center_size(pos2(head.right() - 28.0, head.center().y), vec2(30.0, 30.0));
                        let hr = ui.interact(Rect::from_min_max(head.min, pos2(close_r.left() - 4.0, head.max.y)), egui::Id::new("batch_head"), egui::Sense::click_and_drag());
                        if hr.double_clicked() {
                            c.send_viewport_cmd(egui::ViewportCommand::Maximized(!maxed));
                        } else if hr.drag_started() {
                            c.send_viewport_cmd(egui::ViewportCommand::StartDrag);
                        }
                        let cr = ui.interact(close_r, egui::Id::new("batch_close"), egui::Sense::click());
                        if cr.hovered() {
                            p.rect_filled(close_r, egui::CornerRadius::same(9), pal.hover);
                        }
                        theme::draw_glyph(&p, close_r.center(), 14.0, theme::Glyph::Close, pal.text);
                        if cr.clicked() {
                            open = false;
                        }
                        p.text(pos2(head.left() + 18.0, head.center().y), egui::Align2::LEFT_CENTER, t("Renombrar en lote"), FontId::proportional(16.0), pal.text);
                        p.line_segment([pos2(head.left() + 14.0, head.bottom()), pos2(head.right() - 14.0, head.bottom())], Stroke::new(1.0, pal.border));
                        }
                        let fw = (screen.width() - 32.0 - 150.0).max(120.0);
                        egui::CentralPanel::default().frame(egui::Frame::new().inner_margin(egui::Margin { left: 16, right: 16, top: if emb { 8 } else { 56 }, bottom: 14 })).show(ui, |ui| {
                    if emb {
                        // incrustada (móvil, web) no hay cabecera propia: título y cerrar como los demás diálogos
                        theme::dialog_header(ui, &pal, t("Renombrar en lote"), Some(&mut open));
                        ui.add_space(6.0);
                    }
                    egui::Grid::new("batch").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                        ui.label(t("Plantilla"));
                        ui.add(egui::TextEdit::singleline(&mut d.template).hint_text("{nombre} {n:3} {fecha} {padre}").desired_width(fw));
                        ui.end_row();
                        ui.label(t("Buscar"));
                        ui.add(egui::TextEdit::singleline(&mut d.find).desired_width(fw));
                        ui.end_row();
                        ui.label(t("Reemplazar por"));
                        ui.add(egui::TextEdit::singleline(&mut d.repl).hint_text(if d.regex { "$1" } else { "" }).desired_width(fw));
                        ui.end_row();
                        ui.label("");
                        ui.checkbox(&mut d.regex, t("Expresión regular"));
                        ui.end_row();
                        ui.label(t("Prefijo"));
                        ui.add(egui::TextEdit::singleline(&mut d.prefix).desired_width(fw));
                        ui.end_row();
                        ui.label(t("Sufijo"));
                        ui.add(egui::TextEdit::singleline(&mut d.suffix).desired_width(fw));
                        ui.end_row();
                        ui.label(t("Numerar"));
                        ui.horizontal(|ui| {
                            ui.add_enabled(d.template.trim().is_empty(), egui::Checkbox::new(&mut d.number, t("añadir contador")));
                            ui.add_enabled(d.number || !d.template.trim().is_empty(), egui::DragValue::new(&mut d.start).range(0..=99999).prefix(t("desde ")));
                        });
                        ui.end_row();
                        ui.label(t("Mayúsculas"));
                        ui.horizontal_wrapped(|ui| {
                            ui.selectable_value(&mut d.case, 0, t("sin cambio"));
                            ui.selectable_value(&mut d.case, 1, t("minúsculas"));
                            ui.selectable_value(&mut d.case, 2, t("MAYÚSCULAS"));
                            ui.selectable_value(&mut d.case, 3, t("Tipo título"));
                        });
                        ui.end_row();
                    });
                    ui.add_space(6.0);
                    ui.add(egui::Label::new(egui::RichText::new(&example).size(12.5).color(if ex_bad { pal.danger } else { pal.subtle })).truncate());
                    if bad_re {
                        ui.colored_label(pal.danger, t("Expresión regular no válida."));
                    } else if conflict {
                        ui.colored_label(pal.danger, t("Hay nombres vacíos, repetidos o que ya existen."));
                    }
                    ui.add_space(6.0);
                    ui.checkbox(&mut d.preview, t("Vista previa"));
                    ui.add_space(8.0);
                    let ok = !conflict && changes > 0;
                    match app_h2::dialog_footer(ui, &pal, theme::DialogButtons::new(t("Cancelar"), t("Renombrar")).primary_enabled(ok), app_h2::enter_primary(ok)) {
                        theme::DialogAction::Primary if ok => act = 1,
                        theme::DialogAction::Cancel => act = 2,
                        _ => {}
                    }
                        });
                    },
                );
                if !open {
                    act = 2;
                }
                if act == 1 && self.remote_ro(&d.dir.clone()) {
                    act = 2;
                }
                if act == 1 {
                    let pairs: Vec<(String, String)> =
                        d.names.iter().cloned().zip(new.iter().cloned()).filter(|(o, n)| o != n).collect();
                    // fuera del hilo de la interfaz: en carpetas de red o con muchos nombres tarda
                    let dir = d.dir.clone();
                    let work = (dir.clone(), pairs.clone());
                    self.spawn_io(
                        move || batch_rename(&work.0, &work.1),
                        move |a, r| {
                            match r {
                                Err(e) => a.say(tf("Error al renombrar: {e}", &[&e]), true),
                                Ok(()) => {
                                    a.say(crate::i18n::tn("{} elemento renombrado|{} elementos renombrados", pairs.len(), &[]), false);
                                    let mv = pairs.iter().map(|(o, n)| (dir.join(o), dir.join(n))).collect();
                                    a.push_undo(Undo::Moved(mv), true);
                                }
                            }
                            if a.cwd == dir {
                                a.pending_select = pairs.into_iter().map(|(_, n)| n).collect();
                                a.refresh();
                            }
                        },
                    );
                }
                keep = act == 0;
                if !keep {
                    self.batch_names.clear();
                }
            }
            Dlg::Options => {
                let mut open = true;
                let mut changed = false;
                let mut res_sort = false;
                let mut install_sys = false;
                let width = theme::dialog_width(ctx, 540.0) - 8.0;
                let act = theme::card_window_with_footer(ctx, &pal, t("Opciones"), 540.0, &mut open, |ui| {
                        let mut tab = self.opts_tab;
                        theme::tab_bar(ui, &pal, &mut tab, &[t("General"), t("Columnas"), t("Temas")]);
                        self.opts_tab = tab;
                        ui.add_space(8.0);
                        if self.opts_tab == 2 {
                            changed |= self.themes_tab(ui, width);
                            return;
                        }
                        if self.opts_tab == 1 {
                            ui.add(egui::Label::new(egui::RichText::new(t("Elige qué columnas ver en la vista de lista y su orden. Si la ventana es estrecha, se ocultan las menos importantes.")).color(pal.subtle)).wrap());
                            ui.add_space(6.0);
                            changed |= self.columns_editor(ui);
                            return;
                        }
                        theme::section(ui, &pal, t("Comportamiento"));
                        let mut single = self.cfg.single_click.unwrap_or(cfg!(target_os = "android"));
                        if ui.checkbox(&mut single, t("Abrir con un solo clic o toque")).changed() {
                            self.cfg.single_click = Some(single);
                            changed = true;
                        }
                        changed |= ui.checkbox(&mut self.cfg.restore_tabs, t("Restaurar las pestañas al iniciar")).changed();
                        if ui.checkbox(&mut self.cfg.index_enabled, t("Búsqueda indexada (instantánea)")).changed() {
                            changed = true;
                            if self.cfg.index_enabled {
                                index::start(tools::all_roots(), ctx.clone());
                            } else {
                                index::disable();
                            }
                        }
                        if self.cfg.index_enabled {
                            ui.horizontal(|ui| {
                                let txt = match index::status() {
                                    index::Status::Off => String::new(),
                                    index::Status::Building(n) => tf("Indexando… {} elementos", &[&n]),
                                    index::Status::Ready(n, secs) => tf("{} elementos · actualizado hace {} min", &[&n, &(secs / 60)]),
                                };
                                ui.label(egui::RichText::new(txt).size(12.0).color(pal.subtle));
                                if ui.small_button(t("Reindexar")).clicked() {
                                    index::rebuild(tools::all_roots(), ctx.clone());
                                }
                            });
                        }
                        changed |= ui.checkbox(&mut self.cfg.preview_window, t("Abrir el visor en una ventana aparte")).changed();
                        changed |= ui.checkbox(&mut self.cfg.native_deco, t("Usar la barra de título del sistema (requiere reiniciar)")).changed();
                        changed |= self.h1_opts_behaviour(ui);
                        theme::section(ui, &pal, t("Vista"));
                        changed |= ui.checkbox(&mut self.cfg.sys_icons, t("Usar iconos del sistema")).changed();
                        changed |= ui.checkbox(&mut self.cfg.folder_icons_os, t("Leer iconos personalizados de carpetas")).changed();
                        changed |= ui.checkbox(&mut self.cfg.thumbs_on, t("Mostrar miniaturas de imágenes y vídeos")).changed();
                        let hid = ui.checkbox(&mut self.cfg.show_hidden, t("Mostrar archivos ocultos")).changed();
                        changed |= hid;
                        res_sort |= hid;
                        changed |= ui.checkbox(&mut self.cfg.show_sidebar, t("Mostrar barra lateral")).changed();
                        changed |= ui.checkbox(&mut self.cfg.clip_panel, t("Mostrar panel del portapapeles")).changed();
                        ui.horizontal(|ui| {
                            let lbl = ui.label(t("Tamaño de iconos")).id;
                            changed |= ui.add(egui::Slider::new(&mut self.cfg.zoom, 64.0..=200.0).show_value(false)).labelled_by(lbl).changed();
                        });
                        changed |= self.h1_opts_view(ui);
                        theme::section(ui, &pal, t("Sistema"));
                        if system::can_install() {
                            if ui.button(t("Usar como gestor de archivos del sistema")).clicked() {
                                install_sys = true;
                            }
                        }
                        ui.add_space(6.0);
                        let mut auto = self.cfg.lang == "auto";
                        let mut lang_change = None;
                        if ui.checkbox(&mut auto, t("Usar el idioma del sistema")).changed() {
                            lang_change = Some(if auto { "auto".to_string() } else { crate::i18n::LANGS[crate::i18n::current()].0.to_string() });
                        }
                        ui.add_enabled_ui(!auto, |ui| {
                            ui.horizontal(|ui| {
                                let lbl = ui.label(t("Idioma")).id;
                                let cur = crate::i18n::current();
                                let _ = egui::ComboBox::from_id_salt("lang_combo").selected_text(crate::i18n::LANGS[cur].1).show_ui(ui, |ui| {
                                    for (i, (code, name)) in crate::i18n::LANGS.iter().enumerate() {
                                        if ui.selectable_label(i == cur, *name).clicked() {
                                            lang_change = Some(code.to_string());
                                        }
                                    }
                                }).response.labelled_by(lbl);
                            });
                        });
                        changed |= self.date_format_row(ui);
                        if let Some(l) = lang_change {
                            self.cfg.lang = l;
                            let li = crate::i18n::resolve(&self.cfg.lang);
                            crate::i18n::set(li);
                            crate::i18n::install_fonts(ctx, li);
                            changed = true;
                        }
                        ui.add_space(6.0);
                        ui.label(t("Intérprete del terminal (vacío = el predeterminado)"));
                        changed |= ui
                            .add(egui::TextEdit::singleline(&mut self.cfg.shell).hint_text(crate::terminal::default_shell()).desired_width(width - 20.0))
                            .changed();
                        changed |= self.sys_term_option(ui, width);
                    }, |ui| theme::DialogButtons::close_only(t("Cerrar")).show(ui, &pal));
                if act == Some(theme::DialogAction::Cancel) {
                    open = false;
                }
                if res_sort {
                    self.spawn_sort();
                }
                if changed {
                    self.save_cfg();
                }
                if install_sys {
                    match system::install() {
                        Ok(_) => self.say(t("Registrado como gestor de archivos del sistema").into(), false),
                        Err(e) => self.say(format!("{}: {e}", t("No se pudo registrar")), true),
                    }
                }
                keep = open;
            }
            Dlg::About => {
                let act = app_h2::std_dialog(ctx, &pal, t("Acerca de"), 340.0, theme::DialogButtons::close_only(t("Cerrar")), Some(theme::DialogAction::Cancel), |ui| {
                        ui.label(egui::RichText::new("File Browser").size(22.0).strong());
                        ui.label(egui::RichText::new(tf("Versión {}", &[&env!("CARGO_PKG_VERSION")])).color(pal.subtle));
                        ui.add_space(8.0);
                        ui.add(egui::Label::new(t("Gestor de archivos ligero y rápido, escrito en Rust con egui.")).wrap());
                    });
                keep = act == theme::DialogAction::None;
            }
            Dlg::OpenWith(d) => {
                keep = self.open_with_dialog(ctx, d);
            }
            Dlg::Compress(d) => {
                let valid = !d.name.trim().is_empty() && (!d.protect || (!d.pw.is_empty() && d.pw == d.pw2));
                let buttons = theme::DialogButtons::new(t("Cancelar"), t("Comprimir")).primary_enabled(valid);
                // pie fijo fuera de la zona con desplazamiento: con «Proteger con contraseña» los botones siguen
                // a la vista aunque la ventana mida 640×420
                let act = app_h2::std_dialog(ctx, &pal, t("Comprimir"), 460.0, buttons, app_h2::enter_primary(valid), |ui| {
                    let width = ui.available_width();
                    theme::section(ui, &pal, t("Formato"));
                    let mut sel = match d.fmt {
                        tools::ArchFmt::Zip => 0u8,
                        tools::ArchFmt::TarGz => 1,
                        tools::ArchFmt::SevenZ => 2,
                    };
                    theme::tab_bar(ui, &pal, &mut sel, &["ZIP", "TAR.GZ", "7Z"]);
                    d.fmt = [tools::ArchFmt::Zip, tools::ArchFmt::TarGz, tools::ArchFmt::SevenZ][sel as usize];
                    if !d.fmt.can_encrypt() {
                        d.protect = false;
                    }
                    theme::section(ui, &pal, t("Nombre del archivo"));
                    ui.horizontal(|ui| {
                        let r = ui.add(egui::TextEdit::singleline(&mut d.name).desired_width((width - 80.0).max(80.0)));
                        if d.focus {
                            r.request_focus();
                            d.focus = false;
                        }
                        ui.label(egui::RichText::new(format!(".{}", d.fmt.ext())).color(pal.subtle));
                    });
                    let lbl = theme::section_r(ui, &pal, t("Nivel de compresión")).id;
                    let lv = [(0u32, t("Sin compresión")), (1, t("Rápida")), (6, t("Normal")), (9, t("Máxima"))];
                    let cur = lv.iter().find(|(l, _)| *l == d.level).map(|(_, n)| *n).unwrap_or("");
                    let _ = egui::ComboBox::from_id_salt("compress_level").selected_text(cur).width(200.0f32.min(width)).show_ui(ui, |ui| {
                        for (l, n) in lv {
                            ui.selectable_value(&mut d.level, l, n);
                        }
                    }).response.labelled_by(lbl);
                    theme::section(ui, &pal, t("Contraseña"));
                    ui.add_enabled_ui(d.fmt.can_encrypt(), |ui| {
                        ui.checkbox(&mut d.protect, t("Proteger con contraseña"));
                    });
                    if !d.fmt.can_encrypt() {
                        ui.label(egui::RichText::new(t("TAR.GZ no admite contraseña.")).size(12.0).color(pal.subtle));
                    }
                    if d.protect {
                        ui.add_space(4.0);
                        ui.add(egui::TextEdit::singleline(&mut d.pw).password(!d.show).hint_text(t("Contraseña")).desired_width(width - 8.0));
                        ui.add(egui::TextEdit::singleline(&mut d.pw2).password(!d.show).hint_text(t("Repetir contraseña")).desired_width(width - 8.0));
                        ui.checkbox(&mut d.show, t("Mostrar"));
                        if !d.pw2.is_empty() && d.pw != d.pw2 {
                            ui.label(egui::RichText::new(t("Las contraseñas no coinciden")).size(12.0).color(pal.danger));
                        }
                        ui.add(egui::Label::new(egui::RichText::new(t("Cifrado AES-256. Otros programas también lo abren.")).size(12.0).color(pal.subtle)).wrap());
                    }
                });
                let valid = !d.name.trim().is_empty() && (!d.protect || (!d.pw.is_empty() && d.pw == d.pw2));
                if act == theme::DialogAction::Primary && valid {
                    let clean: String = d.name.trim().chars().filter(|c| !matches!(c, '/' | '\\' | '\0')).collect();
                    let ext = format!(".{}", d.fmt.ext());
                    let stem = clean.strip_suffix(&ext).unwrap_or(&clean).to_string();
                    let out = self.cwd.join(format!("{stem}{ext}"));
                    let opts = tools::CompressOpts { fmt: d.fmt, level: d.level, password: if d.protect { Some(d.pw.clone()) } else { None } };
                    let srcs = d.srcs.clone();
                    self.start_op(Op::Compress { srcs, out, opts }, OpMeta::Generated);
                }
                keep = act == theme::DialogAction::None || (act == theme::DialogAction::Primary && !valid);
            }
            Dlg::ArchPass(d) => {
                let name = d.src.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                let ok = !d.pass.is_empty();
                let buttons = theme::DialogButtons::new(t("Cancelar"), t("Extraer")).primary_enabled(ok);
                let act = app_h2::std_dialog(ctx, &pal, t("Archivo protegido"), 400.0, buttons, app_h2::enter_primary(ok), |ui| {
                    ui.add(egui::Label::new(egui::RichText::new(tf("«{}» está protegido con contraseña.", &[&name])).color(pal.text)).wrap());
                    ui.add_space(8.0);
                    let r = ui.add(egui::TextEdit::singleline(&mut d.pass).password(!d.show).hint_text(t("Contraseña")).desired_width(ui.available_width() - 8.0));
                    if d.focus {
                        r.request_focus();
                        d.focus = false;
                    }
                    ui.checkbox(&mut d.show, t("Mostrar"));
                    if d.wrong {
                        ui.label(egui::RichText::new(t("Contraseña incorrecta")).size(12.0).color(pal.danger));
                    }
                });
                if act == theme::DialogAction::Primary && !d.pass.is_empty() {
                    let meta = std::mem::replace(&mut d.meta, OpMeta::Other);
                    let op = Op::Unzip { src: d.src.clone(), dst: d.dst.clone(), pass: Some(d.pass.clone()) };
                    self.start_op(op, meta);
                } else if act == theme::DialogAction::Cancel {
                    self.pending_select.clear();
                }
                keep = act == theme::DialogAction::None;
            }
            Dlg::Hash(d) => {
                let act = app_h2::std_dialog(ctx, &pal, t("Hash del archivo"), 520.0, theme::DialogButtons::close_only(t("Cerrar")), Some(theme::DialogAction::Cancel), |ui| {
                        ui.add(egui::Label::new(egui::RichText::new(d.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()).strong()).wrap());
                        ui.add_space(6.0);
                        if !d.job.finished.load(Ordering::Acquire) {
                            let tt = d.job.total.load(Ordering::Relaxed).max(1);
                            let dn = d.job.done.load(Ordering::Relaxed);
                            ui.add(egui::ProgressBar::new((dn as f32 / tt as f32).clamp(0.0, 1.0)).corner_radius(6));
                            return;
                        }
                        let res = crate::thumbs::lock(&d.job.result).clone();
                        match res {
                            Some(Ok(h)) => {
                                // nombre y botón de copiar en una línea, el valor debajo (cabe en pantallas estrechas)
                                for (n, v) in ["MD5", "SHA-1", "SHA-256"].iter().zip(h.iter()) {
                                    ui.horizontal(|ui| {
                                        ui.label(egui::RichText::new(*n).strong());
                                        ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                                            if theme::icon_button(ui, &pal, theme::Icon::Copy, t("Copiar")).clicked() {
                                                ui.ctx().copy_text(v.clone());
                                            }
                                        });
                                    });
                                    ui.add(egui::Label::new(egui::RichText::new(v).monospace().size(12.0)).wrap());
                                    ui.add_space(6.0);
                                }
                                ui.add_space(4.0);
                                ui.label(t("Comparar con:"));
                                ui.add(egui::TextEdit::singleline(&mut d.cmp).desired_width(ui.available_width() - 8.0));
                                let c = d.cmp.trim().to_lowercase();
                                if !c.is_empty() {
                                    if h.iter().any(|x| *x == c) {
                                        ui.label(egui::RichText::new(t("Coincide")).color(pal.accent));
                                    } else {
                                        ui.label(egui::RichText::new(t("No coincide")).color(pal.danger));
                                    }
                                }
                            }
                            Some(Err(e)) => {
                                ui.add(egui::Label::new(egui::RichText::new(e).color(pal.danger)).wrap());
                            }
                            None => {}
                        }
                    });
                keep = act == theme::DialogAction::None;
            }
            Dlg::Dups(d) => {
                let mut show: Option<PathBuf> = None;
                let mut trash: Option<PathBuf> = None;
                let bh = vec2(0.0, app_h2::touch_h(self.phone));
                let act = app_h2::std_dialog(ctx, &pal, t("Archivos duplicados"), 680.0, theme::DialogButtons::close_only(t("Cerrar")), Some(theme::DialogAction::Cancel), |ui| {
                        let done = d.job.finished.load(Ordering::Acquire);
                        let (groups, total): (Vec<(u64, Vec<PathBuf>)>, usize) = {
                            let g = crate::thumbs::lock(&d.job.groups);
                            (g.iter().take(300).cloned().collect(), g.len())
                        };
                        let scanned = d.job.scanned.load(Ordering::Relaxed);
                        let summary = format!(
                            "{} · {}{}",
                            crate::i18n::tn("{} grupo|{} grupos", total, &[]),
                            crate::i18n::tn("{} archivo revisado|{} archivos revisados", scanned, &[]),
                            if done { "" } else { t(" · buscando…") }
                        );
                        ui.label(egui::RichText::new(summary).size(12.0).color(pal.subtle));
                        ui.separator();
                        for (gi, (size, files)) in groups.iter().enumerate() {
                            let Some(first) = files.first() else { continue };
                            let title = crate::i18n::tn("{} archivo de {}|{} archivos de {}", files.len(), &[&files.len(), &fsys::human_size(*size)]);
                            egui::CollapsingHeader::new(format!("{title}  ·  {}", first.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()))
                                .id_salt(("dupg", gi))
                                .show(ui, |ui| {
                                    for f in files {
                                        ui.horizontal(|ui| {
                                            let rel = f.strip_prefix(&d.root).unwrap_or(f);
                                            // la ruta se recorta y deja sitio a los dos botones
                                            let w = (ui.available_width() - 190.0).max(60.0);
                                            ui.allocate_ui(vec2(w, 22.0), |ui| {
                                                ui.add(egui::Label::new(rel.display().to_string()).truncate());
                                            });
                                            ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                                                // sin papelera (Android, web) no se ofrece mover a ella
                                                if extras::trash_supported() && ui.add(egui::Button::new(t("Papelera")).min_size(bh)).clicked() {
                                                    trash = Some(f.clone());
                                                }
                                                if ui.add(egui::Button::new(t("Mostrar")).min_size(bh)).clicked() {
                                                    show = Some(f.clone());
                                                }
                                            });
                                        });
                                    }
                                });
                        }
                    });
                if let Some(f) = trash {
                    match crate::extras::trash_delete(&f) {
                        Ok(()) => {
                            let mut g = crate::thumbs::lock(&d.job.groups);
                            for grp in g.iter_mut() {
                                grp.1.retain(|x| *x != f);
                            }
                            g.retain(|grp| grp.1.len() > 1);
                            drop(g);
                            self.push_undo(Undo::Trashed(vec![f.clone()]), true);
                            self.ux_announce(crate::i18n::tn("{} elemento movido a la papelera|{} elementos movidos a la papelera", 1, &[]), true);
                            self.refresh();
                        }
                        Err(e) => self.say(format!("{} — {}: {e}", t("Mover a la papelera"), f.display()), true),
                    }
                }
                if let Some(f) = &show {
                    if let Some(parent) = f.parent() {
                        self.navigate(parent.to_path_buf());
                        self.pending_select = vec![f.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()];
                    }
                }
                keep = act == theme::DialogAction::None && show.is_none();
            }
            Dlg::Keys => keep = self.keys_dialog(ctx),
            Dlg::Welcome => keep = self.welcome_dialog(ctx),
            Dlg::TrashAsk(v) => keep = self.trash_ask_dialog(ctx, v),
        }
        if keep && matches!(self.dlg, Dlg::None) {
            self.dlg = dlg;
        }
    }
}
