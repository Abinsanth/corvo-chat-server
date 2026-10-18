//! The admin subcommands that work on the database directly: `activate`,
//! `invite`, `stats`, `tree`, `ban` and `unban`.

use std::path::Path;

use anyhow::{Context, bail};
use corvo_chat_proto::Number;

use crate::{accounts, clock::Clock, db, invites, limits::Limits};

/// Finds the pending account for a typed number, or fails with
/// `not a pending account`.
fn pending(conn: &rusqlite::Connection, number: &str, now: i64) -> anyhow::Result<accounts::AccountRow> {
    let number = Number::parse(number).context("not a valid number")?;
    match accounts::by_number(conn, number, now)? {
        Some(row) if row.pending_until.is_some() => Ok(row),
        _ => bail!("{number} is not a pending account"),
    }
}

/// Activates a pending account with no inviter. Returns the line to print.
pub fn activate(data_dir: &Path, number: &str) -> anyhow::Result<String> {
    let conn = db::open(&data_dir.join(db::DB_FILE))?;
    let now = Clock::system().now();
    let row = pending(&conn, number, now)?;
    accounts::activate(&conn, row.id, None, now)?;
    Ok(format!("activated {} ({})", row.username, row.number))
}

/// Makes an invite code for a pending account. Returns the two lines to print.
pub fn invite(data_dir: &Path, number: &str) -> anyhow::Result<String> {
    let conn = db::open(&data_dir.join(db::DB_FILE))?;
    let now = Clock::system().now();
    let row = pending(&conn, number, now)?;
    let code = invites::create(&conn, row.id, None, now, Limits::default().open_invites)
        .map_err(|e| anyhow::anyhow!("could not make a code: {e:?}"))?;
    let until = row.pending_until.unwrap_or(now);
    Ok(format!("{code}\nfor {}, valid until {}", row.display_name, utc_time(until)))
}

/// Any account (pending, active, banned or deleted) for a typed number.
fn any_account(conn: &rusqlite::Connection, number: &str, now: i64) -> anyhow::Result<accounts::AccountRow> {
    let number = Number::parse(number).with_context(|| format!("{number} is not a valid number"))?;
    accounts::by_number(conn, number, now)?.with_context(|| format!("{number} is not an account"))
}

/// Accounts with reports, most reported first: one line each.
pub fn stats(data_dir: &Path) -> anyhow::Result<String> {
    let conn = db::open(&data_dir.join(db::DB_FILE))?;
    let rows = crate::reports::stats(&conn)?;
    if rows.is_empty() {
        return Ok("no reports".into());
    }
    let lines: Vec<String> = rows
        .iter()
        .map(|r| {
            let noun = if r.reports == 1 { "report" } else { "reports" };
            let flags =
                format!("{}{}", if r.banned { "  banned" } else { "" }, if r.deleted { "  deleted" } else { "" });
            format!("{}  {}  {} {noun}  last {}{flags}", r.number, r.username, r.reports, utc_time(r.last))
        })
        .collect();
    Ok(lines.join("\n"))
}

/// One account as `tree` shows it, and who invited it.
struct Node {
    line: String,
    invited_by: Option<i64>,
}

fn node(conn: &rusqlite::Connection, id: i64) -> anyhow::Result<Node> {
    conn.query_row(
        "SELECT username, number, banned_at IS NOT NULL, deleted_at IS NOT NULL, invited_by FROM accounts WHERE id = ?1",
        [id],
        |r| {
            let (username, number, banned, deleted): (String, String, bool, bool) = (r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?);
            let number = Number::parse(&number).map(|n| n.to_string()).unwrap_or(number);
            let flags = format!("{}{}", if banned { " banned" } else { "" }, if deleted { " deleted" } else { "" });
            Ok(Node { line: format!("{username} ({number}){flags}"), invited_by: r.get(4)? })
        },
    )
    .map_err(Into::into)
}

fn invitees(conn: &rusqlite::Connection, id: i64) -> anyhow::Result<Vec<i64>> {
    let mut stmt = conn.prepare("SELECT id FROM accounts WHERE invited_by = ?1 ORDER BY id")?;
    Ok(stmt.query_map([id], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?)
}

/// Who invited the account, up to an admin-activated root, then whom it
/// invited, two spaces deeper per level; the account itself is marked `*`.
pub fn tree(data_dir: &Path, number: &str) -> anyhow::Result<String> {
    let conn = db::open(&data_dir.join(db::DB_FILE))?;
    let me = any_account(&conn, number, Clock::system().now())?.id;
    let mut chain = vec![me];
    while let Some(up) = node(&conn, *chain.last().expect("never empty"))?.invited_by {
        if chain.contains(&up) {
            break;
        }
        chain.push(up);
    }
    chain.reverse();
    let mut out = String::new();
    for (depth, id) in chain.iter().enumerate() {
        let mark = if *id == me { "* " } else { "" };
        out += &format!("{}{mark}{}\n", "  ".repeat(depth), node(&conn, *id)?.line);
    }
    let mut stack: Vec<(i64, usize)> = invitees(&conn, me)?.into_iter().rev().map(|id| (id, chain.len())).collect();
    while let Some((id, depth)) = stack.pop() {
        out += &format!("{}{}\n", "  ".repeat(depth), node(&conn, id)?.line);
        stack.extend(invitees(&conn, id)?.into_iter().rev().map(|child| (child, depth + 1)));
    }
    Ok(out)
}

/// Bans each number, signs out its devices (the server's sweep closes any
/// that are connected) and kills the invites it made. Returns the lines for what was banned and the
/// errors for the rest.
pub fn ban(data_dir: &Path, numbers: &[String]) -> anyhow::Result<(Vec<String>, Vec<String>)> {
    let mut conn = db::open(&data_dir.join(db::DB_FILE))?;
    let now = Clock::system().now();
    let (mut done, mut errors) = (Vec::new(), Vec::new());
    for number in numbers {
        match any_account(&conn, number, now) {
            Ok(row) => {
                // The ban, its devices and its open invites go together.
                let tx = conn.transaction()?;
                accounts::ban(&tx, row.id, now)?;
                tx.execute("DELETE FROM sessions WHERE account_id = ?1", [row.id])?;
                tx.execute("DELETE FROM invites WHERE inviter_id = ?1", [row.id])?;
                tx.commit()?;
                done.push(format!("banned {} ({})", row.username, row.number));
            }
            Err(err) => errors.push(err.to_string()),
        }
    }
    Ok((done, errors))
}

pub fn unban(data_dir: &Path, number: &str) -> anyhow::Result<String> {
    let conn = db::open(&data_dir.join(db::DB_FILE))?;
    let row = any_account(&conn, number, Clock::system().now())?;
    accounts::unban(&conn, row.id)?;
    Ok(format!("unbanned {} ({})", row.username, row.number))
}

/// Formats Unix seconds as `YYYY-MM-DD HH:MM UTC`.
pub fn utc_time(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    // Days since 1970-01-01 to a civil date (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02} {:02}:{:02} UTC", rem / 3_600, rem % 3_600 / 60)
}

#[cfg(test)]
mod tests {
    use super::utc_time;

    #[test]
    fn formats_unix_seconds_as_utc() {
        assert_eq!(utc_time(1_800_000_000), "2027-01-15 08:00 UTC");
        assert_eq!(utc_time(0), "1970-01-01 00:00 UTC");
    }
}
