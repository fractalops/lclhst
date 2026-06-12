//! Websocket echo through the full tunnel (exercises the upgrade path).

use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio_tungstenite::tungstenite::Message;

async fn ws_echo_app() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        loop {
            let (sock, _) = listener.accept().await.unwrap();
            tokio::spawn(async move {
                let mut ws = tokio_tungstenite::accept_async(sock).await.unwrap();
                while let Some(Ok(msg)) = ws.next().await {
                    if msg.is_text() || msg.is_binary() {
                        ws.send(msg).await.unwrap();
                    }
                }
            });
        }
    });
    port
}

#[tokio::test]
async fn websocket_echo_through_tunnel() {
    let app_port = ws_echo_app().await;

    let (ticket_tx, ticket_rx) = oneshot::channel();
    fn test_ca(tag: &str) -> lclhst::ca::Ca {
        let dir = std::env::temp_dir().join(format!("lclhst-ws-ca-{tag}-{}", std::process::id()));
        lclhst::ca::Ca::load_or_create(&dir).unwrap()
    }
    // local_only: tests must not bind 0.0.0.0 or chatter mDNS on CI networks
    tokio::spawn(lclhst::serve(
        lclhst::Target::Port(app_port),
        "myapp".to_string(),
        Some(0),
        true,
        test_ca("serve"),
        ticket_tx,
    ));
    let ticket = ticket_rx.await.unwrap().ticket;
    let (ready_tx, ready_rx) = oneshot::channel();
    tokio::spawn(lclhst::open(
        ticket,
        Some(0),
        true,
        test_ca("open"),
        ready_tx,
    ));
    let edge_addr = ready_rx.await.unwrap();

    // TLS client that trusts anything (v0.1 self-signed edge cert)
    let tls = rustls::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(std::sync::Arc::new(NoVerify))
        .with_no_client_auth();
    let connector = tokio_tungstenite::Connector::Rustls(std::sync::Arc::new(tls));

    let tcp = tokio::net::TcpStream::connect(edge_addr).await.unwrap();
    let (mut ws, _) = tokio_tungstenite::client_async_tls_with_config(
        format!("wss://myapp.localhost:{}/ws", edge_addr.port()),
        tcp,
        None,
        Some(connector),
    )
    .await
    .unwrap();

    ws.send(Message::text("marco")).await.unwrap();
    let reply = ws.next().await.unwrap().unwrap();
    assert_eq!(reply.into_text().unwrap().as_str(), "marco");
}

/// Accepts any server cert. Test-only.
#[derive(Debug)]
struct NoVerify;

impl rustls::client::danger::ServerCertVerifier for NoVerify {
    fn verify_server_cert(
        &self,
        _: &rustls::pki_types::CertificateDer<'_>,
        _: &[rustls::pki_types::CertificateDer<'_>],
        _: &rustls::pki_types::ServerName<'_>,
        _: &[u8],
        _: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(
        &self,
        _: &[u8],
        _: &rustls::pki_types::CertificateDer<'_>,
        _: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }
    fn verify_tls13_signature(
        &self,
        _: &[u8],
        _: &rustls::pki_types::CertificateDer<'_>,
        _: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }
    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        rustls::crypto::ring::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}
