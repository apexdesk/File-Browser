//! Experiencia de uso (módulo hijo de `app`): avisos apilados, errores del sistema traducidos,
//! decisión de copiar o mover al soltar, recuento de la barra de estado, vistas vacías, emblema de los
//! enlaces simbólicos y los diálogos de conflicto, de borrado definitivo y de compartir la carpeta.

use super::*;
use crate::i18n::{tn, tn_in};
use std::io::ErrorKind;
use web_time::UNIX_EPOCH;

// ---------------------------------------------------------------------------
// Estado propio (un único campo `ux` de `App`)
// ---------------------------------------------------------------------------

#[derive(Default)]
pub(super) struct UxState {
    pub(super) toasts: Toasts,
    /// cambia cada vez que cambia la pila de deshacer (invalida el «Deshacer» de los avisos anteriores)
    pub(super) undo_gen: u64,
    /// aviso informativo del último `say`: se retira cuando alguien borra `msg`
    say_id: Option<u64>,
    /// qué anunciar cuando termine la tarea en curso
    pub(super) op_ann: Ann,
    /// por qué se pide confirmar un borrado definitivo
    pub(super) del_note: DelNote,
    /// volumen de cada carpeta (para decidir copiar o mover al soltar), `None` si no se sabe
    devs: HashMap<PathBuf, Option<u64>>,
    dev_wait: HashSet<PathBuf>,
    /// enlaces simbólicos de la carpeta `links_dir`: ¿roto?
    pub(super) links: HashMap<PathBuf, bool>,
    link_wait: HashSet<PathBuf>,
    link_fly: HashSet<PathBuf>,
    links_dir: PathBuf,
    /// recuento de la barra de estado ya calculado: (clave, texto)
    status: Option<((usize, usize, usize, bool, usize), String)>,
    #[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
    pub(super) share: Option<ShareDlg>,
}

// ---------------------------------------------------------------------------
// Avisos apilados
// ---------------------------------------------------------------------------

pub(super) struct Toast {
    pub(super) id: u64,
    pub(super) text: String,
    /// texto original del sistema (se ve al pasar el ratón)
    pub(super) detail: Option<String>,
    pub(super) err: bool,
    pub(super) born: Instant,
    /// «Deshacer» vale mientras la pila de deshacer tenga este tamaño
    /// (generación, longitud) de la pila de deshacer cuando se mostró: el botón «Deshacer» sólo vale
    /// mientras la pila siga exactamente igual
    pub(super) undo_at: Option<(u64, usize)>,
}

#[derive(Default)]
pub(super) struct Toasts {
    pub(super) v: Vec<Toast>,
    next: u64,
}

pub(super) const TOAST_INFO: Duration = Duration::from_secs(4);
pub(super) const TOAST_UNDO: Duration = Duration::from_secs(8);
pub(super) const TOAST_ERR: Duration = Duration::from_secs(15);
const TOAST_MAX: usize = 4;

impl Toasts {
    fn ttl(t: &Toast) -> Duration {
        if t.err {
            TOAST_ERR
        } else if t.undo_at.is_some() {
            TOAST_UNDO
        } else {
            TOAST_INFO
        }
    }

    /// Añade un aviso; el mismo texto otra vez se renueva en vez de apilarse.
    pub(super) fn push(&mut self, text: String, detail: Option<String>, err: bool, undo_at: Option<(u64, usize)>, now: Instant) -> u64 {
        if let Some(t) = self.v.iter_mut().find(|t| t.text == text && t.err == err) {
            t.born = now;
            t.detail = detail;
            t.undo_at = undo_at;
            return t.id;
        }
        self.next = self.next.wrapping_add(1);
        let id = self.next;
        self.v.push(Toast { id, text, detail, err, born: now, undo_at });
        while self.v.len() > TOAST_MAX {
            // sobra uno: se va el informativo más antiguo (si todos son errores, el más antiguo)
            let i = self.v.iter().position(|t| !t.err).unwrap_or(0);
            self.v.remove(i);
        }
        id
    }

    pub(super) fn dismiss(&mut self, id: u64) {
        self.v.retain(|t| t.id != id);
    }

    /// Quita los caducados.
    pub(super) fn expire(&mut self, now: Instant) {
        self.v.retain(|t| now.saturating_duration_since(t.born) < Self::ttl(t));
    }

    /// Cuánto falta para que caduque el siguiente.
    pub(super) fn next_due(&self, now: Instant) -> Option<Duration> {
        self.v.iter().map(|t| Self::ttl(t).saturating_sub(now.saturating_duration_since(t.born))).min()
    }
}

// ---------------------------------------------------------------------------
// Errores de E/S con texto traducido
// ---------------------------------------------------------------------------

/// Texto traducido de un tipo de error de E/S (None: no hay uno mejor que el del sistema).
pub(super) fn kind_text(k: ErrorKind) -> Option<&'static str> {
    use ErrorKind as K;
    Some(match k {
        K::PermissionDenied => t("No tienes permiso para hacerlo"),
        K::NotFound => t("El elemento ya no existe"),
        K::AlreadyExists => t("Ya existe un elemento con ese nombre"),
        K::StorageFull | K::QuotaExceeded => t("No queda espacio en el disco"),
        K::ReadOnlyFilesystem => t("El disco es de sólo lectura"),
        K::DirectoryNotEmpty => t("La carpeta no está vacía"),
        K::CrossesDevices => t("No se puede mover entre discos distintos"),
        K::InvalidFilename => t("Nombre no válido"),
        K::ResourceBusy | K::ExecutableFileBusy => t("El elemento está en uso"),
        K::TimedOut => t("Se agotó el tiempo de espera"),
        K::IsADirectory => t("Es una carpeta"),
        K::NotADirectory => t("No es una carpeta"),
        K::FileTooLarge => t("El archivo es demasiado grande para ese disco"),
        K::ConnectionRefused
        | K::ConnectionReset
        | K::ConnectionAborted
        | K::NotConnected
        | K::NetworkUnreachable
        | K::HostUnreachable
        | K::NetworkDown
        | K::StaleNetworkFileHandle
        | K::BrokenPipe => t("Se perdió la conexión"),
        _ => return None,
    })
}

/// Texto de un error de E/S para el usuario: el traducido si lo hay, si no el del sistema.
#[allow(dead_code)]
pub(super) fn io_text(e: &std::io::Error) -> String {
    kind_text(e.kind()).map(str::to_string).unwrap_or_else(|| e.to_string())
}

/// Cambia en un mensaje el texto del sistema «… (os error N)» por el traducido. Devuelve el mensaje y,
/// si ha cambiado, el original entero como detalle.
pub(super) fn localize(s: &str) -> (String, Option<String>) {
    const MARK: &str = "(os error ";
    let same = || (s.to_string(), None);
    let Some(i) = s.find(MARK) else { return same() };
    let rest = &s[i + MARK.len()..];
    let Some(j) = rest.find(')') else { return same() };
    let Ok(code) = rest[..j].trim().parse::<i32>() else { return same() };
    let Some(txt) = kind_text(std::io::Error::from_raw_os_error(code).kind()) else { return same() };
    // el texto del sistema empieza tras el último «: » anterior
    let start = s[..i].rfind(": ").map(|k| k + 2).unwrap_or(0);
    let end = i + MARK.len() + j + 1;
    (format!("{}{}{}", &s[..start], txt, &s[end..]), Some(s.to_string()))
}

/// Mensaje de «sólo lectura»: el de SFTP sólo si la ubicación es de un servidor SFTP.
pub(super) fn ro_text(sftp: bool) -> &'static str {
    if sftp { t("Los servidores SFTP son de sólo lectura") } else { t("Esta ubicación es de sólo lectura") }
}

// ---------------------------------------------------------------------------
// Arrastrar y soltar dentro del gestor
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum DropAct {
    Copy,
    Move,
}

/// Copiar o mover al soltar: con la tecla de copiar (Ctrl; Opción en macOS) se copia, con Mayús se
/// mueve y, sin teclas, se mueve dentro del mismo volumen y se copia a otro distinto (como en los
/// gestores habituales). Si no se sabe el volumen, se mueve.
pub(super) fn drop_action(copy_key: bool, move_key: bool, same_volume: Option<bool>) -> DropAct {
    if move_key {
        DropAct::Move
    } else if copy_key || same_volume == Some(false) {
        DropAct::Copy
    } else {
        DropAct::Move
    }
}

/// (copiar, mover) según las teclas pulsadas al soltar.
pub(super) fn drop_keys(m: Modifiers) -> (bool, bool) {
    (if cfg!(target_os = "macos") { m.alt } else { m.ctrl }, m.shift)
}

/// Identificador del volumen que contiene la ruta (sin seguir el último enlace).
pub(super) fn volume_of(p: &Path) -> Option<u64> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        std::fs::symlink_metadata(p).ok().map(|m| m.dev())
    }
    #[cfg(windows)]
    {
        use std::hash::{Hash, Hasher};
        match p.components().next() {
            Some(Component::Prefix(pf)) => {
                let mut h = std::collections::hash_map::DefaultHasher::new();
                pf.as_os_str().to_string_lossy().to_uppercase().hash(&mut h);
                Some(h.finish())
            }
            _ => None,
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = p;
        None
    }
}

/// Volumen de una carpeta (destino o la que contiene los orígenes) siguiendo el enlace: soltar
/// sobre `~/usb` → `/media/usb` va a otro volumen aunque el enlace esté en el de la carpeta personal.
pub(super) fn dir_volume(p: &Path) -> Option<u64> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        std::fs::metadata(p).ok().map(|m| m.dev())
    }
    #[cfg(not(unix))]
    {
        volume_of(p)
    }
}

/// ¿Están todos los orígenes en el mismo volumen que el destino? (None si no se puede saber)
pub(super) fn same_volume(srcs: &[PathBuf], dst: &Path) -> Option<bool> {
    let d = dir_volume(dst)?;
    let mut all = true;
    for s in srcs {
        all &= volume_of(s)? == d;
    }
    Some(all)
}

// ---------------------------------------------------------------------------
// Barra de estado
// ---------------------------------------------------------------------------

/// «19 elementos (1 oculto, 3 filtrados)» en el idioma `lang`.
pub(super) fn status_count(lang: usize, shown: usize, hidden: usize, filtered: usize) -> String {
    let mut s = tn_in(lang, "{} elemento|{} elementos", shown, &[]);
    let mut extra = Vec::new();
    if hidden > 0 {
        extra.push(tn_in(lang, "{} oculto|{} ocultos", hidden, &[]));
    }
    if filtered > 0 {
        extra.push(tn_in(lang, "{} filtrado|{} filtrados", filtered, &[]));
    }
    if !extra.is_empty() {
        s.push_str(&format!(" ({})", extra.join(", ")));
    }
    s
}

// ---------------------------------------------------------------------------
// Fin de las tareas y borrado
// ---------------------------------------------------------------------------

/// Qué anunciar cuando acabe la tarea en curso.
#[derive(Default)]
pub(super) enum Ann {
    #[default]
    None,
    Copy { mv: bool },
    /// lo que se pidió mandar a la papelera (lo que falle puede borrarse definitivamente)
    Trash(Vec<PathBuf>),
    Delete(usize),
}

impl Ann {
    pub(super) fn of(op: &Op, meta: &OpMeta) -> Ann {
        match (op, meta) {
            (Op::Copy { mv, .. }, OpMeta::Copy { replay: false, .. } | OpMeta::Replace { .. }) => Ann::Copy { mv: *mv },
            (Op::Trash(v), OpMeta::Trash { replay: false }) => Ann::Trash(v.clone()),
            (Op::Delete(v), _) => Ann::Delete(v.len()),
            _ => Ann::None,
        }
    }
}

#[derive(Default, Clone, Copy, PartialEq, Eq)]
pub(super) enum DelNote {
    #[default]
    Plain,
    /// no hay papelera (Android, navegador): «Mover a la papelera» pasa por aquí
    NoTrash,
    /// la papelera no los aceptó (p. ej. un volumen sin papelera)
    TrashFailed,
}

// ---------------------------------------------------------------------------
// Conflictos al copiar o mover
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Default)]
pub(super) struct Side {
    pub(super) dir: bool,
    pub(super) size: u64,
    pub(super) mtime: i64,
}

/// Un elemento que ya existe en el destino: lo que llega (`new`) y lo que hay (`old`).
pub(super) struct Clash {
    pub(super) name: String,
    pub(super) new: Side,
    pub(super) old: Side,
}

#[derive(Default)]
pub(super) struct ConflictUx {
    /// en el mismo orden que `ConflictDlg::names` (vacío si no se pudo leer)
    pub(super) info: Vec<Clash>,
    /// conflicto que se está preguntando
    pub(super) pos: usize,
    pub(super) all: bool,
    /// «Combinar» con «Aplicar a todos»: vale para las demás carpetas; los archivos se siguen preguntando
    pub(super) merge_all: bool,
    /// decisión tomada para cada conflicto anterior a `pos`
    pub(super) picks: Vec<Policy>,
}

fn side_of(p: &Path) -> Option<Side> {
    let m = crate::vfs::fs::symlink_metadata(p).ok()?;
    let dir = m.is_dir();
    let mtime = m.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map(|d| d.as_secs().min(i64::MAX as u64) as i64).unwrap_or(0);
    Some(Side { dir, size: if dir { 0 } else { m.len() }, mtime })
}

/// Los orígenes que ya existen en el destino, con tamaño y fecha de los dos (E/S: en un hilo).
pub(super) fn clashes(srcs: &[PathBuf], dst: &Path) -> Vec<Clash> {
    srcs.iter()
        .filter(|s| s.parent() != Some(dst))
        .filter_map(|s| {
            let n = s.file_name()?;
            let old = side_of(&dst.join(n))?;
            Some(Clash { name: n.to_string_lossy().into_owned(), new: side_of(s).unwrap_or_default(), old })
        })
        .collect()
}

fn side_text(s: &Side, newer: bool) -> String {
    let mut v = vec![if s.dir { t("Carpeta").to_string() } else { fsys::human_size(s.size) }];
    let d = fsys::fmt_time(s.mtime);
    if !d.is_empty() {
        v.push(d);
    }
    if newer {
        v.push(t("más reciente").to_string());
    }
    v.join("  ·  ")
}

/// Fila de botones como `theme::DialogButtons`, pero cada botón con su estilo (un «Reemplazar» rojo
/// junto a una acción principal segura). Devuelve el índice del pulsado.
fn button_row(ui: &mut egui::Ui, pal: &Pal, items: &[(&str, theme::Btn)]) -> Option<usize> {
    let h = theme::min_target().max(34.0);
    let sp = 10.0;
    let total: f32 = items
        .iter()
        .map(|(s, _)| {
            let g = ui.painter().layout_no_wrap(s.to_string(), FontId::proportional(14.0), Color32::WHITE);
            (g.size().x + 36.0).max(84.0).max(h)
        })
        .sum::<f32>()
        + sp * items.len().saturating_sub(1) as f32;
    let avail = ui.available_width();
    let mut hit = None;
    let mut add = |ui: &mut egui::Ui| {
        ui.spacing_mut().item_spacing.x = sp;
        for (i, (s, k)) in items.iter().enumerate() {
            if theme::dialog_button(ui, pal, *k, s).clicked() {
                hit = Some(i);
            }
        }
    };
    if total <= avail - 1.0 {
        ui.horizontal(|ui| {
            ui.add_space(avail - total - 1.0);
            add(ui);
        });
    } else {
        ui.horizontal_wrapped(|ui| add(ui));
    }
    hit
}

// ---------------------------------------------------------------------------
// Compartir la carpeta con `filebrowser-server`
// ---------------------------------------------------------------------------

#[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
pub(super) struct ShareDlg {
    dir: PathBuf,
    exe: PathBuf,
    lan: bool,
    port: String,
    write: bool,
    tls: bool,
    auth: bool,
    user: String,
    pass: String,
    ttl: bool,
    pub(super) open: bool,
    run: Option<ShareRun>,
}

#[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
struct ShareRun {
    child: std::process::Child,
    lines: Arc<std::sync::Mutex<Vec<String>>>,
    /// el servidor terminó (estado de salida)
    ended: Option<String>,
}

#[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
impl Drop for ShareRun {
    fn drop(&mut self) {
        // al dejar de compartir (o al salir del gestor) el servidor no se queda sirviendo
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
impl ShareDlg {
    pub(super) fn new(dir: PathBuf, exe: PathBuf) -> Self {
        ShareDlg { dir, exe, lan: true, port: String::new(), write: false, tls: false, auth: false, user: String::new(), pass: String::new(), ttl: true, open: true, run: None }
    }

    pub(super) fn running(&self) -> bool {
        self.run.as_ref().is_some_and(|r| r.ended.is_none())
    }

    /// Qué impide compartir con estas opciones (None: todo bien).
    fn problem(&self) -> Option<&'static str> {
        if !self.port.trim().is_empty() && self.port.trim().parse::<u16>().is_err() {
            return Some(t("Puerto no válido"));
        }
        if self.auth {
            let u = self.user.trim();
            if u.is_empty() || u.contains(':') || u.chars().any(char::is_control) {
                return Some(t("Escribe un nombre de usuario (sin «:»)"));
            }
            if self.pass.chars().count() < 8 {
                return Some(t("La contraseña debe tener al menos 8 caracteres"));
            }
        }
        None
    }

    /// Arranca el servidor con estas opciones. La contraseña va en la variable FB_PASSWORD, nunca en
    /// la línea de órdenes (la verían otros usuarios del equipo).
    fn start(&mut self, ctx: &egui::Context) -> std::io::Result<()> {
        use std::io::{BufRead, Read};
        let mut cmd = std::process::Command::new(&self.exe);
        cmd.args(["--bind", if self.lan { "0.0.0.0" } else { "127.0.0.1" }]);
        if let Ok(p) = self.port.trim().parse::<u16>() {
            cmd.args(["--port", &p.to_string()]);
        }
        if self.ttl {
            cmd.args(["--ttl", "2h"]);
        }
        if self.write {
            cmd.arg("--write");
        }
        if self.tls {
            cmd.arg("--tls");
        }
        if self.auth {
            cmd.arg("--user").arg(self.user.trim());
            cmd.env("FB_PASSWORD", &self.pass);
        } else {
            cmd.env_remove("FB_PASSWORD");
        }
        cmd.arg(&self.dir);
        cmd.stdin(std::process::Stdio::null()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            // sin ventana de consola
            cmd.creation_flags(0x0800_0000);
        }
        let mut child = cmd.spawn()?;
        let lines = Arc::new(std::sync::Mutex::new(Vec::new()));
        let outs: [Option<Box<dyn Read + Send>>; 2] = [child.stdout.take().map(|s| Box::new(s) as Box<dyn Read + Send>), child.stderr.take().map(|s| Box::new(s) as Box<dyn Read + Send>)];
        for r in outs.into_iter().flatten() {
            let (l, c) = (lines.clone(), ctx.clone());
            std::thread::spawn(move || {
                for line in std::io::BufReader::new(r).lines().map_while(Result::ok) {
                    let mut g = l.lock().unwrap_or_else(|e| e.into_inner());
                    if g.len() < 400 {
                        g.push(line);
                    }
                    drop(g);
                    c.request_repaint();
                }
            });
        }
        self.pass.clear();
        self.run = Some(ShareRun { child, lines, ended: None });
        Ok(())
    }
}

/// Las líneas del código QR que imprime el servidor (bloques «█▀▄»).
#[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
fn is_qr_line(l: &str) -> bool {
    l.contains(['█', '▀', '▄'])
}

/// Pinta el QR impreso con medios bloques (cada carácter: una columna y dos filas de módulos).
#[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
fn paint_qr(ui: &mut egui::Ui, rows: &[&str]) {
    let w = rows.iter().map(|r| r.chars().count()).max().unwrap_or(0);
    if w == 0 {
        return;
    }
    let h = rows.len() * 2;
    let m = (180.0 / w.max(h) as f32).clamp(2.0, 5.0);
    let quiet = 4.0 * m;
    let size = vec2(w as f32 * m + 2.0 * quiet, h as f32 * m + 2.0 * quiet);
    let (r, _) = ui.allocate_exact_size(size, Sense::hover());
    let p = ui.painter();
    p.rect_filled(r, 4.0, Color32::WHITE);
    let o = r.min + vec2(quiet, quiet);
    for (y, row) in rows.iter().enumerate() {
        for (x, c) in row.chars().enumerate() {
            let (top, bottom) = match c {
                '█' => (true, true),
                '▀' => (true, false),
                '▄' => (false, true),
                _ => (false, false),
            };
            let at = |dy: usize| Rect::from_min_size(o + vec2(x as f32 * m, (y * 2 + dy) as f32 * m), vec2(m, m));
            if top {
                p.rect_filled(at(0), 0.0, Color32::BLACK);
            }
            if bottom {
                p.rect_filled(at(1), 0.0, Color32::BLACK);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Partes de `App`
// ---------------------------------------------------------------------------

impl App {
    /// Muestra un mensaje como aviso apilado (lo usa `say`).
    pub(super) fn ux_say(&mut self, s: &str, err: bool) {
        let (text, detail) = localize(s);
        let id = self.ux.toasts.push(text, detail, err, None, Instant::now());
        self.ux.say_id = if err { None } else { Some(id) };
    }

    /// Anuncia el final de una tarea; con `undo`, el aviso lleva «Deshacer».
    pub(super) fn ux_announce(&mut self, text: String, undo: bool) {
        let at = undo.then_some((self.ux.undo_gen, self.undo_stack.len()));
        self.ux.toasts.push(text, None, false, at, Instant::now());
        self.ux.say_id = None;
        self.ctx.request_repaint();
    }

    /// Final de una tarea de archivos: anuncio con «Deshacer» o, si la papelera no aceptó algo,
    /// ofrecer el borrado definitivo de lo que quedó.
    pub(super) fn ux_op_done(&mut self, ann: Ann, errs: &[String], pairs: &[(PathBuf, PathBuf)], created: &[String], cancelled: bool) {
        let ok = errs.is_empty() && !cancelled;
        match ann {
            Ann::Copy { mv } if ok && !created.is_empty() => {
                let n = created.len();
                let s = if mv { tn("{} elemento movido|{} elementos movidos", n, &[]) } else { tn("{} elemento copiado|{} elementos copiados", n, &[]) };
                self.ux_announce(s, !pairs.is_empty());
            }
            Ann::Trash(list) => {
                if ok && !pairs.is_empty() {
                    self.ux_announce(tn("{} elemento movido a la papelera|{} elementos movidos a la papelera", pairs.len(), &[]), true);
                } else if !cancelled && !errs.is_empty() {
                    let done: HashSet<&PathBuf> = pairs.iter().map(|(a, _)| a).collect();
                    let left: Vec<PathBuf> = list.into_iter().filter(|p| !done.contains(p)).collect();
                    self.spawn_io(
                        move || left.into_iter().filter(|p| crate::vfs::fs::symlink_metadata(p).is_ok()).collect::<Vec<_>>(),
                        |a, v| {
                            if !v.is_empty() && a.confirm_delete.is_none() {
                                a.ux.del_note = DelNote::TrashFailed;
                                a.confirm_delete = Some(v);
                            }
                        },
                    );
                }
            }
            Ann::Delete(n) if ok => self.ux_announce(tn("{} elemento eliminado|{} elementos eliminados", n, &[]), false),
            _ => {}
        }
    }

    /// Lo de cada fotograma: avisos, comprobación de enlaces y diálogo de compartir.
    pub(super) fn ux_frame(&mut self, ctx: &egui::Context) {
        if self.msg.is_none() {
            if let Some(id) = self.ux.say_id.take() {
                self.ux.toasts.dismiss(id);
            }
        }
        self.ux_flush_links();
        // lo que el sistema no pudo abrir: se avisa y, si no hay aplicación asociada, «Abrir con…»
        fsys::set_open_ctx(ctx);
        for f in fsys::take_open_fails() {
            let name = f.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            self.say(format!("{} «{name}»: {}", t("No se pudo abrir"), f.msg), true);
            if f.pick && matches!(self.dlg, Dlg::None) {
                self.open_with_path(f.path);
            }
        }
        #[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
        self.ux_share_dialog(ctx);
        self.ux_toasts(ctx);
    }

    /// ¿Hay un diálogo propio abierto? (el resto de la interfaz no recibe teclas)
    pub(super) fn ux_modal(&self) -> bool {
        #[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
        if self.ux.share.as_ref().is_some_and(|d| d.open) {
            return true;
        }
        false
    }

    fn ux_toasts(&mut self, ctx: &egui::Context) {
        let now = Instant::now();
        self.ux.toasts.expire(now);
        if self.ux.toasts.v.is_empty() {
            return;
        }
        let pal = self.pal;
        let screen = ctx.content_rect();
        let phone = self.phone;
        let bottom = self.insets[3] + if phone { 72.0 } else { 40.0 };
        let w = if phone { (screen.width() - 32.0).max(120.0) } else { 400.0f32.min(screen.width() - 32.0).max(120.0) };
        let (anchor, off) = if phone { (Align2::CENTER_BOTTOM, vec2(0.0, -bottom)) } else { (Align2::RIGHT_BOTTOM, vec2(-16.0, -bottom)) };
        let can_undo = (self.ux.undo_gen, self.undo_stack.len());
        let (mut close, mut undo) = (None, false);
        let shadow = ctx.global_style().visuals.popup_shadow;
        let area = egui::Area::new(egui::Id::new("ux_toasts")).order(egui::Order::Foreground).anchor(anchor, off).show(ctx, |ui| {
            ui.set_width(w);
            for tst in &self.ux.toasts.v {
                let col = if tst.err { pal.danger } else { pal.text };
                egui::Frame::new()
                    .fill(pal.panel)
                    .stroke(Stroke::new(if tst.err { 1.5 } else { 1.0 }, if tst.err { pal.danger } else { pal.border }))
                    .corner_radius(CornerRadius::same(12))
                    .shadow(shadow)
                    .inner_margin(egui::Margin::symmetric(12, 8))
                    .show(ui, |ui| {
                        ui.set_width(w - 24.0);
                        ui.horizontal_top(|ui| {
                            let tw = (w - 24.0 - theme::min_target().max(28.0) - 10.0).max(60.0);
                            ui.vertical(|ui| {
                                ui.set_width(tw);
                                let lr = ui.add(egui::Label::new(egui::RichText::new(&tst.text).color(col)).wrap());
                                // lector de pantalla: el aviso se anuncia al aparecer (los errores, con prioridad)
                                let live = if tst.err { egui::accesskit::Live::Assertive } else { egui::accesskit::Live::Polite };
                                ui.ctx().accesskit_node_builder(lr.id, |b| b.set_live(live));
                                if let Some(d) = &tst.detail {
                                    lr.on_hover_text(d);
                                }
                                if tst.undo_at == Some(can_undo) && can_undo.1 > 0 && ui.button(t("Deshacer")).clicked() {
                                    undo = true;
                                    close = Some(tst.id);
                                }
                            });
                            if theme::icon_button(ui, &pal, theme::Icon::Close, t("Cerrar")).clicked() {
                                close = Some(tst.id);
                            }
                        });
                    });
                ui.add_space(6.0);
            }
        });
        // con el puntero encima no caducan (da tiempo a leer y a pulsar)
        if ctx.pointer_hover_pos().is_some_and(|q| area.response.rect.contains(q)) {
            for tst in &mut self.ux.toasts.v {
                tst.born = now;
            }
        }
        if let Some(id) = close {
            self.ux.toasts.dismiss(id);
        }
        if undo {
            self.undo();
        }
        if let Some(d) = self.ux.toasts.next_due(now) {
            ctx.request_repaint_after(d + Duration::from_millis(30));
        }
    }

    /// Botón «×» dentro del borde derecho de un campo de texto (el campo deja ese margen libre).
    /// Devuelve true si se ha pulsado; el foco vuelve al campo.
    pub(super) fn ux_clear_button(&self, ui: &mut egui::Ui, edit: &egui::Response) -> bool {
        let pal = self.pal;
        let side = theme::target_size(vec2(24.0, 24.0)).x.min(edit.rect.height().max(24.0));
        let rc = Rect::from_center_size(pos2(edit.rect.right() - side / 2.0 - 2.0, edit.rect.center().y), vec2(side, side));
        let resp = ui.interact(rc, edit.id.with("ux_clear"), Sense::click());
        resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, t("Borrar filtro")));
        let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_text(t("Borrar filtro"));
        let c = rc.center();
        if resp.hovered() {
            ui.painter().circle_filled(c, 10.0, pal.hover);
        }
        let st = Stroke::new(1.6, if resp.hovered() { pal.text } else { pal.subtle });
        ui.painter().line_segment([c + vec2(-4.0, -4.0), c + vec2(4.0, 4.0)], st);
        ui.painter().line_segment([c + vec2(-4.0, 4.0), c + vec2(4.0, -4.0)], st);
        theme::focus_ring(ui, &pal, &resp, side / 2.0);
        if resp.clicked() {
            edit.request_focus();
            true
        } else {
            false
        }
    }

    /// Texto del filtro con el atajo vigente.
    pub(super) fn ux_filter_hint(&self) -> String {
        let k = commands::shortcut_text_in(Cmd::Filter, &self.cfg.keys);
        if k.is_empty() { t("Filtrar…").to_string() } else { format!("{}  ({k})", t("Filtrar…")) }
    }

    /// Recuento de la barra de estado (se recalcula sólo cuando cambia la lista).
    pub(super) fn ux_status_count(&mut self) -> String {
        let total = self.entries.len();
        let shown = self.order.len();
        let lang = crate::i18n::current();
        let key = (Arc::as_ptr(&self.entries) as usize, total, shown, self.cfg.show_hidden, lang);
        if let Some((k, s)) = &self.ux.status {
            if *k == key {
                return s.clone();
            }
        }
        let rest = total.saturating_sub(shown);
        let hidden = if self.cfg.show_hidden || rest == 0 { 0 } else { self.entries.iter().filter(|e| e.is_hidden()).count().min(rest) };
        let s = status_count(lang, shown, hidden, rest - hidden);
        self.ux.status = Some((key, s.clone()));
        s
    }

    /// Texto (y acción) cuando la vista no tiene nada que mostrar.
    pub(super) fn ux_empty_state(&mut self, ui: &mut egui::Ui) {
        #[derive(PartialEq)]
        enum Act {
            None,
            ClearFilter,
            ShowHidden,
        }
        let pal = self.pal;
        let total = self.entries.len();
        let (title, hint, act): (String, Option<&str>, Act) = if let Some(e) = &self.load_err {
            (tf("No se puede abrir la carpeta: {e}", &[e]), None, Act::None)
        } else if !self.filter.trim().is_empty() && total > 0 {
            (t("Ningún elemento coincide con el filtro").into(), None, Act::ClearFilter)
        } else {
            match &self.vmode {
                VMode::Search { query, .. } => {
                    if self.search_st.is_some() {
                        return;
                    }
                    if query.trim().is_empty() {
                        (t("Escribe para buscar…").into(), Some(t("Escribe un nombre y pulsa Intro")), Act::None)
                    } else {
                        (t("Sin resultados").into(), Some(t("Prueba con otro texto o busca en todo el equipo")), Act::None)
                    }
                }
                VMode::Trash => (t("La papelera está vacía").into(), None, Act::None),
                VMode::Recent => (t("No hay archivos recientes").into(), Some(t("Los archivos que abras aparecerán aquí")), Act::None),
                _ if total > 0 && !self.cfg.show_hidden => (
                    tn("Esta carpeta sólo tiene {} elemento oculto|Esta carpeta sólo tiene {} elementos ocultos", total, &[]),
                    None,
                    Act::ShowHidden,
                ),
                _ => (t("Carpeta vacía").into(), None, Act::None),
            }
        };
        let r = ui.min_rect();
        let c = r.center();
        let p = ui.painter().clone();
        let wrap = (r.width() - 32.0).max(80.0);
        let g = p.layout(title, FontId::proportional(15.0), pal.subtle, wrap);
        let mut y = c.y - 24.0;
        p.galley(pos2(c.x - g.size().x / 2.0, y), g.clone(), pal.subtle);
        y += g.size().y + 6.0;
        if let Some(h) = hint {
            let g2 = p.layout(h.to_string(), FontId::proportional(12.5), pal.subtle, wrap);
            p.galley(pos2(c.x - g2.size().x / 2.0, y), g2.clone(), pal.subtle);
            y += g2.size().y + 6.0;
        }
        if act != Act::None {
            let label = if act == Act::ClearFilter { t("Quitar filtro") } else { t("Mostrar ocultos") };
            let gl = p.layout_no_wrap(label.to_string(), FontId::proportional(14.0), pal.text);
            let h = theme::min_target().max(34.0);
            let bw = (gl.size().x + 36.0).max(84.0).max(h);
            let br = Rect::from_min_size(pos2(c.x - bw / 2.0, y + 4.0), vec2(bw, h));
            if ui.put(br, |ui: &mut egui::Ui| theme::dialog_button(ui, &pal, theme::Btn::Plain, label)).clicked() {
                if act == Act::ClearFilter {
                    self.filter.clear();
                } else {
                    self.cfg.show_hidden = true;
                    self.save_cfg();
                }
                self.spawn_sort();
            }
        }
    }

    /// ¿Está roto el enlace simbólico? (None mientras se comprueba en un hilo)
    pub(super) fn ux_link_broken(&mut self, e: &Entry) -> Option<bool> {
        if e.flags & fsys::F_LINK == 0 {
            return Some(false);
        }
        if self.ux.links_dir != self.cwd {
            self.ux.links.clear();
            self.ux.link_wait.clear();
            self.ux.links_dir = self.cwd.clone();
        }
        let p = self.cwd.join(&*e.name);
        if let Some(&b) = self.ux.links.get(&p) {
            return Some(b);
        }
        if self.ux.link_wait.len() < 512 && !self.ux.link_fly.contains(&p) {
            self.ux.link_wait.insert(p);
        }
        None
    }

    fn ux_flush_links(&mut self) {
        if self.ux.link_wait.is_empty() || !self.ux.link_fly.is_empty() {
            return;
        }
        let v: Vec<PathBuf> = self.ux.link_wait.drain().collect();
        self.ux.link_fly = v.iter().cloned().collect();
        self.spawn_io(
            move || v.into_iter().map(|p| (crate::vfs::fs::metadata(&p).is_err(), p)).collect::<Vec<_>>(),
            |a, r| {
                a.ux.link_fly.clear();
                for (broken, p) in r {
                    if a.ux.links.len() < 8192 {
                        a.ux.links.insert(p, broken);
                    }
                }
            },
        );
    }

    /// Emblema de enlace (flecha) o de enlace roto (aspa en rojo y el icono atenuado) sobre un icono
    /// ya pintado.
    pub(super) fn ux_link_emblem(&mut self, painter: &egui::Painter, icon: Rect, e: &Entry) {
        if e.flags & fsys::F_LINK == 0 {
            return;
        }
        let pal = self.pal;
        let broken = self.ux_link_broken(e) == Some(true);
        if broken {
            painter.rect_filled(icon.expand(1.0), CornerRadius::same(4), pal.surface.gamma_multiply(0.55));
        }
        let s = (icon.width() * 0.36).clamp(11.0, 22.0);
        let c = pos2(icon.left() + s * 0.45, icon.bottom() - s * 0.45);
        let col = if broken { pal.danger } else { pal.text };
        painter.circle_filled(c, s * 0.5, pal.panel);
        painter.circle_stroke(c, s * 0.5, Stroke::new(1.0, col));
        let k = s * 0.2;
        let st = Stroke::new((s / 10.0).clamp(1.2, 2.0), col);
        if broken {
            painter.line_segment([c + vec2(-k, -k), c + vec2(k, k)], st);
            painter.line_segment([c + vec2(-k, k), c + vec2(k, -k)], st);
        } else {
            painter.line_segment([c + vec2(-k, k), c + vec2(k, -k)], st);
            painter.line_segment([c + vec2(k, -k), c + vec2(0.0, -k)], st);
            painter.line_segment([c + vec2(k, -k), c + vec2(k, 0.0)], st);
        }
    }

    /// Volumen de una carpeta, de la caché; si no está, se pide en un hilo (None mientras tanto).
    fn ux_dev(&mut self, p: &Path) -> Option<Option<u64>> {
        if let Some(d) = self.ux.devs.get(p) {
            return Some(*d);
        }
        if self.ux.dev_wait.len() < 64 && self.ux.dev_wait.insert(p.to_path_buf()) {
            let q = p.to_path_buf();
            self.spawn_io(
                move || {
                    let d = dir_volume(&q);
                    (q, d)
                },
                |a, (q, d)| {
                    a.ux.dev_wait.remove(&q);
                    if a.ux.devs.len() > 512 {
                        a.ux.devs.clear();
                    }
                    a.ux.devs.insert(q, d);
                },
            );
        }
        None
    }

    /// Texto del fantasma de arrastre: qué pasará al soltar sobre `dst`.
    pub(super) fn ux_ghost_text(&mut self, dst: Option<PathBuf>, m: Modifiers) -> String {
        let n = tn("{} elemento|{} elementos", self.sel_count, &[]);
        let Some(dst) = dst else { return n };
        let (ck, mk) = drop_keys(m);
        let cwd = self.cwd.clone();
        let same = match (self.ux_dev(&cwd), self.ux_dev(&dst)) {
            (Some(Some(a)), Some(Some(b))) => Some(a == b),
            _ => None,
        };
        let name = dst.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| dst.display().to_string());
        let what = match drop_action(ck, mk, same) {
            DropAct::Copy => tf("Copiar a «{}»", &[&name]),
            DropAct::Move => tf("Mover a «{}»", &[&name]),
        };
        format!("{what}  ·  {n}")
    }

    /// Soltar la selección sobre `dst`: copia o mueve según las teclas y los volúmenes.
    pub(super) fn ux_drop(&mut self, dst: PathBuf, m: Modifiers) {
        let srcs = self.selected_paths();
        if srcs.is_empty() {
            return;
        }
        let (ck, mk) = drop_keys(m);
        if ck || mk {
            let mv = drop_action(ck, mk, None) == DropAct::Move;
            self.start_copy(srcs, dst, mv);
            return;
        }
        self.spawn_io(
            move || {
                let same = same_volume(&srcs, &dst);
                (srcs, dst, same)
            },
            |a, (srcs, dst, same)| {
                let mv = drop_action(false, false, same) == DropAct::Move;
                a.start_copy(srcs, dst, mv);
            },
        );
    }

    /// Renombrado fallido: el editor vuelve a abrirse con lo escrito (el error ya está en un aviso).
    pub(super) fn ux_rename_again(&mut self, idx: u32, old_full: &str, new: String) {
        if self.touch {
            if matches!(self.dlg, Dlg::None) {
                self.dlg = Dlg::Rename(old_full.to_string(), new);
            }
        } else if self.rename.is_none() && self.entries.get(idx as usize).is_some_and(|e| &*e.name == old_full) {
            self.rename = Some((idx, new, true));
        }
    }

    /// Diálogo de conflicto: uno a uno, con lo que hay y lo que llega, y «Aplicar a todos».
    pub(super) fn ux_conflict_dialog(&mut self, ctx: &egui::Context) {
        let Some(mut c) = self.conflict.take() else { return };
        let pal = self.pal;
        let total = c.names.len();
        if total == 0 {
            self.ux_conflict_finish(c);
            return;
        }
        let pos = c.ux.pos.min(total - 1);
        let name = c.names[pos].clone();
        let info = c.ux.info.get(pos).filter(|_| c.ux.info.len() == total);
        let both_dirs = info.is_some_and(|i| i.new.dir && i.old.dir);
        let (old_s, new_s) = match info {
            Some(i) => (Some(side_text(&i.old, i.old.mtime > i.new.mtime)), Some(side_text(&i.new, i.new.mtime > i.old.mtime))),
            None => (None, None),
        };
        let left = total - pos - 1;
        let dst_name = c.dst.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| c.dst.display().to_string());
        let trash = extras::trash_supported() && !crate::vfs::WEB;
        let mut items: Vec<(&str, theme::Btn)> = vec![(t("Cancelar"), theme::Btn::Plain), (t("Omitir"), theme::Btn::Plain)];
        if both_dirs {
            items.push((t("Combinar"), theme::Btn::Plain));
        }
        items.push((t("Reemplazar"), theme::Btn::Danger));
        items.push((t("Conservar ambos"), theme::Btn::Primary));
        let mut open = true;
        let all = &mut c.ux.all;
        let hit = theme::card_window_with_footer(
            ctx,
            &pal,
            t("El destino ya existe"),
            480.0,
            &mut open,
            |ui| {
                ui.label(egui::RichText::new(tf("Ya existe «{}» en «{}».", &[&name, &dst_name])).strong());
                if total > 1 {
                    ui.label(egui::RichText::new(tf("Conflicto {} de {}", &[&(pos + 1), &total])).color(pal.subtle));
                }
                if let (Some(o), Some(n)) = (&old_s, &new_s) {
                    ui.add_space(6.0);
                    egui::Grid::new("ux_conflict_grid").num_columns(2).spacing(vec2(12.0, 4.0)).show(ui, |ui| {
                        ui.label(egui::RichText::new(t("Existente")).color(pal.subtle));
                        ui.label(o);
                        ui.end_row();
                        ui.label(egui::RichText::new(t("Nuevo")).color(pal.subtle));
                        ui.label(n);
                        ui.end_row();
                    });
                }
                ui.add_space(6.0);
                let fate = if trash { t("Al reemplazar, lo que había va a la papelera.") } else { t("Al reemplazar, lo que había se elimina definitivamente.") };
                ui.label(egui::RichText::new(fate).color(pal.subtle));
                if both_dirs {
                    ui.label(egui::RichText::new(t("Combinar junta el contenido de las dos carpetas; los archivos con el mismo nombre se reemplazan.")).color(pal.subtle));
                }
                if left > 0 {
                    ui.add_space(6.0);
                    ui.checkbox(all, tn("Aplicar a todos ({} restante)|Aplicar a todos ({} restantes)", left, &[]));
                }
            },
            |ui| button_row(ui, &pal, &items),
        )
        .flatten();
        // Intro: la acción segura (si ningún control tiene el foco)
        let enter = hit.is_none() && ctx.memory(|m| m.focused()).is_none() && ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Enter));
        let label = hit.map(|i| items[i].0);
        let pick = if !open || label == Some(t("Cancelar")) {
            return;
        } else if enter || label == Some(t("Conservar ambos")) {
            Policy::Rename
        } else if label == Some(t("Omitir")) {
            Policy::Skip
        } else if label == Some(t("Combinar")) {
            Policy::Merge
        } else if label == Some(t("Reemplazar")) {
            Policy::Replace
        } else {
            self.conflict = Some(c);
            return;
        };
        c.ux.picks.truncate(pos);
        c.ux.picks.push(pick);
        if c.ux.all && pick == Policy::Merge {
            // combinar sólo tiene sentido entre carpetas: con un archivo sería reemplazar sin preguntar
            c.ux.merge_all = true;
            c.ux.all = false;
        }
        while c.ux.picks.len() < total {
            let j = c.ux.picks.len();
            let dirs = c.ux.info.get(j).filter(|_| c.ux.info.len() == total).is_some_and(|i| i.new.dir && i.old.dir);
            if c.ux.merge_all && dirs {
                c.ux.picks.push(Policy::Merge);
            } else if c.ux.all {
                c.ux.picks.push(pick);
            } else {
                break;
            }
        }
        c.ux.pos = c.ux.picks.len();
        if c.ux.pos >= total {
            self.ux_conflict_finish(c);
        } else {
            self.conflict = Some(c);
        }
    }

    /// Lanza la copia con lo decidido: una tarea por política (conservar ambos, reemplazar, combinar).
    fn ux_conflict_finish(&mut self, c: ConflictDlg) {
        let ConflictDlg { srcs, dst, mv, names, ux } = c;
        let (mut keep, mut repl, mut rnames, mut merge) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
        // `names` sigue el orden de `srcs` (sin los que no chocan): dos orígenes con el mismo nombre
        // (p. ej. desde una búsqueda) son conflictos distintos, cada uno con su respuesta
        let mut used = vec![false; names.len()];
        for s in srcs {
            let n = s.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
            let i = (s.parent() != Some(dst.as_path())).then(|| names.iter().enumerate().position(|(j, x)| !used[j] && *x == n)).flatten();
            let Some(i) = i else {
                keep.push(s);
                continue;
            };
            used[i] = true;
            let dirs = ux.info.get(i).filter(|_| ux.info.len() == names.len()).is_some_and(|c| c.new.dir && c.old.dir);
            match ux.picks.get(i).copied().unwrap_or(Policy::Skip) {
                Policy::Rename => keep.push(s),
                Policy::Skip => {}
                Policy::Merge if dirs => merge.push(s),
                Policy::Merge | Policy::Replace => {
                    repl.push(s);
                    rnames.push(n);
                }
            }
        }
        if !keep.is_empty() {
            self.start_op(Op::Copy { srcs: keep, dst: dst.clone(), mv, policy: Policy::Rename }, OpMeta::Copy { mv, replay: false });
        }
        if !repl.is_empty() {
            self.start_op(Op::Copy { srcs: repl, dst: dst.clone(), mv, policy: Policy::Replace }, OpMeta::Replace { mv, names: rnames });
        }
        if !merge.is_empty() {
            self.start_op(Op::Copy { srcs: merge, dst, mv, policy: Policy::Merge }, OpMeta::Copy { mv, replay: false });
        }
    }

    /// Confirmación del borrado definitivo: cuántos, cuáles (los primeros) y que no se puede deshacer.
    pub(super) fn ux_confirm_delete(&mut self, ctx: &egui::Context) {
        let Some(list) = self.confirm_delete.clone() else { return };
        let pal = self.pal;
        let n = list.len();
        let note = match self.ux.del_note {
            DelNote::Plain => None,
            DelNote::NoTrash => Some(t("Aquí no hay papelera: lo que elimines no se podrá recuperar.")),
            DelNote::TrashFailed => Some(t("No se pudieron mover a la papelera.")),
        };
        let filtered = self.ux.del_note == DelNote::Plain && !self.filter.trim().is_empty();
        let (mut go, mut close) = (false, false);
        theme::modal_card(
            ctx,
            &pal,
            theme::Tone::Danger,
            t("Eliminar definitivamente"),
            420.0,
            |ui| {
                if let Some(s) = note {
                    ui.label(egui::RichText::new(s).color(pal.danger));
                    ui.add_space(4.0);
                }
                ui.label(egui::RichText::new(tn("Se eliminará {} elemento de forma permanente. No se puede deshacer.|Se eliminarán {} elementos de forma permanente. No se puede deshacer.", n, &[])).color(pal.subtle));
                ui.add_space(4.0);
                for p in list.iter().take(5) {
                    let s = p.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_else(|| p.display().to_string());
                    ui.label(format!("•  {s}"));
                }
                if n > 5 {
                    ui.label(egui::RichText::new(tn("y {} más|y {} más", n - 5, &[])).color(pal.subtle));
                }
                if filtered {
                    ui.add_space(4.0);
                    ui.label(egui::RichText::new(t("Sólo cuentan los elementos seleccionados que deja ver el filtro.")).size(12.0).color(pal.subtle));
                }
            },
            |ui| {
                if theme::dialog_button(ui, &pal, theme::Btn::Danger, t("Eliminar")).clicked() {
                    go = true;
                }
                if theme::dialog_button(ui, &pal, theme::Btn::Plain, t("Cancelar")).clicked() {
                    close = true;
                }
            },
        );
        if ctx.input(|i| i.key_pressed(Key::Escape)) {
            close = true;
        }
        if go {
            self.start_op(Op::Delete(list), OpMeta::Other);
            close = true;
        }
        if close {
            self.confirm_delete = None;
            self.ux.del_note = DelNote::Plain;
        }
    }

    /// Abre el diálogo de compartir (o el que ya está compartiendo).
    #[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
    pub(super) fn ux_share_open(&mut self, exe: PathBuf) {
        if let Some(d) = self.ux.share.as_mut().filter(|d| d.running()) {
            d.open = true;
            return;
        }
        self.ux.share = Some(ShareDlg::new(self.cwd.clone(), exe));
    }

    #[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
    fn ux_share_dialog(&mut self, ctx: &egui::Context) {
        let Some(d) = self.ux.share.as_mut() else { return };
        // ¿terminó el servidor?
        if let Some(r) = d.run.as_mut().filter(|r| r.ended.is_none()) {
            if let Ok(Some(st)) = r.child.try_wait() {
                r.ended = Some(st.to_string());
            }
        }
        if !d.open {
            if d.run.as_ref().is_some_and(|r| r.ended.is_some()) || d.run.is_none() {
                self.ux.share = None;
            }
            return;
        }
        let pal = self.pal;
        let mut open = true;
        let folder = d.dir.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| d.dir.display().to_string());
        let title = t("Compartir esta carpeta");
        let mut copy: Option<String> = None;
        let act = if let Some(r) = &d.run {
            let lines: Vec<String> = r.lines.lock().map(|g| g.clone()).unwrap_or_default();
            let url = lines.iter().map(|l| l.trim()).find(|l| l.starts_with("http://") || l.starts_with("https://")).map(str::to_string);
            let ended = r.ended.clone();
            let write = d.write;
            let stop = if ended.is_some() { t("Cerrar") } else { t("Dejar de compartir") };
            theme::card_window_with_footer(
                ctx,
                &pal,
                title,
                480.0,
                &mut open,
                |ui| {
                    ui.label(egui::RichText::new(tf("Compartiendo «{}»", &[&folder])).strong());
                    ui.label(egui::RichText::new(d.dir.display().to_string()).size(12.0).color(pal.subtle));
                    ui.add_space(6.0);
                    if let Some(e) = &ended {
                        ui.label(egui::RichText::new(tf("El servidor se ha detenido: {}", &[e])).color(pal.danger));
                    } else if let Some(u) = &url {
                        ui.label(t("Abre este enlace en el otro dispositivo:"));
                        ui.horizontal_wrapped(|ui| {
                            ui.add(egui::Label::new(egui::RichText::new(u).monospace()).selectable(true));
                            if ui.button(t("Copiar enlace")).clicked() {
                                copy = Some(u.clone());
                            }
                        });
                        let qr: Vec<&str> = lines.iter().map(String::as_str).filter(|l| is_qr_line(l)).collect();
                        if !qr.is_empty() {
                            ui.add_space(6.0);
                            paint_qr(ui, &qr);
                        }
                        if write {
                            ui.label(egui::RichText::new(t("Quien tenga el enlace puede subir, borrar y mover archivos.")).color(pal.warn));
                        }
                    } else {
                        ui.label(egui::RichText::new(t("Arrancando el servidor…")).color(pal.subtle));
                    }
                    // el resto de lo que imprime el servidor (avisos, huella del certificado…)
                    let other: Vec<&String> = lines.iter().filter(|l| !l.trim().is_empty() && !is_qr_line(l) && Some(l.trim()) != url.as_deref()).collect();
                    if !other.is_empty() {
                        ui.add_space(6.0);
                        egui::CollapsingHeader::new(t("Mensajes del servidor")).id_salt("ux_share_log").show(ui, |ui| {
                            for l in other {
                                ui.label(egui::RichText::new(l).size(12.0).monospace().color(pal.subtle));
                            }
                        });
                    }
                },
                |ui| theme::DialogButtons::new(t("Ocultar"), stop).danger().show(ui, &pal),
            )
        } else {
            let problem = d.problem();
            theme::card_window_with_footer(
                ctx,
                &pal,
                title,
                480.0,
                &mut open,
                |ui| {
                    ui.label(egui::RichText::new(tf("Carpeta: {}", &[&d.dir.display()])).strong());
                    ui.add_space(6.0);
                    ui.radio_value(&mut d.lan, true, t("Dispositivos de la red local"));
                    ui.radio_value(&mut d.lan, false, t("Sólo este equipo"));
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        ui.label(t("Puerto"));
                        ui.add(egui::TextEdit::singleline(&mut d.port).hint_text(t("automático")).desired_width(90.0));
                    });
                    ui.checkbox(&mut d.write, t("Permitir subir, borrar y mover (lectura y escritura)"));
                    ui.checkbox(&mut d.tls, t("Cifrar con HTTPS (certificado autofirmado)"));
                    ui.checkbox(&mut d.auth, t("Pedir usuario y contraseña"));
                    if d.auth {
                        egui::Grid::new("ux_share_auth").num_columns(2).spacing(vec2(10.0, 6.0)).show(ui, |ui| {
                            ui.label(t("Usuario"));
                            ui.add(egui::TextEdit::singleline(&mut d.user).desired_width(200.0));
                            ui.end_row();
                            ui.label(t("Contraseña"));
                            ui.add(egui::TextEdit::singleline(&mut d.pass).password(true).desired_width(200.0));
                            ui.end_row();
                        });
                    }
                    ui.checkbox(&mut d.ttl, t("Dejar de compartir a las 2 horas"));
                    ui.add_space(6.0);
                    let mut notes = vec![t("El enlace lleva una clave aleatoria que deja de valer al dejar de compartir.")];
                    if !d.write {
                        notes.push(t("Sólo lectura: nadie puede cambiar nada."));
                    } else {
                        notes.push(t("Quien tenga el enlace puede subir, borrar y mover archivos."));
                    }
                    if d.lan && !d.tls {
                        notes.push(t("Sin HTTPS el tráfico va sin cifrar: úsalo sólo en redes de confianza."));
                    }
                    for s in notes {
                        ui.label(egui::RichText::new(s).size(12.5).color(pal.subtle));
                    }
                    if let Some(p) = problem {
                        ui.label(egui::RichText::new(p).color(pal.danger));
                    }
                },
                |ui| theme::DialogButtons::new(t("Cancelar"), t("Compartir")).primary_enabled(problem.is_none()).show(ui, &pal),
            )
        };
        if let Some(u) = copy {
            ctx.copy_text(u);
        }
        let act = act.unwrap_or(theme::DialogAction::None);
        let running = d.run.is_some();
        if !running {
            if !open || act == theme::DialogAction::Cancel {
                self.ux.share = None;
            } else if act == theme::DialogAction::Primary && d.problem().is_none() {
                if let Err(e) = d.start(ctx) {
                    self.say(format!("{}: {e}", t("No se pudo abrir")), true);
                }
            }
        } else if act == theme::DialogAction::Primary {
            // dejar de compartir (o cerrar si ya terminó): `Drop` para el servidor
            self.ux.share = None;
        } else if !open || act == theme::DialogAction::Cancel {
            d.open = false;
            if d.running() {
                self.say(t("Se sigue compartiendo: «Compartir esta carpeta…» para pararlo").into(), false);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn soltar_copia_o_mueve() {
        // sin teclas: mismo volumen mueve, otro volumen copia, sin saberlo mueve
        assert_eq!(drop_action(false, false, Some(true)), DropAct::Move);
        assert_eq!(drop_action(false, false, Some(false)), DropAct::Copy);
        assert_eq!(drop_action(false, false, None), DropAct::Move);
        // la tecla de copiar copia incluso en el mismo volumen; Mayús mueve incluso entre volúmenes
        assert_eq!(drop_action(true, false, Some(true)), DropAct::Copy);
        assert_eq!(drop_action(false, true, Some(false)), DropAct::Move);
        assert_eq!(drop_action(true, true, None), DropAct::Move);
        let ctrl = Modifiers { ctrl: true, ..Default::default() };
        let alt = Modifiers { alt: true, ..Default::default() };
        if cfg!(target_os = "macos") {
            assert_eq!(drop_keys(alt), (true, false));
        } else {
            assert_eq!(drop_keys(ctrl), (true, false));
            assert_eq!(drop_keys(alt), (false, false));
        }
        assert_eq!(drop_keys(Modifiers::SHIFT), (false, true));
    }

    #[test]
    fn mismo_volumen_en_disco() {
        let d = crate::vfs::temp_dir().join(format!("fb-ux-vol-{}", std::process::id()));
        let _ = std::fs::create_dir_all(d.join("sub"));
        let f = d.join("a.txt");
        let _ = std::fs::write(&f, b"x");
        #[cfg(any(unix, windows))]
        assert_eq!(same_volume(std::slice::from_ref(&f), &d.join("sub")), Some(true));
        assert_eq!(same_volume(&[d.join("no-existe")], &d), if cfg!(windows) { Some(true) } else { None });
        // destino que es un enlace a otro volumen (como ~/usb → /media/usb): copiar, no mover
        #[cfg(target_os = "linux")]
        {
            let l = d.join("enlace-proc");
            if std::os::unix::fs::symlink("/proc", &l).is_ok() && dir_volume(Path::new("/proc")) != dir_volume(&d) {
                assert_eq!(same_volume(std::slice::from_ref(&f), &l), Some(false));
                assert_eq!(drop_action(false, false, same_volume(std::slice::from_ref(&f), &l)), DropAct::Copy);
            }
        }
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn errores_traducidos() {
        assert_eq!(kind_text(ErrorKind::PermissionDenied), Some(t("No tienes permiso para hacerlo")));
        assert_eq!(kind_text(ErrorKind::Other), None);
        let e = std::io::Error::new(ErrorKind::StorageFull, "x");
        assert_eq!(io_text(&e), t("No queda espacio en el disco"));
        // sin código del sistema: igual que estaba
        assert_eq!(localize("No se pudo renombrar: algo raro"), ("No se pudo renombrar: algo raro".to_string(), None));
        assert_eq!(localize("roto (os error x)").1, None);
        #[cfg(unix)]
        {
            let raw = "No se pudo renombrar: Permission denied (os error 13)";
            let (s, d) = localize(raw);
            assert_eq!(s, format!("No se pudo renombrar: {}", t("No tienes permiso para hacerlo")));
            assert_eq!(d.as_deref(), Some(raw));
            // sin «: » delante: se cambia desde el principio
            let (s, _) = localize("No space left on device (os error 28) al copiar");
            assert_eq!(s, format!("{} al copiar", t("No queda espacio en el disco")));
        }
        #[cfg(windows)]
        {
            let (s, d) = localize("a.txt: Access is denied. (os error 5)");
            assert_eq!(s, format!("a.txt: {}", t("No tienes permiso para hacerlo")));
            assert!(d.is_some());
        }
        assert_eq!(ro_text(true), t("Los servidores SFTP son de sólo lectura"));
        assert_eq!(ro_text(false), t("Esta ubicación es de sólo lectura"));
    }

    #[test]
    fn recuento_de_la_barra_de_estado() {
        let es = crate::i18n::index_of("es").unwrap();
        let en = crate::i18n::index_of("en").unwrap();
        assert_eq!(status_count(es, 19, 0, 0), "19 elementos");
        assert_eq!(status_count(es, 19, 1, 0), "19 elementos (1 oculto)");
        assert_eq!(status_count(es, 1, 2, 3), "1 elemento (2 ocultos, 3 filtrados)");
        assert_eq!(status_count(es, 0, 0, 4), "0 elementos (4 filtrados)");
        assert_eq!(status_count(en, 19, 1, 1), "19 items (1 hidden, 1 filtered out)");
    }

    #[test]
    fn cola_de_avisos() {
        let t0 = Instant::now();
        let mut q = Toasts::default();
        let a = q.push("uno".into(), None, false, None, t0);
        let b = q.push("error".into(), Some("detalle".into()), true, None, t0);
        assert_ne!(a, b);
        // el mismo texto no se apila: se renueva
        assert_eq!(q.push("uno".into(), None, false, None, t0), a);
        assert_eq!(q.v.len(), 2);
        // los informativos caducan antes que los errores
        q.expire(t0 + TOAST_INFO + Duration::from_millis(1));
        assert_eq!(q.v.len(), 1);
        assert!(q.v[0].err);
        assert!(q.next_due(t0 + TOAST_INFO).is_some_and(|d| d <= TOAST_ERR - TOAST_INFO));
        q.expire(t0 + TOAST_ERR);
        assert!(q.v.is_empty());
        assert_eq!(q.next_due(t0), None);
        // como mucho TOAST_MAX: se va el informativo más antiguo, no los errores
        let e1 = q.push("e1".into(), None, true, None, t0);
        for i in 0..6 {
            q.push(format!("i{i}"), None, false, None, t0);
        }
        assert_eq!(q.v.len(), TOAST_MAX);
        assert!(q.v.iter().any(|t| t.id == e1));
        assert_eq!(q.v.last().map(|t| t.text.as_str()), Some("i5"));
        // «Deshacer» dura más que un aviso normal; cerrar lo quita
        let u = q.push("hecho".into(), None, false, Some((0, 3)), t0);
        q.expire(t0 + TOAST_INFO + Duration::from_millis(1));
        assert!(q.v.iter().any(|t| t.id == u));
        q.dismiss(u);
        assert!(!q.v.iter().any(|t| t.id == u));
    }

    #[test]
    fn conflictos_con_tamano_y_fecha() {
        let d = crate::vfs::temp_dir().join(format!("fb-ux-clash-{}", std::process::id()));
        let (a, b) = (d.join("a"), d.join("b"));
        let _ = std::fs::create_dir_all(a.join("dir"));
        let _ = std::fs::create_dir_all(b.join("dir"));
        let _ = std::fs::write(a.join("f.txt"), b"12345");
        let _ = std::fs::write(b.join("f.txt"), b"1");
        let _ = std::fs::write(a.join("solo.txt"), b"1");
        let v = clashes(&[a.join("f.txt"), a.join("dir"), a.join("solo.txt"), b.join("f.txt")], &b);
        let _ = std::fs::remove_dir_all(&d);
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].name, "f.txt");
        assert_eq!((v[0].new.size, v[0].old.size), (5, 1));
        assert!(v[1].new.dir && v[1].old.dir);
    }
}
