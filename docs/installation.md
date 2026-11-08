# Install AETHER Midnight v0.1

v0.1 is distributed as a Rust CLI installed by a shell script. There is no macOS app,
PKG or Developer ID signing step. The release is one macOS **Universal 2** binary
(arm64 + x86_64, ad-hoc signed only). Linux, Windows and other architectures are
unsupported, and the script refuses to run there.

## Script install (recommended)

```sh
curl --proto '=https' --tlsv1.2 -fsSL https://github.com/ferxalbs/aether-midnight/releases/download/v0.1.0-rc.1/install.sh | sh -s -- --no-runtime
# offline: sh scripts/install.sh --archive-file aether-midnight-0.1.0-rc.1-macos-universal2.tar.gz --sha256 <hex>
```

`scripts/install.sh` (from the repository) runs as your user (it refuses root/sudo) and:

1. checks the platform, and requires `--sha256` (an unverified archive is never used);
2. downloads (HTTPS only, TLS 1.2+) or copies the archive, verifies its SHA-256, rejects
   absolute or `..` member paths, then verifies the bundled `SHA256SUMS`;
3. installs `aether-midnight` atomically into `~/.local/bin` (`--prefix DIR` changes the
   root); re-running with the same archive leaves the file unchanged;
4. runs `aether-midnight --version` to confirm the installed binary runs and is the pinned
   version, then `runtime install` (pinned ONNX Runtime 1.23.2, size and SHA-256 verified;
   skip with `--no-runtime`). It prints nothing else: run `midnight doctor` yourself whenever
   you want a health report.

The script never edits shell profiles, removes quarantine attributes, disables
Gatekeeper or uses `sudo`; add `~/.local/bin` to `PATH` yourself when it tells you to.
Before anything is written, a bad checksum, unreadable archive, unsafe path or
unwritable prefix aborts with a distinct exit code (2 usage, 3 file, 4 download,
5 checksum, 6 archive, 7 install location, 8 binary did not run, 9 runtime install
failed after a successful binary install, 10 unsupported platform, 11 root, 12 missing
tool). `--dry-run` prints the plan. Next to the binary it also creates `midnight`, a relative symlink
to `aether-midnight` (the program takes its display name from how it is invoked); an existing
`midnight` that is not that link is left untouched with a warning. With no source option the script downloads the
v0.1.0-rc.1 archive and its `.sha256` from the GitHub Release; `--sha256` overrides it.

### Update and uninstall

```sh
midnight update --check     # is a newer release published? (one request to api.github.com)
midnight update             # download, verify and install it (alias: upgrade, self-update)
midnight update --pre       # also consider prereleases (automatic while you run one)
midnight update --to 0.1.0-rc.1   # install an exact version, older ones included
midnight update --force     # reinstall the current version (repair a damaged binary)
midnight uninstall --dry-run
midnight uninstall          # removes the program and the `midnight` link only
midnight uninstall --purge-config --purge-data --remove-client-entries
```

`update` talks to GitHub only when you run it, accepts only `https` GitHub hosts (each redirect
hop is checked), requires the archive to match the release's `.sha256`, requires the binary
inside to match the archive's own `SHA256SUMS`, runs the staged binary to confirm its version, and
only then swaps it in with an atomic rename next to the old one. A development build under
`target/` is never overwritten. `uninstall` never deletes configuration or data unless you pass
`--purge-config` / `--purge-data`, never touches a project's `.midnight/`, and reports client
entries instead of editing them unless you pass `--remove-client-entries`. The user-wide
`config.json`, `credentials.json` and the project trust list are what `--purge-config` removes;
the database, token registry, models and ONNX Runtime are what `--purge-data` removes.

### Gatekeeper

The executable is **not Developer ID signed and not notarized**, and
`spctl --assess` rejects it. The installer stays quiet about this and never works around
it; it only says something if macOS actually blocks the program (or if the file carries a
quarantine attribute). A file copied or fetched by `curl` normally carries no quarantine attribute and runs
from Terminal; a browser download does carry one and macOS may block it. If the program
is blocked, either use Apple's documented per-app override for software you trust or
use the source build below. See [release-signing.md](release-signing.md).

### Source-build fallback

Needs Rust 1.98+ and the Xcode Command Line Tools (a locally built binary is not
quarantined):

```sh
cargo build --release --locked
mkdir -p "$HOME/.local/bin"
install -m 755 target/release/aether-midnight "$HOME/.local/bin/aether-midnight"
aether-midnight runtime install && aether-midnight doctor
```

## Manual install of the same archive

For a locally built or subsequently signed and approved archive:

```sh
shasum -a 256 -c aether-midnight-0.1.0-rc.1-macos-universal2.tar.gz.sha256
tar -xzf aether-midnight-0.1.0-rc.1-macos-universal2.tar.gz
cd aether-midnight-0.1.0-rc.1-macos-universal2
shasum -a 256 -c SHA256SUMS
mkdir -p "$HOME/.local/bin"
install -m 755 bin/aether-midnight "$HOME/.local/bin/aether-midnight"
export PATH="$HOME/.local/bin:$PATH"
aether-midnight --version
aether-midnight doctor
```

No Rust toolchain, Cargo, SQLite installation or cloud key is needed for the binary.
Lexical search is offline. For **local hybrid search**, make the two explicit downloads
with Internet access and system TLS trust available (about 12 MB runtime archive and
490 MB model files; allow extra space for extraction/cache). Use a writable local data
directory and allow about 2 GiB available RAM for inference:

```sh
BIN="$HOME/.local/bin/aether-midnight"
export AETHER_MIDNIGHT_MODELS_DIR="$HOME/.local/share/aether-midnight/models"
DB="$HOME/.local/share/aether-midnight/demo.sqlite3"
mkdir -p "$(dirname "$DB")"
$BIN runtime install
$BIN runtime status
$BIN models pull multilingual-e5-small
$BIN models verify
$BIN --db "$DB" init
$BIN --db "$DB" memory add --namespace project:demo 'La política de pagos permite tres reintentos.'
$BIN --db "$DB" embed --namespace project:demo
$BIN --db "$DB" memory search --namespace project:demo 'payment retry policy' --mode hybrid --json

# Replace this with your repository path. Re-run index and embed after editing files.
$BIN --db "$DB" index --repo /path/to/repository
$BIN --db "$DB" embed --repo /path/to/repository
$BIN --db "$DB" search --repo /path/to/repository --query 'retry policy' --mode hybrid
```

After setup, these local embed/search commands require no network. A fresh semantic CLI
process costs about four seconds and roughly 1.5 GiB peak RSS; inference is opt-in.
`degraded: true` means lexical fallback; `notice` and `semantic_coverage` report partial
semantic indexes. A failed `embed` exits nonzero; re-run it to resume. Never interpret
lexical fallback as successful semantic inference.

The managed runtime is pinned ONNX Runtime 1.23.2, dynamically loaded and rehashed before
use. It and its notices are installed by `runtime install`; weights are installed only
by `models pull`. Neither is included in the CLI archive. `ORT_DYLIB_PATH` is an advanced
external override with different integrity guarantees; omit it for the managed setup.
Cloud providers require explicit selection and send text off-device; see
[embeddings.md](embeddings.md) and [privacy.md](privacy.md).

Source builds need Rust 1.98+ and a C compiler (macOS Command Line Tools), then
`cargo build --release --locked`. Package assembly uses `scripts/package-release.sh`
on macOS x86_64. It collects locked dependency license texts and normalizes archive
order, ownership, mode, UTC timestamps and gzip metadata, and remaps build-user paths
in Rust diagnostics. Two runs with identical inputs
must hash identically. This guarantees archive reproducibility from the same binary,
not cross-toolchain or cross-directory bit-for-bit compilation.
