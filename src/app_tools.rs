//! Terminal integrado y acciones de herramientas (comprimir, propiedades, renombrado por lotes, papelera, búsquedas…).
use super::*;

impl App {
    // ---- terminal ----

    pub(super) fn ensure_term(&mut self) {
        let dead = self.term.as_ref().map(|t| !t.alive()).unwrap_or(true);
        if dead {
            match Terminal::spawn(&self.ctx, &self.cwd, &self.cfg.shell) {
                Ok(t) => {
                    self.term = Some(t);
                    self.term_shell = if self.cfg.shell.trim().is_empty() { crate::terminal::default_shell() } else { self.cfg.shell.clone() };
                }
                Err(e) => {
                    self.term = None;
                    self.cfg.term_open = false;
                    self.say(tf("No se pudo abrir el terminal: {e}", &[&e]), true);
                }
            }
        }
    }

    pub(super) fn toggle_terminal(&mut self) {
        if crate::vfs::WEB {
            return;
        }
        if self.cfg.term_open {
            self.cfg.term_open = false;
            self.term_focus = false;
        } else {
            self.cfg.term_open = true;
            self.ensure_term();
            self.term_focus = self.term.is_some();
        }
        self.save_cfg();
    }

    pub(super) fn terminal_here(&mut self) {
        self.cfg.term_open = true;
        self.ensure_term();
        let cmd = shell_quote(&self.cwd, &self.term_shell);
        if cmd.is_empty() {
            self.say(t("La ruta tiene caracteres de control: no se escribe en el terminal").into(), true);
        }
        if let (Some(t), false) = (&mut self.term, cmd.is_empty()) {
            t.send(cmd.as_bytes());
            self.term_focus = true;
        }
        self.save_cfg();
    }

    /// Comparte la carpeta actual con `filebrowser-server` (proyecto aparte). Antes se confirma qué se
    /// comparte y cómo (red, escritura, HTTPS, contraseña); el servidor corre como proceso propio, sin
    /// escribir en el terminal, y el diálogo muestra el enlace y el QR (`ux.rs`).
    #[cfg(not(target_os = "android"))]
    pub(super) fn share_folder(&mut self) {
        let Some(exe) = server_exe() else {
            self.say(t("filebrowser-server no está instalado junto al gestor ni en el PATH").into(), true);
            return;
        };
        #[cfg(not(target_arch = "wasm32"))]
        self.ux_share_open(exe);
        #[cfg(target_arch = "wasm32")]
        let _ = exe;
    }

    pub(super) fn terminal_panel(&mut self, ui: &mut egui::Ui) {
        let pal = self.pal;
        if self.term.as_ref().map(|t| !t.alive()).unwrap_or(true) {
            self.term = None;
            self.cfg.term_open = false;
            self.term_focus = false;
            return;
        }
        let mut close = false;
        let mut cd_here = false;
        // atajo para volver a la lista con el teclado (el terminal se queda Tab, flechas y Esc)
        let leave = Some(commands::shortcut_text(Cmd::OtherPane)).filter(|s| !s.is_empty()).unwrap_or_else(|| commands::display("Ctrl+F6"));
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(t("TERMINAL")).size(11.0).color(pal.subtle).strong());
            if self.term_focus {
                ui.label(egui::RichText::new(tf("{}: volver a la lista de archivos", &[&leave])).size(11.0).color(pal.subtle));
            }
            ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                if icon_button(ui, &pal, Glyph::Close, true, false, &commands::with_shortcut(t("Cerrar"), Cmd::Terminal)).clicked() {
                    close = true;
                }
                if ui.small_button(t("cd a la carpeta actual")).clicked() {
                    cd_here = true;
                }
            });
        });
        let (rect, resp) = ui.allocate_exact_size(ui.available_size(), Sense::click_and_drag());
        resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Other, true, t("Terminal")));
        if ui.input(|i| i.pointer.primary_pressed()) {
            self.term_focus = resp.hovered();
        }
        // llegar con Tab también le da el teclado
        if resp.gained_focus() {
            self.term_focus = true;
        }
        // foco real de egui (si no, Tab y Espacio/Intro irían al primer botón de la ventana) con Tab,
        // flechas y Esc para el terminal
        if self.term_focus {
            if !resp.has_focus() {
                resp.request_focus();
            }
            ui.memory_mut(|m| m.set_focus_lock_filter(resp.id, egui::EventFilter { tab: true, horizontal_arrows: true, vertical_arrows: true, escape: true }));
        } else if resp.has_focus() {
            ui.memory_mut(|m| m.surrender_focus(resp.id));
        }
        let focus = self.term_focus;
        if let Some(t) = &mut self.term {
            t.show(ui, rect, &resp, &pal, focus);
            let q = shell_quote(&self.cwd, &self.term_shell);
            if cd_here && !q.is_empty() {
                t.send(q.as_bytes());
            }
        }
        if close {
            self.cfg.term_open = false;
            self.term_focus = false;
            self.save_cfg();
        }
    }

    // ---- acciones de herramientas ----

    pub(super) fn invert_selection(&mut self) {
        let order = self.order.clone();
        for i in order {
            let on = !self.is_sel(i);
            self.set_bit(i, on);
        }
        self.recount();
    }

    pub(super) fn busy(&self) -> bool {
        self.op.as_ref().map(|o| !o.finished.load(Ordering::Acquire)).unwrap_or(false)
    }

    pub(super) fn do_duplicate(&mut self) {
        let srcs = self.selected_paths();
        if srcs.is_empty() {
            return;
        }
        let dst = self.cwd.clone();
        self.start_op(Op::Copy { srcs, dst, mv: false, policy: Policy::Rename }, OpMeta::Copy { mv: false, replay: false });
    }

    /// Abre el diálogo de compresión con ZIP, el nombre habitual y el nivel normal.
    pub(super) fn do_zip(&mut self) {
        let srcs = self.selected_paths();
        if srcs.is_empty() {
            return;
        }
        let name = if srcs.len() == 1 {
            srcs[0].file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "archivo".into())
        } else {
            self.cwd.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "archivo".into())
        };
        self.dlg = Dlg::Compress(CompressDlg { srcs, name, fmt: tools::ArchFmt::Zip, level: 6, protect: false, pw: String::new(), pw2: String::new(), show: false, focus: true });
    }

    /// Abre un comprimido como carpeta de sólo lectura: lo extrae a la caché (una vez) y navega dentro.
    pub(super) fn open_archive(&mut self, p: PathBuf) {
        let Some(root) = archive_cache_dir(&p) else {
            fsys::open_external(&p);
            return;
        };
        if archive_marker(&root).v_exists() && root.v_is_dir() {
            if !self.arch.iter().any(|(r, _)| *r == root) {
                self.arch.push((root.clone(), p));
            }
            self.navigate(root);
            return;
        }
        // ya se está extrayendo ahí: borrar la caché ahora pisaría esa extracción
        let busy = matches!(&self.op_meta, OpMeta::OpenArchive { root: r, .. } if *r == root) && self.op.is_some()
            || self.queue.iter().any(|(_, m)| matches!(m, OpMeta::OpenArchive { root: r, .. } if *r == root));
        if busy {
            return;
        }
        if let Some(par) = root.parent() {
            fsys::private_dir(par);
        }
        let _ = crate::vfs::fs::remove_dir_all(&root);
        self.start_op(Op::Unzip { src: p.clone(), dst: root.clone(), pass: None }, OpMeta::OpenArchive { archive: p, root });
    }

    /// Nombre corto de una carpeta; la raíz de un comprimido abierto se muestra con el nombre del archivo.
    /// Raíz del montaje gvfs que contiene `p` y su nombre legible.
    pub(super) fn gvfs_mount_of(&self, p: &Path) -> Option<(PathBuf, String)> {
        // servidor SFTP integrado: la raíz del espejo se enseña como «usuario@servidor (SFTP)»
        if let Some(r) = crate::sftp::session_root(p) {
            return Some(r);
        }
        #[cfg(target_os = "linux")]
        {
            let g = gvfs_dir()?;
            let first = p.strip_prefix(&g).ok()?.components().next()?;
            let name = first.as_os_str().to_string_lossy().into_owned();
            return Some((g.join(&name), gvfs_label(&name)));
        }
        #[allow(unreachable_code)]
        {
            let _ = p;
            None
        }
    }

    pub(super) fn dir_label(&self, p: &Path) -> String {
        if let Some((m, l)) = self.gvfs_mount_of(p) {
            if m == p {
                return l;
            }
        }
        match virt_mode(p) {
            Some(VMode::Trash) => return t("Papelera").to_string(),
            Some(VMode::Recent) => return t("Recientes").to_string(),
            _ => {}
        }
        let p = match self.arch.iter().find(|(r, _)| r == p) {
            Some((_, a)) => a.as_path(),
            None => p,
        };
        p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| p.display().to_string())
    }

    /// Archivo original si `cwd` está dentro de un comprimido abierto.
    pub(super) fn arch_of(&self, cwd: &Path) -> Option<(PathBuf, PathBuf)> {
        self.arch.iter().find(|(r, _)| cwd.starts_with(r)).cloned()
    }

    /// Franja sobre la lista cuando se está dentro de un comprimido.
    pub(super) fn arch_banner(&mut self, ui: &mut egui::Ui) {
        let Some((_, archive)) = self.arch_of(&self.cwd) else { return };
        let pal = self.pal;
        let name = archive.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        ui.horizontal(|ui| {
            ui.add_space(8.0);
            ui.label(egui::RichText::new(tf("Contenido de «{}» (sólo lectura)", &[&name])).color(pal.subtle).size(12.5));
            ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                if ui.button(t("Extraer todo")).clicked() {
                    if let Some(dir) = archive.parent() {
                        let stem = archive.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "extraido".into());
                        // `start_op` elige el nombre libre al empezar
                        let dst = dir.join(&stem);
                        self.start_op(Op::Unzip { src: archive.clone(), dst, pass: None }, OpMeta::Generated);
                    }
                }
            });
        });
        ui.add_space(2.0);
    }

    /// Borra la extracción de los comprimidos que ya no se usan: ni un panel, ni una pestaña, ni su
    /// historial, ni el portapapeles, ni una vista previa, ni una tarea. Lo abierto desde ahí con otra
    /// aplicación se deja para la limpieza diaria (puede seguir usándolo).
    pub(super) fn release_archives(&mut self) {
        if self.arch.is_empty() || self.busy() || !self.queue.is_empty() {
            return;
        }
        let all = std::mem::take(&mut self.arch);
        let used = |root: &Path| -> bool {
            let hit = |p: &PathBuf| p.starts_with(root);
            let pane = |cwd: &PathBuf, back: &[PathBuf], fwd: &[PathBuf]| hit(cwd) || back.iter().any(hit) || fwd.iter().any(hit);
            pane(&self.cwd, &self.back, &self.fwd)
                || self.other.as_deref().map(|o| pane(&o.cwd, &o.back, &o.fwd)).unwrap_or(false)
                || self.tabs.iter().any(|t| pane(&t.cwd, &t.back, &t.fwd) || t.split.as_deref().map(|s| pane(&s.cwd, &s.back, &s.fwd)).unwrap_or(false))
                || self.clipboard.as_ref().map(|(v, _)| v.iter().any(hit)).unwrap_or(false)
                || self.preview.as_ref().map(|p| hit(&p.path)).unwrap_or(false)
                || self.side.as_ref().map(|p| hit(&p.path)).unwrap_or(false)
                || self.arch_ext.contains(root)
        };
        let (keep, gone): (Vec<_>, Vec<_>) = all.into_iter().partition(|(r, _)| used(r));
        self.arch = keep;
        for (root, _) in gone {
            // se aparta con otro nombre antes de borrarla en un hilo: si se vuelve a abrir el mismo
            // comprimido enseguida, su nueva extracción no choca con el borrado
            let mut tmp = root.file_name().map(|n| n.to_os_string()).unwrap_or_default();
            tmp.push(format!(".del{}", self.generation));
            let tmp = root.with_file_name(tmp);
            let _ = crate::vfs::fs::remove_file(archive_marker(&root));
            if crate::vfs::fs::rename(&root, &tmp).is_ok() {
                crate::vfs::thread::spawn(move || {
                    let _ = crate::vfs::fs::remove_dir_all(&tmp);
                });
            }
        }
    }

    pub(super) fn do_unzip(&mut self) {
        let Some(src) = self.selected_paths().into_iter().next() else { return };
        let stem = src.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "extraido".into());
        let dst = self.cwd.join(&stem);
        self.start_op(Op::Unzip { src, dst, pass: None }, OpMeta::Generated);
    }

    pub(super) fn open_props(&mut self) {
        let paths = if self.sel_count > 0 { self.selected_paths() } else { vec![self.cwd.clone()] };
        self.open_props_paths(paths);
    }

    pub(super) fn open_props_paths(&mut self, paths: Vec<PathBuf>) {
        if paths.is_empty() {
            return;
        }
        let single = paths.len() == 1;
        let md = crate::vfs::fs::symlink_metadata(&paths[0]).ok();
        let title = if single {
            paths[0].file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| paths[0].display().to_string())
        } else {
            crate::i18n::tn("{} elemento|{} elementos", paths.len(), &[])
        };
        let location = paths[0].parent().map(|p| p.display().to_string()).unwrap_or_default();
        let kind = if single {
            match &md {
                Some(m) if m.v_is_dir() => t("Carpeta").to_string(),
                Some(m) if m.file_type().v_is_symlink() => {
                    // enlace roto: lo que ya sabe la lista (`ux_link_broken`) o, si no está en ella, se mira aquí
                    let ent = paths[0].parent().filter(|d| *d == self.cwd).and(paths[0].file_name()).and_then(|n| {
                        let n = n.to_string_lossy();
                        self.entries.iter().find(|e| *e.name == *n).cloned()
                    });
                    let broken = match ent.and_then(|e| self.ux_link_broken(&e)) {
                        Some(b) => b,
                        None => crate::vfs::fs::metadata(&paths[0]).is_err(),
                    };
                    if broken { t("Enlace roto").to_string() } else { t("Enlace simbólico").to_string() }
                }
                _ => {
                    let e = paths[0].extension().map(|e| e.to_string_lossy().to_uppercase()).unwrap_or_default();
                    if e.is_empty() { t("Archivo").to_string() } else { tf("Archivo {e}", &[&e]) }
                }
            }
        } else {
            t("Selección múltiple").into()
        };
        let stats = tools::spawn_stats(paths.clone(), self.ctx.clone());
        self.dlg = Dlg::Props(PropsDlg {
            job: None,
            title,
            location,
            kind,
            mtime: md.as_ref().map(mtime_secs).unwrap_or(0),
            mode: if single { md.as_ref().map(mode_string).unwrap_or_default() } else { String::new() },
            name: if single { paths[0].file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default() } else { String::new() },
            name_orig: if single { paths[0].file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default() } else { String::new() },
            time: md.as_ref().map(|m| fsys::fmt_time_iso(mtime_secs(m))).unwrap_or_default(),
            time_orig: md.as_ref().map(|m| fsys::fmt_time_iso(mtime_secs(m))).unwrap_or_default(),
            perm: md.as_ref().map(perm_bits).unwrap_or(0),
            perm_orig: md.as_ref().map(perm_bits).unwrap_or(0),
            recursive: false,
            // lo que ya sabe la lista; sólo se mira el disco (con límite de tiempo) si no está en ella
            has_dir: paths.iter().any(|p| {
                let known = p.parent().filter(|d| *d == self.cwd).and(p.file_name()).and_then(|n| {
                    let n = n.to_string_lossy();
                    self.entries.iter().find(|e| *e.name == *n).map(|e| e.is_dir())
                });
                known.unwrap_or_else(|| app_nav::quick_stat(p).is_none_or(|s| s.0))
            }),
            err: String::new(),
            ok: false,
            paths,
            stats,
        });
    }

    /// Aplica los cambios hechos en la ventana de propiedades: permisos, fecha y nombre. Lo escrito se
    /// comprueba aquí; la E/S (un `chmod` recursivo, una carpeta de red) va en un hilo con avance y
    /// cancelación, y el resultado vuelve al diálogo si sigue abierto (si no, los errores van a un aviso).
    pub(super) fn apply_props(&mut self, d: &mut PropsDlg) {
        if d.job.is_some() {
            return;
        }
        d.err.clear();
        d.ok = false;
        // copia local de un servidor SFTP: cambiarla no cambia el servidor
        if d.paths.iter().any(|p| crate::sftp::is_remote(p)) {
            d.err = t("Los servidores SFTP son de sólo lectura").to_string();
            return;
        }
        let perm = (d.perm != d.perm_orig).then_some(d.perm);
        let secs = if d.time != d.time_orig {
            let Some(secs) = fsys::parse_time(&d.time) else {
                d.err = t("Fecha no válida (AAAA-MM-DD HH:MM:SS)").to_string();
                return;
            };
            Some(secs)
        } else {
            None
        };
        let rename = if d.paths.len() == 1 && d.name != d.name_orig {
            let new = d.name.trim().to_string();
            if bad_name(&new) {
                d.err = t("Nombre no válido").to_string();
                return;
            }
            Some((new, d.name_orig.clone()))
        } else {
            None
        };
        if perm.is_none() && secs.is_none() && rename.is_none() {
            return;
        }
        let job = Arc::new(tools::Work::default());
        d.job = Some(job.clone());
        let (paths, recursive, sent_time, sent_name) = (d.paths.clone(), d.recursive, d.time.clone(), d.name.clone());
        let w = job.clone();
        self.spawn_io(
            move || props_work(&paths, perm, recursive, secs, rename, &w),
            move |a, r: PropsDone| {
                let changed = r.perm.is_some() || r.secs.is_some() || r.renamed.is_some();
                match &mut a.dlg {
                    Dlg::Props(d) if d.job.as_ref().is_some_and(|j| Arc::ptr_eq(j, &job)) => {
                        d.job = None;
                        if let Some(bits) = r.perm {
                            d.perm_orig = bits;
                            d.mode = r.mode.unwrap_or_default();
                        }
                        if let Some(secs) = r.secs {
                            d.mtime = secs;
                            let iso = fsys::fmt_time_iso(secs);
                            // lo que se haya vuelto a editar mientras tanto se respeta
                            if d.time == sent_time {
                                d.time = iso.clone();
                            }
                            d.time_orig = iso;
                        }
                        if let Some((dst, new)) = r.renamed {
                            d.paths[0] = dst;
                            d.title = new.clone();
                            if d.name == sent_name {
                                d.name = new.clone();
                            }
                            d.name_orig = new;
                        }
                        match r.err {
                            Some(e) => d.err = e,
                            None => d.ok = changed,
                        }
                    }
                    // el diálogo ya se cerró: el error se avisa igualmente
                    _ => {
                        if let Some(e) = r.err {
                            a.say(e, true);
                        }
                    }
                }
                if changed {
                    a.refresh();
                }
            },
        );
    }

    pub(super) fn open_batch(&mut self) {
        let names = self.selected_names();
        if names.len() < 2 {
            return;
        }
        if names.iter().any(|n| n.contains(['/', '\\'])) || virt_mode(&self.cwd).is_some() {
            self.say(t("Renombrar en lote sólo funciona dentro de una carpeta normal").into(), true);
            return;
        }
        let mut mtimes = Vec::new();
        self.for_each_selected(|i| {
            if let Some(e) = self.entries.get(i as usize) {
                mtimes.push(e.mtime);
            }
        });
        let parent = self.cwd.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        self.dlg = Dlg::Batch(BatchDlg {
            template: String::new(),
            regex: false,
            mtimes,
            parent,
            dir: self.cwd.clone(),
            names,
            find: String::new(),
            repl: String::new(),
            prefix: String::new(),
            suffix: String::new(),
            number: false,
            start: 1,
            case: 0,
            preview: true,
            exists: HashMap::new(),
            re_cache: None,
        });
    }

    /// Ejecuta un programa directamente, en su carpeta y sin ventana.
    pub(super) fn run_program(&mut self, p: &Path) {
        // Windows: .exe/.com/.bat/.cmd se lanzan directamente; .msi, .lnk, .vbs… los abre su programa asociado
        #[cfg(windows)]
        if !matches!(p.extension().map(|e| e.to_string_lossy().to_lowercase()).as_deref(), Some("exe" | "com" | "bat" | "cmd")) {
            fsys::open_external(p);
            return;
        }
        let mut c = std::process::Command::new(p);
        c.current_dir(p.parent().unwrap_or(Path::new("/")))
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        match c.spawn() {
            // se recoge el hijo al terminar: sin wait quedaría como zombi
            Ok(mut ch) => {
                crate::vfs::thread::spawn(move || {
                    let _ = ch.wait();
                });
            }
            Err(e) => self.say(format!("{}: {e}", t("No se pudo abrir")), true),
        }
    }

    /// Ejecuta un programa dentro del terminal integrado (en Windows, en una consola nueva).
    pub(super) fn run_in_terminal(&mut self, p: &Path) {
        #[cfg(windows)]
        {
            // cmd interpreta estos caracteres aunque vayan entre comillas: no se ejecuta esa ruta
            if p.to_string_lossy().contains(CMD_BAD) || has_ctl(p) {
                self.say(t("La ruta tiene caracteres de control: no se escribe en el terminal").to_string(), true);
                return;
            }
            let r = std::process::Command::new("cmd")
                .args(["/C", "start", "", "cmd", "/K"])
                .arg(p)
                .current_dir(p.parent().unwrap_or(Path::new("\\")))
                .spawn();
            if let Err(e) = r {
                self.say(format!("{}: {e}", t("No se pudo abrir")), true);
            }
            return;
        }
        #[cfg(not(windows))]
        self.run_in_terminal_pty(p);
    }

    #[cfg(not(windows))]
    fn run_in_terminal_pty(&mut self, p: &Path) {
        self.cfg.term_open = true;
        self.ensure_term();
        let dir = p.parent().unwrap_or(Path::new("/")).to_path_buf();
        if has_ctl(p) || has_ctl(&dir) {
            self.say(t("La ruta tiene caracteres de control: no se escribe en el terminal").into(), true);
            return;
        }
        let line = format!(
            "{}'{}'\r",
            shell_quote(&dir, "").trim_end_matches('\r').to_string() + " && ",
            p.display().to_string().replace('\'', "'\\''")
        );
        if let Some(t) = &mut self.term {
            t.send(line.as_bytes());
            self.term_focus = true;
        }
        self.save_cfg();
    }

    pub(super) fn open_hash(&mut self) {
        let Some(path) = self.selected_paths().into_iter().next() else { return };
        if path.v_is_dir() {
            return;
        }
        let job = extras::spawn_hash(path.clone(), self.ctx.clone());
        self.dlg = Dlg::Hash(HashDlg { path, job, cmp: String::new() });
    }

    pub(super) fn open_dups(&mut self) {
        let root = self.cwd.clone();
        let job = extras::spawn_dups(root.clone(), self.ctx.clone());
        self.dlg = Dlg::Dups(DupsDlg { root, job });
    }

    /// Filas de la papelera que corresponden a la selección actual.
    pub(super) fn trash_selected(&self) -> Vec<extras::TrashRow> {
        let rows = crate::thumbs::lock(&self.trash_rows);
        let mut v = Vec::new();
        self.for_each_selected(|i| {
            // la fila solo vale si coincide con la entrada mostrada (las filas pueden ser de una carga más nueva)
            if let (Some(r), Some(e)) = (rows.get(i as usize), self.entries.get(i as usize)) {
                if *r.parent.join(&r.name).to_string_lossy() == *e.name {
                    v.push(r.clone());
                }
            }
        });
        v
    }

    pub(super) fn trash_ask_purge(&mut self) {
        let rows = self.trash_selected();
        if !rows.is_empty() {
            self.confirm_purge = Some(PurgeReq::Rows(rows));
        }
    }

    /// Restaura (`restore`) o elimina definitivamente `rows` (todas si `None`) en segundo plano.
    pub(super) fn trash_run(&mut self, restore: bool, rows: Option<Vec<extras::TrashRow>>) {
        let (tx, rx) = std::sync::mpsc::channel();
        let ctx = self.ctx.clone();
        // un trabajo anterior sin terminar: este espera a que acabe y reenvía sus resultados (en cola)
        let prev = self.trash_job.take();
        crate::vfs::thread::spawn(move || {
            if let Some(prev) = prev {
                for r in prev {
                    let _ = tx.send(r);
                }
            }
            let res = (|| -> Result<(bool, usize), String> {
                let rows = match rows {
                    Some(r) => r,
                    None => extras::trash_list()?,
                };
                let n = rows.len();
                if restore {
                    extras::trash_restore(&rows)?;
                } else {
                    extras::trash_purge(&rows)?;
                }
                Ok((restore, n))
            })();
            let _ = tx.send(res);
            ctx.request_repaint();
        });
        self.trash_job = Some(rx);
        self.say(t("Trabajando…").into(), false);
    }

    pub(super) fn trash_poll(&mut self) {
        let Some(rx) = &self.trash_job else { return };
        // puede traer varios resultados (trabajos en cola); se acaba cuando el hilo suelta el canal
        let mut got = Vec::new();
        let mut finished = false;
        loop {
            match rx.try_recv() {
                Ok(r) => got.push(r),
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    finished = true;
                    break;
                }
            }
        }
        if finished {
            self.trash_job = None;
        }
        if got.is_empty() {
            return;
        }
        for r in got {
            match r {
                Ok((restore, n)) => {
                    let m = if restore { crate::i18n::tn("{} elemento restaurado|{} elementos restaurados", n, &[]) } else { crate::i18n::tn("{} elemento eliminado|{} elementos eliminados", n, &[]) };
                    self.say(m, false);
                }
                Err(e) => self.say(e, true),
            }
        }
        if self.vmode == VMode::Trash {
            self.refresh();
        } else if self.vmode == VMode::Dir {
            self.refresh(); // lo restaurado puede estar en esta carpeta
        }
    }

    pub(super) fn purge_dialog(&mut self, ctx: &egui::Context) {
        let Some(req) = &self.confirm_purge else { return };
        let text = match req {
            PurgeReq::All => t("Se vaciará la papelera: todo se eliminará de forma permanente. No se puede deshacer.").to_string(),
            PurgeReq::Rows(v) => crate::i18n::tn("Se eliminará {} elemento de forma permanente. No se puede deshacer.|Se eliminarán {} elementos de forma permanente. No se puede deshacer.", v.len(), &[]),
        };
        let mut decision: Option<bool> = None;
        let pal = self.pal;
        theme::modal_card(
            ctx,
            &pal,
            theme::Tone::Danger,
            t("Eliminar definitivamente"),
            400.0,
            |ui| {
                ui.label(egui::RichText::new(text).color(pal.subtle));
            },
            |ui| {
                if theme::dialog_button(ui, &pal, theme::Btn::Danger, t("Eliminar")).clicked() {
                    decision = Some(true);
                }
                if theme::dialog_button(ui, &pal, theme::Btn::Plain, t("Cancelar")).clicked() {
                    decision = Some(false);
                }
            },
        );
        if ctx.input(|i| i.key_pressed(Key::Escape)) {
            decision = Some(false);
        }
        match decision {
            Some(true) => match self.confirm_purge.take() {
                Some(PurgeReq::All) => self.trash_run(false, None),
                Some(PurgeReq::Rows(v)) => self.trash_run(false, Some(v)),
                None => {}
            },
            Some(false) => self.confirm_purge = None,
            None => {}
        }
    }

    /// Pregunta antes de ejecutar un programa abierto con doble clic o Enter.
    pub(super) fn run_dialog(&mut self, ctx: &egui::Context) {
        let Some(p) = &self.confirm_run else { return };
        let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let dir = p.parent().map(|d| d.display().to_string()).unwrap_or_default();
        let mut decision: Option<bool> = None;
        let pal = self.pal;
        theme::modal_card(
            ctx,
            &pal,
            theme::Tone::Warn,
            t("¿Ejecutar este programa?"),
            420.0,
            |ui| {
                ui.label(egui::RichText::new(name).strong());
                ui.label(egui::RichText::new(dir).color(pal.subtle).size(12.0));
                ui.add_space(6.0);
                ui.label(egui::RichText::new(t("Es un programa: se ejecutará con tus permisos. Ejecútalo solo si confías en su origen.")).color(pal.subtle));
            },
            |ui| {
                if theme::dialog_button(ui, &pal, theme::Btn::Primary, t("Ejecutar")).clicked() {
                    decision = Some(true);
                }
                if theme::dialog_button(ui, &pal, theme::Btn::Plain, t("Cancelar")).clicked() {
                    decision = Some(false);
                }
            },
        );
        if ctx.input(|i| i.key_pressed(Key::Escape)) {
            decision = Some(false);
        }
        match decision {
            Some(true) => {
                if let Some(p) = self.confirm_run.take() {
                    self.run_program(&p);
                }
            }
            Some(false) => self.confirm_run = None,
            None => {}
        }
    }

    /// Muestra la huella de un servidor SSH nuevo y deja que el usuario decida si confía en él.
    pub(super) fn host_key_dialog(&mut self, ctx: &egui::Context) {
        let Some((k, _)) = &self.host_key else { return };
        let host = match k.port {
            Some(p) if p != 22 => format!("{}:{p}", k.host),
            _ => k.host.clone(),
        };
        let prints = k.fingerprints.clone();
        let mut decision: Option<bool> = None;
        let pal = self.pal;
        theme::modal_card(
            ctx,
            &pal,
            theme::Tone::Warn,
            t("Servidor desconocido"),
            520.0,
            |ui| {
                ui.label(egui::RichText::new(host).strong());
                ui.add_space(4.0);
                ui.label(egui::RichText::new(t("Es la primera vez que te conectas. Comprueba que esta huella coincide con la del servidor antes de continuar; si no, alguien podría estar suplantándolo.")).color(pal.subtle));
                ui.add_space(6.0);
                for p in &prints {
                    ui.add(egui::Label::new(egui::RichText::new(p).monospace().size(12.0)).wrap());
                }
            },
            |ui| {
                if theme::dialog_button(ui, &pal, theme::Btn::Primary, t("Confiar y conectar")).clicked() {
                    decision = Some(true);
                }
                if theme::dialog_button(ui, &pal, theme::Btn::Plain, t("Cancelar")).clicked() {
                    decision = Some(false);
                }
            },
        );
        if ctx.input(|i| i.key_pressed(Key::Escape)) {
            decision = Some(false);
        }
        match decision {
            Some(true) => {
                if let Some((k, l)) = self.host_key.take() {
                    match crate::sftp::trust_host(&k) {
                        Ok(()) => self.start_mount(&l),
                        Err(e) => self.say(tf("No se pudo conectar: {}", &[&e]), true),
                    }
                }
            }
            Some(false) => self.host_key = None,
            None => {}
        }
    }

    pub(super) fn conflict_dialog(&mut self, ctx: &egui::Context) {
        // uno a uno, con tamaño y fecha de los dos, «Aplicar a todos» y «Conservar ambos» como acción segura
        self.ux_conflict_dialog(ctx);
    }

    /// Abre una búsqueda en una pestaña nueva (en la carpeta actual o en todo el equipo).
    pub(super) fn open_search(&mut self) {
        let q = self.filter.trim().to_string();
        self.open_search_tab(q, false);
    }

    pub(super) fn open_search_tab(&mut self, query: String, all: bool) {
        let root = self.cwd.clone();
        self.new_tab(root.clone());
        self.sbuf = query.clone();
        self.search_focus = true;
        self.load_mode(root, false, VMode::Search { query, all, content: false });
    }

    pub(super) fn search_all_from_filter(&mut self) {
        let q = self.filter.trim().to_string();
        self.filter.clear();
        self.spawn_sort();
        self.open_search_tab(q, true);
    }

    pub(super) fn set_deep(&mut self, on: bool) {
        if virt_mode(&self.cwd).is_some() {
            return;
        }
        let p = self.cwd.clone();
        self.preview = None;
        self.load_mode(p, true, if on { VMode::Deep } else { VMode::Dir });
    }

    pub(super) fn reveal(&mut self, path: &Path) {
        let Some(parent) = path.parent().map(|p| p.to_path_buf()) else { return };
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned());
        if parent == self.cwd {
            self.filter.clear();
            if let Some(n) = name {
                self.pending_select = vec![n];
            }
            self.load_mode(parent, false, VMode::Dir);
        } else {
            self.navigate(parent);
            if let Some(n) = name {
                self.pending_select = vec![n];
            }
        }
    }
}
