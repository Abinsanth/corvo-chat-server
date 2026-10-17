//! The admin subcommands that work on the database directly: `activate` and
//! `invite`.

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
