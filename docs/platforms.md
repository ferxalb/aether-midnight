# Build and runtime matrix

| Component | Intended support | Verified in this iteration |
| --- | --- | --- |
| Midnight core | Stable Rust, OS-local app-data path, bundled SQLite | Linux 6.18.57, x86_64, AMD EPYC 9V74 host; release build, tests, Clippy, formatting, and 10,000-file benchmark |
| SQLite | Bundled SQLite with FTS5 unicode61 code tokenizer | SQLite 3.54.1; integrity_check returned ok |
| Structural parsing | Tree-sitter Rust, Python, TypeScript, TSX | Tree-sitter 0.27.1; Rust grammar 0.24.2, Python 0.25.0, TypeScript/TSX 0.23.2; parser tests passed |
| Node.js client | Node.js built-in child-process and readline modules | Node.js 24.22.1; end-to-end MCP example passed |
| Go client | Go standard library | Unverified: the installed go command rejected both go version and go run |
| Bun client | No support claim | Not verified; Bun was unavailable |
| MCP wire protocol | stdio JSON-RPC subset pinned to 2025-11-25 | Initialize, tool discovery, tool call, and search integration test passed |

The current pipeline has not been rerun on macOS. The previous macOS 26.7.1 figures are historical pre-pipeline measurements only. Linux is the only platform verified for the current pipeline in this iteration. Windows, ARM, and non-Unix SQLite behavior were not tested. Unix file reads use openat/O_NOFOLLOW; non-Unix source reads fail closed until an equivalent rooted handle API is implemented, so Windows and other non-Unix systems are not supported for code indexing/search in this version.

No sanitizers, allocator profiler, or mmap profiler were available or run. Tantivy and Zoekt were not evaluated.
