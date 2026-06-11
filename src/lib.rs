//! lclhst — share a running localhost app peer-to-peer.

pub mod edge;
pub mod fileserve;
pub mod mdns;
pub mod protocol;
pub mod splice;
pub mod target;
pub mod ticket;
pub mod tls;
pub mod tunnel;

use std::net::SocketAddr;
use std::time::Duration;

use anyhow::Result;
use tokio::sync::oneshot;

/// Serve a local port. Sends the ticket once the endpoint is up, then
/// accepts peers until cancelled.
pub async fn serve(
    port: u16,
    name: String,
    ticket_tx: oneshot::Sender<ticket::Ticket>,
) -> Result<()> {
    anyhow::ensure!(
        protocol::valid_name(&name),
        "invalid name {name:?}: use a lowercase DNS label like my-app"
    );
    let endpoint = tunnel::serve_endpoint().await?;
    if tokio::time::timeout(Duration::from_secs(5), endpoint.online())
        .await
        .is_err()
    {
        eprintln!("warning: no relay reachable — only direct connections will work");
    }
    let t = ticket::Ticket {
        name,
        endpoint: iroh_tickets::endpoint::EndpointTicket::new(endpoint.addr()),
    };
    ticket_tx.send(t).ok();
    target::run(endpoint, port).await
}

/// Open a ticket. Sends the local edge address once listening, then
/// proxies until cancelled.
pub async fn open(
    t: ticket::Ticket,
    edge_port: u16,
    ready_tx: oneshot::Sender<SocketAddr>,
) -> Result<()> {
    let endpoint = tunnel::open_endpoint().await?;
    let conn = endpoint
        .connect(t.endpoint.endpoint_addr().clone(), protocol::ALPN)
        .await
        .map_err(|e| anyhow::anyhow!("could not reach the serving side (stale ticket?): {e}"))?;
    let opener = edge::TunnelClient::new(conn, t.name.clone());
    edge::run(opener, t.name, edge_port, ready_tx).await
}
