# Privacy, ignored files, and storage

Midnight is local-first, with lexical retrieval as its unconfigured default and no telemetry.
Remote embeddings require an explicitly selected provider and credentials; saved configurations
also require provider-bound upload consent. Once enabled, CLI and MCP stdio code searches default
to hybrid: searches send query text, and indexing sends eligible source chunks to that provider.
MCP HTTP additionally requires a token with `--allow-remote-embeddings`; existing tokens deny
remote calls. MCP tools cannot select providers, supply credentials or grant consent. Explicit
lexical retrieval stays offline, and `midnight mcp --local-only` disables remote calls for that
process. A key in the environment alone never enables cloud retrieval. Missing or mismatched
vectors do not cause query uploads or a provider switch. See [MCP modes and consent](hybrid-mcp.md).

Provider keys are read from `OPENROUTER_API_KEY` / `RAINY_API_KEY` or the private credential store
when saved through setup; they are never logged or returned by MCP. Vectors stay in the local
database, while the provider's terms govern text it receives. Runtime/model installation,
setup validation/model discovery, and explicit release checks/updates can also use the network;
ordinary lexical retrieval and local inference do not. Runtime and model downloads are pinned
and verified as described in [embeddings](embeddings.md). MCP uses stdio or authenticated loopback
HTTP; neither transport implies permission to upload content.

MCP connections cache up to 32 successful query vectors, reusable for 60 seconds from creation,
with hashed query keys, scope and full model identity. Expired entries are purged lazily on the
next cache access or connection close; an idle connection can retain expired vectors. No query plaintext is retained by this
cache and it is never persisted. Consent is checked even on cache hits; fresh database/source
retrieval runs on every search. Closing the connection drops the cache, and HTTP changes of
bearer principal reset it. This does not alter the retention of explicitly saved memories.

## Stored data

The default database is `midnight.sqlite3` below the OS local application-data directory in `aether-midnight/`. Override it with global `--db PATH`. On Unix the database file is set to owner-only read/write (`0600`); SQLite WAL files are stored beside it. The database contains repository paths and hashes, token postings (not whole code files), extracted symbols and spans, Git repository/worktree metadata, skip counts, and raw memory sources, derived facts, and their provenance/time links. Code snippets are read from current files at query time. Midnight does not encrypt the database at rest. Protect the containing volume and backups according to your local policy.

Memory FTS uses a contentless-delete index: it stores searchable terms and a namespace token rather than a second full-text copy. Searchable postings and the primary memory row are still local data. `diagnose` prints the selected path as `database_path`. For a portable export, stop Midnight and use SQLite's backup command, for example `sqlite3 /path/to/midnight.sqlite3 ".backup 'midnight-export.sqlite3'"`. To delete all local data, stop Midnight and remove the database plus any `midnight.sqlite3-wal` and `midnight.sqlite3-shm` sidecars.

## Project storage and trust

With `--storage project` the database lives in `<repo>/.midnight/` (directory `0700`, database `0600`) and `.midnight/` is added to `.gitignore`. A `.midnight/` that arrives with a clone is not opened and its `midnight.json` is not read until you adopt that repository (`init --storage project`, `setup`, or `connect --storage project`), which records its path in `trusted_projects.json` in the Midnight config directory. `project untrust` removes the record and leaves the data in place. `midnight.json` cannot contain a provider, endpoint, credential or upload consent; those exist only in the user-wide `config.json` and `credentials.json`. `.midnight/` is never indexed.

## Network use by `update`

`midnight update` is the only command that contacts GitHub, and only when you run it: one request
for the release list (`api.github.com`), then, unless `--check` or `--dry-run`, the release's archive
and checksum. Requests carry a generic `aether-midnight/<version>` user agent and no identifier.
Nothing is checked in the background. `AETHER_MIDNIGHT_UPDATE_API` can point `update` at a mirror
(`https`, or `http` on loopback for tests).

## Repository discovery and exclusions

Git ignore and exclude rules are respected, including for untracked files; `--require-git` is disabled so non-Git folders also work. A root `.aetherignore` adds ignore rules when it is a regular, non-symlink file no larger than 1 MiB; oversized ignore files are not parsed and are reported. The walker does not follow symlinks, and opens re-check root containment and use no-follow semantics where the OS supports them. Credentials and private keys remain excluded even when generated/vendor paths are explicitly included.

The default generated/vendor exclusions include `.git`, `.hg`, `.svn`, `node_modules`, `target`, `dist`, `build`, `out`, `vendor`, `third_party`, `coverage`, `.next`, `.nuxt`, `.cache`, Python environments/caches, `Pods`, `DerivedData`, `bower_components`, `.turbo`, `.parcel-cache`, `*.min.js`, `*.map`, and `*.lockb`. Secret-name/extension exclusions include `.env*`, `.netrc`, `.npmrc`, names containing `credentials` or `secret`, private-key/container extensions (`.pem`, `.key`, `.p12`, `.pfx`, `.keystore`, `.mobileprovision`), and common SSH key/host files. The exact applied counts and reasons are returned from indexing.

`--include-generated` opts into generated/vendor directories and file patterns, but not credentials/private keys or VCS metadata. Files larger than 1 MiB are skipped. Files with NUL bytes, disallowed ASCII control bytes, or invalid UTF-8 are skipped. The explicit index may contain source text that was not recognized as sensitive; review exclusions and do not index repositories with material you cannot store locally.

## Memory semantics

Memories are never inferred from conversations or stored automatically. Call `remember` explicitly. `source` rows are raw provenance; `derived` rows require a source id in the same namespace or a source URI. Timestamps accept RFC 3339 and are normalized to fixed-precision UTC before temporal comparisons. Replacements add a new row and close the earlier validity interval when applicable.

`forget` immediately soft-deletes the row and removes its FTS postings in one SQLite statement/trigger path. Recall and listing filter forgotten rows, and the operation is namespace-scoped. The row's content remains in SQLite until `compact` clears forgotten content, optimizes FTS, checkpoints/truncates the WAL, and vacuums the database. This is logical deletion followed by database compaction, not a guarantee of forensic erasure: SQLite page reuse, filesystem snapshots, backups, and storage-device behavior can retain prior bytes. Midnight does not claim secure erasure. Forget does not implicitly delete facts that link to the forgotten source; callers should forget each derived record they intend to suppress. Provenance references can therefore point to a forgotten source by design, while normal recall/listing excludes that source.

Every recall candidate query applies the exact namespace, non-deleted, and validity-time SQL predicates before loading results. Production uses class-staged exact top-k BM25 retrieval (`docs/retrieval-v04.md`); when a namespace holds most memories the FTS expression omits the namespace token and the SQL `m.scope = ?` predicate alone enforces the boundary, which the oracle and skewed-namespace tests verify; optional retrieval experiments can use bounded row-ID sampling and label returned rows with `candidate_coverage: "row_id_sample"`. Candidate ranking changes which eligible records can be returned, but it does not widen namespace access or bypass deletion and temporal filters. A namespace remains a retrieval boundary, not an authentication mechanism.

Code and memories have separate tables, APIs, and indexes. Code queries require one repo/worktree scope; memory queries require one exact namespace. No default operation merges them.

## Operational limits

Midnight uses a bounded per-index worker pool and one SQLite writer, with no live filesystem watcher. Each Tree-sitter parse has a 250 ms cooperative budget; a parse that reaches the budget keeps lexical retrieval available but marks structural indexing incomplete (`structural_complete=false`; `complete` is unaffected). The progress callback also checks cancellation. A canceled index stops dispatch, releases queued buffers, joins workers, stays marked partial, and can be recovered by the next index run. Incomplete indexes can be read by a separate process while indexing continues, with results marked partial. MCP requests in one process are serialized. Search contexts use a deterministic token estimate rather than a model tokenizer. See [performance](performance.md) for measured memory limits and unverified allocator/mmap values.

## Published benchmark evidence

Public benchmark reports may retain workload definitions, aggregate timings, sample counts, and process resource measurements. They must omit personal or third-party host identifiers, machine models, local absolute paths, environment variables, account identifiers, and billing or reservation records. Keep unredacted local run artifacts out of version control.

Optional operation keys add a local schema-5 ledger containing the exact namespace/key, input SHA-256 digest and memory ID. These metadata persist after forget and compaction to prevent key reuse; hashes are not encryption and can confirm guesses about low-entropy payloads. Do not put secrets in operation keys. No duplicate memory content is stored in the ledger. `maintain` offers bounded forgotten-content scrubbing and FTS merging; full `compact` remains available. Neither operation promises forensic erasure.
