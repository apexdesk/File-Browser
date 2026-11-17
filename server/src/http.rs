//! HTTP/1.1 mínimo: solo GET/HEAD, una petición por conexión, cabeceras acotadas.

use std::io::{self, Read, Write};
use crate::stream::Stream;
use std::time::Duration;

const MAX_HEAD: usize = 8192;
const MAX_HEADERS: usize = 64;
/// Antes de la autenticación todo es de un desconocido: plazo corto para handshake TLS + cabeceras.
const HEAD_DEADLINE: Duration = Duration::from_secs(5);
const ERR_DEADLINE: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Method {
    Get,
    Head,
    Put,
    Post,
}

pub struct Req {
    pub method: Method,
    pub head: bool,
    pub path: String,
    pub query: String,
    pub range: Option<String>,
    pub inm: Option<String>,
    pub auth: Option<String>,
    /// `Cookie` (solo se usa la de sesión que deja entrar con la IP bloqueada; ver `server::route`)
    pub cookie: Option<String>,
    /// Solo PUT/POST: tamaño del cuerpo, cabeceras para el control de origen y lo ya leído del cuerpo.
    pub content_length: u64,
    pub has_length: bool,
    pub origin: Option<String>,
    pub host: Option<String>,
    pub xfb: bool,
    pub leftover: Vec<u8>,
    /// Idioma de las páginas y mensajes (de `Accept-Language`).
    pub lang: Lang,
    /// Última dirección de `X-Forwarded-For` (solo cuenta con `--proxy` y si la conexión llega del propio equipo).
    pub fwd: Option<String>,
}

/// Idiomas de las páginas del servidor: español (por defecto) o inglés.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Lang {
    #[default]
    Es,
    En,
}

impl Lang {
    pub fn code(self) -> &'static str {
        match self {
            Lang::Es => "es",
            Lang::En => "en",
        }
    }

    /// Elige entre los dos textos.
    pub fn tr(self, es: &'static str, en: &'static str) -> &'static str {
        if self == Lang::En { en } else { es }
    }
}

/// `Accept-Language` → idioma: el de más peso entre «es» y «en»; si solo pide otros, inglés.
pub fn pick_lang(v: &str) -> Lang {
    let mut best: Option<(f32, Lang)> = None;
    let mut other = false;
    for part in v.split(',') {
        let mut it = part.split(';');
        let tag = it.next().unwrap_or("").trim().to_ascii_lowercase();
        let q = it.find_map(|p| p.trim().strip_prefix("q=").and_then(|q| q.parse::<f32>().ok())).unwrap_or(1.0);
        if tag.is_empty() || q <= 0.0 {
            continue;
        }
        let l = match tag.split('-').next() {
            Some("es") => Lang::Es,
            Some("en") => Lang::En,
            _ => {
                other = true;
                continue;
            }
        };
        if best.is_none_or(|(bq, _)| q > bq) {
            best = Some((q, l));
        }
    }
    match best {
        Some((_, l)) => l,
        None if other => Lang::En,
        None => Lang::Es,
    }
}

/// Lee y valida una petición. El `Err` es el código de estado con el que responder.
pub fn read_request(s: &mut Stream) -> Result<Req, u16> {
    s.set_deadline(Some(HEAD_DEADLINE));
    let r = read_head(s);
    // si falla, la respuesta de error también tiene plazo: con TLS el «write» puede estar completando
    // un handshake que nunca llega, y sin plazo retendría el hilo para siempre
    s.set_deadline(if r.is_err() { Some(ERR_DEADLINE) } else { None });
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
    let mut req = parse(&buf[..end])?;
    req.leftover = buf[(end + 4).min(buf.len())..].to_vec();
    Ok(req)
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
    let method = match method {
        "GET" => Method::Get,
        "HEAD" => Method::Head,
        "PUT" => Method::Put,
        "POST" => Method::Post,
        _ => return Err(405),
    };
    let head_only = method == Method::Head;
    if !target.starts_with('/') || target.contains('#') || target.bytes().any(|b| b < 0x21 || b == 0x7f) {
        return Err(400);
    }
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    let mut req = Req { method, head: head_only, path: path.to_string(), query: query.to_string(), range: None, inm: None, auth: None, cookie: None, content_length: 0, has_length: false, origin: None, host: None, xfb: false, leftover: Vec::new(), lang: Lang::Es, fwd: None };
    let mut n = 0;
    let mut seen = std::collections::HashSet::new();
    for l in lines {
        n += 1;
        if n > MAX_HEADERS || l.starts_with(' ') || l.starts_with('\t') {
            return Err(400);
        }
        // un CR o LF suelto lo trataría como fin de línea otro intermediario y no este servidor
        if l.contains(['\r', '\n']) {
            return Err(400);
        }
        let (k, v) = l.split_once(':').ok_or(400u16)?;
        if k.is_empty() || k.contains([' ', '\t']) {
            return Err(400);
        }
        let v = v.trim();
        let key = k.to_ascii_lowercase();
        // una cabecera que decide algo no puede repetirse: dos lecturas distintas serían una puerta a la ambigüedad
        if matches!(key.as_str(), "content-length" | "host" | "origin" | "x-fb" | "authorization" | "range" | "if-none-match") && !seen.insert(key.clone()) {
            return Err(400);
        }
        match key.as_str() {
            "range" => req.range = Some(v.to_string()),
            "if-none-match" => req.inm = Some(v.to_string()),
            "authorization" => req.auth = Some(v.to_string()),
            // HTTP/1.1 permite varias `Cookie`; se juntan como haría un intermediario
            "cookie" => match &mut req.cookie {
                Some(c) if c.len() + v.len() < MAX_HEAD => {
                    c.push_str("; ");
                    c.push_str(v);
                }
                Some(_) => {}
                None => req.cookie = Some(v.to_string()),
            },
            "transfer-encoding" => return Err(400),
            "content-length" => {
                if v.is_empty() || !v.bytes().all(|b| b.is_ascii_digit()) {
                    return Err(400);
                }
                req.content_length = v.parse::<u64>().map_err(|_| 400u16)?;
                req.has_length = true;
                // un GET con cuerpo no tiene sentido aquí
                if req.content_length != 0 && matches!(method, Method::Get | Method::Head) {
                    return Err(400);
                }
            }
            "origin" => req.origin = Some(v.to_string()),
            "host" => req.host = Some(v.to_string()),
            "x-fb" => req.xfb = v == "1",
            "accept-language" => req.lang = pick_lang(v),
            // puede repetirse: cuenta la última dirección (la que puso el intermediario más cercano)
            "x-forwarded-for" => req.fwd = v.rsplit(',').next().map(|a| a.trim().to_string()).filter(|a| !a.is_empty()),
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
        403 => "Forbidden",
        401 => "Unauthorized",
        404 => "Not Found",
        405 => "Method Not Allowed",
        408 => "Request Timeout",
        409 => "Conflict",
        411 => "Length Required",
        412 => "Precondition Failed",
        413 => "Payload Too Large",
        416 => "Range Not Satisfiable",
        429 => "Too Many Requests",
        431 => "Request Header Fields Too Large",
        500 => "Internal Server Error",
        503 => "Service Unavailable",
        507 => "Insufficient Storage",
        _ => "Error",
    }
}

thread_local! {
    /// Idioma de la petición de este hilo (un hilo por conexión); `None` antes de leerla.
    static LANG: std::cell::Cell<Option<Lang>> = const { std::cell::Cell::new(None) };
    /// Cabeceras que lleva cualquier respuesta de esta conexión (un hilo por conexión y una petición por
    /// conexión): así `Set-Cookie` sale igual en un listado, una descarga, un ZIP o un error.
    static EXTRA: std::cell::RefCell<Vec<(&'static str, String)>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// Añade una cabecera a todas las respuestas que se escriban después desde este hilo.
pub fn add_extra(k: &'static str, v: String) {
    EXTRA.with(|e| e.borrow_mut().push((k, v)));
}

pub fn clear_extra() {
    EXTRA.with(|e| e.borrow_mut().clear());
    LANG.with(|l| l.set(None));
}

/// Idioma de las páginas de error que se escriban después desde este hilo.
pub fn set_lang(l: Lang) {
    LANG.with(|c| c.set(Some(l)));
}

/// Escribe línea de estado y cabeceras (más las de seguridad comunes). Los valores se sanean.
pub fn write_head(w: &mut impl Write, code: u16, headers: &[(&str, String)]) -> io::Result<()> {
    let mut o = String::with_capacity(512);
    o.push_str(&format!("HTTP/1.1 {code} {}\r\n", reason(code)));
    let extra = EXTRA.with(|e| e.borrow().clone());
    for (k, v) in headers.iter().map(|(k, v)| (*k, v)).chain(extra.iter().map(|(k, v)| (*k, v))) {
        let v: String = v.chars().filter(|c| *c != '\r' && *c != '\n').collect();
        o.push_str(&format!("{k}: {v}\r\n"));
    }
    o.push_str("Connection: close\r\nX-Content-Type-Options: nosniff\r\nReferrer-Policy: no-referrer\r\nX-Frame-Options: DENY\r\nCross-Origin-Resource-Policy: same-origin\r\nCross-Origin-Opener-Policy: same-origin\r\n\r\n");
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

/// Explicación de un código de error (español, inglés).
fn explain(code: u16) -> (&'static str, &'static str) {
    match code {
        400 => ("La petición no es válida.", "The request is not valid."),
        401 => ("Se necesita usuario y contraseña.", "A username and password are required."),
        403 => ("No está permitido.", "Not allowed."),
        404 => ("No existe, o el enlace no es válido o ha caducado.", "Not found, or the link is not valid or has expired."),
        405 => ("No está permitido (el servidor puede ser de solo lectura).", "Not allowed (the server may be read-only)."),
        408 => ("La petición tardó demasiado.", "The request took too long."),
        409 => ("Ya existe o está en conflicto con otro elemento.", "It already exists or conflicts with another item."),
        411 => ("Falta la longitud del envío.", "The upload length is missing."),
        412 => ("Ya existe.", "It already exists."),
        413 => ("Es demasiado grande para este servidor.", "It is too large for this server."),
        416 => ("Ese rango no existe en el archivo.", "That range does not exist in the file."),
        429 => ("Demasiados intentos: espera un minuto y vuelve a probar.", "Too many attempts: wait a minute and try again."),
        431 => ("La petición es demasiado larga.", "The request is too long."),
        503 => ("El servidor está ocupado: vuelve a probar en un momento.", "The server is busy: try again in a moment."),
        507 => ("No queda espacio en el servidor.", "There is no space left on the server."),
        _ => ("Error del servidor.", "Server error."),
    }
}

/// Página de error breve, en el idioma de la petición (en los dos si aún no se conoce).
pub fn error_page(code: u16) -> String {
    let (es, en) = explain(code);
    let (lang, msg) = match LANG.with(|l| l.get()) {
        Some(l) => (l.code(), l.tr(es, en).to_string()),
        None => ("es", format!("{es}<br>{en}")),
    };
    format!(
        "<!doctype html><html lang=\"{lang}\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\
         <title>{code} {r}</title><style>body{{font:15px system-ui,sans-serif;padding:16px}}@media(prefers-color-scheme:dark){{body{{background:#17181c;color:#e6e6e9}}}}</style></head>\
         <body><h1>{code}</h1><p>{msg}</p></body></html>",
        r = reason(code)
    )
}

/// Error como página HTML breve.
pub fn error(w: &mut impl Write, code: u16, head_only: bool) -> io::Result<()> {
    error_h(w, code, head_only, &[])
}

/// `error` con cabeceras extra (`Retry-After`, `WWW-Authenticate`…).
pub fn error_h(w: &mut impl Write, code: u16, head_only: bool, extra: &[(&str, String)]) -> io::Result<()> {
    let mut h: Vec<(&str, String)> = extra.to_vec();
    if code == 405 {
        h.push(("Allow", "GET, HEAD, PUT, POST".to_string()));
    }
    send(w, code, "text/html; charset=utf-8", error_page(code).as_bytes(), head_only, &h)
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
        assert_eq!(req("DELETE / HTTP/1.1").err(), Some(405));
        assert_eq!(req("PUT /x HTTP/1.1\r\nContent-Length: 5\r\nOrigin: http://a\r\nX-FB: 1").map(|r| (r.method, r.content_length, r.xfb)).ok(), Some((Method::Put, 5, true)));
        assert_eq!(req("PUT /x HTTP/1.1\r\nContent-Length: -1").err(), Some(400));
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
    fn cabeceras_ambiguas_se_rechazan() {
        // repetidas
        assert_eq!(req("PUT /x HTTP/1.1\r\nContent-Length: 3\r\nContent-Length: 0").err(), Some(400));
        assert_eq!(req("GET / HTTP/1.1\r\nHost: a\r\nHost: b").err(), Some(400));
        assert_eq!(req("PUT /x HTTP/1.1\r\nX-FB: 1\r\nx-fb: 1").err(), Some(400));
        // solo dígitos
        assert_eq!(req("PUT /x HTTP/1.1\r\nContent-Length: +1").err(), Some(400));
        assert_eq!(req("PUT /x HTTP/1.1\r\nContent-Length: 0x1").err(), Some(400));
        assert_eq!(req("PUT /x HTTP/1.1\r\nContent-Length:").err(), Some(400));
        // LF o CR sueltos dentro de la cabecera
        assert_eq!(req("PUT /x HTTP/1.1\r\nX-A: 1\nTransfer-Encoding: chunked\r\nContent-Length: 1").err(), Some(400));
        assert_eq!(req("GET / HTTP/1.1\r\nX-A: 1\rY: 2").err(), Some(400));
        // la longitud declarada se distingue de la ausente
        assert_eq!(req("PUT /x HTTP/1.1\r\nContent-Length: 0").map(|r| r.has_length).ok(), Some(true));
        assert_eq!(req("PUT /x HTTP/1.1\r\nHost: a").map(|r| r.has_length).ok(), Some(false));
    }

    #[test]
    fn idioma_y_reenvio() {
        assert_eq!(pick_lang("en-US,en;q=0.9,es;q=0.8"), Lang::En);
        assert_eq!(pick_lang("es-ES,es;q=0.9,en;q=0.8"), Lang::Es);
        assert_eq!(pick_lang("fr-FR,en;q=0.5,es;q=0.7"), Lang::Es);
        assert_eq!(pick_lang("de-DE"), Lang::En);
        assert_eq!(pick_lang(""), Lang::Es);
        assert_eq!(req("GET / HTTP/1.1\r\nAccept-Language: en").map(|r| r.lang).ok(), Some(Lang::En));
        let r = req("GET / HTTP/1.1\r\nX-Forwarded-For: 1.2.3.4, 10.0.0.1\r\nX-Forwarded-For: 5.6.7.8").unwrap();
        assert_eq!(r.fwd.as_deref(), Some("5.6.7.8"));
        // página de error en el idioma pedido, sin él en los dos
        clear_extra();
        assert!(error_page(429).contains("espera un minuto") && error_page(429).contains("wait a minute"));
        set_lang(Lang::En);
        assert!(error_page(429).contains("wait a minute") && !error_page(429).contains("espera"));
        clear_extra();
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
