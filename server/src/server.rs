//! Aceptación de conexiones, límites y enrutado.

use crate::http::{self, Method, Req, Rng};
use crate::jail::{self, Root};
use crate::page::{self, Hit, Row};
use crate::{mime, timefmt, walk, webui, zipstream};
use std::collections::HashMap;
use std::fs::File;
use std::io::{self, BufWriter, Read, Seek, SeekFrom, Write};
use crate::stream::Stream;
use std::net::{IpAddr, TcpListener, TcpStream};
use std::path::Path;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const MAX_LIST: usize = 20_000;
/// Entradas que se miran como mucho en una carpeta: el listado se queda con las `MAX_LIST` primeras
/// en el orden pedido (no en el del disco).
const MAX_SCAN: usize = 1_000_000;
const MAX_HITS: usize = 300;
const MAX_VISITED: usize = 200_000;
const MAX_ZIP_ENTRIES: usize = 200_000;
const SEARCH_TIME: Duration = Duration::from_secs(3);
const MAX_PER_IP: usize = 8;
const FAIL_LIMIT: u32 = 20;
const FAIL_WINDOW: Duration = Duration::from_secs(60);
/// Contraseñas fallidas (con el token bueno) por ventana sumando todas las IPs: tope total de intentos
/// aunque el atacante reparta las conjeturas entre muchas direcciones.
const GLOBAL_FAIL_LIMIT: u32 = 100;
const MAX_SESSIONS: usize = 1024;
const SESSION_TTL: Duration = Duration::from_secs(24 * 3600);
const SESSION_COOKIE: &str = "fbs";
/// Con todos los huecos ocupados, una conexión que lleva al menos esto sin autenticarse cede el suyo.
const EVICT_AGE: Duration = Duration::from_secs(1);

pub struct Ctx {
    pub roots: Vec<Root>,
    pub token: String,
    pub hidden: bool,
    /// `--write`: deja subir, borrar, mover y copiar (por defecto, solo lectura)
    pub write: bool,
    pub max_upload: u64,
    pub tls: Option<crate::tls::Config>,
    /// `--user`: además del token, usuario y contraseña (HTTP Basic)
    pub auth: Option<crate::auth::Creds>,
    /// `--proxy`: detrás de un intermediario en este mismo equipo (túnel, proxy TLS). Las conexiones que
    /// llegan de 127.0.0.1/::1 no cuentan para el tope por IP y los bloqueos usan `X-Forwarded-For`.
    pub proxy: bool,
    limiter: Mutex<HashMap<IpAddr, (u32, Instant)>>,
    global_fail: Mutex<(u32, Instant)>,
    /// Sesiones ya autenticadas (cookie aleatoria → alta); ver `route`.
    sessions: Mutex<HashMap<String, Instant>>,
    per_ip: Mutex<HashMap<IpAddr, usize>>,
    /// Conexiones aún sin autenticar (id → llegada, socket para poder cortarlas).
    pending: Mutex<HashMap<u64, (Instant, TcpStream)>>,
    next_conn: AtomicU64,
}

impl Ctx {
    pub fn new(roots: Vec<Root>, token: String, hidden: bool, write: bool, max_upload: u64, tls: Option<crate::tls::Config>) -> Self {
        Ctx {
            roots,
            token,
            hidden,
            write,
            max_upload,
            tls,
            auth: None,
            proxy: false,
            limiter: Mutex::new(HashMap::new()),
            global_fail: Mutex::new((0, Instant::now())),
            sessions: Mutex::new(HashMap::new()),
            per_ip: Mutex::new(HashMap::new()),
            pending: Mutex::new(HashMap::new()),
            next_conn: AtomicU64::new(0),
        }
    }

    fn add_pending(&self, id: u64, s: &TcpStream) {
        let Ok(c) = s.try_clone() else { return };
        if let Ok(mut m) = self.pending.lock() {
            m.insert(id, (Instant::now(), c));
        }
    }

    /// La conexión ya pasó token (y contraseña): deja de ser candidata a desalojo.
    fn authed(&self, id: u64) {
        if let Ok(mut m) = self.pending.lock() {
            m.remove(&id);
        }
    }

    /// Corta la conexión sin autenticar más antigua (si tiene al menos `EVICT_AGE`). Así quien abre
    /// conexiones y no manda nada (o manda un byte cada poco) ya no puede quedarse con todos los huecos:
    /// tendría que abrir `max_conn` conexiones por segundo, y las de quien trae el enlace no se tocan.
    fn evict_stale(&self) -> bool {
        let Ok(mut m) = self.pending.lock() else { return false };
        let Some((&id, _)) = m.iter().filter(|(_, (t, _))| t.elapsed() >= EVICT_AGE).min_by_key(|(_, (t, _))| *t) else { return false };
        if let Some((_, s)) = m.remove(&id) {
            let _ = s.shutdown(std::net::Shutdown::Both);
        }
        true
    }

    fn session_ok(&self, cookie: Option<&str>) -> bool {
        let Some(c) = cookie else { return false };
        let Ok(m) = self.sessions.lock() else { return false };
        c.split(';').filter_map(|kv| kv.trim().split_once('=')).any(|(k, v)| k == SESSION_COOKIE && m.get(v).is_some_and(|t| t.elapsed() < SESSION_TTL))
    }

    fn new_session(&self) -> Option<String> {
        let mut b = [0u8; 16];
        getrandom::fill(&mut b).ok()?;
        let id: String = b.iter().map(|x| format!("{x:02x}")).collect();
        let mut m = self.sessions.lock().ok()?;
        if m.len() >= MAX_SESSIONS {
            m.retain(|_, t| t.elapsed() < SESSION_TTL);
            if m.len() >= MAX_SESSIONS {
                // la más antigua deja sitio
                if let Some(k) = m.iter().min_by_key(|(_, t)| **t).map(|(k, _)| k.clone()) {
                    m.remove(&k);
                }
            }
        }
        m.insert(id.clone(), Instant::now());
        Some(id)
    }

    fn global_blocked(&self) -> bool {
        self.global_fail.lock().is_ok_and(|g| g.0 >= GLOBAL_FAIL_LIMIT && g.1.elapsed() < FAIL_WINDOW)
    }

    fn fail_global(&self) {
        let Ok(mut g) = self.global_fail.lock() else { return };
        if g.1.elapsed() >= FAIL_WINDOW {
            *g = (0, Instant::now());
        }
        g.0 = g.0.saturating_add(1);
    }

    /// Reserva un hueco de conexión para esa IP; `false` si ya tiene demasiadas abiertas.
    fn enter(&self, ip: Option<IpAddr>) -> bool {
        let Some(ip) = self.conn_key(ip) else { return true };
        let Ok(mut m) = self.per_ip.lock() else { return true };
        let n = m.entry(ip).or_insert(0);
        if *n >= MAX_PER_IP {
            return false;
        }
        *n += 1;
        true
    }

    fn leave(&self, ip: Option<IpAddr>) {
        let Some(ip) = self.conn_key(ip) else { return };
        let Ok(mut m) = self.per_ip.lock() else { return };
        if let Some(n) = m.get_mut(&ip) {
            *n = n.saturating_sub(1);
            if *n == 0 {
                m.remove(&ip);
            }
        }
    }

    /// Clave del tope de conexiones por IP; con `--proxy`, el intermediario local no tiene tope propio
    /// (todas las conexiones del túnel llegan desde él; queda el tope global `--max-conn`).
    fn conn_key(&self, ip: Option<IpAddr>) -> Option<IpAddr> {
        ip.map(ip_key).filter(|i| !(self.proxy && i.is_loopback()))
    }

    /// IP del cliente para los bloqueos: la de la conexión, o con `--proxy` y conexión local, la última
    /// de `X-Forwarded-For` (la que puso el intermediario). De nadie más se cree esa cabecera.
    fn client_ip(&self, req: &Req, peer: Option<IpAddr>) -> Option<IpAddr> {
        if self.proxy && peer.is_some_and(|p| ip_key(p).is_loopback()) {
            if let Some(ip) = req.fwd.as_deref().and_then(|a| a.parse::<IpAddr>().ok()) {
                return Some(ip);
            }
        }
        peer
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
        IpAddr::V6(a) if a.to_ipv4_mapped().is_some() => IpAddr::V4(a.to_ipv4_mapped().unwrap()),
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
    #[cfg(windows)]
    {
        // se abre la entrada misma: si es un enlace o una unión, se rechaza. Otros puntos de análisis
        // (OneDrive, deduplicación…) hay que abrirlos normalmente para leer datos; la segunda apertura
        // debe dar el mismo archivo, si no alguien lo cambió por un enlace entre medias.
        use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
        let f = std::fs::OpenOptions::new().read(true).custom_flags(crate::win::FILE_FLAG_OPEN_REPARSE_POINT).open(p)?;
        let md = f.metadata()?;
        if md.file_type().is_symlink() {
            return Err(io::ErrorKind::NotFound.into());
        }
        if md.file_attributes() & 0x400 == 0 {
            return Ok(f);
        }
        let g = File::open(p)?;
        match (crate::win::file_id(&f), crate::win::file_id(&g)) {
            (Some(a), Some(b)) if a == b => Ok(g),
            _ => Err(io::ErrorKind::NotFound.into()),
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        File::open(p)
    }
}

pub fn ct_eq(a: &str, b: &str) -> bool {
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
    id: u64,
}
impl Drop for Guard {
    fn drop(&mut self) {
        self.ctx.authed(self.id);
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
        // lleno: se desaloja una conexión sin autenticar que lleve un rato (su hilo sale enseguida; mientras
        // tanto se pasa del tope por como mucho las desalojadas)
        let full = active.fetch_add(1, Ordering::SeqCst) >= max_conn;
        if (full && !ctx.evict_stale()) || !ctx.enter(ip) {
            // `enter` solo reserva cuando devuelve true, así que aquí basta con soltar el hueco global
            active.fetch_sub(1, Ordering::SeqCst);
            let _ = s.set_write_timeout(Some(Duration::from_secs(2)));
            let _ = http::error(&mut s, 503, false);
            continue;
        }
        let id = ctx.next_conn.fetch_add(1, Ordering::Relaxed);
        ctx.add_pending(id, &s);
        let g = Guard { active: active.clone(), ctx: ctx.clone(), ip, id };
        let ctx = ctx.clone();
        let r = std::thread::Builder::new().stack_size(512 * 1024).spawn(move || {
            let _g = g;
            handle(s, &ctx, id);
        });
        let _ = r; // si no se pudo crear el hilo, el cierre del closure suelta `g` y devuelve los huecos
    }
}

fn handle(tcp: TcpStream, ctx: &Ctx, id: u64) {
    http::clear_extra();
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
            http::set_lang(req.lang);
            let ip = ctx.client_ip(&req, ip);
            // si la respuesta se corta a medias no se cierra «limpio»: el cliente debe notar el fallo
            if route(&mut s, ctx, &req, ip, id).is_ok() {
                s.finish();
            }
        }
        Err(code) => {
            let _ = http::error(&mut s, code, false);
            s.finish();
        }
    }
}

pub fn query_get(q: &str, key: &str) -> Option<String> {
    q.split('&').find_map(|kv| {
        let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
        (k == key).then(|| jail::pct_decode(&v.replace('+', " "))).flatten()
    })
}

/// 429 de un cliente bloqueado. Es la misma respuesta acierte o no: no se ha mirado la contraseña.
fn too_many(s: &mut Stream, req: &Req) -> io::Result<()> {
    http::error_h(s, 429, req.head, &[("Retry-After", FAIL_WINDOW.as_secs().to_string())])
}

fn route(s: &mut Stream, ctx: &Ctx, req: &Req, ip: Option<IpAddr>, conn: u64) -> io::Result<()> {
    let Some(rest) = req.path.strip_prefix("/s/") else {
        return http::error(s, 404, req.head);
    };
    let (tok, tail) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, ""),
    };
    let tok_ok = ct_eq(tok, &ctx.token);
    match &ctx.auth {
        // Solo token. Es un secreto de 128 bits: el bloqueo por IP solo frena a quien prueba tokens (y su
        // registro); dejar pasar el token correcto con la IP bloqueada no da a nadie un oráculo útil, y así
        // quien comparte IP con un atacante (NAT, túnel) sigue entrando.
        None => {
            if !tok_ok {
                if ctx.blocked(ip) {
                    return too_many(s, req);
                }
                ctx.fail(ip);
                return http::error(s, 404, req.head);
            }
        }
        // Token y contraseña. La contraseña puede ser corta (8 caracteres): con la IP bloqueada (o con
        // demasiados fallos en total, de todas las IPs) NO se evalúa, la respuesta es siempre 429 y por
        // tanto no dice si se acertó. La excepción es un navegador que ya se autenticó antes desde
        // cualquier sitio: lleva la cookie de sesión aleatoria (128 bits) que se le dio entonces, que un
        // atacante no tiene ni puede adivinar. Así quien ya estaba dentro no queda fuera por los intentos de
        // otro tras su misma IP, y si su navegador pierde las credenciales recibe el 401 para volver a
        // pedirlas. La cookie no sustituye a la contraseña: solo exime del bloqueo.
        Some(c) => {
            let session = tok_ok && ctx.session_ok(req.cookie.as_deref());
            if !session && (ctx.blocked(ip) || ctx.global_blocked()) {
                return too_many(s, req);
            }
            if !tok_ok {
                ctx.fail(ip);
                return http::error(s, 404, req.head);
            }
            if !c.check(req.auth.as_deref()) {
                // los intentos fallidos cuentan para el bloqueo por IP y para el total
                if req.auth.is_some() {
                    ctx.fail(ip);
                    ctx.fail_global();
                }
                let h = [("WWW-Authenticate", "Basic realm=\"filebrowser-server\", charset=\"UTF-8\"".to_string())];
                return http::error_h(s, 401, req.head, &h);
            }
            if !session && let Some(id) = ctx.new_session() {
                let secure = if ctx.tls.is_some() { "; Secure" } else { "" };
                http::add_extra("Set-Cookie", format!("{SESSION_COOKIE}={id}; Path=/s/{tok}/; HttpOnly; SameSite=Lax{secure}"));
            }
        }
    }
    ctx.authed(conn);
    if tail.is_empty() {
        return http::redirect(s, &format!("/s/{tok}/"), req.head);
    }
    if matches!(req.method, Method::Put | Method::Post) {
        return crate::write::dispatch(s, ctx, req, tail);
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
        return http::send(s, 200, "text/html; charset=utf-8", page::roots_page(req.lang, tok, &names, webui::available(), ctx.write).as_bytes(), req.head, &[]);
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
            return send_zip(s, ctx, req, root, &comps, &canon);
        }
        if let Some(q) = query_get(&req.query, "q").filter(|q| !q.trim().is_empty()) {
            return send_search(s, ctx, req, root, &comps, &canon, q.trim());
        }
        let sort = query_get(&req.query, "sort").unwrap_or_default();
        let desc = query_get(&req.query, "o").as_deref() == Some("d");
        let (rows, truncated) = list_dir(ctx, root, &canon, &sort, desc);
        let html = page::dir_page(req.lang, tok, &comps, &rows, &sort, desc, truncated, ctx.write && !webui::available());
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
pub fn virt_path(p: &str) -> Option<Vec<String>> {
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
        "info" => http::send(s, 200, json, format!("{{\"write\":{},\"max_upload\":{}}}", ctx.write, ctx.max_upload).as_bytes(), req.head, &[]),
        "list" => {
            let Some(comps) = virt_path(&query_get(&req.query, "p").unwrap_or_default()) else {
                return http::error(s, 400, req.head);
            };
            let (mut rows, truncated);
            if comps.is_empty() {
                rows = ctx.roots.iter().map(|r| Row { name: r.name.clone(), is_dir: true, size: 0, mtime: 0 }).collect::<Vec<_>>();
                truncated = false;
                sort_rows(&mut rows, "name", false);
            } else {
                let Some((root, canon)) = jail::resolve(&ctx.roots, &comps, ctx.hidden) else { return http::error(s, 404, req.head) };
                if !std::fs::metadata(&canon).is_ok_and(|m| m.is_dir()) {
                    return http::error(s, 404, req.head);
                }
                (rows, truncated) = list_dir(ctx, root, &canon, "name", false);
            }
            http::send(s, 200, json, webui::list_json(&comps, &rows, truncated).as_bytes(), req.head, &[])
        }
        _ => http::error(s, 404, req.head),
    }
}

/// Entradas visibles de `dir`, ya ordenadas, como mucho `MAX_LIST` (las primeras en ese orden).
fn list_dir(ctx: &Ctx, root: &Root, dir: &Path, sort: &str, desc: bool) -> (Vec<Row>, bool) {
    let mut rows = Vec::new();
    let Ok(rd) = std::fs::read_dir(dir) else { return (rows, false) };
    let mut truncated = false;
    for (seen, e) in rd.flatten().enumerate() {
        if seen >= MAX_SCAN {
            truncated = true;
            break;
        }
        // se ordena y recorta a trozos: la memoria queda en 2×MAX_LIST filas por grande que sea la carpeta
        if rows.len() >= 2 * MAX_LIST {
            sort_rows(&mut rows, sort, desc);
            rows.truncate(MAX_LIST);
            truncated = true;
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
    sort_rows(&mut rows, sort, desc);
    if rows.len() > MAX_LIST {
        rows.truncate(MAX_LIST);
        truncated = true;
    }
    (rows, truncated)
}

/// Destino de un enlace simbólico si queda dentro de la raíz (y no es oculto): (ruta canónica, metadatos).
fn link_inside(ctx: &Ctx, root: &Root, p: &Path) -> Option<(std::path::PathBuf, std::fs::Metadata)> {
    let canon = std::fs::canonicalize(p).ok()?;
    if !jail::inside(root, &canon, ctx.hidden) {
        return None;
    }
    let md = std::fs::metadata(&canon).ok()?;
    (md.is_dir() || md.is_file()).then_some((canon, md))
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

fn send_search(s: &mut Stream, ctx: &Ctx, req: &Req, root: &Root, comps: &[String], dir: &Path, q: &str) -> io::Result<()> {
    let needle = q.to_lowercase();
    let t0 = Instant::now();
    let mut hits: Vec<Hit> = Vec::new();
    let mut truncated = false;
    let missed = walk::walk(dir, ctx.hidden, MAX_VISITED, &mut |it| {
        if t0.elapsed() > SEARCH_TIME {
            truncated = true;
            return false;
        }
        if it.rel.last().is_some_and(|n| n.to_lowercase().contains(&needle)) {
            if hits.len() >= MAX_HITS {
                truncated = true;
                return false;
            }
            // un enlace sale si su destino está dentro de lo compartido, como en el listado (sin entrar en él)
            let (is_dir, size) = if it.link {
                match link_inside(ctx, root, &it.path) {
                    Some((_, md)) => (md.is_dir(), if md.is_file() { md.len() } else { 0 }),
                    None => return true,
                }
            } else {
                (it.is_dir, it.size)
            };
            hits.push(Hit { rel: it.rel, is_dir, size });
        }
        true
    });
    let html = page::search_page(req.lang, &ctx.token, comps, q, &hits, truncated || missed.cut, missed.unreadable_count);
    http::send(s, 200, "text/html; charset=utf-8", html.as_bytes(), req.head, &[])
}

fn ascii_name(n: &str) -> String {
    n.chars().map(|c| if c.is_ascii_graphic() && !matches!(c, '"' | '\\' | ';' | '%') || c == ' ' { c } else { '_' }).collect()
}

fn disposition(kind: &str, name: &str) -> String {
    format!("{kind}; filename=\"{}\"; filename*=UTF-8''{}", ascii_name(name), jail::pct_encode(name))
}

fn send_zip(s: &mut Stream, ctx: &Ctx, req: &Req, root: &Root, comps: &[String], dir: &Path) -> io::Result<()> {
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
    let l = req.lang;
    let mut z = zipstream::Zip::new(BufWriter::with_capacity(64 * 1024, &mut *s));
    z.add_dir(&top, 0)?;
    let mut err: Option<io::Error> = None;
    // lo que no entra se apunta en un archivo de texto dentro del propio ZIP
    let note_name = l.tr("OMITIDOS.txt", "SKIPPED.txt");
    let mut skipped: Vec<String> = Vec::new();
    let mut skipped_n = 0usize;
    let mut clash = false;
    let mut full = false;
    let mut note = |v: &mut Vec<String>, rel: &[String], why: &str| {
        skipped_n += 1;
        if v.len() < walk::MAX_NOTED {
            v.push(format!("{}  ({why})", rel.join("/")));
        }
    };
    let missed = walk::walk(dir, ctx.hidden, MAX_VISITED, &mut |it| {
        if z.count() >= MAX_ZIP_ENTRIES {
            full = true;
            return false;
        }
        if it.rel.len() == 1 && it.rel[0].eq_ignore_ascii_case(note_name) {
            clash = true;
        }
        let name = format!("{top}/{}", it.rel.join("/"));
        let r = if it.is_dir {
            z.add_dir(&name, it.mtime)
        } else {
            // un enlace a un archivo de dentro se incluye con su contenido; a una carpeta no se entra (ciclos)
            let target = if it.link {
                match link_inside(ctx, root, &it.path) {
                    Some((c, md)) if md.is_file() => Some(c),
                    Some(_) => {
                        note(&mut skipped, &it.rel, l.tr("enlace a una carpeta: no se sigue", "link to a folder: not followed"));
                        None
                    }
                    None => {
                        note(&mut skipped, &it.rel, l.tr("enlace roto o que sale de lo compartido", "broken link, or it points outside the share"));
                        None
                    }
                }
            } else {
                Some(it.path.clone())
            };
            match target.map(|p| open_nofollow(&p)) {
                None => Ok(()),
                // un fichero ilegible se omite (y se apunta); un fallo de red corta el ZIP
                Some(Err(_)) => {
                    note(&mut skipped, &it.rel, l.tr("no se pudo leer", "could not be read"));
                    Ok(())
                }
                Some(Ok(mut f)) => match f.metadata() {
                    Ok(m) if m.is_file() => z.add_file(&name, it.mtime, &mut f, m.len()),
                    _ => {
                        note(&mut skipped, &it.rel, l.tr("no se pudo leer", "could not be read"));
                        Ok(())
                    }
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
    if let Some(e) = err {
        return Err(e); // se cierra la conexión sin directorio central: el cliente ve el ZIP incompleto
    }
    let mut text = String::new();
    if full || missed.cut {
        text.push_str(l.tr(
            "Este ZIP está incompleto: la carpeta tiene demasiadas entradas o es demasiado profunda.\n\n",
            "This ZIP is incomplete: the folder has too many entries or is too deep.\n\n",
        ));
    }
    if skipped_n + missed.unreadable_count > 0 {
        text.push_str(l.tr("No se incluyó:\n", "Not included:\n"));
        for d in &missed.unreadable {
            text.push_str(&format!("{d}/  ({})\n", l.tr("carpeta que no se pudo leer", "folder could not be read")));
        }
        for line in &skipped {
            text.push_str(line);
            text.push('\n');
        }
        let shown = skipped.len() + missed.unreadable.len();
        if skipped_n + missed.unreadable_count > shown {
            text.push_str(&format!("… (+{})\n", skipped_n + missed.unreadable_count - shown));
        }
    }
    if !text.is_empty() {
        let n = if clash { format!("{top}/_{note_name}") } else { format!("{top}/{note_name}") };
        let b = text.into_bytes();
        z.add_file(&n, timefmt::unix(std::time::SystemTime::now()), &mut &b[..], b.len() as u64)?;
    }
    z.finish()
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
    fn tope_global_de_fallos_y_sesiones() {
        let c = Ctx::new(Vec::new(), "t".into(), false, false, 0, None);
        for _ in 0..GLOBAL_FAIL_LIMIT - 1 {
            c.fail_global();
        }
        assert!(!c.global_blocked());
        c.fail_global();
        assert!(c.global_blocked());
        let id = c.new_session().unwrap();
        assert!(c.session_ok(Some(&format!("a=b; {SESSION_COOKIE}={id}"))));
        assert!(!c.session_ok(Some(&format!("{SESSION_COOKIE}=x{id}"))));
        assert!(!c.session_ok(None));
        // la tabla de sesiones no crece sin límite
        for _ in 0..MAX_SESSIONS + 10 {
            c.new_session();
        }
        assert!(c.sessions.lock().unwrap().len() <= MAX_SESSIONS);
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
