//! Arnés de capturas automáticas para pruebas visuales: con la característica `selftest`
//! (`cargo run --features selftest`) unas variables de entorno conducen la aplicación y guardan una
//! captura (FB_SHOT, FB_DLG…, véase el README). Sin la característica no se compila nada de esto.
use super::*;

/// Estado del arnés dentro de `App`.
#[derive(Default)]
pub(super) struct Shot {
    /// Arranque del autotest (primer fotograma).
    start: Option<Instant>,
    /// Captura pedida.
    req: bool,
    /// Prueba (vista previa, tareas o diálogo) ya preparada.
    prev: bool,
    /// Doble clic simulado (FB_DBL): 0 no, 1 pendiente, 2 hecho.
    dbl: u8,
    /// Traza del primer fotograma ya escrita.
    traced_ui: bool,
}

/// FB_PHONE: disposición de teléfono en el PC.
pub(super) fn phone() -> bool {
    std::env::var_os("FB_PHONE").is_some()
}

/// FB_INSETS=izq,arriba,der,abajo (en puntos): márgenes seguros simulados en el PC.
pub(super) fn insets() -> Option<[f32; 4]> {
    let v = std::env::var_os("FB_INSETS")?;
    let n: Vec<f32> = v.to_string_lossy().split(',').filter_map(|x| x.parse().ok()).collect();
    Some(if n.len() == 4 { [n[0], n[1], n[2], n[3]] } else { [0.0; 4] })
}

impl App {
    /// FB_COLS (vista de columnas) y FB_LIST (lista en vez de cuadrícula), al arrancar.
    pub(super) fn shot_view(&mut self) {
        if std::env::var_os("FB_COLS").is_some() {
            self.cfg.col_view = true;
        }
        if std::env::var("FB_LIST").is_ok() {
            self.cfg.grid = false;
        }
    }

    /// FB_SPLIT[=carpeta]: abre la vista dividida (en la carpeta dada o en la actual).
    pub(super) fn shot_split(&self) -> Option<PathBuf> {
        if std::env::var_os("FB_SPLIT").is_some() {
            Some(std::env::var_os("FB_SPLIT").map(PathBuf::from).filter(|p| p.v_is_dir()).unwrap_or_else(|| self.cwd.clone()))
        } else {
            None
        }
    }

    /// FB_FILTER: texto inicial del filtro.
    pub(super) fn shot_filter(&mut self) {
        if let Ok(f) = std::env::var("FB_FILTER") {
            self.filter = f;
        }
    }

    /// FB_DBL: ¿toca simular ahora el doble clic sobre el primer elemento? (una sola vez)
    pub(super) fn shot_dbl(&mut self) -> bool {
        let forced = self.shot.dbl == 1;
        if forced {
            self.shot.dbl = 2;
        }
        forced
    }

    /// Traza del primer fotograma (FB_SHOT).
    pub(super) fn shot_trace_ui(&mut self) {
        if !self.shot.traced_ui {
            self.shot.traced_ui = true;
            if std::env::var_os("FB_SHOT").is_some() {
                eprintln!("[trace] primer fotograma: {:?}", crate::T0.get().map(|t| t.elapsed()));
            }
        }
    }
}

impl App {
    /// Autotest opcional: FB_SHOT=ruta.png [FB_PREVIEW=n] [FB_WAIT=ms] [FB_SCROLL=filas]
    pub(super) fn selftest(&mut self, ctx: &egui::Context) {
        let Ok(path) = std::env::var("FB_SHOT") else { return };
        // las ventanas aparte se incrustan para que salgan en la captura
        ctx.set_embed_viewports(true);
        let wait: u128 = std::env::var("FB_WAIT").ok().and_then(|v| v.parse().ok()).unwrap_or(2500);
        let t0 = *self.shot.start.get_or_insert_with(Instant::now);
        let el = t0.elapsed().as_millis();
        ctx.request_repaint_after(Duration::from_millis(100));
        if el > 600 && self.preview.is_none() && !self.shot.prev && std::env::var("FB_PREVIEW").is_ok() {
            self.shot.prev = true;
            let k: usize = std::env::var("FB_PREVIEW").ok().and_then(|v| v.parse().ok()).unwrap_or(0);
            if let Some(&i) = self.order.get(k) {
                self.select_only(i);
                self.open_preview_at(i);
            }
        }
        if el > 1400 && std::env::var_os("FB_PLAY").is_some() {
            // prueba del reproductor: arranca la reproducción una sola vez
            static ONCE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
            if self.preview.is_some() && !ONCE.swap(true, Ordering::SeqCst) {
                if let Some(p) = &mut self.preview {
                    p.toggle_play(ctx);
                }
            }
        }
        if el > 800 && self.op.is_none() && self.queue.is_empty() && !self.shot.prev && std::env::var_os("FB_TASKS").is_some() {
            // prueba del centro de tareas: tres copias seguidas de la carpeta actual
            self.shot.prev = true;
            let dst = PathBuf::from(std::env::var("FB_TASKS").unwrap_or_default());
            let _ = crate::vfs::fs::create_dir_all(&dst);
            let srcs: Vec<PathBuf> = self.entries.iter().map(|e| self.cwd.join(&*e.name)).collect();
            for _ in 0..3 {
                self.start_op(Op::Copy { srcs: srcs.clone(), dst: dst.clone(), mv: false, policy: Policy::Rename }, OpMeta::Other);
            }
            self.tasks_open = true;
        }
        if el > 900 && self.shot.dbl == 0 && std::env::var_os("FB_DBL").is_some() {
            self.shot.dbl = 1; // simula doble clic sobre el primer elemento
        }
        if el > 400 && std::env::var("FB_TERM").is_ok() && self.term.is_none() {
            self.toggle_terminal();
            if let Some(t) = &mut self.term {
                t.send(b"ls --color=always | head -5; echo hola\r");
            }
        }
        if el > 500 && std::env::var("FB_DLG").is_ok() && matches!(self.dlg, Dlg::None) && !self.shot.prev {
            self.shot.prev = true;
            match std::env::var("FB_DLG").unwrap().as_str() {
                "opts" => self.dlg = Dlg::Options,
                "welcome" | "keys_ask" | "keys_reset" | "keys_find" | "trash_ask" => self.h1_selftest(&std::env::var("FB_DLG").unwrap_or_default()),
                "cols" => {
                    self.opts_tab = 1;
                    self.dlg = Dlg::Options;
                }
                "temas" => {
                    self.opts_tab = 2;
                    self.dlg = Dlg::Options;
                }
                "temas_new" => {
                    self.opts_tab = 2;
                    self.dlg = Dlg::Options;
                    self.cfg.custom_themes.push(crate::config::CustomTheme { name: "Mio".into(), dark: true, colors: theme::palette(true).colors() });
                    self.edit_theme = "Mio".into();
                }
                "compress" => {
                    if let Some(&i) = self.order.get(1) {
                        self.select_only(i);
                        self.do_zip();
                        if let Dlg::Compress(d) = &mut self.dlg {
                            d.protect = true;
                            d.pw = "abc".into();
                            d.pw2 = "abc".into();
                        }
                    }
                }
                "run" => self.confirm_run = Some(self.cwd.join("programa")),
                "toast" => {
                    self.say(t("Renombrando…").into(), false);
                    self.say(tf("No se pudo renombrar: {e}", &[&"Permission denied (os error 13)"]), true);
                    self.ux_announce(crate::i18n::tn("{} elemento copiado|{} elementos copiados", 12, &[]), false);
                }
                "conflict_info" => {
                    let side = |dir, size, mtime| ux::Side { dir, size, mtime };
                    let info = vec![
                        ux::Clash { name: "informe.pdf".into(), new: side(false, 482_113, 1_760_000_000), old: side(false, 377_020, 1_750_000_000) },
                        ux::Clash { name: "Fotos".into(), new: side(true, 0, 1_760_000_000), old: side(true, 0, 1_755_000_000) },
                        ux::Clash { name: "notas.txt".into(), new: side(false, 120, 1_740_000_000), old: side(false, 98, 1_760_000_000) },
                    ];
                    let names = info.iter().map(|c| c.name.clone()).collect();
                    let srcs = info.iter().map(|c| PathBuf::from("/tmp/origen").join(&c.name)).collect();
                    self.conflict = Some(ConflictDlg { srcs, dst: self.cwd.clone(), mv: false, names, ux: ux::ConflictUx { info, ..Default::default() } });
                }
                "delete_many" => {
                    self.ux.del_note = ux::DelNote::NoTrash;
                    self.confirm_delete = Some((1..=8).map(|i| PathBuf::from(format!("/tmp/archivo {i}.txt"))).collect());
                }
                #[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
                "share" => self.ux_share_open(PathBuf::from("/usr/bin/filebrowser-server")),
                "filter_empty" => {
                    self.filter = "zzzz-sin-coincidencias".into();
                    self.spawn_sort();
                }
                "hostkey" => {
                    let k = crate::sftp::PendingKey::sample();
                    self.host_key = Some((k, NetLogin { uri: "sftp://servidor".into(), user: String::new(), domain: String::new(), pass: String::new(), anon: false }));
                }
                "batch" => {
                    self.select_range(0, 3, false);
                    self.open_batch();
                    if let Dlg::Batch(d) = &mut self.dlg {
                        d.template = "{padre}_{n:3}".into();
                        d.find = "(a|e)".into();
                        d.regex = true;
                        d.repl = "_".into();
                        d.case = 3;
                    }
                }
                "pass" => {
                    self.dlg = Dlg::ArchPass(ArchPass { src: PathBuf::from("/tmp/secreto.zip"), dst: PathBuf::from("/tmp/x"), meta: OpMeta::Other, pass: String::new(), show: false, wrong: true, focus: true });
                }
                "del" => self.confirm_delete = Some(vec![PathBuf::from("/tmp/a"), PathBuf::from("/tmp/b")]),
                "props" => self.open_props(),
                "keys" => self.dlg = Dlg::Keys,
                "net" => self.connect_uri("smb://nas/media".into()),
                "rename" => {
                    if let Some(&i) = self.order.get(1) {
                        self.select_only(i);
                        self.start_rename();
                    }
                }
                "selmode" => {
                    self.touch = true;
                    self.sel_mode = true;
                    if let (Some(&a), Some(&b)) = (self.order.get(1), self.order.get(3)) {
                        self.select_only(a);
                        self.set_bit(b, true);
                        self.recount();
                    }
                }
                "rename_touch" => {
                    self.touch = true;
                    if let Some(&i) = self.order.get(1) {
                        self.select_only(i);
                        self.start_rename();
                    }
                }
                "palette" => self.palette = Some(PaletteState { query: "pes".into(), sel: 1, born: 0 }),
                "conflict" => {
                    let a = self.cwd.join("a.txt");
                    let sub = self.cwd.join("sub");
                    let _ = crate::vfs::fs::create_dir_all(&sub);
                    let _ = crate::vfs::fs::write(sub.join("a.txt"), b"x");
                    let _ = crate::vfs::fs::write(sub.join("b.txt"), b"x");
                    self.start_copy(vec![a, self.cwd.join("b.txt")], sub, false);
                }
                "hash" | "openwith" | "dups" | "trash" => {
                    let m = std::env::var("FB_DLG").unwrap();
                    if let Some(&i) = self.order.iter().find(|&&i| !self.entries[i as usize].is_dir()) {
                        self.select_only(i);
                    }
                    match m.as_str() {
                        "hash" => self.open_hash(),
                        "openwith" => self.open_with(),
                        "dups" => self.open_dups(),
                        _ => self.navigate(PathBuf::from(LOC_TRASH)),
                    }
                }
                "openwith-search" | "openwith-browse" => {
                    // diálogo «Abrir con» con una búsqueda escrita (FB_OW_QUERY, «text» si falta) o con «Examinar…» abierto
                    if let Some(&i) = self.order.iter().find(|&&i| !self.entries[i as usize].is_dir()) {
                        self.select_only(i);
                    }
                    self.open_with();
                    let browse = std::env::var("FB_DLG").is_ok_and(|m| m == "openwith-browse");
                    let q = std::env::var("FB_OW_QUERY").unwrap_or_else(|_| "text".into());
                    if let Dlg::OpenWith(d) = &mut self.dlg {
                        d.selftest(if browse { None } else { Some(q) });
                    }
                }
                "themeanim" => self.set_dark(!self.cfg.dark),
                n @ ("newfile" | "netlogin" | "netshares" | "errors" | "about" | "quit" | "tasks" | "phonetabs") => self.h2_selftest(n),
                "copyui" => {
                    // prueba del panel de progreso: copia FB_COPY_SRC a FB_COPY_DST
                    if let (Some(src), Some(dst)) = (std::env::var_os("FB_COPY_SRC"), std::env::var_os("FB_COPY_DST")) {
                        self.start_op(
                            Op::Copy { srcs: vec![PathBuf::from(src)], dst: PathBuf::from(dst), mv: false, policy: Policy::Rename },
                            OpMeta::Copy { mv: false, replay: false },
                        );
                    }
                }
                "undotest" => {
                    let a = self.cwd.join("ut_a.txt");
                    let b = self.cwd.join("ut_b.txt");
                    let _ = crate::vfs::fs::write(&a, b"x");
                    let ok = crate::vfs::fs::rename(&a, &b).is_ok();
                    self.push_undo(Undo::Rename { from: a.clone(), to: b.clone() }, true);
                    eprintln!("[test] renamed={ok}");
                    self.undo();
                    eprintln!("[test] after undo: a={} b={}", a.exists(), b.exists());
                    self.redo();
                    eprintln!("[test] after redo: a={} b={}", a.exists(), b.exists());
                    let _ = crate::vfs::fs::remove_file(&a);
                    let _ = crate::vfs::fs::remove_file(&b);
                }
                "gear" => {
                    if let Some(id) = self.gear_id {
                        egui::Popup::toggle_id(ctx, id);
                    }
                }
                "foldericon" => self.open_folder_icon(self.cwd.clone()),
                "favlabel" => {
                    if let Some(p) = self.places.first().map(|p| p.path.clone()) {
                        self.open_fav_label(p);
                    }
                }
                "btnall" => {
                    self.filter = "png".into();
                    self.search_all_from_filter();
                }
                "deep" => self.set_deep(true),
                "searchall" => self.open_search_tab("sub".into(), true),
                "searchdir" => self.open_search_tab("txt".into(), false),
                "addr" => {
                    self.addr_edit = Some(self.cwd.display().to_string());
                    self.addr_select = true;
                }
                "tabsplit" => {
                    self.toggle_split();
                    self.ensure_live(1);
                    if let Some(p) = self.cwd.parent().map(|p| p.to_path_buf()) {
                        self.navigate(p);
                    }
                    self.ensure_live(0);
                    self.active_side = 0;
                    self.new_tab(self.cwd.clone());
                    self.toggle_split();
                    self.switch_tab(0);
                }
                "tab" => {
                    let c = self.cwd.join("Documentos");
                    self.new_tab(if c.v_is_dir() { c } else { self.cwd.clone() });
                }
                _ => self.open_search(),
            }
        }
        if el > wait && !self.shot.req {
            self.shot.req = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::default()));
        }
        let img = ctx.input(|i| {
            i.events.iter().find_map(|e| match e {
                egui::Event::Screenshot { image, .. } => Some(image.clone()),
                _ => None,
            })
        });
        if let Some(img) = img {
            if let Ok(st) = crate::vfs::fs::read_to_string("/proc/self/status") {
                for l in st.lines().filter(|l| l.starts_with("VmRSS") || l.starts_with("VmHWM") || l.starts_with("Threads")) {
                    eprintln!("[trace] {l}");
                }
            }
            let [w, h] = img.size;
            let mut raw = Vec::with_capacity(w * h * 4);
            for c in &img.pixels {
                raw.extend_from_slice(&[c.r(), c.g(), c.b(), 255]);
            }
            let _ = image::save_buffer(&path, &raw, w as u32, h as u32, image::ColorType::Rgba8);
            std::process::exit(0);
        }
    }
}
