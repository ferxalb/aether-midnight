![AETHER Midnight — Find code. Recall context.](assets/aether-midnight-readme-bn.jpg)

# AETHER Midnight

**Local code search and explicit memory for AI agents, in a single binary.**

Midnight indexes your repositories on your machine so you, or an AI agent like Claude Code or Codex, can find the right file, line range, or symbol, with every result traced back to the source. It also keeps memories you save on purpose, so an agent can recall them later.

- **Private by default.** Keyword search runs fully offline: no server, account, API key, or model.
- **Built for agents.** One command registers it as an MCP server in Claude Code, Claude Desktop, or Codex.
- **Optional semantic search.** Use a local model, or a cloud provider you choose (OpenRouter or Rainy).

> **Status:** prerelease (`v0.1.0-rc.5`). Commands and output fields may still change. macOS is the supported platform; **Linux and Windows are experimental** and not stable (see [docs/platforms.md](docs/platforms.md)).

## Install

**macOS (Apple silicon or Intel).** No Rust or sudo needed:

```sh
curl --proto '=https' --tlsv1.2 -fsSL https://github.com/ferxalbs/aether-midnight/releases/download/v0.1.0-rc.5/install.sh | sh -s -- --no-runtime
```

This verifies the download's SHA-256 and installs `aether-midnight` to `~/.local/bin`, plus a shorter `midnight` alias. Add `~/.local/bin` to your `PATH` if it is not there yet. The binary is not signed or notarized; if macOS blocks it, see [docs/installation.md](docs/installation.md).

**From source (any platform).** Needs Rust 1.98+ and a C compiler. SQLite is bundled.

```sh
git clone https://github.com/ferxalbs/aether-midnight.git
cd aether-midnight
cargo build --release --locked   # binaries: target/release/midnight, target/release/aether-midnight
```

## Quick start

```sh
midnight setup              # choose keyword-only, local, or cloud search
cd ~/src/my-app
midnight index              # index this repository (and embed it, if you chose a model)
midnight search "retry budget"
midnight status             # configuration, storage, and index state
```

`index` shows live progress on a terminal. Run it again after you change files; there is no file watcher.

Output is readable text on a terminal and JSON when piped or with `--json`.

## Connect an AI client

```sh
midnight connect claude-code --scope project   # writes ./.mcp.json
midnight connect claude-code --scope user      # writes ~/.claude.json
midnight connect claude-desktop --scope user
midnight connect codex --scope user            # writes ~/.codex/config.toml
```

- `connect` merges into your existing config and keeps a `.midnight.bak` backup.
- `--dry-run` previews the change, `--read-only` exposes only search and recall tools, and `--remove` takes Midnight out again.
- Project config files call `midnight` from your `PATH`, so you can commit them.

The agent gets these MCP tools:

| Area | Tools |
| --- | --- |
| Code | `index_repository`, `refresh_indexed_file`, `search_code`, `open_range`, `list_symbols`, `repo_map`, `index_status` |
| Memory | `remember`, `remember_batch`, `recall`, `list_memories`, `forget` |

For any other MCP client, run `midnight mcp` over stdio:

```json
{ "mcpServers": { "aether-midnight": { "command": "/absolute/path/to/midnight", "args": ["mcp"] } } }
```

HTTP clients can use `midnight serve-http` (localhost only, bearer token from `midnight token create`). See [docs/dots-integration.md](docs/dots-integration.md).

## Search code

```sh
midnight search "payment timeout" --limit 5
midnight open-range --path src/payments.rs --start 20 --end 45
midnight list-symbols --name charge
midnight repo-map
```

- Commands work on the current directory; use `--repo PATH` for another one.
- Each result includes its path, line range, why it matched, and whether the index is fresh.
- Search matches whole words, paths, and symbol names, not arbitrary substrings.
- Symbols are extracted for Rust, Python, TypeScript, TSX, and JavaScript. Other files are searchable as text.
- Each repository and Git worktree has its own index.

## Remember and recall

```sh
# Save a source (id 1), then a fact that cites it (id 2)
midnight remember --scope project:payments --kind source \
  --content 'Payment policy: the retry budget is capped at 3 attempts.'
midnight remember --scope project:payments --kind derived \
  --content 'Retry cap is 3 attempts' --source-id 1

midnight recall --scope project:payments --query retry
midnight forget --scope project:payments --id 2
```

- Scopes need a prefix, such as `user:alice` or `project:my-app`. Recall never crosses scopes.
- Midnight never captures conversations on its own. It stores only what you or your agent save.
- `forget` hides a memory immediately. Run `compact` to remove its text from the database file.

## Semantic search (optional)

Pick a provider in `midnight setup`. After that, `index` embeds your code and `search` uses hybrid (keyword + semantic) ranking automatically.

| Provider | What happens |
| --- | --- |
| **Keyword only** (default) | Nothing leaves your machine. No model needed. |
| **Local** | Downloads a pinned ONNX Runtime and the `multilingual-e5-small` model (about 500 MB) once, then runs offline. Allow about 2 GiB of free RAM. |
| **OpenRouter / Rainy** | Sends the text being embedded (code chunks, memories, queries) to that provider, only after you consent in `setup`. The API key is stored with `0600` permissions, or read from `OPENROUTER_API_KEY` / `RAINY_API_KEY`. |

If embedding fails or is incomplete, results say so (`semantic_coverage`); Midnight never silently switches provider. Details are in [docs/embeddings.md](docs/embeddings.md).

## Per-project storage

By default all data lives in one database in your OS data directory. Run `midnight setup` inside a Git repository (after the first setup) to keep that project's data in `<repo>/.midnight/` instead. Midnight adds `.midnight/` to `.gitignore` and never indexes it.

Optional per-project settings go in `.midnight/midnight.json`:

```json
{ "version": 1, "embeddings": true, "mcp": { "read_only": false, "memory_scopes": ["project:my-app"] } }
```

- `embeddings: false` keeps the repository on keyword search.
- `mcp` limits what the MCP server exposes for this project.
- The file cannot set a provider, key, or upload consent, so a project can turn features off but never send your code anywhere.
- A `.midnight/` that arrives with a clone is ignored until you run `setup` in that repository. `midnight project list` and `midnight project untrust` manage which projects are trusted.

## Update and uninstall

```sh
midnight update --check    # see whether a newer release exists
midnight update            # download, verify, and replace the binary in place
midnight uninstall         # remove the program; your config and data stay
```

`update --to VERSION` installs a specific release. `uninstall --purge-config --purge-data` also deletes settings, keys, databases, and models; `--remove-client-entries` removes Midnight from your Claude Code, Claude Desktop, and Codex configs. Both commands accept `--dry-run`.

## Privacy

- No telemetry. Midnight goes online only when you install, update, download the local model, or use a cloud provider you selected.
- Data is stored in SQLite (macOS: `~/Library/Application Support/aether-midnight/`, Linux: `~/.local/share/aether-midnight/`, or `<repo>/.midnight/`). Files are private (`0600`) but **not encrypted**.
- Indexing respects `.gitignore` and `.aetherignore`, and skips dependencies, build output, secret-looking files (`.env*`, `*.pem`, `*.key`), binaries, and files over 1 MiB.

More in [docs/privacy.md](docs/privacy.md).

## Known limitations

- Two processes writing to the same database at once can fail with `database is locked`. Give each server process its own database.
- A project database does not see memories stored in the global database. Use global storage to share memories across projects.
- Token budgets are an estimate, not any specific model's token count.

## Documentation

- [API reference](docs/api.md): every command, MCP tool, and output field
- [Installation](docs/installation.md) · [Embeddings](docs/embeddings.md) · [Privacy](docs/privacy.md)
- [Performance](docs/performance.md) · [Platforms](docs/platforms.md) · [Architecture](docs/architecture.md)
- Contributing: see [AGENTS.md](AGENTS.md)

## License

[Apache License 2.0](LICENSE).
