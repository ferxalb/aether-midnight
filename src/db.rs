use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OpenFlags};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

use crate::INDEX_FORMAT_VERSION;

const SCHEMA_VERSION: i64 = 7;

const MEMORY_FTS_SCHEMA: &str = "CREATE VIRTUAL TABLE memory_fts USING fts5(
         scope_token,
         content,
         content='',
         contentless_delete=1,
         tokenize='unicode61 remove_diacritics 2'
     );
     CREATE TRIGGER memories_delete_fts AFTER DELETE ON memories BEGIN
         DELETE FROM memory_fts WHERE rowid = old.id;
     END;
     CREATE TRIGGER memories_soft_delete_fts AFTER UPDATE OF deleted_at ON memories
     WHEN old.deleted_at IS NULL AND new.deleted_at IS NOT NULL BEGIN
         DELETE FROM memory_fts WHERE rowid = new.id;
     END;";

/// Trigger-maintained active-memory counts per namespace. The empty-string row (never a valid
/// scope) holds the total across namespaces. They let the recall planner size a namespace in
/// O(log n) instead of counting rows on every query.
const MEMORY_SCOPE_COUNTS_SCHEMA: &str = "CREATE TABLE memory_scope_counts (
         scope TEXT PRIMARY KEY,
         active INTEGER NOT NULL CHECK(active >= 0)
     ) WITHOUT ROWID;
     INSERT INTO memory_scope_counts(scope,active)
         SELECT scope,count(*) FROM memories WHERE deleted_at IS NULL GROUP BY scope;
     INSERT INTO memory_scope_counts(scope,active)
         SELECT '',COALESCE(sum(active),0) FROM memory_scope_counts;
     CREATE TRIGGER memories_count_insert AFTER INSERT ON memories
     WHEN new.deleted_at IS NULL BEGIN
         INSERT INTO memory_scope_counts(scope,active) VALUES(new.scope,1)
             ON CONFLICT(scope) DO UPDATE SET active=active+1;
         UPDATE memory_scope_counts SET active=active+1 WHERE scope='';
     END;
     CREATE TRIGGER memories_count_soft_delete AFTER UPDATE OF deleted_at ON memories
     WHEN old.deleted_at IS NULL AND new.deleted_at IS NOT NULL BEGIN
         UPDATE memory_scope_counts SET active=active-1 WHERE scope=old.scope;
         UPDATE memory_scope_counts SET active=active-1 WHERE scope='';
     END;
     CREATE TRIGGER memories_count_delete AFTER DELETE ON memories
     WHEN old.deleted_at IS NULL BEGIN
         UPDATE memory_scope_counts SET active=active-1 WHERE scope=old.scope;
         UPDATE memory_scope_counts SET active=active-1 WHERE scope='';
     END;";

/// Persisted embedding vectors (schema 6). `scope` is the memory namespace or `repo_id:worktree_id`, so
/// every semantic query can filter by scope in SQL before any vector is read. Triggers remove vectors
/// whose source row is deleted, forgotten, or (for code) changes content hash.
const EMBEDDINGS_SCHEMA: &str = "CREATE TABLE IF NOT EXISTS embeddings (
         kind TEXT NOT NULL CHECK(kind IN ('memory','code')),
         scope TEXT NOT NULL,
         owner_id INTEGER NOT NULL,
         chunk INTEGER NOT NULL DEFAULT 0,
         line_start INTEGER,
         line_end INTEGER,
         model TEXT NOT NULL,
         revision TEXT NOT NULL,
         dims INTEGER NOT NULL CHECK(dims > 0),
         source_hash TEXT NOT NULL,
         vector BLOB NOT NULL,
         PRIMARY KEY(kind, owner_id, chunk)
     ) WITHOUT ROWID;
     CREATE INDEX IF NOT EXISTS embeddings_scope ON embeddings(kind, scope, model, revision);
     CREATE TRIGGER IF NOT EXISTS memories_embeddings_delete AFTER DELETE ON memories BEGIN
         DELETE FROM embeddings WHERE kind='memory' AND owner_id=old.id;
     END;
     CREATE TRIGGER IF NOT EXISTS memories_embeddings_soft_delete AFTER UPDATE OF deleted_at ON memories
     WHEN old.deleted_at IS NULL AND new.deleted_at IS NOT NULL BEGIN
         DELETE FROM embeddings WHERE kind='memory' AND owner_id=new.id;
     END;
     CREATE TRIGGER IF NOT EXISTS code_files_embeddings_delete AFTER DELETE ON code_files BEGIN
         DELETE FROM embeddings WHERE kind='code' AND owner_id=old.id;
     END;
     CREATE TRIGGER IF NOT EXISTS code_files_embeddings_rehash AFTER UPDATE OF content_hash ON code_files
     WHEN old.content_hash IS NOT new.content_hash BEGIN
         DELETE FROM embeddings WHERE kind='code' AND owner_id=new.id;
     END;
     UPDATE metadata SET value='6' WHERE key='schema_version';
     PRAGMA user_version=6;";

/// Schema 7: when a code file's content changes its vectors leave `embeddings` at once (searches
/// see the file as not embedded, exactly as before) but are parked in `embeddings_reuse`, so the
/// next `embed` can keep the vector of every chunk whose text did not change.
const EMBEDDINGS_REUSE_SCHEMA: &str = "DROP TRIGGER IF EXISTS code_files_embeddings_rehash;
     DROP TRIGGER IF EXISTS code_files_embeddings_delete;
     CREATE TABLE IF NOT EXISTS embeddings_reuse (
         owner_id INTEGER NOT NULL,
         chunk INTEGER NOT NULL,
         scope TEXT NOT NULL,
         model TEXT NOT NULL,
         revision TEXT NOT NULL,
         dims INTEGER NOT NULL CHECK(dims > 0),
         source_hash TEXT NOT NULL,
         vector BLOB NOT NULL,
         PRIMARY KEY(owner_id, chunk)
     ) WITHOUT ROWID;
     CREATE TRIGGER code_files_embeddings_delete AFTER DELETE ON code_files BEGIN
         DELETE FROM embeddings WHERE kind='code' AND owner_id=old.id;
         DELETE FROM embeddings_reuse WHERE owner_id=old.id;
     END;
     CREATE TRIGGER code_files_embeddings_rehash AFTER UPDATE OF content_hash ON code_files
     WHEN old.content_hash IS NOT new.content_hash BEGIN
         INSERT OR REPLACE INTO embeddings_reuse(owner_id,chunk,scope,model,revision,dims,source_hash,vector)
             SELECT owner_id,chunk,scope,model,revision,dims,source_hash,vector
             FROM embeddings WHERE kind='code' AND owner_id=new.id;
         DELETE FROM embeddings WHERE kind='code' AND owner_id=new.id;
     END;
     UPDATE metadata SET value='7' WHERE key='schema_version';
     PRAGMA user_version=7;";

#[derive(Clone, Debug, Serialize)]
pub struct RepoScope {
    pub root: PathBuf,
    pub repo_id: String,
    pub worktree_id: String,
    pub branch: Option<String>,
    pub revision: Option<String>,
}

pub fn default_db_path() -> Result<PathBuf> {
    let base = dirs::data_local_dir()
        .or_else(dirs::data_dir)
        .context("OS application-data directory is unavailable")?;
    Ok(base.join("aether-midnight").join("midnight.sqlite3"))
}

pub fn open_db(path: &Path) -> Result<Connection> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create database directory {}", parent.display()))?;
    }
    create_private_db_file(path)?;
    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_CREATE
            | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .with_context(|| format!("open database {}", path.display()))?;
    connection.busy_timeout(std::time::Duration::from_secs(5))?;
    connection.pragma_update(None, "foreign_keys", "ON")?;
    // Reading the persistent journal mode avoids requesting a mode transition on
    // every short-lived HTTP connection. Do not retry writes or change durability.
    let journal_started = std::time::Instant::now();
    let journal: String = connection
        .pragma_query_value(None, "journal_mode", |r| r.get(0))
        .with_context(|| {
            format!(
                "read journal mode after {} microseconds",
                journal_started.elapsed().as_micros()
            )
        })?;
    if !journal.eq_ignore_ascii_case("wal") {
        connection
            .pragma_update(None, "journal_mode", "WAL")
            .with_context(|| {
                format!(
                    "initialize WAL journal mode after {} microseconds",
                    journal_started.elapsed().as_micros()
                )
            })?;
    }
    connection.pragma_update(None, "synchronous", "NORMAL")?;
    #[cfg(feature = "retrieval-experiments")]
    if let Ok(raw_pages) = std::env::var("AETHER_WAL_AUTOCHECKPOINT_PAGES") {
        let pages = raw_pages.parse::<u32>().with_context(
            || "AETHER_WAL_AUTOCHECKPOINT_PAGES must be an integer between 1 and 1000000",
        )?;
        if !(1..=1_000_000).contains(&pages) {
            bail!("AETHER_WAL_AUTOCHECKPOINT_PAGES must be between 1 and 1000000");
        }
        connection.pragma_update(None, "wal_autocheckpoint", pages)?;
    }
    connection.pragma_update(None, "temp_store", "FILE")?;
    migrate(&connection)?;
    set_private_db_permissions(path)?;
    Ok(connection)
}

fn create_private_db_file(path: &Path) -> Result<()> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    match options.open(path) {
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(error) => {
            Err(error).with_context(|| format!("create database file {}", path.display()))
        }
    }
}

fn set_private_db_permissions(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .with_context(|| format!("restrict database permissions for {}", path.display()))?;
    }
    Ok(())
}

fn migrate(connection: &Connection) -> Result<()> {
    let version: i64 = connection.pragma_query_value(None, "user_version", |r| r.get(0))?;
    if version > SCHEMA_VERSION {
        bail!("database schema {version} is newer than this binary supports ({SCHEMA_VERSION})");
    }
    if version < SCHEMA_VERSION {
        connection.execute_batch("BEGIN IMMEDIATE;")?;
        let migration = (|| -> Result<()> {
            let locked_version: i64 =
                connection.pragma_query_value(None, "user_version", |r| r.get(0))?;
            if locked_version > SCHEMA_VERSION {
                bail!(
                    "database schema {locked_version} is newer than this binary supports ({SCHEMA_VERSION})"
                );
            }
            if locked_version == 0 {
                connection.execute_batch(
                    "CREATE TABLE metadata (
                         key TEXT PRIMARY KEY,
                         value TEXT NOT NULL
                     );
                     CREATE TABLE repositories (
                         repo_id TEXT NOT NULL,
                         worktree_id TEXT NOT NULL,
                         root TEXT NOT NULL,
                         branch TEXT,
                         revision TEXT,
                         generation INTEGER NOT NULL DEFAULT 0,
                         is_complete INTEGER NOT NULL DEFAULT 0 CHECK(is_complete IN (0,1)),
                         structural_complete INTEGER NOT NULL DEFAULT 0 CHECK(structural_complete IN (0,1)),
                         last_index_started TEXT,
                         last_index_finished TEXT,
                         eligible_files INTEGER NOT NULL DEFAULT 0,
                         eligible_bytes INTEGER NOT NULL DEFAULT 0,
                         skipped_json TEXT NOT NULL DEFAULT '{}',
                         PRIMARY KEY(repo_id, worktree_id)
                     );
                     CREATE TABLE code_files (
                         id INTEGER PRIMARY KEY,
                         repo_id TEXT NOT NULL,
                         worktree_id TEXT NOT NULL,
                         rel_path TEXT NOT NULL,
                         content_hash TEXT NOT NULL,
                         language TEXT NOT NULL,
                         size_bytes INTEGER NOT NULL,
                         modified_ns INTEGER NOT NULL,
                         indexed_generation INTEGER NOT NULL,
                         parse_error INTEGER NOT NULL DEFAULT 0 CHECK(parse_error IN (0,1)),
                         UNIQUE(repo_id, worktree_id, rel_path),
                         FOREIGN KEY(repo_id, worktree_id)
                           REFERENCES repositories(repo_id, worktree_id) ON DELETE CASCADE
                     );
                     CREATE INDEX code_files_scope_path
                         ON code_files(repo_id, worktree_id, rel_path);
                     CREATE INDEX code_files_generation
                         ON code_files(repo_id, worktree_id, indexed_generation);
                     CREATE VIRTUAL TABLE code_fts USING fts5(
                         content,
                         content='',
                         contentless_delete=1,
                         tokenize='unicode61 remove_diacritics 2'
                     );
                     CREATE TABLE code_symbols (
                         id INTEGER PRIMARY KEY,
                         file_id INTEGER NOT NULL REFERENCES code_files(id) ON DELETE CASCADE,
                         name TEXT NOT NULL,
                         kind TEXT NOT NULL,
                         line_start INTEGER NOT NULL,
                         line_end INTEGER NOT NULL,
                         byte_start INTEGER NOT NULL,
                         byte_end INTEGER NOT NULL
                     );
                     CREATE INDEX code_symbols_name ON code_symbols(name COLLATE NOCASE);
                     CREATE INDEX code_symbols_file ON code_symbols(file_id);
                     CREATE TABLE memories (
                         id INTEGER PRIMARY KEY,
                         scope TEXT NOT NULL,
                         kind TEXT NOT NULL CHECK(kind IN ('source','derived')),
                         content TEXT NOT NULL,
                         source_uri TEXT,
                         source_id INTEGER,
                         supersedes_id INTEGER,
                         created_at TEXT NOT NULL,
                         observed_at TEXT,
                         valid_from TEXT,
                         valid_to TEXT,
                         deleted_at TEXT,
                         FOREIGN KEY(source_id) REFERENCES memories(id),
                         FOREIGN KEY(supersedes_id) REFERENCES memories(id)
                     );
                     CREATE INDEX memories_scope_active
                         ON memories(scope, deleted_at, created_at);
                     CREATE INDEX memories_source ON memories(source_id);
                     INSERT INTO metadata(key,value) VALUES
                         ('index_format_version','2'), ('schema_version','3');
                     PRAGMA user_version=3;",
                )?;
                connection.execute_batch(MEMORY_FTS_SCHEMA)?;
            } else if locked_version == 1 {
                connection.execute_batch(
                    "DROP TABLE code_trigram;
                     CREATE VIRTUAL TABLE code_fts USING fts5(
                         content,
                         content='',
                         contentless_delete=1,
                         tokenize='unicode61 remove_diacritics 2'
                     );
                     DELETE FROM code_files;
                     UPDATE repositories SET is_complete=0,structural_complete=0,
                         eligible_files=0,eligible_bytes=0,skipped_json='{}';
                     UPDATE metadata SET value='2' WHERE key IN ('index_format_version','schema_version');
                     PRAGMA user_version=2;",
                )?;
            }
            let mut migrated_version: i64 =
                connection.pragma_query_value(None, "user_version", |r| r.get(0))?;
            if migrated_version == 2 {
                migrate_memory_fts_v2(connection)?;
                migrated_version = 3;
            }
            if migrated_version == 3 {
                migrate_scope_counts_v4(connection)?;
                migrated_version = 4;
            }
            if migrated_version == 4 {
                connection.execute_batch("CREATE TABLE memory_operations (
                    scope TEXT NOT NULL, operation_key TEXT NOT NULL, payload_hash BLOB NOT NULL,
                    memory_id INTEGER NOT NULL REFERENCES memories(id),
                    PRIMARY KEY(scope,operation_key)) WITHOUT ROWID;
                    UPDATE metadata SET value='5' WHERE key='schema_version'; PRAGMA user_version=5;")?;
                migrated_version = 5;
            }
            if migrated_version == 5 {
                connection.execute_batch(EMBEDDINGS_SCHEMA)?;
                migrated_version = 6;
            }
            if migrated_version == 6 {
                connection.execute_batch(EMBEDDINGS_REUSE_SCHEMA)?;
                migrated_version = 7;
            }
            if migrated_version != SCHEMA_VERSION {
                bail!("no migration path from database schema {migrated_version}");
            }
            Ok(())
        })();
        match migration {
            Ok(()) => connection.execute_batch("COMMIT;")?,
            Err(error) => {
                let _ = connection.execute_batch("ROLLBACK;");
                return Err(error);
            }
        }
    }
    let format: String = connection.query_row(
        "SELECT value FROM metadata WHERE key='index_format_version'",
        [],
        |row| row.get(0),
    )?;
    if format.parse::<u32>().ok() != Some(INDEX_FORMAT_VERSION) {
        bail!("unsupported index format version {format}");
    }
    Ok(())
}

fn migrate_memory_fts_v2(connection: &Connection) -> Result<()> {
    connection.execute_batch(
        "DROP TRIGGER IF EXISTS memories_delete_fts;
         DROP TRIGGER IF EXISTS memories_soft_delete_fts;
         DROP TABLE IF EXISTS memory_fts;",
    )?;
    connection.execute_batch(MEMORY_FTS_SCHEMA)?;

    let mut rows =
        connection.prepare("SELECT id,scope,content FROM memories WHERE deleted_at IS NULL")?;
    let mut insert = connection
        .prepare_cached("INSERT INTO memory_fts(rowid,scope_token,content) VALUES(?1,?2,?3)")?;
    let mut rows = rows.query([])?;
    while let Some(row) = rows.next()? {
        let id: i64 = row.get(0)?;
        let scope: String = row.get(1)?;
        let content: String = row.get(2)?;
        insert.execute(rusqlite::params![id, memory_scope_token(&scope), content])?;
    }
    connection.execute(
        "UPDATE metadata SET value='3' WHERE key='schema_version'",
        [],
    )?;
    connection.pragma_update(None, "user_version", 3)?;
    Ok(())
}

fn migrate_scope_counts_v4(connection: &Connection) -> Result<()> {
    connection.execute_batch(
        "DROP TRIGGER IF EXISTS memories_count_insert;
         DROP TRIGGER IF EXISTS memories_count_soft_delete;
         DROP TRIGGER IF EXISTS memories_count_delete;
         DROP TABLE IF EXISTS memory_scope_counts;",
    )?;
    connection.execute_batch(MEMORY_SCOPE_COUNTS_SCHEMA)?;
    connection.execute(
        "UPDATE metadata SET value='4' WHERE key='schema_version'",
        [],
    )?;
    connection.pragma_update(None, "user_version", 4)?;
    Ok(())
}

pub(crate) fn memory_scope_token(scope: &str) -> String {
    format!("s{}", hash_bytes(scope.as_bytes()))
}

pub fn repo_scope(path: &Path) -> Result<RepoScope> {
    let root = path
        .canonicalize()
        .with_context(|| format!("resolve repository {}", path.display()))?;
    if !root.is_dir() {
        bail!("repository path is not a directory: {}", root.display());
    }
    let facts = if has_git_context(&root) {
        git_facts_from_files(&root).or_else(|| git_facts_via_git(&root))
    } else {
        None
    };
    let git_dir = facts.as_ref().map(|facts| facts.git_dir.clone());
    let common = facts
        .as_ref()
        .map(|facts| facts.common.clone())
        .unwrap_or_else(|| root.clone());
    let branch = git_dir.as_ref().and_then(|git_dir| {
        std::fs::read_to_string(git_dir.join("HEAD"))
            .ok()
            .and_then(|head| {
                head.trim()
                    .strip_prefix("ref: refs/heads/")
                    .map(str::to_string)
            })
    });
    let revision = facts.and_then(|facts| facts.revision);
    let repo_id = hash_bytes(common.to_string_lossy().as_bytes());
    let worktree_id = hash_bytes(root.to_string_lossy().as_bytes());
    Ok(RepoScope {
        root,
        repo_id,
        worktree_id,
        branch,
        revision,
    })
}

/// What identifies a Git checkout: its git dir, the common dir shared by worktrees, and HEAD.
#[derive(Debug, PartialEq, Eq)]
struct GitFacts {
    git_dir: PathBuf,
    common: PathBuf,
    revision: Option<String>,
}

fn is_object_id(text: &str) -> bool {
    matches!(text.len(), 40 | 64) && text.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Reads the facts straight from `.git` (a directory, or a `gitdir:` file for worktrees and
/// submodules) so a tool call does not pay for spawning `git` (about 130 ms with the macOS shim).
/// Returns `None` for anything unusual (`GIT_DIR`, reftable, symbolic or missing refs), and the
/// caller then asks `git` itself.
fn git_facts_from_files(root: &Path) -> Option<GitFacts> {
    if std::env::var_os("GIT_DIR").is_some() || std::env::var_os("GIT_WORK_TREE").is_some() {
        return None;
    }
    let (base, meta) = root
        .ancestors()
        .find_map(|dir| dir.join(".git").symlink_metadata().ok().map(|m| (dir, m)))?;
    let dot_git = base.join(".git");
    let git_dir = if meta.is_dir() {
        dot_git
    } else if meta.is_file() {
        let text = std::fs::read_to_string(&dot_git).ok()?;
        let target = PathBuf::from(text.lines().next()?.strip_prefix("gitdir:")?.trim());
        if target.is_absolute() {
            target
        } else {
            base.join(target)
        }
    } else {
        return None;
    };
    let git_dir = git_dir.canonicalize().ok()?;
    let common = match std::fs::read_to_string(git_dir.join("commondir")) {
        Ok(text) => {
            let path = PathBuf::from(text.trim());
            let path = if path.is_absolute() {
                path
            } else {
                git_dir.join(path)
            };
            path.canonicalize().ok()?
        }
        Err(_) => git_dir.clone(),
    };
    if common.join("reftable").exists() {
        return None;
    }
    let head = std::fs::read_to_string(git_dir.join("HEAD")).ok()?;
    let head = head.trim();
    let revision = if let Some(reference) = head.strip_prefix("ref: ") {
        let reference = reference.trim();
        if reference.contains("..") {
            return None;
        }
        let loose = [git_dir.join(reference), common.join(reference)]
            .iter()
            .find_map(|path| std::fs::read_to_string(path).ok())
            .map(|text| text.trim().to_string());
        let sha = match loose {
            Some(text) => text,
            None => std::fs::read_to_string(common.join("packed-refs"))
                .ok()?
                .lines()
                .filter(|line| !line.starts_with('#') && !line.starts_with('^'))
                .find_map(|line| {
                    let (sha, name) = line.split_once(' ')?;
                    (name.trim() == reference).then(|| sha.to_string())
                })?,
        };
        if !is_object_id(&sha) {
            return None;
        }
        Some(sha)
    } else if is_object_id(head) {
        Some(head.to_string())
    } else {
        return None;
    };
    Some(GitFacts {
        git_dir,
        common,
        revision,
    })
}

/// The original path: ask `git` itself.
fn git_facts_via_git(root: &Path) -> Option<GitFacts> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args([
            "rev-parse",
            "--absolute-git-dir",
            "--git-common-dir",
            "HEAD",
        ])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&output.stdout);
    let lines: Vec<&str> = text.lines().collect();
    let git_dir = PathBuf::from(lines.first()?);
    let common = lines.get(1).map(|value| {
        let path = PathBuf::from(value);
        let path = if path.is_absolute() {
            path
        } else {
            root.join(path)
        };
        path.canonicalize().unwrap_or(path)
    })?;
    let revision = lines
        .get(2)
        .filter(|revision| revision.len() >= 40 && revision.bytes().all(|b| b.is_ascii_hexdigit()))
        .map(|revision| revision.to_string());
    Some(GitFacts {
        git_dir,
        common,
        revision,
    })
}

fn has_git_context(root: &Path) -> bool {
    std::env::var_os("GIT_DIR").is_some()
        || std::env::var_os("GIT_WORK_TREE").is_some()
        || root
            .ancestors()
            .any(|ancestor| ancestor.join(".git").symlink_metadata().is_ok())
}

pub fn hash_bytes(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(out, "{byte:02x}");
    }
    out
}

pub fn now_utc() -> String {
    format_utc(time::OffsetDateTime::now_utc())
}

pub fn normalize_timestamp(value: &str) -> Result<String> {
    let parsed = time::OffsetDateTime::parse(value, &time::format_description::well_known::Rfc3339)
        .context("timestamp must use RFC 3339 format")?;
    Ok(format_utc(parsed.to_offset(time::UtcOffset::UTC)))
}

fn format_utc(value: time::OffsetDateTime) -> String {
    let date = value.date();
    let clock = value.time();
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:09}Z",
        date.year(),
        u8::from(date.month()),
        date.day(),
        clock.hour(),
        clock.minute(),
        clock.second(),
        clock.nanosecond()
    )
}

#[cfg(test)]
mod git_facts_tests {
    use super::*;
    use std::process::Command;

    fn git(dir: &Path, args: &[&str]) -> bool {
        Command::new("git")
            .args(["-c", "user.name=t", "-c", "user.email=t@example.com", "-C"])
            .arg(dir)
            .args(args)
            .output()
            .is_ok_and(|out| out.status.success())
    }

    /// The file reader must agree with `git` itself on every layout it accepts.
    #[test]
    fn reading_git_files_matches_git_rev_parse() {
        let temp = tempfile::TempDir::new().unwrap();
        let repo = temp.path().join("repo").canonicalize().unwrap_or_else(|_| {
            std::fs::create_dir_all(temp.path().join("repo")).unwrap();
            temp.path().join("repo").canonicalize().unwrap()
        });
        if !git(&repo, &["init", "-q"]) {
            return; // no git available
        }
        std::fs::write(repo.join("a.txt"), "a").unwrap();
        assert!(git(&repo, &["add", "."]));
        assert!(git(&repo, &["commit", "-q", "-m", "one"]));
        std::fs::create_dir_all(repo.join("sub/dir")).unwrap();

        let check = |dir: &Path, label: &str| {
            let fast = git_facts_from_files(dir);
            let slow = git_facts_via_git(dir);
            assert!(fast.is_some(), "{label}: file reader declined");
            assert_eq!(fast, slow, "{label}");
        };
        check(&repo, "loose ref");
        check(&repo.join("sub/dir"), "subdirectory");
        assert!(git(&repo, &["pack-refs", "--all"]));
        check(&repo, "packed ref");
        assert!(git(&repo, &["checkout", "-q", "--detach"]));
        check(&repo, "detached HEAD");
        assert!(git(&repo, &["checkout", "-q", "-"]));

        let wt = temp.path().join("wt");
        if git(
            &repo,
            &["worktree", "add", "-q", wt.to_str().unwrap(), "-b", "other"],
        ) {
            let wt = wt.canonicalize().unwrap();
            check(&wt, "linked worktree");
            let fast = git_facts_from_files(&wt).unwrap();
            let main = git_facts_from_files(&repo).unwrap();
            assert_eq!(fast.common, main.common, "worktrees share a repo identity");
            assert_ne!(fast.git_dir, main.git_dir);
        }
        // Not a repository at all: the reader declines.
        assert!(
            git_facts_from_files(temp.path()).is_none()
                || temp.path().ancestors().any(|a| a.join(".git").exists())
        );
    }
}
