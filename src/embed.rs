//! Local embeddings (multilingual-e5-small via FastEmbed/ONNX Runtime), persistent vectors, exact
//! KNN, and deterministic hybrid retrieval (Reciprocal Rank Fusion over FTS5/BM25 + cosine).
//!
//! Design rules:
//! * Lexical operations never touch this module's model code. The ONNX Runtime is loaded only by
//!   [`FastEmbedder::load_installed`], and only after SQLite reports stored vectors for the scope.
//! * Models are downloaded only by [`pull_model`] (`models pull`). Inference reads local files.
//! * Isolation is enforced in SQL: every vector query filters by `kind`, `scope`, and the exact
//!   model identity before a single vector is read, and joins the live source row.

use anyhow::{Context, Result, bail};
use rusqlite::{Connection, params};
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
const MODEL_FILES: [&str; 5] = [
    "onnx/model.onnx",
    "tokenizer.json",
    "config.json",
    "special_tokens_map.json",
    "tokenizer_config.json",
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
    pub installed: bool,
    pub revision: Option<String>,
    pub path: Option<PathBuf>,
    pub missing_files: Vec<String>,
}

fn repo_cache_dir(dir: &Path) -> PathBuf {
    dir.join(format!("models--{}", MODEL_REPO.replace('/', "--")))
}

/// Locate a complete snapshot of the model inside `dir` without touching the network.
pub fn model_status(dir: &Path) -> ModelStatus {
    let repo = repo_cache_dir(dir);
    let snapshots = repo.join("snapshots");
    let mut candidates: Vec<String> = Vec::new();
    if let Ok(main) = std::fs::read_to_string(repo.join("refs").join("main")) {
        candidates.push(main.trim().to_string());
    }
    if let Ok(entries) = std::fs::read_dir(&snapshots) {
        let mut names: Vec<String> = entries
            .flatten()
            .filter_map(|e| e.file_name().into_string().ok())
            .collect();
        names.sort();
        candidates.extend(names);
    }
    let mut missing = MODEL_FILES
        .iter()
        .map(|f| f.to_string())
        .collect::<Vec<_>>();
    for revision in candidates {
        if revision.is_empty() || revision.contains(['/', '\\']) {
            continue;
        }
        let path = snapshots.join(&revision);
        let absent: Vec<String> = MODEL_FILES
            .iter()
            .filter(|f| !path.join(f).is_file())
            .map(|f| f.to_string())
            .collect();
        if absent.is_empty() {
            return ModelStatus {
                name: MODEL_NAME,
                repo: MODEL_REPO,
                dimensions: MODEL_DIMS,
                max_input_tokens: MAX_INPUT_TOKENS,
                installed: true,
                revision: Some(revision),
                path: Some(path),
                missing_files: Vec::new(),
            };
        }
        missing = absent;
    }
    ModelStatus {
        name: MODEL_NAME,
        repo: MODEL_REPO,
        dimensions: MODEL_DIMS,
        max_input_tokens: MAX_INPUT_TOKENS,
        installed: false,
        revision: None,
        path: None,
        missing_files: missing,
    }
}

/// Download the model files into `dir` (the only network operation in Midnight). Idempotent: files
/// already in the cache are reused. Does not need or load the ONNX Runtime.
pub fn pull_model(dir: &Path, progress: bool) -> Result<ModelStatus> {
    std::fs::create_dir_all(dir)
        .with_context(|| format!("create models directory {}", dir.display()))?;
    let api = hf_hub::api::sync::ApiBuilder::new()
        .with_cache_dir(dir.to_path_buf())
        .with_progress(progress)
        .build()
        .context("initialize model downloader")?;
    let repo = api.model(MODEL_REPO.to_string());
    for file in MODEL_FILES {
        repo.get(file)
            .with_context(|| format!("download {MODEL_REPO}/{file}"))?;
    }
    let status = model_status(dir);
    if !status.installed {
        bail!(
            "download finished but model files are missing: {:?}",
            status.missing_files
        );
    }
    Ok(status)
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
        ensure_ort_library(&dir)?;
        let read = |name: &str| {
            std::fs::read(path.join(name)).with_context(|| format!("read model file {name}"))
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

/// ONNX Runtime is loaded dynamically (no prebuilt static binary exists for every target). Find a
/// library before initialization so a missing runtime is an error, not a panic.
fn ensure_ort_library(models: &Path) -> Result<()> {
    let name = if cfg!(target_os = "macos") {
        "libonnxruntime.dylib"
    } else if cfg!(windows) {
        "onnxruntime.dll"
    } else {
        "libonnxruntime.so"
    };
    if let Some(explicit) = std::env::var_os("ORT_DYLIB_PATH") {
        if Path::new(&explicit).is_file() {
            return Ok(());
        }
        bail!(
            "ORT_DYLIB_PATH points to a missing file: {}",
            Path::new(&explicit).display()
        );
    }
    let candidates = [
        models.join("onnxruntime").join(name),
        models.join("onnxruntime").join("lib").join(name),
        PathBuf::from("/opt/homebrew/lib").join(name),
        PathBuf::from("/usr/local/lib").join(name),
        PathBuf::from("/usr/lib").join(name),
    ];
    match candidates.iter().find(|p| p.is_file()) {
        Some(found) => {
            // Single-threaded CLI path: nothing else reads the environment concurrently.
            unsafe { std::env::set_var("ORT_DYLIB_PATH", found) };
            Ok(())
        }
        None => bail!(
            "ONNX Runtime library ({name}) not found. Set ORT_DYLIB_PATH or place it at {}",
            models.join("onnxruntime").join(name).display()
        ),
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

fn sort_hits<T>(hits: &mut [(T, f32)], tie: impl Fn(&T, &T) -> std::cmp::Ordering) {
    hits.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| tie(&a.0, &b.0)));
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

fn chunk_text(text: &str) -> (Vec<Chunk>, bool) {
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

/// Incrementally embed the indexed files of one repository/worktree as line-window chunks. Files
/// that changed on disk since indexing are skipped (their vectors are removed): embed after `index`.
pub fn embed_code(
    db_path: &Path,
    root: &Path,
    embedder: &mut dyn TextEmbedder,
    cancel: &AtomicBool,
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
    let files: Vec<(i64, String, String)> = {
        let mut stmt = conn.prepare(
            "SELECT id,rel_path,content_hash FROM code_files
             WHERE repo_id=?1 AND worktree_id=?2 ORDER BY rel_path",
        )?;
        stmt.query_map(params![scope.repo_id, scope.worktree_id], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })?
        .collect::<rusqlite::Result<_>>()?
    };
    for (file_id, rel_path, file_hash) in files {
        if cancel.load(Ordering::Relaxed) {
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
                continue;
            }
        };
        let (chunks, truncated) = chunk_text(&text);
        if truncated {
            report.truncated_files += 1;
        }
        let hashes: Vec<String> = chunks
            .iter()
            .map(|c| hash_bytes(format!("{file_hash}\n{}", c.text).as_bytes()))
            .collect();
        let existing: HashMap<i64, String> = {
            let mut stmt = conn.prepare(
                "SELECT chunk,source_hash FROM embeddings WHERE kind='code' AND owner_id=?1",
            )?;
            stmt.query_map([file_id], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<rusqlite::Result<_>>()?
        };
        let current_match = existing.len() == chunks.len()
            && chunks
                .iter()
                .zip(&hashes)
                .all(|(c, h)| existing.get(&(c.index as i64)) == Some(h));
        if current_match {
            report.unchanged += chunks.len();
            continue;
        }
        let texts: Vec<&str> = chunks.iter().map(|c| c.text.as_str()).collect();
        let mut vectors = Vec::with_capacity(chunks.len());
        for batch in texts.chunks(BATCH) {
            vectors.extend(embedder.embed(TextKind::Passage, batch)?);
        }
        if vectors.len() != chunks.len() {
            bail!(
                "embedder returned {} vectors for {} chunks",
                vectors.len(),
                chunks.len()
            );
        }
        let tx = conn.unchecked_transaction()?;
        tx.execute(
            "DELETE FROM embeddings WHERE kind='code' AND owner_id=?1",
            [file_id],
        )?;
        for ((chunk, hash), vector) in chunks.iter().zip(&hashes).zip(&vectors) {
            tx.execute(
                "INSERT INTO embeddings(kind,scope,owner_id,chunk,line_start,line_end,model,revision,dims,source_hash,vector)
                 VALUES('code',?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
                params![
                    key,
                    file_id,
                    chunk.index as i64,
                    chunk.line_start,
                    chunk.line_end,
                    id.name,
                    id.revision,
                    id.dimensions as i64,
                    hash,
                    encode_vector(vector)
                ],
            )?;
        }
        tx.commit()?;
        report.embedded += chunks.len();
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

const MEMORY_COLUMNS: &str = "m.id,m.scope,m.kind,m.content,m.source_uri,m.source_id,m.supersedes_id,m.created_at,m.observed_at,m.valid_from,m.valid_to";

fn memory_knn(
    conn: &Connection,
    scope: &str,
    id: &ModelIdentity,
    query: &[f32],
    now: &str,
    k: usize,
) -> Result<(Vec<(i64, f32)>, Coverage)> {
    let mut stmt = conn.prepare(
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
        let blob: Vec<u8> = row.get(1)?;
        if let Some(sim) = dot_with_blob(&blob, query) {
            hits.push((owner, sim));
        }
    }
    let embedded = hits.len() as u64;
    sort_hits(&mut hits, |a, b| a.cmp(b));
    hits.truncate(k);
    let eligible: i64 = conn.query_row(
        "SELECT count(*) FROM memories WHERE scope=?1 AND deleted_at IS NULL
           AND (valid_from IS NULL OR valid_from<=?2) AND (valid_to IS NULL OR valid_to>?2)",
        params![scope, now],
        |r| r.get(0),
    )?;
    Ok((
        hits,
        Coverage {
            eligible: eligible as u64,
            embedded,
        },
    ))
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
    let limit = limit.clamp(1, 100);
    let conn = open_db(db_path)?;
    let mut meta = info(mode);
    let mut semantic: Option<(Vec<(i64, f32)>, ModelIdentity)> = None;
    if mode != SearchMode::Lexical {
        match prepare_query(
            &conn,
            "memory",
            scope,
            query,
            loader,
            "aether-midnight embed --namespace <ns>",
        ) {
            Prepared::Ready(embedder, vector) => {
                let id = embedder.identity().clone();
                let now = crate::db::now_utc();
                let (hits, coverage) = memory_knn(&conn, scope, &id, &vector, &now, POOL)?;
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
        memory::recall(
            db_path,
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
            if let Some(memory) = fetch_memory(&conn, scope, *id)? {
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
            None => match fetch_memory(&conn, scope, id)? {
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
    let mut stmt = conn.prepare(
        "SELECT f.rel_path,e.chunk,e.line_start,e.line_end,e.vector,f.content_hash,f.indexed_generation
         FROM embeddings e JOIN code_files f ON f.id=e.owner_id
         WHERE e.kind='code' AND e.scope=?1 AND e.model=?2 AND e.revision=?3 AND e.dims=?4
           AND f.repo_id=?5 AND f.worktree_id=?6",
    )?;
    let mut best: HashMap<String, CodeHit> = HashMap::new();
    let mut embedded_files = std::collections::HashSet::new();
    let mut rows = stmt.query(params![
        code_scope_key(scope),
        id.name,
        id.revision,
        id.dimensions as i64,
        scope.repo_id,
        scope.worktree_id
    ])?;
    while let Some(row) = rows.next()? {
        let path: String = row.get(0)?;
        let blob: Vec<u8> = row.get(4)?;
        let Some(similarity) = dot_with_blob(&blob, query) else {
            continue;
        };
        embedded_files.insert(path.clone());
        let hit = CodeHit {
            path: path.clone(),
            chunk: row.get(1)?,
            line_start: row.get::<_, Option<u32>>(2)?.unwrap_or(1),
            line_end: row.get::<_, Option<u32>>(3)?.unwrap_or(1),
            similarity,
            file_hash: row.get(5)?,
            generation: row.get::<_, i64>(6)? as u64,
        };
        match best.get(&path) {
            Some(cur)
                if cur.similarity > similarity
                    || (cur.similarity == similarity && cur.chunk <= hit.chunk) => {}
            _ => {
                best.insert(path, hit);
            }
        }
    }
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
    sort_hits(&mut hits, |a, b| a.path.cmp(&b.path));
    hits.truncate(20);
    Ok((
        hits.into_iter().map(|h| h.0).collect(),
        Coverage {
            eligible: eligible as u64,
            embedded: embedded_files.len() as u64,
        },
    ))
}

/// Code search in `lexical`, `semantic` or `hybrid` mode for one repository/worktree. Semantic
/// hits are chunk-level and are verified against the current file hash before being returned.
pub fn search_code(
    db_path: &Path,
    root: &Path,
    query: &str,
    mode: SearchMode,
    limit: usize,
    token_budget: usize,
    loader: Loader<'_>,
) -> Result<CodeHybridResponse> {
    if query.trim().is_empty() {
        bail!("code search query cannot be empty");
    }
    let scope = repo_scope(root)?;
    let limit = limit.clamp(1, 20);
    let budget = token_budget.clamp(64, code::MAX_TOKEN_BUDGET);
    let conn = open_db(db_path)?;
    let mut meta = info(mode);
    let mut semantic: Option<(Vec<CodeHit>, ModelIdentity)> = None;
    if mode != SearchMode::Lexical {
        match prepare_query(
            &conn,
            "code",
            &code_scope_key(&scope),
            query,
            loader,
            "aether-midnight embed --repo <path>",
        ) {
            Prepared::Ready(embedder, vector) => {
                let id = embedder.identity().clone();
                let (hits, coverage) = code_knn(&conn, &scope, &id, &vector)?;
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
        Some(code::search_code_with_budget(
            db_path,
            root,
            query,
            if semantic.is_some() { 20 } else { limit },
            if semantic.is_some() {
                code::MAX_TOKEN_BUDGET
            } else {
                budget
            },
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
            "SELECT generation,is_complete,structural_complete FROM repositories WHERE repo_id=?1 AND worktree_id=?2",
            params![scope.repo_id, scope.worktree_id],
            |r| Ok((r.get::<_, i64>(0)? as u64, r.get::<_, i64>(1)? != 0 && r.get::<_, i64>(2)? != 0)),
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
        let snippet: String = text
            .lines()
            .skip(hit.line_start.saturating_sub(1) as usize)
            .take(((hit.line_end + 1).saturating_sub(hit.line_start)).min(8) as usize)
            .collect::<Vec<_>>()
            .join("\n");
        let (snippet, truncated) = code::bounded_snippet(snippet);
        let mut breakdown = BTreeMap::new();
        breakdown.insert("semantic_cosine".to_string(), f64::from(hit.similarity));
        semantic_results.insert(
            hit.path.clone(),
            CodeResult {
                path: hit.path.clone(),
                line_start: Some(hit.line_start),
                line_end: Some(hit.line_end),
                match_reason: "semantic".into(),
                scope: scope_label.clone(),
                index_generation: hit.generation,
                index_complete: complete,
                freshness: "fresh".into(),
                snippet: Some(snippet),
                snippet_truncated: truncated,
                estimated_tokens: 0,
                score: f64::from(hit.similarity),
                score_breakdown: breakdown,
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
        meta.fusion = Some(format!("rrf(k={RRF_K})"));
        for (path, score) in rrf_fuse(&[&lex_paths, &sem_paths]).into_iter().take(limit) {
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
    if token_estimator.is_empty() {
        token_estimator =
            "midnight-heuristic-v1 (identifier chunks, punctuation, and fixed metadata allowance)"
                .into();
    }
    Ok(CodeHybridResponse {
        info: meta,
        search: CodeSearchResponse {
            results,
            token_budget: budget,
            estimated_tokens,
            truncated: truncated_flag || packed_truncated || was_at_limit || !complete,
            token_estimator,
        },
    })
}

#[cfg(test)]
mod tests;
