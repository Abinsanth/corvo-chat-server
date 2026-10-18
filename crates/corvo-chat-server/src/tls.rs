//! TLS on our own socket: Let's Encrypt challenges are answered here, and every
//! other connection goes to the router with its peer address attached.

use std::{net::SocketAddr, path::PathBuf, sync::Arc, time::Duration};

use axum::{Router, extract::Request};
use futures::StreamExt;
use hyper::{body::Incoming, server::conn::http1, service::service_fn};
use hyper_util::rt::{TokioIo, TokioTimer};
use rustls::{ServerConfig, server::Acceptor};
use rustls_acme::{AcmeConfig, caches::DirCache, is_tls_alpn_challenge};
use tokio::net::{TcpListener, TcpStream};
use tokio_rustls::{LazyConfigAcceptor, server::TlsStream};
use tower::ServiceExt;

/// How long a connection may take to finish its TLS handshake.
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// How long to wait after a failed `accept` (usually out of file handles) before trying again.
const ACCEPT_RETRY: Duration = Duration::from_millis(100);

/// How long a connection may wait before sending a request's headers, so an
/// idle connection doesn't hold its socket forever.
pub const HEADER_TIMEOUT: Duration = Duration::from_secs(30);

/// The address a connection comes from, in every request's extensions. The
/// per-IP limits key on its IP.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PeerAddr(pub SocketAddr);

/// Where the server's certificate comes from.
pub enum Tls {
    /// Let's Encrypt (production), cached in `cache_dir`.
    Acme { domain: String, cache_dir: PathBuf },
    /// A fixed certificate, for tests.
    Static(Arc<ServerConfig>),
}

/// How long a connection may take over its TLS handshake, and over each
/// request's headers (also while it sits idle between requests).
#[derive(Clone, Copy, Debug)]
pub struct Timeouts {
    pub handshake: Duration,
    pub header: Duration,
}

impl Default for Timeouts {
    fn default() -> Timeouts {
        Timeouts { handshake: HANDSHAKE_TIMEOUT, header: HEADER_TIMEOUT }
    }
}

/// Accepts connections forever. A failed `accept` is logged and retried, never returned.
pub async fn serve(listener: TcpListener, tls: Tls, app: Router) {
    serve_with(listener, tls, app, Timeouts::default()).await
}

/// [`serve`] with chosen timeouts.
pub async fn serve_with(listener: TcpListener, tls: Tls, app: Router, timeouts: Timeouts) {
    let (default, challenge) = match tls {
        Tls::Static(config) => (config, None),
        Tls::Acme { domain, cache_dir } => {
            let mut state =
                AcmeConfig::new([domain]).cache(DirCache::new(cache_dir)).directory_lets_encrypt(true).state();
            let configs = (state.default_rustls_config(), Some(state.challenge_rustls_config()));
            // Errors (DNS not ready, Let's Encrypt down) are retried by rustls-acme: log them, never exit.
            tokio::spawn(async move {
                while let Some(event) = state.next().await {
                    match event {
                        Ok(ok) => tracing::info!("certificate: {ok:?}"),
                        Err(err) => tracing::warn!("certificate: {err:?}"),
                    }
                }
            });
            configs
        }
    };
    loop {
        let (tcp, peer) = match listener.accept().await {
            Ok((tcp, addr)) => (tcp, PeerAddr(addr)),
            // Out of file handles, or a connection that died before we took it: wait a moment, keep serving.
            Err(err) => {
                tracing::warn!("accept: {err}");
                tokio::time::sleep(ACCEPT_RETRY).await;
                continue;
            }
        };
        let (default, challenge, app) = (default.clone(), challenge.clone(), app.clone());
        tokio::spawn(async move {
            let Ok(Some(stream)) = tokio::time::timeout(timeouts.handshake, handshake(tcp, default, challenge)).await
            else {
                return;
            };
            let service = service_fn(move |mut req: Request<Incoming>| {
                req.extensions_mut().insert(peer);
                app.clone().oneshot(req)
            });
            // The header timer also runs while a connection sits idle, before or between requests.
            let _ = http1::Builder::new()
                .timer(TokioTimer::new())
                .header_read_timeout(timeouts.header)
                .serve_connection(TokioIo::new(stream), service)
                .with_upgrades()
                .await;
        });
    }
}

/// The TLS handshake. `None` for a Let's Encrypt challenge (answered here) or a
/// failed handshake.
async fn handshake(
    tcp: TcpStream,
    default: Arc<ServerConfig>,
    challenge: Option<Arc<ServerConfig>>,
) -> Option<TlsStream<TcpStream>> {
    let start = LazyConfigAcceptor::new(Acceptor::default(), tcp).await.ok()?;
    if let Some(challenge) = challenge.filter(|_| is_tls_alpn_challenge(&start.client_hello())) {
        let _ = start.into_stream(challenge).await;
        return None;
    }
    start.into_stream(default).await.ok()
}
