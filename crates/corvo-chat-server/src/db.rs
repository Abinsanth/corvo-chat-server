//! SQLite storage: opening `server.db`, migrations, and an async handle.

use std::path::Path;
use std::sync::{Arc, Mutex};

use anyhow::{Context, bail};
use rusqlite::Connection;

/// The database file inside the data directory.
pub const DB_FILE: &str = "server.db";

/// Migration 1: M1's tables (accounts). Migration 2: M2's tables (chats and
/// sync). Migration 3: where each side's current request starts. Migration 4:
/// bans, deleted accounts and reports.
const MIGRATIONS: &[&str] = &[
    "
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
  id INTEGER PRIMARY KEY AUTOINCREMENT,
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
",
    "
CREATE TABLE relations (
  account_id INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
  other_id INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
  state TEXT NOT NULL,
  since INTEGER NOT NULL,
  PRIMARY KEY (account_id, other_id)
);
CREATE INDEX relations_by_other ON relations (other_id);
CREATE TABLE chats (
  id BLOB PRIMARY KEY,
  low INTEGER NOT NULL, high INTEGER NOT NULL,
  creator INTEGER NOT NULL,
  signature BLOB NOT NULL,
  last_seq INTEGER NOT NULL DEFAULT 0,
  created_at INTEGER NOT NULL,
  UNIQUE (low, high)
);
CREATE TABLE chat_members (
  chat_id BLOB NOT NULL REFERENCES chats(id) ON DELETE CASCADE,
  account_id INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
  sealed_key BLOB NOT NULL,
  received_through INTEGER NOT NULL DEFAULT 0,
  read_through INTEGER NOT NULL DEFAULT 0,
  cleared_through INTEGER NOT NULL DEFAULT 0,
  announced INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (chat_id, account_id)
);
CREATE INDEX chat_members_by_account ON chat_members (account_id);
CREATE TABLE messages (
  chat_id BLOB NOT NULL REFERENCES chats(id) ON DELETE CASCADE,
  seq INTEGER NOT NULL,
  msg_id BLOB NOT NULL,
  sender_id INTEGER NOT NULL,
  body BLOB NOT NULL,
  sent_at INTEGER NOT NULL,
  withheld INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (chat_id, seq),
  UNIQUE (chat_id, msg_id)
);
CREATE TABLE events (
  account_id INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
  seq INTEGER NOT NULL,
  kind TEXT NOT NULL,
  chat_id BLOB, other_id INTEGER, msg_seq INTEGER, number TEXT,
  PRIMARY KEY (account_id, seq)
);
CREATE TABLE pins (
  account_id INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
  number TEXT NOT NULL,
  pin BLOB NOT NULL,
  PRIMARY KEY (account_id, number)
);
",
    "
ALTER TABLE chat_members ADD COLUMN request_from INTEGER NOT NULL DEFAULT 0;
",
    "
ALTER TABLE accounts ADD COLUMN banned_at INTEGER;
ALTER TABLE accounts ADD COLUMN deleted_at INTEGER;
CREATE TABLE reports (
  id INTEGER PRIMARY KEY,
  reporter_id INTEGER REFERENCES accounts(id) ON DELETE SET NULL,
  reported_id INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
  reason TEXT,
  at INTEGER NOT NULL
);
CREATE INDEX reports_by_reported ON reports (reported_id);
",
];

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
        assert_eq!(version, 4_i64);
    }

    #[test]
    fn newer_database_is_refused() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "user_version", 99).unwrap();
        let err = migrate(&mut conn).unwrap_err();
        assert!(err.to_string().contains("newer corvo-chat-server"));
    }
}
