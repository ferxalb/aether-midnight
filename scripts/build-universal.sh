#!/bin/sh
# Build the macOS Universal 2 (arm64 + x86_64) release binary. No credentials, no network
# use beyond fetching Rust targets and crates. Targets build sequentially with -j 1.
# Output: target/universal/aether-midnight and target/universal/midnight-harness-host
set -eu
cd "$(dirname "$0")/.."
RUSTFLAGS="${RUSTFLAGS:-} --remap-path-prefix=$PWD=/src/aether-midnight --remap-path-prefix=$HOME=/build-user"
export RUSTFLAGS
OUT="$PWD/target/universal"
rm -rf "$OUT"
mkdir -p "$OUT"
for t in aarch64-apple-darwin x86_64-apple-darwin; do
    rustup target add "$t"
    cargo build --release --locked -j 1 --target "$t" --bin aether-midnight
done
# Host-native harness, only used to check/emit license data during packaging.
cargo build --release --locked -j 1 --bin midnight-harness
cp target/release/midnight-harness "$OUT/midnight-harness-host"
lipo -create -output "$OUT/aether-midnight" \
    target/aarch64-apple-darwin/release/aether-midnight \
    target/x86_64-apple-darwin/release/aether-midnight
lipo -verify_arch "$OUT/aether-midnight" arm64 x86_64
[ "$(lipo -archs "$OUT/aether-midnight" | tr ' ' '\n' | LC_ALL=C sort | tr '\n' ' ')" = "arm64 x86_64 " ] ||
    { echo 'universal binary must contain exactly arm64 and x86_64' >&2; exit 1; }
# Ad-hoc signature only (required for arm64 to load); not Developer ID, not notarized.
codesign --force --sign - "$OUT/aether-midnight"
codesign --verify "$OUT/aether-midnight"
lipo -info "$OUT/aether-midnight"
# Smoke-test whichever slices can execute on this host.
HOST=$(uname -m)
for arch in arm64 x86_64; do
    if arch "-$arch" "$OUT/aether-midnight" --version >"$OUT/ver.$arch" 2>&1; then
        echo "slice $arch: $(cat "$OUT/ver.$arch")"
    elif [ "$arch" = "$HOST" ]; then
        echo "native slice $arch failed to run" >&2; exit 1
    else
        echo "slice $arch: cannot execute here (no Rosetta); structure verified by lipo only" >&2
    fi
    rm -f "$OUT/ver.$arch"
done
ls -l "$OUT/aether-midnight"
