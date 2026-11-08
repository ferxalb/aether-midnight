#!/bin/sh
# Reproducible archive assembly from the Universal 2 binary built by build-universal.sh.
set -eu
TZ=UTC
export TZ
cd "$(dirname "$0")/.."
BIN="${UNIVERSAL_BINARY:-$PWD/target/universal/aether-midnight}"
HARNESS="${HARNESS_BINARY:-$PWD/target/universal/midnight-harness-host}"
[ -f "$BIN" ] && [ -f "$HARNESS" ] || { echo 'run scripts/build-universal.sh first' >&2; exit 1; }
lipo -verify_arch "$BIN" arm64 x86_64
OUT="$PWD/target/release-candidate"
mkdir -p "$OUT"
STAGE=$(mktemp -d "$OUT/stage.XXXXXX")
"$HARNESS" license-inventory --output "$STAGE/inventory.json" > "$OUT/inventory-package.log"
cmp THIRD_PARTY_LICENSES.json "$STAGE/inventory.json"
VERSION=$(sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -n 1)
[ -n "$VERSION" ] || { echo 'cannot read version from Cargo.toml' >&2; exit 1; }
NAME=aether-midnight-$VERSION-macos-universal2
PKG="$STAGE/$NAME"
mkdir -p "$PKG/bin" "$PKG/docs"
cp "$BIN" "$PKG/bin/aether-midnight"
if strings "$PKG/bin/aether-midnight" | grep -F "$HOME/" >/dev/null; then
    echo 'Refusing to package a binary containing the build-user home path' >&2
    exit 1
fi
cp LICENSE README.md CHANGELOG.md HISTORY-TRACKER.md THIRD_PARTY_LICENSES.json "$PKG/"
sed -e 's|](release-signing.md)|](docs/release-signing.md)|g' -e 's|](embeddings.md)|](docs/embeddings.md)|g' -e 's|](privacy.md)|](docs/privacy.md)|g' docs/installation.md > "$PKG/INSTALL.md"
cp docs/*.md "$PKG/docs/"
cp -R examples "$PKG/"
"$HARNESS" license-notices --output "$PKG/licenses" > "$OUT/license-notices.log"
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
