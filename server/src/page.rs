//! HTML de los listados. Sin JavaScript; todo nombre de archivo se escapa.

use crate::jail::pct_encode;
use crate::timefmt;

pub struct Row {
    pub name: String,
    pub is_dir: bool,
    pub size: u64,
    pub mtime: i64,
}

const PREVIEW_MAX: u64 = 1 << 20;

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

fn shell(title: &str, body: &str) -> String {
    format!(
        "<!doctype html><html lang=\"es\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\
         <meta name=\"referrer\" content=\"no-referrer\"><title>{}</title><style>{CSS}</style></head><body>{body}</body></html>",
        esc(title)
    )
}

fn crumbs(token: &str, comps: &[String]) -> String {
    let mut h = format!("<a href=\"/s/{token}/\">Inicio</a>");
    for i in 0..comps.len() {
        h.push_str(&format!(" / <a href=\"{}\">{}</a>", base_url(token, &comps[..=i]), esc(&comps[i])));
    }
    h
}

pub fn roots_page(token: &str, names: &[String], web: bool) -> String {
    let mut b = String::from("<h1>Carpetas compartidas</h1>");
    if web {
        b.push_str(&format!("<p><a href=\"/s/{token}/app/\">Abrir la interfaz web</a></p>"));
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
    shell("Carpetas compartidas", &b)
}

pub fn dir_page(token: &str, comps: &[String], rows: &[Row], sort: &str, desc: bool, truncated: bool) -> String {
    let base = base_url(token, comps);
    let th = |k: &str, label: &str| {
        let d = if sort == k && !desc { "&amp;o=d" } else { "" };
        format!("<th><a href=\"?sort={k}{d}\">{label}</a></th>")
    };
    let mut b = format!("<h1>{}</h1>", crumbs(token, comps));
    b.push_str(&format!(
        "<div class=\"bar\"><form method=\"get\" action=\"{base}\"><input type=\"text\" name=\"q\" placeholder=\"Buscar por nombre\"> <input type=\"submit\" value=\"Buscar\"></form>\
         <a href=\"{base}?zip=1\">Descargar carpeta (ZIP)</a></div>"
    ));
    b.push_str(&format!("<table><tr>{}{}{}</tr>", th("name", "Nombre"), th("size", "Tamaño"), th("date", "Modificado")));
    if comps.len() > 1 {
        b.push_str(&format!("<tr><td class=\"n\"><a href=\"{}\">..</a></td><td></td><td></td></tr>", base_url(token, &comps[..comps.len() - 1])));
    } else {
        b.push_str(&format!("<tr><td class=\"n\"><a href=\"/s/{token}/\">..</a></td><td></td><td></td></tr>"));
    }
    for r in rows {
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
                "<tr><td class=\"n\">{thumb}<a href=\"{base}{e}\">{}</a> <a href=\"{base}{e}?dl=1\" title=\"Descargar\">⬇</a></td><td class=\"s\">{}</td><td class=\"d\">{}</td></tr>",
                esc(&r.name),
                human(r.size),
                timefmt::short(r.mtime)
            ));
        }
    }
    b.push_str("</table>");
    if truncated {
        b.push_str("<p>Listado recortado: hay más entradas de las que se muestran.</p>");
    }
    shell(comps.last().map(String::as_str).unwrap_or("Carpeta"), &b)
}

pub struct Hit {
    pub rel: Vec<String>,
    pub is_dir: bool,
    pub size: u64,
}

pub fn search_page(token: &str, comps: &[String], q: &str, hits: &[Hit], truncated: bool) -> String {
    let base = base_url(token, comps);
    let mut b = format!("<h1>{}</h1><p>Resultados para «{}»: <a href=\"{base}\">volver</a></p><table>", crumbs(token, comps), esc(q));
    for h in hits {
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
        b.push_str("<p>Sin resultados.</p>");
    }
    if truncated {
        b.push_str("<p>Búsqueda recortada por tamaño o tiempo; afina el término.</p>");
    }
    shell("Búsqueda", &b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapa_nombres_hostiles() {
        let rows = vec![Row { name: "<script>alert(1)</script>\"'&.txt".into(), is_dir: false, size: 1, mtime: 0 }];
        let h = dir_page("tok", &["r".into()], &rows, "name", false, false);
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
        let h = dir_page("t", &["r".into()], &rows, "name", false, false);
        assert_eq!(h.matches("<img ").count(), 1);
    }

    #[test]
    fn tamanos() {
        assert_eq!(human(0), "0 B");
        assert_eq!(human(1536), "1.5 KiB");
    }
}
