//! Wire format for `ppexchanger`.
//!
//! Two distinct formats:
//!   1. UDP **beacon** — short announcement on the multicast group.
//!   2. TCP session frames — length-prefixed, AEAD-encrypted payloads.
//!
//! All multi-byte integers are big-endian. No external serialization crate is
//! used; the encoders/decoders are hand-written because the schema is small,
//! stable, and the zero-dep ethos is worth preserving for the protocol layer.

use crate::crypto::sha256;
use rand_core::{OsRng, RngCore};
use std::convert::TryInto;

/// UDP beacon magic: `"LANC"` in ASCII.
pub const BEACON_MAGIC: [u8; 4] = [0x4C, 0x41, 0x4E, 0x43];
pub const BEACON_VERSION: u8 = 2;
pub const BEACON_MSG_TYPE: u8 = 1;

/// TCP connection request magic: sent after PPXP probe to indicate connection request.
/// If listener sees this, it should prompt user for accept/deny instead of auto-accept.
pub const PPXP_REQUEST: [u8; 4] = [0x50, 0x50, 0x58, 0x50]; // "PPXP" in ASCII
/// TCP connection response magic: sent after PPXP_REQUEST to accept the connection.
pub const PPXP_ACCEPT: [u8; 4] = [0x41, 0x43, 0x43, 0x45]; // "ACCE"
/// TCP connection response magic: sent after PPXP_REQUEST to deny the connection.
pub const PPXP_DENY: [u8; 4] = [0x44, 0x45, 0x4E, 0x59]; // "DENY"

/// Hard cap on beacon body — keeps malformed packets from chewing CPU.
pub const BEACON_MAX_BYTES: usize = 256;

/// Maximum encrypted TCP frame payload (after AEAD tag is added).
pub const FRAME_MAX_PAYLOAD: usize = 64 * 1024;

/// Cap on the data portion of a single `FileChunk`. The whole chunk
/// (tag + id + offset + data_len + data) must fit under
/// `FRAME_MAX_PAYLOAD` minus the outer `[seq,len]` header. With 32 KiB
/// data the chunk is `1 + 16 + 8 + 4 + 32_768 = 32_797` bytes, leaving
/// ~32 KiB of headroom under the 64 KiB plaintext cap.
pub const FILE_CHUNK_MAX_DATA: usize = 32 * 1024;

/// Tag byte for `FrameBody::FileOffer`.
pub const TAG_FILE_OFFER: u8 = 2;
/// Tag byte for `FrameBody::FileAccept`.
pub const TAG_FILE_ACCEPT: u8 = 3;
/// Tag byte for `FrameBody::FileReject`.
pub const TAG_FILE_REJECT: u8 = 4;
/// Tag byte for `FrameBody::FileChunk`.
pub const TAG_FILE_CHUNK: u8 = 5;
/// Tag byte for `FrameBody::FileDone`.
pub const TAG_FILE_DONE: u8 = 6;
/// Tag byte for the encrypted identity greeting exchanged after handshake.
pub const TAG_HELLO: u8 = 7;

/// Decoded contents of a UDP beacon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Beacon {
    pub peer_id: [u8; 16],
    pub public_key: [u8; 32],
    pub tcp_port: u16,
    pub name: String,
    /// Machine hostname for identification (may be empty for v1 beacons)
    pub hostname: String,
    /// Ephemeral UDP port that accepts reverse-connect requests. `0` means
    /// an older peer which does not support the fallback.
    pub control_port: u16,
}

/// 16-byte random transfer identifier. Generated sender-side with
/// `FileId::random()` and used to correlate `Offer`/`Accept`/`Chunk`/
/// `Done` frames between sender and receiver.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FileId(pub [u8; 16]);

impl FileId {
    pub fn random() -> Self {
        let mut bytes = [0u8; 16];
        OsRng.fill_bytes(&mut bytes);
        FileId(bytes)
    }

    pub fn to_hex(self) -> String {
        let mut s = String::with_capacity(32);
        for b in self.0.iter() {
            s.push_str(&format!("{:02x}", b));
        }
        s
    }
}

/// Encode a beacon into bytes. Returns `None` if `name` is too long or `tcp_port == 0`.
pub fn encode_beacon(b: &Beacon) -> Option<Vec<u8>> {
    if b.name.is_empty() || b.name.len() > u16::MAX as usize {
        return None;
    }
    let name_bytes = b.name.as_bytes();
    let hostname_bytes = b.hostname.as_bytes();
    let mut out = Vec::with_capacity(
        4 + 1 + 1 + 2 + 16 + 32 + 2 + 2 + name_bytes.len() + 2 + hostname_bytes.len() + 2 + 4,
    );
    out.extend_from_slice(&BEACON_MAGIC);
    out.push(BEACON_VERSION);
    out.push(BEACON_MSG_TYPE);
    let payload_len = (16 + 32 + 2 + 2 + name_bytes.len() + 2 + hostname_bytes.len() + 2) as u16;
    out.extend_from_slice(&payload_len.to_be_bytes());
    out.extend_from_slice(&b.peer_id);
    out.extend_from_slice(&b.public_key);
    out.extend_from_slice(&b.tcp_port.to_be_bytes());
    out.extend_from_slice(&(name_bytes.len() as u16).to_be_bytes());
    out.extend_from_slice(name_bytes);
    out.extend_from_slice(&(hostname_bytes.len() as u16).to_be_bytes());
    out.extend_from_slice(hostname_bytes);
    out.extend_from_slice(&b.control_port.to_be_bytes());
    let crc = crc32(&out);
    out.extend_from_slice(&crc.to_be_bytes());
    Some(out)
}

/// Decode a beacon from bytes. Returns `None` on any structural or CRC failure.
/// Handles both v1 (no hostname) and v2 (with hostname) beacons.
pub fn decode_beacon(bytes: &[u8]) -> Option<Beacon> {
    if bytes.len() < 4 + 1 + 1 + 2 + 16 + 32 + 2 + 2 + 4 {
        return None;
    }
    if bytes.len() > BEACON_MAX_BYTES {
        return None;
    }
    if bytes[0..4] != BEACON_MAGIC {
        return None;
    }
    let version = bytes[4];
    let msg_type = bytes[5];
    // Accept v1 (version=1) or v2 (version=2) beacons
    if (version != 1 && version != 2) || msg_type != BEACON_MSG_TYPE {
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
    if p + name_len > total_len - 4 {
        return None;
    }
    let name = std::str::from_utf8(&bytes[p..p + name_len]).ok()?.to_string();
    if name.is_empty() {
        return None;
    }
    p += name_len;

    // v2 beacons have hostname after name, v1 beacons end here
    let hostname = if version >= 2 && p + 2 <= total_len - 4 {
        let hostname_len = u16::from_be_bytes(bytes[p..p + 2].try_into().ok()?) as usize;
        p += 2;
        if p + hostname_len > total_len - 4 {
            return None;
        }
        let hostname = std::str::from_utf8(&bytes[p..p + hostname_len])
            .ok()?
            .to_string();
        p += hostname_len;
        hostname
        // hostname can be empty string
    } else {
        String::new()
    };
    // v2.1 extension: two optional bytes after hostname. Older v2 peers
    // simply ignore this suffix; missing bytes mean no reverse fallback.
    let control_port = if p + 2 <= total_len - 4 {
        u16::from_be_bytes(bytes[p..p + 2].try_into().ok()?)
    } else {
        0
    };

    Some(Beacon {
        peer_id,
        public_key,
        tcp_port,
        name,
        hostname,
        control_port,
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
///
/// Wire layout is one tag byte followed by tag-specific payload bytes.
/// Tags:
///   0 = Bye
///   1 = Text
///   2 = FileOffer  `[id:16][name_len:2][name][size:8][mime_len:2][mime]`
///   3 = FileAccept `[id:16]`
///   4 = FileReject `[id:16]`
///   5 = FileChunk  `[id:16][offset:8][data_len:4][data]`
///   6 = FileDone   `[id:16]`
///   7 = Hello      `[name_len:2][name][hostname_len:2][hostname]`
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FrameBody {
    Hello { name: String, hostname: String },
    Text(String),
    Bye,
    FileOffer {
        id: FileId,
        name: String,
        size: u64,
        mime: Option<String>,
    },
    FileAccept {
        id: FileId,
    },
    FileReject {
        id: FileId,
    },
    FileChunk {
        id: FileId,
        offset: u64,
        data: Vec<u8>,
    },
    FileDone {
        id: FileId,
    },
}

/// Length-prefixed cleartext frame as it appears on the wire before encryption.
#[derive(Debug, PartialEq, Eq)]
pub struct PlainFrame {
    pub seq: u64,
    pub body: FrameBody,
}

/// Reason a cleartext frame could not be decoded. `UnknownTag` is
/// recoverable (the peer is running a newer/different ppexchanger that
/// uses tags we don't know); `Malformed` and `Overflow` are hard
/// errors — the frame is junk and the session should be torn down.
#[derive(Debug, PartialEq, Eq)]
pub enum DecodeError {
    Malformed,
    UnknownTag(u8),
    Overflow,
}

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DecodeError::Malformed => write!(f, "frame decode failed"),
            DecodeError::UnknownTag(t) => write!(f, "unknown frame tag {}", t),
            DecodeError::Overflow => write!(f, "frame length out of bounds"),
        }
    }
}

/// Encode `(seq, body)` into a cleartext byte buffer.
pub fn encode_plain_frame(seq: u64, body: &FrameBody) -> Vec<u8> {
    let payload = match body {
        FrameBody::Hello { name, hostname } => {
            let name_bytes = name.as_bytes();
            let hostname_bytes = hostname.as_bytes();
            let mut v = Vec::with_capacity(1 + 2 + name_bytes.len() + 2 + hostname_bytes.len());
            v.push(TAG_HELLO);
            v.extend_from_slice(&(name_bytes.len() as u16).to_be_bytes());
            v.extend_from_slice(name_bytes);
            v.extend_from_slice(&(hostname_bytes.len() as u16).to_be_bytes());
            v.extend_from_slice(hostname_bytes);
            v
        }
        FrameBody::Text(s) => {
            let mut v = Vec::with_capacity(1 + s.len());
            v.push(1u8);
            v.extend_from_slice(s.as_bytes());
            v
        }
        FrameBody::Bye => vec![0u8],
        FrameBody::FileOffer { id, name, size, mime } => {
            let name_bytes = name.as_bytes();
            // mime is optional; emit 0-length when None.
            let mime_bytes = mime.as_ref().map(|m| m.as_bytes()).unwrap_or(&[]);
            let cap = 1 + 16 + 2 + name_bytes.len() + 8 + 2 + mime_bytes.len();
            let mut v = Vec::with_capacity(cap);
            v.push(TAG_FILE_OFFER);
            v.extend_from_slice(&id.0);
            v.extend_from_slice(&(name_bytes.len() as u16).to_be_bytes());
            v.extend_from_slice(name_bytes);
            v.extend_from_slice(&size.to_be_bytes());
            v.extend_from_slice(&(mime_bytes.len() as u16).to_be_bytes());
            v.extend_from_slice(mime_bytes);
            v
        }
        FrameBody::FileAccept { id } => {
            let mut v = Vec::with_capacity(1 + 16);
            v.push(TAG_FILE_ACCEPT);
            v.extend_from_slice(&id.0);
            v
        }
        FrameBody::FileReject { id } => {
            let mut v = Vec::with_capacity(1 + 16);
            v.push(TAG_FILE_REJECT);
            v.extend_from_slice(&id.0);
            v
        }
        FrameBody::FileChunk { id, offset, data } => {
            let cap = 1 + 16 + 8 + 4 + data.len();
            let mut v = Vec::with_capacity(cap);
            v.push(TAG_FILE_CHUNK);
            v.extend_from_slice(&id.0);
            v.extend_from_slice(&offset.to_be_bytes());
            v.extend_from_slice(&(data.len() as u32).to_be_bytes());
            v.extend_from_slice(data);
            v
        }
        FrameBody::FileDone { id } => {
            let mut v = Vec::with_capacity(1 + 16);
            v.push(TAG_FILE_DONE);
            v.extend_from_slice(&id.0);
            v
        }
    };
    let mut out = Vec::with_capacity(FRAME_HEADER_LEN + payload.len());
    out.extend_from_slice(&seq.to_be_bytes());
    out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    out.extend_from_slice(&payload);
    out
}

/// Decode a cleartext frame buffer.
pub fn decode_plain_frame(buf: &[u8]) -> Result<PlainFrame, DecodeError> {
    if buf.len() < FRAME_HEADER_LEN {
        return Err(DecodeError::Malformed);
    }
    let seq = u64::from_be_bytes(buf[0..8].try_into().unwrap());
    let len = u32::from_be_bytes(buf[8..12].try_into().unwrap()) as usize;
    if buf.len() != FRAME_HEADER_LEN + len || len == 0 {
        return Err(DecodeError::Malformed);
    }
    if len > FRAME_MAX_PAYLOAD {
        return Err(DecodeError::Overflow);
    }
    let payload = &buf[FRAME_HEADER_LEN..];
    let body = match payload[0] {
        TAG_HELLO => decode_hello(&payload[1..])?,
        0 => FrameBody::Bye,
        1 => {
            let s = std::str::from_utf8(&payload[1..])
                .map_err(|_| DecodeError::Malformed)?;
            FrameBody::Text(s.to_string())
        }
        TAG_FILE_OFFER => decode_file_offer(&payload[1..])?,
        TAG_FILE_ACCEPT => decode_file_id_only(&payload[1..], TAG_FILE_ACCEPT)?,
        TAG_FILE_REJECT => decode_file_id_only(&payload[1..], TAG_FILE_REJECT)?,
        TAG_FILE_CHUNK => decode_file_chunk(&payload[1..])?,
        TAG_FILE_DONE => decode_file_id_only(&payload[1..], TAG_FILE_DONE)?,
        t => return Err(DecodeError::UnknownTag(t)),
    };
    Ok(PlainFrame { seq, body })
}

fn decode_hello(payload: &[u8]) -> Result<FrameBody, DecodeError> {
    if payload.len() < 4 {
        return Err(DecodeError::Malformed);
    }
    let name_len = u16::from_be_bytes(payload[..2].try_into().unwrap()) as usize;
    let name_end = 2 + name_len;
    if name_end + 2 > payload.len() {
        return Err(DecodeError::Malformed);
    }
    let name = std::str::from_utf8(&payload[2..name_end])
        .map_err(|_| DecodeError::Malformed)?
        .to_string();
    let host_len = u16::from_be_bytes(payload[name_end..name_end + 2].try_into().unwrap()) as usize;
    if name_end + 2 + host_len != payload.len() {
        return Err(DecodeError::Malformed);
    }
    let hostname = std::str::from_utf8(&payload[name_end + 2..])
        .map_err(|_| DecodeError::Malformed)?
        .to_string();
    Ok(FrameBody::Hello { name, hostname })
}

fn read_id(rest: &[u8]) -> Result<FileId, DecodeError> {
    if rest.len() < 16 {
        return Err(DecodeError::Malformed);
    }
    let mut id = [0u8; 16];
    id.copy_from_slice(&rest[..16]);
    Ok(FileId(id))
}

fn decode_file_id_only(payload: &[u8], tag: u8) -> Result<FrameBody, DecodeError> {
    if payload.len() != 16 {
        return Err(DecodeError::Malformed);
    }
    let id = read_id(payload)?;
    Ok(match tag {
        TAG_FILE_ACCEPT => FrameBody::FileAccept { id },
        TAG_FILE_REJECT => FrameBody::FileReject { id },
        TAG_FILE_DONE => FrameBody::FileDone { id },
        _ => return Err(DecodeError::Malformed),
    })
}

fn decode_file_offer(payload: &[u8]) -> Result<FrameBody, DecodeError> {
    // [id:16][name_len:2][name][size:8][mime_len:2][mime]
    if payload.len() < 16 + 2 + 8 + 2 {
        return Err(DecodeError::Malformed);
    }
    let id = read_id(&payload[..16])?;
    let mut p = 16;
    let name_len = u16::from_be_bytes(payload[p..p + 2].try_into().unwrap()) as usize;
    p += 2;
    if p + name_len + 8 + 2 > payload.len() {
        return Err(DecodeError::Malformed);
    }
    let name = std::str::from_utf8(&payload[p..p + name_len])
        .map_err(|_| DecodeError::Malformed)?
        .to_string();
    p += name_len;
    let size = u64::from_be_bytes(payload[p..p + 8].try_into().unwrap());
    p += 8;
    let mime_len = u16::from_be_bytes(payload[p..p + 2].try_into().unwrap()) as usize;
    p += 2;
    if p + mime_len > payload.len() {
        return Err(DecodeError::Malformed);
    }
    let mime = if mime_len == 0 {
        None
    } else {
        Some(
            std::str::from_utf8(&payload[p..p + mime_len])
                .map_err(|_| DecodeError::Malformed)?
                .to_string(),
        )
    };
    Ok(FrameBody::FileOffer { id, name, size, mime })
}

fn decode_file_chunk(payload: &[u8]) -> Result<FrameBody, DecodeError> {
    // [id:16][offset:8][data_len:4][data]
    if payload.len() < 16 + 8 + 4 {
        return Err(DecodeError::Malformed);
    }
    let id = read_id(&payload[..16])?;
    let offset = u64::from_be_bytes(payload[16..24].try_into().unwrap());
    let data_len = u32::from_be_bytes(payload[24..28].try_into().unwrap()) as usize;
    if data_len > FILE_CHUNK_MAX_DATA {
        return Err(DecodeError::Overflow);
    }
    if payload.len() != 16 + 8 + 4 + data_len {
        return Err(DecodeError::Malformed);
    }
    let data = payload[28..].to_vec();
    Ok(FrameBody::FileChunk { id, offset, data })
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
            hostname: "test-host".into(),
            control_port: 0,
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
            hostname: "test-host".into(),
            control_port: 0,
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
            hostname: "test-host".into(),
            control_port: 0,
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
    fn frame_roundtrip_hello() {
        let body = FrameBody::Hello {
            name: "alice".into(),
            hostname: "laptop".into(),
        };
        let buf = encode_plain_frame(3, &body);
        assert_eq!(decode_plain_frame(&buf).unwrap().body, body);
    }

    #[test]
    fn frame_roundtrip_bye() {
        let buf = encode_plain_frame(0, &FrameBody::Bye);
        let dec = decode_plain_frame(&buf).unwrap();
        assert_eq!(dec, PlainFrame { seq: 0, body: FrameBody::Bye });
    }

    #[test]
    fn frame_roundtrip_file_offer() {
        let id = FileId([0xAAu8; 16]);
        let body = FrameBody::FileOffer {
            id,
            name: "report.pdf".into(),
            size: 123456,
            mime: Some("application/pdf".into()),
        };
        let buf = encode_plain_frame(7, &body);
        let dec = decode_plain_frame(&buf).unwrap();
        assert_eq!(dec.seq, 7);
        assert_eq!(dec.body, body);
    }

    #[test]
    fn frame_roundtrip_file_offer_no_mime() {
        let id = FileId([1u8; 16]);
        let body = FrameBody::FileOffer {
            id,
            name: "blob".into(),
            size: 0,
            mime: None,
        };
        let buf = encode_plain_frame(0, &body);
        let dec = decode_plain_frame(&buf).unwrap();
        assert_eq!(dec.body, body);
    }

    #[test]
    fn frame_roundtrip_file_accept_reject_done() {
        let id = FileId([2u8; 16]);
        for body in [
            FrameBody::FileAccept { id },
            FrameBody::FileReject { id },
            FrameBody::FileDone { id },
        ] {
            let buf = encode_plain_frame(3, &body);
            let dec = decode_plain_frame(&buf).unwrap();
            assert_eq!(dec.body, body);
        }
    }

    #[test]
    fn frame_roundtrip_file_chunk() {
        let id = FileId([3u8; 16]);
        let data: Vec<u8> = (0..1024u32).map(|i| (i & 0xFF) as u8).collect();
        let body = FrameBody::FileChunk {
            id,
            offset: 1024,
            data: data.clone(),
        };
        let buf = encode_plain_frame(5, &body);
        let dec = decode_plain_frame(&buf).unwrap();
        assert_eq!(dec.body, body);
        if let FrameBody::FileChunk { data: d, .. } = dec.body {
            assert_eq!(d, data);
        }
    }

    #[test]
    fn decode_unknown_tag_returns_err() {
        // Build a syntactically valid frame whose tag byte is 99.
        let mut buf = encode_plain_frame(0, &FrameBody::Bye);
        // Replace tag byte (right after header) with 99.
        buf[FRAME_HEADER_LEN] = 99;
        let res = decode_plain_frame(&buf);
        assert_eq!(res, Err(DecodeError::UnknownTag(99)));
    }

    #[test]
    fn fingerprint_is_16_hex() {
        let fp = fingerprint(&[0u8; 32]);
        assert_eq!(fp.len(), 16);
        assert!(fp.chars().all(|c| c.is_ascii_hexdigit()));
    }
}
