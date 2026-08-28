#!/usr/bin/env bash
#
# Builds the desktop application for Windows.
#
#   scripts/build-windows.sh                 # portable .exe + zip
#   scripts/build-windows.sh --installer     # also builds the NSIS installer
#
# ## Native, not cross-compiled
#
# This script expects to run on Windows (Git Bash, MSYS2) or in a Windows CI runner.
# Cross-compiling a Slint GUI from Linux to `*-pc-windows-msvc` needs the MSVC toolchain
# and the Windows SDK, which cannot be redistributed; the `*-windows-gnu` target avoids
# that but produces a binary that behaves differently around DPI awareness and font
# rendering. Building on the platform is the honest option, and CI has Windows runners.
#
# See docs/CROSS_COMPILATION.md for the full reasoning and the split by platform.

set -euo pipefail

cd "$(dirname "$0")/.."

TARGET="${TARGET:-$(rustc -vV | awk '/^host:/ {print $2}')}"
OUT_DIR="${OUT_DIR:-dist/windows}"
APP="vds-admin"
VERSION="$(awk -F'"' '/^version/ {print $2; exit}' Cargo.toml)"
WITH_INSTALLER="no"

say() { printf '\033[1m==>\033[0m %s\n' "$*"; }
warn() { printf '\033[33mwarning:\033[0m %s\n' "$*" >&2; }
die() { printf '\033[31merror:\033[0m %s\n' "$*" >&2; exit 1; }

while [ $# -gt 0 ]; do
    case "$1" in
        --target) TARGET="${2:-}"; shift 2 ;;
        --installer) WITH_INSTALLER="yes"; shift ;;
        --out) OUT_DIR="${2:-}"; shift 2 ;;
        -h|--help) sed -n '2,18p' "$0"; exit 0 ;;
        *) die "unknown option: $1" ;;
    esac
done

case "$TARGET" in
    *windows*) ;;
    *) die "$TARGET is not a Windows target. Run this on Windows, or pass --target." ;;
esac

say "Building $APP $VERSION for $TARGET"
rustup target add "$TARGET" >/dev/null 2>&1 || true
cargo build --release --target "$TARGET" --package vds-admin

BINARY="target/$TARGET/release/$APP.exe"
[ -f "$BINARY" ] || die "expected $BINARY but it is not there"

mkdir -p "$OUT_DIR"

# --- portable ---------------------------------------------------------------------
#
# A single .exe that runs from a USB stick and writes its database next to the user's
# profile, not next to itself — an installer-free copy must not need a writable
# directory to start.
say "Packaging the portable build"
stage="$(mktemp -d)"
install -D -m 0755 "$BINARY" "$stage/$APP-$VERSION/$APP.exe"
install -D -m 0644 README.md "$stage/$APP-$VERSION/README.md"

if command -v 7z >/dev/null 2>&1; then
    ( cd "$stage" && 7z a -bso0 "$OLDPWD/$OUT_DIR/$APP-$VERSION-$TARGET-portable.zip" "$APP-$VERSION" )
elif command -v zip >/dev/null 2>&1; then
    ( cd "$stage" && zip -qr "$OLDPWD/$OUT_DIR/$APP-$VERSION-$TARGET-portable.zip" "$APP-$VERSION" )
else
    warn "neither 7z nor zip is available; copying the bare .exe instead"
    install -m 0755 "$BINARY" "$OUT_DIR/$APP-$VERSION-$TARGET.exe"
fi
rm -rf "$stage"

# --- installer ---------------------------------------------------------------------
if [ "$WITH_INSTALLER" = "yes" ]; then
    if command -v makensis >/dev/null 2>&1; then
        say "Building the NSIS installer"
        makensis -DVERSION="$VERSION" -DBINARY="$PWD/$BINARY" -DOUTDIR="$PWD/$OUT_DIR" \
            packaging/windows/installer.nsi
    else
        warn "makensis is not installed; skipping the installer"
        warn "  https://nsis.sourceforge.io/Download"
    fi
fi

say "Writing checksums"
(
    cd "$OUT_DIR" || die "$OUT_DIR disappeared"
    # An empty directory is not an error here: a format may have been skipped
    # for a missing tool, and the checksum file is then legitimately empty.
    sha256sum ./* > SHA256SUMS 2>/dev/null || true
)

say "Done:"
ls -lh "$OUT_DIR"
