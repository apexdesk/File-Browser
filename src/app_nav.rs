//! Vista dividida, navegación y carga del listado, selección y pestañas.
use super::*;

impl App {
    // -----------------------------------------------------------------------
    // Vista dividida
    // -----------------------------------------------------------------------

    /// Intercambia los campos de listado de `App` con el panel inactivo.
    pub(super) fn swap_pane(&mut self) {
        let Some(o) = self.other.as_deref_mut() else { return };
        macro_rules! sw {
            ($($f:ident),* $(,)?) => { $( std::mem::swap(&mut self.$f, &mut o.$f); )* };
        }
        sw!(
            cwd, back, fwd, back_snap, fwd_snap, generation, gen_shared, load_tx, load_rx, loading, load_err, dir_hash, dir_mtime,
            last_check, entries, order, sort_seq, sort_tx, sort_rx, filter, filter_focus, sel, sel_count, sel_size,
            anchor, cursor, band, drag_items, scroll_to, cols, page_rows, rename, pending_select, pending_rename,
            typeahead, vmode, search_st, search_taken, sbuf, t_load, hover_name,
            // la inercia y el desplazamiento también son de cada panel
            sort_pending, last_tap, kin_off, kin_vel, kin_k, kin_wheel_until, kin_hist, keep_scroll, keep_cursor
        );
        #[cfg(feature = "selftest")]
        sw!(traced_first);
        self.live_side = 1 - self.live_side;
    }

    pub(super) fn ensure_live(&mut self, side: u8) {
        if self.other.is_some() && self.live_side != side {
            self.swap_pane();
        }
    }

    pub(super) fn open_split(&mut self, dir: PathBuf) {
        if self.other.is_some() {
            return;
        }
        self.other = Some(Box::new(PaneState::new(dir.clone())));
        self.swap_pane();
        self.load(dir, false);
        self.swap_pane();
        self.active_side = 0;
        self.live_side = 0;
    }

    /// 0 = lista, 1 = cuadrícula, 2 = columnas (cierra la vista dividida).
    pub(super) fn set_view(&mut self, m: u8) {
        self.cfg.grid = m == 1;
        self.cfg.col_view = m == 2;
        if m == 2 && self.other.is_some() {
            self.toggle_split();
        }
        self.save_cfg();
    }

    pub(super) fn toggle_split(&mut self) {
        if self.cfg.col_view && self.other.is_none() {
            self.say(t("La vista de columnas no admite vista dividida").into(), true);
            return;
        }
        if self.other.is_some() {
            // el panel con foco pasa a ser el único
            self.ensure_live(self.active_side);
            if let Some(o) = self.other.as_ref().and_then(|o| o.search_st.clone()) {
                o.cancel.store(true, Ordering::Relaxed);
            }
            self.other = None;
            self.active_side = 0;
            self.live_side = 0;
            self.cfg.split = false;
        } else {
            self.open_split(self.cwd.clone());
            self.cfg.split = true;
        }
        self.preview = None;
        self.save_cfg();
    }

    pub(super) fn focus_side(&mut self, side: u8) {
        if self.other.is_none() || side == self.active_side {
            return;
        }
        self.active_side = side;
        self.ensure_live(side);
        self.addr_edit = None;
        self.preview = None;
        self.cfg.last_dir = Some(self.cwd.clone());
    }

    pub(super) fn to_other_pane(&mut self, mv: bool) {
        let Some(o) = self.other.as_deref() else { return };
        let dst = o.cwd.clone();
        let srcs = self.selected_paths();
        if !srcs.is_empty() {
            self.start_copy(srcs, dst, mv);
        }
    }

    pub(super) fn show_split(&mut self, ui: &mut egui::Ui, card: Color32) {
        let pal = self.pal;
        let full = ui.available_rect_before_wrap();
        let gap = 8.0;
        let stack = self.cfg.split_stack;
        let total = if stack { full.height() } else { full.width() } - gap;
        let first = (total * self.cfg.split_ratio).round().clamp(120.0, (total - 120.0).max(120.0));
        let ctx = ui.ctx().clone();
        // divisor movible
        let div = if stack {
            Rect::from_min_size(pos2(full.left(), full.top() + first), vec2(full.width(), gap))
        } else {
            Rect::from_min_size(pos2(full.left() + first, full.top()), vec2(gap, full.height()))
        };
        let dr = ui.interact(div.expand(2.0), egui::Id::new("split_divider"), Sense::drag());
        if dr.hovered() || dr.dragged() {
            ctx.set_cursor_icon(if stack { egui::CursorIcon::ResizeVertical } else { egui::CursorIcon::ResizeHorizontal });
            ui.painter().rect_filled(div.shrink2(if stack { vec2(40.0, 2.5) } else { vec2(2.5, 40.0) }), 2.0, pal.accent.gamma_multiply(0.6));
        }
        if dr.dragged() {
            let d = if stack { dr.drag_delta().y } else { dr.drag_delta().x };
            self.cfg.split_ratio = ((first + d) / total).clamp(0.2, 0.8);
        }
        if dr.double_clicked() {
            self.cfg.split_ratio = 0.5;
        }
        if dr.drag_stopped() || dr.double_clicked() {
            self.save_cfg();
        }
        let (pressed, pos) = ctx.input(|i| (i.pointer.primary_pressed(), i.pointer.interact_pos()));
        for side in 0..2u8 {
            let (off, len) = if side == 0 { (0.0, first) } else { (first + gap, total - first) };
            let rect = if stack {
                Rect::from_min_size(pos2(full.left(), full.top() + off), vec2(full.width(), len))
            } else {
                Rect::from_min_size(pos2(full.left() + off, full.top()), vec2(len, full.height()))
            };
            self.pane_rects[side as usize] = rect;
            self.ensure_live(side);
            let active = side == self.active_side;
            let stroke = if active {
                Stroke::new(1.5, pal.accent.gamma_multiply(0.85))
            } else {
                Stroke::new(1.0, pal.border.gamma_multiply(0.6))
            };
            ui.scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
                ui.push_id(("pane", side), |ui| {
                    egui::Frame::new()
                        .fill(card)
                        .stroke(stroke)
                        .corner_radius(CornerRadius::same(14))
                        .inner_margin(egui::Margin::symmetric(6, 6))
                        .show(ui, |ui| {
                            let sz = ui.available_size();
                            ui.set_min_size(sz);
                            if self.preview.is_some() {
                                ui.disable();
                            }
                            let name = self.cwd.display().to_string();
                            let col = if active { pal.text } else { pal.subtle };
                            ui.add(egui::Label::new(egui::RichText::new(name).size(12.0).color(col)).truncate());
                            ui.add_space(2.0);
                            self.show_files(ui);
                        });
                });
            });
            if pressed && side != self.active_side && pos.map(|q| rect.contains(q)).unwrap_or(false) {
                self.active_side = side;
                self.addr_edit = None;
                self.preview = None;
                self.cfg.last_dir = Some(self.cwd.clone());
            }
        }
        self.ensure_live(self.active_side);
    }

    // -----------------------------------------------------------------------
    // Navegación y carga
    // -----------------------------------------------------------------------

    pub(super) fn load(&mut self, path: PathBuf, keep_filter: bool) {
        // recargar la misma carpeta conserva el modo (recursivo / búsqueda); navegar vuelve a carpeta normal
        let mode = match virt_mode(&path) {
            Some(m) => m,
            None if path == self.cwd => self.vmode.clone(),
            None => VMode::Dir,
        };
        self.load_mode(path, keep_filter, mode);
    }

    pub(super) fn load_mode(&mut self, path: PathBuf, keep_filter: bool, mode: VMode) {
        // dirección sftp:// guardada (pestaña, carpeta de inicio, historial): su carpeta espejo si ese
        // servidor ya está conectado; si no, se pide conectar (sólo al llegar, no en cada recarga)
        let (path, net) = match path.to_str().filter(|u| app_places::is_net_uri(u)).map(str::to_string) {
            Some(u) => match sftp_mirror(&u, false) {
                Some(p) => (p, None),
                None => (path, Some(u)),
            },
            None => (path, None),
        };
        let ask = net.is_some() && (path != self.cwd || self.load_err.is_none());
        // un renombrado a medio escribir al irse a otra carpeta (favorito, pestaña…) se aplica, como al
        // hacer clic fuera; antes de cambiar `cwd` y la lista, que es a lo que se refiere
        if path != self.cwd || mode != self.vmode {
            if let Some((idx, buf, _)) = self.rename.take() {
                self.commit_rename(idx, buf);
            }
        }
        self.extra_cache.clear();
        // un orden pendiente de la lista anterior ya no vale
        self.sort_seq += 1;
        if let Some(old) = self.search_st.take() {
            old.cancel.store(true, Ordering::Relaxed);
        }
        self.search_taken = 0;
        self.vmode = mode;
        self.generation += 1;
        self.gen_shared.store(self.generation, Ordering::Relaxed);
        self.cwd = path.clone();
        self.entries = Arc::new(Vec::new());
        self.order.clear();
        self.sel.clear();
        self.sel_count = 0;
        self.sel_size = 0;
        self.anchor = None;
        self.cursor = None;
        self.band = None;
        self.rename = None;
        if !keep_filter {
            self.filter.clear();
        }
        self.loading = true;
        self.t_load = Instant::now();
        #[cfg(feature = "selftest")]
        {
            self.traced_first = false;
        }
        self.load_err = None;
        self.keep_scroll = None;
        self.keep_cursor = None;
        self.dir_hash = hash_str(0, &path.to_string_lossy());
        // sin `stat` aquí (en red bloquearía la interfaz): la base es la última fecha que vio la
        // vigilancia; si no la hay, la primera que vea
        self.dir_mtime = crate::thumbs::lock(&self.watch_res).iter().find(|(p, _)| *p == path).and_then(|(_, m)| *m);
        self.prog_cache = None;
        if self.live_side == self.active_side && virt_mode(&path).is_none() {
            self.cfg.last_dir = Some(path.clone());
        }
        if let Some(u) = net {
            if sftp_live_root(&u).is_some() {
                // servidor conectado, carpeta aún sin reflejar: se busca por la red
                let (want, u2) = (path.clone(), u);
                self.spawn_io(
                    move || sftp_mirror(&u2, true),
                    move |a, r| {
                        if a.cwd != want {
                            return;
                        }
                        match r {
                            Some(p) => a.load(p, true),
                            None => {
                                a.loading = false;
                                a.load_err = Some(tf("No existe: {}", &[&want.display()]));
                            }
                        }
                    },
                );
            } else {
                self.loading = false;
                self.load_err = Some(t("Sin conexión con el servidor").into());
                if ask {
                    self.connect_uri(u);
                }
            }
            return;
        }
        match self.vmode.clone() {
            VMode::Dir => {
                fsys::spawn_load(path, self.generation, self.gen_shared.clone(), self.load_tx.clone(), self.ctx.clone());
            }
            VMode::Deep => {
                let hidden = self.cfg.show_hidden;
                fsys::spawn_load_deep(path, hidden, self.generation, self.gen_shared.clone(), self.load_tx.clone(), self.ctx.clone());
            }
            VMode::Recent => {
                let list = self.cfg.recent.clone();
                let (tx, generation, ctx) = (self.load_tx.clone(), self.generation, self.ctx.clone());
                crate::vfs::thread::spawn(move || {
                    // un archivo de un servidor SFTP se guarda por su dirección: se busca en el espejo si
                    // ese servidor está conectado (si no, no se muestra pero sigue en la lista)
                    let mut seen = std::collections::HashSet::new();
                    let ents: Vec<fsys::Entry> = list
                        .iter()
                        .filter_map(|p| match p.to_str().filter(|u| app_places::is_net_uri(u)) {
                            Some(u) => sftp_mirror(u, true),
                            None => Some(p.clone()),
                        })
                        .filter(|p| seen.insert(p.clone()))
                        .filter_map(|p| fsys::entry_from_path(&p, p.to_string_lossy().into_owned()))
                        .collect();
                    let _ = tx.send(fsys::LoadMsg::Chunk(generation, ents));
                    let _ = tx.send(fsys::LoadMsg::Done(generation, None));
                    ctx.request_repaint();
                });
            }
            VMode::Trash => {
                let (tx, generation, cur, ctx) = (self.load_tx.clone(), self.generation, self.gen_shared.clone(), self.ctx.clone());
                let slot = self.trash_rows.clone();
                crate::vfs::thread::spawn(move || {
                    match extras::trash_list() {
                        Ok(rows) => {
                            let ents: Vec<fsys::Entry> = rows
                                .iter()
                                .map(|r| {
                                    let full = r.parent.join(&r.name).to_string_lossy().into_owned();
                                    let kind = if r.dir {
                                        Kind::Folder
                                    } else {
                                        match r.name.rsplit_once('.') {
                                            Some((st, e)) if !st.is_empty() => fsys::kind_of_ext(e),
                                            _ => Kind::Other,
                                        }
                                    };
                                    fsys::Entry {
                                        name: full.into_boxed_str(),
                                        size: r.size,
                                        mtime: r.time,
                                        kind,
                                        flags: if r.dir { fsys::F_DIR } else { 0 },
                                    }
                                })
                                .collect();
                            // sólo la carga vigente fija las filas (una anterior llegaría tarde)
                            if cur.load(Ordering::Relaxed) == generation {
                                *crate::thumbs::lock(&slot) = rows;
                            }
                            let _ = tx.send(fsys::LoadMsg::Chunk(generation, ents));
                            let _ = tx.send(fsys::LoadMsg::Done(generation, None));
                        }
                        Err(e) => {
                            let _ = tx.send(fsys::LoadMsg::Done(generation, Some(e)));
                        }
                    }
                    ctx.request_repaint();
                });
            }
            VMode::Search { query, all, content } => {
                let q = query.trim().to_string();
                if q.is_empty() {
                    self.loading = false;
                } else if content {
                    let roots = if all { tools::all_roots() } else { vec![path] };
                    self.search_st = Some(crate::contentsearch::spawn(roots, q, self.cfg.show_hidden, self.ctx.clone()));
                } else {
                    let indexed = if self.cfg.index_enabled {
                        index::spawn_search(if all { None } else { Some(path.clone()) }, q.clone(), self.cfg.show_hidden, self.ctx.clone())
                    } else {
                        None
                    };
                    self.search_st = Some(indexed.unwrap_or_else(|| {
                        let roots = if all { tools::all_roots() } else { vec![path] };
                        tools::spawn_search(roots, q, self.cfg.show_hidden, self.ctx.clone())
                    }));
                }
            }
        }
    }

    /// Pasa los resultados nuevos de la búsqueda en curso a la lista del panel.
    fn poll_search(&mut self) {
        let Some(st) = self.search_st.clone() else { return };
        let needle = self.filter.to_lowercase();
        // el `stat` de cada resultado se hace en otro hilo (en red puede tardar); aquí sólo se recoge
        tools::stat_pending(&st, 400, &self.ctx);
        let fresh: Vec<fsys::Entry> = std::mem::take(&mut *crate::thumbs::lock(&st.entries));
        let got = fresh.len();
        if got > 0 {
            let v = Arc::make_mut(&mut self.entries);
            let base = v.len();
            v.extend(fresh);
            let new: Vec<u32> = (base..v.len())
                .filter(|&i| self.passes(&self.entries[i], &needle))
                .map(|i| i as u32)
                .collect();
            self.order.extend(new);
            self.sel.resize(self.entries.len().div_ceil(64), 0);
            self.search_taken += got;
            self.ctx.request_repaint();
        }
        if tools::stat_finished(&st) && crate::thumbs::lock(&st.entries).is_empty() {
            // búsqueda por contenido en un servidor SFTP que dejó archivos sin examinar por los topes
            if let Some(m) = crate::contentsearch::take_note(&st) {
                self.say(m, false);
            }
            self.search_st = None;
            self.loading = false;
            self.spawn_sort();
            self.apply_pending();
        } else {
            self.loading = true;
            // queda un lote ya leído sin recoger (el hilo de `stat` pidió fotograma antes de que se recogiera)
            if !crate::thumbs::lock(&st.entries).is_empty() {
                self.ctx.request_repaint();
            }
        }
    }

    pub(super) fn navigate(&mut self, path: PathBuf) {
        // en Android «/storage/emulated» no se puede listar: el almacenamiento real es el usuario 0
        #[cfg(target_os = "android")]
        let path = if path == Path::new("/storage/emulated") { PathBuf::from("/storage/emulated/0") } else { path };
        // dirección de red (escrita, o guardada en favoritos/recientes/pestañas): se conecta, salvo
        // que sea de un servidor SFTP ya conectado: entonces se va a su carpeta espejo
        if let Some(u) = path.to_str().filter(|u| app_places::is_net_uri(u)).map(str::to_string) {
            if let Some(p) = sftp_mirror(&u, false) {
                self.navigate(p);
            } else if sftp_live_root(&u).is_some() {
                // carpeta aún sin reflejar: se busca por la red (si ya no está, se pide conectar)
                self.spawn_io(
                    {
                        let u = u.clone();
                        move || sftp_mirror(&u, true)
                    },
                    move |a, r| match r {
                        Some(p) => a.navigate(p),
                        None => a.say(tf("No existe: {}", &[&u]), true),
                    },
                );
            } else {
                self.connect_uri(u);
            }
            return;
        }
        if let Some(m) = virt_mode(&path) {
            if path == self.cwd && self.vmode == m {
                return;
            }
            self.push_back();
            self.preview = None;
            self.load_mode(path, false, m);
            self.scroll_to_top();
            return;
        }
        // sin esperar a una unidad de red caída: si no contesta enseguida se trata como carpeta y la
        // carga en segundo plano dirá si no se puede leer
        match quick_stat(&path) {
            Some((false, true)) => {
                self.open_ext(&path);
                return;
            }
            Some((false, false)) => {
                self.say(tf("No existe: {}", &[&path.display()]), true);
                return;
            }
            _ => {}
        }
        if path == self.cwd {
            return;
        }
        self.push_back();
        self.preview = None;
        self.load(path, false);
        self.scroll_to_top();
    }

    /// La vista actual entra en el historial «Atrás» (y se olvida «Adelante»).
    fn push_back(&mut self) {
        let snap = self.hist_snap();
        self.back.push(self.cwd.clone());
        self.back_snap.resize(self.back.len() - 1, None);
        self.back_snap.push(snap);
        self.fwd.clear();
        self.fwd_snap.clear();
    }

    /// Una búsqueda se recuerda entera (consulta y resultados, si ya terminó); una carpeta, sólo por su ruta.
    fn hist_snap(&self) -> HistSnap {
        if !matches!(self.vmode, VMode::Search { .. }) {
            return None;
        }
        let mut t = self.pane_snapshot();
        // sin historial dentro del historial
        t.back.clear();
        t.fwd.clear();
        Some(Box::new(t))
    }

    /// Saca el último paso de `paths` con su vista guardada (si las listas siguen alineadas).
    fn pop_hist(paths: &mut Vec<PathBuf>, snaps: &mut Vec<HistSnap>) -> Option<(PathBuf, HistSnap)> {
        let p = paths.pop()?;
        let snap = if snaps.len() == paths.len() + 1 { snaps.pop().flatten().filter(|t| t.cwd == p) } else { None };
        snaps.truncate(paths.len());
        Some((p, snap))
    }

    fn push_hist(paths: &mut Vec<PathBuf>, snaps: &mut Vec<HistSnap>, p: PathBuf, snap: HistSnap) {
        paths.push(p);
        snaps.resize(paths.len() - 1, None);
        snaps.push(snap);
    }

    /// Vuelve a un paso del historial: una búsqueda recupera su consulta y resultados.
    fn go_hist(&mut self, p: PathBuf, snap: HistSnap) {
        self.preview = None;
        match snap {
            Some(t) => {
                let mut t = *t;
                t.back = std::mem::take(&mut self.back);
                t.fwd = std::mem::take(&mut self.fwd);
                t.back_snap = std::mem::take(&mut self.back_snap);
                t.fwd_snap = std::mem::take(&mut self.fwd_snap);
                self.apply_pane(t);
            }
            None => {
                let from = self.came_from(&p);
                self.load(p, false);
                self.scroll_to_top();
                self.pending_select = from;
            }
        }
    }

    pub(super) fn go_back(&mut self) {
        if let Some((p, snap)) = Self::pop_hist(&mut self.back, &mut self.back_snap) {
            let cur = self.hist_snap();
            Self::push_hist(&mut self.fwd, &mut self.fwd_snap, self.cwd.clone(), cur);
            self.go_hist(p, snap);
        }
    }

    pub(super) fn go_forward(&mut self) {
        if let Some((p, snap)) = Self::pop_hist(&mut self.fwd, &mut self.fwd_snap) {
            let cur = self.hist_snap();
            Self::push_hist(&mut self.back, &mut self.back_snap, self.cwd.clone(), cur);
            self.go_hist(p, snap);
        }
    }

    /// Al volver a la carpeta que contiene la actual (Atrás/Adelante), ésta queda seleccionada (como «Subir»).
    fn came_from(&self, to: &Path) -> Vec<String> {
        match (self.cwd.parent(), self.cwd.file_name()) {
            (Some(par), Some(n)) if par == to && virt_mode(&self.cwd).is_none() => vec![n.to_string_lossy().into_owned()],
            _ => Vec::new(),
        }
    }

    pub(super) fn go_up(&mut self) {
        if let Some((root, archive)) = self.arch_of(&self.cwd) {
            if self.cwd == root {
                // salir del comprimido: carpeta que lo contiene, con el archivo seleccionado
                if let Some(dir) = archive.parent().map(|p| p.to_path_buf()) {
                    self.navigate(dir);
                    if let Some(n) = archive.file_name() {
                        self.pending_select = vec![n.to_string_lossy().into_owned()];
                    }
                }
                return;
            }
        }
        if virt_mode(&self.cwd).is_some() {
            // las ubicaciones virtuales no tienen carpeta superior: se vuelve al inicio
            if let Some(h) = self.home.clone() {
                self.navigate(h);
            }
            return;
        }
        if let Some(parent) = self.cwd.parent().map(|p| p.to_path_buf()) {
            let from = self.cwd.file_name().map(|n| n.to_string_lossy().into_owned());
            self.navigate(parent);
            if let Some(n) = from {
                self.pending_select = vec![n];
            }
        }
    }

    pub(super) fn refresh(&mut self) {
        if self.sel_count > 0 && self.sel_count < 5000 && self.pending_select.is_empty() {
            self.pending_select = self.selected_names();
        }
        let p = self.cwd.clone();
        self.load(p, true);
    }

    /// Recarga sin mover la vista: conserva el desplazamiento y el elemento del cursor (por nombre).
    pub(super) fn refresh_keep(&mut self) {
        let cur = self.cursor.and_then(|c| self.entries.get(c as usize)).map(|e| e.name.to_string());
        let off = self.kin_off;
        self.refresh();
        self.keep_scroll = Some(off);
        self.keep_cursor = cur;
    }

    pub(super) fn scroll_to_top(&mut self) {
        let id = egui::Id::new(("files", self.live_side));
        self.ctx.data_mut(|d| d.insert_temp(id, true));
    }

    /// Marca la configuración para guardarla en cuanto pase medio segundo sin cambios (`ui`) o al salir:
    /// un deslizador no escribe el archivo en cada fotograma.
    pub(super) fn save_cfg(&mut self) {
        let now = self.ctx.input(|i| i.time);
        self.cfg_save.mark(now);
        self.ctx.request_repaint_after(Duration::from_millis(600));
    }

    pub(super) fn say(&mut self, s: String, err: bool) {
        self.ux_say(&s, err);
        self.msg = Some((s, Instant::now(), err));
        self.ctx.request_repaint_after(Duration::from_secs(6));
    }

    pub(super) fn spawn_sort(&mut self) {
        // filtro u ocultos pueden haber cambiado: lo seleccionado que deja de verse se deselecciona ya
        self.prune_sel();
        self.sort_seq += 1;
        if self.loading {
            // con el listado a medias no se ordena (al terminar se hará): sólo se refiltra, sin que
            // un hilo comparta la lista mientras siguen llegando bloques
            let needle = self.filter.to_lowercase();
            self.order = (0..self.entries.len()).filter(|&i| self.passes(&self.entries[i], &needle)).map(|i| i as u32).collect();
            return;
        }
        self.sort_pending = true;
        let (seq, entries, tx, ctx) = (self.sort_seq, self.entries.clone(), self.sort_tx.clone(), self.ctx.clone());
        let spec = SortSpec {
            key: self.cfg.sort,
            asc: self.cfg.asc,
            show_hidden: self.cfg.show_hidden,
            filter: self.filter.clone(),
        };
        crate::vfs::thread::spawn(move || {
            let order = fsys::build_order(&entries, &spec);
            let _ = tx.send((seq, order));
            ctx.request_repaint();
        });
    }

    fn passes(&self, e: &Entry, needle: &str) -> bool {
        // en búsquedas el nombre es una ruta: el filtro mira sólo el nombre del archivo
        (self.cfg.show_hidden || !e.is_hidden()) && (needle.is_empty() || fsys::contains_ci(base_name(&e.name), needle))
    }

    pub(super) fn poll(&mut self) {
        self.poll_search();
        while let Ok((k, m)) = self.extra_rx.try_recv() {
            self.extra_pending.remove(&k);
            if self.extra_cache.len() > 4000 {
                self.extra_cache.clear();
            }
            self.extra_cache.insert(k, m);
        }
        let mut done = false;
        let needle = self.filter.to_lowercase();
        // los bloques de un fotograma se añaden de una vez: `make_mut` copia la lista entera si otro
        // hilo (un orden en curso) la comparte, y hacerlo por bloque sería cuadrático
        let mut got: Vec<Entry> = Vec::new();
        while let Ok(m) = self.load_rx.try_recv() {
            match m {
                LoadMsg::Chunk(g, chunk) if g == self.generation => {
                    #[cfg(feature = "selftest")]
                    if !self.traced_first {
                        self.traced_first = true;
                        if std::env::var_os("FB_SHOT").is_some() {
                            eprintln!("[trace] primer bloque ({} entradas): {:?}", chunk.len(), self.t_load.elapsed());
                        }
                    }
                    got.extend(chunk);
                }
                LoadMsg::Done(g, err) if g == self.generation => {
                    #[cfg(feature = "selftest")]
                    if std::env::var_os("FB_SHOT").is_some() {
                        eprintln!("[trace] listado completo ({} entradas): {:?}", self.entries.len(), self.t_load.elapsed());
                    }
                    self.loading = false;
                    self.load_err = err;
                    done = true;
                }
                LoadMsg::Warn(g, m) if g == self.generation => self.say(m, true),
                _ => {}
            }
        }
        if !got.is_empty() {
            let v = Arc::make_mut(&mut self.entries);
            let base = v.len();
            v.extend(got);
            let new: Vec<u32> = (base..v.len())
                .filter(|&i| self.passes(&self.entries[i], &needle))
                .map(|i| i as u32)
                .collect();
            self.order.extend(new);
            self.sel.resize(self.entries.len().div_ceil(64), 0);
        }
        if done {
            self.spawn_sort();
            self.apply_pending();
        }
        let mut latest = None;
        while let Ok((seq, order)) = self.sort_rx.try_recv() {
            if seq == self.sort_seq {
                latest = Some(order);
            }
        }
        if let Some(order) = latest {
            #[cfg(feature = "selftest")]
            if std::env::var_os("FB_SHOT").is_some() {
                eprintln!("[trace] ordenado ({} visibles): {:?}", order.len(), self.t_load.elapsed());
            }
            self.order = order;
            self.sort_pending = false;
            if let Some(c) = self.cursor.filter(|_| self.keep_scroll.is_none()) {
                self.scroll_to = Some(c);
            }
        }
    }

    fn apply_pending(&mut self) {
        if let Some(k) = self.keep_cursor.take() {
            // recarga que conserva la vista: el cursor vuelve a su elemento sin desplazar la lista
            let needle = self.filter.to_lowercase();
            let hit = self.entries.iter().position(|e| *e.name == *k && self.passes(e, &needle)).map(|i| i as u32);
            let keep_sel = std::mem::take(&mut self.pending_select);
            let names: HashSet<&str> = keep_sel.iter().map(|s| s.as_str()).collect();
            let hits: Vec<u32> = self.entries.iter().enumerate().filter(|(_, e)| names.contains(&*e.name) && self.passes(e, &needle)).map(|(i, _)| i as u32).collect();
            for i in hits {
                self.set_bit(i, true);
            }
            self.recount();
            self.cursor = hit;
            self.anchor = hit;
            if !self.pending_rename {
                return;
            }
        }
        if self.pending_select.is_empty() && !self.pending_rename {
            return;
        }
        let names: HashSet<&str> = self.pending_select.iter().map(|s| s.as_str()).collect();
        let needle = self.filter.to_lowercase();
        let mut first = None;
        let mut hits = Vec::new();
        for (i, e) in self.entries.iter().enumerate() {
            // sólo lo visible: no se selecciona algo que el filtro u «ocultos» esconden
            if names.contains(&*e.name) && self.passes(e, &needle) {
                hits.push(i as u32);
                first.get_or_insert(i as u32);
            }
        }
        for i in hits {
            self.set_bit(i, true);
        }
        self.recount();
        if let Some(f) = first {
            self.cursor = Some(f);
            self.anchor = Some(f);
            if self.keep_scroll.is_none() {
                self.scroll_to = Some(f);
            }
            if self.pending_rename {
                let n = self.entries[f as usize].name.to_string();
                if self.touch {
                    self.dlg = Dlg::Rename(n.clone(), base_name(&n).to_string());
                } else {
                    self.rename = Some((f, n, true));
                }
            }
        }
        self.pending_select.clear();
        self.pending_rename = false;
    }

    // -----------------------------------------------------------------------
    // Selección
    // -----------------------------------------------------------------------

    #[inline]
    pub(super) fn is_sel(&self, i: u32) -> bool {
        self.sel.get((i >> 6) as usize).map(|w| w >> (i & 63) & 1 == 1).unwrap_or(false)
    }

    pub(super) fn set_bit(&mut self, i: u32, on: bool) {
        let w = (i >> 6) as usize;
        if w >= self.sel.len() {
            self.sel.resize(w + 1, 0);
        }
        if on {
            self.sel[w] |= 1 << (i & 63);
        } else {
            self.sel[w] &= !(1 << (i & 63));
        }
    }

    pub(super) fn clear_sel(&mut self) {
        self.sel.iter_mut().for_each(|w| *w = 0);
        self.sel_count = 0;
        self.sel_size = 0;
    }

    /// Recorre sólo los seleccionados que están visibles (filtro y ocultos actuales): una acción nunca
    /// debe tocar algo que el usuario no ve, aunque su bit siga puesto.
    pub(super) fn for_each_selected(&self, mut f: impl FnMut(u32)) {
        let needle = self.filter.to_lowercase();
        for_each_bit(&self.sel, |i| {
            if self.entries.get(i as usize).map(|e| self.passes(e, &needle)).unwrap_or(false) {
                f(i);
            }
        });
    }

    /// Quita de la selección (y del cursor/ancla) lo que el filtro u ocultar archivos dejan fuera.
    fn prune_sel(&mut self) {
        let needle = self.filter.to_lowercase();
        let entries = self.entries.clone();
        let mut sel = std::mem::take(&mut self.sel);
        let vis = |i: u32| entries.get(i as usize).map(|e| self.passes(e, &needle)).unwrap_or(false);
        prune_bits(&mut sel, vis);
        let (cur_ok, anc_ok) = (self.cursor.map(vis).unwrap_or(true), self.anchor.map(vis).unwrap_or(true));
        let ren_ok = self.rename.as_ref().map(|r| vis(r.0)).unwrap_or(true);
        self.sel = sel;
        if !cur_ok {
            self.cursor = None;
        }
        if !anc_ok {
            self.anchor = None;
        }
        if !ren_ok {
            self.rename = None;
        }
        self.recount();
    }

    pub(super) fn recount(&mut self) {
        let (mut c, mut s) = (0usize, 0u64);
        let entries = self.entries.clone();
        self.for_each_selected(|i| {
            if let Some(e) = entries.get(i as usize) {
                c += 1;
                s += e.size;
            }
        });
        self.sel_count = c;
        self.sel_size = s;
    }

    pub(super) fn select_only(&mut self, i: u32) {
        self.clear_sel();
        self.set_bit(i, true);
        self.sel_count = 1;
        self.sel_size = self.entries[i as usize].size;
        self.cursor = Some(i);
        self.anchor = Some(i);
    }

    pub(super) fn pos_of(&self, i: u32) -> Option<usize> {
        self.order.iter().position(|&x| x == i)
    }

    pub(super) fn select_range(&mut self, a: usize, b: usize, additive: bool) {
        if !additive {
            self.clear_sel();
        }
        if self.order.is_empty() {
            return;
        }
        let (lo, hi) = (a.min(b), a.max(b));
        for p in lo..=hi.min(self.order.len() - 1) {
            let i = self.order[p];
            self.set_bit(i, true);
        }
        self.recount();
    }

    pub(super) fn selected_names(&self) -> Vec<String> {
        let mut v = Vec::new();
        self.for_each_selected(|i| {
            if let Some(e) = self.entries.get(i as usize) {
                v.push(e.name.to_string());
            }
        });
        v
    }

    pub(super) fn selected_paths(&self) -> Vec<PathBuf> {
        if self.vmode == VMode::Trash {
            return Vec::new(); // lo de la papelera no está en esas rutas: tiene sus propias acciones
        }
        self.selected_names().into_iter().map(|n| self.cwd.join(n)).collect()
    }

    pub(super) fn move_cursor(&mut self, delta: isize, extend: bool) {
        if self.order.is_empty() {
            return;
        }
        let cur = self.cursor.and_then(|c| self.pos_of(c));
        let target = match cur {
            None => 0usize,
            Some(p) => (p as isize + delta).clamp(0, self.order.len() as isize - 1) as usize,
        };
        let idx = self.order[target];
        if extend {
            let a = self.anchor.and_then(|a| self.pos_of(a)).unwrap_or(target);
            self.select_range(a, target, false);
            self.cursor = Some(idx);
        } else {
            self.select_only(idx);
        }
        self.scroll_to = Some(idx);
    }

    // ---- pestañas ----

    /// Estado del panel que está en los campos vivos de `App`.
    fn pane_snapshot(&self) -> TabState {
        let sel = if self.sel_count > 0 && self.sel_count < 2000 { self.selected_names() } else { Vec::new() };
        TabState {
            cwd: self.cwd.clone(),
            back: self.back.clone(),
            fwd: self.fwd.clone(),
            back_snap: self.back_snap.clone(),
            fwd_snap: self.fwd_snap.clone(),
            filter: self.filter.clone(),
            sel,
            split: None,
            side: 0,
            vmode: self.vmode.clone(),
            scroll: self.kin_off,
            sbuf: self.sbuf.clone(),
            results: (matches!(self.vmode, VMode::Search { .. }) && self.search_st.is_none() && !self.loading).then(|| self.entries.clone()),
        }
    }

    /// Guarda en la pestaña actual la vista completa (uno o dos paneles).
    pub(super) fn save_tab(&mut self) {
        let t = if self.other.is_some() {
            let live = self.live_side;
            self.ensure_live(0);
            let mut first = self.pane_snapshot();
            self.ensure_live(1);
            first.split = Some(Box::new(self.pane_snapshot()));
            first.side = self.active_side;
            self.ensure_live(live);
            first
        } else {
            self.pane_snapshot()
        };
        if let Some(slot) = self.tabs.get_mut(self.cur_tab) {
            *slot = t;
        }
    }

    /// Carga en el panel vivo el estado guardado.
    fn apply_pane(&mut self, t: TabState) {
        self.back = t.back;
        self.fwd = t.fwd;
        self.back_snap = t.back_snap;
        self.fwd_snap = t.fwd_snap;
        self.filter = t.filter;
        self.sbuf = t.sbuf;
        match (t.vmode.clone(), t.results) {
            // búsqueda ya terminada: se vuelven a poner sus resultados sin lanzarla otra vez
            (VMode::Search { all, content, .. }, Some(res)) => {
                self.load_mode(t.cwd, true, VMode::Search { query: String::new(), all, content });
                self.vmode = t.vmode;
                self.sel = vec![0; res.len().div_ceil(64)];
                self.entries = res;
                self.pending_select = t.sel;
                self.keep_scroll = Some(t.scroll);
                self.spawn_sort();
                self.apply_pending();
            }
            _ => {
                self.load_mode(t.cwd, true, t.vmode);
                self.pending_select = t.sel;
                // se vuelve a donde estaba la lista (cuando termine de cargar)
                self.keep_scroll = Some(t.scroll);
            }
        }
    }

    fn restore_tab(&mut self, i: usize) {
        let mut t = self.tabs[i].clone();
        self.cur_tab = i;
        self.preview = None;
        self.addr_edit = None;
        let split = t.split.take();
        let side = t.side.min(1);
        match split {
            Some(s) => {
                if self.other.is_none() {
                    self.open_split(s.cwd.clone());
                }
                self.ensure_live(0);
                self.apply_pane(t);
                self.ensure_live(1);
                self.apply_pane(*s);
                self.active_side = side;
                self.ensure_live(side);
                self.cfg.split = true;
            }
            None => {
                if self.other.is_some() {
                    self.ensure_live(0);
                    self.other = None;
                    self.active_side = 0;
                    self.live_side = 0;
                    self.cfg.split = false;
                }
                self.apply_pane(t);
            }
        }
        self.cfg.last_dir = Some(self.cwd.clone());
    }

    pub(super) fn switch_tab(&mut self, i: usize) {
        if i == self.cur_tab || i >= self.tabs.len() {
            return;
        }
        self.save_tab();
        self.restore_tab(i);
    }

    pub(super) fn step_tab(&mut self, d: isize) {
        let n = self.tabs.len() as isize;
        if n > 1 {
            self.switch_tab(((self.cur_tab as isize + d).rem_euclid(n)) as usize);
        }
    }

    pub(super) fn new_tab_bg(&mut self, path: PathBuf) {
        self.save_tab();
        let at = self.cur_tab + 1;
        self.tabs.insert(at, TabState::new(path));
    }

    pub(super) fn reopen_tab(&mut self) {
        if let Some(t) = self.closed_tabs.pop() {
            self.save_tab();
            let at = self.cur_tab + 1;
            self.tabs.insert(at, t);
            self.restore_tab(at);
        }
    }

    pub(super) fn new_window(&self) {
        self.new_window_at(&self.cwd);
    }

    pub(super) fn new_window_at(&self, dir: &Path) {
        if let Ok(exe) = std::env::current_exe() {
            // la ventana nueva se abre desplazada para que no tape exactamente a la actual
            let pos = self.ctx.input(|i| i.viewport().outer_rect).map(|r| format!("{},{}", r.min.x + 40.0, r.min.y + 40.0));
            let mut cmd = std::process::Command::new(exe);
            cmd.arg("--new-window").arg(dir);
            if let Some(p) = pos {
                cmd.env("FB_POS", p);
            }
            if let Ok(mut ch) = cmd.spawn() {
                crate::vfs::thread::spawn(move || {
                    let _ = ch.wait();
                });
            }
        }
    }

    /// Arrastre del sistema: al sacar la selección de la ventana pasa a ser un arrastre hacia otras aplicaciones.
    pub(super) fn drag_out_tick(&mut self, ctx: &egui::Context, frame: &eframe::Frame) {
        if self.dragout.is_none() && !self.dragout_tried && !self.touch {
            self.dragout_tried = true;
            self.dragout = crate::dragout::DragOut::new(frame, ctx);
        }
        let (pos, down) = ctx.input(|i| (i.pointer.latest_pos(), i.pointer.primary_down()));
        if pos.is_some() {
            self.last_ptr = pos;
        }
        let outside = pos.map(|p| !ctx.content_rect().contains(p)).unwrap_or(true);
        let want = self.drag_items && down && outside && self.sel_count > 0;
        let paths = if want { self.selected_paths() } else { Vec::new() };
        // de un servidor SFTP: primero se descargan (si no, se soltarían marcadores vacíos)
        if want && self.dragout.is_some() && self.fetch_first(&paths, app_places::FetchThen::Drag) {
            self.drag_items = false;
            return;
        }
        let Some(d) = self.dragout.as_mut() else { return };
        d.pump();
        let mut dropped = d.take_dropped();
        let done = d.take_done();
        let mut started = false;
        if want && !paths.is_empty() && d.start(&paths) {
            started = true;
        }
        if d.active() {
            ctx.request_repaint_after(Duration::from_millis(10));
        }
        self.ext_dropped.append(&mut dropped);
        if started {
            self.drag_items = false;
            self.bm_drop = false;
            self.side_drop = None;
            self.release_inject = true;
            ctx.request_repaint();
        }
        if done {
            self.release_inject = true;
            self.refresh();
            ctx.request_repaint();
        }
        if !self.ext_dropped.is_empty() {
            ctx.request_repaint();
        }
    }

    /// Abre lo que pidan otras invocaciones: carpetas en pestañas nuevas, archivos en su carpeta.
    pub(super) fn drain_inbox(&mut self, frame: &eframe::Frame) {
        let items: Vec<PathBuf> = std::mem::take(&mut *crate::thumbs::lock(&self.inbox));
        if items.is_empty() {
            return;
        }
        let token = crate::system::take_activation_token();
        // por carpeta, en orden de llegada: los archivos de una misma carpeta van juntos y seleccionados
        let mut groups: Vec<(PathBuf, Vec<String>)> = Vec::new();
        for p in items {
            let (dir, name) = if p.v_is_dir() {
                (p, None)
            } else if let (Some(parent), Some(name)) = (p.parent(), p.file_name()) {
                (parent.to_path_buf(), Some(name.to_string_lossy().into_owned()))
            } else {
                continue;
            };
            let g = match groups.iter().position(|g| g.0 == dir) {
                Some(g) => g,
                None => {
                    groups.push((dir, Vec::new()));
                    groups.len() - 1
                }
            };
            if let Some(n) = name {
                if !groups[g].1.contains(&n) {
                    groups[g].1.push(n);
                }
            }
        }
        // la carpeta de la pestaña actual, si viene, al final: es la que queda a la vista
        if let Some(i) = groups.iter().position(|g| self.other.is_none() && g.0 == self.cwd) {
            let g = groups.remove(i);
            groups.push(g);
        }
        let n = groups.len();
        let mut ins = self.cur_tab + 1;
        for (k, (dir, names)) in groups.into_iter().enumerate() {
            // otra pestaña (sin vista dividida) que ya muestra esa carpeta: se reutiliza
            let open = self.tabs.iter().enumerate().position(|(i, t)| i != self.cur_tab && t.split.is_none() && t.cwd == dir);
            if k + 1 < n {
                // las demás quedan en segundo plano con su selección, que se aplica al abrirlas
                match open {
                    Some(i) if !names.is_empty() => self.tabs[i].sel = names,
                    Some(_) => {}
                    None => {
                        let mut t = TabState::new(dir);
                        t.sel = names;
                        self.tabs.insert(ins, t);
                        ins += 1;
                    }
                }
                continue;
            }
            if self.other.is_none() && dir == self.cwd {
                if !names.is_empty() {
                    self.clear_sel();
                    self.pending_select = names;
                    self.refresh();
                }
                continue;
            }
            match open {
                Some(i) => self.switch_tab(i),
                None => {
                    self.save_tab();
                    self.tabs.insert(ins, TabState::new(dir));
                    self.restore_tab(ins);
                }
            }
            if !names.is_empty() {
                self.pending_select = names;
            }
        }
        self.ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        // Wayland no deja que una ventana se ponga delante sola: hace falta el token de activación de
        // quien nos llama (xdg-activation, que winit no expone). Sin token o sin el protocolo, al
        // menos se pide atención (resaltado en la barra de tareas).
        #[cfg(all(unix, not(any(target_os = "macos", target_os = "android"))))]
        let activated = token.is_some_and(|t| crate::wl_activate::activate(frame, &t));
        #[cfg(not(all(unix, not(any(target_os = "macos", target_os = "android")))))]
        let activated = {
            let _ = (frame, token);
            false
        };
        if !activated && std::env::var_os("WAYLAND_DISPLAY").is_some() {
            self.ctx.send_viewport_cmd(egui::ViewportCommand::RequestUserAttention(egui::UserAttentionType::Informational));
        }
    }

    pub(super) fn new_tab(&mut self, path: PathBuf) {
        self.save_tab();
        let at = self.cur_tab + 1;
        self.tabs.insert(at, TabState::new(path));
        self.restore_tab(at);
    }

    pub(super) fn close_tab(&mut self, i: usize) {
        if self.tabs.len() <= 1 || i >= self.tabs.len() {
            return;
        }
        if i == self.cur_tab {
            self.save_tab();
            let t = self.tabs.remove(i);
            self.closed_tabs.push(t);
            if self.closed_tabs.len() > 20 {
                self.closed_tabs.remove(0);
            }
            let n = i.min(self.tabs.len() - 1);
            self.restore_tab(n);
        } else {
            self.save_tab();
            let t = self.tabs.remove(i);
            self.closed_tabs.push(t);
            if self.closed_tabs.len() > 20 {
                self.closed_tabs.remove(0);
            }
            if i < self.cur_tab {
                self.cur_tab -= 1;
            }
        }
    }

    fn close_other_tabs(&mut self, keep: usize) {
        if keep != self.cur_tab {
            self.switch_tab(keep);
        }
        self.save_tab();
        let t = self.tabs[self.cur_tab].clone();
        // las cerradas se pueden reabrir (Ctrl+Mayús+T), como al cerrar una a una
        let cur = self.cur_tab;
        for (i, o) in std::mem::take(&mut self.tabs).into_iter().enumerate() {
            if i != cur {
                self.closed_tabs.push(o);
            }
        }
        if self.closed_tabs.len() > 20 {
            let extra = self.closed_tabs.len() - 20;
            self.closed_tabs.drain(..extra);
        }
        self.tabs = vec![t];
        self.cur_tab = 0;
    }

    pub(super) fn tab_title(&self, i: usize) -> String {
        let name = |p: &PathBuf| self.dir_label(p);
        let mode = if i == self.cur_tab { &self.vmode } else { &self.tabs[i].vmode };
        match mode {
            VMode::Search { query, all, .. } => {
                let scope = if *all { t("todo el equipo").to_string() } else if i == self.cur_tab { name(&self.cwd) } else { name(&self.tabs[i].cwd) };
                return if query.trim().is_empty() { format!("{} · {}", t("Buscar"), scope) } else { format!("«{}» · {}", query.trim(), scope) };
            }
            VMode::Deep => {
                let base = if i == self.cur_tab { name(&self.cwd) } else { name(&self.tabs[i].cwd) };
                return format!("{base} {}", t("(todo)"));
            }
            VMode::Recent => return t("Recientes").to_string(),
            VMode::Trash => return t("Papelera").to_string(),
            VMode::Dir => {}
        }
        if i == self.cur_tab {
            match self.other.as_deref() {
                Some(o) => {
                    let (a, b) = if self.live_side == 0 { (&self.cwd, &o.cwd) } else { (&o.cwd, &self.cwd) };
                    format!("{} | {}", name(a), name(b))
                }
                None => name(&self.cwd),
            }
        } else {
            let tab = &self.tabs[i];
            match &tab.split {
                Some(s) => format!("{} | {}", name(&tab.cwd), name(&s.cwd)),
                None => name(&tab.cwd),
            }
        }
    }

    pub(super) fn tab_bar(&mut self, ui: &mut egui::Ui, max_w: f32) {
        let pal = self.pal;
        let n = self.tabs.len();
        let h = 34.0;
        let plus_w = 32.0;
        let tab_w = ((max_w - plus_w - 8.0) / n as f32 - 4.0).clamp(90.0, 210.0);
        // solo el ancho necesario: el resto de la barra queda libre para arrastrar la ventana
        let need = (n as f32 * (tab_w + 4.0) + plus_w + 8.0).min(max_w);
        let (rect, _) = ui.allocate_exact_size(vec2(need, h), Sense::hover());
        let p = ui.painter().clone();
        let mut switch = None;
        let mut close = None;
        let mut dup = None;
        let mut others = None;
        let font = FontId::proportional(13.0);
        for i in 0..n {
            let r = Rect::from_min_size(pos2(rect.left() + i as f32 * (tab_w + 4.0), rect.top() + 3.0), vec2(tab_w, h - 6.0));
            let id = ui.id().with(("tab", i));
            let resp = ui.interact(r, id, Sense::click());
            let active = i == self.cur_tab;
            resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, true, active, self.tab_title(i)));
            let fill = if active {
                pal.sel
            } else if resp.hovered() || ui.input(|i| i.pointer.hover_pos()).map_or(false, |q| r.contains(q)) {
                pal.hover
            } else {
                pal.surface.gamma_multiply(0.55)
            };
            p.rect_filled(r, CornerRadius::same(9), fill);
            if active {
                // contorno ≥ 3:1 (antes el acento a media opacidad): la pestaña activa no depende del tinte
                p.rect_stroke(r, CornerRadius::same(9), Stroke::new(1.0, a11y::sel_ring(&pal)), egui::StrokeKind::Inside);
            }
            theme::focus_ring(ui, &pal, &resp, 9.0);
            let can_close = n > 1;
            // botón de cerrar de al menos `MIN_TARGET` (antes 20×20), sin pasar del alto de la pestaña
            let xs = theme::target_size(vec2(20.0, 20.0));
            let xs = vec2(xs.x.min(32.0), xs.y.min(r.height()));
            let tx_w = tab_w - if can_close { 34.0 + xs.x } else { 28.0 };
            let mut title = self.tab_title(i);
            let gl = p.layout_no_wrap(title.clone(), font.clone(), pal.text);
            if gl.size().x > tx_w {
                let keep = ((tx_w / gl.size().x) * title.chars().count() as f32) as usize;
                title = title.chars().take(keep.saturating_sub(1)).collect::<String>() + "…";
            }
            let col = if active { pal.text } else { pal.subtle };
            let tex = self.tab_folder_tex(i).or_else(|| if self.cfg.sys_icons { self.sys.folder(&self.ctx, None) } else { None });
            match tex {
                Some(t) => {
                    let ir = Rect::from_center_size(pos2(r.left() + 16.0, r.center().y), vec2(16.0, 16.0));
                    p.image(t, ir, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
                }
                None => theme::draw_folder(&p, Rect::from_center_size(pos2(r.left() + 16.0, r.center().y), vec2(15.0, 15.0))),
            }
            p.text(pos2(r.left() + 30.0, r.center().y), Align2::LEFT_CENTER, title, font.clone(), col);
            let over = ui.input(|i| i.pointer.hover_pos()).map_or(false, |q| r.contains(q));
            if can_close && (active || over || self.touch) {
                // no depende de `resp.hovered()` de la pestaña: deja de estar «hovered» al pasar sobre la X,
                // lo que hacía parpadear el botón y perder los clics en pestañas inactivas
                let xr = Rect::from_center_size(pos2(r.right() - 4.0 - xs.x / 2.0, r.center().y), xs);
                let xresp = ui.interact(xr, id.with("x"), Sense::click()).on_hover_text(commands::with_shortcut(t("Cerrar pestaña"), Cmd::CloseTab));
                xresp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, t("Cerrar pestaña")));
                if xresp.hovered() {
                    p.rect_filled(xr.shrink(2.0), CornerRadius::same(6), pal.hover);
                }
                theme::focus_ring(ui, &pal, &xresp, 6.0);
                theme::draw_glyph(&p, xr.center(), 10.0, Glyph::Close, pal.subtle);
                if xresp.clicked() {
                    close = Some(i);
                }
            }
            if resp.clicked() && close.is_none() {
                switch = Some(i);
            }
            if resp.middle_clicked() {
                close = Some(i);
            }
            resp.context_menu(|ui| {
                if mi(ui, t("Duplicar pestaña"), "", true) {
                    dup = Some(i);
                }
                if mi(ui, t("Cerrar pestaña"), &commands::shortcut_text(Cmd::CloseTab), n > 1) {
                    close = Some(i);
                }
                if mi(ui, t("Cerrar las demás"), "", n > 1) {
                    others = Some(i);
                }
            });
        }
        let pr = Rect::from_min_size(pos2(rect.left() + n as f32 * (tab_w + 4.0) + 2.0, rect.top() + 3.0), vec2(28.0, 28.0));
        let presp = ui.interact(pr, ui.id().with("tab_plus"), Sense::click());
        presp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, t("Nueva pestaña")));
        p.rect_filled(pr, CornerRadius::same(9), if presp.hovered() { pal.hover } else { pal.surface.gamma_multiply(0.55) });
        theme::focus_ring(ui, &pal, &presp, 9.0);
        theme::draw_glyph(&p, pr.center(), 15.0, Glyph::Plus, pal.subtle);
        if presp.on_hover_text(commands::with_shortcut(t("Nueva pestaña"), Cmd::NewTab)).clicked() {
            self.new_tab(self.cwd.clone());
        }
        if let Some(i) = close {
            self.close_tab(i);
        } else if let Some(i) = dup {
            let p = if i == self.cur_tab { self.cwd.clone() } else { self.tabs[i].cwd.clone() };
            self.new_tab(p);
        } else if let Some(i) = others {
            self.close_other_tabs(i);
        } else if let Some(i) = switch {
            self.switch_tab(i);
        }
    }
}

/// `(es carpeta, existe)` sin colgar la interfaz: el `stat` va en un hilo y se espera poco. En una
/// unidad de red que no contesta devuelve None (quien llama sigue como si fuera una carpeta y la carga
/// en segundo plano dará el error).
/// ¿Se puede restaurar al arrancar? (carpeta que existe, que no contesta enseguida, o dirección de red)
pub(super) fn restorable(p: &Path) -> bool {
    p.to_str().is_some_and(app_places::is_net_uri) || quick_stat(p).is_none_or(|s| s.0)
}

/// Raíz del espejo del servidor SFTP conectado al que se refiere `uri` («sftp://usuario@servidor:puerto/ruta»,
/// como la da `sftp::persist_path`) y el resto de la ruta (codificada igual).
fn sftp_live_root(uri: &str) -> Option<(PathBuf, String)> {
    let rest = uri.strip_prefix("sftp://")?;
    crate::sftp::live_sessions().into_iter().find_map(|(a, root)| {
        let tail = rest.strip_prefix(a.as_str())?;
        (tail.is_empty() || tail.starts_with('/')).then(|| (root, tail.to_string()))
    })
}

/// Ruta en el espejo local de una dirección sftp:// de un servidor conectado. Sin `sync` sólo mira lo ya
/// reflejado (rápido, sin red); con `sync` refleja por el camino lo que falte (bloquea: usar en un hilo).
pub(super) fn sftp_mirror(uri: &str, sync: bool) -> Option<PathBuf> {
    let (mut cur, tail) = sftp_live_root(uri)?;
    let mut label = crate::sftp::remote_label(&cur)?;
    for comp in tail.split('/').filter(|c| !c.is_empty()) {
        let want = format!("{}/{comp}", label.trim_end_matches('/'));
        let is = |p: &Path| crate::sftp::remote_label(p).as_deref() == Some(want.as_str());
        // casi siempre el nombre local es el mismo; si no (caracteres no admitidos, «~2»…), se busca
        let find = |d: &Path| {
            let direct = d.join(comp);
            if direct.v_exists() && is(&direct) {
                return Some(direct);
            }
            crate::vfs::fs::read_dir(d).ok()?.flatten().map(|e| e.path()).find(|p| is(p))
        };
        cur = match find(&cur) {
            Some(p) => p,
            None if sync => {
                crate::sftp::sync_dir(&cur).ok()?;
                find(&cur)?
            }
            None => return None,
        };
        label = want;
    }
    Some(cur)
}

pub(super) fn quick_stat(path: &Path) -> Option<(bool, bool)> {
    if crate::vfs::WEB {
        return Some((path.v_is_dir(), path.v_exists()));
    }
    let (tx, rx) = channel();
    let p = path.to_path_buf();
    crate::vfs::thread::spawn(move || {
        let d = p.v_is_dir();
        let _ = tx.send((d, d || p.v_exists()));
    });
    rx.recv_timeout(Duration::from_millis(300)).ok()
}

impl App {
    /// Destino de lo soltado desde fuera: la carpeta (fila) bajo el puntero, la carpeta del otro panel o la
    /// actual. winit no siempre da la posición durante un arrastre del sistema: entonces, la carpeta actual.
    pub(super) fn ext_drop_dst(&mut self, ctx: &egui::Context) -> PathBuf {
        let row = self.hover_dir.take();
        let Some(q) = ctx.pointer_hover_pos() else { return self.cwd.clone() };
        if let Some(d) = row {
            return d;
        }
        if let Some(o) = self.other.as_deref() {
            if self.pane_rects[1 - self.live_side as usize].contains(q) {
                return o.cwd.clone();
            }
        }
        self.cwd.clone()
    }
}
