# AETHER Midnight v1

AETHER Midnight is a local Rust engine for deterministic code retrieval and explicitly stored agent memory. It uses SQLite for persistence and lexical indexes, Tree-sitter for selected structural indexes, and one process boundary shared by the CLI and MCP tools. It needs no service, key, model, or network connection.

V1 is intentionally small. It supports repository/worktree-scoped code search, source-grounded ranges and symbols, plus separately namespaced source and derived memories. It does not provide embeddings, automatic conversation capture, cloud sync, a UI, or an LLM.

## Build and use

```sh
cargo build --release --locked
./target/release/aether-midnight index --repo /path/to/repository
./target/release/aether-midnight search --repo /path/to/repository --query 'retry budget'
./target/release/aether-midnight index-status --repo /path/to/repository
```

The default SQLite database is under the OS local application-data directory in `aether-midnight/midnight.sqlite3`. `--db /path/to/midnight.sqlite3` selects another database. On Unix, Midnight creates and restricts the database file to owner read/write (`0600`). SQLite stores source text and memory content locally in that database; data is not encrypted by Midnight.

An empty or incomplete index can still return a direct-read result marked `fresh_unindexed`; that fallback does not persist an index. Run `index` for repeatable lexical and symbol search. Incremental scans reconcile the full eligible file catalog. `--verify-hashes` disables the size/mtime fast path and re-reads each eligible file.

## Interfaces

- `aether-midnight --help` lists CLI operations for indexing, code search, ranges, symbols, maps, status, explicit memory operations, compaction, diagnostics, benchmarking, and MCP.
- `aether-midnight mcp` serves the pinned MCP 2025-11-25 stdio protocol subset implemented here. Node.js and Bun use [examples/node/client.mjs](examples/node/client.mjs); Go uses [examples/go](examples/go).
- Search and MCP tools require a repository path for every code query. Memory calls require a namespace such as `user:alice` or `project:my-app`.

See [API and tools](docs/api.md), [architecture](docs/architecture.md), and [privacy and storage](docs/privacy.md).

## Performance evidence

Targets, reproduction instructions, machine details, measured percentiles, and resource limits are in [performance and benchmarks](docs/performance.md). Run `python3 scripts/run_benchmark.py` to regenerate the exact 10,000-file / 100 MiB fixture in a temporary directory and emit machine-readable JSON to stdout. The command does not evict the OS file cache. Its results are measurements for the stated machine, not universal latency claims.

## Supported-platform status

Rust stable is the implementation target. macOS x86_64 is the only platform built and measured in this workspace. Node.js and Go examples are verified on the listed versions; Bun is documented as a compatible runtime for the Node example but was not available for execution here. See [platform matrix](docs/platforms.md).

## Project docs

- [Performance contract and benchmark reproduction](docs/performance.md)
- [Privacy, exclusions, deletion, and storage](docs/privacy.md)
- [API/tool reference and client examples](docs/api.md)
- [Layering and data flow](docs/architecture.md)
- [Build and runtime matrix](docs/platforms.md)
- [Third-party dependency/license inventory](THIRD_PARTY_LICENSES.json), reproducible with `python3 scripts/license_inventory.py`

Licensed under Apache-2.0; see [LICENSE](LICENSE).
