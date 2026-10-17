use ed25519_dalek::{Signature, VerifyingKey};
use serde::{Deserialize, Serialize};

use crate::B64;

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

impl PublicKeys {
    /// True when `signature` is the signing key's strict Ed25519 signature
    /// over the exchange key. A malformed key is just `false`.
    pub fn verify(&self) -> bool {
        let Ok(key) = VerifyingKey::from_bytes(&self.signing.0) else { return false };
        let signature = Signature::from_bytes(&self.signature.0);
        key.verify_strict(&exchange_key_proof(&self.exchange.0), &signature).is_ok()
    }
}
