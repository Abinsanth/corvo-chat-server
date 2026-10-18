//! Reports: who reported whom, when and why, for the admin's `stats`.

use corvo_chat_proto::Number;
use rusqlite::{Connection, params};

/// One reported account, as `stats` lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reported {
    pub number: Number,
    pub username: String,
    pub reports: i64,
    /// The newest report's time.
    pub last: i64,
    pub banned: bool,
    pub deleted: bool,
}

pub fn add(
    c: &Connection,
    reporter: Option<i64>,
    reported: i64,
    reason: Option<&str>,
    now: i64,
) -> rusqlite::Result<()> {
    c.execute(
        "INSERT INTO reports (reporter_id, reported_id, reason, at) VALUES (?1, ?2, ?3, ?4)",
        params![reporter, reported, reason, now],
    )?;
    Ok(())
}

/// Every account with a report, most reported first.
pub fn stats(c: &Connection) -> rusqlite::Result<Vec<Reported>> {
    let mut stmt = c.prepare(
        "SELECT a.number, a.username, COUNT(*), MAX(r.at), a.banned_at IS NOT NULL, a.deleted_at IS NOT NULL
         FROM reports r JOIN accounts a ON a.id = r.reported_id
         GROUP BY a.id ORDER BY COUNT(*) DESC, a.id",
    )?;
    stmt.query_map([], |r| {
        let number: String = r.get(0)?;
        Ok(Reported {
            number: Number::parse(&number)
                .map_err(|e| rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e)))?,
            username: r.get(1)?,
            reports: r.get(2)?,
            last: r.get(3)?,
            banned: r.get(4)?,
            deleted: r.get(5)?,
        })
    })?
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stats_counts_reports_per_account() {
        let c = crate::testing::conn_with_accounts(4);
        for at in [10, 30, 20] {
            add(&c, Some(3), 1, Some("spam"), at).unwrap();
        }
        add(&c, Some(4), 2, None, 15).unwrap();
        let stats = stats(&c).unwrap();
        let rows: Vec<_> = stats.iter().map(|r| (r.username.as_str(), r.reports, r.last)).collect();
        assert_eq!(rows, vec![("a-0001", 3, 30), ("a-0002", 1, 15)]);
        assert!(stats.iter().all(|r| !r.banned && !r.deleted));
    }
}
