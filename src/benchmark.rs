use anyhow::{Context, Result, bail};
use rusqlite::{Connection, params};
use serde::Deserialize;
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
    kind: String,
    query: String,
    relevant_paths: Vec<String>,
}

pub fn run(
    _requested_db: &Path,
    repo: &Path,
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
    let empty_db = base.path().join("empty.sqlite3");
    let binary = std::env::current_exe().context("locate benchmark executable")?;
    let mut first_useful_ms = Vec::with_capacity(samples.max(1));
    for _ in 0..samples.max(1) {
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
    let rg = ripgrep_baseline(&repo, &manifest.queries)?;
    let incremental = incremental_samples(&primary_db, &repo, samples.clamp(1, 10))?;
    let partial_context = partial_context_during_index(base.path(), &repo, cold_query)?;
    let stress = stress_cycles_run(base.path(), stress_cycles.max(1))?;

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
            "index_bytes":index_bytes,"index_to_source_ratio":index_bytes as f64/source_bytes as f64
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
        "resources":{
            "peak_process_rss_bytes":code::peak_rss_bytes(),
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

fn partial_context_during_index(base: &Path, repo: &Path, query: &str) -> Result<Value> {
    let temp = TempDir::new_in(base)?;
    let db_path = temp.path().join("partial.sqlite3");
    let binary = std::env::current_exe().context("locate benchmark executable")?;
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
    let search_ms = search_started.elapsed().as_secs_f64() * 1000.0;
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
    Ok(json!({
        "search_ms":search_ms,
        "sample_count":1,
        "partial_catalog_observed_before_search":partial_observed,
        "index_process_running_when_search_returned":indexing_running_at_result,
        "useful_result_returned":result.is_some(),
        "result_explicitly_partial":marked_partial,
        "result_freshness":result.and_then(|value|value["freshness"].as_str()),
        "index_process_completed_successfully":index_exit.success(),
        "full_index_process_elapsed_ms":index_total_ms
    }))
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
    let cpu = command_stdout("sysctl", &["-n", "machdep.cpu.brand_string"])
        .or_else(|| command_stdout("sysctl", &["-n", "hw.model"]))
        .or_else(linux_cpu_model);
    let cores = command_stdout("sysctl", &["-n", "hw.logicalcpu"])
        .and_then(|s| s.trim().parse::<u32>().ok())
        .or_else(|| {
            std::thread::available_parallelism()
                .ok()
                .map(|n| n.get() as u32)
        });
    let memory = command_stdout("sysctl", &["-n", "hw.memsize"])
        .and_then(|s| s.trim().parse::<u64>().ok())
        .or_else(linux_memory_bytes);
    let memory_limit = std::fs::read_to_string("/sys/fs/cgroup/memory.max")
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok());
    let storage = command_stdout("df", &["-h", repo.to_string_lossy().as_ref()])
        .and_then(|s| s.lines().last().map(str::to_string));
    json!({"os":std::env::consts::OS,"arch":std::env::consts::ARCH,"cpu":cpu,"logical_cores_available_to_process":cores,"ram_bytes":memory,"memory_limit_bytes":memory_limit,
        "storage_mount":storage,"rustc":command_stdout("rustc",&["--version"]),"cargo_profile":if cfg!(debug_assertions){"debug"}else{"release"},
        "sqlite_version":sqlite,"rusqlite_crate":"0.41.1","tree_sitter_crate":"0.27.1",
        "tree_sitter_grammar_crates":{"rust":"0.24.2","python":"0.25.0","typescript":"0.23.2"},
        "ripgrep":command_version("rg",&["--version"]),"node":command_version("node",&["--version"]),
        "bun":command_version("bun",&["--version"]),"go":command_version("go",&["version"]),"corpus_root":repo})
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
    json!({"p50_ms":percentile(0.50),"p95_ms":percentile(0.95),"min_ms":sorted.first().copied(),"max_ms":sorted.last().copied(),"sample_count":sorted.len(),"samples_ms":sorted})
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
