# CLI, MCP, and client API

## CLI

All operations accept global `--db PATH`; without it Midnight selects the OS local application-data database. Data-producing operations print JSON.

```sh
aether-midnight index --repo /work/app
aether-midnight search --repo /work/app --query 'payment timeout' --limit 5 --token-budget 1200
aether-midnight open-range --repo /work/app --path src/payments.rs --start 20 --end 45
aether-midnight list-symbols --repo /work/app --name charge
aether-midnight repo-map --repo /work/app
aether-midnight index-status --repo /work/app
aether-midnight remember --scope project:app --kind source --content 'Policy source text' --source-uri file:///policy.md
aether-midnight remember --scope project:app --kind derived --content 'Timeout is 40 ms' --source-id 1 --valid-from '2025-01-01T00:00:00Z'
aether-midnight recall --scope project:app --query timeout --at '2025-01-15T00:00:00Z'
aether-midnight list-memories --scope project:app
aether-midnight forget --scope project:app --id 2
aether-midnight diagnose
aether-midnight compact
```

For a raw source, `source_uri` is optional. Derived memories must cite a raw `source_id` from the same scope or provide a `source_uri`. Scopes must include a namespace prefix and colon. Time inputs are RFC 3339 and normalized to UTC.

`search` returns `{results, token_budget, estimated_tokens, truncated, token_estimator}`. Each result includes a source path, optional current line range and snippet, freshness, scope, generation/completeness, match reason, deterministic score components, and estimated token count. Stale or unavailable source results never include a snippet or line range. `open-range` allows at most 200 lines and rejects absolute paths and `..` traversal.

## MCP

Run `aether-midnight mcp` with JSON-RPC messages over stdio. The implementation pins protocol version `2025-11-25`, advertises tools, and accepts `initialize`, `ping`, `tools/list`, and `tools/call`. It does not use a third-party MCP SDK. This is a small v1 tool-server surface, not a claim of support for unrelated MCP transports or protocol capabilities.

Available tools are `index_repository`, `search_code`, `open_range`, `list_symbols`, `repo_map`, `index_status`, `remember`, `recall`, `list_memories`, and `forget`. Every code tool receives `repo_path`; every memory tool receives `scope`.

## Node.js and Bun

The same no-dependency client example runs under Node.js and Bun and starts the Rust MCP process:

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
go run ./examples/go
```

`examples/go` contains a small Go module and JSON-RPC stdio client. It uses the same optional environment variables as the Node example. Both clients require the Rust executable to be built or installed separately.
