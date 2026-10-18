//! Escritura (solo con `--write`): subir/sobrescribir un archivo (`PUT /s/<token>/ruta`) y las
//! operaciones sueltas (`POST /s/<token>/api/op?op=…&p=…&to=…`): crear carpeta, borrar, mover y copiar.
//!
//! Las mismas reglas que la lectura: toda ruta pasa por `jail` (componentes validados, la carpeta
//! contenedora canonicalizada y dentro de la raíz, nada oculto) y nunca se toca una raíz compartida.
//! Además, cada petición debe llevar `X-FB: 1` y un `Origin` igual al `Host`: una página de otro
//! sitio no puede mandarlas desde el navegador de quien tenga el enlace abierto.

use crate::http::{self, Req};
use crate::jail::{self, Root};
use crate::server::{Ctx, query_get, virt_path};
use crate::stream::Stream;
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Plazo para cada lectura del cuerpo: un envío que se queda parado se corta.
const BODY_STALL: Duration = Duration::from_secs(30);

fn csrf_ok(req: &Req) -> bool {
    let (Some(origin), Some(host)) = (&req.origin, &req.host) else { return false };
    let o = origin.strip_prefix("http://").or_else(|| origin.strip_prefix("https://")).unwrap_or("");
    req.xfb && !o.is_empty() && o == host
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

fn rand_hex() -> String {
    let mut b = [0u8; 8];
    let _ = getrandom::fill(&mut b);
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// (raíz, carpeta contenedora canónica, nombre) de la ruta virtual `comps`. Las raíces mismas no se tocan.
fn parent_and_name<'a>(ctx: &'a Ctx, comps: &[String]) -> Result<(&'a Root, PathBuf, String), u16> {
    let (name, parent) = comps.split_last().ok_or(400u16)?;
    if parent.is_empty() {
        return Err(403);
    }
    let (root, dir) = jail::resolve(&ctx.roots, parent, ctx.hidden).ok_or(404u16)?;
    if !dir.is_dir() {
        return Err(404);
    }
    if !ctx.hidden && name.starts_with('.') {
        return Err(403);
    }
    Ok((root, dir, name.clone()))
}

fn ok(s: &mut Stream) -> io::Result<()> {
    http::send(s, 200, "application/json; charset=utf-8", b"{\"ok\":true}", false, &[])
}

/// Entrada de `route` para PUT y POST.
pub fn dispatch(s: &mut Stream, ctx: &Ctx, req: &Req, tail: &str) -> io::Result<()> {
    if !ctx.write {
        return http::error(s, 405, false);
    }
    if !csrf_ok(req) {
        return http::error(s, 403, false);
    }
    let r = if req.method == http::Method::Put {
        put_file(s, ctx, req, tail)
    } else if tail == "/api/op" {
        op(ctx, req)
    } else {
        Err(404)
    };
    match r {
        Ok(()) => ok(s),
        Err(code) => http::error(s, code, false),
    }
}

fn put_file(s: &mut Stream, ctx: &Ctx, req: &Req, tail: &str) -> Result<(), u16> {
    let (comps, slash) = jail::split_url(tail).ok_or(404u16)?;
    if slash {
        return Err(404);
    }
    let (_, dir, name) = parent_and_name(ctx, &comps)?;
    let target = dir.join(&name);
    if fs::symlink_metadata(&target).is_ok_and(|m| m.is_dir()) {
        return Err(409);
    }
    if req.content_length > ctx.max_upload {
        return Err(413);
    }
    let tmp = dir.join(format!(".fbtmp-{}", rand_hex()));
    let mut f = OpenOptions::new().write(true).create_new(true).open(&tmp).map_err(|e| status_of(&e))?;
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
        let _ = fs::remove_file(&tmp);
        return Err(if e.kind() == io::ErrorKind::UnexpectedEof || e.kind() == io::ErrorKind::TimedOut { 400 } else { status_of(&e) });
    }
    if let Err(e) = fs::rename(&tmp, &target) {
        let _ = fs::remove_file(&tmp);
        return Err(status_of(&e));
    }
    eprintln!("[escritura] subido {} ({} B)", comps.join("/"), req.content_length);
    Ok(())
}

fn op(ctx: &Ctx, req: &Req) -> Result<(), u16> {
    let name = query_get(&req.query, "op").ok_or(400u16)?;
    let p = virt_path(&query_get(&req.query, "p").ok_or(400u16)?).ok_or(400u16)?;
    let to = match query_get(&req.query, "to") {
        Some(t) => Some(virt_path(&t).ok_or(400u16)?),
        None => None,
    };
    let r = match (name.as_str(), &to) {
        ("mkdir", None) => {
            let (_, dir, n) = parent_and_name(ctx, &p)?;
            fs::create_dir(dir.join(n)).map_err(|e| status_of(&e))
        }
        ("mkdirs", None) => mkdirs(ctx, &p).map_err(|e| status_of(&e)),
        ("rm", None) => {
            let (_, dir, n) = parent_and_name(ctx, &p)?;
            let t = dir.join(n);
            if fs::symlink_metadata(&t).map_err(|e| status_of(&e))?.is_dir() {
                return Err(409);
            }
            fs::remove_file(t).map_err(|e| status_of(&e))
        }
        ("rmdir", None) => {
            let (_, dir, n) = parent_and_name(ctx, &p)?;
            fs::remove_dir(dir.join(n)).map_err(|e| status_of(&e))
        }
        ("rmrf", None) => {
            let (_, dir, n) = parent_and_name(ctx, &p)?;
            let t = dir.join(n);
            let md = fs::symlink_metadata(&t).map_err(|e| status_of(&e))?;
            if md.is_dir() { fs::remove_dir_all(t) } else { fs::remove_file(t) }.map_err(|e| status_of(&e))
        }
        ("mv", Some(to)) => {
            let (r1, d1, n1) = parent_and_name(ctx, &p)?;
            let (r2, d2, n2) = parent_and_name(ctx, to)?;
            if r1.name != r2.name {
                return Err(400);
            }
            let (src, dst) = (d1.join(n1), d2.join(n2));
            fs::symlink_metadata(&src).map_err(|e| status_of(&e))?;
            // solo un cambio de mayúsculas puede «existir» ya en el destino (es el mismo archivo)
            if fs::symlink_metadata(&dst).is_ok() && !same_ignoring_case(&src, &dst) {
                return Err(409);
            }
            fs::rename(src, dst).map_err(|e| status_of(&e))
        }
        ("cp", Some(to)) => copy(ctx, &p, to),
        _ => return Err(400),
    };
    match r {
        Ok(()) => {
            eprintln!("[escritura] {name} {}{}", p.join("/"), to.map(|t| format!(" → {}", t.join("/"))).unwrap_or_default());
            Ok(())
        }
        Err(c) => Err(c),
    }
}

fn same_ignoring_case(a: &Path, b: &Path) -> bool {
    a != b && a.to_string_lossy().to_lowercase() == b.to_string_lossy().to_lowercase()
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

fn copy(ctx: &Ctx, from: &[String], to: &[String]) -> Result<(), u16> {
    let (r1, src) = jail::resolve(&ctx.roots, from, ctx.hidden).ok_or(404u16)?;
    if !fs::metadata(&src).map_err(|e| status_of(&e))?.is_file() {
        return Err(400);
    }
    let (r2, d2, n2) = parent_and_name(ctx, to)?;
    if r1.name != r2.name {
        return Err(400);
    }
    let dst = d2.join(n2);
    if fs::symlink_metadata(&dst).is_ok_and(|m| m.is_dir()) {
        return Err(409);
    }
    let tmp = d2.join(format!(".fbtmp-{}", rand_hex()));
    if let Err(e) = fs::copy(&src, &tmp) {
        let _ = fs::remove_file(&tmp);
        return Err(status_of(&e));
    }
    fs::rename(&tmp, &dst).map_err(|e| {
        let _ = fs::remove_file(&tmp);
        status_of(&e)
    })
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
}
