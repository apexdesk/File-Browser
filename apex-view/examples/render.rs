//! Draws pages of a document to PNG files and prints their text:
//! `render FILE [WIDTH] [OUT_DIR]`.

use std::time::Instant;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let Some(path) = args.get(1) else {
        eprintln!("usage: render FILE [WIDTH] [OUT_DIR]");
        std::process::exit(2);
    };
    let width: u32 = args.get(2).and_then(|w| w.parse().ok()).unwrap_or(800);
    let out = args.get(3).map(std::path::PathBuf::from);
    let t = Instant::now();
    let mut doc = match apex_view::Document::open_path(path.as_ref()) {
        Ok(d) => d,
        Err(e) => {
            println!("{path}: {e}");
            return;
        }
    };
    println!("{path}: {:?}, {} pages, opened in {:?}", doc.format(), doc.page_count(), t.elapsed());
    for n in 0..doc.page_count().min(if out.is_some() { 50 } else { 1 }) {
        let t = Instant::now();
        match doc.render_page(n, width, &|| false) {
            Ok(img) => {
                println!("page {}: {}x{} in {:?}", n + 1, img.width, img.height, t.elapsed());
                if let Some(dir) = &out {
                    let _ = std::fs::create_dir_all(dir);
                    let _ = std::fs::write(dir.join(format!("page-{:03}.png", n + 1)), png(&img));
                }
            }
            Err(e) => println!("page {}: {e}", n + 1),
        }
        if n == 0 {
            match doc.page_text(0) {
                Ok(t) => println!("--- text ---\n{}", t.chars().take(600).collect::<String>()),
                Err(e) => println!("text: {e}"),
            }
        }
    }
}

fn png(img: &apex_view::Image) -> Vec<u8> {
    fn chunk(out: &mut Vec<u8>, kind: &[u8], data: &[u8]) {
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        let start = out.len();
        out.extend_from_slice(kind);
        out.extend_from_slice(data);
        let crc = crc32(&out[start..]);
        out.extend_from_slice(&crc.to_be_bytes());
    }
    let mut raw = Vec::new();
    for row in img.rgba.chunks(img.width as usize * 4) {
        raw.push(0);
        raw.extend_from_slice(row);
    }
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&img.width.to_be_bytes());
    ihdr.extend_from_slice(&img.height.to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
    chunk(&mut out, b"IHDR", &ihdr);
    chunk(&mut out, b"IDAT", &miniz_oxide::deflate::compress_to_vec_zlib(&raw, 3));
    chunk(&mut out, b"IEND", &[]);
    out
}

fn crc32(data: &[u8]) -> u32 {
    let mut c = 0xFFFF_FFFFu32;
    for &b in data {
        c ^= b as u32;
        for _ in 0..8 {
            c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
        }
    }
    !c
}
