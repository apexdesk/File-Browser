//! Prueba de robustez: archivos de ejemplo mutados (truncados, bytes cambiados) pasados por los lectores.
//! Con `panic = "abort"` cualquier pánico cierra la aplicación, así que aquí se buscan.
//! Se ejecuta con `FUZZ_DIR=<carpeta> cargo test --release --lib fuzz -- --ignored --nocapture`.
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
            let _ = crate::rich::markdown(&t);
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

#[test]
fn markdown_pathological_lines_are_bounded() {
    let t = Instant::now();
    for s in ["[".repeat(100_000), "![a](".repeat(30_000), format!("{}x{}", "[x](".repeat(5_000), ")".repeat(5_000))] {
        let _ = crate::rich::markdown(&s);
    }
    assert!(t.elapsed() < Duration::from_secs(3), "{:?}", t.elapsed());
}
