//! Shared by the integration tests: a server on a free port with a self-signed
//! certificate, and a client that trusts only that certificate.
#![allow(dead_code)] // each test binary uses only some of these

use std::{net::SocketAddr, sync::Arc};

use axum::Router;
use corvo_chat_proto::opaque::Suite;
use corvo_chat_server::{
    AppState, Tls,
    clock::Clock,
    db::{self, Db},
    limits::Limits,
    serve,
};
use opaque_ke::ServerSetup;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName};
use tempfile::TempDir;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};
use tokio_rustls::{TlsConnector, client::TlsStream};

pub fn ring() -> Arc<rustls::crypto::CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

pub fn self_signed() -> (Arc<rustls::ServerConfig>, CertificateDer<'static>) {
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
pub async fn start(app: Router) -> (SocketAddr, CertificateDer<'static>) {
    let (config, der) = self_signed();
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("a free port");
    let addr = listener.local_addr().expect("a bound address");
    tokio::spawn(serve(listener, Tls::Static(config), app));
    (addr, der)
}

pub async fn connect(addr: SocketAddr, der: CertificateDer<'static>) -> TlsStream<TcpStream> {
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

pub async fn fetch(stream: &mut TlsStream<TcpStream>, path: &str) -> String {
    let request = format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n");
    stream.write_all(request.as_bytes()).await.expect("the request is sent");
    let mut response = String::new();
    stream.read_to_string(&mut response).await.expect("the response arrives");
    response
}

/// A fresh key and database in a temp dir, with a clock stopped at
/// 1_800_000_000. Keep the `TempDir` alive as long as the state is used.
pub fn state(limits: Limits) -> (AppState, TempDir) {
    let dir = tempfile::tempdir().expect("a temp dir");
    let conn = db::open(&dir.path().join(db::DB_FILE)).expect("a database");
    let state = AppState {
        db: Db::new(conn),
        setup: Arc::new(ServerSetup::<Suite>::new(&mut rand_core::OsRng)),
        clock: Clock::manual(1_800_000_000),
        limits: Arc::new(limits),
    };
    (state, dir)
}
