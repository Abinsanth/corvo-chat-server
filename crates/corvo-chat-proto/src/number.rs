use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Damm (2004) quasigroup of order 10: `TABLE[interim][digit]`.
const TABLE: [[u8; 10]; 10] = [
    [0, 3, 1, 7, 5, 9, 8, 6, 4, 2],
    [7, 0, 9, 2, 1, 5, 4, 8, 6, 3],
    [4, 2, 0, 6, 8, 7, 1, 3, 5, 9],
    [1, 7, 5, 0, 9, 8, 3, 4, 2, 6],
    [6, 1, 2, 3, 0, 4, 5, 9, 7, 8],
    [3, 6, 7, 4, 2, 0, 9, 5, 8, 1],
    [5, 8, 6, 9, 7, 2, 0, 1, 3, 4],
    [8, 9, 4, 5, 3, 6, 2, 0, 1, 7],
    [9, 4, 3, 8, 6, 1, 7, 2, 0, 5],
    [2, 5, 8, 1, 4, 3, 6, 7, 9, 0],
];

/// The interim digit after running `digits` through the table from 0. For a
/// body it is the check digit; for a full number it is 0 when valid.
fn damm(digits: impl IntoIterator<Item = u8>) -> u8 {
    digits.into_iter().fold(0, |interim, d| TABLE[interim as usize][d as usize])
}

/// A 10-digit account number: nine digits (not starting with 0) and a Damm check digit.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Number(u64);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NumberError {
    Length,
    LeadingZero,
    NotDigit,
    CheckDigit,
}

impl fmt::Display for NumberError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("that number has a typo")
    }
}

impl std::error::Error for NumberError {}

impl Number {
    /// Reads a number typed by a person; spaces and `-` are ignored.
    pub fn parse(s: &str) -> Result<Number, NumberError> {
        let mut digits = Vec::with_capacity(10);
        for c in s.chars().filter(|c| *c != ' ' && *c != '-') {
            digits.push(c.to_digit(10).filter(|_| c.is_ascii_digit()).ok_or(NumberError::NotDigit)? as u8);
        }
        if digits.len() != 10 {
            return Err(NumberError::Length);
        }
        if digits[0] == 0 {
            return Err(NumberError::LeadingZero);
        }
        if damm(digits.iter().copied()) != 0 {
            return Err(NumberError::CheckDigit);
        }
        Ok(Number(digits.iter().fold(0u64, |n, d| n * 10 + u64::from(*d))))
    }

    /// Builds a number from its nine-digit body by appending the check digit.
    pub fn from_body(body: u32) -> Option<Number> {
        if !(100_000_000..=999_999_999).contains(&body) {
            return None;
        }
        let check = damm(body.to_string().bytes().map(|b| b - b'0'));
        Some(Number(u64::from(body) * 10 + u64::from(check)))
    }

    /// The 10 digits, for storage.
    pub fn digits(&self) -> String {
        format!("{:010}", self.0)
    }
}

impl fmt::Display for Number {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let d = self.digits();
        write!(f, "{} {} {}", &d[..3], &d[3..6], &d[6..])
    }
}

impl Serialize for Number {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.digits())
    }
}

impl<'de> Deserialize<'de> for Number {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Number, D::Error> {
        let s = String::deserialize(d)?;
        Number::parse(&s).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::damm;

    #[test]
    fn damm_check_digit_of_572_is_4() {
        assert_eq!(damm([5, 7, 2]), 4);
    }
}
