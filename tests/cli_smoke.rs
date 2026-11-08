use std::fs;
use std::path::Path;
use std::process::{Command, Output};

use serde_json::Value;
use tempfile::TempDir;

fn run(db: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_aether-midnight"))
        .arg("--db")
        .arg(db)
        .args(args)
        .output()
        .unwrap()
}

fn json(db: &Path, args: &[&str]) -> Value {
    let out = run(db, args);
    assert!(
        out.status.success(),
        "{args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| panic!("{args:?} not JSON: {e}"))
}

#[test]
fn cli_init_doctor_memory_persistence_namespaces_and_deletion() {
    let temp = TempDir::new().unwrap();
    let db = temp.path().join("m.sqlite3");

    let init = json(&db, &["init", "--json"]);
    assert_eq!(init["initialized"], true);
    assert!(db.exists());
    assert_eq!(json(&db, &["init", "--json"])["initialized"], true);

    let doctor = json(&db, &["doctor", "--json"]);
    assert_eq!(doctor["ok"], true);

    let a = json(
        &db,
        &[
            "memory",
            "add",
            "--namespace",
            "proj:a",
            "alpha zebra fact",
            "--json",
        ],
    );
    json(
        &db,
        &[
            "memory",
            "add",
            "--namespace",
            "proj:b",
            "beta zebra fact",
            "--json",
        ],
    );
    let id = a["id"].as_i64().unwrap().to_string();

    // Persistence across processes and exact-namespace get.
    assert_eq!(
        json(
            &db,
            &["memory", "get", "--namespace", "proj:a", &id, "--json"]
        )["content"],
        "alpha zebra fact"
    );
    // Namespace isolation: same id in another namespace is not found.
    assert!(
        !run(&db, &["memory", "get", "--namespace", "proj:b", &id])
            .status
            .success()
    );

    let hits = json(
        &db,
        &[
            "memory",
            "search",
            "--namespace",
            "proj:a",
            "zebra",
            "--json",
        ],
    );
    let hits = hits.as_array().unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0]["memory"]["scope"], "proj:a");
    assert_eq!(
        json(&db, &["memory", "list", "--namespace", "proj:b", "--json"])
            .as_array()
            .unwrap()
            .len(),
        1
    );

    // Deleting from the wrong namespace fails; the right one succeeds.
    assert!(
        !run(&db, &["memory", "delete", "--namespace", "proj:b", &id])
            .status
            .success()
    );
    assert_eq!(
        json(
            &db,
            &["memory", "delete", "--namespace", "proj:a", &id, "--json"]
        )["deleted"],
        true
    );
    assert!(
        json(
            &db,
            &[
                "memory",
                "search",
                "--namespace",
                "proj:a",
                "zebra",
                "--json"
            ]
        )
        .as_array()
        .unwrap()
        .is_empty()
    );
    assert!(
        !run(&db, &["memory", "get", "--namespace", "proj:a", &id])
            .status
            .success()
    );

    // Human output works without --json.
    let out = run(&db, &["memory", "list", "--namespace", "proj:b"]);
    assert!(String::from_utf8_lossy(&out.stdout).contains("beta zebra fact"));
}

#[test]
fn cli_index_search_incremental_and_ignore_rules() {
    let temp = TempDir::new().unwrap();
    let db = temp.path().join("c.sqlite3");
    let repo = temp.path().join("repo");
    fs::create_dir_all(repo.join("src")).unwrap();
    fs::create_dir_all(repo.join("ignored")).unwrap();
    fs::write(repo.join(".gitignore"), "ignored/\n").unwrap();
    fs::write(repo.join("src/lib.rs"), "pub fn smoke_marker_one() {}\n").unwrap();
    fs::write(repo.join("ignored/x.rs"), "pub fn hidden_marker_two() {}\n").unwrap();
    let r = repo.to_str().unwrap();

    let first = json(&db, &["index", "--repo", r, "--json"]);
    assert_eq!(first["cancelled"], false);
    let found = json(&db, &["search", "--repo", r, "smoke_marker_one", "--json"]);
    assert!(found.to_string().contains("src/lib.rs"));
    assert!(
        !json(
            &db,
            &["search", "--repo", r, "--query", "hidden_marker_two"]
        )
        .to_string()
        .contains("ignored/x.rs")
    );

    fs::write(repo.join("src/lib.rs"), "pub fn smoke_marker_three() {}\n").unwrap();
    json(&db, &["index", "--repo", r]);
    assert!(
        json(&db, &["search", "--repo", r, "smoke_marker_three"])
            .to_string()
            .contains("src/lib.rs")
    );
    assert!(
        !json(&db, &["search", "--repo", r, "smoke_marker_one"])
            .to_string()
            .contains("smoke_marker_one()")
    );
}

#[test]
fn cli_help_lists_phase_one_commands() {
    let out = Command::new(env!("CARGO_BIN_EXE_aether-midnight"))
        .arg("--help")
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    for c in ["init", "index", "search", "memory", "doctor"] {
        assert!(text.contains(&format!("\n  {c} ")), "missing {c}");
    }
}
