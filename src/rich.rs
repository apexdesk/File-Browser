//! Texto con formato en la vista previa: Markdown y libros EPUB. Un documento es una lista de
//! bloques (títulos, párrafos, listas, código, tablas, imágenes) que se maqueta al ancho disponible.
use crate::markup::{self, Tok};
use crate::theme::Pal;
use crate::thumbs::Decoded;
use egui::text::{LayoutJob, TextFormat};
use egui::{pos2, vec2, Color32, FontId, Galley, Rect, Stroke};
use std::path::Path;
use std::sync::Arc;

const MAX_BLOCKS: usize = 40_000;
const MAX_CHARS: usize = 3_000_000;
/// Memoria máxima de las imágenes de un libro (RGBA).
const MAX_IMG_BYTES: usize = 160 << 20;

#[derive(Clone, Default, PartialEq)]
pub struct Span {
    pub text: String,
    pub bold: bool,
    pub italic: bool,
    pub code: bool,
    pub link: bool,
    pub strike: bool,
}

pub enum Block {
    Heading(u8, Vec<Span>),
    Para(Vec<Span>),
    Quote(Vec<Span>),
    Code(Vec<String>),
    Item { depth: u8, marker: String, spans: Vec<Span> },
    Rule,
    Table(Vec<Vec<String>>),
    Image(usize),
}

#[derive(Default)]
pub struct Rich {
    pub blocks: Vec<Block>,
    pub images: Vec<Decoded>,
    pub meta: Vec<(String, String)>,
    pub truncated: bool,
}

// ---------------------------------------------------------------------------
// Markdown
// ---------------------------------------------------------------------------

/// Texto en línea de Markdown: **negrita**, *cursiva*, `código`, ~~tachado~~, [enlaces](url) e imágenes.
pub fn inline(s: &str) -> Vec<Span> {
    inline_d(s, 0)
}

/// Líneas larguísimas o enlaces anidados sin fin: se muestran como texto plano (evita coste cuadrático y recursión sin límite).
fn inline_d(s: &str, depth: u32) -> Vec<Span> {
    let c: Vec<char> = s.chars().collect();
    if c.len() > 8000 || depth > 6 {
        return vec![Span { text: s.to_string(), ..Default::default() }];
    }
    let mut out: Vec<Span> = Vec::new();
    let (mut bold, mut ital, mut strike, link) = (false, false, false, false);
    let mut buf = String::new();
    let flush = |buf: &mut String, out: &mut Vec<Span>, b: bool, i: bool, st: bool, l: bool, code: bool| {
        if !buf.is_empty() {
            out.push(Span { text: std::mem::take(buf), bold: b, italic: i, code, link: l, strike: st });
        }
    };
    let alnum = |ch: Option<&char>| ch.map(|c| c.is_alphanumeric()).unwrap_or(false);
    let mut i = 0;
    while i < c.len() {
        let ch = c[i];
        match ch {
            '\\' if i + 1 < c.len() && c[i + 1].is_ascii_punctuation() => {
                buf.push(c[i + 1]);
                i += 2;
            }
            '`' => {
                let n = c[i..].iter().take_while(|x| **x == '`').count();
                let close = (i + n..c.len()).find(|&j| c[j] == '`' && c[j..].iter().take_while(|x| **x == '`').count() == n);
                if let Some(j) = close {
                    flush(&mut buf, &mut out, bold, ital, strike, link, false);
                    let t: String = c[i + n..j].iter().collect();
                    out.push(Span { text: t.trim().to_string(), bold, italic: ital, code: true, link, strike });
                    i = j + n;
                } else {
                    buf.extend(std::iter::repeat('`').take(n));
                    i += n;
                }
            }
            '*' | '_' => {
                let n = c[i..].iter().take_while(|x| **x == ch).count();
                // un guion bajo dentro de una palabra (snake_case) no es cursiva
                let intra = ch == '_' && alnum(i.checked_sub(1).and_then(|k| c.get(k))) && alnum(c.get(i + n));
                let ws_after = c.get(i + n).map(|x| x.is_whitespace()).unwrap_or(true);
                let ws_before = i == 0 || c[i - 1].is_whitespace();
                let can_open = !ws_after;
                let can_close = !ws_before;
                let on = if ch == '*' || !intra { (if bold || ital { can_close } else { can_open }) || can_close } else { false };
                if intra || n > 3 || !on {
                    buf.extend(std::iter::repeat(ch).take(n));
                } else {
                    flush(&mut buf, &mut out, bold, ital, strike, link, false);
                    match n {
                        1 => ital = !ital,
                        2 => bold = !bold,
                        _ => {
                            bold = !bold;
                            ital = !ital;
                        }
                    }
                }
                i += n;
            }
            '~' if c.get(i + 1) == Some(&'~') => {
                flush(&mut buf, &mut out, bold, ital, strike, link, false);
                strike = !strike;
                i += 2;
            }
            '!' | '[' => {
                let is_img = ch == '!' && c.get(i + 1) == Some(&'[');
                let open = if is_img { i + 1 } else { i };
                if ch == '!' && !is_img {
                    buf.push(ch);
                    i += 1;
                    continue;
                }
                // [texto](destino)
                let mut nest = 0;
                let mut close = None;
                for j in open..c.len() {
                    match c[j] {
                        '[' => nest += 1,
                        ']' => {
                            nest -= 1;
                            if nest == 0 {
                                close = Some(j);
                                break;
                            }
                        }
                        _ => {}
                    }
                }
                if let Some(cl) = close.filter(|cl| c.get(cl + 1) == Some(&'(')) {
                    if let Some(end) = (cl + 2..c.len()).find(|&j| c[j] == ')') {
                        flush(&mut buf, &mut out, bold, ital, strike, link, false);
                        let label: String = c[open + 1..cl].iter().collect();
                        if is_img {
                            out.push(Span { text: format!("[{}]", if label.is_empty() { "imagen" } else { &label }), italic: true, ..Default::default() });
                        } else {
                            for mut sp in inline_d(&label, depth + 1) {
                                sp.link = true;
                                out.push(sp);
                            }
                        }
                        i = end + 1;
                        continue;
                    }
                }
                buf.push(ch);
                i += 1;
            }
            '<' => {
                // <https://…> o etiquetas HTML sueltas
                if let Some(j) = (i + 1..c.len().min(i + 300)).find(|&j| c[j] == '>') {
                    let inner: String = c[i + 1..j].iter().collect();
                    if inner.starts_with("http://") || inner.starts_with("https://") || inner.starts_with("mailto:") {
                        flush(&mut buf, &mut out, bold, ital, strike, link, false);
                        out.push(Span { text: inner, link: true, ..Default::default() });
                        i = j + 1;
                        continue;
                    }
                    if inner.chars().next().map(|x| x.is_ascii_alphabetic() || x == '/').unwrap_or(false) && !inner.contains(' ') | inner.contains('=') {
                        if inner.trim_start_matches('/').starts_with("br") {
                            buf.push(' ');
                        }
                        i = j + 1;
                        continue;
                    }
                }
                buf.push('<');
                i += 1;
            }
            _ => {
                buf.push(ch);
                i += 1;
            }
        }
    }
    flush(&mut buf, &mut out, bold, ital, strike, link, false);
    out
}

fn is_rule(l: &str) -> bool {
    let t: String = l.chars().filter(|c| !c.is_whitespace()).collect();
    t.len() >= 3 && (t.chars().all(|c| c == '-') || t.chars().all(|c| c == '*') || t.chars().all(|c| c == '_'))
}

fn split_row(l: &str) -> Vec<String> {
    let t = l.trim().trim_start_matches('|').trim_end_matches('|');
    t.split('|').map(|c| c.trim().to_string()).collect()
}

fn is_sep_row(l: &str) -> bool {
    let t = l.trim();
    t.contains('-') && t.contains('|') | t.starts_with(':') && t.chars().all(|c| matches!(c, '|' | '-' | ':' | ' '))
}

/// Marcador de lista (`-`, `*`, `+`, `1.`) al principio de la línea: (sangría, marcador, resto).
fn list_item(l: &str) -> Option<(usize, String, &str)> {
    let ind = l.len() - l.trim_start().len();
    let t = &l[ind..];
    let b = t.as_bytes();
    if b.len() >= 2 && matches!(b[0], b'-' | b'*' | b'+') && b[1] == b' ' && !is_rule(t) {
        let rest = &t[2..];
        let (m, rest) = if let Some(r) = rest.strip_prefix("[ ] ") {
            ("☐".to_string(), r)
        } else if let Some(r) = rest.strip_prefix("[x] ").or_else(|| rest.strip_prefix("[X] ")) {
            ("☑".to_string(), r)
        } else {
            ("•".to_string(), rest)
        };
        return Some((ind, m, rest));
    }
    let d = t.bytes().take_while(|c| c.is_ascii_digit()).count();
    if d > 0 && d <= 9 && t.len() > d + 1 && (b[d] == b'.' || b[d] == b')') && b[d + 1] == b' ' {
        return Some((ind, format!("{}.", &t[..d]), &t[d + 2..]));
    }
    None
}

pub fn markdown(text: &str) -> Rich {
    let mut r = Rich::default();
    let lines: Vec<&str> = text.lines().collect();
    let mut i = 0;
    let mut para: Vec<String> = Vec::new();
    macro_rules! flush_para {
        () => {
            if !para.is_empty() {
                let joined = para.join(" ");
                r.blocks.push(Block::Para(inline(&joined)));
                para.clear();
            }
        };
    }
    // cabecera YAML al principio: se muestra como código
    if lines.first().map(|l| l.trim_end() == "---").unwrap_or(false) {
        if let Some(end) = (1..lines.len().min(60)).find(|&k| lines[k].trim_end() == "---" || lines[k].trim_end() == "...") {
            r.blocks.push(Block::Code(lines[1..end].iter().map(|l| l.to_string()).collect()));
            i = end + 1;
        }
    }
    while i < lines.len() {
        if r.blocks.len() >= MAX_BLOCKS {
            r.truncated = true;
            break;
        }
        let l = lines[i];
        let tr = l.trim();
        if tr.starts_with("```") || tr.starts_with("~~~") {
            flush_para!();
            let fence = &tr[..3];
            let mut code = Vec::new();
            i += 1;
            while i < lines.len() && !lines[i].trim_start().starts_with(fence) {
                code.push(lines[i].replace('\t', "    "));
                i += 1;
            }
            i += 1;
            r.blocks.push(Block::Code(code));
            continue;
        }
        if tr.is_empty() {
            flush_para!();
            i += 1;
            continue;
        }
        if let Some(h) = tr.strip_prefix('#') {
            let lvl = 1 + h.chars().take_while(|c| *c == '#').count();
            let body = h.trim_start_matches('#');
            if lvl <= 6 && (body.starts_with(' ') || body.is_empty()) {
                flush_para!();
                r.blocks.push(Block::Heading(lvl as u8, inline(body.trim().trim_end_matches('#').trim())));
                i += 1;
                continue;
            }
        }
        // título con subrayado (=== o ---)
        if !para.is_empty() && !tr.is_empty() && (tr.chars().all(|c| c == '=') || (tr.chars().all(|c| c == '-') && tr.len() >= 2)) {
            let lvl = if tr.starts_with('=') { 1 } else { 2 };
            let joined = para.join(" ");
            para.clear();
            r.blocks.push(Block::Heading(lvl, inline(&joined)));
            i += 1;
            continue;
        }
        if is_rule(tr) {
            flush_para!();
            r.blocks.push(Block::Rule);
            i += 1;
            continue;
        }
        if tr.starts_with('>') {
            flush_para!();
            let mut q = Vec::new();
            while i < lines.len() && lines[i].trim_start().starts_with('>') {
                q.push(lines[i].trim_start().trim_start_matches('>').trim().to_string());
                i += 1;
            }
            r.blocks.push(Block::Quote(inline(&q.join(" "))));
            continue;
        }
        if tr.contains('|') && i + 1 < lines.len() && is_sep_row(lines[i + 1]) {
            flush_para!();
            let mut rows = vec![split_row(tr)];
            i += 2;
            while i < lines.len() && lines[i].contains('|') && !lines[i].trim().is_empty() {
                rows.push(split_row(lines[i]));
                i += 1;
            }
            r.blocks.push(Block::Table(rows));
            continue;
        }
        if let Some((ind, marker, rest)) = list_item(l) {
            flush_para!();
            r.blocks.push(Block::Item { depth: (ind / 2).min(6) as u8, marker, spans: inline(rest) });
            i += 1;
            continue;
        }
        // sangría de código (4 espacios) sólo si no continúa un párrafo
        if para.is_empty() && (l.starts_with("    ") || l.starts_with('\t')) {
            let mut code = Vec::new();
            while i < lines.len() && (lines[i].starts_with("    ") || lines[i].starts_with('\t') || lines[i].trim().is_empty()) {
                code.push(lines[i].trim_start_matches("    ").trim_start_matches('\t').replace('\t', "    "));
                i += 1;
            }
            while code.last().map(|l| l.trim().is_empty()).unwrap_or(false) {
                code.pop();
            }
            r.blocks.push(Block::Code(code));
            continue;
        }
        para.push(tr.to_string());
        i += 1;
    }
    flush_para!();
    r
}

// ---------------------------------------------------------------------------
// HTML (capítulos de EPUB)
// ---------------------------------------------------------------------------

struct HtmlCx<'a> {
    out: &'a mut Rich,
    spans: Vec<Span>,
    bold: u32,
    ital: u32,
    code: u32,
    link: u32,
    pre: bool,
    heading: u8,
    quote: u32,
    lists: Vec<(bool, u32)>,
    item: Option<(u8, String)>,
    chars: usize,
    img_bytes: usize,
    base: String,
    read: &'a mut dyn FnMut(&str) -> Option<Vec<u8>>,
}

impl HtmlCx<'_> {
    fn push_text(&mut self, t: &str) {
        let t = if self.pre {
            t.replace('\t', "    ")
        } else {
            let mut s = String::with_capacity(t.len());
            let mut sp = self.spans.last().map(|x| x.text.ends_with(' ')).unwrap_or(true);
            for ch in t.chars() {
                if ch.is_whitespace() && ch != '\u{a0}' {
                    if !sp {
                        s.push(' ');
                    }
                    sp = true;
                } else {
                    s.push(ch);
                    sp = false;
                }
            }
            s
        };
        if t.is_empty() {
            return;
        }
        self.chars += t.len();
        let sp = Span { text: t, bold: self.bold > 0 || self.heading > 0, italic: self.ital > 0, code: self.code > 0, link: self.link > 0, strike: false };
        match self.spans.last_mut() {
            Some(l) if l.bold == sp.bold && l.italic == sp.italic && l.code == sp.code && l.link == sp.link => l.text.push_str(&sp.text),
            _ => self.spans.push(sp),
        }
    }

    fn flush(&mut self) {
        let mut spans = std::mem::take(&mut self.spans);
        if self.pre {
            // los saltos de línea del código pasan a ser líneas
            let text: String = spans.iter().map(|s| s.text.as_str()).collect();
            let lines: Vec<String> = text.trim_matches('\n').lines().map(|l| l.to_string()).collect();
            if !lines.is_empty() {
                self.out.blocks.push(Block::Code(lines));
            }
            return;
        }
        if let Some(f) = spans.first_mut() {
            f.text = f.text.trim_start().to_string();
        }
        if let Some(l) = spans.last_mut() {
            l.text = l.text.trim_end().to_string();
        }
        spans.retain(|s| !s.text.is_empty());
        if spans.is_empty() {
            return;
        }
        let b = if let Some((depth, marker)) = self.item.take() {
            Block::Item { depth, marker, spans }
        } else if self.heading > 0 {
            Block::Heading(self.heading, spans)
        } else if self.quote > 0 {
            Block::Quote(spans)
        } else {
            Block::Para(spans)
        };
        self.out.blocks.push(b);
    }

    fn image(&mut self, src: &str) {
        self.flush();
        let path = resolve(&self.base, src);
        if self.img_bytes > MAX_IMG_BYTES {
            return;
        }
        if let Some(d) = (self.read)(&path).and_then(|b| crate::comic::decode_bytes(b, 900)) {
            if d.w >= 24 && d.h >= 24 {
                self.img_bytes += d.rgba.len();
                self.out.images.push(d);
                self.out.blocks.push(Block::Image(self.out.images.len() - 1));
            }
        }
    }
}

/// Ruta de `href` relativa al archivo `base` dentro del paquete, sin `..` ni parámetros.
pub fn resolve(base: &str, href: &str) -> String {
    let href = href.split(['#', '?']).next().unwrap_or("");
    let href = href.replace("%20", " ");
    let mut parts: Vec<&str> = if href.starts_with('/') { Vec::new() } else { base.rsplit_once('/').map(|x| x.0.split('/').collect()).unwrap_or_default() };
    for p in href.split('/') {
        match p {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            p => parts.push(p),
        }
    }
    parts.join("/")
}

fn html_into(out: &mut Rich, html: &str, base: &str, read: &mut dyn FnMut(&str) -> Option<Vec<u8>>, img_bytes: &mut usize) {
    let mut cx = HtmlCx {
        out,
        spans: Vec::new(),
        bold: 0,
        ital: 0,
        code: 0,
        link: 0,
        pre: false,
        heading: 0,
        quote: 0,
        lists: Vec::new(),
        item: None,
        chars: 0,
        img_bytes: *img_bytes,
        base: base.to_string(),
        read,
    };
    let mut skip = 0u32;
    for tok in markup::tokens(html, true) {
        if cx.out.blocks.len() >= MAX_BLOCKS || cx.chars > MAX_CHARS {
            cx.out.truncated = true;
            break;
        }
        match tok {
            Tok::Text(t) => {
                if skip == 0 {
                    let t = markup::decode_entities(t);
                    cx.push_text(&t);
                }
            }
            Tok::Open { name, attrs, empty } => {
                let n = markup::local(&name);
                match n {
                    "head" | "script" | "style" | "title" if !empty => skip += 1,
                    _ if skip > 0 => {}
                    "br" => {
                        if cx.pre {
                            cx.push_text("\n");
                        } else {
                            cx.flush();
                        }
                    }
                    "hr" => {
                        cx.flush();
                        cx.out.blocks.push(Block::Rule);
                    }
                    "img" | "image" => {
                        let src = markup::attr(&attrs, "src").or_else(|| markup::attr(&attrs, "xlink:href")).or_else(|| markup::attr(&attrs, "href")).map(|s| s.to_string());
                        if let Some(s) = src {
                            cx.image(&s);
                        }
                    }
                    "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                        cx.flush();
                        cx.heading = n.as_bytes()[1] - b'0';
                    }
                    "p" | "div" | "section" | "article" | "tr" | "table" | "figure" | "figcaption" | "dd" | "dt" | "header" | "footer" => cx.flush(),
                    "blockquote" => {
                        cx.flush();
                        cx.quote += 1;
                    }
                    "pre" => {
                        cx.flush();
                        cx.pre = true;
                    }
                    "ul" | "ol" => {
                        cx.flush();
                        cx.lists.push((n == "ol", 0));
                    }
                    "li" => {
                        cx.flush();
                        let depth = cx.lists.len().saturating_sub(1).min(6) as u8;
                        let marker = match cx.lists.last_mut() {
                            Some((true, k)) => {
                                *k += 1;
                                format!("{k}.")
                            }
                            _ => "•".to_string(),
                        };
                        cx.item = Some((depth, marker));
                    }
                    "td" | "th" => {
                        if !cx.spans.is_empty() {
                            cx.push_text("  |  ");
                        }
                    }
                    "b" | "strong" => cx.bold += 1,
                    "i" | "em" | "cite" => cx.ital += 1,
                    "code" | "tt" | "kbd" | "samp" => cx.code += 1,
                    "a" => cx.link += 1,
                    _ => {}
                }
            }
            Tok::Close(name) => {
                let n = markup::local(&name);
                match n {
                    "head" | "script" | "style" | "title" => skip = skip.saturating_sub(1),
                    _ if skip > 0 => {}
                    "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                        cx.flush();
                        cx.heading = 0;
                    }
                    "p" | "div" | "section" | "article" | "tr" | "table" | "li" | "figure" | "figcaption" | "dd" | "dt" | "header" | "footer" => cx.flush(),
                    "blockquote" => {
                        cx.flush();
                        cx.quote = cx.quote.saturating_sub(1);
                    }
                    "pre" => {
                        cx.flush();
                        cx.pre = false;
                    }
                    "ul" | "ol" => {
                        cx.flush();
                        cx.lists.pop();
                    }
                    "b" | "strong" => cx.bold = cx.bold.saturating_sub(1),
                    "i" | "em" | "cite" => cx.ital = cx.ital.saturating_sub(1),
                    "code" | "tt" | "kbd" | "samp" => cx.code = cx.code.saturating_sub(1),
                    "a" => cx.link = cx.link.saturating_sub(1),
                    _ => {}
                }
            }
        }
    }
    cx.flush();
    *img_bytes = cx.img_bytes;
}

// ---------------------------------------------------------------------------
// EPUB
// ---------------------------------------------------------------------------

struct Pkg {
    ar: zip::ZipArchive<std::io::BufReader<std::fs::File>>,
}

impl Pkg {
    fn open(path: &Path) -> Option<Pkg> {
        let f = std::fs::File::open(path).ok()?;
        Some(Pkg { ar: zip::ZipArchive::new(std::io::BufReader::new(f)).ok()? })
    }

    fn read(&mut self, name: &str) -> Option<Vec<u8>> {
        use std::io::Read;
        let mut f = self.ar.by_name(name).ok()?;
        if f.size() > 64 << 20 {
            return None;
        }
        let mut v = Vec::with_capacity(f.size() as usize);
        f.read_to_end(&mut v).ok()?;
        Some(v)
    }

    fn text(&mut self, name: &str) -> Option<String> {
        self.read(name).map(|b| String::from_utf8_lossy(&b).into_owned())
    }
}

struct Opf {
    base: String,
    title: String,
    author: String,
    /// (id, ruta, tipo, propiedades)
    items: Vec<(String, String, String, String)>,
    spine: Vec<String>,
    cover_id: Option<String>,
}

fn read_opf(pkg: &mut Pkg) -> Option<Opf> {
    let container = pkg.text("META-INF/container.xml")?;
    let root = markup::tokens(&container, true).into_iter().find_map(|t| match t {
        Tok::Open { name, attrs, .. } if markup::local(&name) == "rootfile" => markup::attr(&attrs, "full-path").map(|s| s.to_string()),
        _ => None,
    })?;
    let opf = pkg.text(&root)?;
    let mut o = Opf { base: root, title: String::new(), author: String::new(), items: Vec::new(), spine: Vec::new(), cover_id: None };
    let mut field = "";
    for t in markup::tokens(&opf, true) {
        match t {
            Tok::Open { name, attrs, .. } => {
                let a = |k: &str| markup::attr(&attrs, k).unwrap_or("").to_string();
                match markup::local(&name) {
                    "item" => o.items.push((a("id"), resolve(&o.base, &a("href")), a("media-type"), a("properties"))),
                    "itemref" => o.spine.push(a("idref")),
                    "meta" if a("name") == "cover" => o.cover_id = Some(a("content")),
                    "title" => field = "title",
                    "creator" => field = "author",
                    _ => {}
                }
            }
            Tok::Text(t) => {
                let t = markup::decode_entities(t.trim());
                let empty = t.is_empty();
                match field {
                    "title" if o.title.is_empty() => o.title = t,
                    "author" if o.author.is_empty() => o.author = t,
                    _ => {}
                }
                if !empty {
                    field = "";
                }
            }
            Tok::Close(_) => field = "",
        }
    }
    Some(o)
}

fn cover_path(o: &Opf) -> Option<String> {
    let by_prop = o.items.iter().find(|i| i.3.split_whitespace().any(|p| p == "cover-image"));
    let by_id = o.cover_id.as_ref().and_then(|id| o.items.iter().find(|i| &i.0 == id));
    by_prop.or(by_id).or_else(|| o.items.iter().find(|i| i.2.starts_with("image/") && i.1.to_lowercase().contains("cover"))).map(|i| i.1.clone())
}

pub fn epub_cover(path: &Path, max: u32) -> Option<Decoded> {
    if std::fs::metadata(path).ok()?.len() > 512 << 20 {
        return None;
    }
    let mut pkg = Pkg::open(path)?;
    let o = read_opf(&mut pkg)?;
    let p = cover_path(&o)?;
    crate::comic::decode_bytes(pkg.read(&p)?, max)
}

pub fn epub(path: &Path) -> Result<Rich, String> {
    let t = crate::i18n::t;
    let fail = || t("No se pudo mostrar el documento").to_string();
    if std::fs::metadata(path).map(|m| m.len() > 512 << 20).unwrap_or(true) {
        return Err(t("El documento es demasiado grande para mostrarlo").into());
    }
    let mut pkg = Pkg::open(path).ok_or_else(fail)?;
    let o = read_opf(&mut pkg).ok_or_else(fail)?;
    let mut r = Rich::default();
    if !o.title.is_empty() {
        r.meta.push((t("Título").into(), o.title.clone()));
    }
    if !o.author.is_empty() {
        r.meta.push((t("Autor").into(), o.author.clone()));
    }
    let mut img_bytes = 0usize;
    for id in &o.spine {
        let Some(item) = o.items.iter().find(|i| &i.0 == id) else { continue };
        if !(item.2.contains("html") || item.2.contains("xml")) {
            continue;
        }
        let Some(html) = pkg.text(&item.1) else { continue };
        let before = r.blocks.len();
        let pkg_ref = &mut pkg;
        html_into(&mut r, &html, &item.1, &mut |p| pkg_ref.read(p), &mut img_bytes);
        if r.blocks.len() > before {
            r.blocks.push(Block::Rule);
        }
        if r.truncated {
            break;
        }
    }
    if r.blocks.is_empty() {
        return Err(fail());
    }
    Ok(r)
}

// ---------------------------------------------------------------------------
// Maquetación y dibujo
// ---------------------------------------------------------------------------

enum Item {
    Text { galley: Arc<Galley>, x: f32, marker: Option<Arc<Galley>>, deco: Deco },
    Rule,
    Image { idx: usize, size: egui::Vec2 },
    Table { cells: Vec<Vec<Arc<Galley>>>, cols: Vec<f32>, row_h: f32 },
}

#[derive(Clone, Copy, PartialEq)]
enum Deco {
    None,
    Quote,
    Code,
}

struct Layout {
    width: f32,
    dark: bool,
    items: Vec<(f32, f32, Item)>,
    total: f32,
}

pub struct RichView {
    pub rich: Rich,
    tex: Vec<Option<egui::TextureHandle>>,
    layout: Option<Layout>,
    pub scroll: f32,
}

impl RichView {
    pub fn new(rich: Rich) -> RichView {
        let n = rich.images.len();
        RichView { rich, tex: (0..n).map(|_| None).collect(), layout: None, scroll: 0.0 }
    }

    fn build(&self, p: &egui::Painter, width: f32, pal: &Pal) -> Layout {
        let dark = pal.bg.r() < 128;
        let body = pal.text.gamma_multiply(0.9);
        let mut items: Vec<(f32, f32, Item)> = Vec::new();
        let mut y = 12.0f32;
        let fmt = |size: f32, sp: &Span, base: Color32| -> TextFormat {
            let mut f = TextFormat::simple(if sp.code { FontId::monospace(size * 0.92) } else { FontId::proportional(size) }, if sp.bold { pal.text } else { base });
            f.italics = sp.italic;
            if sp.link {
                f.color = pal.accent;
                f.underline = Stroke::new(1.0, pal.accent);
            }
            if sp.strike {
                f.strikethrough = Stroke::new(1.0, base);
            }
            if sp.code {
                f.background = pal.surface;
            }
            f
        };
        let job = |spans: &[Span], size: f32, color: Color32, wrap: f32| -> LayoutJob {
            let mut j = LayoutJob::default();
            j.wrap.max_width = wrap.max(40.0);
            for sp in spans {
                j.append(&sp.text, 0.0, fmt(size, sp, color));
            }
            j
        };
        for b in &self.rich.blocks {
            match b {
                Block::Heading(l, spans) => {
                    let size = [28.0, 24.0, 20.0, 17.5, 16.0, 15.0][(*l as usize).clamp(1, 6) - 1];
                    y += if *l <= 2 { 14.0 } else { 8.0 };
                    let g = p.layout_job(job(spans, size, pal.text, width));
                    let h = g.size().y;
                    items.push((y, h, Item::Text { galley: g, x: 0.0, marker: None, deco: Deco::None }));
                    y += h + 8.0;
                    if *l <= 2 {
                        items.push((y - 4.0, 1.0, Item::Rule));
                        y += 6.0;
                    }
                }
                Block::Para(spans) => {
                    let g = p.layout_job(job(spans, 15.0, body, width));
                    let h = g.size().y;
                    items.push((y, h, Item::Text { galley: g, x: 0.0, marker: None, deco: Deco::None }));
                    y += h + 10.0;
                }
                Block::Quote(spans) => {
                    let g = p.layout_job(job(spans, 15.0, pal.subtle, width - 22.0));
                    let h = g.size().y;
                    items.push((y, h, Item::Text { galley: g, x: 22.0, marker: None, deco: Deco::Quote }));
                    y += h + 10.0;
                }
                Block::Item { depth, marker, spans } => {
                    let ind = 8.0 + *depth as f32 * 22.0;
                    let g = p.layout_job(job(spans, 15.0, body, width - ind - 22.0));
                    let m = p.layout_no_wrap(marker.clone(), FontId::proportional(15.0), pal.subtle);
                    let h = g.size().y;
                    items.push((y, h, Item::Text { galley: g, x: ind + 22.0, marker: Some(m), deco: Deco::None }));
                    y += h + 4.0;
                }
                Block::Code(lines) => {
                    let mut j = LayoutJob::default();
                    j.wrap.max_width = f32::INFINITY;
                    j.append(&lines.join("\n"), 0.0, TextFormat::simple(FontId::monospace(13.0), pal.text));
                    let g = p.layout_job(j);
                    let h = g.size().y + 16.0;
                    items.push((y, h, Item::Text { galley: g, x: 10.0, marker: None, deco: Deco::Code }));
                    y += h + 10.0;
                }
                Block::Rule => {
                    items.push((y + 6.0, 1.0, Item::Rule));
                    y += 18.0;
                }
                Block::Table(rows) => {
                    let ncol = rows.iter().map(|r| r.len()).max().unwrap_or(0);
                    let cells: Vec<Vec<Arc<Galley>>> = rows
                        .iter()
                        .enumerate()
                        .map(|(ri, r)| {
                            (0..ncol)
                                .map(|c| {
                                    let sp = inline(r.get(c).map(|s| s.as_str()).unwrap_or(""));
                                    let sp: Vec<Span> = sp.into_iter().map(|mut s| { s.bold |= ri == 0; s }).collect();
                                    p.layout_job(job(&sp, 14.0, body, 360.0))
                                })
                                .collect()
                        })
                        .collect();
                    let mut cols = vec![60.0f32; ncol];
                    for r in &cells {
                        for (c, g) in r.iter().enumerate() {
                            cols[c] = cols[c].max(g.size().x + 16.0);
                        }
                    }
                    let total: f32 = cols.iter().sum();
                    if total > width {
                        let k = width / total;
                        for c in &mut cols {
                            *c = (*c * k).max(40.0);
                        }
                    }
                    let row_h = cells.iter().flat_map(|r| r.iter()).map(|g| g.size().y).fold(0.0, f32::max) + 8.0;
                    let h = row_h * cells.len() as f32;
                    items.push((y, h, Item::Table { cells, cols, row_h }));
                    y += h + 12.0;
                }
                Block::Image(idx) => {
                    let d = &self.rich.images[*idx];
                    let s = (width / d.w as f32).min(1.0).min(520.0 / d.h as f32).max(0.05);
                    let size = vec2(d.w as f32 * s, d.h as f32 * s);
                    items.push((y, size.y, Item::Image { idx: *idx, size }));
                    y += size.y + 12.0;
                }
            }
        }
        Layout { width, dark, items, total: y + 20.0 }
    }

    /// Dibuja el documento en `area` con el desplazamiento propio; entrada de rueda y teclas incluida.
    pub fn draw(&mut self, ctx: &egui::Context, p: &egui::Painter, area: Rect, pal: &Pal, hot: bool) {
        p.rect_filled(area, 10.0, pal.bg);
        let col_w = (area.width() - 48.0).clamp(200.0, 820.0);
        let dark = pal.bg.r() < 128;
        if self.layout.as_ref().map(|l| (l.width - col_w).abs() > 1.0 || l.dark != dark).unwrap_or(true) {
            self.layout = Some(self.build(p, col_w, pal));
        }
        let total = self.layout.as_ref().map(|l| l.total).unwrap_or(0.0);
        scroll_input(ctx, area, total, &mut self.scroll, hot);
        let lay = self.layout.as_ref().unwrap();
        let x0 = area.center().x - col_w / 2.0;
        let (top, bottom) = (self.scroll - 40.0, self.scroll + area.height() + 40.0);
        let cp = p.with_clip_rect(area);
        for (y, h, it) in &lay.items {
            if *y + *h < top {
                continue;
            }
            if *y > bottom {
                break;
            }
            let sy = area.top() + *y - self.scroll;
            match it {
                Item::Text { galley, x, marker, deco } => {
                    match deco {
                        Deco::Quote => {
                            cp.rect_filled(Rect::from_min_size(pos2(x0 + 6.0, sy), vec2(3.0, *h)), 1.5, pal.accent.gamma_multiply(0.7));
                        }
                        Deco::Code => {
                            cp.rect_filled(Rect::from_min_size(pos2(x0, sy), vec2(col_w, *h)), 8.0, pal.surface);
                        }
                        Deco::None => {}
                    }
                    if let Some(m) = marker {
                        cp.galley(pos2(x0 + *x - 20.0, sy), m.clone(), pal.subtle);
                    }
                    let ty = if *deco == Deco::Code { sy + 8.0 } else { sy };
                    cp.galley(pos2(x0 + *x, ty), galley.clone(), pal.text);
                }
                Item::Rule => {
                    cp.line_segment([pos2(x0, sy), pos2(x0 + col_w, sy)], Stroke::new(1.0, pal.border));
                }
                Item::Table { cells, cols, row_h } => {
                    for (ri, row) in cells.iter().enumerate() {
                        let ry = sy + ri as f32 * row_h;
                        if ri == 0 {
                            cp.rect_filled(Rect::from_min_size(pos2(x0, ry), vec2(cols.iter().sum(), *row_h)), 0.0, pal.surface);
                        }
                        let mut cx = x0;
                        for (c, g) in row.iter().enumerate() {
                            cp.galley(pos2(cx + 8.0, ry + 4.0), g.clone(), pal.text);
                            cx += cols[c];
                        }
                        cp.line_segment([pos2(x0, ry + row_h), pos2(x0 + cols.iter().sum::<f32>(), ry + row_h)], Stroke::new(1.0, pal.border));
                    }
                }
                Item::Image { idx, size } => {
                    if self.tex[*idx].is_none() {
                        let d = &self.rich.images[*idx];
                        let img = crate::thumbs::color_image(ctx, d.w, d.h, &d.rgba);
                        self.tex[*idx] = Some(ctx.load_texture("richimg", img, egui::TextureOptions::LINEAR));
                    }
                    if let Some(t) = &self.tex[*idx] {
                        let r = Rect::from_min_size(pos2(area.center().x - size.x / 2.0, sy), *size);
                        cp.add(egui::epaint::RectShape::filled(r, 4.0, Color32::WHITE).with_texture(t.id(), Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0))));
                    }
                }
            }
        }
        scrollbar(p, area, total, self.scroll, pal);
        if self.rich.truncated {
            p.text(pos2(area.right() - 12.0, area.bottom() - 8.0), egui::Align2::RIGHT_BOTTOM, crate::i18n::t("(vista parcial)"), FontId::proportional(11.5), pal.subtle);
        }
    }
}

/// Rueda, Re Pág/Av Pág, Inicio y Fin sobre un contenido que mide `total` de alto.
pub fn scroll_input(ctx: &egui::Context, area: Rect, total: f32, scroll: &mut f32, hot: bool) {
    let max_scroll = (total - area.height()).max(0.0);
    if hot {
        let dy = ctx.input(|i| i.smooth_scroll_delta.y);
        *scroll = (*scroll - dy).clamp(0.0, max_scroll);
    }
    let (pg, home, end) = ctx.input(|i| {
        (
            i.key_pressed(egui::Key::PageDown) as i32 - i.key_pressed(egui::Key::PageUp) as i32,
            i.key_pressed(egui::Key::Home),
            i.key_pressed(egui::Key::End),
        )
    });
    *scroll = (*scroll + pg as f32 * area.height() * 0.9).clamp(0.0, max_scroll);
    if home {
        *scroll = 0.0;
    }
    if end {
        *scroll = max_scroll;
    }
}

pub fn scrollbar(p: &egui::Painter, area: Rect, total: f32, scroll: f32, pal: &Pal) {
    if total <= area.height() {
        return;
    }
    let track = Rect::from_min_max(pos2(area.right() - 7.0, area.top() + 4.0), pos2(area.right() - 3.0, area.bottom() - 4.0));
    let h = (track.height() * area.height() / total).max(24.0);
    let f = scroll / (total - area.height()).max(1.0);
    let y = track.top() + (track.height() - h) * f.clamp(0.0, 1.0);
    p.rect_filled(Rect::from_min_size(pos2(track.left(), y), vec2(track.width(), h)), 2.0, pal.subtle.gamma_multiply(0.5));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markdown_blocks() {
        let r = markdown("# Título\n\nHola **mundo** y `x`.\n\n- uno\n- dos\n\n```\ncódigo\n```\n\n| a | b |\n|---|---|\n| 1 | 2 |\n");
        assert!(matches!(r.blocks[0], Block::Heading(1, _)));
        assert!(matches!(&r.blocks[1], Block::Para(s) if s.iter().any(|x| x.bold && x.text == "mundo") && s.iter().any(|x| x.code)));
        assert!(matches!(r.blocks[2], Block::Item { .. }));
        assert!(matches!(&r.blocks[4], Block::Code(l) if l == &["código"]));
        assert!(matches!(&r.blocks[5], Block::Table(t) if t.len() == 2 && t[1] == ["1", "2"]));
    }

    #[test]
    fn snake_case_is_not_italic() {
        let s = inline("usa nombre_de_variable y *énfasis*");
        assert!(s.iter().all(|x| !x.italic || x.text == "énfasis"));
        assert_eq!(s.iter().map(|x| x.text.as_str()).collect::<String>(), "usa nombre_de_variable y énfasis");
    }

    #[test]
    fn resolve_paths() {
        assert_eq!(resolve("OEBPS/text/ch1.xhtml", "../img/a%20b.png#x"), "OEBPS/img/a b.png");
        assert_eq!(resolve("content.opf", "ch.xhtml"), "ch.xhtml");
    }
}
