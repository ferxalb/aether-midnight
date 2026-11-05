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
```

For a raw source, `source_uri` is optional. Derived memories must cite a raw `source_id` from the same scope or provide a `source_uri`. Scopes must include a namespace prefix and colon. Time inputs are RFC 3339 and normalized to UTC.

`search` returns `{results, token_budget, estimated_tokens, truncated, token_estimator}`. Each result includes a source path, optional current line range and snippet, freshness, scope, generation/completeness, match reason, deterministic score components, and estimated token count. Stale or unavailable source results never include a snippet or line range. `open-range` allows at most 200 lines and rejects absolute paths and `..` traversal.

`refresh-file` updates one repository-relative path without a repository walk. A full `index` is still required afterward to reconcile other changes and restore complete status. `bench-memory` runs the isolated synthetic 1k/10k/100k memory scaling workload.

`recall` returns an array of `MemorySearchResult` objects. Each result includes `candidate_coverage`, an additive field describing the candidate generator: `complete` means every row matching the lexical query and scope/deletion/validity filters was considered; `bm25_top_k` means only the bounded FTS5 BM25 candidate window was reranked; `row_id_sample` identifies the experimental row-ID window strategy. `bm25_top_k` does not claim semantic completeness. An empty array has no per-result coverage field and means no evidence was returned, not that the whole namespace was exhaustively searched. The production path uses BM25 ordering; row-ID sampling is available only in retrieval-experiment builds.

Use `bench-memory --namespace-count 1` for one large namespace or `--namespace-count 4` for the distributed workload. Both use the same total record counts and report warm recall, storage, CPU, RSS, and retrieval-quality measurements.

To reproduce the 10k-file full-reconciliation versus direct-refresh visibility workload, run `python3 scripts/benchmark_refresh_file.py --files 10000 --samples 30`; it emits JSON and never contacts a service.

## MCP

Run `aether-midnight mcp` with JSON-RPC messages over stdio. The implementation pins [MCP protocol version 2025-11-25](https://modelcontextprotocol.io/specification/2025-11-25), advertises tools, and accepts `initialize`, `ping`, `tools/list`, and `tools/call`. It does not use a third-party MCP SDK. This is the 2025-11-25 handshake subset; the later stateless protocol revision and other transports are not implemented.

Available tools are `index_repository`, `refresh_indexed_file`, `search_code`, `open_range`, `list_symbols`, `repo_map`, `index_status`, `remember`, `remember_batch`, `recall`, `list_memories`, and `forget`. Every code tool receives `repo_path`; every memory tool receives `scope`. `remember_batch` accepts 1–512 records with a 16 MiB combined content/source-URI cap, validates every item before writing, and commits atomically; an invalid record fails the complete batch. Persistent MCP sessions reuse one lazily opened SQLite connection for memory operations. Requests remain serial and response IDs/order retain the existing JSON-RPC contract.

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
