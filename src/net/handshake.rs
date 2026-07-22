//! Noise_XX-lite handshake over TCP.
//!
//! Canonical Noise XX pattern, three messages, mutual authentication of static
//! X25519 keys, no pre-shared secret. Adapted to our wire format by
//! encrypting the static keys in their respective messages (which is what the
//! Noise spec does too).
//!
//! ## Wire layout
//! Every message: `[u32 len][u32 version=1][payload...]`.
//!
//! msg1 payload (I → R): `e` (32 bytes, cleartext).
//! msg2 payload (R → I): `e` (32) || ENC(s) (48) || MAC (16)
//!     where ENC(s) = ChaCha20Poly1305(key=handshake_key, nonce=0, aad=h, msg=s)
//!     (encrypts the responder's static key under a key derived from h after
//!      mixing ee). MAC is over h_after_mixing_es.
//! msg3 payload (I → R): `ENC(s)` (48) || MAC (16)
//!     where ENC(s) = ChaCha20Poly1305 encrypts the initiator's static key.
//!
//! ## Token processing (canonical XX, §5.3 of the Noise spec)
//!
//!   I → R: msg1 = e
//!     I: h = HASH(h || e), then send e.
//!     R: h = HASH(h || e).  [no DH yet — at msg1 both sides only MixHash]
//!
//!   R → I: msg2 = e, ENC(s), MAC
//!     R: h = HASH(h || e)
//!        MixKey(ee = DH(re, ie))
//!        ENC(s) = encrypt(s under handshake-key derived from ck, aad=h)
//!        h = HASH(h || ENC(s))
//!        MixKey(es = DH(s, ie))           ← responder's static × initiator's e
//!        MAC = encrypt(empty under key derived from new ck, aad=h)
//!        send e || ENC(s) || MAC
//!     I: read e, h = HASH(h || e)
//!        read ENC(s), decrypt → s, h = HASH(h || ENC(s))
//!        MixKey(ee = DH(e, re))
//!        MixKey(es = DH(s, rs))
//!        read MAC, verify
//!
//!   I → R: msg3 = ENC(s), MAC
//!     I: ENC(s) = encrypt(initiator's s under key derived from ck, aad=h)
//!        h = HASH(h || ENC(s))
//!        MixKey(se = DH(s, re))           ← initiator's static × responder's e
//!        MAC = encrypt(empty under new key, aad=h)
//!        send ENC(s) || MAC
//!     R: read ENC(s), decrypt → s, h = HASH(h || ENC(s))
//!        MixKey(se = DH(e, rs))
//!        read MAC, verify
//!
//!   transport keys = HKDF(ck, h, "lanchat-session", 64) → (send, recv)

use crate::crypto::{
    derive_session_keys, hkdf_sha256, Aead, ChaCha20Poly1305, Key, KeyInit, Keypair, Nonce,
};
use crate::protocol::fingerprint;
use chacha20poly1305::aead::Payload;
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use x25519_dalek::PublicKey as X25519Public;

const PROTOCOL_NAME: &[u8] = b"Noise_XX_lanchat_25519_ChaChaPoly_SHA256";
const HANDSHAKE_VERSION: u32 = 1;
const AEAD_TAG: usize = 16;
const STATIC_LEN: usize = 32;

/// Result of a successful handshake.
#[derive(Clone)]
pub struct HandshakeResult {
    pub send_key: Key,
    pub recv_key: Key,
    pub remote_static: [u8; 32],
    pub remote_fingerprint: String,
    pub is_initiator: bool,
}

// ─── wire helpers ────────────────────────────────────────────────────────────

fn read_exact(r: &mut impl Read, n: usize) -> std::io::Result<Vec<u8>> {
    let mut buf = vec![0u8; n];
    r.read_exact(&mut buf)?;
    Ok(buf)
}

fn send_msg(w: &mut impl Write, payload: &[u8]) -> std::io::Result<()> {
    let len = (4 + payload.len()) as u32;
    w.write_all(&len.to_be_bytes())?;
    w.write_all(&HANDSHAKE_VERSION.to_be_bytes())?;
    w.write_all(payload)
}

fn recv_msg(r: &mut impl Read) -> std::io::Result<Vec<u8>> {
    let hdr = read_exact(r, 8)?;
    let len = u32::from_be_bytes(hdr[0..4].try_into().unwrap()) as usize;
    if len < 4 || len > 4096 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "handshake message out of bounds",
        ));
    }
    let ver = u32::from_be_bytes(hdr[4..8].try_into().unwrap());
    if ver != HANDSHAKE_VERSION {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "handshake version mismatch",
        ));
    }
    read_exact(r, len - 4)
}

// ─── symmetric-state helpers ────────────────────────────────────────────────

fn initialize_symmetric_state() -> ([u8; 32], Sha256) {
    let mut h = Sha256::new();
    h.update(PROTOCOL_NAME);
    let h_full = h.clone().finalize();
    let mut ck = [0u8; 32];
    hkdf_sha256(&h_full, &[], b"", &mut ck);
    (ck, h)
}

fn mix_key(ck: &mut [u8; 32], h: &mut Sha256, dh_output: [u8; 32]) {
    h.update(dh_output);
    let mut new_ck = [0u8; 32];
    hkdf_sha256(&dh_output, ck, b"", &mut new_ck);
    *ck = new_ck;
}

fn mix_hash(h: &mut Sha256, data: &[u8]) {
    h.update(data);
}

fn handshake_key(ck: &[u8; 32]) -> Key {
    let mut raw = [0u8; 32];
    hkdf_sha256(ck, &[], b"handshake-key", &mut raw);
    *Key::from_slice(&raw)
}

/// Encrypt `plaintext` (≤ 32 bytes for our usage) under handshake-key(ck),
/// nonce = 0, AAD = SHA256(h). Returns ciphertext || 16-byte Poly1305 tag.
fn encrypt_with_ad(ck: &[u8; 32], h: &Sha256, plaintext: &[u8]) -> Vec<u8> {
    let cipher = ChaCha20Poly1305::new(&handshake_key(ck));
    let nonce = Nonce::from_slice(&[0u8; 12]);
    let aad = h.clone().finalize().to_vec();
    cipher
        .encrypt(nonce, Payload { msg: plaintext, aad: &aad })
        .expect("handshake AEAD encrypt cannot fail with empty AAD/short plaintext")
}

fn decrypt_with_ad(ck: &[u8; 32], h: &Sha256, ciphertext: &[u8]) -> std::io::Result<Vec<u8>> {
    let cipher = ChaCha20Poly1305::new(&handshake_key(ck));
    let nonce = Nonce::from_slice(&[0u8; 12]);
    let aad = h.clone().finalize().to_vec();
    cipher
        .decrypt(nonce, Payload { msg: ciphertext, aad: &aad })
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidData, "handshake MAC failed"))
}

// ─── initiator ───────────────────────────────────────────────────────────────

pub fn run_initiator<S: Read + Write>(
    stream: &mut S,
    static_kp: &Keypair,
) -> std::io::Result<HandshakeResult> {
    let (mut ck, mut h) = initialize_symmetric_state();

    // msg1 -> e
    let e = Keypair::generate();
    mix_hash(&mut h, &e.public_bytes());
    send_msg(stream, &e.public_bytes())?;

    // msg2 <- e || ENC(s) || MAC
    let payload = recv_msg(stream)?;
    let expected_len = 32 + STATIC_LEN + AEAD_TAG + AEAD_TAG;
    if payload.len() != expected_len {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "msg2 wrong size",
        ));
    }
    let re_bytes: [u8; 32] = payload[0..32].try_into().unwrap();
    let re = X25519Public::from(re_bytes);
    mix_hash(&mut h, &re_bytes);
    // MixKey(ee)
    mix_key(
        &mut ck,
        &mut h,
        e.secret.diffie_hellman(&re).to_bytes(),
    );
    // Decrypt ENC(s)
    let enc_s = &payload[32..32 + STATIC_LEN + AEAD_TAG];
    let rs_bytes_vec = decrypt_with_ad(&ck, &h, enc_s)?;
    if rs_bytes_vec.len() != STATIC_LEN {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "decrypted static key wrong size",
        ));
    }
    let rs_bytes: [u8; 32] = rs_bytes_vec[..].try_into().unwrap();
    mix_hash(&mut h, enc_s);
    let rs = X25519Public::from(rs_bytes);
    // MixKey(es) — both sides compute the same shared secret s_resp * e_init.
    // Initiator knows e_init (own ephemeral) and rs (just decrypted);
    // so use e_init * rs.
    mix_key(
        &mut ck,
        &mut h,
        e.secret.diffie_hellman(&rs).to_bytes(),
    );
    // Verify MAC over the new h
    let mac = &payload[32 + STATIC_LEN + AEAD_TAG..];
    decrypt_with_ad(&ck, &h, mac)?;

    // msg3 -> ENC(s) || MAC
    let enc_s_init = encrypt_with_ad(&ck, &h, &static_kp.public_bytes());
    mix_hash(&mut h, &enc_s_init);
    // MixKey(se = s_init * e_resp)
    mix_key(
        &mut ck,
        &mut h,
        static_kp.secret.diffie_hellman(&re).to_bytes(),
    );
    let mac = encrypt_with_ad(&ck, &h, &[]);
    let mut msg3 = Vec::with_capacity(STATIC_LEN + AEAD_TAG + AEAD_TAG);
    msg3.extend_from_slice(&enc_s_init);
    msg3.extend_from_slice(&mac);
    send_msg(stream, &msg3)?;

    let (send_key, recv_key) = derive_session_keys(&ck, &h.clone().finalize());
    Ok(HandshakeResult {
        send_key,
        recv_key,
        remote_static: rs_bytes,
        remote_fingerprint: fingerprint(&rs_bytes),
        is_initiator: true,
    })
}

// ─── responder ───────────────────────────────────────────────────────────────

pub fn run_responder<S: Read + Write>(
    stream: &mut S,
    static_kp: &Keypair,
) -> std::io::Result<HandshakeResult> {
    let (mut ck, mut h) = initialize_symmetric_state();

    // msg1 <- e
    let payload = recv_msg(stream)?;
    if payload.len() != 32 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "msg1 wrong size",
        ));
    }
    let ie_bytes: [u8; 32] = payload[..32].try_into().unwrap();
    let ie = X25519Public::from(ie_bytes);
    mix_hash(&mut h, &ie_bytes);

    // msg2 -> e || ENC(s) || MAC
    let e = Keypair::generate();
    mix_hash(&mut h, &e.public_bytes());
    // MixKey(ee = DH(re, ie))
    mix_key(
        &mut ck,
        &mut h,
        e.secret.diffie_hellman(&ie).to_bytes(),
    );
    // Encrypt responder's static
    let enc_s_resp = encrypt_with_ad(&ck, &h, &static_kp.public_bytes());
    mix_hash(&mut h, &enc_s_resp);
    // MixKey(es = DH(s_resp, ie))
    mix_key(
        &mut ck,
        &mut h,
        static_kp.secret.diffie_hellman(&ie).to_bytes(),
    );
    // MAC over h
    let mac = encrypt_with_ad(&ck, &h, &[]);
    let mut msg2 = Vec::with_capacity(32 + STATIC_LEN + AEAD_TAG + AEAD_TAG);
    msg2.extend_from_slice(&e.public_bytes());
    msg2.extend_from_slice(&enc_s_resp);
    msg2.extend_from_slice(&mac);
    send_msg(stream, &msg2)?;

    // msg3 <- ENC(s) || MAC
    let payload = recv_msg(stream)?;
    let expected_len = STATIC_LEN + AEAD_TAG + AEAD_TAG;
    if payload.len() != expected_len {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "msg3 wrong size",
        ));
    }
    let enc_s_init = &payload[..STATIC_LEN + AEAD_TAG];
    let mac = &payload[STATIC_LEN + AEAD_TAG..];
    let is_bytes_vec = decrypt_with_ad(&ck, &h, enc_s_init)?;
    if is_bytes_vec.len() != STATIC_LEN {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "decrypted static key wrong size",
        ));
    }
    let is_bytes: [u8; 32] = is_bytes_vec[..].try_into().unwrap();
    mix_hash(&mut h, enc_s_init);
    // MixKey(se = DH(e_resp, s_init))
    mix_key(
        &mut ck,
        &mut h,
        e.secret.diffie_hellman(&X25519Public::from(is_bytes)).to_bytes(),
    );
    // Verify MAC
    decrypt_with_ad(&ck, &h, mac)?;

    let (recv_key, send_key) = derive_session_keys(&ck, &h.clone().finalize());
    Ok(HandshakeResult {
        send_key,
        recv_key,
        remote_static: is_bytes,
        remote_fingerprint: fingerprint(&is_bytes),
        is_initiator: false,
    })
}
