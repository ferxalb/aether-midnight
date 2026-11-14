# Build and runtime matrix

| Component | Intended support | Built or run in this workspace |
| --- | --- | --- |
| Midnight core | Stable Rust; OS-local app-data path; bundled SQLite | Historical Linux and native desktop benchmark measurements are available; host identifiers are omitted from published evidence. |
| SQLite | Bundled SQLite with FTS5 `unicode61` code tokenizer | Runtime version recorded in each result file. |
| Structural parsing | Tree-sitter Rust, Python, TypeScript, TSX | Release benchmark indexed Linux and VS Code repositories; no separate parser test suite run. |
| Node.js client | Node.js with built-in child-process and readline modules | pending (not run) |
| Bun client | Bun using Node-compatible built-ins | pending (not run) |
| Go client | Go standard library | pending (not run) |
| ONNX Runtime 1.23.2 (local embeddings) | Pinned official Microsoft archive, installed by `runtime install` with SHA-256 verification; dynamic loading | **macOS x86_64: installed, loaded and used for real inference from a clean environment.** No pin exists for other platforms; none is claimed. |
| Local model `multilingual-e5-small` @ `614241f6…` | Pinned revision, SHA-256 verified by `models pull`/`verify` and at load | macOS x86_64: pulled, verified, tamper-tested, used for hybrid search. |
| Cloud embeddings (`rainy-sdk` 0.6.51; OpenRouter, Rainy) | Rust ≥ 1.98, C compiler for `aws-lc-sys`; HTTPS | Built and tested on macOS x86_64 against a loopback mock only. No live provider call was made in the release run unless a credential was configured (see `v0.1-status.md`). |
| MCP wire protocol | stdio JSON-RPC `tools` subset; negotiates 2025-11-25, 2025-06-18, 2025-03-26, 2024-11-05 | Linux historical round trips plus native macOS process-start and persistent-session measurements in `benchmark/runs/`. |

Published measurements are limited to their recorded test environments and do not establish behavior on every platform. Host and hardware identifiers are intentionally omitted. No sanitizers or allocator profiler were run.

## Support tiers

| Platform | Tier | Meaning |
| --- | --- | --- |
| macOS (Intel and Apple Silicon, universal2 binary) | Supported | Release target; installer, `update` and the documented verification apply. |
| Linux | **Experimental** | Builds from source and has historical benchmark runs, but no release archive, installer or `update` path. Not stable: expect gaps and report them. |
| Windows | **Experimental** | Not run by the maintainers yet. Unix-specific containment (`openat`/`O_NOFOLLOW`), file modes and the PTY tests do not apply as documented. Not stable. |

Experimental means behavior, storage format guarantees and security properties documented for
macOS are not promised there. Do not rely on it for important data.

## Pending verification

- host model withheld native desktop host with identifiers withheld: smoke and standard fixture campaigns run; see [comparison](performance-comparison.md)
- Apple Silicon macOS: pending (not run); an arm64 ONNX Runtime exists upstream but Midnight (runtime install, model, cloud) has not been run on it
- Go: pending (not run)
- Node.js: pending (not run)
- Bun: pending (not run)
- Windows: pending (not run)
- Linux ARM: pending (not run)

## v0.2 experimental platform work

Linux glibc x86-64 and ARM64 archive build and update paths are **NOT RUN** on Linux.
ORT 1.23.2 archive and library bytes were checked against the committed pins. Actual runtime
loading, minimum glibc and vector parity are **OPEN**. Windows x86-64 and ARM64 containment,
archive assembly and ORT install are **NOT RUN**. User-only ACLs and named-pipe daemon
transport are **OPEN**.
