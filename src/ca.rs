//! Per-machine certificate authority, mkcert-style.
//!
//! Created once and persisted; every share's leaf certificate is minted
//! from it. Trust the CA once per device (see `lclhst trust` and the
//! `/.lclhst/` onboarding page) and every share gets a clean padlock.

use std::net::IpAddr;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, ensure};
use rcgen::{
    BasicConstraints, CertificateParams, DistinguishedName, DnType, ExtendedKeyUsagePurpose, IsCa,
    Issuer, KeyPair, KeyUsagePurpose, SanType,
};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject};
use time::{Duration, OffsetDateTime};

use crate::protocol::valid_name;

const CA_CERT_FILE: &str = "rootCA.pem";
const CA_KEY_FILE: &str = "rootCA-key.pem";
/// Apple rejects trusted leaf certificates valid for longer than 825 days.
const LEAF_VALIDITY_DAYS: i64 = 398;
const CA_VALIDITY_DAYS: i64 = 365 * 10;

pub struct Ca {
    issuer: Issuer<'static, KeyPair>,
    cert_pem: String,
}

impl Ca {
    /// Where the CA lives by default: `$XDG_CONFIG_HOME/lclhst` or
    /// `~/.config/lclhst`.
    pub fn default_dir() -> PathBuf {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::home_dir().map(|h| h.join(".config")))
            .unwrap_or_else(|| PathBuf::from("."))
            .join("lclhst")
    }

    /// Load the persisted CA from `dir`, creating (and persisting) it on
    /// first use.
    pub fn load_or_create(dir: &Path) -> Result<Ca> {
        let cert_path = dir.join(CA_CERT_FILE);
        let key_path = dir.join(CA_KEY_FILE);
        if cert_path.exists() && key_path.exists() {
            let cert_pem = std::fs::read_to_string(&cert_path)
                .with_context(|| format!("reading {}", cert_path.display()))?;
            let key_pem = std::fs::read_to_string(&key_path)
                .with_context(|| format!("reading {}", key_path.display()))?;
            let key = KeyPair::from_pem(&key_pem).context("parsing CA key")?;
            let issuer = Issuer::from_ca_cert_pem(&cert_pem, key).context("parsing CA cert")?;
            return Ok(Ca { issuer, cert_pem });
        }

        let host = hostname();
        let mut params = CertificateParams::default();
        params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        params.key_usages = vec![
            KeyUsagePurpose::KeyCertSign,
            KeyUsagePurpose::CrlSign,
            KeyUsagePurpose::DigitalSignature,
        ];
        let mut dn = DistinguishedName::new();
        dn.push(DnType::CommonName, format!("lclhst CA ({host})"));
        dn.push(DnType::OrganizationName, "lclhst");
        params.distinguished_name = dn;
        params.not_before = OffsetDateTime::now_utc() - Duration::days(1);
        params.not_after = OffsetDateTime::now_utc() + Duration::days(CA_VALIDITY_DAYS);

        let key = KeyPair::generate().context("generating CA key")?;
        let cert = params
            .self_signed(&key)
            .context("self-signing CA certificate")?;
        let cert_pem = cert.pem();

        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        std::fs::write(&cert_path, &cert_pem)
            .with_context(|| format!("writing {}", cert_path.display()))?;
        write_private(&key_path, key.serialize_pem().as_bytes())?;

        let issuer = Issuer::new(params, key);
        Ok(Ca { issuer, cert_pem })
    }

    /// The CA certificate in PEM form — what devices install to trust us.
    pub fn cert_pem(&self) -> &str {
        &self.cert_pem
    }

    /// Mint a leaf certificate for `<name>.localhost` / `<name>.local` /
    /// `localhost` plus the given IPs, and build a rustls server config
    /// from it.
    pub fn server_config(&self, name: &str, ips: &[IpAddr]) -> Result<rustls::ServerConfig> {
        ensure!(valid_name(name), "invalid tunnel name: {name:?}");
        let mut params = CertificateParams::new(vec![
            format!("{name}.localhost"),
            format!("{name}.local"),
            "localhost".to_string(),
        ])
        .context("building leaf params")?;
        for ip in ips {
            params.subject_alt_names.push(SanType::IpAddress(*ip));
        }
        params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
        params.key_usages = vec![
            KeyUsagePurpose::DigitalSignature,
            KeyUsagePurpose::KeyEncipherment,
        ];
        let mut dn = DistinguishedName::new();
        dn.push(DnType::CommonName, format!("{name}.local"));
        params.distinguished_name = dn;
        params.not_before = OffsetDateTime::now_utc() - Duration::days(1);
        params.not_after = OffsetDateTime::now_utc() + Duration::days(LEAF_VALIDITY_DAYS);

        let leaf_key = KeyPair::generate().context("generating leaf key")?;
        let leaf = params
            .signed_by(&leaf_key, &self.issuer)
            .context("signing leaf certificate")?;

        let chain = vec![
            leaf.der().clone(),
            CertificateDer::from_pem_slice(self.cert_pem.as_bytes())
                .context("re-encoding CA cert")?,
        ];
        let key = PrivateKeyDer::Pkcs8(leaf_key.serialize_der().into());
        let mut cfg = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(chain, key)?;
        cfg.alpn_protocols = vec![b"http/1.1".to_vec()];
        Ok(cfg)
    }
}

fn hostname() -> String {
    std::process::Command::new("hostname")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown-host".to_string())
}

/// Write a key file with owner-only permissions.
fn write_private(path: &Path, contents: &[u8]) -> Result<()> {
    std::fs::write(path, contents).with_context(|| format!("writing {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tempdir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("lclhst-ca-test-{tag}-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        dir
    }

    #[test]
    fn creates_then_reuses_the_same_ca() {
        let dir = tempdir("reuse");
        let a = Ca::load_or_create(&dir).unwrap();
        let b = Ca::load_or_create(&dir).unwrap();
        assert_eq!(
            a.cert_pem(),
            b.cert_pem(),
            "second load must reuse, not regenerate"
        );
        assert!(dir.join("rootCA.pem").exists());
        assert!(dir.join("rootCA-key.pem").exists());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn ca_pem_looks_like_a_certificate() {
        let dir = tempdir("pem");
        let ca = Ca::load_or_create(&dir).unwrap();
        assert!(ca.cert_pem().starts_with("-----BEGIN CERTIFICATE-----"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn mints_a_server_config_with_ips() {
        let dir = tempdir("mint");
        let ca = Ca::load_or_create(&dir).unwrap();
        let ips = vec!["192.168.1.10".parse().unwrap()];
        let cfg = ca.server_config("myapp", &ips).unwrap();
        assert_eq!(cfg.alpn_protocols, vec![b"http/1.1".to_vec()]);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn rejects_invalid_name() {
        let dir = tempdir("badname");
        let ca = Ca::load_or_create(&dir).unwrap();
        assert!(ca.server_config("Bad.Name", &[]).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }
}
