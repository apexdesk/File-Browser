//! Aceptación de conexiones, límites y enrutado.

use crate::http::{self, Req, Rng};
use crate::jail::{self, Root};
use crate::page::{self, Hit, Row};
use crate::{mime, timefmt, walk, webui, zipstream};
use std::collections::HashMap;
use std::fs::File;
use std::io::{self, BufWriter, Read, Seek, SeekFrom, Write};
use crate::stream::Stream;
use std::net::{IpAddr, TcpListener, TcpStream};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const MAX_LIST: usize = 20_000;
const MAX_HITS: usize = 300;
const MAX_VISITED: usize = 200_000;
const MAX_ZIP_ENTRIES: usize = 200_000;
const SEARCH_TIME: Duration = Duration::from_secs(3);
const MAX_PER_IP: usize = 8;
const FAIL_LIMIT: u32 = 20;
const FAIL_WINDOW: Duration = Duration::from_secs(60);

pub struct Ctx {
    pub roots: Vec<Root>,
    pub token: String,
    pub hidden: bool,
    pub tls: Option<crate::tls::Config>,
    limiter: Mutex<HashMap<IpAddr, (u32, Instant)>>,
    per_ip: Mutex<HashMap<IpAddr, usize>>,
}

impl Ctx {
    pub fn new(roots: Vec<Root>, token: String, hidden: bool, tls: Option<crate::tls::Config>) -> Self {
        Ctx { roots, token, hidden, tls, limiter: Mutex::new(HashMap::new()), per_ip: Mutex::new(HashMap::new()) }
    }

    /// Reserva un hueco de conexión para esa IP; `false` si ya tiene demasiadas abiertas.
    fn enter(&self, ip: Option<IpAddr>) -> bool {
        let Some(ip) = ip.map(ip_key) else { return true };
        let Ok(mut m) = self.per_ip.lock() else { return true };
        let n = m.entry(ip).or_insert(0);
        if *n >= MAX_PER_IP {
            return false;
        }
        *n += 1;
        true
    }

    fn leave(&self, ip: Option<IpAddr>) {
        let Some(ip) = ip.map(ip_key) else { return };
        let Ok(mut m) = self.per_ip.lock() else { return };
        if let Some(n) = m.get_mut(&ip) {
            *n = n.saturating_sub(1);
            if *n == 0 {
                m.remove(&ip);
            }
        }
    }

    fn blocked(&self, ip: Option<IpAddr>) -> bool {
        let Some(ip) = ip.map(ip_key) else { return false };
        let Ok(m) = self.limiter.lock() else { return false };
        m.get(&ip).is_some_and(|(n, t)| *n >= FAIL_LIMIT && t.elapsed() < FAIL_WINDOW)
    }

    fn fail(&self, ip: Option<IpAddr>) {
        let Some(ip) = ip.map(ip_key) else { return };
        let Ok(mut m) = self.limiter.lock() else { return };
        if m.len() > 4096 {
            m.retain(|_, (_, t)| t.elapsed() < FAIL_WINDOW);
            if m.len() > 4096 {
                m.clear();
            }
        }
        let e = m.entry(ip).or_insert((0, Instant::now()));
        if e.1.elapsed() >= FAIL_WINDOW {
            *e = (0, Instant::now());
        }
        e.0 = e.0.saturating_add(1);
    }
}

/// Clave de limitación: las IPv6 se agrupan por /64 (un atacante controla todo ese rango).
fn ip_key(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(a) => {
            let mut o = a.octets();
            o[8..].fill(0);
            IpAddr::V6(o.into())
        }
        v4 => v4,
    }
}

/// Abre sin seguir un enlace en el último componente (cierra la ventana de carrera entre comprobar y abrir).
fn open_nofollow(p: &Path) -> io::Result<File> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW).open(p)
    }
    #[cfg(not(unix))]
    {
        File::open(p)
    }
}

fn ct_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    let mut d = (a.len() ^ b.len()) as u32;
    for i in 0..a.len().max(b.len()) {
        d |= u32::from(a.get(i).copied().unwrap_or(0) ^ b.get(i).copied().unwrap_or(0));
    }
    d == 0
}

struct Guard {
    active: Arc<AtomicUsize>,
    ctx: Arc<Ctx>,
    ip: Option<IpAddr>,
}
impl Drop for Guard {
    fn drop(&mut self) {
        self.active.fetch_sub(1, Ordering::SeqCst);
        self.ctx.leave(self.ip);
    }
}

pub fn serve(listener: TcpListener, ctx: Arc<Ctx>, max_conn: usize) {
    let active = Arc::new(AtomicUsize::new(0));
    for conn in listener.incoming() {
        let mut s = match conn {
            Ok(s) => s,
            Err(_) => {
                std::thread::sleep(Duration::from_millis(50));
                continue;
            }
        };
        let ip = s.peer_addr().ok().map(|a| a.ip());
        if active.fetch_add(1, Ordering::SeqCst) >= max_conn || !ctx.enter(ip) {
            // `enter` solo reserva cuando devuelve true, así que aquí basta con soltar el hueco global
            active.fetch_sub(1, Ordering::SeqCst);
            let _ = s.set_write_timeout(Some(Duration::from_secs(2)));
            let _ = http::error(&mut s, 503, false);
            continue;
        }
        let g = Guard { active: active.clone(), ctx: ctx.clone(), ip };
        let ctx = ctx.clone();
        let r = std::thread::Builder::new().stack_size(512 * 1024).spawn(move || {
            let _g = g;
            handle(s, &ctx);
        });
        let _ = r; // si no se pudo crear el hilo, el cierre del closure suelta `g` y devuelve los huecos
    }
}

fn handle(tcp: TcpStream, ctx: &Ctx) {
    let _ = tcp.set_write_timeout(Some(Duration::from_secs(30)));
    let _ = tcp.set_nodelay(true);
    let ip = tcp.peer_addr().ok().map(|a| a.ip());
    let mut s = match &ctx.tls {
        None => Stream::plain(tcp),
        #[cfg(feature = "tls")]
        Some(cfg) => match rustls::ServerConnection::new(cfg.clone()) {
            Ok(c) => Stream::tls(c, tcp),
            Err(_) => return,
        },
        #[cfg(not(feature = "tls"))]
        Some(never) => match *never {},
    };
    match http::read_request(&mut s) {
        Ok(req) => {
            // si la respuesta se corta a medias no se cierra «limpio»: el cliente debe notar el fallo
            if route(&mut s, ctx, &req, ip).is_ok() {
                s.finish();
            }
        }
        Err(code) => {
            let _ = http::error(&mut s, code, false);
            s.finish();
        }
    }
}

fn query_get(q: &str, key: &str) -> Option<String> {
    q.split('&').find_map(|kv| {
        let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
        (k == key).then(|| jail::pct_decode(&v.replace('+', " "))).flatten()
    })
}

fn route(s: &mut Stream, ctx: &Ctx, req: &Req, ip: Option<IpAddr>) -> io::Result<()> {
    if ctx.blocked(ip) {
        return http::error(s, 429, req.head);
    }
    let Some(rest) = req.path.strip_prefix("/s/") else {
        return http::error(s, 404, req.head);
    };
    let (tok, tail) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, ""),
    };
    if !ct_eq(tok, &ctx.token) {
        ctx.fail(ip);
        return http::error(s, 404, req.head);
    }
    if tail.is_empty() {
        return http::redirect(s, &format!("/s/{tok}/"), req.head);
    }
    if let Some(api) = tail.strip_prefix("/api/") {
        return api_route(s, ctx, req, api);
    }
    if tail == "/app" {
        return http::redirect(s, &format!("/s/{tok}/app/"), req.head);
    }
    if let Some(f) = tail.strip_prefix("/app/") {
        let f = if f.is_empty() { "index.html" } else { f };
        let Some((ctype, body)) = webui::get(f) else { return http::error(s, 404, req.head) };
        return http::send(s, 200, ctype, body, req.head, &[("Content-Security-Policy", webui::CSP.to_string())]);
    }
    let Some((comps, slash)) = jail::split_url(tail) else {
        return http::error(s, 404, req.head);
    };
    if comps.is_empty() {
        let names: Vec<String> = ctx.roots.iter().map(|r| r.name.clone()).collect();
        return http::send(s, 200, "text/html; charset=utf-8", page::roots_page(tok, &names, webui::available()).as_bytes(), req.head, &[]);
    }
    let Some((root, canon)) = jail::resolve(&ctx.roots, &comps, ctx.hidden) else {
        return http::error(s, 404, req.head);
    };
    let Ok(md) = std::fs::metadata(&canon) else {
        return http::error(s, 404, req.head);
    };
    if md.is_dir() {
        if !slash {
            let mut to = page::base_url(tok, &comps);
            if !req.query.is_empty() {
                to.push('?');
                to.push_str(&req.query);
            }
            return http::redirect(s, &to, req.head);
        }
        if query_get(&req.query, "zip").is_some() {
            return send_zip(s, ctx, req, &comps, &canon);
        }
        if let Some(q) = query_get(&req.query, "q").filter(|q| !q.trim().is_empty()) {
            return send_search(s, ctx, req, &comps, &canon, q.trim());
        }
        let (mut rows, truncated) = list_dir(ctx, root, &canon);
        let sort = query_get(&req.query, "sort").unwrap_or_default();
        let desc = query_get(&req.query, "o").as_deref() == Some("d");
        sort_rows(&mut rows, &sort, desc);
        let html = page::dir_page(tok, &comps, &rows, &sort, desc, truncated);
        http::send(s, 200, "text/html; charset=utf-8", html.as_bytes(), req.head, &[])
    } else if md.is_file() {
        if slash {
            return http::error(s, 404, req.head);
        }
        send_file(s, req, &comps, &canon)
    } else {
        http::error(s, 404, req.head)
    }
}

/// Ruta virtual «a/b/c» (ya decodificada) → componentes validados.
fn virt_path(p: &str) -> Option<Vec<String>> {
    let mut v = Vec::new();
    for c in p.split('/').filter(|c| !c.is_empty()) {
        if !jail::valid_component(c) {
            return None;
        }
        v.push(c.to_string());
    }
    (v.len() <= 64).then_some(v)
}

fn api_route(s: &mut Stream, ctx: &Ctx, req: &Req, api: &str) -> io::Result<()> {
    let json = "application/json; charset=utf-8";
    match api {
        "list" => {
            let Some(comps) = virt_path(&query_get(&req.query, "p").unwrap_or_default()) else {
                return http::error(s, 400, req.head);
            };
            let (mut rows, truncated);
            if comps.is_empty() {
                rows = ctx.roots.iter().map(|r| Row { name: r.name.clone(), is_dir: true, size: 0, mtime: 0 }).collect::<Vec<_>>();
                truncated = false;
            } else {
                let Some((root, canon)) = jail::resolve(&ctx.roots, &comps, ctx.hidden) else { return http::error(s, 404, req.head) };
                if !std::fs::metadata(&canon).is_ok_and(|m| m.is_dir()) {
                    return http::error(s, 404, req.head);
                }
                (rows, truncated) = list_dir(ctx, root, &canon);
            }
            sort_rows(&mut rows, "name", false);
            http::send(s, 200, json, webui::list_json(&comps, &rows, truncated).as_bytes(), req.head, &[])
        }
        _ => http::error(s, 404, req.head),
    }
}

fn list_dir(ctx: &Ctx, root: &Root, dir: &Path) -> (Vec<Row>, bool) {
    let mut rows = Vec::new();
    let Ok(rd) = std::fs::read_dir(dir) else { return (rows, false) };
    let mut truncated = false;
    for e in rd.flatten() {
        if rows.len() >= MAX_LIST {
            truncated = true;
            break;
        }
        let Ok(name) = e.file_name().into_string() else { continue };
        if !jail::valid_component(&name) || (!ctx.hidden && name.starts_with('.')) {
            continue;
        }
        let Ok(ft) = e.file_type() else { continue };
        let md = if ft.is_symlink() {
            let Ok(canon) = std::fs::canonicalize(e.path()) else { continue };
            if !jail::inside(root, &canon, ctx.hidden) {
                continue;
            }
            std::fs::metadata(&canon)
        } else {
            e.metadata()
        };
        let Ok(md) = md else { continue };
        if !md.is_dir() && !md.is_file() {
            continue;
        }
        rows.push(Row { name, is_dir: md.is_dir(), size: if md.is_dir() { 0 } else { md.len() }, mtime: md.modified().map(timefmt::unix).unwrap_or(0) });
    }
    (rows, truncated)
}

fn sort_rows(rows: &mut [Row], sort: &str, desc: bool) {
    rows.sort_by(|a, b| {
        // carpetas siempre primero, aunque se invierta el orden
        b.is_dir.cmp(&a.is_dir).then_with(|| {
            let o = match sort {
                "size" => a.size.cmp(&b.size),
                "date" => a.mtime.cmp(&b.mtime),
                _ => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
            };
            (if desc { o.reverse() } else { o }).then_with(|| a.name.cmp(&b.name))
        })
    });
}

fn send_search(s: &mut Stream, ctx: &Ctx, req: &Req, comps: &[String], dir: &Path, q: &str) -> io::Result<()> {
    let needle = q.to_lowercase();
    let t0 = Instant::now();
    let mut hits: Vec<Hit> = Vec::new();
    let mut truncated = false;
    walk::walk(dir, ctx.hidden, MAX_VISITED, &mut |it| {
        if t0.elapsed() > SEARCH_TIME {
            truncated = true;
            return false;
        }
        if it.rel.last().is_some_and(|n| n.to_lowercase().contains(&needle)) {
            if hits.len() >= MAX_HITS {
                truncated = true;
                return false;
            }
            hits.push(Hit { rel: it.rel, is_dir: it.is_dir, size: it.size });
        }
        true
    });
    let html = page::search_page(&ctx.token, comps, q, &hits, truncated);
    http::send(s, 200, "text/html; charset=utf-8", html.as_bytes(), req.head, &[])
}

fn ascii_name(n: &str) -> String {
    n.chars().map(|c| if c.is_ascii_graphic() && !matches!(c, '"' | '\\' | ';' | '%') || c == ' ' { c } else { '_' }).collect()
}

fn disposition(kind: &str, name: &str) -> String {
    format!("{kind}; filename=\"{}\"; filename*=UTF-8''{}", ascii_name(name), jail::pct_encode(name))
}

fn send_zip(s: &mut Stream, ctx: &Ctx, req: &Req, comps: &[String], dir: &Path) -> io::Result<()> {
    let name = format!("{}.zip", comps.last().map(String::as_str).unwrap_or("carpeta"));
    http::write_head(
        s,
        200,
        &[
            ("Content-Type", "application/zip".to_string()),
            ("Content-Disposition", disposition("attachment", &name)),
            ("Cache-Control", "private, no-store".to_string()),
            ("Content-Security-Policy", "default-src 'none'; sandbox".to_string()),
        ],
    )?;
    if req.head {
        return s.flush();
    }
    let top = comps.last().cloned().unwrap_or_default();
    let mut z = zipstream::Zip::new(BufWriter::with_capacity(64 * 1024, &mut *s));
    z.add_dir(&top, 0)?;
    let mut err: Option<io::Error> = None;
    walk::walk(dir, ctx.hidden, MAX_VISITED, &mut |it| {
        if z.count() >= MAX_ZIP_ENTRIES {
            return false;
        }
        let name = format!("{top}/{}", it.rel.join("/"));
        let r = if it.is_dir {
            z.add_dir(&name, it.mtime)
        } else {
            match open_nofollow(&it.path) {
                // un fichero ilegible se omite; un fallo de red corta el ZIP
                Err(_) => Ok(()),
                Ok(mut f) => match f.metadata() {
                    Ok(m) if m.is_file() => z.add_file(&name, it.mtime, &mut f, m.len()),
                    _ => Ok(()),
                },
            }
        };
        match r {
            Ok(()) => true,
            Err(e) => {
                err = Some(e);
                false
            }
        }
    });
    match err {
        Some(e) => Err(e), // se cierra la conexión sin directorio central: el cliente ve el ZIP incompleto
        None => z.finish(),
    }
}

fn send_file(s: &mut Stream, req: &Req, comps: &[String], path: &Path) -> io::Result<()> {
    let Ok(mut f) = open_nofollow(path) else { return http::error(s, 404, req.head) };
    let Ok(md) = f.metadata() else { return http::error(s, 404, req.head) };
    if !md.is_file() {
        return http::error(s, 404, req.head);
    }
    let len = md.len();
    let mtime = md.modified().map(timefmt::unix).unwrap_or(0);
    let etag = format!("W/\"{len:x}-{mtime:x}\"");
    let name = comps.last().map(String::as_str).unwrap_or("archivo");
    let kind = mime::of(name);
    let force_dl = query_get(&req.query, "dl").is_some();
    let inline = kind.inline && !force_dl;
    let csp = if kind.mime == "application/pdf" && inline { "default-src 'none'; object-src 'self'" } else { "default-src 'none'; sandbox" };
    let mut h: Vec<(&str, String)> = vec![
        ("ETag", etag.clone()),
        ("Last-Modified", timefmt::http_date(mtime)),
        ("Accept-Ranges", "bytes".to_string()),
        ("Cache-Control", "private, no-cache".to_string()),
        ("Content-Security-Policy", csp.to_string()),
        ("Content-Disposition", disposition(if inline { "inline" } else { "attachment" }, name)),
        ("Content-Type", if inline { kind.mime.to_string() } else { "application/octet-stream".to_string() }),
    ];
    if req.inm.as_deref().is_some_and(|v| v == "*" || v.split(',').any(|t| t.trim() == etag)) {
        h.retain(|(k, _)| matches!(*k, "ETag" | "Last-Modified" | "Cache-Control"));
        http::write_head(s, 304, &h)?;
        return s.flush();
    }
    let (code, start, count) = match http::parse_range(req.range.as_deref(), len) {
        Rng::Full => (200, 0, len),
        Rng::Part(a, b) => {
            h.push(("Content-Range", format!("bytes {a}-{b}/{len}")));
            (206, a, b - a + 1)
        }
        Rng::Bad => {
            return http::write_head(s, 416, &[("Content-Range", format!("bytes */{len}")), ("Content-Length", "0".to_string())]).and_then(|_| s.flush());
        }
    };
    h.push(("Content-Length", count.to_string()));
    http::write_head(s, code, &h)?;
    if req.head {
        return s.flush();
    }
    if start > 0 {
        f.seek(SeekFrom::Start(start))?;
    }
    let mut left = count;
    let mut buf = vec![0u8; 64 * 1024];
    while left > 0 {
        let want = usize::try_from(left.min(buf.len() as u64)).unwrap_or(buf.len());
        let n = match f.read(&mut buf[..want]) {
            Ok(0) => break, // el archivo se encogió: se corta (Content-Length no se cumple y el cliente lo nota)
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        s.write_all(&buf[..n])?;
        left -= n as u64;
    }
    s.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ipv6_se_agrupa_por_64() {
        let a: IpAddr = "2001:db8:1:2:aaaa:bbbb:cccc:dddd".parse().unwrap();
        let b: IpAddr = "2001:db8:1:2:1::1".parse().unwrap();
        let c: IpAddr = "2001:db8:1:3::1".parse().unwrap();
        assert_eq!(ip_key(a), ip_key(b));
        assert_ne!(ip_key(a), ip_key(c));
        let v4: IpAddr = "10.0.0.7".parse().unwrap();
        assert_eq!(ip_key(v4), v4);
    }

    #[test]
    fn comparacion_constante() {
        assert!(ct_eq("abc", "abc"));
        assert!(!ct_eq("abc", "abd"));
        assert!(!ct_eq("abc", "abcd"));
        assert!(!ct_eq("", "a"));
    }

    #[test]
    fn nombre_ascii_sin_comillas() {
        assert_eq!(ascii_name("a\"b;ñ\\.txt"), "a_b___.txt");
        assert!(disposition("inline", "ñ.txt").contains("filename*=UTF-8''%C3%B1.txt"));
    }

    #[test]
    fn consultas() {
        assert_eq!(query_get("q=a+b%21&x=1", "q").as_deref(), Some("a b!"));
        assert_eq!(query_get("zip", "zip").as_deref(), Some(""));
        assert!(query_get("a=1", "q").is_none());
    }
}
