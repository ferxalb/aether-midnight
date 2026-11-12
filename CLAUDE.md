# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

AETHER Midnight is a local-first Rust engine (single binary, edition 2024, `rust-version` 1.98) for deterministic code search and explicit agent memory, backed by SQLite. `AGENTS.md` holds the maintainer rules (style, retrieval-change gate, performance-claim rules, benchmark privacy, HISTORY-TRACKER format). Read it before changing retrieval, benchmarks, or docs.

## Commands

```sh
cargo build --locked                       # debug build
cargo build --release --locked             # release binary: target/release/aether-midnight
cargo test --locked                        # all unit + integration tests
cargo test --locked <name_substring>       # one test (matches by name across all targets)
cargo test --locked --test mcp_stdio       # one integration-test file (tests/<name>.rs)
cargo test --locked --test mcp_stdio mcp_stdio_handshake_lists_tools_and_returns_grounded_result
cargo fmt --check                          # formatting (run `cargo fmt` to fix)
cargo clippy --all-targets --all-features -- -D warnings
cargo run -- --help                        # run the CLI from the working tree
```

Feature flags (off by default): `retrieval-experiments` (diagnostic and rejected-experiment code paths), `sqlite-scanstatus` (needs a SQLite build with `SQLITE_ENABLE_STMT_SCANSTATUS`), `allocation-diagnostics` (implies `retrieval-experiments`). Only enable them for the measurement workflows in `docs/performance.md`.

Binaries (`src/bin/`): `midnight` and `aether-midnight` are the same program (`src/main.rs` included by path); `midnight-harness` is the fixture/quality/measurement harness (`generate-fixture`, `memory-quality`, `retrieval-quality`, `measure-mcp`, `license-inventory`); `paired_kernel` is a diagnostic helper.

Tests never call a real embedding provider. Cloud tests use the loopback mock in `tests/common/mock.rs`. Filesystem and database tests must use temporary directories and databases. `AETHER_MIDNIGHT_CONFIG_DIR` redirects the config directory.

## Architecture

The CLI (`src/main.rs`), the stdio and HTTP MCP servers, and the examples all call the same library (`src/lib.rs`) and the same SQLite schema. Behavior must not drift between transports, so put logic in the library, not in an adapter.

Layers, from the outside in:

- **Entry and UX.** `main.rs` parses clap commands and `--json`/`--human`. `ux.rs` owns the first-run `setup` wizard, consent prompts, status reports, and human rendering. Data goes to stdout; prompts and progress go to stderr.
- **Agent interfaces.** `mcp.rs` implements JSON-RPC dispatch (`handle_message`, `run_stdio`, `tools()`, `call_tool`) with one request at a time and bounded line reads. `mcp_http.rs` is a dependency-free loopback HTTP transport (binds only 127.0.0.1/::1, rejects foreign Host/Origin). Both transports go through `Policy` (`mcp.rs`), which restricts tools, memory scopes (prefix `*` allowed), and repo roots. When a policy pins one scope, `authorize` injects it, so model-supplied arguments cannot override it. `auth.rs` is the bearer-token registry for HTTP (SHA-256 digests only, per-token policy, revocation checked on every request).
- **Code retrieval.** `code.rs` (the largest module) owns indexing: Git-aware walk, `.aetherignore`, secret/size/containment checks, a bounded parallel parse pipeline with one SQLite writer, generation-based reconciliation (rows not seen in a complete scan are removed), Tree-sitter extraction for Rust/Python/TypeScript/TSX, search with freshness labels, and `open_range`/`list_symbols`/`repo_map`. `text_index.rs` is the narrow internal boundary over FTS5 `unicode61`.
- **Memory.** `memory.rs` stores explicit memories in namespaces (`prefix:name`, required prefix) with `source` and `derived` kinds, validity windows, supersession, and soft-forget followed by `compact`. Recall uses a separate memory FTS index, Unicode/stopword normalization, and a lexicographic ranking (phrase class, then distinct informative terms, then BM25). `candidate_coverage` in results states whether the answer is exact; any non-exact shortcut must be labeled that way and must not become the default.
- **Semantic search (opt-in).** `embed.rs` holds the pinned local model (`multilingual-e5-small`, pinned revision and per-file SHA-256), persistent vectors, exact KNN, and RRF hybrid fusion. `runtime.rs` manages the pinned, checksummed ONNX Runtime install (ORT is loaded dynamically). `cloud.rs` is the explicit OpenRouter/Rainy embedder built on `rainy-sdk`: nothing is sent unless the provider was selected, credentials come only from environment variables there, and a failed call never falls back to another provider. `config.rs` stores preferences (`config.json`) and the API key file (`credentials.json`, 0600). Lexical paths must never load the model; `ux::embedder_for` is the gate.
- **Storage.** `db.rs` opens SQLite (WAL, `busy_timeout` 5 s, foreign keys on, bundled SQLite with FTS5), creates private files, and runs `migrate` for schema version 9. Migrations are single immediate transactions. `metadata.index_format_version` (2) is separate from the schema version. Memory and code are stored in separate tables and are never mixed in one query.
- **Benchmarks.** `benchmark.rs` and `src/bin/midnight-harness.rs` produce evidence; frozen suites and labels live in `benchmark/` (`suites/`, `retrieval_queries.json`, `evidence/`). Labels are never edited to raise a score.

Invariants that span modules (check all of them when touching one):

- Code queries are always scoped by repository/worktree identity; memory queries always filter the exact namespace. Each Git worktree has its own index.
- Indexing stays bounded: 1 MiB per file, bounded worker and queue counts, a 250 ms Tree-sitter budget per parse, and a budget against in-flight source bytes. Keep these limits when adding fields or stages.
- Repository-relative reads use `openat(O_NOFOLLOW)`-style containment; the walker does not follow symlinks; VCS metadata stays excluded even with `--include-generated`.
- A retrieval change must keep `src/retrieval_tests.rs` (exact oracle agreement) passing and leave every frozen-suite metric unchanged unless it is a documented contract change. Report paired before/after numbers as `AGENTS.md` requires.

## Where things are documented

- `docs/architecture.md`: data flow and SQLite format, with the detailed reasoning behind the invariants above.
- `docs/retrieval-v04.md`, `docs/retrieval-v05.md`: retrieval guarantees, rejected designs, and the open native macOS gate.
- `docs/embeddings.md`, `docs/dots-integration.md`, `docs/privacy.md`, `docs/api.md`: model/runtime setup, the HTTP token model, data handling, and the MCP/CLI contract.
- `docs/performance.md`: how to measure (release binary, `wait4`/`/usr/bin/time -l`, one request in flight) and the current numbers. Any performance claim must match it.
- `HISTORY-TRACKER.md`: append-only change log. New entries go on top and follow the format in `AGENTS.md`.
