//! What someone does about another person: accept, reject, block, unblock,
//! remove. Each changes only their own row, except an accept, which the
//! other side asked for.

use corvo_chat_proto::{Number, chat::Relation, frames::ErrorCode};
use rusqlite::Connection;

use super::{Conn, Flow, Link, chat::Touched};
use crate::{accounts, chats, events::Ev, hub::Hub, relations};

/// The five changes, as their frames ask.
#[derive(Clone, Copy)]
pub(super) enum Change {
    Accept,
    Reject,
    Block,
    Unblock,
    Remove { clear_history: bool },
}

/// What the presence of the two should do after a change.
enum Seen {
    Unchanged,
    /// They're mutual contacts now: each sees the other's state.
    Shown,
    /// They were mutual contacts and aren't now: each sees the other offline.
    Hidden,
}

/// A refusal's code and message.
type Refusal = (ErrorCode, &'static str);

/// Tells each of the two whether the other is online.
pub(super) fn announce_pair(hub: &Hub, (a, a_number): (i64, Number), (b, b_number): (i64, Number), visible: bool) {
    hub.announce(a, a_number, &[b], visible);
    hub.announce(b, b_number, &[a], visible);
}

/// Clears `me`'s side of the chat with `other`, if there is one.
fn clear_side(c: &Connection, me: i64, other: i64, touched: &mut Touched) -> rusqlite::Result<()> {
    if let Some(chat) = chats::between(c, me, other)? {
        chats::clear(c, &chat.id, me)?;
        touched.append(c, me, &Ev::Cleared { chat: chat.id })?;
    }
    Ok(())
}

fn change(
    c: &Connection,
    me: i64,
    other: i64,
    what: Change,
    now: i64,
    touched: &mut Touched,
) -> rusqlite::Result<Result<Seen, Refusal>> {
    let mine = relations::get(c, me, other)?;
    let mutual = mine == Relation::Contact && relations::get(c, other, me)? == Relation::Contact;
    let seen = match what {
        Change::Accept => {
            if mine != Relation::Request {
                return Ok(Err((ErrorCode::NotAllowed, "No request from that number.")));
            }
            relations::set(c, me, other, Relation::Contact, now)?;
            // Someone who removed you since doesn't become your contact again.
            if relations::get(c, other, me)? == Relation::Asked {
                relations::set(c, other, me, Relation::Contact, now)?;
            }
            touched.append(c, other, &Ev::Person { other: me })?;
            if let Some(chat) = chats::between(c, me, other)? {
                touched.append(c, other, &Ev::Marks { chat: chat.id })?;
            }
            if relations::get(c, other, me)? == Relation::Contact { Seen::Shown } else { Seen::Unchanged }
        }
        Change::Reject => {
            if mine != Relation::Request {
                return Ok(Err((ErrorCode::NotAllowed, "No request from that number.")));
            }
            relations::set(c, me, other, Relation::None, now)?;
            clear_side(c, me, other, touched)?;
            Seen::Unchanged
        }
        Change::Block => {
            if mine == Relation::Blocked {
                return Ok(Err((ErrorCode::NotAllowed, "Already blocked.")));
            }
            relations::set(c, me, other, Relation::Blocked, now)?;
            if mutual { Seen::Hidden } else { Seen::Unchanged }
        }
        Change::Unblock => {
            if mine != Relation::Blocked {
                return Ok(Err((ErrorCode::NotAllowed, "They aren't blocked.")));
            }
            relations::set(c, me, other, Relation::None, now)?;
            Seen::Unchanged
        }
        Change::Remove { clear_history } => {
            if !matches!(mine, Relation::Contact | Relation::Asked) {
                return Ok(Err((ErrorCode::NotAllowed, "They aren't in your contacts.")));
            }
            relations::set(c, me, other, Relation::None, now)?;
            if clear_history {
                clear_side(c, me, other, touched)?;
            }
            if mutual { Seen::Hidden } else { Seen::Unchanged }
        }
    };
    touched.append(c, me, &Ev::Person { other })?;
    Ok(Ok(seen))
}

impl Link {
    pub(super) async fn relate(&mut self, number: Number, what: Change) -> Flow {
        let Conn::SignedIn { account_id: me, .. } = self.conn else {
            return self.error(ErrorCode::NotSignedIn, "sign in first").await;
        };
        let (now, hub) = (self.state.clock.now(), self.state.hub.clone());
        let done = self
            .state
            .db
            .call(move |c| -> rusqlite::Result<Result<(), Refusal>> {
                let other = accounts::active_by_number(c, number, now)?.filter(|row| row.id != me);
                let Some(other) = other else {
                    return Ok(Err((ErrorCode::NumberUnknown, "That number isn't on corvo-chat.")));
                };
                let mine = accounts::by_id(c, me, now)?.ok_or(rusqlite::Error::QueryReturnedNoRows)?;
                let mut touched = Touched::default();
                let tx = c.transaction()?;
                let seen = match change(&tx, me, other.id, what, now, &mut touched)? {
                    Ok(seen) => seen,
                    Err(refusal) => return Ok(Err(refusal)),
                };
                tx.commit()?;
                touched.poke(&hub);
                let pair = ((me, mine.number), (other.id, other.number));
                match seen {
                    Seen::Shown => announce_pair(&hub, pair.0, pair.1, true),
                    Seen::Hidden => announce_pair(&hub, pair.0, pair.1, false),
                    Seen::Unchanged => {}
                }
                Ok(Ok(()))
            })
            .await;
        match done {
            Ok(Ok(())) => Flow::Stay,
            Ok(Err((code, message))) => self.error(code, message).await,
            Err(err) => self.internal("changing a relation", err).await,
        }
    }
}

/// An invite was used: the inviter and the new account become contacts and
/// see each other online. Runs inside the activation's `Db::call`.
pub(super) fn befriend_on_activation(
    c: &Connection,
    hub: &Hub,
    me: (i64, Number),
    inviter: i64,
    now: i64,
) -> rusqlite::Result<()> {
    let Some(inviter_row) = accounts::by_id(c, inviter, now)? else { return Ok(()) };
    relations::befriend(c, me.0, inviter, now)?;
    let mut touched = Touched::default();
    touched.append(c, me.0, &Ev::Person { other: inviter })?;
    touched.append(c, inviter, &Ev::Person { other: me.0 })?;
    touched.poke(hub);
    announce_pair(hub, me, (inviter, inviter_row.number), true);
    Ok(())
}
