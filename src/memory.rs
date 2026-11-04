use anyhow::{Context, Result, bail};
use rusqlite::{OptionalExtension, params};
use serde::Serialize;
use std::path::Path;

use crate::db::{normalize_timestamp, now_utc, open_db};

pub const MAX_MEMORY_BYTES: usize = 64 * 1024;

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
    validate_scope(scope)?;
    let observed_at = observed_at.map(normalize_timestamp).transpose()?;
    let valid_from = valid_from.map(normalize_timestamp).transpose()?;
    let valid_to = valid_to.map(normalize_timestamp).transpose()?;
    if !matches!(kind, "source" | "derived") {
        bail!("kind must be 'source' or 'derived'");
    }
    if content.trim().is_empty() {
        bail!("memory content cannot be empty");
    }
    if content.len() > MAX_MEMORY_BYTES {
        bail!("memory content exceeds 64 KiB");
    }
    if let (Some(from), Some(to)) = (valid_from.as_deref(), valid_to.as_deref())
        && from >= to
    {
        bail!("valid_to must be later than valid_from");
    }
    if kind == "source" && source_id.is_some() {
        bail!("raw source memories cannot link to another source id");
    }
    if kind == "derived" && source_id.is_none() && source_uri.is_none() {
        // A derived fact may be caller-authored, but provenance must remain explicit.
        // source_uri is accepted as the provenance pointer in that case.
        bail!("derived memories require source_id or source_uri provenance");
    }
    if supersedes_id.is_some() && (kind != "derived" || valid_from.is_none()) {
        bail!("superseding a fact requires kind=derived and an explicit valid_from");
    }
    let mut conn = open_db(db_path)?;
    let tx = conn.transaction()?;
    if let Some(source_id) = source_id {
        let valid: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM memories WHERE id=?1 AND scope=?2 AND kind='source' AND deleted_at IS NULL)",
            params![source_id,scope], |row| row.get(0),
        )?;
        if !valid {
            bail!("source_id must name an active raw source in the same memory scope");
        }
    }
    if let Some(supersedes_id) = supersedes_id {
        let prior: Option<(Option<String>, Option<String>)> = tx
            .query_row(
                "SELECT valid_from,valid_to FROM memories WHERE id=?1 AND scope=?2 AND kind='derived' AND deleted_at IS NULL",
                params![supersedes_id,scope],
                |row| Ok((row.get(0)?,row.get(1)?)),
            )
            .optional()?;
        let Some((prior_from, prior_to)) = prior else {
            bail!("supersedes_id must name an active derived fact in the same scope");
        };
        let new_from = valid_from.as_deref().expect("validated above");
        if prior_from.as_deref().is_some_and(|from| from >= new_from) {
            bail!("replacement valid_from must be later than the prior fact's start");
        }
        if prior_to.as_deref().is_none_or(|to| to > new_from) {
            tx.execute(
                "UPDATE memories SET valid_to=?3 WHERE id=?1 AND scope=?2",
                params![supersedes_id, scope, new_from],
            )?;
        }
    }
    let created_at = now_utc();
    let id: i64 = tx.query_row(
        "INSERT INTO memories(scope,kind,content,source_uri,source_id,supersedes_id,created_at,observed_at,valid_from,valid_to)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10) RETURNING id",
        params![scope,kind,content,source_uri,source_id,supersedes_id,created_at,observed_at,valid_from,valid_to],
        |row| row.get(0),
    )?;
    tx.execute(
        "INSERT INTO memory_fts(rowid,content) VALUES(?1,?2)",
        params![id, content],
    )?;
    tx.commit()?;
    Ok(MemoryRecord {
        id,
        scope: scope.to_string(),
        kind: kind.to_string(),
        content: content.to_string(),
        source_uri: source_uri.map(str::to_string),
        source_id,
        supersedes_id,
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
    validate_scope(scope)?;
    if query.trim().is_empty() {
        bail!("recall query cannot be empty");
    }
    let match_query = memory_match_query(query);
    let conn = open_db(db_path)?;
    let at = at
        .map(normalize_timestamp)
        .transpose()?
        .unwrap_or_else(now_utc);
    let mut stmt = conn.prepare(
        "SELECT m.id,m.scope,m.kind,m.content,m.source_uri,m.source_id,m.supersedes_id,m.created_at,m.observed_at,m.valid_from,m.valid_to,bm25(memory_fts)
         FROM memory_fts JOIN memories m ON m.id=memory_fts.rowid
         WHERE memory_fts MATCH ?1 AND m.scope=?2 AND m.deleted_at IS NULL
           AND (m.valid_from IS NULL OR m.valid_from<=?3)
           AND (m.valid_to IS NULL OR m.valid_to>?3)
         ORDER BY bm25(memory_fts),m.created_at DESC,m.id DESC LIMIT ?4",
    )?;
    let rows = stmt.query_map(
        params![match_query, scope, at, limit.clamp(1, 100) as i64],
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
            Ok(MemorySearchResult {
                memory,
                match_reason: "memory_lexical".into(),
                score: (20.0 + (-rank * 1_000_000.0).clamp(0.0, 30.0)).clamp(0.0, 50.0),
            })
        },
    )?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .context("read memory search results")
}

pub fn list_memories(db_path: &Path, scope: &str, limit: usize) -> Result<Vec<MemoryRecord>> {
    validate_scope(scope)?;
    let conn = open_db(db_path)?;
    let mut stmt = conn.prepare(
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
    validate_scope(scope)?;
    let conn = open_db(db_path)?;
    let changed = conn.execute(
        "UPDATE memories SET deleted_at=?3 WHERE id=?1 AND scope=?2 AND deleted_at IS NULL",
        params![id, scope, now_utc()],
    )?;
    Ok(changed != 0)
}

pub fn compact(db_path: &Path) -> Result<()> {
    let conn = open_db(db_path)?;
    conn.execute_batch("INSERT INTO code_trigram(code_trigram) VALUES('optimize'); INSERT INTO memory_fts(memory_fts) VALUES('optimize'); PRAGMA wal_checkpoint(TRUNCATE); VACUUM;")?;
    Ok(())
}

fn validate_scope(scope: &str) -> Result<()> {
    let scope = scope.trim();
    if scope.is_empty() || scope.len() > 256 || scope.contains('\0') {
        bail!("memory scope must be a non-empty key of at most 256 bytes");
    }
    if !scope.contains(':') {
        bail!("scope must be namespaced, for example user:alice or project:my-app");
    }
    Ok(())
}

fn memory_match_query(query: &str) -> String {
    query
        .split(|c: char| !(c.is_alphanumeric() || c == '_'))
        .filter(|term| !term.is_empty())
        .map(|term| format!("\"{}\"", term.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(" OR ")
}
