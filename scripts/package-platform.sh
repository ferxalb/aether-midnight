#!/bin/sh
set -eu
platform=$1
source_binary=$2
out=$3
version=$(sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -n 1)
name=aether-midnight-$version-$platform
case "$platform" in
windows-*) binary=aether-midnight.exe ;;
linux-*-gnu) binary=aether-midnight ;;
*) exit 2 ;;
esac
stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
mkdir -p "$stage/$name/bin" "$out"
install -m 755 "$source_binary" "$stage/$name/bin/$binary"
cp LICENSE THIRD_PARTY_LICENSES.json "$stage/$name/"
(cd "$stage/$name" && sha256sum "./bin/$binary" > SHA256SUMS)
tar -C "$stage" -czf "$out/$name.tar.gz" "$name"
(cd "$out" && sha256sum "$name.tar.gz" > "$name.tar.gz.sha256")
