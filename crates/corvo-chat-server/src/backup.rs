//! The daily backup: a consistent copy of `server.db` in `backups/`, keeping
//! the newest few. `opaque.key` isn't in it: that one is backed up offline.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Context;

use crate::{admin::utc_time, db};

/// The folder inside the data directory.
pub const DIR: &str = "backups";
/// How many backups `run` keeps by default.
pub const KEEP: usize = 7;

/// Copies the database to `backups/server-<UTC date>.db` (a same-day copy is
/// replaced), then deletes all but the newest `keep`. Returns the copy's path.
pub fn run(data_dir: &Path, now: i64, keep: usize) -> anyhow::Result<PathBuf> {
    let dir = data_dir.join(DIR);
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder.create(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let date = &utc_time(now)[..10];
    let path = dir.join(format!("server-{date}.db"));
    if path.exists() {
        fs::remove_file(&path).with_context(|| format!("replacing {}", path.display()))?;
    }
    let conn = db::open(&data_dir.join(db::DB_FILE))?;
    let target = path.to_str().context("the backup path isn't UTF-8")?;
    conn.execute("VACUUM INTO ?1", [target]).context("copying the database")?;
    #[cfg(unix)]
    fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o600))?;
    let mut copies: Vec<PathBuf> = fs::read_dir(&dir)?
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|p| {
            p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("server-") && n.ends_with(".db"))
        })
        .collect();
    copies.sort();
    let extra = copies.len().saturating_sub(keep);
    for old in &copies[..extra] {
        fs::remove_file(old).with_context(|| format!("deleting {}", old.display()))?;
    }
    Ok(path)
}
