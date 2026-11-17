//! Prueba de robustez: archivos de ejemplo mutados (truncados, bytes cambiados) pasados por los lectores.
//! Los pánicos de los hilos de decodificación se atrapan (`thumbs::guarded`), pero un pánico sigue
//! siendo un fallo (y fuera de esas zonas cierra la aplicación), así que aquí se buscan.
//! Se ejecuta con `FUZZ_DIR=<carpeta> cargo test --release --lib fuzz -- --ignored --nocapture`.
//! `smoke_synthesized_inputs` (no ignorada) hace una pasada corta y determinista con entradas generadas.
#[allow(unused_imports)]
use crate::vfs::{KindV as _, PathV as _};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::Path;
use std::time::{Duration};
use web_time::{Instant};

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

fn run(path: &Path) {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
    let size = crate::vfs::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    let kind = crate::fsys::kind_of_ext(&ext);
    let _ = crate::meta::collect(path, kind, size);
    match ext.as_str() {
        "glb" | "gltf" | "stl" | "obj" | "ply" | "3mf" => {
            let _ = crate::mesh3d::load(path);
        }
        "sqlite" | "db" => {
            let _ = crate::sqlite::read(path);
        }
        "json" => {
            let t = crate::vfs::fs::read_to_string(path).unwrap_or_default();
            let _ = crate::tree::from_json(&t);
        }
        "xml" => {
            let t = crate::vfs::fs::read_to_string(path).unwrap_or_default();
            let _ = crate::tree::from_xml(&t);
        }
        "csv" => {
            let t = crate::vfs::fs::read_to_string(path).unwrap_or_default();
            let _ = crate::table::csv(&t);
        }
        "md" => {
            let t = crate::vfs::fs::read_to_string(path).unwrap_or_default();
            let _ = crate::rich::layout_for_test(crate::rich::markdown(&t), 600.0);
        }
        "png" | "gif" | "webp" | "tif" | "tiff" | "bmp" | "ico" | "svg" => {
            let _ = crate::thumbs::decode_image(path, 128);
            let _ = crate::thumbs::decode_image(path, 2048);
            if let Ok(b) = crate::vfs::fs::read(path) {
                let _ = crate::comic::decode_fit(b, 900, crate::comic::PAGE_MAX_H);
            }
        }
        "blend" => {
            let _ = crate::imgx::decode(path, "blend", 128);
        }
        "mp3" | "flac" | "ogg" | "wav" | "m4a" | "aac" | "opus" | "mp4" | "mkv" | "webm" | "mov" => {
            // `meta::collect` (arriba) ya lee las cabeceras y las etiquetas
        }
        "ttf" | "otf" => {
            let _ = crate::fontview::specimen(path);
            let _ = crate::fontview::info(path);
        }
        "cbz" => {
            let _ = crate::comic::open(path);
            let _ = crate::comic::thumb(path, 128);
        }
        "epub" => {
            let _ = crate::rich::epub(path);
            let _ = crate::rich::epub_cover(path, 128);
        }
        "zip" | "docx" | "gz" => {
            let _ = crate::archive::list(path, 1000, Duration::from_secs(2));
            if ext == "docx" {
                let _ = crate::docview::thumb(path, 128);
            }
        }
        "pdf" | "rtf" | "odt" => {
            let _ = crate::docview::thumb(path, 128);
        }
        "jpg" | "jpeg" => {
            let _ = crate::exif::summary(path);
            let _ = crate::exif::orientation(path);
            let _ = crate::exif::embedded_jpeg(path);
            let _ = crate::exif::scan_jpeg(path);
            let _ = crate::thumbs::decode_image(path, 128);
        }
        "psd" => {
            let _ = crate::imgx::decode(path, "psd", 128);
        }
        // xls, xlsx, ppt, pptx, doc, ods, odp
        e if crate::docview::is_doc_ext(e) => {
            let _ = crate::docview::thumb(path, 128);
        }
        _ => {}
    }
}

#[test]
#[ignore]
fn fuzz_readers() {
    let Some(dir) = std::env::var_os("FUZZ_DIR") else { return };
    let iters: usize = std::env::var("FUZZ_ITERS").ok().and_then(|v| v.parse().ok()).unwrap_or(150);
    let tmp = crate::vfs::temp_dir().join(format!("fb-fuzz-{}", crate::vfs::pid()));
    crate::vfs::fs::create_dir_all(&tmp).unwrap();
    let mut rng = Rng(0x9E3779B97F4A7C15);
    let mut bad = Vec::new();
    std::panic::set_hook(Box::new(|_| {}));
    let mut files: Vec<_> = crate::vfs::fs::read_dir(&dir).unwrap().flatten().map(|e| e.path()).collect();
    files.sort();
    for f in files {
        let data = crate::vfs::fs::read(&f).unwrap();
        let name = f.file_name().unwrap().to_string_lossy().into_owned();
        let t0 = Instant::now();
        for it in 0..iters {
            let mut d = data.clone();
            let span = d.len().min(if it % 3 == 0 { d.len() } else { 4096 });
            match rng.below(4) {
                0 => d.truncate(rng.below(d.len() + 1)),
                1 => {
                    for _ in 0..1 + rng.below(8) {
                        let i = rng.below(span);
                        d[i] = rng.next() as u8;
                    }
                }
                2 => {
                    // valores extremos (tamaños, desplazamientos)
                    let i = rng.below(span.saturating_sub(4).max(1));
                    let v = [0u8, 0xFF, 0x7F, 0x80][rng.below(4)];
                    for k in 0..4.min(d.len() - i) {
                        d[i + k] = v;
                    }
                }
                _ => {
                    let i = rng.below(span);
                    let n = rng.below(64).min(d.len() - i);
                    d.drain(i..i + n);
                }
            }
            let p = tmp.join(&name);
            crate::vfs::fs::write(&p, &d).unwrap();
            let start = Instant::now();
            let r = catch_unwind(AssertUnwindSafe(|| run(&p)));
            if let Err(e) = r {
                let msg = e.downcast_ref::<String>().cloned().or_else(|| e.downcast_ref::<&str>().map(|s| s.to_string())).unwrap_or_default();
                let keep = tmp.join(format!("crash-{it}-{name}"));
                let _ = crate::vfs::fs::copy(&p, &keep);
                bad.push(format!("{name} iter {it}: PÁNICO {msg} ({})", keep.display()));
                if bad.iter().filter(|b| b.starts_with(&name)).count() >= 3 {
                    break;
                }
            }
            if start.elapsed() > Duration::from_secs(5) {
                let keep = tmp.join(format!("slow-{it}-{name}"));
                let _ = crate::vfs::fs::copy(&p, &keep);
                bad.push(format!("{name} iter {it}: LENTO {:?} ({})", start.elapsed(), keep.display()));
            }
        }
        eprintln!("{name}: {:?}", t0.elapsed());
    }
    let _ = std::panic::take_hook();
    for b in &bad {
        eprintln!("{b}");
    }
    assert!(bad.is_empty(), "{} problemas", bad.len());
}

#[test]
#[ignore]
fn fuzz_one() {
    if let Some(f) = std::env::var_os("FUZZ_FILE") {
        run(Path::new(&f));
    }
}

#[test]
#[ignore]
fn fuzz_dir_once() {
    let Some(dir) = std::env::var_os("FUZZ_ONCE") else { return };
    let mut files: Vec<_> = crate::vfs::fs::read_dir(&dir).unwrap().flatten().map(|e| e.path()).collect();
    files.sort();
    for f in files {
        let t = Instant::now();
        eprintln!("{} ...", f.display());
        run(&f);
        eprintln!("   {:?}", t.elapsed());
    }
}

/// Muta `d` como `fuzz_readers` (truncar, cambiar bytes, valores extremos, borrar trozos).
fn mutate(rng: &mut Rng, d: &mut Vec<u8>) {
    if d.is_empty() {
        return;
    }
    let span = d.len();
    match rng.below(4) {
        0 => d.truncate(rng.below(d.len() + 1)),
        1 => {
            for _ in 0..1 + rng.below(8) {
                let i = rng.below(span);
                d[i] = rng.next() as u8;
            }
        }
        2 => {
            let i = rng.below(span.saturating_sub(4).max(1));
            let v = [0u8, 0xFF, 0x7F, 0x80][rng.below(4)];
            for k in 0..4.min(d.len() - i) {
                d[i + k] = v;
            }
        }
        _ => {
            let i = rng.below(span);
            let n = rng.below(64).min(d.len() - i);
            d.drain(i..i + n);
        }
    }
}

/// Texto aleatorio hecho de trozos de `parts`.
fn text_from(rng: &mut Rng, parts: &[&str], n: usize) -> String {
    let mut s = String::new();
    for _ in 0..n {
        s.push_str(parts[rng.below(parts.len())]);
    }
    s
}

fn put_varint(out: &mut Vec<u8>, v: u64) {
    let mut g = Vec::new();
    let mut x = v;
    loop {
        g.push((x & 0x7F) as u8);
        x >>= 7;
        if x == 0 {
            break;
        }
    }
    for (i, b) in g.iter().rev().enumerate() {
        out.push(if i + 1 < g.len() { b | 0x80 } else { *b });
    }
}

/// Registro SQLite con textos y enteros pequeños.
fn sqlite_record(vals: &[Result<&str, u8>]) -> Vec<u8> {
    let mut types = Vec::new();
    let mut body = Vec::new();
    for v in vals {
        match v {
            Ok(s) => {
                put_varint(&mut types, 2 * s.len() as u64 + 13);
                body.extend_from_slice(s.as_bytes());
            }
            Err(n) => {
                types.push(1);
                body.push(*n);
            }
        }
    }
    let mut r = vec![(types.len() + 1) as u8];
    r.extend(types);
    r.extend(body);
    r
}

/// Página hoja de tabla de 512 bytes con las celdas dadas (`hdr` = 100 en la página 1).
fn sqlite_leaf(page: &mut [u8], hdr: usize, cells: &[Vec<u8>]) {
    page[hdr] = 0x0d;
    page[hdr + 3..hdr + 5].copy_from_slice(&(cells.len() as u16).to_be_bytes());
    let mut end = page.len();
    for (i, rec) in cells.iter().enumerate() {
        let mut c = Vec::new();
        put_varint(&mut c, rec.len() as u64);
        put_varint(&mut c, i as u64 + 1);
        c.extend_from_slice(rec);
        end -= c.len();
        page[end..end + c.len()].copy_from_slice(&c);
        page[hdr + 8 + 2 * i..hdr + 10 + 2 * i].copy_from_slice(&(end as u16).to_be_bytes());
    }
}

fn sample_sqlite() -> Vec<u8> {
    let mut f = vec![0u8; 512 * 2];
    f[..16].copy_from_slice(b"SQLite format 3\0");
    f[16..18].copy_from_slice(&512u16.to_be_bytes());
    f[56..60].copy_from_slice(&1u32.to_be_bytes());
    let sql = "CREATE TABLE t (id INTEGER PRIMARY KEY, a TEXT, b INT)";
    sqlite_leaf(&mut f[..512], 100, &[sqlite_record(&[Ok("table"), Ok("t"), Ok("t"), Err(2), Ok(sql)])]);
    let rows: Vec<Vec<u8>> = (0..5).map(|i| sqlite_record(&[Err(0), Ok("hola"), Err(i)])).collect();
    sqlite_leaf(&mut f[512..], 0, &rows);
    f
}

fn sample_psd() -> Vec<u8> {
    let mut b = b"8BPS".to_vec();
    b.extend_from_slice(&1u16.to_be_bytes());
    b.extend_from_slice(&[0; 6]);
    b.extend_from_slice(&3u16.to_be_bytes());
    b.extend_from_slice(&4u32.to_be_bytes());
    b.extend_from_slice(&4u32.to_be_bytes());
    b.extend_from_slice(&8u16.to_be_bytes());
    b.extend_from_slice(&3u16.to_be_bytes());
    b.extend_from_slice(&[0; 12]);
    b.extend_from_slice(&0u16.to_be_bytes());
    b.extend((0..48).map(|i| i as u8 * 5));
    b
}

fn sample_blend() -> Vec<u8> {
    let mut b = b"BLENDER-v300".to_vec();
    b.extend_from_slice(b"REND");
    b.extend_from_slice(&16u32.to_le_bytes());
    b.extend_from_slice(&[0; 16]);
    b.extend_from_slice(&[7; 16]);
    b.extend_from_slice(b"TEST");
    b.extend_from_slice(&(8 + 4 * 4 * 4u32).to_le_bytes());
    b.extend_from_slice(&[0; 16]);
    b.extend_from_slice(&4u32.to_le_bytes());
    b.extend_from_slice(&4u32.to_le_bytes());
    b.extend((0..64).map(|i| i as u8 * 3));
    b.extend_from_slice(b"ENDB");
    b
}

fn sample_png() -> Vec<u8> {
    let img = image::RgbaImage::from_fn(16, 16, |x, y| image::Rgba([x as u8 * 16, y as u8 * 16, 0, 255]));
    let mut v = Vec::new();
    image::DynamicImage::ImageRgba8(img).write_to(&mut std::io::Cursor::new(&mut v), image::ImageFormat::Png).unwrap();
    v
}

/// JPEG mínimo con un APP1 Exif (IFD0 con Orientation) delante de datos que no son imagen.
fn sample_jpeg_exif() -> Vec<u8> {
    let mut b = vec![0xFF, 0xD8, 0xFF, 0xE1, 0x00, 0x30];
    b.extend_from_slice(b"Exif\0\0II*\0");
    b.extend_from_slice(&8u32.to_le_bytes());
    b.extend_from_slice(&[1, 0, 0x12, 0x01, 3, 0, 1, 0, 0, 0, 6, 0, 0, 0, 0, 0, 0, 0]);
    b.resize(0x30 + 4, 0);
    b.extend_from_slice(&[0xFF, 0xD9]);
    b
}

fn sample_mp4() -> Vec<u8> {
    let mut mvhd = vec![0u8; 4];
    mvhd.extend_from_slice(&[0; 8]);
    mvhd.extend_from_slice(&1000u32.to_be_bytes());
    mvhd.extend_from_slice(&5000u32.to_be_bytes());
    mvhd.resize(100, 0);
    let boxed = |ty: &[u8], body: &[u8]| {
        let mut v = ((body.len() + 8) as u32).to_be_bytes().to_vec();
        v.extend_from_slice(ty);
        v.extend_from_slice(body);
        v
    };
    let mut f = boxed(b"ftyp", b"isom\0\0\0\0isom");
    f.extend(boxed(b"moov", &boxed(b"mvhd", &mvhd)));
    f
}

fn sample_wav() -> Vec<u8> {
    let data: Vec<u8> = (0..800u32).flat_map(|i| ((i as i16).wrapping_mul(97)).to_le_bytes()).collect();
    let mut b = b"RIFF".to_vec();
    b.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
    b.extend_from_slice(b"WAVEfmt ");
    b.extend_from_slice(&16u32.to_le_bytes());
    b.extend_from_slice(&1u16.to_le_bytes());
    b.extend_from_slice(&1u16.to_le_bytes());
    b.extend_from_slice(&8000u32.to_le_bytes());
    b.extend_from_slice(&16000u32.to_le_bytes());
    b.extend_from_slice(&2u16.to_le_bytes());
    b.extend_from_slice(&16u16.to_le_bytes());
    b.extend_from_slice(b"data");
    b.extend_from_slice(&(data.len() as u32).to_le_bytes());
    b.extend(data);
    b
}

/// Pasada corta y determinista: entradas sintetizadas y mutadas con una semilla fija por todos
/// los lectores (y la maquetación del Markdown). Ningún pánico y pocos segundos.
#[test]
fn smoke_synthesized_inputs() {
    let tmp = std::env::temp_dir().join(format!("fb-fuzz-smoke-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    let mut rng = Rng(0x2545F4914F6CDD1D);
    let t0 = Instant::now();
    let mut bad = Vec::new();
    let mut check = |what: &str, it: usize, f: &mut dyn FnMut()| {
        let s = Instant::now();
        if catch_unwind(AssertUnwindSafe(f)).is_err() {
            bad.push(format!("{what} {it}: pánico"));
        }
        if s.elapsed() > Duration::from_secs(3) {
            bad.push(format!("{what} {it}: lento {:?}", s.elapsed()));
        }
    };
    // texto generado
    let md = ["|", " a ", "---", "\n", "# ", "**", "`", "[x](", ")", "![i](", "> ", "- ", "1. ", "```\n", "    ", "~~", "\\", "é", "\t"];
    let json = ["{", "}", "[", "]", ",", ":", "\"k\"", "\"\\u00e9\"", "1e400", "-0", "null", "true", " ", "\"\\ud800\""];
    let xml = ["<a", ">", "</a>", "<b x='1'", "/>", "<!--", "-->", "<![CDATA[", "]]>", "&amp;", "&#x41;", "texto", "\"", "<?x?>"];
    let csv = [",", ";", "\t", "\"", "\"\"", "\n", "\r\n", "a", "é", "1"];
    let obj = ["v 1 2 3\n", "v 0 0 0\n", "f 1 2 3\n", "f -1 -2 -3\n", "f 1/1 2/2 3/3 4/4\n", "vn 0 0 1\n", "# x\n", "v nan inf 1\n"];
    let ply = ["ply\n", "format ascii 1.0\n", "element vertex 3\n", "property float x\n", "property float y\n", "property float z\n", "element face 1\n", "property list uchar int vertex_indices\n", "end_header\n", "0 0 0\n", "1 0 0\n", "3 0 1 2\n", "99999999\n"];
    for it in 0..150 {
        let n = 1 + rng.below(400);
        let s = text_from(&mut rng, &md, n);
        check("markdown", it, &mut || {
            let _ = crate::rich::layout_for_test(crate::rich::markdown(&s), 300.0 + (it as f32) * 10.0);
        });
        let s = text_from(&mut rng, &json, n);
        check("json", it, &mut || {
            let _ = crate::tree::from_json(&s);
        });
        let s = text_from(&mut rng, &xml, n);
        check("xml", it, &mut || {
            let _ = crate::tree::from_xml(&s);
        });
        let s = text_from(&mut rng, &csv, n * 4);
        check("csv", it, &mut || {
            let _ = crate::table::csv(&s);
        });
        for (ext, parts) in [("obj", &obj[..]), ("ply", &ply[..])] {
            let s = if ext == "ply" && it % 2 == 0 { ply[..12].concat() + &text_from(&mut rng, parts, n / 8) } else { text_from(&mut rng, parts, n) };
            let p = tmp.join(format!("m.{ext}"));
            std::fs::write(&p, s).unwrap();
            check(ext, it, &mut || {
                let _ = crate::mesh3d::load(&p);
            });
        }
    }
    // binarios mutados
    let samples: Vec<(&str, Vec<u8>)> = vec![
        ("db.sqlite", sample_sqlite()),
        ("x.psd", sample_psd()),
        ("x.blend", sample_blend()),
        ("x.png", sample_png()),
        ("x.jpg", sample_jpeg_exif()),
        ("x.mp4", sample_mp4()),
        ("x.wav", sample_wav()),
        ("x.stl", { let mut s = vec![0u8; 80]; s.extend_from_slice(&1u32.to_le_bytes()); s.extend([0u8; 50]); s }),
    ];
    // la base sin mutar se lee entera
    let p = tmp.join("db.sqlite");
    std::fs::write(&p, &samples[0].1).unwrap();
    let t = crate::sqlite::read(&p).expect("base de ejemplo");
    assert_eq!(t[0].1.rows.len(), 5);
    let p = tmp.join("x.psd");
    std::fs::write(&p, &samples[1].1).unwrap();
    assert!(crate::imgx::decode(&p, "psd", 64).is_some());
    let p = tmp.join("x.blend");
    std::fs::write(&p, &samples[2].1).unwrap();
    assert!(crate::imgx::decode(&p, "blend", 64).is_some());
    for (name, data) in &samples {
        for it in 0..400 {
            let mut d = data.clone();
            for _ in 0..1 + rng.below(3) {
                mutate(&mut rng, &mut d);
            }
            let p = tmp.join(name);
            std::fs::write(&p, &d).unwrap();
            check(name, it, &mut || run(&p));
        }
    }
    let _ = std::fs::remove_dir_all(&tmp);
    for b in &bad {
        eprintln!("{b}");
    }
    assert!(bad.is_empty(), "{} problemas", bad.len());
    assert!(t0.elapsed() < Duration::from_secs(60), "{:?}", t0.elapsed());
}

#[test]
fn markdown_pathological_lines_are_bounded() {
    let t = Instant::now();
    for s in ["[".repeat(100_000), "![a](".repeat(30_000), format!("{}x{}", "[x](".repeat(5_000), ")".repeat(5_000))] {
        let _ = crate::rich::markdown(&s);
    }
    assert!(t.elapsed() < Duration::from_secs(3), "{:?}", t.elapsed());
}
