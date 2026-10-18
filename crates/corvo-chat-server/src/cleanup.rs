//! Hourly cleanup of expired pending accounts, unused devices and old
//! rate-limit attempts.

use std::{collections::HashSet, time::Duration};

use corvo_chat_proto::frames::SignedOutReason;
use rusqlite::Connection;

use crate::{attempts, hub::Kick, limits::Limits, sessions, state::AppState};

/// How long attempts are kept: the longest window is a day.
const ATTEMPTS_KEEP_SECS: i64 = 86_400;

pub struct Cleaned {
    /// Pending accounts deleted.
    pub pending: usize,
    /// Ids of the idle sessions deleted.
    pub sessions: Vec<i64>,
    /// The accounts those sessions belonged to.
    pub accounts: Vec<i64>,
    /// 15-minute locks that ran out.
    pub locks: usize,
}

/// Clears locks that ran out, deletes expired pending accounts (their
/// sessions and invite go with them),
/// sessions idle for `device_idle_secs`, and attempts older than a day. The
/// `online` sessions are in use, so they count as used now.
pub fn run(conn: &mut Connection, now: i64, limits: &Limits, online: &HashSet<i64>) -> rusqlite::Result<Cleaned> {
    let tx = conn.transaction()?;
    let pending = tx.execute("DELETE FROM accounts WHERE pending_until IS NOT NULL AND pending_until <= ?1", [now])?;
    for id in online {
        sessions::touch(&tx, *id, now)?;
    }
    let before = now - limits.device_idle_secs;
    let accounts = tx
        .prepare("SELECT DISTINCT account_id FROM sessions WHERE last_used < ?1 ORDER BY account_id")?
        .query_map([before], |r| r.get(0))?
        .collect::<rusqlite::Result<Vec<i64>>>()?;
    let idle = sessions::idle(&tx, before)?;
    for id in &idle {
        sessions::delete(&tx, *id)?;
    }
    attempts::prune(&tx, now - ATTEMPTS_KEEP_SECS)?;
    let locks = tx.execute(
        "UPDATE accounts SET locked_until = NULL WHERE locked_until IS NOT NULL AND locked_until <= ?1",
        [now],
    )?;
    tx.commit()?;
    Ok(Cleaned { pending, sessions: idle, accounts, locks })
}

/// One cleanup of the live server. The hub is read under the database lock,
/// which orders it against sign-ins, and any connection still holding a
/// deleted session is signed out.
pub async fn once(state: &AppState) -> rusqlite::Result<Cleaned> {
    let (now, limits, hub) = (state.clock.now(), state.limits.clone(), state.hub.clone());
    state
        .db
        .call(move |conn| {
            let cleaned = run(conn, now, &limits, &hub.sessions())?;
            for id in &cleaned.sessions {
                hub.kick(*id, Kick::SignedOut(SignedOutReason::Expired));
            }
            for account in &cleaned.accounts {
                crate::events::note_devices(conn, &hub, *account)?;
            }
            Ok(cleaned)
        })
        .await
}

/// Runs [`once`] every hour in the background and logs the counts.
pub fn spawn_cleanup(state: AppState) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(3600));
        loop {
            tick.tick().await;
            match once(&state).await {
                Ok(c) => {
                    tracing::info!(
                        "cleanup: {} pending accounts, {} idle devices removed, {} locks expired",
                        c.pending,
                        c.sessions.len(),
                        c.locks
                    )
                }
                Err(e) => tracing::error!("cleanup failed: {e}"),
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cleanup_clears_expired_locks() {
        let mut conn = crate::testing::conn_with_accounts(2);
        let now = 10_000;
        conn.execute("UPDATE accounts SET locked_until = ?2 WHERE id = ?1", [1, now - 1]).unwrap();
        conn.execute("UPDATE accounts SET locked_until = ?2 WHERE id = ?1", [2, now + 1]).unwrap();
        let cleaned = run(&mut conn, now, &Limits::default(), &HashSet::new()).unwrap();
        assert_eq!(cleaned.locks, 1);
        let lock = |id: i64| {
            conn.query_row("SELECT locked_until FROM accounts WHERE id = ?1", [id], |r| r.get::<_, Option<i64>>(0))
                .unwrap()
        };
        assert_eq!((lock(1), lock(2)), (None, Some(now + 1)));
    }

    #[test]
    fn cleanup_removes_expired_pending_idle_sessions_and_old_attempts() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "foreign_keys", true).unwrap();
        crate::db::migrate(&mut conn).unwrap();
        let limits = Limits::default();
        let now = 10_000_000;
        for (id, name, pending) in [(1, "a-0001", None), (2, "b-0002", Some(now - 1)), (3, "c-0003", Some(now + 1))] {
            conn.execute(
                "INSERT INTO accounts (id, username, number, display_name, password_record, recovery_record,
                   password_copy, recovery_copy, signing_key, exchange_key, key_signature, created_at, pending_until)
                 VALUES (?1, ?2, ?3, 'X', x'', x'', x'', x'', x'', x'', x'', 0, ?4)",
                rusqlite::params![id, name, format!("10000000{id}"), pending],
            )
            .unwrap();
        }
        for (device, last_used) in [(1, now - limits.device_idle_secs - 1), (2, now - limits.device_idle_secs)] {
            conn.execute(
                "INSERT INTO sessions (account_id, device_id, kind, name, token_hash, first_seen, last_used)
                 VALUES (1, ?1, 'cli', 'd', ?1, 0, ?2)",
                rusqlite::params![vec![device as u8; 4], last_used],
            )
            .unwrap();
        }
        attempts::note(&conn, "sign_in", "1", now - ATTEMPTS_KEEP_SECS - 1).unwrap();
        attempts::note(&conn, "sign_in", "1", now - 10).unwrap();

        let cleaned = run(&mut conn, now, &limits, &HashSet::new()).unwrap();

        assert_eq!(cleaned.pending, 1);
        assert_eq!(cleaned.sessions.len(), 1);
        let count = |sql: &str| conn.query_row(sql, [], |r| r.get::<_, i64>(0)).unwrap();
        assert_eq!(count("SELECT COUNT(*) FROM accounts"), 2);
        assert_eq!(count("SELECT COUNT(*) FROM sessions"), 1);
        assert_eq!(count("SELECT COUNT(*) FROM attempts"), 1);
    }
}
