# v0.1.0-rc.8 checklist and stable decision

rc.8 replaces rc.7 as the candidate for the v0.1.0 stable decision. It ships the v0.2 line (token-saving
search results, watcher, single-writer daemon, schema 10 memory provenance, symbol-aware embeddings)
as one release. A passing candidate workflow does not automatically authorize or establish stable
readiness. Notes: [rc.8](release-notes/v0.1.0-rc.8.md). rc.7 gates that this release touches are
re-opened below; its record follows in the archived table.

| rc.8 gate | State |
| --- | --- |
| rc.8 version/installer/notes consistency | PASS by inspection — manifest, lockfile, installer pin and `docs/release-notes/v0.1.0-rc.8.md` agree; `cargo check --locked` is the check |
| Formatting, strict Clippy, `cargo test --locked --no-fail-fast` on the final tree | PASS on Linux and macOS in CI for `1084416` (not rerun in the 2026-11-13 local campaign); Windows compiles but its test suite FAILS in CI (continue-on-error) |
| Frozen code/memory quality identical to rc.7 (`retrieval-quality`, four memory fixtures) | PASS — 2026-11-13, per-query ranks and top-10 identical (code n=51, five memory suites incl. v3); [performance](performance.md) |
| Token report vs the published rc.7 binary (paths/order identical, tokens and bytes) | PASS — paths/order identical; estimated tokens -27.3%, bytes -30.5% (estimate); 5 empty-result queries cost more |
| Watcher one-file visibility p95 ≤ 150 ms and idle CPU | PASS — p95 58.9 ms (n=30), idle about 0.001% of a core; one Intel macOS host |
| Daemon: N concurrent writers without `database is locked`, cold start, per-request overhead, RSS by daemon PID | PASS — 0 lock errors (also 0 without the daemon), cold start 139 ms, +0.11 ms p50 per request, current RSS 16.1 MiB (not peak) |
| Schema 9 → 10 migration, rollback, rc.7 refusal and JSONL compatibility | PASS — temporary copy built by the published rc.7 binary; 0.06 s (n=1, tiny database); large-database timing NOT MEASURED |
| Memory v3 recall, duplicate detection, trust and layer isolation | PARTIAL — run as `--split all` only: R@5 0.571, MRR 0.476, no leaks, duplicate detection 1 of 1; dev and held-out reported separately NOT RUN; cross-layer isolation NOT MEASURED |
| Semantic code v1, symbol chunk reuse and classified cloud retries | NOT MEASURED — lexical baseline only (identical to rc.7); semantic and hybrid need the local model or a mock mode the harness lacks |
| Local-model reuse and RSS | NOT MEASURED — deliberately not loaded in the 2026-11-13 run |
| Real Claude Code / Codex sessions with the compact shape, daemon and refs | NOT RUN (smoke only: MCP `initialize` and `tools/list` via daemon and `--no-daemon`) |
| Linux, Windows, npm builds and smoke tests | NOT RUN — experimental |
| Tagged rc.8 workflow, archive, checksums, install and rc.7 → rc.8 update | OPEN — maintainer publication below |
| Signing/notarization, arm64 runtime, live Rainy, clean-machine install | OPEN — unchanged |

The [validation report](part2-validation.md) lists the pending commands. Stable promotion requires a separate decision.

## rc.7 record (archived)

| rc.7 gate | State |
| --- | --- |
| Implementation checks (formatting, strict Clippy, `cargo test --locked`) | PASS — on Linux x86_64; one `cli_update` test fails intermittently while spawning a freshly copied binary, also on the rc.6 code |
| Frozen code and memory quality, identical responses on paired workloads | PASS — [performance](performance.md) sections 2026-11-11 and 2026-11-12 |
| rc.7 version/installer/notes consistency | PASS — manifest, lockfile, installer pin and `docs/release-notes/v0.1.0-rc.7.md` agree |
| Schema 7 → 9 migration and refusal by older binaries | PASS — fixture tests; downgrade advice in [installation](installation.md) |
| Native macOS paired measurements (code workload, KNN, local-model reuse) | OPEN — run the [rc.7 procedure](macos-benchmark-guide.md#rc7-validation) |
| Large real repository (exact-symbol and path-trigram gains, schema 9 size and index cost) | OPEN — same procedure |
| Real Claude Code / Codex sessions using default scope, `--with-instructions` and write-time embedding | OPEN — same procedure, last section |
| Tagged rc.7 workflow, archive, checksums, install and rc.6 → rc.7 update | OPEN — maintainer publication below |
| Signing/notarization, arm64 runtime, live Rainy, clean-machine install | OPEN — unchanged from rc.6 (below) |

Do not promote rc.7 automatically: review these gates, the open rc.6 gates and
[platform support](platforms.md), then record the stable decision. No tag or release is created by
editing this checklist.

## rc.6 campaign

These rc.6 results describe the rc.6 artifact. Gates that rc.7 does not touch (signing, arm64,
Rainy, clean-machine installation, default cloud hybrid availability) remain open for rc.7.

| rc.6 gate | State |
| --- | --- |
| MCP modes, provider consent, HTTP grants, offline overrides and failure fallback | PASS — implementation campaign; [evidence](hybrid-mcp-validation.md) |
| Literal-evidence ranking, frozen code/memory quality and paired diagnostics | PASS — deliberate code-order changes and slower cases documented in the evidence |
| Automated implementation checks | PASS — 210 tests, zero failures, three ignored; strict Clippy, formatting and release build |
| rc.6 version/installer/archive/notes consistency | PASS — manifest, lockfile, installer pin and workflow notes agree; `cargo check --locked` and installer shell syntax pass |
| Tagged rc.6 workflow, Universal 2 archive and published checksums | PASS — published workflow succeeded, archive checksum verified, installed binary matches the asset; [real-repository report](rc6-real-repo-validation.md) |
| Install and update from the published rc.6 assets | PASS in isolated prefixes — repeat installation and negative checks passed; real rc.5 → rc.6 update succeeded after an initial HTTP 504; clean-machine quarantine test remains open |
| Real MCP clients against the published rc.6 artifact | PARTIAL — real Claude Code/Codex CLI calls completed; repeated remote fallbacks and unconfirmed live HTTP hybrid remain open; Desktop GUI access was blocked |
| Runtime checks for every architecture claimed as supported | PARTIAL — native x86_64 real-repository and bounded 100k diagnostics passed; arm64 runtime not tested |
| Signing/notarization and clean-machine installation expectations | OPEN — ad-hoc verification passes, but no Developer ID/notarization and Gatekeeper rejects the artifact; clean-machine quarantined download not tested |
| Live Rainy provider, if claimed as validated for stable | PENDING — no Rainy credential available for this campaign |
| Real-repository structural extraction | PARTIAL — valid Rust cfg-attributed pattern in src/main.rs triggers the grammar's error flag; lexical completeness is preserved |
| Default cloud hybrid availability | OPEN — 10 of 17 authorized full-coverage remote-mode probe calls degraded with generic network/timeout notices; not a production failure-rate estimate |


## Historical local RC campaign

The following results predate rc.6. They provide background and do not certify the new artifact.

| Gate | Result |
| --- | --- |
| Provider optional fields and strict vector validation | PASS |
| HTTP 402 classified by status, no text heuristics | PASS |
| Failed batch preserves committed vectors and reports partial coverage | PASS |
| Namespace/provider/model isolation and privacy defaults | PASS |
| Formatting, strict Clippy, workspace tests, locked release build | PASS |
| Real local inference, hybrid search, model corruption rejection | PASS |
| Memory CRUD, indexing, MCP handshake, migration, cancellation | PASS |
| Frozen lexical quality suites | PASS |
| Fresh-process, warm batch, exact 10k/100k KNN, CPU/RSS | PASS |
| Reproducible archive, checksums, licenses, examples | PASS |
| Extraction and isolated local install outside repository | PASS |
| Live OpenRouter | PASS (one manual run, 2026-11-08) — see `docs/embeddings.md`; live Rainy is still unverified |
| Script install (`scripts/install.sh`): checksum, idempotency, tamper/path/platform/prefix failures | PASS — tested locally, isolated HOME |
| CLI signature / Gatekeeper | FAIL — unsigned; Gatekeeper rejects (script reports, never bypasses) |
| Microsoft runtime signature | PASS — upstream signature verifies |
| Notarization and hardened CLI + runtime loading | SKIP — no identity; not authorized |
| Version pin: installer refuses other versions (name, root dir, URL, `--version`) | PASS — tested locally |
| Public HTTPS download + quarantined browser download on a clean Intel Mac | PENDING — no published release yet |
| Other architectures / older macOS | SKIP — support not claimed |

Historical decision: **NO-GO for public announcement; GO to publish as a GitHub prerelease** once the
maintainer steps below are done. The binary is unsigned and not notarized (documented).
Detailed commands, evidence and measurements: [v0.1-status.md](v0.1-status.md).
Installation: [installation.md](installation.md). Signing: [release-signing.md](release-signing.md).

## Publish the v0.1.0-rc.8 prerelease (maintainer)

Version, tag and archive name come from `Cargo.toml` (`0.1.0-rc.8`, tag `v0.1.0-rc.8`,
`aether-midnight-0.1.0-rc.8-macos-universal2.tar.gz`). `midnight update` and `scripts/install.sh`
both read the release by that tag, so they only work once it exists. The workflow creates it.

1. Write `docs/release-notes/v<version>.md` (state: unsigned, not notarized, what was and was not
   tested). The workflow appends the archive SHA-256 and fails if the file is missing.
2. Commit, then tag that exact commit and push the tag:
   `git tag v0.1.0-rc.8 && git push origin v0.1.0-rc.8`.
3. The "macOS release artifacts" workflow runs on the tag: it builds the Universal 2 binary,
   packages it, runs the offline installer smoke test, then a `release` job checks that the tag
   equals `v` + the `Cargo.toml` version, and creates the GitHub Release with the archive, its
   `.sha256` and `install.sh`. A version containing `-` (rc, beta, alpha) is published as a
   prerelease; a plain version is not. It refuses to touch a release that already exists.
4. Confirm the public URLs with `curl -fsSIL` and run `midnight update --check` from the prior build.
5. From an installed rc.6, copy the database (`sqlite3 … ".backup …"`), run `midnight update`, then
   `midnight status` and `midnight memory scopes`: the first rc.8 run migrates to schema 10.

Re-running by hand: "Run workflow" on the tag ref does the same. Run on a branch it only builds and
uploads the artifact (no release). To fix the notes of a published release use `gh release edit`.

## Test the published artifact on a clean Intel Mac

Use a native Intel Mac with default Gatekeeper settings (App Store and identified
developers), no Rust, and a fresh user account. Do not change any security setting.

1. Browser path (quarantine): in Safari, download the `.tar.gz` and `install.sh` from the
   release page. Verify the quarantine flag exists:
   `xattr -p com.apple.quarantine ~/Downloads/install.sh` (expected: a value is printed).
2. `cd ~/Downloads && shasum -a 256 aether-midnight-0.1.0-rc.8-macos-universal2.tar.gz` must equal
   `<SHA256>` from the release notes.
3. `sh ./install.sh --archive-file aether-midnight-0.1.0-rc.8-macos-universal2.tar.gz --sha256 <SHA256> --no-runtime`.
   Record the exit code, the `--version` line (`aether-midnight 0.1.0-rc.8`) and the Gatekeeper
   assessment. Also run `xattr -l ~/.local/bin/aether-midnight` and record whether a quarantine
   attribute is present (the installer only reports it, never removes it). If macOS blocks it, record that exactly, do not override it, and mark the
   gate FAIL.
4. Direct HTTPS path (no browser): `curl -fsSLO <asset URL>` for `install.sh`, then
   `sh ./install.sh --archive-url <archive URL> --sha256 <SHA256>`. Confirm `Archive
   SHA-256 verified.`, `midnight doctor` (run by hand; the installer does not run it) passes required checks, and a second run prints
   `already up to date`.
5. Negative checks: run with a wrong `--sha256` (expect exit 5, nothing installed) and an
   `http://` URL (expect exit 2).
6. Record macOS version, CPU, results and any Gatekeeper dialog text. Do not remove
   quarantine, use `spctl --master-disable` or alter any system setting.

## v0.2 Part 2 gates

| Gate | Status |
| --- | --- |
| Additional ORT archive and member digests | PASS |
| Native Linux and Windows CI and installed runtime smoke | NOT RUN |
| Windows private ACLs and daemon named pipes | OPEN |
| npm trusted publishing setup and real registry publish | OPEN / NOT RUN |
| Apple notarization and Authenticode | NOT RUN |
