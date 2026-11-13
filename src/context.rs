//! Token-saving response shaping for agents (design: `docs/design/v0.2-token-savings.md`).
//!
//! Everything here runs *after* retrieval: candidate generation, ranking and evidence classes are
//! untouched. The renderer only decides the detail level of each result (`full` ranges, a
//! `signature` line plus the match, or a bare `reference`) and where to cut the tail, under the
//! caller's hard `token_budget`. Result order is never changed.
//!
//! * Handles are stateless: `path#start-end@hash12`. A handle whose hash no longer matches the
//!   file is reported as stale and never served as fresh content.
//! * The [`DeliveryLedger`] only shortens responses (a range the client already received with the
//!   same content hash becomes a reference). It stores paths, ranges and hashes — never queries.
use std::cell::RefCell;
use std::collections::{HashSet, VecDeque};
use std::rc::Rc;

use anyhow::{Result, bail};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};

use crate::code::{self, CodeResult, CodeSearchResponse, estimate_tokens};
use crate::db::RepoScope;

/// Hex characters of the content hash carried by a handle.
pub const HANDLE_HASH_LEN: usize = 12;
/// Most ranges one result may carry; adjacent or overlapping ranges are merged first.
pub const MAX_RANGES_PER_FILE: usize = 2;
/// Entries the per-connection ledger remembers (least recently recorded evicted first).
pub const LEDGER_CAPACITY: usize = 512;
const MAX_HANDLE_RANGE_LINES: u32 = 200;
pub const COMPACT_SHAPE: &str = "compact-v1";

// ---------- handles ----------

#[derive(Debug, PartialEq, Eq)]
pub struct Handle {
    pub path: String,
    pub start: u32,
    pub end: u32,
    pub hash_prefix: String,
}

pub fn make_handle(path: &str, start: u32, end: u32, content_hash: &str) -> String {
    let prefix: String = content_hash.chars().take(HANDLE_HASH_LEN).collect();
    format!("{path}#{start}-{end}@{prefix}")
}

pub fn parse_handle(text: &str) -> Result<Handle> {
    let (left, hash) = text
        .rsplit_once('@')
        .ok_or_else(|| anyhow::anyhow!("invalid ref: expected path#start-end@hash"))?;
    let (path, range) = left
        .rsplit_once('#')
        .ok_or_else(|| anyhow::anyhow!("invalid ref: expected path#start-end@hash"))?;
    let (start, end) = range
        .split_once('-')
        .ok_or_else(|| anyhow::anyhow!("invalid ref: line range must look like 10-24"))?;
    let (start, end) = (
        start
            .parse::<u32>()
            .map_err(|_| anyhow::anyhow!("invalid ref: bad start line"))?,
        end.parse::<u32>()
            .map_err(|_| anyhow::anyhow!("invalid ref: bad end line"))?,
    );
    if path.is_empty() || hash.is_empty() || !hash.chars().all(|c| c.is_ascii_hexdigit()) {
        bail!("invalid ref: expected path#start-end@hash");
    }
    Ok(Handle {
        path: path.to_string(),
        start,
        end,
        hash_prefix: hash.to_ascii_lowercase(),
    })
}

// ---------- delivery ledger ----------

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LedgerKey {
    pub scope: String,
    pub path: String,
    pub start: u32,
    pub end: u32,
    pub hash: String,
}

/// What a connection has already been sent. Implementations must be bounded and must not store
/// query text. The content hash is part of the key, so a changed file is never "delivered".
pub trait DeliveryLedger {
    fn delivered(&self, key: &LedgerKey) -> bool;
    fn record(&mut self, key: LedgerKey);
    fn clear(&mut self);
}

#[derive(Debug)]
pub struct MemoryLedger {
    capacity: usize,
    seen: HashSet<LedgerKey>,
    order: VecDeque<LedgerKey>,
}

impl Default for MemoryLedger {
    fn default() -> Self {
        Self::with_capacity(LEDGER_CAPACITY)
    }
}

impl MemoryLedger {
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            seen: HashSet::new(),
            order: VecDeque::new(),
        }
    }
    pub fn len(&self) -> usize {
        self.seen.len()
    }
    pub fn is_empty(&self) -> bool {
        self.seen.is_empty()
    }
}

impl DeliveryLedger for MemoryLedger {
    fn delivered(&self, key: &LedgerKey) -> bool {
        self.seen.contains(key)
    }
    fn record(&mut self, key: LedgerKey) {
        if self.seen.insert(key.clone()) {
            self.order.push_back(key);
            while self.order.len() > self.capacity {
                if let Some(old) = self.order.pop_front() {
                    self.seen.remove(&old);
                }
            }
        }
    }
    fn clear(&mut self) {
        self.seen.clear();
        self.order.clear();
    }
}

/// Ledger shared by the tool handlers of one connection (single-threaded, like `Session`).
pub type SharedLedger = Rc<RefCell<MemoryLedger>>;

// ---------- compact search response ----------

#[derive(Debug, Clone)]
pub struct CompactOptions {
    pub token_budget: usize,
    pub limit: usize,
    pub fresh_context: bool,
    pub explain: bool,
}

struct Symbol {
    label: String,
    start: u32,
    end: u32,
}

struct Range {
    start: u32,
    end: u32,
    /// Numbered lines: signature segment plus the match window.
    text: String,
    /// Numbered lines of the signature level (symbol signature and the matching line).
    short: String,
    handle: String,
    delivered: bool,
}

struct Item {
    class: u8,
    score: f64,
    levels: [Value; 3],
    tokens: [usize; 3],
    level: usize,
    record: Vec<LedgerKey>,
}

fn enclosing_symbol(conn: &Connection, scope: &RepoScope, path: &str, line: u32) -> Option<Symbol> {
    conn.prepare_cached(
        "SELECT s.kind,s.name,s.line_start,s.line_end FROM code_symbols s
         JOIN code_files f ON f.id=s.file_id
         WHERE f.repo_id=?1 AND f.worktree_id=?2 AND f.rel_path=?3
           AND s.line_start<=?4 AND s.line_end>=?4
         ORDER BY (s.line_end-s.line_start) ASC, s.line_start DESC LIMIT 1",
    )
    .ok()?
    .query_row(
        params![scope.repo_id, scope.worktree_id, path, line],
        |row| {
            Ok(Symbol {
                label: format!("{} {}", row.get::<_, String>(0)?, row.get::<_, String>(1)?),
                start: row.get::<_, i64>(2)? as u32,
                end: row.get::<_, i64>(3)? as u32,
            })
        },
    )
    .optional()
    .ok()
    .flatten()
}

/// Line numbers (1-based) of up to `max` non-overlapping match windows, using the same needle the
/// excerpt used: the whole query if present, otherwise the first query term that occurs.
fn match_lines(text: &str, query: &str, first_line: u32, window: u32, max: usize) -> Vec<u32> {
    let mut lines = vec![first_line];
    let query = query.trim().to_ascii_lowercase();
    if query.is_empty() || max < 2 {
        return lines;
    }
    let lower = text.to_ascii_lowercase();
    let needle = if lower.contains(&query) {
        query
    } else {
        match query.split_whitespace().find(|t| lower.contains(*t)) {
            Some(t) => t.to_string(),
            None => return lines,
        }
    };
    let mut covered_until = first_line.saturating_add(window.saturating_sub(1));
    let mut line = 1u32;
    let mut last_offset = 0usize;
    for (offset, _) in lower.match_indices(&needle) {
        line += lower.as_bytes()[last_offset..offset]
            .iter()
            .filter(|&&b| b == b'\n')
            .count() as u32;
        last_offset = offset;
        if line > covered_until {
            lines.push(line);
            covered_until = line.saturating_add(window.saturating_sub(1));
            if lines.len() >= max {
                break;
            }
        }
    }
    lines
}

#[allow(clippy::too_many_arguments)]
fn build_ranges(
    conn: &Connection,
    scope: &RepoScope,
    result: &CodeResult,
    text: &str,
    hash: &str,
    query: &str,
    window: u32,
    ledger: &dyn DeliveryLedger,
    opts: &CompactOptions,
) -> (Option<String>, Vec<Range>, Vec<LedgerKey>) {
    let total = text.lines().count().max(1) as u32;
    let first = result.line_start.unwrap_or(1).clamp(1, total);
    let starts = match_lines(text, query, first, window, MAX_RANGES_PER_FILE);
    let mut ranges: Vec<Range> = Vec::new();
    let mut symbol_label = None;
    let mut first_symbol_start: Option<u32> = None;
    for (index, &m) in starts.iter().enumerate() {
        let symbol = enclosing_symbol(conn, scope, &result.path, m);
        let mut window_end = (m + window - 1).min(total);
        let mut sig: Option<(u32, u32)> = None;
        if let Some(sym) = &symbol {
            window_end = window_end.min(sym.end.max(m));
            let same_as_first = index > 0 && first_symbol_start == Some(sym.start);
            if index == 0 {
                symbol_label = Some(sym.label.clone());
                first_symbol_start = Some(sym.start);
            }
            if sym.start < m && !same_as_first {
                // Signature: the symbol's opening line and, when room allows, the next one.
                sig = Some((sym.start, (sym.start + 1).min(m - 1)));
            }
        }
        let start = sig.map_or(m, |(s, _)| s);
        let line = |a: u32, b: u32| code::snippet_lines(text, a, b, None).0;
        let (full, short) = match sig {
            Some((s, e)) => {
                let head = line(s, e);
                let short = if m > e {
                    format!("{head}\n{}", line(m, m))
                } else {
                    head
                };
                let full = if e + 1 >= m {
                    line(s, window_end)
                } else {
                    format!(
                        "{short_head}\n{}",
                        line(m, window_end),
                        short_head = line(s, e)
                    )
                };
                (full, short)
            }
            None => (line(m, window_end), line(m, m)),
        };
        // Merge with the previous range when the lines touch or overlap.
        if let Some(prev) = ranges.last_mut()
            && start <= prev.end + 2
        {
            if window_end > prev.end {
                let extra = line(prev.end + 1, window_end);
                if !extra.is_empty() {
                    prev.text.push('\n');
                    prev.text.push_str(&extra);
                }
                prev.end = window_end;
                prev.handle = make_handle(
                    &result.path,
                    prev.start,
                    prev.end.min(prev.start + MAX_HANDLE_RANGE_LINES - 1),
                    hash,
                );
            }
            continue;
        }
        let end = window_end;
        let handle = make_handle(
            &result.path,
            start,
            end.min(start + MAX_HANDLE_RANGE_LINES - 1),
            hash,
        );
        ranges.push(finish_range(
            scope,
            &result.path,
            start,
            end,
            full,
            short,
            handle,
            hash,
            ledger,
            opts,
        ));
    }
    let record = ranges
        .iter()
        .filter(|r| !r.delivered)
        .map(|r| LedgerKey {
            scope: scope_key(scope),
            path: result.path.clone(),
            start: r.start,
            end: r.end,
            hash: hash.to_string(),
        })
        .collect();
    (symbol_label, ranges, record)
}

#[allow(clippy::too_many_arguments)]
fn finish_range(
    scope: &RepoScope,
    path: &str,
    start: u32,
    end: u32,
    text: String,
    short: String,
    handle: String,
    hash: &str,
    ledger: &dyn DeliveryLedger,
    opts: &CompactOptions,
) -> Range {
    let delivered = !opts.fresh_context
        && ledger.delivered(&LedgerKey {
            scope: scope_key(scope),
            path: path.to_string(),
            start,
            end,
            hash: hash.to_string(),
        });
    Range {
        start,
        end,
        text,
        short,
        handle,
        delivered,
    }
}

fn scope_key(scope: &RepoScope) -> String {
    format!("{}:{}", scope.repo_id, scope.worktree_id)
}

fn range_value(r: &Range, level: usize) -> Value {
    let lines = format!("{}-{}", r.start, r.end);
    if r.delivered {
        return json!({"lines": lines, "ref": r.handle, "delivered": true});
    }
    match level {
        0 => json!({"lines": lines, "ref": r.handle, "text": r.text}),
        1 => json!({"lines": lines, "ref": r.handle, "text": r.short}),
        _ => json!({"lines": lines, "ref": r.handle}),
    }
}

fn build_item(
    conn: &Connection,
    scope: &RepoScope,
    result: &CodeResult,
    query: &str,
    window: u32,
    ledger: &dyn DeliveryLedger,
    opts: &CompactOptions,
) -> Item {
    let class = crate::embed::code_evidence_class(result);
    let mut base = json!({
        "path": result.path,
        "freshness": result.freshness,
        "match_reason": result.match_reason,
    });
    let readable = matches!(result.freshness.as_str(), "fresh" | "fresh_unindexed");
    let source: Option<(String, String)> = if !readable {
        None
    } else if let Some(source) = &result.source {
        Some((source.to_string(), result.content_hash.clone()))
    } else {
        code::current_text(&scope.root, &scope.root.join(&result.path)).ok()
    };
    let mut ranges = Vec::new();
    let mut record = Vec::new();
    if let Some((text, hash)) = &source {
        let (label, built, rec) =
            build_ranges(conn, scope, result, text, hash, query, window, ledger, opts);
        if let Some(label) = label {
            base["symbol"] = json!(label);
        }
        ranges = built;
        record = rec;
    }
    if opts.explain {
        base["score"] = json!(result.score);
        base["score_breakdown"] = json!(result.score_breakdown);
    }
    let make = |level: usize| -> Value {
        let mut v = base.clone();
        if ranges.is_empty() {
            return v;
        }
        let all_delivered = ranges.iter().all(|r| r.delivered);
        let shown: Vec<Value> = if level >= 2 {
            ranges.iter().take(1).map(|r| range_value(r, 2)).collect()
        } else {
            ranges.iter().map(|r| range_value(r, level)).collect()
        };
        v["ranges"] = Value::Array(shown);
        if level == 1 && !all_delivered {
            v["detail"] = json!("signature");
        } else if level >= 2 && !all_delivered {
            v["detail"] = json!("reference");
        }
        v
    };
    let levels = [make(0), make(1), make(2)];
    let tokens = [
        estimate_tokens(&levels[0].to_string()),
        estimate_tokens(&levels[1].to_string()),
        estimate_tokens(&levels[2].to_string()),
    ];
    Item {
        class,
        score: result.score,
        levels,
        tokens,
        level: 0,
        record,
    }
}

fn assemble(header: &Value, items: &[Item], truncated: bool) -> Value {
    let mut v = header.clone();
    v["results"] = Value::Array(items.iter().map(|i| i.levels[i.level].clone()).collect());
    v["truncated"] = json!(truncated);
    v
}

/// Render a retrieval response as the compact, budgeted shape. `response` must have been produced
/// with the largest budget so no result was cut by the legacy packer; this function owns the cut.
pub fn compact_search(
    conn: &Connection,
    scope: &RepoScope,
    response: CodeSearchResponse,
    query: &str,
    context_lines: usize,
    opts: &CompactOptions,
    ledger: &mut dyn DeliveryLedger,
) -> Result<Value> {
    let budget = opts.token_budget.clamp(64, code::MAX_TOKEN_BUDGET);
    let window = context_lines.clamp(1, code::MAX_CONTEXT_LINES) as u32;
    let (generation, complete) = match response.results.first() {
        Some(first) => (first.index_generation, first.index_complete),
        None => conn
            .query_row(
                "SELECT generation,is_complete FROM repositories WHERE repo_id=?1 AND worktree_id=?2",
                params![scope.repo_id, scope.worktree_id],
                |row| Ok((row.get::<_, i64>(0)? as u64, row.get::<_, i64>(1)? != 0)),
            )
            .optional()?
            .unwrap_or((0, false)),
    };
    let at_limit = response.results.len() >= opts.limit;
    let cold = response
        .results
        .iter()
        .any(|r| r.freshness == "fresh_unindexed");
    let mut items: Vec<Item> = response
        .results
        .iter()
        .map(|r| build_item(conn, scope, r, query, window, ledger, opts))
        .collect();
    let header = json!({
        "scope": response.scope,
        "generation": generation,
        "complete": complete,
        "shape": COMPACT_SHAPE,
        "token_budget": budget,
        "estimated_tokens": 9999,
        "token_estimator": response.token_estimator,
    });
    let baseline_truncated = !complete || at_limit || cold;
    let mut changed = false;
    let estimate = |items: &[Item], truncated: bool| -> usize {
        estimate_tokens(&assemble(&header, items, truncated).to_string())
    };
    while estimate(&items, baseline_truncated || changed) > budget {
        // Demote the cheapest-to-lose body first: lowest evidence class, then lowest score per
        // token, then the later result. Order and membership of the head never change.
        let pick = items
            .iter()
            .enumerate()
            .filter(|(_, i)| i.level < 2 && i.tokens[i.level] > i.tokens[i.level + 1])
            .min_by(|(ia, a), (ib, b)| {
                let spt = |i: &Item| i.score / i.tokens[i.level].max(1) as f64;
                a.class
                    .cmp(&b.class)
                    .then(spt(a).total_cmp(&spt(b)))
                    .then(ib.cmp(ia))
            })
            .map(|(index, _)| index);
        changed = true;
        match pick {
            Some(index) => items[index].level += 1,
            None => {
                if items.pop().is_none() {
                    break;
                }
            }
        }
    }
    let truncated = baseline_truncated || changed;
    let mut value = assemble(&header, &items, truncated);
    let final_estimate = estimate_tokens(&value.to_string());
    value["estimated_tokens"] = json!(final_estimate);
    for item in &items {
        if item.level == 0 {
            for key in &item.record {
                ledger.record(key.clone());
            }
        }
    }
    Ok(value)
}

/// Merge the object fields of `extra` (for example hybrid `SearchInfo`) into `value`.
pub fn merge_info(value: &mut Value, extra: Value) {
    if let (Some(target), Value::Object(extra)) = (value.as_object_mut(), extra) {
        for (k, v) in extra {
            target.entry(k).or_insert(v);
        }
    }
}

// ---------- open_range by handle ----------

pub fn open_by_handle(
    conn: &Connection,
    scope: &RepoScope,
    handle: &str,
    ledger: &mut dyn DeliveryLedger,
) -> Result<Value> {
    let h = parse_handle(handle)?;
    let end = h
        .end
        .min(h.start.saturating_add(MAX_HANDLE_RANGE_LINES - 1));
    let opened = code::open_range_scoped(conn, scope, &h.path, h.start, end)?;
    if !opened.content_hash.starts_with(&h.hash_prefix) {
        return Ok(json!({
            "path": opened.path,
            "line_start": h.start,
            "line_end": end,
            "freshness": "stale",
            "stale_handle": true,
            "content_hash": opened.content_hash,
            "index_generation": opened.index_generation,
            "message": "the file changed since this ref was issued; search again or open the range by line numbers",
        }));
    }
    ledger.record(LedgerKey {
        scope: scope_key(scope),
        path: opened.path.clone(),
        start: h.start,
        end: h.end,
        hash: opened.content_hash.clone(),
    });
    let mut value = serde_json::to_value(&opened)?;
    value["ref"] = json!(handle);
    Ok(value)
}

// ---------- ranked, budgeted repo map ----------

/// Deterministic file ranking for `repo_map`: `ln(1 + symbols) - 0.1 * depth` plus a query bias
/// (+2.0 path contains a query term, +1.5 a symbol name does). Ties break by path. Fan-in
/// centrality is designed but not implemented (docs/design/v0.2-token-savings.md).
pub fn ranked_repo_files(
    conn: &Connection,
    scope: &RepoScope,
    query: Option<&str>,
    token_budget: usize,
) -> Result<Value> {
    let budget = token_budget.clamp(64, code::MAX_TOKEN_BUDGET);
    let terms: Vec<String> = {
        let mut seen = HashSet::new();
        query
            .unwrap_or("")
            .split_whitespace()
            .map(|t| t.to_lowercase())
            .filter(|t| t.chars().count() >= 2 && seen.insert(t.clone()))
            .take(4)
            .collect()
    };
    let mut files: Vec<(String, i64)> = Vec::new();
    {
        let mut stmt = conn.prepare(
            "SELECT f.rel_path,count(s.id) FROM code_files f
             LEFT JOIN code_symbols s ON s.file_id=f.id
             WHERE f.repo_id=?1 AND f.worktree_id=?2 GROUP BY f.id",
        )?;
        for row in stmt.query_map(params![scope.repo_id, scope.worktree_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })? {
            files.push(row?);
        }
    }
    let mut symbol_hits: HashSet<String> = HashSet::new();
    for term in &terms {
        let mut stmt = conn.prepare_cached(
            "SELECT DISTINCT f.rel_path FROM code_symbols s JOIN code_files f ON f.id=s.file_id
             WHERE f.repo_id=?1 AND f.worktree_id=?2 AND instr(lower(s.name),?3)>0",
        )?;
        for row in stmt.query_map(params![scope.repo_id, scope.worktree_id, term], |r| {
            r.get::<_, String>(0)
        })? {
            symbol_hits.insert(row?);
        }
    }
    let mut ranked: Vec<(f64, String, i64)> = files
        .into_iter()
        .map(|(path, symbols)| {
            let lower = path.to_lowercase();
            let depth = path.matches('/').count() as f64;
            let mut score = (1.0 + symbols as f64).ln() - 0.1 * depth;
            if terms.iter().any(|t| lower.contains(t.as_str())) {
                score += 2.0;
            }
            if symbol_hits.contains(&path) {
                score += 1.5;
            }
            (score, path, symbols)
        })
        .collect();
    ranked.sort_by(|a, b| b.0.total_cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    let total_files = ranked.len();
    let mut entries: Vec<Value> = Vec::new();
    let mut top_stmt = conn.prepare_cached(
        "SELECT s.kind,s.name FROM code_symbols s JOIN code_files f ON f.id=s.file_id
         WHERE f.repo_id=?1 AND f.worktree_id=?2 AND f.rel_path=?3
         ORDER BY (s.line_end-s.line_start) DESC, s.line_start LIMIT 3",
    )?;
    let mut used = 20usize;
    let mut truncated = false;
    for (_, path, symbols) in ranked.into_iter().take(400) {
        let top: Vec<String> = top_stmt
            .query_map(params![scope.repo_id, scope.worktree_id, path], |row| {
                Ok(format!(
                    "{} {}",
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?
                ))
            })?
            .collect::<std::result::Result<_, _>>()?;
        let entry = json!({"path": path, "symbols": symbols, "top": top});
        let cost = estimate_tokens(&entry.to_string()) + 1;
        if used + cost > budget {
            truncated = true;
            break;
        }
        used += cost;
        entries.push(entry);
    }
    if entries.len() < total_files {
        truncated = true;
    }
    Ok(json!({
        "files": entries,
        "files_total": total_files,
        "files_token_budget": budget,
        "files_estimated_tokens": used,
        "files_truncated": truncated,
        "files_ranking": "ln(1+symbols) - 0.1*depth + query bias; deterministic; estimate, not a tokenizer count",
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handle_round_trips_and_rejects_garbage() {
        let h = make_handle("src/a#b@c.rs", 10, 24, "abcdef0123456789abcdef");
        assert_eq!(h, "src/a#b@c.rs#10-24@abcdef012345");
        let parsed = parse_handle(&h).unwrap();
        assert_eq!(parsed.path, "src/a#b@c.rs");
        assert_eq!((parsed.start, parsed.end), (10, 24));
        assert_eq!(parsed.hash_prefix, "abcdef012345");
        for bad in [
            "", "a", "a#1-2", "a#x-2@ab", "a#1-2@zz", "#1-2@ab", "a#1@ab",
        ] {
            assert!(parse_handle(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn ledger_is_bounded_keyed_by_hash_and_clearable() {
        let key = |n: u32, hash: &str| LedgerKey {
            scope: "s".into(),
            path: "p".into(),
            start: n,
            end: n + 1,
            hash: hash.into(),
        };
        let mut ledger = MemoryLedger::with_capacity(3);
        for n in 0..5 {
            ledger.record(key(n, "h1"));
        }
        assert_eq!(ledger.len(), 3);
        assert!(!ledger.delivered(&key(0, "h1")));
        assert!(ledger.delivered(&key(4, "h1")));
        assert!(
            !ledger.delivered(&key(4, "h2")),
            "changed hash is not delivered"
        );
        ledger.clear();
        assert!(ledger.is_empty());
    }

    #[test]
    fn match_windows_merge_and_cap() {
        let text = "alpha\nx\nx\nx\nx\nx\nx\nx\nalpha\nx\nx\nalpha\n";
        let lines = match_lines(text, "alpha", 1, 3, MAX_RANGES_PER_FILE);
        assert_eq!(lines, vec![1, 9]);
    }
}
