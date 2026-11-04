# Build and runtime matrix

| Component | Intended support | Built or run in this workspace |
| --- | --- | --- |
| Midnight core | Stable Rust; OS-local app-data path; bundled SQLite | macOS 26.7.1, x86_64, Rust 1.99.1; release build, tests, clippy, and 10k-file benchmark |
| SQLite | Bundled SQLite with FTS5 `unicode61` code tokenizer | SQLite 3.54.1 in benchmark build |
| Structural parsing | Tree-sitter Rust, Python, TypeScript, TSX | Crate versions recorded in lockfile; Rust/Python/TypeScript symbol tests run |
| Node.js client | Node.js with built-in child-process and readline modules | Node.js 26.13.0 syntax and end-to-end MCP example |
| Bun client | Bun using Node-compatible built-ins | Not verified: Bun was not installed on the benchmark host |
| Go client | Go standard library | Go 1.27.3 build and end-to-end MCP example |
| MCP wire protocol | stdio JSON-RPC subset pinned to 2025-11-25 | Initialize, tool discovery, tool call, and search integration test |

Linux, Windows, ARM, and non-macOS SQLite behavior are not built or tested in this delivery. Unix file reads use `openat`/`O_NOFOLLOW`; non-Unix source reads fail closed until an equivalent rooted handle API is implemented. Windows and other non-Unix systems are therefore not supported for code indexing/search in this version. `std::os::unix` no-follow and database permission behavior was tested only on macOS. No sanitizers or allocator profiler were available or run.
