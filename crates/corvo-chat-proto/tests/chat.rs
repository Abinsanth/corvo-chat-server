use corvo_chat_proto::{B64, BODY_MAX, Body, Number, PublicKeys, chat_proof, exchange_key_proof};
use ed25519_dalek::{Signer, SigningKey};

fn creator() -> Number {
    Number::parse("482 910 5572").unwrap()
}

fn other() -> Number {
    Number::from_body(100_000_000).unwrap()
}

fn public_keys(sk: &SigningKey) -> PublicKeys {
    let exchange = [42u8; 32];
    PublicKeys {
        signing: B64(sk.verifying_key().to_bytes()),
        exchange: B64(exchange),
        signature: B64(sk.sign(&exchange_key_proof(&exchange)).to_bytes()),
    }
}

#[test]
fn body_of_64_kib_passes_and_one_more_byte_fails() {
    assert_eq!(BODY_MAX, 65_536);
    let full = serde_json::to_string(&Body(vec![1; BODY_MAX])).unwrap();
    assert_eq!(serde_json::from_str::<Body>(&full).unwrap(), Body(vec![1; BODY_MAX]));
    let over = serde_json::to_string(&Body(vec![1; BODY_MAX + 1])).unwrap();
    assert!(serde_json::from_str::<Body>(&over).is_err());
    assert!(!format!("{:?}", Body(vec![7; 3])).contains('7'));
}

#[test]
fn chat_proof_has_the_documented_layout() {
    let proof = chat_proof(&[1; 16], creator(), &[2; 3], other(), &[3; 1]);
    let expected = [
        b"corvo-chat chat v1".as_slice(),
        &[1; 16],
        creator().digits().as_bytes(),
        other().digits().as_bytes(),
        &[0, 3, 2, 2, 2, 0, 1, 3],
    ]
    .concat();
    assert_eq!(proof, expected);
}

#[test]
fn chat_signature_verifies_only_for_its_own_proof() {
    let sk = SigningKey::from_bytes(&[11; 32]);
    let keys = public_keys(&sk);
    let (mine, theirs) = (vec![5u8; 104], vec![6u8; 104]);
    let proof = chat_proof(&[1; 16], creator(), &mine, other(), &theirs);
    let signature = B64(sk.sign(&proof).to_bytes());
    assert!(keys.verifies(&proof, &signature));

    let mut flipped = theirs.clone();
    flipped[0] ^= 1;
    assert!(!keys.verifies(&chat_proof(&[1; 16], creator(), &mine, other(), &flipped), &signature));

    let stranger = public_keys(&SigningKey::from_bytes(&[12; 32]));
    assert!(!stranger.verifies(&proof, &signature));
}
