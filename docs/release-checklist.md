# v0.1 RC checklist

Target: November 8, 2026, 08:00 America/Los_Angeles (16:00 UTC).
All artifacts stay local; all changes stay unstaged and uncommitted.

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
| Live Rainy and OpenRouter | SKIP — no configured environment credentials |
| Script install (`scripts/install.sh`): checksum, idempotency, tamper/path/platform/prefix failures | PASS — tested locally, isolated HOME |
| CLI signature / Gatekeeper | FAIL — unsigned; Gatekeeper rejects (script reports, never bypasses) |
| Microsoft runtime signature | PASS — upstream signature verifies |
| Notarization and hardened CLI + runtime loading | SKIP — no identity; not authorized |
| Version pin: installer refuses other versions (name, root dir, URL, `--version`) | PASS — tested locally |
| Public HTTPS download + quarantined browser download on a clean Intel Mac | PENDING — no published release yet |
| Other architectures / older macOS | SKIP — support not claimed |

Decision: **NO-GO for public announcement; GO to publish as a GitHub prerelease** once the
maintainer steps below are done. The binary is unsigned and not notarized (documented).
Detailed commands, evidence and measurements: [v0.1-status.md](v0.1-status.md).
Installation: [installation.md](installation.md). Signing: [release-signing.md](release-signing.md).

## Publish the v0.1.0 prerelease (maintainer, manual)

The release name and archive name are derived from `Cargo.toml` (`0.1.0`):
`aether-midnight-0.1.0-macos-x86_64.tar.gz`.

1. On a native Intel Mac, from a clean checkout of the release commit:
   `sh scripts/package-release.sh` twice; both `target/release-candidate/*.tar.gz.sha256`
   files must be identical. Record the SHA-256 (`<SHA256>`).
2. Create GitHub release tag `v0.1.0`, marked **pre-release**, with these assets:
   the `.tar.gz`, its `.tar.gz.sha256`, and `scripts/install.sh`. Put `<SHA256>` in the notes.
3. The asset URL is then
   `https://github.com/ferxalbs/aether-midnight/releases/download/v0.1.0/aether-midnight-0.1.0-macos-x86_64.tar.gz`.
   Confirm it with `curl -fsSIL <url>` before using it.
4. Optionally set `PIN_URL` and `PIN_SHA256` in `scripts/install.sh` to that URL and
   `<SHA256>`, then re-upload the edited `install.sh` (it is not inside the archive, so the
   archive hash does not change). Record `shasum -a 256 install.sh` in the notes.

## Test the published artifact on a clean Intel Mac

Use a native Intel Mac with default Gatekeeper settings (App Store and identified
developers), no Rust, and a fresh user account. Do not change any security setting.

1. Browser path (quarantine): in Safari, download the `.tar.gz` and `install.sh` from the
   release page. Verify the quarantine flag exists:
   `xattr -p com.apple.quarantine ~/Downloads/install.sh` (expected: a value is printed).
2. `cd ~/Downloads && shasum -a 256 aether-midnight-0.1.0-macos-x86_64.tar.gz` must equal
   `<SHA256>` from the release notes.
3. `sh ./install.sh --archive-file aether-midnight-0.1.0-macos-x86_64.tar.gz --sha256 <SHA256> --no-runtime`.
   Record the exit code, the `--version` line (`aether-midnight 0.1.0`) and the Gatekeeper
   assessment. Also run `xattr -l ~/.local/bin/aether-midnight` and record whether a quarantine
   attribute is present (the installer only reports it, never removes it). If macOS blocks it, record that exactly, do not override it, and mark the
   gate FAIL.
4. Direct HTTPS path (no browser): `curl -fsSLO <asset URL>` for `install.sh`, then
   `sh ./install.sh --archive-url <archive URL> --sha256 <SHA256>`. Confirm `Archive
   SHA-256 verified.`, `doctor` passes required checks, and a second run prints
   `already up to date`.
5. Negative checks: run with a wrong `--sha256` (expect exit 5, nothing installed) and an
   `http://` URL (expect exit 2).
6. Record macOS version, CPU, results and any Gatekeeper dialog text. Do not remove
   quarantine, use `spctl --master-disable` or alter any system setting.
