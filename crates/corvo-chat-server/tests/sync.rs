mod common;

use common::{
    chat::{contacts, new_chat, random_id},
    client::{Client, TestServer, assert_error, start_server},
    person::{Person, device},
};
use corvo_chat_proto::{
    B64, Number,
    chat::{EventBody, Message, Relation},
    frames::{ClientFrame, ErrorCode, ServerFrame},
};
use corvo_chat_server::{
    chats,
    events::{self, Ev},
    hub::Push,
};
use tokio_tungstenite::tungstenite::Message as WsMessage;

/// Appends `evs` to the account's log and pokes it, as handlers do.
async fn append(server: &TestServer, account: i64, evs: Vec<Ev>) {
    let hub = server.state.hub.clone();
    server
        .state
        .db
        .call(move |c| {
            for ev in &evs {
                events::append(c, account, ev).unwrap();
            }
            hub.push(account, Push::Events);
        })
        .await;
}

/// The account's last event seq.
async fn head(server: &TestServer, account: i64) -> i64 {
    server.state.db.call(move |c| events::head(c, account).unwrap()).await
}

fn pins(n: u32) -> Vec<Ev> {
    (0..n).map(|i| Ev::Pin { number: Number::from_body(100_000_000 + i).unwrap() }).collect()
}

#[tokio::test]
async fn sync_from_zero_replays_the_log_then_says_synced() {
    let server = start_server().await;
    let (abin, mut a) = Person::active(&server, "abin").await;
    let id = abin.id(&server).await;
    let base = head(&server, id).await;
    append(&server, id, pins(3)).await;
    let synced = a.sync_from(Some(base)).await;
    assert_eq!(synced.events.iter().map(|e| e.seq).collect::<Vec<_>>(), vec![base + 1, base + 2, base + 3]);
    assert!(synced.events.iter().all(|e| matches!(e.event, EventBody::Pin { .. })));
    assert_eq!(synced.cursor, base + 3);
}

#[tokio::test]
async fn live_events_arrive_after_sync_and_not_before() {
    let server = start_server().await;
    let (abin, mut a) = Person::active(&server, "abin").await;
    let id = abin.id(&server).await;
    append(&server, id, pins(1)).await;
    assert!(a.quiet().await, "an unsynced device gets no pushes");
    let at = head(&server, id).await;
    let synced = a.sync_from(Some(at)).await;
    assert!(synced.events.is_empty());
    assert_eq!(synced.cursor, at);
    append(&server, id, pins(1)).await;
    assert_eq!(a.next_event().await.seq, at + 1);
    assert!(a.quiet().await);
}

#[tokio::test]
async fn pages_stop_at_one_hundred_events() {
    let server = start_server().await;
    let (abin, mut a) = Person::active(&server, "abin").await;
    let id = abin.id(&server).await;
    let base = head(&server, id).await;
    append(&server, id, pins(250)).await;
    a.send(&ClientFrame::Sync { cursor: Some(base) }).await;
    for (len, cursor) in [(100, 100), (100, 200), (50, 250)] {
        match a.recv_raw().await {
            ServerFrame::Events { events, cursor: got } => assert_eq!((events.len(), got), (len, base + cursor)),
            other => panic!("expected events, got {other:?}"),
        }
    }
    assert_eq!(a.recv_raw().await, ServerFrame::Synced { cursor: base + 250 });
}

#[tokio::test]
async fn hidden_events_still_move_the_cursor() {
    let server = start_server().await;
    let (abin, mut a) = Person::active(&server, "abin").await;
    let (rahul, _b) = Person::active(&server, "rahul").await;
    let (a_id, b_id, now) = (abin.id(&server).await, rahul.id(&server).await, server.state.clock.now());
    // Rahul's chat with Abin exists, but hasn't been announced to Abin.
    server.state.db.call(move |c| chats::create(c, &[1; 16], b_id, a_id, &[1], &[2], &[3; 64], now).unwrap()).await;
    let base = head(&server, a_id).await;
    append(&server, a_id, vec![Ev::Chat { chat: [1; 16] }]).await;
    a.send(&ClientFrame::Sync { cursor: Some(base) }).await;
    assert_eq!(a.recv_raw().await, ServerFrame::Events { events: vec![], cursor: base + 1 });
    assert_eq!(a.recv_raw().await, ServerFrame::Synced { cursor: base + 1 });
}

/// Abin and Rahul as contacts with 120 messages between them (60 each, to
/// stay inside the per-minute limit).
async fn long_chat(server: &TestServer) -> (Person, Client, Person, Client, B64<16>) {
    let (abin, mut a, rahul, mut b) = contacts(server).await;
    let chat_id = random_id();
    a.send_body(&chat_id, b"1".to_vec(), Some(new_chat(&abin, &chat_id, rahul.number))).await;
    for n in 2..=120 {
        let sender = if n % 2 == 0 { &mut b } else { &mut a };
        let sent = sender.send_body(&chat_id, n.to_string().into_bytes(), None).await;
        assert!(matches!(sent, ServerFrame::Sent { .. }), "message {n}: {sent:?}");
    }
    (abin, a, rahul, b, chat_id)
}

fn seqs(messages: &[Message]) -> Vec<i64> {
    messages.iter().map(|m| m.seq).collect()
}

async fn history(client: &mut Client, chat_id: &B64<16>, before: i64, limit: u32) -> (Vec<i64>, bool) {
    client.send(&ClientFrame::History { chat_id: chat_id.clone(), before, limit }).await;
    match client.recv().await {
        ServerFrame::History { messages, complete, .. } => (seqs(&messages), complete),
        other => panic!("expected history, got {other:?}"),
    }
}

#[tokio::test]
async fn new_device_gets_a_snapshot_with_the_latest_fifty_per_chat() {
    let server = start_server().await;
    let (abin, _a, rahul, _b, chat_id) = long_chat(&server).await;
    let (mut phone, _) = rahul.sign_in(&server, &device("phone")).await;
    let synced = phone.sync_from(None).await;
    let snapshot = synced.snapshot.expect("a snapshot");
    assert_eq!(snapshot.account.number, rahul.number);
    assert_eq!(snapshot.chats.iter().map(|c| c.chat_id.clone()).collect::<Vec<_>>(), vec![chat_id.clone()]);
    assert!(snapshot.people.iter().any(|p| p.number == abin.number && p.relation == Relation::Contact));
    assert_eq!(synced.histories.len(), 1);
    let (id, messages, complete) = &synced.histories[0];
    assert_eq!((id, seqs(messages), *complete), (&chat_id, (71..=120).collect(), false));
    let rahul_id = rahul.id(&server).await;
    let head = server.state.db.call(move |c| events::head(c, rahul_id).unwrap()).await;
    assert_eq!((snapshot.cursor, synced.cursor), (head, head));
}

#[tokio::test]
async fn history_pages_back_to_the_start() {
    let server = start_server().await;
    let (_abin, _a, _rahul, mut b, chat_id) = long_chat(&server).await;
    assert_eq!(history(&mut b, &chat_id, 71, 50).await, ((21..=70).collect(), false));
    assert_eq!(history(&mut b, &chat_id, 21, 50).await, ((1..=20).collect(), true));
    assert_eq!(history(&mut b, &chat_id, i64::MAX, 500).await, ((71..=120).collect(), false));
}

#[tokio::test]
async fn history_of_a_chat_i_am_not_in_is_refused() {
    let server = start_server().await;
    let (abin, mut a, rahul, _b) = contacts(&server).await;
    let (_sam, mut s) = Person::active(&server, "sam").await;
    let chat_id = random_id();
    a.send_body(&chat_id, b"x".to_vec(), Some(new_chat(&abin, &chat_id, rahul.number))).await;
    s.send(&ClientFrame::History { chat_id, before: i64::MAX, limit: 50 }).await;
    assert_error(s.recv().await, ErrorCode::ChatUnknown);
}

#[tokio::test]
async fn cursor_past_the_head_gets_a_snapshot() {
    let server = start_server().await;
    let (abin, mut a) = Person::active(&server, "abin").await;
    let id = abin.id(&server).await;
    append(&server, id, pins(2)).await;
    let at = head(&server, id).await;
    let synced = a.sync_from(Some(at + 10)).await;
    assert_eq!(synced.snapshot.expect("a snapshot").cursor, at);
    assert_eq!(synced.cursor, at);
}

#[tokio::test]
async fn offline_device_catches_up_to_the_same_state() {
    let server = start_server().await;
    let (abin, mut a, rahul, mut b) = contacts(&server).await;
    let (mut phone, _) = rahul.sign_in(&server, &device("phone")).await;
    let cursor = phone.sync_from(Some(0)).await.cursor;
    phone.send_raw(WsMessage::Close(None)).await;
    assert!(phone.closed().await);

    let chat_id = random_id();
    a.send_body(&chat_id, b"1".to_vec(), Some(new_chat(&abin, &chat_id, rahul.number))).await;
    for n in 2..=5 {
        a.send_body(&chat_id, n.to_string().into_bytes(), None).await;
    }
    // Rahul's side is cleared, as removing with history will do.
    let (rahul_id, hub) = (rahul.id(&server).await, server.state.hub.clone());
    let chat = chat_id.0;
    server
        .state
        .db
        .call(move |c| {
            chats::clear(c, &chat, rahul_id).unwrap();
            events::append(c, rahul_id, &Ev::Cleared { chat }).unwrap();
            hub.push(rahul_id, Push::Events);
        })
        .await;
    for n in 6..=7 {
        a.send_body(&chat_id, n.to_string().into_bytes(), None).await;
    }

    // The device that was away catches up from its cursor.
    let (mut phone, _) = rahul.sign_in(&server, &device("phone")).await;
    let synced = phone.sync_from(Some(cursor)).await;

    // The connected device saw everything live, up to the same place, and
    // applies the clear.
    let (mut live, mut cleared, mut last) = (Vec::new(), 0, 0);
    while last < synced.cursor {
        let event = b.next_event().await;
        last = event.seq;
        match event.event {
            EventBody::Message { message } => live.push(message.seq),
            EventBody::Cleared { cleared_through, .. } => cleared = cleared_through,
            _ => {}
        }
    }
    let live: Vec<i64> = live.into_iter().filter(|seq| *seq > cleared).collect();
    let caught_up: Vec<i64> = synced
        .events
        .iter()
        .filter_map(|e| match &e.event {
            EventBody::Message { message } => Some(message.seq),
            _ => None,
        })
        .collect();
    assert_eq!(caught_up, live);
    assert_eq!(caught_up, vec![6, 7]);
    assert_eq!(synced.cursor, last);
}
