//! The receiving side's front door: HTTP knowledge lives here and only here.

use std::future::Future;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use bytes::Bytes;
use http_body_util::{BodyExt, Full, combinators::BoxBody};
use hyper::body::Body;
use hyper::header::HeaderValue;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite};

use crate::protocol::{self, STATUS_OK};

/// How long we give the tunnel to yield a handshaken stream before 504.
const OPEN_TIMEOUT: Duration = Duration::from_secs(10);

/// Anything that can produce a fresh, already-handshaken byte stream
/// into the tunnel. Real impl: `TunnelClient`. Tests use an in-memory pipe.
pub trait Opener: Clone + Send + Sync + 'static {
    type R: AsyncRead + Unpin + Send + 'static;
    type W: AsyncWrite + Unpin + Send + 'static;

    fn open(&self) -> impl Future<Output = Result<(Self::R, Self::W)>> + Send;
}

/// Send the hello, consume the status byte. Shared by every Opener.
pub async fn client_handshake<R, W>(r: &mut R, w: &mut W, name: &str) -> Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    protocol::write_hello(w, name).await?;
    let mut status = [0u8; 1];
    r.read_exact(&mut status)
        .await
        .context("reading status byte")?;
    if status[0] != STATUS_OK {
        bail!("the app is not reachable on the far side");
    }
    Ok(())
}

/// The real Opener: one iroh connection, one QUIC stream per request.
#[derive(Clone)]
pub struct TunnelClient {
    conn: iroh::endpoint::Connection,
    name: String,
}

impl TunnelClient {
    pub fn new(conn: iroh::endpoint::Connection, name: String) -> Self {
        Self { conn, name }
    }
}

impl Opener for TunnelClient {
    type R = noq::RecvStream;
    type W = noq::SendStream;

    async fn open(&self) -> Result<(Self::R, Self::W)> {
        let (mut send, mut recv) = self
            .conn
            .open_bi()
            .await
            .map_err(|e| anyhow::anyhow!("tunnel unreachable: {e}"))?;
        client_handshake(&mut recv, &mut send, &self.name).await?;
        Ok((recv, send))
    }
}

/// Opener that skips the tunnel entirely: each open is a TCP connection to
/// a port on this machine. This is what serves the LAN on the `serve` side,
/// where the target is local and no QUIC hop exists.
#[derive(Clone)]
pub struct DirectOpener {
    pub port: u16,
}

impl Opener for DirectOpener {
    type R = tokio::net::tcp::OwnedReadHalf;
    type W = tokio::net::tcp::OwnedWriteHalf;

    async fn open(&self) -> Result<(Self::R, Self::W)> {
        let tcp = tokio::net::TcpStream::connect(("127.0.0.1", self.port))
            .await
            .with_context(|| format!("the app on port {} is not reachable", self.port))?;
        Ok(tcp.into_split())
    }
}

type ProxyBody = BoxBody<Bytes, hyper::Error>;

/// Proxy one request through the tunnel. Infallible: failures become
/// styled 502/504 pages instead of connection drops.
pub async fn handle_request<O, B>(
    opener: O,
    tunnel_name: String,
    mut req: Request<B>,
) -> Response<ProxyBody>
where
    O: Opener,
    B: Body<Data = Bytes> + Send + 'static,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    // Capture the downstream upgrade handle before the request moves.
    let downstream_upgrade = req.extensions_mut().remove::<hyper::upgrade::OnUpgrade>();

    let opened = tokio::time::timeout(OPEN_TIMEOUT, opener.open()).await;
    let (r, w) = match opened {
        Ok(Ok(pair)) => pair,
        Ok(Err(e)) => {
            return error_page(StatusCode::BAD_GATEWAY, &tunnel_name, &e.to_string());
        }
        Err(_) => {
            return error_page(
                StatusCode::GATEWAY_TIMEOUT,
                &tunnel_name,
                "the tunnel did not answer in time",
            );
        }
    };

    match proxy(r, w, &tunnel_name, downstream_upgrade, req).await {
        Ok(resp) => resp,
        Err(e) => error_page(StatusCode::BAD_GATEWAY, &tunnel_name, &e.to_string()),
    }
}

async fn proxy<R, W, B>(
    r: R,
    w: W,
    tunnel_name: &str,
    downstream_upgrade: Option<hyper::upgrade::OnUpgrade>,
    mut req: Request<B>,
) -> Result<Response<ProxyBody>>
where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
    B: Body<Data = Bytes> + Send + 'static,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    let io = TokioIo::new(tokio::io::join(r, w));
    let (mut sender, conn) = hyper::client::conn::http1::handshake(io).await?;
    tokio::spawn(conn.with_upgrades());

    // The far-side app sees plain localhost, not <name>.localhost:4433.
    req.headers_mut()
        .insert(hyper::header::HOST, HeaderValue::from_static("localhost"));

    let mut resp = sender.send_request(req).await?;

    if resp.status() == StatusCode::SWITCHING_PROTOCOLS {
        let tunnel_name = tunnel_name.to_string();
        let upstream_upgrade = hyper::upgrade::on(&mut resp);
        if let Some(downstream) = downstream_upgrade {
            tokio::spawn(async move {
                let result: Result<()> = async {
                    let (up, down) = tokio::try_join!(upstream_upgrade, downstream)?;
                    let mut up = TokioIo::new(up);
                    let mut down = TokioIo::new(down);
                    tokio::io::copy_bidirectional(&mut up, &mut down).await?;
                    Ok(())
                }
                .await;
                if let Err(e) = result {
                    tracing::warn!("upgraded connection for {tunnel_name} ended: {e}");
                }
            });
        }
    }

    Ok(resp.map(BodyExt::boxed))
}

fn error_page(status: StatusCode, tunnel_name: &str, detail: &str) -> Response<ProxyBody> {
    let html = format!(
        "<!doctype html><html><head><title>{code} — lclhst</title>\
         <style>body{{font-family:system-ui;max-width:36rem;margin:4rem auto;color:#333}}\
         code{{background:#f4f4f4;padding:.1rem .3rem;border-radius:4px}}</style></head>\
         <body><h1>{code} {reason}</h1>\
         <p>The tunnel <code>{tunnel_name}</code> is up, but the request didn't make it through:</p>\
         <p><em>{detail}</em></p>\
         <p>The app on the serving side may have stopped. Ask the person running\
         <code>lclhst serve</code> to check.</p></body></html>",
        code = status.as_u16(),
        reason = status.canonical_reason().unwrap_or(""),
    );
    let body = Full::new(Bytes::from(html))
        .map_err(|never| match never {})
        .boxed();
    Response::builder()
        .status(status)
        .header("content-type", "text/html; charset=utf-8")
        .body(body)
        .expect("static response")
}

use std::net::SocketAddr;
use std::sync::Arc;

use hyper::service::service_fn;
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio_rustls::TlsAcceptor;

/// Serve an opener over HTTPS at `bind`. With a loopback bind the share is
/// reachable as https://<name>.localhost:<port>; with an unspecified bind
/// (0.0.0.0) the LAN can reach it as https://<name>.local:<port> (mDNS).
/// Sends the bound address once listening (port 0 picks a free port — tests).
pub async fn run<O: Opener>(
    opener: O,
    name: String,
    bind: SocketAddr,
    ready: oneshot::Sender<SocketAddr>,
) -> Result<()> {
    let tls = TlsAcceptor::from(Arc::new(crate::tls::server_config(&name)?));
    let listener = TcpListener::bind(bind)
        .await
        .with_context(|| format!("binding {bind}"))?;
    let addr = listener.local_addr()?;
    ready.send(addr).ok();

    loop {
        let (tcp, _) = listener.accept().await?;
        let tls = tls.clone();
        let opener = opener.clone();
        let name = name.clone();
        tokio::spawn(async move {
            let stream = match tls.accept(tcp).await {
                Ok(s) => s,
                Err(e) => {
                    tracing::debug!("TLS handshake failed: {e}");
                    return;
                }
            };
            let service = service_fn(move |req| {
                let opener = opener.clone();
                let name = name.clone();
                async move { Ok::<_, std::convert::Infallible>(handle_request(opener, name, req).await) }
            });
            if let Err(e) = hyper::server::conn::http1::Builder::new()
                .serve_connection(TokioIo::new(stream), service)
                .with_upgrades()
                .await
            {
                tracing::debug!("connection ended: {e}");
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use http_body_util::{BodyExt, Empty};
    use hyper::Request;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    /// Opener wired straight to the real target logic via an in-memory pipe.
    #[derive(Clone)]
    struct LocalOpener {
        port: u16,
    }

    impl Opener for LocalOpener {
        type R = tokio::io::ReadHalf<tokio::io::DuplexStream>;
        type W = tokio::io::WriteHalf<tokio::io::DuplexStream>;

        async fn open(&self) -> anyhow::Result<(Self::R, Self::W)> {
            let (edge_side, target_side) = tokio::io::duplex(8192);
            let (tr, tw) = tokio::io::split(target_side);
            tokio::spawn(crate::target::handle_stream(tr, tw, self.port));
            let (mut r, mut w) = tokio::io::split(edge_side);
            client_handshake(&mut r, &mut w, "myapp").await?;
            Ok((r, w))
        }
    }

    /// Minimal HTTP/1.1 app: replies with the request's Host header in the body.
    async fn hello_app() -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            loop {
                let (mut sock, _) = listener.accept().await.unwrap();
                tokio::spawn(async move {
                    let mut buf = vec![0u8; 4096];
                    let n = sock.read(&mut buf).await.unwrap();
                    let req = String::from_utf8_lossy(&buf[..n]).to_string();
                    let host = req
                        .lines()
                        .find(|l| l.to_lowercase().starts_with("host:"))
                        .map(|l| l[5..].trim().to_string())
                        .unwrap_or_default();
                    let body = format!("host={host}");
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\ncontent-length: {}\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    sock.write_all(resp.as_bytes()).await.unwrap();
                });
            }
        });
        port
    }

    #[tokio::test]
    async fn proxies_a_request_and_rewrites_host() {
        let port = hello_app().await;
        let req = Request::builder()
            .uri("/")
            .header("host", "myapp.localhost:4433")
            .body(Empty::<bytes::Bytes>::new())
            .unwrap();
        let resp = handle_request(LocalOpener { port }, "myapp".to_string(), req).await;
        assert_eq!(resp.status(), 200);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        // Host must be rewritten for the local app, not leak myapp.localhost
        assert_eq!(&body[..], b"host=localhost");
    }

    #[tokio::test]
    async fn app_down_becomes_502_naming_the_tunnel() {
        let dead = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = dead.local_addr().unwrap().port();
        drop(dead);
        let req = Request::builder()
            .uri("/")
            .body(Empty::<bytes::Bytes>::new())
            .unwrap();
        let resp = handle_request(LocalOpener { port }, "myapp".to_string(), req).await;
        assert_eq!(resp.status(), 502);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let text = String::from_utf8_lossy(&body);
        assert!(
            text.contains("myapp"),
            "502 page should name the tunnel: {text}"
        );
    }
}
