mod common;

use common::{
    chat::{contacts, first_message, new_chat, random_id},
    client::{Client, TestServer, assert_error, start_server},
    person::{Outcome, Person, device, login},
};
use corvo_chat_proto::frames::{ClientFrame, ErrorCode, ServerFrame, SignedOutReason};
use corvo_chat_server::{accounts, sweep};

/// Bans `person` the way the admin CLI does: the flag, and no sessions left.
async fn ban(server: &TestServer, person: &Person) {
    let (id, now) = (person.id(server).await, server.state.clock.now());
    server
        .state
        .db
        .call(move |c| {
            accounts::ban(c, id, now).unwrap();
            c.execute("DELETE FROM sessions WHERE account_id = ?1", [id]).unwrap();
        })
        .await;
}

fn assert_signed_out_banned(frame: ServerFrame) {
    assert_eq!(frame, ServerFrame::SignedOut { reason: SignedOutReason::Banned });
}

#[tokio::test]
async fn ban_shows_only_after_the_password_is_proven() {
    let server = start_server().await;
    let (abin, _a) = Person::active(&server, "abin").await;
    ban(&server, &abin).await;
    let mut client = Client::open(&server).await;
    match login(&mut client, &abin.username, &abin.password, &device("phone")).await {
        Outcome::Server(frame) => assert_error(frame, ErrorCode::Banned),
        Outcome::ClientRefused => panic!("the start must look normal and the password work"),
    }
    assert!(abin.wrong_count(&server).await <= 1);
}

#[tokio::test]
async fn banned_device_is_signed_out_by_the_sweep() {
    let server = start_server().await;
    let (rahul, mut b) = Person::active(&server, "rahul").await;
    ban(&server, &rahul).await;
    assert_eq!(sweep::once(&server.state).await.unwrap(), 1);
    assert_signed_out_banned(b.recv_raw().await);
    assert!(b.closed().await);
}

async fn sql(server: &TestServer, sql: &'static str, id: i64) {
    server.state.db.call(move |c| c.execute(sql, [id]).unwrap()).await;
}

#[tokio::test]
async fn sweep_signs_out_a_device_whose_session_is_gone() {
    let server = start_server().await;
    let (rahul, mut b) = Person::active(&server, "rahul").await;
    sql(&server, "DELETE FROM sessions WHERE account_id = ?1", rahul.id(&server).await).await;
    assert_eq!(sweep::once(&server.state).await.unwrap(), 1);
    assert_eq!(b.recv_raw().await, ServerFrame::SignedOut { reason: SignedOutReason::Expired });
    assert!(b.closed().await);
}

#[tokio::test]
async fn sweep_signs_out_a_banned_account_that_kept_its_session() {
    let server = start_server().await;
    let (rahul, mut b) = Person::active(&server, "rahul").await;
    let (id, now) = (rahul.id(&server).await, server.state.clock.now());
    server.state.db.call(move |c| accounts::ban(c, id, now).unwrap()).await;
    assert_eq!(sweep::once(&server.state).await.unwrap(), 1);
    assert_signed_out_banned(b.recv_raw().await);
    assert!(b.closed().await);
}

#[tokio::test]
async fn sweep_signs_out_a_deleted_account() {
    let server = start_server().await;
    let (rahul, mut b) = Person::active(&server, "rahul").await;
    sql(&server, "UPDATE accounts SET deleted_at = 1 WHERE id = ?1", rahul.id(&server).await).await;
    assert_eq!(sweep::once(&server.state).await.unwrap(), 1);
    assert_eq!(b.recv_raw().await, ServerFrame::SignedOut { reason: SignedOutReason::AccountDeleted });
    assert!(b.closed().await);
}

#[tokio::test]
async fn sweep_leaves_a_healthy_device_alone() {
    let server = start_server().await;
    let (_rahul, mut b) = Person::active(&server, "rahul").await;
    assert_eq!(sweep::once(&server.state).await.unwrap(), 0);
    assert!(b.quiet().await);
}

#[tokio::test]
async fn resend_after_a_ban_gets_the_stored_copy() {
    let server = start_server().await;
    let (abin, mut a, rahul, _b) = contacts(&server).await;
    let chat_id = first_message(&mut a, &abin, rahul.number).await;
    let msg_id = random_id();
    let first = a.send_with_id(&chat_id, &msg_id, b"x".to_vec(), None).await;
    assert!(matches!(first, ServerFrame::Sent { seq: 2, .. }), "{first:?}");
    ban(&server, &rahul).await;
    assert_eq!(a.send_with_id(&chat_id, &msg_id, b"x".to_vec(), None).await, first);
    match a.send_body(&chat_id, b"y".to_vec(), None).await {
        ServerFrame::NotSent { code, .. } => assert_eq!(code, ErrorCode::Banned),
        other => panic!("expected not_sent banned, got {other:?}"),
    }
}

#[tokio::test]
async fn banned_devices_next_frame_signs_it_out() {
    let server = start_server().await;
    let (rahul, mut b) = Person::active(&server, "rahul").await;
    ban(&server, &rahul).await;
    b.send(&ClientFrame::ListDevices).await;
    assert_signed_out_banned(b.recv_raw().await);
    assert!(b.closed().await);
}

#[tokio::test]
async fn messages_and_lookups_to_a_banned_account_are_refused() {
    let server = start_server().await;
    let (abin, mut a, rahul, _b) = contacts(&server).await;
    let (sam, _s) = Person::active(&server, "sam").await;
    let chat_id = first_message(&mut a, &abin, rahul.number).await;
    ban(&server, &rahul).await;
    ban(&server, &sam).await;
    a.send(&ClientFrame::Lookup { number: rahul.number }).await;
    assert_error(a.recv().await, ErrorCode::Banned);
    for (chat, new) in [(chat_id, None), {
        let id = random_id();
        let new = Some(new_chat(&abin, &id, sam.number));
        (id, new)
    }] {
        match a.send_body(&chat, b"x".to_vec(), new).await {
            ServerFrame::NotSent { code, .. } => assert_eq!(code, ErrorCode::Banned),
            other => panic!("expected not_sent banned, got {other:?}"),
        }
    }
}

#[tokio::test]
async fn unbanned_account_can_sign_in_again() {
    let server = start_server().await;
    let (abin, _a) = Person::active(&server, "abin").await;
    ban(&server, &abin).await;
    let id = abin.id(&server).await;
    server.state.db.call(move |c| accounts::unban(c, id).unwrap()).await;
    abin.sign_in(&server, &device("phone")).await;
}

async fn report(
    client: &mut Client,
    number: corvo_chat_proto::Number,
    reason: Option<&str>,
    block: bool,
) -> ServerFrame {
    client.send(&ClientFrame::Report { number, reason: reason.map(String::from), block }).await;
    client.recv().await
}

#[tokio::test]
async fn report_is_stored_and_can_block() {
    use corvo_chat_proto::chat::Relation;
    let server = start_server().await;
    let (abin, _a, rahul, mut b) = contacts(&server).await;
    assert_eq!(report(&mut b, abin.number, Some("  spam links  "), true).await, ServerFrame::Reported);
    assert_eq!(b.next_person(abin.number).await.relation, Relation::Blocked);
    let (a_id, b_id) = (abin.id(&server).await, rahul.id(&server).await);
    let row: (i64, Option<String>) = server
        .state
        .db
        .call(move |c| {
            c.query_row("SELECT reporter_id, reason FROM reports WHERE reported_id = ?1", [a_id], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap()
        })
        .await;
    assert_eq!(row, (b_id, Some("spam links".into())));
}

#[tokio::test]
async fn long_reason_is_refused() {
    let server = start_server().await;
    let (abin, _a, _rahul, mut b) = contacts(&server).await;
    let reason = "x".repeat(201);
    assert_error(report(&mut b, abin.number, Some(&reason), false).await, ErrorCode::BadFrame);
    assert_eq!(report(&mut b, abin.number, Some(&"é".repeat(200)), false).await, ServerFrame::Reported);
}

#[tokio::test]
async fn eleventh_report_in_a_day_is_refused() {
    let server = start_server().await;
    let (abin, _a, _rahul, mut b) = contacts(&server).await;
    for _ in 0..10 {
        assert_eq!(report(&mut b, abin.number, None, false).await, ServerFrame::Reported);
    }
    assert_error(report(&mut b, abin.number, None, false).await, ErrorCode::RateLimited);
}

#[tokio::test]
async fn refused_report_charges_no_lookup() {
    let server = start_server().await;
    let (abin, mut a, rahul, _b) = common::chat::strangers(&server).await;
    for _ in 0..10 {
        assert_eq!(report(&mut a, rahul.number, None, false).await, ServerFrame::Reported);
    }
    assert_error(report(&mut a, rahul.number, None, false).await, ErrorCode::RateLimited);
    let key = abin.id(&server).await.to_string();
    let lookups: i64 = server
        .state
        .db
        .call(move |c| {
            c.query_row("SELECT COUNT(*) FROM attempts WHERE kind = 'lookup' AND key = ?1", [key], |r| r.get(0))
                .unwrap()
        })
        .await;
    assert_eq!(lookups, 10);
}
