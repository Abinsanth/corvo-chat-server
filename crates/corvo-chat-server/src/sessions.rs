//! Device sessions: a random token per signed-in device, stored only as its
//! SHA-256, at most a few devices per account.

use corvo_chat_proto::B64;
use corvo_chat_proto::frames::{Device, DeviceKind};
use rusqlite::{Connection, OptionalExtension, Row, params};
use sha2::{Digest, Sha256};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionRow {
    pub id: i64,
    pub account_id: i64,
    pub device: Device,
    pub first_seen: i64,
    pub last_used: i64,
}

pub struct NewSession {
    pub id: i64,
    pub token: [u8; 32],
    /// The id of the session this one replaced (same device id), so the
    /// caller can close that connection.
    pub replaced: Option<i64>,
}

#[derive(Debug)]
pub enum SessionError {
    DeviceLimit,
    Db(rusqlite::Error),
}

impl From<rusqlite::Error> for SessionError {
    fn from(e: rusqlite::Error) -> Self {
        SessionError::Db(e)
    }
}

const COLUMNS: &str = "id, account_id, device_id, kind, name, first_seen, last_used";

fn kind_text(kind: DeviceKind) -> &'static str {
    match kind {
        DeviceKind::Cli => "cli",
        DeviceKind::Web => "web",
        DeviceKind::Phone => "phone",
    }
}

fn kind_from(text: &str) -> Option<DeviceKind> {
    match text {
        "cli" => Some(DeviceKind::Cli),
        "web" => Some(DeviceKind::Web),
        "phone" => Some(DeviceKind::Phone),
        _ => None,
    }
}

fn hash(token: &[u8; 32]) -> Vec<u8> {
    Sha256::digest(token).to_vec()
}

fn row(r: &Row) -> rusqlite::Result<SessionRow> {
    let bad = |i: usize| rusqlite::Error::InvalidColumnType(i, "device".into(), rusqlite::types::Type::Blob);
    let id: Vec<u8> = r.get(2)?;
    let kind: String = r.get(3)?;
    Ok(SessionRow {
        id: r.get(0)?,
        account_id: r.get(1)?,
        device: Device {
            id: B64(id.try_into().map_err(|_| bad(2))?),
            kind: kind_from(&kind).ok_or_else(|| bad(3))?,
            name: r.get(4)?,
        },
        first_seen: r.get(5)?,
        last_used: r.get(6)?,
    })
}

/// Signs a device in. The same device id replaces its old session; a new
/// device beyond `max_devices` is refused.
pub fn create(
    conn: &mut Connection,
    account_id: i64,
    device: &Device,
    now: i64,
    max_devices: usize,
) -> Result<NewSession, SessionError> {
    let tx = conn.transaction()?;
    let device_id = device.id.0.to_vec();
    let replaced: Option<i64> = tx
        .query_row(
            "SELECT id FROM sessions WHERE account_id = ?1 AND device_id = ?2",
            params![account_id, device_id],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(old) = replaced {
        tx.execute("DELETE FROM sessions WHERE id = ?1", [old])?;
    } else {
        let count: i64 =
            tx.query_row("SELECT COUNT(*) FROM sessions WHERE account_id = ?1", [account_id], |r| r.get(0))?;
        if count >= max_devices as i64 {
            return Err(SessionError::DeviceLimit);
        }
    }
    let token: [u8; 32] = rand::random();
    tx.execute(
        "INSERT INTO sessions (account_id, device_id, kind, name, token_hash, first_seen, last_used)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)",
        params![account_id, device_id, kind_text(device.kind), device.name, hash(&token), now],
    )?;
    let id = tx.last_insert_rowid();
    tx.commit()?;
    Ok(NewSession { id, token, replaced })
}

pub fn by_token(conn: &Connection, token: &[u8; 32]) -> rusqlite::Result<Option<SessionRow>> {
    conn.query_row(&format!("SELECT {COLUMNS} FROM sessions WHERE token_hash = ?1"), [hash(token)], row).optional()
}

pub fn touch(conn: &Connection, id: i64, now: i64) -> rusqlite::Result<()> {
    conn.execute("UPDATE sessions SET last_used = ?2 WHERE id = ?1", params![id, now])?;
    Ok(())
}

/// The account's sessions, oldest first.
pub fn list(conn: &Connection, account_id: i64) -> rusqlite::Result<Vec<SessionRow>> {
    let mut stmt =
        conn.prepare(&format!("SELECT {COLUMNS} FROM sessions WHERE account_id = ?1 ORDER BY first_seen, id"))?;
    stmt.query_map([account_id], row)?.collect()
}

pub fn delete(conn: &Connection, id: i64) -> rusqlite::Result<bool> {
    Ok(conn.execute("DELETE FROM sessions WHERE id = ?1", [id])? > 0)
}

/// Deletes every session of the account except `keep`; returns their ids.
pub fn delete_others(conn: &Connection, account_id: i64, keep: i64) -> rusqlite::Result<Vec<i64>> {
    let mut stmt = conn.prepare("DELETE FROM sessions WHERE account_id = ?1 AND id != ?2 RETURNING id")?;
    stmt.query_map(params![account_id, keep], |r| r.get(0))?.collect()
}

/// Ids of sessions whose `last_used` is before `before`.
pub fn idle(conn: &Connection, before: i64) -> rusqlite::Result<Vec<i64>> {
    let mut stmt = conn.prepare("SELECT id FROM sessions WHERE last_used < ?1 ORDER BY id")?;
    stmt.query_map([before], |r| r.get(0))?.collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conn() -> Connection {
        let mut c = Connection::open_in_memory().unwrap();
        c.pragma_update(None, "foreign_keys", true).unwrap();
        crate::db::migrate(&mut c).unwrap();
        c.execute(
            "INSERT INTO accounts (id, username, number, display_name, password_record, recovery_record,
               password_copy, recovery_copy, signing_key, exchange_key, key_signature, created_at)
             VALUES (1, 'abin-0000', '100000001', 'Abin', x'', x'', x'', x'', x'', x'', x'', 0)",
            [],
        )
        .unwrap();
        c
    }

    fn device(n: u8) -> Device {
        Device { id: B64([n; 16]), kind: DeviceKind::Cli, name: format!("laptop {n}") }
    }

    #[test]
    fn fourth_device_is_refused() {
        let mut c = conn();
        for n in 1..=3 {
            create(&mut c, 1, &device(n), 10, 3).unwrap();
        }
        assert!(matches!(create(&mut c, 1, &device(4), 10, 3), Err(SessionError::DeviceLimit)));
        assert_eq!(list(&c, 1).unwrap().len(), 3);
    }

    #[test]
    fn same_device_id_replaces_its_session() {
        let mut c = conn();
        let first = create(&mut c, 1, &device(1), 10, 3).unwrap();
        assert_eq!(first.replaced, None);
        let second = create(&mut c, 1, &device(1), 20, 3).unwrap();
        assert_eq!(second.replaced, Some(first.id));
        assert_ne!(second.id, first.id);
        assert!(by_token(&c, &first.token).unwrap().is_none());
        let found = by_token(&c, &second.token).unwrap().unwrap();
        assert_eq!((found.id, found.device, found.first_seen), (second.id, device(1), 20));
        assert_eq!(list(&c, 1).unwrap().len(), 1);
        // A full account can still replace its own device.
        create(&mut c, 1, &device(2), 10, 3).unwrap();
        create(&mut c, 1, &device(3), 10, 3).unwrap();
        assert!(create(&mut c, 1, &device(1), 30, 3).unwrap().replaced.is_some());
    }

    #[test]
    fn token_is_stored_hashed() {
        let mut c = conn();
        let s = create(&mut c, 1, &device(1), 10, 3).unwrap();
        let stored: Vec<u8> = c.query_row("SELECT token_hash FROM sessions", [], |r| r.get(0)).unwrap();
        assert_eq!(stored.len(), 32);
        assert_ne!(stored, s.token.to_vec());
        let kind: String = c.query_row("SELECT kind FROM sessions", [], |r| r.get(0)).unwrap();
        assert_eq!(kind, "cli");
    }

    #[test]
    fn delete_others_keeps_one() {
        let mut c = conn();
        let a = create(&mut c, 1, &device(1), 10, 3).unwrap();
        let b = create(&mut c, 1, &device(2), 11, 3).unwrap();
        let d = create(&mut c, 1, &device(3), 12, 3).unwrap();
        let mut gone = delete_others(&c, 1, b.id).unwrap();
        gone.sort();
        assert_eq!(gone, vec![a.id, d.id]);
        assert_eq!(list(&c, 1).unwrap().iter().map(|s| s.id).collect::<Vec<_>>(), vec![b.id]);
        assert!(delete(&c, b.id).unwrap());
        assert!(!delete(&c, b.id).unwrap());
    }

    #[test]
    fn idle_finds_old_sessions() {
        let mut c = conn();
        let a = create(&mut c, 1, &device(1), 10, 3).unwrap();
        let b = create(&mut c, 1, &device(2), 10, 3).unwrap();
        touch(&c, b.id, 500).unwrap();
        assert_eq!(idle(&c, 100).unwrap(), vec![a.id]);
        assert_eq!(idle(&c, 10).unwrap(), Vec::<i64>::new());
        assert_eq!(by_token(&c, &b.token).unwrap().unwrap().last_used, 500);
    }
}
