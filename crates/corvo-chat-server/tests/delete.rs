mod common;

use common::{
    chat::{contacts, first_message},
    client::{Client, TestServer, assert_error, start_server},
    person::{Outcome, Person, device, login, reauth, resume},
};
use corvo_chat_proto::frames::{ClientFrame, ErrorCode, ServerFrame, SignedOutReason};

const DELETED: ServerFrame = ServerFrame::SignedOut { reason: SignedOutReason::AccountDeleted };

/// Proves the password again, then deletes the account; returns the frame
/// that answers it.
async fn delete(client: &mut Client, person: &Person) -> ServerFrame {
    assert!(matches!(reauth(client, &person.password).await, Outcome::Server(ServerFrame::Reauthed)));
    client.send(&ClientFrame::DeleteAccount).await;
    client.recv().await
}

async fn count(server: &TestServer, sql: &'static str, id: i64) -> i64 {
    server.state.db.call(move |c| c.query_row(sql, [id], |r| r.get(0)).unwrap()).await
}

#[tokio::test]
async fn delete_needs_a_reauth() {
    let server = start_server().await;
    let (_abin, mut a) = Person::active(&server, "abin").await;
    a.send(&ClientFrame::DeleteAccount).await;
    assert_error(a.recv().await, ErrorCode::NotAllowed);
}

#[tokio::test]
async fn delete_signs_out_every_device() {
    let server = start_server().await;
    let (abin, mut a) = Person::active(&server, "abin").await;
    let phone = device("phone");
    let (mut a2, token) = abin.sign_in(&server, &phone).await;
    assert_eq!(delete(&mut a, &abin).await, DELETED);
    assert!(a.closed().await);
    assert_eq!(a2.recv().await, DELETED);
    assert!(a2.closed().await);
    for (token, id) in [(&abin.token, &abin.device.id), (&token, &phone.id)] {
        let (_, answer) = resume(&server, token, id).await;
        assert_error(answer, ErrorCode::SessionUnknown);
    }
}

#[tokio::test]
async fn other_side_keeps_the_chat_after_a_delete() {
    let server = start_server().await;
    let (abin, mut a, rahul, mut b) = contacts(&server).await;
    let chat_id = first_message(&mut a, &abin, rahul.number).await;
    a.send_body(&chat_id, b"bye".to_vec(), None).await;
    b.drain().await;
    assert_eq!(delete(&mut a, &abin).await, DELETED);
    let person = b.next_person(abin.number).await;
    assert!(person.deleted && person.display_name.is_none());

    let (mut fresh, _) = rahul.sign_in(&server, &device("fresh")).await;
    let synced = fresh.sync_from(None).await;
    let froms: Vec<_> = synced.histories[0].1.iter().map(|m| m.from).collect();
    assert_eq!(froms, vec![abin.number, abin.number]);
    assert!(synced.snapshot.expect("a snapshot").people.iter().any(|p| p.number == abin.number && p.deleted));
    match b.send_body(&chat_id, b"hello?".to_vec(), None).await {
        ServerFrame::NotSent { code, .. } => assert_eq!(code, ErrorCode::AccountDeleted),
        other => panic!("expected not_sent account_deleted, got {other:?}"),
    }
}

#[tokio::test]
async fn deleted_number_and_username_are_retired() {
    let server = start_server().await;
    let (abin, mut a, _rahul, mut b) = contacts(&server).await;
    let id = abin.id(&server).await;
    delete(&mut a, &abin).await;
    let mut client = Client::open(&server).await;
    let outcome = login(&mut client, &abin.username, &abin.password, &device("x")).await;
    assert!(matches!(outcome, Outcome::ClientRefused), "a fake offer: {outcome:?}");
    assert_eq!(count(&server, "SELECT wrong_count FROM accounts WHERE id = ?1", id).await, 0);
    b.send(&ClientFrame::Lookup { number: abin.number }).await;
    assert_error(b.recv().await, ErrorCode::NumberUnknown);
}

#[tokio::test]
async fn deleted_account_keeps_no_secrets() {
    let server = start_server().await;
    let (abin, mut a) = Person::active(&server, "abin").await;
    let id = abin.id(&server).await;
    delete(&mut a, &abin).await;
    let wiped = "SELECT length(password_record) + length(recovery_record) + length(password_copy)
                 + length(recovery_copy) + length(display_name) FROM accounts WHERE id = ?1";
    assert_eq!(count(&server, wiped, id).await, 0);
    for sql in [
        "SELECT COUNT(*) FROM pins WHERE account_id = ?1",
        "SELECT COUNT(*) FROM events WHERE account_id = ?1",
        "SELECT COUNT(*) FROM sessions WHERE account_id = ?1",
        "SELECT COUNT(*) FROM invites WHERE account_id = ?1 OR inviter_id = ?1",
        "SELECT COUNT(*) FROM relations WHERE account_id = ?1",
    ] {
        assert_eq!(count(&server, sql, id).await, 0, "{sql}");
    }
    assert_eq!(count(&server, "SELECT deleted_at IS NOT NULL FROM accounts WHERE id = ?1", id).await, 1);
}
