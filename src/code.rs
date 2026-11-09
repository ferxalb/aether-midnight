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
use std::sync::{Arc, Mutex, OnceLock, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use crate::db::{RepoScope, hash_bytes, now_utc, open_db, repo_scope};
use crate::text_index::{SqliteFtsIndex, TextIndex};
use crate::{INDEX_FORMAT_VERSION, MAX_FILE_BYTES};

const DEFAULT_BATCH_FILES: usize = 512;
const DEFAULT_BATCH_BYTES: usize = 16 * 1024 * 1024;
const MAX_OPEN_RANGE_LINES: u32 = 200;
const MAX_SNIPPET_LINES: usize = 8;
const MAX_SNIPPET_BYTES: usize = 4096;
const DEFAULT_TOKEN_BUDGET: usize = 2048;
pub(crate) const MAX_TOKEN_BUDGET: usize = 8192;
const MAX_SEARCH_QUERY_BYTES: usize = 4096;
const MAX_RELATIVE_PATH_BYTES: usize = 4096;
const MAX_AETHERIGNORE_BYTES: u64 = MAX_FILE_BYTES;
const MAX_PARSE_ERROR_PATHS: usize = 20;
const PARSER_TIME_BUDGET: Duration = Duration::from_millis(250);
const MAX_INDEX_WORKERS: usize = 12;
const MAX_PIPELINE_BYTES: usize = 8 * 1024 * 1024;

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
    pub sample_id: String,
    pub worker_count: usize,
    pub batch_file_limit: usize,
    pub batch_byte_limit: usize,
    pub files_discovered: u64,
    pub bytes_read: u64,
    pub source_file_open_count: u64,
    pub max_in_flight_bytes: u64,
    pub max_pipeline_buffered_bytes: u64,
    pub max_queued_artifacts: u64,
    pub max_pipeline_artifacts: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct FileRefreshReport {
    pub repo_id: String,
    pub worktree_id: String,
    pub path: String,
    pub operation: String,
    pub generation: u64,
    pub catalog_complete: bool,
    pub reconciliation_required: bool,
    pub reason: Option<String>,
    pub elapsed_ms: f64,
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
    /// Indexed files whose Tree-sitter parse failed (syntax error or time budget). They stay
    /// searchable by text; only their structural symbols are affected.
    pub parse_error_files: u64,
    /// At most `MAX_PARSE_ERROR_PATHS` of those paths, sorted.
    pub parse_error_paths: Vec<String>,
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

struct PendingFile {
    rel_path: String,
    hash: String,
    language: String,
    size: u64,
    modified_ns: i64,
    text: String,
    identifier_terms: String,
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
    metadata: f64,
    read: f64,
    hashing: f64,
    identifier_extraction: f64,
    parsing: f64,
    index_writing: f64,
    fts_payload_assembly: f64,
    fts_execution: f64,
    metadata_writes: f64,
    symbol_writes: f64,
    commit: f64,
    reconciliation: f64,
    worker_wait: f64,
}

#[derive(Default)]
struct WorkerWork {
    read_ms: f64,
    hash_ms: f64,
    identifier_extraction_ms: f64,
    parse_ms: f64,
    bytes_read: u64,
    source_file_open_count: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum IndexStage {
    Discovery,
    FileRead,
    Parsing,
    QueueBackpressure,
    SqliteBatchWrite,
}

pub(crate) type IndexStageHook = Arc<dyn Fn(IndexStage) + Send + Sync>;

struct IndexJob {
    sequence: usize,
    root: PathBuf,
    rel_path: String,
    size: u64,
    modified_ns: i64,
    previous_hash: Option<String>,
    previous_parse_error: bool,
    is_new: bool,
    language: Option<String>,
    verify_hashes: bool,
    reserved_bytes: usize,
}

enum PreparedKind {
    Unchanged { parse_error: bool },
    Changed(PendingFile),
    Skipped(&'static str),
    Cancelled,
}

struct PreparedFile {
    sequence: usize,
    rel_path: String,
    size: u64,
    modified_ns: i64,
    reserved_bytes: usize,
    work: WorkerWork,
    kind: PreparedKind,
}

struct WriterState {
    batch_file_limit: usize,
    batch_byte_limit: usize,
    pending: Vec<PendingFile>,
    pending_seen: Vec<SeenFile>,
    pending_source_bytes: usize,
    pending_reserved_bytes: usize,
    eligible_files: u64,
    eligible_bytes: u64,
    changed: u64,
    unchanged: u64,
    files_discovered: u64,
    bytes_read: u64,
    source_file_open_count: u64,
    max_in_flight_bytes: u64,
    max_pipeline_buffered_bytes: u64,
    max_queued_artifacts: u64,
    max_pipeline_artifacts: u64,
    skipped: BTreeMap<String, u64>,
    structural_complete: bool,
    cancelled: bool,
    stage_hook: Option<IndexStageHook>,
}

impl Default for WriterState {
    fn default() -> Self {
        Self {
            batch_file_limit: DEFAULT_BATCH_FILES,
            batch_byte_limit: DEFAULT_BATCH_BYTES,
            pending: Vec::with_capacity(DEFAULT_BATCH_FILES),
            pending_seen: Vec::with_capacity(256),
            pending_source_bytes: 0,
            pending_reserved_bytes: 0,
            eligible_files: 0,
            eligible_bytes: 0,
            changed: 0,
            unchanged: 0,
            files_discovered: 0,
            bytes_read: 0,
            source_file_open_count: 0,
            max_in_flight_bytes: 0,
            max_pipeline_buffered_bytes: 0,
            max_queued_artifacts: 0,
            max_pipeline_artifacts: 0,
            skipped: BTreeMap::new(),
            structural_complete: true,
            cancelled: false,
            stage_hook: None,
        }
    }
}

/// Files examined by the index run in progress (reset at the start of each run). Only for live
/// progress display; the authoritative numbers are in `IndexReport`.
static FILES_SCANNED: AtomicU64 = AtomicU64::new(0);

pub fn files_scanned() -> u64 {
    FILES_SCANNED.load(Ordering::Relaxed)
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
        None,
    )
}

/// Refresh one explicit repository-relative path without walking the repository.
/// The path is checked against current ignore/security rules. This updates a known file or
/// removes it when it is missing or no longer eligible, then marks the catalog partial until
/// `index_repository` performs full reconciliation.
pub fn refresh_indexed_file(
    db_path: &Path,
    root: &Path,
    relative_path: &str,
    cancellation: &AtomicBool,
) -> Result<FileRefreshReport> {
    let started = Instant::now();
    let scope = repo_scope(root)?;
    let requested_relative_path = safe_relative_path(relative_path)?;
    if Path::new(&requested_relative_path).components().count() > 128 {
        bail!("repository-relative path exceeds the 128 component index limit");
    }
    let mut relative_path = requested_relative_path.clone();
    let requested_relative = Path::new(&requested_relative_path);
    let _index_lock = acquire_index_lock(db_path, &scope)?;
    let mut conn = open_db(db_path)?;
    let status: Option<i64> = conn
        .query_row(
            "SELECT generation FROM repositories WHERE repo_id=?1 AND worktree_id=?2",
            params![scope.repo_id, scope.worktree_id],
            |row| row.get(0),
        )
        .optional()?;
    let Some(previous_generation) = status else {
        bail!("repository has no index yet; run index_repository before refreshing a file");
    };
    let generation = (previous_generation as u64).saturating_add(1);
    if cancellation.load(Ordering::Relaxed) {
        bail!("file refresh cancelled before reading the target");
    }

    let target = scope.root.join(requested_relative);
    let mut target_metadata = None;
    let eligibility_reason = if is_secret_path(requested_relative) {
        Some("credential_or_key")
    } else if is_vcs_metadata_path(requested_relative) {
        Some("version_control_metadata")
    } else if default_generated_path(requested_relative) {
        Some("generated_or_vendor_default")
    } else {
        match fs::symlink_metadata(&target) {
            Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
                if metadata.len() > MAX_FILE_BYTES {
                    Some("oversized")
                } else if has_symlink_component(&scope.root, requested_relative)? {
                    Some("missing_or_unsafe_file")
                } else {
                    match target.canonicalize() {
                        Ok(canonical) if canonical.starts_with(&scope.root) => {
                            let canonical_relative = canonical
                                .strip_prefix(&scope.root)?
                                .to_string_lossy()
                                .replace('\\', "/");
                            let canonical_path = Path::new(&canonical_relative);
                            if is_secret_path(canonical_path) {
                                Some("credential_or_key")
                            } else if is_vcs_metadata_path(canonical_path) {
                                Some("version_control_metadata")
                            } else if default_generated_path(canonical_path) {
                                Some("generated_or_vendor_default")
                            } else {
                                relative_path = canonical_relative;
                                target_metadata = Some(metadata);
                                match target_is_eligible(&scope.root, Path::new(&relative_path))? {
                                    Ok(true) => None,
                                    Ok(false) | Err(_) => {
                                        Some("ignored_or_unreadable_ignore_rules")
                                    }
                                }
                            }
                        }
                        _ => Some("missing_or_unsafe_file"),
                    }
                }
            }
            _ => Some("missing_or_unsafe_file"),
        }
    };

    let existing: Option<i64> = conn
        .query_row(
            "SELECT id FROM code_files WHERE repo_id=?1 AND worktree_id=?2 AND rel_path=?3",
            params![scope.repo_id, scope.worktree_id, relative_path],
            |row| row.get(0),
        )
        .optional()?;

    let prepared = if eligibility_reason.is_none() {
        let metadata = target_metadata.expect("eligible target metadata was recorded");
        if metadata.len() > MAX_FILE_BYTES {
            None
        } else {
            let job = IndexJob {
                sequence: 0,
                root: scope.root.clone(),
                rel_path: relative_path.clone(),
                size: metadata.len(),
                modified_ns: modified_ns(&metadata),
                previous_hash: None,
                previous_parse_error: false,
                is_new: existing.is_none(),
                language: language_for(&relative_path),
                verify_hashes: true,
                reserved_bytes: (metadata.len() as usize).saturating_mul(3),
            };
            let mut parser_cache = ParserCache::default();
            Some(prepare_file(job, cancellation, &mut parser_cache, &None))
        }
    } else {
        None
    };

    if cancellation.load(Ordering::Relaxed)
        || matches!(
            prepared.as_ref().map(|file| &file.kind),
            Some(PreparedKind::Cancelled)
        )
    {
        bail!("file refresh cancelled while reading the target");
    }

    let (operation, reason) = match prepared.as_ref().map(|file| &file.kind) {
        Some(PreparedKind::Changed(_)) => ("updated", None),
        Some(PreparedKind::Unchanged { .. }) => ("unchanged", None),
        Some(PreparedKind::Skipped(reason)) => ("removed", Some((*reason).to_string())),
        Some(PreparedKind::Cancelled) => unreachable!("cancellation handled above"),
        None if eligibility_reason.is_some() => (
            if existing.is_some() {
                "removed"
            } else {
                "skipped"
            },
            eligibility_reason.map(str::to_string),
        ),
        None => (
            if existing.is_some() {
                "removed"
            } else {
                "skipped"
            },
            Some("not_a_regular_indexable_file".to_string()),
        ),
    };

    let tx = conn.transaction()?;
    match prepared {
        Some(prepared) => match prepared.kind {
            PreparedKind::Changed(file) => refresh_changed_file(&tx, &scope, generation, file)?,
            PreparedKind::Unchanged { .. } => {
                tx.execute(
                    "UPDATE code_files SET size_bytes=?4,modified_ns=?5,indexed_generation=?6 WHERE repo_id=?1 AND worktree_id=?2 AND rel_path=?3",
                    params![
                        scope.repo_id,
                        scope.worktree_id,
                        relative_path,
                        prepared.size as i64,
                        prepared.modified_ns,
                        generation as i64
                    ],
                )?;
            }
            PreparedKind::Skipped(_) => {
                if let Some(id) = existing {
                    tx.execute("DELETE FROM code_fts WHERE rowid=?1", [id])?;
                    tx.execute("DELETE FROM code_files WHERE id=?1", [id])?;
                }
            }
            PreparedKind::Cancelled => unreachable!("cancellation handled above"),
        },
        None => {
            if let Some(id) = existing {
                tx.execute("DELETE FROM code_fts WHERE rowid=?1", [id])?;
                tx.execute("DELETE FROM code_files WHERE id=?1", [id])?;
            }
        }
    }
    let now = now_utc();
    tx.execute(
        "UPDATE repositories SET generation=?3,is_complete=0,structural_complete=0,
         last_index_started=?4,last_index_finished=?4 WHERE repo_id=?1 AND worktree_id=?2",
        params![scope.repo_id, scope.worktree_id, generation as i64, now],
    )?;
    tx.commit()?;

    Ok(FileRefreshReport {
        repo_id: scope.repo_id,
        worktree_id: scope.worktree_id,
        path: relative_path,
        operation: operation.to_string(),
        generation,
        catalog_complete: false,
        reconciliation_required: true,
        reason,
        elapsed_ms: started.elapsed().as_secs_f64() * 1000.0,
    })
}

fn target_is_eligible(root: &Path, relative: &Path) -> Result<std::result::Result<bool, String>> {
    let aetherignore = root.join(".aetherignore");
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
    if valid_aetherignore(&aetherignore, root)
        && let Some(error) = builder.add_ignore(&aetherignore)
    {
        return Ok(Err(error.to_string()));
    }
    let mut matchers = builder.build_matchers();
    let Some(mut matcher) = matchers.pop() else {
        bail!("failed to construct repository ignore matcher");
    };
    let (matched, error) = matcher.matched_with_errors(relative, false);
    if let Some(error) = error {
        return Ok(Err(error.to_string()));
    }
    Ok(Ok(!matched.is_ignore()))
}

fn has_symlink_component(root: &Path, relative: &Path) -> Result<bool> {
    let mut current = root.to_path_buf();
    for component in relative.components() {
        current.push(component.as_os_str());
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => return Ok(true),
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(error.into()),
        }
    }
    Ok(false)
}

fn refresh_changed_file(
    tx: &rusqlite::Transaction<'_>,
    scope: &RepoScope,
    generation: u64,
    file: PendingFile,
) -> Result<()> {
    let file_id: i64 = tx
        .prepare_cached(
            "INSERT INTO code_files(repo_id,worktree_id,rel_path,content_hash,language,size_bytes,modified_ns,indexed_generation,parse_error)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)
             ON CONFLICT(repo_id,worktree_id,rel_path) DO UPDATE SET content_hash=excluded.content_hash,
               language=excluded.language,size_bytes=excluded.size_bytes,modified_ns=excluded.modified_ns,
               indexed_generation=excluded.indexed_generation,parse_error=excluded.parse_error
             RETURNING id",
        )?
        .query_row(
            params![
                scope.repo_id,
                scope.worktree_id,
                file.rel_path,
                file.hash,
                file.language,
                file.size as i64,
                file.modified_ns,
                generation as i64,
                file.parse_error
            ],
            |row| row.get(0),
        )?;
    let mut searchable = file.text;
    if !file.identifier_terms.is_empty() {
        searchable.push('\n');
        searchable.push_str(&file.identifier_terms);
    }
    SqliteFtsIndex.replace(tx, file_id, &searchable)?;
    tx.execute("DELETE FROM code_symbols WHERE file_id=?1", [file_id])?;
    let mut symbol_stmt = tx.prepare_cached(
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
    Ok(())
}

#[cfg(test)]
pub(crate) fn index_repository_with_stage_hook(
    db_path: &Path,
    root: &Path,
    cancellation: &AtomicBool,
    stage_hook: IndexStageHook,
) -> Result<IndexReport> {
    let scope = repo_scope(root)?;
    index_scoped(
        db_path,
        &scope,
        false,
        false,
        cancellation,
        Some(stage_hook),
    )
}

fn index_scoped(
    db_path: &Path,
    scope: &RepoScope,
    include_generated: bool,
    verify_hashes: bool,
    cancellation: &AtomicBool,
    stage_hook: Option<IndexStageHook>,
) -> Result<IndexReport> {
    static NEXT_SAMPLE: AtomicU64 = AtomicU64::new(1);

    let started = Instant::now();
    let sample_id = format!(
        "{}-{}",
        std::process::id(),
        NEXT_SAMPLE.fetch_add(1, Ordering::Relaxed)
    );
    let _index_lock = acquire_index_lock(db_path, scope)?;
    FILES_SCANNED.store(0, Ordering::Relaxed);
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
    let mut builder = WalkBuilder::new(root);
    builder
        .hidden(false)
        .git_ignore(true)
        .git_exclude(true)
        .require_git(false)
        .parents(false)
        .current_dir(root)
        .follow_links(false)
        .max_depth(Some(128))
        .sort_by_file_path(|left, right| left.cmp(right));
    let aetherignore = root.join(".aetherignore");
    let mut skipped = BTreeMap::new();
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
    let excluded_generated = Arc::new(AtomicU64::new(0));
    let excluded_vcs = Arc::new(AtomicU64::new(0));
    let excluded_depth = Arc::new(AtomicU64::new(0));
    let excluded_generated_for_walk = excluded_generated.clone();
    let excluded_vcs_for_walk = excluded_vcs.clone();
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
            excluded_vcs_for_walk.fetch_add(1, Ordering::Relaxed);
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

    let worker_count = configured_worker_count();
    let (batch_file_limit, batch_byte_limit) = configured_batch_limits();
    let queue_capacity = worker_count.saturating_mul(2).max(2);
    let worker_cancel = Arc::new(AtomicBool::new(false));
    let (job_tx, job_rx) = mpsc::sync_channel::<IndexJob>(queue_capacity);
    let (result_tx, result_rx) = mpsc::sync_channel::<PreparedFile>(queue_capacity);
    let shared_job_rx = Arc::new(Mutex::new(job_rx));
    let mut timers = Timers::default();
    let mut writer = WriterState {
        batch_file_limit,
        batch_byte_limit,
        pending: Vec::with_capacity(batch_file_limit),
        skipped,
        stage_hook: stage_hook.clone(),
        ..WriterState::default()
    };
    let mut last_partial_publish = Instant::now();
    let mut outstanding_items = 0usize;
    let mut outstanding_bytes = 0usize;
    let mut next_sequence = 0usize;
    let mut next_to_write = 0usize;
    let mut completed = BTreeMap::<usize, PreparedFile>::new();
    let mut walk_finished = false;

    thread::scope(|thread_scope| -> Result<()> {
        let mut workers = Vec::with_capacity(worker_count);
        for _ in 0..worker_count {
            let jobs = shared_job_rx.clone();
            let results = result_tx.clone();
            let cancelled = worker_cancel.clone();
            let worker_stage_hook = stage_hook.clone();
            workers.push(thread_scope.spawn(move || {
                let mut parser_cache = ParserCache::default();
                loop {
                    let job = {
                        let receiver = jobs.lock().map_err(|_| "index job receiver poisoned")?;
                        receiver.recv()
                    };
                    let Ok(job) = job else { break };
                    let prepared =
                        prepare_file(job, &cancelled, &mut parser_cache, &worker_stage_hook);
                    if results.send(prepared).is_err() {
                        break;
                    }
                }
                Ok::<(), &'static str>(())
            }));
        }
        drop(result_tx);

        let coordinator_result = (|| -> Result<()> {
            let mut walker = builder.build();
            loop {
                if cancellation.load(Ordering::Relaxed) {
                    writer.cancelled = true;
                    worker_cancel.store(true, Ordering::Relaxed);
                    break;
                }

                drain_ready_prepared(
                    &result_rx,
                    &mut completed,
                    &mut next_to_write,
                    &mut outstanding_items,
                    &mut outstanding_bytes,
                    &mut writer,
                    &mut conn,
                    scope,
                    generation,
                    &mut timers,
                )?;

                if !walk_finished {
                    let discovery_start = Instant::now();
                    let entry = walker.next();
                    timers.discovery += discovery_start.elapsed().as_secs_f64() * 1000.0;
                    let Some(entry) = entry else {
                        walk_finished = true;
                        continue;
                    };
                    let entry = match entry {
                        Ok(entry) => entry,
                        Err(_) => {
                            bump(&mut writer.skipped, "walk_error");
                            continue;
                        }
                    };
                    notify_index_stage(&stage_hook, IndexStage::Discovery);
                    if cancellation.load(Ordering::Relaxed) {
                        writer.cancelled = true;
                        worker_cancel.store(true, Ordering::Relaxed);
                        break;
                    }
                    let path = entry.path();
                    if path == root || entry.file_type().is_some_and(|t| t.is_dir()) {
                        continue;
                    }
                    if entry.file_type().is_some_and(|t| t.is_symlink()) {
                        bump(&mut writer.skipped, "symlink");
                        continue;
                    }
                    let relative_path = match path.strip_prefix(root) {
                        Ok(path) => path,
                        Err(_) => {
                            bump(&mut writer.skipped, "outside_root");
                            continue;
                        }
                    };
                    if is_secret_path(relative_path) {
                        bump(&mut writer.skipped, "credential_or_key");
                        continue;
                    }
                    if !include_generated && default_generated_path(relative_path) {
                        bump(&mut writer.skipped, "generated_or_vendor_default");
                        continue;
                    }
                    if !entry
                        .file_type()
                        .is_some_and(|file_type| file_type.is_file())
                    {
                        continue;
                    }

                    let metadata_start = Instant::now();
                    let metadata = match fs::symlink_metadata(path) {
                        Ok(metadata)
                            if metadata.is_file() && !metadata.file_type().is_symlink() =>
                        {
                            metadata
                        }
                        _ => {
                            bump(&mut writer.skipped, "symlink_or_stat_error");
                            timers.metadata += metadata_start.elapsed().as_secs_f64() * 1000.0;
                            continue;
                        }
                    };
                    let size = metadata.len();
                    if size > MAX_FILE_BYTES {
                        bump(&mut writer.skipped, "oversized");
                        timers.metadata += metadata_start.elapsed().as_secs_f64() * 1000.0;
                        continue;
                    }
                    let relative = relative_path.to_string_lossy().replace('\\', "/");
                    let modified_ns = modified_ns(&metadata);
                    let existing: Option<(String, i64, i64, bool)> = conn
                        .prepare_cached(
                            "SELECT content_hash,size_bytes,modified_ns,parse_error FROM code_files
                             WHERE repo_id=?1 AND worktree_id=?2 AND rel_path=?3",
                        )?
                        .query_row(params![scope.repo_id, scope.worktree_id, relative], |row| {
                            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
                        })
                        .optional()?;
                    writer.files_discovered += 1;
                    FILES_SCANNED.fetch_add(1, Ordering::Relaxed);

                    if !verify_hashes
                        && existing
                            .as_ref()
                            .is_some_and(|(_, previous_size, previous_mtime, _)| {
                                *previous_size == size as i64 && *previous_mtime == modified_ns
                            })
                    {
                        let parse_error = existing
                            .as_ref()
                            .is_some_and(|(_, _, _, parse_error)| *parse_error);
                        record_seen_file(&mut writer, relative, size, modified_ns, parse_error);
                        if parse_error {
                            bump(&mut writer.skipped, "tree_sitter_parse_error");
                            writer.structural_complete = false;
                        }
                        timers.metadata += metadata_start.elapsed().as_secs_f64() * 1000.0;
                        if writer.pending_seen.len() >= 256 {
                            flush_writer_batch(
                                &mut conn,
                                scope,
                                generation,
                                &mut writer,
                                &mut timers,
                            )?;
                        }
                        continue;
                    }
                    if !matches!(path.canonicalize(), Ok(actual) if actual.starts_with(root)) {
                        bump(&mut writer.skipped, "outside_root_or_raced_symlink");
                        timers.metadata += metadata_start.elapsed().as_secs_f64() * 1000.0;
                        continue;
                    }
                    timers.metadata += metadata_start.elapsed().as_secs_f64() * 1000.0;

                    let reservation = (size as usize)
                        .saturating_mul(3)
                        .min(MAX_FILE_BYTES as usize * 3);
                    while outstanding_items >= queue_capacity
                        || outstanding_bytes.saturating_add(reservation) > MAX_PIPELINE_BYTES
                        || outstanding_bytes
                            .saturating_add(writer.pending_reserved_bytes)
                            .saturating_add(reservation)
                            > MAX_PIPELINE_BYTES + batch_byte_limit
                    {
                        notify_index_stage(&stage_hook, IndexStage::QueueBackpressure);
                        if cancellation.load(Ordering::Relaxed) {
                            writer.cancelled = true;
                            worker_cancel.store(true, Ordering::Relaxed);
                            break;
                        }
                        if writer.pending_source_bytes > 0
                            && outstanding_items == 0
                            && completed.is_empty()
                        {
                            flush_writer_batch(
                                &mut conn,
                                scope,
                                generation,
                                &mut writer,
                                &mut timers,
                            )?;
                            continue;
                        }
                        receive_one_prepared(
                            &result_rx,
                            &mut completed,
                            &mut next_to_write,
                            &mut outstanding_items,
                            &mut outstanding_bytes,
                            &mut writer,
                            &mut conn,
                            scope,
                            generation,
                            &mut timers,
                        )?;
                    }
                    if writer.cancelled {
                        break;
                    }
                    if cancellation.load(Ordering::Relaxed) {
                        writer.cancelled = true;
                        worker_cancel.store(true, Ordering::Relaxed);
                        break;
                    }

                    let language = language_for(&relative);
                    let job = IndexJob {
                        sequence: next_sequence,
                        root: root.to_path_buf(),
                        rel_path: relative,
                        size,
                        modified_ns,
                        previous_hash: existing.as_ref().map(|row| row.0.clone()),
                        previous_parse_error: existing.as_ref().is_some_and(|row| row.3),
                        is_new: existing.is_none(),
                        language,
                        verify_hashes,
                        reserved_bytes: reservation,
                    };
                    let send_started = Instant::now();
                    job_tx
                        .send(job)
                        .map_err(|_| anyhow::anyhow!("index worker pool stopped unexpectedly"))?;
                    timers.worker_wait += send_started.elapsed().as_secs_f64() * 1000.0;
                    next_sequence += 1;
                    outstanding_items += 1;
                    outstanding_bytes = outstanding_bytes.saturating_add(reservation);
                    writer.max_in_flight_bytes =
                        writer.max_in_flight_bytes.max(outstanding_bytes as u64);
                    writer.max_pipeline_buffered_bytes = writer
                        .max_pipeline_buffered_bytes
                        .max((outstanding_bytes + writer.pending_reserved_bytes) as u64);
                    writer.max_queued_artifacts =
                        writer.max_queued_artifacts.max(outstanding_items as u64);
                    writer.max_pipeline_artifacts = writer
                        .max_pipeline_artifacts
                        .max((outstanding_items + writer.pending.len()) as u64);
                }

                if last_partial_publish.elapsed().as_secs_f64() >= 0.15 {
                    flush_writer_batch(&mut conn, scope, generation, &mut writer, &mut timers)?;
                    publish_progress(
                        &conn,
                        scope,
                        writer.eligible_files,
                        writer.eligible_bytes,
                        &writer.skipped,
                    )?;
                    last_partial_publish = Instant::now();
                }

                if walk_finished && outstanding_items == 0 {
                    break;
                }
                if walk_finished || outstanding_items >= queue_capacity {
                    receive_one_prepared(
                        &result_rx,
                        &mut completed,
                        &mut next_to_write,
                        &mut outstanding_items,
                        &mut outstanding_bytes,
                        &mut writer,
                        &mut conn,
                        scope,
                        generation,
                        &mut timers,
                    )?;
                }
            }

            if cancellation.load(Ordering::Relaxed) {
                writer.cancelled = true;
                worker_cancel.store(true, Ordering::Relaxed);
            }
            Ok(())
        })();

        if coordinator_result.is_err() || writer.cancelled {
            worker_cancel.store(true, Ordering::Relaxed);
        }
        drop(job_tx);

        if coordinator_result.is_ok() && !writer.cancelled {
            while outstanding_items > 0 {
                receive_one_prepared(
                    &result_rx,
                    &mut completed,
                    &mut next_to_write,
                    &mut outstanding_items,
                    &mut outstanding_bytes,
                    &mut writer,
                    &mut conn,
                    scope,
                    generation,
                    &mut timers,
                )?;
            }
        } else {
            for (_, prepared) in std::mem::take(&mut completed) {
                outstanding_items = outstanding_items.saturating_sub(1);
                outstanding_bytes = outstanding_bytes.saturating_sub(prepared.reserved_bytes);
            }
            while outstanding_items > 0 {
                let wait_started = Instant::now();
                match result_rx.recv_timeout(Duration::from_millis(50)) {
                    Ok(prepared) => {
                        outstanding_items = outstanding_items.saturating_sub(1);
                        outstanding_bytes =
                            outstanding_bytes.saturating_sub(prepared.reserved_bytes);
                        timers.worker_wait += wait_started.elapsed().as_secs_f64() * 1000.0;
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        timers.worker_wait += wait_started.elapsed().as_secs_f64() * 1000.0;
                        if cancellation.load(Ordering::Relaxed) {
                            worker_cancel.store(true, Ordering::Relaxed);
                        }
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => {
                        anyhow::bail!("all index workers stopped before draining queued files")
                    }
                }
            }
        }

        for worker in workers {
            worker
                .join()
                .map_err(|_| anyhow::anyhow!("index worker panicked"))?
                .map_err(|message| anyhow::anyhow!(message))?;
        }
        coordinator_result
    })?;

    let excluded_count = excluded_generated.load(Ordering::Relaxed);
    if excluded_count > 0 {
        *writer
            .skipped
            .entry("generated_or_vendor_default".to_string())
            .or_default() += excluded_count;
    }
    let depth_count = excluded_depth.load(Ordering::Relaxed);
    if depth_count > 0 {
        *writer
            .skipped
            .entry("depth_limit_128".to_string())
            .or_default() += depth_count;
    }
    let vcs_count = excluded_vcs.load(Ordering::Relaxed);
    if vcs_count > 0 {
        *writer
            .skipped
            .entry("version_control_metadata".to_string())
            .or_default() += vcs_count;
    }
    if ignore_error.is_some() {
        bump(&mut writer.skipped, "aetherignore_parse_error");
    }

    flush_writer_batch(&mut conn, scope, generation, &mut writer, &mut timers)?;
    let was_cancelled = writer.cancelled || cancellation.load(Ordering::Relaxed);
    if was_cancelled {
        let skipped_json = serde_json::to_string(&writer.skipped)?;
        conn.execute(
            "UPDATE repositories SET eligible_files=?3,eligible_bytes=?4,skipped_json=?5,
             is_complete=0,structural_complete=0
             WHERE repo_id=?1 AND worktree_id=?2",
            params![
                scope.repo_id,
                scope.worktree_id,
                writer.eligible_files as i64,
                writer.eligible_bytes as i64,
                skipped_json
            ],
        )?;
        let total = started.elapsed().as_secs_f64() * 1000.0;
        return Ok(report(
            scope,
            generation,
            false,
            false,
            true,
            false,
            writer,
            timers,
            total,
            sample_id,
            worker_count,
            batch_file_limit,
            batch_byte_limit,
        ));
    }

    let reconciliation_start = Instant::now();
    let commit_before_reconciliation = timers.commit;
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
        let commit_start = Instant::now();
        tx.commit()?;
        timers.commit += commit_start.elapsed().as_secs_f64() * 1000.0;
    }
    let skipped_json = serde_json::to_string(&writer.skipped)?;
    conn.execute(
        "UPDATE repositories SET eligible_files=?3,eligible_bytes=?4,skipped_json=?5,is_complete=1,
         structural_complete=?6,last_index_finished=?7
         WHERE repo_id=?1 AND worktree_id=?2",
        params![
            scope.repo_id,
            scope.worktree_id,
            writer.eligible_files as i64,
            writer.eligible_bytes as i64,
            skipped_json,
            writer.structural_complete,
            now_utc()
        ],
    )?;
    timers.reconciliation += (reconciliation_start.elapsed().as_secs_f64() * 1000.0)
        - (timers.commit - commit_before_reconciliation);
    let total = started.elapsed().as_secs_f64() * 1000.0;
    Ok(report(
        scope,
        generation,
        true,
        true,
        false,
        writer.structural_complete,
        writer,
        timers,
        total,
        sample_id,
        worker_count,
        batch_file_limit,
        batch_byte_limit,
    ))
}

fn prepare_file(
    job: IndexJob,
    cancellation: &AtomicBool,
    parser_cache: &mut ParserCache,
    stage_hook: &Option<IndexStageHook>,
) -> PreparedFile {
    let mut work = WorkerWork::default();
    let sequence = job.sequence;
    let rel_path = job.rel_path.clone();
    let size = job.size;
    let modified_ns = job.modified_ns;
    let reserved_bytes = job.reserved_bytes;
    macro_rules! prepared {
        ($kind:expr) => {
            PreparedFile {
                sequence,
                rel_path: rel_path.clone(),
                size,
                modified_ns,
                reserved_bytes,
                work: std::mem::take(&mut work),
                kind: $kind,
            }
        };
    }
    if cancellation.load(Ordering::Relaxed) {
        return prepared!(PreparedKind::Cancelled);
    }
    work.source_file_open_count = 1;
    let read_started = Instant::now();
    let read_result = read_limited_from_root(
        &job.root,
        Path::new(&job.rel_path),
        MAX_FILE_BYTES as usize + 1,
        Some(cancellation),
        stage_hook,
    );
    work.read_ms = read_started.elapsed().as_secs_f64() * 1000.0;
    let bytes = match read_result {
        Ok(bytes) => bytes,
        Err(_) if cancellation.load(Ordering::Relaxed) => {
            return prepared!(PreparedKind::Cancelled);
        }
        Err(_) => return prepared!(PreparedKind::Skipped("read_error_or_raced_symlink")),
    };
    if cancellation.load(Ordering::Relaxed) {
        return prepared!(PreparedKind::Cancelled);
    }
    work.bytes_read = bytes.len() as u64;
    if bytes.len() > MAX_FILE_BYTES as usize {
        return prepared!(PreparedKind::Skipped("oversized_after_read"));
    }
    if bytes.contains(&0) {
        return prepared!(PreparedKind::Skipped("binary_nul"));
    }
    if has_binary_controls(&bytes) {
        return prepared!(PreparedKind::Skipped("binary_control_bytes"));
    }
    let text = match String::from_utf8(bytes) {
        Ok(text) => text,
        Err(_) => return prepared!(PreparedKind::Skipped("non_utf8")),
    };
    let hash_started = Instant::now();
    let hash = hash_bytes(text.as_bytes());
    work.hash_ms = hash_started.elapsed().as_secs_f64() * 1000.0;
    if !job.verify_hashes
        && job
            .previous_hash
            .as_ref()
            .is_some_and(|previous| previous == &hash)
    {
        return prepared!(PreparedKind::Unchanged {
            parse_error: job.previous_parse_error,
        });
    }

    let identifier_started = Instant::now();
    let identifier_terms = identifier_search_terms(&text);
    work.identifier_extraction_ms = identifier_started.elapsed().as_secs_f64() * 1000.0;
    if cancellation.load(Ordering::Relaxed) {
        return prepared!(PreparedKind::Cancelled);
    }
    let parse_started = Instant::now();
    let extracted = parser_cache.extract(job.language.as_deref(), &text, cancellation);
    work.parse_ms = parse_started.elapsed().as_secs_f64() * 1000.0;
    notify_index_stage(stage_hook, IndexStage::Parsing);
    if cancellation.load(Ordering::Relaxed) {
        return prepared!(PreparedKind::Cancelled);
    }
    let (symbols, parse_error) = extracted.unwrap_or_default();
    let pending = PendingFile {
        rel_path: job.rel_path,
        hash,
        language: job.language.unwrap_or_else(|| "text".to_string()),
        size: job.size,
        modified_ns: job.modified_ns,
        text,
        identifier_terms,
        symbols,
        parse_error,
        is_new: job.is_new,
    };
    prepared!(PreparedKind::Changed(pending))
}

fn notify_index_stage(hook: &Option<IndexStageHook>, stage: IndexStage) {
    if let Some(hook) = hook {
        hook(stage);
    }
}

fn record_seen_file(
    writer: &mut WriterState,
    rel_path: String,
    size: u64,
    modified_ns: i64,
    parse_error: bool,
) {
    writer.pending_seen.push(SeenFile {
        rel_path,
        size,
        modified_ns,
        parse_error,
    });
    writer.eligible_files += 1;
    writer.eligible_bytes += size;
    writer.unchanged += 1;
}

#[allow(clippy::too_many_arguments)] // Queue accounting and writer state must stay synchronized.
fn receive_one_prepared(
    receiver: &mpsc::Receiver<PreparedFile>,
    completed: &mut BTreeMap<usize, PreparedFile>,
    next_to_write: &mut usize,
    outstanding_items: &mut usize,
    outstanding_bytes: &mut usize,
    writer: &mut WriterState,
    conn: &mut Connection,
    scope: &RepoScope,
    generation: u64,
    timers: &mut Timers,
) -> Result<()> {
    let wait_started = Instant::now();
    let prepared = match receiver.recv_timeout(Duration::from_millis(50)) {
        Ok(prepared) => prepared,
        Err(mpsc::RecvTimeoutError::Timeout) => {
            timers.worker_wait += wait_started.elapsed().as_secs_f64() * 1000.0;
            return Ok(());
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            bail!("all index workers stopped before completing queued files")
        }
    };
    timers.worker_wait += wait_started.elapsed().as_secs_f64() * 1000.0;
    completed.insert(prepared.sequence, prepared);
    apply_ordered_prepared(
        completed,
        next_to_write,
        outstanding_items,
        outstanding_bytes,
        writer,
        conn,
        scope,
        generation,
        timers,
    )
}

#[allow(clippy::too_many_arguments)] // Drains into the single writer in deterministic order.
fn drain_ready_prepared(
    receiver: &mpsc::Receiver<PreparedFile>,
    completed: &mut BTreeMap<usize, PreparedFile>,
    next_to_write: &mut usize,
    outstanding_items: &mut usize,
    outstanding_bytes: &mut usize,
    writer: &mut WriterState,
    conn: &mut Connection,
    scope: &RepoScope,
    generation: u64,
    timers: &mut Timers,
) -> Result<()> {
    while let Ok(prepared) = receiver.try_recv() {
        completed.insert(prepared.sequence, prepared);
    }
    apply_ordered_prepared(
        completed,
        next_to_write,
        outstanding_items,
        outstanding_bytes,
        writer,
        conn,
        scope,
        generation,
        timers,
    )
}

#[allow(clippy::too_many_arguments)] // The writer state and connection are intentionally explicit.
fn apply_ordered_prepared(
    completed: &mut BTreeMap<usize, PreparedFile>,
    next_to_write: &mut usize,
    outstanding_items: &mut usize,
    outstanding_bytes: &mut usize,
    writer: &mut WriterState,
    conn: &mut Connection,
    scope: &RepoScope,
    generation: u64,
    timers: &mut Timers,
) -> Result<()> {
    while let Some(prepared) = completed.remove(next_to_write) {
        *outstanding_items = outstanding_items.saturating_sub(1);
        *outstanding_bytes = outstanding_bytes.saturating_sub(prepared.reserved_bytes);
        timers.read += prepared.work.read_ms;
        timers.hashing += prepared.work.hash_ms;
        timers.identifier_extraction += prepared.work.identifier_extraction_ms;
        timers.parsing += prepared.work.parse_ms;
        writer.bytes_read += prepared.work.bytes_read;
        writer.source_file_open_count += prepared.work.source_file_open_count;
        match prepared.kind {
            PreparedKind::Unchanged { parse_error } => {
                record_seen_file(
                    writer,
                    prepared.rel_path,
                    prepared.size,
                    prepared.modified_ns,
                    parse_error,
                );
                if parse_error {
                    writer.structural_complete = false;
                    bump(&mut writer.skipped, "tree_sitter_parse_error");
                }
            }
            PreparedKind::Changed(file) => {
                writer.eligible_files += 1;
                writer.eligible_bytes += file.size;
                writer.changed += 1;
                if file.parse_error {
                    writer.structural_complete = false;
                    bump(&mut writer.skipped, "tree_sitter_parse_error");
                }
                writer.pending_source_bytes =
                    writer.pending_source_bytes.saturating_add(file.text.len());
                writer.pending_reserved_bytes = writer
                    .pending_reserved_bytes
                    .saturating_add(file.text.len())
                    .saturating_add(file.identifier_terms.len())
                    .saturating_add(file.symbols.len().saturating_mul(128));
                writer.pending.push(file);
            }
            PreparedKind::Skipped(reason) => bump(&mut writer.skipped, reason),
            PreparedKind::Cancelled => writer.cancelled = true,
        }
        *next_to_write += 1;
        writer.max_in_flight_bytes = writer.max_in_flight_bytes.max(*outstanding_bytes as u64);
        writer.max_pipeline_buffered_bytes = writer
            .max_pipeline_buffered_bytes
            .max((*outstanding_bytes + writer.pending_reserved_bytes) as u64);
        writer.max_queued_artifacts = writer.max_queued_artifacts.max(*outstanding_items as u64);
        writer.max_pipeline_artifacts = writer
            .max_pipeline_artifacts
            .max((*outstanding_items + writer.pending.len()) as u64);
        if writer.pending.len() >= writer.batch_file_limit
            || writer.pending_source_bytes >= writer.batch_byte_limit
            || writer.pending_seen.len() >= 256
        {
            flush_writer_batch(conn, scope, generation, writer, timers)?;
        }
    }
    Ok(())
}

fn flush_writer_batch(
    conn: &mut Connection,
    scope: &RepoScope,
    generation: u64,
    writer: &mut WriterState,
    timers: &mut Timers,
) -> Result<()> {
    if writer.pending.is_empty() && writer.pending_seen.is_empty() {
        return Ok(());
    }
    flush_batch(
        conn,
        &scope.repo_id,
        &scope.worktree_id,
        generation,
        &mut writer.pending,
        &mut writer.pending_seen,
        timers,
        &writer.stage_hook,
    )?;
    writer.pending_source_bytes = 0;
    writer.pending_reserved_bytes = 0;
    Ok(())
}

fn configured_worker_count() -> usize {
    std::env::var("MIDNIGHT_INDEX_WORKERS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or_else(|| {
            std::thread::available_parallelism()
                .map(usize::from)
                .unwrap_or(1)
        })
        .clamp(1, MAX_INDEX_WORKERS)
}

fn configured_batch_limits() -> (usize, usize) {
    let parse = |name: &str, default: usize, maximum: usize| {
        std::env::var(name)
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .filter(|value| *value > 0)
            .map(|value| value.min(maximum))
            .unwrap_or(default)
    };
    (
        parse("MIDNIGHT_BENCH_BATCH_FILES", DEFAULT_BATCH_FILES, 2048),
        parse(
            "MIDNIGHT_BENCH_BATCH_BYTES",
            DEFAULT_BATCH_BYTES,
            DEFAULT_BATCH_BYTES,
        ),
    )
}

struct IndexLock(File);

impl Drop for IndexLock {
    fn drop(&mut self) {
        // Closing alone leaves flock held by descriptors inherited by a concurrent fork.
        // Unlock the shared open-file description when the protected operation finishes.
        let _ = FileExt::unlock(&self.0);
    }
}

fn acquire_index_lock(db_path: &Path, scope: &RepoScope) -> Result<IndexLock> {
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
    Ok(IndexLock(file))
}

#[allow(clippy::too_many_arguments)] // Keeps this internal report construction local and explicit.
fn report(
    scope: &RepoScope,
    generation: u64,
    complete: bool,
    catalog_complete: bool,
    cancelled: bool,
    structural_complete: bool,
    writer: WriterState,
    timers: Timers,
    total_ms: f64,
    sample_id: String,
    worker_count: usize,
    batch_file_limit: usize,
    batch_byte_limit: usize,
) -> IndexReport {
    let measured_main = timers.discovery
        + timers.metadata
        + timers.worker_wait
        + timers.index_writing
        + timers.commit
        + timers.reconciliation;
    let overhead = total_ms - measured_main;
    let mut timings_ms = BTreeMap::new();
    timings_ms.insert("discovery".into(), timers.discovery);
    timings_ms.insert("metadata_lookup".into(), timers.metadata);
    timings_ms.insert("read".into(), timers.read);
    timings_ms.insert("hashing".into(), timers.hashing);
    timings_ms.insert("identifier_extraction".into(), timers.identifier_extraction);
    timings_ms.insert("parsing".into(), timers.parsing);
    timings_ms.insert("index_writing".into(), timers.index_writing);
    timings_ms.insert("fts_payload_assembly".into(), timers.fts_payload_assembly);
    timings_ms.insert("fts_statement_execution".into(), timers.fts_execution);
    timings_ms.insert("symbol_writes".into(), timers.symbol_writes);
    timings_ms.insert("metadata_writes".into(), timers.metadata_writes);
    timings_ms.insert("transaction_commit".into(), timers.commit);
    timings_ms.insert("reconciliation".into(), timers.reconciliation);
    timings_ms.insert("worker_wait_and_backpressure".into(), timers.worker_wait);
    timings_ms.insert("residual_overhead".into(), overhead);
    timings_ms.insert("phase_accounted_total".into(), measured_main + overhead);
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
        eligible_files: writer.eligible_files,
        eligible_bytes: writer.eligible_bytes,
        indexed_or_updated: writer.changed,
        unchanged: writer.unchanged,
        skipped: writer.skipped,
        timings_ms,
        sample_id,
        worker_count,
        batch_file_limit,
        batch_byte_limit,
        files_discovered: writer.files_discovered,
        bytes_read: writer.bytes_read,
        source_file_open_count: writer.source_file_open_count,
        max_in_flight_bytes: writer.max_in_flight_bytes,
        max_pipeline_buffered_bytes: writer.max_pipeline_buffered_bytes,
        max_queued_artifacts: writer.max_queued_artifacts,
        max_pipeline_artifacts: writer.max_pipeline_artifacts,
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

#[allow(clippy::too_many_arguments)] // Keeps the single-writer transaction inputs explicit.
fn flush_batch(
    conn: &mut Connection,
    repo_id: &str,
    worktree_id: &str,
    generation: u64,
    pending: &mut Vec<PendingFile>,
    pending_seen: &mut Vec<SeenFile>,
    timers: &mut Timers,
    stage_hook: &Option<IndexStageHook>,
) -> Result<()> {
    if pending.is_empty() && pending_seen.is_empty() {
        return Ok(());
    }
    let write_start = Instant::now();
    let tx = conn.transaction()?;
    let metadata_start = Instant::now();
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
    timers.metadata_writes += metadata_start.elapsed().as_secs_f64() * 1000.0;
    let mut notified_batch_write = false;
    let mut file_stmt = tx.prepare_cached(
        "INSERT INTO code_files(repo_id,worktree_id,rel_path,content_hash,language,size_bytes,modified_ns,indexed_generation,parse_error)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)
         ON CONFLICT(repo_id,worktree_id,rel_path) DO UPDATE SET content_hash=excluded.content_hash,
           language=excluded.language,size_bytes=excluded.size_bytes,modified_ns=excluded.modified_ns,
           indexed_generation=excluded.indexed_generation,parse_error=excluded.parse_error
         RETURNING id",
    )?;
    let mut symbol_stmt = tx.prepare_cached(
        "INSERT INTO code_symbols(file_id,name,kind,line_start,line_end,byte_start,byte_end)
         VALUES(?1,?2,?3,?4,?5,?6,?7)",
    )?;
    for file in pending.drain(..) {
        let metadata_start = Instant::now();
        let file_id = file_stmt.query_row(
            params![
                repo_id,
                worktree_id,
                file.rel_path,
                file.hash,
                file.language,
                file.size as i64,
                file.modified_ns,
                generation as i64,
                file.parse_error
            ],
            |row| row.get::<_, i64>(0),
        )?;
        timers.metadata_writes += metadata_start.elapsed().as_secs_f64() * 1000.0;
        let payload_started = Instant::now();
        let mut searchable = file.text.clone();
        if !file.identifier_terms.is_empty() {
            searchable.push('\n');
            searchable.push_str(&file.identifier_terms);
        }
        timers.fts_payload_assembly += payload_started.elapsed().as_secs_f64() * 1000.0;
        let fts_started = Instant::now();
        if file.is_new {
            SqliteFtsIndex.insert(&tx, file_id, &searchable)?;
        } else {
            SqliteFtsIndex.replace(&tx, file_id, &searchable)?;
        }
        timers.fts_execution += fts_started.elapsed().as_secs_f64() * 1000.0;
        if !notified_batch_write {
            notify_index_stage(stage_hook, IndexStage::SqliteBatchWrite);
            notified_batch_write = true;
        }
        let symbol_started = Instant::now();
        if !file.is_new {
            tx.prepare_cached("DELETE FROM code_symbols WHERE file_id=?1")?
                .execute([file_id])?;
        }
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
        timers.symbol_writes += symbol_started.elapsed().as_secs_f64() * 1000.0;
    }
    drop(symbol_stmt);
    drop(file_stmt);
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
            parse_error_files: 0,
            parse_error_paths: Vec::new(),
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
    let parse_error_files: u64 = conn.query_row(
        "SELECT count(*) FROM code_files WHERE repo_id=?1 AND worktree_id=?2 AND parse_error=1",
        params![scope.repo_id, scope.worktree_id],
        |row| row.get::<_, i64>(0).map(|n| n as u64),
    )?;
    let parse_error_paths = conn
        .prepare(
            "SELECT rel_path FROM code_files WHERE repo_id=?1 AND worktree_id=?2 AND parse_error=1
             ORDER BY rel_path LIMIT ?3",
        )?
        .query_map(
            params![
                scope.repo_id,
                scope.worktree_id,
                MAX_PARSE_ERROR_PATHS as i64
            ],
            |row| row.get::<_, String>(0),
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(IndexStatus {
        repo_id: scope.repo_id,
        worktree_id: scope.worktree_id,
        root,
        branch,
        revision,
        generation: generation as u64,
        complete: catalog_complete != 0,
        catalog_complete: catalog_complete != 0,
        structural_complete: structural_complete != 0,
        parse_error_files,
        parse_error_paths,
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
        .map(|(g, catalog, _structural)| (g as u64, catalog != 0))
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
        .map(|term| {
            if !identifier_decomposition_enabled() {
                return format!("\"{}\"", term.replace('"', "\"\""));
            }
            let parts = split_identifier_parts(term);
            if parts.len() < 2 {
                return format!("\"{}\"", term.replace('"', "\"\""));
            }
            let pieces = parts
                .iter()
                .enumerate()
                .map(|(index, part)| {
                    let escaped = part.replace('"', "\"\"");
                    if index + 1 == parts.len() {
                        format!("\"{escaped}\"*")
                    } else {
                        format!("\"{escaped}\"")
                    }
                })
                .collect::<Vec<_>>();
            format!("({})", pieces.join(" AND "))
        })
        .collect::<Vec<_>>();
    (!terms.is_empty()).then(|| terms.join(" OR "))
}

fn identifier_search_terms(text: &str) -> String {
    if !identifier_decomposition_enabled() {
        return String::new();
    }
    let mut out = String::new();
    let bytes = text.as_bytes();
    let mut start = 0usize;
    let mut in_word = false;
    for index in 0..=bytes.len() {
        let byte = bytes.get(index).copied().unwrap_or(b' ');
        if byte.is_ascii_alphanumeric() || byte == b'_' {
            if !in_word {
                start = index;
                in_word = true;
            }
        } else if in_word {
            append_ascii_identifier_parts(&mut out, &text[start..index]);
            in_word = false;
        }
    }
    out
}

fn identifier_decomposition_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| {
        std::env::var("MIDNIGHT_IDENTIFIER_DECOMPOSITION")
            .map(|value| !matches!(value.as_str(), "0" | "false" | "off"))
            .unwrap_or(true)
    })
}

fn append_ascii_identifier_parts(out: &mut String, word: &str) {
    if !word.bytes().any(|byte| byte.is_ascii_uppercase()) {
        return;
    }
    let bytes = word.as_bytes();
    let mut start = 0usize;
    for index in 1..bytes.len() {
        let previous = bytes[index - 1];
        let current = bytes[index];
        let next = bytes.get(index + 1).copied();
        let digit_boundary = previous.is_ascii_digit() != current.is_ascii_digit();
        let lower_to_upper = previous.is_ascii_lowercase() && current.is_ascii_uppercase();
        let acronym_boundary = previous.is_ascii_uppercase()
            && current.is_ascii_uppercase()
            && next.is_some_and(|byte| byte.is_ascii_lowercase());
        if current == b'_' || digit_boundary || lower_to_upper || acronym_boundary {
            if start < index {
                out.push_str(&word[start..index]);
                out.push(' ');
            }
            start = if current == b'_' { index + 1 } else { index };
        }
    }
    if start < bytes.len() {
        out.push_str(&word[start..]);
        out.push(' ');
    }
}

fn split_identifier_parts(word: &str) -> Vec<&str> {
    if word.is_ascii() {
        return split_ascii_identifier_parts(word);
    }
    let chars = word.char_indices().collect::<Vec<_>>();
    let mut parts = Vec::new();
    let mut start = None;
    for (index, (byte, ch)) in chars.iter().enumerate() {
        if !ch.is_alphanumeric() {
            if let Some(part_start) = start.take() {
                parts.push(&word[part_start..*byte]);
            }
            continue;
        }
        if let Some(part_start) = start {
            let previous = chars[index - 1].1;
            let next = chars.get(index + 1).map(|(_, ch)| *ch);
            let digit_boundary = previous.is_ascii_digit() != ch.is_ascii_digit();
            let lower_to_upper = previous.is_lowercase() && ch.is_uppercase();
            let acronym_boundary = previous.is_uppercase()
                && ch.is_uppercase()
                && next.is_some_and(char::is_lowercase);
            if digit_boundary || lower_to_upper || acronym_boundary {
                parts.push(&word[part_start..*byte]);
                start = Some(*byte);
            }
        } else {
            start = Some(*byte);
        }
    }
    if let Some(part_start) = start {
        parts.push(&word[part_start..]);
    }
    parts
}

fn split_ascii_identifier_parts(word: &str) -> Vec<&str> {
    let bytes = word.as_bytes();
    let mut parts = Vec::new();
    let mut start = 0usize;
    for index in 0..bytes.len() {
        let current = bytes[index];
        if current == b'_' {
            if start < index {
                parts.push(&word[start..index]);
            }
            start = index + 1;
            continue;
        }
        if index == 0 {
            continue;
        }
        let previous = bytes[index - 1];
        let next = bytes.get(index + 1).copied();
        let digit_boundary = previous.is_ascii_digit() != current.is_ascii_digit();
        let lower_to_upper = previous.is_ascii_lowercase() && current.is_ascii_uppercase();
        let acronym_boundary = previous.is_ascii_uppercase()
            && current.is_ascii_uppercase()
            && next.is_some_and(|byte| byte.is_ascii_lowercase());
        if digit_boundary || lower_to_upper || acronym_boundary {
            if start < index {
                parts.push(&word[start..index]);
            }
            start = index;
        }
    }
    if start < bytes.len() {
        parts.push(&word[start..]);
    }
    parts
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
        .map(|r| (r.2 as u64, r.3 != 0))
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

pub(crate) fn current_text(root: &Path, path: &Path) -> Result<(String, String)> {
    let canonical = path.canonicalize()?;
    if !canonical.starts_with(root) {
        bail!("path escapes repository root");
    }
    let metadata = fs::symlink_metadata(&canonical)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > MAX_FILE_BYTES {
        bail!("source is a symlink, not a file, or exceeds the 1 MiB read limit");
    }
    let relative = canonical.strip_prefix(root)?;
    let bytes = read_limited_from_root(root, relative, MAX_FILE_BYTES as usize + 1, None, &None)?;
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
    let normalized = input.replace('\\', "/");
    let path = Path::new(&normalized);
    if path.is_absolute()
        || path
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
    {
        bail!("path must be repository-relative and cannot contain traversal components");
    }
    Ok(path.to_string_lossy().into_owned())
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

pub(crate) fn bounded_snippet(mut snippet: String) -> (String, bool) {
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

pub(crate) fn pack_context(
    results: Vec<CodeResult>,
    token_budget: usize,
) -> (Vec<CodeResult>, usize, bool) {
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

fn read_limited_from_root(
    root: &Path,
    relative: &Path,
    limit: usize,
    cancellation: Option<&AtomicBool>,
    stage_hook: &Option<IndexStageHook>,
) -> Result<Vec<u8>> {
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
        read_bounded(file, limit, cancellation, stage_hook)
    }
    #[cfg(not(unix))]
    {
        let _ = (root, relative, limit, cancellation, stage_hook);
        bail!("safe repository-relative file access requires Unix openat support on this build")
    }
}

fn read_bounded(
    mut file: File,
    limit: usize,
    cancellation: Option<&AtomicBool>,
    stage_hook: &Option<IndexStageHook>,
) -> Result<Vec<u8>> {
    let mut data = Vec::with_capacity(limit.min(64 * 1024));
    let mut chunk = [0_u8; 64 * 1024];
    let mut notified = false;
    while data.len() < limit {
        if cancellation.is_some_and(|flag| flag.load(Ordering::Relaxed)) {
            bail!("file read cancelled");
        }
        let available = (limit - data.len()).min(chunk.len());
        let count = file.read(&mut chunk[..available])?;
        if count == 0 {
            break;
        }
        data.extend_from_slice(&chunk[..count]);
        if !notified {
            notify_index_stage(stage_hook, IndexStage::FileRead);
            notified = true;
        }
    }
    if !notified {
        notify_index_stage(stage_hook, IndexStage::FileRead);
    }
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
        // `.midnight` is Midnight's own project storage and must never be indexed.
        [".git", ".hg", ".svn", ".midnight"]
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
    fn extract(
        &mut self,
        language: Option<&str>,
        text: &str,
        cancellation: &AtomicBool,
    ) -> Option<(Vec<Symbol>, bool)> {
        self.extract_with_budget(language, text, PARSER_TIME_BUDGET, cancellation)
    }

    fn extract_with_budget(
        &mut self,
        language: Option<&str>,
        text: &str,
        budget: Duration,
        cancellation: &AtomicBool,
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
            if cancellation.load(Ordering::Relaxed) || started.elapsed() >= budget {
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

/// Return process user plus system CPU time in seconds where the platform exposes it.
pub fn process_cpu_seconds() -> Option<f64> {
    #[cfg(unix)]
    unsafe {
        let mut usage: libc::rusage = std::mem::zeroed();
        if libc::getrusage(libc::RUSAGE_SELF, &mut usage) != 0 {
            return None;
        }
        let timeval_seconds =
            |value: libc::timeval| value.tv_sec as f64 + value.tv_usec as f64 / 1_000_000.0;
        Some(timeval_seconds(usage.ru_utime) + timeval_seconds(usage.ru_stime))
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
            .extract_with_budget(
                Some("rust"),
                "pub fn bounded() {}",
                Duration::ZERO,
                &std::sync::atomic::AtomicBool::new(false),
            )
            .unwrap();
        assert!(incomplete);
    }
}

#[cfg(test)]
mod index_lock_tests {
    use super::*;

    #[test]
    fn completed_index_releases_lock_even_with_inherited_descriptor() {
        let temp = tempfile::TempDir::new().unwrap();
        let scope = RepoScope {
            root: temp.path().to_path_buf(),
            repo_id: "repo".into(),
            worktree_id: "tree".into(),
            branch: None,
            revision: None,
        };
        let database = temp.path().join("index.sqlite3");
        let lock = acquire_index_lock(&database, &scope).unwrap();
        // dup shares the open-file description, just like a concurrent fork before exec.
        let inherited = lock.0.try_clone().unwrap();
        assert!(acquire_index_lock(&database, &scope).is_err());
        drop(lock);
        let next = acquire_index_lock(&database, &scope).unwrap();
        assert!(acquire_index_lock(&database, &scope).is_err());
        drop(inherited);
        assert!(acquire_index_lock(&database, &scope).is_err());
        drop(next);
        assert!(acquire_index_lock(&database, &scope).is_ok());
    }
}
