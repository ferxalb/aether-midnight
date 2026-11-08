# Repository Guidelines

## Project Structure

This is a Rust 2024 Cargo project. Core implementation lives in `src/`: `code.rs` handles repository indexing and retrieval, `memory.rs` manages explicit memories, `db.rs` owns SQLite persistence and migrations, and `mcp.rs` implements the MCP interface (shared dispatch and permission `Policy`) and `embed.rs` the opt-in embeddings (pinned local model management and verification, persistent vectors, exact KNN, RRF hybrid search; lexical paths never load the model), `runtime.rs` the pinned/verified ONNX Runtime install and diagnostics, `cloud.rs` the opt-in OpenRouter/Rainy embedder over `rainy-sdk` (explicit provider selection only), `config.rs` the saved provider preferences and credential store, `ux.rs` the first-run wizard, consent, status and readable output, `mcp_http.rs` the loopback-only HTTP transport used by AETHER Dots; `tests/mcp_http.rs` covers it. `tests/common/mock.rs` is the deterministic loopback provider mock shared by `src/cloud/tests.rs` and `tests/cli_cloud.rs`; never call a real provider from tests. Unit tests are in `src/tests.rs`, `src/retrieval_tests.rs` (exhaustive BM25 oracle and adversarial memory-retrieval cases), and module-local test blocks; the CLI integration test is in `tests/mcp_stdio.rs`. User and architecture documentation is in `docs/`, client examples in `examples/`, and the Rust measurement/oracle harness `src/bin/midnight-harness.rs`.

## Build, Test, and Development

- `cargo build --locked` builds the project using the committed dependency lockfile.
- `cargo run -- --help` runs the CLI help from the working tree; add a command and its options to try a feature.
- `cargo test --locked` runs unit and integration tests.
- `cargo fmt --check` checks Rust formatting; run `cargo fmt` to apply it.
- `cargo clippy --all-targets --all-features -- -D warnings` checks all Rust targets for lint issues.
- `cargo build --release --locked` creates the optimized release binary.

## Coding Style

Follow `rustfmt` defaults (four-space indentation). Use `snake_case` for modules, functions, and variables, and `PascalCase` for types and traits. Keep changes focused within the existing module boundaries, return actionable errors, and document behavior that affects CLI or MCP users. Avoid adding dependencies without a clear need; commit `Cargo.lock` changes with dependency updates.

## Testing Guidelines

Add regression tests alongside the affected unit tests in `src/` or in `tests/` for end-to-end behavior. Use descriptive test names that state the behavior, such as `mcp_stdio_handshake_lists_tools_and_returns_grounded_result`. Tests should use temporary directories and databases for filesystem or persistence scenarios. Run `cargo test --locked` before submitting changes that affect behavior.

## Commits and Pull Requests

Recent commits use concise subjects, often with Conventional Commit prefixes such as `feat:`. Use a clear imperative summary, optionally scoped (for example, `fix: preserve partial index status`). Pull requests should explain user-visible behavior and design choices, list validation commands and results, link related issues when available, and update relevant docs or changelog entries. Include benchmark evidence when changing performance claims or indexing behavior.

## Performance Claims

All performance claims must come from running the compiled release CLI or app through a real product entry point, such as `target/release/aether-midnight --db DB mcp` (build with `cargo build --release --locked`). Measure wall time, CPU time (user + system), and peak RSS for Midnight itself (`wait4` rusage, as `midnight-harness` does, or `/usr/bin/time -l`). Keep fixture setup, builds, seeding, and client overhead outside the measurement; seed once through Midnight, reuse the database, and keep one request in flight. Verify exact ordered results before reporting numbers. The repository has no Python tooling: `src/bin/midnight-harness.rs` provides `generate-fixture`, `memory-quality`, `retrieval-quality`, `measure-mcp`, and `license-inventory`.

## Retrieval Changes

Any change to memory candidate generation or ranking must (1) keep `src/retrieval_tests.rs` passing, including exact agreement with the exhaustive oracle for up to four informative terms, (2) leave every metric and per-query rank of the frozen suites in `benchmark/` unchanged unless the change is a deliberate, documented contract change (never edit labels to improve a score), and (3) report paired before/after numbers from `bench-retrieval`, `bench-memory`, and `midnight-harness memory-quality` run on the same host, including classes that got slower. Do not make a non-exact shortcut the default; opt-in shortcuts must be labeled through `candidate_coverage`. `docs/retrieval-v04.md` records the guarantees, rejected experiments, and the pending native macOS gate.

## Data and Documentation

Midnight stores explicit memories and index metadata locally. Changes to storage, retention, path exclusions, or indexing limits should be reflected in the relevant privacy, architecture, API, or performance docs. Do not add secrets, private repositories, or generated benchmark data to commits.

## Public Benchmark Privacy

Treat benchmark, profiling, diagnostic, and campaign output as private by default. Before tracking or sharing it, remove personal or third-party host and account identifiers, machine and runtime fingerprints, local absolute paths and usernames, environment-variable values, credentials, provider project or run identifiers, and billing, cost, or reservation records. Keep unredacted raw runs in ignored local storage. Public reports may include workload definitions, aggregate measurements, sample counts, and non-identifying platform categories only. Review the staged diff and the resulting JSON or Markdown for these fields before committing; do not copy raw output into issues, pull requests, documentation, or chat reports.

## Rules for HISTORY TRACKER

- Each entry is a heading: `## YYYY-MM-DD (n) — Short title`, followed by a body of one or more short paragraphs or bullets.
- `(n)` is the modification's ordinal within its date, starting at `(1)` for the first change of that day. A new entry on an existing date takes the next number.
- Entries are ordered newest first: dates descend, and within a date `(n)` descends from top to bottom.
- State what changed, the measured effect or the decision, and where the evidence or docs live. Keep claims consistent with `docs/performance.md`; do not paste raw benchmark output or host/account identifiers.
- Never renumber or rewrite past entries except to fix a broken reference; corrections go in a new entry.

## The Projects Future

The Projects are evolving very quickly. Don't limit yourself—if there are major changes to the core, don't restrict it, but stay on the same track; let it happen and allow it, and edit these rules, which may change as the project progresses, and edit your rules whenever necessary.
