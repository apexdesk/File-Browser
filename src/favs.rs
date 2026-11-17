//! Favoritos de la barra lateral: una sola lista con los de serie (Inicio, Documentos…) y las carpetas
//! que añade el usuario (los antiguos «marcadores»), en el orden que él elija.
//!
//! En la configuración se guarda la lista completa (`fav=`) y, para poder volver a una versión
//! anterior sin perder nada, también el formato antiguo: `fav_order` (orden de los de serie),
//! `fav_hidden` (los de serie quitados) y `bookmark` (las carpetas del usuario).
use std::path::{Path, PathBuf};

/// Lista visible de favoritos.
/// - `builtins`: los de serie que existen ahora, en su orden natural.
/// - `favs`: la lista guardada en el formato nuevo (`None` en una configuración antigua).
/// - `hidden`/`order`/`bookmarks`: el formato antiguo.
pub fn merged(builtins: &[PathBuf], favs: Option<&[PathBuf]>, hidden: &[PathBuf], order: &[PathBuf], bookmarks: &[PathBuf]) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    let push = |out: &mut Vec<PathBuf>, p: &PathBuf| {
        if !p.as_os_str().is_empty() && !out.contains(p) {
            out.push(p.clone());
        }
    };
    match favs {
        Some(list) => {
            for p in list {
                // un favorito de serie ya no presente (la carpeta desapareció) se omite; uno del usuario se muestra
                // atenuado. Si figura como oculto, manda la lista (se añadió después de ocultarlo).
                push(&mut out, p);
            }
            // favoritos de serie nuevos (p. ej. una carpeta XDG creada después) que el usuario no ha quitado
            for b in builtins {
                if !hidden.contains(b) {
                    push(&mut out, b);
                }
            }
        }
        None => {
            // configuración antigua: los de serie con su orden y sin los ocultos, y detrás los marcadores
            let mut v: Vec<&PathBuf> = builtins.iter().filter(|p| !hidden.contains(p)).collect();
            v.sort_by_key(|p| order.iter().position(|o| o == *p).unwrap_or(usize::MAX));
            for p in v {
                push(&mut out, p);
            }
        }
    }
    // marcadores escritos por una versión anterior (o por otra ventana con ella) que aún no están
    for b in bookmarks {
        push(&mut out, b);
    }
    out
}

/// Lo que se escribe en la configuración para la lista `list`: (fav, fav_order, bookmarks).
pub fn store(list: &[PathBuf], builtins: &[PathBuf]) -> (Vec<PathBuf>, Vec<PathBuf>, Vec<PathBuf>) {
    let order = list.iter().filter(|p| builtins.contains(p)).cloned().collect();
    let user = list.iter().filter(|p| !builtins.contains(p)).cloned().collect();
    (list.to_vec(), order, user)
}

/// Quita `p` de la lista: si es de serie queda además en `hidden` (para que no vuelva a aparecer).
pub fn remove(list: &mut Vec<PathBuf>, hidden: &mut Vec<PathBuf>, builtins: &[PathBuf], p: &Path) {
    list.retain(|x| x != p);
    if builtins.iter().any(|b| b == p) && !hidden.iter().any(|h| h == p) {
        hidden.push(p.to_path_buf());
    }
}

/// Añade `p` al final (si no estaba); si era uno de serie oculto, deja de estarlo.
pub fn add(list: &mut Vec<PathBuf>, hidden: &mut Vec<PathBuf>, p: &Path) -> bool {
    hidden.retain(|h| h != p);
    if list.iter().any(|x| x == p) {
        return false;
    }
    list.push(p.to_path_buf());
    true
}

/// Mueve el elemento `from` a la posición `to` (índice en la lista resultante).
pub fn move_item(list: &mut Vec<PathBuf>, from: usize, to: usize) -> bool {
    if from >= list.len() || from == to {
        return false;
    }
    let it = list.remove(from);
    let to = to.min(list.len());
    list.insert(to, it);
    true
}

/// «Restaurar favoritos»: vuelven los de serie (en su orden natural, delante) sin perder los del usuario.
pub fn restore(list: &[PathBuf], builtins: &[PathBuf]) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = builtins.to_vec();
    for p in list {
        if !out.contains(p) {
            out.push(p.clone());
        }
    }
    out
}

/// Nombre personalizado de un favorito.
pub fn label_of<'a>(labels: &'a [(PathBuf, String)], p: &Path) -> Option<&'a str> {
    labels.iter().find(|(q, _)| q == p).map(|(_, l)| l.as_str())
}

/// Pone (o quita, si queda vacío) el nombre personalizado de `p`.
pub fn set_label(labels: &mut Vec<(PathBuf, String)>, p: &Path, label: &str) {
    let label = label.trim().replace(['\n', '\r'], " ");
    labels.retain(|(q, _)| q != p);
    if !label.is_empty() {
        labels.push((p.to_path_buf(), label));
    }
}

// ---------------------------------------------------------------------------
// Pares «ruta|valor» en una línea de la configuración

/// Escapa `%`, `|` y saltos de línea para poder unir dos textos con `|` en una línea.
pub fn esc(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '%' => o.push_str("%25"),
            '|' => o.push_str("%7C"),
            '\n' => o.push_str("%0A"),
            '\r' => o.push_str("%0D"),
            c => o.push(c),
        }
    }
    o
}

pub fn unesc(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('%') {
        o.push_str(&rest[..i]);
        let code = rest.get(i + 1..i + 3);
        let ch = match code {
            Some("25") => Some('%'),
            Some("7C") | Some("7c") => Some('|'),
            Some("0A") | Some("0a") => Some('\n'),
            Some("0D") | Some("0d") => Some('\r'),
            _ => None,
        };
        match ch {
            Some(c) => {
                o.push(c);
                rest = &rest[i + 3..];
            }
            None => {
                o.push('%');
                rest = &rest[i + 1..];
            }
        }
    }
    o.push_str(rest);
    o
}

/// `ruta|valor` (ambos escapados).
pub fn pair(p: &Path, v: &str) -> String {
    format!("{}|{}", esc(&p.to_string_lossy()), esc(v))
}

pub fn split_pair(s: &str) -> Option<(PathBuf, String)> {
    let (a, b) = s.split_once('|')?;
    let p = unesc(a);
    if p.is_empty() {
        return None;
    }
    Some((PathBuf::from(p), unesc(b)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pb(v: &[&str]) -> Vec<PathBuf> {
        v.iter().map(PathBuf::from).collect()
    }

    #[test]
    fn old_config_migrates_after_builtins_without_duplicates() {
        let builtins = pb(&["/h", "/h/Docs", "/h/Down", "/h/Pics"]);
        let hidden = pb(&["/h/Pics"]);
        let order = pb(&["/h/Down", "/h", "/h/Docs"]);
        let bookmarks = pb(&["/w/a", "/h/Docs", "/w/b", "/w/a"]);
        let m = merged(&builtins, None, &hidden, &order, &bookmarks);
        assert_eq!(m, pb(&["/h/Down", "/h", "/h/Docs", "/w/a", "/w/b"]));
        // un marcador que era un favorito de serie oculto vuelve como favorito
        let m = merged(&builtins, None, &hidden, &[], &pb(&["/h/Pics"]));
        assert_eq!(m, pb(&["/h", "/h/Docs", "/h/Down", "/h/Pics"]));
    }

    #[test]
    fn new_list_wins_and_new_builtins_are_appended() {
        let builtins = pb(&["/h", "/h/Docs", "/h/Proj"]);
        let favs = pb(&["/w/a", "/h", "/h/Docs"]);
        let m = merged(&builtins, Some(&favs), &[], &[], &pb(&["/w/a"]));
        assert_eq!(m, pb(&["/w/a", "/h", "/h/Docs", "/h/Proj"]));
        // uno de serie quitado no vuelve
        let m = merged(&builtins, Some(&favs), &pb(&["/h/Proj"]), &[], &[]);
        assert_eq!(m, pb(&["/w/a", "/h", "/h/Docs"]));
        // marcador añadido por una versión anterior: al final
        let m = merged(&builtins, Some(&favs), &pb(&["/h/Proj"]), &[], &pb(&["/w/a", "/w/z"]));
        assert_eq!(m, pb(&["/w/a", "/h", "/h/Docs", "/w/z"]));
    }

    #[test]
    fn store_writes_old_format_too() {
        let builtins = pb(&["/h", "/h/Docs"]);
        let list = pb(&["/w/a", "/h/Docs", "/h", "/w/b"]);
        let (f, o, b) = store(&list, &builtins);
        assert_eq!(f, list);
        assert_eq!(o, pb(&["/h/Docs", "/h"]));
        assert_eq!(b, pb(&["/w/a", "/w/b"]));
        // y vuelve a dar lo mismo
        assert_eq!(merged(&builtins, Some(&f), &[], &o, &b), list);
        // una versión anterior vería lo mismo: los de serie en su orden y los del usuario como marcadores
        assert_eq!(merged(&builtins, None, &[], &o, &b), pb(&["/h/Docs", "/h", "/w/a", "/w/b"]));
    }

    #[test]
    fn remove_add_move_restore() {
        let builtins = pb(&["/h", "/h/Docs"]);
        let mut list = pb(&["/h", "/w/a", "/h/Docs"]);
        let mut hidden = Vec::new();
        remove(&mut list, &mut hidden, &builtins, Path::new("/h"));
        remove(&mut list, &mut hidden, &builtins, Path::new("/w/a"));
        assert_eq!(list, pb(&["/h/Docs"]));
        assert_eq!(hidden, pb(&["/h"]));
        assert!(add(&mut list, &mut hidden, Path::new("/w/c")));
        assert!(!add(&mut list, &mut hidden, Path::new("/w/c")));
        assert!(add(&mut list, &mut hidden, Path::new("/h")));
        assert!(hidden.is_empty());
        assert_eq!(list, pb(&["/h/Docs", "/w/c", "/h"]));
        assert!(move_item(&mut list, 2, 0));
        assert_eq!(list, pb(&["/h", "/h/Docs", "/w/c"]));
        assert!(move_item(&mut list, 0, 99));
        assert_eq!(list, pb(&["/h/Docs", "/w/c", "/h"]));
        assert!(!move_item(&mut list, 5, 0));
        let r = restore(&pb(&["/w/c", "/w/d"]), &builtins);
        assert_eq!(r, pb(&["/h", "/h/Docs", "/w/c", "/w/d"]));
    }

    #[test]
    fn labels_and_pairs() {
        let mut l = Vec::new();
        set_label(&mut l, Path::new("/a|b"), "Mi | carpeta\n");
        assert_eq!(label_of(&l, Path::new("/a|b")), Some("Mi | carpeta"));
        let line = pair(Path::new("/a|b%"), "x|y%z");
        assert!(!line[line.find('|').unwrap() + 1..].contains('|'));
        assert_eq!(split_pair(&line), Some((PathBuf::from("/a|b%"), "x|y%z".to_string())));
        assert_eq!(unesc("50%zz%7c"), "50%zz|");
        set_label(&mut l, Path::new("/a|b"), "  ");
        assert!(l.is_empty());
        assert_eq!(split_pair("|x"), None);
    }
}
