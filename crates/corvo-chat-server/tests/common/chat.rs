//! Chat helpers for the integration tests: first-message chats signed with
//! the test person's key, sending, syncing, and reading pushed frames.

use std::time::Duration;

use corvo_chat_proto::{
    B64, Blob, Body, Number,
    chat::{Event, EventBody, Message, NewChat, Person, Snapshot},
    chat_proof,
    frames::{ClientFrame, ServerFrame},
};
use corvo_chat_server::relations;
use ed25519_dalek::Signer;
use tokio::time::timeout;

use super::{
    client::{Client, TestServer},
    person::Person as TestPerson,
};

/// How long `quiet` waits for a frame that shouldn't come.
const QUIET: Duration = Duration::from_millis(300);

/// A random 16-byte id, for chats and messages.
pub fn random_id() -> B64<16> {
    B64(rand::random())
}

/// The `new_chat` of a first message from `sender` to `to`: random sealed
/// keys, signed with the sender's key.
pub fn new_chat(sender: &TestPerson, chat_id: &B64<16>, to: Number) -> NewChat {
    let (my_key, their_key) = (rand::random::<[u8; 104]>().to_vec(), rand::random::<[u8; 104]>().to_vec());
    let proof = chat_proof(&chat_id.0, sender.number, &my_key, to, &their_key);
    NewChat {
        number: to,
        my_key: Blob(my_key),
        their_key: Blob(their_key),
        signature: B64(sender.keys.sign(&proof).to_bytes()),
    }
}

/// Starts a chat from `from` to `to` with one message; returns its id.
pub async fn first_message(client: &mut Client, from: &TestPerson, to: Number) -> B64<16> {
    let chat_id = random_id();
    match client.send_body(&chat_id, b"hello".to_vec(), Some(new_chat(from, &chat_id, to))).await {
        ServerFrame::Sent { seq: 1, .. } => chat_id,
        other => panic!("expected the first message sent, got {other:?}"),
    }
}

/// What a `sync` brought, up to and including `synced`.
#[derive(Debug, Default)]
pub struct Synced {
    pub snapshot: Option<Snapshot>,
    pub histories: Vec<(B64<16>, Vec<Message>, bool)>,
    pub events: Vec<Event>,
    pub cursor: i64,
}

impl Client {
    /// Sends one message with a fresh `msg_id`; the answer is `sent` or `not_sent`.
    pub async fn send_body(&mut self, chat_id: &B64<16>, body: Vec<u8>, new_chat: Option<NewChat>) -> ServerFrame {
        self.send_with_id(chat_id, &random_id(), body, new_chat).await
    }

    /// Sends one message with the given `msg_id`.
    pub async fn send_with_id(
        &mut self,
        chat_id: &B64<16>,
        msg_id: &B64<16>,
        body: Vec<u8>,
        new_chat: Option<NewChat>,
    ) -> ServerFrame {
        self.send(&ClientFrame::Send { chat_id: chat_id.clone(), msg_id: msg_id.clone(), body: Body(body), new_chat })
            .await;
        self.recv().await
    }

    /// Sends `sync` and gathers everything up to `synced`.
    pub async fn sync_from(&mut self, cursor: Option<i64>) -> Synced {
        self.send(&ClientFrame::Sync { cursor }).await;
        let mut synced = Synced::default();
        loop {
            match self.recv_raw().await {
                ServerFrame::Snapshot { snapshot } => synced.snapshot = Some(snapshot),
                ServerFrame::History { chat_id, messages, complete } => {
                    synced.histories.push((chat_id, messages, complete))
                }
                ServerFrame::Events { events, .. } => synced.events.extend(events),
                ServerFrame::Synced { cursor } => {
                    synced.cursor = cursor;
                    return synced;
                }
                ServerFrame::Presence { number, online } => self.presence.push_back((number, online)),
                other => panic!("expected sync frames, got {other:?}"),
            }
        }
    }

    /// The next pushed event. Panics on any other frame, or after 5 s.
    pub async fn next_event(&mut self) -> Event {
        loop {
            if let Some(event) = self.events.pop_front() {
                return event;
            }
            let frame = self.recv_raw().await;
            if !self.keep(&frame) {
                panic!("expected an event, got {frame:?}");
            }
        }
    }

    /// The next pushed presence change. Panics on any other frame, or after 5 s.
    pub async fn next_presence(&mut self) -> (Number, bool) {
        loop {
            if let Some(presence) = self.presence.pop_front() {
                return presence;
            }
            let frame = self.recv_raw().await;
            if !self.keep(&frame) {
                panic!("expected presence, got {frame:?}");
            }
        }
    }

    /// Reads pushed frames until 300 ms pass without one, and forgets them.
    pub async fn drain(&mut self) {
        while !self.quiet().await {
            self.events.clear();
            self.presence.clear();
        }
    }

    /// The next pushed `person` event about `number`, skipping other events.
    pub async fn next_person(&mut self, number: Number) -> Person {
        loop {
            if let EventBody::Person { person } = self.next_event().await.event
                && person.number == number
            {
                return person;
            }
        }
    }

    /// Whether no event or presence arrives within 300 ms. Only for checking
    /// that something doesn't happen.
    pub async fn quiet(&mut self) -> bool {
        if !self.events.is_empty() || !self.presence.is_empty() {
            return false;
        }
        match timeout(QUIET, self.recv_raw()).await {
            Err(_) => true,
            Ok(frame) => {
                self.keep(&frame);
                false
            }
        }
    }
}

/// Two active people with one synced connection each.
pub async fn strangers(server: &TestServer) -> (TestPerson, Client, TestPerson, Client) {
    let (a, mut a_client) = TestPerson::active(server, "abin").await;
    let (b, mut b_client) = TestPerson::active(server, "rahul").await;
    a_client.sync_from(Some(0)).await;
    b_client.sync_from(Some(0)).await;
    (a, a_client, b, b_client)
}

/// Like `strangers`, befriended in the database.
pub async fn contacts(server: &TestServer) -> (TestPerson, Client, TestPerson, Client) {
    let (a, a_client, b, b_client) = strangers(server).await;
    let (a_id, b_id, now) = (a.id(server).await, b.id(server).await, server.state.clock.now());
    server.state.db.call(move |c| relations::befriend(c, a_id, b_id, now).unwrap()).await;
    (a, a_client, b, b_client)
}
