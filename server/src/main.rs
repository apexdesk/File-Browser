//! filebrowser-server: comparte carpetas por HTTP, solo lectura, con enlace secreto.
//! Proyecto aparte del gestor `filebrowser`; no comparte binario con él.

mod auth;
mod http;
mod jail;
mod mime;
mod page;
mod server;
mod stream;
mod tls;
mod timefmt;
mod walk;
mod webui;
mod write;
mod zipstream;

use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener, UdpSocket};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

const USO: &str = "\
filebrowser-server — comparte carpetas por HTTP (por defecto, solo lectura)

Uso: filebrowser-server [opciones] CARPETA [CARPETA…]

  --bind DIR     dirección de escucha (por defecto 127.0.0.1; usa 0.0.0.0 para la red local)
  --port N       puerto (por defecto: uno libre elegido por el sistema)
  --ttl TIEMPO   caduca pasado ese tiempo: 30s, 90m, 2h, 1d (por defecto: sin caducidad)
  --max-conn N   conexiones simultáneas máximas (por defecto 32)
  --tls          HTTPS con certificado autofirmado (el navegador avisará; se imprime la huella)
  --write        deja también subir, borrar, mover y copiar (por defecto, solo lectura)
  --max-upload N tamaño máximo de un archivo subido, en MiB (por defecto 2048)
  --user NOMBRE  pide usuario y contraseña además del enlace (la contraseña: variable FB_PASSWORD,
                 --password-file ARCHIVO o se pregunta al arrancar; mínimo 8 caracteres)
  --password-file ARCHIVO  lee la contraseña de ese archivo
  --hidden       sirve también archivos y carpetas que empiezan por «.» (por defecto se ocultan)
  --i-know       permite ejecutarlo como root/administrador
  -h, --help     esta ayuda

El enlace incluye un token aleatorio que solo existe mientras el programa corre.
No hay HTTPS: úsalo en redes de confianza o a través de un túnel (WireGuard, Tailscale…).";

struct Opts {
    dirs: Vec<PathBuf>,
    bind: IpAddr,
    port: u16,
    ttl: Option<Duration>,
    max_conn: usize,
    hidden: bool,
    tls: bool,
    i_know: bool,
    write: bool,
    max_upload_mib: u64,
    user: Option<String>,
    password_file: Option<String>,
}

fn parse_ttl(s: &str) -> Option<Duration> {
    let (n, u) = s.split_at(s.len().checked_sub(1)?);
    let n: u64 = n.parse().ok()?;
    let mult = match u {
        "s" => 1,
        "m" => 60,
        "h" => 3600,
        "d" => 86_400,
        _ => return None,
    };
    n.checked_mul(mult).filter(|v| *v > 0).map(Duration::from_secs)
}

fn parse_args(args: Vec<String>) -> Result<Opts, String> {
    let mut o = Opts { dirs: Vec::new(), bind: IpAddr::V4(Ipv4Addr::LOCALHOST), port: 0, ttl: None, max_conn: 32, hidden: false, tls: false, i_know: false, write: false, max_upload_mib: 2048, user: None, password_file: None };
    let mut it = args.into_iter();
    while let Some(a) = it.next() {
        let mut val = |name: &str| it.next().ok_or_else(|| format!("{name} necesita un valor"));
        match a.as_str() {
            "-h" | "--help" => return Err(String::new()),
            "--bind" => o.bind = val("--bind")?.parse().map_err(|_| "--bind: dirección IP no válida".to_string())?,
            "--port" => o.port = val("--port")?.parse().map_err(|_| "--port: número no válido".to_string())?,
            "--ttl" => o.ttl = Some(parse_ttl(&val("--ttl")?).ok_or("--ttl: usa por ejemplo 30s, 90m, 2h, 1d")?),
            "--max-conn" => {
                o.max_conn = val("--max-conn")?.parse().ok().filter(|n| (1..=1024).contains(n)).ok_or("--max-conn: entre 1 y 1024")?
            }
            "--hidden" => o.hidden = true,
            "--write" => o.write = true,
            "--max-upload" => o.max_upload_mib = val("--max-upload")?.parse().ok().filter(|n| *n >= 1).ok_or("--max-upload: MiB, al menos 1")?,
            "--user" => {
                let u = val("--user")?;
                if u.is_empty() || u.contains(':') {
                    return Err("--user: nombre no vacío y sin «:»".into());
                }
                o.user = Some(u);
            }
            "--password-file" => o.password_file = Some(val("--password-file")?),
            "--tls" => o.tls = true,
            "--i-know" => o.i_know = true,
            s if s.starts_with("--") => return Err(format!("opción desconocida: {s}")),
            _ => o.dirs.push(PathBuf::from(a)),
        }
    }
    if o.dirs.is_empty() {
        return Err("indica al menos una carpeta".into());
    }
    Ok(o)
}

fn is_root() -> bool {
    #[cfg(unix)]
    {
        // SAFETY: geteuid no tiene precondiciones.
        unsafe { libc::geteuid() == 0 }
    }
    #[cfg(not(unix))]
    {
        false
    }
}

/// IP de la interfaz de salida (no envía ningún paquete).
fn lan_ip() -> Option<IpAddr> {
    let s = UdpSocket::bind("0.0.0.0:0").ok()?;
    s.connect("192.0.2.1:9").ok()?;
    s.local_addr().ok().map(|a| a.ip())
}

fn token() -> Result<String, String> {
    let mut b = [0u8; 16];
    getrandom::fill(&mut b).map_err(|e| format!("sin fuente de aleatoriedad: {e}"))?;
    Ok(b.iter().map(|x| format!("{x:02x}")).collect())
}

fn make_roots(dirs: &[PathBuf]) -> Result<Vec<jail::Root>, String> {
    let mut roots: Vec<jail::Root> = Vec::new();
    for d in dirs {
        let canon = std::fs::canonicalize(d).map_err(|e| format!("{}: {e}", d.display()))?;
        if !canon.is_dir() {
            return Err(format!("{}: no es una carpeta", d.display()));
        }
        if canon.parent().is_none() && !cfg!(test) {
            return Err(format!("{}: no se comparte la raíz del sistema; elige una carpeta concreta", d.display()));
        }
        let base = canon.file_name().and_then(|n| n.to_str()).filter(|n| jail::valid_component(n)).unwrap_or("carpeta").to_string();
        let mut name = base.clone();
        let mut i = 2;
        while roots.iter().any(|r| r.name == name) {
            name = format!("{base}-{i}");
            i += 1;
        }
        roots.push(jail::Root { name, path: canon });
    }
    Ok(roots)
}

fn run() -> Result<(), String> {
    let o = match parse_args(std::env::args().skip(1).collect()) {
        Ok(o) => o,
        Err(m) => {
            if m.is_empty() {
                println!("{USO}");
                return Ok(());
            }
            return Err(format!("{m}\n\n{USO}"));
        }
    };
    if is_root() && !o.i_know {
        return Err("no se ejecuta como root/administrador (usa --i-know si de verdad lo quieres)".into());
    }
    let roots = make_roots(&o.dirs)?;
    let token = token()?;
    let auth = match &o.user {
        Some(u) => Some(auth::Creds::new(u.clone(), auth::read_password(o.password_file.as_deref())?)),
        None if o.password_file.is_some() => return Err("--password-file necesita --user".into()),
        None => None,
    };
    let listener = TcpListener::bind(SocketAddr::new(o.bind, o.port)).map_err(|e| format!("no se pudo abrir el puerto: {e}"))?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    let host = if o.bind.is_unspecified() { lan_ip().unwrap_or(IpAddr::V4(Ipv4Addr::LOCALHOST)) } else { o.bind };
    let host = match host {
        IpAddr::V6(a) => format!("[{a}]"),
        a => a.to_string(),
    };
    let (tls_cfg, fp) = if o.tls {
        let mut ips = vec![IpAddr::V4(Ipv4Addr::LOCALHOST)];
        for ip in std::iter::once(o.bind).chain(lan_ip()) {
            if !ip.is_unspecified() && !ips.contains(&ip) {
                ips.push(ip);
            }
        }
        let (c, fp) = tls::make(&ips)?;
        (Some(c), Some(fp))
    } else {
        (None, None)
    };
    let url = format!("{}://{host}:{port}/s/{token}/", if o.tls { "https" } else { "http" });

    println!("Compartiendo ({}):", if o.write { "LECTURA Y ESCRITURA: quien tenga el enlace puede subir, borrar y mover" } else { "solo lectura" });
    for r in &roots {
        println!("  {} → {}", r.name, r.path.display());
    }
    if !o.hidden {
        println!("Los archivos y carpetas que empiezan por «.» no se sirven (--hidden para incluirlos).");
    }
    println!("\n{url}\n");
    if let Ok(qr) = qrcode::QrCode::new(url.as_bytes()) {
        println!("{}", qr.render::<qrcode::render::unicode::Dense1x2>().quiet_zone(true).build());
    }
    if let Some(fp) = &fp {
        println!("HTTPS con certificado autofirmado (válido 30 días). Huella SHA-256:\n  {fp}");
    }
    if let Some(u) = &o.user {
        println!("Acceso con usuario «{u}» y contraseña, además del enlace.");
        if !o.bind.is_loopback() && !o.tls {
            println!("Aviso: sin HTTPS la contraseña viaja sin cifrar por la red; usa --tls o un túnel.");
        }
    }
    if !o.bind.is_loopback() && !o.tls {
        println!("Aviso: sin HTTPS. Cualquiera en esa red con el enlace puede leer lo compartido; para salir a internet usa un túnel.");
    }
    if let Some(t) = o.ttl {
        println!("El enlace caduca en {} s.", t.as_secs());
        std::thread::spawn(move || {
            std::thread::sleep(t);
            eprintln!("Tiempo cumplido: se cierra el servidor.");
            std::process::exit(0);
        });
    }
    println!("Ctrl+C para parar.");
    let mut ctx = server::Ctx::new(roots, token, o.hidden, o.write, o.max_upload_mib.saturating_mul(1 << 20), tls_cfg);
    ctx.auth = auth;
    server::serve(listener, Arc::new(ctx), o.max_conn);
    Ok(())
}

fn main() {
    if let Err(e) = run() {
        eprintln!("filebrowser-server: {e}");
        std::process::exit(2);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ttl() {
        assert_eq!(parse_ttl("90m"), Some(Duration::from_secs(5400)));
        assert_eq!(parse_ttl("2h"), Some(Duration::from_secs(7200)));
        assert!(parse_ttl("0s").is_none());
        assert!(parse_ttl("5").is_none());
        assert!(parse_ttl("").is_none());
        assert!(parse_ttl("99999999999999999999d").is_none());
    }

    #[test]
    fn argumentos() {
        let a = |v: &[&str]| parse_args(v.iter().map(|s| s.to_string()).collect());
        assert!(a(&[]).is_err());
        assert!(a(&["--port", "x", "d"]).is_err());
        assert!(a(&["--max-conn", "0", "d"]).is_err());
        assert!(a(&["--raro", "d"]).is_err());
        let o = a(&["--port", "80", "--hidden", "x", "y"]).ok().unwrap();
        assert_eq!((o.port, o.hidden, o.dirs.len()), (80, true, 2));
    }
}
