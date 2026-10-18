//! Deleting an account. It becomes a tombstone: its number and username are
//! retired, the other side keeps each chat (the person shown as deleted),
//! and every device of it is signed out.

use corvo_chat_proto::frames::{ErrorCode, SignedOutReason};

use super::{Conn, Flow, Link, chat::Touched};
use crate::{accounts, chats, events::Ev, hub::Kick, relations, sessions};

impl Link {
    /// `delete_account`, after a fresh reauth. This connection is signed out
    /// with the rest, through the hub.
    pub(super) async fn delete_account(&mut self) -> Flow {
        let Conn::SignedIn { account_id: me, .. } = self.conn else {
            return self.error(ErrorCode::NotSignedIn, "sign in first").await;
        };
        let (now, hub) = (self.state.clock.now(), self.state.hub.clone());
        let done = self
            .state
            .db
            .call(move |c| -> rusqlite::Result<()> {
                let mine = accounts::by_id(c, me, now)?.ok_or(rusqlite::Error::QueryReturnedNoRows)?;
                let mutual = relations::mutual_contacts(c, me)?;
                let chats = chats::member_of(c, me)?;
                // Everyone who knows of it: a row about it, or a chat with it.
                let mut others: Vec<i64> = c
                    .prepare("SELECT account_id FROM relations WHERE other_id = ?1")?
                    .query_map([me], |r| r.get(0))?
                    .collect::<rusqlite::Result<_>>()?;
                others.extend(chats.iter().map(|chat| chat.other(me)));
                others.sort_unstable();
                others.dedup();
                let gone: Vec<i64> = sessions::list(c, me)?.iter().map(|s| s.id).collect();
                let mut touched = Touched::default();
                let tx = c.transaction()?;
                for chat in &chats {
                    chats::clear(&tx, &chat.id, me)?;
                }
                accounts::tombstone(&tx, me, now)?;
                for other in others {
                    touched.append(&tx, other, &Ev::Person { other: me })?;
                }
                tx.commit()?;
                touched.poke(&hub);
                hub.announce(me, mine.number, &mutual, false);
                for session in gone {
                    hub.kick(session, Kick::SignedOut(SignedOutReason::AccountDeleted));
                }
                tracing::info!("deleted account {}", mine.number);
                Ok(())
            })
            .await;
        match done {
            Ok(()) => Flow::Stay,
            Err(err) => self.internal("deleting an account", err).await,
        }
    }
}
