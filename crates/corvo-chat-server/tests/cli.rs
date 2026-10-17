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
    for default in ["chat.corvo.live", "/var/lib/corvo-chat", "443"] {
        assert!(help.contains(default), "{default} missing from:\n{help}");
    }
}
