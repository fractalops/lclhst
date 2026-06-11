//! The serving side: forwards tunnel streams to the one configured local port.

use anyhow::Result;
use iroh::Endpoint;
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::protocol::{self, STATUS_CONNECT_FAILED, STATUS_OK};
use crate::splice::splice;

/// Accept connections and streams forever, forwarding each stream to
/// `127.0.0.1:<port>`. Prints connecting peers — the ticket is the
/// capability, so the user should see who redeemed it.
pub async fn run(endpoint: Endpoint, port: u16) -> Result<()> {
    while let Some(incoming) = endpoint.accept().await {
        let Ok(accepting) = incoming.accept() else {
            continue;
        };
        tokio::spawn(async move {
            let conn = match accepting.await {
                Ok(conn) => conn,
                Err(e) => {
                    tracing::warn!("failed to accept connection: {e}");
                    return;
                }
            };
            eprintln!("peer connected: {}", conn.remote_id());
            loop {
                match conn.accept_bi().await {
                    Ok((send, recv)) => {
                        tokio::spawn(async move {
                            if let Err(e) = handle_stream(recv, send, port).await {
                                tracing::warn!("stream error: {e}");
                            }
                        });
                    }
                    Err(e) => {
                        eprintln!("peer disconnected: {e}");
                        break;
                    }
                }
            }
        });
    }
    Ok(())
}

/// One stream: read hello, dial the local app, report status, splice.
///
/// The port comes exclusively from our own configuration — the protocol
/// carries no port, so a peer can never pick a different local target.
pub async fn handle_stream<R, W>(mut recv: R, mut send: W, port: u16) -> Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let hello = protocol::read_hello(&mut recv).await?;
    tracing::debug!("stream for tunnel {:?}", hello.name);
    match TcpStream::connect(("127.0.0.1", port)).await {
        Ok(tcp) => {
            send.write_all(&[STATUS_OK]).await?;
            send.flush().await?;
            let (tcp_r, tcp_w) = tcp.into_split();
            splice(recv, send, tcp_r, tcp_w).await
        }
        Err(e) => {
            tracing::warn!("local app on port {port} unreachable: {e}");
            send.write_all(&[STATUS_CONNECT_FAILED]).await?;
            send.shutdown().await.ok();
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    /// A TCP server that answers one connection with a fixed line.
    async fn one_shot_tcp_server(reply: &'static [u8]) -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 5];
            sock.read_exact(&mut buf).await.unwrap();
            assert_eq!(&buf, b"howdy");
            sock.write_all(reply).await.unwrap();
        });
        port
    }

    #[tokio::test]
    async fn connects_and_splices_on_ok() {
        let port = one_shot_tcp_server(b"yo").await;
        let (mut edge, target_side) = tokio::io::duplex(1024);
        let (r, w) = tokio::io::split(target_side);
        let task = tokio::spawn(handle_stream(r, w, port));

        crate::protocol::write_hello(&mut edge, "myapp")
            .await
            .unwrap();
        let mut status = [0u8; 1];
        edge.read_exact(&mut status).await.unwrap();
        assert_eq!(status[0], crate::protocol::STATUS_OK);

        edge.write_all(b"howdy").await.unwrap();
        let mut reply = [0u8; 2];
        edge.read_exact(&mut reply).await.unwrap();
        assert_eq!(&reply, b"yo");
        drop(edge);
        task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn reports_connect_failed_when_app_is_down() {
        // bind-then-drop to get a port with nothing listening
        let dead = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = dead.local_addr().unwrap().port();
        drop(dead);

        let (mut edge, target_side) = tokio::io::duplex(1024);
        let (r, w) = tokio::io::split(target_side);
        tokio::spawn(handle_stream(r, w, port));

        crate::protocol::write_hello(&mut edge, "myapp")
            .await
            .unwrap();
        let mut status = [0u8; 1];
        edge.read_exact(&mut status).await.unwrap();
        assert_eq!(status[0], crate::protocol::STATUS_CONNECT_FAILED);
        // stream must then be cleanly EOF
        let mut rest = Vec::new();
        edge.read_to_end(&mut rest).await.unwrap();
        assert!(rest.is_empty());
    }
}
