mod common;

use common::{
    client::{Client, TestServer, start_server, start_server_with},
    person::{Person, begin_signup, device},
};
use corvo_chat_proto::{
    Number,
    frames::{Account, ClientFrame, ErrorCode, InviteInfo, ServerFrame, SignedOutReason},
    names::is_username,
};
use corvo_chat_server::{accounts, limits::Limits};

fn assert_error(frame: ServerFrame, code: ErrorCode) {
    match frame {
        ServerFrame::Error { code: got, .. } => assert_eq!(got, code),
        other => panic!("expected {code:?}, got {other:?}"),
    }
}

/// Invites `number` from `client` and returns the invite.
async fn invite(client: &mut Client, number: Number) -> InviteInfo {
    client.send(&ClientFrame::Invite { number }).await;
    match client.recv().await {
        ServerFrame::Invited { invite } => invite,
        other => panic!("expected invited, got {other:?}"),
    }
}

/// A code that is not `code`.
fn other_code(code: &str) -> String {
    if code == "000000" { "111111".into() } else { "000000".into() }
}

const ACCOUNT_ROWS: [&str; 3] = [
    "SELECT COUNT(*) FROM accounts WHERE id = ?1",
    "SELECT COUNT(*) FROM sessions WHERE account_id = ?1",
    "SELECT COUNT(*) FROM invites WHERE account_id = ?1",
];

async fn count(server: &TestServer, sql: &'static str, arg: impl rusqlite::ToSql + Send + 'static) -> i64 {
    server.state.db.call(move |c| c.query_row(sql, [arg], |r| r.get(0)).unwrap()).await
}

#[tokio::test]
async fn signup_makes_a_pending_account_with_an_ending() {
    let server = start_server().await;
    let mut client = Client::connect(&server).await;
    client.hello().await;
    let begun = begin_signup(&mut client, "abin", "Abin", device("laptop")).await.expect("an offer");
    assert!(begun.username.starts_with("abin-"));
    assert!(is_username(&begun.username));
    assert!(Number::parse(&begun.number.digits()).is_ok());
    client.send(&begun.finish).await;
    match client.recv().await {
        ServerFrame::SignedIn { account, copy, .. } => {
            assert_eq!(account.username, begun.username);
            assert_eq!(account.number, begun.number);
            assert_eq!(account.display_name, "Abin");
            assert_eq!(account.pending_until, Some(server.state.clock.now() + 86_400));
            assert!(copy.is_none());
        }
        other => panic!("expected signed_in, got {other:?}"),
    }
}

#[tokio::test]
async fn same_base_twice_gets_two_usernames() {
    let server = start_server_with(Limits { signups_per_ip_day: 1_000, ..Limits::default() }).await;
    let (a, _a) = Person::sign_up(&server, "abin").await;
    let (b, _b) = Person::sign_up(&server, "abin").await;
    assert_ne!(a.username, b.username);
    assert_ne!(a.number, b.number);
}

#[tokio::test]
async fn bad_names_are_refused() {
    let server = start_server().await;
    let mut client = Client::connect(&server).await;
    client.hello().await;
    let err = begin_signup(&mut client, "1x", "Abin", device("laptop")).await.expect_err("refused");
    assert_error(*err, ErrorCode::BadUsername);
    let err = begin_signup(&mut client, "abin", "", device("laptop")).await.expect_err("refused");
    assert_error(*err, ErrorCode::BadDisplayName);
    let err = begin_signup(&mut client, "abin", "Abin", device("")).await.expect_err("refused");
    assert_error(*err, ErrorCode::BadDevice);
}

#[tokio::test]
async fn public_keys_must_verify() {
    let server = start_server().await;
    let mut client = Client::connect(&server).await;
    client.hello().await;
    let mut begun = begin_signup(&mut client, "abin", "Abin", device("laptop")).await.expect("an offer");
    if let ClientFrame::SignupFinish { public_keys, .. } = &mut begun.finish {
        public_keys.signature.0[0] ^= 1;
    }
    client.send(&begun.finish).await;
    assert_error(client.recv().await, ErrorCode::BadPublicKeys);
    assert_eq!(count(&server, "SELECT COUNT(*) FROM accounts WHERE username = ?1", begun.username).await, 0);
}

#[tokio::test]
async fn pending_account_can_only_activate_or_sign_out() {
    let server = start_server().await;
    let (_person, mut client) = Person::sign_up(&server, "abin").await;
    client.send(&ClientFrame::ListDevices).await;
    assert_error(client.recv().await, ErrorCode::NotActive);
    client.send(&ClientFrame::Invite { number: Number::from_body(123_456_789).unwrap() }).await;
    assert_error(client.recv().await, ErrorCode::NotActive);
}

#[tokio::test]
async fn invite_then_activate() {
    let server = start_server().await;
    let (inviter, mut inviter_client) = Person::active(&server, "ines").await;
    let (pending, mut client) = Person::sign_up(&server, "abin").await;
    let invite = invite(&mut inviter_client, pending.number).await;
    assert_eq!(invite.number, pending.number);
    assert_eq!(invite.display_name, "Abin");
    assert_eq!(invite.code.len(), 6);
    assert!(invite.code.bytes().all(|b| b.is_ascii_digit()));
    client.send(&ClientFrame::Activate { code: invite.code }).await;
    match client.recv().await {
        ServerFrame::Activated { account: Account { pending_until, username, .. } } => {
            assert_eq!(pending_until, None);
            assert_eq!(username, pending.username);
        }
        other => panic!("expected activated, got {other:?}"),
    }
    let by: Option<i64> = server
        .state
        .db
        .call(move |c| {
            c.query_row("SELECT invited_by FROM accounts WHERE username = ?1", [pending.username], |r| r.get(0))
                .unwrap()
        })
        .await;
    assert_eq!(by, Some(inviter.id(&server).await));
    // Now active: it can use active frames, and a second activate is out of place.
    client.send(&ClientFrame::ListInvites).await;
    assert!(matches!(client.recv().await, ServerFrame::Invites { .. }));
}

#[tokio::test]
async fn wrong_code_five_times_kills_it() {
    let server = start_server().await;
    let (_inviter, mut inviter_client) = Person::active(&server, "ines").await;
    let (pending, mut client) = Person::sign_up(&server, "abin").await;
    let first = invite(&mut inviter_client, pending.number).await;
    let wrong = other_code(&first.code);
    for _ in 0..4 {
        client.send(&ClientFrame::Activate { code: wrong.clone() }).await;
        assert_error(client.recv().await, ErrorCode::CodeWrong);
    }
    client.send(&ClientFrame::Activate { code: wrong }).await;
    assert_error(client.recv().await, ErrorCode::CodeDead);
    client.send(&ClientFrame::Activate { code: first.code }).await;
    assert_error(client.recv().await, ErrorCode::CodeDead);
    let fresh = invite(&mut inviter_client, pending.number).await;
    client.send(&ClientFrame::Activate { code: fresh.code }).await;
    assert!(matches!(client.recv().await, ServerFrame::Activated { .. }));
}

#[tokio::test]
async fn inviting_an_active_or_unknown_number_is_refused() {
    let server = start_server().await;
    let (_inviter, mut client) = Person::active(&server, "ines").await;
    let (other, _other_client) = Person::active(&server, "olga").await;
    client.send(&ClientFrame::Invite { number: other.number }).await;
    assert_error(client.recv().await, ErrorCode::NotPending);
    client.send(&ClientFrame::Invite { number: Number::from_body(123_456_789).unwrap() }).await;
    assert_error(client.recv().await, ErrorCode::NotPending);
}

#[tokio::test]
async fn sixth_open_invite_is_refused() {
    let server = start_server().await;
    let (_inviter, mut client) = Person::active(&server, "ines").await;
    let mut waiting = Vec::new();
    for _ in 0..6 {
        waiting.push(Person::sign_up(&server, "abin").await);
    }
    for (person, _) in &waiting[..5] {
        invite(&mut client, person.number).await;
    }
    client.send(&ClientFrame::Invite { number: waiting[5].0.number }).await;
    assert_error(client.recv().await, ErrorCode::TooManyInvites);
}

#[tokio::test]
async fn pending_account_expires_after_a_day() {
    let server = start_server().await;
    let (_inviter, mut inviter_client) = Person::active(&server, "ines").await;
    let (pending, mut client) = Person::sign_up(&server, "abin").await;
    let invite = invite(&mut inviter_client, pending.number).await;
    let id = pending.id(&server).await;
    for sql in ACCOUNT_ROWS {
        assert_eq!(count(&server, sql, id).await, 1, "{sql}");
    }
    server.state.clock.advance(86_400);
    client.send(&ClientFrame::Activate { code: invite.code }).await;
    assert_error(client.recv().await, ErrorCode::SignupExpired);
    assert_eq!(client.recv().await, ServerFrame::SignedOut { reason: SignedOutReason::Expired });
    assert!(client.closed().await);
    for sql in ACCOUNT_ROWS {
        assert_eq!(count(&server, sql, id).await, 0, "{sql}");
    }
    let (name, number) = (pending.username.clone(), pending.number);
    let now = server.state.clock.now();
    server
        .state
        .db
        .call(move |c| {
            assert!(accounts::by_username(c, &name, now).unwrap().is_none());
            assert!(accounts::by_number(c, number, now).unwrap().is_none());
        })
        .await;
}

#[tokio::test]
async fn second_signup_from_one_device_in_a_day_is_refused() {
    let server = start_server().await;
    let device = device("laptop");
    let first = Person::try_sign_up(&server, "abin", device.clone()).await;
    assert!(first.is_ok());
    match Person::try_sign_up(&server, "abin", device.clone()).await {
        Err(frame) => assert_error(*frame, ErrorCode::SignupLimit),
        Ok(_) => panic!("a second signup from one device"),
    }
    server.state.clock.advance(86_400);
    assert!(Person::try_sign_up(&server, "abin", device).await.is_ok());
}

#[tokio::test]
async fn fourth_signup_from_one_ip_in_a_day_is_refused() {
    let server = start_server_with(Limits { signups_per_ip_day: 3, ..Limits::default() }).await;
    let mut kept = Vec::new();
    for _ in 0..3 {
        kept.push(Person::try_sign_up(&server, "abin", device("laptop")).await.expect("within the limit"));
    }
    match Person::try_sign_up(&server, "abin", device("laptop")).await {
        Err(frame) => {
            let ServerFrame::Error { code: ErrorCode::SignupLimit, message, .. } = *frame else { panic!("{frame:?}") };
            assert_eq!(message, "Too many new accounts from this network today.");
        }
        other => panic!("expected signup_limit, got {:?}", other.map(|_| ())),
    }
}

#[tokio::test]
async fn sign_out_closes_and_deletes_the_session() {
    let server = start_server().await;
    let (person, mut client) = Person::sign_up(&server, "abin").await;
    let id = person.id(&server).await;
    assert_eq!(count(&server, "SELECT COUNT(*) FROM sessions WHERE account_id = ?1", id).await, 1);
    client.send(&ClientFrame::SignOut).await;
    assert_eq!(client.recv().await, ServerFrame::SignedOut { reason: SignedOutReason::ThisDevice });
    assert!(client.closed().await);
    assert_eq!(count(&server, "SELECT COUNT(*) FROM sessions WHERE account_id = ?1", id).await, 0);
}

#[tokio::test]
async fn parallel_signups_from_one_device_cannot_beat_the_limit() {
    let server = start_server().await;
    let device = device("laptop");
    let mut first = Client::connect(&server).await;
    let mut second = Client::connect(&server).await;
    first.hello().await;
    second.hello().await;
    let a = begin_signup(&mut first, "abin", "Abin", device.clone()).await.expect("an offer");
    let b = begin_signup(&mut second, "abin", "Abin", device).await.expect("an offer");
    first.send(&a.finish).await;
    assert!(matches!(first.recv().await, ServerFrame::SignedIn { .. }));
    second.send(&b.finish).await;
    assert_error(second.recv().await, ErrorCode::SignupLimit);
    assert_eq!(count(&server, "SELECT COUNT(*) FROM accounts WHERE username = ?1", b.username).await, 0);
}
