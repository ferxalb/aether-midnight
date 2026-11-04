use anyhow::{Context, Result, bail};
use fs2::FileExt;
use ignore::WalkBuilder;
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use crate::db::{RepoScope, hash_bytes, now_utc, open_db, repo_scope};
use crate::text_index::{SqliteFtsIndex, TextIndex};
use crate::{INDEX_FORMAT_VERSION, MAX_FILE_BYTES};

const BATCH_FILES: usize = 512;
const BATCH_BYTES: usize = 16 * 1024 * 1024;
const MAX_OPEN_RANGE_LINES: u32 = 200;
const MAX_SNIPPET_LINES: usize = 8;
const MAX_SNIPPET_BYTES: usize = 4096;
const DEFAULT_TOKEN_BUDGET: usize = 2048;
const MAX_TOKEN_BUDGET: usize = 8192;
const MAX_SEARCH_QUERY_BYTES: usize = 4096;
const MAX_RELATIVE_PATH_BYTES: usize = 4096;
const MAX_AETHERIGNORE_BYTES: u64 = MAX_FILE_BYTES;
const PARSER_TIME_BUDGET: Duration = Duration::from_millis(250);

#[derive(Debug, Clone, Serialize)]
pub struct Symbol {
    pub name: String,
    pub kind: String,
    pub line_start: u32,
    pub line_end: u32,
    pub byte_start: usize,
    pub byte_end: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct IndexReport {
    pub repo_id: String,
    pub worktree_id: String,
    pub root: String,
    pub generation: u64,
    pub complete: bool,
    pub catalog_complete: bool,
    pub structural_complete: bool,
    pub cancelled: bool,
    pub eligible_files: u64,
    pub eligible_bytes: u64,
    pub indexed_or_updated: u64,
    pub unchanged: u64,
    pub skipped: BTreeMap<String, u64>,
    pub timings_ms: BTreeMap<String, f64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct IndexStatus {
    pub repo_id: String,
    pub worktree_id: String,
    pub root: String,
    pub branch: Option<String>,
    pub revision: Option<String>,
    pub generation: u64,
    pub complete: bool,
    pub catalog_complete: bool,
    pub structural_complete: bool,
    pub eligible_files: u64,
    pub eligible_bytes: u64,
    pub indexed_files: u64,
    pub indexed_bytes: u64,
    pub index_format_version: u32,
    pub last_index_started: Option<String>,
    pub last_index_finished: Option<String>,
    pub skipped: BTreeMap<String, u64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CodeResult {
    pub path: String,
    pub line_start: Option<u32>,
    pub line_end: Option<u32>,
    pub match_reason: String,
    pub scope: String,
    pub index_generation: u64,
    pub index_complete: bool,
    pub freshness: String,
    pub snippet: Option<String>,
    pub snippet_truncated: bool,
    pub estimated_tokens: usize,
    pub score: f64,
    pub score_breakdown: BTreeMap<String, f64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CodeSearchResponse {
    pub results: Vec<CodeResult>,
    pub token_budget: usize,
    pub estimated_tokens: usize,
    pub truncated: bool,
    pub token_estimator: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct OpenRangeResult {
    pub path: String,
    pub line_start: u32,
    pub line_end: u32,
    pub content_hash: String,
    pub freshness: String,
    pub index_generation: u64,
    pub index_complete: bool,
    pub content: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct RepoMap {
    pub repo_id: String,
    pub worktree_id: String,
    pub generation: u64,
    pub complete: bool,
    pub files_by_language: BTreeMap<String, u64>,
    pub symbols_by_kind: BTreeMap<String, u64>,
    pub top_directories: BTreeMap<String, u64>,
}

#[derive(Clone)]
struct PendingFile {
    rel_path: String,
    hash: String,
    language: String,
    size: u64,
    modified_ns: i64,
    text: String,
    symbols: Vec<Symbol>,
    parse_error: bool,
    is_new: bool,
}

#[derive(Clone)]
struct SeenFile {
    rel_path: String,
    size: u64,
    modified_ns: i64,
    parse_error: bool,
}

#[derive(Default)]
struct Timers {
    discovery: f64,
    hashing: f64,
    parsing: f64,
    index_writing: f64,
    commit: f64,
}

pub fn index_repository(
    db_path: &Path,
    root: &Path,
    include_generated: bool,
    verify_hashes: bool,
    cancellation: &AtomicBool,
) -> Result<IndexReport> {
    let scope = repo_scope(root)?;
    index_scoped(
        db_path,
        &scope,
        include_generated,
        verify_hashes,
        cancellation,
    )
}

fn index_scoped(
    db_path: &Path,
    scope: &RepoScope,
    include_generated: bool,
    verify_hashes: bool,
    cancellation: &AtomicBool,
) -> Result<IndexReport> {
    let started = Instant::now();
    let _index_lock = acquire_index_lock(db_path, scope)?;
    let mut conn = open_db(db_path)?;
    let previous_generation: i64 = conn
        .query_row(
            "SELECT generation FROM repositories WHERE repo_id=?1 AND worktree_id=?2",
            params![scope.repo_id, scope.worktree_id],
            |row| row.get(0),
        )
        .optional()?
        .unwrap_or(0);
    let generation = (previous_generation as u64).saturating_add(1);
    let started_at = now_utc();
    conn.execute(
        "INSERT INTO repositories(repo_id,worktree_id,root,branch,revision,generation,is_complete,
          last_index_started,last_index_finished,eligible_files,eligible_bytes,skipped_json)
         VALUES(?1,?2,?3,?4,?5,?6,0,?7,NULL,0,0,'{}')
         ON CONFLICT(repo_id,worktree_id) DO UPDATE SET root=excluded.root,branch=excluded.branch,
          revision=excluded.revision,generation=excluded.generation,is_complete=0,
          structural_complete=0,last_index_started=excluded.last_index_started,
          eligible_files=0,eligible_bytes=0,skipped_json='{}'",
        params![
            scope.repo_id,
            scope.worktree_id,
            scope.root.to_string_lossy(),
            scope.branch,
            scope.revision,
            generation as i64,
            started_at
        ],
    )?;

    let root = &scope.root;
    let mut skipped = BTreeMap::new();
    let excluded_generated = std::sync::Arc::new(AtomicU64::new(0));
    let excluded_depth = std::sync::Arc::new(AtomicU64::new(0));
    let mut builder = WalkBuilder::new(root);
    builder
        .hidden(false)
        .git_ignore(true)
        .git_exclude(true)
        .require_git(false)
        .parents(false)
        .current_dir(root)
        .follow_links(false)
        .max_depth(Some(128));
    let aetherignore = root.join(".aetherignore");
    if fs::symlink_metadata(&aetherignore).is_ok_and(|meta| {
        meta.is_file() && !meta.file_type().is_symlink() && meta.len() > MAX_AETHERIGNORE_BYTES
    }) {
        bump(&mut skipped, "aetherignore_oversized");
    }
    let ignore_error = if valid_aetherignore(&aetherignore, root) {
        builder
            .add_ignore(&aetherignore)
            .map(|error| error.to_string())
    } else {
        None
    };
    let excluded_generated_for_walk = excluded_generated.clone();
    let excluded_vcs_for_walk = std::sync::Arc::new(AtomicU64::new(0));
    let excluded_vcs_count = excluded_vcs_for_walk.clone();
    let excluded_depth_for_walk = excluded_depth.clone();
    let root_for_filter = root.to_path_buf();
    builder.filter_entry(move |entry| {
        if entry.depth() > 128 {
            excluded_depth_for_walk.fetch_add(1, Ordering::Relaxed);
            return false;
        }
        if entry.depth() == 0 {
            return true;
        }
        let Some(relative) = entry.path().strip_prefix(&root_for_filter).ok() else {
            return false;
        };
        if is_vcs_metadata_path(relative) {
            excluded_vcs_count.fetch_add(1, Ordering::Relaxed);
            return false;
        }
        if include_generated {
            return true;
        }
        if default_generated_path(relative) {
            excluded_generated_for_walk.fetch_add(1, Ordering::Relaxed);
            false
        } else {
            true
        }
    });
    let mut timers = Timers::default();
    let mut eligible_files = 0u64;
    let mut eligible_bytes = 0u64;
    let mut changed = 0u64;
    let mut unchanged = 0u64;
    let mut pending = Vec::<PendingFile>::with_capacity(BATCH_FILES);
    let mut pending_bytes = 0usize;
    let mut pending_seen = Vec::<SeenFile>::with_capacity(256);
    let mut parser_cache = ParserCache::default();
    let mut structural_complete = true;
    let mut walker = builder.build();
    let mut last_partial_publish = Instant::now();

    loop {
        let discovery_start = Instant::now();
        let entry = walker.next();
        timers.discovery += discovery_start.elapsed().as_secs_f64() * 1000.0;
        let Some(entry) = entry else { break };
        if cancellation.load(Ordering::Relaxed) {
            if !pending.is_empty() || !pending_seen.is_empty() {
                flush_batch(
                    &mut conn,
                    &scope.repo_id,
                    &scope.worktree_id,
                    generation,
                    &mut pending,
                    &mut pending_seen,
                    &mut timers,
                )?;
            }
            let excluded_count = excluded_generated.load(Ordering::Relaxed);
            if excluded_count > 0 {
                *skipped
                    .entry("generated_or_vendor_default".to_string())
                    .or_default() += excluded_count;
            }
            let depth_count = excluded_depth.load(Ordering::Relaxed);
            if depth_count > 0 {
                *skipped.entry("depth_limit_128".to_string()).or_default() += depth_count;
            }
            if ignore_error.is_some() {
                bump(&mut skipped, "aetherignore_parse_error");
            }
            let skipped_json = serde_json::to_string(&skipped)?;
            conn.execute(
                "UPDATE repositories SET eligible_files=?3,eligible_bytes=?4,skipped_json=?5,is_complete=0,structural_complete=0
                 WHERE repo_id=?1 AND worktree_id=?2",
                params![scope.repo_id, scope.worktree_id, eligible_files as i64, eligible_bytes as i64, skipped_json],
            )?;
            return Ok(report(
                scope,
                generation,
                false,
                false,
                true,
                false,
                eligible_files,
                eligible_bytes,
                changed,
                unchanged,
                skipped,
                timers,
                started.elapsed().as_secs_f64() * 1000.0,
            ));
        }
        let entry = match entry {
            Ok(e) => e,
            Err(_) => {
                bump(&mut skipped, "walk_error");
                continue;
            }
        };
        let path = entry.path();
        if path == root || entry.file_type().is_some_and(|t| t.is_dir()) {
            continue;
        }
        if entry.file_type().is_some_and(|t| t.is_symlink()) {
            bump(&mut skipped, "symlink");
            continue;
        }
        let rel = match path.strip_prefix(root) {
            Ok(p) => p,
            Err(_) => {
                bump(&mut skipped, "outside_root");
                continue;
            }
        };
        if is_secret_path(rel) {
            bump(&mut skipped, "credential_or_key");
            continue;
        }
        if !include_generated && default_generated_path(rel) {
            bump(&mut skipped, "generated_or_vendor_default");
            continue;
        }
        let file_type = match entry.file_type() {
            Some(t) if t.is_file() => t,
            _ => continue,
        };
        let _ = file_type;
        let metadata = match fs::symlink_metadata(path) {
            Ok(m) if m.is_file() && !m.file_type().is_symlink() => m,
            _ => {
                bump(&mut skipped, "symlink_or_stat_error");
                continue;
            }
        };
        let size = metadata.len();
        if size > MAX_FILE_BYTES {
            bump(&mut skipped, "oversized");
            continue;
        }
        let relative = rel.to_string_lossy().replace('\\', "/");
        let modified_ns = modified_ns(&metadata);
        let existing: Option<(String, i64, i64, bool)> = conn
            .query_row(
                "SELECT content_hash,size_bytes,modified_ns,parse_error FROM code_files
                 WHERE repo_id=?1 AND worktree_id=?2 AND rel_path=?3",
                params![scope.repo_id, scope.worktree_id, relative],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()?;
        if !verify_hashes
            && existing
                .as_ref()
                .is_some_and(|(_, prev_size, prev_mtime, _)| {
                    *prev_size == size as i64 && *prev_mtime == modified_ns
                })
        {
            let parse_error = existing
                .as_ref()
                .is_some_and(|(_, _, _, parse_error)| *parse_error);
            if parse_error {
                structural_complete = false;
                bump(&mut skipped, "tree_sitter_parse_error");
            }
            pending_seen.push(SeenFile {
                rel_path: relative.clone(),
                size,
                modified_ns,
                parse_error,
            });
            eligible_files += 1;
            eligible_bytes += size;
            unchanged += 1;
            if pending_seen.len() >= 256 {
                flush_batch(
                    &mut conn,
                    &scope.repo_id,
                    &scope.worktree_id,
                    generation,
                    &mut pending,
                    &mut pending_seen,
                    &mut timers,
                )?;
            }
            if last_partial_publish.elapsed().as_secs_f64() >= 0.15 {
                flush_batch(
                    &mut conn,
                    &scope.repo_id,
                    &scope.worktree_id,
                    generation,
                    &mut pending,
                    &mut pending_seen,
                    &mut timers,
                )?;
                publish_progress(&conn, scope, eligible_files, eligible_bytes, &skipped)?;
                last_partial_publish = Instant::now();
            }
            continue;
        }
        let _actual_path = match path.canonicalize() {
            Ok(p) if p.starts_with(root) => p,
            _ => {
                bump(&mut skipped, "outside_root_or_raced_symlink");
                continue;
            }
        };
        let read_start = Instant::now();
        let bytes = match read_limited_from_root(root, rel, MAX_FILE_BYTES as usize + 1) {
            Ok(bytes) => bytes,
            Err(_) => {
                bump(&mut skipped, "read_error_or_raced_symlink");
                continue;
            }
        };
        if bytes.len() > MAX_FILE_BYTES as usize {
            timers.hashing += read_start.elapsed().as_secs_f64() * 1000.0;
            bump(&mut skipped, "oversized_after_read");
            continue;
        }
        if bytes.contains(&0) {
            timers.hashing += read_start.elapsed().as_secs_f64() * 1000.0;
            bump(&mut skipped, "binary_nul");
            continue;
        }
        if has_binary_controls(&bytes) {
            timers.hashing += read_start.elapsed().as_secs_f64() * 1000.0;
            bump(&mut skipped, "binary_control_bytes");
            continue;
        }
        let text = match String::from_utf8(bytes) {
            Ok(t) => t,
            Err(_) => {
                timers.hashing += read_start.elapsed().as_secs_f64() * 1000.0;
                bump(&mut skipped, "non_utf8");
                continue;
            }
        };
        let hash = hash_bytes(text.as_bytes());
        timers.hashing += read_start.elapsed().as_secs_f64() * 1000.0;
        if !verify_hashes
            && existing
                .as_ref()
                .is_some_and(|(previous, _, _, _)| previous == &hash)
        {
            let parse_error = existing
                .as_ref()
                .is_some_and(|(_, _, _, parse_error)| *parse_error);
            if parse_error {
                structural_complete = false;
                bump(&mut skipped, "tree_sitter_parse_error");
            }
            pending_seen.push(SeenFile {
                rel_path: relative.clone(),
                size,
                modified_ns,
                parse_error,
            });
            eligible_files += 1;
            eligible_bytes += size;
            unchanged += 1;
            if pending_seen.len() >= 256 {
                flush_batch(
                    &mut conn,
                    &scope.repo_id,
                    &scope.worktree_id,
                    generation,
                    &mut pending,
                    &mut pending_seen,
                    &mut timers,
                )?;
            }
            continue;
        }
        let language = language_for(&relative);
        let parse_start = Instant::now();
        let extracted = parser_cache.extract(language.as_deref(), &text);
        timers.parsing += parse_start.elapsed().as_secs_f64() * 1000.0;
        let (symbols, parse_error) = extracted.unwrap_or_default();
        if parse_error {
            structural_complete = false;
            bump(&mut skipped, "tree_sitter_parse_error");
        }
        let text_size = text.len();
        pending.push(PendingFile {
            rel_path: relative,
            hash,
            language: language.unwrap_or_else(|| "text".to_string()),
            size: size as u64,
            modified_ns,
            text,
            symbols,
            parse_error,
            is_new: existing.is_none(),
        });
        pending_bytes += text_size;
        eligible_files += 1;
        eligible_bytes += size;
        changed += 1;
        if pending.len() >= BATCH_FILES || pending_bytes >= BATCH_BYTES || pending_seen.len() >= 256
        {
            flush_batch(
                &mut conn,
                &scope.repo_id,
                &scope.worktree_id,
                generation,
                &mut pending,
                &mut pending_seen,
                &mut timers,
            )?;
            pending_bytes = 0;
            last_partial_publish = Instant::now();
        }
    }
    let excluded_count = excluded_generated.load(Ordering::Relaxed);
    if excluded_count > 0 {
        *skipped
            .entry("generated_or_vendor_default".to_string())
            .or_default() += excluded_count;
    }
    let depth_count = excluded_depth.load(Ordering::Relaxed);
    if depth_count > 0 {
        *skipped.entry("depth_limit_128".to_string()).or_default() += depth_count;
    }
    let vcs_count = excluded_vcs_for_walk.load(Ordering::Relaxed);
    if vcs_count > 0 {
        *skipped
            .entry("version_control_metadata".to_string())
            .or_default() += vcs_count;
    }
    if ignore_error.is_some() {
        bump(&mut skipped, "aetherignore_parse_error");
    }
    if !pending.is_empty() || !pending_seen.is_empty() {
        flush_batch(
            &mut conn,
            &scope.repo_id,
            &scope.worktree_id,
            generation,
            &mut pending,
            &mut pending_seen,
            &mut timers,
        )?;
    }

    let commit_start = Instant::now();
    let stale_exists: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM code_files WHERE repo_id=?1 AND worktree_id=?2 AND indexed_generation<>?3 LIMIT 1)",
        params![scope.repo_id, scope.worktree_id, generation as i64],
        |row| row.get(0),
    )?;
    if stale_exists {
        let tx = conn.transaction()?;
        SqliteFtsIndex.remove_stale(&tx, &scope.repo_id, &scope.worktree_id, generation)?;
        tx.execute(
            "DELETE FROM code_files WHERE repo_id=?1 AND worktree_id=?2 AND indexed_generation<>?3",
            params![scope.repo_id, scope.worktree_id, generation as i64],
        )?;
        tx.commit()?;
    }
    let skipped_json = serde_json::to_string(&skipped)?;
    conn.execute(
        "UPDATE repositories SET eligible_files=?3,eligible_bytes=?4,skipped_json=?5,is_complete=1,
         structural_complete=?6,last_index_finished=?7
         WHERE repo_id=?1 AND worktree_id=?2",
        params![
            scope.repo_id,
            scope.worktree_id,
            eligible_files as i64,
            eligible_bytes as i64,
            skipped_json,
            structural_complete,
            now_utc()
        ],
    )?;
    timers.commit += commit_start.elapsed().as_secs_f64() * 1000.0;
    let total = started.elapsed().as_secs_f64() * 1000.0;
    Ok(report(
        scope,
        generation,
        structural_complete,
        true,
        false,
        structural_complete,
        eligible_files,
        eligible_bytes,
        changed,
        unchanged,
        skipped,
        timers,
        total,
    ))
}

fn acquire_index_lock(db_path: &Path, scope: &RepoScope) -> Result<File> {
    let parent = db_path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    fs::create_dir_all(parent)
        .with_context(|| format!("create database directory {}", parent.display()))?;
    let lock_path = parent.join(format!(
        ".midnight-index-{}-{}.lock",
        scope.repo_id, scope.worktree_id
    ));
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&lock_path)
        .with_context(|| format!("open index lock {}", lock_path.display()))?;
    file.try_lock_exclusive().with_context(|| {
        format!(
            "another index operation holds the lock for {}",
            scope.root.display()
        )
    })?;
    Ok(file)
}

#[allow(clippy::too_many_arguments)] // Keeps this internal report construction local and explicit.
fn report(
    scope: &RepoScope,
    generation: u64,
    complete: bool,
    catalog_complete: bool,
    cancelled: bool,
    structural_complete: bool,
    eligible_files: u64,
    eligible_bytes: u64,
    changed: u64,
    unchanged: u64,
    skipped: BTreeMap<String, u64>,
    timers: Timers,
    total_ms: f64,
) -> IndexReport {
    let mut timings_ms = BTreeMap::new();
    timings_ms.insert("discovery".into(), timers.discovery);
    timings_ms.insert("hashing_and_read".into(), timers.hashing);
    timings_ms.insert("parsing".into(), timers.parsing);
    timings_ms.insert("index_writing".into(), timers.index_writing);
    timings_ms.insert("commit_and_reconcile".into(), timers.commit);
    timings_ms.insert("full_index_ready".into(), total_ms);
    IndexReport {
        repo_id: scope.repo_id.clone(),
        worktree_id: scope.worktree_id.clone(),
        root: scope.root.to_string_lossy().into_owned(),
        generation,
        complete,
        catalog_complete,
        structural_complete,
        cancelled,
        eligible_files,
        eligible_bytes,
        indexed_or_updated: changed,
        unchanged,
        skipped,
        timings_ms,
    }
}

fn publish_progress(
    conn: &Connection,
    scope: &RepoScope,
    files: u64,
    bytes: u64,
    skipped: &BTreeMap<String, u64>,
) -> Result<()> {
    conn.execute(
        "UPDATE repositories SET eligible_files=?3,eligible_bytes=?4,skipped_json=?5
         WHERE repo_id=?1 AND worktree_id=?2",
        params![
            scope.repo_id,
            scope.worktree_id,
            files as i64,
            bytes as i64,
            serde_json::to_string(skipped)?
        ],
    )?;
    Ok(())
}

fn flush_batch(
    conn: &mut Connection,
    repo_id: &str,
    worktree_id: &str,
    generation: u64,
    pending: &mut Vec<PendingFile>,
    pending_seen: &mut Vec<SeenFile>,
    timers: &mut Timers,
) -> Result<()> {
    if pending.is_empty() && pending_seen.is_empty() {
        return Ok(());
    }
    let write_start = Instant::now();
    let tx = conn.transaction()?;
    {
        let mut stmt = tx.prepare(
            "UPDATE code_files SET size_bytes=?4,modified_ns=?5,indexed_generation=?6,parse_error=?7
             WHERE repo_id=?1 AND worktree_id=?2 AND rel_path=?3",
        )?;
        for file in pending_seen.drain(..) {
            stmt.execute(params![
                repo_id,
                worktree_id,
                file.rel_path,
                file.size as i64,
                file.modified_ns,
                generation as i64,
                file.parse_error,
            ])?;
        }
    }
    for file in pending.drain(..) {
        let file_id = tx
            .query_row(
                "INSERT INTO code_files(repo_id,worktree_id,rel_path,content_hash,language,size_bytes,modified_ns,indexed_generation,parse_error)
                 VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)
                 ON CONFLICT(repo_id,worktree_id,rel_path) DO UPDATE SET content_hash=excluded.content_hash,
                   language=excluded.language,size_bytes=excluded.size_bytes,modified_ns=excluded.modified_ns,
                   indexed_generation=excluded.indexed_generation,parse_error=excluded.parse_error
                 RETURNING id",
                params![repo_id, worktree_id, file.rel_path, file.hash, file.language, file.size as i64, file.modified_ns, generation as i64, file.parse_error],
                |row| row.get::<_, i64>(0),
            )?;
        if file.is_new {
            SqliteFtsIndex.insert(&tx, file_id, &file.text)?;
        } else {
            SqliteFtsIndex.replace(&tx, file_id, &file.text)?;
            tx.prepare_cached("DELETE FROM code_symbols WHERE file_id=?1")?
                .execute([file_id])?;
        }
        {
            let mut symbol_stmt = tx.prepare(
                "INSERT INTO code_symbols(file_id,name,kind,line_start,line_end,byte_start,byte_end)
                 VALUES(?1,?2,?3,?4,?5,?6,?7)",
            )?;
            for symbol in file.symbols {
                symbol_stmt.execute(params![
                    file_id,
                    symbol.name,
                    symbol.kind,
                    symbol.line_start,
                    symbol.line_end,
                    symbol.byte_start as i64,
                    symbol.byte_end as i64
                ])?;
            }
        }
    }
    timers.index_writing += write_start.elapsed().as_secs_f64() * 1000.0;
    let commit_start = Instant::now();
    tx.commit()?;
    timers.commit += commit_start.elapsed().as_secs_f64() * 1000.0;
    Ok(())
}

pub fn index_status(db_path: &Path, root: &Path) -> Result<IndexStatus> {
    let scope = repo_scope(root)?;
    let conn = open_db(db_path)?;
    let row = conn
        .query_row(
            "SELECT root,branch,revision,generation,is_complete,structural_complete,eligible_files,eligible_bytes,
          last_index_started,last_index_finished,skipped_json
         FROM repositories WHERE repo_id=?1 AND worktree_id=?2",
            params![scope.repo_id, scope.worktree_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, i64>(6)?,
                    row.get::<_, i64>(7)?,
                    row.get::<_, Option<String>>(8)?,
                    row.get::<_, Option<String>>(9)?,
                    row.get::<_, String>(10)?,
                ))
            },
        )
        .optional()?;
    let Some((
        root,
        branch,
        revision,
        generation,
        catalog_complete,
        structural_complete,
        eligible_files,
        eligible_bytes,
        started,
        finished,
        skipped_json,
    )) = row
    else {
        return Ok(IndexStatus {
            repo_id: scope.repo_id,
            worktree_id: scope.worktree_id,
            root: scope.root.to_string_lossy().into_owned(),
            branch: scope.branch,
            revision: scope.revision,
            generation: 0,
            complete: false,
            catalog_complete: false,
            structural_complete: false,
            eligible_files: 0,
            eligible_bytes: 0,
            indexed_files: 0,
            indexed_bytes: 0,
            index_format_version: INDEX_FORMAT_VERSION,
            last_index_started: None,
            last_index_finished: None,
            skipped: BTreeMap::new(),
        });
    };
    let (indexed_files, indexed_bytes): (i64, i64) = conn.query_row(
        "SELECT count(*),coalesce(sum(size_bytes),0) FROM code_files WHERE repo_id=?1 AND worktree_id=?2",
        params![scope.repo_id, scope.worktree_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    Ok(IndexStatus {
        repo_id: scope.repo_id,
        worktree_id: scope.worktree_id,
        root,
        branch,
        revision,
        generation: generation as u64,
        complete: catalog_complete != 0 && structural_complete != 0,
        catalog_complete: catalog_complete != 0,
        structural_complete: structural_complete != 0,
        eligible_files: eligible_files as u64,
        eligible_bytes: eligible_bytes as u64,
        indexed_files: indexed_files as u64,
        indexed_bytes: indexed_bytes as u64,
        index_format_version: INDEX_FORMAT_VERSION,
        last_index_started: started,
        last_index_finished: finished,
        skipped: serde_json::from_str(&skipped_json).unwrap_or_default(),
    })
}

pub fn search_code(
    db_path: &Path,
    root: &Path,
    query: &str,
    limit: usize,
) -> Result<Vec<CodeResult>> {
    Ok(search_code_with_budget(db_path, root, query, limit, DEFAULT_TOKEN_BUDGET)?.results)
}

pub fn search_code_with_budget(
    db_path: &Path,
    root: &Path,
    query: &str,
    limit: usize,
    token_budget: usize,
) -> Result<CodeSearchResponse> {
    if query.trim().is_empty() {
        bail!("code search query cannot be empty");
    }
    if query.len() > MAX_SEARCH_QUERY_BYTES {
        bail!("code search query exceeds 4096 bytes");
    }
    let scope = repo_scope(root)?;
    let limit = limit.clamp(1, 20);
    let conn = open_db(db_path)?;
    let status: Option<(i64, i64, i64)> = conn
        .query_row(
            "SELECT generation,is_complete,structural_complete FROM repositories WHERE repo_id=?1 AND worktree_id=?2",
            params![scope.repo_id, scope.worktree_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    let (generation, complete) = status
        .map(|(g, catalog, structural)| (g as u64, catalog != 0 && structural != 0))
        .unwrap_or((0, false));
    let candidates = indexed_candidates(&conn, &scope, query, limit)?;
    let mut results = make_results(&scope, query, candidates, complete, limit)?;
    let used_direct_fallback = results.is_empty() && !complete;
    if used_direct_fallback {
        results = cold_search(&conn, &scope, query, generation)?;
    }
    let fallback_has_result = used_direct_fallback && !results.is_empty();
    let budget = token_budget.clamp(64, MAX_TOKEN_BUDGET);
    let was_at_limit = results.len() >= limit;
    let (results, estimated_tokens, context_truncated) = pack_context(results, budget);
    Ok(CodeSearchResponse {
        results,
        token_budget: budget,
        estimated_tokens,
        truncated: !complete || fallback_has_result || was_at_limit || context_truncated,
        token_estimator:
            "midnight-heuristic-v1 (identifier chunks, punctuation, and fixed metadata allowance)"
                .into(),
    })
}

#[derive(Default)]
struct Candidate {
    path: String,
    hash: String,
    generation: u64,
    exact_path: f64,
    exact_symbol: f64,
    lexical: f64,
}

fn indexed_candidates(
    conn: &Connection,
    scope: &RepoScope,
    query: &str,
    limit: usize,
) -> Result<Vec<Candidate>> {
    let mut map = BTreeMap::<i64, Candidate>::new();
    let path_query = query.contains('/') || query.contains('\\');
    if path_query {
        let mut stmt = conn.prepare(
            "SELECT id,rel_path,content_hash,indexed_generation FROM code_files
             WHERE repo_id=?1 AND worktree_id=?2 AND rel_path=?3 LIMIT ?4",
        )?;
        let rows = stmt.query_map(
            params![scope.repo_id, scope.worktree_id, query, limit as i64],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                ))
            },
        )?;
        for row in rows {
            let (id, path, hash, generation_value) = row?;
            map.insert(
                id,
                Candidate {
                    path,
                    hash,
                    generation: generation_value as u64,
                    exact_path: 100.0,
                    ..Default::default()
                },
            );
        }
        if map.is_empty() {
            let mut stmt = conn.prepare(
                "SELECT id,rel_path,content_hash,indexed_generation FROM code_files
                 WHERE repo_id=?1 AND worktree_id=?2 AND instr(lower(rel_path),lower(?3))>0
                 ORDER BY rel_path LIMIT ?4",
            )?;
            let rows = stmt.query_map(
                params![scope.repo_id, scope.worktree_id, query, limit as i64],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, i64>(3)?,
                    ))
                },
            )?;
            for row in rows {
                let (id, path, hash, generation_value) = row?;
                map.insert(
                    id,
                    Candidate {
                        path,
                        hash,
                        generation: generation_value as u64,
                        exact_path: 60.0,
                        ..Default::default()
                    },
                );
            }
        }
        return Ok(map.into_values().collect());
    }
    {
        let mut stmt = conn.prepare(
            "SELECT id,rel_path,content_hash,indexed_generation FROM code_files
             WHERE repo_id=?1 AND worktree_id=?2 AND instr(lower(rel_path),lower(?3))>0
             ORDER BY CASE WHEN lower(rel_path)=lower(?3) THEN 0 ELSE 1 END,rel_path LIMIT ?4",
        )?;
        let rows = stmt.query_map(
            params![scope.repo_id, scope.worktree_id, query, limit as i64],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                ))
            },
        )?;
        for row in rows {
            let (id, path, hash, generation_value) = row?;
            let score = if path.eq_ignore_ascii_case(query) {
                100.0
            } else {
                60.0
            };
            map.entry(id).or_insert(Candidate {
                path,
                hash,
                generation: generation_value as u64,
                exact_path: score,
                ..Default::default()
            });
        }
    }
    {
        let mut stmt = conn.prepare(
            "SELECT DISTINCT f.id,f.rel_path,f.content_hash,f.indexed_generation
             FROM code_symbols s JOIN code_files f ON f.id=s.file_id
             WHERE f.repo_id=?1 AND f.worktree_id=?2 AND lower(s.name)=lower(?3)
             ORDER BY f.rel_path LIMIT ?4",
        )?;
        let rows = stmt.query_map(
            params![scope.repo_id, scope.worktree_id, query.trim(), limit as i64],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                ))
            },
        )?;
        for row in rows {
            let (id, path, hash, generation_value) = row?;
            let candidate = map.entry(id).or_insert(Candidate {
                path,
                hash,
                generation: generation_value as u64,
                ..Default::default()
            });
            candidate.exact_symbol = 80.0;
        }
    }
    if let Some(match_query) = code_match_query(query) {
        let index = SqliteFtsIndex;
        for hit in index.search(
            conn,
            &scope.repo_id,
            &scope.worktree_id,
            &match_query,
            limit,
        )? {
            let lexical = (25.0 + (-hit.rank * 1_000_000.0).clamp(0.0, 25.0)).clamp(0.0, 50.0);
            let candidate = map.entry(hit.document_id).or_insert(Candidate {
                path: hit.path,
                hash: hit.content_hash,
                generation: hit.generation,
                ..Default::default()
            });
            candidate.lexical = lexical;
        }
    }
    let mut candidates = map.into_values().collect::<Vec<_>>();
    candidates.sort_by(|a, b| {
        let sa = a.exact_path + a.exact_symbol + a.lexical;
        let sb = b.exact_path + b.exact_symbol + b.lexical;
        sb.total_cmp(&sa).then_with(|| a.path.cmp(&b.path))
    });
    candidates.truncate(limit);
    Ok(candidates)
}

fn code_match_query(query: &str) -> Option<String> {
    let terms = query
        .split(|ch: char| !(ch.is_alphanumeric() || ch == '_'))
        .filter(|term| term.chars().any(char::is_alphanumeric))
        .map(|term| format!("\"{}\"", term.replace('"', "\"\"")))
        .collect::<Vec<_>>();
    (!terms.is_empty()).then(|| terms.join(" OR "))
}

fn make_results(
    scope: &RepoScope,
    query: &str,
    candidates: Vec<Candidate>,
    complete: bool,
    limit: usize,
) -> Result<Vec<CodeResult>> {
    let mut out = Vec::new();
    for candidate in candidates.into_iter().take(limit) {
        let abs = scope.root.join(&candidate.path);
        let checked = current_text(&scope.root, &abs).ok();
        let (freshness, snippet, line_start, line_end) = if let Some((text, current_hash)) = checked
        {
            if current_hash == candidate.hash {
                let (start, end, snippet) = excerpt(&text, query);
                ("fresh".to_string(), Some(snippet), Some(start), Some(end))
            } else {
                ("stale".to_string(), None, None, None)
            }
        } else {
            ("unavailable".to_string(), None, None, None)
        };
        let (snippet, snippet_truncated) = snippet
            .map(bounded_snippet)
            .map(|(text, truncated)| (Some(text), truncated))
            .unwrap_or((None, false));
        let mut reasons = Vec::new();
        let mut breakdown = BTreeMap::new();
        breakdown.insert("exact_path".into(), candidate.exact_path);
        breakdown.insert("exact_symbol".into(), candidate.exact_symbol);
        breakdown.insert("lexical_fts".into(), candidate.lexical);
        if candidate.exact_path > 0.0 {
            reasons.push("path".to_string());
        }
        if candidate.exact_symbol > 0.0 {
            reasons.push("symbol".to_string());
        }
        if candidate.lexical > 0.0 {
            reasons.push("lexical".to_string());
        }
        out.push(CodeResult {
            path: candidate.path,
            line_start,
            line_end,
            match_reason: reasons.join("+"),
            scope: format!("repo:{}:worktree:{}", scope.repo_id, scope.worktree_id),
            index_generation: candidate.generation,
            index_complete: complete,
            freshness,
            snippet,
            snippet_truncated,
            estimated_tokens: 0,
            score: candidate.exact_path + candidate.exact_symbol + candidate.lexical,
            score_breakdown: breakdown,
        });
    }
    Ok(out)
}

fn cold_search(
    conn: &Connection,
    scope: &RepoScope,
    query: &str,
    generation: u64,
) -> Result<Vec<CodeResult>> {
    let mut builder = WalkBuilder::new(&scope.root);
    builder
        .hidden(false)
        .git_ignore(true)
        .git_exclude(true)
        .require_git(false)
        .parents(false)
        .current_dir(&scope.root)
        .follow_links(false)
        .max_depth(Some(128));
    let aetherignore = scope.root.join(".aetherignore");
    if valid_aetherignore(&aetherignore, &scope.root) {
        builder.add_ignore(aetherignore);
    }
    let root_for_filter = scope.root.clone();
    builder.filter_entry(move |entry| {
        if entry.depth() == 0 {
            return true;
        }
        entry
            .path()
            .strip_prefix(&root_for_filter)
            .ok()
            .is_some_and(|rel| !is_vcs_metadata_path(rel) && !default_generated_path(rel))
    });
    conn.execute_batch(
        "CREATE TEMP TABLE IF NOT EXISTS midnight_cold_paths(
             path TEXT PRIMARY KEY
         ) WITHOUT ROWID;
         DELETE FROM midnight_cold_paths;",
    )?;
    {
        let mut insert =
            conn.prepare_cached("INSERT OR IGNORE INTO midnight_cold_paths(path) VALUES(?1)")?;
        for entry in builder.build() {
            let Ok(entry) = entry else { continue };
            if entry.depth() == 0
                || entry
                    .file_type()
                    .is_some_and(|t| t.is_dir() || t.is_symlink())
            {
                continue;
            }
            let path = entry.path();
            let Some(rel) = path.strip_prefix(&scope.root).ok() else {
                continue;
            };
            if is_secret_path(rel) || is_vcs_metadata_path(rel) || default_generated_path(rel) {
                continue;
            }
            let Ok(meta) = fs::symlink_metadata(path) else {
                continue;
            };
            if meta.is_file() && !meta.file_type().is_symlink() && meta.len() <= MAX_FILE_BYTES {
                insert.execute([rel.to_string_lossy().replace('\\', "/")])?;
            }
        }
    }
    let mut results = Vec::new();
    let terms = query
        .split_whitespace()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>();
    let mut paths = conn.prepare("SELECT path FROM midnight_cold_paths ORDER BY path")?;
    let path_rows = paths.query_map([], |row| row.get::<_, String>(0))?;
    for path_row in path_rows {
        let rel_path = path_row?;
        let rel = Path::new(&rel_path);
        let Ok(text) = current_text(&scope.root, &scope.root.join(rel)) else {
            continue;
        };
        let lower_path = rel.to_string_lossy().to_ascii_lowercase();
        let lower_text = text.0.to_ascii_lowercase();
        let path_hit = lower_path.contains(&query.to_ascii_lowercase());
        let text_hit = !terms.is_empty()
            && terms
                .iter()
                .any(|term| lower_text.contains(&term.to_ascii_lowercase()));
        if !path_hit && !text_hit {
            continue;
        }
        let (line_start, line_end, snippet) = if path_hit {
            (1, 1, first_lines(&text.0, 1, 1))
        } else {
            excerpt(&text.0, query)
        };
        let (snippet, snippet_truncated) = bounded_snippet(snippet);
        let mut breakdown = BTreeMap::new();
        breakdown.insert("cold_path".into(), if path_hit { 60.0 } else { 0.0 });
        breakdown.insert("cold_lexical".into(), if text_hit { 25.0 } else { 0.0 });
        results.push(CodeResult {
            path: rel_path,
            line_start: Some(line_start),
            line_end: Some(line_end),
            match_reason: if path_hit {
                "cold_path".into()
            } else {
                "cold_lexical".into()
            },
            scope: format!("repo:{}:worktree:{}", scope.repo_id, scope.worktree_id),
            index_generation: generation,
            index_complete: false,
            freshness: "fresh_unindexed".into(),
            snippet: Some(snippet),
            snippet_truncated,
            estimated_tokens: 0,
            score: if path_hit { 60.0 } else { 25.0 },
            score_breakdown: breakdown,
        });
        if !results.is_empty() {
            break;
        }
    }
    Ok(results)
}

pub fn open_range(
    db_path: &Path,
    root: &Path,
    path: &str,
    start: u32,
    end: u32,
) -> Result<OpenRangeResult> {
    if start == 0 || end < start || end - start + 1 > MAX_OPEN_RANGE_LINES {
        bail!("line range must be 1..=200 lines and start at line 1 or later");
    }
    let scope = repo_scope(root)?;
    let rel = safe_relative_path(path)?;
    let absolute = scope.root.join(&rel);
    let indexed: Option<(String, i64, i64, i64, i64)> = {
        let conn = open_db(db_path)?;
        conn.query_row(
            "SELECT f.content_hash,f.indexed_generation,r.generation,r.is_complete,r.structural_complete
             FROM code_files f JOIN repositories r ON r.repo_id=f.repo_id AND r.worktree_id=f.worktree_id
             WHERE f.repo_id=?1 AND f.worktree_id=?2 AND f.rel_path=?3",
            params![scope.repo_id,scope.worktree_id,rel],
            |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?)),
        ).optional()?
    };
    let (text, hash) = current_text(&scope.root, &absolute)
        .with_context(|| format!("read source file {}", path))?;
    let stored_hash = indexed.as_ref().map(|r| r.0.as_str());
    let freshness = match stored_hash {
        Some(expected) if expected == hash => "fresh",
        Some(_) => "stale",
        None => "unindexed",
    };
    let (generation, complete) = indexed
        .as_ref()
        .map(|r| (r.2 as u64, r.3 != 0 && r.4 != 0))
        .unwrap_or((0, false));
    let count = text.lines().count().max(1) as u32;
    if start > count {
        bail!("line_start exceeds current source length ({count} lines)");
    }
    let bounded_end = end.min(count);
    let content = first_lines(&text, start, bounded_end);
    Ok(OpenRangeResult {
        path: rel,
        line_start: start,
        line_end: bounded_end,
        content_hash: hash,
        freshness: freshness.into(),
        index_generation: generation,
        index_complete: complete,
        content,
    })
}

pub fn list_symbols(
    db_path: &Path,
    root: &Path,
    name: Option<&str>,
    limit: usize,
) -> Result<Vec<serde_json::Value>> {
    if name.is_some_and(|name| name.len() > MAX_SEARCH_QUERY_BYTES) {
        bail!("symbol search query exceeds 4096 bytes");
    }
    let scope = repo_scope(root)?;
    let conn = open_db(db_path)?;
    let pattern = name.unwrap_or("");
    let mut stmt = conn.prepare(
        "SELECT f.rel_path,s.name,s.kind,s.line_start,s.line_end,s.byte_start,s.byte_end,f.indexed_generation,r.generation,(r.is_complete AND r.structural_complete),f.content_hash
         FROM code_symbols s JOIN code_files f ON f.id=s.file_id
         JOIN repositories r ON r.repo_id=f.repo_id AND r.worktree_id=f.worktree_id
         WHERE f.repo_id=?1 AND f.worktree_id=?2 AND (?3='' OR instr(lower(s.name),lower(?3))>0)
         ORDER BY CASE WHEN lower(s.name)=lower(?3) THEN 0 ELSE 1 END,s.name,f.rel_path LIMIT ?4",
    )?;
    let rows = stmt.query_map(
        params![
            scope.repo_id,
            scope.worktree_id,
            pattern,
            limit.clamp(1, 1000) as i64
        ],
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, u32>(3)?,
                row.get::<_, u32>(4)?,
                row.get::<_, i64>(5)?,
                row.get::<_, i64>(6)?,
                row.get::<_, i64>(7)?,
                row.get::<_, i64>(8)?,
                row.get::<_, i64>(9)? != 0,
                row.get::<_, String>(10)?,
            ))
        },
    )?;
    let rows = rows.collect::<rusqlite::Result<Vec<_>>>()?;
    drop(stmt);
    drop(conn);
    let mut freshness_by_path = BTreeMap::<String, bool>::new();
    let mut results = Vec::with_capacity(rows.len());
    for (
        path,
        symbol,
        kind,
        line_start,
        line_end,
        byte_start,
        byte_end,
        file_generation,
        repo_generation,
        complete,
        expected_hash,
    ) in rows
    {
        let fresh = *freshness_by_path.entry(path.clone()).or_insert_with(|| {
            current_text(&scope.root, &scope.root.join(&path))
                .is_ok_and(|(_, hash)| hash == expected_hash)
        });
        results.push(serde_json::json!({
            "path":path,"name":symbol,"kind":kind,"line_start":line_start,"line_end":line_end,
            "byte_start":byte_start,"byte_end":byte_end,"index_generation":file_generation,
            "repository_generation":repo_generation,"index_complete":complete,
            "content_hash":expected_hash,"freshness":if fresh{"fresh"}else{"stale"},
            "scope":format!("repo:{}:worktree:{}",scope.repo_id,scope.worktree_id)
        }));
    }
    Ok(results)
}

pub fn repo_map(db_path: &Path, root: &Path) -> Result<RepoMap> {
    let scope = repo_scope(root)?;
    let conn = open_db(db_path)?;
    let status: Option<(u64, bool)> = conn
        .query_row(
            "SELECT generation,(is_complete AND structural_complete) FROM repositories WHERE repo_id=?1 AND worktree_id=?2",
            params![scope.repo_id, scope.worktree_id],
            |row| Ok((row.get::<_, i64>(0)? as u64, row.get::<_, i64>(1)? != 0)),
        )
        .optional()?;
    let (generation, complete) = status.unwrap_or((0, false));
    let mut files_by_language = BTreeMap::new();
    let mut top_directories = BTreeMap::new();
    {
        let mut stmt = conn.prepare(
            "SELECT rel_path,language FROM code_files WHERE repo_id=?1 AND worktree_id=?2",
        )?;
        let rows = stmt.query_map(params![scope.repo_id, scope.worktree_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        for row in rows {
            let (path, language) = row?;
            *files_by_language.entry(language).or_insert(0) += 1;
            let directory = Path::new(&path)
                .components()
                .next()
                .map(|c| c.as_os_str().to_string_lossy().to_string())
                .unwrap_or_else(|| ".".into());
            *top_directories.entry(directory).or_insert(0) += 1;
        }
    }
    let mut symbols_by_kind = BTreeMap::new();
    {
        let mut stmt = conn.prepare(
            "SELECT s.kind,count(*) FROM code_symbols s JOIN code_files f ON f.id=s.file_id WHERE f.repo_id=?1 AND f.worktree_id=?2 GROUP BY s.kind",
        )?;
        let rows = stmt.query_map(params![scope.repo_id, scope.worktree_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?;
        for row in rows {
            let (kind, count) = row?;
            symbols_by_kind.insert(kind, count as u64);
        }
    }
    Ok(RepoMap {
        repo_id: scope.repo_id,
        worktree_id: scope.worktree_id,
        generation,
        complete,
        files_by_language,
        symbols_by_kind,
        top_directories,
    })
}

fn current_text(root: &Path, path: &Path) -> Result<(String, String)> {
    let canonical = path.canonicalize()?;
    if !canonical.starts_with(root) {
        bail!("path escapes repository root");
    }
    let metadata = fs::symlink_metadata(&canonical)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > MAX_FILE_BYTES {
        bail!("source is a symlink, not a file, or exceeds the 1 MiB read limit");
    }
    let relative = canonical.strip_prefix(root)?;
    let bytes = read_limited_from_root(root, relative, MAX_FILE_BYTES as usize + 1)?;
    if bytes.len() > MAX_FILE_BYTES as usize || bytes.contains(&0) || has_binary_controls(&bytes) {
        bail!("source is binary or oversized");
    }
    let text = String::from_utf8(bytes).context("source is not UTF-8")?;
    let hash = hash_bytes(text.as_bytes());
    Ok((text, hash))
}

fn safe_relative_path(input: &str) -> Result<String> {
    if input.len() > MAX_RELATIVE_PATH_BYTES {
        bail!("repository-relative path exceeds 4096 bytes");
    }
    let path = Path::new(input);
    if path.is_absolute()
        || path
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
    {
        bail!("path must be repository-relative and cannot contain traversal components");
    }
    Ok(path.to_string_lossy().replace('\\', "/"))
}

fn excerpt(text: &str, query: &str) -> (u32, u32, String) {
    let query = query.trim();
    let lower = text.to_ascii_lowercase();
    let found = (!query.is_empty())
        .then(|| lower.find(&query.to_ascii_lowercase()))
        .flatten();
    let offset = found.unwrap_or_else(|| {
        query
            .split_whitespace()
            .find_map(|term| lower.find(&term.to_ascii_lowercase()))
            .unwrap_or(0)
    });
    let line = text.as_bytes()[..offset.min(text.len())]
        .iter()
        .filter(|&&b| b == b'\n')
        .count()
        + 1;
    let start = line as u32;
    let end = (start + MAX_SNIPPET_LINES as u32 - 1).min(text.lines().count().max(1) as u32);
    (start, end, first_lines(text, start, end))
}

fn first_lines(text: &str, start: u32, end: u32) -> String {
    text.lines()
        .enumerate()
        .filter(|(i, _)| (*i + 1) >= start as usize && (*i + 1) <= end as usize)
        .map(|(i, line)| format!("{}: {}", i + 1, line))
        .collect::<Vec<_>>()
        .join("\n")
}

fn bounded_snippet(mut snippet: String) -> (String, bool) {
    if snippet.len() <= MAX_SNIPPET_BYTES {
        return (snippet, false);
    }
    let mut end = MAX_SNIPPET_BYTES;
    while !snippet.is_char_boundary(end) {
        end -= 1;
    }
    snippet.truncate(end);
    (snippet, true)
}

fn pack_context(results: Vec<CodeResult>, token_budget: usize) -> (Vec<CodeResult>, usize, bool) {
    let input_count = results.len();
    let mut packed = Vec::with_capacity(input_count);
    let mut used = 0usize;
    let mut truncated = false;
    for mut result in results {
        let metadata_tokens = 28
            + estimate_tokens(&result.path)
            + estimate_tokens(&result.match_reason)
            + estimate_tokens(&result.scope);
        if used.saturating_add(metadata_tokens) > token_budget {
            truncated = true;
            break;
        }
        let available = token_budget - used - metadata_tokens;
        let mut snippet_tokens = 0;
        if let Some(snippet) = result.snippet.take() {
            let estimate = estimate_tokens(&snippet);
            if estimate > available {
                truncated = true;
                result.snippet_truncated = true;
                if available > 0 {
                    let shorter = truncate_to_tokens(&snippet, available);
                    snippet_tokens = estimate_tokens(&shorter);
                    result.snippet = (!shorter.is_empty()).then_some(shorter);
                }
            } else {
                snippet_tokens = estimate;
                result.snippet = Some(snippet);
            }
        }
        result.estimated_tokens = metadata_tokens + snippet_tokens;
        used += result.estimated_tokens;
        packed.push(result);
        if used >= token_budget {
            if packed.len() < input_count {
                truncated = true;
            }
            break;
        }
    }
    if packed.len() < input_count {
        truncated = true;
    }
    (packed, used, truncated)
}

fn estimate_tokens(text: &str) -> usize {
    let mut tokens = 0usize;
    let mut identifier_bytes = 0usize;
    for character in text.chars() {
        if character.is_ascii_alphanumeric() || character == '_' {
            identifier_bytes += 1;
        } else {
            tokens += identifier_bytes.div_ceil(4);
            identifier_bytes = 0;
            if !character.is_whitespace() {
                tokens += 1;
            }
        }
    }
    tokens + identifier_bytes.div_ceil(4)
}

fn truncate_to_tokens(text: &str, token_limit: usize) -> String {
    let ends = text
        .char_indices()
        .map(|(index, character)| index + character.len_utf8())
        .collect::<Vec<_>>();
    let mut low = 0usize;
    let mut high = ends.len();
    while low < high {
        let middle = (low + high).div_ceil(2);
        let byte_end = ends.get(middle.saturating_sub(1)).copied().unwrap_or(0);
        if estimate_tokens(&text[..byte_end]) <= token_limit {
            low = middle;
        } else {
            high = middle - 1;
        }
    }
    let byte_end = ends.get(low.saturating_sub(1)).copied().unwrap_or(0);
    text[..byte_end].to_string()
}

fn modified_ns(metadata: &fs::Metadata) -> i64 {
    metadata
        .modified()
        .ok()
        .and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| {
            (d.as_secs() as i128 * 1_000_000_000 + d.subsec_nanos() as i128).min(i64::MAX as i128)
                as i64
        })
        .unwrap_or(0)
}

fn read_limited_from_root(root: &Path, relative: &Path, limit: usize) -> Result<Vec<u8>> {
    #[cfg(unix)]
    {
        use std::ffi::CString;
        use std::os::fd::{AsRawFd, FromRawFd};
        use std::os::unix::ffi::OsStrExt;
        use std::os::unix::fs::OpenOptionsExt;

        let parts = relative
            .components()
            .map(|component| match component {
                Component::Normal(name) => Ok(name.to_os_string()),
                _ => bail!("source path must stay beneath repository root"),
            })
            .collect::<Result<Vec<_>>>()?;
        if parts.is_empty() || parts.len() > 128 {
            bail!("source path has an invalid or excessive directory depth");
        }
        let (filename, directories) = parts.split_last().expect("non-empty path checked");
        let mut directory = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(root)
            .context("open repository root without following symlinks")?;
        for component in directories {
            let name = CString::new(component.as_bytes())?;
            let descriptor = unsafe {
                libc::openat(
                    directory.as_raw_fd(),
                    name.as_ptr(),
                    libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                )
            };
            if descriptor < 0 {
                return Err(std::io::Error::last_os_error())
                    .context("open repository subdirectory without following symlinks");
            }
            directory = unsafe { File::from_raw_fd(descriptor) };
        }
        let name = CString::new(filename.as_bytes())?;
        let descriptor = unsafe {
            libc::openat(
                directory.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if descriptor < 0 {
            return Err(std::io::Error::last_os_error())
                .context("open source file without following symlinks");
        }
        let file = unsafe { File::from_raw_fd(descriptor) };
        read_bounded(file, limit)
    }
    #[cfg(not(unix))]
    {
        let _ = (root, relative, limit);
        bail!("safe repository-relative file access requires Unix openat support on this build")
    }
}

fn read_bounded(file: File, limit: usize) -> Result<Vec<u8>> {
    let mut data = Vec::with_capacity(limit.min(64 * 1024));
    file.take(limit as u64).read_to_end(&mut data)?;
    Ok(data)
}

fn has_binary_controls(bytes: &[u8]) -> bool {
    bytes
        .iter()
        .any(|byte| (*byte < 0x20 && !matches!(*byte, b'\n' | b'\r' | b'\t')) || *byte == 0x7f)
}

fn is_secret_path(path: &Path) -> bool {
    if path.components().any(|component| {
        let component = component.as_os_str().to_string_lossy();
        [".aws", ".ssh", ".gnupg", ".docker", ".kube"]
            .iter()
            .any(|name| component.eq_ignore_ascii_case(name))
    }) {
        return true;
    }
    let lower = path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_ascii_lowercase();
    let ext = path
        .extension()
        .unwrap_or_default()
        .to_string_lossy()
        .to_ascii_lowercase();
    lower == ".env"
        || lower.starts_with(".env.")
        || lower == ".netrc"
        || lower == ".npmrc"
        || [".pypirc", ".pgpass", ".htpasswd"].contains(&lower.as_str())
        || lower.contains("credentials")
        || lower.contains("secret")
        || ["pem", "key", "p12", "pfx", "keystore", "mobileprovision"].contains(&ext.as_str())
        || lower.starts_with("id_rsa")
        || lower.starts_with("id_ed25519")
        || lower.starts_with("id_ecdsa")
        || lower.starts_with("id_dsa")
        || lower == "authorized_keys"
        || lower == "known_hosts"
}

fn is_vcs_metadata_path(path: &Path) -> bool {
    path.components().any(|component| {
        let component = component.as_os_str().to_string_lossy();
        [".git", ".hg", ".svn"]
            .iter()
            .any(|name| component.eq_ignore_ascii_case(name))
    })
}

fn valid_aetherignore(path: &Path, root: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|meta| {
        meta.is_file() && !meta.file_type().is_symlink() && meta.len() <= MAX_AETHERIGNORE_BYTES
    }) && path
        .canonicalize()
        .is_ok_and(|canonical| canonical.starts_with(root))
}

fn default_generated_path(path: &Path) -> bool {
    const DIRS: &[&str] = &[
        ".git",
        ".hg",
        ".svn",
        "node_modules",
        "target",
        "dist",
        "build",
        "out",
        "vendor",
        "third_party",
        "coverage",
        ".next",
        ".nuxt",
        ".cache",
        "__pycache__",
        ".venv",
        "venv",
        "Pods",
        "DerivedData",
        "bower_components",
        "site-packages",
        ".turbo",
        ".parcel-cache",
    ];
    path.components().any(|component| {
        let component = component.as_os_str().to_string_lossy();
        DIRS.iter().any(|dir| component.eq_ignore_ascii_case(dir))
    }) || path.file_name().is_some_and(|name| {
        let name = name.to_string_lossy().to_ascii_lowercase();
        name.ends_with(".min.js") || name.ends_with(".map") || name.ends_with(".lockb")
    })
}

fn bump(skipped: &mut BTreeMap<String, u64>, reason: &str) {
    *skipped.entry(reason.into()).or_default() += 1;
}

fn language_for(path: &str) -> Option<String> {
    let ext = Path::new(path)
        .extension()?
        .to_string_lossy()
        .to_ascii_lowercase();
    match ext.as_str() {
        "rs" => Some("rust".into()),
        "py" | "pyi" => Some("python".into()),
        "ts" | "mts" | "cts" => Some("typescript".into()),
        "tsx" => Some("tsx".into()),
        "js" | "mjs" | "cjs" | "jsx" => Some("javascript".into()),
        "go" => Some("go".into()),
        "c" | "h" => Some("c".into()),
        "cc" | "cpp" | "cxx" | "hpp" | "hh" => Some("cpp".into()),
        "java" => Some("java".into()),
        "kt" => Some("kotlin".into()),
        "swift" => Some("swift".into()),
        "json" => Some("json".into()),
        "md" | "mdx" => Some("markdown".into()),
        "toml" => Some("toml".into()),
        "yaml" | "yml" => Some("yaml".into()),
        "sh" | "bash" => Some("shell".into()),
        _ => None,
    }
}

#[derive(Default)]
struct ParserCache {
    rust: Option<tree_sitter::Parser>,
    python: Option<tree_sitter::Parser>,
    typescript: Option<tree_sitter::Parser>,
    tsx: Option<tree_sitter::Parser>,
}

impl ParserCache {
    fn extract(&mut self, language: Option<&str>, text: &str) -> Option<(Vec<Symbol>, bool)> {
        self.extract_with_budget(language, text, PARSER_TIME_BUDGET)
    }

    fn extract_with_budget(
        &mut self,
        language: Option<&str>,
        text: &str,
        budget: Duration,
    ) -> Option<(Vec<Symbol>, bool)> {
        let parser_slot = match language? {
            "rust" => &mut self.rust,
            "python" => &mut self.python,
            "typescript" | "javascript" => &mut self.typescript,
            "tsx" => &mut self.tsx,
            _ => return None,
        };
        if parser_slot.is_none() {
            let language = match language? {
                "rust" => tree_sitter_rust::LANGUAGE.into(),
                "python" => tree_sitter_python::LANGUAGE.into(),
                "typescript" | "javascript" => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
                "tsx" => tree_sitter_typescript::LANGUAGE_TSX.into(),
                _ => return None,
            };
            let mut parser = tree_sitter::Parser::new();
            if parser.set_language(&language).is_err() {
                return Some((Vec::new(), true));
            }
            *parser_slot = Some(parser);
        }
        let parser = parser_slot.as_mut()?;
        let started = Instant::now();
        let mut progress = |_state: &tree_sitter::ParseState| {
            if started.elapsed() >= budget {
                std::ops::ControlFlow::Break(())
            } else {
                std::ops::ControlFlow::Continue(())
            }
        };
        let options = tree_sitter::ParseOptions::new().progress_callback(&mut progress);
        let bytes = text.as_bytes();
        let mut input =
            |offset: usize, _position: tree_sitter::Point| bytes.get(offset..).unwrap_or_default();
        let Some(tree) = parser.parse_with_options(&mut input, None, Some(options)) else {
            return Some((Vec::new(), true));
        };
        let parse_error = started.elapsed() >= budget || tree.root_node().has_error();
        let mut symbols = Vec::new();
        collect_symbols(tree.root_node(), text.as_bytes(), &mut symbols);
        Some((symbols, parse_error))
    }
}

fn collect_symbols(node: tree_sitter::Node<'_>, source: &[u8], out: &mut Vec<Symbol>) {
    const TYPES: &[&str] = &[
        "function_item",
        "struct_item",
        "enum_item",
        "trait_item",
        "type_item",
        "const_item",
        "static_item",
        "mod_item",
        "union_item",
        "macro_definition",
        "function_definition",
        "class_definition",
        "function_declaration",
        "class_declaration",
        "interface_declaration",
        "type_alias_declaration",
        "enum_declaration",
        "method_definition",
        "variable_declarator",
    ];
    if TYPES.contains(&node.kind())
        && let Some(name) = node.child_by_field_name("name")
        && let Ok(value) = name.utf8_text(source)
    {
        let start = node.start_position();
        let end = node.end_position();
        let end_line = if end.column == 0 && end.row > start.row {
            end.row
        } else {
            end.row + 1
        };
        out.push(Symbol {
            name: value.to_string(),
            kind: node.kind().to_string(),
            line_start: start.row as u32 + 1,
            line_end: end_line as u32,
            byte_start: node.start_byte(),
            byte_end: node.end_byte(),
        });
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_symbols(child, source, out);
    }
}

/// A coarse process high-water mark. macOS reports bytes; Linux reports KiB.
pub fn peak_rss_bytes() -> Option<u64> {
    #[cfg(unix)]
    unsafe {
        let mut usage: libc::rusage = std::mem::zeroed();
        if libc::getrusage(libc::RUSAGE_SELF, &mut usage) != 0 {
            return None;
        }
        let value = usage.ru_maxrss as u64;
        #[cfg(target_os = "macos")]
        {
            Some(value)
        }
        #[cfg(not(target_os = "macos"))]
        {
            Some(value.saturating_mul(1024))
        }
    }
    #[cfg(not(unix))]
    {
        None
    }
}

pub fn database_bytes(path: &Path) -> u64 {
    [
        path.to_path_buf(),
        PathBuf::from(format!("{}-wal", path.display())),
        PathBuf::from(format!("{}-shm", path.display())),
    ]
    .iter()
    .filter_map(|p| fs::metadata(p).ok())
    .map(|m| m.len())
    .sum()
}

pub fn wal_checkpoint(db_path: &Path) -> Result<()> {
    let conn = open_db(db_path)?;
    conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;
    Ok(())
}

pub fn sqlite_version(db_path: &Path) -> Result<String> {
    let conn = open_db(db_path)?;
    Ok(conn.query_row("SELECT sqlite_version()", [], |row| row.get(0))?)
}

#[cfg(test)]
mod parser_budget_tests {
    use super::ParserCache;
    use std::time::Duration;

    #[test]
    fn parser_budget_marks_result_structurally_incomplete() {
        let mut cache = ParserCache::default();
        let (_, incomplete) = cache
            .extract_with_budget(Some("rust"), "pub fn bounded() {}", Duration::ZERO)
            .unwrap();
        assert!(incomplete);
    }
}
