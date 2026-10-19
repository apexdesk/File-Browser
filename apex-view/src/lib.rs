//! View-only renderer of office documents and PDF files: each page drawn
//! as RGBA pixels, and its text. Nothing is ever written, run or fetched;
//! every file is treated as untrusted and read within [`Limits`].
//!
//! ```no_run
//! let mut doc = apex_view::Document::open_path("report.docx".as_ref())?;
//! let thumb = doc.render_page(0, 128, &|| false)?;
//! assert_eq!(thumb.rgba.len(), (thumb.width * thumb.height * 4) as usize);
//! # Ok::<(), apex_view::Error>(())
//! ```

// Parts shared between formats are partly unused when only some are built.
#![cfg_attr(
    not(all(feature = "docx", feature = "xlsx", feature = "pptx", feature = "pdf", feature = "odt", feature = "ods", feature = "odp", feature = "rtf", feature = "doc", feature = "xls", feature = "ppt")),
    allow(dead_code, unused_imports)
)]

#[cfg(not(any(feature = "docx", feature = "xlsx", feature = "pptx", feature = "pdf", feature = "odt", feature = "ods", feature = "odp", feature = "rtf", feature = "doc", feature = "xls", feature = "ppt")))]
compile_error!("apex-view reads nothing without at least one format feature");

mod error;
mod font_check;
mod fonts;
mod limits;
mod text;

#[cfg(feature = "pdf")]
mod crypto;
#[cfg(feature = "flow")]
mod docx;
#[cfg(any(feature = "rtf", feature = "doc", feature = "xls", feature = "ppt"))]
mod codepage;
#[cfg(feature = "rtf")]
mod rtf;
#[cfg(feature = "doc")]
mod doc;
#[cfg(feature = "xls")]
mod xls;
#[cfg(feature = "ppt")]
mod ppt;
#[cfg(any(feature = "doc", feature = "xls", feature = "ppt"))]
mod ole;
#[cfg(feature = "ooxml")]
mod draw;
#[cfg(feature = "ooxml")]
mod images;
#[cfg(feature = "ooxml")]
mod ooxml;
#[cfg(any(feature = "pptx", feature = "odp", feature = "ppt"))]
mod geom;
#[cfg(feature = "pptx")]
mod pptx;
#[cfg(any(feature = "odt", feature = "ods", feature = "odp"))]
mod odf;
#[cfg(feature = "ooxml")]
mod typeset;
#[cfg(feature = "grid")]
mod xlsx;
#[cfg(feature = "pdf")]
mod pdf;

use std::path::Path;

pub use error::Error;
pub use fonts::Fonts;
pub use limits::Limits;
pub use text::TextRun;

/// A page drawn: `width` × `height` pixels, RGBA8 row by row, opaque.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// The kinds of file this crate reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Format {
    Pdf,
    Docx,
    Xlsx,
    Pptx,
    Odt,
    Ods,
    Odp,
    Rtf,
    Doc,
    Xls,
    Ppt,
}

enum Inner {
    #[cfg(feature = "pdf")]
    Pdf(pdf::Doc),
    #[cfg(feature = "docx")]
    Docx(docx::Doc),
    #[cfg(feature = "xlsx")]
    Xlsx(xlsx::Doc),
    #[cfg(feature = "pptx")]
    Pptx(pptx::Doc),
    #[cfg(feature = "ods")]
    Ods(odf::ods::Doc),
    #[cfg(feature = "xls")]
    Xls(xls::Doc),
    #[cfg(feature = "ppt")]
    Ppt(ppt::Doc),
    #[cfg(feature = "odp")]
    Odp(odf::odp::Doc),
    /// Documents laid out like Word's: .odt, .rtf, .doc.
    #[cfg(feature = "flow")]
    Flow(Format, docx::Doc),
}

/// An open document. Opening reads only what is needed to count the
/// pages; each page is read and laid out when first asked for.
pub struct Document {
    inner: Inner,
    fonts: Fonts,
    limits: Limits,
}

impl Document {
    /// Opens the file at `path` (see [`Document::open_bytes`]).
    pub fn open_path(path: &Path) -> Result<Document, Error> {
        Document::open_path_with(path, Limits::default(), Fonts::system())
    }

    /// Opens a document from its bytes, with the default limits and the
    /// system's fonts.
    pub fn open_bytes(data: Vec<u8>) -> Result<Document, Error> {
        Document::open_with(data, Limits::default(), Fonts::system())
    }

    pub fn open_path_with(path: &Path, limits: Limits, fonts: Fonts) -> Result<Document, Error> {
        let len = std::fs::metadata(path)?.len();
        if len > limits.max_file_bytes {
            return Err(Error::TooLarge);
        }
        Document::open_with(std::fs::read(path)?, limits, fonts)
    }

    /// Opens a document from its bytes. `fonts` can be shared by every
    /// document opened (the system's fonts are looked up once).
    pub fn open_with(data: Vec<u8>, limits: Limits, fonts: Fonts) -> Result<Document, Error> {
        if data.len() as u64 > limits.max_file_bytes {
            return Err(Error::TooLarge);
        }
        if encrypted_package(&data) {
            return Err(Error::Encrypted);
        }
        let inner = match sniff(&data) {
            #[cfg(feature = "pdf")]
            Some(Format::Pdf) => Inner::Pdf(pdf::Doc::open(data, &limits)?),
            #[cfg(feature = "docx")]
            Some(Format::Docx) => Inner::Docx(docx::Doc::open(data, &limits, &fonts)?),
            #[cfg(feature = "xlsx")]
            Some(Format::Xlsx) => Inner::Xlsx(xlsx::Doc::open(data, &limits, &fonts)?),
            #[cfg(feature = "pptx")]
            Some(Format::Pptx) => Inner::Pptx(pptx::Doc::open(data, &limits, &fonts)?),
            #[cfg(feature = "ods")]
            Some(Format::Ods) => Inner::Ods(odf::ods::Doc::open(data, &limits, &fonts)?),
            #[cfg(feature = "xls")]
            Some(Format::Xls) => Inner::Xls(xls::Doc::open(data, &limits, &fonts)?),
            #[cfg(feature = "ppt")]
            Some(Format::Ppt) => Inner::Ppt(ppt::Doc::open(data, &limits, &fonts)?),
            #[cfg(feature = "odp")]
            Some(Format::Odp) => Inner::Odp(odf::odp::Doc::open(data, &limits, &fonts)?),
            #[cfg(feature = "odt")]
            Some(Format::Odt) => Inner::Flow(Format::Odt, odf::odt::open(data, &limits, &fonts)?),
            #[cfg(feature = "rtf")]
            Some(Format::Rtf) => Inner::Flow(Format::Rtf, rtf::open(data, &limits, &fonts)?),
            #[cfg(feature = "doc")]
            Some(Format::Doc) => Inner::Flow(Format::Doc, doc::open(data, &limits, &fonts)?),
            _ => return Err(Error::Unsupported),
        };
        Ok(Document { inner, fonts, limits })
    }

    pub fn format(&self) -> Format {
        match &self.inner {
            #[cfg(feature = "pdf")]
            Inner::Pdf(_) => Format::Pdf,
            #[cfg(feature = "docx")]
            Inner::Docx(_) => Format::Docx,
            #[cfg(feature = "xlsx")]
            Inner::Xlsx(_) => Format::Xlsx,
            #[cfg(feature = "pptx")]
            Inner::Pptx(_) => Format::Pptx,
            #[cfg(feature = "flow")]
            Inner::Flow(f, _) => *f,
            #[cfg(feature = "ods")]
            Inner::Ods(_) => Format::Ods,
            #[cfg(feature = "xls")]
            Inner::Xls(_) => Format::Xls,
            #[cfg(feature = "ppt")]
            Inner::Ppt(_) => Format::Ppt,
            #[cfg(feature = "odp")]
            Inner::Odp(_) => Format::Odp,
        }
    }

    pub fn page_count(&self) -> usize {
        match &self.inner {
            #[cfg(feature = "pdf")]
            Inner::Pdf(d) => d.page_count(),
            #[cfg(feature = "docx")]
            Inner::Docx(d) => d.page_count(),
            #[cfg(feature = "xlsx")]
            Inner::Xlsx(d) => d.page_count(),
            #[cfg(feature = "pptx")]
            Inner::Pptx(d) => d.page_count(),
            #[cfg(feature = "flow")]
            Inner::Flow(_, d) => d.page_count(),
            #[cfg(feature = "ods")]
            Inner::Ods(d) => d.page_count(),
            #[cfg(feature = "xls")]
            Inner::Xls(d) => d.page_count(),
            #[cfg(feature = "ppt")]
            Inner::Ppt(d) => d.page_count(),
            #[cfg(feature = "odp")]
            Inner::Odp(d) => d.page_count(),
        }
    }

    /// The size of page `n` in points (1/72 inch), as it is drawn.
    pub fn page_size(&mut self, n: usize) -> Result<(f32, f32), Error> {
        self.check_page(n)?;
        match &mut self.inner {
            #[cfg(feature = "pdf")]
            Inner::Pdf(d) => d.page_size(n),
            #[cfg(feature = "docx")]
            Inner::Docx(d) => d.page_size(n),
            #[cfg(feature = "xlsx")]
            Inner::Xlsx(d) => d.page_size(n),
            #[cfg(feature = "pptx")]
            Inner::Pptx(d) => d.page_size(n),
            #[cfg(feature = "flow")]
            Inner::Flow(_, d) => d.page_size(n),
            #[cfg(feature = "ods")]
            Inner::Ods(d) => d.page_size(n),
            #[cfg(feature = "xls")]
            Inner::Xls(d) => d.page_size(n),
            #[cfg(feature = "ppt")]
            Inner::Ppt(d) => d.page_size(n),
            #[cfg(feature = "odp")]
            Inner::Odp(d) => d.page_size(n),
        }
    }

    /// Draws page `n` (from 0) `width` pixels wide; the height keeps the
    /// page's proportions. `cancel` is called now and then while drawing:
    /// when it returns true the drawing stops with [`Error::Cancelled`].
    pub fn render_page(&mut self, n: usize, width: u32, cancel: &dyn Fn() -> bool) -> Result<Image, Error> {
        self.check_page(n)?;
        let width = width.clamp(1, self.limits.max_render_width.max(1));
        match &mut self.inner {
            #[cfg(feature = "pdf")]
            Inner::Pdf(d) => d.render(n, width, &self.fonts, &self.limits, cancel),
            #[cfg(feature = "docx")]
            Inner::Docx(d) => d.render(n, width, cancel),
            #[cfg(feature = "xlsx")]
            Inner::Xlsx(d) => d.render(n, width, cancel),
            #[cfg(feature = "pptx")]
            Inner::Pptx(d) => d.render(n, width, cancel),
            #[cfg(feature = "flow")]
            Inner::Flow(_, d) => d.render(n, width, cancel),
            #[cfg(feature = "ods")]
            Inner::Ods(d) => d.render(n, width, cancel),
            #[cfg(feature = "xls")]
            Inner::Xls(d) => d.render(n, width, cancel),
            #[cfg(feature = "ppt")]
            Inner::Ppt(d) => d.render(n, width, cancel),
            #[cfg(feature = "odp")]
            Inner::Odp(d) => d.render(n, width, cancel),
        }
    }

    /// The text of page `n`, in reading order, lines ended by '\n'.
    pub fn page_text(&mut self, n: usize) -> Result<String, Error> {
        Ok(text::plain(&self.page_runs(n)?))
    }

    /// The text of page `n` in runs placed on the page.
    pub fn page_runs(&mut self, n: usize) -> Result<Vec<TextRun>, Error> {
        self.check_page(n)?;
        match &mut self.inner {
            #[cfg(feature = "pdf")]
            Inner::Pdf(d) => d.runs(n),
            #[cfg(feature = "docx")]
            Inner::Docx(d) => d.runs(n),
            #[cfg(feature = "xlsx")]
            Inner::Xlsx(d) => d.runs(n),
            #[cfg(feature = "pptx")]
            Inner::Pptx(d) => d.runs(n),
            #[cfg(feature = "flow")]
            Inner::Flow(_, d) => d.runs(n),
            #[cfg(feature = "ods")]
            Inner::Ods(d) => d.runs(n),
            #[cfg(feature = "xls")]
            Inner::Xls(d) => d.runs(n),
            #[cfg(feature = "ppt")]
            Inner::Ppt(d) => d.runs(n),
            #[cfg(feature = "odp")]
            Inner::Odp(d) => d.runs(n),
        }
    }

    fn check_page(&self, n: usize) -> Result<(), Error> {
        if n < self.page_count() { Ok(()) } else { Err(Error::NoSuchPage) }
    }
}

/// The format of a file by its first bytes (and, for zip packages, the
/// names of their parts).
pub fn sniff(data: &[u8]) -> Option<Format> {
    let head = &data[..data.len().min(1024)];
    if head.trim_ascii_start().starts_with(b"{\\rtf") {
        return Some(Format::Rtf);
    }
    if memchr::memmem::find(head, b"%PDF-").is_some() {
        return Some(Format::Pdf);
    }
    #[cfg(any(feature = "doc", feature = "xls", feature = "ppt"))]
    if ole::is_ole(data)
        && let Ok(f) = ole::Ole::open(data, &Limits::default())
    {
        if f.has("WordDocument") {
            return Some(Format::Doc);
        }
        if f.has("Workbook") || f.has("Book") {
            return Some(Format::Xls);
        }
        if f.has("PowerPoint Document") {
            return Some(Format::Ppt);
        }
    }
    #[cfg(feature = "ooxml")]
    if data.starts_with(b"PK\x03\x04")
        && let Ok(z) = ooxml::ZipReader::new(data, &Limits::default())
    {
        if z.contains("word/document.xml") {
            return Some(Format::Docx);
        }
        if z.contains("xl/workbook.xml") {
            return Some(Format::Xlsx);
        }
        if z.contains("ppt/presentation.xml") {
            return Some(Format::Pptx);
        }
        // The package's type: its `mimetype`, else the media type of its
        // manifest's root entry.
        let kind = z.read("mimetype").ok().and_then(|m| String::from_utf8(m).ok()).or_else(|| {
            let m = z.read("META-INF/manifest.xml").ok()?;
            let m = String::from_utf8_lossy(&m);
            let at = m.find("full-path=\"/\"")?;
            let tag = &m[m[..at].rfind('<')?..at + m[at..].find('>')?];
            let t = &tag[tag.find("media-type=\"")? + 12..];
            Some(t[..t.find('"')?].to_string())
        });
        if let Some(m) = kind {
            let m = m.trim();
            if m.starts_with("application/vnd.oasis.opendocument.text") {
                return Some(Format::Odt);
            }
            if m.starts_with("application/vnd.oasis.opendocument.spreadsheet") {
                return Some(Format::Ods);
            }
            if m.starts_with("application/vnd.oasis.opendocument.presentation") || m.starts_with("application/vnd.oasis.opendocument.graphics") {
                return Some(Format::Odp);
            }
        }
    }
    None
}

/// An Office file protected by a password: an OLE container holding the
/// encrypted package.
fn encrypted_package(data: &[u8]) -> bool {
    const OLE: &[u8] = &[0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];
    let name: Vec<u8> = "EncryptedPackage".encode_utf16().flat_map(u16::to_le_bytes).collect();
    data.starts_with(OLE) && memchr::memmem::find(data, &name).is_some()
}
