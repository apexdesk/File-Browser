//! PowerPoint presentations (.pptx): one page per slide, notes left out.
//! Opening reads only the list of slides; each is drawn when asked for.

mod slide;

use std::collections::VecDeque;

use crate::draw::{self, Page};
use crate::ooxml::{Package, Pictures};
use crate::typeset::FontCache;
use crate::{Error, Fonts, Image, Limits, TextRun};

/// Slides kept laid out.
const KEEP: usize = 4;

pub(crate) struct Doc {
    pkg: Package,
    slides: Vec<String>,
    /// The slide size in points.
    size: (f32, f32),
    pages: VecDeque<(usize, Page)>,
    fonts: FontCache,
    pictures: Pictures,
    limits: Limits,
}

impl Doc {
    pub fn open(data: Vec<u8>, limits: &Limits, fonts: &Fonts) -> Result<Doc, Error> {
        let pkg = Package::open(data, limits)?;
        let main = pkg.rel_of("", "officeDocument").filter(|m| pkg.has(m)).unwrap_or_else(|| "ppt/presentation.xml".to_string());
        let root = pkg.xml(&main, &[])?;
        let rels = pkg.rels(&main);
        let mut slides = Vec::new();
        if let Some(list) = root.child("sldIdLst") {
            for s in list.children("sldId").take(limits.max_pages.max(1)) {
                // `id` and `r:id` both read as `id`: the one naming a relationship.
                if let Some(r) = s.attrs.iter().filter(|(k, _)| k == "id").find_map(|(_, id)| rels.get(id))
                    && r.kind == "slide"
                    && !r.external
                    && pkg.has(&r.target)
                {
                    slides.push(r.target.clone());
                }
            }
        }
        if slides.is_empty() {
            return Err(crate::ooxml::bad("the presentation has no slides"));
        }
        let sz = root.child("sldSz");
        let dim = |k: &str, d: f32| sz.and_then(|s| s.num(k)).map_or(d, |v| (v / 12_700.0).clamp(36.0, 4000.0));
        let size = (dim("cx", 720.0), dim("cy", 540.0));
        Ok(Doc { pkg, slides, size, pages: VecDeque::new(), fonts: FontCache::new(fonts.clone()), pictures: Pictures::default(), limits: limits.clone() })
    }

    pub fn page_count(&self) -> usize {
        self.slides.len()
    }

    pub fn page_size(&self, n: usize) -> Result<(f32, f32), Error> {
        if n < self.slides.len() { Ok(self.size) } else { Err(Error::NoSuchPage) }
    }

    fn page(&mut self, n: usize) -> Result<&Page, Error> {
        let part = self.slides.get(n).ok_or(Error::NoSuchPage)?.clone();
        if !self.pages.iter().any(|(i, _)| *i == n) {
            let page = slide::page(&self.pkg, &part, self.size, &mut self.fonts)?;
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
        let (pkg, pictures) = (&self.pkg, &mut self.pictures);
        draw::render(page, width, &mut self.fonts, &mut |key| pictures.get(pkg, key), &self.limits, cancel)
    }

    pub fn runs(&mut self, n: usize) -> Result<Vec<TextRun>, Error> {
        self.page(n).map(Page::runs)
    }
}
