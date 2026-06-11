//! mDNS announcer: registers `<name>.local` on the local network so phones
//! and tablets resolve the share name with nothing installed (RFC 6762).
//!
//! Uses mdns-sd's full responder: it answers A-record queries for the
//! registered hostname and additionally advertises an `_https._tcp` service
//! (visible to network browsers like Bonjour/Discovery apps).

use std::collections::HashMap;
use std::net::IpAddr;

use anyhow::{Context, Result, ensure};
use mdns_sd::{ServiceDaemon, ServiceInfo};

use crate::protocol::valid_name;

/// Keeps the mDNS registration alive; dropping it stops answering.
pub struct Announcement {
    daemon: ServiceDaemon,
    fullname: String,
}

impl Drop for Announcement {
    fn drop(&mut self) {
        self.daemon.unregister(&self.fullname).ok();
        self.daemon.shutdown().ok();
    }
}

/// Announce `<name>.local` → this machine's LAN IP(s), advertising the
/// HTTPS edge on `port`.
pub fn announce(name: &str, port: u16) -> Result<Announcement> {
    let ips = lan_ips()?;
    announce_ips(name, port, &ips)
}

/// Like [`announce`], with explicit addresses (testable without a NIC).
pub fn announce_ips(name: &str, port: u16, ips: &[IpAddr]) -> Result<Announcement> {
    ensure!(valid_name(name), "invalid tunnel name: {name:?}");
    ensure!(!ips.is_empty(), "no LAN address to announce");
    let daemon = ServiceDaemon::new().context("starting mDNS daemon")?;
    let host = format!("{name}.local.");
    let info = ServiceInfo::new(
        "_https._tcp.local.",
        name,
        &host,
        ips,
        port,
        None::<HashMap<String, String>>,
    )
    .context("building mDNS service info")?;
    let fullname = info.get_fullname().to_string();
    daemon.register(info).context("registering mDNS service")?;
    Ok(Announcement { daemon, fullname })
}

/// This machine's non-loopback LAN addresses (IPv4 only for now).
pub fn lan_ips() -> Result<Vec<IpAddr>> {
    let mut ips: Vec<IpAddr> = local_ip_address::list_afinet_netifas()
        .context("listing network interfaces")?
        .into_iter()
        .map(|(_, ip)| ip)
        .filter(|ip| match ip {
            IpAddr::V4(v4) => !v4.is_loopback() && !v4.is_link_local(),
            // Skip v6 for announcing: link-local needs zone ids, ULAs are rare.
            IpAddr::V6(_) => false,
        })
        .collect();
    ips.sort();
    ips.dedup();
    ensure!(
        !ips.is_empty(),
        "no LAN address found — is Wi-Fi/Ethernet up?"
    );
    Ok(ips)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_invalid_input() {
        let ip: IpAddr = "192.168.1.10".parse().unwrap();
        assert!(announce_ips("Bad.Name", 4433, &[ip]).is_err());
        assert!(announce_ips("ok-name", 4433, &[]).is_err());
    }

    #[test]
    fn builds_an_announcement_for_valid_input() {
        let ip: IpAddr = "192.168.1.10".parse().unwrap();
        let _a = announce_ips("myapp", 4433, &[ip]).unwrap();
    }
}
