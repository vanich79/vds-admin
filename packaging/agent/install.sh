#!/bin/sh
#
# vds-agent installer.
#
#   curl -fsSL https://github.com/vds-admin/vds-admin/releases/latest/download/install.sh | sudo sh
#
# ## On piping a script into a root shell
#
# This is the install method people actually use, so it is supported and made as safe as
# a one-liner can be: every download is verified against a SHA256SUMS file before
# anything is executed or installed, and the script refuses to continue if verification
# fails. What it cannot defend against is a compromised release host serving a matching
# pair of tarball and checksum file.
#
# If that matters to you — and on a production fleet it should — use the offline path
# instead, which verifies a signature made with a key that never touches the release
# host. `docs/AGENT.md` gives the commands. In short:
#
#   1. download the tarball, SHA256SUMS and SHA256SUMS.asc on a machine you trust;
#   2. gpg --verify SHA256SUMS.asc SHA256SUMS
#   3. sha256sum --check SHA256SUMS --ignore-missing
#   4. copy the verified tarball to each host and run: ./install.sh --archive <file>
#
# Set VDS_AGENT_GPG_KEY to a key fingerprint and this script will do step 2 itself.
#
# ## What it does
#
#   * creates the system user `vds-agent` (no shell, no home, no login)
#   * installs the binary to /usr/local/bin/vds-agent
#   * writes /etc/vds-agent/agent.toml and a 0600 token file, if they do not exist
#   * installs and enables the systemd unit
#
# It is safe to re-run: an existing configuration and token are never overwritten.

set -eu

REPO="${VDS_AGENT_REPO:-vds-admin/vds-admin}"
VERSION="${VDS_AGENT_VERSION:-latest}"
PREFIX="${VDS_AGENT_PREFIX:-/usr/local/bin}"
CONFIG_DIR="${VDS_AGENT_CONFIG_DIR:-/etc/vds-agent}"
STATE_DIR="${VDS_AGENT_STATE_DIR:-/var/lib/vds-agent}"
UNIT_DIR="${VDS_AGENT_UNIT_DIR:-/etc/systemd/system}"
SERVICE_USER="vds-agent"
ARCHIVE=""
SKIP_VERIFY="no"

say() { printf '%s\n' "$*"; }
step() { printf '\033[1m==>\033[0m %s\n' "$*"; }
warn() { printf '\033[33mwarning:\033[0m %s\n' "$*" >&2; }
die() { printf '\033[31merror:\033[0m %s\n' "$*" >&2; exit 1; }

usage() {
    cat <<'USAGE'
Usage: install.sh [options]

  --archive <path>   Install from an already-downloaded tarball instead of fetching one.
                     Use this for the verified-offline path.
  --version <tag>    Release to install (default: latest).
  --prefix <dir>     Where the binary goes (default: /usr/local/bin).
  --uninstall        Stop the service and remove the binary and unit. Configuration,
                     token and certificate are left in place.
  --purge            As --uninstall, and also removes the configuration, token,
                     certificate and the service user. Destructive.
  --skip-verify      Do not check SHA256SUMS. Refuses to run unless
                     VDS_AGENT_I_KNOW_WHAT_I_AM_DOING=yes is also set.
  -h, --help         This message.

Environment:
  VDS_AGENT_GPG_KEY  Fingerprint of the release signing key. When set, SHA256SUMS.asc is
                     downloaded and its signature verified before the checksums are used.
USAGE
}

# --- argument parsing ---------------------------------------------------------------

ACTION="install"
while [ $# -gt 0 ]; do
    case "$1" in
        --archive) ARCHIVE="${2:-}"; [ -n "$ARCHIVE" ] || die "--archive needs a path"; shift 2 ;;
        --version) VERSION="${2:-}"; [ -n "$VERSION" ] || die "--version needs a tag"; shift 2 ;;
        --prefix) PREFIX="${2:-}"; [ -n "$PREFIX" ] || die "--prefix needs a directory"; shift 2 ;;
        --uninstall) ACTION="uninstall"; shift ;;
        --purge) ACTION="purge"; shift ;;
        --skip-verify) SKIP_VERIFY="yes"; shift ;;
        -h|--help) usage; exit 0 ;;
        *) die "unknown option: $1 (try --help)" ;;
    esac
done

[ "$(id -u)" = "0" ] || die "this installer must run as root; try: sudo sh install.sh"

# --- uninstall ----------------------------------------------------------------------

remove_service() {
    if command -v systemctl >/dev/null 2>&1; then
        systemctl stop vds-agent.service 2>/dev/null || true
        systemctl disable vds-agent.service 2>/dev/null || true
    fi
    rm -f "$UNIT_DIR/vds-agent.service"
    rm -f "$PREFIX/vds-agent"
    if command -v systemctl >/dev/null 2>&1; then
        systemctl daemon-reload || true
    fi
}

if [ "$ACTION" = "uninstall" ]; then
    step "Removing vds-agent"
    remove_service
    say "Done. Configuration in $CONFIG_DIR and state in $STATE_DIR were kept."
    say "Run with --purge to remove those too."
    exit 0
fi

if [ "$ACTION" = "purge" ]; then
    step "Removing vds-agent and all its data"
    remove_service
    rm -rf "$CONFIG_DIR" "$STATE_DIR"
    if id "$SERVICE_USER" >/dev/null 2>&1; then
        userdel "$SERVICE_USER" 2>/dev/null || true
    fi
    say "Done. The app will now show this server as offline until it is removed there too."
    exit 0
fi

# --- platform detection -------------------------------------------------------------

detect_target() {
    machine="$(uname -m)"
    case "$machine" in
        x86_64|amd64)   say "x86_64-unknown-linux-musl" ;;
        aarch64|arm64)  say "aarch64-unknown-linux-musl" ;;
        armv7l|armv7|armhf) say "armv7-unknown-linux-musleabihf" ;;
        armv6l)         say "arm-unknown-linux-musleabihf" ;;
        i686|i386)      say "i686-unknown-linux-musl" ;;
        *) die "unsupported architecture: $machine. Build from source; see docs/BUILDING.md" ;;
    esac
}

[ "$(uname -s)" = "Linux" ] || die "the agent runs on Linux; this is $(uname -s)"

TARGET="$(detect_target)"
step "Detected $TARGET"

# musl builds are static, so there is no libc version to check. The one real
# prerequisite is systemd, and its absence is worth saying out loud rather than
# discovering after the files are in place.
if ! command -v systemctl >/dev/null 2>&1; then
    warn "systemd was not found. The binary will be installed but not started;"
    warn "you will need to supervise it yourself."
fi

# --- download and verify -------------------------------------------------------------

fetch() {
    url="$1"; out="$2"
    if command -v curl >/dev/null 2>&1; then
        curl -fsSL --proto '=https' --tlsv1.2 -o "$out" "$url"
    elif command -v wget >/dev/null 2>&1; then
        wget -q --https-only -O "$out" "$url"
    else
        die "neither curl nor wget is available"
    fi
}

WORK="$(mktemp -d)"
# The token is written under this directory before being moved into place, so it is
# cleaned up whatever happens.
trap 'rm -rf "$WORK"' EXIT INT TERM
chmod 700 "$WORK"

TARBALL="vds-agent-$TARGET.tar.gz"

if [ -n "$ARCHIVE" ]; then
    [ -f "$ARCHIVE" ] || die "no such file: $ARCHIVE"
    step "Installing from $ARCHIVE"
    cp "$ARCHIVE" "$WORK/$TARBALL"
else
    if [ "$VERSION" = "latest" ]; then
        BASE="https://github.com/$REPO/releases/latest/download"
    else
        BASE="https://github.com/$REPO/releases/download/$VERSION"
    fi

    step "Downloading $TARBALL"
    fetch "$BASE/$TARBALL" "$WORK/$TARBALL" || die "could not download $BASE/$TARBALL"

    if [ "$SKIP_VERIFY" = "yes" ]; then
        [ "${VDS_AGENT_I_KNOW_WHAT_I_AM_DOING:-no}" = "yes" ] || die \
            "--skip-verify installs an unverified binary as root. Set
   VDS_AGENT_I_KNOW_WHAT_I_AM_DOING=yes
if that is genuinely what you want."
        warn "skipping checksum verification at your request"
    else
        step "Verifying checksum"
        fetch "$BASE/SHA256SUMS" "$WORK/SHA256SUMS" || die "could not download SHA256SUMS"

        if [ -n "${VDS_AGENT_GPG_KEY:-}" ]; then
            command -v gpg >/dev/null 2>&1 || die "VDS_AGENT_GPG_KEY is set but gpg is not installed"
            step "Verifying the signature on SHA256SUMS"
            fetch "$BASE/SHA256SUMS.asc" "$WORK/SHA256SUMS.asc" \
                || die "could not download SHA256SUMS.asc"
            gpg --status-fd 1 --verify "$WORK/SHA256SUMS.asc" "$WORK/SHA256SUMS" 2>/dev/null \
                | grep -q "VALIDSIG $VDS_AGENT_GPG_KEY" \
                || die "SHA256SUMS is not signed by $VDS_AGENT_GPG_KEY. Stopping."
            say "Signature is valid."
        fi

        # `--ignore-missing` because SHA256SUMS covers every artefact in the release and
        # only one was downloaded. Without it the check fails on the files that are
        # legitimately absent.
        ( cd "$WORK" && sha256sum --check --ignore-missing SHA256SUMS >/dev/null 2>&1 ) \
            || die "checksum mismatch on $TARBALL. Do not install this file."
        say "Checksum matches."
    fi
fi

# --- install --------------------------------------------------------------------------

step "Unpacking"
tar -xzf "$WORK/$TARBALL" -C "$WORK" || die "could not unpack $TARBALL"
BINARY="$(find "$WORK" -type f -name vds-agent -perm -u+x 2>/dev/null | head -n 1)"
[ -n "$BINARY" ] || die "the archive does not contain a vds-agent binary"

if ! id "$SERVICE_USER" >/dev/null 2>&1; then
    step "Creating the $SERVICE_USER system user"
    # No shell, no home, no login: the account exists to own a socket and a certificate.
    useradd --system --no-create-home --shell /usr/sbin/nologin "$SERVICE_USER" 2>/dev/null \
        || useradd --system --no-create-home --shell /sbin/nologin "$SERVICE_USER" 2>/dev/null \
        || die "could not create the $SERVICE_USER user"
fi

step "Installing the binary to $PREFIX/vds-agent"
install -d -m 0755 "$PREFIX"
# Installed to a temporary name and renamed, so an interrupted install never leaves a
# half-written binary that systemd will happily try to execute.
install -m 0755 "$BINARY" "$PREFIX/.vds-agent.new"
mv -f "$PREFIX/.vds-agent.new" "$PREFIX/vds-agent"

install -d -m 0750 -o "$SERVICE_USER" -g "$SERVICE_USER" "$STATE_DIR"
install -d -m 0755 "$CONFIG_DIR"

TOKEN_FILE="$CONFIG_DIR/token"
NEW_TOKEN="no"
if [ ! -f "$TOKEN_FILE" ]; then
    step "Generating an access token"
    # Written with a restrictive umask from the start; a token that is briefly
    # world-readable has already leaked.
    ( umask 077 && head -c 32 /dev/urandom | base64 | tr -d '\n' > "$WORK/token" ) \
        || die "could not generate a token"
    install -m 0600 -o "$SERVICE_USER" -g "$SERVICE_USER" "$WORK/token" "$TOKEN_FILE"
    NEW_TOKEN="yes"
else
    say "Keeping the existing token in $TOKEN_FILE"
fi

CONFIG_FILE="$CONFIG_DIR/agent.toml"
if [ ! -f "$CONFIG_FILE" ]; then
    step "Writing $CONFIG_FILE"
    cat > "$WORK/agent.toml" <<CONFIG
# vds-agent configuration. See docs/AGENT.md for every option.
#
# Check this file after editing, before restarting:
#   vds-agent --check --config $CONFIG_FILE

token_file = "$TOKEN_FILE"
bind = "0.0.0.0"
port = 9443
state_dir = "$STATE_DIR"
log_level = "info"
CONFIG
    install -m 0644 "$WORK/agent.toml" "$CONFIG_FILE"
else
    say "Keeping the existing configuration in $CONFIG_FILE"
fi

# --- service ----------------------------------------------------------------------------

if command -v systemctl >/dev/null 2>&1; then
    step "Installing the systemd unit"
    UNIT_SOURCE="$(find "$WORK" -type f -name vds-agent.service 2>/dev/null | head -n 1)"
    if [ -n "$UNIT_SOURCE" ]; then
        install -m 0644 "$UNIT_SOURCE" "$UNIT_DIR/vds-agent.service"
    else
        die "the archive does not contain vds-agent.service"
    fi

    # Validated before the running agent is replaced, so a bad edit cannot take the
    # monitoring down.
    "$PREFIX/vds-agent" --check --config "$CONFIG_FILE" >/dev/null \
        || die "the installed configuration did not validate; nothing was started"

    systemctl daemon-reload
    systemctl enable vds-agent.service >/dev/null 2>&1 || true
    systemctl restart vds-agent.service || die "the service did not start; see: journalctl -u vds-agent -n 50"

    # A unit that starts and exits two seconds later looks like success to `restart`.
    sleep 1
    systemctl is-active --quiet vds-agent.service \
        || die "the service is not running; see: journalctl -u vds-agent -n 50"
fi

FINGERPRINT="$("$PREFIX/vds-agent" --fingerprint --config "$CONFIG_FILE" 2>/dev/null || true)"

say ""
step "vds-agent is installed and running"
say ""
say "  Version:      $("$PREFIX/vds-agent" --version 2>/dev/null || say unknown)"
say "  Listening on: $(hostname):9443"
say "  Fingerprint:  ${FINGERPRINT:-unavailable}"
say ""

if [ "$NEW_TOKEN" = "yes" ]; then
    say "Add this server in VDS Admin with the token below."
    say ""
    say "  Token: $(cat "$TOKEN_FILE")"
    say ""
    say "It is not shown again. To read it later:  sudo cat $TOKEN_FILE"
    say ""
fi

say "Compare the fingerprint above with the one the app shows when it first connects."
say "They must match; if they do not, something is between you and this host."
say ""
say "  Logs:    journalctl -u vds-agent -f"
say "  Status:  systemctl status vds-agent"
say ""
say "Remember to allow port 9443 from the machine running VDS Admin, and only from it."
