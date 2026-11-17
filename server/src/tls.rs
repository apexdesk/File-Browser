//! HTTPS opcional: certificado autofirmado generado en memoria al arrancar (`--ttl` + 1 día, o
//! `MAX_DAYS` sin caducidad; ver `days_for`).
//! El navegador avisará de que no lo conoce; la huella SHA-256 impresa en consola permite comprobarlo.

/// Días de validez como mucho: algunos sistemas (Apple) rechazan certificados de más de 825 días y los
/// navegadores aplican 398 a los públicos; se queda por debajo.
pub const MAX_DAYS: u64 = 397;

/// Validez del certificado para un `--ttl`: lo que dura el enlace y un día de margen; sin `--ttl`, `MAX_DAYS`.
pub fn days_for(ttl: Option<std::time::Duration>) -> u64 {
    ttl.map(|t| t.as_secs().div_ceil(86_400) + 1).unwrap_or(MAX_DAYS).min(MAX_DAYS)
}

#[cfg(feature = "tls")]
mod imp {
    use sha2::{Digest, Sha256};
    use std::net::IpAddr;
    use std::sync::Arc;

    pub type Config = Arc<rustls::ServerConfig>;

    #[cfg(test)]
    pub fn make(ips: &[IpAddr]) -> Result<(Config, String), String> {
        make_days(ips, 30)
    }

    /// Certificado válido desde ayer hasta dentro de `days` días.
    pub fn make_days(ips: &[IpAddr], days: u64) -> Result<(Config, String), String> {
        let mut sans: Vec<String> = vec!["localhost".into()];
        sans.extend(ips.iter().map(|i| i.to_string()));
        let mut params = rcgen::CertificateParams::new(sans).map_err(|e| e.to_string())?;
        let now = crate::timefmt::unix(std::time::SystemTime::now());
        let (a, b) = (crate::timefmt::civil(now - 86_400), crate::timefmt::civil(now + days as i64 * 86_400));
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

    #[cfg(test)]
    pub fn make(ips: &[IpAddr]) -> Result<(Config, String), String> {
        make_days(ips, 30)
    }

    pub fn make_days(_: &[IpAddr], _: u64) -> Result<(Config, String), String> {
        Err("este binario se compiló sin HTTPS (característica «tls»)".into())
    }
}

#[cfg(test)]
pub use imp::make;
pub use imp::{Config, make_days};

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn validez_segun_ttl() {
        assert_eq!(days_for(None), MAX_DAYS);
        assert_eq!(days_for(Some(Duration::from_secs(3600))), 2);
        assert_eq!(days_for(Some(Duration::from_secs(86_400))), 2);
        assert_eq!(days_for(Some(Duration::from_secs(90 * 86_400))), 91);
        assert_eq!(days_for(Some(Duration::from_secs(5000 * 86_400))), MAX_DAYS);
    }
}
