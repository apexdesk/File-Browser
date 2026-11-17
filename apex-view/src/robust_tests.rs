//! Hostile files: each must open (or fail) and draw without a panic, an
//! overflow, a hang or a huge allocation. The inputs are built here.

use std::sync::mpsc;
use std::time::{Duration, Instant};

use crate::{Document, Error, Fonts, Image, Limits};

/// Opens `data` and draws its first page `width` pixels wide, in a thread
/// that must finish within `secs` seconds.
fn run(data: Vec<u8>, width: u32, secs: u64) -> Result<Image, Error> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let r = (|| {
            let mut doc = Document::open_with(data, Limits::default(), Fonts::none())?;
            doc.page_size(0)?;
            doc.render_page(0, width, &|| false)
        })();
        let _ = tx.send(r);
    });
    match rx.recv_timeout(Duration::from_secs(secs)) {
        Ok(r) => r,
        Err(mpsc::RecvTimeoutError::Timeout) => panic!("took over {secs} s"),
        Err(mpsc::RecvTimeoutError::Disconnected) => panic!("panicked"),
    }
}

/// A PDF of objects 1, 2... with a cross-reference table.
fn pdf(objs: &[Vec<u8>]) -> Vec<u8> {
    let mut out = b"%PDF-1.4\n".to_vec();
    let mut offs = Vec::new();
    for (i, o) in objs.iter().enumerate() {
        offs.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n", i + 1).as_bytes());
        out.extend_from_slice(o);
        out.extend_from_slice(b"\nendobj\n");
    }
    let xref = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).as_bytes());
    for o in offs {
        out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objs.len() + 1).as_bytes());
    out
}

fn stream(dict: &str, data: &[u8]) -> Vec<u8> {
    let mut s = format!("<< {dict} /Length {} >>\nstream\n", data.len()).into_bytes();
    s.extend_from_slice(data);
    s.extend_from_slice(b"\nendstream");
    s
}

/// A one-page PDF (200 x 200 points): its resources, its content and
/// objects 5 on.
fn page_pdf(res: &str, content: &[u8], more: &[Vec<u8>]) -> Vec<u8> {
    page_pdf_annots("", res, content, more)
}

/// Same, with annotations on the page.
fn page_pdf_annots(annots: &str, res: &str, content: &[u8], more: &[Vec<u8>]) -> Vec<u8> {
    let mut objs = vec![
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
        format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Resources << {res} >> /Contents 4 0 R /Annots [{annots}] >>").into_bytes(),
        stream("", content),
    ];
    objs.extend_from_slice(more);
    pdf(&objs)
}

fn annot_pdf(annots: &str, more: &[Vec<u8>]) -> Vec<u8> {
    page_pdf_annots(annots, "", b"", more)
}

/// A zip of stored parts (no CRC: the reader doesn't check it).
fn zip(parts: &[(&str, &[u8])]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut dir = Vec::new();
    for (name, data) in parts {
        let off = out.len() as u32;
        let fixed = |out: &mut Vec<u8>| {
            out.extend_from_slice(&[20, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
            out.extend_from_slice(&(data.len() as u32).to_le_bytes());
            out.extend_from_slice(&(data.len() as u32).to_le_bytes());
            out.extend_from_slice(&(name.len() as u16).to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes());
        };
        out.extend_from_slice(b"PK\x03\x04");
        fixed(&mut out);
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(data);
        dir.extend_from_slice(b"PK\x01\x02\x14\x00");
        fixed(&mut dir);
        dir.extend_from_slice(&[0; 10]);
        dir.extend_from_slice(&off.to_le_bytes());
        dir.extend_from_slice(name.as_bytes());
    }
    let at = out.len() as u32;
    out.extend_from_slice(&dir);
    out.extend_from_slice(b"PK\x05\x06\0\0\0\0");
    out.extend_from_slice(&(parts.len() as u16).to_le_bytes());
    out.extend_from_slice(&(parts.len() as u16).to_le_bytes());
    out.extend_from_slice(&(dir.len() as u32).to_le_bytes());
    out.extend_from_slice(&at.to_le_bytes());
    out.extend_from_slice(&[0; 2]);
    out
}

// The five files reproduced.

#[test]
fn xref_stream_of_zero_widths() {
    let d = b"%PDF-1.5\n1 0 obj\n<< /Type /XRef /W [0 0 0 1] /Size 1 /Root 2 0 R /Length 4 >>\nstream\nAAAA\nendstream\nendobj\nstartxref\n9\n%%EOF\n";
    let _ = run(d.to_vec(), 64, 60);
}

#[test]
fn zip64_locator_at_the_end_of_memory() {
    let mut d = b"PK\x03\x04".to_vec();
    d.extend_from_slice(&[0; 26]);
    // Zip64 end of directory locator: disk, offset, disks.
    d.extend_from_slice(b"PK\x06\x07");
    d.extend_from_slice(&0u32.to_le_bytes());
    d.extend_from_slice(&u64::MAX.to_le_bytes());
    d.extend_from_slice(&1u32.to_le_bytes());
    // End of directory, all counts and offsets saying "see Zip64".
    d.extend_from_slice(b"PK\x05\x06");
    d.extend_from_slice(&[0xFF; 16]);
    d.extend_from_slice(&[0; 2]);
    assert!(crate::sniff(&d).is_none());
    assert!(run(d, 64, 60).is_err());
}

#[test]
fn huge_white_image_drawn_small_stays_white() {
    let (w, h) = (8000usize, 5000usize);
    let z = miniz_oxide::deflate::compress_to_vec_zlib(&vec![0xFF; w * h], 1);
    let img = stream(&format!("/Type /XObject /Subtype /Image /Width {w} /Height {h} /ColorSpace /DeviceGray /BitsPerComponent 8 /Filter /FlateDecode"), &z);
    let d = page_pdf("/XObject << /Im0 5 0 R >>", b"0 g 0 0 200 200 re f q 10 0 0 6 100 100 cm /Im0 Do Q", &[img]);
    let p = run(d, 200, 120).unwrap();
    let px = |x: u32, y: u32| p.rgba[((y * p.width + x) * 4) as usize];
    // Points are pixels here: the image is at x 100..110, y 94..100.
    assert!(px(105, 97) > 230, "{}", px(105, 97));
    assert!(px(50, 50) < 20);
}

#[test]
fn startxref_before_the_file() {
    let mut d = b"junk junk junk\n".to_vec();
    let body = page_pdf("", b"0 0 10 10 re f", &[]);
    let at = memchr::memmem::rfind(&body, b"startxref").unwrap();
    d.extend_from_slice(&body[..at]);
    d.extend_from_slice(b"startxref\n-1\n%%EOF\n");
    let _ = run(d, 64, 60);
}

#[test]
fn rtf_table_at_the_ends_of_i32() {
    let d = br"{\rtf1\ansi \trowd\trleft-2147483647\cellx2147483647\cellx-2147483648 a\cell b\cell\row}";
    let _ = run(d.to_vec(), 64, 60);
}

// Findings.

#[test]
fn chain_of_indirect_lengths() {
    // Each stream's /Length is the next stream object: thousands deep.
    let n = 20_000;
    let mut more = Vec::new();
    for k in 0..n {
        let next = 5 + k + 1;
        if k + 1 == n {
            more.push(b"4".to_vec());
        } else {
            more.push(format!("<< /Length {next} 0 R >>\nstream\nabcd\nendstream").into_bytes());
        }
    }
    let mut objs = vec![
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R >>".to_vec(),
        b"<< /Length 5 0 R >>\nstream\n0 0 10 10 re f\nendstream".to_vec(),
    ];
    objs.extend(more);
    let _ = run(pdf(&objs), 64, 60);
}

#[test]
fn tiling_far_from_the_page() {
    let pat = stream("/PatternType 1 /PaintType 1 /TilingType 1 /BBox [0 0 1 1] /XStep 1 /YStep 1 /Matrix [1 0 0 1 1e18 1e18] /Resources << >>", b"0 g 0 0 1 1 re f");
    let d = page_pdf("/Pattern << /P0 5 0 R >>", b"/Pattern cs /P0 scn 0 0 200 200 re f", &[pat]);
    let _ = run(d, 64, 60);
}

#[test]
fn tiling_of_tiny_steps() {
    let pat = stream("/PatternType 1 /PaintType 1 /TilingType 1 /BBox [0 0 1e-30 1e-30] /XStep 1e-30 /YStep 1e-30 /Resources << >>", b"0 g 0 0 1 1 re f");
    let d = page_pdf("/Pattern << /P0 5 0 R >>", b"/Pattern cs /P0 scn 0 0 200 200 re f", &[pat]);
    let _ = run(d, 64, 60);
}

#[test]
fn squiggly_as_wide_as_the_floats() {
    let d = annot_pdf("5 0 R 6 0 R", &[
        b"<< /Type /Annot /Subtype /Squiggly /Rect [0 0 200 200] /QuadPoints [-1e300 20 1e300 20 -1e300 10 1e300 10] >>".to_vec(),
        format!("<< /Type /Annot /Subtype /Squiggly /Rect [0 0 200 200] /QuadPoints [0 20 {}0 20 0 10 1 10] >>", "9".repeat(400)).into_bytes(),
    ]);
    let _ = run(d, 64, 60);
}

#[test]
fn ink_of_one_path_named_over_and_over() {
    let pts: String = (0..100_000).map(|i| format!("{} ", i % 200)).collect();
    let list = "7 0 R ".repeat(1000);
    let d = annot_pdf("5 0 R", &[
        format!("<< /Type /Annot /Subtype /Ink /Rect [0 0 200 200] /InkList 6 0 R >>").into_bytes(),
        format!("[{list}]").into_bytes(),
        format!("[{pts}]").into_bytes(),
    ]);
    let _ = run(d, 64, 60);
}

#[test]
fn sampled_function_read_over_and_over() {
    // 4M samples of 8 bits, in a shading drawn 300 times.
    let f = stream("/FunctionType 0 /Domain [0 1] /Range [0 1] /Size [4000000] /BitsPerSample 8", b"");
    let sh = "<< /ShadingType 2 /ColorSpace /DeviceGray /Coords [0 0 200 0] /Function 5 0 R >>";
    let content = b"/Sh0 sh\n".repeat(300);
    let d = page_pdf(&format!("/Shading << /Sh0 {sh} >>"), &content, &[f]);
    let _ = run(d, 64, 60);
}

#[test]
fn hex_string_cut_short() {
    let d = page_pdf("", b"BT /F1 12 Tf <414", &[]);
    let _ = run(d, 64, 60);
    let _ = run(b"%PDF-1.4\n1 0 obj <</A <4".to_vec(), 64, 60);
}

// Mutations of small valid files.

/// xorshift64*: deterministic, no dependency.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

const TOKENS: &[&[u8]] = &[b"-1", b"0", b"4294967295", b"-2147483648", b"2147483647", b"99999999999999999999", b"1e308", b"-1e308", b"65535", b"<<", b">>", b"[", b"]", b"R", b" 1 0 R", b"\\", b"{", b"}", b"<", b">", b"\"", b"\xFF\xFF\xFF\xFF", b"\0\0\0\0"];

fn mutate(rng: &mut Rng, seed: &[u8]) -> Vec<u8> {
    let mut d = seed.to_vec();
    for _ in 0..1 + rng.below(4) {
        if d.is_empty() {
            break;
        }
        let at = rng.below(d.len());
        match rng.below(6) {
            0 => d[at] ^= 1 << rng.below(8),
            1 => d[at] = rng.next() as u8,
            2 => d.truncate(at),
            3 => {
                // A piece of the file copied elsewhere.
                let len = rng.below(64).min(d.len() - at);
                let piece = d[at..at + len].to_vec();
                let to = rng.below(d.len());
                d.splice(to..to, piece);
            }
            4 => {
                let t = TOKENS[rng.below(TOKENS.len())];
                let end = (at + rng.below(4)).min(d.len());
                d.splice(at..end, t.iter().copied());
            }
            _ => {
                // A digit run swapped for a number at an edge.
                if let Some(s) = d[at..].iter().position(u8::is_ascii_digit) {
                    let s = at + s;
                    let e = s + d[s..].iter().take_while(|b| b.is_ascii_digit()).count();
                    let t = TOKENS[rng.below(7)];
                    d.splice(s..e, t.iter().copied());
                }
            }
        }
    }
    d
}

fn seeds() -> Vec<(&'static str, Vec<u8>)> {
    let img = stream("/Type /XObject /Subtype /Image /Width 4 /Height 2 /ColorSpace /DeviceRGB /BitsPerComponent 8", &[0x80; 24]);
    let font = b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /FirstChar 32 /LastChar 126 /Widths [278 278] >>".to_vec();
    let pat = stream("/PatternType 1 /PaintType 1 /TilingType 1 /BBox [0 0 10 10] /XStep 10 /YStep 10 /Resources << >>", b"1 0 0 rg 0 0 5 5 re f");
    let func = b"<< /FunctionType 2 /Domain [0 1] /C0 [0] /C1 [1] /N 1 >>".to_vec();
    let content = b"q 1 0 0 1 10 10 cm 0 0 1 rg 0 0 50 50 re f Q\nBT /F1 12 Tf 20 150 Td (Hello) Tj [(W) 120 (orld)] TJ ET\nq 40 0 0 20 100 100 cm /Im0 Do Q\n/Pattern cs /P0 scn 10 60 80 30 re f\n/Sh0 sh\n2 w 0 0 m 100 100 l 150 20 50 50 200 0 c S";
    let res = "/XObject << /Im0 5 0 R >> /Font << /F1 6 0 R >> /Pattern << /P0 7 0 R >> /Shading << /Sh0 << /ShadingType 2 /ColorSpace /DeviceGray /Coords [0 0 200 0] /Function 8 0 R >> >>";
    let annot = b"<< /Type /Annot /Subtype /Squiggly /Rect [10 10 90 30] /QuadPoints [10 30 90 30 10 10 90 10] >>".to_vec();
    let p = page_pdf_annots("9 0 R", res, content, &[img, font, pat, func, annot]);
    let rtf = br"{\rtf1\ansi\deff0{\fonttbl{\f0 Arial;}}{\colortbl;\red255\green0\blue0;}\paperw12240\paperh15840 {\pard\li720\fi-360\b Hello\b0  \cf1 world\par}\trowd\trgaph108\trleft-108\cellx2000\cellx4000 a\cell b\cell\row {\*\listtable{\list{\listlevel\levelnfc4{\leveltext\'02\'00.;}}}}\pard\ls1\ilvl0 item\par}".to_vec();
    let doc = br#"<?xml version="1.0"?><w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:pPr><w:ind w:left="720" w:hanging="360"/></w:pPr><w:r><w:rPr><w:sz w:val="24"/></w:rPr><w:t>Hello world</w:t></w:r></w:p><w:tbl><w:tblGrid><w:gridCol w:w="2000"/><w:gridCol w:w="3000"/></w:tblGrid><w:tr><w:tc><w:tcPr><w:gridSpan w:val="2"/></w:tcPr><w:p><w:r><w:t>a</w:t></w:r></w:p></w:tc></w:tr></w:tbl><w:sectPr><w:pgSz w:w="12240" w:h="15840"/><w:pgMar w:top="1440" w:bottom="1440" w:left="1440" w:right="1440"/><w:cols w:num="2"/></w:sectPr></w:body></w:document>"#;
    let docx = zip(&[("word/document.xml", doc)]);
    let sheet = br#"<?xml version="1.0"?><worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><cols><col min="1" max="3" width="12"/></cols><sheetData><row r="1"><c r="A1" t="inlineStr"><is><t>x</t></is></c><c r="B1" s="1"><v>44000.5</v></c><c r="C1"><v>1e10</v></c></row><row r="3"><c r="AA3"><v>-1</v></c></row></sheetData><mergeCells><mergeCell ref="A1:B2"/></mergeCells></worksheet>"#;
    let styles = br#"<?xml version="1.0"?><styleSheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><numFmts><numFmt numFmtId="164" formatCode="[$-409]dddd, mmmm dd, yyyy h:mm:ss.000 AM/PM;0.00E+00;@"/></numFmts><cellXfs><xf numFmtId="0"/><xf numFmtId="164"/></cellXfs></styleSheet>"#;
    let wb = br#"<?xml version="1.0"?><workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="S" sheetId="1" r:id="rId1"/></sheets></workbook>"#;
    let rels = br#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/></Relationships>"#;
    let xlsx = zip(&[("xl/workbook.xml", wb), ("xl/_rels/workbook.xml.rels", rels), ("xl/worksheets/sheet1.xml", sheet), ("xl/styles.xml", styles)]);
    vec![("pdf", p), ("rtf", rtf), ("docx", docx), ("xlsx", xlsx)]
}

/// Mutated files, `APEX_FUZZ_ITERS` of each kind (default 20000): none may
/// panic. A case taking over 10 s of drawing is cancelled and counted too.
#[test]
fn mutations_neither_panic_nor_hang() {
    let iters: usize = std::env::var("APEX_FUZZ_ITERS").ok().and_then(|v| v.parse().ok()).unwrap_or(20000);
    let seed: u64 = std::env::var("APEX_FUZZ_SEED").ok().and_then(|v| v.parse().ok()).unwrap_or(0x5EED_A9E7_1234_5678);
    let fonts = Fonts::none();
    let mut failed = Vec::new();
    for (kind, base) in seeds() {
        // The seed itself must read.
        let mut doc = Document::open_with(base.clone(), Limits::default(), fonts.clone()).unwrap_or_else(|e| panic!("{kind}: {e}"));
        doc.render_page(0, 48, &|| false).unwrap_or_else(|e| panic!("{kind}: {e}"));
        let mut rng = Rng(seed ^ kind.len() as u64 ^ (base.len() as u64) << 20);
        for i in 0..iters {
            let d = mutate(&mut rng, &base);
            let fonts = fonts.clone();
            let start = Instant::now();
            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let Ok(mut doc) = Document::open_with(d.clone(), Limits::default(), fonts) else { return false };
                let _ = doc.page_size(0);
                let deadline = Instant::now() + Duration::from_secs(10);
                let cancel = || Instant::now() > deadline;
                matches!(doc.render_page(0, 48, &cancel), Err(Error::Cancelled))
            }));
            let fail = match r {
                Err(_) => format!("{kind} #{i}: panicked"),
                Ok(true) => format!("{kind} #{i}: cancelled after {:?}", start.elapsed()),
                Ok(false) => continue,
            };
            // APEX_FUZZ_DUMP=dir keeps the files that failed there.
            if let Some(dir) = std::env::var_os("APEX_FUZZ_DUMP") {
                let _ = std::fs::write(std::path::Path::new(&dir).join(format!("{kind}-{i}")), &d);
            }
            failed.push(fail);
        }
    }
    assert!(failed.is_empty(), "{failed:#?}");
}

// Found by the mutations above (vello panics, debug and release alike).

#[test]
fn clip_past_the_right_edge_with_a_path_along_it() {
    // 48 x 1 pixels: x 9600 is the right edge.
    let objs = vec![
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 9600 200] /Contents 4 0 R >>".to_vec(),
        stream("", b"q 9600 0 5 200 re W n 0 G 1 w 9600 10 m 9600 100 l 9600 10 l 9600 100 l h f Q"),
    ];
    let _ = run(pdf(&objs), 48, 60);
    // The same through a markup annotation on a page of a negative width.
    let objs = vec![
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 -2147483648 200] /Contents 4 0 R /Annots [5 0 R] >>".to_vec(),
        stream("", b""),
        b"<< /Type /Annot /Subtype /Squiggly /Rect [10 10 90 30] /QuadPoints [10 30 90 30 10 10 90 10] >>".to_vec(),
    ];
    let _ = run(pdf(&objs), 48, 60);
}

#[test]
fn tiling_cell_far_from_its_origin() {
    let pat = stream("/PatternType 1 /PaintType 1 /TilingType 1 /BBox [-2147483648 0 10 10] /XStep 10 /YStep 10 /Resources << >>", b"1 0 0 rg 0 0 5 5 re f");
    let d = page_pdf("/Pattern << /P0 5 0 R >>", b"/Pattern cs /P0 scn 10 60 80 30 re f", &[pat]);
    let _ = run(d, 48, 60);
}
