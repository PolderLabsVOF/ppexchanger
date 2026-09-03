# ppexchanger

Fully-local, fully-encrypted LAN P2P terminal messenger written in Rust.

```
╭─ ppx ─ alice ───────────────────────────╮
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

The installer drops `ppx` into `~/.local/bin`. If that directory is
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
curl -fsSL -O https://github.com/PolderLabsVOF/ppexchanger/releases/latest/download/ppexchanger-<version>-x86_64-unknown-linux-gnu.tar.gz
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

Config + identity live under `$XDG_CONFIG_HOME/ppexchanger/` (typically
`~/.config/ppexchanger/`).

### macOS

The same installer detects Apple targets via `uname -s` and downloads
the matching tarball. Universal binary support: the `x86_64-apple-darwin`
asset runs natively on Apple silicon via Rosetta, and the
`aarch64-apple-darwin` asset runs natively on M-series chips. Modern
macOS users on Apple silicon get the native asset.

```sh
curl -fsSL https://github.com/PolderLabsVOF/ppexchanger/releases/latest/download/install.sh | bash
```

The installer drops `ppx` into `~/.local/bin`. macOS does **not**
put `~/.local/bin` on `PATH` by default — once is enough:

```sh
echo 'export PATH="$HOME/.local/bin:$PATH"' >> ~/.zshrc   # or ~/.bash_profile
. ~/.zshrc
```

**Note on macOS Firewall**: the first launch prompts for incoming
network connections. Click **Allow** when prompted so UDP multicast
discovery (`/discover`) works. If you denied earlier, open
*System Settings → Network → Firewall → Options…* and remove the
deny rule for `ppx`.

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
xattr -dr com.apple.quarantine ~/.local/bin/ppx
```

Config + identity live under `~/Library/Application Support/ppexchanger/`
(equivalent to `$XDG_CONFIG_HOME/ppx`).

### Windows

A native Windows binary (`ppx.exe`, x86_64 MSVC) ships alongside
the Linux and macOS assets. The installer and the binary both work
under **Git Bash**, **MSYS2**, or **Cygwin** — pick whichever you
already have. PowerShell or WSL alone are not enough on their own;
the installer is a bash script.

#### Prerequisites

- **Git for Windows** (ships Git Bash + curl + tar + sha256sum):
  <https://git-scm.com/download/win>. Pick "Git from the command line
  and also from 3rd-party software" so `curl.exe` lands on your PATH.
- **Visual Studio Build Tools** with the **"Desktop development with
  C++"** workload — required for the source-build path only (the
  installer uses `link.exe`). Skip if you only install the prebuilt
  binary.
  <https://visualstudio.microsoft.com/visual-cpp-build-tools/>

#### Install via the bash installer

Open **Git Bash** (not PowerShell or cmd) and run:

```sh
curl -fsSL https://github.com/PolderLabsVOF/ppexchanger/releases/latest/download/install.sh | bash
```

What happens:

1. The script detects `MINGW*`, `MSYS*`, or `CYGWIN*` from `uname -s`
   and downloads the Windows tarball.
2. The binary drops into `$HOME/.local/bin/ppx.exe`. On Git Bash,
   `$HOME` resolves to `C:\Users\<you>\` so the install dir is
   `C:\Users\<you>\.local\bin\`. If that directory isn't on your PATH
   yet, the installer prints the `export` line to add — append it to
   `~/.bashrc` to persist across shells.
3. `chmod +x` is skipped (Windows PE binaries don't carry the bit —
   the file association lives in the NTFS ACL).
4. Smoke test: `ppx --version` runs to confirm the binary responds.

To pin a specific tag:

```sh
curl -fsSL https://github.com/PolderLabsVOF/ppexchanger/releases/latest/download/install.sh \
  | bash -s -- --tag v0.5.0
```

To install into a different directory (e.g. one already on PATH):

```sh
PPX_INSTALL_DIR="/c/Users/Public/bin" \
  bash <(curl -fsSL https://github.com/PolderLabsVOF/ppexchanger/releases/latest/download/install.sh)
```

> Git Bash mounts `C:\` as `/c/` — use the unix-style path in
> `PPX_INSTALL_DIR`. The installer writes the literal value into the
> filesystem, so `C:\Users\Public\bin` and `/c/Users/Public/bin` both
> resolve to the same directory.

#### Manual install (no bash session)

If you only have PowerShell or a plain cmd window — no bash — grab
the zip from the release page:

```
https://github.com/PolderLabsVOF/ppexchanger/releases/latest/download/ppexchanger-<version>-x86_64-pc-windows-msvc.zip
```

Extract it (Windows Explorer's "Extract All…" works) and move
`ppx.exe` somewhere on your `%PATH%`. Sensible choices:

- `C:\Users\<you>\AppData\Local\Microsoft\WindowsApps` (no admin
  needed; already on PATH on Windows 10/11).
- `C:\Program Files\ppexchanger\` (admin needed; tidier if you want
  it grouped with other tools).

Open **PowerShell** and verify:

```powershell
ppx --version
# ppexchanger 0.5.0
```

#### Build from source on Windows

The installer can build from source instead of downloading the
prebuilt binary. Two extra prerequisites beyond the binary install:

1. **Visual Studio Build Tools** with the "Desktop development with
   C++" workload installed (the installer shells out to `link.exe`).
2. **Rust** via <https://rustup.rs> — the installer runs
   `cargo install --path . --locked`, which needs both `cargo` and
   `rustc` on PATH.

Open **Git Bash** (so `uname -s` reports a value the installer
recognises as Windows), then:

```sh
curl -fsSL https://github.com/PolderLabsVOF/ppexchanger/releases/latest/download/install.sh \
  | bash -s -- --method source
```

The first build takes a few minutes — `cargo install` resolves and
compiles the dep tree before linking `ppx.exe`. Subsequent runs hit
the cargo cache and complete in seconds.

If you only have the **MSVC developer command prompt** open (not Git
Bash), launch Git Bash from it with:

```cmd
"C:\Program Files\Git\bin\bash.exe"
```

so the installer sees a bash environment with MSVC's `link.exe` already
on PATH.

#### Architectures published

| Arch   | Triple                       |
| ------ | ---------------------------- |
| x86_64 | `x86_64-pc-windows-msvc`     |

aarch64 Windows is **not yet published**. If you're on ARM64,
[build from source](#from-source) with
`rustup target add aarch64-pc-windows-msvc`.

#### Where config + identity live

Config + identity + contacts live under `%APPDATA%\ppexchanger\`
(typically `C:\Users\<you>\AppData\Roaming\ppexchanger\`), created on
first run. On Git Bash, that path is `/c/Users/<you>/AppData/Roaming/ppexchanger/`.

#### Windows Firewall

Windows Firewall blocks inbound connections by default, so a freshly
installed `ppx` will bind `0.0.0.0:7777` but no peer on the LAN can
dial in until you allow inbound TCP on that port.

The installer adds the rule for you on Windows. The behavior depends
on how you invoke it:

| invocation                                            | result                                                                                  |
| ----------------------------------------------------- | --------------------------------------------------------------------------------------- |
| `bash install.sh` (interactive, Windows)              | After the binary is installed, prompts once: runs the UAC-elevated `netsh` add if Y.    |
| `curl … \| bash -s --` (Windows, non-interactive)     | Skips silently. Prints the manual `netsh` one-liner as a warning so you can finish it. |
| `curl … \| bash -s -- --firewall` (Windows)           | Always runs the UAC-elevated `netsh` add — no prompt.                                   |
| `curl … \| bash -s -- --no-firewall` (Windows)        | Suppresses both the prompt and the warning. Use this if you manage rules out-of-band.  |
| Linux                                                 | On the first startup, adds idempotent UFW rules for the TCP listener and UDP control port (sudo prompts once per port pair). |
| macOS                                                 | No automatic pf changes; the flag is silently ignored.                                  |

The rule written:

```
Name:        ppexchanger (TCP/7777)
Direction:   in
Action:      allow
Protocol:    TCP
Local port:  7777
Profiles:    private, domain
```

Re-running the installer with `--firewall` overwrites the rule by
name (Windows firewall store semantics), so updates are idempotent.

**Manual fallback** (if UAC was declined or you ran the non-interactive
variants above): open an elevated PowerShell ("Run as administrator")
and run:

```powershell
netsh advfirewall firewall add rule name="ppexchanger (TCP/7777)" dir=in action=allow protocol=TCP localport=7777 profile=private,domain
```

The same one-liner is also shown in the `/discover` popup if a scan
returns zero peers — the popup first checks whether the rule is
already in place so you won't be nagged once the install is healthy.

UDP multicast discovery (used by `/discover`) is often blocked even
when the TCP rule is allowed; in that case the popup falls back to
the TCP subnet scan, which reaches any peer whose firewall accepts
inbound TCP/7777.

### From source

Requires Rust 1.75+ (only audited dependencies, no native libraries).
Two routes:

**Through the installer** — interactive prompt or explicit flag. The
installer asks whether to download the binary or build from source
when stdin is a TTY; otherwise it defaults to the binary path. Force
source builds explicitly (use the `=` form to survive any quoting the
shell applies around `curl … | bash -s --`):

```sh
curl -fsSL https://github.com/PolderLabsVOF/ppexchanger/releases/latest/download/install.sh \
  | bash -s -- --method=source
```

The source path needs `git` and `cargo install` (i.e. `rustup`). It
clones the repo at the resolved tag, runs `cargo install --path . --locked`
into the same `$INSTALL_DIR`, then runs the same smoke test the
binary path uses. Pin a tag the same way as the binary path:

```sh
curl -fsSL https://github.com/PolderLabsVOF/ppexchanger/releases/latest/download/install.sh \
  | bash -s -- --method source --tag v0.5.0
```

> **Windows source builds** additionally need the
> [Visual Studio Build Tools](https://visualstudio.microsoft.com/visual-cpp-build-tools/)
> ("Desktop development with C++" workload) so `link.exe` is on PATH.
> Run the installer from **Git Bash** (not PowerShell or cmd) — the
> `uname -s` detection only recognises `MINGW*`/`MSYS*`/`CYGWIN*`.
> Full walk-through in the [Windows section](#windows) above.

**Manual** — clone + build, no installer:

```sh
cargo install --git https://github.com/PolderLabsVOF/ppexchanger --locked
# or
git clone https://github.com/PolderLabsVOF/ppexchanger
cd ppexchanger
cargo build --release
./target/release/ppx
```

## Usage

```sh
ppx                      # start the TUI
ppx --name alice         # override display name
ppx --port 7777          # bind a specific TCP port
ppx --theme amber        # default | solarized | monochrome | neon | amber
ppx --config /tmp/c.toml # alternate config path
ppx --no-mouse           # explicitly keep native terminal mouse selection
ppx --gen-identity       # print fingerprint + peer_id and exit
ppx --version
ppx --help
```

On first run, ppx generates an identity keypair and stores it under
`~/.config/ppexchanger/` (or `$XDG_CONFIG_HOME/ppx`):

- `identity` — 32-byte X25519 secret, 16-byte peer_id, display name
- `peerdb` — known contacts (name, public key, last seen, trust flag)
- `config.toml` — UI config (theme, scrollback, mouse, footer)
- `history` — authenticated ChaCha20-Poly1305 encrypted chat history

## Commands

Slash-commands are entered in the input line and start with `/`:

| Command                | What it does                                                  |
| ---------------------- | ------------------------------------------------------------- |
| `/peers`               | list every known peer with trust + state + fingerprint        |
| `/trust <name>`        | mark a peer as trusted; persists to peerdb                    |
| `/revoke <name>`       | remove a peer from peerdb                                     |
| `/theme <name>`        | switch theme (`default` / `solarized` / `monochrome` / `neon` / `amber`); saved to config.toml |
| `/settings`            | open the settings popup (theme, mouse, footer, scrollback)    |
| `/discover`            | run a UDP + TCP subnet scan and open the discovery popup      |
| `/map`                 | toggle the discovery popup between list view and Canvas map  |
| `/send <path>`         | send a file at `<path>` to the selected peer (binary; max 32 KiB per chunk) |
| `/quit`                | exit cleanly                                                  |

### Sending files

There are two ways to send a file to the selected peer:

1. **Paste a path** — type or paste a path like
   `/home/alice/report.pdf` and press Enter. If the path points at an
   existing regular file, ppx auto-detects it and starts a binary
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
| `Ctrl-,`      | open the settings popup                                |
| `1` / `2` / `3` | in settings popup: jump to Display / Input / About   |
| `←` / `→`     | in settings popup: cycle theme / scrollback / toggle mouse |
| `Enter` / `Space` | in settings popup: apply / toggle the selected row    |
| `Esc`         | cancel / clear input / close modal                     |
| `Ctrl-C` / `Ctrl-Q` | quit                                              |
| `?`           | toggle the help overlay                                |

### Menu bar

A single-row menu sits above the sidebar / chat panes:

```
[ Peers ]  [ Discover ]  [ Settings ]  [ Help ]  [ Quit ]
```

Each button is clickable. They map to the same actions as the
matching slash commands (`/discover`, `/settings`, `/help`, `/quit`)
plus a `Peers` shortcut that focuses the sidebar.

### Mouse

Native terminal mouse selection is always enabled so chat text can be
selected and copied exactly like normal terminal output. The legacy mouse
capture setting is ignored for chat selection.

* Left-click a row in the sidebar to select that peer and focus the sidebar.
* Left-click the chat pane to focus the chat.
* Left-click a value cell in the settings popup (theme / footer / mouse
  / scrollback) to toggle it.
* Scroll wheel in the chat pane scrolls the message history.

### Settings popup

`Ctrl-,` (or `/settings`) opens a live settings modal grouped into
three tabs (`1`/`2`/`3` to jump):

* **Display** — Theme (cycles through the five built-in palettes),
  Show footer (on / off), Scrollback (±100, clamped 16..50,000).
* **Input** — Mouse capture (on / off; effective next launch).
* **About** — read-only: version, fingerprint, config path, received
  files directory.

Every change is `dirty` until you press `Esc`, which persists the live
config back to `config.toml`. Click the right half of any row to apply
the same as `Enter`. The popup renders a `Tabs` widget for sub-nav and
a `Table` for the rows — both with the active theme's accent color.

The TUI does not intercept drag selection; selecting a region may include
surrounding chrome, just like selecting text in any terminal application.

### Pasting

Bracketed paste is **always on**. Paste any text — including a path
that resolves to an existing file — directly into the input line and
press Enter. Pasted payloads are capped at 1 MiB; anything bigger is
dropped silently so a stray log-file paste can't OOM the UI thread.

Pastes of 20 or more lines are offered as encrypted `.txt` attachments
instead of giant chat bubbles. The recipient can preview the first lines in
the offer, accept the download, and review the complete file at the saved
path after transfer.

PNG and JPEG images can be pasted with `/paste-image` or dragged from a file
manager into the composer. The receiver gets an inline terminal preview after
accepting the encrypted transfer; the original is kept under the received
files directory.

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

### Peer-map (Canvas) view

The discovery popup also has a second view — invoke `/map` (while the
popup is open) to flip to a Canvas-based peer map:

* x axis = last IPv4 octet (0..=255)
* y axis = 16-row hash of the /24 prefix
* marker = `Marker::Braille` for sub-cell dots so a single peer reads
  clearly even on a long LAN

Multicast-finds plot in green accent (trusted discovery); TCP-subnet
finds in amber. Press `/map` again to flip back to the list.

## Look & feel

The TUI ships a retro amber-phosphor CRT vibe out of the box:

* **Theme** — `amber` is the new default: dark brownish bg
  (`#1a0f00`), amber phosphor fg (`#ffb000`), green accent
  (`#66ff66`). Run `/theme default | solarized | monochrome | neon` for
  alternatives.
* **ASCII banners** — three layered glyph weights (light dots `·`,
  medium shade `▒`/`░`, heavy block `█`/`▌`) so logos read as varied
  weight instead of fixed-width ASCII. Visible as the startup banner
  in the chat pane and as the settings popup header.
* **CRT scanlines** — the chat pane alternates `Modifier::DIM` on
  every other row each frame, so messages appear to scan downward
  like an old terminal. Toggle-able by setting `theme` to anything
  non-amber if you need a clean look.

## Configuration

`~/.config/ppexchanger/config.toml`:

```toml
[ui]
theme = "default"        # default | solarized | monochrome | neon | amber
show_footer = true
mouse = false              # retained for compatibility; native selection is always used
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
  half is stored with 0600 permissions in `~/.config/ppexchanger/identity`
  on Linux/macOS (Windows uses NTFS ACL inheritance instead).
- **Trust model** — every peer is `untrusted` by default. Use `/trust
  <name>` to mark a peer as verified (typically after checking their
  fingerprint out-of-band). The trusted/untrusted flag persists in
  peerdb.
- **No server, no telemetry, no update channel** — the binary doesn't
  phone home. Run `ppx --gen-identity` to dump your fingerprint for
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
│   ├── input.rs   Line editor + EditorEvent dispatch (Ctrl-, → OpenSettings)
│   ├── theme.rs   Five built-in palettes + Unicode/ASCII glyph detection
│   ├── config.rs  Hand-rolled TOML-subset parser
│   ├── art.rs     ASCII banners (logo_large / logo_small / logo_settings)
│   ├── help.rs    `?` overlay
│   ├── discovery_popup.rs  `/discover` results modal
│   ├── file_offer_popup.rs Inbound file-offer modal
│   └── settings_popup.rs   `/settings` / `Ctrl-,` modal (Tabs + Table)
└── main.rs        CLI parsing, threading, action handling
```

## License

MIT.
