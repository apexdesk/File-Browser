//! Registro de comandos: paleta, atajos configurables y etiquetas únicas para todo el programa.
use egui::{Key, Modifiers};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Cmd {
    Palette,
    Back,
    Forward,
    Up,
    Refresh,
    Home,
    EditPath,
    Bookmark,
    OpenTrash,
    SysTerminal,
    NewTab,
    CloseTab,
    ReopenTab,
    NextTab,
    PrevTab,
    NewWindow,
    NewFolder,
    NewFile,
    Rename,
    BatchRename,
    Trash,
    DeletePerm,
    Duplicate,
    CopyPath,
    Undo,
    Redo,
    SelectAll,
    InvertSel,
    Props,
    OpenWith,
    Hash,
    Dups,
    Zip,
    Unzip,
    Copy,
    Cut,
    Paste,
    Preview,
    ToggleHidden,
    ToggleSidebar,
    PreviewPanel,
    Terminal,
    TerminalHere,
    ToggleSplit,
    SwitchPane,
    CopyOther,
    MoveOther,
    GridView,
    ListView,
    Theme,
    Search,
    Filter,
    Options,
    Keys,
    ConnectServer,
    Quit,
    Menu,
    ZoomIn,
    ZoomOut,
    ZoomReset,
    IconsBigger,
    IconsSmaller,
    EmptyTrash,
    OtherPane,
}

pub struct Info {
    pub cmd: Cmd,
    pub id: &'static str,
    /// Etiqueta (clave de traducción en español).
    pub label: &'static str,
    /// Atajo por defecto ("" = ninguno).
    pub default: &'static str,
    /// El usuario puede cambiar el atajo.
    pub rebind: bool,
}

/// En macOS ⌘H oculta la aplicación: los archivos ocultos van con ⌘⇧. como en el Finder.
const HIDDEN_KEY: &str = if cfg!(target_os = "macos") { "Ctrl+Shift+Period" } else { "Ctrl+H" };

macro_rules! c {
    ($cmd:ident, $id:expr, $label:expr, $def:expr, $rb:expr) => {
        Info { cmd: Cmd::$cmd, id: $id, label: $label, default: $def, rebind: $rb }
    };
}

pub const ALL: &[Info] = &[
    c!(Palette, "palette", "Paleta de comandos", "Shift+Space", true),
    c!(Back, "back", "Atrás", "Alt+ArrowLeft", true),
    c!(Forward, "forward", "Adelante", "Alt+ArrowRight", true),
    c!(Up, "up", "Carpeta superior", "Backspace", true),
    c!(Refresh, "refresh", "Actualizar", "F5", true),
    c!(Home, "home", "Inicio", "", true),
    c!(EditPath, "editpath", "Editar ruta", "Ctrl+L", true),
    c!(Bookmark, "bookmark", "Añadir o quitar de favoritos", "Ctrl+D", true),
    c!(OpenTrash, "trashview", "Papelera", "", true),
    c!(SysTerminal, "systerminal", "Abrir en el terminal del sistema", "", true),
    c!(NewTab, "newtab", "Nueva pestaña", "Ctrl+T", true),
    c!(CloseTab, "closetab", "Cerrar pestaña", "Ctrl+W", true),
    c!(ReopenTab, "reopentab", "Reabrir pestaña cerrada", "Ctrl+Shift+T", true),
    c!(NextTab, "nexttab", "Siguiente pestaña", "Ctrl+Tab", true),
    c!(PrevTab, "prevtab", "Pestaña anterior", "Ctrl+Shift+Tab", true),
    c!(NewWindow, "newwindow", "Nueva ventana", "Ctrl+N", true),
    c!(NewFolder, "newfolder", "Nueva carpeta", "Ctrl+Shift+N", true),
    c!(NewFile, "newfile", "Nuevo archivo vacío…", "", true),
    c!(Rename, "rename", "Renombrar", "F2", true),
    c!(BatchRename, "batchrename", "Renombrar en lote…", "", true),
    c!(Trash, "trash", "Mover a la papelera", "Delete", true),
    c!(DeletePerm, "deleteperm", "Eliminar definitivamente", "Shift+Delete", true),
    c!(Duplicate, "duplicate", "Duplicar", "", true),
    c!(CopyPath, "copypath", "Copiar ruta", "", true),
    c!(Undo, "undo", "Deshacer", "Ctrl+Z", true),
    c!(Redo, "redo", "Rehacer", "Ctrl+Y", true),
    c!(SelectAll, "selectall", "Seleccionar todo", "Ctrl+A", true),
    c!(InvertSel, "invertsel", "Invertir selección", "", true),
    c!(Props, "props", "Propiedades", "Alt+Enter", true),
    c!(OpenWith, "openwith", "Abrir con…", "", true),
    c!(Hash, "hash", "Calcular hash…", "", true),
    c!(Dups, "dups", "Buscar archivos duplicados…", "", true),
    c!(Zip, "zip", "Comprimir…", "", true),
    c!(Unzip, "unzip", "Extraer aquí", "", true),
    c!(Copy, "copy", "Copiar", "Ctrl+C", false),
    c!(Cut, "cut", "Cortar", "Ctrl+X", false),
    c!(Paste, "paste", "Pegar", "Ctrl+V", false),
    c!(Preview, "preview", "Vista previa", "Space", false),
    c!(ToggleHidden, "hidden", "Ocultos", HIDDEN_KEY, true),
    c!(ToggleSidebar, "sidebar", "Barra lateral", "Ctrl+B", true),
    c!(PreviewPanel, "previewpanel", "Panel de vista previa", "F3", true),
    c!(Terminal, "terminal", "Terminal", "F12", false),
    c!(TerminalHere, "terminalhere", "Terminal integrado aquí", "", true),
    c!(ToggleSplit, "split", "Vista dividida (dos paneles)", "F9", true),
    c!(SwitchPane, "switchpane", "Cambiar de panel", "Tab", true),
    c!(CopyOther, "copyother", "Copiar al otro panel", "F6", true),
    c!(MoveOther, "moveother", "Mover al otro panel", "F7", true),
    c!(GridView, "grid", "Vista de cuadrícula", "", true),
    c!(ListView, "list", "Vista de lista", "", true),
    c!(Theme, "theme", "Cambiar tema", "", true),
    c!(Search, "search", "Buscar en subcarpetas", "Ctrl+Shift+F", true),
    c!(Filter, "filter", "Filtrar carpeta", "Ctrl+F", true),
    c!(Options, "options", "Opciones", "", true),
    c!(Keys, "keys", "Atajos de teclado", "", true),
    c!(ConnectServer, "connectserver", "Conectar a un servidor…", "", true),
    c!(Quit, "quit", "Salir", "Ctrl+Q", true),
    c!(Menu, "menu", "Abrir el menú", "F10", true),
    c!(ZoomIn, "zoomin", "Ampliar la interfaz", "Ctrl+Plus", true),
    c!(ZoomOut, "zoomout", "Reducir la interfaz", "Ctrl+Minus", true),
    c!(ZoomReset, "zoomreset", "Interfaz a tamaño normal", "Ctrl+0", true),
    c!(IconsBigger, "iconsbigger", "Iconos más grandes", "", true),
    c!(IconsSmaller, "iconssmaller", "Iconos más pequeños", "", true),
    c!(EmptyTrash, "emptytrash", "Vaciar papelera", "", true),
    // F6 ya es «Copiar al otro panel»; funciona aunque el foco esté en un botón
    c!(OtherPane, "otherpane", "Ir al otro panel", "Ctrl+F6", true),
];

/// ¿Existe el comando en esta plataforma? (Salir y Nueva ventana no tienen sentido en Android ni en
/// la web; el terminal y las conexiones de red no existen en la web.) Lo que no existe no sale en la
/// paleta ni en el editor de atajos.
pub fn available(c: Cmd) -> bool {
    let web = crate::vfs::WEB;
    match c {
        Cmd::Quit => !web && !cfg!(target_os = "android"),
        Cmd::NewWindow | Cmd::Terminal | Cmd::TerminalHere | Cmd::ConnectServer => !web,
        Cmd::OpenTrash | Cmd::EmptyTrash => crate::extras::trash_supported(),
        _ => true,
    }
}

/// «Etiqueta (atajo)» para descripciones emergentes; sólo la etiqueta si el comando no tiene atajo.
pub fn with_shortcut(label: &str, cmd: Cmd) -> String {
    let sc = shortcut_text(cmd);
    if sc.is_empty() { label.to_string() } else { format!("{label} ({sc})") }
}

/// ¿Coincide la búsqueda del editor de atajos con el nombre del comando o con su atajo (tal como se
/// ve o tal como se guarda)? Sin distinguir mayúsculas; una búsqueda vacía coincide con todo.
pub fn matches_query(q: &str, label: &str, shown: &str, stored: &str) -> bool {
    let q = q.trim().to_lowercase();
    if q.is_empty() {
        return true;
    }
    let shown_l = shown.to_lowercase();
    // «ctrl+n» encuentra «Ctrl+Mayús+N»: cada trozo separado por «+» debe estar en el atajo
    let keyish = q.contains('+') && q.split('+').filter(|p| !p.trim().is_empty()).all(|p| shown_l.contains(p.trim()) || stored.to_lowercase().contains(p.trim()));
    label.to_lowercase().contains(&q) || shown_l.contains(&q) || stored.to_lowercase().contains(&q) || keyish
}

/// Cambia el atajo de `cmd` en `keys` (None = sin atajo). Los demás comandos reasignables que tenían esa
/// misma combinación (comparando teclas, no textos) se quedan sin atajo. Si coincide con el de serie no se
/// guarda nada. No comprueba los comandos fijos: eso lo decide antes quien llama (`conflict_in`).
pub fn apply_binding(keys: &mut Vec<(String, String)>, cmd: Cmd, spec: Option<&str>) {
    let i = info(cmd);
    keys.retain(|(k, _)| k != i.id);
    match spec.filter(|s| !s.is_empty()) {
        Some(s) => {
            for o in ALL.iter().filter(|o| o.rebind && o.cmd != cmd) {
                if same_binding(&spec_in(o.cmd, keys), s) {
                    keys.retain(|(k, _)| k != o.id);
                    keys.push((o.id.to_string(), String::new()));
                }
            }
            if !same_binding(i.default, s) {
                keys.push((i.id.to_string(), s.to_string()));
            }
        }
        None => {
            if !i.default.is_empty() {
                keys.push((i.id.to_string(), String::new()));
            }
        }
    }
}

/// ¿Es una tecla suelta (sin Ctrl/Alt/⌘) que hace falta para escribir o moverse por la lista? Esas no
/// se pueden asignar a un comando: escribir en la lista filtra y las flechas, Intro y Esc ya tienen uso.
/// Se permiten las teclas de función y Supr/Retroceso/Insert/Tab/Espacio (algunas ya son de serie).
pub fn reserved_plain(spec: &str) -> bool {
    let Some((m, k)) = parse(spec) else { return false };
    if m.command || m.ctrl || m.alt || m.mac_cmd {
        return false;
    }
    let name = k.name();
    let fkey = name.len() >= 2 && name.starts_with('F') && name[1..].chars().all(|c| c.is_ascii_digit());
    !(fkey || matches!(k, Key::Delete | Key::Backspace | Key::Insert | Key::Tab | Key::Space))
}

pub fn info(c: Cmd) -> &'static Info {
    ALL.iter().find(|i| i.cmd == c).expect("comando registrado")
}

pub fn parse(spec: &str) -> Option<(Modifiers, Key)> {
    let mut m = Modifiers::NONE;
    let mut key = None;
    for part in spec.split('+') {
        match part.trim() {
            "Ctrl" | "Cmd" | "Command" => m.command = true,
            // tecla Control de macOS, distinta de ⌘ (en los demás sistemas Ctrl ya es `command`)
            "Control" => m.ctrl = true,
            "Alt" => m.alt = true,
            "Shift" => m.shift = true,
            "" => return None,
            name => key = Key::from_name(name),
        }
    }
    let key = key?;
    if m.command {
        m = m | Modifiers::COMMAND;
    }
    Some((m, key))
}

/// Texto del atajo a partir de modificadores y tecla pulsados, en el formato que se guarda en los
/// ajustes (inglés, «Ctrl+Shift+N»; lo entiende `parse`). Para mostrarlo, `display`.
pub fn format(m: Modifiers, key: Key) -> String {
    let mut s = String::new();
    // «Ctrl» guardado es ⌘ en macOS; Control a secas (sólo allí, sin ⌘) se guarda como «Control»
    if m.command || m.mac_cmd {
        s.push_str("Ctrl+");
    }
    if m.ctrl && !m.command && !m.mac_cmd {
        s.push_str("Control+");
    }
    if m.alt {
        s.push_str("Alt+");
    }
    if m.shift {
        s.push_str("Shift+");
    }
    s.push_str(key.name());
    s
}

// ---------------------------------------------------------------------------
// Atajos vigentes y su texto en pantalla
// ---------------------------------------------------------------------------

/// Atajos personalizados vigentes (copia de `Config::keys`: id del comando → atajo, "" = sin atajo).
static USER_KEYS: std::sync::RwLock<Vec<(String, String)>> = std::sync::RwLock::new(Vec::new());

/// Publica los atajos personalizados para `spec`, `shortcut_text` y `conflict_for`. Llamar al cargar
/// la configuración y cada vez que cambie `cfg.keys`.
pub fn set_user_keys(keys: &[(String, String)]) {
    if let Ok(mut w) = USER_KEYS.write() {
        *w = keys.to_vec();
    }
}

/// Atajo efectivo de un comando en formato guardado (personalizado o por defecto; "" si no tiene).
pub fn spec_in(cmd: Cmd, keys: &[(String, String)]) -> String {
    let i = info(cmd);
    keys.iter().find(|(id, _)| id == i.id).map(|(_, s)| s.clone()).unwrap_or_else(|| i.default.to_string())
}

/// `spec_in` con los atajos publicados por `set_user_keys`.
pub fn spec(cmd: Cmd) -> String {
    match USER_KEYS.read() {
        Ok(k) => spec_in(cmd, &k),
        Err(_) => info(cmd).default.to_string(),
    }
}

/// Texto en pantalla del atajo vigente de un comando ("" si no tiene), para menús y descripciones
/// emergentes en vez de escribir «Ctrl+…» a mano.
pub fn shortcut_text(cmd: Cmd) -> String {
    display(&spec(cmd))
}

/// `shortcut_text` con una lista de atajos personalizados dada.
pub fn shortcut_text_in(cmd: Cmd, keys: &[(String, String)]) -> String {
    display(&spec_in(cmd, keys))
}

/// Forma canónica de un atajo para compararlo (Ctrl y Cmd cuentan igual, igual que en `parse`).
fn canon(spec: &str) -> Option<((bool, bool), bool, bool, Key)> {
    parse(spec).map(|(m, k)| ((m.command || m.mac_cmd, m.ctrl && !m.command && !m.mac_cmd), m.alt, m.shift, k))
}

/// ¿Son el mismo atajo? Compara teclas, no textos («Alt+Left» = «Alt+ArrowLeft»).
pub fn same_binding(a: &str, b: &str) -> bool {
    match (canon(a), canon(b)) {
        (Some(x), Some(y)) => x == y,
        _ => false,
    }
}

/// Comando (distinto de `except`) que ya usa ese atajo con los atajos `keys`.
pub fn conflict_in(spec: &str, keys: &[(String, String)], except: Option<Cmd>) -> Option<Cmd> {
    canon(spec)?;
    ALL.iter().filter(|i| Some(i.cmd) != except).find(|i| same_binding(&spec_in(i.cmd, keys), spec)).map(|i| i.cmd)
}

/// Comando que ya usa ese atajo (con los atajos publicados), para avisar en el editor de atajos.
pub fn conflict_for(spec: &str) -> Option<Cmd> {
    match USER_KEYS.read() {
        Ok(k) => conflict_in(spec, &k, None),
        Err(_) => conflict_in(spec, &[], None),
    }
}

/// Como `conflict_for`, sin contar el propio comando que se está cambiando.
pub fn conflict_for_other(spec: &str, cmd: Cmd) -> Option<Cmd> {
    match USER_KEYS.read() {
        Ok(k) => conflict_in(spec, &k, Some(cmd)),
        Err(_) => conflict_in(spec, &[], Some(cmd)),
    }
}

/// Texto en pantalla de un atajo guardado: traducido y con las convenciones de la plataforma
/// («Ctrl+Mayús+N», «Strg+Umschalt+N», «⌘+Shift+N»). Un texto que no se entiende se devuelve tal cual.
pub fn display(spec: &str) -> String {
    match parse(spec) {
        Some((m, k)) => display_keys(m, k),
        None => spec.to_string(),
    }
}

/// Texto en pantalla de una combinación de modificadores y tecla.
pub fn display_keys(m: Modifiers, key: Key) -> String {
    display_with(crate::i18n::current(), cfg!(target_os = "macos"), m, key)
}

fn display_with(lang: usize, mac: bool, m: Modifiers, key: Key) -> String {
    use crate::i18n::t_in;
    let mut parts: Vec<&str> = Vec::new();
    if mac {
        // en macOS sólo «⌘» está en las fuentes de egui; el resto, con su nombre
        if m.command || m.mac_cmd {
            parts.push("⌘");
        }
        if m.ctrl && !m.command {
            parts.push("Control");
        }
        if m.alt {
            parts.push("Option");
        }
    } else {
        if m.command || m.ctrl {
            parts.push(t_in(lang, "Ctrl"));
        }
        if m.alt {
            parts.push("Alt");
        }
    }
    if m.shift {
        parts.push(t_in(lang, "Mayús"));
    }
    let tecla = |k: &'static str| t_in(lang, k).trim_end_matches(" (tecla)");
    // flechas: las de la fuente de iconos de egui (←→ no están en sus fuentes proporcionales)
    let name: &str = match key {
        Key::ArrowLeft => "⏴",
        Key::ArrowRight => "⏵",
        Key::ArrowUp => "⏶",
        Key::ArrowDown => "⏷",
        Key::Delete => tecla("Supr"),
        Key::Backspace => tecla("Retroceso"),
        Key::Enter => tecla("Intro"),
        Key::Space => tecla("Espacio"),
        Key::PageUp => tecla("RePág"),
        Key::PageDown => tecla("AvPág"),
        Key::Home => tecla("Inicio (tecla)"),
        Key::End => tecla("Fin (tecla)"),
        Key::Insert => tecla("Insert"),
        Key::Escape => "Esc",
        k => k.symbol_or_name(),
    };
    parts.push(name);
    parts.join("+")
}

/// Puntuación de coincidencia difusa (subsecuencia); None si no coincide.
pub fn fuzzy(q: &str, text: &str) -> Option<i32> {
    if q.is_empty() {
        return Some(0);
    }
    let t: Vec<char> = text.to_lowercase().chars().collect();
    let mut score = 0i32;
    let mut ti = 0usize;
    let mut prev: Option<usize> = None;
    for qc in q.to_lowercase().chars() {
        if qc.is_whitespace() {
            continue;
        }
        let mut found = None;
        while ti < t.len() {
            if t[ti] == qc {
                found = Some(ti);
                ti += 1;
                break;
            }
            ti += 1;
        }
        let i = found?;
        score += 10;
        if prev.map(|p| p + 1 == i).unwrap_or(false) {
            score += 8; // seguidas
        }
        if i == 0 || t[i - 1] == ' ' {
            score += 6; // inicio de palabra
        }
        prev = Some(i);
    }
    Some(score - t.len() as i32 / 8)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(lang: &str, mac: bool, spec: &str) -> String {
        let (m, k) = parse(spec).expect(spec);
        display_with(crate::i18n::index_of(lang).unwrap(), mac, m, k)
    }

    #[test]
    fn texto_de_atajos_por_idioma_y_plataforma() {
        assert_eq!(d("es", false, "Ctrl+Shift+N"), "Ctrl+Mayús+N");
        assert_eq!(d("en", false, "Ctrl+Shift+N"), "Ctrl+Shift+N");
        assert_eq!(d("de", false, "Ctrl+Shift+N"), "Strg+Umschalt+N");
        assert_eq!(d("en", true, "Ctrl+Shift+N"), "⌘+Shift+N");
        assert_eq!(d("es", false, "Alt+ArrowLeft"), "Alt+⏴");
        assert_eq!(d("en", true, "Alt+ArrowLeft"), "Option+⏴");
        assert_eq!(d("es", false, "Delete"), "Supr");
        assert_eq!(d("en", false, "Shift+Delete"), "Shift+Del");
        assert_eq!(d("es", false, "Backspace"), "Retroceso");
        assert_eq!(d("es", false, "Alt+Enter"), "Alt+Intro");
        assert_eq!(d("en", false, "Alt+Enter"), "Alt+Enter");
        assert_eq!(d("es", false, "Shift+Space"), "Mayús+Espacio");
        assert_eq!(d("es", false, "Home"), "Inicio");
        assert_eq!(d("de", false, "Home"), "Pos1");
        assert_eq!(d("es", false, "End"), "Fin");
        assert_eq!(d("es", false, "PageUp"), "RePág");
        assert_eq!(d("en", false, "F5"), "F5");
        // lo que no se entiende se muestra tal cual; vacío sigue vacío
        assert_eq!(display(""), "");
        assert_eq!(display("Ctrl+"), "Ctrl+");
    }

    #[test]
    fn todos_los_atajos_por_defecto_se_entienden_y_no_chocan() {
        for i in ALL {
            if i.default.is_empty() {
                continue;
            }
            assert!(parse(i.default).is_some(), "{}", i.default);
            assert!(!display(i.default).is_empty());
            assert_eq!(conflict_in(i.default, &[], Some(i.cmd)), None, "{} choca", i.default);
            // lo que se guarda al grabar una combinación se vuelve a entender igual
            let (m, k) = parse(i.default).unwrap();
            assert!(same_binding(&format(m, k), i.default), "{}", i.default);
        }
    }

    #[test]
    fn atajos_personalizados_y_conflictos() {
        let keys = vec![("back".to_string(), "Ctrl+K".to_string()), ("refresh".to_string(), String::new())];
        assert_eq!(spec_in(Cmd::Back, &keys), "Ctrl+K");
        assert_eq!(spec_in(Cmd::Refresh, &keys), "");
        assert_eq!(spec_in(Cmd::EditPath, &keys), "Ctrl+L");
        assert_eq!(conflict_in("Ctrl+K", &keys, None), Some(Cmd::Back));
        assert_eq!(conflict_in("Ctrl+K", &keys, Some(Cmd::Back)), None);
        // F5 quedó libre al quitarle el atajo a «Actualizar»
        assert_eq!(conflict_in("F5", &keys, None), None);
        // se comparan teclas, no textos: «Alt+Left» (como lo graba `format`) = «Alt+ArrowLeft»
        assert_eq!(conflict_in("Alt+Left", &[], None), Some(Cmd::Back));
        assert!(same_binding("Cmd+L", "Ctrl+L"));
        // Control de macOS no es ⌘: se graba aparte y no choca con «Ctrl+K»
        let mut m = Modifiers::NONE;
        m.ctrl = true;
        assert_eq!(format(m, Key::K), "Control+K");
        assert_eq!(parse("Control+K"), Some((m, Key::K)));
        assert!(!same_binding("Control+K", "Ctrl+K"));
        assert_eq!(format(Modifiers::COMMAND, Key::K), "Ctrl+K");
        assert!(!same_binding("Ctrl+L", "Ctrl+Shift+L"));
        assert_eq!(conflict_in("", &[], None), None);
        assert_eq!(shortcut_text_in(Cmd::Refresh, &keys), "");
    }

    #[test]
    fn reasignar_quita_el_atajo_al_anterior_dueno() {
        let mut keys: Vec<(String, String)> = Vec::new();
        // «Alt+Left» (como lo graba `format`) es el atajo de serie de «Atrás»: «Arriba» se lo queda
        apply_binding(&mut keys, Cmd::Up, Some("Alt+Left"));
        assert_eq!(spec_in(Cmd::Up, &keys), "Alt+Left");
        assert_eq!(spec_in(Cmd::Back, &keys), "");
        assert_eq!(conflict_in("Alt+ArrowLeft", &keys, None), Some(Cmd::Up));
        // volver al de serie no deja una entrada y libera la combinación de quien la tuviera
        apply_binding(&mut keys, Cmd::Back, Some(info(Cmd::Back).default));
        assert_eq!(spec_in(Cmd::Back, &keys), "Alt+ArrowLeft");
        assert!(!keys.iter().any(|(k, _)| k == "back"));
        assert_eq!(spec_in(Cmd::Up, &keys), "");
        // quitar el atajo
        apply_binding(&mut keys, Cmd::Refresh, None);
        assert_eq!(spec_in(Cmd::Refresh, &keys), "");
        // un comando sin atajo de serie no necesita entrada para no tenerlo
        apply_binding(&mut keys, Cmd::Duplicate, None);
        assert!(!keys.iter().any(|(k, _)| k == "duplicate"));
        // los comandos fijos (Copiar) no se tocan aunque coincidan
        apply_binding(&mut keys, Cmd::Duplicate, Some("Ctrl+C"));
        assert_eq!(spec_in(Cmd::Copy, &keys), "Ctrl+C");
        // ninguna combinación queda repetida entre comandos reasignables
        for a in ALL.iter().filter(|i| i.rebind) {
            let s = spec_in(a.cmd, &keys);
            if s.is_empty() {
                continue;
            }
            for b in ALL.iter().filter(|i| i.rebind && i.cmd != a.cmd) {
                assert!(!same_binding(&s, &spec_in(b.cmd, &keys)), "{} y {} comparten {s}", a.id, b.id);
            }
        }
    }

    #[test]
    fn busqueda_del_editor_de_atajos() {
        assert!(matches_query("", "Nueva carpeta", "Ctrl+Mayús+N", "Ctrl+Shift+N"));
        assert!(matches_query("carp", "Nueva carpeta", "Ctrl+Mayús+N", "Ctrl+Shift+N"));
        assert!(matches_query("CARPETA", "Nueva carpeta", "", ""));
        assert!(matches_query("ctrl+n", "Nueva carpeta", "Ctrl+Mayús+N", "Ctrl+Shift+N"));
        assert!(matches_query("shift+n", "Nueva carpeta", "Ctrl+Mayús+N", "Ctrl+Shift+N"));
        assert!(matches_query("f5", "Actualizar", "F5", "F5"));
        assert!(!matches_query("f6", "Actualizar", "F5", "F5"));
        assert!(!matches_query("ctrl+q", "Nueva carpeta", "Ctrl+Mayús+N", "Ctrl+Shift+N"));
    }

    #[test]
    fn atajos_nuevos_y_plataformas() {
        assert_eq!(d("en", false, "Ctrl+Q"), "Ctrl+Q");
        assert_eq!(d("en", true, "Ctrl+Q"), "⌘+Q");
        assert_eq!(d("es", false, "Ctrl+0"), "Ctrl+0");
        assert!(d("de", false, "Ctrl+Minus").starts_with("Strg+"));
        assert_eq!(d("en", true, "Ctrl+Plus"), "⌘++");
        assert_eq!(d("es", false, "F10"), "F10");
        // todos los comandos tienen id y etiqueta únicos
        for (n, a) in ALL.iter().enumerate() {
            for b in &ALL[n + 1..] {
                assert_ne!(a.id, b.id);
                assert_ne!(a.cmd, b.cmd);
            }
        }
        // la búsqueda del registro no falla con ningún comando
        for a in ALL {
            let _ = info(a.cmd);
        }
        assert!(available(Cmd::Palette));
        assert_eq!(available(Cmd::Quit), !crate::vfs::WEB && !cfg!(target_os = "android"));
    }

    #[test]
    fn teclas_sueltas_reservadas() {
        assert!(reserved_plain("A"));
        assert!(reserved_plain("Shift+A"));
        assert!(reserved_plain("Enter"));
        assert!(reserved_plain("ArrowDown"));
        assert!(reserved_plain("Escape"));
        assert!(!reserved_plain("F10"));
        assert!(!reserved_plain("F1"));
        assert!(!reserved_plain("Delete"));
        assert!(!reserved_plain("Backspace"));
        assert!(!reserved_plain("Ctrl+A"));
        assert!(!reserved_plain("Alt+Enter"));
        assert!(!reserved_plain("basura"));
        // ningún atajo de serie reasignable es una tecla reservada
        for a in ALL.iter().filter(|a| a.rebind && !a.default.is_empty()) {
            assert!(!reserved_plain(a.default), "{}", a.id);
        }
    }
}
