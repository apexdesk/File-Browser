//! Cliente web embebido (generado por build.rs a partir de `webui/`) y API JSON.

use crate::page::Row;

include!(concat!(env!("OUT_DIR"), "/webui.rs"));

/// ¿El binario lleva el cliente completo (HTML + wasm)?
pub fn available() -> bool {
    FILES.iter().any(|f| f.0 == "index.html") && FILES.iter().any(|f| f.0.ends_with(".wasm"))
}

pub fn get(name: &str) -> Option<(&'static str, &'static [u8])> {
    FILES.iter().find(|f| f.0 == name).map(|f| (f.1, f.2))
}

/// CSP del cliente: scripts y wasm solo del propio origen.
pub const CSP: &str = "default-src 'none'; script-src 'self' 'wasm-unsafe-eval'; connect-src 'self'; img-src 'self' data: blob:; media-src 'self'; style-src 'unsafe-inline'; frame-ancestors 'none'; base-uri 'none'; form-action 'none'";

pub fn json_str(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 2);
    o.push('"');
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            c if (c as u32) < 0x20 || c == '\u{7f}' => o.push_str(&format!("\\u{:04x}", c as u32)),
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

pub fn list_json(comps: &[String], rows: &[Row], mut truncated: bool) -> String {
    let path: Vec<String> = comps.iter().map(|c| json_str(c)).collect();
    // mismo tope de bytes que el listado HTML
    let mut e = String::new();
    for (i, r) in rows.iter().enumerate() {
        if e.len() > crate::page::MAX_PAGE {
            truncated = true;
            break;
        }
        if i > 0 {
            e.push(',');
        }
        e.push_str(&format!("{{\"n\":{},\"d\":{},\"s\":{},\"m\":{}}}", json_str(&r.name), r.is_dir, r.size, r.mtime));
    }
    format!("{{\"path\":[{}],\"truncated\":{truncated},\"entries\":[{e}]}}", path.join(","))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_escapa() {
        assert_eq!(json_str("a\"b\\c\n\u{1}é"), "\"a\\\"b\\\\c\\u000a\\u0001é\"");
        let rows = vec![Row { name: "x\"y".into(), is_dir: true, size: 0, mtime: 5 }];
        assert_eq!(list_json(&["r".into()], &rows, false), "{\"path\":[\"r\"],\"truncated\":false,\"entries\":[{\"n\":\"x\\\"y\",\"d\":true,\"s\":0,\"m\":5}]}");
    }
}
