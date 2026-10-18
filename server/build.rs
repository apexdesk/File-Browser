//! Embebe en el binario los ficheros de `webui/` (cliente web) si existen.

use std::{env, fs, path::Path};

fn main() {
    println!("cargo:rerun-if-changed=webui");
    let mut files: Vec<_> = fs::read_dir("webui").map(|rd| rd.flatten().map(|e| e.path()).filter(|p| p.is_file()).collect()).unwrap_or_default();
    files.sort();
    let mut out = String::from("pub static FILES: &[(&str, &str, &[u8])] = &[\n");
    for p in files {
        let Some(name) = p.file_name().and_then(|n| n.to_str()).map(str::to_string) else { continue };
        let ctype = match p.extension().and_then(|e| e.to_str()) {
            Some("html") => "text/html; charset=utf-8",
            Some("js") => "text/javascript; charset=utf-8",
            Some("wasm") => "application/wasm",
            Some("css") => "text/css; charset=utf-8",
            Some("png") => "image/png",
            _ => continue,
        };
        let Ok(abs) = fs::canonicalize(&p) else { continue };
        out.push_str(&format!("    ({name:?}, {ctype:?}, include_bytes!({:?})),\n", abs.to_string_lossy()));
    }
    out.push_str("];\n");
    let dest = Path::new(&env::var("OUT_DIR").expect("OUT_DIR")).join("webui.rs");
    fs::write(dest, out).expect("escribir webui.rs");
}
