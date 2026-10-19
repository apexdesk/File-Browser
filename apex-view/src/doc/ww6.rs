//! Word 6.0 and Word 95 documents (FIB versions 101 to 105). Their
//! structures live in the WordDocument stream itself; the text is 8-bit in
//! the code page of each run's font (16-bit in Far East files with
//! extended characters); property lists use one-byte operation codes,
//! which are translated to their Word 97 form (two-byte borders become
//! `Brc80`s, table cells `TC80`s) so the Word 97 reader applies them.
//! Pictures are metafiles: the bitmap they draw is kept.

use std::sync::Arc;

use super::{Fib, Reader, Runs, TW, find_run, i16_at, plc, read_fkps, u16_at, u32_at};
use crate::Error;
use crate::codepage::codepage;

/// What the Word 97 reader doesn't find in a Word 6 FIB.
pub(super) struct Info {
    /// Text of a non-complex file: from `fc_min` to `fc_mac`.
    fc_min: u32,
    fc_mac: u32,
    complex: bool,
    /// 16-bit text (Far East versions).
    wide: bool,
    /// Code page of the document's language.
    pub(super) cp: u16,
    /// First page and count of the character and paragraph FKPs, for
    /// bin tables that don't list them all.
    pub(super) chp_pages: (u16, u16),
    pub(super) pap_pages: (u16, u16),
}

/// The FIB of a Word 6/95 file: the structures' positions keep the Word
/// 97 numbering, with a gap after the first 38.
pub(super) fn read_fib(wd: &[u8]) -> Result<(Fib, Info), Error> {
    let bad = || Error::Malformed("short FIB".into());
    let flags = u16_at(wd, 0x0A).ok_or_else(bad)?;
    if flags & 0x0100 != 0 {
        return Err(Error::Encrypted);
    }
    let at = |p: usize| u32_at(wd, p).unwrap_or(0);
    let pair = |p: usize| (at(p), at(p + 4));
    let mut fclcb: Vec<(u32, u32)> = (0..38).map(|i| pair(0x58 + i * 8)).collect();
    fclcb.extend((0..22).map(|i| pair(0x192 + i * 8)));
    let ccp = [at(0x34), at(0x38), at(0x3C), at(0x44), at(0x48)];
    let pn = |p: usize| u16_at(wd, p).unwrap_or(0);
    let info = Info {
        fc_min: at(0x18),
        fc_mac: at(0x1C),
        complex: flags & 0x0004 != 0,
        wide: flags & 0x1000 != 0,
        cp: lid_cp(u16_at(wd, 6).ok_or_else(bad)?),
        chp_pages: (pn(0x18A), pn(0x18E)),
        pap_pages: (pn(0x18C), pn(0x190)),
    };
    Ok((Fib { ccp, fclcb, word6: true }, info))
}

/// The ANSI code page of a language.
fn lid_cp(lid: u16) -> u16 {
    match lid & 0x3FF {
        0x02 | 0x19 | 0x22 | 0x23 | 0x2F => 1251,
        0x1A if lid == 0x0C1A => 1251,
        0x05 | 0x0E | 0x15 | 0x18 | 0x1A | 0x1B | 0x1C | 0x24 => 1250,
        0x08 => 1253,
        0x1F => 1254,
        0x0D => 1255,
        0x01 => 1256,
        0x25..=0x27 => 1257,
        0x2A => 1258,
        0x1E => 874,
        0x11 => 932,
        0x12 => 949,
        0x04 if matches!(lid, 0x0404 | 0x0C04 | 0x1404) => 950,
        0x04 => 936,
        _ => 1252,
    }
}

/// The code page of a font's character set (`None`: a symbol font).
fn charset_cp(chs: u8, doc: u16) -> Option<u16> {
    Some(match chs {
        0 => 1252,
        2 => return None,
        77 => 10000,
        128 => 932,
        129 | 130 => 949,
        134 => 936,
        136 => 950,
        161 => 1253,
        162 => 1254,
        163 => 1258,
        177 => 1255,
        178 => 1256,
        186 => 1257,
        204 => 1251,
        222 => 874,
        238 => 1250,
        _ => doc,
    })
}

fn decode_cp(bytes: &[u8], cp: u16) -> String {
    codepage(i32::from(cp)).unwrap_or(encoding_rs::WINDOWS_1252).decode_without_bom_handling(bytes).0.into_owned()
}

// --- property lists -------------------------------------------------------------

/// Operand size of a sprm: fixed, a size byte first, or (table
/// definitions) a 16-bit size one more than the rest.
enum Size {
    Fixed(usize),
    Byte,
    Word,
}

fn size(op: u8) -> Option<Size> {
    use Size::*;
    Some(match op {
        0 | 83 => Fixed(0),
        4..=11 | 13 | 14 | 24 | 25 | 29 | 37 | 44 | 50 | 51 | 53..=58 | 65..=67 | 71 | 75 | 85..=92 | 94 | 98 | 100 | 102 | 104 | 110 | 117..=119 => Fixed(1),
        131 | 132 | 138 | 139 | 142 | 143 | 146 | 147 | 150..=153 | 158 | 159 | 162 | 163 | 185 | 186 => Fixed(1),
        2 | 16..=19 | 21 | 22 | 26..=28 | 30..=36 | 38..=43 | 45..=49 | 69 | 72 | 80 | 93 | 96 | 97 | 99 | 101 | 107 | 109 => Fixed(2),
        121..=124 | 140 | 141 | 144 | 145 | 148 | 149 | 154..=157 | 160 | 161 | 164..=171 | 182..=184 | 189 | 195 | 197 | 198 => Fixed(2),
        73 | 95 | 136 | 137 => Fixed(3),
        20 | 70 | 192 | 194 | 196 | 200 => Fixed(4),
        193 | 199 => Fixed(5),
        187 => Fixed(12),
        3 | 12 | 15 | 23 | 52 | 68 | 74 | 81 | 82 | 103 | 105 | 106 | 108 | 120 | 133 | 191 => Byte,
        188 | 190 => Word,
        _ => return None,
    })
}

/// A two-byte border as a `Brc80`: width in eighths of a point, type,
/// color and distance.
fn brc80(v: u16) -> [u8; 4] {
    let (width, ty, ico, space) = (v & 7, (v >> 3) & 3, (v >> 6) & 0x1F, (v >> 11) & 0x1F);
    let (width, ty) = match width {
        _ if v == 0 || ty == 0 => (0, 0),
        6 => (6, 6),
        7 => (6, 7),
        w => (w * 6, ty),
    };
    [width as u8, ty as u8, ico as u8, space as u8]
}

fn brcs(o: &[u8]) -> Vec<u8> {
    o.chunks_exact(2).flat_map(|c| brc80(u16::from_le_bytes([c[0], c[1]]))).collect()
}

/// A property list in its Word 97 form (operations without one left out).
pub(super) fn translate(g: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut p = 0;
    while let Some(&op) = g.get(p) {
        p += 1;
        let (start, len) = match size(op) {
            Some(Size::Fixed(n)) => (p, n),
            Some(Size::Byte) => (p + 1, *g.get(p).unwrap_or(&0) as usize),
            Some(Size::Word) => (p + 2, (u16_at(g, p).unwrap_or(0) as usize).saturating_sub(1)),
            None => break,
        };
        let Some(o) = g.get(start..start + len) else { break };
        p = start + len;
        let mut put = |w97: u16, o: &[u8]| {
            out.extend_from_slice(&w97.to_le_bytes());
            out.extend_from_slice(o);
        };
        match op {
            5 => put(0x2403, o),
            7 => put(0x2405, o),
            9 => put(0x2407, o),
            13 => put(0x25FF, o),
            16 => put(0x840E, o),
            17 => put(0x840F, o),
            19 => put(0x8411, o),
            20 => put(0x6412, o),
            21 => put(0xA413, o),
            22 => put(0xA414, o),
            24 => put(0x2416, o),
            25 => put(0x2417, o),
            47 => put(0x442D, o),
            // Variable operands keep their size byte.
            15 | 23 | 191 => {
                let w97 = match op {
                    15 => 0xC60D,
                    23 => 0xC615,
                    _ => 0xD609,
                };
                put(w97, &[&[len as u8][..], o].concat());
            }
            65 => put(0x0800, o),
            68 => put(0x6A03, &[o, &[0; 4]].concat()[..4]),
            80 => put(0x4A30, o),
            85..=92 => put(0x0835 + u16::from(op - 85), o),
            93 => put(0x4A4F, o),
            94 => put(0x2A3E, o),
            98 => put(0x2A42, o),
            99 => put(0x4A43, o),
            104 => put(0x2A48, o),
            117 => put(0x0855, o),
            142 => put(0x3009, o),
            143 => put(0x300A, o),
            146 => put(0x3011, o),
            153 => put(0x3014, o),
            156 => put(0xB017, o),
            157 => put(0xB018, o),
            161 => put(0x501C, o),
            164 => put(0xB01F, o),
            165 => put(0xB020, o),
            166 => put(0xB021, o),
            167 => put(0xB022, o),
            168 => put(0x9023, o),
            169 => put(0x9024, o),
            182 => put(0x5400, o),
            184 => put(0x9602, o),
            186 => put(0x3404, o),
            189 => put(0x9407, o),
            187 => {
                let b = brcs(o);
                put(0xD605, &[&[b.len() as u8][..], &b].concat());
            }
            190 => {
                // Cell edges, then 10-byte cells (flags and four
                // borders) as 20-byte ones.
                let n = *o.first().unwrap_or(&0) as usize;
                let edges = o.get(..1 + (n + 1) * 2).unwrap_or(o);
                let mut d = edges.to_vec();
                for tc in o.get(edges.len()..).unwrap_or(&[]).chunks_exact(10).take(n) {
                    d.extend_from_slice(&tc[..2]);
                    d.extend_from_slice(&[0, 0]);
                    d.extend(brcs(&tc[2..]));
                }
                put(0xD608, &[&((d.len() + 1) as u16).to_le_bytes()[..], &d].concat());
            }
            193 if o.len() == 5 => put(0xD620, &[&[7][..], &o[..3], &brc80(u16::from_le_bytes([o[3], o[4]]))].concat()),
            _ => {}
        }
    }
    out
}

// --- styles, fonts and formatting -------------------------------------------------

/// Font names and character sets.
pub(super) fn read_fonts(d: &[u8], doc_cp: u16) -> (Vec<(Arc<str>, bool)>, Vec<u8>) {
    let (mut names, mut sets) = (Vec::new(), Vec::new());
    let end = (u16_at(d, 0).unwrap_or(0) as usize).min(d.len());
    let mut p = 2;
    while p < end && names.len() < 4096 {
        let cb = d[p] as usize;
        let ffn = d.get(p + 1..p + 1 + cb).unwrap_or(&[]);
        let chs = ffn.get(3).copied().unwrap_or(0);
        let name = ffn.get(5..).unwrap_or(&[]).split(|&b| b == 0).next().unwrap_or(&[]);
        names.push((Arc::from(decode_cp(name, charset_cp(chs, doc_cp).unwrap_or(1252)).as_str()), chs == 2));
        sets.push(chs);
        p += 1 + cb;
    }
    (names, sets)
}

/// The formatting pages of a bin table (`pages`: the first page and the
/// count, for tables that list fewer).
pub(super) fn fkps(bte: &[u8], pages: (u16, u16), wd: &[u8], para: bool) -> Runs {
    let (_, items) = plc(bte, 2);
    let mut pns: Vec<u32> = items.iter().map(|it| u32::from(u16::from_le_bytes([it[0], it[1]]))).collect();
    if pns.len() < pages.1 as usize {
        pns = (0..pages.1).map(|i| u32::from(pages.0) + u32::from(i)).collect();
    }
    let mut runs = read_fkps(&pns, wd, para, true);
    for r in &mut runs {
        r.2 = if para { [r.2.get(..2).unwrap_or(&[0, 0]), &translate(r.2.get(2..).unwrap_or(&[]))].concat() } else { translate(&r.2) };
    }
    runs
}

/// The text's characters (still bytes of their code pages) and their
/// positions in the WordDocument stream.
pub(super) fn text(fib: &Fib, info: &Info, wd: &[u8]) -> Result<(Vec<u16>, Vec<u32>), Error> {
    // Pieces of text: (first position, end, file position).
    let mut pieces = Vec::new();
    let clx = fib.part(wd, super::CLX);
    if info.complex && !clx.is_empty() {
        let mut p = 0;
        while p < clx.len() {
            match clx[p] {
                1 => p += 3 + u16_at(clx, p + 1).unwrap_or(0) as usize,
                2 => {
                    let lcb = u32_at(clx, p + 1).unwrap_or(0) as usize;
                    let (cps, pcds) = plc(clx.get(p + 5..(p + 5).saturating_add(lcb).min(clx.len())).unwrap_or(&[]), 8);
                    for (i, pcd) in pcds.iter().enumerate() {
                        if let (Some(&a), Some(&b)) = (cps.get(i), cps.get(i + 1)) {
                            pieces.push((a, b, u32::from_le_bytes([pcd[2], pcd[3], pcd[4], pcd[5]]) & 0x3FFF_FFFF));
                        }
                    }
                    break;
                }
                _ => break,
            }
        }
    } else {
        let n = info.fc_mac.saturating_sub(info.fc_min) / if info.wide { 2 } else { 1 };
        pieces.push((0, n, info.fc_min));
    }
    if pieces.is_empty() {
        return Err(Error::Malformed("the document has no text".into()));
    }
    let mut text = Vec::new();
    let mut fcs = Vec::new();
    // Pieces that point at the same bytes again and again stop at about
    // the text the file can hold.
    let most = wd.len() + (1 << 16);
    for (a, b, fc) in pieces {
        for i in 0..b.saturating_sub(a).min(wd.len() as u32) {
            if text.len() >= most {
                break;
            }
            let pos = fc.saturating_add(if info.wide { i.saturating_mul(2) } else { i });
            let u = if info.wide { u16_at(wd, pos as usize) } else { wd.get(pos as usize).map(|&b| u16::from(b)) };
            let Some(u) = u else { break };
            text.push(u);
            fcs.push(pos);
        }
    }
    Ok((text, fcs))
}

/// Turns the bytes of the text into characters, each in the code page of
/// its font. A double-byte character takes its two positions: the
/// character, then a 0 the reader skips. 16-bit text holds one or two
/// bytes of the code page in each unit.
pub(super) fn decode(r: &mut Reader, sets: &[u8], ftc: u16, info: &Info) {
    let doc_cp = info.cp;
    let mut cps: Vec<Option<u16>> = Vec::with_capacity(r.text.len());
    for cp in 0..r.text.len() {
        let istd = find_run(&r.papx, r.fcs[cp]).and_then(|i| u16_at(&r.papx[i].2, 0)).unwrap_or(0);
        let font = r.chp_at(cp, istd).0.font;
        let chs = sets.get(font.unwrap_or(ftc) as usize).copied().unwrap_or(0);
        cps.push(charset_cp(chs, doc_cp));
    }
    if info.wide {
        for (u, page) in r.text.iter_mut().zip(cps) {
            *u = match (*u, page) {
                (0..0x80, _) => continue,
                (v @ 0x80..=0xFF, None) => 0xF000 | v,
                (v, page) => {
                    let bytes = if v <= 0xFF { vec![v as u8] } else { v.to_le_bytes().to_vec() };
                    let s = decode_cp(&bytes, page.unwrap_or(1252));
                    s.chars().find(|&c| c >= ' ').and_then(|c| c.encode_utf16(&mut [0; 2]).first().copied()).unwrap_or(0)
                }
            };
        }
        return;
    }
    let mut cp = 0;
    while cp < r.text.len() {
        let page = cps[cp];
        let end = (cp..r.text.len()).find(|&i| cps[i] != page).unwrap_or(r.text.len());
        let bytes: Vec<u8> = r.text[cp..end].iter().map(|&u| u as u8).collect();
        match page {
            // Symbol fonts: their private use area, as Word 97 keeps it.
            None => {
                for (i, &b) in bytes.iter().enumerate() {
                    r.text[cp + i] = if b >= 0x20 { 0xF000 | u16::from(b) } else { u16::from(b) };
                }
            }
            Some(page @ (932 | 936 | 949 | 950)) => {
                let lead = |b: u8| if page == 932 { matches!(b, 0x81..=0x9F | 0xE0..=0xFC) } else { b >= 0x81 && b != 0xFF };
                let mut i = 0;
                while i < bytes.len() {
                    let n = if lead(bytes[i]) && bytes.get(i + 1).is_some_and(|&t| t >= 0x40) { 2 } else { 1 };
                    let c = decode_cp(&bytes[i..i + n], page).encode_utf16().next().unwrap_or(0xFFFD);
                    r.text[cp + i] = if bytes[i] < 0x80 { u16::from(bytes[i]) } else { c };
                    if n == 2 {
                        r.text[cp + i + 1] = 0;
                    }
                    i += n;
                }
            }
            Some(page) => {
                let s: Vec<u16> = decode_cp(&bytes, page).encode_utf16().collect();
                if s.len() == bytes.len() {
                    r.text[cp..end].copy_from_slice(&s);
                } else {
                    for (i, &b) in bytes.iter().enumerate() {
                        r.text[cp + i] = super::cp1252(b);
                    }
                }
            }
        }
        cp = end;
    }
}

// --- pictures -------------------------------------------------------------------------

/// The picture at `at` in the WordDocument stream: the largest bitmap its
/// metafile draws, and its size in points.
pub(super) fn picture(wd: &[u8], at: u32) -> Option<(Vec<u8>, f32, f32)> {
    let d = wd.get(at as usize..)?;
    let lcb = u32_at(d, 0)? as usize;
    let header = u16_at(d, 4)? as usize;
    let d = d.get(..lcb.min(d.len()))?;
    let (gx, gy) = (i16_at(d, 0x1C)? as f32, i16_at(d, 0x1E)? as f32);
    let (mx, my) = (u16_at(d, 0x20)? as f32, u16_at(d, 0x22)? as f32);
    if lcb <= header || header < 0x2C {
        return None;
    }
    let dib = wmf_dib(d.get(header..)?)?;
    let bytes = crate::images::bmp_from_dib(dib)?;
    let scale = |g: f32, m: f32| g * if m > 0.0 { m / 1000.0 } else { 1.0 } / TW;
    Some((bytes, scale(gx, mx).max(1.0), scale(gy, my).max(1.0)))
}

/// The largest device-independent bitmap drawn by a Windows metafile.
fn wmf_dib(m: &[u8]) -> Option<&[u8]> {
    let mut p = u16_at(m, 2)? as usize * 2;
    let mut best: Option<&[u8]> = None;
    while let (Some(words), Some(func)) = (u32_at(m, p), u16_at(m, p + 4)) {
        let len = (words as usize).saturating_mul(2);
        if len < 6 {
            break;
        }
        let rec = m.get(p + 6..p.saturating_add(len).min(m.len()))?;
        // StretchDIBits, DIBStretchBlt, SetDIBitsToDevice, DIBBitBlt.
        let skip = match func {
            0x0F43 => Some(22),
            0x0B41 => Some(20),
            0x0D33 => Some(18),
            0x0940 => Some(16),
            _ => None,
        };
        if let Some(dib) = skip.and_then(|s| rec.get(s..)).filter(|d| u32_at(d, 0) == Some(40))
            && best.is_none_or(|b| b.len() < dib.len())
        {
            best = Some(dib);
        }
        p = p.saturating_add(len);
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn translated_properties() {
        // Bold, 14 pt, font 2, centered, 0.5" left indent.
        let g = [85, 1, 99, 28, 0, 93, 2, 0, 5, 1, 17, 0xD0, 0x02];
        let t = translate(&g);
        let ops: Vec<u16> = super::super::sprms(&t).iter().map(|s| s.0).collect();
        assert_eq!(ops, vec![0x0835, 0x4A43, 0x4A4F, 0x2403, 0x840F]);
        assert_eq!(brc80(0x0089), [6, 1, 2, 0]);
        // Unknown operations end the list.
        assert_eq!(translate(&[250, 1, 2]), Vec::<u8>::new());
    }
}
