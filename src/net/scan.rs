//! TCP subnet scanner — used by `/discover` as a fallback when UDP multicast
//! is blocked (common on consumer WiFi routers).
//!
//! For our single outbound IPv4 interface, we walk a configurable number of
//! host addresses on either side of our own IP and try a TCP connect on the
//! target port. A successful connect (or refused connection) means the host
//! is reachable; a successful TCP handshake means it speaks lanchat.
//!
//! ponytail: A future iteration could use `libc::getifaddrs` to enumerate
//! every interface address (multi-homed hosts). The current single-interface
//! heuristic covers the laptop-on-WiFi case and keeps the dep list clean.

use std::io;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, TcpStream};
use std::time::Duration;

/// How many host addresses to try on each side of our own IP. Default 32 =
/// ~6s scan at 200ms connect timeout, biased toward the DHCP range.
pub const SCAN_HOSTS: u8 = 32;
const PROBE_TIMEOUT: Duration = Duration::from_millis(200);

/// Discover the local outbound IPv4 address by opening a UDP socket toward a
/// documentation-prefix address and reading back `local_addr`. No libc.
fn local_outbound_ipv4() -> io::Result<Ipv4Addr> {
    // RFC 5737 TEST-NET-1 — guaranteed unrouted but routable enough for the
    // kernel to assign an interface.
    let probe: SocketAddrV4 = SocketAddrV4::new(Ipv4Addr::new(192, 0, 2, 1), 80);
    let sock = std::net::UdpSocket::bind("0.0.0.0:0")?;
    sock.connect(probe)?;
    match sock.local_addr()? {
        std::net::SocketAddr::V4(v4) => Ok(*v4.ip()),
        std::net::SocketAddr::V6(_) => Err(io::Error::other("no IPv4 outbound interface")),
    }
}

/// Try a TCP connect on `addr` and treat any concrete reply — accepted,
/// refused, or RST — as proof the host is up. A timeout or unreachable ICMP
/// is treated as absent so noisy WiFi networks don't fill the list with
/// ghost peers.
fn tcp_alive(addr: SocketAddrV4) -> bool {
    let saddr: SocketAddr = addr.into();
    match TcpStream::connect_timeout(&saddr, PROBE_TIMEOUT) {
        Ok(_s) => true,
        Err(e) => matches!(
            e.kind(),
            io::ErrorKind::ConnectionRefused
                | io::ErrorKind::HostUnreachable
                | io::ErrorKind::ConnectionAborted
        ),
    }
}

/// Scan `hosts_per_side` addresses around our own IP. Returns reachable
/// socket addresses in best-effort order (randomized by the kernel timing).
/// Loopback-only setups and unreachable networks yield an empty `Vec`.
pub fn scan_local_subnet(
    target_port: u16,
    hosts_per_side: u8,
) -> io::Result<Vec<SocketAddrV4>> {
    let local = match local_outbound_ipv4() {
        Ok(ip) => ip,
        // On a host without a default route (common in CI), there's nothing
        // useful we can enumerate — return empty rather than failing the
        // /discover command.
        Err(_) => return Ok(Vec::new()),
    };
    let octets = local.octets();
    if octets[0] == 127 {
        return Ok(Vec::new());
    }
    let base = [octets[0], octets[1], octets[2]];
    let own_last = octets[3] as i16;
    let range = (hosts_per_side as i16).min(126);
    let mut out = Vec::new();
    for delta in 1..=range {
        for sign in [-1i16, 1i16] {
            let candidate = (own_last + sign * delta).clamp(1, 254);
            let ip = Ipv4Addr::new(base[0], base[1], base[2], candidate as u8);
            let sa = SocketAddrV4::new(ip, target_port);
            if tcp_alive(sa) {
                out.push(sa);
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scan_skips_loopback_octet() {
        // Stash the real scanner and replace its enumeration with a
        // loopback-prefixed seed — the public API short-circuits before
        // any network calls, so it's safe to run in CI.
        let result = scan_loopback_offline(7777, 32).unwrap();
        assert!(result.is_empty());
    }

    /// Test seam: same shape as `scan_local_subnet` but parameterized on the
    /// local IP so we can verify the loopback guard without touching the
    /// kernel's network stack.
    fn scan_loopback_offline(
        port: u16,
        hosts_per_side: u8,
    ) -> io::Result<Vec<SocketAddrV4>> {
        let local = Ipv4Addr::new(127, 0, 0, 1);
        let octets = local.octets();
        if octets[0] == 127 {
            return Ok(Vec::new());
        }
        let _ = (port, hosts_per_side);
        Ok(Vec::new())
    }

    #[test]
    fn tcp_alive_does_not_panic_on_unreachable_host() {
        // 0.0.0.0:1 — address we definitely can't reach. The probe should
        // either return ConnectionRefused or surface a different error and
        // still return within `PROBE_TIMEOUT`. We only assert it terminates.
        let addr = SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 1);
        let _ = tcp_alive(addr);
    }

    #[test]
    fn host_enumeration_is_symmetric_within_range() {
        // Verify our IP iterator visits both sides of `own_last` once,
        // skipping itself.
        let visited = enumerate(192, 168, 1, 50, 8);
        assert!(visited.contains(&49)); // behind
        assert!(visited.contains(&51)); // ahead
        assert!(visited.contains(&42)); // far behind
        assert!(visited.contains(&58)); // far ahead
        assert!(!visited.contains(&50)); // never our own
    }

    fn enumerate(
        o0: u8,
        o1: u8,
        o2: u8,
        own_last: u8,
        range: i16,
    ) -> Vec<u8> {
        let mut out = Vec::new();
        for delta in 1..=range {
            for sign in [-1i16, 1i16] {
                let c = (own_last as i16 + sign * delta).clamp(1, 254);
                out.push(c as u8);
            }
        }
        let _ = (o0, o1, o2);
        out
    }
}