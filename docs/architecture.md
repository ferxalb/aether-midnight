# Architecture and data flow

Midnight is one local Rust engine. The CLI, MCP server, and examples call the same core code and use the same SQLite schema.

## Four layers

1. **Sources and scopes.** A repository is identified by its canonical root and Git common directory where available; a worktree gets its own identity. Repository and worktree identifiers are mandatory columns in code index lookups. Agent memories use an explicit caller namespace (for example `user:alice` or `project:payments`) in every query. Raw source rows and derived facts are distinct; derived rows can cite a source row or URI and can supersede an earlier fact while retaining its history.
2. **Deterministic indexes.** SQLite stores file metadata and content, an FTS5 trigram index, extracted symbol spans, and a separate FTS5 memory index. Exact paths and symbols have dedicated lookups. Tree-sitter extraction runs for Rust, Python, TypeScript, and TSX. Other file types remain searchable lexically and are labelled by extension; they do not receive structural parsing in v1.
3. **Retrieval and context packing.** Candidate scores expose path, symbol, and lexical contributions. Results carry repository/worktree scope, index generation, completeness, freshness, source path, and current-file snippets where the indexed content hash still matches. `open_range` reads current source. Search snippets are packed under a deterministic heuristic token budget; the estimate is not a model tokenizer.
4. **Agent interfaces.** The CLI and stdio MCP adapter expose the same operations. Node.js, Bun, and Go examples start the Rust process and speak the same MCP wire protocol; they add no native ABI or duplicate index.

## Index flow

`index` walks one root with Git ignore/exclude behavior and `.aetherignore`, filters default exclusions, checks file type/size and repository containment, reads at most 1 MiB, hashes and optionally parses it, then writes bounded SQLite batches. The walker does not follow symlinks. On Unix file opens use `O_NOFOLLOW`. VCS metadata directories remain excluded even with `--include-generated` because their config can contain credentials. The batch is capped at 64 files and approximately 8 MiB of text (with at most one max-sized file of threshold overshoot). Work is synchronous and serial in v1, so the queue and worker counts are bounded at one.

Each successful full scan advances a generation. Rows not seen in that generation are removed, including files deleted or newly ignored since the prior scan. A canceled scan leaves `catalog_complete=false`; it does not perform generation reconciliation. Progress rows are published while scanning so a separate process can search the same WAL database and see explicitly partial results. MCP calls within one stdio process are handled serially, so use a second process if indexing and search must overlap.

For an empty/incomplete index, search tries the scoped indexed candidates first and then walks eligible files directly until it finds enough matches. These are marked `fresh_unindexed`, have a bounded current-source snippet, and are never represented as indexed results. The fallback does not persist data.

## SQLite and format

SQLite is built from the pinned `rusqlite` bundled SQLite dependency, with FTS5. Database schema migrations use one immediate transaction. `PRAGMA user_version` and the `metadata.index_format_version` both identify v1 as format `1`; changing incompatible index semantics requires a migration and format-version change. WAL mode provides readers during short indexing commits. `compact` optimizes FTS, checkpoints WAL, and vacuums the database.

`TextIndex` is an internal interface around trigram replace/remove/search operations. SQLite FTS5 trigram is the only shipped code text engine. Queries without any term of three or more characters skip trigram matching and still use path/symbol lookup. Memory uses SQLite's Unicode tokenizer; code uses SQLite trigram. `rg` is run only as an external benchmark baseline.

## Current boundaries

There is no filesystem watcher. Every explicit index command reconciles the file catalog, while metadata size/mtime avoids reading unchanged files unless `--verify-hashes` is set. A file that changes while indexing may require a later reconciliation. Tree-sitter exposes no hard per-file timeout through the current binding; the 1 MiB cap bounds input size but not wall time. No allocator live-heap or mmap-page instrumentation is included. These are tracked limitations, not measured guarantees.
