# Repository Guidelines

## Project Structure

This is a Rust 2024 Cargo project. Core implementation lives in `src/`: `code.rs` handles repository indexing and retrieval, `memory.rs` manages explicit memories, `db.rs` owns SQLite persistence and migrations, and `mcp.rs` implements the MCP interface (shared dispatch and permission `Policy`) and `mcp_http.rs` the loopback-only HTTP transport used by AETHER Dots; `tests/mcp_http.rs` covers it. Unit tests are in `src/tests.rs`, `src/retrieval_tests.rs` (exhaustive BM25 oracle and adversarial memory-retrieval cases), and module-local test blocks; the CLI integration test is in `tests/mcp_stdio.rs`. User and architecture documentation is in `docs/`, client examples in `examples/`, and benchmark and maintenance scripts in `scripts/`.

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

## Retrieval Changes

Any change to memory candidate generation or ranking must (1) keep `src/retrieval_tests.rs` passing, including exact agreement with the exhaustive oracle for up to four informative terms, (2) leave every metric and per-query rank of the frozen suites in `benchmark/` unchanged unless the change is a deliberate, documented contract change (never edit labels to improve a score), and (3) report paired before/after numbers from `bench-retrieval` and `bench-memory` run on the same host, including classes that got slower. Do not make a non-exact shortcut the default; opt-in shortcuts must be labeled through `candidate_coverage`. `docs/retrieval-v04.md` records the guarantees, rejected experiments, and the pending native macOS gate.

## Data and Documentation

Midnight stores explicit memories and index metadata locally. Changes to storage, retention, path exclusions, or indexing limits should be reflected in the relevant privacy, architecture, API, or performance docs. Do not add secrets, private repositories, or generated benchmark data to commits.

## Public Benchmark Privacy

Treat benchmark, profiling, diagnostic, and campaign output as private by default. Before tracking or sharing it, remove personal or third-party host and account identifiers, machine and runtime fingerprints, local absolute paths and usernames, environment-variable values, credentials, provider project or run identifiers, and billing, cost, or reservation records. Keep unredacted raw runs in ignored local storage. Public reports may include workload definitions, aggregate measurements, sample counts, and non-identifying platform categories only. Review the staged diff and the resulting JSON or Markdown for these fields before committing; do not copy raw output into issues, pull requests, documentation, or chat reports.

## The Projects Future

The Projects are evolving very quickly. Don't limit yourself—if there are major changes to the core, don't restrict it, but stay on the same track; let it happen and allow it, and edit these rules, which may change as the project progresses, and edit your rules whenever necessary.
