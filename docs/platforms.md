# Build and runtime matrix

| Component | Intended support | Built or run in this workspace |
| --- | --- | --- |
| Midnight core | Stable Rust; OS-local app-data path; bundled SQLite | Historical Linux and native desktop benchmark measurements are available; host identifiers are omitted from published evidence. |
| SQLite | Bundled SQLite with FTS5 `unicode61` code tokenizer | Runtime version recorded in each result file. |
| Structural parsing | Tree-sitter Rust, Python, TypeScript, TSX | Release benchmark indexed Linux and VS Code repositories; no separate parser test suite run. |
| Node.js client | Node.js with built-in child-process and readline modules | pending (not run) |
| Bun client | Bun using Node-compatible built-ins | pending (not run) |
| Go client | Go standard library | pending (not run) |
| MCP wire protocol | stdio JSON-RPC subset pinned to 2025-11-25 | Linux historical round trips plus native macOS process-start and persistent-session measurements in `benchmark/runs/`. |

Published measurements are limited to their recorded test environments and do not establish behavior on every platform. Host and hardware identifiers are intentionally omitted. No sanitizers or allocator profiler were run.

## Pending verification

- host model withheld native desktop host with identifiers withheld: smoke and standard fixture campaigns run; see [comparison](performance-comparison.md)
- Apple Silicon macOS: pending (not run)
- Go: pending (not run)
- Node.js: pending (not run)
- Bun: pending (not run)
- Windows: pending (not run)
- Linux ARM: pending (not run)
