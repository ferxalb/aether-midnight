# Build and runtime matrix

| Component | Intended support | Built or run in this workspace |
| --- | --- | --- |
| Midnight core | Stable Rust; OS-local app-data path; bundled SQLite | Linux x86_64, kernel and machine recorded in `benchmark/results.json`; release build and Linux benchmark run. Tests and clippy not run in this benchmark task. |
| SQLite | Bundled SQLite with FTS5 `unicode61` code tokenizer | Runtime version recorded in each result file. |
| Structural parsing | Tree-sitter Rust, Python, TypeScript, TSX | Release benchmark indexed Linux and VS Code repositories; no separate parser test suite run. |
| Node.js client | Node.js with built-in child-process and readline modules | pending (not run) |
| Bun client | Bun using Node-compatible built-ins | pending (not run) |
| Go client | Go standard library | pending (not run) |
| MCP wire protocol | stdio JSON-RPC subset pinned to 2025-11-25 | Linux MCP process-start plus initialized `search_code` round trips are in `benchmark/real-repositories.json`. |

This Linux run does not establish results for the macOS reference host, Windows, or ARM. The runtime and operating-system checks listed below are pending (not run). Linux source indexing was exercised on this host; this does not establish behavior on other Linux systems. No sanitizers or allocator profiler were run.

## Pending verification

- macOS reference host: pending (not run)
- Go: pending (not run)
- Node.js: pending (not run)
- Bun: pending (not run)
- Windows: pending (not run)
- ARM: pending (not run)
