//! Invite codes: six digits, one per pending account, with a try counter.

use corvo_chat_proto::Number;
use corvo_chat_proto::frames::InviteInfo;
use rusqlite::{Connection, OptionalExtension, params};

#[derive(Debug)]
pub enum InviteError {
    TooMany,
    Wrong,
    Dead,
    Db(rusqlite::Error),
}

impl From<rusqlite::Error> for InviteError {
    fn from(e: rusqlite::Error) -> Self {
        InviteError::Db(e)
    }
}

/// Makes a fresh code for the pending account, replacing any earlier one
/// (tries reset). The admin (`inviter: None`) has no cap; a user may have
/// `open_max` open invites for other accounts.
pub fn create(
    conn: &Connection,
    pending_id: i64,
    inviter: Option<i64>,
    now: i64,
    open_max: usize,
) -> Result<String, InviteError> {
    if let Some(inviter) = inviter {
        let open: i64 = conn.query_row(
            "SELECT COUNT(*) FROM invites i JOIN accounts a ON a.id = i.account_id
             WHERE i.inviter_id = ?1 AND i.account_id != ?2 AND a.pending_until > ?3",
            params![inviter, pending_id, now],
            |r| r.get(0),
        )?;
        if open >= open_max as i64 {
            return Err(InviteError::TooMany);
        }
    }
    let code = format!("{:06}", rand::random_range(0..1_000_000));
    conn.execute(
        "INSERT OR REPLACE INTO invites (account_id, inviter_id, code, wrong_tries, created_at)
         VALUES (?1, ?2, ?3, 0, ?4)",
        params![pending_id, inviter, code, now],
    )?;
    Ok(code)
}

/// The inviter's open invites, oldest first. Each expires with its pending
/// account.
pub fn open(conn: &Connection, inviter: i64, now: i64) -> rusqlite::Result<Vec<InviteInfo>> {
    let mut stmt = conn.prepare(
        "SELECT a.number, a.display_name, i.code, a.pending_until
         FROM invites i JOIN accounts a ON a.id = i.account_id
         WHERE i.inviter_id = ?1 AND a.pending_until > ?2
         ORDER BY i.created_at, i.account_id",
    )?;
    stmt.query_map(params![inviter, now], |r| {
        let digits: String = r.get(0)?;
        let number = Number::parse(&digits)
            .map_err(|_| rusqlite::Error::InvalidColumnType(0, "number".into(), rusqlite::types::Type::Text))?;
        Ok(InviteInfo { number, display_name: r.get(1)?, code: r.get(2)?, expires_at: r.get(3)? })
    })?
    .collect()
}

/// Checks a code typed by the pending user. A match returns the inviter and
/// uses the invite up. The `tries`th miss kills the code.
pub fn redeem(
    conn: &Connection,
    pending_id: i64,
    code: &str,
    _now: i64,
    tries: i64,
) -> Result<Option<i64>, InviteError> {
    let found: Option<(Option<i64>, String, i64)> = conn
        .query_row("SELECT inviter_id, code, wrong_tries FROM invites WHERE account_id = ?1", [pending_id], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })
        .optional()?;
    let Some((inviter, stored, wrong)) = found else {
        return Err(InviteError::Dead);
    };
    let typed: String = code.chars().filter(|c| !c.is_whitespace()).collect();
    if typed == stored {
        conn.execute("DELETE FROM invites WHERE account_id = ?1", [pending_id])?;
        return Ok(inviter);
    }
    if wrong + 1 >= tries {
        conn.execute("DELETE FROM invites WHERE account_id = ?1", [pending_id])?;
        return Err(InviteError::Dead);
    }
    conn.execute("UPDATE invites SET wrong_tries = wrong_tries + 1 WHERE account_id = ?1", [pending_id])?;
    Err(InviteError::Wrong)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conn() -> Connection {
        let mut c = Connection::open_in_memory().unwrap();
        c.pragma_update(None, "foreign_keys", true).unwrap();
        crate::db::migrate(&mut c).unwrap();
        c
    }

    /// Inserts an account; `pending_until: None` makes it active.
    fn account(c: &Connection, id: i64, pending_until: Option<i64>) {
        c.execute(
            "INSERT INTO accounts (id, username, number, display_name, password_record, recovery_record,
               password_copy, recovery_copy, signing_key, exchange_key, key_signature, created_at, pending_until)
             VALUES (?1, ?2, ?3, ?4, x'', x'', x'', x'', x'', x'', x'', 0, ?5)",
            params![
                id,
                format!("user-{id:04}"),
                Number::from_body(100_000_000 + id as u32).unwrap().digits(),
                format!("User {id}"),
                pending_until
            ],
        )
        .unwrap();
    }

    /// Forces a known code so tests can type it with odd spacing.
    fn set_code(c: &Connection, id: i64, code: &str) {
        c.execute("UPDATE invites SET code = ?2 WHERE account_id = ?1", params![id, code]).unwrap();
    }

    #[test]
    fn code_is_six_digits_and_redeems_once() {
        let c = conn();
        account(&c, 1, None);
        account(&c, 2, Some(1_000));
        let code = create(&c, 2, Some(1), 0, 5).unwrap();
        assert_eq!(code.len(), 6);
        assert!(code.bytes().all(|b| b.is_ascii_digit()));
        set_code(&c, 2, "482913");
        assert_eq!(redeem(&c, 2, " 482 913 ", 1, 5).unwrap(), Some(1));
        assert!(matches!(redeem(&c, 2, "482913", 1, 5), Err(InviteError::Dead)));
    }

    #[test]
    fn fifth_wrong_code_kills_it() {
        let c = conn();
        account(&c, 1, None);
        account(&c, 2, Some(1_000));
        create(&c, 2, Some(1), 0, 5).unwrap();
        set_code(&c, 2, "482913");
        for _ in 0..4 {
            assert!(matches!(redeem(&c, 2, "000000", 1, 5), Err(InviteError::Wrong)));
        }
        assert!(matches!(redeem(&c, 2, "000000", 1, 5), Err(InviteError::Dead)));
        assert!(matches!(redeem(&c, 2, "482913", 1, 5), Err(InviteError::Dead)));
    }

    #[test]
    fn inviting_again_replaces_the_code() {
        let c = conn();
        account(&c, 1, None);
        account(&c, 2, Some(1_000));
        create(&c, 2, Some(1), 0, 5).unwrap();
        set_code(&c, 2, "111111");
        assert!(matches!(redeem(&c, 2, "000000", 1, 5), Err(InviteError::Wrong)));
        create(&c, 2, Some(1), 5, 5).unwrap();
        set_code(&c, 2, "222222");
        let tries: i64 = c.query_row("SELECT wrong_tries FROM invites", [], |r| r.get(0)).unwrap();
        assert_eq!(tries, 0);
        assert!(matches!(redeem(&c, 2, "111111", 6, 5), Err(InviteError::Wrong)));
        assert_eq!(redeem(&c, 2, "222222", 6, 5).unwrap(), Some(1));
    }

    #[test]
    fn sixth_open_invite_is_refused() {
        let c = conn();
        account(&c, 1, None);
        for id in 2..=7 {
            account(&c, id, Some(1_000));
        }
        for id in 2..=6 {
            create(&c, id, Some(1), 0, 5).unwrap();
        }
        assert!(matches!(create(&c, 7, Some(1), 0, 5), Err(InviteError::TooMany)));
        create(&c, 3, Some(1), 1, 5).unwrap();
        // The admin has no cap of five.
        for id in 2..=7 {
            create(&c, id, None, 2, 5).unwrap();
        }
        for id in 8..=12 {
            account(&c, id, Some(1_000));
        }
        for id in 8..=12 {
            create(&c, id, None, 2, 5).unwrap();
        }
        assert_eq!(
            c.query_row::<i64, _, _>("SELECT COUNT(*) FROM invites WHERE inviter_id IS NULL", [], |r| r.get(0))
                .unwrap(),
            11
        );
    }

    #[test]
    fn open_invites_expire_with_the_pending_account() {
        let c = conn();
        account(&c, 1, None);
        account(&c, 2, Some(100));
        account(&c, 3, Some(200));
        create(&c, 2, Some(1), 0, 5).unwrap();
        create(&c, 3, Some(1), 1, 5).unwrap();
        let both = open(&c, 1, 50).unwrap();
        assert_eq!(both.len(), 2);
        assert_eq!(both[0].display_name, "User 2");
        assert_eq!(both[0].expires_at, 100);
        let one = open(&c, 1, 100).unwrap();
        assert_eq!(one.len(), 1);
        assert_eq!(one[0].display_name, "User 3");
        assert!(open(&c, 1, 200).unwrap().is_empty());
        // Expired invites no longer count against the cap.
        for id in 4..=7 {
            account(&c, id, Some(1_000));
            create(&c, id, Some(1), 150, 5).unwrap();
        }
    }
}
