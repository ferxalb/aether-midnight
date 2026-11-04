# Privacy, ignored files, and storage

Midnight is local-only in v1. The engine has no HTTP client, telemetry, cloud endpoint, embedding provider, or network-dependent startup path. The MCP server communicates over its parent process's stdio. Benchmark baselines (`rg`) run locally.

## Stored data

The default database is `midnight.sqlite3` below the OS local application-data directory in `aether-midnight/`. Override it with global `--db PATH`. On Unix the database file is set to owner-only read/write (`0600`); SQLite WAL files are stored beside it. The database contains indexed source text, paths, hashes, symbols and spans, Git repository/worktree metadata, skip counts, raw memory sources, derived facts, and their provenance/time links. Midnight does not encrypt the database at rest. Protect the containing volume and backups according to your local policy.

Use `compact` to optimize FTS and physically reclaim SQLite pages after forget/deletion. Use `--db` to open the database with SQLite tools or copy it while Midnight is closed (include or checkpoint WAL consistently). To delete all local data, stop Midnight and remove the database plus any `midnight.sqlite3-wal` and `midnight.sqlite3-shm` sidecars. The default location is printed by `diagnose` as `database_path`.

## Repository discovery and exclusions

Git ignore and exclude rules are respected, including for untracked files; `--require-git` is disabled so non-Git folders also work. A root `.aetherignore` adds ignore rules. The walker does not follow symlinks, and opens re-check root containment and use no-follow semantics where the OS supports them. Credentials and private keys remain excluded even when generated/vendor paths are explicitly included.

The default generated/vendor exclusions include `.git`, `.hg`, `.svn`, `node_modules`, `target`, `dist`, `build`, `out`, `vendor`, `third_party`, `coverage`, `.next`, `.nuxt`, `.cache`, Python environments/caches, `Pods`, `DerivedData`, `bower_components`, `.turbo`, `.parcel-cache`, `*.min.js`, `*.map`, and `*.lockb`. Secret-name/extension exclusions include `.env*`, `.netrc`, `.npmrc`, names containing `credentials` or `secret`, private-key/container extensions (`.pem`, `.key`, `.p12`, `.pfx`, `.keystore`, `.mobileprovision`), and common SSH key/host files. The exact applied counts and reasons are returned from indexing.

`--include-generated` opts into generated/vendor directories and file patterns, but not credentials/private keys or VCS metadata. Files larger than 1 MiB are skipped. Files with NUL bytes or invalid UTF-8 are skipped. The explicit index may contain source text that was not recognized as sensitive; review exclusions and do not index repositories with material you cannot store locally.

## Memory semantics

Memories are never inferred from conversations or stored automatically. Call `remember` explicitly. `source` rows are raw provenance; `derived` rows require a source id in the same namespace or a source URI. Timestamps accept RFC 3339 and are normalized to fixed-precision UTC before temporal comparisons. Replacements add a new row and close the earlier validity interval when applicable.

`forget` immediately soft-deletes the row and removes it from the FTS index in one SQLite statement/trigger path. Recall and listing filter forgotten rows, and the operation is namespace-scoped. The underlying record content remains in SQLite pages until `compact` runs; external backups are unaffected. Forget does not implicitly delete facts that link to the forgotten source; callers should forget each derived record they intend to suppress.

Code and memories have separate tables, APIs, and indexes. Code queries require one repo/worktree scope; memory queries require one exact namespace. No default operation merges them.

## Operational limits

Midnight uses synchronous serial indexing, has no live filesystem watcher, and cannot interrupt an individual Tree-sitter parse with the current binding. Cancellation is checked between discovered entries; a canceled index stays marked partial. Incomplete indexes can be read by a separate process while indexing continues, with results marked partial. MCP requests in one process are serialized. Search contexts use a deterministic token estimate rather than a model tokenizer. See [performance](performance.md) for measured memory limits and unverified allocator/mmap values.
