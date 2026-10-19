//! Word 97-2003 documents (.doc) read into the model Word documents are
//! laid out from: the text through its piece table, character and
//! paragraph formatting with their styles, list labels, tables, inline
//! pictures, fields' results, and the sections with their page setup,
//! headers and footers.

use std::collections::HashMap;
use std::sync::Arc;

use crate::docx::read::{Block, Cell, DocModel, PAGE, PAGES, Para, Row, Section, Table, number};
use crate::docx::{Doc, Source};
use crate::draw::Rgba;
use crate::ole::Ole;
use crate::typeset::{Align, LineRule, Object, ParaFormat, Span, Style, Tab, TabAlign};
use crate::{Error, Fonts, Limits};

mod ww6;

/// Twips per point.
const TW: f32 = 20.0;
const MAX_BLOCKS: usize = 200_000;
const MAX_CELLS: usize = 64;

pub(crate) fn open(data: Vec<u8>, limits: &Limits, fonts: &Fonts) -> Result<Doc, Error> {
    let (model, blobs) = read(&data, limits)?;
    Ok(Doc::laid_out(Source::Blobs(blobs), &model, limits, fonts))
}

fn bad(m: &str) -> Error {
    Error::Malformed(m.into())
}

fn u16_at(d: &[u8], p: usize) -> Option<u16> {
    Some(u16::from_le_bytes(d.get(p..p.checked_add(2)?)?.try_into().ok()?))
}

fn u32_at(d: &[u8], p: usize) -> Option<u32> {
    Some(u32::from_le_bytes(d.get(p..p.checked_add(4)?)?.try_into().ok()?))
}

fn i16_at(d: &[u8], p: usize) -> Option<i16> {
    u16_at(d, p).map(|v| v as i16)
}

// --- the file information block ---------------------------------------------

struct Fib {
    /// Characters in the main text, footnotes, headers, comments, endnotes.
    ccp: [u32; 5],
    fclcb: Vec<(u32, u32)>,
    /// A Word 6.0/95 file (see `ww6`).
    word6: bool,
}

const STSHF: usize = 1;
const PLCF_SED: usize = 6;
const PLCF_HDD: usize = 11;
const PLCF_BTE_CHPX: usize = 12;
const PLCF_BTE_PAPX: usize = 13;
const STTBF_FFN: usize = 15;
const CLX: usize = 33;
const DOP: usize = 31;
const PLF_LST: usize = 73;
const PLF_LFO: usize = 74;

impl Fib {
    /// The FIB, and for a Word 6.0/95 file what only it has.
    fn read(wd: &[u8]) -> Result<(Fib, Option<ww6::Info>), Error> {
        // Word 6.0 and 95 (also their Far East versions) and 97 and later.
        if !matches!(u16_at(wd, 0), Some(0xA5EC | 0xA5DC | 0xA699 | 0xA697)) {
            return Err(Error::Unsupported);
        }
        match u16_at(wd, 2) {
            Some(n) if n >= 0x00C1 => {}
            Some(n) if n >= 0x0065 => return ww6::read_fib(wd).map(|(f, i)| (f, Some(i))),
            _ => return Err(Error::Unsupported),
        }
        let csw = u16_at(wd, 32).ok_or_else(|| bad("short FIB"))? as usize;
        let p = 34 + csw * 2;
        let cslw = u16_at(wd, p).ok_or_else(|| bad("short FIB"))? as usize;
        let lw = p + 2;
        let lw_at = |i: usize| if i < cslw { u32_at(wd, lw + i * 4).unwrap_or(0) } else { 0 };
        let ccp = [lw_at(3), lw_at(4), lw_at(5), lw_at(7), lw_at(8)];
        let p = lw + cslw * 4;
        let n = (u16_at(wd, p).ok_or_else(|| bad("short FIB"))? as usize).min(512);
        let fclcb = (0..n).map(|i| (u32_at(wd, p + 2 + i * 8).unwrap_or(0), u32_at(wd, p + 6 + i * 8).unwrap_or(0))).collect();
        Ok((Fib { ccp, fclcb, word6: false }, None))
    }

    /// A structure in the table stream.
    fn part<'a>(&self, table: &'a [u8], i: usize) -> &'a [u8] {
        match self.fclcb.get(i) {
            Some(&(fc, lcb)) if lcb > 0 => table.get(fc as usize..(fc as usize).saturating_add(lcb as usize).min(table.len())).unwrap_or(&[]),
            _ => &[],
        }
    }
}

/// A PLC: `n + 1` character positions and `n` items of `size` bytes.
fn plc(d: &[u8], size: usize) -> (Vec<u32>, Vec<&[u8]>) {
    if d.len() < 4 {
        return (Vec::new(), Vec::new());
    }
    let n = (d.len() - 4) / (4 + size);
    let cps = (0..=n).filter_map(|i| u32_at(d, i * 4)).collect();
    let items = (0..n).filter_map(|i| d.get(4 * (n + 1) + i * size..4 * (n + 1) + (i + 1) * size)).collect();
    (cps, items)
}

// --- properties ---------------------------------------------------------------

/// The operations (`sprm`) of a property list with their operands.
fn sprms(g: &[u8]) -> Vec<(u16, &[u8])> {
    let mut out = Vec::new();
    let mut p = 0;
    while p + 2 <= g.len() {
        let op = u16::from_le_bytes([g[p], g[p + 1]]);
        p += 2;
        let (start, len) = match op >> 13 {
            0 | 1 => (p, 1),
            2 | 4 | 5 => (p, 2),
            3 => (p, 4),
            7 => (p, 3),
            _ => match op {
                // Table definitions: a 16-bit size, one more than the rest.
                0xD608 | 0xD606 => {
                    let cb = u16_at(g, p).unwrap_or(0) as usize;
                    (p + 2, cb.saturating_sub(1))
                }
                0xC615 if g.get(p) == Some(&255) => {
                    // Tab changes with deletion distances: computed size.
                    let del = *g.get(p + 1).unwrap_or(&0) as usize;
                    let add = *g.get(p + 2 + del * 4).unwrap_or(&0) as usize;
                    (p + 1, 1 + del * 4 + 1 + add * 3)
                }
                _ => {
                    let cb = *g.get(p).unwrap_or(&0) as usize;
                    (p + 1, cb)
                }
            },
        };
        let Some(operand) = g.get(start..start + len) else { break };
        out.push((op, operand));
        p = start + len;
    }
    out
}

const ICO: [u32; 17] = [0, 0x000000, 0x0000FF, 0x00FFFF, 0x00FF00, 0xFF00FF, 0xFF0000, 0xFFFF00, 0xFFFFFF, 0x000080, 0x008080, 0x008000, 0x800080, 0x800000, 0x808000, 0x808080, 0xC0C0C0];

fn ico(i: u8) -> Option<Rgba> {
    (i > 0).then(|| ICO.get(i as usize).map(|&c| Rgba::rgb(c))).flatten()
}

fn colorref(o: &[u8]) -> Option<Rgba> {
    (o.len() >= 4 && o[3] != 0xFF).then(|| Rgba([o[0], o[1], o[2], 255]))
}

fn toggle(v: u8, base: bool) -> bool {
    match v {
        0 => false,
        1 => true,
        0x81 => !base,
        _ => base,
    }
}

#[derive(Clone, Default)]
struct Chp {
    bold: bool,
    italic: bool,
    strike: bool,
    underline: bool,
    caps: bool,
    hidden: bool,
    deleted: bool,
    /// Half points.
    size: u16,
    font: Option<u16>,
    color: Option<Rgba>,
    highlight: Option<Rgba>,
    /// 1 superscript, 2 subscript.
    iss: u8,
    special: bool,
    pic: Option<u32>,
    /// A character of a symbol font (`sprmCSymbol`).
    sym: Option<u16>,
}

impl Chp {
    fn apply(&mut self, g: &[u8], base: &Chp) {
        for (op, o) in sprms(g) {
            let b = o.first().copied().unwrap_or(0);
            match op {
                0x0835 => self.bold = toggle(b, base.bold),
                0x0836 => self.italic = toggle(b, base.italic),
                0x0837 | 0x2A53 => self.strike = toggle(b, base.strike),
                0x083B | 0x083A => self.caps = toggle(b, base.caps),
                0x083C => self.hidden = toggle(b, base.hidden),
                0x0800 => self.deleted = b != 0,
                0x2A3E => self.underline = b != 0,
                0x4A43 => self.size = u16_at(o, 0).unwrap_or(24).clamp(2, 3276),
                0x4A4F => self.font = u16_at(o, 0),
                0x2A42 => self.color = ico(b),
                0x6870 => self.color = colorref(o),
                0x2A0C => self.highlight = ico(b),
                0x2A48 => self.iss = b,
                0x0855 => self.special = b != 0,
                0x6A03 => self.pic = u32_at(o, 0),
                0x6A09 => self.sym = u16_at(o, 2),
                _ => {}
            }
        }
    }
}

/// A border's width and colour (none for type 0 or 255).
fn brc80(o: &[u8]) -> Option<Option<(f32, Rgba)>> {
    let b = o.get(..4)?;
    Some((!matches!(b[1], 0 | 0xFF)).then(|| ((b[0] as f32 / 8.0).clamp(0.25, 6.0), ico(b[2]).unwrap_or(Rgba::BLACK))))
}

fn brc(o: &[u8]) -> Option<Option<(f32, Rgba)>> {
    let b = o.get(..8)?;
    Some((!matches!(b[5], 0 | 0xFF)).then(|| ((b[4] as f32 / 8.0).clamp(0.25, 6.0), colorref(b).unwrap_or(Rgba::BLACK))))
}

/// A background from a `Shd80` (`None` for none, automatic or white).
fn shd80(v: u16) -> Option<Rgba> {
    let (fore, back, pat) = ((v & 0x1F) as u8, ((v >> 5) & 0x1F) as u8, v >> 10);
    if pat == 0 && back == 0 {
        return None;
    }
    let c = if pat == 1 { ico(fore) } else { ico(back) };
    c.filter(|c| *c != Rgba::WHITE)
}

/// A background from a `Shd` (10 bytes).
fn shd(o: &[u8]) -> Option<Rgba> {
    let pat = u16_at(o, 8)?;
    if pat == 0xFFFF {
        return None;
    }
    let c = if pat == 1 { colorref(o.get(0..4)?) } else { colorref(o.get(4..8)?) };
    c.filter(|c| *c != Rgba::WHITE)
}

/// Tab stop changes: deleted positions, then added stops (`close` holds
/// the extra deletion distances of `sprmPChgTabs`).
fn change_tabs(tabs: &mut Vec<Tab>, o: &[u8], close: bool) {
    let del = *o.first().unwrap_or(&0) as usize;
    for i in 0..del {
        if let Some(x) = i16_at(o, 1 + i * 2) {
            let x = x as f32 / TW;
            tabs.retain(|t| (t.pos - x).abs() > 0.5);
        }
    }
    let p = 1 + del * if close { 4 } else { 2 };
    let add = *o.get(p).unwrap_or(&0) as usize;
    for i in 0..add {
        let (Some(x), Some(&tbd)) = (i16_at(o, p + 1 + i * 2), o.get(p + 1 + add * 2 + i)) else { break };
        let align = match tbd & 7 {
            1 => TabAlign::Center,
            2 | 3 => TabAlign::Right,
            4 => continue,
            _ => TabAlign::Left,
        };
        let leader = match (tbd >> 3) & 7 {
            1 | 5 => Some('.'),
            2 => Some('-'),
            3 | 4 => Some('_'),
            _ => None,
        };
        if x >= 0 && tabs.len() < 64 {
            let pos = x as f32 / TW;
            tabs.retain(|t| (t.pos - pos).abs() > 0.5);
            let at = tabs.partition_point(|t| t.pos < pos);
            tabs.insert(at, Tab { pos, align, leader });
        }
    }
}

/// The most of a `sprmTDefTable` kept: 64 cells' edges and descriptions.
const MAX_TDEF: usize = 1 + 65 * 2 + 64 * 20;

type Border = Option<(f32, Rgba)>;

#[derive(Clone, Default)]
struct RowDef {
    /// `sprmTDefTable`: the cell edges and the old cell descriptions.
    tdef: Vec<u8>,
    /// The table's borders: top, left, bottom, right, inside lines.
    borders: Option<[Border; 6]>,
    shd: Vec<Option<Rgba>>,
    /// Borders set on cells `a..b`: (a, b, sides top-left-bottom-right as
    /// bits, border).
    set_brc: Vec<(u8, u8, u8, Border)>,
    height: i16,
    header: bool,
    gap: Option<i16>,
    jc: u8,
}

#[derive(Clone, Default)]
struct Pap {
    istd: u16,
    jc: u8,
    ilfo: u16,
    ilvl: u8,
    /// Old numbering (Word 6/95): 10 a numbered, 11 a bulleted paragraph.
    nlvlanm: u8,
    in_table: bool,
    ttp: bool,
    itap: u32,
    inner_ttp: bool,
    left: i16,
    right: i16,
    first: i16,
    before: u16,
    after: u16,
    line: Option<(i16, u16)>,
    page_break: bool,
    contextual: bool,
    tabs: Vec<Tab>,
    shading: Option<Rgba>,
    row: RowDef,
}

impl Pap {
    fn apply(&mut self, g: &[u8]) {
        for (op, o) in sprms(g) {
            let b = o.first().copied().unwrap_or(0);
            match op {
                0x2403 | 0x2461 => self.jc = b,
                0x260A => self.ilvl = b.min(8),
                0x25FF => self.nlvlanm = b,
                0x460B => self.ilfo = u16_at(o, 0).unwrap_or(0),
                0x2416 => self.in_table = b != 0,
                0x2417 => self.ttp = b != 0,
                0x6649 => self.itap = u32_at(o, 0).unwrap_or(0),
                0x244C => self.inner_ttp = b != 0,
                0x840F | 0x845E => self.left = i16_at(o, 0).unwrap_or(0),
                0x840E | 0x845D => self.right = i16_at(o, 0).unwrap_or(0),
                0x8411 | 0x8460 => self.first = i16_at(o, 0).unwrap_or(0),
                0xA413 => self.before = u16_at(o, 0).unwrap_or(0),
                0xA414 => self.after = u16_at(o, 0).unwrap_or(0),
                0x6412 => self.line = Some((i16_at(o, 0).unwrap_or(240), u16_at(o, 2).unwrap_or(1))),
                0x2407 => self.page_break = b != 0,
                0x246D => self.contextual = b != 0,
                0xC60D => change_tabs(&mut self.tabs, o, false),
                0xC615 => change_tabs(&mut self.tabs, o, true),
                0x442D => self.shading = shd80(u16_at(o, 0).unwrap_or(0)),
                0xC64D => self.shading = shd(o),
                0xD608 => self.row.tdef = o[..o.len().min(MAX_TDEF)].to_vec(),
                0xD605 => self.row.borders = Some(std::array::from_fn(|i| brc80(o.get(i * 4..).unwrap_or(&[])).flatten())),
                0xD613 => self.row.borders = Some(std::array::from_fn(|i| brc(o.get(i * 8..).unwrap_or(&[])).flatten())),
                0xD609 => self.row.shd = o.chunks_exact(2).take(MAX_CELLS).map(|c| shd80(u16::from_le_bytes([c[0], c[1]]))).collect(),
                // Cells from 0, 22 and 44.
                0xD612 | 0xD616 | 0xD60C => {
                    let from = match op {
                        0xD612 => 0,
                        0xD616 => 22,
                        _ => 44,
                    };
                    for (i, c) in o.chunks_exact(10).enumerate().take(MAX_CELLS.saturating_sub(from)) {
                        if self.row.shd.len() <= from + i {
                            self.row.shd.resize(from + i + 1, None);
                        }
                        self.row.shd[from + i] = shd(c);
                    }
                }
                0xD620 | 0xD62F if o.len() >= 3 => {
                    let b = if op == 0xD620 { brc80(&o[3..]) } else { brc(&o[3..]) };
                    if let Some(b) = b
                        && self.row.set_brc.len() < 256
                    {
                        self.row.set_brc.push((o[0], o[1], o[2], b));
                    }
                }
                0x9407 => self.row.height = i16_at(o, 0).unwrap_or(0),
                0x3404 => self.row.header = b != 0,
                0x9602 => self.row.gap = i16_at(o, 0),
                0x5400 | 0x548A => self.row.jc = b,
                _ => {}
            }
        }
    }
}

// --- formatting runs (FKPs) ------------------------------------------------------

/// The pages of a bin table.
fn bte_pages(plc_data: &[u8]) -> Vec<u32> {
    plc(plc_data, 4).1.iter().map(|it| u32::from_le_bytes([it[0], it[1], it[2], it[3]]) & 0x3F_FFFF).collect()
}

type Runs = Vec<(u32, u32, Vec<u8>)>;

/// Runs of file positions with their property list; paragraph runs start
/// with the style. Word 6 pages have shorter paragraph entries and sizes
/// in words.
fn read_fkps(pns: &[u32], wd: &[u8], para: bool, word6: bool) -> Runs {
    let mut out = Vec::new();
    // A page named over and over is read once.
    let mut seen = std::collections::HashSet::new();
    for &pn in pns {
        if !seen.insert(pn) {
            continue;
        }
        let pn = pn as usize;
        let Some(page) = wd.get(pn * 512..pn * 512 + 512) else { continue };
        let n = page[511] as usize;
        for i in 0..n {
            let (Some(a), Some(b)) = (u32_at(page, i * 4), u32_at(page, (i + 1) * 4)) else { break };
            let base = 4 * (n + 1);
            let g = if para {
                let off = page.get(base + if word6 { 7 } else { 13 } * i).copied().unwrap_or(0) as usize * 2;
                let cb = page.get(off).copied().unwrap_or(0) as usize;
                let (start, len) = match cb {
                    _ if word6 => (off + 1, cb * 2),
                    0 => (off + 2, page.get(off + 1).copied().unwrap_or(0) as usize * 2),
                    _ => (off + 1, cb * 2 - 1),
                };
                page.get(start..start + len).unwrap_or(&[]).to_vec()
            } else {
                let off = page.get(base + i).copied().unwrap_or(0) as usize * 2;
                if off == 0 {
                    Vec::new()
                } else {
                    let cb = page.get(off).copied().unwrap_or(0) as usize;
                    page.get(off + 1..off + 1 + cb).unwrap_or(&[]).to_vec()
                }
            };
            out.push((a, b, g));
        }
    }
    out.sort_by_key(|r| r.0);
    out
}

fn find_run(runs: &Runs, fc: u32) -> Option<usize> {
    let i = runs.partition_point(|r| r.1 <= fc);
    runs.get(i).filter(|r| r.0 <= fc).map(|_| i)
}

// --- styles, fonts and lists -------------------------------------------------------

struct StyleDef {
    base: u16,
    papx: Vec<u8>,
    chpx: Vec<u8>,
}

/// The style sheet (Word 6 names are 8-bit).
fn read_styles(d: &[u8], word6: bool) -> Vec<Option<StyleDef>> {
    let mut out = Vec::new();
    let Some(cb_stshi) = u16_at(d, 0) else { return out };
    let cstd = (u16_at(d, 2).unwrap_or(0) as usize).min(4096);
    let cb_base = u16_at(d, 4).unwrap_or(10) as usize;
    let mut p = 2 + cb_stshi as usize;
    for _ in 0..cstd {
        let Some(cb) = u16_at(d, p) else { break };
        let std = d.get(p + 2..p + 2 + cb as usize).unwrap_or(&[]);
        p += 2 + cb as usize;
        if std.len() < cb_base || cb_base < 4 {
            out.push(None);
            continue;
        }
        let w1 = u16_at(std, 2).unwrap_or(0);
        let cupx = u16_at(std, 4).unwrap_or(0) & 0xF;
        let para = w1 & 0xF == 1;
        let base = w1 >> 4;
        let mut q = if word6 {
            cb_base + 1 + std.get(cb_base).copied().unwrap_or(0) as usize + 1
        } else {
            cb_base + 2 + u16_at(std, cb_base).unwrap_or(0) as usize * 2 + 2
        };
        let mut upx = Vec::new();
        for _ in 0..cupx {
            if q % 2 == 1 {
                q += 1;
            }
            let n = u16_at(std, q).unwrap_or(0) as usize;
            upx.push(std.get(q + 2..q + 2 + n).unwrap_or(&[]).to_vec());
            q += 2 + n;
        }
        let (papx, chpx) = if para {
            (upx.first().map(|u| u.get(2..).unwrap_or(&[]).to_vec()).unwrap_or_default(), upx.get(1).cloned().unwrap_or_default())
        } else {
            (Vec::new(), upx.first().cloned().unwrap_or_default())
        };
        out.push(Some(StyleDef { base, papx, chpx }));
    }
    out
}

/// Font names, and which are symbol fonts (charset 2).
fn read_fonts(d: &[u8]) -> Vec<(Arc<str>, bool)> {
    let mut out = Vec::new();
    let n = (u16_at(d, 0).unwrap_or(0) as usize).min(4096);
    let mut p = 4;
    for _ in 0..n {
        let Some(&cb) = d.get(p) else { break };
        let ffn = d.get(p + 1..p + 1 + cb as usize).unwrap_or(&[]);
        let name: Vec<u16> = ffn.get(39..).unwrap_or(&[]).chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).take_while(|&c| c != 0).collect();
        let symbol = ffn.get(5) == Some(&2);
        out.push((Arc::from(String::from_utf16_lossy(&name).as_str()), symbol));
        p += 1 + cb as usize;
    }
    out
}

#[derive(Clone, Default)]
struct Level {
    start: i32,
    nfc: u8,
    /// After the label: 0 a tab, 1 a space, 2 nothing.
    follow: u8,
    papx: Vec<u8>,
    /// The label: characters below 9 stand for the levels' numbers.
    text: Vec<u16>,
}

/// Lists by `ilfo` (from 1): their id and levels.
fn read_lists(lst: &[u8], lfo: &[u8]) -> Vec<(i32, Arc<Vec<Level>>)> {
    let mut by_id: HashMap<i32, Arc<Vec<Level>>> = HashMap::new();
    let n = (u16_at(lst, 0).unwrap_or(0) as usize).min(4096);
    let mut lists = Vec::new();
    for i in 0..n {
        let p = 2 + i * 28;
        let (Some(id), Some(flags)) = (u32_at(lst, p), lst.get(p + 26)) else { break };
        lists.push((id as i32, if flags & 1 != 0 { 1 } else { 9 }));
    }
    let mut p = 2 + n * 28;
    for (id, levels) in lists {
        let mut out = Vec::new();
        for _ in 0..levels {
            let Some(lvlf) = lst.get(p..p + 28) else { break };
            let (chpx, papx) = (lvlf[24] as usize, lvlf[25] as usize);
            let mut lv = Level { start: i32::from_le_bytes([lvlf[0], lvlf[1], lvlf[2], lvlf[3]]).clamp(0, 1 << 20), nfc: lvlf[4], follow: lvlf[15], papx: lst.get(p + 28..p + 28 + papx).unwrap_or(&[]).to_vec(), text: Vec::new() };
            p += 28 + papx + chpx;
            let cch = (u16_at(lst, p).unwrap_or(0) as usize).min(64);
            lv.text = (0..cch).filter_map(|i| u16_at(lst, p + 2 + i * 2)).collect();
            p += 2 + u16_at(lst, p).unwrap_or(0) as usize * 2;
            out.push(lv);
        }
        by_id.insert(id, Arc::new(out));
    }
    let m = (u32_at(lfo, 0).unwrap_or(0) as usize).min(4096);
    (0..m).map(|i| u32_at(lfo, 4 + i * 16).map(|id| id as i32).map(|id| (id, by_id.get(&id).cloned().unwrap_or_default())).unwrap_or_default()).collect()
}

fn nfc_name(nfc: u8) -> &'static str {
    match nfc {
        1 => "upperRoman",
        2 => "lowerRoman",
        3 => "upperLetter",
        4 => "lowerLetter",
        22 => "decimalZero",
        _ => "decimal",
    }
}

/// A symbol font's character as the nearest Unicode one.
fn symbol(c: u16) -> char {
    match c & 0xFF {
        0xB7 | 0x9F | 0x6C => '•',
        0xA7 | 0x6E => '▪',
        0xFC => '✓',
        0xD8 | 0xE8 => '➢',
        0x71 | 0x6F => '□',
        0x76 => '❖',
        b if c < 0x100 || (0xF000..0xF100).contains(&c) => {
            if (0x20..0x7F).contains(&b) {
                b as u8 as char
            } else {
                '•'
            }
        }
        _ => char::from_u32(c as u32).unwrap_or('•'),
    }
}

// --- pictures -------------------------------------------------------------------

/// The picture of an Office Art record tree (PNG, JPEG or bitmap).
fn find_blip(d: &[u8], depth: u32) -> Option<Vec<u8>> {
    if depth > 16 {
        return None;
    }
    let mut p = 0;
    while p + 8 <= d.len() {
        let vi = u16_at(d, p)?;
        let ty = u16_at(d, p + 2)?;
        let len = u32_at(d, p + 4)? as usize;
        let body = d.get(p + 8..(p + 8).saturating_add(len).min(d.len()))?;
        let inst = vi >> 4;
        let found = match ty {
            _ if vi & 0xF == 0xF => find_blip(body, depth + 1),
            0xF007 => body.get(33).and_then(|&n| find_blip(body.get(36 + n as usize..)?, depth + 1)),
            0xF01D | 0xF02A => body.get(if matches!(inst, 0x46B | 0x6E3) { 33 } else { 17 }..).map(<[u8]>::to_vec),
            0xF01E => body.get(if inst == 0x6E1 { 33 } else { 17 }..).map(<[u8]>::to_vec),
            0xF01F => body.get(if inst == 0x7A9 { 33 } else { 17 }..).and_then(crate::images::bmp_from_dib),
            _ => None,
        };
        if found.is_some() {
            return found;
        }
        p = p.saturating_add(8 + len);
    }
    None
}

/// The picture kept at `at` in the Data stream, and its size in points.
fn read_picture(data: &[u8], at: u32) -> Option<(Vec<u8>, f32, f32)> {
    let d = data.get(at as usize..)?;
    let lcb = u32_at(d, 0)? as usize;
    let header = u16_at(d, 4)? as usize;
    let d = d.get(..lcb.min(d.len()))?;
    let mm = u16_at(d, 6)?;
    let (gx, gy) = (i16_at(d, 0x1C)? as f32, i16_at(d, 0x1E)? as f32);
    let (mx, my) = (u16_at(d, 0x20)? as f32, u16_at(d, 0x22)? as f32);
    let mut p = header;
    if mm == 0x66 {
        p += 1 + *d.get(p)? as usize;
    }
    let bytes = find_blip(d.get(p..)?, 0)?;
    let scale = |g: f32, m: f32| g * if m > 0.0 { m / 1000.0 } else { 1.0 } / TW;
    Some((bytes, scale(gx, mx), scale(gy, my)))
}

// --- the reader --------------------------------------------------------------------

struct Reader<'a> {
    /// Characters (UTF-16 units) by character position, and where each is
    /// in the WordDocument stream.
    text: Vec<u16>,
    fcs: Vec<u32>,
    chpx: Runs,
    papx: Runs,
    styles: Vec<Option<StyleDef>>,
    fonts: Vec<(Arc<str>, bool)>,
    default_font: Arc<str>,
    lists: Vec<(i32, Arc<Vec<Level>>)>,
    counters: HashMap<i32, [i32; 9]>,
    data: &'a [u8],
    /// Field start -> separator (if any) and end.
    fields: HashMap<u32, (Option<u32>, u32)>,
    pap_styles: HashMap<u16, Pap>,
    chp_styles: HashMap<u16, Chp>,
    styles_cache: HashMap<(usize, u16), (Chp, Arc<Style>)>,
    /// Pictures by where they're kept: their key and size.
    pics: HashMap<u32, Option<(Arc<str>, f32, f32)>>,
    blobs: HashMap<Arc<str>, Vec<u8>>,
    blob_bytes: u64,
    max_blobs: u64,
    blocks: usize,
    /// Characters the stories may still read (headers may name the
    /// whole text over and over).
    copy_left: usize,
    /// A Word 6.0/95 file: its pictures are in the WordDocument stream.
    word6: bool,
}

impl Reader<'_> {
    fn style_pap(&mut self, istd: u16, depth: u32) -> Pap {
        if depth == 0
            && let Some(p) = self.pap_styles.get(&istd)
        {
            return p.clone();
        }
        let mut p = Pap::default();
        if let Some(Some(s)) = self.styles.get(istd as usize) {
            let (base, papx) = (s.base, s.papx.clone());
            if base != 0x0FFF && depth < 10 {
                p = self.style_pap(base, depth + 1);
            }
            p.apply(&papx);
        }
        p.istd = istd;
        if depth == 0 {
            self.pap_styles.insert(istd, p.clone());
        }
        p
    }

    fn style_chp(&mut self, istd: u16, depth: u32) -> Chp {
        if depth == 0
            && let Some(c) = self.chp_styles.get(&istd)
        {
            return c.clone();
        }
        let mut c = Chp { size: 20, ..Default::default() };
        if let Some(Some(s)) = self.styles.get(istd as usize) {
            let (base, chpx) = (s.base, s.chpx.clone());
            if base != 0x0FFF && depth < 10 {
                c = self.style_chp(base, depth + 1);
            }
            let b = c.clone();
            c.apply(&chpx, &b);
        }
        if depth == 0 {
            self.chp_styles.insert(istd, c.clone());
        }
        c
    }

    fn pap_at(&mut self, cp: usize) -> Pap {
        let fc = self.fcs.get(cp).copied().unwrap_or(0);
        let Some(i) = find_run(&self.papx, fc) else { return self.style_pap(0, 0) };
        let g = self.papx[i].2.clone();
        let istd = u16_at(&g, 0).unwrap_or(0);
        let mut p = self.style_pap(istd, 0);
        let own = g.get(2..).unwrap_or(&[]);
        p.apply(own);
        // A list level's indents come before the paragraph's own.
        if p.ilfo > 0
            && let Some(lv) = self.level(&p)
        {
            let mut q = self.style_pap(istd, 0);
            q.apply(&lv.papx);
            q.apply(own);
            p = q;
        }
        p
    }

    fn level(&self, p: &Pap) -> Option<Level> {
        let (_, levels) = self.lists.get((p.ilfo as usize).checked_sub(1)?)?;
        levels.get(p.ilvl as usize).or(levels.first()).cloned()
    }

    /// The character formatting at `cp` and its style.
    fn chp_at(&mut self, cp: usize, istd: u16) -> (Chp, Arc<Style>) {
        let fc = self.fcs.get(cp).copied().unwrap_or(0);
        let run = find_run(&self.chpx, fc).unwrap_or(usize::MAX);
        if let Some(c) = self.styles_cache.get(&(run, istd)) {
            return c.clone();
        }
        let mut c = self.style_chp(istd, 0);
        if let Some(r) = self.chpx.get(run) {
            let g = r.2.clone();
            // A character style first, then the direct formatting.
            for (op, o) in sprms(&g) {
                if op == 0x4A30
                    && let Some(cs) = u16_at(o, 0)
                {
                    let mut chain = Vec::new();
                    let mut i = cs;
                    while let Some(Some(st)) = self.styles.get(i as usize)
                        && chain.len() < 10
                    {
                        chain.push(i);
                        i = st.base;
                    }
                    for i in chain.into_iter().rev() {
                        if let Some(Some(st)) = self.styles.get(i as usize) {
                            let b = c.clone();
                            let chpx = st.chpx.clone();
                            c.apply(&chpx, &b);
                        }
                    }
                }
            }
            let styled = c.clone();
            c.apply(&g, &styled);
        }
        let family = c.font.and_then(|f| self.fonts.get(f as usize)).map(|f| f.0.clone()).filter(|f| !f.is_empty()).unwrap_or_else(|| self.default_font.clone());
        let style = Arc::new(Style {
            family,
            size: c.size as f32 / 2.0,
            bold: c.bold,
            italic: c.italic,
            color: c.color.unwrap_or(Rgba::BLACK),
            underline: c.underline,
            strike: c.strike,
            highlight: c.highlight,
            shift: match c.iss {
                1 => 1,
                2 => -1,
                _ => 0,
            },
            caps: c.caps,
        });
        if self.styles_cache.len() > 100_000 {
            self.styles_cache.clear();
        }
        self.styles_cache.insert((run, istd), (c.clone(), style.clone()));
        (c, style)
    }

    /// The instruction of a field, without its nested fields' results.
    fn field_code(&self, start: usize, sep: usize) -> String {
        let mut s = String::new();
        let mut depth = 0;
        for cp in start + 1..sep.min(self.text.len()) {
            match self.text[cp] {
                0x13 => depth += 1,
                0x15 => depth -= 1,
                0x14 => {}
                u if depth == 0 && u >= 0x20 && s.len() < 256 => s.push(char::from_u32(u32::from(u)).unwrap_or(' ')),
                _ => {}
            }
        }
        s
    }

    fn picture(&mut self, at: u32) -> Option<(Arc<str>, f32, f32)> {
        if let Some(p) = self.pics.get(&at) {
            return p.clone();
        }
        let p = if self.word6 { ww6::picture(self.data, at) } else { read_picture(self.data, at) }.and_then(|(bytes, w, h)| {
            if !(w > 0.5 && h > 0.5 && w < 20_000.0 && h < 20_000.0) || self.blob_bytes + bytes.len() as u64 > self.max_blobs {
                return None;
            }
            self.blob_bytes += bytes.len() as u64;
            let key: Arc<str> = Arc::from(format!("pic{at}"));
            self.blobs.insert(key.clone(), bytes);
            Some((key, w, h))
        });
        if self.pics.len() < 10_000 {
            self.pics.insert(at, p.clone());
        }
        p
    }

    /// The label of a list paragraph, advancing its count.
    fn label(&mut self, p: &Pap) -> Option<String> {
        let (id, levels) = self.lists.get((p.ilfo as usize).checked_sub(1)?)?.clone();
        let l = (p.ilvl as usize).min(levels.len().checked_sub(1)?);
        let lv = &levels[l];
        let counts = self.counters.entry(id).or_insert([i32::MIN; 9]);
        for c in counts.iter_mut().skip(l + 1) {
            *c = i32::MIN;
        }
        counts[l] = if counts[l] == i32::MIN { lv.start } else { counts[l].saturating_add(1) };
        for (i, c) in counts.iter_mut().enumerate().take(l) {
            if *c == i32::MIN {
                *c = levels.get(i).map_or(1, |l| l.start);
            }
        }
        let counts = *counts;
        let mut out = String::new();
        match lv.nfc {
            0xFF => {}
            23 => out.push(lv.text.first().map_or('•', |&c| symbol(c))),
            _ => {
                for &c in &lv.text {
                    if c < 9 {
                        let i = c as usize;
                        out.push_str(&number(counts[i].max(0), nfc_name(levels.get(i).map_or(0, |l| l.nfc))));
                    } else if let Some(c) = char::from_u32(c as u32) {
                        out.push(if (0xF000..0xF100).contains(&(c as u32)) { symbol(c as u16) } else { c });
                    }
                }
            }
        }
        if out.is_empty() {
            return None;
        }
        match lv.follow {
            0 => out.push('\t'),
            1 => out.push(' '),
            _ => {}
        }
        Some(out)
    }

    fn para(&self, pap: &Pap, spans: Vec<Span>) -> Para {
        let line = match pap.line {
            Some((l, 1)) if l > 0 => LineRule::Multiple((l as f32 / 240.0).clamp(0.25, 10.0)),
            Some((l, _)) if l < 0 => LineRule::Exact(l.unsigned_abs() as f32 / TW),
            Some((l, _)) if l > 0 => LineRule::AtLeast(l as f32 / TW),
            _ => LineRule::Multiple(1.0),
        };
        let mut fmt = ParaFormat {
            align: match pap.jc {
                1 => Align::Center,
                2 => Align::Right,
                3..=5 => Align::Justify,
                _ => Align::Left,
            },
            left: pap.left as f32 / TW,
            right: pap.right as f32 / TW,
            first: pap.first as f32 / TW,
            line,
            tabs: pap.tabs.clone(),
        };
        if fmt.first < 0.0 && !fmt.tabs.iter().any(|t| t.pos == fmt.left) {
            let at = fmt.tabs.partition_point(|t| t.pos < fmt.left);
            fmt.tabs.insert(at, Tab { pos: fmt.left, align: TabAlign::Left, leader: None });
        }
        Para {
            fmt,
            spans,
            before: (pap.before as f32 / TW).min(1584.0),
            after: (pap.after as f32 / TW).min(1584.0),
            page_break_before: pap.page_break,
            break_after: false,
            shading: pap.shading,
            contextual: pap.contextual,
            style: pap.istd.to_string(),
            floats: Vec::new(),
            section_end: None,
        }
    }

    /// The blocks of the characters `a..b` of a story.
    fn story(&mut self, a: usize, b: usize) -> Vec<Block> {
        let b = b.min(self.text.len());
        let mut out: Vec<Block> = Vec::new();
        let n = b.saturating_sub(a);
        if n > self.copy_left {
            return out;
        }
        self.copy_left -= n;
        let mut rows: Vec<(Vec<Vec<Block>>, RowDef)> = Vec::new();
        let mut row: Vec<Vec<Block>> = Vec::new();
        let mut cell: Vec<Block> = Vec::new();
        let mut skip_to = 0usize;
        let mut break_before = false;
        let mut start = a;
        while start < b {
            if self.blocks >= MAX_BLOCKS {
                break;
            }
            self.blocks += 1;
            let end = (start..b).find(|&cp| matches!(self.text[cp], 0x0D | 0x07)).unwrap_or(b);
            let pap = self.pap_at(end.min(b.saturating_sub(1)));
            let mark = self.text.get(end).copied().unwrap_or(0x0D);
            let mut paras: Vec<Para> = Vec::new();
            let mut spans: Vec<Span> = Vec::new();
            let mut first_style: Option<Arc<Style>> = None;
            let mut cp = start;
            while cp < end {
                if cp < skip_to {
                    cp += 1;
                    continue;
                }
                let u = self.text[cp];
                let (chp, style) = self.chp_at(cp, pap.istd);
                first_style.get_or_insert_with(|| style.clone());
                if chp.deleted || chp.hidden && u != 0x0C {
                    cp += 1;
                    continue;
                }
                let push = |spans: &mut Vec<Span>, s: &str| {
                    if let Some(last) = spans.last_mut()
                        && last.object.is_none()
                        && Arc::ptr_eq(&last.style, &style)
                    {
                        last.text.push_str(s);
                    } else if spans.len() < 100_000 {
                        spans.push(Span::new(s, style.clone()));
                    }
                };
                match u {
                    0x13 => {
                        let (sep, fend) = self.fields.get(&(cp as u32)).copied().unwrap_or((None, cp as u32));
                        let (sep, fend) = (sep.map(|s| s as usize), fend as usize);
                        let code = self.field_code(cp, sep.unwrap_or(fend));
                        let word = code.split_whitespace().next().unwrap_or("").to_ascii_uppercase();
                        match word.as_str() {
                            "PAGE" => {
                                push(&mut spans, &PAGE.to_string());
                                skip_to = fend + 1;
                            }
                            "NUMPAGES" | "SECTIONPAGES" => {
                                push(&mut spans, &PAGES.to_string());
                                skip_to = fend + 1;
                            }
                            _ => skip_to = sep.map_or(fend + 1, |s| s + 1),
                        }
                    }
                    0x14 | 0x15 => {}
                    0x01 if chp.special => {
                        if let Some((key, w, h)) = chp.pic.and_then(|at| self.picture(at))
                            && spans.len() < 100_000
                        {
                            spans.push(Span { text: String::new(), style: style.clone(), object: Some(Object { w, h, key }) });
                        }
                    }
                    0x0C => {
                        // A page or section break: what follows starts a new
                        // paragraph on a new page.
                        if spans.is_empty() {
                            break_before = true;
                        } else {
                            let mut p = self.para(&pap, std::mem::take(&mut spans));
                            p.page_break_before |= std::mem::take(&mut break_before);
                            p.break_after = true;
                            paras.push(p);
                        }
                    }
                    0x0B | 0x0E => push(&mut spans, "\n"),
                    0x1E => push(&mut spans, "\u{2011}"),
                    0x09 => push(&mut spans, "\t"),
                    0x28 if chp.special && chp.sym.is_some() => {
                        let c = symbol(chp.sym.unwrap_or(0xB7));
                        push(&mut spans, c.encode_utf8(&mut [0; 4]));
                    }
                    u if u < 0x20 => {}
                    _ => {
                        let c = if (0xD800..0xDC00).contains(&u)
                            && let Some(&l) = self.text.get(cp + 1)
                            && (0xDC00..0xE000).contains(&l)
                        {
                            cp += 1;
                            char::decode_utf16([u, l]).next().and_then(Result::ok).unwrap_or('\u{FFFD}')
                        } else if (0xF000..0xF100).contains(&u) {
                            symbol(u)
                        } else {
                            char::from_u32(u32::from(u)).unwrap_or('\u{FFFD}')
                        };
                        push(&mut spans, c.encode_utf8(&mut [0; 4]));
                    }
                }
                cp += 1;
            }
            let style = match first_style {
                Some(s) => s,
                None => self.chp_at(end.min(b.saturating_sub(1)), pap.istd).1,
            };
            if !spans.is_empty() || paras.is_empty() {
                if spans.is_empty() {
                    spans.push(Span::new("", style.clone()));
                }
                paras.push(self.para(&pap, spans));
            }
            // The list label goes before the paragraph's first text.
            let label = match pap.ilfo {
                0 if self.word6 && pap.nlvlanm == 11 => Some("•\t".to_string()),
                0 => None,
                _ => self.label(&pap),
            };
            if let Some(label) = label
                && let Some(p) = paras.first_mut()
            {
                p.spans.insert(0, Span::new(label, style.clone()));
            }
            if let Some(p) = paras.first_mut() {
                p.page_break_before |= std::mem::take(&mut break_before);
            }
            let in_table = pap.in_table || pap.itap > 0;
            if in_table && pap.inner_ttp {
                // The end of a nested table's row: its cells stay as
                // paragraphs of the outer cell.
            } else if in_table && pap.ttp {
                if !cell.is_empty() {
                    row.push(std::mem::take(&mut cell));
                }
                if rows.len() < 32_767 {
                    rows.push((std::mem::take(&mut row), pap.row.clone()));
                } else {
                    row.clear();
                }
            } else if in_table {
                cell.extend(paras.into_iter().map(Block::Para));
                if mark == 0x07 && pap.itap <= 1 {
                    row.push(std::mem::take(&mut cell));
                }
            } else {
                flush_table(&mut out, &mut rows);
                out.extend(paras.into_iter().map(Block::Para));
            }
            start = end + 1;
        }
        if !cell.is_empty() {
            row.push(cell);
        }
        if !row.is_empty() {
            rows.push((row, RowDef::default()));
        }
        flush_table(&mut out, &mut rows);
        out
    }
}

/// Adds the table read so far to `out`.
fn flush_table(out: &mut Vec<Block>, rows: &mut Vec<(Vec<Vec<Block>>, RowDef)>) {
    if rows.is_empty() {
        return;
    }
    let rows = std::mem::take(rows);
    // Column edges and cell descriptions (`TC80`s) from the definitions.
    let defs: Vec<(Vec<i16>, Vec<Vec<u8>>)> = rows
        .iter()
        .map(|(cells, def)| {
            let t = &def.tdef[..];
            let n = (t.first().copied().unwrap_or(0) as usize).min(MAX_CELLS);
            let edges: Vec<i16> = (0..=n).filter_map(|i| i16_at(t, 1 + i * 2)).collect();
            let tcs: Vec<Vec<u8>> = (0..n).map(|i| t.get(1 + (n + 1) * 2 + i * 20..).map_or(Vec::new(), |c| c[..c.len().min(20)].to_vec())).collect();
            // Without a definition, an inch per cell.
            if edges.len() == n + 1 && n > 0 { (edges, tcs) } else { ((0..=cells.len().min(MAX_CELLS) as i16).map(|i| i * 1440).collect(), vec![Vec::new(); cells.len().min(MAX_CELLS)]) }
        })
        .collect();
    let mut edges: Vec<i16> = defs.iter().flat_map(|d| d.0.iter().copied()).collect();
    edges.sort_unstable();
    edges.dedup_by(|a, b| (i32::from(*a) - i32::from(*b)).abs() < 30);
    if edges.len() < 2 {
        edges = vec![0, 1440];
    }
    edges.truncate(MAX_CELLS * 2 + 1);
    let ncols = edges.len() - 1;
    let col_of = |x: i16| edges.iter().enumerate().min_by_key(|(_, e)| (i32::from(**e) - i32::from(x)).abs()).map_or(0, |(i, _)| i);
    let first = &rows[0].1;
    let table_borders = first.borders;
    let align = match first.jc {
        1 => Align::Center,
        2 => Align::Right,
        _ => Align::Left,
    };
    let nrows = rows.len();
    let mut out_rows = Vec::with_capacity(nrows);
    for (r, ((cells, def), (dx, tcs))) in rows.into_iter().zip(defs).enumerate() {
        let gap = def.gap.unwrap_or(108).clamp(0, 1440) as f32 / TW;
        let mut row: Vec<Cell> = Vec::new();
        let mut at = 0;
        for (i, blocks) in cells.into_iter().enumerate() {
            let (Some(&x0), Some(&x1)) = (dx.get(i), dx.get(i + 1)) else { continue };
            let c0 = col_of(x0).max(at);
            if c0 >= ncols {
                continue;
            }
            let c1 = col_of(x1).max(c0 + 1).min(ncols);
            if c0 > at {
                row.push(Cell { span: c0 - at, merged: false, blocks: Vec::new(), fill: None, borders: [None; 4], margins: [0.0; 4], valign: 0 });
            }
            at = c1;
            let tc = tcs.get(i).map_or(&[][..], Vec::as_slice);
            let f = u16_at(tc, 0).unwrap_or(0);
            if f & 2 != 0
                && let Some(prev) = row.last_mut()
            {
                prev.span += c1 - c0;
                continue;
            }
            // Borders: the table's (outer or inside), the cell's own, then
            // the ones set on ranges of cells.
            let mut borders: [Border; 4] = [None; 4];
            if let Some(tb) = table_borders {
                borders[0] = if r == 0 { tb[0] } else { tb[4] };
                borders[2] = if r + 1 == nrows { tb[2] } else { tb[4] };
                borders[1] = if c0 == 0 { tb[1] } else { tb[5] };
                borders[3] = if c1 >= ncols { tb[3] } else { tb[5] };
            }
            if tc.len() == 20 {
                for (k, b) in borders.iter_mut().enumerate() {
                    if let Some(Some(own)) = brc80(tc.get(4 + k * 4..).unwrap_or(&[])) {
                        *b = Some(own);
                    }
                }
            }
            for &(a, z, which, brc) in &def.set_brc {
                if (a as usize..z as usize).contains(&i) {
                    for (k, b) in borders.iter_mut().enumerate() {
                        if which & (1 << k) != 0 {
                            *b = brc;
                        }
                    }
                }
            }
            let merged = f & 0x20 != 0 && f & 0x40 == 0 && r > 0;
            row.push(Cell {
                span: c1 - c0,
                merged,
                blocks: if merged { Vec::new() } else { blocks },
                fill: def.shd.get(i).copied().flatten(),
                borders,
                margins: [0.0, gap, 0.0, gap],
                valign: ((f >> 7) & 3) as u8,
            });
        }
        if row.is_empty() {
            continue;
        }
        out_rows.push(Row { cells: row, height: def.height.unsigned_abs() as f32 / TW, exact: def.height < 0, header: def.header });
    }
    if out_rows.is_empty() {
        return;
    }
    let cols = edges.windows(2).map(|w| ((f32::from(w[1]) - f32::from(w[0])) / TW).max(1.0)).collect();
    out.push(Block::Table(Table { cols, rows: out_rows, indent: (f32::from(edges[0]) / TW).clamp(-720.0, 720.0), align }));
}

/// A section's page setup from its property list.
/// A section's page setup, and (Word 6) which header stories it has.
fn section(g: &[u8]) -> (Section, u8) {
    let (mut w, mut h) = (12240.0f32, 15840.0f32);
    let (mut l, mut r, mut t, mut bm) = (1800.0f32, 1800.0f32, 1440.0f32, 1440.0f32);
    let (mut hd, mut fd) = (720.0f32, 720.0f32);
    let (mut title_page, mut new_page, mut restart, mut start) = (false, true, false, 1usize);
    let mut hdr_bits = 0u8;
    for (op, o) in sprms(g) {
        let v = u16_at(o, 0).unwrap_or(0) as f32;
        let s = i16_at(o, 0).unwrap_or(0) as f32;
        let b = o.first().copied().unwrap_or(0);
        match op {
            0xB01F => w = v,
            0xB020 => h = v,
            0xB021 => l = v,
            0xB022 => r = v,
            0x9023 => t = s.abs(),
            0x9024 => bm = s.abs(),
            0xB017 => hd = v,
            0xB018 => fd = v,
            0x300A => title_page = b != 0,
            0x3009 => new_page = b != 0,
            0x3011 => restart = b != 0,
            0x501C => start = u16_at(o, 0).unwrap_or(1) as usize,
            0x3014 => hdr_bits = b,
            _ => {}
        }
    }
    let (mut width, mut height) = (w / TW, h / TW);
    if !(width > 72.0 && width < 14_400.0) {
        width = 612.0;
    }
    if !(height > 72.0 && height < 14_400.0) {
        height = 792.0;
    }
    let mut m = [t / TW, r / TW, bm / TW, l / TW].map(|m| m.clamp(0.0, 1440.0));
    if m[1] + m[3] > width - 72.0 {
        m[1] = 18.0;
        m[3] = 18.0;
    }
    if m[0] + m[2] > height - 72.0 {
        m[0] = 18.0;
        m[2] = 18.0;
    }
    (
        Section {
            width,
            height,
            margins: m,
            header_dist: (hd / TW).min(720.0),
            footer_dist: (fd / TW).min(720.0),
            header: None,
            footer: None,
            first_header: None,
            first_footer: None,
            page_start: restart.then_some(start),
            title_page,
            new_page,
        },
        hdr_bits,
    )
}

/// Reads a .doc file: its model and its pictures' bytes.
fn read(data: &[u8], limits: &Limits) -> Result<(DocModel, HashMap<Arc<str>, Vec<u8>>), Error> {
    let ole = Ole::open(data, limits)?;
    let wd = ole.stream("WordDocument").ok_or(Error::Unsupported)?;
    let (fib, info) = Fib::read(&wd)?;
    let flags = u16_at(&wd, 0x0A).unwrap_or(0);
    if flags & 0x0100 != 0 {
        return Err(Error::Encrypted);
    }
    // Word 6/95 keeps everything in the WordDocument stream.
    let (table, data_stream) = match info {
        Some(_) => (wd.clone(), Vec::new()),
        None => (ole.stream(if flags & 0x0200 != 0 { "1Table" } else { "0Table" }).ok_or_else(|| bad("the table stream is missing"))?, ole.stream("Data").unwrap_or_default()),
    };
    let (text, fcs) = match &info {
        Some(i) => ww6::text(&fib, i, &wd)?,
        None => text97(&fib, &table, &wd)?,
    };
    let ((fonts, sets), chpx, papx) = match &info {
        Some(i) => (ww6::read_fonts(fib.part(&table, STTBF_FFN), i.cp), ww6::fkps(fib.part(&table, PLCF_BTE_CHPX), i.chp_pages, &wd, false), ww6::fkps(fib.part(&table, PLCF_BTE_PAPX), i.pap_pages, &wd, true)),
        None => ((read_fonts(fib.part(&table, STTBF_FFN)), Vec::new()), read_fkps(&bte_pages(fib.part(&table, PLCF_BTE_CHPX)), &wd, false, false), read_fkps(&bte_pages(fib.part(&table, PLCF_BTE_PAPX)), &wd, true, false)),
    };
    let mut styles = read_styles(fib.part(&table, STSHF), fib.word6);
    if fib.word6 {
        for st in styles.iter_mut().flatten() {
            st.papx = ww6::translate(&st.papx);
            st.chpx = ww6::translate(&st.chpx);
        }
    }
    let mut r = Reader {
        text,
        fcs,
        chpx,
        papx,
        styles,
        fonts,
        default_font: Arc::from("Times New Roman"),
        lists: read_lists(fib.fclcb.get(PLF_LST).and_then(|&(fc, _)| table.get(fc as usize..)).unwrap_or(&[]), fib.part(&table, PLF_LFO)),
        counters: HashMap::new(),
        data: if fib.word6 { &wd } else { &data_stream },
        fields: HashMap::new(),
        pap_styles: HashMap::new(),
        chp_styles: HashMap::new(),
        styles_cache: HashMap::new(),
        pics: HashMap::new(),
        blobs: HashMap::new(),
        blob_bytes: 0,
        max_blobs: limits.max_unpacked_bytes,
        blocks: 0,
        copy_left: 0,
        word6: fib.word6,
    };
    r.copy_left = 4 * r.text.len() + (1 << 20);
    // Without a font in Normal, the style sheet's standard one.
    let stsh = fib.part(&table, STSHF);
    let standard = u16_at(stsh, 0).filter(|&cb| cb >= 14).and_then(|_| u16_at(stsh, 14));
    if let Some(i) = &info {
        ww6::decode(&mut r, &sets, standard.unwrap_or(0), i);
        r.styles_cache.clear();
    }
    let normal = r.style_chp(0, 0).font.or(standard);
    if let Some(f) = normal.and_then(|f| r.fonts.get(f as usize)).filter(|f| !f.0.is_empty()) {
        r.default_font = f.0.clone();
    }
    // Fields: begin, separator and end marks.
    let mut stack: Vec<(u32, Option<u32>)> = Vec::new();
    for (cp, &u) in r.text.iter().enumerate() {
        match u {
            0x13 if stack.len() < 1024 => stack.push((cp as u32, None)),
            0x14 => {
                if let Some(f) = stack.last_mut() {
                    f.1 = Some(cp as u32);
                }
            }
            0x15 => {
                if let Some((s, sep)) = stack.pop() {
                    r.fields.insert(s, (sep, cp as u32));
                }
            }
            _ => {}
        }
    }
    let [ccp_text, ccp_ftn, ..] = fib.ccp.map(|v| v as usize);
    let ccp_text = ccp_text.min(r.text.len());
    // Sections: where each ends, and its setup.
    let (scps, seds) = plc(fib.part(&table, PLCF_SED), 12);
    let mut bounds = Vec::new();
    let mut sections = Vec::new();
    for (i, sed) in seds.iter().enumerate().take(10_000) {
        let fc = u32::from_le_bytes([sed[2], sed[3], sed[4], sed[5]]) as usize;
        let g = match u16_at(&wd, fc) {
            Some(cb) if fc != 0xFFFF_FFFF => wd.get(fc + 2..(fc + 2 + cb as usize).min(wd.len())).unwrap_or(&[]),
            _ => &[],
        };
        let g = if fib.word6 { ww6::translate(g) } else { g.to_vec() };
        sections.push(section(&g));
        let end = scps.get(i + 1).map_or(ccp_text, |&e| (e as usize).min(ccp_text));
        bounds.push(end);
    }
    if sections.is_empty() {
        sections.push(section(&[]));
        bounds.push(ccp_text);
    }
    if let Some(b) = bounds.last_mut() {
        *b = ccp_text;
    }
    // Headers and footers: after the six note separators, each section's
    // even and odd headers, even and odd footers, first page header and
    // footer. An empty one is the previous section's. Word 6 lists only
    // the separators and stories there are.
    let (hcps, _) = plc(fib.part(&table, PLCF_HDD), 0);
    let hdd = ccp_text + ccp_ftn;
    let mut prev: [Option<Arc<Vec<Block>>>; 6] = Default::default();
    let mut word6_at = fib.part(&table, DOP).get(1).copied().unwrap_or(0).count_ones() as usize;
    for (s, (sec, bits)) in sections.iter_mut().enumerate() {
        let mut got: [Option<Arc<Vec<Block>>>; 6] = Default::default();
        for (k, g) in got.iter_mut().enumerate() {
            let i = match fib.word6 {
                true if *bits & (1 << k) == 0 => usize::MAX,
                true => word6_at + (*bits & ((1 << k) - 1)).count_ones() as usize,
                false => 6 + s * 6 + k,
            };
            let (Some(&a), Some(&b)) = (hcps.get(i), hcps.get(i.saturating_add(1))) else {
                *g = prev[k].clone();
                continue;
            };
            if b <= a + 1 {
                *g = prev[k].clone();
                continue;
            }
            let mut blocks = r.story(hdd + a as usize, hdd + b as usize);
            // The story's last paragraph mark is a guard.
            if blocks.len() > 1 && matches!(blocks.last(), Some(Block::Para(p)) if p.spans.iter().all(|s| s.text.is_empty() && s.object.is_none())) {
                blocks.pop();
            }
            *g = Some(Arc::new(blocks));
        }
        sec.header = got[1].clone();
        sec.footer = got[3].clone();
        if sec.title_page {
            sec.first_header = got[4].clone().or_else(|| Some(Arc::new(Vec::new())));
            sec.first_footer = got[5].clone().or_else(|| Some(Arc::new(Vec::new())));
        }
        prev = got;
        if fib.word6 {
            word6_at += bits.count_ones() as usize;
        }
    }
    // The body, section by section.
    let mut body = Vec::new();
    let mut from = 0;
    let last = bounds.len() - 1;
    for (i, &end) in bounds.iter().enumerate() {
        let end = end.max(from);
        let mut blocks = r.story(from, end);
        from = end;
        if i < last {
            // The section break's own page break is the section's.
            if let Some(Block::Para(p)) = blocks.last_mut() {
                p.break_after = false;
            }
            match blocks.last_mut() {
                Some(Block::Para(p)) => p.section_end = Some(i),
                _ => {
                    let mut marker = Para {
                        fmt: ParaFormat::default(),
                        spans: vec![Span::new("", Arc::new(Style { size: 0.5, ..Style::default() }))],
                        before: 0.0,
                        after: 0.0,
                        page_break_before: false,
                        break_after: false,
                        shading: None,
                        contextual: false,
                        style: String::new(),
                        floats: Vec::new(),
                        section_end: Some(i),
                    };
                    marker.fmt.line = LineRule::Exact(0.1);
                    blocks.push(Block::Para(marker));
                }
            }
        }
        body.extend(blocks);
    }
    if body.is_empty() {
        body.push(Block::Para(r.para(&Pap::default(), vec![Span::new("", Arc::new(Style { family: r.default_font.clone(), size: 10.0, ..Style::default() }))])));
    }
    Ok((DocModel { body, sections: sections.into_iter().map(|s| s.0).collect(), background: None }, r.blobs))
}

/// The text of a Word 97 file through its piece table, and where each
/// character is in the WordDocument stream.
fn text97(fib: &Fib, table: &[u8], wd: &[u8]) -> Result<(Vec<u16>, Vec<u32>), Error> {
    // The piece table: where each run of text is stored.
    let clx = fib.part(table, CLX);
    let mut p = 0;
    let mut pieces = Vec::new();
    while p < clx.len() {
        match clx[p] {
            1 => p += 3 + i16_at(clx, p + 1).unwrap_or(0).max(0) as usize,
            2 => {
                let lcb = u32_at(clx, p + 1).unwrap_or(0) as usize;
                let (cps, pcds) = plc(clx.get(p + 5..(p + 5).saturating_add(lcb).min(clx.len())).unwrap_or(&[]), 8);
                for (i, pcd) in pcds.iter().enumerate() {
                    let fc = u32::from_le_bytes([pcd[2], pcd[3], pcd[4], pcd[5]]);
                    if let (Some(&a), Some(&b)) = (cps.get(i), cps.get(i + 1)) {
                        pieces.push((a, b, fc & 0x3FFF_FFFF, fc & 0x4000_0000 != 0));
                    }
                }
                break;
            }
            _ => break,
        }
    }
    if pieces.is_empty() {
        return Err(bad("the document has no text"));
    }
    let mut text = Vec::new();
    let mut fcs = Vec::new();
    // Pieces naming the same bytes over and over stop at about the text
    // the stream can hold.
    let most = wd.len() + (1 << 16);
    for (a, b, fc, compressed) in pieces {
        for i in 0..b.saturating_sub(a) {
            if text.len() >= most {
                break;
            }
            let (pos, u) = if compressed {
                let pos = (fc / 2).saturating_add(i);
                (pos, wd.get(pos as usize).map(|&b| cp1252(b)))
            } else {
                let pos = fc.saturating_add(i.saturating_mul(2));
                (pos, u16_at(wd, pos as usize))
            };
            let Some(u) = u else { break };
            text.push(u);
            fcs.push(pos);
        }
    }
    Ok((text, fcs))
}

fn cp1252(b: u8) -> u16 {
    const HIGH: [u16; 32] = [
        0x20AC, 0x81, 0x201A, 0x0192, 0x201E, 0x2026, 0x2020, 0x2021, 0x02C6, 0x2030, 0x0160, 0x2039, 0x0152, 0x8D, 0x017D, 0x8F, 0x90, 0x2018, 0x2019, 0x201C, 0x201D, 0x2022, 0x2013, 0x2014, 0x02DC, 0x2122, 0x0161, 0x203A, 0x0153, 0x9D, 0x017E, 0x0178,
    ];
    if (0x80..0xA0).contains(&b) { HIGH[(b - 0x80) as usize] } else { b as u16 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn property_lists() {
        // Bold on, 14 pt, a variable-length operand and justified.
        let g = [0x35, 0x08, 0x01, 0x43, 0x4A, 28, 0, 0x2A, 0xC6, 2, 9, 9, 0x61, 0x24, 3];
        let s = sprms(&g);
        assert_eq!(s.iter().map(|x| x.0).collect::<Vec<_>>(), vec![0x0835, 0x4A43, 0xC62A, 0x2461]);
        let mut c = Chp::default();
        c.apply(&g, &Chp::default());
        assert!(c.bold);
        assert_eq!(c.size, 28);
        let mut p = Pap::default();
        p.apply(&g);
        assert_eq!(p.jc, 3);
        assert!(!toggle(0x81, true));
        assert!(toggle(0x80, true));
    }
}
