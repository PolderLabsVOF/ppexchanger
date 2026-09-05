# Self-hosted relay

PPX can optionally connect one trusted pair over the Internet through a small relay you run yourself. LAN discovery and direct LAN connections continue to work unchanged.

The relay is not a general proxy: it only joins the two clients in the same room, never dials arbitrary destinations, and does not store offline messages. PPX encrypts the conversation end to end; the relay can observe client addresses, room identifiers, timing, and packet sizes, but not message contents.

## Run a relay

### One-line install (recommended)

A guided installer downloads the binary, asks which exposure mode you want (Tailscale Funnel is recommended), writes a hardened systemd unit, sets up Tailscale (Funnel mode) or opens the firewall (direct mode), and starts the service:

```sh
curl -fsSL https://github.com/PolderLabsVOF/ppexchanger/releases/latest/download/install-relay.sh | sudo bash
```

Without flags, the installer prompts you to pick a mode. Pass `--yes` to skip the prompts and use the defaults, or pre-answer with flags such as `--bind 0.0.0.0 --max-clients 256`. Run `install-relay.sh --help` for the full list.

### Beta / pre-release builds

Pre-release tags (e.g. `v0.7.11-beta.1`) are tagged **Pre-release** on GitHub, so `/releases/latest/download/` always serves the most recent stable release. To install a beta, point the URL at the specific tag and pass `--tag` so the installer knows which binaries to fetch:

```sh
curl -fsSL https://github.com/PolderLabsVOF/ppexchanger/releases/download/v0.7.11-beta.1/install-relay.sh \
    | sudo bash -s -- --tag v0.7.11-beta.1
```

The script and the binaries it installs always come from the same tag, so features like `--funnel` that are still in beta are guaranteed to work. Add `--funnel` and/or `--tailscale-authkey tskey-...` as usual.

### Tailscale Funnel (recommended for most setups)

Funnel exposes the relay on the public Internet through Tailscale's edge. No port forwarding, no firewall rule, no public IP revealed. Requires a free Tailscale account.

```sh
sudo bash install-relay.sh --funnel
```

The installer downloads Tailscale, starts `tailscaled`, prints a login URL on the operator's terminal, and resumes automatically once the browser-login completes. The relay binds `127.0.0.1:10000` and Tailscale forwards `tcp://<node>.<tailnet>.ts.net:10000` to it. The final report shows the public `<node>.<tailnet>.ts.net:10000` address to share with the peer.

For unattended installs (CI, fleet provisioning), pass a Tailscale auth key:

```sh
sudo bash install-relay.sh --funnel --tailscale-authkey tskey-auth-...
```

The admin who generates the key may need to enable Funnel for the tailnet in the Tailscale admin panel (Settings → Funnel) — Funnel is free for personal and most paid plans.

### Direct bind

Bind a local address and open the firewall. Use this when you want to expose the relay on a specific VPS without Tailscale.

```sh
sudo bash install-relay.sh --bind 0.0.0.0 --port 47393 --max-clients 256 --max-per-ip 8
```

The installer opens inbound TCP port 47393 via ufw, firewalld, or iptables when available. Open the same port on any provider or cloud panel that fronts the relay host. PPX clients only need normal outbound TCP access.

### Manual install

Build and install the binary by hand:

```sh
cargo build --release --locked --bin ppx-relay
sudo install -m 0755 target/release/ppx-relay /usr/local/bin/ppx-relay
```

Then copy `deploy/ppx-relay.service` from this repository to `/etc/systemd/system/ppx-relay.service`. Its default listens only on loopback; change `--bind 127.0.0.1:47393` to `--bind 0.0.0.0:47393` only when the host firewall is configured as above. Then run:

```sh
sudo systemctl daemon-reload
sudo systemctl enable --now ppx-relay
```

## Configure a pair

Each person needs their own PPX identity. Exchange public keys out of band, for example in person or through an authenticated password manager entry:

```sh
ppx --gen-identity
```

Generate a fresh room token and share it only with this one peer:

```sh
ppx --relay-token
```

Create `relay.conf` in PPX's configuration directory (normally `~/.config/ppexchanger/relay.conf` on Linux) on each device:

```toml
server = "relay.example.net:47393"
room = "the-64-hex-character-token-from-ppx-relay-token"
peer_key = "the-other-persons-64-hex-character-public-key"
```

`peer_key` is always the **other** person's public key. This mandatory pin prevents a relay or anyone knowing a room identifier from impersonating your peer. Start PPX normally; it automatically loads the file. Use `--relay-config /path/to/relay.conf` to select another file, or `--no-relay` to temporarily disable relay use.

The initial implementation supports one configured relay peer per PPX installation. Use a fresh room for every pair and rotate it immediately when sharing is revoked. If either person resets their identity, manually replace the corresponding `peer_key` on the other device before reconnecting.

## Format and troubleshooting

The file is intentionally strict and small (at most 4 KiB). It accepts only the three quoted fields above, once each. `server` is a hostname or IPv4 address with a non-zero port, or an IPv6 literal in brackets, for example `"[2001:db8::10]:47393"`. `room` and `peer_key` must each be 64 hexadecimal characters and may not be all zeroes. PPX rejects malformed, duplicate, or unknown entries rather than guessing.

Both clients must be online at the same time. Existing PPX local queueing and reconnect behavior still applies when the peer returns.
