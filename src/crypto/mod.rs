//! Cryptographic primitives, all via audited crates.
//!
//! - `x25519-dalek`        — X25519 ECDH (key exchange)
//! - `chacha20poly1305`    — ChaCha20-Poly1305 AEAD (transport encryption)
//! - `sha2` + `hkdf`       — HKDF-SHA256 (key derivation in the handshake and for nonces)
//!
//! This module is a facade: protocol-level helpers (HKDF info-string constants,
//! AEAD nonce derivation, the Noise_XX-lite mix function) live here so the
//! handshake and session code can stay clean.

pub use chacha20poly1305::{
    aead::{Aead, KeyInit, Payload},
    ChaCha20Poly1305, Key, Nonce,
};
pub use hkdf::Hkdf;
pub use sha2::Sha256;
pub use x25519_dalek::{PublicKey as X25519Public, StaticSecret as X25519Secret};

use chacha20poly1305::aead::AeadCore;
use rand_core::{OsRng, RngCore};

/// 32-byte X25519 keypair (private + public).
pub struct Keypair {
    pub secret: X25519Secret,
    pub public: X25519Public,
}

impl Keypair {
    pub fn generate() -> Self {
        let secret = X25519Secret::random_from_rng(OsRng);
        let public = X25519Public::from(&secret);
        Self { secret, public }
    }

    pub fn from_bytes(secret_bytes: [u8; 32]) -> Self {
        let secret = X25519Secret::from(secret_bytes);
        let public = X25519Public::from(&secret);
        Self { secret, public }
    }

    pub fn public_bytes(&self) -> [u8; 32] {
        self.public.to_bytes()
    }
}

/// HKDF-SHA256 Extract+Expand in one shot.
pub fn hkdf_sha256(secret: &[u8], salt: &[u8], info: &[u8], out: &mut [u8]) {
    let hk = Hkdf::<Sha256>::new(Some(salt), secret);
    hk.expand(info, out)
        .expect("HKDF expand failed: output length too large");
}

/// Derive a 12-byte ChaCha20-Poly1305 nonce from a base key + 8-byte seq counter.
/// `HKDF-Expand(key, "lanchat-nonce" || seq_be, 12)`.
pub fn derive_nonce(base_key: &[u8; 32], seq: u64) -> [u8; 12] {
    let mut info = Vec::with_capacity(8 + 13);
    info.extend_from_slice(b"lanchat-nonce");
    info.extend_from_slice(&seq.to_be_bytes());
    let mut nonce = [0u8; 12];
    hkdf_sha256(base_key, &[], &info, &mut nonce);
    nonce
}

/// 16-byte Poly1305 key derived from `key`.
pub fn derive_poly_key(key: &[u8; 32]) -> [u8; 32] {
    let mut out = [0u8; 32];
    hkdf_sha256(key, &[], b"lanchat-poly1305", &mut out);
    out
}

/// 32-byte session send/recv key.
pub fn derive_session_keys(secret: &[u8], salt: &[u8]) -> (Key, Key) {
    let mut buf = [0u8; 64];
    hkdf_sha256(secret, salt, b"lanchat-session", &mut buf);
    let (send, recv) = buf.split_at(32);
    (*Key::from_slice(send), *Key::from_slice(recv))
}

/// Fresh 16-byte random nonce for the handshake (NEVER reused for AEAD).
pub fn random_handshake_nonce() -> [u8; 16] {
    let mut n = [0u8; 16];
    OsRng.fill_bytes(&mut n);
    n
}

/// SHA-256 of `data`, used for peer fingerprints and protocol MACs.
pub fn sha256(data: &[u8]) -> [u8; 32] {
    use sha2::Digest;
    let mut h = Sha256::new();
    h.update(data);
    h.finalize().into()
}