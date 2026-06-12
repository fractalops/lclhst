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
use tokio_rustls::TlsAcceptor;

/// TLS material for an edge: the CA-minted server config plus the CA
/// certificate itself, served on the onboarding route.
#[derive(Clone)]
pub struct EdgeTls {
    pub config: Arc<rustls::ServerConfig>,
    pub ca_pem: String,
}

impl EdgeTls {
    pub fn new(config: rustls::ServerConfig, ca_pem: String) -> Self {
        Self {
            config: Arc::new(config),
            ca_pem,
        }
    }
}

/// Bind a listener for an edge, trying each port in order. `None` means
/// auto: 443 first (bare https URLs — works unprivileged on macOS), then
/// the registered fallback 4433.
pub async fn bind(ip: std::net::IpAddr, port: Option<u16>) -> Result<TcpListener> {
    let candidates: &[u16] = match port {
        Some(p) => &[p],
        None => &[443, 4433],
    };
    let mut last_err = None;
    for &p in candidates {
        match TcpListener::bind(SocketAddr::new(ip, p)).await {
            Ok(l) => return Ok(l),
            Err(e) => {
                tracing::debug!("could not bind {ip}:{p}: {e}");
                last_err = Some((p, e));
            }
        }
    }
    let (p, e) = last_err.expect("at least one candidate");
    Err(anyhow::anyhow!(e)).with_context(|| format!("binding {ip}:{p}"))
}

/// Serve an opener over HTTPS on an already-bound listener. With a loopback
/// bind the share is reachable as https://<name>.localhost:<port>; with an
/// unspecified bind (0.0.0.0) the LAN can reach it as
/// https://<name>.local:<port> (mDNS).
pub async fn run<O: Opener>(
    opener: O,
    name: String,
    listener: TcpListener,
    tls: EdgeTls,
) -> Result<()> {
    let acceptor = TlsAcceptor::from(tls.config.clone());

    loop {
        let (tcp, _) = listener.accept().await?;
        let acceptor = acceptor.clone();
        let opener = opener.clone();
        let name = name.clone();
        let ca_pem = tls.ca_pem.clone();
        tokio::spawn(async move {
            // Browsers given a bare host:port try plain http first. A TLS
            // record always starts with 0x16 (handshake); anything else is
            // plaintext — serve onboarding routes or redirect to https.
            let mut first = [0u8; 1];
            match tcp.peek(&mut first).await {
                Ok(0) | Err(_) => return,
                Ok(_) if first[0] != 0x16 => {
                    serve_plaintext(tcp, &name, &ca_pem).await;
                    return;
                }
                Ok(_) => {}
            }
            let stream = match acceptor.accept(tcp).await {
                Ok(s) => s,
                Err(e) => {
                    tracing::debug!("TLS handshake failed: {e}");
                    return;
                }
            };
            let service = service_fn(move |req| {
                let opener = opener.clone();
                let name = name.clone();
                let ca_pem = ca_pem.clone();
                async move {
                    let resp = match onboarding_response(req.uri().path(), &name, &ca_pem) {
                        Some(resp) => resp,
                        None => handle_request(opener, name, req).await,
                    };
                    Ok::<_, std::convert::Infallible>(resp)
                }
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

/// The reserved path prefix for lclhst's own pages (CA download, help).
const ONBOARDING_PREFIX: &str = "/.lclhst";

/// Responses for the reserved `/.lclhst/` routes, or None to proxy normally.
fn onboarding_response(path: &str, name: &str, ca_pem: &str) -> Option<Response<ProxyBody>> {
    let body = |bytes: Bytes| {
        Full::new(bytes)
            .map_err(|never: std::convert::Infallible| match never {})
            .boxed()
    };
    match path {
        "/.lclhst/ca" => Some(
            Response::builder()
                .status(StatusCode::OK)
                // This mime type makes iOS offer profile installation and
                // Android offer CA installation on tap.
                .header("content-type", "application/x-x509-ca-cert")
                .header(
                    "content-disposition",
                    "attachment; filename=\"lclhst-ca.crt\"",
                )
                .body(body(Bytes::from(ca_pem.to_string())))
                .expect("static response"),
        ),
        "/.lclhst" | "/.lclhst/" => Some(
            Response::builder()
                .status(StatusCode::OK)
                .header("content-type", "text/html; charset=utf-8")
                .body(body(Bytes::from(onboarding_page(name))))
                .expect("static response"),
        ),
        _ => None,
    }
}

fn onboarding_page(name: &str) -> String {
    format!(
        "<!doctype html><html><head><title>trust this device — lclhst</title>\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
         <style>body{{font-family:system-ui;max-width:36rem;margin:2rem auto;padding:0 1rem;color:#333}}\
         code{{background:#f4f4f4;padding:.1rem .3rem;border-radius:4px}}\
         a.button{{display:inline-block;background:#0a7;color:#fff;padding:.6rem 1.2rem;\
         border-radius:8px;text-decoration:none;margin:.5rem 0}}</style></head>\
         <body><h2>Trust this device once, browse without warnings</h2>\
         <p><code>{name}</code> is shared via lclhst. Its certificates are signed by a\
         private authority on the sharing machine. Install that authority once and\
         every lclhst share from it gets a normal padlock.</p>\
         <p><a class=\"button\" href=\"/.lclhst/ca\">Download the certificate</a></p>\
         <p><strong>iPhone/iPad:</strong> after downloading, Settings → General →\
         VPN &amp; Device Management → install the profile, then Settings → General →\
         About → Certificate Trust Settings → enable full trust.</p>\
         <p><strong>Android:</strong> after downloading, tap the file (or Settings →\
         Security → Install a certificate → CA certificate).</p>\
         <p><strong>Mac/Linux:</strong> run <code>lclhst trust</code> on the machine\
         that serves, or import the downloaded file into your trust store.</p>\
         <p>Skipping this is fine too — traffic is encrypted either way; your browser\
         just can't verify who it's talking to, so it shows a warning.</p>\
         </body></html>"
    )
}

/// Answer one plaintext HTTP request: onboarding routes are served directly
/// (a device can't fetch the CA over https before trusting it); everything
/// else gets a 301 to the same host over https.
async fn serve_plaintext(mut tcp: tokio::net::TcpStream, name: &str, ca_pem: &str) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let mut buf = vec![0u8; 4096];
    let Ok(n) = tcp.read(&mut buf).await else {
        return;
    };
    let head = String::from_utf8_lossy(&buf[..n]);
    let path = head
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .filter(|p| p.starts_with('/'))
        .unwrap_or("/");

    let raw = if path == "/.lclhst/ca" {
        format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/x-x509-ca-cert\r\n\
             content-disposition: attachment; filename=\"lclhst-ca.crt\"\r\n\
             content-length: {}\r\nconnection: close\r\n\r\n{}",
            ca_pem.len(),
            ca_pem
        )
    } else if path.starts_with(ONBOARDING_PREFIX) {
        let page = onboarding_page(name);
        format!(
            "HTTP/1.1 200 OK\r\ncontent-type: text/html; charset=utf-8\r\n\
             content-length: {}\r\nconnection: close\r\n\r\n{}",
            page.len(),
            page
        )
    } else {
        // Preserve whatever host:port the client used; it serves https too.
        let host = head
            .lines()
            .find_map(|l| {
                l.strip_prefix("Host: ")
                    .or_else(|| l.strip_prefix("host: "))
            })
            .map(str::trim)
            .filter(|h| !h.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| format!("{name}.local"));
        format!(
            "HTTP/1.1 301 Moved Permanently\r\nlocation: https://{host}{path}\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
        )
    };
    tcp.write_all(raw.as_bytes()).await.ok();
    tcp.shutdown().await.ok();
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

    fn test_tls() -> EdgeTls {
        let dir = std::env::temp_dir().join(format!("lclhst-edge-test-ca-{}", std::process::id()));
        let ca = crate::ca::Ca::load_or_create(&dir).unwrap();
        EdgeTls::new(
            ca.server_config("myapp", &[]).unwrap(),
            ca.cert_pem().to_string(),
        )
    }

    async fn start_edge() -> std::net::SocketAddr {
        let listener = bind("127.0.0.1".parse().unwrap(), Some(0)).await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(run(
            LocalOpener { port: 1 }, // never dialed in these tests
            "myapp".to_string(),
            listener,
            test_tls(),
        ));
        addr
    }

    async fn plaintext_request(addr: std::net::SocketAddr, path: &str) -> String {
        let mut tcp = tokio::net::TcpStream::connect(addr).await.unwrap();
        tcp.write_all(
            format!(
                "GET {path} HTTP/1.1\r\nHost: myapp.local:{}\r\n\r\n",
                addr.port()
            )
            .as_bytes(),
        )
        .await
        .unwrap();
        let mut resp = String::new();
        tcp.read_to_string(&mut resp).await.unwrap();
        resp
    }

    #[tokio::test]
    async fn plain_http_serves_onboarding_routes() {
        let addr = start_edge().await;
        let ca = plaintext_request(addr, "/.lclhst/ca").await;
        assert!(ca.starts_with("HTTP/1.1 200"), "{ca}");
        assert!(ca.contains("-----BEGIN CERTIFICATE-----"), "{ca}");
        let help = plaintext_request(addr, "/.lclhst/").await;
        assert!(help.starts_with("HTTP/1.1 200"), "{help}");
        assert!(help.contains("Trust this device"), "{help}");
    }

    #[tokio::test]
    async fn plain_http_gets_redirected_to_https() {
        let addr = start_edge().await;
        let resp = plaintext_request(addr, "/sub/page?x=1").await;
        assert!(resp.starts_with("HTTP/1.1 301"), "{resp}");
        assert!(
            resp.contains(&format!(
                "location: https://myapp.local:{}/sub/page?x=1",
                addr.port()
            )),
            "{resp}"
        );
    }
}
