#!/usr/bin/env bash
#
# Builds the macOS application bundle.
#
#   scripts/build-macos.sh              # universal .app + .dmg
#   scripts/build-macos.sh --arch arm64 # one architecture
#
# ## Universal by default
#
# Apple Silicon and Intel Macs are both in service, and a user who downloads the wrong
# one gets an error message that does not explain itself. `lipo` merges the two builds
# into a single binary that runs natively on both; it costs build time, not runtime.
#
# ## Signing and notarisation
#
# Unsigned, Gatekeeper refuses to open the app on first launch and offers no obvious way
# round it. This script signs when `MACOS_SIGNING_IDENTITY` is set and notarises when
# `MACOS_NOTARY_PROFILE` is too; otherwise it produces an unsigned bundle and says so,
# which is fine for a developer on their own machine and not fine for a release.

set -euo pipefail

cd "$(dirname "$0")/.."

OUT_DIR="${OUT_DIR:-dist/macos}"
APP="vds-admin"
DISPLAY_NAME="VDS Admin"
VERSION="$(awk -F'"' '/^version/ {print $2; exit}' Cargo.toml)"
ARCHS=("aarch64-apple-darwin" "x86_64-apple-darwin")

say() { printf '\033[1m==>\033[0m %s\n' "$*"; }
warn() { printf '\033[33mwarning:\033[0m %s\n' "$*" >&2; }
die() { printf '\033[31merror:\033[0m %s\n' "$*" >&2; exit 1; }

while [ $# -gt 0 ]; do
    case "$1" in
        --arch)
            case "${2:-}" in
                arm64|aarch64) ARCHS=("aarch64-apple-darwin") ;;
                x86_64|intel)  ARCHS=("x86_64-apple-darwin") ;;
                *) die "unknown architecture: ${2:-}" ;;
            esac
            shift 2 ;;
        --out) OUT_DIR="${2:-}"; shift 2 ;;
        -h|--help) sed -n '2,22p' "$0"; exit 0 ;;
        *) die "unknown option: $1" ;;
    esac
done

[ "$(uname -s)" = "Darwin" ] || die "the macOS bundle must be built on macOS"

for target in "${ARCHS[@]}"; do
    say "Building $target"
    rustup target add "$target" >/dev/null 2>&1 || true
    cargo build --release --target "$target" --package vds-admin
done

mkdir -p "$OUT_DIR"
BUNDLE="$OUT_DIR/$DISPLAY_NAME.app"
rm -rf "$BUNDLE"
mkdir -p "$BUNDLE/Contents/MacOS" "$BUNDLE/Contents/Resources"

if [ ${#ARCHS[@]} -gt 1 ]; then
    say "Merging into a universal binary"
    lipo -create -output "$BUNDLE/Contents/MacOS/$APP" \
        "target/aarch64-apple-darwin/release/$APP" \
        "target/x86_64-apple-darwin/release/$APP"
else
    cp "target/${ARCHS[0]}/release/$APP" "$BUNDLE/Contents/MacOS/$APP"
fi
chmod 0755 "$BUNDLE/Contents/MacOS/$APP"

cp packaging/macos/AppIcon.icns "$BUNDLE/Contents/Resources/AppIcon.icns" 2>/dev/null \
    || warn "no AppIcon.icns; the bundle will use the generic icon"

cat > "$BUNDLE/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key><string>$DISPLAY_NAME</string>
    <key>CFBundleDisplayName</key><string>$DISPLAY_NAME</string>
    <key>CFBundleIdentifier</key><string>dev.vdsadmin.app</string>
    <key>CFBundleVersion</key><string>$VERSION</string>
    <key>CFBundleShortVersionString</key><string>$VERSION</string>
    <key>CFBundleExecutable</key><string>$APP</string>
    <key>CFBundleIconFile</key><string>AppIcon</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>LSMinimumSystemVersion</key><string>11.0</string>
    <key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
PLIST

if [ -n "${MACOS_SIGNING_IDENTITY:-}" ]; then
    say "Signing"
    codesign --force --deep --options runtime --timestamp \
        --sign "$MACOS_SIGNING_IDENTITY" "$BUNDLE"
    codesign --verify --strict --verbose=2 "$BUNDLE"
else
    warn "MACOS_SIGNING_IDENTITY is not set; the bundle is unsigned."
    warn "Gatekeeper will refuse to open it on another Mac."
fi

say "Packaging the disk image"
DMG="$OUT_DIR/$APP-$VERSION.dmg"
rm -f "$DMG"
hdiutil create -volname "$DISPLAY_NAME" -srcfolder "$BUNDLE" -ov -format UDZO "$DMG" >/dev/null

if [ -n "${MACOS_NOTARY_PROFILE:-}" ]; then
    say "Notarising (this takes a few minutes)"
    xcrun notarytool submit "$DMG" --keychain-profile "$MACOS_NOTARY_PROFILE" --wait
    xcrun stapler staple "$DMG"
elif [ -n "${MACOS_SIGNING_IDENTITY:-}" ]; then
    warn "signed but not notarised; set MACOS_NOTARY_PROFILE for a distributable build"
fi

say "Writing checksums"
( cd "$OUT_DIR" && shasum -a 256 ./*.dmg > SHA256SUMS )

say "Done:"
ls -lh "$OUT_DIR"
