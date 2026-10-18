mod common;

use common::{
    client::{Client, TestServer, assert_error, start_server},
    person::{Outcome, Person, copy, device, login, recover, reregister, resume, secret},
};
use corvo_chat_proto::{
    B64, Blob,
    frames::{ClientFrame, Device, ErrorCode, ServerFrame, SignedOutReason},
};
use tokio_tungstenite::tungstenite::Message;

/// What a finished recovery set.
struct Recovered {
    client: Client,
    password: String,
    code: String,
}

/// Recovers `person`'s account from `device` with new secrets.
async fn recover_fully(server: &TestServer, person: &Person, device: &Device) -> Recovered {
    let mut client = Client::open(server).await;
    match recover(&mut client, &person.username, &person.recovery, device).await {
        Outcome::Server(ServerFrame::RecoveryOffer { recovery_copy }) => {
            assert_eq!(recovery_copy.0, person.recovery_copy)
        }
        other => panic!("expected recovery_offer, got {other:?}"),
    }
    let (password, code) = (secret("pw"), secret("rc"));
    let (Some(pw), Some(rc)) = reregister(&mut client, Some(&password), Some(&code)).await else {
        panic!("expected both registrations");
    };
    client
        .send(&ClientFrame::RecoveryComplete {
            password: pw,
            recovery: rc,
            password_copy: Blob(copy()),
            recovery_copy: Blob(copy()),
        })
        .await;
    match client.recv().await {
        ServerFrame::SignedIn { account, copy, .. } => {
            assert_eq!(account.username, person.username);
            assert!(copy.is_none());
        }
        other => panic!("expected signed_in, got {other:?}"),
    }
    Recovered { client, password, code }
}

#[tokio::test]
async fn recovery_unlocks_a_locked_account() {
    let server = start_server().await;
    let (abin, _a) = Person::active(&server, "abin").await;
    abin.lock_for_good(&server).await;
    let recovered = recover_fully(&server, &abin, &device("new laptop")).await;
    assert_eq!(abin.wrong_count(&server).await, 0);

    let mut client = Client::open(&server).await;
    let outcome = login(&mut client, &abin.username, &recovered.password, &device("phone")).await;
    assert!(matches!(outcome, Outcome::Server(ServerFrame::SignedIn { .. })), "got {outcome:?}");
}

#[tokio::test]
async fn recovery_signs_out_every_other_device() {
    let server = start_server().await;
    let (abin, mut a) = Person::active(&server, "abin").await;
    let phone = device("phone");
    let (mut b, b_token) = abin.sign_in(&server, &phone).await;
    let mut recovered = recover_fully(&server, &abin, &device("new laptop")).await;

    for (client, token, id) in [(&mut a, &abin.token, &abin.device.id), (&mut b, &b_token, &phone.id)] {
        assert_eq!(client.recv().await, ServerFrame::SignedOut { reason: SignedOutReason::Recovered });
        assert!(client.closed().await);
        let (_, answer) = resume(&server, token, id).await;
        assert_error(answer, ErrorCode::SessionUnknown);
    }
    recovered.client.send(&ClientFrame::ListDevices).await;
    match recovered.client.recv().await {
        ServerFrame::Devices { devices } => assert_eq!(devices.len(), 1),
        other => panic!("expected devices, got {other:?}"),
    }
}

#[tokio::test]
async fn recovery_gives_a_new_code_and_kills_the_old_one() {
    let server = start_server().await;
    let (abin, _a) = Person::active(&server, "abin").await;
    let recovered = recover_fully(&server, &abin, &device("new laptop")).await;
    let mut client = Client::open(&server).await;
    let outcome = recover(&mut client, &abin.username, &abin.recovery, &device("x")).await;
    assert!(matches!(outcome, Outcome::ClientRefused), "the old code still works: {outcome:?}");
    let outcome = recover(&mut client, &abin.username, &recovered.code, &device("x")).await;
    assert!(matches!(outcome, Outcome::Server(ServerFrame::RecoveryOffer { .. })), "got {outcome:?}");
}

#[tokio::test]
async fn recovery_works_at_the_device_and_daily_limits() {
    let server = start_server().await;
    let (abin, _a) = Person::active(&server, "abin").await;
    let phone = device("phone");
    abin.sign_in(&server, &device("tablet")).await;
    for _ in 0..4 {
        abin.sign_in(&server, &phone).await;
    }
    let mut client = Client::open(&server).await;
    match login(&mut client, &abin.username, &abin.password, &device("desktop")).await {
        Outcome::Server(ServerFrame::Error { code: ErrorCode::SignInLimit, .. }) => {}
        other => panic!("expected sign_in_limit, got {other:?}"),
    }
    recover_fully(&server, &abin, &device("desktop")).await;
}

#[tokio::test]
async fn sixth_recovery_try_in_an_hour_is_rate_limited() {
    let server = start_server().await;
    let (abin, _a) = Person::active(&server, "abin").await;
    let mut client = Client::open(&server).await;
    let thief = device("thief");
    for _ in 0..5 {
        assert!(matches!(recover(&mut client, &abin.username, "wrong", &thief).await, Outcome::ClientRefused));
    }
    match recover(&mut client, &abin.username, &abin.recovery, &thief).await {
        Outcome::Server(frame) => assert_error(frame, ErrorCode::RateLimited),
        other => panic!("expected rate_limited, got {other:?}"),
    }
    // Recovery tries don't touch the password count.
    assert_eq!(abin.wrong_count(&server).await, 0);
    server.state.clock.advance(3_600);
    let outcome = recover(&mut client, &abin.username, &abin.recovery, &thief).await;
    assert!(matches!(outcome, Outcome::Server(ServerFrame::RecoveryOffer { .. })), "got {outcome:?}");
}

#[tokio::test]
async fn abandoned_recovery_changes_nothing() {
    let server = start_server().await;
    let (abin, mut a) = Person::active(&server, "abin").await;
    let mut client = Client::open(&server).await;
    let outcome = recover(&mut client, &abin.username, &abin.recovery, &device("x")).await;
    assert!(matches!(outcome, Outcome::Server(ServerFrame::RecoveryOffer { .. })), "got {outcome:?}");
    client.send_raw(Message::Close(None)).await;
    assert!(client.closed().await);

    a.send(&ClientFrame::ListDevices).await;
    assert!(matches!(a.recv().await, ServerFrame::Devices { .. }));
    abin.sign_in(&server, &device("phone")).await;
}

#[tokio::test]
async fn recovering_connection_can_only_reregister_and_complete() {
    let server = start_server().await;
    let (abin, _a) = Person::active(&server, "abin").await;
    let mut client = Client::open(&server).await;
    recover(&mut client, &abin.username, &abin.recovery, &device("x")).await;
    client.send(&ClientFrame::ListDevices).await;
    assert_error(client.recv().await, ErrorCode::NotSignedIn);
    let upload = B64(rand::random::<[u8; 192]>());
    client.send(&ClientFrame::ChangePassword { password: upload, password_copy: Blob(copy()) }).await;
    assert_error(client.recv().await, ErrorCode::NotSignedIn);
}

#[tokio::test]
async fn unknown_name_recovery_gets_an_offer_that_never_finishes() {
    let server = start_server().await;
    let mut client = Client::open(&server).await;
    let (_, request) = common::client::login_start("whatever");
    let device = device("x");
    client.send(&ClientFrame::RecoveryStart { username: "nobody-0000".into(), request, device }).await;
    assert!(matches!(client.recv().await, ServerFrame::LoginOffer { .. }));
    client.send(&ClientFrame::RecoveryFinish { finalization: B64(rand::random()) }).await;
    assert_error(client.recv().await, ErrorCode::WrongCredentials);
}
