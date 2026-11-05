use anyhow::{Context, Result, bail};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;
use tempfile::TempDir;

use crate::{code, memory};

#[derive(Debug, Deserialize)]
struct Manifest {
    fixture: Fixture,
    queries: Vec<BenchmarkQuery>,
}
#[derive(Debug, Deserialize)]
struct Fixture {
    seed: u64,
    eligible_files: u64,
    eligible_bytes: u64,
    generator: String,
}
#[derive(Debug, Deserialize)]
struct BenchmarkQuery {
    id: String,
    #[serde(alias = "category", default)]
    kind: String,
    query: String,
    #[serde(default)]
    relevant_paths: Vec<String>,
    #[serde(default)]
    relevant: Vec<GroundTruth>,
    #[serde(default)]
    expected: String,
    #[serde(default)]
    expected_behavior: String,
}
#[derive(Debug, Deserialize, Serialize)]
struct GroundTruth {
    path: String,
    symbol: Option<String>,
}
#[derive(Debug, Deserialize)]
struct QuerySuite {
    queries: Vec<BenchmarkQuery>,
}

pub fn run(
    _requested_db: &Path,
    repo: &Path,
    heldout_repo: &Path,
    manifest_path: &Path,
    samples: usize,
    index_samples: usize,
    stress_cycles: usize,
) -> Result<Value> {
    let repo = repo
        .canonicalize()
        .with_context(|| format!("resolve benchmark corpus {}", repo.display()))?;
    let manifest: Manifest = serde_json::from_slice(
        &std::fs::read(manifest_path)
            .with_context(|| format!("read manifest {}", manifest_path.display()))?,
    )
    .context("parse benchmark manifest")?;
    if manifest.queries.is_empty() {
        bail!("benchmark manifest has no retrieval queries");
    }
    let base = TempDir::new().context("create isolated benchmark database directory")?;
    static NEXT_RUN: AtomicU64 = AtomicU64::new(1);
    let run_id = format!(
        "{}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos(),
        NEXT_RUN.fetch_add(1, Ordering::Relaxed)
    );
    let primary_db = base.path().join("midnight.sqlite3");
    let run_count = index_samples.max(1);
    let mut full_index_ms = Vec::with_capacity(run_count);
    let mut full_index_database_bytes = Vec::with_capacity(run_count);
    let mut wal_checkpoint_ms = Vec::with_capacity(run_count);
    let mut full_index_samples = Vec::with_capacity(run_count);
    let mut index_reports = Vec::with_capacity(run_count);
    let cancellation = std::sync::atomic::AtomicBool::new(false);
    for sample in 0..run_count {
        let db_path = if sample == 0 {
            primary_db.clone()
        } else {
            base.path().join(format!("index-sample-{sample}.sqlite3"))
        };
        let before = Instant::now();
        let report = code::index_repository(&db_path, &repo, false, true, &cancellation)?;
        let elapsed = before.elapsed().as_secs_f64() * 1000.0;
        if !report.complete {
            bail!("complete-index benchmark unexpectedly produced a partial index");
        }
        full_index_ms.push(elapsed);
        let checkpoint_started = Instant::now();
        code::wal_checkpoint(&db_path)?;
        let checkpoint_elapsed = checkpoint_started.elapsed().as_secs_f64() * 1000.0;
        wal_checkpoint_ms.push(checkpoint_elapsed);
        full_index_database_bytes.push(code::database_bytes(&db_path));
        full_index_samples.push(json!({
            "sample_number":sample+1,
            "cache_state":"OS file cache unknown; the harness does not evict it",
            "sample_id":report.sample_id,
            "full_index_ready_ms":elapsed,
            "wal_checkpoint_ms":checkpoint_elapsed,
            "report":report.clone()
        }));
        index_reports.push(report);
    }
    let primary_report = &index_reports[0];
    let primary_status = code::index_status(&primary_db, &repo)?;
    if primary_status.eligible_files != manifest.fixture.eligible_files
        || primary_status.eligible_bytes != manifest.fixture.eligible_bytes
    {
        bail!(
            "reference corpus mismatch: expected {} eligible files/{} bytes, measured {}/{}; preserve the benchmark contract",
            manifest.fixture.eligible_files,
            manifest.fixture.eligible_bytes,
            primary_status.eligible_files,
            primary_status.eligible_bytes
        );
    }

    let cold_query = &manifest.queries[0].query;
    let binary = std::env::current_exe().context("locate benchmark executable")?;
    let mut first_useful_ms = Vec::with_capacity(samples.max(1));
    for sample in 0..samples.max(1) {
        let empty_db = base.path().join(format!("empty-{sample}.sqlite3"));
        let before = Instant::now();
        let output = Command::new(&binary)
            .arg("--db")
            .arg(&empty_db)
            .arg("search")
            .arg("--repo")
            .arg(&repo)
            .arg("--query")
            .arg(cold_query)
            .arg("--limit")
            .arg("5")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .context("launch cold-process grounded search")?;
        let elapsed = before.elapsed().as_secs_f64() * 1000.0;
        if !output.status.success() {
            bail!(
                "fresh-process search failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        let value: Value =
            serde_json::from_slice(&output.stdout).context("parse fresh-process search output")?;
        if !value["results"].as_array().is_some_and(|a| !a.is_empty()) {
            bail!("fresh-process search did not return grounded source results");
        }
        first_useful_ms.push(elapsed);
    }

    let mut search_ms = Vec::with_capacity(samples.max(1));
    for index in 0..samples.max(1) {
        let q = &manifest.queries[index % manifest.queries.len()];
        let before = Instant::now();
        let results = code::search_code(&primary_db, &repo, &q.query, 5)?;
        let elapsed = before.elapsed().as_secs_f64() * 1000.0;
        if results.is_empty() {
            bail!(
                "warm search returned no results for manifest query {}",
                q.id
            );
        }
        search_ms.push(elapsed);
    }

    let quality = retrieval_quality(&primary_db, &repo, &manifest.queries)?;
    let suite_dir = manifest_path.parent().unwrap_or(Path::new("."));
    let dev_suite: QuerySuite =
        serde_json::from_slice(&fs::read(suite_dir.join("suites/dev.json"))?)?;
    let heldout_suite: QuerySuite =
        serde_json::from_slice(&fs::read(suite_dir.join("suites/heldout.json"))?)?;
    let dev_repo = suite_dir
        .join("retrieval_fixture")
        .canonicalize()
        .context("resolve frozen dev retrieval fixture")?;
    let heldout_repo = heldout_repo
        .canonicalize()
        .context("resolve clean held-out repository archive")?;
    let dev_db = base.path().join("dev-retrieval.sqlite3");
    let dev_index = code::index_repository(&dev_db, &dev_repo, false, true, &cancellation)?;
    if !dev_index.complete {
        bail!("dev retrieval fixture index was incomplete");
    }
    let heldout_db = base.path().join("heldout-retrieval.sqlite3");
    let heldout_index =
        code::index_repository(&heldout_db, &heldout_repo, false, true, &cancellation)?;
    if !heldout_index.complete {
        bail!("held-out repository index was incomplete");
    }
    let dev_quality = retrieval_suite_quality(&dev_db, &dev_repo, &dev_suite.queries)?;
    let heldout_quality =
        retrieval_suite_quality(&heldout_db, &heldout_repo, &heldout_suite.queries)?;
    let rg = ripgrep_baseline(&repo, &manifest.queries)?;
    let memory_scaling = memory_scaling(base.path(), &[1_000, 10_000, 100_000], false, 4)?;
    let incremental = incremental_samples(&primary_db, &repo, 30)?;
    let partial_context = partial_context_during_index(base.path(), &repo, cold_query, 30)?;
    let stress = stress_cycles_run(base.path(), stress_cycles.max(1))?;
    let normalized_ru_maxrss_bytes =
        code::peak_rss_bytes().context("read normalized process peak RSS")?;
    let memory_sampled_rss_peak = memory_scaling["workloads"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|workload| workload["max_sampled_rss_bytes"].as_u64())
        .max()
        .unwrap_or(0);
    let stress_sampled_rss_peak = stress["rss_samples_bytes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_u64)
        .max()
        .unwrap_or(0);
    let max_sampled_rss_bytes = memory_sampled_rss_peak.max(stress_sampled_rss_peak);
    let peak_process_rss_bytes = normalized_ru_maxrss_bytes.max(max_sampled_rss_bytes);
    if peak_process_rss_bytes < max_sampled_rss_bytes {
        bail!(
            "process peak RSS {peak_process_rss_bytes} bytes is below sampled RSS {max_sampled_rss_bytes} bytes"
        );
    }

    let source_bytes = primary_status.eligible_bytes.max(1);
    let index_bytes = code::database_bytes(&primary_db);
    let sqlite = code::sqlite_version(&primary_db)?;
    let mut per_stage = serde_json::Map::new();
    if let Some(first) = index_reports.first() {
        for key in first.timings_ms.keys() {
            if key == "phase_accounted_total" || key == "full_index_ready" {
                continue;
            }
            let values = index_reports
                .iter()
                .filter_map(|report| report.timings_ms.get(key).copied())
                .collect::<Vec<_>>();
            per_stage.insert(key.clone(), distribution(&values));
        }
    }
    let database_path_bytes = std::fs::metadata(&primary_db).map_or(0, |m| m.len());
    let wal_path_bytes =
        std::fs::metadata(format!("{}-wal", primary_db.display())).map_or(0, |m| m.len());
    let shm_path_bytes =
        std::fs::metadata(format!("{}-shm", primary_db.display())).map_or(0, |m| m.len());
    let integrity_check: String =
        crate::db::open_db(&primary_db)?
            .query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
    if integrity_check != "ok" {
        bail!("SQLite integrity_check failed: {integrity_check}");
    }
    let output = json!({
        "schema_version":1,
        "run_id":run_id,
        "measurement_conditions":{
            "scope":"locally generated deterministic reference fixture",
            "network":"not used",
            "database_cache":"fresh database for full index; warm database for search; fresh process with empty index for first useful context",
            "filesystem_cache":"not evicted; first cold-search sample is OS-cache-unknown and later samples may be warm",
            "timers":"process launch through grounded JSON result for first useful context; full-index-ready excludes the separately reported WAL checkpoint; worker stage work is summed across workers and may overlap; operation start through query-visible result for warm and incremental calls",
            "build_excluded":true
        },
        "environment":environment(&repo,&sqlite),
        "fixture":{
            "seed":manifest.fixture.seed,"generator":manifest.fixture.generator,
            "eligible_files":primary_status.eligible_files,"eligible_bytes":primary_status.eligible_bytes,
            "skipped_files":primary_report.skipped.values().sum::<u64>(),"skip_reasons":primary_report.skipped,
            "index_bytes":index_bytes,"index_to_source_ratio":index_bytes as f64/source_bytes as f64,
            "database_bytes_distribution":distribution_u64(&full_index_database_bytes),
            "index_to_source_ratio_samples":distribution_f64(&full_index_database_bytes.iter().map(|n|*n as f64/source_bytes as f64).collect::<Vec<_>>())
        },
        "timings":{
            "first_useful_context_fresh_process_empty_index":distribution(&first_useful_ms),
            "full_index_fresh_database":distribution(&full_index_ms),
            "full_index_phases_first_sample_ms":primary_report.timings_ms,
            "full_index_stage_distributions_ms":per_stage,
            "full_index_samples":full_index_samples,
            "wal_checkpoint":distribution(&wal_checkpoint_ms),
            "warm_search":distribution(&search_ms),
            "incremental_update_query_visible":distribution(&incremental),
            "large_repository_partial_context_during_index":partial_context,
            "ripgrep_identical_query_set":rg
        },
        "retrieval_quality":quality,
        "frozen_query_suites":{
            "dev":{"corpus":"benchmark/retrieval_fixture","query_suite":"benchmark/suites/dev.json","evaluation":dev_quality},
            "held_out":{"corpus_root":heldout_repo,"query_suite":"benchmark/suites/heldout.json",
                "frozen_corpus_commit":"950e8f4bec475051e365b5f51e4953fb12972f0b",
                "indexed_files":heldout_index.eligible_files,"indexed_bytes":heldout_index.eligible_bytes,"evaluation":heldout_quality},
            "frozen_before_measurement":true,"ranking_tuned_after_heldout":false,"contaminated":false
        },
        "memory_scaling":memory_scaling,
        "resources":{
            "peak_process_rss_bytes":peak_process_rss_bytes,
            "normalized_ru_maxrss_bytes":normalized_ru_maxrss_bytes,
            "max_sampled_rss_bytes":max_sampled_rss_bytes,
            "peak_rss_ge_every_sampled_rss":true,
            "peak_process_rss_status":"maximum of normalized getrusage high-water and sampled current RSS; asserted against every sampled RSS",
            "allocator_live_heap_bytes":Value::Null,
            "memory_mapped_pages_bytes":Value::Null,
            "rss_measurement_scope":"benchmark process only; peak high-water mark",
            "index_database_bytes":database_path_bytes,
            "index_wal_bytes":wal_path_bytes,
            "index_shm_bytes":shm_path_bytes,
            "maximum_in_flight_bytes":index_reports.iter().map(|r|r.max_in_flight_bytes).max().unwrap_or(0),
            "maximum_pipeline_buffered_bytes":index_reports.iter().map(|r|r.max_pipeline_buffered_bytes).max().unwrap_or(0),
            "maximum_queued_artifacts":index_reports.iter().map(|r|r.max_queued_artifacts).max().unwrap_or(0),
            "maximum_pipeline_artifacts":index_reports.iter().map(|r|r.max_pipeline_artifacts).max().unwrap_or(0),
            "rss_after_stress_cycles_bytes":stress.get("rss_samples_bytes").cloned().unwrap_or(json!([])),
            "rss_after_stress_cycles_distribution":distribution_u64(&stress.get("rss_samples_bytes").and_then(Value::as_array).cloned().unwrap_or_default().iter().filter_map(Value::as_u64).collect::<Vec<_>>()),
            "stress":stress,
            "unavailable_measurements":"allocator live heap and mmap pages are not instrumented on this build"
        },
        "index_status":primary_status,
        "sqlite_integrity_check":integrity_check,
        "database_file_bytes":index_bytes
    });
    Ok(output)
}

pub fn evaluate_fts5(repo: &Path, manifest_path: &Path, samples: usize) -> Result<Value> {
    let repo = repo
        .canonicalize()
        .with_context(|| format!("resolve tokenizer-evaluation corpus {}", repo.display()))?;
    let manifest: Manifest = serde_json::from_slice(&fs::read(manifest_path)?)?;
    let mut files = fs::read_dir(repo.join("src"))?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension().is_some_and(|extension| extension == "rs"))
        .collect::<Vec<_>>();
    files.sort();
    let source_bytes = files
        .iter()
        .map(|path| fs::metadata(path).map(|metadata| metadata.len()))
        .collect::<std::io::Result<Vec<_>>>()?
        .into_iter()
        .sum::<u64>();
    if files.len() as u64 != manifest.fixture.eligible_files
        || source_bytes != manifest.fixture.eligible_bytes
    {
        bail!(
            "tokenizer evaluation fixture mismatch: expected {}/{} bytes, found {}/{}",
            manifest.fixture.eligible_files,
            manifest.fixture.eligible_bytes,
            files.len(),
            source_bytes
        );
    }
    let temporary = TempDir::new().context("create tokenizer evaluation databases")?;
    let tokenizer_configs = [
        ("trigram", "trigram case_sensitive 0"),
        ("unicode61", "unicode61 remove_diacritics 2"),
    ];
    let mut configurations = serde_json::Map::new();
    let mut runtime_version = String::new();
    for (name, definition) in tokenizer_configs {
        let database = temporary.path().join(format!("{name}.sqlite3"));
        let build_started = Instant::now();
        let connection = build_tokenizer_database(&database, &repo, &files, definition)?;
        runtime_version = connection.query_row("SELECT sqlite_version()", [], |row| row.get(0))?;
        connection.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;
        let build_ms = build_started.elapsed().as_secs_f64() * 1000.0;
        let index_bytes = sqlite_file_bytes(&database);
        let evaluated =
            evaluate_tokenizer_queries(&connection, name, &manifest.queries, samples.max(1))?;
        configurations.insert(
            name.to_string(),
            json!({
                "sqlite_version":runtime_version,
                "build_ms":build_ms,
                "index_bytes":index_bytes,
                "index_to_source_ratio":index_bytes as f64/source_bytes.max(1) as f64,
                "eligible_files":files.len(),
                "eligible_bytes":source_bytes,
                "warm_search_ms":evaluated["warm_search_ms"],
                "recall_at_5":evaluated["recall_at_5"],
                "mrr":evaluated["mrr"],
                "queries":evaluated["queries"]
            }),
        );
        drop(connection);
    }
    Ok(json!({
        "schema_version":1,
        "sqlite_version":runtime_version,
        "runtime":"rusqlite bundled SQLite",
        "fixture_seed":manifest.fixture.seed,
        "eligible_files":files.len(),
        "eligible_bytes":source_bytes,
        "insertion":"same source read, document table insert, FTS insert, 512-file transaction batches, WAL/NORMAL synchronous and checkpoint for each tokenizer",
        "queries":"same content-query subset of benchmark/manifest.json for each tokenizer; path query excluded from content search",
        "samples_per_query":samples.max(1),
        "tokenizers":configurations
    }))
}

fn build_tokenizer_database(
    database: &Path,
    repo: &Path,
    files: &[std::path::PathBuf],
    tokenizer: &str,
) -> Result<Connection> {
    let mut connection = Connection::open(database)?;
    connection.execute_batch(&format!(
        "PRAGMA journal_mode=WAL;
         PRAGMA synchronous=NORMAL;
         PRAGMA temp_store=FILE;
         CREATE TABLE documents(id INTEGER PRIMARY KEY,path TEXT NOT NULL);
         CREATE VIRTUAL TABLE content_fts USING fts5(
             content,content='',contentless_delete=1,tokenize='{tokenizer}'
         );"
    ))?;
    for (batch_number, batch) in files.chunks(512).enumerate() {
        let mut rows = Vec::with_capacity(batch.len());
        for (offset, path) in batch.iter().enumerate() {
            let text = fs::read_to_string(path)
                .with_context(|| format!("read tokenizer corpus file {}", path.display()))?;
            let document_id = (batch_number * 512 + offset + 1) as i64;
            let relative = path
                .strip_prefix(repo)
                .context("tokenizer corpus file escaped the root")?
                .to_string_lossy()
                .replace('\\', "/");
            rows.push((document_id, relative, text));
        }
        let transaction = connection.transaction()?;
        {
            let mut document_insert =
                transaction.prepare_cached("INSERT INTO documents(id,path) VALUES(?1,?2)")?;
            let mut fts_insert = transaction
                .prepare_cached("INSERT INTO content_fts(rowid,content) VALUES(?1,?2)")?;
            for (document_id, path, content) in rows {
                document_insert.execute(params![document_id, path])?;
                fts_insert.execute(params![document_id, content])?;
            }
        }
        transaction.commit()?;
    }
    Ok(connection)
}

fn evaluate_tokenizer_queries(
    connection: &Connection,
    tokenizer: &str,
    queries: &[BenchmarkQuery],
    samples: usize,
) -> Result<Value> {
    let mut details = Vec::new();
    let mut total_recall = 0.0;
    let mut total_reciprocal = 0.0;
    let mut all_samples = Vec::new();
    for query in queries.iter().filter(|query| query.kind != "path") {
        let expression = tokenizer_match_query(&query.query, tokenizer);
        let mut elapsed_samples = Vec::new();
        let top_paths = if let Some(expression) = expression {
            let sql = "SELECT documents.path FROM content_fts \
                       JOIN documents ON documents.id=content_fts.rowid \
                       WHERE content_fts MATCH ?1 \
                       ORDER BY bm25(content_fts),documents.path LIMIT 5";
            let mut statement = connection.prepare_cached(sql)?;
            let rows = statement.query_map([&expression], |row| row.get::<_, String>(0))?;
            let mut paths = Vec::new();
            for row in rows {
                paths.push(row?);
            }
            for _ in 0..samples {
                let started = Instant::now();
                statement
                    .query_map([&expression], |row| row.get::<_, String>(0))?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                elapsed_samples.push(started.elapsed().as_secs_f64() * 1000.0);
            }
            paths
        } else {
            Vec::new()
        };
        all_samples.extend(elapsed_samples.iter().copied());
        let relevant = query
            .relevant_paths
            .iter()
            .collect::<std::collections::HashSet<_>>();
        let ranks = top_paths
            .iter()
            .enumerate()
            .filter_map(|(index, path)| relevant.contains(path).then_some(index + 1))
            .collect::<Vec<_>>();
        let recall = ranks.len() as f64 / relevant.len().max(1) as f64;
        let reciprocal = ranks.first().map_or(0.0, |rank| 1.0 / *rank as f64);
        total_recall += recall;
        total_reciprocal += reciprocal;
        details.push(json!({
            "id":query.id,
            "query":query.query,
            "top5":top_paths,
            "recall_at_5":recall,
            "reciprocal_rank":reciprocal,
            "search_samples_ms":elapsed_samples
        }));
    }
    let count = details.len().max(1) as f64;
    Ok(json!({
        "recall_at_5":total_recall/count,
        "mrr":total_reciprocal/count,
        "warm_search_ms":distribution(&all_samples),
        "queries":details
    }))
}

fn tokenizer_match_query(query: &str, tokenizer: &str) -> Option<String> {
    let terms = query
        .split(|ch: char| !(ch.is_alphanumeric() || ch == '_'))
        .filter(|term| term.chars().any(char::is_alphanumeric))
        .filter(|term| tokenizer != "trigram case_sensitive 0" || term.chars().count() >= 3)
        .map(|term| format!("\"{}\"", term.replace('"', "\"\"")))
        .collect::<Vec<_>>();
    (!terms.is_empty()).then(|| terms.join(" OR "))
}

fn sqlite_file_bytes(path: &Path) -> u64 {
    [
        path.to_path_buf(),
        PathBuf::from(format!("{}-wal", path.display())),
        PathBuf::from(format!("{}-shm", path.display())),
    ]
    .iter()
    .filter_map(|path| fs::metadata(path).ok())
    .map(|metadata| metadata.len())
    .sum()
}

pub fn run_memory_scaling(sizes: &[usize]) -> Result<Value> {
    run_memory_scaling_with_profile(sizes, false)
}

pub fn run_memory_scaling_with_profile(sizes: &[usize], profile_recall: bool) -> Result<Value> {
    run_memory_scaling_with_layout(sizes, profile_recall, 4)
}

pub fn run_memory_scaling_with_layout(
    sizes: &[usize],
    profile_recall: bool,
    namespace_count: usize,
) -> Result<Value> {
    if sizes.is_empty() || sizes.iter().any(|size| !(4..=100_000).contains(size)) {
        bail!("memory benchmark sizes must be between 4 and 100000 records");
    }
    if !matches!(namespace_count, 1 | 4) {
        bail!("memory benchmark namespace count must be 1 or 4");
    }
    let base = TempDir::new().context("create isolated memory benchmark directory")?;
    memory_scaling(base.path(), sizes, profile_recall, namespace_count)
}

fn memory_scaling(
    base: &Path,
    requested_sizes: &[usize],
    profile_recall: bool,
    namespace_count: usize,
) -> Result<Value> {
    let all_scopes = ["bench:alpha", "bench:beta", "bench:gamma", "bench:delta"];
    let scopes = &all_scopes[..namespace_count];
    let mut workloads = Vec::new();
    for target in requested_sizes.iter().copied() {
        let workload_cpu_started = code::process_cpu_seconds();
        let db_path = base.join(format!("memory-{target}.sqlite3"));
        let mut source_ids = std::collections::HashMap::new();
        let setup_started = Instant::now();
        let mut memory_session = memory::MemorySession::open(&db_path)?;
        for scope in scopes.iter().copied() {
            let record = memory_session.remember(
                scope,
                "source",
                "benchmark namespace anchor",
                Some("benchmark://anchor"),
                None,
                None,
                None,
                None,
                None,
            )?;
            source_ids.insert(scope, record.id);
        }
        let mut quality = Vec::new();
        let mut batch = Vec::with_capacity(memory::MAX_MEMORY_BATCH_RECORDS);
        let mut batch_labels = Vec::with_capacity(memory::MAX_MEMORY_BATCH_RECORDS);
        for index in 0..(target - scopes.len()) {
            let scope = scopes[index % scopes.len()];
            let (kind, token) = if index < 25 {
                ("source", format!("sourcequality{index:03}"))
            } else if index < 50 {
                ("derived", format!("derivedquality{index:03}"))
            } else if index % 2 == 0 {
                ("source", format!("memoryload{index:06}"))
            } else {
                ("derived", format!("memoryload{index:06}"))
            };
            let source_id = source_ids[scope];
            batch.push(memory::MemoryInput {
                scope: scope.to_string(),
                kind: kind.to_string(),
                content: format!("{token} namespace workload record {index}"),
                source_uri: Some("benchmark://load".into()),
                source_id: (kind == "derived").then_some(source_id),
                supersedes_id: None,
                observed_at: None,
                valid_from: None,
                valid_to: None,
            });
            batch_labels.push((scope.to_string(), token, kind.to_string(), index < 50));
            if batch.len() == memory::MAX_MEMORY_BATCH_RECORDS || index + 1 == target - scopes.len()
            {
                let records = memory_session.remember_batch(&batch)?;
                for (record, (scope, token, kind, is_quality_record)) in
                    records.into_iter().zip(batch_labels.drain(..))
                {
                    if is_quality_record {
                        quality.push((scope, token, record.id, kind));
                    }
                }
                batch.clear();
            }
        }
        let setup_ms = setup_started.elapsed().as_secs_f64() * 1000.0;
        let recall_query_plan =
            memory_session.recall_query_plan(scopes[0], "namespace workload record 0")?;
        code::wal_checkpoint(&db_path)?;
        let db_bytes_before_ops = code::database_bytes(&db_path);

        let mut store_ms = Vec::new();
        let mut database_size_during_store = Vec::new();
        let mut rss_during_store = Vec::new();
        for index in 0..30 {
            let start = Instant::now();
            memory::remember(
                &db_path,
                scopes[index % scopes.len()],
                "source",
                &format!("store sample marker {target} {index}"),
                Some("benchmark://store"),
                None,
                None,
                None,
                None,
                None,
            )?;
            store_ms.push(start.elapsed().as_secs_f64() * 1000.0);
            database_size_during_store.push(code::database_bytes(&db_path));
            if let Some(rss) = current_rss_bytes() {
                rss_during_store.push(rss);
            }
        }

        let mut prior_ids = std::collections::HashMap::new();
        for scope in scopes.iter().copied() {
            let seed = memory::remember(
                &db_path,
                scope,
                "derived",
                "update chain seed",
                Some("benchmark://update"),
                Some(source_ids[scope]),
                None,
                None,
                Some("2020-01-01T00:00:00Z"),
                None,
            )?;
            prior_ids.insert(scope, seed.id);
        }
        let mut update_ms = Vec::new();
        for index in 0..30 {
            let scope = scopes[index % scopes.len()];
            let day = index / scopes.len() + 2;
            let valid_from = format!("2020-01-{day:02}T00:00:00Z");
            let start = Instant::now();
            let updated = memory::remember(
                &db_path,
                scope,
                "derived",
                &format!("updated fact sample {index}"),
                Some("benchmark://update"),
                Some(source_ids[scope]),
                Some(prior_ids[scope]),
                None,
                Some(&valid_from),
                None,
            )?;
            update_ms.push(start.elapsed().as_secs_f64() * 1000.0);
            prior_ids.insert(scope, updated.id);
        }

        let mut delete_ids = Vec::new();
        for index in 0..30 {
            let scope = scopes[index % scopes.len()];
            let record = memory::remember(
                &db_path,
                scope,
                "source",
                &format!("delete sample marker {target} {index}"),
                Some("benchmark://delete"),
                None,
                None,
                None,
                None,
                None,
            )?;
            delete_ids.push((scope, record.id));
        }
        let mut delete_ms = Vec::new();
        for (scope, id) in delete_ids {
            let start = Instant::now();
            if !memory::forget(&db_path, scope, id)? {
                bail!("memory delete benchmark did not delete id {id}");
            }
            delete_ms.push(start.elapsed().as_secs_f64() * 1000.0);
        }

        let mut recall_ms = Vec::new();
        for index in 0..100 {
            let start = Instant::now();
            let result = memory::recall(
                &db_path,
                scopes[index % scopes.len()],
                "namespace",
                None,
                10,
            )?;
            if result.is_empty() {
                bail!("memory recall benchmark returned no results at size {target}");
            }
            recall_ms.push(start.elapsed().as_secs_f64() * 1000.0);
        }

        let mut persistent_store_ms = Vec::new();
        for index in 0..30 {
            let start = Instant::now();
            memory_session.remember(
                scopes[index % scopes.len()],
                "source",
                &format!("persistent store marker {target} {index}"),
                Some("benchmark://persistent-store"),
                None,
                None,
                None,
                None,
                None,
            )?;
            persistent_store_ms.push(start.elapsed().as_secs_f64() * 1000.0);
        }
        let mut persistent_list_ms = Vec::new();
        for _ in 0..30 {
            let start = Instant::now();
            if memory_session.list_memories(scopes[0], 10)?.is_empty() {
                bail!("persistent memory list returned no records at size {target}");
            }
            persistent_list_ms.push(start.elapsed().as_secs_f64() * 1000.0);
        }
        let mut persistent_recall_ms = Vec::new();
        for index in 0..100 {
            let start = Instant::now();
            let result =
                memory_session.recall(scopes[index % scopes.len()], "namespace", None, 10)?;
            if result.is_empty() {
                bail!("persistent memory recall returned no results at size {target}");
            }
            persistent_recall_ms.push(start.elapsed().as_secs_f64() * 1000.0);
        }
        let mut high_fanout_profile_samples = Vec::new();
        let mut rowid_newest_profile_samples = Vec::new();
        let mut rowid_oldest_profile_samples = Vec::new();
        let mut rowid_balanced_profile_samples = Vec::new();
        let mut selective_profile_samples = Vec::new();
        if profile_recall {
            for (query_class, query, candidate_order, samples) in [
                (
                    "high_fanout_ranked",
                    "namespace",
                    memory::RecallCandidateOrder::Ranked,
                    &mut high_fanout_profile_samples,
                ),
                (
                    "high_fanout_rowid_newest",
                    "namespace",
                    memory::RecallCandidateOrder::RowIdDescending,
                    &mut rowid_newest_profile_samples,
                ),
                (
                    "high_fanout_rowid_oldest",
                    "namespace",
                    memory::RecallCandidateOrder::RowIdAscending,
                    &mut rowid_oldest_profile_samples,
                ),
                (
                    "high_fanout_rowid_balanced",
                    "namespace",
                    memory::RecallCandidateOrder::BalancedRowIdWindows,
                    &mut rowid_balanced_profile_samples,
                ),
                (
                    "selective_single_term",
                    "sourcequality000",
                    memory::RecallCandidateOrder::Ranked,
                    &mut selective_profile_samples,
                ),
            ] {
                for _ in 0..100 {
                    let (result, mut profile) = memory_session.recall_profiled_ordered(
                        scopes[0],
                        query,
                        None,
                        10,
                        candidate_order,
                    )?;
                    if query_class.starts_with("high_fanout") && result.is_empty() {
                        bail!("profiled broad memory recall returned no results at size {target}");
                    }
                    if query_class == "selective_single_term" && result.len() != 1 {
                        bail!(
                            "profiled selective memory recall expected one result at size {target}"
                        );
                    }
                    let serialization_started = Instant::now();
                    let serialized = serde_json::to_vec(&result)?;
                    profile.result_bytes_serialized = Some(serialized.len());
                    profile.add_benchmark_wall_phase(
                        "response_serialization",
                        serialization_started
                            .elapsed()
                            .as_nanos()
                            .min(u64::MAX as u128) as u64,
                    );
                    samples.push(serde_json::to_value(&profile)?);
                }
            }
        }
        let mut persistent_delete_ids = Vec::new();
        for index in 0..30 {
            let record = memory_session.remember(
                scopes[index % scopes.len()],
                "source",
                &format!("persistent delete marker {target} {index}"),
                Some("benchmark://persistent-delete"),
                None,
                None,
                None,
                None,
                None,
            )?;
            persistent_delete_ids.push((scopes[index % scopes.len()], record.id));
        }
        let mut persistent_delete_ms = Vec::new();
        for (scope, id) in persistent_delete_ids {
            let start = Instant::now();
            if !memory_session.forget(scope, id)? {
                bail!("persistent memory delete did not delete id {id}");
            }
            persistent_delete_ms.push(start.elapsed().as_secs_f64() * 1000.0);
        }

        let mut quality_scores = [0.0f64; 3];
        let mut mrr = 0.0;
        let mut quality_hits = 0usize;
        let mut leakage_checks = 0usize;
        let mut quality_source_scores = [0.0f64; 3];
        let mut quality_derived_scores = [0.0f64; 3];
        let mut source_count = 0usize;
        let mut derived_count = 0usize;
        let mut source_mrr = 0.0;
        let mut derived_mrr = 0.0;
        let mut quality_rows = Vec::new();
        let mut leakage_rows = Vec::new();
        for (scope, token, expected_id, _kind) in &quality {
            let results = memory::recall(&db_path, scope, token, None, 10)?;
            let rank = results
                .iter()
                .position(|result| result.memory.id == *expected_id);
            if rank.is_some() {
                quality_hits += 1;
            }
            let kind = if token.starts_with("sourcequality") {
                "source"
            } else {
                "derived"
            };
            let (kind_count, kind_scores, kind_mrr) = if kind == "source" {
                (
                    &mut source_count,
                    &mut quality_source_scores,
                    &mut source_mrr,
                )
            } else {
                (
                    &mut derived_count,
                    &mut quality_derived_scores,
                    &mut derived_mrr,
                )
            };
            *kind_count += 1;
            if let Some(rank) = rank {
                for (i, limit) in [1usize, 5, 10].iter().enumerate() {
                    if rank < *limit {
                        quality_scores[i] += 1.0;
                    }
                    if rank < *limit {
                        kind_scores[i] += 1.0;
                    }
                }
                mrr += 1.0 / (rank as f64 + 1.0);
                *kind_mrr += 1.0 / (rank as f64 + 1.0);
            }
            quality_rows.push(json!({"query":token,"namespace":scope,"expected_kind":kind,"expected_memory_id":expected_id,
                "rank":rank.map(|r|r+1),"found":rank.is_some()}));
            for other in scopes.iter().copied().filter(|other| *other != scope) {
                leakage_checks += 1;
                let foreign = memory::recall(&db_path, other, token, None, 10)?;
                leakage_rows.push(json!({"query":token,"target_scope":scope,"queried_foreign_scope":other,"returned_count":foreign.len()}));
                if !foreign.is_empty() {
                    bail!("cross-namespace memory leakage for query {token} into {other}");
                }
            }
        }
        code::wal_checkpoint(&db_path)?;
        let db_bytes_after_ops = code::database_bytes(&db_path);
        let current_rss = current_rss_bytes();
        let normalized_ru_maxrss_bytes =
            code::peak_rss_bytes().context("read normalized process peak RSS")?;
        let max_sampled_rss = rss_during_store
            .iter()
            .copied()
            .chain(current_rss)
            .max()
            .unwrap_or(0);
        let peak_rss = normalized_ru_maxrss_bytes.max(max_sampled_rss);
        if peak_rss < max_sampled_rss {
            bail!(
                "normalized process peak RSS {peak_rss} bytes is below sampled RSS {max_sampled_rss} bytes"
            );
        }
        let persistent_session = json!({
            "store": distribution(&persistent_store_ms),
            "list": distribution(&persistent_list_ms),
            "delete": distribution(&persistent_delete_ms),
            "recall": distribution(&persistent_recall_ms)
        });
        let recall_profile = if profile_recall {
            let page_cache_experiment = profile_page_cache_sizes(&memory_session, scopes[0])?;
            json!({
                "enabled":true,
                "query_plans":{
                    "ranked":memory_session.recall_query_plan_ordered(scopes[0],"namespace",memory::RecallCandidateOrder::Ranked)?,
                    "rowid_newest":memory_session.recall_query_plan_ordered(scopes[0],"namespace",memory::RecallCandidateOrder::RowIdDescending)?,
                    "rowid_oldest":memory_session.recall_query_plan_ordered(scopes[0],"namespace",memory::RecallCandidateOrder::RowIdAscending)?,
                    "rowid_balanced":memory_session.recall_query_plan_ordered(scopes[0],"namespace",memory::RecallCandidateOrder::BalancedRowIdWindows)?
                },
                "high_fanout_ranked":profile_retrieval_samples(&high_fanout_profile_samples),
                "high_fanout_rowid_newest":profile_retrieval_samples(&rowid_newest_profile_samples),
                "high_fanout_rowid_oldest":profile_retrieval_samples(&rowid_oldest_profile_samples),
                "high_fanout_rowid_balanced":profile_retrieval_samples(&rowid_balanced_profile_samples),
                "selective_single_term":profile_retrieval_samples(&selective_profile_samples),
                "page_cache_experiment":page_cache_experiment
            })
        } else {
            json!({"enabled":false})
        };
        let retrieval_quality = json!({"labeled_query_count":quality.len(),"recall_at_1":quality_scores[0]/quality.len().max(1) as f64,
            "recall_at_5":quality_scores[1]/quality.len().max(1) as f64,"recall_at_10":quality_scores[2]/quality.len().max(1) as f64,
            "mrr":mrr/quality.len().max(1) as f64,"found_expected_record_count":quality_hits,"cross_namespace_checks":leakage_checks,
            "by_kind":{"source":{"query_count":source_count,"recall_at_1":quality_source_scores[0]/source_count.max(1) as f64,
                "recall_at_5":quality_source_scores[1]/source_count.max(1) as f64,"recall_at_10":quality_source_scores[2]/source_count.max(1) as f64,"mrr":source_mrr/source_count.max(1) as f64},
                "derived":{"query_count":derived_count,"recall_at_1":quality_derived_scores[0]/derived_count.max(1) as f64,
                "recall_at_5":quality_derived_scores[1]/derived_count.max(1) as f64,"recall_at_10":quality_derived_scores[2]/derived_count.max(1) as f64,"mrr":derived_mrr/derived_count.max(1) as f64}},
            "queries":quality_rows,"cross_namespace_leaks":0,"cross_namespace_isolation":"pass","cross_namespace_checks_raw":leakage_rows});
        let workload_cpu_seconds = workload_cpu_started
            .zip(code::process_cpu_seconds())
            .map(|(started, finished)| (finished - started).max(0.0));
        workloads.push(json!({"memory_count":target,"namespace_count":scopes.len(),"namespaces":scopes,
            "setup_ms":setup_ms,"setup_sample_count":1,"setup_status":"anecdotal","setup_connection_mode":"one warm SQLite session; atomic batches of at most 512 records",
            "setup_records_per_second":(target.saturating_sub(scopes.len())) as f64/(setup_ms/1000.0).max(f64::EPSILON),
            "records_loaded":target,"db_bytes_before_measured_ops":db_bytes_before_ops,
            "db_bytes_after_measured_ops":db_bytes_after_ops,"db_growth_bytes_during_operations":db_bytes_after_ops.saturating_sub(db_bytes_before_ops),
            "database_bytes_during_30_store_samples":distribution_u64(&database_size_during_store),
            "database_growth_bytes_during_30_store_samples":distribution_u64(&database_size_during_store.iter()
                .map(|bytes|bytes.saturating_sub(db_bytes_before_ops)).collect::<Vec<_>>()),
            "current_rss_during_30_store_samples_bytes":distribution_u64(&rss_during_store),
            "store":distribution(&store_ms),"update_by_supersession":distribution(&update_ms),"delete":distribution(&delete_ms),"recall":distribution(&recall_ms),
            "persistent_session":persistent_session,
            "recall_profile":recall_profile,
            "recall_query_plan":recall_query_plan,
            "process_cpu_seconds_for_workload":workload_cpu_seconds,
            "process_cpu_seconds_per_stored_record":workload_cpu_seconds.map(|seconds|seconds/target.max(1) as f64),
            "current_rss_bytes_after_workload":current_rss,"current_rss_after_workload_status":"anecdotal; one observation",
            "process_peak_rss_bytes":peak_rss,"process_peak_rss_status":"max of normalized process getrusage high-water and sampled current RSS; process-wide cumulative across sizes",
            "normalized_ru_maxrss_bytes":normalized_ru_maxrss_bytes,
            "max_sampled_rss_bytes":max_sampled_rss,"peak_rss_ge_every_sampled_rss":true,
            "retrieval_quality":retrieval_quality}));
    }
    Ok(
        json!({"memory_records_include_source_and_derived_across_four_namespaces":namespace_count == 4,"namespace_layout":if namespace_count == 1 {"single_namespace"} else {"four_distributed_namespaces"},"workloads":workloads,
        "rss_method":"current RSS sampled with ps; getrusage ru_maxrss normalized to bytes; reported peak is max(getrusage peak, every sampled RSS) and asserted against all samples; peak is cumulative across sizes"}),
    )
}

fn partial_context_during_index(
    base: &Path,
    repo: &Path,
    query: &str,
    count: usize,
) -> Result<Value> {
    let binary = std::env::current_exe().context("locate benchmark executable")?;
    let mut search_ms = Vec::with_capacity(count);
    let mut details = Vec::with_capacity(count);
    for sample in 0..(count * 10) {
        if search_ms.len() >= count {
            break;
        }
        let temp = TempDir::new_in(base)?;
        let db_path = temp.path().join("partial.sqlite3");
        let mut indexer = Command::new(&binary)
            .arg("--db")
            .arg(&db_path)
            .arg("index")
            .arg("--repo")
            .arg(repo)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .context("start concurrent full-corpus index")?;
        let index_started = Instant::now();
        let deadline = Instant::now() + std::time::Duration::from_secs(3);
        let mut partial_observed = false;
        while Instant::now() < deadline {
            if let Ok(status) = code::index_status(&db_path, repo)
                && status.generation > 0
                && !status.catalog_complete
            {
                partial_observed = true;
                break;
            }
            if indexer.try_wait()?.is_some() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }

        if !partial_observed || indexer.try_wait()?.is_some() {
            let _ = indexer.wait();
            details.push(json!({"sample":sample,"partial_catalog_observed":partial_observed,"index_active_before_search":false}));
            continue;
        }
        let search_started = Instant::now();
        let output = Command::new(&binary)
            .arg("--db")
            .arg(&db_path)
            .arg("search")
            .arg("--repo")
            .arg(repo)
            .arg("--query")
            .arg(query)
            .arg("--limit")
            .arg("5")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .context("search while full indexing continues")?;
        let elapsed_ms = search_started.elapsed().as_secs_f64() * 1000.0;
        let indexing_running_at_result = indexer.try_wait()?.is_none();
        let index_exit = indexer.wait()?;
        let index_total_ms = index_started.elapsed().as_secs_f64() * 1000.0;
        if !output.status.success() {
            bail!(
                "search during partial index failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        let value: Value = serde_json::from_slice(&output.stdout)?;
        let result = value["results"]
            .as_array()
            .and_then(|results| results.first());
        let marked_partial = result
            .is_some_and(|result| result["index_complete"] == false && value["truncated"] == true);
        search_ms.push(elapsed_ms);
        details.push(json!({"sample":sample,"search_ms":elapsed_ms,"partial_catalog_observed_before_search":partial_observed,
        "index_process_running_when_search_returned":indexing_running_at_result,"useful_result_returned":result.is_some(),
        "result_explicitly_partial":marked_partial,"result_freshness":result.and_then(|value|value["freshness"].as_str()),
        "index_process_completed_successfully":index_exit.success(),"full_index_process_elapsed_ms":index_total_ms}));
    }
    Ok(
        json!({"sample_count":search_ms.len(),"timing":distribution(&search_ms),"samples":details,
        "insufficient_samples_reason":(search_ms.len()<count).then_some("some concurrent indexing attempts completed before a partial catalog could be queried")}),
    )
}

fn retrieval_quality(db_path: &Path, repo: &Path, queries: &[BenchmarkQuery]) -> Result<Value> {
    let mut per_query = Vec::new();
    let mut recall_sum = 0.0;
    let mut rr_sum = 0.0;
    for query in queries {
        let results = code::search_code(db_path, repo, &query.query, 5)?;
        let relevant = query
            .relevant_paths
            .iter()
            .collect::<std::collections::HashSet<_>>();
        let found = results
            .iter()
            .enumerate()
            .filter(|(_, r)| relevant.contains(&r.path))
            .collect::<Vec<_>>();
        let recall = if relevant.is_empty() {
            0.0
        } else {
            found.len() as f64 / relevant.len() as f64
        };
        let reciprocal = found
            .iter()
            .map(|(index, _)| 1.0 / (*index as f64 + 1.0))
            .fold(0.0, f64::max);
        recall_sum += recall;
        rr_sum += reciprocal;
        per_query.push(json!({"id":query.id,"kind":query.kind,"query":query.query,"relevant_paths":query.relevant_paths,
            "top5":results.iter().map(|r|json!({"path":r.path,"score":r.score,"freshness":r.freshness})).collect::<Vec<_>>(),
            "recall_at_5":recall,"reciprocal_rank":reciprocal}));
    }
    let denom = queries.len().max(1) as f64;
    let memory_cases = memory_quality_cases(db_path)?;
    Ok(
        json!({"query_count":queries.len(),"recall_at_5":recall_sum/denom,"mrr":rr_sum/denom,"queries":per_query,"memory_cases":memory_cases}),
    )
}

fn retrieval_suite_quality(
    db_path: &Path,
    repo: &Path,
    queries: &[BenchmarkQuery],
) -> Result<Value> {
    let mut rows = Vec::with_capacity(queries.len());
    let mut recall_scores = [Vec::new(), Vec::new(), Vec::new()];
    let mut reciprocal_scores = Vec::new();
    let mut expected_miss_rows = Vec::new();
    let mut negative_rows = Vec::new();
    for query in queries {
        let results = code::search_code(db_path, repo, &query.query, 10)?;
        let mut relevant_paths = query.relevant_paths.clone();
        relevant_paths.extend(query.relevant.iter().map(|truth| truth.path.clone()));
        relevant_paths.sort();
        relevant_paths.dedup();
        let expected_miss =
            query.expected == "expected-miss" || query.expected_behavior.contains("unsupported");
        let positive = !relevant_paths.is_empty() && !expected_miss;
        let result_paths = results
            .iter()
            .map(|result| result.path.clone())
            .collect::<Vec<_>>();
        let ranks = result_paths
            .iter()
            .enumerate()
            .filter_map(|(index, path)| relevant_paths.contains(path).then_some(index + 1))
            .collect::<Vec<_>>();
        let reciprocal = ranks.first().map_or(0.0, |rank| 1.0 / *rank as f64);
        let recalls = [1usize, 5, 10].map(|limit| {
            if relevant_paths.is_empty() {
                0.0
            } else {
                result_paths
                    .iter()
                    .take(limit)
                    .filter(|path| relevant_paths.contains(path))
                    .count() as f64
                    / relevant_paths.len() as f64
            }
        });
        if positive {
            for (index, score) in recalls.iter().enumerate() {
                recall_scores[index].push(*score);
            }
            reciprocal_scores.push(reciprocal);
        }
        let no_result = results.is_empty();
        if expected_miss {
            expected_miss_rows.push(
                json!({"id":query.id,"query":query.query,"returned_no_results":no_result,
                "returned_target_path_count":ranks.len(),"passed":no_result}),
            );
        } else if relevant_paths.is_empty() {
            negative_rows.push(
                json!({"id":query.id,"query":query.query,"returned_no_results":no_result,
                "false_positive":!no_result}),
            );
        }
        rows.push(json!({"id":query.id,"category":query.kind,"query":query.query,"expected":query.expected,
            "expected_behavior":query.expected_behavior,"ground_truth":query.relevant,"relevant_paths":relevant_paths,
            "top10":results.iter().map(|result|json!({"path":result.path,"score":result.score,"freshness":result.freshness})).collect::<Vec<_>>(),
            "recall_at_1":recalls[0],"recall_at_5":recalls[1],"recall_at_10":recalls[2],"reciprocal_rank":reciprocal,
            "first_relevant_rank":ranks.first()}));
    }
    let mean = |values: &[f64]| values.iter().sum::<f64>() / values.len().max(1) as f64;
    let metric_distribution = |values: &[f64]| {
        let mut sorted = values.to_vec();
        sorted.sort_by(f64::total_cmp);
        let percentile = |p: f64| {
            if sorted.is_empty() {
                0.0
            } else {
                sorted[(((sorted.len() - 1) as f64 * p).ceil() as usize).min(sorted.len() - 1)]
            }
        };
        json!({"p50":percentile(0.50),"p95":percentile(0.95),"p99":percentile(0.99),"n":sorted.len(),"samples_sorted":sorted})
    };
    Ok(
        json!({"query_count":queries.len(),"positive_query_count":reciprocal_scores.len(),
        "recall_at_1":mean(&recall_scores[0]),"recall_at_5":mean(&recall_scores[1]),
        "recall_at_10":mean(&recall_scores[2]),"mrr":mean(&reciprocal_scores),
        "query_level_distributions":{"recall_at_1":metric_distribution(&recall_scores[0]),
            "recall_at_5":metric_distribution(&recall_scores[1]),"recall_at_10":metric_distribution(&recall_scores[2]),
            "mrr":metric_distribution(&reciprocal_scores)},
        "expected_miss_count":expected_miss_rows.len(),"expected_miss_pass_count":expected_miss_rows.iter().filter(|row|row["passed"]==true).count(),
        "expected_misses":expected_miss_rows,"negative_query_count":negative_rows.len(),
        "negative_false_positive_count":negative_rows.iter().filter(|row|row["false_positive"]==true).count(),
        "negative_queries":negative_rows,"queries":rows}),
    )
}

fn memory_quality_cases(db_path: &Path) -> Result<Value> {
    let scope = "benchmark:memory";
    let source = memory::remember(
        db_path,
        scope,
        "source",
        "Raw policy source: retain the retry timeout rule through March.",
        Some("benchmark://policy"),
        None,
        None,
        Some("2025-01-02T00:00:00Z"),
        Some("2025-01-01T00:00:00Z"),
        Some("2025-02-01T00:00:00Z"),
    )?;
    let fact = memory::remember(
        db_path,
        scope,
        "derived",
        "Derived fact: retry timeout is forty milliseconds.",
        Some("benchmark://policy#fact"),
        Some(source.id),
        None,
        Some("2025-01-03T00:00:00Z"),
        Some("2025-01-01T00:00:00Z"),
        Some("2025-02-01T00:00:00Z"),
    )?;
    let active = memory::recall(
        db_path,
        scope,
        "retry timeout",
        Some("2025-01-15T00:00:00Z"),
        10,
    )?;
    let expired = memory::recall(
        db_path,
        scope,
        "retry timeout",
        Some("2025-03-01T00:00:00Z"),
        10,
    )?;
    let provenance_ok = fact.source_id == Some(source.id)
        && active
            .iter()
            .any(|r| r.memory.id == fact.id && r.memory.source_id == Some(source.id));
    let isolated = memory::recall(
        db_path,
        "user:someone-else",
        "retry timeout",
        Some("2025-01-15T00:00:00Z"),
        10,
    )?
    .is_empty();
    let source_forgotten = memory::forget(db_path, scope, source.id)?;
    let derived_forgotten = memory::forget(db_path, scope, fact.id)?;
    let forgotten_recall = memory::recall(
        db_path,
        scope,
        "retry timeout",
        Some("2025-01-15T00:00:00Z"),
        10,
    )?
    .is_empty();
    Ok(
        json!({"temporal_active":active.iter().any(|r|r.memory.id==fact.id),"temporal_expired":expired.iter().all(|r|r.memory.id!=fact.id),
        "provenance_link":provenance_ok,"scope_isolation":isolated,"forget_immediate":source_forgotten&&derived_forgotten&&forgotten_recall,
        "all_pass":provenance_ok&&isolated&&source_forgotten&&derived_forgotten&&forgotten_recall&&active.iter().any(|r|r.memory.id==fact.id)&&expired.iter().all(|r|r.memory.id!=fact.id)}),
    )
}

fn ripgrep_baseline(repo: &Path, queries: &[BenchmarkQuery]) -> Result<Value> {
    let mut timings = Vec::new();
    let mut details = Vec::new();
    for query in queries {
        let before = Instant::now();
        let output = Command::new("rg")
            .args([
                "--files-with-matches",
                "--fixed-strings",
                "--ignore-case",
                "--no-messages",
                "--hidden",
                "--glob",
                "!.git/**",
                "--glob",
                "!target/**",
                "--glob",
                "!node_modules/**",
            ])
            .arg(&query.query)
            .arg(repo)
            .output()
            .context("run local ripgrep baseline")?;
        let elapsed = before.elapsed().as_secs_f64() * 1000.0;
        if !output.status.success() && output.status.code() != Some(1) {
            bail!("ripgrep baseline failed with status {}", output.status);
        }
        let paths = String::from_utf8_lossy(&output.stdout).lines().count();
        timings.push(elapsed);
        details.push(
            json!({"id":query.id,"query":query.query,"elapsed_ms":elapsed,"matching_files":paths}),
        );
    }
    Ok(
        json!({"executable":command_version("rg",&["--version"]),"timing":distribution(&timings),"queries":details}),
    )
}

fn incremental_samples(db_path: &Path, repo: &Path, count: usize) -> Result<Vec<f64>> {
    let file = repo.join("src/09999.rs");
    let original = std::fs::read(&file)
        .with_context(|| format!("read incremental benchmark file {}", file.display()))?;
    let mut durations = Vec::with_capacity(count);
    let cancel = std::sync::atomic::AtomicBool::new(false);
    for sample in 0..count {
        let marker = format!("// midnight_incremental_marker_{sample:04}\n");
        let mut updated = original.clone();
        if let Some(pos) = updated.iter().position(|&byte| byte == b'\n') {
            updated.splice(pos + 1..pos + 1, marker.bytes());
        }
        std::fs::write(&file, &updated)?;
        let detected = Instant::now();
        let report = code::index_repository(db_path, repo, false, false, &cancel)?;
        let query = format!("midnight_incremental_marker_{sample:04}");
        let visible = code::search_code(db_path, repo, &query, 5)?
            .iter()
            .any(|r| r.path == "src/09999.rs" && r.freshness == "fresh");
        durations.push(detected.elapsed().as_secs_f64() * 1000.0);
        if !report.complete || !visible {
            bail!("single-file incremental update did not become fresh and query-visible");
        }
        std::fs::write(&file, &original)?;
        code::index_repository(db_path, repo, false, false, &cancel)?;
    }
    Ok(durations)
}

fn stress_cycles_run(base: &Path, cycles: usize) -> Result<Value> {
    let temp = TempDir::new_in(base)?;
    let repo = temp.path().join("stress-repo");
    std::fs::create_dir_all(repo.join("src"))?;
    let file = repo.join("src/stress.rs");
    let db_path = temp.path().join("stress.sqlite3");
    let mut rss_samples = Vec::new();
    let mut durations = Vec::new();
    let cancel = std::sync::atomic::AtomicBool::new(false);
    for cycle in 0..cycles {
        let text = format!("pub fn stress_cycle_{cycle}() {{}} // marker cycle_{cycle}\n");
        std::fs::write(&file, &text)?;
        let start = Instant::now();
        let report = code::index_repository(&db_path, &repo, false, false, &cancel)?;
        let found = code::search_code(&db_path, &repo, &format!("stress_cycle_{cycle}"), 5)?;
        let _closed = code::index_status(&db_path, &repo)?;
        durations.push(start.elapsed().as_secs_f64() * 1000.0);
        if !report.complete || found.is_empty() {
            bail!("stress cycle {cycle} failed index/search consistency")
        }
        if let Some(rss) = current_rss_bytes() {
            rss_samples.push(rss)
        }
    }
    let cancelled = std::sync::atomic::AtomicBool::new(true);
    let cancel_report = code::index_repository(&db_path, &repo, false, false, &cancelled)?;
    Ok(
        json!({"cycles":cycles,"cycle_ms":distribution(&durations),"rss_samples_bytes":rss_samples,
        "rss_first_bytes":rss_samples.first().copied(),"rss_last_bytes":rss_samples.last().copied(),
        "cancelled_index_remains_partial":cancel_report.cancelled&&!cancel_report.complete,
        "peak_process_rss_bytes":code::peak_rss_bytes()}),
    )
}

fn environment(repo: &Path, sqlite: &str) -> Value {
    let cpu = linux_cpu_model();
    let cores = std::thread::available_parallelism()
        .ok()
        .map(|n| n.get() as u32);
    let system_cores = std::fs::read_to_string("/proc/cpuinfo")
        .ok()
        .map(|contents| {
            contents
                .lines()
                .filter(|line| line.starts_with("processor\t"))
                .count() as u32
        });
    let memory = linux_memory_bytes();
    let memory_limit = std::fs::read_to_string("/sys/fs/cgroup/memory.max")
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok());
    let storage = command_stdout(
        "findmnt",
        &[
            "-n",
            "-o",
            "SOURCE,FSTYPE,TARGET",
            "--target",
            repo.to_string_lossy().as_ref(),
        ],
    );
    let disk_devices = command_stdout("lsblk", &["-dn", "-o", "NAME,ROTA,TYPE,TRAN,MODEL"]);
    json!({"os":std::env::consts::OS,"arch":std::env::consts::ARCH,"cpu_model":cpu,
        "logical_core_count_system":system_cores,"logical_core_count_available_to_process":cores,
        "ram_bytes":memory,"memory_limit_bytes":memory_limit,"disk_devices_and_media_fields":disk_devices,
        "filesystem_mount_source_type_target":storage,"kernel":command_stdout("uname", &["-r"]),
        "rustc":command_stdout("rustc",&["--version"]),"cargo_profile":if cfg!(debug_assertions){"debug"}else{"release"},
        "sqlite_version":sqlite,"rusqlite_crate":"0.41.1","tree_sitter_crate":"0.27.1",
        "tree_sitter_grammar_crates":{"rust":"0.24.2","python":"0.25.0","typescript":"0.23.2"},
        "ripgrep":command_version("rg",&["--version"]),"out_of_scope_runtimes":{"Go":"pending (not run)","Node.js":"pending (not run)","Bun":"pending (not run)"},
        "pending_platforms":{"macOS reference host":"pending (not run)","Windows":"pending (not run)","ARM":"pending (not run)"},
        "corpus_root":repo})
}

fn linux_cpu_model() -> Option<String> {
    std::fs::read_to_string("/proc/cpuinfo")
        .ok()?
        .lines()
        .find_map(|line| {
            line.strip_prefix("model name")
                .and_then(|value| value.split_once(':'))
                .map(|(_, value)| value.trim().to_string())
        })
}

fn linux_memory_bytes() -> Option<u64> {
    std::fs::read_to_string("/proc/meminfo")
        .ok()?
        .lines()
        .find_map(|line| {
            let value = line.strip_prefix("MemTotal:")?.split_whitespace().next()?;
            value.parse::<u64>().ok().map(|kilobytes| kilobytes * 1024)
        })
}

fn distribution(values: &[f64]) -> Value {
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    let percentile = |p: f64| -> f64 {
        if sorted.is_empty() {
            return 0.0;
        }
        let ix = ((sorted.len() - 1) as f64 * p).ceil() as usize;
        sorted[ix.min(sorted.len() - 1)]
    };
    json!({"p50_ms":percentile(0.50),"p95_ms":percentile(0.95),"p99_ms":percentile(0.99),"min_ms":sorted.first().copied(),"max_ms":sorted.last().copied(),"n":sorted.len(),"sample_count":sorted.len(),"samples_ms":sorted})
}

fn distribution_u64(values: &[u64]) -> Value {
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    let percentile = |p: f64| -> u64 {
        if sorted.is_empty() {
            return 0;
        }
        let ix = ((sorted.len() - 1) as f64 * p).ceil() as usize;
        sorted[ix.min(sorted.len() - 1)]
    };
    json!({"p50":percentile(0.50),"p95":percentile(0.95),"p99":percentile(0.99),"n":sorted.len(),"samples_sorted":sorted})
}

fn profile_retrieval_samples(samples: &[Value]) -> Value {
    if samples.is_empty() {
        return json!({"enabled":false});
    }
    let phase_names = [
        "total",
        "query_validation",
        "unicode_normalization",
        "stopword_filtering",
        "fts_query_construction",
        "sqlite_statement_prepare",
        "sqlite_parameter_bind",
        "sqlite_candidate_step",
        "candidate_record_hydration",
        "candidate_text_tokenization",
        "lexical_scoring",
        "final_sort",
        "response_serialization",
        "diagnostic_active_scope_count",
    ];
    let mut phases = serde_json::Map::new();
    for name in phase_names {
        let wall_ms = samples
            .iter()
            .filter_map(|sample| sample["phase_wall_ns"][name].as_u64())
            .map(|ns| ns as f64 / 1_000_000.0)
            .collect::<Vec<_>>();
        let cpu_ms = samples
            .iter()
            .filter_map(|sample| sample["phase_cpu_ns"][name].as_u64())
            .map(|ns| ns as f64 / 1_000_000.0)
            .collect::<Vec<_>>();
        phases.insert(
            name.to_string(),
            json!({
                "wall_ms":distribution(&wall_ms),
                "cpu_ms":distribution(&cpu_ms)
            }),
        );
    }
    let mut counters = serde_json::Map::new();
    for name in [
        "candidates_returned",
        "candidate_rows_deduplicated",
        "candidates_hydrated",
        "candidates_rejected_by_term_coverage",
        "content_bytes_hydrated",
        "record_bytes_hydrated",
        "sqlite_cache_hits",
        "sqlite_cache_misses",
        "sqlite_cache_writes",
        "sqlite_cache_spills",
        "sqlite_cache_size_kib",
        "sqlite_cache_used_bytes",
        "sqlite_vm_steps",
        "sqlite_fullscan_steps",
        "sqlite_sort_operations",
        "sqlite_statement_memory_bytes",
    ] {
        let values = samples
            .iter()
            .filter_map(|sample| sample[name].as_u64())
            .collect::<Vec<_>>();
        counters.insert(name.to_string(), distribution_u64(&values));
    }
    json!({
        "enabled":true,
        "sample_count":samples.len(),
        "candidate_strategy":samples[0]["candidate_strategy"],
        "candidate_limit":samples[0]["candidate_limit"],
        "active_scope_record_count":samples[0]["active_scope_record_count"],
        "output_fraction_of_scope":samples[0]["output_fraction_of_scope"],
        "candidate_budget_saturated":samples[0]["candidate_budget_saturated"],
        "temporal_filter_location":samples[0]["temporal_filter_location"],
        "temporal_rejections_observable":samples[0]["temporal_rejections_observable"],
        "sqlite_scanstatus_available":samples[0]["sqlite_scanstatus_available"],
        "sqlite_scanstatus_semantics":"NVISIT counts SQLite query-plan loop rows. It does not expose the number of internal FTS5 term postings examined.",
        "sqlite_scan_loops":samples[0]["sqlite_scan_loops"],
        "unavailable_measurements":samples[0]["unavailable_measurements"],
        "phases":phases,
        "counters":counters,
        "raw_samples":samples
    })
}

fn profile_page_cache_sizes(memory_session: &memory::MemorySession, scope: &str) -> Result<Value> {
    let cache_sizes_kib = [2048usize, 8192, 4096, 16_384, 16_384, 4096, 8192, 2048];
    let mut groups = Vec::with_capacity(cache_sizes_kib.len());
    let mut expected_ids: Option<Vec<i64>> = None;
    for (run_index, cache_size_kib) in cache_sizes_kib.into_iter().enumerate() {
        memory_session.shrink_page_cache()?;
        memory_session.set_page_cache_size_kib(cache_size_kib)?;
        for _ in 0..10 {
            let warm = memory_session.recall(scope, "namespace", None, 10)?;
            if warm.is_empty() {
                bail!("page-cache screen returned no broad recall results");
            }
        }
        let mut latency_ms = Vec::with_capacity(100);
        let mut observed_ids = None;
        for _ in 0..100 {
            let started = Instant::now();
            let results = memory_session.recall(scope, "namespace", None, 10)?;
            if results.is_empty() {
                bail!("page-cache screen returned no broad recall results");
            }
            latency_ms.push(started.elapsed().as_secs_f64() * 1000.0);
            observed_ids = Some(
                results
                    .iter()
                    .map(|result| result.memory.id)
                    .collect::<Vec<_>>(),
            );
        }
        let observed_ids = observed_ids.unwrap_or_default();
        let result_ids_stable = expected_ids
            .as_ref()
            .is_none_or(|expected| expected == &observed_ids);
        if !result_ids_stable {
            bail!("page-cache configuration changed deterministic recall results");
        }
        expected_ids.get_or_insert_with(|| observed_ids.clone());
        groups.push(json!({
            "run_index":run_index+1,
            "round":if run_index<4 {1}else{2},
            "cache_size_kib":cache_size_kib,
            "warmup_queries":10,
            "warm_recall":distribution(&latency_ms),
            "sqlite_cache_used_bytes_after_run":memory_session.page_cache_used_bytes(),
            "process_rss_bytes_after_run":current_rss_bytes(),
            "result_ids_stable":result_ids_stable
        }));
    }
    memory_session.shrink_page_cache()?;
    memory_session.set_page_cache_size_kib(2048)?;
    Ok(json!({
        "workload":"broad single-term same-namespace recall",
        "sample_count_per_group":100,
        "query_limit":10,
        "run_order":"2048,8192,4096,16384 KiB, then the reverse group order",
        "warm_state":"10 warm-up recalls after PRAGMA shrink_memory; the host OS page cache was not flushed",
        "database_result_ids_stable":true,
        "default_cache_restored_kib":2048,
        "groups":groups
    }))
}

fn distribution_f64(values: &[f64]) -> Value {
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    let percentile = |p: f64| -> f64 {
        if sorted.is_empty() {
            return 0.0;
        }
        let ix = ((sorted.len() - 1) as f64 * p).ceil() as usize;
        sorted[ix.min(sorted.len() - 1)]
    };
    json!({"p50":percentile(0.50),"p95":percentile(0.95),"p99":percentile(0.99),"n":sorted.len(),"samples_sorted":sorted})
}

fn command_stdout(command: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(command).args(args).output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}
fn command_version(command: &str, args: &[&str]) -> Option<String> {
    command_stdout(command, args)
}
fn current_rss_bytes() -> Option<u64> {
    let out = Command::new("ps")
        .args(["-o", "rss=", "-p", &std::process::id().to_string()])
        .output()
        .ok()?;
    let kb = String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse::<u64>()
        .ok()?;
    Some(kb.saturating_mul(1024))
}

/// Paired staged-vs-v0.3.1 recall measurement on a deterministic Zipf corpus.
///
/// Both strategies run in the same process against the same database, so differences are not
/// confounded by hardware or build. Output contains workload definitions and aggregate
/// measurements only (no host, user, or path identifiers).
pub fn run_retrieval_ab(size: usize, namespace_count: usize, samples: usize) -> Result<Value> {
    if !(1_000..=100_000).contains(&size) || !matches!(namespace_count, 1 | 4) || samples == 0 {
        bail!("retrieval A/B requires 1000..=100000 records, 1 or 4 namespaces, and samples > 0");
    }
    const VOCABULARY: usize = 3_000;
    let scopes = ["bench:alpha", "bench:beta", "bench:gamma", "bench:delta"];
    let scopes = &scopes[..namespace_count];
    let mut cumulative = Vec::with_capacity(VOCABULARY);
    let mut total = 0.0;
    for rank in 0..VOCABULARY {
        total += 1.0 / (rank as f64 + 1.0);
        cumulative.push(total);
    }
    let mut state = 0x9E37_79B9_7F4A_7C15u64;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    let temp = TempDir::new().context("create isolated retrieval A/B directory")?;
    let db_path = temp.path().join("retrieval-ab.sqlite3");
    let mut session = memory::MemorySession::open(&db_path)?;
    let mut batch = Vec::with_capacity(memory::MAX_MEMORY_BATCH_RECORDS);
    let mut planted = String::new();
    for index in 0..size {
        let length = 12 + (next() % 29) as usize;
        let words = (0..length)
            .map(|_| {
                let target = (next() >> 11) as f64 / (1u64 << 53) as f64 * total;
                let word = cumulative.partition_point(|value| *value < target);
                format!("w{}", word.min(VOCABULARY - 1))
            })
            .collect::<Vec<_>>();
        if index == size / 2 {
            planted = words[2..5.min(words.len())].join(" ");
        }
        batch.push(memory::MemoryInput {
            scope: scopes[index % scopes.len()].to_string(),
            kind: "source".into(),
            content: words.join(" "),
            source_uri: Some("benchmark://zipf".into()),
            source_id: None,
            supersedes_id: None,
            observed_at: None,
            valid_from: None,
            valid_to: None,
        });
        if batch.len() == memory::MAX_MEMORY_BATCH_RECORDS || index + 1 == size {
            session.remember_batch(&batch)?;
            batch.clear();
        }
    }
    code::wal_checkpoint(&db_path)?;
    let scope = scopes[(size / 2) % scopes.len()];
    let natural = format!("what is the {planted} and w2000");
    let classes: Vec<(&str, String)> = vec![
        ("broad_ubiquitous_single_term", "w0".into()),
        ("broad_mid_single_term", "w10".into()),
        ("selective_single_term", "w2000".into()),
        ("two_heavy_terms", "w3 w5".into()),
        ("heavy_plus_rare", "w10 w2000".into()),
        ("three_mixed_terms", "w50 w300 w2000".into()),
        ("planted_contiguous_phrase", planted.clone()),
        ("natural_language_with_stop_words", natural),
    ];
    let mut reports = Vec::new();
    for (class, query) in classes {
        let mut strategies = serde_json::Map::new();
        let mut top_ids: Vec<(String, Vec<i64>, String)> = Vec::new();
        let run = |session: &memory::MemorySession, label: &str| match label {
            "staged_recent_opt_in" => {
                session.recall_with_policy(scope, &query, None, 10, memory::BroadTermPolicy::Recent)
            }
            "staged" => session.recall_ordered(
                scope,
                &query,
                None,
                10,
                memory::RecallCandidateOrder::Staged,
            ),
            _ => session.recall_ordered(
                scope,
                &query,
                None,
                10,
                memory::RecallCandidateOrder::Ranked,
            ),
        };
        for label in ["staged", "v0_3_1_bm25_window", "staged_recent_opt_in"] {
            for _ in 0..3 {
                run(&session, label)?;
            }
            let mut wall_ms = Vec::with_capacity(samples);
            let cpu_started = code::process_cpu_seconds();
            let mut last = Vec::new();
            for _ in 0..samples {
                let started = Instant::now();
                last = run(&session, label)?;
                wall_ms.push(started.elapsed().as_secs_f64() * 1000.0);
            }
            let cpu_ms = code::process_cpu_seconds()
                .zip(cpu_started)
                .map(|(end, start)| (end - start) * 1000.0 / samples as f64);
            let coverage = last
                .first()
                .map(|result| format!("{:?}", result.candidate_coverage))
                .unwrap_or_else(|| "NoResults".into());
            let mut entry = distribution(&wall_ms);
            if let Some(object) = entry.as_object_mut() {
                object.remove("samples_ms");
                object.insert("cpu_ms_per_query".into(), cpu_ms.into());
                object.insert("coverage".into(), coverage.clone().into());
                object.insert("result_count".into(), last.len().into());
            }
            strategies.insert(label.into(), entry);
            top_ids.push((
                label.into(),
                last.iter().map(|result| result.memory.id).collect(),
                coverage,
            ));
        }
        // Staged vs v0.3.1 only: the opt-in recent policy is a deliberately different contract.
        let identical_top_10 = top_ids[0].1 == top_ids[1].1;
        let shared = top_ids[0]
            .1
            .iter()
            .filter(|id| top_ids[1].1.contains(id))
            .count();
        reports.push(json!({
            "query_class": class,
            "query": query,
            "strategies": strategies,
            "top_10_identical_between_strategies": identical_top_10,
            "top_10_overlap": shared,
        }));
    }
    Ok(json!({
        "workload": {
            "records": size,
            "namespaces": namespace_count,
            "queried_namespace_records": size / namespace_count,
            "vocabulary": VOCABULARY,
            "distribution": "zipf s=1",
            "samples_per_strategy": samples,
            "result_limit": 10,
            "cache": "warm persistent connection, default 2 MiB SQLite page cache",
        },
        "platform": {"os": std::env::consts::OS, "arch": std::env::consts::ARCH, "sqlite": rusqlite::version()},
        "database_bytes": code::database_bytes(&db_path),
        "peak_rss_bytes": current_rss_bytes(),
        "classes": reports,
    }))
}
