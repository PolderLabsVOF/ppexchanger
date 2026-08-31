//! UDP multicast peer discovery.
//!
//! Every `ppexchanger` instance joins the multicast group `239.255.42.99` on
//! port `7777` and announces itself by periodically broadcasting a
//! `protocol::Beacon`. Received beacons (from other peers) are yielded to
//! callers via `recv_beacons`.
//!
//! ## Broadcast fallback
//!
//! Consumer WiFi routers often block multicast between associated clients.
//! When multicast fails to find peers, we fall back to UDP broadcast on the
//! local subnet. The `announce_both` method sends to BOTH the multicast
//! group AND the local subnet's broadcast address, maximizing discovery
//! chances regardless of network configuration.

use crate::protocol::{decode_beacon, encode_beacon, Beacon};
use std::io;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, UdpSocket};
use std::time::{Duration, Instant};

/// Multicast group used by every `ppexchanger` instance.
pub const MULTICAST_GROUP: Ipv4Addr = Ipv4Addr::new(239, 255, 42, 99);
pub const MULTICAST_PORT: u16 = 7777;

/// Announcement interval.
pub const ANNOUNCE_INTERVAL: Duration = Duration::from_secs(2);

/// UDP socket bound to the multicast port, joined to the multicast group,
/// configured to send announcements back to the group.
pub struct Discovery {
    socket: UdpSocket,
    group_addr: SocketAddr,
    /// Optional local IP for broadcast fallback. Computed on demand via
    /// `local_subnet_broadcast()` when `announce_both` is called.
    local_ip: Option<Ipv4Addr>,
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
        Ok(Self { socket, group_addr, local_ip: None })
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

    /// Compute the local subnet's broadcast address (assumes /24 subnet).
    /// Uses the same trick as `scan::local_outbound_ipv4`: bind a UDP socket
    /// to an external address and read back our local IP.
    pub fn local_subnet_broadcast() -> io::Result<Ipv4Addr> {
        let probe: SocketAddrV4 = SocketAddrV4::new(Ipv4Addr::new(192, 0, 2, 1), 80);
        let sock = UdpSocket::bind("0.0.0.0:0")?;
        sock.connect(probe)?;
        match sock.local_addr()? {
            std::net::SocketAddr::V4(v4) => {
                // Assume /24 — broadcast is x.x.x.255
                let ip = *v4.ip();
                let mut octets = ip.octets();
                octets[3] = 255;
                Ok(Ipv4Addr::new(octets[0], octets[1], octets[2], octets[3]))
            }
            std::net::SocketAddr::V6(_) => Err(io::Error::other("no IPv4 outbound interface")),
        }
    }

    /// Send one beacon to the given address. Used for broadcast fallback.
    fn send_to(&self, addr: SocketAddr, beacon: &Beacon) -> io::Result<()> {
        let bytes = encode_beacon(beacon).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "beacon encode failed")
        })?;
        self.socket.send_to(&bytes, addr)?;
        Ok(())
    }

    /// Announce via BOTH multicast group AND local subnet broadcast.
    /// This maximizes discovery chances on networks where multicast may be
    /// blocked by consumer routers.
    pub fn announce_both(&mut self, beacon: &Beacon) -> io::Result<()> {
        // Always send to multicast group.
        self.send_to(self.group_addr, beacon)?;
        // Also try local subnet broadcast if available.
        if self.local_ip.is_none() {
            self.local_ip = Self::local_subnet_broadcast().ok();
        }
        if let Some(local_ip) = self.local_ip {
            let broadcast_addr = SocketAddr::V4(SocketAddrV4::new(local_ip, MULTICAST_PORT));
            let _ = self.send_to(broadcast_addr, beacon);
        }
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
        &mut self,
        beacon: Beacon,
        stop: &std::sync::atomic::AtomicBool,
    ) -> io::Result<()> {
        use std::sync::atomic::Ordering;
        // Send one immediately so the UI is non-empty.
        let _ = self.announce_both(&beacon);
        let mut last = Instant::now();
        loop {
            if stop.load(Ordering::Relaxed) {
                return Ok(());
            }
            if last.elapsed() >= ANNOUNCE_INTERVAL {
                self.announce_both(&beacon)?;
                last = Instant::now();
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}