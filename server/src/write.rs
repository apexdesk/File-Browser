//! Escritura (solo con `--write`): subir/sobrescribir un archivo (`PUT /s/<token>/ruta`) y las
//! operaciones sueltas (`POST /s/<token>/api/op?op=…&p=…&to=…`): crear carpeta, borrar, mover y copiar.
//!
//! Las mismas reglas que la lectura: toda ruta pasa por `jail` (componentes validados, la carpeta
//! contenedora canonicalizada y dentro de la raíz, nada oculto) y nunca se toca una raíz compartida.
//! Además, cada petición debe llevar `X-FB: 1` y un `Origin` con el mismo host[:puerto] que el `Host`:
//! una página de otro sitio no puede mandarlas desde el navegador de quien tenga el enlace abierto.
//!
//! Los errores van en JSON (`{"ok":false,"error":"<motivo>"}`) para que el cliente diga qué pasó.

use crate::http::{self, Req};
use crate::jail::{self, Root};
use crate::server::{Ctx, query_get, virt_path};
use crate::stream::Stream;
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Plazo para cada lectura del cuerpo: un envío que se queda parado se corta.
const BODY_STALL: Duration = Duration::from_secs(30);

/// Fallo de una escritura: código HTTP y motivo para el cliente (vacío: el genérico del código).
#[derive(Debug, PartialEq, Eq)]
pub struct Fail(pub u16, pub &'static str);

impl From<u16> for Fail {
    fn from(c: u16) -> Self {
        Fail(c, "")
    }
}

/// Motivo por defecto de cada código.
fn why_of(code: u16) -> &'static str {
    match code {
        400 => "bad_request",
        403 => "forbidden",
        404 => "not_found",
        405 => "read_only",
        409 | 412 => "exists",
        411 => "length_required",
        413 => "too_large",
        507 => "full",
        _ => "error",
    }
}

/// host[:puerto] en minúsculas y sin el puerto por defecto.
fn host_key(h: &str) -> String {
    let h = h.to_ascii_lowercase();
    let h = h.strip_suffix(":80").or_else(|| h.strip_suffix(":443")).unwrap_or(&h);
    h.to_string()
}

/// `Origin` con el mismo host[:puerto] que `Host`, con cualquiera de los dos esquemas: detrás de un
/// proxy que termina TLS (túnel) el navegador habla https y aquí llega http. El token va en la ruta,
/// así que el esquema no añade nada; lo que importa es que la página sea de este mismo sitio.
fn csrf_ok(req: &Req) -> bool {
    let (Some(origin), Some(host)) = (&req.origin, &req.host) else { return false };
    let o = origin.strip_prefix("https://").or_else(|| origin.strip_prefix("http://")).unwrap_or("");
    req.xfb && !o.is_empty() && !o.contains('/') && !host.is_empty() && host_key(o) == host_key(host)
}

fn status_of(e: &io::Error) -> u16 {
    match e.kind() {
        io::ErrorKind::NotFound => 404,
        io::ErrorKind::AlreadyExists | io::ErrorKind::DirectoryNotEmpty | io::ErrorKind::IsADirectory | io::ErrorKind::NotADirectory => 409,
        io::ErrorKind::PermissionDenied => 403,
        io::ErrorKind::InvalidInput | io::ErrorKind::InvalidFilename => 400,
        io::ErrorKind::StorageFull | io::ErrorKind::QuotaExceeded => 507,
        _ => 500,
    }
}

const TMP_PREFIX: &str = ".fbtmp-";

/// ¿Es un nombre temporal de este servidor? Exactamente `.fbtmp-` + 16 cifras hexadecimales (`rand_hex`):
/// así el barrido no toca nunca un archivo del usuario que solo empiece igual.
fn is_own_temp(name: &str) -> bool {
    name.strip_prefix(TMP_PREFIX).is_some_and(|h| h.len() == 16 && h.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')))
}

/// Temporales que este proceso tiene a medias (subidas y copias en curso).
static LIVE_TEMPS: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

/// Nombre temporal nuevo en `dir`, apuntado para poder borrarlo si el proceso termina a medias.
fn new_temp(dir: &Path) -> PathBuf {
    let p = dir.join(format!("{TMP_PREFIX}{}", rand_hex()));
    if let Ok(mut v) = LIVE_TEMPS.lock() {
        v.push(p.clone());
    }
    p
}

fn forget_temp(p: &Path) {
    if let Ok(mut v) = LIVE_TEMPS.lock() {
        v.retain(|x| x != p);
    }
}

/// Al caducar (`--ttl`): borra los temporales de las subidas y copias aún en curso antes de salir.
pub fn remove_live_temps() {
    let Ok(mut v) = LIVE_TEMPS.lock() else { return };
    for p in v.drain(..) {
        let _ = fs::remove_file(p);
    }
}

/// Al arrancar con `--write`: borra los temporales propios (`is_own_temp`) de más de una hora que dejó
/// un corte (kill, apagón). Una subida viva toca el suyo al menos cada `BODY_STALL`, así que uno de una
/// hora no es de nadie, ni siquiera de otra instancia que comparta la misma carpeta.
pub fn sweep_temp(roots: &[Root]) {
    let limit = Duration::from_secs(3600);
    let mut budget = 50_000usize;
    let mut stack: Vec<PathBuf> = roots.iter().map(|r| r.path.clone()).collect();
    while let Some(d) = stack.pop() {
        let Ok(rd) = fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            if budget == 0 {
                return;
            }
            budget -= 1;
            let Ok(t) = e.file_type() else { continue };
            if t.is_dir() {
                stack.push(e.path());
            } else if t.is_file()
                && e.file_name().to_str().is_some_and(is_own_temp)
                && e.metadata().and_then(|m| m.modified()).is_ok_and(|m| m.elapsed().is_ok_and(|a| a > limit))
            {
                let _ = fs::remove_file(e.path());
            }
        }
    }
}

fn rand_hex() -> String {
    let mut b = [0u8; 8];
    let _ = getrandom::fill(&mut b);
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// (raíz, carpeta contenedora canónica, nombre) de la ruta virtual `comps`. Las raíces mismas no se tocan.
fn parent_and_name<'a>(ctx: &'a Ctx, comps: &[String]) -> Result<(&'a Root, PathBuf, String), Fail> {
    let (name, parent) = comps.split_last().ok_or(Fail(400, ""))?;
    if parent.is_empty() {
        return Err(Fail(403, "root"));
    }
    let (root, dir) = jail::resolve(&ctx.roots, parent, ctx.hidden).ok_or(Fail(404, ""))?;
    if !dir.is_dir() {
        return Err(404.into());
    }
    if !ctx.hidden && name.starts_with('.') {
        return Err(Fail(403, "hidden_name"));
    }
    let target = dir.join(name);
    // Windows: un nombre corto 8.3 (`GIT~1`, `ENV~1`) es otro nombre de una entrada existente y se
    // saltaría el filtro de ocultos de arriba. Si existe y su nombre largo real es otro, se rechaza.
    #[cfg(windows)]
    if name.contains('~') && fs::symlink_metadata(&target).is_ok() {
        match crate::win::long_name(&target) {
            Some(long) if long.to_lowercase() == name.to_lowercase() => {}
            _ => return Err(Fail(403, "hidden_name")),
        }
    }
    // ni una raíz ni una carpeta que contenga otra raíz compartida (raíces anidadas). Además de comparar
    // rutas, se compara la identidad de archivo: en un sistema que no distingue mayúsculas (Windows,
    // macOS) o con un nombre corto, `X/SUB` es la misma carpeta que la raíz `x/sub`.
    if ctx.roots.iter().any(|r| r.path.starts_with(&target)) {
        return Err(Fail(403, "root"));
    }
    if let Some(id) = entry_id(&target) {
        if ctx.roots.iter().any(|r| r.path.ancestors().any(|a| entry_id(a) == Some(id))) {
            return Err(Fail(403, "root"));
        }
    }
    Ok((root, dir, name.clone()))
}

/// Identidad de la entrada (sin seguir un enlace en el último componente): igual ⇔ mismo archivo.
fn entry_id(p: &Path) -> Option<(u64, u128)> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        fs::symlink_metadata(p).ok().map(|m| (m.dev(), u128::from(m.ino())))
    }
    #[cfg(windows)]
    {
        crate::win::entry_id(p)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = p;
        None
    }
}

fn ok(s: &mut Stream) -> io::Result<()> {
    http::send(s, 200, "application/json; charset=utf-8", b"{\"ok\":true}", false, &[])
}

fn fail(s: &mut Stream, f: &Fail) -> io::Result<()> {
    let why = if f.1.is_empty() { why_of(f.0) } else { f.1 };
    let body = format!("{{\"ok\":false,\"error\":\"{why}\"}}");
    let extra = if f.0 == 405 { vec![("Allow", "GET, HEAD, PUT, POST".to_string())] } else { Vec::new() };
    http::send(s, f.0, "application/json; charset=utf-8", body.as_bytes(), false, &extra)
}

/// Tras responder un error a un PUT sin haber leído su cuerpo: se lee y descarta un poco (con tope de
/// bytes y de tiempo) para que el navegador vea la respuesta en vez de un corte a mitad de envío.
fn drain(s: &mut Stream, req: &Req) {
    const CAP: u64 = 16 << 20;
    const TIME: Duration = Duration::from_secs(10);
    let mut left = req.content_length.saturating_sub(req.leftover.len() as u64).min(CAP);
    let t0 = Instant::now();
    let mut buf = [0u8; 16 * 1024];
    while left > 0 && t0.elapsed() < TIME {
        s.set_deadline(Some(Duration::from_secs(5)));
        let want = buf.len().min(left as usize);
        match s.read(&mut buf[..want]) {
            Ok(0) | Err(_) => break,
            Ok(n) => left -= n as u64,
        }
    }
    s.set_deadline(None);
}

/// Entrada de `route` para PUT y POST.
pub fn dispatch(s: &mut Stream, ctx: &Ctx, req: &Req, tail: &str) -> io::Result<()> {
    let put = req.method == http::Method::Put;
    let mut body_read = false;
    let r = if !ctx.write {
        Err(Fail(405, "read_only"))
    } else if !csrf_ok(req) {
        Err(Fail(403, "origin"))
    } else if put {
        put_file(s, ctx, req, tail, &mut body_read)
    } else if tail == "/api/op" {
        op(ctx, req)
    } else {
        Err(404.into())
    };
    match r {
        Ok(()) => ok(s),
        Err(f) => {
            fail(s, &f)?;
            if put && !body_read {
                drain(s, req);
            }
            Ok(())
        }
    }
}

/// PUT: con `If-None-Match: *` solo crea (412 si ya existe); sin ella, reemplaza.
fn put_file(s: &mut Stream, ctx: &Ctx, req: &Req, tail: &str, body_read: &mut bool) -> Result<(), Fail> {
    let (comps, slash) = jail::split_url(tail).ok_or(Fail(404, ""))?;
    if slash {
        return Err(404.into());
    }
    // sin longitud declarada no se sabe si el cuerpo viene vacío o el cliente falló: no se vacía el archivo
    if !req.has_length {
        return Err(411.into());
    }
    let (_, dir, name) = parent_and_name(ctx, &comps)?;
    let target = dir.join(&name);
    let old = fs::symlink_metadata(&target).ok();
    if old.as_ref().is_some_and(|m| m.is_dir()) {
        return Err(Fail(409, "is_dir"));
    }
    let only_new = req.inm.as_deref().map(str::trim) == Some("*");
    if only_new && old.is_some() {
        return Err(Fail(412, "exists"));
    }
    if req.content_length > ctx.max_upload {
        return Err(413.into());
    }
    let tmp = new_temp(&dir);
    let r = put_body(s, req, &tmp, old.as_ref(), &target, only_new, body_read);
    forget_temp(&tmp);
    r?;
    eprintln!("[escritura] subido {} ({} B)", comps.join("/"), req.content_length);
    Ok(())
}

fn put_body(s: &mut Stream, req: &Req, tmp: &Path, old: Option<&fs::Metadata>, target: &Path, only_new: bool, body_read: &mut bool) -> Result<(), Fail> {
    let mut f = OpenOptions::new().write(true).create_new(true).open(tmp).map_err(|e| status_of(&e))?;
    *body_read = true;
    let r = (|| -> io::Result<()> {
        let mut left = req.content_length;
        let first = req.leftover.len().min(left as usize);
        f.write_all(&req.leftover[..first])?;
        left -= first as u64;
        let mut buf = vec![0u8; 64 * 1024];
        while left > 0 {
            s.set_deadline(Some(BODY_STALL));
            let want = buf.len().min(left as usize);
            let n = s.read(&mut buf[..want])?;
            if n == 0 {
                return Err(io::ErrorKind::UnexpectedEof.into());
            }
            f.write_all(&buf[..n])?;
            left -= n as u64;
        }
        s.set_deadline(None);
        f.flush()?;
        Ok(())
    })();
    drop(f);
    if let Err(e) = r {
        let _ = fs::remove_file(tmp);
        return Err(if e.kind() == io::ErrorKind::UnexpectedEof || e.kind() == io::ErrorKind::TimedOut { 400 } else { status_of(&e) }.into());
    }
    // al reemplazar un archivo se conservan sus permisos (p. ej. el bit ejecutable)
    if let Some(m) = old.filter(|m| m.is_file()) {
        let _ = fs::set_permissions(tmp, m.permissions());
    }
    // solo crear: tampoco se pisa lo que haya aparecido mientras llegaba el cuerpo
    let r = if only_new { rename_noreplace(tmp, target, false) } else { fs::rename(tmp, target) };
    if let Err(e) = r {
        let _ = fs::remove_file(tmp);
        return Err(if e.kind() == io::ErrorKind::AlreadyExists { Fail(412, "exists") } else { status_of(&e).into() });
    }
    Ok(())
}

fn op(ctx: &Ctx, req: &Req) -> Result<(), Fail> {
    let bad = || Fail(400, "");
    let name = query_get(&req.query, "op").ok_or_else(bad)?;
    let p = virt_path(&query_get(&req.query, "p").ok_or_else(bad)?).ok_or_else(bad)?;
    let to = match query_get(&req.query, "to") {
        Some(t) => Some(virt_path(&t).ok_or_else(bad)?),
        None => None,
    };
    let ioe = |r: io::Result<()>| r.map_err(|e| Fail::from(status_of(&e)));
    let r = match (name.as_str(), &to) {
        ("mkdir", None) => {
            let (_, dir, n) = parent_and_name(ctx, &p)?;
            ioe(fs::create_dir(dir.join(n)))
        }
        ("mkdirs", None) => ioe(mkdirs(ctx, &p)),
        ("rm", None) => {
            let (_, dir, n) = parent_and_name(ctx, &p)?;
            let t = dir.join(n);
            if fs::symlink_metadata(&t).map_err(|e| status_of(&e))?.is_dir() {
                return Err(Fail(409, "is_dir"));
            }
            ioe(fs::remove_file(t))
        }
        ("rmdir", None) => {
            let (_, dir, n) = parent_and_name(ctx, &p)?;
            fs::remove_dir(dir.join(n)).map_err(|e| match status_of(&e) {
                409 => Fail(409, "not_empty"),
                c => c.into(),
            })
        }
        ("rmrf", None) => {
            let (_, dir, n) = parent_and_name(ctx, &p)?;
            let t = dir.join(n);
            let md = fs::symlink_metadata(&t).map_err(|e| status_of(&e))?;
            if md.is_dir() {
                // lo oculto no se ve ni se lista: tampoco se borra de rebote; y nada demasiado profundo
                check_tree(&t, !ctx.hidden)?;
                ioe(remove_tree(&t))
            } else {
                ioe(fs::remove_file(t))
            }
        }
        ("mv", Some(to)) => {
            let (r1, d1, n1) = parent_and_name(ctx, &p)?;
            let (r2, d2, n2) = parent_and_name(ctx, to)?;
            if r1.name != r2.name {
                return Err(Fail(400, "other_root"));
            }
            let (src, dst) = (d1.join(n1), d2.join(n2));
            let md = fs::symlink_metadata(&src).map_err(|e| status_of(&e))?;
            if md.is_dir() && !ctx.hidden {
                check_tree(&src, true)?;
            }
            // nunca se pisa nada: solo un cambio de mayúsculas puede «existir» ya en el destino, y solo si
            // es el mismo archivo
            let same = match fs::symlink_metadata(&dst) {
                Ok(_) => {
                    if entry_id(&src).is_none() || entry_id(&src) != entry_id(&dst) {
                        return Err(Fail(409, "exists"));
                    }
                    true
                }
                Err(_) => false,
            };
            ioe(rename_noreplace(&src, &dst, same))
        }
        ("cp", Some(to)) => copy(ctx, &p, to),
        _ => return Err(bad()),
    };
    match r {
        Ok(()) => {
            eprintln!("[escritura] {name} {}{}", p.join("/"), to.map(|t| format!(" → {}", t.join("/"))).unwrap_or_default());
            Ok(())
        }
        Err(c) => Err(c),
    }
}

/// Renombra sin reemplazar un destino que aparezca entre la comprobación y el cambio. `same`: el destino
/// ya existe y es el mismo archivo (cambio de mayúsculas en un sistema que no las distingue).
fn rename_noreplace(src: &Path, dst: &Path, same: bool) -> io::Result<()> {
    #[cfg(windows)]
    {
        // MoveFileEx sin MOVEFILE_REPLACE_EXISTING: falla si el destino existe (salvo cambio de mayúsculas)
        let _ = same;
        crate::win::rename_noreplace(src, dst)
    }
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::ffi::OsStrExt;
        if !same {
            let (Ok(a), Ok(b)) = (std::ffi::CString::new(src.as_os_str().as_bytes()), std::ffi::CString::new(dst.as_os_str().as_bytes())) else {
                return Err(io::ErrorKind::InvalidInput.into());
            };
            // SAFETY: cadenas C válidas durante la llamada; AT_FDCWD con rutas absolutas
            let r = unsafe { libc::syscall(libc::SYS_renameat2, libc::AT_FDCWD, a.as_ptr(), libc::AT_FDCWD, b.as_ptr(), libc::RENAME_NOREPLACE) };
            if r == 0 {
                return Ok(());
            }
            let e = io::Error::last_os_error();
            // sistema de archivos o núcleo sin RENAME_NOREPLACE: queda la comprobación previa
            if !matches!(e.raw_os_error(), Some(libc::EINVAL | libc::ENOSYS | libc::EOPNOTSUPP)) {
                return Err(e);
            }
        }
        fs::rename(src, dst)
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        let _ = same;
        fs::rename(src, dst)
    }
}

/// Tope de trabajo de `check_tree`: pasado, se niega (409 «too_big») en vez de arriesgarse.
const TREE_MAX_ENTRIES: usize = 1_000_000;
/// Profundidad máxima que se deja borrar o mover entera. Por debajo, la recursión de `remove_dir_all`
/// cabe de sobra en la pila de `remove_tree`.
const TREE_MAX_DEPTH: usize = 1024;

/// Antes de `rmrf`/`mv` de una carpeta: recorre el árbol sin recursión y sin seguir enlaces. Niega (409)
/// si hay algo oculto (con `hidden`; motivo «hidden»), si es más profundo o más grande que los topes
/// («too_big»), o si una carpeta no se puede leer («unreadable»): ante la duda, no.
fn check_tree(dir: &Path, hidden: bool) -> Result<(), Fail> {
    let unreadable = || Fail(409, "unreadable");
    let mut budget = TREE_MAX_ENTRIES;
    let mut stack: Vec<(PathBuf, usize)> = vec![(dir.to_path_buf(), 0)];
    while let Some((d, depth)) = stack.pop() {
        let rd = fs::read_dir(&d).map_err(|_| unreadable())?;
        for e in rd {
            let e = e.map_err(|_| unreadable())?;
            budget = budget.checked_sub(1).ok_or(Fail(409, "too_big"))?;
            if hidden && e.file_name().to_string_lossy().starts_with('.') {
                return Err(Fail(409, "hidden"));
            }
            if e.file_type().map_err(|_| unreadable())?.is_dir() {
                if depth + 1 >= TREE_MAX_DEPTH {
                    return Err(Fail(409, "too_big"));
                }
                stack.push((e.path(), depth + 1));
            }
        }
    }
    Ok(())
}

/// `remove_dir_all` en un hilo con pila holgada: su recursión no cabe en los 512 KiB de un manejador
/// con árboles de cientos de niveles (y un árbol puede crecer entre `check_tree` y el borrado).
fn remove_tree(p: &Path) -> io::Result<()> {
    std::thread::scope(|sc| {
        std::thread::Builder::new()
            .stack_size(64 << 20)
            .spawn_scoped(sc, || fs::remove_dir_all(p))
            .map_err(|_| io::Error::from(io::ErrorKind::OutOfMemory))?
            .join()
            .unwrap_or_else(|_| Err(io::ErrorKind::Other.into()))
    })
}

fn mkdirs(ctx: &Ctx, comps: &[String]) -> io::Result<()> {
    let bad = |c: u16| io::Error::from(match c {
        403 => io::ErrorKind::PermissionDenied,
        409 => io::ErrorKind::AlreadyExists,
        _ => io::ErrorKind::NotFound,
    });
    let mut n = comps.len();
    let (_, mut dir) = loop {
        if n == 0 {
            return Err(bad(403));
        }
        if let Some(found) = jail::resolve(&ctx.roots, &comps[..n], ctx.hidden) {
            break found;
        }
        n -= 1;
    };
    if !dir.is_dir() {
        return Err(bad(409));
    }
    for c in &comps[n..] {
        if !ctx.hidden && c.starts_with('.') {
            return Err(bad(403));
        }
        dir.push(c);
        fs::create_dir(&dir)?;
    }
    Ok(())
}

/// Copia un archivo sin pisar nada: si el destino existe (o aparece mientras se copia), 409.
fn copy(ctx: &Ctx, from: &[String], to: &[String]) -> Result<(), Fail> {
    let (r1, src) = jail::resolve(&ctx.roots, from, ctx.hidden).ok_or(Fail(404, ""))?;
    let md = fs::metadata(&src).map_err(|e| status_of(&e))?;
    if !md.is_file() {
        return Err(Fail(400, "not_file"));
    }
    if md.len() > ctx.max_upload {
        return Err(413.into());
    }
    let (r2, d2, n2) = parent_and_name(ctx, to)?;
    if r1.name != r2.name {
        return Err(Fail(400, "other_root"));
    }
    let dst = d2.join(n2);
    if fs::symlink_metadata(&dst).is_ok() {
        return Err(Fail(409, "exists"));
    }
    let tmp = new_temp(&d2);
    let r = match fs::copy(&src, &tmp) {
        Err(e) => Err(e),
        Ok(_) => rename_noreplace(&tmp, &dst, false),
    };
    if r.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    forget_temp(&tmp);
    r.map_err(|e| Fail::from(status_of(&e)))
}

#[cfg(test)]
mod tests {
    use crate::jail::Root;
    use crate::server::{serve, Ctx};
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::path::PathBuf;
    use std::sync::Arc;

    struct Srv {
        port: u16,
        dir: PathBuf,
    }

    fn start(write: bool, max_upload: u64) -> Srv {
        let dir = std::env::temp_dir().join(format!("fbs_w_{}_{}", std::process::id(), crate::write::rand_hex()));
        std::fs::create_dir_all(dir.join("raiz/sub")).unwrap();
        std::fs::create_dir_all(dir.join("fuera")).unwrap();
        std::fs::write(dir.join("fuera/secreto.txt"), "no").unwrap();
        std::fs::write(dir.join("raiz/a.txt"), "hola").unwrap();
        std::fs::write(dir.join("raiz/.oculto"), "x").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(dir.join("fuera"), dir.join("raiz/enlace")).unwrap();
        let root = Root { name: "raiz".into(), path: std::fs::canonicalize(dir.join("raiz")).unwrap() };
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let ctx = Ctx::new(vec![root], "tok".into(), false, write, max_upload, None);
        std::thread::spawn(move || serve(l, Arc::new(ctx), 16));
        Srv { port, dir }
    }

    /// Petición cruda; devuelve (código, cuerpo).
    fn req(s: &Srv, method: &str, path: &str, headers: &[(&str, String)], body: &[u8]) -> (u16, String) {
        let mut t = TcpStream::connect(("127.0.0.1", s.port)).unwrap();
        let mut h = format!("{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\n", s.port);
        for (k, v) in headers {
            h.push_str(&format!("{k}: {v}\r\n"));
        }
        if method == "PUT" || method == "POST" {
            h.push_str(&format!("Content-Length: {}\r\n", body.len()));
        }
        h.push_str("\r\n");
        t.write_all(h.as_bytes()).unwrap();
        t.write_all(body).unwrap();
        let mut out = Vec::new();
        let _ = t.read_to_end(&mut out);
        let txt = String::from_utf8_lossy(&out).into_owned();
        let code = txt.split(' ').nth(1).and_then(|c| c.parse().ok()).unwrap_or(0);
        let body = txt.split("\r\n\r\n").nth(1).unwrap_or("").to_string();
        (code, body)
    }

    fn good(s: &Srv) -> Vec<(&'static str, String)> {
        vec![("X-FB", "1".into()), ("Origin", format!("http://127.0.0.1:{}", s.port))]
    }

    fn put(s: &Srv, p: &str, body: &[u8]) -> u16 {
        req(s, "PUT", &format!("/s/tok/raiz/{p}"), &good(s), body).0
    }

    fn op(s: &Srv, q: &str) -> u16 {
        req(s, "POST", &format!("/s/tok/api/op?{q}"), &good(s), b"").0
    }

    #[test]
    fn solo_lectura_por_defecto() {
        let s = start(false, 1 << 20);
        assert_eq!(put(&s, "n.txt", b"x"), 405);
        assert_eq!(op(&s, "op=mkdir&p=raiz/d"), 405);
        assert!(!s.dir.join("raiz/n.txt").exists());
        let _ = std::fs::remove_dir_all(&s.dir);
    }

    #[test]
    fn exige_cabeceras_de_origen() {
        let s = start(true, 1 << 20);
        assert_eq!(req(&s, "PUT", "/s/tok/raiz/n.txt", &[], b"x").0, 403);
        assert_eq!(req(&s, "PUT", "/s/tok/raiz/n.txt", &[("X-FB", "1".into())], b"x").0, 403);
        let ajeno = vec![("X-FB", "1".into()), ("Origin", "http://evil.example".into())];
        assert_eq!(req(&s, "PUT", "/s/tok/raiz/n.txt", &ajeno, b"x").0, 403);
        assert!(!s.dir.join("raiz/n.txt").exists());
        let _ = std::fs::remove_dir_all(&s.dir);
    }

    #[test]
    fn subir_crear_mover_copiar_borrar() {
        let s = start(true, 1 << 20);
        assert_eq!(put(&s, "n.txt", b"contenido"), 200);
        assert_eq!(std::fs::read_to_string(s.dir.join("raiz/n.txt")).unwrap(), "contenido");
        // un archivo existente se reemplaza de golpe (sin temporales); una carpeta no
        assert_eq!(put(&s, "n.txt", b"otro"), 200);
        assert_eq!(std::fs::read_to_string(s.dir.join("raiz/n.txt")).unwrap(), "otro");
        assert_eq!(put(&s, "sub", b"x"), 409);
        assert_eq!(put(&s, "n.txt", b"contenido"), 200);
        assert!(std::fs::read_dir(s.dir.join("raiz")).unwrap().all(|e| !e.unwrap().file_name().to_string_lossy().starts_with(".fbtmp")));
        assert_eq!(op(&s, "op=mkdir&p=raiz/nueva"), 200);
        assert!(s.dir.join("raiz/nueva").is_dir());
        assert_eq!(op(&s, "op=mv&p=raiz/n.txt&to=raiz/nueva/m.txt"), 200);
        assert!(s.dir.join("raiz/nueva/m.txt").is_file() && !s.dir.join("raiz/n.txt").exists());
        assert_eq!(op(&s, "op=cp&p=raiz/nueva/m.txt&to=raiz/c.txt"), 200);
        assert_eq!(std::fs::read_to_string(s.dir.join("raiz/c.txt")).unwrap(), "contenido");
        assert_eq!(op(&s, "op=rm&p=raiz/c.txt"), 200);
        assert_eq!(op(&s, "op=rmrf&p=raiz/nueva"), 200);
        assert!(!s.dir.join("raiz/nueva").exists());
        let _ = std::fs::remove_dir_all(&s.dir);
    }

    #[test]
    fn no_sale_de_la_carpeta_compartida() {
        let s = start(true, 1 << 20);
        assert_ne!(put(&s, "../fuera/x.txt", b"x"), 200);
        assert_ne!(put(&s, "%2e%2e/fuera/x.txt", b"x"), 200);
        assert_ne!(op(&s, "op=mv&p=raiz/a.txt&to=raiz/../../fuera/a.txt"), 200);
        assert!(!s.dir.join("fuera/x.txt").exists() && !s.dir.join("fuera/a.txt").exists());
        // nombres ocultos
        assert_eq!(put(&s, ".nuevo", b"x"), 403);
        assert_eq!(op(&s, "op=rm&p=raiz/.oculto"), 403);
        assert!(s.dir.join("raiz/.oculto").exists());
        // la raíz misma no se toca
        assert_ne!(op(&s, "op=rmrf&p=raiz"), 200);
        assert!(s.dir.join("raiz").is_dir());
        // un enlace que sale de la raíz no deja escribir fuera
        #[cfg(unix)]
        {
            assert_ne!(put(&s, "enlace/x.txt", b"x"), 200);
            assert!(!s.dir.join("fuera/x.txt").exists());
            assert_eq!(std::fs::read_to_string(s.dir.join("fuera/secreto.txt")).unwrap(), "no");
        }
        let _ = std::fs::remove_dir_all(&s.dir);
    }

    #[test]
    fn respeta_el_tope_de_subida() {
        let s = start(true, 10);
        assert_eq!(put(&s, "grande.bin", &[0u8; 64]), 413);
        assert!(!s.dir.join("raiz/grande.bin").exists());
        assert_eq!(put(&s, "ok.bin", &[0u8; 8]), 200);
        let _ = std::fs::remove_dir_all(&s.dir);
    }

    fn raw(s: &Srv, bytes: &[u8]) -> u16 {
        let mut t = TcpStream::connect(("127.0.0.1", s.port)).unwrap();
        t.write_all(bytes).unwrap();
        let mut out = Vec::new();
        let _ = t.read_to_end(&mut out);
        String::from_utf8_lossy(&out).split(' ').nth(1).and_then(|c| c.parse().ok()).unwrap_or(0)
    }

    #[test]
    fn put_sin_longitud_no_vacia_el_archivo() {
        let s = start(true, 1 << 20);
        let h = format!("PUT /s/tok/raiz/a.txt HTTP/1.1\r\nHost: 127.0.0.1:{p}\r\nX-FB: 1\r\nOrigin: http://127.0.0.1:{p}\r\n\r\n", p = s.port);
        assert_eq!(raw(&s, h.as_bytes()), 411);
        assert_eq!(std::fs::read_to_string(s.dir.join("raiz/a.txt")).unwrap(), "hola");
        // declarar 0 explícitamente sí vacía
        assert_eq!(put(&s, "a.txt", b""), 200);
        let _ = std::fs::remove_dir_all(&s.dir);
    }

    #[cfg(unix)]
    #[test]
    fn mv_no_pisa_otro_archivo_que_solo_difiere_en_mayusculas() {
        let s = start(true, 1 << 20);
        std::fs::write(s.dir.join("raiz/Uno"), "A").unwrap();
        std::fs::write(s.dir.join("raiz/uno"), "B").unwrap();
        assert_eq!(op(&s, "op=mv&p=raiz/Uno&to=raiz/uno"), 409);
        assert_eq!(std::fs::read_to_string(s.dir.join("raiz/uno")).unwrap(), "B");
        assert_eq!(std::fs::read_to_string(s.dir.join("raiz/Uno")).unwrap(), "A");
        let _ = std::fs::remove_dir_all(&s.dir);
    }

    #[test]
    fn lo_oculto_no_se_arrastra_al_borrar_ni_mover() {
        let s = start(true, 1 << 20);
        std::fs::create_dir_all(s.dir.join("raiz/d/x/.git")).unwrap();
        std::fs::write(s.dir.join("raiz/d/x/.git/config"), "c").unwrap();
        assert_eq!(op(&s, "op=rmrf&p=raiz/d"), 409);
        assert_eq!(op(&s, "op=mv&p=raiz/d&to=raiz/e"), 409);
        assert!(s.dir.join("raiz/d/x/.git/config").exists());
        // sin nada oculto sí se puede
        std::fs::create_dir_all(s.dir.join("raiz/limpia/y")).unwrap();
        assert_eq!(op(&s, "op=rmrf&p=raiz/limpia"), 200);
        let _ = std::fs::remove_dir_all(&s.dir);
    }

    #[test]
    fn raices_anidadas_no_se_tocan_desde_la_exterior() {
        let dir = std::env::temp_dir().join(format!("fbs_n_{}_{}", std::process::id(), crate::write::rand_hex()));
        std::fs::create_dir_all(dir.join("x/sub/dentro")).unwrap();
        std::fs::write(dir.join("x/sub/dentro/f"), "1").unwrap();
        let canon = |p: &str| std::fs::canonicalize(dir.join(p)).unwrap();
        let roots = vec![Root { name: "x".into(), path: canon("x") }, Root { name: "sub".into(), path: canon("x/sub") }];
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let ctx = Ctx::new(roots, "tok".into(), false, true, 1 << 20, None);
        std::thread::spawn(move || serve(l, Arc::new(ctx), 16));
        let s = Srv { port, dir };
        assert_eq!(op(&s, "op=rmrf&p=x/sub"), 403);
        assert_eq!(op(&s, "op=mv&p=x/sub&to=x/otro"), 403);
        assert_eq!(op(&s, "op=rmrf&p=x/sub/dentro"), 200);
        assert!(s.dir.join("x/sub").is_dir());
        let _ = std::fs::remove_dir_all(&s.dir);
    }

    #[test]
    fn cp_respeta_el_tope() {
        let s = start(true, 10);
        std::fs::write(s.dir.join("raiz/grande"), [0u8; 64]).unwrap();
        assert_eq!(op(&s, "op=cp&p=raiz/grande&to=raiz/copia"), 413);
        assert!(!s.dir.join("raiz/copia").exists());
        let _ = std::fs::remove_dir_all(&s.dir);
    }

    #[cfg(unix)]
    #[test]
    fn al_reemplazar_se_conservan_los_permisos() {
        use std::os::unix::fs::PermissionsExt;
        let s = start(true, 1 << 20);
        let p = s.dir.join("raiz/run.sh");
        std::fs::write(&p, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o750)).unwrap();
        assert_eq!(put(&s, "run.sh", b"#!/bin/sh\necho\n"), 200);
        assert_eq!(std::fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o750);
        let _ = std::fs::remove_dir_all(&s.dir);
    }

    #[test]
    fn el_origen_vale_con_cualquier_esquema_pero_mismo_host() {
        let s = start(true, 1 << 20);
        // detrás de un proxy TLS el navegador manda https aunque aquí llegue http
        let h = vec![("X-FB", "1".into()), ("Origin", format!("https://127.0.0.1:{}", s.port))];
        assert_eq!(req(&s, "PUT", "/s/tok/raiz/n.txt", &h, b"x").0, 200);
        // otro puerto u otro host no
        let h = vec![("X-FB", "1".into()), ("Origin", format!("http://127.0.0.1:{}", s.port + 1))];
        assert_eq!(req(&s, "PUT", "/s/tok/raiz/m.txt", &h, b"x").0, 403);
        let h = vec![("X-FB", "1".into()), ("Origin", format!("http://127.0.0.1:{}/x", s.port))];
        assert_eq!(req(&s, "PUT", "/s/tok/raiz/m.txt", &h, b"x").0, 403);
        let h = vec![("X-FB", "1".into()), ("Origin", "null".into())];
        assert_eq!(req(&s, "PUT", "/s/tok/raiz/m.txt", &h, b"x").0, 403);
        assert!(!s.dir.join("raiz/m.txt").exists());
        let _ = std::fs::remove_dir_all(&s.dir);
    }

    #[test]
    fn puertos_por_defecto_y_mayusculas() {
        assert_eq!(super::host_key("Ejemplo.ORG:443"), "ejemplo.org");
        assert_eq!(super::host_key("ejemplo.org:80"), "ejemplo.org");
        assert_eq!(super::host_key("ejemplo.org:8080"), "ejemplo.org:8080");
    }

    #[test]
    fn cp_no_pisa_y_put_solo_crea_con_if_none_match() {
        let s = start(true, 1 << 20);
        std::fs::write(s.dir.join("raiz/b.txt"), "otro").unwrap();
        let (c, body) = req(&s, "POST", "/s/tok/api/op?op=cp&p=raiz/a.txt&to=raiz/b.txt", &good(&s), b"");
        assert_eq!((c, body.contains("\"exists\"")), (409, true));
        assert_eq!(std::fs::read_to_string(s.dir.join("raiz/b.txt")).unwrap(), "otro");
        let mut h = good(&s);
        h.push(("If-None-Match", "*".into()));
        assert_eq!(req(&s, "PUT", "/s/tok/raiz/a.txt", &h, b"nuevo").0, 412);
        assert_eq!(std::fs::read_to_string(s.dir.join("raiz/a.txt")).unwrap(), "hola");
        assert_eq!(req(&s, "PUT", "/s/tok/raiz/nuevo.txt", &h, b"nuevo").0, 200);
        assert_eq!(std::fs::read_to_string(s.dir.join("raiz/nuevo.txt")).unwrap(), "nuevo");
        // sin la cabecera se sigue reemplazando
        assert_eq!(put(&s, "a.txt", b"cambiado"), 200);
        let _ = std::fs::remove_dir_all(&s.dir);
    }

    #[test]
    fn los_errores_dicen_el_motivo() {
        let s = start(true, 10);
        std::fs::create_dir_all(s.dir.join("raiz/d/x")).unwrap();
        std::fs::write(s.dir.join("raiz/d/x/.oculto"), "x").unwrap();
        let (c, b) = req(&s, "POST", "/s/tok/api/op?op=rmrf&p=raiz/d", &good(&s), b"");
        assert_eq!(c, 409);
        assert!(b.contains("\"error\":\"hidden\""), "{b}");
        let (c, b) = req(&s, "PUT", "/s/tok/raiz/g.bin", &good(&s), &[0u8; 64]);
        assert_eq!(c, 413);
        assert!(b.contains("too_large"), "{b}");
        // el tope se publica para que el cliente lo compruebe antes de leer el archivo
        let (c, b) = req(&s, "GET", "/s/tok/api/info", &[], b"");
        assert_eq!(c, 200);
        assert!(b.contains("\"max_upload\":10"), "{b}");
        let _ = std::fs::remove_dir_all(&s.dir);
    }

    #[test]
    fn un_413_llega_aunque_el_cuerpo_siga_enviandose() {
        // cuerpo de 4 MiB contra un tope de 10 B: el servidor responde y descarta lo que sigue llegando,
        // así el cliente lee el 413 en vez de un corte
        let s = start(true, 10);
        let body = vec![0u8; 4 << 20];
        let (c, b) = req(&s, "PUT", "/s/tok/raiz/g.bin", &good(&s), &body);
        assert_eq!(c, 413);
        assert!(b.contains("too_large"));
        let _ = std::fs::remove_dir_all(&s.dir);
    }

    #[test]
    fn sweep_borra_solo_temporales_viejos() {
        let s = start(true, 1 << 20);
        let viejo = s.dir.join("raiz/sub/.fbtmp-0123456789abcdef");
        let nuevo = s.dir.join("raiz/.fbtmp-fedcba9876543210");
        // viejos pero no con el patrón exacto del servidor: son del usuario y no se tocan
        let ajenos = [s.dir.join("raiz/.fbtmp-viejo"), s.dir.join("raiz/.fbtmp-0123456789ABCDEF"), s.dir.join("raiz/.fbtmp-0123456789abcdef0")];
        for p in std::iter::once(&viejo).chain(&ajenos) {
            std::fs::write(p, "x").unwrap();
            let f = std::fs::File::options().write(true).open(p).unwrap();
            f.set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(7200)).unwrap();
        }
        std::fs::write(&nuevo, "x").unwrap();
        let root = Root { name: "raiz".into(), path: std::fs::canonicalize(s.dir.join("raiz")).unwrap() };
        crate::write::sweep_temp(&[root]);
        assert!(!viejo.exists() && nuevo.exists());
        assert!(ajenos.iter().all(|p| p.exists()));
        let _ = std::fs::remove_dir_all(&s.dir);
    }
}
