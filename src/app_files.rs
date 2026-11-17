//! Zona principal: vista de columnas, cabecera y columnas de la lista, cuadrícula/lista virtualizada y menú contextual.
use super::*;

/// Columna opcional de la vista de lista. El orden de `COLS` es también la prioridad cuando no caben todas.
struct ColDef {
    id: &'static str,
    /// Etiqueta en el editor (clave de traducción).
    label: &'static str,
    /// Etiqueta en la cabecera (clave de traducción).
    head: &'static str,
    w: f32,
    right: bool,
    sort: Option<SortKey>,
    /// Necesita consultar el sistema de archivos además de lo que ya trae el listado.
    meta: bool,
    unix_only: bool,
}

const COLS: &[ColDef] = &[
    ColDef { id: "size", label: "Tamaño", head: "Tamaño", w: 80.0, right: true, sort: Some(SortKey::Size), meta: false, unix_only: false },
    ColDef { id: "date", label: "Fecha de modificación", head: "Modificado", w: 165.0, right: false, sort: Some(SortKey::Date), meta: false, unix_only: false },
    ColDef { id: "kind", label: "Tipo", head: "Tipo", w: 110.0, right: false, sort: Some(SortKey::Type), meta: false, unix_only: false },
    ColDef { id: "ext", label: "Extensión", head: "Extensión", w: 80.0, right: false, sort: None, meta: false, unix_only: false },
    ColDef { id: "perms", label: "Permisos", head: "Permisos", w: 92.0, right: false, sort: None, meta: true, unix_only: false },
    ColDef { id: "owner", label: "Propietario", head: "Propietario", w: 100.0, right: false, sort: None, meta: true, unix_only: true },
    ColDef { id: "group", label: "Grupo", head: "Grupo", w: 100.0, right: false, sort: None, meta: true, unix_only: true },
    ColDef { id: "items", label: "Elementos", head: "Elementos", w: 84.0, right: true, sort: None, meta: true, unix_only: false },
    ColDef { id: "created", label: "Fecha de creación", head: "Creado", w: 165.0, right: false, sort: None, meta: true, unix_only: false },
    ColDef { id: "accessed", label: "Último acceso", head: "Último acceso", w: 165.0, right: false, sort: None, meta: true, unix_only: false },
    ColDef { id: "mode", label: "Permisos (octal)", head: "Octal", w: 60.0, right: false, sort: None, meta: true, unix_only: false },
    ColDef { id: "link", label: "Destino del enlace", head: "Enlace a", w: 180.0, right: false, sort: None, meta: true, unix_only: false },
];

/// Columnas por defecto, de izquierda a derecha.
const DEFAULT_COLS: [&str; 3] = ["date", "size", "kind"];

/// Columna colocada: `right` es la distancia de su borde derecho al borde derecho de la fila.
struct PCol {
    def: &'static ColDef,
    right: f32,
    w: f32,
}

struct ListCols {
    cols: Vec<PCol>,
    /// Ancho reservado a la derecha del nombre.
    reserve: f32,
}

/// Columnas visibles según el ancho disponible: entran por prioridad y se colocan en el orden del usuario.
fn list_cols(w: f32, order: &[String], widths: &[(String, f32)]) -> ListCols {
    let cw = |d: &ColDef| widths.iter().find(|(i, _)| i == d.id).map(|(_, v)| v.clamp(40.0, 400.0)).unwrap_or(d.w);
    let mut fit: Vec<&str> = Vec::new();
    let mut used = 16.0;
    for d in COLS {
        if order.iter().any(|o| o == d.id) && w - (used + cw(d) + 14.0) >= 170.0 {
            used += cw(d) + 14.0;
            fit.push(d.id);
        }
    }
    let mut cols = Vec::new();
    let mut cur = 16.0;
    for c in order.iter().rev() {
        if let Some(d) = COLS.iter().find(|d| d.id == c && fit.contains(&d.id)) {
            let w = cw(d);
            cols.push(PCol { def: d, right: cur, w });
            cur += w + 14.0;
        }
    }
    ListCols { cols, reserve: cur.max(8.0) }
}

/// Texto de una celda de la lista.
fn col_text(id: &str, e: &fsys::Entry, x: Option<&fsys::ExtraMeta>) -> String {
    match id {
        "date" => fsys::fmt_time(e.mtime),
        "size" if !e.is_dir() => fsys::human_size(e.size),
        "kind" => e.kind.label().to_string(),
        "ext" if !e.is_dir() => e.ext().to_string(),
        "created" => x.map(|x| fsys::fmt_time(x.created)).unwrap_or_default(),
        "accessed" => x.map(|x| fsys::fmt_time(x.accessed)).unwrap_or_default(),
        "perms" => x.and_then(|x| x.mode).map(fsys::mode_str).unwrap_or_default(),
        "mode" => x.and_then(|x| x.mode).map(|m| format!("{m:03o}")).unwrap_or_default(),
        "owner" => x.map(|x| x.owner.clone()).unwrap_or_default(),
        "group" => x.map(|x| x.group.clone()).unwrap_or_default(),
        "items" => x.and_then(|x| x.items).map(|n| n.to_string()).unwrap_or_default(),
        "link" => x.map(|x| x.link.clone()).unwrap_or_default(),
        _ => String::new(),
    }
}

impl App {
    /// Listado ligero de una carpeta para las columnas estáticas (con caché de unos segundos).
    fn col_rows(&mut self, dir: &Path) -> Arc<Vec<(String, bool)>> {
        let cached = self.col_cache.get(dir).cloned();
        if let Some((t0, v)) = &cached {
            if t0.elapsed() < Duration::from_secs(3) {
                return v.clone();
            }
        }
        // el listado se lee en un hilo (una carpeta de red o un disco dormido no debe congelar la ventana)
        if self.col_busy.insert(dir.to_path_buf()) {
            let hidden = self.cfg.show_hidden;
            let d = dir.to_path_buf();
            self.spawn_io(
                {
                    let d = d.clone();
                    move || {
                        let mut rows: Vec<(String, bool)> = Vec::new();
                        if let Ok(rd) = crate::vfs::fs::read_dir(&d) {
                            for e in rd.flatten().take(20_000) {
                                let n = e.file_name().to_string_lossy().into_owned();
                                if !hidden && n.starts_with('.') {
                                    continue;
                                }
                                let dir = e.file_type().map(|t| t.v_is_dir() || (t.v_is_symlink() && e.path().v_is_dir())).unwrap_or(false);
                                rows.push((n, dir));
                            }
                        }
                        rows.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| fsys::natural_cmp(&a.0, &b.0)));
                        Arc::new(rows)
                    }
                },
                move |a, v| {
                    a.col_busy.remove(&d);
                    if a.col_cache.len() > 64 {
                        a.col_cache.clear();
                    }
                    a.col_cache.insert(d, (Instant::now(), v));
                },
            );
        }
        cached.map(|(_, v)| v).unwrap_or_default()
    }

    /// Columna estática (carpeta ascendente o contenido de la carpeta seleccionada).
    /// Devuelve el elemento pulsado: (nombre, es carpeta).
    fn col_static(&mut self, ui: &mut egui::Ui, rect: Rect, dir: &Path, hl: Option<&str>, salt: usize) -> Option<(String, bool)> {
        let pal = self.pal;
        let rows = self.col_rows(dir);
        let folder_tex = if self.cfg.sys_icons { self.sys.folder(&self.ctx, None) } else { None };
        let mut out = None;
        ui.scope_builder(egui::UiBuilder::new().max_rect(rect.shrink2(vec2(0.0, 0.0))), |ui| {
            egui::ScrollArea::vertical().id_salt(("col_static", salt)).auto_shrink([false, false]).show_rows(ui, 28.0, rows.len(), |ui, range| {
                for i in range {
                    let (name, is_dir) = &rows[i];
                    let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 28.0), Sense::click());
                    resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, true, hl == Some(name.as_str()), name));
                    theme::focus_ring(ui, &pal, &resp, 8.0);
                    let p = ui.painter();
                    if hl == Some(name.as_str()) {
                        p.rect_filled(r.shrink2(vec2(3.0, 1.0)), CornerRadius::same(8), pal.sel);
                        p.rect_stroke(r.shrink2(vec2(3.0, 1.0)), CornerRadius::same(8), Stroke::new(1.0, a11y::sel_ring(&pal)), egui::StrokeKind::Inside);
                    } else if resp.hovered() {
                        p.rect_filled(r.shrink2(vec2(3.0, 1.0)), CornerRadius::same(8), pal.hover.gamma_multiply(0.7));
                    }
                    let ir = Rect::from_center_size(pos2(r.left() + 18.0, r.center().y), vec2(18.0, 18.0));
                    if let (true, Some(tex)) = (*is_dir, if *is_dir { self.folder_tex_at(&dir.join(name)).or(folder_tex) } else { None }) {
                        p.image(tex, ir, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
                    } else if *is_dir {
                        theme::draw_folder(p, ir);
                    } else {
                        let ext = name.rsplit_once('.').map(|x| x.1).unwrap_or("");
                        theme::draw_file(p, ir, fsys::kind_of_ext(&ext.to_lowercase()), ext, &pal);
                    }
                    let mut job = egui::text::LayoutJob::simple_singleline(name.clone(), FontId::proportional(13.0), pal.text);
                    job.wrap.max_width = (r.width() - 62.0).max(20.0);
                    job.wrap.max_rows = 1;
                    job.wrap.break_anywhere = true;
                    let g = p.layout_job(job);
                    p.galley(pos2(r.left() + 34.0, r.center().y - g.size().y / 2.0), g, pal.text);
                    if *is_dir {
                        let c = pos2(r.right() - 14.0, r.center().y);
                        p.add(egui::Shape::line(
                            vec![c + vec2(-2.0, -4.0), c + vec2(2.0, 0.0), c + vec2(-2.0, 4.0)],
                            Stroke::new(1.3, pal.subtle),
                        ));
                    }
                    if resp.clicked() {
                        out = Some((name.clone(), *is_dir));
                    }
                }
            });
        });
        out
    }

    /// Vista de columnas: ascendentes | carpeta actual (columna activa, completa) | contenido/ficha de la selección.
    pub(super) fn show_columns(&mut self, ui: &mut egui::Ui) {
        let pal = self.pal;
        let full = ui.available_rect_before_wrap();
        let n_fit = ((full.width() / 230.0).floor() as usize).max(2);
        let mut anc: Vec<PathBuf> = self.cwd.ancestors().skip(1).map(|p| p.to_path_buf()).collect();
        let n = n_fit.min(anc.len() + 2);
        anc.truncate(n.saturating_sub(2));
        anc.reverse();
        let w = full.width() / n as f32;
        let mut nav: Option<(PathBuf, String, bool)> = None; // (carpeta de la columna, nombre, es carpeta)
        let mut x = full.left();
        for (i, dir) in anc.iter().enumerate() {
            let rect = Rect::from_min_size(pos2(x, full.top()), vec2(w, full.height()));
            let next: &Path = anc.get(i + 1).map(|p| p.as_path()).unwrap_or(self.cwd.as_path());
            let hl = next.file_name().map(|n| n.to_string_lossy().into_owned());
            if let Some((name, is_dir)) = self.col_static(ui, rect.shrink2(vec2(2.0, 0.0)), dir, hl.as_deref(), i) {
                nav = Some((dir.clone(), name, is_dir));
            }
            ui.painter().line_segment([rect.right_top() + vec2(-0.5, 6.0), rect.right_bottom() + vec2(-0.5, -6.0)], Stroke::new(1.0, pal.border));
            x += w;
        }
        // columna activa: la lista completa de siempre, sin cabecera ni columnas extra
        let arect = Rect::from_min_size(pos2(x, full.top()), vec2(w, full.height()));
        self.col_active = true;
        ui.scope_builder(egui::UiBuilder::new().max_rect(arect.shrink2(vec2(2.0, 0.0))), |ui| {
            ui.push_id("col_active", |ui| self.show_files(ui));
        });
        self.col_active = false;
        ui.painter().line_segment([arect.right_top() + vec2(-0.5, 6.0), arect.right_bottom() + vec2(-0.5, -6.0)], Stroke::new(1.0, pal.border));
        x += w;
        // columna de vista: contenido de la carpeta seleccionada o ficha del archivo
        let prect = Rect::from_min_size(pos2(x, full.top()), vec2(full.right() - x, full.height()));
        let sel: Option<(String, bool, u64, i64)> = if self.sel_count == 1 {
            self.cursor
                .and_then(|c| self.entries.get(c as usize))
                .map(|e| (e.name.to_string(), e.is_dir(), e.size, e.mtime))
        } else {
            None
        };
        match sel {
            Some((name, true, _, _)) => {
                let dir = self.cwd.join(&name);
                if let Some((n2, is_dir)) = self.col_static(ui, prect.shrink2(vec2(2.0, 0.0)), &dir, None, 99) {
                    nav = Some((dir, n2, is_dir));
                }
            }
            Some((name, false, size, mtime)) => {
                let ext = name.rsplit_once('.').map(|x| x.1).unwrap_or("").to_string();
                let c = pos2(prect.center().x, prect.top() + 90.0);
                theme::draw_file(ui.painter(), Rect::from_center_size(c, vec2(96.0, 96.0)), fsys::kind_of_ext(&ext.to_lowercase()), &ext, &pal);
                let p = ui.painter();
                p.text(pos2(prect.center().x, c.y + 72.0), Align2::CENTER_CENTER, &name, FontId::proportional(14.0), pal.text);
                p.text(pos2(prect.center().x, c.y + 94.0), Align2::CENTER_CENTER, fsys::human_size(size), FontId::proportional(12.5), pal.subtle);
                p.text(pos2(prect.center().x, c.y + 114.0), Align2::CENTER_CENTER, fsys::fmt_time(mtime), FontId::proportional(12.5), pal.subtle);
            }
            None => {}
        }
        if let Some((dir, name, is_dir)) = nav {
            // carpeta de una columna ascendente → entrar; archivo (o elemento de la columna de vista) → ir a su carpeta y seleccionarlo
            let from_preview = dir.starts_with(&self.cwd) && dir != self.cwd;
            if is_dir && !from_preview {
                self.navigate(dir.join(&name));
            } else {
                self.navigate(dir);
                self.pending_select = vec![name];
            }
        }
    }

    pub(super) fn confirm_dialog(&mut self, ctx: &egui::Context) {
        // cuántos y cuáles, por qué no van a la papelera y que no se puede deshacer: `ux.rs`
        self.ux_confirm_delete(ctx);
    }

    // -----------------------------------------------------------------------
    // Zona principal: cuadrícula / lista virtualizada
    // -----------------------------------------------------------------------

    fn list_header(&mut self, ui: &mut egui::Ui) {
        let pal = self.pal;
        // sin FOCUSABLE: no es una parada del Tab que no haga nada (el orden se cambia también desde el menú)
        let (full, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 30.0), Sense::CLICK);
        let ak = a11y::ak_on(ui.ctx(), resp.id);
        // misma geometría que las filas (sangría de 6 px a cada lado y 4 px del borde del área)
        let rect = Rect::from_min_max(pos2(full.left() + 6.0, full.top()), pos2(full.right() - 10.0, full.bottom()));
        let lc = list_cols(rect.width(), &self.cfg.columns, &self.cfg.col_w);
        let mut cols: Vec<(Option<SortKey>, &str, f32, Align2)> =
            vec![(Some(SortKey::Name), t("Nombre"), rect.left() + 46.0, Align2::LEFT_CENTER)];
        for pc in &lc.cols {
            let d = pc.def;
            if d.right {
                cols.push((d.sort, t(d.head), rect.right() - pc.right, Align2::RIGHT_CENTER));
            } else {
                cols.push((d.sort, t(d.head), rect.right() - pc.right - pc.w, Align2::LEFT_CENTER));
            }
        }
        // tiradores para cambiar la anchura: borde izquierdo de cada columna
        let mut set_w: Option<(&'static str, f32)> = None;
        let mut reset: Option<&'static str> = None;
        for pc in &lc.cols {
            let x = rect.right() - pc.right - pc.w - 7.0;
            let hr = Rect::from_center_size(pos2(x, rect.center().y), vec2(10.0, rect.height() - 6.0));
            // sin FOCUSABLE: el tirador es sólo para el ratón (no es una parada más del Tab)
            let hr_resp = ui.interact(hr, egui::Id::new(("col_w", pc.def.id, self.live_side)), Sense::CLICK | Sense::DRAG);
            if hr_resp.hovered() || hr_resp.dragged() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
                ui.painter().line_segment([pos2(x, hr.top() + 4.0), pos2(x, hr.bottom() - 4.0)], Stroke::new(1.5, pal.accent));
            }
            if hr_resp.dragged() {
                set_w = Some((pc.def.id, (pc.w - hr_resp.drag_delta().x).clamp(40.0, 400.0)));
            }
            if hr_resp.double_clicked() {
                reset = Some(pc.def.id);
            }
            if hr_resp.drag_stopped() {
                self.save_cfg();
            }
        }
        if let Some((id, w)) = set_w {
            match self.cfg.col_w.iter_mut().find(|(i, _)| i == id) {
                Some(e) => e.1 = w,
                None => self.cfg.col_w.push((id.to_string(), w)),
            }
        }
        if let Some(id) = reset {
            self.cfg.col_w.retain(|(i, _)| i != id);
            self.save_cfg();
        }
        let p = ui.painter();
        let rect = full;
        let ptr = ui.input(|i| i.pointer.hover_pos());
        for (i, (k, label, x, al)) in cols.iter().enumerate() {
            let active = *k == Some(self.cfg.sort);
            let col = if active { pal.accent } else { pal.subtle };
            let mut r = p.text(pos2(*x, rect.center().y), *al, *label, FontId::proportional(12.5), col);
            if active {
                let c = pos2(r.right() + 10.0, rect.center().y);
                let d = if self.cfg.asc { 1.0 } else { -1.0 };
                p.add(egui::Shape::convex_polygon(
                    vec![c + vec2(-4.0, 2.5 * d), c + vec2(4.0, 2.5 * d), c + vec2(0.0, -2.5 * d)],
                    col,
                    Stroke::NONE,
                ));
                r.max.x += 16.0;
            }
            let hit = r.expand2(vec2(12.0, 8.0));
            if ak {
                // cabecera para el lector de pantalla (sólo «pasa por encima»: el clic sigue siendo de `resp`)
                let h = ui.interact(hit, egui::Id::new(("list_head", i, self.live_side)), Sense::hover());
                h.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, true, active, *label));
            }
            if let (Some(k), true) = (*k, resp.clicked() && ptr.map(|q| hit.contains(q)).unwrap_or(false)) {
                self.sort_by(k);
            }
        }
        p.line_segment([rect.left_bottom(), rect.right_bottom()], Stroke::new(1.0, pal.border));
    }

    /// Editor de las columnas de la vista de lista: cuáles se muestran y en qué orden. Devuelve si cambió algo.
    pub(super) fn columns_editor(&mut self, ui: &mut egui::Ui) -> bool {
        let pal = self.pal;
        let mut changed = false;
        let avail = |d: &&ColDef| !d.unix_only || cfg!(unix);
        let shown: Vec<&ColDef> = self.cfg.columns.iter().filter_map(|c| COLS.iter().find(|d| d.id == c)).filter(avail).collect();
        let hidden: Vec<&ColDef> = COLS.iter().filter(avail).filter(|d| !shown.iter().any(|s| s.id == d.id)).collect();
        let mut order: Vec<&str> = shown.iter().map(|d| d.id).collect();
        ui.label(egui::RichText::new(t("Mostradas")).color(pal.subtle));
        if shown.is_empty() {
            ui.label(egui::RichText::new(t("Sólo se muestra el nombre")).color(pal.subtle));
        }
        let mut swap: Option<(usize, usize)> = None;
        let mut remove: Option<usize> = None;
        for (i, d) in shown.iter().enumerate() {
            ui.horizontal(|ui| {
                let mut on = true;
                if ui.checkbox(&mut on, t(d.label)).changed() {
                    remove = Some(i);
                }
                ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                    if tri_button(ui, &pal, false, i + 1 < shown.len()).clicked() {
                        swap = Some((i, i + 1));
                    }
                    if tri_button(ui, &pal, true, i > 0).clicked() {
                        swap = Some((i, i - 1));
                    }
                });
            });
        }
        let mut add: Option<&str> = None;
        if !hidden.is_empty() {
            ui.add_space(6.0);
            ui.label(egui::RichText::new(t("Disponibles")).color(pal.subtle));
            for d in &hidden {
                let mut on = false;
                if ui.checkbox(&mut on, t(d.label)).changed() {
                    add = Some(d.id);
                }
            }
        }
        if let Some((a, b)) = swap {
            order.swap(a, b);
            changed = true;
        }
        if let Some(i) = remove {
            order.remove(i);
            changed = true;
        }
        if let Some(id) = add {
            order.push(id);
            changed = true;
        }
        ui.add_space(8.0);
        if ui.button(t("Restaurar valores")).clicked() {
            order = DEFAULT_COLS.to_vec();
            changed = true;
        }
        if changed {
            self.cfg.columns = order.into_iter().map(String::from).collect();
        }
        changed
    }

    /// Franja superior que deja claro qué se está viendo: filtro, listado recursivo o búsqueda.
    fn mode_strip(&mut self, ui: &mut egui::Ui) {
        let pal = self.pal;
        match self.vmode.clone() {
            VMode::Search { query, all, content } => {
                let mut run = false;
                let mut new_all = all;
                let mut new_content = content;
                let mut close = false;
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    let w = (ui.available_width() - 400.0).clamp(120.0, 420.0);
                    let r = ui.add(
                        egui::TextEdit::singleline(&mut self.sbuf)
                            .hint_text(if content { t("Texto contenido…") } else { t("Nombre contiene…") })
                            .desired_width(w)
                            .margin(egui::Margin { left: 8, right: 30, top: 6, bottom: 6 }),
                    );
                    if !self.sbuf.is_empty() && self.ux_clear_button(ui, &r) {
                        self.sbuf.clear();
                    }
                    if self.search_focus {
                        r.request_focus();
                        self.search_focus = false;
                    }
                    if r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                        run = true;
                    }
                    if ui.button(t("Buscar")).clicked() {
                        run = true;
                    }
                    if ui.selectable_label(!all, t("Esta carpeta y subcarpetas")).clicked() && all {
                        new_all = false;
                        run = true;
                    }
                    if ui.selectable_label(all, t("Todo el equipo")).clicked() && !all {
                        new_all = true;
                        run = true;
                    }
                    if ui.selectable_label(content, t("En el contenido")).on_hover_text(t("Busca el texto dentro de archivos de texto, PDF y documentos de oficina")).clicked() {
                        new_content = !content;
                        run = true;
                    }
                    if ui.button(t("Cerrar búsqueda")).clicked() {
                        close = true;
                    }
                });
                let scope = if all { t("todo el equipo").to_string() } else { self.cwd.display().to_string() };
                let text = if query.trim().is_empty() {
                    tf("Escribe un nombre y pulsa Intro. Se buscará en: {}", &[&scope])
                } else {
                    let scanned = self.search_st.as_ref().map(|s| s.scanned.load(Ordering::Relaxed)).unwrap_or(0);
                    let mut state = if self.search_st.is_some() { format!(" · {} {}", t("buscando…"), scanned) } else { String::new() };
                    // la búsqueda se corta en MAX_RESULTS: que no parezca que no hay más
                    if self.entries.len() >= tools::MAX_RESULTS {
                        state.push_str(&tf(" · sólo se muestran los primeros {}; afina la búsqueda", &[&tools::MAX_RESULTS]));
                    }
                    tf("«{}» en {} · {} resultados{}", &[&query.trim(), &scope, &self.entries.len(), &state])
                };
                ui.label(egui::RichText::new(text).size(12.0).color(pal.subtle));
                ui.add_space(2.0);
                if close {
                    if self.tabs.len() > 1 {
                        self.close_tab(self.cur_tab);
                    } else {
                        let p = self.cwd.clone();
                        self.load_mode(p, false, VMode::Dir);
                    }
                } else if run {
                    let p = self.cwd.clone();
                    self.preview = None;
                    self.load_mode(p, false, VMode::Search { query: self.sbuf.clone(), all: new_all, content: new_content });
                }
            }
            VMode::Recent => {
                let mut clear = false;
                ui.horizontal_wrapped(|ui| {
                    ui.label(egui::RichText::new(crate::i18n::tn("Archivos abiertos recientemente · {} elemento|Archivos abiertos recientemente · {} elementos", self.entries.len(), &[])).size(12.0).color(pal.subtle));
                    if ui.add_enabled(!self.cfg.recent.is_empty(), egui::Button::new(t("Vaciar recientes")).small()).clicked() {
                        clear = true;
                    }
                });
                ui.add_space(2.0);
                if clear {
                    self.cfg.recent.clear();
                    self.save_cfg();
                    self.refresh();
                }
            }
            VMode::Trash => {
                let (mut restore, mut purge, mut empty) = (false, false, false);
                ui.horizontal_wrapped(|ui| {
                    ui.label(egui::RichText::new(crate::i18n::tn("Papelera · {} elemento|Papelera · {} elementos", self.entries.len(), &[])).size(12.0).color(pal.subtle));
                    let any = self.sel_count > 0;
                    if ui.add_enabled(any, egui::Button::new(t("Restaurar")).small()).clicked() {
                        restore = true;
                    }
                    if ui.add_enabled(any, egui::Button::new(t("Eliminar definitivamente")).small()).clicked() {
                        purge = true;
                    }
                    if ui.add_enabled(!self.entries.is_empty(), egui::Button::new(t("Vaciar papelera")).small()).clicked() {
                        empty = true;
                    }
                });
                ui.add_space(2.0);
                if restore {
                    let rows = self.trash_selected();
                    if !rows.is_empty() {
                        self.trash_run(true, Some(rows));
                    }
                } else if purge {
                    self.trash_ask_purge();
                } else if empty {
                    self.confirm_purge = Some(PurgeReq::All);
                }
            }
            VMode::Deep => {
                let mut back = false;
                ui.horizontal_wrapped(|ui| {
                    let text = tf(
                        "Mostrando todo el contenido de {} y sus subcarpetas · {} elementos",
                        &[&self.cwd.display(), &self.order.len()],
                    );
                    ui.label(egui::RichText::new(text).size(12.0).color(pal.subtle));
                    if ui.small_button(t("Volver a la carpeta")).clicked() {
                        back = true;
                    }
                });
                ui.add_space(2.0);
                if back {
                    self.set_deep(false);
                }
            }
            VMode::Dir => {
                if self.filter.trim().is_empty() {
                    return;
                }
                let (mut deep, mut all, mut clear) = (false, false, false);
                ui.horizontal_wrapped(|ui| {
                    let text = tf("Filtrando por «{}»: solo oculta elementos de esta carpeta", &[&self.filter.trim()]);
                    ui.label(egui::RichText::new(text).size(12.0).color(pal.subtle));
                    if ui.small_button(t("Incluir subcarpetas")).clicked() {
                        deep = true;
                    }
                    if ui.small_button(t("Buscar en todo el equipo")).clicked() {
                        all = true;
                    }
                    if ui.small_button(t("Quitar filtro")).clicked() {
                        clear = true;
                    }
                });
                ui.add_space(2.0);
                if deep {
                    self.set_deep(true);
                } else if all {
                    self.search_all_from_filter();
                } else if clear {
                    self.filter.clear();
                    self.spawn_sort();
                }
            }
        }
    }

    pub(super) fn show_files(&mut self, ui: &mut egui::Ui) {
        let pal = self.pal;
        let grid = self.cfg.grid;
        self.mode_strip(ui);
        if self.touch {
            // el hueco de la barra de selección está siempre reservado: nada salta al seleccionar
            self.sel_bar(ui);
        }
        if cfg!(target_os = "android") && !self.perm_ok {
            self.perm_banner(ui);
        }
        self.arch_banner(ui);
        if !grid && !self.col_active {
            self.list_header(ui);
        }
        let entries = self.entries.clone();
        // al cambiar de carpeta/pestaña no pintamos el listado a medias (llega sin orden y salta):
        // se espera un instante a que esté completo y ordenado
        let hold = (self.loading || self.sort_pending) && self.t_load.elapsed() < Duration::from_millis(150);
        if hold {
            self.ctx.request_repaint_after(Duration::from_millis(30));
        }
        let n = if hold { 0 } else { self.order.len() };
        let zoom0 = if self.phone {
            // móvil: 3 columnas que llenan el ancho
            ((ui.available_width() - 4.0 - 8.0) / 3.0 - 26.0).clamp(64.0, 150.0)
        } else {
            self.cfg.zoom
        };
        let width = ui.available_width() - 4.0;
        let cols = if grid { (((width - 8.0) / (zoom0 + 26.0)).floor() as usize).max(1) } else { 1 };
        // el tamaño del icono sigue al deslizador de forma continua; el sobrante de ancho se
        // reparte como separación entre celdas, sin huecos fijos a los lados
        let zoom = zoom0;
        let (cell_w, cell_h, row_pad) = if grid { ((width - 8.0) / cols as f32, zoom + 50.0, 8.0) } else { (0.0, if self.phone { 44.0 } else if self.touch { 48.0 } else { 32.0 }, 4.0) };
        let single = self.cfg.single_click.unwrap_or(cfg!(target_os = "android"));
        self.cols = cols;
        let rows = n.div_ceil(cols);
        let total_h = rows as f32 * cell_h + row_pad * 2.0;
        let x0 = if grid { ((width - cols as f32 * cell_w) / 2.0).max(4.0) } else { 6.0 };
        let cell_w = if grid { cell_w } else { width - 12.0 };

        let mut want: Vec<(usize, Request)> = Vec::new();
        let center_pos_cell: usize;
        let mut hover_pos_idx: Option<usize> = None;
        let mut area_rect = Rect::NOTHING;

        let top_id = egui::Id::new(("files", self.live_side));
        let reset_top = self.ctx.data_mut(|d| d.remove_temp::<bool>(top_id)).unwrap_or(false);
        // con la vista rápida encima la rueda es suya, no de la lista de detrás
        let wheel = self.preview.is_none();
        let mut sa = egui::ScrollArea::vertical().id_salt("files").auto_shrink([false, false]).scroll_source(egui::scroll_area::ScrollSource { drag: egui::scroll_area::DragScroll::Never, mouse_wheel: wheel, ..Default::default() });
        let kin = self.kinetic(ui, total_h);
        if let Some(o) = kin {
            sa = sa.vertical_scroll_offset(o);
        }
        if reset_top {
            sa = sa.vertical_scroll_offset(0.0);
            self.kin_vel = 0.0;
            self.keep_scroll = None;
        }
        // recarga de la misma carpeta: se vuelve al desplazamiento de antes cuando la lista está completa
        if !self.loading && !self.sort_pending {
            if let Some(k) = self.keep_scroll.take() {
                sa = sa.vertical_scroll_offset(k);
                self.kin_vel = 0.0;
            }
        }
        let mut vis_center = 0usize;
        let out = sa.show_viewport(ui, |ui, vp| {
            // en táctil no reclamamos el arrastre: así el dedo desplaza la lista
            let sense = if self.touch { Sense::click() } else { Sense::click_and_drag() };
            let (rect, resp) = ui.allocate_exact_size(vec2(width, total_h.max(vp.height())), sense);
            area_rect = rect;
            let clip = ui.clip_rect();
            self.a11y_files_area(ui, &resp, clip);
            let (sel_ring, kb_cursor, ak) = (a11y::sel_ring(&pal), self.a11y_kb_cursor(ui.ctx()), a11y::ak_on(ui.ctx(), resp.id));
            let painter = ui.painter().clone();
            let first_row = (((vp.min.y - row_pad) / cell_h).floor().max(0.0)) as usize;
            let last_row = (((vp.max.y - row_pad) / cell_h).ceil().max(0.0)) as usize;
            let vis_rows = last_row.saturating_sub(first_row).max(1);
            self.page_rows = vis_rows.saturating_sub(1).max(1);
            vis_center = (first_row + vis_rows / 2) * cols;

            let cell_rect = |pos: usize| -> Rect {
                let (r, c) = (pos / cols, pos % cols);
                Rect::from_min_size(
                    pos2(rect.left() + x0 + c as f32 * cell_w, rect.top() + row_pad + r as f32 * cell_h),
                    vec2(cell_w, cell_h),
                )
            };
            let hit = |q: Pos2| -> Option<usize> {
                if !rect.contains(q) {
                    return None;
                }
                let dx = q.x - rect.left() - x0;
                let dy = q.y - rect.top() - row_pad;
                if dx < 0.0 || dy < 0.0 {
                    return None;
                }
                let c = (dx / cell_w) as usize;
                if c >= cols {
                    return None;
                }
                let pos = (dy / cell_h) as usize * cols + c;
                if pos >= n {
                    return None;
                }
                if grid {
                    // sólo zona de icono+nombre, no los huecos
                    let r = cell_rect(pos).shrink2(vec2(4.0, 2.0));
                    if !r.contains(q) {
                        return None;
                    }
                }
                Some(pos)
            };

            // scroll a elemento (teclado)
            if let Some(idx) = self.scroll_to.take() {
                if let Some(pos) = self.order.iter().position(|&x| x == idx) {
                    ui.scroll_to_rect(cell_rect(pos).expand2(vec2(0.0, 6.0)), None);
                }
            }

            let hover = resp.hover_pos().filter(|q| clip.contains(*q) && !self.drag_items_blocked());
            let hovered_pos = hover.and_then(|q| hit(q));
            hover_pos_idx = hovered_pos;

            // ---- Entrada ----
            let (pressed, sec_pressed, mods) = ui.input(|i| (i.pointer.primary_pressed(), i.pointer.secondary_pressed(), i.modifiers));
            let ctrl = mods.command;
            if self.touch && pressed {
                // al soltar el dedo ya no hay posición de puntero: la guardamos al tocar
                // posición real del dedo (no la del último «hover», que puede ser de otro toque)
                self.touch_pt = ui.input(|i| i.pointer.press_origin().or(i.pointer.interact_pos()));
                // se guarda también el elemento: si la lista se reordena o recarga antes de soltar, la
                // posición ya no es la del elemento tocado y el toque se descarta
                self.touch_press = self.touch_pt.and_then(|q| hit(q)).and_then(|pos| self.order.get(pos).map(|&i| (pos, i)));
            }
            let mut blocked = false;
            if self.touch && self.tap_block && (resp.clicked() || resp.long_touched()) {
                self.touch_press = None;
                blocked = true;
                if resp.clicked() {
                    self.tap_block = false;
                }
            }
            if self.touch && resp.long_touched() && !blocked {
                // pulsación larga sobre un elemento: menú contextual (o alterna en selección múltiple)
                if let Some(idx) = self.touch_press.take().and_then(|(pos, i)| (self.order.get(pos) == Some(&i)).then_some(i)) {
                    if self.sel_mode {
                        let on = !self.is_sel(idx);
                        self.set_bit(idx, on);
                        self.recount();
                        if self.sel_count == 0 {
                            self.sel_mode = false;
                        }
                    } else if !self.is_sel(idx) {
                        // pulsación larga = clic derecho: selecciona y se abre el menú contextual
                        self.select_only(idx);
                    }
                    self.cursor = Some(idx);
                    self.anchor = Some(idx);
                }
            }
            if self.touch && resp.clicked() && !blocked {
                self.rename_commit_if_any();
                // zona de selección: esquina (círculo) en cuadrícula, icono en lista; el resto abre
                let in_zone = match (self.touch_pt, self.touch_press) {
                    (Some(q), Some((pos, _))) => {
                        let r = cell_rect(pos);
                        if grid {
                            // sólo la esquina (el círculo) selecciona; el resto abre
                            let c = r.shrink2(vec2(4.0, 2.0));
                            q.x < c.left() + 44.0 && q.y < c.top() + 44.0
                        } else {
                            q.x < r.left() + 46.0
                        }
                    }
                    _ => false,
                };
                match self.touch_press.take().and_then(|(pos, i)| (self.order.get(pos) == Some(&i)).then_some(i)) {
                    Some(idx) if self.sel_mode || in_zone => {
                        // selección múltiple (o esquina/icono de selección): tocar alterna
                        let on = !self.is_sel(idx);
                        self.sel_mode = true;
                        self.set_bit(idx, on);
                        self.recount();
                        if self.sel_count == 0 {
                            self.sel_mode = false;
                        }
                        self.cursor = Some(idx);
                        self.anchor = Some(idx);
                    }
                    Some(idx) => {
                        // toque simple: abre (si está activado); si no, selecciona y el doble toque abre
                        let now = ui.input(|i| i.time);
                        let double = single || self.last_tap.map(|(i, t)| i == idx && now - t < 0.45).unwrap_or(false);
                        self.last_tap = Some((idx, now));
                        if single {
                            // abrir no deja nada seleccionado
                            self.clear_sel();
                            self.cursor = Some(idx);
                            self.anchor = Some(idx);
                        } else {
                            self.select_only(idx);
                        }
                        if double && !resp.long_touched() {
                            self.last_tap = None;
                            let gen0 = self.generation;
                            self.open_index(idx);
                            if self.generation != gen0 {
                                return;
                            }
                        }
                    }
                    None if self.sel_mode => {}
                    None => self.clear_sel(),
                }
            }
            if !self.touch && resp.hovered() && (pressed || sec_pressed) {
                self.rename_commit_if_any();
                match hovered_pos {
                    Some(pos) => {
                        let idx = self.order[pos];
                        if mods.shift && !sec_pressed {
                            let a = self.anchor.and_then(|a| self.pos_of(a)).unwrap_or(pos);
                            self.select_range(a, pos, ctrl);
                            self.cursor = Some(idx);
                        } else if ctrl && !sec_pressed {
                            let on = !self.is_sel(idx);
                            self.set_bit(idx, on);
                            self.recount();
                            self.cursor = Some(idx);
                            self.anchor = Some(idx);
                        } else if !self.is_sel(idx) {
                            self.select_only(idx);
                        } else {
                            self.cursor = Some(idx);
                        }
                    }
                    None => {
                        if !ctrl && !mods.shift {
                            self.clear_sel();
                        }
                    }
                }
            }
            if resp.clicked() && !ctrl && !mods.shift {
                if let Some(pos) = hovered_pos {
                    let idx = self.order[pos];
                    if self.sel_count > 1 {
                        self.select_only(idx);
                    }
                }
            }
            #[cfg(feature = "selftest")]
            let forced = self.shot_dbl();
            #[cfg(not(feature = "selftest"))]
            let forced = false;
            if !self.touch && single && resp.clicked() && !ctrl && !mods.shift {
                if let Some(idx) = hovered_pos.and_then(|pos| self.order.get(pos).copied()) {
                    let gen0 = self.generation;
                    self.open_index(idx);
                    if self.generation != gen0 {
                        return;
                    }
                }
            }
            if (!self.touch && !single && resp.double_clicked()) || forced {
                if let Some(idx) = hovered_pos.or(forced.then_some(0)).and_then(|pos| self.order.get(pos).copied()) {
                    let gen0 = self.generation;
                    self.open_index(idx);
                    if self.generation != gen0 {
                        // se ha cambiado de carpeta: el listado de este fotograma ya no es válido
                        return;
                    }
                }
            }
            if resp.middle_clicked() {
                if let Some(pos) = hovered_pos {
                    let idx = self.order[pos];
                    if let Some(e) = self.entries.get(idx as usize).filter(|e| e.is_dir()) {
                        let p = self.cwd.join(&*e.name);
                        self.new_tab_bg(p);
                    }
                }
            }
            // arrastre: banda o mover elementos
            if resp.drag_started() {
                let origin = ui.input(|i| i.pointer.press_origin());
                let on_item = origin.and_then(|q| hit(q)).is_some();
                if on_item {
                    if self.sel_count == 0 {
                        if let Some(idx) = origin.and_then(|q| hit(q)).and_then(|pos| self.order.get(pos).copied()) {
                            self.select_only(idx);
                        }
                    }
                    self.drag_items = self.sel_count > 0 || self.sel.iter().any(|&b| b != 0);
                } else if let Some(o) = origin {
                    self.band = Some(Band { start: o - rect.min, base: if ctrl { self.sel.clone() } else { vec![0; self.sel.len()] } });
                }
            }
            if resp.dragged() {
                if let (Some(b), Some(cur)) = (&self.band, ui.input(|i| i.pointer.interact_pos())) {
                    let a = b.start;
                    let c = cur - rect.min;
                    let br = Rect::from_two_pos(pos2(a.x, a.y), pos2(c.x, c.y));
                    let base = b.base.clone();
                    // auto-scroll en los bordes
                    if cur.y < clip.top() + 24.0 {
                        ui.scroll_with_delta(vec2(0.0, 14.0));
                        ui.ctx().request_repaint();
                    } else if cur.y > clip.bottom() - 24.0 {
                        ui.scroll_with_delta(vec2(0.0, -14.0));
                        ui.ctx().request_repaint();
                    }
                    self.sel = base;
                    if self.sel.len() < self.entries.len().div_ceil(64) {
                        self.sel.resize(self.entries.len().div_ceil(64), 0);
                    }
                    let c0 = (((br.left() - x0) / cell_w).floor().max(0.0)) as usize;
                    let c1 = (((br.right() - x0) / cell_w).floor().max(0.0) as usize).min(cols - 1);
                    let r0 = (((br.top() - row_pad) / cell_h).floor().max(0.0)) as usize;
                    let r1 = ((br.bottom() - row_pad) / cell_h).floor().max(0.0) as usize;
                    for r in r0..=r1.min(rows.saturating_sub(1)) {
                        for c in c0..=c1 {
                            let pos = r * cols + c;
                            if pos >= n {
                                continue;
                            }
                            let cr = cell_rect(pos).translate(-rect.min.to_vec2()).shrink2(vec2(6.0, 4.0));
                            if cr.intersects(br) {
                                let idx = self.order[pos];
                                self.set_bit(idx, true);
                            }
                        }
                    }
                    self.recount();
                    let sr = br.translate(rect.min.to_vec2());
                    painter.rect_filled(sr, CornerRadius::same(4), pal.sel);
                    painter.rect_stroke(sr, CornerRadius::same(4), Stroke::new(1.0, pal.accent), egui::StrokeKind::Inside);
                }
            }
            if resp.drag_stopped() {
                self.band = None;
                if self.drag_items && std::mem::take(&mut self.bm_drop) {
                    self.drag_items = false;
                    let dirs: Vec<PathBuf> = self.selected_paths().into_iter().filter(|p| p.v_is_dir()).collect();
                    self.add_favs(dirs);
                } else if self.drag_items {
                    self.drag_items = false;
                    let target = self.side_drop.take().or_else(|| {
                        let q = ui.input(|i| i.pointer.interact_pos())?;
                        let Some(pos) = hit(q) else {
                            // soltar sobre el otro panel: carpeta actual de ese panel
                            let o = self.other.as_deref()?;
                            return self.pane_rects[1 - self.live_side as usize].contains(q).then(|| o.cwd.clone());
                        };
                        let idx = self.order[pos];
                        let e = &self.entries[idx as usize];
                        if e.is_dir() && !self.is_sel(idx) { Some(self.cwd.join(&*e.name)) } else { None }
                    });
                    if let Some(t) = target {
                        // Ctrl (Opción en macOS) copia, Mayús mueve; sin teclas, a otro volumen se copia
                        let m = ui.input(|i| i.modifiers);
                        self.ux_drop(t, m);
                    }
                }
            }
            let drop_pos = if self.drag_items {
                ui.input(|i| i.pointer.interact_pos()).and_then(|q| hit(q)).filter(|&p| {
                    let idx = self.order[p];
                    self.entries[idx as usize].is_dir() && !self.is_sel(idx)
                })
            } else {
                None
            };

            // ---- Pintado de elementos visibles ----
            let pref_first = first_row.saturating_sub(1) * cols;
            let pref_last = ((last_row + 2) * cols).min(n);
            let first = (first_row * cols).min(n);
            let last = ((last_row + 1) * cols).min(n);
            for pos in first..last {
                let idx = self.order[pos];
                let e = &entries[idx as usize];
                let r = cell_rect(pos);
                let sel = self.is_sel(idx);
                let hov = hovered_pos == Some(pos) || drop_pos == Some(pos);
                let renaming = self.rename.as_ref().map(|x| x.0 == idx).unwrap_or(false);
                self.a11y_entry(ui, ak, r.intersect(clip), pos, e, sel);
                if grid {
                    let card = r.shrink2(vec2(4.0, 2.0));
                    if sel || hov {
                        let fill = if sel { pal.sel } else { pal.hover.gamma_multiply(0.7) };
                        painter.rect_filled(card, CornerRadius::same(14), fill);
                        if (sel && !renaming) || drop_pos == Some(pos) {
                            // contorno ≥ 3:1: la selección no depende sólo del tinte
                            painter.rect_stroke(card, CornerRadius::same(14), Stroke::new(1.5, sel_ring), egui::StrokeKind::Inside);
                        }
                    }
                    if kb_cursor == Some(idx) && !renaming {
                        // cursor del teclado: anillo exterior, distinto del contorno de selección
                        painter.rect_stroke(card.expand(1.0), CornerRadius::same(15), Stroke::new(2.0, sel_ring), egui::StrokeKind::Outside);
                    }
                    let icon_r = Rect::from_center_size(pos2(r.center().x, r.top() + 8.0 + zoom / 2.0), vec2(zoom, zoom));
                    self.paint_icon(&painter, icon_r, e, true, &mut want, pos);
                    self.ux_link_emblem(&painter, icon_r, e);
                    if self.sel_mode || (self.touch && single) {
                        let c = pos2(card.left() + 18.0, card.top() + 18.0);
                        if sel {
                            painter.circle_filled(c, 10.0, pal.accent);
                            let st = Stroke::new(2.0, pal.on_accent);
                            painter.line_segment([c + vec2(-4.5, 0.5), c + vec2(-1.0, 4.0)], st);
                            painter.line_segment([c + vec2(-1.0, 4.0), c + vec2(5.0, -3.5)], st);
                        } else if self.sel_mode {
                            painter.circle_filled(c, 10.0, pal.bg.gamma_multiply(0.6));
                            painter.circle_stroke(c, 10.0, Stroke::new(1.5, pal.subtle));
                        } else {
                            // pista discreta de la zona de selección
                            painter.circle_stroke(c, 9.0, Stroke::new(1.2, pal.subtle.gamma_multiply(0.55)));
                        }
                    }
                    if renaming {
                        // más ancho que la celda para que el nombre quepa; sin salirse de la lista
                        let w = (cell_w + 40.0).min(ui.clip_rect().width() - 8.0);
                        let cx = r.center().x.clamp(ui.clip_rect().left() + 4.0 + w / 2.0, ui.clip_rect().right() - 4.0 - w / 2.0);
                        let lr = Rect::from_center_size(pos2(cx, r.top() + zoom + 29.0), vec2(w, 28.0));
                        self.rename_editor(ui, lr);
                    } else {
                        let mut job = egui::text::LayoutJob::simple(self.batch_names.get(&*e.name).map(|n| base_name(n)).unwrap_or(base_name(&e.name)).to_string(), FontId::proportional(12.5), pal.text, cell_w - 16.0);
                        job.wrap.max_rows = 2;
                        job.wrap.break_anywhere = true;
                        job.halign = Align::Center;
                        let g = painter.layout_job(job);
                        painter.galley(pos2(r.center().x, r.top() + zoom + 16.0), g, pal.text);
                    }
                } else {
                    let card = r.shrink2(vec2(2.0, 1.0));
                    if sel || hov {
                        let fill = if sel { pal.sel } else { pal.hover.gamma_multiply(0.7) };
                        painter.rect_filled(card, CornerRadius::same(9), fill);
                        if drop_pos == Some(pos) {
                            painter.rect_stroke(card, CornerRadius::same(9), Stroke::new(1.5, sel_ring), egui::StrokeKind::Inside);
                        } else if sel && !renaming {
                            painter.rect_stroke(card, CornerRadius::same(9), Stroke::new(1.0, sel_ring), egui::StrokeKind::Inside);
                        }
                    }
                    if kb_cursor == Some(idx) && !renaming {
                        painter.rect_stroke(card, CornerRadius::same(9), Stroke::new(2.0, sel_ring), egui::StrokeKind::Outside);
                    }
                    let icon_r = Rect::from_center_size(pos2(r.left() + 24.0, r.center().y), vec2(24.0, 24.0));
                    self.paint_icon(&painter, icon_r, e, false, &mut want, pos);
                    if !self.sel_mode {
                        self.ux_link_emblem(&painter, icon_r, e);
                    }
                    if self.sel_mode {
                        // en selección múltiple el icono se cambia por la marca de selección
                        let c = icon_r.center();
                        painter.circle_filled(c, 12.0, if sel { pal.accent } else { pal.bg });
                        if sel {
                            let st = Stroke::new(2.0, pal.on_accent);
                            painter.line_segment([c + vec2(-5.0, 0.5), c + vec2(-1.5, 4.0)], st);
                            painter.line_segment([c + vec2(-1.5, 4.0), c + vec2(5.0, -3.5)], st);
                        } else {
                            painter.circle_stroke(c, 11.0, Stroke::new(1.5, pal.subtle));
                        }
                    }
                    let cy = r.center().y;
                    let name_x = r.left() + 46.0;
                    let lc = list_cols(r.width(), if self.col_active { &[] } else { &self.cfg.columns }, &self.cfg.col_w);
                    let name_w = (r.right() - lc.reserve - name_x).max(40.0);
                    if renaming {
                        let lr = Rect::from_min_size(pos2(name_x - 4.0, r.top() + 2.0), vec2(name_w, r.height() - 4.0));
                        self.rename_editor(ui, lr);
                    } else {
                        let mut job = egui::text::LayoutJob::simple_singleline(self.batch_names.get(&*e.name).map(|n| base_name(n)).unwrap_or(base_name(&e.name)).to_string(), FontId::proportional(13.5), pal.text);
                        job.wrap.max_width = name_w;
                        job.wrap.max_rows = 1;
                        job.wrap.break_anywhere = true;
                        let g = painter.layout_job(job);
                        let gw = g.size().x;
                        painter.galley(pos2(name_x, cy - g.size().y / 2.0), g, pal.text);
                        if self.vmode != VMode::Dir {
                            // ubicación (carpeta que contiene el elemento), en tono apagado
                            let full = &*e.name;
                            let dir = &full[..full.len() - base_name(full).len()];
                            let dir = dir.trim_end_matches(['/', '\\']);
                            let rest = name_w - gw - 12.0;
                            if !dir.is_empty() && rest > 40.0 {
                                let mut j = egui::text::LayoutJob::simple_singleline(dir.to_string(), FontId::proportional(12.0), pal.subtle);
                                j.wrap.max_width = rest;
                                j.wrap.max_rows = 1;
                                j.wrap.break_anywhere = true;
                                let g2 = painter.layout_job(j);
                                painter.galley(pos2(name_x + gw + 12.0, cy - g2.size().y / 2.0), g2, pal.subtle);
                            }
                        }
                    }
                    let sub = pal.subtle;
                    if !lc.cols.is_empty() {
                        let key = self.cwd.join(&*e.name);
                        if lc.cols.iter().any(|c| c.def.meta) && !self.extra_cache.contains_key(&key) && self.extra_pending.len() < 64 && !self.extra_pending.contains(&key) {
                            // en un hilo: owner/elementos pueden tocar la red o NSS; la celda sale vacía hasta que llegue
                            self.extra_pending.insert(key.clone());
                            let (tx, k, d, c) = (self.extra_tx.clone(), key.clone(), e.is_dir(), self.ctx.clone());
                            crate::vfs::thread::spawn(move || {
                                let m = fsys::extra_meta(&k, d);
                                let _ = tx.send((k, m));
                                c.request_repaint();
                            });
                        }
                        let ex = self.extra_cache.get(&key);
                        for pc in &lc.cols {
                            let txt = if pc.def.id == "kind" && e.flags & fsys::F_LINK != 0 && self.ux.links.get(&key) == Some(&true) {
                                t("Enlace roto").to_string()
                            } else {
                                col_text(pc.def.id, e, ex)
                            };
                            if txt.is_empty() {
                                continue;
                            }
                            let mut job = egui::text::LayoutJob::simple_singleline(txt, FontId::proportional(12.5), sub);
                            job.wrap.max_width = pc.w;
                            job.wrap.max_rows = 1;
                            job.wrap.break_anywhere = true;
                            let g = painter.layout_job(job);
                            let x = if pc.def.right { r.right() - pc.right - g.size().x } else { r.right() - pc.right - pc.w };
                            painter.galley(pos2(x, cy - g.size().y / 2.0), g, sub);
                        }
                    }
                }
            }
            // prefetch (una fila antes/después)
            for pos in pref_first..pref_last {
                if pos >= first && pos < last {
                    continue;
                }
                let e = &entries[self.order[pos] as usize];
                self.want_thumb(e, pos, &mut want);
            }

            // menú contextual (en selección múltiple táctil lo ofrece la barra superior)
            if !(self.touch && self.sel_mode) {
                // siempre completo, sin scroll: si no cabe, se compacta el alto de las filas
                let avail = ui.ctx().content_rect().height() - 16.0;
                resp.context_menu(|ui| {
                    let row = (avail / 30.0).max(16.0);
                    let sp = ui.spacing_mut();
                    sp.item_spacing.y = sp.item_spacing.y.min(row * 0.15);
                    sp.interact_size.y = sp.interact_size.y.min(row - sp.item_spacing.y);
                    sp.button_padding.y = sp.button_padding.y.min(((row - 14.0) / 2.0).max(1.0));
                    self.context_menu(ui);
                });
            }

            // fantasma de arrastre
            if self.drag_items {
                if let Some(q) = ui.input(|i| i.pointer.interact_pos()) {
                    let gp = ui.ctx().layer_painter(egui::LayerId::new(egui::Order::Tooltip, egui::Id::new("ghost")));
                    // destino bajo el puntero (como al soltar) y si se copiará o moverá
                    let tgt = self.side_drop.clone().or_else(|| match hit(q) {
                        Some(pos) => {
                            let idx = self.order[pos];
                            let e = &self.entries[idx as usize];
                            (e.is_dir() && !self.is_sel(idx)).then(|| self.cwd.join(&*e.name))
                        }
                        None => {
                            let o = self.other.as_deref()?;
                            self.pane_rects[1 - self.live_side as usize].contains(q).then(|| o.cwd.clone())
                        }
                    });
                    let m = ui.input(|i| i.modifiers);
                    let txt = self.ux_ghost_text(tgt, m);
                    let g = gp.layout_no_wrap(txt, FontId::proportional(13.0), pal.on_accent);
                    let r = Rect::from_min_size(q + vec2(14.0, 14.0), g.size() + vec2(20.0, 12.0));
                    gp.rect_filled(r, CornerRadius::same(10), pal.accent);
                    gp.galley(r.min + vec2(10.0, 6.0), g, pal.on_accent);
                }
            }
        });
        self.kin_off = out.state.offset.y;
        center_pos_cell = vis_center;
        self.hover_name = hover_pos_idx.and_then(|p| self.order.get(p)).and_then(|&i| entries.get(i as usize)).map(|e| e.name.to_string());
        // carpeta bajo el puntero: destino de lo que se suelte desde otra aplicación
        if let Some(e) = hover_pos_idx.and_then(|p| self.order.get(p)).and_then(|&i| entries.get(i as usize)).filter(|e| e.is_dir()) {
            self.hover_dir = Some(self.cwd.join(&*e.name));
        }
        if let Some(n) = &mut self.hover_name {
            if self.ux.links.get(&self.cwd.join(&*n)) == Some(&true) {
                n.push_str("  ·  ");
                n.push_str(t("Enlace roto"));
            }
        }
        // cola de miniaturas: más cerca del centro visible primero
        // con la vista dividida se juntan las de los dos paneles y se piden una vez por fotograma (`ui`)
        self.thumb_want.extend(want.into_iter().map(|(pos, r)| (pos.abs_diff(center_pos_cell), r)));
        self.thumb_any = true;
        let _ = area_rect;
        if n == 0 && !self.loading && !hold {
            // según la vista: búsqueda sin texto o sin resultados, papelera, recientes, filtro, ocultos…
            self.ux_empty_state(ui);
        }
    }

    fn drag_items_blocked(&self) -> bool {
        false
    }

    fn rename_commit_if_any(&mut self) {
        if let Some((idx, text, _)) = self.rename.take() {
            self.commit_rename(idx, text);
        }
    }

    /// Inercia del desplazamiento (arrastre táctil y gestos de trackpad). Devuelve el offset a
    /// imponer a la lista cuando lo controlamos nosotros; `None` si manda el ScrollArea.
    fn kinetic(&mut self, ui: &mut egui::Ui, total_h: f32) -> Option<f32> {
        let area = ui.available_rect_before_wrap();
        let max = (total_h - area.height()).max(0.0);
        if self.touch && ui.input(|i| i.pointer.primary_pressed()) {
            // un toque que frena el deslizamiento, o que cierra un menú/diálogo, no activa nada
            let modal = !matches!(self.dlg, Dlg::None) || self.preview.is_some() || egui::Popup::is_any_open(ui.ctx());
            self.tap_block = self.kin_vel != 0.0 || modal;
        }
        // la vista rápida está encima: ni rueda ni arrastre para la lista
        if self.preview.is_some() {
            self.kin_vel = 0.0;
            self.kin_hist.clear();
            return None;
        }
        let (now, dt, pdelta, down, key, pad, notch) = ui.input(|i| {
            let mut pad = 0.0f32;
            let mut notch = 0.0f32;
            let mut wheel = false;
            let mut key = false;
            for e in &i.events {
                match e {
                    egui::Event::MouseWheel { unit: egui::MouseWheelUnit::Point, delta, .. } => pad -= delta.y,
                    // rueda del ratón: cada muesca da un impulso (ver más abajo)
                    egui::Event::MouseWheel { unit, delta, modifiers, .. } if !modifiers.any() => {
                        notch -= delta.y * if matches!(unit, egui::MouseWheelUnit::Line) { 64.0 } else { area.height() * 0.9 }
                    }
                    egui::Event::MouseWheel { .. } => wheel = true,
                    egui::Event::Key { pressed: true, .. } => key = true,
                    _ => {}
                }
            }
            (i.time, i.unstable_dt.min(0.05), i.pointer.delta(), i.pointer.any_down(), key || wheel, pad, notch)
        });
        // Rueda del ratón con inercia: cada muesca suma velocidad (giros seguidos aceleran) y la
        // lista se desliza con rozamiento fuerte; así responde al instante y no «arrastra».
        let over = ui.input(|i| i.pointer.hover_pos()).is_some_and(|q| area.contains(q));
        if !self.touch && over {
            if notch != 0.0 {
                self.kin_wheel_until = now + 0.5;
                let base = if self.kin_vel * notch > 0.0 { self.kin_vel } else { 0.0 };
                self.kin_vel = (base + notch * 13.0).clamp(-8000.0, 8000.0);
                self.kin_k = 7.0;
                self.kin_hist.clear();
            }
            if now < self.kin_wheel_until {
                // la rueda ya la gestionamos nosotros: que el ScrollArea no la aplique también
                ui.input_mut(|i| i.smooth_scroll_delta = egui::Vec2::ZERO);
            }
        }
        // velocidad media de la última fracción de gesto, medida hasta `end` (instante de referencia)
        let fling = |hist: &[(f64, f32)], end: f64| -> f32 {
            let w = 0.12;
            let sum: f32 = hist.iter().filter(|(t, _)| end - t <= w).map(|(_, d)| d).sum();
            let v = (sum / w as f32 * 1.2).clamp(-12000.0, 12000.0);
            if v.abs() < 60.0 { 0.0 } else { v }
        };
        if self.touch {
            let r = ui.interact(area, egui::Id::new(("kin_drag", self.live_side)), Sense::drag());
            if r.dragged() {
                self.kin_vel = 0.0;
                // el primer fotograma incluye el recorrido del umbral de arrastre, que si no se perdería
                let dy = if r.drag_started() {
                    ui.input(|i| i.pointer.press_origin().zip(i.pointer.latest_pos()).map(|(a, b)| b.y - a.y)).unwrap_or(pdelta.y)
                } else {
                    pdelta.y
                };
                self.kin_hist.retain(|(t, _)| now - t <= 0.1);
                self.kin_hist.push((now, -dy * 1.875));
                self.kin_off = (self.kin_off - dy * 1.875).clamp(0.0, max);
                return Some(self.kin_off);
            }
            if r.drag_stopped() {
                self.kin_vel = fling(&self.kin_hist, now);
                self.kin_k = 1.4;
                self.kin_hist.clear();
            }
        }
        // trackpad: acumulamos el gesto y al terminar lo dejamos deslizar (sólo el panel bajo el puntero:
        // con la vista dividida el gesto sobre un panel no debe mover el otro)
        if pad != 0.0 && !over {
            if !self.touch {
                self.kin_hist.clear();
            }
        } else if pad != 0.0 {
            self.kin_vel = 0.0;
            self.kin_hist.retain(|(t, _)| now - t <= 0.1);
            self.kin_hist.push((now, pad));
        } else if let Some(&(t, _)) = self.kin_hist.last() {
            if now - t > 0.05 {
                self.kin_vel = fling(&self.kin_hist, t);
                self.kin_k = 1.4;
                self.kin_hist.clear();
            } else {
                self.ctx.request_repaint_after(Duration::from_millis(60));
            }
        }
        if self.kin_vel != 0.0 {
            if down || key {
                self.kin_vel = 0.0;
                return None;
            }
            let off = self.kin_off + self.kin_vel * dt;
            let clamped = off.clamp(0.0, max);
            if clamped != off {
                self.kin_vel = 0.0;
            }
            self.kin_vel *= (-self.kin_k * dt).exp();
            if self.kin_vel.abs() < 10.0 {
                self.kin_vel = 0.0;
            }
            self.kin_off = clamped;
            self.ctx.request_repaint();
            return Some(clamped);
        }
        None
    }

    /// Barra de acciones de la selección múltiple táctil.
    /// Aviso (Android): sin acceso a todos los archivos sólo se ven las carpetas.
    fn perm_banner(&mut self, ui: &mut egui::Ui) {
        let pal = self.pal;
        egui::Frame::new().fill(pal.panel).corner_radius(CornerRadius::same(12)).inner_margin(egui::Margin::symmetric(12, 10)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(t("Sin el acceso a todos los archivos, Android sólo deja ver las carpetas."));
            ui.add_space(6.0);
            ui.spacing_mut().button_padding = vec2(14.0, 10.0);
            if ui.button(t("Conceder acceso")).clicked() {
                crate::android_request_all_files();
            }
        });
        ui.add_space(4.0);
    }

    fn sel_bar(&mut self, ui: &mut egui::Ui) {
        let pal = self.pal;
        let mut cmd: Option<Cmd> = None;
        let mut exit = false;
        egui::Frame::new()
            .fill(pal.panel)
            .corner_radius(CornerRadius::same(12))
            .inner_margin(egui::Margin::symmetric(8, 6))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.set_height(40.0);
                if !self.sel_mode {
                    ui.centered_and_justified(|ui| {
                        ui.label(egui::RichText::new(if self.cfg.grid { t("Toca el círculo de un elemento para seleccionarlo") } else { t("Toca el icono de un elemento para seleccionarlo") }).color(pal.subtle));
                    });
                    return;
                }
                egui::ScrollArea::horizontal().id_salt("selbar").scroll_source(egui::scroll_area::ScrollSource::ALL).show(ui, |ui| {
                  ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing = vec2(8.0, 8.0);
                    ui.spacing_mut().button_padding = vec2(12.0, 9.0);
                    if icon_button(ui, &pal, Glyph::Close, true, false, "").clicked() {
                        exit = true;
                    }
                    ui.label(egui::RichText::new(crate::i18n::tn("{} elemento|{} elementos", self.sel_count, &[])).strong());
                    if ui.button(t("Seleccionar todo")).clicked() {
                        cmd = Some(Cmd::SelectAll);
                    }
                    if ui.button(t("Copiar")).clicked() {
                        cmd = Some(Cmd::Copy);
                    }
                    if ui.button(t("Cortar")).clicked() {
                        cmd = Some(Cmd::Cut);
                    }
                    if self.clipboard.is_some() && ui.button(t("Pegar")).clicked() {
                        cmd = Some(Cmd::Paste);
                    }
                    if ui.button(t("Eliminar")).clicked() {
                        cmd = Some(if cfg!(target_os = "android") || crate::vfs::WEB { Cmd::DeletePerm } else { Cmd::Trash });
                    }
                    ui.menu_button("...", |ui| self.context_menu(ui));
                  });
                });
            });
        ui.add_space(4.0);
        if exit {
            self.sel_mode = false;
            self.clear_sel();
        } else if let Some(c) = cmd {
            self.run_cmd(c);
            if matches!(c, Cmd::Copy | Cmd::Cut | Cmd::Paste | Cmd::Trash | Cmd::DeletePerm) {
                // la acción termina el modo de selección (el borrado pide confirmación)
                if !matches!(c, Cmd::DeletePerm) || matches!(self.dlg, Dlg::None) {
                    self.sel_mode = false;
                }
            }
        }
    }

    fn rename_editor(&mut self, ui: &mut egui::Ui, rect: Rect) {
        let Some((idx, buf, first)) = self.rename.as_mut() else { return };
        let id = egui::Id::new("rename_edit");
        let pal = self.pal;
        // campo propio: fondo redondeado y borde de acento, sin el marco por defecto
        ui.painter().rect_filled(rect, CornerRadius::same(7), pal.panel);
        ui.painter().rect_stroke(rect, CornerRadius::same(7), Stroke::new(1.5, pal.accent), egui::StrokeKind::Inside);
        let te = egui::TextEdit::singleline(buf)
            .id(id)
            .frame(egui::Frame::NONE)
            .margin(vec2(6.0, 3.0))
            .horizontal_align(if self.cfg.grid { Align::Center } else { Align::LEFT })
            .text_color(pal.text)
            .vertical_align(Align::Center)
            .desired_width(rect.width());
        let r = ui.put(rect, te);
        if *first {
            r.request_focus();
            // seleccionar sólo el nombre (sin extensión)
            if let Some(mut st) = egui::TextEdit::load_state(ui.ctx(), id) {
                let stem = buf.rsplit_once('.').filter(|(s, _)| !s.is_empty()).map(|(s, _)| s.chars().count()).unwrap_or(buf.chars().count());
                st.cursor.set_char_range(Some(egui::text::CCursorRange::two(egui::text::CCursor::new(0), egui::text::CCursor::new(stem))));
                st.store(ui.ctx(), id);
            }
            *first = false;
        }
        let enter = ui.input(|i| i.key_pressed(Key::Enter));
        let esc = ui.input(|i| i.key_pressed(Key::Escape));
        let idx = *idx;
        if esc {
            self.rename = None;
        } else if enter || r.lost_focus() {
            let text = buf.clone();
            self.rename = None;
            self.commit_rename(idx, text);
        }
    }

    fn want_thumb(&self, e: &Entry, pos: usize, want: &mut Vec<(usize, Request)>) {
        if !self.cfg.thumbs_on || !crate::thumbs::thumbable(e.kind, e.ext()) || e.size == 0 {
            return;
        }
        let key = self.thumb_key(e);
        if self.thumbs.has(key) || self.thumbs.is_failed(key) {
            return;
        }
        want.push((pos, Request { key, path: self.cwd.join(&*e.name) }));
    }

    fn special_of(&self, e: &Entry) -> Option<&'static str> {
        if self.home.as_deref() != Some(self.cwd.as_path()) {
            return None;
        }
        self.special_dirs.iter().find(|(n, _)| **n == *e.name).map(|(_, k)| *k)
    }

    /// Dibuja el icono del tema del sistema; `false` si no hay (se usa el vectorial).
    fn paint_sys(&mut self, p: &egui::Painter, r: Rect, e: &Entry) -> bool {
        if !self.cfg.sys_icons {
            return false;
        }
        let id = if e.is_dir() {
            let sp = self.special_of(e);
            self.sys.folder(&self.ctx, sp)
        } else if matches!(e.ext().to_ascii_lowercase().as_str(), "exe" | "lnk" | "ico" | "scr" | "url") {
            // programas y accesos directos: su icono propio (el del juego o aplicación), no el genérico
            let own = self.sys.for_path(&self.ctx, &self.cwd.join(&*e.name));
            own.or_else(|| self.sys.file(&self.ctx, e.ext()))
        } else {
            self.sys.file(&self.ctx, e.ext())
        };
        match id {
            Some(id) => {
                p.image(id, r, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
                true
            }
            None => false,
        }
    }

    fn paint_icon(&mut self, p: &egui::Painter, r: Rect, e: &Entry, big: bool, want: &mut Vec<(usize, Request)>, pos: usize) {
        let pal = self.pal;
        let thumbable = self.cfg.thumbs_on && crate::thumbs::thumbable(e.kind, e.ext());
        if thumbable {
            let key = self.thumb_key(e);
            if let Some(t) = self.thumbs.get(key) {
                let ts = t.size_vec2();
                let inner = if big { r.shrink(2.0) } else { r };
                let s = (inner.width() / ts.x).min(inner.height() / ts.y);
                let ir = Rect::from_center_size(inner.center(), ts * s);
                if big {
                    p.rect_filled(ir.expand(3.0), CornerRadius::same(10), pal.surface);
                }
                let shape = egui::epaint::RectShape::filled(ir, CornerRadius::same(if big { 8 } else { 4 }), Color32::WHITE)
                    .with_texture(t.id(), Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)));
                p.add(shape);
                return;
            }
            self.want_thumb(e, pos, want);
        }
        if self.paint_folder_custom(p, r, e) || self.paint_sys(p, r, e) {
            return;
        }
        match e.kind {
            Kind::Folder => theme::draw_folder(p, r),
            k => theme::draw_file(p, r, k, e.ext(), &pal),
        }
    }

    fn context_menu(&mut self, ui: &mut egui::Ui) {
        ui.set_min_width(190.0);
        if self.vmode == VMode::Trash {
            let any = self.sel_count > 0;
            if ui.add_enabled(any, egui::Button::new(t("Restaurar"))).clicked() {
                let rows = self.trash_selected();
                if !rows.is_empty() {
                    self.trash_run(true, Some(rows));
                }
                ui.close();
            }
            if ui.add_enabled(any, egui::Button::new(t("Eliminar definitivamente"))).clicked() {
                self.trash_ask_purge();
                ui.close();
            }
            ui.separator();
            if ui.add_enabled(!self.entries.is_empty(), egui::Button::new(t("Vaciar papelera"))).clicked() {
                self.confirm_purge = Some(PurgeReq::All);
                ui.close();
            }
            return;
        }
        if self.sel_count > 0 {
            if let Some(c) = self.cursor.filter(|&c| self.is_sel(c)) {
                if ui.button(t("Abrir")).clicked() {
                    self.open_index(c);
                    ui.close();
                    // abrir una carpeta recarga la lista: `c` ya no vale
                    return;
                }
                if ui.add(menus::sc_btn(t("Vista previa"), Cmd::Preview)).clicked() {
                    self.toggle_preview();
                    ui.close();
                }
                if self.vmode != VMode::Dir {
                    let Some(en) = self.entries.get(c as usize) else { return };
                    let p = self.cwd.join(&*en.name);
                    if ui.button(t("Mostrar en su carpeta")).clicked() {
                        self.reveal(&p);
                        ui.close();
                        // la lista se ha recargado: `c` ya no es un índice válido
                        return;
                    }
                }
                let Some(e) = self.entries.get(c as usize) else { return };
                if e.is_dir() {
                    let p = self.cwd.join(&*e.name);
                    if ui.button(t("Abrir en una pestaña nueva")).clicked() {
                        self.new_tab_bg(p.clone());
                        ui.close();
                    }
                    self.sys_term_menu_item(ui, &p);
                } else if self.sel_count == 1 {
                    let p = self.cwd.join(&*e.name);
                    // `is_program` lee el archivo: una vez por menú y en un hilo, no en cada fotograma
                    if self.prog_cache.as_ref().map(|(q, _)| *q != p).unwrap_or(true) {
                        self.prog_cache = Some((p.clone(), None));
                        let (q, q2) = (p.clone(), p.clone());
                        self.spawn_io(move || is_program(&q), move |a, r| {
                            if let Some((k, v)) = &mut a.prog_cache {
                                if *k == q2 {
                                    *v = Some(r);
                                }
                            }
                        });
                    }
                    if self.prog_cache.as_ref().and_then(|(_, v)| *v) == Some(true) {
                        if ui.button(t("Ejecutar")).clicked() {
                            self.run_program(&p);
                            ui.close();
                        }
                        if !crate::vfs::WEB && ui.button(t("Ejecutar en el terminal")).clicked() {
                            self.run_in_terminal(&p);
                            ui.close();
                        }
                    }
                    self.open_with_menu(ui);
                }
                let Some(e) = self.entries.get(c as usize) else { return };
                if e.is_dir() {
                    let p = self.cwd.join(&*e.name);
                    let has = self.is_fav(&p);
                    if ui.button(if has { t("Quitar de favoritos") } else { t("Añadir a favoritos") }).clicked() {
                        self.toggle_fav(p.clone());
                        ui.close();
                    }
                    if self.vmode != VMode::Trash && ui.button(t("Cambiar icono…")).clicked() {
                        self.open_folder_icon(p);
                        ui.close();
                    }
                }
                ui.separator();
            }
            if ui.add(menus::sc_btn(t("Copiar"), Cmd::Copy)).clicked() {
                self.do_copy(false);
                ui.close();
            }
            if ui.add(menus::sc_btn(t("Cortar"), Cmd::Cut)).clicked() {
                self.do_copy(true);
                ui.close();
            }
        }
        if ui.add_enabled(!self.undo_stack.is_empty(), menus::sc_btn(t("Deshacer"), Cmd::Undo)).clicked() {
            self.undo();
            ui.close();
        }
        if ui.add_enabled(self.clipboard.is_some(), menus::sc_btn(t("Pegar"), Cmd::Paste)).clicked() {
            self.do_paste();
            ui.close();
        }
        if self.sel_count > 0 {
            if self.other.is_some() {
                if ui.button(t("Copiar al otro panel")).clicked() {
                    self.to_other_pane(false);
                    ui.close();
                }
                if ui.button(t("Mover al otro panel")).clicked() {
                    self.to_other_pane(true);
                    ui.close();
                }
            }
            if ui.button(t("Duplicar")).clicked() {
                self.do_duplicate();
                ui.close();
            }
            if ui.button(t("Copiar ruta")).clicked() {
                let t = self.selected_paths().iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join("\n");
                self.ctx.copy_text(t);
                ui.close();
            }
            ui.separator();
            if ui.button(t("Comprimir…")).clicked() {
                self.do_zip();
                ui.close();
            }
            let one_file = self.sel_count == 1 && self.cursor.and_then(|c| self.entries.get(c as usize)).map(|e| !e.is_dir()).unwrap_or(false);
            if one_file && ui.button(t("Calcular hash…")).clicked() {
                self.open_hash();
                ui.close();
            }
            let is_zip = self.sel_count == 1
                && self.cursor.and_then(|c| self.entries.get(c as usize)).map(|e| crate::archive::detect(Path::new(&*e.name)).is_some()).unwrap_or(false);
            if is_zip && ui.button(t("Abrir comprimido")).clicked() {
                if let Some(p) = self.selected_paths().into_iter().next() {
                    self.open_archive(p);
                }
                ui.close();
            }
            if is_zip && ui.button(t("Extraer aquí")).clicked() {
                self.do_unzip();
                ui.close();
            }
            ui.separator();
            if ui.add(menus::sc_btn(t("Renombrar"), Cmd::Rename)).clicked() {
                self.start_rename();
                ui.close();
            }
            if extras::trash_supported() && ui.add(menus::sc_btn(t("Mover a la papelera"), Cmd::Trash)).clicked() {
                self.run_cmd(Cmd::Trash);
                ui.close();
            }
            if ui.add(menus::sc_btn(t("Eliminar definitivamente"), Cmd::DeletePerm)).clicked() {
                self.run_cmd(Cmd::DeletePerm);
                ui.close();
            }
            ui.separator();
            if ui.add(menus::sc_btn(t("Propiedades"), Cmd::Props)).clicked() {
                self.open_props();
                ui.close();
            }
            #[cfg(windows)]
            if ui.button(t("Más opciones de Windows…")).clicked() {
                ui.close();
                self.native_menu(self.selected_paths(), false);
            }
        } else {
            ui.separator();
            if ui.button(t("Nueva carpeta")).clicked() {
                self.new_folder();
                ui.close();
            }
            if ui.button(t("Nuevo archivo vacío…")).clicked() {
                self.dlg = Dlg::NewFile(t("Nuevo archivo.txt").into());
                ui.close();
            }
            let here = self.cwd.clone();
            self.sys_term_menu_item(ui, &here);
            if !crate::vfs::WEB && ui.button(t("Terminal integrado aquí")).clicked() {
                self.terminal_here();
                ui.close();
            }
            #[cfg(not(target_os = "android"))]
            if ui.button(t("Compartir esta carpeta…")).clicked() {
                self.share_folder();
                ui.close();
            }
            if ui.button(t("Propiedades de la carpeta")).clicked() {
                self.open_props();
                ui.close();
            }
            #[cfg(windows)]
            if self.vmode == VMode::Dir && ui.button(t("Más opciones de Windows…")).clicked() {
                ui.close();
                self.native_menu(vec![self.cwd.clone()], true);
            }
            if ui.add(menus::sc_btn(t("Seleccionar todo"), Cmd::SelectAll)).clicked() {
                self.select_range(0, self.order.len().saturating_sub(1), false);
                ui.close();
            }
            if ui.add(menus::sc_btn(t("Actualizar"), Cmd::Refresh)).clicked() {
                self.refresh();
                ui.close();
            }
        }
    }
}
