//! PDF files: pages drawn by our own renderer, and their text.

mod appearance;
mod content;
mod file;
mod font;
mod image;
mod obj;
mod render;
mod tables;

use std::sync::Arc;

use crate::{Error, Fonts, Image, Limits, TextRun};

/// Errors inside the PDF reader, turned into [`Error`] at its edge.
#[derive(Debug)]
pub(crate) struct Fail(pub String);

impl Fail {
    pub fn new(msg: impl Into<String>) -> Fail {
        Fail(msg.into())
    }
}

const NEEDS_PASSWORD: &str = "the file is protected with a password";
const WRONG_PASSWORD: &str = "the password is not correct";

fn no_password() -> Fail {
    Fail::new(NEEDS_PASSWORD)
}

impl From<Fail> for Error {
    fn from(f: Fail) -> Error {
        match f.0.as_str() {
            NEEDS_PASSWORD | WRONG_PASSWORD => Error::Encrypted,
            "unsupported PDF encryption" => Error::Unsupported,
            _ => Error::Malformed(f.0),
        }
    }
}

/// An open PDF: its pages found, nothing drawn yet.
pub(crate) struct Doc {
    inner: render::Document,
    /// Text pages read, kept for the next call.
    text: Option<(usize, Vec<TextRun>)>,
}

impl Doc {
    pub fn open(data: Vec<u8>, _limits: &Limits) -> Result<Doc, Error> {
        let inner = render::Document::open(Arc::from(data), None)?;
        if inner.page_count() == 0 {
            return Err(Error::Malformed("the PDF file has no pages".into()));
        }
        Ok(Doc { inner, text: None })
    }

    pub fn page_count(&self) -> usize {
        self.inner.page_count()
    }

    pub fn page_size(&mut self, n: usize) -> Result<(f32, f32), Error> {
        let (w, h) = self.inner.page_size(n).ok_or(Error::NoSuchPage)?;
        Ok((w as f32, h as f32))
    }

    pub fn render(&mut self, n: usize, width: u32, fonts: &Fonts, limits: &Limits, cancel: &dyn Fn() -> bool) -> Result<Image, Error> {
        let fallback = |style: &font::Style, base: &str| fonts.pdf_font(&style.family, style.bold, style.italic, style.serif, style.mono, base);
        let r = self.inner.render(n, width, &fallback, limits, cancel)?;
        Ok(Image { width: r.width, height: r.height, rgba: r.rgba })
    }

    pub fn runs(&mut self, n: usize) -> Result<Vec<TextRun>, Error> {
        if let Some((at, runs)) = &self.text
            && *at == n
        {
            return Ok(runs.clone());
        }
        let page = self.inner.text_page(n).ok_or(Error::NoSuchPage)?;
        let (w, h) = (page.width, page.height);
        let chars = page.chars.iter().filter(|c| !c.ch.is_control()).map(|c| crate::text::Glyph { ch: c.ch, x: c.x, y: c.y, w: c.w, size: c.size });
        let runs = crate::text::runs(chars, w, h);
        self.text = Some((n, runs.clone()));
        Ok(runs)
    }
}

/// A PDF text string: UTF-16 or UTF-8 with a byte order mark, otherwise
/// PDFDocEncoding (Latin-1 for the characters that matter).
pub(crate) fn text_string(b: &[u8]) -> String {
    if let Some(rest) = b.strip_prefix(&[0xFE, 0xFF]) {
        let units = rest.chunks(2).map(|c| u16::from_be_bytes([c[0], *c.get(1).unwrap_or(&0)]));
        return char::decode_utf16(units).filter_map(Result::ok).collect();
    }
    if let Some(rest) = b.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        return String::from_utf8_lossy(rest).into_owned();
    }
    b.iter()
        .map(|&c| match c {
            0x18..=0x1F | 0x80..=0x9F => pdfdoc(c),
            c => c as char,
        })
        .collect()
}

fn pdfdoc(c: u8) -> char {
    const HIGH: [char; 32] = ['•', '†', '‡', '…', '—', '–', 'ƒ', '⁄', '‹', '›', '−', '‰', '„', '“', '”', '‘', '’', '‚', '™', 'ﬁ', 'ﬂ', 'Ł', 'Œ', 'Š', 'Ÿ', 'Ž', 'ı', 'ł', 'œ', 'š', 'ž', '\u{FFFD}'];
    const LOW: [char; 8] = ['˘', 'ˇ', 'ˆ', '˙', '˝', '˛', '˚', '˜'];
    match c {
        0x18..=0x1F => LOW[(c - 0x18) as usize],
        0x80..=0x9F => HIGH[(c - 0x80) as usize],
        c => c as char,
    }
}
