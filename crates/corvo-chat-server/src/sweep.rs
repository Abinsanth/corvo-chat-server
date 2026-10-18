//! The minute sweep. The admin's `ban` runs in another process and can't
//! reach this one's hub, so it changes the database; the sweep then signs
//! out any connection whose session or account is no longer good.

use std::time::Duration;

use corvo_chat_proto::frames::SignedOutReason;
use rusqlite::{Connection, OptionalExtension};

use crate::{hub::Kick, state::AppState};

/// How often the sweep runs.
pub const EVERY: Duration = Duration::from_secs(60);

/// Signs out every connection whose session is gone, or whose account is
/// gone, deleted or banned. Returns how many.
pub async fn once(state: &AppState) -> rusqlite::Result<usize> {
    let hub = state.hub.clone();
    state
        .db
        .call(move |c| {
            let mut kicked = 0;
            for (session, account) in hub.entries() {
                if let Some(why) = reason(c, session, account)? {
                    hub.kick(session, Kick::SignedOut(why));
                    kicked += 1;
                }
            }
            Ok(kicked)
        })
        .await
}

/// Why the connection holding `session` of `account` must go, if it must.
fn reason(c: &Connection, session: i64, account: i64) -> rusqlite::Result<Option<SignedOutReason>> {
    let state: Option<(bool, bool)> = c
        .query_row("SELECT deleted_at IS NOT NULL, banned_at IS NOT NULL FROM accounts WHERE id = ?1", [account], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .optional()?;
    let held: bool = c.query_row("SELECT EXISTS (SELECT 1 FROM sessions WHERE id = ?1)", [session], |r| r.get(0))?;
    Ok(match state {
        None => Some(SignedOutReason::Expired),
        Some((true, _)) => Some(SignedOutReason::AccountDeleted),
        Some((_, true)) => Some(SignedOutReason::Banned),
        Some(_) if !held => Some(SignedOutReason::Expired),
        Some(_) => None,
    })
}

/// Runs [`once`] every [`EVERY`] in the background.
pub fn spawn(state: AppState) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(EVERY);
        loop {
            tick.tick().await;
            match once(&state).await {
                Ok(0) => {}
                Ok(n) => tracing::info!("sweep: {n} devices signed out"),
                Err(e) => tracing::error!("sweep failed: {e}"),
            }
        }
    });
}
