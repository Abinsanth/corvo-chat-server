//! Each account's pinned keys, one blob per number, encrypted by its client
//! with the data key so all its devices share them.

use corvo_chat_proto::Number;
use rusqlite::{Connection, OptionalExtension, params};

/// Stores or, with `None`, removes the pin for `number`.
pub fn set(c: &Connection, account: i64, number: Number, pin: Option<&[u8]>) -> rusqlite::Result<()> {
    match pin {
        Some(pin) => c.execute(
            "INSERT INTO pins (account_id, number, pin) VALUES (?1, ?2, ?3)
             ON CONFLICT (account_id, number) DO UPDATE SET pin = excluded.pin",
            params![account, number.digits(), pin],
        )?,
        None => {
            c.execute("DELETE FROM pins WHERE account_id = ?1 AND number = ?2", params![account, number.digits()])?
        }
    };
    Ok(())
}

pub fn get(c: &Connection, account: i64, number: Number) -> rusqlite::Result<Option<Vec<u8>>> {
    c.query_row("SELECT pin FROM pins WHERE account_id = ?1 AND number = ?2", params![account, number.digits()], |r| {
        r.get(0)
    })
    .optional()
}

pub fn count(c: &Connection, account: i64) -> rusqlite::Result<i64> {
    c.query_row("SELECT COUNT(*) FROM pins WHERE account_id = ?1", [account], |r| r.get(0))
}

/// Every pin of the account, by number.
pub fn all(c: &Connection, account: i64) -> rusqlite::Result<Vec<(Number, Vec<u8>)>> {
    let mut stmt = c.prepare("SELECT number, pin FROM pins WHERE account_id = ?1 ORDER BY number")?;
    stmt.query_map([account], |r| {
        let number: String = r.get(0)?;
        let number = Number::parse(&number)
            .map_err(|e| rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e)))?;
        Ok((number, r.get(1)?))
    })?
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    #[test]
    fn set_get_and_remove_a_pin() {
        let mut c = Connection::open_in_memory().unwrap();
        crate::db::migrate(&mut c).unwrap();
        c.execute(
            "INSERT INTO accounts (id, username, number, display_name, password_record, recovery_record,
               password_copy, recovery_copy, signing_key, exchange_key, key_signature, created_at)
             VALUES (1, 'a-0001', '1000000011', 'X', x'', x'', x'', x'', x'', x'', x'', 0)",
            [],
        )
        .unwrap();
        let (a, b) = (Number::from_body(100_000_000).unwrap(), Number::from_body(100_000_001).unwrap());
        assert_eq!(get(&c, 1, a).unwrap(), None);
        set(&c, 1, a, Some(&[1, 2])).unwrap();
        set(&c, 1, b, Some(&[3])).unwrap();
        set(&c, 1, a, Some(&[4])).unwrap();
        assert_eq!(get(&c, 1, a).unwrap(), Some(vec![4]));
        assert_eq!(all(&c, 1).unwrap().len(), 2);
        set(&c, 1, a, None).unwrap();
        assert_eq!(get(&c, 1, a).unwrap(), None);
        assert_eq!(all(&c, 1).unwrap(), vec![(b, vec![3])]);
    }
}
