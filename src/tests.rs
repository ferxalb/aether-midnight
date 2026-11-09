use std::fs;
use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Barrier};

use tempfile::TempDir;

use crate::{code, db, memory};

#[test]
fn code_literal_phrase_window_uses_one_rank_basis_at_every_limit() {
    let temp = TempDir::new().unwrap();
    let repo = new_repo(&temp, "phrase-window");
    let database = temp.path().join("m.sqlite3");
    for i in 0..30 {
        fs::write(
            repo.join(format!("a_{i:02}.txt")),
            "privacy scattered first scattered personal scattered intelligence\n",
        )
        .unwrap();
    }
    for (name, padding) in [
        ("z_short.txt", 0),
        ("z_medium.txt", 30),
        ("z_long.txt", 300),
    ] {
        fs::write(
            repo.join(name),
            format!(
                "{}\nPrivacy-first personal intelligence.\n",
                "unrelated filler\n".repeat(padding)
            ),
        )
        .unwrap();
    }
    code::index_repository(&database, &repo, false, false, &AtomicBool::new(false)).unwrap();
    let query = "Privacy-first personal intelligence";
    let full = code::search_code(&database, &repo, query, 20).unwrap();
    assert!(
        full[..3]
            .iter()
            .all(|r| r.match_reason.contains("literal_phrase"))
    );
    for limit in [1, 2, 3, 10] {
        let limited = code::search_code(&database, &repo, query, limit).unwrap();
        assert_eq!(
            limited.iter().map(|r| &r.path).collect::<Vec<_>>(),
            full[..limited.len()]
                .iter()
                .map(|r| &r.path)
                .collect::<Vec<_>>(),
            "limit={limit}"
        );
    }
}

#[test]
fn code_phrase_class_requires_literal_source_punctuation_and_boundaries() {
    let temp = TempDir::new().unwrap();
    let repo = new_repo(&temp, "phrase-literal");
    let database = temp.path().join("m.sqlite3");
    for (name, content) in [
        ("exact.txt", "PRIVACY-first personal intelligence."),
        ("punctuation.txt", "Privacy - first personal intelligence."),
        (
            "infix.txt",
            "prefixPrivacy-first personal intelligenceSuffix",
        ),
    ] {
        fs::write(repo.join(name), content).unwrap();
    }
    code::index_repository(&database, &repo, false, false, &AtomicBool::new(false)).unwrap();
    let results =
        code::search_code(&database, &repo, "Privacy-first personal intelligence", 10).unwrap();
    let protected: Vec<_> = results
        .iter()
        .filter(|r| r.match_reason.contains("literal_phrase"))
        .map(|r| r.path.as_str())
        .collect();
    assert_eq!(protected, vec!["exact.txt"]);
}

fn new_repo(base: &TempDir, name: &str) -> std::path::PathBuf {
    let root = base.path().join(name);
    fs::create_dir_all(root.join("src")).unwrap();
    root
}

fn run_index(database: &Path, repo: &Path) -> code::IndexReport {
    code::index_repository(database, repo, false, false, &AtomicBool::new(false)).unwrap()
}

#[test]
fn indexing_reuses_batch_statements_across_catalog_and_symbol_rows() {
    let temp = TempDir::new().unwrap();
    let database = temp.path().join("batch.sqlite3");
    let repo = new_repo(&temp, "batch-repo");
    for index in 0..513 {
        fs::write(
            repo.join("src").join(format!("{index:04}.rs")),
            format!("pub fn batch_symbol_{index:04}() {{}}\n"),
        )
        .unwrap();
    }

    let first = run_index(&database, &repo);
    assert!(first.catalog_complete);
    assert!(first.structural_complete);
    assert_eq!(first.eligible_files, 513);
    assert!(
        code::search_code(&database, &repo, "batch_symbol_0512", 5)
            .unwrap()
            .iter()
            .any(|result| result.path == "src/0512.rs" && result.freshness == "fresh")
    );

    fs::write(
        repo.join("src/0000.rs"),
        "pub fn reindexed_symbol_0000() {}\n",
    )
    .unwrap();
    let second = run_index(&database, &repo);
    assert!(second.catalog_complete);
    assert!(second.structural_complete);
    assert!(second.generation > first.generation);
    assert!(
        code::search_code(&database, &repo, "reindexed_symbol_0000", 5)
            .unwrap()
            .iter()
            .any(|result| result.path == "src/0000.rs" && result.freshness == "fresh")
    );
    assert!(
        code::search_code(&database, &repo, "batch_symbol_0000", 5)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn migrations_and_interrupted_transaction_recover() {
    let temp = TempDir::new().unwrap();
    let database = temp.path().join("db.sqlite3");
    let conn = db::open_db(&database).unwrap();
    let version: i64 = conn
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    assert_eq!(version, 7);
    let tx = conn.unchecked_transaction().unwrap();
    tx.execute(
        "INSERT INTO metadata(key,value) VALUES('uncommitted-test','x')",
        [],
    )
    .unwrap();
    drop(tx);
    drop(conn);
    let reopened = db::open_db(&database).unwrap();
    let exists: bool = reopened
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM metadata WHERE key='uncommitted-test')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(!exists);
    let format: String = reopened
        .query_row(
            "SELECT value FROM metadata WHERE key='index_format_version'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(format, "2");
    let schema: String = reopened
        .query_row(
            "SELECT value FROM metadata WHERE key='schema_version'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(schema, "7");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&database).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}

#[test]
fn trigram_format_migration_discards_unrebuildable_postings_and_marks_reindex_needed() {
    let temp = TempDir::new().unwrap();
    let database = temp.path().join("legacy.sqlite3");
    let legacy = rusqlite::Connection::open(&database).unwrap();
    legacy
        .execute_batch(
            "CREATE TABLE metadata(key TEXT PRIMARY KEY,value TEXT NOT NULL);
             INSERT INTO metadata(key,value) VALUES('index_format_version','1'),('schema_version','1');
             CREATE TABLE repositories(
                 repo_id TEXT,worktree_id TEXT,is_complete INTEGER,structural_complete INTEGER,
                 eligible_files INTEGER,eligible_bytes INTEGER,skipped_json TEXT
             );
             INSERT INTO repositories VALUES('repo','worktree',1,1,1,32,'{}');
             CREATE TABLE code_files(id INTEGER PRIMARY KEY);
             INSERT INTO code_files(id) VALUES(1);
             CREATE VIRTUAL TABLE code_trigram USING fts5(
                 content,content='',contentless_delete=1,tokenize='trigram case_sensitive 0'
             );
             INSERT INTO code_trigram(rowid,content) VALUES(1,'legacy trigram content');
             CREATE TABLE memories(
                 id INTEGER PRIMARY KEY,scope TEXT NOT NULL,kind TEXT NOT NULL,content TEXT NOT NULL,
                 source_uri TEXT,source_id INTEGER,supersedes_id INTEGER,created_at TEXT NOT NULL,
                 observed_at TEXT,valid_from TEXT,valid_to TEXT,deleted_at TEXT
             );
             CREATE VIRTUAL TABLE memory_fts USING fts5(content,tokenize='unicode61 remove_diacritics 2');
             CREATE TRIGGER memories_delete_fts AFTER DELETE ON memories BEGIN
                 DELETE FROM memory_fts WHERE rowid=old.id;
             END;
             CREATE TRIGGER memories_soft_delete_fts AFTER UPDATE OF deleted_at ON memories
             WHEN old.deleted_at IS NULL AND new.deleted_at IS NOT NULL BEGIN
                 DELETE FROM memory_fts WHERE rowid=new.id;
             END;
             INSERT INTO memories(id,scope,kind,content,source_uri,created_at)
                 VALUES(10,'user:legacy','source','legacy memory provenance fact','legacy://source','2025-01-01T00:00:00.000000000Z');
             INSERT INTO memory_fts(rowid,content) VALUES(10,'legacy memory provenance fact');
             PRAGMA user_version=1;",
        )
        .unwrap();
    drop(legacy);

    let migrated = db::open_db(&database).unwrap();
    let version: i64 = migrated
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    assert_eq!(version, 7);
    let (format, old_table, new_table, files, complete): (String, bool, bool, i64, bool) = migrated
        .query_row(
            "SELECT
                 (SELECT value FROM metadata WHERE key='index_format_version'),
                 EXISTS(SELECT 1 FROM sqlite_master WHERE name='code_trigram'),
                 EXISTS(SELECT 1 FROM sqlite_master WHERE name='code_fts'),
                 (SELECT count(*) FROM code_files),
                 (SELECT is_complete AND structural_complete FROM repositories WHERE repo_id='repo')",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
        )
        .unwrap();
    assert_eq!(format, "2");
    assert!(!old_table);
    assert!(new_table);
    assert_eq!(files, 0);
    assert!(!complete);
    assert!(
        memory::recall(&database, "user:legacy", "legacy provenance", None, 10)
            .unwrap()
            .iter()
            .any(|result| result.memory.id == 10
                && result.memory.source_uri.as_deref() == Some("legacy://source"))
    );
}

#[test]
fn schema_two_memory_fts_migration_preserves_history_rebuilds_scope_tokens_and_is_idempotent() {
    let temp = TempDir::new().unwrap();
    let database = temp.path().join("schema-two.sqlite3");
    let legacy = rusqlite::Connection::open(&database).unwrap();
    legacy
        .execute_batch(
            "CREATE TABLE metadata(key TEXT PRIMARY KEY,value TEXT NOT NULL);
             INSERT INTO metadata(key,value) VALUES('index_format_version','2'),('schema_version','2');
             CREATE TABLE code_files(id INTEGER PRIMARY KEY,content_hash TEXT NOT NULL);
             CREATE TABLE memories(
                 id INTEGER PRIMARY KEY,scope TEXT NOT NULL,kind TEXT NOT NULL,content TEXT NOT NULL,
                 source_uri TEXT,source_id INTEGER,supersedes_id INTEGER,created_at TEXT NOT NULL,
                 observed_at TEXT,valid_from TEXT,valid_to TEXT,deleted_at TEXT
             );
             CREATE INDEX memories_scope_active ON memories(scope,deleted_at,created_at);
             CREATE INDEX memories_source ON memories(source_id);
             CREATE VIRTUAL TABLE memory_fts USING fts5(content,tokenize='unicode61 remove_diacritics 2');
             CREATE TRIGGER memories_delete_fts AFTER DELETE ON memories BEGIN
                 DELETE FROM memory_fts WHERE rowid=old.id;
             END;
             CREATE TRIGGER memories_soft_delete_fts AFTER UPDATE OF deleted_at ON memories
             WHEN old.deleted_at IS NULL AND new.deleted_at IS NOT NULL BEGIN
                 DELETE FROM memory_fts WHERE rowid=new.id;
             END;
             INSERT INTO memories(id,scope,kind,content,source_uri,created_at,valid_from)
                 VALUES(1,'user:alice','source','shared preference morning review','uri://source','2025-01-01T00:00:00.000000000Z','2024-01-01T00:00:00.000000000Z');
             INSERT INTO memories(id,scope,kind,content,source_id,supersedes_id,created_at,valid_from)
                 VALUES(2,'user:bob','derived','shared preference evening review',1,1,'2025-02-01T00:00:00.000000000Z','2025-01-01T00:00:00.000000000Z');
             INSERT INTO memories(id,scope,kind,content,created_at,deleted_at)
                 VALUES(3,'user:alice','source','removed forgotten memory','2025-03-01T00:00:00.000000000Z','2025-04-01T00:00:00.000000000Z');
             INSERT INTO memory_fts(rowid,content) VALUES
                 (1,'shared preference morning review'),(2,'shared preference evening review');
             PRAGMA user_version=2;",
        )
        .unwrap();
    drop(legacy);

    let original = database;
    let database = temp.path().join("migration-copy.sqlite3");
    fs::copy(&original, &database).unwrap();
    let original_bytes = fs::read(&original).unwrap();
    let migrated = db::open_db(&database).unwrap();
    assert_eq!(fs::read(&original).unwrap(), original_bytes);
    let integrity: String = migrated
        .query_row("PRAGMA integrity_check", [], |r| r.get(0))
        .unwrap();
    assert_eq!(integrity, "ok");
    let version: i64 = migrated
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    assert_eq!(version, 7);
    let schema_version: String = migrated
        .query_row(
            "SELECT value FROM metadata WHERE key='schema_version'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(schema_version, "7");
    let (records, source_uri, source_link, supersedes_link, deleted_at, content_column_is_null): (
        i64,
        String,
        i64,
        i64,
        String,
        bool,
    ) = migrated
        .query_row(
            "SELECT count(*),
                    (SELECT source_uri FROM memories WHERE id=1),
                    (SELECT source_id FROM memories WHERE id=2),
                    (SELECT supersedes_id FROM memories WHERE id=2),
                    (SELECT deleted_at FROM memories WHERE id=3),
                    (SELECT content IS NULL FROM memory_fts WHERE rowid=1)
             FROM memories",
            [],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(records, 3);
    assert_eq!(source_uri, "uri://source");
    assert_eq!(source_link, 1);
    assert_eq!(supersedes_link, 1);
    assert!(deleted_at.contains("2025-04-01"));
    assert!(content_column_is_null);

    let alice_results = memory::recall(
        &database,
        "user:alice",
        "shared preference morning review",
        None,
        10,
    )
    .unwrap();
    assert_eq!(alice_results.len(), 1);
    assert_eq!(alice_results[0].memory.id, 1);
    assert!(
        memory::recall(
            &database,
            "user:alice",
            "removed forgotten memory",
            None,
            10
        )
        .unwrap()
        .is_empty()
    );
    assert_eq!(
        memory::recall(
            &database,
            "user:bob",
            "shared preference evening review",
            None,
            10
        )
        .unwrap()[0]
            .memory
            .id,
        2
    );
    drop(migrated);

    let reopened = db::open_db(&database).unwrap();
    let reopened_version: i64 = reopened
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    assert_eq!(reopened_version, 7);
    assert_eq!(
        memory::recall(&database, "user:alice", "morning review", None, 10)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn schema_two_memory_fts_migration_rolls_back_on_invalid_legacy_rows() {
    let temp = TempDir::new().unwrap();
    let database = temp.path().join("invalid-schema-two.sqlite3");
    let legacy = rusqlite::Connection::open(&database).unwrap();
    legacy
        .execute_batch(
            "CREATE TABLE metadata(key TEXT PRIMARY KEY,value TEXT NOT NULL);
             INSERT INTO metadata(key,value) VALUES('index_format_version','2'),('schema_version','2');
             CREATE TABLE memories(id INTEGER PRIMARY KEY,content TEXT,deleted_at TEXT);
             CREATE VIRTUAL TABLE memory_fts USING fts5(content);
             INSERT INTO memories(id,content) VALUES(1,'preserve old row');
             INSERT INTO memory_fts(rowid,content) VALUES(1,'preserve old row');
             PRAGMA user_version=2;",
        )
        .unwrap();
    drop(legacy);

    assert!(db::open_db(&database).is_err());
    let reopened = rusqlite::Connection::open(&database).unwrap();
    let (version, old_fts, old_row): (i64, bool, bool) = reopened
        .query_row(
            "SELECT (SELECT user_version FROM pragma_user_version),
                    EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='memory_fts'),
                    EXISTS(SELECT 1 FROM memories WHERE id=1 AND content='preserve old row')",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(version, 2);
    assert!(old_fts);
    assert!(old_row);
}

#[test]
fn memory_batch_is_bounded_and_rolls_back_every_record_when_a_link_is_invalid() {
    let temp = TempDir::new().unwrap();
    let database = temp.path().join("memory-batch.sqlite3");
    let inputs = vec![
        memory::MemoryInput {
            scope: "user:batch".into(),
            kind: "source".into(),
            content: "valid first source".into(),
            source_uri: Some("batch://valid".into()),
            source_id: None,
            supersedes_id: None,
            observed_at: None,
            valid_from: None,
            valid_to: None,
        },
        memory::MemoryInput {
            scope: "user:batch".into(),
            kind: "derived".into(),
            content: "invalid second fact".into(),
            source_uri: None,
            source_id: Some(999),
            supersedes_id: None,
            observed_at: None,
            valid_from: None,
            valid_to: None,
        },
    ];
    assert!(memory::remember_batch(&database, &inputs).is_err());
    let connection = db::open_db(&database).unwrap();
    let record_count: i64 = connection
        .query_row("SELECT count(*) FROM memories", [], |row| row.get(0))
        .unwrap();
    let indexed_count: i64 = connection
        .query_row("SELECT count(*) FROM memory_fts", [], |row| row.get(0))
        .unwrap();
    assert_eq!(record_count, 0);
    assert_eq!(indexed_count, 0);

    let too_many = vec![inputs[0].clone(); memory::MAX_MEMORY_BATCH_RECORDS + 1];
    assert!(memory::remember_batch(&database, &too_many).is_err());
}

#[test]
fn memory_timestamps_normalize_to_lexically_ordered_utc() {
    assert_eq!(
        db::normalize_timestamp("2025-01-01T01:00:00+01:00").unwrap(),
        "2025-01-01T00:00:00.000000000Z"
    );
    assert!(db::normalize_timestamp("yesterday").is_err());
}

#[test]
fn repository_search_isolated_by_worktree_and_repo() {
    let temp = TempDir::new().unwrap();
    let database = temp.path().join("scope.sqlite3");
    let a = new_repo(&temp, "repo-a");
    let b = new_repo(&temp, "repo-b");
    fs::write(a.join("src/a.rs"), "pub fn private_moon_token() {}\n").unwrap();
    fs::write(b.join("src/b.rs"), "pub fn another_scope() {}\n").unwrap();
    run_index(&database, &a);
    run_index(&database, &b);
    assert!(
        code::search_code(&database, &a, "private_moon_token", 5)
            .unwrap()
            .iter()
            .any(|r| r.path == "src/a.rs")
    );
    assert!(
        code::search_code(&database, &b, "private_moon_token", 5)
            .unwrap()
            .is_empty()
    );

    let git_root = temp.path().join("git-main");
    fs::create_dir_all(git_root.join("src")).unwrap();
    fs::write(
        git_root.join("src/base.rs"),
        "pub fn shared_baseline() {}\n",
    )
    .unwrap();
    let git = |dir: &Path, args: &[&str]| {
        let result = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "git {:?}: {}",
            args,
            String::from_utf8_lossy(&result.stderr)
        );
    };
    git(&git_root, &["init", "-b", "main"]);
    git(
        &git_root,
        &["config", "user.email", "midnight-test@example.invalid"],
    );
    git(&git_root, &["config", "user.name", "Midnight Test"]);
    git(&git_root, &["add", "src/base.rs"]);
    git(&git_root, &["commit", "-m", "fixture"]);
    let worktree = temp.path().join("git-worktree");
    git(
        &git_root,
        &[
            "worktree",
            "add",
            "-b",
            "feature",
            worktree.to_str().unwrap(),
        ],
    );
    fs::write(
        worktree.join("src/only.rs"),
        "pub fn worktree_secret_token() {}\n",
    )
    .unwrap();
    let main_scope = db::repo_scope(&git_root).unwrap();
    let worktree_scope = db::repo_scope(&worktree).unwrap();
    assert_eq!(main_scope.repo_id, worktree_scope.repo_id);
    assert_ne!(main_scope.worktree_id, worktree_scope.worktree_id);
    assert_eq!(main_scope.branch.as_deref(), Some("main"));
    assert_eq!(worktree_scope.branch.as_deref(), Some("feature"));
    assert!(main_scope.revision.is_some());
    run_index(&database, &git_root);
    run_index(&database, &worktree);
    assert!(
        code::search_code(&database, &worktree, "worktree_secret_token", 5)
            .unwrap()
            .iter()
            .any(|r| r.path == "src/only.rs")
    );
    assert!(
        code::search_code(&database, &git_root, "worktree_secret_token", 5)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn fts_symbols_paths_and_retrieval_time_freshness_work() {
    let temp = TempDir::new().unwrap();
    let database = temp.path().join("db.sqlite3");
    let repo = new_repo(&temp, "repo");
    let source = repo.join("src/lib.rs");
    fs::write(
        &source,
        "pub fn exact_symbol_name() {\n    // three character lexical phrase\n}\n",
    )
    .unwrap();
    let report = run_index(&database, &repo);
    assert!(report.complete);
    let symbol = code::search_code(&database, &repo, "exact_symbol_name", 5).unwrap();
    assert_eq!(symbol[0].path, "src/lib.rs");
    assert_eq!(symbol[0].freshness, "fresh");
    assert_eq!(symbol[0].line_start, Some(1));
    let lexical = code::search_code(&database, &repo, "lexical phrase", 5).unwrap();
    assert_eq!(lexical[0].match_reason, "literal_phrase+lexical");
    assert!(
        !code::search_code(&database, &repo, "fn", 5)
            .unwrap()
            .is_empty(),
        "unicode61 must support short lexical terms"
    );
    assert_eq!(
        code::list_symbols(&database, &repo, Some("exact_symbol_name"), 5)
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        code::search_code(&database, &repo, "zz", 5).unwrap().len(),
        0
    );

    fs::write(&source, "pub fn new_fresh_symbol() {}\n").unwrap();
    let stale = code::search_code(&database, &repo, "exact_symbol_name", 5).unwrap();
    assert_eq!(stale[0].freshness, "stale");
    assert_eq!(stale[0].line_start, None);
    assert!(stale[0].snippet.is_none());
    let opened = code::open_range(&database, &repo, "src/lib.rs", 1, 1).unwrap();
    assert_eq!(opened.freshness, "stale");
    assert!(run_index(&database, &repo).complete);
    let fresh = code::search_code(&database, &repo, "new_fresh_symbol", 5).unwrap();
    assert_eq!(fresh[0].freshness, "fresh");
    fs::remove_file(source).unwrap();
    run_index(&database, &repo);
    assert_eq!(
        code::index_status(&database, &repo).unwrap().indexed_files,
        0
    );
    assert!(
        code::search_code(&database, &repo, "new_fresh_symbol", 5)
            .unwrap()
            .is_empty()
    );
    let conn = db::open_db(&database).unwrap();
    let fts_documents: i64 = conn
        .query_row("SELECT count(*) FROM code_fts", [], |row| row.get(0))
        .unwrap();
    assert_eq!(fts_documents, 0);
}

#[test]
fn explicit_file_refresh_updates_one_path_and_requires_full_reconciliation() {
    let temp = TempDir::new().unwrap();
    let database = temp.path().join("refresh.sqlite3");
    let repo = new_repo(&temp, "refresh-repo");
    let source = repo.join("src/lib.rs");
    fs::write(&source, "pub fn before_refresh_token() {}\n").unwrap();
    let initial = run_index(&database, &repo);
    assert!(initial.complete);

    fs::write(&source, "pub fn after_refresh_token() {}\n").unwrap();
    let refreshed =
        code::refresh_indexed_file(&database, &repo, "src/lib.rs", &AtomicBool::new(false))
            .unwrap();
    assert_eq!(refreshed.operation, "updated");
    assert!(refreshed.generation > initial.generation);
    assert!(!refreshed.catalog_complete);
    assert!(refreshed.reconciliation_required);
    assert_eq!(
        code::search_code(&database, &repo, "after_refresh_token", 5).unwrap()[0].freshness,
        "fresh"
    );
    assert!(
        code::search_code(&database, &repo, "before_refresh_token", 5)
            .unwrap()
            .is_empty()
    );
    assert!(
        !code::index_status(&database, &repo)
            .unwrap()
            .catalog_complete
    );

    let added = repo.join("src/added.rs");
    fs::write(&added, "pub fn added_refresh_token() {}\n").unwrap();
    let added_report =
        code::refresh_indexed_file(&database, &repo, "src/added.rs", &AtomicBool::new(false))
            .unwrap();
    assert_eq!(added_report.operation, "updated");
    assert_eq!(
        code::search_code(&database, &repo, "added_refresh_token", 5).unwrap()[0].freshness,
        "fresh"
    );

    fs::write(repo.join(".gitignore"), "src/added.rs\n").unwrap();
    let ignored =
        code::refresh_indexed_file(&database, &repo, "src/added.rs", &AtomicBool::new(false))
            .unwrap();
    assert_eq!(ignored.operation, "removed");
    assert!(
        code::search_code(&database, &repo, "added_refresh_token", 5)
            .unwrap()
            .is_empty()
    );

    assert!(
        code::refresh_indexed_file(&database, &repo, "../outside.rs", &AtomicBool::new(false),)
            .is_err()
    );
    assert!(
        code::refresh_indexed_file(&database, &repo, "..\\outside.rs", &AtomicBool::new(false),)
            .is_err()
    );
    assert!(run_index(&database, &repo).complete);
    fs::remove_file(source).unwrap();
    let deleted =
        code::refresh_indexed_file(&database, &repo, "src/lib.rs", &AtomicBool::new(false))
            .unwrap();
    assert_eq!(deleted.operation, "removed");
    assert!(
        code::search_code(&database, &repo, "after_refresh_token", 5)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn fresh_process_fallback_returns_one_useful_partial_result_early() {
    let temp = TempDir::new().unwrap();
    let database = temp.path().join("empty-index.sqlite3");
    let repo = new_repo(&temp, "fallback-repo");
    fs::write(
        repo.join("src/first.rs"),
        "pub fn launch_guardrail_token() {}\n",
    )
    .unwrap();
    fs::write(
        repo.join("src/second.rs"),
        "pub fn launch_guardrail_token() {}\n",
    )
    .unwrap();
    let response =
        code::search_code_with_budget(&database, &repo, "launch_guardrail_token", 5, 1024).unwrap();
    assert_eq!(response.results.len(), 1);
    assert!(response.truncated);
    assert_eq!(response.results[0].freshness, "fresh_unindexed");
    assert!(!response.results[0].index_complete);
}

#[test]
fn oversized_aetherignore_is_bounded_and_reported() {
    let temp = TempDir::new().unwrap();
    let database = temp.path().join("ignore.sqlite3");
    let repo = new_repo(&temp, "ignore-repo");
    fs::write(repo.join(".aetherignore"), vec![b'#'; 1_048_577]).unwrap();
    fs::write(
        repo.join("src/visible.rs"),
        "pub fn visible_after_ignore_limit() {}\n",
    )
    .unwrap();
    let report = run_index(&database, &repo);
    assert!(report.complete);
    assert_eq!(report.skipped.get("aetherignore_oversized"), Some(&1));
    assert!(
        code::search_code(&database, &repo, "visible_after_ignore_limit", 5)
            .unwrap()
            .iter()
            .any(|result| result.path == "src/visible.rs")
    );
}

#[test]
fn code_context_is_bounded_by_the_reported_estimated_token_budget() {
    let temp = TempDir::new().unwrap();
    let database = temp.path().join("budget.sqlite3");
    let repo = new_repo(&temp, "repo");
    let body = format!(
        "pub fn budget_marker() {{}} // {}\n",
        "long_identifier_fragment ".repeat(300)
    );
    fs::write(repo.join("src/context.rs"), body).unwrap();
    run_index(&database, &repo);
    let response =
        code::search_code_with_budget(&database, &repo, "budget_marker", 10, 128).unwrap();
    assert_eq!(response.token_budget, 128);
    assert!(response.estimated_tokens <= response.token_budget);
    assert!(response.truncated);
    assert!(response.results[0].snippet_truncated);
    assert!(response.results[0].estimated_tokens <= response.token_budget);
}

#[test]
fn tree_sitter_extracts_supported_python_and_typescript_symbols() {
    let temp = TempDir::new().unwrap();
    let database = temp.path().join("syntax.sqlite3");
    let repo = new_repo(&temp, "repo");
    fs::write(
        repo.join("src/module.py"),
        "class ParserThing:\n    def parse_token(self):\n        return None\n",
    )
    .unwrap();
    fs::write(
        repo.join("src/app.ts"),
        "export interface UserState { id: string }\nexport function buildState() { return {}; }\n",
    )
    .unwrap();
    let report = run_index(&database, &repo);
    assert!(report.complete);
    let python = code::list_symbols(&database, &repo, Some("parse_token"), 10).unwrap();
    assert_eq!(python.len(), 1);
    assert_eq!(python[0]["freshness"], "fresh");
    let typescript = code::list_symbols(&database, &repo, Some("buildState"), 10).unwrap();
    assert_eq!(typescript.len(), 1);
    assert_eq!(typescript[0]["kind"], "function_declaration");
}

#[test]
fn indexed_identifier_subwords_match_camel_case_fragments() {
    let temp = TempDir::new().unwrap();
    let database = temp.path().join("identifier-fragments.sqlite3");
    let repo = new_repo(&temp, "identifier-fragments");
    fs::write(
        repo.join("src/client.ts"),
        "export function refreshToken() { return 'ok'; }\nexport function buildHTTP2Client() { return {}; }\n// retryToken refreshToken glossary\n",
    )
    .unwrap();
    let report = run_index(&database, &repo);
    assert!(report.complete);
    let results = code::search_code(&database, &repo, "refreshTok", 5).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].path, "src/client.ts");
    assert_eq!(results[0].freshness, "fresh");
    let normalized = code::search_code(&database, &repo, "http2 client", 5).unwrap();
    assert_eq!(normalized.len(), 1);
    assert_eq!(normalized[0].path, "src/client.ts");
}

#[test]
fn parse_errors_keep_catalog_complete_and_flag_structural_separately() {
    let temp = TempDir::new().unwrap();
    let database = temp.path().join("parse-error.sqlite3");
    let repo = new_repo(&temp, "malformed-repo");
    fs::write(repo.join("src/good.rs"), "pub fn good_symbol() {}\n").unwrap();
    fs::write(
        repo.join("src/broken.rs"),
        "pub fn broken_symbol() { let = ; }\n// BROKEN_TEXT_MARKER lives only in a comment\n",
    )
    .unwrap();

    let report = run_index(&database, &repo);
    assert!(report.complete);
    assert!(report.catalog_complete);
    assert!(!report.structural_complete);
    assert_eq!(report.skipped.get("tree_sitter_parse_error"), Some(&1));

    let status = code::index_status(&database, &repo).unwrap();
    assert!(status.complete);
    assert!(status.catalog_complete);
    assert!(!status.structural_complete);
    assert_eq!(status.parse_error_files, 1);
    assert_eq!(status.parse_error_paths, vec!["src/broken.rs".to_string()]);

    // Text retrieval over the file with the parse error stays complete.
    let found = code::search_code(&database, &repo, "BROKEN_TEXT_MARKER", 5).unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].path, "src/broken.rs");
    assert!(found[0].index_complete);
    assert!(
        code::open_range(&database, &repo, "src/broken.rs", 1, 1)
            .unwrap()
            .index_complete
    );

    // Symbol surfaces keep reporting that structural data is partial.
    assert!(!code::repo_map(&database, &repo).unwrap().complete);
    let good = code::list_symbols(&database, &repo, Some("good_symbol"), 5).unwrap();
    assert_eq!(good.len(), 1);
    assert_eq!(good[0]["index_complete"], false);
    // No symbol is invented for text that is not a declaration.
    assert!(
        code::list_symbols(&database, &repo, Some("BROKEN_TEXT_MARKER"), 5)
            .unwrap()
            .is_empty()
    );

    // Fixing the file clears the flag on the next index.
    fs::write(repo.join("src/broken.rs"), "pub fn fixed_symbol() {}\n").unwrap();
    let fixed = run_index(&database, &repo);
    assert!(fixed.complete && fixed.structural_complete);
    let status = code::index_status(&database, &repo).unwrap();
    assert_eq!(status.parse_error_files, 0);
    assert!(status.parse_error_paths.is_empty());
}

#[test]
fn discovery_honors_ignores_security_limits_and_safe_override() {
    let temp = TempDir::new().unwrap();
    let database = temp.path().join("db.sqlite3");
    let repo = new_repo(&temp, "repo");
    fs::write(repo.join(".gitignore"), "ignored.rs\n").unwrap();
    fs::write(repo.join(".aetherignore"), "private.rs\n").unwrap();
    fs::write(repo.join("src/normal.rs"), "pub fn normal_marker() {}\n").unwrap();
    fs::write(repo.join("ignored.rs"), "pub fn ignored_marker() {}\n").unwrap();
    fs::write(repo.join("private.rs"), "pub fn private_marker() {}\n").unwrap();
    fs::create_dir_all(repo.join("target")).unwrap();
    fs::write(
        repo.join("target/generated.rs"),
        "pub fn generated_marker() {}\n",
    )
    .unwrap();
    fs::create_dir_all(repo.join("node_modules/pkg")).unwrap();
    fs::write(
        repo.join("node_modules/pkg/dependency.rs"),
        "pub fn dependency_marker() {}\n",
    )
    .unwrap();
    fs::write(repo.join("id_rsa"), "private key content\n").unwrap();
    fs::write(repo.join(".env.local"), "API_TOKEN=secret\n").unwrap();
    fs::create_dir_all(repo.join(".docker")).unwrap();
    fs::write(
        repo.join(".docker/config.json"),
        "{\"auths\":{\"example.invalid\":\"docker_secret_marker\"}}\n",
    )
    .unwrap();
    fs::create_dir_all(repo.join(".git")).unwrap();
    fs::write(
        repo.join(".git/config"),
        "url = https://user:git_secret_marker@example.invalid/repo\n",
    )
    .unwrap();
    fs::write(repo.join("blob.bin"), b"a\0b").unwrap();
    fs::write(
        repo.join("control.bin"),
        b"pub fn binary_control_marker() {}\x01\n",
    )
    .unwrap();
    fs::write(repo.join("large.txt"), vec![b'x'; 1_048_577]).unwrap();
    fs::write(repo.join("src/malformed.rs"), "pub fn malformed(\n").unwrap();
    let outside = temp.path().join("outside.rs");
    fs::write(&outside, "pub fn escaped_marker() {}\n").unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(&outside, repo.join("escape.rs")).unwrap();
    #[cfg(unix)]
    {
        let outside_dir = temp.path().join("outside-dir");
        fs::create_dir_all(&outside_dir).unwrap();
        fs::write(
            outside_dir.join("nested.rs"),
            "pub fn escaped_directory_marker() {}\n",
        )
        .unwrap();
        std::os::unix::fs::symlink(&outside_dir, repo.join("escape-dir")).unwrap();
    }
    let report = run_index(&database, &repo);
    assert!(report.catalog_complete);
    assert!(!report.structural_complete);
    assert!(report.complete);
    assert!(
        report
            .skipped
            .get("tree_sitter_parse_error")
            .copied()
            .unwrap_or(0)
            >= 1
    );
    assert!(
        report
            .skipped
            .get("credential_or_key")
            .copied()
            .unwrap_or(0)
            >= 2
    );
    assert!(report.skipped.get("oversized").copied().unwrap_or(0) >= 1);
    assert!(report.skipped.get("binary_nul").copied().unwrap_or(0) >= 1);
    assert!(
        report
            .skipped
            .get("binary_control_bytes")
            .copied()
            .unwrap_or(0)
            >= 1
    );
    for query in [
        "ignored_marker",
        "private_marker",
        "generated_marker",
        "dependency_marker",
        "escaped_marker",
        "API_TOKEN",
        "docker_secret_marker",
        "binary_control_marker",
    ] {
        assert!(
            code::search_code(&database, &repo, query, 5)
                .unwrap()
                .is_empty(),
            "unexpected result for {query}"
        );
    }
    assert!(
        !code::search_code(&database, &repo, "malformed", 5)
            .unwrap()
            .is_empty()
    );
    assert!(code::open_range(&database, &repo, "../outside.rs", 1, 1).is_err());
    #[cfg(unix)]
    assert!(code::open_range(&database, &repo, "escape.rs", 1, 1).is_err());
    #[cfg(unix)]
    assert!(code::open_range(&database, &repo, "escape-dir/nested.rs", 1, 1).is_err());
    let generated =
        code::index_repository(&database, &repo, true, false, &AtomicBool::new(false)).unwrap();
    assert!(generated.catalog_complete);
    assert!(!generated.structural_complete);
    assert!(
        !code::search_code(&database, &repo, "generated_marker", 5)
            .unwrap()
            .is_empty()
    );
    assert!(
        code::search_code(&database, &repo, "API_TOKEN", 5)
            .unwrap()
            .is_empty(),
        "generated override must never include credentials"
    );
    assert!(
        code::search_code(&database, &repo, "git_secret_marker", 5)
            .unwrap()
            .is_empty(),
        "generated override must never include VCS metadata"
    );
}

#[test]
fn cancellation_stays_partial_and_reconciliation_recovers() {
    let temp = TempDir::new().unwrap();
    let database = temp.path().join("db.sqlite3");
    let repo = new_repo(&temp, "repo");
    fs::write(repo.join("src/a.rs"), "pub fn cancellation_marker() {}\n").unwrap();
    let cancelled =
        code::index_repository(&database, &repo, false, false, &AtomicBool::new(true)).unwrap();
    assert!(cancelled.cancelled && !cancelled.complete);
    assert!(!code::index_status(&database, &repo).unwrap().complete);
    let recovered = run_index(&database, &repo);
    assert!(recovered.complete);
    let conn = db::open_db(&database).unwrap();
    let (files, fts): (i64, i64) = conn
        .query_row(
            "SELECT (SELECT count(*) FROM code_files),(SELECT count(*) FROM code_fts)",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(files, fts);
}

#[test]
fn cancellation_during_pipeline_stages_stays_partial_and_recovers() {
    for stage in [
        code::IndexStage::Discovery,
        code::IndexStage::FileRead,
        code::IndexStage::Parsing,
        code::IndexStage::QueueBackpressure,
        code::IndexStage::SqliteBatchWrite,
    ] {
        let temp = TempDir::new().unwrap();
        let database = temp.path().join("db.sqlite3");
        let repo = new_repo(&temp, "repo");
        match stage {
            code::IndexStage::QueueBackpressure => {
                for index in 0..32 {
                    fs::write(
                        repo.join(format!("src/{index:03}.txt")),
                        "lowercase queue payload ".repeat(1024),
                    )
                    .unwrap();
                }
            }
            code::IndexStage::SqliteBatchWrite => {
                for index in 0..512 {
                    fs::write(
                        repo.join(format!("src/{index:03}.rs")),
                        format!("pub fn batch_marker_{index}() {{}}\n"),
                    )
                    .unwrap();
                }
            }
            code::IndexStage::FileRead => {
                fs::write(
                    repo.join("src/a.rs"),
                    format!(
                        "// {}\npub fn cancellation_marker() {{}}\n",
                        "x".repeat(256 * 1024)
                    ),
                )
                .unwrap();
            }
            _ => {
                fs::write(repo.join("src/a.rs"), "pub fn cancellation_marker() {}\n").unwrap();
            }
        }
        let cancellation = Arc::new(AtomicBool::new(false));
        let fired = Arc::new(AtomicBool::new(false));
        let cancellation_for_hook = cancellation.clone();
        let fired_for_hook = fired.clone();
        let hook: code::IndexStageHook = Arc::new(move |observed| {
            if stage == code::IndexStage::QueueBackpressure
                && observed == code::IndexStage::FileRead
            {
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            if observed == stage && !fired_for_hook.swap(true, Ordering::SeqCst) {
                cancellation_for_hook.store(true, Ordering::SeqCst);
            }
        });
        let cancelled =
            code::index_repository_with_stage_hook(&database, &repo, &cancellation, hook).unwrap();
        assert!(
            fired.load(Ordering::SeqCst),
            "stage hook did not fire: {stage:?}"
        );
        assert!(cancelled.cancelled && !cancelled.complete, "{stage:?}");
        assert!(!code::index_status(&database, &repo).unwrap().complete);
        assert!(
            run_index(&database, &repo).complete,
            "recovery after {stage:?}"
        );
    }
}

#[test]
fn memory_temporal_provenance_scope_and_forget_are_enforced() {
    let temp = TempDir::new().unwrap();
    let database = temp.path().join("memories.sqlite3");
    let scope = "user:alice";
    assert!(
        memory::remember(
            &database,
            "alice",
            "source",
            "bad namespace",
            None,
            None,
            None,
            None,
            None,
            None
        )
        .is_err()
    );
    let source = memory::remember(
        &database,
        scope,
        "source",
        "Raw source about a timeout policy.",
        Some("file:///policy.txt"),
        None,
        None,
        Some("2025-01-02T00:00:00Z"),
        Some("2025-01-01T00:00:00Z"),
        Some("2025-02-01T00:00:00Z"),
    )
    .unwrap();
    let fact = memory::remember(
        &database,
        scope,
        "derived",
        "Derived timeout is 40 milliseconds.",
        None,
        Some(source.id),
        None,
        Some("2025-01-03T00:00:00Z"),
        Some("2025-01-01T00:00:00Z"),
        Some("2025-02-01T00:00:00Z"),
    )
    .unwrap();
    assert_eq!(fact.source_id, Some(source.id));
    assert!(
        memory::remember(
            &database,
            "project:other",
            "derived",
            "cross-scope link",
            None,
            Some(source.id),
            None,
            None,
            None,
            None
        )
        .is_err()
    );
    let active = memory::recall(
        &database,
        scope,
        "timeout",
        Some("2025-01-15T00:00:00Z"),
        10,
    )
    .unwrap();
    assert_eq!(active.len(), 2);
    assert!(
        memory::recall(
            &database,
            scope,
            "timeout",
            Some("2025-03-01T00:00:00Z"),
            10
        )
        .unwrap()
        .is_empty()
    );
    let replacement = memory::remember(
        &database,
        scope,
        "derived",
        "Derived timeout is 55 milliseconds.",
        None,
        Some(source.id),
        Some(fact.id),
        Some("2025-02-01T00:00:00Z"),
        Some("2025-02-01T00:00:00Z"),
        None,
    )
    .unwrap();
    assert_eq!(replacement.supersedes_id, Some(fact.id));
    assert!(
        memory::recall(
            &database,
            scope,
            "timeout",
            Some("2025-03-01T00:00:00Z"),
            10
        )
        .unwrap()
        .iter()
        .any(|item| item.memory.id == replacement.id)
    );
    assert!(
        memory::recall(
            &database,
            "user:bob",
            "timeout",
            Some("2025-01-15T00:00:00Z"),
            10
        )
        .unwrap()
        .is_empty()
    );
    assert!(memory::forget(&database, scope, source.id).unwrap());
    assert!(
        memory::recall(&database, scope, "Raw source", None, 10)
            .unwrap()
            .is_empty()
    );
    assert!(!memory::forget(&database, "user:bob", fact.id).unwrap());
    assert!(
        memory::list_memories(&database, scope, 10)
            .unwrap()
            .iter()
            .any(|m| m.id == fact.id)
    );
    assert!(memory::forget(&database, scope, fact.id).unwrap());
    assert!(memory::forget(&database, scope, replacement.id).unwrap());
    assert!(
        memory::recall(&database, scope, "timeout", None, 10)
            .unwrap()
            .is_empty()
    );
    memory::compact(&database).unwrap();
    let conn = db::open_db(&database).unwrap();
    let compacted_content: String = conn
        .query_row(
            "SELECT content FROM memories WHERE id=?1",
            [replacement.id],
            |row| row.get(0),
        )
        .unwrap();
    assert!(compacted_content.is_empty());
}

#[test]
fn memory_recall_remains_scoped_during_concurrent_writes() {
    let temp = TempDir::new().unwrap();
    let database = temp.path().join("memory-concurrency.sqlite3");
    let scope = "project:concurrency";
    memory::remember(
        &database,
        scope,
        "source",
        "concurrent smoke marker seed",
        Some("benchmark://concurrency/seed"),
        None,
        None,
        None,
        None,
        None,
    )
    .unwrap();

    let barrier = Arc::new(Barrier::new(5));
    let readers = (0..4)
        .map(|reader_index| {
            let database = database.clone();
            let scope = scope.to_string();
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                barrier.wait();
                for _ in 0..20 {
                    let results =
                        memory::recall(&database, &scope, "concurrent smoke marker", None, 10)
                            .unwrap();
                    assert!(!results.is_empty(), "reader {reader_index} lost the seed");
                    assert!(results.iter().all(|result| result.memory.scope == scope));
                }
            })
        })
        .collect::<Vec<_>>();
    let writer_database = database.clone();
    let writer_scope = scope.to_string();
    let writer_barrier = Arc::clone(&barrier);
    let writer = std::thread::spawn(move || {
        writer_barrier.wait();
        for sample in 0..20 {
            memory::remember(
                &writer_database,
                &writer_scope,
                "source",
                &format!("concurrent smoke marker write {sample}"),
                Some(&format!("benchmark://concurrency/{sample}")),
                None,
                None,
                None,
                None,
                None,
            )
            .unwrap();
        }
    });

    for reader in readers {
        reader.join().unwrap();
    }
    writer.join().unwrap();
    assert!(
        !memory::recall(&database, scope, "concurrent smoke marker", None, 10)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn memory_recall_prefers_exact_and_covered_evidence_and_handles_unicode_and_abstention() {
    let temp = TempDir::new().unwrap();
    let database = temp.path().join("memory-ranking.sqlite3");
    let scope = "user:rank";
    let exact = memory::remember(
        &database,
        scope,
        "source",
        "Maya design review schedule 10:30",
        Some("benchmark://exact"),
        None,
        None,
        None,
        None,
        None,
    )
    .unwrap();
    memory::remember(
        &database,
        scope,
        "source",
        "Maya weekly planning review note records recurring calendar context.",
        Some("benchmark://shared-terms"),
        None,
        None,
        None,
        None,
        None,
    )
    .unwrap();
    memory::remember(
        &database,
        scope,
        "source",
        "Maya disfruta del café y trabaja los miércoles.",
        Some("benchmark://accented"),
        None,
        None,
        None,
        None,
        None,
    )
    .unwrap();

    let exact_results = memory::recall(
        &database,
        scope,
        "Maya design review schedule 10:30",
        None,
        10,
    )
    .unwrap();
    assert_eq!(exact_results[0].memory.id, exact.id);
    assert_eq!(exact_results[0].match_reason, "memory_exact_phrase");
    assert!(
        memory::recall(&database, scope, "Maya home mailing address Oslo", None, 10)
            .unwrap()
            .is_empty()
    );
    assert!(
        memory::recall(&database, scope, "Maya trabaja los miercoles", None, 10)
            .unwrap()
            .iter()
            .any(|result| result.memory.content.contains("miércoles"))
    );
    assert!(
        memory::recall(&database, scope, "!!!", None, 10)
            .unwrap()
            .is_empty()
    );
    assert!(memory::recall(&database, scope, "Maya\" OR content:secret*", None, 10).is_ok());
    assert!(memory::recall(&database, scope, "!!!", Some("yesterday"), 10).is_err());
    let too_many_terms = (0..65)
        .map(|index| format!("word{index}"))
        .collect::<Vec<_>>()
        .join(" ");
    assert!(memory::recall(&database, scope, &too_many_terms, None, 10).is_err());
}

#[test]
fn balanced_memory_recall_deduplicates_overlapping_candidate_windows() {
    let temp = TempDir::new().unwrap();
    let database = temp.path().join("memory-balanced-recall.sqlite3");
    let scope = "user:balanced";
    let mut session = memory::MemorySession::open(&database).unwrap();
    let inputs = (0..30)
        .map(|index| memory::MemoryInput {
            scope: scope.into(),
            kind: "source".into(),
            content: format!("status fact {index}"),
            source_uri: Some(format!("benchmark://status/{index}")),
            source_id: None,
            supersedes_id: None,
            observed_at: None,
            valid_from: None,
            valid_to: None,
        })
        .collect::<Vec<_>>();
    session.remember_batch(&inputs).unwrap();

    let (first, profile) = session
        .recall_profiled_ordered(
            scope,
            "status",
            None,
            10,
            memory::RecallCandidateOrder::BalancedRowIdWindows,
        )
        .unwrap();
    let (second, _) = session
        .recall_profiled_ordered(
            scope,
            "status",
            None,
            10,
            memory::RecallCandidateOrder::BalancedRowIdWindows,
        )
        .unwrap();

    assert_eq!(profile.candidates_returned, 50);
    assert_eq!(profile.candidate_rows_deduplicated, 20);
    assert_eq!(profile.candidates_hydrated, 30);
    assert!(profile.candidate_budget_saturated);
    assert_eq!(first.len(), 10);
    assert_eq!(
        first[0].candidate_coverage,
        memory::CandidateCoverage::RowIdSample
    );
    assert_eq!(
        first
            .iter()
            .map(|result| result.memory.id)
            .collect::<Vec<_>>(),
        second
            .iter()
            .map(|result| result.memory.id)
            .collect::<Vec<_>>()
    );
}

#[test]
fn default_memory_recall_finds_exact_phrase_at_every_matching_position() {
    const FANOUT: usize = 1_000;
    let temp = TempDir::new().unwrap();
    let database = temp.path().join("memory-position-recall.sqlite3");
    let scope = "project:position-recall";
    let positions = [
        0,
        FANOUT / 10,
        FANOUT / 4,
        FANOUT / 2,
        FANOUT * 3 / 4,
        FANOUT * 9 / 10,
        FANOUT - 1,
    ];
    let mut session = memory::MemorySession::open(&database).unwrap();
    let inputs = positions
        .iter()
        .enumerate()
        .flat_map(|(case, target_position)| {
            (0..FANOUT).map(move |index| {
                let term = format!("helioscase{case}");
                let content = if index == *target_position {
                    format!("where {term}")
                } else {
                    format!("{term} low value background entry {index:04}")
                };
                memory::MemoryInput {
                    scope: scope.into(),
                    kind: "source".into(),
                    content,
                    source_uri: Some(format!("benchmark://position/{case}/{index}")),
                    source_id: None,
                    supersedes_id: None,
                    observed_at: None,
                    valid_from: None,
                    valid_to: None,
                }
            })
        })
        .collect::<Vec<_>>();
    for batch in inputs.chunks(memory::MAX_MEMORY_BATCH_RECORDS) {
        session.remember_batch(batch).unwrap();
    }

    for (case, target_position) in positions.into_iter().enumerate() {
        let query = format!("where helioscase{case}");
        let (balanced, balanced_profile) = session
            .recall_profiled_ordered(
                scope,
                &query,
                None,
                10,
                memory::RecallCandidateOrder::BalancedRowIdWindows,
            )
            .unwrap();
        let balanced_finds_target = balanced.iter().any(|result| result.memory.content == query);
        assert_eq!(
            balanced_finds_target,
            !(25..FANOUT - 25).contains(&target_position),
            "balanced-window coverage changed at matching position {target_position}/{FANOUT}"
        );
        assert!(balanced_profile.candidate_budget_saturated);
        assert_eq!(
            balanced[0].candidate_coverage,
            memory::CandidateCoverage::RowIdSample
        );

        let (ranked, _) = session
            .recall_profiled_ordered(
                scope,
                &query,
                None,
                10,
                memory::RecallCandidateOrder::Ranked,
            )
            .unwrap();
        assert!(ranked.iter().any(|result| result.memory.content == query));

        let results = session.recall(scope, &query, None, 10).unwrap();
        let exact_phrase = results
            .iter()
            .find(|result| result.memory.content == query)
            .unwrap_or_else(|| {
                panic!(
                    "exact-phrase evidence at matching position {target_position}/{FANOUT} was omitted"
                )
            });
        assert_eq!(
            exact_phrase.candidate_coverage,
            memory::CandidateCoverage::ExactTopK,
            "exact-phrase evidence at matching position {target_position}/{FANOUT} was omitted"
        );
        // Both put the exact phrase first; the filler that follows differs only because equal
        // BM25 ties now resolve to the most recently remembered rows instead of index order.
        assert_eq!(results[0].memory.content, query);
        assert_eq!(ranked[0].memory.content, query);
        assert_eq!(results.len(), ranked.len());
        assert!(results[1..].iter().all(|result| {
            result
                .memory
                .content
                .starts_with(&format!("helioscase{case} low value"))
        }));
    }
}

#[test]
fn memory_batch_inserts_all_records_and_indexes_them_together() {
    let temp = TempDir::new().unwrap();
    let database = temp.path().join("memory-batch-success.sqlite3");
    let source = memory::remember(
        &database,
        "project:batch",
        "source",
        "The service API timeout policy is documented.",
        Some("batch://source"),
        None,
        None,
        None,
        None,
        None,
    )
    .unwrap();
    let inputs = vec![
        memory::MemoryInput {
            scope: "project:batch".into(),
            kind: "derived".into(),
            content: "The service API timeout is 250 milliseconds.".into(),
            source_uri: None,
            source_id: Some(source.id),
            supersedes_id: None,
            observed_at: None,
            valid_from: Some("2025-01-01T00:00:00Z".into()),
            valid_to: None,
        },
        memory::MemoryInput {
            scope: "project:batch".into(),
            kind: "source".into(),
            content: "The service API retries twice after timeout.".into(),
            source_uri: Some("batch://retry".into()),
            source_id: None,
            supersedes_id: None,
            observed_at: None,
            valid_from: None,
            valid_to: None,
        },
    ];
    let inserted = memory::remember_batch(&database, &inputs).unwrap();
    assert_eq!(inserted.len(), 2);
    assert_eq!(inserted[0].source_id, Some(source.id));
    let rows = memory::recall(&database, "project:batch", "service API timeout", None, 10).unwrap();
    assert!(rows.iter().any(|row| row.memory.id == inserted[0].id));
    assert!(rows.iter().any(|row| row.memory.id == inserted[1].id));
}

#[test]
fn operation_ledger_migration_failure_rolls_back_and_retries() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("ledger.sqlite3");
    let conn = crate::db::open_db(&path).unwrap();
    conn.execute_batch(
        "DROP TABLE memory_operations; PRAGMA user_version=4;
        UPDATE metadata SET value='4' WHERE key='schema_version';
        CREATE VIEW memory_operations AS SELECT 1;",
    )
    .unwrap();
    drop(conn);
    assert!(crate::db::open_db(&path).is_err());
    let conn = rusqlite::Connection::open(&path).unwrap();
    assert_eq!(
        conn.pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
            .unwrap(),
        4
    );
    conn.execute_batch("DROP VIEW memory_operations").unwrap();
    drop(conn);
    crate::db::open_db(&path).unwrap();
    crate::memory::verify_integrity(&path).unwrap();
}

#[test]
fn maintenance_and_reopening_connections_preserve_concurrent_keyed_writes() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("concurrent.sqlite3");
    crate::db::open_db(&path).unwrap();
    let threads: Vec<_> = (0..8).map(|writer| {
        let path = path.clone();
        std::thread::spawn(move || {
            for i in 0..40 {
                let input: crate::memory::MemoryInput = serde_json::from_value(serde_json::json!({"scope":"project:stress","kind":"source","content":format!("concurrent observation {writer} {i}")})).unwrap();
                let key = format!("{writer}-{i}");
                let first = crate::memory::remember_idempotent(&path, &key, &input).unwrap();
                assert_eq!(first.id, crate::memory::remember_idempotent(&path, &key, &input).unwrap().id);
                if i % 4 == 0 { crate::memory::forget(&path, &input.scope, first.id).unwrap(); crate::memory::maintain(&path, 8).unwrap(); }
            }
        })
    }).collect();
    for thread in threads {
        thread.join().unwrap();
    }
    crate::memory::verify_integrity(&path).unwrap();
}

#[test]
#[ignore = "explicit journal setup contention diagnostic"]
fn journal_mode_contention_diagnostic() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("journal.sqlite3");
    crate::db::open_db(&path).unwrap();
    for setter in [true, false] {
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
        let threads: Vec<_> = (0..8)
            .map(|_| {
                let path = path.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    let mut errors = Vec::new();
                    for _ in 0..500 {
                        let started = std::time::Instant::now();
                        let conn = rusqlite::Connection::open(&path).unwrap();
                        conn.busy_timeout(std::time::Duration::from_secs(5))
                            .unwrap();
                        let result = if setter {
                            conn.pragma_update(None, "journal_mode", "WAL")
                        } else {
                            conn.pragma_query_value(None, "journal_mode", |r| r.get::<_, String>(0))
                                .map(|_| ())
                        };
                        match result {
                            Ok(()) => {}
                            Err(rusqlite::Error::SqliteFailure(e, _)) => {
                                errors.push((e.extended_code, started.elapsed().as_micros()))
                            }
                            Err(error) => panic!("unexpected journal diagnostic failure: {error}"),
                        }
                    }
                    errors
                })
            })
            .collect();
        let errors: Vec<_> = threads
            .into_iter()
            .flat_map(|t| t.join().unwrap())
            .collect();
        println!("journal_setter={setter} calls=4000 errors={errors:?}");
    }
}

#[test]
fn rare_token_file_outranks_saturated_common_word_matches_regardless_of_path() {
    let temp = TempDir::new().unwrap();
    let database = temp.path().join("rank.sqlite3");
    let repo = new_repo(&temp, "rank-repo");
    // Each common word appears in a minority of files, so their BM25 is far above the point
    // where the bounded lexical score saturates; the path must not decide the order.
    for i in 0..120 {
        let mut body = String::from("pub fn filler() {}\n");
        if i % 5 == 0 {
            body.push_str("// unique unique unique\n");
        }
        if i % 4 == 0 {
            body.push_str("// marker marker\n");
        }
        if i % 6 == 0 {
            body.push_str("// alpha alpha alpha alpha\n");
        }
        fs::write(repo.join("src").join(format!("f{i:03}.rs")), body).unwrap();
    }
    fs::write(
        repo.join("src/z_target.rs"),
        "// zxqv-unique-marker-alpha\npub fn t() {}\n",
    )
    .unwrap();
    run_index(&database, &repo);
    for limit in [1, 5, 10] {
        let results =
            code::search_code(&database, &repo, "zxqv-unique-marker-alpha", limit).unwrap();
        assert_eq!(results[0].path, "src/z_target.rs", "limit {limit}");
    }
}

#[test]
fn search_snippets_default_to_three_lines_and_honor_context_lines() {
    let temp = TempDir::new().unwrap();
    let database = temp.path().join("context.sqlite3");
    let repo = new_repo(&temp, "context-repo");
    let body: String = (1..=20)
        .map(|n| {
            if n == 5 {
                "// context_anchor_token\n".to_string()
            } else {
                format!("// filler line {n}\n")
            }
        })
        .collect();
    fs::write(repo.join("src/context.rs"), body).unwrap();
    run_index(&database, &repo);

    let default = code::search_code(&database, &repo, "context_anchor_token", 5).unwrap();
    assert_eq!(default[0].line_start, Some(5));
    assert_eq!(default[0].line_end, Some(7));
    assert_eq!(default[0].snippet.as_ref().unwrap().lines().count(), 3);

    for (asked, shown) in [(1, 1), (8, 8), (50, 8), (0, 1)] {
        let response = code::search_code_with_context(
            &database,
            &repo,
            "context_anchor_token",
            5,
            2048,
            asked,
        )
        .unwrap();
        let result = &response.results[0];
        assert_eq!(
            result.snippet.as_ref().unwrap().lines().count(),
            shown,
            "context_lines {asked}"
        );
        assert_eq!(result.line_end, Some(5 + shown as u32 - 1));
    }
}

#[test]
fn long_snippet_lines_are_cut_around_the_match_and_open_range_stays_exact() {
    let temp = TempDir::new().unwrap();
    let database = temp.path().join("longline.sqlite3");
    let repo = new_repo(&temp, "longline-repo");
    let long = format!("{} needle_far_right {}", "x ".repeat(800), "y ".repeat(800));
    fs::write(
        repo.join("src/long.md"),
        format!("{long}\nshort second line\n"),
    )
    .unwrap();
    run_index(&database, &repo);

    let response =
        code::search_code_with_context(&database, &repo, "needle_far_right", 5, 2048, 3).unwrap();
    let result = &response.results[0];
    let snippet = result.snippet.as_ref().unwrap();
    assert!(
        snippet.contains("needle_far_right"),
        "match must stay visible"
    );
    assert!(snippet.contains('…'));
    assert!(result.snippet_truncated);
    assert!(response.truncated);
    assert!(snippet.lines().next().unwrap().chars().count() < 215);
    assert_eq!(snippet.lines().nth(1).unwrap(), "2: short second line");

    let full = code::open_range(&database, &repo, "src/long.md", 1, 1).unwrap();
    assert!(
        full.content.contains(&long),
        "open_range returns the whole line"
    );
}

#[test]
fn search_scope_is_stated_once_per_response_not_per_result() {
    let temp = TempDir::new().unwrap();
    let database = temp.path().join("scope.sqlite3");
    let repo = new_repo(&temp, "scope-repo");
    fs::write(repo.join("src/a.rs"), "pub fn scoped_marker_a() {}\n").unwrap();
    fs::write(repo.join("src/b.rs"), "pub fn scoped_marker_b() {}\n").unwrap();
    run_index(&database, &repo);
    let response =
        code::search_code_with_budget(&database, &repo, "scoped_marker_a scoped_marker_b", 5, 2048)
            .unwrap();
    assert!(response.results.len() >= 2);
    let status = code::index_status(&database, &repo).unwrap();
    assert_eq!(
        response.scope,
        format!("repo:{}:worktree:{}", status.repo_id, status.worktree_id)
    );
    let json = serde_json::to_value(&response).unwrap();
    assert!(json["scope"].is_string());
    for result in json["results"].as_array().unwrap() {
        assert!(result.get("scope").is_none());
        for kept in ["freshness", "index_generation", "index_complete"] {
            assert!(result.get(kept).is_some(), "{kept} stays per result");
        }
    }
}
