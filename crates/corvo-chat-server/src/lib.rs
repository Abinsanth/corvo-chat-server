//! The corvo-chat server. It ends TLS itself, gets its own Let's Encrypt
//! certificate, and only ever stores and forwards ciphertext.

mod tls;

use axum::{Router, routing::get};

pub use tls::{CHANNEL_BINDING_LABEL, ChannelBinding, HANDSHAKE_TIMEOUT, HEADER_TIMEOUT, Tls, serve};

/// Every route the server answers.
pub fn app() -> Router {
    Router::new().route("/", get(|| async { concat!("corvo-chat-server ", env!("CARGO_PKG_VERSION")) }))
}
