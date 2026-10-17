use corvo_chat_proto::{Number, NumberError};

fn parsed(s: &str) -> Result<Number, NumberError> {
    Number::parse(s)
}

#[test]
fn from_body_appends_the_damm_digit() {
    assert_eq!(Number::from_body(482_910_557).unwrap().digits(), "4829105572");
    assert!(Number::from_body(99_999_999).is_none());
    assert!(Number::from_body(1_000_000_000).is_none());
}

#[test]
fn parse_accepts_spaces_and_dashes() {
    for s in ["4829105572", "482 910 5572", "482-910-5572", " 482 910 5572 "] {
        assert_eq!(parsed(s).unwrap().digits(), "4829105572", "{s:?}");
    }
}

#[test]
fn parse_rejects_typos() {
    assert_eq!(parsed("482 910 5573"), Err(NumberError::CheckDigit));
    assert_eq!(parsed("482 910 557"), Err(NumberError::Length));
    assert_eq!(parsed("0829105572"), Err(NumberError::LeadingZero));
    assert_eq!(parsed("48291055a2"), Err(NumberError::NotDigit));
    assert_eq!(NumberError::CheckDigit.to_string(), "that number has a typo");
}

#[test]
fn display_groups_three_three_four() {
    assert_eq!(parsed("4829105572").unwrap().to_string(), "482 910 5572");
}

#[test]
fn serde_uses_the_ten_digit_string() {
    let n: Number = serde_json::from_str("\"4829105572\"").unwrap();
    assert_eq!(serde_json::to_string(&n).unwrap(), "\"4829105572\"");
    assert!(serde_json::from_str::<Number>("\"4829105573\"").is_err());
}

#[test]
fn every_single_digit_error_and_adjacent_swap_is_caught() {
    let mut bodies = vec![100_000_000u32, 999_999_999];
    let mut x: u64 = 12345;
    while bodies.len() < 1002 {
        x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        bodies.push(100_000_000 + ((x >> 33) % 900_000_000) as u32);
    }
    for body in bodies {
        let good = Number::from_body(body).unwrap().digits().into_bytes();
        assert!(Number::parse(std::str::from_utf8(&good).unwrap()).is_ok());
        for (i, &orig) in good.iter().enumerate() {
            for d in b'0'..=b'9' {
                if d == orig {
                    continue;
                }
                let mut bad = good.clone();
                bad[i] = d;
                assert!(Number::parse(std::str::from_utf8(&bad).unwrap()).is_err(), "substitution in {body} at {i}");
            }
        }
        for (i, pair) in good.windows(2).enumerate() {
            if pair[0] == pair[1] {
                continue;
            }
            let mut bad = good.clone();
            bad.swap(i, i + 1);
            assert!(Number::parse(std::str::from_utf8(&bad).unwrap()).is_err(), "swap in {body} at {i}");
        }
    }
}
