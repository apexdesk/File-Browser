//! HTTP/1.1 mínimo: solo GET/HEAD, una petición por conexión, cabeceras acotadas.

use std::io::{self, Read, Write};
use crate::stream::Stream;
use std::time::Duration;

const MAX_HEAD: usize = 8192;
const MAX_HEADERS: usize = 64;
const HEAD_DEADLINE: Duration = Duration::from_secs(10);

pub struct Req {
    pub head: bool,
    pub path: String,
    pub query: String,
    pub range: Option<String>,
    pub inm: Option<String>,
}

/// Lee y valida una petición. El `Err` es el código de estado con el que responder.
pub fn read_request(s: &mut Stream) -> Result<Req, u16> {
    s.set_deadline(Some(HEAD_DEADLINE));
    let r = read_head(s);
    s.set_deadline(None);
    r
}

fn read_head(s: &mut Stream) -> Result<Req, u16> {
    let mut buf: Vec<u8> = Vec::with_capacity(1024);
    let mut chunk = [0u8; 1024];
    let end = loop {
        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break i;
        }
        if buf.len() >= MAX_HEAD {
            return Err(431);
        }
        match s.read(&mut chunk) {
            Ok(0) => return Err(400),
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
            Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => return Err(408),
            Err(_) => return Err(400),
        }
    };
    if end > MAX_HEAD {
        return Err(431);
    }
    parse(&buf[..end])
}

pub fn parse(head: &[u8]) -> Result<Req, u16> {
    let text = std::str::from_utf8(head).map_err(|_| 400u16)?;
    let mut lines = text.split("\r\n");
    let first = lines.next().ok_or(400u16)?;
    let mut p = first.split(' ');
    let (method, target, version) = (p.next().ok_or(400u16)?, p.next().ok_or(400u16)?, p.next().ok_or(400u16)?);
    if p.next().is_some() || !(version == "HTTP/1.1" || version == "HTTP/1.0") {
        return Err(400);
    }
    let head_only = match method {
        "GET" => false,
        "HEAD" => true,
        _ => return Err(405),
    };
    if !target.starts_with('/') || target.contains('#') || target.bytes().any(|b| b < 0x21 || b == 0x7f) {
        return Err(400);
    }
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    let mut req = Req { head: head_only, path: path.to_string(), query: query.to_string(), range: None, inm: None };
    let mut n = 0;
    for l in lines {
        n += 1;
        if n > MAX_HEADERS || l.starts_with(' ') || l.starts_with('\t') {
            return Err(400);
        }
        let (k, v) = l.split_once(':').ok_or(400u16)?;
        if k.is_empty() || k.contains([' ', '\t']) {
            return Err(400);
        }
        let v = v.trim();
        match k.to_ascii_lowercase().as_str() {
            "range" => req.range = Some(v.to_string()),
            "if-none-match" => req.inm = Some(v.to_string()),
            "transfer-encoding" => return Err(400),
            "content-length" if v != "0" => return Err(400),
            _ => {}
        }
    }
    Ok(req)
}

pub fn reason(code: u16) -> &'static str {
    match code {
        200 => "OK",
        206 => "Partial Content",
        301 => "Moved Permanently",
        304 => "Not Modified",
        400 => "Bad Request",
        404 => "Not Found",
        405 => "Method Not Allowed",
        408 => "Request Timeout",
        416 => "Range Not Satisfiable",
        429 => "Too Many Requests",
        431 => "Request Header Fields Too Large",
        503 => "Service Unavailable",
        _ => "Error",
    }
}

/// Escribe línea de estado y cabeceras (más las de seguridad comunes). Los valores se sanean.
pub fn write_head(w: &mut impl Write, code: u16, headers: &[(&str, String)]) -> io::Result<()> {
    let mut o = String::with_capacity(512);
    o.push_str(&format!("HTTP/1.1 {code} {}\r\n", reason(code)));
    for (k, v) in headers {
        let v: String = v.chars().filter(|c| *c != '\r' && *c != '\n').collect();
        o.push_str(&format!("{k}: {v}\r\n"));
    }
    o.push_str("Connection: close\r\nX-Content-Type-Options: nosniff\r\nReferrer-Policy: no-referrer\r\nX-Frame-Options: DENY\r\nCross-Origin-Resource-Policy: same-origin\r\n\r\n");
    w.write_all(o.as_bytes())
}

/// Respuesta completa de texto/HTML.
pub fn send(w: &mut impl Write, code: u16, ctype: &str, body: &[u8], head_only: bool, extra: &[(&str, String)]) -> io::Result<()> {
    let mut h: Vec<(&str, String)> = vec![
        ("Content-Type", ctype.to_string()),
        ("Content-Length", body.len().to_string()),
        ("Cache-Control", "private, no-store".to_string()),
        ("Content-Security-Policy", "default-src 'none'; style-src 'unsafe-inline'; img-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'".to_string()),
    ];
    if extra.iter().any(|(k, _)| k.eq_ignore_ascii_case("content-security-policy")) {
        h.retain(|(k, _)| !k.eq_ignore_ascii_case("content-security-policy"));
    }
    h.extend(extra.iter().map(|(k, v)| (*k, v.clone())));
    write_head(w, code, &h)?;
    if !head_only {
        w.write_all(body)?;
    }
    w.flush()
}

/// Error simple de texto.
pub fn error(w: &mut impl Write, code: u16, head_only: bool) -> io::Result<()> {
    let extra = if code == 405 { vec![("Allow", "GET, HEAD".to_string())] } else { Vec::new() };
    send(w, code, "text/plain; charset=utf-8", format!("{code} {}\n", reason(code)).as_bytes(), head_only, &extra)
}

pub fn redirect(w: &mut impl Write, to: &str, head_only: bool) -> io::Result<()> {
    send(w, 301, "text/plain; charset=utf-8", b"", head_only, &[("Location", to.to_string())])
}

pub enum Rng {
    Full,
    Part(u64, u64),
    Bad,
}

/// Interpreta `Range: bytes=…` (un solo rango; varios o sintaxis rara se ignoran).
pub fn parse_range(h: Option<&str>, len: u64) -> Rng {
    let Some(spec) = h.and_then(|h| h.strip_prefix("bytes=")) else { return Rng::Full };
    if spec.contains(',') || len == 0 {
        return Rng::Full;
    }
    let Some((a, b)) = spec.trim().split_once('-') else { return Rng::Full };
    let last = len - 1;
    if a.is_empty() {
        return match b.parse::<u64>() {
            Ok(0) => Rng::Bad,
            Ok(n) => Rng::Part(len.saturating_sub(n), last),
            Err(_) => Rng::Full,
        };
    }
    let Ok(a) = a.parse::<u64>() else { return Rng::Full };
    let b = if b.is_empty() {
        last
    } else {
        match b.parse::<u64>() {
            Ok(b) => b.min(last),
            Err(_) => return Rng::Full,
        }
    };
    if a > last {
        return Rng::Bad;
    }
    if b < a { Rng::Full } else { Rng::Part(a, b) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(s: &str) -> Result<Req, u16> {
        parse(s.as_bytes())
    }

    #[test]
    fn peticiones_validas_e_invalidas() {
        let r = req("GET /s/abc/?x=1 HTTP/1.1\r\nHost: a\r\nRange: bytes=0-9").unwrap();
        assert_eq!((r.path.as_str(), r.query.as_str(), r.range.as_deref()), ("/s/abc/", "x=1", Some("bytes=0-9")));
        assert_eq!(req("POST / HTTP/1.1").err(), Some(405));
        assert_eq!(req("GET / HTTP/2").err(), Some(400));
        assert_eq!(req("GET http://x/ HTTP/1.1").err(), Some(400));
        assert_eq!(req("GET /a b HTTP/1.1").err(), Some(400));
        assert_eq!(req("GET / HTTP/1.1\r\nTransfer-Encoding: chunked").err(), Some(400));
        assert_eq!(req("GET / HTTP/1.1\r\nContent-Length: 5").err(), Some(400));
        assert_eq!(req("GET / HTTP/1.1\r\n folded: x").err(), Some(400));
        assert_eq!(req("GET / HTTP/1.1\r\nsin-dos-puntos").err(), Some(400));
        assert_eq!(req("GET / HTTP/1.1\r\nTransfer-Encoding : chunked").err(), Some(400));
    }

    #[test]
    fn rangos() {
        assert!(matches!(parse_range(Some("bytes=0-9"), 100), Rng::Part(0, 9)));
        assert!(matches!(parse_range(Some("bytes=90-"), 100), Rng::Part(90, 99)));
        assert!(matches!(parse_range(Some("bytes=-10"), 100), Rng::Part(90, 99)));
        assert!(matches!(parse_range(Some("bytes=5-500"), 100), Rng::Part(5, 99)));
        assert!(matches!(parse_range(Some("bytes=100-"), 100), Rng::Bad));
        assert!(matches!(parse_range(Some("bytes=-0"), 100), Rng::Bad));
        assert!(matches!(parse_range(Some("bytes=0-1,5-6"), 100), Rng::Full));
        assert!(matches!(parse_range(Some("bytes=9-3"), 100), Rng::Full));
        assert!(matches!(parse_range(Some("items=0-1"), 100), Rng::Full));
        assert!(matches!(parse_range(Some("bytes=0-5"), 0), Rng::Full));
        assert!(matches!(parse_range(None, 10), Rng::Full));
    }
}
