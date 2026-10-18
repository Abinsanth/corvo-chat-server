//! Looking a number up, and sending messages: new chats, requests,
//! withheld messages and the message limits.

use corvo_chat_proto::{
    B64, Body, Number, PublicKeys,
    chat::{NewChat, Relation},
    chat_proof,
    frames::{ErrorCode, ServerFrame},
};
use rusqlite::Connection;

use super::{
    Conn, Flow, Link,
    people::announce_pair,
    sign_in::{BANNED, HOUR},
};
use crate::{
    accounts, attempts,
    chats::{self, Added, ChatError},
    events::{self, Ev},
    hub::{Hub, Push},
    limits::Limits,
    relations,
};

const MINUTE: i64 = 60;
const DAY: i64 = 86_400;

/// Why a message wasn't stored: the code and message of its `not_sent`.
struct Refused(ErrorCode, String);

fn refused<T>(code: ErrorCode, message: impl Into<String>) -> rusqlite::Result<Result<T, Refused>> {
    Ok(Err(Refused(code, message.into())))
}

/// How a lookup ended.
enum Looked {
    Found(Number, PublicKeys),
    Unknown,
    Banned,
    Limit,
}

/// Accounts whose logs got events, and pairs who now see each other online,
/// to tell once the writes are in.
#[derive(Default)]
pub(super) struct Touched {
    accounts: Vec<i64>,
    shown: Vec<((i64, Number), (i64, Number))>,
}

impl Touched {
    pub(super) fn append(&mut self, c: &Connection, account: i64, ev: &Ev) -> rusqlite::Result<()> {
        events::append(c, account, ev)?;
        self.accounts.push(account);
        Ok(())
    }

    /// The two became mutual contacts: each is to see the other online.
    pub(super) fn show(&mut self, a: (i64, Number), b: (i64, Number)) {
        self.shown.push((a, b));
    }

    /// Pokes every account that got an event, once each, and announces the
    /// pairs that now see each other.
    pub(super) fn poke(mut self, hub: &Hub) {
        self.accounts.sort_unstable();
        self.accounts.dedup();
        for account in self.accounts {
            hub.push(account, Push::Events);
        }
        for (a, b) in self.shown {
            announce_pair(hub, a, b, true);
        }
    }
}

/// Counts one number lookup for `me`: `false` when the hour's are used up.
/// Every frame that finds out whether a number exists pays one, so the
/// lookup limit can't be got around.
pub(super) fn charge_lookup(c: &Connection, me: i64, now: i64, limits: &Limits) -> rusqlite::Result<bool> {
    let key = me.to_string();
    if attempts::count_since(c, "lookup", &key, now - HOUR + 1)? >= i64::from(limits.lookups_per_hour) {
        return Ok(false);
    }
    attempts::note(c, "lookup", &key, now)?;
    Ok(true)
}

/// The refusal once the lookups are used up.
pub(super) const LOOKUP_LIMIT: &str = "Too many lookups. Wait an hour.";

/// What one `send` carries.
struct Outgoing {
    chat_id: [u8; 16],
    msg_id: [u8; 16],
    body: Vec<u8>,
    new_chat: Option<NewChat>,
}

/// Finds the chat, creating it for a first message.
fn open_chat(
    c: &Connection,
    me: i64,
    out: &Outgoing,
    now: i64,
    touched: &mut Touched,
) -> rusqlite::Result<Result<chats::ChatRow, Refused>> {
    if let Some(chat) = chats::get(c, &out.chat_id)? {
        if chats::member(c, &out.chat_id, me)?.is_none() {
            return refused(ErrorCode::ChatUnknown, "No such chat.");
        }
        return Ok(Ok(chat));
    }
    let Some(new) = &out.new_chat else { return refused(ErrorCode::ChatUnknown, "No such chat.") };
    let other = accounts::active_by_number(c, new.number, now)?.filter(|row| row.id != me);
    let Some(other) = other else { return refused(ErrorCode::NumberUnknown, "That number isn't on corvo-chat.") };
    if other.banned {
        return refused(ErrorCode::Banned, BANNED);
    }
    let mine = accounts::by_id(c, me, now)?.ok_or(rusqlite::Error::QueryReturnedNoRows)?;
    let proof = chat_proof(&out.chat_id, mine.number, &new.my_key.0, other.number, &new.their_key.0);
    if !accounts::public_keys(c, me)?.verifies(&proof, &new.signature) {
        return refused(ErrorCode::BadFrame, "The chat signature doesn't check out.");
    }
    if let Some(existing) = chats::between(c, me, other.id)? {
        // The sender wants this chat, so it may see it even if it was withheld.
        if chats::announce(c, &existing.id, me)? {
            touched.append(c, me, &Ev::Chat { chat: existing.id })?;
        }
        return refused(ErrorCode::ChatExists, "You already have a chat with them.");
    }
    match chats::create(c, &out.chat_id, me, other.id, &new.my_key.0, &new.their_key.0, &new.signature.0, now) {
        Ok(()) => {}
        Err(ChatError::IdTaken | ChatError::PairTaken) => {
            return refused(ErrorCode::BadFrame, "That chat id is taken.");
        }
        Err(ChatError::Db(err)) => return Err(err),
    }
    chats::announce(c, &out.chat_id, me)?;
    touched.append(c, me, &Ev::Chat { chat: out.chat_id })?;
    Ok(Ok(chats::get(c, &out.chat_id)?.ok_or(rusqlite::Error::QueryReturnedNoRows)?))
}

/// Stores one message from `me`, with every check and event of a `send`.
fn store(
    c: &Connection,
    me: i64,
    out: &Outgoing,
    now: i64,
    limits: &Limits,
    touched: &mut Touched,
) -> rusqlite::Result<Result<Added, Refused>> {
    let chat = match open_chat(c, me, out, now, touched)? {
        Ok(chat) => chat,
        Err(refusal) => return Ok(Err(refusal)),
    };
    let other = chat.other(me);
    if accounts::by_id(c, other, now)?.is_some_and(|row| row.banned) {
        return refused(ErrorCode::Banned, BANNED);
    }
    // A resend after a reconnect: the first one stands, and counts once.
    if let Some(known) = chats::by_msg_id(c, &chat.id, &out.msg_id)? {
        return Ok(Ok(known));
    }
    let key = me.to_string();
    if attempts::count_since(c, "message", &key, now - MINUTE + 1)? >= i64::from(limits.messages_per_minute) {
        return refused(ErrorCode::RateLimited, "Too many messages. Slow down.");
    }
    match relations::get(c, me, other)? {
        Relation::Request => return refused(ErrorCode::NotAllowed, "Accept the request first."),
        Relation::Blocked => return refused(ErrorCode::NotAllowed, "Unblock them first."),
        Relation::None => {
            if attempts::count_since(c, "new_person", &key, now - DAY + 1)? >= i64::from(limits.new_people_per_day) {
                let message = format!("You've messaged {} new people today.", limits.new_people_per_day);
                return refused(ErrorCode::RateLimited, message);
            }
            relations::set(c, me, other, Relation::Asked, now)?;
            attempts::note(c, "new_person", &key, now)?;
            touched.append(c, me, &Ev::Person { other })?;
        }
        Relation::Asked | Relation::Contact => {}
    }
    let mut new_request = false;
    let shown = match relations::get(c, other, me)? {
        Relation::Blocked => false,
        Relation::Contact => true,
        // Each asked the other: that's agreement.
        // Each asked the other: that's agreement, with all an accept brings.
        Relation::Asked => {
            relations::befriend(c, me, other, now)?;
            touched.append(c, me, &Ev::Person { other })?;
            touched.append(c, other, &Ev::Person { other: me })?;
            touched.append(c, me, &Ev::Marks { chat: chat.id })?;
            touched.append(c, other, &Ev::Marks { chat: chat.id })?;
            let number = |id| -> rusqlite::Result<Number> {
                Ok(accounts::by_id(c, id, now)?.ok_or(rusqlite::Error::QueryReturnedNoRows)?.number)
            };
            touched.show((me, number(me)?), (other, number(other)?));
            true
        }
        Relation::Request => chats::request_messages(c, &chat.id, me, other)? < limits.request_messages,
        Relation::None => {
            let room = relations::open_requests(c, other)? < limits.open_requests;
            if room {
                relations::set(c, other, me, Relation::Request, now)?;
                touched.append(c, other, &Ev::Person { other: me })?;
            }
            new_request = room;
            room
        }
    };
    let added = chats::add_message(c, &chat.id, &out.msg_id, me, &out.body, now, !shown)?;
    if new_request {
        // The request's 20 messages count from here, not from older history.
        chats::start_request(c, &chat.id, other, added.seq)?;
    }
    attempts::note(c, "message", &key, now)?;
    let ev = Ev::Message { chat: chat.id, seq: added.seq };
    touched.append(c, me, &ev)?;
    if shown {
        if chats::announce(c, &chat.id, other)? {
            touched.append(c, other, &Ev::Chat { chat: chat.id })?;
        }
        touched.append(c, other, &ev)?;
    }
    Ok(Ok(added))
}

impl Link {
    pub(super) async fn lookup(&mut self, number: Number) -> Flow {
        let Conn::SignedIn { account_id, .. } = self.conn else {
            return self.error(ErrorCode::NotSignedIn, "sign in first").await;
        };
        let (now, limits) = (self.state.clock.now(), self.state.limits.clone());
        let looked = self
            .state
            .db
            .call(move |c| -> rusqlite::Result<Looked> {
                if !charge_lookup(c, account_id, now, &limits)? {
                    return Ok(Looked::Limit);
                }
                match accounts::active_by_number(c, number, now)?.filter(|row| row.id != account_id) {
                    Some(row) if row.banned => Ok(Looked::Banned),
                    Some(row) => Ok(Looked::Found(row.number, accounts::public_keys(c, row.id)?)),
                    None => Ok(Looked::Unknown),
                }
            })
            .await;
        match looked {
            Ok(Looked::Found(number, public_keys)) => self.send(&ServerFrame::Found { number, public_keys }).await,
            Ok(Looked::Unknown) => self.error(ErrorCode::NumberUnknown, "That number isn't on corvo-chat.").await,
            Ok(Looked::Limit) => self.error(ErrorCode::RateLimited, LOOKUP_LIMIT).await,
            Ok(Looked::Banned) => self.error(ErrorCode::Banned, BANNED).await,
            Err(err) => self.internal("looking up a number", err).await,
        }
    }

    pub(super) async fn send_message(
        &mut self,
        chat_id: B64<16>,
        msg_id: B64<16>,
        body: Body,
        new_chat: Option<NewChat>,
    ) -> Flow {
        let Conn::SignedIn { account_id, .. } = self.conn else {
            return self.error(ErrorCode::NotSignedIn, "sign in first").await;
        };
        let (now, limits, hub) = (self.state.clock.now(), self.state.limits.clone(), self.state.hub.clone());
        let out = Outgoing { chat_id: chat_id.0, msg_id: msg_id.0, body: body.0, new_chat };
        let stored = self
            .state
            .db
            .call(move |c| -> rusqlite::Result<Result<Added, Refused>> {
                // A new chat finds out whether a number exists: charged
                // before the transaction, so a refusal can't roll it back.
                if out.new_chat.is_some()
                    && chats::get(c, &out.chat_id)?.is_none()
                    && !charge_lookup(c, account_id, now, &limits)?
                {
                    return refused(ErrorCode::RateLimited, LOOKUP_LIMIT);
                }
                let mut touched = Touched::default();
                let tx = c.transaction()?;
                let stored = store(&tx, account_id, &out, now, &limits, &mut touched)?;
                // A refusal leaves nothing behind, except the announce of an
                // existing chat, which the sender needs.
                if matches!(&stored, Err(Refused(code, _)) if *code != ErrorCode::ChatExists) {
                    return Ok(stored);
                }
                tx.commit()?;
                touched.poke(&hub);
                Ok(stored)
            })
            .await;
        match stored {
            Ok(Ok(Added { seq, sent_at, .. })) => self.send(&ServerFrame::Sent { chat_id, msg_id, seq, sent_at }).await,
            Ok(Err(Refused(code, message))) => {
                self.send(&ServerFrame::NotSent { chat_id, msg_id, code, message }).await
            }
            Err(err) => self.internal("sending a message", err).await,
        }
    }
}

impl Link {
    /// `received` and `read`: move this side's cursor; tell this account's
    /// other devices about a read, and the other side about either if this
    /// side counts them as a contact. No reply.
    pub(super) async fn ack(&mut self, chat_id: B64<16>, through: i64, read: bool) -> Flow {
        let Conn::SignedIn { account_id: me, .. } = self.conn else {
            return self.error(ErrorCode::NotSignedIn, "sign in first").await;
        };
        let hub = self.state.hub.clone();
        let acked = self
            .state
            .db
            .call(move |c| -> rusqlite::Result<bool> {
                let chat = chat_id.0;
                let (Some(row), Some(member)) = (chats::get(c, &chat)?, chats::member(c, &chat, me)?) else {
                    return Ok(false);
                };
                if !member.announced {
                    return Ok(false);
                }
                let mut touched = Touched::default();
                let tx = c.transaction()?;
                let received_before = member.received_through;
                let moved = if read {
                    chats::read(&tx, &chat, me, through)?
                } else {
                    chats::received(&tx, &chat, me, through)?
                };
                let received_moved =
                    chats::member(&tx, &chat, me)?.is_some_and(|m| m.received_through > received_before);
                if read && moved {
                    touched.append(&tx, me, &Ev::Read { chat })?;
                }
                let other = row.other(me);
                if (moved || received_moved) && relations::shows_marks(&tx, me, other)? {
                    touched.append(&tx, other, &Ev::Marks { chat })?;
                }
                tx.commit()?;
                touched.poke(&hub);
                Ok(true)
            })
            .await;
        match acked {
            Ok(true) => Flow::Stay,
            Ok(false) => self.error(ErrorCode::ChatUnknown, "No such chat.").await,
            Err(err) => self.internal("acking", err).await,
        }
    }
}
