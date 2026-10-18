//! Rate-limit bookkeeping: one row per try, counted inside a time window.
//!
//! Kinds: `signup_device` (key: base64 device id), `signup_ip`, `sign_in`
//! (key: account id), `recovery` (account id), `unknown_user` (IP), and
//! per account id `lookup`, `message`, `new_person` and `change`.

use rusqlite::{Connection, params};

pub fn note(conn: &Connection, kind: &str, key: &str, now: i64) -> rusqlite::Result<()> {
    conn.execute("INSERT INTO attempts (kind, key, at) VALUES (?1, ?2, ?3)", params![kind, key, now])?;
    Ok(())
}

/// How many tries of this kind and key happened at or after `since`.
pub fn count_since(conn: &Connection, kind: &str, key: &str, since: i64) -> rusqlite::Result<i64> {
    conn.query_row(
        "SELECT COUNT(*) FROM attempts WHERE kind = ?1 AND key = ?2 AND at >= ?3",
        params![kind, key, since],
        |r| r.get(0),
    )
}

/// Forgets tries older than `before`.
pub fn prune(conn: &Connection, before: i64) -> rusqlite::Result<()> {
    conn.execute("DELETE FROM attempts WHERE at < ?1", [before])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn count_only_inside_the_window() {
        let mut c = Connection::open_in_memory().unwrap();
        crate::db::migrate(&mut c).unwrap();
        note(&c, "sign_in", "7", 100).unwrap();
        note(&c, "sign_in", "7", 200).unwrap();
        note(&c, "sign_in", "8", 200).unwrap();
        note(&c, "recovery", "7", 200).unwrap();
        assert_eq!(count_since(&c, "sign_in", "7", 150).unwrap(), 1);
        assert_eq!(count_since(&c, "sign_in", "7", 100).unwrap(), 2);
        assert_eq!(count_since(&c, "sign_in", "7", 201).unwrap(), 0);
        prune(&c, 150).unwrap();
        assert_eq!(count_since(&c, "sign_in", "7", 0).unwrap(), 1);
    }
}
