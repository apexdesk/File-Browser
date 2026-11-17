//! OLE compound files (the legacy .doc, .xls and .ppt): the streams at
//! the top of the file, read leniently (chains past the end of the file
//! or running in circles stop where they go wrong).

use std::collections::HashSet;

use crate::{Error, Limits};

const SIGNATURE: &[u8] = &[0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];
/// Sector numbers from here on aren't sectors (end of chain, free...).
const LAST: u32 = 0xFFFF_FFFA;

pub(crate) fn is_ole(data: &[u8]) -> bool {
    data.starts_with(SIGNATURE)
}

fn u16_at(d: &[u8], p: usize) -> Option<u16> {
    Some(u16::from_le_bytes(d.get(p..p.checked_add(2)?)?.try_into().ok()?))
}

fn u32_at(d: &[u8], p: usize) -> Option<u32> {
    Some(u32::from_le_bytes(d.get(p..p.checked_add(4)?)?.try_into().ok()?))
}

struct Entry {
    name: String,
    kind: u8,
    left: u32,
    right: u32,
    child: u32,
    start: u32,
    size: u64,
}

pub(crate) struct Ole<'a> {
    d: &'a [u8],
    shift: u32,
    fat: Vec<u32>,
    minifat: Vec<u32>,
    mini: Vec<u8>,
    cutoff: u64,
    entries: Vec<Entry>,
    /// The root's streams and storages: their entries.
    top: Vec<usize>,
    max_part: u64,
}

impl<'a> Ole<'a> {
    pub(crate) fn open(d: &'a [u8], limits: &Limits) -> Result<Ole<'a>, Error> {
        let bad = || Error::Malformed("not a compound file".into());
        if !is_ole(d) {
            return Err(bad());
        }
        let shift = u16_at(d, 30).ok_or_else(bad)? as u32;
        if !(7..=16).contains(&shift) {
            return Err(bad());
        }
        let len = 1usize << shift;
        let sectors = d.len() / len;
        // The FAT's sectors: 109 in the header, the rest in a chain of
        // DIFAT sectors.
        let nfat = (u32_at(d, 44).unwrap_or(0) as usize).min(sectors / (len / 4) + 2);
        let mut fat_sectors: Vec<u32> = (0..109.min(nfat)).filter_map(|i| u32_at(d, 76 + i * 4)).collect();
        let mut difat = u32_at(d, 68).unwrap_or(u32::MAX);
        let mut seen = HashSet::new();
        while fat_sectors.len() < nfat && difat < LAST && seen.insert(difat) {
            let at = (difat as usize).saturating_add(1).saturating_mul(len);
            let per = len / 4 - 1;
            for j in 0..per {
                if fat_sectors.len() >= nfat {
                    break;
                }
                fat_sectors.push(u32_at(d, at.saturating_add(j * 4)).unwrap_or(u32::MAX));
            }
            difat = u32_at(d, at.saturating_add(per * 4)).unwrap_or(u32::MAX);
        }
        let mut fat = Vec::with_capacity(fat_sectors.len() * len / 4);
        for s in fat_sectors {
            let at = (s as usize).saturating_add(1).saturating_mul(len);
            match d.get(at..at.saturating_add(len)) {
                Some(b) => fat.extend(b.chunks_exact(4).map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))),
                None => fat.extend(std::iter::repeat_n(u32::MAX, len / 4)),
            }
        }
        let mut ole = Ole { d, shift, fat, minifat: Vec::new(), mini: Vec::new(), cutoff: u32_at(d, 56).unwrap_or(4096) as u64, entries: Vec::new(), top: Vec::new(), max_part: limits.max_part_bytes };
        let dir = ole.chain(u32_at(d, 48).unwrap_or(u32::MAX), u64::MAX);
        for e in dir.chunks_exact(128).take(1 << 20) {
            let n = (u16_at(e, 64).unwrap_or(0) as usize / 2).saturating_sub(1).min(31);
            let name: Vec<u16> = (0..n).filter_map(|i| u16_at(e, i * 2)).collect();
            ole.entries.push(Entry {
                name: String::from_utf16_lossy(&name),
                kind: e[66],
                left: u32_at(e, 68).unwrap_or(u32::MAX),
                right: u32_at(e, 72).unwrap_or(u32::MAX),
                child: u32_at(e, 76).unwrap_or(u32::MAX),
                start: u32_at(e, 116).unwrap_or(u32::MAX),
                size: if shift == 9 { u32_at(e, 120).unwrap_or(0) as u64 } else { u32_at(e, 120).unwrap_or(0) as u64 | (u32_at(e, 124).unwrap_or(0) as u64) << 32 },
            });
        }
        let root = ole.entries.first().ok_or_else(bad)?;
        let (root_start, root_size, root_child) = (root.start, root.size, root.child);
        let minifat = ole.chain(u32_at(d, 60).unwrap_or(u32::MAX), u64::MAX);
        ole.minifat = minifat.chunks_exact(4).map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect();
        ole.mini = ole.chain(root_start, root_size.min(ole.max_part));
        // The root's children: a tree of siblings.
        let mut stack = vec![root_child];
        let mut seen = HashSet::new();
        while let Some(i) = stack.pop() {
            if i as usize >= ole.entries.len() || !seen.insert(i) {
                continue;
            }
            let e = &ole.entries[i as usize];
            stack.push(e.left);
            stack.push(e.right);
            ole.top.push(i as usize);
        }
        Ok(ole)
    }

    /// The bytes of the chain of sectors from `start`, at most `size`. A
    /// chain coming back to a sector stops there (each sector is read once:
    /// at most the file's size).
    fn chain(&self, start: u32, size: u64) -> Vec<u8> {
        let len = 1usize << self.shift;
        let mut out = Vec::new();
        let mut s = start;
        let mut n = 0;
        let mut seen = vec![false; self.d.len() / len + 1];
        while s < LAST && (out.len() as u64) < size && n <= self.fat.len() {
            match seen.get_mut(s as usize) {
                Some(v) if !*v => *v = true,
                _ => break,
            }
            let at = (s as usize).saturating_add(1).saturating_mul(len);
            let Some(b) = self.d.get(at..at.saturating_add(len).min(self.d.len())).filter(|b| !b.is_empty()) else { break };
            out.extend_from_slice(b);
            s = self.fat.get(s as usize).copied().unwrap_or(u32::MAX);
            n += 1;
        }
        if (out.len() as u64) > size {
            out.truncate(size as usize);
        }
        out
    }

    fn mini_chain(&self, start: u32, size: u64) -> Vec<u8> {
        let mut out = Vec::new();
        let mut s = start;
        let mut n = 0;
        let mut seen = vec![false; self.mini.len() / 64 + 1];
        while s < LAST && (out.len() as u64) < size && n <= self.minifat.len() {
            match seen.get_mut(s as usize) {
                Some(v) if !*v => *v = true,
                _ => break,
            }
            let at = (s as usize).saturating_mul(64);
            let Some(b) = self.mini.get(at..at.saturating_add(64).min(self.mini.len())).filter(|b| !b.is_empty()) else { break };
            out.extend_from_slice(b);
            s = self.minifat.get(s as usize).copied().unwrap_or(u32::MAX);
            n += 1;
        }
        out.truncate(size.min(out.len() as u64) as usize);
        out
    }

    /// Whether the file has a stream or storage `name` at its top.
    pub(crate) fn has(&self, name: &str) -> bool {
        self.top.iter().any(|&i| self.entries[i].name.eq_ignore_ascii_case(name))
    }

    /// The stream `name` at the top of the file.
    pub(crate) fn stream(&self, name: &str) -> Option<Vec<u8>> {
        let e = self.top.iter().map(|&i| &self.entries[i]).find(|e| e.kind == 2 && e.name.eq_ignore_ascii_case(name))?;
        let size = e.size.min(self.max_part);
        Some(if e.size < self.cutoff { self.mini_chain(e.start, size) } else { self.chain(e.start, size) })
    }
}

/// A compound file holding `streams` (test documents): 512-byte sectors,
/// one FAT sector, no mini stream. `cycle` makes the first stream's chain
/// come back to its first sector.
#[cfg(test)]
pub(crate) fn test_file(streams: &[(&str, &[u8])], cycle: bool) -> Vec<u8> {
    const END: u32 = 0xFFFF_FFFE;
    let mut dir = vec![0u8; 512 * (streams.len() + 1).div_ceil(4)];
    let entry = |dir: &mut [u8], i: usize, name: &str, kind: u8, right: u32, child: u32, start: u32, size: u32| {
        let e = &mut dir[i * 128..(i + 1) * 128];
        let units: Vec<u16> = name.encode_utf16().collect();
        for (k, u) in units.iter().enumerate() {
            e[k * 2..k * 2 + 2].copy_from_slice(&u.to_le_bytes());
        }
        e[64..66].copy_from_slice(&(((units.len() + 1) * 2) as u16).to_le_bytes());
        e[66] = kind;
        e[68..72].copy_from_slice(&u32::MAX.to_le_bytes());
        e[72..76].copy_from_slice(&right.to_le_bytes());
        e[76..80].copy_from_slice(&child.to_le_bytes());
        e[116..120].copy_from_slice(&start.to_le_bytes());
        e[120..124].copy_from_slice(&size.to_le_bytes());
    };
    entry(&mut dir, 0, "Root Entry", 5, u32::MAX, if streams.is_empty() { u32::MAX } else { 1 }, END, 0);
    // Sector 0 is the FAT, then the directory, then the streams.
    let mut fat = vec![0xFFFF_FFFDu32];
    let dir_sectors = dir.len() / 512;
    for k in 0..dir_sectors {
        fat.push(if k + 1 == dir_sectors { END } else { 2 + k as u32 });
    }
    let mut body = Vec::new();
    for (i, (name, data)) in streams.iter().enumerate() {
        let first = fat.len() as u32;
        let n = data.len().div_ceil(512).max(1);
        for k in 0..n {
            fat.push(if k + 1 < n { first + k as u32 + 1 } else if cycle && i == 0 { first } else { END });
        }
        let mut d = data.to_vec();
        d.resize(n * 512, 0);
        body.extend(d);
        let right = if i + 1 < streams.len() { i as u32 + 2 } else { u32::MAX };
        let size = if cycle && i == 0 { 1 << 30 } else { data.len() as u32 };
        entry(&mut dir, i + 1, name, 2, right, u32::MAX, first, size);
    }
    assert!(fat.len() <= 128, "test file too large");
    fat.resize(128, u32::MAX);
    let mut out = vec![0u8; 512];
    out[..8].copy_from_slice(SIGNATURE);
    out[24..26].copy_from_slice(&0x3Eu16.to_le_bytes());
    out[26..28].copy_from_slice(&3u16.to_le_bytes());
    out[28..30].copy_from_slice(&0xFFFEu16.to_le_bytes());
    out[30..32].copy_from_slice(&9u16.to_le_bytes());
    out[32..34].copy_from_slice(&6u16.to_le_bytes());
    out[44..48].copy_from_slice(&1u32.to_le_bytes());
    out[48..52].copy_from_slice(&1u32.to_le_bytes());
    // No mini stream: every stream is at least the cutoff.
    out[56..60].copy_from_slice(&0u32.to_le_bytes());
    out[60..64].copy_from_slice(&END.to_le_bytes());
    out[68..72].copy_from_slice(&END.to_le_bytes());
    for k in 0..109 {
        out[76 + k * 4..80 + k * 4].copy_from_slice(&(if k == 0 { 0 } else { u32::MAX }).to_le_bytes());
    }
    out.extend(fat.iter().flat_map(|v| v.to_le_bytes()));
    out.extend(dir);
    out.extend(body);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_streams_and_stops_circular_chains() {
        let a = vec![7u8; 1500];
        let f = test_file(&[("Alpha", &a), ("Beta", b"xyz")], false);
        let ole = Ole::open(&f, &Limits::default()).unwrap();
        assert!(ole.has("alpha") && ole.has("Beta"));
        assert_eq!(ole.stream("Alpha").unwrap(), a);
        assert_eq!(ole.stream("Beta").unwrap(), b"xyz");
        // A chain running in circles, of a stream claiming a gigabyte, is
        // read once: no more than the file holds.
        let f = test_file(&[("Alpha", &a)], true);
        let ole = Ole::open(&f, &Limits::default()).unwrap();
        assert_eq!(ole.stream("Alpha").unwrap().len(), 1536);
        // Hostile header fields: sector numbers at the top of the range.
        let mut f = test_file(&[("Alpha", &a)], false);
        f[68..72].copy_from_slice(&0xFFFF_FFF0u32.to_le_bytes());
        f[44..48].copy_from_slice(&u32::MAX.to_le_bytes());
        f[48..52].copy_from_slice(&0xFFFF_FFF0u32.to_le_bytes());
        let _ = Ole::open(&f, &Limits::default());
    }
}
