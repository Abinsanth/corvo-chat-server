use ed25519_dalek::{Signature, VerifyingKey};
use serde::{Deserialize, Serialize};

use crate::{B64, Number};

/// A device-independent bundle: the Ed25519 signing key, the X25519 exchange
/// key, and the signing key's signature over the exchange key.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct PublicKeys {
    pub signing: B64<32>,
    pub exchange: B64<32>,
    pub signature: B64<64>,
}

/// The bytes the signing key signs to vouch for an exchange key.
pub fn exchange_key_proof(exchange: &[u8; 32]) -> Vec<u8> {
    [b"corvo-chat exchange key v1".as_slice(), exchange].concat()
}

/// The bytes a chat's creator signs: the chat id, both numbers and both
/// sealed copies of the chat key, the creator's first. Each copy carries its
/// length as a big-endian `u16`.
pub fn chat_proof(chat_id: &[u8; 16], creator: Number, creator_key: &[u8], other: Number, other_key: &[u8]) -> Vec<u8> {
    let len = |key: &[u8]| u16::try_from(key.len()).expect("a sealed key fits in a u16").to_be_bytes();
    [
        b"corvo-chat chat v1".as_slice(),
        chat_id,
        creator.digits().as_bytes(),
        other.digits().as_bytes(),
        &len(creator_key),
        creator_key,
        &len(other_key),
        other_key,
    ]
    .concat()
}

impl PublicKeys {
    /// True when `signature` is the signing key's strict Ed25519 signature
    /// over the exchange key. A malformed key is just `false`.
    pub fn verify(&self) -> bool {
        self.verifies(&exchange_key_proof(&self.exchange.0), &self.signature)
    }

    /// True when `signature` is the signing key's strict Ed25519 signature
    /// over `message`. A malformed key is just `false`.
    pub fn verifies(&self, message: &[u8], signature: &B64<64>) -> bool {
        let Ok(key) = VerifyingKey::from_bytes(&self.signing.0) else { return false };
        key.verify_strict(message, &Signature::from_bytes(&signature.0)).is_ok()
    }
}
