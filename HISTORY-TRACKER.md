# History Tracker

Development history of unreleased work, not a release record (releases go in `CHANGELOG.md`).

## 2026-11-08 (16) — v0.1.0-rc.4 published automatically; validated on the installed release

- The `v0.1.0-rc.4` tag push ran the workflow and its new `release` job created the prerelease with the archive, `.sha256` and `install.sh`. Verified against the public URLs: checksums, the installed `~/.local/bin/aether-midnight` is byte-identical to the published binary (SHA-256 prefix `98fe9e4483a5ff2c`), universal (`x86_64 arm64`), the README command run with an isolated `HOME` installs, links `midnight`, reports `0.1.0-rc.4` and re-runs idempotently, `update --check` reports up to date.
- **Real database migration:** the maintainer's global database (schema 6, 97 files embedded with OpenRouter) migrated in place to schema 7 on first use, still `97/97` embedded. A backup was taken first.
- **Real clients, installed binary, project created with the product's own `connect`:** Claude Code 2.1.319 (`claude -p --mcp-config`, a real model run, 4 turns, about USD 0.08) connected, listed the 12 tools and called `index_repository {}` and `search_code {query, limit}` without `repo_path`, correct results. Codex CLI (`codex exec`, MCP server set by `-c`) completed `index_status` and `search_code` without `repo_path`. Claude Code left the project-scope `.mcp.json` as "pending approval" (its trust prompt) until approved interactively. Claude Desktop was not run. In both runs approval was pre-granted or disabled, so auto-approval by the read-only annotations was not observed.
- **Real OpenRouter (`qwen/qwen3-embedding-8b`), a Git clone of a real project, 384 files:** first `index` embedded 5,291 chunks in 259 s. Then: no change 0 embedded (0.37 s total); one line edited 1; two lines in different chunks 2; one line deleted mid-file 10 (later chunks shift); a new 30-line file 1 and a removed file dropped its chunks; a following unchanged run 0. Search saw the edits; a removed file no longer appears; editing without embedding made search report `383 of 384 indexed files embedded`.
- **Concurrency:** 3,579 MCP calls (`remember`, `recall`, `search_code`) on the same project database while a real `index` with embedding ran: 0 errors, no "database is locked". `search_code` median 3.1 ms. One scenario, project-local database; two long writers were not tried.
- **Open findings, not fixed:** (1) Retrieval quality: on that project, the query `zxqv-unique-marker-alpha` (a rare token present in exactly one file plus three common words) ranks that file 10th lexically and outside the top 10 in `hybrid` and `semantic`, while `zxqv` alone ranks it first; a new file found by an exact phrase was also not first. Pre-existing (retrieval untouched since rc.3, `retrieval_tests` passes); needs the retrieval-change gate and paired numbers. (2) `search_code` with `limit` 3 estimated 1,854 tokens (default budget 2,048), so results are large for an agent. (3) A file that fails to parse keeps `complete: false`.

## 2026-11-08 (15) — Version 0.1.0-rc.4 and automatic prerelease

- `Cargo.toml`, `Cargo.lock`, `scripts/install.sh`, README, `docs/installation.md`, `docs/release-checklist.md`, `CHANGELOG.md` and the `docs/v0.1-status.md` banner now name `0.1.0-rc.4`. The README and installation URLs point at a release that exists only after the tag is pushed and the workflow finishes.
- `.github/workflows/macos-release.yml` now also runs on a pushed `v*` tag and has a `release` job (tag refs only, `contents: write` for that job alone). After the build and installer smoke test it checks that the tag equals `v` + the `Cargo.toml` version, that the archive, `.sha256` and `install.sh` are present and consistent (and `install.sh` pins the same version), and that `docs/release-notes/<tag>.md` exists; then it creates the GitHub Release with the archive, checksum and installer, as a prerelease when the version contains `-`, appending the archive SHA-256 to the notes. It refuses to modify an existing release. A branch run still only builds and uploads the artifact. The guard exists because the first `v0.1.0-rc.2` tag once pointed at a commit whose version still said rc.1 (entry 8).
- `docs/release-notes/v0.1.0-rc.4.md` holds the notes, including the schema 7 upgrade warning and the correction of the rc.3 claim about incremental embedding. The notes of the published `v0.1.0-rc.3` release were left as they are.
- Evidence: the workflow YAML parses and `sh -n` passes on `install.sh`; the workflow itself has not run. No Rust, schema or retrieval change in this entry. The first real run will be the rc.4 tag.

## 2026-11-08 (14) — Fixes found testing rc.3: real chunk reuse, no `git` subprocess, repository root above the working directory

- **Correction to (11).** rc.3 did not deliver "re-embed only changed chunks". The trigger `code_files_embeddings_rehash` deleted every vector of a file as soon as `index` saw its content change, before `embed` could reuse anything. Measured on rc.3 with OpenRouter (copy of a real project's `src/`, 53 files, 213 chunks): no change → 0 embedded; editing one line → 19 embedded (the whole file). `tests/embed_incremental.rs` would have caught it and had not been run.
- **Schema 7.** `embeddings_reuse` parks a file's vectors when its content changes. They still leave `embeddings` immediately, so search sees the file as not embedded exactly as before; `embed` reuses a parked vector for every chunk whose text and model/revision/dimensions match and deletes the parked rows when it stores the file (the file-delete trigger clears them too). Migration 6→7 is one transaction; an rc.3 database upgrades in place (verified on a database rc.3 created, with live vectors). Consequence: an rc.2/rc.3 binary refuses a v7 database ("newer than this binary supports"). The expected schema numbers in tests, `CLAUDE.md` and `docs/architecture.md` moved from 6 to 7.
- Re-measured on the migrated rc.3 database: editing one line → `embedded 1, unchanged 212`, 0 parked rows left afterwards, a following run `embedded 0, unchanged 213`. Chunks are still fixed 40-line windows, so inserting or deleting lines shifts the later chunks, which re-embed.
- **`git` subprocess removed from the hot path.** Every code tool call ran `git rev-parse` (about 130 ms here, the macOS shim), which was the whole cost of `open_range`, `index_status` and `search_code` on a 383-file repository. `repo_scope` now reads `.git` (directory or `gitdir:` file, `commondir`, loose or packed refs, detached HEAD) and asks `git` only for layouts it declines (`GIT_DIR`, reftable, symbolic or missing refs). Same `repo_id`, `worktree_id`, branch and revision as rc.3 on the same repository (compared). Measured through MCP stdio on that repository, median of 10 calls after indexing: rc.3 release build 127.6 ms; this tree, debug build, 2.2 ms. Different builds, one machine, n=10, anecdotal. One behavior difference: a repository `git` would refuse as "dubious ownership" is now read instead of falling back to the directory path.
- **MCP default repository** now searches upward from the working directory for the nearest `.git`, but never at or above the home directory or `/`.
- Evidence: `cargo clippy --locked --all-targets -- -D warnings` clean; passed with `nice`, `-j2`: `--lib` (126 passed, includes `retrieval_tests`, the schema migrations and a test comparing the file reader with `git rev-parse` for loose refs, packed refs, detached HEAD, subdirectory and a linked worktree), and the `mcp_stdio`, `mcp_http`, `cli_smoke`, `cli_journey`, `cli_cloud`, `cli_embeddings`, `migration_fixtures`, `embed_incremental` targets. Not run: the PTY, SIGINT and update targets, and no frozen-suite benchmark (retrieval ranking was not touched).
- Still open: the release notes of `v0.1.0-rc.3` repeat the claim corrected above. A single file that fails to parse (`tree_sitter_parse_error`) keeps a repository `complete: false` permanently.

## 2026-11-08 (13) — Version points to 0.1.0-rc.3

- `Cargo.toml`, `Cargo.lock`, `scripts/install.sh` (pinned `VERSION`), the README, `docs/installation.md`, `docs/release-checklist.md` and `CHANGELOG.md` now name `0.1.0-rc.3` (tag `v0.1.0-rc.3`, archive `aether-midnight-0.1.0-rc.3-macos-universal2.tar.gz`). The install URLs in the README and installation guide point at a release that does not exist until the maintainer tags and publishes it. Left as is on purpose: `docs/installation.md` `update --to 0.1.0-rc.1` example and the version strings in `src/update.rs` and `tests/cli_update.rs`, which are SemVer fixtures, and the earlier entries of this file.
- Contents of rc.3 relative to rc.2: embed re-sends only changed chunks (11), MCP version negotiation, tool annotations and optional `repo_path` (12), Linux and Windows documented as experimental. No build or test was run for this change.

## 2026-11-08 (12) — MCP: version negotiation, tool annotations, default repository

- `initialize` used to answer 2025-11-25 whatever the client asked, which can make a client pinned to an older revision drop the connection. It now echoes the requested version when it is one of 2025-11-25, 2025-06-18, 2025-03-26, 2024-11-05 and otherwise offers 2025-11-25. Only the `tools` surface exists, which these revisions share. The later stateless revision is still not implemented.
- Every tool now has `annotations` (`readOnlyHint`, `openWorldHint: false`, `destructiveHint` only for `forget`, `idempotentHint` for `index_repository`, `refresh_indexed_file`, `forget`) so clients can skip confirmation for read-only tools. This is a hint to the client, not an enforcement; the `Policy` is unchanged.
- `repo_path` is optional on code tools: default is the policy's single root, else the working directory only if it is a Git repository root. A non-repository directory (a GUI client's `/` or home) is never used; the call fails asking for `repo_path`. Explicit paths are never replaced and policy roots are still checked after defaulting. Tool schemas no longer list `repo_path` as required and the server `instructions` say so once.
- Evidence: `cargo clippy --locked --all-targets -- -D warnings` clean; new unit tests in `src/mcp.rs` (older-version echo, annotations, default repo) were NOT run. No real MCP client (Claude Code, Claude Desktop, Codex) has been tried; that is for rc.3. No retrieval, schema or performance change.

## 2026-11-08 (11) — Re-embed only the chunks that changed

- Found: the stored hash of each code chunk was salted with the whole-file hash, so editing one line changed every chunk hash and `embed` re-embedded (and, for cloud providers, re-uploaded) the entire file. The chunk hash is now computed from the chunk text only, and a changed file reuses the stored vector of every chunk whose text is identical, wherever it sits in the file; only new or edited chunks go to the embedder. A file whose chunks all match is rewritten without any provider call (line ranges are refreshed). Reused vectors are checked against the model's dimension count.
- Known limit: chunks are fixed 40-line/2000-byte windows, so inserting or deleting lines shifts every later chunk and those still re-embed; edits that keep line counts re-embed only the touched chunks. Content-defined chunking would fix this and is not done.
- One-time cost: stored hashes from earlier versions no longer match, so the first `embed` after upgrading re-embeds each repository once.
- Indexer: the per-file lookup of the stored size/mtime row now uses a cached prepared statement (same query, same results). Indexing already skipped unchanged files by size and mtime without reading them, and wrote only changed files in their own transactions; that is unchanged.
- Docs: `docs/v0.1-status.md` marked superseded; `docs/platforms.md` and the README now state Linux and Windows are experimental and not stable.
- Evidence: `cargo check --locked` for the library and the new `tests/embed_incremental.rs` only. The test and the existing suites were NOT run (deliberately, to be exercised with the rc.3 build); no performance or cost figure is claimed.

## 2026-11-08 (10) — Concurrent remote code embedding

- Repository embedding now gathers chunks across files and sends up to four remote requests in parallel by default. `MIDNIGHT_EMBED_CONCURRENCY` tunes the bounded concurrency from 1 to 8. Responses retain input order and each completed file is persisted in its own transaction, so later runs can resume from stored files.
- The behavior and tuning are documented in `docs/embeddings.md`. `cargo check --locked --all-targets` passed; no live-provider latency claim was made because no real-provider measurement was run.

## 2026-11-08 (9) — v0.1.0-rc.2 published; first real `update`

- Prerelease `v0.1.0-rc.2` published from the workflow run on the corrected tag (commit `4846dc9`): archive, `.sha256` and `install.sh`. Verified before publishing: checksum, installer against the artifact (quiet output, link created), reported version `0.1.0-rc.2`, `update --force` present, universal binary (`x86_64`, `arm64`).
- First real in-place upgrade: the maintainer's installed rc.1 ran `midnight update --check` and `midnight update` against the public release and became rc.2 (atomic swap, `midnight` link intact, no staging files left, saved provider settings untouched).
- Still unverified: Rainy, the arm64 slice at run time, and the browser-quarantine download on a clean Mac.

## 2026-11-08 (8) — v0.1.0-rc.2 version bump

- The first `v0.1.0-rc.2` tag pointed at a commit whose `Cargo.toml` still said `0.1.0-rc.1`; its workflow run produced `aether-midnight-0.1.0-rc.1-*` archives, which would have been indistinguishable from the published rc.1 for `update`. Nothing from that run was published. The version is now `0.1.0-rc.2` (`Cargo.toml`, `Cargo.lock`, `scripts/install.sh`, README and installation docs, release checklist) and the unpublished tag is moved to the commit that carries it.
- Contents of rc.2 relative to rc.1: live progress for `index`, the five-at-a-time text-model picker, `update --force`, a quiet installer (no Gatekeeper assessment, no `doctor`), isolated test configuration, and the recorded live OpenRouter verification.

## 2026-11-08 (7) — Live OpenRouter verified (manual)

- First live cloud run, on the published `v0.1.0-rc.1` binary: `setup` with OpenRouter and `qwen/qwen3-embedding-8b` (4096 dimensions, validation accepted, consented upload), `index` on a real 186-file project embedded 607 chunks, and `search` returned hybrid-fused results with `semantic coverage: 186/186 files embedded`, including Spanish text. The same run showed that `index` printed nothing while working (fixed in (5)) and that the model list offered an image-only model first (fixed in (6)).
- Not covered: Rainy, live failure paths, incremental re-embedding against the live service, latency and cost. Raw output and project content are not recorded here. Checklist rows in `docs/release-checklist.md`, `docs/v0.1-status.md` and `docs/embeddings.md` updated.

## 2026-11-08 (6) — Model picker: five at a time, text models only

- The OpenRouter model step of `setup` listed the first 15 models alphabetically, with image-only models such as `amazon/titan-embed-image-v1` first (and as the Enter default). It now hides image-only models (`image`, `clip`, `vision` in the id), puts the suggested text models first (`openai/text-embedding-3-small|large`, `qwen/qwen3-embedding-8b|4b`, `google/gemini-embedding-001`, `baai/bge-m3`, only when the provider really lists them), then the rest alphabetically, and shows five at a time: `m` reveals the next five, a number picks one, and any id can be typed. The first entry is the Enter default. The vector-size prompt pre-fills the documented size for the suggested models (e.g. 4096 for `qwen/qwen3-embedding-8b`).
- Evidence: `config::model_options` unit test and a wizard test with a mock listing seven text models plus an image model (`tests/cli_journey.rs`). The suggested sizes come from the models' public documentation and were not checked against the live provider in this change.

## 2026-11-08 (5) — Live progress for `index`

- `midnight index` was silent while indexing and while uploading embeddings. On a terminal it now shows a spinner with a live count of files scanned and elapsed time while indexing, then `Embedding with <provider> (<model>) · done/total files (n%) · new/unchanged chunks · elapsed` redrawn in place, then summary lines (`Indexed N files in Xs`, `Embedded N files in Xs`). Not a terminal (pipes, CI): plain start and finish lines. `--json`: nothing extra. All of it goes to stderr, so stdout stays the report.
- Implementation: `src/progress.rs`; `embed::embed_code_with_progress` / `ux::embed_repository_with_progress` (the old functions delegate with a no-op); one relaxed atomic increment per scanned file in the indexer (`code::files_scanned`), used only for display. No change to indexing results, retrieval, schema or stored data; the performance numbers in `docs/performance.md` were not re-measured for the extra increment.
- Test isolation fix: `cli_smoke`, `cli_sigint`, `cli_embeddings` and `cli_cloud` did not redirect `AETHER_MIDNIGHT_CONFIG_DIR`, so a developer who had run `midnight setup` on the same machine changed their results (a saved provider turned legacy `search` into hybrid). They now point at a non-existent config directory.
- Evidence: `tests/cli_pty.rs` (terminal run), `tests/cli_journey.rs` (cloud mock: plain lines on stderr, none on stdout), manual capture of the raw terminal stream.

## 2026-11-08 (4) — Quiet installer

- `scripts/install.sh` no longer prints the Gatekeeper assessment or runs `doctor`. It still verifies the archive, installs, confirms the binary runs and reports the pinned version, and only speaks about macOS when the program is actually blocked or the file carries a quarantine attribute. `doctor` is for explicit use only. Docs updated (`docs/installation.md`, `docs/release-checklist.md`); the unsigned/not-notarized status stays documented there and in the release notes. The `install.sh` asset already attached to `v0.1.0-rc.1` still has the old output until it is re-uploaded.

## 2026-11-08 (3) — v0.1.0-rc.1 published; `update --force`

- Prerelease `v0.1.0-rc.1` published on GitHub Releases from the workflow run on the tag commit (`faf2afe`): archive, `.sha256` and `install.sh`. Verified against the public URL: `install.sh` via `curl`, checksum and internal `SHA256SUMS`, symlink, idempotent re-run, `update --check` and `--pre --dry-run` against the real GitHub API.
- `update --force` reinstalls the selected release even at the same version. Used to exercise the real download path: a locally built x86_64 binary replaced itself with the published Universal 2 binary through GitHub's real redirects, and the `midnight` link stayed intact. Unit and process tests in `tests/cli_update.rs`.
- Still unverified: live Rainy/OpenRouter embeddings, the arm64 slice at run time, and the browser-quarantine download on a clean Mac.

## 2026-11-08 (2) — Version 0.1.0-rc.1, `update` and `uninstall`

- Version is now `0.1.0-rc.1` (tag `v0.1.0-rc.1`, archive `aether-midnight-0.1.0-rc.1-macos-universal2.tar.gz`); `scripts/install.sh` is pinned to it. The workflow smoke test also checks the `midnight` link, `update --help`, `uninstall --dry-run` and installer idempotency.
- `update` (aliases `upgrade`, `self-update`; `--check`, `--pre`, `--dry-run`, `--to VERSION`): one request for the release list at `api.github.com`, SemVer selection (prereleases only with `--pre` or while running one), https-only GitHub hosts with every redirect hop re-validated, archive SHA-256 against the release `.sha256`, binary against the archive's `SHA256SUMS`, staged binary must report the expected version, then an atomic rename; cargo build outputs are refused. No new dependency (own SemVer comparison, the existing tar reader and TLS agent). Network use is documented in `docs/privacy.md`.
- `uninstall` removes the program and links resolving to it; `--purge-config` / `--purge-data` delete exact files (the config and data directories are the same folder on macOS), `--remove-client-entries` edits user-scope client configs, project `.midnight/` is never touched; `--dry-run`, `--yes`.
- Evidence: unit tests in `src/update.rs`/`src/uninstall.rs` and `tests/cli_update.rs` (real processes against a loopback stand-in for the GitHub API, including tampered checksums, wrong reported version, foreign asset hosts). The real GitHub redirect flow is verified only after the release exists. No retrieval, schema or performance change.

## 2026-11-08 (1) — Installer creates the `midnight` alias

- `scripts/install.sh` now creates `midnight`, a relative symlink to `aether-midnight` beside it (the program already derives its display name from `argv[0]`), so the archive and its checksums are unchanged. It is idempotent and never replaces an existing `midnight` that is not that link; in that case it warns and the final hint uses `aether-midnight`. `--dry-run` lists the alias.
- `connect --scope project` writes the name the user invoked (`midnight` through the symlink) instead of the canonicalized binary name. Evidence: a real-process test through a symlink in `tests/cli_connect.rs`; the installer was exercised against a locally built stand-in archive (fresh install, re-run, foreign file, foreign link) and `sh -n`; ShellCheck is not installed here and remains pending. No Rust retrieval, schema or performance change.

## 2026-11-07 (10) — Consolidation: project trust commands, status, PTY tests, docs

- `project list` / `project untrust [PATH]` manage the trust allow-list; `status` now reports project storage (global or project), trust, whether a leftover `.midnight/` is being ignored, and the effective `midnight.json` values, without creating anything.
- `tests/cli_pty.rs` drives the interactive `setup` and `connect` menus through a real pseudo-terminal (Unix); `tests/cli_connect.rs` covers list/untrust/status. `scripts/install.sh` prints the next step (`setup`) and stays non-interactive. Docs: `docs/api.md`, `docs/privacy.md`, `docs/architecture.md`.
- Decisions: a project database does not see global-database memories (documented; use global storage to share), Claude Desktop entries always use the absolute binary path, project-scope entries rely on `PATH`. `doctor` is unchanged. The installer change was only syntax-checked (`sh -n`); ShellCheck is still pending. No retrieval, schema or performance change.

## 2026-11-07 (9) — Layered project configuration and per-repository setup

- Correction to (8): `.mcp.json`/`.codex/config.toml` written with `--scope project` now use the bare command name and no `--db`, so they are portable; user-scope files keep the absolute binary path. `connect` pins a database only when `--db` is given (a server started inside a project finds `.midnight/` itself). Project-scope backups go to `.midnight/backups/`.
- Correction to (8): the "owned by the current user" check does not stop a database that arrives with a clone (clones are owned by the user). Project storage and `midnight.json` are now honored only for projects registered in `trusted_projects.json` in the config directory, written by `init --storage project`, `setup` and `connect --storage project`.
- New `.midnight/midnight.json` (`src/project.rs`): `embeddings` (false keeps the repo on keyword search for `index`/`search`) and `mcp.read_only` / `mcp.memory_scopes` (narrow the MCP policy; flags still apply). It has no provider, endpoint, key or consent field and rejects unknown fields, so it cannot redirect uploads.
- `setup` is split: user-wide (provider, key, consent) vs. per repository (storage with the project preselected, optional client registration, `midnight.json`), chosen automatically inside a Git repository once user-wide setup exists; `--global`, `--project`, `--storage`. Evidence: `tests/cli_connect.rs`, `src/project.rs`/`src/connect.rs` unit tests. The interactive menus and real client apps are not covered by automated tests. No retrieval, schema or performance change.

## 2026-11-07 (8) — `connect` and project-local storage

- New `connect <claude-code|claude-desktop|codex> --scope project|user [--storage global|project] [--read-only] [--dry-run] [--remove]` registers `aether-midnight` in the client's MCP config (JSON merge, or a line-wise edit of only our table in Codex TOML; invalid input is refused, previous contents are saved as `*.midnight.bak`, writes are atomic). Scope is asked on a terminal and required otherwise.
- `init --storage project` creates `<repo>/.midnight/` (0700), creates or extends `.gitignore` with `.midnight/`, and puts the database there. Without `--db` the CLI now uses the nearest `.midnight/midnight.sqlite3` above the working directory (regular file owned by the current user, else ignored), otherwise the OS data directory as before. `.midnight` joins the always-excluded VCS paths in the walker. Global storage stays the default.
- Evidence: `src/connect.rs` unit tests and `tests/cli_connect.rs` (real process, redirected HOME). Not verified against the real Claude Code, Claude Desktop or Codex apps. No retrieval, schema or performance change.

## 2026-11-07 (7) — `midnight` command and guided first run

- New `midnight` binary (same program as `aether-midnight`, `src/bin/midnight.rs` includes `src/main.rs`). New commands: `setup` (interactive wizard or scripted flags for Rainy, OpenRouter, local, lexical), `status`; `index [PATH]` embeds with the saved provider; `search` uses the saved mode (hybrid with a provider, else lexical). Preferences live in `config.json`; an API key typed into `setup` goes to `credentials.json` (0600, directory 0700), an environment variable always wins and is never copied. Remote uploads need consent recorded by `setup` (bound to provider, destination and model), `--yes` for one run, or a terminal prompt.
- Output: `index`/`search` print readable text on a terminal or with `--human`; piped output and `--json` keep the previous JSON, so existing automation is unchanged. Cloud and lexical flows never load ONNX.
- Evidence: `tests/cli_journey.rs` (8 real-process tests from a clean config directory against the loopback mock). Live Rainy/OpenRouter behavior, including OpenRouter model discovery (`GET {base}/embeddings/models`), is unverified: no credentials were available. No retrieval, ONNX or packaging change.

## 2026-11-07 (6) — v0.1.0 installer pin and release handoff

- `scripts/install.sh` is now pinned to `VERSION=0.1.0`: it accepts only `aether-midnight-0.1.0-macos-x86_64` archives (URL name, single top-level directory, installed `--version`), rejects link members, and removes its staged file on interruption. `scripts/package-release.sh` derives the archive name from `Cargo.toml` (the `-rc1` suffix is gone). Release-pin values stay empty until a release is published.
- `docs/release-checklist.md` now holds the manual prerelease publication and clean-Intel-Mac test procedure; the public-download gate is PENDING. ShellCheck was not available on the host and is still to be run. No code, retrieval or performance change.

## 2026-11-07 (5) — Script-first installation

- v0.1 is now distributed as a Rust CLI via `scripts/install.sh` (macOS x86_64, user-local, no sudo) instead of a signed app/PKG. The script requires an explicit archive SHA-256, verifies the bundled `SHA256SUMS`, rejects unsafe archive paths, installs atomically and idempotently, runs the pinned ONNX Runtime install and `doctor`, and never bypasses Gatekeeper or touches quarantine. Developer ID signing work is dropped from the v0.1 plan.
- Docs: `docs/installation.md` (script, Gatekeeper, source-build fallback), `docs/release-signing.md`, `docs/release-checklist.md`. The release pin in the script is empty until an archive is published. Clean-machine and quarantined-download behavior are not yet tested; no code, retrieval or performance change.

## 2026-11-07 (4) — v0.1 release candidate verification

- Corrected cloud response compatibility for omitted optional fields and classified HTTP 402 by status. Added ordering, redirect/privacy and recovered partial-index regression tests; local inference, retrieval, persistence and MCP/HTTP behavior are preserved.
- Prepared a reproducible local macOS x86_64 archive with checksums, instructions, examples and dependency notices. Clean-directory installation and real offline hybrid search passed. Gatekeeper rejects the unsigned CLI; the upstream runtime signature verifies. Public binary distribution remains NO-GO pending signing/notarization and hardened dynamic loading checks.
- Commands, sanitized measurements, limitations and release gates: `docs/v0.1-status.md`, `docs/release-checklist.md`, `docs/release-signing.md` and `docs/performance.md`. Raw evidence remains ignored and local; nothing is committed or published.

## 2026-11-07 (3) — v0.1 Phase 3 release hardening and cloud embeddings

- ONNX Runtime: pinned official 1.23.2 for macOS x86_64 with `runtime install|status` (explicit download, size + SHA-256 verified before extraction, named members only). `doctor` now reports runtime, model and credentials with remedies (optional checks never fail it). Model pinned to Hugging Face commit `614241f6…` with per-file SHA-256, verified at pull, `models verify`, `doctor` and load. Licence inventory gains ONNX Runtime, model weights and remote services (375 crates).
- Cloud embeddings over `rainy-sdk` 0.6.51 for both OpenRouter and Rainy-compatible endpoints: explicit `--provider/--model/--dimensions/--endpoint/--timeout-secs/--param`, env-only credentials, bounded redacted errors, response validation, no retry, space isolation by provider/model/endpoint+params/dimensions, queries never embedded in a mismatching space, failures degrade to lexical. Embedding stays explicit; MCP/HTTP unchanged. MSRV is now 1.98.
- Evidence: 120 tests pass (mock-server suites in `src/cloud/tests.rs`, `tests/cli_cloud.rs`); live-provider smoke skipped (no credentials). Exact KNN: +20 ms at 10k, +240 ms at 100k vectors, flat RSS (`docs/performance.md`). Lexical code untouched; frozen suites reproduce. Details, risks and platform scope: `docs/v0.1-status.md`, `docs/embeddings.md`.

## 2026-11-07 (2) — v0.1 Phase 2 local embeddings and hybrid retrieval

- Added `multilingual-e5-small` embeddings (FastEmbed 5.13, runtime-loaded ONNX Runtime), schema 6 `embeddings` table with cleanup triggers, exact KNN, deterministic RRF hybrid search, and CLI `models list|pull`, `embed`, `--mode lexical|semantic|hybrid` on `memory search` and `search`. Lexical code, defaults and JSON are unchanged; missing models degrade to lexical with an explicit notice.
- Closed Phase 1 blockers: real SIGINT test, migration tests against databases produced by real earlier builds, release build smoke. Frozen quality suites identical to Phase 1; paired index timing unchanged. Results, risks (ONNX Runtime packaging) and docs: `docs/v0.1-status.md`, `docs/embeddings.md`.

## 2026-11-07 (1) — v0.1 Phase 1 CLI

- Added `init`, `memory add|get|list|delete|search`, `doctor`, a global `--json` flag, a positional `search` query and `memory::get_memory`. Existing commands, MCP and retrieval code are untouched.
- Tests: `tests/cli_smoke.rs`; status in `docs/v0.1-status.md`.

## 2026-11-06 (4) — Final stabilization validation (no code change)

- Re-validated against Dots at the final integration state: `cargo fmt --check`, `clippy -D warnings`, `cargo test` (48 unit + 7 HTTP + 4 stdio, retrieval oracle/fixtures included) and the real-binary offline Dots E2E all pass. Retrieval code is untouched since v0.4.
- Measured (2,000 memories, mock model): HTTP recall p50/p95/p99 2.3/3.5/4.9 ms; startup 3.6 ms; RSS 9.1 MiB; idle CPU ≈ 0%. Matches the post-accept-poll baseline (2.2/3.0/7.2).

## 2026-11-06 (3) — HTTP accept latency

- Root cause of the CLI/HTTP recall gap: the non-blocking accept loop slept a fixed 15 ms between polls, adding 0–15 ms to every new connection (bimodal 1/8/15 ms in curl). The loop now `poll(2)`s the listener (100 ms cancellation interval, `libc` already a dependency). Retrieval and authorization code unchanged.
- Recall over HTTP p50/p95 5.4/7.3 → 1.6/2.1 ms (paired, 200 iterations, 2,000 memories).

## 2026-11-06 (2) — Authenticated HTTP MCP sessions

- `serve-http` now requires a bearer token on every request. `token create|list|revoke` manage `<db>.tokens.json` (SHA-256 only, 0600, flock + atomic replace); each token holds its own tool/memory-scope/repo-root policy, may expire, and is re-checked per request so revocation is immediate and survives restarts. Memory tools need `--memory-scope` or explicit `--all-scopes`; code tools need `--repo-root`. A token pinned to one literal scope has it injected into calls and any other scope is denied. `/mcp/<label>` is accepted as an alias (label carries no authority). Stdio `mcp` is unchanged and keeps its flags.
- Tests: `tests/mcp_http.rs` (401 matrix, revocation, expiry, no plaintext at rest/echo, cross-Dot forged scope/omission/forget, token-creation guards) and `src/auth.rs` unit tests. Retrieval code and frozen suites untouched.

## 2026-11-06 (1) — Dots integration

- Added `serve-http` (loopback MCP Streamable HTTP) and the shared permission `Policy` (read-only, tool allowlist, memory scopes, repo roots) for stdio and HTTP. Docs: `docs/dots-integration.md`. Tests: `tests/mcp_http.rs` (restart, scope denial, read-only, Host rejection, loopback-only bind, two-process concurrent writes).
- Evaluated and rejected a warm `MemorySession` pool: no measurable recall change.

## 2026-11-05 (10) — Restore exact recall scan default

- Restored the original exact recall scan default after interleaved tied/Zipf diagnosis; retained descending only for benchmark comparison, added per-call CPU/cache diagnostics and oracle/plan/mutation regressions. The historical p99 spike was not reproducible as a plan-specific failure.

## 2026-11-05 (9) — Descending FTS recall scan

- Reduced exact single-token recall sorting work with a descending FTS coroutine, preserving exhaustive oracle and frozen-suite results. Added tie/cutoff and filtered-winner regression coverage; native paired evidence and remaining v0.5 gates are in `docs/retrieval-v05.md`.

## 2026-11-05 (8) — v0.4 retrieval record

- Documented the v0.4 retrieval record (`docs/retrieval-v04.md`): verified FTS5 `bm25()`/rank-ordering internals in the bundled source, the decision record, seven rejected or reverted experiments, paired v0.3.1 measurements, and the pending native macOS gate.

## 2026-11-05 (7) — Schema 4 namespace counters and sorted staging

- Added schema 4 per-namespace active-memory counters (atomic migration, trigger-maintained), sorted retrieval stages inside FTS5 columns with a newest-first exact-tie rule, dropped the namespace posting-list walk for majority namespaces, and capped per-class staging at four terms; 100k one-namespace p50 fell from 77.7 to 59.7 ms on the broad-term workload with identical frozen-suite metrics.

## 2026-11-05 (6) — Class-staged exact top-k recall

- Replaced the single BM25 window with class-staged exact top-k recall, added an exhaustive BM25 oracle validated against FTS5 rank, new `exact_top_k` and `recency_window` coverage values, an opt-in `broad_terms=recent` shortcut (rejected as default after it dropped relevant dev evidence), and the `bench-retrieval` paired A/B command.

## 2026-11-05 (5) — Benchmark report redaction

- Redacted host, runtime, local-path, and account-cost identifiers from published benchmark reports while retaining measured result distributions.

## 2026-11-05 (4) — BM25 candidate order and coverage

- Reproduced middle-window evidence loss, made BM25 ranking the production candidate order, exposed bounded candidate coverage, and added positional, insertion-order, and MCP regressions.

## 2026-11-05 (3) — Recall phase profiling

- Added non-stripped recall phase profiling, SQLite statement and scan counters, and an alternating page-cache screen; measured FTS execution as the 100k broad-query bottleneck.

## 2026-11-05 (2) — Two-window one-term retrieval

- Added bounded two-window candidate retrieval for one-term queries; preserved existing dev and held-out retrieval metrics and reduced repeated 100k persistent recall p95 to 7.32–7.53 ms.

## 2026-11-05 (1) — WAL checkpoint outliers

- Correlated most >4 ms remember outliers with WAL checkpoint progress; tested 256/1000/4096-page thresholds and retained SQLite's 1000-page default.

## 2026-11-04 (8) — Direct-file MCP refresh measurement

- Measured direct-file MCP refresh at 12.7 ms p95 on a 10k-file fixture versus 379.0 ms for full reconciliation; freshness and final reconciliation passed.

## 2026-11-04 (7) — Persistent memory sessions and batch ingestion

- Added persistent MCP memory sessions, atomic bounded batch ingestion, scoped contentless memory FTS migration, and deterministic lexical reranking.

## 2026-11-04 (6) — Frozen 10k-background retrieval evaluation

- Added frozen 10k-background retrieval evaluation and paired persistent MCP CPU, latency, storage, and quality evidence.

## 2026-11-04 (5) — One-file index refresh

- Added explicit one-file index refresh with containment/security checks and full-reconciliation freshness semantics.

## 2026-11-04 (4) — Reverted three-term threshold

- Reverted a three-term retrieval threshold after held-out R@5 and evidence completeness regressed; retained the rejected-run artifact.

## 2026-11-04 (3) — Memory scaling benchmark

- Added isolated 1k/10k/100k persistent memory scaling benchmark and documented root-cause/query-plan findings.

## 2026-11-04 (2) — Parallel indexing

- Added bounded parallel indexing, cancellation-aware reads, identifier decomposition, and fixed-fixture benchmark evidence.

## 2026-11-04 (1) — Changelog and tracker

- Added release-only changelog and this tracker; v0.1.0 remains unreleased.

## 2026-11-03 (6) — FTS5 tokenizer selection

- Benchmarked SQLite FTS5 tokenizers on a fixed workload, selected `unicode61`, and added the index-format-2 migration.

## 2026-11-03 (5) — Agent memory operations

- Added explicit agent-memory operations with source provenance, scopes, validity periods, supersession links, and forget behavior.

## 2026-11-03 (4) — Stdio MCP interface

- Added the stdio MCP interface and Node.js, Bun, and Go client examples; exercised Node.js and Go integrations.

## 2026-11-03 (3) — Bounded incremental indexing

- Added bounded, incremental repository indexing with ignore rules, default exclusions, symlink containment, cancellation, and partial-index reporting.

## 2026-11-03 (2) — Source-grounded retrieval

- Added source-grounded retrieval for paths, symbols, lexical matches, ranges, repository maps, and index status, including freshness checks.

## 2026-11-03 (1) — Benchmarks, safety checks and docs

- Added reproducible benchmark tooling and results, integration and safety checks, and product, architecture, API, privacy, platform, and dependency documentation.
