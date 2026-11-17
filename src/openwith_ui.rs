//! «Abrir con» (submenú con iconos y diálogo de elección de aplicación) y «Abrir en el terminal
//! del sistema». Módulo hijo de `app` (`impl App` aparte para no engordar app.rs).
use super::*;
use crate::sysapps::{self, Choices};

/// Estado de la sesión: iconos de aplicaciones ya subidos y consulta del submenú en curso.
#[derive(Default)]
pub(super) struct OwState {
    icons: HashMap<String, egui::TextureHandle>,
    /// iconos pedidos (con o sin éxito): no se vuelven a pedir
    tried: HashSet<String>,
    /// archivo cuyo submenú se está consultando en segundo plano
    menu_loading: Option<PathBuf>,
}

/// Resultado de abrir con una aplicación desde el diálogo.
enum OwLaunched {
    /// abierto; con un aviso si no se pudo fijar la predeterminada
    Opened(Option<String>),
    /// hay que fijar la predeterminada (y abrir) desde la ventana del sistema (Windows)
    NeedsSystemUi,
}

/// Diálogo «Abrir con».
pub(super) struct OwDlg {
    path: PathBuf,
    /// None mientras se consultan las aplicaciones
    choices: Option<Arc<Choices>>,
    query: String,
    /// índice en la lista filtrada (recomendadas y luego el resto)
    sel: Option<usize>,
    always: bool,
    browse_open: bool,
    browse: String,
    cmd: String,
    /// primer fotograma: el foco va al buscador
    fresh: bool,
    /// hay que desplazar la lista hasta la selección
    scroll: bool,
}

impl OwDlg {
    pub(super) fn new(path: PathBuf) -> OwDlg {
        OwDlg { path, choices: None, query: String::new(), sel: None, always: false, browse_open: false, browse: String::new(), cmd: String::new(), fresh: true, scroll: false }
    }

    /// Para FB_DLG: búsqueda escrita (`Some`) o «Examinar…» abierto (`None`).
    #[cfg(feature = "selftest")]
    pub(super) fn selftest(&mut self, query: Option<String>) {
        match query {
            Some(q) => self.query = q,
            None => {
                self.browse_open = true;
                self.browse = if cfg!(windows) { "C:\\Windows\\".into() } else { "/usr/bin/".into() };
            }
        }
    }

    /// Aplicaciones visibles con el filtro: (recomendadas, resto).
    fn filtered(&self) -> (Vec<extras::App>, Vec<extras::App>) {
        let Some(c) = &self.choices else { return (Vec::new(), Vec::new()) };
        let q = self.query.trim().to_lowercase();
        let f = |v: &[extras::App]| v.iter().filter(|a| app_matches(a, &q)).cloned().collect::<Vec<_>>();
        (f(&c.recommended), f(&c.others))
    }
}

/// ¿Coincide la aplicación con la búsqueda (ya en minúsculas)? Nombre, descripción o identificador.
pub(super) fn app_matches(a: &extras::App, q: &str) -> bool {
    q.is_empty() || [&a.name, &a.generic, &a.id].iter().any(|s| s.to_lowercase().contains(q))
}

fn icon_key(a: &extras::App) -> &str {
    a.icon.trim()
}

/// Resultado de mirar en disco lo escrito en «Examinar…»: comprobación y nombres para completar.
type BrowseInfo = (Result<PathBuf, &'static str>, Arc<Vec<(String, bool)>>);

/// Carpeta y comienzo de nombre de lo escrito (sin tocar el disco).
fn browse_split(text: &str) -> (PathBuf, String) {
    let typed = PathBuf::from(text.trim());
    if text.ends_with(['/', '\\']) {
        (typed, String::new())
    } else {
        (typed.parent().map(|p| p.to_path_buf()).unwrap_or_default(), typed.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default())
    }
}

/// Lo que hay en disco para el texto escrito, leído en otro hilo (una ruta de red puede tardar
/// segundos y no debe parar la ventana). `None` mientras se lee; se guarda por texto escrito.
fn browse_info(ctx: &egui::Context, text: &str) -> Option<BrowseInfo> {
    type Slot = Arc<std::sync::Mutex<Option<BrowseInfo>>>;
    let cid = egui::Id::new("ow-browse-io");
    let cur: Option<(String, Slot)> = ctx.data(|d| d.get_temp(cid));
    if let Some((k, slot)) = &cur {
        if k == text {
            return slot.lock().unwrap_or_else(|e| e.into_inner()).clone();
        }
    }
    let slot: Slot = Arc::new(std::sync::Mutex::new(None));
    ctx.data_mut(|d| d.insert_temp(cid, (text.to_string(), slot.clone())));
    let (text, ctx) = (text.to_string(), ctx.clone());
    crate::vfs::thread::spawn(move || {
        let check = check_program(&text);
        let (dir, stem) = browse_split(&text);
        let mut names: Vec<(String, bool)> = if dir.is_absolute() {
            std::fs::read_dir(&dir)
                .map(|rd| {
                    rd.flatten()
                        .take(4000)
                        .map(|e| (e.file_name().to_string_lossy().into_owned(), e.file_type().is_ok_and(|t| t.is_dir())))
                        .filter(|(n, _)| n.to_lowercase().starts_with(&stem) && !n.starts_with('.'))
                        .collect()
                })
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        names.sort_by_cached_key(|(n, d)| (!*d, n.to_lowercase()));
        *slot.lock().unwrap_or_else(|e| e.into_inner()) = Some((check, Arc::new(names)));
        ctx.request_repaint();
    });
    None
}

/// Comprueba la ruta escrita en «Examinar…»: un archivo que exista y se pueda ejecutar.
pub(super) fn check_program(s: &str) -> Result<PathBuf, &'static str> {
    let s = s.trim().trim_matches('"');
    if s.is_empty() {
        return Err("");
    }
    let p = PathBuf::from(s);
    if !p.is_absolute() {
        return Err("Escriba la ruta completa del programa");
    }
    let Ok(m) = std::fs::metadata(&p) else { return Err("No existe") };
    if m.is_dir() {
        #[cfg(target_os = "macos")]
        if p.extension().is_some_and(|e| e == "app") {
            return Ok(p);
        }
        return Err("Es una carpeta");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if m.permissions().mode() & 0o111 == 0 {
            return Err("No es un programa ejecutable");
        }
    }
    #[cfg(windows)]
    if !matches!(p.extension().map(|e| e.to_string_lossy().to_lowercase()).as_deref(), Some("exe" | "com" | "bat" | "cmd")) {
        return Err("No es un programa ejecutable");
    }
    Ok(p)
}

/// Orden personalizada que ejecuta `prog` con el archivo: en Windows, palabras entre comillas
/// (`systerm::split_words`); en el resto, formato Exec con `%f` (`extras::exec_argv`).
pub(super) fn program_cmd(prog: &Path) -> String {
    let s = prog.to_string_lossy();
    if cfg!(windows) {
        return win_quote(&s);
    }
    let mut q = String::from("\"");
    for c in s.chars() {
        match c {
            // escape general (\\ → \) y luego el de dentro de comillas (\\ → \)
            '\\' => q.push_str("\\\\\\\\"),
            '"' | '`' | '$' => {
                q.push('\\');
                q.push(c);
            }
            '%' => q.push_str("%%"),
            o => q.push(o),
        }
    }
    q.push_str("\" %f");
    q
}

/// Ruta de Windows entre comillas para `systerm::split_words`, que dentro de comillas cambia
/// `\\` por `\`: el `\\` inicial de una ruta UNC (`\\servidor\recurso\app.exe`) se duplica.
fn win_quote(s: &str) -> String {
    format!("\"{}\"", s.replace("\\\\", "\\\\\\\\"))
}

/// Casilla de color con la inicial (aplicación sin icono).
fn letter_tile(p: &egui::Painter, r: Rect, name: &str, pal: &Pal) {
    let h = name.bytes().fold(7u32, |h, b| h.wrapping_mul(31).wrapping_add(b as u32));
    let hues = [(0x4c, 0x7d, 0xd8), (0x2f, 0x9e, 0x74), (0xc0, 0x6a, 0x2b), (0x8e, 0x5c, 0xc7), (0xc2, 0x44, 0x6a), (0x3a, 0x8f, 0xa8)];
    let (r_, g, b) = hues[(h as usize) % hues.len()];
    let bg = Color32::from_rgb(r_, g, b);
    p.rect_filled(r, CornerRadius::same(6), bg);
    let c = name.chars().find(|c| c.is_alphanumeric()).map(|c| c.to_uppercase().to_string()).unwrap_or_else(|| "?".into());
    p.text(r.center(), Align2::CENTER_CENTER, c, FontId::proportional(r.height() * 0.5), theme::on_color(bg));
    let _ = pal;
}

/// Texto de una línea recortado a `w`.
fn line(p: &egui::Painter, s: &str, size: f32, col: Color32, w: f32) -> Arc<egui::Galley> {
    let mut job = egui::text::LayoutJob::simple_singleline(s.to_string(), FontId::proportional(size), col);
    job.wrap.max_width = w.max(10.0);
    job.wrap.max_rows = 1;
    job.wrap.break_anywhere = true;
    p.layout_job(job)
}

impl App {
    // ---- iconos de aplicaciones ----

    /// Pide en segundo plano los iconos que falten de `apps`.
    fn ow_want_icons(&mut self, apps: &[extras::App]) {
        let mut want: Vec<extras::App> = Vec::new();
        for a in apps {
            let k = icon_key(a);
            if !k.is_empty() && self.ow.tried.insert(k.to_string()) {
                want.push(a.clone());
            }
        }
        if want.is_empty() {
            return;
        }
        // por tandas, para que las primeras (recomendadas) lleguen antes
        for chunk in want.chunks(24).map(|c| c.to_vec()) {
            self.spawn_io(
                move || chunk.iter().filter_map(|a| sysapps::load_icon(a, 48).map(|d| (icon_key(a).to_string(), d))).collect::<Vec<_>>(),
                |app, v| {
                    for (k, d) in v {
                        let img = egui::ColorImage::from_rgba_unmultiplied([d.w, d.h], &d.rgba);
                        let tex = app.ctx.load_texture(format!("ow-{k}"), img, egui::TextureOptions::LINEAR);
                        app.ow.icons.insert(k, tex);
                    }
                },
            );
        }
    }

    fn ow_icon(&mut self, a: &extras::App) -> Option<egui::TextureId> {
        // Windows: el .exe del manejador; macOS: el paquete .app (icono del sistema por ruta)
        #[cfg(any(windows, target_os = "macos"))]
        if a.icon.is_empty() && Path::new(&a.id).is_absolute() {
            let ctx = self.ctx.clone();
            return self.sys.for_path(&ctx, Path::new(&a.id));
        }
        self.ow.icons.get(icon_key(a)).map(|t| t.id())
    }

    // ---- lanzar ----

    /// Abre `path` con `app` (o la orden `custom`) en segundo plano; con `always`, la fija antes
    /// como predeterminada de `choices`. Los errores se avisan.
    fn ow_launch(&mut self, path: PathBuf, app: Option<extras::App>, custom: String, always: Option<Arc<Choices>>) {
        let term = self.cfg.sys_term.clone();
        if always.is_some() {
            // el submenú tiene que volver a mostrar la predeterminada nueva
            self.ow_cache = None;
        }
        let (p2, a2, c2) = (path.clone(), app.clone(), always.clone());
        self.spawn_io(
            move || {
                let mut warn = None;
                let mut system_ui = false;
                if let (Some(c), Some(a)) = (&always, &app) {
                    match sysapps::set_default(c, a) {
                        Ok(sysapps::Defaulted::Done) => {}
                        Ok(sysapps::Defaulted::NeedsSystemUi) => system_ui = true,
                        Err(e) => warn = Some(e),
                    }
                }
                if crate::sftp::is_remote(&path) {
                    crate::sftp::ensure_local(&path)?;
                }
                if system_ui {
                    // se abre desde la ventana del sistema, que es la que fija la predeterminada
                    return Ok(OwLaunched::NeedsSystemUi);
                }
                extras::launch_app(app.as_ref(), &custom, &path, &term)?;
                Ok::<_, String>(OwLaunched::Opened(warn))
            },
            move |app, r| match r {
                Ok(OwLaunched::Opened(None)) => {}
                Ok(OwLaunched::Opened(Some(e))) => app.say(tf("No se pudo fijar la aplicación predeterminada: {}", &[&e]), true),
                Ok(OwLaunched::NeedsSystemUi) => app.ow_default_via_system(p2, a2, c2),
                Err(e) => app.say(format!("{}: {e}", t("No se pudo abrir")), true),
            },
        );
    }

    /// Windows no deja a los programas cambiar la aplicación predeterminada (desde Windows 8 la
    /// elección del usuario está protegida): se abre su ventana «Abrir con» con «Usar siempre»
    /// marcado para que el usuario la confirme. Sólo se dice que cambió si de verdad cambió; si
    /// cierra la ventana sin elegir, el archivo se abre igualmente con la aplicación pedida.
    #[cfg_attr(not(windows), allow(unused_variables))]
    fn ow_default_via_system(&mut self, path: PathBuf, app: Option<extras::App>, always: Option<Arc<Choices>>) {
        #[cfg(windows)]
        {
            let name = app.as_ref().map(|a| a.name.clone()).unwrap_or_default();
            self.say(tf("Windows sólo deja cambiar la aplicación predeterminada desde su propia ventana: elige «{}» y deja marcado «Usar siempre esta aplicación»", &[&name]), false);
            let term = self.cfg.sys_term.clone();
            self.spawn_io(
                move || -> Result<Option<bool>, String> {
                    if sysapps::win_choose_default_dialog(&path)? {
                        let now = match (&always, &app) {
                            (Some(c), Some(a)) => sysapps::win_is_default(&c.mime, &a.id),
                            _ => false,
                        };
                        return Ok(Some(now));
                    }
                    // cerrada sin elegir: se abre con lo que se había pedido
                    extras::launch_app(app.as_ref(), "", &path, &term)?;
                    Ok(None)
                },
                move |a, r| {
                    // la predeterminada pudo cambiar (a la pedida o a otra): el submenú se vuelve a consultar
                    a.ow_cache = None;
                    match r {
                        Ok(Some(true)) => a.say(tf("«{}» es ahora la aplicación predeterminada", &[&name]), false),
                        // eligió otra aplicación: Windows ya la ha abierto y fijado, no hay nada que avisar
                        Ok(Some(false)) => {}
                        Ok(None) => a.say(tf("No se pudo fijar la aplicación predeterminada: {}", &[&t("cancelado")]), true),
                        Err(e) => a.say(format!("{}: {e}", t("No se pudo abrir")), true),
                    }
                },
            );
        }
    }

    // ---- diálogo ----

    pub(super) fn open_with(&mut self) {
        let Some(path) = self.selected_paths().into_iter().next() else { return };
        self.open_with_path(path);
    }

    /// «Abrir con…» para un archivo concreto (también cuando el sistema no tiene aplicación asociada).
    pub(super) fn open_with_path(&mut self, path: PathBuf) {
        if path.v_is_dir() {
            return;
        }
        // Android: el selector del sistema
        #[cfg(target_os = "android")]
        {
            if let Err(e) = crate::android_open(&path) {
                self.say(format!("{}: {e}", t("No se pudo abrir")), true);
            }
            return;
        }
        #[allow(unreachable_code)]
        if !sysapps::supported() {
            return;
        }
        self.dlg = Dlg::OpenWith(OwDlg::new(path.clone()));
        let p = path.clone();
        self.spawn_io(
            move || Arc::new(sysapps::choices_for(&p)),
            move |app, c| {
                app.ow_want_icons(&c.recommended);
                app.ow_want_icons(&c.others);
                if let Dlg::OpenWith(d) = &mut app.dlg {
                    if d.path == path && d.choices.is_none() {
                        d.sel = (!c.recommended.is_empty() || !c.others.is_empty()).then_some(0);
                        d.choices = Some(c);
                    }
                }
            },
        );
    }

    /// Fila de una aplicación: icono, nombre, descripción tenue y la marca de predeterminada.
    fn ow_row(&mut self, ui: &mut egui::Ui, a: &extras::App, selected: bool) -> egui::Response {
        let pal = self.pal;
        let h = 44f32.max(theme::min_target());
        let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), h), Sense::click());
        if ui.is_rect_visible(rect) {
            let icon = self.ow_icon(a);
            let p = ui.painter();
            let rad = CornerRadius::same(6);
            if selected {
                p.rect_filled(rect, rad, pal.sel);
            } else if resp.hovered() {
                p.rect_filled(rect, rad, pal.hover);
            }
            let ic = Rect::from_min_size(pos2(rect.left() + 8.0, rect.center().y - 14.0), vec2(28.0, 28.0));
            match icon {
                Some(id) => {
                    p.image(id, ic, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
                }
                None => letter_tile(p, ic, &a.name, &pal),
            }
            let x = ic.right() + 10.0;
            let mut right = rect.right() - 8.0;
            if a.default {
                let g = line(p, t("Predeterminada"), 11.5, pal.accent, 140.0);
                right -= g.size().x;
                p.galley(pos2(right, rect.center().y - g.size().y / 2.0), g, pal.accent);
                right -= 8.0;
            }
            let w = right - x;
            let name = line(p, &a.name, 14.0, pal.text, w);
            if a.generic.is_empty() || a.generic == a.name {
                p.galley(pos2(x, rect.center().y - name.size().y / 2.0), name, pal.text);
            } else {
                let sub = line(p, &a.generic, 11.5, pal.subtle, w);
                let top = rect.center().y - (name.size().y + sub.size().y + 1.0) / 2.0;
                let nh = name.size().y;
                p.galley(pos2(x, top), name, pal.text);
                p.galley(pos2(x, top + nh + 1.0), sub, pal.subtle);
            }
            theme::focus_ring(ui, &pal, &resp, 6.0);
        }
        resp
    }

    /// Dibuja el diálogo; devuelve si sigue abierto.
    pub(super) fn open_with_dialog(&mut self, ctx: &egui::Context, d: &mut OwDlg) -> bool {
        let pal = self.pal;
        let mut open = true;
        let (rec, others) = d.filtered();
        let total = rec.len() + others.len();
        if let Some(s) = d.sel {
            if s >= total {
                d.sel = total.checked_sub(1);
            }
        }
        // teclado: flechas para moverse por la lista, Intro para abrir (si no se escribe en otro campo)
        let focus = ctx.memory(|m| m.focused());
        let in_search = focus == Some(egui::Id::new("ow-search"));
        let focused_other = focus.is_some_and(|id| !in_search && egui::text_edit::TextEditState::load(ctx, id).is_some());
        let mut activate = false;
        if !focused_other && total > 0 {
            let (up, down, enter, home, end) = ctx.input_mut(|i| {
                (
                    i.consume_key(Modifiers::NONE, Key::ArrowUp),
                    i.consume_key(Modifiers::NONE, Key::ArrowDown),
                    in_search && i.key_pressed(Key::Enter),
                    i.consume_key(Modifiers::CTRL, Key::Home),
                    i.consume_key(Modifiers::CTRL, Key::End),
                )
            });
            let cur = d.sel.unwrap_or(0);
            let next = if up {
                Some(cur.saturating_sub(1))
            } else if down {
                Some(if d.sel.is_none() { 0 } else { (cur + 1).min(total - 1) })
            } else if home {
                Some(0)
            } else if end {
                Some(total - 1)
            } else {
                None
            };
            if let Some(n) = next {
                d.sel = Some(n);
                d.scroll = true;
            }
            activate = enter && d.sel.is_some();
        }
        let name = d.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let width = theme::dialog_width(ctx, 560.0);
        let list_h = (ctx.content_rect().height() - 330.0).clamp(110.0, 340.0);
        let mut pick: Option<extras::App> = None;
        let mut run_custom: Option<String> = None;
        let can_default = d.choices.as_ref().is_some_and(|c| c.can_default);
        let extras_labels: Vec<&str> = if cfg!(windows) { vec![t("Elegir otra aplicación del sistema…")] } else { Vec::new() };
        let primary_ok = d.sel.is_some() && total > 0;
        let act = theme::card_window_with_footer(
            ctx,
            &pal,
            t("Abrir con"),
            width,
            &mut open,
            |ui| {
                // cabecera: archivo y tipo
                ui.add(egui::Label::new(egui::RichText::new(&name).strong().size(15.0)).truncate());
                let ty = d.choices.as_ref().map(|c| if c.type_name.is_empty() { c.mime.clone() } else { c.type_name.clone() }).unwrap_or_default();
                if !ty.is_empty() {
                    ui.add(egui::Label::new(egui::RichText::new(ty).color(pal.subtle).size(12.5)).truncate());
                }
                ui.add_space(8.0);
                let r = ui.add(
                    egui::TextEdit::singleline(&mut d.query)
                        .id(egui::Id::new("ow-search"))
                        .hint_text(t("Buscar aplicación…"))
                        .desired_width(ui.available_width()),
                );
                if d.fresh {
                    r.request_focus();
                    d.fresh = false;
                }
                if r.changed() {
                    d.sel = Some(0);
                    d.scroll = true;
                }
                ui.add_space(4.0);
                egui::ScrollArea::vertical().id_salt("ow-list").max_height(list_h).min_scrolled_height(list_h.min(110.0)).auto_shrink([false, true]).show(ui, |ui| {
                    if d.choices.is_none() {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label(egui::RichText::new(t("Buscando aplicaciones…")).color(pal.subtle));
                        });
                        return;
                    }
                    if total == 0 {
                        ui.label(egui::RichText::new(t("No se encontraron aplicaciones")).color(pal.subtle));
                        return;
                    }
                    let mut i = 0usize;
                    for (title, list) in [(t("Recomendadas"), &rec), (t("Todas las aplicaciones"), &others)] {
                        if list.is_empty() {
                            continue;
                        }
                        theme::section(ui, &pal, title);
                        for a in list.iter() {
                            let selected = d.sel == Some(i);
                            let resp = self.ow_row(ui, a, selected);
                            if selected && d.scroll {
                                resp.scroll_to_me(None);
                                d.scroll = false;
                            }
                            if resp.clicked() {
                                d.sel = Some(i);
                            }
                            if resp.double_clicked() {
                                pick = Some(a.clone());
                            }
                            // Intro o Espacio sobre una fila con foco (navegación con Tab)
                            if resp.has_focus() && ui.input(|inp| inp.key_pressed(Key::Enter)) {
                                pick = Some(a.clone());
                            }
                            resp.on_hover_text(if a.exec.is_empty() { a.id.clone() } else { a.exec.clone() });
                            i += 1;
                        }
                    }
                });
                ui.add_space(6.0);
                if can_default {
                    ui.checkbox(&mut d.always, t("Usar siempre para este tipo de archivo"));
                }
                // Examinar…: ruta de un programa
                let r = ui.toggle_value(&mut d.browse_open, t("Examinar…"));
                if r.clicked() && d.browse_open && d.browse.is_empty() {
                    d.browse = if cfg!(windows) { "C:\\Program Files\\".into() } else { "/usr/bin/".into() };
                }
                if d.browse_open {
                    // se mira en disco en otro hilo; mientras tanto no se puede abrir (Err(""))
                    let info = browse_info(ui.ctx(), &d.browse);
                    let check = info.as_ref().map(|i| i.0.clone()).unwrap_or(Err(""));
                    ui.horizontal(|ui| {
                        let r = ui.add(egui::TextEdit::singleline(&mut d.browse).hint_text(t("Ruta del programa")).desired_width((ui.available_width() - 90.0).max(80.0)));
                        let enter = r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                        if ui.add_enabled(check.is_ok(), egui::Button::new(t("Abrir"))).clicked() || (enter && check.is_ok()) {
                            if let Ok(p) = &check {
                                run_custom = Some(program_cmd(p));
                            }
                        }
                    });
                    if let Err(e) = check {
                        // una carpeta es un paso normal al ir escribiendo la ruta: no es un error que avisar
                        if !e.is_empty() && e != "Es una carpeta" {
                            ui.label(egui::RichText::new(t(e)).color(theme::error_color(&pal)).size(12.0));
                        }
                    }
                    // carpetas y programas que empiezan por lo escrito (ayuda para completar la ruta)
                    let (dir, _) = browse_split(&d.browse);
                    if let Some((check, names)) = info.filter(|_| dir.is_absolute()) {
                        if !names.is_empty() && names.len() <= 400 && !(names.len() == 1 && check.is_ok()) {
                            egui::ScrollArea::vertical().id_salt("ow-browse").max_height(96.0).show(ui, |ui| {
                                ui.horizontal_wrapped(|ui| {
                                    for (n, is_dir) in names.iter().take(120) {
                                        let label = if *is_dir { format!("{n}/") } else { n.clone() };
                                        if ui.small_button(label).clicked() {
                                            let mut p = dir.join(n).to_string_lossy().into_owned();
                                            if *is_dir {
                                                p.push(std::path::MAIN_SEPARATOR);
                                            }
                                            d.browse = p;
                                        }
                                    }
                                });
                            });
                        }
                    }
                }
                egui::CollapsingHeader::new(t("Avanzado")).id_salt("ow-adv").show(ui, |ui| {
                    ui.label(egui::RichText::new(t("Orden personalizada (%f = el archivo; si falta, va al final)")).color(pal.subtle).size(12.0));
                    ui.horizontal(|ui| {
                        let r = ui.add(egui::TextEdit::singleline(&mut d.cmd).hint_text(t("Comando…")).desired_width((ui.available_width() - 90.0).max(80.0)));
                        let enter = r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                        let ok = !d.cmd.trim().is_empty();
                        if (ui.add_enabled(ok, egui::Button::new(t("Ejecutar"))).clicked() || enter) && ok {
                            run_custom = Some(d.cmd.trim().to_string());
                        }
                    });
                });
            },
            |ui| theme::DialogButtons::new(t("Cancelar"), t("Abrir")).extras(&extras_labels).primary_enabled(primary_ok).show(ui, &pal),
        );
        let sel_app = || d.sel.and_then(|i| rec.get(i).or_else(|| others.get(i.wrapping_sub(rec.len())))).cloned();
        if activate || act == Some(theme::DialogAction::Primary) {
            pick = pick.or_else(sel_app);
        }
        if let Some(a) = pick {
            let always = (d.always && can_default).then(|| d.choices.clone()).flatten();
            self.ow_launch(d.path.clone(), Some(a), String::new(), always);
            return false;
        }
        if let Some(c) = run_custom {
            self.ow_launch(d.path.clone(), None, c, None);
            return false;
        }
        match act {
            Some(theme::DialogAction::Cancel) => false,
            Some(theme::DialogAction::Extra(_)) => {
                // Windows: selector nativo (orden vacía → SHOpenWithDialog)
                self.ow_launch(d.path.clone(), None, String::new(), None);
                false
            }
            _ => open,
        }
    }

    // ---- submenú ----

    /// Submenú «Abrir con» del archivo seleccionado: aplicaciones con icono (la predeterminada
    /// marcada) y, al final, «Otra aplicación…». La consulta se hace en segundo plano.
    pub(super) fn open_with_menu(&mut self, ui: &mut egui::Ui) {
        if crate::vfs::WEB {
            return;
        }
        let Some(path) = self.selected_paths().into_iter().next() else { return };
        if cfg!(target_os = "android") {
            // el selector del sistema ya lista las aplicaciones
            if ui.button(t("Abrir con…")).clicked() {
                self.open_with();
            }
            return;
        }
        let mut run: Option<extras::App> = None;
        let mut other = false;
        ui.menu_button(t("Abrir con"), |ui| {
            ui.set_min_width(220.0);
            let fresh = self.ow_cache.as_ref().is_some_and(|(p, _)| p == &path);
            if !fresh && self.ow.menu_loading.as_ref() != Some(&path) {
                self.ow.menu_loading = Some(path.clone());
                let (p, key) = (path.clone(), path.clone());
                self.spawn_io(
                    move || extras::apps_for(&p),
                    move |app, apps| {
                        if app.ow.menu_loading.as_ref() == Some(&key) {
                            app.ow.menu_loading = None;
                            app.ow_want_icons(&apps);
                            app.ow_cache = Some((key, apps));
                        }
                    },
                );
            }
            if !fresh {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(egui::RichText::new(t("Buscando aplicaciones…")).color(self.pal.subtle));
                });
            } else {
                let apps = self.ow_cache.as_ref().map(|c| c.1.clone()).unwrap_or_default();
                egui::ScrollArea::vertical().max_height(320.0).auto_shrink([true, true]).show(ui, |ui| {
                    for a in &apps {
                        let text = if a.default { egui::RichText::new(&a.name).strong() } else { egui::RichText::new(&a.name) };
                        let mut b = match self.ow_icon(a) {
                            Some(id) => egui::Button::image_and_text(egui::Image::from_texture((id, vec2(16.0, 16.0))), text),
                            None => egui::Button::new(text),
                        };
                        if a.default {
                            b = b.shortcut_text(t("Predeterminada"));
                        }
                        if ui.add(b).clicked() {
                            run = Some(a.clone());
                            ui.close();
                        }
                    }
                });
                if !apps.is_empty() {
                    ui.separator();
                }
            }
            if ui.button(t("Otra aplicación…")).clicked() {
                other = true;
                ui.close();
            }
        });
        if let Some(a) = run {
            self.ow_launch(path, Some(a), String::new(), None);
        } else if other {
            self.open_with();
        }
    }

    // ---- terminal del sistema ----

    /// Carpeta real donde abrir el terminal del sistema para la ubicación `p`, o el motivo por el
    /// que no se puede (para el texto de ayuda de la entrada deshabilitada).
    pub(super) fn sys_term_dir(&self, p: &Path) -> Result<PathBuf, &'static str> {
        if !crate::systerm::supported() || crate::vfs::WEB {
            return Err("No disponible en este sistema");
        }
        if virt_mode(p).is_some() {
            return Err("No disponible en esta ubicación");
        }
        if crate::sftp::is_remote(p) {
            return Err("No disponible en una carpeta remota");
        }
        // dentro de un comprimido: la carpeta que lo contiene
        if let Some((_, archive)) = self.arch_of(p) {
            return archive.parent().map(|d| d.to_path_buf()).ok_or("No disponible en esta ubicación");
        }
        if p.is_dir() { Ok(p.to_path_buf()) } else { Err("No disponible en esta ubicación") }
    }

    /// Entrada de menú «Abrir en el terminal del sistema» para la carpeta `p` (oculta en Android y
    /// en la web; deshabilitada con explicación en ubicaciones virtuales o remotas).
    pub(super) fn sys_term_menu_item(&mut self, ui: &mut egui::Ui, p: &Path) {
        if !crate::systerm::supported() || crate::vfs::WEB {
            return;
        }
        let sc = commands::shortcut_text(Cmd::SysTerminal);
        match self.sys_term_dir(p) {
            Ok(dir) => {
                if mi(ui, t("Abrir en el terminal del sistema"), &sc, true) {
                    self.sys_terminal_at(dir);
                    ui.close();
                }
            }
            Err(why) => {
                ui.add_enabled(false, egui::Button::new(t("Abrir en el terminal del sistema"))).on_disabled_hover_text(t(why));
            }
        }
    }

    /// Orden «Abrir en el terminal del sistema» sobre la carpeta actual.
    pub(super) fn sys_terminal_here(&mut self) {
        match self.sys_term_dir(&self.cwd.clone()) {
            Ok(d) => self.sys_terminal_at(d),
            Err(why) => {
                if crate::systerm::supported() && !crate::vfs::WEB {
                    self.say(t(why).into(), true);
                }
            }
        }
    }

    /// Abre el terminal del sistema en `dir`; si falla, lo avisa y abre el integrado allí.
    pub(super) fn sys_terminal_at(&mut self, dir: PathBuf) {
        let term = self.cfg.sys_term.clone();
        self.spawn_io(move || crate::systerm::open_dir(&term, &dir).map_err(|e| (e, dir)), |app, r| {
            if let Err((e, dir)) = r {
                app.say(tf("No se pudo abrir el terminal del sistema: {}", &[&e]), true);
                app.integrated_terminal_at(&dir);
            }
        });
    }

    /// Terminal integrado con `cd` a `dir`.
    fn integrated_terminal_at(&mut self, dir: &Path) {
        if crate::vfs::WEB {
            return;
        }
        self.cfg.term_open = true;
        self.ensure_term();
        let cmd = shell_quote(dir, &self.term_shell);
        if cmd.is_empty() {
            self.say(t("La ruta tiene caracteres de control: no se escribe en el terminal").into(), true);
        } else if let Some(t) = &mut self.term {
            t.send(cmd.as_bytes());
            self.term_focus = true;
        }
        self.save_cfg();
    }

    /// Fila de Opciones: terminal del sistema (vacío = automático). Devuelve si cambió.
    pub(super) fn sys_term_option(&mut self, ui: &mut egui::Ui, width: f32) -> bool {
        if !crate::systerm::supported() || crate::vfs::WEB {
            return false;
        }
        ui.add_space(6.0);
        ui.label(t("Terminal del sistema (vacío = automático)"));
        let hint = match crate::systerm::auto_label() {
            Some(l) if !l.is_empty() => tf("Automático: {}", &[&l]),
            Some(_) => t("No se encontró ningún terminal").to_string(),
            None => t("Automático").to_string(),
        };
        if crate::systerm::auto_label().is_none() {
            // la detección termina en segundo plano: se vuelve a pintar para mostrarla
            ui.ctx().request_repaint_after(Duration::from_millis(300));
        }
        let changed = ui.add(egui::TextEdit::singleline(&mut self.cfg.sys_term).hint_text(hint).desired_width(width - 20.0)).changed();
        ui.label(egui::RichText::new(tf("Use {} en la orden para la carpeta", &[&"{dir}"])).color(self.pal.subtle).size(12.0));
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn orden_de_programa_examinado() {
        let p = Path::new("/opt/Mi App/bin/ed\"i`t$o%r\\x");
        let c = program_cmd(p);
        #[cfg(not(windows))]
        {
            let argv = extras::exec_argv(&c, Path::new("/tmp/a b.txt"), "", "", Path::new("")).unwrap();
            assert_eq!(argv, [p.as_os_str(), std::ffi::OsStr::new("/tmp/a b.txt")]);
        }
        let _ = c;
        // Windows: unidad y ruta UNC vuelven intactas de split_words
        for w in [r"C:\Program Files\App\app.exe", r"\\srv\share\app.exe"] {
            assert_eq!(crate::systerm::split_words(&win_quote(w)), Some(vec![w.to_string()]), "{w}");
        }
        assert!(check_program("").is_err());
        assert!(check_program("relativo/x").is_err());
        #[cfg(unix)]
        {
            assert!(check_program("/bin/sh").is_ok());
            assert_eq!(check_program("/"), Err("Es una carpeta"));
        }
        let a = extras::App { name: "Editor de textos".into(), generic: "Editor".into(), id: "org.gnome.TextEditor.desktop".into(), ..Default::default() };
        assert!(app_matches(&a, "") && app_matches(&a, "textos") && app_matches(&a, "gnome") && !app_matches(&a, "vlc"));
    }
}
