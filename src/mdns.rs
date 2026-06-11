//! mDNS announcer: answers multicast-DNS A/AAAA queries for `<name>.local`
//! with this machine's LAN addresses, so phones and tablets on the same
//! network resolve the name with nothing installed (RFC 6762).

use std::net::IpAddr;

use anyhow::{Context, Result, ensure};
use simple_dns::rdata::{A, AAAA, RData};
use simple_dns::{CLASS, Name, ResourceRecord};
use simple_mdns::async_discovery::SimpleMdnsResponder;

use crate::protocol::valid_name;

/// Record TTL in seconds. Short, because shares are ephemeral.
const TTL: u32 = 60;

/// Announce `<name>.local` → this machine's LAN IP(s). The responder
/// answers queries for as long as the returned guard is alive.
pub async fn announce(name: &str) -> Result<SimpleMdnsResponder> {
    let ips = lan_ips()?;
    announce_ips(name, &ips).await
}

/// Like [`announce`], with explicit addresses (testable without a NIC).
pub async fn announce_ips(name: &str, ips: &[IpAddr]) -> Result<SimpleMdnsResponder> {
    ensure!(valid_name(name), "invalid tunnel name: {name:?}");
    ensure!(!ips.is_empty(), "no LAN address to announce");
    let fqdn = format!("{name}.local");
    let dns_name = Name::new(&fqdn)
        .with_context(|| format!("invalid mDNS name {fqdn:?}"))?
        .into_owned();
    let mut responder = SimpleMdnsResponder::new(TTL);
    for ip in ips {
        let rdata = match ip {
            IpAddr::V4(v4) => RData::A(A::from(*v4)),
            IpAddr::V6(v6) => RData::AAAA(AAAA::from(*v6)),
        };
        responder
            .add_resource(ResourceRecord::new(dns_name.clone(), CLASS::IN, TTL, rdata))
            .await;
    }
    Ok(responder)
}

/// This machine's non-loopback LAN addresses (IPv4 preferred first).
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

    #[tokio::test]
    async fn rejects_invalid_names() {
        let ip: IpAddr = "192.168.1.10".parse().unwrap();
        assert!(announce_ips("Bad.Name", &[ip]).await.is_err());
        assert!(announce_ips("ok-name", &[]).await.is_err());
    }

    #[tokio::test]
    async fn builds_a_responder_for_valid_input() {
        let ip: IpAddr = "192.168.1.10".parse().unwrap();
        let _responder = announce_ips("myapp", &[ip]).await.unwrap();
    }
}
