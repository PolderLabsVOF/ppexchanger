# lanchat

Fully-local, fully-encrypted LAN P2P terminal messenger written in Rust.

```
╭─ lanchat ─ alice ───────────────────────────╮
│ Peers (3)         │ alice: hi                │
│  ●★ bob   (con)   │ bob:  yo                 │
│  ○☆ carol (seen)  │ alice: how r u?          │
│  ×  dave  (gone)  │ bob:  same               │
├───────────────────┴──────────────────────────┤
│ > type and press Enter                       │
╰──────────────────────────────────────────────╯
```

No server, no account, no telemetry. Two binaries on the same WiFi, same
subnet, or wired into the same LAN find each other and exchange encrypted
text directly over TCP. UDP multicast is used for discovery; once two peers
handshake, all traffic is encrypted peer-to-peer.

## Install

Pre-built binaries ship for Linux (x86_64 + aarch64), macOS (x86_64 +
Apple silicon), and Windows (x86_64 MSVC). The installer fetches the
asset that matches your host, verifies it against a SHA256SUMS manifest
published alongside the release, and drops the binary into
`~/.local/bin` by default.

**Pick your platform:**

- [Linux](#linux)
- [macOS](#macos)
- [Windows](#windows)
- [From source](#from-source) — git + cargo, with or without the installer

### Linux

Requires `curl`, `tar`, and `sha256sum` (all pre-installed on every
mainstream distro):

```sh
curl -fsSL https://github.com/PolderLabsVOF/ppexchanger/releases/latest/download/install.sh | bash
```

The installer drops `lanchat` into `~/.local/bin`. If that directory is
not already on your `PATH`, the installer prints the export line to
add; most distros pick up `~/.local/bin` automatically.

Pin a specific version:

```sh
curl -fsSL https://github.com/PolderLabsVOF/ppexchanger/releases/latest/download/install.sh | bash -s -- --tag v0.3.1
```

Install system-wide (`/usr/local/bin`) — needs `sudo`:

```sh
curl -fsSL ... | bash -s -- --dir /usr/local/bin
```

Update later by re-running the same command. The installer detects the
existing binary, prints its previous version, and replaces it in place.

**Verify a download manually** — both files end up in `$TMPDIR`; the
manifest lists every asset:

```sh
curl -fsSL -O https://github.com/PolderLabsVOF/ppexchanger/releases/latest/download/SHA256SUMS
curl -fsSL -O https://github.com/PolderLabsVOF/ppexchanger/releases/latest/download/lanchat-<version>-x86_64-unknown-linux-gnu.tar.gz
sha256sum -c SHA256SUMS
```

**Architectures published:**

| Arch      | Triple                              |
| --------- | ----------------------------------- |
| x86_64    | `x86_64-unknown-linux-gnu`          |
| aarch64   | `aarch64-unknown-linux-gnu`         |

On Alpine / musl-based distros the gnu tarball runs in practice but
isn't an officially published asset — [build from source](#from-source)
if you hit a glibc symbol error.

Config + identity live under `$XDG_CONFIG_HOME/lanchat/` (typically
`~/.config/lanchat/`).

### macOS

The same installer detects Apple targets via `uname -s` and downloads
the matching tarball. Universal binary support: the `x86_64-apple-darwin`
asset runs natively on Apple silicon via Rosetta, and the
`aarch64-apple-darwin` asset runs natively on M-series chips. Modern
macOS users on Apple silicon get the native asset.

```sh
curl -fsSL https://github.com/PolderLabsVOF/ppexchanger/releases/latest/download/install.sh | bash
```

The installer drops `lanchat` into `~/.local/bin`. macOS does **not**
put `~/.local/bin` on `PATH` by default — once is enough:

```sh
echo 'export PATH="$HOME/.local/bin:$PATH"' >> ~/.zshrc   # or ~/.bash_profile
. ~/.zshrc
```

**Note on macOS Firewall**: the first launch prompts for incoming
network connections. Click **Allow** when prompted so UDP multicast
discovery (`/discover`) works. If you denied earlier, open
*System Settings → Network → Firewall → Options…* and remove the
deny rule for `lanchat`.

**Architectures published:**

| Arch              | Triple                      |
| ----------------- | --------------------------- |
| x86_64 (Intel)    | `x86_64-apple-darwin`       |
| aarch64 (Apple)   | `aarch64-apple-darwin`      |

The installer does not currently codesign or notarize the binary; on
first launch Gatekeeper may surface a "cannot be opened because the
developer cannot be verified" dialog. Either right-click → Open the
first time, or strip the quarantine attribute:

```sh
xattr -dr com.apple.quarantine ~/.local/bin/lanchat
```

Config + identity live under `~/Library/Application Support/lanchat/`
(equivalent to `$XDG_CONFIG_HOME/lanchat`).

### Windows

A native Windows binary (`lanchat.exe`, x86_64 MSVC) ships alongside
the Linux and macOS assets.

**Recommended — via the bash installer** (Git Bash / MSYS2 / Cygwin on
Windows):

```sh
curl -fsSL https://github.com/PolderLabsVOF/ppexchanger/releases/latest/download/install.sh | bash
```

The installer detects `MINGW*`, `MSYS*`, `CYGWIN*` from `uname -s` and
downloads the Windows tarball. `chmod +x` is skipped (PE binaries don't
carry the bit).

**Manual download** — if you don't have a bash shell handy, grab the
zip from the release page:

```
https://github.com/PolderLabsVOF/ppexchanger/releases/latest/download/lanchat-<version>-x86_64-pc-windows-msvc.zip
```

Extract it (Windows Explorer's "Extract All…" works) and put
`lanchat.exe` somewhere on your `%PATH%` — typically
`C:\Users\<you>\AppData\Local\Microsoft\WindowsApps` (no admin needed)
or `C:\Program Files\lanchat\` (admin needed).

**Architectures published:**

| Arch   | Triple                       |
| ------ | ---------------------------- |
| x86_64 | `x86_64-pc-windows-msvc`     |

aarch64 Windows is **not yet published**. If you're on ARM64,
[build from source](#from-source) with
`rustup target add aarch64-pc-windows-msvc`.

Config + identity + contacts live under `%APPDATA%\lanchat\`
(typically `C:\Users\<you>\AppData\Roaming\lanchat\`), created on
first run.

**Windows Firewall** will prompt on first launch when `lanchat` binds
the listening port (default `0.0.0.0:7777`). Allow access when asked,
or open the port manually. UDP multicast discovery may be silently
dropped by Windows Firewall; the `/discover` command falls back to a
TCP subnet scan.

### From source

Requires Rust 1.75+ (only audited dependencies, no native libraries).
Two routes:

**Through the installer** — interactive prompt or explicit flag. The
installer asks whether to download the binary or build from source
when stdin is a TTY; otherwise it defaults to the binary path. Force
source builds explicitly:

```sh
curl -fsSL https://github.com/PolderLabsVOF/ppexchanger/releases/latest/download/install.sh \
  | bash -s -- --method source
```

The source path needs `git` and `cargo install` (i.e. `rustup`). It
clones the repo at the resolved tag, runs `cargo install --path . --locked`
into the same `$INSTALL_DIR`, then runs the same smoke test the
binary path uses. Pin a tag the same way as the binary path:

```sh
curl -fsSL https://github.com/PolderLabsVOF/ppexchanger/releases/latest/download/install.sh \
  | bash -s -- --method source --tag v0.3.1
```

**Manual** — clone + build, no installer:

```sh
cargo install --git https://github.com/PolderLabsVOF/ppexchanger --locked
# or
git clone https://github.com/PolderLabsVOF/ppexchanger
cd ppexchanger
cargo build --release
./target/release/lanchat
```

## Usage

```sh
lanchat                      # start the TUI
lanchat --name alice         # override display name
lanchat --port 7777          # bind a specific TCP port
lanchat --theme neon         # solarized | monochrome | neon
lanchat --config /tmp/c.toml # alternate config path
lanchat --no-mouse           # disable mouse capture
lanchat --gen-identity       # print fingerprint + peer_id and exit
lanchat --version
lanchat --help
```

On first run, lanchat generates an identity keypair and stores it under
`~/.config/lanchat/` (or `$XDG_CONFIG_HOME/lanchat`):

- `identity` — 32-byte X25519 secret, 16-byte peer_id, display name
- `peerdb` — known contacts (name, public key, last seen, trust flag)
- `config.toml` — UI config (theme, scrollback, mouse, footer)

## Commands

Slash-commands are entered in the input line and start with `/`:

| Command                | What it does                                                  |
| ---------------------- | ------------------------------------------------------------- |
| `/peers`               | list every known peer with trust + state + fingerprint        |
| `/trust <name>`        | mark a peer as trusted; persists to peerdb                    |
| `/revoke <name>`       | remove a peer from peerdb                                     |
| `/theme <name>`        | switch theme (`default` / `solarized` / `monochrome` / `neon`); saved to config.toml |
| `/send <path>`         | send a file at `<path>` to the selected peer (binary; max 32 KiB per chunk) |
| `/quit`                | exit cleanly                                                  |

### Sending files

There are two ways to send a file to the selected peer:

1. **Paste a path** — type or paste a path like
   `/home/alice/report.pdf` and press Enter. If the path points at an
   existing regular file, lanchat auto-detects it and starts a binary
   transfer; otherwise the text is sent as a chat message.
2. **`/send <path>`** — explicit escape hatch. Bypasses auto-detect; use
   this when the file has no extension or you want unambiguous behaviour.

The receiver sees a file-offer popup with the sender's name, file name,
and human-readable size. `Enter` accepts, `Esc` rejects. Received files
land under `<config_dir>/received/<id>-<sanitized-name>` and the
sender's bytes are written verbatim (sha256 matches across both ends).

Send to a specific peer by name even when your focus is on the chat pane:

```
@bob   hey, this routes to the peer named "bob"
```

A bare message goes to the currently-selected connected peer. If no peer is
selected, the first connected peer receives it.

## Key bindings

| Key           | Action                                                 |
| ------------- | ------------------------------------------------------ |
| `Tab`         | cycle focus between sidebar and chat                   |
| `↑` / `↓`     | in sidebar: move selection. In empty input: history recall |
| `PageUp/Down` | in chat: scroll scrollback. In sidebar: page through peers |
| `Enter`       | send the message                                       |
| `Ctrl-N`      | hint to start a new chat                               |
| `Ctrl-T`      | trust the selected peer                                |
| `Ctrl-R`      | revoke the selected peer                               |
| `Ctrl-L`      | clear input                                            |
| `Esc`         | cancel / clear input / close modal                     |
| `Ctrl-C` / `Ctrl-Q` | quit                                              |
| `?`           | toggle the help overlay                                |

### Mouse

Mouse capture is **off by default** — start lanchat with no flags for
keyboard-only mode. To enable click-to-select and scroll-wheel chat
scrolling, drop `mouse = true` into the `[ui]` section of your
`config.toml` (or run with the default — the field flips per session).
With capture on:

* Left-click a row in the sidebar to select that peer and focus the sidebar.
* Left-click the chat pane to focus the chat.
* Scroll wheel in the chat pane scrolls the message history.

Capture breaks tmux / native drag-select inside the TUI; run with
`--no-mouse` to recover native selection.

### Pasting

Bracketed paste is **always on**. Paste any text — including a path
that resolves to an existing file — directly into the input line and
press Enter. Pasted payloads are capped at 1 MiB; anything bigger is
dropped silently so a stray log-file paste can't OOM the UI thread.

## Discovery

Discovery is **manual**. Press `/discover` (or use the command in any
context) to fan out two scans:

1. **UDP multicast** — sends one beacon to `239.255.42.99:7777` and
   listens for ~3 seconds. Works on most flat LANs.
2. **TCP subnet scan** — walks the local IPv4 /24 around the host's
   outbound IP, probing each host for an open TCP listener on the
   announced port. Fallback for networks where multicast is blocked
   (common on consumer WiFi APs).

Results appear in a modal popup with one section per method. Press `Esc`
to dismiss. Identified peers are added to the sidebar as `Seen`; once
you (or they) send a message, the connection upgrades to `Connected`.

## Configuration

`~/.config/lanchat/config.toml`:

```toml
[ui]
theme = "default"        # default | solarized | monochrome | neon
show_footer = true
mouse = true
scrollback = 500          # max chat history lines; clamped to 16..50_000
```

Lines starting with `#` are comments. Unknown keys are ignored. Missing
keys fall back to defaults. The file is overwritten when you run
`/theme <name>` from the TUI — keep the change you don't want to lose
above the `[ui]` header in a non-overwritten file.

## Security

- **Key exchange** — Noise_XX (canonical 3-message mutual authentication
  pattern), per-session keys derived via HKDF-SHA256.
- **Transport** — ChaCha20-Poly1305 AEAD with per-direction sequence
  counters; no plaintext on the wire after the handshake completes.
- **Static keys** — X25519, generated from the kernel CSPRNG; the secret
  half is stored with 0600 permissions in `~/.config/lanchat/identity`
  on Linux/macOS (Windows uses NTFS ACL inheritance instead).
- **Trust model** — every peer is `untrusted` by default. Use `/trust
  <name>` to mark a peer as verified (typically after checking their
  fingerprint out-of-band). The trusted/untrusted flag persists in
  peerdb.
- **No server, no telemetry, no update channel** — the binary doesn't
  phone home. Run `lanchat --gen-identity` to dump your fingerprint for
  out-of-band verification with a peer before you `/trust` them.

## Layout

```
src/
├── crypto/        Keypair + HKDF + AEAD helpers (over audited crates)
├── protocol/      Wire formats: Beacon, Frame, length-prefix codec
├── net/           Discovery + listener + handshake + session + scan
├── events.rs      mpsc bus between UI and network threads
├── identity.rs    On-disk identity (32-byte X25519 secret + name)
├── peerdb.rs      On-disk contact list (name, pubkey, trust, last seen)
├── config.rs      XDG-aware paths
├── tui/           ratatui frontend
│   ├── mod.rs     UiState, render, focus, scroll, modals
│   ├── input.rs   Line editor + EditorEvent dispatch
│   ├── theme.rs   Theme palettes + Unicode/ASCII glyph detection
│   ├── config.rs  Hand-rolled TOML-subset parser
│   ├── help.rs    `?` overlay
│   └── discovery_popup.rs  `/discover` results modal
└── main.rs        CLI parsing, threading, action handling
```

## License

MIT.
