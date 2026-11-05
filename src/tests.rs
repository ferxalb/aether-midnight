use std::fs;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tempfile::TempDir;

use crate::{code, db, memory};

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
    assert_eq!(version, 2);
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
             PRAGMA user_version=1;",
        )
        .unwrap();
    drop(legacy);

    let migrated = db::open_db(&database).unwrap();
    let version: i64 = migrated
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    assert_eq!(version, 2);
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
    assert_eq!(lexical[0].match_reason, "lexical");
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
fn structural_parse_errors_keep_every_index_surface_marked_incomplete() {
    let temp = TempDir::new().unwrap();
    let database = temp.path().join("parse-error.sqlite3");
    let repo = new_repo(&temp, "malformed-repo");
    fs::write(
        repo.join("src/broken.rs"),
        "pub fn broken_symbol() { let = ; }\n",
    )
    .unwrap();

    let report = run_index(&database, &repo);
    assert!(!report.complete);
    assert!(report.catalog_complete);
    assert!(!report.structural_complete);
    assert!(!code::index_status(&database, &repo).unwrap().complete);
    assert!(!code::repo_map(&database, &repo).unwrap().complete);
    assert!(
        code::search_code(&database, &repo, "broken_symbol", 5)
            .unwrap()
            .iter()
            .all(|result| !result.index_complete)
    );
    assert!(
        code::list_symbols(&database, &repo, Some("broken_symbol"), 5)
            .unwrap()
            .iter()
            .all(|result| result["index_complete"] == false)
    );
    assert!(
        !code::open_range(&database, &repo, "src/broken.rs", 1, 1)
            .unwrap()
            .index_complete
    );
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
    assert!(!report.complete);
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
