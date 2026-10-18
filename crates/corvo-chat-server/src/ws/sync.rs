//! Bringing a device up to date: the account's event log from its cursor,
//! each event rendered from the current state, then live events as they're
//! appended.

use corvo_chat_proto::{
    B64, Blob, Body, Number,
    chat::{ChatInfo, Event, EventBody, Marks, Message, Person, Pin, SealedKey, Snapshot},
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
    // Only now that marks are shared may the viewer learn which of its own
    // messages never arrived, so they stay `·`.
    let withheld = chats::withheld_ranges(c, &chat.id, viewer)?;
    Ok(chats::member(c, &chat.id, other)?.map(|m| Marks {
        received_through: m.received_through,
        read_through: m.read_through,
        withheld,
    }))
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
    let display_name = (!row.deleted && relations::shows_name(c, other, viewer)?).then_some(row.display_name);
    Ok(Some(Person {
        number: row.number,
        display_name,
        public_keys: accounts::public_keys(c, other)?,
        relation: relations::get(c, viewer, other)?,
        deleted: row.deleted,
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

/// What a new device starts from: the snapshot, and each chat's id and last
/// seq, to page its latest messages from one call at a time.
struct Initial {
    snapshot: Snapshot,
    chats: Vec<([u8; 16], i64)>,
}

/// Stored messages as frames, dropping any whose sender is gone.
fn message_frames(
    c: &Connection,
    chat_id: &[u8; 16],
    rows: Vec<MessageRow>,
    now: i64,
) -> rusqlite::Result<Vec<Message>> {
    let mut messages = Vec::with_capacity(rows.len());
    for row in rows {
        messages.extend(message_frame(c, chat_id, row, now)?);
    }
    Ok(messages)
}

fn initial(c: &Connection, viewer: Viewer) -> rusqlite::Result<Option<Initial>> {
    let (me, now) = (viewer.account, viewer.now);
    let Some(account) = accounts::by_id(c, me, now)? else { return Ok(None) };
    let cursor = events::head(c, me)?;
    let announced = chats::announced_for(c, me)?;
    // Everyone with a row, and the other side of every chat without one.
    let mut others: Vec<i64> = relations::rows(c, me)?.into_iter().map(|(other, _)| other).collect();
    for chat in &announced {
        let other = chat.other(me);
        if !others.contains(&other) {
            others.push(other);
        }
    }
    let mut people = Vec::new();
    for other in others {
        people.extend(person(c, me, other, now)?);
    }
    let mut chat_infos = Vec::new();
    for chat in &announced {
        chat_infos.extend(chat_info(c, me, chat, now)?);
    }
    let pins = pins::all(c, me)?.into_iter().map(|(number, pin)| Pin { number, pin: Some(Blob(pin)) }).collect();
    let snapshot = Snapshot { cursor, account: account.to_frame(), people, chats: chat_infos, pins };
    let chats = announced.iter().map(|chat| (chat.id, chat.last_seq)).collect();
    Ok(Some(Initial { snapshot, chats }))
}

/// Up to `limit` messages `viewer` sees before `before`, oldest first, cut
/// from the oldest end once their bodies reach [`PAGE_BYTES`]; and whether
/// nothing older is visible. `None` if the chat isn't the viewer's.
fn history_page(
    c: &Connection,
    viewer: Viewer,
    chat: &[u8; 16],
    before: i64,
    limit: usize,
) -> rusqlite::Result<Option<(Vec<Message>, bool)>> {
    if !chats::member(c, chat, viewer.account)?.is_some_and(|m| m.announced) {
        return Ok(None);
    }
    let (mut rows, mut complete) = chats::visible(c, chat, viewer.account, before, limit)?;
    let mut bytes = 0;
    let keep = rows
        .iter()
        .rev()
        .take_while(|row| {
            let under = bytes < PAGE_BYTES;
            bytes += row.body.len();
            under
        })
        .count();
    if keep < rows.len() {
        rows.drain(..rows.len() - keep);
        complete = false;
    }
    Ok(Some((message_frames(c, chat, rows, viewer.now)?, complete)))
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
        // A new device, or a cursor this log never reached (a restored
        // database): start over from a snapshot.
        let from = match cursor {
            Some(cursor) if (0..=head).contains(&cursor) => cursor,
            _ => return self.send_snapshot(viewer).await,
        };
        if let (Flow::Close, _) = self.send_events(from, Some(head)).await {
            return Flow::Close;
        }
        self.synced(head).await
    }

    async fn send_snapshot(&mut self, viewer: Viewer) -> Flow {
        let per_chat = self.state.limits.snapshot_messages;
        let Initial { snapshot, chats } = match self.state.db.call(move |c| initial(c, viewer)).await {
            Ok(Some(initial)) => initial,
            Ok(None) => return self.internal("making a snapshot", "the account is gone").await,
            Err(err) => return self.internal("making a snapshot", err).await,
        };
        let cursor = snapshot.cursor;
        if let Flow::Close = self.send(&ServerFrame::Snapshot { snapshot }).await {
            return Flow::Close;
        }
        // One chat per call, so a big snapshot never holds the database for long.
        for (chat, last_seq) in chats {
            let page = self.state.db.call(move |c| history_page(c, viewer, &chat, last_seq + 1, per_chat)).await;
            let (messages, complete) = match page {
                Ok(Some(page)) => page,
                Ok(None) => continue,
                Err(err) => return self.internal("reading a chat for a snapshot", err).await,
            };
            let chat_id = B64(chat);
            if let Flow::Close = self.send(&ServerFrame::History { chat_id, messages, complete }).await {
                return Flow::Close;
            }
        }
        self.synced(cursor).await
    }

    /// The device is at `cursor`: say so, list the contacts online, then
    /// send whatever was appended meanwhile.
    async fn synced(&mut self, cursor: i64) -> Flow {
        if let Flow::Close = self.send(&ServerFrame::Synced { cursor }).await {
            return Flow::Close;
        }
        self.cursor = Some(cursor);
        let Some(viewer) = self.viewer() else { return Flow::Stay };
        let hub = self.state.hub.clone();
        let online = self
            .state
            .db
            .call(move |c| -> rusqlite::Result<Vec<Number>> {
                let mut online = Vec::new();
                for contact in relations::mutual_contacts(c, viewer.account)? {
                    if hub.online(contact) {
                        online.extend(number(c, contact, viewer.now)?);
                    }
                }
                Ok(online)
            })
            .await;
        match online {
            Ok(numbers) => {
                for number in numbers {
                    if let Flow::Close = self.send(&ServerFrame::Presence { number, online: true }).await {
                        return Flow::Close;
                    }
                }
            }
            Err(err) => return self.internal("listing contacts online", err).await,
        }
        self.flush().await
    }

    /// A page of older messages, before `before`, at most `snapshot_messages`.
    pub(super) async fn history(&mut self, chat_id: B64<16>, before: i64, limit: u32) -> Flow {
        let Some(viewer) = self.viewer() else {
            return self.error(ErrorCode::NotSignedIn, "sign in first").await;
        };
        let limit = (limit as usize).clamp(1, self.state.limits.snapshot_messages);
        let chat = chat_id.0;
        let page = self.state.db.call(move |c| history_page(c, viewer, &chat, before, limit)).await;
        match page {
            Ok(Some((messages, complete))) => self.send(&ServerFrame::History { chat_id, messages, complete }).await,
            Ok(None) => self.error(ErrorCode::ChatUnknown, "No such chat.").await,
            Err(err) => self.internal("reading history", err).await,
        }
    }

    /// Sends the events after the device's cursor and moves it.
    pub(super) async fn flush(&mut self) -> Flow {
        let Some(from) = self.cursor else { return Flow::Stay };
        let (flow, reached) = self.send_events(from, None).await;
        self.cursor = Some(reached);
        flow
    }
}
