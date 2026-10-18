use std::process::Command;

fn run(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_corvo-chat-server")).args(args).output().expect("the binary runs")
}

#[test]
fn version_flag_prints_name_and_version() {
    let out = run(&["--version"]);
    assert!(out.status.success());
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), format!("corvo-chat-server {}", env!("CARGO_PKG_VERSION")));
}

#[test]
fn serve_help_shows_the_production_defaults() {
    let out = run(&["serve", "--help"]);
    assert!(out.status.success());
    let help = String::from_utf8_lossy(&out.stdout);
    for default in ["chat.corvo.live", "/var/lib/corvo-chat", "443", "--max-devices"] {
        assert!(help.contains(default), "{default} missing from:\n{help}");
    }
}

fn dir() -> tempfile::TempDir {
    tempfile::tempdir().expect("a temp dir")
}

fn run_in(dir: &tempfile::TempDir, args: &[&str]) -> std::process::Output {
    let mut all = args.to_vec();
    all.extend(["--data-dir", dir.path().to_str().expect("a utf-8 path")]);
    run(&all)
}

const NUMBER: &str = "482 910 5572";

/// A pending account with that number, made through the library.
fn pending_account(dir: &tempfile::TempDir) {
    use corvo_chat_proto::{B64, Number, PublicKeys};
    use corvo_chat_server::{
        accounts::{NewAccount, create_pending},
        clock::Clock,
        db,
    };
    let mut conn = db::open(&dir.path().join(db::DB_FILE)).expect("a database");
    let new = NewAccount {
        username: "abin-0042".into(),
        number: Number::parse(NUMBER).expect("a valid number"),
        display_name: "Abin S".into(),
        password_record: vec![1; 192],
        recovery_record: vec![2; 192],
        password_copy: vec![3; 40],
        recovery_copy: vec![4; 40],
        public_keys: PublicKeys { signing: B64([5; 32]), exchange: B64([6; 32]), signature: B64([7; 64]) },
    };
    create_pending(&mut conn, &new, Clock::system().now(), 86_400).expect("a pending account");
}

#[test]
fn keygen_writes_the_key_once() {
    let dir = dir();
    let out = run_in(&dir, &["keygen"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(String::from_utf8_lossy(&out.stdout).contains("Back it up offline"));
    let path = dir.path().join("opaque.key");
    let first = std::fs::read(&path).expect("the key exists");
    assert_eq!(first.len(), 128);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&path).expect("metadata").permissions().mode() & 0o777, 0o600);
    }
    let again = run_in(&dir, &["keygen"]);
    assert!(!again.status.success());
    assert!(String::from_utf8_lossy(&again.stderr).contains("left alone"));
    assert_eq!(std::fs::read(&path).expect("the key exists"), first);
}

#[test]
fn serve_without_a_key_says_to_run_keygen() {
    let dir = dir();
    let out = run_in(&dir, &["serve", "--port", "0"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("keygen"));
}

#[test]
fn activate_and_invite_need_a_pending_number() {
    let dir = dir();
    for command in ["activate", "invite"] {
        let out = run_in(&dir, &[command, NUMBER]);
        assert_eq!(out.status.code(), Some(1), "{command}");
        assert!(String::from_utf8_lossy(&out.stderr).contains("not a pending account"), "{command}");
    }
}

#[test]
fn invite_prints_a_six_digit_code() {
    let dir = dir();
    pending_account(&dir);
    let out = run_in(&dir, &["invite", NUMBER]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8_lossy(&out.stdout);
    let mut lines = stdout.lines();
    let code = lines.next().expect("a first line");
    assert!(code.len() == 6 && code.bytes().all(|b| b.is_ascii_digit()), "{code}");
    let second = lines.next().expect("a second line");
    assert!(second.starts_with("for Abin S, valid until ") && second.ends_with(" UTC"), "{second}");
}

#[test]
fn activate_makes_the_account_active() {
    let dir = dir();
    pending_account(&dir);
    let out = run_in(&dir, &["activate", NUMBER]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "activated abin-0042 (482 910 5572)");
    let again = run_in(&dir, &["activate", NUMBER]);
    assert_eq!(again.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&again.stderr).contains("not a pending account"));
}

/// The number with this 9-digit body.
fn number(body: u32) -> corvo_chat_proto::Number {
    corvo_chat_proto::Number::from_body(body).expect("a valid body")
}

/// An active account made through the library; returns its id.
fn account(dir: &tempfile::TempDir, username: &str, body: u32, invited_by: Option<i64>) -> i64 {
    use corvo_chat_proto::{B64, PublicKeys};
    use corvo_chat_server::{
        accounts::{self, NewAccount, create_pending},
        clock::Clock,
        db,
    };
    let mut conn = db::open(&dir.path().join(db::DB_FILE)).expect("a database");
    let new = NewAccount {
        username: username.into(),
        number: number(body),
        display_name: "X".into(),
        password_record: vec![1; 192],
        recovery_record: vec![2; 192],
        password_copy: vec![3; 40],
        recovery_copy: vec![4; 40],
        public_keys: PublicKeys { signing: B64([5; 32]), exchange: B64([6; 32]), signature: B64([7; 64]) },
    };
    let now = Clock::system().now();
    let id = create_pending(&mut conn, &new, now, 86_400).expect("an account");
    accounts::activate(&conn, id, invited_by, now).expect("activated");
    id
}

fn with_db<T>(dir: &tempfile::TempDir, f: impl FnOnce(&rusqlite::Connection) -> T) -> T {
    let conn = corvo_chat_server::db::open(&dir.path().join(corvo_chat_server::db::DB_FILE)).expect("a database");
    f(&conn)
}

fn stdout(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn stats_lists_reported_accounts() {
    let dir = dir();
    let out = run_in(&dir, &["stats"]);
    assert!(out.status.success());
    assert_eq!(stdout(&out).trim(), "no reports");
    let (a, b) = (account(&dir, "abin-0001", 100_000_001, None), account(&dir, "rahul-0002", 100_000_002, None));
    with_db(&dir, |c| {
        for (reported, at) in [(a, 10), (a, 20), (b, 15)] {
            corvo_chat_server::reports::add(c, None, reported, None, at).unwrap();
        }
    });
    let out = stdout(&run_in(&dir, &["stats"]));
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), 2, "{out}");
    assert!(
        lines[0].contains(&number(100_000_001).to_string())
            && lines[0].contains("abin-0001")
            && lines[0].contains("2 reports")
    );
    assert!(lines[1].contains("rahul-0002") && lines[1].contains("1 report") && !lines[1].contains("1 reports"));
}

#[test]
fn tree_shows_inviters_and_invitees() {
    let dir = dir();
    let root = account(&dir, "root-0001", 100_000_001, None);
    let a = account(&dir, "aaa-0002", 100_000_002, Some(root));
    account(&dir, "bbb-0003", 100_000_003, Some(a));
    account(&dir, "ccc-0004", 100_000_004, Some(a));
    let out = run_in(&dir, &["tree", &number(100_000_002).to_string()]);
    assert!(out.status.success());
    let n = |body| number(body).to_string();
    let expected = format!(
        "root-0001 ({})\n  * aaa-0002 ({})\n    bbb-0003 ({})\n    ccc-0004 ({})\n",
        n(100_000_001),
        n(100_000_002),
        n(100_000_003),
        n(100_000_004)
    );
    assert_eq!(stdout(&out), expected);
    let missing = run_in(&dir, &["tree", &n(100_000_009)]);
    assert!(!missing.status.success());
    assert!(String::from_utf8_lossy(&missing.stderr).contains("is not an account"));
}

#[test]
fn ban_bans_each_number_and_reports_unknown_ones() {
    let dir = dir();
    let (a, b) = (account(&dir, "abin-0001", 100_000_001, None), account(&dir, "rahul-0002", 100_000_002, None));
    let n = |body| number(body).to_string();
    let out = run_in(&dir, &["ban", &n(100_000_001), &n(100_000_009), &n(100_000_002)]);
    assert!(!out.status.success(), "an unknown number makes the exit code 1");
    let printed = stdout(&out);
    assert!(printed.contains(&format!("banned abin-0001 ({})", n(100_000_001))), "{printed}");
    assert!(printed.contains(&format!("banned rahul-0002 ({})", n(100_000_002))), "{printed}");
    assert!(String::from_utf8_lossy(&out.stderr).contains(&format!("{} is not an account", n(100_000_009))));
    let banned = with_db(&dir, |c| {
        c.query_row("SELECT COUNT(*) FROM accounts WHERE banned_at IS NOT NULL AND id IN (?1, ?2)", [a, b], |r| {
            r.get::<_, i64>(0)
        })
        .unwrap()
    });
    assert_eq!(banned, 2);
}

#[test]
fn unban_lifts_a_ban() {
    let dir = dir();
    let a = account(&dir, "abin-0001", 100_000_001, None);
    let n = number(100_000_001).to_string();
    assert!(run_in(&dir, &["ban", &n]).status.success());
    let out = run_in(&dir, &["unban", &n]);
    assert!(out.status.success());
    assert_eq!(stdout(&out).trim(), format!("unbanned abin-0001 ({n})"));
    let banned = with_db(&dir, |c| {
        c.query_row("SELECT banned_at IS NOT NULL FROM accounts WHERE id = ?1", [a], |r| r.get::<_, bool>(0)).unwrap()
    });
    assert!(!banned);
}

#[test]
fn backup_writes_a_readable_copy() {
    let dir = dir();
    account(&dir, "abin-0001", 100_000_001, None);
    let out = run_in(&dir, &["backup"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let printed = stdout(&out);
    let path = printed.trim().strip_prefix("backed up to ").expect("the path is printed");
    let path = std::path::Path::new(path);
    assert!(path.starts_with(dir.path().join("backups")));
    let copy = rusqlite::Connection::open(path).expect("the copy opens");
    let accounts: i64 = copy.query_row("SELECT COUNT(*) FROM accounts", [], |r| r.get(0)).unwrap();
    assert_eq!(accounts, 1);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(path).unwrap().permissions().mode() & 0o777, 0o600);
    }
}

#[test]
fn backup_keeps_the_newest_seven() {
    let dir = dir();
    account(&dir, "abin-0001", 100_000_001, None);
    let backups = dir.path().join("backups");
    std::fs::create_dir_all(&backups).unwrap();
    for day in 1..=9 {
        std::fs::write(backups.join(format!("server-2026-01-0{day}.db")), b"old").unwrap();
    }
    assert!(run_in(&dir, &["backup"]).status.success());
    let mut names: Vec<String> =
        std::fs::read_dir(&backups).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    names.sort();
    assert_eq!(names.len(), 7, "{names:?}");
    assert_eq!(names[0], "server-2026-01-04.db", "the three oldest went");
}

#[test]
fn backup_refuses_keep_zero() {
    let dir = dir();
    account(&dir, "abin-0001", 100_000_001, None);
    let out = run_in(&dir, &["backup", "--keep", "0"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("--keep must be at least 1"));
    assert!(!dir.path().join("backups").exists());
}

#[test]
fn backup_refuses_a_missing_database() {
    let dir = dir();
    let out = run_in(&dir, &["backup"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("no server.db in"));
    assert!(!dir.path().join("server.db").exists());
}

#[test]
fn backup_leaves_the_database_unmigrated() {
    let dir = dir();
    let source = dir.path().join("server.db");
    rusqlite::Connection::open(&source).unwrap().execute_batch("CREATE TABLE t (x)").unwrap();
    let out = run_in(&dir, &["backup"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let source = rusqlite::Connection::open(&source).unwrap();
    let version: i64 = source.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
    assert_eq!(version, 0);
    let tables = |c: &rusqlite::Connection| -> Vec<String> {
        let mut stmt = c.prepare("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name").unwrap();
        stmt.query_map([], |r| r.get(0)).unwrap().map(Result::unwrap).collect()
    };
    assert_eq!(tables(&source), vec!["t"]);
    let printed = stdout(&out);
    let copy = rusqlite::Connection::open(printed.trim().strip_prefix("backed up to ").unwrap()).unwrap();
    assert_eq!(tables(&copy), vec!["t"]);
}

#[test]
fn backup_replaces_a_stale_tmp() {
    let dir = dir();
    account(&dir, "abin-0001", 100_000_001, None);
    let out = run_in(&dir, &["backup"]);
    assert!(out.status.success());
    let printed = stdout(&out);
    let path = std::path::PathBuf::from(printed.trim().strip_prefix("backed up to ").unwrap());
    std::fs::write(path.with_extension("db.tmp"), b"junk").unwrap();
    let out = run_in(&dir, &["backup"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let copy = rusqlite::Connection::open(&path).unwrap();
    let accounts: i64 = copy.query_row("SELECT COUNT(*) FROM accounts", [], |r| r.get(0)).unwrap();
    assert_eq!(accounts, 1);
    let left: Vec<_> = std::fs::read_dir(dir.path().join("backups"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".tmp"))
        .collect();
    assert!(left.is_empty(), "{left:?}");
}

#[test]
fn ban_kills_the_accounts_open_invites() {
    use corvo_chat_server::{clock::Clock, invites};
    let dir = dir();
    let inviter = account(&dir, "abin-0001", 100_000_001, None);
    pending_account(&dir);
    let pending = with_db(&dir, |c| {
        let id: i64 = c.query_row("SELECT id FROM accounts WHERE username = 'abin-0042'", [], |r| r.get(0)).unwrap();
        invites::create(c, id, Some(inviter), Clock::system().now(), 5).unwrap();
        id
    });
    assert!(run_in(&dir, &["ban", &number(100_000_001).to_string()]).status.success());
    let left = with_db(&dir, |c| {
        c.query_row("SELECT COUNT(*) FROM invites WHERE account_id = ?1", [pending], |r| r.get::<_, i64>(0)).unwrap()
    });
    assert_eq!(left, 0, "the banned account's invite no longer works");
}
