# History Tracker

Development history of unreleased work, not a release record (releases go in `CHANGELOG.md`).

## 2026-11-07 (6) — v0.1.0 installer pin and release handoff

- `scripts/install.sh` is now pinned to `VERSION=0.1.0`: it accepts only `aether-midnight-0.1.0-macos-x86_64` archives (URL name, single top-level directory, installed `--version`), rejects link members, and removes its staged file on interruption. `scripts/package-release.sh` derives the archive name from `Cargo.toml` (the `-rc1` suffix is gone). Release-pin values stay empty until a release is published.
- `docs/release-checklist.md` now holds the manual prerelease publication and clean-Intel-Mac test procedure; the public-download gate is PENDING. ShellCheck was not available on the host and is still to be run. No code, retrieval or performance change.

## 2026-11-07 (5) — Script-first installation

- v0.1 is now distributed as a Rust CLI via `scripts/install.sh` (macOS x86_64, user-local, no sudo) instead of a signed app/PKG. The script requires an explicit archive SHA-256, verifies the bundled `SHA256SUMS`, rejects unsafe archive paths, installs atomically and idempotently, runs the pinned ONNX Runtime install and `doctor`, and never bypasses Gatekeeper or touches quarantine. Developer ID signing work is dropped from the v0.1 plan.
- Docs: `docs/installation.md` (script, Gatekeeper, source-build fallback), `docs/release-signing.md`, `docs/release-checklist.md`. The release pin in the script is empty until an archive is published. Clean-machine and quarantined-download behavior are not yet tested; no code, retrieval or performance change.

## 2026-11-07 (4) — v0.1 release candidate verification

- Corrected cloud response compatibility for omitted optional fields and classified HTTP 402 by status. Added ordering, redirect/privacy and recovered partial-index regression tests; local inference, retrieval, persistence and MCP/HTTP behavior are preserved.
- Prepared a reproducible local macOS x86_64 archive with checksums, instructions, examples and dependency notices. Clean-directory installation and real offline hybrid search passed. Gatekeeper rejects the unsigned CLI; the upstream runtime signature verifies. Public binary distribution remains NO-GO pending signing/notarization and hardened dynamic loading checks.
- Commands, sanitized measurements, limitations and release gates: `docs/v0.1-status.md`, `docs/release-checklist.md`, `docs/release-signing.md` and `docs/performance.md`. Raw evidence remains ignored and local; nothing is committed or published.

## 2026-11-07 (3) — v0.1 Phase 3 release hardening and cloud embeddings

- ONNX Runtime: pinned official 1.23.2 for macOS x86_64 with `runtime install|status` (explicit download, size + SHA-256 verified before extraction, named members only). `doctor` now reports runtime, model and credentials with remedies (optional checks never fail it). Model pinned to Hugging Face commit `614241f6…` with per-file SHA-256, verified at pull, `models verify`, `doctor` and load. Licence inventory gains ONNX Runtime, model weights and remote services (375 crates).
- Cloud embeddings over `rainy-sdk` 0.6.51 for both OpenRouter and Rainy-compatible endpoints: explicit `--provider/--model/--dimensions/--endpoint/--timeout-secs/--param`, env-only credentials, bounded redacted errors, response validation, no retry, space isolation by provider/model/endpoint+params/dimensions, queries never embedded in a mismatching space, failures degrade to lexical. Embedding stays explicit; MCP/HTTP unchanged. MSRV is now 1.98.
- Evidence: 120 tests pass (mock-server suites in `src/cloud/tests.rs`, `tests/cli_cloud.rs`); live-provider smoke skipped (no credentials). Exact KNN: +20 ms at 10k, +240 ms at 100k vectors, flat RSS (`docs/performance.md`). Lexical code untouched; frozen suites reproduce. Details, risks and platform scope: `docs/v0.1-status.md`, `docs/embeddings.md`.

## 2026-11-07 (2) — v0.1 Phase 2 local embeddings and hybrid retrieval

- Added `multilingual-e5-small` embeddings (FastEmbed 5.13, runtime-loaded ONNX Runtime), schema 6 `embeddings` table with cleanup triggers, exact KNN, deterministic RRF hybrid search, and CLI `models list|pull`, `embed`, `--mode lexical|semantic|hybrid` on `memory search` and `search`. Lexical code, defaults and JSON are unchanged; missing models degrade to lexical with an explicit notice.
- Closed Phase 1 blockers: real SIGINT test, migration tests against databases produced by real earlier builds, release build smoke. Frozen quality suites identical to Phase 1; paired index timing unchanged. Results, risks (ONNX Runtime packaging) and docs: `docs/v0.1-status.md`, `docs/embeddings.md`.

## 2026-11-07 (1) — v0.1 Phase 1 CLI

- Added `init`, `memory add|get|list|delete|search`, `doctor`, a global `--json` flag, a positional `search` query and `memory::get_memory`. Existing commands, MCP and retrieval code are untouched.
- Tests: `tests/cli_smoke.rs`; status in `docs/v0.1-status.md`.

## 2026-11-06 (4) — Final stabilization validation (no code change)

- Re-validated against Dots at the final integration state: `cargo fmt --check`, `clippy -D warnings`, `cargo test` (48 unit + 7 HTTP + 4 stdio, retrieval oracle/fixtures included) and the real-binary offline Dots E2E all pass. Retrieval code is untouched since v0.4.
- Measured (2,000 memories, mock model): HTTP recall p50/p95/p99 2.3/3.5/4.9 ms; startup 3.6 ms; RSS 9.1 MiB; idle CPU ≈ 0%. Matches the post-accept-poll baseline (2.2/3.0/7.2).

## 2026-11-06 (3) — HTTP accept latency

- Root cause of the CLI/HTTP recall gap: the non-blocking accept loop slept a fixed 15 ms between polls, adding 0–15 ms to every new connection (bimodal 1/8/15 ms in curl). The loop now `poll(2)`s the listener (100 ms cancellation interval, `libc` already a dependency). Retrieval and authorization code unchanged.
- Recall over HTTP p50/p95 5.4/7.3 → 1.6/2.1 ms (paired, 200 iterations, 2,000 memories).

## 2026-11-06 (2) — Authenticated HTTP MCP sessions

- `serve-http` now requires a bearer token on every request. `token create|list|revoke` manage `<db>.tokens.json` (SHA-256 only, 0600, flock + atomic replace); each token holds its own tool/memory-scope/repo-root policy, may expire, and is re-checked per request so revocation is immediate and survives restarts. Memory tools need `--memory-scope` or explicit `--all-scopes`; code tools need `--repo-root`. A token pinned to one literal scope has it injected into calls and any other scope is denied. `/mcp/<label>` is accepted as an alias (label carries no authority). Stdio `mcp` is unchanged and keeps its flags.
- Tests: `tests/mcp_http.rs` (401 matrix, revocation, expiry, no plaintext at rest/echo, cross-Dot forged scope/omission/forget, token-creation guards) and `src/auth.rs` unit tests. Retrieval code and frozen suites untouched.

## 2026-11-06 (1) — Dots integration

- Added `serve-http` (loopback MCP Streamable HTTP) and the shared permission `Policy` (read-only, tool allowlist, memory scopes, repo roots) for stdio and HTTP. Docs: `docs/dots-integration.md`. Tests: `tests/mcp_http.rs` (restart, scope denial, read-only, Host rejection, loopback-only bind, two-process concurrent writes).
- Evaluated and rejected a warm `MemorySession` pool: no measurable recall change.

## 2026-11-05 (10) — Restore exact recall scan default

- Restored the original exact recall scan default after interleaved tied/Zipf diagnosis; retained descending only for benchmark comparison, added per-call CPU/cache diagnostics and oracle/plan/mutation regressions. The historical p99 spike was not reproducible as a plan-specific failure.

## 2026-11-05 (9) — Descending FTS recall scan

- Reduced exact single-token recall sorting work with a descending FTS coroutine, preserving exhaustive oracle and frozen-suite results. Added tie/cutoff and filtered-winner regression coverage; native paired evidence and remaining v0.5 gates are in `docs/retrieval-v05.md`.

## 2026-11-05 (8) — v0.4 retrieval record

- Documented the v0.4 retrieval record (`docs/retrieval-v04.md`): verified FTS5 `bm25()`/rank-ordering internals in the bundled source, the decision record, seven rejected or reverted experiments, paired v0.3.1 measurements, and the pending native macOS gate.

## 2026-11-05 (7) — Schema 4 namespace counters and sorted staging

- Added schema 4 per-namespace active-memory counters (atomic migration, trigger-maintained), sorted retrieval stages inside FTS5 columns with a newest-first exact-tie rule, dropped the namespace posting-list walk for majority namespaces, and capped per-class staging at four terms; 100k one-namespace p50 fell from 77.7 to 59.7 ms on the broad-term workload with identical frozen-suite metrics.

## 2026-11-05 (6) — Class-staged exact top-k recall

- Replaced the single BM25 window with class-staged exact top-k recall, added an exhaustive BM25 oracle validated against FTS5 rank, new `exact_top_k` and `recency_window` coverage values, an opt-in `broad_terms=recent` shortcut (rejected as default after it dropped relevant dev evidence), and the `bench-retrieval` paired A/B command.

## 2026-11-05 (5) — Benchmark report redaction

- Redacted host, runtime, local-path, and account-cost identifiers from published benchmark reports while retaining measured result distributions.

## 2026-11-05 (4) — BM25 candidate order and coverage

- Reproduced middle-window evidence loss, made BM25 ranking the production candidate order, exposed bounded candidate coverage, and added positional, insertion-order, and MCP regressions.

## 2026-11-05 (3) — Recall phase profiling

- Added non-stripped recall phase profiling, SQLite statement and scan counters, and an alternating page-cache screen; measured FTS execution as the 100k broad-query bottleneck.

## 2026-11-05 (2) — Two-window one-term retrieval

- Added bounded two-window candidate retrieval for one-term queries; preserved existing dev and held-out retrieval metrics and reduced repeated 100k persistent recall p95 to 7.32–7.53 ms.

## 2026-11-05 (1) — WAL checkpoint outliers

- Correlated most >4 ms remember outliers with WAL checkpoint progress; tested 256/1000/4096-page thresholds and retained SQLite's 1000-page default.

## 2026-11-04 (8) — Direct-file MCP refresh measurement

- Measured direct-file MCP refresh at 12.7 ms p95 on a 10k-file fixture versus 379.0 ms for full reconciliation; freshness and final reconciliation passed.

## 2026-11-04 (7) — Persistent memory sessions and batch ingestion

- Added persistent MCP memory sessions, atomic bounded batch ingestion, scoped contentless memory FTS migration, and deterministic lexical reranking.

## 2026-11-04 (6) — Frozen 10k-background retrieval evaluation

- Added frozen 10k-background retrieval evaluation and paired persistent MCP CPU, latency, storage, and quality evidence.

## 2026-11-04 (5) — One-file index refresh

- Added explicit one-file index refresh with containment/security checks and full-reconciliation freshness semantics.

## 2026-11-04 (4) — Reverted three-term threshold

- Reverted a three-term retrieval threshold after held-out R@5 and evidence completeness regressed; retained the rejected-run artifact.

## 2026-11-04 (3) — Memory scaling benchmark

- Added isolated 1k/10k/100k persistent memory scaling benchmark and documented root-cause/query-plan findings.

## 2026-11-04 (2) — Parallel indexing

- Added bounded parallel indexing, cancellation-aware reads, identifier decomposition, and fixed-fixture benchmark evidence.

## 2026-11-04 (1) — Changelog and tracker

- Added release-only changelog and this tracker; v0.1.0 remains unreleased.

## 2026-11-03 (6) — FTS5 tokenizer selection

- Benchmarked SQLite FTS5 tokenizers on a fixed workload, selected `unicode61`, and added the index-format-2 migration.

## 2026-11-03 (5) — Agent memory operations

- Added explicit agent-memory operations with source provenance, scopes, validity periods, supersession links, and forget behavior.

## 2026-11-03 (4) — Stdio MCP interface

- Added the stdio MCP interface and Node.js, Bun, and Go client examples; exercised Node.js and Go integrations.

## 2026-11-03 (3) — Bounded incremental indexing

- Added bounded, incremental repository indexing with ignore rules, default exclusions, symlink containment, cancellation, and partial-index reporting.

## 2026-11-03 (2) — Source-grounded retrieval

- Added source-grounded retrieval for paths, symbols, lexical matches, ranges, repository maps, and index status, including freshness checks.

## 2026-11-03 (1) — Benchmarks, safety checks and docs

- Added reproducible benchmark tooling and results, integration and safety checks, and product, architecture, API, privacy, platform, and dependency documentation.
