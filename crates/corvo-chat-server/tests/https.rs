use std::{net::SocketAddr, sync::Arc, time::Duration};

use axum::{Extension, Router, routing::get};
use corvo_chat_server::{CHANNEL_BINDING_LABEL, ChannelBinding, Tls, app, serve};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    time::timeout,
};
use tokio_rustls::{TlsConnector, client::TlsStream};

fn ring() -> Arc<rustls::crypto::CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

fn self_signed() -> (Arc<rustls::ServerConfig>, CertificateDer<'static>) {
    let cert = rcgen::generate_simple_self_signed(["localhost".to_string()]).expect("a test certificate");
    let der = cert.cert.der().clone();
    let key = PrivateKeyDer::Pkcs8(cert.signing_key.serialize_der().into());
    let config = rustls::ServerConfig::builder_with_provider(ring())
        .with_safe_default_protocol_versions()
        .expect("ring supports the defaults")
        .with_no_client_auth()
        .with_single_cert(vec![der.clone()], key)
        .expect("the test certificate is valid");
    (Arc::new(config), der)
}

/// Serves `app` on a free local port with a fresh self-signed certificate.
async fn start(app: Router) -> (SocketAddr, CertificateDer<'static>) {
    let (config, der) = self_signed();
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("a free port");
    let addr = listener.local_addr().expect("a bound address");
    tokio::spawn(serve(listener, Tls::Static(config), app));
    (addr, der)
}

async fn connect(addr: SocketAddr, der: CertificateDer<'static>) -> TlsStream<TcpStream> {
    let mut roots = rustls::RootCertStore::empty();
    roots.add(der).expect("the test certificate is a valid root");
    let config = rustls::ClientConfig::builder_with_provider(ring())
        .with_safe_default_protocol_versions()
        .expect("ring supports the defaults")
        .with_root_certificates(roots)
        .with_no_client_auth();
    let tcp = TcpStream::connect(addr).await.expect("the server accepts");
    let name = ServerName::try_from("localhost").expect("a valid name");
    TlsConnector::from(Arc::new(config)).connect(name, tcp).await.expect("the TLS handshake succeeds")
}

async fn fetch(stream: &mut TlsStream<TcpStream>, path: &str) -> String {
    let request = format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n");
    stream.write_all(request.as_bytes()).await.expect("the request is sent");
    let mut response = String::new();
    stream.read_to_string(&mut response).await.expect("the response arrives");
    response
}

fn hex(bytes: [u8; 32]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[tokio::test]
async fn root_says_name_and_version() {
    let (addr, der) = start(app()).await;
    let response = fetch(&mut connect(addr, der).await, "/").await;
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    assert!(response.ends_with(&format!("corvo-chat-server {}", env!("CARGO_PKG_VERSION"))), "{response}");
}

#[tokio::test]
async fn handler_sees_the_same_channel_binding_as_the_client() {
    let echo = app().route("/cb", get(|Extension(ChannelBinding(b)): Extension<ChannelBinding>| async move { hex(b) }));
    let (addr, der) = start(echo).await;
    let mut stream = connect(addr, der).await;
    let mut mine = [0; 32];
    stream.get_ref().1.export_keying_material(&mut mine, CHANNEL_BINDING_LABEL, None).expect("the client exports");
    let response = fetch(&mut stream, "/cb").await;
    assert!(response.ends_with(&hex(mine)), "{response}");
}

#[tokio::test]
async fn plain_http_does_not_stop_the_server() {
    let (addr, der) = start(app()).await;
    let mut plain = TcpStream::connect(addr).await.expect("the server accepts");
    plain.write_all(b"GET / HTTP/1.1\r\nHost: x\r\n\r\n").await.expect("the request is sent");
    let _ = plain.read_to_end(&mut Vec::new()).await;
    let response = fetch(&mut connect(addr, der).await, "/").await;
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
}

#[tokio::test(start_paused = true)]
async fn silent_connection_is_closed_after_the_handshake_timeout() {
    let (addr, _) = start(app()).await;
    let mut tcp = TcpStream::connect(addr).await.expect("the server accepts");
    let mut buf = [0; 1];
    assert!(timeout(Duration::from_secs(9), tcp.read(&mut buf)).await.is_err(), "closed before 9 s");
    let read = timeout(Duration::from_secs(2), tcp.read(&mut buf)).await.expect("closed by 11 s");
    assert_eq!(read.expect("a clean close"), 0);
}

#[tokio::test(start_paused = true)]
async fn client_that_stalls_after_its_hello_is_closed_after_the_handshake_timeout() {
    let (addr, der) = start(app()).await;
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
