# Performance contract and benchmark results

This page describes the latest Linux-only measurement run. Raw sorted samples and host metadata are in the JSON files linked below. The committed benchmark targets and gates are unchanged.

## Host and method

- OS/kernel: Linux x86_64, kernel `6.18.57`.
- CPU: AMD EPYC 9V74 80-Core Processor; 3 logical CPUs visible to the container.
- RAM: `10,452,459,520` bytes reported by `/proc/meminfo`.
- Storage: overlay filesystem mounted at `/`; `lsblk` reports `vda` as a virtual virtio block device with ROTA=1. The physical backing medium cannot be identified from this VM.
- Rust: `rustc 1.99.1 (58cf8f9c3 2026-10-13)`; bundled SQLite `3.54.1`; `rusqlite 0.41.1`.

No OS page-cache drop was attempted. A fresh SQLite database is “cold” only at the database layer: later index samples may benefit from warm OS file cache. Search comparisons use a completed, warm database and leave OS caches alone. CLI and `rg` timings include process start. MCP timings include process start, initialize, and one `tools/call` round trip.

Database byte measurements sum the SQLite main file and any present WAL/SHM files. Database-to-source ratios divide that sum by eligible source bytes.

Percentiles use the benchmark’s sorted nearest-index rule: `ceil((n - 1) × p)`, capped at `n - 1`. Each raw result file stores sorted sample arrays alongside p50, p95, p99, and n. `benchmark/results.json` is the deterministic synthetic fixture run; the real-repository evidence is split between the index/search file and the incremental visibility file.

## Reproduce

From the repository root, using the recorded Linux host and Rust toolchain:

```sh
source "$HOME/.cargo/env"
cargo build --release --locked
MIDNIGHT_BENCH_SAMPLES=100 MIDNIGHT_INDEX_SAMPLES=30 MIDNIGHT_STRESS_CYCLES=30 python3 scripts/run_benchmark.py > benchmark/results.json
```

The real repositories were shallow-cloned from these URLs; their checked-out commit SHAs are recorded in the raw file:

```sh
git clone --depth 1 https://github.com/torvalds/linux.git /tmp/aether-repos/linux
git clone --depth 1 https://github.com/microsoft/vscode.git /tmp/aether-repos/vscode
MIDNIGHT_INDEX_SAMPLES=30 MIDNIGHT_QUERY_SAMPLES=100 python3 scripts/run_repository_benchmarks.py --repo linux=/tmp/aether-repos/linux --repo vscode=/tmp/aether-repos/vscode > benchmark/real-repositories.json
MIDNIGHT_INCREMENTAL_SAMPLES=30 python3 scripts/run_incremental_repositories.py --samples 30 --repo linux=/tmp/aether-repos/linux@init/main.c --repo vscode=/tmp/aether-repos/vscode@src/vs/code/electron-main/app.ts > benchmark/incremental-repositories.json
```

Network access was available and used to fetch Rust dependencies and the two repositories. The measurement commands do not contact a service. No test suite or clippy run is part of this benchmark task.

## Synthetic fixture and existing gates

The existing synthetic fixture remains a separate result row: seed `20261103`, 10,000 eligible Rust files, 104,857,600 source bytes, no skipped files. Database bytes after indexing were p50/p95/p99 18,767,872 / 18,767,872 / 18,767,872 (n=30); database-to-source ratio was 0.178984 / 0.178984 / 0.178984 (n=30). The final database size is also recorded as a separate n=1 anecdotal observation in `benchmark/results.json`.

| Existing gate | Target | Linux result | n | Status |
| --- | --- | ---: | ---: | --- |
| First useful grounded context, fresh process and empty DB | p50 ≤ 1,000 ms; p95 ≤ 3,000 ms | p50 54.016 / p95 63.857 / p99 69.514 ms | 100 | Pass |
| Complete lexical and structural index, 10,000 files / 100 MiB | p95 ≤ 3,000 ms | p50 2,657.774 / p95 2,847.152 / p99 2,857.061 ms | 30 | Pass |
| Useful partial result while indexing continues | ≤ 3,000 ms | p50 61.319 / p95 69.433 / p99 72.126 ms | 100 | Pass |
| Warm search on complete index | p95 ≤ 50 ms | p50 10.681 / p95 12.908 / p99 21.378 ms | 100 | Pass |
| One-file update, query-visible | p95 ≤ 1,000 ms | p50 180.760 / p95 208.836 / p99 278.215 ms | 100 | Pass |

All existing synthetic fixture gates pass in the retained final run; no target was changed. All 100 partial-context samples returned a useful explicitly partial result while the index process was active. Per-sample flags and sorted timings are in the raw JSON. A first execution during this task had full-index p95 3,310.7 ms (n=30), above target; its result file was overwritten by the final rerun and its sorted sample array is no longer available. Treat that prior summary as unmeasured due to missing raw evidence; the final retained n=30 series is the reportable result, and the run-to-run difference warrants reproduction before relying on the pass.

The current `benchmark/manifest.json` contains **5** dev queries, not the 51 described in the task. Those five definitions were frozen unchanged in [`benchmark/suites/dev.json`](../benchmark/suites/dev.json). The new [`benchmark/suites/heldout.json`](../benchmark/suites/heldout.json) contains 64 held-out queries and was written before any benchmark run: 50 exact-symbol queries, 4 natural-language paraphrases, 4 multi-term queries, 1 snake_case boundary query, 1 camelCase boundary query, 2 expected-miss in-token fragments, and 2 ambiguous negatives.

| Retrieval suite | Positive queries | Recall@1 | Recall@5 | Recall@10 | MRR | Additional checks | Status |
| --- | ---: | ---: | ---: | ---: | ---: | --- | --- |
| Dev, synthetic fixture | 5 | 0.900 | 1.000 | 1.000 | 1.000 | Five fixed cases | Anecdotal |
| Held-out, clean repository archive | 60 of 64 | 0.833 | 0.833 | 0.850 | 0.835 | 2 expected-miss queries had no hits; 2 ambiguous negatives had no hits | Pass |

Held-out retrieval used a clean archive of the repository commit, excluding the suite and benchmark output files from the search corpus. The query set includes natural-language paraphrases, multi-term queries, snake/camel boundary cases, expected misses for unsupported in-token fragments, and ambiguous negatives. The held-out results did not change retrieval code, tokenization, or ranking; `contaminated` is `false` in the raw result.

Raw per-query ranks are included in the result JSON, with sorted per-query Recall@1/5/10 and reciprocal-rank distributions (n=5 dev positives and n=60 held-out positives).

No minimum retrieval-score threshold was specified. The held-out row’s Pass status means the n=64 suite and its expected-miss/negative checks completed; the displayed scores are descriptive quality evidence.

## Real repositories

Both repositories exceed 10,000 files and include URL and commit SHA in [`benchmark/real-repositories.json`](../benchmark/real-repositories.json). Index samples use a fresh SQLite database each time. The host page cache was not flushed.

| Repository | URL and commit SHA | Eligible files / source bytes (constant across n=30 index samples) | Fresh-DB full index p50 / p95 / p99 (ms) | DB bytes p50 / p95 / p99; ratio p50 / p95 / p99 | Sampled peak RSS p50 / p95 / p99 (bytes) | n | Catalog / structural status |
| --- | --- | ---: | ---: | ---: | ---: | ---: | --- |
| Linux kernel | `https://github.com/torvalds/linux.git` · `05441d697f7f9a927264f4058cbbc6a03dfcac9d` | 96,783 / 1,191,484,237 | 51,426 / 58,873 / 59,691 | 382,930,944 / 382,930,944 / 382,930,944; 0.321 / 0.321 / 0.321 | 54,390,784 / 56,479,744 / 56,483,840 | 30 per distribution | Catalog complete; structural incomplete |
| VS Code | `https://github.com/microsoft/vscode.git` · `e80d09a84b2bb74d3e7b8fe3545db99882e15419` | 19,900 / 257,862,301 | 45,215 / 49,209 / 49,255 | 138,063,872 / 138,063,872 / 138,063,872; 0.535 / 0.535 / 0.535 | 86,712,320 / 86,781,952 / 86,835,200 | 30 per distribution | Catalog complete; structural incomplete |

Both repositories had complete file catalogs but incomplete structural indexes in all 30 index samples, so structural completion is a **Fail**. The last index status reported 7 Tree-sitter parse errors for Linux and 54 for VS Code (each a final-status observation, n=1 anecdotal). The skip reasons and each sample’s completeness flags are in the JSON. RSS is a sampled high-water mark: the parent polls child `VmRSS` in `/proc/<pid>/status` every 5 ms during indexing.

The same eight queries (`TODO`, `error`, `test`, `parse`, `config`, `cache`, `main`, `timeout`) were used on each repository for Midnight CLI, MCP, and `rg`. Every query/method pair has n=100 raw wall-clock samples, return bytes, approximate tokens, and result counts. Aggregated across the eight queries (n=800 per method):

| Repository | Method | Wall p50 / p95 / p99 (ms) | Payload bytes p50 / p95 / p99 | Approx. tokens p50 / p95 / p99 | n per distribution |
| --- | --- | ---: | ---: | ---: | ---: |
| Linux kernel | CLI | 83.963 / 108.822 / 126.018 | 4,686 / 5,123 / 5,123 | 1,172 / 1,281 / 1,281 | 800 |
| Linux kernel | MCP round trip | 82.065 / 107.027 / 120.805 | 3,906 / 4,343 / 4,343 | 977 / 1,086 / 1,086 | 800 |
| Linux kernel | `rg` | 663.187 / 816.823 / 870.958 | 1,271,005 / 2,412,268 / 2,412,268 | 317,752 / 603,067 / 603,067 | 800 |
| VS Code | CLI | 23.142 / 107.239 / 122.425 | 5,065 / 5,687 / 5,687 | 1,267 / 1,422 / 1,422 | 800 |
| VS Code | MCP round trip | 22.901 / 108.691 / 119.322 | 4,285 / 4,907 / 4,907 | 1,072 / 1,227 / 1,227 | 800 |
| VS Code | `rg` | 177.738 / 215.699 / 228.594 | 434,413 / 743,163 / 743,163 | 108,604 / 185,791 / 185,791 | 800 |

Approximate tokens are `ceil(returned UTF-8 bytes / 4)`, a size heuristic rather than a model tokenizer. CLI and MCP return JSON with snippets and metadata; `rg` returns matching paths only, so output size semantics differ. Per-query distributions are preserved in the raw JSON.

Incremental visibility was measured separately in [`benchmark/incremental-repositories.json`](../benchmark/incremental-repositories.json). Each sample starts after writing one source change and ends after the index command plus a fresh search confirms a fresh, query-visible result. The temporary edit was restored and Git-clean status was checked.

All 30 update samples per repository restored query visibility and completed the catalog, while structural indexing remained incomplete. The visibility timings therefore measure catalog and lexical update visibility with the parser errors described above.

| Repository | Updated file | Visibility p50 / p95 / p99 (ms) | n | DB bytes before / after (n=1 snapshots, anecdotal); growth p50 / p95 / p99 (bytes, n=30) |
| --- | --- | ---: | ---: | ---: |
| Linux kernel | `init/main.c` | 1,917.909 / 2,184.154 / 2,239.533 | 30 | 382,930,944 / 384,245,760; 1,269,760 / 1,310,720 / 1,314,816 |
| VS Code | `src/vs/code/electron-main/app.ts` | 606.148 / 658.168 / 704.286 | 30 | 138,063,872 / 138,063,872; 0 / 0 / 0 |

## Memory benchmark

The memory workload stores source and derived memories across four namespaces. Store, superseding update, delete, and recall each have sorted raw samples. Store/update/delete use n=30 per size; recall latency uses n=100 per size. The labeled retrieval set contains 50 queries, reported overall and separately for source and derived records. Each dataset also runs 150 cross-namespace checks; a leak aborts the benchmark.

| Memories | Store p50 / p95 / p99 ms (n=30) | Update p50 / p95 / p99 ms (n=30) | Delete p50 / p95 / p99 ms (n=30) | Recall p50 / p95 / p99 ms (n=100) | DB growth during measured operations | Recall@1/5/10; MRR | Namespace leakage |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: | --- |
| 1,000 | 2.162 / 3.010 / 3.395 | 1.886 / 2.448 / 2.471 | 1.866 / 2.436 / 2.889 | 0.945 / 1.429 / 2.525 | +20,480 bytes (one before/after snapshot, n=1); store-only growth 4,096 / 20,480 / 20,480 (n=30) | 1.00 / 1.00 / 1.00; 1.00 (50 queries) | 0 / 150 |
| 10,000 | 2.013 / 2.736 / 2.848 | 2.228 / 4.216 / 5.308 | 2.020 / 4.175 / 6.202 | 4.244 / 5.911 / 11.693 | +32,768 bytes (n=1); store-only growth 4,096 / 16,384 / 16,384 (n=30) | 1.00 / 1.00 / 1.00; 1.00 (50 queries) | 0 / 150 |
| 100,000 | 2.060 / 4.051 / 6.443 | 1.943 / 2.233 / 2.331 | 1.986 / 2.423 / 2.950 | 36.963 / 45.142 / 75.915 | 0 bytes (n=1); store-only growth 0 / 0 / 0 (n=30) | 1.00 / 1.00 / 1.00; 1.00 (50 queries) | 0 / 150 |

Source and derived recall each had n=25 per size and Recall@1/5/10 and MRR of 1.00. The namespace check ran 150 cross-namespace queries at each size and found zero leakage; any leak aborts the benchmark. The table’s store/update/delete n is 30 and recall n is 100. The database growth over the full operation batch is a single before/after snapshot (n=1, anecdotal); store-only growth has a raw 30-sample distribution. Current RSS during those 30 store samples had p50/p95/p99 33,210,368 / 33,210,368 / 33,210,368 bytes at 1k and 10k, and 33,234,944 / 33,234,944 / 33,234,944 bytes at 100k. Process peak RSS is **unmeasured**: the `getrusage` high-water counter was below later sampled current RSS, so it is retained only as an inconsistent raw counter in the JSON. Current RSS after each workload is one anecdotal observation. Allocator live heap and mmap-page sizes are unmeasured because this build does not instrument them.

Per-query memory Recall@1/5/10 and reciprocal-rank distributions, including source and derived splits, are also stored with sorted raw values and n in the result JSON. Setup time was measured once per size and is marked anecdotal.

## FTS5 tokenizer comparison and lifecycle stress

The fixed fixture also compared contentless FTS5 trigram and `unicode61 remove_diacritics 2` using Python SQLite 3.53.1; the release build uses bundled SQLite 3.54.1. Build and final index-size values are single observations (n=1, anecdotal); each tokenizer’s four-query retrieval score is also anecdotal (n=4). Warm search used 100 samples per content query (n=400 aggregate), and exact path lookup used n=100.

| Tokenizer | Build ms (n=1) | Index bytes / source ratio (n=1) | Warm search p50 / p95 / p99 ms (n=400) | Recall@5 / MRR (n=4) |
| --- | ---: | ---: | ---: | ---: |
| FTS5 trigram | 3,909.651 | 112,640,000 / 1.074× | 0.322 / 0.514 / 0.903 | 1.00 / 1.00 |
| FTS5 `unicode61 remove_diacritics 2` | 810.087 | 15,368,192 / 0.147× | 0.055 / 0.138 / 0.210 | 1.00 / 1.00 |

The separate exact path lookup measured p50/p95/p99 0.001051 / 0.002303 / 0.008172 ms (n=100). Sorted per-query recall and reciprocal-rank values are in the result JSON; quality sample n=4 is anecdotal. These tokenizer scores apply only to the fixed four-query fixture. Midnight uses `unicode61`; it supports short whole-token matches and does not provide arbitrary in-token substring search. Tantivy and Zoekt were not evaluated in this Linux run.

The 30 open/index/update/search/status/close stress cycles measured p50/p95/p99 4.236 / 6.158 / 6.240 ms (n=30). Current RSS after each cycle was 33,431,552 bytes for all 30 samples. This does not establish zero leaks. The separate process-wide peak RSS counter was below the sampled current RSS and is treated as unmeasured. One cancellation observation was partial (n=1, anecdotal). Allocator live-heap and mmap-page measurements remain unmeasured.

## Pending verification

These items are out of scope for this Linux run and are **pending (not run)**:

- macOS reference host: pending (not run)
- Go: pending (not run)
- Node.js: pending (not run)
- Bun: pending (not run)
- Windows: pending (not run)
- ARM: pending (not run)

The first synthetic run used the inherited environment collector, which invoked `node --version`, `go version`, and attempted `bun --version`. No Go, Node.js, or Bun example/build was run. Those version probes have been removed from the current collector; runtime verification remains pending.

## Historical summary retained from the previous report

The previous documentation reported these macOS summary values. They are retained here as historical evidence; the original sample arrays and `benchmark/results.json` were not present in this checkout, so p99/raw samples cannot be reconstructed. The macOS reference host was not run in the current Linux task and is pending above.

| Historical metric | Previously reported result |
| --- | --- |
| First useful context | p50 156.7 / p95 167.2 ms, n=30 |
| Full index, 10,000 files / 100 MiB | p50 4,722.1 / p95 7,142.1 ms, n=3; gate miss |
| Partial context during indexing | 150.6 ms, n=1; anecdotal |
| Warm search | p50 15.4 / p95 16.4 ms, n=30 |
| Incremental update visibility | p50 315.5 / p95 424.5 ms, n=10 |

The 3-sample full-index series and single partial-index sample are preserved as previously reported, not promoted to current-run passes. Current targets remain unchanged.

The previous macOS report also summarized FTS5 and lifecycle memory results. They are retained as historical summaries because the raw result file and sample arrays were absent from this checkout. The current Linux measurements above use the same tokenizer definitions but a different host and Python SQLite version.

| Historical metric | Previously reported result |
| --- | --- |
| Trigram tokenizer build / index size | 6,337 ms / 112,640,000 bytes (single build) |
| `unicode61` tokenizer build / index size | 1,504 ms / 15,368,192 bytes (single build) |
| Trigram warm FTS search | p50 2.09 / p95 2.90 ms, n=120 |
| `unicode61` warm FTS search | p50 0.38 / p95 0.62 ms, n=120 |
| Exact path lookup | p50 0.0025 / p95 0.0037 ms, n=30 |
| Benchmark-process peak RSS | 28,917,760 bytes (single observation) |
| 20 lifecycle cycles | RSS 28,827,648 bytes before and 28,884,992 bytes after; endpoint delta 57,344 bytes |

The prior report stated that the four tokenizer queries had Recall@5 and MRR 1.00 for both tokenizers and that cancellation remained partial. These historical claims were not revalidated against their original raw vectors in this run.
