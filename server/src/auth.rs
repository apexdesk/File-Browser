//! Usuario y contraseña opcionales (`--user`): HTTP Basic además del token del enlace.
use std::io::{self, BufRead, Write};

pub struct Creds {
    user: String,
    pass: String,
}

impl Creds {
    pub fn new(user: String, pass: String) -> Creds {
        Creds { user, pass }
    }

    /// ¿Es válida la cabecera `Authorization`? Compara en tiempo constante.
    pub fn check(&self, header: Option<&str>) -> bool {
        let Some(h) = header else { return false };
        let Some(b64) = h.strip_prefix("Basic ").or_else(|| h.strip_prefix("basic ")) else { return false };
        let Some(raw) = b64_decode(b64.trim()) else { return false };
        let Ok(raw) = String::from_utf8(raw) else { return false };
        let Some((u, p)) = raw.split_once(':') else { return false };
        let a = crate::server::ct_eq(u, &self.user);
        let b = crate::server::ct_eq(p, &self.pass);
        a & b
    }
}

fn b64_decode(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len() / 4 * 3);
    let (mut acc, mut bits) = (0u32, 0u32);
    for c in s.trim_end_matches('=').bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        };
        acc = (acc << 6) | u32::from(v);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Some(out)
}

/// Contraseña: variable `FB_PASSWORD`, o archivo (`--password-file`), o se pregunta sin eco.
/// Nunca por la línea de órdenes (se vería en `ps`).
pub fn read_password(file: Option<&str>) -> Result<String, String> {
    let p = if let Some(f) = file {
        std::fs::read_to_string(f).map_err(|e| format!("{f}: {e}"))?
    } else if let Ok(v) = std::env::var("FB_PASSWORD") {
        v
    } else {
        prompt()?
    };
    let p = p.trim_end_matches(['\r', '\n']).to_string();
    if p.len() < 8 {
        return Err("la contraseña debe tener al menos 8 caracteres".into());
    }
    Ok(p)
}

fn prompt() -> Result<String, String> {
    eprint!("Contraseña para entrar por web: ");
    let _ = io::stderr().flush();
    #[cfg(unix)]
    let _echo = NoEcho::new();
    let mut l = String::new();
    io::stdin().lock().read_line(&mut l).map_err(|e| e.to_string())?;
    eprintln!();
    Ok(l)
}

#[cfg(unix)]
struct NoEcho(Option<libc::termios>);

#[cfg(unix)]
impl NoEcho {
    fn new() -> NoEcho {
        // SAFETY: termios sobre la entrada estándar; si no es un terminal, falla y no se toca nada
        unsafe {
            let mut t: libc::termios = std::mem::zeroed();
            if libc::tcgetattr(0, &mut t) != 0 {
                return NoEcho(None);
            }
            let old = t;
            t.c_lflag &= !libc::ECHO;
            libc::tcsetattr(0, libc::TCSANOW, &t);
            NoEcho(Some(old))
        }
    }
}

#[cfg(unix)]
impl Drop for NoEcho {
    fn drop(&mut self) {
        if let Some(t) = &self.0 {
            // SAFETY: restaura lo guardado en `new`
            unsafe { libc::tcsetattr(0, libc::TCSANOW, t) };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic() {
        let c = Creds::new("ana".into(), "s3cret:pass".into());
        // ana:s3cret:pass
        assert!(c.check(Some("Basic YW5hOnMzY3JldDpwYXNz")));
        assert!(!c.check(Some("Basic YW5hOnMzY3JldA==")));
        assert!(!c.check(Some("Bearer x")));
        assert!(!c.check(None));
    }
}
