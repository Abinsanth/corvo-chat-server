//! Hourly cleanup of expired pending accounts, unused devices and old
//! rate-limit attempts.

use std::time::Duration;

use rusqlite::Connection;

use crate::{attempts, limits::Limits, sessions, state::AppState};

/// How long attempts are kept: the longest window is a day.
const ATTEMPTS_KEEP_SECS: i64 = 86_400;

pub struct Cleaned {
    /// Pending accounts deleted.
    pub pending: usize,
    /// Ids of the idle sessions deleted.
    pub sessions: Vec<i64>,
}

/// Deletes expired pending accounts (their sessions and invite go with them),
/// sessions idle for `device_idle_secs`, and attempts older than a day.
pub fn run(conn: &mut Connection, now: i64, limits: &Limits) -> rusqlite::Result<Cleaned> {
    let tx = conn.transaction()?;
    let pending = tx.execute("DELETE FROM accounts WHERE pending_until IS NOT NULL AND pending_until <= ?1", [now])?;
    let idle = sessions::idle(&tx, now - limits.device_idle_secs)?;
    for id in &idle {
        sessions::delete(&tx, *id)?;
    }
    attempts::prune(&tx, now - ATTEMPTS_KEEP_SECS)?;
    tx.commit()?;
    Ok(Cleaned { pending, sessions: idle })
}

/// Runs [`run`] every hour in the background and logs the counts.
pub fn spawn_cleanup(state: AppState) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(3600));
        loop {
            tick.tick().await;
            let (now, limits) = (state.clock.now(), state.limits.clone());
            match state.db.call(move |conn| run(conn, now, &limits)).await {
                Ok(c) => {
                    tracing::info!("cleanup: {} pending accounts, {} idle devices removed", c.pending, c.sessions.len())
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

        let cleaned = run(&mut conn, now, &limits).unwrap();

        assert_eq!(cleaned.pending, 1);
        assert_eq!(cleaned.sessions.len(), 1);
        let count = |sql: &str| conn.query_row(sql, [], |r| r.get::<_, i64>(0)).unwrap();
        assert_eq!(count("SELECT COUNT(*) FROM accounts"), 2);
        assert_eq!(count("SELECT COUNT(*) FROM sessions"), 1);
        assert_eq!(count("SELECT COUNT(*) FROM attempts"), 1);
    }
}
