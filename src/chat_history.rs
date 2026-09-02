//! Encrypted local chat-history persistence.
//!
//! History is intentionally kept separate from the network protocol. The
//! on-disk file is an authenticated ChaCha20-Poly1305 envelope whose key is
//! derived from the local identity secret, so copying the file alone does not
//! reveal message contents and rotating the identity naturally starts a new
//! history.

use crate::crypto::{hkdf_sha256, Aead, ChaCha20Poly1305, Key, KeyInit, Nonce, Payload};
use crate::events::PeerId;
use crate::tui::UiMessage;
use rand_core::{OsRng, RngCore};
use std::collections::VecDeque;
use std::io;
use std::path::Path;

const MAGIC: &[u8; 4] = b"PCH2";
const LEGACY_MAGIC: &[u8; 4] = b"PCH1";
const NONCE_LEN: usize = 12;
const TAG_LEN: usize = 16;
const MAX_MESSAGES: usize = 10_000;
const MAX_FIELD_LEN: usize = 1024 * 1024;
const MAX_PLAINTEXT_LEN: usize = 32 * 1024 * 1024;

/// Derive a storage-only key; the identity secret is never written to the
/// history file. Binding the peer id prevents accidental cross-identity reuse
/// if a file is copied between config directories.
fn derive_key(secret: &[u8; 32], peer_id: &PeerId) -> Key {
    let mut raw = [0u8; 32];
    hkdf_sha256(secret, peer_id, b"ppexchanger-chat-history-v1", &mut raw);
    *Key::from_slice(&raw)
}

/// Save the current ring as an encrypted envelope. A random nonce is used for
/// every write; the fixed magic is authenticated as associated data.
pub fn save(
    path: &Path,
    secret: &[u8; 32],
    peer_id: &PeerId,
    messages: &VecDeque<UiMessage>,
) -> io::Result<()> {
    let plaintext = encode(messages)?;
    let mut nonce_bytes = [0u8; NONCE_LEN];
    OsRng.fill_bytes(&mut nonce_bytes);
    let cipher = ChaCha20Poly1305::new(&derive_key(secret, peer_id));
    let ciphertext = cipher
        .encrypt(
            Nonce::from_slice(&nonce_bytes),
            Payload {
                msg: &plaintext,
                aad: MAGIC,
            },
        )
        .map_err(|_| io::Error::other("chat history encryption failed"))?;

    let mut envelope = Vec::with_capacity(MAGIC.len() + NONCE_LEN + ciphertext.len());
    envelope.extend_from_slice(MAGIC);
    envelope.extend_from_slice(&nonce_bytes);
    envelope.extend_from_slice(&ciphertext);
    std::fs::write(path, envelope)?;
    // Ciphertext is safe to expose cryptographically, but keeping the file
    // private avoids leaking message count/timing metadata to other users.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = std::fs::metadata(path)?.permissions();
        permissions.set_mode(0o600);
        std::fs::set_permissions(path, permissions)?;
    }
    Ok(())
}

/// Load and authenticate an encrypted history file. A missing file is a
/// normal first-run condition and returns `Ok(None)`.
pub fn load(
    path: &Path,
    secret: &[u8; 32],
    peer_id: &PeerId,
) -> io::Result<Option<VecDeque<UiMessage>>> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    if bytes.len() < MAGIC.len() + NONCE_LEN + TAG_LEN {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "chat history file is truncated",
        ));
    }
    let file_magic = &bytes[..MAGIC.len()];
    let has_pending = if file_magic == MAGIC {
        true
    } else if file_magic == LEGACY_MAGIC {
        false
    } else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "chat history format mismatch",
        ));
    };
    let aad = if has_pending { MAGIC } else { LEGACY_MAGIC };
    if bytes.len() > MAGIC.len() + NONCE_LEN + TAG_LEN + MAX_PLAINTEXT_LEN {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "chat history is too large",
        ));
    }
    let nonce = Nonce::from_slice(&bytes[MAGIC.len()..MAGIC.len() + NONCE_LEN]);
    let cipher = ChaCha20Poly1305::new(&derive_key(secret, peer_id));
    let plaintext = cipher
        .decrypt(
            nonce,
            Payload {
                msg: &bytes[MAGIC.len() + NONCE_LEN..],
                aad,
            },
        )
        .map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "chat history authentication failed",
            )
        })?;
    if plaintext.len() > MAX_PLAINTEXT_LEN {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "chat history is too large",
        ));
    }
    decode(&plaintext, has_pending).map(Some)
}

fn encode(messages: &VecDeque<UiMessage>) -> io::Result<Vec<u8>> {
    if messages.len() > MAX_MESSAGES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "too many chat messages",
        ));
    }
    let mut out = Vec::new();
    out.extend_from_slice(&(messages.len() as u32).to_be_bytes());
    for message in messages {
        let name = message.from_name.as_bytes();
        let body = message.body.as_bytes();
        if name.len() > MAX_FIELD_LEN || body.len() > MAX_FIELD_LEN {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "chat message field is too large",
            ));
        }
        out.extend_from_slice(&message.from_peer);
        out.push(u8::from(message.outgoing));
        out.push(u8::from(message.pending));
        out.extend_from_slice(&message.ts_unix.to_be_bytes());
        out.extend_from_slice(&(name.len() as u32).to_be_bytes());
        out.extend_from_slice(&(body.len() as u32).to_be_bytes());
        out.extend_from_slice(name);
        out.extend_from_slice(body);
    }
    if out.len() > MAX_PLAINTEXT_LEN {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "chat history is too large",
        ));
    }
    Ok(out)
}

fn decode(bytes: &[u8], has_pending: bool) -> io::Result<VecDeque<UiMessage>> {
    let mut cursor = Cursor::new(bytes);
    let count = cursor.u32()? as usize;
    if count > MAX_MESSAGES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "chat history message count is invalid",
        ));
    }
    let mut messages = VecDeque::with_capacity(count);
    for _ in 0..count {
        let from_peer = cursor.array_16()?;
        let outgoing = match cursor.byte()? {
            0 => false,
            1 => true,
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "chat history outgoing flag is invalid",
                ));
            }
        };
        let pending = if has_pending {
            match cursor.byte()? {
                0 => false,
                1 => true,
                _ => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "chat history pending flag is invalid",
                    ));
                }
            }
        } else {
            false
        };
        let ts_unix = cursor.u64()?;
        let name_len = cursor.u32()? as usize;
        let body_len = cursor.u32()? as usize;
        if name_len > MAX_FIELD_LEN || body_len > MAX_FIELD_LEN {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "chat history field is too large",
            ));
        }
        let from_name = cursor.string(name_len)?;
        let body = cursor.string(body_len)?;
        messages.push_back(UiMessage {
            from_peer,
            from_name,
            body,
            outgoing,
            pending,
            ts_unix,
        });
    }
    if !cursor.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "chat history has trailing data",
        ));
    }
    Ok(messages)
}

struct Cursor<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> Cursor<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn take(&mut self, len: usize) -> io::Result<&'a [u8]> {
        let end = self.offset.checked_add(len).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "chat history length overflow")
        })?;
        if end > self.bytes.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "chat history record is truncated",
            ));
        }
        let value = &self.bytes[self.offset..end];
        self.offset = end;
        Ok(value)
    }

    fn byte(&mut self) -> io::Result<u8> {
        Ok(self.take(1)?[0])
    }

    fn u32(&mut self) -> io::Result<u32> {
        Ok(u32::from_be_bytes(self.take(4)?.try_into().unwrap()))
    }

    fn u64(&mut self) -> io::Result<u64> {
        Ok(u64::from_be_bytes(self.take(8)?.try_into().unwrap()))
    }

    fn array_16(&mut self) -> io::Result<[u8; 16]> {
        Ok(self.take(16)?.try_into().unwrap())
    }

    fn string(&mut self, len: usize) -> io::Result<String> {
        std::str::from_utf8(self.take(len)?)
            .map(str::to_owned)
            .map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidData, "chat history text is not utf-8")
            })
    }

    fn is_empty(&self) -> bool {
        self.offset == self.bytes.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(label: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "ppexchanger-history-{}-{}-{}",
            label,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    #[test]
    fn roundtrip_is_authenticated_and_encrypted() {
        let path = temp_path("roundtrip");
        let secret = [7u8; 32];
        let peer_id = [9u8; 16];
        let mut messages = VecDeque::new();
        messages.push_back(UiMessage {
            from_peer: peer_id,
            from_name: "alice".into(),
            body: "keep this private".into(),
            outgoing: true,
            pending: true,
            ts_unix: 42,
        });
        save(&path, &secret, &peer_id, &messages).unwrap();
        let raw = std::fs::read(&path).unwrap();
        assert!(!raw.windows(17).any(|w| w == b"keep this private"));
        let loaded = load(&path, &secret, &peer_id).unwrap().unwrap();
        assert_eq!(loaded.len(), 1);
        assert!(loaded.front().unwrap().pending);
        assert!(load(&path, &[8u8; 32], &peer_id).is_err());
        let _ = std::fs::remove_file(path);
    }
}
