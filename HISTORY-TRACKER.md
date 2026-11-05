# History Tracker

This file records unreleased development progress. Entries use `YYYY-MM-DD (n) - short description` and are ordered newest first. The number in parentheses is the modification's ordinal within that date; for six modifications on one date, entries are numbered `(6)` through `(1)` from top to bottom. This is a development history, not a release record.

* 2026-11-05 (5) - Redacted host, runtime, local-path, and account-cost identifiers from published benchmark reports while retaining measured result distributions.
* 2026-11-05 (4) - Reproduced middle-window evidence loss, made BM25 ranking the production candidate order, exposed bounded candidate coverage, and added positional, insertion-order, and MCP regressions.
* 2026-11-05 (3) - Added non-stripped recall phase profiling, SQLite statement and scan counters, and an alternating page-cache screen; measured FTS execution as the 100k broad-query bottleneck.
* 2026-11-05 (2) - Added bounded two-window candidate retrieval for one-term queries; preserved existing dev and held-out retrieval metrics and reduced repeated 100k persistent recall p95 to 7.32–7.53 ms.
* 2026-11-05 (1) - Correlated most >4 ms remember outliers with WAL checkpoint progress; tested 256/1000/4096-page thresholds and retained SQLite's 1000-page default.
* 2026-11-04 (8) - Measured direct-file MCP refresh at 12.7 ms p95 on a 10k-file fixture versus 379.0 ms for full reconciliation; freshness and final reconciliation passed.
* 2026-11-04 (7) - Added persistent MCP memory sessions, atomic bounded batch ingestion, scoped contentless memory FTS migration, and deterministic lexical reranking.
* 2026-11-04 (6) - Added frozen 10k-background retrieval evaluation and paired persistent MCP CPU, latency, storage, and quality evidence.
* 2026-11-04 (5) - Added explicit one-file index refresh with containment/security checks and full-reconciliation freshness semantics.
* 2026-11-04 (4) - Reverted a three-term retrieval threshold after held-out R@5 and evidence completeness regressed; retained the rejected-run artifact.
* 2026-11-04 (3) - Added isolated 1k/10k/100k persistent memory scaling benchmark and documented root-cause/query-plan findings.
* 2026-11-04 (2) - Added bounded parallel indexing, cancellation-aware reads, identifier decomposition, and fixed-fixture benchmark evidence.
* 2026-11-04 (1) - Added release-only changelog and this tracker; v0.1.0 remains unreleased.
* 2026-11-03 (6) - Benchmarked SQLite FTS5 tokenizers on a fixed workload, selected `unicode61`, and added the index-format-2 migration.
* 2026-11-03 (5) - Added explicit agent-memory operations with source provenance, scopes, validity periods, supersession links, and forget behavior.
* 2026-11-03 (4) - Added the stdio MCP interface and Node.js, Bun, and Go client examples; exercised Node.js and Go integrations.
* 2026-11-03 (3) - Added bounded, incremental repository indexing with ignore rules, default exclusions, symlink containment, cancellation, and partial-index reporting.
* 2026-11-03 (2) - Added source-grounded retrieval for paths, symbols, lexical matches, ranges, repository maps, and index status, including freshness checks.
* 2026-11-03 (1) - Added reproducible benchmark tooling and results, integration and safety checks, and product, architecture, API, privacy, platform, and dependency documentation.
