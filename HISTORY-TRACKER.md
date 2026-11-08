# History Tracker

Development history of unreleased work, not a release record (releases go in `CHANGELOG.md`).

## 2026-11-08 (2) — Version 0.1.0-rc.1, `update` and `uninstall`

- Version is now `0.1.0-rc.1` (tag `v0.1.0-rc.1`, archive `aether-midnight-0.1.0-rc.1-macos-universal2.tar.gz`); `scripts/install.sh` is pinned to it. The workflow smoke test also checks the `midnight` link, `update --help`, `uninstall --dry-run` and installer idempotency.
- `update` (aliases `upgrade`, `self-update`; `--check`, `--pre`, `--dry-run`, `--to VERSION`): one request for the release list at `api.github.com`, SemVer selection (prereleases only with `--pre` or while running one), https-only GitHub hosts with every redirect hop re-validated, archive SHA-256 against the release `.sha256`, binary against the archive's `SHA256SUMS`, staged binary must report the expected version, then an atomic rename; cargo build outputs are refused. No new dependency (own SemVer comparison, the existing tar reader and TLS agent). Network use is documented in `docs/privacy.md`.
- `uninstall` removes the program and links resolving to it; `--purge-config` / `--purge-data` delete exact files (the config and data directories are the same folder on macOS), `--remove-client-entries` edits user-scope client configs, project `.midnight/` is never touched; `--dry-run`, `--yes`.
- Evidence: unit tests in `src/update.rs`/`src/uninstall.rs` and `tests/cli_update.rs` (real processes against a loopback stand-in for the GitHub API, including tampered checksums, wrong reported version, foreign asset hosts). The real GitHub redirect flow is verified only after the release exists. No retrieval, schema or performance change.

## 2026-11-08 (1) — Installer creates the `midnight` alias

- `scripts/install.sh` now creates `midnight`, a relative symlink to `aether-midnight` beside it (the program already derives its display name from `argv[0]`), so the archive and its checksums are unchanged. It is idempotent and never replaces an existing `midnight` that is not that link; in that case it warns and the final hint uses `aether-midnight`. `--dry-run` lists the alias.
- `connect --scope project` writes the name the user invoked (`midnight` through the symlink) instead of the canonicalized binary name. Evidence: a real-process test through a symlink in `tests/cli_connect.rs`; the installer was exercised against a locally built stand-in archive (fresh install, re-run, foreign file, foreign link) and `sh -n`; ShellCheck is not installed here and remains pending. No Rust retrieval, schema or performance change.

## 2026-11-07 (10) — Consolidation: project trust commands, status, PTY tests, docs

- `project list` / `project untrust [PATH]` manage the trust allow-list; `status` now reports project storage (global or project), trust, whether a leftover `.midnight/` is being ignored, and the effective `midnight.json` values, without creating anything.
- `tests/cli_pty.rs` drives the interactive `setup` and `connect` menus through a real pseudo-terminal (Unix); `tests/cli_connect.rs` covers list/untrust/status. `scripts/install.sh` prints the next step (`setup`) and stays non-interactive. Docs: `docs/api.md`, `docs/privacy.md`, `docs/architecture.md`.
- Decisions: a project database does not see global-database memories (documented; use global storage to share), Claude Desktop entries always use the absolute binary path, project-scope entries rely on `PATH`. `doctor` is unchanged. The installer change was only syntax-checked (`sh -n`); ShellCheck is still pending. No retrieval, schema or performance change.

## 2026-11-07 (9) — Layered project configuration and per-repository setup

- Correction to (8): `.mcp.json`/`.codex/config.toml` written with `--scope project` now use the bare command name and no `--db`, so they are portable; user-scope files keep the absolute binary path. `connect` pins a database only when `--db` is given (a server started inside a project finds `.midnight/` itself). Project-scope backups go to `.midnight/backups/`.
- Correction to (8): the "owned by the current user" check does not stop a database that arrives with a clone (clones are owned by the user). Project storage and `midnight.json` are now honored only for projects registered in `trusted_projects.json` in the config directory, written by `init --storage project`, `setup` and `connect --storage project`.
- New `.midnight/midnight.json` (`src/project.rs`): `embeddings` (false keeps the repo on keyword search for `index`/`search`) and `mcp.read_only` / `mcp.memory_scopes` (narrow the MCP policy; flags still apply). It has no provider, endpoint, key or consent field and rejects unknown fields, so it cannot redirect uploads.
- `setup` is split: user-wide (provider, key, consent) vs. per repository (storage with the project preselected, optional client registration, `midnight.json`), chosen automatically inside a Git repository once user-wide setup exists; `--global`, `--project`, `--storage`. Evidence: `tests/cli_connect.rs`, `src/project.rs`/`src/connect.rs` unit tests. The interactive menus and real client apps are not covered by automated tests. No retrieval, schema or performance change.

## 2026-11-07 (8) — `connect` and project-local storage

- New `connect <claude-code|claude-desktop|codex> --scope project|user [--storage global|project] [--read-only] [--dry-run] [--remove]` registers `aether-midnight` in the client's MCP config (JSON merge, or a line-wise edit of only our table in Codex TOML; invalid input is refused, previous contents are saved as `*.midnight.bak`, writes are atomic). Scope is asked on a terminal and required otherwise.
- `init --storage project` creates `<repo>/.midnight/` (0700), creates or extends `.gitignore` with `.midnight/`, and puts the database there. Without `--db` the CLI now uses the nearest `.midnight/midnight.sqlite3` above the working directory (regular file owned by the current user, else ignored), otherwise the OS data directory as before. `.midnight` joins the always-excluded VCS paths in the walker. Global storage stays the default.
- Evidence: `src/connect.rs` unit tests and `tests/cli_connect.rs` (real process, redirected HOME). Not verified against the real Claude Code, Claude Desktop or Codex apps. No retrieval, schema or performance change.

## 2026-11-07 (7) — `midnight` command and guided first run

- New `midnight` binary (same program as `aether-midnight`, `src/bin/midnight.rs` includes `src/main.rs`). New commands: `setup` (interactive wizard or scripted flags for Rainy, OpenRouter, local, lexical), `status`; `index [PATH]` embeds with the saved provider; `search` uses the saved mode (hybrid with a provider, else lexical). Preferences live in `config.json`; an API key typed into `setup` goes to `credentials.json` (0600, directory 0700), an environment variable always wins and is never copied. Remote uploads need consent recorded by `setup` (bound to provider, destination and model), `--yes` for one run, or a terminal prompt.
- Output: `index`/`search` print readable text on a terminal or with `--human`; piped output and `--json` keep the previous JSON, so existing automation is unchanged. Cloud and lexical flows never load ONNX.
- Evidence: `tests/cli_journey.rs` (8 real-process tests from a clean config directory against the loopback mock). Live Rainy/OpenRouter behavior, including OpenRouter model discovery (`GET {base}/embeddings/models`), is unverified: no credentials were available. No retrieval, ONNX or packaging change.

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
