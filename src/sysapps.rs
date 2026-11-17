//! Aplicaciones del sistema para «Abrir con»: qué aplicaciones abren un archivo, cuál es la
//! predeterminada, la lista de todas las instaladas y sus iconos.
//! - Linux/BSD: archivos .desktop de las carpetas XDG (más Flatpak y Snap), mimeapps.list en todas
//!   las ubicaciones de la especificación, base de datos MIME compartida (globs2, subclases, alias).
//! - Windows: manejadores de asociación del Shell (`SHAssocEnumHandlers` / `IAssocHandler`).
//! - macOS: Launch Services (`LSCopyApplicationURLsForURL`) y las carpetas de aplicaciones.
//! Todo lo que lee disco o lanza procesos bloquea: se llama desde hilos. El catálogo se guarda
//! durante la sesión.
#![cfg_attr(not(all(unix, not(any(target_os = "macos", target_os = "android")))), allow(dead_code))]

use crate::extras::App;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// Aplicaciones para un archivo.
#[derive(Clone, Default)]
pub struct Choices {
    /// tipo MIME (Linux), extensión con punto (Windows) o vacío
    pub mime: String,
    /// descripción legible del tipo («Documento de texto»)
    pub type_name: String,
    /// registradas para el tipo; la predeterminada primero (con `default`)
    pub recommended: Vec<App>,
    /// el resto de aplicaciones instaladas, por orden alfabético
    pub others: Vec<App>,
    /// se puede fijar la predeterminada desde aquí
    pub can_default: bool,
}

/// ¿Hay elección de aplicación en esta plataforma? (En la web no.)
pub fn supported() -> bool {
    cfg!(not(target_arch = "wasm32"))
}

/// Consulta las aplicaciones para `path`. Bloquea.
pub fn choices_for(path: &Path) -> Choices {
    #[cfg(all(unix, not(any(target_os = "macos", target_os = "android"))))]
    {
        return fd::choices(path);
    }
    #[cfg(windows)]
    {
        return win::choices(path);
    }
    #[cfg(target_os = "macos")]
    {
        return mac::choices(path);
    }
    #[allow(unreachable_code)]
    {
        let _ = path;
        Choices::default()
    }
}

/// Qué pasó al fijar la aplicación predeterminada.
#[derive(Debug, PartialEq, Eq)]
pub enum Defaulted {
    /// fijada y comprobada
    Done,
    /// Windows no deja fijarla desde un programa: el usuario tiene que confirmarla en la ventana
    /// «Abrir con» del sistema (Windows 8 y posteriores protegen la elección con un hash)
    NeedsSystemUi,
}

/// Fija `app` como predeterminada para el tipo de `c`. Bloquea.
pub fn set_default(c: &Choices, app: &App) -> Result<Defaulted, String> {
    #[cfg(all(unix, not(any(target_os = "macos", target_os = "android"))))]
    {
        return fd::set_default(&c.mime, &app.id).map(|()| Defaulted::Done);
    }
    #[cfg(windows)]
    {
        return win::make_default(&c.mime, &app.id);
    }
    #[allow(unreachable_code)]
    {
        let _ = (c, app);
        Err(crate::i18n::t("no disponible en este sistema").into())
    }
}

/// Icono de una aplicación ya decodificado (como mucho `px` de lado). Bloquea.
pub fn load_icon(app: &App, px: u32) -> Option<crate::thumbs::Decoded> {
    #[cfg(all(unix, not(any(target_os = "macos", target_os = "android"))))]
    {
        let p = fd::icon_path(&app.icon)?;
        return crate::thumbs::guarded(|| crate::thumbs::decode_image(&p, px)).flatten();
    }
    #[allow(unreachable_code)]
    {
        let _ = (app, px);
        None
    }
}

#[cfg(windows)]
pub use win::{choose_default_dialog as win_choose_default_dialog, invoke as win_invoke, is_default as win_is_default, open_with_dialog as win_open_with_dialog};

// ---------------------------------------------------------------------------
// Formato de los .desktop y de mimeapps.list (puro: se prueba en cualquier plataforma)
// ---------------------------------------------------------------------------

/// Quita los escapes de un valor de tipo cadena (`\s`, `\n`, `\t`, `\r`, `\\`).
pub fn unescape(v: &str) -> String {
    let mut out = String::with_capacity(v.len());
    let mut it = v.chars();
    while let Some(c) = it.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match it.next() {
            Some('s') => out.push(' '),
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some(o) => out.push(o),
            None => {}
        }
    }
    out
}

/// Parte una lista separada por «;» (con `\;` como «;» literal); sin elementos vacíos.
pub fn split_list(v: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut it = v.chars();
    while let Some(c) = it.next() {
        match c {
            '\\' => match it.next() {
                Some(';') => cur.push(';'),
                Some(o) => {
                    cur.push('\\');
                    cur.push(o);
                }
                None => {}
            },
            ';' => {
                let t = cur.trim();
                if !t.is_empty() {
                    out.push(t.to_string());
                }
                cur.clear();
            }
            o => cur.push(o),
        }
    }
    let t = cur.trim();
    if !t.is_empty() {
        out.push(t.to_string());
    }
    out
}

/// Claves de idioma para `Name[xx]`, de la más a la menos concreta, para el idioma de la interfaz
/// `ui` («es», «zh»…) y la variable de entorno de idioma (`es_ES.UTF-8@euro`).
pub fn locale_keys(ui: &str, env_lang: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let e = env_lang.split('.').next().unwrap_or("");
    let (base, modi) = match env_lang.split_once('@') {
        Some((_, m)) => (e.split('@').next().unwrap_or(""), Some(m)),
        None => (e, None),
    };
    let (lang, country) = match base.split_once('_') {
        Some((l, c)) => (l, Some(c)),
        None => (base, None),
    };
    let mut add = |s: String| {
        if !s.is_empty() && !out.contains(&s) {
            out.push(s);
        }
    };
    if !lang.is_empty() && lang == ui {
        if let (Some(c), Some(m)) = (country, modi) {
            add(format!("{lang}_{c}@{m}"));
        }
        if let Some(c) = country {
            add(format!("{lang}_{c}"));
        }
        if let Some(m) = modi {
            add(format!("{lang}@{m}"));
        }
    }
    // variante más habitual de las traducciones de la interfaz
    match ui {
        "zh" => add("zh_CN".into()),
        "pt" => add("pt_PT".into()),
        _ => {}
    }
    add(ui.to_string());
    out
}

/// Entrada [Desktop Entry] de un archivo .desktop.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DesktopEntry {
    pub id: String,
    pub path: PathBuf,
    pub name: String,
    pub generic: String,
    pub comment: String,
    /// tal cual (los escapes los resuelve `extras::exec_argv`)
    pub exec: String,
    pub try_exec: String,
    pub icon: String,
    pub terminal: bool,
    pub no_display: bool,
    pub hidden: bool,
    pub application: bool,
    pub only_show_in: Vec<String>,
    pub not_show_in: Vec<String>,
    pub mime: Vec<String>,
}

/// Lee la sección [Desktop Entry]; `langs` son las claves de idioma por preferencia.
pub fn parse_desktop(txt: &str, langs: &[String]) -> DesktopEntry {
    let mut e = DesktopEntry::default();
    let mut in_entry = false;
    // (clave, idioma) → valor de las claves traducibles
    let mut loc: HashMap<(&str, &str), &str> = HashMap::new();
    for l in txt.lines() {
        let l = l.trim_start();
        if l.starts_with('#') || l.is_empty() {
            continue;
        }
        if l.starts_with('[') {
            in_entry = l.trim_end() == "[Desktop Entry]";
            continue;
        }
        if !in_entry {
            continue;
        }
        let Some((k, v)) = l.split_once('=') else { continue };
        let (k, v) = (k.trim(), v.trim());
        if let Some((base, lang)) = k.strip_suffix(']').and_then(|k| k.split_once('[')) {
            if matches!(base, "Name" | "GenericName" | "Comment") {
                loc.entry((base, lang)).or_insert(v);
            }
            continue;
        }
        match k {
            "Type" => e.application = v == "Application",
            "Name" if e.name.is_empty() => e.name = unescape(v),
            "GenericName" if e.generic.is_empty() => e.generic = unescape(v),
            "Comment" if e.comment.is_empty() => e.comment = unescape(v),
            "Exec" if e.exec.is_empty() => e.exec = v.to_string(),
            "TryExec" if e.try_exec.is_empty() => e.try_exec = unescape(v),
            "Icon" if e.icon.is_empty() => e.icon = unescape(v),
            "Terminal" => e.terminal = v == "true",
            "NoDisplay" => e.no_display = v == "true",
            "Hidden" => e.hidden = v == "true",
            "OnlyShowIn" => e.only_show_in = split_list(v),
            "NotShowIn" => e.not_show_in = split_list(v),
            "MimeType" => e.mime = split_list(v),
            _ => {}
        }
    }
    for (key, field) in [("Name", &mut e.name), ("GenericName", &mut e.generic), ("Comment", &mut e.comment)] {
        if let Some(v) = langs.iter().find_map(|l| loc.get(&(key, l.as_str()))) {
            *field = unescape(v);
        }
    }
    e
}

impl DesktopEntry {
    /// ¿Se puede usar en este escritorio? (no borrada, con Exec, OnlyShowIn/NotShowIn, TryExec)
    pub fn usable(&self, desktops: &[String], path: &[PathBuf]) -> bool {
        if !self.application || self.hidden || self.exec.trim().is_empty() || self.name.is_empty() {
            return false;
        }
        let has = |l: &[String]| l.iter().any(|d| desktops.iter().any(|x| x.eq_ignore_ascii_case(d)));
        if !self.only_show_in.is_empty() && !has(&self.only_show_in) {
            return false;
        }
        if has(&self.not_show_in) {
            return false;
        }
        if self.try_exec.is_empty() || crate::systerm::find_in(path, &self.try_exec).is_some() {
            return true;
        }
        // ruta absoluta del anfitrión vista desde Flatpak (/usr/bin/x → /run/host/usr/bin/x)
        let p = Path::new(&self.try_exec);
        p.is_absolute()
            && crate::systerm::in_flatpak()
            && p.strip_prefix("/").is_ok_and(|r| crate::systerm::find_in(&[], &Path::new("/run/host").join(r).to_string_lossy()).is_some())
    }

    pub fn to_app(&self) -> App {
        App {
            name: self.name.clone(),
            exec: self.exec.clone(),
            icon: self.icon.clone(),
            desktop: self.path.clone(),
            id: self.id.clone(),
            generic: if self.generic.is_empty() { self.comment.clone() } else { self.generic.clone() },
            terminal: self.terminal,
            default: false,
        }
    }
}

/// Grupos de un mimeapps.list.
#[derive(Clone, Debug, Default)]
pub struct MimeApps {
    pub default: HashMap<String, Vec<String>>,
    pub added: HashMap<String, Vec<String>>,
    pub removed: HashMap<String, Vec<String>>,
}

pub fn parse_mimeapps(txt: &str) -> MimeApps {
    let mut m = MimeApps::default();
    let mut group = "";
    for l in txt.lines() {
        let l = l.trim();
        if l.starts_with('#') || l.is_empty() {
            continue;
        }
        if l.starts_with('[') && l.ends_with(']') {
            group = &l[1..l.len() - 1];
            continue;
        }
        let Some((k, v)) = l.split_once('=') else { continue };
        let target = match group {
            "Default Applications" => &mut m.default,
            "Added Associations" => &mut m.added,
            "Removed Associations" => &mut m.removed,
            _ => continue,
        };
        // una clave repetida en el mismo grupo: vale la primera
        target.entry(k.trim().to_string()).or_insert_with(|| split_list(v));
    }
    m
}

/// Devuelve `txt` (un mimeapps.list) con `id` como predeterminada de `mime`: en [Default
/// Applications] la línea de `mime` pasa a ser `mime=id;`, en [Added Associations] `id` queda la
/// primera y se quita de [Removed Associations]. Todo lo demás (comentarios, otros grupos, orden) se
/// conserva tal cual; los grupos que falten se añaden al final.
pub fn set_default_text(txt: &str, mime: &str, id: &str) -> String {
    const DEF: &str = "Default Applications";
    const ADD: &str = "Added Associations";
    const REM: &str = "Removed Associations";
    let mut out: Vec<String> = Vec::new();
    let (mut done_def, mut done_add) = (false, false);
    let (mut seen_def, mut seen_add) = (false, false);
    let mut group = String::new();
    // al cerrar un grupo sin la línea, se añade tras su último renglón no vacío
    let finish = |out: &mut Vec<String>, group: &str, done_def: &mut bool, done_add: &mut bool| {
        let line = if group == DEF && !*done_def {
            *done_def = true;
            format!("{mime}={id};")
        } else if group == ADD && !*done_add {
            *done_add = true;
            format!("{mime}={id};")
        } else {
            return;
        };
        let at = out.iter().rposition(|l| !l.trim().is_empty()).map(|i| i + 1).unwrap_or(out.len());
        out.insert(at, line);
    };
    for line in txt.lines() {
        let t = line.trim();
        if t.starts_with('[') && t.ends_with(']') {
            finish(&mut out, &group, &mut done_def, &mut done_add);
            group = t[1..t.len() - 1].to_string();
            seen_def |= group == DEF;
            seen_add |= group == ADD;
            out.push(line.to_string());
            continue;
        }
        if !t.starts_with('#') {
            if let Some((k, v)) = t.split_once('=') {
                if k.trim() == mime {
                    match group.as_str() {
                        DEF => {
                            if !done_def {
                                out.push(format!("{mime}={id};"));
                                done_def = true;
                            }
                            continue;
                        }
                        ADD => {
                            if !done_add {
                                let mut l: Vec<String> = split_list(v).into_iter().filter(|x| x != id).collect();
                                l.insert(0, id.to_string());
                                out.push(format!("{mime}={};", l.join(";")));
                                done_add = true;
                            }
                            continue;
                        }
                        REM => {
                            let l: Vec<String> = split_list(v).into_iter().filter(|x| x != id).collect();
                            if !l.is_empty() {
                                out.push(format!("{mime}={};", l.join(";")));
                            }
                            continue;
                        }
                        _ => {}
                    }
                }
            }
        }
        out.push(line.to_string());
    }
    finish(&mut out, &group, &mut done_def, &mut done_add);
    for (seen, name) in [(seen_def, DEF), (seen_add, ADD)] {
        if !seen {
            if out.last().is_some_and(|l| !l.trim().is_empty()) {
                out.push(String::new());
            }
            out.push(format!("[{name}]"));
            out.push(format!("{mime}={id};"));
        }
    }
    let mut s = out.join("\n");
    s.push('\n');
    s
}

// ---------------------------------------------------------------------------
// Base de datos MIME compartida (globs2, alias, subclases)
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
enum Tok {
    Any,
    Star,
    Lit(char),
    Class(Vec<(char, char)>, bool),
}

fn glob_tokens(p: &str) -> Vec<Tok> {
    let mut out = Vec::new();
    let mut it = p.chars().peekable();
    while let Some(c) = it.next() {
        match c {
            '*' => out.push(Tok::Star),
            '?' => out.push(Tok::Any),
            '[' => {
                let mut set = Vec::new();
                let neg = matches!(it.peek(), Some('!' | '^'));
                if neg {
                    it.next();
                }
                let mut closed = false;
                while let Some(a) = it.next() {
                    if a == ']' && !set.is_empty() {
                        closed = true;
                        break;
                    }
                    if it.peek() == Some(&'-') {
                        it.next();
                        match it.next() {
                            Some(']') | None => {
                                set.push((a, a));
                                set.push(('-', '-'));
                                closed = true;
                                break;
                            }
                            Some(b) => set.push((a, b)),
                        }
                    } else {
                        set.push((a, a));
                    }
                }
                if closed {
                    out.push(Tok::Class(set, neg));
                } else {
                    out.push(Tok::Lit('['));
                }
            }
            o => out.push(Tok::Lit(o)),
        }
    }
    out
}

fn tok_match(t: &Tok, c: char) -> bool {
    match t {
        Tok::Any => true,
        Tok::Lit(l) => *l == c,
        Tok::Class(set, neg) => set.iter().any(|(a, b)| *a <= c && c <= *b) != *neg,
        Tok::Star => false,
    }
}

/// Coincidencia de patrón con `*`, `?` y `[…]` (algoritmo lineal con retroceso a la última estrella).
fn glob_match(p: &[Tok], s: &[char]) -> bool {
    let (mut pi, mut si) = (0usize, 0usize);
    let mut star: Option<(usize, usize)> = None;
    while si < s.len() {
        if pi < p.len() && !matches!(p[pi], Tok::Star) && tok_match(&p[pi], s[si]) {
            pi += 1;
            si += 1;
        } else if pi < p.len() && matches!(p[pi], Tok::Star) {
            star = Some((pi, si));
            pi += 1;
        } else if let Some((sp, ss)) = star {
            pi = sp + 1;
            si = ss + 1;
            star = Some((sp, ss + 1));
        } else {
            return false;
        }
    }
    while pi < p.len() && matches!(p[pi], Tok::Star) {
        pi += 1;
    }
    pi == p.len()
}

#[derive(Clone, Debug)]
struct Glob {
    weight: u32,
    pat: String,
    toks: Vec<Tok>,
    cs: bool,
    mime: String,
}

#[derive(Clone, Debug, Default)]
pub struct MimeDb {
    globs: Vec<Glob>,
    /// «*.ext» sin comodines más, por extensión en minúsculas: (peso, mime, distingue mayúsculas, ext original)
    ext: HashMap<String, Vec<(u32, String, bool, String)>>,
    aliases: HashMap<String, String>,
    parents: HashMap<String, Vec<String>>,
}

impl MimeDb {
    /// Añade un globs2 («peso:tipo:patrón[:flags]»). Las carpetas de más prioridad van primero.
    pub fn add_globs2(&mut self, txt: &str) {
        for l in txt.lines() {
            if l.starts_with('#') {
                continue;
            }
            let mut it = l.split(':');
            let (Some(w), Some(mime), Some(pat)) = (it.next(), it.next(), it.next()) else { continue };
            let cs = it.next().is_some_and(|f| f.split(',').any(|x| x == "cs"));
            let Ok(weight) = w.parse::<u32>() else { continue };
            if mime.is_empty() || pat.is_empty() || pat == "__NOGLOBS__" {
                continue;
            }
            if let Some(e) = pat.strip_prefix("*.").filter(|e| !e.contains(['*', '?', '['])) {
                self.ext.entry(e.to_lowercase()).or_default().push((weight, mime.to_string(), cs, e.to_string()));
                continue;
            }
            let p = if cs { pat.to_string() } else { pat.to_lowercase() };
            self.globs.push(Glob { weight, toks: glob_tokens(&p), pat: p, cs, mime: mime.to_string() });
        }
    }

    pub fn add_aliases(&mut self, txt: &str) {
        for l in txt.lines() {
            if let Some((a, c)) = l.trim().split_once(' ') {
                self.aliases.entry(a.to_string()).or_insert_with(|| c.trim().to_string());
            }
        }
    }

    pub fn add_subclasses(&mut self, txt: &str) {
        for l in txt.lines() {
            if let Some((c, p)) = l.trim().split_once(' ') {
                let v = self.parents.entry(c.to_string()).or_default();
                if !v.iter().any(|x| x == p.trim()) {
                    v.push(p.trim().to_string());
                }
            }
        }
    }

    pub fn canonical(&self, m: &str) -> String {
        self.aliases.get(m).cloned().unwrap_or_else(|| m.to_string())
    }

    /// Tipo según el nombre: el patrón de más peso y, a igual peso, el más largo.
    pub fn by_name(&self, name: &str) -> Option<String> {
        let lower = name.to_lowercase();
        // (peso, longitud, distingue mayúsculas, tipo): a igualdad gana el que distingue mayúsculas
        let mut best: Option<(u32, usize, bool, &str)> = None;
        // extensiones: todas las colas «.a.b», «.b» del nombre
        let mut i = 0;
        while let Some(off) = lower[i..].find('.') {
            let ext = &lower[i + off + 1..];
            if let Some(v) = self.ext.get(ext) {
                for (w, m, cs, orig) in v {
                    if *cs && !name.ends_with(&format!(".{orig}")) {
                        continue;
                    }
                    let len = ext.len() + 2;
                    if best.is_none_or(|(bw, bl, bc, _)| (*w, len, *cs) > (bw, bl, bc)) {
                        best = Some((*w, len, *cs, m.as_str()));
                    }
                }
            }
            i += off + 1;
        }
        let chars: Vec<char> = name.chars().collect();
        let lchars: Vec<char> = lower.chars().collect();
        for g in &self.globs {
            if best.is_some_and(|(bw, bl, bc, _)| (g.weight, g.pat.len(), g.cs) <= (bw, bl, bc)) {
                continue;
            }
            if glob_match(&g.toks, if g.cs { &chars } else { &lchars }) {
                best = Some((g.weight, g.pat.len(), g.cs, g.mime.as_str()));
            }
        }
        best.map(|b| self.canonical(b.3))
    }

    /// El tipo y sus antecesores (subclases), del más concreto al más general; todo `text/*` acaba en
    /// `text/plain`. No incluye `application/octet-stream` (lo abriría cualquier cosa).
    pub fn with_parents(&self, m: &str) -> Vec<String> {
        let mut out = vec![self.canonical(m)];
        let mut i = 0;
        while i < out.len() && out.len() < 16 {
            let cur = out[i].clone();
            let mut ps: Vec<String> = self.parents.get(&cur).cloned().unwrap_or_default();
            if cur.starts_with("text/") && cur != "text/plain" {
                ps.push("text/plain".into());
            }
            for p in ps {
                let p = self.canonical(&p);
                if p != "application/octet-stream" && !out.contains(&p) {
                    out.push(p);
                }
            }
            i += 1;
        }
        out
    }
}

/// Tipo de un archivo sin patrón conocido, mirando sus primeros bytes: vacío, texto o binario.
pub fn sniff(head: &[u8]) -> &'static str {
    if head.is_empty() {
        return "application/x-zerosize";
    }
    let text = !head.contains(&0)
        && match std::str::from_utf8(head) {
            Ok(_) => true,
            // un carácter cortado al final del bloque leído no lo hace binario
            Err(e) => e.error_len().is_none() && e.valid_up_to() + 4 > head.len(),
        };
    if text { "text/plain" } else { "application/octet-stream" }
}

/// ¿Es `m` un nombre de tipo MIME seguro para usarlo en una ruta (`tipo/subtipo`)?
pub fn mime_ok(m: &str) -> bool {
    let mut parts = m.split('/');
    let ok = |s: &str| !s.is_empty() && !s.starts_with('.') && s.chars().all(|c| c.is_ascii_alphanumeric() || "+-._".contains(c));
    matches!((parts.next(), parts.next(), parts.next()), (Some(a), Some(b), None) if ok(a) && ok(b))
}

/// Comentario traducido de `mime/<tipo>.xml` (descripción legible del tipo).
pub fn mime_comment(xml: &str, langs: &[String]) -> Option<String> {
    let mut plain: Option<String> = None;
    let mut by_lang: HashMap<String, String> = HashMap::new();
    let mut rest = xml;
    while let Some(i) = rest.find("<comment") {
        rest = &rest[i + 8..];
        let Some(gt) = rest.find('>') else { break };
        let attrs = &rest[..gt];
        let Some(end) = rest[gt + 1..].find("</comment>") else { break };
        let text = xml_unescape(rest[gt + 1..gt + 1 + end].trim());
        match attrs.split_once("xml:lang=\"").and_then(|(_, r)| r.split_once('"')) {
            Some((l, _)) => {
                by_lang.entry(l.to_string()).or_insert(text);
            }
            None if attrs.trim().is_empty() => {
                plain.get_or_insert(text);
            }
            None => {}
        }
        rest = &rest[gt + 1 + end..];
    }
    langs.iter().find_map(|l| by_lang.get(l).cloned()).or(plain).filter(|s| !s.is_empty())
}

fn xml_unescape(s: &str) -> String {
    s.replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&apos;", "'").replace("&amp;", "&")
}

// ---------------------------------------------------------------------------
// Entorno XDG y catálogo (Linux/BSD)
// ---------------------------------------------------------------------------

/// Carpetas y datos del entorno que intervienen (separado para poder probarlo con carpetas temporales).
#[derive(Clone, Debug, Default)]
pub struct XdgEnv {
    /// XDG_DATA_HOME primero, luego XDG_DATA_DIRS (y las de Flatpak y Snap)
    pub data_dirs: Vec<PathBuf>,
    pub config_home: PathBuf,
    pub config_dirs: Vec<PathBuf>,
    /// XDG_CURRENT_DESKTOP, en minúsculas
    pub desktops: Vec<String>,
    /// claves de idioma (`locale_keys`)
    pub langs: Vec<String>,
    /// PATH (para TryExec)
    pub path: Vec<PathBuf>,
}

impl XdgEnv {
    pub fn from_process(ui: &str) -> XdgEnv {
        let home = dirs::home_dir().unwrap_or_default();
        let flatpak = crate::systerm::in_flatpak();
        let var = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
        // en Flatpak XDG_DATA_HOME y XDG_CONFIG_HOME apuntan a ~/.var/app/<id>: las asociaciones
        // reales del usuario están en ~/.local/share y ~/.config (visibles con --filesystem=host)
        let home_var = |k: &str| if flatpak { None } else { var(k) };
        let split = |v: String| v.split(':').filter(|s| !s.is_empty()).map(PathBuf::from).collect::<Vec<_>>();
        let mut data_dirs = vec![home_var("XDG_DATA_HOME").map(PathBuf::from).unwrap_or_else(|| home.join(".local/share"))];
        data_dirs.extend(split(var("XDG_DATA_DIRS").unwrap_or_else(|| "/usr/local/share:/usr/share".into())));
        let mut extra = vec![home.join(".local/share/flatpak/exports/share"), PathBuf::from("/var/lib/flatpak/exports/share"), PathBuf::from("/var/lib/snapd/desktop")];
        if flatpak {
            // aplicaciones del anfitrión (con --filesystem=host)
            extra.extend([PathBuf::from("/run/host/usr/share"), PathBuf::from("/run/host/usr/local/share")]);
        }
        for d in extra {
            if !data_dirs.contains(&d) {
                data_dirs.push(d);
            }
        }
        let lang = var("LC_ALL").or_else(|| var("LC_MESSAGES")).or_else(|| var("LANG")).unwrap_or_default();
        let mut config_dirs = split(var("XDG_CONFIG_DIRS").unwrap_or_else(|| "/etc/xdg".into()));
        let mut path: Vec<PathBuf> = std::env::var_os("PATH").map(|p| crate::vfs::split_paths(&p).collect()).unwrap_or_default();
        if flatpak {
            // TryExec de las aplicaciones del anfitrión: sus programas están bajo /run/host
            config_dirs.push(PathBuf::from("/run/host/etc/xdg"));
            path.extend(["/run/host/usr/local/bin", "/run/host/usr/bin", "/run/host/bin", "/var/lib/flatpak/exports/bin"].map(PathBuf::from));
            path.push(home.join(".local/share/flatpak/exports/bin"));
            path.push(home.join(".local/bin"));
        }
        XdgEnv {
            data_dirs,
            config_home: home_var("XDG_CONFIG_HOME").map(PathBuf::from).unwrap_or_else(|| home.join(".config")),
            config_dirs,
            desktops: var("XDG_CURRENT_DESKTOP").unwrap_or_default().split(':').filter(|s| !s.is_empty()).map(|s| s.to_lowercase()).collect(),
            langs: locale_keys(ui, &lang),
            path,
        }
    }

    /// Los mimeapps.list por orden de prioridad (especificación «Association between MIME types and applications»).
    pub fn mimeapps_files(&self) -> Vec<PathBuf> {
        let mut out = Vec::new();
        let mut add = |d: &Path| {
            for de in &self.desktops {
                out.push(d.join(format!("{de}-mimeapps.list")));
            }
            out.push(d.join("mimeapps.list"));
        };
        add(&self.config_home);
        for d in &self.config_dirs {
            add(d);
        }
        for d in &self.data_dirs {
            add(&d.join("applications"));
        }
        out
    }
}

/// Todas las aplicaciones y asociaciones leídas una vez.
pub struct Catalog {
    pub env: XdgEnv,
    /// por prioridad de carpeta; un id aparece una sola vez (también las borradas con Hidden)
    pub apps: Vec<DesktopEntry>,
    by_id: HashMap<String, usize>,
    lists: Vec<MimeApps>,
    pub mime: MimeDb,
}

/// Lee un archivo de texto del disco: sólo archivos normales (un FIFO o un dispositivo con ese
/// nombre no bloquea ni se lee sin fin) y de como mucho `max` bytes.
fn read_text(p: &Path, max: u64) -> Option<String> {
    use std::io::Read;
    let f = crate::thumbs::open_regular(p)?;
    let mut b = Vec::new();
    f.take(max.saturating_add(1)).read_to_end(&mut b).ok()?;
    if b.len() as u64 > max {
        return None;
    }
    String::from_utf8(b).ok()
}

/// Tope de los archivos de configuración (.desktop, mimeapps.list, XML de tipos).
const TEXT_MAX: u64 = 1 << 20;
/// Tope de las tablas de tipos (globs2, aliases, subclasses).
const MIME_MAX: u64 = 16 << 20;

/// Sello de lo que lee `Catalog::build`: fechas de las carpetas de aplicaciones y de los
/// mimeapps.list (instalar o quitar una aplicación cambia la fecha de su carpeta).
pub fn stamp(env: &XdgEnv) -> Vec<Option<std::time::SystemTime>> {
    let mt = |p: &Path| std::fs::metadata(p).and_then(|m| m.modified()).ok();
    env.data_dirs.iter().map(|d| mt(&d.join("applications"))).chain(env.mimeapps_files().iter().map(|f| mt(f))).collect()
}

fn scan_apps(root: &Path, dir: &Path, depth: u32, out: &mut Vec<(String, PathBuf)>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let mut ents: Vec<_> = rd.flatten().collect();
    ents.sort_by_key(|e| e.file_name());
    for e in ents {
        let p = e.path();
        let ft = e.file_type();
        if ft.as_ref().is_ok_and(|t| t.is_dir()) {
            if depth < 4 {
                scan_apps(root, &p, depth + 1, out);
            }
        } else if p.extension().is_some_and(|x| x == "desktop") {
            if let Ok(rel) = p.strip_prefix(root) {
                let id = rel.to_string_lossy().replace(['/', '\\'], "-");
                out.push((id, p));
            }
        }
    }
}

impl Catalog {
    pub fn build(env: XdgEnv) -> Catalog {
        let mut apps: Vec<DesktopEntry> = Vec::new();
        let mut by_id: HashMap<String, usize> = HashMap::new();
        for d in &env.data_dirs {
            let root = d.join("applications");
            let mut files = Vec::new();
            scan_apps(&root, &root, 0, &mut files);
            for (id, p) in files {
                if by_id.contains_key(&id) {
                    continue;
                }
                // un .desktop enorme (o que no es un archivo normal) no es una aplicación
                let Some(txt) = read_text(&p, TEXT_MAX) else { continue };
                let mut e = parse_desktop(&txt, &env.langs);
                e.id = id.clone();
                e.path = p;
                by_id.insert(id, apps.len());
                apps.push(e);
            }
        }
        let lists = env.mimeapps_files().iter().filter_map(|f| read_text(f, TEXT_MAX)).map(|t| parse_mimeapps(&t)).collect();
        let mut mime = MimeDb::default();
        for d in &env.data_dirs {
            let m = d.join("mime");
            if let Some(t) = read_text(&m.join("globs2"), MIME_MAX) {
                mime.add_globs2(&t);
            }
            if let Some(t) = read_text(&m.join("aliases"), MIME_MAX) {
                mime.add_aliases(&t);
            }
            if let Some(t) = read_text(&m.join("subclasses"), MIME_MAX) {
                mime.add_subclasses(&t);
            }
        }
        Catalog { env, apps, by_id, lists, mime }
    }

    fn get(&self, id: &str) -> Option<&DesktopEntry> {
        self.by_id.get(id).map(|&i| &self.apps[i]).filter(|e| e.usable(&self.env.desktops, &self.env.path))
    }

    /// Ids de las aplicaciones para `mime` y la predeterminada (la primera de la lista si la hay).
    pub fn resolve(&self, mime: &str) -> (Vec<String>, Option<String>) {
        let mut out: Vec<String> = Vec::new();
        let mut default: Option<String> = None;
        let push = |out: &mut Vec<String>, id: &str| {
            if !out.iter().any(|x| x == id) {
                out.push(id.to_string());
            }
        };
        for m in self.mime.with_parents(mime) {
            if default.is_none() {
                default = self.lists.iter().flat_map(|l| l.default.get(&m).into_iter().flatten()).find(|id| self.get(id).is_some()).cloned();
            }
            let mut removed: HashSet<&str> = HashSet::new();
            for l in &self.lists {
                for id in l.added.get(&m).into_iter().flatten() {
                    if !removed.contains(id.as_str()) && self.get(id).is_some() {
                        push(&mut out, id);
                    }
                }
                removed.extend(l.removed.get(&m).into_iter().flatten().map(|s| s.as_str()));
            }
            for e in &self.apps {
                if e.mime.iter().any(|x| self.mime.canonical(x) == m) && !removed.contains(e.id.as_str()) && self.get(&e.id).is_some() {
                    push(&mut out, &e.id);
                }
            }
        }
        if default.is_none() {
            default = out.first().cloned();
        }
        if let Some(d) = &default {
            out.retain(|x| x != d);
            out.insert(0, d.clone());
        }
        (out, default)
    }

    pub fn choices(&self, mime: &str) -> Choices {
        let (ids, default) = self.resolve(mime);
        let recommended: Vec<App> = ids
            .iter()
            .filter_map(|id| self.get(id))
            .map(|e| {
                let mut a = e.to_app();
                a.default = Some(&e.id) == default.as_ref();
                a
            })
            .collect();
        let mut others: Vec<App> = self
            .apps
            .iter()
            .filter(|e| !e.no_display && !ids.contains(&e.id) && e.usable(&self.env.desktops, &self.env.path))
            .map(|e| e.to_app())
            .collect();
        others.sort_by_cached_key(|a| a.name.to_lowercase());
        let mut type_name = String::new();
        if mime_ok(mime) {
            for d in &self.env.data_dirs {
                if let Some(x) = read_text(&d.join("mime").join(format!("{mime}.xml")), TEXT_MAX) {
                    if let Some(c) = mime_comment(&x, &self.env.langs) {
                        type_name = c;
                        break;
                    }
                }
            }
        }
        Choices { mime: mime.to_string(), type_name, recommended, others, can_default: true }
    }

    /// Tipo de `path`: por el nombre y, si no, por el contenido (texto o binario).
    pub fn mime_of(&self, path: &Path) -> String {
        if path.is_dir() {
            return "inode/directory".into();
        }
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        if let Some(m) = self.mime.by_name(&name) {
            return m;
        }
        let mut head = Vec::new();
        if let Some(f) = crate::thumbs::open_regular(path) {
            use std::io::Read;
            let _ = f.take(512).read_to_end(&mut head);
        }
        sniff(&head).to_string()
    }
}

/// Escribe `data` en `path` de forma atómica (archivo temporal en la misma carpeta y renombrado).
/// Si `path` es un enlace simbólico (configuración gestionada aparte), se escribe en su destino.
pub fn write_atomic(path: &Path, data: &str) -> Result<(), String> {
    let target = match std::fs::symlink_metadata(path) {
        Ok(m) if m.file_type().is_symlink() => std::fs::canonicalize(path).map_err(|e| e.to_string())?,
        _ => path.to_path_buf(),
    };
    let dir = target.parent().ok_or(crate::i18n::t("ruta sin carpeta"))?;
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let name = target.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let tmp = dir.join(format!(".{name}.fb-{}.tmp", std::process::id()));
    let res = (|| -> std::io::Result<()> {
        use std::io::Write;
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(data.as_bytes())?;
        f.sync_all()?;
        if let Ok(m) = std::fs::metadata(&target) {
            let _ = std::fs::set_permissions(&tmp, m.permissions());
        }
        std::fs::rename(&tmp, &target)
    })();
    if res.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    res.map_err(|e| e.to_string())
}

/// Fija la predeterminada en el mimeapps.list del usuario (y en su variante del escritorio si ya
/// fijaba ese tipo, porque tiene prioridad).
pub fn set_default_in(env: &XdgEnv, mime: &str, id: &str) -> Result<(), String> {
    if mime.is_empty() || id.is_empty() || mime.contains(['\n', '=', '[']) || id.contains(['\n', ';', '=']) {
        return Err(crate::i18n::t("tipo o aplicación no válidos").into());
    }
    let main = env.config_home.join("mimeapps.list");
    // si el archivo existe pero no se puede leer entero (permisos, no es UTF-8, no es un archivo
    // normal), no se toca: reescribirlo con sólo la nueva asociación perdería las demás
    let txt = match std::fs::metadata(&main) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e.to_string()),
        Ok(_) => read_text(&main, TEXT_MAX).ok_or_else(|| crate::i18n::tf("{}: no se puede leer", &[&main.display()]))?,
    };
    write_atomic(&main, &set_default_text(&txt, mime, id))?;
    for de in &env.desktops {
        let f = env.config_home.join(format!("{de}-mimeapps.list"));
        if let Some(t) = read_text(&f, TEXT_MAX) {
            if parse_mimeapps(&t).default.contains_key(mime) {
                write_atomic(&f, &set_default_text(&t, mime, id))?;
            }
        }
    }
    Ok(())
}

#[cfg(all(unix, not(any(target_os = "macos", target_os = "android"))))]
mod fd {
    use super::*;
    use std::sync::{Arc, Mutex};

    /// idioma, sello de las carpetas (`stamp`) y catálogo
    type Cached = (String, Vec<Option<std::time::SystemTime>>, Arc<Catalog>);
    static CATALOG: Mutex<Option<Cached>> = Mutex::new(None);

    fn ui_lang() -> String {
        crate::i18n::LANGS.get(crate::i18n::current()).map(|l| l.0).unwrap_or("en").to_string()
    }

    /// Catálogo de la sesión (se rehace si cambia el idioma, tras fijar una predeterminada o si
    /// se instaló o quitó una aplicación o cambió un mimeapps.list desde la última consulta).
    pub fn catalog() -> Arc<Catalog> {
        let lang = ui_lang();
        let env = XdgEnv::from_process(&lang);
        let st = stamp(&env);
        if let Some((l, s, c)) = crate::thumbs::lock(&CATALOG).as_ref() {
            if *l == lang && *s == st {
                return c.clone();
            }
        }
        let c = Arc::new(Catalog::build(env));
        *crate::thumbs::lock(&CATALOG) = Some((lang, st, c.clone()));
        c
    }

    pub fn choices(path: &Path) -> Choices {
        let c = catalog();
        let mime = c.mime_of(path);
        c.choices(&mime)
    }

    pub fn set_default(mime: &str, id: &str) -> Result<(), String> {
        let env = catalog().env.clone();
        let r = set_default_in(&env, mime, id);
        // las asociaciones han cambiado: se vuelven a leer en la próxima consulta
        *crate::thumbs::lock(&CATALOG) = None;
        r
    }

    /// Tema de iconos del escritorio (GTK o KDE), sin lanzar procesos.
    fn icon_theme(env: &XdgEnv) -> Option<String> {
        for f in ["gtk-4.0/settings.ini", "gtk-3.0/settings.ini"] {
            if let Ok(t) = std::fs::read_to_string(env.config_home.join(f)) {
                for l in t.lines() {
                    if let Some(v) = l.trim().strip_prefix("gtk-icon-theme-name") {
                        let v = v.trim_start_matches([' ', '=']).trim();
                        if !v.is_empty() {
                            return Some(v.to_string());
                        }
                    }
                }
            }
        }
        if let Ok(t) = std::fs::read_to_string(env.config_home.join("kdeglobals")) {
            let mut icons = false;
            for l in t.lines() {
                let l = l.trim();
                if l.starts_with('[') {
                    icons = l == "[Icons]";
                } else if icons {
                    if let Some(v) = l.strip_prefix("Theme=") {
                        return Some(v.trim().to_string());
                    }
                }
            }
        }
        None
    }

    static ICONS: Mutex<Option<HashMap<String, Option<PathBuf>>>> = Mutex::new(None);
    static THEMES: Mutex<Option<(Vec<PathBuf>, Vec<String>)>> = Mutex::new(None);

    /// Archivo del icono `name` (nombre del tema o ruta absoluta) en tamaño de aplicación.
    pub fn icon_path(name: &str) -> Option<PathBuf> {
        let name = name.trim();
        if name.is_empty() {
            return None;
        }
        if name.starts_with('/') {
            let p = PathBuf::from(name);
            return p.is_file().then_some(p);
        }
        if name.contains('/') || name.starts_with('.') {
            return None;
        }
        if let Some(r) = crate::thumbs::lock(&ICONS).as_ref().and_then(|m| m.get(name).cloned()) {
            return r;
        }
        let (bases, themes) = {
            let mut g = crate::thumbs::lock(&THEMES);
            if g.is_none() {
                let env = catalog().env.clone();
                let mut bases: Vec<PathBuf> = Vec::new();
                if let Some(h) = dirs::home_dir() {
                    bases.push(h.join(".icons"));
                }
                bases.extend(env.data_dirs.iter().map(|d| d.join("icons")));
                bases.retain(|b| b.is_dir());
                let mut themes: Vec<String> = Vec::new();
                if let Some(t) = icon_theme(&env).filter(|t| !t.contains('/') && !t.starts_with('.')) {
                    themes.push(t.clone());
                    // un nivel de herencia (Inherits=) basta para los iconos de aplicación
                    if let Some(txt) = bases.iter().find_map(|b| std::fs::read_to_string(b.join(&t).join("index.theme")).ok()) {
                        for l in txt.lines() {
                            if let Some(v) = l.trim().strip_prefix("Inherits=") {
                                themes.extend(v.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty() && !s.contains('/') && !s.starts_with('.')));
                            }
                        }
                    }
                }
                themes.push("hicolor".into());
                themes.dedup();
                *g = Some((bases, themes));
            }
            g.clone().unwrap_or_default()
        };
        const SUB: &[&str] = &[
            "48x48/apps", "64x64/apps", "scalable/apps", "96x96/apps", "128x128/apps", "256x256/apps", "32x32/apps", "512x512/apps",
            "apps/48", "apps/64", "apps/scalable", "apps/32", "48x48@2x/apps", "24x24/apps",
        ];
        let mut found = None;
        'search: for t in &themes {
            for b in &bases {
                let root = b.join(t);
                if !root.is_dir() {
                    continue;
                }
                for s in SUB {
                    for ext in ["png", "svg"] {
                        let p = root.join(s).join(format!("{name}.{ext}"));
                        if p.is_file() {
                            found = Some(p);
                            break 'search;
                        }
                    }
                }
            }
        }
        if found.is_none() {
            for ext in ["png", "svg", "xpm"] {
                let p = PathBuf::from("/usr/share/pixmaps").join(format!("{name}.{ext}"));
                if ext != "xpm" && p.is_file() {
                    found = Some(p);
                    break;
                }
            }
        }
        crate::thumbs::lock(&ICONS).get_or_insert_with(HashMap::new).insert(name.to_string(), found.clone());
        found
    }
}

// ---------------------------------------------------------------------------
// Windows
// ---------------------------------------------------------------------------

#[cfg(windows)]
mod win {
    use super::*;
    use windows::core::{PCWSTR, PWSTR};
    use windows::Win32::System::Com::{CoInitializeEx, CoTaskMemFree, CoUninitialize, IDataObject, COINIT_APARTMENTTHREADED};
    use windows::Win32::UI::Shell::{
        AssocQueryStringW, IAssocHandler, IShellItem, SHAssocEnumHandlers, SHCreateItemFromParsingName, SHOpenWithDialog, ASSOCF_INIT_IGNOREUNKNOWN,
        ASSOCSTR, ASSOCSTR_APPID, ASSOCSTR_EXECUTABLE, ASSOCSTR_FRIENDLYDOCNAME, ASSOC_FILTER, ASSOC_FILTER_NONE, ASSOC_FILTER_RECOMMENDED,
        BHID_DataObject, OAIF_ALLOW_REGISTRATION, OAIF_EXEC, OAIF_FORCE_REGISTRATION, OAIF_REGISTER_EXT, OPENASINFO,
    };

    fn wide(s: &std::ffi::OsStr) -> Vec<u16> {
        use std::os::windows::ffi::OsStrExt;
        s.encode_wide().chain(std::iter::once(0)).collect()
    }

    /// COM en este hilo mientras viva el valor.
    struct Com(bool);
    impl Com {
        fn init() -> Com {
            // SAFETY: inicialización de COM en el hilo actual; se deshace en Drop si tuvo éxito.
            Com(unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }.is_ok())
        }
    }
    impl Drop for Com {
        fn drop(&mut self) {
            if self.0 {
                // SAFETY: empareja el CoInitializeEx que tuvo éxito en este mismo hilo.
                unsafe { CoUninitialize() };
            }
        }
    }

    /// Cadena devuelta por el Shell (se libera con CoTaskMemFree).
    fn take(p: PWSTR) -> String {
        if p.is_null() {
            return String::new();
        }
        // SAFETY: `p` es una cadena terminada en NUL reservada por el Shell con CoTaskMemFree.
        unsafe {
            let s = p.to_string().unwrap_or_default();
            CoTaskMemFree(Some(p.0 as *const _));
            s
        }
    }

    fn assoc_str(what: ASSOCSTR, ext: &str) -> Option<String> {
        let e = wide(std::ffi::OsStr::new(ext));
        let mut buf = vec![0u16; 1024];
        let mut len = buf.len() as u32;
        // SAFETY: `e` termina en NUL; `buf` tiene `len` caracteres.
        let hr = unsafe { AssocQueryStringW(ASSOCF_INIT_IGNOREUNKNOWN, what, PCWSTR(e.as_ptr()), PCWSTR::null(), Some(PWSTR(buf.as_mut_ptr())), &mut len) };
        if hr.is_err() {
            return None;
        }
        let n = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        let s = String::from_utf16_lossy(&buf[..n]);
        (!s.is_empty()).then_some(s)
    }

    fn ext_of(path: &Path) -> Option<String> {
        let e = path.extension()?.to_string_lossy().to_lowercase();
        (!e.is_empty() && !e.contains(['"', '\\', ' ', '/'])).then(|| format!(".{e}"))
    }

    fn handlers(ext: &str, filter: ASSOC_FILTER) -> Vec<IAssocHandler> {
        let e = wide(std::ffi::OsStr::new(ext));
        let mut out = Vec::new();
        // SAFETY: `e` termina en NUL; el enumerador entrega referencias propias en `h`.
        unsafe {
            let Ok(en) = SHAssocEnumHandlers(PCWSTR(e.as_ptr()), filter) else { return out };
            for _ in 0..256 {
                let mut h: [Option<IAssocHandler>; 1] = [None];
                let mut n = 0u32;
                if en.Next(&mut h, Some(&mut n)).is_err() || n == 0 {
                    break;
                }
                if let Some(h) = h[0].take() {
                    out.push(h);
                }
            }
        }
        out
    }

    /// Cómo identifica Windows la aplicación predeterminada de `ext`: el ejecutable (programas de
    /// escritorio) y el AppUserModelID (aplicaciones de la Tienda/UWP, que no tienen un .exe propio
    /// con el que comparar). Vacíos si no hay predeterminada.
    struct DefaultIds {
        exe: String,
        app_id: String,
    }

    impl DefaultIds {
        fn of(ext: &str) -> DefaultIds {
            DefaultIds { exe: assoc_str(ASSOCSTR_EXECUTABLE, ext).unwrap_or_default(), app_id: assoc_str(ASSOCSTR_APPID, ext).unwrap_or_default() }
        }

        /// ¿Es `id` (el nombre del manejador del Shell) la predeterminada?
        fn matches(&self, id: &str) -> bool {
            let id = id.trim();
            !id.is_empty() && [&self.exe, &self.app_id].iter().any(|d| !d.is_empty() && d.trim().eq_ignore_ascii_case(id))
        }
    }

    fn to_app(h: &IAssocHandler, def: &DefaultIds) -> Option<App> {
        // SAFETY: llamadas a un IAssocHandler válido; las cadenas se liberan en `take`.
        let (id, name) = unsafe { (take(h.GetName().ok()?), take(h.GetUIName().ok()?)) };
        if id.is_empty() {
            return None;
        }
        let name = if name.is_empty() { Path::new(&id).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| id.clone()) } else { name };
        Some(App { name, default: def.matches(&id), id, ..Default::default() })
    }

    /// ¿Es el manejador `id` la aplicación predeterminada de `ext` ahora mismo?
    pub fn is_default(ext: &str, id: &str) -> bool {
        DefaultIds::of(ext).matches(id)
    }

    pub fn choices(path: &Path) -> Choices {
        let Some(ext) = ext_of(path) else { return Choices::default() };
        let _com = Com::init();
        let def = DefaultIds::of(&ext);
        let mut rec: Vec<App> = handlers(&ext, ASSOC_FILTER_RECOMMENDED).iter().filter_map(|h| to_app(h, &def)).collect();
        let mut others: Vec<App> = handlers(&ext, ASSOC_FILTER_NONE)
            .iter()
            .filter_map(|h| to_app(h, &def))
            .filter(|a| !rec.iter().any(|r| r.id.eq_ignore_ascii_case(&a.id)))
            .collect();
        if rec.is_empty() && others.is_empty() {
            // sin manejadores del Shell: lo que diga el registro
            rec = crate::extras::windows_apps(path);
        }
        // el Shell puede dar el mismo manejador más de una vez
        let mut seen = std::collections::HashSet::new();
        rec.retain(|a| seen.insert(a.id.to_lowercase()));
        others.retain(|a| seen.insert(a.id.to_lowercase()));
        if let Some(i) = rec.iter().position(|a| a.default) {
            let d = rec.remove(i);
            rec.insert(0, d);
        }
        others.sort_by_cached_key(|a| a.name.to_lowercase());
        let type_name = assoc_str(ASSOCSTR_FRIENDLYDOCNAME, &ext).unwrap_or_default();
        Choices { mime: ext, type_name, recommended: rec, others, can_default: true }
    }

    fn find(ext: &str, id: &str) -> Option<IAssocHandler> {
        handlers(ext, ASSOC_FILTER_NONE).into_iter().find(|h| {
            // SAFETY: IAssocHandler válido; la cadena se libera en `take`.
            unsafe { h.GetName().ok().map(take) }.is_some_and(|n| n.eq_ignore_ascii_case(id))
        })
    }

    /// Abre `path` con el manejador `id` (bloquea un momento: llamar desde un hilo).
    pub fn invoke(id: &str, path: &Path) -> Result<(), String> {
        let ext = ext_of(path).ok_or(crate::i18n::t("archivo sin extensión"))?;
        let _com = Com::init();
        let h = find(&ext, id).ok_or(crate::i18n::t("aplicación no encontrada"))?;
        let w = wide(path.as_os_str());
        // SAFETY: `w` termina en NUL; las interfaces se sueltan al salir de ámbito.
        unsafe {
            let item: IShellItem = SHCreateItemFromParsingName(PCWSTR(w.as_ptr()), None).map_err(|e| e.message())?;
            let data: IDataObject = item.BindToHandler(None, &BHID_DataObject).map_err(|e| e.message())?;
            h.Invoke(&data).map_err(|e| e.message())
        }
    }

    /// Intenta hacer predeterminado el manejador `id` para la extensión `ext`. Desde Windows 8 la
    /// elección del usuario (UserChoice) está protegida y `MakeDefault` puede no tener efecto (o
    /// fallar, como con las aplicaciones de la Tienda), así que el resultado se comprueba leyendo
    /// otra vez la asociación: si no cambió, hace falta la ventana del sistema.
    pub fn make_default(ext: &str, id: &str) -> Result<Defaulted, String> {
        let _com = Com::init();
        if is_default(ext, id) {
            return Ok(Defaulted::Done);
        }
        let h = find(ext, id).ok_or(crate::i18n::t("aplicación no encontrada"))?;
        let d = wide(std::ffi::OsStr::new(ext));
        // SAFETY: IAssocHandler válido; `d` termina en NUL.
        let r = unsafe { h.MakeDefault(PCWSTR(d.as_ptr())) };
        Ok(if r.is_ok() && is_default(ext, id) { Defaulted::Done } else { Defaulted::NeedsSystemUi })
    }

    fn sh_open_with(path: &Path, flags: windows::Win32::UI::Shell::OPEN_AS_INFO_FLAGS) -> windows::core::Result<()> {
        let _com = Com::init();
        let w = wide(path.as_os_str());
        let info = OPENASINFO { pcszFile: PCWSTR(w.as_ptr()), pcszClass: PCWSTR::null(), oaifInFlags: flags };
        // SAFETY: `info` y `w` viven durante la llamada.
        unsafe { SHOpenWithDialog(None, &info) }
    }

    /// Selector «Abrir con» nativo de Windows (modal: bloquea hasta que se cierra).
    pub fn open_with_dialog(path: &Path) -> Result<(), String> {
        sh_open_with(path, OAIF_EXEC | OAIF_ALLOW_REGISTRATION).map_err(|e| e.message())
    }

    /// Selector «Abrir con» de Windows con «Usar siempre esta aplicación» marcado: lo que elija el
    /// usuario queda como predeterminada (lo guarda el propio Windows) y abre el archivo. Modal.
    /// `Ok(false)` si el usuario lo cerró sin elegir.
    pub fn choose_default_dialog(path: &Path) -> Result<bool, String> {
        // HRESULT_FROM_WIN32(ERROR_CANCELLED): el usuario cerró la ventana
        const CANCELLED: i32 = 0x8007_04C7_u32 as i32;
        match sh_open_with(path, OAIF_EXEC | OAIF_ALLOW_REGISTRATION | OAIF_REGISTER_EXT | OAIF_FORCE_REGISTRATION) {
            Ok(()) => Ok(true),
            Err(e) if e.code().0 == CANCELLED => Ok(false),
            Err(e) => Err(e.message()),
        }
    }
}

// ---------------------------------------------------------------------------
// macOS (no se puede compilar aquí: código mínimo y aislado)
// ---------------------------------------------------------------------------

#[cfg(target_os = "macos")]
mod mac {
    use super::*;
    use std::ffi::c_void;

    type CFRef = *const c_void;
    const LS_ROLES_ALL: u32 = 0xFFFF_FFFF;

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFURLCreateFromFileSystemRepresentation(alloc: CFRef, buf: *const u8, len: isize, is_dir: u8) -> CFRef;
        fn CFURLGetFileSystemRepresentation(url: CFRef, resolve: u8, buf: *mut u8, max: isize) -> u8;
        fn CFArrayGetCount(a: CFRef) -> isize;
        fn CFArrayGetValueAtIndex(a: CFRef, i: isize) -> CFRef;
        fn CFRelease(cf: CFRef);
    }
    #[link(name = "CoreServices", kind = "framework")]
    unsafe extern "C" {
        fn LSCopyApplicationURLsForURL(url: CFRef, roles: u32) -> CFRef;
        fn LSCopyDefaultApplicationURLForURL(url: CFRef, roles: u32, err: *mut CFRef) -> CFRef;
    }

    fn url_path(u: CFRef) -> Option<PathBuf> {
        use std::os::unix::ffi::OsStrExt;
        let mut buf = vec![0u8; 4096];
        // SAFETY: `u` es un CFURL válido; `buf` tiene la longitud indicada.
        let ok = unsafe { CFURLGetFileSystemRepresentation(u, 1, buf.as_mut_ptr(), buf.len() as isize) };
        if ok == 0 {
            return None;
        }
        let n = buf.iter().position(|&c| c == 0)?;
        Some(PathBuf::from(std::ffi::OsStr::from_bytes(&buf[..n])))
    }

    fn app_of(p: &Path) -> App {
        let name = p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        App { exec: name.clone(), name, id: p.to_string_lossy().into_owned(), ..Default::default() }
    }

    pub fn choices(path: &Path) -> Choices {
        use std::os::unix::ffi::OsStrExt;
        let bytes = path.as_os_str().as_bytes();
        let mut rec: Vec<App> = Vec::new();
        let mut def: Option<PathBuf> = None;
        // SAFETY: los objetos Core Foundation creados o copiados aquí se liberan con CFRelease.
        unsafe {
            let url = CFURLCreateFromFileSystemRepresentation(std::ptr::null(), bytes.as_ptr(), bytes.len() as isize, 0);
            if !url.is_null() {
                let d = LSCopyDefaultApplicationURLForURL(url, LS_ROLES_ALL, std::ptr::null_mut());
                if !d.is_null() {
                    def = url_path(d);
                    CFRelease(d);
                }
                let arr = LSCopyApplicationURLsForURL(url, LS_ROLES_ALL);
                if !arr.is_null() {
                    let n = CFArrayGetCount(arr);
                    for i in 0..n.min(200) {
                        if let Some(p) = url_path(CFArrayGetValueAtIndex(arr, i)) {
                            if !rec.iter().any(|a| a.id == p.to_string_lossy()) {
                                rec.push(app_of(&p));
                            }
                        }
                    }
                    CFRelease(arr);
                }
                CFRelease(url);
            }
        }
        if let Some(d) = def {
            let id = d.to_string_lossy().into_owned();
            rec.retain(|a| a.id != id);
            let mut a = app_of(&d);
            a.default = true;
            rec.insert(0, a);
        }
        let mut others = Vec::new();
        let mut dirs_ = vec![PathBuf::from("/Applications"), PathBuf::from("/Applications/Utilities"), PathBuf::from("/System/Applications"), PathBuf::from("/System/Applications/Utilities")];
        if let Some(h) = dirs::home_dir() {
            dirs_.push(h.join("Applications"));
        }
        for d in dirs_ {
            if let Ok(rd) = std::fs::read_dir(&d) {
                for e in rd.flatten() {
                    let p = e.path();
                    if p.extension().is_some_and(|x| x == "app") {
                        let id = p.to_string_lossy().into_owned();
                        if !rec.iter().any(|a| a.id == id) && !others.iter().any(|a: &App| a.id == id) {
                            others.push(app_of(&p));
                        }
                    }
                }
            }
        }
        others.sort_by_cached_key(|a| a.name.to_lowercase());
        Choices { mime: String::new(), type_name: String::new(), recommended: rec, others, can_default: false }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let n = web_time::SystemTime::now().duration_since(web_time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
        let d = std::env::temp_dir().join(format!("fbw2-apps-{tag}-{}-{n:x}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn desktop_localizado_y_campos() {
        let txt = "# comentario\n[Desktop Entry]\nType=Application\nName=Text Editor\nName[es]=Editor de textos\nName[es_ES]=Editor (España)\nGenericName=Editor\nGenericName[fr]=Éditeur\nComment=Edit\\stext\nExec=gedit %U\nIcon=org.gnome.gedit\nTerminal=false\nMimeType=text/plain;text/x-c\\;weird;\nOnlyShowIn=GNOME;Unity;\n\n[Desktop Action new]\nName=New Window\nExec=gedit --new-window\n";
        let e = parse_desktop(txt, &locale_keys("es", "es_ES.UTF-8"));
        assert_eq!(e.name, "Editor (España)");
        assert_eq!(parse_desktop(txt, &locale_keys("es", "en_US.UTF-8")).name, "Editor de textos");
        assert_eq!(parse_desktop(txt, &locale_keys("de", "de_DE.UTF-8")).name, "Text Editor");
        assert_eq!(parse_desktop(txt, &locale_keys("fr", "")).generic, "Éditeur");
        assert_eq!(e.comment, "Edit text");
        assert_eq!(e.exec, "gedit %U", "la acción [Desktop Action] no cuenta");
        assert_eq!(e.mime, ["text/plain", "text/x-c;weird"]);
        assert!(e.application && !e.terminal);
        assert!(e.usable(&["gnome".into()], &[]));
        assert!(!e.usable(&["kde".into()], &[]), "OnlyShowIn");
        let mut h = e.clone();
        h.hidden = true;
        assert!(!h.usable(&["gnome".into()], &[]));
        let n = parse_desktop("[Desktop Entry]\nType=Application\nName=X\nExec=x\nNotShowIn=KDE;\nTryExec=no-existe-fb\n", &[]);
        assert!(!n.usable(&[], &[]), "TryExec sin ejecutable");
        let link = parse_desktop("[Desktop Entry]\nType=Link\nName=X\nURL=http://x\n", &[]);
        assert!(!link.usable(&[], &[]));
        assert_eq!(locale_keys("zh", "zh_TW.UTF-8@x"), ["zh_TW@x", "zh_TW", "zh@x", "zh_CN", "zh"]);
    }

    #[test]
    fn mimeapps_lectura_y_escritura() {
        let orig = "# mío\n[Default Applications]\ntext/plain=gedit.desktop;\nimage/png=eog.desktop;\n\n[Added Associations]\ntext/plain=kate.desktop;org.x.desktop;\n\n[Removed Associations]\ntext/plain=vim.desktop;org.x.desktop;\n[Otro]\nclave=valor\n";
        let m = parse_mimeapps(orig);
        assert_eq!(m.default["text/plain"], ["gedit.desktop"]);
        assert_eq!(m.added["text/plain"], ["kate.desktop", "org.x.desktop"]);
        let out = set_default_text(orig, "text/plain", "org.x.desktop");
        let m2 = parse_mimeapps(&out);
        assert_eq!(m2.default["text/plain"], ["org.x.desktop"]);
        assert_eq!(m2.default["image/png"], ["eog.desktop"]);
        assert_eq!(m2.added["text/plain"], ["org.x.desktop", "kate.desktop"]);
        assert_eq!(m2.removed["text/plain"], ["vim.desktop"]);
        assert!(out.starts_with("# mío\n[Default Applications]\n"));
        assert!(out.contains("[Otro]\nclave=valor\n"));
        // un tipo nuevo: va al final de su grupo, antes de la línea en blanco
        let out = set_default_text(orig, "video/mp4", "mpv.desktop");
        assert!(out.contains("image/png=eog.desktop;\nvideo/mp4=mpv.desktop;\n\n[Added Associations]"), "{out}");
        assert!(out.contains("text/plain=kate.desktop;org.x.desktop;\nvideo/mp4=mpv.desktop;\n"), "{out}");
        // archivo vacío o sin grupos
        let out = set_default_text("", "a/b", "x.desktop");
        assert_eq!(out, "[Default Applications]\na/b=x.desktop;\n\n[Added Associations]\na/b=x.desktop;\n");
        // idempotente
        assert_eq!(set_default_text(&out, "a/b", "x.desktop"), out);
        // escritura atómica en un XDG_CONFIG_HOME temporal, con la variante del escritorio
        let d = tmp("mimeapps");
        let env = XdgEnv { config_home: d.clone(), desktops: vec!["gnome".into()], ..Default::default() };
        std::fs::write(d.join("gnome-mimeapps.list"), "[Default Applications]\ntext/plain=old.desktop;\n").unwrap();
        std::fs::write(d.join("mimeapps.list"), orig).unwrap();
        set_default_in(&env, "text/plain", "new.desktop").unwrap();
        assert_eq!(parse_mimeapps(&std::fs::read_to_string(d.join("mimeapps.list")).unwrap()).default["text/plain"], ["new.desktop"]);
        assert_eq!(parse_mimeapps(&std::fs::read_to_string(d.join("gnome-mimeapps.list")).unwrap()).default["text/plain"], ["new.desktop"]);
        assert!(set_default_in(&env, "text/plain", "a\n[x]").is_err());
        let left: Vec<_> = std::fs::read_dir(&d).unwrap().flatten().map(|e| e.file_name()).collect();
        assert_eq!(left.len(), 2, "sin temporales: {left:?}");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn base_mime() {
        let mut db = MimeDb::default();
        db.add_globs2("# x\n50:text/plain:*.txt\n50:text/x-csrc:*.c\n50:text/x-c++src:*.C:cs\n55:application/x-compressed-tar:*.tar.gz\n50:application/gzip:*.gz\n50:text/x-makefile:makefile\n50:text/x-readme:README*\n10:text/x-log:*.lo[gt]\n");
        db.add_aliases("text/x-c text/x-csrc\n");
        db.add_subclasses("text/x-csrc text/plain\napplication/x-compressed-tar application/gzip\n");
        assert_eq!(db.by_name("a.TXT").as_deref(), Some("text/plain"));
        assert_eq!(db.by_name("x.tar.gz").as_deref(), Some("application/x-compressed-tar"));
        assert_eq!(db.by_name("x.gz").as_deref(), Some("application/gzip"));
        assert_eq!(db.by_name("Makefile").as_deref(), Some("text/x-makefile"));
        assert_eq!(db.by_name("README.md").as_deref(), Some("text/x-readme"));
        assert_eq!(db.by_name("a.C").as_deref(), Some("text/x-c++src"));
        assert_eq!(db.by_name("a.c").as_deref(), Some("text/x-csrc"));
        assert_eq!(db.by_name("x.lot").as_deref(), Some("text/x-log"));
        assert_eq!(db.by_name("x.lox"), None);
        assert_eq!(db.with_parents("text/x-c"), ["text/x-csrc", "text/plain"]);
        assert_eq!(db.with_parents("text/x-log"), ["text/x-log", "text/plain"]);
        assert_eq!(sniff(b""), "application/x-zerosize");
        assert_eq!(sniff("hola ñ".as_bytes()), "text/plain");
        assert_eq!(sniff(b"\x7fELF\0\0"), "application/octet-stream");
        assert!(mime_ok("text/plain") && !mime_ok("../x") && !mime_ok("a/../../b") && !mime_ok("a/.b"));
        let xml = r#"<mime-type type="text/plain"><comment>plain text document</comment><comment xml:lang="es">documento de texto &amp; más</comment></mime-type>"#;
        assert_eq!(mime_comment(xml, &["es".into()]).as_deref(), Some("documento de texto & más"));
        assert_eq!(mime_comment(xml, &["de".into()]).as_deref(), Some("plain text document"));
    }

    #[test]
    fn catalogo_en_carpetas_temporales() {
        let d = tmp("cat");
        let (home, sys, cfg) = (d.join("home"), d.join("sys"), d.join("cfg"));
        let w = |p: PathBuf, t: &str| {
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, t).unwrap();
        };
        let app = |name: &str, extra: &str| format!("[Desktop Entry]\nType=Application\nName={name}\nExec={name} %f\n{extra}");
        w(sys.join("applications/gedit.desktop"), &app("gedit", "MimeType=text/plain;\n"));
        w(sys.join("applications/kate.desktop"), &app("kate", "MimeType=text/x-csrc;\n"));
        w(sys.join("applications/vim.desktop"), &app("vim", "MimeType=text/plain;\nTerminal=true\n"));
        w(sys.join("applications/hidden.desktop"), &app("hidden", "MimeType=text/plain;\n"));
        w(sys.join("applications/nodisp.desktop"), &app("nodisp", "MimeType=text/plain;\nNoDisplay=true\n"));
        w(sys.join("applications/zz.desktop"), &app("Zeta", ""));
        w(sys.join("applications/kde/okular.desktop"), &app("Okular", ""));
        // el usuario borra hidden.desktop con Hidden=true (tiene prioridad)
        w(home.join("applications/hidden.desktop"), "[Desktop Entry]\nType=Application\nName=hidden\nExec=x\nHidden=true\n");
        w(sys.join("mime/globs2"), "50:text/plain:*.txt\n50:text/x-csrc:*.c\n");
        w(sys.join("mime/subclasses"), "text/x-csrc text/plain\n");
        w(cfg.join("mimeapps.list"), "[Default Applications]\ntext/plain=missing.desktop;vim.desktop;\n[Removed Associations]\ntext/plain=gedit.desktop;\n");
        let env = XdgEnv { data_dirs: vec![home.clone(), sys.clone()], config_home: cfg.clone(), langs: vec!["es".into()], ..Default::default() };
        let c = Catalog::build(env);
        assert_eq!(c.mime_of(Path::new("/x/a.c")), "text/x-csrc");
        let ch = c.choices("text/plain");
        let ids: Vec<&str> = ch.recommended.iter().map(|a| a.id.as_str()).collect();
        // predeterminada: la primera instalada de la lista; gedit quitada; hidden borrada; NoDisplay sí vale
        assert_eq!(ids, ["vim.desktop", "nodisp.desktop"]);
        assert!(ch.recommended[0].default && ch.recommended[0].terminal);
        let others: Vec<&str> = ch.others.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(others, ["gedit", "kate", "Okular", "Zeta"], "alfabético, sin NoDisplay ni recomendadas");
        assert!(ch.others.iter().any(|a| a.id == "kde-okular.desktop"));
        // un subtipo hereda las de su padre después de las suyas
        let ids: Vec<String> = c.choices("text/x-csrc").recommended.into_iter().map(|a| a.id).collect();
        assert_eq!(ids[0], "vim.desktop", "predeterminada heredada de text/plain");
        assert!(ids.contains(&"kate.desktop".to_string()));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn mimeapps_ilegible_no_se_pisa() {
        let d = tmp("mimebad");
        let env = XdgEnv { config_home: d.clone(), ..Default::default() };
        // un mimeapps.list con un byte que no es UTF-8 (p. ej. un comentario en Latin-1)
        let orig = b"# caf\xe9\n[Default Applications]\nimage/png=eog.desktop;\nvideo/mp4=mpv.desktop;\n".to_vec();
        std::fs::write(d.join("mimeapps.list"), &orig).unwrap();
        assert!(set_default_in(&env, "text/plain", "gedit.desktop").is_err());
        assert_eq!(std::fs::read(d.join("mimeapps.list")).unwrap(), orig, "las demás asociaciones se conservan");
        // si no existe, se crea
        let d2 = tmp("mimenew");
        set_default_in(&XdgEnv { config_home: d2.clone(), ..Default::default() }, "a/b", "x.desktop").unwrap();
        assert_eq!(parse_mimeapps(&std::fs::read_to_string(d2.join("mimeapps.list")).unwrap()).default["a/b"], ["x.desktop"]);
        let _ = std::fs::remove_dir_all(&d);
        let _ = std::fs::remove_dir_all(&d2);
    }

    #[cfg(all(unix, not(target_arch = "wasm32")))]
    #[test]
    fn catalogo_no_se_bloquea_con_un_fifo() {
        let d = tmp("fifo");
        let apps = d.join("applications");
        std::fs::create_dir_all(&apps).unwrap();
        std::fs::write(apps.join("ok.desktop"), "[Desktop Entry]\nType=Application\nName=ok\nExec=ok\n").unwrap();
        let f = std::ffi::CString::new(apps.join("trampa.desktop").to_str().unwrap()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(f.as_ptr(), 0o600) }, 0);
        let env = XdgEnv { data_dirs: vec![d.clone()], config_home: d.join("cfg"), ..Default::default() };
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(Catalog::build(env).apps.iter().map(|a| a.id.clone()).collect::<Vec<_>>());
        });
        let ids = rx.recv_timeout(std::time::Duration::from_secs(10)).expect("Catalog::build se quedó bloqueado en el FIFO");
        assert_eq!(ids, ["ok.desktop"]);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn sello_cambia_al_instalar_una_aplicacion() {
        let d = tmp("stamp");
        std::fs::create_dir_all(d.join("applications")).unwrap();
        let env = XdgEnv { data_dirs: vec![d.clone()], config_home: d.join("cfg"), ..Default::default() };
        let a = stamp(&env);
        assert_eq!(a, stamp(&env));
        // fecha anterior para que el cambio se note aunque el reloj del sistema de archivos sea grueso
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(60);
        std::fs::File::open(d.join("applications")).unwrap().set_modified(old).unwrap();
        let b = stamp(&env);
        std::fs::write(d.join("applications/nueva.desktop"), "[Desktop Entry]\n").unwrap();
        assert_ne!(b, stamp(&env), "una aplicación nueva cambia el sello");
        let c = stamp(&env);
        std::fs::create_dir_all(d.join("cfg")).unwrap();
        std::fs::write(d.join("cfg/mimeapps.list"), "").unwrap();
        assert_ne!(c, stamp(&env), "un mimeapps.list nuevo cambia el sello");
        let _ = std::fs::remove_dir_all(&d);
    }
}
