//! Documents that flow into pages (.docx here; .odt, .rtf and .doc read
//! into the same model): laid out when opened, each page drawn when asked
//! for, its pictures decoded then.

pub(crate) mod layout;
pub(crate) mod read;

use std::collections::HashMap;
use std::sync::Arc;

use crate::draw::{self, Page};
use crate::ooxml::{Package, Pictures};
use crate::typeset::FontCache;
use crate::{Error, Fonts, Image, Limits, TextRun};

pub(crate) use read::DocModel;

/// Where a document's pictures are: parts of its package, or bytes kept
/// by key for formats holding them in their stream.
pub(crate) enum Source {
    Package(Package),
    Blobs(HashMap<Arc<str>, Vec<u8>>),
}

pub(crate) struct Doc {
    source: Source,
    pages: Vec<Page>,
    fonts: FontCache,
    pictures: Pictures,
    limits: Limits,
}

impl Doc {
    #[cfg(feature = "docx")]
    pub fn open(data: Vec<u8>, limits: &Limits, fonts: &Fonts) -> Result<Doc, Error> {
        let pkg = Package::open(data, limits)?;
        let model = read::read(&pkg)?;
        Ok(Doc::laid_out(Source::Package(pkg), &model, limits, fonts))
    }

    /// A document read into `model`, laid out.
    pub fn laid_out(source: Source, model: &DocModel, limits: &Limits, fonts: &Fonts) -> Doc {
        let mut fonts = FontCache::new(fonts.clone());
        let pages = layout::pages(model, &mut fonts, limits.max_pages.max(1));
        Doc { source, pages, fonts, pictures: Pictures::default(), limits: limits.clone() }
    }

    pub fn page_count(&self) -> usize {
        self.pages.len()
    }

    pub fn page_size(&self, n: usize) -> Result<(f32, f32), Error> {
        self.pages.get(n).map(|p| (p.width, p.height)).ok_or(Error::NoSuchPage)
    }

    pub fn render(&mut self, n: usize, width: u32, cancel: &dyn Fn() -> bool) -> Result<Image, Error> {
        let page = self.pages.get(n).ok_or(Error::NoSuchPage)?;
        let (source, pictures, limits) = (&self.source, &mut self.pictures, &self.limits);
        let mut get = |key: &str| match source {
            Source::Package(pkg) => pictures.get(pkg, key),
            Source::Blobs(blobs) => pictures.get_with(key, limits, || blobs.get(key).cloned()),
        };
        draw::render(page, width, &mut self.fonts, &mut get, limits, cancel)
    }

    pub fn runs(&self, n: usize) -> Result<Vec<TextRun>, Error> {
        self.pages.get(n).map(Page::runs).ok_or(Error::NoSuchPage)
    }
}
