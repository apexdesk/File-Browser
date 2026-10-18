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

fn csrf_ok(req: &Req, tls: bool) -> bool {
    let (Some(origin), Some(host)) = (&req.origin, &req.host) else { return false };
    let o = origin.strip_prefix(if tls { "https://" } else { "http://" }).unwrap_or("");
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

/// Al arrancar con `--write`: borra los `.fbtmp-*` de más de una hora que dejó un corte (kill, apagón).
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
                && e.file_name().to_string_lossy().starts_with(".fbtmp-")
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
    // ni una raíz ni una carpeta que contenga otra raíz compartida (raíces anidadas)
    let target = dir.join(name);
    if ctx.roots.iter().any(|r| r.path.starts_with(&target)) {
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
    if !csrf_ok(req, ctx.tls.is_some()) {
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
    // sin longitud declarada no se sabe si el cuerpo viene vacío o el cliente falló: no se vacía el archivo
    if !req.has_length {
        return Err(411);
    }
    let (_, dir, name) = parent_and_name(ctx, &comps)?;
    let target = dir.join(&name);
    let old = fs::symlink_metadata(&target).ok();
    if old.as_ref().is_some_and(|m| m.is_dir()) {
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
    // al reemplazar un archivo se conservan sus permisos (p. ej. el bit ejecutable)
    if let Some(m) = old.filter(|m| m.is_file()) {
        let _ = fs::set_permissions(&tmp, m.permissions());
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
            if md.is_dir() {
                // lo oculto no se ve ni se lista: tampoco se borra de rebote
                if !ctx.hidden && has_hidden(&t) {
                    return Err(409);
                }
                fs::remove_dir_all(t)
            } else {
                fs::remove_file(t)
            }
            .map_err(|e| status_of(&e))
        }
        ("mv", Some(to)) => {
            let (r1, d1, n1) = parent_and_name(ctx, &p)?;
            let (r2, d2, n2) = parent_and_name(ctx, to)?;
            if r1.name != r2.name {
                return Err(400);
            }
            let (src, dst) = (d1.join(n1), d2.join(n2));
            let md = fs::symlink_metadata(&src).map_err(|e| status_of(&e))?;
            if md.is_dir() && !ctx.hidden && has_hidden(&src) {
                return Err(409);
            }
            // solo un cambio de mayúsculas puede «existir» ya en el destino, y solo si es el mismo archivo
            if let Ok(d) = fs::symlink_metadata(&dst) {
                if !same_file(&md, &d) {
                    return Err(409);
                }
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

/// ¿Son el mismo archivo? (un sistema que no distingue mayúsculas ve `A` y `a` como uno solo)
fn same_file(a: &fs::Metadata, b: &fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        a.dev() == b.dev() && a.ino() == b.ino()
    }
    #[cfg(not(unix))]
    {
        let _ = (a, b);
        // sin identificador de archivo a mano: solo se admite si el nombre difiere en mayúsculas (ya comprobado por el sistema)
        true
    }
}

/// ¿Hay algo oculto (nombre con «.») dentro de la carpeta, a cualquier profundidad? No sigue enlaces.
fn has_hidden(dir: &Path) -> bool {
    let Ok(rd) = fs::read_dir(dir) else { return false };
    for e in rd.flatten() {
        if e.file_name().to_string_lossy().starts_with('.') {
            return true;
        }
        if e.file_type().is_ok_and(|t| t.is_dir()) && has_hidden(&e.path()) {
            return true;
        }
    }
    false
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
    let md = fs::metadata(&src).map_err(|e| status_of(&e))?;
    if !md.is_file() {
        return Err(400);
    }
    if md.len() > ctx.max_upload {
        return Err(413);
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
    fn el_origen_debe_coincidir_con_el_esquema() {
        let s = start(true, 1 << 20);
        let h = vec![("X-FB", "1".into()), ("Origin", format!("https://127.0.0.1:{}", s.port))];
        assert_eq!(req(&s, "PUT", "/s/tok/raiz/n.txt", &h, b"x").0, 403);
        let _ = std::fs::remove_dir_all(&s.dir);
    }

    #[test]
    fn quien_conoce_el_enlace_no_queda_bloqueado_por_otros() {
        let s = start(false, 1 << 20);
        for i in 0..25 {
            assert_eq!(req(&s, "GET", &format!("/s/mal{i}/"), &[], b"").0, if i < 20 { 404 } else { 429 });
        }
        // la IP está bloqueada para adivinar, pero el enlace correcto sigue entrando
        assert_eq!(req(&s, "GET", "/s/tok/api/info", &[], b"").0, 200);
        assert_eq!(req(&s, "GET", "/s/otro/", &[], b"").0, 429);
        let _ = std::fs::remove_dir_all(&s.dir);
    }

    #[test]
    fn sweep_borra_solo_temporales_viejos() {
        let s = start(true, 1 << 20);
        let viejo = s.dir.join("raiz/sub/.fbtmp-viejo");
        let nuevo = s.dir.join("raiz/.fbtmp-nuevo");
        std::fs::write(&viejo, "x").unwrap();
        std::fs::write(&nuevo, "x").unwrap();
        let f = std::fs::File::options().write(true).open(&viejo).unwrap();
        f.set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(7200)).unwrap();
        drop(f);
        let root = Root { name: "raiz".into(), path: std::fs::canonicalize(s.dir.join("raiz")).unwrap() };
        crate::write::sweep_temp(&[root]);
        assert!(!viejo.exists() && nuevo.exists());
        let _ = std::fs::remove_dir_all(&s.dir);
    }
}
