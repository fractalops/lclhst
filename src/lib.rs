//! lclhst — share local apps and folders with other devices, over the LAN
//! or peer-to-peer, with no server in the middle.

pub mod ca;
pub mod edge;
pub mod fileserve;
pub mod mdns;
pub mod protocol;
pub mod splice;
pub mod target;
pub mod ticket;
pub mod trust;
pub mod tunnel;

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};
use tokio::sync::oneshot;

/// What `serve` shares: a port something is listening on, or a path the
/// built-in file server will serve.
#[derive(Debug, Clone)]
pub enum Target {
    Port(u16),
    Path(PathBuf),
}

impl Target {
    /// A bare number is a port; anything else must be an existing path.
    pub fn parse(s: &str) -> Result<Self> {
        if let Ok(port) = s.parse::<u16>() {
            return Ok(Target::Port(port));
        }
        let path = PathBuf::from(s);
        anyhow::ensure!(
            path.exists(),
            "{s:?} is neither a port number nor an existing path"
        );
        Ok(Target::Path(path))
    }

    /// Default share name: the file/directory name for paths, "app" for ports.
    pub fn default_name(&self) -> String {
        match self {
            Target::Port(_) => "app".to_string(),
            Target::Path(p) => p
                .canonicalize()
                .ok()
                .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
                .map(|n| sanitize_name(&n))
                .filter(|n| protocol::valid_name(n))
                .unwrap_or_else(|| "files".to_string()),
        }
    }
}

/// Lowercase a filesystem name into a DNS label (best effort).
fn sanitize_name(s: &str) -> String {
    let mut out: String = s
        .to_lowercase()
        .chars()
        .map(|c| {
            if c.is_ascii_lowercase() || c.is_ascii_digit() {
                c
            } else {
                '-'
            }
        })
        .collect();
    out.truncate(63);
    out.trim_matches('-').to_string()
}

/// What the running share exposes; sent once everything is listening.
#[derive(Debug)]
pub struct ServeInfo {
    pub ticket: ticket::Ticket,
    /// LAN URL (https://<name>.local:<port>) if LAN exposure is on.
    pub lan: Option<SocketAddr>,
}

/// Serve a target: print a ticket for remote peers and, unless
/// `local_only`, expose an HTTPS edge + mDNS name on the LAN.
pub async fn serve(
    target: Target,
    name: String,
    edge_port: Option<u16>,
    local_only: bool,
    ca: ca::Ca,
    info_tx: oneshot::Sender<ServeInfo>,
) -> Result<()> {
    anyhow::ensure!(
        protocol::valid_name(&name),
        "invalid name {name:?}: use a lowercase DNS label like my-app"
    );
    let port = match &target {
        Target::Port(p) => *p,
        Target::Path(path) => fileserve::serve(path.clone())
            .await
            .context("starting the file server")?,
    };

    let endpoint = tunnel::serve_endpoint().await?;
    if tokio::time::timeout(Duration::from_secs(5), endpoint.online())
        .await
        .is_err()
    {
        eprintln!("warning: no relay reachable — only direct connections will work");
    }
    let t = ticket::Ticket {
        name: name.clone(),
        endpoint: iroh_tickets::endpoint::EndpointTicket::new(endpoint.addr()),
    };

    // LAN edge: same proxy as `open` runs, but the opener dials straight
    // to the local port — no tunnel hop for devices in the same room.
    let lan = if local_only {
        None
    } else {
        match start_lan_edge(edge::DirectOpener { port }, &name, edge_port, &ca).await {
            Ok(addr) => Some(addr),
            Err(e) => {
                eprintln!("warning: LAN exposure failed ({e}); continuing with ticket only");
                None
            }
        }
    };

    info_tx.send(ServeInfo { ticket: t, lan }).ok();
    target::run(endpoint, port).await
}

/// Open a ticket: serve it at https://<name>.localhost:<port> on this
/// machine and, unless `local_only`, at https://<name>.local:<port> for
/// devices on this machine's network.
pub async fn open(
    t: ticket::Ticket,
    edge_port: Option<u16>,
    local_only: bool,
    ca: ca::Ca,
    ready_tx: oneshot::Sender<SocketAddr>,
) -> Result<()> {
    let endpoint = tunnel::open_endpoint().await?;
    let conn = endpoint
        .connect(t.endpoint.endpoint_addr().clone(), protocol::ALPN)
        .await
        .map_err(|e| anyhow::anyhow!("could not reach the serving side (stale ticket?): {e}"))?;
    let opener = edge::TunnelClient::new(conn, t.name.clone());

    let (bind_ip, cert_ips) = if local_only {
        (IpAddr::V4(Ipv4Addr::LOCALHOST), Vec::new())
    } else {
        (
            IpAddr::V4(Ipv4Addr::UNSPECIFIED),
            mdns::lan_ips().unwrap_or_default(),
        )
    };
    let listener = edge::bind(bind_ip, edge_port).await?;
    let addr = listener.local_addr()?;
    if addr.port() == 443 && !local_only {
        // Plain http://<name>.local then lands on the redirect/onboarding.
        edge::spawn_port_80_helper(bind_ip, t.name.clone(), ca.cert_pem().to_string());
    }

    let _mdns_guard = if local_only {
        None
    } else {
        match mdns::announce(&t.name, addr.port()) {
            Ok(g) => Some(g),
            Err(e) => {
                eprintln!("warning: mDNS announce failed ({e}); LAN devices must use the IP");
                None
            }
        }
    };
    let tls = edge::EdgeTls::new(
        ca.server_config(&t.name, &cert_ips)?,
        ca.cert_pem().to_string(),
    );
    ready_tx.send(addr).ok();
    edge::run(opener, t.name, listener, tls).await
}

/// Bind an edge on all interfaces and announce its name over mDNS.
/// Returns the address the LAN should use (LAN IP + bound port).
async fn start_lan_edge<O: edge::Opener>(
    opener: O,
    name: &str,
    edge_port: Option<u16>,
    ca: &ca::Ca,
) -> Result<SocketAddr> {
    let lan_ips = mdns::lan_ips()?;
    let lan_ip = *lan_ips.first().expect("lan_ips is non-empty");
    let tls = edge::EdgeTls::new(ca.server_config(name, &lan_ips)?, ca.cert_pem().to_string());
    let listener = edge::bind(IpAddr::V4(Ipv4Addr::UNSPECIFIED), edge_port).await?;
    let bound = listener.local_addr()?;
    if bound.port() == 443 {
        // Plain http://<name>.local then lands on the redirect/onboarding.
        edge::spawn_port_80_helper(
            IpAddr::V4(Ipv4Addr::UNSPECIFIED),
            name.to_string(),
            ca.cert_pem().to_string(),
        );
    }
    let responder = mdns::announce(name, bound.port())?;
    let name = name.to_string();
    tokio::spawn(async move {
        // The mDNS registration lives exactly as long as the edge: dropping
        // the guard unregisters the name.
        let _responder = responder;
        if let Err(e) = edge::run(opener, name, listener, tls).await {
            tracing::warn!("LAN edge stopped: {e}");
        }
    });
    Ok(SocketAddr::new(lan_ip, bound.port()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_parses_ports_and_paths() {
        assert!(matches!(Target::parse("3000"), Ok(Target::Port(3000))));
        assert!(matches!(Target::parse("."), Ok(Target::Path(_))));
        assert!(Target::parse("/definitely/not/a/real/path").is_err());
        assert!(Target::parse("70000").is_err()); // not a u16, not a path
    }

    #[test]
    fn default_names_are_valid_dns_labels() {
        assert_eq!(Target::Port(3000).default_name(), "app");
        let name = Target::Path(PathBuf::from(".")).default_name();
        assert!(protocol::valid_name(&name), "{name:?}");
        assert_eq!(sanitize_name("My Photos (2026)"), "my-photos--2026");
    }
}
