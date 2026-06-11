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

async fn start_tunnel(app_port: u16) -> std::net::SocketAddr {
    let (ticket_tx, ticket_rx) = oneshot::channel();
    // local_only: tests must not bind 0.0.0.0 or chatter mDNS on CI networks
    tokio::spawn(lclhst::serve(
        lclhst::Target::Port(app_port),
        "myapp".to_string(),
        0,
        true,
        ticket_tx,
    ));
    let ticket = ticket_rx.await.unwrap().ticket;

    let (ready_tx, ready_rx) = oneshot::channel();
    tokio::spawn(lclhst::open(ticket, 0, true, ready_tx));
    ready_rx.await.unwrap()
}

fn client_for(addr: std::net::SocketAddr) -> reqwest::Client {
    reqwest::Client::builder()
        .danger_accept_invalid_certs(true) // v0.1 self-signed
        .resolve("myapp.localhost", addr) // don't depend on OS .localhost resolution
        .build()
        .unwrap()
}

#[tokio::test]
async fn get_through_the_tunnel() {
    let app_port = hello_app().await;
    let edge_addr = start_tunnel(app_port).await;
    let resp = client_for(edge_addr)
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
    let edge_addr = start_tunnel(app_port).await;
    let resp = client_for(edge_addr)
        .get(format!("https://myapp.localhost:{}/", edge_addr.port()))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 502);
    assert!(resp.text().await.unwrap().contains("myapp"));
}
