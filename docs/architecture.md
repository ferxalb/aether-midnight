# Architecture and data flow

Midnight is one local Rust engine. The CLI, MCP server, and examples call the same core code and use the same SQLite schema.

## Four layers

1. **Sources and scopes.** A repository is identified by its canonical root and Git common directory where available; a worktree gets its own identity. Repository and worktree identifiers are mandatory columns in code index lookups. Agent memories use an explicit caller namespace (for example `user:alice` or `project:payments`) in every query. Raw source rows and derived facts are distinct; derived rows can cite a source row or URI and can supersede an earlier fact while retaining its history.
2. **Deterministic indexes.** SQLite stores file metadata, a contentless FTS5 `unicode61` token index, extracted symbol spans, and a separate FTS5 memory index. The code index retains token postings without storing whole source text; snippets are read from the current file. Exact paths and symbols have dedicated lookups. Tree-sitter extraction runs for Rust, Python, TypeScript, and TSX. Other file types remain searchable lexically and are labelled by extension; they do not receive structural parsing in v1.
3. **Retrieval and context packing.** Candidate scores expose path, symbol, and lexical contributions. Results carry repository/worktree scope, index generation, completeness, freshness, source path, and current-file snippets where the indexed content hash still matches. `open_range` reads current source. Search snippets are packed under a deterministic heuristic token budget; the estimate is not a model tokenizer.
4. **Agent interfaces.** The CLI and stdio MCP adapter expose the same operations. Node.js, Bun, and Go client examples are provided; their runtime verification is pending (not run) in the latest Linux benchmark run.

## Index flow

`index` walks one root with Git ignore/exclude behavior and `.aetherignore`, filters default exclusions, checks file type/size and repository containment, reads at most 1 MiB, hashes and optionally parses it, then writes bounded SQLite batches. On Unix, repository-relative file access opens each directory component from a root handle with `openat(O_NOFOLLOW)`, so a raced symlink cannot redirect a read outside the root. The walker also does not follow symlinks. VCS metadata directories remain excluded even with `--include-generated` because their config can contain credentials. A batch is capped at 512 files and 16 MiB of text (with at most one max-sized file of threshold overshoot). Work is synchronous and serial in v1, so the queue and worker counts are bounded at one.

Each successful full scan advances a generation. Rows not seen in that generation are removed, including files deleted or newly ignored since the prior scan. A canceled scan leaves `catalog_complete=false`; it does not perform generation reconciliation. Progress rows are published while scanning so a separate process can search the same WAL database and see explicitly partial results. MCP calls within one stdio process are handled serially, so use a second process if indexing and search must overlap.

For an empty/incomplete index, search tries scoped indexed candidates first, places eligible paths in a SQLite disk-backed temporary table, then reads them in path order until it finds the first match. It returns that bounded result with `fresh_unindexed`, `index_complete=false`, and a truncated response flag; the temporary candidate list disappears with the connection. Index the repository to retrieve a ranked multi-result set.

## SQLite and format

SQLite is built from the pinned `rusqlite` bundled SQLite dependency, with FTS5. Database schema migrations use one immediate transaction. `PRAGMA user_version` and the `metadata.index_format_version` both identify the current index as format `2`. The v1 trigram-to-`unicode61` migration drops unrebuildable code postings, clears the file catalog, and marks repositories incomplete so a full reindex is required. WAL mode provides readers during short indexing commits. `compact` clears forgotten memory content, optimizes FTS, checkpoints WAL, and vacuums the database.

`TextIndex` is an internal interface around replace, stale-document removal, and search operations. SQLite FTS5 `unicode61 remove_diacritics 2` is the shipped code text tokenizer. Queries split into alphanumeric/underscore terms, including one- and two-character terms, and are matched with OR semantics. This returns whole-token matches; exact paths and extracted symbols have dedicated lookups. A fixed-fixture comparison found `unicode61` much smaller and faster than trigram with equal Recall@5/MRR for the tested content queries. The comparison method and SQLite versions are recorded in the performance guide. `rg` remains an external baseline.

## Current boundaries

There is no filesystem watcher. Every explicit index command reconciles the file catalog, while metadata size/mtime avoids reading unchanged files unless `--verify-hashes` is set. A file that changes while indexing may require a later reconciliation. Each Tree-sitter parse has a 250 ms budget checked through its progress callback; a timed-out parse keeps the lexical file entry, omits structural symbols, and marks the structural index incomplete. The callback is cooperative and may overshoot until Tree-sitter's next progress check; the 1 MiB file cap bounds each input. No allocator live-heap or mmap-page instrumentation is included. These are tracked limits, not claims of a strict operating-system wall-clock deadline.
