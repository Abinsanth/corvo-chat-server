//! Account rows: names and numbers, pending accounts and their expiry,
//! the OPAQUE records and locked copies, and wrong-password counting.

use corvo_chat_proto::frames::Account;
use corvo_chat_proto::names::{ENDING_ALPHABET, ENDING_LEN};
use corvo_chat_proto::{B64, Number, PublicKeys};
use rusqlite::{Connection, OptionalExtension, Row, params};

use crate::limits::Limits;

/// Everything signup stores about a new account.
pub struct NewAccount {
    pub username: String,
    pub number: Number,
    pub display_name: String,
    pub password_record: Vec<u8>,
    pub recovery_record: Vec<u8>,
    pub password_copy: Vec<u8>,
    pub recovery_copy: Vec<u8>,
    pub public_keys: PublicKeys,
}

/// An account as the server reads it back (no secrets).
pub struct AccountRow {
    pub id: i64,
    pub username: String,
    pub number: Number,
    pub display_name: String,
    pub pending_until: Option<i64>,
    pub wrong_count: i64,
    pub locked_until: Option<i64>,
    pub locked_forever: bool,
    pub banned: bool,
    /// A tombstone: the account deleted itself.
    pub deleted: bool,
}

impl AccountRow {
    pub fn to_frame(&self) -> Account {
        Account {
            username: self.username.clone(),
            number: self.number,
            display_name: self.display_name.clone(),
            pending_until: self.pending_until,
        }
    }
}

#[derive(Debug)]
pub enum CreateError {
    /// The username or number was taken between `pick_name` and the insert.
    Taken,
    Db(rusqlite::Error),
}

/// Which of the two OPAQUE records (and their locked copies) to touch.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Which {
    Password,
    Recovery,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LockState {
    Open,
    Until(i64),
    Forever,
}

const COLUMNS: &str = "id, username, number, display_name, pending_until, wrong_count, locked_until, locked_forever, \
                       banned_at IS NOT NULL, deleted_at IS NOT NULL";

fn row_to_account(r: &Row) -> rusqlite::Result<AccountRow> {
    let number: String = r.get(2)?;
    Ok(AccountRow {
        id: r.get(0)?,
        username: r.get(1)?,
        number: Number::parse(&number)
            .map_err(|e| rusqlite::Error::FromSqlConversionFailure(2, rusqlite::types::Type::Text, Box::new(e)))?,
        display_name: r.get(3)?,
        pending_until: r.get(4)?,
        wrong_count: r.get(5)?,
        locked_until: r.get(6)?,
        locked_forever: r.get(7)?,
        banned: r.get(8)?,
        deleted: r.get(9)?,
    })
}

/// Draws a free `<base>-<ending>` username and a free number.
pub fn pick_name(conn: &Connection, base: &str) -> rusqlite::Result<(String, Number)> {
    pick_name_with(
        conn,
        base,
        &mut || {
            (0..ENDING_LEN).map(|_| ENDING_ALPHABET[rand::random_range(0..ENDING_ALPHABET.len())] as char).collect()
        },
        &mut || loop {
            if let Some(n) = Number::from_body(rand::random_range(100_000_000..=999_999_999)) {
                break n;
            }
        },
    )
}

fn pick_name_with(
    conn: &Connection,
    base: &str,
    ending: &mut dyn FnMut() -> String,
    number: &mut dyn FnMut() -> Number,
) -> rusqlite::Result<(String, Number)> {
    loop {
        let username = format!("{base}-{}", ending());
        let n = number();
        let taken: bool = conn.query_row(
            "SELECT EXISTS (SELECT 1 FROM accounts WHERE username = ?1 OR number = ?2)",
            params![username, n.digits()],
            |r| r.get(0),
        )?;
        if !taken {
            return Ok((username, n));
        }
    }
}

/// Inserts a pending account that expires `pending_secs` after `now`.
pub fn create_pending(
    conn: &mut Connection,
    new: &NewAccount,
    now: i64,
    pending_secs: i64,
) -> Result<i64, CreateError> {
    let result = conn.execute(
        "INSERT INTO accounts (username, number, display_name, password_record, recovery_record, password_copy, recovery_copy,
                               signing_key, exchange_key, key_signature, created_at, pending_until)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        params![
            new.username,
            new.number.digits(),
            new.display_name,
            new.password_record,
            new.recovery_record,
            new.password_copy,
            new.recovery_copy,
            new.public_keys.signing.0.as_slice(),
            new.public_keys.exchange.0.as_slice(),
            new.public_keys.signature.0.as_slice(),
            now,
            now + pending_secs,
        ],
    );
    match result {
        Ok(_) => Ok(conn.last_insert_rowid()),
        Err(rusqlite::Error::SqliteFailure(e, _)) if e.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_UNIQUE => {
            Err(CreateError::Taken)
        }
        Err(e) => Err(CreateError::Db(e)),
    }
}

/// Looks an account up by one column; a pending account past its time is
/// deleted (its sessions and invite cascade) and not returned.
fn find(
    conn: &Connection,
    column: &str,
    value: impl rusqlite::ToSql,
    now: i64,
) -> rusqlite::Result<Option<AccountRow>> {
    conn.execute("DELETE FROM accounts WHERE pending_until IS NOT NULL AND pending_until <= ?1", [now])?;
    conn.query_row(&format!("SELECT {COLUMNS} FROM accounts WHERE {column} = ?1"), [value], row_to_account).optional()
}

pub fn by_username(conn: &Connection, username: &str, now: i64) -> rusqlite::Result<Option<AccountRow>> {
    find(conn, "username", username, now)
}

pub fn by_id(conn: &Connection, id: i64, now: i64) -> rusqlite::Result<Option<AccountRow>> {
    find(conn, "id", id, now)
}

pub fn by_number(conn: &Connection, number: Number, now: i64) -> rusqlite::Result<Option<AccountRow>> {
    find(conn, "number", number.digits(), now)
}

/// An active account by number: `None` for a pending, deleted or unknown
/// one. A banned account is returned; callers answer `banned`.
pub fn active_by_number(conn: &Connection, number: Number, now: i64) -> rusqlite::Result<Option<AccountRow>> {
    Ok(by_number(conn, number, now)?.filter(|row| row.pending_until.is_none() && !row.deleted))
}

/// Deletes an account as a tombstone: the row, number, username and public
/// keys stay (the number and username are retired, and old messages still
/// show who sent them); every secret and the display name are wiped; its
/// sessions, invites, pins, event log and its own relation rows go. Run it
/// inside the caller's transaction.
pub fn tombstone(conn: &Connection, id: i64, now: i64) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE accounts SET deleted_at = ?2, display_name = '', password_record = x'', recovery_record = x'',
           password_copy = x'', recovery_copy = x'' WHERE id = ?1",
        params![id, now],
    )?;
    for sql in [
        "DELETE FROM sessions WHERE account_id = ?1",
        "DELETE FROM invites WHERE account_id = ?1 OR inviter_id = ?1",
        "DELETE FROM pins WHERE account_id = ?1",
        "DELETE FROM events WHERE account_id = ?1",
        "DELETE FROM relations WHERE account_id = ?1",
    ] {
        conn.execute(sql, [id])?;
    }
    Ok(())
}

pub fn ban(conn: &Connection, id: i64, now: i64) -> rusqlite::Result<()> {
    conn.execute("UPDATE accounts SET banned_at = ?2 WHERE id = ?1", params![id, now])?;
    Ok(())
}

pub fn unban(conn: &Connection, id: i64) -> rusqlite::Result<()> {
    conn.execute("UPDATE accounts SET banned_at = NULL WHERE id = ?1", [id])?;
    Ok(())
}

pub fn set_display_name(conn: &Connection, id: i64, name: &str) -> rusqlite::Result<()> {
    conn.execute("UPDATE accounts SET display_name = ?2 WHERE id = ?1", params![id, name])?;
    Ok(())
}

/// The public keys the account signed up with.
pub fn public_keys(conn: &Connection, id: i64) -> rusqlite::Result<PublicKeys> {
    let bad = |i: usize| rusqlite::Error::InvalidColumnType(i, "public key".into(), rusqlite::types::Type::Blob);
    conn.query_row("SELECT signing_key, exchange_key, key_signature FROM accounts WHERE id = ?1", [id], |r| {
        let (signing, exchange, signature): (Vec<u8>, Vec<u8>, Vec<u8>) = (r.get(0)?, r.get(1)?, r.get(2)?);
        Ok(PublicKeys {
            signing: B64(signing.try_into().map_err(|_| bad(0))?),
            exchange: B64(exchange.try_into().map_err(|_| bad(1))?),
            signature: B64(signature.try_into().map_err(|_| bad(2))?),
        })
    })
}

/// The password and recovery OPAQUE records.
pub fn records(conn: &Connection, id: i64) -> rusqlite::Result<(Vec<u8>, Vec<u8>)> {
    conn.query_row("SELECT password_record, recovery_record FROM accounts WHERE id = ?1", [id], |r| {
        Ok((r.get(0)?, r.get(1)?))
    })
}

pub fn copy(conn: &Connection, id: i64, which: Which) -> rusqlite::Result<Vec<u8>> {
    let column = match which {
        Which::Password => "password_copy",
        Which::Recovery => "recovery_copy",
    };
    conn.query_row(&format!("SELECT {column} FROM accounts WHERE id = ?1"), [id], |r| r.get(0))
}

/// Swaps one OPAQUE record and its locked copy.
pub fn replace(conn: &Connection, id: i64, which: Which, record: &[u8], copy: &[u8]) -> rusqlite::Result<()> {
    let sql = match which {
        Which::Password => "UPDATE accounts SET password_record = ?2, password_copy = ?3 WHERE id = ?1",
        Which::Recovery => "UPDATE accounts SET recovery_record = ?2, recovery_copy = ?3 WHERE id = ?1",
    };
    conn.execute(sql, params![id, record, copy])?;
    Ok(())
}

/// Makes a pending account active, remembering who invited it.
pub fn activate(conn: &Connection, id: i64, invited_by: Option<i64>, _now: i64) -> rusqlite::Result<()> {
    conn.execute("UPDATE accounts SET pending_until = NULL, invited_by = ?2 WHERE id = ?1", params![id, invited_by])?;
    conn.execute("DELETE FROM invites WHERE account_id = ?1", [id])?;
    Ok(())
}

/// Counts one password try. Every sign-in start counts until a success clears it.
pub fn note_try(conn: &Connection, id: i64, now: i64, limits: &Limits) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE accounts SET wrong_count = wrong_count + 1,
           locked_forever = CASE WHEN wrong_count + 1 >= ?3 THEN 1 ELSE locked_forever END,
           locked_until = CASE WHEN wrong_count + 1 = ?2 THEN ?4 ELSE locked_until END
         WHERE id = ?1",
        params![id, limits.wrong_before_lock, limits.wrong_before_forever, now + limits.lock_secs],
    )?;
    Ok(())
}

/// A successful sign-in or recovery: count back to 0, locks lifted.
pub fn note_success(conn: &Connection, id: i64) -> rusqlite::Result<()> {
    conn.execute("UPDATE accounts SET wrong_count = 0, locked_until = NULL, locked_forever = 0 WHERE id = ?1", [id])?;
    Ok(())
}

pub fn lock_state(row: &AccountRow, now: i64) -> LockState {
    match row.locked_until {
        _ if row.locked_forever => LockState::Forever,
        Some(t) if t > now => LockState::Until(t),
        _ => LockState::Open,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    fn conn() -> Connection {
        let mut c = Connection::open_in_memory().unwrap();
        c.pragma_update(None, "foreign_keys", true).unwrap();
        crate::db::migrate(&mut c).unwrap();
        c
    }

    fn number(body: u32) -> Number {
        Number::from_body(body).unwrap()
    }

    fn new_account(username: &str, n: Number) -> NewAccount {
        NewAccount {
            username: username.into(),
            number: n,
            display_name: "Abin".into(),
            password_record: vec![1; 192],
            recovery_record: vec![2; 192],
            password_copy: vec![3; 40],
            recovery_copy: vec![4; 40],
            public_keys: PublicKeys { signing: B64([5; 32]), exchange: B64([6; 32]), signature: B64([7; 64]) },
        }
    }

    #[test]
    fn tombstoned_number_is_never_drawn() {
        let mut c = conn();
        let (taken, free) = (number(482_910_557), number(100_000_000));
        let id = create_pending(&mut c, &new_account("abin-0000", taken), 100, 86_400).unwrap();
        activate(&c, id, None, 100).unwrap();
        tombstone(&c, id, 200).unwrap();
        let mut numbers = [taken, free].into_iter();
        let mut endings = ["0000", "0001"].into_iter();
        let picked =
            pick_name_with(&c, "abin", &mut || endings.next().unwrap().into(), &mut || numbers.next().unwrap());
        assert_eq!(picked.unwrap(), ("abin-0001".into(), free));
        assert!(by_id(&c, id, 300).unwrap().unwrap().deleted);
    }

    #[test]
    fn ban_and_unban_show_on_the_row() {
        let mut c = conn();
        let n = number(482_910_557);
        let id = create_pending(&mut c, &new_account("abin-0000", n), 100, 86_400).unwrap();
        activate(&c, id, None, 100).unwrap();
        assert!(!by_id(&c, id, 100).unwrap().unwrap().banned);
        ban(&c, id, 200).unwrap();
        let row = active_by_number(&c, n, 200).unwrap().expect("a banned account is still found");
        assert!(row.banned && !row.deleted);
        unban(&c, id).unwrap();
        assert!(!by_id(&c, id, 300).unwrap().unwrap().banned);
    }

    #[test]
    fn pending_account_is_not_found_by_active_by_number() {
        let mut c = conn();
        let n = number(482_910_557);
        let id = create_pending(&mut c, &new_account("abin-0000", n), 100, 86_400).unwrap();
        assert!(active_by_number(&c, n, 100).unwrap().is_none());
        activate(&c, id, None, 100).unwrap();
        assert_eq!(active_by_number(&c, n, 100).unwrap().map(|row| row.id), Some(id));
        assert!(active_by_number(&c, number(100_000_000), 100).unwrap().is_none());
    }

    #[test]
    fn set_display_name_changes_the_frame() {
        let mut c = conn();
        let id = create_pending(&mut c, &new_account("abin-0000", number(482_910_557)), 100, 86_400).unwrap();
        set_display_name(&c, id, "Abin P").unwrap();
        assert_eq!(by_id(&c, id, 100).unwrap().unwrap().to_frame().display_name, "Abin P");
    }

    #[test]
    fn public_keys_come_back_as_stored() {
        let mut c = conn();
        let new = new_account("abin-0000", number(482_910_557));
        let id = create_pending(&mut c, &new, 100, 86_400).unwrap();
        assert_eq!(public_keys(&c, id).unwrap(), new.public_keys);
    }

    #[test]
    fn pick_name_skips_taken_endings_and_numbers() {
        let mut c = conn();
        create_pending(&mut c, &new_account("abin-0000", number(100_000_001)), 0, 100).unwrap();
        let (n1, n2) = (number(100_000_001), number(100_000_002));
        let mut endings = ["0000", "0001"].into_iter();
        let mut numbers = [n1, n2].into_iter();
        let got =
            pick_name_with(&c, "abin", &mut || endings.next().unwrap().to_string(), &mut || numbers.next().unwrap())
                .unwrap();
        assert_eq!(got, ("abin-0001".to_string(), n2));
    }

    #[test]
    fn two_people_with_the_same_base_get_different_names() {
        let mut c = conn();
        let (u1, n1) = pick_name(&c, "abin").unwrap();
        create_pending(&mut c, &new_account(&u1, n1), 0, 100).unwrap();
        let (u2, n2) = pick_name(&c, "abin").unwrap();
        assert_ne!(u1, u2);
        assert_ne!(n1, n2);
        assert!(corvo_chat_proto::names::is_username(&u2));
    }

    #[test]
    fn create_pending_then_read_back() {
        let mut c = conn();
        let n = number(482_910_557);
        let id = create_pending(&mut c, &new_account("abin-0000", n), 1_000, 86_400).unwrap();
        let row = by_username(&c, "abin-0000", 1_001).unwrap().unwrap();
        assert_eq!(row.id, id);
        assert_eq!(row.pending_until, Some(1_000 + 86_400));
        let frame = row.to_frame();
        assert_eq!((frame.username.as_str(), frame.number, frame.display_name.as_str()), ("abin-0000", n, "Abin"));
        assert_eq!(by_id(&c, id, 1_001).unwrap().unwrap().username, "abin-0000");
        assert_eq!(by_number(&c, n, 1_001).unwrap().unwrap().id, id);
    }

    #[test]
    fn taken_name_at_create_is_reported() {
        let mut c = conn();
        create_pending(&mut c, &new_account("abin-0000", number(100_000_001)), 0, 100).unwrap();
        let again = create_pending(&mut c, &new_account("abin-0000", number(100_000_002)), 0, 100);
        assert!(matches!(again, Err(CreateError::Taken)));
        let same_number = create_pending(&mut c, &new_account("abin-0001", number(100_000_001)), 0, 100);
        assert!(matches!(same_number, Err(CreateError::Taken)));
    }

    #[test]
    fn expired_pending_account_disappears() {
        let mut c = conn();
        let id = create_pending(&mut c, &new_account("abin-0000", number(100_000_001)), 0, 100).unwrap();
        c.execute(
            "INSERT INTO sessions (account_id, device_id, kind, name, token_hash, first_seen, last_used) VALUES (?1, x'00', 'web', 'd', x'01', 0, 0)",
            [id],
        )
        .unwrap();
        c.execute("INSERT INTO invites (account_id, code, created_at) VALUES (?1, 'abc', 0)", [id]).unwrap();
        assert!(by_username(&c, "abin-0000", 99).unwrap().is_some());
        assert!(by_username(&c, "abin-0000", 100).unwrap().is_none());
        for table in ["accounts", "sessions", "invites"] {
            let n: i64 = c.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0)).unwrap();
            assert_eq!(n, 0, "{table}");
        }
        let active = create_pending(&mut c, &new_account("bob-0000", number(100_000_002)), 0, 100).unwrap();
        activate(&c, active, None, 0).unwrap();
        assert!(by_username(&c, "bob-0000", 10 * 365 * 86_400).unwrap().is_some());
    }

    #[test]
    fn five_tries_lock_for_15_minutes_ten_lock_for_good() {
        let mut c = conn();
        let id = create_pending(&mut c, &new_account("abin-0000", number(100_000_001)), 0, 100).unwrap();
        activate(&c, id, None, 0).unwrap();
        let limits = Limits::default();
        let now = 1_000;
        let state = |c: &Connection, at| lock_state(&by_id(c, id, at).unwrap().unwrap(), at);
        for _ in 0..4 {
            note_try(&c, id, now, &limits).unwrap();
        }
        assert_eq!(state(&c, now), LockState::Open);
        note_try(&c, id, now, &limits).unwrap();
        assert_eq!(state(&c, now), LockState::Until(now + 900));
        assert_eq!(state(&c, now + 900), LockState::Open);
        for _ in 0..4 {
            note_try(&c, id, now + 900, &limits).unwrap();
            assert_eq!(state(&c, now + 900), LockState::Open);
        }
        note_try(&c, id, now + 900, &limits).unwrap();
        let far = now + 10 * 365 * 86_400;
        assert_eq!(state(&c, far), LockState::Forever);
    }

    #[test]
    fn success_clears_everything() {
        let mut c = conn();
        let id = create_pending(&mut c, &new_account("abin-0000", number(100_000_001)), 0, 100).unwrap();
        activate(&c, id, None, 0).unwrap();
        for _ in 0..10 {
            note_try(&c, id, 0, &Limits::default()).unwrap();
        }
        note_success(&c, id).unwrap();
        let row = by_id(&c, id, 0).unwrap().unwrap();
        assert_eq!(lock_state(&row, 0), LockState::Open);
        assert_eq!(row.wrong_count, 0);
    }

    #[test]
    fn replace_swaps_one_record_and_copy() {
        let mut c = conn();
        let id = create_pending(&mut c, &new_account("abin-0000", number(100_000_001)), 0, 100).unwrap();
        replace(&c, id, Which::Password, &[9; 192], &[8; 40]).unwrap();
        assert_eq!(records(&c, id).unwrap(), (vec![9; 192], vec![2; 192]));
        assert_eq!(copy(&c, id, Which::Password).unwrap(), vec![8; 40]);
        assert_eq!(copy(&c, id, Which::Recovery).unwrap(), vec![4; 40]);
    }
}
