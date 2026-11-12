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
    // Portable: no machine-specific binary path and no database path in a project file.
    assert_eq!(entry["args"], serde_json::json!(["mcp", "--read-only"]));
    assert_eq!(entry["command"], "midnight");

    let out = run(&repo, &home, &["connect", "codex", "--scope", "user"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let toml = fs::read_to_string(home.join(".codex/config.toml")).unwrap();
    assert!(toml.contains("[mcp_servers.aether-midnight]") && toml.contains("args = [\"mcp\"]"));
    // User-scope files name the exact binary.
    let command = toml.lines().find(|l| l.starts_with("command")).unwrap();
    assert!(
        command.contains(env!("CARGO_BIN_EXE_midnight")),
        "{command}"
    );

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

#[test]
fn project_backup_stays_in_the_ignored_directory() {
    let (_t, repo, home) = setup();
    fs::write(
        repo.join(".mcp.json"),
        r#"{"mcpServers":{"other":{"command":"x"}}}"#,
    )
    .unwrap();
    let out = run(
        &repo,
        &home,
        &["connect", "claude-code", "--scope", "project"],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        repo.join(".midnight/backups/.mcp.json.midnight.bak")
            .is_file()
    );
    assert!(!repo.join(".mcp.json.midnight.bak").exists());
    assert!(
        fs::read_to_string(repo.join(".gitignore"))
            .unwrap()
            .contains(".midnight/")
    );
}

#[test]
fn a_cloned_midnight_directory_is_not_trusted_until_the_user_sets_it_up() {
    let (_t, repo, home) = setup();
    // Simulate a repository that ships its own .midnight (never set up on this machine).
    fs::create_dir_all(repo.join(".midnight")).unwrap();
    fs::write(repo.join(".midnight/midnight.sqlite3"), b"not a database").unwrap();
    fs::write(
        repo.join(".midnight/midnight.json"),
        r#"{"embeddings":false}"#,
    )
    .unwrap();
    let out = run(
        &repo,
        &home,
        &["--db", home.join("g.sqlite3").to_str().unwrap(), "init"],
    );
    assert!(out.status.success());
    // Indexing without --db must not open the planted file; it uses the global database.
    let out = run(&repo, &home, &["index", ".", "--json"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        fs::read(repo.join(".midnight/midnight.sqlite3")).unwrap(),
        b"not a database"
    );
}

#[test]
fn project_setup_without_a_terminal_adopts_the_project() {
    let (_t, repo, home) = setup();
    // A user-wide setup exists (keyword-only), so `setup` inside the repo is the project flow.
    let out = run(
        &repo,
        &home,
        &["setup", "--provider", "lexical", "--no-validate"],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let out = run(&repo, &home, &["setup", "--no-input"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(repo.join(".midnight/midnight.sqlite3").is_file());
    let cfg: Value =
        serde_json::from_str(&fs::read_to_string(repo.join(".midnight/midnight.json")).unwrap())
            .unwrap();
    assert_eq!(cfg["embeddings"], true);
    assert!(
        fs::read_to_string(repo.join(".gitignore"))
            .unwrap()
            .contains(".midnight/")
    );
    // Trusted now: discovery works, and a bad project file is reported, not ignored.
    let out = run(&repo, &home, &["index", ".", "--json"]);
    assert!(out.status.success());
    fs::write(
        repo.join(".midnight/midnight.json"),
        r#"{"endpoint":"https://x"}"#,
    )
    .unwrap();
    let out = run(&repo, &home, &["index", ".", "--json"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("midnight.json"));
}

#[test]
fn project_list_untrust_and_status_describe_the_project_state() {
    let (_t, repo, home) = setup();
    let status = |repo: &Path| -> Value {
        let out = run(repo, &home, &["status", "--json"]);
        serde_json::from_slice(&out.stdout).unwrap()
    };
    // A repository with no Midnight data: reported as global storage, nothing created.
    let s = status(&repo);
    assert_eq!(s["project"]["storage"], "global");
    assert!(!repo.join(".midnight").exists());

    assert!(
        run(&repo, &home, &["init", "--storage", "project"])
            .status
            .success()
    );
    let s = status(&repo);
    assert_eq!(s["project"]["storage"], "project");
    assert_eq!(s["project"]["trusted"], true);
    assert_eq!(s["project"]["embeddings"], true);

    let list = run(&repo, &home, &["project", "list", "--json"]);
    let list: Value = serde_json::from_slice(&list.stdout).unwrap();
    assert_eq!(list["projects"].as_array().unwrap().len(), 1);

    let out = run(&repo, &home, &["project", "untrust", "--json"]);
    assert_eq!(
        serde_json::from_slice::<Value>(&out.stdout).unwrap()["removed"],
        true
    );
    let s = status(&repo);
    assert_eq!(s["project"]["storage"], "global");
    assert_eq!(s["project"]["trusted"], false);
    assert!(
        s["next"].to_string().contains("init --storage project"),
        "status should explain how to adopt the leftover .midnight/: {s}"
    );
    assert!(
        repo.join(".midnight/midnight.sqlite3").is_file(),
        "data is kept"
    );
    let out = run(&repo, &home, &["project", "untrust", "--json"]);
    assert_eq!(
        serde_json::from_slice::<Value>(&out.stdout).unwrap()["removed"],
        false
    );
}

#[cfg(unix)]
#[test]
fn project_entries_use_the_name_the_user_invoked_even_through_a_symlink() {
    let (t, repo, home) = setup();
    let link = t.path().join("midnight");
    std::os::unix::fs::symlink(env!("CARGO_BIN_EXE_aether-midnight"), &link).unwrap();
    let out = Command::new(&link)
        .args(["connect", "claude-code", "--scope", "project"])
        .current_dir(&repo)
        .env("HOME", &home)
        .env("AETHER_MIDNIGHT_CONFIG_DIR", home.join("cfg"))
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let cfg: Value =
        serde_json::from_str(&fs::read_to_string(repo.join(".mcp.json")).unwrap()).unwrap();
    assert_eq!(cfg["mcpServers"]["aether-midnight"]["command"], "midnight");
}

#[test]
fn connect_with_instructions_manages_only_its_block_in_claude_md_and_agents_md() {
    let (_t, repo, home) = setup();
    fs::write(repo.join("CLAUDE.md"), "# House rules\n\nUse tabs.\n").unwrap();
    let ok = |out: Output| {
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_slice::<Value>(&out.stdout).unwrap()
    };

    let preview = ok(run(
        &repo,
        &home,
        &[
            "connect",
            "claude-code",
            "--scope",
            "project",
            "--with-instructions",
            "--dry-run",
            "--json",
        ],
    ));
    assert_eq!(preview["instructions"]["changed"], true);
    assert_eq!(
        fs::read_to_string(repo.join("CLAUDE.md")).unwrap(),
        "# House rules\n\nUse tabs.\n"
    );

    let added = ok(run(
        &repo,
        &home,
        &[
            "connect",
            "claude-code",
            "--scope",
            "project",
            "--with-instructions",
            "--json",
        ],
    ));
    assert_eq!(added["instructions"]["changed"], true);
    let text = fs::read_to_string(repo.join("CLAUDE.md")).unwrap();
    assert!(
        text.starts_with("# House rules\n\nUse tabs.\n\n<!-- aether-midnight:begin"),
        "{text}"
    );
    assert!(text.contains("`project:repo`"), "{text}");
    assert!(
        added["instructions"]["backup"]
            .as_str()
            .unwrap()
            .contains(".midnight")
    );

    let again = ok(run(
        &repo,
        &home,
        &[
            "connect",
            "claude-code",
            "--scope",
            "project",
            "--with-instructions",
            "--json",
        ],
    ));
    assert_eq!(again["instructions"]["changed"], false);

    let removed = ok(run(
        &repo,
        &home,
        &[
            "connect",
            "claude-code",
            "--scope",
            "project",
            "--with-instructions",
            "--remove",
            "--json",
        ],
    ));
    assert_eq!(removed["instructions"]["changed"], true);
    assert_eq!(
        fs::read_to_string(repo.join("CLAUDE.md")).unwrap(),
        "# House rules\n\nUse tabs.\n"
    );

    // Codex writes AGENTS.md; a file that held only the block is removed again.
    ok(run(
        &repo,
        &home,
        &[
            "connect",
            "codex",
            "--scope",
            "project",
            "--with-instructions",
            "--json",
        ],
    ));
    assert!(
        fs::read_to_string(repo.join("AGENTS.md"))
            .unwrap()
            .contains("aether-midnight:begin")
    );
    ok(run(
        &repo,
        &home,
        &[
            "connect",
            "codex",
            "--scope",
            "project",
            "--with-instructions",
            "--remove",
            "--json",
        ],
    ));
    assert!(!repo.join("AGENTS.md").exists());

    let desktop = run(
        &repo,
        &home,
        &[
            "connect",
            "claude-desktop",
            "--scope",
            "user",
            "--with-instructions",
            "--dry-run",
        ],
    );
    assert!(!desktop.status.success());
}
