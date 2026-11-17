//! Vista rápida: abrir la vista previa, panel lateral y ventana aparte.
use super::*;

impl App {
    // -----------------------------------------------------------------------
    // Vista rápida
    // -----------------------------------------------------------------------

    pub(super) fn open_preview_at(&mut self, i: u32) {
        let Some(e) = self.entries.get(i as usize) else { return };
        let path = self.cwd.join(&*e.name);
        let pos = self.pos_of(i).map(|p| p + 1).unwrap_or(0);
        let label = tf("{} de {}", &[&pos, &self.order.len()]);
        let key = self.thumb_key(e);
        let thumb = if crate::thumbs::thumbable(e.kind, e.ext()) { self.thumbs.get(key).cloned() } else { None };
        // que no suenen a la vez el panel lateral y la vista rápida
        if let Some(s) = &mut self.side {
            s.pause_media();
        }
        self.preview = Some(Preview::open(
            &mut self.pworker,
            path,
            e.name.to_string(),
            e.kind,
            e.size,
            e.mtime,
            label,
            thumb,
        ));
    }

    /// Panel derecho con la vista previa de la selección actual (siempre visible mientras esté activo).
    pub(super) fn side_panel(&mut self, ui: &mut egui::Ui, pal: &Pal, bgfill: Color32, phone: bool) {
        let want = self.cfg.preview_panel && !phone && self.vmode != VMode::Trash && ui.ctx().content_rect().width() >= 700.0;
        if !want {
            self.side = None;
            return;
        }
        let target = self.cursor.filter(|&c| self.is_sel(c)).or_else(|| {
            let mut first = None;
            self.for_each_selected(|i| {
                first.get_or_insert(i);
            });
            first
        });
        let cur = target.and_then(|i| self.entries.get(i as usize)).map(|e| (self.cwd.join(&*e.name), e.mtime, e.size));
        // también se rehace si el archivo cambió en el disco (fecha o tamaño)
        let same = match (&self.side, &cur) {
            (Some(p), Some((c, m, z))) => &p.path == c && p.mtime == *m && p.size == *z,
            (None, None) => true,
            _ => false,
        };
        if !same {
            self.side = None;
            if let (Some(i), Some(_)) = (target, &cur) {
                let e = &self.entries[i as usize];
                let path = self.cwd.join(&*e.name);
                let pos = self.pos_of(i).map(|p| p + 1).unwrap_or(0);
                let label = tf("{} de {}", &[&pos, &self.order.len()]);
                let key = self.thumb_key(e);
                let thumb = if crate::thumbs::thumbable(e.kind, e.ext()) { self.thumbs.get(key).cloned() } else { None };
                self.side = Some(Preview::open(&mut self.sworker, path, e.name.to_string(), e.kind, e.size, e.mtime, label, thumb));
            }
        }
        let modal = self.preview.is_some() || !matches!(self.dlg, Dlg::None);
        let resp = egui::Panel::right("preview_panel")
            .frame(egui::Frame::new().fill(bgfill).inner_margin(egui::Margin { left: 4, right: 10, top: 0, bottom: 4 }))
            .default_size(self.cfg.preview_w)
            .size_range(220.0..=760.0)
            .resizable(true)
            .show_separator_line(false)
            .show(ui, |ui| {
                let r = ui.available_rect_before_wrap();
                let p = ui.painter();
                p.rect_filled(r, CornerRadius::same(14), pal.panel);
                p.rect_stroke(r, CornerRadius::same(14), Stroke::new(1.0, pal.border.gamma_multiply(0.6)), egui::StrokeKind::Inside);
                match &mut self.side {
                    Some(pv) => {
                        pv.show_panel(ui.ctx(), p, r, pal, !modal);
                    }
                    None => {
                        p.text(r.center(), Align2::CENTER_CENTER, t("Vista previa"), FontId::proportional(14.0), pal.subtle);
                    }
                }
                ui.allocate_rect(r, egui::Sense::hover());
            });
        // el ancho se recuerda al soltar el borde
        let w = resp.response.rect.width();
        if (w - self.cfg.preview_w).abs() > 1.0 && !ui.ctx().input(|i| i.pointer.any_down()) {
            self.cfg.preview_w = w;
            self.save_cfg();
        }
    }

    pub(super) fn toggle_preview(&mut self) {
        if self.vmode == VMode::Trash {
            return;
        }
        if self.preview.is_some() {
            self.preview = None;
            return;
        }
        let target = self.cursor.filter(|&c| self.is_sel(c)).or_else(|| {
            let mut first = None;
            self.for_each_selected(|i| {
                first.get_or_insert(i);
            });
            first
        });
        let target = target.or_else(|| self.order.first().copied());
        if let Some(i) = target {
            if self.sel_count == 0 {
                self.select_only(i);
            }
            self.open_preview_at(i);
        }
    }

    pub(super) fn preview_step(&mut self, delta: isize) {
        self.move_cursor(delta, false);
        if let Some(c) = self.cursor {
            self.open_preview_at(c);
        }
    }

    pub(super) fn thumb_key(&self, e: &Entry) -> u64 {
        let h = hash_str(self.dir_hash, &e.name);
        mix(mix(h, e.mtime as u64), e.size)
    }

    /// Teclado del visor (overlay o ventana aparte).
    pub(super) fn preview_keys(&mut self, ctx: &egui::Context) {
        let k = |m: Modifiers, key: Key| ctx.input_mut(|i| consume_exact(i, m, key));
        let (is_video, active) = self
            .preview
            .as_ref()
            .map(|p| (p.can_play(), p.player_active()))
            .unwrap_or((false, false));
        if k(Modifiers::NONE, Key::Escape) {
            let closed = self.preview.as_mut().map(|p| p.close_find() || p.close_inner()).unwrap_or(false);
            if !closed {
                self.preview = None;
            }
        } else if self.preview.as_ref().map(|p| p.typing()).unwrap_or(false) {
            // el cuadro de búsqueda del documento se queda con el teclado
        } else if k(Modifiers::NONE, Key::Space) {
            if is_video {
                if let Some(p) = &mut self.preview {
                    p.toggle_play(ctx);
                }
            } else {
                self.preview = None;
            }
        } else if k(Modifiers::NONE, Key::I) {
            if let Some(p) = &mut self.preview {
                p.toggle_meta(ctx);
            }
        } else if k(Modifiers::NONE, Key::ArrowDown) {
            // en texto, Markdown, tablas y árboles ↑/↓ desplazan; ←/→ cambian de archivo
            match self.preview.as_mut().filter(|p| p.scrollable()) {
                Some(p) => p.scroll_by(40.0),
                None => self.preview_step(1),
            }
        } else if k(Modifiers::NONE, Key::ArrowUp) {
            match self.preview.as_mut().filter(|p| p.scrollable()) {
                Some(p) => p.scroll_by(-40.0),
                None => self.preview_step(-1),
            }
        } else if k(Modifiers::NONE, Key::ArrowRight) {
            if active {
                if let Some(p) = &mut self.preview {
                    p.seek_by(5.0);
                }
            } else {
                self.preview_step(1);
            }
        } else if k(Modifiers::NONE, Key::ArrowLeft) {
            if active {
                if let Some(p) = &mut self.preview {
                    p.seek_by(-5.0);
                }
            } else {
                self.preview_step(-1);
            }
        } else if is_video && k(Modifiers::NONE, Key::Enter) {
            if let Some(p) = &mut self.preview {
                p.toggle_play(ctx);
            }
        } else if k(Modifiers::NONE, Key::Enter) || k(Modifiers::NONE, Key::O) {
            if let Some(p) = &self.preview {
                let path = p.path.clone();
                if p.kind == Kind::Folder {
                    self.preview = None;
                    self.navigate(path);
                } else {
                    self.open_ext(&path);
                }
            }
        }
    }

    /// Visor en ventana propia (viewport inmediato; se cierra solo al dejar de llamarlo).
    pub(super) fn show_preview_window(&mut self, ctx: &egui::Context) {
        let pal = self.pal;
        let title = self.preview.as_ref().map(|p| p.name.clone()).unwrap_or_default();
        let builder = egui::ViewportBuilder::default()
            .with_title(title)
            .with_decorations(false)
            .with_transparent(self.transparent)
            .with_inner_size([980.0, 700.0])
            .with_min_inner_size([380.0, 280.0]);
        let mut close = false;
        ctx.show_viewport_immediate(egui::ViewportId::from_hash_of("preview_window"), builder, |ui, _class| {
            let c = ui.ctx().clone();
            if c.input(|i| i.viewport().close_requested()) {
                close = true;
                return;
            }
            self.preview_keys(&c);
            if let Some(d) = self.resize_edges(&c) {
                // sin marco del sistema: bordes propios para redimensionar
                c.send_viewport_cmd(egui::ViewportCommand::BeginResize(d));
            }
            let rounded = self.transparent;
            if let Some(pv) = &mut self.preview {
                if matches!(pv.show(&c, &pal, Some(rounded)), preview::Action::Close) {
                    close = true;
                }
            }
        });
        if close {
            self.preview = None;
        }
    }
}
