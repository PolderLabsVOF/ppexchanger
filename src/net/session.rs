//! Encrypted framed message stream on top of an arbitrary `Read+Write` transport.
//!
//! Wire layout per frame (after the handshake):
//!   [u32 ciphertext_len] [ ciphertext || 16-byte Poly1305 tag ]
//!
//! Ciphertext is `ChaCha20-Poly1305-Encrypt(plaintext)` where the plaintext is
//! the cleartext frame produced by `protocol::encode_plain_frame`, and the
//! nonce is derived from the per-direction session key + monotonic sequence
//! counter via `crypto::derive_nonce`. Reordering / replay of old sequences is
//! rejected by `recv`.
//!
//! Concurrent send + recv on a single `Session<S>` is handled in the
//! application layer: each connection runs in a dedicated thread that owns
//! the whole `Session`. The thread loops on `recv` with a short timeout and
//! between iterations drains outbound frames from an `mpsc::Receiver`
//! registered for that peer. Outbound sends go through `Action::SendText`,
//! which the action thread forwards to the registered sender handle.

use crate::crypto::{derive_nonce, Aead, ChaCha20Poly1305, Key, KeyInit, Nonce, Payload};
use crate::protocol::{decode_plain_frame, encode_plain_frame, DecodeError, FrameBody, PlainFrame};
use std::io::{Read, Write};
use x25519_dalek::PublicKey;

/// Direction-specific message stream. Both sides of a TCP session hold one of
/// these; `send_key`/`recv_key` come from the handshake result.
pub struct Session<S> {
    stream: S,
    send_key: Key,
    recv_key: Key,
    send_seq: u64,
    recv_seq: u64,
    /// Cached remote static pubkey for display/identify; not used by crypto.
    pub remote_static: [u8; 32],
}

impl<S: Read + Write> Session<S> {
    pub fn new(stream: S, send_key: Key, recv_key: Key, remote_static: [u8; 32]) -> Self {
        Self {
            stream,
            send_key,
            recv_key,
            send_seq: 0,
            recv_seq: 0,
            remote_static,
        }
    }

    pub fn into_inner(self) -> S {
        self.stream
    }

    pub fn remote_pubkey(&self) -> PublicKey {
        PublicKey::from(self.remote_static)
    }

    /// Encrypt and send one message body.
    pub fn send(&mut self, body: &FrameBody) -> std::io::Result<()> {
        let seq = self.send_seq;
        let plaintext = encode_plain_frame(seq, body);
        let cipher = ChaCha20Poly1305::new(&self.send_key);
        let nonce_arr = derive_nonce(self.send_key.as_slice().try_into().unwrap(), seq);
        let nonce = Nonce::from_slice(&nonce_arr);
        let ct = cipher
            .encrypt(nonce, Payload { msg: &plaintext, aad: &[] })
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::Other, "AEAD encrypt failed"))?;
        let len = ct.len() as u32;
        self.stream.write_all(&len.to_be_bytes())?;
        self.stream.write_all(&ct)?;
        self.send_seq = self.send_seq.wrapping_add(1);
        Ok(())
    }

    /// Receive and decrypt one message body, validating the embedded sequence
    /// matches the expected monotonic `recv_seq`.
    pub fn recv(&mut self) -> std::io::Result<PlainFrame> {
        let mut len_buf = [0u8; 4];
        self.stream.read_exact(&mut len_buf)?;
        let len = u32::from_be_bytes(len_buf) as usize;
        if len == 0 || len > 64 * 1024 + 16 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "frame length out of bounds",
            ));
        }
        let mut ct = vec![0u8; len];
        self.stream.read_exact(&mut ct)?;

        let expected_seq = self.recv_seq;
        let cipher = ChaCha20Poly1305::new(&self.recv_key);
        let nonce_arr = derive_nonce(self.recv_key.as_slice().try_into().unwrap(), expected_seq);
        let nonce = Nonce::from_slice(&nonce_arr);
        let plaintext = cipher
            .decrypt(nonce, Payload { msg: &ct, aad: &[] })
            .map_err(|_| {
                std::io::Error::new(std::io::ErrorKind::InvalidData, "AEAD decrypt failed")
            })?;
        let frame = decode_plain_frame(&plaintext).map_err(|e| match e {
            DecodeError::Overflow => std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "frame length out of bounds",
            ),
            // Malformed is a hard error — tear the session down.
            // UnknownTag is recoverable; today we also tear down (since
            // there are no unknown tags yet), but the variant gives
            // callers a hook to keep the session alive in future.
            DecodeError::Malformed | DecodeError::UnknownTag(_) => {
                std::io::Error::new(std::io::ErrorKind::InvalidData, "frame decode failed")
            }
        })?;
        if frame.seq != expected_seq {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "replay or reordered frame",
            ));
        }
        self.recv_seq = self.recv_seq.wrapping_add(1);
        Ok(frame)
    }

    /// Test/integration hook: write arbitrary bytes onto the underlying stream
    /// without AEAD framing. Used by the tamper-detection test to inject
    /// deliberately invalid ciphertext and confirm `recv` rejects it.
    #[doc(hidden)]
    pub fn write_raw(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        self.stream.write_all(bytes)
    }
}

/// Concrete specialization: TcpStream supports `set_read_timeout`, so the
/// generic `try_recv` (blocking) can be overridden to poll briefly.
impl Session<std::net::TcpStream> {
    /// Non-blocking poll for one inbound frame. Returns `Ok(None)` if no
    /// frame arrives within ~50ms — the connection thread uses this to
    /// interleave outbound sends with inbound receives.
    pub fn try_recv(&mut self) -> std::io::Result<Option<PlainFrame>> {
        use std::time::Duration;
        self.stream.set_read_timeout(Some(Duration::from_millis(50)))?;
        let result = self.recv();
        // Reset to blocking so a long-lived idle connection doesn't hang.
        self.stream.set_read_timeout(None)?;
        match result {
            Ok(f) => Ok(Some(f)),
            Err(e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut =>
            {
                Ok(None)
            }
            Err(e) => Err(e),
        }
    }
}
