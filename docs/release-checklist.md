# v0.1.0-rc.6 checklist and stable decision

rc.6 is the final planned candidate before deciding whether to release v0.1.0 stable. A passing
candidate workflow does not automatically authorize or establish stable readiness.

| rc.6 gate | State |
| --- | --- |
| MCP modes, provider consent, HTTP grants, offline overrides and failure fallback | PASS — implementation campaign; [evidence](hybrid-mcp-validation.md) |
| Literal-evidence ranking, frozen code/memory quality and paired diagnostics | PASS — deliberate code-order changes and slower cases documented in the evidence |
| Automated implementation checks | PASS — 210 tests, zero failures, three ignored; strict Clippy, formatting and release build |
| rc.6 version/installer/archive/notes consistency | PASS — manifest, lockfile, installer pin and workflow notes agree; `cargo check --locked` and installer shell syntax pass |
| Tagged rc.6 workflow, Universal 2 archive and published checksums | PENDING — not established by local source validation |
| Install and update from the published rc.6 assets | PENDING |
| Real MCP clients against the published rc.6 artifact | PENDING — record client/version and results privately; publish only reviewed aggregate conclusions |
| Runtime checks for every architecture claimed as supported | PENDING — packaging is not runtime validation |
| Signing/notarization and clean-machine installation expectations | OPEN — unsigned distribution remains documented; resolve the stable distribution decision explicitly |
| Live Rainy provider, if claimed as validated for stable | PENDING |

Do not promote rc.6 automatically: review these gates and [platform support](platforms.md), then
record the stable decision. No tag or release is created by editing this checklist.

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

## Publish the v0.1.0-rc.6 prerelease (maintainer)

Version, tag and archive name come from `Cargo.toml` (`0.1.0-rc.6`, tag `v0.1.0-rc.6`,
`aether-midnight-0.1.0-rc.6-macos-universal2.tar.gz`). `midnight update` and `scripts/install.sh`
both read the release by that tag, so they only work once it exists. The workflow creates it.

1. Write `docs/release-notes/v<version>.md` (state: unsigned, not notarized, what was and was not
   tested). The workflow appends the archive SHA-256 and fails if the file is missing.
2. Commit, then tag that exact commit and push the tag:
   `git tag v0.1.0-rc.6 && git push origin v0.1.0-rc.6`.
3. The "macOS release artifacts" workflow runs on the tag: it builds the Universal 2 binary,
   packages it, runs the offline installer smoke test, then a `release` job checks that the tag
   equals `v` + the `Cargo.toml` version, and creates the GitHub Release with the archive, its
   `.sha256` and `install.sh`. A version containing `-` (rc, beta, alpha) is published as a
   prerelease; a plain version is not. It refuses to touch a release that already exists.
4. Confirm the public URLs with `curl -fsSIL` and run `midnight update --check` from the prior build.

Re-running by hand: "Run workflow" on the tag ref does the same. Run on a branch it only builds and
uploads the artifact (no release). To fix the notes of a published release use `gh release edit`.

## Test the published artifact on a clean Intel Mac

Use a native Intel Mac with default Gatekeeper settings (App Store and identified
developers), no Rust, and a fresh user account. Do not change any security setting.

1. Browser path (quarantine): in Safari, download the `.tar.gz` and `install.sh` from the
   release page. Verify the quarantine flag exists:
   `xattr -p com.apple.quarantine ~/Downloads/install.sh` (expected: a value is printed).
2. `cd ~/Downloads && shasum -a 256 aether-midnight-0.1.0-rc.6-macos-universal2.tar.gz` must equal
   `<SHA256>` from the release notes.
3. `sh ./install.sh --archive-file aether-midnight-0.1.0-rc.6-macos-universal2.tar.gz --sha256 <SHA256> --no-runtime`.
   Record the exit code, the `--version` line (`aether-midnight 0.1.0-rc.6`) and the Gatekeeper
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
