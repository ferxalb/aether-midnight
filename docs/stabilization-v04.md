# v0.4 native Intel macOS stabilization — 2026-11-05

**Decision: NO-GO against the documented native latency targets.** Correctness and build gates pass; exact high-frequency recall still misses the 10k and 100k latency targets. No retrieval contract changed. No paid cloud compute was used.

Implementation commit: `9205846` (`fix: release index locks explicitly and validate recovery paths`).

Work stayed on `main` at the explicit request of the operator, preserving existing commits. The comparison binary was built from v0.3.1 baseline `bdc940f` in an ignored archive directory on the same native Intel macOS host. Raw artifacts remain ignored; this report contains only workload definitions and aggregate measurements.

## Fix and correctness evidence

- The index-operation lock used file close as its release mechanism. A duplicate descriptor (equivalent to the shared open-file description inherited during concurrent fork/exec) retained the advisory lock after the operation ended. A deterministic test reproduced the reported “another index operation holds the lock” error before the fix. `IndexLock::drop` now explicitly unlocks when the protected operation ends. Contending operations still fail while indexing is active; SQLite transactions, timeout, and durability policy are unchanged. This was an advisory lock failure before SQLite, not a SQLite busy-timeout failure.
- The deterministic descriptor test passes, and the original explicit-refresh test passed 50 consecutive runs. There are no remaining reproducible concurrency failures in these checks.
- Disposable copies of legacy v2/v3 fixture databases migrate with originals unchanged. Checks cover history, provenance links, soft deletion, namespace counters and integrity. A late injected v3 migration error rolls back table/backfill/version changes; retry succeeds. An actual database created by the v0.3.1 binary also migrated from v3 to v4 with identical memory rows and correct counters.
- Killing a process with uncommitted soft deletion, FTS/counter-trigger changes and DDL preserves the committed memory, provenance, counts, recall result, and `PRAGMA integrity_check=ok` after reopening. This tests process termination, not power-loss durability. No user production database was supplied; historical v2 coverage uses the maintained fixture.
- All four frozen memory suites (v1, v2, single-term, adversarial), 83 queries total, have identical before/after top-10 lists, per-query first-relevant ranks and non-latency quality metrics. Temporal-invalid and namespace-leak counts remain zero. Exhaustive oracle and adversarial retrieval tests pass. Class-staged exact top-k for up to four informative terms and the existing coverage labels are unchanged.
- `cargo test --locked`: 43 unit tests and 4 integration tests passed; one query-plan diagnostic remains intentionally ignored. Formatting, all-target/all-feature clippy with warnings denied, and locked release build pass.

## Native retrieval A/B

`bench-retrieval`, deterministic Zipf corpus, 100 warm samples per strategy/query, result limit 10. Strategies are interleaved in one process and database. This compares the retained v0.3.1 ranking strategy with staged ranking; the separate binary comparison follows below. Values are milliseconds. CPU is mean process CPU milliseconds per query; RSS is process peak MiB shared by both strategies. The two small one-namespace runs originally overlapped compilation and were repeated once after campaign jobs stopped; the repeat is used here. OS cache was warm; no cold-cache or exclusive-host claim is made.

| Records / namespaces | Ubiquitous term v0.3.1 p50/p95/p99 | v0.4 p50/p95/p99 | CPU old → new | Peak RSS |
|---|---|---|---|---|
| 1,000 / 1 | 1.54/1.62/1.77 | 0.75/0.77/1.08 | 1.54 → 0.75 | 13.8 |
| 10,000 / 1 | 10.12/10.55/10.81 | 5.01/5.19/5.78 | 10.15 → 5.03 | 21.9 |
| 100,000 / 1 | 116.85/145.20/180.60 | 52.98/59.14/73.28 | 117.03 → 53.87 | 44.4 |
| 1,000 / 4 | 0.97/1.13/1.39 | 0.57/0.65/0.84 | 0.99 → 0.58 | 16.5 |
| 10,000 / 4 | 3.84/4.42/4.80 | 2.30/2.69/2.92 | 3.88 → 2.33 | 19.2 |
| 100,000 / 4 | 34.53/35.93/47.91 | 20.26/21.05/21.59 | 34.42 → 20.15 | 30.8 |

Every class with a slower measured p50 is reported below (sampling noise is not a demonstrated algorithmic regression):

| Records / namespaces | Class | v0.3.1 p50/p95/p99 | v0.4 p50/p95/p99 |
|---|---|---|---|
| 1,000 / 1 | planted_contiguous_phrase | 0.97/1.08/1.20 | 1.28/1.36/1.45 |
| 1,000 / 1 | natural_language_with_stop_words | 0.92/0.98/1.07 | 1.68/1.79/2.43 |
| 1,000 / 4 | planted_contiguous_phrase | 0.96/1.08/1.37 | 1.60/1.74/1.83 |
| 1,000 / 4 | natural_language_with_stop_words | 0.90/1.38/2.05 | 1.98/2.65/4.20 |

High-frequency investigation: exact BM25 still evaluates all matching rows inside FTS5. The class staging, scope-term omission and sparse probe already avoid unnecessary work where valid. Recency truncation cannot safely replace exact ranking: document length and term frequency still affect order even at the IDF floor, and the frozen single-term suite guards this. No additional cheap, recall-preserving pruning was established; no speculative ranking change was introduced.

## Actual binary comparison

`bench-memory --sizes 1000,10000,100000`, both binaries on the same host, one process per version/layout. Recall below uses the persistent connection and 30 samples. CPU is total process CPU seconds for the whole size workload, including setup and measured writes; peak RSS is cumulative within the process across sizes. Some setup overlapped quality evaluation/compilation, so these are shared-host observations, not isolated CPU limits.

| Records / namespaces | v0.3.1 recall p50/p95/p99 | v0.4 recall p50/p95/p99 | Workload CPU seconds old → new | Peak RSS MiB old → new |
|---|---|---|---|---|
| 1,000 / 1 | 1.35/1.37/1.42 | 0.89/0.98/1.08 | 0.55 → 0.46 | 14.3 → 13.8 |
| 10,000 / 1 | 9.07/9.45/9.79 | 7.10/7.33/7.53 | 2.44 → 2.03 | 22.7 → 17.3 |
| 100,000 / 1 | 104.06/109.62/142.83 | 73.47/77.67/82.07 | 25.93 → 21.50 | 36.6 → 32.2 |
| 1,000 / 4 | 0.71/0.83/1.08 | 0.49/0.55/0.66 | 0.52 → 0.51 | 14.1 → 14.2 |
| 10,000 / 4 | 3.23/3.41/3.51 | 2.61/2.69/2.82 | 1.36 → 1.29 | 18.1 → 18.1 |
| 100,000 / 4 | 29.41/30.54/31.48 | 24.09/24.66/24.82 | 9.94 → 9.28 | 44.7 → 31.6 |

## Persistent MCP integration

Each binary completed 100 cycles in one persistent MCP process: remember → recall → supersede → recall updated evidence → forget → recall empty. Assertions verified IDs, source URI, supersession link and absence after deletion. The committed integration test also checks namespace isolation and database integrity. This simulates the Dots protocol workflow; it is not a run of the actual Dots application.

Across 600 tool calls/version, v0.3.1 p50/p95/p99 = 0.62/1.37/1.51 ms; v0.4 = 0.59/1.35/1.44 ms. Total child CPU = 0.421 → 0.449 seconds (a measured increase). RSS before close = 12.86 → 13.05 MiB. These include JSON-RPC serialization and pipe round trips.

## Remaining gates

Native ubiquitous-term p95 exceeds the documented 10k ≤5 ms target and 100k ≤20 ms (one namespace) / ≤15 ms (four namespaces) targets. The supported exact retrieval contract should not be weakened to clear those targets. Apple Silicon, actual Dots application integration, production-database copies, power-loss recovery and long-duration thermal behavior remain untested. The previously pending native Intel measurement gate is now measured, with performance failures explicitly recorded above.
