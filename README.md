![AETHER Midnight — Find code. Recall context.](assets/aether-midnight-readme.png)

# AETHER Midnight

**Local code search and explicit memory for AI agents, in a single binary.**

AETHER Midnight indexes a repository on your machine so you, or an AI agent, can find the right file, line range, or symbol, with every result traceable to the source. It also stores memories you save on purpose, kept apart by namespace, so an agent can recall them later.

Keyword search works fully offline: no server, account, API key, or model needed.

> **Status:** early development (`v0.1.0`). Commands and output fields may still change.

## Install

**macOS (Apple silicon or Intel).** No Rust or sudo needed. The script downloads the prebuilt binary, checks its SHA-256, and installs it to `~/.local/bin/aether-midnight`:

```sh
curl --proto '=https' --tlsv1.2 -fsSL https://github.com/ferxalbs/aether-midnight/releases/download/v0.1.0/install.sh | sh -s -- --no-runtime
```

Add `~/.local/bin` to your `PATH`. The binary is not signed or notarized, so macOS Gatekeeper may block it the first time; see [docs/installation.md](docs/installation.md).

**From source (any platform).** Needs Rust 1.98+ and a C compiler. SQLite is bundled.

```sh
git clone https://github.com/ferxalbs/aether-midnight.git
cd aether-midnight
cargo build --release --locked   # binary: target/release/aether-midnight
```

## Quick start

```sh
aether-midnight setup                     # choose keyword-only, local, or cloud search
aether-midnight index .                   # index this repository
aether-midnight search "retry budget"     # search it
aether-midnight status                    # show configuration and index state
```

Run `index` again after you change files; there is no file watcher.

## Search code

```sh
aether-midnight search --repo ~/src/payments "payment timeout" --limit 5
aether-midnight open-range --repo ~/src/payments --path src/payments.rs --start 20 --end 45
aether-midnight list-symbols --repo ~/src/payments --name charge
aether-midnight repo-map --repo ~/src/payments
```

- Each result includes its path, line range, why it matched, and whether the index is fresh.
- Search matches whole words, paths, and symbol names, not arbitrary substrings.
- Symbols are extracted for Rust, Python, TypeScript, TSX, and JavaScript. Other files are searchable as text.
- Each repository and Git worktree has its own index.

## Remember and recall

```sh
# Save a source (gets id 1), then a fact that cites it
aether-midnight remember --scope project:payments --kind source \
  --content 'Payment policy: the retry budget is capped at 3 attempts.'
aether-midnight remember --scope project:payments --kind derived \
  --content 'Retry cap is 3 attempts' --source-id 1

aether-midnight recall --scope project:payments --query retry
aether-midnight forget --scope project:payments --id 2
```

- Scopes need a prefix, such as `user:alice` or `project:my-app`. Recall never crosses scopes.
- Midnight never captures conversations on its own. It stores only what you tell it to.
- `forget` hides a memory immediately. Run `compact` to remove its text from the database file.

## Semantic search (optional)

Keyword search is the default. For meaning-based search, run `aether-midnight setup` and pick one:

- **Local:** downloads a pinned ONNX Runtime and the `multilingual-e5-small` model (about 500 MB), then runs offline. Allow about 2 GiB of free RAM.
- **Cloud (OpenRouter or Rainy):** sends the text being embedded (code chunks, memories, queries) to that provider. Nothing is sent unless you choose it.

```sh
aether-midnight embed --repo .
aether-midnight search "retry policy" --mode hybrid
```

Details are in [docs/embeddings.md](docs/embeddings.md).

## Use with AI agents (MCP)

Add Midnight to any MCP client:

```json
{
  "mcpServers": {
    "aether-midnight": {
      "command": "/absolute/path/to/aether-midnight",
      "args": ["mcp"]
    }
  }
}
```

| Area | Tools |
| --- | --- |
| Code | `index_repository`, `refresh_indexed_file`, `search_code`, `open_range`, `list_symbols`, `repo_map`, `index_status` |
| Memory | `remember`, `remember_batch`, `recall`, `list_memories`, `forget` |

For HTTP clients, `serve-http` serves the same tools on localhost only and requires a bearer token:

```sh
aether-midnight token create --name my-agent --memory-scope 'project:my-app'
aether-midnight serve-http
```

See [docs/dots-integration.md](docs/dots-integration.md).

## Privacy

- No telemetry. Network use happens only for the optional model download and a cloud provider you selected.
- Data lives in one SQLite file (macOS: `~/Library/Application Support/aether-midnight/`, Linux: `~/.local/share/aether-midnight/`). Use `--db PATH` to change it. The file is private (`0600`) but **not encrypted**.
- Indexing respects `.gitignore` and `.aetherignore`, and skips dependencies, build output, secret-looking files (`.env*`, `*.pem`, `*.key`), binaries, and files over 1 MiB.

More in [docs/privacy.md](docs/privacy.md).

## Known limitations

- Give each server process its own database; two writers on the same file can fail with `database is locked`.
- Token budgets are an estimate, not any specific model's token count.

## Documentation

- [API reference](docs/api.md): every command, MCP tool, and output field
- [Installation](docs/installation.md) · [Embeddings](docs/embeddings.md) · [Privacy](docs/privacy.md)
- [Performance](docs/performance.md) · [Platforms](docs/platforms.md) · [Architecture](docs/architecture.md)
- Contributing: see [AGENTS.md](AGENTS.md)

## License

[Apache License 2.0](LICENSE).
