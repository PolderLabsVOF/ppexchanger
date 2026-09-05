# Changelog

All notable ppexchanger updates are listed here in plain language.

## 0.7.10 — 2026-09-05 — relay installer

- New `install-relay.sh` does a guided, one-line install of the self-hosted relay: download, checksum, install, write a hardened systemd unit, open the firewall, start the service.
  `curl -fsSL https://github.com/PolderLabsVOF/ppexchanger/releases/latest/download/install-relay.sh | sudo bash`
- The installer works in three modes: system-wide (default when run as root), per-user (no root needed, `--user`), or binary-only (`--no-systemd`). It is fully scriptable via flags and idempotent on re-run.
- ufw / firewalld / iptables are detected in order; the rule is only added when the bind address is not loopback.

## 0.7.9 — 2026-09-05 — self-hosted relay

PPX can now connect one trusted pair over the Internet through a small relay you run yourself. LAN discovery and direct LAN connections continue to work unchanged.

- New `ppx-relay` binary pairs two PPX clients in the same room over a public server you operate; the relay never decrypts traffic, never stores messages, and never dials arbitrary destinations.
- Each client pins the other device's public key in `relay.conf` so a relay — or anyone who learns a room identifier — cannot impersonate your peer.
- New CLI flags: `--relay-config <path>` to pick a specific file, `--no-relay` to ignore a saved file for one run, and `--relay-token` to mint a fresh 32-byte room token.
- Linux release tarballs ship `ppx-relay` alongside `ppx` plus a hardened systemd unit (`ppx-relay.service`).
- See `docs/relay.md` for setup, configuration format, and troubleshooting.

## 0.7.8 — 2026-09-05

- Settings now have clear Profile, Appearance, Chat, Privacy, and About sections, with explanations for each option.
- Control desktop alerts and image previews separately, and adjust the sidebar layout from settings.
- Hiding the status footer keeps the message box visible.
- Cancel name edits with Escape; failed saves keep settings open for retry.

## 0.7.7 — 2026-09-05

- Changed the default connection and discovery port to 47391, with reverse connections on 47392.
- Update both devices when moving from the old ports so they can discover each other.

## 0.7.6 — 2026-09-05

### Smoother conversations

- Pasted text stays attached when you switch focus or navigate.
- Holding Backspace works as expected, and unused keyboard shortcuts no longer type stray letters.
- Peer selection and menu clicks match what you see, including smaller windows and longer peer lists.
- Wide characters fit the message box more accurately.
- Long chat histories use less memory while drawing, and scrolling no longer leaves the chat empty.

### More reliable updates

- Run `ppx update` to install the latest release.
- If a ready-made download is unavailable, ppx tries building the release from source into the same installation folder.
- Failed updates now report failure correctly, without changing firewall settings.

## 0.7.5 — 2026-09-03

### Reliable releases

- Stable releases now publish the matching npm launcher automatically.
- Install the latest version with one global npm command, without script approval prompts.
- Release checks keep the native app and npm launcher versions in sync.

## 0.7.2 — 2026-09-03

### Cleaner npm installs

- npm installs no longer depend on lifecycle scripts or script approvals.
- The launcher downloads the matching native binary the first time `ppx` is
  run, keeping installs safe and predictable.

## 0.7.1 — 2026-09-03

### Safer updates and releases

- Added clear development, beta, and stable release channels.
- Nightly builds now follow the latest development work without changing the
  stable release channel.
- Improved release validation so beta builds cannot be published as stable
  releases, and stable releases cannot be cut from the wrong branch.

## 0.7.0 — 2026-09-03

### A smoother chat experience

- Long pasted text now appears as a tidy text-paste card instead of an
  unreadable temporary filename.
- Click a text-paste card to expand it inside the conversation. Scroll through
  the full content, then click the card again to collapse it.
- Incoming and outgoing file transfers now complete reliably, including when a
  transfer starts immediately after a connection is opened.
- Image previews are faster and render consistently across terminal programs.
- Click an image attachment to reveal it in your normal file manager.
- New messages appear promptly and the conversation no longer displays an
  in-terminal notification popup over your chat.

### Commands and setup

- Press Enter to finish a partially typed slash command, such as `/disc` →
  `/discover`.
- Added `/help` to open the keyboard and command guide.
- `/map` can now start discovery for you when no discovery window is open.
- `/peers`, `/trust`, and `/revoke` provide clearer feedback and handle full
  display names more naturally.
- Display names, encrypted chat history, saved peers, and reconnect behavior
  continue to work across restarts.

### Reliability

- Improved offline handling and queued delivery for messages and attachments.
- Better status messages for failed, queued, completed, and unavailable
  transfers.
- Reduced redraw work so the interface remains responsive with image history.

## Earlier releases

See the [GitHub release history](https://github.com/PolderLabsVOF/ppexchanger/releases)
for previous versions.
