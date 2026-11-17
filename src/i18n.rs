//! Traducciones: el texto en español es la clave; `t()` devuelve la cadena del idioma activo.
#[allow(unused_imports)]
use crate::vfs::{KindV as _, PathV as _};
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
    t_in(current(), s)
}

/// Como `t`, en el idioma `lang` (índice de `LANGS`) en vez del activo.
pub fn t_in(lang: usize, s: &'static str) -> &'static str {
    if lang == 0 {
        return s;
    }
    match map().get(s) {
        Some(&row) => {
            let v = TABLE[row].get(lang).copied().unwrap_or("");
            if v.is_empty() { s } else { v }
        }
        None => s,
    }
}

/// Traduce una plantilla y sustituye sus `{...}` en orden por los argumentos.
pub fn tf(key: &'static str, args: &[&dyn Display]) -> String {
    fill(t(key), args)
}

/// Forma de plural (índice dentro de la celda «forma|forma|…») que toca para `n` en el idioma `lang`.
/// es, en, de, pt, it: 1 → singular; fr: 0 y 1 → singular; ru: una / pocas / muchas;
/// zh, ja, ko: una sola forma.
pub fn plural_form(lang: usize, n: u64) -> usize {
    match LANGS.get(lang).map(|l| l.0).unwrap_or("en") {
        "fr" => (n > 1) as usize,
        "ru" => {
            let (d, c) = (n % 10, n % 100);
            if d == 1 && c != 11 {
                0
            } else if (2..=4).contains(&d) && !(12..=14).contains(&c) {
                1
            } else {
                2
            }
        }
        "zh" | "ja" | "ko" => 0,
        _ => (n != 1) as usize,
    }
}

/// Elige la forma `idx` de una celda «forma|forma|…»; si la celda tiene menos formas, la última.
fn pick_form(cell: &str, idx: usize) -> &str {
    let mut last = cell;
    for (i, f) in cell.split('|').enumerate() {
        last = f;
        if i == idx {
            break;
        }
    }
    last
}

/// Traduce una plantilla con plural: la celda lleva sus formas separadas por `|`
/// («{} elemento|{} elementos»; en ruso tres, en chino/japonés/coreano una) y se elige según `n` y la
/// regla del idioma. Los `{}` se sustituyen por `args`, o por `n` si `args` está vacío.
pub fn tn<N: TryInto<u64> + Display + Copy>(key: &'static str, n: N, args: &[&dyn Display]) -> String {
    tn_in(current(), key, n, args)
}

/// Como `tn`, en el idioma `lang`.
pub fn tn_in<N: TryInto<u64> + Display + Copy>(lang: usize, key: &'static str, n: N, args: &[&dyn Display]) -> String {
    let cell = t_in(lang, key);
    // sin traducción se usa la clave española, con la regla del español
    let rule = if std::ptr::eq(cell, key) { 0 } else { lang };
    // negativos o fuera de rango: plural «muchos»
    let k: u64 = n.try_into().unwrap_or(u64::MAX);
    let form = pick_form(cell, plural_form(rule, k));
    if args.is_empty() { fill(form, &[&n]) } else { fill(form, args) }
}

/// Sustituye en orden los `{...}` de una plantilla por los argumentos.
fn fill(tpl: &str, args: &[&dyn Display]) -> String {
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
        if let Ok(b) = crate::vfs::fs::read(&p) {
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Número de marcadores `{…}` de un texto.
    fn holes(s: &str) -> usize {
        let mut n = 0;
        let mut rest = s;
        while let Some(i) = rest.find('{') {
            let Some(j) = rest[i..].find('}') else { break };
            n += 1;
            rest = &rest[i + j + 1..];
        }
        n
    }

    /// Formas esperadas por idioma en una fila con plural.
    fn forms_for(lang: usize) -> usize {
        match LANGS[lang].0 {
            "ru" => 3,
            "zh" | "ja" | "ko" => 1,
            _ => 2,
        }
    }

    #[test]
    fn tabla_completa_y_coherente() {
        for row in TABLE {
            let es = row[0];
            let plural = es.contains('|');
            let want = es.split('|').map(holes).max().unwrap_or(0);
            for (l, cell) in row.iter().enumerate() {
                assert!(!cell.trim().is_empty(), "celda vacía ({}) en «{es}»", LANGS[l].0);
                let max = cell.split('|').map(holes).max().unwrap_or(0);
                assert_eq!(max, want, "marcadores distintos ({}) en «{es}»: «{cell}»", LANGS[l].0);
                if plural {
                    assert_eq!(cell.split('|').count(), forms_for(l), "formas de plural ({}) en «{es}»", LANGS[l].0);
                    assert!(cell.split('|').all(|f| !f.trim().is_empty()), "forma vacía ({}) en «{es}»", LANGS[l].0);
                } else {
                    assert!(!cell.contains('|'), "«|» en una fila sin plural ({}): «{es}»", LANGS[l].0);
                }
            }
        }
    }

    #[test]
    fn sin_claves_repetidas() {
        let mut seen = std::collections::HashSet::new();
        for row in TABLE {
            assert!(seen.insert(row[0]), "clave repetida: «{}»", row[0]);
        }
    }

    #[test]
    fn reglas_de_plural() {
        let cases: [(&str, &[(u64, usize)]); 10] = [
            ("es", &[(0, 1), (1, 0), (2, 1), (21, 1)]),
            ("en", &[(0, 1), (1, 0), (2, 1), (101, 1)]),
            ("fr", &[(0, 0), (1, 0), (2, 1), (21, 1)]),
            ("de", &[(0, 1), (1, 0), (2, 1)]),
            ("pt", &[(0, 1), (1, 0), (2, 1)]),
            ("it", &[(0, 1), (1, 0), (2, 1)]),
            ("ru", &[(0, 2), (1, 0), (2, 1), (4, 1), (5, 2), (11, 2), (12, 2), (14, 2), (21, 0), (22, 1), (25, 2), (111, 2), (101, 0), (1002, 1)]),
            ("zh", &[(0, 0), (1, 0), (5, 0)]),
            ("ja", &[(0, 0), (1, 0), (5, 0)]),
            ("ko", &[(0, 0), (1, 0), (5, 0)]),
        ];
        for (code, list) in cases {
            let l = index_of(code).unwrap();
            for &(n, want) in list {
                assert_eq!(plural_form(l, n), want, "{code} n={n}");
            }
        }
    }

    #[test]
    fn tn_en_los_diez_idiomas() {
        let k = "{} elemento|{} elementos";
        let want: [(&str, [&str; 3]); 10] = [
            ("es", ["1 elemento", "2 elementos", "5 elementos"]),
            ("en", ["1 item", "2 items", "5 items"]),
            ("fr", ["1 élément", "2 éléments", "5 éléments"]),
            ("de", ["1 Element", "2 Elemente", "5 Elemente"]),
            ("pt", ["1 item", "2 itens", "5 itens"]),
            ("it", ["1 elemento", "2 elementi", "5 elementi"]),
            ("ru", ["1 элемент", "2 элемента", "5 элементов"]),
            ("zh", ["1 个项目", "2 个项目", "5 个项目"]),
            ("ja", ["1 個のアイテム", "2 個のアイテム", "5 個のアイテム"]),
            ("ko", ["1개 항목", "2개 항목", "5개 항목"]),
        ];
        for (code, w) in want {
            let l = index_of(code).unwrap();
            for (n, s) in [1u64, 2, 5].iter().zip(w) {
                assert_eq!(tn_in(l, k, *n, &[]), s, "{code} n={n}");
            }
        }
        // francés: 0 es singular
        assert_eq!(tn_in(index_of("fr").unwrap(), k, 0usize, &[]), "0 élément");
        // con argumentos explícitos (el número no tiene por qué ser el primero)
        let l = index_of("en").unwrap();
        assert_eq!(tn_in(l, "{} {} elemento|{} {} elementos", 3usize, &[&"Cut:", &3]), "Cut: 3 items");
    }

    #[test]
    fn tn_degrada_con_pocas_formas_o_sin_traduccion() {
        // una clave que no está en la tabla usa sus propias formas con la regla del español
        assert_eq!(tn_in(6, "{} cosa|{} cosas", 1u64, &[]), "1 cosa");
        assert_eq!(tn_in(6, "{} cosa|{} cosas", 5u64, &[]), "5 cosas");
        // una sola forma vale para todo
        assert_eq!(tn_in(0, "{} x", 7u64, &[]), "7 x");
        // si faltan formas, la última
        assert_eq!(pick_form("a|b", 2), "b");
        assert_eq!(pick_form("a", 1), "a");
        // negativos: plural
        assert_eq!(tn_in(0, "{} cosa|{} cosas", -1i64, &[]), "-1 cosas");
    }
}
