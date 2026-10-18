use std::fmt;

use data_encoding::BASE64;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// The most bytes a [`Blob`] may hold after decoding.
pub const BLOB_MAX: usize = 1024;
/// The most bytes a [`Body`] may hold after decoding: 64 KiB of ciphertext.
pub const BODY_MAX: usize = 65_536;

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

/// Bytes of at most `$max` after decoding, carried as padded standard base64,
/// with a `Debug` that never shows them.
macro_rules! capped_bytes {
    ($(#[$doc:meta])* $name:ident, $max:ident) => {
        $(#[$doc])*
        #[derive(Clone, PartialEq, Eq, Hash)]
        pub struct $name(pub Vec<u8>);

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(concat!(stringify!($name), "(…)"))
            }
        }

        impl Serialize for $name {
            fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                s.serialize_str(&BASE64.encode(&self.0))
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                let bytes = decode(d)?;
                if bytes.len() > $max {
                    return Err(serde::de::Error::custom(format!("more than {} bytes", $max)));
                }
                Ok($name(bytes))
            }
        }
    };
}

capped_bytes!(
    /// Up to [`BLOB_MAX`] bytes carried as padded standard base64.
    Blob,
    BLOB_MAX
);

capped_bytes!(
    /// A message body: up to [`BODY_MAX`] bytes of ciphertext, as padded
    /// standard base64.
    Body,
    BODY_MAX
);

fn decode<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
    let s = String::deserialize(d)?;
    BASE64.decode(s.as_bytes()).map_err(|_| serde::de::Error::custom("invalid base64"))
}
