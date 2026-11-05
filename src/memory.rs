use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};
use std::ffi::CStr;
#[cfg(feature = "sqlite-scanstatus")]
use std::ffi::c_void;
use std::path::Path;
use std::ptr;
use std::time::Instant;
use unicode_normalization::{UnicodeNormalization, char::is_combining_mark};

use crate::db::{memory_scope_token, normalize_timestamp, now_utc, open_db};

pub const MAX_MEMORY_BYTES: usize = 64 * 1024;
pub const MAX_MEMORY_BATCH_RECORDS: usize = 512;
const MAX_MEMORY_BATCH_BYTES: usize = 16 * 1024 * 1024;
const MAX_MEMORY_QUERY_TERMS: usize = 64;
const MAX_RERANK_MULTIPLIER: usize = 5;
const MAX_RERANK_CANDIDATES: usize = 500;
const MIN_DISTINCT_TERM_MATCHES: usize = 2;
/// Largest informative-term count for which every "at least m of n" class is its own stage.
const MAX_STAGED_CLASS_TERMS: usize = 6;
/// Matches counted before a single-term query is considered for the non-discriminative path.
const BROAD_TERM_PROBE_MATCHES: usize = 2048;
/// Top-N by `(rank, rowid DESC)` entirely inside the FTS5 columns: no `memories` join for rows
/// that lose, and equal scores resolve to the most recently remembered row, independent of
/// content. Winners are then `LEFT JOIN`ed for namespace, deletion, and validity filters; an
/// ineligible winner yields NULL columns so the caller can tell filtering from exhaustion.
const MEMORY_RANKED_SQL: &str = "SELECT m.id,m.scope,m.kind,m.content,m.source_uri,m.source_id,m.supersedes_id,m.created_at,m.observed_at,m.valid_from,m.valid_to,f.rank
         FROM (SELECT rowid AS rid,rank FROM memory_fts
               WHERE memory_fts MATCH ?1 AND rank MATCH 'bm25(0.0,1.0)'
               ORDER BY rank,rowid DESC LIMIT ?4) f
         LEFT JOIN memories m ON m.id=f.rid AND m.scope=?2 AND m.deleted_at IS NULL
              AND (m.valid_from IS NULL OR m.valid_from<=?3)
              AND (m.valid_to IS NULL OR m.valid_to>?3)
         ORDER BY f.rank,f.rid DESC";
const MEMORY_RECENCY_SQL: &str = "SELECT m.id,m.scope,m.kind,m.content,m.source_uri,m.source_id,m.supersedes_id,m.created_at,m.observed_at,m.valid_from,m.valid_to,-CAST(m.id AS REAL)
         FROM memory_fts JOIN memories m ON m.id=memory_fts.rowid
         WHERE memory_fts MATCH ?1
           AND m.scope=?2 AND m.deleted_at IS NULL
           AND (m.valid_from IS NULL OR m.valid_from<=?3)
           AND (m.valid_to IS NULL OR m.valid_to>?3)
         ORDER BY memory_fts.rowid DESC LIMIT ?4";
const MEMORY_RECALL_SQL: &str = "SELECT m.id,m.scope,m.kind,m.content,m.source_uri,m.source_id,m.supersedes_id,m.created_at,m.observed_at,m.valid_from,m.valid_to,memory_fts.rank
         FROM memory_fts JOIN memories m ON m.id=memory_fts.rowid
         WHERE memory_fts MATCH ?1 AND memory_fts.rank MATCH 'bm25(0.0,1.0)'
           AND m.scope=?2 AND m.deleted_at IS NULL
           AND (m.valid_from IS NULL OR m.valid_from<=?3)
           AND (m.valid_to IS NULL OR m.valid_to>?3)
         ORDER BY memory_fts.rank LIMIT ?4";
const MEMORY_RECALL_ROWID_ASC_SQL: &str = "SELECT m.id,m.scope,m.kind,m.content,m.source_uri,m.source_id,m.supersedes_id,m.created_at,m.observed_at,m.valid_from,m.valid_to,memory_fts.rank
         FROM memory_fts JOIN memories m ON m.id=memory_fts.rowid
         WHERE memory_fts MATCH ?1 AND memory_fts.rank MATCH 'bm25(0.0,1.0)'
           AND m.scope=?2 AND m.deleted_at IS NULL
           AND (m.valid_from IS NULL OR m.valid_from<=?3)
           AND (m.valid_to IS NULL OR m.valid_to>?3)
         ORDER BY memory_fts.rowid ASC LIMIT ?4";
const MEMORY_RECALL_ROWID_DESC_SQL: &str = "SELECT m.id,m.scope,m.kind,m.content,m.source_uri,m.source_id,m.supersedes_id,m.created_at,m.observed_at,m.valid_from,m.valid_to,memory_fts.rank
         FROM memory_fts JOIN memories m ON m.id=memory_fts.rowid
         WHERE memory_fts MATCH ?1 AND memory_fts.rank MATCH 'bm25(0.0,1.0)'
           AND m.scope=?2 AND m.deleted_at IS NULL
           AND (m.valid_from IS NULL OR m.valid_from<=?3)
           AND (m.valid_to IS NULL OR m.valid_to>?3)
         ORDER BY memory_fts.rowid DESC LIMIT ?4";

#[derive(Clone, Copy, Debug, Serialize)]
pub(crate) enum RecallCandidateOrder {
    /// Production: class-staged exact top-k (see `recall_staged`).
    Staged,
    /// v0.3.1 single-statement BM25 window, kept for paired A/B measurement.
    Ranked,
    RowIdAscending,
    RowIdDescending,
    BalancedRowIdWindows,
}

impl RecallCandidateOrder {
    fn sql(self) -> &'static str {
        match self {
            Self::Staged => MEMORY_RANKED_SQL,
            Self::Ranked => MEMORY_RECALL_SQL,
            Self::RowIdAscending => MEMORY_RECALL_ROWID_ASC_SQL,
            Self::RowIdDescending => MEMORY_RECALL_ROWID_DESC_SQL,
            Self::BalancedRowIdWindows => MEMORY_RECALL_ROWID_ASC_SQL,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Staged => "staged_exact_bm25",
            Self::Ranked => "bm25_rank",
            Self::RowIdAscending => "rowid_ascending",
            Self::RowIdDescending => "rowid_descending",
            Self::BalancedRowIdWindows => "rowid_balanced_windows",
        }
    }
}

/// How a single query term found in at least half of a namespace is retrieved.
///
/// BM25 length and frequency normalization still orders such terms meaningfully (the frozen
/// `common-status-single-token` case ranks the short status-focused record first), so `Exact`
/// is the default. `Recent` is an explicit, bounded, observable shortcut: it skips ranking and
/// returns the newest matches, labeled `recency_window`, and can omit older relevant evidence.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BroadTermPolicy {
    #[default]
    Exact,
    Recent,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CandidateCoverage {
    /// Every row matching the lexical query and SQL filters was considered.
    Complete,
    /// The returned list is the exact top-k of all eligible rows under the documented
    /// ordering (phrase, distinct-term coverage, BM25, deterministic tie-breaks), but
    /// lower-priority classes were not enumerated once the result limit was filled.
    ExactTopK,
    /// Only the top bounded FTS5 BM25 candidate window was reranked.
    Bm25TopK,
    /// The single query term occurs in at least half of the namespace and carries no
    /// BM25 weight; the newest matching rows were returned instead of ranking all of them.
    RecencyWindow,
    /// Candidates came from bounded row-ID windows and may omit interior rows.
    RowIdSample,
}

#[derive(Debug, Clone, Serialize)]
pub struct MemoryRecord {
    pub id: i64,
    pub scope: String,
    pub kind: String,
    pub content: String,
    pub source_uri: Option<String>,
    pub source_id: Option<i64>,
    pub supersedes_id: Option<i64>,
    pub created_at: String,
    pub observed_at: Option<String>,
    pub valid_from: Option<String>,
    pub valid_to: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct MemorySearchResult {
    pub memory: MemoryRecord,
    pub match_reason: String,
    pub score: f64,
    pub candidate_coverage: CandidateCoverage,
}

#[derive(Debug, Default, Serialize)]
pub(crate) struct MemoryRecallProfile {
    pub candidate_strategy: Option<&'static str>,
    pub phase_wall_ns: BTreeMap<String, u64>,
    pub phase_cpu_ns: BTreeMap<String, Option<u64>>,
    pub candidate_limit: usize,
    pub active_scope_record_count: Option<usize>,
    pub output_fraction_of_scope: Option<f64>,
    pub candidate_budget_saturated: bool,
    pub candidates_returned: usize,
    pub candidate_rows_deduplicated: usize,
    pub candidates_hydrated: usize,
    pub candidates_rejected_by_term_coverage: usize,
    pub content_bytes_hydrated: usize,
    pub record_bytes_hydrated: usize,
    pub result_count: usize,
    pub result_bytes_serialized: Option<usize>,
    pub sqlite_cache_hits: Option<u64>,
    pub sqlite_cache_misses: Option<u64>,
    pub sqlite_cache_writes: Option<u64>,
    pub sqlite_cache_spills: Option<u64>,
    pub sqlite_cache_size_kib: Option<u64>,
    pub sqlite_cache_used_bytes: Option<u64>,
    pub sqlite_vm_steps: Option<u64>,
    pub sqlite_fullscan_steps: Option<u64>,
    pub sqlite_sort_operations: Option<u64>,
    pub sqlite_statement_memory_bytes: Option<u64>,
    pub sqlite_scanstatus_available: bool,
    pub sqlite_scan_loops: Option<Vec<SqliteScanLoop>>,
    pub temporal_filter_location: Option<&'static str>,
    pub temporal_rejections_observable: bool,
    pub allocation_count: Option<u64>,
    pub peak_temporary_memory_bytes: Option<u64>,
    pub unavailable_measurements: Vec<&'static str>,
}

#[derive(Debug, Serialize)]
pub(crate) struct SqliteScanLoop {
    pub name: String,
    pub loops: i64,
    pub visits: i64,
}

impl MemoryRecallProfile {
    fn add_phase(&mut self, name: &'static str, wall_ns: u64, cpu_ns: Option<u64>) {
        *self.phase_wall_ns.entry(name.to_string()).or_default() += wall_ns;
        let total = self.phase_cpu_ns.entry(name.to_string()).or_insert(Some(0));
        *total = match (*total, cpu_ns) {
            (Some(previous), Some(current)) => Some(previous.saturating_add(current)),
            _ => None,
        };
    }

    pub(crate) fn add_benchmark_wall_phase(&mut self, name: &'static str, wall_ns: u64) {
        *self.phase_wall_ns.entry(name.to_string()).or_default() += wall_ns;
        self.phase_cpu_ns.entry(name.to_string()).or_insert(None);
    }
}

struct ProfileTimer<'a> {
    profile: &'a mut MemoryRecallProfile,
    name: &'static str,
    wall_started: Instant,
    cpu_started_ns: Option<u64>,
}

impl<'a> ProfileTimer<'a> {
    fn start(profile: Option<&'a mut MemoryRecallProfile>, name: &'static str) -> Option<Self> {
        profile.map(|profile| Self {
            profile,
            name,
            wall_started: Instant::now(),
            cpu_started_ns: thread_cpu_time_ns(),
        })
    }

    fn finish(self) {
        let wall_ns = self.wall_started.elapsed().as_nanos().min(u64::MAX as u128) as u64;
        let cpu_ns = self
            .cpu_started_ns
            .zip(thread_cpu_time_ns())
            .map(|(started, finished)| finished.saturating_sub(started));
        self.profile.add_phase(self.name, wall_ns, cpu_ns);
    }
}

#[cfg(unix)]
fn thread_cpu_time_ns() -> Option<u64> {
    let mut time = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    let result = unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut time) };
    (result == 0).then(|| {
        (time.tv_sec.max(0) as u64)
            .saturating_mul(1_000_000_000)
            .saturating_add(time.tv_nsec.max(0) as u64)
    })
}

#[cfg(not(unix))]
fn thread_cpu_time_ns() -> Option<u64> {
    None
}

#[derive(Debug, Clone, Deserialize)]
pub struct MemoryInput {
    pub scope: String,
    pub kind: String,
    pub content: String,
    #[serde(default)]
    pub source_uri: Option<String>,
    #[serde(default)]
    pub source_id: Option<i64>,
    #[serde(default)]
    pub supersedes_id: Option<i64>,
    #[serde(default)]
    pub observed_at: Option<String>,
    #[serde(default)]
    pub valid_from: Option<String>,
    #[serde(default)]
    pub valid_to: Option<String>,
}

#[derive(Debug, Clone, Copy)]
struct MemoryInputRef<'a> {
    scope: &'a str,
    kind: &'a str,
    content: &'a str,
    source_uri: Option<&'a str>,
    source_id: Option<i64>,
    supersedes_id: Option<i64>,
    observed_at: Option<&'a str>,
    valid_from: Option<&'a str>,
    valid_to: Option<&'a str>,
}

struct ValidatedMemoryInput<'a> {
    input: MemoryInputRef<'a>,
    observed_at: Option<String>,
    valid_from: Option<String>,
    valid_to: Option<String>,
}

impl<'a> From<&'a MemoryInput> for MemoryInputRef<'a> {
    fn from(input: &'a MemoryInput) -> Self {
        Self {
            scope: &input.scope,
            kind: &input.kind,
            content: &input.content,
            source_uri: input.source_uri.as_deref(),
            source_id: input.source_id,
            supersedes_id: input.supersedes_id,
            observed_at: input.observed_at.as_deref(),
            valid_from: input.valid_from.as_deref(),
            valid_to: input.valid_to.as_deref(),
        }
    }
}

pub(crate) struct MemorySession {
    connection: Connection,
}

impl MemorySession {
    pub(crate) fn open(db_path: &Path) -> Result<Self> {
        Ok(Self {
            connection: open_db(db_path)?,
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn remember(
        &mut self,
        scope: &str,
        kind: &str,
        content: &str,
        source_uri: Option<&str>,
        source_id: Option<i64>,
        supersedes_id: Option<i64>,
        observed_at: Option<&str>,
        valid_from: Option<&str>,
        valid_to: Option<&str>,
    ) -> Result<MemoryRecord> {
        remember_on(
            &mut self.connection,
            MemoryInputRef {
                scope,
                kind,
                content,
                source_uri,
                source_id,
                supersedes_id,
                observed_at,
                valid_from,
                valid_to,
            },
        )
    }

    pub(crate) fn remember_batch(&mut self, inputs: &[MemoryInput]) -> Result<Vec<MemoryRecord>> {
        remember_batch_on(
            &mut self.connection,
            &inputs.iter().map(MemoryInputRef::from).collect::<Vec<_>>(),
        )
    }

    pub(crate) fn recall(
        &self,
        scope: &str,
        query: &str,
        at: Option<&str>,
        limit: usize,
    ) -> Result<Vec<MemorySearchResult>> {
        recall_on(&self.connection, scope, query, at, limit)
    }

    /// Recall with an explicit policy for single terms that occur in most of a namespace.
    pub(crate) fn recall_with_policy(
        &self,
        scope: &str,
        query: &str,
        at: Option<&str>,
        limit: usize,
        broad_terms: BroadTermPolicy,
    ) -> Result<Vec<MemorySearchResult>> {
        recall_on_profiled(
            &self.connection,
            scope,
            query,
            at,
            limit,
            None,
            configured_candidate_order(),
            broad_terms,
        )
    }

    /// Unprofiled recall with an explicit candidate strategy, for paired A/B measurement.
    pub(crate) fn recall_ordered(
        &self,
        scope: &str,
        query: &str,
        at: Option<&str>,
        limit: usize,
        candidate_order: RecallCandidateOrder,
    ) -> Result<Vec<MemorySearchResult>> {
        recall_on_profiled(
            &self.connection,
            scope,
            query,
            at,
            limit,
            None,
            candidate_order,
            BroadTermPolicy::Exact,
        )
    }

    pub(crate) fn recall_profiled_ordered(
        &self,
        scope: &str,
        query: &str,
        at: Option<&str>,
        limit: usize,
        candidate_order: RecallCandidateOrder,
    ) -> Result<(Vec<MemorySearchResult>, MemoryRecallProfile)> {
        let mut profile = MemoryRecallProfile {
            candidate_strategy: Some(candidate_order.label()),
            temporal_filter_location: Some(
                "SQLite WHERE predicates; included in candidate-step time",
            ),
            temporal_rejections_observable: false,
            unavailable_measurements: vec![
                "FTS5 term-posting traversal count is not exposed; SQLite scan-status NVISIT counts plan-loop rows, not internal postings examined",
                "allocation count is unavailable; the system allocator is not instrumented",
                "per-query peak temporary memory is unavailable; process RSS is measured by the outer benchmark",
                "physical index-page reads are unavailable; SQLite cache misses do not imply filesystem reads",
            ],
            ..MemoryRecallProfile::default()
        };
        let results = recall_on_profiled(
            &self.connection,
            scope,
            query,
            at,
            limit,
            Some(&mut profile),
            candidate_order,
            BroadTermPolicy::Exact,
        )?;
        profile.result_count = results.len();
        let count_started = Instant::now();
        let active_scope_record_count = self.connection.query_row(
            "SELECT count(*) FROM memories WHERE scope=?1 AND deleted_at IS NULL",
            [scope],
            |row| row.get::<_, i64>(0),
        )? as usize;
        profile.active_scope_record_count = Some(active_scope_record_count);
        profile.output_fraction_of_scope = (active_scope_record_count > 0)
            .then(|| profile.candidates_hydrated as f64 / active_scope_record_count as f64);
        profile.add_benchmark_wall_phase(
            "diagnostic_active_scope_count",
            count_started.elapsed().as_nanos().min(u64::MAX as u128) as u64,
        );
        profile.sqlite_cache_size_kib = self
            .connection
            .pragma_query_value(None, "cache_size", |row| row.get::<_, i64>(0))
            .ok()
            .map(|value| {
                if value < 0 {
                    value.unsigned_abs()
                } else {
                    let page_size = self
                        .connection
                        .pragma_query_value(None, "page_size", |row| row.get::<_, i64>(0))
                        .unwrap_or(4096)
                        .max(1) as u64;
                    value as u64 * page_size / 1024
                }
            });
        profile.sqlite_cache_used_bytes =
            sqlite_db_status(&self.connection, rusqlite::ffi::SQLITE_DBSTATUS_CACHE_USED);
        Ok((results, profile))
    }

    pub(crate) fn set_page_cache_size_kib(&self, cache_size_kib: usize) -> Result<()> {
        if !(1..=65_536).contains(&cache_size_kib) {
            bail!("SQLite page cache size must be between 1 and 65536 KiB");
        }
        self.connection
            .pragma_update(None, "cache_size", -(cache_size_kib as i64))?;
        Ok(())
    }

    pub(crate) fn shrink_page_cache(&self) -> Result<()> {
        self.connection.execute_batch("PRAGMA shrink_memory;")?;
        Ok(())
    }

    pub(crate) fn page_cache_used_bytes(&self) -> Option<u64> {
        sqlite_db_status(&self.connection, rusqlite::ffi::SQLITE_DBSTATUS_CACHE_USED)
    }

    pub(crate) fn list_memories(&self, scope: &str, limit: usize) -> Result<Vec<MemoryRecord>> {
        list_memories_on(&self.connection, scope, limit)
    }

    pub(crate) fn forget(&self, scope: &str, id: i64) -> Result<bool> {
        forget_on(&self.connection, scope, id)
    }

    pub(crate) fn recall_query_plan(&self, scope: &str, query: &str) -> Result<Vec<String>> {
        self.recall_query_plan_ordered(scope, query, RecallCandidateOrder::Ranked)
    }

    pub(crate) fn recall_query_plan_ordered(
        &self,
        scope: &str,
        query: &str,
        candidate_order: RecallCandidateOrder,
    ) -> Result<Vec<String>> {
        validate_scope(scope)?;
        let full_terms = normalized_tokens(query);
        let query_terms = informative_terms(&full_terms);
        if query_terms.is_empty() {
            return Ok(Vec::new());
        }
        let match_query = memory_match_query(scope, &query_terms, &full_terms);
        let orders = if matches!(candidate_order, RecallCandidateOrder::BalancedRowIdWindows) {
            vec![
                RecallCandidateOrder::RowIdAscending,
                RecallCandidateOrder::RowIdDescending,
            ]
        } else {
            vec![candidate_order]
        };
        let mut plans = Vec::new();
        for order in orders {
            let sql = format!("EXPLAIN QUERY PLAN {}", order.sql());
            let mut statement = self.connection.prepare(&sql)?;
            let rows = statement
                .query_map(params![match_query, scope, now_utc(), 10i64], |row| {
                    row.get(3)
                })?;
            plans.extend(rows.collect::<rusqlite::Result<Vec<String>>>()?);
        }
        Ok(plans)
    }
}

#[allow(clippy::too_many_arguments)] // Public arguments mirror provenance and validity fields.
pub fn remember(
    db_path: &Path,
    scope: &str,
    kind: &str,
    content: &str,
    source_uri: Option<&str>,
    source_id: Option<i64>,
    supersedes_id: Option<i64>,
    observed_at: Option<&str>,
    valid_from: Option<&str>,
    valid_to: Option<&str>,
) -> Result<MemoryRecord> {
    let mut connection = open_db(db_path)?;
    remember_on(
        &mut connection,
        MemoryInputRef {
            scope,
            kind,
            content,
            source_uri,
            source_id,
            supersedes_id,
            observed_at,
            valid_from,
            valid_to,
        },
    )
}

/// Insert a bounded set of memories atomically. Any invalid record or provenance link rolls back
/// every row in this call. Larger imports should be split into caller-managed chunks.
pub fn remember_batch(db_path: &Path, inputs: &[MemoryInput]) -> Result<Vec<MemoryRecord>> {
    if inputs.is_empty() {
        return Ok(Vec::new());
    }
    let refs = inputs.iter().map(MemoryInputRef::from).collect::<Vec<_>>();
    validate_batch(&refs)?;
    let mut connection = open_db(db_path)?;
    remember_batch_on(&mut connection, &refs)
}

fn remember_on(connection: &mut Connection, input: MemoryInputRef<'_>) -> Result<MemoryRecord> {
    let validated = validate_input(input)?;
    let tx = connection.transaction()?;
    let record = insert_memory(&tx, validated)?;
    tx.commit()?;
    Ok(record)
}

fn remember_batch_on(
    connection: &mut Connection,
    inputs: &[MemoryInputRef<'_>],
) -> Result<Vec<MemoryRecord>> {
    if inputs.is_empty() {
        return Ok(Vec::new());
    }
    validate_batch(inputs)?;
    let validated = inputs
        .iter()
        .copied()
        .map(validate_input)
        .collect::<Result<Vec<_>>>()?;
    let tx = connection.transaction()?;
    let mut records = Vec::with_capacity(validated.len());
    for input in validated {
        records.push(insert_memory(&tx, input)?);
    }
    tx.commit()?;
    Ok(records)
}

fn validate_batch(inputs: &[MemoryInputRef<'_>]) -> Result<()> {
    if inputs.len() > MAX_MEMORY_BATCH_RECORDS {
        bail!("memory batch exceeds {MAX_MEMORY_BATCH_RECORDS} records");
    }
    let content_bytes = inputs.iter().try_fold(0usize, |total, input| {
        total
            .checked_add(input.content.len())
            .and_then(|bytes| bytes.checked_add(input.source_uri.map_or(0, str::len)))
            .ok_or_else(|| anyhow::anyhow!("memory batch byte count overflow"))
    })?;
    if content_bytes > MAX_MEMORY_BATCH_BYTES {
        bail!("memory batch content exceeds 16 MiB");
    }
    Ok(())
}

fn validate_input(input: MemoryInputRef<'_>) -> Result<ValidatedMemoryInput<'_>> {
    validate_scope(input.scope)?;
    let observed_at = input.observed_at.map(normalize_timestamp).transpose()?;
    let valid_from = input.valid_from.map(normalize_timestamp).transpose()?;
    let valid_to = input.valid_to.map(normalize_timestamp).transpose()?;
    if !matches!(input.kind, "source" | "derived") {
        bail!("kind must be 'source' or 'derived'");
    }
    if input.content.trim().is_empty() {
        bail!("memory content cannot be empty");
    }
    if input.content.len() > MAX_MEMORY_BYTES {
        bail!("memory content exceeds 64 KiB");
    }
    if let (Some(from), Some(to)) = (valid_from.as_deref(), valid_to.as_deref())
        && from >= to
    {
        bail!("valid_to must be later than valid_from");
    }
    if input.kind == "source" && input.source_id.is_some() {
        bail!("raw source memories cannot link to another source id");
    }
    if input.kind == "derived" && input.source_id.is_none() && input.source_uri.is_none() {
        bail!("derived memories require source_id or source_uri provenance");
    }
    if input.supersedes_id.is_some() && (input.kind != "derived" || valid_from.is_none()) {
        bail!("superseding a fact requires kind=derived and an explicit valid_from");
    }
    Ok(ValidatedMemoryInput {
        input,
        observed_at,
        valid_from,
        valid_to,
    })
}

fn insert_memory(
    tx: &Transaction<'_>,
    validated: ValidatedMemoryInput<'_>,
) -> Result<MemoryRecord> {
    let ValidatedMemoryInput {
        input,
        observed_at,
        valid_from,
        valid_to,
    } = validated;
    if let Some(source_id) = input.source_id {
        let valid: bool = tx
            .prepare_cached(
                "SELECT EXISTS(SELECT 1 FROM memories WHERE id=?1 AND scope=?2 AND kind='source' AND deleted_at IS NULL)",
            )?
            .query_row(params![source_id, input.scope], |row| row.get(0))?;
        if !valid {
            bail!("source_id must name an active raw source in the same memory scope");
        }
    }
    if let Some(supersedes_id) = input.supersedes_id {
        let prior: Option<(Option<String>, Option<String>)> = tx
            .prepare_cached(
                "SELECT valid_from,valid_to FROM memories WHERE id=?1 AND scope=?2 AND kind='derived' AND deleted_at IS NULL",
            )?
            .query_row(params![supersedes_id, input.scope], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .optional()?;
        let Some((prior_from, prior_to)) = prior else {
            bail!("supersedes_id must name an active derived fact in the same scope");
        };
        let new_from = valid_from.as_deref().expect("validated above");
        if prior_from.as_deref().is_some_and(|from| from >= new_from) {
            bail!("replacement valid_from must be later than the prior fact's start");
        }
        if prior_to.as_deref().is_none_or(|to| to > new_from) {
            tx.prepare_cached("UPDATE memories SET valid_to=?3 WHERE id=?1 AND scope=?2")?
                .execute(params![supersedes_id, input.scope, new_from])?;
        }
    }
    let created_at = now_utc();
    let id: i64 = tx
        .prepare_cached(
            "INSERT INTO memories(scope,kind,content,source_uri,source_id,supersedes_id,created_at,observed_at,valid_from,valid_to)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10) RETURNING id",
        )?
        .query_row(
            params![
                input.scope,
                input.kind,
                input.content,
                input.source_uri,
                input.source_id,
                input.supersedes_id,
                created_at,
                observed_at,
                valid_from,
                valid_to
            ],
            |row| row.get(0),
        )?;
    tx.prepare_cached("INSERT INTO memory_fts(rowid,scope_token,content) VALUES(?1,?2,?3)")?
        .execute(params![id, memory_scope_token(input.scope), input.content])?;
    Ok(MemoryRecord {
        id,
        scope: input.scope.to_string(),
        kind: input.kind.to_string(),
        content: input.content.to_string(),
        source_uri: input.source_uri.map(str::to_string),
        source_id: input.source_id,
        supersedes_id: input.supersedes_id,
        created_at,
        observed_at,
        valid_from,
        valid_to,
    })
}

pub fn recall(
    db_path: &Path,
    scope: &str,
    query: &str,
    at: Option<&str>,
    limit: usize,
) -> Result<Vec<MemorySearchResult>> {
    let conn = open_db(db_path)?;
    recall_on(&conn, scope, query, at, limit)
}

fn recall_on(
    conn: &Connection,
    scope: &str,
    query: &str,
    at: Option<&str>,
    limit: usize,
) -> Result<Vec<MemorySearchResult>> {
    recall_on_profiled(
        conn,
        scope,
        query,
        at,
        limit,
        None,
        configured_candidate_order(),
        BroadTermPolicy::Exact,
    )
}

fn configured_candidate_order() -> RecallCandidateOrder {
    #[cfg(feature = "retrieval-experiments")]
    if let Ok(order) = std::env::var("AETHER_RECALL_EXPERIMENT") {
        match order.as_str() {
            "staged" => return RecallCandidateOrder::Staged,
            "ranked" => return RecallCandidateOrder::Ranked,
            "rowid_ascending" => return RecallCandidateOrder::RowIdAscending,
            "rowid_descending" => return RecallCandidateOrder::RowIdDescending,
            "rowid_balanced_windows" => return RecallCandidateOrder::BalancedRowIdWindows,
            _ => {}
        }
    }
    RecallCandidateOrder::Staged
}

#[allow(clippy::too_many_arguments)] // Mirrors the public recall parameters plus profiling and policy.
fn recall_on_profiled(
    conn: &Connection,
    scope: &str,
    query: &str,
    at: Option<&str>,
    limit: usize,
    mut profile: Option<&mut MemoryRecallProfile>,
    candidate_order: RecallCandidateOrder,
    broad_terms: BroadTermPolicy,
) -> Result<Vec<MemorySearchResult>> {
    let scanstatus_enabled = if profile.is_some() {
        sqlite_scanstatus_enabled(conn)
    } else {
        false
    };
    if let Some(profile) = profile.as_deref_mut() {
        profile.sqlite_scanstatus_available = scanstatus_enabled;
    }
    let total_wall_started = profile.as_ref().map(|_| Instant::now());
    let total_cpu_started = profile.as_ref().and_then(|_| thread_cpu_time_ns());

    let phase = ProfileTimer::start(profile.as_deref_mut(), "query_validation");
    validate_scope(scope)?;
    if query.trim().is_empty() {
        bail!("recall query cannot be empty");
    }
    if query.len() > 4096 {
        bail!("memory recall query exceeds 4096 bytes");
    }
    let at = at
        .map(normalize_timestamp)
        .transpose()?
        .unwrap_or_else(now_utc);
    if let Some(phase) = phase {
        phase.finish();
    }

    let phase = ProfileTimer::start(profile.as_deref_mut(), "unicode_normalization");
    let full_terms = normalized_tokens(query);
    if full_terms.len() > MAX_MEMORY_QUERY_TERMS {
        bail!("memory recall query contains more than {MAX_MEMORY_QUERY_TERMS} searchable terms");
    }
    if let Some(phase) = phase {
        phase.finish();
    }

    let phase = ProfileTimer::start(profile.as_deref_mut(), "stopword_filtering");
    let query_terms = informative_terms(&full_terms);
    if query_terms.is_empty() {
        finish_profile_total(
            profile.as_deref_mut(),
            total_wall_started,
            total_cpu_started,
        );
        return Ok(Vec::new());
    }
    if let Some(phase) = phase {
        phase.finish();
    }

    if matches!(candidate_order, RecallCandidateOrder::Staged) {
        let results = recall_staged(
            conn,
            scope,
            &at,
            &full_terms,
            &query_terms,
            limit.clamp(1, 100),
            broad_terms,
            profile.as_deref_mut(),
        )?;
        finish_profile_total(
            profile.as_deref_mut(),
            total_wall_started,
            total_cpu_started,
        );
        return Ok(results);
    }
    let candidate_order = if query_terms.len() == 1 {
        candidate_order
    } else {
        RecallCandidateOrder::Ranked
    };
    if let Some(profile) = profile.as_deref_mut() {
        profile.candidate_strategy = Some(candidate_order.label());
    }

    let phase = ProfileTimer::start(profile.as_deref_mut(), "fts_query_construction");
    let match_query = memory_match_query(scope, &query_terms, &full_terms);
    let result_limit = limit.clamp(1, 100);
    let candidate_limit = result_limit
        .saturating_mul(MAX_RERANK_MULTIPLIER)
        .min(MAX_RERANK_CANDIDATES)
        .max(result_limit);
    if let Some(phase) = phase {
        phase.finish();
    }
    if let Some(profile) = profile.as_deref_mut() {
        profile.candidate_limit = candidate_limit;
    }

    let cache_before = profile.is_some().then(|| sqlite_cache_snapshot(conn));
    let mut candidates = Vec::with_capacity(candidate_limit.min(64));
    let mut candidate_ids = matches!(candidate_order, RecallCandidateOrder::BalancedRowIdWindows)
        .then(|| HashSet::with_capacity(candidate_limit.min(64)));
    let (scan_specs, scan_count, ascending_budget) = match candidate_order {
        RecallCandidateOrder::BalancedRowIdWindows => {
            let ascending_budget = candidate_limit.div_ceil(2);
            (
                [
                    (RecallCandidateOrder::RowIdAscending, ascending_budget),
                    (
                        RecallCandidateOrder::RowIdDescending,
                        candidate_limit - ascending_budget,
                    ),
                ],
                2,
                Some(ascending_budget),
            )
        }
        order => ([(order, candidate_limit), (order, 0)], 1, None),
    };
    let mut candidate_scan_truncated = false;
    for (scan_index, (order, scan_limit)) in scan_specs.into_iter().take(scan_count).enumerate() {
        if scan_limit == 0 {
            continue;
        }
        let sql = order.sql();
        let phase = ProfileTimer::start(profile.as_deref_mut(), "sqlite_statement_prepare");
        let mut stmt = conn.prepare_cached(sql)?;
        if let Some(phase) = phase {
            phase.finish();
        }
        if profile.is_some() {
            sqlite_reset_statement_profile(conn, sql);
        }

        let phase = ProfileTimer::start(profile.as_deref_mut(), "sqlite_parameter_bind");
        let mut rows = stmt.query(params![&match_query, scope, at, scan_limit as i64])?;
        if let Some(phase) = phase {
            phase.finish();
        }

        let mut scan_rows = 0;
        loop {
            let phase = ProfileTimer::start(profile.as_deref_mut(), "sqlite_candidate_step");
            let next = rows.next();
            if let Some(phase) = phase {
                phase.finish();
            }
            let row = match next.context("read memory search results")? {
                Some(row) => row,
                None => break,
            };
            scan_rows += 1;
            if let Some(profile) = profile.as_deref_mut() {
                profile.candidates_returned += 1;
            }

            let phase = ProfileTimer::start(profile.as_deref_mut(), "candidate_record_hydration");
            let candidate = (|| -> rusqlite::Result<Option<(MemorySearchResult, f64)>> {
                let id: i64 = row.get(0)?;
                if candidate_ids
                    .as_mut()
                    .is_some_and(|candidate_ids| !candidate_ids.insert(id))
                {
                    return Ok(None);
                }
                let memory = MemoryRecord {
                    id,
                    scope: row.get(1)?,
                    kind: row.get(2)?,
                    content: row.get(3)?,
                    source_uri: row.get(4)?,
                    source_id: row.get(5)?,
                    supersedes_id: row.get(6)?,
                    created_at: row.get(7)?,
                    observed_at: row.get(8)?,
                    valid_from: row.get(9)?,
                    valid_to: row.get(10)?,
                };
                let rank: f64 = row.get(11)?;
                Ok(Some((
                    MemorySearchResult {
                        memory,
                        match_reason: "memory_lexical".into(),
                        score: (20.0 + (-rank * 1_000_000.0).clamp(0.0, 30.0)).clamp(0.0, 50.0),
                        candidate_coverage: CandidateCoverage::Complete,
                    },
                    rank,
                )))
            })();
            if let Some(phase) = phase {
                phase.finish();
            }
            if let Some(candidate) = candidate.context("hydrate memory search candidate")? {
                if let Some(profile) = profile.as_deref_mut() {
                    profile.candidates_hydrated += 1;
                    profile.content_bytes_hydrated += candidate.0.memory.content.len();
                    profile.record_bytes_hydrated += memory_record_bytes(&candidate.0.memory);
                }
                candidates.push(candidate);
            } else if let Some(profile) = profile.as_deref_mut() {
                profile.candidate_rows_deduplicated += 1;
            }
        }
        drop(rows);
        drop(stmt);
        candidate_scan_truncated |= scan_rows == scan_limit;
        if let Some(profile) = profile.as_deref_mut() {
            sqlite_statement_profile(conn, sql, profile);
        }
        if scan_index == 0 && ascending_budget.is_some_and(|budget| scan_rows < budget) {
            break;
        }
    }
    let candidate_coverage = if !candidate_scan_truncated {
        CandidateCoverage::Complete
    } else if matches!(candidate_order, RecallCandidateOrder::Ranked) {
        CandidateCoverage::Bm25TopK
    } else {
        CandidateCoverage::RowIdSample
    };
    if let Some(profile) = profile.as_deref_mut() {
        profile.candidate_budget_saturated = candidate_scan_truncated;
    }
    if let Some(profile) = profile.as_deref_mut()
        && let Some(cache_before) = cache_before
    {
        let cache_after = sqlite_cache_snapshot(conn);
        profile.sqlite_cache_hits = cache_after
            .hits
            .zip(cache_before.hits)
            .map(|(after, before)| after.saturating_sub(before));
        profile.sqlite_cache_misses = cache_after
            .misses
            .zip(cache_before.misses)
            .map(|(after, before)| after.saturating_sub(before));
        profile.sqlite_cache_writes = cache_after
            .writes
            .zip(cache_before.writes)
            .map(|(after, before)| after.saturating_sub(before));
        profile.sqlite_cache_spills = cache_after
            .spills
            .zip(cache_before.spills)
            .map(|(after, before)| after.saturating_sub(before));
    }

    let mut ranked = candidates
        .into_iter()
        .filter_map(|(mut result, rank)| {
            let phase = ProfileTimer::start(profile.as_deref_mut(), "candidate_text_tokenization");
            let content_terms = token_vec(&result.memory.content);
            if let Some(phase) = phase {
                phase.finish();
            }

            let phase = ProfileTimer::start(profile.as_deref_mut(), "lexical_scoring");
            let matched_terms = query_terms
                .iter()
                .filter(|term| {
                    content_terms
                        .iter()
                        .any(|content_term| content_term == *term)
                })
                .count();
            let required_matches = MIN_DISTINCT_TERM_MATCHES.min(query_terms.len());
            let rejected_by_term_coverage = matched_terms < required_matches;
            let exact_phrase = !rejected_by_term_coverage
                && contains_contiguous_terms(&content_terms, &full_terms);
            if exact_phrase {
                result.match_reason = "memory_exact_phrase".into();
            }
            if let Some(phase) = phase {
                phase.finish();
            }
            if rejected_by_term_coverage {
                if let Some(profile) = profile.as_deref_mut() {
                    profile.candidates_rejected_by_term_coverage += 1;
                }
                return None;
            }
            Some((result, exact_phrase, matched_terms, rank))
        })
        .collect::<Vec<_>>();
    let phase = ProfileTimer::start(profile.as_deref_mut(), "final_sort");
    ranked.sort_by(compare_ranked_candidates);
    if let Some(phase) = phase {
        phase.finish();
    }
    let results = ranked
        .into_iter()
        .take(result_limit)
        .map(|(mut result, _, _, _)| {
            result.candidate_coverage = candidate_coverage;
            result
        })
        .collect::<Vec<_>>();
    finish_profile_total(profile, total_wall_started, total_cpu_started);
    Ok(results)
}

type RankedCandidate = (MemorySearchResult, bool, usize, f64);

/// Final deterministic ordering: exact phrase, distinct-term coverage, lexical rank,
/// then temporal and provenance fields, ending in the row ID.
fn compare_ranked_candidates(
    left: &RankedCandidate,
    right: &RankedCandidate,
) -> std::cmp::Ordering {
    right
        .1
        .cmp(&left.1)
        .then_with(|| right.2.cmp(&left.2))
        .then_with(|| left.3.total_cmp(&right.3))
        .then_with(|| right.0.memory.observed_at.cmp(&left.0.memory.observed_at))
        .then_with(|| right.0.memory.valid_from.cmp(&left.0.memory.valid_from))
        .then_with(|| left.0.memory.content.cmp(&right.0.memory.content))
        .then_with(|| left.0.memory.source_uri.cmp(&right.0.memory.source_uri))
        .then_with(|| left.0.memory.kind.cmp(&right.0.memory.kind))
        .then_with(|| left.0.memory.source_id.cmp(&right.0.memory.source_id))
        .then_with(|| {
            left.0
                .memory
                .supersedes_id
                .cmp(&right.0.memory.supersedes_id)
        })
        .then_with(|| left.0.memory.id.cmp(&right.0.memory.id))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum StageKind {
    /// FTS5 BM25 order with deterministic tie-breaks.
    Ranked,
    /// Newest row IDs first; used only for non-discriminative single terms.
    Recency,
}

/// Namespace sizes from the trigger-maintained counters: (this namespace, all namespaces).
fn scope_sizes(conn: &Connection, scope: &str) -> Result<(usize, usize)> {
    let (scope_active, total_active): (i64, i64) = conn
        .prepare_cached(
            "SELECT COALESCE((SELECT active FROM memory_scope_counts WHERE scope=?1),0),
                    COALESCE((SELECT active FROM memory_scope_counts WHERE scope=''),0)",
        )?
        .query_row([scope], |row| Ok((row.get(0)?, row.get(1)?)))?;
    Ok((scope_active.max(0) as usize, total_active.max(0) as usize))
}

/// Restrict the FTS expression to a namespace. When the namespace holds at least half of all
/// memories the `scope_token` term prunes little but forces FTS5 to walk a posting list as long
/// as the namespace, so the SQL `m.scope = ?` predicate alone enforces the boundary. The
/// scope column has BM25 weight 0, so scores are identical either way.
fn scoped_match(scope: &str, content_expr: &str, use_scope_term: bool) -> String {
    if use_scope_term {
        format!(
            "scope_token : \"{}\" AND content : ({content_expr})",
            memory_scope_token(scope)
        )
    } else {
        format!("content : ({content_expr})")
    }
}

fn all_terms_expr(terms: &[&str]) -> String {
    terms
        .iter()
        .map(|term| format!("\"{term}\""))
        .collect::<Vec<_>>()
        .join(" AND ")
}

fn term_subsets<'a>(terms: &[&'a str], size: usize) -> Vec<Vec<&'a str>> {
    fn walk<'a>(
        terms: &[&'a str],
        start: usize,
        size: usize,
        current: &mut Vec<&'a str>,
        out: &mut Vec<Vec<&'a str>>,
    ) {
        if current.len() == size {
            out.push(current.clone());
            return;
        }
        for index in start..terms.len() {
            current.push(terms[index]);
            walk(terms, index + 1, size, current, out);
            current.pop();
        }
    }
    let mut out = Vec::new();
    walk(terms, 0, size, &mut Vec::new(), &mut out);
    out
}

/// Rows containing at least `size` of the terms. Every term appears in the same number of
/// subsets, so BM25 over this expression is a uniform multiple of BM25 over the terms.
fn at_least_expr(terms: &[&str], size: usize) -> String {
    term_subsets(terms, size)
        .iter()
        .map(|subset| format!("({})", all_terms_expr(subset)))
        .collect::<Vec<_>>()
        .join(" OR ")
}

fn count_matches_up_to(conn: &Connection, match_query: &str, cap: usize) -> Result<usize> {
    let count: i64 = conn
        .prepare_cached(
            "SELECT count(*) FROM (SELECT 1 FROM memory_fts WHERE memory_fts MATCH ?1 LIMIT ?2)",
        )?
        .query_row(params![match_query, cap as i64], |row| row.get(0))?;
    Ok(count as usize)
}

/// A single term found in at least half of a namespace has no positive BM25 weight, so ranking
/// every match only orders noise. Small namespaces are never classified this way.
fn is_non_discriminative_term(
    conn: &Connection,
    term_match: &str,
    scope_active: usize,
) -> Result<bool> {
    if scope_active < BROAD_TERM_PROBE_MATCHES
        || count_matches_up_to(conn, term_match, BROAD_TERM_PROBE_MATCHES)?
            < BROAD_TERM_PROBE_MATCHES
    {
        return Ok(false);
    }
    let threshold = scope_active.div_ceil(2).max(BROAD_TERM_PROBE_MATCHES);
    Ok(count_matches_up_to(conn, term_match, threshold)? >= threshold)
}

fn classify_candidate(
    content: &str,
    query_terms: &[String],
    full_terms: &[String],
) -> Option<(bool, usize)> {
    let content_terms = token_vec(content);
    let matched_terms = query_terms
        .iter()
        .filter(|term| {
            content_terms
                .iter()
                .any(|content_term| content_term == *term)
        })
        .count();
    if matched_terms < MIN_DISTINCT_TERM_MATCHES.min(query_terms.len()) {
        return None;
    }
    Some((
        contains_contiguous_terms(&content_terms, full_terms),
        matched_terms,
    ))
}

fn hydrate_memory(row: &rusqlite::Row<'_>) -> rusqlite::Result<MemoryRecord> {
    Ok(MemoryRecord {
        id: row.get(0)?,
        scope: row.get(1)?,
        kind: row.get(2)?,
        content: row.get(3)?,
        source_uri: row.get(4)?,
        source_id: row.get(5)?,
        supersedes_id: row.get(6)?,
        created_at: row.get(7)?,
        observed_at: row.get(8)?,
        valid_from: row.get(9)?,
        valid_to: row.get(10)?,
    })
}

fn stage_candidate(
    memory: MemoryRecord,
    rank: f64,
    kind: StageKind,
    classification: (bool, usize),
) -> RankedCandidate {
    let (exact_phrase, matched_terms) = classification;
    let score = match kind {
        StageKind::Ranked => (20.0 + (-rank * 1_000_000.0).clamp(0.0, 30.0)).clamp(0.0, 50.0),
        StageKind::Recency => 20.0,
    };
    (
        MemorySearchResult {
            memory,
            match_reason: if exact_phrase {
                "memory_exact_phrase".into()
            } else {
                "memory_lexical".into()
            },
            score,
            candidate_coverage: CandidateCoverage::Complete,
        },
        exact_phrase,
        matched_terms,
        rank,
    )
}

/// Collect up to `want` new rows from one FTS5 stage.
///
/// Rows arrive in the total order `(rank, rowid DESC)`, so the first `want` eligible rows in that
/// order are exactly the stage's top rows and exact BM25 ties are broken toward the most recently
/// remembered row regardless of insertion batching or content. `fetch` widens only when winners
/// were filtered out (other namespaces, expiry) or rejected by Rust-side term coverage.
///
/// Returns true when eligible rows were left unread.
#[allow(clippy::too_many_arguments)]
fn run_ranked_stage(
    conn: &Connection,
    match_query: &str,
    scope: &str,
    at: &str,
    want: usize,
    seen: &mut HashSet<i64>,
    query_terms: &[String],
    full_terms: &[String],
    collected: &mut Vec<RankedCandidate>,
    mut profile: Option<&mut MemoryRecallProfile>,
) -> Result<bool> {
    // Duplicates of earlier stages and a few filtered rows are absorbed without a second pass.
    let mut fetch = want + seen.len() + 8;
    loop {
        let phase = ProfileTimer::start(profile.as_deref_mut(), "sqlite_candidate_step");
        let mut statement = conn.prepare_cached(MEMORY_RANKED_SQL)?;
        let mut rows = statement.query(params![match_query, scope, at, fetch as i64])?;
        let (mut fetched, mut rejected) = (0usize, 0usize);
        let mut accepted: Vec<RankedCandidate> = Vec::new();
        while let Some(row) = rows.next().context("read memory search results")? {
            fetched += 1;
            let Some(id) = row.get::<_, Option<i64>>(0)? else {
                continue; // filtered by namespace, deletion, or validity
            };
            if seen.contains(&id) || accepted.len() > want {
                continue;
            }
            let memory = hydrate_memory(row)?;
            let rank: f64 = row.get(11)?;
            match classify_candidate(&memory.content, query_terms, full_terms) {
                Some(classification) => accepted.push(stage_candidate(
                    memory,
                    rank,
                    StageKind::Ranked,
                    classification,
                )),
                None => rejected += 1,
            }
        }
        drop(rows);
        if let Some(phase) = phase {
            phase.finish();
        }
        let exhausted = fetched < fetch;
        if accepted.len() >= want || exhausted {
            let overflow = accepted.len() > want;
            accepted.truncate(want);
            if let Some(profile) = profile.as_deref_mut() {
                profile.candidates_returned += fetched;
                profile.candidates_rejected_by_term_coverage += rejected;
                profile.candidates_hydrated += accepted.len();
                for candidate in &accepted {
                    profile.content_bytes_hydrated += candidate.0.memory.content.len();
                    profile.record_bytes_hydrated += memory_record_bytes(&candidate.0.memory);
                }
            }
            let more = overflow || (!exhausted && accepted.len() >= want);
            for candidate in accepted {
                seen.insert(candidate.0.memory.id);
                collected.push(candidate);
            }
            return Ok(more);
        }
        fetch = fetch.saturating_mul(4);
    }
}

/// Newest-first window for the opt-in `BroadTermPolicy::Recent`. Returns true when more
/// eligible rows existed than were returned.
#[allow(clippy::too_many_arguments)]
fn run_recency_stage(
    conn: &Connection,
    match_query: &str,
    scope: &str,
    at: &str,
    want: usize,
    seen: &mut HashSet<i64>,
    query_terms: &[String],
    full_terms: &[String],
    collected: &mut Vec<RankedCandidate>,
    mut profile: Option<&mut MemoryRecallProfile>,
) -> Result<bool> {
    let mut fetch = want + seen.len() + 1;
    loop {
        let phase = ProfileTimer::start(profile.as_deref_mut(), "sqlite_candidate_step");
        let mut statement = conn.prepare_cached(MEMORY_RECENCY_SQL)?;
        let mut rows = statement.query(params![match_query, scope, at, fetch as i64])?;
        let (mut fetched, mut rejected) = (0usize, 0usize);
        let mut accepted: Vec<RankedCandidate> = Vec::new();
        while let Some(row) = rows.next().context("read memory search results")? {
            fetched += 1;
            let id: i64 = row.get(0)?;
            if seen.contains(&id) {
                continue;
            }
            let memory = hydrate_memory(row)?;
            let rank: f64 = row.get(11)?;
            match classify_candidate(&memory.content, query_terms, full_terms) {
                Some(classification) => accepted.push(stage_candidate(
                    memory,
                    rank,
                    StageKind::Recency,
                    classification,
                )),
                None => rejected += 1,
            }
        }
        drop(rows);
        if let Some(phase) = phase {
            phase.finish();
        }
        let exhausted = fetched < fetch;
        if accepted.len() > want || exhausted {
            let overflow = accepted.len() > want;
            accepted.truncate(want);
            if let Some(profile) = profile.as_deref_mut() {
                profile.candidates_returned += fetched;
                profile.candidates_rejected_by_term_coverage += rejected;
                profile.candidates_hydrated += accepted.len();
                for candidate in &accepted {
                    profile.content_bytes_hydrated += candidate.0.memory.content.len();
                    profile.record_bytes_hydrated += memory_record_bytes(&candidate.0.memory);
                }
            }
            for candidate in accepted {
                seen.insert(candidate.0.memory.id);
                collected.push(candidate);
            }
            return Ok(overflow);
        }
        fetch = fetch.saturating_mul(2);
    }
}

/// Class-staged exact top-k retrieval.
///
/// Results are ordered by (exact phrase, distinct informative terms matched, BM25, tie-breaks).
/// Because the class dominates BM25, classes are enumerated best-first with FTS5 AND/phrase
/// queries and enumeration stops as soon as `limit` rows are collected. BM25 only orders rows
/// *within* a class, where the stage expression yields a score that is a uniform multiple of the
/// BM25 of the query terms. Every collected row therefore outranks every row not enumerated.
#[allow(clippy::too_many_arguments)]
fn recall_staged(
    conn: &Connection,
    scope: &str,
    at: &str,
    full_terms: &[String],
    query_terms: &[String],
    limit: usize,
    broad_terms: BroadTermPolicy,
    mut profile: Option<&mut MemoryRecallProfile>,
) -> Result<Vec<MemorySearchResult>> {
    let phase = ProfileTimer::start(profile.as_deref_mut(), "fts_query_construction");
    let terms = query_terms.iter().map(String::as_str).collect::<Vec<_>>();
    let term_count = terms.len();
    let (scope_active, total_active) = scope_sizes(conn, scope)?;
    let use_scope_term = scope_active * 2 < total_active;
    let mut stages: Vec<(String, StageKind, usize)> = Vec::new();
    let mut bounded_window = false;
    let mut recency_stage = false;
    if full_terms.len() > 1 {
        let phrase = format!("\"{}\"", full_terms.join(" "));
        stages.push((
            scoped_match(
                scope,
                &format!("{phrase} AND {}", all_terms_expr(&terms)),
                use_scope_term,
            ),
            StageKind::Ranked,
            limit,
        ));
    }
    if term_count == 1 {
        let term_match = scoped_match(scope, &all_terms_expr(&terms), use_scope_term);
        if broad_terms == BroadTermPolicy::Recent
            && is_non_discriminative_term(conn, &term_match, scope_active)?
        {
            recency_stage = true;
            stages.push((term_match, StageKind::Recency, limit));
        } else {
            stages.push((term_match, StageKind::Ranked, limit));
        }
    } else {
        stages.push((
            scoped_match(scope, &all_terms_expr(&terms), use_scope_term),
            StageKind::Ranked,
            limit,
        ));
        if term_count <= MAX_STAGED_CLASS_TERMS {
            for size in (MIN_DISTINCT_TERM_MATCHES..term_count).rev() {
                stages.push((
                    scoped_match(scope, &at_least_expr(&terms, size), use_scope_term),
                    StageKind::Ranked,
                    limit,
                ));
            }
        } else {
            // Long queries keep the uniform-weight v0.3.1 expression over a bounded window.
            bounded_window = true;
            let alternatives = terms
                .iter()
                .map(|term| format!("\"{term}\""))
                .collect::<Vec<_>>()
                .join(" OR ");
            stages.push((
                scoped_match(scope, &alternatives, use_scope_term),
                StageKind::Ranked,
                limit
                    .saturating_mul(MAX_RERANK_MULTIPLIER)
                    .min(MAX_RERANK_CANDIDATES)
                    .max(limit),
            ));
        }
    }
    if let Some(phase) = phase {
        phase.finish();
    }
    if let Some(profile) = profile.as_deref_mut() {
        profile.candidate_strategy = Some("staged_exact_bm25");
        profile.candidate_limit = limit;
    }

    let mut collected: Vec<RankedCandidate> = Vec::with_capacity(limit);
    let mut seen = HashSet::with_capacity(limit);
    let mut every_stage_complete = true;
    let mut recency_truncated = false;
    let stage_total = stages.len();
    for (index, (match_query, kind, window)) in stages.into_iter().enumerate() {
        let want = if window > limit {
            window
        } else {
            limit - collected.len()
        };
        let truncated = match kind {
            StageKind::Ranked => run_ranked_stage(
                conn,
                &match_query,
                scope,
                at,
                want,
                &mut seen,
                query_terms,
                full_terms,
                &mut collected,
                profile.as_deref_mut(),
            )?,
            StageKind::Recency => run_recency_stage(
                conn,
                &match_query,
                scope,
                at,
                want,
                &mut seen,
                query_terms,
                full_terms,
                &mut collected,
                profile.as_deref_mut(),
            )?,
        };
        if truncated {
            every_stage_complete = false;
            recency_truncated |= kind == StageKind::Recency;
        }
        if collected.len() >= limit && index + 1 < stage_total {
            every_stage_complete = false;
            break;
        }
    }
    let coverage = if recency_stage && recency_truncated {
        CandidateCoverage::RecencyWindow
    } else if every_stage_complete {
        CandidateCoverage::Complete
    } else if bounded_window {
        CandidateCoverage::Bm25TopK
    } else {
        CandidateCoverage::ExactTopK
    };
    if let Some(profile) = profile.as_deref_mut() {
        profile.candidate_budget_saturated = !every_stage_complete;
    }
    let phase = ProfileTimer::start(profile, "final_sort");
    collected.sort_by(compare_ranked_candidates);
    if let Some(phase) = phase {
        phase.finish();
    }
    Ok(collected
        .into_iter()
        .take(limit)
        .map(|(mut result, _, _, _)| {
            result.candidate_coverage = coverage;
            result
        })
        .collect())
}

fn finish_profile_total(
    profile: Option<&mut MemoryRecallProfile>,
    wall_started: Option<Instant>,
    cpu_started_ns: Option<u64>,
) {
    if let (Some(profile), Some(wall_started)) = (profile, wall_started) {
        let wall_ns = wall_started.elapsed().as_nanos().min(u64::MAX as u128) as u64;
        let cpu_ns = cpu_started_ns
            .zip(thread_cpu_time_ns())
            .map(|(started, finished)| finished.saturating_sub(started));
        profile.add_phase("total", wall_ns, cpu_ns);
    }
}

fn memory_record_bytes(memory: &MemoryRecord) -> usize {
    memory.scope.len()
        + memory.kind.len()
        + memory.content.len()
        + memory.source_uri.as_ref().map_or(0, String::len)
        + memory.created_at.len()
        + memory.observed_at.as_ref().map_or(0, String::len)
        + memory.valid_from.as_ref().map_or(0, String::len)
        + memory.valid_to.as_ref().map_or(0, String::len)
}

#[derive(Default)]
struct SqliteCacheSnapshot {
    hits: Option<u64>,
    misses: Option<u64>,
    writes: Option<u64>,
    spills: Option<u64>,
}

fn sqlite_cache_snapshot(connection: &Connection) -> SqliteCacheSnapshot {
    SqliteCacheSnapshot {
        hits: sqlite_db_status(connection, rusqlite::ffi::SQLITE_DBSTATUS_CACHE_HIT),
        misses: sqlite_db_status(connection, rusqlite::ffi::SQLITE_DBSTATUS_CACHE_MISS),
        writes: sqlite_db_status(connection, rusqlite::ffi::SQLITE_DBSTATUS_CACHE_WRITE),
        spills: sqlite_db_status(connection, rusqlite::ffi::SQLITE_DBSTATUS_CACHE_SPILL),
    }
}

fn sqlite_db_status(connection: &Connection, operation: i32) -> Option<u64> {
    let mut current = 0;
    let mut highwater = 0;
    let result = unsafe {
        rusqlite::ffi::sqlite3_db_status(
            connection.handle(),
            operation,
            &mut current,
            &mut highwater,
            0,
        )
    };
    (result == rusqlite::ffi::SQLITE_OK).then_some(current.max(0) as u64)
}

#[cfg(feature = "sqlite-scanstatus")]
fn sqlite_scanstatus_enabled(connection: &Connection) -> bool {
    let compiled: bool = connection
        .query_row(
            "SELECT sqlite_compileoption_used('ENABLE_STMT_SCANSTATUS')",
            [],
            |row| row.get(0),
        )
        .unwrap_or(false);
    compiled
        && connection
            .set_db_config(
                rusqlite::config::DbConfig::SQLITE_DBCONFIG_STMT_SCANSTATUS,
                true,
            )
            .unwrap_or(false)
}

#[cfg(not(feature = "sqlite-scanstatus"))]
fn sqlite_scanstatus_enabled(_connection: &Connection) -> bool {
    false
}

fn sqlite_statement_profile(connection: &Connection, sql: &str, profile: &mut MemoryRecallProfile) {
    let statement = unsafe { sqlite_statement_for_sql(connection, sql) };
    if statement.is_null() {
        return;
    }
    unsafe {
        let status = |operation| {
            let value = rusqlite::ffi::sqlite3_stmt_status(statement, operation, 0);
            (value >= 0).then_some(value as u64)
        };
        add_sqlite_counter(
            &mut profile.sqlite_vm_steps,
            status(rusqlite::ffi::SQLITE_STMTSTATUS_VM_STEP),
        );
        add_sqlite_counter(
            &mut profile.sqlite_fullscan_steps,
            status(rusqlite::ffi::SQLITE_STMTSTATUS_FULLSCAN_STEP),
        );
        add_sqlite_counter(
            &mut profile.sqlite_sort_operations,
            status(rusqlite::ffi::SQLITE_STMTSTATUS_SORT),
        );
        add_sqlite_counter(
            &mut profile.sqlite_statement_memory_bytes,
            status(rusqlite::ffi::SQLITE_STMTSTATUS_MEMUSED),
        );

        #[cfg(feature = "sqlite-scanstatus")]
        {
            let mut loops = Vec::new();
            let mut index = 0;
            loop {
                let mut count = 0i64;
                let result = rusqlite::ffi::sqlite3_stmt_scanstatus_v2(
                    statement,
                    index,
                    rusqlite::ffi::SQLITE_SCANSTAT_NLOOP,
                    0,
                    &mut count as *mut i64 as *mut c_void,
                );
                if result != rusqlite::ffi::SQLITE_OK {
                    if index == 0 {
                        profile.sqlite_scanstatus_available = false;
                        profile.sqlite_scan_loops = None;
                    } else {
                        merge_scan_loops(&mut profile.sqlite_scan_loops, loops);
                    }
                    return;
                }
                let mut visits = 0i64;
                let visits_result = rusqlite::ffi::sqlite3_stmt_scanstatus_v2(
                    statement,
                    index,
                    rusqlite::ffi::SQLITE_SCANSTAT_NVISIT,
                    0,
                    &mut visits as *mut i64 as *mut c_void,
                );
                let mut name: *const std::ffi::c_char = ptr::null();
                let name_result = rusqlite::ffi::sqlite3_stmt_scanstatus_v2(
                    statement,
                    index,
                    rusqlite::ffi::SQLITE_SCANSTAT_NAME,
                    0,
                    &mut name as *mut *const std::ffi::c_char as *mut c_void,
                );
                if visits_result != rusqlite::ffi::SQLITE_OK
                    || name_result != rusqlite::ffi::SQLITE_OK
                {
                    profile.sqlite_scanstatus_available = false;
                    profile.sqlite_scan_loops = None;
                    return;
                }
                let name = if name.is_null() {
                    "unknown".to_string()
                } else {
                    CStr::from_ptr(name).to_string_lossy().into_owned()
                };
                loops.push(SqliteScanLoop {
                    name,
                    loops: count,
                    visits,
                });
                index += 1;
            }
        }
        #[cfg(not(feature = "sqlite-scanstatus"))]
        {
            profile.sqlite_scanstatus_available = false;
            profile.sqlite_scan_loops = None;
        }
    }
}

fn add_sqlite_counter(total: &mut Option<u64>, value: Option<u64>) {
    *total = match (*total, value) {
        (Some(total), Some(value)) => Some(total.saturating_add(value)),
        (None, value) => value,
        (total, None) => total,
    };
}

#[cfg(feature = "sqlite-scanstatus")]
fn merge_scan_loops(total: &mut Option<Vec<SqliteScanLoop>>, incoming: Vec<SqliteScanLoop>) {
    let total = total.get_or_insert_with(Vec::new);
    for incoming in incoming {
        if let Some(existing) = total.iter_mut().find(|entry| entry.name == incoming.name) {
            existing.loops += incoming.loops;
            existing.visits += incoming.visits;
        } else {
            total.push(incoming);
        }
    }
}

fn sqlite_reset_statement_profile(connection: &Connection, sql: &str) {
    let statement = unsafe { sqlite_statement_for_sql(connection, sql) };
    if statement.is_null() {
        return;
    }
    unsafe {
        for operation in [
            rusqlite::ffi::SQLITE_STMTSTATUS_VM_STEP,
            rusqlite::ffi::SQLITE_STMTSTATUS_FULLSCAN_STEP,
            rusqlite::ffi::SQLITE_STMTSTATUS_SORT,
            rusqlite::ffi::SQLITE_STMTSTATUS_REPREPARE,
            rusqlite::ffi::SQLITE_STMTSTATUS_RUN,
        ] {
            rusqlite::ffi::sqlite3_stmt_status(statement, operation, 1);
        }
        #[cfg(feature = "sqlite-scanstatus")]
        rusqlite::ffi::sqlite3_stmt_scanstatus_reset(statement);
    }
}

unsafe fn sqlite_statement_for_sql(
    connection: &Connection,
    expected_sql: &str,
) -> *mut rusqlite::ffi::sqlite3_stmt {
    let database = unsafe { connection.handle() };
    let mut statement = unsafe { rusqlite::ffi::sqlite3_next_stmt(database, ptr::null_mut()) };
    while !statement.is_null() {
        let actual_sql = unsafe { rusqlite::ffi::sqlite3_sql(statement) };
        if !actual_sql.is_null()
            && unsafe { CStr::from_ptr(actual_sql) }.to_bytes() == expected_sql.as_bytes()
        {
            return statement;
        }
        statement = unsafe { rusqlite::ffi::sqlite3_next_stmt(database, statement) };
    }
    ptr::null_mut()
}

pub fn list_memories(db_path: &Path, scope: &str, limit: usize) -> Result<Vec<MemoryRecord>> {
    let conn = open_db(db_path)?;
    list_memories_on(&conn, scope, limit)
}

fn list_memories_on(conn: &Connection, scope: &str, limit: usize) -> Result<Vec<MemoryRecord>> {
    validate_scope(scope)?;
    let mut stmt = conn.prepare_cached(
        "SELECT id,scope,kind,content,source_uri,source_id,supersedes_id,created_at,observed_at,valid_from,valid_to
         FROM memories WHERE scope=?1 AND deleted_at IS NULL ORDER BY created_at DESC,id DESC LIMIT ?2",
    )?;
    let rows = stmt.query_map(params![scope, limit.clamp(1, 1000) as i64], |row| {
        Ok(MemoryRecord {
            id: row.get(0)?,
            scope: row.get(1)?,
            kind: row.get(2)?,
            content: row.get(3)?,
            source_uri: row.get(4)?,
            source_id: row.get(5)?,
            supersedes_id: row.get(6)?,
            created_at: row.get(7)?,
            observed_at: row.get(8)?,
            valid_from: row.get(9)?,
            valid_to: row.get(10)?,
        })
    })?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .context("list memories")
}

pub fn forget(db_path: &Path, scope: &str, id: i64) -> Result<bool> {
    let conn = open_db(db_path)?;
    forget_on(&conn, scope, id)
}

fn forget_on(conn: &Connection, scope: &str, id: i64) -> Result<bool> {
    validate_scope(scope)?;
    let changed = conn
        .prepare_cached(
            "UPDATE memories SET deleted_at=?3 WHERE id=?1 AND scope=?2 AND deleted_at IS NULL",
        )?
        .execute(params![id, scope, now_utc()])?;
    Ok(changed != 0)
}

pub fn compact(db_path: &Path) -> Result<()> {
    let conn = open_db(db_path)?;
    conn.execute(
        "UPDATE memories SET content='',source_uri=NULL WHERE deleted_at IS NOT NULL",
        [],
    )?;
    conn.execute_batch("INSERT INTO code_fts(code_fts) VALUES('optimize'); INSERT INTO memory_fts(memory_fts) VALUES('optimize'); PRAGMA wal_checkpoint(TRUNCATE); VACUUM;")?;
    Ok(())
}

fn validate_scope(scope: &str) -> Result<()> {
    if scope.trim().is_empty() || scope.len() > 256 || scope.contains('\0') {
        bail!("memory scope must be a non-empty key of at most 256 bytes");
    }
    if scope != scope.trim() {
        bail!("memory scope cannot have leading or trailing whitespace");
    }
    if !scope.contains(':') {
        bail!("scope must be namespaced, for example user:alice or project:my-app");
    }
    Ok(())
}

pub(crate) fn memory_match_query(
    scope: &str,
    query_terms: &[String],
    full_terms: &[String],
) -> String {
    let alternatives = query_terms
        .iter()
        .map(|term| format!("\"{term}\""))
        .collect::<Vec<_>>()
        .join(" OR ");
    let content_query = if full_terms.len() > 1 {
        let phrase = format!("\"{}\"", full_terms.join(" "));
        format!("{phrase} OR ({alternatives})")
    } else {
        alternatives
    };
    format!(
        "scope_token : \"{}\" AND content : ({content_query})",
        memory_scope_token(scope)
    )
}

pub(crate) fn normalized_tokens(text: &str) -> Vec<String> {
    let folded = text
        .nfkd()
        .filter(|character| !is_combining_mark(*character))
        .flat_map(char::to_lowercase)
        .collect::<String>();
    folded
        .split(|character: char| !character.is_alphanumeric())
        .filter(|term| !term.is_empty())
        .map(str::to_string)
        .collect()
}

pub(crate) fn informative_terms(terms: &[String]) -> Vec<String> {
    const STOP_WORDS: &[&str] = &[
        "a", "an", "and", "are", "as", "at", "be", "before", "but", "by", "can", "did", "do",
        "does", "for", "from", "had", "has", "have", "he", "her", "his", "how", "i", "in", "is",
        "it", "its", "me", "my", "of", "on", "or", "our", "she", "should", "s", "that", "the",
        "their", "them", "then", "there", "these", "they", "this", "those", "to", "us", "was",
        "we", "were", "what", "when", "where", "which", "who", "why", "will", "with", "would",
        "you", "your", "aquel", "aquella", "aquellas", "aquellos", "como", "con", "cual", "cuales",
        "cuando", "de", "del", "desde", "donde", "el", "ella", "ellas", "ellos", "en", "era",
        "eran", "es", "esa", "esas", "ese", "eso", "esos", "esta", "estas", "este", "esto",
        "estos", "fue", "fueron", "ha", "han", "hasta", "la", "las", "le", "les", "lo", "los",
        "me", "mi", "mis", "o", "para", "por", "que", "se", "sin", "su", "sus", "un", "una",
        "unas", "uno", "unos", "y",
    ];
    let mut seen = HashSet::new();
    terms
        .iter()
        .filter(|term| !STOP_WORDS.contains(&term.as_str()) && seen.insert(term.as_str()))
        .cloned()
        .collect()
}

pub(crate) fn token_vec(text: &str) -> Vec<String> {
    normalized_tokens(text)
}

pub(crate) fn contains_contiguous_terms(content: &[String], query: &[String]) -> bool {
    !query.is_empty()
        && query.len() <= content.len()
        && content
            .windows(query.len())
            .any(|window| window.iter().eq(query.iter()))
}
