#!/bin/sh
# AETHER Midnight v0.1 user-local installer. macOS (Apple silicon and Intel). Never uses sudo.
#
# Usage:
#   install.sh [--archive-file FILE | --archive-url https://...] [--sha256 HEX]
#              [--prefix DIR] [--no-runtime] [--dry-run] [--help]
#
# With no source option it downloads the pinned Universal 2 archive and its .sha256 file
# from the public GitHub Release v$VERSION (see VERSION below). The archive SHA-256 is always checked before
# anything is extracted or installed. --sha256 overrides the downloaded checksum.
# This script is pinned to release VERSION below. It only accepts an archive named
# aether-midnight-VERSION-macos-universal2.tar.gz whose top-level directory has the same
# name, and the installed binary must report VERSION; any other release is refused.
#
# This script does NOT bypass Gatekeeper, does not touch quarantine attributes, does not
# edit shell profiles, and never claims the executable is signed or notarized.
set -eu
umask 022

VERSION=0.1.0-rc.1
APP=aether-midnight
ALIAS=midnight
ARCHIVE_NAME="$APP-$VERSION-macos-universal2"
RELEASE_BASE="https://github.com/ferxalbs/aether-midnight/releases/download/v$VERSION"
PIN_URL="$RELEASE_BASE/$ARCHIVE_NAME.tar.gz"

PREFIX="${AETHER_MIDNIGHT_PREFIX:-$HOME/.local}"
URL="$PIN_URL"
FILE=""
SHA=""
WANT_RUNTIME=1
DRY=0
TMP=""
STAGED=""

say() { printf '%s\n' "$*"; }
warn() { printf 'warning: %s\n' "$*" >&2; }
die() { # die CODE MESSAGE...
    code=$1; shift
    printf 'error: %s\n' "$*" >&2
    exit "$code"
}
cleanup() {
    [ -z "$STAGED" ] || rm -f "$STAGED"
    [ -z "$TMP" ] || rm -rf "$TMP"
}
trap cleanup EXIT
trap 'exit 130' INT TERM HUP

usage() { sed -n '2,15p' "$0" | sed 's/^# \{0,1\}//'; }

while [ $# -gt 0 ]; do
    case "$1" in
    --archive-file) [ $# -ge 2 ] || die 2 "$1 needs a value"; FILE=$2; shift 2 ;;
    --archive-url) [ $# -ge 2 ] || die 2 "$1 needs a value"; URL=$2; shift 2 ;;
    --sha256) [ $# -ge 2 ] || die 2 "$1 needs a value"; SHA=$2; shift 2 ;;
    --prefix) [ $# -ge 2 ] || die 2 "$1 needs a value"; PREFIX=$2; shift 2 ;;
    --no-runtime) WANT_RUNTIME=0; shift ;;
    --dry-run) DRY=1; shift ;;
    -h | --help) usage; exit 0 ;;
    *) die 2 "unknown option: $1 (see --help)" ;;
    esac
done

SOURCE_BUILD_HELP="Fallback: build from source (Rust 1.98+, Xcode Command Line Tools):
    git clone <repository> && cd aether-midnight
    cargo build --release --locked
    install -m 755 target/release/aether-midnight \"$PREFIX/bin/aether-midnight\"
  See docs/installation.md."

# ---- platform gate -------------------------------------------------------------------
[ "$(uname -s)" = Darwin ] || die 10 "unsupported OS $(uname -s). Only macOS is supported. $SOURCE_BUILD_HELP"
case "$(uname -m)" in
x86_64 | arm64) ;;
*) die 10 "unsupported architecture $(uname -m). Only macOS arm64 and x86_64 are supported. $SOURCE_BUILD_HELP" ;;
esac
[ "$(id -u)" -ne 0 ] || die 11 "do not run as root or with sudo; this installs into your home directory."
case "$PREFIX" in /*) ;; *) die 2 "--prefix must be an absolute path" ;; esac
for tool in shasum tar curl mktemp install; do
    command -v "$tool" >/dev/null 2>&1 || die 12 "required tool not found: $tool"
done

# ---- inputs --------------------------------------------------------------------------
if [ -n "$FILE" ] && [ "$URL" != "$PIN_URL" ]; then
    die 2 "pass only one of --archive-file and --archive-url"
fi
if [ -n "$FILE" ]; then
    [ -f "$FILE" ] || die 3 "archive not found: $FILE"
    URL=""
    [ -n "$SHA" ] || die 2 "--sha256 is required with --archive-file: refusing to install an unverified archive"
else
    case "$URL" in
    https://*) ;;
    *) die 2 "download URL must use https:// (use --archive-file for a local archive)" ;;
    esac
    case "$URL" in
    */"$ARCHIVE_NAME.tar.gz") ;;
    *) die 2 "this installer is pinned to v$VERSION: the URL must end in /$ARCHIVE_NAME.tar.gz" ;;
    esac
fi
if [ -n "$SHA" ]; then
    SHA=$(printf '%s' "$SHA" | tr 'A-F' 'a-f')
    case "$SHA" in
    *[!0-9a-f]*) die 2 "--sha256 must be 64 hex characters" ;;
    esac
    [ "${#SHA}" -eq 64 ] || die 2 "--sha256 must be 64 hex characters"
fi

BIN_DIR="$PREFIX/bin"
DEST="$BIN_DIR/$APP"

if [ "$DRY" -eq 1 ]; then
    say "dry run: would install $APP into $DEST"
    say "  alias:   $BIN_DIR/$ALIAS -> $APP (relative symlink; skipped if $ALIAS already exists and is not that link)"
    say "  source:  ${FILE:-$URL}"
    say "  sha256:  ${SHA:-downloaded from $URL.sha256}"
    if [ "$WANT_RUNTIME" -eq 1 ]; then say "  runtime: pinned ONNX Runtime via runtime install"; else say "  runtime: skipped"; fi
    exit 0
fi

# ---- fetch + verify ------------------------------------------------------------------
TMP=$(mktemp -d "${TMPDIR:-/tmp}/aether-midnight-install.XXXXXX") || die 12 "cannot create a temporary directory"
ARCHIVE="$TMP/archive.tar.gz"
if [ -n "$FILE" ]; then
    cp "$FILE" "$ARCHIVE" || die 3 "cannot read $FILE"
else
    say "Downloading $URL"
    curl --proto '=https' --tlsv1.2 -fsSL --retry 2 --connect-timeout 20 -o "$ARCHIVE" "$URL" ||
        die 4 "download failed (network, TLS or HTTP error). Nothing was installed. You can fetch the archive by other means and use --archive-file."
    if [ -z "$SHA" ]; then
        curl --proto '=https' --tlsv1.2 -fsSL --retry 2 --connect-timeout 20 -o "$TMP/archive.sha256" "$URL.sha256" ||
            die 4 "checksum download failed ($URL.sha256). Nothing was installed."
        SHA=$(awk 'NR==1 {print tolower($1)}' "$TMP/archive.sha256")
        case "$SHA" in
        *[!0-9a-f]* | "") die 5 "downloaded checksum file is not a SHA-256. Nothing was installed." ;;
        esac
        [ "${#SHA}" -eq 64 ] || die 5 "downloaded checksum file is not a SHA-256. Nothing was installed."
    fi
fi
ACTUAL=$(shasum -a 256 "$ARCHIVE" | cut -d' ' -f1)
if [ "$ACTUAL" != "$SHA" ]; then
    die 5 "SHA-256 mismatch: expected $SHA, got $ACTUAL. The archive is corrupt or not the pinned release. Nothing was installed."
fi
say "Archive SHA-256 verified."

# Refuse unsafe member names before extracting anything.
tar -tzf "$ARCHIVE" >"$TMP/list" 2>/dev/null || die 6 "archive is not a readable gzip tar. Nothing was installed."
if grep -E '(^/|(^|/)\.\.(/|$))' "$TMP/list" >/dev/null; then
    die 6 "archive contains absolute or parent-relative paths; refusing to extract."
fi
tar -tvzf "$ARCHIVE" 2>/dev/null | grep -E '^[lh]' >/dev/null &&
    die 6 "archive contains links; refusing to extract."
if grep -v -E "^$ARCHIVE_NAME(/|\$)" "$TMP/list" >/dev/null; then
    die 6 "archive is not release v$VERSION (expected every entry under $ARCHIVE_NAME/). Nothing was installed."
fi
mkdir "$TMP/x"
tar -xzf "$ARCHIVE" -C "$TMP/x" -o 2>/dev/null || die 6 "extraction failed. Nothing was installed."
ROOT="$TMP/x/$ARCHIVE_NAME"
[ -d "$ROOT" ] && [ ! -L "$ROOT/bin/$APP" ] && [ -f "$ROOT/bin/$APP" ] && [ -f "$ROOT/SHA256SUMS" ] || die 6 "archive layout is not an AETHER Midnight release (missing bin/$APP or SHA256SUMS)."
(cd "$ROOT" && shasum -a 256 -c SHA256SUMS >"$TMP/sums.log" 2>&1) || {
    grep -v ": OK$" "$TMP/sums.log" >&2 || true
    die 6 "internal SHA256SUMS check failed. Nothing was installed."
}
say "Archive contents verified against SHA256SUMS."

# ---- install (atomic, idempotent) ----------------------------------------------------
NEW_SHA=$(shasum -a 256 "$ROOT/bin/$APP" | cut -d' ' -f1)
mkdir -p "$BIN_DIR" || die 7 "cannot create $BIN_DIR"
if [ -f "$DEST" ] && [ "$(shasum -a 256 "$DEST" | cut -d' ' -f1)" = "$NEW_SHA" ]; then
    say "$DEST is already up to date; leaving it unchanged."
else
    STAGED="$BIN_DIR/.$APP.new.$$"
    install -m 755 "$ROOT/bin/$APP" "$STAGED" || { rm -f "$STAGED"; die 7 "cannot write to $BIN_DIR"; }
    mv -f "$STAGED" "$DEST" || die 7 "cannot replace $DEST"
    STAGED=""
    say "Installed $DEST"
fi

# ---- `midnight` alias: a relative symlink to the same binary ---------------------------
# The program picks its display name from argv[0], so no second binary is needed. An existing
# `midnight` that is not our link is never replaced.
ALIAS_PATH="$BIN_DIR/$ALIAS"
CMD=$APP
if [ -L "$ALIAS_PATH" ] && [ "$(readlink "$ALIAS_PATH")" = "$APP" ]; then
    say "$ALIAS_PATH already points to $APP."
    CMD=$ALIAS
elif [ -e "$ALIAS_PATH" ] || [ -L "$ALIAS_PATH" ]; then
    warn "$ALIAS_PATH already exists and is not a link to $APP; leaving it alone. Use '$APP' instead of '$ALIAS'."
elif ln -s "$APP" "$ALIAS_PATH"; then
    say "Linked $ALIAS_PATH -> $APP"
    CMD=$ALIAS
else
    warn "cannot create $ALIAS_PATH; use '$APP' instead of '$ALIAS'."
fi

# ---- Gatekeeper: report only, never bypass -------------------------------------------
if xattr -p com.apple.quarantine "$DEST" >/dev/null 2>&1; then
    warn "$DEST carries a quarantine attribute. This script does not remove it. If macOS blocks the program, follow Apple's documented override for software you trust, or build from source."
fi
if ! "$DEST" --version >"$TMP/ver" 2>"$TMP/ver.err"; then
    warn "the installed binary did not run: $(head -c 300 "$TMP/ver.err")"
    warn "If macOS (Gatekeeper) blocked it: this build is unsigned and not notarized. Nothing was bypassed. Build from source instead (docs/installation.md)."
    exit 8
fi
say "$(cat "$TMP/ver")"
case "$(cat "$TMP/ver")" in
*" $VERSION") ;;
*) warn "installed binary does not report version $VERSION (see above); this is not the pinned release."; exit 8 ;;
esac
ASSESS=$(spctl --assess --type execute "$DEST" 2>&1 || true)
if [ -n "$ASSESS" ]; then
    say "Gatekeeper assessment (informational): $ASSESS"
    say "This executable is not Developer ID signed or notarized."
fi

# ---- ONNX Runtime + doctor -----------------------------------------------------------
STATUS=0
if [ "$WANT_RUNTIME" -eq 1 ]; then
    say "Installing the pinned ONNX Runtime (verified download)..."
    if ! "$DEST" runtime install; then
        warn "runtime install failed; $APP is installed, lexical features work. Retry: $DEST runtime install"
        STATUS=9
    fi
fi
say "Running doctor..."
"$DEST" doctor || { warn "doctor reported failing required checks; see the output above."; [ "$STATUS" -ne 0 ] || STATUS=1; }

case ":$PATH:" in
*":$BIN_DIR:"*) ;;
*) say "Add to your shell profile (not done automatically): export PATH=\"$BIN_DIR:\$PATH\"" ;;
esac
say "Next: run '$CMD setup' once to choose your search provider, then '$CMD setup' again inside a repository to set up that project."
if [ "$STATUS" -eq 0 ]; then say "Done."; else say "Finished with warnings (exit $STATUS)."; fi
exit "$STATUS"
