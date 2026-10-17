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
