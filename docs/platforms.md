# Build and runtime matrix

| Component | Intended support | Built or run in this workspace |
| --- | --- | --- |
| Midnight core | Stable Rust; OS-local app-data path; bundled SQLite | Linux x86_64 historical benchmark plus native macOS 26.7.1 x86_64 Intel release/index/retrieval/MCP measurements recorded in `benchmark/runs/`; Apple Silicon pending. |
| SQLite | Bundled SQLite with FTS5 `unicode61` code tokenizer | Runtime version recorded in each result file. |
| Structural parsing | Tree-sitter Rust, Python, TypeScript, TSX | Release benchmark indexed Linux and VS Code repositories; no separate parser test suite run. |
| Node.js client | Node.js with built-in child-process and readline modules | pending (not run) |
| Bun client | Bun using Node-compatible built-ins | pending (not run) |
| Go client | Go standard library | pending (not run) |
| MCP wire protocol | stdio JSON-RPC subset pinned to 2025-11-25 | Linux historical round trips plus native macOS process-start and persistent-session measurements in `benchmark/runs/`. |

The native macOS run exercises an Intel x86_64 host only; it does not establish Apple Silicon, Windows, or ARM results. Linux source indexing was exercised on the earlier recorded Linux host; this does not establish behavior on other Linux systems. No sanitizers or allocator profiler were run.

## Pending verification

- Intel macOS reference host: smoke and standard fixture campaigns run; see [comparison](performance-comparison.md)
- Apple Silicon macOS: pending (not run)
- Go: pending (not run)
- Node.js: pending (not run)
- Bun: pending (not run)
- Windows: pending (not run)
- Linux ARM: pending (not run)
