#!/usr/bin/env bash
#
# Builds `vds-agent` for every Linux architecture a release covers, and packages each
# one as a tarball with its systemd unit.
#
#   scripts/build-agent.sh                 # every target
#   scripts/build-agent.sh x86_64 aarch64  # only these
#
# ## Why musl
#
# The agent is installed on machines nobody controls: a 2019 CentOS box, a current
# Debian, an Alpine container. A glibc build carries a minimum glibc version with it and
# fails on anything older with a link error that means nothing to the person installing
# it. A static musl binary has no such dependency, which is worth the handful of
# kilobytes it costs.
#
# ## Why `cross`
#
# Cross-compiling to five targets needs five linkers and five sysroots. `cross` supplies
# them in a container, so this script works the same on a developer's laptop and in CI.
# If `cross` is unavailable, the native target is still built with plain `cargo`; the
# rest are skipped with a message rather than a failure, because a developer checking
# their own changes should not need Docker.

set -euo pipefail

cd "$(dirname "$0")/.."

OUT_DIR="${OUT_DIR:-dist/agent}"
PROFILE="agent-release"
UNIT="packaging/agent/vds-agent.service"
INSTALLER="packaging/agent/install.sh"

# Target triple, followed by the short name used on the command line.
TARGETS=(
    "x86_64-unknown-linux-musl:x86_64"
    "aarch64-unknown-linux-musl:aarch64"
    "armv7-unknown-linux-musleabihf:armv7"
    "arm-unknown-linux-musleabihf:armv6"
    "i686-unknown-linux-musl:i686"
)

say() { printf '\033[1m==>\033[0m %s\n' "$*"; }
warn() { printf '\033[33mwarning:\033[0m %s\n' "$*" >&2; }
die() { printf '\033[31merror:\033[0m %s\n' "$*" >&2; exit 1; }

selected=("$@")
host_target="$(rustc -vV | awk '/^host:/ {print $2}')"

if command -v cross >/dev/null 2>&1; then
    builder="cross"
else
    builder="cargo"
    warn "cross is not installed; only the host target will be built."
    warn "install it with: cargo install cross --git https://github.com/cross-rs/cross"
fi

mkdir -p "$OUT_DIR"

wanted() {
    local short="$1"
    [ ${#selected[@]} -eq 0 ] && return 0
    local candidate
    for candidate in "${selected[@]}"; do
        [ "$candidate" = "$short" ] && return 0
    done
    return 1
}

built=0
for entry in "${TARGETS[@]}"; do
    target="${entry%%:*}"
    short="${entry##*:}"

    wanted "$short" || continue

    if [ "$builder" = "cargo" ] && [ "$target" != "$host_target" ]; then
        warn "skipping $short: needs cross"
        continue
    fi

    say "Building $short ($target)"
    rustup target add "$target" >/dev/null 2>&1 || true
    "$builder" build --profile "$PROFILE" --target "$target" --package vds-agent

    binary="target/$target/$PROFILE/vds-agent"
    [ -f "$binary" ] || die "expected $binary but it is not there"

    # Staged in a directory named after the target so the tarball unpacks into something
    # recognisable rather than scattering files into the current directory.
    stage="$(mktemp -d)"
    trap 'rm -rf "$stage"' RETURN

    install -m 0755 "$binary" "$stage/vds-agent"
    install -m 0644 "$UNIT" "$stage/vds-agent.service"
    install -m 0755 "$INSTALLER" "$stage/install.sh"
    install -m 0644 packaging/agent/agent.toml.example "$stage/agent.toml.example"

    tarball="$OUT_DIR/vds-agent-$target.tar.gz"
    tar -czf "$tarball" -C "$stage" .
    rm -rf "$stage"
    trap - RETURN

    size="$(du -h "$tarball" | cut -f1)"
    say "  $tarball ($size)"
    built=$((built + 1))
done

[ "$built" -gt 0 ] || die "nothing was built"

# The installer is published alongside the tarballs so `curl | sh` can reach it without
# unpacking anything.
install -m 0755 "$INSTALLER" "$OUT_DIR/install.sh"

say "Writing checksums"
( cd "$OUT_DIR" && sha256sum ./*.tar.gz install.sh > SHA256SUMS )

say "Done: $built target(s) in $OUT_DIR"
cat "$OUT_DIR/SHA256SUMS"
