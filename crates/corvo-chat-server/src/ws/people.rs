//! What someone does about another person (accept, reject, block, unblock,
//! remove; each changes only their own row, except an accept, which the
//! other side asked for), and about their own profile: display name,
//! device name, pinned keys.

use corvo_chat_proto::{
    Number,
    chat::{Pin, Relation},
    frames::{ErrorCode, ServerFrame},
    names::{check_device_name, check_display_name},
};
use rusqlite::Connection;

use super::{
    Conn, Flow, Link,
    chat::{LOOKUP_LIMIT, Touched, charge_lookup},
    sign_in::HOUR,
};
use crate::{accounts, attempts, chats, events::Ev, hub::Hub, limits::Limits, pins, relations, sessions};

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

/// The window of the daily report limit.
const DAY: i64 = 86_400;

/// The refusal once the hour's changes are used up.
const CHANGE_LIMIT: &str = "Too many changes. Wait an hour.";

/// Counts one change (display name, device name, pin, block, unblock) for
/// `me`: `false` when the hour's are used up.
fn charge_change(c: &Connection, me: i64, now: i64, limits: &Limits) -> rusqlite::Result<bool> {
    let key = me.to_string();
    if attempts::count_since(c, "change", &key, now - HOUR + 1)? >= i64::from(limits.changes_per_hour) {
        return Ok(false);
    }
    attempts::note(c, "change", &key, now)?;
    Ok(true)
}

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
        Change::Accept if accounts::is_deleted(c, other)? => {
            return Ok(Err((ErrorCode::NotAllowed, "That account was deleted.")));
        }
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
        let (now, hub, limits) = (self.state.clock.now(), self.state.hub.clone(), self.state.limits.clone());
        let done = self
            .state
            .db
            .call(move |c| -> rusqlite::Result<Result<(), Refusal>> {
                let other = match accounts::active_by_number(c, number, now)?.filter(|row| row.id != me) {
                    Some(other) => Some(other),
                    None => accounts::known_tombstone(c, me, number, now)?,
                };
                // Finding out whether a number exists costs a lookup, known or not.
                let stranger = match &other {
                    Some(other) => relations::get(c, me, other.id)? == Relation::None,
                    None => true,
                };
                if stranger && !charge_lookup(c, me, now, &limits)? {
                    return Ok(Err((ErrorCode::RateLimited, LOOKUP_LIMIT)));
                }
                let Some(other) = other else {
                    return Ok(Err((ErrorCode::NumberUnknown, "That number isn't on corvo-chat.")));
                };
                if matches!(what, Change::Block | Change::Unblock) && !charge_change(c, me, now, &limits)? {
                    return Ok(Err((ErrorCode::RateLimited, CHANGE_LIMIT)));
                }
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
/// see each other online. Runs inside the activation's transaction; the
/// caller pokes `touched` after the commit.
pub(super) fn befriend_on_activation(
    c: &Connection,
    me: (i64, Number),
    inviter: i64,
    now: i64,
    touched: &mut Touched,
) -> rusqlite::Result<()> {
    let Some(inviter_row) = accounts::by_id(c, inviter, now)? else { return Ok(()) };
    relations::befriend(c, me.0, inviter, now)?;
    touched.append(c, me.0, &Ev::Person { other: inviter })?;
    touched.append(c, inviter, &Ev::Person { other: me.0 })?;
    touched.show(me, (inviter, inviter_row.number));
    Ok(())
}

impl Link {
    /// Runs a profile change for this account inside one transaction, after
    /// the hourly change limit.
    async fn profile_change(
        &mut self,
        what: &'static str,
        change: impl FnOnce(&Connection, i64, &mut Touched) -> rusqlite::Result<Result<(), Refusal>> + Send + 'static,
    ) -> Flow {
        let Conn::SignedIn { account_id: me, .. } = self.conn else {
            return self.error(ErrorCode::NotSignedIn, "sign in first").await;
        };
        let (now, hub, limits) = (self.state.clock.now(), self.state.hub.clone(), self.state.limits.clone());
        let done = self
            .state
            .db
            .call(move |c| -> rusqlite::Result<Result<(), Refusal>> {
                if !charge_change(c, me, now, &limits)? {
                    return Ok(Err((ErrorCode::RateLimited, CHANGE_LIMIT)));
                }
                let mut touched = Touched::default();
                let tx = c.transaction()?;
                if let Err(refusal) = change(&tx, me, &mut touched)? {
                    return Ok(Err(refusal));
                }
                tx.commit()?;
                touched.poke(&hub);
                Ok(Ok(()))
            })
            .await;
        match done {
            Ok(Ok(())) => Flow::Stay,
            Ok(Err((code, message))) => self.error(code, message).await,
            Err(err) => self.internal(what, err).await,
        }
    }

    /// A new display name: this account's devices get `profile`, and
    /// everyone it shows its name to gets `person`, unless they blocked it or
    /// dropped it.
    pub(super) async fn set_display_name(&mut self, display_name: String) -> Flow {
        let name = match check_display_name(&display_name) {
            Ok(name) => name,
            Err(err) => return self.error(ErrorCode::BadDisplayName, &err.to_string()).await,
        };
        self.profile_change("changing the display name", move |c, me, touched| {
            accounts::set_display_name(c, me, &name)?;
            touched.append(c, me, &Ev::Profile)?;
            for (other, relation) in relations::rows(c, me)? {
                let theirs = relations::get(c, other, me)?;
                if matches!(relation, Relation::Asked | Relation::Contact)
                    && matches!(theirs, Relation::Asked | Relation::Request | Relation::Contact)
                {
                    touched.append(c, other, &Ev::Person { other: me })?;
                }
            }
            Ok(Ok(()))
        })
        .await
    }

    /// Renames this device; every device of the account gets `devices`.
    pub(super) async fn rename_device(&mut self, name: String) -> Flow {
        let Conn::SignedIn { session, .. } = self.conn else {
            return self.error(ErrorCode::NotSignedIn, "sign in first").await;
        };
        let name = match check_device_name(&name) {
            Ok(name) => name,
            Err(err) => return self.error(ErrorCode::BadDevice, &err.to_string()).await,
        };
        self.profile_change("renaming a device", move |c, me, touched| {
            sessions::rename(c, session, &name)?;
            touched.append(c, me, &Ev::Devices)?;
            Ok(Ok(()))
        })
        .await
    }

    /// Stores or removes a pinned key; every device of the account gets `pin`.
    pub(super) async fn set_pin(&mut self, pin: Pin) -> Flow {
        let max_pins = self.state.limits.max_pins;
        self.profile_change("setting a pin", move |c, me, touched| {
            let new = pin.pin.is_some() && pins::get(c, me, pin.number)?.is_none();
            if new && pins::count(c, me)? >= max_pins {
                return Ok(Err((ErrorCode::NotAllowed, "Too many pinned keys.")));
            }
            pins::set(c, me, pin.number, pin.pin.as_ref().map(|blob| blob.0.as_slice()))?;
            touched.append(c, me, &Ev::Pin { number: pin.number })?;
            Ok(Ok(()))
        })
        .await
    }
}

/// The longest report reason, in characters.
const REASON_MAX: usize = 200;

/// A report's reason, trimmed; an empty one is none.
fn check_reason(reason: Option<String>) -> Result<Option<String>, &'static str> {
    let Some(reason) = reason.map(|r| r.trim().to_string()).filter(|r| !r.is_empty()) else { return Ok(None) };
    if reason.chars().count() > REASON_MAX {
        return Err("That reason is too long.");
    }
    if reason.chars().any(char::is_control) {
        return Err("That reason has characters that aren't allowed.");
    }
    Ok(Some(reason))
}

impl Link {
    /// Reports someone for the admin, and blocks them too if asked.
    pub(super) async fn report(&mut self, number: Number, reason: Option<String>, block: bool) -> Flow {
        let Conn::SignedIn { account_id: me, .. } = self.conn else {
            return self.error(ErrorCode::NotSignedIn, "sign in first").await;
        };
        let reason = match check_reason(reason) {
            Ok(reason) => reason,
            Err(message) => return self.error(ErrorCode::BadFrame, message).await,
        };
        let (now, hub, limits) = (self.state.clock.now(), self.state.hub.clone(), self.state.limits.clone());
        let done = self
            .state
            .db
            .call(move |c| -> rusqlite::Result<Result<(), Refusal>> {
                let other = match accounts::active_by_number(c, number, now)?.filter(|row| row.id != me) {
                    Some(other) => Some(other),
                    None => accounts::known_tombstone(c, me, number, now)?,
                };
                let stranger = match &other {
                    Some(other) => relations::get(c, me, other.id)? == Relation::None,
                    None => true,
                };
                if stranger && !charge_lookup(c, me, now, &limits)? {
                    return Ok(Err((ErrorCode::RateLimited, LOOKUP_LIMIT)));
                }
                let Some(other) = other else {
                    return Ok(Err((ErrorCode::NumberUnknown, "That number isn't on corvo-chat.")));
                };
                let key = me.to_string();
                if attempts::count_since(c, "report", &key, now - DAY + 1)? >= i64::from(limits.reports_per_day) {
                    return Ok(Err((ErrorCode::RateLimited, "Too many reports today.")));
                }
                let mine = accounts::by_id(c, me, now)?.ok_or(rusqlite::Error::QueryReturnedNoRows)?;
                let mut touched = Touched::default();
                let tx = c.transaction()?;
                attempts::note(&tx, "report", &key, now)?;
                crate::reports::add(&tx, Some(me), other.id, reason.as_deref(), now)?;
                let mut seen = Seen::Unchanged;
                if block && relations::get(&tx, me, other.id)? != Relation::Blocked {
                    seen = match change(&tx, me, other.id, Change::Block, now, &mut touched)? {
                        Ok(seen) => seen,
                        Err(refusal) => return Ok(Err(refusal)),
                    };
                }
                tx.commit()?;
                touched.poke(&hub);
                if let Seen::Hidden = seen {
                    announce_pair(&hub, (me, mine.number), (other.id, other.number), false);
                }
                Ok(Ok(()))
            })
            .await;
        match done {
            Ok(Ok(())) => self.send(&ServerFrame::Reported).await,
            Ok(Err((code, message))) => self.error(code, message).await,
            Err(err) => self.internal("reporting", err).await,
        }
    }
}
