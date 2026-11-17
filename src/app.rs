#[allow(unused_imports)]
use crate::vfs::{KindV as _, PathV as _};
use crate::config::Config;
use crate::i18n::{t, tf};
use crate::extras;
use crate::index;
use crate::fsys::{self, Entry, Kind, LoadMsg, Op, Policy, Progress, SortKey, SortSpec};
use crate::commands::{self, Cmd};
use crate::preview::{self, Preview};
use crate::sysicons::SysIcons;
use crate::terminal::Terminal;
use crate::theme::{self, Glyph, Pal};
use crate::thumbs::{Request, Thumbs};
use crate::system;
use crate::tools;
use egui::{pos2, vec2, Align, Align2, Color32, CornerRadius, FontId, Key, Modifiers, Pos2, Rect, Sense, Stroke};
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration};
use web_time::{Instant, SystemTime};
#[path = "app_h2.rs"]
mod app_h2;
#[path = "app_menus.rs"] mod menus;
#[path = "openwith_ui.rs"]
mod openwith_ui;
#[path = "app_icons.rs"] mod app_icons;

#[path = "ux.rs"]
mod ux;
#[path = "a11y.rs"]
mod a11y;
/// Arnés de capturas automáticas (FB_SHOT, FB_DLG…): sólo con la característica `selftest`.
#[cfg(feature = "selftest")]
#[path = "selftest.rs"]
mod selftest;
#[path = "app_nav.rs"]
mod app_nav;
#[path = "app_ops.rs"]
mod app_ops;
#[path = "app_preview.rs"]
mod app_preview;
#[path = "app_keys.rs"]
mod app_keys;
#[path = "app_bars.rs"]
mod app_bars;
#[path = "app_places.rs"]
mod app_places;
#[path = "app_files.rs"]
mod app_files;
#[path = "app_tools.rs"]
mod app_tools;
#[path = "app_window.rs"]
mod app_window;
#[path = "app_dialogs.rs"]
mod app_dialogs;
use app_places::*;

struct Place {
    name: String,
    path: PathBuf,
    glyph: Glyph,
    /// Clave para el icono de sistema ("home", "documents", "drive"…).
    sys: &'static str,
}

/// Origen del listado de un panel.
#[derive(Clone, PartialEq)]
enum VMode {
    /// Carpeta normal.
    Dir,
    /// Carpeta y todas sus subcarpetas en una sola lista.
    Deep,
    /// Resultados de una búsqueda por nombre (en la carpeta o en todo el equipo).
    Search { query: String, all: bool, content: bool },
    /// Archivos abiertos hace poco (ubicación «Recientes»).
    Recent,
    /// Contenido de la papelera del sistema (ubicación «Papelera»).
    Trash,
}

/// Rutas centinela de las ubicaciones virtuales: no son carpetas reales.
const LOC_TRASH: &str = "trash:";
const LOC_RECENT: &str = "recent:";

fn virt_mode(p: &Path) -> Option<VMode> {
    if p == Path::new(LOC_TRASH) {
        Some(VMode::Trash)
    } else if p == Path::new(LOC_RECENT) {
        Some(VMode::Recent)
    } else {
        None
    }
}

/// Qué se va a eliminar definitivamente de la papelera.
enum PurgeReq {
    All,
    Rows(Vec<extras::TrashRow>),
}

#[derive(Clone)]
struct TabState {
    cwd: PathBuf,
    back: Vec<PathBuf>,
    fwd: Vec<PathBuf>,
    filter: String,
    sel: Vec<String>,
    /// Segundo panel de la pestaña (vista dividida) y panel con el foco.
    split: Option<Box<TabState>>,
    side: u8,
    vmode: VMode,
    /// desplazamiento de la lista, texto del cuadro de búsqueda y resultados de una búsqueda ya
    /// terminada (al volver a la pestaña no se repite)
    scroll: f32,
    sbuf: String,
    results: Option<Arc<Vec<Entry>>>,
    /// Vista de búsqueda de cada paso de `back`/`fwd` (alineadas con ellas): al volver a unos
    /// resultados se recuperan la consulta y la lista en vez de la carpeta normal.
    back_snap: Vec<HistSnap>,
    fwd_snap: Vec<HistSnap>,
}

/// Lo que se recuerda de un paso del historial además de la ruta (sólo si era una búsqueda).
type HistSnap = Option<Box<TabState>>;

impl TabState {
    fn new(cwd: PathBuf) -> TabState {
        TabState {
            cwd,
            back: vec![],
            fwd: vec![],
            filter: String::new(),
            sel: vec![],
            split: None,
            side: 0,
            vmode: VMode::Dir,
            scroll: 0.0,
            sbuf: String::new(),
            results: None,
            back_snap: vec![],
            fwd_snap: vec![],
        }
    }
}

struct PropsDlg {
    paths: Vec<PathBuf>,
    /// Cambios aplicándose en segundo plano (permisos recursivos, red…): avance y cancelación.
    job: Option<Arc<tools::Work>>,
    stats: Arc<tools::Stats>,
    title: String,
    location: String,
    kind: String,
    mtime: i64,
    mode: String,
    // edición
    name: String,
    name_orig: String,
    time: String,
    time_orig: String,
    perm: u32,
    perm_orig: u32,
    recursive: bool,
    has_dir: bool,
    err: String,
    ok: bool,
}

/// Consume la tecla sólo si los modificadores coinciden exactamente (Ctrl+Mayús+F no dispara Ctrl+F,
/// AltGr = Ctrl+Alt no dispara atajos de una sola tecla). En teclas de símbolo se tolera Mayús, porque
/// según la distribución del teclado hace falta para escribirlas (p. ej. «+» o «?» en teclado inglés).
fn consume_exact(i: &mut egui::InputState, m: Modifiers, key: Key) -> bool {
    let named = matches!(key, Key::Plus | Key::Minus | Key::Equals | Key::Questionmark | Key::Colon | Key::Slash | Key::Backslash | Key::Comma | Key::Period | Key::Semicolon | Key::Quote | Key::OpenBracket | Key::CloseBracket | Key::Backtick | Key::Pipe | Key::Exclamationmark | Key::OpenCurlyBracket | Key::CloseCurlyBracket);
    let mut hit = false;
    i.events.retain(|e| {
        if let egui::Event::Key { key: k, modifiers: em, pressed: true, .. } = e {
            if *k == key && (em.matches_exact(m) || (named && !m.shift && em.matches_exact(m | Modifiers::SHIFT))) {
                hit = true;
                return false;
            }
        }
        true
    });
    hit
}

impl Drop for PropsDlg {
    fn drop(&mut self) {
        self.stats.cancel.store(true, Ordering::Relaxed);
    }
}

struct BatchDlg {
    names: Vec<String>,
    find: String,
    repl: String,
    prefix: String,
    suffix: String,
    number: bool,
    start: u32,
    case: u8,
    /// ¿se muestra la lista de nombres nuevos en la ventana?
    preview: bool,
    /// Plantilla del nombre (vacía = se parte del nombre original): {nombre} {n} {n:3} {fecha} {padre}.
    template: String,
    regex: bool,
    mtimes: Vec<i64>,
    parent: String,
    /// Carpeta donde se abrió: el diálogo no se mueve con la navegación.
    dir: PathBuf,
    /// ¿Existe ya ese nombre en la carpeta? (una consulta por nombre, no por fotograma)
    exists: HashMap<String, bool>,
    /// Expresión compilada para el texto de búsqueda actual.
    re_cache: Option<(String, Option<regex::Regex>)>,
}

/// Acción reversible registrada para deshacer/rehacer.
#[derive(Clone)]
enum Undo {
    Rename { from: PathBuf, to: PathBuf },
    /// Copia con «Reemplazar»/«Combinar»: (origen, copia) y los destinos reemplazados que están en la papelera.
    CopiedRepl(Vec<(PathBuf, PathBuf)>, Vec<PathBuf>),
    /// (origen, nuevo) de cada elemento movido o renombrado en lote.
    Moved(Vec<(PathBuf, PathBuf)>),
    /// Movimiento con «Reemplazar»: además, lo que había en esos destinos (ahora en la papelera).
    MovedRepl(Vec<(PathBuf, PathBuf)>, Vec<PathBuf>),
    /// (origen, copia)
    Copied(Vec<(PathBuf, PathBuf)>),
    /// Nuevo archivo/carpeta vacío: (ruta, es_carpeta)
    Created(Vec<(PathBuf, bool)>),
    Trashed(Vec<PathBuf>),
    /// Zip/extracción: sólo se puede deshacer.
    Generated(Vec<PathBuf>),
}

/// Qué hay que registrar cuando termina la operación en curso.
enum OpMeta {
    Copy { mv: bool, replay: bool },
    /// Copia/movimiento con «Reemplazar»: `names` son los nombres que ya existían en el destino.
    Replace { mv: bool, names: Vec<String> },
    /// Deshacer/rehacer un movimiento entre sistemas de archivos: al terminar, `done` pasa a la otra
    /// pila (rehacer si `to_redo`) y lo de `restore` vuelve de la papelera.
    MoveBack { done: Undo, to_redo: bool, restore: Vec<PathBuf> },
    Trash { replay: bool },
    Generated,
    /// Extracción a la caché para navegar dentro de un comprimido.
    OpenArchive { archive: PathBuf, root: PathBuf },
    /// Deshacer asíncrono: al terminar, el elemento pasa a la pila de rehacer.
    Replay(Undo),
    /// Descarga de un servidor SFTP: al terminar, lo que se iba a hacer con los archivos.
    Fetch(app_places::FetchThen),
    Other,
}

struct ConflictDlg {
    srcs: Vec<PathBuf>,
    dst: PathBuf,
    mv: bool,
    names: Vec<String>,
    /// detalle de cada conflicto y lo decidido hasta ahora
    ux: ux::ConflictUx,
}

/// Diálogo «Abrir con» (en `openwith_ui.rs`).
type OpenWithDlg = openwith_ui::OwDlg;

struct HashDlg {
    path: PathBuf,
    job: Arc<extras::HashJob>,
    cmp: String,
}

impl Drop for HashDlg {
    fn drop(&mut self) {
        self.job.cancel.store(true, Ordering::Relaxed);
    }
}

struct DupsDlg {
    root: PathBuf,
    job: Arc<extras::DupJob>,
}

impl Drop for DupsDlg {
    fn drop(&mut self) {
        self.job.cancel.store(true, Ordering::Relaxed);
    }
}

enum Dlg {
    None,
    OpenWith(OpenWithDlg),
    Hash(HashDlg),
    Compress(CompressDlg),
    ArchPass(ArchPass),
    Dups(DupsDlg),
    Props(PropsDlg),
    Batch(BatchDlg),
    NewFile(String),
    /// Renombrar con diálogo (táctil): nombre actual completo y texto editado.
    Rename(String, String),
    NetLogin(NetLogin),
    /// Recursos compartidos de un servidor: se monta sólo el que se elija.
    NetShares(NetShares),
    /// Favoritos e iconos de carpeta: «Cambiar icono…» y «Cambiar nombre…».
    Fav(app_icons::JDlg),
    /// Lista completa de errores de una operación con varios fallos.
    Errors(Vec<String>),
    /// Programa ejecutable (ELF o script con shebang) al que se ha hecho doble clic.
    Options,
    About,
    Keys,
    /// Tarjeta de bienvenida del primer arranque (idioma, tema e índice).
    Welcome,
    /// Confirmación antes de mover a la papelera (opción «Confirmar al mover a la papelera»).
    TrashAsk(Vec<PathBuf>),
}

/// Contraseña de un comprimido protegido; al aceptar se repite la extracción.
struct ArchPass {
    src: PathBuf,
    dst: PathBuf,
    meta: OpMeta,
    pass: String,
    show: bool,
    wrong: bool,
    focus: bool,
}

/// Diálogo «Comprimir…».
struct CompressDlg {
    srcs: Vec<PathBuf>,
    /// Nombre sin extensión (la pone el formato).
    name: String,
    fmt: tools::ArchFmt,
    level: u32,
    protect: bool,
    pw: String,
    pw2: String,
    show: bool,
    focus: bool,
}

/// Datos para conectar con un recurso de red (la contraseña solo vive en memoria).
#[derive(Clone)]
struct NetLogin {
    uri: String,
    user: String,
    domain: String,
    pass: String,
    anon: bool,
}

/// Recursos de un servidor (sin montar) y las credenciales con las que se listaron.
struct NetShares {
    base: String,
    shares: Vec<String>,
    login: NetLogin,
}

impl Drop for NetLogin {
    fn drop(&mut self) {
        self.pass.clear();
    }
}

#[derive(Default)]
struct PaletteState {
    query: String,
    sel: usize,
    /// fotograma en que se abrió: el clic que la abrió (desde un menú) no la cierra
    born: u64,
}

struct Band {
    start: egui::Vec2,
    base: Vec<u64>,
}

#[inline]
fn mix(h: u64, v: u64) -> u64 {
    (h.rotate_left(5) ^ v).wrapping_mul(0x517c_c1b7_2722_0a95)
}

fn hash_str(mut h: u64, s: &str) -> u64 {
    for c in s.as_bytes().chunks(8) {
        let mut b = [0u8; 8];
        b[..c.len()].copy_from_slice(c);
        h = mix(h, u64::from_le_bytes(b));
    }
    mix(h, s.len() as u64)
}

/// Índices con el bit puesto en un conjunto de bits.
fn for_each_bit(bits: &[u64], mut f: impl FnMut(u32)) {
    for (wi, &w) in bits.iter().enumerate() {
        let mut w = w;
        while w != 0 {
            let b = w.trailing_zeros();
            f((wi as u32) * 64 + b);
            w &= w - 1;
        }
    }
}

/// Apaga los bits cuyos índices no cumplen `keep`.
fn prune_bits(bits: &mut [u64], keep: impl Fn(u32) -> bool) {
    for (wi, w) in bits.iter_mut().enumerate() {
        let mut rest = *w;
        while rest != 0 {
            let b = rest.trailing_zeros();
            if !keep((wi as u32) * 64 + b) {
                *w &= !(1u64 << b);
            }
            rest &= rest - 1;
        }
    }
}

/// Primer nombre libre para `p` en su carpeta («x», «x (2)»…).
fn free_path(p: &Path) -> PathBuf {
    match (p.parent(), p.file_name()) {
        (Some(d), Some(n)) => fsys::unique_path(d, &n.to_string_lossy()),
        _ => p.to_path_buf(),
    }
}

/// Entrada de deshacer de un movimiento, con lo que reemplazó (si algo) entre sus destinos.
fn moved_undo(v: Vec<(PathBuf, PathBuf)>, replaced: &[PathBuf]) -> Undo {
    let rep: Vec<PathBuf> = replaced.iter().filter(|r| v.iter().any(|(_, n)| n == *r)).cloned().collect();
    if rep.is_empty() { Undo::Moved(v) } else { Undo::MovedRepl(v, rep) }
}

/// Entrada de deshacer de una copia o un movimiento; `trashed` son los destinos que se reemplazaron y
/// están en la papelera (lo borrado definitivamente no se puede recuperar y no se anota).
fn copy_undo(mv: bool, v: Vec<(PathBuf, PathBuf)>, trashed: &[PathBuf]) -> Undo {
    if mv {
        return moved_undo(v, trashed);
    }
    let rep: Vec<PathBuf> = trashed.iter().filter(|r| v.iter().any(|(_, n)| n == *r)).cloned().collect();
    if rep.is_empty() { Undo::Copied(v) } else { Undo::CopiedRepl(v, rep) }
}

/// Deshacer «copiar reemplazando», antes de mandar las copias a la papelera: cada copia que ocupa el
/// sitio de algo que está en la papelera se aparta a un nombre libre («x (2)»), lo anterior vuelve con
/// `restore` y las copias apartadas son las que luego van a la papelera (así no se confunden con lo que
/// hay que recuperar, que tiene la misma ruta). Devuelve los pares deshechos, lo que hay que tirar y
/// un error si algo no se pudo recuperar (esa copia vuelve a su sitio y no se toca).
#[allow(clippy::type_complexity)]
fn undo_copy_repl(pairs: &[(PathBuf, PathBuf)], rep: &[PathBuf], restore: impl FnOnce(&[PathBuf]) -> Result<(), String>) -> Result<(Vec<(PathBuf, PathBuf)>, Vec<PathBuf>, Option<String>), String> {
    let (mut done, mut trash, mut aside) = (Vec::new(), Vec::new(), Vec::new());
    for pr in pairs {
        if !rep.contains(&pr.1) {
            done.push(pr.clone());
            trash.push(pr.1.clone());
            continue;
        }
        let to = free_path(&pr.1);
        if let Err(e) = crate::vfs::fs::rename(&pr.1, &to) {
            for (_, n, to) in &aside {
                let _ = crate::vfs::fs::rename(to, n);
            }
            return Err(format!("{}: {e}", pr.1.display()));
        }
        aside.push((pr.0.clone(), pr.1.clone(), to));
    }
    let back: Vec<PathBuf> = aside.iter().map(|(_, n, _)| n.clone()).collect();
    if back.is_empty() {
        return Ok((done, trash, None));
    }
    let res = restore(&back);
    let mut missing = Vec::new();
    for (src, n, to) in aside {
        if crate::vfs::fs::symlink_metadata(&n).is_ok() {
            done.push((src, n));
            trash.push(to);
        } else {
            // no ha vuelto de la papelera: la copia recupera su sitio
            let _ = crate::vfs::fs::rename(&to, &n);
            missing.push(n.display().to_string());
        }
    }
    if done.is_empty() {
        return Err(res.err().unwrap_or_else(|| missing.join(", ")));
    }
    let err = (!missing.is_empty()).then(|| res.err().unwrap_or_else(|| tf("{}: no está en la papelera", &[&missing.join(", ")])));
    Ok((done, trash, err))
}

/// ¿El error de `rename` es por cruzar de sistema de archivos?
fn is_xdev(e: &std::io::Error) -> bool {
    e.kind() == std::io::ErrorKind::CrossesDevices || e.raw_os_error() == Some(if cfg!(windows) { 17 } else { 18 })
}

/// Renombra sin pisar nada; el error dice si es por cruzar de sistema de archivos.
fn rename_undo(a: &Path, b: &Path) -> Result<(), (bool, String)> {
    // sólo cambian las mayúsculas del mismo elemento (FAT/SMB en Linux)
    if fsys::case_alias(a, b) {
        return fsys::rename_case(a, b).map_err(|e| (false, format!("{}: {e}", a.display())));
    }
    let same = matches!((crate::vfs::fs::canonicalize(a), crate::vfs::fs::canonicalize(b)), (Ok(x), Ok(y)) if x == y);
    if !same && crate::vfs::fs::symlink_metadata(b).is_ok() {
        return Err((false, tf("{}: ya existe", &[&b.display()])));
    }
    crate::vfs::fs::rename(a, b).map_err(|e| (is_xdev(&e), format!("{}: {e}", a.display())))
}

/// Renombra todos los pares pasando por nombres intermedios (sirve para intercambios a↔b). Si algo
/// falla deshace lo que pueda y devuelve los índices que sí quedaron hechos y el error.
fn rename_two_phase(steps: &[(PathBuf, PathBuf)]) -> Result<(), (Vec<usize>, String)> {
    let tmp = |k: usize, a: &Path| a.with_file_name(format!("{}fbu-{}-{k}", crate::vfs::TMP_LEAD, crate::vfs::pid()));
    let mut errs: Vec<String> = Vec::new();
    for (k, (a, _)) in steps.iter().enumerate() {
        let t = tmp(k, a);
        let r = if crate::vfs::fs::symlink_metadata(&t).is_ok() {
            Err(tf("{}: ya existe", &[&t.display()]))
        } else {
            crate::vfs::fs::rename(a, &t).map_err(|e| format!("{}: {e}", a.display()))
        };
        if let Err(e) = r {
            errs.push(e);
            for (j, (aj, _)) in steps.iter().enumerate().take(k) {
                if let Err(e2) = crate::vfs::fs::rename(tmp(j, aj), aj) {
                    errs.push(format!("{}: {e2}", aj.display()));
                }
            }
            return Err((Vec::new(), errs.join("; ")));
        }
    }
    for (k, (a, b)) in steps.iter().enumerate() {
        let t = tmp(k, a);
        let r = if crate::vfs::fs::symlink_metadata(b).is_ok() {
            Err(tf("{}: ya existe", &[&b.display()]))
        } else {
            crate::vfs::fs::rename(&t, b).map_err(|e| format!("{}: {e}", b.display()))
        };
        if let Err(e) = r {
            errs.push(e);
            // lo que no llegó a su nombre vuelve al de antes si está libre; si no, se dice dónde quedó
            for (j, (aj, _)) in steps.iter().enumerate().skip(k) {
                let tj = tmp(j, aj);
                let back = if crate::vfs::fs::symlink_metadata(aj).is_ok() { Err(std::io::Error::from(std::io::ErrorKind::AlreadyExists)) } else { crate::vfs::fs::rename(&tj, aj) };
                if let Err(e2) = back {
                    errs.push(format!("{}: {e2}", tj.display()));
                }
            }
            return Err(((0..k).collect(), errs.join("; ")));
        }
    }
    Ok(())
}

/// Renombrado en lote dentro de `dir` (pares viejo→nuevo) en dos fases, para permitir intercambios y
/// cadenas. Si algo falla vuelve cada uno a su nombre original; el error incluye los del retroceso.
fn batch_rename(dir: &Path, pairs: &[(String, String)]) -> Result<(), String> {
    let pid = crate::vfs::pid();
    let tmpn = |i: usize| dir.join(format!("{}fbtmp-{pid}-{i}", crate::vfs::TMP_LEAD));
    let mut errs: Vec<String> = Vec::new();
    // fase 1: nombres temporales
    let mut staged = 0;
    for (i, (o, _)) in pairs.iter().enumerate() {
        // rename sobrescribe: un temporal viejo con el mismo nombre se perdería sin aviso
        let tmp = tmpn(i);
        let r = if crate::vfs::fs::symlink_metadata(&tmp).is_ok() {
            Err(std::io::Error::new(std::io::ErrorKind::AlreadyExists, t("ya existe")))
        } else {
            crate::vfs::fs::rename(dir.join(o), &tmp)
        };
        if let Err(e) = r {
            errs.push(format!("{o}: {e}"));
            break;
        }
        staged = i + 1;
    }
    // fase 2: nombres finales, sin sobrescribir nada que haya aparecido entre la vista previa y ahora
    let mut done = Vec::new();
    if errs.is_empty() {
        for i in 0..staged {
            let dst = dir.join(&pairs[i].1);
            let r = if crate::vfs::fs::symlink_metadata(&dst).is_ok() {
                Err(std::io::Error::new(std::io::ErrorKind::AlreadyExists, t("ya existe")))
            } else {
                crate::vfs::fs::rename(tmpn(i), &dst)
            };
            match r {
                Ok(()) => done.push(i),
                Err(e) => {
                    errs.push(format!("{}: {e}", pairs[i].1));
                    break;
                }
            }
        }
    }
    if errs.is_empty() {
        return Ok(());
    }
    for &i in &done {
        if let Err(e) = crate::vfs::fs::rename(dir.join(&pairs[i].1), tmpn(i)) {
            errs.push(format!("{}: {e}", pairs[i].1));
        }
    }
    for i in 0..staged {
        let back = dir.join(&pairs[i].0);
        let tmp = tmpn(i);
        if crate::vfs::fs::symlink_metadata(&tmp).is_err() {
            continue; // no se pudo devolver a temporal: sigue con el nombre nuevo (ya dicho arriba)
        }
        // si el original ya está ocupado no se sobrescribe: se deja con un nombre reconocible
        if crate::vfs::fs::symlink_metadata(&back).is_ok() || crate::vfs::fs::rename(&tmp, &back).is_err() {
            let alt = fsys::unique_path(dir, &format!("{} (recuperado)", pairs[i].0));
            if let Err(e) = crate::vfs::fs::rename(&tmp, &alt) {
                errs.push(format!("{}: {e}", tmp.display()));
            }
        }
    }
    Err(errs.join("; "))
}

/// Pestañas que se guardan al salir (sin las virtuales): rutas, paneles divididos con el índice de su
/// pestaña dentro de la lista guardada, y la pestaña activa en esa misma lista.
fn saved_tabs(tabs: &[(PathBuf, Option<PathBuf>)], cur: usize) -> (Vec<PathBuf>, Vec<(usize, PathBuf)>, usize) {
    let (mut out, mut split, mut cur_out) = (Vec::new(), Vec::new(), 0);
    for (i, (p, s)) in tabs.iter().enumerate() {
        if virt_mode(p).is_some() {
            continue;
        }
        if i <= cur {
            cur_out = out.len();
        }
        if let Some(s) = s.as_ref().filter(|s| virt_mode(s).is_none()) {
            split.push((out.len(), s.clone()));
        }
        out.push(p.clone());
    }
    (out, split, cur_out)
}

/// Familia del shell del terminal, que decide cómo se escribe una ruta en él.
#[derive(Clone, Copy, PartialEq, Debug)]
enum ShellKind {
    Posix,
    Power,
    Cmd,
}

fn shell_kind(shell: &str) -> ShellKind {
    let s = shell.trim().trim_matches('"');
    let base = s.rsplit(['/', '\\']).next().unwrap_or(s).to_lowercase();
    match base.strip_suffix(".exe").unwrap_or(&base) {
        "powershell" | "pwsh" => ShellKind::Power,
        "cmd" => ShellKind::Cmd,
        _ => ShellKind::Posix,
    }
}

/// `cd` a la ruta (con su Intro) para teclearlo en ese shell; vacío si no se puede escribir sin riesgo.
fn quote_cd(p: &str, kind: ShellKind) -> String {
    if p.chars().any(|c| c.is_control()) {
        return String::new();
    }
    match kind {
        ShellKind::Posix => format!("cd '{}'\r", p.replace('\'', "'\\''")),
        ShellKind::Power => {
            // PowerShell toma también las comillas simples tipográficas como comillas: se doblan todas
            let mut q = String::with_capacity(p.len() + 2);
            for c in p.chars() {
                if matches!(c, '\'' | '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}') {
                    q.push(c);
                }
                q.push(c);
            }
            format!("Set-Location -LiteralPath '{q}'\r")
        }
        // cmd expande %…% y !…! incluso entre comillas, y no hay forma de escapar «"»
        ShellKind::Cmd if p.contains(['%', '"', '^', '!']) => String::new(),
        ShellKind::Cmd => format!("cd /d \"{p}\"\r"),
    }
}

/// Windows: extensiones que ejecutan código al abrirlas (se pregunta antes).
#[cfg_attr(not(windows), allow(dead_code))]
fn win_program_ext(ext: &str) -> bool {
    matches!(
        ext.to_ascii_lowercase().as_str(),
        "exe" | "com" | "bat" | "cmd" | "scr" | "pif" | "msi" | "msp" | "mst" | "vbs" | "vbe" | "js" | "jse" | "wsf" | "wsh" | "ws" | "hta"
            | "cpl" | "ps1" | "psm1" | "reg" | "jar" | "lnk" | "msc" | "application" | "appref-ms" | "gadget" | "scf" | "inf" | "settingcontent-ms"
    )
}

/// cmd trata estos caracteres como órdenes o separadores aunque vayan entre comillas en `start … /K`.
#[cfg_attr(not(windows), allow(dead_code))]
const CMD_BAD: [char; 13] = ['&', '|', '^', '%', '<', '>', '(', ')', '!', '"', ',', ';', '='];

/// Guardado aplazado de la configuración: cada cambio marca y el guardado espera un rato sin cambios.
#[derive(Default)]
struct SaveDebounce {
    since: Option<f64>,
}

impl SaveDebounce {
    const QUIET: f64 = 0.5;
    fn mark(&mut self, now: f64) {
        self.since = Some(now);
    }
    /// ¿Toca guardar ya? Si sí, queda limpio.
    fn due(&mut self, now: f64) -> bool {
        match self.since {
            Some(t) if now - t >= Self::QUIET => {
                self.since = None;
                true
            }
            _ => false,
        }
    }
    /// Guardado inmediato (al salir): ¿había algo pendiente?
    fn flush(&mut self) -> bool {
        self.since.take().is_some()
    }
    fn pending(&self) -> bool {
        self.since.is_some()
    }
}

/// Estado de listado de un panel. El panel activo vive en los campos de `App`;
/// el inactivo (vista dividida) se guarda aquí y se intercambia al dibujarlo.
struct PaneState {
    cwd: PathBuf,
    back: Vec<PathBuf>,
    fwd: Vec<PathBuf>,
    back_snap: Vec<HistSnap>,
    fwd_snap: Vec<HistSnap>,
    generation: u64,
    gen_shared: Arc<AtomicU64>,
    load_tx: Sender<LoadMsg>,
    load_rx: Receiver<LoadMsg>,
    loading: bool,
    load_err: Option<String>,
    dir_hash: u64,
    dir_mtime: Option<SystemTime>,
    last_check: Instant,
    entries: Arc<Vec<Entry>>,
    order: Vec<u32>,
    sort_seq: u64,
    sort_tx: Sender<(u64, Vec<u32>)>,
    sort_rx: Receiver<(u64, Vec<u32>)>,
    filter: String,
    filter_focus: bool,
    sel: Vec<u64>,
    sel_count: usize,
    sel_size: u64,
    anchor: Option<u32>,
    cursor: Option<u32>,
    band: Option<Band>,
    drag_items: bool,
    scroll_to: Option<u32>,
    cols: usize,
    page_rows: usize,
    rename: Option<(u32, String, bool)>,
    pending_select: Vec<String>,
    pending_rename: bool,
    typeahead: (String, Instant),
    vmode: VMode,
    search_st: Option<Arc<tools::Search>>,
    search_taken: usize,
    sbuf: String,
    t_load: Instant,
    #[cfg(feature = "selftest")]
    traced_first: bool,
    hover_name: Option<String>,
    sort_pending: bool,
    last_tap: Option<(u32, f64)>,
    kin_off: f32,
    kin_vel: f32,
    kin_k: f32,
    kin_wheel_until: f64,
    kin_hist: Vec<(f64, f32)>,
    /// Desplazamiento vertical de la lista, para conservarlo al recargar.
    keep_scroll: Option<f32>,
    keep_cursor: Option<String>,
}

impl PaneState {
    fn new(cwd: PathBuf) -> PaneState {
        let (load_tx, load_rx) = channel();
        let (sort_tx, sort_rx) = channel();
        PaneState {
            cwd,
            back: vec![],
            fwd: vec![],
            back_snap: vec![],
            fwd_snap: vec![],
            generation: 0,
            gen_shared: Arc::new(AtomicU64::new(0)),
            load_tx,
            load_rx,
            loading: false,
            load_err: None,
            dir_hash: 0,
            dir_mtime: None,
            last_check: Instant::now(),
            entries: Arc::new(Vec::new()),
            order: Vec::new(),
            sort_seq: 0,
            sort_tx,
            sort_rx,
            filter: String::new(),
            filter_focus: false,
            sel: Vec::new(),
            sel_count: 0,
            sel_size: 0,
            anchor: None,
            cursor: None,
            band: None,
            drag_items: false,
            scroll_to: None,
            cols: 1,
            page_rows: 10,
            rename: None,
            pending_select: Vec::new(),
            pending_rename: false,
            typeahead: (String::new(), Instant::now()),
            vmode: VMode::Dir,
            search_st: None,
            search_taken: 0,
            sbuf: String::new(),
            t_load: Instant::now(),
            #[cfg(feature = "selftest")]
            traced_first: false,
            hover_name: None,
            sort_pending: false,
            last_tap: None,
            kin_off: 0.0,
            kin_vel: 0.0,
            kin_k: 1.4,
            kin_wheel_until: 0.0,
            kin_hist: Vec::new(),
            keep_scroll: None,
            keep_cursor: None,
        }
    }
}

pub struct App {
    was_focused: bool,
    cfg: Config,
    pal: Pal,
    ctx: egui::Context,

    cwd: PathBuf,
    back: Vec<PathBuf>,
    fwd: Vec<PathBuf>,
    back_snap: Vec<HistSnap>,
    fwd_snap: Vec<HistSnap>,

    generation: u64,
    gen_shared: Arc<AtomicU64>,
    load_tx: Sender<LoadMsg>,
    load_rx: Receiver<LoadMsg>,
    loading: bool,
    sort_pending: bool,
    /// Entrada táctil: arrastrar desplaza, un toque abre y la pulsación larga abre el menú.
    touch: bool,
    /// Posición (en `order`) y elemento bajo el dedo al empezar el toque.
    touch_press: Option<(usize, u32)>,
    /// Interfaz de móvil (Android con pantalla estrecha): sin barra lateral, terminal ni pestañas.
    phone: bool,
    /// Posición del último toque (al soltar el dedo egui ya no tiene posición de puntero).
    addr_press: Option<Pos2>,
    /// Último toque sobre un elemento (índice, instante) para detectar el doble toque.
    last_tap: Option<(u32, f64)>,
    /// Punto donde empezó el último toque (para saber si cayó en la zona de selección).
    touch_pt: Option<Pos2>,
    /// Android: acceso a todos los archivos concedido (se vuelve a comprobar cada segundo).
    perm_ok: bool,
    /// El toque actual no debe activar ningún elemento (frenaba el deslizamiento, cerraba un menú…).
    tap_block: bool,
    preview_was: bool,
    /// Datos extra de las columnas opcionales de la lista, por ruta; se vacía al recargar.
    extra_cache: HashMap<PathBuf, fsys::ExtraMeta>,
    /// Metadatos de columnas que se calculan en un hilo (red, NSS…): llegan por aquí.
    extra_tx: Sender<(PathBuf, fsys::ExtraMeta)>,
    extra_rx: Receiver<(PathBuf, fsys::ExtraMeta)>,
    extra_pending: std::collections::HashSet<PathBuf>,
    /// Comprimidos abiertos como carpeta: (carpeta de caché, archivo original).
    arch: Vec<(PathBuf, PathBuf)>,
    /// Comprimidos (carpeta de caché) de los que se ha abierto algo con otra aplicación.
    arch_ext: HashSet<PathBuf>,
    /// El usuario cerró el panel del portapapeles (reaparece al copiar de nuevo).
    clip_hidden: bool,
    /// Listados de las columnas estáticas de la vista de columnas: ruta -> (instante, filas).
    col_cache: HashMap<PathBuf, (Instant, Arc<Vec<(String, bool)>>)>,
    /// Carpetas cuyo listado de columnas se está leyendo en un hilo.
    col_busy: HashSet<PathBuf>,
    /// Se está pintando la columna activa de la vista de columnas (sin cabecera ni columnas extra).
    col_active: bool,
    /// Pestaña abierta en Opciones: 0 = general, 1 = columnas, 2 = temas.
    opts_tab: u8,
    /// Tema propio abierto en el editor de la pestaña Temas.
    edit_theme: String,
    perm_t: f64,
    /// Márgenes seguros (izq, arriba, der, abajo) en puntos: cámara, barras del sistema.
    insets: [f32; 4],
    insets_t: f64,
    /// Modo de selección múltiple táctil (pulsación larga); tocar alterna elementos.
    sel_mode: bool,
    /// Inercia de desplazamiento: offset actual, velocidad (px/s) y muestras recientes (t, dy).
    kin_off: f32,
    kin_vel: f32,
    /// fricción de la inercia actual (1/s) y hasta cuándo se atiende la rueda del ratón
    kin_k: f32,
    kin_wheel_until: f64,
    kin_hist: Vec<(f64, f32)>,
    /// Al recargar sin cambiar de carpeta: desplazamiento y elemento del cursor (por nombre) a conservar.
    keep_scroll: Option<f32>,
    keep_cursor: Option<String>,
    /// Tema del sistema detectado fuera de winit: 0 = desconocido, 1 = claro, 2 = oscuro.
    sys_theme: Arc<std::sync::atomic::AtomicU8>,
    load_err: Option<String>,
    dir_hash: u64,
    dir_mtime: Option<SystemTime>,
    last_check: Instant,

    entries: Arc<Vec<Entry>>,
    order: Vec<u32>,
    sort_seq: u64,
    sort_tx: Sender<(u64, Vec<u32>)>,
    sort_rx: Receiver<(u64, Vec<u32>)>,
    filter: String,
    filter_focus: bool,

    sel: Vec<u64>,
    sel_count: usize,
    sel_size: u64,
    anchor: Option<u32>,
    cursor: Option<u32>,
    band: Option<Band>,
    drag_items: bool,
    /// Arrastre hacia otras aplicaciones (origen de DnD del sistema).
    dragout: Option<crate::dragout::DragOut>,
    dragout_tried: bool,
    /// Tras iniciar el arrastre del sistema, hay que soltar el botón en egui (el compositor se queda el evento).
    release_inject: bool,
    last_ptr: Option<Pos2>,
    ext_dropped: Vec<PathBuf>,
    /// arrastre/redimensionado de la ventana en curso (Windows): respaldo si el del sistema no responde
    win_drag: Option<WinDrag>,
    /// Rutas enviadas por otras invocaciones del programa.
    inbox: system::Inbox,
    /// Aplicaciones del submenú «Abrir con» para el último archivo consultado.
    ow_cache: Option<(PathBuf, Vec<extras::App>)>,
    scroll_to: Option<u32>,
    cols: usize,
    page_rows: usize,

    thumbs: Thumbs,
    /// Miniaturas pedidas por los paneles en este fotograma (distancia al centro, petición).
    thumb_want: Vec<(usize, Request)>,
    thumb_any: bool,
    places: Vec<Place>,
    /// favoritos de serie e iconos de carpeta (propios y del sistema)
    j: app_icons::JState,
    drives: Vec<Place>,
    network: Vec<NetPlace>,
    net_check: Instant,
    net_nav: Arc<std::sync::Mutex<Option<PathBuf>>>,
    net_err: Arc<std::sync::Mutex<Option<String>>>,
    net_busy: Arc<AtomicBool>,
    /// Lista de red calculada en segundo plano (consulta procesos y puede tardar).
    net_list: Arc<std::sync::Mutex<Option<Vec<NetPlace>>>>,
    /// Unidades y raíces del índice, calculadas en un hilo (en Windows `exists()` de cada letra puede colgarse).
    drives_slot: Arc<std::sync::Mutex<Option<(Vec<Place>, Vec<PathBuf>)>>>,
    drives_busy: Arc<AtomicBool>,
    index_roots: Vec<PathBuf>,
    net_refreshing: Arc<AtomicBool>,
    net_shares: Arc<std::sync::Mutex<Option<NetShares>>>,
    /// Trabajos de E/S lentos (crear, renombrar…) hechos en un hilo; cada uno deja aquí lo que debe
    /// ejecutarse en el hilo de la interfaz al terminar.
    io_done: Arc<std::sync::Mutex<Vec<Box<dyn FnOnce(&mut App) + Send>>>>,
    /// Vigilancia de cambios del directorio, consultada en un hilo (un `stat` en red puede bloquear).
    watch_res: Arc<std::sync::Mutex<Vec<(PathBuf, Option<web_time::SystemTime>)>>>,
    net_users: HashMap<String, String>,
    side_drop: Option<PathBuf>,
    /// carpeta (fila) bajo el puntero en el fotograma anterior: destino de lo soltado desde fuera
    hover_dir: Option<PathBuf>,
    bm_drop: bool,

    addr_edit: Option<String>,
    /// al abrir la edición de la ruta (Ctrl+L, clic en las migas) se selecciona todo el texto
    addr_select: bool,
    /// fotogramas restantes en los que se reaplica «seleccionar todo» (el foco puede tardar o perderse un instante)
    addr_select_n: u8,
    addr_hl: Option<usize>,
    addr_cache: (String, Vec<String>),
    addr_sugg_rect: Option<Rect>,
    queue: VecDeque<(Op, OpMeta)>,
    op_meta: OpMeta,
    op_started: Instant,
    /// el usuario ocultó el panel flotante de progreso (queda la barra de estado)
    op_card_hidden: bool,
    /// popup con la pila de tareas (escritorio)
    tasks_open: bool,
    /// diálogos y móvil (app_h2.rs): hoja de pestañas
    h2: app_h2::H2,
    /// Diálogo «hay tareas en curso» al intentar cerrar; `quit_ok` = ya confirmado.
    quit_ask: bool,
    /// Filas de la papelera mostradas (en el mismo orden que `entries` en la vista Papelera).
    trash_rows: Arc<std::sync::Mutex<Vec<extras::TrashRow>>>,
    trash_job: Option<std::sync::mpsc::Receiver<Result<(bool, usize), String>>>,
    confirm_purge: Option<PurgeReq>,
    /// Programa que se ejecutaría con doble clic o Enter: antes se pregunta.
    confirm_run: Option<PathBuf>,
    /// ¿Es programa? del archivo del menú contextual (None = calculándose en un hilo).
    prog_cache: Option<(PathBuf, Option<bool>)>,
    /// Carpeta de destino de la operación en curso (para seleccionar lo creado sólo si es la que se ve).
    op_dst: Option<PathBuf>,
    /// Errores de una operación que terminó con otro diálogo abierto: se muestran al cerrarlo.
    errs_later: Vec<String>,
    /// Shell con el que se abrió el terminal integrado (decide cómo se entrecomilla el `cd`).
    term_shell: String,
    /// Guardado de la configuración aplazado: se escribe tras un rato sin cambios (y al salir).
    cfg_save: SaveDebounce,
    /// Último enfoque/visibilidad comunicado al hilo de vigilancia: 0 minimizada, 1 sin foco, 2 con foco.
    vis_state: Arc<std::sync::atomic::AtomicU8>,
    /// Rutas que vigila el hilo de cambios de carpeta y su aviso para despertarlo.
    watch_paths: Arc<std::sync::Mutex<Vec<PathBuf>>>,
    #[cfg(not(target_arch = "wasm32"))]
    watch_wake: Option<Sender<()>>,
    /// Clave de un servidor SSH nuevo y los datos con los que se intentaba conectar.
    host_key: Option<(crate::sftp::PendingKey, NetLogin)>,
    net_last: Option<NetLogin>,
    quit_ok: bool,
    /// descripción de la operación en curso
    op_desc: String,
    /// Cambio de tema en curso: paleta de origen y de destino, si es oscuro al final y cuándo empezó.
    theme_anim: Option<(theme::Pal, theme::Pal, bool, Instant)>,
    undo_stack: Vec<Undo>,
    redo_stack: Vec<Undo>,
    conflict: Option<ConflictDlg>,
    closed_tabs: Vec<TabState>,
    rename: Option<(u32, String, bool)>,
    clipboard: Option<(Vec<PathBuf>, bool)>,
    /// `clipboard` se copió también al portapapeles de archivos del sistema.
    clip_sys: bool,
    op: Option<Arc<Progress>>,
    msg: Option<(String, Instant, bool)>,
    /// avisos apilados, diálogos de conflicto/borrado/compartir y cachés de la interfaz (`ux.rs`)
    ux: ux::UxState,
    confirm_delete: Option<Vec<PathBuf>>,
    /// Vista previa del renombrado en lote: nombre actual -> nombre nuevo (se pinta en la carpeta).
    batch_names: HashMap<String, String>,
    pending_select: Vec<String>,
    pending_rename: bool,
    typeahead: (String, Instant),
    vmode: VMode,
    search_st: Option<Arc<tools::Search>>,
    search_taken: usize,
    sbuf: String,
    search_focus: bool,

    transparent: bool,
    tabs: Vec<TabState>,
    cur_tab: usize,
    sys: SysIcons,
    special_dirs: Vec<(String, &'static str)>,
    home: Option<PathBuf>,
    term: Option<Terminal>,
    term_focus: bool,
    /// «Abrir con»: iconos de aplicaciones y consulta del submenú en curso
    ow: openwith_ui::OwState,
    menu_id: Option<egui::Id>,
    gear_id: Option<egui::Id>,
    dlg: Dlg,

    preview: Option<Preview>,
    pworker: preview::Worker,
    /// Panel de vista previa acoplado a la derecha (y su propio trabajador de carga).
    side: Option<Preview>,
    /// Origen y destino de la extracción en curso, por si pide contraseña.
    op_unzip: Option<(PathBuf, PathBuf)>,
    sworker: preview::Worker,
    hover_name: Option<String>,
    last_title: String,
    t_load: Instant,
    #[cfg(feature = "selftest")]
    traced_first: bool,
    #[cfg(feature = "selftest")]
    shot: selftest::Shot,

    /// Panel inactivo de la vista dividida (None = un solo panel).
    other: Option<Box<PaneState>>,
    /// Lado (0 izquierda / 1 derecha) del panel con foco y del que ocupa ahora los campos de `App`.
    active_side: u8,
    live_side: u8,
    pane_rects: [Rect; 2],

    palette: Option<PaletteState>,
    /// Comando cuyo atajo se está reasignando en Opciones.
    rebinding: Option<Cmd>,
    /// Estado de menús, atajos, bienvenida y opciones de interfaz (app_menus.rs)
    h1: menus::State,
    /// Accesibilidad: foco del área de archivos (a11y.rs)
    a11y: a11y::A11y,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, cfg: Config) -> App {
        let ctx = cc.egui_ctx.clone();
        let li = crate::i18n::resolve(&cfg.lang);
        crate::i18n::set(li);
        fsys::set_date_style(fsys::DateStyle::from_code(&cfg.date_fmt));
        let font_ok = crate::i18n::install_fonts(&ctx, li);
        if !font_ok {
            eprintln!("aviso: no se encontró una fuente CJK del sistema");
        }
        let mut cfg = cfg;
        if cfg!(target_os = "android") || crate::vfs::WEB {
            cfg.preview_window = false;
        }
        // Android y web: sin botones de ventana propios (minimizar/maximizar/cerrar no existen)
        if cfg!(target_os = "android") || crate::vfs::WEB {
            cfg.native_deco = true;
        }
        // en pantallas estrechas (móvil) la barra lateral empieza oculta
        if cfg!(target_os = "android") && ctx.content_rect().width() < 620.0 {
            cfg.show_sidebar = false;
        }
        let sys_theme = Arc::new(std::sync::atomic::AtomicU8::new(0));
        let vis_state = Arc::new(std::sync::atomic::AtomicU8::new(2));
        #[cfg(target_os = "linux")]
        {
            // la detección lanza procesos (gdbus, gsettings): fuera del arranque; mientras tanto se
            // usa el último tema guardado y, si el sistema ha cambiado, se ajusta con la animación.
            // Con la ventana minimizada no se consulta, y sin foco se espera más entre consultas.
            let (st, c, vis) = (sys_theme.clone(), ctx.clone(), vis_state.clone());
            crate::vfs::thread::spawn(move || loop {
                let v_now = vis.load(Ordering::Relaxed);
                if v_now != 0 {
                    let v = detect_system_dark().map(|d| if d { 2 } else { 1 }).unwrap_or(0);
                    if st.swap(v, Ordering::Relaxed) != v {
                        c.request_repaint();
                    }
                }
                crate::vfs::thread::sleep(Duration::from_secs(if v_now == 2 { 5 } else { 15 }));
            });
        }
        if cfg.theme_mode == 0 {
            let th = ctx.system_theme().map(|t| t == egui::Theme::Dark).or(match sys_theme.load(Ordering::Relaxed) {
                1 => Some(false),
                2 => Some(true),
                _ => None,
            });
            if let Some(d) = th {
                cfg.dark = d;
            }
        }
        cfg.sync_active();
        let (places, j) = app_icons::JState::init(&cfg);
        let pal = theme::resolve(&cfg, cfg.dark);
        theme::apply_style(&ctx, &pal, cfg.dark);
        let (load_tx, load_rx) = channel();
        let (sort_tx, sort_rx) = channel();
        let (extra_tx, extra_rx) = channel();
        // Rutas de la línea de órdenes: carpetas → pestañas; archivos → su carpeta con el archivo seleccionado.
        let mut cli_dirs: Vec<PathBuf> = Vec::new();
        // nombres a seleccionar en cada carpeta de `cli_dirs`
        let mut cli_sels: Vec<Vec<String>> = Vec::new();
        for p in system::initial_targets() {
            let (dir, name) = if p.v_is_dir() {
                (p, None)
            } else if let (Some(parent), Some(name)) = (p.parent(), p.file_name()) {
                if !(p.v_exists() && parent.v_is_dir()) {
                    continue;
                }
                (parent.to_path_buf(), Some(name.to_string_lossy().into_owned()))
            } else {
                continue;
            };
            let i = match cli_dirs.iter().position(|d| *d == dir) {
                Some(i) => i,
                None => {
                    cli_dirs.push(dir);
                    cli_sels.push(Vec::new());
                    cli_dirs.len() - 1
                }
            };
            if let Some(n) = name {
                if !cli_sels[i].contains(&n) {
                    cli_sels[i].push(n);
                }
            }
        }
        let cli_select: Vec<String> = cli_sels.first().cloned().unwrap_or_default();
        let cli_dir = cli_dirs.first().cloned();
        let mut start = cli_dir
            .clone()
            .or_else(|| cfg.last_dir.clone())
            // sin bloquear en un montaje de red muerto: si no contesta, se intenta igual (carga en segundo plano).
            // Una dirección sftp:// guardada se reconecta al cargarla (ver `load_mode`).
            .filter(|p| app_nav::restorable(p))
            .or_else(dirs::home_dir)
            .unwrap_or_else(|| PathBuf::from("/"));
        let mut tabs_init: Vec<TabState> = Vec::new();
        let mut cur_init = 0;
        if cli_dirs.len() > 1 {
            tabs_init = cli_dirs
                .iter()
                .zip(&cli_sels)
                .map(|(p, s)| {
                    let mut t = TabState::new(p.clone());
                    t.sel = s.clone();
                    t
                })
                .collect();
        }
        if cli_dir.is_none() && cfg.restore_tabs && (cfg.tabs.len() > 1 || !cfg.tabs2.is_empty()) {
            for (n, p) in cfg.tabs.iter().enumerate() {
                if !app_nav::restorable(p) {
                    continue;
                }
                let mut t = TabState::new(p.clone());
                if let Some((_, p2)) = cfg.tabs2.iter().find(|(i, p2)| *i == n && app_nav::restorable(p2)) {
                    t.split = Some(Box::new(TabState::new(p2.clone())));
                }
                tabs_init.push(t);
            }
            if !tabs_init.is_empty() {
                cur_init = cfg.cur_tab.min(tabs_init.len() - 1);
                start = tabs_init[cur_init].cwd.clone();
            }
        }
        if tabs_init.is_empty() {
            tabs_init.push(TabState::new(start.clone()));
        }
        let ctx_for_inbox = ctx.clone();
        #[cfg(target_os = "macos")]
        let ctx_for_mac = ctx.clone();
        if cfg.index_enabled {
            index::start(tools::all_roots(), cc.egui_ctx.clone());
        }
        let mut app = App {
            was_focused: true,
            transparent: !cfg.native_deco,
            thumbs: Thumbs::new(&ctx),
            thumb_want: Vec::new(),
            thumb_any: false,
            pworker: preview::Worker::new(&ctx),
            side: None,
            op_unzip: None,
            sworker: preview::Worker::new(&ctx),
            ctx,
            pal,
            cfg,
            cwd: start.clone(),
            back: vec![],
            fwd: vec![],
            back_snap: vec![],
            fwd_snap: vec![],
            generation: 0,
            gen_shared: Arc::new(AtomicU64::new(0)),
            load_tx,
            load_rx,
            loading: false,
            sort_pending: false,
            touch: cfg!(target_os = "android"),
            touch_press: None,
            phone: false,
            addr_press: None,
            last_tap: None,
            touch_pt: None,
            perm_ok: true,
            tap_block: false,
            preview_was: false,
            extra_cache: HashMap::new(),
            extra_tx,
            extra_rx,
            extra_pending: Default::default(),
            arch: Vec::new(),
            arch_ext: HashSet::new(),
            clip_hidden: false,
            col_cache: HashMap::new(),
            col_busy: HashSet::new(),
            col_active: false,
            opts_tab: 0,
            edit_theme: String::new(),
            perm_t: -10.0,
            insets: [0.0; 4],
            insets_t: -10.0,
            sel_mode: false,
            kin_off: 0.0,
            kin_vel: 0.0,
            kin_k: 1.4,
            kin_wheel_until: 0.0,
            kin_hist: Vec::new(),
            keep_scroll: None,
            keep_cursor: None,
            sys_theme: sys_theme.clone(),
            load_err: None,
            dir_hash: 0,
            dir_mtime: None,
            last_check: Instant::now(),
            entries: Arc::new(Vec::new()),
            order: Vec::new(),
            sort_seq: 0,
            sort_tx,
            sort_rx,
            filter: String::new(),
            filter_focus: false,
            sel: Vec::new(),
            sel_count: 0,
            sel_size: 0,
            anchor: None,
            cursor: None,
            band: None,
            drag_items: false,
            dragout: None,
            dragout_tried: false,
            release_inject: false,
            last_ptr: None,
            ext_dropped: Vec::new(),
            win_drag: None,
            inbox: system::listen({
                let c = ctx_for_inbox;
                move || c.request_repaint()
            }),
            ow_cache: None,
            scroll_to: None,
            cols: 1,
            page_rows: 10,
            places,
            j,
            drives: list_drives(),
            network: Vec::new(),
            // vencido: la primera pasada de la lista de red se hace en segundo plano al arrancar
            net_check: Instant::now().checked_sub(Duration::from_secs(10)).unwrap_or_else(Instant::now),
            net_nav: Arc::new(std::sync::Mutex::new(None)),
            net_err: Arc::new(std::sync::Mutex::new(None)),
            net_busy: Arc::new(AtomicBool::new(false)),
            net_list: Arc::new(std::sync::Mutex::new(None)),
            drives_slot: Arc::new(std::sync::Mutex::new(None)),
            drives_busy: Arc::new(AtomicBool::new(false)),
            index_roots: tools::all_roots(),
            net_refreshing: Arc::new(AtomicBool::new(false)),
            net_shares: Arc::new(std::sync::Mutex::new(None)),
            io_done: Arc::new(std::sync::Mutex::new(Vec::new())),
            watch_res: Arc::new(std::sync::Mutex::new(Vec::new())),
            net_users: HashMap::new(),
            side_drop: None,
            hover_dir: None,
            bm_drop: false,
            addr_edit: None,
            addr_select: false,
            addr_select_n: 0,
            addr_hl: None,
            addr_cache: (String::new(), Vec::new()),
            addr_sugg_rect: None,
            queue: VecDeque::new(),
            op_meta: OpMeta::Other,
            op_started: Instant::now(),
            op_card_hidden: false,
            tasks_open: false,
            h2: app_h2::H2::default(),
            quit_ask: false,
            trash_rows: Arc::new(std::sync::Mutex::new(Vec::new())),
            trash_job: None,
            confirm_purge: None,
            confirm_run: None,
            prog_cache: None,
            op_dst: None,
            errs_later: Vec::new(),
            term_shell: String::new(),
            cfg_save: SaveDebounce::default(),
            vis_state,
            watch_paths: Arc::new(std::sync::Mutex::new(Vec::new())),
            #[cfg(not(target_arch = "wasm32"))]
            watch_wake: None,
            host_key: None,
            net_last: None,
            quit_ok: false,
            op_desc: String::new(),
            theme_anim: None,
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            conflict: None,
            closed_tabs: Vec::new(),
            rename: None,
            clipboard: None,
            clip_sys: false,
            op: None,
            msg: None,
            ux: ux::UxState::default(),
            confirm_delete: None,
            pending_select: cli_select,
            batch_names: HashMap::new(),
            pending_rename: false,
            typeahead: (String::new(), Instant::now()),
            vmode: VMode::Dir,
            search_st: None,
            search_taken: 0,
            sbuf: String::new(),
            search_focus: false,
            preview: None,
            tabs: tabs_init,
            cur_tab: cur_init,
            sys: SysIcons::new(),
            special_dirs: [
                (dirs::document_dir(), "documents"),
                (dirs::download_dir(), "downloads"),
                (dirs::audio_dir(), "music"),
                (dirs::picture_dir(), "pictures"),
                (dirs::video_dir(), "videos"),
                (dirs::desktop_dir(), "desktop"),
            ]
            .into_iter()
            .filter_map(|(p, k)| p.and_then(|p| p.file_name().map(|n| (n.to_string_lossy().into_owned(), k))))
            .collect(),
            home: dirs::home_dir(),
            term: None,
            term_focus: false,
            ow: Default::default(),
            menu_id: None,
            gear_id: None,
            dlg: Dlg::None,
            hover_name: None,
            last_title: String::new(),
            t_load: Instant::now(),
            #[cfg(feature = "selftest")]
            traced_first: false,
            #[cfg(feature = "selftest")]
            shot: Default::default(),
            other: None,
            active_side: 0,
            live_side: 0,
            pane_rects: [Rect::NOTHING; 2],
            palette: None,
            rebinding: None,
            h1: menus::State::default(),
            a11y: a11y::A11y::default(),
        };
        app.h1_init();
        crate::vfs::thread::spawn(clean_archive_cache);
        #[cfg(feature = "selftest")]
        app.shot_view();
        #[cfg(target_os = "macos")]
        system::hook_open_files(app.inbox.clone(), move || ctx_for_mac.request_repaint());
        app.load(start, false);
        #[cfg(feature = "selftest")]
        let shot_split = app.shot_split();
        #[cfg(not(feature = "selftest"))]
        let shot_split: Option<PathBuf> = None;
        let split_dir = if shot_split.is_some() {
            shot_split
        } else if app.cfg.split && !app.cfg.col_view {
            Some(app.cfg.split_dir.clone().filter(|p| p.v_is_dir()).unwrap_or_else(|| app.cwd.clone()))
        } else {
            None
        };
        if let Some(d) = split_dir {
            app.open_split(d);
        }
        #[cfg(feature = "selftest")]
        app.shot_filter();
        app
    }

}

// ---------------------------------------------------------------------------
// Dibujo
// ---------------------------------------------------------------------------

/// Botón de icono que es un interruptor (vista de lista, cuadrícula…): anuncia si está activo.
fn toggle_button(ui: &mut egui::Ui, pal: &Pal, g: Glyph, on: bool, tip: &str) -> egui::Response {
    let r = icon_button(ui, pal, g, true, on, tip);
    r.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Button, true, on, tip));
    r
}

fn icon_button(ui: &mut egui::Ui, pal: &Pal, g: Glyph, enabled: bool, active: bool, tip: &str) -> egui::Response {
    // tamaño mínimo de objetivo (mayor en pantallas táctiles)
    let (rect, resp) = ui.allocate_exact_size(theme::target_size(vec2(34.0, 34.0)), if enabled { Sense::click() } else { Sense::hover() });
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, enabled, tip));
    let p = ui.painter();
    if enabled && (resp.hovered() || active) {
        let fill = if active { pal.sel } else { pal.hover };
        p.rect_filled(rect, CornerRadius::same(10), fill);
        if active {
            // activo (vista elegida…): contorno ≥ 3:1, no sólo el tinte y el color del icono
            p.rect_stroke(rect, CornerRadius::same(10), Stroke::new(1.0, a11y::sel_ring(pal)), egui::StrokeKind::Inside);
        }
    }
    if enabled {
        theme::focus_ring(ui, pal, &resp, 10.0);
    }
    let col = if !enabled {
        pal.disabled
    } else if active {
        pal.accent
    } else {
        pal.text
    };
    theme::draw_glyph(p, rect.center(), 20.0, g, col);
    // también inactivo: la descripción dice qué es (el botón inactivo sólo «pasa por encima»)
    if !tip.is_empty() {
        resp.on_hover_text(tip)
    } else {
        resp
    }
}

/// Botón pequeño con un triángulo (subir/bajar), dibujado a mano: la fuente no trae flechas.
fn tri_button(ui: &mut egui::Ui, pal: &Pal, up: bool, enabled: bool) -> egui::Response {
    // objetivo mínimo pulsable (mayor en táctil); el triángulo sigue igual de pequeño
    let (rect, resp) = ui.allocate_exact_size(theme::target_size(vec2(26.0, 22.0)), if enabled { Sense::click() } else { Sense::hover() });
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, enabled, if up { t("Subir") } else { t("Bajar") }));
    let vis = ui.visuals();
    let col = if enabled { vis.text_color() } else { pal.disabled };
    if enabled && resp.hovered() {
        ui.painter().rect_filled(rect, CornerRadius::same(6), vis.widgets.hovered.weak_bg_fill);
    }
    theme::focus_ring(ui, pal, &resp, 6.0);
    let c = rect.center();
    let d = if up { -1.0 } else { 1.0 };
    ui.painter().add(egui::Shape::convex_polygon(
        vec![c + vec2(-4.5, -2.5 * d), c + vec2(4.5, -2.5 * d), c + vec2(0.0, 3.0 * d)],
        col,
        Stroke::NONE,
    ));
    resp
}

fn menu_head(ui: &mut egui::Ui, pal: &Pal, text: &str) {
    ui.add_space(4.0);
    ui.label(egui::RichText::new(text.to_uppercase()).size(10.5).color(pal.subtle).strong());
}

/// Fila de menú con resalte cuando está activa.
fn menu_row(ui: &mut egui::Ui, pal: &Pal, text: &str, on: bool, asc: Option<bool>) -> bool {
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), ui.spacing().interact_size.y.max(28.0)), Sense::click());
    resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, true, on, text));
    theme::focus_ring(ui, pal, &resp, 8.0);
    let p = ui.painter();
    if on {
        p.rect_filled(rect, CornerRadius::same(8), pal.sel);
        p.rect_stroke(rect, CornerRadius::same(8), Stroke::new(1.0, a11y::sel_ring(pal)), egui::StrokeKind::Inside);
    } else if resp.hovered() {
        p.rect_filled(rect, CornerRadius::same(8), pal.hover);
    }
    let col = if on { pal.accent } else { pal.text };
    p.text(pos2(rect.left() + 10.0, rect.center().y), Align2::LEFT_CENTER, text, FontId::proportional(13.5), col);
    if let (true, Some(asc)) = (on, asc) {
        // triángulo de dirección de orden, como en la cabecera de la lista
        let c = pos2(rect.right() - 14.0, rect.center().y);
        let d = if asc { 1.0 } else { -1.0 };
        p.add(egui::Shape::convex_polygon(
            vec![c + vec2(-4.0, 2.5 * d), c + vec2(4.0, 2.5 * d), c + vec2(0.0, -2.5 * d)],
            col,
            Stroke::NONE,
        ));
    }
    resp.clicked()
}

/// Último componente de un nombre de entrada (en vistas recursivas/de búsqueda el nombre es una ruta).
fn base_name(s: &str) -> &str {
    let pos = if cfg!(windows) { s.rfind(['/', '\\']) } else { s.rfind('/') };
    match pos {
        Some(i) => &s[i + 1..],
        None => s,
    }
}

/// Texto corto de una operación: «Moviendo 3 elementos», «Comprimiendo informe.pdf»…
fn op_desc(op: &Op) -> String {
    let what = |v: &[PathBuf]| -> String {
        match v {
            [one] => one.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
            _ => crate::i18n::tn("{} elemento|{} elementos", v.len(), &[]),
        }
    };
    match op {
        Op::Copy { srcs, mv, .. } => format!("{} {}", if *mv { t("Moviendo") } else { t("Copiando") }, what(srcs)),
        Op::Trash(v) => format!("{} {}", t("Moviendo a la papelera"), what(v)),
        Op::Delete(v) => format!("{} {}", t("Eliminando"), what(v)),
        Op::Compress { srcs, .. } => format!("{} {}", t("Comprimiendo"), what(srcs)),
        Op::Unzip { src, .. } => format!("{} {}", t("Extrayendo"), what(std::slice::from_ref(src))),
        Op::Fetch(v) => format!("{} {}", t("Descargando"), what(v)),
    }
}

fn side_item(ui: &mut egui::Ui, pal: &Pal, g: Glyph, tex: Option<egui::TextureId>, label: &str, active: bool, drop_hot: bool) -> egui::Response {
    let w = ui.available_width();
    let (rect, resp) = ui.allocate_exact_size(vec2(w, theme::min_target().max(32.0)), Sense::click());
    resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, true, active, label));
    let p = ui.painter();
    if drop_hot {
        p.rect_filled(rect, CornerRadius::same(10), pal.sel);
        p.rect_stroke(rect, CornerRadius::same(10), Stroke::new(1.5, pal.accent), egui::StrokeKind::Inside);
    } else if active {
        // el relleno de selección es suave: el contorno marca el estado
        p.rect_filled(rect, CornerRadius::same(10), pal.sel);
        p.rect_stroke(rect, CornerRadius::same(10), Stroke::new(1.0, pal.focus), egui::StrokeKind::Inside);
    } else if resp.hovered() {
        p.rect_filled(rect, CornerRadius::same(10), pal.hover);
    }
    theme::focus_ring(ui, pal, &resp, 10.0);
    let col = if active { pal.accent } else { pal.text };
    match tex {
        Some(id) => {
            let ir = Rect::from_center_size(pos2(rect.left() + 19.0, rect.center().y), vec2(20.0, 20.0));
            p.image(id, ir, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
        }
        None => theme::draw_glyph(p, pos2(rect.left() + 19.0, rect.center().y), 16.0, g, col),
    }
    let mut t = label.to_string();
    let maxw = w - 46.0;
    let gl = p.layout_no_wrap(t.clone(), FontId::proportional(13.5), col);
    if gl.size().x > maxw {
        let keep = ((maxw / gl.size().x) * t.chars().count() as f32) as usize;
        t = t.chars().take(keep.saturating_sub(1)).collect::<String>() + "…";
    }
    p.text(pos2(rect.left() + 36.0, rect.center().y), Align2::LEFT_CENTER, t, FontId::proportional(13.5), col);
    resp
}

/// Estado de un arrastre de ventana iniciado por nosotros.
struct WinDrag {
    /// `None`: mover; `Some`: redimensionar por ese borde
    dir: Option<egui::ResizeDirection>,
    cursor0: Pos2,
    outer0: Rect,
    inner0: Rect,
    /// el arrastre del sistema no ha movido la ventana: se mueve a mano
    manual: bool,
}

/// Posición del cursor en la pantalla, en puntos (sólo Windows).
#[cfg(windows)]
fn screen_cursor(ppp: f32) -> Option<Pos2> {
    use windows::Win32::Foundation::POINT;
    use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;
    let mut p = POINT::default();
    unsafe { GetCursorPos(&mut p).ok()? };
    Some(pos2(p.x as f32 / ppp, p.y as f32 / ppp))
}

#[cfg(not(windows))]
fn screen_cursor(_ppp: f32) -> Option<Pos2> {
    None
}

// ---------------------------------------------------------------------------
// Pestañas, menús, diálogos y terminal
// ---------------------------------------------------------------------------

fn mi(ui: &mut egui::Ui, label: &str, shortcut: &str, enabled: bool) -> bool {
    let mut b = egui::Button::new(label);
    if !shortcut.is_empty() {
        b = b.shortcut_text(shortcut);
    }
    let c = ui.add_enabled(enabled, b).clicked();
    if c {
        ui.close();
    }
    c
}

fn mi_check(ui: &mut egui::Ui, label: &str, shortcut: &str, on: bool) -> bool {
    let mut b = egui::Button::new(format!("{}  {label}", if on { "✔" } else { "   " }));
    if !shortcut.is_empty() {
        b = b.shortcut_text(shortcut);
    }
    let r = ui.add(b);
    // la marca ✔ no se lee: el estado va como casilla
    r.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Checkbox, true, on, label));
    let c = r.clicked();
    if c {
        ui.close();
    }
    c
}

fn mtime_secs(md: &crate::vfs::fs::Metadata) -> i64 {
    md.modified()
        .ok()
        .and_then(|t| t.duration_since(web_time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn split_ext(name: &str) -> (&str, &str) {
    match name.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() && e.len() <= 8 => (s, e),
        _ => (name, ""),
    }
}

/// Expande {nombre}, {n}, {n:ANCHO}, {fecha} y {padre} de la plantilla.
fn expand_template(tpl: &str, stem: &str, n: usize, date: &str, parent: &str) -> String {
    let mut out = String::new();
    let mut rest = tpl;
    while let Some(a) = rest.find('{') {
        out.push_str(&rest[..a]);
        let Some(b) = rest[a..].find('}') else {
            rest = &rest[a..];
            break;
        };
        let key = &rest[a + 1..a + b];
        match key {
            "nombre" => out.push_str(stem),
            "fecha" => out.push_str(date),
            "padre" => out.push_str(parent),
            "n" => out.push_str(&n.to_string()),
            k if k.starts_with("n:") => match k[2..].parse::<usize>() {
                Ok(w) if w <= 12 => out.push_str(&format!("{n:0w$}")),
                _ => out.push_str(&rest[a..a + b + 1]),
            },
            _ => out.push_str(&rest[a..a + b + 1]),
        }
        rest = &rest[a + b + 1..];
    }
    out.push_str(rest);
    out
}

fn batch_name(old: &str, d: &BatchDlg, i: usize, re: Option<&regex::Regex>) -> String {
    let (stem, ext) = split_ext(old);
    let mut s = if d.template.trim().is_empty() {
        stem.to_string()
    } else {
        let date = d.mtimes.get(i).map(|&m| crate::fsys::fmt_time_iso(m)).unwrap_or_default();
        expand_template(&d.template, stem, (d.start as usize).saturating_add(i), date.get(..10).unwrap_or(""), &d.parent)
    };
    if !d.find.is_empty() {
        s = match (d.regex, re) {
            (true, Some(r)) => r.replace_all(&s, d.repl.as_str()).into_owned(),
            (true, None) => s,
            _ => s.replace(&d.find, &d.repl),
        };
    }
    match d.case {
        1 => s = s.to_lowercase(),
        2 => s = s.to_uppercase(),
        3 => {
            s = s
                .split(' ')
                .map(|w| {
                    let mut c = w.chars();
                    c.next().map(|f| f.to_uppercase().collect::<String>() + &c.as_str().to_lowercase()).unwrap_or_default()
                })
                .collect::<Vec<_>>()
                .join(" ")
        }
        _ => {}
    }
    s = format!("{}{}{}", d.prefix, s, d.suffix);
    if d.number && d.template.trim().is_empty() {
        s = format!("{} {:03}", s, (d.start as usize).saturating_add(i));
    }
    if ext.is_empty() { s } else { format!("{s}.{ext}") }
}

#[cfg(unix)]
fn perm_bits(md: &crate::vfs::fs::Metadata) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    md.permissions().mode() & 0o777
}

#[cfg(not(unix))]
fn perm_bits(md: &crate::vfs::fs::Metadata) -> u32 {
    if md.permissions().readonly() { 0o444 } else { 0o666 }
}

/// Aplica permisos (rwx de propietario/grupo/otros); en carpetas puede propagarse al contenido
/// conservando en los archivos sus bits de ejecución.
#[cfg(unix)]
fn set_perms(p: &Path, bits: u32, recursive: bool, w: &tools::Work) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    w.step()?;
    let md = crate::vfs::fs::symlink_metadata(p)?;
    if md.file_type().v_is_symlink() {
        return Ok(());
    }
    let old = md.permissions().mode();
    let new = if md.v_is_dir() { (old & !0o777) | bits } else { (old & !0o777) | bits };
    // si la carpeta pierde lectura/ejecución, primero se recorre el contenido y la carpeta se cambia al final
    let last = recursive && md.v_is_dir() && new & 0o500 != 0o500;
    if !last {
        crate::vfs::fs::set_permissions(p, crate::vfs::fs::Permissions::from_mode(new))?;
    }
    if recursive && md.v_is_dir() {
        for e in crate::vfs::fs::read_dir(p)? {
            let e = e?;
            let path = e.path();
            let child = crate::vfs::fs::symlink_metadata(&path)?;
            if child.v_is_dir() {
                set_perms(&path, bits, true, w)?;
            } else if !child.file_type().v_is_symlink() {
                w.step()?;
                // archivos: se cambian lectura/escritura, la ejecución se queda como estaba
                let cm = child.permissions().mode();
                let nb = (cm & !0o666) | (bits & 0o666);
                crate::vfs::fs::set_permissions(&path, crate::vfs::fs::Permissions::from_mode(nb))?;
            }
        }
    }
    if last {
        crate::vfs::fs::set_permissions(p, crate::vfs::fs::Permissions::from_mode(new))?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn set_perms(p: &Path, bits: u32, recursive: bool, w: &tools::Work) -> std::io::Result<()> {
    // en Windows sólo existe el atributo de sólo lectura (bit de escritura del propietario)
    w.step()?;
    let md = crate::vfs::fs::symlink_metadata(p)?;
    if md.file_type().v_is_symlink() {
        return Ok(());
    }
    let mut perm = md.permissions();
    perm.set_readonly(bits & 0o200 == 0);
    crate::vfs::fs::set_permissions(p, perm)?;
    if recursive && md.v_is_dir() {
        for e in crate::vfs::fs::read_dir(p)? {
            set_perms(&e?.path(), bits, true, w)?;
        }
    }
    Ok(())
}

/// Lo que llegó a aplicarse desde el diálogo de propiedades (se para en el primer error).
#[derive(Default)]
struct PropsDone {
    perm: Option<u32>,
    mode: Option<String>,
    secs: Option<i64>,
    renamed: Option<(PathBuf, String)>,
    err: Option<String>,
}

/// E/S de «Aplicar» en Propiedades, en el mismo orden de siempre: permisos, fecha y nombre.
/// `rename` es (nombre nuevo, nombre actual). Se ejecuta fuera del hilo de la interfaz.
fn props_work(paths: &[PathBuf], perm: Option<u32>, recursive: bool, secs: Option<i64>, rename: Option<(String, String)>, w: &tools::Work) -> PropsDone {
    let mut r = PropsDone::default();
    let stopped = |e: &std::io::Error, what: &str| if e.kind() == std::io::ErrorKind::Interrupted { t("Operación cancelada").to_string() } else { format!("{what}: {e}") };
    if let Some(bits) = perm {
        for p in paths {
            if let Err(e) = set_perms(p, bits, recursive, w) {
                r.err = Some(stopped(&e, t("No se pudieron cambiar los permisos")));
                return r;
            }
        }
        r.perm = Some(bits);
        if paths.len() == 1 {
            r.mode = crate::vfs::fs::symlink_metadata(&paths[0]).ok().map(|md| mode_string(&md));
        }
    }
    if let Some(secs) = secs {
        for p in paths {
            if let Err(e) = w.step().and_then(|()| fsys::set_mtime(p, secs)) {
                r.err = Some(stopped(&e, t("No se pudo cambiar la fecha")));
                return r;
            }
        }
        r.secs = Some(secs);
    }
    if let (Some((new, orig)), [src]) = (rename, paths) {
        let dst = src.with_file_name(&new);
        let case_only = new.to_lowercase() == orig.to_lowercase()
            && dst.v_symlink_metadata().is_ok()
            && matches!((crate::vfs::fs::canonicalize(src), crate::vfs::fs::canonicalize(&dst)), (Ok(a), Ok(b)) if a == b);
        if !case_only && dst.v_symlink_metadata().is_ok() {
            r.err = Some(t("Ya existe un elemento con ese nombre").to_string());
            return r;
        }
        if let Err(e) = crate::vfs::fs::rename(src, &dst) {
            r.err = Some(format!("{}: {e}", t("No se pudo renombrar")));
            return r;
        }
        r.renamed = Some((dst, new));
    }
    r
}

#[cfg(unix)]
fn mode_string(md: &crate::vfs::fs::Metadata) -> String {
    use std::os::unix::fs::PermissionsExt;
    let m = md.permissions().mode();
    let mut s = String::new();
    for sh in [6, 3, 0] {
        let b = (m >> sh) & 7;
        s.push(if b & 4 != 0 { 'r' } else { '-' });
        s.push(if b & 2 != 0 { 'w' } else { '-' });
        s.push(if b & 1 != 0 { 'x' } else { '-' });
    }
    format!("{s}  ({:o})", m & 0o777)
}

#[cfg(not(unix))]
fn mode_string(md: &crate::vfs::fs::Metadata) -> String {
    if md.permissions().readonly() { t("Sólo lectura").into() } else { t("Lectura y escritura").into() }
}

/// ¿Es un ejecutable nativo (ELF) o un script con shebang con permiso de ejecución?
#[cfg(all(unix, not(target_os = "android")))]
fn is_program(p: &Path) -> bool {
    use std::io::Read;
    use std::os::unix::fs::PermissionsExt;
    let Ok(md) = crate::vfs::fs::metadata(p) else { return false };
    if !md.v_is_file() || md.permissions().mode() & 0o111 == 0 {
        return false;
    }
    let mut head = [0u8; 4];
    let Ok(n) = crate::vfs::fs::File::open(p).and_then(|mut f| f.read(&mut head)) else { return false };
    n >= 2
        && (head[..2] == *b"#!"
            || head == [0x7f, b'E', b'L', b'F']
            // Mach-O (macOS): 32/64 bits y binarios universales
            || head == [0xcf, 0xfa, 0xed, 0xfe]
            || head == [0xce, 0xfa, 0xed, 0xfe]
            || head == [0xca, 0xfe, 0xba, 0xbe])
}

/// Windows: ejecutables, scripts, instaladores, accesos directos y demás tipos que ejecutan código.
#[cfg(windows)]
fn is_program(p: &Path) -> bool {
    let ext = p.extension().map(|e| e.to_string_lossy().into_owned()).unwrap_or_default();
    win_program_ext(&ext) && p.v_is_file()
}

#[cfg(not(any(windows, all(unix, not(target_os = "android")))))]
fn is_program(_: &Path) -> bool {
    false
}

/// `filebrowser-server` junto al ejecutable del gestor o en el PATH.
#[cfg(not(target_os = "android"))]
fn server_exe() -> Option<PathBuf> {
    let name = if cfg!(windows) { "filebrowser-server.exe" } else { "filebrowser-server" };
    let beside = std::env::current_exe().ok().and_then(|e| e.parent().map(|d| d.join(name)));
    if let Some(p) = beside.filter(|p| p.v_is_file()) {
        return Some(p);
    }
    std::env::var_os("PATH").and_then(|v| crate::vfs::split_paths(&v).map(|d| d.join(name)).find(|p| p.v_is_file()))
}

/// ¿Lleva la ruta caracteres de control? Teclearla en el terminal permitiría inyectar órdenes.
fn has_ctl(p: &Path) -> bool {
    p.to_string_lossy().chars().any(|c| c.is_control())
}

/// `cd` a la ruta para teclearlo en el terminal (según su shell); vacío si la ruta no es segura de teclear.
fn shell_quote(p: &Path, shell: &str) -> String {
    if has_ctl(p) {
        return String::new();
    }
    let kind = if cfg!(windows) { shell_kind(shell) } else { ShellKind::Posix };
    quote_cd(&p.display().to_string(), kind)
}

impl eframe::App for App {
    fn raw_input_hook(&mut self, _ctx: &egui::Context, raw: &mut egui::RawInput) {
        if cfg!(target_os = "android") {
            // Al salir el teclado, Android quita un instante el foco a la ventana. egui sólo pide el
            // teclado (IME) a un campo de texto si la ventana tiene el foco, así que el teclado se
            // ocultaba solo y volvía a salir con el texto deseleccionado: se ignora esa pérdida.
            raw.focused = true;
            raw.events.retain(|e| !matches!(e, egui::Event::WindowFocused(false)));
        }
        if std::mem::take(&mut self.release_inject) {
            let pos = self.last_ptr.unwrap_or(Pos2::ZERO);
            raw.events.push(egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: Modifiers::NONE,
            });
            raw.events.push(egui::Event::PointerGone);
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        // ¿había un desplegable abierto al empezar? (Esc lo cierra a él, no al diálogo)
        theme::note_frame_start(&ctx);
        #[cfg(feature = "selftest")]
        self.shot_trace_ui();
        // al volver de otra aplicación: forzar repintado (el compositor deja de avisar de fotogramas a las
        // ventanas ocultas) y resincronizar el vídeo de la vista previa
        let focused = ctx.input(|i| i.viewport().focused.unwrap_or(true));
        if focused && !self.was_focused {
            // lo que se dejó de consultar sin foco se pone al día ya
            self.net_check = Instant::now().checked_sub(Duration::from_secs(60)).unwrap_or_else(Instant::now);
            ctx.request_repaint();
            ctx.request_repaint_after(Duration::from_millis(60));
            ctx.request_repaint_after(Duration::from_millis(250));
            if let Some(p) = self.preview.as_mut().and_then(|pv| pv.player.as_mut()) {
                p.resync();
            }
        }
        self.was_focused = focused;
        self.flush_thumbs();
        self.drag_out_tick(&ctx, frame);
        self.window_drag_tick(&ctx);
        let pal = self.pal;
        self.thumbs.pump(&ctx);
        self.sys.begin_frame();
        self.poll();
        if self.other.is_some() {
            self.swap_pane();
            self.poll();
            self.swap_pane();
        }
        self.tick_theme();
        if self.cfg.index_enabled {
            index::tick(self.index_roots.clone(), &ctx);
            if matches!(index::status(), index::Status::Building(_)) {
                ctx.request_repaint_after(std::time::Duration::from_millis(500));
            }
        }
        self.drain_inbox(frame);
        if self.cfg.theme_mode == 0 {
            if let Some(dark) = self.system_dark() {
                if dark != self.cfg.dark {
                    self.apply_dark(dark);
                    self.save_cfg();
                }
            }
        }
        // botones laterales del ratón: atrás / adelante
        let (mb_back, mb_fwd) = ctx.input(|i| (i.pointer.button_pressed(egui::PointerButton::Extra1), i.pointer.button_pressed(egui::PointerButton::Extra2)));
        // la tecla Atrás de Android cierra lo abierto antes de retroceder; el botón del ratón (y la tecla
        // Atrás de un teclado de escritorio) sólo recorre el historial, como Adelante
        let key_back = ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::BrowserBack));
        if key_back && cfg!(target_os = "android") {
            self.on_back_key();
        } else if (key_back || mb_back) && self.preview.is_none() && matches!(self.dlg, Dlg::None) {
            self.run_cmd(Cmd::Back);
        } else if mb_fwd && self.preview.is_none() && matches!(self.dlg, Dlg::None) {
            self.run_cmd(Cmd::Forward);
        }
        if !self.touch && ctx.input(|i| i.events.iter().any(|e| matches!(e, egui::Event::Touch { .. }))) {
            self.touch = true;
        }
        self.poll_op();
        // unidades y red: cada 10 s sólo con la ventana activa (al volver a ella, enseguida)
        if focused && self.net_check.elapsed() > Duration::from_secs(10) {
            self.net_check = Instant::now();
            self.refresh_drives();
            self.refresh_network();
        }
        if focused {
            ctx.request_repaint_after(Duration::from_secs(10));
        }
        if let Some((d, r)) = crate::thumbs::lock(&self.drives_slot).take() {
            self.drives = d;
            self.index_roots = r;
        }
        if let Some(v) = crate::thumbs::lock(&self.net_list).take() {
            self.network = v;
        }
        let nerr = crate::thumbs::lock(&self.net_err).take();
        if let Some(m) = nerr {
            match (m == crate::sftp::HOSTKEY, crate::sftp::take_pending_key(), self.net_last.clone()) {
                // servidor SSH nuevo: el usuario comprueba la huella antes de confiar en él
                (true, Some(k), Some(l)) => {
                    self.msg = None;
                    self.host_key = Some((k, l));
                }
                _ => self.say(tf("No se pudo conectar: {}", &[&m]), true),
            }
        }
        let shares = crate::thumbs::lock(&self.net_shares).take();
        if let Some(sh) = shares {
            self.msg = None;
            self.dlg = Dlg::NetShares(sh);
        }
        let nav = crate::thumbs::lock(&self.net_nav).take();
        if let Some(p) = nav {
            self.refresh_network();
            self.navigate(p);
        }
        if let Some(pv) = &mut self.preview {
            pv.poll(&mut self.pworker, &ctx);
        }
        if let Some(pv) = &mut self.side {
            pv.poll(&mut self.sworker, &ctx);
        }

        // título de ventana
        let title = format!(
            "{} — File Browser",
            self.dir_label(&self.cwd)
        );
        if title != self.last_title {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.last_title = title;
        }
        // tamaño sin la escala de la interfaz (al arrancar se abre con él antes de aplicarla)
        let sz = ctx.content_rect().size() * ctx.zoom_factor();
        // maximizada o a pantalla completa no se guarda: al restaurar vuelve al tamaño normal
        let full = ctx.input(|i| i.viewport().maximized.unwrap_or(false) || i.viewport().fullscreen.unwrap_or(false));
        if !full && ((sz.x - self.cfg.win.0).abs() > 1.0 || (sz.y - self.cfg.win.1).abs() > 1.0) {
            self.cfg.win = (sz.x, sz.y);
        }

        // trabajos de E/S terminados en segundo plano
        let done: Vec<_> = std::mem::take(&mut *crate::thumbs::lock(&self.io_done));
        for f in done {
            f(self);
        }
        // errores de una tarea que acabó con otro diálogo abierto
        if !self.errs_later.is_empty() && matches!(self.dlg, Dlg::None) {
            self.dlg = Dlg::Errors(std::mem::take(&mut self.errs_later));
        }

        // vigilancia de cambios en el directorio: un único hilo hace el `stat` (en red puede tardar), más
        // a menudo con foco que sin él y nunca con la ventana minimizada; avisa sólo si algo cambia
        self.watch_tick(&ctx, focused);
        self.release_archives();
        // configuración: se escribe tras medio segundo sin cambios
        if self.cfg_save.pending() {
            let now = ctx.input(|i| i.time);
            if self.cfg_save.due(now) {
                if let Err(e) = self.cfg.try_save() {
                    self.say(tf("No se pudo guardar la configuración: {}", &[&e]), true);
                }
            } else {
                ctx.request_repaint_after(Duration::from_millis(250));
            }
        }

        let modal = !matches!(self.dlg, Dlg::None) || self.palette.is_some() || self.confirm_delete.is_some() || self.confirm_purge.is_some() || self.conflict.is_some() || self.quit_ask || self.confirm_run.is_some() || self.host_key.is_some() || self.ux_modal();
        if !modal {
            self.handle_keys(&ctx);
        }

        // Archivos soltados desde el sistema: se copian a la carpeta actual.
        let (dropped, hovering) = ctx.input(|i| {
            #[cfg(target_arch = "wasm32")]
            for f in &i.raw.dropped_files {
                crate::vfs::net::read_dropped(f.clone(), ctx.clone());
            }
            (
                if crate::vfs::WEB { Vec::new() } else { i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).collect::<Vec<_>>() },
                !i.raw.hovered_files.is_empty(),
            )
        });
        let mut dropped = dropped;
        dropped.append(&mut self.ext_dropped);
        // destino: la carpeta bajo el puntero, el otro panel o la carpeta actual (sin posición conocida)
        let drop_dst = self.ext_drop_dst(&ctx);
        if !dropped.is_empty() {
            let dst = drop_dst.clone();
            // soltar en la carpeta donde ya están no los duplica como «x (2)»
            dropped.retain(|p| p.parent() != Some(dst.as_path()) && !dst.starts_with(p));
            if dropped.is_empty() {
                self.say(t("Ya está en esta carpeta").into(), false);
            } else {
                self.start_copy(dropped, dst, false);
            }
        }

        // Web: lo soltado en la página o elegido con «Subir…» se guarda en la carpeta actual
        #[cfg(target_arch = "wasm32")]
        for (name, r) in crate::vfs::net::run_uploads(&self.cwd) {
            match r {
                Ok(()) => self.refresh(),
                Err(e) => self.say(format!("{name}: {e}"), true),
            }
        }

        // Ventana redondeada: el fondo se pinta aquí y los paneles son transparentes.
        let bgfill = if self.transparent { Color32::TRANSPARENT } else { pal.bg };
        if self.transparent {
            let full = ctx.input(|i| i.viewport().maximized.unwrap_or(false) || i.viewport().fullscreen.unwrap_or(false));
            let r = if full { 0 } else { 12 };
            let rc = ctx.content_rect();
            ui.painter().rect_filled(rc, CornerRadius::same(r), pal.bg);
            if !full {
                ui.painter().rect_stroke(rc, CornerRadius::same(r), Stroke::new(1.0, pal.border), egui::StrokeKind::Inside);
            }
        }
        let bar = |top: i8, bottom: i8| egui::Frame::new().fill(bgfill).inner_margin(egui::Margin { left: 10, right: 10, top, bottom });

        #[cfg(feature = "selftest")]
        let shot_phone = selftest::phone();
        #[cfg(not(feature = "selftest"))]
        let shot_phone = false;
        self.phone = shot_phone
            // en la web un móvil también usa el diseño de teléfono
            || ((cfg!(target_os = "android") || crate::vfs::WEB) && {
                let r = ctx.content_rect();
                r.width().min(r.height()) < 600.0
            });
        let phone = self.phone;
        // márgenes seguros (cámara, reloj, gestos): se consultan cada medio segundo
        let now_t = ctx.input(|i| i.time);
        if now_t - self.insets_t > 0.5 {
            self.insets_t = now_t;
            let ppp = ctx.pixels_per_point();
            #[cfg(feature = "selftest")]
            let shot_insets = selftest::insets();
            #[cfg(not(feature = "selftest"))]
            let shot_insets: Option<[f32; 4]> = None;
            self.insets = if let Some(v) = shot_insets {
                v
            } else {
                crate::android_insets().map(|px| (px / ppp).round())
            };
        }
        if cfg!(target_os = "android") && now_t - self.perm_t > 1.0 {
            self.perm_t = now_t;
            let ok = crate::android_all_files();
            if ok && !self.perm_ok {
                // acaba de concederse: se vuelve a leer la carpeta
                self.refresh();
            }
            self.perm_ok = ok;
            if !ok {
                ctx.request_repaint_after(Duration::from_secs(1));
            }
        }
        if self.preview.is_some() && !self.cfg.preview_window {
            // el visor es un overlay pintado a mano: sin esto, el toque en su ✕ llega también
            // al botón que queda debajo (menú, ajustes…)
            ui.disable();
        }
        let ins = self.insets;
        let inset_frame = egui::Frame::new().fill(bgfill);
        if ins[0] > 0.5 {
            egui::Panel::left("inset_l").frame(inset_frame).exact_size(ins[0]).resizable(false).show_separator_line(false).show(ui, |_| {});
        }
        if ins[2] > 0.5 {
            egui::Panel::right("inset_r").frame(inset_frame).exact_size(ins[2]).resizable(false).show_separator_line(false).show(ui, |_| {});
        }
        if ins[1] > 0.5 {
            egui::Panel::top("inset_t").frame(inset_frame).exact_size(ins[1]).show_separator_line(false).show(ui, |_| {});
        }
        if ins[3] > 0.5 {
            egui::Panel::bottom("inset_b").frame(inset_frame).exact_size(ins[3]).show_separator_line(false).show(ui, |_| {});
        }
        if phone {
            egui::Panel::top("phonebar")
                .frame(bar(6, 4))
                .show_separator_line(false)
                .show(ui, |ui| self.phone_bar(ui));
            // filtro por nombre de la carpeta actual (icono de la barra superior)
            let pf_id = egui::Id::new("phone_filter");
            let pf_n = ui.ctx().data(|d| d.get_temp::<u8>(pf_id)).unwrap_or(0);
            if pf_n > 0 || !self.filter.is_empty() {
                egui::Panel::top("phonefilter").frame(bar(2, 4)).show_separator_line(false).show(ui, |ui| {
                    ui.horizontal(|ui| {
                        let te = egui::TextEdit::singleline(&mut self.filter)
                            // id fijo, como en la barra de direcciones: si cambia se pierde el foco y se oculta el teclado
                            .id(egui::Id::new("phone_filter_edit"))
                            .hint_text(t("Filtrar…"))
                            .desired_width((ui.available_width() - 60.0).max(80.0))
                            .margin(egui::Margin { left: 12, right: 48, top: 10, bottom: 10 });
                        let r = ui.add(te);
                        // borrar el texto sin cerrar la barra (el «×» de la derecha la cierra)
                        if !self.filter.is_empty() && self.ux_clear_button(ui, &r) {
                            self.filter.clear();
                            self.spawn_sort();
                        }
                        if pf_n > 1 {
                            if !r.has_focus() {
                                r.request_focus();
                            }
                            ui.ctx().data_mut(|d| d.insert_temp(pf_id, pf_n - 1));
                            ui.ctx().request_repaint();
                        }
                        if r.changed() {
                            self.spawn_sort();
                        }
                        let (rc, resp) = ui.allocate_exact_size(vec2(48.0, 48.0), Sense::click());
                        theme::draw_glyph(ui.painter(), rc.center(), 22.0, Glyph::Close, self.pal.text);
                        if resp.clicked() {
                            self.filter.clear();
                            self.spawn_sort();
                            ui.ctx().data_mut(|d| d.insert_temp(pf_id, 0u8));
                        }
                    });
                });
            }
        } else {
            egui::Panel::top("titlebar")
                .frame(bar(2, 0))
                .show_separator_line(false)
                .show(ui, |ui| self.title_bar(ui));

            egui::Panel::top("toolbar")
                .frame(bar(6, 6))
                .show_separator_line(false)
                .show(ui, |ui| self.toolbar(ui));
        }

        egui::Panel::bottom("status")
            .frame(egui::Frame::new().fill(bgfill).inner_margin(egui::Margin::symmetric(14, 4)))
            .exact_size(30.0)
            .show_separator_line(false)
            .show(ui, |ui| self.status_bar(ui));

        if self.cfg.clip_panel && !phone && !self.clip_hidden && self.clipboard.is_some() {
            egui::Panel::bottom("clip")
                .frame(egui::Frame::new().fill(bgfill).inner_margin(egui::Margin::symmetric(14, 4)))
                .exact_size(34.0)
                .show_separator_line(false)
                .show(ui, |ui| self.clip_bar(ui));
        }

        if self.cfg.term_open && !phone {
            let r = egui::Panel::bottom("terminal")
                .frame(egui::Frame::new().fill(bgfill).inner_margin(egui::Margin { left: 10, right: 10, top: 4, bottom: 6 }))
                .default_size(self.cfg.term_h)
                .size_range(110.0..=800.0)
                .resizable(true)
                .show_separator_line(false)
                .show(ui, |ui| self.terminal_panel(ui));
            let h = r.response.rect.height();
            if (h - self.cfg.term_h).abs() > 1.0 {
                self.cfg.term_h = h;
            }
        } else {
            self.term_focus = false;
        }

        // en escritorio, la barra lateral se oculta sola si la ventana queda muy estrecha
        if self.cfg.show_sidebar && !phone && ctx.content_rect().width() >= 760.0 {
            egui::Panel::left("sidebar")
                .frame(egui::Frame::new().fill(bgfill).inner_margin(egui::Margin { left: 10, right: 6, top: 2, bottom: 4 }))
                .default_size(210.0)
                .size_range(150.0..=380.0)
                .resizable(true)
                .show_separator_line(false)
                .show(ui, |ui| self.sidebar(ui));
        }

        // rectángulo de la vista previa lateral (lo que el panel quita a la derecha), para el teclado
        let before_side = ui.available_rect_before_wrap();
        self.side_panel(ui, &pal, bgfill, phone);
        let after_side = ui.available_rect_before_wrap();
        self.a11y.side_rect = (self.side.is_some() && after_side.right() < before_side.right() - 1.0)
            .then(|| Rect::from_min_max(pos2(after_side.right(), before_side.top()), before_side.right_bottom()));

        let card = if self.cfg.dark { pal.panel } else { pal.surface };
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(bgfill).inner_margin(if phone {
                egui::Margin { left: 6, right: 6, top: 0, bottom: 2 }
            } else {
                egui::Margin { left: 4, right: 10, top: 0, bottom: 4 }
            }))
            .show(ui, |ui| {
                if self.other.is_none() || phone {
                    egui::Frame::new()
                        .fill(card)
                        .stroke(Stroke::new(1.0, pal.border.gamma_multiply(0.6)))
                        .corner_radius(CornerRadius::same(14))
                        .inner_margin(egui::Margin::symmetric(6, 6))
                        .show(ui, |ui| {
                            let sz = ui.available_size();
                            ui.set_min_size(sz);
                            if self.preview.is_some() {
                                // La vista rápida captura la entrada: el fondo no recibe interacción.
                                ui.disable();
                            }
                            if self.cfg.col_view && !phone {
                                self.show_columns(ui);
                            } else {
                                self.show_files(ui);
                            }
                        });
                } else {
                    self.show_split(ui, card);
                }
            });
        self.flush_thumbs();

        if hovering {
            let r = ctx.content_rect().shrink(10.0);
            let p = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::new("drop_hint")));
            p.rect_filled(r, CornerRadius::same(16), pal.sel);
            p.rect_stroke(r, CornerRadius::same(16), Stroke::new(2.0, pal.accent), egui::StrokeKind::Inside);
            let txt = if drop_dst == self.cwd {
                t("Suelta para copiar aquí").to_string()
            } else {
                let n = drop_dst.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| drop_dst.display().to_string());
                tf("Suelta para copiar en «{}»", &[&n])
            };
            p.text(r.center(), Align2::CENTER_CENTER, txt, FontId::proportional(20.0), pal.accent);
        }

        if self.preview.is_some() {
            if self.cfg.preview_window {
                self.show_preview_window(&ctx);
            } else if let Some(pv) = &mut self.preview {
                match pv.show(&ctx, &pal, None) {
                    preview::Action::Close => self.preview = None,
                    preview::Action::Step(d) => self.preview_step(d),
                    preview::Action::Open => {
                        let p = pv.path.clone();
                        self.open_ext(&p);
                    }
                    preview::Action::None => {}
                }
            }
        }
        // al cerrar el visor en táctil no queda seleccionado el último archivo visto
        let pv_open = self.preview.is_some();
        if self.preview_was && !pv_open && self.touch && !self.sel_mode {
            self.clear_sel();
        }
        self.preview_was = pv_open;
        if !self.cfg.native_deco {
            if let Some(d) = self.resize_edges(&ctx) {
                self.begin_window_drag(&ctx, Some(d));
            }
        }
        self.palette_window(&ctx);
        self.confirm_dialog(&ctx);
        self.purge_dialog(&ctx);
        self.run_dialog(&ctx);
        self.host_key_dialog(&ctx);
        self.trash_poll();
        self.conflict_dialog(&ctx);
        self.ux_frame(&ctx);
        self.dialogs(&ctx);
        self.h1_frame(&ctx);
        self.op_overlay(&ctx);
        self.a11y_end_frame(&ctx);
        #[cfg(feature = "selftest")]
        self.selftest(&ctx);
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        if self.other.is_some() {
            self.ensure_live(0);
            self.cfg.split_dir = self.other.as_deref().map(|o| o.cwd.clone());
        }
        self.save_tab();
        // las pestañas virtuales no se guardan: los índices de `tabs2` y `cur_tab` cuentan sólo las guardadas
        let (tabs, tabs2, cur) = saved_tabs(&self.tabs.iter().map(|t| (t.cwd.clone(), t.split.as_ref().map(|s| s.cwd.clone()))).collect::<Vec<_>>(), self.cur_tab);
        self.cfg.tabs = tabs;
        self.cfg.tabs2 = tabs2;
        self.cfg.cur_tab = cur;
        self.cfg_save.flush();
        if let Err(e) = self.cfg.try_save() {
            eprintln!("File Browser: {}", tf("No se pudo guardar la configuración: {}", &[&e]));
        }
        // la caché de los comprimidos abiertos no sobrevive a la sesión (salvo lo que usa otra
        // aplicación, que queda para la limpieza diaria)
        for (root, _) in std::mem::take(&mut self.arch) {
            if !self.arch_ext.contains(&root) {
                let _ = crate::vfs::fs::remove_file(archive_marker(&root));
                let _ = crate::vfs::fs::remove_dir_all(&root);
            }
        }
    }

    fn clear_color(&self, _v: &egui::Visuals) -> [f32; 4] {
        if self.transparent {
            return [0.0; 4];
        }
        self.pal.bg.to_normalized_gamma_f32()
    }
}

/// Carpeta de caché donde se extrae un comprimido para navegar dentro (única por ruta, tamaño y fecha).
fn archive_cache_dir(p: &Path) -> Option<PathBuf> {
    use std::hash::{Hash, Hasher};
    let md = crate::vfs::fs::metadata(p).ok()?;
    let mut h = std::collections::hash_map::DefaultHasher::new();
    p.hash(&mut h);
    md.len().hash(&mut h);
    mtime_secs(&md).hash(&mut h);
    // una carpeta por proceso (cada ventana es uno): cerrar una ventana no borra lo que ve otra
    Some(crate::archive::cache_root()?.join(format!("{:016x}", h.finish())))
}

/// Marca de extracción completa (junto a la carpeta de caché).
fn archive_marker(root: &Path) -> PathBuf {
    let mut n = root.file_name().map(|n| n.to_os_string()).unwrap_or_default();
    n.push(".done");
    root.with_file_name(n)
}

/// Borra de la caché las extracciones de comprimidos de procesos que ya terminaron (y las antiguas
/// de más de un día) y las carpetas temporales (`inner-*`) de la vista previa que dejó un cierre brusco.
pub fn clean_archive_cache() {
    if let Some(dir) = dirs::cache_dir().map(|d| d.join("filebrowser")) {
        if let Ok(rd) = crate::vfs::fs::read_dir(&dir) {
            for e in rd.flatten() {
                let n = e.file_name().to_string_lossy().into_owned();
                // `comic-*`: páginas de CBR que dejó un cierre brusco
                let stale = (n.starts_with("inner-") || n.starts_with("comic-"))
                    && e.metadata().and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok()).map(|a| a > Duration::from_secs(3600)).unwrap_or(false);
                if stale {
                    let _ = crate::vfs::fs::remove_dir_all(e.path());
                }
            }
        }
    }
    crate::archive::clean_cache();
}

/// Nombre que no se puede usar para un archivo en ningún sistema (vacío, `.`/`..`, separadores,
/// caracteres prohibidos en Windows, punto o espacio final, más de 255 bytes).
fn bad_name(n: &str) -> bool {
    n.is_empty()
        || n == "."
        || n == ".."
        || n.len() > 255
        || n.contains(['/', '\0'])
        || n.chars().any(|c| c.is_control())
        // reglas de Windows sólo allí: en Linux y macOS «a:b» o «nota.» son nombres válidos
        || (cfg!(windows) && (n.contains(['\\', '<', '>', ':', '"', '|', '?', '*']) || n.ends_with(['.', ' '])))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Carpeta temporal propia de cada prueba; se borra al soltarla.
    struct Tmp(PathBuf);
    impl Tmp {
        fn new(tag: &str) -> Tmp {
            let n = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
            let p = std::env::temp_dir().join(format!("fb-app-test-{tag}-{}-{n}", std::process::id()));
            std::fs::create_dir_all(&p).unwrap();
            Tmp(p)
        }
        fn put(&self, name: &str, body: &str) {
            std::fs::write(self.0.join(name), body).unwrap();
        }
        fn get(&self, name: &str) -> Option<String> {
            std::fs::read_to_string(self.0.join(name)).ok()
        }
    }
    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn prune_drops_hidden_bits() {
        let mut bits = vec![0u64; 3];
        for i in [0u32, 5, 63, 64, 70, 130] {
            bits[(i / 64) as usize] |= 1 << (i % 64);
        }
        // sólo quedan los visibles (pares)
        prune_bits(&mut bits, |i| i % 2 == 0);
        let mut got = Vec::new();
        for_each_bit(&bits, |i| got.push(i));
        assert_eq!(got, vec![0, 64, 70, 130]);
        prune_bits(&mut bits, |_| false);
        assert!(bits.iter().all(|w| *w == 0));
    }

    #[test]
    fn shell_kind_by_basename() {
        assert_eq!(shell_kind("powershell.exe"), ShellKind::Power);
        assert_eq!(shell_kind(r"C:\Windows\System32\WindowsPowerShell\v1.0\PowerShell.EXE"), ShellKind::Power);
        assert_eq!(shell_kind("\"C:\\Program Files\\PowerShell\\7\\pwsh.exe\""), ShellKind::Power);
        assert_eq!(shell_kind(r"C:\Windows\system32\cmd.exe"), ShellKind::Cmd);
        assert_eq!(shell_kind("/bin/bash"), ShellKind::Posix);
        assert_eq!(shell_kind(""), ShellKind::Posix);
    }

    #[test]
    fn quote_cd_posix_hostile() {
        assert_eq!(quote_cd("/tmp/a b", ShellKind::Posix), "cd '/tmp/a b'\r");
        assert_eq!(quote_cd("/x/it's; rm -rf ~ $(id)", ShellKind::Posix), "cd '/x/it'\\''s; rm -rf ~ $(id)'\r");
        assert_eq!(quote_cd("/x/a\nrm -rf ~", ShellKind::Posix), "");
        assert_eq!(quote_cd("/x/a\rb", ShellKind::Posix), "");
    }

    #[test]
    fn quote_cd_powershell_hostile() {
        assert_eq!(quote_cd(r"C:\a b", ShellKind::Power), "Set-Location -LiteralPath 'C:\\a b'\r");
        // comilla recta y tipográfica: se doblan, así no cierran la cadena
        assert_eq!(quote_cd("C:\\it's", ShellKind::Power), "Set-Location -LiteralPath 'C:\\it''s'\r");
        assert_eq!(quote_cd("C:\\x\u{2019}; calc", ShellKind::Power), "Set-Location -LiteralPath 'C:\\x\u{2019}\u{2019}; calc'\r");
        // $() y ; dentro de comillas simples no se evalúan
        assert_eq!(quote_cd("C:\\$(calc);x", ShellKind::Power), "Set-Location -LiteralPath 'C:\\$(calc);x'\r");
        assert_eq!(quote_cd("C:\\a\u{7}", ShellKind::Power), "");
    }

    #[test]
    fn quote_cd_cmd_hostile() {
        assert_eq!(quote_cd(r"C:\a b & c", ShellKind::Cmd), "cd /d \"C:\\a b & c\"\r");
        for bad in [r"C:\%PATH%", r"C:\a!b!", "C:\\a\"&calc", r"C:\a^b", "C:\\a\r\ncalc"] {
            assert_eq!(quote_cd(bad, ShellKind::Cmd), "", "{bad}");
        }
    }

    #[test]
    fn windows_program_extensions() {
        for e in ["exe", "EXE", "bat", "cmd", "ps1", "vbs", "js", "hta", "msi", "lnk", "scr", "jar", "reg", "settingcontent-ms"] {
            assert!(win_program_ext(e), "{e}");
        }
        for e in ["txt", "png", "pdf", "json", "", "exe.txt"] {
            assert!(!win_program_ext(e), "{e}");
        }
        for c in [',', ';', '=', '&', '%', '"'] {
            assert!(CMD_BAD.contains(&c));
        }
    }

    #[test]
    fn save_debounce_waits_for_quiet() {
        let mut s = SaveDebounce::default();
        assert!(!s.pending() && !s.due(10.0) && !s.flush());
        s.mark(1.0);
        assert!(s.pending());
        assert!(!s.due(1.2));
        s.mark(1.3); // otro cambio reinicia la espera
        assert!(!s.due(1.6));
        assert!(s.due(1.8));
        assert!(!s.pending() && !s.due(5.0));
        s.mark(2.0);
        assert!(s.flush());
        assert!(!s.pending() && !s.flush());
    }

    #[test]
    fn saved_tabs_skip_virtual_and_reindex() {
        let v = PathBuf::from(LOC_TRASH);
        let a = PathBuf::from("/a");
        let b = PathBuf::from("/b");
        let tabs = vec![(a.clone(), None), (v.clone(), None), (b.clone(), Some(a.clone())), (a.clone(), Some(v.clone()))];
        let (out, split, cur) = saved_tabs(&tabs, 2);
        assert_eq!(out, vec![a.clone(), b.clone(), a.clone()]);
        // el panel dividido apunta al índice dentro de la lista guardada; uno virtual no se guarda
        assert_eq!(split, vec![(1, a.clone())]);
        assert_eq!(cur, 1);
        // la activa es virtual: queda la guardada anterior
        assert_eq!(saved_tabs(&tabs, 1).2, 0);
        assert_eq!(saved_tabs(&tabs, 3).2, 2);
    }

    #[test]
    fn free_path_picks_unused_name() {
        let d = Tmp::new("free");
        assert_eq!(free_path(&d.0.join("x.zip")), d.0.join("x.zip"));
        d.put("x.zip", "1");
        assert_eq!(free_path(&d.0.join("x.zip")), d.0.join("x (2).zip"));
        d.put("x (2).zip", "2");
        assert_eq!(free_path(&d.0.join("x.zip")), d.0.join("x (3).zip"));
        std::fs::create_dir(d.0.join("dir")).unwrap();
        assert_eq!(free_path(&d.0.join("dir")), d.0.join("dir (2)"));
    }

    #[test]
    fn rename_undo_never_overwrites() {
        let d = Tmp::new("rundo");
        d.put("a", "A");
        d.put("b", "B");
        assert!(rename_undo(&d.0.join("a"), &d.0.join("b")).is_err());
        assert_eq!(d.get("a").as_deref(), Some("A"));
        assert_eq!(d.get("b").as_deref(), Some("B"));
        assert!(rename_undo(&d.0.join("a"), &d.0.join("c")).is_ok());
        assert_eq!(d.get("c").as_deref(), Some("A"));
        assert!(d.get("a").is_none());
    }

    #[test]
    fn two_phase_swaps_and_rolls_back() {
        let d = Tmp::new("swap");
        d.put("a", "A");
        d.put("b", "B");
        let (a, b) = (d.0.join("a"), d.0.join("b"));
        rename_two_phase(&[(a.clone(), b.clone()), (b.clone(), a.clone())]).unwrap();
        assert_eq!(d.get("a").as_deref(), Some("B"));
        assert_eq!(d.get("b").as_deref(), Some("A"));
        // destino ocupado por un tercero: lo hecho se queda, lo demás vuelve y nada se pisa
        d.put("c", "C");
        let (done, _) = rename_two_phase(&[(a.clone(), d.0.join("x")), (b.clone(), d.0.join("c"))]).unwrap_err();
        assert_eq!(done, vec![0]);
        assert_eq!(d.get("x").as_deref(), Some("B"));
        assert_eq!(d.get("b").as_deref(), Some("A"));
        assert_eq!(d.get("c").as_deref(), Some("C"));
        let left = std::fs::read_dir(&d.0).unwrap().filter_map(|e| e.ok()).filter(|e| e.file_name().to_string_lossy().starts_with(".fbu-")).count();
        assert_eq!(left, 0);
    }

    #[test]
    fn batch_rename_swap_and_conflict() {
        let d = Tmp::new("batch");
        d.put("1", "one");
        d.put("2", "two");
        let p = |o: &str, n: &str| (o.to_string(), n.to_string());
        batch_rename(&d.0, &[p("1", "2"), p("2", "1")]).unwrap();
        assert_eq!(d.get("1").as_deref(), Some("two"));
        assert_eq!(d.get("2").as_deref(), Some("one"));
        // un destino que aparece antes de ejecutar: falla, nada se pisa y todo vuelve
        d.put("taken", "T");
        assert!(batch_rename(&d.0, &[p("1", "x"), p("2", "taken")]).is_err());
        assert_eq!(d.get("1").as_deref(), Some("two"));
        assert_eq!(d.get("2").as_deref(), Some("one"));
        assert_eq!(d.get("taken").as_deref(), Some("T"));
        assert!(d.get("x").is_none());
        assert_eq!(std::fs::read_dir(&d.0).unwrap().count(), 3);
    }

    #[test]
    fn moved_undo_keeps_only_replaced_targets() {
        let v = vec![(PathBuf::from("/s/a"), PathBuf::from("/d/a")), (PathBuf::from("/s/b"), PathBuf::from("/d/b"))];
        assert!(matches!(moved_undo(v.clone(), &[]), Undo::Moved(_)));
        match moved_undo(v, &[PathBuf::from("/d/b"), PathBuf::from("/d/zz")]) {
            Undo::MovedRepl(_, r) => assert_eq!(r, vec![PathBuf::from("/d/b")]),
            _ => panic!("sin reemplazados"),
        }
    }

    #[test]
    fn copy_undo_records_trashed_targets() {
        let v = vec![(PathBuf::from("/s/a"), PathBuf::from("/d/a")), (PathBuf::from("/s/b"), PathBuf::from("/d/b"))];
        assert!(matches!(copy_undo(false, v.clone(), &[]), Undo::Copied(_)));
        assert!(matches!(copy_undo(true, v.clone(), &[PathBuf::from("/d/a")]), Undo::MovedRepl(..)));
        match copy_undo(false, v, &[PathBuf::from("/d/b"), PathBuf::from("/otro")]) {
            Undo::CopiedRepl(p, r) => {
                assert_eq!(p.len(), 2);
                assert_eq!(r, vec![PathBuf::from("/d/b")]);
            }
            _ => panic!("sin reemplazados"),
        }
    }

    /// Deshacer «copiar reemplazando» con una papelera falsa (nunca la del usuario): la copia se aparta,
    /// lo anterior vuelve y sólo las copias quedan para tirar; si no vuelve, la copia recupera su sitio.
    #[test]
    fn undo_copy_replace_restores_original() {
        let d = Tmp::new("undorepl");
        std::fs::create_dir_all(d.0.join("bin")).unwrap();
        d.put("a", "copia A");
        d.put("b", "copia B");
        d.put("c", "copia C");
        std::fs::write(d.0.join("bin/a"), "original A").unwrap();
        let (a, b, c) = (d.0.join("a"), d.0.join("b"), d.0.join("c"));
        let pairs = vec![(PathBuf::from("/s/a"), a.clone()), (PathBuf::from("/s/b"), b.clone()), (PathBuf::from("/s/c"), c.clone())];
        let bin = d.0.join("bin");
        // «a» está en la papelera; «c» se reemplazó pero ya no está en ella; «b» no reemplazó nada
        let restore = |v: &[PathBuf]| -> Result<(), String> {
            for p in v {
                let from = bin.join(p.file_name().unwrap());
                if from.exists() {
                    std::fs::rename(from, p).map_err(|e| e.to_string())?;
                }
            }
            Ok(())
        };
        let (done, trash, err) = undo_copy_repl(&pairs, &[a.clone(), c.clone()], restore).unwrap();
        assert_eq!(d.get("a").as_deref(), Some("original A"));
        assert_eq!(d.get("a (2)").as_deref(), Some("copia A"));
        assert_eq!(d.get("c").as_deref(), Some("copia C"), "sin original, la copia se queda");
        assert!(d.get("c (2)").is_none());
        assert!(err.is_some_and(|e| e.contains("/c") || e.contains("\\c")));
        let mut tr = trash.clone();
        tr.sort();
        assert_eq!(tr, vec![d.0.join("a (2)"), b.clone()]);
        let mut dn: Vec<PathBuf> = done.iter().map(|(_, n)| n.clone()).collect();
        dn.sort();
        assert_eq!(dn, vec![a.clone(), b.clone()]);
        // la recuperación falla del todo: todo queda como estaba
        let d2 = Tmp::new("undorepl2");
        d2.put("x", "copia X");
        let x = d2.0.join("x");
        let r = undo_copy_repl(&[(PathBuf::from("/s/x"), x.clone())], &[x.clone()], |_| Err("sin papelera".into()));
        assert_eq!(r.unwrap_err(), "sin papelera");
        assert_eq!(d2.get("x").as_deref(), Some("copia X"));
        assert_eq!(std::fs::read_dir(&d2.0).unwrap().count(), 1);
    }

    /// «Aplicar» en Propiedades (lo que ahora corre en un hilo): permisos recursivos, fecha y nombre;
    /// se para en el primer error y se puede cancelar.
    #[test]
    fn props_work_applies_and_cancels() {
        let d = Tmp::new("props");
        std::fs::create_dir_all(d.0.join("dir/sub")).unwrap();
        d.put("dir/f.txt", "x");
        d.put("dir/sub/g.txt", "y");
        d.put("otro", "z");
        let w = tools::Work::default();
        let dir = d.0.join("dir");
        let r = props_work(std::slice::from_ref(&dir), Some(0o700), true, Some(1_000_000_000), Some(("nuevo".into(), "dir".into())), &w);
        assert!(r.err.is_none(), "{:?}", r.err);
        assert_eq!(r.perm, Some(0o700));
        assert_eq!(r.secs, Some(1_000_000_000));
        let nd = d.0.join("nuevo");
        assert_eq!(r.renamed, Some((nd.clone(), "nuevo".to_string())));
        assert!(nd.join("sub/g.txt").exists() && !dir.exists());
        assert!(w.done.load(Ordering::Relaxed) >= 4, "cuenta lo recorrido");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let m = std::fs::metadata(nd.join("sub/g.txt")).unwrap().permissions().mode() & 0o777;
            assert_eq!(m & 0o066, 0, "lectura/escritura de grupo y otros quitadas: {m:o}");
        }
        // nombre ocupado: error, nada se renombra
        let r = props_work(std::slice::from_ref(&nd), None, false, None, Some(("otro".into(), "nuevo".into())), &w);
        assert!(r.err.is_some() && r.renamed.is_none());
        assert!(nd.exists());
        // cancelado antes de empezar: no se aplica nada
        let w = tools::Work::default();
        w.cancel.store(true, Ordering::Relaxed);
        let r = props_work(std::slice::from_ref(&nd), Some(0o755), true, Some(5), None, &w);
        assert_eq!(r.err.as_deref(), Some(t("Operación cancelada")));
        assert!(r.perm.is_none() && r.secs.is_none());
    }
}
