//! Migration of databases created by real earlier builds (see tests/fixtures/README.md).

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use rusqlite::Connection;
use serde_json::Value;
use tempfile::TempDir;

fn fixture(name: &str, temp: &TempDir) -> PathBuf {
    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    let target = temp.path().join(name);
    std::fs::copy(source, &target).unwrap();
    target
}

fn run(db: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_aether-midnight"))
        .arg("--db")
        .arg(db)
        .args(args)
        .env(
            "AETHER_MIDNIGHT_MODELS_DIR",
            db.parent().unwrap().join("no-models"),
        )
        .output()
        .unwrap()
}

fn json(db: &Path, args: &[&str]) -> Value {
    let out = run(db, args);
    assert!(
        out.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap()
}

fn user_version(db: &Path) -> i64 {
    Connection::open(db)
        .unwrap()
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap()
}

#[test]
fn databases_from_real_schema_v3_v4_v5_builds_migrate_and_keep_their_data() {
    for (name, original) in [
        ("schema_v3.sqlite3", 3),
        ("schema_v4.sqlite3", 4),
        ("schema_v5.sqlite3", 5),
    ] {
        let temp = TempDir::new().unwrap();
        let db = fixture(name, &temp);
        assert_eq!(
            user_version(&db),
            original,
            "{name} is not the schema it claims to be"
        );

        let init = json(&db, &["init", "--json"]);
        assert_eq!(init["schema_version"], 10, "{name}");
        assert_eq!(user_version(&db), 10);
        let duplicate_index: i64 = Connection::open(&db)
            .unwrap()
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE type='index' AND name='code_files_scope_path'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            duplicate_index, 0,
            "{name}: schema 8 drops the duplicate path index"
        );
        let conn = Connection::open(&db).unwrap();
        let (files, paths): (i64, i64) = conn
            .query_row(
                "SELECT (SELECT count(*) FROM code_files),
                        (SELECT count(*) FROM code_files f WHERE EXISTS (
                            SELECT 1 FROM code_path_fts p WHERE p.rowid=f.id
                              AND code_path_fts MATCH '\"' || replace(f.rel_path,'\"','\"\"') || '\"'))",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(
            files, paths,
            "{name}: schema 9 backfills every catalog path"
        );
        drop(conn);

        // Data written by the old build is intact, namespace-isolated, and the forgotten row stays gone.
        let listed = json(&db, &["memory", "list", "--namespace", "proj:a", "--json"]);
        let contents: Vec<&str> = listed
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["content"].as_str().unwrap())
            .collect();
        assert_eq!(
            contents,
            vec!["alpha zebra memory from old schema"],
            "{name}"
        );
        let a = json(
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
        assert_eq!(a.as_array().unwrap().len(), 1, "{name}");
        let b = json(
            &db,
            &[
                "memory",
                "search",
                "--namespace",
                "proj:b",
                "zebra",
                "--json",
            ],
        );
        assert_eq!(
            b[0]["memory"]["content"],
            "gamma zebra memory in another namespace"
        );
        assert!(
            json(
                &db,
                &[
                    "memory",
                    "search",
                    "--namespace",
                    "proj:a",
                    "giraffe",
                    "--json"
                ]
            )
            .as_array()
            .unwrap()
            .is_empty()
        );

        // Code index metadata survived and the new table starts empty.
        let conn = Connection::open(&db).unwrap();
        let files: i64 = conn
            .query_row("SELECT count(*) FROM code_files", [], |r| r.get(0))
            .unwrap();
        assert_eq!(files, 2, "{name}");
        let vectors: i64 = conn
            .query_row("SELECT count(*) FROM embeddings", [], |r| r.get(0))
            .unwrap();
        assert_eq!(vectors, 0);
        drop(conn);

        assert!(run(&db, &["verify-integrity"]).status.success(), "{name}");
        assert_eq!(json(&db, &["doctor", "--json"])["ok"], true, "{name}");

        // Semantic modes degrade (no vectors / no model) but lexical results stay correct.
        let hybrid = json(
            &db,
            &[
                "memory",
                "search",
                "--namespace",
                "proj:a",
                "zebra",
                "--mode",
                "hybrid",
                "--json",
            ],
        );
        assert_eq!(hybrid["degraded"], true);
        assert_eq!(hybrid["results"].as_array().unwrap().len(), 1);

        // Migrated databases accept writes and the new cleanup triggers work.
        let added = json(
            &db,
            &[
                "memory",
                "add",
                "--namespace",
                "proj:a",
                "post-migration zebra",
                "--json",
            ],
        );
        let id = added["id"].as_i64().unwrap();
        let conn = Connection::open(&db).unwrap();
        conn.execute(
            "INSERT INTO embeddings(kind,scope,owner_id,chunk,model,revision,dims,source_hash,vector) VALUES('memory','proj:a',?1,0,'m','r',1,'h',x'00000000')",
            [id],
        ).unwrap();
        drop(conn);
        json(
            &db,
            &[
                "memory",
                "delete",
                "--namespace",
                "proj:a",
                &id.to_string(),
                "--json",
            ],
        );
        let conn = Connection::open(&db).unwrap();
        let left: i64 = conn
            .query_row("SELECT count(*) FROM embeddings", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            left, 0,
            "{name}: forgetting a memory must delete its vector"
        );
        drop(conn);

        // Idempotent re-open.
        assert_eq!(json(&db, &["init", "--json"])["schema_version"], 10);
    }
}

#[test]
fn database_from_a_newer_schema_is_refused_without_modification() {
    let temp = TempDir::new().unwrap();
    let db = fixture("schema_v5.sqlite3", &temp);
    Connection::open(&db)
        .unwrap()
        .pragma_update(None, "user_version", 11)
        .unwrap();
    let out = run(&db, &["init"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("newer than this binary supports"));
    assert_eq!(user_version(&db), 11);
}

#[test]
fn rc7_database_and_export_upgrade_and_rc7_refuses_schema_ten() {
    let Some(rc7) = std::env::var_os("MIDNIGHT_RC7_BINARY") else {
        eprintln!("set MIDNIGHT_RC7_BINARY to run the real rc.7 upgrade test");
        return;
    };
    let temp = TempDir::new().unwrap();
    let db = temp.path().join("rc7.sqlite3");
    let export = temp.path().join("rc7.jsonl");
    let run_rc7 = |args: &[&str]| {
        Command::new(&rc7)
            .arg("--db")
            .arg(&db)
            .args(args)
            .env("AETHER_MIDNIGHT_CONFIG_DIR", temp.path().join("config"))
            .output()
            .unwrap()
    };
    let stored = run_rc7(&[
        "remember",
        "--scope",
        "project:fixture",
        "--content",
        "retained rc7 record",
        "--json",
    ]);
    assert!(
        stored.status.success(),
        "{}",
        String::from_utf8_lossy(&stored.stderr)
    );
    assert_eq!(user_version(&db), 9);
    let exported = run_rc7(&[
        "memory",
        "export",
        "--namespace",
        "project:fixture",
        "--output",
        export.to_str().unwrap(),
    ]);
    assert!(
        exported.status.success(),
        "{}",
        String::from_utf8_lossy(&exported.stderr)
    );

    assert_eq!(json(&db, &["init", "--json"])["schema_version"], 10);
    let rejected = run_rc7(&["init"]);
    assert!(!rejected.status.success());
    assert!(String::from_utf8_lossy(&rejected.stderr).contains("schema 10 is newer"));
    assert_eq!(user_version(&db), 10);
    let list = ["memory", "list", "--namespace", "project:fixture", "--json"];
    assert_eq!(json(&db, &list)[0]["content"], "retained rc7 record");
    let imported = temp.path().join("imported.sqlite3");
    assert_eq!(
        json(
            &imported,
            &["memory", "import", export.to_str().unwrap(), "--json"]
        )["imported"],
        1
    );
    assert_eq!(json(&imported, &list)[0]["origin"], "agent");
}
