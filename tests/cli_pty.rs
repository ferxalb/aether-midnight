//! The interactive menus (`setup` inside a repository, `connect` without `--scope`) driven
//! through a real pseudo-terminal, so `stdin_is_tty` is true. HOME and the config directory are
//! redirected; nothing here touches a real client configuration.
#![cfg(unix)]
use std::fs;
use std::io::{Read, Write};
use std::os::fd::{FromRawFd, OwnedFd};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use tempfile::TempDir;

/// Run the binary on a pty, feed `input`, return (success, everything it printed).
fn run_pty(dir: &Path, home: &Path, args: &[&str], input: &str) -> (bool, String) {
    let (mut master, slave) = unsafe {
        let (mut m, mut s) = (0, 0);
        assert_eq!(
            libc::openpty(
                &mut m,
                &mut s,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut()
            ),
            0
        );
        (fs::File::from_raw_fd(m), OwnedFd::from_raw_fd(s))
    };
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_midnight"));
    cmd.args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("AETHER_MIDNIGHT_CONFIG_DIR", home.join("cfg"))
        .env_remove("CODEX_HOME")
        .env_remove("MIDNIGHT_NO_INPUT")
        .stdin(Stdio::from(slave.try_clone().unwrap()))
        .stdout(Stdio::from(slave.try_clone().unwrap()))
        .stderr(Stdio::from(slave.try_clone().unwrap()));
    let mut child = cmd.spawn().unwrap();
    drop(cmd);
    drop(slave);
    // Read while the child runs: some platforms drop unread pty output once the slave closes.
    let mut reader = master.try_clone().unwrap();
    let collector = std::thread::spawn(move || {
        let mut out = Vec::new();
        let mut buf = [0u8; 4096];
        while let Ok(n) = reader.read(&mut buf) {
            if n == 0 {
                break;
            }
            out.extend_from_slice(&buf[..n]);
        }
        out
    });
    master.write_all(input.as_bytes()).unwrap();

    let deadline = Instant::now() + Duration::from_secs(30);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        assert!(Instant::now() < deadline, "interactive command hung");
        std::thread::sleep(Duration::from_millis(20));
    };
    let out = collector.join().unwrap();
    (status.success(), String::from_utf8_lossy(&out).into_owned())
}

fn repo_and_home() -> (TempDir, std::path::PathBuf, std::path::PathBuf) {
    let temp = TempDir::new().unwrap();
    let base = temp.path().canonicalize().unwrap();
    let (repo, home) = (base.join("repo"), base.join("home"));
    fs::create_dir_all(repo.join(".git")).unwrap();
    fs::create_dir_all(&home).unwrap();
    fs::write(repo.join("lib.rs"), "pub fn f() {}\n").unwrap();
    (temp, repo, home)
}

fn lexical_global_setup(repo: &Path, home: &Path) {
    let status = Command::new(env!("CARGO_BIN_EXE_midnight"))
        .args(["setup", "--provider", "lexical", "--no-validate"])
        .current_dir(repo)
        .env("HOME", home)
        .env("AETHER_MIDNIGHT_CONFIG_DIR", home.join("cfg"))
        .status()
        .unwrap();
    assert!(status.success());
}

#[test]
fn project_setup_defaults_to_project_storage_and_skips_the_client() {
    let (_t, repo, home) = repo_and_home();
    lexical_global_setup(&repo, &home);
    // Two Enters: accept "in this project" and "skip" for the client.
    let (ok, out) = run_pty(&repo, &home, &["setup"], "\n\n");
    assert!(ok, "{out}");
    assert!(
        out.contains("Where should this project's index and memories live?"),
        "{out}"
    );
    assert!(
        out.contains("1) in this project (.midnight/, git-ignored) (default)"),
        "{out}"
    );
    assert!(repo.join(".midnight/midnight.sqlite3").is_file());
    assert!(repo.join(".midnight/midnight.json").is_file());
    assert!(!repo.join(".mcp.json").exists());
}

#[test]
fn project_setup_can_choose_global_storage_and_register_claude_code() {
    let (_t, repo, home) = repo_and_home();
    lexical_global_setup(&repo, &home);
    let (ok, out) = run_pty(&repo, &home, &["setup"], "2\n1\n");
    assert!(ok, "{out}");
    assert!(
        !repo.join(".midnight").exists(),
        "global storage must not create .midnight"
    );
    let cfg = fs::read_to_string(repo.join(".mcp.json")).unwrap();
    assert!(cfg.contains("\"command\": \"midnight\""), "{cfg}");
}

#[test]
fn connect_asks_for_the_scope_on_a_terminal() {
    let (_t, repo, home) = repo_and_home();
    let (ok, out) = run_pty(&repo, &home, &["connect", "claude-code"], "2\n");
    assert!(ok, "{out}");
    assert!(out.contains("Register Midnight for:"), "{out}");
    assert!(home.join(".claude.json").is_file());
    assert!(!repo.join(".mcp.json").exists());

    let (ok, _) = run_pty(&repo, &home, &["connect", "claude-code"], "9\n");
    assert!(!ok, "an invalid choice must fail instead of guessing");
}
