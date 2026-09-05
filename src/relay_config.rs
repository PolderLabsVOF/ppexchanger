//! Strict local configuration for an optional self-hosted PPX relay.

use std::fmt;
use std::fs;
use std::io;
use std::net::Ipv6Addr;
use std::path::Path;

const MAX_CONFIG_BYTES: u64 = 4 * 1024;

/// Configuration for the optional, pair-only Internet relay.
/// `room` is a shared secret and is intentionally redacted by `Debug`.
#[derive(Clone)]
pub struct RelayConfig {
    pub server: String,
    pub room: [u8; 32],
    pub peer_key: [u8; 32],
}

impl fmt::Debug for RelayConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RelayConfig")
            .field("server", &self.server)
            .field("room", &"[REDACTED]")
            .field("peer_key", &hex(&self.peer_key))
            .finish()
    }
}

impl RelayConfig {
    /// Load and validate a configuration file, refusing input over 4 KiB.
    pub fn load(path: &Path) -> io::Result<Self> {
        if fs::metadata(path)?.len() > MAX_CONFIG_BYTES {
            return invalid("relay configuration exceeds 4 KiB");
        }
        Self::parse(&fs::read_to_string(path)?)
    }

    /// Parse the compact `relay.conf` format without DNS or other networking.
    pub fn parse(text: &str) -> io::Result<Self> {
        if text.len() > MAX_CONFIG_BYTES as usize {
            return invalid("relay configuration exceeds 4 KiB");
        }
        let mut server = None;
        let mut room = None;
        let mut peer_key = None;
        for (number, raw) in text.lines().enumerate() {
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let (key, raw_value) = line.split_once('=').ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "relay configuration line {} must use key = \"value\"",
                        number + 1
                    ),
                )
            })?;
            let value = quoted_value(raw_value.trim(), number + 1)?;
            match key.trim() {
                "server" => set_once(&mut server, value.to_owned(), "server")?,
                "room" => set_once(&mut room, decode_key(value, "room")?, "room")?,
                "peer_key" => set_once(&mut peer_key, decode_key(value, "peer_key")?, "peer_key")?,
                unknown => return invalid(format!("unknown relay configuration key: {unknown}")),
            }
        }
        let server = server.ok_or_else(|| missing("server"))?;
        validate_endpoint(&server)?;
        let room = room.ok_or_else(|| missing("room"))?;
        let peer_key = peer_key.ok_or_else(|| missing("peer_key"))?;
        if room.iter().all(|&b| b == 0) || peer_key.iter().all(|&b| b == 0) {
            return invalid("room and peer_key must not be all zeroes");
        }
        Ok(Self {
            server,
            room,
            peer_key,
        })
    }
}

fn quoted_value(value: &str, line: usize) -> io::Result<&str> {
    value
        .strip_prefix('"')
        .and_then(|v| v.strip_suffix('"'))
        .filter(|v| !v.contains('"'))
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("relay configuration line {line} must use a quoted value"),
            )
        })
}

fn set_once<T>(slot: &mut Option<T>, value: T, name: &str) -> io::Result<()> {
    if slot.replace(value).is_some() {
        return invalid(format!("duplicate relay configuration key: {name}"));
    }
    Ok(())
}

fn decode_key(value: &str, name: &str) -> io::Result<[u8; 32]> {
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return invalid(format!("{name} must be exactly 64 hexadecimal characters"));
    }
    let mut result = [0; 32];
    for (index, byte) in result.iter_mut().enumerate() {
        let i = index * 2;
        *byte = u8::from_str_radix(&value[i..i + 2], 16).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{name} must be exactly 64 hexadecimal characters"),
            )
        })?;
    }
    Ok(result)
}

fn validate_endpoint(endpoint: &str) -> io::Result<()> {
    if endpoint.is_empty()
        || endpoint.len() > 255
        || endpoint
            .bytes()
            .any(|b| b.is_ascii_whitespace() || b.is_ascii_control())
    {
        return invalid("server must be a host:port endpoint");
    }
    let port = if let Some(rest) = endpoint.strip_prefix('[') {
        let (host, port) = rest
            .split_once("]:")
            .filter(|(_, p)| !p.contains(':'))
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidData, "server must use [ipv6]:port")
            })?;
        if host.is_empty() || host.parse::<Ipv6Addr>().is_err() {
            return invalid("server must use a valid IPv6 address in brackets");
        }
        port
    } else {
        let (host, port) = endpoint
            .rsplit_once(':')
            .filter(|(h, _)| !h.contains(':'))
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "server must be a host:port endpoint",
                )
            })?;
        if !valid_hostname(host) {
            return invalid("server host contains unsupported characters");
        }
        port
    };
    let port: u16 = port.parse().map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "server port must be a number from 1 to 65535",
        )
    })?;
    if port == 0 {
        return invalid("server port must be non-zero");
    }
    Ok(())
}

fn valid_hostname(host: &str) -> bool {
    !host.is_empty()
        && !host.starts_with('.')
        && !host.ends_with('.')
        && host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-'))
}

fn invalid<T>(message: impl Into<String>) -> io::Result<T> {
    Err(io::Error::new(io::ErrorKind::InvalidData, message.into()))
}

fn missing(name: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("missing relay configuration key: {name}"),
    )
}

fn hex(bytes: &[u8; 32]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut result = String::with_capacity(64);
    for byte in bytes {
        result.push(DIGITS[(byte >> 4) as usize] as char);
        result.push(DIGITS[(byte & 15) as usize] as char);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::RelayConfig;
    use std::{fs, io};
    const KEY: &str = "0101010101010101010101010101010101010101010101010101010101010101";
    const PEER: &str = "0202020202020202020202020202020202020202020202020202020202020202";
    fn config(server: &str) -> String {
        format!("server = \"{server}\"\nroom = \"{KEY}\"\npeer_key = \"{PEER}\"\n")
    }
    #[test]
    fn parses_hostname_and_ipv6_endpoints() {
        assert_eq!(
            RelayConfig::parse(&config("relay.example.test:47393"))
                .unwrap()
                .server,
            "relay.example.test:47393"
        );
        assert_eq!(
            RelayConfig::parse(&config("[2001:db8::1]:47393"))
                .unwrap()
                .server,
            "[2001:db8::1]:47393"
        );
    }
    #[test]
    fn parser_fails_closed_for_bad_files() {
        let duplicate = format!("{}room = \"{KEY}\"\n", config("relay.example.test:47393"));
        let unknown = format!("{}extra = \"no\"\n", config("relay.example.test:47393"));
        for text in [
            "server = \"relay.example.test:47393\"\nroom = \"0101\"\n",
            &duplicate,
            &unknown,
            &config("relay.example.test:0"),
            &config("2001:db8::1:47393"),
            &config("relay.example.test:not-a-port"),
        ] {
            assert_eq!(
                RelayConfig::parse(text).unwrap_err().kind(),
                io::ErrorKind::InvalidData
            );
        }
    }
    #[test]
    fn parser_rejects_zero_secrets_and_large_input() {
        let zero = "00".repeat(32);
        assert!(RelayConfig::parse(&format!(
            "server = \"relay.example.test:47393\"\nroom = \"{zero}\"\npeer_key = \"{PEER}\"\n"
        ))
        .is_err());
        assert!(RelayConfig::parse(&"#".repeat(4097)).is_err());
    }
    #[test]
    fn load_rejects_large_file() {
        let path = std::env::temp_dir().join(format!("ppx-relay-config-{}", std::process::id()));
        fs::write(&path, "x".repeat(4097)).unwrap();
        assert!(RelayConfig::load(&path).is_err());
        let _ = fs::remove_file(path);
    }
    #[test]
    fn debug_redacts_room_token() {
        let shown = format!(
            "{:?}",
            RelayConfig::parse(&config("relay.example.test:47393")).unwrap()
        );
        assert!(shown.contains("[REDACTED]"));
        assert!(!shown.contains(KEY));
    }
}
