//! Accesibilidad del área de archivos: foco del teclado (Tab / «Ir al otro panel»), contorno de la
//! selección y nodos para el lector de pantalla (AccessKit) de cada elemento visible.
//! Módulo hijo de `app` (`#[path]` en app.rs): accede a lo privado de `App`.
use super::*;

/// Estado propio dentro de `App` (campo `a11y`).
#[derive(Default)]
pub(super) struct A11y {
    /// id egui del área de archivos de cada panel (0 izquierda / 1 derecha), del último fotograma
    pub files_ids: [Option<egui::Id>; 2],
    /// campo de texto con el foco al acabar el fotograma anterior (egui suelta el foco con Esc antes de
    /// que lo veamos: así se sabe que ese Esc era del campo)
    pub te_prev: Option<egui::Id>,
    /// rectángulo del panel de vista previa lateral del fotograma anterior
    pub side_rect: Option<Rect>,
    /// Retroceso borró el filtro: no sube de carpeta hasta soltar la tecla
    pub bs_hold: bool,
}

/// id fijo del cuadro «Filtrar» de la barra de herramientas (Esc en él sí borra el filtro)
pub(super) fn filter_id() -> egui::Id {
    egui::Id::new("filter_edit")
}

/// Color del contorno de la selección y del cursor del teclado: el de foco, corregido para que tenga
/// ≥ 3:1 también contra el tinte de selección (dentro de la tarjeta) además de fondo, panel y superficie.
pub(super) fn sel_ring(p: &Pal) -> Color32 {
    let on = [p.bg, p.panel, p.surface, theme::over(p.sel, p.bg), theme::over(p.sel, p.surface)];
    theme::ensure_contrast_all(p.focus, &on, theme::MIN_UI_CONTRAST + 0.03)
}

/// ¿Tab (sin modificadores) cambia de panel? Sólo con la vista dividida, el foco en el área de archivos
/// (o en ningún control) y sin menú ni diálogo abierto; en cualquier otro caso Tab recorre los controles.
pub(super) fn tab_switches_pane(split: bool, files_focus: bool, popup_open: bool, modal: bool) -> bool {
    split && files_focus && !popup_open && !modal
}

/// Texto del lector de pantalla para un elemento: «nombre, tipo, tamaño, fecha».
pub(super) fn entry_label(e: &fsys::Entry, broken_link: bool) -> String {
    let kind = if broken_link { t("Enlace roto") } else { e.kind.label() };
    let mut s = format!("{}, {}", base_name(&e.name), kind);
    if !e.is_dir() {
        s.push_str(", ");
        s.push_str(&fsys::human_size(e.size));
    }
    if e.mtime != 0 {
        s.push_str(", ");
        s.push_str(&fsys::fmt_time(e.mtime));
    }
    s
}

/// ¿Hay un lector de pantalla (u otra tecnología de apoyo) conectado? Se pregunta por el nodo de un
/// control que ya existe, así que con AccessKit apagado no crea nada.
pub(super) fn ak_on(ctx: &egui::Context, existing: egui::Id) -> bool {
    ctx.accesskit_node_builder(existing, |_| ()).is_some()
}

impl App {
    /// ¿Está el foco del teclado en el área de archivos? (en ningún control también cuenta: es lo que
    /// queda tras hacer clic en la lista o pulsar Esc)
    pub(super) fn a11y_files_focused(&self, ctx: &egui::Context) -> bool {
        match ctx.memory(|m| m.focused()) {
            None => true,
            Some(id) => self.a11y.files_ids.contains(&Some(id)),
        }
    }

    /// Al acabar el fotograma: recuerda qué campo de texto tiene el foco.
    pub(super) fn a11y_end_frame(&mut self, ctx: &egui::Context) {
        self.a11y.te_prev = if ctx.text_edit_focused() { ctx.memory(|m| m.focused()) } else { None };
    }

    /// ¿Es este Esc de un campo de texto, de la paleta, del renombrado o de la barra de dirección?
    /// (entonces la lista no lo usa para borrar el filtro ni la selección)
    pub(super) fn a11y_esc_taken(&self) -> bool {
        self.rename.is_some()
            || self.addr_edit.is_some()
            || self.palette.is_some()
            || self.a11y.te_prev.is_some_and(|id| id != filter_id() && id != egui::Id::new("phone_filter_edit"))
    }

    /// «Ir al otro panel»: cambia de panel y deja el foco del teclado en su área de archivos.
    pub(super) fn a11y_other_pane(&mut self) {
        if self.other.is_none() {
            return;
        }
        let to = 1 - self.active_side;
        self.focus_side(to);
        let id = self.a11y.files_ids[to as usize];
        self.ctx.memory_mut(|m| match (m.focused(), id) {
            // si el foco estaba en un área de archivos (o en un botón), pasa a la del otro panel
            (Some(_), Some(id)) => m.request_focus(id),
            (Some(f), None) => m.surrender_focus(f),
            (None, _) => {}
        });
    }

    /// Con la vista dividida el área de archivos se queda el Tab (cambia de panel): Mayús+Tab sigue
    /// sacando el foco hacia el control anterior.
    pub(super) fn a11y_shift_tab(&mut self, ctx: &egui::Context) -> bool {
        let focused = ctx.memory(|m| m.focused());
        if self.other.is_none() || !focused.is_some_and(|f| self.a11y.files_ids.contains(&Some(f))) {
            return false;
        }
        let hit = ctx.input_mut(|i| consume_exact(i, Modifiers::SHIFT, Key::Tab));
        if hit {
            ctx.memory_mut(|m| m.move_focus(egui::FocusDirection::Previous));
        }
        hit
    }

    /// Tras pintar el área de archivos de este panel: recuerda su id, fija qué teclas no le quitan el
    /// foco (las flechas mueven el cursor; Tab, con la vista dividida, cambia de panel) y pinta el anillo
    /// de foco cuando se llegó a ella con el teclado.
    pub(super) fn a11y_files_area(&mut self, ui: &egui::Ui, resp: &egui::Response, clip: Rect) {
        let side = (self.live_side as usize).min(1);
        self.a11y.files_ids[side] = Some(resp.id);
        let split = self.other.is_some();
        resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Other, true, t("Lista de archivos")));
        if resp.has_focus() {
            ui.memory_mut(|m| {
                m.set_focus_lock_filter(resp.id, egui::EventFilter { tab: split, horizontal_arrows: true, vertical_arrows: true, escape: false })
            });
            if theme::focus_visible(ui.ctx()) && self.cursor.is_none() {
                ui.painter().rect_stroke(clip.shrink(1.0), CornerRadius::same(8), Stroke::new(2.0, self.pal.focus), egui::StrokeKind::Inside);
            }
        }
    }

    /// Cursor del teclado que se dibuja en este panel: sólo si se está usando el teclado (como
    /// `:focus-visible`) y es el panel activo.
    pub(super) fn a11y_kb_cursor(&self, ctx: &egui::Context) -> Option<u32> {
        let active = self.other.is_none() || self.live_side == self.active_side;
        if !active || self.touch || !theme::focus_visible(ctx) || !self.a11y_files_focused(ctx) {
            return None;
        }
        self.cursor
    }

    /// Nodo del lector de pantalla para un elemento visible. Sólo con AccessKit activo; el área sólo
    /// «pasa por encima» (`Sense::hover`): no cambia clics ni arrastres de la lista.
    pub(super) fn a11y_entry(&self, ui: &mut egui::Ui, on: bool, rect: Rect, pos: usize, e: &fsys::Entry, sel: bool) {
        if !on {
            return;
        }
        let r = ui.interact(rect, egui::Id::new(("fb_entry", self.live_side, pos)), Sense::hover());
        let broken = e.flags & fsys::F_LINK != 0 && self.ux.links.get(&self.cwd.join(&*e.name)) == Some(&true);
        r.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, true, sel, entry_label(e, broken)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contorno_de_seleccion_se_ve_en_todos_los_temas() {
        let mut pals = vec![theme::palette(false), theme::palette(true)];
        pals.extend(theme::presets().iter().map(theme::palette_of));
        for p in &pals {
            let c = sel_ring(p);
            for bg in [p.bg, p.panel, p.surface, theme::over(p.sel, p.bg), theme::over(p.sel, p.surface)] {
                let r = theme::contrast_ratio(c, bg);
                assert!(r >= theme::MIN_UI_CONTRAST, "contorno {c:?} sobre {bg:?}: {r}");
            }
            // el anillo de foco del tema ya cumple sobre los fondos lisos
            for bg in [p.bg, p.panel, p.surface, p.hover] {
                assert!(theme::contrast_ratio(p.focus, bg) >= theme::MIN_UI_CONTRAST);
            }
            // marca de selección (✓) sobre el acento y texto sobre el botón de peligro
            assert!(theme::contrast_ratio(p.on_accent, p.accent) >= theme::MIN_UI_CONTRAST);
            assert!(theme::contrast_ratio(p.on_danger, p.danger_fill) >= theme::MIN_TEXT_CONTRAST);
            // iconos inactivos y ámbar del árbol JSON/XML
            for bg in [p.bg, p.panel, p.surface] {
                assert!(theme::contrast_ratio(p.disabled, bg) >= theme::MIN_UI_CONTRAST);
                assert!(theme::contrast_ratio(theme::warn_color(p), bg) >= theme::MIN_TEXT_CONTRAST);
            }
        }
    }

    #[test]
    fn tab_cambia_de_panel_solo_en_la_lista_con_vista_dividida() {
        assert!(tab_switches_pane(true, true, false, false));
        // un solo panel: Tab recorre los controles
        assert!(!tab_switches_pane(false, true, false, false));
        // foco en un campo de texto, botón u otro control
        assert!(!tab_switches_pane(true, false, false, false));
        // menú o diálogo abierto
        assert!(!tab_switches_pane(true, true, true, false));
        assert!(!tab_switches_pane(true, true, false, true));
    }

    #[test]
    fn etiqueta_de_un_elemento() {
        let f = fsys::Entry { name: "docs/informe.pdf".into(), size: 2048, mtime: 0, kind: fsys::Kind::Doc, flags: 0 };
        let s = entry_label(&f, false);
        assert!(s.starts_with("informe.pdf, "), "{s}");
        assert!(s.contains(&fsys::human_size(2048)), "{s}");
        let d = fsys::Entry { name: "fotos".into(), size: 0, mtime: 0, kind: fsys::Kind::Folder, flags: fsys::F_DIR };
        assert_eq!(entry_label(&d, false), format!("fotos, {}", fsys::Kind::Folder.label()));
        assert!(entry_label(&f, true).contains(t("Enlace roto")));
    }
}
