mod common;

use common::{
    client::{TestServer, start_server},
    person::Person,
};
use corvo_chat_proto::{
    Number,
    chat::EventBody,
    frames::{ClientFrame, ServerFrame},
};
use corvo_chat_server::{
    chats,
    events::{self, Ev},
    hub::Push,
};

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

fn pins(n: u32) -> Vec<Ev> {
    (0..n).map(|i| Ev::Pin { number: Number::from_body(100_000_000 + i).unwrap() }).collect()
}

#[tokio::test]
async fn sync_from_zero_replays_the_log_then_says_synced() {
    let server = start_server().await;
    let (abin, mut a) = Person::active(&server, "abin").await;
    append(&server, abin.id(&server).await, pins(3)).await;
    let synced = a.sync_from(Some(0)).await;
    assert_eq!(synced.events.iter().map(|e| e.seq).collect::<Vec<_>>(), vec![1, 2, 3]);
    assert!(synced.events.iter().all(|e| matches!(e.event, EventBody::Pin { .. })));
    assert_eq!(synced.cursor, 3);
}

#[tokio::test]
async fn live_events_arrive_after_sync_and_not_before() {
    let server = start_server().await;
    let (abin, mut a) = Person::active(&server, "abin").await;
    let id = abin.id(&server).await;
    append(&server, id, pins(1)).await;
    assert!(a.quiet().await, "an unsynced device gets no pushes");
    let synced = a.sync_from(Some(1)).await;
    assert!(synced.events.is_empty());
    assert_eq!(synced.cursor, 1);
    append(&server, id, pins(1)).await;
    assert_eq!(a.next_event().await.seq, 2);
    assert!(a.quiet().await);
}

#[tokio::test]
async fn pages_stop_at_one_hundred_events() {
    let server = start_server().await;
    let (abin, mut a) = Person::active(&server, "abin").await;
    append(&server, abin.id(&server).await, pins(250)).await;
    a.send(&ClientFrame::Sync { cursor: Some(0) }).await;
    for (len, cursor) in [(100, 100), (100, 200), (50, 250)] {
        match a.recv_raw().await {
            ServerFrame::Events { events, cursor: got } => assert_eq!((events.len(), got), (len, cursor)),
            other => panic!("expected events, got {other:?}"),
        }
    }
    assert_eq!(a.recv_raw().await, ServerFrame::Synced { cursor: 250 });
}

#[tokio::test]
async fn hidden_events_still_move_the_cursor() {
    let server = start_server().await;
    let (abin, mut a) = Person::active(&server, "abin").await;
    let (rahul, _b) = Person::active(&server, "rahul").await;
    let (a_id, b_id, now) = (abin.id(&server).await, rahul.id(&server).await, server.state.clock.now());
    // Rahul's chat with Abin exists, but hasn't been announced to Abin.
    server.state.db.call(move |c| chats::create(c, &[1; 16], b_id, a_id, &[1], &[2], &[3; 64], now).unwrap()).await;
    append(&server, a_id, vec![Ev::Chat { chat: [1; 16] }]).await;
    a.send(&ClientFrame::Sync { cursor: Some(0) }).await;
    assert_eq!(a.recv_raw().await, ServerFrame::Events { events: vec![], cursor: 1 });
    assert_eq!(a.recv_raw().await, ServerFrame::Synced { cursor: 1 });
}
