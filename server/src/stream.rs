//! Conexión de cliente: TCP en claro o, con `--tls`, TCP bajo rustls.

use std::io::{self, Read, Write};
use std::net::TcpStream;

pub enum Stream {
    Plain(TcpStream),
    #[cfg(feature = "tls")]
    Tls(Box<rustls::StreamOwned<rustls::ServerConnection, TcpStream>>),
}

impl Stream {
    /// El socket subyacente (tiempos de espera, dirección del cliente).
    pub fn tcp(&self) -> &TcpStream {
        match self {
            Stream::Plain(s) => s,
            #[cfg(feature = "tls")]
            Stream::Tls(s) => &s.sock,
        }
    }
}

impl Stream {
    /// Cierre limpio: en TLS avisa con `close_notify` para que el cliente sepa que no se cortó.
    pub fn finish(&mut self) {
        #[cfg(feature = "tls")]
        if let Stream::Tls(s) = self {
            s.conn.send_close_notify();
            let _ = s.flush();
        }
        let _ = self.tcp().shutdown(std::net::Shutdown::Write);
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
