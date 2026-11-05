# AETHER Midnight

AETHER Midnight is a local Rust engine for deterministic code retrieval and explicitly stored agent memory. It uses SQLite for persistence and lexical indexes, Tree-sitter for selected structural indexes, and one process boundary shared by the CLI and MCP tools. It needs no service, key, model, or network connection.

v0.1 is intentionally small. It supports repository/worktree-scoped code search, source-grounded ranges and symbols, plus separately namespaced source and derived memories. It does not provide embeddings, automatic conversation capture, cloud sync, a UI, or an LLM.

## Build and use

```sh
cargo build --release --locked
./target/release/aether-midnight index --repo /path/to/repository
./target/release/aether-midnight search --repo /path/to/repository --query 'retry budget'
./target/release/aether-midnight index-status --repo /path/to/repository
```

The default SQLite database is under the OS local application-data directory in `aether-midnight/midnight.sqlite3`. `--db /path/to/midnight.sqlite3` selects another database. On Unix, Midnight creates and restricts the database file to owner read/write (`0600`). The database stores code-token postings and metadata plus explicit memory content; whole code files are read from the repository at retrieval time. Data is not encrypted by Midnight.

An empty or incomplete index can still return a direct-read result marked `fresh_unindexed`; that fallback does not persist an index. Run `index` for repeatable lexical and symbol search. Incremental scans reconcile the full eligible file catalog. `--verify-hashes` disables the size/mtime fast path and re-reads each eligible file.

## Interfaces

- `aether-midnight --help` lists CLI operations for indexing, code search, ranges, symbols, maps, status, explicit memory operations, compaction, diagnostics, benchmarking, and MCP.
- `aether-midnight mcp` serves the pinned MCP 2025-11-25 stdio protocol subset implemented here. Node.js and Bun use [examples/node/client.mjs](examples/node/client.mjs); Go uses [examples/go](examples/go).
- Search and MCP tools require a repository path for every code query. Memory calls require a namespace such as `user:alice` or `project:my-app`.

See [API and tools](docs/api.md), [architecture](docs/architecture.md), and [privacy and storage](docs/privacy.md).

## Performance evidence

Targets, reproduction instructions, machine details, measured percentiles, and resource limits are in [performance and benchmarks](docs/performance.md). Run `python3 scripts/performance_campaign.py --mode smoke` for a bounded local fixture campaign; it writes immutable, timestamped raw artifacts under `benchmark/runs/` and leaves committed benchmark history intact. See the [native macOS guide](docs/macos-benchmark-guide.md), [Modal CPU guide](docs/modal-benchmark-guide.md), [investigation](docs/performance-investigation.md), and [before/after comparison](docs/performance-comparison.md). The historical real-repository results remain in `benchmark/real-repositories.json` and `benchmark/incremental-repositories.json`.

## Supported-platform status

Rust stable is the implementation target. The current local measurements include Linux x86_64 historical results and a native Intel macOS release run. Apple Silicon and the Node.js, Go, and Bun examples remain unverified. See the [platform matrix](docs/platforms.md).

### Pending verification

- Apple Silicon macOS: pending (not run)
- Go: pending (not run)
- Node.js: pending (not run)
- Bun: pending (not run)
- Windows: pending (not run)
- ARM: pending (not run)

## Project docs

- [Performance contract and benchmark reproduction](docs/performance.md)
- [Privacy, exclusions, deletion, and storage](docs/privacy.md)
- [API/tool reference and client examples](docs/api.md)
- [Layering and data flow](docs/architecture.md)
- [Build and runtime matrix](docs/platforms.md)
- [Third-party dependency/license inventory](THIRD_PARTY_LICENSES.json), reproducible with `python3 scripts/license_inventory.py`

Licensed under Apache-2.0; see [LICENSE](LICENSE).
