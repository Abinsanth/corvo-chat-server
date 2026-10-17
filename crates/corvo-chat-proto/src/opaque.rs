use argon2::{Algorithm, Argon2, Params, Version};
use hkdf::Hkdf;
use opaque_ke::{CipherSuite, Ristretto255, TripleDh};
use sha2::{Sha256, Sha512};

use crate::B64;

/// The OPAQUE cipher suite for passwords and recovery codes. The Argon2
/// settings are not part of the suite: both sides pass them at `finish`, from
/// [`password_ksf`] or [`recovery_ksf`].
pub struct Suite;

impl CipherSuite for Suite {
    type OprfCs = Ristretto255;
    type KeyExchange = TripleDh<Ristretto255, Sha512>;
    type Ksf = Argon2<'static>;
}

fn argon2id(m_cost: u32, t_cost: u32, p_cost: u32) -> Argon2<'static> {
    let params = Params::new(m_cost, t_cost, p_cost, None).expect("the Argon2 settings are valid");
    Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
}

/// The password's key stretching: Argon2id, 64 MiB, 3 passes, 1 lane.
pub fn password_ksf() -> Argon2<'static> {
    argon2id(65_536, 3, 1)
}

/// The recovery code's key stretching. The code has 120 bits, so the slow
/// step isn't needed.
pub fn recovery_ksf() -> Argon2<'static> {
    argon2id(8, 1, 1)
}

/// The OPAQUE credential identifier of a username's password.
pub fn password_id(username: &str) -> Vec<u8> {
    [b"password:".as_slice(), username.as_bytes()].concat()
}

/// The OPAQUE credential identifier of a username's recovery code.
pub fn recovery_id(username: &str) -> Vec<u8> {
    [b"recovery:".as_slice(), username.as_bytes()].concat()
}

/// The key that locks the account's secrets, from OPAQUE's export key. The
/// server never calls it; it's here so every client derives it the same way.
pub fn wrap_key(export_key: &[u8]) -> [u8; 32] {
    let mut key = [0; 32];
    Hkdf::<Sha256>::new(None, export_key)
        .expand(b"corvo-chat wrap v1", &mut key)
        .expect("32 bytes is a valid HKDF output length");
    key
}

pub type RegRequest = B64<32>;
pub type RegResponse = B64<64>;
pub type RegUpload = B64<192>;
pub type CredRequest = B64<96>;
pub type CredResponse = B64<320>;
pub type CredFinal = B64<64>;
