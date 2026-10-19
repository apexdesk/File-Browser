//! Excel workbooks (.xlsx): one page per visible worksheet, its top-left
//! area drawn as a grid. Opening reads only the list of sheets; shared
//! strings and formats are read with the first page, each sheet when it
//! is asked for.

#![cfg_attr(not(feature = "xlsx"), allow(dead_code))]

pub(crate) mod numfmt;
pub(crate) mod sheet;
pub(crate) mod styles;

use std::collections::VecDeque;

use crate::draw::{self, Page};
use crate::ooxml::{Package, Pictures};
use crate::typeset::FontCache;
use crate::{Error, Fonts, Image, Limits, TextRun};

/// Sheets kept laid out.
const KEEP: usize = 4;

struct Shared {
    strings: Vec<String>,
    styles: styles::Styles,
}

pub(crate) struct Doc {
    pkg: Package,
    /// Worksheet parts, in tab order.
    sheets: Vec<String>,
    workbook: String,
    date1904: bool,
    shared: Option<Shared>,
    pages: VecDeque<(usize, Page)>,
    fonts: FontCache,
    pictures: Pictures,
    limits: Limits,
}

impl Doc {
    pub fn open(data: Vec<u8>, limits: &Limits, fonts: &Fonts) -> Result<Doc, Error> {
        let pkg = Package::open(data, limits)?;
        let workbook = pkg.rel_of("", "officeDocument").filter(|m| pkg.has(m)).unwrap_or_else(|| "xl/workbook.xml".to_string());
        let root = pkg.xml(&workbook, &[])?;
        let rels = pkg.rels(&workbook);
        let date1904 = root.child("workbookPr").and_then(|p| p.attr("date1904")).is_some_and(|v| v == "1" || v == "true");
        let mut sheets = Vec::new();
        if let Some(list) = root.child("sheets") {
            for s in list.children("sheet").take(limits.max_pages.max(1)) {
                if matches!(s.attr("state"), Some("hidden" | "veryHidden")) {
                    continue;
                }
                let Some(rel) = s.attr("id").and_then(|id| rels.get(id)) else { continue };
                if rel.kind == "worksheet" && !rel.external && pkg.has(&rel.target) {
                    sheets.push(rel.target.clone());
                }
            }
        }
        if sheets.is_empty() {
            return Err(crate::ooxml::bad("the workbook has no worksheet to show"));
        }
        Ok(Doc { pkg, sheets, workbook, date1904, shared: None, pages: VecDeque::new(), fonts: FontCache::new(fonts.clone()), pictures: Pictures::default(), limits: limits.clone() })
    }

    pub fn page_count(&self) -> usize {
        self.sheets.len()
    }

    fn page(&mut self, n: usize) -> Result<&Page, Error> {
        let part = self.sheets.get(n).ok_or(Error::NoSuchPage)?.clone();
        if !self.pages.iter().any(|(i, _)| *i == n) {
            if self.shared.is_none() {
                let rels = self.pkg.rels(&self.workbook);
                let part_of = |kind: &str| rels.values().find(|r| r.kind == kind && !r.external).map(|r| r.target.clone());
                let theme = styles::theme_colors(&self.pkg, &self.workbook);
                let strings = sheet::shared_strings(&self.pkg, part_of("sharedStrings").as_deref(), &self.limits)?;
                let styles = styles::read(&self.pkg, part_of("styles").as_deref(), &theme);
                self.shared = Some(Shared { strings, styles });
            }
            let Some(shared) = &self.shared else { return Err(Error::NoSuchPage) };
            let book = sheet::Book { pkg: &self.pkg, shared: &shared.strings, styles: &shared.styles, date1904: self.date1904, locale: locale() };
            let page = sheet::page(&book, &part, &mut self.fonts, &self.limits)?;
            if self.pages.len() >= KEEP {
                self.pages.pop_front();
            }
            self.pages.push_back((n, page));
        }
        self.pages.iter().find(|(i, _)| *i == n).map(|(_, p)| p).ok_or(Error::NoSuchPage)
    }

    pub fn page_size(&mut self, n: usize) -> Result<(f32, f32), Error> {
        self.page(n).map(|p| (p.width, p.height))
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

/// How numbers are written where the program runs (the decimal comma of
/// most of Europe), from the environment.
pub(crate) fn locale() -> numfmt::Locale {
    let tag = ["LC_ALL", "LC_NUMERIC", "LANG"].iter().filter_map(|k| std::env::var(k).ok()).find(|v| !v.is_empty()).unwrap_or_default();
    numfmt::Locale::from_tag(&tag)
}
