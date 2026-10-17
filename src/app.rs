#[cfg(target_os = "android")]
use crate::trash;
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
use std::time::{Duration, Instant, SystemTime};

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
}

impl TabState {
    fn new(cwd: PathBuf) -> TabState {
        TabState { cwd, back: vec![], fwd: vec![], filter: String::new(), sel: vec![], split: None, side: 0, vmode: VMode::Dir }
    }
}

struct PropsDlg {
    paths: Vec<PathBuf>,
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
    readonly: bool,
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
    /// Plantilla del nombre (vacía = se parte del nombre original): {nombre} {n} {n:3} {fecha} {padre}.
    template: String,
    regex: bool,
    mtimes: Vec<i64>,
    parent: String,
}

/// Acción reversible registrada para deshacer/rehacer.
#[derive(Clone)]
enum Undo {
    Rename { from: PathBuf, to: PathBuf },
    /// (origen, nuevo) de cada elemento movido o renombrado en lote.
    Moved(Vec<(PathBuf, PathBuf)>),
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
    Trash { replay: bool },
    Generated,
    /// Extracción a la caché para navegar dentro de un comprimido.
    OpenArchive { archive: PathBuf, root: PathBuf },
    /// Deshacer asíncrono: al terminar, el elemento pasa a la pila de rehacer.
    Replay(Undo),
    Other,
}

struct ConflictDlg {
    srcs: Vec<PathBuf>,
    dst: PathBuf,
    mv: bool,
    names: Vec<String>,
}

struct OpenWithDlg {
    path: PathBuf,
    apps: Vec<extras::App>,
    cmd: String,
    /// Solo el campo de comando (las aplicaciones están en el submenú «Abrir con»).
    custom_only: bool,
}

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
    /// Lista completa de errores de una operación con varios fallos.
    Errors(Vec<String>),
    /// Programa ejecutable (ELF o script con shebang) al que se ha hecho doble clic.
    Options,
    About,
    Keys,
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

/// Estado de listado de un panel. El panel activo vive en los campos de `App`;
/// el inactivo (vista dividida) se guarda aquí y se intercambia al dibujarlo.
struct PaneState {
    cwd: PathBuf,
    back: Vec<PathBuf>,
    fwd: Vec<PathBuf>,
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
    traced_first: bool,
    hover_name: Option<String>,
}

impl PaneState {
    fn new(cwd: PathBuf) -> PaneState {
        let (load_tx, load_rx) = channel();
        let (sort_tx, sort_rx) = channel();
        PaneState {
            cwd,
            back: vec![],
            fwd: vec![],
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
            traced_first: false,
            hover_name: None,
        }
    }
}

pub struct App {
    traced_ui: bool,
    was_focused: bool,
    cfg: Config,
    pal: Pal,
    ctx: egui::Context,

    cwd: PathBuf,
    back: Vec<PathBuf>,
    fwd: Vec<PathBuf>,

    generation: u64,
    gen_shared: Arc<AtomicU64>,
    load_tx: Sender<LoadMsg>,
    load_rx: Receiver<LoadMsg>,
    loading: bool,
    sort_pending: bool,
    /// Entrada táctil: arrastrar desplaza, un toque abre y la pulsación larga abre el menú.
    touch: bool,
    /// Posición (en `order`) del elemento bajo el dedo al empezar el toque.
    touch_press: Option<usize>,
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
    /// Comprimidos abiertos como carpeta: (carpeta de caché, archivo original).
    arch: Vec<(PathBuf, PathBuf)>,
    /// El usuario cerró el panel del portapapeles (reaparece al copiar de nuevo).
    clip_hidden: bool,
    /// Listados de las columnas estáticas de la vista de columnas: ruta -> (instante, filas).
    col_cache: HashMap<PathBuf, (Instant, Arc<Vec<(String, bool)>>)>,
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
    kin_hist: Vec<(f64, f32)>,
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
    places: Vec<Place>,
    drives: Vec<Place>,
    network: Vec<NetPlace>,
    net_check: Instant,
    net_nav: Arc<std::sync::Mutex<Option<PathBuf>>>,
    net_err: Arc<std::sync::Mutex<Option<String>>>,
    net_busy: Arc<AtomicBool>,
    /// Lista de red calculada en segundo plano (consulta procesos y puede tardar).
    net_list: Arc<std::sync::Mutex<Option<Vec<NetPlace>>>>,
    net_refreshing: Arc<AtomicBool>,
    net_shares: Arc<std::sync::Mutex<Option<NetShares>>>,
    /// Trabajos de E/S lentos (crear, renombrar…) hechos en un hilo; cada uno deja aquí lo que debe
    /// ejecutarse en el hilo de la interfaz al terminar.
    io_done: Arc<std::sync::Mutex<Vec<Box<dyn FnOnce(&mut App) + Send>>>>,
    /// Vigilancia de cambios del directorio, consultada en un hilo (un `stat` en red puede bloquear).
    watch_res: Arc<std::sync::Mutex<Vec<(PathBuf, Option<std::time::SystemTime>)>>>,
    watch_busy: Arc<AtomicBool>,
    net_users: HashMap<String, String>,
    side_drop: Option<PathBuf>,
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
    /// Diálogo «hay tareas en curso» al intentar cerrar; `quit_ok` = ya confirmado.
    quit_ask: bool,
    /// Filas de la papelera mostradas (en el mismo orden que `entries` en la vista Papelera).
    trash_rows: Arc<std::sync::Mutex<Vec<extras::TrashRow>>>,
    trash_job: Option<std::sync::mpsc::Receiver<Result<(bool, usize), String>>>,
    confirm_purge: Option<PurgeReq>,
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
    op: Option<Arc<Progress>>,
    msg: Option<(String, Instant, bool)>,
    confirm_delete: Option<Vec<PathBuf>>,
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
    start: Option<Instant>,
    t_load: Instant,
    traced_first: bool,
    shot_req: bool,
    shot_prev: bool,
    shot_dbl: u8,

    /// Panel inactivo de la vista dividida (None = un solo panel).
    other: Option<Box<PaneState>>,
    /// Lado (0 izquierda / 1 derecha) del panel con foco y del que ocupa ahora los campos de `App`.
    active_side: u8,
    live_side: u8,
    pane_rects: [Rect; 2],

    palette: Option<PaletteState>,
    /// Comando cuyo atajo se está reasignando en Opciones.
    rebinding: Option<Cmd>,
}

fn standard_places() -> Vec<Place> {
    let mut v = Vec::new();
    let mut add = |name: &str, p: Option<PathBuf>, g: Glyph, sys: &'static str| {
        if let Some(p) = p {
            if p.is_dir() {
                v.push(Place { name: name.to_string(), path: p, glyph: g, sys });
            }
        }
    };
    add(t("Inicio"), dirs::home_dir(), Glyph::Home, "home");
    #[cfg(target_os = "android")]
    {
        // Android no define los directorios XDG: usamos los habituales del almacenamiento
        let h = PathBuf::from("/storage/emulated/0");
        add(t("Descargas"), Some(h.join("Download")), Glyph::Download, "downloads");
        add(t("Imágenes"), Some(h.join("Pictures")), Glyph::Picture, "pictures");
        add("DCIM", Some(h.join("DCIM")), Glyph::Picture, "pictures");
        add(t("Documentos"), Some(h.join("Documents")), Glyph::Document, "documents");
        add(t("Música"), Some(h.join("Music")), Glyph::Music, "music");
        add(t("Vídeos"), Some(h.join("Movies")), Glyph::Video, "videos");
    }
    add(t("Escritorio"), dirs::desktop_dir(), Glyph::Desktop, "desktop");
    add(t("Documentos"), dirs::document_dir(), Glyph::Document, "documents");
    add(t("Descargas"), dirs::download_dir(), Glyph::Download, "downloads");
    add(t("Imágenes"), dirs::picture_dir(), Glyph::Picture, "pictures");
    add(t("Música"), dirs::audio_dir(), Glyph::Music, "music");
    add(t("Vídeos"), dirs::video_dir(), Glyph::Video, "videos");
    v
}

/// Sitio de red conocido: ya montado (`path`) o solo conocido por su dirección (`uri`).
struct NetPlace {
    name: String,
    path: Option<PathBuf>,
    uri: Option<String>,
}

#[cfg(target_os = "linux")]
fn gvfs_dir() -> Option<PathBuf> {
    std::env::var_os("XDG_RUNTIME_DIR").map(|d| PathBuf::from(d).join("gvfs")).filter(|p| p.is_dir())
}

/// Nombre legible de un montaje gvfs («smb-share:server=nas,share=media» -> «media en nas»).
#[cfg(target_os = "linux")]
fn gvfs_label(dir_name: &str) -> String {
    let (scheme, rest) = dir_name.split_once(':').unwrap_or((dir_name, ""));
    let get = |k: &str| rest.split(',').find_map(|kv| kv.strip_prefix(&format!("{k}=")).map(|v| v.to_string()));
    match scheme {
        "smb-share" => match (get("share"), get("server")) {
            (Some(sh), Some(sv)) => tf("{} en {}", &[&sh, &sv]),
            _ => dir_name.to_string(),
        },
        "sftp" | "ftp" | "dav" | "davs" | "nfs" | "mtp" | "gphoto2" => {
            format!("{} ({})", get("host").unwrap_or_else(|| rest.to_string()).replace("%20", " "), scheme.to_uppercase())
        }
        _ => dir_name.to_string(),
    }
}

#[cfg(target_os = "linux")]
fn pct_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(if b[i + 1..i + 3].iter().all(u8::is_ascii_hexdigit) { std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or("zz") } else { "zz" }, 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Dirección (smb://servidor/recurso) -> directorio gvfs correspondiente, si está montado.
/// Tolera alias de esquema (ssh/sftp, davs/dav, ftps/ftp), mayúsculas, usuario@ y %20.
#[cfg(target_os = "linux")]
fn uri_to_gvfs(uri: &str) -> Option<PathBuf> {
    let base = gvfs_dir()?;
    let (scheme, rest) = uri.split_once("://")?;
    let scheme = scheme.to_lowercase();
    let want = match scheme.as_str() {
        "smb" => "smb-share",
        "ssh" | "sftp" => "sftp",
        "dav" | "davs" => "dav",
        "ftp" | "ftps" => "ftp",
        other => other,
    }
    .to_string();
    let host = rest.split('/').next()?.rsplit('@').next()?;
    let host = host.rsplit_once(':').filter(|(_, p)| p.chars().all(|c| c.is_ascii_digit())).map(|(h, _)| h).unwrap_or(host);
    let host = pct_decode(host).to_lowercase();
    let share = pct_decode(rest.split('/').nth(1).unwrap_or("")).to_lowercase();
    for e in std::fs::read_dir(base).ok()?.flatten() {
        let n = e.file_name().to_string_lossy().into_owned();
        let Some((sch, props)) = n.split_once(':') else { continue };
        if sch.to_lowercase() != want {
            continue;
        }
        let get = |k: &str| props.split(',').find_map(|kv| kv.strip_prefix(&format!("{k}=")).map(|v| pct_decode(v).to_lowercase()));
        let hk = if want == "smb-share" { "server" } else { "host" };
        if get(hk).map(|h| h == host).unwrap_or(false) && (share.is_empty() || want != "smb-share" || get("share").map(|v| v == share).unwrap_or(false)) {
            return Some(e.path());
        }
    }
    None
}

/// Ruta local de una dirección ya montada según gvfs (`gio info`), aunque no aparezca por nombre en
/// el directorio gvfs. No monta nada; con límite de tiempo por si el servidor no responde.
#[cfg(target_os = "linux")]
fn gio_local_path(uri: &str) -> Option<PathBuf> {
    let o = std::process::Command::new("timeout")
        .args(["4", "gio", "info", "-a", "standard::name", uri])
        .env("LC_ALL", "C")
        .stdin(std::process::Stdio::null())
        .output()
        .ok()?;
    if !o.status.success() {
        return None;
    }
    String::from_utf8_lossy(&o.stdout)
        .lines()
        .find_map(|l| l.trim().strip_prefix("local path:").map(|p| PathBuf::from(p.trim())))
        .filter(|p| p.exists())
}

/// Texto en minúsculas de `gio mount -l` (montajes conocidos por gvfs), para saber si merece consultar.
#[cfg(target_os = "linux")]
fn gio_mounts_text() -> String {
    std::process::Command::new("timeout")
        .args(["4", "gio", "mount", "-l"])
        .env("LC_ALL", "C")
        .stdin(std::process::Stdio::null())
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).to_lowercase())
        .unwrap_or_default()
}

/// Tema del sistema en Linux (winit no lo informa en Wayland): portal XDG, gsettings, KDE y GTK.
#[cfg(target_os = "linux")]
fn detect_system_dark() -> Option<bool> {
    use std::process::Command;
    let run = |prog: &str, args: &[&str]| -> Option<String> {
        let o = Command::new(prog).args(args).output().ok()?;
        o.status.success().then(|| String::from_utf8_lossy(&o.stdout).trim().to_string())
    };
    // portal: (<<uint32 1>>,) 1 = oscuro, 2 = claro, 0 = sin preferencia
    if let Some(o) = run(
        "gdbus",
        &["call", "--session", "--dest", "org.freedesktop.portal.Desktop", "--object-path", "/org/freedesktop/portal/desktop",
          "--method", "org.freedesktop.portal.Settings.Read", "org.freedesktop.appearance", "color-scheme"],
    ) {
        if let Some(n) = o.rsplit("uint32").next().and_then(|r| r.trim().chars().find(|c| c.is_ascii_digit())) {
            match n {
                '1' => return Some(true),
                '2' => return Some(false),
                _ => {}
            }
        }
    }
    if let Some(o) = run("gsettings", &["get", "org.gnome.desktop.interface", "color-scheme"]) {
        if o.contains("prefer-dark") {
            return Some(true);
        }
        if o.contains("prefer-light") {
            return Some(false);
        }
    }
    let home = dirs::home_dir()?;
    let cfg = dirs::config_dir().unwrap_or_else(|| home.join(".config"));
    if let Ok(txt) = std::fs::read_to_string(cfg.join("kdeglobals")) {
        if let Some(l) = txt.lines().find(|l| l.starts_with("ColorScheme=")) {
            return Some(l.to_lowercase().contains("dark"));
        }
    }
    if let Ok(txt) = std::fs::read_to_string(cfg.join("gtk-3.0/settings.ini")) {
        for l in txt.lines() {
            let l = l.trim();
            if let Some(v) = l.strip_prefix("gtk-application-prefer-dark-theme=") {
                if v.trim() == "1" || v.trim() == "true" {
                    return Some(true);
                }
            }
            if let Some(v) = l.strip_prefix("gtk-theme-name=") {
                return Some(v.to_lowercase().contains("dark"));
            }
        }
    }
    if let Ok(v) = std::env::var("GTK_THEME") {
        return Some(v.to_lowercase().contains("dark"));
    }
    None
}

#[cfg(target_os = "linux")]
struct GioCred {
    user: String,
    domain: String,
    pass: String,
    anon: bool,
}

/// Ejecuta `gio <args> <uri>` respondiendo a sus preguntas de usuario, dominio y contraseña.
#[cfg(target_os = "linux")]
fn run_gio(args: &[&str], uri: &str, c: &GioCred) -> Result<(), String> {
    use std::io::{Read, Write};
    use std::process::{Command, Stdio};
    let mut cmd = Command::new("gio");
    cmd.args(args);
    // preguntas en inglés: se reconocen por texto y, en otro idioma, gio se quedaba esperando
    cmd.env("LC_ALL", "C").env("LANGUAGE", "C");
    if c.anon {
        cmd.arg("-a");
    }
    cmd.arg(uri).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn().map_err(|e| format!("gio: {e}"))?;
    let (tx, rx) = std::sync::mpsc::channel::<Vec<u8>>();
    let outs: [Option<Box<dyn Read + Send>>; 2] = [
        child.stdout.take().map(|o| Box::new(o) as Box<dyn Read + Send>),
        child.stderr.take().map(|o| Box::new(o) as Box<dyn Read + Send>),
    ];
    for mut src in outs.into_iter().flatten() {
        let tx = tx.clone();
        std::thread::spawn(move || {
            let mut b = [0u8; 512];
            while let Ok(n) = src.read(&mut b) {
                if n == 0 || tx.send(b[..n].to_vec()).is_err() {
                    break;
                }
            }
        });
    }
    drop(tx);
    let mut stdin = child.stdin.take();
    let (mut buf, mut log) = (String::new(), String::new());
    let t0 = Instant::now();
    loop {
        match rx.recv_timeout(Duration::from_millis(250)) {
            Ok(chunk) => {
                let txt = String::from_utf8_lossy(&chunk).into_owned();
                buf.push_str(&txt);
                log.push_str(&txt);
                let last = buf.rsplit('\n').next().unwrap_or("").trim_end().to_lowercase();
                if last.ends_with(':') || last.ends_with("]:") {
                    let ans = if last.contains("anonymous") || last.contains("anónim") {
                        Some("n")
                    } else if last.contains("password") || last.contains("passphrase") || last.contains("contraseña") {
                        Some(c.pass.as_str())
                    } else if last.contains("domain") || last.contains("dominio") {
                        Some(c.domain.as_str())
                    } else if last.contains("user") || last.contains("usuario") {
                        Some(c.user.as_str())
                    } else {
                        None
                    };
                    if let (Some(a), Some(w)) = (ans, stdin.as_mut()) {
                        let _ = writeln!(w, "{a}");
                        let _ = w.flush();
                        buf.push('\n');
                    }
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                if t0.elapsed() > Duration::from_secs(90) {
                    let _ = child.kill();
                    return Err(t("Tiempo de espera agotado").to_string());
                }
            }
            Err(_) => break,
        }
    }
    drop(stdin);
    match child.wait() {
        Ok(st) if st.success() => Ok(()),
        _ => Err(log.lines().rev().map(|l| l.trim()).find(|l| !l.is_empty() && !l.to_lowercase().ends_with(':')).unwrap_or("gio").to_string()),
    }
}

fn list_network() -> Vec<NetPlace> {
    let mut v: Vec<NetPlace> = Vec::new();
    #[cfg(target_os = "linux")]
    {
        // montajes de red del sistema (cifs, nfs, sshfs…)
        if let Ok(txt) = std::fs::read_to_string("/proc/self/mounts") {
            for l in txt.lines() {
                let mut it = l.split_whitespace();
                let (Some(dev), Some(mp), Some(fs)) = (it.next(), it.next(), it.next()) else { continue };
                if matches!(fs, "cifs" | "smb3" | "smbfs" | "nfs" | "nfs4" | "fuse.sshfs" | "davfs" | "fuse.davfs" | "ceph" | "afs") {
                    let mp = mp.replace("\\040", " ");
                    v.push(NetPlace { name: dev.trim_start_matches('/').to_string(), path: Some(PathBuf::from(mp)), uri: None });
                }
            }
        }
        // montajes gvfs (smb, sftp, ftp, dav, mtp…)
        let mut mounted: Vec<PathBuf> = Vec::new();
        let gio_text = gio_mounts_text();
        if let Some(g) = gvfs_dir() {
            if let Ok(rd) = std::fs::read_dir(&g) {
                for e in rd.flatten() {
                    let n = e.file_name().to_string_lossy().into_owned();
                    mounted.push(e.path());
                    v.push(NetPlace { name: gvfs_label(&n), path: Some(e.path()), uri: None });
                }
            }
        }
        // cada servidor SMB con recursos montados sigue disponible para conectar otros
        let mut servers: Vec<String> = Vec::new();
        for p in &mounted {
            let n = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            if let Some(props) = n.strip_prefix("smb-share:") {
                if let Some(h) = props.split(',').find_map(|kv| kv.strip_prefix("server=")) {
                    if !servers.iter().any(|x| x.eq_ignore_ascii_case(h)) {
                        servers.push(h.to_string());
                    }
                }
            }
        }
        for h in &servers {
            v.push(NetPlace { name: pct_decode(h).to_uppercase(), path: None, uri: Some(format!("smb://{h}/")) });
        }
        // sitios conocidos: marcadores GTK con direcciones de red
        if let Some(h) = dirs::home_dir() {
            if let Ok(txt) = std::fs::read_to_string(h.join(".config/gtk-3.0/bookmarks")) {
                for l in txt.lines() {
                    let (uri, label) = l.split_once(' ').map(|(a, b)| (a, Some(b))).unwrap_or((l, None));
                    let Some((scheme, _)) = uri.split_once("://") else { continue };
                    if !matches!(scheme, "smb" | "sftp" | "ftp" | "ftps" | "dav" | "davs" | "nfs" | "ssh") {
                        continue;
                    }
                    let host = uri.split_once("://").and_then(|(_, r)| r.split('/').next()).and_then(|a| a.rsplit('@').next()).map(|h| pct_decode(h.split(':').next().unwrap_or(h)).to_lowercase()).unwrap_or_default();
                    let server_only = scheme == "smb" && uri.split_once("://").map(|(_, r)| r.trim_end_matches('/').split('/').nth(1).is_none()).unwrap_or(false);
                    // un servidor sin recurso sigue ofreciéndose aunque tenga recursos montados: sirve para añadir más
                    let path = if server_only { None } else { uri_to_gvfs(uri).or_else(|| if !host.is_empty() && gio_text.contains(&host) { gio_local_path(uri) } else { None }) };
                    if path.as_ref().map(|p| mounted.contains(p)).unwrap_or(false) {
                        continue; // ya listado como montado
                    }
                    if server_only && servers.iter().any(|x| x.eq_ignore_ascii_case(&host)) {
                        v.retain(|p| p.path.is_some() || p.uri.as_deref().map(|u| !u.to_lowercase().contains(&format!("//{host}/"))).unwrap_or(true));
                    }
                    let name = label.map(|s| s.to_string()).unwrap_or_else(|| uri.to_string());
                    v.push(NetPlace { name, path, uri: Some(uri.to_string()) });
                }
            }
        }
    }
    #[cfg(windows)]
    {
        if let Some(a) = dirs::config_dir() {
            // «Entorno de red»: accesos a recursos compartidos que el usuario ha abierto
            let d = a.join("Microsoft").join("Windows").join("Network Shortcuts");
            if let Ok(rd) = std::fs::read_dir(d) {
                for e in rd.flatten() {
                    v.push(NetPlace { name: e.file_name().to_string_lossy().into_owned(), path: Some(e.path()), uri: None });
                }
            }
        }
    }
    v
}

fn list_drives() -> Vec<Place> {
    let mut v = Vec::new();
    #[cfg(windows)]
    for c in b'A'..=b'Z' {
        let p = PathBuf::from(format!("{}:\\", c as char));
        if p.exists() {
            v.push(Place { name: tf("Disco ({}:)", &[&(c as char)]), path: p, glyph: Glyph::Drive, sys: "drive" });
        }
    }
    #[cfg(target_os = "linux")]
    {
        v.push(Place { name: t("Sistema").into(), path: PathBuf::from("/"), glyph: Glyph::Drive, sys: "drive" });
        if let Ok(txt) = std::fs::read_to_string("/proc/self/mounts") {
            for l in txt.lines() {
                let mut it = l.split_whitespace();
                let (Some(dev), Some(mp)) = (it.next(), it.next()) else { continue };
                let mp = mp.replace("\\040", " ");
                if dev.starts_with("/dev/")
                    && (mp.starts_with("/media") || mp.starts_with("/run/media") || mp.starts_with("/mnt"))
                {
                    let name = Path::new(&mp).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or(mp.clone());
                    v.push(Place { name, path: PathBuf::from(mp), glyph: Glyph::Drive, sys: "drive" });
                }
            }
        }
    }
    #[cfg(target_os = "macos")]
    {
        v.push(Place { name: "Macintosh HD".into(), path: PathBuf::from("/"), glyph: Glyph::Drive, sys: "drive" });
        if let Ok(rd) = std::fs::read_dir("/Volumes") {
            for e in rd.flatten() {
                let p = e.path();
                if p != Path::new("/Volumes/Macintosh HD") {
                    v.push(Place { name: e.file_name().to_string_lossy().into_owned(), path: p, glyph: Glyph::Drive, sys: "drive" });
                }
            }
        }
    }
    v
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, cfg: Config) -> App {
        let ctx = cc.egui_ctx.clone();
        let li = crate::i18n::resolve(&cfg.lang);
        crate::i18n::set(li);
        let font_ok = crate::i18n::install_fonts(&ctx, li);
        if !font_ok {
            eprintln!("aviso: no se encontró una fuente CJK del sistema");
        }
        let mut cfg = cfg;
        if cfg!(target_os = "android") {
            cfg.preview_window = false;
        }
        // Android: sin botones de ventana propios (minimizar/maximizar/cerrar no existen)
        if cfg!(target_os = "android") {
            cfg.native_deco = true;
        }
        // en pantallas estrechas (móvil) la barra lateral empieza oculta
        if cfg!(target_os = "android") && ctx.content_rect().width() < 620.0 {
            cfg.show_sidebar = false;
        }
        let sys_theme = Arc::new(std::sync::atomic::AtomicU8::new(0));
        #[cfg(target_os = "linux")]
        {
            // la detección lanza procesos (gdbus, gsettings): fuera del arranque; mientras tanto se
            // usa el último tema guardado y, si el sistema ha cambiado, se ajusta con la animación
            let (st, c) = (sys_theme.clone(), ctx.clone());
            std::thread::spawn(move || loop {
                let v = detect_system_dark().map(|d| if d { 2 } else { 1 }).unwrap_or(0);
                if st.swap(v, Ordering::Relaxed) != v {
                    c.request_repaint();
                }
                std::thread::sleep(Duration::from_secs(3));
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
        let pal = theme::resolve(&cfg, cfg.dark);
        theme::apply_style(&ctx, &pal, cfg.dark);
        let (load_tx, load_rx) = channel();
        let (sort_tx, sort_rx) = channel();
        // Rutas de la línea de órdenes: carpetas → pestañas; archivos → su carpeta con el archivo seleccionado.
        let mut cli_dirs: Vec<PathBuf> = Vec::new();
        let mut cli_select: Vec<String> = Vec::new();
        for p in system::initial_targets() {
            if p.is_dir() {
                if !cli_dirs.contains(&p) {
                    cli_dirs.push(p);
                }
            } else if let (Some(parent), Some(name)) = (p.parent(), p.file_name()) {
                if p.exists() && parent.is_dir() {
                    if cli_dirs.is_empty() {
                        cli_select.push(name.to_string_lossy().into_owned());
                    }
                    let parent = parent.to_path_buf();
                    if !cli_dirs.contains(&parent) {
                        cli_dirs.push(parent);
                    }
                }
            }
        }
        let cli_dir = cli_dirs.first().cloned();
        let mut start = cli_dir
            .clone()
            .or_else(|| cfg.last_dir.clone())
            .filter(|p| p.is_dir())
            .or_else(dirs::home_dir)
            .unwrap_or_else(|| PathBuf::from("/"));
        let mut tabs_init: Vec<TabState> = Vec::new();
        let mut cur_init = 0;
        if cli_dirs.len() > 1 {
            tabs_init = cli_dirs.iter().map(|p| TabState::new(p.clone())).collect();
        }
        if cli_dir.is_none() && cfg.restore_tabs && (cfg.tabs.len() > 1 || !cfg.tabs2.is_empty()) {
            for (n, p) in cfg.tabs.iter().enumerate() {
                if !p.is_dir() {
                    continue;
                }
                let mut t = TabState::new(p.clone());
                if let Some((_, p2)) = cfg.tabs2.iter().find(|(i, p2)| *i == n && p2.is_dir()) {
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
            traced_ui: false,
            was_focused: true,
            transparent: !cfg.native_deco,
            thumbs: Thumbs::new(&ctx),
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
            arch: Vec::new(),
            clip_hidden: false,
            col_cache: HashMap::new(),
            col_active: false,
            opts_tab: 0,
            edit_theme: String::new(),
            perm_t: -10.0,
            insets: [0.0; 4],
            insets_t: -10.0,
            sel_mode: false,
            kin_off: 0.0,
            kin_vel: 0.0,
            kin_hist: Vec::new(),
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
            places: standard_places(),
            drives: list_drives(),
            network: Vec::new(),
            // vencido: la primera pasada de la lista de red se hace en segundo plano al arrancar
            net_check: Instant::now().checked_sub(Duration::from_secs(10)).unwrap_or_else(Instant::now),
            net_nav: Arc::new(std::sync::Mutex::new(None)),
            net_err: Arc::new(std::sync::Mutex::new(None)),
            net_busy: Arc::new(AtomicBool::new(false)),
            net_list: Arc::new(std::sync::Mutex::new(None)),
            net_refreshing: Arc::new(AtomicBool::new(false)),
            net_shares: Arc::new(std::sync::Mutex::new(None)),
            io_done: Arc::new(std::sync::Mutex::new(Vec::new())),
            watch_res: Arc::new(std::sync::Mutex::new(Vec::new())),
            watch_busy: Arc::new(AtomicBool::new(false)),
            net_users: HashMap::new(),
            side_drop: None,
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
            quit_ask: false,
            trash_rows: Arc::new(std::sync::Mutex::new(Vec::new())),
            trash_job: None,
            confirm_purge: None,
            quit_ok: false,
            op_desc: String::new(),
            theme_anim: None,
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            conflict: None,
            closed_tabs: Vec::new(),
            rename: None,
            clipboard: None,
            op: None,
            msg: None,
            confirm_delete: None,
            pending_select: cli_select,
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
            menu_id: None,
            gear_id: None,
            dlg: Dlg::None,
            hover_name: None,
            last_title: String::new(),
            start: None,
            t_load: Instant::now(),
            traced_first: false,
            shot_req: false,
            shot_prev: false,
            shot_dbl: 0,
            other: None,
            active_side: 0,
            live_side: 0,
            pane_rects: [Rect::NOTHING; 2],
            palette: None,
            rebinding: None,
        };
        std::thread::spawn(clean_archive_cache);
        if std::env::var_os("FB_COLS").is_some() {
            app.cfg.col_view = true;
        }
        if std::env::var("FB_LIST").is_ok() {
            app.cfg.grid = false;
        }
        #[cfg(target_os = "macos")]
        system::hook_open_files(app.inbox.clone(), move || ctx_for_mac.request_repaint());
        app.load(start, false);
        let split_dir = if std::env::var_os("FB_SPLIT").is_some() {
            Some(std::env::var_os("FB_SPLIT").map(PathBuf::from).filter(|p| p.is_dir()).unwrap_or_else(|| app.cwd.clone()))
        } else if app.cfg.split && !app.cfg.col_view {
            Some(app.cfg.split_dir.clone().filter(|p| p.is_dir()).unwrap_or_else(|| app.cwd.clone()))
        } else {
            None
        };
        if let Some(d) = split_dir {
            app.open_split(d);
        }
        if let Ok(f) = std::env::var("FB_FILTER") {
            app.filter = f;
        }
        app
    }

    // -----------------------------------------------------------------------
    // Vista dividida
    // -----------------------------------------------------------------------

    /// Intercambia los campos de listado de `App` con el panel inactivo.
    fn swap_pane(&mut self) {
        let Some(o) = self.other.as_deref_mut() else { return };
        macro_rules! sw {
            ($($f:ident),* $(,)?) => { $( std::mem::swap(&mut self.$f, &mut o.$f); )* };
        }
        sw!(
            cwd, back, fwd, generation, gen_shared, load_tx, load_rx, loading, load_err, dir_hash, dir_mtime,
            last_check, entries, order, sort_seq, sort_tx, sort_rx, filter, filter_focus, sel, sel_count, sel_size,
            anchor, cursor, band, drag_items, scroll_to, cols, page_rows, rename, pending_select, pending_rename,
            typeahead, vmode, search_st, search_taken, sbuf, t_load, traced_first, hover_name
        );
        self.live_side = 1 - self.live_side;
    }

    fn ensure_live(&mut self, side: u8) {
        if self.other.is_some() && self.live_side != side {
            self.swap_pane();
        }
    }

    fn open_split(&mut self, dir: PathBuf) {
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
    fn set_view(&mut self, m: u8) {
        self.cfg.grid = m == 1;
        self.cfg.col_view = m == 2;
        if m == 2 && self.other.is_some() {
            self.toggle_split();
        }
        self.cfg.save();
    }

    fn toggle_split(&mut self) {
        if self.cfg.col_view && self.other.is_none() {
            self.say(t("La vista de columnas no admite vista dividida").into(), true);
            return;
        }
        if self.other.is_some() {
            // el panel con foco pasa a ser el único
            self.ensure_live(self.active_side);
            self.other = None;
            self.active_side = 0;
            self.live_side = 0;
            self.cfg.split = false;
        } else {
            self.open_split(self.cwd.clone());
            self.cfg.split = true;
        }
        self.preview = None;
        self.cfg.save();
    }

    fn focus_side(&mut self, side: u8) {
        if self.other.is_none() || side == self.active_side {
            return;
        }
        self.active_side = side;
        self.ensure_live(side);
        self.addr_edit = None;
        self.preview = None;
        self.cfg.last_dir = Some(self.cwd.clone());
    }

    fn to_other_pane(&mut self, mv: bool) {
        let Some(o) = self.other.as_deref() else { return };
        let dst = o.cwd.clone();
        let srcs = self.selected_paths();
        if !srcs.is_empty() {
            self.start_copy(srcs, dst, mv);
        }
    }

    fn show_split(&mut self, ui: &mut egui::Ui, card: Color32) {
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
            self.cfg.save();
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

    fn load(&mut self, path: PathBuf, keep_filter: bool) {
        // recargar la misma carpeta conserva el modo (recursivo / búsqueda); navegar vuelve a carpeta normal
        let mode = match virt_mode(&path) {
            Some(m) => m,
            None if path == self.cwd => self.vmode.clone(),
            None => VMode::Dir,
        };
        self.load_mode(path, keep_filter, mode);
    }

    fn load_mode(&mut self, path: PathBuf, keep_filter: bool, mode: VMode) {
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
        self.traced_first = false;
        self.load_err = None;
        self.dir_hash = hash_str(0, &path.to_string_lossy());
        self.dir_mtime = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
        if self.live_side == self.active_side && virt_mode(&path).is_none() {
            self.cfg.last_dir = Some(path.clone());
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
                std::thread::spawn(move || {
                    let ents: Vec<fsys::Entry> =
                        list.iter().filter_map(|p| fsys::entry_from_path(p, p.to_string_lossy().into_owned())).collect();
                    let _ = tx.send(fsys::LoadMsg::Chunk(generation, ents));
                    let _ = tx.send(fsys::LoadMsg::Done(generation, None));
                    ctx.request_repaint();
                });
            }
            VMode::Trash => {
                let (tx, generation, cur, ctx) = (self.load_tx.clone(), self.generation, self.gen_shared.clone(), self.ctx.clone());
                let slot = self.trash_rows.clone();
                std::thread::spawn(move || {
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
                                *slot.lock().unwrap() = rows;
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
        let paths: Vec<(PathBuf, bool)> = {
            let res = st.results.lock().unwrap();
            let end = res.len().min(self.search_taken + 400);
            let v = res[self.search_taken..end].to_vec();
            v
        };
        let got = paths.len();
        if got > 0 {
            let v = Arc::make_mut(&mut self.entries);
            let base = v.len();
            for (p, _) in paths {
                if let Some(e) = fsys::entry_from_path(&p, p.to_string_lossy().into_owned()) {
                    v.push(e);
                }
            }
            let new: Vec<u32> = (base..v.len())
                .filter(|&i| self.passes(&self.entries[i], &needle))
                .map(|i| i as u32)
                .collect();
            self.order.extend(new);
            self.sel.resize(self.entries.len().div_ceil(64), 0);
            self.search_taken += got;
            self.ctx.request_repaint();
        }
        let total = st.results.lock().unwrap().len();
        if st.done.load(Ordering::Acquire) && self.search_taken >= total {
            self.search_st = None;
            self.loading = false;
            self.spawn_sort();
            self.apply_pending();
        } else {
            self.loading = true;
            if self.search_taken < total {
                self.ctx.request_repaint();
            }
        }
    }

    fn navigate(&mut self, path: PathBuf) {
        // en Android «/storage/emulated» no se puede listar: el almacenamiento real es el usuario 0
        #[cfg(target_os = "android")]
        let path = if path == Path::new("/storage/emulated") { PathBuf::from("/storage/emulated/0") } else { path };
        if let Some(m) = virt_mode(&path) {
            if path == self.cwd && self.vmode == m {
                return;
            }
            self.back.push(self.cwd.clone());
            self.fwd.clear();
            self.preview = None;
            self.load_mode(path, false, m);
            self.scroll_to_top();
            return;
        }
        if !path.is_dir() {
            if path.exists() {
                fsys::open_external(&path);
            } else {
                self.say(tf("No existe: {}", &[&path.display()]), true);
            }
            return;
        }
        if path == self.cwd {
            return;
        }
        self.back.push(self.cwd.clone());
        self.fwd.clear();
        self.preview = None;
        self.load(path, false);
        self.scroll_to_top();
    }

    fn go_back(&mut self) {
        if let Some(p) = self.back.pop() {
            self.fwd.push(self.cwd.clone());
            self.preview = None;
            self.load(p, false);
        }
    }

    fn go_forward(&mut self) {
        if let Some(p) = self.fwd.pop() {
            self.back.push(self.cwd.clone());
            self.preview = None;
            self.load(p, false);
        }
    }

    fn go_up(&mut self) {
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

    fn refresh(&mut self) {
        if self.sel_count > 0 && self.sel_count < 5000 && self.pending_select.is_empty() {
            self.pending_select = self.selected_names();
        }
        let p = self.cwd.clone();
        self.load(p, true);
    }

    fn scroll_to_top(&mut self) {
        let id = egui::Id::new(("files", self.live_side));
        self.ctx.data_mut(|d| d.insert_temp(id, true));
    }

    fn say(&mut self, s: String, err: bool) {
        self.msg = Some((s, Instant::now(), err));
        self.ctx.request_repaint_after(Duration::from_secs(6));
    }

    fn spawn_sort(&mut self) {
        self.sort_seq += 1;
        self.sort_pending = true;
        let (seq, entries, tx, ctx) = (self.sort_seq, self.entries.clone(), self.sort_tx.clone(), self.ctx.clone());
        let spec = SortSpec {
            key: self.cfg.sort,
            asc: self.cfg.asc,
            show_hidden: self.cfg.show_hidden,
            filter: self.filter.clone(),
        };
        std::thread::spawn(move || {
            let order = fsys::build_order(&entries, &spec);
            let _ = tx.send((seq, order));
            ctx.request_repaint();
        });
    }

    fn passes(&self, e: &Entry, needle: &str) -> bool {
        (self.cfg.show_hidden || !e.is_hidden()) && (needle.is_empty() || fsys::contains_ci(&e.name, needle))
    }

    fn poll(&mut self) {
        self.poll_search();
        let mut done = false;
        let needle = self.filter.to_lowercase();
        while let Ok(m) = self.load_rx.try_recv() {
            match m {
                LoadMsg::Chunk(g, chunk) if g == self.generation => {
                    if !self.traced_first {
                        self.traced_first = true;
                        if std::env::var_os("FB_SHOT").is_some() {
                            eprintln!("[trace] primer bloque ({} entradas): {:?}", chunk.len(), self.t_load.elapsed());
                        }
                    }
                    let v = Arc::make_mut(&mut self.entries);
                    let base = v.len();
                    v.extend(chunk);
                    let new: Vec<u32> = (base..v.len())
                        .filter(|&i| self.passes(&self.entries[i], &needle))
                        .map(|i| i as u32)
                        .collect();
                    self.order.extend(new);
                    self.sel.resize(self.entries.len().div_ceil(64), 0);
                }
                LoadMsg::Done(g, err) if g == self.generation => {
                    if std::env::var_os("FB_SHOT").is_some() {
                        eprintln!("[trace] listado completo ({} entradas): {:?}", self.entries.len(), self.t_load.elapsed());
                    }
                    self.loading = false;
                    self.load_err = err;
                    done = true;
                }
                _ => {}
            }
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
            if std::env::var_os("FB_SHOT").is_some() {
                eprintln!("[trace] ordenado ({} visibles): {:?}", order.len(), self.t_load.elapsed());
            }
            self.order = order;
            self.sort_pending = false;
            if let Some(c) = self.cursor {
                self.scroll_to = Some(c);
            }
        }
    }

    fn apply_pending(&mut self) {
        if self.pending_select.is_empty() && !self.pending_rename {
            return;
        }
        let names: HashSet<&str> = self.pending_select.iter().map(|s| s.as_str()).collect();
        let mut first = None;
        let mut hits = Vec::new();
        for (i, e) in self.entries.iter().enumerate() {
            if names.contains(&*e.name) {
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
            self.scroll_to = Some(f);
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
    fn is_sel(&self, i: u32) -> bool {
        self.sel.get((i >> 6) as usize).map(|w| w >> (i & 63) & 1 == 1).unwrap_or(false)
    }

    fn set_bit(&mut self, i: u32, on: bool) {
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

    fn clear_sel(&mut self) {
        self.sel.iter_mut().for_each(|w| *w = 0);
        self.sel_count = 0;
        self.sel_size = 0;
    }

    fn for_each_selected(&self, mut f: impl FnMut(u32)) {
        for (wi, &w) in self.sel.iter().enumerate() {
            let mut w = w;
            while w != 0 {
                let b = w.trailing_zeros();
                f((wi as u32) * 64 + b);
                w &= w - 1;
            }
        }
    }

    fn recount(&mut self) {
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

    fn select_only(&mut self, i: u32) {
        self.clear_sel();
        self.set_bit(i, true);
        self.sel_count = 1;
        self.sel_size = self.entries[i as usize].size;
        self.cursor = Some(i);
        self.anchor = Some(i);
    }

    fn pos_of(&self, i: u32) -> Option<usize> {
        self.order.iter().position(|&x| x == i)
    }

    fn select_range(&mut self, a: usize, b: usize, additive: bool) {
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

    fn selected_names(&self) -> Vec<String> {
        let mut v = Vec::new();
        self.for_each_selected(|i| {
            if let Some(e) = self.entries.get(i as usize) {
                v.push(e.name.to_string());
            }
        });
        v
    }

    fn selected_paths(&self) -> Vec<PathBuf> {
        if self.vmode == VMode::Trash {
            return Vec::new(); // lo de la papelera no está en esas rutas: tiene sus propias acciones
        }
        self.selected_names().into_iter().map(|n| self.cwd.join(n)).collect()
    }

    fn move_cursor(&mut self, delta: isize, extend: bool) {
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

    // -----------------------------------------------------------------------
    // Operaciones
    // -----------------------------------------------------------------------

    /// Botón «Atrás» (Android): cierra lo que haya encima; si no, vuelve atrás o sube de carpeta.
    fn on_back_key(&mut self) {
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
        self.cfg.recent.retain(|r| r != p);
        self.cfg.recent.insert(0, p.to_path_buf());
        self.cfg.recent.truncate(40);
        self.cfg.save();
    }

    fn open_index(&mut self, i: u32) {
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
            self.run_program(&p);
        } else if self.touch && matches!(e.kind, Kind::Image | Kind::Text | Kind::Doc | Kind::Audio | Kind::Video) {
            self.note_recent(&p);
            self.open_preview_at(i);
        } else {
            self.note_recent(&p);
            fsys::open_external(&p);
        }
    }

    fn do_copy(&mut self, cut: bool) {
        let v = self.selected_paths();
        if v.is_empty() {
            return;
        }
        // Portapapeles de archivos del sistema si hay herramienta (wl-copy/xclip); si no, texto con rutas.
        if !tools::clip_set_files(&v, cut) {
            let text = v.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join("\n");
            self.ctx.copy_text(text);
        }
        self.say(tf("{} {} elemento(s)", &[&(if cut { t("Cortado:") } else { t("Copiado:") }), &v.len()]), false);
        self.clipboard = Some((v, cut));
        self.clip_hidden = false;
    }

    fn do_paste(&mut self) {
        // Si lo del portapapeles del sistema es lo que cortamos nosotros, se conserva el «cortar»
        // (macOS no lo transporta en el portapapeles).
        let own = self.clipboard.clone();
        let picked = match (tools::clip_get_files(), own) {
            (Some((sp, _)), Some((op, true))) if sp == op => Some((op, true)),
            (Some(sys), _) => Some(sys),
            (None, own) => own,
        };
        let Some((srcs, cut)) = picked else { return };
        let dst = self.cwd.clone();
        self.start_copy(srcs, dst, cut);
        if cut {
            self.clipboard = None;
        }
    }

    fn do_move_to(&mut self, dst: PathBuf) {
        let srcs = self.selected_paths();
        if srcs.is_empty() {
            return;
        }
        self.start_copy(srcs, dst, true);
    }

    fn do_trash(&mut self) {
        let v = self.selected_paths();
        if !v.is_empty() {
            self.start_op(Op::Trash(v), OpMeta::Trash { replay: false });
        }
    }

    fn start_rename(&mut self) {
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

    fn commit_rename(&mut self, idx: u32, new: String) {
        if self.remote_ro(&self.cwd.clone()) {
            return;
        }
        let Some(e) = self.entries.get(idx as usize) else { return };
        let old_full = e.name.to_string();
        let old = base_name(&old_full).to_string();
        let new = new.trim().to_string();
        if new.is_empty() || new == old || new == "." || new == ".." || new.contains(['/', '\\']) {
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
                    if !case_only && std::fs::symlink_metadata(&t2).is_ok() {
                        return Err(std::io::Error::new(std::io::ErrorKind::AlreadyExists, t("Ya existe un elemento con ese nombre")));
                    }
                    std::fs::rename(&f, &t2)
                }
            },
            move |a, r| match r {
                Ok(()) => {
                    a.msg = None;
                    a.push_undo(Undo::Rename { from, to }, true);
                    a.pending_select = vec![sel];
                    a.refresh();
                }
                Err(e) => a.say(tf("No se pudo renombrar: {e}", &[&e]), true),
            },
        );
    }

    fn new_folder(&mut self) {
        if self.remote_ro(&self.cwd.clone()) {
            return;
        }
        let (cwd, name) = (self.cwd.clone(), t("Nueva carpeta").to_string());
        self.say(t("Creando carpeta…").into(), false);
        self.spawn_io(
            move || {
                let p = fsys::unique_path(&cwd, &name);
                std::fs::create_dir(&p).map(|_| p)
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

    fn toggle_bookmark(&mut self, p: PathBuf) {
        if let Some(i) = self.cfg.bookmarks.iter().position(|b| *b == p) {
            self.cfg.bookmarks.remove(i);
        } else {
            self.cfg.bookmarks.push(p);
        }
        self.cfg.save();
    }

    /// Los servidores SFTP se reflejan en una carpeta de caché de sólo lectura: nada se escribe ahí.
    fn remote_ro(&mut self, p: &Path) -> bool {
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

    fn start_op(&mut self, op: Op, meta: OpMeta) {
        let remote = |v: &Vec<PathBuf>| v.iter().any(|p| crate::sftp::is_remote(p));
        let inarch = |p: &Path| self.arch_of(p).is_some();
        let touches = match &op {
            Op::Copy { srcs, dst, mv, .. } => {
                virt_mode(dst).is_some() || crate::sftp::is_remote(dst) || inarch(dst) || (*mv && (remote(srcs) || srcs.iter().any(|p| inarch(p))))
            }
            Op::Trash(v) | Op::Delete(v) => remote(v) || v.iter().any(|p| inarch(p)),
            Op::Compress { srcs, out, .. } => crate::sftp::is_remote(out) || remote(srcs) || inarch(out),
            Op::Unzip { src, dst, .. } => crate::sftp::is_remote(dst) || crate::sftp::is_remote(src) || inarch(dst),
        };
        if touches {
            self.say(t("Los servidores SFTP son de sólo lectura").into(), true);
            return;
        }
        if self.busy() {
            self.queue.push_back((op, meta));
            return;
        }
        self.op_unzip = if let Op::Unzip { src, dst, .. } = &op { Some((src.clone(), dst.clone())) } else { None };
        self.op_meta = meta;
        self.op_started = Instant::now();
        self.op_desc = op_desc(&op);
        self.op = Some(fsys::run_op(op, self.ctx.clone()));
    }

    /// Copia/mueve; si algún destino ya existe pregunta qué hacer.
    fn start_copy(&mut self, srcs: Vec<PathBuf>, dst: PathBuf, mv: bool) {
        self.spawn_io(
            move || {
                let names: Vec<String> = srcs
                    .iter()
                    .filter(|s| s.parent() != Some(dst.as_path()))
                    .filter_map(|s| {
                        let n = s.file_name()?;
                        std::fs::symlink_metadata(dst.join(n)).ok().map(|_| n.to_string_lossy().into_owned())
                    })
                    .collect();
                (srcs, dst, names)
            },
            move |a, (srcs, dst, names)| {
                if names.is_empty() {
                    a.start_op(Op::Copy { srcs, dst, mv, policy: Policy::Rename }, OpMeta::Copy { mv, replay: false });
                } else {
                    a.conflict = Some(ConflictDlg { srcs, dst, mv, names });
                }
            },
        );
    }

    fn push_undo(&mut self, u: Undo, clear_redo: bool) {
        if clear_redo {
            self.redo_stack.clear();
        }
        self.undo_stack.push(u);
        if self.undo_stack.len() > 100 {
            self.undo_stack.remove(0);
        }
    }

    fn undo(&mut self) {
        match self.undo_stack.pop() {
            Some(u) => self.apply_history(u, false),
            None => self.say(t("Nada que deshacer").into(), false),
        }
    }

    fn redo(&mut self) {
        match self.redo_stack.pop() {
            Some(u) => self.apply_history(u, true),
            None => self.say(t("Nada que rehacer").into(), false),
        }
    }

    fn rename_checked(a: &Path, b: &Path) -> Result<(), String> {
        if std::fs::symlink_metadata(b).is_ok() {
            return Err(tf("{}: ya existe", &[&b.display()]));
        }
        std::fs::rename(a, b).map_err(|e| format!("{}: {e}", a.display()))
    }

    fn apply_history(&mut self, u: Undo, is_redo: bool) {
        let mut err: Option<String> = None;
        match &u {
            Undo::Rename { from, to } => {
                let (a, b) = if is_redo { (from, to) } else { (to, from) };
                err = Self::rename_checked(a, b).err();
            }
            Undo::Moved(pairs) => {
                for (orig, new) in pairs {
                    let (a, b) = if is_redo { (orig, new) } else { (new, orig) };
                    if let Err(e) = Self::rename_checked(a, b) {
                        err = Some(e);
                        break;
                    }
                }
            }
            Undo::Created(items) => {
                if is_redo {
                    for (p, dir) in items {
                        let r = if *dir { std::fs::create_dir_all(p) } else { std::fs::File::create(p).map(|_| ()) };
                        if let Err(e) = r {
                            err = Some(format!("{}: {e}", p.display()));
                            break;
                        }
                    }
                } else {
                    let v = items.iter().map(|(p, _)| p.clone()).collect();
                    self.start_op(Op::Trash(v), OpMeta::Replay(u.clone()));
                    return;
                }
            }
            Undo::Copied(pairs) => {
                if is_redo {
                    for (src, new) in pairs {
                        if let Some(parent) = new.parent() {
                            self.start_op(
                                Op::Copy { srcs: vec![src.clone()], dst: parent.to_path_buf(), mv: false, policy: Policy::Rename },
                                OpMeta::Copy { mv: false, replay: true },
                            );
                        }
                    }
                    return;
                }
                let v = pairs.iter().map(|(_, n)| n.clone()).collect();
                self.start_op(Op::Trash(v), OpMeta::Replay(u.clone()));
                return;
            }
            Undo::Trashed(items) => {
                if is_redo {
                    self.start_op(Op::Trash(items.clone()), OpMeta::Trash { replay: true });
                    return;
                }
                err = extras::restore_paths(items).err();
            }
            Undo::Generated(paths) => {
                if is_redo {
                    return;
                }
                self.start_op(Op::Trash(paths.clone()), OpMeta::Other);
                return;
            }
        }
        match err {
            Some(e) => {
                self.say(tf("No se pudo deshacer: {e}", &[&e]), true);
                // se conserva en su pila original
                if is_redo { self.redo_stack.push(u) } else { self.undo_stack.push(u) }
            }
            None => {
                if is_redo { self.undo_stack.push(u) } else { self.redo_stack.push(u) }
                self.preview = None;
                self.refresh();
            }
        }
    }

    fn poll_op(&mut self) {
        let Some(op) = self.op.clone() else {
            if let Some((o, m)) = self.queue.pop_front() {
                self.start_op(o, m);
            }
            return;
        };
        if op.finished.load(Ordering::Acquire) {
            let errs = op.errors.lock().unwrap().clone();
            let created = op.created.lock().unwrap().clone();
            let pairs = op.pairs.lock().unwrap().clone();
            self.op = None;
            let meta = std::mem::replace(&mut self.op_meta, OpMeta::Other);
            // comprimido protegido: se pide la contraseña y se repite la extracción
            if let (Some(e), Some((src, dst))) = (errs.first().filter(|e| *e == crate::archive::NEED_PASS || *e == crate::archive::BAD_PASS), self.op_unzip.take()) {
                let _ = std::fs::remove_dir_all(&dst);
                self.dlg = Dlg::ArchPass(ArchPass { src, dst, meta, pass: String::new(), show: false, wrong: e == crate::archive::BAD_PASS, focus: true });
                return;
            }
            let mut enter: Option<PathBuf> = None;
            match meta {
                OpMeta::OpenArchive { archive, root } => {
                    if errs.is_empty() && !op.cancelled() {
                        let _ = std::fs::write(archive_marker(&root), b"");
                        if !self.arch.iter().any(|(r, _)| *r == root) {
                            self.arch.push((root.clone(), archive));
                        }
                        enter = Some(root);
                    } else {
                        let _ = std::fs::remove_dir_all(&root);
                    }
                }
                OpMeta::Copy { mv, replay } if !pairs.is_empty() => {
                    self.push_undo(if mv { Undo::Moved(pairs) } else { Undo::Copied(pairs) }, !replay);
                }
                OpMeta::Trash { replay } if !pairs.is_empty() => {
                    self.push_undo(Undo::Trashed(pairs.into_iter().map(|(a, _)| a).collect()), !replay);
                }
                OpMeta::Generated if !pairs.is_empty() => {
                    self.push_undo(Undo::Generated(pairs.into_iter().map(|(a, _)| a).collect()), true);
                }
                OpMeta::Replay(u) if errs.is_empty() => self.redo_stack.push(u),
                _ => {}
            }
            if op.cancelled() {
                self.say(t("Operación cancelada").into(), false);
            }
            if let Some((o, m)) = self.queue.pop_front() {
                self.start_op(o, m);
            }
            if errs.len() > 1 {
                self.dlg = Dlg::Errors(errs.clone());
            }
            if let Some(e) = errs.first() {
                self.say(tf("Error: {e}{}", &[e, &(if errs.len() > 1 { tf(" (+{} más)", &[&(errs.len() - 1)]) } else { String::new() })]), true);
            }
            self.pending_select = created;
            self.preview = None;
            self.refresh();
            if let Some(root) = enter {
                self.pending_select.clear();
                self.navigate(root);
            }
        }
    }

    fn sort_by(&mut self, key: SortKey) {
        if self.cfg.sort == key {
            self.cfg.asc = !self.cfg.asc;
        } else {
            self.cfg.sort = key;
            self.cfg.asc = true;
        }
        self.cfg.save();
        self.spawn_sort();
    }

    // -----------------------------------------------------------------------
    // Vista rápida
    // -----------------------------------------------------------------------

    fn open_preview_at(&mut self, i: u32) {
        let Some(e) = self.entries.get(i as usize) else { return };
        let path = self.cwd.join(&*e.name);
        let pos = self.pos_of(i).map(|p| p + 1).unwrap_or(0);
        let label = format!("{} de {}", pos, self.order.len());
        let key = self.thumb_key(e);
        let thumb = if crate::thumbs::thumbable(e.kind, e.ext()) { self.thumbs.get(key).cloned() } else { None };
        self.preview = Some(Preview::open(
            &mut self.pworker,
            path,
            e.name.to_string(),
            e.kind,
            e.size,
            e.mtime,
            label,
            thumb,
        ));
    }

    /// Panel derecho con la vista previa de la selección actual (siempre visible mientras esté activo).
    fn side_panel(&mut self, ui: &mut egui::Ui, pal: &Pal, bgfill: Color32, phone: bool) {
        let want = self.cfg.preview_panel && !phone && self.vmode != VMode::Trash && ui.ctx().content_rect().width() >= 700.0;
        if !want {
            self.side = None;
            return;
        }
        let target = self.cursor.filter(|&c| self.is_sel(c)).or_else(|| {
            let mut first = None;
            self.for_each_selected(|i| {
                first.get_or_insert(i);
            });
            first
        });
        let cur = target.and_then(|i| self.entries.get(i as usize)).map(|e| self.cwd.join(&*e.name));
        let same = match (&self.side, &cur) {
            (Some(p), Some(c)) => &p.path == c,
            (None, None) => true,
            _ => false,
        };
        if !same {
            self.side = None;
            if let (Some(i), Some(_)) = (target, &cur) {
                let e = &self.entries[i as usize];
                let path = self.cwd.join(&*e.name);
                let pos = self.pos_of(i).map(|p| p + 1).unwrap_or(0);
                let label = format!("{} de {}", pos, self.order.len());
                let key = self.thumb_key(e);
                let thumb = if crate::thumbs::thumbable(e.kind, e.ext()) { self.thumbs.get(key).cloned() } else { None };
                self.side = Some(Preview::open(&mut self.sworker, path, e.name.to_string(), e.kind, e.size, e.mtime, label, thumb));
            }
        }
        let modal = self.preview.is_some() || !matches!(self.dlg, Dlg::None);
        let resp = egui::Panel::right("preview_panel")
            .frame(egui::Frame::new().fill(bgfill).inner_margin(egui::Margin { left: 4, right: 10, top: 0, bottom: 4 }))
            .default_size(self.cfg.preview_w)
            .size_range(220.0..=760.0)
            .resizable(true)
            .show_separator_line(false)
            .show(ui, |ui| {
                let r = ui.available_rect_before_wrap();
                let p = ui.painter();
                p.rect_filled(r, CornerRadius::same(14), pal.panel);
                p.rect_stroke(r, CornerRadius::same(14), Stroke::new(1.0, pal.border.gamma_multiply(0.6)), egui::StrokeKind::Inside);
                match &mut self.side {
                    Some(pv) => {
                        pv.show_panel(ui.ctx(), p, r, pal, !modal);
                    }
                    None => {
                        p.text(r.center(), Align2::CENTER_CENTER, t("Vista previa"), FontId::proportional(14.0), pal.subtle);
                    }
                }
                ui.allocate_rect(r, egui::Sense::hover());
            });
        // el ancho se recuerda al soltar el borde
        let w = resp.response.rect.width();
        if (w - self.cfg.preview_w).abs() > 1.0 && !ui.ctx().input(|i| i.pointer.any_down()) {
            self.cfg.preview_w = w;
            self.cfg.save();
        }
    }

    fn toggle_preview(&mut self) {
        if self.vmode == VMode::Trash {
            return;
        }
        if self.preview.is_some() {
            self.preview = None;
            return;
        }
        let target = self.cursor.filter(|&c| self.is_sel(c)).or_else(|| {
            let mut first = None;
            self.for_each_selected(|i| {
                first.get_or_insert(i);
            });
            first
        });
        let target = target.or_else(|| self.order.first().copied());
        if let Some(i) = target {
            if self.sel_count == 0 {
                self.select_only(i);
            }
            self.open_preview_at(i);
        }
    }

    fn preview_step(&mut self, delta: isize) {
        self.move_cursor(delta, false);
        if let Some(c) = self.cursor {
            self.open_preview_at(c);
        }
    }

    fn thumb_key(&self, e: &Entry) -> u64 {
        let h = hash_str(self.dir_hash, &e.name);
        mix(mix(h, e.mtime as u64), e.size)
    }

    /// Teclado del visor (overlay o ventana aparte).
    fn preview_keys(&mut self, ctx: &egui::Context) {
        let k = |m: Modifiers, key: Key| ctx.input_mut(|i| consume_exact(i, m, key));
        let (is_video, active) = self
            .preview
            .as_ref()
            .map(|p| (p.can_play(), p.player_active()))
            .unwrap_or((false, false));
        if k(Modifiers::NONE, Key::Escape) {
            let closed = self.preview.as_mut().map(|p| p.close_find() || p.close_inner()).unwrap_or(false);
            if !closed {
                self.preview = None;
            }
        } else if self.preview.as_ref().map(|p| p.typing()).unwrap_or(false) {
            // el cuadro de búsqueda del documento se queda con el teclado
        } else if k(Modifiers::NONE, Key::Space) {
            if is_video {
                if let Some(p) = &mut self.preview {
                    p.toggle_play(ctx);
                }
            } else {
                self.preview = None;
            }
        } else if k(Modifiers::NONE, Key::I) {
            if let Some(p) = &mut self.preview {
                p.toggle_meta(ctx);
            }
        } else if k(Modifiers::NONE, Key::ArrowDown) {
            self.preview_step(1);
        } else if k(Modifiers::NONE, Key::ArrowUp) {
            self.preview_step(-1);
        } else if k(Modifiers::NONE, Key::ArrowRight) {
            if active {
                if let Some(p) = &mut self.preview {
                    p.seek_by(5.0);
                }
            } else {
                self.preview_step(1);
            }
        } else if k(Modifiers::NONE, Key::ArrowLeft) {
            if active {
                if let Some(p) = &mut self.preview {
                    p.seek_by(-5.0);
                }
            } else {
                self.preview_step(-1);
            }
        } else if k(Modifiers::NONE, Key::Enter) && is_video {
            if let Some(p) = &mut self.preview {
                p.toggle_play(ctx);
            }
        } else if k(Modifiers::NONE, Key::Enter) || k(Modifiers::NONE, Key::O) {
            if let Some(p) = &self.preview {
                let path = p.path.clone();
                if p.kind == Kind::Folder {
                    self.preview = None;
                    self.navigate(path);
                } else {
                    fsys::open_external(&path);
                }
            }
        }
    }

    /// Visor en ventana propia (viewport inmediato; se cierra solo al dejar de llamarlo).
    fn show_preview_window(&mut self, ctx: &egui::Context) {
        let pal = self.pal;
        let title = self.preview.as_ref().map(|p| p.name.clone()).unwrap_or_default();
        let builder = egui::ViewportBuilder::default()
            .with_title(title)
            .with_decorations(false)
            .with_transparent(self.transparent)
            .with_inner_size([980.0, 700.0])
            .with_min_inner_size([380.0, 280.0]);
        let mut close = false;
        ctx.show_viewport_immediate(egui::ViewportId::from_hash_of("preview_window"), builder, |ui, _class| {
            let c = ui.ctx().clone();
            if c.input(|i| i.viewport().close_requested()) {
                close = true;
                return;
            }
            self.preview_keys(&c);
            if let Some(d) = self.resize_edges(&c) {
                // sin marco del sistema: bordes propios para redimensionar
                c.send_viewport_cmd(egui::ViewportCommand::BeginResize(d));
            }
            let rounded = self.transparent;
            if let Some(pv) = &mut self.preview {
                if matches!(pv.show(&c, &pal, Some(rounded)), preview::Action::Close) {
                    close = true;
                }
            }
        });
        if close {
            self.preview = None;
        }
    }

    // -----------------------------------------------------------------------
    // Comandos, atajos y paleta
    // -----------------------------------------------------------------------

    /// Atajo efectivo de un comando (personalizado o por defecto).
    fn binding(&self, cmd: Cmd) -> Option<(Modifiers, Key)> {
        let i = commands::info(cmd);
        let spec = self.cfg.keys.iter().find(|(id, _)| id == i.id).map(|(_, s)| s.as_str()).unwrap_or(i.default);
        if spec.is_empty() { None } else { commands::parse(spec) }
    }

    fn shortcut_text(&self, cmd: Cmd) -> String {
        let i = commands::info(cmd);
        self.cfg.keys.iter().find(|(id, _)| id == i.id).map(|(_, s)| s.clone()).unwrap_or_else(|| i.default.to_string())
    }

    fn set_binding(&mut self, cmd: Cmd, spec: Option<String>) {
        let id = commands::info(cmd).id;
        self.cfg.keys.retain(|(k, _)| k != id);
        match spec {
            Some(s) => {
                // si otro comando tenía ese atajo, queda sin atajo
                for i in commands::ALL.iter().filter(|i| i.rebind && i.cmd != cmd) {
                    if self.shortcut_text(i.cmd) == s {
                        self.cfg.keys.retain(|(k, _)| k != i.id);
                        self.cfg.keys.push((i.id.to_string(), String::new()));
                    }
                }
                if commands::info(cmd).default != s {
                    self.cfg.keys.push((id.to_string(), s));
                }
            }
            None => self.cfg.keys.push((id.to_string(), String::new())),
        }
        self.cfg.save();
    }

    fn run_cmd(&mut self, cmd: Cmd) {
        use Cmd as C;
        // en la papelera, «eliminar» es eliminar definitivamente de ella
        if self.vmode == VMode::Trash && matches!(cmd, C::Trash | C::DeletePerm) {
            self.trash_ask_purge();
            return;
        }
        match cmd {
            C::Palette => self.palette = Some(PaletteState::default()),
            C::Back => self.go_back(),
            C::Forward => self.go_forward(),
            C::Up => self.go_up(),
            C::Refresh => self.refresh(),
            C::Home => {
                if let Some(h) = self.home.clone() {
                    self.navigate(h);
                }
            }
            C::EditPath => {
                self.addr_edit = Some(self.cwd.display().to_string());
                self.addr_select = true;
            }
            C::Bookmark => self.toggle_bookmark(self.cwd.clone()),
            C::OpenTrash => {
                if extras::trash_supported() {
                    self.navigate(PathBuf::from(LOC_TRASH));
                }
            }
            C::NewTab => self.new_tab(self.cwd.clone()),
            C::CloseTab => self.close_tab(self.cur_tab),
            C::ReopenTab => self.reopen_tab(),
            C::NextTab => self.step_tab(1),
            C::PrevTab => self.step_tab(-1),
            C::NewWindow => self.new_window(),
            C::NewFolder => self.new_folder(),
            C::NewFile => self.dlg = Dlg::NewFile(t("Nuevo archivo.txt").into()),
            C::Rename => self.start_rename(),
            C::BatchRename => {
                if self.sel_count > 1 {
                    self.open_batch();
                }
            }
            C::Trash => self.do_trash(),
            C::DeletePerm => {
                let v = self.selected_paths();
                if !v.is_empty() {
                    self.confirm_delete = Some(v);
                }
            }
            C::Duplicate => self.do_duplicate(),
            C::CopyPath => {
                let t = self.selected_paths().iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join("\n");
                self.ctx.copy_text(t);
            }
            C::Undo => self.undo(),
            C::Redo => self.redo(),
            C::SelectAll => self.select_range(0, self.order.len().saturating_sub(1), false),
            C::InvertSel => self.invert_selection(),
            C::Props => self.open_props(),
            C::OpenWith => {
                if self.sel_count == 1 {
                    self.open_with();
                }
            }
            C::Hash => {
                if self.sel_count == 1 {
                    self.open_hash();
                }
            }
            C::Dups => self.open_dups(),
            C::Zip => self.do_zip(),
            C::Unzip => self.do_unzip(),
            C::Copy => self.do_copy(false),
            C::Cut => self.do_copy(true),
            C::Paste => self.do_paste(),
            C::Preview => self.toggle_preview(),
            C::ToggleHidden => {
                self.cfg.show_hidden = !self.cfg.show_hidden;
                self.cfg.save();
                self.spawn_sort();
            }
            C::PreviewPanel => {
                self.cfg.preview_panel = !self.cfg.preview_panel;
                self.cfg.save();
            }
            C::ToggleSidebar => {
                self.cfg.show_sidebar = !self.cfg.show_sidebar;
                self.cfg.save();
            }
            C::Terminal => self.toggle_terminal(),
            C::TerminalHere => self.terminal_here(),
            C::ToggleSplit => self.toggle_split(),
            C::SwitchPane => self.focus_side(1 - self.active_side),
            C::CopyOther => self.to_other_pane(false),
            C::MoveOther => self.to_other_pane(true),
            C::GridView => self.set_view(1),
            C::ListView => self.set_view(0),
            C::Theme => self.set_dark(!self.cfg.dark),
            C::Search => self.open_search(),
            C::Filter => self.filter_focus = true,
            C::Options => self.dlg = Dlg::Options,
            C::Keys => self.dlg = Dlg::Keys,
            C::ConnectServer => self.connect_uri(String::new()),
        }
    }

    fn palette_window(&mut self, ctx: &egui::Context) {
        let Some(mut ps) = self.palette.take() else { return };
        let pal = self.pal;
        let q = ps.query.trim().to_lowercase();
        let mut items: Vec<(i32, &'static commands::Info)> = commands::ALL
            .iter()
            .filter_map(|i| commands::fuzzy(&q, t(i.label)).map(|s| (s, i)))
            .collect();
        items.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| t(a.1.label).cmp(t(b.1.label))));
        let n = items.len();
        let (down, up, enter, esc) = ctx.input_mut(|i| {
            (
                i.consume_key(Modifiers::NONE, Key::ArrowDown),
                i.consume_key(Modifiers::NONE, Key::ArrowUp),
                i.consume_key(Modifiers::NONE, Key::Enter),
                i.consume_key(Modifiers::NONE, Key::Escape),
            )
        });
        if down && n > 0 {
            ps.sel = (ps.sel + 1) % n;
        }
        if up && n > 0 {
            ps.sel = (ps.sel + n - 1) % n;
        }
        ps.sel = ps.sel.min(n.saturating_sub(1));
        let mut run: Option<Cmd> = None;
        let mut close = esc;
        let before = ps.query.clone();
        let resp = egui::Area::new(egui::Id::new("palette"))
            .order(egui::Order::Foreground)
            .anchor(Align2::CENTER_TOP, vec2(0.0, 70.0))
            .show(ctx, |ui| {
                egui::Frame::new()
                    .fill(pal.panel)
                    .stroke(Stroke::new(1.0, pal.border))
                    .corner_radius(CornerRadius::same(16))
                    .inner_margin(egui::Margin::same(12))
                    .shadow(egui::Shadow { offset: [0, 8], blur: 24, spread: 0, color: Color32::from_black_alpha(110) })
                    .show(ui, |ui| {
                        ui.set_width(540.0);
                        let te = ui.add(
                            egui::TextEdit::singleline(&mut ps.query)
                                .hint_text(t("Escribe un comando…"))
                                .desired_width(f32::INFINITY)
                                .margin(vec2(8.0, 7.0)),
                        );
                        te.request_focus();
                        ui.add_space(6.0);
                        if n == 0 {
                            ui.label(egui::RichText::new(t("Sin resultados")).color(pal.subtle));
                        }
                        egui::ScrollArea::vertical().max_height(360.0).auto_shrink([false, true]).show(ui, |ui| {
                            for (pos, (_, info)) in items.iter().enumerate() {
                                let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 30.0), Sense::click());
                                let on = pos == ps.sel;
                                if on || resp.hovered() {
                                    ui.painter().rect_filled(r, CornerRadius::same(8), if on { pal.sel } else { pal.hover });
                                }
                                if on && (down || up) {
                                    ui.scroll_to_rect(r, None);
                                }
                                ui.painter().text(
                                    pos2(r.left() + 10.0, r.center().y),
                                    Align2::LEFT_CENTER,
                                    t(info.label),
                                    FontId::proportional(14.0),
                                    pal.text,
                                );
                                let sc = self.shortcut_text(info.cmd);
                                if !sc.is_empty() {
                                    ui.painter().text(
                                        pos2(r.right() - 10.0, r.center().y),
                                        Align2::RIGHT_CENTER,
                                        sc,
                                        FontId::proportional(12.0),
                                        pal.subtle,
                                    );
                                }
                                if resp.clicked() {
                                    run = Some(info.cmd);
                                }
                            }
                        });
                    });
            });
        if ps.query != before {
            ps.sel = 0;
        }
        if enter {
            run = items.get(ps.sel).map(|(_, i)| i.cmd);
        }
        if ctx.input(|i| i.pointer.primary_clicked()) {
            if let Some(q) = ctx.input(|i| i.pointer.interact_pos()) {
                if !resp.response.rect.contains(q) {
                    close = true;
                }
            }
        }
        if run.is_some() || close {
            if let Some(c) = run {
                self.run_cmd(c);
            }
        } else {
            self.palette = Some(ps);
        }
    }

    // -----------------------------------------------------------------------
    // Teclado
    // -----------------------------------------------------------------------

    fn handle_keys(&mut self, ctx: &egui::Context) {
        if let Some(cmd) = self.rebinding {
            let ev = ctx.input(|i| {
                i.events.iter().find_map(|e| match e {
                    egui::Event::Key { key, pressed: true, modifiers, .. } => Some((*key, *modifiers)),
                    _ => None,
                })
            });
            if let Some((key, m)) = ev {
                ctx.input_mut(|i| i.events.retain(|e| !matches!(e, egui::Event::Key { .. } | egui::Event::Text(_))));
                self.rebinding = None;
                if key != Key::Escape {
                    self.set_binding(cmd, Some(commands::format(m, key)));
                }
            }
            return;
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::F12) || i.consume_key(Modifiers::COMMAND, Key::Backtick)) {
            self.toggle_terminal();
            return;
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::F10)) {
            if let Some(id) = self.menu_id {
                egui::Popup::toggle_id(ctx, id);
            }
            return;
        }
        if self.term_focus && self.term.is_some() {
            return; // el teclado es del terminal
        }
        if ctx.egui_wants_keyboard_input() {
            return;
        }
        let (copy, cut, paste, text) = ctx.input(|i| {
            let mut t = String::new();
            for e in &i.events {
                if let egui::Event::Text(s) = e {
                    t.push_str(s);
                }
            }
            (
                i.events.iter().any(|e| matches!(e, egui::Event::Copy)),
                i.events.iter().any(|e| matches!(e, egui::Event::Cut)),
                i.events.iter().any(|e| matches!(e, egui::Event::Paste(_))),
                t,
            )
        });
        let k = |m: Modifiers, key: Key| ctx.input_mut(|i| consume_exact(i, m, key));

        if let Some((m, key)) = self.binding(Cmd::Palette) {
            if k(m, key) {
                self.run_cmd(Cmd::Palette);
                return;
            }
        }
        if self.preview.is_some() {
            self.preview_keys(ctx);
            return;
        }

        // Comandos con atajo (configurable): primero los más específicos (más modificadores).
        let mut bound: Vec<(Cmd, Modifiers, Key)> = commands::ALL
            .iter()
            .filter(|i| i.rebind && i.cmd != Cmd::Palette)
            .filter_map(|i| self.binding(i.cmd).map(|(m, key)| (i.cmd, m, key)))
            .collect();
        bound.sort_by_key(|(_, m, _)| std::cmp::Reverse(m.alt as u8 + m.shift as u8 + (m.command || m.ctrl) as u8));
        if !self.filter.is_empty() {
            // con un filtro escrito: Retroceso borra una letra y Espacio es una letra más
            if k(Modifiers::NONE, Key::Backspace) {
                self.filter.pop();
                self.spawn_sort();
                return;
            }
            k(Modifiers::NONE, Key::Space);
        }
        for (cmd, m, key) in bound {
            if matches!(cmd, Cmd::SwitchPane | Cmd::CopyOther | Cmd::MoveOther) && self.other.is_none() {
                continue;
            }
            if k(m, key) {
                self.run_cmd(cmd);
            }
        }
        if k(Modifiers::NONE, Key::Space) {
            self.toggle_preview();
            return;
        }
        // alias fijos
        if k(Modifiers::COMMAND | Modifiers::SHIFT, Key::Z) {
            self.redo();
        }
        if k(Modifiers::COMMAND, Key::R) {
            self.refresh();
        }
        if k(Modifiers::ALT, Key::ArrowUp) {
            self.go_up();
        }
        let cols = self.cols.max(1) as isize;
        let grid = self.cfg.grid;
        let step_v = if grid { cols } else { 1 };
        let page = self.page_rows.max(1) as isize * step_v;
        for (shift, m) in [(false, Modifiers::NONE), (true, Modifiers::SHIFT)] {
            if k(m, Key::ArrowDown) {
                self.move_cursor(step_v, shift);
            }
            if k(m, Key::ArrowUp) {
                self.move_cursor(-step_v, shift);
            }
            if grid {
                if k(m, Key::ArrowRight) {
                    self.move_cursor(1, shift);
                }
                if k(m, Key::ArrowLeft) {
                    self.move_cursor(-1, shift);
                }
            }
            if k(m, Key::PageDown) {
                self.move_cursor(page, shift);
            }
            if k(m, Key::PageUp) {
                self.move_cursor(-page, shift);
            }
            if k(m, Key::Home) {
                self.move_cursor(-(self.order.len() as isize), shift);
            }
            if k(m, Key::End) {
                self.move_cursor(self.order.len() as isize, shift);
            }
        }
        if k(Modifiers::NONE, Key::Enter) {
            if let Some(c) = self.cursor.filter(|&c| self.is_sel(c)) {
                self.open_index(c);
            }
        }
        if k(Modifiers::NONE, Key::Escape) {
            if !self.filter.is_empty() {
                self.filter.clear();
                self.spawn_sort();
            } else {
                self.clear_sel();
            }
        }
        if copy {
            self.do_copy(false);
        }
        if cut {
            self.do_copy(true);
        }
        if paste {
            self.do_paste();
        }
        // Escribir en la lista filtra la carpeta con esas letras
        let printable: String = text.chars().filter(|c| !c.is_control()).collect();
        let ok = !printable.is_empty()
            && self.palette.is_none()
            && self.rebinding.is_none()
            && (printable.trim().len() == printable.len() || !self.filter.is_empty());
        if ok {
            self.filter.push_str(&printable);
            self.spawn_sort();
            self.scroll_to_top();
        }
    }
}

// ---------------------------------------------------------------------------
// Dibujo
// ---------------------------------------------------------------------------

fn icon_button(ui: &mut egui::Ui, pal: &Pal, g: Glyph, enabled: bool, active: bool, tip: &str) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(34.0, 34.0), if enabled { Sense::click() } else { Sense::hover() });
    let p = ui.painter();
    if enabled && (resp.hovered() || active) {
        let fill = if active { pal.sel } else { pal.hover };
        p.rect_filled(rect, CornerRadius::same(10), fill);
    }
    let col = if !enabled {
        pal.bg.lerp_to_gamma(pal.subtle, 0.5)
    } else if active {
        pal.accent
    } else {
        pal.text
    };
    theme::draw_glyph(p, rect.center(), 20.0, g, col);
    if enabled && !tip.is_empty() {
        resp.on_hover_text(tip)
    } else {
        resp
    }
}

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

/// Botón pequeño con un triángulo (subir/bajar), dibujado a mano: la fuente no trae flechas.
fn tri_button(ui: &mut egui::Ui, up: bool, enabled: bool) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(26.0, 22.0), if enabled { Sense::click() } else { Sense::hover() });
    let vis = ui.visuals();
    let col = if enabled { vis.text_color() } else { vis.weak_text_color().gamma_multiply(0.5) };
    if enabled && resp.hovered() {
        ui.painter().rect_filled(rect, CornerRadius::same(6), vis.widgets.hovered.weak_bg_fill);
    }
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
    let p = ui.painter();
    if on {
        p.rect_filled(rect, CornerRadius::same(8), pal.sel);
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
            _ => tf("{count} elementos", &[&v.len()]),
        }
    };
    match op {
        Op::Copy { srcs, mv, .. } => format!("{} {}", if *mv { t("Moviendo") } else { t("Copiando") }, what(srcs)),
        Op::Trash(v) => format!("{} {}", t("Moviendo a la papelera"), what(v)),
        Op::Delete(v) => format!("{} {}", t("Eliminando"), what(v)),
        Op::Compress { srcs, .. } => format!("{} {}", t("Comprimiendo"), what(srcs)),
        Op::Unzip { src, .. } => format!("{} {}", t("Extrayendo"), what(std::slice::from_ref(src))),
    }
}

fn side_item(ui: &mut egui::Ui, pal: &Pal, g: Glyph, tex: Option<egui::TextureId>, label: &str, active: bool, drop_hot: bool) -> egui::Response {
    let w = ui.available_width();
    let (rect, resp) = ui.allocate_exact_size(vec2(w, 32.0), Sense::click());
    let p = ui.painter();
    if drop_hot {
        p.rect_filled(rect, CornerRadius::same(10), pal.sel);
        p.rect_stroke(rect, CornerRadius::same(10), Stroke::new(1.5, pal.accent), egui::StrokeKind::Inside);
    } else if active {
        p.rect_filled(rect, CornerRadius::same(10), pal.sel);
    } else if resp.hovered() {
        p.rect_filled(rect, CornerRadius::same(10), pal.hover);
    }
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

impl App {
    /// Pide al sistema mover (`dir == None`) o redimensionar la ventana. En Windows, si el sistema no
    /// responde (le pasa a algunas instalaciones), `window_drag_tick` lo hace a mano.
    fn begin_window_drag(&mut self, ctx: &egui::Context, dir: Option<egui::ResizeDirection>) {
        use egui::ViewportCommand as V;
        ctx.send_viewport_cmd(match dir {
            Some(d) => V::BeginResize(d),
            None => V::StartDrag,
        });
        if cfg!(windows) {
            let (outer, inner) = ctx.input(|i| (i.viewport().outer_rect, i.viewport().inner_rect));
            if let (Some(c), Some(o), Some(inn)) = (screen_cursor(ctx.pixels_per_point()), outer, inner) {
                self.win_drag = Some(WinDrag { dir, cursor0: c, outer0: o, inner0: inn, manual: false });
            }
        }
    }

    fn window_drag_tick(&mut self, ctx: &egui::Context) {
        use egui::{ResizeDirection as D, ViewportCommand as V};
        let Some(wd) = self.win_drag.as_mut() else { return };
        if !ctx.input(|i| i.pointer.primary_down()) {
            self.win_drag = None;
            return;
        }
        let Some(cur) = screen_cursor(ctx.pixels_per_point()) else {
            self.win_drag = None;
            return;
        };
        let d = cur - wd.cursor0;
        if !wd.manual {
            let moved = ctx
                .input(|i| i.viewport().outer_rect)
                .map(|o| (o.min - wd.outer0.min).length() > 0.5 || (o.size() - wd.outer0.size()).length() > 0.5)
                .unwrap_or(true);
            if moved {
                // el sistema está moviendo/redimensionando la ventana: nada que hacer
                self.win_drag = None;
            } else if d.length() > 8.0 {
                wd.manual = true;
            }
            return;
        }
        let mut r = wd.outer0;
        match wd.dir {
            None => r = r.translate(d),
            Some(dir) => {
                let (w, e, n, s) = (
                    matches!(dir, D::West | D::NorthWest | D::SouthWest),
                    matches!(dir, D::East | D::NorthEast | D::SouthEast),
                    matches!(dir, D::North | D::NorthWest | D::NorthEast),
                    matches!(dir, D::South | D::SouthWest | D::SouthEast),
                );
                let (min_w, min_h) = (640.0, 420.0);
                if w {
                    r.min.x = (r.min.x + d.x).min(r.max.x - min_w);
                }
                if e {
                    r.max.x = (r.max.x + d.x).max(r.min.x + min_w);
                }
                if n {
                    r.min.y = (r.min.y + d.y).min(r.max.y - min_h);
                }
                if s {
                    r.max.y = (r.max.y + d.y).max(r.min.y + min_h);
                }
                let frame = wd.outer0.size() - wd.inner0.size();
                ctx.send_viewport_cmd(V::InnerSize(r.size() - frame));
            }
        }
        ctx.send_viewport_cmd(V::OuterPosition(r.min));
        ctx.request_repaint();
    }

    fn toolbar(&mut self, ui: &mut egui::Ui) {
        let pal = self.pal;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            if icon_button(ui, &pal, Glyph::Back, !self.back.is_empty(), false, t("Atrás (Alt+Left)")).clicked() {
                self.go_back();
            }
            if icon_button(ui, &pal, Glyph::Forward, !self.fwd.is_empty(), false, t("Adelante (Alt+Right)")).clicked() {
                self.go_forward();
            }
            if icon_button(ui, &pal, Glyph::Up, self.cwd.parent().is_some(), false, t("Subir (Retroceso)")).clicked() {
                self.go_up();
            }
            if icon_button(ui, &pal, Glyph::Refresh, true, false, t("Actualizar (F5)")).clicked() {
                self.refresh();
            }
            ui.add_space(4.0);
            ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                // rueda dentada: ordenar, ocultos y tema
                let gear = icon_button(ui, &pal, Glyph::Gear, true, false, t("Ajustes de vista"));
                self.gear_id = Some(egui::Popup::default_response_id(&gear));
                egui::Popup::menu(&gear).show(|ui| {
                    ui.set_width(190.0);
                    ui.spacing_mut().item_spacing.y = 2.0;
                    menu_head(ui, &pal, t("Ordenar"));
                    for (k, l) in [
                        (SortKey::Name, t("Nombre")),
                        (SortKey::Date, t("Fecha de modificación")),
                        (SortKey::Size, t("Tamaño")),
                        (SortKey::Type, t("Tipo")),
                    ] {
                        let cur = self.cfg.sort == k;
                        if menu_row(ui, &pal, l, cur, Some(self.cfg.asc)) {
                            self.sort_by(k);
                            ui.close();
                        }
                    }
                    ui.separator();
                    menu_head(ui, &pal, t("Tema"));
                    for (m, l) in [(0u8, t("Sistema")), (1, t("Claro")), (2, t("Oscuro"))] {
                        if menu_row(ui, &pal, l, self.cfg.theme_mode == m, None) {
                            self.set_theme_mode(m);
                            ui.close();
                        }
                    }
                    ui.separator();
                    menu_head(ui, &pal, t("Mostrar"));
                    if menu_row(ui, &pal, t("Ocultos"), self.cfg.show_hidden, None) {
                        self.cfg.show_hidden = !self.cfg.show_hidden;
                        self.cfg.save();
                        self.spawn_sort();
                    }
                });
                if icon_button(ui, &pal, Glyph::List, true, !self.cfg.grid && !self.cfg.col_view, t("Vista de lista")).clicked() {
                    self.set_view(0);
                }
                if icon_button(ui, &pal, Glyph::Grid, true, self.cfg.grid, t("Vista de cuadrícula")).clicked() {
                    self.set_view(1);
                }
                if !self.phone && icon_button(ui, &pal, Glyph::Columns, true, self.cfg.col_view, t("Vista de columnas")).clicked() {
                    self.set_view(2);
                }
                ui.add_space(4.0);
                // filtro
                let te = egui::TextEdit::singleline(&mut self.filter)
                    .hint_text(t("Filtrar…  (Ctrl+F)"))
                    .desired_width(170.0)
                    .margin(egui::Margin::symmetric(10, 8));
                let r = ui.add(te);
                if self.filter_focus {
                    r.request_focus();
                    self.filter_focus = false;
                }
                if r.changed() {
                    self.spawn_sort();
                }
                ui.add_space(6.0);
                if icon_button(ui, &pal, Glyph::FolderPlus, true, false, t("Nueva carpeta (Ctrl+Mayús+N)")).clicked() {
                    self.new_folder();
                }
                if self.touch {
                    // selección múltiple táctil: con el modo activo, tocar alterna elementos
                    if icon_button(ui, &pal, Glyph::Check, true, self.sel_mode, t("Seleccionar")).clicked() {
                        self.sel_mode = !self.sel_mode;
                        if !self.sel_mode {
                            self.clear_sel();
                        }
                    }
                }
                ui.add_space(6.0);
                // barra de dirección: ocupa el resto
                let w = ui.available_width().max(80.0);
                self.address_bar(ui, w);
            });
        });
    }

    /// Barra superior del móvil, en dos líneas: arriba los iconos (favoritos y atrás a la izquierda,
    /// selección múltiple y menú a la derecha) y debajo sólo la ruta, a todo el ancho.
    fn phone_bar(&mut self, ui: &mut egui::Ui) {
        let pal = self.pal;
        let screen = self.ctx.content_rect();
        ui.spacing_mut().item_spacing.y = 6.0;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            // favoritos y lugares (la antigua barra lateral)
            let (r, places) = ui.allocate_exact_size(vec2(48.0, 48.0), Sense::click());
            if places.hovered() || egui::Popup::is_id_open(ui.ctx(), egui::Popup::default_response_id(&places)) {
                ui.painter().rect_filled(r, CornerRadius::same(12), pal.hover);
            }
            theme::draw_glyph(ui.painter(), r.center(), 22.0, Glyph::Bookmark, pal.text);
            egui::Popup::menu(&places).show(|ui| {
                let w = (screen.width() - 24.0).min(300.0);
                ui.set_width(w);
                ui.set_height((screen.height() - 90.0).min(520.0));
                let gen0 = self.generation;
                self.sidebar(ui);
                if self.generation != gen0 {
                    ui.close();
                }
            });
            // atrás
            let can_back = !self.back.is_empty();
            let (r, back) = ui.allocate_exact_size(vec2(48.0, 48.0), Sense::click());
            if can_back && back.hovered() {
                ui.painter().rect_filled(r, CornerRadius::same(12), pal.hover);
            }
            theme::draw_glyph(ui.painter(), r.center(), 22.0, Glyph::Back, if can_back { pal.text } else { pal.subtle.gamma_multiply(0.5) });
            if can_back && back.clicked() {
                self.go_back();
            }
            ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                // menú
                let (r, more) = ui.allocate_exact_size(vec2(48.0, 48.0), Sense::click());
                if more.hovered() || egui::Popup::is_id_open(ui.ctx(), egui::Popup::default_response_id(&more)) {
                    ui.painter().rect_filled(r, CornerRadius::same(12), pal.hover);
                }
                theme::draw_glyph(ui.painter(), r.center(), 22.0, Glyph::More, pal.text);
                let max_h = (screen.height() - 90.0).max(200.0);
                egui::Popup::menu(&more).show(|ui| {
                    egui::ScrollArea::vertical().max_height(max_h).show(ui, |ui| self.phone_menu(ui));
                });
                // selección múltiple
                let (r, sel) = ui.allocate_exact_size(vec2(48.0, 48.0), Sense::click());
                if sel.hovered() || self.sel_mode {
                    ui.painter().rect_filled(r, CornerRadius::same(12), if self.sel_mode { pal.sel } else { pal.hover });
                }
                theme::draw_glyph(ui.painter(), r.center(), 22.0, Glyph::Check, if self.sel_mode { pal.accent } else { pal.text });
                if sel.clicked() {
                    self.sel_mode = !self.sel_mode;
                    if !self.sel_mode {
                        self.clear_sel();
                    }
                }
            });
        });
        // segunda línea: sólo la ruta (tocar una carpeta navega; tocar el resto la edita)
        let w = ui.available_width().max(80.0);
        self.address_bar(ui, w);
    }

    /// Menú del móvil: sólo lo imprescindible.
    fn phone_menu(&mut self, ui: &mut egui::Ui) {
        let pal = self.pal;
        ui.set_width(250.0);
        ui.spacing_mut().interact_size.y = 48.0;
        ui.spacing_mut().item_spacing.y = 2.0;
        let (lbl, to_grid) = if self.cfg.grid { (t("Vista de lista"), false) } else { (t("Vista de cuadrícula"), true) };
        if menu_row(ui, &pal, lbl, false, None) {
            self.cfg.grid = to_grid;
            self.cfg.save();
            ui.close();
        }
        let bm = self.cfg.bookmarks.contains(&self.cwd);
        if menu_row(ui, &pal, if bm { t("Quitar marcador") } else { t("Añadir marcador") }, bm, None) {
            self.toggle_bookmark(self.cwd.clone());
            ui.close();
        }
        if menu_row(ui, &pal, t("Nueva carpeta"), false, None) {
            self.new_folder();
            ui.close();
        }
        if menu_row(ui, &pal, t("Nuevo archivo vacío…"), false, None) {
            self.dlg = Dlg::NewFile(t("Nuevo archivo.txt").into());
            ui.close();
        }
        if self.clipboard.is_some() && menu_row(ui, &pal, t("Pegar"), false, None) {
            self.do_paste();
            ui.close();
        }
        if menu_row(ui, &pal, t("Seleccionar todo"), false, None) {
            self.sel_mode = true;
            self.run_cmd(Cmd::SelectAll);
            ui.close();
        }
        if menu_row(ui, &pal, t("Buscar en subcarpetas"), false, None) {
            self.run_cmd(Cmd::Search);
            ui.close();
        }
        if self.cwd.parent().is_some() && menu_row(ui, &pal, t("Carpeta superior"), false, None) {
            self.go_up();
            ui.close();
        }
        if menu_row(ui, &pal, t("Actualizar"), false, None) {
            self.refresh();
            ui.close();
        }
        ui.separator();
        menu_head(ui, &pal, t("Ordenar"));
        for (k, l) in [
            (SortKey::Name, t("Nombre")),
            (SortKey::Date, t("Fecha de modificación")),
            (SortKey::Size, t("Tamaño")),
            (SortKey::Type, t("Tipo")),
        ] {
            if menu_row(ui, &pal, l, self.cfg.sort == k, Some(self.cfg.asc)) {
                self.sort_by(k);
                ui.close();
            }
        }
        ui.separator();
        menu_head(ui, &pal, t("Mostrar"));
        if menu_row(ui, &pal, t("Ocultos"), self.cfg.show_hidden, None) {
            self.cfg.show_hidden = !self.cfg.show_hidden;
            self.cfg.save();
            self.spawn_sort();
            ui.close();
        }
        ui.separator();
        menu_head(ui, &pal, t("Tema"));
        for (m, l) in [(0u8, t("Sistema")), (1, t("Claro")), (2, t("Oscuro"))] {
            if menu_row(ui, &pal, l, self.cfg.theme_mode == m, None) {
                self.set_theme_mode(m);
                ui.close();
            }
        }
        ui.separator();
        if menu_row(ui, &pal, t("Conectar a un servidor…"), false, None) {
            self.connect_uri(String::new());
            ui.close();
        }
        if menu_row(ui, &pal, t("Opciones"), false, None) {
            self.dlg = Dlg::Options;
            ui.close();
        }
    }

    fn address_bar(&mut self, ui: &mut egui::Ui, w: f32) {
        let pal = self.pal;
        let (rect, resp) = ui.allocate_exact_size(vec2(w, if self.phone { 48.0 } else { 34.0 }), Sense::click());
        let p = ui.painter().clone();
        p.rect_filled(rect, CornerRadius::same(10), pal.surface);
        let rect = if self.phone {
            rect
        } else {
        // marcador dentro de la barra, a la derecha
        let bm = self.cfg.bookmarks.contains(&self.cwd);
        let br = Rect::from_center_size(pos2(rect.right() - 20.0, rect.center().y), vec2(30.0, 30.0));
        let bresp = ui.interact(br, ui.id().with("addr_bm"), Sense::click());
        if bresp.hovered() || bm {
            p.rect_filled(br, CornerRadius::same(8), if bm { pal.sel } else { pal.hover });
        }
        theme::draw_glyph(
            &p,
            br.center(),
            18.0,
            if bm { Glyph::BookmarkFilled } else { Glyph::Bookmark },
            if bm { pal.accent } else { pal.subtle },
        );
        if bresp.on_hover_text(if bm { t("Quitar marcador (Ctrl+D)") } else { t("Añadir marcador (Ctrl+D)") }).clicked() {
            self.toggle_bookmark(self.cwd.clone());
        }
            Rect::from_min_max(rect.min, pos2(rect.right() - 38.0, rect.bottom()))
        };
        if self.addr_edit.is_some() {
            let mut buf = self.addr_edit.take().unwrap();
            let sep = std::path::MAIN_SEPARATOR;
            // Sugerencias: subcarpetas que empiezan por el fragmento tecleado.
            let expanded = match (buf.strip_prefix('~'), &self.home) {
                (Some(rest), Some(h)) => format!("{}{rest}", h.display()),
                _ => buf.clone(),
            };
            let (dir_s, frag) = match expanded.rfind(['/', '\\']) {
                Some(i) => (expanded[..=i].to_string(), expanded[i + 1..].to_string()),
                None => (String::new(), expanded.clone()),
            };
            if self.addr_cache.0 != dir_s {
                let mut names: Vec<String> = Vec::new();
                if !dir_s.is_empty() {
                    if let Ok(rd) = std::fs::read_dir(&dir_s) {
                        for e in rd.flatten() {
                            if e.file_type().map(|t| t.is_dir()).unwrap_or(false)
                                || std::fs::metadata(e.path()).map(|m| m.is_dir()).unwrap_or(false)
                            {
                                names.push(e.file_name().to_string_lossy().into_owned());
                            }
                        }
                    }
                    names.sort_by(|a, b| fsys::natural_cmp(a, b));
                }
                self.addr_cache = (dir_s.clone(), names);
            }
            let fl = frag.to_lowercase();
            let sugg: Vec<String> = self
                .addr_cache
                .1
                .iter()
                .filter(|n| if fl.is_empty() { !n.starts_with('.') } else { n.to_lowercase().starts_with(&fl) })
                .filter(|n| n.to_lowercase() != fl)
                .take(8)
                .cloned()
                .collect();
            let prefix_len = buf.rfind(['/', '\\']).map(|i| i + 1).unwrap_or(0);
            let complete = |buf: &mut String, name: &str| {
                buf.truncate(prefix_len);
                buf.push_str(name);
                buf.push(sep);
            };
            let mut enter_pick: Option<String> = None;
            if !sugg.is_empty() {
                if ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::ArrowDown)) {
                    self.addr_hl = Some(self.addr_hl.map(|h| (h + 1).min(sugg.len() - 1)).unwrap_or(0));
                }
                if ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::ArrowUp)) {
                    self.addr_hl = Some(self.addr_hl.map(|h| h.saturating_sub(1)).unwrap_or(0));
                }
                if ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Tab)) {
                    let n = &sugg[self.addr_hl.unwrap_or(0).min(sugg.len() - 1)];
                    complete(&mut buf, n);
                    self.addr_hl = None;
                }
                if let Some(h) = self.addr_hl {
                    if ui.input(|i| i.key_pressed(Key::Enter)) {
                        enter_pick = sugg.get(h).cloned();
                    }
                }
            }
            if let Some(n) = enter_pick {
                complete(&mut buf, &n);
                self.addr_hl = None;
                self.navigate(PathBuf::from(buf.trim()));
                return;
            }
            let te = egui::TextEdit::singleline(&mut buf)
                // id fijo: con el teclado del móvil cambian los márgenes y un id automático podría variar,
                // perdiendo el foco (y ocultando el teclado) y la selección
                .id(egui::Id::new("addr_edit"))
                .font(FontId::proportional(14.0))
                .desired_width(rect.width() - 8.0)
                .min_size(vec2(rect.width() - 8.0, rect.height()))
                .vertical_align(Align::Center)
                .margin(egui::Margin::symmetric(8, 0))
                .frame(egui::Frame::NONE);
            let r = ui.put(rect.shrink2(vec2(2.0, 0.0)), te);
            if r.changed() {
                self.addr_hl = None;
            }
            if !r.has_focus() && !r.lost_focus() {
                r.request_focus();
            }
            if std::mem::take(&mut self.addr_select) {
                self.addr_select_n = 12;
            }
            if self.addr_select_n > 0 {
                if r.changed() {
                    self.addr_select_n = 0;
                } else if r.has_focus() {
                    self.addr_select_n -= 1;
                    let mut st = egui::TextEdit::load_state(ui.ctx(), r.id).unwrap_or_default();
                    let end = egui::text::CCursor::new(buf.chars().count());
                    st.cursor.set_char_range(Some(egui::text::CCursorRange::two(egui::text::CCursor::new(0), end)));
                    st.store(ui.ctx(), r.id);
                }
                ui.ctx().request_repaint();
            }
            let mut sugg_rect = None;
            if !sugg.is_empty() {
                let hl = self.addr_hl;
                let mut picked: Option<String> = None;
                let area = egui::Area::new(egui::Id::new("addr_sugg"))
                    .order(egui::Order::Foreground)
                    .fixed_pos(rect.left_bottom() + vec2(0.0, 4.0))
                    .show(ui.ctx(), |ui| {
                        egui::Frame::popup(ui.style()).show(ui, |ui| {
                            ui.set_min_width(rect.width() - 16.0);
                            for (i, n) in sugg.iter().enumerate() {
                                if ui.selectable_label(hl == Some(i), n).clicked() {
                                    picked = Some(n.clone());
                                }
                            }
                        });
                    });
                sugg_rect = Some(area.response.rect);
                if let Some(n) = picked {
                    complete(&mut buf, &n);
                    self.addr_hl = None;
                    self.addr_edit = Some(buf);
                    return;
                }
            }
            self.addr_sugg_rect = sugg_rect;
            let enter = ui.input(|i| i.key_pressed(Key::Enter));
            let esc = ui.input(|i| i.key_pressed(Key::Escape));
            let in_sugg = ui
                .input(|i| i.pointer.interact_pos())
                .map(|q| self.addr_sugg_rect.map(|r| r.contains(q)).unwrap_or(false))
                .unwrap_or(false);
            if enter {
                let t = buf.trim().to_string();
                let t = match (t.strip_prefix('~'), &self.home) {
                    (Some(rest), Some(h)) => format!("{}{rest}", h.display()),
                    _ => t,
                };
                self.addr_hl = None;
                self.navigate(PathBuf::from(t));
            } else if esc || (r.lost_focus() && !in_sugg && (!self.phone || ui.input(|i| i.pointer.any_pressed()))) {
                // en el móvil el teclado puede quitar el foco al aparecer o recolocarse: sólo se cierra al tocar fuera
                self.addr_hl = None;
            } else {
                self.addr_edit = Some(buf);
            }
            return;
        }
        // migas de pan
        let mut segs: Vec<(String, PathBuf)> = Vec::new();
        let push_segs = |segs: &mut Vec<(String, PathBuf)>, path: &Path, base: Option<&Path>| {
            let mut acc = base.map(|b| b.to_path_buf()).unwrap_or_default();
            for c in path.components() {
                acc.push(c.as_os_str());
                match c {
                    Component::Prefix(pf) => segs.push((pf.as_os_str().to_string_lossy().into_owned(), acc.clone())),
                    Component::RootDir => {
                        if let Some(l) = segs.last_mut() {
                            l.1 = acc.clone();
                        } else {
                            segs.push(("/".into(), acc.clone()));
                        }
                    }
                    Component::Normal(n) => segs.push((n.to_string_lossy().into_owned(), acc.clone())),
                    _ => {}
                }
            }
        };
        if let Some((root, archive)) = self.arch_of(&self.cwd) {
            // dentro de un comprimido: ruta virtual «carpeta/archivo.zip/subcarpeta»
            push_segs(&mut segs, &archive, None);
            if let Some(l) = segs.last_mut() {
                l.1 = root.clone();
            }
            let rel = self.cwd.strip_prefix(&root).unwrap_or(Path::new(""));
            push_segs(&mut segs, rel, Some(&root));
        } else if virt_mode(&self.cwd).is_some() {
            segs.push((self.dir_label(&self.cwd), self.cwd.clone()));
        } else if let Some((mount, label)) = self.gvfs_mount_of(&self.cwd) {
            // carpeta de red montada: un solo tramo legible en vez de /run/user/1000/gvfs/smb-share:…
            segs.push((label, mount.clone()));
            let rel = self.cwd.strip_prefix(&mount).unwrap_or(Path::new(""));
            push_segs(&mut segs, rel, Some(&mount));
        } else {
            push_segs(&mut segs, &self.cwd, None);
        }
        let font = FontId::proportional(14.0);
        let widths: Vec<f32> = segs
            .iter()
            .map(|(s, _)| p.layout_no_wrap(s.clone(), font.clone(), pal.text).size().x + 18.0)
            .collect();
        let avail = rect.width() - 16.0;
        let sep = 14.0;
        let mut start = segs.len();
        let mut used = 0.0;
        while start > 0 {
            let add = widths[start - 1] + if start < segs.len() { sep } else { 0.0 };
            if used + add > avail - 28.0 && start < segs.len() {
                break;
            }
            used += add;
            start -= 1;
        }
        let mut x = rect.left() + 8.0;
        let ptr = ui.input(|i| i.pointer.hover_pos());
        if let Some(q) = ui.input(|i| if i.pointer.primary_pressed() { i.pointer.interact_pos() } else { None }) {
            self.addr_press = Some(q);
        }
        // al soltar el dedo no hay posición de puntero: usamos la del toque
        let click_pos = ptr.or(self.addr_press);
        let mut clicked_path: Option<PathBuf> = None;
        if start > 0 {
            p.text(pos2(x + 8.0, rect.center().y), Align2::LEFT_CENTER, "…", font.clone(), pal.subtle);
            x += 24.0;
        }
        for i in start..segs.len() {
            let r = Rect::from_min_size(pos2(x, rect.top() + 4.0), vec2(widths[i], rect.height() - 8.0));
            let hot = ptr.map(|q| r.contains(q) && resp.hovered()).unwrap_or(false);
            let last = i + 1 == segs.len();
            if hot {
                p.rect_filled(r, CornerRadius::same(8), pal.hover);
            }
            p.text(r.center(), Align2::CENTER_CENTER, &segs[i].0, font.clone(), if last { pal.text } else { pal.subtle });
            if resp.clicked() && click_pos.map(|q| r.contains(q)).unwrap_or(false) {
                clicked_path = Some(segs[i].1.clone());
            }
            x += widths[i];
            if !last {
                let c = pos2(x + sep / 2.0, rect.center().y);
                p.add(egui::Shape::line(
                    vec![c + vec2(-2.5, -4.0), c + vec2(1.5, 0.0), c + vec2(-2.5, 4.0)],
                    Stroke::new(1.3, pal.subtle),
                ));
                x += sep;
            }
        }
        if let Some(path) = clicked_path {
            self.navigate(path);
        } else if resp.clicked() {
            self.addr_edit = Some(self.cwd.display().to_string());
            self.addr_select = true;
        }
    }

    /// Hace `work` (E/S que puede tardar, p. ej. en una carpeta de red) en un hilo y, al terminar,
    /// ejecuta `done` con su resultado en el hilo de la interfaz.
    fn spawn_io<T: Send + 'static>(&self, work: impl FnOnce() -> T + Send + 'static, done: impl FnOnce(&mut App, T) + Send + 'static) {
        let (q, ctx) = (self.io_done.clone(), self.ctx.clone());
        std::thread::spawn(move || {
            let r = work();
            q.lock().unwrap().push(Box::new(move |a: &mut App| done(a, r)));
            ctx.request_repaint();
        });
    }

    /// Desmonta un recurso de red (gvfs o montaje del sistema) en un hilo.
    #[cfg(target_os = "linux")]
    fn unmount(&mut self, path: PathBuf) {
        // si se está dentro del recurso, se sale antes para que no quede ocupado
        if self.cwd.starts_with(&path) {
            let home = self.home.clone().unwrap_or_else(|| PathBuf::from("/"));
            self.navigate(home);
        }
        for tab in self.other.as_deref_mut() {
            if tab.cwd.starts_with(&path) {
                tab.cwd = self.home.clone().unwrap_or_else(|| PathBuf::from("/"));
            }
        }
        self.say(t("Desmontando…").into(), false);
        let is_gvfs = gvfs_dir().map(|g| path.starts_with(g)).unwrap_or(false);
        let p2 = path.clone();
        self.spawn_io(
            move || -> Result<(), String> {
                use std::process::{Command, Stdio};
                let run = |prog: &str, args: &[&std::ffi::OsStr]| -> Result<(), String> {
                    let o = Command::new("timeout")
                        .arg("30")
                        .arg(prog)
                        .args(args)
                        .stdin(Stdio::null())
                        .output()
                        .map_err(|e| e.to_string())?;
                    if o.status.success() {
                        Ok(())
                    } else {
                        let e = String::from_utf8_lossy(&o.stderr).trim().to_string();
                        Err(if e.is_empty() { prog.to_string() } else { e })
                    }
                };
                let p = p2.as_os_str();
                if is_gvfs {
                    let r = run("gio", &[std::ffi::OsStr::new("mount"), std::ffi::OsStr::new("-u"), p]);
                    if r.is_ok() {
                        return r;
                    }
                    // por dirección, reconstruida del nombre del directorio gvfs
                    let n = p2.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    let (sch, props) = n.split_once(':').unwrap_or((&n, ""));
                    let get = |k: &str| props.split(',').find_map(|kv| kv.strip_prefix(&format!("{k}=")).map(String::from));
                    let uri = match sch {
                        "smb-share" => format!("smb://{}/{}", get("server").unwrap_or_default(), get("share").unwrap_or_default()),
                        "dav" | "davs" => format!("{}://{}/", if get("ssl").as_deref() == Some("true") { "davs" } else { "dav" }, get("host").unwrap_or_default()),
                        other => format!("{other}://{}/", get("host").unwrap_or_default()),
                    };
                    return run("gio", &[std::ffi::OsStr::new("mount"), std::ffi::OsStr::new("-u"), std::ffi::OsStr::new(&uri)]).or(r);
                }
                // montaje del sistema: gio (udisks), luego fusermount y umount
                run("gio", &[std::ffi::OsStr::new("mount"), std::ffi::OsStr::new("-u"), p])
                    .or_else(|_| run("fusermount", &[std::ffi::OsStr::new("-u"), p]))
                    .or_else(|_| run("umount", &[p]))
            },
            |a, r| {
                match r {
                    Ok(()) => a.say(t("Desmontado").into(), false),
                    Err(e) => a.say(tf("No se pudo desmontar: {}", &[&e]), true),
                }
                a.refresh_network();
            },
        );
    }

    /// Recalcula la lista de sitios de red en un hilo (evita bloquear la interfaz).
    fn refresh_network(&mut self) {
        if self.net_refreshing.swap(true, Ordering::Relaxed) {
            return;
        }
        let (slot, flag, ctx) = (self.net_list.clone(), self.net_refreshing.clone(), self.ctx.clone());
        std::thread::spawn(move || {
            let v = list_network();
            *slot.lock().unwrap() = Some(v);
            flag.store(false, Ordering::Relaxed);
            ctx.request_repaint();
        });
    }

    /// Abre el diálogo de conexión para una dirección (vacía = el usuario la escribe).
    fn connect_uri(&mut self, uri: String) {
        let host = uri.split_once("://").map(|(_, r)| r.split('/').next().unwrap_or("").to_string()).unwrap_or_default();
        let (user_in_uri, host) = match host.rsplit_once('@') {
            Some((u, h)) => (Some(u.to_string()), h.to_string()),
            None => (None, host),
        };
        let user = user_in_uri.or_else(|| self.net_users.get(&host).cloned()).unwrap_or_default();
        self.dlg = Dlg::NetLogin(NetLogin { uri, user, domain: "WORKGROUP".into(), pass: String::new(), anon: false });
    }

    /// Monta con `gio mount`; si la dirección no indica recurso, monta cada recurso compartido.
    fn start_mount(&mut self, l: &NetLogin) {
        #[cfg(target_os = "linux")]
        {
            let uri = l.uri.trim().to_string();
            let cred = GioCred { user: l.user.clone(), domain: l.domain.clone(), pass: l.pass.clone(), anon: l.anon };
            if let Some((_, r)) = uri.split_once("://") {
                let host = r.split('/').next().unwrap_or("").rsplit('@').next().unwrap_or("").to_string();
                self.net_users.insert(host, l.user.clone());
            }
            self.net_busy.store(true, Ordering::Relaxed);
            self.say(tf("Conectando con {}…", &[&uri]), false);
            let (slot, err_slot, busy, ctx) = (self.net_nav.clone(), self.net_err.clone(), self.net_busy.clone(), self.ctx.clone());
            let shares_slot = self.net_shares.clone();
            let login = NetLogin { uri: uri.clone(), user: l.user.clone(), domain: l.domain.clone(), pass: l.pass.clone(), anon: l.anon };
            std::thread::spawn(move || {
                let res = (|| -> Result<Option<PathBuf>, String> {
                    // ya montado (por otra aplicación o sesión anterior): sin volver a autenticar
                    let server_only = uri.starts_with("smb://") && uri.trim_end_matches('/').split('/').nth(3).is_none();
                    if !server_only {
                        if let Some(p) = uri_to_gvfs(&uri).or_else(|| gio_local_path(&uri)) {
                            return Ok(Some(p));
                        }
                    }
                    // «ya montada» (el mensaje sale en el idioma del sistema): si gio puede listarla, vale.
                    // Un servidor ya montado en modo exploración no tiene ruta local: se montan sus recursos.
                    if let Err(m) = run_gio(&["mount"], &uri, &cred) {
                        let listable = std::process::Command::new("timeout")
                            .args(["15", "gio", "list", &uri])
                            .stdin(std::process::Stdio::null())
                            .output()
                            .map(|o| o.status.success())
                            .unwrap_or(false);
                        if !listable {
                            return Err(m);
                        }
                    }
                    if !server_only {
                        if let Some(p) = uri_to_gvfs(&uri) {
                            return Ok(Some(p));
                        }
                    }
                    // servidor sin recurso: gvfs no lo expone como carpeta; se ofrecen sus recursos
                    // compartidos para elegir uno (sólo se monta ése)
                    let base = uri.trim_end_matches('/').to_string();
                    let out = std::process::Command::new("timeout")
                        .args(["20", "gio", "list"])
                        .arg(format!("{base}/"))
                        .stdin(std::process::Stdio::null())
                        .output()
                        .map_err(|e| e.to_string())?;
                    let shares: Vec<String> = String::from_utf8_lossy(&out.stdout)
                        .lines()
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty() && !s.ends_with('$'))
                        .collect();
                    if shares.is_empty() {
                        return Err(t("No hay recursos compartidos accesibles").to_string());
                    }
                    *shares_slot.lock().unwrap() = Some(NetShares { base, shares, login });
                    Ok(None)
                })();
                match res {
                    Ok(Some(p)) => *slot.lock().unwrap() = Some(p),
                    Ok(None) => {}
                    Err(m) => *err_slot.lock().unwrap() = Some(m),
                }
                busy.store(false, Ordering::Relaxed);
                ctx.request_repaint();
            });
        }
        #[cfg(any(windows, target_os = "macos"))]
        {
            let uri = l.uri.trim().to_string();
            self.net_busy.store(true, Ordering::Relaxed);
            self.say(tf("Conectando con {}…", &[&uri]), false);
            let (slot, err_slot, busy, ctx) = (self.net_nav.clone(), self.net_err.clone(), self.net_busy.clone(), self.ctx.clone());
            let login = crate::netmount::Login { uri, user: l.user.clone(), domain: l.domain.clone(), pass: l.pass.clone(), anon: l.anon };
            std::thread::spawn(move || {
                match crate::netmount::connect(&login) {
                    Ok(p) => *slot.lock().unwrap() = Some(p),
                    Err(m) => *err_slot.lock().unwrap() = Some(m),
                }
                busy.store(false, Ordering::Relaxed);
                ctx.request_repaint();
            });
        }
        #[cfg(not(any(target_os = "linux", windows, target_os = "macos")))]
        {
            let _ = l;
            self.say(t("Conectar a servidores no está disponible en este sistema").into(), true);
        }
    }

    fn sidebar(&mut self, ui: &mut egui::Ui) {
        let pal = self.pal;
        let ptr = ui.input(|i| i.pointer.hover_pos());
        let dragging = self.drag_items;
        self.side_drop = None;
        self.bm_drop = false;
        let mut go: Option<PathBuf> = None;
        let mut remove: Option<usize> = None;
        let mut connect: Option<String> = None;
        let mut unmount: Option<PathBuf> = None;
        let mut show_recent = false;
        let mut show_trash = false;
        let mut clear_recent = false;
        let mut empty_trash = false;
        let cwd = self.cwd.clone();
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            let head = |ui: &mut egui::Ui, t: &str| {
                ui.add_space(8.0);
                ui.label(egui::RichText::new(t.to_uppercase()).size(11.0).color(pal.subtle).strong());
                ui.add_space(2.0);
            };
            head(ui, t("Favoritos"));
            let sys_on = self.cfg.sys_icons;
            for pl in &self.places {
                let tex = if sys_on { self.sys.folder(&self.ctx, Some(pl.sys)) } else { None };
                let r = side_item(ui, &pal, pl.glyph, tex, &pl.name, pl.path == cwd, false);
                if dragging && ptr.map(|q| r.rect.contains(q)).unwrap_or(false) {
                    self.side_drop = Some(pl.path.clone());
                }
                if r.clicked() {
                    go = Some(pl.path.clone());
                }
            }
            let r = side_item(ui, &pal, Glyph::Clock, None, t("Recientes"), cwd == Path::new(LOC_RECENT), false);
            if r.clicked() {
                show_recent = true;
            }
            r.context_menu(|ui| {
                if ui.add_enabled(!self.cfg.recent.is_empty(), egui::Button::new(t("Vaciar recientes"))).clicked() {
                    clear_recent = true;
                    ui.close();
                }
            });
            if extras::trash_supported() {
                let r = side_item(ui, &pal, Glyph::Trash, None, t("Papelera"), cwd == Path::new(LOC_TRASH), false);
                if r.clicked() {
                    show_trash = true;
                }
                r.context_menu(|ui| {
                    if ui.button(t("Vaciar papelera")).clicked() {
                        empty_trash = true;
                        ui.close();
                    }
                });
            }
            if !self.cfg.bookmarks.is_empty() || dragging {
                head(ui, t("Marcadores"));
            }
            for (i, b) in self.cfg.bookmarks.iter().enumerate() {
                let name = b.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| b.display().to_string());
                let (g, sys) = match self.places.iter().find(|pl| &pl.path == b) {
                    Some(pl) => (pl.glyph, Some(pl.sys)),
                    None => (Glyph::Folder, None),
                };
                let tex = if sys_on { self.sys.folder(&self.ctx, sys) } else { None };
                let r = side_item(ui, &pal, g, tex, &name, *b == cwd, false);
                if dragging && ptr.map(|q| r.rect.contains(q)).unwrap_or(false) {
                    self.side_drop = Some(b.clone());
                }
                if r.clicked() {
                    go = Some(b.clone());
                }
                r.on_hover_text(b.display().to_string()).context_menu(|ui| {
                    if ui.button(t("Quitar marcador")).clicked() {
                        remove = Some(i);
                        ui.close();
                    }
                });
            }
            if dragging {
                let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 34.0), Sense::hover());
                let hot = ptr.map(|q| r.contains(q)).unwrap_or(false);
                self.bm_drop = hot;
                let p = ui.painter();
                p.rect_filled(r, CornerRadius::same(10), if hot { pal.sel } else { pal.hover.gamma_multiply(0.5) });
                p.rect_stroke(r, CornerRadius::same(10), Stroke::new(1.2, if hot { pal.accent } else { pal.border }), egui::StrokeKind::Inside);
                p.text(r.center(), Align2::CENTER_CENTER, t("Suelta aquí para añadir a marcadores"), FontId::proportional(12.0), if hot { pal.accent } else { pal.subtle });
            }
            if !self.drives.is_empty() {
                head(ui, t("Dispositivos"));
            }
            for d in &self.drives {
                let tex = if sys_on { self.sys.folder(&self.ctx, Some(d.sys)) } else { None };
                let r = side_item(ui, &pal, d.glyph, tex, &d.name, d.path == cwd, false);
                if dragging && ptr.map(|q| r.rect.contains(q)).unwrap_or(false) {
                    self.side_drop = Some(d.path.clone());
                }
                if r.clicked() {
                    go = Some(d.path.clone());
                }
            }
            head(ui, t("Red"));
            for n in &self.network {
                let active = n.path.as_ref().map(|p| *p == cwd).unwrap_or(false);
                let r = side_item(ui, &pal, Glyph::Network, None, &n.name, active, false);
                let tip = match (&n.path, &n.uri) {
                    (Some(p), _) => p.display().to_string(),
                    (None, Some(u)) => u.clone(),
                    _ => String::new(),
                };
                let r = r.on_hover_text(tip);
                #[cfg(target_os = "linux")]
                if let Some(p) = &n.path {
                    r.context_menu(|ui| {
                        if ui.button(t("Desmontar")).clicked() {
                            unmount = Some(p.clone());
                            ui.close();
                        }
                    });
                }
                if r.clicked() {
                    match (&n.path, &n.uri) {
                        (Some(p), _) => go = Some(p.clone()),
                        (None, Some(u)) => connect = Some(u.clone()),
                        _ => {}
                    }
                }
            }
            let r = side_item(ui, &pal, Glyph::Plus, None, t("Conectar a un servidor…"), false, false);
            if r.clicked() {
                connect = Some(String::new());
            }
        });
        if show_recent {
            self.navigate(PathBuf::from(LOC_RECENT));
        }
        if show_trash {
            self.navigate(PathBuf::from(LOC_TRASH));
        }
        if clear_recent {
            self.cfg.recent.clear();
            self.cfg.save();
            if self.vmode == VMode::Recent {
                self.refresh();
            }
        }
        if empty_trash {
            self.confirm_purge = Some(PurgeReq::All);
        }
        if let Some(u) = connect {
            self.connect_uri(u);
        }
        #[cfg(target_os = "linux")]
        if let Some(p) = unmount {
            self.unmount(p);
        }
        #[cfg(not(target_os = "linux"))]
        let _ = unmount;
        if let Some(i) = remove {
            self.cfg.bookmarks.remove(i);
            self.cfg.save();
        }
        if let Some(p) = go {
            self.navigate(p);
        }
    }

    fn status_bar(&mut self, ui: &mut egui::Ui) {
        let pal = self.pal;
        ui.horizontal_centered(|ui| {
            let total = self.entries.len();
            let shown = self.order.len();
            let mut s = if shown == total || self.loading {
                tf("{} elementos", &[&shown])
            } else {
                tf("{shown} de {total} elementos", &[&shown, &total])
            };
            if self.loading {
                s.push_str(t("  ·  cargando…"));
            }
            if self.sel_count > 0 {
                s.push_str(&tf("  ·  {} seleccionados ({})", &[&self.sel_count, &fsys::human_size(self.sel_size)]));
            }
            ui.label(egui::RichText::new(s).size(12.5).color(pal.subtle));
            if let Some(n) = &self.hover_name {
                ui.label(egui::RichText::new(format!("  ·  {n}")).size(12.5).color(pal.text));
            }
            if let Some((m, t, err)) = &self.msg {
                if t.elapsed() < Duration::from_secs(6) {
                    let c = if *err { Color32::from_rgb(235, 100, 100) } else { pal.accent };
                    ui.label(egui::RichText::new(format!("  {m}")).size(12.5).color(c));
                }
            }
            if self.phone {
                return;
            }
            ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                let label = egui::RichText::new(t("Terminal")).size(12.0);
                if ui.selectable_label(self.cfg.term_open, label).on_hover_text(t("Mostrar/ocultar terminal (F12)")).clicked() {
                    self.toggle_terminal();
                }
                if self.cfg.grid {
                    ui.spacing_mut().slider_width = 130.0;
                    let r = ui.add(egui::Slider::new(&mut self.cfg.zoom, 64.0..=200.0).show_value(false));
                    if r.drag_stopped() {
                        self.cfg.save();
                    }
                }
                if let Some(op) = self.op.clone() {
                    let tot = op.total.load(Ordering::Relaxed).max(1);
                    let d = op.done.load(Ordering::Relaxed).min(tot);
                    let scanning = op.label.lock().unwrap().as_str() == t("Calculando…");
                    let frac = if scanning { 0.0 } else { d as f32 / tot as f32 };
                    let mut txt = self.op_desc.clone();
                    if !scanning {
                        txt.push_str(&format!("  {:.0}%", frac * 100.0));
                    }
                    if !self.queue.is_empty() {
                        txt.push_str(&format!("  +{}", self.queue.len()));
                    }
                    let r = ui.add(egui::ProgressBar::new(frac).desired_width(220.0).text(egui::RichText::new(txt).size(12.0)).corner_radius(6));
                    let r = ui.interact(r.rect, egui::Id::new("tasks_btn"), Sense::click()).on_hover_text(t("Tareas en curso"));
                    if r.clicked() {
                        self.tasks_open = !self.tasks_open;
                    }
                    ui.ctx().set_cursor_icon(if r.hovered() { egui::CursorIcon::PointingHand } else { egui::CursorIcon::Default });
                }
            });
        });
    }

    /// Panel del portapapeles: lo copiado/cortado, pegar aquí, vaciar y ocultar.
    fn clip_bar(&mut self, ui: &mut egui::Ui) {
        let pal = self.pal;
        let Some((paths, cut)) = self.clipboard.clone() else { return };
        let mut paste = false;
        let mut clear = false;
        let mut hide = false;
        ui.horizontal_centered(|ui| {
            ui.label(egui::RichText::new(if cut { t("Cortado:") } else { t("Copiado:") }).size(12.5).color(pal.accent));
            let mut shown = 0;
            let mut used = 0.0;
            let room = (ui.available_width() - 260.0).max(80.0);
            for p in &paths {
                let n = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| p.display().to_string());
                let g = ui.painter().layout_no_wrap(n.clone(), FontId::proportional(12.5), pal.text);
                if used + g.size().x + 14.0 > room && shown > 0 {
                    break;
                }
                used += g.size().x + 14.0;
                shown += 1;
                egui::Frame::new().fill(pal.hover).corner_radius(CornerRadius::same(6)).inner_margin(egui::Margin::symmetric(6, 1)).show(ui, |ui| {
                    ui.label(egui::RichText::new(n).size(12.5).color(pal.text));
                });
            }
            if paths.len() > shown {
                ui.label(egui::RichText::new(format!("+{}", paths.len() - shown)).size(12.5).color(pal.subtle));
            }
            ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                if ui.small_button(t("Cerrar")).clicked() {
                    hide = true;
                }
                if ui.small_button(t("Vaciar")).clicked() {
                    clear = true;
                }
                if ui.small_button(t("Pegar aquí")).clicked() {
                    paste = true;
                }
            });
        });
        if paste {
            self.do_paste();
        }
        if clear {
            self.clipboard = None;
        }
        if hide {
            self.clip_hidden = true;
        }
    }

    /// Listado ligero de una carpeta para las columnas estáticas (con caché de unos segundos).
    fn col_rows(&mut self, dir: &Path) -> Arc<Vec<(String, bool)>> {
        if let Some((t0, v)) = self.col_cache.get(dir) {
            if t0.elapsed() < Duration::from_secs(3) {
                return v.clone();
            }
        }
        let hidden = self.cfg.show_hidden;
        let mut rows: Vec<(String, bool)> = Vec::new();
        if let Ok(rd) = std::fs::read_dir(dir) {
            for e in rd.flatten().take(20_000) {
                let n = e.file_name().to_string_lossy().into_owned();
                if !hidden && n.starts_with('.') {
                    continue;
                }
                let d = e.file_type().map(|t| t.is_dir() || (t.is_symlink() && e.path().is_dir())).unwrap_or(false);
                rows.push((n, d));
            }
        }
        rows.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| fsys::natural_cmp(&a.0, &b.0)));
        let v = Arc::new(rows);
        if self.col_cache.len() > 64 {
            self.col_cache.clear();
        }
        self.col_cache.insert(dir.to_path_buf(), (Instant::now(), v.clone()));
        v
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
                    let p = ui.painter();
                    if hl == Some(name.as_str()) {
                        p.rect_filled(r.shrink2(vec2(3.0, 1.0)), CornerRadius::same(8), pal.sel);
                    } else if resp.hovered() {
                        p.rect_filled(r.shrink2(vec2(3.0, 1.0)), CornerRadius::same(8), pal.hover.gamma_multiply(0.7));
                    }
                    let ir = Rect::from_center_size(pos2(r.left() + 18.0, r.center().y), vec2(18.0, 18.0));
                    if let (true, Some(tex)) = (*is_dir, folder_tex) {
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
    fn show_columns(&mut self, ui: &mut egui::Ui) {
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

    fn confirm_dialog(&mut self, ctx: &egui::Context) {
        let Some(list) = self.confirm_delete.clone() else { return };
        let mut close = false;
        let pal = self.pal;
        let mut go = false;
        theme::modal_card(
            ctx,
            &pal,
            theme::Tone::Danger,
            t("Eliminar definitivamente"),
            400.0,
            |ui| {
                ui.label(egui::RichText::new(tf("Se eliminarán {} elemento(s) de forma permanente. No se puede deshacer.", &[&list.len()])).color(pal.subtle));
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
            self.start_op(Op::Delete(list.clone()), OpMeta::Other);
            close = true;
        }
        if close {
            self.confirm_delete = None;
        }
    }

    // -----------------------------------------------------------------------
    // Zona principal: cuadrícula / lista virtualizada
    // -----------------------------------------------------------------------

    fn list_header(&mut self, ui: &mut egui::Ui) {
        let pal = self.pal;
        let (full, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 30.0), Sense::click());
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
            let hr_resp = ui.interact(hr, egui::Id::new(("col_w", pc.def.id, self.live_side)), Sense::click_and_drag());
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
                self.cfg.save();
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
            self.cfg.save();
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
            let _ = i;
            if let (Some(k), true) = (*k, resp.clicked() && ptr.map(|q| hit.contains(q)).unwrap_or(false)) {
                self.sort_by(k);
            }
        }
        p.line_segment([rect.left_bottom(), rect.right_bottom()], Stroke::new(1.0, pal.border));
    }

    /// Editor de las columnas de la vista de lista: cuáles se muestran y en qué orden. Devuelve si cambió algo.
    fn columns_editor(&mut self, ui: &mut egui::Ui) -> bool {
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
                    if tri_button(ui, false, i + 1 < shown.len()).clicked() {
                        swap = Some((i, i + 1));
                    }
                    if tri_button(ui, true, i > 0).clicked() {
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
                            .margin(egui::Margin::symmetric(8, 6)),
                    );
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
                    let state = if self.search_st.is_some() { format!(" · {} {}", t("buscando…"), scanned) } else { String::new() };
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
                    ui.label(egui::RichText::new(tf("Archivos abiertos recientemente · {} elementos", &[&self.entries.len()])).size(12.0).color(pal.subtle));
                    if ui.add_enabled(!self.cfg.recent.is_empty(), egui::Button::new(t("Vaciar recientes")).small()).clicked() {
                        clear = true;
                    }
                });
                ui.add_space(2.0);
                if clear {
                    self.cfg.recent.clear();
                    self.cfg.save();
                    self.refresh();
                }
            }
            VMode::Trash => {
                let (mut restore, mut purge, mut empty) = (false, false, false);
                ui.horizontal_wrapped(|ui| {
                    ui.label(egui::RichText::new(tf("Papelera · {} elementos", &[&self.entries.len()])).size(12.0).color(pal.subtle));
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

    fn show_files(&mut self, ui: &mut egui::Ui) {
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
        let (cell_w, cell_h, row_pad) = if grid { ((width - 8.0) / cols as f32, zoom + 50.0, 8.0) } else { (0.0, if self.phone { 56.0 } else if self.touch { 48.0 } else { 32.0 }, 4.0) };
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
        let mut sa = egui::ScrollArea::vertical().id_salt("files").auto_shrink([false, false]).scroll_source(egui::scroll_area::ScrollSource { drag: egui::scroll_area::DragScroll::Never, ..Default::default() });
        let kin = self.kinetic(ui, total_h);
        if let Some(o) = kin {
            sa = sa.vertical_scroll_offset(o);
        }
        if reset_top {
            sa = sa.vertical_scroll_offset(0.0);
            self.kin_vel = 0.0;
        }
        let mut vis_center = 0usize;
        let out = sa.show_viewport(ui, |ui, vp| {
            // en táctil no reclamamos el arrastre: así el dedo desplaza la lista
            let sense = if self.touch { Sense::click() } else { Sense::click_and_drag() };
            let (rect, resp) = ui.allocate_exact_size(vec2(width, total_h.max(vp.height())), sense);
            area_rect = rect;
            let clip = ui.clip_rect();
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
                self.touch_press = self.touch_pt.and_then(|q| hit(q));
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
                if let Some(idx) = self.touch_press.take().and_then(|pos| self.order.get(pos).copied()) {
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
                    (Some(q), Some(pos)) => {
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
                match self.touch_press.take().and_then(|pos| self.order.get(pos).copied()) {
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
            let forced = self.shot_dbl == 1;
            if forced {
                self.shot_dbl = 2;
            }
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
                    let mut added = 0;
                    for p in self.selected_paths() {
                        if p.is_dir() && !self.cfg.bookmarks.contains(&p) {
                            self.cfg.bookmarks.push(p);
                            added += 1;
                        }
                    }
                    if added > 0 {
                        self.cfg.save();
                    }
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
                        self.do_move_to(t);
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
                if grid {
                    let card = r.shrink2(vec2(4.0, 2.0));
                    if sel || hov {
                        let fill = if sel { pal.sel } else { pal.hover.gamma_multiply(0.7) };
                        painter.rect_filled(card, CornerRadius::same(14), fill);
                        if (sel && !renaming) || drop_pos == Some(pos) {
                            painter.rect_stroke(card, CornerRadius::same(14), Stroke::new(1.5, pal.accent), egui::StrokeKind::Inside);
                        }
                    }
                    let icon_r = Rect::from_center_size(pos2(r.center().x, r.top() + 8.0 + zoom / 2.0), vec2(zoom, zoom));
                    self.paint_icon(&painter, icon_r, e, true, &mut want, pos);
                    if self.sel_mode || (self.touch && single) {
                        let c = pos2(card.left() + 18.0, card.top() + 18.0);
                        if sel {
                            painter.circle_filled(c, 10.0, pal.accent);
                            let st = Stroke::new(2.0, Color32::WHITE);
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
                        let mut job = egui::text::LayoutJob::simple(base_name(&e.name).to_string(), FontId::proportional(12.5), pal.text, cell_w - 16.0);
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
                            painter.rect_stroke(card, CornerRadius::same(9), Stroke::new(1.5, pal.accent), egui::StrokeKind::Inside);
                        }
                    }
                    let icon_r = Rect::from_center_size(pos2(r.left() + 24.0, r.center().y), vec2(24.0, 24.0));
                    self.paint_icon(&painter, icon_r, e, false, &mut want, pos);
                    if self.sel_mode {
                        // en selección múltiple el icono se cambia por la marca de selección
                        let c = icon_r.center();
                        painter.circle_filled(c, 12.0, if sel { pal.accent } else { pal.bg });
                        if sel {
                            let st = Stroke::new(2.0, Color32::WHITE);
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
                        let mut job = egui::text::LayoutJob::simple_singleline(base_name(&e.name).to_string(), FontId::proportional(13.5), pal.text);
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
                        if lc.cols.iter().any(|c| c.def.meta) && !self.extra_cache.contains_key(&key) {
                            if self.extra_cache.len() > 4000 {
                                self.extra_cache.clear();
                            }
                            self.extra_cache.insert(key.clone(), fsys::extra_meta(&key, e.is_dir()));
                        }
                        let ex = self.extra_cache.get(&key);
                        for pc in &lc.cols {
                            let txt = col_text(pc.def.id, e, ex);
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
                    let txt = tf("{} elemento(s)", &[&self.sel_count]);
                    let g = gp.layout_no_wrap(txt, FontId::proportional(13.0), Color32::WHITE);
                    let r = Rect::from_min_size(q + vec2(14.0, 14.0), g.size() + vec2(20.0, 12.0));
                    gp.rect_filled(r, CornerRadius::same(10), pal.accent);
                    gp.galley(r.min + vec2(10.0, 6.0), g, Color32::WHITE);
                }
            }
        });
        self.kin_off = out.state.offset.y;
        center_pos_cell = vis_center;
        self.hover_name = hover_pos_idx.and_then(|p| self.order.get(p)).and_then(|&i| entries.get(i as usize)).map(|e| e.name.to_string());
        // cola de miniaturas: más cerca del centro visible primero
        want.sort_by_key(|(pos, _)| pos.abs_diff(center_pos_cell));
        self.thumbs.set_wanted(want.into_iter().map(|(_, r)| r).collect());
        let _ = area_rect;
        if n == 0 && !self.loading && !hold {
            let r = ui.min_rect();
            let msg = if let Some(e) = &self.load_err {
                tf("No se puede abrir la carpeta: {e}", &[e])
            } else if !self.filter.is_empty() {
                t("Sin resultados").to_string()
            } else {
                t("Carpeta vacía").to_string()
            };
            ui.painter().text(r.center(), Align2::CENTER_CENTER, msg, FontId::proportional(15.0), pal.subtle);
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
        let (now, dt, pdelta, down, key, pad) = ui.input(|i| {
            let mut pad = 0.0f32;
            let mut wheel = false;
            let mut key = false;
            for e in &i.events {
                match e {
                    egui::Event::MouseWheel { unit: egui::MouseWheelUnit::Point, delta, .. } => pad -= delta.y,
                    egui::Event::MouseWheel { .. } => wheel = true,
                    egui::Event::Key { pressed: true, .. } => key = true,
                    _ => {}
                }
            }
            (i.time, i.unstable_dt.min(0.05), i.pointer.delta(), i.pointer.any_down(), key || wheel, pad)
        });
        // velocidad media de la última fracción de gesto, medida hasta `end` (instante de referencia)
        let fling = |hist: &[(f64, f32)], end: f64| -> f32 {
            let w = 0.12;
            let sum: f32 = hist.iter().filter(|(t, _)| end - t <= w).map(|(_, d)| d).sum();
            let v = (sum / w as f32 * 1.5).clamp(-9000.0, 9000.0);
            if v.abs() < 60.0 { 0.0 } else { v }
        };
        if self.touch {
            let r = ui.interact(area, egui::Id::new(("kin_drag", self.live_side)), Sense::drag());
            if r.dragged() {
                self.kin_vel = 0.0;
                self.kin_hist.retain(|(t, _)| now - t <= 0.1);
                self.kin_hist.push((now, -pdelta.y * 1.25));
                self.kin_off = (self.kin_off - pdelta.y * 1.25).clamp(0.0, max);
                return Some(self.kin_off);
            }
            if r.drag_stopped() {
                self.kin_vel = fling(&self.kin_hist, now);
                self.kin_hist.clear();
            }
        }
        // trackpad: acumulamos el gesto y al terminar lo dejamos deslizar
        if pad != 0.0 {
            self.kin_vel = 0.0;
            self.kin_hist.retain(|(t, _)| now - t <= 0.1);
            self.kin_hist.push((now, pad));
        } else if let Some(&(t, _)) = self.kin_hist.last() {
            if now - t > 0.05 {
                self.kin_vel = fling(&self.kin_hist, t);
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
            self.kin_vel *= (-1.7 * dt).exp();
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
                    ui.label(egui::RichText::new(tf("{} elemento(s)", &[&self.sel_count])).strong());
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
                        cmd = Some(if cfg!(target_os = "android") { Cmd::DeletePerm } else { Cmd::Trash });
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
        if self.paint_sys(p, r, e) {
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
                if ui.button(t("Vista previa   (Espacio)")).clicked() {
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
                } else if self.sel_count == 1 {
                    if is_program(&self.cwd.join(&*e.name)) {
                        let p = self.cwd.join(&*e.name);
                        if ui.button(t("Ejecutar")).clicked() {
                            self.run_program(&p);
                            ui.close();
                        }
                        if ui.button(t("Ejecutar en el terminal")).clicked() {
                            self.run_in_terminal(&p);
                            ui.close();
                        }
                    }
                    self.open_with_menu(ui);
                    if ui.button(t("Calcular hash…")).clicked() {
                        self.open_hash();
                        ui.close();
                    }
                }
                let Some(e) = self.entries.get(c as usize) else { return };
                if e.is_dir() {
                    let p = self.cwd.join(&*e.name);
                    let has = self.cfg.bookmarks.contains(&p);
                    if ui.button(if has { t("Quitar de marcadores") } else { t("Añadir a marcadores") }).clicked() {
                        self.toggle_bookmark(p);
                        ui.close();
                    }
                }
                ui.separator();
            }
            if ui.button(t("Copiar   (Ctrl+C)")).clicked() {
                self.do_copy(false);
                ui.close();
            }
            if ui.button(t("Cortar   (Ctrl+X)")).clicked() {
                self.do_copy(true);
                ui.close();
            }
        }
        if ui.add_enabled(!self.undo_stack.is_empty(), egui::Button::new(t("Deshacer   (Ctrl+Z)"))).clicked() {
            self.undo();
            ui.close();
        }
        if ui.add_enabled(self.clipboard.is_some(), egui::Button::new(t("Pegar   (Ctrl+V)"))).clicked() {
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
            if ui.button(t("Renombrar   (F2)")).clicked() {
                self.start_rename();
                ui.close();
            }
            if self.sel_count > 1 && ui.button(t("Renombrar en lote…")).clicked() {
                self.open_batch();
                ui.close();
            }
            if ui.button(t("Mover a la papelera   (Supr)")).clicked() {
                self.do_trash();
                ui.close();
            }
            if ui.button(t("Eliminar definitivamente")).clicked() {
                let v = self.selected_paths();
                self.confirm_delete = Some(v);
                ui.close();
            }
            ui.separator();
            if ui.button(t("Propiedades   (Alt+Intro)")).clicked() {
                self.open_props();
                ui.close();
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
            if ui.button(t("Abrir terminal aquí")).clicked() {
                self.terminal_here();
                ui.close();
            }
            if ui.button(t("Propiedades de la carpeta")).clicked() {
                self.open_props();
                ui.close();
            }
            if ui.button(t("Seleccionar todo   (Ctrl+A)")).clicked() {
                self.select_range(0, self.order.len().saturating_sub(1), false);
                ui.close();
            }
            if ui.button(t("Actualizar   (F5)")).clicked() {
                self.refresh();
                ui.close();
            }
        }
    }
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
    let c = ui.add(b).clicked();
    if c {
        ui.close();
    }
    c
}

fn mtime_secs(md: &std::fs::Metadata) -> i64 {
    md.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
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
        let date = d.mtimes.get(i).map(|&m| crate::fsys::fmt_time(m)).unwrap_or_default();
        expand_template(&d.template, stem, d.start as usize + i, date.get(..10).unwrap_or(""), &d.parent)
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
        s = format!("{} {:03}", s, d.start as usize + i);
    }
    if ext.is_empty() { s } else { format!("{s}.{ext}") }
}

#[cfg(unix)]
fn perm_bits(md: &std::fs::Metadata) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    md.permissions().mode() & 0o777
}

#[cfg(not(unix))]
fn perm_bits(md: &std::fs::Metadata) -> u32 {
    if md.permissions().readonly() { 0o444 } else { 0o666 }
}

/// Aplica permisos (rwx de propietario/grupo/otros); en carpetas puede propagarse al contenido
/// conservando en los archivos sus bits de ejecución.
#[cfg(unix)]
fn set_perms(p: &Path, bits: u32, recursive: bool) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let md = std::fs::symlink_metadata(p)?;
    if md.file_type().is_symlink() {
        return Ok(());
    }
    let old = md.permissions().mode();
    let new = if md.is_dir() { (old & !0o777) | bits } else { (old & !0o777) | bits };
    std::fs::set_permissions(p, std::fs::Permissions::from_mode(new))?;
    if recursive && md.is_dir() {
        for e in std::fs::read_dir(p)? {
            let e = e?;
            let path = e.path();
            let child = std::fs::symlink_metadata(&path)?;
            if child.is_dir() {
                set_perms(&path, bits, true)?;
            } else if !child.file_type().is_symlink() {
                // archivos: se cambian lectura/escritura, la ejecución se queda como estaba
                let cm = child.permissions().mode();
                let nb = (cm & !0o666) | (bits & 0o666);
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(nb))?;
            }
        }
    }
    Ok(())
}

#[cfg(not(unix))]
fn set_perms(p: &Path, bits: u32, recursive: bool) -> std::io::Result<()> {
    // en Windows sólo existe el atributo de sólo lectura (bit de escritura del propietario)
    let md = std::fs::metadata(p)?;
    let mut perm = md.permissions();
    perm.set_readonly(bits & 0o200 == 0);
    std::fs::set_permissions(p, perm)?;
    if recursive && md.is_dir() {
        for e in std::fs::read_dir(p)? {
            set_perms(&e?.path(), bits, true)?;
        }
    }
    Ok(())
}

#[cfg(unix)]
fn mode_string(md: &std::fs::Metadata) -> String {
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
fn mode_string(md: &std::fs::Metadata) -> String {
    if md.permissions().readonly() { t("Sólo lectura").into() } else { t("Lectura y escritura").into() }
}

/// ¿Es un ejecutable nativo (ELF) o un script con shebang con permiso de ejecución?
#[cfg(all(unix, not(target_os = "android")))]
fn is_program(p: &Path) -> bool {
    use std::io::Read;
    use std::os::unix::fs::PermissionsExt;
    let Ok(md) = std::fs::metadata(p) else { return false };
    if !md.is_file() || md.permissions().mode() & 0o111 == 0 {
        return false;
    }
    let mut head = [0u8; 4];
    let Ok(n) = std::fs::File::open(p).and_then(|mut f| f.read(&mut head)) else { return false };
    n >= 2
        && (head[..2] == *b"#!"
            || head == [0x7f, b'E', b'L', b'F']
            // Mach-O (macOS): 32/64 bits y binarios universales
            || head == [0xcf, 0xfa, 0xed, 0xfe]
            || head == [0xce, 0xfa, 0xed, 0xfe]
            || head == [0xca, 0xfe, 0xba, 0xbe])
}

/// Windows: ejecutables y scripts de lote.
#[cfg(windows)]
fn is_program(p: &Path) -> bool {
    let ext = p.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
    matches!(ext.as_str(), "exe" | "bat" | "cmd" | "com") && p.is_file()
}

#[cfg(not(any(windows, all(unix, not(target_os = "android")))))]
fn is_program(_: &Path) -> bool {
    false
}

fn shell_quote(p: &Path) -> String {
    #[cfg(windows)]
    {
        format!("cd /d \"{}\"\r", p.display())
    }
    #[cfg(not(windows))]
    {
        format!("cd '{}'\r", p.display().to_string().replace('\'', "'\\''"))
    }
}

impl App {
    // ---- pestañas ----

    /// Estado del panel que está en los campos vivos de `App`.
    fn pane_snapshot(&self) -> TabState {
        let sel = if self.sel_count > 0 && self.sel_count < 2000 { self.selected_names() } else { Vec::new() };
        TabState {
            cwd: self.cwd.clone(),
            back: self.back.clone(),
            fwd: self.fwd.clone(),
            filter: self.filter.clone(),
            sel,
            split: None,
            side: 0,
            vmode: self.vmode.clone(),
        }
    }

    /// Guarda en la pestaña actual la vista completa (uno o dos paneles).
    fn save_tab(&mut self) {
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
        self.filter = t.filter;
        self.load_mode(t.cwd, true, t.vmode);
        self.pending_select = t.sel;
        self.scroll_to_top();
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

    fn switch_tab(&mut self, i: usize) {
        if i == self.cur_tab || i >= self.tabs.len() {
            return;
        }
        self.save_tab();
        self.restore_tab(i);
    }

    fn step_tab(&mut self, d: isize) {
        let n = self.tabs.len() as isize;
        if n > 1 {
            self.switch_tab(((self.cur_tab as isize + d).rem_euclid(n)) as usize);
        }
    }

    fn new_tab_bg(&mut self, path: PathBuf) {
        self.save_tab();
        let at = self.cur_tab + 1;
        self.tabs.insert(at, TabState::new(path));
    }

    fn reopen_tab(&mut self) {
        if let Some(t) = self.closed_tabs.pop() {
            self.save_tab();
            let at = self.cur_tab + 1;
            self.tabs.insert(at, t);
            self.restore_tab(at);
        }
    }

    fn new_window(&self) {
        if let Ok(exe) = std::env::current_exe() {
            // la ventana nueva se abre desplazada para que no tape exactamente a la actual
            let pos = self.ctx.input(|i| i.viewport().outer_rect).map(|r| format!("{},{}", r.min.x + 40.0, r.min.y + 40.0));
            let mut cmd = std::process::Command::new(exe);
            cmd.arg("--new-window").arg(&self.cwd);
            if let Some(p) = pos {
                cmd.env("FB_POS", p);
            }
            let _ = cmd.spawn();
        }
    }

    /// Arrastre del sistema: al sacar la selección de la ventana pasa a ser un arrastre hacia otras aplicaciones.
    fn drag_out_tick(&mut self, ctx: &egui::Context, frame: &eframe::Frame) {
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
    fn drain_inbox(&mut self) {
        let items: Vec<PathBuf> = std::mem::take(&mut *self.inbox.lock().unwrap());
        if items.is_empty() {
            return;
        }
        for p in items {
            if p.is_dir() {
                self.new_tab(p);
            } else if let (Some(parent), Some(name)) = (p.parent().map(|x| x.to_path_buf()), p.file_name()) {
                let name = name.to_string_lossy().into_owned();
                self.new_tab(parent);
                self.pending_select = vec![name];
            }
        }
        self.ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
    }

    fn new_tab(&mut self, path: PathBuf) {
        self.save_tab();
        let at = self.cur_tab + 1;
        self.tabs.insert(at, TabState::new(path));
        self.restore_tab(at);
    }

    fn close_tab(&mut self, i: usize) {
        if self.tabs.len() <= 1 || i >= self.tabs.len() {
            return;
        }
        if i == self.cur_tab {
            self.save_tab();
            let t = self.tabs.remove(i);
            self.closed_tabs.push(t);
            let n = i.min(self.tabs.len() - 1);
            self.restore_tab(n);
        } else {
            self.save_tab();
            let t = self.tabs.remove(i);
            self.closed_tabs.push(t);
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
        self.tabs = vec![t];
        self.cur_tab = 0;
    }

    fn tab_title(&self, i: usize) -> String {
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

    fn tab_bar(&mut self, ui: &mut egui::Ui, max_w: f32) {
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
            let fill = if active {
                pal.sel
            } else if resp.hovered() || ui.input(|i| i.pointer.hover_pos()).map_or(false, |q| r.contains(q)) {
                pal.hover
            } else {
                pal.surface.gamma_multiply(0.55)
            };
            p.rect_filled(r, CornerRadius::same(9), fill);
            if active {
                p.rect_stroke(r, CornerRadius::same(9), Stroke::new(1.0, pal.accent.gamma_multiply(0.7)), egui::StrokeKind::Inside);
            }
            let can_close = n > 1;
            let tx_w = tab_w - if can_close { 52.0 } else { 28.0 };
            let mut title = self.tab_title(i);
            let gl = p.layout_no_wrap(title.clone(), font.clone(), pal.text);
            if gl.size().x > tx_w {
                let keep = ((tx_w / gl.size().x) * title.chars().count() as f32) as usize;
                title = title.chars().take(keep.saturating_sub(1)).collect::<String>() + "…";
            }
            let col = if active { pal.text } else { pal.subtle };
            let tex = if self.cfg.sys_icons { self.sys.folder(&self.ctx, None) } else { None };
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
                let xr = Rect::from_center_size(pos2(r.right() - 16.0, r.center().y), vec2(20.0, 20.0));
                let xresp = ui.interact(xr, id.with("x"), Sense::click());
                if xresp.hovered() {
                    p.rect_filled(xr, CornerRadius::same(6), pal.hover);
                }
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
                if mi(ui, t("Cerrar pestaña"), "Ctrl+W", n > 1) {
                    close = Some(i);
                }
                if mi(ui, t("Cerrar las demás"), "", n > 1) {
                    others = Some(i);
                }
            });
        }
        let pr = Rect::from_min_size(pos2(rect.left() + n as f32 * (tab_w + 4.0) + 2.0, rect.top() + 3.0), vec2(28.0, 28.0));
        let presp = ui.interact(pr, ui.id().with("tab_plus"), Sense::click());
        p.rect_filled(pr, CornerRadius::same(9), if presp.hovered() { pal.hover } else { pal.surface.gamma_multiply(0.55) });
        theme::draw_glyph(&p, pr.center(), 15.0, Glyph::Plus, pal.subtle);
        if presp.on_hover_text(t("Nueva pestaña (Ctrl+T)")).clicked() {
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

    // ---- terminal ----

    fn ensure_term(&mut self) {
        let dead = self.term.as_ref().map(|t| !t.alive()).unwrap_or(true);
        if dead {
            match Terminal::spawn(&self.ctx, &self.cwd, &self.cfg.shell) {
                Ok(t) => self.term = Some(t),
                Err(e) => {
                    self.term = None;
                    self.cfg.term_open = false;
                    self.say(tf("No se pudo abrir el terminal: {e}", &[&e]), true);
                }
            }
        }
    }

    fn toggle_terminal(&mut self) {
        if self.cfg.term_open {
            self.cfg.term_open = false;
            self.term_focus = false;
        } else {
            self.cfg.term_open = true;
            self.ensure_term();
            self.term_focus = self.term.is_some();
        }
        self.cfg.save();
    }

    fn terminal_here(&mut self) {
        self.cfg.term_open = true;
        self.ensure_term();
        let cmd = shell_quote(&self.cwd);
        if let Some(t) = &mut self.term {
            t.send(cmd.as_bytes());
            self.term_focus = true;
        }
        self.cfg.save();
    }

    fn terminal_panel(&mut self, ui: &mut egui::Ui) {
        let pal = self.pal;
        if self.term.as_ref().map(|t| !t.alive()).unwrap_or(true) {
            self.term = None;
            self.cfg.term_open = false;
            self.term_focus = false;
            return;
        }
        let mut close = false;
        let mut cd_here = false;
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(t("TERMINAL")).size(11.0).color(pal.subtle).strong());
            ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                if icon_button(ui, &pal, Glyph::Close, true, false, t("Cerrar (F12)")).clicked() {
                    close = true;
                }
                if ui.small_button(t("cd a la carpeta actual")).clicked() {
                    cd_here = true;
                }
            });
        });
        let (rect, resp) = ui.allocate_exact_size(ui.available_size(), Sense::click());
        if ui.input(|i| i.pointer.primary_pressed()) {
            let over = resp.hovered();
            self.term_focus = over;
            if over {
                if let Some(id) = ui.memory(|m| m.focused()) {
                    ui.memory_mut(|m| m.surrender_focus(id));
                }
            }
        }
        let focus = self.term_focus;
        if let Some(t) = &mut self.term {
            t.show(ui, rect, &pal, focus);
            if cd_here {
                t.send(shell_quote(&self.cwd).as_bytes());
            }
        }
        if close {
            self.cfg.term_open = false;
            self.term_focus = false;
            self.cfg.save();
        }
    }

    // ---- menú ----

    fn menu_items(&mut self, ui: &mut egui::Ui) {
        let has_sel = self.sel_count > 0;
        let one_zip = has_sel
            && self.sel_count == 1
            && self.cursor.and_then(|c| self.entries.get(c as usize)).map(|e| crate::archive::detect(Path::new(&*e.name)).is_some()).unwrap_or(false);
        {
            ui.menu_button(t("Archivo"), |ui| {
                ui.set_min_width(250.0);
                if mi(ui, t("Nueva pestaña"), "Ctrl+T", true) {
                    self.new_tab(self.cwd.clone());
                }
                if mi(ui, t("Cerrar pestaña"), "Ctrl+W", self.tabs.len() > 1) {
                    self.close_tab(self.cur_tab);
                }
                ui.separator();
                if mi(ui, t("Nueva carpeta"), t("Ctrl+Mayús+N"), true) {
                    self.new_folder();
                }
                if mi(ui, t("Nuevo archivo vacío…"), "", true) {
                    self.dlg = Dlg::NewFile(t("Nuevo archivo.txt").into());
                }
                ui.separator();
                if mi(ui, t("Abrir"), t("Enter"), has_sel) {
                    if let Some(c) = self.cursor.filter(|&c| self.is_sel(c)) {
                        self.open_index(c);
                    }
                }
                if mi(ui, t("Vista previa"), t("Espacio"), true) {
                    self.toggle_preview();
                }
                if self.sel_count == 1 {
                    self.open_with_menu(ui);
                } else {
                    ui.add_enabled(false, egui::Button::new(t("Abrir con")));
                }
                if mi(ui, t("Propiedades"), "Alt+Enter", true) {
                    self.open_props();
                }
                ui.separator();
                if mi(ui, t("Nueva ventana"), "Ctrl+N", true) {
                    self.new_window();
                }
                if mi(ui, t("Reabrir pestaña cerrada"), "Ctrl+Mayús+T", !self.closed_tabs.is_empty()) {
                    self.reopen_tab();
                }
                ui.separator();
                if mi(ui, t("Salir"), "Ctrl+Q", true) {
                    self.cfg.save();
                    self.ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            });
            ui.menu_button(t("Editar"), |ui| {
                ui.set_min_width(250.0);
                if mi(ui, t("Deshacer"), "Ctrl+Z", !self.undo_stack.is_empty()) {
                    self.undo();
                }
                if mi(ui, t("Rehacer"), "Ctrl+Y", !self.redo_stack.is_empty()) {
                    self.redo();
                }
                ui.separator();
                if mi(ui, t("Cortar"), "Ctrl+X", has_sel) {
                    self.do_copy(true);
                }
                if mi(ui, t("Copiar"), "Ctrl+C", has_sel) {
                    self.do_copy(false);
                }
                if mi(ui, t("Pegar"), "Ctrl+V", true) {
                    self.do_paste();
                }
                if mi(ui, t("Duplicar"), "", has_sel) {
                    self.do_duplicate();
                }
                if mi(ui, t("Copiar ruta"), "", has_sel) {
                    let t = self.selected_paths().iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join("\n");
                    self.ctx.copy_text(t);
                }
                ui.separator();
                if mi(ui, t("Renombrar"), "F2", has_sel) {
                    self.start_rename();
                }
                if mi(ui, t("Renombrar en lote…"), "", self.sel_count > 1) {
                    self.open_batch();
                }
                if mi(ui, t("Mover a la papelera"), t("Supr"), has_sel) {
                    self.do_trash();
                }
                if mi(ui, t("Eliminar definitivamente"), t("Mayús+Supr"), has_sel) {
                    self.confirm_delete = Some(self.selected_paths());
                }
                ui.separator();
                if mi(ui, t("Seleccionar todo"), "Ctrl+A", true) {
                    self.select_range(0, self.order.len().saturating_sub(1), false);
                }
                if mi(ui, t("Invertir selección"), "", true) {
                    self.invert_selection();
                }
            });
            ui.menu_button(t("Ver"), |ui| {
                ui.set_min_width(250.0);
                if mi_check(ui, t("Cuadrícula"), "", self.cfg.grid) {
                    self.set_view(1);
                }
                if mi_check(ui, t("Lista"), "", !self.cfg.grid && !self.cfg.col_view) {
                    self.set_view(0);
                }
                if mi_check(ui, t("Vista de columnas"), "", self.cfg.col_view) {
                    self.set_view(2);
                }
                ui.separator();
                if mi_check(ui, t("Vista dividida (dos paneles)"), "F9", self.other.is_some()) {
                    self.toggle_split();
                }
                if mi_check(ui, t("Paneles apilados (uno encima de otro)"), "", self.cfg.split_stack) {
                    self.cfg.split_stack = !self.cfg.split_stack;
                    self.cfg.save();
                }
                if mi_check(ui, t("Panel de vista previa"), "F3", self.cfg.preview_panel) {
                    self.cfg.preview_panel = !self.cfg.preview_panel;
                    self.cfg.save();
                }
                if mi_check(ui, t("Barra lateral"), "Ctrl+B", self.cfg.show_sidebar) {
                    self.cfg.show_sidebar = !self.cfg.show_sidebar;
                    self.cfg.save();
                }
                if mi_check(ui, t("Mostrar botón de menú (hamburguesa)"), "F10", self.cfg.show_menu_btn) {
                    self.cfg.show_menu_btn = !self.cfg.show_menu_btn;
                    self.cfg.save();
                }
                if mi_check(ui, t("Terminal"), "F12", self.cfg.term_open) {
                    self.toggle_terminal();
                }
                ui.separator();
                if mi_check(ui, t("Archivos ocultos"), "Ctrl+H", self.cfg.show_hidden) {
                    self.cfg.show_hidden = !self.cfg.show_hidden;
                    self.cfg.save();
                    self.spawn_sort();
                }
                if mi_check(ui, t("Miniaturas"), "", self.cfg.thumbs_on) {
                    self.cfg.thumbs_on = !self.cfg.thumbs_on;
                    self.cfg.save();
                }
                if mi_check(ui, t("Iconos del sistema"), "", self.cfg.sys_icons) {
                    self.cfg.sys_icons = !self.cfg.sys_icons;
                    self.cfg.save();
                }
                if mi_check(ui, t("Tema oscuro"), "", self.cfg.dark) {
                    self.set_dark(!self.cfg.dark);
                }
                ui.separator();
                if mi(ui, t("Actualizar"), "F5", true) {
                    self.refresh();
                }
            });
            ui.menu_button(t("Ir"), |ui| {
                ui.set_min_width(230.0);
                if mi(ui, t("Atrás"), "Alt+Left", !self.back.is_empty()) {
                    self.go_back();
                }
                if mi(ui, t("Adelante"), "Alt+Right", !self.fwd.is_empty()) {
                    self.go_forward();
                }
                if mi(ui, t("Carpeta superior"), t("Retroceso"), self.cwd.parent().is_some()) {
                    self.go_up();
                }
                if mi(ui, t("Carpeta personal"), "", self.home.is_some()) {
                    if let Some(h) = self.home.clone() {
                        self.navigate(h);
                    }
                }
                if mi(ui, t("Ir a la ruta…"), "Ctrl+L", true) {
                    self.addr_select = true;
                    self.addr_edit = Some(self.cwd.display().to_string());
                }
                if extras::trash_supported() && mi(ui, t("Papelera"), "", true) {
                    self.navigate(PathBuf::from(LOC_TRASH));
                }
                ui.separator();
                let bm = self.cfg.bookmarks.contains(&self.cwd);
                if mi(ui, if bm { t("Quitar marcador de esta carpeta") } else { t("Añadir esta carpeta a marcadores") }, "Ctrl+D", true) {
                    self.toggle_bookmark(self.cwd.clone());
                }
            });
            ui.menu_button(t("Herramientas"), |ui| {
                ui.set_min_width(260.0);
                if mi(ui, t("Buscar en subcarpetas…"), t("Ctrl+Mayús+F"), true) {
                    self.open_search();
                }
                if mi(ui, t("Filtrar esta carpeta"), "Ctrl+F", true) {
                    self.filter_focus = true;
                }
                ui.separator();
                if mi(ui, t("Comprimir…"), "", has_sel) {
                    self.do_zip();
                }
                if mi(ui, t("Calcular hash (MD5, SHA-1, SHA-256)…"), "", self.sel_count == 1) {
                    self.open_hash();
                }
                if mi(ui, t("Extraer aquí"), "", one_zip) {
                    self.do_unzip();
                }
                ui.separator();
                if mi(ui, t("Buscar archivos duplicados…"), "", true) {
                    self.open_dups();
                }
                ui.separator();
                if mi(ui, t("Abrir terminal aquí"), "", true) {
                    self.terminal_here();
                }
                if mi(ui, t("Calcular tamaño / Propiedades"), "Alt+Enter", true) {
                    self.open_props();
                }
                ui.separator();
                if mi(ui, t("Opciones…"), "", true) {
                    self.dlg = Dlg::Options;
                }
            });
            ui.menu_button(t("Ayuda"), |ui| {
                if mi(ui, t("Atajos de teclado"), "", true) {
                    self.dlg = Dlg::Keys;
                }
                if mi(ui, t("Acerca de"), "", true) {
                    self.dlg = Dlg::About;
                }
            });
        }
    }

    /// Decoración de ventana integrada: zona de arrastre con título + minimizar/maximizar/cerrar.
    fn window_controls(&mut self, ui: &mut egui::Ui, show_title: bool) {
        let pal = self.pal;
        let ctx = self.ctx.clone();
        let maximized = ctx.input(|i| i.viewport().maximized.unwrap_or(false));
        let btn_w = if self.cfg.native_deco { 0.0 } else { 40.0 };
        let w = (ui.available_width() - btn_w * 3.0 - 4.0).max(10.0);
        let (r, resp) = ui.allocate_exact_size(vec2(w, 34.0), Sense::click_and_drag());
        let title = format!(
            "{} — File Browser",
            self.dir_label(&self.cwd)
        );
        if show_title {
            ui.painter().text(r.center(), Align2::CENTER_CENTER, title, FontId::proportional(12.5), pal.subtle);
        }
        resp.context_menu(|ui| {
            if mi_check(ui, t("Mostrar botón de menú (hamburguesa)"), "", self.cfg.show_menu_btn) {
                self.cfg.show_menu_btn = !self.cfg.show_menu_btn;
                self.cfg.save();
            }
            if mi(ui, t("Nueva pestaña"), "Ctrl+T", true) {
                self.new_tab(self.cwd.clone());
            }
        });
        if resp.drag_started() {
            self.begin_window_drag(&ctx, None);
        }
        if resp.double_clicked() {
            ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(!maximized));
        }
        for k in 0..if self.cfg.native_deco { 0 } else { 3 } {
            let (br, bresp) = ui.allocate_exact_size(vec2(btn_w, 28.0), Sense::click());
            let danger = k == 2;
            if bresp.hovered() {
                let fill = if danger { Color32::from_rgb(210, 60, 60) } else { pal.hover };
                ui.painter().rect_filled(br.shrink2(vec2(3.0, 1.0)), CornerRadius::same(8), fill);
            }
            let col = if danger && bresp.hovered() { Color32::WHITE } else { pal.text };
            let g = match k {
                0 => Glyph::Minimize,
                1 if maximized => Glyph::Restore,
                1 => Glyph::Maximize,
                _ => Glyph::Close,
            };
            theme::draw_glyph(ui.painter(), br.center(), 18.0, g, col);
            if bresp.clicked() {
                match k {
                    0 => ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true)),
                    1 => ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(!maximized)),
                    _ => {
                        self.cfg.save();
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                }
            }
        }
    }

    /// Bordes de 5 px para redimensionar la ventana sin decoración.
    fn resize_edges(&self, ctx: &egui::Context) -> Option<egui::ResizeDirection> {
        use egui::{CursorIcon as C, ResizeDirection as D};
        if ctx.input(|i| i.viewport().maximized.unwrap_or(false) || i.viewport().fullscreen.unwrap_or(false)) {
            return None;
        }
        let r = ctx.content_rect();
        let q = ctx.input(|i| i.pointer.hover_pos())?;
        let m = 5.0;
        let (l, rt, t, b) = (q.x < r.left() + m, q.x > r.right() - m, q.y < r.top() + m, q.y > r.bottom() - m);
        let hit = match (l, rt, t, b) {
            (true, _, true, _) => Some((D::NorthWest, C::ResizeNwSe)),
            (_, true, true, _) => Some((D::NorthEast, C::ResizeNeSw)),
            (true, _, _, true) => Some((D::SouthWest, C::ResizeNeSw)),
            (_, true, _, true) => Some((D::SouthEast, C::ResizeNwSe)),
            (true, ..) => Some((D::West, C::ResizeHorizontal)),
            (_, true, ..) => Some((D::East, C::ResizeHorizontal)),
            (_, _, true, _) => Some((D::North, C::ResizeVertical)),
            (_, _, _, true) => Some((D::South, C::ResizeVertical)),
            _ => None,
        };
        if let Some((dir, cur)) = hit {
            ctx.set_cursor_icon(cur);
            if ctx.input(|i| i.pointer.primary_pressed()) {
                return Some(dir);
            }
        }
        None
    }

    /// Fila superior: botón de menú (☰), pestañas (si hay más de una), zona de arrastre y botones de ventana.
    fn title_bar(&mut self, ui: &mut egui::Ui) {
        let pal = self.pal;
        ui.horizontal(|ui| {
            ui.set_height(36.0);
            ui.spacing_mut().item_spacing.x = 4.0;
            let resp = if self.cfg.show_menu_btn {
                let (r, resp) = ui.allocate_exact_size(vec2(38.0, 34.0), Sense::click());
                if resp.hovered() || egui::Popup::is_id_open(ui.ctx(), egui::Popup::default_response_id(&resp)) {
                    ui.painter().rect_filled(r, CornerRadius::same(10), pal.hover);
                }
                theme::draw_glyph(ui.painter(), r.center(), 20.0, Glyph::Menu, pal.text);
                resp.on_hover_text(t("Menú"))
            } else {
                let rr = Rect::from_min_size(ui.cursor().min, vec2(1.0, 1.0));
                ui.interact(rr, ui.id().with("menu_anchor"), Sense::hover())
            };
            self.menu_id = Some(egui::Popup::default_response_id(&resp));
            egui::Popup::menu(&resp).show(|ui| {
                ui.set_min_width(210.0);
                self.menu_items(ui);
            });
            let multi = self.tabs.len() > 1;
            if multi {
                let ctl = if self.cfg.native_deco { 0.0 } else { 124.0 };
                let max_w = (ui.available_width() - ctl - 70.0).max(120.0);
                self.tab_bar(ui, max_w);
            }
            self.window_controls(ui, !multi);
        });
    }

    /// Tema del sistema: el de winit si lo informa, o el detectado por nosotros.
    fn system_dark(&self) -> Option<bool> {
        self.ctx.system_theme().map(|t| t == egui::Theme::Dark).or(match self.sys_theme.load(Ordering::Relaxed) {
            1 => Some(false),
            2 => Some(true),
            _ => None,
        })
    }

    fn apply_dark(&mut self, dark: bool) {
        self.cfg.dark = dark;
        self.cfg.sync_active();
        // el cambio es gradual: `tick_theme` va mezclando la paleta actual con la nueva
        self.theme_anim = Some((self.pal, theme::resolve(&self.cfg, dark), dark, Instant::now()));
        self.ctx.request_repaint();
    }

    /// Avanza la animación del cambio de tema claro/oscuro.
    fn tick_theme(&mut self) {
        let Some((from, to, dark, t0)) = self.theme_anim else { return };
        const DUR: f32 = 0.30;
        let k = (t0.elapsed().as_secs_f32() / DUR).min(1.0);
        let e = k * k * (3.0 - 2.0 * k); // suavizado
        self.pal = from.lerp(&to, e);
        theme::apply_style(&self.ctx, &self.pal, dark);
        if k >= 1.0 {
            self.theme_anim = None;
        } else {
            self.ctx.request_repaint();
        }
    }

    /// Pestaña «Temas» de Opciones: una lista con los dos de serie arriba, los predefinidos y los propios.
    /// Cada tema tiene dos casillas: si hace de tema claro y/o de tema oscuro. Los de serie nunca se pierden
    /// ni se editan; solo los propios tienen colores editables (los demás se duplican).
    fn themes_tab(&mut self, ui: &mut egui::Ui, width: f32) -> bool {
        let pal = self.pal;
        let mut changed = false;
        // (nombre, origen 0 serie / 1 predefinido / 2 propio, colores, clave en ranura clara, clave en ranura oscura)
        let mut rows: Vec<(String, u8, [[u8; 3]; 9], String, String)> = Vec::new();
        rows.push((t("Claro").to_string(), 0, theme::palette(false).colors(), String::new(), "@Claro".into()));
        rows.push((t("Oscuro").to_string(), 0, theme::palette(true).colors(), "@Oscuro".into(), String::new()));
        // los propios van justo tras los de serie, para que un tema nuevo se vea sin desplazar la lista
        for th in &self.cfg.custom_themes {
            rows.push((th.name.clone(), 2, th.colors, th.name.clone(), th.name.clone()));
        }
        for th in crate::theme::presets() {
            rows.push((th.name.clone(), 1, th.colors, th.name.clone(), th.name.clone()));
        }
        let mut mode: Option<u8> = None;
        let mut pick: Option<(bool, String)> = None;
        let mut dup: Option<usize> = None;
        let mut del: Option<String> = None;
        let mut edit: Option<String> = None;
        let mut create = false;

        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(t("Modo")).color(pal.subtle));
            for (m, l) in [(0u8, t("Sistema")), (1, t("Claro")), (2, t("Oscuro"))] {
                if ui.radio(self.cfg.theme_mode == m, l).clicked() {
                    mode = Some(m);
                }
            }
        });
        ui.add_space(2.0);
        ui.add(egui::Label::new(egui::RichText::new(t("Elige un tema claro y uno oscuro; «Sistema» alterna entre ellos. Los de serie no se pierden nunca.")).size(12.0).color(pal.subtle)).wrap());
        ui.add_space(8.0);

        // columnas: nombre | claro | oscuro | duplicar | eliminar
        let (cw, iw) = (56.0, 30.0);
        let list_w = width - 14.0;
        let name_w = list_w - 2.0 * cw - 2.0 * iw;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            ui.add_space(name_w);
            for l in [t("Claro"), t("Oscuro")] {
                let (r, _) = ui.allocate_exact_size(vec2(cw, 16.0), egui::Sense::hover());
                ui.painter().text(r.center(), Align2::CENTER_CENTER, l, egui::FontId::proportional(11.5), pal.subtle);
            }
        });
        let cur_slot_light = self.cfg.theme_light.clone();
        let cur_slot_dark = self.cfg.theme_dark.clone();
        egui::ScrollArea::vertical().max_height(300.0).auto_shrink([false, true]).show(ui, |ui| {
            ui.set_width(list_w);
            for i in 0..rows.len() {
                let (name, orig, colors, kl, kd) = rows[i].clone();
                if i > 0 && rows[i - 1].1 != orig {
                    ui.add_space(2.0);
                    ui.painter().hline(ui.available_rect_before_wrap().x_range(), ui.cursor().top(), Stroke::new(1.0, pal.border));
                    ui.add_space(4.0);
                }
                let editing = orig == 2 && self.edit_theme == name;
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
                    let (r, resp) = ui.allocate_exact_size(vec2(name_w, 34.0), egui::Sense::click());
                    if editing {
                        ui.painter().rect_filled(Rect::from_min_max(r.min, pos2(r.max.x + cw * 2.0 + iw * 2.0, r.max.y)), 8.0, pal.sel);
                    } else if resp.hovered() {
                        ui.painter().rect_filled(Rect::from_min_max(r.min, pos2(r.max.x + cw * 2.0 + iw * 2.0, r.max.y)), 8.0, pal.hover);
                    }
                    let mut x = r.min.x + 10.0;
                    for k in [0usize, 1, 2, 5, 7] {
                        let c = colors[k];
                        let sw = Rect::from_min_size(pos2(x, r.center().y - 7.0), vec2(14.0, 14.0));
                        ui.painter().rect_filled(sw, 4.0, Color32::from_rgb(c[0], c[1], c[2]));
                        ui.painter().rect_stroke(sw, 4.0, Stroke::new(1.0, pal.border), egui::StrokeKind::Inside);
                        x += 16.0;
                    }
                    ui.painter().text(pos2(x + 10.0, r.center().y), Align2::LEFT_CENTER, &name, egui::FontId::proportional(14.0), pal.text);
                    if orig == 2 && resp.clicked() {
                        edit = Some(name.clone());
                    }
                    for (dark, key, cur) in [(false, &kl, &cur_slot_light), (true, &kd, &cur_slot_dark)] {
                        let on = cur == key;
                        let (cr, cresp) = ui.allocate_exact_size(vec2(cw, 34.0), egui::Sense::click());
                        let cresp = cresp.on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_text(if dark { t("Usar en modo oscuro") } else { t("Usar en modo claro") });
                        let b = Rect::from_center_size(cr.center(), vec2(18.0, 18.0));
                        if on {
                            ui.painter().rect_filled(b, 5.0, pal.accent);
                            let st = Stroke::new(2.0, Color32::WHITE);
                            let c = b.center();
                            ui.painter().line_segment([c + vec2(-4.0, 0.5), c + vec2(-1.0, 3.5)], st);
                            ui.painter().line_segment([c + vec2(-1.0, 3.5), c + vec2(4.5, -3.0)], st);
                        } else {
                            ui.painter().rect_stroke(b, 5.0, Stroke::new(1.5, if cresp.hovered() { pal.accent } else { pal.border }), egui::StrokeKind::Inside);
                        }
                        if cresp.clicked() && !on {
                            pick = Some((dark, key.clone()));
                        }
                    }
                    let (dr, _) = ui.allocate_exact_size(vec2(iw, 34.0), egui::Sense::hover());
                    if theme::icon_button(&mut ui.new_child(egui::UiBuilder::new().max_rect(Rect::from_center_size(dr.center(), vec2(28.0, 28.0)))), &pal, theme::Icon::Copy, t("Duplicar")).clicked() {
                        dup = Some(i);
                    }
                    let (er, _) = ui.allocate_exact_size(vec2(iw, 34.0), egui::Sense::hover());
                    if orig == 2
                        && theme::icon_button(&mut ui.new_child(egui::UiBuilder::new().max_rect(Rect::from_center_size(er.center(), vec2(28.0, 28.0)))), &pal, theme::Icon::Trash, t("Eliminar tema")).clicked()
                    {
                        del = Some(name.clone());
                    }
                });
            }
        });
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if theme::dialog_button(ui, &pal, theme::Btn::Plain, t("Nuevo tema")).clicked() {
                create = true;
            }
        });

        // editor del tema propio seleccionado
        if let Some(i) = self.cfg.custom_themes.iter().position(|t| t.name == self.edit_theme) {
            ui.add_space(10.0);
            let mut edited = false;
            let mut rename: Option<String> = None;
            egui::Frame::new().fill(pal.surface).stroke(Stroke::new(1.0, pal.border)).corner_radius(CornerRadius::same(12)).inner_margin(egui::Margin::same(12)).show(ui, |ui| {
                ui.set_width(width - 26.0);
                let th = &mut self.cfg.custom_themes[i];
                ui.horizontal(|ui| {
                    ui.label(t("Nombre"));
                    let mut n = th.name.clone();
                    if ui.add(egui::TextEdit::singleline(&mut n).desired_width(220.0)).changed() {
                        rename = Some(n);
                    }
                });
                ui.add_space(6.0);
                let labels = [t("Fondo"), t("Panel lateral"), t("Superficie"), t("Resaltado"), t("Borde"), t("Texto"), t("Texto secundario"), t("Acento"), t("Fondo del visor")];
                egui::Grid::new("theme_colors").num_columns(3).spacing([18.0, 8.0]).show(ui, |ui| {
                    for (k, l) in labels.iter().enumerate() {
                        ui.horizontal(|ui| {
                            edited |= ui.color_edit_button_srgb(&mut th.colors[k]).changed();
                            ui.label(*l);
                        });
                        if k % 3 == 2 {
                            ui.end_row();
                        }
                    }
                });
            });
            if let Some(n) = rename {
                let n: String = n.chars().filter(|c| !matches!(c, '|' | '\n' | '\r' | '@')).take(40).collect();
                let taken = self.cfg.custom_themes.iter().any(|t| t.name == n) || crate::theme::presets().iter().any(|t| t.name == n);
                if !n.is_empty() && !taken {
                    let old = std::mem::replace(&mut self.cfg.custom_themes[i].name, n.clone());
                    if self.cfg.theme_light == old {
                        self.cfg.theme_light = n.clone();
                    }
                    if self.cfg.theme_dark == old {
                        self.cfg.theme_dark = n.clone();
                    }
                    self.edit_theme = n;
                    self.cfg.sync_active();
                    changed = true;
                }
            }
            if edited {
                self.refresh_custom_theme();
                changed = true;
            }
        } else {
            ui.add_space(6.0);
            ui.add(egui::Label::new(egui::RichText::new(t("Los temas de serie y predefinidos no se pueden modificar: duplícalos para editar sus colores.")).size(12.0).color(pal.subtle)).wrap());
        }

        if let Some(m) = mode {
            self.set_theme_mode(m);
        }
        if let Some((dark, key)) = pick {
            // con un modo fijo se pasa al de la casilla para que el tema elegido se vea; con «Sistema» solo se asigna
            if self.cfg.theme_mode != 0 && self.cfg.theme_mode != if dark { 2 } else { 1 } {
                self.cfg.theme_mode = if dark { 2 } else { 1 };
            }
            if dark {
                self.cfg.theme_dark = key;
            } else {
                self.cfg.theme_light = key;
            }
            let show = if self.cfg.theme_mode == 0 { self.cfg.dark } else { dark };
            self.apply_dark(show);
            changed = true;
        }
        let create_from: Option<(Option<usize>, bool)> = if let Some(i) = dup { Some((Some(i), self.cfg.dark)) } else if create { Some((None, self.cfg.dark)) } else { None };
        if let Some((src, side)) = create_from {
            if self.cfg.custom_themes.len() < 32 {
                let (colors, stem) = match src {
                    Some(i) => (rows[i].2, rows[i].0.clone()),
                    None => (self.pal.colors(), t("Tema").to_string()),
                };
                let mut k = 1;
                let name = loop {
                    let n = if src.is_some() { format!("{stem} {}", k + 1) } else { format!("{stem} {}", self.cfg.custom_themes.len() + k) };
                    let taken = self.cfg.custom_themes.iter().any(|t| t.name == n) || crate::theme::presets().iter().any(|t| t.name == n);
                    if !taken {
                        break n;
                    }
                    k += 1;
                };
                self.cfg.custom_themes.push(crate::config::CustomTheme { name: name.clone(), dark: side, colors });
                self.edit_theme = name.clone();
                if side {
                    self.cfg.theme_dark = name;
                } else {
                    self.cfg.theme_light = name;
                }
                let d = self.cfg.dark;
                self.apply_dark(d);
                changed = true;
            }
        }
        if let Some(name) = del {
            if let Some(i) = self.cfg.custom_themes.iter().position(|t| t.name == name) {
                self.cfg.custom_themes.remove(i);
                // las ranuras que lo usaban vuelven al tema de serie
                if self.cfg.theme_light == name {
                    self.cfg.theme_light.clear();
                }
                if self.cfg.theme_dark == name {
                    self.cfg.theme_dark.clear();
                }
                if self.edit_theme == name {
                    self.edit_theme.clear();
                }
                let d = self.cfg.dark;
                self.apply_dark(d);
                changed = true;
            }
        }
        if let Some(n) = edit {
            self.edit_theme = n;
        }
        changed
    }

    /// Elige el tema de la ranura clara u oscura ('' = el de serie) y, si es la del modo vigente, lo aplica.
    fn pick_theme(&mut self, dark_slot: bool, name: String) {
        if dark_slot {
            self.cfg.theme_dark = name;
        } else {
            self.cfg.theme_light = name;
        }
        if self.cfg.dark == dark_slot {
            self.apply_dark(dark_slot);
        }
        self.cfg.save();
    }

    /// Edición en vivo de los colores del tema propio activo.
    fn refresh_custom_theme(&mut self) {
        self.theme_anim = None;
        let dark = self.cfg.dark;
        self.pal = theme::resolve(&self.cfg, dark);
        theme::apply_style(&self.ctx, &self.pal, dark);
        self.ctx.request_repaint();
    }

    /// Tema explícito (claro u oscuro).
    fn set_dark(&mut self, dark: bool) {
        self.cfg.theme_mode = if dark { 2 } else { 1 };
        self.apply_dark(dark);
        self.cfg.save();
    }

    /// 0 = según el sistema, 1 = claro, 2 = oscuro.
    fn set_theme_mode(&mut self, mode: u8) {
        self.cfg.theme_mode = mode;
        let dark = match mode {
            1 => false,
            2 => true,
            _ => self.system_dark().unwrap_or(self.cfg.dark),
        };
        self.apply_dark(dark);
        self.cfg.save();
    }

    // ---- acciones de herramientas ----

    fn invert_selection(&mut self) {
        let order = self.order.clone();
        for i in order {
            let on = !self.is_sel(i);
            self.set_bit(i, on);
        }
        self.recount();
    }

    fn busy(&self) -> bool {
        self.op.as_ref().map(|o| !o.finished.load(Ordering::Acquire)).unwrap_or(false)
    }

    fn do_duplicate(&mut self) {
        let srcs = self.selected_paths();
        if srcs.is_empty() {
            return;
        }
        let dst = self.cwd.clone();
        self.start_op(Op::Copy { srcs, dst, mv: false, policy: Policy::Rename }, OpMeta::Copy { mv: false, replay: false });
    }

    /// Abre el diálogo de compresión con ZIP, el nombre habitual y el nivel normal.
    fn do_zip(&mut self) {
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
    fn open_archive(&mut self, p: PathBuf) {
        let Some(root) = archive_cache_dir(&p) else {
            fsys::open_external(&p);
            return;
        };
        if archive_marker(&root).exists() && root.is_dir() {
            if !self.arch.iter().any(|(r, _)| *r == root) {
                self.arch.push((root.clone(), p));
            }
            self.navigate(root);
            return;
        }
        let _ = std::fs::remove_dir_all(&root);
        self.start_op(Op::Unzip { src: p.clone(), dst: root.clone(), pass: None }, OpMeta::OpenArchive { archive: p, root });
    }

    /// Nombre corto de una carpeta; la raíz de un comprimido abierto se muestra con el nombre del archivo.
    /// Raíz del montaje gvfs que contiene `p` y su nombre legible.
    fn gvfs_mount_of(&self, p: &Path) -> Option<(PathBuf, String)> {
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

    fn dir_label(&self, p: &Path) -> String {
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
    fn arch_of(&self, cwd: &Path) -> Option<(PathBuf, PathBuf)> {
        self.arch.iter().find(|(r, _)| cwd.starts_with(r)).cloned()
    }

    /// Franja sobre la lista cuando se está dentro de un comprimido.
    fn arch_banner(&mut self, ui: &mut egui::Ui) {
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
                        let dst = fsys::unique_path(dir, &stem);
                        self.start_op(Op::Unzip { src: archive.clone(), dst, pass: None }, OpMeta::Generated);
                    }
                }
            });
        });
        ui.add_space(2.0);
    }

    fn do_unzip(&mut self) {
        let Some(src) = self.selected_paths().into_iter().next() else { return };
        let stem = src.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "extraido".into());
        let dst = fsys::unique_path(&self.cwd, &stem);
        self.start_op(Op::Unzip { src, dst, pass: None }, OpMeta::Generated);
    }

    fn open_props(&mut self) {
        let paths = if self.sel_count > 0 { self.selected_paths() } else { vec![self.cwd.clone()] };
        if paths.is_empty() {
            return;
        }
        let single = paths.len() == 1;
        let md = std::fs::symlink_metadata(&paths[0]).ok();
        let title = if single {
            paths[0].file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| paths[0].display().to_string())
        } else {
            tf("{} elementos", &[&paths.len()])
        };
        let location = paths[0].parent().map(|p| p.display().to_string()).unwrap_or_default();
        let kind = if single {
            match &md {
                Some(m) if m.is_dir() => t("Carpeta").to_string(),
                Some(m) if m.file_type().is_symlink() => t("Enlace simbólico").to_string(),
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
            title,
            location,
            kind,
            mtime: md.as_ref().map(mtime_secs).unwrap_or(0),
            mode: if single { md.as_ref().map(mode_string).unwrap_or_default() } else { String::new() },
            name: if single { paths[0].file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default() } else { String::new() },
            name_orig: if single { paths[0].file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default() } else { String::new() },
            time: md.as_ref().map(|m| fsys::fmt_time(mtime_secs(m))).unwrap_or_default(),
            time_orig: md.as_ref().map(|m| fsys::fmt_time(mtime_secs(m))).unwrap_or_default(),
            perm: md.as_ref().map(perm_bits).unwrap_or(0),
            perm_orig: md.as_ref().map(perm_bits).unwrap_or(0),
            recursive: false,
            has_dir: paths.iter().any(|p| p.is_dir()),
            readonly: md.as_ref().map(|m| m.permissions().readonly()).unwrap_or(false),
            err: String::new(),
            ok: false,
            paths,
            stats,
        });
    }

    /// Aplica los cambios hechos en la ventana de propiedades: permisos, fecha y nombre.
    fn apply_props(&mut self, d: &mut PropsDlg) {
        d.err.clear();
        d.ok = false;
        let mut changed = false;
        if d.perm != d.perm_orig {
            for p in &d.paths {
                if let Err(e) = set_perms(p, d.perm, d.recursive) {
                    d.err = format!("{}: {e}", t("No se pudieron cambiar los permisos"));
                    return;
                }
            }
            d.perm_orig = d.perm;
            d.mode.clear();
            if d.paths.len() == 1 {
                if let Ok(md) = std::fs::symlink_metadata(&d.paths[0]) {
                    d.mode = mode_string(&md);
                }
            }
            changed = true;
        }
        if d.time != d.time_orig {
            let Some(secs) = fsys::parse_time(&d.time) else {
                d.err = t("Fecha no válida (AAAA-MM-DD HH:MM:SS)").to_string();
                return;
            };
            for p in &d.paths {
                if let Err(e) = fsys::set_mtime(p, secs) {
                    d.err = format!("{}: {e}", t("No se pudo cambiar la fecha"));
                    return;
                }
            }
            d.mtime = secs;
            d.time = fsys::fmt_time(secs);
            d.time_orig = d.time.clone();
            changed = true;
        }
        if d.paths.len() == 1 && d.name != d.name_orig {
            let new = d.name.trim().to_string();
            if new.is_empty() || new.contains(['/', '\\']) || new == "." || new == ".." {
                d.err = t("Nombre no válido").to_string();
                return;
            }
            let src = d.paths[0].clone();
            let dst = src.with_file_name(&new);
            if dst.exists() {
                d.err = t("Ya existe un elemento con ese nombre").to_string();
                return;
            }
            if let Err(e) = std::fs::rename(&src, &dst) {
                d.err = format!("{}: {e}", t("No se pudo renombrar"));
                return;
            }
            d.paths[0] = dst;
            d.title = new.clone();
            d.name = new.clone();
            d.name_orig = new;
            changed = true;
        }
        if changed {
            d.ok = true;
            self.refresh();
        }
    }

    fn open_batch(&mut self) {
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
            names,
            find: String::new(),
            repl: String::new(),
            prefix: String::new(),
            suffix: String::new(),
            number: false,
            start: 1,
            case: 0,
        });
    }

    fn open_with(&mut self) {
        let Some(path) = self.selected_paths().into_iter().next() else { return };
        if path.is_dir() {
            return;
        }
        let apps = extras::apps_for(&path);
        self.dlg = Dlg::OpenWith(OpenWithDlg { path, apps, cmd: String::new(), custom_only: false });
    }

    /// Ejecuta un programa directamente, en su carpeta y sin ventana.
    fn run_program(&mut self, p: &Path) {
        let mut c = std::process::Command::new(p);
        c.current_dir(p.parent().unwrap_or(Path::new("/")))
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        if let Err(e) = c.spawn() {
            self.say(format!("{}: {e}", t("No se pudo abrir")), true);
        }
    }

    /// Ejecuta un programa dentro del terminal integrado (en Windows, en una consola nueva).
    fn run_in_terminal(&mut self, p: &Path) {
        #[cfg(windows)]
        {
            let _ = std::process::Command::new("cmd")
                .args(["/C", "start", "", "cmd", "/K"])
                .arg(p)
                .current_dir(p.parent().unwrap_or(Path::new("\\")))
                .spawn();
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
        let line = format!(
            "{}'{}'\r",
            shell_quote(&dir).trim_end_matches('\r').to_string() + " && ",
            p.display().to_string().replace('\'', "'\\''")
        );
        if let Some(t) = &mut self.term {
            t.send(line.as_bytes());
            self.term_focus = true;
        }
        self.cfg.save();
    }

    /// Submenú «Abrir con» del archivo seleccionado: aplicaciones asociadas y «Otra aplicación…».
    fn open_with_menu(&mut self, ui: &mut egui::Ui) {
        let Some(path) = self.selected_paths().into_iter().next() else { return };
        let mut run: Option<usize> = None;
        let mut other = false;
        ui.menu_button(t("Abrir con"), |ui| {
            ui.set_min_width(200.0);
            if self.ow_cache.as_ref().map(|(p, _)| p != &path).unwrap_or(true) {
                self.ow_cache = Some((path.clone(), extras::apps_for(&path)));
            }
            let apps = &self.ow_cache.as_ref().unwrap().1;
            egui::ScrollArea::vertical().max_height(320.0).auto_shrink([true, true]).show(ui, |ui| {
                for (i, a) in apps.iter().enumerate() {
                    if ui.button(&a.name).clicked() {
                        run = Some(i);
                        ui.close();
                    }
                }
            });
            if !apps.is_empty() {
                ui.separator();
            }
            if ui.button(t("Otra aplicación…")).clicked() {
                other = true;
                ui.close();
            }
        });
        if let Some(i) = run {
            if let Some((p, apps)) = &self.ow_cache {
                extras::launch(Some(&apps[i]), "", p);
            }
        } else if other {
            // En Windows, el selector nativo «Abrir con»; en el resto, un campo de comando.
            #[cfg(windows)]
            extras::launch(None, "", &path);
            #[cfg(not(windows))]
            {
                self.dlg = Dlg::OpenWith(OpenWithDlg { path, apps: Vec::new(), cmd: String::new(), custom_only: true });
            }
        }
    }

    fn open_hash(&mut self) {
        let Some(path) = self.selected_paths().into_iter().next() else { return };
        if path.is_dir() {
            return;
        }
        let job = extras::spawn_hash(path.clone(), self.ctx.clone());
        self.dlg = Dlg::Hash(HashDlg { path, job, cmp: String::new() });
    }

    fn open_dups(&mut self) {
        let root = self.cwd.clone();
        let job = extras::spawn_dups(root.clone(), self.ctx.clone());
        self.dlg = Dlg::Dups(DupsDlg { root, job });
    }

    /// Filas de la papelera que corresponden a la selección actual.
    fn trash_selected(&self) -> Vec<extras::TrashRow> {
        let rows = self.trash_rows.lock().unwrap();
        let mut v = Vec::new();
        self.for_each_selected(|i| {
            if let Some(r) = rows.get(i as usize) {
                v.push(r.clone());
            }
        });
        v
    }

    fn trash_ask_purge(&mut self) {
        let rows = self.trash_selected();
        if !rows.is_empty() {
            self.confirm_purge = Some(PurgeReq::Rows(rows));
        }
    }

    /// Restaura (`restore`) o elimina definitivamente `rows` (todas si `None`) en segundo plano.
    fn trash_run(&mut self, restore: bool, rows: Option<Vec<extras::TrashRow>>) {
        let (tx, rx) = std::sync::mpsc::channel();
        let ctx = self.ctx.clone();
        std::thread::spawn(move || {
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

    fn trash_poll(&mut self) {
        let Some(rx) = &self.trash_job else { return };
        let Ok(r) = rx.try_recv() else { return };
        self.trash_job = None;
        match r {
            Ok((restore, n)) => {
                let m = if restore { tf("{} elemento(s) restaurados", &[&n]) } else { tf("{} elemento(s) eliminados", &[&n]) };
                self.say(m, false);
            }
            Err(e) => self.say(e, true),
        }
        if self.vmode == VMode::Trash {
            self.refresh();
        } else if self.vmode == VMode::Dir {
            self.refresh(); // lo restaurado puede estar en esta carpeta
        }
    }

    fn purge_dialog(&mut self, ctx: &egui::Context) {
        let Some(req) = &self.confirm_purge else { return };
        let text = match req {
            PurgeReq::All => t("Se vaciará la papelera: todo se eliminará de forma permanente. No se puede deshacer.").to_string(),
            PurgeReq::Rows(v) => tf("Se eliminarán {} elemento(s) de forma permanente. No se puede deshacer.", &[&v.len()]),
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

    fn conflict_dialog(&mut self, ctx: &egui::Context) {
        let Some(c) = self.conflict.take() else { return };
        let mut choice: Option<Option<Policy>> = None; // Some(None) = cancelar
        let pal = self.pal;
        theme::modal_card(
            ctx,
            &pal,
            theme::Tone::Warn,
            t("El destino ya existe"),
            470.0,
            |ui| {
                ui.label(egui::RichText::new(tf("{} elemento(s) ya existen en el destino:", &[&c.names.len()])).color(pal.subtle));
                ui.add_space(4.0);
                for n in c.names.iter().take(6) {
                    ui.label(format!("•  {n}"));
                }
                if c.names.len() > 6 {
                    ui.label(egui::RichText::new(tf("… (+{} más)", &[&(c.names.len() - 6)])).color(pal.subtle));
                }
            },
            |ui| {
                if theme::dialog_button(ui, &pal, theme::Btn::Primary, t("Reemplazar")).clicked() {
                    choice = Some(Some(Policy::Replace));
                }
                if theme::dialog_button(ui, &pal, theme::Btn::Plain, t("Mantener ambos")).clicked() {
                    choice = Some(Some(Policy::Rename));
                }
                if theme::dialog_button(ui, &pal, theme::Btn::Plain, t("Omitir")).clicked() {
                    choice = Some(Some(Policy::Skip));
                }
                if theme::dialog_button(ui, &pal, theme::Btn::Plain, t("Cancelar")).clicked() {
                    choice = Some(None);
                }
            },
        );
        if ctx.input(|i| i.key_pressed(Key::Escape)) {
            choice = Some(None);
        }
        match choice {
            None => self.conflict = Some(c),
            Some(None) => {}
            Some(Some(policy)) => {
                let ConflictDlg { srcs, dst, mv, .. } = c;
                self.start_op(Op::Copy { srcs, dst, mv, policy }, OpMeta::Copy { mv, replay: false });
            }
        }
    }

    /// Abre una búsqueda en una pestaña nueva (en la carpeta actual o en todo el equipo).
    fn open_search(&mut self) {
        let q = self.filter.trim().to_string();
        self.open_search_tab(q, false);
    }

    fn open_search_tab(&mut self, query: String, all: bool) {
        let root = self.cwd.clone();
        self.new_tab(root.clone());
        self.sbuf = query.clone();
        self.search_focus = true;
        self.load_mode(root, false, VMode::Search { query, all, content: false });
    }

    fn search_all_from_filter(&mut self) {
        let q = self.filter.trim().to_string();
        self.filter.clear();
        self.spawn_sort();
        self.open_search_tab(q, true);
    }

    fn set_deep(&mut self, on: bool) {
        if virt_mode(&self.cwd).is_some() {
            return;
        }
        let p = self.cwd.clone();
        self.preview = None;
        self.load_mode(p, true, if on { VMode::Deep } else { VMode::Dir });
    }

    fn reveal(&mut self, path: &Path) {
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

    // ---- diálogos ----

    /// Pila de tareas (escritorio): la operación en curso y las que esperan en la cola.
    fn tasks_popup(&mut self, ctx: &egui::Context) {
        let Some(op) = self.op.clone() else {
            self.tasks_open = false;
            return;
        };
        ctx.request_repaint_after(Duration::from_millis(120));
        if !self.tasks_open {
            return;
        }
        if ctx.input(|i| i.key_pressed(Key::Escape)) {
            self.tasks_open = false;
            return;
        }
        let pal = self.pal;
        let elapsed = self.op_started.elapsed().as_secs_f64();
        let total = op.total.load(Ordering::Relaxed).max(1);
        let done = op.done.load(Ordering::Relaxed).min(total);
        let label = op.label.lock().unwrap().clone();
        let scanning = label == t("Calculando…");
        let bytes = matches!(self.op_meta, OpMeta::Copy { .. } | OpMeta::Generated);
        let detail = if scanning {
            String::new()
        } else if bytes {
            let speed = done as f64 / elapsed.max(0.001);
            let mut d = format!("{} / {}  ·  {}/s", fsys::human_size(done), fsys::human_size(total), fsys::human_size(speed as u64));
            if speed > 1.0 && done > 0 {
                let eta = ((total - done) as f64 / speed) as u64;
                let hms = if eta >= 3600 { format!("{}:{:02}:{:02}", eta / 3600, eta / 60 % 60, eta % 60) } else { format!("{}:{:02}", eta / 60, eta % 60) };
                d.push_str(&format!("  ·  {}", tf("quedan {}", &[&hms])));
            }
            d
        } else {
            tf("{} de {}", &[&done, &total])
        };
        let ppp = ctx.pixels_per_point();
        let inset_b = self.insets[3] / ppp;
        let width = (ctx.content_rect().width() - 28.0).clamp(240.0, 400.0);
        let mut cancel_cur = false;
        let mut drop_q: Option<usize> = None;
        let mut close = false;
        let desc = self.op_desc.clone();
        let queued: Vec<String> = self.queue.iter().map(|(o, _)| op_desc(o)).collect();
        let area = egui::Area::new(egui::Id::new("tasks_popup"))
            .order(egui::Order::Foreground)
            .anchor(Align2::RIGHT_BOTTOM, vec2(-14.0, -(36.0 + inset_b)))
            .show(ctx, |ui| {
                egui::Frame::new()
                    .fill(pal.panel)
                    .stroke(Stroke::new(1.0, pal.border))
                    .corner_radius(CornerRadius::same(12))
                    .inner_margin(egui::Margin::same(12))
                    .shadow(egui::epaint::Shadow { offset: [0, 4], blur: 16, spread: 0, color: Color32::from_black_alpha(90) })
                    .show(ui, |ui| {
                        ui.set_width(width);
                        ui.horizontal(|ui| {
                            ui.label(egui::RichText::new(t("Tareas en curso")).strong());
                            ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                                if ui.small_button(t("Ocultar")).clicked() {
                                    close = true;
                                }
                            });
                        });
                        ui.add_space(6.0);
                        ui.horizontal(|ui| {
                            ui.add(egui::Label::new(egui::RichText::new(&desc).strong()).truncate());
                            ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                                if ui.small_button(t("Cancelar")).clicked() {
                                    cancel_cur = true;
                                }
                            });
                        });
                        if scanning {
                            ui.add(egui::ProgressBar::new(0.0).desired_width(width).text(t("Calculando…")).animate(true).corner_radius(6));
                        } else {
                            ui.add(egui::ProgressBar::new(done as f32 / total as f32).desired_width(width).show_percentage().corner_radius(6));
                            ui.add_space(4.0);
                            ui.label(egui::RichText::new(detail).size(12.0).color(pal.subtle));
                            if !label.is_empty() {
                                ui.add(egui::Label::new(egui::RichText::new(label).size(12.0).color(pal.text)).truncate());
                            }
                        }
                        if !queued.is_empty() {
                            ui.add_space(8.0);
                            ui.separator();
                            ui.label(egui::RichText::new(t("En cola")).size(12.0).color(pal.subtle));
                            for (i, q) in queued.iter().enumerate() {
                                ui.horizontal(|ui| {
                                    ui.add(egui::Label::new(egui::RichText::new(format!("{}.  {q}", i + 1)).size(12.5)).truncate());
                                    ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                                        if ui.small_button(t("Cancelar")).clicked() {
                                            drop_q = Some(i);
                                        }
                                    });
                                });
                            }
                        }
                    });
            });
        // clic fuera de la pila: se cierra (el botón de la barra la alterna por su cuenta)
        let outside = ctx.input(|i| i.pointer.primary_clicked())
            && ctx.input(|i| i.pointer.interact_pos()).map(|p| !area.response.rect.contains(p) && p.y < ctx.content_rect().bottom() - 34.0).unwrap_or(false);
        if close || outside {
            self.tasks_open = false;
        }
        if let Some(i) = drop_q {
            self.queue.remove(i);
        }
        if cancel_cur {
            op.cancel.store(true, Ordering::Relaxed);
        }
    }

    /// Panel flotante con el progreso de la operación en curso (copiar, mover, borrar…).
    fn op_overlay(&mut self, ctx: &egui::Context) {
        if !self.phone {
            return self.tasks_popup(ctx);
        }
        let Some(op) = self.op.clone() else {
            self.op_card_hidden = false;
            return;
        };
        ctx.request_repaint_after(Duration::from_millis(120));
        if self.op_card_hidden {
            return;
        }
        let elapsed = self.op_started.elapsed().as_secs_f64();
        if elapsed < 0.4 {
            return; // las operaciones rápidas no necesitan panel
        }
        let pal = self.pal;
        let bytes = matches!(self.op_meta, OpMeta::Copy { .. } | OpMeta::Generated);
        let title = match self.op_meta {
            OpMeta::Copy { mv: false, .. } => t("Copiando"),
            OpMeta::Copy { mv: true, .. } => t("Moviendo"),
            OpMeta::Trash { .. } => t("Moviendo a la papelera"),
            _ => t("Trabajando…"),
        };
        let total = op.total.load(Ordering::Relaxed).max(1);
        let done = op.done.load(Ordering::Relaxed).min(total);
        let label = op.label.lock().unwrap().clone();
        let scanning = label == t("Calculando…");
        let frac = if scanning { 0.0 } else { done as f32 / total as f32 };
        let detail = if scanning {
            String::new()
        } else if bytes {
            let speed = done as f64 / elapsed.max(0.001);
            let mut d = format!("{} / {}  ·  {}/s", fsys::human_size(done), fsys::human_size(total), fsys::human_size(speed as u64));
            if speed > 1.0 && done > 0 {
                let eta = ((total - done) as f64 / speed) as u64;
                let hms = if eta >= 3600 { format!("{}:{:02}:{:02}", eta / 3600, eta / 60 % 60, eta % 60) } else { format!("{}:{:02}", eta / 60, eta % 60) };
                d.push_str(&format!("  ·  {}", tf("quedan {}", &[&hms])));
            }
            d
        } else {
            tf("{} de {}", &[&done, &total])
        };
        let ppp = ctx.pixels_per_point();
        let inset_b = self.insets[3] / ppp;
        let width = (ctx.content_rect().width() - 28.0).clamp(220.0, 380.0);
        let mut cancel = false;
        let mut hide = false;
        egui::Area::new(egui::Id::new("op_overlay"))
            .order(egui::Order::Foreground)
            .anchor(Align2::RIGHT_BOTTOM, vec2(-14.0, -(42.0 + inset_b)))
            .show(ctx, |ui| {
                egui::Frame::new()
                    .fill(pal.panel)
                    .stroke(Stroke::new(1.0, pal.border))
                    .corner_radius(CornerRadius::same(12))
                    .inner_margin(egui::Margin::same(12))
                    .shadow(egui::epaint::Shadow { offset: [0, 4], blur: 16, spread: 0, color: Color32::from_black_alpha(90) })
                    .show(ui, |ui| {
                        ui.set_width(width);
                        ui.horizontal(|ui| {
                            ui.label(egui::RichText::new(title).strong());
                            if !self.queue.is_empty() {
                                ui.label(egui::RichText::new(tf("(+{} en cola)", &[&self.queue.len()])).size(12.0).color(pal.subtle));
                            }
                            ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                                if ui.small_button(t("Cancelar")).clicked() {
                                    cancel = true;
                                }
                                if ui.small_button(t("Ocultar")).clicked() {
                                    hide = true;
                                }
                            });
                        });
                        ui.add_space(6.0);
                        if scanning {
                            ui.add(egui::ProgressBar::new(0.0).desired_width(width).text(t("Calculando…")).animate(true).corner_radius(6));
                        } else {
                            ui.add(egui::ProgressBar::new(frac).desired_width(width).show_percentage().corner_radius(6));
                            ui.add_space(4.0);
                            ui.label(egui::RichText::new(detail).size(12.0).color(pal.subtle));
                            if !label.is_empty() {
                                ui.add(egui::Label::new(egui::RichText::new(label).size(12.0).color(pal.text)).truncate());
                            }
                        }
                    });
            });
        if hide {
            self.op_card_hidden = true;
        }
        if cancel {
            op.cancel.store(true, Ordering::Relaxed);
        }
    }

    fn dialogs(&mut self, ctx: &egui::Context) {
        let pal = self.pal;
        // cerrar con tareas en curso: se pide confirmación
        if ctx.input(|i| i.viewport().close_requested()) && !self.quit_ok && (self.op.is_some() || !self.queue.is_empty()) {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.quit_ask = true;
        }
        if self.quit_ask {
            let n = self.queue.len() + self.op.is_some() as usize;
            let mut decision: Option<bool> = None;
            theme::modal_card(
                ctx,
                &pal,
                theme::Tone::Warn,
                t("Hay tareas en curso"),
                420.0,
                |ui| {
                    ui.label(egui::RichText::new(tf("Hay {} tarea(s) sin terminar. Si cierras ahora se interrumpirán.", &[&n.to_string()])).color(pal.subtle));
                },
                |ui| {
                    if theme::dialog_button(ui, &pal, theme::Btn::Danger, t("Cerrar de todos modos")).clicked() {
                        decision = Some(true);
                    }
                    if theme::dialog_button(ui, &pal, theme::Btn::Plain, t("Seguir trabajando")).clicked() {
                        decision = Some(false);
                    }
                },
            );
            if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
                decision = Some(false);
            }
            match decision {
                Some(true) => {
                    self.quit_ask = false;
                    self.quit_ok = true;
                    self.cfg.save();
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
                Some(false) => self.quit_ask = false,
                None => {}
            }
        }
        let mut dlg = std::mem::replace(&mut self.dlg, Dlg::None);
        let mut keep = true;
        match &mut dlg {
            Dlg::None => keep = false,
            Dlg::Props(d) => {
                let mut open = true;
                let (mut apply_now, mut close_now) = (false, false);
                egui::Window::new(t("Propiedades"))
                    .open(&mut open)
                    .collapsible(false)
                    .resizable(false)
                    .anchor(Align2::CENTER_CENTER, vec2(0.0, 0.0))
                    .show(ctx, |ui| {
                        ui.set_min_width(380.0);
                        ui.label(egui::RichText::new(&d.title).size(17.0).strong());
                        ui.add_space(6.0);
                        let st = &d.stats;
                        let bytes = st.bytes.load(Ordering::Relaxed);
                        let done = st.done.load(Ordering::Acquire);
                        egui::Grid::new("props").num_columns(2).spacing([16.0, 6.0]).show(ui, |ui| {
                            let row = |ui: &mut egui::Ui, k: &str, v: String| {
                                ui.label(egui::RichText::new(k).color(pal.subtle));
                                ui.label(v);
                                ui.end_row();
                            };
                            row(ui, t("Tipo"), d.kind.clone());
                            row(ui, t("Ubicación"), d.location.clone());
                            row(
                                ui,
                                t("Tamaño"),
                                tf("{} ({} bytes){}", &[&fsys::human_size(bytes), &bytes, &(if done { "" } else { t("  · calculando…") })]),
                            );
                            let (f, dr) = (st.files.load(Ordering::Relaxed), st.dirs.load(Ordering::Relaxed));
                            let dirs_inside = if d.paths.len() == 1 && d.kind == t("Carpeta") { dr.saturating_sub(1) } else { dr };
                            if d.paths.len() > 1 || d.kind == t("Carpeta") {
                                row(ui, t("Contenido"), tf("{f} archivos, {dirs_inside} carpetas", &[&f, &dirs_inside]));
                            }
                            if d.mtime > 0 {
                                row(ui, t("Modificado"), fsys::fmt_time(d.mtime));
                            }
                            if !d.mode.is_empty() {
                                row(ui, t("Permisos"), d.mode.clone());
                            }
                        });
                        ui.add_space(8.0);
                        ui.separator();
                        ui.add_space(4.0);
                        ui.label(egui::RichText::new(t("Editar").to_uppercase()).size(10.5).color(pal.subtle).strong());
                        egui::Grid::new("props_edit").num_columns(2).spacing([16.0, 8.0]).show(ui, |ui| {
                            if d.paths.len() == 1 {
                                ui.label(t("Nombre"));
                                ui.add(egui::TextEdit::singleline(&mut d.name).desired_width(260.0));
                                ui.end_row();
                            }
                            ui.label(t("Modificado"));
                            ui.add(egui::TextEdit::singleline(&mut d.time).hint_text("AAAA-MM-DD HH:MM:SS").desired_width(260.0));
                            ui.end_row();
                            #[cfg(unix)]
                            {
                                ui.label(t("Permisos"));
                                egui::Grid::new("props_perm").num_columns(4).spacing([14.0, 4.0]).show(ui, |ui| {
                                    ui.label("");
                                    ui.label(egui::RichText::new(t("Lectura")).color(pal.subtle));
                                    ui.label(egui::RichText::new(t("Escritura")).color(pal.subtle));
                                    ui.label(egui::RichText::new(t("Ejecución")).color(pal.subtle));
                                    ui.end_row();
                                    for (who, sh) in [(t("Propietario"), 6u32), (t("Grupo"), 3), (t("Otros"), 0)] {
                                        ui.label(who);
                                        for bit in [4u32, 2, 1] {
                                            let m = bit << sh;
                                            let mut on = d.perm & m != 0;
                                            if ui.checkbox(&mut on, "").changed() {
                                                if on { d.perm |= m } else { d.perm &= !m }
                                            }
                                        }
                                        ui.end_row();
                                    }
                                });
                                ui.end_row();
                            }
                            #[cfg(not(unix))]
                            {
                                ui.label(t("Permisos"));
                                let mut ro = d.perm & 0o200 == 0;
                                if ui.checkbox(&mut ro, t("Sólo lectura")).changed() {
                                    d.perm = if ro { 0o444 } else { 0o666 };
                                }
                                ui.end_row();
                            }
                        });
                        if d.has_dir {
                            ui.add_space(4.0);
                            ui.checkbox(&mut d.recursive, t("Aplicar también al contenido"));
                        }
                        if !d.err.is_empty() {
                            ui.add_space(4.0);
                            ui.colored_label(Color32::from_rgb(230, 90, 90), &d.err);
                        } else if d.ok {
                            ui.add_space(4.0);
                            ui.colored_label(pal.accent, t("Cambios aplicados"));
                        }
                        ui.add_space(8.0);
                        let dirty = d.name != d.name_orig || d.time != d.time_orig || d.perm != d.perm_orig;
                        ui.horizontal(|ui| {
                            if ui.add_enabled(dirty, egui::Button::new(t("Aplicar"))).clicked() {
                                apply_now = true;
                            }
                            if ui.button(t("Cerrar")).clicked() {
                                close_now = true;
                            }
                        });
                    });
                if apply_now {
                    self.apply_props(d);
                }
                if close_now {
                    open = false;
                }
                keep = open;
            }
            Dlg::NetLogin(l) => {
                let mut act = 0; // 1 conectar, 2 cancelar
                let smb = l.uri.trim().starts_with("smb://") || l.uri.trim().is_empty();
                egui::Window::new(t("Conectar a un servidor"))
                    .collapsible(false)
                    .resizable(false)
                    .anchor(Align2::CENTER_CENTER, vec2(0.0, 0.0))
                    .show(ctx, |ui| {
                        ui.set_min_width(380.0);
                        egui::Grid::new("netlogin").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                            ui.label(t("Dirección"));
                            let r = ui.add(
                                egui::TextEdit::singleline(&mut l.uri).hint_text("smb://nas/recurso   sftp://servidor").desired_width(260.0),
                            );
                            if l.uri.is_empty() && !r.has_focus() && !r.lost_focus() {
                                r.request_focus();
                            }
                            ui.end_row();
                            ui.add_enabled_ui(!l.anon, |ui| {
                                ui.label(t("Usuario"));
                            });
                            ui.add_enabled(!l.anon, egui::TextEdit::singleline(&mut l.user).desired_width(260.0));
                            ui.end_row();
                            if smb {
                                ui.add_enabled_ui(!l.anon, |ui| {
                                    ui.label(t("Dominio"));
                                });
                                ui.add_enabled(!l.anon, egui::TextEdit::singleline(&mut l.domain).desired_width(260.0));
                                ui.end_row();
                            }
                            ui.add_enabled_ui(!l.anon, |ui| {
                                ui.label(t("Contraseña"));
                            });
                            let r = ui.add_enabled(!l.anon, egui::TextEdit::singleline(&mut l.pass).password(true).desired_width(260.0));
                            if r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                                act = 1;
                            }
                            ui.end_row();
                        });
                        ui.checkbox(&mut l.anon, t("Acceso anónimo (invitado)"));
                        ui.add_space(8.0);
                        ui.horizontal(|ui| {
                            if ui.add_enabled(!l.uri.trim().is_empty(), egui::Button::new(t("Conectar"))).clicked() {
                                act = 1;
                            }
                            if ui.button(t("Cancelar")).clicked() {
                                act = 2;
                            }
                        });
                    });
                if ctx.input(|i| i.key_pressed(Key::Escape)) {
                    act = 2;
                }
                if act == 1 && !l.uri.trim().is_empty() {
                    let snapshot = NetLogin {
                        uri: l.uri.clone(),
                        user: l.user.clone(),
                        domain: l.domain.clone(),
                        pass: l.pass.clone(),
                        anon: l.anon,
                    };
                    self.start_mount(&snapshot);
                }
                keep = act == 0;
            }
            Dlg::Errors(list) => {
                let mut close = false;
                egui::Window::new(tf("Errores: {}", &[&list.len()]))
                    .collapsible(false)
                    .resizable(false)
                    .anchor(Align2::CENTER_CENTER, vec2(0.0, 0.0))
                    .show(ctx, |ui| {
                        ui.set_min_width(420.0);
                        egui::ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
                            for e in list.iter() {
                                ui.add(egui::Label::new(egui::RichText::new(e).size(12.5)).wrap());
                            }
                        });
                        ui.add_space(8.0);
                        if ui.button(t("Cerrar")).clicked() {
                            close = true;
                        }
                    });
                if ctx.input(|i| i.key_pressed(Key::Escape)) {
                    close = true;
                }
                keep = !close;
            }
            Dlg::NetShares(d) => {
                let mut pick: Option<usize> = None;
                let mut cancel = false;
                let host = d.base.split_once("://").map(|(_, r)| r.to_string()).unwrap_or_else(|| d.base.clone());
                egui::Window::new(tf("Recursos de {}", &[&host]))
                    .collapsible(false)
                    .resizable(false)
                    .anchor(Align2::CENTER_CENTER, vec2(0.0, 0.0))
                    .show(ctx, |ui| {
                        ui.set_min_width(340.0);
                        ui.label(egui::RichText::new(t("Elige el recurso que quieres montar")).color(pal.subtle));
                        ui.add_space(6.0);
                        egui::ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
                            for (i, s) in d.shares.iter().enumerate() {
                                if ui.add(egui::Button::new(s).min_size(vec2(320.0, 30.0))).clicked() {
                                    pick = Some(i);
                                }
                            }
                        });
                        ui.add_space(8.0);
                        if ui.button(t("Cancelar")).clicked() {
                            cancel = true;
                        }
                    });
                if ctx.input(|i| i.key_pressed(Key::Escape)) {
                    cancel = true;
                }
                if let Some(i) = pick {
                    let l = NetLogin {
                        uri: format!("{}/{}", d.base, d.shares[i]),
                        user: d.login.user.clone(),
                        domain: d.login.domain.clone(),
                        pass: d.login.pass.clone(),
                        anon: d.login.anon,
                    };
                    self.start_mount(&l);
                }
                keep = pick.is_none() && !cancel;
            }
            Dlg::Rename(old, text) => {
                let mut act = 0; // 1 aceptar, 2 cancelar
                egui::Window::new(t("Renombrar"))
                    .collapsible(false)
                    .resizable(false)
                    .anchor(Align2::CENTER_CENTER, vec2(0.0, 0.0))
                    .show(ctx, |ui| {
                        let r = ui.add(egui::TextEdit::singleline(text).desired_width(300.0));
                        if !r.has_focus() && !r.lost_focus() {
                            r.request_focus();
                        }
                        if r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                            act = 1;
                        }
                        ui.add_space(8.0);
                        ui.horizontal(|ui| {
                            if ui.button(t("Aceptar")).clicked() {
                                act = 1;
                            }
                            if ui.button(t("Cancelar")).clicked() {
                                act = 2;
                            }
                        });
                    });
                if ctx.input(|i| i.key_pressed(Key::Escape)) {
                    act = 2;
                }
                if act == 1 {
                    if let Some(idx) = self.entries.iter().position(|e| *e.name == **old) {
                        let new = text.clone();
                        self.commit_rename(idx as u32, new);
                    }
                }
                keep = act == 0;
            }
            Dlg::NewFile(name) => {
                let mut act = 0; // 1 crear, 2 cancelar
                egui::Window::new(t("Nuevo archivo"))
                    .collapsible(false)
                    .resizable(false)
                    .anchor(Align2::CENTER_CENTER, vec2(0.0, 0.0))
                    .show(ctx, |ui| {
                        let r = ui.add(egui::TextEdit::singleline(name).desired_width(300.0));
                        if !r.has_focus() && !r.lost_focus() {
                            r.request_focus();
                        }
                        let enter = r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                        ui.add_space(8.0);
                        ui.horizontal(|ui| {
                            if ui.button(t("Crear")).clicked() || enter {
                                act = 1;
                            }
                            if ui.button(t("Cancelar")).clicked() {
                                act = 2;
                            }
                        });
                    });
                if ctx.input(|i| i.key_pressed(Key::Escape)) {
                    act = 2;
                }
                if act == 1 {
                    let n = name.trim().to_string();
                    if n.is_empty() || n.contains(['/', '\\']) {
                        self.say(t("Nombre no válido").into(), true);
                    } else if self.remote_ro(&self.cwd.clone()) {
                    } else {
                        let cwd = self.cwd.clone();
                        self.say(t("Creando archivo…").into(), false);
                        self.spawn_io(
                            move || {
                                let p = fsys::unique_path(&cwd, &n);
                                std::fs::File::create(&p).map(|_| p)
                            },
                            |a, r| match r {
                                Ok(p) => {
                                    a.msg = None;
                                    a.push_undo(Undo::Created(vec![(p.clone(), false)]), true);
                                    if p.parent() == Some(a.cwd.as_path()) {
                                        a.pending_select = vec![p.file_name().unwrap().to_string_lossy().into_owned()];
                                        a.refresh();
                                    }
                                }
                                Err(e) => a.say(tf("No se pudo crear el archivo: {e}", &[&e]), true),
                            },
                        );
                    }
                }
                keep = act == 0;
            }
            Dlg::Batch(d) => {
                let re = if d.regex && !d.find.is_empty() { regex::Regex::new(&d.find).ok() } else { None };
                let bad_re = d.regex && !d.find.is_empty() && re.is_none();
                let new: Vec<String> = d.names.iter().enumerate().map(|(i, n)| batch_name(n, d, i, re.as_ref())).collect();
                let olds: HashSet<&str> = d.names.iter().map(|s| s.as_str()).collect();
                let mut seen: HashSet<&str> = HashSet::new();
                // por fila: ¿tiene problema? (vacío, repetido, con barra o ya existente)
                let mut bad: Vec<bool> = Vec::with_capacity(new.len());
                for (o, n) in d.names.iter().zip(&new) {
                    let b = bad_name(n) || !seen.insert(n.as_str()) || (n != o && !olds.contains(n.as_str()) && self.cwd.join(n).exists());
                    bad.push(b);
                }
                let conflict = bad.iter().any(|b| *b) || bad_re;
                let changes = d.names.iter().zip(&new).filter(|(o, n)| o != n).count();
                let mut act = 0;
                let mut open = true;
                let width = 560.0f32.min(ctx.content_rect().width() - 56.0).max(260.0);
                let fw = (width - 150.0).max(120.0);
                theme::card_window(ctx, &pal, t("Renombrar en lote"), 560.0, &mut open, |ui| {
                    egui::Grid::new("batch").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                        ui.label(t("Plantilla"));
                        ui.add(egui::TextEdit::singleline(&mut d.template).hint_text("{nombre} {n:3} {fecha} {padre}").desired_width(fw));
                        ui.end_row();
                        ui.label(t("Buscar"));
                        ui.add(egui::TextEdit::singleline(&mut d.find).desired_width(fw));
                        ui.end_row();
                        ui.label(t("Reemplazar por"));
                        ui.add(egui::TextEdit::singleline(&mut d.repl).hint_text(if d.regex { "$1" } else { "" }).desired_width(fw));
                        ui.end_row();
                        ui.label("");
                        ui.checkbox(&mut d.regex, t("Expresión regular"));
                        ui.end_row();
                        ui.label(t("Prefijo"));
                        ui.add(egui::TextEdit::singleline(&mut d.prefix).desired_width(fw));
                        ui.end_row();
                        ui.label(t("Sufijo"));
                        ui.add(egui::TextEdit::singleline(&mut d.suffix).desired_width(fw));
                        ui.end_row();
                        ui.label(t("Numerar"));
                        ui.horizontal(|ui| {
                            ui.add_enabled(d.template.trim().is_empty(), egui::Checkbox::new(&mut d.number, t("añadir contador")));
                            ui.add_enabled(d.number || !d.template.trim().is_empty(), egui::DragValue::new(&mut d.start).range(0..=99999).prefix(t("desde ")));
                        });
                        ui.end_row();
                        ui.label(t("Mayúsculas"));
                        ui.horizontal_wrapped(|ui| {
                            ui.selectable_value(&mut d.case, 0, t("sin cambio"));
                            ui.selectable_value(&mut d.case, 1, t("minúsculas"));
                            ui.selectable_value(&mut d.case, 2, t("MAYÚSCULAS"));
                            ui.selectable_value(&mut d.case, 3, t("Título"));
                        });
                        ui.end_row();
                    });
                    theme::section(ui, &pal, t("Vista previa"));
                    egui::ScrollArea::vertical().id_salt("batch_prev").min_scrolled_height(180.0).max_height(180.0).auto_shrink([false, false]).show(ui, |ui| {
                        for ((o, n), b) in d.names.iter().zip(&new).zip(&bad).take(300) {
                            let col = if *b { Color32::from_rgb(235, 100, 100) } else if o == n { pal.subtle } else { pal.text };
                            ui.add(egui::Label::new(egui::RichText::new(format!("{o}  ->  {n}")).size(12.5).color(col)).truncate());
                        }
                    });
                    if bad_re {
                        ui.colored_label(Color32::from_rgb(235, 100, 100), t("Expresión regular no válida."));
                    } else if conflict {
                        ui.colored_label(Color32::from_rgb(235, 100, 100), t("Hay nombres vacíos, repetidos o que ya existen."));
                    }
                    ui.add_space(12.0);
                    ui.allocate_ui_with_layout(vec2(width - 32.0, 34.0), egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.spacing_mut().item_spacing.x = 10.0;
                        ui.add_enabled_ui(!conflict && changes > 0, |ui| {
                            if theme::dialog_button(ui, &pal, theme::Btn::Primary, t("Renombrar")).clicked() {
                                act = 1;
                            }
                        });
                        if theme::dialog_button(ui, &pal, theme::Btn::Plain, t("Cancelar")).clicked() {
                            act = 2;
                        }
                    });
                });
                if !open {
                    act = 2;
                }
                if act == 1 && self.remote_ro(&self.cwd.clone()) {
                    act = 2;
                }
                if act == 1 {
                    let pairs: Vec<(String, String)> =
                        d.names.iter().cloned().zip(new.iter().cloned()).filter(|(o, n)| o != n).collect();
                    let mut err = None;
                    // fase 1: nombres temporales (permite intercambios y cadenas)
                    let pid = std::process::id();
                    let tmpn = |i: usize| format!(".fbtmp-{pid}-{i}");
                    let mut staged = 0;
                    for (i, (o, _)) in pairs.iter().enumerate() {
                        if let Err(e) = std::fs::rename(self.cwd.join(o), self.cwd.join(tmpn(i))) {
                            err = Some(format!("{o}: {e}"));
                            break;
                        }
                        staged = i + 1;
                    }
                    // fase 2: nombres finales; si algo falla se devuelve cada uno a su nombre original
                    let mut done = Vec::new();
                    if err.is_none() {
                        for i in 0..staged {
                            let dst = self.cwd.join(&pairs[i].1);
                            // nunca sobrescribir algo que haya aparecido entre la vista previa y ahora
                            let r = if dst.symlink_metadata().is_ok() {
                                Err(std::io::Error::new(std::io::ErrorKind::AlreadyExists, t("ya existe")))
                            } else {
                                std::fs::rename(self.cwd.join(tmpn(i)), &dst)
                            };
                            match r {
                                Ok(()) => done.push(i),
                                Err(e) => {
                                    err = Some(format!("{}: {e}", pairs[i].1));
                                    break;
                                }
                            }
                        }
                    }
                    if err.is_some() {
                        for &i in &done {
                            let _ = std::fs::rename(self.cwd.join(&pairs[i].1), self.cwd.join(tmpn(i)));
                        }
                        for i in 0..staged {
                            let back = self.cwd.join(&pairs[i].0);
                            let tmp = self.cwd.join(tmpn(i));
                            if std::fs::rename(&tmp, &back).is_err() && back.symlink_metadata().is_err() {
                                // último recurso: dejarlo con un nombre reconocible, nunca «.fbtmp»
                                let _ = std::fs::rename(&tmp, self.cwd.join(format!("{} (recuperado)", pairs[i].0)));
                            }
                        }
                    }
                    match err {
                        Some(e) => self.say(tf("Error al renombrar: {e}", &[&e]), true),
                        None => {
                            self.say(tf("{} elementos renombrados", &[&pairs.len()]), false);
                            let mv = pairs.iter().map(|(o, n)| (self.cwd.join(o), self.cwd.join(n))).collect();
                            self.push_undo(Undo::Moved(mv), true);
                        }
                    }
                    self.pending_select = pairs.into_iter().map(|(_, n)| n).collect();
                    self.refresh();
                }
                keep = act == 0;
            }
            Dlg::Options => {
                let mut open = true;
                let mut changed = false;
                let mut res_sort = false;
                let mut install_sys = false;
                let width = 540.0f32.min(ctx.content_rect().width() - 56.0).max(240.0) - 8.0;
                theme::card_window(ctx, &pal, t("Opciones"), 540.0, &mut open, |ui| {
                        let mut tab = self.opts_tab;
                        theme::tab_bar(ui, &pal, &mut tab, &[t("General"), t("Columnas"), t("Temas")]);
                        self.opts_tab = tab;
                        ui.add_space(8.0);
                        if self.opts_tab == 2 {
                            changed |= self.themes_tab(ui, width);
                            return;
                        }
                        if self.opts_tab == 1 {
                            ui.add(egui::Label::new(egui::RichText::new(t("Elige qué columnas ver en la vista de lista y su orden. Si la ventana es estrecha, se ocultan las menos importantes.")).color(pal.subtle)).wrap());
                            ui.add_space(6.0);
                            changed |= self.columns_editor(ui);
                            return;
                        }
                        theme::section(ui, &pal, t("Comportamiento"));
                        let mut single = self.cfg.single_click.unwrap_or(cfg!(target_os = "android"));
                        if ui.checkbox(&mut single, t("Abrir con un solo clic o toque")).changed() {
                            self.cfg.single_click = Some(single);
                            changed = true;
                        }
                        changed |= ui.checkbox(&mut self.cfg.restore_tabs, t("Restaurar las pestañas al iniciar")).changed();
                        if ui.checkbox(&mut self.cfg.index_enabled, t("Búsqueda indexada (instantánea)")).changed() {
                            changed = true;
                            if self.cfg.index_enabled {
                                index::start(tools::all_roots(), ctx.clone());
                            } else {
                                index::disable();
                            }
                        }
                        if self.cfg.index_enabled {
                            ui.horizontal(|ui| {
                                let txt = match index::status() {
                                    index::Status::Off => String::new(),
                                    index::Status::Building(n) => tf("Indexando… {} elementos", &[&n]),
                                    index::Status::Ready(n, secs) => tf("{} elementos · actualizado hace {} min", &[&n, &(secs / 60)]),
                                };
                                ui.label(egui::RichText::new(txt).size(12.0).color(pal.subtle));
                                if ui.small_button(t("Reindexar")).clicked() {
                                    index::rebuild(tools::all_roots(), ctx.clone());
                                }
                            });
                        }
                        changed |= ui.checkbox(&mut self.cfg.preview_window, t("Abrir el visor en una ventana aparte")).changed();
                        changed |= ui.checkbox(&mut self.cfg.native_deco, t("Usar la barra de título del sistema (requiere reiniciar)")).changed();
                        theme::section(ui, &pal, t("Vista"));
                        changed |= ui.checkbox(&mut self.cfg.sys_icons, t("Usar iconos del sistema")).changed();
                        changed |= ui.checkbox(&mut self.cfg.thumbs_on, t("Mostrar miniaturas de imágenes y vídeos")).changed();
                        let hid = ui.checkbox(&mut self.cfg.show_hidden, t("Mostrar archivos ocultos")).changed();
                        changed |= hid;
                        res_sort |= hid;
                        changed |= ui.checkbox(&mut self.cfg.show_sidebar, t("Mostrar barra lateral")).changed();
                        changed |= ui.checkbox(&mut self.cfg.clip_panel, t("Mostrar panel del portapapeles")).changed();
                        ui.horizontal(|ui| {
                            ui.label(t("Tamaño de iconos"));
                            changed |= ui.add(egui::Slider::new(&mut self.cfg.zoom, 64.0..=200.0).show_value(false)).changed();
                        });
                        theme::section(ui, &pal, t("Sistema"));
                        if system::can_install() {
                            if ui.button(t("Usar como gestor de archivos del sistema")).clicked() {
                                install_sys = true;
                            }
                        }
                        ui.add_space(6.0);
                        let mut auto = self.cfg.lang == "auto";
                        let mut lang_change = None;
                        if ui.checkbox(&mut auto, t("Usar el idioma del sistema")).changed() {
                            lang_change = Some(if auto { "auto".to_string() } else { crate::i18n::LANGS[crate::i18n::current()].0.to_string() });
                        }
                        ui.add_enabled_ui(!auto, |ui| {
                            ui.horizontal(|ui| {
                                ui.label(t("Idioma"));
                                let cur = crate::i18n::current();
                                egui::ComboBox::from_id_salt("lang_combo").selected_text(crate::i18n::LANGS[cur].1).show_ui(ui, |ui| {
                                    for (i, (code, name)) in crate::i18n::LANGS.iter().enumerate() {
                                        if ui.selectable_label(i == cur, *name).clicked() {
                                            lang_change = Some(code.to_string());
                                        }
                                    }
                                });
                            });
                        });
                        if let Some(l) = lang_change {
                            self.cfg.lang = l;
                            let li = crate::i18n::resolve(&self.cfg.lang);
                            crate::i18n::set(li);
                            crate::i18n::install_fonts(ctx, li);
                            changed = true;
                        }
                        ui.add_space(6.0);
                        ui.label(t("Intérprete del terminal (vacío = el predeterminado)"));
                        changed |= ui
                            .add(egui::TextEdit::singleline(&mut self.cfg.shell).hint_text(crate::terminal::default_shell()).desired_width(width - 20.0))
                            .changed();
                    });
                if res_sort {
                    self.spawn_sort();
                }
                if changed {
                    self.cfg.save();
                }
                if install_sys {
                    match system::install() {
                        Ok(_) => self.say(t("Registrado como gestor de archivos del sistema").into(), false),
                        Err(e) => self.say(format!("{}: {e}", t("No se pudo registrar")), true),
                    }
                }
                keep = open;
            }
            Dlg::About => {
                let mut open = true;
                egui::Window::new(t("Acerca de"))
                    .open(&mut open)
                    .collapsible(false)
                    .resizable(false)
                    .anchor(Align2::CENTER_CENTER, vec2(0.0, 0.0))
                    .show(ctx, |ui| {
                        ui.set_min_width(300.0);
                        ui.label(egui::RichText::new("File Browser").size(22.0).strong());
                        ui.label(egui::RichText::new(tf("Versión {}", &[&env!("CARGO_PKG_VERSION")])).color(pal.subtle));
                        ui.add_space(8.0);
                        ui.label(t("Gestor de archivos ligero y rápido, escrito en Rust con egui."));
                    });
                keep = open;
            }
            Dlg::OpenWith(d) => {
                let mut open = true;
                let mut pick: Option<usize> = None;
                let mut custom = false;
                egui::Window::new(t("Abrir con…"))
                    .open(&mut open)
                    .collapsible(false)
                    .resizable(false)
                    .anchor(Align2::CENTER_CENTER, vec2(0.0, 0.0))
                    .show(ctx, |ui| {
                        ui.set_min_width(340.0);
                        let name = d.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                        ui.label(egui::RichText::new(name).strong());
                        ui.add_space(6.0);
                        if d.apps.is_empty() && !d.custom_only {
                            ui.label(egui::RichText::new(t("No se encontraron aplicaciones")).color(pal.subtle));
                        }
                        egui::ScrollArea::vertical().max_height(260.0).show(ui, |ui| {
                            for (i, a) in d.apps.iter().enumerate() {
                                if ui.add(egui::Button::new(&a.name).min_size(vec2(320.0, 26.0))).clicked() {
                                    pick = Some(i);
                                }
                            }
                        });
                        ui.separator();
                        ui.label(t("Comando personalizado"));
                        ui.horizontal(|ui| {
                            let r = ui.add(egui::TextEdit::singleline(&mut d.cmd).hint_text(t("Comando…")).desired_width(240.0));
                            let enter = r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                            if ui.button(t("Abrir")).clicked() || enter {
                                custom = true;
                            }
                        });
                    });
                let mut done = false;
                if let Some(i) = pick {
                    extras::launch(Some(&d.apps[i]), "", &d.path);
                    done = true;
                } else if custom {
                    extras::launch(None, &d.cmd, &d.path);
                    done = true;
                }
                keep = open && !done;
            }
            Dlg::Compress(d) => {
                let mut open = true;
                let mut go = false;
                let mut cancel = false;
                let width = 460.0;
                theme::card_window(ctx, &pal, t("Comprimir"), width, &mut open, |ui| {
                    theme::section(ui, &pal, t("Formato"));
                    let mut sel = match d.fmt {
                        tools::ArchFmt::Zip => 0u8,
                        tools::ArchFmt::TarGz => 1,
                        tools::ArchFmt::SevenZ => 2,
                    };
                    theme::tab_bar(ui, &pal, &mut sel, &["ZIP", "TAR.GZ", "7Z"]);
                    d.fmt = [tools::ArchFmt::Zip, tools::ArchFmt::TarGz, tools::ArchFmt::SevenZ][sel as usize];
                    if !d.fmt.can_encrypt() {
                        d.protect = false;
                    }
                    theme::section(ui, &pal, t("Nombre del archivo"));
                    ui.horizontal(|ui| {
                        let r = ui.add(egui::TextEdit::singleline(&mut d.name).desired_width(width - 90.0));
                        if d.focus {
                            r.request_focus();
                            d.focus = false;
                        }
                        ui.label(egui::RichText::new(format!(".{}", d.fmt.ext())).color(pal.subtle));
                        if r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                            go = true;
                        }
                    });
                    theme::section(ui, &pal, t("Nivel de compresión"));
                    let lv = [(0u32, t("Sin compresión")), (1, t("Rápida")), (6, t("Normal")), (9, t("Máxima"))];
                    let cur = lv.iter().find(|(l, _)| *l == d.level).map(|(_, n)| *n).unwrap_or("");
                    egui::ComboBox::from_id_salt("compress_level").selected_text(cur).width(200.0).show_ui(ui, |ui| {
                        for (l, n) in lv {
                            ui.selectable_value(&mut d.level, l, n);
                        }
                    });
                    theme::section(ui, &pal, t("Contraseña"));
                    ui.add_enabled_ui(d.fmt.can_encrypt(), |ui| {
                        ui.checkbox(&mut d.protect, t("Proteger con contraseña"));
                    });
                    if !d.fmt.can_encrypt() {
                        ui.label(egui::RichText::new(t("TAR.GZ no admite contraseña.")).size(12.0).color(pal.subtle));
                    }
                    if d.protect {
                        ui.add_space(4.0);
                        ui.add(egui::TextEdit::singleline(&mut d.pw).password(!d.show).hint_text(t("Contraseña")).desired_width(width - 20.0));
                        ui.add(egui::TextEdit::singleline(&mut d.pw2).password(!d.show).hint_text(t("Repetir contraseña")).desired_width(width - 20.0));
                        ui.checkbox(&mut d.show, t("Mostrar"));
                        if !d.pw2.is_empty() && d.pw != d.pw2 {
                            ui.label(egui::RichText::new(t("Las contraseñas no coinciden")).size(12.0).color(Color32::from_rgb(226, 86, 86)));
                        }
                        ui.add(egui::Label::new(egui::RichText::new(t("Cifrado AES-256. Otros programas también lo abren.")).size(12.0).color(pal.subtle)).wrap());
                    }
                    ui.add_space(16.0);
                    let valid = !d.name.trim().is_empty() && (!d.protect || (!d.pw.is_empty() && d.pw == d.pw2));
                    ui.allocate_ui_with_layout(vec2(width, 34.0), egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.spacing_mut().item_spacing.x = 10.0;
                        ui.add_enabled_ui(valid, |ui| {
                            if theme::dialog_button(ui, &pal, theme::Btn::Primary, t("Comprimir")).clicked() {
                                go = true;
                            }
                        });
                        if theme::dialog_button(ui, &pal, theme::Btn::Plain, t("Cancelar")).clicked() {
                            cancel = true;
                        }
                    });
                    if !valid {
                        go = false;
                    }
                });
                if cancel {
                    open = false;
                }
                if go && open {
                    let clean: String = d.name.trim().chars().filter(|c| !matches!(c, '/' | '\\' | '\0')).collect();
                    let ext = format!(".{}", d.fmt.ext());
                    let stem = clean.strip_suffix(&ext).unwrap_or(&clean).to_string();
                    let out = fsys::unique_path(&self.cwd, &format!("{stem}{ext}"));
                    let opts = tools::CompressOpts { fmt: d.fmt, level: d.level, password: if d.protect { Some(d.pw.clone()) } else { None } };
                    let srcs = d.srcs.clone();
                    self.start_op(Op::Compress { srcs, out, opts }, OpMeta::Generated);
                    open = false;
                }
                keep = open;
            }
            Dlg::ArchPass(d) => {
                let mut open = true;
                let mut go = false;
                let mut cancel = false;
                let width = 400.0;
                let name = d.src.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                theme::card_window(ctx, &pal, t("Archivo protegido"), width, &mut open, |ui| {
                    ui.add(egui::Label::new(egui::RichText::new(tf("«{}» está protegido con contraseña.", &[&name])).color(pal.text)).wrap());
                    ui.add_space(8.0);
                    let r = ui.add(egui::TextEdit::singleline(&mut d.pass).password(!d.show).hint_text(t("Contraseña")).desired_width(width - 20.0));
                    if d.focus {
                        r.request_focus();
                        d.focus = false;
                    }
                    if r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                        go = true;
                    }
                    ui.checkbox(&mut d.show, t("Mostrar"));
                    if d.wrong {
                        ui.label(egui::RichText::new(t("Contraseña incorrecta")).size(12.0).color(Color32::from_rgb(226, 86, 86)));
                    }
                    ui.add_space(16.0);
                    ui.allocate_ui_with_layout(vec2(width, 34.0), egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.spacing_mut().item_spacing.x = 10.0;
                        ui.add_enabled_ui(!d.pass.is_empty(), |ui| {
                            if theme::dialog_button(ui, &pal, theme::Btn::Primary, t("Extraer")).clicked() {
                                go = true;
                            }
                        });
                        if theme::dialog_button(ui, &pal, theme::Btn::Plain, t("Cancelar")).clicked() {
                            cancel = true;
                        }
                    });
                });
                if cancel {
                    open = false;
                }
                if go && open && !d.pass.is_empty() {
                    let meta = std::mem::replace(&mut d.meta, OpMeta::Other);
                    let op = Op::Unzip { src: d.src.clone(), dst: d.dst.clone(), pass: Some(d.pass.clone()) };
                    self.start_op(op, meta);
                    open = false;
                } else if !open {
                    self.pending_select.clear();
                }
                keep = open;
            }
            Dlg::Hash(d) => {
                let mut open = true;
                egui::Window::new(t("Hash del archivo"))
                    .open(&mut open)
                    .collapsible(false)
                    .resizable(false)
                    .anchor(Align2::CENTER_CENTER, vec2(0.0, 0.0))
                    .show(ctx, |ui| {
                        ui.set_min_width(520.0);
                        ui.label(egui::RichText::new(d.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()).strong());
                        ui.add_space(6.0);
                        if !d.job.finished.load(Ordering::Acquire) {
                            let tt = d.job.total.load(Ordering::Relaxed).max(1);
                            let dn = d.job.done.load(Ordering::Relaxed);
                            ui.add(egui::ProgressBar::new((dn as f32 / tt as f32).clamp(0.0, 1.0)).corner_radius(6));
                            return;
                        }
                        let res = d.job.result.lock().unwrap().clone();
                        match res {
                            Some(Ok(h)) => {
                                egui::Grid::new("hashes").num_columns(3).spacing([12.0, 8.0]).show(ui, |ui| {
                                    for (n, v) in ["MD5", "SHA-1", "SHA-256"].iter().zip(h.iter()) {
                                        ui.label(egui::RichText::new(*n).strong());
                                        ui.label(egui::RichText::new(v).monospace().size(12.0));
                                        if ui.small_button(t("Copiar")).clicked() {
                                            ui.ctx().copy_text(v.clone());
                                        }
                                        ui.end_row();
                                    }
                                });
                                ui.add_space(8.0);
                                ui.horizontal(|ui| {
                                    ui.label(t("Comparar con:"));
                                    ui.add(egui::TextEdit::singleline(&mut d.cmp).desired_width(300.0));
                                });
                                let c = d.cmp.trim().to_lowercase();
                                if !c.is_empty() {
                                    if h.iter().any(|x| *x == c) {
                                        ui.label(egui::RichText::new(t("Coincide")).color(pal.accent));
                                    } else {
                                        ui.label(egui::RichText::new(t("No coincide")).color(Color32::from_rgb(235, 100, 100)));
                                    }
                                }
                            }
                            Some(Err(e)) => {
                                ui.label(egui::RichText::new(e).color(Color32::from_rgb(235, 100, 100)));
                            }
                            None => {}
                        }
                    });
                keep = open;
            }
            Dlg::Dups(d) => {
                let mut open = true;
                let mut show: Option<PathBuf> = None;
                let mut trash: Option<PathBuf> = None;
                egui::Window::new(t("Archivos duplicados"))
                    .open(&mut open)
                    .collapsible(false)
                    .default_size([680.0, 460.0])
                    .anchor(Align2::CENTER_CENTER, vec2(0.0, 0.0))
                    .show(ctx, |ui| {
                        let done = d.job.finished.load(Ordering::Acquire);
                        let groups: Vec<(u64, Vec<PathBuf>)> = d.job.groups.lock().unwrap().iter().take(300).cloned().collect();
                        let total = d.job.groups.lock().unwrap().len();
                        ui.label(
                            egui::RichText::new(tf(
                                "{} grupos · {} archivos revisados{}",
                                &[&total, &d.job.scanned.load(Ordering::Relaxed), &(if done { "" } else { t(" · buscando…") })],
                            ))
                            .size(12.0)
                            .color(pal.subtle),
                        );
                        ui.separator();
                        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                            for (gi, (size, files)) in groups.iter().enumerate() {
                                let title = tf("{} archivos de {}", &[&files.len(), &fsys::human_size(*size)]);
                                egui::CollapsingHeader::new(format!("{title}  ·  {}", files[0].file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()))
                                    .id_salt(("dupg", gi))
                                    .show(ui, |ui| {
                                        for f in files {
                                            ui.horizontal(|ui| {
                                                let rel = f.strip_prefix(&d.root).unwrap_or(f);
                                                ui.label(rel.display().to_string());
                                                ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                                                    if ui.small_button(t("Papelera")).clicked() {
                                                        trash = Some(f.clone());
                                                    }
                                                    if ui.small_button(t("Mostrar")).clicked() {
                                                        show = Some(f.clone());
                                                    }
                                                });
                                            });
                                        }
                                    });
                            }
                        });
                    });
                if let Some(f) = trash {
                    if crate::extras::trash_delete(&f).is_ok() {
                        let mut g = d.job.groups.lock().unwrap();
                        for grp in g.iter_mut() {
                            grp.1.retain(|x| *x != f);
                        }
                        g.retain(|grp| grp.1.len() > 1);
                        drop(g);
                        self.refresh();
                    }
                }
                if let Some(f) = &show {
                    if let Some(parent) = f.parent() {
                        self.navigate(parent.to_path_buf());
                        self.pending_select = vec![f.file_name().unwrap().to_string_lossy().into_owned()];
                    }
                }
                keep = open && show.is_none();
            }
            Dlg::Keys => {
                let mut open = true;
                let pal = self.pal;
                let mut start: Option<Cmd> = None;
                let mut reset: Option<Cmd> = None;
                let mut clear: Option<Cmd> = None;
                let mut reset_all = false;
                egui::Window::new(t("Atajos de teclado"))
                    .open(&mut open)
                    .collapsible(false)
                    .resizable(false)
                    .anchor(Align2::CENTER_CENTER, vec2(0.0, 0.0))
                    .show(ctx, |ui| {
                        ui.label(egui::RichText::new(t("Pulsa un atajo para cambiarlo. Con Esc se cancela.")).color(pal.subtle));
                        ui.add_space(4.0);
                        egui::ScrollArea::vertical().max_height(440.0).show(ui, |ui| {
                            egui::Grid::new("keys").num_columns(3).spacing([18.0, 6.0]).show(ui, |ui| {
                                for info in commands::ALL {
                                    ui.label(t(info.label));
                                    let sc = self.shortcut_text(info.cmd);
                                    if info.rebind {
                                        let waiting = self.rebinding == Some(info.cmd);
                                        let txt = if waiting {
                                            t("Pulsa una combinación…").to_string()
                                        } else if sc.is_empty() {
                                            "—".to_string()
                                        } else {
                                            sc.clone()
                                        };
                                        if ui.add_sized([170.0, 24.0], egui::Button::new(txt).selected(waiting)).clicked() {
                                            start = Some(info.cmd);
                                        }
                                        ui.horizontal(|ui| {
                                            if ui.small_button(t("Quitar")).clicked() {
                                                clear = Some(info.cmd);
                                            }
                                            if ui.small_button(t("Restablecer")).clicked() {
                                                reset = Some(info.cmd);
                                            }
                                        });
                                    } else {
                                        ui.label(egui::RichText::new(sc).color(pal.subtle));
                                        ui.label("");
                                    }
                                    ui.end_row();
                                }
                            });
                        });
                        ui.add_space(6.0);
                        ui.label(egui::RichText::new(t("Fijos: Enter abrir · Clic central: carpeta en pestaña nueva · teclear: ir al nombre")).color(pal.subtle).size(12.0));
                        if ui.button(t("Restablecer todos")).clicked() {
                            reset_all = true;
                        }
                    });
                if let Some(c) = start {
                    self.rebinding = Some(c);
                }
                if let Some(c) = clear {
                    self.set_binding(c, None);
                }
                if let Some(c) = reset {
                    let id = commands::info(c).id;
                    self.cfg.keys.retain(|(k, _)| k != id);
                    self.cfg.save();
                }
                if reset_all {
                    self.cfg.keys.clear();
                    self.cfg.save();
                }
                if !open {
                    self.rebinding = None;
                }
                keep = open;
            }
        }
        if keep && matches!(self.dlg, Dlg::None) {
            self.dlg = dlg;
        }
    }
}

impl App {
    /// Autotest opcional: FB_SHOT=ruta.png [FB_PREVIEW=n] [FB_WAIT=ms] [FB_SCROLL=filas]
    fn selftest(&mut self, ctx: &egui::Context) {
        let Ok(path) = std::env::var("FB_SHOT") else { return };
        let wait: u128 = std::env::var("FB_WAIT").ok().and_then(|v| v.parse().ok()).unwrap_or(2500);
        let t0 = *self.start.get_or_insert_with(Instant::now);
        let el = t0.elapsed().as_millis();
        ctx.request_repaint_after(Duration::from_millis(100));
        if el > 600 && self.preview.is_none() && !self.shot_prev && std::env::var("FB_PREVIEW").is_ok() {
            self.shot_prev = true;
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
        if el > 800 && self.op.is_none() && self.queue.is_empty() && !self.shot_prev && std::env::var_os("FB_TASKS").is_some() {
            // prueba del centro de tareas: tres copias seguidas de la carpeta actual
            self.shot_prev = true;
            let dst = PathBuf::from(std::env::var("FB_TASKS").unwrap_or_default());
            let _ = std::fs::create_dir_all(&dst);
            let srcs: Vec<PathBuf> = self.entries.iter().map(|e| self.cwd.join(&*e.name)).collect();
            for _ in 0..3 {
                self.start_op(Op::Copy { srcs: srcs.clone(), dst: dst.clone(), mv: false, policy: Policy::Rename }, OpMeta::Other);
            }
            self.tasks_open = true;
        }
        if el > 900 && self.shot_dbl == 0 && std::env::var_os("FB_DBL").is_some() {
            self.shot_dbl = 1; // simula doble clic sobre el primer elemento
        }
        if el > 400 && std::env::var("FB_TERM").is_ok() && self.term.is_none() {
            self.toggle_terminal();
            if let Some(t) = &mut self.term {
                t.send(b"ls --color=always | head -5; echo hola\r");
            }
        }
        if el > 500 && std::env::var("FB_DLG").is_ok() && matches!(self.dlg, Dlg::None) && !self.shot_prev {
            self.shot_prev = true;
            match std::env::var("FB_DLG").unwrap().as_str() {
                "opts" => self.dlg = Dlg::Options,
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
                "palette" => self.palette = Some(PaletteState { query: "pes".into(), sel: 1 }),
                "conflict" => {
                    let a = self.cwd.join("a.txt");
                    let sub = self.cwd.join("sub");
                    let _ = std::fs::create_dir_all(&sub);
                    let _ = std::fs::write(sub.join("a.txt"), b"x");
                    let _ = std::fs::write(sub.join("b.txt"), b"x");
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
                "themeanim" => self.set_dark(!self.cfg.dark),
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
                    let _ = std::fs::write(&a, b"x");
                    let ok = std::fs::rename(&a, &b).is_ok();
                    self.push_undo(Undo::Rename { from: a.clone(), to: b.clone() }, true);
                    eprintln!("[test] renamed={ok}");
                    self.undo();
                    eprintln!("[test] after undo: a={} b={}", a.exists(), b.exists());
                    self.redo();
                    eprintln!("[test] after redo: a={} b={}", a.exists(), b.exists());
                    let _ = std::fs::remove_file(&a);
                    let _ = std::fs::remove_file(&b);
                }
                "gear" => {
                    if let Some(id) = self.gear_id {
                        egui::Popup::toggle_id(ctx, id);
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
                    self.new_tab(if c.is_dir() { c } else { self.cwd.clone() });
                }
                _ => self.open_search(),
            }
        }
        if el > wait && !self.shot_req {
            self.shot_req = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::default()));
        }
        let img = ctx.input(|i| {
            i.events.iter().find_map(|e| match e {
                egui::Event::Screenshot { image, .. } => Some(image.clone()),
                _ => None,
            })
        });
        if let Some(img) = img {
            if let Ok(st) = std::fs::read_to_string("/proc/self/status") {
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
        if !self.traced_ui {
            self.traced_ui = true;
            if std::env::var_os("FB_SHOT").is_some() {
                eprintln!("[trace] primer fotograma: {:?}", crate::T0.get().map(|t| t.elapsed()));
            }
        }
        // al volver de otra aplicación: forzar repintado (el compositor deja de avisar de fotogramas a las
        // ventanas ocultas) y resincronizar el vídeo de la vista previa
        let focused = ctx.input(|i| i.viewport().focused.unwrap_or(true));
        if focused && !self.was_focused {
            ctx.request_repaint();
            ctx.request_repaint_after(Duration::from_millis(60));
            ctx.request_repaint_after(Duration::from_millis(250));
            if let Some(p) = self.preview.as_mut().and_then(|pv| pv.player.as_mut()) {
                p.resync();
            }
        }
        self.was_focused = focused;
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
            index::tick(tools::all_roots(), &ctx);
            if matches!(index::status(), index::Status::Building(_)) {
                ctx.request_repaint_after(std::time::Duration::from_millis(500));
            }
        }
        self.drain_inbox();
        if self.cfg.theme_mode == 0 {
            if let Some(dark) = self.system_dark() {
                if dark != self.cfg.dark {
                    self.apply_dark(dark);
                    self.cfg.save();
                }
            }
        }
        // botones laterales del ratón: atrás / adelante
        let (mb_back, mb_fwd) = ctx.input(|i| (i.pointer.button_pressed(egui::PointerButton::Extra1), i.pointer.button_pressed(egui::PointerButton::Extra2)));
        if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::BrowserBack)) || mb_back {
            self.on_back_key();
        } else if mb_fwd && self.preview.is_none() && matches!(self.dlg, Dlg::None) {
            self.run_cmd(Cmd::Forward);
        }
        if !self.touch && ctx.input(|i| i.events.iter().any(|e| matches!(e, egui::Event::Touch { .. }))) {
            self.touch = true;
        }
        self.poll_op();
        if self.net_check.elapsed() > Duration::from_secs(5) {
            self.net_check = Instant::now();
            self.drives = list_drives();
            self.refresh_network();
        }
        if let Some(v) = self.net_list.lock().unwrap().take() {
            self.network = v;
        }
        let nerr = self.net_err.lock().unwrap().take();
        if let Some(m) = nerr {
            self.say(tf("No se pudo conectar: {}", &[&m]), true);
        }
        let shares = self.net_shares.lock().unwrap().take();
        if let Some(sh) = shares {
            self.msg = None;
            self.dlg = Dlg::NetShares(sh);
        }
        let nav = self.net_nav.lock().unwrap().take();
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
        let sz = ctx.content_rect().size();
        if (sz.x - self.cfg.win.0).abs() > 1.0 || (sz.y - self.cfg.win.1).abs() > 1.0 {
            self.cfg.win = (sz.x, sz.y);
        }

        // trabajos de E/S terminados en segundo plano
        let done: Vec<_> = std::mem::take(&mut *self.io_done.lock().unwrap());
        for f in done {
            f(self);
        }

        // vigilancia de cambios en el directorio: el `stat` se hace en un hilo (en red puede tardar)
        if self.last_check.elapsed() > Duration::from_millis(1500) && !self.watch_busy.swap(true, Ordering::Relaxed) {
            self.last_check = Instant::now();
            let mut paths = vec![self.cwd.clone()];
            if let Some(o) = self.other.as_deref() {
                paths.push(o.cwd.clone());
            }
            let (slot, busy, ctx2) = (self.watch_res.clone(), self.watch_busy.clone(), ctx.clone());
            std::thread::spawn(move || {
                let r: Vec<_> = paths.into_iter().map(|p| {
                    let m = std::fs::metadata(&p).and_then(|m| m.modified()).ok();
                    (p, m)
                }).collect();
                *slot.lock().unwrap() = r;
                busy.store(false, Ordering::Relaxed);
                ctx2.request_repaint();
            });
        }
        let seen: Vec<_> = std::mem::take(&mut *self.watch_res.lock().unwrap());
        for (path, m) in seen {
            if m.is_none() {
                continue;
            }
            if path == self.cwd {
                if self.vmode == VMode::Dir && !self.loading && self.op.is_none() && self.rename.is_none() && self.preview.is_none() && m != self.dir_mtime {
                    index::mark_dirty();
                    self.refresh();
                }
            } else if self.other.as_deref().map(|o| o.cwd == path && o.vmode == VMode::Dir && !o.loading && m != o.dir_mtime).unwrap_or(false)
                && self.op.is_none()
                && self.preview.is_none()
            {
                self.swap_pane();
                self.refresh();
                self.swap_pane();
            }
        }
        ctx.request_repaint_after(Duration::from_millis(1500));

        self.handle_keys(&ctx);

        // Archivos soltados desde el sistema: se copian a la carpeta actual.
        let (dropped, hovering) = ctx.input(|i| {
            (
                i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).collect::<Vec<_>>(),
                !i.raw.hovered_files.is_empty(),
            )
        });
        let mut dropped = dropped;
        dropped.append(&mut self.ext_dropped);
        if !dropped.is_empty() {
            let dst = self.cwd.clone();
            self.start_copy(dropped, dst, false);
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

        self.phone = std::env::var_os("FB_PHONE").is_some()
            || (cfg!(target_os = "android") && {
                let r = ctx.content_rect();
                r.width().min(r.height()) < 600.0
            });
        let phone = self.phone;
        // márgenes seguros (cámara, reloj, gestos): se consultan cada medio segundo
        let now_t = ctx.input(|i| i.time);
        if now_t - self.insets_t > 0.5 {
            self.insets_t = now_t;
            let ppp = ctx.pixels_per_point();
            self.insets = if let Some(v) = std::env::var_os("FB_INSETS") {
                // prueba en el PC: FB_INSETS=izq,arriba,der,abajo (en puntos)
                let n: Vec<f32> = v.to_string_lossy().split(',').filter_map(|x| x.parse().ok()).collect();
                if n.len() == 4 { [n[0], n[1], n[2], n[3]] } else { [0.0; 4] }
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

        self.side_panel(ui, &pal, bgfill, phone);

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

        if hovering {
            let r = ctx.content_rect().shrink(10.0);
            let p = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::new("drop_hint")));
            p.rect_filled(r, CornerRadius::same(16), pal.sel);
            p.rect_stroke(r, CornerRadius::same(16), Stroke::new(2.0, pal.accent), egui::StrokeKind::Inside);
            p.text(r.center(), Align2::CENTER_CENTER, t("Suelta para copiar aquí"), FontId::proportional(20.0), pal.accent);
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
                        fsys::open_external(&p);
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
        self.trash_poll();
        self.conflict_dialog(&ctx);
        self.dialogs(&ctx);
        self.op_overlay(&ctx);
        self.selftest(&ctx);
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        if self.other.is_some() {
            self.ensure_live(0);
            self.cfg.split_dir = self.other.as_deref().map(|o| o.cwd.clone());
        }
        self.save_tab();
        self.cfg.tabs = self.tabs.iter().map(|t| t.cwd.clone()).filter(|p| virt_mode(p).is_none()).collect();
        self.cfg.tabs2 = self
            .tabs
            .iter()
            .enumerate()
            .filter_map(|(i, t)| t.split.as_ref().map(|s| (i, s.cwd.clone())))
            .collect();
        self.cfg.cur_tab = self.cur_tab;
        self.cfg.save();
    }

    fn clear_color(&self, _v: &egui::Visuals) -> [f32; 4] {
        if self.transparent {
            return [0.0; 4];
        }
        self.pal.bg.to_normalized_gamma_f32()
    }
}

#[cfg(all(test, target_os = "linux"))]
mod gvfs_tests {
    use super::*;

    #[test]
    fn uri_matches_gvfs_names() {
        let d = std::env::temp_dir().join(format!("fbgvfs{}", std::process::id()));
        let g = d.join("gvfs");
        for n in ["smb-share:server=NAS,share=My Share", "sftp:host=pi.local,user=pi", "dav:host=cloud.x.org,ssl=true,prefix=%2Fremote.php"] {
            std::fs::create_dir_all(g.join(n)).unwrap();
        }
        unsafe { std::env::set_var("XDG_RUNTIME_DIR", &d) };
        assert!(uri_to_gvfs("smb://nas/My%20Share").is_some());
        assert!(uri_to_gvfs("smb://user@nas/my share").is_some());
        assert!(uri_to_gvfs("smb://nas").is_some());
        assert!(uri_to_gvfs("smb://nas/otro").is_none());
        assert!(uri_to_gvfs("ssh://pi@pi.local:22/home").is_some());
        assert!(uri_to_gvfs("davs://cloud.x.org/remote.php").is_some());
        assert!(uri_to_gvfs("sftp://otro").is_none());
        let _ = std::fs::remove_dir_all(d);
    }
}

/// Carpeta de caché donde se extrae un comprimido para navegar dentro (única por ruta, tamaño y fecha).
fn archive_cache_dir(p: &Path) -> Option<PathBuf> {
    use std::hash::{Hash, Hasher};
    let md = std::fs::metadata(p).ok()?;
    let mut h = std::collections::hash_map::DefaultHasher::new();
    p.hash(&mut h);
    md.len().hash(&mut h);
    mtime_secs(&md).hash(&mut h);
    Some(dirs::cache_dir()?.join("filebrowser-rs").join("archives").join(format!("{:016x}", h.finish())))
}

/// Marca de extracción completa (junto a la carpeta de caché).
fn archive_marker(root: &Path) -> PathBuf {
    let mut n = root.file_name().map(|n| n.to_os_string()).unwrap_or_default();
    n.push(".done");
    root.with_file_name(n)
}

/// Borra de la caché los comprimidos extraídos hace más de un día y las carpetas temporales
/// (`inner-*`) de la vista previa que dejó un cierre brusco.
pub fn clean_archive_cache() {
    if let Some(dir) = dirs::cache_dir().map(|d| d.join("filebrowser")) {
        if let Ok(rd) = std::fs::read_dir(&dir) {
            for e in rd.flatten() {
                let stale = e.file_name().to_string_lossy().starts_with("inner-")
                    && e.metadata().and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok()).map(|a| a > Duration::from_secs(3600)).unwrap_or(false);
                if stale {
                    let _ = std::fs::remove_dir_all(e.path());
                }
            }
        }
    }
    let Some(dir) = dirs::cache_dir().map(|d| d.join("filebrowser-rs").join("archives")) else { return };
    let Ok(rd) = std::fs::read_dir(&dir) else { return };
    let limit = Duration::from_secs(24 * 3600);
    for e in rd.flatten() {
        let old = e.metadata().and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok()).map(|a| a > limit).unwrap_or(false);
        if old {
            let p = e.path();
            if p.is_dir() {
                let _ = std::fs::remove_dir_all(&p);
            } else {
                let _ = std::fs::remove_file(&p);
            }
        }
    }
}

/// Nombre que no se puede usar para un archivo en ningún sistema (vacío, `.`/`..`, separadores,
/// caracteres prohibidos en Windows, punto o espacio final, más de 255 bytes).
fn bad_name(n: &str) -> bool {
    n.is_empty()
        || n == "."
        || n == ".."
        || n.len() > 255
        || n.contains(['/', '\\', '<', '>', ':', '"', '|', '?', '*', '\0'])
        || n.chars().any(|c| c.is_control())
        || n.ends_with(['.', ' '])
}
