//! TLS on our own socket: Let's Encrypt challenges are answered here, and every
//! other connection goes to the router with its channel binding attached.

use std::{path::PathBuf, sync::Arc, time::Duration};

use axum::{Router, extract::Request};
use futures::StreamExt;
use hyper::{body::Incoming, server::conn::http1, service::service_fn};
use hyper_util::rt::{TokioIo, TokioTimer};
use rustls::{ServerConfig, server::Acceptor};
use rustls_acme::{AcmeConfig, caches::DirCache, is_tls_alpn_challenge};
use tokio::net::{TcpListener, TcpStream};
use tokio_rustls::{LazyConfigAcceptor, server::TlsStream};
use tower::ServiceExt;

/// The RFC 9266 exporter label for TLS 1.3 channel binding.
pub const CHANNEL_BINDING_LABEL: &[u8] = b"EXPORTER-Channel-Binding";

/// How long a connection may take to finish its TLS handshake.
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// How long a connection may wait before sending a request's headers, so an
/// idle connection doesn't hold its socket forever.
pub const HEADER_TIMEOUT: Duration = Duration::from_secs(30);

/// This connection's TLS exporter value, in every request's extensions. Both
/// ends compute the same value; a middleman has a different one on each side.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChannelBinding(pub [u8; 32]);

/// Where the server's certificate comes from.
pub enum Tls {
    /// Let's Encrypt (production), cached in `cache_dir`.
    Acme { domain: String, cache_dir: PathBuf },
    /// A fixed certificate, for tests.
    Static(Arc<ServerConfig>),
}

/// Accepts connections forever; returns only if `accept` itself fails.
pub async fn serve(listener: TcpListener, tls: Tls, app: Router) -> anyhow::Result<()> {
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
        let (tcp, _) = listener.accept().await?;
        let (default, challenge, app) = (default.clone(), challenge.clone(), app.clone());
        tokio::spawn(async move {
            let Ok(Some(stream)) = tokio::time::timeout(HANDSHAKE_TIMEOUT, handshake(tcp, default, challenge)).await
            else {
                return;
            };
            let mut binding = [0; 32];
            if stream.get_ref().1.export_keying_material(&mut binding, CHANNEL_BINDING_LABEL, None).is_err() {
                return;
            }
            let binding = ChannelBinding(binding);
            let service = service_fn(move |mut req: Request<Incoming>| {
                req.extensions_mut().insert(binding);
                app.clone().oneshot(req)
            });
            // The header timer also runs while a connection sits idle, before or between requests.
            let _ = http1::Builder::new()
                .timer(TokioTimer::new())
                .header_read_timeout(HEADER_TIMEOUT)
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
