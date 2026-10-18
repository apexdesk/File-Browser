//! Confinamiento: toda ruta que llega de la red se valida aquí antes de tocar el disco.
//! Es la pieza que más hay que revisar.

use std::path::{Path, PathBuf};

pub struct Root {
    /// Nombre visible en la URL (último componente de la carpeta compartida).
    pub name: String,
    /// Ruta canónica; todo lo servido debe quedar dentro.
    pub path: PathBuf,
}

/// Decodifica %XX una sola vez. `None` si es inválido o el resultado no es UTF-8.
pub fn pct_decode(s: &str) -> Option<String> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            let h = b.get(i + 1..i + 3)?;
            let h = std::str::from_utf8(h).ok()?;
            out.push(u8::from_str_radix(h, 16).ok()?);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// Codifica un componente para usarlo en una URL (todo salvo caracteres no reservados).
pub fn pct_encode(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for &c in s.as_bytes() {
        if c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_' | b'.' | b'~') {
            o.push(c as char);
        } else {
            o.push_str(&format!("%{c:02X}"));
        }
    }
    o
}

/// Un componente de ruta aceptable: sin separadores, sin controles, sin `.`/`..`.
pub fn valid_component(c: &str) -> bool {
    if c.is_empty() || c == "." || c == ".." || c.len() > 255 {
        return false;
    }
    if c.chars().any(|ch| ch == '/' || ch == '\\' || ch.is_control()) {
        return false;
    }
    #[cfg(windows)]
    {
        if c.contains(':') || c.ends_with('.') || c.ends_with(' ') {
            return false;
        }
        let stem = c.split('.').next().unwrap_or("").to_ascii_uppercase();
        let reservado = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$")
            || ((stem.starts_with("COM") || stem.starts_with("LPT")) && stem.len() == 4 && stem.as_bytes()[3].is_ascii_digit());
        if reservado {
            return false;
        }
    }
    true
}

/// Parte `/a/b/c/` (resto de la URL tras el token) en componentes ya decodificados y validados.
/// Devuelve también si acababa en `/`.
pub fn split_url(tail: &str) -> Option<(Vec<String>, bool)> {
    let t = tail.strip_prefix('/')?;
    if t.is_empty() {
        return Some((Vec::new(), true));
    }
    let slash = t.ends_with('/');
    let t = if slash { &t[..t.len() - 1] } else { t };
    let mut comps = Vec::new();
    for raw in t.split('/') {
        let c = pct_decode(raw)?;
        if !valid_component(&c) {
            return None;
        }
        comps.push(c);
        if comps.len() > 64 {
            return None;
        }
    }
    Some((comps, slash))
}

fn hidden_in(rel: &Path) -> bool {
    rel.components().any(|c| c.as_os_str().to_str().is_none_or(|s| s.starts_with('.')))
}

/// ¿La ruta canónica queda dentro de la raíz (y, salvo `hidden`, sin partes ocultas)?
pub fn inside(root: &Root, canon: &Path, hidden: bool) -> bool {
    match canon.strip_prefix(&root.path) {
        Ok(rel) => hidden || !hidden_in(rel),
        Err(_) => false,
    }
}

/// Resuelve `comps` (el primero es el nombre de la raíz) a una ruta canónica dentro de ella.
pub fn resolve<'a>(roots: &'a [Root], comps: &[String], hidden: bool) -> Option<(&'a Root, PathBuf)> {
    let (first, rest) = comps.split_first()?;
    let root = roots.iter().find(|r| &r.name == first)?;
    if !hidden && rest.iter().any(|c| c.starts_with('.')) {
        return None;
    }
    let mut p = root.path.clone();
    for c in rest {
        p.push(c);
    }
    let canon = std::fs::canonicalize(&p).ok()?;
    inside(root, &canon, hidden).then_some((root, canon))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("fbs-{tag}-{}-{}", std::process::id(), line!()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn decodifica_una_sola_vez() {
        assert_eq!(pct_decode("a%20b").as_deref(), Some("a b"));
        assert_eq!(pct_decode("%252e").as_deref(), Some("%2e")); // no se decodifica dos veces
        assert!(pct_decode("%zz").is_none());
        assert!(pct_decode("%f0%28").is_none()); // UTF-8 inválido
        assert!(pct_decode("%").is_none());
    }

    #[test]
    fn rechaza_traversal_y_raros() {
        for u in ["/..", "/a/../b", "/a/%2e%2e/b", "/a/%2E%2E", "/a//b", "/a/b%2Fc", "/a/b%5Cc", "/a/%00", "/a/%0a", "/a\\b"] {
            assert!(split_url(u).is_none(), "{u}");
        }
        assert_eq!(split_url("/a/b/").unwrap(), (s(&["a", "b"]), true));
        assert_eq!(split_url("/a/b").unwrap(), (s(&["a", "b"]), false));
        assert_eq!(split_url("/").unwrap(), (vec![], true));
        assert!(split_url("a").is_none());
    }

    #[test]
    fn resuelve_dentro_y_oculta_puntos() {
        let d = tmp("res");
        fs::create_dir_all(d.join("sub")).unwrap();
        fs::write(d.join("sub/f.txt"), "x").unwrap();
        fs::write(d.join(".secreto"), "x").unwrap();
        let roots = vec![Root { name: "r".into(), path: fs::canonicalize(&d).unwrap() }];
        assert!(resolve(&roots, &s(&["r", "sub", "f.txt"]), false).is_some());
        assert!(resolve(&roots, &s(&["r", ".secreto"]), false).is_none());
        assert!(resolve(&roots, &s(&["r", ".secreto"]), true).is_some());
        assert!(resolve(&roots, &s(&["otra", "sub"]), false).is_none());
        assert!(resolve(&roots, &s(&["r", "no-existe"]), false).is_none());
        let _ = fs::remove_dir_all(&d);
    }

    #[cfg(unix)]
    #[test]
    fn enlace_que_sale_o_apunta_a_oculto_se_rechaza() {
        use std::os::unix::fs::symlink;
        let d = tmp("sym");
        let fuera = tmp("fuera");
        fs::write(fuera.join("pass"), "x").unwrap();
        fs::create_dir_all(d.join(".priv")).unwrap();
        fs::write(d.join(".priv/k"), "x").unwrap();
        symlink(fuera.join("pass"), d.join("sale")).unwrap();
        symlink(&fuera, d.join("dir-sale")).unwrap();
        symlink(d.join(".priv/k"), d.join("a-oculto")).unwrap();
        fs::write(d.join("ok"), "x").unwrap();
        symlink(d.join("ok"), d.join("enlace-ok")).unwrap();
        let roots = vec![Root { name: "r".into(), path: fs::canonicalize(&d).unwrap() }];
        assert!(resolve(&roots, &s(&["r", "sale"]), false).is_none());
        assert!(resolve(&roots, &s(&["r", "dir-sale", "pass"]), false).is_none());
        assert!(resolve(&roots, &s(&["r", "a-oculto"]), false).is_none());
        assert!(resolve(&roots, &s(&["r", "enlace-ok"]), false).is_some());
        let _ = fs::remove_dir_all(&d);
        let _ = fs::remove_dir_all(&fuera);
    }

    #[test]
    fn prefijo_parecido_no_cuenta_como_dentro() {
        let d = tmp("pref");
        let hermana = PathBuf::from(format!("{}-x", d.display()));
        fs::create_dir_all(&hermana).unwrap();
        let r = Root { name: "r".into(), path: fs::canonicalize(&d).unwrap() };
        assert!(!inside(&r, &fs::canonicalize(&hermana).unwrap(), true));
        let _ = fs::remove_dir_all(&d);
        let _ = fs::remove_dir_all(&hermana);
    }

    #[test]
    fn codifica_para_url() {
        assert_eq!(pct_encode("a b/ñ.txt"), "a%20b%2F%C3%B1.txt");
    }
}
