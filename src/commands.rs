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
    c!(Bookmark, "bookmark", "Marcador", "Ctrl+D", true),
    c!(OpenTrash, "trashview", "Papelera", "", true),
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
    c!(Zip, "zip", "Comprimir en ZIP", "", true),
    c!(Unzip, "unzip", "Extraer aquí", "", true),
    c!(Copy, "copy", "Copiar", "Ctrl+C", false),
    c!(Cut, "cut", "Cortar", "Ctrl+X", false),
    c!(Paste, "paste", "Pegar", "Ctrl+V", false),
    c!(Preview, "preview", "Vista previa", "Space", false),
    c!(ToggleHidden, "hidden", "Ocultos", "Ctrl+H", true),
    c!(ToggleSidebar, "sidebar", "Barra lateral", "Ctrl+B", true),
    c!(Terminal, "terminal", "Terminal", "F12", false),
    c!(TerminalHere, "terminalhere", "Abrir terminal aquí", "", true),
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
];

pub fn info(c: Cmd) -> &'static Info {
    ALL.iter().find(|i| i.cmd == c).expect("comando registrado")
}

pub fn parse(spec: &str) -> Option<(Modifiers, Key)> {
    let mut m = Modifiers::NONE;
    let mut key = None;
    for part in spec.split('+') {
        match part.trim() {
            "Ctrl" | "Cmd" | "Command" => m.command = true,
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

/// Texto del atajo a partir de modificadores y tecla pulsados.
pub fn format(m: Modifiers, key: Key) -> String {
    let mut s = String::new();
    if m.command || m.ctrl {
        s.push_str("Ctrl+");
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
