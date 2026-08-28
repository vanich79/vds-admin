#!/usr/bin/env bash
#
# Enforces the dependency rule from docs/ARCHITECTURE.md, in CI.
#
#   scripts/check-layering.sh
#
# ## Why a script and not a convention
#
# "Dependencies point inward" is the load-bearing decision of the whole design, and it is
# violated by adding one line to a Cargo.toml. A reviewer will catch that on a good day.
# This catches it every day.
#
# The rule, from innermost outward:
#
#   domain          depends on nothing in this workspace
#   application     depends on domain only
#   infra-*         depends on domain, and application only for the ports it implements
#   composition     depends on everything — it is the composition root, that is its job
#   apps/ui         depends on composition, application and domain; never on an infra crate
#   agent           depends on domain, agent-protocol and infra-collectors
#
# The last one is the subtlest: the UI must reach infrastructure through the interfaces
# in `application`, never by naming an implementation. The moment it imports
# `vds-infra-ssh`, swapping the SSH layer stops being a composition-root change.

set -uo pipefail

cd "$(dirname "$0")/.."

VIOLATIONS=0

fail() {
    printf '\033[31mviolation:\033[0m %s\n' "$*" >&2
    VIOLATIONS=$((VIOLATIONS + 1))
}

ok() { printf '\033[32mok\033[0m       %s\n' "$*"; }

# Internal workspace dependencies of a crate, one per line.
deps_of() {
    local manifest="$1"
    # Only the [dependencies] section: dev-dependencies may legitimately reach anywhere,
    # because a test is allowed to know things production code is not.
    awk '
        /^\[dependencies\]/       { in_deps = 1; next }
        /^\[dev-dependencies\]/   { in_deps = 0; next }
        /^\[build-dependencies\]/ { in_deps = 0; next }
        /^\[/                     { if ($0 !~ /^\[dependencies/) in_deps = 0 }
        in_deps && /^vds-/        { split($0, parts, /[ .=]/); print parts[1] }
    ' "$manifest" | sort -u
}

# Fails when `crate` depends on anything outside `allowed`.
assert_only() {
    local name="$1" manifest="$2"
    shift 2
    local allowed=" $* "
    local clean=1

    while read -r dep; do
        [ -z "$dep" ] && continue
        case "$allowed" in
            *" $dep "*) ;;
            *) fail "$name depends on $dep"; clean=0 ;;
        esac
    done <<< "$(deps_of "$manifest")"

    [ "$clean" = 1 ] && ok "$name"
}

echo "Checking the dependency rule"
echo

assert_only "domain        " crates/domain/Cargo.toml
assert_only "agent-protocol" crates/agent-protocol/Cargo.toml
assert_only "application   " crates/application/Cargo.toml vds-domain

for manifest in crates/infra-*/Cargo.toml; do
    name="$(basename "$(dirname "$manifest")")"
    # `infra-collectors` is deliberately permitted here. It is a pure parsing library
    # with no I/O of its own, and it is *driven* by the transports: `infra-ssh` runs its
    # commands over a channel, the agent runs them locally. That is a horizontal
    # dependency between adapters, not an outward-pointing one, and forbidding it would
    # only force the parsers to be duplicated.
    assert_only "$name" "$manifest" \
        vds-domain vds-application vds-agent-protocol vds-infra-collectors
done

assert_only "apps/ui       " apps/ui/Cargo.toml vds-domain vds-application vds-composition
assert_only "agent         " agent/Cargo.toml vds-domain vds-agent-protocol vds-infra-collectors

# composition is deliberately unconstrained: wiring every implementation together is
# precisely what a composition root is for.
ok "composition   (unconstrained by design)"

echo

# --- the domain must not name a framework ---------------------------------------------
#
# A dependency check alone would miss a domain type that imports Slint through a
# re-export, so the source is checked too.
FORBIDDEN_IN_DOMAIN="slint reqwest rusqlite russh keyring axum tokio"
for crate in $FORBIDDEN_IN_DOMAIN; do
    if grep -rqE "^\s*(use|extern crate)\s+${crate//-/_}\b" crates/domain/src 2>/dev/null; then
        fail "the domain imports $crate"
    fi
done

if grep -rqE "^\s*use\s+vds_infra_" apps/ui/src 2>/dev/null; then
    fail "the UI imports an infrastructure crate directly"
fi

echo
if [ "$VIOLATIONS" -eq 0 ]; then
    printf '\033[32mThe dependency rule holds.\033[0m\n'
    exit 0
fi

printf '\033[31m%s violation(s).\033[0m See docs/ARCHITECTURE.md §3.\n' "$VIOLATIONS" >&2
exit 1
