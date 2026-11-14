# CLI, MCP, and client API

## CLI

All operations accept global `--db PATH`; without it Midnight selects the OS local application-data database. Data-producing operations print JSON.

```sh
aether-midnight index --repo /work/app
aether-midnight refresh-file --repo /work/app --path src/payments.rs
aether-midnight search --repo /work/app --query 'payment timeout' --limit 5 --token-budget 1200
aether-midnight open-range --repo /work/app --path src/payments.rs --start 20 --end 45
aether-midnight list-symbols --repo /work/app --name charge
aether-midnight repo-map --repo /work/app
aether-midnight index-status --repo /work/app
aether-midnight remember --scope project:app --kind source --content 'Policy source text' --source-uri file:///policy.md
aether-midnight remember --scope project:app --kind derived --content 'Timeout is 40 ms' --source-id 1 --valid-from '2025-01-01T00:00:00Z'
aether-midnight bench-memory --sizes 1000,10000,100000
aether-midnight recall --scope project:app --query timeout --at '2025-01-15T00:00:00Z'
aether-midnight list-memories --scope project:app
aether-midnight forget --scope project:app --id 2
aether-midnight diagnose
aether-midnight compact

# Phase 1 CLI and Phase 2 embeddings (docs/embeddings.md)
aether-midnight init
aether-midnight memory add|get|list|delete|search --namespace project:app …
aether-midnight memory list --namespace project:app --kind derived --limit 50 --offset 50
aether-midnight memory scopes                                   # namespaces with active memories
aether-midnight memory export --namespace project:app --output app.jsonl
aether-midnight memory import app.jsonl [--namespace project:copy]
aether-midnight models list
aether-midnight models pull multilingual-e5-small
aether-midnight embed --namespace project:app --repo /work/app
aether-midnight memory search --namespace project:app "query" --mode hybrid
aether-midnight search --repo /work/app "query" --mode hybrid
```

For a raw source, `source_uri` is optional. Derived memories must cite a raw `source_id` from the same scope or provide a `source_uri`. Scopes must include a namespace prefix and colon. Time inputs are RFC 3339 and normalized to UTC.

`search` returns `{scope, results, token_budget, estimated_tokens, truncated, token_estimator}`. `scope` (`repo:<id>:worktree:<id>`) is stated once for the whole response. Each result includes a source path, optional current line range and snippet, freshness, generation/completeness, match reason, deterministic score components, and estimated token count; it no longer carries `scope`. Stale or unavailable source results never include a snippet or line range. `open-range` allows at most 200 lines and rejects absolute paths and `..` traversal.

`memory export` writes JSON Lines: a header `{"format":"aether-midnight-memory-export","version":1,"scope":…}` and then every active memory of the namespace as a `MemoryRecord`, oldest first. Forgotten memories are not exported. `--output` creates a new owner-only (`0600`) file and never overwrites one; without it the export goes to standard output and the summary to standard error. `memory import` reads such a file (`-` for standard input) in one transaction: every record is stored or none is. Records get new ids and `created_at`; content, kind, `source_uri`, `observed_at` and validity windows are kept, and `source_id`/`supersedes_id` are remapped to the new ids. A link to a memory that is not in the file is dropped when the record keeps other provenance (`source_uri`) or only recorded supersession (counted in `dropped_source_links`/`dropped_supersedes_links`); a derived record left without any provenance fails the whole import. `--namespace` places every record in that namespace. Lines are limited to 1 MiB and imports to 1,000,000 records. `memory list` pages with `--offset` (at most 100,000) and filters with `--kind`; `memory scopes` reads the trigger-maintained per-namespace counts and never scans memory rows.

`refresh-file` updates one repository-relative path without a repository walk. A full `index` is still required afterward to reconcile other changes and restore complete status. `bench-memory` runs the isolated synthetic 1k/10k/100k memory scaling workload.

`recall` returns an array of `MemorySearchResult` objects. Each result includes `candidate_coverage`, an additive field describing how much of the eligible set was examined: `complete` means every eligible row was examined; `exact_top_k` means the list is the exact top-k under the documented ordering (exact phrase, distinct informative terms, BM25, then the most recently remembered row among exact ties) even though lower-priority classes were not enumerated after the limit filled; `bm25_top_k` means a bounded BM25 window was reranked (queries with more than four informative terms) and is not an exactness claim; `recency_window` marks the opt-in newest-first shortcut; `row_id_sample` identifies the experimental row-ID sampler. No value claims semantic completeness, and an empty array means no evidence was returned, not that the namespace was exhaustively searched.

The optional `recall` argument `broad_terms` (`exact`, the default, or `recent`) applies only to a single informative term found in at least half of a namespace of at least 2,048 memories. `recent` skips ranking and returns the newest matches in about 3 ms at 100k records; it can omit older evidence that exact BM25 would rank first (the frozen `common-status-single-token` case), so agents should request it only for latency-bounded browsing.

Use `bench-memory --namespace-count 1` for one large namespace or `--namespace-count 4` for the distributed workload. Both use the same total record counts and report warm recall, storage, CPU, RSS, and retrieval-quality measurements.

To reproduce the 10k-file full-reconciliation versus direct-refresh visibility workload, run `target/release/aether-midnight refresh-file --repo "$FIXTURE" --path src/00000.rs  # measure under /usr/bin/time -l

### Project storage and client registration

```sh
midnight init --storage project            # <repo>/.midnight/ (private), .gitignore updated, project trusted
midnight setup                             # user-wide in a fresh machine; per-repository inside a Git repo afterwards
midnight project list | untrust [PATH]     # projects whose .midnight/ Midnight may use
midnight connect claude-code|claude-desktop|codex --scope project|user [--read-only] [--dry-run] [--remove] [--with-instructions]
```

`--with-instructions` also writes a short managed block into the project's `CLAUDE.md` (Claude Code) or `AGENTS.md` (Codex) that tells the agent to search code with Midnight, recall at the start of a task, and remember decisions as a source followed by a derived fact, and names the project's default memory namespace. The block sits between `<!-- aether-midnight:begin … -->` and `<!-- aether-midnight:end -->`; text outside it is never changed, a second run replaces the block in place, and `--remove --with-instructions` takes it out again (removing the file when it held only the block). An existing file is backed up under `.midnight/backups/`. Claude Desktop has no project instructions file, so the flag is refused for it. Mismatched or duplicated markers are refused instead of guessed.

Database selection, highest precedence first: `--db`, the `.midnight/midnight.sqlite3` of a trusted project containing the working directory, the OS data directory. `connect` pins a database only when `--db` is given. Project-scope client files run `midnight` from `PATH` (portable, safe to commit); user-scope files, including every Claude Desktop entry, name the absolute binary. Existing client files are merged, never replaced; invalid JSON is refused; the previous contents are saved as `<file>.midnight.bak` (under `.midnight/backups/` for project files). `status --json` reports `project.storage`, `trusted`, and the effective `midnight.json` values.

`.midnight/midnight.json` accepts `version`, `embeddings` (`false` = keyword search for this project) and `mcp.read_only` / `mcp.memory_scopes` (narrowing only). Unknown fields are errors. Memories live in whichever database is selected, so a project with its own database does not see memories stored in the global one; use `--storage global` when memories must be shared across repositories.

### Update and uninstall

```sh
midnight update [--check] [--pre] [--dry-run] [--to VERSION] [--force]   # aliases: upgrade, self-update
midnight uninstall [--dry-run] [--yes] [--purge-config] [--purge-data] [--remove-client-entries]
```

With `--json`, `update --check` prints `{current, latest, tag, prerelease, update_available}`; `uninstall --dry-run` prints the plan. See `docs/installation.md` for the safety rules.

## MCP

**v0.1.0-rc.6:** `search_code` and `recall` accept `mode: lexical|semantic|hybrid`.
Code search defaults to hybrid with a saved, permitted embedding provider; recall stays lexical
unless explicitly requested. `index_repository` also updates embeddings when configured and
authorized, adding `embedding` or `embedding_notice`. HTTP tokens need
`--allow-remote-embeddings`; existing tokens remain local-only. Stdio supports `--local-only`.
Search/index tools that may call a provider now advertise `openWorldHint: true`. See
[the complete mode and privacy contract](hybrid-mcp.md).

**Code ranking contract change:** fresh literal phrase candidates have their own FTS lookup and
rank behind exact full paths/symbols but ahead of other matches. Hybrid protects those evidence
classes before RRF and reports `fusion: "literal-priority+rrf(k=60)"`. Its numeric `score` is the
raw within-class RRF score, not a globally descending value. Preserve response order. Memory
lexical ranking and memory RRF are unchanged.

Run `aether-midnight mcp` with JSON-RPC messages over stdio. The implementation targets [MCP protocol version 2025-11-25](https://modelcontextprotocol.io/specification/2025-11-25) and negotiates: if the client's `initialize` names 2025-11-25, 2025-06-18, 2025-03-26 or 2024-11-05, that version is echoed back; any other request gets 2025-11-25. Only the `tools` surface is implemented, which these revisions share. The server advertises tools, and accepts `initialize`, `ping`, `tools/list`, and `tools/call`. It does not use a third-party MCP SDK. This is the 2025-11-25 handshake subset; the later stateless protocol revision and other transports are not implemented.

Available tools are `index_repository`, `refresh_indexed_file`, `search_code`, `open_range`, `list_symbols`, `repo_map`, `index_status`, `remember`, `remember_batch`, `recall`, `get_memory`, `list_memories`, `list_scopes`, and `forget`. Every code tool accepts `repo_path`, which is optional: when omitted it defaults to the single repository root of the policy, else to the server's working directory if that is a Git repository root (never a non-repository directory); otherwise the call fails asking for `repo_path`. An explicit path is never replaced, and policy roots still apply. Every memory tool except `list_scopes` receives `scope`. When a call omits it, a policy that pins exactly one scope supplies that scope; otherwise the server uses `project:<directory name of the default repository>` (whitespace and control characters become `-`), the same repository the code tools default to, provided the policy's scope list allows it. An explicit scope is never replaced and policy scope checks still apply; with no default repository, or a scope list that excludes the default, the call fails asking for `scope`. The `initialize` instructions state which scope applies. `get_memory` fetches one active memory by `id` within its exact namespace (an id from another namespace is not found). `list_memories` accepts `offset` (0–100,000) and `kind` (`source`/`derived`) and stays newest first. `list_scopes` returns `{scopes:[{scope, active_memories}], default_scope}`, listing only namespaces with active memories that the policy allows. `remember` and `remember_batch` accept `embed` (default `true`): with a configured provider that `index_repository` would also be allowed to use (saved provider, project embeddings not disabled, the policy's remote grant and saved provider-bound consent for remote providers), the stored memories are embedded right after the write commits. `remember` then adds `embedding` (an embed report) or `embedding_notice`; the memory stays stored on any embedding failure. `remember_batch` keeps returning the stored records; a failure there leaves those memories unembedded, which semantic and hybrid recall report as partial `semantic_coverage`, and `embed --namespace` completes. Remote write-time embedding uses the interactive budget (one attempt, five-second timeout). No embedding happens without a configured provider. Each tool carries MCP `annotations`: `readOnlyHint` is true for every tool except `index_repository`, `refresh_indexed_file`, `remember`, `remember_batch` and `forget`; only `forget` sets `destructiveHint`; `openWorldHint` is true for `index_repository`, `search_code`, `recall`, `remember` and `remember_batch` because their configured behavior can contact a provider, and false for other tools. `remember_batch` accepts 1–512 records with a 16 MiB combined content/source-URI cap, validates every item before writing, and commits atomically; an invalid record fails the complete batch. Persistent MCP sessions reuse one lazily opened SQLite connection for memory and read-only code operations, and load the installed local embedding model at most once per connection.

**Index completeness (contract change, 0.1.0-rc.5).** `complete` in `index_repository` and `index_status` reports, and `index_complete` in `search_code` and `open_range` results, mean the file catalog and the lexical index are complete: the last full scan finished and reconciled. They no longer depend on Tree-sitter. `structural_complete` is separate and is `false` when at least one indexed file failed to parse (syntax error the grammar rejects, or the 250 ms parse budget). `index_status` also returns `parse_error_files` (count) and `parse_error_paths` (at most 20, sorted); `skipped.tree_sitter_parse_error` keeps counting them. Such files remain fully searchable by text; their symbols come only from the parts of the tree that parsed. `list_symbols` and `repo_map`, which return structural data, keep `index_complete`/`complete` equal to catalog complete AND structural complete. Clients that treated `complete: false` as "some file had a parse error" must read `structural_complete` or `parse_error_files` instead. No database schema change.

**Compact code-search results (contract change, 0.1.0-rc.5).** (1) `scope` moved from every result to the response (`CodeSearchResponse.scope`); `freshness`, `index_generation` and `index_complete` stay per result, so freshness and coverage are not lost. Clients that read `result.scope` must read `response.scope`. (2) Each snippet line is capped at 200 characters; on the matching line the kept window is centered on the match, and every cut side is marked `…`. A capped snippet sets `snippet_truncated`, and the response `truncated` is true whenever any snippet is truncated. `open_range` is unchanged and returns full lines. (3) Snippets now default to 3 lines starting at the matching line instead of 8; `context_lines` (MCP argument and CLI `--context-lines`, 1–8, values outside are clamped) restores up to 8. The reported `line_end` is the last line shown (also for semantic hits, whose range previously followed the whole chunk). `estimated_tokens` no longer charges the scope string to each result. On four queries with limit 3 over this repository the sum of per-result estimated tokens went from 3470 to 1220 and the JSON from 14353 to 7199 bytes, with identical result paths and order; the estimator is the project heuristic, not a model tokenizer.

**Code search ordering (0.1.0-rc.5).** `score` and `score_breakdown.lexical_fts` are unchanged and still saturate at 50 for any text match with a BM25 magnitude above about 2.5e-5. Results with equal `score` are now ordered by raw FTS5 BM25 (better first) and only then by path. Before, equal scores were ordered by path alone, so a file with the rarest query term could rank below path-earlier files that matched only common words. Requests remain serial and response IDs/order retain the existing JSON-RPC contract.

**Compact, budgeted code-search results (contract change, 0.2 development).** Design: `docs/design/v0.2-token-savings.md`. MCP `search_code` now returns the `compact-v1` shape by default; `verbose: true` returns the exact previous shape (old CLI output is unchanged). Ranking, evidence classes, result order and `limit` are untouched; only presentation and the budget cut change.

Old (per result, rc.7):

```json
{"scope":"repo:R:worktree:W","results":[{"path":"src/alpha.rs","line_start":12,"line_end":14,"match_reason":"lexical","index_generation":3,"index_complete":true,"freshness":"fresh","snippet":"12:     let needle = value;\n13: ...","snippet_truncated":false,"estimated_tokens":61,"score":4.2,"score_breakdown":{"exact_path":0,"exact_symbol":0,"lexical_fts":4.2}}],"token_budget":2048,"estimated_tokens":61,"truncated":false,"token_estimator":"midnight-heuristic-v1 (...)"}
```

New (default):

```json
{"scope":"repo:R:worktree:W","generation":3,"complete":true,"shape":"compact-v1","results":[{"path":"src/alpha.rs","freshness":"fresh","match_reason":"lexical","symbol":"function outer_function","ranges":[{"lines":"1-14","ref":"src/alpha.rs#1-14@0a1b2c3d4e5f","text":"1: pub fn outer_function(...) {\n2: ...\n12:     let needle = value;"}]}],"token_budget":2048,"estimated_tokens":88,"truncated":false,"token_estimator":"midnight-heuristic-v1 (...)"}
```

* Hoisted to the response: `generation`, `complete` (formerly per-result `index_generation`, `index_complete`). Removed from each result unless `explain: true` (which adds `score` and `score_breakdown`): `line_start`, `line_end`, `snippet`, `snippet_truncated`, `estimated_tokens`, `index_generation`, `index_complete`, `score`, `score_breakdown`. Kept: `path`, `freshness`, `match_reason`.
* `ranges` (at most 2 per result, adjacent ones merged) are bounded by the innermost indexed symbol: the symbol's signature line(s) plus a `context_lines` window at the match; without a symbol, the plain window. Text lines are numbered and capped at 200 characters as before.
* `ref` is a stateless handle `path#start-end@hash12` (first 12 hex characters of the file's content hash). `open_range` accepts `{"ref": ...}` instead of `path`/`line_start`/`line_end` and expands the range (up to 200 lines). If the file changed since the ref was issued, `open_range` returns `stale_handle: true`, `freshness: "stale"`, the current `content_hash` and **no content**; a stale ref is never served as fresh.
* Budget: `estimated_tokens` (a heuristic estimate of the serialized `results` array; the small fixed header — scope, generation, labels — is not charged, as before) is at most `token_budget`. When the budget is short, bodies are reduced in this order — `detail: "signature"` (signature plus the matching line), then `detail: "reference"` (only `lines` and `ref`) — starting with the lowest evidence class and the lowest score per estimated token; the tail is dropped only when even references do not fit. Order is never changed; `truncated` is true whenever anything was reduced or dropped, the index is incomplete, or the result limit was reached.
* Connection history: a range already sent on this connection with the same content hash is returned as `{"lines","ref","delivered":true}` without text. `fresh_context: true` forces text, the history is cleared on `initialize`, holds at most 512 range hashes, never stores query text, and is lost on restart (the next response simply carries text again).
* `repo_map` keeps its previous fields and adds a deterministic, ranked, budgeted `files` list (`ln(1+symbols) - 0.1*depth`, plus a bias for `query`; `token_budget` default 1024) with `files_total`, `files_estimated_tokens`, `files_truncated`. Fan-in centrality is not implemented yet.
* Hybrid and semantic responses keep their `SearchInfo` fields (`mode_used`, `degraded`, `notice`, ...) next to the compact fields.

**File watcher (0.2 development, opt-in).** Design: `docs/design/v0.2-watcher.md`. With `MIDNIGHT_WATCH=1` (and by default inside the daemon, once it exists) a long-lived process watches each indexed repository with native OS events (FSEvents, inotify, ReadDirectoryChangesW; never polling), refreshes changed files through the single-file path and reconciles once after storms, branch switches, queue overflow and backend errors. `index_status` gains `watcher`: `{state, reason, pending, watches, refresh_count, reconcile_count, overflow_count, error_count, last_error}` where `state` is `off`, `catching_up`, `active`, `degraded` (for example inotify watch limit reached; the reason names the limit) or `unavailable`. Result freshness labels do not depend on the watcher: they come from a content-hash comparison at query time, so an edit is `stale` until it is refreshed. The watcher uses `refresh_watched_file`, which leaves the catalog `complete` flag as it was (the watcher reconciles whenever it could have missed a change); the public `refresh_indexed_file` tool still marks the catalog partial.

**Single-writer daemon (0.2 development).** Design: `docs/design/v0.2-daemon.md`. `midnight mcp` (stdio) and `serve-http` connect to one daemon per database file, starting it when none runs; the daemon owns all writes (a FIFO write gate around `index_repository`, `refresh_indexed_file`, `remember`, `remember_batch`, `forget`), hosts the file watchers (default on there) and the per-connection delivery history, and keeps one local model. The same tools, `Policy` checks, scope injection and repo-root checks run daemon-side, per connection, from the policy the front end sends in its handshake; relative `repo_path` values and the default repository resolve against the client's working directory.

* `midnight daemon start|stop|status` (`run` is internal). `mcp --no-daemon` or `MIDNIGHT_DAEMON=0` serves in-process as before. When the daemon cannot be used, `mcp` prints `midnight: daemon unavailable (<reason>); running in-process` to stderr, and the `initialize` instructions say `Server mode: in-process (<reason>)` or `Server mode: daemon`.
* Idle shutdown after `MIDNIGHT_DAEMON_IDLE_SECS` (default 600, `0` = never) without connections. `MIDNIGHT_RUN_DIR` overrides the socket directory (default `$XDG_RUNTIME_DIR/midnight`, else `<config dir>/run`, falling back to `/tmp/midnight-<uid>` when the path would exceed the socket-path limit). The directory is `0700`, the socket `0600`, ownership is verified.
* Wire: newline-delimited JSON. First line is a control message (`hello` with `protocol`, `schema`, `client_version`, `policy`, `cwd`; or `status`, `shutdown`, `shutdown_if_idle`); after `{"midnight":"ready"}` the stream is the stdio MCP dialect. A different protocol or schema yields `version_mismatch`: an idle daemon is restarted once, a busy one is reported with how to stop it; the client never falls back to a second writer for that case.
* Not routed through the daemon yet: one-shot CLI commands (`index`, `memory ...`). They keep the SQLite WAL, the index lock and the 5 s `busy_timeout`, so they can still report `database is locked` while a daemon holds a long write. Windows has no transport yet (named pipes are designed), so it runs in-process.

## Node.js and Bun client examples

The same no-dependency client example is provided for Node.js and Bun and starts the Rust MCP process. Runtime verification is pending (not run) in the latest benchmark run:

```sh
export AETHER_MIDNIGHT_BIN="$PWD/target/release/aether-midnight"
export AETHER_REPO="$PWD"
node examples/node/client.mjs
# or: bun examples/node/client.mjs
```

Optional variables: `AETHER_MIDNIGHT_DB` selects the SQLite file and `AETHER_QUERY` selects the code query. The example initializes the server, calls `search_code`, and prints the returned tool payload.

## Go

```sh
export AETHER_MIDNIGHT_BIN="$PWD/target/release/aether-midnight"
export AETHER_REPO="$PWD"
(cd examples/go && go run .)
```

`examples/go` contains a small Go module and JSON-RPC stdio client. It uses the same optional environment variables as the Node example. Go runtime verification is pending (not run) in the latest benchmark run. Both clients require the Rust executable to be built or installed separately.

### Schema 10 memory recall

An explicit MCP `recall` scope searches a single exact namespace. Omitted scope in a trusted
project can add `global:user` when allowed by policy, returning a `results` array and `layers`
with per-layer search information. The project wins ties within lexical evidence classes.
Records carry `origin`; writes can return `possible_duplicate_of` and `duplicate_evidence`
without merging observations. Current-time recall can use the configured provider when
compatible active vectors exist and saved consent allows it. An explicit lexical mode stays
offline. Pass `token_budget` to request a packed response with previews or ID references.
