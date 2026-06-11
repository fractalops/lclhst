//! iroh endpoint construction. All iroh setup lives here.

use anyhow::Result;
use iroh::{Endpoint, endpoint::presets};

use crate::protocol::ALPN;

/// Endpoint for the serving side: accepts lclhst connections.
pub async fn serve_endpoint() -> Result<Endpoint> {
    let ep = Endpoint::builder(presets::N0)
        .alpns(vec![ALPN.to_vec()])
        .bind()
        .await?;
    Ok(ep)
}

/// Endpoint for the opening side: outgoing connections only.
pub async fn open_endpoint() -> Result<Endpoint> {
    let ep = Endpoint::builder(presets::N0).bind().await?;
    Ok(ep)
}

#[cfg(test)]
mod tests {
    use crate::protocol;
    use tokio::io::AsyncWriteExt;

    #[tokio::test]
    async fn hello_status_round_trip_over_loopback_quic() {
        let server = super::serve_endpoint().await.unwrap();
        // addr() is only meaningful once the endpoint knows its addresses
        tokio::time::timeout(std::time::Duration::from_secs(5), server.online())
            .await
            .ok();
        let addr = server.addr();

        let accept = tokio::spawn(async move {
            let incoming = server.accept().await.expect("endpoint closed");
            let conn = incoming.accept().unwrap().await.unwrap();
            let (mut send, mut recv) = conn.accept_bi().await.unwrap();
            let hello = protocol::read_hello(&mut recv).await.unwrap();
            send.write_all(&[protocol::STATUS_OK]).await.unwrap();
            send.shutdown().await.unwrap();
            // Returning here would drop the connection (and the whole
            // endpoint) before the status byte hits the wire, leaving
            // the client to idle out. Hold until the client closes.
            conn.closed().await;
            hello.name
        });

        let client = super::open_endpoint().await.unwrap();
        let conn = client.connect(addr, protocol::ALPN).await.unwrap();
        let (mut send, mut recv) = conn.open_bi().await.unwrap();
        protocol::write_hello(&mut send, "myapp").await.unwrap();
        let mut status = [0u8; 1];
        recv.read_exact(&mut status).await.unwrap();
        assert_eq!(status[0], protocol::STATUS_OK);
        conn.close(0u32.into(), b"done");
        assert_eq!(accept.await.unwrap(), "myapp");
    }
}
