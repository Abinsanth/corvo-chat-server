//! Every frame between the corvo-chat client and server (JSON text frames),
//! the protocol version and the 10-digit number with its Damm check digit.
//! It's the only code the client and the server share.

mod bytes;
pub mod chat;
pub mod frames;
mod keys;
pub mod names;
mod number;
pub mod opaque;

/// The protocol version a client says in its `hello`.
pub const PROTOCOL_VERSION: u32 = 1;

pub use bytes::{B64, BLOB_MAX, BODY_MAX, Blob, Body};
pub use keys::{PublicKeys, chat_proof, exchange_key_proof};
pub use names::NameError;
pub use number::{Number, NumberError};
