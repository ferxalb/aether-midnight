# Privacy, ignored files, and storage

Midnight is local-only in v1. The engine has no HTTP client, telemetry, cloud endpoint, embedding provider, or network-dependent startup path. The MCP server communicates over its parent process's stdio. Benchmark baselines (`rg`) run locally.

## Stored data

The default database is `midnight.sqlite3` below the OS local application-data directory in `aether-midnight/`. Override it with global `--db PATH`. On Unix the database file is set to owner-only read/write (`0600`); SQLite WAL files are stored beside it. The database contains repository paths and hashes, token postings (not whole code files), extracted symbols and spans, Git repository/worktree metadata, skip counts, and raw memory sources, derived facts, and their provenance/time links. Code snippets are read from current files at query time. Midnight does not encrypt the database at rest. Protect the containing volume and backups according to your local policy.

Memory FTS uses a contentless-delete index: it stores searchable terms and a namespace token rather than a second full-text copy. Searchable postings and the primary memory row are still local data. `diagnose` prints the selected path as `database_path`. For a portable export, stop Midnight and use SQLite's backup command, for example `sqlite3 /path/to/midnight.sqlite3 ".backup 'midnight-export.sqlite3'"`. To delete all local data, stop Midnight and remove the database plus any `midnight.sqlite3-wal` and `midnight.sqlite3-shm` sidecars.

## Repository discovery and exclusions

Git ignore and exclude rules are respected, including for untracked files; `--require-git` is disabled so non-Git folders also work. A root `.aetherignore` adds ignore rules when it is a regular, non-symlink file no larger than 1 MiB; oversized ignore files are not parsed and are reported. The walker does not follow symlinks, and opens re-check root containment and use no-follow semantics where the OS supports them. Credentials and private keys remain excluded even when generated/vendor paths are explicitly included.

The default generated/vendor exclusions include `.git`, `.hg`, `.svn`, `node_modules`, `target`, `dist`, `build`, `out`, `vendor`, `third_party`, `coverage`, `.next`, `.nuxt`, `.cache`, Python environments/caches, `Pods`, `DerivedData`, `bower_components`, `.turbo`, `.parcel-cache`, `*.min.js`, `*.map`, and `*.lockb`. Secret-name/extension exclusions include `.env*`, `.netrc`, `.npmrc`, names containing `credentials` or `secret`, private-key/container extensions (`.pem`, `.key`, `.p12`, `.pfx`, `.keystore`, `.mobileprovision`), and common SSH key/host files. The exact applied counts and reasons are returned from indexing.

`--include-generated` opts into generated/vendor directories and file patterns, but not credentials/private keys or VCS metadata. Files larger than 1 MiB are skipped. Files with NUL bytes, disallowed ASCII control bytes, or invalid UTF-8 are skipped. The explicit index may contain source text that was not recognized as sensitive; review exclusions and do not index repositories with material you cannot store locally.

## Memory semantics

Memories are never inferred from conversations or stored automatically. Call `remember` explicitly. `source` rows are raw provenance; `derived` rows require a source id in the same namespace or a source URI. Timestamps accept RFC 3339 and are normalized to fixed-precision UTC before temporal comparisons. Replacements add a new row and close the earlier validity interval when applicable.

`forget` immediately soft-deletes the row and removes its FTS postings in one SQLite statement/trigger path. Recall and listing filter forgotten rows, and the operation is namespace-scoped. The row's content remains in SQLite until `compact` clears forgotten content, optimizes FTS, checkpoints/truncates the WAL, and vacuums the database. This is logical deletion followed by database compaction, not a guarantee of forensic erasure: SQLite page reuse, filesystem snapshots, backups, and storage-device behavior can retain prior bytes. Midnight does not claim secure erasure. Forget does not implicitly delete facts that link to the forgotten source; callers should forget each derived record they intend to suppress. Provenance references can therefore point to a forgotten source by design, while normal recall/listing excludes that source.

Every recall candidate query applies the exact namespace, non-deleted, and validity-time SQL predicates before loading results. Production uses BM25-ranked candidates; optional retrieval experiments can use bounded row-ID sampling and label returned rows with `candidate_coverage: "row_id_sample"`. Candidate ranking changes which eligible records can be returned, but it does not widen namespace access or bypass deletion and temporal filters. A namespace remains a retrieval boundary, not an authentication mechanism.

Code and memories have separate tables, APIs, and indexes. Code queries require one repo/worktree scope; memory queries require one exact namespace. No default operation merges them.

## Operational limits

Midnight uses a bounded per-index worker pool and one SQLite writer, with no live filesystem watcher. Each Tree-sitter parse has a 250 ms cooperative budget; a parse that reaches the budget keeps lexical retrieval available but marks structural indexing incomplete. The progress callback also checks cancellation. A canceled index stops dispatch, releases queued buffers, joins workers, stays marked partial, and can be recovered by the next index run. Incomplete indexes can be read by a separate process while indexing continues, with results marked partial. MCP requests in one process are serialized. Search contexts use a deterministic token estimate rather than a model tokenizer. See [performance](performance.md) for measured memory limits and unverified allocator/mmap values.
