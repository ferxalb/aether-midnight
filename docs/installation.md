# Install the v0.1 release candidate

Only **macOS x86_64** is verified. This candidate was tested on the current macOS host;
older macOS versions, Apple silicon, Linux and Windows are not certified. The CLI is
unsigned and unnotarized: Gatekeeper rejects it. This is a local review candidate,
not an approved public download. See [release-signing.md](release-signing.md).
Do not disable Gatekeeper or remove quarantine to install it.

For a locally built or subsequently signed and approved archive:

```sh
shasum -a 256 -c aether-midnight-0.1.0-rc1-macos-x86_64.tar.gz.sha256
tar -xzf aether-midnight-0.1.0-rc1-macos-x86_64.tar.gz
cd aether-midnight-0.1.0-rc1-macos-x86_64
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
