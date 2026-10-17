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
        Ok(Mesh { tris, center, radius, size })
    }
}

pub fn is_mesh_ext(ext: &str) -> bool {
    matches!(ext.to_ascii_lowercase().as_str(), "stl" | "obj")
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
    let mut parse = |line: &str, tris: &mut Vec<f32>| {
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
    let mut z = vec![f32::INFINITY; rw * rh];
    let mut col = vec![[bg[0], bg[1], bg[2]]; rw * rh];
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
    let mut t = 0;
    while t < n_tris {
        let d = &m.tris[t * 9..t * 9 + 9];
        t += step;
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
