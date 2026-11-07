# History Tracker

This file records unreleased development progress. Entries use `YYYY-MM-DD (n) - short description` and are ordered newest first. The number in parentheses is the modification's ordinal within that date; for six modifications on one date, entries are numbered `(6)` through `(1)` from top to bottom. This is a development history, not a release record.

* 2026-11-05 (10) - Restored the original exact recall scan default after interleaved tied/Zipf diagnosis; retained descending only for benchmark comparison, added per-call CPU/cache diagnostics and oracle/plan/mutation regressions. The historical p99 spike was not reproducible as a plan-specific failure.

* 2026-11-05 (9) - Reduced exact single-token recall sorting work with a descending FTS coroutine, preserving exhaustive oracle and frozen-suite results. Added tie/cutoff and filtered-winner regression coverage; native paired evidence and remaining v0.5 gates are in `docs/retrieval-v05.md`.
* 2026-11-05 (8) - Documented the v0.4 retrieval record (`docs/retrieval-v04.md`): verified FTS5 `bm25()`/rank-ordering internals in the bundled source, the decision record, seven rejected or reverted experiments, paired v0.3.1 measurements, and the pending native macOS gate.
* 2026-11-05 (7) - Added schema 4 per-namespace active-memory counters (atomic migration, trigger-maintained), sorted retrieval stages inside FTS5 columns with a newest-first exact-tie rule, dropped the namespace posting-list walk for majority namespaces, and capped per-class staging at four terms; 100k one-namespace p50 fell from 77.7 to 59.7 ms on the broad-term workload with identical frozen-suite metrics.
* 2026-11-05 (6) - Replaced the single BM25 window with class-staged exact top-k recall, added an exhaustive BM25 oracle validated against FTS5 rank, new `exact_top_k` and `recency_window` coverage values, an opt-in `broad_terms=recent` shortcut (rejected as default after it dropped relevant dev evidence), and the `bench-retrieval` paired A/B command.
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

## 2026-11-06 — Dots integration

- Added `serve-http` (loopback MCP Streamable HTTP) and the shared permission `Policy` (read-only, tool allowlist, memory scopes, repo roots) for stdio and HTTP. Docs: `docs/dots-integration.md`. Tests: `tests/mcp_http.rs` (restart, scope denial, read-only, Host rejection, loopback-only bind, two-process concurrent writes).
- Evaluated and rejected a warm `MemorySession` pool: no measurable recall change.

## 2026-11-06 — Authenticated HTTP MCP sessions

- `serve-http` now requires a bearer token on every request. `token create|list|revoke` manage `<db>.tokens.json` (SHA-256 only, 0600, flock + atomic replace); each token holds its own tool/memory-scope/repo-root policy, may expire, and is re-checked per request so revocation is immediate and survives restarts. Memory tools need `--memory-scope` or explicit `--all-scopes`; code tools need `--repo-root`. A token pinned to one literal scope has it injected into calls and any other scope is denied. `/mcp/<label>` is accepted as an alias (label carries no authority). Stdio `mcp` is unchanged and keeps its flags.
- Tests: `tests/mcp_http.rs` (401 matrix, revocation, expiry, no plaintext at rest/echo, cross-Dot forged scope/omission/forget, token-creation guards) and `src/auth.rs` unit tests. Retrieval code and frozen suites untouched.

## 2026-11-06 — HTTP accept latency

- Root cause of the CLI/HTTP recall gap: the non-blocking accept loop slept a fixed 15 ms between polls, adding 0–15 ms to every new connection (bimodal 1/8/15 ms in curl). The loop now `poll(2)`s the listener (100 ms cancellation interval, `libc` already a dependency). Retrieval and authorization code unchanged. Recall over HTTP p50/p95 5.4/7.3 → 1.6/2.1 ms (paired, 200 iterations, 2,000 memories).

## 2026-11-06 — Final stabilization validation (no code change)

- Re-validated against Dots at the final integration state: `cargo fmt --check`, `clippy -D warnings`, `cargo test` (48 unit + 7 HTTP + 4 stdio, retrieval oracle/fixtures included) and the real-binary offline Dots E2E all pass. Retrieval code is untouched since v0.4.
- Measured (2,000 memories, mock model): HTTP recall p50/p95/p99 2.3/3.5/4.9 ms; startup 3.6 ms; RSS 9.1 MiB; idle CPU ≈ 0%. Matches the post-accept-poll baseline (2.2/3.0/7.2).
