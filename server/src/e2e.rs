//! Pruebas de extremo a extremo: el servidor de verdad en 127.0.0.1 con un puerto efímero, dentro del
//! propio proceso de pruebas (hilos de 512 KiB como en producción), y peticiones crudas por TCP o TLS.
//! Más una prueba de humo del analizador con mutaciones deterministas (sin dependencias nuevas).

use crate::auth::Creds;
use crate::jail::Root;
use crate::server::{Ctx, serve};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

const TOK: &str = "0123456789abcdef0123456789abcdef";

struct Srv {
    port: u16,
    dir: PathBuf,
    #[cfg(feature = "tls")]
    fp: Option<String>,
}

impl Drop for Srv {
    fn drop(&mut self) {
        // con pila holgada: algunas pruebas dejan árboles de más de mil niveles
        let d = self.dir.clone();
        let _ = std::thread::Builder::new().stack_size(64 << 20).spawn(move || std::fs::remove_dir_all(d)).map(|h| h.join());
    }
}

#[derive(Default)]
struct Opts {
    write: bool,
    auth: bool,
    tls: bool,
    max_conn: usize,
    proxy: bool,
}

fn uniq() -> String {
    let mut b = [0u8; 8];
    getrandom::fill(&mut b).unwrap();
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn start(o: Opts) -> Srv {
    let dir = std::env::temp_dir().join(format!("fbs_e2e_{}_{}", std::process::id(), uniq()));
    std::fs::create_dir_all(dir.join("raiz/sub")).unwrap();
    std::fs::create_dir_all(dir.join("raiz/.git")).unwrap();
    std::fs::create_dir_all(dir.join("fuera")).unwrap();
    std::fs::write(dir.join("raiz/a.txt"), "hola mundo").unwrap();
    std::fs::write(dir.join("raiz/sub/b.bin"), [7u8; 1000]).unwrap();
    std::fs::write(dir.join("raiz/p.html"), "<script>alert(1)</script>").unwrap();
    std::fs::write(dir.join("raiz/.env"), "SECRETO=1").unwrap();
    std::fs::write(dir.join("raiz/.git/config"), "c").unwrap();
    std::fs::write(dir.join("fuera/secreto.txt"), "no").unwrap();
    let root = Root { name: "raiz".into(), path: std::fs::canonicalize(dir.join("raiz")).unwrap() };
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    #[cfg(feature = "tls")]
    let (tls, fp) = if o.tls {
        let (c, fp) = crate::tls::make(&["127.0.0.1".parse().unwrap()]).unwrap();
        (Some(c), Some(fp))
    } else {
        (None, None)
    };
    #[cfg(not(feature = "tls"))]
    let tls = {
        assert!(!o.tls);
        None
    };
    let mut ctx = Ctx::new(vec![root], TOK.into(), false, o.write, 1 << 20, tls);
    if o.auth {
        ctx.auth = Some(Creds::new("ana".into(), "contraseña-larga".into()));
    }
    ctx.proxy = o.proxy;
    let max = if o.max_conn == 0 { 32 } else { o.max_conn };
    std::thread::spawn(move || serve(l, Arc::new(ctx), max));
    Srv {
        port,
        dir,
        #[cfg(feature = "tls")]
        fp,
    }
}

struct Resp {
    code: u16,
    head: String,
    body: Vec<u8>,
}

impl Resp {
    fn header(&self, k: &str) -> Option<String> {
        self.head.split("\r\n").skip(1).find_map(|l| l.split_once(':').filter(|(a, _)| a.eq_ignore_ascii_case(k)).map(|(_, v)| v.trim().to_string()))
    }
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
}

fn parse_resp(out: &[u8]) -> Resp {
    let end = out.windows(4).position(|w| w == b"\r\n\r\n").unwrap_or(out.len());
    let head = String::from_utf8_lossy(&out[..end]).into_owned();
    let code = head.split(' ').nth(1).and_then(|c| c.parse().ok()).unwrap_or(0);
    Resp { code, head, body: out.get(end + 4..).unwrap_or(&[]).to_vec() }
}

fn raw(s: &Srv, bytes: &[u8]) -> Resp {
    let mut t = TcpStream::connect(("127.0.0.1", s.port)).unwrap();
    let _ = t.set_read_timeout(Some(Duration::from_secs(20)));
    let _ = t.write_all(bytes);
    let mut out = Vec::new();
    let _ = t.read_to_end(&mut out);
    parse_resp(&out)
}

fn build(s: &Srv, method: &str, path: &str, headers: &[(&str, String)], body: &[u8]) -> Vec<u8> {
    let mut h = format!("{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\n", s.port);
    for (k, v) in headers {
        h.push_str(&format!("{k}: {v}\r\n"));
    }
    if method == "PUT" || method == "POST" {
        h.push_str(&format!("Content-Length: {}\r\n", body.len()));
    }
    h.push_str("\r\n");
    let mut v = h.into_bytes();
    v.extend_from_slice(body);
    v
}

fn get(s: &Srv, path: &str) -> Resp {
    raw(s, &build(s, "GET", path, &[], b""))
}

fn get_h(s: &Srv, path: &str, h: &[(&str, String)]) -> Resp {
    raw(s, &build(s, "GET", path, h, b""))
}

fn wr(s: &Srv) -> Vec<(&'static str, String)> {
    vec![("X-FB", "1".into()), ("Origin", format!("http://127.0.0.1:{}", s.port))]
}

fn put(s: &Srv, p: &str, body: &[u8]) -> u16 {
    raw(s, &build(s, "PUT", &format!("/s/{TOK}/raiz/{p}"), &wr(s), body)).code
}

fn op(s: &Srv, q: &str) -> u16 {
    raw(s, &build(s, "POST", &format!("/s/{TOK}/api/op?{q}"), &wr(s), b"")).code
}

#[test]
fn lectura_listado_descarga_rango_y_zip() {
    let s = start(Opts::default());
    // listado
    let r = get(&s, &format!("/s/{TOK}/raiz/"));
    assert_eq!(r.code, 200);
    let t = r.text();
    assert!(t.contains("a.txt") && t.contains("sub/"));
    assert!(!t.contains(".env") && !t.contains(".git"));
    assert_eq!(r.header("X-Content-Type-Options").as_deref(), Some("nosniff"));
    assert!(r.header("Content-Security-Policy").is_some());
    // sin barra final redirige
    assert_eq!(get(&s, &format!("/s/{TOK}/raiz")).code, 301);
    // descarga
    let r = get(&s, &format!("/s/{TOK}/raiz/a.txt"));
    assert_eq!((r.code, r.text().as_str()), (200, "hola mundo"));
    let etag = r.header("ETag").unwrap();
    // HEAD sin cuerpo
    let r = raw(&s, &build(&s, "HEAD", &format!("/s/{TOK}/raiz/a.txt"), &[], b""));
    assert_eq!((r.code, r.body.len()), (200, 0));
    assert_eq!(r.header("Content-Length").as_deref(), Some("10"));
    // rangos
    let r = get_h(&s, &format!("/s/{TOK}/raiz/a.txt"), &[("Range", "bytes=1-3".into())]);
    assert_eq!((r.code, r.text().as_str()), (206, "ola"));
    assert_eq!(r.header("Content-Range").as_deref(), Some("bytes 1-3/10"));
    assert_eq!(get_h(&s, &format!("/s/{TOK}/raiz/a.txt"), &[("Range", "bytes=50-".into())]).code, 416);
    assert_eq!(get_h(&s, &format!("/s/{TOK}/raiz/a.txt"), &[("If-None-Match", etag)]).code, 304);
    // HTML siempre como descarga
    let r = get(&s, &format!("/s/{TOK}/raiz/p.html"));
    assert_eq!(r.code, 200);
    assert_eq!(r.header("Content-Type").as_deref(), Some("application/octet-stream"));
    assert!(r.header("Content-Disposition").unwrap().starts_with("attachment"));
    // ocultos y fuera de la raíz
    for p in ["raiz/.env", "raiz/.git/config", "raiz/../fuera/secreto.txt", "raiz/%2e%2e/fuera/secreto.txt", "fuera/secreto.txt"] {
        assert_eq!(get(&s, &format!("/s/{TOK}/{p}")).code, 404, "{p}");
    }
    // API JSON
    let r = get(&s, &format!("/s/{TOK}/api/list?p=raiz"));
    assert_eq!(r.code, 200);
    assert!(r.text().contains("\"a.txt\"") && !r.text().contains(".env"));
    // ZIP de la carpeta, leído con otra biblioteca
    let r = get(&s, &format!("/s/{TOK}/raiz/?zip=1"));
    assert_eq!(r.code, 200);
    let mut ar = zip::ZipArchive::new(std::io::Cursor::new(r.body)).unwrap();
    let mut txt = String::new();
    ar.by_name("raiz/a.txt").unwrap().read_to_string(&mut txt).unwrap();
    assert_eq!(txt, "hola mundo");
    assert_eq!(ar.by_name("raiz/sub/b.bin").unwrap().size(), 1000);
    assert!((0..ar.len()).all(|i| !ar.by_index(i).unwrap().name().unwrap().contains("/.")));
    // búsqueda
    assert!(get(&s, &format!("/s/{TOK}/raiz/?q=b.bin")).text().contains("sub/b.bin"));
    // solo lectura de verdad
    assert_eq!(put(&s, "n.txt", b"x"), 405);
    assert!(!s.dir.join("raiz/n.txt").exists());
}

#[test]
fn el_token_se_exige_en_todas_las_rutas() {
    let s = start(Opts { write: true, ..Default::default() });
    let mal = "0123456789abcdef0123456789abcdee";
    let rutas = [
        "/".to_string(),
        "/s/".into(),
        "/raiz/a.txt".into(),
        format!("/s/{mal}"),
        format!("/s/{mal}/"),
        format!("/s/{mal}/raiz/"),
        format!("/s/{mal}/raiz/a.txt"),
        format!("/s/{mal}/raiz/?zip=1"),
        format!("/s/{mal}/raiz/?q=a"),
        format!("/s/{mal}/api/info"),
        format!("/s/{mal}/api/list?p=raiz"),
        format!("/s/{mal}/app/"),
        format!("/s/{}/raiz/a.txt", &TOK[..31]),
        format!("/s/{TOK}0/raiz/a.txt"),
    ];
    for p in &rutas {
        let r = get(&s, p);
        assert_eq!(r.code, 404, "{p}");
        assert!(!r.text().contains("hola"));
    }
    let r = raw(&s, &build(&s, "PUT", &format!("/s/{mal}/raiz/n.txt"), &wr(&s), b"x"));
    assert_eq!(r.code, 404);
    let r = raw(&s, &build(&s, "POST", &format!("/s/{mal}/api/op?op=rm&p=raiz/a.txt"), &wr(&s), b""));
    assert_eq!(r.code, 404);
    assert!(!s.dir.join("raiz/n.txt").exists() && s.dir.join("raiz/a.txt").exists());
    // con el token bueno sí
    assert_eq!(get(&s, &format!("/s/{TOK}/raiz/a.txt")).code, 200);
}

fn basic(user: &str, pass: &str) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let raw = format!("{user}:{pass}").into_bytes();
    let mut o = String::new();
    for c in raw.chunks(3) {
        let n = (u32::from(c[0]) << 16) | (u32::from(*c.get(1).unwrap_or(&0)) << 8) | u32::from(*c.get(2).unwrap_or(&0));
        for i in 0..4 {
            if i <= c.len() {
                o.push(A[((n >> (18 - 6 * i)) & 63) as usize] as char);
            } else {
                o.push('=');
            }
        }
    }
    format!("Basic {o}")
}

#[test]
fn contrasena_y_bloqueo_sin_oraculo() {
    let s = start(Opts { auth: true, ..Default::default() });
    let url = format!("/s/{TOK}/raiz/a.txt");
    let bien = basic("ana", "contraseña-larga");
    // sin credenciales: 401 con lo que el navegador necesita para pedirlas (no cuenta como fallo)
    let r = get(&s, &url);
    assert_eq!(r.code, 401);
    assert!(r.header("WWW-Authenticate").unwrap().starts_with("Basic"));
    // con las buenas: entra y recibe la cookie de sesión
    let r = get_h(&s, &url, &[("Authorization", bien.clone())]);
    assert_eq!((r.code, r.text().as_str()), (200, "hola mundo"));
    let sc = r.header("Set-Cookie").unwrap();
    assert!(sc.contains("HttpOnly") && sc.contains(&format!("Path=/s/{TOK}/")));
    let cookie = sc.split(';').next().unwrap().to_string();
    // 20 contraseñas falsas: 401; luego, bloqueado
    for i in 0..20 {
        assert_eq!(get_h(&s, &url, &[("Authorization", basic("ana", &format!("mala{i}")))]).code, 401, "{i}");
    }
    let mala = get_h(&s, &url, &[("Authorization", basic("ana", "otra-mala"))]);
    assert_eq!(mala.code, 429);
    // bloqueado, acertar no sirve de nada: misma respuesta, byte a byte, que fallar (no hay oráculo)
    let buena = get_h(&s, &url, &[("Authorization", bien.clone())]);
    assert_eq!(buena.code, 429);
    assert_eq!((buena.head.clone(), buena.body.clone()), (mala.head.clone(), mala.body.clone()));
    assert!(buena.header("Retry-After").is_some());
    // tampoco con una cookie inventada
    let falsa = get_h(&s, &url, &[("Authorization", bien.clone()), ("Cookie", "fbs=00000000000000000000000000000000".into())]);
    assert_eq!(falsa.code, 429);
    // quien ya había entrado (lleva su cookie) no queda fuera por los intentos de otro tras su IP
    let r = get_h(&s, &url, &[("Authorization", bien.clone()), ("Cookie", format!("otra=1; {cookie}"))]);
    assert_eq!((r.code, r.text().as_str()), (200, "hola mundo"));
    // y si su navegador perdió las credenciales, se le vuelven a pedir
    let r = get_h(&s, &url, &[("Cookie", cookie.clone())]);
    assert_eq!(r.code, 401);
    assert!(r.header("WWW-Authenticate").is_some());
    // la cookie sola no da acceso: la contraseña sigue siendo obligatoria
    assert_eq!(get_h(&s, &url, &[("Cookie", cookie.clone()), ("Authorization", basic("ana", "mala"))]).code, 401);
    // el token sigue haciendo falta aunque se tenga la cookie
    assert_eq!(get_h(&s, "/s/0123456789abcdef0123456789abcdee/raiz/a.txt", &[("Cookie", cookie), ("Authorization", bien)]).code, 429);
}

#[test]
fn quien_conoce_el_enlace_no_queda_bloqueado_sin_contrasena() {
    // Solo token (128 bits): el bloqueo por IP frena a quien prueba tokens, no a quien trae el bueno.
    let s = start(Opts::default());
    for i in 0..25 {
        assert_eq!(get(&s, &format!("/s/mal{i}/")).code, if i < 20 { 404 } else { 429 });
    }
    assert_eq!(get(&s, &format!("/s/{TOK}/api/info")).code, 200);
    assert_eq!(get(&s, "/s/otro/").code, 429);
}

#[test]
fn escritura_de_extremo_a_extremo() {
    let s = start(Opts { write: true, ..Default::default() });
    assert_eq!(put(&s, "n.txt", b"contenido"), 200);
    assert_eq!(std::fs::read_to_string(s.dir.join("raiz/n.txt")).unwrap(), "contenido");
    assert_eq!(op(&s, "op=mkdir&p=raiz/d"), 200);
    assert_eq!(op(&s, "op=mkdirs&p=raiz/d/e/f"), 200);
    assert!(s.dir.join("raiz/d/e/f").is_dir());
    assert_eq!(op(&s, "op=mv&p=raiz/n.txt&to=raiz/d/m.txt"), 200);
    assert!(s.dir.join("raiz/d/m.txt").is_file());
    // mv nunca pisa
    assert_eq!(op(&s, "op=mv&p=raiz/d/m.txt&to=raiz/a.txt"), 409);
    assert_eq!(std::fs::read_to_string(s.dir.join("raiz/a.txt")).unwrap(), "hola mundo");
    assert_eq!(op(&s, "op=mv&p=raiz/d/e&to=raiz/sub"), 409);
    assert!(s.dir.join("raiz/sub/b.bin").is_file());
    assert_eq!(op(&s, "op=rm&p=raiz/d/m.txt"), 200);
    assert_eq!(op(&s, "op=rmrf&p=raiz/d"), 200);
    assert!(!s.dir.join("raiz/d").exists());
    // nombres ocultos: ni crear, ni sobrescribir, ni borrar, ni mover encima o desde
    assert_eq!(put(&s, ".env", b"x"), 403);
    assert_eq!(put(&s, ".nuevo", b"x"), 403);
    assert_eq!(op(&s, "op=mkdir&p=raiz/.x"), 403);
    assert_ne!(op(&s, "op=mkdirs&p=raiz/z/.x"), 200);
    assert_eq!(op(&s, "op=rm&p=raiz/.env"), 403);
    assert_eq!(op(&s, "op=mv&p=raiz/a.txt&to=raiz/.a"), 403);
    assert_eq!(op(&s, "op=mv&p=raiz/.git&to=raiz/git"), 403);
    assert_eq!(std::fs::read_to_string(s.dir.join("raiz/.env")).unwrap(), "SECRETO=1");
    assert!(!s.dir.join("raiz/z/.x").exists() && !s.dir.join("raiz/.a").exists());
    // no quedan temporales
    assert!(std::fs::read_dir(s.dir.join("raiz")).unwrap().all(|e| !e.unwrap().file_name().to_string_lossy().starts_with(".fbtmp")));
}

fn deep(base: &std::path::Path, levels: usize) -> PathBuf {
    let mut p = base.to_path_buf();
    for _ in 0..levels {
        p.push("d");
    }
    std::fs::create_dir_all(&p).unwrap();
    p
}

#[test]
fn arboles_profundos_no_tumban_el_servidor() {
    let s = start(Opts { write: true, ..Default::default() });
    // 1000 niveles con algo oculto al fondo: la comprobación (antes recursiva, en la pila de 512 KiB del
    // manejador) los recorre sin recursión y lo encuentra
    let fondo = deep(&s.dir.join("raiz/h"), 1000);
    std::fs::write(fondo.join(".oculto"), "x").unwrap();
    assert_eq!(op(&s, "op=mv&p=raiz/h&to=raiz/h2"), 409);
    assert_eq!(op(&s, "op=rmrf&p=raiz/h"), 409);
    assert!(fondo.join(".oculto").exists());
    // 700 niveles sin nada oculto se borran (en la pila holgada de `remove_tree`)
    deep(&s.dir.join("raiz/limpio"), 700);
    assert_eq!(op(&s, "op=rmrf&p=raiz/limpio"), 200);
    assert!(!s.dir.join("raiz/limpio").exists());
    // pasado el tope de profundidad se niega en lugar de arriesgarse
    deep(&s.dir.join("raiz/muy"), 1100);
    assert_eq!(op(&s, "op=rmrf&p=raiz/muy"), 409);
    assert!(s.dir.join("raiz/muy").exists());
    // y el servidor sigue vivo
    assert_eq!(get(&s, &format!("/s/{TOK}/raiz/a.txt")).code, 200);
}

#[cfg(unix)]
#[test]
fn una_carpeta_ilegible_cuenta_como_no() {
    use std::os::unix::fs::PermissionsExt;
    let s = start(Opts { write: true, ..Default::default() });
    let x = s.dir.join("raiz/cerrada/x");
    std::fs::create_dir_all(&x).unwrap();
    std::fs::set_permissions(&x, std::fs::Permissions::from_mode(0o000)).unwrap();
    // root lee igualmente: entonces la prueba no dice nada
    let legible = std::fs::read_dir(&x).is_ok();
    if !legible {
        assert_eq!(op(&s, "op=mv&p=raiz/cerrada&to=raiz/abierta"), 409);
        assert!(s.dir.join("raiz/cerrada").exists());
    }
    std::fs::set_permissions(&x, std::fs::Permissions::from_mode(0o755)).unwrap();
}

#[test]
fn las_conexiones_mudas_ceden_su_hueco() {
    let s = start(Opts { max_conn: 4, ..Default::default() });
    // cuatro conexiones que no mandan nada ocupan todos los huecos
    let mudas: Vec<TcpStream> = (0..4).map(|_| TcpStream::connect(("127.0.0.1", s.port)).unwrap()).collect();
    std::thread::sleep(Duration::from_millis(1300));
    // una petición de verdad entra igualmente (antes: 503 hasta que caducaban, 10 s)
    let r = get(&s, &format!("/s/{TOK}/raiz/a.txt"));
    assert_eq!((r.code, r.text().as_str()), (200, "hola mundo"));
    drop(mudas);
}

#[cfg(feature = "tls")]
mod tls {
    use super::*;
    use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
    use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
    use rustls::{DigitallySignedStruct, SignatureScheme};
    use sha2::{Digest, Sha256};

    /// Acepta solo el certificado cuya huella imprime el servidor (lo que haría quien la compara a mano).
    #[derive(Debug)]
    struct Pin(String, Arc<rustls::crypto::CryptoProvider>);

    impl ServerCertVerifier for Pin {
        fn verify_server_cert(&self, ee: &CertificateDer<'_>, _: &[CertificateDer<'_>], _: &ServerName<'_>, _: &[u8], _: UnixTime) -> Result<ServerCertVerified, rustls::Error> {
            let fp = Sha256::digest(ee.as_ref()).iter().map(|x| format!("{x:02X}")).collect::<Vec<_>>().join(":");
            if fp == self.0 { Ok(ServerCertVerified::assertion()) } else { Err(rustls::Error::General("huella distinta".into())) }
        }
        fn verify_tls12_signature(&self, m: &[u8], c: &CertificateDer<'_>, d: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, rustls::Error> {
            rustls::crypto::verify_tls12_signature(m, c, d, &self.1.signature_verification_algorithms)
        }
        fn verify_tls13_signature(&self, m: &[u8], c: &CertificateDer<'_>, d: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, rustls::Error> {
            rustls::crypto::verify_tls13_signature(m, c, d, &self.1.signature_verification_algorithms)
        }
        fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
            self.1.signature_verification_algorithms.supported_schemes()
        }
    }

    fn tls_get(s: &Srv, fp: &str, req: &[u8]) -> Result<Resp, String> {
        let prov = Arc::new(rustls::crypto::ring::default_provider());
        let cfg = rustls::ClientConfig::builder_with_provider(prov.clone())
            .with_safe_default_protocol_versions()
            .map_err(|e| e.to_string())?
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(Pin(fp.to_string(), prov)))
            .with_no_client_auth();
        let conn = rustls::ClientConnection::new(Arc::new(cfg), ServerName::try_from("localhost").unwrap()).map_err(|e| e.to_string())?;
        let tcp = TcpStream::connect(("127.0.0.1", s.port)).map_err(|e| e.to_string())?;
        let _ = tcp.set_read_timeout(Some(Duration::from_secs(20)));
        let mut t = rustls::StreamOwned::new(conn, tcp);
        t.write_all(req).map_err(|e| e.to_string())?;
        let mut out = Vec::new();
        t.read_to_end(&mut out).map_err(|e| e.to_string())?; // sin close_notify sería un error
        Ok(parse_resp(&out))
    }

    #[test]
    fn https_con_certificado_autofirmado() {
        let s = start(Opts { tls: true, auth: true, ..Default::default() });
        let fp = s.fp.clone().unwrap();
        let req = build(&s, "GET", &format!("/s/{TOK}/raiz/a.txt"), &[("Authorization", basic("ana", "contraseña-larga"))], b"");
        let r = tls_get(&s, &fp, &req).unwrap();
        assert_eq!((r.code, r.text().as_str()), (200, "hola mundo"));
        assert!(r.header("Set-Cookie").unwrap().contains("Secure"));
        // otra huella: el cliente rechaza el certificado
        assert!(tls_get(&s, "00:11", &req).is_err());
        // HTTP en claro contra el puerto TLS no recibe nada servible
        assert_ne!(raw(&s, &req).code, 200);
    }
}

/// Generador determinista (xorshift64*): misma semilla, mismas mutaciones en cada ejecución.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

fn mutate(r: &mut Rng, seed: &[u8]) -> Vec<u8> {
    const SPICE: &[&[u8]] = &[b"\r\n", b"\r", b"\n", b":", b" ", b"%", b"%2e%2e", b"%00", b"%ff", b"/", b"..", b"\\", b"?", b"&", b"=", b"\xc3", b"\xe2\x80\xae", b"bytes=", b"-", b",", b"Basic ", b"999999999999999999999999"];
    let mut v = seed.to_vec();
    for _ in 0..1 + r.below(8) {
        let at = r.below(v.len() + 1);
        match r.below(6) {
            0 if !v.is_empty() => {
                let i = r.below(v.len());
                v[i] = r.next() as u8;
            }
            1 if !v.is_empty() => {
                let end = (at + r.below(16)).min(v.len());
                v.drain(at.min(end)..end);
            }
            2 => {
                let s = SPICE[r.below(SPICE.len())];
                v.splice(at..at, s.iter().copied());
            }
            3 if !v.is_empty() => {
                // duplica un trozo
                let a = r.below(v.len());
                let b = (a + r.below(32)).min(v.len());
                let chunk = v[a..b].to_vec();
                v.splice(at..at, chunk);
            }
            _ => v.insert(at, r.next() as u8),
        }
    }
    v
}

#[test]
fn analizador_sin_panicos_con_mutaciones() {
    let seeds: &[&[u8]] = &[
        b"GET /s/tok/raiz/a.txt?zip=1&q=a+b HTTP/1.1\r\nHost: a\r\nRange: bytes=0-9\r\nIf-None-Match: \"x\"\r\nCookie: fbs=1; a=b",
        b"PUT /s/tok/raiz/%C3%B1.txt HTTP/1.1\r\nHost: 127.0.0.1:80\r\nContent-Length: 5\r\nOrigin: http://127.0.0.1:80\r\nX-FB: 1\r\nAuthorization: Basic YW5hOnMzY3JldDpwYXNz",
        b"POST /s/tok/api/op?op=mv&p=raiz/a&to=raiz/b HTTP/1.0\r\nContent-Length: 0",
        b"HEAD /s/tok/raiz/sub/ HTTP/1.1\r\nRange: bytes=-5",
    ];
    let creds = Creds::new("ana".into(), "s3cret:pass".into());
    let mut r = Rng(0x5eed_f11e_b405_e7);
    for i in 0..30_000 {
        let m = mutate(&mut r, seeds[i % seeds.len()]);
        if let Ok(req) = crate::http::parse(&m) {
            let _ = crate::jail::split_url(&req.path);
            let _ = crate::server::query_get(&req.query, "p").map(|p| crate::server::virt_path(&p));
            let _ = crate::http::parse_range(req.range.as_deref(), 1 + r.next() % 1000);
            let _ = creds.check(req.auth.as_deref());
        }
        let txt = String::from_utf8_lossy(&m);
        let _ = crate::jail::pct_decode(&txt);
        let _ = crate::http::parse_range(Some(&txt), r.next());
    }
}

#[test]
fn servidor_vivo_tras_peticiones_mutadas() {
    let s = start(Opts { write: true, auth: true, ..Default::default() });
    let seed = build(&s, "GET", &format!("/s/{TOK}/raiz/a.txt"), &[("Range", "bytes=0-3".into())], b"");
    let mut r = Rng(42);
    for _ in 0..300 {
        let m = mutate(&mut r, &seed);
        // se cierra el envío: si la mutación se comió el fin de cabeceras, el servidor lo ve enseguida
        let mut t = TcpStream::connect(("127.0.0.1", s.port)).unwrap();
        let _ = t.set_read_timeout(Some(Duration::from_secs(20)));
        let _ = t.write_all(&m);
        let _ = t.shutdown(std::net::Shutdown::Write);
        let mut out = Vec::new();
        let _ = t.read_to_end(&mut out);
        let resp = parse_resp(&out);
        // respuesta HTTP bien formada o cierre, nunca un cuelgue
        assert!(resp.code == 0 || (100..600).contains(&resp.code));
    }
    // el bloqueo por IP pudo saltar (tokens mutados); el servidor sigue respondiendo
    let r = get(&s, "/");
    assert_eq!(r.code, 404);
}

#[test]
fn idioma_de_las_paginas_y_mensajes_de_error() {
    let s = start(Opts::default());
    let en = [("Accept-Language", "en-GB,en;q=0.9".to_string())];
    let r = get_h(&s, &format!("/s/{TOK}/"), &en);
    assert!(r.text().contains("Shared folders") && r.text().contains("lang=\"en\""));
    assert!(get(&s, &format!("/s/{TOK}/")).text().contains("Carpetas compartidas"));
    let r = get_h(&s, &format!("/s/{TOK}/raiz/no-existe"), &en);
    assert_eq!(r.code, 404);
    assert!(r.header("Content-Type").unwrap().starts_with("text/html") && r.text().contains("Not found"));
    // 429 con un mensaje claro
    for i in 0..20 {
        get(&s, &format!("/s/mal{i}/"));
    }
    let r = get(&s, "/s/otro/");
    assert_eq!(r.code, 429);
    assert!(r.text().contains("espera un minuto"), "{}", r.text());
    assert_eq!(r.header("Retry-After").as_deref(), Some("60"));
}

#[cfg(unix)]
#[test]
fn zip_y_busqueda_dicen_lo_que_omiten() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let s = start(Opts::default());
    let r = &s.dir.join("raiz");
    symlink(r.join("a.txt"), r.join("enlace.txt")).unwrap();
    symlink(r.join("sub"), r.join("enlace-dir")).unwrap();
    symlink(s.dir.join("fuera/secreto.txt"), r.join("sale.txt")).unwrap();
    std::fs::write(r.join("cerrado.txt"), "x").unwrap();
    std::fs::set_permissions(r.join("cerrado.txt"), std::fs::Permissions::from_mode(0o000)).unwrap();
    let ilegible = std::fs::File::open(r.join("cerrado.txt")).is_err();
    let z = get(&s, &format!("/s/{TOK}/raiz/?zip=1"));
    assert_eq!(z.code, 200);
    let mut ar = zip::ZipArchive::new(std::io::Cursor::new(z.body)).unwrap();
    // el enlace a un archivo de dentro va con su contenido
    let mut txt = String::new();
    ar.by_name("raiz/enlace.txt").unwrap().read_to_string(&mut txt).unwrap();
    assert_eq!(txt, "hola mundo");
    assert!(ar.by_name("raiz/sale.txt").is_err());
    let mut nota = String::new();
    ar.by_name("raiz/OMITIDOS.txt").unwrap().read_to_string(&mut nota).unwrap();
    assert!(nota.contains("enlace-dir") && nota.contains("sale.txt") && !nota.contains("secreto"), "{nota}");
    if ilegible {
        assert!(nota.contains("cerrado.txt"), "{nota}");
    }
    // la búsqueda encuentra los enlaces de dentro, como el listado
    let t = get(&s, &format!("/s/{TOK}/raiz/?q=enlace")).text();
    assert!(t.contains("enlace.txt") && t.contains("enlace-dir/"), "{t}");
    assert!(!get(&s, &format!("/s/{TOK}/raiz/?q=sale")).text().contains("sale.txt</a>"));
    std::fs::set_permissions(r.join("cerrado.txt"), std::fs::Permissions::from_mode(0o644)).unwrap();
}

#[test]
fn carpeta_enorme_se_ordena_antes_de_recortar() {
    let s = start(Opts::default());
    let d = s.dir.join("raiz/mucho");
    std::fs::create_dir_all(&d).unwrap();
    // 20 050 entradas: el recorte se queda con las primeras por nombre, no por el orden del disco
    for i in (0..20_050).rev() {
        std::fs::File::create(d.join(format!("f{i:05}"))).unwrap();
    }
    let t = get(&s, &format!("/s/{TOK}/api/list?p=raiz/mucho")).text();
    assert!(t.contains("\"truncated\":true"));
    assert!(t.contains("\"f00000\"") && t.contains("\"f19999\"") && !t.contains("\"f20000\""));
    let t = get(&s, &format!("/s/{TOK}/raiz/mucho/?sort=name&o=d")).text();
    assert!(t.contains(">f20049<") && !t.contains(">f00000<"));
}

#[test]
fn con_proxy_los_bloqueos_usan_x_forwarded_for() {
    let s = start(Opts { proxy: true, ..Default::default() });
    let de = |ip: &str| [("X-Forwarded-For", format!("203.0.113.9, {ip}"))];
    for i in 0..20 {
        get_h(&s, &format!("/s/mal{i}/"), &de("198.51.100.1"));
    }
    assert_eq!(get_h(&s, "/s/otro/", &de("198.51.100.1")).code, 429);
    // otro cliente tras el mismo túnel no queda bloqueado
    assert_eq!(get_h(&s, "/s/otro/", &de("198.51.100.2")).code, 404);
    // sin --proxy la cabecera no cuenta
    let s2 = start(Opts::default());
    for i in 0..20 {
        get_h(&s2, &format!("/s/mal{i}/"), &de("198.51.100.1"));
    }
    assert_eq!(get_h(&s2, "/s/otro/", &de("198.51.100.2")).code, 429);
}
