use std::fmt;

use data_encoding::BASE64;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// The most bytes a [`Blob`] may hold after decoding.
pub const BLOB_MAX: usize = 1024;

/// `N` bytes carried as padded standard base64. The wrong length is an error.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct B64<const N: usize>(pub [u8; N]);

impl<const N: usize> fmt::Debug for B64<N> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "B64<{N}>(…)")
    }
}

impl<const N: usize> Serialize for B64<N> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&BASE64.encode(&self.0))
    }
}

impl<'de, const N: usize> Deserialize<'de> for B64<N> {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let bytes = decode(d)?;
        let bytes: [u8; N] = bytes.try_into().map_err(|_| serde::de::Error::custom(format!("expected {N} bytes")))?;
        Ok(B64(bytes))
    }
}

/// Up to [`BLOB_MAX`] bytes carried as padded standard base64.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct Blob(pub Vec<u8>);

impl fmt::Debug for Blob {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Blob(…)")
    }
}

impl Serialize for Blob {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&BASE64.encode(&self.0))
    }
}

impl<'de> Deserialize<'de> for Blob {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let bytes = decode(d)?;
        if bytes.len() > BLOB_MAX {
            return Err(serde::de::Error::custom(format!("more than {BLOB_MAX} bytes")));
        }
        Ok(Blob(bytes))
    }
}

fn decode<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
    let s = String::deserialize(d)?;
    BASE64.decode(s.as_bytes()).map_err(|_| serde::de::Error::custom("invalid base64"))
}
