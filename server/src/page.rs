//! HTML de los listados. Sin JavaScript; todo nombre de archivo se escapa.

use crate::http::Lang;
use crate::jail::pct_encode;
use crate::timefmt;

pub struct Row {
    pub name: String,
    pub is_dir: bool,
    pub size: u64,
    pub mtime: i64,
}

const PREVIEW_MAX: u64 = 1 << 20;
/// Tope del HTML de un listado o una búsqueda: con nombres largos y llenos de caracteres a escapar,
/// 20 000 filas podrían ocupar cientos de MiB en memoria. Pasado el tope se recorta (y se avisa).
pub const MAX_PAGE: usize = 4 << 20;

fn is_image(name: &str) -> bool {
    matches!(name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).as_deref(), Some("png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp"))
}

pub fn esc(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => o.push_str("&amp;"),
            '<' => o.push_str("&lt;"),
            '>' => o.push_str("&gt;"),
            '"' => o.push_str("&quot;"),
            '\'' => o.push_str("&#39;"),
            c => o.push(c),
        }
    }
    o
}

pub fn human(n: u64) -> String {
    const U: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i < U.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 { format!("{n} B") } else { format!("{v:.1} {}", U[i]) }
}

/// `/s/<token>/a/b/` para los componentes dados.
pub fn base_url(token: &str, comps: &[String]) -> String {
    let mut u = format!("/s/{token}/");
    for c in comps {
        u.push_str(&pct_encode(c));
        u.push('/');
    }
    u
}

const CSS: &str = "body{font:15px system-ui,sans-serif;margin:0;padding:16px;max-width:60em;margin-inline:auto;background:#fff;color:#1b1b1f}\
a{color:#1a56c4;text-decoration:none}a:hover{text-decoration:underline}h1{font-size:1.1em;word-break:break-all}\
table{width:100%;border-collapse:collapse}td,th{padding:6px 8px;border-bottom:1px solid #8884;text-align:left}\
td.n{width:100%;word-break:break-all}td.s,td.d{white-space:nowrap;color:#777}.bar{display:flex;gap:12px;flex-wrap:wrap;align-items:center;margin:10px 0}\
input[type=text]{padding:4px 8px;border:1px solid #8886;border-radius:4px;background:transparent;color:inherit}\
@media(prefers-color-scheme:dark){body{background:#17181c;color:#e6e6e9}a{color:#8ab4ff}}";

fn shell(l: Lang, title: &str, body: &str) -> String {
    format!(
        "<!doctype html><html lang=\"{}\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\
         <meta name=\"referrer\" content=\"no-referrer\"><title>{}</title><style>{CSS}</style></head><body>{body}</body></html>",
        l.code(),
        esc(title)
    )
}

fn crumbs(l: Lang, token: &str, comps: &[String]) -> String {
    let mut h = format!("<a href=\"/s/{token}/\">{}</a>", l.tr("Inicio", "Home"));
    for i in 0..comps.len() {
        h.push_str(&format!(" / <a href=\"{}\">{}</a>", base_url(token, &comps[..=i]), esc(&comps[i])));
    }
    h
}

/// Aviso cuando el servidor deja escribir pero el binario no lleva la interfaz web: estas páginas no
/// tienen acciones de escritura.
fn ro_note(l: Lang) -> String {
    format!(
        "<p><em>{}</em></p>",
        l.tr(
            "Solo lectura desde esta página: el servidor permite escribir, pero este binario no incluye la interfaz web.",
            "Read-only from this page: the server allows writing, but this build does not include the web interface."
        )
    )
}

pub fn roots_page(l: Lang, token: &str, names: &[String], web: bool, write: bool) -> String {
    let title = l.tr("Carpetas compartidas", "Shared folders");
    let mut b = format!("<h1>{title}</h1>");
    if web {
        b.push_str(&format!("<p><a href=\"/s/{token}/app/\">{}</a></p>", l.tr("Abrir la interfaz web", "Open the web interface")));
    } else if write {
        b.push_str(&ro_note(l));
    }
    b.push_str("<table>");
    for n in names {
        b.push_str(&format!(
            "<tr><td class=\"n\"><a href=\"/s/{token}/{e}/\">{}/</a></td><td class=\"s\"><a href=\"/s/{token}/{e}/?zip=1\">ZIP</a></td></tr>",
            esc(n),
            e = pct_encode(n)
        ));
    }
    b.push_str("</table>");
    shell(l, title, &b)
}

/// `ro`: el servidor deja escribir pero no hay interfaz web (ver `ro_note`).
#[allow(clippy::too_many_arguments)]
pub fn dir_page(l: Lang, token: &str, comps: &[String], rows: &[Row], sort: &str, desc: bool, mut truncated: bool, ro: bool) -> String {
    let base = base_url(token, comps);
    let th = |k: &str, label: &str| {
        let d = if sort == k && !desc { "&amp;o=d" } else { "" };
        format!("<th><a href=\"?sort={k}{d}\">{label}</a></th>")
    };
    let mut b = format!("<h1>{}</h1>", crumbs(l, token, comps));
    if ro {
        b.push_str(&ro_note(l));
    }
    b.push_str(&format!(
        "<div class=\"bar\"><form method=\"get\" action=\"{base}\"><input type=\"text\" name=\"q\" placeholder=\"{}\"> <input type=\"submit\" value=\"{}\"></form>\
         <a href=\"{base}?zip=1\">{}</a></div>",
        l.tr("Buscar por nombre", "Search by name"),
        l.tr("Buscar", "Search"),
        l.tr("Descargar carpeta (ZIP)", "Download folder (ZIP)")
    ));
    b.push_str(&format!(
        "<table><tr>{}{}{}</tr>",
        th("name", l.tr("Nombre", "Name")),
        th("size", l.tr("Tamaño", "Size")),
        th("date", l.tr("Modificado", "Modified"))
    ));
    let dl = l.tr("Descargar", "Download");
    if comps.len() > 1 {
        b.push_str(&format!("<tr><td class=\"n\"><a href=\"{}\">..</a></td><td></td><td></td></tr>", base_url(token, &comps[..comps.len() - 1])));
    } else {
        b.push_str(&format!("<tr><td class=\"n\"><a href=\"/s/{token}/\">..</a></td><td></td><td></td></tr>"));
    }
    for r in rows {
        if b.len() > MAX_PAGE {
            truncated = true;
            break;
        }
        let e = pct_encode(&r.name);
        if r.is_dir {
            b.push_str(&format!(
                "<tr><td class=\"n\"><a href=\"{base}{e}/\">{}/</a></td><td class=\"s\"><a href=\"{base}{e}/?zip=1\">ZIP</a></td><td class=\"d\">{}</td></tr>",
                esc(&r.name),
                timefmt::short(r.mtime)
            ));
        } else {
            // vista previa solo de imágenes pequeñas; las decodifica el navegador, no el servidor
            let thumb = if r.size <= PREVIEW_MAX && is_image(&r.name) {
                format!("<img loading=\"lazy\" src=\"{base}{e}\" width=\"40\" height=\"40\" alt=\"\" style=\"object-fit:cover;vertical-align:middle;margin-right:8px;border-radius:3px\">")
            } else {
                String::new()
            };
            b.push_str(&format!(
                "<tr><td class=\"n\">{thumb}<a href=\"{base}{e}\">{}</a> <a href=\"{base}{e}?dl=1\" title=\"{dl}\">⬇</a></td><td class=\"s\">{}</td><td class=\"d\">{}</td></tr>",
                esc(&r.name),
                human(r.size),
                timefmt::short(r.mtime)
            ));
        }
    }
    b.push_str("</table>");
    if truncated {
        b.push_str(l.tr("<p>Listado recortado: hay más entradas de las que se muestran.</p>", "<p>Listing cut short: there are more entries than shown.</p>"));
    }
    shell(l, comps.last().map(String::as_str).unwrap_or(l.tr("Carpeta", "Folder")), &b)
}

pub struct Hit {
    pub rel: Vec<String>,
    pub is_dir: bool,
    pub size: u64,
}

/// `unreadable`: carpetas que no se pudieron leer durante la búsqueda.
#[allow(clippy::too_many_arguments)]
pub fn search_page(l: Lang, token: &str, comps: &[String], q: &str, hits: &[Hit], mut truncated: bool, unreadable: usize) -> String {
    let base = base_url(token, comps);
    let mut b = format!(
        "<h1>{}</h1><p>{} «{}»: <a href=\"{base}\">{}</a></p><table>",
        crumbs(l, token, comps),
        l.tr("Resultados para", "Results for"),
        esc(q),
        l.tr("volver", "back")
    );
    for h in hits {
        if b.len() > MAX_PAGE {
            truncated = true;
            break;
        }
        let mut u = base.clone();
        for (i, c) in h.rel.iter().enumerate() {
            u.push_str(&pct_encode(c));
            if i + 1 < h.rel.len() || h.is_dir {
                u.push('/');
            }
        }
        let size = if h.is_dir { String::new() } else { human(h.size) };
        b.push_str(&format!("<tr><td class=\"n\"><a href=\"{u}\">{}{}</a></td><td class=\"s\">{size}</td></tr>", esc(&h.rel.join("/")), if h.is_dir { "/" } else { "" }));
    }
    b.push_str("</table>");
    if hits.is_empty() {
        b.push_str(l.tr("<p>Sin resultados.</p>", "<p>No results.</p>"));
    }
    if truncated {
        b.push_str(l.tr("<p>Búsqueda recortada por tamaño o tiempo; afina el término.</p>", "<p>Search cut short by size or time; refine the term.</p>"));
    }
    if unreadable > 0 {
        b.push_str(&format!("<p>{}: {unreadable}</p>", l.tr("Carpetas que no se pudieron leer", "Folders that could not be read")));
    }
    shell(l, l.tr("Búsqueda", "Search"), &b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapa_nombres_hostiles() {
        let rows = vec![Row { name: "<script>alert(1)</script>\"'&.txt".into(), is_dir: false, size: 1, mtime: 0 }];
        let h = dir_page(Lang::Es, "tok", &["r".into()], &rows, "name", false, false, false);
        assert!(!h.contains("<script>"));
        assert!(h.contains("&lt;script&gt;"));
        assert!(h.contains("%3Cscript%3E"));
    }

    #[test]
    fn vista_previa_solo_de_imagenes_pequenas() {
        let rows = vec![
            Row { name: "a.PNG".into(), is_dir: false, size: 10, mtime: 0 },
            Row { name: "grande.jpg".into(), is_dir: false, size: PREVIEW_MAX + 1, mtime: 0 },
            Row { name: "v.svg".into(), is_dir: false, size: 10, mtime: 0 },
        ];
        let h = dir_page(Lang::Es, "t", &["r".into()], &rows, "name", false, false, false);
        assert_eq!(h.matches("<img ").count(), 1);
    }

    #[test]
    fn el_listado_tiene_tope_de_bytes() {
        // 255 caracteres que se escapan a 5-6 bytes cada uno y además se codifican en la URL
        let name: String = "<&\"'>".chars().cycle().take(255).collect();
        let rows: Vec<Row> = (0..20_000).map(|_| Row { name: name.clone(), is_dir: false, size: 1, mtime: 0 }).collect();
        let h = dir_page(Lang::Es, "t", &["r".into()], &rows, "name", false, false, false);
        assert!(h.len() < MAX_PAGE + 64 * 1024, "{}", h.len());
        assert!(h.contains("Listado recortado"));
        let hits: Vec<Hit> = (0..300).map(|_| Hit { rel: vec![name.clone(); 64], is_dir: false, size: 1 }).collect();
        let h = search_page(Lang::Es, "t", &["r".into()], "x", &hits, false, 0);
        assert!(h.len() < MAX_PAGE + 256 * 1024, "{}", h.len());
        assert!(h.contains("recortada"));
    }

    #[test]
    fn paginas_en_ingles_y_aviso_de_solo_lectura() {
        let h = dir_page(Lang::En, "t", &["r".into()], &[], "name", false, true, true);
        assert!(h.contains("lang=\"en\"") && h.contains("Download folder") && h.contains("Listing cut short") && h.contains("Read-only"));
        let h = roots_page(Lang::Es, "t", &["r".into()], false, true);
        assert!(h.contains("lang=\"es\"") && h.contains("Solo lectura"));
        assert!(!roots_page(Lang::Es, "t", &["r".into()], true, true).contains("Solo lectura"));
    }

    #[test]
    fn tamanos() {
        assert_eq!(human(0), "0 B");
        assert_eq!(human(1536), "1.5 KiB");
    }
}
