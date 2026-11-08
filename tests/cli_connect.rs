//! `init --storage project` and `connect` through the real binary, with HOME and the config
//! directory redirected so no real client configuration is touched.
use std::fs;
use std::path::Path;
use std::process::{Command, Output};

use serde_json::Value;
use tempfile::TempDir;

fn run(dir: &Path, home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_midnight"))
        .args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("AETHER_MIDNIGHT_CONFIG_DIR", home.join("cfg"))
        .env_remove("CODEX_HOME")
        .output()
        .unwrap()
}

fn setup() -> (TempDir, std::path::PathBuf, std::path::PathBuf) {
    let temp = TempDir::new().unwrap();
    let base = temp.path().canonicalize().unwrap();
    let repo = base.join("repo");
    let home = base.join("home");
    fs::create_dir_all(repo.join(".git")).unwrap();
    fs::create_dir_all(&home).unwrap();
    fs::write(repo.join("lib.rs"), "pub fn billing_total() {}\n").unwrap();
    (temp, repo, home)
}

#[test]
fn project_storage_is_gitignored_found_automatically_and_never_indexed() {
    let (_t, repo, home) = setup();
    let out = run(&repo, &home, &["init", "--storage", "project"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(repo.join(".midnight/midnight.sqlite3").is_file());
    assert_eq!(
        fs::read_to_string(repo.join(".gitignore")).unwrap(),
        "# AETHER Midnight local data\n.midnight/\n"
    );

    // No --db: the project database is discovered from the working directory.
    let out = run(&repo, &home, &["index", ".", "--json"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let report: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(
        report["eligible_files"], 2,
        "lib.rs and .gitignore only: {report}"
    );
    assert!(!home.join("Library").exists() && !home.join(".local").exists());
}

#[test]
fn connect_writes_each_client_config_and_remove_undoes_it() {
    let (_t, repo, home) = setup();

    let out = run(
        &repo,
        &home,
        &[
            "connect",
            "claude-code",
            "--scope",
            "project",
            "--read-only",
        ],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let cfg: Value =
        serde_json::from_str(&fs::read_to_string(repo.join(".mcp.json")).unwrap()).unwrap();
    let entry = &cfg["mcpServers"]["aether-midnight"];
    assert_eq!(entry["args"], serde_json::json!(["mcp", "--read-only"]));
    assert!(Path::new(entry["command"].as_str().unwrap()).is_absolute());

    let out = run(&repo, &home, &["connect", "codex", "--scope", "user"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let toml = fs::read_to_string(home.join(".codex/config.toml")).unwrap();
    assert!(toml.contains("[mcp_servers.aether-midnight]") && toml.contains("args = [\"mcp\"]"));

    // Second run is a no-op.
    let out = run(
        &repo,
        &home,
        &["connect", "codex", "--scope", "user", "--read-only"],
    );
    assert!(out.status.success());
    assert!(
        fs::read_to_string(home.join(".codex/config.toml"))
            .unwrap()
            .contains("--read-only")
    );

    let out = run(
        &repo,
        &home,
        &["connect", "codex", "--scope", "user", "--remove"],
    );
    assert!(out.status.success());
    assert!(
        !fs::read_to_string(home.join(".codex/config.toml"))
            .unwrap()
            .contains("aether-midnight")
    );

    // Clients without a project config and missing scope fail with an actionable message.
    let out = run(
        &repo,
        &home,
        &["connect", "claude-desktop", "--scope", "project"],
    );
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("--scope user"));
    let out = run(&repo, &home, &["connect", "claude-code"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("--scope"));
}

#[test]
fn connect_refuses_to_overwrite_invalid_json() {
    let (_t, repo, home) = setup();
    fs::write(repo.join(".mcp.json"), "{ not json").unwrap();
    let out = run(
        &repo,
        &home,
        &["connect", "claude-code", "--scope", "project"],
    );
    assert!(!out.status.success());
    assert_eq!(
        fs::read_to_string(repo.join(".mcp.json")).unwrap(),
        "{ not json"
    );
}
