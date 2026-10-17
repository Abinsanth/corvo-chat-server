//! SQLite storage: opening `server.db`, migrations, and an async handle.

use std::path::Path;
use std::sync::{Arc, Mutex};

use anyhow::{Context, bail};
use rusqlite::Connection;

/// The database file inside the data directory.
pub const DB_FILE: &str = "server.db";

/// Migration 1: all of M1's tables.
const MIGRATIONS: &[&str] = &["
CREATE TABLE accounts (
  id INTEGER PRIMARY KEY,
  username TEXT NOT NULL UNIQUE,
  number TEXT NOT NULL UNIQUE,
  display_name TEXT NOT NULL,
  password_record BLOB NOT NULL, recovery_record BLOB NOT NULL,
  password_copy BLOB NOT NULL, recovery_copy BLOB NOT NULL,
  signing_key BLOB NOT NULL, exchange_key BLOB NOT NULL, key_signature BLOB NOT NULL,
  created_at INTEGER NOT NULL,
  pending_until INTEGER,
  invited_by INTEGER REFERENCES accounts(id) ON DELETE SET NULL,
  wrong_count INTEGER NOT NULL DEFAULT 0,
  locked_until INTEGER,
  locked_forever INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE sessions (
  id INTEGER PRIMARY KEY,
  account_id INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
  device_id BLOB NOT NULL, kind TEXT NOT NULL, name TEXT NOT NULL,
  token_hash BLOB NOT NULL UNIQUE,
  first_seen INTEGER NOT NULL, last_used INTEGER NOT NULL,
  UNIQUE (account_id, device_id)
);
CREATE TABLE invites (
  account_id INTEGER PRIMARY KEY REFERENCES accounts(id) ON DELETE CASCADE,
  inviter_id INTEGER REFERENCES accounts(id) ON DELETE CASCADE,
  code TEXT NOT NULL, wrong_tries INTEGER NOT NULL DEFAULT 0, created_at INTEGER NOT NULL
);
CREATE TABLE attempts (kind TEXT NOT NULL, key TEXT NOT NULL, at INTEGER NOT NULL);
CREATE INDEX attempts_by_key ON attempts (kind, key, at);
"];

/// Opens (creating if needed) the database with WAL, foreign keys and a busy
/// timeout, and brings it up to date.
pub fn open(path: &Path) -> anyhow::Result<Connection> {
    let mut conn = Connection::open(path).with_context(|| format!("opening {}", path.display()))?;
    conn.query_row("PRAGMA journal_mode = WAL", [], |_| Ok(()))?;
    conn.pragma_update(None, "foreign_keys", true)?;
    conn.pragma_update(None, "busy_timeout", 5000)?;
    migrate(&mut conn)?;
    Ok(conn)
}

/// Applies the migrations not yet applied, tracked in `user_version`.
pub fn migrate(conn: &mut Connection) -> anyhow::Result<()> {
    let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if version > MIGRATIONS.len() as i64 {
        bail!("server.db is from a newer corvo-chat-server");
    }
    for (i, sql) in MIGRATIONS.iter().enumerate().skip(version as usize) {
        let tx = conn.transaction()?;
        tx.execute_batch(sql)?;
        tx.pragma_update(None, "user_version", i as i64 + 1)?;
        tx.commit()?;
    }
    Ok(())
}

/// The one connection, shared. Queries run on the blocking pool.
#[derive(Clone)]
pub struct Db(Arc<Mutex<Connection>>);

impl Db {
    pub fn new(conn: Connection) -> Db {
        Db(Arc::new(Mutex::new(conn)))
    }

    /// Runs `f` with the connection locked, off the async threads.
    pub async fn call<T: Send + 'static>(&self, f: impl FnOnce(&mut Connection) -> T + Send + 'static) -> T {
        let db = self.0.clone();
        tokio::task::spawn_blocking(move || {
            let mut conn = db.lock().unwrap_or_else(|e| e.into_inner());
            f(&mut conn)
        })
        .await
        .expect("database task panicked")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrate_twice_is_harmless() {
        let mut conn = Connection::open_in_memory().unwrap();
        migrate(&mut conn).unwrap();
        migrate(&mut conn).unwrap();
        let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        assert_eq!(version, 1_i64);
    }

    #[test]
    fn newer_database_is_refused() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "user_version", 99).unwrap();
        let err = migrate(&mut conn).unwrap_err();
        assert!(err.to_string().contains("newer corvo-chat-server"));
    }
}
