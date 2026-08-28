#!/usr/bin/env bash
#
# Builds the desktop application for Linux and packages it.
#
#   scripts/build-linux.sh                # native target, tar.gz
#   scripts/build-linux.sh --target aarch64-unknown-linux-gnu
#   scripts/build-linux.sh --formats tar,deb,appimage
#
# ## Why glibc here and musl for the agent
#
# The opposite of the agent's reasoning, for the opposite reason. The GUI links against
# the system's graphics stack — X11 or Wayland, and their client libraries — which are
# dynamically loaded and are glibc-linked on every mainstream distribution. A static
# musl build cannot use them. The agent has no such dependency, so it gets to be static
# and portable.
#
# The AppImage is what covers the distribution spread instead: it bundles what it needs
# and runs on anything with a recent enough kernel.

set -euo pipefail

cd "$(dirname "$0")/.."

TARGET="${TARGET:-$(rustc -vV | awk '/^host:/ {print $2}')}"
FORMATS="tar"
OUT_DIR="${OUT_DIR:-dist/linux}"
APP="vds-admin"
VERSION="$(awk -F'"' '/^version/ {print $2; exit}' Cargo.toml)"

say() { printf '\033[1m==>\033[0m %s\n' "$*"; }
warn() { printf '\033[33mwarning:\033[0m %s\n' "$*" >&2; }
die() { printf '\033[31merror:\033[0m %s\n' "$*" >&2; exit 1; }

while [ $# -gt 0 ]; do
    case "$1" in
        --target) TARGET="${2:-}"; shift 2 ;;
        --formats) FORMATS="${2:-}"; shift 2 ;;
        --out) OUT_DIR="${2:-}"; shift 2 ;;
        -h|--help) sed -n '2,20p' "$0"; exit 0 ;;
        *) die "unknown option: $1" ;;
    esac
done

has_format() { case ",$FORMATS," in *",$1,"*) return 0 ;; *) return 1 ;; esac; }

say "Building $APP $VERSION for $TARGET"
rustup target add "$TARGET" >/dev/null 2>&1 || true

if [ "$TARGET" = "$(rustc -vV | awk '/^host:/ {print $2}')" ]; then
    cargo build --release --target "$TARGET" --package vds-admin
else
    command -v cross >/dev/null 2>&1 \
        || die "cross-compiling the GUI needs 'cross'; see docs/CROSS_COMPILATION.md"
    cross build --release --target "$TARGET" --package vds-admin
fi

BINARY="target/$TARGET/release/$APP"
[ -f "$BINARY" ] || die "expected $BINARY but it is not there"

mkdir -p "$OUT_DIR"

# --- tar.gz -----------------------------------------------------------------------
#
# The lowest common denominator, and the one format that always works.
if has_format tar; then
    say "Packaging tar.gz"
    stage="$(mktemp -d)"
    install -D -m 0755 "$BINARY" "$stage/$APP-$VERSION/$APP"
    install -D -m 0644 packaging/linux/vds-admin.desktop "$stage/$APP-$VERSION/$APP.desktop"
    install -D -m 0644 packaging/linux/vds-admin.svg "$stage/$APP-$VERSION/$APP.svg"
    install -D -m 0644 README.md "$stage/$APP-$VERSION/README.md"
    tar -czf "$OUT_DIR/$APP-$VERSION-$TARGET.tar.gz" -C "$stage" "$APP-$VERSION"
    rm -rf "$stage"
fi

# --- .deb -------------------------------------------------------------------------
if has_format deb; then
    if command -v cargo-deb >/dev/null 2>&1; then
        say "Packaging .deb"
        cargo deb --no-build --target "$TARGET" --package vds-admin --output "$OUT_DIR"
    else
        warn "cargo-deb is not installed; skipping .deb (cargo install cargo-deb)"
    fi
fi

# --- .rpm -------------------------------------------------------------------------
if has_format rpm; then
    if command -v cargo-generate-rpm >/dev/null 2>&1; then
        say "Packaging .rpm"
        cargo generate-rpm --target "$TARGET" --package apps/ui --output "$OUT_DIR"
    else
        warn "cargo-generate-rpm is not installed; skipping .rpm"
    fi
fi

# --- AppImage ---------------------------------------------------------------------
if has_format appimage; then
    if command -v appimagetool >/dev/null 2>&1; then
        say "Packaging AppImage"
        root="$(mktemp -d)/$APP.AppDir"
        install -D -m 0755 "$BINARY" "$root/usr/bin/$APP"
        install -D -m 0644 packaging/linux/vds-admin.desktop "$root/$APP.desktop"
        install -D -m 0644 packaging/linux/vds-admin.svg "$root/$APP.svg"

        # AppImage requires AppRun to be executable and to exec the real binary.
        cat > "$root/AppRun" <<'APPRUN'
#!/bin/sh
HERE="$(dirname "$(readlink -f "$0")")"
exec "$HERE/usr/bin/vds-admin" "$@"
APPRUN
        chmod 0755 "$root/AppRun"

        ARCH="$(uname -m)" appimagetool "$root" "$OUT_DIR/$APP-$VERSION-$(uname -m).AppImage"
        rm -rf "$(dirname "$root")"
    else
        warn "appimagetool is not installed; skipping AppImage"
        warn "  https://github.com/AppImage/AppImageKit/releases"
    fi
fi

say "Writing checksums"
( cd "$OUT_DIR" && sha256sum ./* > SHA256SUMS 2>/dev/null || true )

say "Done:"
ls -lh "$OUT_DIR"
