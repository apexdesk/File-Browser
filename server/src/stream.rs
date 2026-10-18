//! Conexión de cliente: TCP en claro o, con `--tls`, TCP bajo rustls.
//! El socket lleva un plazo total opcional que se aplica en cada lectura: así ni el
//! handshake TLS ni las cabeceras pueden alargarse enviando un byte cada pocos segundos.

use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::time::{Duration, Instant};

pub struct Sock {
    pub tcp: TcpStream,
    deadline: Option<Instant>,
}

impl Read for Sock {
    fn read(&mut self, b: &mut [u8]) -> io::Result<usize> {
        if let Some(d) = self.deadline {
            let left = d.checked_duration_since(Instant::now()).filter(|l| !l.is_zero());
            let Some(left) = left else { return Err(io::ErrorKind::TimedOut.into()) };
            self.tcp.set_read_timeout(Some(left))?;
        }
        self.tcp.read(b)
    }
}

impl Write for Sock {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        self.tcp.write(b)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.tcp.flush()
    }
}

pub enum Stream {
    Plain(Sock),
    #[cfg(feature = "tls")]
    Tls(Box<rustls::StreamOwned<rustls::ServerConnection, Sock>>),
}

impl Stream {
    pub fn plain(tcp: TcpStream) -> Self {
        Stream::Plain(Sock { tcp, deadline: None })
    }

    #[cfg(feature = "tls")]
    pub fn tls(conn: rustls::ServerConnection, tcp: TcpStream) -> Self {
        Stream::Tls(Box::new(rustls::StreamOwned::new(conn, Sock { tcp, deadline: None })))
    }

    fn sock(&mut self) -> &mut Sock {
        match self {
            Stream::Plain(s) => s,
            #[cfg(feature = "tls")]
            Stream::Tls(s) => &mut s.sock,
        }
    }

    /// Plazo total para las lecturas siguientes (`None`: sin plazo, vale el tiempo de espera normal).
    pub fn set_deadline(&mut self, d: Option<Duration>) {
        let s = self.sock();
        s.deadline = d.map(|d| Instant::now() + d);
        if d.is_none() {
            let _ = s.tcp.set_read_timeout(None);
        }
    }

    /// Cierre limpio: en TLS avisa con `close_notify` para que el cliente sepa que no se cortó.
    pub fn finish(&mut self) {
        #[cfg(feature = "tls")]
        if let Stream::Tls(s) = self {
            s.conn.send_close_notify();
            let _ = s.flush();
        }
        let _ = self.sock().tcp.shutdown(std::net::Shutdown::Write);
    }
}

impl Read for Stream {
    fn read(&mut self, b: &mut [u8]) -> io::Result<usize> {
        match self {
            Stream::Plain(s) => s.read(b),
            #[cfg(feature = "tls")]
            Stream::Tls(s) => s.read(b),
        }
    }
}

impl Write for Stream {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        match self {
            Stream::Plain(s) => s.write(b),
            #[cfg(feature = "tls")]
            Stream::Tls(s) => s.write(b),
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        match self {
            Stream::Plain(s) => s.flush(),
            #[cfg(feature = "tls")]
            Stream::Tls(s) => s.flush(),
        }
    }
}
