//! Lector mínimo de etiquetas XML/HTML y de JSON, para libros EPUB, árboles de datos y glTF.
//! No valida: tolera marcado roto y recorta lo que no entiende.

pub enum Tok<'a> {
    Open { name: String, attrs: Vec<(String, String)>, empty: bool },
    Close(String),
    Text(&'a str),
}

/// Separa el texto en etiquetas y texto. Comentarios, instrucciones y DOCTYPE se descartan;
/// CDATA se devuelve como texto sin interpretar.
pub fn tokens(s: &str, lower: bool) -> Vec<Tok<'_>> {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] != b'<' {
            let j = s[i..].find('<').map(|k| i + k).unwrap_or(b.len());
            out.push(Tok::Text(&s[i..j]));
            i = j;
            continue;
        }
        let rest = &s[i..];
        if rest.starts_with("<!--") {
            i += rest.find("-->").map(|k| k + 3).unwrap_or(rest.len());
        } else if rest.starts_with("<![CDATA[") {
            let end = rest.find("]]>").unwrap_or(rest.len());
            out.push(Tok::Text(&rest[9..end.max(9)]));
            i += (end + 3).min(rest.len());
        } else if rest.starts_with("<?") || rest.starts_with("<!") {
            i += rest.find('>').map(|k| k + 1).unwrap_or(rest.len());
        } else {
            // etiqueta: hasta el '>' que no esté entre comillas
            let mut j = i + 1;
            let mut q = 0u8;
            while j < b.len() {
                match (q, b[j]) {
                    (0, b'"' | b'\'') => q = b[j],
                    (x, c) if x != 0 && c == x => q = 0,
                    (0, b'>') => break,
                    _ => {}
                }
                j += 1;
            }
            let inner = &s[i + 1..j.min(b.len())];
            i = (j + 1).min(b.len());
            if let Some(name) = inner.strip_prefix('/') {
                let n = name.trim();
                out.push(Tok::Close(if lower { n.to_ascii_lowercase() } else { n.to_string() }));
            } else if !inner.is_empty() {
                out.push(parse_open(inner, lower));
            }
        }
    }
    out
}

fn parse_open(inner: &str, lower: bool) -> Tok<'static> {
    let empty = inner.ends_with('/');
    let inner = inner.trim_end_matches('/');
    let name_end = inner.find(|c: char| c.is_whitespace()).unwrap_or(inner.len());
    let name = &inner[..name_end];
    let mut attrs = Vec::new();
    let mut r = inner[name_end..].trim_start();
    while !r.is_empty() {
        let ke = r.find(|c: char| c == '=' || c.is_whitespace()).unwrap_or(r.len());
        let key = &r[..ke];
        r = r[ke..].trim_start();
        let mut val = String::new();
        if let Some(v) = r.strip_prefix('=') {
            let v = v.trim_start();
            if let Some(q) = v.chars().next().filter(|c| *c == '"' || *c == '\'') {
                let end = v[1..].find(q).map(|k| k + 1).unwrap_or(v.len());
                val = decode_entities(&v[1..end]);
                r = v.get(end + 1..).unwrap_or("").trim_start();
            } else {
                let end = v.find(|c: char| c.is_whitespace()).unwrap_or(v.len());
                val = decode_entities(&v[..end]);
                r = v[end..].trim_start();
            }
        }
        if !key.is_empty() {
            attrs.push((if lower { key.to_ascii_lowercase() } else { key.to_string() }, val));
        }
    }
    Tok::Open { name: if lower { name.to_ascii_lowercase() } else { name.to_string() }, attrs, empty }
}

pub fn attr<'a>(attrs: &'a [(String, String)], name: &str) -> Option<&'a str> {
    attrs.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
}

/// Nombre sin prefijo de espacio de nombres: `dc:title` → `title`.
pub fn local(name: &str) -> &str {
    name.rsplit(':').next().unwrap_or(name)
}

pub fn decode_entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut r = s;
    while let Some(i) = r.find('&') {
        out.push_str(&r[..i]);
        r = &r[i..];
        let Some(e) = r.find(';').filter(|e| *e <= 10) else {
            out.push('&');
            r = &r[1..];
            continue;
        };
        let name = &r[1..e];
        let ch = match name {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            "nbsp" => Some('\u{a0}'),
            "hellip" => Some('…'),
            "mdash" => Some('—'),
            "ndash" => Some('–'),
            "lsquo" => Some('‘'),
            "rsquo" => Some('’'),
            "ldquo" => Some('“'),
            "rdquo" => Some('”'),
            "copy" => Some('©'),
            "reg" => Some('®'),
            "laquo" => Some('«'),
            "raquo" => Some('»'),
            "bull" => Some('•'),
            "middot" => Some('·'),
            n if n.starts_with("#x") || n.starts_with("#X") => u32::from_str_radix(&n[2..], 16).ok().and_then(char::from_u32),
            n if n.starts_with('#') => n[1..].parse::<u32>().ok().and_then(char::from_u32),
            _ => None,
        };
        match ch {
            Some(c) => {
                out.push(c);
                r = &r[e + 1..];
            }
            None => {
                out.push('&');
                r = &r[1..];
            }
        }
    }
    out.push_str(r);
    out
}

// ---------------------------------------------------------------------------
// JSON
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    Num(f64, String),
    Str(String),
    Arr(Vec<Json>),
    Obj(Vec<(String, Json)>),
}

impl Json {
    pub fn get(&self, k: &str) -> Option<&Json> {
        match self {
            Json::Obj(v) => v.iter().find(|(n, _)| n == k).map(|(_, x)| x),
            _ => None,
        }
    }

    pub fn at(&self, i: usize) -> Option<&Json> {
        match self {
            Json::Arr(v) => v.get(i),
            _ => None,
        }
    }

    pub fn num(&self) -> Option<f64> {
        match self {
            Json::Num(n, _) => Some(*n),
            _ => None,
        }
    }

    pub fn str(&self) -> Option<&str> {
        match self {
            Json::Str(s) => Some(s),
            _ => None,
        }
    }

    pub fn arr(&self) -> &[Json] {
        match self {
            Json::Arr(v) => v,
            _ => &[],
        }
    }
}

pub fn parse_json(s: &str) -> Option<Json> {
    let mut p = JP { b: s.as_bytes(), i: 0, depth: 0 };
    p.ws();
    let v = p.value()?;
    p.ws();
    (p.i == p.b.len()).then_some(v)
}

struct JP<'a> {
    b: &'a [u8],
    i: usize,
    depth: usize,
}

impl JP<'_> {
    fn ws(&mut self) {
        while self.i < self.b.len() && matches!(self.b[self.i], b' ' | b'\t' | b'\n' | b'\r') {
            self.i += 1;
        }
        // BOM
        if self.b[self.i..].starts_with(&[0xEF, 0xBB, 0xBF]) {
            self.i += 3;
        }
    }

    fn eat(&mut self, lit: &str) -> bool {
        if self.b[self.i..].starts_with(lit.as_bytes()) {
            self.i += lit.len();
            true
        } else {
            false
        }
    }

    fn value(&mut self) -> Option<Json> {
        self.depth += 1;
        if self.depth > 200 {
            return None;
        }
        let v = match *self.b.get(self.i)? {
            b'{' => {
                self.i += 1;
                let mut v = Vec::new();
                self.ws();
                if self.eat("}") {
                    self.depth -= 1;
                    return Some(Json::Obj(v));
                }
                loop {
                    self.ws();
                    let k = self.string()?;
                    self.ws();
                    if !self.eat(":") {
                        return None;
                    }
                    self.ws();
                    v.push((k, self.value()?));
                    self.ws();
                    if self.eat(",") {
                        continue;
                    }
                    if self.eat("}") {
                        break;
                    }
                    return None;
                }
                Json::Obj(v)
            }
            b'[' => {
                self.i += 1;
                let mut v = Vec::new();
                self.ws();
                if self.eat("]") {
                    self.depth -= 1;
                    return Some(Json::Arr(v));
                }
                loop {
                    self.ws();
                    v.push(self.value()?);
                    self.ws();
                    if self.eat(",") {
                        continue;
                    }
                    if self.eat("]") {
                        break;
                    }
                    return None;
                }
                Json::Arr(v)
            }
            b'"' => Json::Str(self.string()?),
            b't' if self.eat("true") => Json::Bool(true),
            b'f' if self.eat("false") => Json::Bool(false),
            b'n' if self.eat("null") => Json::Null,
            _ => {
                let st = self.i;
                while self.i < self.b.len() && matches!(self.b[self.i], b'0'..=b'9' | b'-' | b'+' | b'.' | b'e' | b'E') {
                    self.i += 1;
                }
                let t = std::str::from_utf8(&self.b[st..self.i]).ok()?;
                Json::Num(t.parse().ok()?, t.to_string())
            }
        };
        self.depth -= 1;
        Some(v)
    }

    fn string(&mut self) -> Option<String> {
        if self.b.get(self.i) != Some(&b'"') {
            return None;
        }
        self.i += 1;
        let mut out: Vec<u8> = Vec::new();
        loop {
            let c = *self.b.get(self.i)?;
            self.i += 1;
            match c {
                b'"' => break,
                b'\\' => {
                    let e = *self.b.get(self.i)?;
                    self.i += 1;
                    match e {
                        b'n' => out.push(b'\n'),
                        b't' => out.push(b'\t'),
                        b'r' => out.push(b'\r'),
                        b'b' => out.push(8),
                        b'f' => out.push(12),
                        b'u' => {
                            let hex = |s: &[u8]| u32::from_str_radix(std::str::from_utf8(s).ok()?, 16).ok();
                            let mut cp = hex(self.b.get(self.i..self.i + 4)?)?;
                            self.i += 4;
                            if (0xD800..0xDC00).contains(&cp) && self.b.get(self.i..self.i + 2) == Some(b"\\u") {
                                if let Some(lo) = self.b.get(self.i + 2..self.i + 6).and_then(hex) {
                                    cp = 0x10000 + ((cp - 0xD800) << 10) + (lo.wrapping_sub(0xDC00) & 0x3FF);
                                    self.i += 6;
                                }
                            }
                            let ch = char::from_u32(cp).unwrap_or('\u{FFFD}');
                            let mut buf = [0u8; 4];
                            out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
                        }
                        other => out.push(other),
                    }
                }
                c => out.push(c),
            }
        }
        Some(String::from_utf8_lossy(&out).into_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_roundtrip() {
        let j = parse_json(r#" {"a":[1,2.5,{"b":"xé\n"}],"c":null,"d":true} "#).unwrap();
        assert_eq!(j.get("a").unwrap().at(1).unwrap().num(), Some(2.5));
        assert_eq!(j.get("a").unwrap().at(2).unwrap().get("b").unwrap().str(), Some("xé\n"));
        assert!(parse_json("{").is_none());
    }

    #[test]
    fn tags_and_entities() {
        let t = tokens("<p class=\"a b\">Hola &amp; <b>adiós</b><br/></p>", true);
        assert_eq!(t.len(), 7);
        match &t[0] {
            Tok::Open { name, attrs, .. } => {
                assert_eq!(name, "p");
                assert_eq!(attr(attrs, "class"), Some("a b"));
            }
            _ => panic!(),
        }
        assert_eq!(decode_entities("a &lt; b &#233; &#x41; &foo;"), "a < b é A &foo;");
    }
}
