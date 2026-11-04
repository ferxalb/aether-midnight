# Performance contract and benchmark results

The v0.1 release targets are unchanged. Measurements below use the fixed fixture and existing timer boundaries; results are machine-specific. A fresh database does not mean a cold-disk test: the harness does not evict the OS file cache.

## Reproduce

The selected run used this command from the repository root:

    env PATH=/workspace/.cargo/bin:$PATH RUSTUP_TOOLCHAIN=stable MIDNIGHT_INDEX_WORKERS=3 MIDNIGHT_BENCH_BATCH_FILES=512 MIDNIGHT_BENCH_BATCH_BYTES=16777216 MIDNIGHT_BENCH_SAMPLES=30 MIDNIGHT_INDEX_SAMPLES=30 MIDNIGHT_STRESS_CYCLES=20 python3 scripts/run_benchmark.py > /tmp/aether-midnight-final-current-20261104.json

The runner builds the release binary, generates the manifest fixture in a temporary directory, runs the main benchmark, compares tokenizer implementations using Midnight's bundled SQLite, and evaluates the fixed retrieval suite. It writes JSON to stdout; the command above keeps the exact run in a unique temporary file. The checked-in [results JSON](../benchmark/results.json) is a byte-for-byte copy of the selected final run. The [pre-pipeline Linux baseline](../benchmark/baselines/pre-pipeline-linux-20261104.json) preserves the earlier three-sample run.

The fixture manifest remains seed 20261103, exactly 10,000 eligible files and 104,857,600 source bytes, with no skipped files. No eligibility rule or timer boundary was changed. MIDNIGHT_BENCH_SAMPLES controls first-context and warm-search sample counts (default 30); incremental measurements use at most 10 samples. MIDNIGHT_INDEX_SAMPLES controls fresh-database indexing samples (default 3), MIDNIGHT_FTS_SAMPLES controls warm samples per tokenizer query (default 30), and MIDNIGHT_STRESS_CYCLES controls stress cycles (default 20). MIDNIGHT_INDEX_WORKERS defaults to Rust's available parallelism and is clamped to 1–12. For controlled batch experiments, MIDNIGHT_BENCH_BATCH_FILES and MIDNIGHT_BENCH_BATCH_BYTES override the 512-file and 16 MiB defaults; file count is clamped to 1–2,048 and bytes are capped at 16 MiB.

Percentiles use the harness estimator ceil((n - 1) × p), capped at n - 1. The full-index distribution's samples_ms is sorted for percentile calculation. Run-order samples, unique sample IDs, per-stage timings, and separate WAL checkpoint times are in timings.full_index_samples in the raw JSON. Worker-stage durations are summed across workers and may overlap. The full-index timer ends when indexing and reconciliation return; WAL checkpoint time is reported separately.

Timer boundaries are unchanged: first grounded context starts before launching a fresh Midnight process and ends when grounded JSON returns; build time is excluded. Full-index timing surrounds one indexing call against a fresh database and includes catalog reconciliation and required commits, but excludes the separately timed WAL checkpoint. Warm search runs inside the benchmark process against a complete index and includes current-source validation. Incremental timing starts after the changed source file is written and ends when a fresh search sees the update. The partial-context sample searches while a separate full-corpus index process is still active. All stage durations use Rust Instant. Run and sample IDs are unique process/time/counter identifiers.

## Targets and latest measurements

| Gate | Target | Latest measured result | Status |
| --- | --- | --- | --- |
| First grounded context, fresh process and empty index | p50 ≤ 1,000 ms; p95 ≤ 3,000 ms | 46.1 / 53.4 ms (30) | Pass on this host |
| Complete lexical and structural index, exactly 10,000 files / 100 MiB | p95 ≤ 3,000 ms | 1,658.7 / 1,754.1 ms (30) | Below limit on Linux; reference-host gate **unverified** |
| Useful partial context while indexing | ≤ 3,000 ms | 57.5 ms (1); search returned while the index process was still running and marked the result partial | Pass on one sample |
| Warm search on a complete index | p95 ≤ 50 ms | 6.6 / 7.7 ms (30) | Pass on this host |
| One-file incremental update visible to search | p95 ≤ 1,000 ms | 158.1 / 170.5 ms (10) | Pass on this host |

The full-index result is a measured pass against 3 seconds on the current Linux host. The formal reference-host gate remains unverified because the previously documented reference machine is macOS 26.7.1 and was not available for this run. The last reported run on that Mac, before this pipeline change, was 4.72 / 7.14 seconds p50/p95 (3 samples), so that historical result failed the target. The new code must be measured on that reference host before the release gate can be called passed.

A separate pre-pipeline run on this same Linux environment measured full indexing at 2,355.0 / 2,459.2 ms p50/p95 (3 fresh databases). It used the same fixture and bundled SQLite 3.54.1, with an 18,710,528-byte database (0.1784× source). The selected current database is 12,288 bytes larger at 18,722,816 bytes (0.1786× source). The final run has 30 samples and is the release-quality distribution; the small baseline sample is useful context, not a robust p95 comparison. The older Mac report also recorded first-context 157 / 167 ms, warm search 15.4 / 16.4 ms, and incremental update 315.5 / 424.5 ms p50/p95.

## Full-index distribution and phase accounting

The final 30 external full-index timer values, sorted by the benchmark's percentile estimator, are (milliseconds):

    1575.527035, 1577.925912, 1590.735982, 1603.331621, 1605.563840,
    1610.142400, 1611.395449, 1628.566686, 1629.652085, 1640.863508,
    1647.439460, 1648.401760, 1651.263874, 1656.178743, 1657.998965,
    1658.703971, 1662.881158, 1663.300084, 1676.712645, 1679.178235,
    1685.106571, 1685.708307, 1686.799514, 1692.761366, 1697.623513,
    1706.033868, 1708.107703, 1731.371990, 1754.116527, 1773.605393

Per-stage distributions below use 30 full-index runs. Read, hash, identifier extraction, and parse are aggregate worker service times; they overlap across workers. FTS payload, FTS execution, metadata writes, and symbol writes are nested within index writing and should not be added to it.

| Stage | p50 ms | p95 ms |
| --- | ---: | ---: |
| Discovery | 20.7 | 24.2 |
| Metadata lookup and catalog checks | 136.9 | 157.9 |
| File reads (aggregate worker time) | 156.3 | 176.2 |
| Hashing (same read buffer) | 77.8 | 84.5 |
| Identifier decomposition | 201.3 | 226.3 |
| Tree-sitter parse (aggregate worker time) | 1,169.8 | 1,238.5 |
| Index writes, including nested stages below | 851.0 | 889.1 |
| FTS payload assembly (nested) | 15.1 | 15.9 |
| FTS statement execution (nested) | 320.5 | 337.3 |
| Metadata writes (nested) | 447.6 | 464.5 |
| Symbol writes (nested) | 60.9 | 63.9 |
| Transaction commits | 97.4 | 108.5 |
| WAL checkpoint, separate from full-index timer | 0.5 | 0.8 |
| Reconciliation and completion | 1.4 | 1.9 |
| Worker wait and backpressure | 529.1 | 579.9 |
| Residual coordination/overhead | 20.4 | 23.2 |

For each individual sample, phase_accounted_total equals the report's internal full_index_ready value. Against the external timer, the largest residual difference was 3.055 ms (0.18%), within the 1% accounting requirement. All 30 per-run phase maps and unique sample IDs are retained in the JSON. Sample 1 was ID 21541-1: external full-index 1,773.605 ms; internal phase total 1,772.459 ms; residual 23.2718 ms. Its separate WAL checkpoint took 0.4554 ms.

Each eligible fixture file uses one bounded forward read stream: every full-index report records 10,000 source-file opens and 104,857,600 bytes read. Larger files may require multiple 64 KiB read calls; the code does not reopen or reread a file to hash it. The bounded reader checks cancellation between chunks. The implementation hashes the UTF-8 string made from that read buffer and passes the same string to parsing and indexing; it does not reread a file to hash it. The indexed source throughput for the selected worker configuration was about 60.3 MiB/s at the final p50.

## Worker and transaction sweeps

The host CPU is an 80-core AMD EPYC 9V74, with three CPUs in the process affinity mask and a cgroup quota of two cores. Rust reports two available logical CPUs. The useful worker sweep tested 1, 2, and 3 workers, with the default 512-file / 16 MiB transaction batch held constant. Each exploratory configuration used three fresh databases; at n=3, p95 is the maximum sample.

| Workers | p50 / p95 ms (n=3) | Raw samples ms | Indexed source MiB/s at p50 | Worker wait p50 / p95 ms | Peak RSS bytes | Max buffered bytes | Max queued artifacts | Index bytes |
| ---: | ---: | --- | ---: | ---: | ---: | ---: | ---: | ---: |
| 1 | 2,649.6 / 2,813.7 | 2560.093, 2649.636, 2813.750 | 37.7 | 1534.4 / 1723.9 | 28,008,448 | 5,487,054 | 2 | 18,722,816 |
| 2 | 1,741.8 / 1,806.1 | 1726.569, 1741.842, 1806.080 | 57.4 | 657.7 / 699.4 | 28,282,880 | 5,549,970 | 4 | 18,722,816 |
| 3 | 1,645.8 / 1,690.0 | 1633.530, 1645.800, 1689.969 | 60.8 | 521.8 / 563.6 | 34,107,392 | 5,612,886 | 6 | 18,722,816 |

Three workers won this current-source sweep despite the two-core quota and were selected for the final run. The peak RSS remained below the 57,835,520-byte regression ceiling. Worker count 3 was tested because three CPUs were visible in the affinity mask. Counts 4, 8, and 12 were not run because they exceed the three-CPU affinity mask and two-core quota.

After selecting three workers, transaction batch size was varied while the 16 MiB byte limit stayed fixed. Each row is three fresh-index samples. The 512-file setting had the best measured p50/p95; 1,024 was slower and used substantially more memory.

| Files per transaction | p50 / p95 ms (n=3) | Raw samples ms | Peak RSS bytes | Max buffered bytes | Max pipeline artifacts |
| ---: | ---: | --- | ---: | ---: | ---: |
| 256 | 1,683.9 / 1,723.4 | 1646.406, 1683.924, 1723.365 | 20,938,752 | 2,895,702 | 261 |
| 512 | 1,645.8 / 1,690.0 | 1633.530, 1645.800, 1689.969 | 34,107,392 | 5,612,886 | 517 |
| 1,024 | 1,756.6 / 1,819.6 | 1671.125, 1756.647, 1819.632 | 52,297,728 | 11,047,254 | 1,029 |

## Search quality and SQLite tokenizer evidence

The existing five labelled benchmark cases remain in benchmark/manifest.json: exact symbol, exact path, lexical, cross-file symbol, and natural-language description. Their quality remained Recall@5 1.00 and MRR 1.00. Memory temporal, provenance, scope, and immediate-forget checks also passed.

The expanded fixed suite in [retrieval_queries.json](../benchmark/retrieval_queries.json) has 51 labelled queries: 13 Rust, 12 Python, 12 TypeScript, and 12 TSX, with 46 positive cases and five negative or ambiguous cases. It includes full paths and symbols, snake/camel/Pascal case, acronym/digit boundaries, normalized identifiers, comments/docstrings, cross-file symbols, natural-language queries, negative queries, repository and freshness checks, and the fragment queries refreshTok and reshTok. Existing worktree-isolation coverage passed in the Rust test suite.

| Strategy | Recall@5 | MRR | Unsupported fragment cases | Notes |
| --- | ---: | ---: | ---: | --- |
| unicode61 only | 0.8043 | 0.8043 | 5 | Four refreshTok boundary-prefix cases and one arbitrary-middle fragment were missed |
| unicode61 plus identifier decomposition | 0.9783 | 0.9783 | 1 | Default shipped strategy; refreshTok works, arbitrary reshTok inside refreshToken remains unsupported |
| unicode61 plus bounded literal scan | 1.0000 | 1.0000 | 0 | Comparison only, not shipped; scans at most 100 files / 4 MiB per query; 18,666 bytes scanned across suite fallback cases |

Repository isolation, fresh-result reporting before mutation, and stale-result reporting after source mutation all passed for the indexed strategies. The ambiguous negative query retry returned all four language fixture files under each strategy; this is the recorded false-positive example. The CLI and MCP descriptions explicitly state that arbitrary in-token substrings are unsupported by the shipped search path.

The tokenizer comparison was rerun with the exact source fixture, insertion code, transaction size, and SQLite runtime bundled by Midnight (SQLite 3.54.1), not host Python SQLite. Each tokenizer used the same four content queries with 30 warm samples per query (120 total); exact path matching is measured separately by the primary benchmark.

| FTS5 tokenizer | Build ms | Index bytes | Ratio to source | Warm search p50 / p95 ms (n=120) | Recall@5 / MRR |
| --- | ---: | ---: | ---: | ---: | ---: |
| Trigram | 3,884.0 | 112,640,000 | 1.074× | 0.792 / 1.185 | 1.00 / 1.00 |
| unicode61 remove_diacritics 2 | 548.8 | 15,368,192 | 0.147× | 0.103 / 0.195 | 1.00 / 1.00 |

The shipped format remains version 2 and keeps unicode61. Identifier decomposition adds searchable boundary subwords while preserving full identifiers; it requires no tokenizer or schema migration. The complete measured database, including metadata and symbols, was 18,722,816 bytes (0.179× source bytes).

## Resource safety and cancellation

The selected pool has one SQLite writer, three dedicated workers, and a handoff capacity of two queued/active items per worker (six at the selected setting). Outstanding worker reservations are bounded at 8 MiB; the writer batch is bounded at 16 MiB, for a configured maximum of 24 MiB across worker reservations and pending batch artifacts. The observed maxima across the final 30 runs were 188,748 bytes in flight, 5,612,886 accounted pipeline-buffer bytes, six queued artifacts, and 517 total pipeline artifacts including the writer batch. SQLite size was 18,722,816 bytes; WAL and SHM were both 0 bytes after the benchmark checkpoint.

The regression ceiling is twice the earlier 28,917,760-byte RSS observation: 57,835,520 bytes. Peak benchmark-process RSS in the selected run was 32,976,896 bytes. Across the 20 open/index/update/search/status/close cycles, sampled RSS was 31,911,936 bytes at both the first and last cycle. Allocator live heap and mmap pages are unavailable in this build; stable RSS does not establish zero leaks. SQLite PRAGMA integrity_check returned ok.

Cancellation tests covered discovery, a 256 KiB file read cancelled after its first 64 KiB chunk, parsing, queue backpressure, and SQLite batch writes. Each cancelled run stayed incomplete, the repository was not reported complete, and a later run recovered. The 20-cycle benchmark also confirmed a cancelled index remained partial. Existing migration, scope isolation, immediate-forget, and worktree isolation tests passed.

## Environment and remaining verification

- Current run: Linux 6.18.57, x86_64, AMD EPYC 9V74 (80-core host); affinity exposes CPUs 0–2, cgroup CPU quota is 2 cores. Host RAM was 10.2 GB (9.7 GiB) with an 8 GiB cgroup memory limit.
- Rust 1.99.1, Cargo 1.99.1, release build; rusqlite 0.41.1 with bundled SQLite 3.54.1; Tree-sitter 0.27.1 with Rust grammar 0.24.2, Python 0.25.0, and TypeScript/TSX 0.23.2.
- Fixture seed 20261103; exactly 10,000 eligible files / 104,857,600 bytes; no skips.
- The benchmark does not evict OS file cache. Cache state is unknown; do not interpret fresh databases as cold-disk samples.
- Node.js 24.22.1 MCP example passed end to end. Go is unverified because the installed go command rejects both version and run; Bun was unavailable. Windows, ARM, sanitizers, allocator profiling, and mmap profiling were not verified. Tantivy and Zoekt were not evaluated.
- The previous documentation's macOS 26.7.1 result is a historical pre-pipeline measurement only. The current pipeline still needs a 30-sample run on that documented Mac before the formal full-index gate can be called passed.

The dominant measured work on this Linux run was the serialized write path (index-writing p50 851 ms, with FTS execution p50 320 ms and metadata writes p50 448 ms) plus coordinator wait/backpressure p50 529 ms. Aggregate parse work was 1,170 ms p50 across three workers. The next decision is to run the selected build on the documented Mac; if that run exceeds 3 seconds at p95, optimize the single-writer metadata/FTS path and remeasure without changing the gate or fixture.
