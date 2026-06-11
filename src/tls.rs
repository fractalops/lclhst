//! TLS for the edge: a self-signed cert covering `<name>.localhost` (this
//! machine) and `<name>.local` (the LAN, resolved via mDNS). A local CA with
//! trust installation replaces this in a later milestone.

use anyhow::{Result, ensure};
use rustls::pki_types::PrivateKeyDer;

use crate::protocol::valid_name;

pub fn server_config(name: &str) -> Result<rustls::ServerConfig> {
    ensure!(valid_name(name), "invalid tunnel name: {name:?}");
    let sans = vec![
        format!("{name}.localhost"),
        format!("{name}.local"),
        "localhost".to_string(),
    ];
    let ck = rcgen::generate_simple_self_signed(sans)?;
    let cert = ck.cert.der().clone();
    let key = PrivateKeyDer::Pkcs8(ck.signing_key.serialize_der().into());
    let mut cfg = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![cert], key)?;
    cfg.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(cfg)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_a_server_config_for_the_name() {
        let cfg = server_config("myapp").unwrap();
        assert_eq!(cfg.alpn_protocols, vec![b"http/1.1".to_vec()]);
    }

    #[test]
    fn rejects_invalid_name() {
        assert!(server_config("Bad.Name").is_err());
    }
}
