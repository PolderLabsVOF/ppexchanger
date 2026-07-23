#!/usr/bin/env bash
# install.sh — install or update the `lanchat` release binary.
#
# Usage:
#   curl -fsSL https://github.com/PolderLabsVOF/ppexchanger/releases/latest/download/install.sh | bash
#   curl -fsSL ... | bash -s -- --tag v0.3.1
#   bash install.sh --uninstall
#
# Environment overrides:
#   LANCHAT_INSTALL_DIR   target directory (default: $HOME/.local/bin)
#   LANCHAT_VERSION       specific version tag (default: latest release)
#   LANCHAT_REPO          "owner/name"                  (default: PolderLabsVOF/ppexchanger)
#   LANCHAT_SKIP_VERIFY   set to 1 to skip checksum verification
#
# The script fetches a single-binary tarball (`lanchat-<tag>-<target>.tar.gz`),
# verifies it against `SHA256SUMS`, extracts the `lanchat` binary into the
# install dir, and on update replaces the previous binary in place. Re-running
# the script is the supported update path — it always fetches and verifies
# the latest release (or the pinned tag).

set -euo pipefail

REPO="${LANCHAT_REPO:-PolderLabsVOF/ppexchanger}"
INSTALL_DIR="${LANCHAT_INSTALL_DIR:-$HOME/.local/bin}"
VERSION="${LANCHAT_VERSION:-latest}"

# ANSI colour helpers — used only when stdout is a terminal.
if [ -t 1 ]; then
    BOLD=$'\033[1m'; GREEN=$'\033[32m'; YELLOW=$'\033[33m'; RED=$'\033[31m'; RESET=$'\033[0m'
else
    BOLD=""; GREEN=""; YELLOW=""; RED=""; RESET=""
fi

log()  { printf '%b[lanchat]%b %s\n' "$BOLD" "$RESET" "$*"; }
ok()   { printf '%b[lanchat]%b %b%s%b\n' "$BOLD" "$RESET" "$GREEN" "$*" "$RESET"; }
warn() { printf '%b[lanchat]%b %b%s%b\n' "$BOLD" "$RESET" "$YELLOW" "$*" "$RESET"; }
die()  { printf '%b[lanchat]%b %b%s%b\n' "$BOLD" "$RESET" "$RED" "$*" "$RESET" >&2; exit 1; }


usage() {
    cat <<EOF
install.sh — install or update lanchat

USAGE:
    bash install.sh [options]

OPTIONS:
    --tag <tag>         Install a specific release tag (e.g. v0.3.1). Default: latest.
    --dir <path>        Install directory. Default: \$HOME/.local/bin.
    --yes               Skip the "replacing existing binary" prompt-style warning
                        (the install is non-interactive anyway; useful for log scraping).
    --uninstall         Remove the installed binary.
    --print-target      Print the detected target triple for this host and exit.
    --print-tag         Resolve the latest (or pinned) tag and print it, then exit.
                        Useful in CI:  TAG=\$(curl -fsSL .../install.sh | bash -s -- --print-tag)
    --help              Print this help.

ENV:
    LANCHAT_REPO            GitHub repo (owner/name)  default: PolderLabsVOF/ppexchanger
    LANCHAT_INSTALL_DIR     Same as --dir
    LANCHAT_VERSION         Same as --tag
    LANCHAT_SKIP_VERIFY     Set to 1 to skip SHA256SUMS verification (not recommended)
    LANCHAT_YES             Set to 1 to behave as if --yes was passed

EXAMPLES:
    curl -fsSL https://github.com/${REPO}/releases/latest/download/install.sh | bash
    curl -fsSL https://github.com/${REPO}/releases/latest/download/install.sh | bash -s -- --tag v0.3.1
    LANCHAT_INSTALL_DIR=/usr/local/bin bash install.sh
EOF
}

uninstall() {
    local bin="$INSTALL_DIR/lanchat"
    if [ -e "$bin" ] || [ -L "$bin" ]; then
        rm -f "$bin"
        ok "removed $bin"
    else
        warn "no binary at $bin — nothing to do"
    fi
    exit 0
}

# ---------------------------------------------------------------------------
# Argument parsing
# ---------------------------------------------------------------------------
ASSUME_YES="${LANCHAT_YES:-0}"
PRINT_TARGET=0
PRINT_TAG=0

while [ $# -gt 0 ]; do
    case "$1" in
        --tag)          [ $# -ge 2 ] || die "--tag requires an argument"; VERSION="$2"; shift 2 ;;
        --dir)          [ $# -ge 2 ] || die "--dir requires an argument"; INSTALL_DIR="$2"; shift 2 ;;
        --yes)          ASSUME_YES=1; shift ;;
        --uninstall)    uninstall ;;
        --print-target) PRINT_TARGET=1; shift ;;
        --print-tag)    PRINT_TAG=1; shift ;;
        --help|-h)      usage; exit 0 ;;
        *)              die "unknown argument: $1 (try --help)" ;;
    esac
done

# Normalise the install dir. The binary basename is decided below, once
# `TARGET_TRIPLE` has been resolved.
INSTALL_DIR="${INSTALL_DIR/#\~/$HOME}"
mkdir -p "$INSTALL_DIR" || die "cannot create install dir: $INSTALL_DIR"

# ---------------------------------------------------------------------------
# Host target detection
# ---------------------------------------------------------------------------
# Returns the Rust target triple that matches the current host, or exits
# non-zero with a message on stderr. Centralised so `--print-target` and
# the install path can share it.
#
# Windows hosts are detected via `uname -s` returning one of
# `MINGW*_NT-*`, `MSYS_NT-*`, or `CYGWIN_NT-*` (all uppercase on real
# Git Bash / MSYS2 / Cygwin; lowered here for matching).
detect_target_triple() {
    local host_os host_arch triple
    host_os="$(uname -s 2>/dev/null | tr '[:upper:]' '[:lower:]')"
    host_arch="$(uname -m)"
    case "$host_os" in
        linux|darwin|mingw*|msys*|cygwin*) ;;
        *) die "unsupported OS: $host_os (only linux + macOS + windows are published)" ;;
    esac
    case "$host_arch" in
        x86_64|amd64)   triple="x86_64"   ;;
        aarch64|arm64)  triple="aarch64"  ;;
        *)              die "unsupported architecture: $host_arch (only x86_64 + aarch64 are published)" ;;
    esac
    # Pick the vendor + os suffix for the host OS. Linux is the default
    # carrier triple from `$rustc -vV` style; macOS + Windows get their
    # own triples. A future aarch64-pc-windows-msvc asset would only
    # need a new case branch here.
    case "$host_os" in
        linux)                          triple="${triple}-unknown-linux-gnu" ;;
        darwin)                         triple="${triple}-apple-darwin"      ;;
        mingw*|msys*|cygwin*)
            case "$host_arch" in
                x86_64|amd64)   triple="${triple}-pc-windows-msvc" ;;
                aarch64|arm64)
                    die "aarch64 Windows is not yet published (only x86_64-pc-windows-msvc is built)" ;;
                *)              die "unsupported architecture: $host_arch (only x86_64 is published on Windows)" ;;
            esac
            ;;
    esac
    printf '%s\n' "$triple"
}

TARGET_TRIPLE="$(detect_target_triple)"

# Windows PE binaries carry the `.exe` suffix; ELF/Mach-O use bare
# `lanchat`. Computed once so the rest of the script can reference it.
case "$TARGET_TRIPLE" in
    *-pc-windows-*) BIN_BASENAME="lanchat.exe" ;;
    *)             BIN_BASENAME="lanchat"    ;;
esac
BIN_PATH="$INSTALL_DIR/$BIN_BASENAME"

# `--print-target` is a debug-friendly probe: emit the resolved triple and
# exit before any network or filesystem side effects.
if [ "$PRINT_TARGET" = "1" ]; then
    printf '%s\n' "$TARGET_TRIPLE"
    exit 0
fi

# ---------------------------------------------------------------------------
# Dependency checks
# ---------------------------------------------------------------------------
command -v curl >/dev/null 2>&1 || die "curl is required"
command -v tar  >/dev/null 2>&1 || die "tar is required"
command -v sha256sum >/dev/null 2>&1 || command -v shasum >/dev/null 2>&1 \
    || die "sha256sum (or shasum) is required"

# ---------------------------------------------------------------------------
# Resolve the version + the asset name
# ---------------------------------------------------------------------------
if [ "$VERSION" = "latest" ]; then
    # For `--print-tag` we suppress the log line so the output is a clean
    # single token suitable for `TAG=$(...)` capture. For the install
    # path itself we keep the user-visible status line.
    if [ "$PRINT_TAG" != "1" ]; then
        log "fetching latest release metadata from $REPO..."
    fi
    LATEST_URL="https://api.github.com/repos/$REPO/releases/latest"
    RELEASE_JSON="$(curl -fsSL -H 'Accept: application/vnd.github+json' "$LATEST_URL")" \
        || die "could not fetch release metadata — check your network or REPO setting"
    TAG="$(printf '%s' "$RELEASE_JSON" | grep -o '"tag_name": *"[^"]*"' | head -1 | sed 's/.*"tag_name": *"\([^"]*\)".*/\1/')"
    [ -n "$TAG" ] || die "could not parse tag from release metadata"
else
    TAG="$VERSION"
fi

# Strip a leading "v" if present — assets are named `lanchat-<tag>.tar.gz`
# where `<tag>` is the bare version string ("0.3.1", not "v0.3.1").
TAG_BARE="${TAG#v}"

# `--print-tag` resolves the version and exits without touching the
# filesystem. Used by CI to pin a release: TAG=$(curl -fsSL .../install.sh | bash -s -- --print-tag)
if [ "$PRINT_TAG" = "1" ]; then
    printf '%s\n' "$TAG"
    exit 0
fi

# ---------------------------------------------------------------------------
# Download
# ---------------------------------------------------------------------------
TMPDIR="$(mktemp -d)"
trap 'rm -rf "$TMPDIR"' EXIT

ASSET="lanchat-${TAG_BARE}-${TARGET_TRIPLE}.tar.gz"
BASE_URL="https://github.com/$REPO/releases/download/$TAG"
TARBALL="$TMPDIR/$ASSET"
SUMS="$TMPDIR/SHA256SUMS"

log "downloading $ASSET from tag $TAG..."
curl -fSL --retry 3 -o "$TARBALL" "$BASE_URL/$ASSET" \
    || die "download failed — check that release $TAG exists with asset $ASSET"

log "downloading SHA256SUMS..."
curl -fsSL -o "$SUMS" "$BASE_URL/SHA256SUMS" \
    || die "SHA256SUMS not found at $BASE_URL/SHA256SUMS"

# ---------------------------------------------------------------------------
# Verify
# ---------------------------------------------------------------------------
if [ "${LANCHAT_SKIP_VERIFY:-0}" = "1" ]; then
    warn "LANCHAT_SKIP_VERIFY=1 — skipping checksum verification (NOT recommended)"
else
    log "verifying checksum..."
    (
        cd "$TMPDIR"
        if command -v sha256sum >/dev/null 2>&1; then
            sha256sum --check --strict --ignore-missing < SHA256SUMS || exit 1
        else
            # macOS ships `shasum -a 256` instead. Re-derive the expected hash.
            EXPECTED="$(grep -E "  $ASSET\$" SHA256SUMS | awk '{print $1}')"
            [ -n "$EXPECTED" ] || { echo "expected hash missing for $ASSET" >&2; exit 1; }
            ACTUAL="$(shasum -a 256 "$ASSET" | awk '{print $1}')"
            [ "$EXPECTED" = "$ACTUAL" ] || { echo "checksum mismatch: $ACTUAL != $EXPECTED" >&2; exit 1; }
            echo "$ASSET: OK"
        fi
    ) || die "checksum verification failed — refusing to install"
    ok "checksum verified"
fi

# ---------------------------------------------------------------------------
# Extract + install
# ---------------------------------------------------------------------------
log "extracting..."
tar -xzf "$TARBALL" -C "$TMPDIR"
# Tarball layout is `bin/<binary-name>`; the leaf name depends on the
# target triple (lanchat.exe on Windows, lanchat elsewhere).
SRC_BIN="$TMPDIR/$BIN_BASENAME"
[ -f "$SRC_BIN" ] || SRC_BIN="$TMPDIR/bin/$BIN_BASENAME"
[ -f "$SRC_BIN" ] || die "expected binary '$BIN_BASENAME' not found in the tarball"
# PE binaries carry their executable bit via NTFS ACLs, not the +x bit.
case "$TARGET_TRIPLE" in
    *-pc-windows-*) ;;
    *) chmod +x "$SRC_BIN" || true ;;
esac

# Detect upgrade vs fresh install.
if [ -e "$BIN_PATH" ] || [ -L "$BIN_PATH" ]; then
    PREV_VERSION=""
    if [ -x "$BIN_PATH" ]; then
        # `lanchat --version` emits a single line "lanchat 0.3.0". The last
        # whitespace-delimited token is the bare version. Fall back to
        # $LANCHAT_PREV_VERSION when the binary can't be executed (e.g.
        # the wrong arch was previously installed).
        PREV_VERSION="$("$BIN_PATH" --version 2>/dev/null | head -1 | awk '{print $NF}')" \
            || PREV_VERSION="${LANCHAT_PREV_VERSION:-}" \
            || PREV_VERSION=""
    fi
    if [ "$ASSUME_YES" = "1" ]; then
        log "replacing existing $BIN_PATH (was: ${PREV_VERSION:-unknown})"
    else
        warn "replacing existing $BIN_PATH (was: ${PREV_VERSION:-unknown})"
    fi
    UPDATE=1
else
    UPDATE=0
fi

# Atomic-ish install: move into place, fall back to copy if cross-device.
if mv -f "$SRC_BIN" "$BIN_PATH" 2>/dev/null; then
    :
else
    cp -f "$SRC_BIN" "$BIN_PATH"
fi
# PE binaries already carry executable permission via NTFS ACLs.
case "$TARGET_TRIPLE" in
    *-pc-windows-*) ;;
    *) chmod +x "$BIN_PATH" ;;
esac

# ---------------------------------------------------------------------------
# Post-install: PATH hint + smoke test
# ---------------------------------------------------------------------------
ok "installed lanchat $TAG_BARE → $BIN_PATH"

# Check that the install dir is on PATH — if not, nudge the user.
case ":$PATH:" in
    *":$INSTALL_DIR:"*) ;;
    *)
        warn "$INSTALL_DIR is not on your PATH."
        warn "add this to your shell rc:  export PATH=\"$INSTALL_DIR:\$PATH\""
        ;;
esac

# Smoke test — the binary should at least print its version.
if "$BIN_PATH" --version >/dev/null 2>&1; then
    INSTALLED_VER="$("$BIN_PATH" --version | head -1)"
    ok "smoke test: $INSTALLED_VER"
else
    warn "installed binary did not respond to --version — check $BIN_PATH"
fi

if [ "$UPDATE" = "1" ]; then
    ok "update complete (was ${PREV_VERSION:-unknown} → $TAG_BARE)"
else
    ok "install complete — run \$ $BIN_BASENAME to start"
fi
