//! Lector mínimo de bases SQLite (sólo lectura): lista las tablas y las primeras filas de cada una.
//! No ejecuta SQL; recorre directamente los árboles B de las tablas con clave de fila.
#[allow(unused_imports)]
use crate::vfs::{KindV as _, PathV as _};
use crate::table::Table;
use crate::vfs::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

const MAX_ROWS: usize = 300;
const MAX_TABLES: usize = 400;
const MAX_COLS: usize = 120;
/// Lectura total de una base (todas las tablas): celdas que se repiten o cadenas de
/// desbordamiento circulares no pueden hacer leer gigas.
const MAX_READ_BYTES: u64 = 128 << 20;
/// Páginas visitadas en total (todas las tablas).
const MAX_VISITS: u32 = 100_000;
/// Bytes de registros que se guardan en un recorrido.
const MAX_KEEP_BYTES: usize = 32 << 20;

struct Db {
    f: File,
    visits: u32,
    page: usize,
    usable: usize,
    pages: u64,
    enc: u32,
    /// bytes leídos hasta ahora
    read: u64,
    /// páginas visitadas en todos los recorridos
    visits_all: u32,
    /// bytes de registros guardados en el recorrido actual
    kept: usize,
}

#[derive(Clone)]
enum Val {
    Null,
    Int(i64),
    Real(f64),
    Text(String),
    Blob(usize),
}

impl Val {
    fn show(&self) -> String {
        match self {
            Val::Null => String::new(),
            Val::Int(i) => i.to_string(),
            Val::Real(r) => {
                let s = format!("{r:.6}");
                s.trim_end_matches('0').trim_end_matches('.').to_string()
            }
            Val::Text(s) => {
                let s: String = s.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
                if s.chars().count() > 240 { s.chars().take(240).collect::<String>() + "…" } else { s }
            }
            Val::Blob(n) => format!("<blob {n} B>"),
        }
    }
}

fn varint(b: &[u8]) -> Option<(u64, usize)> {
    let mut v = 0u64;
    for i in 0..9 {
        let c = *b.get(i)?;
        if i == 8 {
            return Some(((v << 8) | c as u64, 9));
        }
        v = (v << 7) | (c & 0x7F) as u64;
        if c & 0x80 == 0 {
            return Some((v, i + 1));
        }
    }
    None
}

fn be(b: &[u8]) -> u64 {
    b.iter().fold(0u64, |a, x| (a << 8) | *x as u64)
}

impl Db {
    fn open(path: &Path) -> Option<Db> {
        let mut f = crate::thumbs::open_regular(path)?;
        let mut h = [0u8; 100];
        f.read_exact(&mut h).ok()?;
        if &h[..16] != b"SQLite format 3\0" {
            return None;
        }
        let ps = u16::from_be_bytes([h[16], h[17]]) as usize;
        let page = if ps == 1 { 65536 } else { ps };
        if !page.is_power_of_two() || page < 512 {
            return None;
        }
        let usable = page - h[20] as usize;
        // la especificación exige al menos 480: con menos las restas de las celdas desbordarían
        if usable < 480 {
            return None;
        }
        let len = f.metadata().ok()?.len();
        Some(Db { f, visits: 0, page, usable, pages: len / page as u64, enc: u32::from_be_bytes([h[56], h[57], h[58], h[59]]), read: 0, visits_all: 0, kept: 0 })
    }

    /// ¿Se ha agotado el presupuesto de lectura?
    fn spent(&self) -> bool {
        self.read.saturating_add(self.page as u64) > MAX_READ_BYTES
    }

    fn page(&mut self, n: u64) -> Option<Vec<u8>> {
        if n == 0 || n > self.pages {
            return None;
        }
        // presupuesto de lectura agotado: se deja de leer (lo ya leído se muestra)
        self.read = self.read.checked_add(self.page as u64).filter(|r| *r <= MAX_READ_BYTES)?;
        self.f.seek(SeekFrom::Start((n - 1).checked_mul(self.page as u64)?)).ok()?;
        let mut v = vec![0u8; self.page];
        self.f.read_exact(&mut v).ok()?;
        Some(v)
    }

    /// Carga útil completa de una celda (siguiendo las páginas de desbordamiento).
    fn payload(&mut self, page: &[u8], at: usize, total: usize) -> Option<Vec<u8>> {
        let u = self.usable;
        let x = u - 35;
        if total <= x {
            return page.get(at..at + total).map(|s| s.to_vec());
        }
        let m = ((u - 12) * 32 / 255) - 23;
        let k = m + (total - m) % (u - 4);
        let local = if k <= x { k } else { m };
        let mut out = page.get(at..at + local)?.to_vec();
        let mut next = be(page.get(at + local..at + local + 4)?);
        // una cadena de desbordamiento no puede tener más páginas que las necesarias (corta ciclos)
        let mut hops = total / (u - 4).max(1) + 2;
        while out.len() < total && next != 0 {
            hops = hops.checked_sub(1)?;
            let p = self.page(next)?;
            let take = (total - out.len()).min(u - 4);
            out.extend_from_slice(p.get(4..4 + take)?);
            next = be(&p[..4]);
        }
        (out.len() == total).then_some(out)
    }

    fn record(&self, b: &[u8]) -> Option<Vec<Val>> {
        let (hl, n) = varint(b)?;
        let hl = usize::try_from(hl).ok().filter(|h| *h <= b.len())?;
        let mut types = Vec::new();
        let mut p = n;
        while p < hl && types.len() < 2000 {
            let (t, k) = varint(b.get(p..)?)?;
            types.push(t);
            p += k;
        }
        let mut q = hl;
        let mut vals = Vec::with_capacity(types.len());
        for t in types {
            let size = match t {
                0 | 8 | 9 => 0,
                1 => 1,
                2 => 2,
                3 => 3,
                4 => 4,
                5 => 6,
                6 | 7 => 8,
                n if n >= 12 => usize::try_from((n - 12) / 2).ok()?,
                _ => return None,
            };
            let end = q.checked_add(size)?;
            let d = b.get(q..end)?;
            q = end;
            vals.push(match t {
                0 => Val::Null,
                8 => Val::Int(0),
                9 => Val::Int(1),
                1..=6 => {
                    let v = be(d);
                    let bits = size * 8;
                    Val::Int(if bits < 64 && v >> (bits - 1) & 1 == 1 { (v as i64) - (1i64 << bits) } else { v as i64 })
                }
                7 => Val::Real(f64::from_bits(be(d))),
                n if n % 2 == 0 => Val::Blob(size),
                _ => Val::Text(match self.enc {
                    2 => String::from_utf16_lossy(&d.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect::<Vec<_>>()),
                    3 => String::from_utf16_lossy(&d.chunks_exact(2).map(|c| u16::from_be_bytes([c[0], c[1]])).collect::<Vec<_>>()),
                    _ => String::from_utf8_lossy(d).into_owned(),
                }),
            });
        }
        Some(vals)
    }

    /// Recorre un árbol B de tabla en orden de clave y entrega (rowid, valores).
    fn walk(&mut self, root: u64, limit: usize, out: &mut Vec<(i64, Vec<Val>)>, depth: u32) -> Option<()> {
        if depth == 0 {
            self.visits = 0;
            self.kept = 0;
        }
        // un archivo con páginas que se apuntan entre sí no puede hacernos dar vueltas sin fin
        self.visits += 1;
        self.visits_all = self.visits_all.saturating_add(1);
        if depth > 12 || out.len() >= limit || self.visits > 20_000 || self.visits_all > MAX_VISITS || self.kept >= MAX_KEEP_BYTES {
            return Some(());
        }
        let page = match self.page(root) {
            Some(p) => p,
            None if self.spent() => return Some(()),
            None => return None,
        };
        let hdr = if root == 1 { 100 } else { 0 };
        let kind = *page.get(hdr)?;
        let n = u16::from_be_bytes([*page.get(hdr + 3)?, *page.get(hdr + 4)?]) as usize;
        match kind {
            0x0d => {
                for i in 0..n {
                    if out.len() >= limit || self.kept >= MAX_KEEP_BYTES {
                        break;
                    }
                    let cp = u16::from_be_bytes([*page.get(hdr + 8 + 2 * i)?, *page.get(hdr + 9 + 2 * i)?]) as usize;
                    let (size, a) = varint(page.get(cp..)?)?;
                    let (rowid, b) = varint(page.get(cp + a..)?)?;
                    if size > 1 << 20 {
                        continue;
                    }
                    let Some(data) = self.payload(&page, cp + a + b, size as usize) else {
                        // sin presupuesto de lectura: se queda lo que hay
                        if self.spent() {
                            break;
                        }
                        return None;
                    };
                    if let Some(vals) = self.record(&data) {
                        self.kept += data.len();
                        out.push((rowid as i64, vals));
                    }
                }
            }
            0x05 => {
                for i in 0..n {
                    let cp = u16::from_be_bytes([*page.get(hdr + 12 + 2 * i)?, *page.get(hdr + 13 + 2 * i)?]) as usize;
                    let child = be(page.get(cp..cp + 4)?);
                    self.walk(child, limit, out, depth + 1)?;
                    if out.len() >= limit {
                        return Some(());
                    }
                }
                let right = be(page.get(hdr + 8..hdr + 12)?);
                self.walk(right, limit, out, depth + 1)?;
            }
            _ => {}
        }
        Some(())
    }
}

/// Columnas de un `CREATE TABLE` y la posición de la clave de fila (INTEGER PRIMARY KEY).
fn columns(sql: &str) -> (Vec<String>, Option<usize>) {
    let Some(open) = sql.find('(') else { return (Vec::new(), None) };
    let body = &sql[open + 1..];
    let (mut depth, mut seg, mut segs) = (0i32, String::new(), Vec::new());
    for c in body.chars() {
        match c {
            '(' => {
                depth += 1;
                seg.push(c);
            }
            ')' if depth == 0 => break,
            ')' => {
                depth -= 1;
                seg.push(c);
            }
            ',' if depth == 0 => segs.push(std::mem::take(&mut seg)),
            _ => seg.push(c),
        }
    }
    segs.push(seg);
    let mut cols = Vec::new();
    let mut rowid = None;
    for s in segs {
        let s = s.trim();
        let up = s.to_uppercase();
        if ["PRIMARY", "UNIQUE", "FOREIGN", "CHECK", "CONSTRAINT"].iter().any(|k| up.starts_with(k)) {
            continue;
        }
        let name = if let Some(r) = s.strip_prefix(['"', '`', '[']) {
            r.split(['"', '`', ']']).next().unwrap_or("").to_string()
        } else {
            s.split_whitespace().next().unwrap_or("").to_string()
        };
        if up.contains("INTEGER") && up.contains("PRIMARY KEY") {
            rowid = Some(cols.len());
        }
        cols.push(name);
    }
    (cols, rowid)
}

/// Tablas de la base: (nombre, contenido de las primeras filas).
pub fn read(path: &Path) -> Result<Vec<(String, Table)>, String> {
    let fail = || crate::i18n::t("No se pudo mostrar el documento").to_string();
    let mut db = Db::open(path).ok_or_else(fail)?;
    let mut master = Vec::new();
    db.walk(1, 5000, &mut master, 0).ok_or_else(fail)?;
    let mut out = Vec::new();
    for (_, v) in master {
        if out.len() >= MAX_TABLES {
            break;
        }
        let (Some(Val::Text(ty)), Some(Val::Text(name)), Some(Val::Int(root))) = (v.first(), v.get(1), v.get(3)) else { continue };
        if ty != "table" || name.starts_with("sqlite_") || *root <= 0 {
            continue;
        }
        let sql = match v.get(4) {
            Some(Val::Text(s)) => s.clone(),
            _ => String::new(),
        };
        if sql.to_uppercase().contains("WITHOUT ROWID") || sql.to_uppercase().starts_with("CREATE VIRTUAL") {
            out.push((name.clone(), Table { head: vec![crate::i18n::t("Sin vista previa disponible").into()], ..Default::default() }));
            continue;
        }
        let (mut cols, rowid_col) = columns(&sql);
        let mut rows = Vec::new();
        db.walk(*root as u64, MAX_ROWS + 1, &mut rows, 0);
        let truncated = rows.len() > MAX_ROWS || db.kept >= MAX_KEEP_BYTES;
        rows.truncate(MAX_ROWS);
        let width = rows.iter().map(|r| r.1.len()).max().unwrap_or(0).max(cols.len()).min(MAX_COLS);
        while cols.len() < width {
            cols.push(format!("c{}", cols.len() + 1));
        }
        cols.truncate(width);
        let body = rows
            .into_iter()
            .map(|(id, mut vals)| {
                if let Some(c) = rowid_col {
                    if matches!(vals.get(c), Some(Val::Null)) {
                        vals[c] = Val::Int(id);
                    }
                }
                vals.truncate(width);
                let mut row: Vec<String> = vals.iter().map(|v| v.show()).collect();
                row.resize(width, String::new());
                row
            })
            .collect();
        out.push((name.clone(), Table { head: cols, rows: body, truncated }));
    }
    if out.is_empty() {
        return Err(crate::i18n::t("Sin datos").into());
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn varints_and_columns() {
        assert_eq!(varint(&[0x81, 0x00]), Some((128, 2)));
        assert_eq!(varint(&[0x05]), Some((5, 1)));
        let (c, r) = columns("CREATE TABLE t (id INTEGER PRIMARY KEY, \"nombre\" TEXT, x NUMERIC(10,2), PRIMARY KEY(id))");
        assert_eq!(c, ["id", "nombre", "x"]);
        assert_eq!(r, Some(0));
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

    /// Página 1 con 5 000 celdas que apuntan a la misma celda de 1 MB cuyo desbordamiento
    /// es una página que se apunta a sí misma.
    #[test]
    fn aliased_cells_and_overflow_cycle_are_budgeted() {
        let ps = 65536usize;
        let mut f = vec![0u8; ps * 3];
        f[..16].copy_from_slice(b"SQLite format 3\0");
        f[16..18].copy_from_slice(&1u16.to_be_bytes()); // 1 = 65 536
        f[56..60].copy_from_slice(&1u32.to_be_bytes());
        let n = 5000u16;
        f[100] = 0x0d;
        f[103..105].copy_from_slice(&n.to_be_bytes());
        let cp = 20_000usize;
        for i in 0..n as usize {
            f[108 + 2 * i..110 + 2 * i].copy_from_slice(&(cp as u16).to_be_bytes());
        }
        let total = 1u64 << 20;
        let mut cell = Vec::new();
        put_varint(&mut cell, total);
        put_varint(&mut cell, 1);
        // registro: cabecera de 5 bytes con un texto que ocupa el resto
        let mut rec = Vec::new();
        rec.push(5);
        put_varint(&mut rec, 2 * (total - 5) + 13);
        assert_eq!(rec.len(), 5);
        rec.resize(8199, b'a');
        cell.extend_from_slice(&rec);
        cell.extend_from_slice(&3u32.to_be_bytes());
        f[cp..cp + cell.len()].copy_from_slice(&cell);
        // la página 3 de desbordamiento apunta a sí misma
        f[2 * ps..2 * ps + 4].copy_from_slice(&3u32.to_be_bytes());
        for b in &mut f[2 * ps + 4..] {
            *b = b'b';
        }
        let dir = std::env::temp_dir().join(format!("fb-sqlite-budget-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let p = dir.join("x.sqlite");
        std::fs::write(&p, &f).unwrap();
        let t = std::time::Instant::now();
        let mut db = Db::open(&p).unwrap();
        let mut out = Vec::new();
        assert!(db.walk(1, 5000, &mut out, 0).is_some());
        assert!(db.read <= MAX_READ_BYTES);
        assert!(!out.is_empty() && out.len() <= MAX_KEEP_BYTES / (1 << 20) + 1, "{}", out.len());
        // la lectura completa termina y no encuentra tablas
        assert!(read(&p).is_err());
        assert!(t.elapsed() < std::time::Duration::from_secs(20), "{:?}", t.elapsed());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
