//! Bringing a device up to date: the account's event log from its cursor,
//! each event rendered from the current state, then live events as they're
//! appended.

use corvo_chat_proto::{
    B64, Blob, Body, Number,
    chat::{ChatInfo, Event, EventBody, Marks, Message, Person, Pin, SealedKey},
    frames::{ErrorCode, ServerFrame},
};
use rusqlite::Connection;

use super::{Conn, Flow, Link, devices::infos};
use crate::{
    accounts,
    chats::{self, ChatRow, MessageRow},
    events::{self, Ev},
    pins, relations,
};

/// The most events in one `events` frame.
pub(super) const EVENT_PAGE: usize = 100;
/// A frame stops taking events once their message bodies reach this.
pub(super) const PAGE_BYTES: usize = 512 * 1024;

/// Whose eyes an event is rendered for.
#[derive(Clone, Copy)]
pub(super) struct Viewer {
    pub account: i64,
    pub session: i64,
    pub now: i64,
}

fn number(c: &Connection, account: i64, now: i64) -> rusqlite::Result<Option<Number>> {
    Ok(accounts::by_id(c, account, now)?.map(|row| row.number))
}

/// A stored message as a frame, or `None` if its sender is gone.
pub(super) fn message_frame(
    c: &Connection,
    chat_id: &[u8; 16],
    row: MessageRow,
    now: i64,
) -> rusqlite::Result<Option<Message>> {
    let Some(from) = number(c, row.sender_id, now)? else { return Ok(None) };
    Ok(Some(Message {
        chat_id: B64(*chat_id),
        seq: row.seq,
        msg_id: B64(row.msg_id),
        from,
        body: Body(row.body),
        sent_at: row.sent_at,
    }))
}

/// The other side's marks, if they show them to `viewer`.
fn marks(c: &Connection, chat: &ChatRow, viewer: i64) -> rusqlite::Result<Option<Marks>> {
    let other = chat.other(viewer);
    if !relations::shows_marks(c, other, viewer)? {
        return Ok(None);
    }
    Ok(chats::member(c, &chat.id, other)?
        .map(|m| Marks { received_through: m.received_through, read_through: m.read_through }))
}

/// A chat as `viewer` sees it, or `None` if a member is gone.
pub(super) fn chat_info(c: &Connection, viewer: i64, chat: &ChatRow, now: i64) -> rusqlite::Result<Option<ChatInfo>> {
    let creator = chat.creator;
    let members = [creator, chat.other(creator)];
    let mut keys = Vec::with_capacity(2);
    for account in members {
        let (Some(number), Some(member)) = (number(c, account, now)?, chats::member(c, &chat.id, account)?) else {
            return Ok(None);
        };
        keys.push(SealedKey { number, key: Blob(member.sealed_key) });
    }
    let Some(me) = chats::member(c, &chat.id, viewer)? else { return Ok(None) };
    let with = keys.iter().zip(members).find(|(_, account)| *account != viewer).map(|(key, _)| key.number);
    let Some(with) = with else { return Ok(None) };
    Ok(Some(ChatInfo {
        chat_id: B64(chat.id),
        with,
        keys,
        signature: B64(chat.signature),
        last_seq: chat.last_seq,
        read_through: me.read_through,
        cleared_through: me.cleared_through,
        marks: marks(c, chat, viewer)?,
    }))
}

/// Someone as `viewer` sees them, or `None` if they're gone.
pub(super) fn person(c: &Connection, viewer: i64, other: i64, now: i64) -> rusqlite::Result<Option<Person>> {
    let Some(row) = accounts::by_id(c, other, now)? else { return Ok(None) };
    let display_name = relations::shows_name(c, other, viewer)?.then_some(row.display_name);
    Ok(Some(Person {
        number: row.number,
        display_name,
        public_keys: accounts::public_keys(c, other)?,
        relation: relations::get(c, viewer, other)?,
    }))
}

/// `ev` as `viewer` would see it now, or `None` if there's nothing to show.
pub(super) fn render(c: &Connection, viewer: Viewer, ev: &Ev) -> rusqlite::Result<Option<EventBody>> {
    let me = viewer.account;
    let announced = |chat: &[u8; 16]| -> rusqlite::Result<Option<(ChatRow, chats::MemberRow)>> {
        let (Some(row), Some(member)) = (chats::get(c, chat)?, chats::member(c, chat, me)?) else { return Ok(None) };
        Ok(member.announced.then_some((row, member)))
    };
    Ok(match ev {
        Ev::Message { chat, seq } => {
            let Some((_, member)) = announced(chat)? else { return Ok(None) };
            let Some(row) = chats::message(c, chat, *seq)? else { return Ok(None) };
            if !row.visible_to(me, member.cleared_through) {
                return Ok(None);
            }
            message_frame(c, chat, row, viewer.now)?.map(|message| EventBody::Message { message })
        }
        Ev::Chat { chat } => {
            let Some((row, _)) = announced(chat)? else { return Ok(None) };
            chat_info(c, me, &row, viewer.now)?.map(|chat| EventBody::Chat { chat })
        }
        Ev::Person { other } => person(c, me, *other, viewer.now)?.map(|person| EventBody::Person { person }),
        Ev::Marks { chat } => {
            let Some((row, _)) = announced(chat)? else { return Ok(None) };
            marks(c, &row, me)?.map(|marks| EventBody::Marks { chat_id: B64(*chat), marks })
        }
        Ev::Read { chat } => announced(chat)?
            .map(|(_, member)| EventBody::Read { chat_id: B64(*chat), read_through: member.read_through }),
        Ev::Cleared { chat } => announced(chat)?
            .map(|(_, member)| EventBody::Cleared { chat_id: B64(*chat), cleared_through: member.cleared_through }),
        Ev::Profile => accounts::by_id(c, me, viewer.now)?.map(|row| EventBody::Profile { account: row.to_frame() }),
        Ev::Devices => Some(EventBody::Devices { devices: infos(c, me, viewer.session)? }),
        Ev::Pin { number } => {
            let pin = pins::get(c, me, *number)?.map(Blob);
            Some(EventBody::Pin { pin: Pin { number: *number, pin } })
        }
    })
}

/// One page of the log after `after`, up to `upto` if given: the shown
/// events and the last seq covered, or `None` when there's nothing more.
fn page(c: &Connection, viewer: Viewer, after: i64, upto: Option<i64>) -> rusqlite::Result<Option<(Vec<Event>, i64)>> {
    let mut shown = Vec::new();
    let (mut last, mut bytes) = (None, 0);
    for (seq, ev) in events::since(c, viewer.account, after, EVENT_PAGE)? {
        if upto.is_some_and(|upto| seq > upto) {
            break;
        }
        last = Some(seq);
        if let Some(event) = render(c, viewer, &ev)? {
            if let EventBody::Message { message } = &event {
                bytes += message.body.0.len();
            }
            shown.push(Event { seq, event });
        }
        if bytes >= PAGE_BYTES {
            break;
        }
    }
    Ok(last.map(|last| (shown, last)))
}

impl Link {
    fn viewer(&self) -> Option<Viewer> {
        match self.conn {
            Conn::SignedIn { session, account_id, .. } => {
                Some(Viewer { account: account_id, session, now: self.state.clock.now() })
            }
            _ => None,
        }
    }

    /// Sends the log after `from` (up to `upto`) as `events` frames; returns
    /// how the connection goes on and the last seq sent.
    async fn send_events(&mut self, from: i64, upto: Option<i64>) -> (Flow, i64) {
        let Some(viewer) = self.viewer() else { return (Flow::Stay, from) };
        let mut cursor = from;
        loop {
            let after = cursor;
            match self.state.db.call(move |c| page(c, viewer, after, upto)).await {
                Ok(None) => return (Flow::Stay, cursor),
                Ok(Some((events, last))) => {
                    cursor = last;
                    if let Flow::Close = self.send(&ServerFrame::Events { events, cursor }).await {
                        return (Flow::Close, cursor);
                    }
                }
                Err(err) => return (self.internal("reading events", err).await, cursor),
            }
        }
    }

    /// `sync`: the log from the device's cursor up to now, then `synced`,
    /// then live events.
    pub(super) async fn sync(&mut self, cursor: Option<i64>) -> Flow {
        let Some(viewer) = self.viewer() else {
            return self.error(ErrorCode::NotSignedIn, "sign in first").await;
        };
        let head = match self.state.db.call(move |c| events::head(c, viewer.account)).await {
            Ok(head) => head,
            Err(err) => return self.internal("reading the log head", err).await,
        };
        let from = match cursor {
            Some(cursor) if (0..=head).contains(&cursor) => cursor,
            _ => return self.error(ErrorCode::BadFrame, "a snapshot isn't supported yet").await,
        };
        if let (Flow::Close, _) = self.send_events(from, Some(head)).await {
            return Flow::Close;
        }
        if let Flow::Close = self.send(&ServerFrame::Synced { cursor: head }).await {
            return Flow::Close;
        }
        self.cursor = Some(head);
        // Whatever was appended while this ran.
        self.flush().await
    }

    /// Sends the events after the device's cursor and moves it.
    pub(super) async fn flush(&mut self) -> Flow {
        let Some(from) = self.cursor else { return Flow::Stay };
        let (flow, reached) = self.send_events(from, None).await;
        self.cursor = Some(reached);
        flow
    }
}
