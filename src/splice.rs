//! Bidirectional byte copying between two read/write pairs.

use anyhow::Result;
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};

/// Copy `r1 -> w2` and `r2 -> w1` until both directions reach EOF.
/// Each writer is shut down when its source direction completes, so
/// half-closes propagate (QUIC stream finish, TCP FIN).
pub async fn splice<R1, W1, R2, W2>(mut r1: R1, mut w1: W1, mut r2: R2, mut w2: W2) -> Result<()>
where
    R1: AsyncRead + Unpin,
    W1: AsyncWrite + Unpin,
    R2: AsyncRead + Unpin,
    W2: AsyncWrite + Unpin,
{
    let forward = async {
        let n = tokio::io::copy(&mut r1, &mut w2).await;
        w2.shutdown().await.ok();
        n
    };
    let backward = async {
        let n = tokio::io::copy(&mut r2, &mut w1).await;
        w1.shutdown().await.ok();
        n
    };
    let (a, b) = tokio::join!(forward, backward);
    a?;
    b?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    async fn splices_both_directions_and_finishes_on_eof() {
        // client <-> [splice] <-> server, both pipes in memory
        let (mut client, splice_a) = tokio::io::duplex(64);
        let (mut server, splice_b) = tokio::io::duplex(64);
        let (ar, aw) = tokio::io::split(splice_a);
        let (br, bw) = tokio::io::split(splice_b);
        let task = tokio::spawn(splice(ar, aw, br, bw));

        client.write_all(b"ping").await.unwrap();
        let mut buf = [0u8; 4];
        server.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"ping");

        server.write_all(b"pong").await.unwrap();
        client.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"pong");

        // closing the client ends the client->server direction, which
        // shuts down the server side; the server's EOF then ends the
        // other direction. splice must terminate, not hang.
        drop(client);
        let mut rest = Vec::new();
        server.read_to_end(&mut rest).await.unwrap();
        drop(server);
        task.await.unwrap().unwrap();
    }
}
