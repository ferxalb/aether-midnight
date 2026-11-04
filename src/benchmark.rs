use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::Path;
use std::process::{Command, Stdio};
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
    let primary_db = base.path().join("midnight.sqlite3");
    let run_count = index_samples.max(1);
    let mut full_index_ms = Vec::with_capacity(run_count);
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
        index_reports.push(report);
        code::wal_checkpoint(&db_path)?;
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
    let stress = stress_cycles_run(base.path(), stress_cycles.max(1))?;

    let source_bytes = primary_status.eligible_bytes.max(1);
    let index_bytes = code::database_bytes(&primary_db);
    let sqlite = code::sqlite_version(&primary_db)?;
    let output = json!({
        "schema_version":1,
        "measurement_conditions":{
            "scope":"locally generated deterministic reference fixture",
            "network":"not used",
            "database_cache":"fresh database for full index; warm database for search; fresh process with empty index for first useful context",
            "filesystem_cache":"not evicted; first cold-search sample is OS-cache-unknown and later samples may be warm",
            "timers":"process launch through grounded JSON result for first useful context; operation start through query-visible result for warm and incremental calls",
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
            "warm_search":distribution(&search_ms),
            "incremental_update_query_visible":distribution(&incremental),
            "ripgrep_identical_query_set":rg
        },
        "retrieval_quality":quality,
        "resources":{
            "peak_process_rss_bytes":code::peak_rss_bytes(),
            "rss_after_stress_cycles_bytes":stress.get("rss_samples_bytes").cloned().unwrap_or(json!([])),
            "stress":stress,
            "allocator_live_heap_bytes":Value::Null,
            "memory_mapped_pages_bytes":Value::Null,
            "unavailable_measurements":"allocator live heap and mmap pages are not instrumented on this build"
        },
        "index_status":primary_status,
        "database_file_bytes":index_bytes
    });
    Ok(output)
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
        .or_else(|| command_stdout("sysctl", &["-n", "hw.model"]));
    let cores = command_stdout("sysctl", &["-n", "hw.logicalcpu"])
        .and_then(|s| s.trim().parse::<u32>().ok())
        .or_else(|| {
            std::thread::available_parallelism()
                .ok()
                .map(|n| n.get() as u32)
        });
    let memory =
        command_stdout("sysctl", &["-n", "hw.memsize"]).and_then(|s| s.trim().parse::<u64>().ok());
    let storage = command_stdout("df", &["-h", repo.to_string_lossy().as_ref()])
        .and_then(|s| s.lines().last().map(str::to_string));
    json!({"os":std::env::consts::OS,"arch":std::env::consts::ARCH,"cpu":cpu,"logical_cores":cores,"ram_bytes":memory,
        "storage_mount":storage,"rustc":command_stdout("rustc",&["--version"]),"cargo_profile":if cfg!(debug_assertions){"debug"}else{"release"},
        "sqlite_version":sqlite,"tree_sitter_crate":"0.27.1","ripgrep":command_version("rg",&["--version"]),"node":command_version("node",&["--version"]),
        "bun":command_version("bun",&["--version"]),"go":command_version("go",&["version"]),"corpus_root":repo})
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
