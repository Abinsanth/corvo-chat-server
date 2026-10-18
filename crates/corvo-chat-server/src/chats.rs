//! Chats: one per pair of accounts, stored once, with each side's sealed
//! key, how far it received and read, and how much history it cleared.
//! Messages are ciphertext the server never reads.

use rusqlite::{Connection, OptionalExtension, Row, params};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChatRow {
    pub id: [u8; 16],
    /// The two members' account ids, `low < high`.
    pub low: i64,
    pub high: i64,
    pub creator: i64,
    pub signature: [u8; 64],
    pub last_seq: i64,
}

impl ChatRow {
    /// The member who isn't `me`.
    pub fn other(&self, me: i64) -> i64 {
        if me == self.low { self.high } else { self.low }
    }
}

/// One side of a chat.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MemberRow {
    pub account_id: i64,
    pub sealed_key: Vec<u8>,
    pub received_through: i64,
    pub read_through: i64,
    pub cleared_through: i64,
    /// The chat has been in this account's event log.
    pub announced: bool,
    /// The first seq of this side's current request, if it ever had one.
    pub request_from: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MessageRow {
    pub seq: i64,
    pub msg_id: [u8; 16],
    pub sender_id: i64,
    pub body: Vec<u8>,
    pub sent_at: i64,
    /// Never shown to the recipient: they blocked the sender, or it was
    /// over the request limits.
    pub withheld: bool,
}

impl MessageRow {
    /// Whether `viewer`, who cleared the history through `cleared_through`,
    /// sees this message.
    pub fn visible_to(&self, viewer: i64, cleared_through: i64) -> bool {
        self.seq > cleared_through && (!self.withheld || self.sender_id == viewer)
    }
}

#[derive(Debug)]
pub enum ChatError {
    IdTaken,
    /// The two already have a chat.
    PairTaken,
    Db(rusqlite::Error),
}

impl From<rusqlite::Error> for ChatError {
    fn from(e: rusqlite::Error) -> Self {
        ChatError::Db(e)
    }
}

/// Where `add_message` put a message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Added {
    pub seq: i64,
    pub sent_at: i64,
    /// `false` when the `msg_id` was already there: a resend.
    pub new: bool,
}

const CHAT_COLUMNS: &str = "id, low, high, creator, signature, last_seq";
const MESSAGE_COLUMNS: &str = "seq, msg_id, sender_id, body, sent_at, withheld";

fn fixed<const N: usize>(r: &Row, i: usize) -> rusqlite::Result<[u8; N]> {
    let bytes: Vec<u8> = r.get(i)?;
    bytes
        .try_into()
        .map_err(|_| rusqlite::Error::InvalidColumnType(i, "fixed-size blob".into(), rusqlite::types::Type::Blob))
}

fn chat_row(r: &Row) -> rusqlite::Result<ChatRow> {
    Ok(ChatRow {
        id: fixed(r, 0)?,
        low: r.get(1)?,
        high: r.get(2)?,
        creator: r.get(3)?,
        signature: fixed(r, 4)?,
        last_seq: r.get(5)?,
    })
}

fn message_row(r: &Row) -> rusqlite::Result<MessageRow> {
    Ok(MessageRow {
        seq: r.get(0)?,
        msg_id: fixed(r, 1)?,
        sender_id: r.get(2)?,
        body: r.get(3)?,
        sent_at: r.get(4)?,
        withheld: r.get(5)?,
    })
}

pub fn get(c: &Connection, id: &[u8; 16]) -> rusqlite::Result<Option<ChatRow>> {
    c.query_row(&format!("SELECT {CHAT_COLUMNS} FROM chats WHERE id = ?1"), [id.as_slice()], chat_row).optional()
}

/// The chat of `a` and `b`, in either order.
pub fn between(c: &Connection, a: i64, b: i64) -> rusqlite::Result<Option<ChatRow>> {
    c.query_row(
        &format!("SELECT {CHAT_COLUMNS} FROM chats WHERE low = ?1 AND high = ?2"),
        params![a.min(b), a.max(b)],
        chat_row,
    )
    .optional()
}

pub fn member(c: &Connection, id: &[u8; 16], account: i64) -> rusqlite::Result<Option<MemberRow>> {
    c.query_row(
        "SELECT account_id, sealed_key, received_through, read_through, cleared_through, announced, request_from
         FROM chat_members WHERE chat_id = ?1 AND account_id = ?2",
        params![id.as_slice(), account],
        |r| {
            Ok(MemberRow {
                account_id: r.get(0)?,
                sealed_key: r.get(1)?,
                received_through: r.get(2)?,
                read_through: r.get(3)?,
                cleared_through: r.get(4)?,
                announced: r.get(5)?,
                request_from: r.get(6)?,
            })
        },
    )
    .optional()
}

/// The chats announced to `account`, oldest first.
pub fn announced_for(c: &Connection, account: i64) -> rusqlite::Result<Vec<ChatRow>> {
    let mut stmt = c.prepare(&format!(
        "SELECT {} FROM chats JOIN chat_members m ON m.chat_id = chats.id
         WHERE m.account_id = ?1 AND m.announced = 1 ORDER BY chats.created_at, chats.id",
        CHAT_COLUMNS.split(", ").map(|col| format!("chats.{col}")).collect::<Vec<_>>().join(", ")
    ))?;
    stmt.query_map([account], chat_row)?.collect()
}

/// Makes the chat of `creator` and `other`, with each one's sealed key.
#[expect(clippy::too_many_arguments, reason = "a chat is made from all of these at once")]
pub fn create(
    c: &Connection,
    id: &[u8; 16],
    creator: i64,
    other: i64,
    creator_key: &[u8],
    other_key: &[u8],
    signature: &[u8; 64],
    now: i64,
) -> Result<(), ChatError> {
    if get(c, id)?.is_some() {
        return Err(ChatError::IdTaken);
    }
    if between(c, creator, other)?.is_some() {
        return Err(ChatError::PairTaken);
    }
    c.execute(
        "INSERT INTO chats (id, low, high, creator, signature, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![id.as_slice(), creator.min(other), creator.max(other), creator, signature.as_slice(), now],
    )?;
    for (account, key) in [(creator, creator_key), (other, other_key)] {
        c.execute(
            "INSERT INTO chat_members (chat_id, account_id, sealed_key) VALUES (?1, ?2, ?3)",
            params![id.as_slice(), account, key],
        )?;
    }
    Ok(())
}

/// Marks the chat as shown to `account`; `true` the first time.
pub fn announce(c: &Connection, id: &[u8; 16], account: i64) -> rusqlite::Result<bool> {
    let changed = c.execute(
        "UPDATE chat_members SET announced = 1 WHERE chat_id = ?1 AND account_id = ?2 AND announced = 0",
        params![id.as_slice(), account],
    )?;
    Ok(changed > 0)
}

/// Stores a message as the chat's next seq. A `msg_id` the chat already has
/// is a resend: nothing is stored, and the first one's place comes back.
pub fn add_message(
    c: &Connection,
    id: &[u8; 16],
    msg_id: &[u8; 16],
    sender: i64,
    body: &[u8],
    now: i64,
    withheld: bool,
) -> rusqlite::Result<Added> {
    if let Some(known) = by_msg_id(c, id, msg_id)? {
        return Ok(known);
    }
    let seq: i64 = c.query_row(
        "UPDATE chats SET last_seq = last_seq + 1 WHERE id = ?1 RETURNING last_seq",
        [id.as_slice()],
        |r| r.get(0),
    )?;
    c.execute(
        "INSERT INTO messages (chat_id, seq, msg_id, sender_id, body, sent_at, withheld)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![id.as_slice(), seq, msg_id.as_slice(), sender, body, now, withheld],
    )?;
    Ok(Added { seq, sent_at: now, new: true })
}

/// Where the message with this `msg_id` is, if the chat has it.
pub fn by_msg_id(c: &Connection, id: &[u8; 16], msg_id: &[u8; 16]) -> rusqlite::Result<Option<Added>> {
    c.query_row(
        "SELECT seq, sent_at FROM messages WHERE chat_id = ?1 AND msg_id = ?2",
        params![id.as_slice(), msg_id.as_slice()],
        |r| Ok(Added { seq: r.get(0)?, sent_at: r.get(1)?, new: false }),
    )
    .optional()
}

pub fn message(c: &Connection, id: &[u8; 16], seq: i64) -> rusqlite::Result<Option<MessageRow>> {
    c.query_row(
        &format!("SELECT {MESSAGE_COLUMNS} FROM messages WHERE chat_id = ?1 AND seq = ?2"),
        params![id.as_slice(), seq],
        message_row,
    )
    .optional()
}

/// The newest `limit` messages `viewer` sees before `before`, oldest first,
/// and whether nothing older is visible.
pub fn visible(
    c: &Connection,
    id: &[u8; 16],
    viewer: i64,
    before: i64,
    limit: usize,
) -> rusqlite::Result<(Vec<MessageRow>, bool)> {
    let Some(me) = member(c, id, viewer)? else { return Ok((Vec::new(), true)) };
    let mut stmt = c.prepare(&format!(
        "SELECT {MESSAGE_COLUMNS} FROM messages
         WHERE chat_id = ?1 AND seq < ?2 AND seq > ?3 AND (withheld = 0 OR sender_id = ?4)
         ORDER BY seq DESC LIMIT ?5"
    ))?;
    let mut rows: Vec<MessageRow> = stmt
        .query_map(params![id.as_slice(), before, me.cleared_through, viewer, limit as i64 + 1], message_row)?
        .collect::<rusqlite::Result<_>>()?;
    let complete = rows.len() <= limit;
    rows.truncate(limit);
    rows.reverse();
    Ok((rows, complete))
}

/// How many of `sender`'s messages `viewer` sees in the request it has now:
/// what that request has used of its message limit.
pub fn request_messages(c: &Connection, id: &[u8; 16], sender: i64, viewer: i64) -> rusqlite::Result<i64> {
    let (cleared, from) = member(c, id, viewer)?.map_or((0, 0), |m| (m.cleared_through, m.request_from));
    c.query_row(
        "SELECT COUNT(*) FROM messages
         WHERE chat_id = ?1 AND sender_id = ?2 AND withheld = 0 AND seq > ?3 AND seq >= ?4",
        params![id.as_slice(), sender, cleared, from],
        |r| r.get(0),
    )
}

/// A request to `viewer` starts at `seq`: its message limit counts from there.
pub fn start_request(c: &Connection, id: &[u8; 16], viewer: i64, seq: i64) -> rusqlite::Result<()> {
    c.execute(
        "UPDATE chat_members SET request_from = ?3 WHERE chat_id = ?1 AND account_id = ?2",
        params![id.as_slice(), viewer, seq],
    )?;
    Ok(())
}

/// The highest seq `through` may name: the chat's last.
fn clamp(c: &Connection, id: &[u8; 16], through: i64) -> rusqlite::Result<i64> {
    let last: i64 = c.query_row("SELECT last_seq FROM chats WHERE id = ?1", [id.as_slice()], |r| r.get(0))?;
    Ok(through.min(last))
}

/// A device of `account` has everything through `through`. Only moves
/// forward; `true` if it moved.
pub fn received(c: &Connection, id: &[u8; 16], account: i64, through: i64) -> rusqlite::Result<bool> {
    let through = clamp(c, id, through)?;
    let changed = c.execute(
        "UPDATE chat_members SET received_through = ?3
         WHERE chat_id = ?1 AND account_id = ?2 AND received_through < ?3",
        params![id.as_slice(), account, through],
    )?;
    Ok(changed > 0)
}

/// `account` read through `through`, so it also received it. Only moves
/// forward; `true` if the read position moved.
pub fn read(c: &Connection, id: &[u8; 16], account: i64, through: i64) -> rusqlite::Result<bool> {
    let through = clamp(c, id, through)?;
    let changed = c.execute(
        "UPDATE chat_members SET read_through = ?3, received_through = MAX(received_through, ?3)
         WHERE chat_id = ?1 AND account_id = ?2 AND read_through < ?3",
        params![id.as_slice(), account, through],
    )?;
    Ok(changed > 0)
}

/// Clears `account`'s side of the history up to now, deletes what both
/// sides have cleared, and returns the new `cleared_through`.
pub fn clear(c: &Connection, id: &[u8; 16], account: i64) -> rusqlite::Result<i64> {
    let last = clamp(c, id, i64::MAX)?;
    c.execute(
        "UPDATE chat_members SET cleared_through = ?3 WHERE chat_id = ?1 AND account_id = ?2",
        params![id.as_slice(), account, last],
    )?;
    c.execute(
        "DELETE FROM messages WHERE chat_id = ?1
           AND seq <= (SELECT MIN(cleared_through) FROM chat_members WHERE chat_id = ?1)",
        [id.as_slice()],
    )?;
    Ok(last)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conn() -> Connection {
        crate::testing::conn_with_accounts(3)
    }

    fn chat(c: &Connection, n: u8, a: i64, b: i64) -> Result<(), ChatError> {
        create(c, &[n; 16], a, b, &[a as u8; 104], &[b as u8; 104], &[9; 64], 100)
    }

    fn send(c: &Connection, sender: i64, n: u8, withheld: bool) -> Added {
        add_message(c, &[1; 16], &[n; 16], sender, b"ciphertext", 100 + i64::from(n), withheld).unwrap()
    }

    #[test]
    fn one_chat_per_pair() {
        let c = conn();
        chat(&c, 1, 1, 2).unwrap();
        assert!(matches!(chat(&c, 2, 2, 1), Err(ChatError::PairTaken)));
        assert!(matches!(chat(&c, 1, 1, 3), Err(ChatError::IdTaken)));
        let row = between(&c, 2, 1).unwrap().unwrap();
        assert_eq!((row.id, row.creator, row.other(1), row.other(2)), ([1; 16], 1, 2, 1));
        assert_eq!(member(&c, &[1; 16], 1).unwrap().unwrap().sealed_key, vec![1; 104]);
        assert_eq!(member(&c, &[1; 16], 2).unwrap().unwrap().sealed_key, vec![2; 104]);
        assert!(member(&c, &[1; 16], 3).unwrap().is_none());
        assert!(get(&c, &[2; 16]).unwrap().is_none());
    }

    #[test]
    fn announce_once() {
        let c = conn();
        chat(&c, 1, 1, 2).unwrap();
        assert!(announced_for(&c, 2).unwrap().is_empty());
        assert!(announce(&c, &[1; 16], 2).unwrap());
        assert!(!announce(&c, &[1; 16], 2).unwrap());
        assert_eq!(announced_for(&c, 2).unwrap().len(), 1);
        assert!(announced_for(&c, 3).unwrap().is_empty());
    }

    #[test]
    fn messages_get_increasing_seqs_and_a_resend_is_not_stored() {
        let c = conn();
        chat(&c, 1, 1, 2).unwrap();
        assert_eq!(send(&c, 1, 1, false), Added { seq: 1, sent_at: 101, new: true });
        assert_eq!(send(&c, 2, 2, false), Added { seq: 2, sent_at: 102, new: true });
        let again = add_message(&c, &[1; 16], &[1; 16], 1, b"ciphertext", 500, false).unwrap();
        assert_eq!(again, Added { seq: 1, sent_at: 101, new: false });
        assert_eq!(get(&c, &[1; 16]).unwrap().unwrap().last_seq, 2);
        let first = message(&c, &[1; 16], 1).unwrap().unwrap();
        assert_eq!((first.msg_id, first.sender_id, first.body.as_slice()), ([1; 16], 1, b"ciphertext".as_slice()));
    }

    #[test]
    fn withheld_messages_are_visible_only_to_their_sender() {
        let c = conn();
        chat(&c, 1, 1, 2).unwrap();
        send(&c, 1, 1, true);
        assert_eq!(visible(&c, &[1; 16], 1, i64::MAX, 50).unwrap().0.len(), 1);
        assert!(visible(&c, &[1; 16], 2, i64::MAX, 50).unwrap().0.is_empty());
        let row = message(&c, &[1; 16], 1).unwrap().unwrap();
        assert!(row.withheld && row.visible_to(1, 0) && !row.visible_to(2, 0));
    }

    #[test]
    fn visible_pages_back_from_before() {
        let c = conn();
        chat(&c, 1, 1, 2).unwrap();
        for n in 1..=120u8 {
            send(&c, 1 + i64::from(n % 2), n, false);
        }
        let seqs = |before| {
            let (rows, complete) = visible(&c, &[1; 16], 2, before, 50).unwrap();
            (rows.iter().map(|m| m.seq).collect::<Vec<_>>(), complete)
        };
        assert_eq!(seqs(i64::MAX), ((71..=120).collect(), false));
        assert_eq!(seqs(71), ((21..=70).collect(), false));
        assert_eq!(seqs(21), ((1..=20).collect(), true));
    }

    #[test]
    fn request_count_skips_withheld_and_cleared() {
        let c = conn();
        chat(&c, 1, 1, 2).unwrap();
        send(&c, 1, 1, false);
        send(&c, 1, 2, false);
        clear(&c, &[1; 16], 2).unwrap();
        send(&c, 1, 3, false);
        send(&c, 1, 4, true);
        send(&c, 2, 5, false);
        assert_eq!(request_messages(&c, &[1; 16], 1, 2).unwrap(), 1);
        // A request starting at seq 6 counts only from there.
        send(&c, 1, 6, false);
        start_request(&c, &[1; 16], 2, 6).unwrap();
        assert_eq!(request_messages(&c, &[1; 16], 1, 2).unwrap(), 1);
    }

    #[test]
    fn cursors_only_move_forward_and_clamp() {
        let c = conn();
        chat(&c, 1, 1, 2).unwrap();
        for n in 1..=3 {
            send(&c, 1, n, false);
        }
        assert!(read(&c, &[1; 16], 2, 5).unwrap());
        let me = member(&c, &[1; 16], 2).unwrap().unwrap();
        assert_eq!((me.received_through, me.read_through), (3, 3));
        assert!(!received(&c, &[1; 16], 2, 2).unwrap());
        assert!(!read(&c, &[1; 16], 2, 1).unwrap());
        assert!(received(&c, &[1; 16], 1, 2).unwrap());
        assert_eq!(member(&c, &[1; 16], 1).unwrap().unwrap().received_through, 2);
    }

    #[test]
    fn clearing_both_sides_deletes_the_messages() {
        let c = conn();
        chat(&c, 1, 1, 2).unwrap();
        for n in 1..=3 {
            send(&c, 1, n, false);
        }
        assert_eq!(clear(&c, &[1; 16], 1).unwrap(), 3);
        let count = || c.query_row("SELECT COUNT(*) FROM messages", [], |r| r.get::<_, i64>(0)).unwrap();
        assert_eq!(count(), 3);
        assert!(visible(&c, &[1; 16], 1, i64::MAX, 50).unwrap().0.is_empty());
        send(&c, 2, 4, false);
        assert_eq!(clear(&c, &[1; 16], 2).unwrap(), 4);
        assert_eq!(count(), 1);
        assert_eq!(get(&c, &[1; 16]).unwrap().unwrap().last_seq, 4);
        assert_eq!(member(&c, &[1; 16], 1).unwrap().unwrap().cleared_through, 3);
    }
}
