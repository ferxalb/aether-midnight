# Build and runtime matrix

| Component | Intended support | Built or run in this workspace |
| --- | --- | --- |
| Midnight core | Stable Rust; OS-local app-data path; bundled SQLite | macOS 26.6, x86_64, Rust 1.99.1; release build, tests, clippy, and 10k-file benchmark |
| SQLite | Bundled SQLite with FTS5 and trigram tokenizer | SQLite 3.51.0 in benchmark build |
| Structural parsing | Tree-sitter Rust, Python, TypeScript, TSX | Crate versions recorded in lockfile; Rust/Python/TypeScript symbol tests run |
| Node.js client | Node.js with built-in child-process and readline modules | Node.js 26.13.0 syntax and end-to-end MCP example |
| Bun client | Bun using Node-compatible built-ins | Not verified: Bun was not installed on the benchmark host |
| Go client | Go standard library | Go 1.27.0 build and end-to-end MCP example |
| MCP wire protocol | stdio JSON-RPC subset pinned to 2025-11-25 | Initialize, tool discovery, tool call, and search integration test |

Linux, Windows, ARM, and non-macOS SQLite behavior are not built or tested in this delivery. They are Rust implementation targets, not verified platform support. `std::os::unix` no-follow and database permission behavior was tested only on macOS. No sanitizers or allocator profiler were available or run.
