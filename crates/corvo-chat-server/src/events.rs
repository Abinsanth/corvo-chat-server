//! Each account's event log. A row only names what changed; the frame is
//! rendered from the current state when it's sent, so replaying an old
//! event is always safe.

use corvo_chat_proto::Number;
use rusqlite::{Connection, Row, params};

use crate::hub::{Hub, Push};

/// What changed for an account.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Ev {
    /// A message in one of its chats, its own included.
    Message { chat: [u8; 16], seq: i64 },
    /// A chat was announced to it.
    Chat { chat: [u8; 16] },
    /// Its standing with someone, or what it may see of them, changed.
    Person { other: i64 },
    /// The other side's marks moved.
    Marks { chat: [u8; 16] },
    /// It read further.
    Read { chat: [u8; 16] },
    /// Its side of a chat's history was cleared.
    Cleared { chat: [u8; 16] },
    /// Its own display name changed.
    Profile,
    /// Its devices changed.
    Devices,
    /// A pinned key changed.
    Pin { number: Number },
}

/// The columns an event is stored in: kind, chat, other, msg_seq, number.
type Columns = (&'static str, Option<[u8; 16]>, Option<i64>, Option<i64>, Option<String>);

fn columns(ev: &Ev) -> Columns {
    match ev {
        Ev::Message { chat, seq } => ("message", Some(*chat), None, Some(*seq), None),
        Ev::Chat { chat } => ("chat", Some(*chat), None, None, None),
        Ev::Person { other } => ("person", None, Some(*other), None, None),
        Ev::Marks { chat } => ("marks", Some(*chat), None, None, None),
        Ev::Read { chat } => ("read", Some(*chat), None, None, None),
        Ev::Cleared { chat } => ("cleared", Some(*chat), None, None, None),
        Ev::Profile => ("profile", None, None, None, None),
        Ev::Devices => ("devices", None, None, None, None),
        Ev::Pin { number } => ("pin", None, None, None, Some(number.digits())),
    }
}

fn row(r: &Row) -> rusqlite::Result<(i64, Ev)> {
    let bad = |i: usize| rusqlite::Error::InvalidColumnType(i, "event".into(), rusqlite::types::Type::Null);
    let seq: i64 = r.get(0)?;
    let kind: String = r.get(1)?;
    let chat = || -> rusqlite::Result<[u8; 16]> {
        let bytes: Vec<u8> = r.get(2)?;
        bytes.try_into().map_err(|_| bad(2))
    };
    let ev = match kind.as_str() {
        "message" => Ev::Message { chat: chat()?, seq: r.get(4)? },
        "chat" => Ev::Chat { chat: chat()? },
        "person" => Ev::Person { other: r.get(3)? },
        "marks" => Ev::Marks { chat: chat()? },
        "read" => Ev::Read { chat: chat()? },
        "cleared" => Ev::Cleared { chat: chat()? },
        "profile" => Ev::Profile,
        "devices" => Ev::Devices,
        "pin" => {
            let number: String = r.get(5)?;
            Ev::Pin { number: Number::parse(&number).map_err(|_| bad(5))? }
        }
        _ => return Err(bad(1)),
    };
    Ok((seq, ev))
}

/// The account's last event seq, 0 for none.
pub fn head(c: &Connection, account: i64) -> rusqlite::Result<i64> {
    c.query_row("SELECT COALESCE(MAX(seq), 0) FROM events WHERE account_id = ?1", [account], |r| r.get(0))
}

/// Adds `ev` to the account's log and returns its seq.
pub fn append(c: &Connection, account: i64, ev: &Ev) -> rusqlite::Result<i64> {
    let seq = head(c, account)? + 1;
    let (kind, chat, other, msg_seq, number) = columns(ev);
    c.execute(
        "INSERT INTO events (account_id, seq, kind, chat_id, other_id, msg_seq, number)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![account, seq, kind, chat.map(|id| id.to_vec()), other, msg_seq, number],
    )?;
    Ok(seq)
}

/// The account's devices changed: a `Devices` event, and its devices told.
pub fn note_devices(c: &Connection, hub: &Hub, account: i64) -> rusqlite::Result<()> {
    append(c, account, &Ev::Devices)?;
    hub.push(account, Push::Events);
    Ok(())
}

/// Up to `limit` events after `after`, in order.
pub fn since(c: &Connection, account: i64, after: i64, limit: usize) -> rusqlite::Result<Vec<(i64, Ev)>> {
    let mut stmt = c.prepare(
        "SELECT seq, kind, chat_id, other_id, msg_seq, number FROM events
         WHERE account_id = ?1 AND seq > ?2 ORDER BY seq LIMIT ?3",
    )?;
    stmt.query_map(params![account, after, limit as i64], row)?.collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seqs_are_per_account_and_start_at_one() {
        let c = crate::testing::conn_with_accounts(3);
        assert_eq!(append(&c, 1, &Ev::Profile).unwrap(), 1);
        assert_eq!(append(&c, 1, &Ev::Devices).unwrap(), 2);
        assert_eq!(append(&c, 2, &Ev::Profile).unwrap(), 1);
        assert_eq!((head(&c, 1).unwrap(), head(&c, 2).unwrap(), head(&c, 3).unwrap()), (2, 1, 0));
    }

    #[test]
    fn since_returns_each_kind_back() {
        let c = crate::testing::conn_with_accounts(2);
        let all = vec![
            Ev::Message { chat: [1; 16], seq: 4 },
            Ev::Chat { chat: [1; 16] },
            Ev::Person { other: 2 },
            Ev::Marks { chat: [1; 16] },
            Ev::Read { chat: [1; 16] },
            Ev::Cleared { chat: [1; 16] },
            Ev::Profile,
            Ev::Devices,
            Ev::Pin { number: Number::from_body(100_000_000).unwrap() },
        ];
        for ev in &all {
            append(&c, 1, ev).unwrap();
        }
        let back = since(&c, 1, 0, 100).unwrap();
        assert_eq!(back, all.iter().cloned().enumerate().map(|(i, ev)| (i as i64 + 1, ev)).collect::<Vec<_>>());
        let page: Vec<i64> = since(&c, 1, 2, 3).unwrap().into_iter().map(|(seq, _)| seq).collect();
        assert_eq!(page, vec![3, 4, 5]);
        assert!(since(&c, 2, 0, 100).unwrap().is_empty());
    }
}
