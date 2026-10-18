//! Visor 3D mínimo: lee STL (binario y ASCII) y OBJ y los dibuja con un rasterizador por software
//! (z-buffer, proyección ortogonal, sombreado plano de dos caras). Sin GPU ni dependencias.
use std::io::{BufRead, BufReader, Read};
use std::path::Path;

/// Límite de triángulos admitidos (memoria: 36 bytes por triángulo).
const MAX_TRIS: usize = 3_000_000;
const MAX_BYTES: u64 = 400 << 20;

pub struct Mesh {
    /// 9 floats por triángulo (x, y, z de cada vértice)
    pub tris: Vec<f32>,
    pub center: [f32; 3],
    pub radius: f32,
    pub size: [f32; 3],
    /// materiales y texturas (sólo glTF); el visor por GPU los usa, el software dibuja `tris` en plano
    pub pbr: Option<std::sync::Arc<crate::pbr::Pbr>>,
}

impl Mesh {
    pub fn count(&self) -> usize {
        self.tris.len() / 9
    }

    fn finish(tris: Vec<f32>) -> Result<Mesh, String> {
        if tris.len() < 9 {
            return Err("sin geometría".into());
        }
        let (mut lo, mut hi) = ([f32::MAX; 3], [f32::MIN; 3]);
        for v in tris.chunks_exact(3) {
            for k in 0..3 {
                if !v[k].is_finite() {
                    return Err("coordenadas no válidas".into());
                }
                lo[k] = lo[k].min(v[k]);
                hi[k] = hi[k].max(v[k]);
            }
        }
        let center = [(lo[0] + hi[0]) / 2.0, (lo[1] + hi[1]) / 2.0, (lo[2] + hi[2]) / 2.0];
        let size = [hi[0] - lo[0], hi[1] - lo[1], hi[2] - lo[2]];
        let radius = ((size[0] * size[0] + size[1] * size[1] + size[2] * size[2]).sqrt() / 2.0).max(1e-6);
        Ok(Mesh { tris, center, radius, size, pbr: None })
    }
}

pub fn is_mesh_ext(ext: &str) -> bool {
    matches!(ext.to_ascii_lowercase().as_str(), "stl" | "obj" | "ply" | "glb" | "gltf" | "3mf")
}

pub fn load(path: &Path) -> Result<Mesh, String> {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    let len = std::fs::metadata(path).map_err(|e| e.to_string())?.len();
    if len > MAX_BYTES {
        return Err("archivo demasiado grande".into());
    }
    let f = std::fs::File::open(path).map_err(|e| e.to_string())?;
    match ext.as_str() {
        "stl" => load_stl(f, len),
        "obj" => load_obj(f),
        "ply" => load_ply(f),
        "glb" | "gltf" => load_gltf(path),
        "3mf" => load_3mf(path),
        _ => Err("formato no admitido".into()),
    }
}

fn load_stl(f: std::fs::File, len: u64) -> Result<Mesh, String> {
    let mut r = BufReader::with_capacity(1 << 20, f);
    let mut head = [0u8; 84];
    let n = r.read(&mut head).map_err(|e| e.to_string())?;
    if n == 84 {
        let count = u32::from_le_bytes([head[80], head[81], head[82], head[83]]) as u64;
        if 84 + 50 * count == len {
            if count as usize > MAX_TRIS {
                return Err("demasiados triángulos".into());
            }
            let mut tris = Vec::with_capacity(count as usize * 9);
            let mut rec = [0u8; 50];
            for _ in 0..count {
                r.read_exact(&mut rec).map_err(|e| e.to_string())?;
                for k in 0..9 {
                    let o = 12 + k * 4;
                    tris.push(f32::from_le_bytes([rec[o], rec[o + 1], rec[o + 2], rec[o + 3]]));
                }
            }
            return Mesh::finish(tris);
        }
    }
    // ASCII: cada «vertex x y z»; tres seguidos forman un triángulo
    let mut text = String::from_utf8_lossy(&head[..n]).into_owned();
    text.push('\n');
    let mut tris: Vec<f32> = Vec::new();
    let parse = |line: &str, tris: &mut Vec<f32>| {
        let l = line.trim();
        if let Some(rest) = l.strip_prefix("vertex") {
            let v: Vec<f32> = rest.split_whitespace().filter_map(|x| x.parse().ok()).collect();
            if v.len() >= 3 {
                tris.extend_from_slice(&v[..3]);
            }
        }
    };
    // la cabecera leída puede cortar una línea: se junta con la primera del resto
    let mut rest_lines = r.lines();
    let first_tail = rest_lines.next().and_then(|l| l.ok()).unwrap_or_default();
    text.pop();
    text.push_str(&first_tail);
    for l in text.lines() {
        parse(l, &mut tris);
    }
    for l in rest_lines {
        let Ok(l) = l else { break };
        parse(&l, &mut tris);
        if tris.len() / 9 > MAX_TRIS {
            return Err("demasiados triángulos".into());
        }
    }
    tris.truncate(tris.len() / 9 * 9);
    Mesh::finish(tris)
}

fn load_obj(f: std::fs::File) -> Result<Mesh, String> {
    let r = BufReader::with_capacity(1 << 20, f);
    let mut verts: Vec<[f32; 3]> = Vec::new();
    let mut tris: Vec<f32> = Vec::new();
    for line in r.lines() {
        let Ok(line) = line else { break };
        let l = line.trim_start();
        if let Some(rest) = l.strip_prefix("v ") {
            let v: Vec<f32> = rest.split_whitespace().take(3).filter_map(|x| x.parse().ok()).collect();
            if v.len() == 3 {
                verts.push([v[0], v[1], v[2]]);
            }
        } else if let Some(rest) = l.strip_prefix("f ") {
            let idx: Vec<usize> = rest
                .split_whitespace()
                .take(1000)
                .filter_map(|t| {
                    let i: i64 = t.split('/').next()?.parse().ok()?;
                    let n = verts.len() as i64;
                    let k = if i > 0 { i - 1 } else { n + i };
                    if k >= 0 && k < n { Some(k as usize) } else { None }
                })
                .collect();
            for w in 1..idx.len().saturating_sub(1) {
                for &i in &[idx[0], idx[w], idx[w + 1]] {
                    tris.extend_from_slice(&verts[i]);
                }
            }
            if tris.len() / 9 > MAX_TRIS {
                return Err("demasiados triángulos".into());
            }
        }
    }
    Mesh::finish(tris)
}

/// Dibuja el modelo en un búfer RGBA de `w`×`h`. `full` = todos los triángulos (si no, se aligera
/// mientras se arrastra).
pub fn render(m: &Mesh, yaw: f32, pitch: f32, zoom: f32, w: usize, h: usize, bg: [u8; 3], base: [u8; 3], full: bool) -> Vec<u8> {
    let n_tris = m.count();
    let ss = if n_tris < 60_000 && w * h <= 700_000 { 2 } else { 1 };
    let (rw, rh) = (w * ss, h * ss);
    let (sy, cy) = yaw.sin_cos();
    let (sp, cp) = pitch.sin_cos();
    let scale = 0.9 * (rw.min(rh) as f32) / (2.0 * m.radius) * zoom;
    let (cx, cyy) = (rw as f32 / 2.0, rh as f32 / 2.0);
    // rotación: guiñada alrededor de Z y luego inclinación; (x, arriba, profundidad)
    let rot = |p: [f32; 3]| -> [f32; 3] {
        let (x, y, zz) = (p[0] - m.center[0], p[1] - m.center[1], p[2] - m.center[2]);
        let x1 = x * cy - y * sy;
        let y1 = x * sy + y * cy;
        [x1, zz * cp + y1 * sp, y1 * cp - zz * sp]
    };
    let l1 = norm([-0.35, 0.55, -0.75]);
    let l2 = norm([0.6, -0.2, -0.4]);
    let step = if full || n_tris <= 400_000 { 1 } else { n_tris / 400_000 + 1 };
    let threads = if n_tris >= 20_000 { std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1).min(8) } else { 1 };
    let raster = |first: usize| -> (Vec<f32>, Vec<[u8; 3]>) {
    let mut z = vec![f32::INFINITY; rw * rh];
    let mut col = vec![[bg[0], bg[1], bg[2]]; rw * rh];
    let mut t = first * step;
    while t < n_tris {
        let d = &m.tris[t * 9..t * 9 + 9];
        t += step * threads;
        let a = rot([d[0], d[1], d[2]]);
        let b = rot([d[3], d[4], d[5]]);
        let c = rot([d[6], d[7], d[8]]);
        let mut nrm = cross(sub(b, a), sub(c, a));
        let len = (nrm[0] * nrm[0] + nrm[1] * nrm[1] + nrm[2] * nrm[2]).sqrt();
        if len < 1e-12 {
            continue;
        }
        nrm = [nrm[0] / len, nrm[1] / len, nrm[2] / len];
        let lit = (dot(nrm, l1).abs() * 0.75 + dot(nrm, l2).abs() * 0.25).min(1.0);
        let k = 0.22 + 0.78 * lit;
        let shade = [(base[0] as f32 * k) as u8, (base[1] as f32 * k) as u8, (base[2] as f32 * k) as u8];
        let p = |v: [f32; 3]| (cx + v[0] * scale, cyy - v[1] * scale, v[2]);
        let (x0, y0, z0) = p(a);
        let (x1, y1, z1) = p(b);
        let (x2, y2, z2) = p(c);
        let area = (x1 - x0) * (y2 - y0) - (x2 - x0) * (y1 - y0);
        if area.abs() < 1e-9 {
            continue;
        }
        let minx = x0.min(x1).min(x2).floor().max(0.0) as usize;
        let maxx = (x0.max(x1).max(x2).ceil().min(rw as f32 - 1.0)).max(0.0) as usize;
        let miny = y0.min(y1).min(y2).floor().max(0.0) as usize;
        let maxy = (y0.max(y1).max(y2).ceil().min(rh as f32 - 1.0)).max(0.0) as usize;
        if x0.max(x1).max(x2) < 0.0 || y0.max(y1).max(y2) < 0.0 || x0.min(x1).min(x2) >= rw as f32 || y0.min(y1).min(y2) >= rh as f32 {
            continue;
        }
        let inv = 1.0 / area;
        for py in miny..=maxy {
            let fy = py as f32 + 0.5;
            for px in minx..=maxx {
                let fx = px as f32 + 0.5;
                let w0 = ((x1 - fx) * (y2 - fy) - (x2 - fx) * (y1 - fy)) * inv;
                let w1 = ((x2 - fx) * (y0 - fy) - (x0 - fx) * (y2 - fy)) * inv;
                let w2 = 1.0 - w0 - w1;
                if w0 < 0.0 || w1 < 0.0 || w2 < 0.0 {
                    continue;
                }
                let zz = w0 * z0 + w1 * z1 + w2 * z2;
                let i = py * rw + px;
                if zz < z[i] {
                    z[i] = zz;
                    col[i] = shade;
                }
            }
        }
    }
    (z, col)
    };
    let col = if threads == 1 {
        raster(0).1
    } else {
        let parts: Vec<(Vec<f32>, Vec<[u8; 3]>)> = std::thread::scope(|sc| {
            let hs: Vec<_> = (0..threads).map(|k| sc.spawn(move || raster(k))).collect();
            hs.into_iter().filter_map(|h| h.join().ok()).collect()
        });
        let mut it = parts.into_iter();
        let (mut z, mut col) = it.next().unwrap();
        for (z2, c2) in it {
            for i in 0..z.len() {
                if z2[i] < z[i] {
                    z[i] = z2[i];
                    col[i] = c2[i];
                }
            }
        }
        col
    };
    let mut out = vec![255u8; w * h * 4];
    for y in 0..h {
        for x in 0..w {
            let (mut r, mut g, mut b) = (0u32, 0u32, 0u32);
            for dy in 0..ss {
                for dx in 0..ss {
                    let c = col[(y * ss + dy) * rw + x * ss + dx];
                    r += c[0] as u32;
                    g += c[1] as u32;
                    b += c[2] as u32;
                }
            }
            let n = (ss * ss) as u32;
            let o = (y * w + x) * 4;
            out[o] = (r / n) as u8;
            out[o + 1] = (g / n) as u8;
            out[o + 2] = (b / n) as u8;
        }
    }
    out
}

fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}
fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
fn norm(a: [f32; 3]) -> [f32; 3] {
    let l = dot(a, a).sqrt();
    [a[0] / l, a[1] / l, a[2] / l]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ascii_stl_and_obj() {
        let d = std::env::temp_dir().join(format!("fbmesh{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let stl = d.join("a.stl");
        std::fs::write(&stl, "solid t\nfacet normal 0 0 1\nouter loop\nvertex 0 0 0\nvertex 1 0 0\nvertex 0 1 0\nendloop\nendfacet\nendsolid t\n").unwrap();
        let m = load(&stl).unwrap();
        assert_eq!(m.count(), 1);
        let obj = d.join("a.obj");
        std::fs::write(&obj, "v 0 0 0\nv 1 0 0\nv 1 1 0\nv 0 1 0\nf 1 2 3 4\n").unwrap();
        let m = load(&obj).unwrap();
        assert_eq!(m.count(), 2);
        let img = render(&m, 0.3, 0.4, 1.0, 64, 64, [0, 0, 0], [200, 200, 255], true);
        assert!(img.chunks_exact(4).any(|p| p[2] > 20));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    #[ignore]
    fn bench_render() {
        let m = load(std::path::Path::new(&std::env::var("MESH").unwrap())).unwrap();
        for full in [true, false] {
            let t = std::time::Instant::now();
            for k in 0..5 {
                render(&m, k as f32 * 0.1, 0.5, 1.0, 800, 600, [20, 20, 30], [150, 185, 255], full);
            }
            println!("full={full}: {:?} por imagen", t.elapsed() / 5);
        }
    }

    #[test]
    fn parses_binary_stl() {
        let d = std::env::temp_dir().join(format!("fbmeshb{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let p = d.join("b.stl");
        let mut v = vec![0u8; 80];
        v.extend_from_slice(&1u32.to_le_bytes());
        v.extend_from_slice(&[0u8; 12]);
        for f in [0f32, 0., 0., 1., 0., 0., 0., 1., 0.] {
            v.extend_from_slice(&f.to_le_bytes());
        }
        v.extend_from_slice(&[0u8; 2]);
        std::fs::write(&p, v).unwrap();
        assert_eq!(load(&p).unwrap().count(), 1);
        let _ = std::fs::remove_dir_all(&d);
    }
}

// ---------------------------------------------------------------------------
// PLY (ASCII y binario)
// ---------------------------------------------------------------------------

fn load_ply(f: std::fs::File) -> Result<Mesh, String> {
    let mut r = BufReader::with_capacity(1 << 20, f);
    let mut line = String::new();
    let read_line = |r: &mut BufReader<std::fs::File>, line: &mut String| -> Result<bool, String> {
        line.clear();
        Ok(r.read_line(line).map_err(|e| e.to_string())? > 0)
    };
    read_line(&mut r, &mut line)?;
    if line.trim() != "ply" {
        return Err("no es un PLY".into());
    }
    // (nombre, cuenta, propiedades (nombre, tipo, tipo del contador si es lista, tipo de elemento))
    struct Prop {
        name: String,
        ty: String,
        list: Option<String>,
    }
    let mut format = String::new();
    let mut elems: Vec<(String, usize, Vec<Prop>)> = Vec::new();
    loop {
        if !read_line(&mut r, &mut line)? {
            return Err("cabecera incompleta".into());
        }
        let w: Vec<&str> = line.split_whitespace().collect();
        match w.as_slice() {
            ["format", f, ..] => format = f.to_string(),
            ["element", n, c] => elems.push((n.to_string(), c.parse().map_err(|_| "cuenta no válida")?, Vec::new())),
            ["property", "list", ct, it, n] => {
                if let Some(e) = elems.last_mut() {
                    e.2.push(Prop { name: n.to_string(), ty: it.to_string(), list: Some(ct.to_string()) });
                }
            }
            ["property", t, n] => {
                if let Some(e) = elems.last_mut() {
                    e.2.push(Prop { name: n.to_string(), ty: t.to_string(), list: None });
                }
            }
            ["end_header"] => break,
            _ => {}
        }
    }
    let size = |t: &str| match t {
        "char" | "uchar" | "int8" | "uint8" => 1,
        "short" | "ushort" | "int16" | "uint16" => 2,
        "int" | "uint" | "float" | "int32" | "uint32" | "float32" => 4,
        _ => 8,
    };
    let be = format == "binary_big_endian";
    let ascii = format == "ascii";
    let mut verts: Vec<[f32; 3]> = Vec::new();
    let mut tris: Vec<f32> = Vec::new();
    // lector de valores numéricos como f64
    let mut toks: Vec<String> = Vec::new();
    let mut tok_i = 0usize;
    let mut next_val = |r: &mut BufReader<std::fs::File>, ty: &str| -> Result<f64, String> {
        if ascii {
            while tok_i >= toks.len() {
                let mut l = String::new();
                if r.read_line(&mut l).map_err(|e| e.to_string())? == 0 {
                    return Err("datos incompletos".into());
                }
                toks = l.split_whitespace().map(|s| s.to_string()).collect();
                tok_i = 0;
            }
            tok_i += 1;
            return toks[tok_i - 1].parse().map_err(|_| "número no válido".into());
        }
        let mut b = [0u8; 8];
        let n = size(ty);
        r.read_exact(&mut b[..n]).map_err(|e| e.to_string())?;
        if be {
            b[..n].reverse();
        }
        Ok(match ty {
            "char" | "int8" => b[0] as i8 as f64,
            "uchar" | "uint8" => b[0] as f64,
            "short" | "int16" => i16::from_le_bytes([b[0], b[1]]) as f64,
            "ushort" | "uint16" => u16::from_le_bytes([b[0], b[1]]) as f64,
            "int" | "int32" => i32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f64,
            "uint" | "uint32" => u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f64,
            "float" | "float32" => f32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f64,
            _ => f64::from_le_bytes(b),
        })
    };
    for (name, count, props) in &elems {
        if *count > 20_000_000 {
            return Err("demasiado grande".into());
        }
        for _ in 0..*count {
            let mut xyz = [0f32; 3];
            let mut idx: Vec<usize> = Vec::new();
            for p in props {
                if let Some(ct) = &p.list {
                    let n = next_val(&mut r, ct)? as usize;
                    if n > 1000 {
                        return Err("polígono no válido".into());
                    }
                    let mut v = Vec::with_capacity(n);
                    for _ in 0..n {
                        v.push(next_val(&mut r, &p.ty)? as usize);
                    }
                    if name == "face" && (p.name == "vertex_indices" || p.name == "vertex_index") {
                        idx = v;
                    }
                } else {
                    let v = next_val(&mut r, &p.ty)?;
                    if name == "vertex" {
                        match p.name.as_str() {
                            "x" => xyz[0] = v as f32,
                            "y" => xyz[1] = v as f32,
                            "z" => xyz[2] = v as f32,
                            _ => {}
                        }
                    }
                }
            }
            if name == "vertex" {
                verts.push(xyz);
            } else if name == "face" {
                for w in 1..idx.len().saturating_sub(1) {
                    for &i in &[idx[0], idx[w], idx[w + 1]] {
                        tris.extend_from_slice(verts.get(i).ok_or("índice fuera de rango")?);
                    }
                }
                if tris.len() / 9 > MAX_TRIS {
                    return Err("demasiados triángulos".into());
                }
            }
        }
    }
    Mesh::finish(tris)
}

// ---------------------------------------------------------------------------
// 3MF (zip con XML)
// ---------------------------------------------------------------------------

fn load_3mf(path: &Path) -> Result<Mesh, String> {
    use crate::markup::{self, Tok};
    let f = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut ar = zip::ZipArchive::new(BufReader::new(f)).map_err(|e| e.to_string())?;
    let mut name = None;
    for i in 0..ar.len() {
        if let Ok(z) = ar.by_index_raw(i) {
            if let Ok(n) = z.name() {
                if n.to_ascii_lowercase().ends_with(".model") {
                    name = Some(n.into_owned());
                    break;
                }
            }
        }
    }
    let name = name.ok_or("sin modelo")?;
    let mut text = String::new();
    ar.by_name(&name).map_err(|e| e.to_string())?.take(MAX_BYTES).read_to_string(&mut text).map_err(|e| e.to_string())?;
    let mut verts: Vec<[f32; 3]> = Vec::new();
    let mut tris: Vec<f32> = Vec::new();
    for t in markup::tokens(&text, false) {
        let Tok::Open { name, attrs, .. } = t else { continue };
        let num = |k: &str| markup::attr(&attrs, k).and_then(|v| v.parse::<f32>().ok());
        match markup::local(&name) {
            "mesh" => verts.clear(),
            "vertex" => verts.push([num("x").unwrap_or(0.0), num("y").unwrap_or(0.0), num("z").unwrap_or(0.0)]),
            "triangle" => {
                for k in ["v1", "v2", "v3"] {
                    let i = num(k).ok_or("triángulo no válido")? as usize;
                    tris.extend_from_slice(verts.get(i).ok_or("índice fuera de rango")?);
                }
                if tris.len() / 9 > MAX_TRIS {
                    return Err("demasiados triángulos".into());
                }
            }
            _ => {}
        }
    }
    Mesh::finish(tris)
}

// ---------------------------------------------------------------------------
// glTF / GLB (mallas y transformaciones; sin texturas ni compresión Draco)
// ---------------------------------------------------------------------------

fn b64(s: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let (mut acc, mut bits) = (0u32, 0);
    for c in s.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            _ => continue,
        } as u32;
        acc = (acc << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    out
}

type M4 = [f32; 16];

fn mul(a: &M4, b: &M4) -> M4 {
    // matrices en columnas (como glTF): r = a * b
    let mut r = [0f32; 16];
    for c in 0..4 {
        for row in 0..4 {
            r[c * 4 + row] = (0..4).map(|k| a[k * 4 + row] * b[c * 4 + k]).sum();
        }
    }
    r
}

fn node_matrix(n: &crate::markup::Json) -> M4 {
    let ident: M4 = [1., 0., 0., 0., 0., 1., 0., 0., 0., 0., 1., 0., 0., 0., 0., 1.];
    if let Some(m) = n.get("matrix") {
        let v: Vec<f32> = m.arr().iter().filter_map(|x| x.num()).map(|x| x as f32).collect();
        if v.len() == 16 {
            let mut o = ident;
            o.copy_from_slice(&v);
            return o;
        }
    }
    let f = |k: &str, d: &[f32]| -> Vec<f32> {
        let v: Vec<f32> = n.get(k).map(|a| a.arr().iter().filter_map(|x| x.num()).map(|x| x as f32).collect()).unwrap_or_default();
        if v.len() == d.len() { v } else { d.to_vec() }
    };
    let (t, q, s) = (f("translation", &[0., 0., 0.]), f("rotation", &[0., 0., 0., 1.]), f("scale", &[1., 1., 1.]));
    let (x, y, z, w) = (q[0], q[1], q[2], q[3]);
    [
        (1.0 - 2.0 * (y * y + z * z)) * s[0], (2.0 * (x * y + z * w)) * s[0], (2.0 * (x * z - y * w)) * s[0], 0.0,
        (2.0 * (x * y - z * w)) * s[1], (1.0 - 2.0 * (x * x + z * z)) * s[1], (2.0 * (y * z + x * w)) * s[1], 0.0,
        (2.0 * (x * z + y * w)) * s[2], (2.0 * (y * z - x * w)) * s[2], (1.0 - 2.0 * (x * x + y * y)) * s[2], 0.0,
        t[0], t[1], t[2], 1.0,
    ]
}

struct Gltf {
    json: crate::markup::Json,
    buffers: Vec<Vec<u8>>,
    /// Nodos visitados: un grafo con hijos repetidos explotaría de forma exponencial.
    visits: std::cell::Cell<u32>,
}

impl Gltf {
    /// Elementos de un accessor como `Vec<f64>` por elemento (1 o 3 componentes).
    fn accessor(&self, idx: usize) -> Option<(Vec<f64>, usize)> {
        let a = self.json.get("accessors")?.at(idx)?;
        let view = self.json.get("bufferViews")?.at(a.get("bufferView")?.num()? as usize)?;
        let buf = self.buffers.get(view.get("buffer")?.num()? as usize)?;
        let count = a.get("count")?.num()? as usize;
        let ncomp = match a.get("type")?.str()? {
            "SCALAR" => 1,
            "VEC2" => 2,
            "VEC3" => 3,
            "VEC4" => 4,
            _ => return None,
        };
        let ct = a.get("componentType")?.num()? as u32;
        let csz = match ct {
            5120 | 5121 => 1,
            5122 | 5123 => 2,
            _ => 4,
        };
        let base = (view.get("byteOffset").and_then(|x| x.num()).unwrap_or(0.0) as usize).saturating_add(a.get("byteOffset").and_then(|x| x.num()).unwrap_or(0.0) as usize);
        let stride = view.get("byteStride").and_then(|x| x.num()).map(|x| x as usize).filter(|s| *s > 0).unwrap_or(csz * ncomp);
        if count > 20_000_000 {
            return None;
        }
        let normalized = matches!(a.get("normalized"), Some(crate::markup::Json::Bool(true)));
        let mut out = Vec::with_capacity((count * ncomp).min(buf.len()));
        for i in 0..count {
            for c in 0..ncomp {
                let o = base.checked_add(i.checked_mul(stride)?)?.checked_add(c * csz)?;
                let d = buf.get(o..o.checked_add(csz)?)?;
                out.push(match ct {
                    5120 => d[0] as i8 as f64 / if normalized { 127.0 } else { 1.0 },
                    5121 => d[0] as f64 / if normalized { 255.0 } else { 1.0 },
                    5122 => i16::from_le_bytes([d[0], d[1]]) as f64 / if normalized { 32767.0 } else { 1.0 },
                    5123 => u16::from_le_bytes([d[0], d[1]]) as f64 / if normalized { 65535.0 } else { 1.0 },
                    5125 => u32::from_le_bytes([d[0], d[1], d[2], d[3]]) as f64,
                    5126 => f32::from_le_bytes([d[0], d[1], d[2], d[3]]) as f64,
                    _ => return None,
                });
            }
        }
        Some((out, ncomp))
    }

    fn mesh_into(&self, mesh: usize, m: &M4, tris: &mut Vec<f32>) -> Option<()> {
        for prim in self.json.get("meshes")?.at(mesh)?.get("primitives")?.arr() {
            let mode = prim.get("mode").and_then(|x| x.num()).unwrap_or(4.0) as u32;
            let pos_idx = prim.get("attributes")?.get("POSITION")?.num()? as usize;
            let (pos, _) = self.accessor(pos_idx)?;
            let idx: Vec<usize> = match prim.get("indices").and_then(|x| x.num()) {
                Some(i) => self.accessor(i as usize)?.0.into_iter().map(|v| v as usize).collect(),
                None => (0..pos.len() / 3).collect(),
            };
            let order: Vec<[usize; 3]> = match mode {
                4 => idx.chunks_exact(3).map(|c| [c[0], c[1], c[2]]).collect(),
                5 => (0..idx.len().saturating_sub(2)).map(|i| if i % 2 == 0 { [idx[i], idx[i + 1], idx[i + 2]] } else { [idx[i + 1], idx[i], idx[i + 2]] }).collect(),
                6 => (1..idx.len().saturating_sub(1)).map(|i| [idx[0], idx[i], idx[i + 1]]).collect(),
                _ => continue,
            };
            for t in order {
                for i in t {
                    let b3 = i.checked_mul(3)?;
                    let p = pos.get(b3..b3.checked_add(3)?)?;
                    let (x, y, z) = (p[0] as f32, p[1] as f32, p[2] as f32);
                    let wx = m[0] * x + m[4] * y + m[8] * z + m[12];
                    let wy = m[1] * x + m[5] * y + m[9] * z + m[13];
                    let wz = m[2] * x + m[6] * y + m[10] * z + m[14];
                    // glTF es Y arriba; el visor, Z arriba
                    tris.extend_from_slice(&[wx, -wz, wy]);
                }
                if tris.len() / 9 > MAX_TRIS {
                    return None;
                }
            }
        }
        Some(())
    }

    fn walk(&self, node: usize, parent: &M4, tris: &mut Vec<f32>, depth: u32) -> Option<()> {
        if depth > 64 {
            return Some(());
        }
        self.visits.set(self.visits.get() + 1);
        if self.visits.get() > 100_000 {
            return None;
        }
        let n = self.json.get("nodes")?.at(node)?;
        let m = mul(parent, &node_matrix(n));
        if let Some(mesh) = n.get("mesh").and_then(|x| x.num()) {
            self.mesh_into(mesh as usize, &m, tris)?;
        }
        for c in n.get("children").map(|c| c.arr()).unwrap_or(&[]) {
            if let Some(i) = c.num() {
                self.walk(i as usize, &m, tris, depth + 1)?;
            }
        }
        Some(())
    }
}

fn load_gltf(path: &Path) -> Result<Mesh, String> {
    use crate::markup::parse_json;
    let data = std::fs::read(path).map_err(|e| e.to_string())?;
    let (json_text, glb_bin): (String, Option<Vec<u8>>) = if data.starts_with(b"glTF") {
        let mut json = None;
        let mut bin = None;
        let mut p = 12;
        while p + 8 <= data.len() {
            let len = u32::from_le_bytes([data[p], data[p + 1], data[p + 2], data[p + 3]]) as usize;
            let ty = &data[p + 4..p + 8];
            let body = data.get(p + 8..p + 8 + len).ok_or("trozo incompleto")?;
            if ty == b"JSON" {
                json = Some(String::from_utf8_lossy(body).into_owned());
            } else if ty == b"BIN\0" {
                bin = Some(body.to_vec());
            }
            p += 8 + len.next_multiple_of(4);
        }
        (json.ok_or("sin JSON")?, bin)
    } else {
        (String::from_utf8_lossy(&data).into_owned(), None)
    };
    let json = parse_json(&json_text).ok_or("JSON no válido")?;
    if json.get("extensionsRequired").map(|e| e.arr().iter().any(|x| x.str() == Some("KHR_draco_mesh_compression"))).unwrap_or(false) {
        return Err("compresión Draco no admitida".into());
    }
    let dir = path.parent().unwrap_or(Path::new("."));
    let mut buffers = Vec::new();
    for (i, b) in json.get("buffers").map(|b| b.arr()).unwrap_or(&[]).iter().enumerate() {
        buffers.push(match b.get("uri").and_then(|u| u.str()) {
            Some(u) if u.starts_with("data:") => b64(u.split_once(',').map(|x| x.1).unwrap_or("")),
            Some(u) => {
                let rel = u.replace("%20", " ");
                // sólo dentro de la carpeta del modelo
                if rel.contains("..") || rel.starts_with('/') || rel.contains([':', '\\']) {
                    return Err("ruta no permitida".into());
                }
                // solo archivos normales y de tamaño razonable (una FIFO o /dev/zero colgaría el hilo)
                let bp = dir.join(rel);
                match std::fs::metadata(&bp) {
                    Ok(m) if m.is_file() && m.len() <= 512 << 20 => {}
                    _ => return Err("buffer no válido".into()),
                }
                std::fs::read(bp).map_err(|e| e.to_string())?
            }
            None if i == 0 => glb_bin.clone().ok_or("sin datos binarios")?,
            None => Vec::new(),
        });
    }
    let g = Gltf { json, buffers, visits: std::cell::Cell::new(0) };
    let ident: M4 = [1., 0., 0., 0., 0., 1., 0., 0., 0., 0., 1., 0., 0., 0., 0., 1.];
    let mut tris = Vec::new();
    let scene = g.json.get("scene").and_then(|s| s.num()).unwrap_or(0.0) as usize;
    let roots: Vec<usize> = g.json.get("scenes").and_then(|s| s.at(scene)).and_then(|s| s.get("nodes")).map(|n| n.arr().iter().filter_map(|x| x.num()).map(|x| x as usize).collect()).unwrap_or_default();
    if roots.is_empty() {
        // sin escena: todas las mallas tal cual
        for i in 0..g.json.get("meshes").map(|m| m.arr().len()).unwrap_or(0) {
            g.mesh_into(i, &ident, &mut tris).ok_or("malla no válida")?;
        }
    } else {
        for &r in &roots {
            g.walk(r, &ident, &mut tris, 0).ok_or("malla no válida")?;
        }
    }
    let mut mesh = Mesh::finish(tris)?;
    mesh.pbr = g.load_pbr(&roots, &ident, dir).map(std::sync::Arc::new);
    Ok(mesh)
}

// ---------------------------------------------------------------------------
// glTF: materiales PBR y texturas para el visor por GPU
// ---------------------------------------------------------------------------

const MAX_PBR_VERTS: usize = 5_000_000;
const MAX_TEX_SIDE: u32 = 2048;

fn jnum(j: Option<&crate::markup::Json>, d: f32) -> f32 {
    j.and_then(|x| x.num()).map(|x| x as f32).unwrap_or(d)
}

fn jvec<const N: usize>(j: Option<&crate::markup::Json>, d: [f32; N]) -> [f32; N] {
    let mut o = d;
    if let Some(a) = j {
        let v: Vec<f32> = a.arr().iter().filter_map(|x| x.num()).map(|x| x as f32).collect();
        if v.len() >= N {
            o.copy_from_slice(&v[..N]);
        }
    }
    o
}

fn wrap_code(w: Option<f64>) -> u32 {
    match w.map(|x| x as u32) {
        Some(33071) => 0x812F, // CLAMP_TO_EDGE
        Some(33648) => 0x8370, // MIRRORED_REPEAT
        _ => 0x2901,           // REPEAT
    }
}

impl Gltf {
    /// Bytes de una imagen: URI de datos, archivo junto al modelo o vista de búfer.
    fn image_bytes(&self, img: &crate::markup::Json, dir: &Path) -> Option<Vec<u8>> {
        if let Some(u) = img.get("uri").and_then(|u| u.str()) {
            if u.starts_with("data:") {
                return Some(b64(u.split_once(',')?.1));
            }
            let rel = u.replace("%20", " ");
            if rel.contains("..") || rel.starts_with('/') || rel.starts_with('\\') || rel.contains(':') {
                return None;
            }
            let p = dir.join(rel);
            // nada de tuberías ni dispositivos, y con tope de tamaño
            if !std::fs::metadata(&p).map(|m| m.is_file() && m.len() <= 256 << 20).unwrap_or(false) {
                return None;
            }
            return std::fs::read(p).ok();
        }
        let view = self.json.get("bufferViews")?.at(img.get("bufferView")?.num()? as usize)?;
        let buf = self.buffers.get(view.get("buffer")?.num()? as usize)?;
        let off = view.get("byteOffset").and_then(|x| x.num()).unwrap_or(0.0) as usize;
        let len = view.get("byteLength")?.num()? as usize;
        buf.get(off..off + len).map(|b| b.to_vec())
    }

    fn decode_image(bytes: &[u8]) -> Option<crate::pbr::Image> {
        let d = if bytes.starts_with(&[0xFF, 0xD8]) {
            crate::thumbs::decode_jpeg_from(std::io::Cursor::new(bytes), MAX_TEX_SIDE, 1)?
        } else {
            let img = image::load_from_memory(bytes).ok()?;
            let img = if img.width() > MAX_TEX_SIDE || img.height() > MAX_TEX_SIDE { img.thumbnail(MAX_TEX_SIDE, MAX_TEX_SIDE) } else { img };
            let rgba = img.into_rgba8();
            crate::thumbs::Decoded { w: rgba.width() as usize, h: rgba.height() as usize, rgba: rgba.into_raw() }
        };
        Some(crate::pbr::Image { w: d.w, h: d.h, rgba: d.rgba })
    }

    /// Materiales, texturas y geometría con transformaciones. `None` si no hay nada que mostrar con PBR.
    fn load_pbr(&self, roots: &[usize], ident: &M4, dir: &Path) -> Option<crate::pbr::Pbr> {
        use crate::pbr::*;
        let mut pbr = Pbr::default();
        // texturas del archivo; las imágenes se decodifican sólo si alguna se usa
        let jtex = self.json.get("textures").map(|t| t.arr()).unwrap_or(&[]);
        let jimg = self.json.get("images").map(|t| t.arr()).unwrap_or(&[]);
        let jsmp = self.json.get("samplers").map(|t| t.arr()).unwrap_or(&[]);
        let mut used: Vec<bool> = vec![false; jtex.len()];
        let tref = |t: Option<&crate::markup::Json>, used: &mut Vec<bool>| -> Option<usize> {
            let i = t?.get("index")?.num()? as usize;
            if i < used.len() {
                used[i] = true;
                Some(i)
            } else {
                None
            }
        };
        for m in self.json.get("materials").map(|m| m.arr()).unwrap_or(&[]) {
            let mut mat = Material::default();
            // valores por defecto de glTF cuando el material existe
            mat.metallic = 1.0;
            mat.roughness = 1.0;
            if let Some(p) = m.get("pbrMetallicRoughness") {
                mat.base = jvec(p.get("baseColorFactor"), [1.0; 4]);
                mat.metallic = jnum(p.get("metallicFactor"), 1.0);
                mat.roughness = jnum(p.get("roughnessFactor"), 1.0);
                mat.base_tex = tref(p.get("baseColorTexture"), &mut used);
                mat.mr_tex = tref(p.get("metallicRoughnessTexture"), &mut used);
            }
            if let Some(sg) = m.get("extensions").and_then(|e| e.get("KHR_materials_pbrSpecularGlossiness")) {
                // aproximación: difuso como color base, sin metal y rugosidad = 1 - brillo
                let d = jvec(sg.get("diffuseFactor"), [1.0; 4]);
                mat.base = d;
                mat.metallic = 0.0;
                mat.roughness = 1.0 - jnum(sg.get("glossinessFactor"), 1.0);
                mat.base_tex = tref(sg.get("diffuseTexture"), &mut used);
                mat.mr_tex = None;
            }
            if let Some(n) = m.get("normalTexture") {
                mat.normal_tex = tref(Some(n), &mut used);
                mat.normal_scale = jnum(n.get("scale"), 1.0);
            }
            if let Some(o) = m.get("occlusionTexture") {
                mat.occlusion_tex = tref(Some(o), &mut used);
                mat.occlusion_strength = jnum(o.get("strength"), 1.0);
            }
            mat.emissive_tex = tref(m.get("emissiveTexture"), &mut used);
            mat.emissive = jvec(m.get("emissiveFactor"), [0.0; 3]);
            if let Some(s) = m.get("extensions").and_then(|e| e.get("KHR_materials_emissive_strength")) {
                let k = jnum(s.get("emissiveStrength"), 1.0);
                for c in &mut mat.emissive {
                    *c *= k;
                }
            }
            mat.alpha_mode = match m.get("alphaMode").and_then(|a| a.str()) {
                Some("MASK") => 1,
                Some("BLEND") => 2,
                _ => 0,
            };
            mat.cutoff = jnum(m.get("alphaCutoff"), 0.5);
            pbr.materials.push(mat);
        }
        // imágenes usadas
        let mut img_of: Vec<Option<usize>> = vec![None; jimg.len()];
        for (ti, t) in jtex.iter().enumerate() {
            if !used[ti] {
                pbr.textures.push(Tex { image: 0, wrap_s: 0x2901, wrap_t: 0x2901 });
                continue;
            }
            // EXT_texture_webp / basisu: se prefiere la fuente que sepamos leer
            let src = t.get("extensions").and_then(|e| e.get("EXT_texture_webp")).and_then(|w| w.get("source")).or_else(|| t.get("source")).and_then(|x| x.num()).map(|x| x as usize);
            let smp = t.get("sampler").and_then(|x| x.num()).and_then(|i| jsmp.get(i as usize));
            let (ws, wt) = (wrap_code(smp.and_then(|s| s.get("wrapS")).and_then(|x| x.num())), wrap_code(smp.and_then(|s| s.get("wrapT")).and_then(|x| x.num())));
            let mut image = 0;
            if let Some(si) = src.filter(|i| *i < jimg.len()) {
                if img_of[si].is_none() && pbr.images.len() < 96 {
                    if let Some(im) = self.image_bytes(&jimg[si], dir).and_then(|b| Self::decode_image(&b)) {
                        img_of[si] = Some(pbr.images.len());
                        pbr.images.push(im);
                    }
                }
                if let Some(k) = img_of[si] {
                    image = k;
                    pbr.textures.push(Tex { image, wrap_s: ws, wrap_t: wt });
                    continue;
                }
            }
            // imagen ilegible: sin textura
            pbr.textures.push(Tex { image: usize::MAX, wrap_s: ws, wrap_t: wt });
            let _ = image;
        }
        // las texturas ilegibles se anulan en los materiales
        let bad = |t: &mut Option<usize>, tex: &[Tex]| {
            if let Some(i) = *t {
                if tex.get(i).map(|x| x.image == usize::MAX).unwrap_or(true) {
                    *t = None;
                }
            }
        };
        for m in &mut pbr.materials {
            bad(&mut m.base_tex, &pbr.textures);
            bad(&mut m.mr_tex, &pbr.textures);
            bad(&mut m.normal_tex, &pbr.textures);
            bad(&mut m.occlusion_tex, &pbr.textures);
            bad(&mut m.emissive_tex, &pbr.textures);
        }
        // geometría
        let mut total = 0usize;
        if roots.is_empty() {
            for i in 0..self.json.get("meshes").map(|m| m.arr().len()).unwrap_or(0) {
                self.prims_of(i, ident, &mut pbr, &mut total)?;
            }
        } else {
            for &r in roots {
                self.walk_pbr(r, ident, &mut pbr, &mut total, 0)?;
            }
        }
        if pbr.prims.is_empty() {
            return None;
        }
        Some(pbr)
    }

    fn walk_pbr(&self, node: usize, parent: &M4, pbr: &mut crate::pbr::Pbr, total: &mut usize, depth: u32) -> Option<()> {
        if depth > 64 {
            return Some(());
        }
        self.visits.set(self.visits.get() + 1);
        if self.visits.get() > 100_000 {
            return None;
        }
        let n = self.json.get("nodes")?.at(node)?;
        let m = mul(parent, &node_matrix(n));
        if let Some(mesh) = n.get("mesh").and_then(|x| x.num()) {
            self.prims_of(mesh as usize, &m, pbr, total)?;
        }
        for c in n.get("children").map(|c| c.arr()).unwrap_or(&[]) {
            if let Some(i) = c.num() {
                self.walk_pbr(i as usize, &m, pbr, total, depth + 1)?;
            }
        }
        Some(())
    }

    fn prims_of(&self, mesh: usize, m: &M4, pbr: &mut crate::pbr::Pbr, total: &mut usize) -> Option<()> {
        use crate::pbr::*;
        let det = m[0] * (m[5] * m[10] - m[6] * m[9]) - m[4] * (m[1] * m[10] - m[2] * m[9]) + m[8] * (m[1] * m[6] - m[2] * m[5]);
        let mirrored = det < 0.0;
        for prim in self.json.get("meshes")?.at(mesh)?.get("primitives")?.arr() {
            let mode = prim.get("mode").and_then(|x| x.num()).unwrap_or(4.0) as u32;
            let attrs = prim.get("attributes")?;
            let (pos, _) = self.accessor(attrs.get("POSITION")?.num()? as usize)?;
            let nv = pos.len() / 3;
            *total += nv;
            if *total > MAX_PBR_VERTS {
                return None;
            }
            let idx: Vec<usize> = match prim.get("indices").and_then(|x| x.num()) {
                Some(i) => self.accessor(i as usize)?.0.into_iter().map(|v| v as usize).collect(),
                None => (0..nv).collect(),
            };
            let tris: Vec<[usize; 3]> = match mode {
                4 => idx.chunks_exact(3).map(|c| [c[0], c[1], c[2]]).collect(),
                5 => (0..idx.len().saturating_sub(2)).map(|i| if i % 2 == 0 { [idx[i], idx[i + 1], idx[i + 2]] } else { [idx[i + 1], idx[i], idx[i + 2]] }).collect(),
                6 => (1..idx.len().saturating_sub(1)).map(|i| [idx[0], idx[i], idx[i + 1]]).collect(),
                _ => continue,
            };
            let nrm = attrs.get("NORMAL").and_then(|a| a.num()).and_then(|i| self.accessor(i as usize)).map(|x| x.0).filter(|n| n.len() == nv * 3);
            let uv = attrs.get("TEXCOORD_0").and_then(|a| a.num()).and_then(|i| self.accessor(i as usize)).filter(|x| x.1 == 2 && x.0.len() == nv * 2).map(|x| x.0);
            let col = attrs.get("COLOR_0").and_then(|a| a.num()).and_then(|i| self.accessor(i as usize)).filter(|x| (x.1 == 3 || x.1 == 4) && x.0.len() == nv * x.1);
            let mut verts = vec![0f32; nv * STRIDE];
            // a espacio del visor: Y arriba -> Z arriba
            let zup = |x: f32, y: f32, z: f32| [x, -z, y];
            for i in 0..nv {
                let (x, y, z) = (pos[i * 3] as f32, pos[i * 3 + 1] as f32, pos[i * 3 + 2] as f32);
                let w = [m[0] * x + m[4] * y + m[8] * z + m[12], m[1] * x + m[5] * y + m[9] * z + m[13], m[2] * x + m[6] * y + m[10] * z + m[14]];
                let p = zup(w[0], w[1], w[2]);
                let v = &mut verts[i * STRIDE..(i + 1) * STRIDE];
                v[0..3].copy_from_slice(&p);
                if let Some(n) = &nrm {
                    let (nx, ny, nz) = (n[i * 3] as f32, n[i * 3 + 1] as f32, n[i * 3 + 2] as f32);
                    let w = [m[0] * nx + m[4] * ny + m[8] * nz, m[1] * nx + m[5] * ny + m[9] * nz, m[2] * nx + m[6] * ny + m[10] * nz];
                    let q = zup(w[0], w[1], w[2]);
                    let l = (q[0] * q[0] + q[1] * q[1] + q[2] * q[2]).sqrt().max(1e-20);
                    v[3..6].copy_from_slice(&[q[0] / l, q[1] / l, q[2] / l]);
                }
                if let Some(t) = &uv {
                    v[6] = t[i * 2] as f32;
                    v[7] = t[i * 2 + 1] as f32;
                }
                match &col {
                    Some((c, k)) => {
                        for j in 0..*k {
                            v[8 + j] = c[i * k + j] as f32;
                        }
                        if *k == 3 {
                            v[11] = 1.0;
                        }
                    }
                    None => v[8..12].copy_from_slice(&[1.0; 4]),
                }
            }
            let mut indices: Vec<u32> = Vec::with_capacity(tris.len() * 3);
            for t in &tris {
                if t.iter().any(|&i| i >= nv) {
                    continue;
                }
                let o = if mirrored { [t[0], t[2], t[1]] } else { *t };
                indices.extend(o.iter().map(|&i| i as u32));
            }
            if nrm.is_none() {
                // normales suaves a partir de las caras
                let mut acc = vec![[0f32; 3]; nv];
                for t in indices.chunks_exact(3) {
                    let p = |k: u32| [verts[k as usize * STRIDE], verts[k as usize * STRIDE + 1], verts[k as usize * STRIDE + 2]];
                    let (a, b, c) = (p(t[0]), p(t[1]), p(t[2]));
                    let n = cross(sub(b, a), sub(c, a));
                    for &k in t {
                        for j in 0..3 {
                            acc[k as usize][j] += n[j];
                        }
                    }
                }
                for (i, a) in acc.iter().enumerate() {
                    let l = (a[0] * a[0] + a[1] * a[1] + a[2] * a[2]).sqrt().max(1e-20);
                    verts[i * STRIDE + 3..i * STRIDE + 6].copy_from_slice(&[a[0] / l, a[1] / l, a[2] / l]);
                }
            }
            let nmat = pbr.materials.len();
            let material = prim.get("material").and_then(|x| x.num()).map(|x| x as usize).filter(|i| *i < nmat).unwrap_or_else(|| {
                pbr.materials.push(Material::default());
                pbr.materials.len() - 1
            });
            pbr.prims.push(Prim { verts, indices, material });
        }
        Some(())
    }
}

