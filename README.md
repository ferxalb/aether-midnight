# AETHER Midnight

**Local code search and explicit memory for AI agents, in a single Rust binary.**

AETHER Midnight indexes a code repository on your machine so you, or an AI agent, can find the right file, line range, or symbol and get results you can trace back to the source. It also stores memories that you save explicitly, separated by namespace, so an agent can recall them later.

Default lexical search needs no server, account, API key, model, or network connection. Local hybrid search requires the explicit runtime and model setup below.

> **Status:** early development. The package is version `0.1.0` and has not been released, so commands and output fields may still change. See [CHANGELOG.md](CHANGELOG.md) and [HISTORY-TRACKER.md](HISTORY-TRACKER.md).

## What it does

- **Code search.** Full-text search over a repository's tokens, paths, and symbols. Each result includes its path, line range, match reason, and freshness.
- **Source-grounded reads.** `open-range` returns up to 200 lines of the current file. `list-symbols` lists functions, types, and other symbols found by Tree-sitter.
- **Repository map.** `repo-map` summarizes languages, symbols, and top-level directories.
- **Explicit memory.** `remember` saves a raw source or a fact that cites a source. `recall` searches one namespace as of a given time. `forget` hides a memory.
- **Agent interfaces.** `mcp` serves 12 tools over MCP on stdio. `serve-http` serves the same tools on a loopback HTTP port that requires bearer tokens.

### What it does not do

- **Semantic search is opt-in.** Default search is lexical (whole tokens, no arbitrary substrings). `--mode semantic|hybrid` needs stored embeddings from the pinned local model (`runtime install`, `models pull`) or from an explicitly selected cloud provider (`--provider openrouter|rainy`, which sends the embedded text to that provider); see [docs/embeddings.md](docs/embeddings.md).
- **No automatic capture.** Midnight never records conversations. It stores only what you tell it to store.
- **No file watching.** After you change files, run `index` again (or `refresh-file` for one file).
- **No UI, cloud sync, or LLM generation.** Cloud embeddings are the only optional network use besides the two explicit downloads (`runtime install`, `models pull`).

## Quick start

You need a Rust toolchain with Cargo that supports edition 2024 (Rust 1.98 or later, the minimum of the `rainy-sdk` dependency) and a C compiler (for `aws-lc-sys`). SQLite is bundled, so you install nothing else.

```sh
git clone https://github.com/ferxalbs/aether-midnight.git
cd aether-midnight
cargo build --release --locked

BIN=./target/release/aether-midnight
$BIN index --repo /path/to/your/repository
$BIN search --repo /path/to/your/repository --query 'retry budget'
$BIN index-status --repo /path/to/your/repository
```

Data commands print JSON. Run `index` once to build the index and again after you change files. Without an index, `search` reads files directly and marks each result `fresh_unindexed`. Run `index` for repeatable, ranked results.

### Local hybrid search

For the macOS x86_64 binary candidate, extraction, checksums and installation are in
[installation instructions](docs/installation.md). The current RC is unsigned and
Gatekeeper rejects it; see the [signing gate](docs/release-signing.md).
After the source build above, run:

```sh
$BIN runtime install
$BIN models pull multilingual-e5-small
$BIN models verify
$BIN memory add --namespace project:demo 'La política de pagos permite tres reintentos.'
$BIN embed --namespace project:demo
$BIN memory search --namespace project:demo 'payment retry policy' --mode hybrid --json
$BIN embed --repo /path/to/your/repository
$BIN search --repo /path/to/your/repository --query 'retry policy' --mode hybrid
```

The two explicit downloads require Internet access and about 500 MB plus cache space.
Subsequent local inference is offline. Allow about 2 GiB free RAM; a fresh semantic CLI
query takes roughly four seconds. Check `degraded`, `notice` and `semantic_coverage` in
JSON: fallback or a partial index is reported explicitly. Only macOS x86_64 is verified
for this embedding release. Cloud credentials are unnecessary for this local workflow.

## Examples

Search, then read the source:

```sh
$BIN search --repo ~/src/payments --query 'payment timeout' --limit 5 --token-budget 1200
$BIN open-range --repo ~/src/payments --path src/payments.rs --start 20 --end 45
$BIN list-symbols --repo ~/src/payments --name charge
```

Save and recall memories:

```sh
# 1. Save a raw source (id 1)
$BIN remember --scope project:payments --kind source \
  --content 'Payment policy: the retry budget is capped at 3 attempts.' \
  --source-uri file:///docs/policy.md

# 2. Save a derived fact that cites source 1 and is valid from a given date (id 2)
$BIN remember --scope project:payments --kind derived \
  --content 'Retry cap is 3 attempts' --source-id 1 \
  --valid-from 2025-01-01T00:00:00Z

$BIN recall --scope project:payments --query retry
$BIN forget --scope project:payments --id 2
```

## Concepts

- **Repository scope.** Code commands take `--repo` (default: the current directory) and search only that repository or worktree. Each Git worktree has its own index. Non-Git folders also work.
- **Namespaces.** Memory scopes must have a prefix and a colon, such as `user:alice` or `project:my-app`. Passing `--scope payments` fails. Code and memory are stored separately and never mixed in a query.
- **Source and derived memories.** A `source` is raw text, optionally with a URI. A `derived` fact must cite a source, either `--source-id` in the same namespace or `--source-uri`. It can have validity times, and `--supersedes-id` replaces it while keeping its history.
- **Forget and compact.** `forget` hides a memory from `recall` and `list-memories` immediately. Its text stays in the database file until `compact` runs. Even after that, backups or filesystem snapshots may keep old bytes, so Midnight does not claim secure erasure.

## Use with AI agents

### MCP over stdio

Run the server with `aether-midnight mcp`. Most MCP clients accept a configuration block like this; check your client's documentation for where the file goes:

```json
{
  "mcpServers": {
    "aether-midnight": {
      "command": "/absolute/path/to/aether-midnight",
      "args": ["--db", "/absolute/path/to/midnight.sqlite3", "mcp"]
    }
  }
}
```

The `--db` option is optional. The server implements MCP protocol version 2025-11-25 with `initialize`, `ping`, `tools/list`, and `tools/call`. It does not use a third-party MCP SDK and handles one request at a time.

| Area | Tools | Required argument |
| --- | --- | --- |
| Code | `index_repository`, `refresh_indexed_file`, `search_code`, `open_range`, `list_symbols`, `repo_map`, `index_status` | `repo_path` |
| Memory | `remember`, `remember_batch`, `recall`, `list_memories`, `forget` | `scope` |

`remember_batch` accepts 1 to 512 records and commits them atomically. If any record is invalid, the whole batch is rejected.

### Loopback HTTP for AETHER Dots

`serve-http` listens only on `127.0.0.1` or `::1`. Every request needs a bearer token, which you create and revoke with the `token` subcommand:

```sh
$BIN --db midnight.sqlite3 serve-http                    # prints http://127.0.0.1:<port>/mcp
$BIN --db midnight.sqlite3 token create --name dot-1 --memory-scope 'dot:conversation-1'
$BIN --db midnight.sqlite3 token list
$BIN --db midnight.sqlite3 token revoke dot-1
```

Revocation takes effect on the next request. The full integration model is in [docs/dots-integration.md](docs/dots-integration.md).

### Client examples

- [`examples/node/client.mjs`](examples/node/client.mjs) uses only built-in Node.js modules. The same file is meant to run under Bun, but that has not been verified.
- [`examples/go`](examples/go) is a small Go module that uses only the standard library.

Both start the Rust binary, so build it first. Their runtime checks are still pending (see [Platform support](#platform-support)).

## Privacy and storage

- **No outbound requests.** Midnight makes no outbound network calls and has no telemetry. The HTTP mode listens only on loopback.
- **Database location.** By default the database is `midnight.sqlite3` in `aether-midnight/` inside the OS local application-data directory. On Linux that is `~/.local/share/aether-midnight/`. On macOS it is `~/Library/Application Support/aether-midnight/`. Use `--db PATH` to choose another file. On Unix the file is created with mode `0600`.
- **What is stored.** File paths and hashes, token postings (not whole source files), symbol locations, Git metadata, and your memories. Code snippets are read from the current files at query time.
- **Not encrypted.** Protect the database and its backups under your own policy.
- **Default exclusions.** Midnight respects `.gitignore` and a root `.aetherignore`. It skips version-control folders, dependency and build output (such as `node_modules`, `target`, `dist`, and `vendor`), and secret-looking files (such as `.env*`, `*.pem`, `*.key`, and names containing `secret` or `credentials`). It also skips files over 1 MiB and files that are binary or not valid UTF-8. Credential and key files stay excluded even with `--include-generated`.
- Do not index a repository whose contents you cannot store on this machine.

Full details are in [docs/privacy.md](docs/privacy.md).

## Known limitations

- **Concurrent writers.** Two processes writing to the same database at once can fail with `database is locked`. An intermittent failure appears in the `two_server_processes_share_one_database_without_losing_acknowledged_writes` test. Until this is fixed, give each server process its own database.
- **Structural parsing** (`list-symbols`, symbol search) covers Rust, Python, TypeScript, TSX, and JavaScript. Other file types are searchable as text only.
- **Token budgets** are a deterministic estimate, not the token count of any particular model.
- **No watcher and no release.** Changes are picked up only when you re-index, and interfaces may change before a release.

## Performance

Performance targets, reproduction steps, and measurements are in [docs/performance.md](docs/performance.md). Results describe the fixture they were measured on and are not guarantees for other machines.

To run a bounded local smoke test:

```sh
cargo build --release --locked
target/release/aether-midnight bench --repo "$FIXTURE" --heldout-repo benchmark/retrieval_fixture --samples 5 --index-samples 5
```

Write raw output under `benchmark/runs/`, which Git ignores. Committed benchmark history in `benchmark/` is not modified.

## Platform support

| Platform | Status |
| --- | --- |
| Linux x86_64 | Builds with `--locked`; the tests pass except the intermittent concurrency case listed under [Known limitations](#known-limitations); historical benchmark results are recorded |
| macOS on Intel | A native release benchmark run is recorded |
| macOS on Apple Silicon | Not run yet |
| Linux ARM and Windows | Not run yet |
| Node.js, Bun, and Go clients | Examples included; runtime checks pending |

The full matrix is in [docs/platforms.md](docs/platforms.md).

## Documentation

- [API and tool reference](docs/api.md): every CLI command, MCP tool, and output field
- [Architecture](docs/architecture.md): layers, indexing flow, and SQLite schema
- [Privacy and storage](docs/privacy.md): exclusions, deletion, and what gets written to disk
- [Performance](docs/performance.md) and [platforms](docs/platforms.md)
- [AETHER Dots integration](docs/dots-integration.md): loopback HTTP and tokens
- [Retrieval design record](docs/retrieval-v04.md): guarantees and rejected experiments for memory recall

## Contributing

Build and check the project with these commands:

```sh
cargo test --locked
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
```

Repository layout:

| Path | Contents |
| --- | --- |
| `src/code.rs` | Repository indexing and code retrieval |
| `src/memory.rs` | Explicit memories |
| `src/db.rs` | SQLite persistence and migrations |
| `src/mcp.rs`, `src/mcp_http.rs` | MCP dispatch, stdio, and loopback HTTP transports |
| `src/auth.rs` | Bearer-token registry for `serve-http` |
| `tests/` | Integration tests for the MCP transports |
| `scripts/` | Benchmark and maintenance scripts |

Contributor guidelines are in [AGENTS.md](AGENTS.md).

## License

Licensed under the [Apache License 2.0](LICENSE). Third-party dependency licenses are listed in [THIRD_PARTY_LICENSES.json](THIRD_PARTY_LICENSES.json), which you can regenerate with `target/release/midnight-harness license-inventory --force`.
