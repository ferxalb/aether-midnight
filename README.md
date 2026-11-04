# AETHER Midnight

AETHER Midnight is a local Rust engine for deterministic code retrieval and explicitly stored agent memory. It uses SQLite for persistence and lexical indexes, Tree-sitter for selected structural indexes, and one process boundary shared by the CLI and MCP tools. It needs no service, key, model, or network connection.

v0.1 is intentionally small. It supports repository/worktree-scoped code search, source-grounded ranges and symbols, plus separately namespaced source and derived memories. It does not provide embeddings, automatic conversation capture, cloud sync, a UI, or an LLM.

## Build and use

    cargo build --release --locked
    ./target/release/aether-midnight index --repo /path/to/repository
    ./target/release/aether-midnight search --repo /path/to/repository --query 'retry budget'
    ./target/release/aether-midnight index-status --repo /path/to/repository

The default SQLite database is under the OS local application-data directory in aether-midnight/midnight.sqlite3. --db /path/to/midnight.sqlite3 selects another database. On Unix, Midnight creates and restricts the database file to owner read/write (0600). The database stores code-token postings and metadata plus explicit memory content; whole code files are read from the repository at retrieval time. Data is not encrypted by Midnight.

An empty or incomplete index can still return a direct-read result marked fresh_unindexed; that fallback does not persist an index. Run index for repeatable lexical and symbol search. Search supports whole tokens and identifier-boundary fragments such as refreshTok matching refreshToken. Arbitrary substrings inside a token, such as reshTok inside refreshToken, are unsupported by the shipped search path. Incremental scans reconcile the full eligible file catalog. --verify-hashes disables the size/mtime fast path and re-reads each eligible file.

## Interfaces

- aether-midnight --help lists CLI operations for indexing, code search, ranges, symbols, maps, status, explicit memory operations, compaction, diagnostics, benchmarking, and MCP.
- aether-midnight mcp serves the pinned MCP 2025-11-25 stdio protocol subset implemented here. Node.js uses examples/node/client.mjs; A Go client example is present, but it was not verified in this iteration.
- Search and MCP tools require a repository path for every code query. Memory calls require a namespace such as user:alice or project:my-app.

See [API and tools](docs/api.md), [architecture](docs/architecture.md), and [privacy and storage](docs/privacy.md).

## Performance evidence

Targets, reproduction instructions, machine details, measured percentiles, and resource limits are in [performance and benchmarks](docs/performance.md). The benchmark recreates the exact 10,000-file / 100 MiB fixture in a temporary directory and emits machine-readable JSON. It does not evict the OS file cache. Use a temporary output path for experiments so the checked-in result and historical baseline remain intact.

## Supported-platform status

The current implementation and performance iteration were built and tested on Linux x86_64. The Node.js MCP example passed on Node 24.22.1. Go could not be verified because the installed command rejected both version and run; Bun was unavailable. The prior macOS result is a historical baseline, not a measurement of the current pipeline. See the [platform matrix](docs/platforms.md).

## Project docs

- [Performance contract and benchmark reproduction](docs/performance.md)
- [Privacy, exclusions, deletion, and storage](docs/privacy.md)
- [API/tool reference and client examples](docs/api.md)
- [Layering and data flow](docs/architecture.md)
- [Build and runtime matrix](docs/platforms.md)
- [Third-party dependency/license inventory](THIRD_PARTY_LICENSES.json), reproducible with python3 scripts/license_inventory.py

Licensed under Apache-2.0; see [LICENSE](LICENSE).
