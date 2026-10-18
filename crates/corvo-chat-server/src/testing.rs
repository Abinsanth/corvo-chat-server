//! Helpers for the unit tests.

use rusqlite::{Connection, params};

/// An in-memory, migrated database with foreign keys on and accounts
/// `1..=accounts`, all active.
pub fn conn_with_accounts(accounts: i64) -> Connection {
    let mut c = Connection::open_in_memory().unwrap();
    c.pragma_update(None, "foreign_keys", true).unwrap();
    crate::db::migrate(&mut c).unwrap();
    for id in 1..=accounts {
        c.execute(
            "INSERT INTO accounts (id, username, number, display_name, password_record, recovery_record,
               password_copy, recovery_copy, signing_key, exchange_key, key_signature, created_at)
             VALUES (?1, ?2, ?3, 'X', x'', x'', x'', x'', x'', x'', x'', 0)",
            params![id, format!("a-{id:04}"), format!("{}", 1_000_000_000 + id)],
        )
        .unwrap();
    }
    c
}
