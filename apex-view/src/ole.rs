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
    Some(u16::from_le_bytes(d.get(p..p + 2)?.try_into().ok()?))
}

fn u32_at(d: &[u8], p: usize) -> Option<u32> {
    Some(u32::from_le_bytes(d.get(p..p + 4)?.try_into().ok()?))
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
            let at = (difat as usize + 1) * len;
            let per = len / 4 - 1;
            for j in 0..per {
                if fat_sectors.len() >= nfat {
                    break;
                }
                fat_sectors.push(u32_at(d, at + j * 4).unwrap_or(u32::MAX));
            }
            difat = u32_at(d, at + per * 4).unwrap_or(u32::MAX);
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

    /// The bytes of the chain of sectors from `start`, at most `size`.
    fn chain(&self, start: u32, size: u64) -> Vec<u8> {
        let len = 1usize << self.shift;
        let mut out = Vec::new();
        let mut s = start;
        let mut n = 0;
        while s < LAST && (out.len() as u64) < size && n <= self.fat.len() {
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
        while s < LAST && (out.len() as u64) < size && n <= self.minifat.len() {
            let at = s as usize * 64;
            let Some(b) = self.mini.get(at..(at + 64).min(self.mini.len())).filter(|b| !b.is_empty()) else { break };
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
