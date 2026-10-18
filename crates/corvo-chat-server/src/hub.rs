//! The live sessions: which connection holds each signed-in session, so the
//! server can sign a device out while it's connected, push it new events,
//! and tell contacts when an account comes online or goes.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, MutexGuard};

use corvo_chat_proto::{Number, frames::SignedOutReason};
use tokio::sync::{mpsc, oneshot};

/// Why a connection is told to stop.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kick {
    /// The session is gone: tell the device, then close.
    SignedOut(SignedOutReason),
    /// The same device connected again: close quietly.
    Replaced,
}

/// What a connection is told while it runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Push {
    /// Its account's log has new events.
    Events,
    /// A contact came online or went.
    Presence { number: Number, online: bool },
}

/// The connection holding a session, and how to reach it.
struct Online {
    conn: u64,
    account: i64,
    /// Taken by the first kick.
    kick: Option<oneshot::Sender<Kick>>,
    push: mpsc::UnboundedSender<Push>,
}

/// What a connection gets when it takes a session.
pub struct Joined {
    pub kick: oneshot::Receiver<Kick>,
    pub push: mpsc::UnboundedReceiver<Push>,
    /// The account had no connection before this one.
    pub came_online: bool,
}

/// Session id → the connection holding it.
#[derive(Clone, Default)]
pub struct Hub(Arc<Mutex<HashMap<i64, Online>>>);

impl Hub {
    /// Connection `conn` now holds `session` of `account`. A connection that
    /// held it before gets [`Kick::Replaced`].
    pub fn join(&self, session: i64, account: i64, conn: u64) -> Joined {
        let (kick, kick_rx) = oneshot::channel();
        let (push, push_rx) = mpsc::unbounded_channel();
        let mut online = self.map();
        let came_online = !online.values().any(|o| o.account == account);
        if let Some(old) = online.insert(session, Online { conn, account, kick: Some(kick), push })
            && let Some(kick) = old.kick
        {
            let _ = kick.send(Kick::Replaced);
        }
        Joined { kick: kick_rx, push: push_rx, came_online }
    }

    /// Tells whichever connection holds `session` to stop. It stays in the
    /// hub until it leaves.
    pub fn kick(&self, session: i64, why: Kick) {
        if let Some(kick) = self.map().get_mut(&session).and_then(|o| o.kick.take()) {
            let _ = kick.send(why);
        }
    }

    /// Connection `conn` is gone. It lets go of `session` only if it still
    /// holds it. Returns the account if that left it with no connection.
    pub fn leave(&self, session: i64, conn: u64) -> Option<i64> {
        let mut online = self.map();
        let account = online.get(&session).filter(|o| o.conn == conn)?.account;
        online.remove(&session);
        (!online.values().any(|o| o.account == account)).then_some(account)
    }

    /// Tells every connection of `account`.
    pub fn push(&self, account: i64, push: Push) {
        Self::send(&self.map(), account, push);
    }

    /// Whether `account` has any connection.
    pub fn online(&self, account: i64) -> bool {
        self.map().values().any(|o| o.account == account)
    }

    /// Tells every connection of each account in `to` whether `account`
    /// (`number`) is online, or offline when `visible` is false. The state
    /// is read under the same lock as joins and leaves, so the last word
    /// each listener gets is the current one.
    pub fn announce(&self, account: i64, number: Number, to: &[i64], visible: bool) {
        let online = self.map();
        let is_online = visible && online.values().any(|o| o.account == account);
        for listener in to {
            Self::send(&online, *listener, Push::Presence { number, online: is_online });
        }
    }

    /// The sessions held by a connection right now.
    pub fn sessions(&self) -> HashSet<i64> {
        self.map().keys().copied().collect()
    }

    fn send(online: &HashMap<i64, Online>, account: i64, push: Push) {
        for o in online.values().filter(|o| o.account == account) {
            let _ = o.push.send(push);
        }
    }

    fn map(&self) -> MutexGuard<'_, HashMap<i64, Online>> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: i64 = 7;

    fn holder(hub: &Hub, session: i64) -> Option<u64> {
        hub.map().get(&session).map(|online| online.conn)
    }

    #[test]
    fn hub_leave_from_an_old_connection_keeps_the_new_one() {
        let hub = Hub::default();
        let mut first = hub.join(1, A, 1);
        let _second = hub.join(1, A, 2);
        assert_eq!(hub.leave(1, 1), None);
        assert_eq!(holder(&hub, 1), Some(2));
        assert_eq!(first.kick.try_recv(), Ok(Kick::Replaced));
    }

    #[test]
    fn first_join_and_last_leave_change_presence() {
        let hub = Hub::default();
        assert!(hub.join(1, A, 1).came_online);
        assert!(!hub.join(2, A, 2).came_online);
        assert_eq!(hub.leave(1, 1), None);
        assert!(hub.online(A));
        assert_eq!(hub.leave(2, 2), Some(A));
        assert!(!hub.online(A));
    }

    #[test]
    fn push_reaches_every_connection_of_the_account() {
        let hub = Hub::default();
        let mut one = hub.join(1, A, 1);
        let mut two = hub.join(2, A, 2);
        let mut other = hub.join(3, A + 1, 3);
        hub.push(A, Push::Events);
        assert_eq!(one.push.try_recv(), Ok(Push::Events));
        assert_eq!(two.push.try_recv(), Ok(Push::Events));
        assert!(other.push.try_recv().is_err());
    }

    #[test]
    fn kick_keeps_the_entry_until_leave() {
        let hub = Hub::default();
        let mut joined = hub.join(1, A, 1);
        hub.kick(1, Kick::SignedOut(SignedOutReason::ByAnotherDevice));
        assert_eq!(joined.kick.try_recv(), Ok(Kick::SignedOut(SignedOutReason::ByAnotherDevice)));
        assert!(hub.online(A));
        assert_eq!(hub.leave(1, 1), Some(A));
    }

    #[test]
    fn announce_sends_the_current_state_to_each_listener() {
        let hub = Hub::default();
        let number = Number::from_body(100_000_000).unwrap();
        let mut listener = hub.join(1, A, 1);
        let _speaker = hub.join(2, A + 1, 2);
        hub.announce(A + 1, number, &[A], true);
        hub.announce(A + 1, number, &[A], false);
        assert_eq!(listener.push.try_recv(), Ok(Push::Presence { number, online: true }));
        assert_eq!(listener.push.try_recv(), Ok(Push::Presence { number, online: false }));
        hub.leave(2, 2);
        hub.announce(A + 1, number, &[A], true);
        assert_eq!(listener.push.try_recv(), Ok(Push::Presence { number, online: false }));
    }
}
