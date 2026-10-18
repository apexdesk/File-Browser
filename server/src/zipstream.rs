//! ZIP en streaming sin dependencias: método «stored» (sin comprimir), ZIP64 siempre y
//! descriptor de datos, de modo que se puede escribir en un socket sin conocer los tamaños.

use std::io::{self, Read, Write};

fn crc_table() -> &'static [u32; 256] {
    static T: std::sync::OnceLock<[u32; 256]> = std::sync::OnceLock::new();
    T.get_or_init(|| {
        let mut t = [0u32; 256];
        for (i, e) in t.iter_mut().enumerate() {
            let mut c = i as u32;
            for _ in 0..8 {
                c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
            }
            *e = c;
        }
        t
    })
}

fn crc_update(mut c: u32, data: &[u8]) -> u32 {
    let t = crc_table();
    for &b in data {
        c = t[((c ^ u32::from(b)) & 0xFF) as usize] ^ (c >> 8);
    }
    c
}

struct Central {
    name: String,
    crc: u32,
    size: u64,
    off: u64,
    time: u16,
    date: u16,
    dir: bool,
}

pub struct Zip<W: Write> {
    w: W,
    pos: u64,
    entries: Vec<Central>,
}

impl<W: Write> Zip<W> {
    pub fn new(w: W) -> Self {
        Zip { w, pos: 0, entries: Vec::new() }
    }

    pub fn count(&self) -> usize {
        self.entries.len()
    }

    fn put(&mut self, b: &[u8]) -> io::Result<()> {
        self.w.write_all(b)?;
        self.pos = self.pos.checked_add(b.len() as u64).ok_or_else(|| io::Error::other("desbordamiento"))?;
        Ok(())
    }

    fn header(&mut self, name: &str, time: u16, date: u16, dir: bool) -> io::Result<()> {
        let nb = name.as_bytes();
        let nlen = u16::try_from(nb.len()).map_err(|_| io::Error::other("nombre largo"))?;
        let mut h = Vec::with_capacity(30 + nb.len() + 20);
        h.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
        h.extend_from_slice(&(if dir { 20u16 } else { 45 }).to_le_bytes());
        h.extend_from_slice(&(if dir { 0x0800u16 } else { 0x0808 }).to_le_bytes());
        h.extend_from_slice(&0u16.to_le_bytes()); // stored
        h.extend_from_slice(&time.to_le_bytes());
        h.extend_from_slice(&date.to_le_bytes());
        h.extend_from_slice(&0u32.to_le_bytes()); // crc (va en el descriptor)
        let sz = if dir { 0u32 } else { 0xFFFF_FFFF };
        h.extend_from_slice(&sz.to_le_bytes());
        h.extend_from_slice(&sz.to_le_bytes());
        h.extend_from_slice(&nlen.to_le_bytes());
        h.extend_from_slice(&(if dir { 0u16 } else { 20 }).to_le_bytes());
        h.extend_from_slice(nb);
        if !dir {
            h.extend_from_slice(&1u16.to_le_bytes());
            h.extend_from_slice(&16u16.to_le_bytes());
            h.extend_from_slice(&[0u8; 16]);
        }
        self.put(&h)
    }

    pub fn add_dir(&mut self, name: &str, mtime: i64) -> io::Result<()> {
        let (time, date) = crate::timefmt::dos(mtime);
        let name = format!("{}/", name.trim_end_matches('/'));
        let off = self.pos;
        self.header(&name, time, date, true)?;
        self.entries.push(Central { name, crc: 0, size: 0, off, time, date, dir: true });
        Ok(())
    }

    /// Añade un fichero leyendo como mucho `limit` bytes de `r`.
    pub fn add_file(&mut self, name: &str, mtime: i64, r: &mut impl Read, limit: u64) -> io::Result<()> {
        let (time, date) = crate::timefmt::dos(mtime);
        let off = self.pos;
        self.header(name, time, date, false)?;
        let mut crc = 0xFFFF_FFFFu32;
        let mut size = 0u64;
        let mut buf = vec![0u8; 64 * 1024];
        while size < limit {
            let want = usize::try_from((limit - size).min(buf.len() as u64)).unwrap_or(buf.len());
            let n = match r.read(&mut buf[..want]) {
                Ok(0) => break,
                Ok(n) => n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            };
            crc = crc_update(crc, &buf[..n]);
            self.put(&buf[..n])?;
            size += n as u64;
        }
        let crc = !crc;
        let mut d = Vec::with_capacity(24);
        d.extend_from_slice(&0x0807_4b50u32.to_le_bytes());
        d.extend_from_slice(&crc.to_le_bytes());
        d.extend_from_slice(&size.to_le_bytes());
        d.extend_from_slice(&size.to_le_bytes());
        self.put(&d)?;
        self.entries.push(Central { name: name.to_string(), crc, size, off, time, date, dir: false });
        Ok(())
    }

    pub fn finish(mut self) -> io::Result<()> {
        let cd_start = self.pos;
        let entries = std::mem::take(&mut self.entries);
        for e in &entries {
            let nb = e.name.as_bytes();
            let mut h = Vec::with_capacity(46 + nb.len() + 28);
            h.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
            h.extend_from_slice(&0x032Du16.to_le_bytes()); // creado en Unix, v4.5
            h.extend_from_slice(&45u16.to_le_bytes());
            h.extend_from_slice(&(if e.dir { 0x0800u16 } else { 0x0808 }).to_le_bytes());
            h.extend_from_slice(&0u16.to_le_bytes());
            h.extend_from_slice(&e.time.to_le_bytes());
            h.extend_from_slice(&e.date.to_le_bytes());
            h.extend_from_slice(&e.crc.to_le_bytes());
            h.extend_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
            h.extend_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
            h.extend_from_slice(&(nb.len() as u16).to_le_bytes());
            h.extend_from_slice(&28u16.to_le_bytes());
            h.extend_from_slice(&0u16.to_le_bytes()); // comentario
            h.extend_from_slice(&0u16.to_le_bytes()); // disco
            h.extend_from_slice(&0u16.to_le_bytes()); // atributos internos
            let mode: u32 = if e.dir { 0o040_755 } else { 0o100_644 };
            h.extend_from_slice(&((mode << 16) | u32::from(e.dir) * 0x10).to_le_bytes());
            h.extend_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
            h.extend_from_slice(nb);
            h.extend_from_slice(&1u16.to_le_bytes());
            h.extend_from_slice(&24u16.to_le_bytes());
            h.extend_from_slice(&e.size.to_le_bytes()); // sin comprimir
            h.extend_from_slice(&e.size.to_le_bytes()); // comprimido
            h.extend_from_slice(&e.off.to_le_bytes());
            self.put(&h)?;
        }
        let cd_size = self.pos - cd_start;
        let n = entries.len() as u64;
        let z64_off = self.pos;
        let mut t = Vec::with_capacity(56 + 20 + 22);
        t.extend_from_slice(&0x0606_4b50u32.to_le_bytes());
        t.extend_from_slice(&44u64.to_le_bytes());
        t.extend_from_slice(&0x032Du16.to_le_bytes());
        t.extend_from_slice(&45u16.to_le_bytes());
        t.extend_from_slice(&0u32.to_le_bytes());
        t.extend_from_slice(&0u32.to_le_bytes());
        t.extend_from_slice(&n.to_le_bytes());
        t.extend_from_slice(&n.to_le_bytes());
        t.extend_from_slice(&cd_size.to_le_bytes());
        t.extend_from_slice(&cd_start.to_le_bytes());
        t.extend_from_slice(&0x0706_4b50u32.to_le_bytes());
        t.extend_from_slice(&0u32.to_le_bytes());
        t.extend_from_slice(&z64_off.to_le_bytes());
        t.extend_from_slice(&1u32.to_le_bytes());
        t.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
        t.extend_from_slice(&0u16.to_le_bytes());
        t.extend_from_slice(&0u16.to_le_bytes());
        t.extend_from_slice(&0xFFFFu16.to_le_bytes());
        t.extend_from_slice(&0xFFFFu16.to_le_bytes());
        t.extend_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
        t.extend_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
        t.extend_from_slice(&0u16.to_le_bytes());
        self.put(&t)?;
        self.w.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc_conocido() {
        assert_eq!(!crc_update(0xFFFF_FFFF, b"123456789"), 0xCBF4_3926);
    }

    #[test]
    fn el_zip_se_lee_con_otra_biblioteca() {
        let mut out = Vec::new();
        let mut z = Zip::new(&mut out);
        z.add_dir("d", 1_700_000_000).unwrap();
        z.add_file("d/a.txt", 1_700_000_000, &mut &b"hola mundo"[..], u64::MAX).unwrap();
        z.add_file("vacio", 0, &mut &b""[..], u64::MAX).unwrap();
        z.add_file("ñ/limitado.bin", 5, &mut &[7u8; 100][..], 10).unwrap();
        z.finish().unwrap();
        let mut ar = zip::ZipArchive::new(io::Cursor::new(out)).unwrap();
        assert_eq!(ar.len(), 4);
        let mut s = String::new();
        ar.by_name("d/a.txt").unwrap().read_to_string(&mut s).unwrap();
        assert_eq!(s, "hola mundo");
        assert_eq!(ar.by_name("vacio").unwrap().size(), 0);
        assert_eq!(ar.by_name("ñ/limitado.bin").unwrap().size(), 10);
        assert!(ar.by_name("d/").unwrap().is_dir());
    }
}
