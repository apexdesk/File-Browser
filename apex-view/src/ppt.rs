//! PowerPoint 97-2003 presentations (.ppt, .pps, .pot): one page per slide,
//! notes left out. Opening follows the last save (the persist directory of
//! its user edits) to the list of slides; each slide is drawn when asked
//! for: its background, its master's shapes, then its own, the text in the
//! styles the master gives each kind of text.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;

use vello_cpu::kurbo::{Affine, BezPath, Point, Shape};

use crate::draw::{self, Item, Page, Rgba};
use crate::ole::Ole;
use crate::ooxml::Pictures;
use crate::text::TextRun;
use crate::typeset::{self, Align, FontCache, LineRule, Lines, ParaFormat, Span, Style, Tab, TabAlign};
use crate::{Error, Fonts, Image, Limits};

/// Slides kept laid out.
const KEEP: usize = 4;
const MAX_SHAPES: usize = 5_000;
const MAX_PARAS: usize = 2_000;
/// Text units read of one text.
const MAX_TEXT: usize = 65_536;
/// Master units (1/576 inch) and EMUs to points.
const MU: f32 = 0.125;
const EMU: f32 = 1.0 / 12_700.0;

// Record types.
const DOCUMENT: u16 = 0x03E8;
const DOCUMENT_ATOM: u16 = 0x03E9;
const SLIDE: u16 = 0x03EE;
const SLIDE_ATOM: u16 = 0x03EF;
const ENVIRONMENT: u16 = 0x03F2;
const SLIDE_PERSIST: u16 = 0x03F3;
const MAIN_MASTER: u16 = 0x03F8;
const DRAWING_GROUP: u16 = 0x040B;
const DRAWING: u16 = 0x040C;
const COLOR_SCHEME: u16 = 0x07F0;
const FONT_COLLECTION: u16 = 0x07D5;
const PLACEHOLDER: u16 = 0x0BC3;
const OUTLINE_REF: u16 = 0x0F9E;
const TEXT_HEADER: u16 = 0x0F9F;
const TEXT_CHARS: u16 = 0x0FA0;
const STYLE_TEXT_PROP: u16 = 0x0FA1;
const MASTER_STYLE: u16 = 0x0FA3;
const CF_EXCEPTION: u16 = 0x0FA4;
const PF_EXCEPTION: u16 = 0x0FA5;
const TEXT_RULER: u16 = 0x0FA6;
const TEXT_BYTES: u16 = 0x0FA8;
const FONT_ENTITY: u16 = 0x0FB7;
const SLIDE_LIST: u16 = 0x0FF0;
const USER_EDIT: u16 = 0x0FF5;
const PERSIST_DIR: u16 = 0x1772;
const DGG: u16 = 0xF000;
const BSTORE: u16 = 0xF001;
const DG: u16 = 0xF002;
const SP_GROUP: u16 = 0xF003;
const SP: u16 = 0xF004;
const BSE: u16 = 0xF007;
const SP_GROUP_ATOM: u16 = 0xF009;
const SHAPE_ATOM: u16 = 0xF00A;
const OPT: u16 = 0xF00B;
const TEXTBOX: u16 = 0xF00D;
const CHILD_ANCHOR: u16 = 0xF00F;
const CLIENT_ANCHOR: u16 = 0xF010;
const CLIENT_DATA: u16 = 0xF011;
const TERTIARY_OPT: u16 = 0xF122;

/// The Current User token of an encrypted file.
const ENCRYPTED: u32 = 0xF3D1_C4DF;

fn u16_at(d: &[u8], p: usize) -> Option<u16> {
    Some(u16::from_le_bytes(d.get(p..p.checked_add(2)?)?.try_into().ok()?))
}

fn u32_at(d: &[u8], p: usize) -> Option<u32> {
    Some(u32::from_le_bytes(d.get(p..p.checked_add(4)?)?.try_into().ok()?))
}

fn i32_at(d: &[u8], p: usize) -> Option<i32> {
    u32_at(d, p).map(|v| v as i32)
}

// --- records ------------------------------------------------------------

/// One record: its type, instance and body, and where the body starts in
/// the stream.
#[derive(Clone, Copy)]
struct Rec<'a> {
    typ: u16,
    inst: u16,
    at: usize,
    body: &'a [u8],
}

/// The record starting at `p` of `d`, which starts at `base` in its stream.
fn rec_at(d: &[u8], p: usize, base: usize) -> Option<Rec<'_>> {
    let vi = u16_at(d, p)?;
    let typ = u16_at(d, p + 2)?;
    let len = u32_at(d, p + 4)? as usize;
    let body = d.get(p + 8..(p + 8).saturating_add(len).min(d.len()))?;
    Some(Rec { typ, inst: vi >> 4, at: base + p + 8, body })
}

/// The records one after another in `d`.
fn recs(d: &[u8], base: usize) -> impl Iterator<Item = Rec<'_>> {
    let mut p = 0;
    std::iter::from_fn(move || {
        let r = rec_at(d, p, base)?;
        p += 8 + r.body.len();
        Some(r)
    })
}

fn kids<'a>(r: Rec<'a>) -> impl Iterator<Item = Rec<'a>> {
    recs(r.body, r.at)
}

fn child(r: Rec<'_>, typ: u16) -> Option<Rec<'_>> {
    kids(r).find(|k| k.typ == typ)
}

/// The offsets of the persist objects of the last save, and the persist
/// id of the document.
fn persist_dir(doc: &[u8], start: usize) -> Option<(HashMap<u32, usize>, u32)> {
    let mut edit = start;
    let mut dir = HashMap::new();
    let mut seen = HashSet::new();
    let mut dirs = HashSet::new();
    let mut doc_ref = None;
    // Directories may overlap: words read, at most a few times the stream's.
    let mut words = doc.len() + 1024;
    while seen.insert(edit) {
        let e = rec_at(doc, edit, 0).filter(|r| r.typ == USER_EDIT)?;
        doc_ref.get_or_insert(u32_at(e.body, 16)?);
        if let Some(pd) = u32_at(e.body, 12).filter(|&o| dirs.insert(o)).and_then(|o| rec_at(doc, o as usize, 0)).filter(|r| r.typ == PERSIST_DIR) {
            let mut p = 0;
            while let Some(info) = u32_at(pd.body, p) {
                let (first, n) = (info & 0xF_FFFF, info >> 20);
                p += 4;
                words = words.saturating_sub(n as usize + 1);
                if words == 0 {
                    break;
                }
                for i in 0..n {
                    if let Some(off) = u32_at(pd.body, p) {
                        dir.entry(first + i).or_insert(off as usize);
                    }
                    p += 4;
                }
            }
        }
        match u32_at(e.body, 8) {
            Some(0) | None => break,
            Some(o) => edit = o as usize,
        }
    }
    Some((dir, doc_ref?))
}

// --- text and its styles -------------------------------------------------

/// A text: its kind (title, body...), UTF-16 units, and the bytes of its
/// style and ruler atoms.
#[derive(Default)]
struct Txt {
    kind: u32,
    units: Vec<u16>,
    style: Vec<u8>,
    ruler: Vec<u8>,
}

/// Adds what `r` says to the texts read so far: a header starts a text.
fn take_text(r: Rec, out: &mut Vec<Txt>) {
    match r.typ {
        TEXT_HEADER => out.push(Txt { kind: u32_at(r.body, 0).unwrap_or(4), ..Txt::default() }),
        TEXT_CHARS => {
            if let Some(t) = out.last_mut() {
                t.units = r.body.chunks_exact(2).take(MAX_TEXT).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
            }
        }
        TEXT_BYTES => {
            if let Some(t) = out.last_mut() {
                let (s, _) = encoding_rs::WINDOWS_1252.decode_without_bom_handling(&r.body[..r.body.len().min(MAX_TEXT)]);
                t.units = s.encode_utf16().collect();
            }
        }
        STYLE_TEXT_PROP => {
            if let Some(t) = out.last_mut() {
                t.style = r.body.to_vec();
            }
        }
        TEXT_RULER => {
            if let Some(t) = out.last_mut() {
                t.ruler = r.body.to_vec();
            }
        }
        _ => {}
    }
}

/// Paragraph properties, those given.
#[derive(Clone, Default)]
struct Pf {
    bullet: Option<bool>,
    bullet_has_font: Option<bool>,
    bullet_has_color: Option<bool>,
    bullet_has_size: Option<bool>,
    bullet_char: Option<u16>,
    bullet_font: Option<u16>,
    bullet_size: Option<i16>,
    bullet_color: Option<u32>,
    align: Option<u16>,
    line: Option<i16>,
    before: Option<i16>,
    after: Option<i16>,
    left: Option<u16>,
    indent: Option<u16>,
}

/// Character properties, those given.
#[derive(Clone, Default)]
struct Cf {
    bold: Option<bool>,
    italic: Option<bool>,
    underline: Option<bool>,
    font: Option<u16>,
    size: Option<u16>,
    color: Option<u32>,
    position: Option<i16>,
}

macro_rules! over {
    ($a:expr, $b:expr, $($f:ident),*) => { $( if $b.$f.is_some() { $a.$f = $b.$f; } )* };
}

impl Pf {
    fn over(&mut self, o: &Pf) {
        over!(self, o, bullet, bullet_has_font, bullet_has_color, bullet_has_size, bullet_char, bullet_font, bullet_size, bullet_color, align, line, before, after, left, indent);
    }
}

impl Cf {
    fn over(&mut self, o: &Cf) {
        over!(self, o, bold, italic, underline, font, size, color, position);
    }
}

/// Reads values one after another.
struct Cur<'a> {
    d: &'a [u8],
    p: usize,
}

impl Cur<'_> {
    fn u16(&mut self) -> Option<u16> {
        let v = u16_at(self.d, self.p)?;
        self.p += 2;
        Some(v)
    }

    fn u32(&mut self) -> Option<u32> {
        let v = u32_at(self.d, self.p)?;
        self.p += 4;
        Some(v)
    }

    fn pf(&mut self) -> Option<Pf> {
        let m = self.u32()?;
        let mut f = Pf::default();
        if m & 0xF != 0 {
            let b = self.u16()?;
            let bit = |i: u32| (m & (1 << i) != 0).then_some(b & (1 << i) != 0);
            (f.bullet, f.bullet_has_font, f.bullet_has_color, f.bullet_has_size) = (bit(0), bit(1), bit(2), bit(3));
        }
        let has = |i: u32| m & (1 << i) != 0;
        if has(7) {
            f.bullet_char = Some(self.u16()?);
        }
        if has(4) {
            f.bullet_font = Some(self.u16()?);
        }
        if has(6) {
            f.bullet_size = Some(self.u16()? as i16);
        }
        if has(5) {
            f.bullet_color = Some(self.u32()?);
        }
        if has(11) {
            f.align = Some(self.u16()?);
        }
        if has(12) {
            f.line = Some(self.u16()? as i16);
        }
        if has(13) {
            f.before = Some(self.u16()? as i16);
        }
        if has(14) {
            f.after = Some(self.u16()? as i16);
        }
        if has(8) {
            f.left = Some(self.u16()?);
        }
        if has(10) {
            f.indent = Some(self.u16()?);
        }
        if has(15) {
            self.u16()?;
        }
        if has(20) {
            let n = self.u16()? as usize;
            self.p = self.p.checked_add(n * 4).filter(|&p| p <= self.d.len())?;
        }
        if has(16) {
            self.u16()?;
        }
        if m & 0xE_0000 != 0 {
            self.u16()?;
        }
        if has(21) {
            self.u16()?;
        }
        Some(f)
    }

    fn cf(&mut self) -> Option<Cf> {
        let m = self.u32()?;
        let mut f = Cf::default();
        if m & 0xFFFF != 0 {
            let s = self.u16()?;
            let bit = |i: u32| (m & (1 << i) != 0).then_some(s & (1 << i) != 0);
            (f.bold, f.italic, f.underline) = (bit(0), bit(1), bit(2));
        }
        let has = |i: u32| m & (1 << i) != 0;
        if has(16) {
            f.font = Some(self.u16()?);
        }
        for i in [21, 22, 23] {
            if has(i) {
                self.u16()?;
            }
        }
        if has(17) {
            f.size = Some(self.u16()?);
        }
        if has(18) {
            f.color = Some(self.u32()?);
        }
        if has(19) {
            f.position = Some(self.u16()? as i16);
        }
        Some(f)
    }
}

/// The paragraph and character runs of a style atom for `len` units:
/// each run's end, and its properties (paragraphs with their level).
fn style_runs(d: &[u8], len: usize) -> (Vec<(usize, u16, Pf)>, Vec<(usize, Cf)>) {
    let mut c = Cur { d, p: 0 };
    let (mut pfs, mut cfs) = (Vec::new(), Vec::new());
    let mut end = 0usize;
    while end <= len && pfs.len() < MAX_PARAS {
        let Some(n) = c.u32() else { break };
        let Some(level) = c.u16() else { break };
        let Some(pf) = c.pf() else { break };
        end = end.saturating_add((n as usize).max(1));
        pfs.push((end, level, pf));
    }
    end = 0;
    while end <= len && cfs.len() < 10_000 {
        let Some(n) = c.u32() else { break };
        let Some(cf) = c.cf() else { break };
        end = end.saturating_add((n as usize).max(1));
        cfs.push((end, cf));
    }
    (pfs, cfs)
}

/// The left margins and indents of a ruler atom, by level.
fn ruler(d: &[u8]) -> [Pf; 5] {
    let mut out: [Pf; 5] = Default::default();
    read_ruler(&mut Cur { d, p: 0 }, &mut out);
    out
}

fn read_ruler(c: &mut Cur, out: &mut [Pf; 5]) -> Option<()> {
    let m = c.u32()?;
    let has = |i: u32| m & (1 << i) != 0;
    if has(1) {
        c.u16()?;
    }
    if has(0) {
        c.u16()?;
    }
    if has(2) {
        let n = c.u16()? as usize;
        c.p = c.p.checked_add(n * 4)?;
    }
    for (l, o) in out.iter_mut().enumerate() {
        if has(3 + l as u32) {
            o.left = Some(c.u16()?);
        }
        if has(8 + l as u32) {
            o.indent = Some(c.u16()?);
        }
    }
    Some(())
}

/// The styles of each kind of text, by level, a master gives.
#[derive(Default)]
struct Styles {
    base: (Pf, Cf),
    kinds: [Vec<(Pf, Cf)>; 9],
}

impl Styles {
    fn read_atom(&mut self, r: Rec) {
        let kind = r.inst as usize;
        if kind > 8 {
            return;
        }
        let mut c = Cur { d: r.body, p: 0 };
        let n = c.u16().unwrap_or(0).min(5);
        let mut levels = Vec::new();
        for _ in 0..n {
            if kind >= 5 && c.u16().is_none() {
                break;
            }
            let Some(pf) = c.pf() else { break };
            let Some(cf) = c.cf() else { break };
            levels.push((pf, cf));
        }
        self.kinds[kind] = levels;
    }

    /// The paragraph and character properties of text `kind` at `level`.
    fn level(&self, env: &Styles, kind: u32, level: usize) -> (Pf, Cf) {
        let (mut pf, mut cf) = env.base.clone();
        let chain: &[usize] = match kind {
            0 => &[0],
            6 => &[0, 6],
            1 => &[1],
            5 | 7 | 8 => &[1, kind as usize],
            2 => &[2],
            _ => &[4],
        };
        let mut apply = |levels: &[(Pf, Cf)]| {
            for l in [0, level] {
                if let Some((p, c)) = levels.get(l) {
                    pf.over(p);
                    cf.over(c);
                }
                if level == 0 {
                    break;
                }
            }
        };
        apply(&env.kinds[4]);
        for &k in chain {
            let levels = if self.kinds[k].is_empty() { &env.kinds[k] } else { &self.kinds[k] };
            apply(levels);
        }
        (pf, cf)
    }
}

/// A colour of a text property: a scheme index or its own.
fn index_color(v: u32, scheme: &[Rgba; 8]) -> Option<Rgba> {
    match v >> 24 {
        0xFE => Some(Rgba([v as u8, (v >> 8) as u8, (v >> 16) as u8, 255])),
        i @ 0..8 => Some(scheme[i as usize]),
        _ => None,
    }
}

/// The bullet character drawn for `c` in font `font`.
fn bullet_char(c: u16, font: &str) -> char {
    let f = font.to_ascii_lowercase();
    let symbol = f.contains("wingdings") || f.contains("symbol") || f.contains("sorts") || f.contains("webdings");
    let low = if (0xF000..=0xF0FF).contains(&c) { c - 0xF000 } else { c };
    if symbol || low != c {
        return match low as u8 {
            b'n' | 0xA7 if f.contains("wingdings") => '\u{25AA}',
            b'q' | b'o' | b'p' => '\u{25A1}',
            b'v' | b'u' | 0xD8 | 0xE0 | 0xE8 | 0xF0 => '\u{25BA}',
            b'-' => '\u{2013}',
            _ => '\u{2022}',
        };
    }
    char::from_u32(c as u32).filter(|c| !c.is_control() && *c != '\u{FFFD}').unwrap_or('\u{2022}')
}

// --- shape properties ------------------------------------------------------

/// The properties of a shape: simple values and complex data by id.
#[derive(Default)]
struct Opt {
    simple: HashMap<u16, u32>,
    complex: HashMap<u16, Vec<u8>>,
}

impl Opt {
    fn read(&mut self, r: Rec) {
        let n = (r.inst as usize).min(r.body.len() / 6);
        let mut extra = n * 6;
        for i in 0..n {
            let (Some(id), Some(v)) = (u16_at(r.body, i * 6), u32_at(r.body, i * 6 + 2)) else { break };
            let pid = id & 0x3FFF;
            if id & 0x8000 != 0 {
                let end = extra.saturating_add(v as usize).min(r.body.len());
                self.complex.insert(pid, r.body.get(extra..end).unwrap_or_default().to_vec());
                extra = end;
            } else {
                self.simple.insert(pid, v);
            }
        }
    }

    fn get(&self, pid: u16) -> Option<u32> {
        self.simple.get(&pid).copied()
    }

    /// Bit `bit` of the boolean property `pid`, when it is given (files
    /// with no "use" bits give every bit).
    fn flag(&self, pid: u16, bit: u32) -> Option<bool> {
        let v = self.get(pid)?;
        (v >> 16 == 0 || v & (1 << (bit + 16)) != 0).then_some(v & (1 << bit) != 0)
    }
}

/// A shape's properties: its own, then the drawing's defaults.
struct Props<'a> {
    own: &'a Opt,
    defaults: Option<&'a Opt>,
}

impl Props<'_> {
    fn get(&self, pid: u16) -> Option<u32> {
        self.own.get(pid).or_else(|| self.defaults?.get(pid))
    }

    fn flag(&self, pid: u16, bit: u32) -> Option<bool> {
        self.own.flag(pid, bit).or_else(|| self.defaults?.flag(pid, bit))
    }
}

/// A shape colour: its own or one of the scheme's (system colours are left
/// to the caller).
fn color(v: u32, scheme: &[Rgba; 8]) -> Option<Rgba> {
    let flags = v >> 24;
    if flags & 0x08 != 0 {
        return scheme.get((v & 0xFF) as usize).copied();
    }
    if flags & 0x10 != 0 {
        return None;
    }
    Some(Rgba([v as u8, (v >> 8) as u8, (v >> 16) as u8, 255]))
}

#[derive(Clone)]
enum Fill {
    None,
    Color(Rgba),
    Image(Arc<str>),
}

/// The outline of preset shape `spt` in a `w` × `h` box, at the origin.
fn geometry(spt: u16, o: &Opt, w: f32, h: f32) -> Option<BezPath> {
    let name = match spt {
        0 => return freeform(o, w, h),
        2 => "roundRect",
        3 => "ellipse",
        4 => "diamond",
        5 => "triangle",
        6 => "rtTriangle",
        7 => "parallelogram",
        8 => "trapezoid",
        9 => "hexagon",
        10 => "octagon",
        11 => "plus",
        13 => "rightArrow",
        15 => "homePlate",
        20 | 32..=40 => "line",
        55 => "chevron",
        56 => "pentagon",
        66 => "leftArrow",
        67 => "downArrow",
        68 => "upArrow",
        110 => "diamond",
        111 => "parallelogram",
        116 | 176 => "roundRect",
        120 => "ellipse",
        _ => "rect",
    };
    Some(crate::geom::preset(name, w, h))
}

/// A freeform shape's outline from its vertices and segments.
fn freeform(o: &Opt, w: f32, h: f32) -> Option<BezPath> {
    let v = o.complex.get(&0x0145)?;
    let n = u16_at(v, 0)? as usize;
    let size = u16_at(v, 4)?;
    let pts: Vec<(f32, f32)> = (0..n.min(20_000))
        .map_while(|i| match size {
            0xFFF0 | 4 => Some((u16_at(v, 6 + i * 4)? as i16 as f32, u16_at(v, 8 + i * 4)? as i16 as f32)),
            _ => Some((i32_at(v, 6 + i * 8)? as f32, i32_at(v, 10 + i * 8)? as f32)),
        })
        .collect();
    if pts.is_empty() {
        return None;
    }
    let (l, t) = (o.get(0x0140).unwrap_or(0) as i32 as f32, o.get(0x0141).unwrap_or(0) as i32 as f32);
    let (r, b) = (o.get(0x0142).unwrap_or(21_600) as i32 as f32, o.get(0x0143).unwrap_or(21_600) as i32 as f32);
    let (sx, sy) = (w / (r - l).max(1.0), h / (b - t).max(1.0));
    let pt = |i: usize| pts.get(i).map(|&(x, y)| Point::new(((x - l) * sx) as f64, ((y - t) * sy) as f64));
    let mut p = BezPath::new();
    let mut i = 0;
    let segs: Vec<u16> = o.complex.get(&0x0146).map(|s| (0..u16_at(s, 0).unwrap_or(0) as usize).filter_map(|k| u16_at(s, 6 + k * 2)).collect()).unwrap_or_default();
    if segs.is_empty() {
        for k in 0..pts.len() {
            let q = pt(k)?;
            if k == 0 { p.move_to(q) } else { p.line_to(q) }
        }
        return Some(p);
    }
    let mut open = false;
    for s in segs.iter().take(20_000) {
        let count = (s & 0x1FFF).max(1) as usize;
        match s >> 13 {
            0 => {
                for _ in 0..count {
                    let Some(q) = pt(i) else { break };
                    if open { p.line_to(q) } else { p.move_to(q) }
                    open = true;
                    i += 1;
                }
            }
            1 => {
                for _ in 0..count {
                    let (Some(a), Some(b), Some(c)) = (pt(i), pt(i + 1), pt(i + 2)) else { break };
                    if !open {
                        p.move_to(a);
                    }
                    p.curve_to(a, b, c);
                    open = true;
                    i += 3;
                }
            }
            2 => {
                if let Some(q) = pt(i) {
                    p.move_to(q);
                    open = true;
                }
                i += 1;
            }
            3 => {
                if open {
                    p.close_path();
                }
            }
            4 => open = false,
            _ => {}
        }
    }
    Some(p)
}

/// Maps drawing coordinates to points.
#[derive(Clone, Copy)]
struct Map {
    sx: f32,
    sy: f32,
    ox: f32,
    oy: f32,
}

const SLIDE_MAP: Map = Map { sx: MU, sy: MU, ox: 0.0, oy: 0.0 };

/// A shape's box in points: x, y, width, height.
fn anchor(sp: Rec, m: Map) -> Option<[f32; 4]> {
    if let Some(a) = child(sp, CLIENT_ANCHOR) {
        let v: Vec<f32> = if a.body.len() >= 16 {
            (0..4).filter_map(|i| i32_at(a.body, i * 4)).map(|v| v as f32).collect()
        } else {
            (0..4).filter_map(|i| u16_at(a.body, i * 2)).map(|v| v as i16 as f32).collect()
        };
        // top, left, right, bottom
        return (v.len() == 4).then(|| [v[1] * MU, v[0] * MU, (v[2] - v[1]) * MU, (v[3] - v[0]) * MU]);
    }
    let a = child(sp, CHILD_ANCHOR)?;
    let v: Vec<f32> = (0..4).filter_map(|i| i32_at(a.body, i * 4)).map(|v| v as f32).collect();
    // left, top, right, bottom
    (v.len() == 4).then(|| [m.ox + v[0] * m.sx, m.oy + v[1] * m.sy, (v[2] - v[0]) * m.sx, (v[3] - v[1]) * m.sy])
}

// --- the document --------------------------------------------------------------

/// A slide of the list: its container and the texts kept with it.
struct Listed {
    persist: u32,
    texts: Vec<Txt>,
}

fn slide_list(r: Rec) -> Vec<Listed> {
    let mut out: Vec<Listed> = Vec::new();
    for k in kids(r) {
        if k.typ == SLIDE_PERSIST {
            out.push(Listed { persist: u32_at(k.body, 0).unwrap_or(0), texts: Vec::new() });
        } else if let Some(l) = out.last_mut() {
            take_text(k, &mut l.texts);
        }
    }
    out
}

/// What the slides are drawn from.
struct Data {
    stream: Vec<u8>,
    pics: Vec<u8>,
    dir: HashMap<u32, usize>,
    slides: Vec<Listed>,
    /// Masters and title masters by their id: their persist reference.
    masters: HashMap<u32, u32>,
    font_names: Vec<Arc<str>>,
    /// The drawing's default shape properties.
    defaults: Opt,
    /// Pictures by their index less one: in the Pictures stream (true) or
    /// the document's, and where their record starts.
    blips: Vec<Option<(bool, usize)>>,
    /// The default text styles.
    env: Styles,
    size: (f32, f32),
}

pub(crate) struct Doc {
    data: Data,
    pages: VecDeque<(usize, Page)>,
    fonts: FontCache,
    pictures: Pictures,
    limits: Limits,
}

impl Doc {
    pub fn open(data: Vec<u8>, limits: &Limits, fonts: &Fonts) -> Result<Doc, Error> {
        let ole = Ole::open(&data, limits)?;
        let stream = ole.stream("PowerPoint Document").ok_or(Error::Unsupported)?;
        let user = ole.stream("Current User").unwrap_or_default();
        let pics = ole.stream("Pictures").unwrap_or_default();
        drop(ole);
        if u32_at(&user, 12) == Some(ENCRYPTED) {
            return Err(Error::Encrypted);
        }
        let bad = |m: &str| Error::Malformed(m.into());
        // The last save: named by Current User, else the last edit in the stream.
        let start = u32_at(&user, 16).map(|o| o as usize).filter(|&o| rec_at(&stream, o, 0).is_some_and(|r| r.typ == USER_EDIT));
        let start = start.or_else(|| {
            let mut p = 0;
            let mut last = None;
            while let Some(r) = rec_at(&stream, p, 0) {
                if r.typ == USER_EDIT {
                    last = Some(p);
                }
                p += 8 + r.body.len();
            }
            last
        });
        let (dir, doc_ref) = start.and_then(|s| persist_dir(&stream, s)).unwrap_or_default();
        let doc = dir
            .get(&doc_ref)
            .and_then(|&o| rec_at(&stream, o, 0))
            .filter(|r| r.typ == DOCUMENT)
            .or_else(|| recs(&stream, 0).filter(|r| r.typ == DOCUMENT).last())
            .ok_or_else(|| bad("no presentation in the file"))?;
        if child(doc, 0x2F14).is_some() {
            return Err(Error::Encrypted);
        }
        let mut size = (720.0, 540.0);
        if let Some(a) = child(doc, DOCUMENT_ATOM) {
            let (x, y) = (i32_at(a.body, 0).unwrap_or(0) as f32 * MU, i32_at(a.body, 4).unwrap_or(0) as f32 * MU);
            if (36.0..=4000.0).contains(&x) && (36.0..=4000.0).contains(&y) {
                size = (x, y);
            }
        }
        let mut slides = Vec::new();
        let mut masters = HashMap::new();
        for l in kids(doc).filter(|r| r.typ == SLIDE_LIST) {
            match l.inst {
                0 if slides.is_empty() => slides = slide_list(l),
                1 => masters.extend(kids(l).filter(|k| k.typ == SLIDE_PERSIST).map(|k| (u32_at(k.body, 12).unwrap_or(0), u32_at(k.body, 0).unwrap_or(0)))),
                _ => {}
            }
        }
        slides.truncate(limits.max_pages.max(1));
        if slides.is_empty() {
            return Err(bad("the presentation has no slides"));
        }
        let mut font_names = Vec::new();
        let mut env = Styles::default();
        if let Some(e) = child(doc, ENVIRONMENT) {
            for k in kids(e) {
                match k.typ {
                    FONT_COLLECTION => {
                        for f in kids(k).filter(|f| f.typ == FONT_ENTITY).take(1000) {
                            let name: Vec<u16> = f.body.chunks_exact(2).take(32).map(|c| u16::from_le_bytes([c[0], c[1]])).take_while(|&c| c != 0).collect();
                            font_names.push(Arc::from(String::from_utf16_lossy(&name)));
                        }
                    }
                    MASTER_STYLE => env.read_atom(k),
                    CF_EXCEPTION => env.base.1 = Cur { d: k.body, p: 0 }.cf().unwrap_or_default(),
                    PF_EXCEPTION => env.base.0 = Cur { d: k.body, p: 2 }.pf().unwrap_or_default(),
                    _ => {}
                }
            }
        }
        let mut defaults = Opt::default();
        let mut blips = Vec::new();
        if let Some(dgg) = child(doc, DRAWING_GROUP).and_then(|g| child(g, DGG)) {
            for k in kids(dgg) {
                match k.typ {
                    OPT | TERTIARY_OPT => defaults.read(k),
                    BSTORE => {
                        for b in kids(k).take(100_000) {
                            if b.typ != BSE {
                                blips.push(None);
                                continue;
                            }
                            let name = b.body.get(33).copied().unwrap_or(0) as usize;
                            blips.push(if b.body.len() > 36 + name + 8 {
                                Some((false, b.at + 36 + name))
                            } else {
                                u32_at(b.body, 28).map(|o| (true, o as usize))
                            });
                        }
                    }
                    _ => {}
                }
            }
        }
        let data = Data { stream: Vec::new(), pics, dir, slides, masters, font_names, defaults, blips, env, size };
        Ok(Doc { data: Data { stream, ..data }, pages: VecDeque::new(), fonts: FontCache::new(fonts.clone()), pictures: Pictures::default(), limits: limits.clone() })
    }

    pub fn page_count(&self) -> usize {
        self.data.slides.len()
    }

    pub fn page_size(&self, n: usize) -> Result<(f32, f32), Error> {
        if n < self.data.slides.len() { Ok(self.data.size) } else { Err(Error::NoSuchPage) }
    }

    fn page(&mut self, n: usize) -> Result<&Page, Error> {
        if n >= self.data.slides.len() {
            return Err(Error::NoSuchPage);
        }
        if !self.pages.iter().any(|(i, _)| *i == n) {
            let page = self.data.draw(n, &mut self.fonts);
            if self.pages.len() >= KEEP {
                self.pages.pop_front();
            }
            self.pages.push_back((n, page));
        }
        self.pages.iter().find(|(i, _)| *i == n).map(|(_, p)| p).ok_or(Error::NoSuchPage)
    }

    pub fn render(&mut self, n: usize, width: u32, cancel: &dyn Fn() -> bool) -> Result<Image, Error> {
        self.page(n)?;
        let page = self.pages.iter().find(|(i, _)| *i == n).map(|(_, p)| p).ok_or(Error::NoSuchPage)?;
        let (data, pictures, limits) = (&self.data, &mut self.pictures, &self.limits);
        draw::render(page, width, &mut self.fonts, &mut |key| pictures.get_with(key, limits, || data.blip(key)), limits, cancel)
    }

    pub fn runs(&mut self, n: usize) -> Result<Vec<TextRun>, Error> {
        self.page(n).map(Page::runs)
    }
}

/// The default colour scheme: background, text, shadow, title, fill and
/// accents.
const SCHEME: [u32; 8] = [0xFFFFFF, 0x000000, 0x808080, 0x000000, 0xBBE0E3, 0x333399, 0x009999, 0x99CC00];

impl Data {
    fn container(&self, persist: u32) -> Option<Rec<'_>> {
        self.dir.get(&persist).and_then(|&o| rec_at(&self.stream, o, 0))
    }

    /// The bytes of picture `key` ("pibN"), as a file the picture
    /// decoder reads.
    fn blip(&self, key: &str) -> Option<Vec<u8>> {
        let n: usize = key.strip_prefix("pib")?.parse().ok()?;
        let &(in_pics, at) = self.blips.get(n.checked_sub(1)?)?.as_ref()?;
        let src = if in_pics { &self.pics } else { &self.stream };
        let r = rec_at(src, at, 0)?;
        let skip = 16 * (1 + (r.inst as usize & 1)) + 1;
        match r.typ {
            0xF01D | 0xF01E | 0xF02A => r.body.get(skip..).map(<[u8]>::to_vec),
            0xF01F => crate::images::bmp_from_dib(r.body.get(skip..)?),
            _ => None,
        }
    }

    /// The masters above a slide: its master, that one's master...
    fn masters_of(&self, c: Rec) -> Vec<Rec<'_>> {
        let mut out = Vec::new();
        let mut id = child(c, SLIDE_ATOM).and_then(|a| u32_at(a.body, 12)).unwrap_or(0);
        let mut seen = HashSet::new();
        while id != 0 && seen.insert(id) && out.len() < 4 {
            let Some(m) = self.masters.get(&id).and_then(|&p| self.container(p)).filter(|m| matches!(m.typ, SLIDE | MAIN_MASTER)) else { break };
            out.push(m);
            id = child(m, SLIDE_ATOM).and_then(|a| u32_at(a.body, 12)).unwrap_or(0);
        }
        if out.is_empty() {
            // A file naming no master: the first main master.
            if let Some(m) = self.masters.values().filter_map(|&p| self.container(p)).find(|m| m.typ == MAIN_MASTER) {
                out.push(m);
            }
        }
        out
    }

    fn draw(&self, n: usize, fonts: &mut FontCache) -> Page {
        let (w, h) = self.size;
        let listed = &self.slides[n];
        let mut page = Page { width: w, height: h, background: Some(Rgba::WHITE), items: Vec::new(), text: Vec::new() };
        let Some(slide) = self.container(listed.persist).filter(|c| c.typ == SLIDE) else { return page };
        let masters = self.masters_of(slide);
        let flags = |c: Rec| child(c, SLIDE_ATOM).and_then(|a| u16_at(a.body, 20)).unwrap_or(if c.typ == MAIN_MASTER { 0 } else { 7 });
        // The colour scheme: the slide's own, else its master's.
        let own_scheme = |c: Rec| child(c, COLOR_SCHEME).map(|s| core::array::from_fn(|i| u32_at(s.body, i * 4).map_or(Rgba::rgb(SCHEME[i]), |v| Rgba([v as u8, (v >> 8) as u8, (v >> 16) as u8, 255]))));
        let follows = |bit: u16| flags(slide) & bit != 0;
        let scheme = std::iter::once(slide)
            .filter(|_| !follows(2))
            .chain(masters.iter().copied())
            .find_map(own_scheme)
            .unwrap_or(SCHEME.map(Rgba::rgb));
        let mut styles = Styles::default();
        if let Some(main) = masters.iter().find(|m| m.typ == MAIN_MASTER) {
            for k in kids(*main).filter(|k| k.typ == MASTER_STYLE) {
                styles.read_atom(k);
            }
        }
        let empty = Vec::new();
        let mut s = Slide { data: self, fonts, scheme, styles: &styles, texts: &empty, items: Vec::new(), text: Vec::new(), shapes: 0, master: false, background: None };
        // The background: the slide's own, else its masters'.
        let mut bg = None;
        for c in std::iter::once(slide).filter(|_| !follows(4)).chain(masters.iter().copied()) {
            s.find_background(c);
            if let Some(f) = s.background.take() {
                bg = Some(f);
                break;
            }
        }
        match bg {
            Some(Fill::Color(c)) => s.items.push(Item::Rect { x: 0.0, y: 0.0, w, h, color: c }),
            Some(Fill::Image(key)) => s.items.push(Item::Image { x: 0.0, y: 0.0, w, h, key }),
            _ => {}
        }
        // The masters' shapes, the farthest first, as far as each follows its master.
        let mut shown = Vec::new();
        let mut cur = slide;
        for &m in &masters {
            if flags(cur) & 1 == 0 {
                break;
            }
            shown.push(m);
            cur = m;
        }
        s.master = true;
        for m in shown.iter().rev() {
            s.drawing(*m);
        }
        s.master = false;
        s.texts = &listed.texts;
        s.drawing(slide);
        page.items = s.items;
        page.text = s.text;
        page
    }
}

// --- drawing a slide -------------------------------------------------------------

/// A paragraph read, to be set at a width.
struct Para {
    fmt: ParaFormat,
    spans: Vec<Span>,
    before: f32,
    after: f32,
}

struct Slide<'a> {
    data: &'a Data,
    fonts: &'a mut FontCache,
    scheme: [Rgba; 8],
    styles: &'a Styles,
    /// The texts kept in the slide list, for shapes pointing at them.
    texts: &'a [Txt],
    items: Vec<Item>,
    text: Vec<TextRun>,
    shapes: usize,
    /// Drawing a master: its placeholders are left out.
    master: bool,
    background: Option<Fill>,
}

impl Slide<'_> {
    /// The shapes of a slide or master.
    fn drawing(&mut self, c: Rec) {
        let Some(dg) = child(c, DRAWING).and_then(|d| child(d, DG)) else { return };
        for k in kids(dg) {
            if k.typ == SP_GROUP {
                self.group(k, SLIDE_MAP, 0);
            }
        }
    }

    fn find_background(&mut self, c: Rec) {
        let Some(dg) = child(c, DRAWING).and_then(|d| child(d, DG)) else { return };
        for k in kids(dg).filter(|k| k.typ == SP) {
            let flags = child(k, SHAPE_ATOM).and_then(|a| u32_at(a.body, 4)).unwrap_or(0);
            if flags & 0x400 != 0 {
                let mut o = Opt::default();
                for r in kids(k).filter(|r| matches!(r.typ, OPT | TERTIARY_OPT)) {
                    o.read(r);
                }
                let p = Props { own: &o, defaults: None };
                self.background = Some(self.fill(&p, true));
                return;
            }
        }
    }

    fn group(&mut self, g: Rec, m: Map, depth: u8) {
        if depth > 8 {
            return;
        }
        let mut inner = m;
        for (i, r) in kids(g).enumerate() {
            if self.shapes >= MAX_SHAPES {
                return;
            }
            match r.typ {
                SP_GROUP => self.group(r, inner, depth + 1),
                SP if i == 0 => {
                    // The group's own shape: its box and the child coordinates.
                    if let (Some(f), Some(sg)) = (anchor(r, m), child(r, SP_GROUP_ATOM)) {
                        let v: Vec<f32> = (0..4).filter_map(|k| i32_at(sg.body, k * 4)).map(|v| v as f32).collect();
                        if v.len() == 4 && v[2] > v[0] && v[3] > v[1] {
                            let (sx, sy) = (f[2] / (v[2] - v[0]), f[3] / (v[3] - v[1]));
                            inner = Map { sx, sy, ox: f[0] - v[0] * sx, oy: f[1] - v[1] * sy };
                        }
                    }
                }
                SP => {
                    self.shapes += 1;
                    self.shape(r, inner);
                }
                _ => {}
            }
        }
    }

    fn fill(&self, p: &Props, background: bool) -> Fill {
        if p.flag(0x01BF, 4) == Some(false) {
            return Fill::None;
        }
        match p.get(0x0180).unwrap_or(0) {
            2 | 3 => match p.get(0x0186).filter(|&b| b > 0) {
                Some(b) => Fill::Image(Arc::from(format!("pib{b}"))),
                None => Fill::None,
            },
            9 if !background => Fill::None,
            _ => {
                let Some(mut c) = p.get(0x0181).and_then(|v| color(v, &self.scheme)).or(Some(if background { self.scheme[0] } else { Rgba::WHITE })) else { return Fill::None };
                let a = p.get(0x0182).map_or(1.0, |v| (v as f32 / 65_536.0).clamp(0.0, 1.0));
                c.0[3] = (a * 255.0) as u8;
                if c.0[3] == 0 { Fill::None } else { Fill::Color(c) }
            }
        }
    }

    fn shape(&mut self, sp: Rec, m: Map) {
        let Some(atom) = child(sp, SHAPE_ATOM) else { return };
        let spt = atom.inst;
        let flags = u32_at(atom.body, 4).unwrap_or(0);
        // Deleted shapes, the background, and groups (drawn through their children).
        if flags & (0x8 | 0x400 | 0x1) != 0 {
            return;
        }
        let mut o = Opt::default();
        for r in kids(sp).filter(|r| matches!(r.typ, OPT | TERTIARY_OPT)) {
            o.read(r);
        }
        if o.flag(0x03BF, 1) == Some(true) {
            return;
        }
        let Some([mut x, mut y, mut w, mut h]) = anchor(sp, m) else { return };
        if !(w.is_finite() && h.is_finite() && x.is_finite() && y.is_finite()) || w.abs() > 1e5 || h.abs() > 1e5 {
            return;
        }
        let placeholder = child(sp, CLIENT_DATA).and_then(|d| child(d, PLACEHOLDER)).is_some();
        if self.master && placeholder {
            return;
        }
        // Text boxes and placeholders have no fill or line unless given.
        let p = Props { own: &o, defaults: (!placeholder && !matches!(spt, 202 | 75)).then_some(&self.data.defaults) };
        let rot = o.get(0x0004).map_or(0.0, |v| (v as i32) as f32 / 65_536.0).rem_euclid(360.0);
        if (45.0..135.0).contains(&rot) || (225.0..315.0).contains(&rot) {
            // Turned on its side: the box is kept as if turned back.
            let (cx, cy) = (x + w / 2.0, y + h / 2.0);
            (w, h) = (h, w);
            (x, y) = (cx - w / 2.0, cy - h / 2.0);
        }
        let (fh, fv) = (flags & 0x40 != 0, flags & 0x80 != 0);
        let at = Affine::translate((x as f64 + w as f64 / 2.0, y as f64 + h as f64 / 2.0))
            * Affine::rotate((rot as f64).to_radians())
            * Affine::scale_non_uniform(if fh { -1.0 } else { 1.0 }, if fv { -1.0 } else { 1.0 })
            * Affine::translate((-w as f64 / 2.0, -h as f64 / 2.0));
        let line_shape = matches!(spt, 20 | 32..=40);
        if let Some(path) = geometry(spt, &o, w.max(0.0), h.max(0.0)) {
            let path = draw::tidy(at * path);
            if !line_shape {
                match self.fill(&p, false) {
                    Fill::Color(c) => self.items.push(Item::Path { path: path.clone(), fill: Some(c), stroke: None }),
                    Fill::Image(key) => {
                        let b = path.bounding_box();
                        let (bx, by, bw, bh) = (b.x0 as f32, b.y0 as f32, b.width() as f32, b.height() as f32);
                        if bw > 0.0 && bh > 0.0 {
                            self.items.push(Item::Clip { x: bx, y: by, w: bw, h: bh });
                            self.items.push(Item::Image { x: bx, y: by, w: bw, h: bh, key });
                            self.items.push(Item::Unclip);
                        }
                    }
                    Fill::None => {}
                }
            }
            if let Some(pib) = o.get(0x0104).filter(|&b| b > 0) {
                self.picture(pib, &o, x, y, w, h);
            }
            let line = p.flag(0x01FF, 3).unwrap_or(!placeholder && !matches!(spt, 202 | 75));
            if line {
                let c = p.get(0x01C0).and_then(|v| color(v, &self.scheme)).unwrap_or(Rgba::BLACK);
                let lw = p.get(0x01CB).map_or(0.75, |v| v as f32 * EMU).clamp(0.1, 100.0);
                self.items.push(Item::Path { path, fill: None, stroke: Some((lw, c)) });
            }
        }
        // Text set sideways is left out rather than drawn wrong.
        if rot.min(360.0 - rot) > 1.0 || o.get(0x0088).is_some_and(|f| f != 0 && f != 4) {
            return;
        }
        let Some(tb) = child(sp, TEXTBOX) else { return };
        let mut local = Vec::new();
        let mut outline = None;
        for k in kids(tb) {
            if k.typ == OUTLINE_REF {
                outline = u32_at(k.body, 0);
            } else {
                take_text(k, &mut local);
            }
        }
        let t = match outline {
            Some(i) => self.texts.get(i as usize),
            None => local.first(),
        };
        if let Some(t) = t.filter(|t| !t.units.is_empty()) {
            self.text_in(t, &o, x, y, w, h);
        }
    }

    fn picture(&mut self, pib: u32, o: &Opt, x: f32, y: f32, w: f32, h: f32) {
        let crop = |pid: u16| o.get(pid).map_or(0.0, |v| (v as i32) as f32 / 65_536.0).clamp(-10.0, 0.99);
        let (t, b, l, r) = (crop(0x0100), crop(0x0101), crop(0x0102), crop(0x0103));
        let key: Arc<str> = Arc::from(format!("pib{pib}"));
        if t == 0.0 && b == 0.0 && l == 0.0 && r == 0.0 {
            self.items.push(Item::Image { x, y, w, h, key });
            return;
        }
        let (fw, fh) = (w / (1.0 - l - r).max(0.01), h / (1.0 - t - b).max(0.01));
        self.items.push(Item::Clip { x, y, w, h });
        self.items.push(Item::Image { x: x - l * fw, y: y - t * fh, w: fw, h: fh, key });
        self.items.push(Item::Unclip);
    }

    fn font(&self, i: Option<u16>) -> Arc<str> {
        i.and_then(|i| self.data.font_names.get(i as usize)).or(self.data.font_names.first()).cloned().unwrap_or_else(|| Arc::from("Arial"))
    }

    /// The paragraphs of text `t`.
    fn paras(&mut self, t: &Txt) -> Vec<Para> {
        let units = &t.units[..t.units.len().min(MAX_TEXT)];
        let len = units.len();
        let (pfs, cfs) = style_runs(&t.style, len);
        let rul = ruler(&t.ruler);
        let title = matches!(t.kind, 0 | 6);
        let mut out = Vec::new();
        let mut start = 0;
        while start <= len && out.len() < MAX_PARAS {
            let end = units[start..].iter().position(|&u| u == 0x0D).map_or(len, |p| start + p);
            let run = pfs.iter().find(|r| r.0 > start);
            let level = run.map_or(0, |r| r.1 as usize).min(4);
            let (mut pf, mcf) = self.styles.level(&self.data.env, t.kind, level);
            pf.over(&rul[level]);
            if let Some(r) = run {
                pf.over(&r.2);
            }
            let style_of = |cf: &Cf, s: &Slide| {
                let size = cf.size.map_or(if title { 44.0 } else { 18.0 }, |v| v as f32).clamp(1.0, 4000.0);
                let color = cf.color.and_then(|c| index_color(c, &s.scheme)).unwrap_or(s.scheme[if title { 3 } else { 1 }]);
                Style {
                    family: s.font(cf.font),
                    size,
                    bold: cf.bold.unwrap_or(false),
                    italic: cf.italic.unwrap_or(false),
                    underline: cf.underline.unwrap_or(false),
                    color,
                    shift: cf.position.map_or(0, |p| p.signum() as i8),
                    ..Style::default()
                }
            };
            // Character runs, cut where the paragraph's runs end.
            let mut spans: Vec<Span> = Vec::new();
            let mut p = start;
            let mut first_style = None;
            while p < end {
                let ci = cfs.iter().position(|r| r.0 > p);
                let stop = ci.map_or(end, |i| cfs[i].0.min(end)).max(p + 1);
                let mut cf = mcf.clone();
                if let Some(i) = ci {
                    cf.over(&cfs[i].1);
                }
                let st = Arc::new(style_of(&cf, self));
                first_style.get_or_insert_with(|| st.clone());
                let s: String = char::decode_utf16(units[p..stop].iter().copied())
                    .map(|c| match c.unwrap_or('\u{FFFD}') {
                        '\u{b}' => '\n',
                        c => c,
                    })
                    .filter(|c| *c == '\n' || *c == '\t' || !c.is_control())
                    .collect();
                spans.push(Span::new(s, st));
                p = stop;
            }
            let st = first_style.unwrap_or_else(|| {
                let mut cf = mcf.clone();
                if let Some(r) = cfs.iter().find(|r| r.0 > start) {
                    cf.over(&r.1);
                }
                Arc::new(style_of(&cf, self))
            });
            if spans.is_empty() {
                spans.push(Span::new("", st.clone()));
            }
            let left = pf.left.unwrap_or(0) as f32 * MU;
            let indent = pf.indent.unwrap_or(0) as f32 * MU;
            let mut tabs = Vec::new();
            if pf.bullet == Some(true) && end > start {
                let font = if pf.bullet_has_font == Some(true) { self.font(pf.bullet_font) } else { st.family.clone() };
                let ch = bullet_char(pf.bullet_char.unwrap_or(0x2022), &font);
                let size = match pf.bullet_size.filter(|_| pf.bullet_has_size == Some(true)) {
                    Some(v) if v > 0 => st.size * (v as f32 / 100.0).clamp(0.25, 4.0),
                    Some(v) if v < 0 => (-(v as f32)).min(4000.0),
                    _ => st.size,
                };
                let color = pf.bullet_color.filter(|_| pf.bullet_has_color == Some(true)).and_then(|c| index_color(c, &self.scheme)).unwrap_or(st.color);
                // The bullet in the text's font when its own is a symbol font.
                let family = if font.to_ascii_lowercase().contains("wingdings") || font.to_ascii_lowercase().contains("symbol") { st.family.clone() } else { font };
                let b = Arc::new(Style { family, size, color, underline: false, bold: false, italic: false, ..(*st).clone() });
                let gap = if left > indent + 1.0 {
                    tabs.push(Tab { pos: left, align: TabAlign::Left, leader: None });
                    "\t"
                } else {
                    " "
                };
                spans.insert(0, Span::new(gap, st.clone()));
                spans.insert(0, Span::new(ch.to_string(), b));
            }
            let line = match pf.line {
                Some(v) if v < 0 => LineRule::Exact((-(v as f32) * MU).max(1.0)),
                Some(v) => LineRule::Multiple((v as f32 / 100.0).clamp(0.25, 10.0)),
                None => LineRule::Multiple(1.0),
            };
            let space = |v: Option<i16>| match v {
                Some(v) if v < 0 => -(v as f32) * MU,
                Some(v) => v as f32 / 100.0 * st.size * 1.2,
                None => 0.0,
            };
            let align = match pf.align.unwrap_or(0) {
                1 => Align::Center,
                2 => Align::Right,
                3 | 4 | 6 => Align::Justify,
                _ => Align::Left,
            };
            let fmt = ParaFormat { align, left, right: 0.0, first: indent - left, line, tabs };
            out.push(Para { fmt, spans, before: if out.is_empty() { 0.0 } else { space(pf.before) }, after: space(pf.after) });
            start = end + 1;
        }
        // The last paragraph mark ends the text: no empty paragraph after it.
        out
    }

    fn text_in(&mut self, t: &Txt, o: &Opt, x: f32, y: f32, w: f32, h: f32) {
        let paras = self.paras(t);
        let inset = |pid: u16, d: f32| o.get(pid).map_or(d, |v| (v as i32) as f32 * EMU).clamp(0.0, 1000.0);
        let (l, top, r, b) = (inset(0x0081, 7.2), inset(0x0082, 3.6), inset(0x0083, 7.2), inset(0x0084, 3.6));
        let nowrap = o.get(0x0085) == Some(2);
        let inner = (w - l - r).max(1.0);
        let mut set: Vec<(f32, Lines, f32, Align)> = Vec::with_capacity(paras.len());
        for p in &paras {
            let lines = if nowrap {
                let fmt = ParaFormat { align: Align::Left, ..p.fmt.clone() };
                typeset::set(&p.spans, &fmt, 100_000.0, self.fonts)
            } else {
                typeset::set(&p.spans, &p.fmt, inner, self.fonts)
            };
            set.push((p.before, lines, p.after, p.fmt.align));
        }
        let total: f32 = set.iter().map(|(a, l, c, _)| a + l.height() + c).sum();
        let room = h - top - b;
        let anchor = o.get(0x0087).unwrap_or(0);
        let mut cy = y
            + top
            + match anchor {
                1 | 4 => (room - total) / 2.0,
                2 | 5 | 7 | 9 => room - total,
                _ => 0.0,
            };
        let widest = set.iter().flat_map(|(_, l, _, _)| l.lines.iter().map(|l| l.width)).fold(0.0, f32::max);
        // Text centred as a block: its widest line in the middle.
        let block = if matches!(anchor, 3 | 4 | 5 | 8 | 9) && !nowrap { (inner - widest).max(0.0) / 2.0 } else { 0.0 };
        for (before, lines, after, align) in &set {
            cy += before;
            for (i, line) in lines.lines.iter().enumerate() {
                let dx = if nowrap {
                    match align {
                        Align::Center => (inner - line.width) / 2.0,
                        Align::Right => inner - line.width,
                        _ => 0.0,
                    }
                } else {
                    0.0
                };
                lines.draw_line(i, x + l + block + dx, cy, &mut self.items, &mut self.text);
                cy += line.height;
            }
            cy += after;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(typ: u16, inst: u16, body: &[u8]) -> Vec<u8> {
        let mut v = Vec::new();
        let ver = if matches!(typ, SLIDE_LIST | SP | SP_GROUP) { 0xF } else { 0 };
        v.extend_from_slice(&((inst << 4) | ver).to_le_bytes());
        v.extend_from_slice(&typ.to_le_bytes());
        v.extend_from_slice(&(body.len() as u32).to_le_bytes());
        v.extend_from_slice(body);
        v
    }

    #[test]
    fn slide_list_texts() {
        let mut persist = vec![0u8; 20];
        persist[0] = 7;
        persist[12..16].copy_from_slice(&256u32.to_le_bytes());
        let mut body = rec(SLIDE_PERSIST, 0, &persist);
        body.extend(rec(TEXT_HEADER, 0, &0u32.to_le_bytes()));
        body.extend(rec(TEXT_BYTES, 0, b"Hello\rworld"));
        body.extend(rec(TEXT_HEADER, 0, &1u32.to_le_bytes()));
        let chars: Vec<u8> = "Caf\u{e9}".encode_utf16().flat_map(u16::to_le_bytes).collect();
        body.extend(rec(TEXT_CHARS, 0, &chars));
        let all = rec(SLIDE_LIST, 0, &body);
        let l = slide_list(rec_at(&all, 0, 0).unwrap());
        assert_eq!(l.len(), 1);
        assert_eq!(l[0].persist, 7);
        assert_eq!(l[0].texts.len(), 2);
        assert_eq!(String::from_utf16_lossy(&l[0].texts[0].units), "Hello\rworld");
        assert_eq!((l[0].texts[1].kind, String::from_utf16_lossy(&l[0].texts[1].units)), (1, "Caf\u{e9}".to_string()));
    }

    #[test]
    fn style_runs_read() {
        let mut d = Vec::new();
        // One paragraph run of 6 units, level 1, centred; bold size 24 for 3 units then 3 plain.
        d.extend(6u32.to_le_bytes());
        d.extend(1u16.to_le_bytes());
        d.extend((1u32 << 11).to_le_bytes());
        d.extend(1u16.to_le_bytes());
        d.extend(3u32.to_le_bytes());
        d.extend((1u32 | 1 << 17).to_le_bytes());
        d.extend(1u16.to_le_bytes());
        d.extend(24u16.to_le_bytes());
        d.extend(3u32.to_le_bytes());
        d.extend(0u32.to_le_bytes());
        let (pfs, cfs) = style_runs(&d, 5);
        assert_eq!(pfs.len(), 1);
        assert_eq!((pfs[0].0, pfs[0].1, pfs[0].2.align), (6, 1, Some(1)));
        assert_eq!(cfs.len(), 2);
        assert_eq!((cfs[0].1.bold, cfs[0].1.size), (Some(true), Some(24)));
        assert_eq!(cfs[1].0, 6);
    }

    #[test]
    fn anchors_in_points() {
        let mut a = Vec::new();
        for v in [80i16, 160, 800, 480] {
            a.extend_from_slice(&v.to_le_bytes());
        }
        let sp = rec(SP, 0, &rec(CLIENT_ANCHOR, 0, &a));
        let f = anchor(rec_at(&sp, 0, 0).unwrap(), SLIDE_MAP).unwrap();
        assert_eq!(f, [20.0, 10.0, 80.0, 50.0]);
    }

    #[test]
    fn symbol_bullets() {
        assert_eq!(bullet_char(0xF06C, "Wingdings"), '\u{2022}');
        assert_eq!(bullet_char(0x2013, "Arial"), '\u{2013}');
        assert_eq!(bullet_char(0, "Arial"), '\u{2022}');
    }
}
