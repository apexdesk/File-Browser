//! Traducciones: el texto en español es la clave; `t()` devuelve la cadena del idioma activo.
use crate::i18n_table::TABLE;
use std::collections::HashMap;
use std::fmt::Display;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::OnceLock;

/// (código, nombre nativo). El orden coincide con las columnas de `TABLE`.
pub const LANGS: [(&str, &str); 10] = [
    ("es", "Español"),
    ("en", "English"),
    ("fr", "Français"),
    ("de", "Deutsch"),
    ("pt", "Português"),
    ("it", "Italiano"),
    ("ru", "Русский"),
    ("zh", "中文 (简体)"),
    ("ja", "日本語"),
    ("ko", "한국어"),
];

static CUR: AtomicUsize = AtomicUsize::new(0);

pub fn set(i: usize) {
    CUR.store(i.min(LANGS.len() - 1), Ordering::Relaxed);
}

pub fn current() -> usize {
    CUR.load(Ordering::Relaxed)
}

pub fn index_of(code: &str) -> Option<usize> {
    LANGS.iter().position(|(c, _)| *c == code)
}

/// Resuelve el valor guardado en la configuración ("auto" o un código).
pub fn resolve(setting: &str) -> usize {
    if setting == "auto" || setting.is_empty() { detect() } else { index_of(setting).unwrap_or_else(detect) }
}

fn from_tag(tag: &str) -> Option<usize> {
    let t = tag.trim().trim_matches('"').to_ascii_lowercase();
    let t = t.replace('_', "-");
    let base = t.split(['.', '@', ':']).next().unwrap_or("");
    let lang = base.split('-').next().unwrap_or("");
    if lang == "c" || lang == "posix" || lang.is_empty() {
        return None;
    }
    index_of(lang)
}

/// Idioma del sistema (por defecto inglés si no es uno de los soportados).
pub fn detect() -> usize {
    let en = 1;
    #[cfg(windows)]
    {
        unsafe extern "system" {
            fn GetUserDefaultLocaleName(buf: *mut u16, len: i32) -> i32;
        }
        let mut b = [0u16; 85];
        let n = unsafe { GetUserDefaultLocaleName(b.as_mut_ptr(), b.len() as i32) };
        if n > 1 {
            let s = String::from_utf16_lossy(&b[..(n as usize - 1)]);
            return from_tag(&s).unwrap_or(en);
        }
        return en;
    }
    #[cfg(target_os = "macos")]
    {
        if let Ok(o) = std::process::Command::new("defaults").args(["read", "-g", "AppleLanguages"]).output() {
            let s = String::from_utf8_lossy(&o.stdout).to_string();
            if let Some(tag) = s.split('"').nth(1) {
                if let Some(i) = from_tag(tag) {
                    return i;
                }
            }
        }
    }
    #[cfg(not(windows))]
    {
        for var in ["LC_ALL", "LC_MESSAGES", "LANGUAGE", "LANG"] {
            if let Ok(v) = std::env::var(var) {
                for part in v.split(':') {
                    if let Some(i) = from_tag(part) {
                        return i;
                    }
                }
            }
        }
        en
    }
    #[cfg(windows)]
    {
        en
    }
}

fn map() -> &'static HashMap<&'static str, usize> {
    static M: OnceLock<HashMap<&'static str, usize>> = OnceLock::new();
    M.get_or_init(|| TABLE.iter().enumerate().map(|(i, r)| (r[0], i)).collect())
}

/// Traduce una cadena literal (clave en español).
#[inline]
pub fn t(s: &'static str) -> &'static str {
    let l = current();
    if l == 0 {
        return s;
    }
    match map().get(s) {
        Some(&row) => {
            let v = TABLE[row][l];
            if v.is_empty() { s } else { v }
        }
        None => s,
    }
}

/// Traduce una plantilla y sustituye sus `{...}` en orden por los argumentos.
pub fn tf(key: &'static str, args: &[&dyn Display]) -> String {
    let tpl = t(key);
    let mut out = String::with_capacity(tpl.len() + 16);
    let mut it = args.iter();
    let mut rest = tpl;
    while let Some(i) = rest.find('{') {
        let Some(j) = rest[i..].find('}') else { break };
        out.push_str(&rest[..i]);
        if let Some(a) = it.next() {
            out.push_str(&a.to_string());
        }
        rest = &rest[i + j + 1..];
    }
    out.push_str(rest);
    out
}

// ---------------------------------------------------------------------------
// Fuentes para CJK (egui sólo trae latín/cirílico/griego)
// ---------------------------------------------------------------------------

fn cjk_font(lang: &str) -> Option<(Vec<u8>, u32)> {
    let mut cands: Vec<(String, u32)> = Vec::new();
    let (noto_idx, win, mac): (u32, &[&str], &[&str]) = match lang {
        "ja" => (
            0,
            &["C:\\Windows\\Fonts\\YuGothR.ttc", "C:\\Windows\\Fonts\\meiryo.ttc", "C:\\Windows\\Fonts\\msgothic.ttc"],
            &["/System/Library/Fonts/ヒラギノ角ゴシック W3.ttc", "/System/Library/Fonts/Hiragino Sans GB.ttc"],
        ),
        "ko" => (
            1,
            &["C:\\Windows\\Fonts\\malgun.ttf"],
            &["/System/Library/Fonts/AppleSDGothicNeo.ttc", "/System/Library/Fonts/Supplemental/AppleGothic.ttf"],
        ),
        _ => (
            2,
            &["C:\\Windows\\Fonts\\msyh.ttc", "C:\\Windows\\Fonts\\simsun.ttc"],
            &["/System/Library/Fonts/PingFang.ttc", "/System/Library/Fonts/STHeiti Light.ttc"],
        ),
    };
    for p in win.iter().chain(mac.iter()) {
        cands.push((p.to_string(), 0));
    }
    for p in [
        "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
        "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc",
        "/usr/share/fonts/truetype/noto/NotoSansCJK-Regular.ttc",
        "/usr/share/fonts/google-noto-cjk/NotoSansCJK-Regular.ttc",
        "/usr/share/fonts/noto/NotoSansCJK-Regular.ttc",
    ] {
        cands.push((p.to_string(), noto_idx));
    }
    #[cfg(unix)]
    if let Ok(o) = std::process::Command::new("fc-match")
        .args(["-f", "%{file}\n%{index}", &format!(":lang={lang}")])
        .output()
    {
        let s = String::from_utf8_lossy(&o.stdout).to_string();
        let mut l = s.lines();
        if let (Some(f), idx) = (l.next(), l.next().and_then(|x| x.trim().parse::<u32>().ok()).unwrap_or(0)) {
            if !f.is_empty() {
                cands.push((f.to_string(), idx));
            }
        }
    }
    for (p, idx) in cands {
        if let Ok(b) = std::fs::read(&p) {
            return Some((b, idx));
        }
    }
    None
}

/// Instala la fuente de respaldo CJK si el idioma la necesita. Devuelve `false` si no se encontró.
pub fn install_fonts(ctx: &egui::Context, lang_idx: usize) -> bool {
    let code = LANGS[lang_idx.min(LANGS.len() - 1)].0;
    // sin CJK, las fuentes por defecto de egui ya están puestas: no se reconstruyen
    if !matches!(code, "zh" | "ja" | "ko") {
        return true;
    }
    let mut defs = egui::FontDefinitions::default();
    let mut ok = true;
    if matches!(code, "zh" | "ja" | "ko") {
        match cjk_font(code) {
            Some((bytes, idx)) => {
                let mut fd = egui::FontData::from_owned(bytes);
                fd.index = idx;
                defs.font_data.insert("cjk".into(), std::sync::Arc::new(fd));
                for fam in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
                    defs.families.entry(fam).or_default().push("cjk".into());
                }
            }
            None => ok = false,
        }
    }
    ctx.set_fonts(defs);
    ok
}
