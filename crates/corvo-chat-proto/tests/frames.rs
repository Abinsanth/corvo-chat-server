use corvo_chat_proto::chat::*;
use corvo_chat_proto::exchange_key_proof;
use corvo_chat_proto::frames::*;
use corvo_chat_proto::opaque::{CredFinal, CredRequest, CredResponse, RegRequest, RegResponse, RegUpload};
use corvo_chat_proto::{B64, Blob, Body, Number, PROTOCOL_VERSION, PublicKeys};
use ed25519_dalek::{Signer, SigningKey};

fn number() -> Number {
    Number::parse("482 910 5572").unwrap()
}

fn device() -> Device {
    Device { id: B64([1; 16]), kind: DeviceKind::Cli, name: "laptop".into() }
}

fn keys() -> PublicKeys {
    PublicKeys { signing: B64([2; 32]), exchange: B64([3; 32]), signature: B64([4; 64]) }
}

fn account(pending_until: Option<i64>) -> Account {
    Account { username: "abin-7k3q".into(), number: number(), display_name: "Abin".into(), pending_until }
}

fn invite() -> InviteInfo {
    InviteInfo { number: number(), display_name: "Sam".into(), code: "ABCD-EFGH".into(), expires_at: 99 }
}

fn blob() -> Blob {
    Blob(vec![9; 40])
}

fn req() -> RegRequest {
    B64([5; 32])
}
fn resp() -> RegResponse {
    B64([6; 64])
}
fn upload() -> RegUpload {
    B64([7; 192])
}
fn cred_req() -> CredRequest {
    B64([8; 96])
}
fn cred_final() -> CredFinal {
    B64([9; 64])
}

fn message() -> Message {
    Message {
        chat_id: B64([1; 16]),
        seq: 3,
        msg_id: B64([2; 16]),
        from: number(),
        body: Body(vec![8; 50]),
        sent_at: 10,
    }
}

fn chat_info(marks: Option<Marks>) -> ChatInfo {
    ChatInfo {
        chat_id: B64([1; 16]),
        with: number(),
        keys: vec![SealedKey { number: number(), key: blob() }, SealedKey { number: number(), key: blob() }],
        signature: B64([4; 64]),
        last_seq: 9,
        read_through: 4,
        cleared_through: 1,
        marks,
    }
}

fn person(display_name: Option<String>, relation: Relation) -> Person {
    Person { number: number(), display_name, public_keys: keys(), relation }
}

fn pin(pin: Option<Blob>) -> Pin {
    Pin { number: number(), pin }
}

fn device_info() -> DeviceInfo {
    DeviceInfo {
        id: B64([1; 16]),
        kind: DeviceKind::Phone,
        name: "p".into(),
        first_seen: 1,
        last_used: 2,
        this_device: true,
    }
}

#[test]
fn hello_has_the_documented_wire_format() {
    let hello = ClientFrame::Hello { protocol_version: PROTOCOL_VERSION, app_version: "0.1.0".into() };
    assert_eq!(
        serde_json::to_string(&hello).unwrap(),
        r#"{"type":"hello","protocol_version":1,"app_version":"0.1.0"}"#
    );
}

#[test]
fn every_frame_round_trips() {
    let clients = vec![
        ClientFrame::Hello { protocol_version: 1, app_version: "0.1.0".into() },
        ClientFrame::SignupStart {
            base: "abin".into(),
            display_name: "Abin".into(),
            device: device(),
            password: req(),
            recovery: req(),
        },
        ClientFrame::SignupFinish {
            password: upload(),
            recovery: upload(),
            password_copy: blob(),
            recovery_copy: blob(),
            public_keys: keys(),
        },
        ClientFrame::LoginStart { username: "abin-7k3q".into(), request: cred_req(), device: device() },
        ClientFrame::LoginFinish { finalization: cred_final() },
        ClientFrame::RecoveryStart { username: "abin-7k3q".into(), request: cred_req(), device: device() },
        ClientFrame::RecoveryFinish { finalization: cred_final() },
        ClientFrame::Resume { token: B64([1; 32]), device_id: B64([2; 16]) },
        ClientFrame::ReauthStart { request: cred_req() },
        ClientFrame::ReauthFinish { finalization: cred_final() },
        ClientFrame::Reregister { password: Some(req()), recovery: None },
        ClientFrame::RecoveryComplete {
            password: upload(),
            recovery: upload(),
            password_copy: blob(),
            recovery_copy: blob(),
        },
        ClientFrame::ChangePassword { password: upload(), password_copy: blob() },
        ClientFrame::NewRecoveryCode { recovery: upload(), recovery_copy: blob() },
        ClientFrame::Activate { code: "ABCD-EFGH".into() },
        ClientFrame::Invite { number: number() },
        ClientFrame::ListInvites,
        ClientFrame::ListDevices,
        ClientFrame::SignOutDevice { id: B64([3; 16]) },
        ClientFrame::SignOut,
        ClientFrame::Sync { cursor: Some(4) },
        ClientFrame::Sync { cursor: None },
        ClientFrame::History { chat_id: B64([1; 16]), before: 71, limit: 50 },
        ClientFrame::Lookup { number: number() },
        ClientFrame::Send {
            chat_id: B64([1; 16]),
            msg_id: B64([2; 16]),
            body: Body(vec![3; 20]),
            new_chat: Some(NewChat { number: number(), my_key: blob(), their_key: blob(), signature: B64([4; 64]) }),
        },
        ClientFrame::Send { chat_id: B64([1; 16]), msg_id: B64([2; 16]), body: Body(vec![3; 20]), new_chat: None },
        ClientFrame::Received { chat_id: B64([1; 16]), through: 3 },
        ClientFrame::Read { chat_id: B64([1; 16]), through: 3 },
        ClientFrame::Accept { number: number() },
        ClientFrame::Reject { number: number() },
        ClientFrame::Block { number: number() },
        ClientFrame::Unblock { number: number() },
        ClientFrame::Remove { number: number(), clear_history: true },
        ClientFrame::SetDisplayName { display_name: "Abin P".into() },
        ClientFrame::RenameDevice { name: "work laptop".into() },
        ClientFrame::SetPin { pin: pin(Some(blob())) },
        ClientFrame::SetPin { pin: pin(None) },
    ];
    for frame in clients {
        let json = serde_json::to_string(&frame).unwrap();
        assert_eq!(serde_json::from_str::<ClientFrame>(&json).unwrap(), frame, "{json}");
    }
    let info = device_info();
    let servers = vec![
        ServerFrame::Ready { server_version: "0.1.0".into() },
        ServerFrame::SignupOffer { username: "abin-7k3q".into(), number: number(), password: resp(), recovery: resp() },
        ServerFrame::LoginOffer { response: B64([1; 320]) as CredResponse },
        ServerFrame::SignedIn { token: B64([1; 32]), account: account(Some(5)), copy: Some(blob()) },
        ServerFrame::SignedIn { token: B64([1; 32]), account: account(None), copy: None },
        ServerFrame::RecoveryOffer { recovery_copy: blob() },
        ServerFrame::Registration { password: None, recovery: Some(resp()) },
        ServerFrame::Reauthed,
        ServerFrame::PasswordChanged,
        ServerFrame::RecoveryCodeChanged,
        ServerFrame::Activated { account: account(None) },
        ServerFrame::Invited { invite: invite() },
        ServerFrame::Invites { invites: vec![invite()] },
        ServerFrame::Devices { devices: vec![info] },
        ServerFrame::SignedOut { reason: SignedOutReason::ByAnotherDevice },
        ServerFrame::Error { code: ErrorCode::Locked, message: "later".into(), until: Some(5) },
        ServerFrame::Error { code: ErrorCode::Internal, message: "oops".into(), until: None },
        ServerFrame::Snapshot {
            snapshot: Snapshot {
                cursor: 12,
                account: account(None),
                people: vec![person(Some("Sam".into()), Relation::Contact)],
                chats: vec![chat_info(None)],
                pins: vec![pin(Some(blob()))],
            },
        },
        ServerFrame::Events { events: all_events(), cursor: 20 },
        ServerFrame::Synced { cursor: 20 },
        ServerFrame::History { chat_id: B64([1; 16]), messages: vec![message()], complete: true },
        ServerFrame::Found { number: number(), public_keys: keys() },
        ServerFrame::Sent { chat_id: B64([1; 16]), msg_id: B64([2; 16]), seq: 3, sent_at: 10 },
        ServerFrame::NotSent {
            chat_id: B64([1; 16]),
            msg_id: B64([2; 16]),
            code: ErrorCode::ChatExists,
            message: "x".into(),
        },
        ServerFrame::Presence { number: number(), online: true },
    ];
    for frame in servers {
        let json = serde_json::to_string(&frame).unwrap();
        assert_eq!(serde_json::from_str::<ServerFrame>(&json).unwrap(), frame, "{json}");
    }
}

/// One event of every kind.
fn all_events() -> Vec<Event> {
    let marks = Marks { received_through: 2, read_through: 1 };
    let bodies = vec![
        EventBody::Message { message: message() },
        EventBody::Chat { chat: chat_info(Some(marks.clone())) },
        EventBody::Person { person: person(None, Relation::Asked) },
        EventBody::Marks { chat_id: B64([1; 16]), marks },
        EventBody::Read { chat_id: B64([1; 16]), read_through: 4 },
        EventBody::Cleared { chat_id: B64([1; 16]), cleared_through: 4 },
        EventBody::Profile { account: account(None) },
        EventBody::Devices { devices: vec![device_info()] },
        EventBody::Pin { pin: pin(None) },
    ];
    bodies.into_iter().enumerate().map(|(i, event)| Event { seq: i as i64 + 1, event }).collect()
}

#[test]
fn event_has_the_documented_wire_format() {
    let event = Event { seq: 7, event: EventBody::Read { chat_id: B64([0; 16]), read_through: 3 } };
    assert_eq!(
        serde_json::to_string(&event).unwrap(),
        r#"{"seq":7,"event":{"kind":"read","chat_id":"AAAAAAAAAAAAAAAAAAAAAA==","read_through":3}}"#
    );
}

#[test]
fn relation_is_snake_case() {
    assert_eq!(serde_json::to_string(&Relation::Asked).unwrap(), r#""asked""#);
}

#[test]
fn send_without_new_chat_may_leave_it_out() {
    let json =
        r#"{"type":"send","chat_id":"AAAAAAAAAAAAAAAAAAAAAA==","msg_id":"AAAAAAAAAAAAAAAAAAAAAA==","body":"AAE="}"#;
    let frame: ClientFrame = serde_json::from_str(json).unwrap();
    assert_eq!(
        frame,
        ClientFrame::Send { chat_id: B64([0; 16]), msg_id: B64([0; 16]), body: Body(vec![0, 1]), new_chat: None }
    );
}

#[test]
fn error_without_until_has_no_until_field() {
    let f = ServerFrame::Error { code: ErrorCode::BadFrame, message: "x".into(), until: None };
    assert!(!serde_json::to_string(&f).unwrap().contains("until"));
    let f = ServerFrame::Error { code: ErrorCode::Locked, message: "x".into(), until: Some(5) };
    assert!(serde_json::to_string(&f).unwrap().contains(r#""until":5"#));
}

#[test]
fn error_codes_are_snake_case() {
    assert_eq!(serde_json::to_string(&ErrorCode::LockedForever).unwrap(), r#""locked_forever""#);
}

#[test]
fn bytes_must_have_the_right_length() {
    use data_encoding::BASE64;
    let frame = |n: usize| {
        format!(
            r#"{{"type":"login_start","username":"a-b","request":"{}","device":{{"id":"{}","kind":"cli","name":"x"}}}}"#,
            BASE64.encode(&vec![0; n]),
            BASE64.encode(&[0; 16])
        )
    };
    assert!(serde_json::from_str::<ClientFrame>(&frame(96)).is_ok());
    assert!(serde_json::from_str::<ClientFrame>(&frame(95)).is_err());
}

#[test]
fn unknown_fields_are_ignored() {
    let f: ClientFrame = serde_json::from_str(r#"{"type":"activate","code":"X","extra":1}"#).unwrap();
    assert_eq!(f, ClientFrame::Activate { code: "X".into() });
}

#[test]
fn public_keys_verify_only_with_their_own_signature() {
    let sk = SigningKey::from_bytes(&[11; 32]);
    let exchange = [42u8; 32];
    let make = |sk: &SigningKey, exchange: [u8; 32]| PublicKeys {
        signing: B64(sk.verifying_key().to_bytes()),
        exchange: B64(exchange),
        signature: B64(sk.sign(&exchange_key_proof(&exchange)).to_bytes()),
    };
    let good = make(&sk, exchange);
    assert!(good.verify());

    let mut flipped = good.clone();
    flipped.exchange.0[0] ^= 1;
    assert!(!flipped.verify());

    let other = SigningKey::from_bytes(&[12; 32]);
    let mut foreign = good.clone();
    foreign.signature = make(&other, exchange).signature;
    assert!(!foreign.verify());

    let junk = PublicKeys { signing: B64([0xff; 32]), exchange: B64(exchange), signature: B64([0; 64]) };
    assert!(!junk.verify());
}
