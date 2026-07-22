//! UDP multicast peer discovery.
//!
//! Every `lanchat` instance joins the multicast group `239.255.42.99` on
//! port `7777` and announces itself by periodically broadcasting a
//! `protocol::Beacon`. Received beacons (from other peers) are yielded to
//! callers via `recv_beacons`.
//!
//! Note: some consumer WiFi routers block multicast between associated
//! clients. If discovery doesn't work on your network, the only stdlib-side
//! fix is to switch from multicast to broadcast (255.255.255.255) — see the
//! `broadcast_fallback_enabled` flag.

use crate::protocol::{decode_beacon, encode_beacon, Beacon};
use std::io;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, UdpSocket};
use std::time::{Duration, Instant};

/// Multicast group used by every `lanchat` instance.
pub const MULTICAST_GROUP: Ipv4Addr = Ipv4Addr::new(239, 255, 42, 99);
pub const MULTICAST_PORT: u16 = 7777;

/// Announcement interval.
pub const ANNOUNCE_INTERVAL: Duration = Duration::from_secs(2);

/// UDP socket bound to the multicast port, joined to the multicast group,
/// configured to send announcements back to the group.
pub struct Discovery {
    socket: UdpSocket,
    group_addr: SocketAddr,
}

impl Discovery {
    /// Bind a UDP socket on the given local port (use `0` for ephemeral) and
    /// join the multicast group on all available IPv4 interfaces.
    pub fn bind(local_port: u16) -> io::Result<Self> {
        let bind: SocketAddr = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, local_port));
        let socket = UdpSocket::bind(bind)?;
        // Permit others on the host to also bind (different processes).
        socket.set_broadcast(true)?;
        // Join the multicast group on every interface std knows about.
        // `join_multicast_v4` on the unspecified addr joins on the default
        // interface, which is enough for the common case. Loopback-only
        // setups will need to bind to 127.0.0.1 explicitly.
        socket.join_multicast_v4(&MULTICAST_GROUP, &Ipv4Addr::UNSPECIFIED)?;
        socket.set_read_timeout(Some(Duration::from_millis(500)))?;
        let group_addr = SocketAddr::V4(SocketAddrV4::new(MULTICAST_GROUP, MULTICAST_PORT));
        Ok(Self { socket, group_addr })
    }

    /// The local UDP port the socket is bound to.
    pub fn local_port(&self) -> io::Result<u16> {
        self.socket.local_addr().map(|a| a.port())
    }

    /// Send one beacon announcing our identity.
    pub fn announce(&self, beacon: &Beacon) -> io::Result<()> {
        let bytes = encode_beacon(beacon).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "beacon encode failed")
        })?;
        self.socket.send_to(&bytes, self.group_addr)?;
        Ok(())
    }

    /// Read one beacon from the multicast group. Returns `Ok(None)` on
    /// read-timeout (no beacon within `timeout`) or on a malformed packet.
    pub fn recv_beacon(&self) -> io::Result<Option<(SocketAddr, Beacon)>> {
        let mut buf = [0u8; 1024];
        match self.socket.recv_from(&mut buf) {
            Ok((n, addr)) => match decode_beacon(&buf[..n]) {
                Some(b) => Ok(Some((addr, b))),
                None => Ok(None),
            },
            Err(e) if e.kind() == io::ErrorKind::WouldBlock || e.kind() == io::ErrorKind::TimedOut => {
                Ok(None)
            }
            Err(e) => Err(e),
        }
    }

    /// Convenience: announce periodically in a loop until the stop signal
    /// is set. The caller is responsible for spawning this on its own thread.
    pub fn announce_loop(
        &self,
        beacon: Beacon,
        stop: &std::sync::atomic::AtomicBool,
    ) -> io::Result<()> {
        use std::sync::atomic::Ordering;
        // Send one immediately so the UI is non-empty.
        let _ = self.announce(&beacon);
        let mut last = Instant::now();
        loop {
            if stop.load(Ordering::Relaxed) {
                return Ok(());
            }
            if last.elapsed() >= ANNOUNCE_INTERVAL {
                self.announce(&beacon)?;
                last = Instant::now();
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}