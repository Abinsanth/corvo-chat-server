mod common;

use common::{
    chat::{contacts, first_message, new_chat, random_id, strangers},
    client::{Client, TestServer, assert_error, start_server, start_server_with},
    person::{Person, device},
};
use corvo_chat_proto::{
    Number,
    chat::{Event, EventBody, Marks, Message, Relation},
    frames::{ClientFrame, ErrorCode, ServerFrame},
};
use corvo_chat_server::{limits::Limits, relations};
use tokio_tungstenite::tungstenite::Message as WsMessage;

/// Runs a counting query with one parameter.
async fn count(server: &TestServer, sql: &'static str, arg: impl rusqlite::ToSql + Send + 'static) -> i64 {
    server.state.db.call(move |c| c.query_row(sql, [arg], |r| r.get(0)).unwrap()).await
}

async fn set_relation(server: &TestServer, me: &Person, other: &Person, relation: Relation) {
    let (me, other, now) = (me.id(server).await, other.id(server).await, server.state.clock.now());
    server.state.db.call(move |c| relations::set(c, me, other, relation, now).unwrap()).await;
}

fn assert_sent(frame: &ServerFrame) -> i64 {
    match frame {
        ServerFrame::Sent { seq, .. } => *seq,
        other => panic!("expected sent, got {other:?}"),
    }
}

fn assert_not_sent(frame: ServerFrame, code: ErrorCode) {
    match frame {
        ServerFrame::NotSent { code: got, .. } => assert_eq!(got, code),
        other => panic!("expected not_sent {code:?}, got {other:?}"),
    }
}

/// The next pushed message, skipping other events.
async fn next_message(client: &mut Client) -> Message {
    loop {
        if let EventBody::Message { message } = client.next_event().await.event {
            return message;
        }
    }
}

async fn take(client: &mut Client, n: usize) -> Vec<Event> {
    let mut events = Vec::new();
    for _ in 0..n {
        events.push(client.next_event().await);
    }
    events
}

async fn lookup(client: &mut Client, number: Number) -> ServerFrame {
    client.send(&ClientFrame::Lookup { number }).await;
    client.recv().await
}

#[tokio::test]
async fn lookup_returns_only_the_public_keys() {
    let server = start_server().await;
    let (_abin, mut a, rahul, _b) = strangers(&server).await;
    assert_eq!(
        lookup(&mut a, rahul.number).await,
        ServerFrame::Found { number: rahul.number, public_keys: rahul.public_keys() }
    );
}

#[tokio::test]
async fn lookup_of_my_own_pending_or_unknown_number_is_refused() {
    let server = start_server().await;
    let (abin, mut a) = Person::active(&server, "abin").await;
    let (pending, _p) = Person::sign_up(&server, "sam").await;
    for number in [abin.number, pending.number, Number::from_body(999_999_999).unwrap()] {
        assert_error(lookup(&mut a, number).await, ErrorCode::NumberUnknown);
    }
}

#[tokio::test]
async fn thirty_first_lookup_in_an_hour_is_refused() {
    let server = start_server().await;
    let (_abin, mut a, rahul, _b) = strangers(&server).await;
    for _ in 0..30 {
        assert!(matches!(lookup(&mut a, rahul.number).await, ServerFrame::Found { .. }));
    }
    assert_error(lookup(&mut a, rahul.number).await, ErrorCode::RateLimited);
    server.state.clock.advance(3_600);
    assert!(matches!(lookup(&mut a, rahul.number).await, ServerFrame::Found { .. }));
}

#[tokio::test]
async fn first_message_makes_a_chat_and_a_request() {
    let server = start_server().await;
    let (abin, mut a, rahul, mut b) = strangers(&server).await;
    let chat_id = first_message(&mut a, &abin, rahul.number).await;

    let mine = take(&mut a, 3).await;
    assert!(
        mine.iter().any(
            |e| matches!(&e.event, EventBody::Chat { chat } if chat.chat_id == chat_id && chat.with == rahul.number)
        )
    );
    assert!(mine.iter().any(|e| matches!(&e.event,
        EventBody::Person { person } if person.number == rahul.number && person.relation == Relation::Asked && person.display_name.is_none())));
    assert!(mine.iter().any(|e| matches!(&e.event, EventBody::Message { message } if message.from == abin.number)));

    let theirs = take(&mut b, 3).await;
    assert!(
        theirs.iter().any(
            |e| matches!(&e.event, EventBody::Chat { chat } if chat.chat_id == chat_id && chat.with == abin.number)
        )
    );
    assert!(theirs.iter().any(|e| matches!(&e.event,
        EventBody::Person { person } if person.number == abin.number && person.relation == Relation::Request && person.display_name.as_deref() == Some("Abin"))));
    assert!(theirs.iter().any(|e| matches!(&e.event,
        EventBody::Message { message } if message.from == abin.number && message.body.0 == b"hello" && message.seq == 1)));
}

#[tokio::test]
async fn message_to_a_contact_reaches_every_device_of_both() {
    let server = start_server().await;
    let (abin, mut a, rahul, mut b) = contacts(&server).await;
    let (mut a2, _) = abin.sign_in(&server, &device("phone")).await;
    a2.sync_from(Some(0)).await;
    let chat_id = first_message(&mut a, &abin, rahul.number).await;
    for client in [&mut a2, &mut b] {
        let message = next_message(client).await;
        assert_eq!((message.chat_id.clone(), message.from, message.seq), (chat_id.clone(), abin.number, 1));
    }
}

#[tokio::test]
async fn bad_chat_signature_is_refused() {
    let server = start_server().await;
    let (abin, mut a, rahul, _b) = strangers(&server).await;
    let chat_id = random_id();
    let mut chat = new_chat(&abin, &chat_id, rahul.number);
    chat.signature.0[0] ^= 1;
    assert_not_sent(a.send_body(&chat_id, b"x".to_vec(), Some(chat)).await, ErrorCode::BadFrame);
    assert_eq!(count(&server, "SELECT COUNT(*) FROM chats WHERE ?1 = 1", 1).await, 0);
}

#[tokio::test]
async fn sending_into_someone_elses_chat_is_refused() {
    let server = start_server().await;
    let (abin, mut a, rahul, _b) = strangers(&server).await;
    let (_sam, mut s) = Person::active(&server, "sam").await;
    let chat_id = first_message(&mut a, &abin, rahul.number).await;
    assert_not_sent(s.send_body(&chat_id, b"x".to_vec(), None).await, ErrorCode::ChatUnknown);
    assert_not_sent(s.send_body(&random_id(), b"x".to_vec(), None).await, ErrorCode::ChatUnknown);
}

#[tokio::test]
async fn simultaneous_first_messages_make_one_chat() {
    let server = start_server().await;
    let (abin, mut a, rahul, mut b) = strangers(&server).await;
    let x = first_message(&mut a, &abin, rahul.number).await;
    let y = random_id();
    let refused = b.send_body(&y, b"hi".to_vec(), Some(new_chat(&rahul, &y, abin.number))).await;
    assert_not_sent(refused, ErrorCode::ChatExists);
    let mut seen = false;
    for event in take(&mut b, 3).await {
        seen |= matches!(event.event, EventBody::Chat { chat } if chat.chat_id == x);
    }
    assert!(seen, "Rahul has the existing chat");
    // Rahul has a request from Abin, so he accepts before writing.
    assert_not_sent(b.send_body(&x, b"hi".to_vec(), None).await, ErrorCode::NotAllowed);
    set_relation(&server, &rahul, &abin, Relation::Contact).await;
    assert_eq!(assert_sent(&b.send_body(&x, b"hi".to_vec(), None).await), 2);
    assert_eq!(count(&server, "SELECT COUNT(*) FROM chats WHERE ?1 = 1", 1).await, 1);
}

#[tokio::test]
async fn chat_exists_announces_a_withheld_chat_to_its_sender() {
    // No room for requests: Abin's first message is withheld from Rahul.
    let server = start_server_with(Limits { signups_per_ip_day: 1_000, open_requests: 0, ..Limits::default() }).await;
    let (abin, mut a, rahul, mut b) = strangers(&server).await;
    let x = first_message(&mut a, &abin, rahul.number).await;
    assert!(b.quiet().await);
    let y = random_id();
    let refused = b.send_body(&y, b"hi".to_vec(), Some(new_chat(&rahul, &y, abin.number))).await;
    assert_not_sent(refused, ErrorCode::ChatExists);
    match b.next_event().await.event {
        EventBody::Chat { chat } => assert_eq!(chat.chat_id, x),
        other => panic!("expected the chat, got {other:?}"),
    }
    // Both asked, so both are contacts now, and the reply goes through.
    assert_eq!(assert_sent(&b.send_body(&x, b"hi".to_vec(), None).await), 2);
    let (a_id, b_id) = (abin.id(&server).await, rahul.id(&server).await);
    let rows = server
        .state
        .db
        .call(move |c| (relations::get(c, a_id, b_id).unwrap(), relations::get(c, b_id, a_id).unwrap()))
        .await;
    assert_eq!(rows, (Relation::Contact, Relation::Contact));
}

#[tokio::test]
async fn resent_message_is_stored_once() {
    let server = start_server().await;
    let (abin, mut a, rahul, _b) = contacts(&server).await;
    let chat_id = random_id();
    let msg_id = random_id();
    let first = a.send_with_id(&chat_id, &msg_id, b"x".to_vec(), Some(new_chat(&abin, &chat_id, rahul.number))).await;
    let again = a.send_with_id(&chat_id, &msg_id, b"x".to_vec(), None).await;
    assert_eq!(first, again);
    assert_eq!(count(&server, "SELECT COUNT(*) FROM messages WHERE ?1 = 1", 1).await, 1);
    let key = abin.id(&server).await.to_string();
    assert_eq!(count(&server, "SELECT COUNT(*) FROM attempts WHERE kind = 'message' AND key = ?1", key).await, 1);
}

#[tokio::test]
async fn blocked_sender_gets_sent_and_nothing_arrives() {
    let server = start_server().await;
    let (abin, mut a, rahul, mut b) = contacts(&server).await;
    set_relation(&server, &rahul, &abin, Relation::Blocked).await;
    first_message(&mut a, &abin, rahul.number).await;
    assert!(b.quiet().await);
    let mut fresh = rahul.client_for(&server).await;
    let synced = fresh.sync_from(Some(0)).await;
    assert!(!synced.events.iter().any(|e| matches!(e.event, EventBody::Message { .. } | EventBody::Chat { .. })));
    let snapshot = fresh.sync_from(None).await;
    assert!(snapshot.snapshot.expect("a snapshot").chats.is_empty());
    assert!(snapshot.histories.is_empty());
    // Abin's own devices still have it.
    let mut a2 = abin.client_for(&server).await;
    let mine = a2.sync_from(Some(0)).await;
    assert!(mine.events.iter().any(|e| matches!(e.event, EventBody::Message { .. })));
}

#[tokio::test]
async fn replying_to_a_request_needs_accept_first() {
    let server = start_server().await;
    let (abin, mut a, _rahul, mut b) = strangers(&server).await;
    let chat_id = first_message(&mut a, &abin, _rahul.number).await;
    assert_not_sent(b.send_body(&chat_id, b"x".to_vec(), None).await, ErrorCode::NotAllowed);
}

#[tokio::test]
async fn messaging_someone_who_blocked_you_looks_normal() {
    let server = start_server().await;
    let (abin, mut a, rahul, _b) = contacts(&server).await;
    let chat_id = first_message(&mut a, &abin, rahul.number).await;
    set_relation(&server, &rahul, &abin, Relation::Blocked).await;
    let sent = a.send_body(&chat_id, b"x".to_vec(), None).await;
    assert!(matches!(sent, ServerFrame::Sent { seq: 2, .. }), "got {sent:?}");
}

#[tokio::test]
async fn sixty_first_message_in_a_minute_is_refused() {
    let server = start_server().await;
    let (abin, mut a, rahul, _b) = contacts(&server).await;
    let chat_id = first_message(&mut a, &abin, rahul.number).await;
    for _ in 1..60 {
        assert_sent(&a.send_body(&chat_id, b"x".to_vec(), None).await);
    }
    assert_not_sent(a.send_body(&chat_id, b"x".to_vec(), None).await, ErrorCode::RateLimited);
    server.state.clock.advance(60);
    assert_sent(&a.send_body(&chat_id, b"x".to_vec(), None).await);
}

#[tokio::test]
async fn twenty_first_new_person_in_a_day_is_refused() {
    let server = start_server().await;
    let (abin, mut a) = Person::active(&server, "abin").await;
    let mut chats = Vec::new();
    for n in 0..20 {
        let (other, _c) = Person::active(&server, &format!("person{n}")).await;
        chats.push(first_message(&mut a, &abin, other.number).await);
    }
    let (last, _c) = Person::active(&server, "last").await;
    let chat_id = random_id();
    let refused = a.send_body(&chat_id, b"x".to_vec(), Some(new_chat(&abin, &chat_id, last.number))).await;
    assert_not_sent(refused, ErrorCode::RateLimited);
    assert_sent(&a.send_body(&chats[0], b"again".to_vec(), None).await);
}

#[tokio::test]
async fn twenty_first_request_is_withheld() {
    let server = start_server().await;
    let (rahul, _b) = Person::active(&server, "rahul").await;
    for n in 0..21 {
        let (other, mut c) = Person::active(&server, &format!("person{n}")).await;
        first_message(&mut c, &other, rahul.number).await;
    }
    let id = rahul.id(&server).await;
    assert_eq!(
        count(&server, "SELECT COUNT(*) FROM relations WHERE account_id = ?1 AND state = 'request'", id).await,
        20
    );
    assert_eq!(count(&server, "SELECT COUNT(*) FROM messages WHERE withheld = ?1", 1).await, 1);
}

#[tokio::test]
async fn twenty_first_message_of_a_request_is_withheld() {
    let server = start_server().await;
    let (abin, mut a, rahul, _b) = strangers(&server).await;
    let chat_id = first_message(&mut a, &abin, rahul.number).await;
    for _ in 1..21 {
        assert_sent(&a.send_body(&chat_id, b"x".to_vec(), None).await);
    }
    assert_eq!(count(&server, "SELECT COUNT(*) FROM messages WHERE withheld = ?1", 1).await, 1);
    assert_eq!(count(&server, "SELECT MAX(seq) FROM messages WHERE withheld = ?1", 1).await, 21);
}

#[tokio::test]
async fn body_over_64_kib_is_a_bad_frame() {
    use data_encoding::BASE64;
    let server = start_server().await;
    let (_abin, mut a, rahul, _b) = strangers(&server).await;
    let raw = format!(
        r#"{{"type":"send","chat_id":"{id}","msg_id":"{id}","body":"{body}"}}"#,
        id = BASE64.encode(&[0; 16]),
        body = BASE64.encode(&vec![0; 65_537])
    );
    a.send_raw(WsMessage::text(raw)).await;
    assert_error(a.recv().await, ErrorCode::BadFrame);
    assert!(matches!(lookup(&mut a, rahul.number).await, ServerFrame::Found { .. }));
}

/// The next pushed `marks`, skipping other events.
async fn next_marks(client: &mut Client) -> Marks {
    loop {
        if let EventBody::Marks { marks, .. } = client.next_event().await.event {
            return marks;
        }
    }
}

#[tokio::test]
async fn marks_move_from_sent_to_received_to_read() {
    let server = start_server().await;
    let (abin, mut a, rahul, mut b) = contacts(&server).await;
    let chat_id = first_message(&mut a, &abin, rahul.number).await;
    b.send(&ClientFrame::Received { chat_id: chat_id.clone(), through: 1 }).await;
    assert_eq!(next_marks(&mut a).await, Marks { received_through: 1, read_through: 0, withheld: vec![] });
    b.send(&ClientFrame::Read { chat_id, through: 1 }).await;
    assert_eq!(next_marks(&mut a).await, Marks { received_through: 1, read_through: 1, withheld: vec![] });
}

#[tokio::test]
async fn first_ack_from_any_device_counts() {
    let server = start_server().await;
    let (abin, mut a, rahul, mut b) = contacts(&server).await;
    let (mut b2, _) = rahul.sign_in(&server, &device("phone")).await;
    let chat_id = first_message(&mut a, &abin, rahul.number).await;
    b2.send(&ClientFrame::Received { chat_id: chat_id.clone(), through: 1 }).await;
    assert_eq!(next_marks(&mut a).await, Marks { received_through: 1, read_through: 0, withheld: vec![] });
    b.send(&ClientFrame::Received { chat_id, through: 1 }).await;
    assert!(a.quiet().await, "a second device's ack changes nothing");
}

#[tokio::test]
async fn reading_on_one_device_tells_my_other_devices() {
    let server = start_server().await;
    let (abin, mut a, rahul, mut b) = contacts(&server).await;
    let (mut b2, _) = rahul.sign_in(&server, &device("phone")).await;
    let chat_id = first_message(&mut a, &abin, rahul.number).await;
    b2.send(&ClientFrame::Read { chat_id: chat_id.clone(), through: 1 }).await;
    loop {
        if let EventBody::Read { chat_id: read, read_through } = b.next_event().await.event {
            assert_eq!((read, read_through), (chat_id, 1));
            break;
        }
    }
}

#[tokio::test]
async fn acks_never_move_backwards() {
    let server = start_server().await;
    let (abin, mut a, rahul, mut b) = contacts(&server).await;
    let chat_id = first_message(&mut a, &abin, rahul.number).await;
    assert_sent(&a.send_body(&chat_id, b"x".to_vec(), None).await);
    b.send(&ClientFrame::Read { chat_id: chat_id.clone(), through: 2 }).await;
    assert_eq!(next_marks(&mut a).await, Marks { received_through: 2, read_through: 2, withheld: vec![] });
    b.send(&ClientFrame::Read { chat_id: chat_id.clone(), through: 1 }).await;
    b.send(&ClientFrame::Received { chat_id, through: 1 }).await;
    assert!(a.quiet().await);
}

#[tokio::test]
async fn acks_for_a_chat_i_am_not_in_are_refused() {
    let server = start_server().await;
    let (abin, mut a, rahul, _b) = contacts(&server).await;
    let (_sam, mut s) = Person::active(&server, "sam").await;
    let chat_id = first_message(&mut a, &abin, rahul.number).await;
    s.send(&ClientFrame::Read { chat_id, through: 1 }).await;
    assert_error(s.recv().await, ErrorCode::ChatUnknown);
}

#[tokio::test]
async fn refused_first_message_leaves_no_chat() {
    let server =
        start_server_with(Limits { signups_per_ip_day: 1_000, new_people_per_day: 0, ..Limits::default() }).await;
    let (abin, mut a, rahul, _b) = strangers(&server).await;
    let chat_id = random_id();
    let refused = a.send_body(&chat_id, b"x".to_vec(), Some(new_chat(&abin, &chat_id, rahul.number))).await;
    assert_not_sent(refused, ErrorCode::RateLimited);
    assert_eq!(count(&server, "SELECT COUNT(*) FROM chats WHERE ?1 = 1", 1).await, 0);
    assert!(a.quiet().await, "no chat event");
}
