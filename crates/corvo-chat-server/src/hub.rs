//! The live sessions: which connection holds each signed-in session, so the
//! server can sign a device out while it's connected.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};

use corvo_chat_proto::frames::SignedOutReason;
use tokio::sync::oneshot;

/// Why a connection is told to stop.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kick {
    /// The session is gone: tell the device, then close.
    SignedOut(SignedOutReason),
    /// The same device connected again: close quietly.
    Replaced,
}

/// The connection holding a session, and how to stop it.
struct Online {
    conn: u64,
    kick: oneshot::Sender<Kick>,
}

/// Session id → the connection holding it.
#[derive(Clone, Default)]
pub struct Hub(Arc<Mutex<HashMap<i64, Online>>>);

impl Hub {
    /// Connection `conn` now holds `session`. A connection that held it before
    /// gets [`Kick::Replaced`].
    pub fn join(&self, session: i64, conn: u64) -> oneshot::Receiver<Kick> {
        let (kick, receiver) = oneshot::channel();
        if let Some(old) = self.online().insert(session, Online { conn, kick }) {
            let _ = old.kick.send(Kick::Replaced);
        }
        receiver
    }

    /// Stops whichever connection holds `session`, if any.
    pub fn kick(&self, session: i64, why: Kick) {
        if let Some(online) = self.online().remove(&session) {
            let _ = online.kick.send(why);
        }
    }

    /// Connection `conn` is gone. It lets go of `session` only if it still holds it.
    pub fn leave(&self, session: i64, conn: u64) {
        let mut online = self.online();
        if online.get(&session).is_some_and(|o| o.conn == conn) {
            online.remove(&session);
        }
    }

    fn online(&self) -> MutexGuard<'_, HashMap<i64, Online>> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn holder(hub: &Hub, session: i64) -> Option<u64> {
        hub.online().get(&session).map(|online| online.conn)
    }

    #[test]
    fn hub_leave_from_an_old_connection_keeps_the_new_one() {
        let hub = Hub::default();
        let mut first = hub.join(1, 1);
        let _second = hub.join(1, 2);
        hub.leave(1, 1);
        assert_eq!(holder(&hub, 1), Some(2));
        assert_eq!(first.try_recv(), Ok(Kick::Replaced));
    }
}
