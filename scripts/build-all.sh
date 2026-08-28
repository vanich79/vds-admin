#!/usr/bin/env bash
#
# Runs every build that can run here, and says clearly what it skipped.
#
#   scripts/build-all.sh
#
# ## What "all" means on one machine
#
# It cannot mean every artefact. The Windows build needs Windows, the Android build needs
# the NDK, and the AppImage needs `appimagetool`. Rather than fail on the first missing
# tool — which would make the script useless to anyone who has not installed all of them
# — each part is attempted and its absence is reported at the end.
#
# The release pipeline in `.github/workflows/release.yml` is what actually produces the
# full set, by running the platform-specific scripts on the matching runners.

set -uo pipefail

cd "$(dirname "$0")/.." || exit 1

OUT_DIR="${OUT_DIR:-dist}"
HOST="$(rustc -vV | awk '/^host:/ {print $2}')"

say() { printf '\033[1m==>\033[0m %s\n' "$*"; }
skip() { printf '\033[33m--\033[0m %s\n' "$*"; SKIPPED+=("$1"); }
fail() { printf '\033[31mxx\033[0m %s\n' "$*"; FAILED+=("$1"); }

SKIPPED=()
FAILED=()
BUILT=()

say "Host is $HOST"
say "Checking the workspace before building anything"

if ! cargo fmt --all -- --check >/dev/null 2>&1; then
    fail "formatting (run: cargo fmt --all)"
elif ! cargo lint >/dev/null 2>&1; then
    fail "lint (run: cargo lint)"
elif ! cargo test --workspace >/dev/null 2>&1; then
    fail "tests (run: cargo test --workspace)"
else
    say "Workspace is clean"
fi

if [ ${#FAILED[@]} -gt 0 ]; then
    printf '\033[31merror:\033[0m the workspace is not clean; nothing was built\n' >&2
    printf '  %s\n' "${FAILED[@]}" >&2
    exit 1
fi

# --- desktop, for whichever platform this is ----------------------------------------

case "$HOST" in
    *windows*)
        say "Building the Windows application"
        if OUT_DIR="$OUT_DIR/windows" scripts/build-windows.sh --installer; then
            BUILT+=("windows")
        else
            fail "windows"
        fi
        skip "linux (this is not Linux)"
        ;;
    *linux*)
        say "Building the Linux application"
        if OUT_DIR="$OUT_DIR/linux" scripts/build-linux.sh --formats tar,deb,appimage; then
            BUILT+=("linux")
        else
            fail "linux"
        fi
        skip "windows (needs a Windows host; see docs/CROSS_COMPILATION.md)"
        ;;
    *darwin*)
        say "Building the macOS application"
        if OUT_DIR="$OUT_DIR/macos" scripts/build-macos.sh; then
            BUILT+=("macos")
        else
            fail "macos"
        fi
        ;;
    *)
        skip "desktop (unrecognised host $HOST)"
        ;;
esac

# --- the agent ------------------------------------------------------------------------

say "Building the agent"
if OUT_DIR="$OUT_DIR/agent" scripts/build-agent.sh; then
    BUILT+=("agent")
else
    fail "agent"
fi

# --- Android --------------------------------------------------------------------------

if [ -n "${ANDROID_NDK_ROOT:-}" ] && command -v cargo-apk >/dev/null 2>&1; then
    say "Building the Android APK"
    if OUT_DIR="$OUT_DIR/android" scripts/build-android.sh; then
        BUILT+=("android")
    else
        fail "android"
    fi
else
    skip "android (needs ANDROID_NDK_ROOT and cargo-apk)"
fi

# --- summary ----------------------------------------------------------------------------

printf '\n'
say "Summary"

if [ ${#BUILT[@]} -gt 0 ]; then
    printf '  built:   %s\n' "${BUILT[*]}"
fi
if [ ${#SKIPPED[@]} -gt 0 ]; then
    printf '  skipped: %s\n' "${SKIPPED[*]}"
fi
if [ ${#FAILED[@]} -gt 0 ]; then
    printf '  \033[31mfailed:  %s\033[0m\n' "${FAILED[*]}"
    exit 1
fi

printf '\nArtefacts are in %s/\n' "$OUT_DIR"
