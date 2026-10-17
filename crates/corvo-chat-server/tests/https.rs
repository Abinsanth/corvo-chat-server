mod common;

use std::{sync::Arc, time::Duration};

use axum::{Extension, routing::get};
use common::{connect, fetch, ring, start, state};
use corvo_chat_server::{CHANNEL_BINDING_LABEL, ChannelBinding, app, limits::Limits};
use rustls::pki_types::ServerName;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    time::timeout,
};

fn hex(bytes: [u8; 32]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[tokio::test]
async fn root_says_name_and_version() {
    let (addr, der) = start(app(state(Limits::default()).0)).await;
    let response = fetch(&mut connect(addr, der).await, "/").await;
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    assert!(response.ends_with(&format!("corvo-chat-server {}", env!("CARGO_PKG_VERSION"))), "{response}");
}

#[tokio::test]
async fn handler_sees_the_same_channel_binding_as_the_client() {
    let echo = app(state(Limits::default()).0)
        .route("/cb", get(|Extension(ChannelBinding(b)): Extension<ChannelBinding>| async move { hex(b) }));
    let (addr, der) = start(echo).await;
    let mut stream = connect(addr, der).await;
    let mut mine = [0; 32];
    stream.get_ref().1.export_keying_material(&mut mine, CHANNEL_BINDING_LABEL, None).expect("the client exports");
    let response = fetch(&mut stream, "/cb").await;
    assert!(response.ends_with(&hex(mine)), "{response}");
}

#[tokio::test]
async fn plain_http_does_not_stop_the_server() {
    let (addr, der) = start(app(state(Limits::default()).0)).await;
    let mut plain = TcpStream::connect(addr).await.expect("the server accepts");
    plain.write_all(b"GET / HTTP/1.1\r\nHost: x\r\n\r\n").await.expect("the request is sent");
    let _ = plain.read_to_end(&mut Vec::new()).await;
    let response = fetch(&mut connect(addr, der).await, "/").await;
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
}

#[tokio::test(start_paused = true)]
async fn silent_connection_is_closed_after_the_handshake_timeout() {
    let (addr, _) = start(app(state(Limits::default()).0)).await;
    let mut tcp = TcpStream::connect(addr).await.expect("the server accepts");
    let mut buf = [0; 1];
    assert!(timeout(Duration::from_secs(9), tcp.read(&mut buf)).await.is_err(), "closed before 9 s");
    let read = timeout(Duration::from_secs(2), tcp.read(&mut buf)).await.expect("closed by 11 s");
    assert_eq!(read.expect("a clean close"), 0);
}

#[tokio::test(start_paused = true)]
async fn client_that_stalls_after_its_hello_is_closed_after_the_handshake_timeout() {
    let (addr, der) = start(app(state(Limits::default()).0)).await;
    let mut roots = rustls::RootCertStore::empty();
    roots.add(der).expect("the test certificate is a valid root");
    let config = rustls::ClientConfig::builder_with_provider(ring())
        .with_safe_default_protocol_versions()
        .expect("ring supports the defaults")
        .with_root_certificates(roots)
        .with_no_client_auth();
    let name = ServerName::try_from("localhost").expect("a valid name");
    let mut client = rustls::ClientConnection::new(Arc::new(config), name).expect("a client");
    let mut hello = Vec::new();
    client.write_tls(&mut hello).expect("a ClientHello");
    let mut tcp = TcpStream::connect(addr).await.expect("the server accepts");
    tcp.write_all(&hello).await.expect("the hello is sent");
    // Never answer the server's reply: drain it until the server gives up.
    let closed = timeout(Duration::from_secs(11), async {
        let mut buf = [0; 4096];
        while tcp.read(&mut buf).await.is_ok_and(|n| n > 0) {}
    });
    closed.await.expect("closed by 11 s");
}

#[tokio::test]
async fn connection_that_sends_no_request_is_closed_after_the_header_timeout() {
    let (addr, der) = start(app(state(Limits::default()).0)).await;
    let mut stream = connect(addr, der).await;
    // Let the server finish its side of the handshake in real time; a paused clock
    // could jump ahead of it and start its timer late.
    tokio::time::sleep(Duration::from_millis(100)).await;
    tokio::time::pause();
    let mut buf = [0; 1];
    assert!(timeout(Duration::from_secs(29), stream.read(&mut buf)).await.is_err(), "closed before 29 s");
    let read = timeout(Duration::from_secs(2), stream.read(&mut buf)).await.expect("closed by 31 s");
    assert!(read.is_err() || read.is_ok_and(|n| n == 0));
}

#[tokio::test]
async fn connection_idle_after_a_request_is_closed_after_the_header_timeout() {
    let (addr, der) = start(app(state(Limits::default()).0)).await;
    let mut stream = connect(addr, der).await;
    stream.write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\n\r\n").await.expect("the request is sent");
    let mut buf = [0; 4096];
    let n = stream.read(&mut buf).await.expect("the response arrives");
    assert!(String::from_utf8_lossy(&buf[..n]).starts_with("HTTP/1.1 200"));
    tokio::time::sleep(Duration::from_millis(100)).await;
    tokio::time::pause();
    let closed = timeout(Duration::from_secs(31), async { while stream.read(&mut buf).await.is_ok_and(|n| n > 0) {} });
    closed.await.expect("closed by 31 s");
}
