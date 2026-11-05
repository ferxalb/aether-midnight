use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::Path;
use unicode_normalization::{UnicodeNormalization, char::is_combining_mark};

use crate::db::{memory_scope_token, normalize_timestamp, now_utc, open_db};

pub const MAX_MEMORY_BYTES: usize = 64 * 1024;
pub const MAX_MEMORY_BATCH_RECORDS: usize = 512;
const MAX_MEMORY_BATCH_BYTES: usize = 16 * 1024 * 1024;
const MAX_MEMORY_QUERY_TERMS: usize = 64;
const MAX_RERANK_MULTIPLIER: usize = 5;
const MAX_RERANK_CANDIDATES: usize = 500;
const MIN_DISTINCT_TERM_MATCHES: usize = 3;
const MEMORY_RECALL_SQL: &str = "SELECT m.id,m.scope,m.kind,m.content,m.source_uri,m.source_id,m.supersedes_id,m.created_at,m.observed_at,m.valid_from,m.valid_to,memory_fts.rank
         FROM memory_fts JOIN memories m ON m.id=memory_fts.rowid
         WHERE memory_fts MATCH ?1 AND memory_fts.rank MATCH 'bm25(0.0,1.0)'
           AND m.scope=?2 AND m.deleted_at IS NULL
           AND (m.valid_from IS NULL OR m.valid_from<=?3)
           AND (m.valid_to IS NULL OR m.valid_to>?3)
         ORDER BY memory_fts.rank LIMIT ?4";

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

    pub(crate) fn list_memories(&self, scope: &str, limit: usize) -> Result<Vec<MemoryRecord>> {
        list_memories_on(&self.connection, scope, limit)
    }

    pub(crate) fn forget(&self, scope: &str, id: i64) -> Result<bool> {
        forget_on(&self.connection, scope, id)
    }

    pub(crate) fn recall_query_plan(&self, scope: &str, query: &str) -> Result<Vec<String>> {
        validate_scope(scope)?;
        let full_terms = normalized_tokens(query);
        let query_terms = informative_terms(&full_terms);
        if query_terms.is_empty() {
            return Ok(Vec::new());
        }
        let match_query = memory_match_query(scope, &query_terms, &full_terms);
        let mut statement = self
            .connection
            .prepare(&format!("EXPLAIN QUERY PLAN {MEMORY_RECALL_SQL}"))?;
        let rows = statement.query_map(params![match_query, scope, now_utc(), 10i64], |row| {
            row.get(3)
        })?;
        rows.collect::<rusqlite::Result<Vec<String>>>()
            .map_err(Into::into)
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
    let full_terms = normalized_tokens(query);
    if full_terms.len() > MAX_MEMORY_QUERY_TERMS {
        bail!("memory recall query contains more than {MAX_MEMORY_QUERY_TERMS} searchable terms");
    }
    let query_terms = informative_terms(&full_terms);
    if query_terms.is_empty() {
        return Ok(Vec::new());
    }
    let match_query = memory_match_query(scope, &query_terms, &full_terms);
    let result_limit = limit.clamp(1, 100);
    let candidate_limit = result_limit
        .saturating_mul(MAX_RERANK_MULTIPLIER)
        .min(MAX_RERANK_CANDIDATES)
        .max(result_limit);
    let mut stmt = conn.prepare_cached(MEMORY_RECALL_SQL)?;
    let rows = stmt.query_map(
        params![match_query, scope, at, candidate_limit as i64],
        |row| {
            let memory = MemoryRecord {
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
            };
            let rank: f64 = row.get(11)?;
            Ok((
                MemorySearchResult {
                    memory,
                    match_reason: "memory_lexical".into(),
                    score: (20.0 + (-rank * 1_000_000.0).clamp(0.0, 30.0)).clamp(0.0, 50.0),
                },
                rank,
            ))
        },
    )?;
    let candidates = rows
        .collect::<rusqlite::Result<Vec<_>>>()
        .context("read memory search results")?;
    let mut ranked = candidates
        .into_iter()
        .filter_map(|(mut result, rank)| {
            let content_terms = token_vec(&result.memory.content);
            let matched_terms = query_terms
                .iter()
                .filter(|term| {
                    content_terms
                        .iter()
                        .any(|content_term| content_term == *term)
                })
                .count();
            let required_matches = MIN_DISTINCT_TERM_MATCHES.min(query_terms.len());
            if matched_terms < required_matches {
                return None;
            }
            let exact_phrase = contains_contiguous_terms(&content_terms, &full_terms);
            if exact_phrase {
                result.match_reason = "memory_exact_phrase".into();
            }
            Some((result, exact_phrase, matched_terms, rank))
        })
        .collect::<Vec<_>>();
    ranked.sort_by(|left, right| {
        right
            .1
            .cmp(&left.1)
            .then_with(|| right.2.cmp(&left.2))
            .then_with(|| left.3.total_cmp(&right.3))
            .then_with(|| right.0.memory.created_at.cmp(&left.0.memory.created_at))
            .then_with(|| right.0.memory.id.cmp(&left.0.memory.id))
    });
    Ok(ranked
        .into_iter()
        .take(result_limit)
        .map(|(result, _, _, _)| result)
        .collect())
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

fn memory_match_query(scope: &str, query_terms: &[String], full_terms: &[String]) -> String {
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

fn normalized_tokens(text: &str) -> Vec<String> {
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

fn informative_terms(terms: &[String]) -> Vec<String> {
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

fn token_vec(text: &str) -> Vec<String> {
    normalized_tokens(text)
}

fn contains_contiguous_terms(content: &[String], query: &[String]) -> bool {
    !query.is_empty()
        && query.len() <= content.len()
        && content
            .windows(query.len())
            .any(|window| window.iter().eq(query.iter()))
}
