//! Terminal del sistema: abrir una carpeta en el terminal del escritorio (no en el integrado) y
//! ejecutar en él las aplicaciones con `Terminal=true`. Sólo se construyen listas de argumentos:
//! nunca se pasa la ruta por `sh -c` ni por `cmd /C`.
#![cfg_attr(any(target_os = "android", target_arch = "wasm32"), allow(dead_code))]

use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// ¿Hay terminal del sistema en esta plataforma? (Android y la web no tienen.)
pub fn supported() -> bool {
    cfg!(not(any(target_os = "android", target_arch = "wasm32")))
}

/// Orden ya construida: programa, argumentos, carpeta de trabajo y si necesita consola nueva (Windows).
#[derive(Clone, Debug, PartialEq)]
pub struct Launch {
    pub program: OsString,
    pub args: Vec<OsString>,
    pub cwd: PathBuf,
    pub new_console: bool,
}

/// Cómo recibe un terminal la carpeta de trabajo.
#[derive(Clone, Copy, Debug, PartialEq)]
enum DirArg {
    /// sólo la carpeta de trabajo del proceso
    Cwd,
    /// `--opcion=CARPETA` en un único argumento
    Eq(&'static str),
    /// `--opcion CARPETA` en dos argumentos
    Flag(&'static str),
}

/// Terminal conocido (Linux/BSD).
struct Known {
    bin: &'static str,
    /// argumentos antes de la carpeta (p. ej. `start` de wezterm)
    pre: &'static [&'static str],
    dir: DirArg,
    /// separador antes de la orden a ejecutar (`[]` = la orden va directamente); `None` = no sabe
    /// ejecutar una orden como lista de argumentos
    exec: Option<&'static [&'static str]>,
}

const KNOWN: &[Known] = &[
    Known { bin: "ptyxis", pre: &["--new-window"], dir: DirArg::Eq("--working-directory="), exec: Some(&["--"]) },
    Known { bin: "kgx", pre: &[], dir: DirArg::Eq("--working-directory="), exec: Some(&["--"]) },
    Known { bin: "gnome-console", pre: &[], dir: DirArg::Eq("--working-directory="), exec: Some(&["--"]) },
    Known { bin: "gnome-terminal", pre: &[], dir: DirArg::Eq("--working-directory="), exec: Some(&["--"]) },
    Known { bin: "konsole", pre: &[], dir: DirArg::Flag("--workdir"), exec: Some(&["-e"]) },
    Known { bin: "xfce4-terminal", pre: &[], dir: DirArg::Eq("--working-directory="), exec: Some(&["-x"]) },
    Known { bin: "tilix", pre: &[], dir: DirArg::Eq("--working-directory="), exec: None },
    Known { bin: "alacritty", pre: &[], dir: DirArg::Flag("--working-directory"), exec: Some(&["-e"]) },
    Known { bin: "kitty", pre: &[], dir: DirArg::Flag("--directory"), exec: Some(&[]) },
    Known { bin: "foot", pre: &[], dir: DirArg::Eq("--working-directory="), exec: Some(&[]) },
    Known { bin: "wezterm", pre: &["start"], dir: DirArg::Flag("--cwd"), exec: Some(&["--"]) },
    Known { bin: "ghostty", pre: &[], dir: DirArg::Eq("--working-directory="), exec: Some(&["-e"]) },
    Known { bin: "terminator", pre: &[], dir: DirArg::Eq("--working-directory="), exec: Some(&["-x"]) },
    Known { bin: "lxterminal", pre: &[], dir: DirArg::Eq("--working-directory="), exec: None },
    Known { bin: "mate-terminal", pre: &[], dir: DirArg::Eq("--working-directory="), exec: Some(&["-x"]) },
    Known { bin: "qterminal", pre: &[], dir: DirArg::Flag("-w"), exec: Some(&["-e"]) },
    Known { bin: "xterm", pre: &[], dir: DirArg::Cwd, exec: Some(&["-e"]) },
];

/// Terminales que no están en la lista pero sí se reconocen por su nombre.
const EXTRA_KNOWN: &[Known] = &[
    Known { bin: "xdg-terminal-exec", pre: &[], dir: DirArg::Cwd, exec: Some(&[]) },
    Known { bin: "x-terminal-emulator", pre: &[], dir: DirArg::Cwd, exec: Some(&["-e"]) },
];

fn known(bin: &str) -> Option<&'static Known> {
    let base = Path::new(bin).file_name().and_then(|n| n.to_str()).unwrap_or(bin);
    KNOWN.iter().chain(EXTRA_KNOWN).find(|k| k.bin == base)
}

/// Terminal elegido: argumentos base (programa primero) y, si se reconoce, cómo pasarle carpeta y orden.
#[derive(Clone, Debug, PartialEq)]
pub struct Spec {
    argv: Vec<String>,
    /// la orden del usuario lleva `{dir}`: no se añade nada más
    placeholder: bool,
    dir: DirArg,
    pre: Vec<String>,
    exec: Option<Vec<String>>,
    /// Windows: consola nueva; macOS: `open -a`
    kind: Kind,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Kind {
    Posix,
    WinTerminal,
    WinPower,
    WinCmd,
    MacApp,
    Custom,
}

impl Spec {
    /// Nombre corto para mostrarlo (sugerencia en Opciones).
    pub fn label(&self) -> String {
        match self.kind {
            Kind::MacApp => self.argv.last().cloned().unwrap_or_default(),
            _ => self.argv.join(" "),
        }
    }

    fn of_known(path: &str, k: &Known) -> Spec {
        Spec {
            argv: vec![path.to_string()],
            placeholder: false,
            dir: k.dir,
            pre: k.pre.iter().map(|s| s.to_string()).collect(),
            exec: k.exec.map(|e| e.iter().map(|s| s.to_string()).collect()),
            kind: Kind::Posix,
        }
    }

    /// Orden escrita por el usuario (o $TERMINAL): palabras con comillas; `{dir}` se sustituye por la carpeta.
    fn custom(cmd: &str) -> Option<Spec> {
        let argv = split_words(cmd)?;
        if argv.is_empty() {
            return None;
        }
        let placeholder = argv.iter().any(|a| a.contains("{dir}"));
        let k = known(&argv[0]);
        Some(Spec {
            placeholder,
            dir: if placeholder { DirArg::Cwd } else { k.map(|k| k.dir).unwrap_or(DirArg::Cwd) },
            pre: if placeholder || argv.len() > 1 { Vec::new() } else { k.map(|k| k.pre.iter().map(|s| s.to_string()).collect()).unwrap_or_default() },
            // un terminal desconocido sigue el convenio de x-terminal-emulator (`-e orden…`)
            exec: Some(k.and_then(|k| k.exec).map(|e| e.iter().map(|s| s.to_string()).collect()).unwrap_or_else(|| vec!["-e".into()])),
            argv,
            kind: Kind::Custom,
        })
    }

    /// Orden para abrir el terminal en `dir`.
    pub fn open(&self, dir: &Path) -> Launch {
        let d = dir.as_os_str().to_os_string();
        let mut args: Vec<OsString> = Vec::new();
        let mut program: OsString = self.argv.first().map(OsString::from).unwrap_or_default();
        let mut new_console = false;
        match self.kind {
            Kind::MacApp => {
                program = "open".into();
                args.push("-a".into());
                args.push(self.argv.last().map(OsString::from).unwrap_or_else(|| "Terminal".into()));
                args.push(d);
            }
            Kind::WinTerminal => {
                args.push("-d".into());
                args.push(d);
            }
            Kind::WinPower => {
                new_console = true;
                args.push("-NoExit".into());
                args.push("-Command".into());
                args.push(ps_set_location(&dir.to_string_lossy()).into());
            }
            Kind::WinCmd => {
                // sin la ruta en la línea de órdenes: cmd arranca en su carpeta de trabajo
                new_console = true;
                args.push("/K".into());
            }
            Kind::Posix | Kind::Custom => {
                // en Windows, una orden propia de consola necesita la suya (en una GUI no hace nada)
                new_console = cfg!(windows);
                if self.placeholder {
                    for a in self.argv.iter().skip(1) {
                        args.push(subst_dir(a, dir));
                    }
                    if let Some(p) = self.argv.first() {
                        if p.contains("{dir}") {
                            program = subst_dir(p, dir);
                        }
                    }
                } else {
                    args.extend(self.argv.iter().skip(1).map(OsString::from));
                    args.extend(self.pre.iter().map(OsString::from));
                    match self.dir {
                        DirArg::Cwd => {}
                        DirArg::Eq(f) => {
                            let mut a = OsString::from(f);
                            a.push(&d);
                            args.push(a);
                        }
                        DirArg::Flag(f) => {
                            args.push(f.into());
                            args.push(d);
                        }
                    }
                }
            }
        }
        Launch { program, args, cwd: dir.to_path_buf(), new_console }
    }

    /// Orden para ejecutar `argv` dentro del terminal, en `dir`. `None` si este terminal no sabe.
    pub fn exec(&self, dir: &Path, argv: &[OsString]) -> Option<Launch> {
        if argv.is_empty() {
            return None;
        }
        match self.kind {
            Kind::Posix | Kind::Custom => {
                let sep = self.exec.as_ref()?;
                let mut l = if self.placeholder {
                    // con `{dir}` la orden del usuario ya fija la carpeta
                    self.open(dir)
                } else {
                    let mut s = self.clone();
                    // wezterm necesita su subcomando aunque no haga falta la carpeta
                    s.dir = DirArg::Cwd;
                    s.open(dir)
                };
                l.args.extend(sep.iter().map(OsString::from));
                l.args.extend(argv.iter().cloned());
                Some(l)
            }
            // en Windows y macOS no hay aplicaciones con `Terminal=true`
            _ => None,
        }
    }
}

fn subst_dir(a: &str, dir: &Path) -> OsString {
    let mut out = OsString::new();
    let mut rest = a;
    while let Some(i) = rest.find("{dir}") {
        out.push(&rest[..i]);
        out.push(dir.as_os_str());
        rest = &rest[i + 5..];
    }
    out.push(rest);
    out
}

/// `Set-Location -LiteralPath '…'` para PowerShell: entre comillas simples no se expande nada
/// (`$()`, `` ` ``, `%VAR%`, `&`); las comillas simples, también las tipográficas, se doblan.
pub fn ps_set_location(p: &str) -> String {
    let mut q = String::with_capacity(p.len() + 2);
    for c in p.chars() {
        if matches!(c, '\'' | '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}') {
            q.push(c);
        }
        q.push(c);
    }
    format!("Set-Location -LiteralPath '{q}'")
}

/// Parte una orden en palabras: espacios separan; '…' literal; "…" admite `\"` y `\\`; fuera de
/// comillas `\` escapa el carácter siguiente. `None` si quedan comillas abiertas.
pub fn split_words(s: &str) -> Option<Vec<String>> {
    let mut out = Vec::new();
    let mut w = String::new();
    let mut has = false;
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        match c {
            c if c.is_whitespace() => {
                if has {
                    out.push(std::mem::take(&mut w));
                    has = false;
                }
            }
            '\'' => {
                has = true;
                loop {
                    match it.next()? {
                        '\'' => break,
                        o => w.push(o),
                    }
                }
            }
            '"' => {
                has = true;
                loop {
                    match it.next()? {
                        '"' => break,
                        '\\' if matches!(it.peek(), Some('"' | '\\')) => w.push(it.next()?),
                        o => w.push(o),
                    }
                }
            }
            // en Windows la barra invertida es separador de ruta: no escapa
            '\\' if !cfg!(windows) => {
                has = true;
                if let Some(n) = it.next() {
                    w.push(n);
                }
            }
            o => {
                has = true;
                w.push(o);
            }
        }
    }
    if has {
        out.push(w);
    }
    Some(out)
}

/// Lo que se consulta del entorno para elegir terminal (separado para poder probarlo).
#[derive(Clone, Debug, Default)]
pub struct TermEnv {
    /// orden configurada en Opciones («Terminal del sistema»)
    pub configured: String,
    /// variable $TERMINAL
    pub terminal_var: String,
    /// carpetas donde buscar ejecutables (PATH)
    pub path: Vec<PathBuf>,
    /// terminal predeterminado de GNOME (gsettings) y de KDE (kdeglobals)
    pub gnome: Option<String>,
    pub kde: Option<String>,
}

/// Busca un ejecutable en `path` (o lo acepta tal cual si es una ruta).
pub fn find_in(path: &[PathBuf], bin: &str) -> Option<PathBuf> {
    if bin.is_empty() {
        return None;
    }
    let p = Path::new(bin);
    if p.components().count() > 1 {
        return is_exec(p).then(|| p.to_path_buf());
    }
    for d in path {
        let c = d.join(bin);
        if is_exec(&c) {
            return Some(c);
        }
        if cfg!(windows) && p.extension().is_none() {
            let c = d.join(format!("{bin}.exe"));
            if is_exec(&c) {
                return Some(c);
            }
        }
    }
    None
}

fn is_exec(p: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(p).map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0).unwrap_or(false)
    }
    #[cfg(windows)]
    {
        // los alias de ejecución de la Tienda (wt.exe) son puntos de análisis que `metadata` no sigue
        std::fs::metadata(p).map(|m| m.is_file()).unwrap_or(false) || std::fs::symlink_metadata(p).is_ok()
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = p;
        false
    }
}

/// Elige terminal en Linux/BSD, por este orden: el configurado, $TERMINAL, `xdg-terminal-exec`,
/// el predeterminado del escritorio (GNOME, KDE, x-terminal-emulator de Debian) y el primero
/// instalado de una lista conocida.
pub fn detect_posix(env: &TermEnv) -> Option<Spec> {
    for c in [&env.configured, &env.terminal_var] {
        if let Some(s) = Spec::custom(c.trim()) {
            if find_in(&env.path, &s.argv[0]).is_some() || s.argv[0].contains("{dir}") {
                return Some(s);
            }
        }
    }
    let mut cands: Vec<String> = vec!["xdg-terminal-exec".into()];
    cands.extend(env.gnome.iter().chain(&env.kde).filter(|s| !s.trim().is_empty()).cloned());
    cands.push("x-terminal-emulator".into());
    for c in cands {
        let Some(s) = Spec::custom(&c) else { continue };
        if let Some(p) = find_in(&env.path, &s.argv[0]) {
            if s.argv.len() == 1 {
                if let Some(k) = known(&s.argv[0]) {
                    return Some(Spec::of_known(&p.to_string_lossy(), k));
                }
            }
            return Some(s);
        }
    }
    for k in KNOWN {
        if let Some(p) = find_in(&env.path, k.bin) {
            return Some(Spec::of_known(&p.to_string_lossy(), k));
        }
    }
    None
}

/// Como `detect_posix`, pero el primero que sepa ejecutar una orden (para `Terminal=true`).
pub fn detect_posix_exec(env: &TermEnv) -> Option<Spec> {
    if let Some(s) = detect_posix(env).filter(|s| s.exec.is_some()) {
        return Some(s);
    }
    KNOWN.iter().filter(|k| k.exec.is_some()).find_map(|k| find_in(&env.path, k.bin).map(|p| Spec::of_known(&p.to_string_lossy(), k)))
}

/// Windows: el configurado; si no, Windows Terminal, PowerShell (7 o la de Windows) y por último cmd.
/// `dir` decide si sirve Windows Terminal (su línea de órdenes trata «;» como separador).
pub fn detect_windows(configured: &str, path: &[PathBuf], dir: &Path) -> Spec {
    let mk = |argv: Vec<String>, kind: Kind| Spec { argv, placeholder: false, dir: DirArg::Cwd, pre: Vec::new(), exec: None, kind };
    if let Some(mut s) = Spec::custom(configured.trim()) {
        let base = Path::new(&s.argv[0]).file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
        if !s.placeholder && s.argv.len() == 1 {
            s.kind = match base.trim_end_matches(".exe") {
                "wt" => Kind::WinTerminal,
                "powershell" | "pwsh" => Kind::WinPower,
                "cmd" => Kind::WinCmd,
                _ => Kind::Custom,
            };
            if s.kind == Kind::WinTerminal && dir.to_string_lossy().contains(';') {
                s.kind = Kind::WinPower;
                s.argv = vec!["powershell.exe".into()];
            }
        }
        return s;
    }
    if !dir.to_string_lossy().contains(';') {
        if let Some(p) = find_in(path, "wt.exe") {
            return mk(vec![p.to_string_lossy().into_owned()], Kind::WinTerminal);
        }
    }
    if let Some(p) = find_in(path, "pwsh.exe") {
        return mk(vec![p.to_string_lossy().into_owned()], Kind::WinPower);
    }
    if let Some(p) = find_in(path, "powershell.exe") {
        return mk(vec![p.to_string_lossy().into_owned()], Kind::WinPower);
    }
    mk(vec!["cmd.exe".into()], Kind::WinCmd)
}

/// macOS: la aplicación configurada (o Terminal) con `open -a`; con `{dir}`, la orden tal cual.
pub fn detect_macos(configured: &str) -> Spec {
    let c = configured.trim();
    if c.contains("{dir}") {
        if let Some(s) = Spec::custom(c) {
            return s;
        }
    }
    let app = if c.is_empty() { "Terminal" } else { c.trim_end_matches(".app") };
    Spec { argv: vec![app.to_string()], placeholder: false, dir: DirArg::Cwd, pre: Vec::new(), exec: None, kind: Kind::MacApp }
}

// ---------------------------------------------------------------------------
// Entorno real del proceso
// ---------------------------------------------------------------------------

/// ¿Dentro de Flatpak? Los terminales están fuera: se lanzan con `flatpak-spawn --host`.
pub fn in_flatpak() -> bool {
    cfg!(target_os = "linux") && (std::env::var_os("FLATPAK_ID").is_some() || Path::new("/.flatpak-info").exists())
}

fn process_path() -> Vec<PathBuf> {
    if in_flatpak() {
        // con --filesystem=host el sistema anfitrión se ve en /run/host
        return ["/run/host/usr/local/bin", "/run/host/usr/bin", "/run/host/bin"].iter().map(PathBuf::from).collect();
    }
    std::env::var_os("PATH").map(|p| crate::vfs::split_paths(&p).collect()).unwrap_or_default()
}

/// Terminal de KDE (kdeglobals, [General] TerminalApplication).
pub fn kde_terminal(kdeglobals: &str) -> Option<String> {
    let mut in_general = false;
    for l in kdeglobals.lines() {
        let l = l.trim();
        if l.starts_with('[') {
            in_general = l == "[General]";
        } else if in_general {
            if let Some((k, v)) = l.split_once('=') {
                if k.trim() == "TerminalApplication" && !v.trim().is_empty() {
                    return Some(v.trim().to_string());
                }
            }
        }
    }
    None
}

#[cfg(all(unix, not(any(target_os = "macos", target_os = "android"))))]
fn process_env(configured: &str) -> TermEnv {
    let desk = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default().to_lowercase();
    let mut env = TermEnv {
        configured: configured.to_string(),
        terminal_var: std::env::var("TERMINAL").unwrap_or_default(),
        path: process_path(),
        gnome: None,
        kde: None,
    };
    if desk.contains("kde") {
        let cfg = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).or_else(|| dirs::home_dir().map(|h| h.join(".config")));
        env.kde = cfg.and_then(|c| std::fs::read_to_string(c.join("kdeglobals")).ok()).and_then(|t| kde_terminal(&t));
        if env.kde.is_none() {
            env.kde = Some("konsole".into());
        }
    }
    if ["gnome", "unity", "cinnamon", "budgie", "pantheon"].iter().any(|d| desk.contains(d)) && !in_flatpak() {
        if let Ok(o) = std::process::Command::new("gsettings")
            .args(["get", "org.gnome.desktop.default-applications.terminal", "exec"])
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .output()
        {
            let s = String::from_utf8_lossy(&o.stdout).trim().trim_matches('\'').to_string();
            if o.status.success() && !s.is_empty() {
                env.gnome = Some(s);
            }
        }
    }
    env
}

/// Terminal que se usará con esta configuración (bloquea: puede consultar gsettings).
pub fn detect(configured: &str, dir: &Path) -> Option<Spec> {
    let _ = dir;
    #[cfg(all(unix, not(any(target_os = "macos", target_os = "android"))))]
    {
        return detect_posix(&process_env(configured));
    }
    #[cfg(windows)]
    {
        return Some(detect_windows(configured, &process_path(), dir));
    }
    #[cfg(target_os = "macos")]
    {
        return Some(detect_macos(configured));
    }
    #[allow(unreachable_code)]
    {
        let _ = configured;
        None
    }
}

/// Nombre del terminal detectado automáticamente (sugerencia del campo en Opciones). La primera
/// llamada lanza la detección en segundo plano y devuelve `None` hasta que termina.
pub fn auto_label() -> Option<String> {
    use std::sync::OnceLock;
    use std::sync::atomic::{AtomicBool, Ordering};
    static LABEL: OnceLock<String> = OnceLock::new();
    static STARTED: AtomicBool = AtomicBool::new(false);
    if let Some(l) = LABEL.get() {
        return Some(l.clone());
    }
    if supported() && !STARTED.swap(true, Ordering::AcqRel) {
        crate::vfs::thread::spawn(|| {
            let home = dirs::home_dir().unwrap_or_default();
            let l = detect("", &home).map(|s| {
                let l = s.label();
                Path::new(&l).file_name().map(|n| n.to_string_lossy().into_owned()).filter(|_| !l.contains(' ')).unwrap_or(l)
            });
            let _ = LABEL.set(l.unwrap_or_default());
        });
    }
    None
}

/// Lanza `l` desacoplado (recogiendo el proceso al terminar); en Flatpak, en el anfitrión.
pub fn spawn(l: &Launch) -> Result<(), String> {
    let mut c = if in_flatpak() {
        let mut c = std::process::Command::new("flatpak-spawn");
        let mut d = OsString::from("--directory=");
        d.push(&l.cwd);
        // lo encontrado en /run/host/usr/bin está en /usr/bin para el anfitrión
        let prog = Path::new(&l.program);
        let prog = prog.strip_prefix("/run/host").map(|p| Path::new("/").join(p)).unwrap_or_else(|_| prog.to_path_buf());
        c.arg("--host").arg(d).arg(prog);
        c
    } else {
        let mut c = std::process::Command::new(&l.program);
        c.current_dir(&l.cwd);
        c
    };
    c.args(&l.args);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // CREATE_NEW_CONSOLE para un shell; CREATE_NO_WINDOW para lo demás (wt.exe abre su ventana)
        c.creation_flags(if l.new_console { 0x0000_0010 } else { 0x0800_0000 });
    }
    if in_flatpak() {
        // sin el permiso org.freedesktop.Flatpak, flatpak-spawn sale enseguida con error: se espera
        // un momento para avisar en vez de dar por abierto un terminal que no aparece
        use std::process::Stdio;
        c.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
        let mut child = c.spawn().map_err(|e| e.to_string())?;
        let r = wait_start(&mut child, std::time::Duration::from_millis(1500));
        let _ = crate::vfs::thread::Builder::new().name("reap".into()).spawn(move || {
            let _ = child.wait();
        });
        return r.map_err(|code| {
            let id = std::env::var("FLATPAK_ID").unwrap_or_else(|_| "io.github.apexdesk.FileBrowser".into());
            crate::i18n::tf(
                "flatpak-spawn falló (código {}): falta el permiso org.freedesktop.Flatpak. Para concederlo: flatpak override --user --talk-name=org.freedesktop.Flatpak {}",
                &[&code.map_or_else(|| "?".to_string(), |c| c.to_string()), &id],
            )
        });
    }
    crate::extras::spawn_detached(c, !l.new_console)
}

/// Espera hasta `window` a que `child` arranque: Err(código) si termina antes con error; Ok si
/// sigue en marcha o terminó bien (un lanzador que delega en otro proceso).
fn wait_start(child: &mut std::process::Child, window: std::time::Duration) -> Result<(), Option<i32>> {
    let t0 = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(st)) if st.success() => return Ok(()),
            Ok(Some(st)) => return Err(st.code()),
            Ok(None) if t0.elapsed() < window => std::thread::sleep(std::time::Duration::from_millis(50)),
            _ => return Ok(()),
        }
    }
}

/// Abre el terminal del sistema en `dir`. Bloquea un momento (detección): llamar desde un hilo.
pub fn open_dir(configured: &str, dir: &Path) -> Result<(), String> {
    if !supported() {
        return Err(crate::i18n::t("sin terminal del sistema").into());
    }
    let spec = detect(configured, dir).ok_or_else(|| crate::i18n::t("no se encontró ningún terminal").to_string())?;
    spawn(&spec.open(dir))
}

/// Ejecuta `argv` en el terminal del sistema (aplicaciones con `Terminal=true`).
pub fn run_in(configured: &str, dir: &Path, argv: &[OsString]) -> Result<(), String> {
    #[cfg(all(unix, not(any(target_os = "macos", target_os = "android"))))]
    {
        let spec = detect_posix_exec(&process_env(configured)).ok_or_else(|| crate::i18n::t("no se encontró ningún terminal").to_string())?;
        let l = spec.exec(dir, argv).ok_or_else(|| crate::i18n::t("el terminal no puede ejecutar órdenes").to_string())?;
        return spawn(&l);
    }
    #[allow(unreachable_code)]
    {
        let spec = detect(configured, dir).ok_or_else(|| crate::i18n::t("no se encontró ningún terminal").to_string())?;
        let l = spec.exec(dir, argv).ok_or_else(|| crate::i18n::t("el terminal no puede ejecutar órdenes").to_string())?;
        spawn(&l)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(all(unix, not(target_arch = "wasm32")))]
    #[test]
    fn arranque_fallido_se_detecta() {
        use std::time::Duration;
        let run = |code: &str| std::process::Command::new("sh").arg("-c").arg(code).spawn().unwrap();
        // como flatpak-spawn sin permiso: sale enseguida con error
        let mut c = run("exit 1");
        assert_eq!(wait_start(&mut c, Duration::from_secs(5)), Err(Some(1)));
        let mut c = run("exit 0");
        assert_eq!(wait_start(&mut c, Duration::from_secs(5)), Ok(()));
        // sigue en marcha pasado el plazo: abierto
        let mut c = run("sleep 5");
        let t0 = std::time::Instant::now();
        assert_eq!(wait_start(&mut c, Duration::from_millis(200)), Ok(()));
        assert!(t0.elapsed() < Duration::from_secs(3));
        let _ = c.kill();
        let _ = c.wait();
    }

    fn s(v: &[OsString]) -> Vec<String> {
        v.iter().map(|a| a.to_string_lossy().into_owned()).collect()
    }

    #[cfg(unix)]
    fn fake_path(bins: &[&str]) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let n = web_time::SystemTime::now().duration_since(web_time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
        let d = std::env::temp_dir().join(format!("fbw2-term-{}-{n:x}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        for b in bins {
            let f = d.join(b);
            std::fs::write(&f, b"#!/bin/sh\n").unwrap();
            std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        // un archivo sin permiso de ejecución no cuenta
        std::fs::write(d.join("alacritty"), b"").unwrap();
        d
    }

    #[cfg(unix)]
    #[test]
    fn orden_de_deteccion() {
        let d = fake_path(&["xterm", "konsole", "kitty", "gnome-terminal", "xdg-terminal-exec", "myterm"]);
        let mut env = TermEnv { path: vec![d.clone()], ..Default::default() };
        let bin = |e: &TermEnv| detect_posix(e).map(|s| Path::new(&s.argv[0]).file_name().unwrap().to_string_lossy().into_owned());
        // configurado > $TERMINAL > xdg-terminal-exec > escritorio > lista
        env.configured = "myterm --flag".into();
        env.terminal_var = "kitty".into();
        assert_eq!(bin(&env).as_deref(), Some("myterm"));
        env.configured = "noexiste".into();
        assert_eq!(bin(&env).as_deref(), Some("kitty"));
        env.terminal_var.clear();
        assert_eq!(bin(&env).as_deref(), Some("xdg-terminal-exec"));
        std::fs::remove_file(d.join("xdg-terminal-exec")).unwrap();
        env.gnome = Some("gnome-terminal".into());
        env.kde = Some("konsole".into());
        assert_eq!(bin(&env).as_deref(), Some("gnome-terminal"));
        env.gnome = None;
        assert_eq!(bin(&env).as_deref(), Some("konsole"));
        env.kde = None;
        // lista conocida: gnome-terminal va antes que konsole, kitty y xterm; alacritty no es ejecutable
        assert_eq!(bin(&env).as_deref(), Some("gnome-terminal"));
        std::fs::remove_file(d.join("gnome-terminal")).unwrap();
        assert_eq!(bin(&env).as_deref(), Some("konsole"));
        env.path = vec![d.join("nada")];
        assert!(detect_posix(&env).is_none());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn argumentos_de_carpeta_posix() {
        let hostile = Path::new("/t/a b/$(rm -rf ~)/'q\"/;x");
        let k = |b: &str| Spec::of_known(&format!("/usr/bin/{b}"), known(b).unwrap());
        let l = k("gnome-terminal").open(hostile);
        assert_eq!(s(&l.args), [format!("--working-directory={}", hostile.display())]);
        assert_eq!(l.cwd, hostile);
        assert_eq!(s(&k("konsole").open(hostile).args), ["--workdir".to_string(), hostile.display().to_string()]);
        assert_eq!(s(&k("wezterm").open(hostile).args), ["start".to_string(), "--cwd".into(), hostile.display().to_string()]);
        assert!(k("xterm").open(hostile).args.is_empty());
        // orden propia con {dir}: la carpeta es un argumento entero, sin intérprete
        let c = Spec::custom("myterm --cd={dir} -T 'Mi terminal'").unwrap();
        assert_eq!(s(&c.open(hostile).args), [format!("--cd={}", hostile.display()), "-T".into(), "Mi terminal".into()]);
        // orden propia sin {dir} de un terminal conocido: se añade su opción
        let c = Spec::custom("alacritty").unwrap();
        assert_eq!(s(&c.open(hostile).args), ["--working-directory".to_string(), hostile.display().to_string()]);
        // ejecutar una aplicación (Terminal=true)
        let argv: Vec<OsString> = vec!["htop".into(), "/t/$(x)".into()];
        assert_eq!(s(&k("gnome-terminal").exec(hostile, &argv).unwrap().args), ["--", "htop", "/t/$(x)"]);
        assert_eq!(s(&k("kitty").exec(hostile, &argv).unwrap().args), ["htop", "/t/$(x)"]);
        assert_eq!(s(&k("wezterm").exec(hostile, &argv).unwrap().args), ["start", "--", "htop", "/t/$(x)"]);
        assert!(k("tilix").exec(hostile, &argv).is_none());
    }

    #[test]
    fn argumentos_de_carpeta_windows() {
        for dir in [r"C:\Users\Ana\a b", r"C:\x\$(calc)\%PATH%\a&b", r"C:\it's ‘q’", r"\\srv\share\d"] {
            let p = Path::new(dir);
            let ps = detect_windows("powershell.exe", &[], p).open(p);
            assert_eq!(ps.program, OsString::from("powershell.exe"));
            assert!(ps.new_console);
            let a = s(&ps.args);
            assert_eq!(a[..2], ["-NoExit", "-Command"]);
            assert_eq!(a.len(), 3, "la orden es un único argumento");
            // entre comillas simples, con las comillas simples dobladas
            let inner = a[2].strip_prefix("Set-Location -LiteralPath '").and_then(|x| x.strip_suffix('\'')).unwrap();
            let mut chars = inner.chars().peekable();
            while let Some(c) = chars.next() {
                if matches!(c, '\'' | '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}') {
                    assert_eq!(chars.next(), Some(c), "comilla sin doblar en {dir}");
                }
            }
            // cmd: la ruta no va en la línea de órdenes (sólo carpeta de trabajo)
            let cmd = detect_windows("cmd", &[], p).open(p);
            assert_eq!(s(&cmd.args), ["/K"]);
            assert_eq!(cmd.cwd, p);
            // Windows Terminal: la ruta como un solo argumento tras -d
            let wt = detect_windows("wt.exe", &[], p).open(p);
            assert_eq!(s(&wt.args), ["-d".to_string(), dir.to_string()]);
        }
        assert_eq!(ps_set_location("C:\\a'b"), "Set-Location -LiteralPath 'C:\\a''b'");
        // «;» separa órdenes en wt: se usa PowerShell
        let p = Path::new(r"C:\a;b");
        assert_eq!(detect_windows("wt", &[], p).open(p).program, OsString::from("powershell.exe"));
        // nada instalado en PATH: cmd
        assert_eq!(detect_windows("", &[], Path::new("C:\\")).open(Path::new("C:\\")).program, OsString::from("cmd.exe"));
    }

    #[test]
    fn macos_open_a() {
        let p = Path::new("/Users/ana/a b");
        let l = detect_macos("").open(p);
        assert_eq!(l.program, OsString::from("open"));
        assert_eq!(s(&l.args), ["-a", "Terminal", "/Users/ana/a b"]);
        assert_eq!(s(&detect_macos("iTerm.app").open(p).args), ["-a", "iTerm", "/Users/ana/a b"]);
    }

    #[test]
    fn palabras_y_kde() {
        assert_eq!(split_words(r#"a 'b c' "d \"e\"" f\ g"#).unwrap(), if cfg!(windows) { vec!["a", "b c", "d \"e\"", "f\\", "g"] } else { vec!["a", "b c", "d \"e\"", "f g"] });
        assert!(split_words("a 'b").is_none());
        assert_eq!(kde_terminal("[General]\nTerminalApplication=kitty\n[KDE]\nx=1"), Some("kitty".into()));
        assert_eq!(kde_terminal("[KDE]\nTerminalApplication=kitty"), None);
    }
}
