//! Local embeddings (multilingual-e5-small via FastEmbed/ONNX Runtime), persistent vectors, exact
//! KNN, and deterministic hybrid retrieval (Reciprocal Rank Fusion over FTS5/BM25 + cosine).
//!
//! Design rules:
//! * Lexical operations never touch this module's model code. The ONNX Runtime is loaded only by
//!   [`FastEmbedder::load_installed`], and only after SQLite reports stored vectors for the scope.
//! * Models are downloaded only by [`pull_model`] (`models pull`), at a pinned revision whose files
//!   are SHA-256 verified. Inference reads local files and re-verifies them as it loads them.
//! * Isolation is enforced in SQL: every vector query filters by `kind`, `scope`, and the exact
//!   model identity before a single vector is read, and joins the live source row.

use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use crate::code::{self, CodeResult, CodeSearchResponse};
use crate::db::{RepoScope, hash_bytes, open_db, repo_scope};
use crate::memory::{self, CandidateCoverage, MemoryRecord, MemorySearchResult};

pub const MODEL_NAME: &str = "multilingual-e5-small";
pub const MODEL_REPO: &str = "intfloat/multilingual-e5-small";
pub const MODEL_DIMS: usize = 384;
/// E5 positional limit; longer inputs are truncated by the tokenizer.
pub const MAX_INPUT_TOKENS: usize = 512;
/// Reciprocal Rank Fusion constant (Cormack et al. 2009).
pub const RRF_K: f64 = 60.0;
/// Candidates taken from each ranker before fusion.
const POOL: usize = 50;
const BATCH: usize = 16;
const CHUNK_MAX_LINES: usize = 40;
const CHUNK_MAX_BYTES: usize = 2000;
const MAX_CHUNKS_PER_FILE: usize = 400;
/// Immutable Hugging Face commit of `intfloat/multilingual-e5-small` (MIT) that Midnight is
/// verified against. `models pull` fetches exactly this revision, never `main`.
pub const MODEL_REVISION: &str = "614241f622f53c4eeff9890bdc4f31cfecc418b3";
/// `(path, size in bytes, SHA-256)` of every model file at [`MODEL_REVISION`]. The two LFS files'
/// hashes are the Hugging Face LFS object ids; the three small files were hashed after download.
pub const MODEL_FILE_PINS: [(&str, u64, &str); 5] = [
    (
        "onnx/model.onnx",
        470_268_510,
        "ca456c06b3a9505ddfd9131408916dd79290368331e7d76bb621f1cba6bc8665",
    ),
    (
        "tokenizer.json",
        17_082_730,
        "0b44a9d7b51c3c62626640cda0e2c2f70fdacdc25bbbd68038369d14ebdf4c39",
    ),
    (
        "config.json",
        655,
        "69137736cab8b8903a07fe8afaafdda25aac55415a12a55d1bffa9f581abf959",
    ),
    (
        "special_tokens_map.json",
        167,
        "d05497f1da52c5e09554c0cd874037a083e1dc1b9cfd48034d1c717f1afc07a7",
    ),
    (
        "tokenizer_config.json",
        443,
        "a1d6bc8734a6f635dc158508bef000f8e2e5a759c7d92f984b2c86e5ff53425b",
    ),
];

// ---------------------------------------------------------------- model management

/// Directory holding downloaded models. `AETHER_MIDNIGHT_MODELS_DIR` overrides the default.
pub fn models_dir() -> Result<PathBuf> {
    if let Some(dir) = std::env::var_os("AETHER_MIDNIGHT_MODELS_DIR") {
        return Ok(PathBuf::from(dir));
    }
    let base = dirs::data_local_dir()
        .or_else(dirs::data_dir)
        .context("OS application-data directory is unavailable")?;
    Ok(base.join("aether-midnight").join("models"))
}

#[derive(Debug, Clone, Serialize)]
pub struct ModelStatus {
    pub name: &'static str,
    pub repo: &'static str,
    pub dimensions: usize,
    pub max_input_tokens: usize,
    pub pinned_revision: &'static str,
    pub installed: bool,
    pub revision: Option<String>,
    pub path: Option<PathBuf>,
    /// Files absent or whose size differs from the pin (full hashes: `models verify`).
    pub missing_files: Vec<String>,
    /// Other cached revisions that are ignored because they are not the pinned one.
    pub ignored_revisions: Vec<String>,
}

fn repo_cache_dir(dir: &Path) -> PathBuf {
    dir.join(format!("models--{}", MODEL_REPO.replace('/', "--")))
}

/// Locate the pinned snapshot inside `dir` without touching the network. Presence and exact byte
/// sizes are checked here (cheap); SHA-256 is checked by [`verify_model`] and at model load.
pub fn model_status(dir: &Path) -> ModelStatus {
    let snapshots = repo_cache_dir(dir).join("snapshots");
    let path = snapshots.join(MODEL_REVISION);
    let missing: Vec<String> = MODEL_FILE_PINS
        .iter()
        .filter(|(f, size, _)| std::fs::metadata(path.join(f)).map(|m| m.len()).ok() != Some(*size))
        .map(|(f, _, _)| f.to_string())
        .collect();
    let mut ignored: Vec<String> = std::fs::read_dir(&snapshots)
        .map(|entries| {
            entries
                .flatten()
                .filter_map(|e| e.file_name().into_string().ok())
                .filter(|n| n != MODEL_REVISION)
                .collect()
        })
        .unwrap_or_default();
    ignored.sort();
    let installed = missing.is_empty();
    ModelStatus {
        name: MODEL_NAME,
        repo: MODEL_REPO,
        dimensions: MODEL_DIMS,
        max_input_tokens: MAX_INPUT_TOKENS,
        pinned_revision: MODEL_REVISION,
        installed,
        revision: installed.then(|| MODEL_REVISION.to_string()),
        path: installed.then_some(path),
        missing_files: missing,
        ignored_revisions: ignored,
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Compare one file's bytes with its pin.
fn check_pin(name: &str, bytes: &[u8]) -> Result<()> {
    let (_, size, sha) = MODEL_FILE_PINS
        .iter()
        .find(|(f, _, _)| *f == name)
        .with_context(|| format!("{name} is not a pinned model file"))?;
    if bytes.len() as u64 != *size {
        bail!(
            "model file {name} has {} bytes, pinned {size}; re-run `models pull`",
            bytes.len()
        );
    }
    let actual = sha256_hex(bytes);
    if actual != *sha {
        bail!(
            "model file {name} fails its SHA-256 check (expected {sha}, got {actual}); delete the model directory and re-run `models pull`"
        );
    }
    Ok(())
}

/// Full integrity check of the installed pinned snapshot (reads every file once).
pub fn verify_model(dir: &Path) -> Result<ModelStatus> {
    let status = model_status(dir);
    let Some(path) = status.path.clone() else {
        bail!(
            "model {MODEL_NAME} revision {MODEL_REVISION} is not installed or incomplete in {} (missing/size mismatch: {:?}); run `aether-midnight models pull {MODEL_NAME}`",
            dir.display(),
            status.missing_files
        );
    };
    for (file, _, _) in MODEL_FILE_PINS {
        let bytes =
            std::fs::read(path.join(file)).with_context(|| format!("read model file {file}"))?;
        check_pin(file, &bytes)?;
    }
    Ok(status)
}

/// Download the pinned model revision into `dir` (the only model-weight network operation).
/// Idempotent: files already in the cache are reused. Every file is verified against its SHA-256
/// pin before success is reported. Does not need or load the ONNX Runtime.
pub fn pull_model(dir: &Path, progress: bool) -> Result<ModelStatus> {
    std::fs::create_dir_all(dir)
        .with_context(|| format!("create models directory {}", dir.display()))?;
    let api = hf_hub::api::sync::ApiBuilder::new()
        .with_cache_dir(dir.to_path_buf())
        .with_progress(progress)
        .build()
        .context("initialize model downloader")?;
    let repo = api.repo(hf_hub::Repo::with_revision(
        MODEL_REPO.to_string(),
        hf_hub::RepoType::Model,
        MODEL_REVISION.to_string(),
    ));
    for (file, _, _) in MODEL_FILE_PINS {
        repo.get(file)
            .with_context(|| format!("download {MODEL_REPO}@{MODEL_REVISION}/{file}"))?;
    }
    verify_model(dir)
}

// ---------------------------------------------------------------- embedder abstraction

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ModelIdentity {
    pub name: String,
    pub revision: String,
    pub dimensions: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextKind {
    Query,
    Passage,
}

pub trait TextEmbedder {
    fn identity(&self) -> &ModelIdentity;
    /// Returns one L2-normalised vector per input, in order.
    fn embed(&mut self, kind: TextKind, texts: &[&str]) -> Result<Vec<Vec<f32>>>;
    /// Preferred number of code chunks to gather before embedding. Remote embedders can use this
    /// to fill several bounded requests at once; local embedders keep the established batch size.
    fn preferred_batch_size(&self) -> usize {
        BATCH
    }
}

/// E5 models require `query: ` / `passage: ` prefixes.
pub fn e5_input(kind: TextKind, text: &str) -> String {
    match kind {
        TextKind::Query => format!("query: {text}"),
        TextKind::Passage => format!("passage: {text}"),
    }
}

pub fn l2_normalize(vector: &mut [f32]) {
    let norm = vector.iter().map(|v| v * v).sum::<f32>().sqrt();
    if norm > 0.0 {
        vector.iter_mut().for_each(|v| *v /= norm);
    }
}

pub struct FastEmbedder {
    model: fastembed::TextEmbedding,
    identity: ModelIdentity,
}

impl FastEmbedder {
    /// Load the installed model from local files. Never downloads.
    pub fn load_installed() -> Result<Self> {
        let dir = models_dir()?;
        let status = model_status(&dir);
        let (Some(path), Some(revision)) = (status.path, status.revision) else {
            bail!(
                "model {MODEL_NAME} is not installed in {}; run `aether-midnight models pull {MODEL_NAME}`",
                dir.display()
            );
        };
        crate::runtime::ensure_loadable(&dir)?;
        // Every file is hashed in memory before it is handed to the tokenizer or ONNX Runtime.
        let read = |name: &str| -> Result<Vec<u8>> {
            let bytes = std::fs::read(path.join(name))
                .with_context(|| format!("read model file {name}"))?;
            check_pin(name, &bytes)?;
            Ok(bytes)
        };
        let onnx = read("onnx/model.onnx")?;
        let tokenizer_files = fastembed::TokenizerFiles {
            tokenizer_file: read("tokenizer.json")?,
            config_file: read("config.json")?,
            special_tokens_map_file: read("special_tokens_map.json")?,
            tokenizer_config_file: read("tokenizer_config.json")?,
        };
        let definition = fastembed::UserDefinedEmbeddingModel::new(onnx, tokenizer_files)
            .with_pooling(fastembed::Pooling::Mean);
        // No execution provider is registered, so inference runs on the CPU provider only.
        let options = fastembed::InitOptionsUserDefined::new().with_max_length(MAX_INPUT_TOKENS);
        let model = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            fastembed::TextEmbedding::try_new_from_user_defined(definition, options)
        }))
        .map_err(|_| anyhow::anyhow!("ONNX Runtime failed to initialize (see ORT_DYLIB_PATH)"))?
        .map_err(|e| anyhow::anyhow!("initialize embedding model: {e}"))?;
        Ok(Self {
            model,
            identity: ModelIdentity {
                name: MODEL_NAME.to_string(),
                revision,
                dimensions: MODEL_DIMS,
            },
        })
    }
}

impl TextEmbedder for FastEmbedder {
    fn identity(&self) -> &ModelIdentity {
        &self.identity
    }

    fn embed(&mut self, kind: TextKind, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        let inputs: Vec<String> = texts.iter().map(|t| e5_input(kind, t)).collect();
        let mut vectors = self
            .model
            .embed(&inputs, Some(BATCH))
            .map_err(|e| anyhow::anyhow!("embedding inference failed: {e}"))?;
        for vector in &mut vectors {
            if vector.len() != MODEL_DIMS {
                bail!(
                    "model returned {} dimensions, expected {MODEL_DIMS}",
                    vector.len()
                );
            }
            l2_normalize(vector);
        }
        Ok(vectors)
    }
}

// ---------------------------------------------------------------- vector storage helpers

fn encode_vector(vector: &[f32]) -> Vec<u8> {
    vector.iter().flat_map(|v| v.to_le_bytes()).collect()
}

fn dot_with_blob(blob: &[u8], query: &[f32]) -> Option<f32> {
    if blob.len() != query.len() * 4 {
        return None;
    }
    Some(
        blob.as_chunks::<4>()
            .0
            .iter()
            .zip(query)
            .map(|(b, q)| f32::from_le_bytes(*b) * q)
            .sum(),
    )
}

/// Reference ordering for [`top_k_hits`]: descending similarity, then `tie`.
#[cfg(test)]
fn sort_hits<T>(hits: &mut [(T, f32)], tie: impl Fn(&T, &T) -> std::cmp::Ordering) {
    hits.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| tie(&a.0, &b.0)));
}

/// The first `k` elements of the `sort_hits` order, in that order, without sorting the rest.
/// `tie` must make the order total (unique keys), so the selected set and its order are exactly
/// those of a full sort followed by truncation.
fn top_k_hits<T>(hits: &mut Vec<(T, f32)>, k: usize, tie: impl Fn(&T, &T) -> std::cmp::Ordering) {
    let order = |a: &(T, f32), b: &(T, f32)| b.1.total_cmp(&a.1).then_with(|| tie(&a.0, &b.0));
    if k == 0 {
        hits.clear();
        return;
    }
    if hits.len() > k {
        hits.select_nth_unstable_by(k - 1, order);
        hits.truncate(k);
    }
    hits.sort_by(order);
}

/// Deterministic Reciprocal Rank Fusion. `score(d) = sum 1 / (RRF_K + rank_i(d))`, ranks 1-based.
/// Ties break by ascending key so equal fused scores always order the same way.
pub fn rrf_fuse<K: Ord + Clone>(lists: &[&[K]]) -> Vec<(K, f64)> {
    let mut scores: BTreeMap<K, f64> = BTreeMap::new();
    for list in lists {
        for (index, key) in list.iter().enumerate() {
            *scores.entry(key.clone()).or_insert(0.0) += 1.0 / (RRF_K + (index + 1) as f64);
        }
    }
    let mut fused: Vec<(K, f64)> = scores.into_iter().collect();
    fused.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    fused
}

// ---------------------------------------------------------------- embedding (write path)

#[derive(Debug, Default, Serialize)]
pub struct EmbedReport {
    pub kind: &'static str,
    pub scope: String,
    pub model: String,
    pub revision: String,
    pub embedded: usize,
    pub unchanged: usize,
    pub removed_stale: usize,
    pub replaced_other_model: usize,
    pub skipped_unreadable: usize,
    pub truncated_files: usize,
    pub cancelled: bool,
}

fn remove_other_models(
    conn: &Connection,
    kind: &str,
    scope: &str,
    id: &ModelIdentity,
) -> Result<usize> {
    Ok(conn.execute(
        "DELETE FROM embeddings WHERE kind=?1 AND scope=?2
           AND (model<>?3 OR revision<>?4 OR dims<>?5)",
        params![kind, scope, id.name, id.revision, id.dimensions as i64],
    )?)
}

/// Embed specific memories right after they were stored, on a caller-owned connection. Rows that
/// are no longer active, or already have a vector of this model for the same content hash, are
/// skipped. Unlike [`embed_memories`] it neither scans the namespace nor removes vectors of other
/// model revisions; `embed --namespace` still reconciles those.
pub(crate) fn embed_memory_ids(
    conn: &Connection,
    scope: &str,
    memory_ids: &[i64],
    embedder: &mut dyn TextEmbedder,
) -> Result<EmbedReport> {
    memory::validate_scope(scope)?;
    let id = embedder.identity().clone();
    let mut report = EmbedReport {
        kind: "memory",
        scope: scope.to_string(),
        model: id.name.clone(),
        revision: id.revision.clone(),
        ..Default::default()
    };
    let mut todo: Vec<(i64, String, String)> = Vec::with_capacity(memory_ids.len());
    {
        let mut stmt = conn.prepare_cached(
            "SELECT m.content,e.source_hash,e.model,e.revision,e.dims FROM memories m
             LEFT JOIN embeddings e ON e.kind='memory' AND e.owner_id=m.id AND e.chunk=0
             WHERE m.id=?1 AND m.scope=?2 AND m.deleted_at IS NULL",
        )?;
        for &mid in memory_ids {
            let row = stmt
                .query_row(params![mid, scope], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, Option<String>>(3)?,
                        row.get::<_, Option<i64>>(4)?,
                    ))
                })
                .optional()?;
            let Some((content, stored, model, revision, dims)) = row else {
                continue;
            };
            let hash = hash_bytes(content.as_bytes());
            let same_model = model.as_deref() == Some(id.name.as_str())
                && revision.as_deref() == Some(id.revision.as_str())
                && dims == Some(id.dimensions as i64);
            if same_model && stored.as_deref() == Some(hash.as_str()) {
                report.unchanged += 1;
            } else {
                todo.push((mid, content, hash));
            }
        }
    }
    for batch in todo.chunks(BATCH) {
        let texts: Vec<&str> = batch.iter().map(|(_, c, _)| c.as_str()).collect();
        let vectors = embedder.embed(TextKind::Passage, &texts)?;
        if vectors.len() != batch.len() {
            bail!(
                "embedder returned {} vectors for {} inputs",
                vectors.len(),
                batch.len()
            );
        }
        if vectors
            .iter()
            .any(|v| v.len() != id.dimensions || v.iter().any(|x| !x.is_finite()))
        {
            bail!("embedder returned a vector with the wrong dimensions or non-finite values");
        }
        let tx = conn.unchecked_transaction()?;
        for ((mid, _, hash), vector) in batch.iter().zip(&vectors) {
            tx.prepare_cached(
                "INSERT OR REPLACE INTO embeddings(kind,scope,owner_id,chunk,line_start,line_end,model,revision,dims,source_hash,vector)
                 SELECT 'memory',?1,?2,0,NULL,NULL,?3,?4,?5,?6,?7
                 WHERE EXISTS (SELECT 1 FROM memories WHERE id=?2 AND scope=?1 AND deleted_at IS NULL)",
            )?
            .execute(params![
                scope,
                mid,
                id.name,
                id.revision,
                id.dimensions as i64,
                hash,
                encode_vector(vector)
            ])?;
            report.embedded += 1;
        }
        tx.commit()?;
    }
    Ok(report)
}

/// Incrementally embed the active memories of one namespace. Unchanged memories are skipped by
/// content hash; vectors of forgotten/missing memories and of other model revisions are removed.
pub fn embed_memories(
    db_path: &Path,
    scope: &str,
    embedder: &mut dyn TextEmbedder,
    cancel: &AtomicBool,
) -> Result<EmbedReport> {
    memory::validate_scope(scope)?;
    let conn = open_db(db_path)?;
    let id = embedder.identity().clone();
    let mut report = EmbedReport {
        kind: "memory",
        scope: scope.to_string(),
        model: id.name.clone(),
        revision: id.revision.clone(),
        ..Default::default()
    };
    report.replaced_other_model = remove_other_models(&conn, "memory", scope, &id)?;
    report.removed_stale = conn.execute(
        "DELETE FROM embeddings WHERE kind='memory' AND scope=?1 AND owner_id NOT IN
           (SELECT id FROM memories WHERE scope=?1 AND deleted_at IS NULL)",
        [scope],
    )?;
    let mut todo: Vec<(i64, String, String)> = Vec::new();
    {
        let mut stmt = conn.prepare(
            "SELECT m.id,m.content,e.source_hash FROM memories m
             LEFT JOIN embeddings e ON e.kind='memory' AND e.owner_id=m.id AND e.chunk=0
             WHERE m.scope=?1 AND m.deleted_at IS NULL ORDER BY m.id",
        )?;
        let mut rows = stmt.query([scope])?;
        while let Some(row) = rows.next()? {
            let (mid, content, stored): (i64, String, Option<String>) =
                (row.get(0)?, row.get(1)?, row.get(2)?);
            let hash = hash_bytes(content.as_bytes());
            if stored.as_deref() == Some(hash.as_str()) {
                report.unchanged += 1;
            } else {
                todo.push((mid, content, hash));
            }
        }
    }
    for batch in todo.chunks(BATCH) {
        if cancel.load(Ordering::Relaxed) {
            report.cancelled = true;
            break;
        }
        let texts: Vec<&str> = batch.iter().map(|(_, c, _)| c.as_str()).collect();
        let vectors = embedder.embed(TextKind::Passage, &texts)?;
        if vectors.len() != batch.len() {
            bail!(
                "embedder returned {} vectors for {} inputs",
                vectors.len(),
                batch.len()
            );
        }
        let tx = conn.unchecked_transaction()?;
        for ((mid, _, hash), vector) in batch.iter().zip(&vectors) {
            // The source row may have been forgotten while embedding; only store live rows.
            tx.execute(
                "INSERT OR REPLACE INTO embeddings(kind,scope,owner_id,chunk,line_start,line_end,model,revision,dims,source_hash,vector)
                 SELECT 'memory',?1,?2,0,NULL,NULL,?3,?4,?5,?6,?7
                 WHERE EXISTS (SELECT 1 FROM memories WHERE id=?2 AND scope=?1 AND deleted_at IS NULL)",
                params![
                    scope,
                    mid,
                    id.name,
                    id.revision,
                    id.dimensions as i64,
                    hash,
                    encode_vector(vector)
                ],
            )?;
            report.embedded += 1;
        }
        tx.commit()?;
    }
    Ok(report)
}

struct Chunk {
    index: usize,
    line_start: u32,
    line_end: u32,
    text: String,
}

struct PendingCodeFile {
    file_id: i64,
    chunks: Vec<Chunk>,
    hashes: Vec<String>,
    /// Stored vector for a chunk whose text did not change; `None` means it must be embedded.
    reused: Vec<Option<Vec<u8>>>,
}

fn chunk_text(text: &str, symbol_ends: &[usize]) -> (Vec<Chunk>, bool) {
    let lines: Vec<_> = text.lines().collect();
    let mut offset = 0;
    let mut chunks = Vec::new();
    while offset < lines.len() && chunks.len() < MAX_CHUNKS_PER_FILE {
        let mut end = offset;
        let mut bytes = 0;
        while end < lines.len() && end - offset < CHUNK_MAX_LINES {
            let size = lines[end].len().min(CHUNK_MAX_BYTES) + 1;
            if end > offset && bytes + size > CHUNK_MAX_BYTES {
                break;
            }
            bytes += size;
            end += 1;
        }
        // Prefer a boundary within the window; ignore tiny candidates that would multiply
        // chunks and leave fewer indexed lines under the existing per-file cap.
        if end < lines.len() {
            let boundary = symbol_ends.partition_point(|candidate| *candidate <= end);
            if let Some(&preferred) = symbol_ends.get(..boundary).and_then(|ends| ends.last())
                && preferred > offset
                && preferred - offset >= (end - offset).div_ceil(2)
            {
                end = preferred;
            }
        }
        let mut body = String::new();
        for mut line in lines[offset..end].iter().copied() {
            if line.len() > CHUNK_MAX_BYTES {
                line = &line[..line.floor_char_boundary(CHUNK_MAX_BYTES)];
            }
            body.push_str(line);
            body.push('\n');
        }
        if !body.trim().is_empty() {
            chunks.push(Chunk {
                index: chunks.len(),
                line_start: offset as u32 + 1,
                line_end: end as u32,
                text: body,
            });
        }
        offset = end;
    }
    (chunks, offset < lines.len())
}

fn fixed_chunks(text: &str) -> (Vec<Chunk>, bool) {
    let mut chunks = Vec::new();
    let mut buffer = String::new();
    let mut start = 0usize;
    let mut lines_in = 0usize;
    let mut truncated = false;
    let flush = |buffer: &mut String, start: usize, end: usize, chunks: &mut Vec<Chunk>| {
        if !buffer.trim().is_empty() {
            let index = chunks.len();
            chunks.push(Chunk {
                index,
                line_start: start as u32,
                line_end: end as u32,
                text: std::mem::take(buffer),
            });
        } else {
            buffer.clear();
        }
    };
    for (number, line) in text.lines().enumerate() {
        let number = number + 1;
        if lines_in == 0 {
            start = number;
        }
        let mut line = line;
        if line.len() > CHUNK_MAX_BYTES {
            let mut end = CHUNK_MAX_BYTES;
            while !line.is_char_boundary(end) {
                end -= 1;
            }
            line = &line[..end];
        }
        if lines_in > 0
            && (lines_in >= CHUNK_MAX_LINES || buffer.len() + line.len() > CHUNK_MAX_BYTES)
        {
            flush(&mut buffer, start, number - 1, &mut chunks);
            lines_in = 0;
            start = number;
        }
        buffer.push_str(line);
        buffer.push('\n');
        lines_in += 1;
        if chunks.len() >= MAX_CHUNKS_PER_FILE {
            truncated = true;
            buffer.clear();
            break;
        }
    }
    if lines_in > 0 {
        let end = start + lines_in - 1;
        flush(&mut buffer, start, end, &mut chunks);
    }
    (chunks, truncated)
}

fn code_scope_key(scope: &RepoScope) -> String {
    format!("{}:{}", scope.repo_id, scope.worktree_id)
}

fn embed_and_store_code_files(
    conn: &Connection,
    key: &str,
    id: &ModelIdentity,
    files: &[PendingCodeFile],
    embedder: &mut dyn TextEmbedder,
    report: &mut EmbedReport,
) -> Result<()> {
    if files.is_empty() {
        return Ok(());
    }
    // Only chunks without a reusable vector go to the embedder (and, for cloud, over the network).
    let texts: Vec<&str> = files
        .iter()
        .flat_map(|file| {
            file.chunks
                .iter()
                .zip(&file.reused)
                .filter(|(_, reused)| reused.is_none())
                .map(|(chunk, _)| chunk.text.as_str())
        })
        .collect();
    let expected = texts.len();
    let vectors = if texts.is_empty() {
        Vec::new()
    } else {
        embedder.embed(TextKind::Passage, &texts)?
    };
    if vectors.len() != expected {
        bail!(
            "embedder returned {} vectors for {expected} chunks",
            vectors.len()
        );
    }

    let mut fresh = vectors.iter();
    for file in files {
        let tx = conn.unchecked_transaction()?;
        tx.execute(
            "DELETE FROM embeddings WHERE kind='code' AND owner_id=?1",
            [file.file_id],
        )?;
        tx.execute(
            "DELETE FROM embeddings_reuse WHERE owner_id=?1",
            [file.file_id],
        )?;
        let mut insert = tx.prepare_cached(
            "INSERT INTO embeddings(kind,scope,owner_id,chunk,line_start,line_end,model,revision,dims,source_hash,vector)
             VALUES('code',?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
        )?;
        for ((chunk, hash), reused) in file.chunks.iter().zip(&file.hashes).zip(&file.reused) {
            let blob = match reused {
                Some(blob) => {
                    report.unchanged += 1;
                    blob.clone()
                }
                None => {
                    let vector = fresh.next().expect("vector count was checked above");
                    report.embedded += 1;
                    encode_vector(vector)
                }
            };
            insert.execute(params![
                key,
                file.file_id,
                chunk.index as i64,
                chunk.line_start,
                chunk.line_end,
                id.name,
                id.revision,
                id.dimensions as i64,
                hash,
                blob
            ])?;
        }
        drop(insert);
        tx.commit()?;
    }
    Ok(())
}

/// Incrementally embed the indexed files of one repository/worktree as line-window chunks. Files
/// that changed on disk since indexing are skipped (their vectors are removed): embed after `index`.
/// Progress counts completed files as their vectors are committed, then reports once more at finish.
#[derive(Debug, Clone, Copy)]
pub struct EmbedProgress {
    pub done_files: u64,
    pub total_files: u64,
    pub embedded_chunks: u64,
    pub unchanged_chunks: u64,
}

pub fn embed_code(
    db_path: &Path,
    root: &Path,
    embedder: &mut dyn TextEmbedder,
    cancel: &AtomicBool,
) -> Result<EmbedReport> {
    embed_code_with_progress(db_path, root, embedder, cancel, &mut |_| {})
}

pub fn embed_code_with_progress(
    db_path: &Path,
    root: &Path,
    embedder: &mut dyn TextEmbedder,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(&EmbedProgress),
) -> Result<EmbedReport> {
    let scope = repo_scope(root)?;
    let key = code_scope_key(&scope);
    let conn = open_db(db_path)?;
    let id = embedder.identity().clone();
    let mut report = EmbedReport {
        kind: "code",
        scope: key.clone(),
        model: id.name.clone(),
        revision: id.revision.clone(),
        ..Default::default()
    };
    report.replaced_other_model = remove_other_models(&conn, "code", &key, &id)?;
    report.removed_stale = conn.execute(
        "DELETE FROM embeddings WHERE kind='code' AND scope=?1 AND owner_id NOT IN
           (SELECT id FROM code_files WHERE repo_id=?2 AND worktree_id=?3)",
        params![key, scope.repo_id, scope.worktree_id],
    )?;
    let files: Vec<(i64, String, String, bool)> = {
        let mut stmt = conn.prepare(
            "SELECT id,rel_path,content_hash,parse_error FROM code_files
             WHERE repo_id=?1 AND worktree_id=?2 ORDER BY rel_path",
        )?;
        stmt.query_map(params![scope.repo_id, scope.worktree_id], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })?
        .collect::<rusqlite::Result<_>>()?
    };
    let total_files = files.len() as u64;
    let mut notify = |done: u64, report: &EmbedReport| {
        progress(&EmbedProgress {
            done_files: done,
            total_files,
            embedded_chunks: report.embedded as u64,
            unchanged_chunks: report.unchanged as u64,
        })
    };
    let preferred_batch_size = embedder.preferred_batch_size().max(1);
    let mut pending_files = Vec::new();
    let mut pending_chunks = 0usize;
    let mut done_files = 0u64;
    for (file_id, rel_path, file_hash, parse_error) in files {
        // Keep the old initial/periodic progress cadence while counting only fully handled files.
        notify(done_files, &report);
        if cancel.load(Ordering::Relaxed) {
            // Cancellation must be prompt: pending files are dropped, not sent to the provider.
            // They stay without vectors and the next `embed` run picks them up.
            report.cancelled = true;
            break;
        }
        let current = code::current_text(&scope.root, &scope.root.join(&rel_path));
        let text = match current {
            Ok((text, hash)) if hash == file_hash => text,
            _ => {
                report.skipped_unreadable += 1;
                report.removed_stale += conn.execute(
                    "DELETE FROM embeddings WHERE kind='code' AND owner_id=?1",
                    [file_id],
                )?;
                done_files += 1;
                notify(done_files, &report);
                continue;
            }
        };
        let count: i64 = conn.query_row(
            "SELECT count(*) FROM embeddings WHERE kind='code' AND owner_id=?1
             AND model=?2 AND revision=?3 AND dims=?4",
            params![file_id, id.name, id.revision, id.dimensions as i64],
            |row| row.get(0),
        )?;
        // The catalog's update trigger parks all current rows whenever the file hash changes.
        if count > 0 {
            report.unchanged += count as usize;
            done_files += 1;
            notify(done_files, &report);
            continue;
        }
        let symbol_ends = if parse_error {
            Vec::new()
        } else {
            let mut stmt = conn.prepare_cached(
                "SELECT DISTINCT line_end FROM code_symbols WHERE file_id=?1 ORDER BY line_end",
            )?;
            stmt.query_map([file_id], |row| {
                row.get::<_, u32>(0).map(|line| line as usize)
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?
        };
        let (chunks, truncated) = if symbol_ends.is_empty() {
            fixed_chunks(&text)
        } else {
            chunk_text(&text, &symbol_ends)
        };
        if truncated {
            report.truncated_files += 1;
        }
        let hashes: Vec<String> = chunks
            .iter()
            .map(|c| hash_bytes(c.text.as_bytes()))
            .collect();
        // Reuse any stored vector whose chunk text is identical, wherever it moved in the file.
        // Vectors parked by the schema trigger when the file's content changed are candidates too.
        let parked: Vec<(String, Vec<u8>)> = {
            let mut stmt = conn.prepare_cached(
                "SELECT source_hash,vector FROM embeddings_reuse
                 WHERE owner_id=?1 AND model=?2 AND revision=?3 AND dims=?4 ORDER BY chunk",
            )?;
            stmt.query_map(
                params![file_id, id.name, id.revision, id.dimensions as i64],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?
            .collect::<rusqlite::Result<_>>()?
        };
        let mut by_hash: HashMap<&str, &Vec<u8>> = HashMap::new();
        for (hash, blob) in &parked {
            by_hash.entry(hash.as_str()).or_insert(blob);
        }
        let reused: Vec<Option<Vec<u8>>> = hashes
            .iter()
            .map(|h| {
                by_hash
                    .get(h.as_str())
                    .filter(|blob| blob.len() == id.dimensions * 4)
                    .map(|blob| (*blob).clone())
            })
            .collect();
        let needed = reused.iter().filter(|r| r.is_none()).count();
        if needed == 0 {
            // Nothing to embed (lines moved or a chunk was removed): rewrite rows now, no provider call.
            let file = PendingCodeFile {
                file_id,
                chunks,
                hashes,
                reused,
            };
            embed_and_store_code_files(&conn, &key, &id, &[file], embedder, &mut report)?;
            done_files += 1;
            notify(done_files, &report);
            continue;
        }
        if chunks.is_empty() {
            conn.execute(
                "DELETE FROM embeddings WHERE kind='code' AND owner_id=?1",
                [file_id],
            )?;
            done_files += 1;
            notify(done_files, &report);
            continue;
        }

        if pending_chunks > 0 && pending_chunks.saturating_add(needed) > preferred_batch_size {
            embed_and_store_code_files(&conn, &key, &id, &pending_files, embedder, &mut report)?;
            done_files += pending_files.len() as u64;
            pending_files.clear();
            pending_chunks = 0;
            notify(done_files, &report);
        }
        pending_chunks += needed;
        pending_files.push(PendingCodeFile {
            file_id,
            chunks,
            hashes,
            reused,
        });
        if pending_chunks >= preferred_batch_size {
            embed_and_store_code_files(&conn, &key, &id, &pending_files, embedder, &mut report)?;
            done_files += pending_files.len() as u64;
            pending_files.clear();
            pending_chunks = 0;
            notify(done_files, &report);
        }
    }
    if !pending_files.is_empty() {
        embed_and_store_code_files(&conn, &key, &id, &pending_files, embedder, &mut report)?;
    }
    if !report.cancelled {
        notify(total_files, &report);
    }
    Ok(report)
}

// ---------------------------------------------------------------- search

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchMode {
    Lexical,
    Semantic,
    Hybrid,
}

#[derive(Debug, Clone, Serialize)]
pub struct Coverage {
    pub eligible: u64,
    pub embedded: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct SearchInfo {
    pub mode_requested: SearchMode,
    pub mode_used: SearchMode,
    /// True when a requested semantic stage could not run and lexical results were returned.
    pub degraded: bool,
    pub notice: Option<String>,
    pub model: Option<ModelIdentity>,
    pub semantic_coverage: Option<Coverage>,
    pub fusion: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct MemorySearchResponse {
    #[serde(flatten)]
    pub info: SearchInfo,
    pub results: Vec<MemorySearchResult>,
}

#[derive(Debug, Serialize)]
pub struct CodeHybridResponse {
    #[serde(flatten)]
    pub info: SearchInfo,
    #[serde(flatten)]
    pub search: CodeSearchResponse,
}

pub type Loader<'a> = &'a mut dyn FnMut() -> Result<Box<dyn TextEmbedder>>;

enum Prepared {
    Ready(Box<dyn TextEmbedder>, Vec<f32>),
    Unavailable(String),
}

fn has_vectors(conn: &Connection, kind: &str, scope: &str) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM embeddings WHERE kind=?1 AND scope=?2)",
        params![kind, scope],
        |r| r.get(0),
    )?)
}

fn identity_has_vectors(
    conn: &Connection,
    kind: &str,
    scope: &str,
    id: &ModelIdentity,
) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM embeddings WHERE kind=?1 AND scope=?2
           AND model=?3 AND revision=?4 AND dims=?5)",
        params![kind, scope, id.name, id.revision, id.dimensions as i64],
        |r| r.get(0),
    )?)
}

pub(crate) fn has_eligible_memory_vectors(
    conn: &Connection,
    scope: &str,
    id: &ModelIdentity,
) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM embeddings e JOIN memories m ON m.id=e.owner_id
         WHERE e.kind='memory' AND e.scope=?1 AND m.scope=?1 AND e.model=?2
           AND e.revision=?3 AND e.dims=?4 AND m.deleted_at IS NULL
           AND (m.valid_from IS NULL OR m.valid_from<=?5)
           AND (m.valid_to IS NULL OR m.valid_to>?5))",
        params![
            scope,
            id.name,
            id.revision,
            id.dimensions as i64,
            crate::db::now_utc()
        ],
        |row| row.get(0),
    )?)
}

fn stored_identities(conn: &Connection, kind: &str, scope: &str) -> String {
    let rows: Vec<String> = conn
        .prepare(
            "SELECT DISTINCT model,revision,dims FROM embeddings WHERE kind=?1 AND scope=?2 ORDER BY 1,2,3 LIMIT 4",
        )
        .and_then(|mut stmt| {
            stmt.query_map(params![kind, scope], |r| {
                Ok(format!(
                    "{}@{} {}d",
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?
                ))
            })?
            .collect()
        })
        .unwrap_or_default();
    rows.join(", ")
}

/// Load the model (lazily, only when vectors exist) and embed the query.
fn prepare_query(
    conn: &Connection,
    kind: &str,
    scope: &str,
    query: &str,
    loader: Loader<'_>,
    embed_hint: &str,
) -> Prepared {
    match has_vectors(conn, kind, scope) {
        Ok(true) => {}
        Ok(false) => {
            return Prepared::Unavailable(format!(
                "no embeddings stored for this scope; run `{embed_hint}` after installing the model"
            ));
        }
        Err(e) => return Prepared::Unavailable(format!("embedding store unreadable: {e:#}")),
    }
    let mut embedder = match loader() {
        Ok(e) => e,
        Err(e) => return Prepared::Unavailable(format!("embedding model unavailable: {e:#}")),
    };
    // Space isolation before any query text can leave the machine: only embed the query when
    // vectors of exactly this provider/model/revision/dimensions exist for the scope.
    let id = embedder.identity().clone();
    match identity_has_vectors(conn, kind, scope, &id) {
        Ok(true) => {}
        Ok(false) => {
            let stored = stored_identities(conn, kind, scope);
            return Prepared::Unavailable(format!(
                "stored vectors ({stored}) belong to a different embedding space than the requested {}@{} ({}d); the query was not embedded. Re-run `{embed_hint}` with the requested provider to reindex (this replaces the stored vectors)",
                id.name, id.revision, id.dimensions
            ));
        }
        Err(e) => return Prepared::Unavailable(format!("embedding store unreadable: {e:#}")),
    }
    match embedder.embed(TextKind::Query, &[query]) {
        Ok(mut v) if v.len() == 1 => {
            let mut vector = v.remove(0);
            l2_normalize(&mut vector);
            Prepared::Ready(embedder, vector)
        }
        Ok(_) => Prepared::Unavailable("embedder returned an unexpected result".into()),
        Err(e) => Prepared::Unavailable(format!("query embedding failed: {e:#}")),
    }
}

fn info(requested: SearchMode) -> SearchInfo {
    SearchInfo {
        mode_requested: requested,
        mode_used: SearchMode::Lexical,
        degraded: false,
        notice: None,
        model: None,
        semantic_coverage: None,
        fusion: None,
    }
}

fn degrade(info: &mut SearchInfo, reason: String) {
    info.degraded = true;
    info.mode_used = SearchMode::Lexical;
    info.notice = Some(format!(
        "semantic search unavailable, returned lexical results: {reason}"
    ));
}

const MEMORY_COLUMNS: &str = "m.id,m.scope,m.kind,m.content,m.source_uri,m.source_id,m.supersedes_id,m.created_at,m.observed_at,m.valid_from,m.valid_to,m.origin";

fn memory_knn(
    conn: &Connection,
    scope: &str,
    id: &ModelIdentity,
    query: &[f32],
    now: &str,
    k: usize,
) -> Result<(Vec<(i64, f32)>, Coverage)> {
    let mut stmt = conn.prepare_cached(
        "SELECT e.owner_id,e.vector FROM embeddings e
         JOIN memories m ON m.id=e.owner_id
         WHERE e.kind='memory' AND e.scope=?1 AND e.model=?2 AND e.revision=?3 AND e.dims=?4
           AND m.scope=?1 AND m.deleted_at IS NULL
           AND (m.valid_from IS NULL OR m.valid_from<=?5)
           AND (m.valid_to IS NULL OR m.valid_to>?5)",
    )?;
    let mut hits: Vec<(i64, f32)> = Vec::new();
    let mut rows = stmt.query(params![
        scope,
        id.name,
        id.revision,
        id.dimensions as i64,
        now
    ])?;
    while let Some(row) = rows.next()? {
        let owner: i64 = row.get(0)?;
        if let Some(sim) = dot_with_blob(row.get_ref(1)?.as_blob()?, query) {
            hits.push((owner, sim));
        }
    }
    drop(rows);
    let embedded = hits.len() as u64;
    top_k_hits(&mut hits, k, |a, b| a.cmp(b));
    let eligible: i64 = conn
        .prepare_cached(
            "SELECT count(*) FROM memories WHERE scope=?1 AND deleted_at IS NULL
           AND (valid_from IS NULL OR valid_from<=?2) AND (valid_to IS NULL OR valid_to>?2)",
        )?
        .query_row(params![scope, now], |r| r.get(0))?;
    Ok((
        hits,
        Coverage {
            eligible: eligible as u64,
            embedded,
        },
    ))
}

/// Compare the newly stored vector with older active memories in exactly the same embedding space.
pub(crate) fn possible_memory_duplicate(
    conn: &Connection,
    memory: &MemoryRecord,
) -> Result<Option<i64>> {
    let mut stmt = conn.prepare_cached(
        "SELECT prior.id, candidate.vector, current.vector FROM embeddings current
         JOIN embeddings candidate ON candidate.kind='memory' AND candidate.scope=current.scope
          AND candidate.model=current.model AND candidate.revision=current.revision AND candidate.dims=current.dims
         JOIN memories prior ON prior.id=candidate.owner_id AND prior.scope=candidate.scope
         WHERE current.kind='memory' AND current.scope=?1 AND current.owner_id=?2
          AND prior.id<?2 AND prior.kind=?3 AND prior.deleted_at IS NULL
          AND (prior.valid_to IS NULL OR ?4 IS NULL OR prior.valid_to>?4)
          AND (?5 IS NULL OR prior.valid_from IS NULL OR prior.valid_from<?5)
         ORDER BY prior.id DESC",
    )?;
    let mut rows = stmt.query(params![
        memory.scope,
        memory.id,
        memory.kind,
        memory.valid_from,
        memory.valid_to
    ])?;
    let mut best = None;
    let mut current = Vec::new();
    while let Some(row) = rows.next()? {
        if current.is_empty() {
            current.extend(
                row.get_ref(2)?
                    .as_blob()?
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .copied()
                    .map(f32::from_le_bytes),
            );
        }
        let Some(score) = dot_with_blob(row.get_ref(1)?.as_blob()?, &current) else {
            continue;
        };
        if score >= 0.92 && best.is_none_or(|(_, prior)| score > prior) {
            best = Some((row.get(0)?, score));
        }
    }
    Ok(best.map(|(id, _)| id))
}

fn fetch_memory(conn: &Connection, scope: &str, id: i64) -> Result<Option<MemoryRecord>> {
    let sql = format!(
        "SELECT {MEMORY_COLUMNS} FROM memories m WHERE m.id=?1 AND m.scope=?2 AND m.deleted_at IS NULL"
    );
    let mut stmt = conn.prepare(&sql)?;
    let mut rows = stmt.query_map(params![id, scope], memory::hydrate_memory)?;
    Ok(rows.next().transpose()?)
}

/// Memory search in `lexical`, `semantic` or `hybrid` mode within one namespace. The model is only
/// loaded (through `loader`) when the mode needs it and vectors exist; any failure degrades to
/// lexical results with an explicit notice.
pub fn search_memory(
    db_path: &Path,
    scope: &str,
    query: &str,
    mode: SearchMode,
    limit: usize,
    loader: Loader<'_>,
) -> Result<MemorySearchResponse> {
    memory::validate_scope(scope)?;
    if query.trim().is_empty() {
        bail!("memory search query cannot be empty");
    }
    let conn = open_db(db_path)?;
    search_memory_on(&conn, scope, query, mode, limit, loader)
}

/// [`search_memory`] on a caller-owned connection. The lexical stage runs on the same
/// connection instead of opening a second one.
pub(crate) fn search_memory_on(
    conn: &Connection,
    scope: &str,
    query: &str,
    mode: SearchMode,
    limit: usize,
    loader: Loader<'_>,
) -> Result<MemorySearchResponse> {
    memory::validate_scope(scope)?;
    if query.trim().is_empty() {
        bail!("memory search query cannot be empty");
    }
    let limit = limit.clamp(1, 100);
    let mut meta = info(mode);
    let mut semantic: Option<(Vec<(i64, f32)>, ModelIdentity)> = None;
    if mode != SearchMode::Lexical {
        match prepare_query(
            conn,
            "memory",
            scope,
            query,
            loader,
            "aether-midnight embed --namespace <ns>",
        ) {
            Prepared::Ready(embedder, vector) => {
                let id = embedder.identity().clone();
                let now = crate::db::now_utc();
                let (hits, coverage) = memory_knn(conn, scope, &id, &vector, &now, POOL)?;
                if coverage.embedded == 0 {
                    degrade(
                        &mut meta,
                        "stored vectors do not match the installed model revision; re-run embed"
                            .into(),
                    );
                } else {
                    if coverage.embedded < coverage.eligible {
                        meta.notice = Some(format!(
                            "partial semantic coverage: {} of {} memories embedded; run embed to complete",
                            coverage.embedded, coverage.eligible
                        ));
                    }
                    meta.semantic_coverage = Some(coverage);
                    meta.model = Some(id.clone());
                    semantic = Some((hits, id));
                }
            }
            Prepared::Unavailable(reason) => degrade(&mut meta, reason),
        }
    }
    let lexical_needed = mode != SearchMode::Semantic || semantic.is_none();
    let lexical = if lexical_needed {
        memory::recall_on(
            conn,
            scope,
            query,
            None,
            if semantic.is_some() { POOL } else { limit },
        )?
    } else {
        Vec::new()
    };
    let Some((hits, _)) = semantic else {
        meta.mode_used = SearchMode::Lexical;
        return Ok(MemorySearchResponse {
            info: meta,
            results: lexical.into_iter().take(limit).collect(),
        });
    };
    meta.mode_used = mode;
    let sim: HashMap<i64, f32> = hits.iter().copied().collect();
    if mode == SearchMode::Semantic {
        let mut results = Vec::new();
        for (id, similarity) in hits.iter().take(limit) {
            if let Some(memory) = fetch_memory(conn, scope, *id)? {
                results.push(MemorySearchResult {
                    memory,
                    match_reason: "semantic".into(),
                    score: f64::from(*similarity),
                    candidate_coverage: CandidateCoverage::Complete,
                });
            }
        }
        return Ok(MemorySearchResponse {
            info: meta,
            results,
        });
    }
    meta.fusion = Some(format!("rrf(k={RRF_K})"));
    let lex_ids: Vec<i64> = lexical.iter().map(|r| r.memory.id).collect();
    let sem_ids: Vec<i64> = hits.iter().map(|h| h.0).collect();
    let mut lexical_by_id: HashMap<i64, MemorySearchResult> =
        lexical.into_iter().map(|r| (r.memory.id, r)).collect();
    let mut results = Vec::new();
    for (id, score) in rrf_fuse(&[&lex_ids, &sem_ids]).into_iter().take(limit) {
        let lex_rank = lex_ids.iter().position(|x| *x == id).map(|p| p + 1);
        let sem_rank = sem_ids.iter().position(|x| *x == id).map(|p| p + 1);
        let mut reasons = Vec::new();
        if let Some(r) = lex_rank {
            reasons.push(format!("lexical#{r}"));
        }
        if let Some(r) = sem_rank {
            reasons.push(format!("semantic#{r}(cos {:.3})", sim[&id]));
        }
        let (memory, coverage) = match lexical_by_id.remove(&id) {
            Some(r) => (r.memory, r.candidate_coverage),
            None => match fetch_memory(conn, scope, id)? {
                Some(m) => (m, CandidateCoverage::Complete),
                None => continue,
            },
        };
        results.push(MemorySearchResult {
            memory,
            match_reason: format!("hybrid:{}", reasons.join("+")),
            score,
            candidate_coverage: coverage,
        });
    }
    Ok(MemorySearchResponse {
        info: meta,
        results,
    })
}

struct CodeHit {
    path: String,
    line_start: u32,
    line_end: u32,
    similarity: f32,
    file_hash: String,
    generation: u64,
    chunk: i64,
}

fn code_knn(
    conn: &Connection,
    scope: &RepoScope,
    id: &ModelIdentity,
    query: &[f32],
) -> Result<(Vec<CodeHit>, Coverage)> {
    let mut stmt = conn.prepare_cached(
        "SELECT f.rel_path,e.chunk,e.line_start,e.line_end,e.vector,f.content_hash,f.indexed_generation
         FROM embeddings e JOIN code_files f ON f.id=e.owner_id
         WHERE e.kind='code' AND e.scope=?1 AND e.model=?2 AND e.revision=?3 AND e.dims=?4
           AND f.repo_id=?5 AND f.worktree_id=?6",
    )?;
    // Every file with at least one valid vector has an entry, so `best.len()` is the number of
    // embedded files.
    let mut best: HashMap<String, CodeHit> = HashMap::new();
    let mut rows = stmt.query(params![
        code_scope_key(scope),
        id.name,
        id.revision,
        id.dimensions as i64,
        scope.repo_id,
        scope.worktree_id
    ])?;
    while let Some(row) = rows.next()? {
        let Some(similarity) = dot_with_blob(row.get_ref(4)?.as_blob()?, query) else {
            continue;
        };
        let path = row.get_ref(0)?.as_str()?;
        let chunk: i64 = row.get(1)?;
        // Same rule as before: keep the higher similarity, then the lower chunk.
        if best.get(path).is_some_and(|cur| {
            cur.similarity > similarity || (cur.similarity == similarity && cur.chunk <= chunk)
        }) {
            continue;
        }
        let hit = CodeHit {
            path: path.to_string(),
            chunk,
            line_start: row.get::<_, Option<u32>>(2)?.unwrap_or(1),
            line_end: row.get::<_, Option<u32>>(3)?.unwrap_or(1),
            similarity,
            file_hash: row.get(5)?,
            generation: row.get::<_, i64>(6)? as u64,
        };
        best.insert(hit.path.clone(), hit);
    }
    drop(rows);
    let embedded_files = best.len();
    let eligible: i64 = conn.query_row(
        "SELECT count(*) FROM code_files WHERE repo_id=?1 AND worktree_id=?2",
        params![scope.repo_id, scope.worktree_id],
        |r| r.get(0),
    )?;
    let mut hits: Vec<(CodeHit, f32)> = best
        .into_values()
        .map(|h| {
            let s = h.similarity;
            (h, s)
        })
        .collect();
    top_k_hits(&mut hits, 20, |a, b| a.path.cmp(&b.path));
    Ok((
        hits.into_iter().map(|h| h.0).collect(),
        Coverage {
            eligible: eligible as u64,
            embedded: embedded_files as u64,
        },
    ))
}

/// Code search in `lexical`, `semantic` or `hybrid` mode for one repository/worktree. Semantic
/// hits are chunk-level and are verified against the current file hash before being returned.
#[allow(clippy::too_many_arguments)] // Flat public entry point shared by the CLI and tests.
pub fn search_code(
    db_path: &Path,
    root: &Path,
    query: &str,
    mode: SearchMode,
    limit: usize,
    token_budget: usize,
    context_lines: usize,
    loader: Loader<'_>,
) -> Result<CodeHybridResponse> {
    if query.trim().is_empty() {
        bail!("code search query cannot be empty");
    }
    if query.len() > code::MAX_SEARCH_QUERY_BYTES {
        bail!("code search query exceeds 4096 bytes");
    }
    let scope = repo_scope(root)?;
    let conn = open_db(db_path)?;
    search_code_on(
        &conn,
        &scope,
        query,
        mode,
        limit,
        token_budget,
        context_lines,
        loader,
    )
}

/// [`search_code`] on a caller-owned connection; the lexical stage reuses it. `scope` must come
/// from [`repo_scope`] for this request.
#[allow(clippy::too_many_arguments)]
pub(crate) fn search_code_on(
    conn: &Connection,
    scope: &RepoScope,
    query: &str,
    mode: SearchMode,
    limit: usize,
    token_budget: usize,
    context_lines: usize,
    loader: Loader<'_>,
) -> Result<CodeHybridResponse> {
    if query.trim().is_empty() {
        bail!("code search query cannot be empty");
    }
    if query.len() > code::MAX_SEARCH_QUERY_BYTES {
        bail!("code search query exceeds 4096 bytes");
    }
    let context_lines = context_lines.clamp(1, code::MAX_CONTEXT_LINES);
    let limit = limit.clamp(1, 20);
    let budget = token_budget.clamp(64, code::MAX_TOKEN_BUDGET);
    let mut meta = info(mode);
    let mut semantic: Option<(Vec<CodeHit>, ModelIdentity)> = None;
    if mode != SearchMode::Lexical {
        match prepare_query(
            conn,
            "code",
            &code_scope_key(scope),
            query,
            loader,
            "aether-midnight embed --repo <path>",
        ) {
            Prepared::Ready(embedder, vector) => {
                let id = embedder.identity().clone();
                let (hits, coverage) = code_knn(conn, scope, &id, &vector)?;
                if coverage.embedded == 0 {
                    degrade(
                        &mut meta,
                        "stored vectors do not match the installed model revision; re-run embed"
                            .into(),
                    );
                } else {
                    if coverage.embedded < coverage.eligible {
                        meta.notice = Some(format!(
                            "partial semantic coverage: {} of {} indexed files embedded; run embed to complete",
                            coverage.embedded, coverage.eligible
                        ));
                    }
                    meta.semantic_coverage = Some(coverage);
                    meta.model = Some(id.clone());
                    semantic = Some((hits, id));
                }
            }
            Prepared::Unavailable(reason) => degrade(&mut meta, reason),
        }
    }
    let lexical_response = if mode == SearchMode::Semantic && semantic.is_some() {
        None
    } else {
        // Maximum pool and budget: packing happens once, after fusion.
        Some(code::search_code_scoped(
            conn,
            scope,
            query,
            if semantic.is_some() { 20 } else { limit },
            if semantic.is_some() {
                code::MAX_TOKEN_BUDGET
            } else {
                budget
            },
            context_lines,
        )?)
    };
    let Some((hits, _)) = semantic else {
        let lexical = lexical_response.expect("lexical response exists without semantic stage");
        meta.mode_used = SearchMode::Lexical;
        return Ok(CodeHybridResponse {
            info: meta,
            search: lexical,
        });
    };
    meta.mode_used = mode;
    let (_generation, complete): (u64, bool) = conn
        .query_row(
            "SELECT generation,is_complete FROM repositories WHERE repo_id=?1 AND worktree_id=?2",
            params![scope.repo_id, scope.worktree_id],
            |r| Ok((r.get::<_, i64>(0)? as u64, r.get::<_, i64>(1)? != 0)),
        )
        .unwrap_or((0, false));
    let scope_label = format!("repo:{}:worktree:{}", scope.repo_id, scope.worktree_id);
    let hit_by_path: HashMap<&str, &CodeHit> = hits.iter().map(|h| (h.path.as_str(), h)).collect();
    // Freshness gate: a semantic hit is only usable if the file still hashes to the indexed value.
    let mut semantic_results: HashMap<String, CodeResult> = HashMap::new();
    let mut sem_paths: Vec<String> = Vec::new();
    for hit in &hits {
        let Ok((text, hash)) = code::current_text(&scope.root, &scope.root.join(&hit.path)) else {
            continue;
        };
        if hash != hit.file_hash {
            continue;
        }
        sem_paths.push(hit.path.clone());
        let shown =
            ((hit.line_end + 1).saturating_sub(hit.line_start) as usize).clamp(1, context_lines);
        let mut line_capped = false;
        let snippet: String = text
            .lines()
            .skip(hit.line_start.saturating_sub(1) as usize)
            .take(shown)
            .map(|line| {
                let (line, capped) = code::cap_line(line, None);
                line_capped |= capped;
                line
            })
            .collect::<Vec<_>>()
            .join("\n");
        let (snippet, truncated) = code::bounded_snippet(snippet);
        let truncated = truncated || line_capped;
        let mut breakdown = BTreeMap::new();
        breakdown.insert("semantic_cosine".to_string(), f64::from(hit.similarity));
        semantic_results.insert(
            hit.path.clone(),
            CodeResult {
                path: hit.path.clone(),
                line_start: Some(hit.line_start),
                line_end: Some(hit.line_start + shown as u32 - 1),
                match_reason: "semantic".into(),
                index_generation: hit.generation,
                index_complete: complete,
                freshness: "fresh".into(),
                snippet: Some(snippet),
                snippet_truncated: truncated,
                estimated_tokens: 0,
                score: f64::from(hit.similarity),
                score_breakdown: breakdown,
                ..Default::default()
            },
        );
    }
    let mut truncated_flag = false;
    let mut token_estimator = String::new();
    let mut lex_results: Vec<CodeResult> = Vec::new();
    if let Some(lexical) = lexical_response {
        truncated_flag = lexical.truncated;
        token_estimator = lexical.token_estimator;
        lex_results = lexical.results;
    }
    let lex_paths: Vec<String> = lex_results.iter().map(|r| r.path.clone()).collect();
    let mut lexical_by_path: HashMap<String, CodeResult> = lex_results
        .into_iter()
        .map(|r| (r.path.clone(), r))
        .collect();
    let mut fused = Vec::new();
    if mode == SearchMode::Semantic {
        for path in sem_paths.iter().take(limit) {
            fused.push(
                semantic_results
                    .remove(path)
                    .expect("semantic result exists"),
            );
        }
    } else {
        meta.fusion = Some(format!("literal-priority+rrf(k={RRF_K})"));
        // Exact source evidence is a retrieval class, not a guessed RRF weight. Within each
        // class keep RRF's deterministic score/path ordering; memory fusion is unchanged.
        let mut ranked = rrf_fuse(&[&lex_paths, &sem_paths]);
        let class = |path: &String| lexical_by_path.get(path).map_or(0, code_evidence_class);
        ranked.sort_by_key(|a| std::cmp::Reverse(class(&a.0)));
        for (path, score) in ranked.into_iter().take(limit) {
            let lex_rank = lex_paths.iter().position(|p| *p == path).map(|p| p + 1);
            let sem_rank = sem_paths.iter().position(|p| *p == path).map(|p| p + 1);
            let mut result = match (
                lexical_by_path.remove(&path),
                semantic_results.remove(&path),
            ) {
                (Some(l), _) => l,
                (None, Some(s)) => s,
                (None, None) => continue,
            };
            let mut reasons = Vec::new();
            let evidence_class = code_evidence_class(&result);
            if evidence_class > 0 {
                result
                    .score_breakdown
                    .insert("evidence_class".into(), f64::from(evidence_class));
                reasons.push(
                    if evidence_class == 2 {
                        "exact"
                    } else {
                        "literal_phrase"
                    }
                    .into(),
                );
            }
            if let Some(r) = lex_rank {
                reasons.push(format!("lexical#{r}"));
            }
            if let Some(r) = sem_rank {
                reasons.push(format!("semantic#{r}"));
                if let Some(h) = hit_by_path.get(path.as_str()) {
                    result
                        .score_breakdown
                        .insert("semantic_cosine".into(), f64::from(h.similarity));
                }
            }
            result.score_breakdown.insert("rrf".into(), score);
            result.match_reason = format!("hybrid:{}", reasons.join("+"));
            result.score = score;
            fused.push(result);
        }
    }
    let was_at_limit = fused.len() >= limit;
    let (results, estimated_tokens, packed_truncated) = code::pack_context(fused, budget);
    let snippet_cut = results.iter().any(|result| result.snippet_truncated);
    if token_estimator.is_empty() {
        token_estimator =
            "midnight-heuristic-v1 (identifier chunks, punctuation, and fixed metadata allowance)"
                .into();
    }
    Ok(CodeHybridResponse {
        info: meta,
        search: CodeSearchResponse {
            scope: scope_label,
            results,
            token_budget: budget,
            estimated_tokens,
            truncated: truncated_flag
                || packed_truncated
                || was_at_limit
                || !complete
                || snippet_cut,
            token_estimator,
        },
    })
}

pub(crate) fn code_evidence_class(result: &CodeResult) -> u8 {
    if result.freshness != "fresh" {
        return 0;
    }
    let score = |key: &str| result.score_breakdown.get(key).copied().unwrap_or(0.0);
    if score("exact_path") >= 100.0 || score("exact_symbol") > 0.0 {
        2
    } else {
        u8::from(score("literal_phrase") > 0.0)
    }
}

#[cfg(test)]
mod tests;
