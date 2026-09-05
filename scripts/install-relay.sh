#!/usr/bin/env bash
# install-relay.sh — install or update the self-hosted PPX relay (ppx-relay).
#
# Usage:
#   curl -fsSL https://github.com/PolderLabsVOF/ppexchanger/releases/latest/download/install-relay.sh | sudo bash
#   curl -fsSL .../install-relay.sh | sudo bash -s -- --bind 0.0.0.0 --port 47393
#   curl -fsSL .../install-relay.sh | sudo bash -s -- --funnel
#   bash install-relay.sh --uninstall
#   bash install-relay.sh --user   # rootless user service
#
# Guided interactive install — without flags, the script asks which exposure
# mode you want (Tailscale Funnel is recommended), then asks port/limits if
# needed, confirms, and handles every step automatically: download, checksum,
# install the binary, set up Tailscale (Funnel mode) or open the firewall
# (direct mode), write the systemd unit, enable and start the service.
# Re-run the script to update in place.
#
# Supported: Linux x86_64 / aarch64 (glibc), systemd. macOS or Windows run
# the binary directly with `./ppx-relay --bind ...` — no installer needed.
#
# Funnel mode requires a Tailscale account (free, https://tailscale.com).
# The installer prints a login URL on first run; open it in any browser to
# authenticate this machine, then the script resumes automatically. For
# unattended installs pass --tailscale-authkey tskey-... — generate one in
# the Tailscale admin panel under Settings → Keys.

set -euo pipefail
IFS=$' \t\n'

# Debug trace: when piped under curl | sudo bash, stdout/stderr are a pipe
# and short progress lines can be silently swallowed when the process exits
# abruptly. Persist a timestamped trace of every interesting step to a file
# the operator can `cat` afterwards — gives us ground truth even when the
# visible output is incomplete.
PPX_TRACE="${PPX_TRACE:-/run/ppx-relay-install.trace}"
: > "$PPX_TRACE" 2>/dev/null || PPX_TRACE="$(mktemp -t ppx-install-trace.XXXXXX)"
_ts_trace() { printf '%s pid=%s line=%s arg=%s\n' "$(date +%H:%M:%S)" "$$" "$1" "${2:-}" >> "$PPX_TRACE" 2>/dev/null || true; }
_ts_trace 0 "start"

trap '_ts_trace 9999 "exit_trap"' EXIT

# When the installer runs from `curl ... | sudo bash -s --`, stdout is a
# pipe — fully buffered by default — and short progress lines can be dropped
# when the process exits. Force line-buffered output so the operator sees
# every step in real time even when piped. `stdbuf` is part of GNU coreutils
# (present on every Linux distro and macOS via brew coreutils); if absent
# the fallback is no worse than the historical behaviour.
if [ ! -t 1 ] && command -v stdbuf >/dev/null 2>&1; then
    exec > >(stdbuf -oL -eL cat) 2>&1
fi

# Bound downloads so a stuck TLS handshake can't trap the installer forever.
CURL_COMMON=(--retry 3 --retry-delay 1 --max-time 120)

REPO="${PPX_REPO:-PolderLabsVOF/ppexchanger}"
VERSION="${PPX_VERSION:-latest}"
BIND_DEFAULT="127.0.0.1"
PORT_DEFAULT="47393"
FUNNEL_PORT_DEFAULT="10000"   # Tailscale Funnel only accepts 443/8443/10000.
MAX_CLIENTS_DEFAULT="128"
MAX_PER_IP_DEFAULT="16"

# Detect install mode up front: --user skips root, --system requires sudo.
SYSTEM_LEVEL=auto
AUTO_YES=0

# Exposure mode:
#   auto    — picked interactively (Tailscale Funnel if a TTY, else direct)
#   direct  — bind a local address; firewall opens inbound TCP
#   funnel  — install/configure Tailscale, expose via Funnel (port 10000)
# `auto` is the default so the guided prompt can recommend Funnel.
EXPOSURE_MODE=auto
TS_AUTHKEY="${PPX_TS_AUTHKEY:-}"

# Pre-set configuration (overridden by CLI flags or interactive prompts).
BIND="$BIND_DEFAULT"
PORT="$PORT_DEFAULT"
MAX_CLIENTS="$MAX_CLIENTS_DEFAULT"
MAX_PER_IP="$MAX_PER_IP_DEFAULT"
DO_START=1
DO_FIREWALL=auto
PRINT_ONLY=0
BIND_EXPLICIT=0   # 1 when --bind (or the auto prompt) chose a value
PORT_EXPLICIT=0   # 1 when --port (or the auto prompt) chose a value

# ANSI colour helpers — used only when stdout is a terminal.
if [ -t 1 ]; then
    BOLD=$'\033[1m'; GREEN=$'\033[32m'; YELLOW=$'\033[33m'; RED=$'\033[31m'; RESET=$'\033[0m'
else
    BOLD=""; GREEN=""; YELLOW=""; RED=""; RESET=""; CLEAR=""
fi
log()  { printf '%b[relay]%b %s\n' "$BOLD" "$RESET" "$*"; }
ok()   { printf '%b[relay]%b %b%s%b\n' "$BOLD" "$RESET" "$GREEN" "$*" "$RESET"; }
warn() { printf '%b[relay]%b %b%s%b\n' "$BOLD" "$RESET" "$YELLOW" "$*" "$RESET"; }
die()  { printf '%b[relay]%b %b%s%b\n' "$BOLD" "$RESET" "$RED" "$*" "$RESET" >&2; exit 1; }

usage() {
    cat <<EOF
install-relay.sh — install or update the self-hosted PPX relay

USAGE:
    curl -fsSL https://github.com/${REPO}/releases/latest/download/install-relay.sh \\
        | sudo bash
    sudo bash install-relay.sh --bind 0.0.0.0 --port 47393 \\
        --max-clients 256 --max-per-ip 8
    sudo bash install-relay.sh --funnel
    sudo bash install-relay.sh --funnel --tailscale-authkey tskey-...

OPTIONS:
    --funnel              Expose the relay publicly via Tailscale Funnel on
                          port ${FUNNEL_PORT_DEFAULT}. The installer downloads
                          and configures Tailscale, opens the browser-login
                          URL, and runs \`tailscale funnel --tcp=<port>\`.
                          The relay binds 127.0.0.1 only; no firewall rule.
    --direct              Bind a local address and open the firewall
                          (the classic mode; see --bind).
    --tailscale-authkey   tskey-auth-... value to authenticate the node
                          non-interactively. Only meaningful with --funnel.
                          Without it, the installer prints a login URL.
    --bind <addr>         Bind address for incoming TCP (direct mode).
                          Default: ${BIND_DEFAULT} (loopback only; local testing).
                          Use 0.0.0.0 to accept connections on every interface
                          (typical for a public self-hosted relay). IPv6
                          listening is supported by the binary but loopback /
                          dual-stack are the common cases.
    --port <port>         TCP port. Default: ${PORT_DEFAULT}.
                          In --funnel mode the port is forced to
                          ${FUNNEL_PORT_DEFAULT} (Tailscale Funnel only allows
                          443, 8443, 10000).
    --max-clients N       Maximum concurrent waiting + paired sockets.
                          Default: ${MAX_CLIENTS_DEFAULT}.
    --max-per-ip N        Maximum admitted sockets per source IP.
                          Default: ${MAX_PER_IP_DEFAULT}.
    --user                Install as a user systemd service (\$XDG_CONFIG_HOME/systemd).
                          Skips sudo when systemd is available for the current user.
    --system              Install as a system systemd service (requires sudo/root).
                          Default mode when running as root.
    --no-firewall         Do not add a firewall rule (direct mode only).
                          Default: opens the port via ufw / firewalld / iptables.
    --no-start            Write the unit file but do not enable or start it.
    --yes                 Accept all prompts with defaults (no interactive Q&A).
                          With --yes and no --funnel flag, defaults to direct.
    --print-tag           Resolve the latest (or pinned) tag and print it.
    --tag <tag>           Install a specific tag (for example v0.7.11-beta.1).
                          Default: latest stable release. Beta tags live at
                          /releases/download/<tag>/install-relay.sh — the
                          /releases/latest/download/ URL always serves stable.
    --uninstall           Remove the binary, the systemd unit, and (if it
                          was set up by this script) the Tailscale Funnel.
    --help                Show this help.

ENV:
    PPX_REPO             GitHub owner/name (default: ${REPO})
    PPX_VERSION          Specific tag (default: latest)
    PPX_YES              1 behaves like --yes
    PPX_NO_FIREWALL      1 behaves like --no-firewall
    PPX_TS_AUTHKEY       tskey-auth-... (alternative to --tailscale-authkey)

EXAMPLES:
    # Guided install on a fresh VPS (interactive):
    curl -fsSL https://github.com/${REPO}/releases/latest/download/install-relay.sh \\
        | sudo bash

    # Public self-hosted relay on a VPS (no Tailscale):
    curl -fsSL https://github.com/${REPO}/releases/latest/download/install-relay.sh \\
        | sudo bash -s -- --bind 0.0.0.0

    # Public relay exposed via Tailscale Funnel (free, hides the VPS IP):
    curl -fsSL https://github.com/${REPO}/releases/latest/download/install-relay.sh \\
        | sudo bash -s -- --funnel

    # Same as above but unattended (CI / fleet provisioning):
    sudo bash install-relay.sh --funnel --tailscale-authkey tskey-auth-...

    # Local testing (loopback only, no firewall rule):
    curl -fsSL https://github.com/${REPO}/releases/latest/download/install-relay.sh \\
        | sudo bash -s -- --no-firewall

    # Personal VPS as a non-root user:
    bash install-relay.sh --user --bind 0.0.0.0

    # Pin a specific release (e.g. rollback or install a beta):
    sudo bash install-relay.sh --tag v0.7.10

    # Install a beta — pre-release tags live at /releases/download/<tag>/,
    # NOT /releases/latest/download/ (which always serves the latest stable):
    curl -fsSL https://github.com/${REPO}/releases/download/v0.7.11-beta.1/install-relay.sh \\
        | sudo bash -s -- --tag v0.7.11-beta.1
EOF
}

# ---------------------------------------------------------------------------
# Argument parsing
# ---------------------------------------------------------------------------
parse_args() {
    while [ $# -gt 0 ]; do
        case "$1" in
            --funnel)               EXPOSURE_MODE=funnel; shift ;;
            --direct)               EXPOSURE_MODE=direct; shift ;;
            --tailscale-authkey)    TS_AUTHKEY="${2:?--tailscale-authkey needs a key}"; shift 2 ;;
            --tailscale-authkey=*)  TS_AUTHKEY="${1#--tailscale-authkey=}"; shift ;;
            --bind)                 BIND="${2:?--bind needs an address}"; BIND_EXPLICIT=1; shift 2 ;;
            --bind=*)               BIND="${1#--bind=}"; BIND_EXPLICIT=1; shift ;;
            --port)                 PORT="${2:?--port needs a number}"; PORT_EXPLICIT=1; shift 2 ;;
            --port=*)               PORT="${1#--port=}"; PORT_EXPLICIT=1; shift ;;
            --max-clients)          MAX_CLIENTS="${2:?--max-clients needs a number}"; shift 2 ;;
            --max-clients=*)        MAX_CLIENTS="${1#--max-clients=}"; shift ;;
            --max-per-ip)           MAX_PER_IP="${2:?--max-per-ip needs a number}"; shift 2 ;;
            --max-per-ip=*)         MAX_PER_IP="${1#--max-per-ip=}"; shift ;;
            --user)                 SYSTEM_LEVEL=user; shift ;;
            --system)               SYSTEM_LEVEL=system; shift ;;
            --no-firewall)          DO_FIREWALL=off; shift ;;
            --no-start)             DO_START=0; shift ;;
            --yes)                  AUTO_YES=1; shift ;;
            --print-tag)            PRINT_ONLY=1; shift ;;
            --tag)                  VERSION="${2:?--tag needs a tag like v0.7.11-beta.1}"; shift 2 ;;
            --tag=*)                VERSION="${1#--tag=}"; shift ;;
            --uninstall)            DO_UNINSTALL=1; shift ;;
            --help|-h)              usage; exit 0 ;;
            *)                      die "unknown option: $1 (try --help)" ;;
        esac
    done

    # Funnel and --bind are mutually exclusive: Funnel needs loopback.
    if [ "$EXPOSURE_MODE" = "funnel" ] && [ "$BIND_EXPLICIT" -eq 1 ]; then
        die "--funnel ignores --bind (Tailscale Funnel requires 127.0.0.1)"
    fi
}

# ---------------------------------------------------------------------------
# Environment detection
# ---------------------------------------------------------------------------
require_root_for_system() {
    if [ "$SYSTEM_LEVEL" = "system" ] && [ "$(id -u)" -ne 0 ]; then
        die "system-level install needs root — rerun with sudo or pass --user"
    fi
}

detect_target_triple() {
    local arch
    arch="$(uname -m)"
    case "$arch" in
        x86_64)  echo "x86_64-unknown-linux-gnu" ;;
        aarch64) echo "aarch64-unknown-linux-gnu" ;;
        *)       die "unsupported architecture: $arch (need x86_64 or aarch64)" ;;
    esac
}

has_systemd() {
    # systemctl is the surface both user and system units expose. If it's
    # missing we cannot install as a service; fall back to plain binary copy.
    command -v systemctl >/dev/null 2>&1
}

systemd_scope() {
    # Returns "--system" for the system manager, "--user" for the user manager.
    if [ "$SYSTEM_LEVEL" = "user" ]; then
        echo "--user"
    else
        echo "--system"
    fi
}

unit_path() {
    # Where the unit file ends up, depending on mode.
    if [ "$SYSTEM_LEVEL" = "user" ]; then
        echo "${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user/ppx-relay.service"
    else
        echo "/etc/systemd/system/ppx-relay.service"
    fi
}

# ---------------------------------------------------------------------------
# Tag / asset resolution
# ---------------------------------------------------------------------------
resolve_tag() {
    # Two endpoint shapes:
    #   /releases/latest           — when VERSION=latest
    #   /releases/tags/<tag>       — when VERSION=vX.Y.Z...
    # The bare /releases/<version> form only accepts numeric release IDs, so
    # passing "v0.7.11-beta.1" there 404s. Route tag-shaped values correctly.
    local api body tag
    case "$VERSION" in
        latest)          api="https://api.github.com/repos/${REPO}/releases/latest" ;;
        v*)              api="https://api.github.com/repos/${REPO}/releases/tags/${VERSION}" ;;
        *)               api="https://api.github.com/repos/${REPO}/releases/${VERSION}" ;;
    esac
    body="$(curl "${CURL_COMMON[@]}" -fsSL "$api")" \
        || die "could not resolve release ${VERSION} from ${REPO} (URL: $api)"
    tag="$(printf '%s' "$body" | grep -m1 '"tag_name"' | sed -E 's/.*"tag_name": *"([^"]+)".*/\1/')"
    [ -n "$tag" ] || die "could not parse tag_name from release metadata"
    printf '%s' "$tag"
}

compose_asset_name() {
    # The asset name is generated by the release workflow from the matrix.
    # Mirror that template exactly so a tag mismatch fails loudly.
    local tag_bare="$1" target="$2"
    echo "ppexchanger-${tag_bare#v}-${target}.tar.gz"
}

# ---------------------------------------------------------------------------
# Interactive prompts — only when stdin is a TTY and the user did not pre-set
# every value via flags or environment variables. Each prompt offers a
# sensible default and a short explainer.
# ---------------------------------------------------------------------------
prompt_if_needed() {
    if [ ! -t 0 ]; then
        AUTO_YES=1
    fi
    [ "$AUTO_YES" -eq 1 ] && return 0

    log "Guided install for the self-hosted PPX relay."
    log "Press Enter to accept the default for each question."
    echo

    local answer
    if [ "$EXPOSURE_MODE" = "auto" ]; then
        printf '%sHow should the relay be exposed to a peer?\n%s' "$BOLD" "$RESET"
        printf '  1) Tailscale Funnel (recommended) — relay listens on 127.0.0.1; Tailscale\n'
        printf '     forwards public TCP traffic from <node>.<tailnet>.ts.net:%s to it.\n' "$FUNNEL_PORT_DEFAULT"
        printf '     No port forwarding, no firewall rule, no public IP exposed.\n'
        printf '     Requires a free Tailscale account; first run prints a login URL.\n'
        printf '  2) Direct bind — listen on a local address and open the firewall.\n'
        printf '     0.0.0.0 is typical when remote PPX clients connect over the Internet.\n'
        printf '  3) Loopback only — local testing on this machine.\n'
        printf '  4) Custom bind address — IPv4 or IPv6 literal.\n'
        printf 'Choice [1]: '
        read -r answer
        case "${answer:-1}" in
            1|"") EXPOSURE_MODE=funnel ;;
            2)    EXPOSURE_MODE=direct; BIND="0.0.0.0"; BIND_EXPLICIT=1 ;;
            3)    EXPOSURE_MODE=direct; BIND="$BIND_DEFAULT"; BIND_EXPLICIT=1 ;;
            4)    EXPOSURE_MODE=direct
                  printf 'Bind address: '; read -r BIND
                  [ -n "$BIND" ] || die "bind address cannot be empty"
                  BIND_EXPLICIT=1 ;;
            *)    die "unknown choice: $answer" ;;
        esac
    fi

    if [ "$EXPOSURE_MODE" = "funnel" ]; then
        # Funnel forces loopback + port 10000. parse_args already rejected
        # an explicit --bind; do the same for --port here so interactive
        # users get the same fast-fail.
        if [ "$PORT_EXPLICIT" -eq 1 ] && [ "$PORT" != "$FUNNEL_PORT_DEFAULT" ]; then
            die "--port $PORT is not allowed with --funnel (Tailscale Funnel only accepts 443/8443/$FUNNEL_PORT_DEFAULT)"
        fi
        PORT="$FUNNEL_PORT_DEFAULT"
        PORT_EXPLICIT=1
        BIND="$BIND_DEFAULT"
        BIND_EXPLICIT=1
    else
        if [ "$BIND_EXPLICIT" -eq 0 ]; then
            printf '%sBind address [default %s]:%s ' "$BOLD" "$BIND_DEFAULT" "$RESET"
            read -r answer
            BIND="${answer:-$BIND_DEFAULT}"
        fi
        if [ "$PORT_EXPLICIT" -eq 0 ]; then
            printf '%sPort [default %s]:%s ' "$BOLD" "$PORT_DEFAULT" "$RESET"
            read -r answer
            PORT="${answer:-$PORT_DEFAULT}"
        fi
    fi

    printf '%sMax concurrent clients (waiting + paired) [default %s]:%s ' \
        "$BOLD" "$MAX_CLIENTS_DEFAULT" "$RESET"
    read -r answer
    MAX_CLIENTS="${answer:-$MAX_CLIENTS_DEFAULT}"

    printf '%sMax clients per source IP [default %s]:%s ' \
        "$BOLD" "$MAX_PER_IP_DEFAULT" "$RESET"
    read -r answer
    MAX_PER_IP="${answer:-$MAX_PER_IP_DEFAULT}"

    echo
    log "About to install with these settings:"
    printf '  mode        = %s\n' "$EXPOSURE_MODE"
    printf '  bind        = %s:%s\n' "$BIND" "$PORT"
    printf '  max_clients = %s\n' "$MAX_CLIENTS"
    printf '  max_per_ip  = %s\n' "$MAX_PER_IP"
    if [ "$SYSTEM_LEVEL" = "user" ]; then
        printf '  service     = user systemd service (no root)\n'
    else
        printf '  service     = system systemd service (root)\n'
    fi
    printf 'Proceed? [Y/n]: '
    read -r answer
    case "${answer:-Y}" in
        Y|y|"") ;;
        *) die "aborted by user" ;;
    esac
}

validate_inputs() {
    case "$PORT" in
        ''|*[!0-9]*) die "port must be a positive integer (got: $PORT)" ;;
    esac
    [ "$PORT" -ge 1 ] && [ "$PORT" -le 65535 ] \
        || die "port must be between 1 and 65535 (got: $PORT)"
    case "$MAX_CLIENTS" in
        ''|*[!0-9]*) die "--max-clients must be a positive integer (got: $MAX_CLIENTS)" ;;
    esac
    [ "$MAX_CLIENTS" -ge 1 ] || die "--max-clients must be at least 1"
    case "$MAX_PER_IP" in
        ''|*[!0-9]*) die "--max-per-ip must be a positive integer (got: $MAX_PER_IP)" ;;
    esac
    [ "$MAX_PER_IP" -ge 1 ] || die "--max-per-ip must be at least 1"
    [ -n "$BIND" ] || die "--bind cannot be empty"
}

# ---------------------------------------------------------------------------
# Download, verify, install binary
# ---------------------------------------------------------------------------
TMPDIR="$(mktemp -d -t ppx-relay-install.XXXXXX)"
trap 'rm -rf "$TMPDIR"' EXIT

WORKDIR="$TMPDIR/staging"
mkdir -p "$WORKDIR/bin"

download_release() {
    local target="$1"
    TARGET_TRIPLE="$target"
    log "resolving release (${VERSION})..."
    TAG="$(resolve_tag)"
    ok "release tag: $TAG"

    TAG_BARE="${TAG#v}"
    ASSET="$(compose_asset_name "$TAG" "$target")"
    SUMS_ASSET="SHA256SUMS"
    URL_BASE="https://github.com/${REPO}/releases/download/${TAG}"
    TARBALL="${TMPDIR}/${ASSET}"
    SUMS_FILE="${TMPDIR}/${SUMS_ASSET}"

    log "downloading ${ASSET}..."
    curl "${CURL_COMMON[@]}" -fsSL -o "$TARBALL" "${URL_BASE}/${ASSET}" \
        || die "download failed — check that release $TAG has asset $ASSET for $target"

    log "downloading SHA256SUMS..."
    curl "${CURL_COMMON[@]}" -fsSL -o "$SUMS_FILE" "${URL_BASE}/${SUMS_ASSET}" \
        || die "checksum download failed"

    log "verifying checksum..."
    EXPECTED="$(awk -v a="$ASSET" '$2 == a {print $1}' "$SUMS_FILE")"
    [ -n "$EXPECTED" ] || die "${ASSET} not listed in SHA256SUMS"
    ACTUAL="$(sha256sum "$TARBALL" | awk '{print $1}')"
    [ "$EXPECTED" = "$ACTUAL" ] \
        || die "checksum mismatch (expected $EXPECTED, got $ACTUAL)"
    ok "checksum verified"

    log "extracting relay binary..."
    tar -xzf "$TARBALL" -C "$WORKDIR"
    [ -f "$WORKDIR/bin/ppx-relay" ] \
        || die "expected binary 'ppx-relay' not found in tarball"
    chmod +x "$WORKDIR/bin/ppx-relay"
}

install_binary() {
    if [ "$SYSTEM_LEVEL" = "user" ]; then
        USER_BIN_DIR="${XDG_BIN_HOME:-$HOME/.local/bin}"
        mkdir -p "$USER_BIN_DIR"
        install -m 0755 "$WORKDIR/bin/ppx-relay" "$USER_BIN_DIR/ppx-relay"
        INSTALLED="$USER_BIN_DIR/ppx-relay"
        case ":$PATH:" in
            *":$USER_BIN_DIR:"*) ;;
            *) warn "$USER_BIN_DIR is not in PATH; add it or invoke \$HOME/.local/bin/ppx-relay directly" ;;
        esac
    else
        install -m 0755 -D "$WORKDIR/bin/ppx-relay" /usr/local/bin/ppx-relay
        INSTALLED="/usr/local/bin/ppx-relay"
    fi
    ok "installed $INSTALLED"
}

# ---------------------------------------------------------------------------
# systemd unit (installed; also serves as a no-systemd fallback when copied
# elsewhere) — bound to the user-selected address, port, and quotas.
# ---------------------------------------------------------------------------
write_unit() {
    UNIT="$(unit_path)"
    if [ "$SYSTEM_LEVEL" = "user" ]; then
        mkdir -p "$(dirname "$UNIT")"
    else
        # /etc/systemd/system is created by systemd itself; nothing to do.
        :
    fi

    # Systemd hardening that is identical for system and user units. User
    # units simply run as the calling user; DynamicUser is a system-only
    # concern and is omitted from the user unit to keep the format clean.
    DYNAMIC_USER=$([ "$SYSTEM_LEVEL" = "system" ] && echo 'DynamicUser=yes')
    RELAY_GROUP=$([ "$SYSTEM_LEVEL" = "system" ] && echo 'Group=nogroup')
    AMBIENT_CAPS=$([ "$SYSTEM_LEVEL" = "system" ] && printf 'AmbientCapabilities=\nCapabilityBoundingSet=\n')
    WANTED_BY=$([ "$SYSTEM_LEVEL" = "user" ] && echo "default.target" || echo "multi-user.target")

    cat > "$UNIT" <<EOF
[Unit]
Description=PPX pair relay
Documentation=https://github.com/${REPO}/tree/main/docs/relay.md
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
${DYNAMIC_USER}
${RELAY_GROUP}
ExecStart=${INSTALLED} --bind ${BIND}:${PORT} --max-clients ${MAX_CLIENTS} --max-per-ip ${MAX_PER_IP}
Restart=on-failure
RestartSec=3s
NoNewPrivileges=yes
${AMBIENT_CAPS}
PrivateTmp=yes
PrivateDevices=yes
ProtectHome=yes
ProtectSystem=strict
ProtectControlGroups=yes
ProtectKernelLogs=yes
ProtectKernelModules=yes
ProtectKernelTunables=yes
ProtectProc=invisible
ProcSubset=pid
RestrictAddressFamilies=AF_UNIX AF_INET AF_INET6
RestrictNamespaces=yes
RestrictRealtime=yes
RestrictSUIDSGID=yes
SystemCallArchitectures=native
UMask=0077

[Install]
WantedBy=${WANTED_BY}
EOF
    ok "wrote $UNIT"
}

enable_and_start() {
    local scope
    scope="$(systemd_scope)"
    systemctl "$scope" daemon-reload
    systemctl "$scope" enable ppx-relay.service
    if [ "$DO_START" -eq 1 ]; then
        systemctl "$scope" restart ppx-relay.service
    fi
}

verify_listening() {
    # Give the unit a moment to bind. ss/netstat fallback for systems where
    # `ss` is missing (older minimal images). The check is informational;
    # we never fail the install on listener timing alone.
    local port="$1"
    sleep 1
    if command -v ss >/dev/null 2>&1; then
        if ss -lnt 2>/dev/null | awk '{print $4}' | grep -qE "[:.]${port}\$"; then
            ok "ppx-relay is listening on port ${port}"
            return 0
        fi
    fi
    if command -v netstat >/dev/null 2>&1; then
        if netstat -lnt 2>/dev/null | awk '{print $4}' | grep -qE "[:.]${port}\$"; then
            ok "ppx-relay is listening on port ${port}"
            return 0
        fi
    fi
    warn "ppx-relay process is up but port ${port} is not yet visible. Check \`journalctl -u ppx-relay\`."
    return 1
}

# ---------------------------------------------------------------------------
# Firewall setup. Tries ufw, firewalld, then iptables — first match wins.
# Idempotent: re-running the installer must not create duplicate rules.
# ---------------------------------------------------------------------------
open_firewall_port() {
    if [ "$DO_FIREWALL" = "off" ]; then
        log "firewall rule skipped (--no-firewall)"
        return 0
    fi
    # Loopback needs no firewall rule.
    case "$BIND" in
        127.*|localhost|::1) log "loopback bind — firewall rule skipped"; return 0 ;;
    esac
    if [ "$SYSTEM_LEVEL" = "user" ] && [ "$DO_FIREWALL" = "auto" ]; then
        # A user service cannot modify system firewall; skip and tell the user.
        warn "user service cannot add system firewall rules — open port ${PORT} manually."
        return 0
    fi

    if command -v ufw >/dev/null 2>&1; then
        if ufw status 2>/dev/null | grep -qE "${PORT}/tcp"; then
            log "ufw already allows ${PORT}/tcp"
        else
            log "opening ${PORT}/tcp via ufw..."
            ufw allow "${PORT}/tcp" || warn "ufw allow failed — add the rule manually"
        fi
        return 0
    fi

    if command -v firewall-cmd >/dev/null 2>&1 && systemctl is-active --quiet firewalld; then
        if firewall-cmd --query-port="${PORT}/tcp" 2>/dev/null; then
            log "firewalld already allows ${PORT}/tcp"
        else
            log "opening ${PORT}/tcp via firewalld..."
            firewall-cmd --permanent --add-port="${PORT}/tcp" \
                && firewall-cmd --add-port="${PORT}/tcp" \
                || warn "firewalld rule failed — add it manually"
        fi
        return 0
    fi

    if command -v iptables >/dev/null 2>&1; then
        if iptables -C INPUT -p tcp --dport "$PORT" -j ACCEPT 2>/dev/null; then
            log "iptables already allows ${PORT}/tcp"
        else
            log "opening ${PORT}/tcp via iptables..."
            iptables -I INPUT -p tcp --dport "$PORT" -j ACCEPT \
                || warn "iptables rule failed (insufficient privileges?) — add it manually"
        fi
        return 0
    fi

    warn "no firewall tooling detected (ufw/firewalld/iptables). Open inbound TCP ${PORT} manually."
}

# ---------------------------------------------------------------------------
# Tailscale Funnel setup. Installs Tailscale if missing, authenticates the
# node (browser login by default, auth key with --tailscale-authkey), and
# enables raw TCP Funnel forwarding from <node>.<tailnet>.ts.net:$PORT to
# 127.0.0.1:$PORT. Sets $PUBLIC_HOSTNAME for the final report.
# ---------------------------------------------------------------------------
ts_authenticated() {
    # "BackendState": "Running" means tailscaled finished auth + key expiry.
    tailscale status --json 2>/dev/null \
        | grep -q '"BackendState": "Running"'
}

ts_wait_for_auth() {
    local deadline=$((SECONDS + 600))   # 10 minutes to click the email link
    local heartbeat=$((SECONDS + 30))   # first heartbeat 30s after start
    while [ "$SECONDS" -lt "$deadline" ]; do
        if ts_authenticated; then
            return 0
        fi
        # Heartbeat so a long-running wait doesn't look like a hang.
        if [ "$SECONDS" -ge "$heartbeat" ]; then
            log "still waiting for Tailscale login... (open the URL above in any browser)"
            heartbeat=$((SECONDS + 30))
        fi
        sleep 2
    done
    return 1
}

ts_install() {
    if command -v tailscale >/dev/null 2>&1; then
        ok "tailscale present: $(tailscale version 2>/dev/null | head -n1)"
        return 0
    fi
    log "installing Tailscale (official script)..."
    if [ "$SYSTEM_LEVEL" = "system" ]; then
        curl "${CURL_COMMON[@]}" -fsSL https://tailscale.com/install.sh \
            | sh >/dev/null \
            || die "Tailscale install script failed. Install manually: https://tailscale.com/download/linux"
    else
        warn "user-mode install: Tailscale must already be installed and reachable."
        command -v tailscale >/dev/null 2>&1 \
            || die "tailscale binary not found in PATH; install it first or rerun with sudo."
    fi
    ok "tailscale installed: $(tailscale version 2>/dev/null | head -n1)"
}

ts_ensure_running() {
    # On distros where Tailscale was installed via its official script,
    # tailscaled runs as a systemd unit. Use systemctl to start it when
    # systemd is available — running `tailscaled --state=...` as a bare
    # background process silently fails on Debian because the binary is
    # configured for systemd (socket activation, environment files, ...).
    if command -v systemctl >/dev/null 2>&1; then
        systemctl is-active --quiet tailscaled 2>/dev/null \
            && return 0  # already running under systemd
        log "starting tailscaled via systemd..."
        systemctl enable --now tailscaled >/dev/null 2>&1 \
            && { sleep 2; return 0; }
        # systemctl failed — fall through to the raw-launch attempt
    fi
    # No systemd or systemd start failed — try raw daemon launch as last
    # resort. This works on some configs (e.g. macOS, Docker, WSL2).
    if ! ts_authenticated; then
        log "starting tailscaled (raw daemon)..."
        tailscaled --state=/var/lib/tailscale/tailscaled.state >/dev/null 2>&1 &
        sleep 2
    fi
}

ts_authenticate() {
    _ts_trace 6830 "ts_authenticate begin"
    if ts_authenticated; then
        _ts_trace 6840 "already authed"
        ok "Tailscale already authenticated (BackendState=Running)"
        return 0
    fi
    if [ -n "$TS_AUTHKEY" ]; then
        log "authenticating with provided auth key..."
        tailscale up --authkey="$TS_AUTHKEY" --timeout=2m \
            || die "tailscale up --authkey failed; check the key and tailnet ACL."
        ok "authenticated via auth key"
        return 0
    fi

    # Browser-login flow. tailscale up prints the login URL to stderr and
    # blocks waiting for the user to click it in any browser.
    #
    # Two terminal-routing pitfalls to avoid:
    #   1. Running `curl ... | sudo bash` makes the script's stderr land
    #      back at the curl pipe's source — not always the operator's
    #      terminal — so redirecting tailscale up's stderr to /dev/null
    #      silently swallows the URL (which is what an earlier version
    #      did, and what operators on headless boxes were hitting).
    #   2. A Tailscale GUI client on the same machine can react to the
    #      IPN BrowseToURL notification by silently launching the desktop
    #      default browser. The installer's contract is "display the URL"
    #      — never "silently open it" — even if the host has a desktop
    #      session the operator is not looking at.
    #
    # Fix: capture tailscale up's stderr to a log file with
    # BROWSER=none + DISPLAY= + WAYLAND_DISPLAY= so no GUI helper launches,
    # poll the log for the URL, then re-print it loudly through the
    # installer's own [relay] stream and also write it to
    # /run/ppx-relay-auth-url as a backup record.
    #
    # IMPORTANT: run tailscale up SYNCHRONOUSLY (no `&`) and gate the wait
    # on BackendState, NOT on a backgrounded PID. Operators running the
    # installer a second time on an already-authed host will see tailscale
    # up exit instantly without printing a URL — in that case BackendState
    # is already Running and we should just continue. Backgrounding the
    # process caused a race where the parent script could exit before the
    # wait loop completed (or output was silently swallowed by stdout
    # buffering under `curl | sudo bash`).
    log "opening Tailscale browser login..."
    _ts_trace 7200 "log opened, pre-bg"
    local ts_log="/run/ppx-relay-auth-url.log"
    : > "$ts_log" 2>/dev/null || ts_log="$(mktemp -t ppx-ts-auth.XXXXXX)"
    chmod 0644 "$ts_log" 2>/dev/null || true
    _ts_trace 7210 "ts_log=$ts_log"

    # Never auto-open the URL in a browser. The Tailscale CLI honours
    # $BROWSER (xdg-open, sensible-browser) and a Tailscale GUI client on
    # the same machine can react to the IPN BrowseToURL notification by
    # launching the desktop default browser. BROWSER=none disables the
    # xdg-open path; clearing DISPLAY and WAYLAND_DISPLAY discourages GUI
    # helpers from launching.
    #
    # Run `tailscale up` with `&` AND `disown` so its lifetime is fully
    # decoupled from the installer's bash process: when the installer
    # exits, tailscale up is reparented to init and continues to wait
    # for the user to click the link. Without disown, bash's job-control
    # SIGHUP-on-exit would kill it before the URL is clicked.
    BROWSER=none DISPLAY= WAYLAND_DISPLAY= \
        nohup tailscale up --timeout=10m >"$ts_log" 2>&1 &
    local up_pid=$!
    disown "$up_pid" 2>/dev/null || true
    _ts_trace 7340 "up_pid=$up_pid"

    # Wait up to ~15s for tailscale up to print the URL. Login URLs start
    # with "https://login.tailscale.com/a/" or "https://<control-plane>/a/".
    local login_url="" deadline=$((SECONDS + 15))
    _ts_trace 7400 "starting url poll"
    while [ "$SECONDS" -lt "$deadline" ] && [ -z "$login_url" ]; do
        login_url="$(grep -oE 'https://[a-zA-Z0-9./_-]+/a/[a-f0-9]+' "$ts_log" 2>/dev/null | head -n1 || true)"
        [ -z "$login_url" ] && sleep 1
    done
    _ts_trace 7440 "url poll done login_url=${login_url:-<empty>}"

    if [ -n "$login_url" ]; then
        # Make the URL unmistakable: bold yellow header, repeated thrice in
        # case the first line scrolls past, and dump the full log path.
        warn "============================================================"
        warn "Tailscale login URL (also written to: $ts_log):"
        warn ""
        warn "  $login_url"
        warn ""
        warn "Open this URL in any browser to authenticate this host."
        warn "============================================================"
        cp "$ts_log" /run/ppx-relay-auth-url 2>/dev/null || true
    else
        warn "Tailscale did not print a login URL within 15s."
        warn "Last lines of the captured log ($ts_log):"
        warn "------------------------------------------------------------"
        if [ -r "$ts_log" ]; then
            tail -n 20 "$ts_log" | sed 's/^/[tail] /' || warn "(could not read log)"
        else
            warn "(log file not present)"
        fi
        warn "------------------------------------------------------------"
        warn "Run \`tailscale up --timeout=10m\` manually in another shell,"
        warn "or use --tailscale-authkey for unattended installs."
    fi

    # Wait up to 10 minutes for BackendState=Running. If the host is
    # already authenticated, ts_authenticated returns immediately.
    if ts_wait_for_auth; then
        kill "$up_pid" 2>/dev/null || true
        wait "$up_pid" 2>/dev/null || true
        ok "authenticated via browser login"
        return 0
    fi

    kill "$up_pid" 2>/dev/null || true
    wait "$up_pid" 2>/dev/null || true
    die "Tailscale login did not complete in 10 minutes. Re-run the installer, run \`tailscale up\` manually, or use --tailscale-authkey for unattended installs. Last login URL was saved to /run/ppx-relay-auth-url."
}

ts_enable_funnel() {
    log "enabling Tailscale Funnel for TCP ${PORT} -> ${BIND}:${PORT}..."
    # Funnel requires MagicDNS and a Tailscale account that has Funnel
    # enabled (admin must allow it in the Tailscale admin panel for
    # personal/free tailnets; paid plans enable it by default).
    if ! tailscale funnel --bg --tcp="$PORT" "${BIND}:${PORT}" 2>&1; then
        die "tailscale funnel failed. If your tailnet has Funnel disabled, enable it at https://login.tailscale.com/admin/acls/file (or under Settings → Funnel)."
    fi
    ok "Funnel forwarding TCP ${PORT} -> ${BIND}:${PORT}"
}

ts_public_hostname() {
    # tailscale status --json has Self.MagicDNSSuffix (e.g. "tail.ts.net.")
    # and Self.NodeName (FQDN with trailing dot). The Funnel hostname is
    # <short-name>.<MagicDNSSuffix-without-trailing-dot>.
    local status_json short suffix
    status_json="$(tailscale status --json 2>/dev/null)" || return 1
    # Prefer NodeName and MagicDNSSuffix which are always present post-auth.
    short="$(printf '%s' "$status_json" \
        | python3 -c 'import json,sys; d=json.load(sys.stdin); n=d["Self"]["NodeName"].rstrip("."); s=d["Self"]["MagicDNSSuffix"].rstrip("."); print(f"{n}.{s}" if s and n else "")' 2>/dev/null)"
    if [ -n "$short" ]; then
        printf '%s' "$short"
        return 0
    fi
    # Last-resort fallback: scrape the funnel status output.
    tailscale funnel status 2>/dev/null \
        | grep -oE '[a-zA-Z0-9-]+\.[a-zA-Z0-9.-]+\.ts\.net' \
        | head -n1
}

setup_tailscale_funnel() {
    ts_install
    ts_ensure_running
    ts_authenticate
    ts_enable_funnel

    PUBLIC_HOSTNAME="$(ts_public_hostname || true)"
    if [ -z "$PUBLIC_HOSTNAME" ]; then
        warn "could not auto-discover Funnel hostname. Run: tailscale status"
        PUBLIC_HOSTNAME="<node>.<tailnet>.ts.net"
    fi
    ok "public Funnel address: ${PUBLIC_HOSTNAME}:${PORT}"
}

teardown_tailscale_funnel() {
    if command -v tailscale >/dev/null 2>&1; then
        tailscale funnel --tcp="$PORT" off 2>/dev/null \
            || tailscale funnel off 2>/dev/null \
            || warn "could not disable Funnel — run: tailscale funnel off"
        ok "Tailscale Funnel disabled"
    fi
}

# ---------------------------------------------------------------------------
# Final report — tells the operator what is installed, how to verify, and
# what to share with a peer.
# ---------------------------------------------------------------------------
final_report() {
    local scope
    scope="$(systemd_scope)"
    echo
    ok "ppx-relay install complete"
    echo
    printf '%sInstalled binary:%s   %s\n' "$BOLD" "$RESET" "$INSTALLED"
    printf '%sService:%s           %s\n' "$BOLD" "$RESET" \
        "$( [ "$SYSTEM_LEVEL" = "user" ] && echo 'user' || echo 'system' ) ppx-relay.service"
    printf '%sExposure:%s          %s\n' "$BOLD" "$RESET" \
        "$( [ "$EXPOSURE_MODE" = "funnel" ] && echo "Tailscale Funnel" || echo "direct bind" )"
    printf '%sListening on:%s      %s:%s\n' "$BOLD" "$RESET" "$BIND" "$PORT"
    printf '%sLimits:%s             %s clients, %s per IP\n' \
        "$BOLD" "$RESET" "$MAX_CLIENTS" "$MAX_PER_IP"
    echo
    printf '%sVerify it is running:%s\n' "$BOLD" "$RESET"
    printf '    systemctl %s status ppx-relay\n' "$scope"
    printf '    journalctl -u ppx-relay -n 50 --no-pager\n'
    if [ "$EXPOSURE_MODE" = "funnel" ]; then
        printf '    tailscale funnel status\n'
    fi
    echo
    printf '%sShare with a peer:%s\n' "$BOLD" "$RESET"
    printf '    1. On this relay host, run:  ppx --relay-token\n'
    printf '    2. Send the 64-hex output + this server address to the peer.\n'
    printf '    3. Each side runs:  ppx --relay-config ~/.config/ppexchanger/relay.conf\n'
    printf '       with contents:\n'
    printf '           server = "%s:%s"\n' "$PUBLIC_HOSTNAME" "$PORT"
    printf '           room   = "<token from ppx --relay-token>"\n'
    printf '           peer_key = "<other side'\''s 64-hex public key from ppx --gen-identity>"\n'
    echo
    if [ "$EXPOSURE_MODE" = "funnel" ]; then
        printf '%sUpdate later:%s       rerun this script (idempotent).\n' "$BOLD" "$RESET"
        printf '%sUninstall:%s          bash install-relay.sh --uninstall (also disables Funnel)\n' "$BOLD" "$RESET"
    else
        printf '%sUpdate later:%s       rerun this script (idempotent).\n' "$BOLD" "$RESET"
        printf '%sUninstall:%s          bash install-relay.sh --uninstall\n' "$BOLD" "$RESET"
    fi
}

# ---------------------------------------------------------------------------
# Top-level flows
# ---------------------------------------------------------------------------
do_install() {
    # Choose install mode if not set explicitly.
    if [ "$SYSTEM_LEVEL" = "auto" ]; then
        if [ "$(id -u)" -eq 0 ]; then
            SYSTEM_LEVEL=system
        elif has_systemd; then
            SYSTEM_LEVEL=user
        else
            # Bare-metal without systemd: copy the binary, no service.
            SYSTEM_LEVEL=none
        fi
    fi

    if [ "$SYSTEM_LEVEL" = "none" ]; then
        warn "no systemd detected — installing the binary only."
        TARGET="$(detect_target_triple)"
        download_release "$TARGET"
        install_binary
        ok "binary installed at $INSTALLED"
        echo
        printf 'Run it manually:\n'
        printf '    %s --bind %s:%s --max-clients %s --max-per-ip %s\n' \
            "$INSTALLED" "$BIND" "$PORT" "$MAX_CLIENTS" "$MAX_PER_IP"
        return 0
    fi

    require_root_for_system
    validate_inputs
    prompt_if_needed
    validate_inputs

    # Resolve any remaining "auto" — non-interactive runs (--yes, piped from
    # curl) default to direct-bind loopback. To use Funnel non-interactively,
    # the operator must pass --funnel (and ideally --tailscale-authkey).
    if [ "$EXPOSURE_MODE" = "auto" ]; then
        EXPOSURE_MODE=direct
        BIND="$BIND_DEFAULT"
    fi

    # Funnel mode forces loopback + port 10000 and a fresh state.
    if [ "$EXPOSURE_MODE" = "funnel" ]; then
        BIND="$BIND_DEFAULT"
        PORT="$FUNNEL_PORT_DEFAULT"
    fi

    TARGET="$(detect_target_triple)"
    download_release "$TARGET"
    install_binary

    if has_systemd; then
        write_unit
        enable_and_start
        verify_listening "$PORT" || true
    else
        warn "systemd missing — wrote no unit. Start ${INSTALLED} manually."
    fi

    if [ "$EXPOSURE_MODE" = "funnel" ]; then
        setup_tailscale_funnel
        # Funnel replaces the firewall; the relay stays on loopback.
    else
        open_firewall_port
        # Best-effort: figure out the public hostname so the operator knows
        # what to share. Never fatal — a LAN-only relay has nothing public.
        PUBLIC_HOSTNAME="$BIND"
        if [ "$BIND" = "0.0.0.0" ] || [ "$BIND" = "::" ]; then
            PUBLIC_HOSTNAME="${PUBLIC_HOSTNAME_OVERRIDE:-$(hostname -f 2>/dev/null || hostname || echo this-host)}"
        fi
    fi

    final_report
}

do_uninstall() {
    local scope unit
    if [ -f "/etc/systemd/system/ppx-relay.service" ] && [ "$(id -u)" -eq 0 ]; then
        systemctl stop ppx-relay.service || true
        systemctl disable ppx-relay.service || true
        rm -f /etc/systemd/system/ppx-relay.service
        systemctl daemon-reload
        ok "removed /etc/systemd/system/ppx-relay.service"
    elif [ -f "${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user/ppx-relay.service" ]; then
        systemctl --user stop ppx-relay.service || true
        systemctl --user disable ppx-relay.service || true
        rm -f "${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user/ppx-relay.service"
        systemctl --user daemon-reload
        ok "removed user ppx-relay.service"
    fi
    for cand in /usr/local/bin/ppx-relay "$HOME/.local/bin/ppx-relay"; do
        if [ -f "$cand" ]; then
            rm -f "$cand"
            ok "removed $cand"
        fi
    done
    # Best-effort: tear down Funnel if it looks like this script set it up.
    # The Funnel rule is keyed by the port we used; try both 10000 and the
    # legacy default 47393 just in case.
    if command -v tailscale >/dev/null 2>&1; then
        for p in "$FUNNEL_PORT_DEFAULT" "$PORT_DEFAULT"; do
            tailscale funnel --tcp="$p" off 2>/dev/null || true
        done
        # `funnel off` clears everything (HTTP+HTTPS+TCP). Use it only if no
        # other Funnel services are running on this tailnet — keep it last.
        if tailscale funnel status 2>/dev/null | grep -q .; then
            : # leave any other Funnel services intact
        else
            tailscale funnel off 2>/dev/null || true
        fi
    fi
    log "uninstall complete"
}

# ---------------------------------------------------------------------------
# Entry point
# ---------------------------------------------------------------------------
parse_args "$@"

# `--print-tag` short-circuits everything else.
if [ "${PRINT_ONLY:-0}" -eq 1 ]; then
    resolve_tag
    exit 0
fi

# Must run on a supported target. The release pipeline only ships the
# relay binary in the Linux tarballs (systemd hosts), so the installer
# is Linux-only. Build the binary from source on other platforms.
case "$(uname -s)" in
    Linux) ;;
    *)     die "ppx-relay installer targets Linux (systemd). On $(uname -s), \`cargo build --release --bin ppx-relay\` is the only path." ;;
esac

if [ "${DO_UNINSTALL:-0}" -eq 1 ]; then
    do_uninstall
    exit 0
fi

do_install
