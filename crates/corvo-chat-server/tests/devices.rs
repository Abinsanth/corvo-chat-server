mod common;

use common::{
    client::{Client, assert_error, start_server},
    person::{Outcome, Person, copy, device, login, reauth, recover, reregister, resume, secret},
};
use corvo_chat_proto::{
    B64, Blob,
    frames::{ClientFrame, Device, DeviceInfo, DeviceKind, ErrorCode, ServerFrame, SignedOutReason},
};
use corvo_chat_server::cleanup;
use tokio_tungstenite::tungstenite::Message;

async fn devices(client: &mut Client) -> Vec<DeviceInfo> {
    client.send(&ClientFrame::ListDevices).await;
    match client.recv().await {
        ServerFrame::Devices { devices } => devices,
        other => panic!("expected devices, got {other:?}"),
    }
}

async fn sign_out_device(client: &mut Client, id: &B64<16>) -> ServerFrame {
    client.send(&ClientFrame::SignOutDevice { id: id.clone() }).await;
    client.recv().await
}

/// Proves the password again and expects `reauthed`.
async fn reauthed(client: &mut Client, password: &str) {
    match reauth(client, password).await {
        Outcome::Server(ServerFrame::Reauthed) => {}
        other => panic!("expected reauthed, got {other:?}"),
    }
}

fn signed_out(frame: ServerFrame) -> SignedOutReason {
    match frame {
        ServerFrame::SignedOut { reason } => reason,
        other => panic!("expected signed_out, got {other:?}"),
    }
}

#[tokio::test]
async fn devices_list_marks_this_device() {
    let server = start_server().await;
    let (abin, mut a) = Person::active(&server, "abin").await;
    let web = Device { id: B64(rand::random()), kind: DeviceKind::Web, name: "Firefox".into() };
    let phone = Device { id: B64(rand::random()), kind: DeviceKind::Phone, name: "Pixel".into() };
    abin.sign_in(&server, &web).await;
    abin.sign_in(&server, &phone).await;

    let list = devices(&mut a).await;
    let seen: Vec<_> = list.iter().map(|d| (d.id.clone(), d.kind, d.name.as_str(), d.this_device)).collect();
    assert_eq!(
        seen,
        vec![
            (abin.device.id.clone(), DeviceKind::Cli, "laptop", true),
            (web.id.clone(), DeviceKind::Web, "Firefox", false),
            (phone.id.clone(), DeviceKind::Phone, "Pixel", false),
        ]
    );
    let now = server.state.clock.now();
    assert!(list.iter().all(|d| d.first_seen == now && d.last_used == now));
}

#[tokio::test]
async fn signed_out_device_is_told_and_its_token_dies() {
    let server = start_server().await;
    let (abin, mut a) = Person::active(&server, "abin").await;
    let phone = device("phone");
    let tablet = device("tablet");
    let (mut b, b_token) = abin.sign_in(&server, &phone).await;
    let (_c, _) = abin.sign_in(&server, &tablet).await;

    match sign_out_device(&mut a, &phone.id).await {
        ServerFrame::Devices { devices } => {
            assert_eq!(devices.iter().map(|d| d.id.clone()).collect::<Vec<_>>(), vec![abin.device.id, tablet.id]);
        }
        other => panic!("expected devices, got {other:?}"),
    }
    assert_eq!(signed_out(b.recv().await), SignedOutReason::ByAnotherDevice);
    assert!(b.closed().await);
    let (_, answer) = resume(&server, &b_token, &phone.id).await;
    assert_error(answer, ErrorCode::SessionUnknown);
}

#[tokio::test]
async fn signing_out_this_device_or_an_unknown_one_by_id() {
    let server = start_server().await;
    let (abin, mut a) = Person::active(&server, "abin").await;
    assert_error(sign_out_device(&mut a, &abin.device.id).await, ErrorCode::BadFrame);
    match sign_out_device(&mut a, &B64(rand::random())).await {
        ServerFrame::Devices { devices } => assert_eq!(devices.len(), 1),
        other => panic!("expected devices, got {other:?}"),
    }
}

#[tokio::test]
async fn same_device_signing_in_again_keeps_one_slot() {
    let server = start_server().await;
    let (abin, mut a) = Person::active(&server, "abin").await;
    abin.sign_in(&server, &device("phone")).await;
    abin.sign_in(&server, &device("tablet")).await;
    let (mut again, _) = abin.sign_in(&server, &abin.device).await;
    assert!(a.closed_quietly().await, "the old connection is closed with no signed_out");
    assert_eq!(devices(&mut again).await.len(), 3);
}

#[tokio::test]
async fn change_password_needs_reauth_and_signs_out_the_others() {
    let server = start_server().await;
    let (abin, mut a) = Person::active(&server, "abin").await;
    let (mut b, _) = abin.sign_in(&server, &device("phone")).await;

    let random_upload = B64(rand::random::<[u8; 192]>());
    a.send(&ClientFrame::ChangePassword { password: random_upload, password_copy: Blob(copy()) }).await;
    assert_error(a.recv().await, ErrorCode::NotAllowed);
    a.send(&ClientFrame::Reregister { password: None, recovery: None }).await;
    assert_error(a.recv().await, ErrorCode::NotAllowed);

    reauthed(&mut a, &abin.password).await;
    let new_password = secret("pw");
    let new_copy = copy();
    let (Some(upload), None) = reregister(&mut a, Some(&new_password), None).await else {
        panic!("expected only a password registration");
    };
    a.send(&ClientFrame::ChangePassword { password: upload, password_copy: Blob(new_copy.clone()) }).await;
    assert_eq!(a.recv().await, ServerFrame::PasswordChanged);

    assert_eq!(signed_out(b.recv().await), SignedOutReason::PasswordChanged);
    assert!(b.closed().await);
    assert_eq!(devices(&mut a).await.len(), 1);

    let mut client = Client::open(&server).await;
    let outcome = login(&mut client, &abin.username, &abin.password, &device("x")).await;
    assert!(matches!(outcome, Outcome::ClientRefused), "the old password still works: {outcome:?}");
    match login(&mut client, &abin.username, &new_password, &device("x")).await {
        Outcome::Server(ServerFrame::SignedIn { copy, .. }) => assert_eq!(copy.map(|c| c.0), Some(new_copy)),
        other => panic!("expected signed_in, got {other:?}"),
    }
}

#[tokio::test]
async fn reauth_with_a_wrong_password_counts_toward_the_lock() {
    let server = start_server().await;
    let (abin, mut a) = Person::active(&server, "abin").await;
    for _ in 0..5 {
        assert!(matches!(reauth(&mut a, "wrong").await, Outcome::ClientRefused));
    }
    assert_eq!(abin.wrong_count(&server).await, 5);
    match reauth(&mut a, &abin.password).await {
        Outcome::Server(ServerFrame::Error { code: ErrorCode::Locked, .. }) => {}
        other => panic!("expected locked, got {other:?}"),
    }
    // A made-up finalization gets the one wrong answer, and the device stays in.
    server.state.clock.advance(900);
    let (_, request) = common::client::login_start("whatever");
    a.send(&ClientFrame::ReauthStart { request }).await;
    assert!(matches!(a.recv().await, ServerFrame::LoginOffer { .. }));
    a.send(&ClientFrame::ReauthFinish { finalization: B64(rand::random()) }).await;
    assert_error(a.recv().await, ErrorCode::WrongCredentials);
    assert_eq!(devices(&mut a).await.len(), 1);
}

#[tokio::test]
async fn reauth_finish_without_a_start_is_out_of_order() {
    let server = start_server().await;
    let (_abin, mut a) = Person::active(&server, "abin").await;
    a.send(&ClientFrame::ReauthFinish { finalization: B64(rand::random()) }).await;
    assert_error(a.recv().await, ErrorCode::BadFrame);
}

#[tokio::test]
async fn reauth_expires_after_five_minutes() {
    let server = start_server().await;
    let (abin, mut a) = Person::active(&server, "abin").await;
    reauthed(&mut a, &abin.password).await;
    server.state.clock.advance(299);
    reregister(&mut a, Some("still fine"), None).await;
    server.state.clock.advance(2);
    a.send(&ClientFrame::Reregister { password: None, recovery: None }).await;
    assert_error(a.recv().await, ErrorCode::NotAllowed);
}

#[tokio::test]
async fn new_recovery_code_replaces_the_old_one() {
    let server = start_server().await;
    let (abin, mut a) = Person::active(&server, "abin").await;
    let (mut b, _) = abin.sign_in(&server, &device("phone")).await;
    reauthed(&mut a, &abin.password).await;
    let new_code = secret("rc");
    let (None, Some(upload)) = reregister(&mut a, None, Some(&new_code)).await else {
        panic!("expected only a recovery registration");
    };
    a.send(&ClientFrame::NewRecoveryCode { recovery: upload, recovery_copy: Blob(copy()) }).await;
    assert_eq!(a.recv().await, ServerFrame::RecoveryCodeChanged);

    assert_eq!(devices(&mut b).await.len(), 2, "the other device stays signed in");
    let mut client = Client::open(&server).await;
    let outcome = recover(&mut client, &abin.username, &abin.recovery, &device("x")).await;
    assert!(matches!(outcome, Outcome::ClientRefused), "the old code still works: {outcome:?}");
    let outcome = recover(&mut client, &abin.username, &new_code, &device("x")).await;
    assert!(matches!(outcome, Outcome::Server(ServerFrame::RecoveryOffer { .. })), "got {outcome:?}");
}

#[tokio::test]
async fn cleanup_signs_out_idle_devices_but_not_connected_ones() {
    let server = start_server().await;
    let (abin, mut a) = Person::active(&server, "abin").await;
    let phone = device("phone");
    let (mut b, b_token) = abin.sign_in(&server, &phone).await;
    // Once the server answers the close, it has let go of the session.
    b.send_raw(Message::Close(None)).await;
    assert!(b.closed().await);

    server.state.clock.advance(server.state.limits.device_idle_secs + 1);
    let cleaned = cleanup::once(&server.state).await.unwrap();
    assert_eq!(cleaned.sessions.len(), 1);

    let (_, answer) = resume(&server, &b_token, &phone.id).await;
    assert_error(answer, ErrorCode::SessionUnknown);
    let list = devices(&mut a).await;
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].last_used, server.state.clock.now(), "a connected device counts as used");
}
