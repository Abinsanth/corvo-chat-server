use std::fmt;

/// Crockford base32, lowercase: no `i l o u`.
pub const ENDING_ALPHABET: &[u8; 32] = b"0123456789abcdefghjkmnpqrstvwxyz";
pub const ENDING_LEN: usize = 4;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NameError {
    Length,
    Characters,
    FirstNotLetter,
}

impl fmt::Display for NameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            NameError::Length => "that name is too short or too long",
            NameError::Characters => "that name has characters that aren't allowed",
            NameError::FirstNotLetter => "that name must start with a letter",
        })
    }
}

impl std::error::Error for NameError {}

/// Trims and lowercases a username base: 3-20 characters from `a-z 0-9 _ .`,
/// the first one a letter.
pub fn check_base(input: &str) -> Result<String, NameError> {
    let base = input.trim().to_lowercase();
    if !(3..=20).contains(&base.chars().count()) {
        return Err(NameError::Length);
    }
    if !base.chars().all(|c| matches!(c, 'a'..='z' | '0'..='9' | '_' | '.')) {
        return Err(NameError::Characters);
    }
    if !base.starts_with(|c: char| c.is_ascii_lowercase()) {
        return Err(NameError::FirstNotLetter);
    }
    Ok(base)
}

/// True for `<valid base>-<4 ending characters>`, already lowercase.
pub fn is_username(s: &str) -> bool {
    let Some((base, ending)) = s.rsplit_once('-') else { return false };
    check_base(base).is_ok_and(|b| b == base)
        && ending.len() == ENDING_LEN
        && ending.bytes().all(|b| ENDING_ALPHABET.contains(&b))
}

fn check_text(input: &str, max: usize) -> Result<String, NameError> {
    let name = input.trim();
    if !(1..=max).contains(&name.chars().count()) {
        return Err(NameError::Length);
    }
    if name.chars().any(char::is_control) {
        return Err(NameError::Characters);
    }
    Ok(name.to_string())
}

/// Trimmed, 1-32 characters, no control characters.
pub fn check_display_name(input: &str) -> Result<String, NameError> {
    check_text(input, 32)
}

/// Trimmed, 1-64 characters, no control characters.
pub fn check_device_name(input: &str) -> Result<String, NameError> {
    check_text(input, 64)
}
