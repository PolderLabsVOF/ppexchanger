#!/usr/bin/env bash
# install.sh — install or update `lanchat`.
#
# Usage:
#   curl -fsSL https://github.com/PolderLabsVOF/ppexchanger/releases/latest/download/install.sh | bash
#   curl -fsSL ... | bash -s -- --tag v0.3.1
#   bash install.sh --uninstall
#   bash install.sh --method source           # build from source instead of fetching the binary
#
# Environment overrides:
#   LANCHAT_INSTALL_DIR   target directory (default: $HOME/.local/bin)
#   LANCHAT_VERSION       specific version tag (default: latest release)
#   LANCHAT_REPO          "owner/name"                  (default: PolderLabsVOF/ppexchanger)
#   LANCHAT_SKIP_VERIFY   set to 1 to skip checksum verification
#   LANCHAT_METHOD        "binary" | "source" | "auto" (default: auto = prompt when TTY, binary when piped)
#
# By default the script fetches a single-binary tarball (`lanchat-<tag>-<target>.tar.gz`),
# verifies it against `SHA256SUMS`, extracts the `lanchat` binary into the
# install dir, and on update replaces the previous binary in place. With
# `--method source` (or by answering "source" at the interactive prompt when
# stdin is a TTY) the script instead clones the repo at the chosen tag and
# runs `cargo install --path . --locked` into the same install dir.
# Re-running the script is the supported update path.

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
    --method <mode>     Install method: "binary" (download release tarball, the default),
                        "source" (git clone + cargo install --path . --locked), or
                        "auto" (prompt when stdin is a TTY, else "binary").
                        Equivalent env var: LANCHAT_METHOD.
    --yes               Skip the "replacing existing binary" prompt-style warning and
                        auto-pick the binary method when --method auto is in effect.
                        (The install is non-interactive anyway; useful for log scraping.)
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
    LANCHAT_METHOD          "binary" | "source" | "auto" (default: auto)
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
METHOD="${LANCHAT_METHOD:-auto}"

while [ $# -gt 0 ]; do
    case "$1" in
        --tag)          [ $# -ge 2 ] || die "--tag requires an argument"; VERSION="$2"; shift 2 ;;
        --dir)          [ $# -ge 2 ] || die "--dir requires an argument"; INSTALL_DIR="$2"; shift 2 ;;
        --method)       [ $# -ge 2 ] || die "--method requires an argument"; METHOD="$2"; shift 2 ;;
        --yes)          ASSUME_YES=1; shift ;;
        --uninstall)    uninstall ;;
        --print-target) PRINT_TARGET=1; shift ;;
        --print-tag)    PRINT_TAG=1; shift ;;
        --help|-h)      usage; exit 0 ;;
        *)              die "unknown argument: $1 (try --help)" ;;
    esac
done

# Validate the method early so a typo fails before we touch the network.
case "$METHOD" in
    binary|source|auto) ;;
    *) die "invalid --method value: '$METHOD' (expected: binary, source, or auto)" ;;
esac

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
# Method resolution
# ---------------------------------------------------------------------------
# In auto mode we ask the user when stdin is a TTY (so an interactive
# `./install.sh` run gets the choice), and default to binary otherwise so
# `curl ... | bash` keeps the fast, hermetic path it always had. The
# chosen method then short-circuits the rest of the script.
choose_method() {
    # Probe flags (--print-target / --print-tag) exit before this block
    # in CI; if either fires after we've been entered, leave METHOD alone.
    if [ "$METHOD" != "auto" ] || [ "$PRINT_TAG" = "1" ]; then
        return
    fi
    if [ ! -t 0 ] || [ "$ASSUME_YES" = "1" ]; then
        METHOD="binary"
        return
    fi
    printf '%s\n' "install method:" >&2
    printf '%s\n' "  1) binary  — download the release tarball (~5 MB, fast)" >&2
    printf '%s\n' "  2) source  — git clone + cargo install (needs git + rustc; minutes)" >&2
    local reply
    # Read with a 30s timeout so a hung terminal doesn't trap the install
    # forever. `read -t` returns >128 on timeout; we treat that as "binary"
    # so unattended terminals still get a working install.
    if ! reply="$(read -r -t 30 -p "choose [1/2, default 1]: " choice; printf '%s' "${choice:-1}")" 2>/dev/null; then
        warn "no prompt reply within 30s — defaulting to binary"
        METHOD="binary"
        return
    fi
    case "$reply" in
        2|source) METHOD="source" ;;
        *)        METHOD="binary" ;;
    esac
}
choose_method

# Skip the log when we're in a probe path — CI captures the tag and
# we don't want extra noise in `$()`.
if [ "$PRINT_TAG" != "1" ]; then
    log "install method: $METHOD"
fi

# ---------------------------------------------------------------------------
# Dependency checks
# ---------------------------------------------------------------------------
command -v curl >/dev/null 2>&1 || die "curl is required"
command -v tar  >/dev/null 2>&1 || die "tar is required"

# Checksum tooling is only needed for the binary path; source builds are
# validated by cargo's lockfile.
if [ "$METHOD" = "binary" ]; then
    command -v sha256sum >/dev/null 2>&1 || command -v shasum >/dev/null 2>&1 \
        || die "sha256sum (or shasum) is required for binary installs"
fi

# Source installs require git (for the clone) and cargo (for the build).
if [ "$METHOD" = "source" ]; then
    command -v git   >/dev/null 2>&1 || die "git is required for --method source"
    command -v cargo >/dev/null 2>&1 || die "cargo is required for --method source (install rustup: https://rustup.rs)"
fi

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
# Must run before the method-resolution block so CI captures don't pick up
# the "install method: …" log line.
if [ "$PRINT_TAG" = "1" ]; then
    printf '%s\n' "$TAG"
    exit 0
fi

# Both paths need a scratch dir for downloads / clones / staged
# cargo-install output. Declared once, cleaned via EXIT trap.
TMPDIR="$(mktemp -d)"
trap 'rm -rf "$TMPDIR"' EXIT

# ---------------------------------------------------------------------------
# Source build path
# ---------------------------------------------------------------------------
# Clones the repo at the resolved tag into a scratch dir and runs
# `cargo install --path . --locked` so the output lands in
# `$INSTALL_DIR/lanchat(.exe)` — same target as the binary path. Uses
# `--locked` so a Cargo.lock mismatch fails loudly instead of silently
# picking up newer dep versions.
install_from_source() {
    local repo_url="https://github.com/$REPO"
    local src_dir="$TMPDIR/src"
    log "cloning $repo_url at $TAG into $src_dir..."
    git clone --depth 1 --branch "$TAG" "$repo_url" "$src_dir" \
        || die "git clone failed — check that tag $TAG exists in $REPO"

    # The source's `Cargo.toml` package name drives the installed binary
    # name; fall back to "lanchat" if we can't introspect.
    local pkg_name
    pkg_name="$(grep -E '^name *= *"' "$src_dir/Cargo.toml" | head -1 | sed 's/.*"\([^"]*\)".*/\1/')"
    [ -n "$pkg_name" ] || pkg_name="lanchat"

    log "running \`cargo install --path . --locked --root $TMPDIR/stage\`..."
    (
        cd "$src_dir"
        cargo install --path . --locked --root "$TMPDIR/stage" --quiet \
            || die "cargo install failed — see compiler output above"
    )

    local built="$TMPDIR/stage/bin/$BIN_BASENAME"
    [ -f "$built" ] || built="$TMPDIR/stage/bin/$pkg_name"
    [ -f "$built" ] || built="$TMPDIR/stage/bin/$pkg_name.exe"
    [ -f "$built" ] || die "cargo install did not produce a binary at $TMPDIR/stage/bin/"

    install_binary_file "$built"
}

# Shared by both paths: stage-source → BIN_PATH move + permission fix +
# upgrade detection + smoke test. Defined before either caller so the
# branch at the bottom of the script can call into it cleanly.
install_binary_file() {
    local src="$1"
    if [ -e "$BIN_PATH" ] || [ -L "$BIN_PATH" ]; then
        PREV_VERSION=""
        if [ -x "$BIN_PATH" ]; then
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

    if mv -f "$src" "$BIN_PATH" 2>/dev/null; then
        :
    else
        cp -f "$src" "$BIN_PATH"
    fi
    case "$TARGET_TRIPLE" in
        *-pc-windows-*) ;;
        *) chmod +x "$BIN_PATH" ;;
    esac

    ok "installed lanchat $TAG_BARE → $BIN_PATH"

    case ":$PATH:" in
        *":$INSTALL_DIR:"*) ;;
        *)
            warn "$INSTALL_DIR is not on your PATH."
            warn "add this to your shell rc:  export PATH=\"$INSTALL_DIR:\$PATH\""
            ;;
    esac

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
}

# ---------------------------------------------------------------------------
# Dispatch
# ---------------------------------------------------------------------------
if [ "$METHOD" = "source" ]; then
    install_from_source
    exit 0
fi

# Binary path: download the tarball, verify, extract, then hand off to
# the shared installer.
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

# Detect upgrade vs fresh install + move into place + smoke test.
# Hand off to the shared installer so binary and source paths converge.
install_binary_file "$SRC_BIN"
