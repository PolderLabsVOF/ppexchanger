//! Wire format for `lanchat`.
//!
//! Two distinct formats:
//!   1. UDP **beacon** — short announcement on the multicast group.
//!   2. TCP session frames — length-prefixed, AEAD-encrypted payloads.
//!
//! All multi-byte integers are big-endian. No external serialization crate is
//! used; the encoders/decoders are hand-written because the schema is small,
//! stable, and the zero-dep ethos is worth preserving for the protocol layer.

use crate::crypto::sha256;
use std::convert::TryInto;

/// UDP beacon magic: `"LANC"` in ASCII.
pub const BEACON_MAGIC: [u8; 4] = [0x4C, 0x41, 0x4E, 0x43];
pub const BEACON_VERSION: u8 = 1;
pub const BEACON_MSG_TYPE: u8 = 1;

/// Hard cap on beacon body — keeps malformed packets from chewing CPU.
pub const BEACON_MAX_BYTES: usize = 256;

/// Maximum encrypted TCP frame payload (after AEAD tag is added).
pub const FRAME_MAX_PAYLOAD: usize = 64 * 1024;

/// Decoded contents of a UDP beacon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Beacon {
    pub peer_id: [u8; 16],
    pub public_key: [u8; 32],
    pub tcp_port: u16,
    pub name: String,
}

/// Encode a beacon into bytes. Returns `None` if `name` is too long or `tcp_port == 0`.
pub fn encode_beacon(b: &Beacon) -> Option<Vec<u8>> {
    if b.name.is_empty() || b.name.len() > u16::MAX as usize {
        return None;
    }
    let name_bytes = b.name.as_bytes();
    let mut out = Vec::with_capacity(4 + 1 + 1 + 2 + 16 + 32 + 2 + name_bytes.len() + 4);
    out.extend_from_slice(&BEACON_MAGIC);
    out.push(BEACON_VERSION);
    out.push(BEACON_MSG_TYPE);
    let payload_len = (16 + 32 + 2 + 2 + name_bytes.len()) as u16;
    out.extend_from_slice(&payload_len.to_be_bytes());
    out.extend_from_slice(&b.peer_id);
    out.extend_from_slice(&b.public_key);
    out.extend_from_slice(&b.tcp_port.to_be_bytes());
    out.extend_from_slice(&(name_bytes.len() as u16).to_be_bytes());
    out.extend_from_slice(name_bytes);
    let crc = crc32(&out);
    out.extend_from_slice(&crc.to_be_bytes());
    Some(out)
}

/// Decode a beacon from bytes. Returns `None` on any structural or CRC failure.
pub fn decode_beacon(bytes: &[u8]) -> Option<Beacon> {
    if bytes.len() < 4 + 1 + 1 + 2 + 16 + 32 + 2 + 2 + 4 {
        return None;
    }
    if bytes.len() > BEACON_MAX_BYTES {
        return None;
    }
    if &bytes[0..4] != &BEACON_MAGIC {
        return None;
    }
    if bytes[4] != BEACON_VERSION || bytes[5] != BEACON_MSG_TYPE {
        return None;
    }
    let payload_len = u16::from_be_bytes(bytes[6..8].try_into().ok()?) as usize;
    let total_len = 8 + payload_len + 4;
    if bytes.len() != total_len {
        return None;
    }
    let crc = u32::from_be_bytes(bytes[total_len - 4..total_len].try_into().ok()?);
    if crc32(&bytes[..total_len - 4]) != crc {
        return None;
    }
    let mut p = 8;
    let mut peer_id = [0u8; 16];
    peer_id.copy_from_slice(&bytes[p..p + 16]);
    p += 16;
    let mut public_key = [0u8; 32];
    public_key.copy_from_slice(&bytes[p..p + 32]);
    p += 32;
    let tcp_port = u16::from_be_bytes(bytes[p..p + 2].try_into().ok()?);
    if tcp_port == 0 {
        return None;
    }
    p += 2;
    let name_len = u16::from_be_bytes(bytes[p..p + 2].try_into().ok()?) as usize;
    p += 2;
    if p + name_len + 4 > total_len {
        return None;
    }
    let name = std::str::from_utf8(&bytes[p..p + name_len]).ok()?.to_string();
    if name.is_empty() {
        return None;
    }
    Some(Beacon {
        peer_id,
        public_key,
        tcp_port,
        name,
    })
}

/// Compute the 16-hex-char fingerprint of a peer's static X25519 public key.
/// First 8 bytes of SHA-256(pubkey), displayed in lowercase hex.
pub fn fingerprint(pubkey: &[u8; 32]) -> String {
    let h = sha256(pubkey);
    let mut s = String::with_capacity(16);
    for b in &h[..8] {
        s.push_str(&format!("{:02x}", b));
    }
    s
}

/// TCP frame header length (cleartext) — 8-byte seq + 4-byte len.
pub const FRAME_HEADER_LEN: usize = 12;

/// Body of an encrypted TCP frame (the plaintext payload type).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FrameBody {
    Text(String),
    Bye,
}

/// Length-prefixed cleartext frame as it appears on the wire before encryption.
#[derive(Debug, PartialEq, Eq)]
pub struct PlainFrame {
    pub seq: u64,
    pub body: FrameBody,
}

/// Encode `(seq, body)` into a cleartext byte buffer.
pub fn encode_plain_frame(seq: u64, body: &FrameBody) -> Vec<u8> {
    let payload = match body {
        FrameBody::Text(s) => {
            let mut v = Vec::with_capacity(1 + s.len());
            v.push(1u8);
            v.extend_from_slice(s.as_bytes());
            v
        }
        FrameBody::Bye => vec![0u8],
    };
    let mut out = Vec::with_capacity(FRAME_HEADER_LEN + payload.len());
    out.extend_from_slice(&seq.to_be_bytes());
    out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    out.extend_from_slice(&payload);
    out
}

/// Decode a cleartext frame buffer.
pub fn decode_plain_frame(buf: &[u8]) -> Option<PlainFrame> {
    if buf.len() < FRAME_HEADER_LEN {
        return None;
    }
    let seq = u64::from_be_bytes(buf[0..8].try_into().ok()?);
    let len = u32::from_be_bytes(buf[8..12].try_into().ok()?) as usize;
    if buf.len() != FRAME_HEADER_LEN + len || len == 0 || len > FRAME_MAX_PAYLOAD {
        return None;
    }
    let payload = &buf[FRAME_HEADER_LEN..];
    let body = match payload[0] {
        0 => FrameBody::Bye,
        1 => {
            let s = std::str::from_utf8(&payload[1..]).ok()?;
            FrameBody::Text(s.to_string())
        }
        _ => return None,
    };
    Some(PlainFrame { seq, body })
}

/// CRC-32 (IEEE 802.3 polynomial, reflected) — used to integrity-protect the
/// beacon payload. Not a security primitive; just a packet-level sanity check.
fn crc32(buf: &[u8]) -> u32 {
    let mut table = [0u32; 256];
    for n in 0..256u32 {
        let mut c = n;
        for _ in 0..8 {
            c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
        }
        table[n as usize] = c;
    }
    let mut crc = 0xFFFF_FFFFu32;
    for &b in buf {
        crc = (crc >> 8) ^ table[((crc ^ b as u32) & 0xFF) as usize];
    }
    !crc
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn beacon_roundtrip() {
        let b = Beacon {
            peer_id: [7u8; 16],
            public_key: [9u8; 32],
            tcp_port: 4242,
            name: "alice".into(),
        };
        let enc = encode_beacon(&b).unwrap();
        let dec = decode_beacon(&enc).unwrap();
        assert_eq!(b, dec);
    }

    #[test]
    fn beacon_rejects_bad_crc() {
        let b = Beacon {
            peer_id: [1u8; 16],
            public_key: [2u8; 32],
            tcp_port: 1,
            name: "x".into(),
        };
        let mut enc = encode_beacon(&b).unwrap();
        let last = enc.len() - 1;
        enc[last] ^= 0x01;
        assert!(decode_beacon(&enc).is_none());
    }

    #[test]
    fn beacon_rejects_bad_magic() {
        let mut b = encode_beacon(&Beacon {
            peer_id: [0u8; 16],
            public_key: [0u8; 32],
            tcp_port: 1,
            name: "x".into(),
        })
        .unwrap();
        b[0] = 0;
        assert!(decode_beacon(&b).is_none());
    }

    #[test]
    fn frame_roundtrip_text() {
        let buf = encode_plain_frame(42, &FrameBody::Text("hello".into()));
        let dec = decode_plain_frame(&buf).unwrap();
        assert_eq!(dec.seq, 42);
        assert_eq!(dec.body, FrameBody::Text("hello".into()));
    }

    #[test]
    fn frame_roundtrip_bye() {
        let buf = encode_plain_frame(0, &FrameBody::Bye);
        let dec = decode_plain_frame(&buf).unwrap();
        assert_eq!(dec, PlainFrame { seq: 0, body: FrameBody::Bye });
    }

    #[test]
    fn fingerprint_is_16_hex() {
        let fp = fingerprint(&[0u8; 32]);
        assert_eq!(fp.len(), 16);
        assert!(fp.chars().all(|c| c.is_ascii_hexdigit()));
    }
}