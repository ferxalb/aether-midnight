# Performance and memory investigation

## v0.3 high-fan-out recall and write-tail investigation (2026-11-05)

The current follow-up starts from `9d39467b5c5531bb53464b3f1ce4254304ef6077`; the measured implementation is `667cea3ad1cb3c475d5deeaf675f5eb374fcdf09` on `perf/midnight-v03-end-to-end-engine`. The main root cause is now measured: with 25,025 active records in one namespace and 50 bounded candidates, `sqlite_candidate_step` consumed about 26.8 ms p50 in the BM25-ranked query, while row hydration consumed about 0.03 ms and the remaining Rust phases were also small. The FTS query and rank execution dominate this broad one-term case. SQLite scanstatus exposes 50 FTS loop rows and 50 primary-key lookups here; it does not count FTS term postings, so the number of internal postings read remains unknown.

The retained implementation uses two bounded row-ID windows for one-informative-term queries only. On the paired profile run it lowered p95 for the profiled broad query from 29.7 to 8.4 ms; the independent persistent-session scale run measured a 7.43 ms median p95 across three 100-sample runs, versus the 26.51 ms baseline run. The original v0.2 development and held-out query metrics were unchanged. This is approximate candidate selection: it can omit matching records between the windows. The new fixture's English query still misses its Spanish-labeled source because no query token matches; both ranked and balanced paths show that limitation. See the [performance report](performance.md) for complete tables and the committed compact evidence summary.

Two suspected optimizations were rejected. Hydration was under 0.06 ms p99, so a second late-materialization lookup would add work around the measured bottleneck. Increasing the SQLite cache limit from 2 MiB through 16 MiB did not improve query latency; actual page-cache use stayed around 0.8 MiB in that fixture.

The write-tail probe measured 10,027 persistent MCP `remember` calls and sampled `PRAGMA wal_checkpoint(NOOP)` after each response. In the 1,000-page default policy, 23 of 28 calls above 4 ms across three counterbalanced runs coincided with a rise in checkpointed frames. The event group was slower than the no-progress group. This is strong evidence that automatic checkpoint work drives most observed spikes, but commit, checkpoint, filesystem sync, and lock-wait durations were not separately timed. The 256-page threshold increased checkpoint frequency and p95; 4,096 pages raised WAL growth and p99.9. We kept the default and `synchronous=NORMAL` durability setting.

The local release-scale campaign measured persistent broad recall p95 at 1k, 10k, and 100k records, with 100 samples per size. Three repeated 100k runs had p95 between 7.32 and 7.53 ms and p99 between 7.51 and 9.94 ms. One initial 100-sample campaign produced a 40.8 ms p99 that the three repeats did not reproduce; it remains preserved as tail-variability evidence. The 200-query evaluation, macOS-native comparison, late materialization prototype, large-payload memory campaign, multi-process contention test, and explicit transaction/commit timing were not completed in this follow-up.

## Scope and evidence

The investigation was prompted by high CPU use during a repeated indexing benchmark. That observation identified active work; it did not measure process memory or establish a leak.

The historical investigation compared indexing revisions on a fixed local fixture. Host identifiers, local paths, and account details are omitted from the published report.

## Resource findings

On the 10,000-file / 100 MiB fixture, the original six-worker run used about 2.02 CPU cores on average during the roughly 2.64-second index. The 126% Activity Monitor reading is therefore consistent with ordinary parallel indexing and is not evidence of memory growth. At 12 workers, throughput was effectively the same as six workers while CPU time and the measured memory high-water marks increased.

Machine model, operating-system revision, storage, power, and host-wide memory details are withheld. Published resource figures retain only the measured process-level aggregates.

| Resource | Original six-worker run, n=5 | Statement-reuse run, n=5 | Measurement meaning |
| --- | ---: | ---: | --- |
| Peak RSS p50 / p95 | 25.94 / 25.99 MB | 25.89 / 26.25 MB | Direct-child high-water from `getrusage(RUSAGE_CHILDREN).ru_maxrss` and sampled task RSS, reported separately from footprint |
| macOS physical-footprint high-water p50 / p95 | 15.09 / 15.14 MB | 15.04 / 15.15 MB | `proc_pid_rusage(RUSAGE_INFO_V4)` lifetime maximum; not interchangeable with Linux RSS |
| Average CPU use p50 / p95 | 1.92 / 1.92 cores | 2.08 / 2.12 cores | Process CPU seconds divided by measured wall time |
| Current RSS after warm MCP calls | — | 18.7 MB p50 / 20.9 MB p95 | Sampled within the persistent MCP child; its 2-second idle interval added no observed CPU time |

Decimal MB are used in this table. The RSS high-water and physical footprint are separate operating-system accounting values. The Python collector and filesystem cache are outside the direct-child scope. No allocator live-heap profiler was attached.

### Confirmed memory-flow properties

- Bounded discovery feeds a worker pool and a single SQLite writer. Worker count defaults to available parallelism, capped at 12. Job and result channels have capacity `2 × workers` (minimum 2).
- Each source read is capped at 1 MiB. The owned byte vector is consumed by `String::from_utf8`, so that conversion does not duplicate the source buffer.
- A parser cache is retained by each worker, with at most one parser per supported grammar (Rust, Python, TypeScript, and TSX). Parse trees are local to extraction; the long-lived parser state is bounded by worker and grammar counts.
- The in-flight reservation is three times the source file size, capped at three times the per-file limit. Completed out-of-order results wait in a sequence-keyed map and remain inside the bounded outstanding-item accounting.
- The writer batches at most 512 changed files or 16 MiB of source text by default. Its reported byte counter includes pending source text, identifier terms, and a fixed 128-byte estimate per symbol. It does not include all parser working memory, allocator metadata, SQLite internals, or OS page cache, so it is telemetry rather than an RSS limit.
- SQLite uses WAL mode, normal synchronous writes, and file-backed temporary storage. The 10k fixture's final database plus sidecars was 18,722,816 bytes. The observed WAL high-water varied by sample; the main database and WAL are sampled separately in the raw artifacts.
- The measured six-worker writer phase fell from 1,720 ms to 1,338 ms after prepared-statement reuse. Metadata statement work fell from 1,058 ms to 780 ms, and symbol statement work from 103 ms to 31 ms. FTS execution, parsing, and reads remained material costs.

### Root-cause assessment

| Suspected cause | Finding |
| --- | --- |
| Excessive application heap or a leak | Not supported by this reproduction. Fresh-process full-index samples had a narrow high-water band; persistent MCP samples had no observed CPU during idle. A longer same-process soak with allocator instrumentation remains unrun. |
| Transient parse / writer pressure | Bounded but not fully represented by the byte reservation. macOS physical-footprint high-water stayed near 15 MB for the measured fixture. Large/deep/parser-failure stress fixtures remain pending. |
| Parser state retained by workers | Bounded by worker count and supported grammar count. Per-parser retained allocation is not separately measured. |
| Duplicated text buffers | Source bytes become an owned `String` without a conversion copy. The writer still clones text to assemble FTS payloads; the timer is about 9 ms for this fixture. Removing it is a small possible saving, not a leading measured bottleneck, and was left unchanged. |
| SQLite cache, WAL, or temporary files | WAL grew during indexing and is visible in watched sidecar samples. No runaway sidecar growth was observed; the final summed database size was stable. SQLite cache bytes are not separately exposed by this build. |
| OS file cache | The source tree and temporary database are on local filesystem details withheld. OS caches were not flushed, so later samples can benefit from filesystem cache. Host-wide `vm_stat`, swap, and memory-pressure snapshots are recorded, but their changes cannot be attributed solely to Midnight. |
| Benchmark harness / subprocess overhead | The legacy runners included Linux-only assumptions and fixed high defaults. The new campaign excludes the Python wrapper from process memory and CPU, separates fresh CLI startup from persistent MCP query latency, and records child scope explicitly. |
| Unexpected parallelism | Worker count is set per run. Six workers produced the best measured throughput plateau on this Mac; 12 workers used more CPU for no useful throughput increase. |
| Measurement defect | Historical Linux `getrusage` peak values were below later sampled RSS and are therefore invalid as peak RSS. The new collector reports sampled RSS, lifetime counters, physical footprint, and cgroup data as distinct fields. |

These findings do not establish behavior on Linux Kernel or VS Code today. The retained repository results report complete file catalogs but incomplete structural indexes (7 and 54 parse-error files, respectively). Those are historical Linux observations, not claims about the new revision.

## Sampling method and limitations

`scripts/measure_process.py` samples one direct child every 50 ms. On macOS it uses `proc_pidinfo(PROC_PIDTASKINFO)` for current resident size, CPU/fault/thread/context-switch counters and `proc_pid_rusage(RUSAGE_INFO_V4)` for physical footprint, lifetime maximum footprint, page-ins, and I/O counters. It also captures host-wide `vm_stat`, swap usage, `memory_pressure`, power, and thermal snapshots before and after each measured child. `/usr/bin/time -l` or Instruments can be used for independent confirmation; `vmmap` is intentionally not spawned repeatedly in the timed path.

On Linux it reads `/proc/<pid>/status`, `/proc/<pid>/stat`, `/proc/<pid>/io`, and periodically `/proc/<pid>/smaps_rollup` for PSS, private, anonymous, file-backed, and swap values. When exposed, it also samples cgroup v2 memory, CPU, I/O, pressure, and event counters. cgroup values cover the sandbox/container and may include the wrapper or other processes, so they are not represented as per-process values.

RSS and physical footprint high-water counters are authoritative OS counters where available; sampled maxima can miss brief peaks. The sample arrays preserve sampled values. The collectors do not aggregate descendants; the measured CLI/MCP commands do not intentionally spawn helper processes. Allocator live bytes, full VM-region classification, and hardware disk write amplification are not measured.

## Artifacts

The machine-readable manifests and raw sample arrays are preserved in the local ignored run directories. For this run, see:

- `benchmark/runs/20261105T002945Z_aedadd122ac4_standard_cc6f7fbc/manifest.json` — pre-change worker-count screen.
- `benchmark/runs/20261105T003600Z_aedadd122ac4_standard_75668a7b/manifest.json` — five-sample before/after trial at six workers, incremental visibility, persistent MCP, and retrieval checks.
- Generated benchmark run artifacts are local-only and are not committed.

These raw generated artifacts are excluded from Git to avoid committing generated benchmark data. Do not replace historical `benchmark/*.json` evidence with local measurements.

## v0.2 memory and targeted-refresh follow-up (2026-11-04)

This follow-up started from the current `origin/main` snapshot `01b98c76ff1241d8e4a8459cf9e42445c8e72d16`, not the earlier investigation's initial `aedadd1` snapshot. The previously observed `09a30f4` statement-reuse and `b0371b3` documentation commits were already present in the base history. Work is on `codex/v0.2-memory-engine-optimization-20261104`; no prior local changes were reset or overwritten. The engine implementation is `9c358e2`; the branch HEAD `2ab6393` reverts an evaluated but rejected threshold experiment.

The persistent MCP evidence runs 10,027 frozen fictional memories, including 10,000 irrelevant records, through 10,195 JSON-RPC requests on one process. The paired release artifacts are the `memory-retrieval-large-v2-*` files linked from the [performance report](performance.md). Process CPU was 17.485 s at baseline and 2.731 s in v0.2; warm recall p95 changed 17.953→17.363 ms, individual remember p95 2.611→0.335 ms, and forget p95 2.296→0.118 ms. These same-host results are more useful than internal-only SQL timings, but they are one campaign rather than a cross-machine guarantee.

### Measured root causes and implementation

- The memory CLI path opened/configured/migrated SQLite separately for each operation. The MCP path now lazily owns one `MemorySession` connection and uses rusqlite's prepared-statement cache. MCP dispatch remains serial; there is no pool or background worker. CPU per request in this workload fell from 1.715 to 0.268 ms. A 10k memory-only scale run measured persistent store p95 0.257 ms, recall p95 3.360 ms, list p95 0.056 ms, and delete p95 0.113 ms; the paired 10k-background same-scope retrieval workload is slower because it intentionally produces many FTS candidates.
- The previous memory query sorted by `bm25()`, which required a temporary B-tree in `EXPLAIN QUERY PLAN`. The current SQL orders by FTS5 `rank` with the weighted rank mapping; the 10k plan uses `SCAN memory_fts VIRTUAL TABLE INDEX 32:rM2` and primary-key lookup, with no temporary sort. The 100k broad-query p95 is still 30.290 ms, so candidate fan-out/re-ranking remains the highest measured memory bottleneck.
- Schema 3 rebuilds active postings into `contentless_delete=1` FTS with a hashed namespace token. In the same 10,027-row campaign, the one-snapshot database/WAL/SHM total after close was 6,811,648 bytes at baseline and 5,369,856 bytes in v0.2. The before-close v0.2 total was 10,482,616 bytes, so WAL lifecycle matters; these are snapshots, not repeated storage distributions. The migration is immediate-transaction scoped and existing memory rows, provenance, temporal fields, and supersession links are preserved.
- Recall now accent-folds Unicode, removes common English/Spanish stopwords, safely quotes query terms, guards empty/oversized queries, scopes FTS by token plus exact SQL namespace, filters validity/deletion in SQL, bounds candidates, and deterministically reranks contiguous phrases and distinct-term coverage before FTS score and stable timestamp/id ties. The frozen suite shows higher all-query R@1 (.475→.550), MRR (.823→.892), and nDCG@10 (.846→.896); R@5, P@5, temporal correctness, and evidence completeness are unchanged. False-positive negative queries fell 3/5→1/5, while namespace leakage and invalid temporal results remained zero. This is lexical retrieval only.
- A three-distinct-term minimum was evaluated and rejected: held-out R@5 fell .1.000→.875, evidence completeness .1.000→.917, MRR .906→.875, and nDCG@10 .900→.860. The trial's raw artifact is preserved; the final engine uses a two-term minimum (or the only available term for one-term queries).
- The explicit `refresh-file` CLI / `refresh_indexed_file` MCP path processes one named file without walking the repository and keeps all current containment/exclusion rules. It marks the catalog partial until the unchanged full scan reconciles other changes. This makes the fast path opt-in and recoverable without relying on filesystem events. On the synthetic 10k-file fixture, 30 MCP update-plus-search samples measured full reconciliation p95 379.019 ms and direct refresh p95 12.689 ms; every new result was fresh and a final full scan restored complete status. See the [paired visibility artifact](../benchmark/evidence/refresh-file-10k-2ab6393-20261104.json). Validation on large user repositories and simultaneous background indexing remains unmeasured.
- `remember_batch` is limited to 512 records/16 MiB, validates the batch before a single transaction, and rolls back on record/provenance failure. The 100k synthetic setup used atomic batches and measured 24,013 records/s in one run; setup throughput is anecdotal, not a percentile or crash-recovery campaign.

The separate 1k/10k/100k `bench-memory` artifact reports 100k DB bytes 25,559,040 after the measured-operation checkpoint and cumulative process peak 45,019,136 bytes across all three sizes. It achieved 50/50 labeled unique-token lookups and 0/150 cross-namespace leaks. The broad 100k recall query returned 30.290 ms p95 (n=100) and misses the proposed 15 ms target. That artificial exact-token set does not validate paraphrase or answer-generation quality. At 10k, `bench-memory` persistent recall p95 was 3.360 ms; the broader 10k-background MCP fixture p95 was 17.363 ms, demonstrating the sensitivity to candidate distribution.

`/usr/bin/sample` captured a 5-second live MCP recall profile at a 1 ms sampling interval while one namespace held 10,000 records; the process completed 157 broad recall calls during sampling. The report contained 3,892 stack samples and showed a 12.4 MiB physical footprint. Frames remained inside the repeated request/retrieval call chain, but the release binary is stripped (`Cargo.toml` sets `strip=true`), so native function names and a trustworthy SQL-vs-rerank CPU split were unavailable. The raw local stack sample is intentionally not committed and contains no source corpus. This is a sampled CPU snapshot, not an allocation profile or a statistical latency benchmark.

Account activity, cost, and billing metadata are not included in this public report. The retrieval fixtures are synthetic and locally generated.

## v0.3.1 memory-candidate retrieval follow-up

The earlier single-informative-term fast path sampled up to 25 matching rows from each end of row-ID order. Its 50-row budget bounded the reranker but could exclude a relevant record in the middle. The positional regression failed at row 100 of 1,000 before the default changed.

The selected production strategy uses FTS5 BM25 rank order for one-term and multi-term queries, then reranks the bounded candidate set by exact phrase, informative-term coverage, lexical rank, temporal evidence, and stable provenance fields. The balanced sample remains available only to retrieval experiments and is labeled `row_id_sample`. `candidate_coverage` reports `complete` when the scan ends before the candidate cap or `bm25_top_k` when the ranked cap fills. It does not change index freshness or temporal-validity flags.

For seven frozen positional queries, balanced windows found 2/7 targets at the 0% and 100% endpoints (Recall@1/5/10 .286). BM25 found all seven exact-phrase targets at rank 1 in forward and reverse insertion order (Recall@1/5/10 1.0). Forward/reverse top-10 Jaccard remained low (.053 after the change) because tied weak fillers vary; the labeled target stayed stable. The existing v0.2 and single-term suites showed no aggregate retrieval, temporal, deletion, or namespace regression.

The candidate window still bounds hydration and Rust reranking, not FTS5 posting traversal. On the bounded local 100k run, FTS candidate stepping accounted for 112.924 / 115.865 / 124.542 ms p50/p95/p99, while total recall was 113.436 / 116.452 / 125.139 ms. The 50 candidates were hydrated in .060 ms p95 and final sorting took .008 ms p95. This made BM25 materially slower than balanced windows, whose p95 was 12.471 ms in the same profile. A separate native release run measured p95 30.285 ms across four namespaces and 102.887 ms in a single large namespace. These are workload observations with host identifiers withheld; they do not establish cross-machine performance.

A SQL secondary tie sort was tested and rejected because it introduced broad-match sorting, a temporary B-tree, and over two million SQLite VM steps. Staged expansion was also rejected because a stopping point without a validated evidence threshold cannot prove that unseen candidates are weaker. The chosen path surfaces bounded coverage and preserves BM25 relevance order without introducing another index or schema migration.

The result is suitable as the default lexical candidate strategy when callers retain the `bm25_top_k` approximation signal. It does not promise semantic completeness; relevance below the BM25 cutoff may remain unseen. The 100k latency goal remains unmet for both tested layouts. See [the performance report](performance.md) and host-redacted [machine-readable evidence](../benchmark/evidence/memory-retrieval-v031-20261105.json) for metrics and reproduction commands.
