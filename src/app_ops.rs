//! Operaciones de archivos (abrir, copiar, pegar, renombrar, papelera…), deshacer/rehacer y su progreso.
use super::*;

impl App {
    // -----------------------------------------------------------------------
    // Operaciones
    // -----------------------------------------------------------------------

    /// Botón «Atrás» (Android): cierra lo que haya encima; si no, vuelve atrás o sube de carpeta.
    pub(super) fn on_back_key(&mut self) {
        if self.preview.is_some() {
            self.preview = None;
        } else if !matches!(self.dlg, Dlg::None) {
            self.dlg = Dlg::None;
        } else if self.sel_mode {
            self.sel_mode = false;
            self.clear_sel();
        } else if self.addr_edit.is_some() {
            self.addr_edit = None;
        } else if self.rename.is_some() {
            self.rename = None;
        } else if virt_mode(&self.cwd).is_some() {
            if !self.back.is_empty() {
                self.run_cmd(Cmd::Back);
            } else if self.tabs.len() > 1 {
                self.close_tab(self.cur_tab);
            } else {
                self.run_cmd(Cmd::Home);
            }
        } else if self.vmode != VMode::Dir && self.tabs.len() > 1 {
            self.close_tab(self.cur_tab);
        } else if self.vmode != VMode::Dir {
            self.set_deep(false);
        } else if !self.filter.is_empty() {
            self.filter.clear();
            self.spawn_sort();
        } else if !self.back.is_empty() {
            self.run_cmd(Cmd::Back);
        } else if self.tabs.len() > 1 {
            self.close_tab(self.cur_tab);
        } else if self.cwd.parent().is_some() && self.cwd != PathBuf::from("/storage/emulated/0") {
            self.run_cmd(Cmd::Up);
        }
    }

    /// Anota un archivo abierto en la lista de recientes.
    fn note_recent(&mut self, p: &Path) {
        // abierto desde dentro de un comprimido: otra aplicación puede seguir usando su extracción
        if let Some((root, _)) = self.arch_of(p) {
            self.arch_ext.insert(root);
        }
        self.cfg.recent.retain(|r| r != p);
        self.cfg.recent.insert(0, p.to_path_buf());
        self.cfg.recent.truncate(40);
        self.save_cfg();
    }

    pub(super) fn open_index(&mut self, i: u32) {
        if self.vmode == VMode::Trash {
            return;
        }
        let Some(e) = self.entries.get(i as usize) else { return };
        let p = self.cwd.join(&*e.name);
        if e.is_dir() && cfg!(target_os = "macos") && p.extension().map(|x| x == "app").unwrap_or(false) {
            fsys::open_external(&p);
        } else if e.is_dir() {
            self.navigate(p);
        } else if crate::archive::detect(&p).is_some() {
            self.note_recent(&p);
            self.open_archive(p);
        } else if is_program(&p) {
            // ejecutar un archivo es una decisión explícita: se pregunta (el menú contextual «Ejecutar» no)
            self.confirm_run = Some(p);
        } else if self.touch && matches!(e.kind, Kind::Image | Kind::Text | Kind::Doc | Kind::Audio | Kind::Video) {
            self.note_recent(&p);
            self.open_preview_at(i);
        } else {
            self.note_recent(&p);
            self.open_ext(&p);
        }
    }

    pub(super) fn do_copy(&mut self, cut: bool) {
        let v = self.selected_paths();
        if v.is_empty() {
            return;
        }
        // de un servidor SFTP: al portapapeles del sistema cuando estén descargados (no los marcadores vacíos)
        if self.fetch_first(&v, app_places::FetchThen::Clip(v.clone(), cut)) {
            self.clip_sys = false;
            self.clipboard = Some((v, cut));
            self.clip_hidden = false;
            return;
        }
        // Portapapeles de archivos del sistema si hay herramienta (wl-copy/xclip); si no, texto con rutas.
        self.clip_sys = tools::clip_set_files(&v, cut);
        if !self.clip_sys {
            let text = v.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join("\n");
            self.ctx.copy_text(text);
        }
        self.say(crate::i18n::tn("{} {} elemento|{} {} elementos", v.len(), &[&(if cut { t("Cortado:") } else { t("Copiado:") }), &v.len()]), false);
        self.clipboard = Some((v, cut));
        self.clip_hidden = false;
    }

    pub(super) fn do_paste(&mut self) {
        // Si lo del portapapeles del sistema es lo que cortamos nosotros, se conserva el «cortar»
        // (macOS no lo transporta en el portapapeles).
        let own = self.clipboard.clone();
        // si lo copiado llegó al portapapeles del sistema y ahora éste no tiene archivos, es que se ha
        // copiado otra cosa después: no se pega lo nuestro, que ya es viejo
        let own_ok = !self.clip_sys;
        let dst = self.cwd.clone();
        // leer el portapapeles del sistema puede lanzar wl-paste/xclip: no se hace en el hilo de la interfaz
        self.spawn_io(tools::clip_get_files, move |a, sys| {
            let picked = match (sys, own) {
                (Some((sp, _)), Some((op, true))) if sp == op => Some((op, true)),
                (Some(sys), _) => Some(sys),
                (None, own) => own.filter(|_| own_ok),
            };
            let Some((srcs, cut)) = picked.filter(|(v, _)| !v.is_empty()) else {
                a.say(t("Nada que pegar").into(), false);
                return;
            };
            a.start_copy(srcs, dst, cut);
            if cut {
                a.clipboard = None;
            }
        });
    }

    pub(super) fn do_trash(&mut self) {
        let v = self.selected_paths();
        if v.is_empty() {
            return;
        }
        if !extras::trash_supported() {
            // sin papelera (Android, navegador): se pide confirmar el borrado definitivo (antes que
            // «¿Mover a la papelera?», que después fallaría)
            self.ux.del_note = ux::DelNote::NoTrash;
            self.confirm_delete = Some(v);
            return;
        }
        if self.cfg.confirm_trash {
            self.dlg = Dlg::TrashAsk(v);
            return;
        }
        self.start_op(Op::Trash(v), OpMeta::Trash { replay: false });
    }

    pub(super) fn start_rename(&mut self) {
        if self.sel_count > 1 {
            self.open_batch();
            return;
        }
        if let Some(c) = self.cursor.filter(|&c| self.is_sel(c)) {
            if let Some(e) = self.entries.get(c as usize) {
                if self.touch {
                    self.dlg = Dlg::Rename(e.name.to_string(), base_name(&e.name).to_string());
                } else {
                    self.rename = Some((c, base_name(&e.name).to_string(), true));
                }
            }
        }
    }

    pub(super) fn commit_rename(&mut self, idx: u32, new: String) {
        if self.remote_ro(&self.cwd.clone()) {
            return;
        }
        let Some(e) = self.entries.get(idx as usize) else { return };
        let old_full = e.name.to_string();
        let old = base_name(&old_full).to_string();
        let new = new.trim().to_string();
        if new.is_empty() || new == old {
            return;
        }
        if new == "." || new == ".." || new.contains(['/', '\\']) || new.len() > 255 || new.chars().any(|c| c.is_control()) || (cfg!(windows) && bad_name(&new)) {
            self.say(t("Nombre no válido").into(), true);
            // el editor vuelve con lo escrito para corregirlo
            self.ux_rename_again(idx, &old_full, new);
            return;
        }
        let from = self.cwd.join(&old_full);
        let to = from.with_file_name(&new);
        let case_only = new.to_lowercase() == old.to_lowercase();
        let sel = format!("{}{}", &old_full[..old_full.len() - old.len()], new);
        self.say(t("Renombrando…").into(), false);
        self.spawn_io(
            {
                let (f, t2) = (from.clone(), to.clone());
                move || {
                    // `rename` sobrescribe el destino en silencio: no se pisa un elemento que ya existe
                    // en un sistema que distingue mayúsculas, «foo» y «Foo» pueden ser archivos distintos
                    // (FAT/exFAT/SMB en Linux: «Foo.txt» encuentra el propio «foo.txt»; se mira el inodo)
                    let same_file = case_only && fsys::case_alias(&f, &t2);
                    if same_file {
                        return fsys::rename_case(&f, &t2);
                    }
                    if crate::vfs::fs::symlink_metadata(&t2).is_ok() {
                        return Err(std::io::Error::new(std::io::ErrorKind::AlreadyExists, t("Ya existe un elemento con ese nombre")));
                    }
                    crate::vfs::fs::rename(&f, &t2)
                }
            },
            move |a, r| match r {
                Ok(()) => {
                    a.msg = None;
                    a.push_undo(Undo::Rename { from, to }, true);
                    a.pending_select = vec![sel];
                    a.refresh();
                }
                Err(e) => {
                    a.say(tf("No se pudo renombrar: {e}", &[&e]), true);
                    a.ux_rename_again(idx, &old_full, new);
                }
            },
        );
    }

    pub(super) fn new_folder(&mut self) {
        if self.remote_ro(&self.cwd.clone()) {
            return;
        }
        let (cwd, name) = (self.cwd.clone(), t("Nueva carpeta").to_string());
        self.say(t("Creando carpeta…").into(), false);
        self.spawn_io(
            move || {
                let p = fsys::unique_path(&cwd, &name);
                crate::vfs::fs::create_dir(&p).map(|_| p)
            },
            |a, r| match r {
                Ok(p) => {
                    a.msg = None;
                    a.push_undo(Undo::Created(vec![(p.clone(), true)]), true);
                    if p.parent() == Some(a.cwd.as_path()) {
                        a.pending_select = vec![p.file_name().unwrap().to_string_lossy().into_owned()];
                        a.pending_rename = true;
                        a.refresh();
                    }
                }
                Err(e) => a.say(tf("No se pudo crear la carpeta: {e}", &[&e]), true),
            },
        );
    }

    /// Los servidores SFTP se reflejan en una carpeta de caché de sólo lectura: nada se escribe ahí.
    pub(super) fn remote_ro(&mut self, p: &Path) -> bool {
        if virt_mode(p).is_some() {
            self.say(t("Esta ubicación es de sólo lectura").into(), true);
            true
        } else if crate::sftp::is_remote(p) {
            self.say(t("Los servidores SFTP son de sólo lectura").into(), true);
            true
        } else if self.arch_of(p).is_some() {
            self.say(t("Esta ubicación es de sólo lectura").into(), true);
            true
        } else {
            false
        }
    }

    pub(super) fn start_op(&mut self, op: Op, meta: OpMeta) {
        let remote = |v: &Vec<PathBuf>| v.iter().any(|p| crate::sftp::is_remote(p));
        let inarch = |p: &Path| self.arch_of(p).is_some();
        let touches = match &op {
            Op::Copy { srcs, dst, mv, .. } => {
                virt_mode(dst).is_some() || crate::sftp::is_remote(dst) || inarch(dst) || (*mv && (remote(srcs) || srcs.iter().any(|p| inarch(p))))
            }
            Op::Trash(v) | Op::Delete(v) => remote(v) || v.iter().any(|p| inarch(p)),
            Op::Compress { srcs, out, .. } => crate::sftp::is_remote(out) || remote(srcs) || inarch(out) || virt_mode(out).is_some(),
            Op::Unzip { src, dst, .. } => crate::sftp::is_remote(dst) || crate::sftp::is_remote(src) || inarch(dst) || virt_mode(dst).is_some(),
            Op::Fetch(_) => false,
        };
        if touches {
            // el aviso de SFTP sólo si de verdad hay un servidor SFTP de por medio
            let sftp = match &op {
                Op::Copy { srcs, dst, mv, .. } => crate::sftp::is_remote(dst) || (*mv && remote(srcs)),
                Op::Trash(v) | Op::Delete(v) => remote(v),
                Op::Compress { srcs, out, .. } => crate::sftp::is_remote(out) || remote(srcs),
                Op::Unzip { src, dst, .. } => crate::sftp::is_remote(dst) || crate::sftp::is_remote(src),
                Op::Fetch(_) => false,
            };
            self.say(ux::ro_text(sftp).into(), true);
            return;
        }
        if self.busy() {
            self.queue.push_back((op, meta));
            return;
        }
        // el nombre libre de lo que se genera se decide al empezar, no al encolar: otra tarea de la cola
        // puede haberlo ocupado, y lo que se limpie si falla debe ser sólo lo creado por esta
        let op = match (op, &meta) {
            (Op::Unzip { src, dst, pass }, OpMeta::Generated) => Op::Unzip { src, dst: free_path(&dst), pass },
            (Op::Compress { srcs, out, opts }, OpMeta::Generated) => Op::Compress { srcs, out: free_path(&out), opts },
            (op, _) => op,
        };
        self.op_dst = match &op {
            Op::Copy { dst, .. } => Some(dst.clone()),
            Op::Compress { out, .. } => out.parent().map(Path::to_path_buf),
            Op::Unzip { dst, .. } => dst.parent().map(Path::to_path_buf),
            Op::Trash(_) | Op::Delete(_) | Op::Fetch(_) => None,
        };
        self.op_unzip = if let Op::Unzip { src, dst, .. } = &op { Some((src.clone(), dst.clone())) } else { None };
        self.op_meta = meta;
        self.op_started = Instant::now();
        self.op_desc = op_desc(&op);
        self.ux.op_ann = ux::Ann::of(&op, &self.op_meta);
        self.op = Some(fsys::run_op(op, self.ctx.clone()));
    }

    /// Copia/mueve; si algún destino ya existe pregunta qué hacer.
    pub(super) fn start_copy(&mut self, srcs: Vec<PathBuf>, dst: PathBuf, mv: bool) {
        self.spawn_io(
            move || {
                // lo que ya existe en el destino, con tamaño y fecha de los dos
                let info = ux::clashes(&srcs, &dst);
                (srcs, dst, info)
            },
            move |a, (srcs, dst, info)| {
                if info.is_empty() {
                    a.start_op(Op::Copy { srcs, dst, mv, policy: Policy::Rename }, OpMeta::Copy { mv, replay: false });
                } else {
                    let names = info.iter().map(|c| c.name.clone()).collect();
                    a.conflict = Some(ConflictDlg { srcs, dst, mv, names, ux: ux::ConflictUx { info, ..Default::default() } });
                }
            },
        );
    }

    pub(super) fn push_undo(&mut self, u: Undo, clear_redo: bool) {
        if clear_redo {
            self.redo_stack.clear();
        }
        self.undo_stack.push(u);
        if self.undo_stack.len() > 100 {
            self.undo_stack.remove(0);
        }
        self.ux.undo_gen += 1;
    }

    pub(super) fn undo(&mut self) {
        self.ux.undo_gen += 1;
        match self.undo_stack.pop() {
            Some(u) => self.apply_history(u, false),
            None => self.say(t("Nada que deshacer").into(), false),
        }
    }

    pub(super) fn redo(&mut self) {
        self.ux.undo_gen += 1;
        match self.redo_stack.pop() {
            Some(u) => self.apply_history(u, true),
            None => self.say(t("Nada que rehacer").into(), false),
        }
    }

    fn rename_checked(a: &Path, b: &Path) -> Result<(), String> {
        if fsys::case_alias(a, b) {
            return fsys::rename_case(a, b).map_err(|e| format!("{}: {e}", a.display()));
        }
        let same = matches!((crate::vfs::fs::canonicalize(a), crate::vfs::fs::canonicalize(b)), (Ok(x), Ok(y)) if x == y);
        if !same && crate::vfs::fs::symlink_metadata(b).is_ok() {
            return Err(tf("{}: ya existe", &[&b.display()]));
        }
        crate::vfs::fs::rename(a, b).map_err(|e| format!("{}: {e}", a.display()))
    }

    fn apply_history(&mut self, u: Undo, is_redo: bool) {
        match &u {
            Undo::Moved(pairs) => {
                self.history_moves(pairs.clone(), Vec::new(), is_redo);
                return;
            }
            Undo::MovedRepl(pairs, replaced) => {
                self.history_moves(pairs.clone(), replaced.clone(), is_redo);
                return;
            }
            Undo::CopiedRepl(pairs, rep) if is_redo => {
                // se repite la copia: lo que se había reemplazado (ya recuperado) vuelve a reemplazarse;
                // lo demás nunca pisa nada
                let mut groups: Vec<(PathBuf, Policy, Vec<PathBuf>)> = Vec::new();
                for (src, new) in pairs {
                    let Some(parent) = new.parent() else { continue };
                    let pol = if rep.contains(new) { Policy::Replace } else { Policy::Rename };
                    match groups.iter_mut().find(|(d, p, _)| d == parent && *p == pol) {
                        Some((_, _, v)) => v.push(src.clone()),
                        None => groups.push((parent.to_path_buf(), pol, vec![src.clone()])),
                    }
                }
                for (dst, policy, srcs) in groups {
                    self.start_op(Op::Copy { srcs, dst, mv: false, policy }, OpMeta::Copy { mv: false, replay: true });
                }
                return;
            }
            Undo::CopiedRepl(pairs, rep) => {
                // deshacer: lo reemplazado vuelve de la papelera y las copias van a ella (en un hilo)
                let (p2, r2) = (pairs.clone(), rep.clone());
                let rep = rep.clone();
                self.spawn_io(
                    move || undo_copy_repl(&p2, &r2, |v| extras::restore_paths(v)),
                    move |a, res| match res {
                        Ok((done, trash, err)) => {
                            if let Some(e) = err {
                                a.say(tf("No se pudo deshacer: {e}", &[&e]), true);
                            }
                            a.start_op(Op::Trash(trash), OpMeta::Replay(copy_undo(false, done, &rep)));
                        }
                        Err(e) => {
                            a.say(tf("No se pudo deshacer: {e}", &[&e]), true);
                            a.undo_stack.push(u);
                            a.refresh_keep();
                        }
                    },
                );
                return;
            }
            Undo::Created(items) if !is_redo => {
                let v = items.iter().map(|(p, _)| p.clone()).collect();
                self.start_op(Op::Trash(v), OpMeta::Replay(u.clone()));
                return;
            }
            Undo::Copied(pairs) => {
                if is_redo {
                    // una sola tarea (y una sola entrada de deshacer) por carpeta de destino
                    let mut groups: Vec<(PathBuf, Vec<PathBuf>)> = Vec::new();
                    for (src, new) in pairs {
                        let Some(parent) = new.parent() else { continue };
                        match groups.iter_mut().find(|(d, _)| d == parent) {
                            Some((_, v)) => v.push(src.clone()),
                            None => groups.push((parent.to_path_buf(), vec![src.clone()])),
                        }
                    }
                    for (dst, srcs) in groups {
                        self.start_op(Op::Copy { srcs, dst, mv: false, policy: Policy::Rename }, OpMeta::Copy { mv: false, replay: true });
                    }
                    return;
                }
                let v = pairs.iter().map(|(_, n)| n.clone()).collect();
                self.start_op(Op::Trash(v), OpMeta::Replay(u.clone()));
                return;
            }
            Undo::Trashed(items) if is_redo => {
                self.start_op(Op::Trash(items.clone()), OpMeta::Trash { replay: true });
                return;
            }
            Undo::Generated(paths) => {
                if is_redo {
                    return;
                }
                self.start_op(Op::Trash(paths.clone()), OpMeta::Other);
                return;
            }
            _ => {}
        }
        // renombrar, crear y sacar de la papelera: E/S que puede ser lenta (red), en un hilo
        let job = u.clone();
        self.spawn_io(
            move || match &job {
                Undo::Rename { from, to } => {
                    let (a, b) = if is_redo { (from, to) } else { (to, from) };
                    Self::rename_checked(a, b).err()
                }
                Undo::Created(items) => {
                    for (p, dir) in items {
                        let r = if *dir { crate::vfs::fs::create_dir_all(p) } else { crate::vfs::fs::OpenOptions::new().write(true).create_new(true).open(p).map(|_| ()) };
                        if let Err(e) = r {
                            return Some(format!("{}: {e}", p.display()));
                        }
                    }
                    None
                }
                Undo::Trashed(items) => extras::restore_paths(items).err(),
                _ => None,
            },
            move |a, err| match err {
                Some(e) => {
                    a.say(tf("No se pudo deshacer: {e}", &[&e]), true);
                    // se conserva en su pila original
                    if is_redo { a.redo_stack.push(u) } else { a.undo_stack.push(u) }
                }
                None => {
                    if is_redo { a.undo_stack.push(u) } else { a.redo_stack.push(u) }
                    a.preview = None;
                    a.refresh_keep();
                }
            },
        );
    }

    /// Deshace (o rehace) un movimiento: renombra en un hilo; lo que está en otro sistema de archivos
    /// se mueve con una tarea de copia, y lo que el movimiento reemplazó se recupera de la papelera
    /// (al rehacer, vuelve a ella antes de mover).
    fn history_moves(&mut self, pairs: Vec<(PathBuf, PathBuf)>, replaced: Vec<PathBuf>, is_redo: bool) {
        let (p2, r2) = (pairs.clone(), replaced.clone());
        self.spawn_io(
            move || {
                let (mut ok, mut xdev, mut err) = (Vec::new(), Vec::new(), None::<String>);
                // (a, b) de cada paso; al deshacer, de lo nuevo a lo original
                let steps: Vec<(PathBuf, PathBuf)> = p2.iter().map(|(o, n)| if is_redo { (o.clone(), n.clone()) } else { (n.clone(), o.clone()) }).collect();
                // un intercambio (renombrado en lote a→b, b→a) necesita nombres intermedios
                let swap = steps.iter().any(|(_, b)| steps.iter().any(|(a, _)| a == b));
                if swap {
                    match rename_two_phase(&steps) {
                        Ok(()) => ok = (0..steps.len()).collect(),
                        Err((done, e)) => {
                            ok = done;
                            err = Some(e);
                        }
                    }
                } else {
                    for (k, (a, b)) in steps.iter().enumerate() {
                        if is_redo && r2.contains(b) && crate::vfs::fs::symlink_metadata(b).is_ok() {
                            // rehacer «reemplazar»: lo recuperado vuelve a la papelera
                            if let Err(e) = extras::trash_delete(b) {
                                err = Some(format!("{}: {e}", b.display()));
                                break;
                            }
                        }
                        // al deshacer «Combinar» la carpeta de origen ya no existe (se borró al vaciarse)
                        if let Some(d) = b.parent().filter(|d| crate::vfs::fs::symlink_metadata(d).is_err()) {
                            let _ = crate::vfs::fs::create_dir_all(d);
                        }
                        match rename_undo(a, b) {
                            Ok(()) => ok.push(k),
                            Err((true, _)) => xdev.push(k),
                            Err((false, e)) => {
                                err = Some(e);
                                break;
                            }
                        }
                    }
                }
                // al deshacer, lo reemplazado vuelve de la papelera cuando su sitio ya está libre
                let mut rerr = None;
                if !is_redo {
                    let back: Vec<PathBuf> = ok.iter().filter_map(|&k| p2.get(k)).map(|(_, n)| n.clone()).filter(|n| r2.contains(n)).collect();
                    if !back.is_empty() {
                        rerr = extras::restore_paths(&back).err();
                    }
                }
                (ok, xdev, err, rerr)
            },
            move |a, (ok, xdev, err, rerr)| {
                let pick = |ks: &[usize]| -> Vec<(PathBuf, PathBuf)> { ks.iter().filter_map(|&k| pairs.get(k).cloned()).collect() };
                let make = |v: Vec<(PathBuf, PathBuf)>| moved_undo(v, &replaced);
                let done = pick(&ok);
                let rest: Vec<usize> = (0..pairs.len()).filter(|k| !ok.contains(k) && !xdev.contains(k)).collect();
                if !done.is_empty() {
                    let u = make(done);
                    if is_redo { a.undo_stack.push(u) } else { a.redo_stack.push(u) }
                }
                if !rest.is_empty() {
                    let u = make(pick(&rest));
                    if is_redo { a.redo_stack.push(u) } else { a.undo_stack.push(u) }
                }
                if let Some(e) = err.or(rerr) {
                    a.say(tf("No se pudo deshacer: {e}", &[&e]), true);
                }
                // otro sistema de archivos: se copia y se borra el origen, como al mover, sin pisar nada
                let mut groups: Vec<(PathBuf, Vec<(PathBuf, PathBuf)>)> = Vec::new();
                let mut odd = Vec::new();
                for (k, (o, n)) in pick(&xdev).into_iter().enumerate() {
                    let (src, dst) = if is_redo { (o.clone(), n.clone()) } else { (n.clone(), o.clone()) };
                    match dst.parent().filter(|_| src.file_name() == dst.file_name()) {
                        Some(d) => match groups.iter_mut().find(|(g, _)| g == d) {
                            Some((_, v)) => v.push((o, n)),
                            None => groups.push((d.to_path_buf(), vec![(o, n)])),
                        },
                        None => odd.push(xdev[k]),
                    }
                }
                if !odd.is_empty() {
                    // con otro nombre no se puede mover entre discos sin arriesgar a pisar algo: queda pendiente
                    let u = make(pick(&odd));
                    if is_redo { a.redo_stack.push(u) } else { a.undo_stack.push(u) }
                    if let Some((_, n)) = pick(&odd).first() {
                        a.say(tf("No se pudo deshacer: {e}", &[&n.display()]), true);
                    }
                }
                for (dst, v) in groups {
                    let srcs = v.iter().map(|(o, n)| if is_redo { o.clone() } else { n.clone() }).collect();
                    let restore = if is_redo { Vec::new() } else { replaced.iter().filter(|r| v.iter().any(|(_, n)| n == *r)).cloned().collect() };
                    a.start_op(Op::Copy { srcs, dst, mv: true, policy: Policy::Skip }, OpMeta::MoveBack { done: make(v), to_redo: !is_redo, restore });
                }
                a.preview = None;
                a.refresh_keep();
            },
        );
    }

    pub(super) fn poll_op(&mut self) {
        let Some(op) = self.op.clone() else {
            if let Some((o, m)) = self.queue.pop_front() {
                self.start_op(o, m);
            }
            return;
        };
        if op.finished.load(Ordering::Acquire) {
            let errs = crate::thumbs::lock(&op.errors).clone();
            let created = crate::thumbs::lock(&op.created).clone();
            let pairs = crate::thumbs::lock(&op.pairs).clone();
            // destinos reemplazados: los que están en la papelera se recuperan al deshacer; los demás no
            let (trashed, lost): (Vec<_>, Vec<_>) = crate::thumbs::lock(&op.replaced).iter().cloned().partition(|(_, t)| *t);
            let trashed: Vec<PathBuf> = trashed.into_iter().map(|(p, _)| p).collect();
            self.op = None;
            let op_dst = self.op_dst.take();
            let meta = std::mem::replace(&mut self.op_meta, OpMeta::Other);
            let ann = std::mem::take(&mut self.ux.op_ann);
            // comprimido protegido: se pide la contraseña y se repite la extracción
            if let (Some(e), Some((src, dst))) = (errs.first().filter(|e| *e == crate::archive::NEED_PASS || *e == crate::archive::BAD_PASS), self.op_unzip.take()) {
                let _ = crate::vfs::fs::remove_dir_all(&dst);
                self.dlg = Dlg::ArchPass(ArchPass { src, dst, meta, pass: String::new(), show: false, wrong: e == crate::archive::BAD_PASS, focus: true });
                return;
            }
            let mut enter: Option<PathBuf> = None;
            match meta {
                OpMeta::OpenArchive { archive, root } => {
                    if errs.is_empty() && !op.cancelled() {
                        let _ = crate::vfs::fs::write(archive_marker(&root), b"");
                        if !self.arch.iter().any(|(r, _)| *r == root) {
                            self.arch.push((root.clone(), archive));
                        }
                        enter = Some(root);
                    } else {
                        let _ = crate::vfs::fs::remove_dir_all(&root);
                    }
                }
                OpMeta::Copy { mv, replay } if !pairs.is_empty() => {
                    // con «Combinar» puede haber reemplazado archivos sueltos
                    self.push_undo(copy_undo(mv, pairs.clone(), &trashed), !replay);
                }
                OpMeta::Replace { mv, names } if !pairs.is_empty() => {
                    // lo reemplazado que fue a la papelera: deshacer lo recupera (al mover y al copiar)
                    let replaced: Vec<PathBuf> = trashed.iter().filter(|n| n.file_name().map(|f| names.iter().any(|x| f == x.as_str())).unwrap_or(false)).cloned().collect();
                    self.push_undo(copy_undo(mv, pairs.clone(), &replaced), true);
                }
                OpMeta::MoveBack { done, to_redo, restore } => {
                    // lo que sí llegó pasa a la otra pila; lo que no, queda donde estaba
                    let (v, rep) = match done {
                        Undo::MovedRepl(v, r) => (v, r),
                        Undo::Moved(v) => (v, Vec::new()),
                        _ => (Vec::new(), Vec::new()),
                    };
                    let (ok, bad): (Vec<_>, Vec<_>) = v.into_iter().partition(|(o, n)| pairs.iter().any(|(s, _)| s == if to_redo { n } else { o }));
                    let back: Vec<PathBuf> = restore.into_iter().filter(|r| ok.iter().any(|(_, n)| n == r)).collect();
                    if !ok.is_empty() {
                        let u = moved_undo(ok, &rep);
                        if to_redo { self.redo_stack.push(u) } else { self.undo_stack.push(u) }
                    }
                    if !bad.is_empty() {
                        let u = moved_undo(bad, &rep);
                        if to_redo { self.undo_stack.push(u) } else { self.redo_stack.push(u) }
                    }
                    if !back.is_empty() {
                        self.spawn_io(move || extras::restore_paths(&back), |a, r| {
                            if let Err(e) = r {
                                a.say(tf("No se pudo deshacer: {e}", &[&e]), true);
                            }
                            a.refresh_keep();
                        });
                    }
                }
                OpMeta::Trash { replay } if !pairs.is_empty() => {
                    self.push_undo(Undo::Trashed(pairs.iter().map(|(a, _)| a.clone()).collect()), !replay);
                }
                OpMeta::Generated if !pairs.is_empty() => {
                    self.push_undo(Undo::Generated(pairs.iter().map(|(a, _)| a.clone()).collect()), true);
                }
                // descarga de un servidor SFTP: ahora sí se entrega a otro programa
                OpMeta::Fetch(then) if errs.is_empty() && !op.cancelled() => self.fetch_done(then),
                // deshacer cancelado o con errores: no pasa a rehacer; vuelve a deshacer para reintentarlo
                OpMeta::Replay(u) if errs.is_empty() && !op.cancelled() => self.redo_stack.push(u),
                OpMeta::Replay(u) => self.undo_stack.push(u),
                _ => {}
            }
            // aviso de fin (con «Deshacer») u oferta de borrado si la papelera no lo aceptó
            self.ux_op_done(ann, &errs, &pairs, &created, op.cancelled());
            if !lost.is_empty() {
                // sin papelera (o no lo aceptó): que «Deshacer» no aparente recuperarlo
                self.say(crate::i18n::tn("{} elemento reemplazado no se pudo mover a la papelera y se eliminó: deshacer no lo recuperará|{} elementos reemplazados no se pudieron mover a la papelera y se eliminaron: deshacer no los recuperará", lost.len(), &[]), false);
            }
            if op.cancelled() {
                self.say(t("Operación cancelada").into(), false);
            }
            if let Some((o, m)) = self.queue.pop_front() {
                self.start_op(o, m);
            }
            if errs.len() > 1 {
                // no se pisa un diálogo abierto (renombrado en lote, propiedades…): se muestran al cerrarlo
                if matches!(self.dlg, Dlg::None) {
                    self.dlg = Dlg::Errors(errs.clone());
                } else {
                    self.errs_later.extend(errs.iter().cloned());
                }
            }
            if let Some(e) = errs.first() {
                self.say(tf("Error: {e}{}", &[e, &(if errs.len() > 1 { tf(" (+{} más)", &[&(errs.len() - 1)]) } else { String::new() })]), true);
            }
            // lo creado se selecciona sólo si está en la carpeta que se ve; si no, se conserva la selección
            // y la posición. La vista previa sólo se cierra si su archivo se ha ido.
            let here = op_dst.as_deref() == Some(self.cwd.as_path()) && !created.is_empty();
            if self.preview.as_ref().map(|p| pairs.iter().any(|(s, _)| *s == p.path)).unwrap_or(false) {
                self.preview = None;
            }
            let touched = |d: &Path| op_dst.as_deref() == Some(d) || pairs.iter().any(|(s, _)| s.parent() == Some(d));
            // con un renombrado en línea abierto no se recarga (lo cerraría): la vigilancia lo hará después
            if self.rename.is_none() {
                if here {
                    self.pending_select = created;
                    self.refresh();
                } else {
                    self.refresh_keep();
                }
            }
            if self.other.as_deref().map(|o| touched(&o.cwd)).unwrap_or(false) {
                self.swap_pane();
                if self.rename.is_none() {
                    self.refresh_keep();
                }
                self.swap_pane();
            }
            if let Some(root) = enter {
                self.pending_select.clear();
                self.navigate(root);
            }
        }
    }

    pub(super) fn sort_by(&mut self, key: SortKey) {
        if self.cfg.sort == key {
            self.cfg.asc = !self.cfg.asc;
        } else {
            self.cfg.sort = key;
            self.cfg.asc = true;
        }
        self.save_cfg();
        self.spawn_sort();
    }
}
