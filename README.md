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

The one-line installer fetches the latest release from GitHub, verifies it
against a SHA256SUMS manifest, and installs `lanchat` into `~/.local/bin`:

```sh
curl -fsSL https://github.com/PolderLabsVOF/ppexchanger/releases/latest/download/install.sh | bash
```

Pin a specific version:

```sh
curl -fsSL https://github.com/PolderLabsVOF/ppexchanger/releases/latest/download/install.sh | bash -s -- --tag v0.3.1
```

Install system-wide (needs sudo):

```sh
curl -fsSL ... | bash -s -- --dir /usr/local/bin
```

Update later by re-running the same command — the installer detects the
existing binary, prints its version, and swaps it in place.

Verify a download manually:

```sh
sha256sum -c SHA256SUMS
```

### Windows

A native Windows binary (`lanchat.exe`, MSVC) is shipped alongside the
Linux and macOS assets. Two install paths:

**Via the bash installer** (Git Bash / MSYS2 / Cygwin on Windows):
the same one-line command as above. The installer detects `MINGW*`,
`MSYS*`, `CYGWIN*` from `uname -s` and downloads the Windows tarball;
`chmod +x` is skipped (PE binaries don't carry the bit).

**Manual** — if you don't have a bash shell handy, grab the zip from
the release page:

```
https://github.com/PolderLabsVOF/ppexchanger/releases/latest/download/lanchat-<version>-x86_64-pc-windows-msvc.zip
```

Extract it (Windows Explorer's "Extract All…" works) and put
`lanchat.exe` somewhere on your `%PATH%`.

Config / identity / contacts on Windows live under
`%APPDATA%\lanchat\` (typically
`C:\Users\<you>\AppData\Roaming\lanchat\`), created on first run.

**Windows Firewall** will prompt on first launch when `lanchat` binds
the listening port (default `0.0.0.0:7777`). Allow access when asked,
or open the port manually. UDP multicast discovery may be silently
dropped by Windows Firewall; the `/discover` command falls back to a
TCP subnet scan.

### From source

Requires Rust 1.75+ (only audited dependencies, no native libraries):

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
| `/quit`                | exit cleanly                                                  |

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
