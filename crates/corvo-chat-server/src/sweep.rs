//! The minute sweep. The admin's `ban` runs in another process and can't
//! reach this one's hub, so it deletes the sessions in the database; the
//! sweep then signs out any connection still holding one of them.

use std::time::Duration;

use corvo_chat_proto::frames::SignedOutReason;
use rusqlite::OptionalExtension;

use crate::{hub::Kick, state::AppState};

/// How often the sweep runs.
pub const EVERY: Duration = Duration::from_secs(60);

/// Signs out every connection whose session is gone and whose account is
/// banned. Returns how many.
pub async fn once(state: &AppState) -> rusqlite::Result<usize> {
    let hub = state.hub.clone();
    state
        .db
        .call(move |c| {
            let mut kicked = 0;
            for (session, account) in hub.entries() {
                let held: bool =
                    c.query_row("SELECT EXISTS (SELECT 1 FROM sessions WHERE id = ?1)", [session], |r| r.get(0))?;
                let banned: Option<bool> = c
                    .query_row("SELECT banned_at IS NOT NULL FROM accounts WHERE id = ?1", [account], |r| r.get(0))
                    .optional()?;
                if !held && banned == Some(true) {
                    hub.kick(session, Kick::SignedOut(SignedOutReason::Banned));
                    kicked += 1;
                }
            }
            Ok(kicked)
        })
        .await
}

/// Runs [`once`] every [`EVERY`] in the background.
pub fn spawn(state: AppState) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(EVERY);
        loop {
            tick.tick().await;
            match once(&state).await {
                Ok(0) => {}
                Ok(n) => tracing::info!("sweep: {n} banned devices signed out"),
                Err(e) => tracing::error!("sweep failed: {e}"),
            }
        }
    });
}
