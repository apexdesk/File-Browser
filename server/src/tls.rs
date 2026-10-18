//! HTTPS opcional: certificado autofirmado generado en memoria al arrancar (30 días).
//! El navegador avisará de que no lo conoce; la huella SHA-256 impresa en consola permite comprobarlo.

#[cfg(feature = "tls")]
mod imp {
    use sha2::{Digest, Sha256};
    use std::net::IpAddr;
    use std::sync::Arc;

    pub type Config = Arc<rustls::ServerConfig>;

    pub fn make(ips: &[IpAddr]) -> Result<(Config, String), String> {
        let mut sans: Vec<String> = vec!["localhost".into()];
        sans.extend(ips.iter().map(|i| i.to_string()));
        let mut params = rcgen::CertificateParams::new(sans).map_err(|e| e.to_string())?;
        let now = crate::timefmt::unix(std::time::SystemTime::now());
        let (a, b) = (crate::timefmt::civil(now - 86_400), crate::timefmt::civil(now + 30 * 86_400));
        params.not_before = rcgen::date_time_ymd(a.year as i32, a.month as u8, a.day as u8);
        params.not_after = rcgen::date_time_ymd(b.year as i32, b.month as u8, b.day as u8);
        let key = rcgen::KeyPair::generate().map_err(|e| e.to_string())?;
        let cert = params.self_signed(&key).map_err(|e| e.to_string())?;
        let der = cert.der().clone();
        let fp = Sha256::digest(der.as_ref()).iter().map(|x| format!("{x:02X}")).collect::<Vec<_>>().join(":");
        let key_der = rustls::pki_types::PrivateKeyDer::Pkcs8(rustls::pki_types::PrivatePkcs8KeyDer::from(key.serialize_der()));
        let cfg = rustls::ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()
            .map_err(|e| e.to_string())?
            .with_no_client_auth()
            .with_single_cert(vec![der], key_der)
            .map_err(|e| e.to_string())?;
        Ok((Arc::new(cfg), fp))
    }
}

#[cfg(not(feature = "tls"))]
mod imp {
    use std::net::IpAddr;

    /// Sin soporte TLS compilado: tipo sin valores.
    pub enum Config {}

    pub fn make(_: &[IpAddr]) -> Result<(Config, String), String> {
        Err("este binario se compiló sin HTTPS (característica «tls»)".into())
    }
}

pub use imp::{Config, make};
