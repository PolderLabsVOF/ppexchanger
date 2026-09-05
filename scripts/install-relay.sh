#!/usr/bin/env bash
# install-relay.sh — install or update the self-hosted PPX relay (ppx-relay).
#
# Usage:
#   curl -fsSL https://github.com/PolderLabsVOF/ppexchanger/releases/latest/download/install-relay.sh | sudo bash
#   curl -fsSL .../install-relay.sh | sudo bash -s -- --bind 0.0.0.0 --port 47393
#   bash install-relay.sh --uninstall
#   bash install-relay.sh --user   # rootless user service
#
# Guided interactive install — without flags, the script asks four questions
# (bind address, port, max-clients, max-per-ip), confirms the answers, then
# handles every step automatically: download, checksum, install the binary,
# write the systemd unit, enable and start the service, open the firewall.
# Re-run the script to update in place.
#
# Supported: Linux x86_64 / aarch64 (glibc), systemd. macOS or Windows run
# the binary directly with `./ppx-relay --bind ...` — no installer needed.

set -euo pipefail
IFS=$' \t\n'

# Bound downloads so a stuck TLS handshake can't trap the installer forever.
CURL_COMMON=(--retry 3 --retry-delay 1 --max-time 120)

REPO="${PPX_REPO:-PolderLabsVOF/ppexchanger}"
VERSION="${PPX_VERSION:-latest}"
BIND_DEFAULT="127.0.0.1"
PORT_DEFAULT="47393"
MAX_CLIENTS_DEFAULT="128"
MAX_PER_IP_DEFAULT="16"

# Detect install mode up front: --user skips root, --system requires sudo.
SYSTEM_LEVEL=auto
AUTO_YES=0

# Pre-set configuration (overridden by CLI flags or interactive prompts).
BIND="$BIND_DEFAULT"
PORT="$PORT_DEFAULT"
MAX_CLIENTS="$MAX_CLIENTS_DEFAULT"
MAX_PER_IP="$MAX_PER_IP_DEFAULT"
DO_START=1
DO_FIREWALL=auto
PRINT_ONLY=0

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

OPTIONS:
    --bind <addr>        Bind address for incoming TCP.
                          Default: ${BIND_DEFAULT} (loopback only; local testing).
                          Use 0.0.0.0 to accept connections on every interface
                          (typical for a public self-hosted relay). IPv6
                          listening is supported by the binary but loopback /
                          dual-stack are the common cases.
    --port <port>        TCP port. Default: ${PORT_DEFAULT}.
    --max-clients N      Maximum concurrent waiting + paired sockets.
                          Default: ${MAX_CLIENTS_DEFAULT}.
    --max-per-ip N       Maximum admitted sockets per source IP.
                          Default: ${MAX_PER_IP_DEFAULT}.
    --user               Install as a user systemd service (\$XDG_CONFIG_HOME/systemd).
                          Skips sudo when systemd is available for the current user.
    --system             Install as a system systemd service (requires sudo/root).
                          Default mode when running as root.
    --no-firewall        Do not add a firewall rule. Default: opens the port
                          via ufw / firewalld / iptables when available.
    --no-start           Write the unit file but do not enable or start it.
    --yes                Accept all prompts with defaults (no interactive Q&A).
    --print-tag          Resolve the latest (or pinned) tag and print it.
    --uninstall          Remove the binary and the systemd unit.
    --help               Show this help.

ENV:
    PPX_REPO        GitHub owner/name (default: ${REPO})
    PPX_VERSION     Specific tag (default: latest)
    PPX_YES         1 behaves like --yes
    PPX_NO_FIREWALL 1 behaves like --no-firewall

EXAMPLES:
    # Public self-hosted relay on a VPS:
    curl -fsSL https://github.com/${REPO}/releases/latest/download/install-relay.sh \\
        | sudo bash -s -- --bind 0.0.0.0

    # Local testing (loopback only, no firewall rule):
    curl -fsSL https://github.com/${REPO}/releases/latest/download/install-relay.sh \\
        | sudo bash -s -- --no-firewall

    # Personal VPS as a non-root user:
    bash install-relay.sh --user --bind 0.0.0.0

    # Pin a specific release (e.g. rollback):
    sudo bash install-relay.sh --tag v0.7.10
EOF
}

# ---------------------------------------------------------------------------
# Argument parsing
# ---------------------------------------------------------------------------
parse_args() {
    while [ $# -gt 0 ]; do
        case "$1" in
            --bind)        BIND="${2:?--bind needs an address}"; shift 2 ;;
            --bind=*)      BIND="${1#--bind=}"; shift ;;
            --port)        PORT="${2:?--port needs a number}"; shift 2 ;;
            --port=*)      PORT="${1#--port=}"; shift ;;
            --max-clients) MAX_CLIENTS="${2:?--max-clients needs a number}"; shift 2 ;;
            --max-clients=*) MAX_CLIENTS="${1#--max-clients=}"; shift ;;
            --max-per-ip)  MAX_PER_IP="${2:?--max-per-ip needs a number}"; shift 2 ;;
            --max-per-ip=*) MAX_PER_IP="${1#--max-per-ip=}"; shift ;;
            --user)        SYSTEM_LEVEL=user; shift ;;
            --system)      SYSTEM_LEVEL=system; shift ;;
            --no-firewall) DO_FIREWALL=off; shift ;;
            --no-start)    DO_START=0; shift ;;
            --yes)         AUTO_YES=1; shift ;;
            --print-tag)   PRINT_ONLY=1; shift ;;
            --uninstall)   DO_UNINSTALL=1; shift ;;
            --help|-h)     usage; exit 0 ;;
            *)             die "unknown option: $1 (try --help)" ;;
        esac
    done
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
    local api="https://api.github.com/repos/${REPO}/releases/${VERSION}"
    local body tag
    body="$(curl "${CURL_COMMON[@]}" -fsSL "$api")" \
        || die "could not resolve release ${VERSION} from ${REPO}"
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
    printf '%sWhere should the relay listen?\n%s' "$BOLD" "$RESET"
    printf '  1) 127.0.0.1:%s  — loopback only. Pick this for local testing on this machine.\n' "$PORT"
    printf '  2) 0.0.0.0:%s    — every interface. Required when remote PPX clients connect.\n' "$PORT"
    printf '     Open inbound TCP %s on this host'\''s firewall AND on the cloud-provider panel.\n' "$PORT"
    printf '  3) Custom IPv4 / IPv6 bind address (for example [::]:%s for IPv6).\n' "$PORT"
    printf 'Choice [1]: '
    read -r answer
    case "${answer:-1}" in
        1|"") BIND="$BIND_DEFAULT" ;;
        2)    BIND="0.0.0.0" ;;
        3)    printf 'Bind address: '; read -r BIND
              [ -n "$BIND" ] || die "bind address cannot be empty" ;;
        *)    die "unknown choice: $answer" ;;
    esac

    printf '%sPort [default %s]:%s ' "$BOLD" "$PORT_DEFAULT" "$RESET"
    read -r answer
    PORT="${answer:-$PORT_DEFAULT}"

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
    printf '  bind       = %s:%s\n' "$BIND" "$PORT"
    printf '  max_clients = %s\n' "$MAX_CLIENTS"
    printf '  max_per_ip  = %s\n' "$MAX_PER_IP"
    if [ "$SYSTEM_LEVEL" = "user" ]; then
        printf '  mode        = user systemd service (no root)\n'
    else
        printf '  mode        = system systemd service (root)\n'
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
    printf '%sListening on:%s      %s:%s\n' "$BOLD" "$RESET" "$BIND" "$PORT"
    printf '%sLimits:%s             %s clients, %s per IP\n' \
        "$BOLD" "$RESET" "$MAX_CLIENTS" "$MAX_PER_IP"
    echo
    printf '%sVerify it is running:%s\n' "$BOLD" "$RESET"
    printf '    systemctl %s status ppx-relay\n' "$scope"
    printf '    journalctl -u ppx-relay -n 50 --no-pager\n'
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
    printf '%sUpdate later:%s       rerun this script (idempotent).\n' "$BOLD" "$RESET"
    printf '%sUninstall:%s          bash install-relay.sh --uninstall\n' "$BOLD" "$RESET"
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

    TARGET="$(detect_target_triple)"
    download_release "$TARGET"
    install_binary

    if has_systemd; then
        write_unit
        enable_and_start
        verify_listening "$PORT" || true
        open_firewall_port
    else
        warn "systemd missing — wrote no unit. Start ${INSTALLED} manually."
    fi

    # Best-effort: figure out the public hostname so the operator knows what
    # to share. Never fatal — a LAN-only relay has nothing public.
    PUBLIC_HOSTNAME="$BIND"
    if [ "$BIND" = "0.0.0.0" ] || [ "$BIND" = "::" ]; then
        PUBLIC_HOSTNAME="${PUBLIC_HOSTNAME_OVERRIDE:-$(hostname -f 2>/dev/null || hostname || echo this-host)}"
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
