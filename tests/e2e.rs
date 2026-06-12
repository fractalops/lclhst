//! Full-stack test: hello app <- target <- iroh loopback <- edge <- reqwest.

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::oneshot;

async fn hello_app() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        loop {
            let (mut sock, _) = listener.accept().await.unwrap();
            tokio::spawn(async move {
                let mut buf = vec![0u8; 4096];
                let _ = sock.read(&mut buf).await.unwrap();
                let body = "hello through the tunnel";
                let resp = format!(
                    "HTTP/1.1 200 OK\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                sock.write_all(resp.as_bytes()).await.unwrap();
            });
        }
    });
    port
}

fn test_ca(tag: &str) -> lclhst::ca::Ca {
    let dir = std::env::temp_dir().join(format!("lclhst-e2e-ca-{tag}-{}", std::process::id()));
    lclhst::ca::Ca::load_or_create(&dir).unwrap()
}

/// Start serve+open with per-test CAs and return a client that trusts ONLY
/// the opener's CA — certificate validation is fully on, so requests assert
/// that the minted chain actually verifies.
async fn start_tunnel(tag: &str, app_port: u16) -> (std::net::SocketAddr, reqwest::Client) {
    let (ticket_tx, ticket_rx) = oneshot::channel();
    // local_only: tests must not bind 0.0.0.0 or chatter mDNS on CI networks
    tokio::spawn(lclhst::serve(
        lclhst::Target::Port(app_port),
        "myapp".to_string(),
        Some(0),
        true,
        test_ca(&format!("{tag}-serve")),
        ticket_tx,
    ));
    let ticket = ticket_rx.await.unwrap().ticket;

    let open_ca = test_ca(&format!("{tag}-open"));
    let ca_pem = open_ca.cert_pem().to_string();
    let (ready_tx, ready_rx) = oneshot::channel();
    tokio::spawn(lclhst::open(ticket, Some(0), true, open_ca, ready_tx));
    let addr = ready_rx.await.unwrap();

    let client = reqwest::Client::builder()
        .add_root_certificate(reqwest::Certificate::from_pem(ca_pem.as_bytes()).unwrap())
        .resolve("myapp.localhost", addr) // don't depend on OS .localhost resolution
        .build()
        .unwrap();
    (addr, client)
}

#[tokio::test]
async fn get_through_the_tunnel() {
    let app_port = hello_app().await;
    let (edge_addr, client) = start_tunnel("get", app_port).await;
    let resp = client
        .get(format!("https://myapp.localhost:{}/", edge_addr.port()))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.text().await.unwrap(), "hello through the tunnel");
}

#[tokio::test]
async fn app_down_yields_502_page() {
    let dead = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let app_port = dead.local_addr().unwrap().port();
    drop(dead);
    let (edge_addr, client) = start_tunnel("502", app_port).await;
    let resp = client
        .get(format!("https://myapp.localhost:{}/", edge_addr.port()))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 502);
    assert!(resp.text().await.unwrap().contains("myapp"));
}
