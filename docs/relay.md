# Self-hosted relay

PPX can optionally connect one trusted pair over the Internet through a small relay you run yourself. LAN discovery and direct LAN connections continue to work unchanged.

The relay is not a general proxy: it only joins the two clients in the same room, never dials arbitrary destinations, and does not store offline messages. PPX encrypts the conversation end to end; the relay can observe client addresses, room identifiers, timing, and packet sizes, but not message contents.

## Run a relay

Build the relay on a Linux host with a public address:

```sh
cargo build --release --locked --bin ppx-relay
sudo install -m 0755 target/release/ppx-relay /usr/local/bin/ppx-relay
```

For a public relay, explicitly choose its public bind address and limits:

```sh
ppx-relay --bind 0.0.0.0:47393 --max-clients 256 --max-per-ip 8
```

Open **only inbound TCP port 47393** on the relay host (and its provider firewall, if applicable). PPX clients only need normal outbound TCP access. Do not add firewall rules on client devices merely for relay use.

For systemd, copy `deploy/ppx-relay.service` to `/etc/systemd/system/ppx-relay.service`. Its default listens only on loopback; change `--bind 127.0.0.1:47393` to `--bind 0.0.0.0:47393` only when the host firewall is configured as above. Then run:

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
