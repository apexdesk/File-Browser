//! Minimal ZIP reader/writer (stored + deflate) for OOXML and ODF packages,
//! with the Zip64 extensions (more than 65535 entries, or sizes and offsets
//! past 4 GB) when reading and when a package needs them.

use std::collections::HashMap;

use super::bad;
use crate::Error;

fn u16_at(b: &[u8], i: usize) -> Result<u16, Error> {
    b.get(i..i + 2).map(|s| u16::from_le_bytes([s[0], s[1]])).ok_or_else(|| bad("truncated ZIP"))
}

fn u32_at(b: &[u8], i: usize) -> Result<u32, Error> {
    b.get(i..i + 4).map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]])).ok_or_else(|| bad("truncated ZIP"))
}

fn u64_at(b: &[u8], i: usize) -> Result<u64, Error> {
    b.get(i..i + 8).map(|s| u64::from_le_bytes(s.try_into().unwrap())).ok_or_else(|| bad("truncated ZIP"))
}

const FULL32: u32 = 0xFFFF_FFFF;

struct Entry {
    name: String,
    method: u16,
    comp_size: usize,
    size: usize,
    offset: usize,
}

pub struct ZipReader<'a> {
    data: &'a [u8],
    entries: Vec<Entry>,
    /// Where each name is in `entries` (the first of its name), as
    /// written and in lowercase: packages may have very many.
    by_name: HashMap<String, usize>,
    by_lower: HashMap<String, usize>,
    /// What reads may still inflate to, all parts together: a package of
    /// many bombs stops there too.
    budget: std::sync::atomic::AtomicUsize,
    /// Most one part may inflate to.
    part_cap: usize,
    /// How many times its stored size a part may inflate to.
    ratio: usize,
}

/// Most one part read whole may inflate to, whatever size it claims.
const PART_CAP: usize = 1 << 30;
impl<'a> ZipReader<'a> {
    pub fn new(data: &'a [u8], limits: &crate::Limits) -> Result<Self, Error> {
        if data.len() < 22 {
            return Err(bad("not a valid ZIP file"));
        }
        let min = data.len().saturating_sub(22 + 65535);
        let eocd = (min..=data.len() - 22)
            .rev()
            .find(|&i| data[i..i + 4] == [0x50, 0x4b, 0x05, 0x06])
            .ok_or_else(|| bad("not a valid ZIP file"))?;
        let mut count = u16_at(data, eocd + 10)? as u64;
        let mut p = u32_at(data, eocd + 16)? as u64;
        // Zip64: a locator just before the end record points to the Zip64
        // end record, which has the real count and directory offset.
        if eocd >= 20 && u32_at(data, eocd - 20)? == 0x0706_4b50 {
            let z = usize::try_from(u64_at(data, eocd - 12)?).map_err(|_| bad("truncated ZIP"))?;
            if u32_at(data, z)? == 0x0606_4b50 {
                count = u64_at(data, z + 32)?;
                p = u64_at(data, z + 48)?;
            }
        }
        let mut p = usize::try_from(p).map_err(|_| bad("truncated ZIP"))?;
        // Each entry takes 46 bytes at least: a larger count is corrupt.
        let count = usize::try_from(count).unwrap_or(usize::MAX).min(data.len() / 46);
        let mut entries = Vec::with_capacity(count);
        for _ in 0..count {
            if u32_at(data, p)? != 0x0201_4b50 {
                return Err(bad("corrupt ZIP directory"));
            }
            let method = u16_at(data, p + 10)?;
            let comp32 = u32_at(data, p + 20)?;
            let size32 = u32_at(data, p + 24)?;
            let name_len = u16_at(data, p + 28)? as usize;
            let extra_len = u16_at(data, p + 30)? as usize;
            let comment_len = u16_at(data, p + 32)? as usize;
            let off32 = u32_at(data, p + 42)?;
            let name = data.get(p + 46..p + 46 + name_len).ok_or_else(|| bad("truncated ZIP"))?;
            let extra = data.get(p + 46 + name_len..p + 46 + name_len + extra_len).unwrap_or(&[]);
            let (mut size, mut comp_size, mut offset) = (size32 as u64, comp32 as u64, off32 as u64);
            // The Zip64 extra field has, in this order, the values of the
            // fields that are full in the fixed part.
            if let Some(z) = extra_field(extra, 0x0001) {
                let mut at = 0;
                for (v, full) in [(&mut size, size32 == FULL32), (&mut comp_size, comp32 == FULL32), (&mut offset, off32 == FULL32)] {
                    if full {
                        if let Ok(x) = u64_at(z, at) {
                            *v = x;
                        }
                        at += 8;
                    }
                }
            }
            let fit = |v: u64| usize::try_from(v).map_err(|_| bad("truncated ZIP"));
            let name = String::from_utf8_lossy(name).replace('\\', "/");
            entries.push(Entry { name, method, comp_size: fit(comp_size)?, size: fit(size)?, offset: fit(offset)? });
            p += 46 + name_len + extra_len + comment_len;
        }
        let mut by_name = HashMap::with_capacity(entries.len());
        let mut by_lower = HashMap::with_capacity(entries.len());
        for (i, e) in entries.iter().enumerate() {
            by_name.entry(e.name.clone()).or_insert(i);
            by_lower.entry(e.name.to_ascii_lowercase()).or_insert(i);
        }
        let budget = std::sync::atomic::AtomicUsize::new(usize::try_from(limits.max_unpacked_bytes).unwrap_or(usize::MAX));
        let part_cap = usize::try_from(limits.max_part_bytes).unwrap_or(usize::MAX).min(PART_CAP);
        let ratio = usize::try_from(limits.max_ratio).unwrap_or(usize::MAX).max(1);
        Ok(ZipReader { data, entries, by_name, by_lower, budget, part_cap, ratio })
    }

    pub fn contains(&self, name: &str) -> bool {
        self.by_name.contains_key(name)
    }

    /// The name `name` is stored under (found as [`ZipReader::read`] finds
    /// it: without a leading `/`, or in another case).
    pub fn entry_name(&self, name: &str) -> Option<&str> {
        let name = name.trim_start_matches('/');
        self.by_name.get(name).or_else(|| self.by_lower.get(&name.to_ascii_lowercase())).map(|&i| self.entries[i].name.as_str())
    }

    /// The entry `name` and its data as stored.
    fn raw(&self, name: &str) -> Result<(&Entry, &'a [u8]), Error> {
        let name = name.trim_start_matches('/');
        let e = self
            .by_name
            .get(name)
            .or_else(|| self.by_lower.get(&name.to_ascii_lowercase()))
            .map(|&i| &self.entries[i])
            .ok_or_else(|| bad(format!("missing part '{}' in package", name)))?;
        let d = self.data;
        if u32_at(d, e.offset)? != 0x0403_4b50 {
            return Err(bad("corrupt ZIP header"));
        }
        let start = e.offset + 30 + u16_at(d, e.offset + 26)? as usize + u16_at(d, e.offset + 28)? as usize;
        let raw = start.checked_add(e.comp_size).and_then(|end| d.get(start..end)).ok_or_else(|| bad("truncated ZIP"))?;
        Ok((e, raw))
    }

    pub fn read(&self, name: &str) -> Result<Vec<u8>, Error> {
        let (e, raw) = self.raw(name)?;
        match e.method {
            0 => {
                if !self.charge(raw.len()) {
                    return Err(bad("ZIP package too large"));
                }
                Ok(raw.to_vec())
            }
            8 => {
                use std::sync::atomic::Ordering;
                let left = self.budget.load(Ordering::Relaxed);
                // A part that fails still took the work: read again and
                // again, it would cost that each time.
                let out = miniz_oxide::inflate::decompress_to_vec_with_limit(raw, inflate_limit(e, self.part_cap, self.ratio).min(left)).map_err(|err| {
                    self.charge(err.output.len().max(raw.len()));
                    bad(format!("could not decompress '{}'", e.name))
                })?;
                self.charge(out.len());
                Ok(out)
            }
            m => Err(bad(format!("unsupported ZIP compression method ({})", m))),
        }
    }

    /// Takes `n` bytes from the package's budget, never below nothing;
    /// false when there weren't that many left.
    fn charge(&self, n: usize) -> bool {
        use std::sync::atomic::Ordering;
        let before = self.budget.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |b| Some(b.saturating_sub(n)));
        before.is_ok_and(|b| b >= n)
    }

    pub fn read_string(&self, name: &str) -> Result<String, Error> {
        let mut b = self.read(name)?;
        if b.starts_with(&[0xEF, 0xBB, 0xBF]) {
            b.drain(..3);
        }
        // Valid text (nearly always) is kept without a copy.
        Ok(String::from_utf8(b).unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned()))
    }
}

/// Most a part may inflate to: damage or a zip bomb stops there. The size
/// it claims is the file's to choose, so the limit is also held to what
/// deflate can give from the bytes stored (under 1032 to 1) and to `cap`.
fn inflate_limit(e: &Entry, cap: usize, ratio: usize) -> usize {
    let claimed = e.size.max(1).saturating_mul(2).saturating_add(512 << 20);
    let possible = e.comp_size.saturating_mul(ratio.min(1040)).saturating_add(1 << 20);
    claimed.min(possible).min(cap)
}

/// The data of the extra field `id` in a list of extra fields.
fn extra_field(mut extra: &[u8], id: u16) -> Option<&[u8]> {
    while extra.len() >= 4 {
        let (tag, len) = (u16::from_le_bytes([extra[0], extra[1]]), u16::from_le_bytes([extra[2], extra[3]]) as usize);
        let body = extra.get(4..4 + len)?;
        if tag == id {
            return Some(body);
        }
        extra = &extra[4 + len..];
    }
    None
}

