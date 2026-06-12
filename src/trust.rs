//! Install the lclhst CA into this machine's trust store, mkcert-style.

use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, bail};

/// Install the CA at `ca_cert_path` into the system trust store.
/// Elevates via sudo where the platform store requires it.
pub fn install(ca_cert_path: &Path) -> Result<()> {
    let path = ca_cert_path
        .to_str()
        .context("CA path is not valid UTF-8")?;

    if cfg!(target_os = "macos") {
        eprintln!("adding the lclhst CA to the system keychain (sudo will prompt)…");
        let status = Command::new("sudo")
            .args([
                "security",
                "add-trusted-cert",
                "-d",
                "-r",
                "trustRoot",
                "-k",
                "/Library/Keychains/System.keychain",
                path,
            ])
            .status()
            .context("running `security` — is this macOS?")?;
        if !status.success() {
            bail!("security add-trusted-cert failed (status {status})");
        }
        eprintln!("done. Safari and Chrome trust lclhst shares from this machine now.");
        eprintln!(
            "Firefox keeps its own store: settings → privacy → certificates → import {path},"
        );
        eprintln!("or set security.enterprise_roots.enabled=true in about:config.");
        Ok(())
    } else if cfg!(target_os = "linux") {
        eprintln!("installing the lclhst CA system-wide (sudo will prompt)…");
        let status = Command::new("sudo")
            .args(["cp", path, "/usr/local/share/ca-certificates/lclhst-ca.crt"])
            .status()
            .context("copying the CA certificate")?;
        if !status.success() {
            bail!("could not copy the CA into /usr/local/share/ca-certificates");
        }
        let status = Command::new("sudo")
            .arg("update-ca-certificates")
            .status()
            .context("running update-ca-certificates (Debian/Ubuntu layout)")?;
        if !status.success() {
            bail!(
                "update-ca-certificates failed — on Fedora/Arch use the trust anchor \
                 tooling with {path}"
            );
        }
        eprintln!("done. Note: browsers with their own stores (Firefox, Chrome's NSS)");
        eprintln!("may need a manual import of {path}.");
        Ok(())
    } else {
        bail!(
            "automatic trust install isn't supported on this platform yet; \
             import {path} into your trust store manually"
        );
    }
}
