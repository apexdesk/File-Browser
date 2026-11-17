//! Familia común de diálogos (cabecera, cuerpo con desplazamiento, botones al pie, Intro / Esc),
//! hoja de pestañas del móvil, fila «Formato de fecha» de Opciones y pruebas visuales (`FB_DLG`).
//! Módulo hijo de `app` (`#[path]` en app.rs): accede a lo privado de `App`.
use super::*;
use crate::i18n::tn;
use crate::theme::{DialogAction, DialogButtons};

/// Estado propio dentro de `App` (campo `h2`).
#[derive(Default)]
pub(super) struct H2 {
    /// hoja con las pestañas abiertas (pensada para el móvil, vale en todas partes)
    pub tabs_sheet: bool,
}

/// Pie estándar de un diálogo: la fila de botones (`DialogButtons`) más el teclado.
/// Intro hace `enter` (normalmente la acción principal si está habilitada; `None` = nada) y Esc cancela,
/// también con el foco en un campo de texto. La tecla del fotograma en que se abre el diálogo no cuenta.
pub(super) fn dialog_footer(ui: &mut egui::Ui, pal: &Pal, b: DialogButtons, enter: Option<DialogAction>) -> DialogAction {
    let ctx = ui.ctx().clone();
    let mut act = b.show(ui, pal);
    let id = ui.id().with("h2_dialog_keys");
    let frame = ctx.cumulative_frame_nr();
    let last = ctx.data(|d| d.get_temp::<u64>(id));
    ctx.data_mut(|d| d.insert_temp(id, frame));
    let fresh = !matches!(last, Some(l) if l.saturating_add(1) >= frame);
    if act != DialogAction::None || fresh {
        return act;
    }
    if theme::dialog_esc(&ctx) {
        act = DialogAction::Cancel;
    } else if let Some(a) = enter {
        // con el foco en un botón o una casilla, Intro ya los pulsa: sólo cuenta sin foco o desde un
        // campo de texto (que al pulsar Intro suelta el foco)
        let free = ctx.memory(|m| m.focused()).map(|f| egui::text_edit::TextEditState::load(&ctx, f).is_some()).unwrap_or(true);
        if free && ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Enter)) {
            act = a;
        }
    }
    act
}

/// Diálogo estándar: tarjeta centrada con cabecera (título + cerrar), cuerpo con desplazamiento que no pasa
/// del alto de la pantalla, ancho limitado a la pantalla (`theme::dialog_width`) y botones fijos al pie.
/// Cerrar con la X o Esc devuelve `Cancel`.
pub(super) fn std_dialog(
    ctx: &egui::Context,
    pal: &Pal,
    title: &str,
    width: f32,
    b: DialogButtons,
    enter: Option<DialogAction>,
    body: impl FnOnce(&mut egui::Ui),
) -> DialogAction {
    let mut open = true;
    let act = theme::card_window_with_footer(ctx, pal, title, width, &mut open, body, |ui| dialog_footer(ui, pal, b, enter)).unwrap_or(DialogAction::None);
    if act == DialogAction::None && !open { DialogAction::Cancel } else { act }
}

/// Intro = acción principal sólo si está habilitada.
pub(super) fn enter_primary(enabled: bool) -> Option<DialogAction> {
    enabled.then_some(DialogAction::Primary)
}

/// Alto mínimo de un botón táctil en la interfaz del móvil (en el escritorio, el de `theme`).
pub(super) fn touch_h(phone: bool) -> f32 {
    if phone { theme::MIN_TARGET_TOUCH } else { theme::min_target() }
}

impl App {
    /// Hoja con las pestañas abiertas: tocar una la activa, la X la cierra; «Nueva pestaña» abre otra aquí.
    pub(super) fn tabs_sheet(&mut self, ctx: &egui::Context) {
        if !self.h2.tabs_sheet {
            return;
        }
        let pal = self.pal;
        let n = self.tabs.len();
        let titles: Vec<String> = (0..n).map(|i| self.tab_title(i)).collect();
        let row_h = touch_h(self.phone).max(40.0);
        let mut pick: Option<usize> = None;
        let mut close: Option<usize> = None;
        let title = tn("{} pestaña abierta|{} pestañas abiertas", n, &[]);
        let act = std_dialog(ctx, &pal, &title, 420.0, DialogButtons::new(t("Cerrar"), t("Nueva pestaña")), None, |ui| {
            ui.spacing_mut().item_spacing.y = 4.0;
            for (i, tt) in titles.iter().enumerate() {
                let cur = i == self.cur_tab;
                let w = ui.available_width();
                let (rect, resp) = ui.allocate_exact_size(vec2(w, row_h), Sense::click());
                resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, true, cur, tt));
                let p = ui.painter();
                if cur {
                    p.rect_filled(rect, CornerRadius::same(10), pal.sel);
                    p.rect_stroke(rect, CornerRadius::same(10), Stroke::new(1.0, pal.focus), egui::StrokeKind::Inside);
                } else if resp.hovered() {
                    p.rect_filled(rect, CornerRadius::same(10), pal.hover);
                }
                theme::draw_glyph(p, pos2(rect.left() + 20.0, rect.center().y), 18.0, Glyph::Folder, if cur { pal.accent } else { pal.subtle });
                // la X a la derecha (sólo si queda más de una pestaña)
                let xr = Rect::from_center_size(pos2(rect.right() - row_h / 2.0, rect.center().y), vec2(row_h, row_h));
                let text_w = (rect.width() - 40.0 - if n > 1 { row_h } else { 8.0 }).max(20.0);
                let g = ui.painter().layout_no_wrap(tt.clone(), FontId::proportional(14.0), pal.text);
                let shown = if g.size().x > text_w {
                    let keep = ((text_w / g.size().x) * tt.chars().count() as f32) as usize;
                    tt.chars().take(keep.saturating_sub(1)).collect::<String>() + "…"
                } else {
                    tt.clone()
                };
                ui.painter().text(pos2(rect.left() + 38.0, rect.center().y), Align2::LEFT_CENTER, shown, FontId::proportional(14.0), pal.text);
                theme::focus_ring(ui, &pal, &resp, 10.0);
                if n > 1 {
                    let xresp = ui.interact(xr, ui.id().with(("tab_sheet_x", i)), Sense::click());
                    xresp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, t("Cerrar pestaña")));
                    let xresp = xresp.on_hover_text(t("Cerrar pestaña"));
                    if xresp.hovered() {
                        ui.painter().rect_filled(xr.shrink(4.0), CornerRadius::same(8), pal.hover);
                    }
                    theme::draw_glyph(ui.painter(), xr.center(), 14.0, Glyph::Close, if xresp.hovered() { pal.text } else { pal.subtle });
                    theme::focus_ring(ui, &pal, &xresp, 8.0);
                    if xresp.clicked() {
                        close = Some(i);
                    } else if resp.clicked() {
                        pick = Some(i);
                    }
                } else if resp.clicked() {
                    pick = Some(i);
                }
            }
        });
        if let Some(i) = close {
            self.close_tab(i);
        } else if let Some(i) = pick {
            self.switch_tab(i);
            self.h2.tabs_sheet = false;
        }
        match act {
            DialogAction::Primary => {
                self.new_tab(self.cwd.clone());
                self.h2.tabs_sheet = false;
            }
            DialogAction::Cancel => self.h2.tabs_sheet = false,
            _ => {}
        }
    }

    /// Fila «Formato de fecha» de Opciones (va tras el selector de idioma). Devuelve true si cambió.
    pub(super) fn date_format_row(&mut self, ui: &mut egui::Ui) -> bool {
        let cur = fsys::DateStyle::from_code(&self.cfg.date_fmt);
        let now = SystemTime::now().duration_since(web_time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
        let mut pick: Option<fsys::DateStyle> = None;
        ui.horizontal_wrapped(|ui| {
            let lbl = ui.label(t("Formato de fecha")).id;
            let _ = egui::ComboBox::from_id_salt("date_fmt_combo").selected_text(t(cur.label())).show_ui(ui, |ui| {
                for s in fsys::DateStyle::ALL {
                    // cada opción con un ejemplo (la fecha de ahora en ese formato)
                    let txt = format!("{}  ·  {}", t(s.label()), fsys::fmt_time_style(now, s));
                    if ui.selectable_label(s == cur, txt).clicked() {
                        pick = Some(s);
                    }
                }
            }).response.labelled_by(lbl);
        });
        match pick {
            Some(s) if s != cur => {
                self.cfg.date_fmt = s.code().to_string();
                fsys::set_date_style(s);
                // las celdas de fecha ya dibujadas se rehacen con el nuevo formato
                self.ctx.request_repaint();
                true
            }
            _ => false,
        }
    }

    /// Pruebas visuales de los diálogos de este módulo (`FB_DLG=<nombre>`).
    #[cfg(feature = "selftest")]
    pub(super) fn h2_selftest(&mut self, name: &str) {
        match name {
            "newfile" => self.dlg = Dlg::NewFile(t("Nuevo archivo.txt").into()),
            "netlogin" => {
                self.dlg = Dlg::NetLogin(NetLogin { uri: "smb://nas/media".into(), user: "ana".into(), domain: String::new(), pass: String::new(), anon: false })
            }
            "netshares" => {
                let login = NetLogin { uri: "smb://nas".into(), user: String::new(), domain: String::new(), pass: String::new(), anon: true };
                self.dlg = Dlg::NetShares(NetShares { base: "smb://nas".into(), shares: vec!["media".into(), "backup".into(), "fotos".into()], login });
            }
            "errors" => {
                self.dlg = Dlg::Errors(
                    (1..=14).map(|i| format!("/home/usuario/Documentos/archivo_{i}.txt: Permission denied (os error 13)")).collect(),
                )
            }
            "about" => self.dlg = Dlg::About,
            "quit" => {
                self.quit_ask = true;
            }
            "tasks" => {
                // copias de la carpeta actual a una carpeta temporal (para ver la pila hace falta que tarden:
                // con carpetas pequeñas, mejor FB_TASKS=<destino> sobre una carpeta grande)
                let dst = std::env::temp_dir().join(format!("fb-selftest-tasks-{}", std::process::id()));
                let _ = crate::vfs::fs::create_dir_all(&dst);
                let srcs: Vec<PathBuf> = self.entries.iter().map(|e| self.cwd.join(&*e.name)).collect();
                for _ in 0..3 {
                    self.start_op(Op::Copy { srcs: srcs.clone(), dst: dst.clone(), mv: false, policy: Policy::Rename }, OpMeta::Other);
                }
                self.tasks_open = true;
            }
            "phonetabs" => {
                let c = self.cwd.clone();
                if let Some(p) = c.parent().map(|p| p.to_path_buf()) {
                    self.new_tab(p);
                }
                self.new_tab(c);
                self.h2.tabs_sheet = true;
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::i18n::{index_of, tn_in, LANGS};

    /// Las filas con plural nuevas de los diálogos dan la forma correcta en los diez idiomas.
    #[test]
    fn plurales_de_dialogos() {
        let keys = [
            "{} pestaña abierta|{} pestañas abiertas",
            "{} grupo|{} grupos",
            "{} archivo revisado|{} archivos revisados",
            "Archivos abiertos recientemente · {} elemento|Archivos abiertos recientemente · {} elementos",
            "Papelera · {} elemento|Papelera · {} elementos",
        ];
        for k in keys {
            for l in 0..LANGS.len() {
                let one = tn_in(l, k, 1u64, &[]);
                let many = tn_in(l, k, 5u64, &[]);
                assert!(one.contains('1') && many.contains('5'), "{k} ({})", LANGS[l].0);
                assert!(!one.contains('|') && !many.contains('|'), "{k} ({})", LANGS[l].0);
                // la fila está en la tabla (en inglés siempre cambia)
                if LANGS[l].0 == "en" {
                    assert_ne!(tn_in(l, k, 5u64, &[]), tn_in(0, k, 5u64, &[]), "{k} sin traducir");
                }
            }
        }
        let es = index_of("es").unwrap();
        let ru = index_of("ru").unwrap();
        assert_eq!(tn_in(es, "{} grupo|{} grupos", 1u64, &[]), "1 grupo");
        assert_eq!(tn_in(es, "{} grupo|{} grupos", 2u64, &[]), "2 grupos");
        assert_eq!(tn_in(ru, "{} grupo|{} grupos", 2u64, &[]), "2 группы");
        assert_eq!(tn_in(ru, "{} grupo|{} grupos", 5u64, &[]), "5 групп");
        let en = index_of("en").unwrap();
        assert_eq!(tn_in(en, "{} archivo de {}|{} archivos de {}", 3u64, &[&3, &"1.0 KB"]), "3 files of 1.0 KB");
    }
}
