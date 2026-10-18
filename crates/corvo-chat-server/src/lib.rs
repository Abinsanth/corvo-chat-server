//! The corvo-chat server. It ends TLS itself, gets its own Let's Encrypt
//! certificate, and only ever stores and forwards ciphertext.

pub mod accounts;
pub mod admin;
pub mod attempts;
pub mod cleanup;
pub mod clock;
pub mod db;
pub mod hub;
pub mod invites;
pub mod key;
pub mod limits;
pub mod pins;
pub mod relations;
pub mod sessions;
pub mod state;
mod tls;
pub mod ws;

use axum::{Router, routing::get};

pub use state::AppState;
pub use tls::{HANDSHAKE_TIMEOUT, HEADER_TIMEOUT, PeerAddr, Tls, serve};

/// Every route the server answers.
pub fn app(state: AppState) -> Router {
    Router::new()
        .route("/", get(|| async { concat!("corvo-chat-server ", env!("CARGO_PKG_VERSION")) }))
        .route(ws::PATH, get(ws::upgrade))
        .with_state(state)
}
