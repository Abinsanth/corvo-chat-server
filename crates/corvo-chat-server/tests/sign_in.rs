mod common;

use common::{
    client::{Client, TestServer, assert_error, start_server},
    person::{Outcome, Person, begin_signup_with, device, login, prove_with, resume},
};
use corvo_chat_proto::{
    B64,
    frames::{ClientFrame, Device, ErrorCode, ServerFrame},
    opaque::password_ksf,
};

/// Sends `login_start` and returns the answer, without finishing.
async fn start_only(client: &mut Client, username: &str, device: &Device) -> ServerFrame {
    let (_, request) = common::client::login_start("whatever");
    client.send(&ClientFrame::LoginStart { username: username.into(), request, device: device.clone() }).await;
    client.recv().await
}

/// Signs in on a new connection; the server's answer.
async fn sign_in(server: &TestServer, person: &Person, device: &Device) -> ServerFrame {
    let mut client = Client::open(server).await;
    match login(&mut client, &person.username, &person.password, device).await {
        Outcome::Server(frame) => frame,
        Outcome::ClientRefused => panic!("the right password failed on the client"),
    }
}

fn assert_signed_in(frame: ServerFrame) {
    assert!(matches!(frame, ServerFrame::SignedIn { .. }), "expected signed_in, got {frame:?}");
}

fn assert_offer(frame: ServerFrame) {
    assert!(matches!(frame, ServerFrame::LoginOffer { .. }), "expected login_offer, got {frame:?}");
}

/// The error code and `until` of an error frame.
fn error_until(frame: ServerFrame) -> (ErrorCode, Option<i64>) {
    match frame {
        ServerFrame::Error { code, until, .. } => (code, until),
        other => panic!("expected an error, got {other:?}"),
    }
}

#[tokio::test]
async fn sign_in_returns_the_password_copy() {
    let server = start_server().await;
    let (abin, _a) = Person::active(&server, "abin").await;
    match sign_in(&server, &abin, &device("phone")).await {
        ServerFrame::SignedIn { account, copy, .. } => {
            assert_eq!(copy.map(|c| c.0), Some(abin.password_copy.clone()));
            assert_eq!(account.username, abin.username);
            assert_eq!(account.number, abin.number);
            assert_eq!(account.pending_until, None);
        }
        other => panic!("expected signed_in, got {other:?}"),
    }
}

#[tokio::test]
async fn wrong_password_gets_no_finalization() {
    let server = start_server().await;
    let (abin, _a) = Person::active(&server, "abin").await;
    let mut client = Client::open(&server).await;
    let outcome = login(&mut client, &abin.username, "not the password", &device("phone")).await;
    assert!(matches!(outcome, Outcome::ClientRefused), "got {outcome:?}");
    assert_eq!(abin.wrong_count(&server).await, 1);
}

#[tokio::test]
async fn abandoned_login_counts_as_a_wrong_password() {
    let server = start_server().await;
    let (abin, _a) = Person::active(&server, "abin").await;
    let mut client = Client::open(&server).await;
    let thief = device("thief");
    for _ in 0..5 {
        assert_offer(start_only(&mut client, &abin.username, &thief).await);
    }
    let now = server.state.clock.now();
    assert_eq!(
        error_until(start_only(&mut client, &abin.username, &thief).await),
        (ErrorCode::Locked, Some(now + 900))
    );
}

#[tokio::test]
async fn fifteen_minutes_then_five_more_locks_for_good() {
    let server = start_server().await;
    let (abin, _a) = Person::active(&server, "abin").await;
    let mut client = Client::open(&server).await;
    let thief = device("thief");
    for _ in 0..5 {
        assert!(matches!(login(&mut client, &abin.username, "wrong", &thief).await, Outcome::ClientRefused));
    }
    let (code, until) = error_until(start_only(&mut client, &abin.username, &thief).await);
    assert_eq!(code, ErrorCode::Locked);
    // One second before the end it's still locked; at the end it opens.
    server.state.clock.advance(899);
    assert_eq!(error_until(start_only(&mut client, &abin.username, &thief).await), (ErrorCode::Locked, until));
    server.state.clock.advance(1);
    for _ in 0..5 {
        assert!(matches!(login(&mut client, &abin.username, "wrong", &thief).await, Outcome::ClientRefused));
    }
    assert_error(start_only(&mut client, &abin.username, &thief).await, ErrorCode::LockedForever);
    // The right password doesn't open it either.
    assert_error(sign_in(&server, &abin, &thief).await, ErrorCode::LockedForever);
    server.state.clock.advance(10 * 365 * 86_400);
    assert_error(start_only(&mut client, &abin.username, &thief).await, ErrorCode::LockedForever);
}

#[tokio::test]
async fn right_password_clears_the_count() {
    let server = start_server().await;
    let (abin, _a) = Person::active(&server, "abin").await;
    let mut client = Client::open(&server).await;
    let phone = device("phone");
    for _ in 0..4 {
        assert!(matches!(login(&mut client, &abin.username, "wrong", &phone).await, Outcome::ClientRefused));
    }
    assert_signed_in(sign_in(&server, &abin, &phone).await);
    assert_eq!(abin.wrong_count(&server).await, 0);
    for _ in 0..4 {
        assert!(matches!(login(&mut client, &abin.username, "wrong", &phone).await, Outcome::ClientRefused));
    }
    assert_eq!(abin.wrong_count(&server).await, 4);
    assert_offer(start_only(&mut client, &abin.username, &phone).await);
}

#[tokio::test]
async fn locks_do_not_touch_signed_in_devices() {
    let server = start_server().await;
    let (abin, mut a) = Person::active(&server, "abin").await;
    abin.lock_for_good(&server).await;
    let mut client = Client::open(&server).await;
    assert_error(start_only(&mut client, &abin.username, &device("thief")).await, ErrorCode::LockedForever);

    a.send(&ClientFrame::ListInvites).await;
    assert!(matches!(a.recv().await, ServerFrame::Invites { .. }));
    let (_, answer) = resume(&server, &abin.token, &abin.device.id).await;
    assert_signed_in(answer);
}

#[tokio::test]
async fn unknown_username_gets_an_offer_and_locks_nothing() {
    let server = start_server().await;
    let (abin, _a) = Person::active(&server, "abin").await;
    let ending = &abin.username[abin.username.len() - 4..];
    let other = format!("abin-{}", if ending == "0000" { "0001" } else { "0000" });
    let mut client = Client::open(&server).await;
    for name in ["nobody-0000", other.as_str(), "Not A Name!"] {
        // The response has a fixed size, so a fake one parses like a real one.
        assert_offer(start_only(&mut client, name, &device("phone")).await);
        client.send(&ClientFrame::LoginFinish { finalization: B64(rand::random()) }).await;
        assert_error(client.recv().await, ErrorCode::WrongCredentials);
    }
    assert_eq!(abin.wrong_count(&server).await, 0);
    // A made-up finalization for a real account fails the same way.
    assert_offer(start_only(&mut client, &abin.username, &device("phone")).await);
    client.send(&ClientFrame::LoginFinish { finalization: B64(rand::random()) }).await;
    assert_error(client.recv().await, ErrorCode::WrongCredentials);
    assert_eq!(abin.wrong_count(&server).await, 1);
}

#[tokio::test]
async fn twenty_unknown_names_from_one_ip_rate_limit_it() {
    let server = start_server().await;
    let (abin, _a) = Person::active(&server, "abin").await;
    let mut client = Client::open(&server).await;
    let phone = device("phone");
    for n in 0..20 {
        assert_offer(start_only(&mut client, &format!("nobody-{n:04}"), &phone).await);
    }
    // Every name is refused now, real or not, so the refusal reveals nothing.
    assert_error(start_only(&mut client, &abin.username, &phone).await, ErrorCode::RateLimited);
    assert_error(start_only(&mut client, "nobody-9999", &phone).await, ErrorCode::RateLimited);
    server.state.clock.advance(3_600);
    assert_offer(start_only(&mut client, &abin.username, &phone).await);
}

#[tokio::test]
async fn sixth_sign_in_in_a_day_is_refused() {
    let server = start_server().await;
    let (abin, _a) = Person::active(&server, "abin").await;
    let phone = device("phone");
    for _ in 0..5 {
        assert_signed_in(sign_in(&server, &abin, &phone).await);
    }
    match sign_in(&server, &abin, &phone).await {
        ServerFrame::Error { code, message, .. } => {
            assert_eq!(code, ErrorCode::SignInLimit);
            assert_eq!(message, "Too many sign-ins today, try again tomorrow.");
        }
        other => panic!("expected sign_in_limit, got {other:?}"),
    }
    // The password was right, so it didn't count as a wrong one.
    assert_eq!(abin.wrong_count(&server).await, 0);
    server.state.clock.advance(86_400);
    assert_signed_in(sign_in(&server, &abin, &phone).await);
}

#[tokio::test]
async fn fourth_device_is_refused_with_the_spec_message() {
    let server = start_server().await;
    let (abin, _a) = Person::active(&server, "abin").await;
    assert_signed_in(sign_in(&server, &abin, &device("phone")).await);
    assert_signed_in(sign_in(&server, &abin, &device("tablet")).await);
    match sign_in(&server, &abin, &device("desktop")).await {
        ServerFrame::Error { code, message, .. } => {
            assert_eq!(code, ErrorCode::DeviceLimit);
            assert_eq!(message, "Maximum devices reached (3). Sign out a device first.");
        }
        other => panic!("expected device_limit, got {other:?}"),
    }
}

#[tokio::test]
async fn pending_account_can_sign_in_but_only_activate() {
    let server = start_server().await;
    let (abin, _a) = Person::sign_up(&server, "abin").await;
    let mut client = Client::open(&server).await;
    match login(&mut client, &abin.username, &abin.password, &device("phone")).await {
        Outcome::Server(ServerFrame::SignedIn { account, .. }) => assert!(account.pending_until.is_some()),
        other => panic!("expected signed_in, got {other:?}"),
    }
    client.send(&ClientFrame::ListDevices).await;
    assert_error(client.recv().await, ErrorCode::NotActive);
    // No invite yet, so the code is dead, but activating is allowed.
    client.send(&ClientFrame::Activate { code: "123456".into() }).await;
    assert_error(client.recv().await, ErrorCode::CodeDead);
}

#[tokio::test]
async fn resume_is_not_a_sign_in() {
    let server = start_server().await;
    let (abin, _a) = Person::active(&server, "abin").await;
    for _ in 0..10 {
        abin.client_for(&server).await;
    }
    let phone = device("phone");
    for _ in 0..5 {
        assert_signed_in(sign_in(&server, &abin, &phone).await);
    }
}

#[tokio::test]
async fn resume_needs_the_right_token_and_device() {
    let server = start_server().await;
    let (abin, _a) = Person::active(&server, "abin").await;
    let (_, answer) = resume(&server, &abin.token, &device("other").id).await;
    assert_error(answer, ErrorCode::SessionUnknown);
    let (_, answer) = resume(&server, &B64(rand::random()), &abin.device.id).await;
    assert_error(answer, ErrorCode::SessionUnknown);
    match resume(&server, &abin.token, &abin.device.id).await {
        (_, ServerFrame::SignedIn { token, account, copy }) => {
            assert_eq!(token, abin.token);
            assert_eq!(account.username, abin.username);
            assert!(copy.is_none());
        }
        (_, other) => panic!("expected signed_in, got {other:?}"),
    }
}

#[tokio::test]
async fn resuming_again_closes_the_older_connection() {
    let server = start_server().await;
    let (abin, mut a) = Person::active(&server, "abin").await;
    let mut b = abin.client_for(&server).await;
    assert!(a.closed_quietly().await);
    b.send(&ClientFrame::ListInvites).await;
    assert!(matches!(b.recv().await, ServerFrame::Invites { .. }));
}

#[tokio::test]
async fn real_password_ksf_sign_up_and_sign_in() {
    let server = start_server().await;
    let mut client = Client::open(&server).await;
    let begun = begin_signup_with(&password_ksf(), &mut client, "abin", "Abin", device("laptop")).await.unwrap();
    client.send(&begun.finish).await;
    assert_signed_in(client.recv().await);

    let mut other = Client::open(&server).await;
    let phone = device("phone");
    let start = |request| ClientFrame::LoginStart { username: begun.username.clone(), request, device: phone.clone() };
    let finish = |finalization| ClientFrame::LoginFinish { finalization };
    match prove_with(&password_ksf(), &mut other, &begun.password, start, finish).await {
        Outcome::Server(frame) => assert_signed_in(frame),
        Outcome::ClientRefused => panic!("the right password failed on the client"),
    }
}
