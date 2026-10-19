//! Office Open XML packages: the zip parts, their relationships and their
//! XML read into small element trees (names without their prefixes).

pub(crate) mod xml;
mod zip;

use std::collections::HashMap;
use std::sync::Arc;

use vello_cpu::Pixmap;

use crate::{Error, Limits};
use xml::{Ev, Reader};
pub(crate) use zip::ZipReader;

pub(crate) fn bad(msg: impl Into<String>) -> Error {
    Error::Malformed(msg.into())
}

/// Elements one part may hold, and how deep they may nest.
const MAX_ELEMENTS: usize = 4_000_000;

/// An XML element: its local name, attributes (local names) and content.
#[derive(Debug, Default, Clone)]
pub(crate) struct El {
    pub name: String,
    pub attrs: Vec<(String, String)>,
    pub kids: Vec<Node>,
}

#[derive(Debug, Clone)]
pub(crate) enum Node {
    El(El),
    Text(String),
}

impl El {
    pub fn attr(&self, name: &str) -> Option<&str> {
        self.attrs.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
    }

    /// The attribute as a number.
    pub fn num(&self, name: &str) -> Option<f32> {
        self.attr(name).and_then(|v| v.trim().parse::<f32>().ok()).filter(|v| v.is_finite())
    }

    pub fn elements(&self) -> impl Iterator<Item = &El> {
        self.kids.iter().filter_map(|k| match k {
            Node::El(e) => Some(e),
            Node::Text(_) => None,
        })
    }

    pub fn child(&self, name: &str) -> Option<&El> {
        self.elements().find(|e| e.name == name)
    }

    pub fn children<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a El> + 'a {
        self.elements().filter(move |e| e.name == name)
    }

    /// The element down a path of child names.
    pub fn path(&self, names: &[&str]) -> Option<&El> {
        let mut e = self;
        for n in names {
            e = e.child(n)?;
        }
        Some(e)
    }

    /// The `val` attribute of child `name`.
    pub fn val(&self, name: &str) -> Option<&str> {
        self.child(name).and_then(|c| c.attr("val"))
    }

    /// An on/off property (`<w:b/>`, `<w:b w:val="0"/>`): None if absent.
    pub fn flag(&self, name: &str) -> Option<bool> {
        self.child(name).map(|c| !matches!(c.attr("val"), Some("0" | "false" | "off" | "none")))
    }

    /// All the text inside, in order.
    pub fn text(&self) -> String {
        let mut out = String::new();
        self.text_into(&mut out, 0);
        out
    }

    fn text_into(&self, out: &mut String, depth: usize) {
        for k in &self.kids {
            match k {
                Node::Text(t) => out.push_str(t),
                Node::El(e) if depth < 64 => e.text_into(out, depth + 1),
                Node::El(_) => {}
            }
        }
    }

    /// The first element named `name` at any depth (breadth first,
    /// bounded).
    pub fn find(&self, name: &str) -> Option<&El> {
        let mut queue: Vec<&El> = vec![self];
        let mut i = 0;
        while i < queue.len() && i < 200_000 {
            let e = queue[i];
            if e.name == name && i > 0 {
                return Some(e);
            }
            queue.extend(e.elements());
            i += 1;
        }
        None
    }
}

/// Parses XML into its root element. Text that is only whitespace
/// between elements is left out unless `keep_space` names its parent.
pub(crate) fn parse(src: &str, limits: &Limits, keep_space: &[&str]) -> Result<El, Error> {
    let mut stack: Vec<El> = vec![El::default()];
    let mut r = Reader::new(src);
    let mut count = 0usize;
    loop {
        match r.next() {
            Ev::Start(t) | Ev::Empty(t) if stack.len() > limits.max_xml_depth => {
                // Too deep: what is inside is left out.
                let _ = t;
                return Err(Error::TooLarge);
            }
            Ev::Start(t) => {
                count += 1;
                stack.push(element(&t));
            }
            Ev::Empty(t) => {
                count += 1;
                let e = element(&t);
                if let Some(top) = stack.last_mut() {
                    top.kids.push(Node::El(e));
                }
            }
            Ev::End(_) => {
                if stack.len() > 1 {
                    let e = stack.pop().unwrap_or_default();
                    if let Some(top) = stack.last_mut() {
                        top.kids.push(Node::El(e));
                    }
                }
            }
            Ev::Text(t) => {
                if let Some(top) = stack.last_mut()
                    && (!t.trim().is_empty() || keep_space.contains(&top.name.as_str()))
                {
                    top.kids.push(Node::Text(t.into_owned()));
                }
            }
            Ev::Eof => break,
        }
        if count > MAX_ELEMENTS {
            return Err(Error::TooLarge);
        }
    }
    // Unclosed elements are closed at the end.
    while stack.len() > 1 {
        let e = stack.pop().unwrap_or_default();
        if let Some(top) = stack.last_mut() {
            top.kids.push(Node::El(e));
        }
    }
    let doc = stack.pop().unwrap_or_default();
    doc.kids.into_iter().find_map(|k| match k {
        Node::El(e) => Some(e),
        Node::Text(_) => None,
    }).ok_or_else(|| bad("no XML element"))
}

fn element(t: &xml::Tag) -> El {
    let attrs = t.attrs().take(256).map(|(k, v)| (xml::local(k).to_string(), v.into_owned())).collect();
    El { name: t.local().to_string(), attrs, kids: Vec::new() }
}

/// A relationship of a part.
#[derive(Debug, Clone)]
pub(crate) struct Rel {
    /// The part it points to (a package path), or the URL of an external
    /// target.
    pub target: String,
    pub kind: String,
    pub external: bool,
}

/// An open package.
pub(crate) struct Package {
    data: Vec<u8>,
    pub limits: Limits,
}

impl Package {
    pub fn open(data: Vec<u8>, limits: &Limits) -> Result<Package, Error> {
        ZipReader::new(&data, limits)?;
        Ok(Package { data, limits: limits.clone() })
    }

    pub fn zip(&self) -> Result<ZipReader<'_>, Error> {
        ZipReader::new(&self.data, &self.limits)
    }

    pub fn has(&self, name: &str) -> bool {
        self.zip().is_ok_and(|z| z.entry_name(name).is_some())
    }

    pub fn read(&self, name: &str) -> Result<Vec<u8>, Error> {
        self.zip()?.read(name)
    }

    /// Part `name` parsed.
    pub fn xml(&self, name: &str, keep_space: &[&str]) -> Result<El, Error> {
        let z = self.zip()?;
        let s = z.read_string(name)?;
        if s.len() as u64 > self.limits.max_part_bytes {
            return Err(Error::TooLarge);
        }
        parse(&s, &self.limits, keep_space)
    }

    /// The relationships of part `part`, by id.
    pub fn rels(&self, part: &str) -> HashMap<String, Rel> {
        let (dir, file) = part.rsplit_once('/').unwrap_or(("", part));
        let name = if dir.is_empty() { format!("_rels/{file}.rels") } else { format!("{dir}/_rels/{file}.rels") };
        let mut out = HashMap::new();
        let Ok(root) = self.xml(&name, &[]) else { return out };
        for r in root.children("Relationship") {
            let (Some(id), Some(target)) = (r.attr("Id"), r.attr("Target")) else { continue };
            let external = r.attr("TargetMode") == Some("External");
            let target = if external { target.to_string() } else { resolve(dir, target) };
            let kind = r.attr("Type").unwrap_or("").rsplit('/').next().unwrap_or("").to_string();
            out.insert(id.to_string(), Rel { target, kind, external });
        }
        out
    }

    /// The part a relationship of type `kind` (the last segment of its
    /// type URI) of `part` points to.
    pub fn rel_of(&self, part: &str, kind: &str) -> Option<String> {
        self.rels(part).into_values().find(|r| r.kind == kind && !r.external).map(|r| r.target)
    }
}

/// A relationship target relative to folder `dir`, as a package path.
pub(crate) fn resolve(dir: &str, target: &str) -> String {
    let target = target.split('#').next().unwrap_or(target);
    let mut parts: Vec<&str> = if target.starts_with('/') { Vec::new() } else { dir.split('/').filter(|s| !s.is_empty()).collect() };
    for seg in target.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            s => parts.push(s),
        }
    }
    parts.join("/")
}

/// A picture of a package decoded into pixels to draw, held to the
/// limits; None for formats not read here (EMF, WMF, TIFF...).
pub(crate) fn picture(data: &[u8], limits: &Limits) -> Option<Arc<Pixmap>> {
    let (w, h, rgba) = crate::images::decode(data, limits)?;
    crate::draw::pixmap(w, h, &rgba)
}

/// Pictures decoded, kept for the next pages that show them.
#[derive(Default)]
pub(crate) struct Pictures {
    decoded: HashMap<Arc<str>, Option<Arc<Pixmap>>>,
}

impl Pictures {
    const KEEP: usize = 32;

    /// The picture at package part `part`.
    pub fn get(&mut self, pkg: &Package, part: &str) -> Option<Arc<Pixmap>> {
        self.get_with(part, &pkg.limits, || pkg.read(part).ok())
    }

    /// The picture known as `key`, its bytes from `bytes` when it is not
    /// decoded yet.
    pub fn get_with(&mut self, key: &str, limits: &Limits, bytes: impl FnOnce() -> Option<Vec<u8>>) -> Option<Arc<Pixmap>> {
        if let Some(p) = self.decoded.get(key) {
            return p.clone();
        }
        let p = bytes().and_then(|data| picture(&data, limits));
        if self.decoded.len() >= Self::KEEP {
            self.decoded.clear();
        }
        self.decoded.insert(Arc::from(key), p.clone());
        p
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relationship_targets_resolve_against_their_folder() {
        assert_eq!(resolve("word", "media/image1.png"), "word/media/image1.png");
        assert_eq!(resolve("ppt/slides", "../media/a.png"), "ppt/media/a.png");
        assert_eq!(resolve("word", "/customXml/item.xml"), "customXml/item.xml");
    }

    #[test]
    fn deep_xml_is_refused() {
        let l = Limits { max_xml_depth: 8, ..Limits::default() };
        let deep = "<a>".repeat(20) + &"</a>".repeat(20);
        assert!(parse(&deep, &l, &[]).is_err());
        let ok = parse("<w:p a:x='1'><w:t>hi</w:t></w:p>", &l, &[]).unwrap();
        assert_eq!(ok.child("t").unwrap().text(), "hi");
        assert_eq!(ok.attr("x"), Some("1"));
    }
}
