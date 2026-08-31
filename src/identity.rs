//! Local long-term identity: random `peer_id` + static X25519 secret + name.
//! Persisted at `config::identity_path()` in a simple binary format.
//!
//! Binary format:
//!   [4 magic "LID1"] [16 peer_id] [32 secret] [16 name_len] [name UTF-8]

use crate::config::identity_path;
use crate::crypto::Keypair;
use rand_core::{OsRng, RngCore};
use std::io;
use std::path::Path;

const MAGIC: &[u8; 4] = b"LID1";
const NAME_MAX: usize = 256;

pub struct Identity {
    pub peer_id: [u8; 16],
    pub keypair: Keypair,
    pub name: String,
    /// Machine hostname for discovery identification.
    /// Loaded from config if set, otherwise falls back to system hostname.
    pub hostname: String,
}

impl Identity {
    /// Accessor for the raw static secret bytes. Exposed so callers can
    /// persist or display the keypair without exposing `keypair.secret`.
    pub fn secret_bytes(&self) -> [u8; 32] {
        self.keypair.secret.to_bytes()
    }
}

pub fn load_or_create(name_override: Option<String>) -> io::Result<Identity> {
    let path = identity_path()?;
    if Path::new(&path).exists() {
        let mut id = load(&path)?;
        if let Some(n) = name_override {
            id.name = n;
            save(&path, &id)?;
        }
        Ok(id)
    } else {
        let id = fresh(name_override)?;
        save(&path, &id)?;
        Ok(id)
    }
}

/// Persist a new local display name without rotating the peer identity or
/// touching its key material. Used by the settings panel.
pub fn update_name(name: &str) -> io::Result<()> {
    if name.trim().is_empty() || name.len() > NAME_MAX {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "name must be between 1 and 256 bytes",
        ));
    }
    let path = identity_path()?;
    let mut id = load(&path)?;
    id.name = name.trim().to_string();
    save(&path, &id)
}

fn fresh(name_override: Option<String>) -> io::Result<Identity> {
    let mut pid = [0u8; 16];
    OsRng.fill_bytes(&mut pid);
    let mut secret_bytes = [0u8; 32];
    OsRng.fill_bytes(&mut secret_bytes);
    let kp = Keypair::from_bytes(secret_bytes);
    let name = name_override.unwrap_or_else(|| "anon".to_string());
    if name.is_empty() || name.len() > NAME_MAX {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "name length out of range",
        ));
    }
    // Get system hostname for discovery identification
    let hostname = hostname::get()
        .map(|h| h.to_string_lossy().to_string())
        .unwrap_or_default();
    Ok(Identity {
        peer_id: pid,
        keypair: kp,
        name,
        hostname,
    })
}

fn load(path: &Path) -> io::Result<Identity> {
    let bytes = std::fs::read(path)?;
    if bytes.len() < 4 + 16 + 32 + 2 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "identity file too short",
        ));
    }
    if &bytes[..4] != MAGIC {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "identity magic mismatch",
        ));
    }
    let mut pid = [0u8; 16];
    pid.copy_from_slice(&bytes[4..20]);
    let mut secret = [0u8; 32];
    secret.copy_from_slice(&bytes[20..52]);
    let name_len = u16::from_le_bytes([bytes[52], bytes[53]]) as usize;
    let mut pos = 54;
    if pos + name_len > bytes.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "name length mismatch",
        ));
    }
    let name = std::str::from_utf8(&bytes[pos..pos + name_len])
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "identity name not utf-8"))?
        .to_string();
    pos += name_len;

    // hostname is optional - try to read it if present (v2 format)
    let hostname = if pos + 2 <= bytes.len() {
        let hostname_len = u16::from_le_bytes([bytes[pos], bytes[pos + 1]]) as usize;
        pos += 2;
        if pos + hostname_len <= bytes.len() && hostname_len > 0 {
            std::str::from_utf8(&bytes[pos..pos + hostname_len])
                .map(|h| h.to_string())
                .unwrap_or_default()
        } else {
            String::new()
        }
    } else {
        String::new()
    };

    Ok(Identity {
        peer_id: pid,
        keypair: Keypair::from_bytes(secret),
        name,
        hostname,
    })
}

fn save(path: &Path, id: &Identity) -> io::Result<()> {
    let secret = id.secret_bytes();
    let hostname_bytes = id.hostname.as_bytes();
    let mut buf = Vec::with_capacity(
        4 + 16 + 32 + 2 + id.name.len() + 2 + hostname_bytes.len(),
    );
    buf.extend_from_slice(MAGIC);
    buf.extend_from_slice(&id.peer_id);
    buf.extend_from_slice(&secret);
    let name_len = id.name.len() as u16;
    buf.extend_from_slice(&name_len.to_le_bytes());
    buf.extend_from_slice(id.name.as_bytes());
    // Include hostname in v2 format
    let hostname_len = id.hostname.len() as u16;
    buf.extend_from_slice(&hostname_len.to_le_bytes());
    buf.extend_from_slice(hostname_bytes);
    std::fs::write(path, &buf)
}
