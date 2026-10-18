//! Each account's one-sided standing with another: `asked`, `request`,
//! `contact` or `blocked`, or no row for a stranger. What X shows Y (name,
//! marks, presence) depends on X's row about Y.

use corvo_chat_proto::chat::Relation;
use rusqlite::{Connection, OptionalExtension, params};

fn text(relation: Relation) -> Option<&'static str> {
    match relation {
        Relation::None => None,
        Relation::Asked => Some("asked"),
        Relation::Request => Some("request"),
        Relation::Contact => Some("contact"),
        Relation::Blocked => Some("blocked"),
    }
}

fn from_text(state: &str) -> Relation {
    match state {
        "asked" => Relation::Asked,
        "request" => Relation::Request,
        "contact" => Relation::Contact,
        "blocked" => Relation::Blocked,
        _ => Relation::None,
    }
}

/// `me`'s row about `other`.
pub fn get(c: &Connection, me: i64, other: i64) -> rusqlite::Result<Relation> {
    let state: Option<String> = c
        .query_row("SELECT state FROM relations WHERE account_id = ?1 AND other_id = ?2", params![me, other], |r| {
            r.get(0)
        })
        .optional()?;
    Ok(state.map_or(Relation::None, |s| from_text(&s)))
}

/// Sets `me`'s row about `other`; `Relation::None` deletes it.
pub fn set(c: &Connection, me: i64, other: i64, relation: Relation, now: i64) -> rusqlite::Result<()> {
    match text(relation) {
        Some(state) => c.execute(
            "INSERT INTO relations (account_id, other_id, state, since) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT (account_id, other_id) DO UPDATE SET state = excluded.state, since = excluded.since",
            params![me, other, state, now],
        )?,
        None => c.execute("DELETE FROM relations WHERE account_id = ?1 AND other_id = ?2", params![me, other])?,
    };
    Ok(())
}

/// Both rows `contact`.
pub fn befriend(c: &Connection, a: i64, b: i64, now: i64) -> rusqlite::Result<()> {
    set(c, a, b, Relation::Contact, now)?;
    set(c, b, a, Relation::Contact, now)
}

/// Every row `me` has, by the other's id.
pub fn rows(c: &Connection, me: i64) -> rusqlite::Result<Vec<(i64, Relation)>> {
    let mut stmt = c.prepare("SELECT other_id, state FROM relations WHERE account_id = ?1 ORDER BY other_id")?;
    stmt.query_map([me], |r| Ok((r.get(0)?, from_text(&r.get::<_, String>(1)?))))?.collect()
}

/// How many requests `me` hasn't answered.
pub fn open_requests(c: &Connection, me: i64) -> rusqlite::Result<i64> {
    c.query_row("SELECT COUNT(*) FROM relations WHERE account_id = ?1 AND state = 'request'", [me], |r| r.get(0))
}

/// The accounts that are `me`'s contacts both ways.
pub fn mutual_contacts(c: &Connection, me: i64) -> rusqlite::Result<Vec<i64>> {
    let mut stmt = c.prepare(
        "SELECT mine.other_id FROM relations mine
         JOIN relations theirs ON theirs.account_id = mine.other_id AND theirs.other_id = mine.account_id
         WHERE mine.account_id = ?1 AND mine.state = 'contact' AND theirs.state = 'contact'
         ORDER BY mine.other_id",
    )?;
    stmt.query_map([me], |r| r.get(0))?.collect()
}

/// Whether `viewer` may see `owner`'s display name.
pub fn shows_name(c: &Connection, owner: i64, viewer: i64) -> rusqlite::Result<bool> {
    Ok(matches!(get(c, owner, viewer)?, Relation::Asked | Relation::Contact))
}

/// Whether `viewer` may see how far `owner` has received and read.
pub fn shows_marks(c: &Connection, owner: i64, viewer: i64) -> rusqlite::Result<bool> {
    Ok(get(c, owner, viewer)? == Relation::Contact)
}

#[cfg(test)]
mod tests {
    use super::*;
    use corvo_chat_proto::chat::Relation;
    use rusqlite::Connection;

    fn conn() -> Connection {
        crate::testing::conn_with_accounts(4)
    }

    #[test]
    fn missing_row_is_none_and_none_deletes() {
        let c = conn();
        assert_eq!(get(&c, 1, 2).unwrap(), Relation::None);
        set(&c, 1, 2, Relation::Asked, 10).unwrap();
        assert_eq!(get(&c, 1, 2).unwrap(), Relation::Asked);
        assert_eq!(get(&c, 2, 1).unwrap(), Relation::None);
        set(&c, 1, 2, Relation::None, 11).unwrap();
        assert_eq!(get(&c, 1, 2).unwrap(), Relation::None);
        let rows: i64 = c.query_row("SELECT COUNT(*) FROM relations", [], |r| r.get(0)).unwrap();
        assert_eq!(rows, 0);
    }

    #[test]
    fn befriend_makes_both_sides_contacts() {
        let c = conn();
        set(&c, 1, 2, Relation::Asked, 10).unwrap();
        befriend(&c, 1, 2, 11).unwrap();
        assert_eq!((get(&c, 1, 2).unwrap(), get(&c, 2, 1).unwrap()), (Relation::Contact, Relation::Contact));
        assert_eq!(rows(&c, 1).unwrap(), vec![(2, Relation::Contact)]);
    }

    #[test]
    fn mutual_contacts_needs_both_rows() {
        let c = conn();
        befriend(&c, 1, 2, 10).unwrap();
        set(&c, 1, 3, Relation::Contact, 10).unwrap();
        set(&c, 3, 1, Relation::Asked, 10).unwrap();
        assert_eq!(mutual_contacts(&c, 1).unwrap(), vec![2]);
        assert_eq!(mutual_contacts(&c, 3).unwrap(), Vec::<i64>::new());
    }

    #[test]
    fn shows_name_for_asked_and_contact_only() {
        let c = conn();
        for relation in [Relation::None, Relation::Asked, Relation::Request, Relation::Contact, Relation::Blocked] {
            set(&c, 1, 2, relation, 10).unwrap();
            let name = matches!(relation, Relation::Asked | Relation::Contact);
            assert_eq!(shows_name(&c, 1, 2).unwrap(), name, "{relation:?}");
            assert_eq!(shows_marks(&c, 1, 2).unwrap(), relation == Relation::Contact, "{relation:?}");
        }
    }

    #[test]
    fn open_requests_counts_request_rows() {
        let c = conn();
        set(&c, 1, 2, Relation::Request, 10).unwrap();
        set(&c, 1, 3, Relation::Request, 10).unwrap();
        set(&c, 1, 4, Relation::Asked, 10).unwrap();
        set(&c, 2, 1, Relation::Request, 10).unwrap();
        assert_eq!(open_requests(&c, 1).unwrap(), 2);
    }
}
