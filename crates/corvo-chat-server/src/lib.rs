//! The corvo-chat server. It ends TLS itself, gets its own Let's Encrypt
//! certificate, and only ever stores and forwards ciphertext.

pub mod accounts;
pub mod admin;
pub mod attempts;
pub mod cleanup;
pub mod clock;
pub mod db;
pub mod invites;
pub mod key;
pub mod limits;
pub mod sessions;
pub mod state;
mod tls;

use axum::{Router, routing::get};

pub use state::AppState;
pub use tls::{CHANNEL_BINDING_LABEL, ChannelBinding, HANDSHAKE_TIMEOUT, HEADER_TIMEOUT, Tls, serve};

/// Every route the server answers.
pub fn app(state: AppState) -> Router {
    Router::new()
        .route("/", get(|| async { concat!("corvo-chat-server ", env!("CARGO_PKG_VERSION")) }))
        .with_state(state)
}
