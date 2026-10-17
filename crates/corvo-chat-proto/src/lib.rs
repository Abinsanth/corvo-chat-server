//! Every frame between the corvo-chat client and server (JSON text frames),
//! the protocol version and the 10-digit number with its Damm check digit.
//! It's the only code the client and the server share.

mod bytes;
pub mod names;
mod number;

pub use bytes::{B64, BLOB_MAX, Blob};
pub use names::NameError;
pub use number::{Number, NumberError};
