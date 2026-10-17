use corvo_chat_proto::names::{check_base, check_display_name, is_username};
use corvo_chat_proto::{B64, BLOB_MAX, Blob};

#[test]
fn base_is_lowercased_and_checked() {
    assert_eq!(check_base(" Abin ").unwrap(), "abin");
    for bad in ["ab", "abcdefghijklmnopqrstu", "1abin", "ab-in", "abín"] {
        assert!(check_base(bad).is_err(), "{bad:?}");
    }
}

#[test]
fn username_needs_a_valid_ending() {
    assert!(is_username("abin-7k3q"));
    for bad in ["abin-7k3", "abin-7k3i", "Abin-7k3q", "abin7k3q"] {
        assert!(!is_username(bad), "{bad:?}");
    }
}

#[test]
fn display_name_is_trimmed_and_limited() {
    assert_eq!(check_display_name("  Rahul ").unwrap(), "Rahul");
    assert!(check_display_name("").is_err());
    assert!(check_display_name(&"a".repeat(33)).is_err());
    assert!(check_display_name("a\u{7}").is_err());
    assert!(check_display_name(&"😀".repeat(32)).is_ok());
}

#[test]
fn blob_over_1024_bytes_is_refused() {
    let ok = Blob(vec![1; BLOB_MAX]);
    let json = serde_json::to_string(&ok).unwrap();
    assert_eq!(serde_json::from_str::<Blob>(&json).unwrap(), ok);
    let json = serde_json::to_string(&Blob(vec![1; BLOB_MAX + 1])).unwrap();
    assert!(serde_json::from_str::<Blob>(&json).is_err());
}

#[test]
fn b64_debug_hides_the_bytes() {
    assert!(!format!("{:?}", B64([7u8; 32])).contains('7'));
    assert!(!format!("{:?}", Blob(vec![7; 8])).contains('7'));
}

#[test]
fn b64_rejects_the_wrong_length() {
    let json = serde_json::to_string(&B64([1u8; 16])).unwrap();
    assert!(serde_json::from_str::<B64<32>>(&json).is_err());
    assert_eq!(serde_json::from_str::<B64<16>>(&json).unwrap(), B64([1u8; 16]));
}
