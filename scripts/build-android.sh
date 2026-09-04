#!/usr/bin/env bash
#
# Builds the Android APK.
#
#   scripts/build-android.sh                    # debug APK, every ABI
#   scripts/build-android.sh --release          # release APK (needs a signing key)
#   scripts/build-android.sh --abi arm64-v8a    # one ABI, much faster
#
# ## Requirements
#
# Android is the one target with a real toolchain prerequisite:
#
#   * the Android SDK, with `ANDROID_HOME` set;
#   * the NDK (r26 or newer), with `ANDROID_NDK_ROOT` set;
#   * `cargo-apk`:  cargo install cargo-apk
#
# `docs/CROSS_COMPILATION.md` has the step-by-step setup, including which NDK version
# matches which Rust release.
#
# ## Signing
#
# A debug APK is signed with `~/.android/debug.keystore`, created here if absent. Keeping
# that file — CI caches it — is what lets a new build install over an old one instead of
# forcing an uninstall that deletes the user's data.
#
# ## Why an unsigned release APK is not produced
#
# An unsigned release APK cannot be installed, so producing one is a way of appearing to
# succeed while delivering nothing. If no key is configured, this script builds a debug
# APK and says so.

set -euo pipefail

cd "$(dirname "$0")/.."

OUT_DIR="${OUT_DIR:-dist/android}"
BUILD_TYPE="debug"
ABIS=("arm64-v8a" "armeabi-v7a" "x86_64" "x86")
SELECTED=()

say() { printf '\033[1m==>\033[0m %s\n' "$*"; }
warn() { printf '\033[33mwarning:\033[0m %s\n' "$*" >&2; }
die() { printf '\033[31merror:\033[0m %s\n' "$*" >&2; exit 1; }

while [ $# -gt 0 ]; do
    case "$1" in
        --release) BUILD_TYPE="release"; shift ;;
        --abi) SELECTED+=("${2:-}"); shift 2 ;;
        --out) OUT_DIR="${2:-}"; shift 2 ;;
        -h|--help) sed -n '2,32p' "$0"; exit 0 ;;
        *) die "unknown option: $1" ;;
    esac
done

[ ${#SELECTED[@]} -gt 0 ] && ABIS=("${SELECTED[@]}")

# --- prerequisites -----------------------------------------------------------------

[ -n "${ANDROID_HOME:-}" ] || die "ANDROID_HOME is not set; see docs/CROSS_COMPILATION.md"
[ -n "${ANDROID_NDK_ROOT:-}" ] || die "ANDROID_NDK_ROOT is not set; see docs/CROSS_COMPILATION.md"
# `--lib` on both invocations below is not optional: Android loads a shared object and
# calls `android_main`. Building the binary target instead produces an executable no
# Android device will ever start.
command -v cargo-apk >/dev/null 2>&1 || die "cargo-apk is not installed: cargo install cargo-apk"

# Rust target per Android ABI. The mapping is fixed by the NDK, not by us.
rust_target_for() {
    case "$1" in
        arm64-v8a)   echo "aarch64-linux-android" ;;
        armeabi-v7a) echo "armv7-linux-androideabi" ;;
        x86_64)      echo "x86_64-linux-android" ;;
        x86)         echo "i686-linux-android" ;;
        *) die "unknown ABI: $1" ;;
    esac
}

if [ "$BUILD_TYPE" = "release" ] && [ -z "${ANDROID_KEYSTORE:-}" ]; then
    warn "ANDROID_KEYSTORE is not set, so a release APK could not be signed."
    warn "Building a debug APK instead — an unsigned release APK cannot be installed."
    BUILD_TYPE="debug"
fi

# --- the debug signing key ------------------------------------------------------------
#
# Android refuses to install an update signed by a different key than the version already
# on the device: the only way through is to uninstall, which takes the database with it.
# `cargo-apk` creates a debug key when it cannot find one, and a fresh CI runner never
# has one — so every build signed with a new key, and every install wiped the settings
# from the last one.
#
# Creating it here, at the path Android has used since the SDK's beginning, means the
# workflow can cache that one file and keep the key stable. The password is `android`:
# not an oversight, it is the documented constant for debug keystores, and this key can
# do nothing but sign debug builds of this application.
DEBUG_KEYSTORE="${ANDROID_DEBUG_KEYSTORE:-$HOME/.android/debug.keystore}"
if [ "$BUILD_TYPE" = "debug" ] && command -v keytool >/dev/null 2>&1; then
    mkdir -p "$(dirname "$DEBUG_KEYSTORE")"
    if [ ! -f "$DEBUG_KEYSTORE" ]; then
        say "Creating a debug signing key at $DEBUG_KEYSTORE"
        keytool -genkeypair -keystore "$DEBUG_KEYSTORE" \
            -storepass android -keypass android \
            -alias androiddebugkey -keyalg RSA -keysize 2048 -validity 10000 \
            -dname "CN=Android Debug,O=Android,C=US" >/dev/null
    fi

    # The fingerprint, never the key. Two builds install over each other if and only if
    # this line matches, which is what makes "why will it not update" answerable from a
    # log rather than by guesswork.
    say "Debug key fingerprint:"
    keytool -list -keystore "$DEBUG_KEYSTORE" -storepass android \
        -alias androiddebugkey 2>/dev/null | grep -i "SHA" || true
fi

mkdir -p "$OUT_DIR"

for abi in "${ABIS[@]}"; do
    target="$(rust_target_for "$abi")"
    say "Building $abi ($target)"
    rustup target add "$target" >/dev/null 2>&1 || true

    if [ "$BUILD_TYPE" = "release" ]; then
        cargo apk build --release --target "$target" --package vds-admin --lib
        built="target/release/apk/$abi/vds-admin.apk"
    else
        cargo apk build --target "$target" --package vds-admin --lib
        built="target/debug/apk/$abi/vds-admin.apk"
    fi

    # cargo-apk's output path has moved between versions; find it rather than guess.
    if [ ! -f "$built" ]; then
        built="$(find target -name '*.apk' -newer Cargo.toml 2>/dev/null | head -n 1)"
    fi
    if [ -z "$built" ] || [ ! -f "$built" ]; then
        die "no APK was produced for $abi"
    fi

    cp "$built" "$OUT_DIR/vds-admin-$abi-$BUILD_TYPE.apk"
    say "  $OUT_DIR/vds-admin-$abi-$BUILD_TYPE.apk"
done

say "Writing checksums"
( cd "$OUT_DIR" && sha256sum ./*.apk > SHA256SUMS )

say "Done ($BUILD_TYPE):"
ls -lh "$OUT_DIR"

if [ "$BUILD_TYPE" = "debug" ]; then
    say ""
    say "This is a debug APK. Install it with:  adb install -r <file>"
fi
