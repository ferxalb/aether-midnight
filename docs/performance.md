# Performance contract and benchmark results

> **v0.5 update:** the v0.5 correction restores the original exact scan default and documents interleaved regression diagnosis. The earlier descending-scan experiment remains in [the v0.5 retrieval record](retrieval-v05.md).
> **v0.4 update:** current memory-recall measurements and the paired v0.3.1 comparison are in [the v0.4 retrieval record](retrieval-v04.md). Older sections here describe earlier strategies.

This page retains the committed Linux benchmark evidence. A separate native macOS run and measured before/after writer optimization are documented in [the performance comparison](performance-comparison.md). The committed benchmark files are historical evidence and are not overwritten by local campaigns.

## Measurement handling

Published benchmark artifacts retain measured result distributions and sample counts. Host, runtime, account, and local path identifiers are omitted. Percentiles use the benchmark's recorded nearest-index rule. Results describe their measured fixture and are not cross-machine guarantees.

## Reproduce

For a low-impact local smoke run, build the current release binary and write a unique artifact under `benchmark/runs/`:

```sh
cargo build --release --locked
target/release/aether-midnight bench --repo "$FIXTURE" --heldout-repo benchmark/retrieval_fixture --samples 5 --index-samples 5
```

For local reproduction guidance, see the [benchmark guide](macos-benchmark-guide.md).

The legacy real-repository runners default to five index samples and 25 query samples. For one isolated comparison, preserve output under a unique directory rather than redirecting over a committed JSON file:

```sh
git clone --depth 1 https://github.com/torvalds/linux.git /tmp/aether-repos/linux
git clone --depth 1 https://github.com/microsoft/vscode.git /tmp/aether-repos/vscode
RUN_DIR=$(mktemp -d benchmark/runs/linux-real.XXXXXX)
target/release/aether-midnight bench --repo /tmp/aether-repos/linux --samples 5 > "$RUN_DIR/real-repositories.json"
target/release/aether-midnight refresh-file --repo /tmp/aether-repos/linux --path init/main.c  # run under /usr/bin/time -l for wall, CPU and peak RSS
```

These legacy real-repository scripts have narrower instrumentation than the new process/cgroup telemetry campaign. The timestamped campaign is the recommended general workflow.

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

## v0.2 persistent memory path and retrieval quality

The local paired run uses the same local reference host (identifiers withheld), release profile, frozen 25-query suite, and 10,000 irrelevant-background records. The verified baseline is commit `01b98c76ff1241d8e4a8459cf9e42445c8e72d16`; the implementation is commit `2ab6393` (engine tree from `9c358e2`, with a rejected ranking threshold experiment reverted). Full raw evidence, including query samples, is in [`memory-retrieval-large-v2-baseline-01b98c7-20261104.json`](../benchmark/evidence/memory-retrieval-large-v2-baseline-01b98c7-20261104.json) and [`memory-retrieval-large-v2-after-2ab6393-20261104.json`](../benchmark/evidence/memory-retrieval-large-v2-after-2ab6393-20261104.json). Percentiles use each artifact's recorded nearest-index rule.

| Persistent MCP measurement | Baseline | v0.2 | Samples / interpretation |
| --- | ---: | ---: | --- |
| Warm recall p50 / p95 / p99 (ms) | 14.446 / 17.953 / 18.481 | 12.398 / 17.363 / 18.515 | n=75 each; one process, 10,027 loaded rows, high-fanout queries |
| Individual remember p50 / p95 / p99 (ms) | 1.980 / 2.611 / 3.273 | 0.162 / 0.335 / 7.212 | 10,027 requests each; p99 includes intermittent SQLite/WAL outliers |
| List p50 / p95 / p99 (ms) | 1.611 / 1.715 / 1.779 | 0.634 / 0.684 / 0.693 | n=30 each |
| Forget p50 / p95 / p99 (ms) | 1.872 / 2.296 / 2.835 | 0.084 / 0.118 / 0.259 | n=31 each |
| Process CPU total | 17.485 s | 2.731 s | 10,195 MCP requests; about 84% lower on this run |
| CPU per request | 1.715 ms | 0.268 ms | CPU time divided by request count |
| Sampled RSS before close | 23,363,584 B | 23,937,024 B | One observation; current RSS, not peak or live heap |
| DB + WAL + SHM before / after close | 6,811,648 / 6,811,648 B | 10,482,616 / 5,369,856 B | One snapshot each; after-close footprint is about 21% smaller, but before-close WAL state is larger |
| Mean response bytes / estimated content tokens | 3,218 B / 236.8 | 2,181 B / 162.9 | Across 25 retrieval queries; ranking changes which bounded results are packed |

Warm recall p95 remains above the proposed 5 ms 10k-record target for this deliberately high-fanout same-namespace dataset. A separate synthetic scaling run (`bench-memory`) uses four namespaces and a different query mix, so its lower latency is not substituted for this user-facing result. Current query planning on that scaling workload is `SCAN memory_fts VIRTUAL TABLE INDEX 32:rM2` followed by an integer-primary-key memory lookup; it has no temporary sort B-tree. The previous `ORDER BY bm25()` plan used a temporary B-tree. The direct-file refresh path is not included in these measurements.

The v2 fixture freezes 25 queries (14 dev, 11 held out) over 27 labeled fictional records plus 10,000 irrelevant records. It covers exact facts, paraphrase, shared vocabulary, conflicts/supersession, temporal history/validity, multi-session references, source provenance, entity ambiguity, Spanish and English, namespace isolation, deletion, abstention, long supersession chains, large background, and multi-hop evidence. This is a retrieval-only evaluation: no model generated answers or received credit for reasoning. The cross-language Spanish/English paraphrase remains an explicit lexical limitation.

| Split | R@1 / R@5 / R@10 | P@5 | MRR | nDCG@10 | Evidence completeness@10 | False positives | Temporal correct / invalid | Namespace leaks |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| Dev baseline (n=14, 12 positive) | .375 / .958 / .958 | .317 | .767 | .809 | .950 | 1 / 2 negatives | 2 / 2; 0 | 0 |
| Dev v0.2 (n=14, 12 positive) | .500 / .958 / .958 | .317 | .875 | .879 | .950 | 0 / 2 negatives | 2 / 2; 0 | 0 |
| Held out baseline (n=11, 8 positive) | .625 / 1.000 / 1.000 | .300 | .906 | .900 | 1.000 | 2 / 3 negatives | 3 / 3; 0 | 0 |
| Held out v0.2 (n=11, 8 positive) | .625 / 1.000 / 1.000 | .300 | .917 | .922 | 1.000 | 1 / 3 negatives | 3 / 3; 0 | 0 |
| All baseline (n=25, 20 positive) | .475 / .975 / .975 | .310 | .823 | .846 | .969 | 3 / 5 negatives | 5 / 5; 0 | 0 |
| All v0.2 (n=25, 20 positive) | .550 / .975 / .975 | .310 | .892 | .896 | .969 | 1 / 5 negatives | 5 / 5; 0 | 0 |

The v0.2 change improved top-rank quality and reduced labeled negative-query false positives in this fixture without changing R@5, temporal correctness, evidence completeness, or namespace leakage. It does not establish semantic retrieval. A candidate requiring three distinct query terms improved a dev measure but regressed held-out R@5 from 1.000 to .875, evidence completeness from 1.000 to .917, MRR from .906 to .875, and nDCG@10 from .900 to .860; that code was reverted. The rejected run is retained as [`memory-retrieval-large-v2-rejected-threshold3-5e19393-20261104.json`](../benchmark/evidence/memory-retrieval-large-v2-rejected-threshold3-5e19393-20261104.json).

The v3 memory FTS migration removes the second full-text copy. In the same paired MCP artifacts, the post-close database plus sidecars was 6,811,648 B at baseline and 5,369,856 B after v0.2 (single snapshots, not a repeated footprint distribution). The v0.2 before-close total was 10,482,616 B, reflecting WAL/SHM state before connection shutdown; this is a lifecycle snapshot, not a comparable compacted-size claim. External-content FTS was not separately prototyped, so contentless-delete is retained based on compatibility and measured post-close footprint, not a claimed exhaustive storage winner comparison.

The synthetic 1k/10k/100k per-operation profile is recorded separately in [`memory-scaling-2ab6393-20261104.json`](../benchmark/evidence/memory-scaling-2ab6393-20261104.json). It includes persistent-session and one-shot store/recall samples, setup throughput, query plan, database bytes, CPU time, and sampled RSS. Its generated corpus is a performance workload, not retrieval-quality ground truth.

## Paired persistent MCP code retrieval

The historical local smoke run (Python tooling since removed) used the same generated 10,000-file / 100 MiB code fixture (seed `20261103`), one indexing worker, and five independent sessions for each release binary. Each session measured the first useful context and five subsequent `search_code` tool calls (n=25 warm searches). Both indexes completed catalog and structural indexing; all measured searches hit the expected file. The paired summary and raw warm samples are in [`mcp-code-context-paired-01b98c7-2ab6393-20261104.json`](../benchmark/evidence/mcp-code-context-paired-01b98c7-2ab6393-20261104.json).

| Persistent MCP code measurement | Baseline | v0.2 | Samples |
| --- | ---: | ---: | --- |
| Initialize + first useful context p50 / p95 / p99 (ms) | 72.135 / 72.793 / 72.793 | 70.014 / 72.607 / 72.607 | n=5 |
| First `search_code` tool latency p50 / p95 / p99 (ms) | 13.856 / 13.983 / 13.983 | 13.975 / 14.044 / 14.044 | n=5 |
| Warm `search_code` p50 / p95 / p99 (ms) | 12.240 / 13.876 / 14.625 | 12.152 / 13.235 / 13.543 | n=25 |
| Fresh-process initialize + one search p50 / p95 / p99 (ms) | 112.511 / 114.030 / 114.030 | 112.772 / 113.913 / 113.913 | n=5 |
| Full index p50 / p95 / p99 (ms, one worker) | 3,919.645 / 4,102.129 / 4,102.129 | 3,814.034 / 3,871.189 / 3,871.189 | n=5 |
| Full-index CPU p50 (s), sampled RSS high-water p50 (MB) | 4.225, 25.997 | 4.124, 25.846 | n=5 |
| Idle MCP CPU | 0.0% | 0.0% | n=5; below measured interval resolution |

The code retrieval path is essentially unchanged, with no observed regression; the 10k-file one-worker full-index p95 is 3.87 s and is not evidence for the proposed 3-second multiworker target. Percentiles from n=5 have low resolution; the p95/p99 values are the largest observations. This campaign does not include direct-file refresh latency or simultaneous code-index and memory-recall load.

The direct-file path has a separate focused comparison from the since-removed Python runner. It created one Rust target plus 9,999 small background files, indexed the 10,000-file repository, and then ran 30 paired updates in one MCP session. Each timer starts after writing the changed source and covers the index/refresh tool call plus a `search_code` call that verifies the new result is fresh.

| 10k-file update-to-visible path | p50 / p95 / p99 (ms) | n | Result status |
| --- | ---: | ---: | --- |
| Full `index_repository` reconciliation + search | 322.038 / 379.019 / 419.110 | 30 | Every hit fresh; catalog complete |
| `refresh_indexed_file` + search | 11.547 / 12.689 / 12.911 | 30 | Every hit fresh; catalog partial until reconciliation |

Direct refresh reduced p95 by 96.7% (29.9× faster) on this generated fixture, under the proposed 150 ms one-file visibility target. The final full reconciliation restored complete status. This is a same-process paired result on one host; it does not measure missed filesystem notifications because the path is explicit, and it does not imply a watcher or automatic change discovery.

## FTS5 tokenizer comparison and lifecycle stress

The fixed fixture also compared contentless FTS5 trigram and `unicode61 remove_diacritics 2` using a Python SQLite 3.53.1 harness (since removed); the release build uses bundled SQLite 3.54.1. Build and final index-size values are single observations (n=1, anecdotal); each tokenizer’s four-query retrieval score is also anecdotal (n=4). Warm search used 100 samples per content query (n=400 aggregate), and exact path lookup used n=100.

| Tokenizer | Build ms (n=1) | Index bytes / source ratio (n=1) | Warm search p50 / p95 / p99 ms (n=400) | Recall@5 / MRR (n=4) |
| --- | ---: | ---: | ---: | ---: |
| FTS5 trigram | 3,909.651 | 112,640,000 / 1.074× | 0.322 / 0.514 / 0.903 | 1.00 / 1.00 |
| FTS5 `unicode61 remove_diacritics 2` | 810.087 | 15,368,192 / 0.147× | 0.055 / 0.138 / 0.210 | 1.00 / 1.00 |

The separate exact path lookup measured p50/p95/p99 0.001051 / 0.002303 / 0.008172 ms (n=100). Sorted per-query recall and reciprocal-rank values are in the result JSON; quality sample n=4 is anecdotal. These tokenizer scores apply only to the fixed four-query fixture. Midnight uses `unicode61`; it supports short whole-token matches and does not provide arbitrary in-token substring search. Tantivy and Zoekt were not evaluated in this Linux run.

The 30 open/index/update/search/status/close stress cycles measured p50/p95/p99 4.236 / 6.158 / 6.240 ms (n=30). Current RSS after each cycle was 33,431,552 bytes for all 30 samples. This does not establish zero leaks. The separate process-wide peak RSS counter was below the sampled current RSS and is treated as unmeasured. One cancellation observation was partial (n=1, anecdotal). Allocator live-heap and mmap-page measurements remain unmeasured.

## Pending verification

These items are **pending (not run)** for the historical Linux run. A separate current Intel macOS reference campaign is documented in the [native comparison](performance-comparison.md):

- Apple Silicon macOS: pending (not run)
- Go: pending (not run)
- Node.js: pending (not run)
- Bun: pending (not run)
- Windows: pending (not run)
- ARM: pending (not run)

The first synthetic run used the inherited environment collector, which invoked `node --version`, `go version`, and attempted `bun --version`. No Go, Node.js, or Bun example/build was run. Those version probes have been removed from the current collector; runtime verification remains pending.

## Historical summary retained from the previous report

The previous documentation reported these older macOS summary values. They are retained here as historical evidence; the original sample arrays and `benchmark/results.json` were not present in this checkout, so p99/raw samples cannot be reconstructed. The new Intel macOS results are separate and available in the [comparison](performance-comparison.md).

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

## v0.3 memory retrieval and WAL follow-up (2026-11-05)

This campaign starts at baseline `9d39467b5c5531bb53464b3f1ce4254304ef6077` and measures candidate code at `667cea3ad1cb3c475d5deeaf675f5eb374fcdf09` on `perf/midnight-v03-end-to-end-engine`. It ran on a local Linux reference environment with host and storage identifiers withheld. Rust was `1.99.1`; bundled SQLite was `3.54.1`. No OS page-cache flush was attempted. The release binary used the normal optimized, stripped profile. Phase profiling used an optimized, debug-symbol build and `SQLITE_ENABLE_STMT_SCANSTATUS`.

### Broad-query results

The benchmark stores 100,000 records across four namespaces; the measured namespace has 25,025 active records. It runs the warm `namespace` one-term query with a result limit of 10. Baseline and candidate are both 100-sample persistent-session runs. The candidate figures below summarize three separate 100-sample campaigns.

| 100k broad recall | p50 ms | p95 ms | p99 ms | Samples |
| --- | ---: | ---: | ---: | ---: |
| Baseline, BM25 rank ordering | 23.725 | 26.508 | 28.368 | 100 |
| Candidate, balanced row-ID windows, median of three runs | 6.892 | 7.434 | 8.409 | 100 per run |

Across the candidate runs, p50 ranged from 6.764–7.016 ms, p95 from 7.324–7.530 ms, and p99 from 7.511–9.938 ms. An initial full-scale run had a single 40.8 ms p99; three repeat runs did not reproduce that outlier. The repeated result meets the proposed 100k p95 ≤15 ms and p99 ≤25 ms targets on this host, with the first outlier retained as a warning about tail variability.

The non-stripped, phase-profiled run collected 100 samples per strategy on the same database. BM25 ordering measured 27.010 / 29.723 / 32.231 ms p50/p95/p99, while balanced row-ID windows measured 7.144 / 8.439 / 16.164 ms. SQLite candidate stepping accounted for 26.846 / 29.541 / 32.066 ms of the ranked path and 6.963 / 8.226 / 15.955 ms of the candidate path. Hydration stayed below 0.06 ms p99 in both. This confirms that FTS query execution dominates this workload; full-record hydration and the Rust reranker are small contributors.

Balanced selection takes up to 25 matching rows from each end of row-ID order when the first window fills. It de-duplicates overlaps before extracting record content and keeps the existing exact namespace, deletion, temporal, phrase, term-coverage, BM25, and stable tie-break checks. Selective one-term queries stop after the first window when it returns fewer than 25 rows; the profiled selective query returned one candidate in 0.828 / 0.980 / 1.075 ms p50/p95/p99. Multi-term queries retain BM25 rank ordering. This is a bounded sample, so a relevant one-term match between the windows can be omitted.

`EXPLAIN QUERY PLAN` showed FTS5 row-ID virtual-table scans (`INDEX 64:rM2` and `INDEX 192:rM2`) plus primary-key memory lookups, with no temporary sort B-tree. Scanstatus reported two FTS loops and 50 plan-loop rows for the balanced strategy, versus one loop and 50 rows for ranked ordering. Statement VM steps were 1,330 versus 1,315; page-cache misses were 162 versus 565. FTS5 does not expose the internal number of term postings visited, so scanstatus rows must not be interpreted as posting counts. The eight-group 2/4/8/16 MiB cache screen did not improve recall materially; the database used about 0.8 MiB in all groups and the process RSS stayed about 15.6 MiB. No larger default cache was adopted.

The normal release scaling run measured persistent recall at 1k records p50/p95/p99 0.459 / 0.710 / 0.796 ms, at 10k 1.080 / 1.377 / 1.472 ms, and at 100k 7.476 / 9.621 / 40.781 ms in that run. The 100k tail was the single outlier described above; the separate three-run series is the reportable repeated result. Database size after measured operations was 446,464 / 2,691,072 / 25,559,040 bytes at 1k / 10k / 100k. The mixed 1k-to-100k campaign's sampled cumulative process peak was 10,260,480 bytes; three isolated 100k runs were 9,478,144–9,744,384 bytes. These are RSS observations, not allocator live-heap measurements.

### Retrieval quality

The production default uses balanced windows only for queries with one informative term. On the unchanged 14-query v0.2 development split, the candidate exactly matched the baseline: Recall@1/5/10 0.500 / 0.958 / 0.958, Precision@5 0.317, MRR 0.875, nDCG@10 0.879, evidence completeness 0.950, false-positive rate 0, and namespace leaks 0. On the 11-query held-out split, metrics also matched exactly: Recall@1/5/10 0.625 / 1.000 / 1.000, Precision@5 0.300, MRR 0.917, nDCG@10 0.922, completeness 1.000, temporal accuracy 1.000, false-positive rate 0.333 (one of three negative queries), and namespace leaks 0.

One supplemental broad-term development query was added before trying the candidate strategies. It asks for `status` and labels an English report plus a Spanish source that contains no shared `status` token. Both baseline and candidate returned the English report at rank 1 and missed the Spanish evidence, so Recall@10 remained 0.5 and Precision@5 remained 0.2. The candidate changed which irrelevant background rows filled the rest of the result list. Its MCP query p95 was 2.166 ms versus 9.193 ms for ranked ordering. This case shows a lexical cross-language limitation; it does not justify a semantic retrieval claim. The full 15-query supplemental development result was Recall@1/5/10 0.500 / 0.923 / 0.923, Precision@5 0.308, MRR 0.885, nDCG@10 0.859, completeness 0.909, temporal accuracy 1.000, no negative-query false positives, and zero namespace leaks.

The fixture remains small (25 original queries plus one supplemental development case), with no confidence intervals. Precision@5 uses a fixed denominator of five, so a query with fewer than five relevant records has a lower maximum score. The candidate did not raise quality metrics, and the existing held-out false-positive limitation remains.

The persistent MCP evaluator also measured 10,027 `remember` calls per development/held-out run. Across the two runs, remember p50/p95/p99 ranged 0.085–0.087 / 0.213–0.253 / 1.977–2.565 ms, while `forget` (n=31) p50/p95/p99 ranged 0.043–0.065 / 0.075–0.114 / 0.267–0.278 ms. Process CPU was 1.30–1.37 seconds for about 10,165 requests, and RSS before close was 9.22–9.42 MB. The database plus WAL/SHM measured 10,482,616 bytes before close and 5,369,856 bytes after close. These are local Linux fixture results and do not replace the separate macOS evidence above.

### WAL checkpoint investigation

The MCP evaluator measured 10,027 individual `remember` requests per run and read `PRAGMA wal_checkpoint(NOOP)` after each completed response. `NOOP` reports WAL progress without performing a checkpoint. The default writer threshold was 1,000 pages. In three counterbalanced runs, median per-run p50/p95/p99/p99.9 was 0.097 / 0.278 / 1.962 / 3.159 ms; p99 stayed below the proposed 4 ms target. There were 106 observed checkpoint-progress events per 10,027 calls, and the largest observed uncheckpointed WAL was 999 frames. Of 28 calls above 4 ms across the three runs, 23 coincided with an increase in checkpointed frames after that request. Checkpoint-progress requests had a much higher latency distribution than requests with no observed progress, while the first local campaign also recorded rare 14–20 ms outliers.

The evaluator compared 256-, 1,000-, and 4,096-page thresholds in counterbalanced three-run campaigns. At 256 pages, median p95 rose to 0.509 ms and checkpoint events increased to 411, while median p99 improved to 1.550 ms and uncheckpointed WAL stayed at or below 255 frames. At 4,096 pages, median p95/p99 fell to 0.194 / 0.354 ms because fewer than 1% of calls coincided with checkpoint progress, but p99.9 rose to 6.524 ms, checkpoint-event p50 was 6.263 ms, and uncheckpointed WAL reached 4,095 frames. The default was retained because it meets p95/p99 goals without the increased frequency of 256 pages or the larger WAL and long checkpoint tails at 4,096 pages.

The end-to-end correlation supports automatic checkpoint work as a major contributor to the write tail, but this campaign did not separately time transaction commit, checkpoint, synchronization, or lock wait. It does not prove checkpoint I/O is the only cause. The durability configuration remains WAL with `synchronous=NORMAL`; no durability setting was weakened. `AETHER_WAL_AUTOCHECKPOINT_PAGES` is an experimental-build-only knob for these disposable fixture runs and is not a production setting.

Compact committed measurements are in [`memory-retrieval-v03-20261105.json`](../benchmark/evidence/memory-retrieval-v03-20261105.json). Full local sample arrays remain under the git-ignored `benchmark/runs/v03-20261105/`. Reproduction commands:

```sh
cargo build --release --locked
target/release/aether-midnight bench-memory --sizes 1000,10000,100000
LIBSQLITE3_FLAGS=SQLITE_ENABLE_STMT_SCANSTATUS cargo build --profile profiling \
  --features sqlite-scanstatus,retrieval-experiments --locked
target/profiling/aether-midnight bench-memory --sizes 100000 --profile-recall
target/release/midnight-harness memory-quality --fixture benchmark/memory_quality_single_term_v1.json --split dev --repeats 3 --json-output /tmp/memory-retrieval-v03-dev.json
```

The native `midnight-harness memory-quality` command no longer samples WAL progress; WAL and database sizes come from `diagnose` and `bench-memory`.

For the WAL experiment, build with `--features retrieval-experiments`, then run the same evaluator with `AETHER_WAL_AUTOCHECKPOINT_PAGES=256`, `1000`, and `4096`. Each run creates its own disposable database. The existing Windows/macOS campaigns, full 200-query evaluation, large-payload stress, multi-process contention, long-reader checkpoint starvation, and separate internal commit timing remain unmeasured.

## v0.3.1 memory retrieval correctness follow-up

The production default now uses FTS5 BM25 rank order for one-term and multi-term recall. The previous two-ended row-ID sample remains available only in retrieval experiment builds. This corrects the measured interior-position omission while retaining the bounded rerank window.

Each returned memory result carries additive `candidate_coverage`: `complete` means the filtered FTS scan ended before the cap; `bm25_top_k` means the BM25 window filled; `row_id_sample` identifies the experimental sampler. Saturation is visible to callers, but it is not a semantic recall guarantee. FTS5 ties may still change which weak filler candidates enter the bounded window when records are inserted in a different order.

The frozen v0.2 suites had identical baseline and candidate metrics. The 25-query suite returned Recall@1/5/10 of .550/.975/.975, P@5 .310, MRR .892, nDCG@10 .896, evidence completeness .969, temporal accuracy 1.0, zero stale-fact results, zero namespace leaks, and one false-positive query out of five negatives. The 26-query single-term suite returned Recall@1/5/10 of .548/.952/.952, P@5 .305, MRR .897, nDCG@10 .883, evidence completeness .941, temporal accuracy 1.0, zero stale-fact results, zero namespace leaks, and one false-positive query out of five negatives.

The adversarial fixture has seven labeled exact-phrase targets placed at 0%, 10%, 25%, 50%, 75%, 90%, and 100% in seven 1,000-record matching collections. Balanced windows found two endpoints (Recall@1/5/10 .286); BM25 found all seven at rank 1 in both forward and reverse insertion order (Recall@1/5/10 1.0). Forward/reverse top-10 Jaccard increased from .015 to .053 because tied low-value fillers still differ; the labeled target stayed stable.

The bounded release scale campaign reported these recall distributions on a local native host. Host and platform identifiers are intentionally withheld:

| Layout / records | Recall p50 / p95 / p99 ms | Process CPU for workload | Peak RSS | DB after measured ops |
| --- | ---: | ---: | ---: | ---: |
| Four namespaces / 1k | 1.514 / 1.619 / 1.732 | 0.580 s | 14,462,976 B | 446,464 B |
| Four namespaces / 10k | 4.101 / 4.218 / 4.496 | 1.418 s | 18,202,624 B | 2,691,072 B |
| Four namespaces / 100k | 29.492 / 30.285 / 30.548 | 10.050 s | 44,675,072 B | 25,559,040 B |
| One namespace / 1k | 2.139 / 2.213 / 2.546 | 0.591 s | 14,639,104 B | 446,464 B |
| One namespace / 10k | 10.103 / 10.323 / 10.433 | 2.474 s | 23,212,032 B | 2,707,456 B |
| One namespace / 100k | 99.984 / 102.887 / 123.926 | 23.450 s | 34,582,528 B | 25,997,312 B |

At 100k, the previous balanced default measured p95 9.156 ms on the same local test setup; BM25 measured 30.285 ms for four distributed namespaces. In one namespace, BM25 measured 102.887 ms p95. The 100k and single-namespace 10k results miss the target latency. A selective one-result profile measured 5.738 / 6.081 / 6.468 ms p50/p95/p99.

Profiling the 100k single-namespace broad query showed the 50-candidate window saturated. Total p50/p95/p99 was 113.436 / 116.452 / 125.139 ms; SQLite candidate stepping was 112.924 / 115.865 / 124.542 ms. Hydration p95 was .060 ms, candidate tokenization .187 ms, and final sort .008 ms. SQLite reported 1,315 VM steps, zero sort operations, 2,103,296 bytes of page cache used, and 688 cache misses at p50. The query plan used the FTS5 rank scan and a primary-key lookup. Host, runtime, and storage identifiers are withheld; OS-level cache and disk I/O were not attributed.

The 10k-file code refresh check completed 30 samples: full reconciliation p95 325.159 ms and direct-file refresh p95 11.847 ms. Every search returned fresh content and final reconciliation restored catalog completeness. The default WAL autocheckpoint remained 1,000 pages. Individual remember, superseding update, forget, migration, temporal, deletion, namespace, concurrent-write, cancellation, indexing freshness, and MCP regression checks passed. Long-reader WAL growth, repeated compaction, battery, and idle CPU were not measured in this follow-up.

The compact, host-redacted result summary is [`memory-retrieval-v031-20261105.json`](../benchmark/evidence/memory-retrieval-v031-20261105.json). Reproduce locally with:

```sh
cargo build --release --locked
target/release/midnight-harness memory-quality --binary target/release/aether-midnight --fixture benchmark/memory_quality_v2.json --split all --repeats 3
target/release/midnight-harness memory-quality --binary target/release/aether-midnight --fixture benchmark/memory_quality_single_term_v1.json --split all --repeats 3
target/release/midnight-harness memory-quality --binary target/release/aether-midnight --fixture benchmark/memory_retrieval_adversarial_v1.json --split all --repeats 1 --insertion-order forward
target/release/aether-midnight bench-memory --sizes 1000,10000,100000 --namespace-count 4
target/release/aether-midnight bench-memory --sizes 1000,10000,100000 --namespace-count 1
target/release/aether-midnight refresh-file --repo "$FIXTURE" --path src/00000.rs  # measure under /usr/bin/time -l
```

## Exact KNN and embedding throughput (v0.1 Phase 3, 2026-11-07)

Measured with the compiled release CLI through its real entry point (`target/release/aether-midnight memory search … --mode semantic`), one process and one request per measurement, `/usr/bin/time -l` for wall, CPU (user + sys) and peak RSS. Platform category: macOS x86_64, 6-core/12-thread desktop CPU, 24 GiB RAM, local SSD. Seeding went through Midnight (MCP `remember_batch`, then `embed`); the databases were reused for every run. Ordering was identical across all 15 runs per size, and the top-10 matched an independent brute-force oracle (ties of mathematically equal scores resolve by ascending id, as specified).

**Scan cost, isolated from model load.** To keep the 4 s local-model load out of the figure, vectors were embedded through the OpenRouter code path against a loopback mock that returns deterministic 384-d bag-of-words vectors (scan cost does not depend on vector values). The query is one loopback request. Each size is a separate memory namespace (15 runs each; milliseconds):

| Vectors (384 d, f32) | Wall median (min–max) | CPU median | Peak RSS median |
| ---: | ---: | ---: | ---: |
| 100 | 230 (230–240) | 220 | 15.9 MiB |
| 10,000 | 250 (250–260) | 240 | 16.2 MiB |
| 100,000 | 470 (460–470) | 450 | 19.5 MiB |
| 100,000, lexical mode (no embedding stage) | 110 | – | 14.5 MiB |

The fixed ≈ 220 ms is process start, the remote client (rustls platform-verifier initialisation) and the loopback request; it is the same for every size. The scan adds ≈ 20 ms at 10,000 vectors and ≈ 240 ms at 100,000 (≈ 2.4 µs per vector including the SQLite read, BLOB copy and dot product), linear as expected for exact search. Memory stays flat because vectors stream from SQLite and only a bounded top-k is kept. Clock resolution is 10 ms, so the 10,000 figure is ±10 ms.

**Local model, 10,000 vectors (real `multilingual-e5-small`).** `embed --namespace` over 10,000 short synthetic memories took 117 s wall, 1,245 s CPU (12 threads) and 1.56 GiB peak RSS (≈ 85 texts/s; 100 memories: 5.0 s including model load). `memory search --mode semantic` took a median of 3.98 s wall for both 100 and 10,000 stored vectors (9 runs each, 1.52 GiB RSS): model load dominates, and the 10,000-vector scan is below the 10 ms clock resolution. A fresh process per query pays that load; a long-lived caller should prefer the persistent MCP path once semantic modes are exposed there.

Not measured: ANN, concurrent queries, Apple silicon, Linux, real cloud-provider latency (network and provider dependent), cold-cache disk. Lexical quality baselines are unchanged (see `docs/v0.1-status.md`).
