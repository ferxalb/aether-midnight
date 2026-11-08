#!/bin/sh
# Reproducible archive assembly from the current, explicitly built binary.
set -eu
TZ=UTC
export TZ
cd "$(dirname "$0")/.."
[ "$(uname -sm)" = "Darwin x86_64" ] || { echo 'Only native macOS x86_64 is verified' >&2; exit 1; }
# Scrub build-user paths from Rust panic/source locations in distributed binaries.
RUSTFLAGS="${RUSTFLAGS:-} --remap-path-prefix=$PWD=/src/aether-midnight --remap-path-prefix=$HOME=/build-user"
export RUSTFLAGS
cargo build --release --locked
OUT="$PWD/target/release-candidate"
mkdir -p "$OUT"
STAGE=$(mktemp -d "$OUT/stage.XXXXXX")
target/release/midnight-harness license-inventory --output "$STAGE/inventory.json" > "$OUT/inventory-package.log"
cmp THIRD_PARTY_LICENSES.json "$STAGE/inventory.json"
VERSION=$(sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -n 1)
[ -n "$VERSION" ] || { echo 'cannot read version from Cargo.toml' >&2; exit 1; }
NAME=aether-midnight-$VERSION-macos-x86_64
PKG="$STAGE/$NAME"
mkdir -p "$PKG/bin" "$PKG/docs"
cp target/release/aether-midnight "$PKG/bin/"
if strings "$PKG/bin/aether-midnight" | grep -F "$HOME/" >/dev/null; then
    echo 'Refusing to package a binary containing the build-user home path' >&2
    exit 1
fi
cp LICENSE README.md CHANGELOG.md HISTORY-TRACKER.md THIRD_PARTY_LICENSES.json "$PKG/"
sed -e 's|](release-signing.md)|](docs/release-signing.md)|g' -e 's|](embeddings.md)|](docs/embeddings.md)|g' -e 's|](privacy.md)|](docs/privacy.md)|g' docs/installation.md > "$PKG/INSTALL.md"
cp docs/*.md "$PKG/docs/"
cp -R examples "$PKG/"
target/release/midnight-harness license-notices --output "$PKG/licenses" > "$OUT/license-notices.log"
(cd "$PKG" && find . -type f ! -name SHA256SUMS | LC_ALL=C sort | while IFS= read -r file; do shasum -a 256 "$file"; done > SHA256SUMS)
find "$PKG" -type d -exec chmod 755 {} +
find "$PKG" -type f -exec chmod 644 {} +
chmod 755 "$PKG/bin/aether-midnight"
# Normalize ownership, modes, entry order, timestamps, gzip header and macOS metadata.
find "$PKG" -exec touch -t 202611070000.00 {} +
(cd "$STAGE" && find "$NAME" | LC_ALL=C sort > "$STAGE/entries")
(cd "$STAGE" && COPYFILE_DISABLE=1 tar --format gnutar --uid 0 --gid 0 --uname root --gname wheel --no-recursion -cf "$OUT/$NAME.tar" -T "$STAGE/entries")
gzip -n -c "$OUT/$NAME.tar" > "$OUT/$NAME.tar.gz"
(cd "$OUT" && shasum -a 256 "$NAME.tar.gz" > "$NAME.tar.gz.sha256")
printf '%s\n' "$OUT/$NAME.tar.gz"
