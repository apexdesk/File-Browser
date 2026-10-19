//! The fonts text is drawn with when a file doesn't bring its own: the
//! system's, found by family name, or a face like the one asked for.

use std::collections::{BTreeMap, HashMap};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

/// A font face: the file's bytes and the face's index in it.
pub(crate) type FaceData = (Arc<[u8]>, u32);

/// Faces of each family (lowercase): regular, bold, italic, bold italic.
type Families = BTreeMap<String, [Option<(PathBuf, u32)>; 4]>;

/// Where fonts are looked for. Cheap to clone: clones share what was
/// found and loaded, so one `Fonts` can serve every document opened.
/// The folders are read the first time a font is needed.
#[derive(Clone)]
pub struct Fonts(Arc<Inner>);

struct Inner {
    dirs: Vec<PathBuf>,
    families: OnceLock<Families>,
    /// Files loaded, and their bytes in all.
    loaded: Mutex<(HashMap<PathBuf, Arc<[u8]>>, usize)>,
}

/// Bytes of font files kept loaded.
const LOADED_CAP: usize = 96 << 20;
/// Largest font file read.
const MAX_FONT_FILE: u64 = 64 << 20;

const SANS: &[&str] = &["liberation sans", "arimo", "arial", "nimbus sans", "helvetica", "dejavu sans", "noto sans", "roboto", "segoe ui"];
const SERIF: &[&str] = &["liberation serif", "tinos", "times new roman", "nimbus roman", "dejavu serif", "noto serif"];
const MONO: &[&str] = &["liberation mono", "cousine", "courier new", "nimbus mono ps", "dejavu sans mono", "noto sans mono", "droid sans mono"];
const CJK: &[&str] = &["noto sans cjk jp", "noto sans cjk sc", "noto serif cjk jp", "source han sans", "noto sans sc", "noto sans jp", "droid sans fallback", "wenquanyi zen hei", "ipagothic", "takao gothic", "microsoft yahei", "yu gothic", "ms gothic", "simsun", "malgun gothic", "pingfang sc", "hiragino sans"];
const CJK_NAMES: &[&str] = &["ming", "song", "hei", "gothic", "mincho", "kai", "batang", "gulim", "dotum", "kozmin", "kozgo", "heisei", "msung", "mhei", "hysmyeongjo", "hygothic", "simsun", "simhei", "fangsong", "ryumin", "gothicbbb", "sun-extb", "malgun", "meiryo", "yugoth", "pmingliu", "mingliu"];
const CJK_SERIF: &[&str] = &["ming", "song", "mincho", "ryumin", "kozmin", "batang", "simsun", "fangsong", "msung", "hysmyeongjo"];

/// Metric-compatible replacements for common proprietary fonts.
const SUBSTITUTES: &[(&str, &[&str])] = &[
    ("calibri light", &["carlito"]),
    ("calibri", &["carlito"]),
    ("cambria", &["caladea"]),
    ("arial", &["liberation sans", "arimo"]),
    ("helvetica", &["liberation sans", "arimo", "nimbus sans"]),
    ("times new roman", &["liberation serif", "tinos", "nimbus roman"]),
    ("times", &["liberation serif", "tinos", "nimbus roman"]),
    ("courier new", &["liberation mono", "cousine", "nimbus mono ps"]),
    ("courier", &["liberation mono", "cousine", "nimbus mono ps"]),
    ("arial narrow", &["liberation sans narrow"]),
    ("georgia", &["gelasio"]),
    ("segoe ui", &["noto sans", "dejavu sans"]),
    ("verdana", &["dejavu sans"]),
];

impl Fonts {
    /// The fonts installed on this system.
    pub fn system() -> Fonts {
        Fonts::from_dirs(system_dirs())
    }

    /// The fonts in these folders (and their subfolders).
    pub fn from_dirs(dirs: Vec<PathBuf>) -> Fonts {
        Fonts(Arc::new(Inner { dirs, families: OnceLock::new(), loaded: Mutex::new((HashMap::new(), 0)) }))
    }

    /// No fonts at all: text a file doesn't bring fonts for is left out.
    pub fn none() -> Fonts {
        Fonts::from_dirs(Vec::new())
    }

    fn families(&self) -> &Families {
        self.0.families.get_or_init(|| {
            let mut paths = Vec::new();
            for d in &self.0.dirs {
                scan(d, 4, &mut paths);
            }
            paths.sort();
            let mut out: Families = BTreeMap::new();
            for p in paths {
                for f in read_faces(&p) {
                    let slot = (f.weight >= 600) as usize + 2 * f.italic as usize;
                    let e = out.entry(f.family.to_lowercase()).or_default();
                    // The weight nearest to regular (400) or bold (700).
                    if e[slot].is_none() || matches!(f.weight, 400 | 700) {
                        e[slot] = Some((f.path.clone(), f.index));
                    }
                }
            }
            out
        })
    }

    fn bytes(&self, path: &Path) -> Option<Arc<[u8]>> {
        let mut loaded = self.0.loaded.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(b) = loaded.0.get(path) {
            return Some(b.clone());
        }
        let mut file = std::fs::File::open(path).ok()?;
        if file.metadata().ok()?.len() > MAX_FONT_FILE {
            return None;
        }
        let mut v = Vec::new();
        file.read_to_end(&mut v).ok()?;
        let b: Arc<[u8]> = Arc::from(v);
        if loaded.1 + b.len() > LOADED_CAP {
            loaded.0.clear();
            loaded.1 = 0;
        }
        loaded.1 += b.len();
        loaded.0.insert(path.to_path_buf(), b.clone());
        Some(b)
    }

    /// The first family of `want` installed, in the style asked for (or
    /// the closest one it has).
    fn first_of<S: AsRef<str>>(&self, want: &[S], bold: bool, italic: bool) -> Option<FaceData> {
        let fams = self.families();
        let slot = bold as usize + 2 * italic as usize;
        for w in want {
            let Some(faces) = fams.get(w.as_ref()) else { continue };
            let pick = faces[slot].as_ref().or(faces[slot & 1].as_ref()).or(faces[0].as_ref()).or(faces.iter().flatten().next());
            if let Some((p, i)) = pick
                && let Some(b) = self.bytes(p)
            {
                return Some((b, *i));
            }
        }
        None
    }

    /// The face for text in `family`: the family itself, a font with its
    /// metrics, or a sans, serif or monospace face like it.
    #[cfg_attr(not(feature = "ooxml"), allow(dead_code))]
    pub(crate) fn face(&self, family: &str, bold: bool, italic: bool) -> Option<FaceData> {
        let fam = family.trim().to_lowercase();
        let mut want = vec![fam.clone()];
        if let Some((_, subs)) = SUBSTITUTES.iter().find(|(n, _)| *n == fam) {
            want.extend(subs.iter().map(|s| s.to_string()));
        }
        let mono = ["mono", "courier", "consol", "code"].iter().any(|k| fam.contains(k));
        let serif = !fam.contains("sans") && ["serif", "times", "roman", "cambria", "georgia", "garamond", "book", "mincho", "song", "ming"].iter().any(|k| fam.contains(k));
        want.extend((if mono { MONO } else if serif { SERIF } else { SANS }).iter().map(|s| s.to_string()));
        self.first_of(&want, bold, italic).or_else(|| self.any(bold, italic))
    }

    /// A face that has `ch`, for characters the chosen one lacks (Chinese,
    /// Japanese, Korean, symbols).
    #[cfg_attr(not(feature = "ooxml"), allow(dead_code))]
    pub(crate) fn for_char(&self, ch: char, bold: bool) -> Option<FaceData> {
        let cjk = matches!(ch as u32, 0x1100..=0x11FF | 0x2E80..=0x9FFF | 0xAC00..=0xD7AF | 0xF900..=0xFAFF | 0xFF00..=0xFFEF | 0x20000..=0x2FFFF);
        let list: &[&str] = if cjk { CJK } else { &["dejavu sans", "noto sans symbols", "noto sans symbols2", "symbola", "segoe ui symbol", "liberation sans"] };
        self.first_of(list, bold, false)
    }

    /// Any font at all.
    fn any(&self, bold: bool, italic: bool) -> Option<FaceData> {
        let first: Vec<&String> = self.families().keys().take(1).collect();
        self.first_of(&first, bold, italic)
    }

    /// The face for a font a PDF names but doesn't embed (`base` is its
    /// PostScript name): metric-compatible faces for the standard 14
    /// fonts, the family it names if installed, or one like it.
    #[cfg(feature = "pdf")]
    pub(crate) fn pdf_font(&self, family: &str, bold: bool, italic: bool, serif: bool, mono: bool, base: &str) -> Option<FaceData> {
        let name = base.split_once('+').filter(|(t, _)| t.len() == 6).map_or(base, |(_, r)| r).to_ascii_lowercase();
        let name = name.replace(['-', ',', '_'], " ");
        let family = family.to_lowercase();
        let mut want: Vec<String> = Vec::new();
        // The standard 14: URW's clones of them first (the shapes, as
        // pdfium's own fonts have them), then fonts with their widths.
        let std14: Option<&[&str]> = if name.starts_with("helvetica") {
            Some(&["nimbus sans", "liberation sans", "arimo", "arial", "dejavu sans"])
        } else if name.starts_with("arial") {
            Some(SANS)
        } else if name.starts_with("times") {
            Some(&["nimbus roman", "liberation serif", "tinos", "times new roman", "dejavu serif"])
        } else if name.starts_with("courier") {
            Some(&["nimbus mono ps", "liberation mono", "cousine", "courier new", "dejavu sans mono"])
        } else if name.starts_with("symbol") {
            Some(&["standard symbols ps", "symbol", "opensymbol"])
        } else if name.contains("dingbats") {
            Some(&["d050000l", "dingbats", "opensymbol"])
        } else {
            None
        };
        match std14 {
            Some(list) => {
                // The family itself first (Arial, Times New Roman):
                // composite fonts reach its glyphs by number.
                if name.replace(' ', "").starts_with(&family.replace(' ', "")) {
                    want.push(family.clone());
                }
                want.extend(list.iter().map(|s| s.to_string()));
            }
            None => {
                want.push(family.clone());
                if let Some((_, subs)) = SUBSTITUTES.iter().find(|(n, _)| *n == family) {
                    want.extend(subs.iter().map(|s| s.to_string()));
                }
                if CJK_NAMES.iter().any(|c| name.contains(c)) || !base.is_ascii() {
                    // Mincho, Ming and Song faces are serif.
                    if CJK_SERIF.iter().any(|c| name.contains(c)) || serif {
                        want.extend(["noto serif cjk jp", "noto serif cjk sc"].map(String::from));
                    }
                    want.extend(CJK.iter().map(|s| s.to_string()));
                }
                let list = if mono {
                    MONO
                } else if serif {
                    SERIF
                } else {
                    SANS
                };
                want.extend(list.iter().map(|s| s.to_string()));
            }
        }
        self.first_of(&want, bold, italic)
    }
}

fn system_dirs() -> Vec<PathBuf> {
    let mut v = Vec::new();
    if cfg!(windows) {
        let root = std::env::var_os("WINDIR").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("C:\\Windows"));
        v.push(root.join("Fonts"));
        if let Some(l) = std::env::var_os("LOCALAPPDATA") {
            v.push(PathBuf::from(l).join("Microsoft\\Windows\\Fonts"));
        }
    } else if cfg!(target_os = "macos") {
        v.push("/System/Library/Fonts/Supplemental".into());
        v.push("/Library/Fonts".into());
        v.push("/System/Library/Fonts".into());
    } else if cfg!(target_os = "android") {
        v.push("/system/fonts".into());
        v.push("/product/fonts".into());
    } else {
        v.push("/usr/share/fonts".into());
        v.push("/usr/local/share/fonts".into());
    }
    if let Some(h) = std::env::var_os("HOME") {
        let h = PathBuf::from(h);
        v.push(h.join(".local/share/fonts"));
        v.push(h.join(".fonts"));
        v.push(h.join("Library/Fonts"));
    }
    v
}

fn scan(dir: &Path, depth: u32, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        if out.len() >= 20_000 {
            return;
        }
        let p = e.path();
        let Ok(ft) = e.file_type() else { continue };
        if ft.is_dir() {
            if depth > 0 {
                scan(&p, depth - 1, out);
            }
        } else if let Some(name) = p.file_name().and_then(|n| n.to_str()) {
            let lower = name.to_ascii_lowercase();
            if lower.ends_with(".ttf") || lower.ends_with(".otf") || lower.ends_with(".ttc") || lower.ends_with(".otc") {
                out.push(p);
            }
        }
    }
}

/// One face of an installed font file.
struct FaceInfo {
    family: String,
    path: PathBuf,
    index: u32,
    weight: u16,
    italic: bool,
}

fn be16(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_be_bytes([*b.get(at)?, *b.get(at + 1)?]))
}

fn be32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes([*b.get(at)?, *b.get(at + 1)?, *b.get(at + 2)?, *b.get(at + 3)?]))
}

fn read_at(f: &mut std::fs::File, at: u64, len: usize) -> Option<Vec<u8>> {
    if len > 4 << 20 {
        return None;
    }
    f.seek(SeekFrom::Start(at)).ok()?;
    let mut v = vec![0; len];
    f.read_exact(&mut v).ok()?;
    Some(v)
}

/// The family name, weight and slant of the faces in a font file, read
/// from the table directory and the `name` and `OS/2` tables only.
fn read_faces(path: &Path) -> Vec<FaceInfo> {
    let mut out = Vec::new();
    let Ok(mut f) = std::fs::File::open(path) else { return out };
    let Some(head) = read_at(&mut f, 0, 12) else { return out };
    let offsets: Vec<u32> = if &head[..4] == b"ttcf" {
        let n = be32(&head, 8).unwrap_or(0).min(64);
        match read_at(&mut f, 12, n as usize * 4) {
            Some(b) => (0..n as usize).filter_map(|i| be32(&b, i * 4)).collect(),
            None => return out,
        }
    } else {
        vec![0]
    };
    for (index, off) in offsets.into_iter().enumerate() {
        if let Some((family, weight, italic)) = read_face(&mut f, off as u64) {
            out.push(FaceInfo { family, path: path.to_path_buf(), index: index as u32, weight, italic });
        }
    }
    out
}

fn read_face(f: &mut std::fs::File, off: u64) -> Option<(String, u16, bool)> {
    let dir = read_at(f, off, 12)?;
    let n = be16(&dir, 4)? as usize;
    let recs = read_at(f, off + 12, n.min(64) * 16)?;
    let mut name = None;
    let mut os2 = None;
    for i in 0..n.min(64) {
        let r = recs.get(i * 16..)?;
        let (o, l) = (be32(r, 8)? as u64, be32(r, 12)? as usize);
        match r.get(..4)? {
            b"name" => name = Some((o, l)),
            b"OS/2" => os2 = Some((o, l)),
            _ => {}
        }
    }
    let (no, nl) = name?;
    let nb = read_at(f, no, nl)?;
    let count = be16(&nb, 2)? as usize;
    let strings = be16(&nb, 4)? as usize;
    // Best string per name id: Windows English, then any Windows, then Mac.
    let mut best: HashMap<u16, (u8, String)> = HashMap::new();
    for i in 0..count {
        let r = 6 + i * 12;
        let (plat, enc, lang, id) = (be16(&nb, r)?, be16(&nb, r + 2)?, be16(&nb, r + 4)?, be16(&nb, r + 6)?);
        if !matches!(id, 1 | 2 | 16 | 17) {
            continue;
        }
        let (len, at) = (be16(&nb, r + 8)? as usize, be16(&nb, r + 10)? as usize);
        let Some(raw) = nb.get(strings + at..strings + at + len) else { continue };
        let (rank, s) = match (plat, enc) {
            (3, 0 | 1 | 10) | (0, _) => {
                let units: Vec<u16> = raw.chunks_exact(2).map(|c| u16::from_be_bytes([c[0], c[1]])).collect();
                (if plat == 3 && lang == 0x409 { 3 } else { 2 }, String::from_utf16_lossy(&units))
            }
            (1, 0) => (1, raw.iter().map(|&b| b as char).collect()),
            _ => continue,
        };
        if best.get(&id).is_none_or(|(r, _)| *r < rank) {
            best.insert(id, (rank, s));
        }
    }
    let family = best.get(&16).or_else(|| best.get(&1))?.1.trim().to_string();
    if family.is_empty() || family.starts_with('.') {
        return None;
    }
    let sub = best.get(&17).or_else(|| best.get(&2)).map(|x| x.1.to_ascii_lowercase()).unwrap_or_default();
    let (mut weight, mut italic) = (if sub.contains("bold") { 700 } else { 400 }, sub.contains("italic") || sub.contains("oblique"));
    if let Some((o, l)) = os2
        && let Some(b) = read_at(f, o, l.min(64))
        && let (Some(w), Some(sel)) = (be16(&b, 4), be16(&b, 62))
    {
        weight = w;
        italic = sel & 1 != 0 || italic;
    }
    Some((family, weight, italic))
}
