//! Contact list: persistent record of every peer we have ever seen, with
//! name, public key, last-seen address, and a trust flag.
//!
//! File format at `<config_dir>/contacts` (XDG `~/.config/lanchat/contacts`
//! on Linux/macOS, `%APPDATA%\lanchat\contacts` on Windows):
//!   [4 magic "LCDB"] [u8 version=1] [u32 count]
//!   for each contact:
//!     [16 peer_id] [u16 name_len] [name UTF-8] [32 pubkey]
//!     [1 has_addr] if has_addr: [u16 port_be] [4 ip_be]
//!     [8 last_seen_unix_be] [1 trusted]

use crate::config::contacts_path;
use std::io;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};

const MAGIC: &[u8; 4] = b"LCDB";
const VERSION: u8 = 1;
const NAME_MAX: usize = 256;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Contact {
    pub peer_id: [u8; 16],
    pub name: String,
    pub public_key: [u8; 32],
    pub last_addr: Option<SocketAddr>,
    pub last_seen_unix: u64,
    pub trusted: bool,
}

#[derive(Debug, Clone, Default)]
pub struct PeerDb {
    contacts: Vec<Contact>,
}

impl PeerDb {
    pub fn load_or_default() -> io::Result<Self> {
        let path = contacts_path()?;
        if path.exists() {
            load(&path)
        } else {
            Ok(Self::default())
        }
    }

    pub fn save(&self) -> io::Result<()> {
        save(&contacts_path()?, self)
    }

    pub fn iter(&self) -> impl Iterator<Item = &Contact> {
        self.contacts.iter()
    }

    pub fn by_peer_id(&self, peer_id: &[u8; 16]) -> Option<&Contact> {
        self.contacts.iter().find(|c| &c.peer_id == peer_id)
    }

    pub fn by_name(&self, name: &str) -> Option<&Contact> {
        self.contacts.iter().find(|c| c.name == name)
    }

    /// Upsert by peer_id. Updates name, public_key, last_addr, last_seen_unix
    /// if the contact exists; otherwise appends a new (untrusted) entry.
    pub fn upsert(&mut self, c: Contact) {
        if let Some(existing) = self.contacts.iter_mut().find(|x| x.peer_id == c.peer_id) {
            existing.name = c.name;
            existing.public_key = c.public_key;
            if c.last_addr.is_some() {
                existing.last_addr = c.last_addr;
            }
            existing.last_seen_unix = c.last_seen_unix;
        } else {
            self.contacts.push(c);
        }
    }

    pub fn mark_seen(&mut self, peer_id: &[u8; 16], addr: SocketAddr, unix: u64) {
        if let Some(c) = self.contacts.iter_mut().find(|x| x.peer_id == *peer_id) {
            c.last_addr = Some(addr);
            c.last_seen_unix = unix;
        }
    }

    pub fn trust(&mut self, peer_id: &[u8; 16]) -> bool {
        if let Some(c) = self.contacts.iter_mut().find(|x| x.peer_id == *peer_id) {
            c.trusted = true;
            true
        } else {
            false
        }
    }

    pub fn revoke(&mut self, peer_id: &[u8; 16]) -> bool {
        self.contacts.retain(|c| &c.peer_id != peer_id);
        true
    }
}

fn load(path: &std::path::Path) -> io::Result<PeerDb> {
    let bytes = std::fs::read(path)?;
    if bytes.len() < 4 + 1 + 4 {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "contacts file too short"));
    }
    if &bytes[..4] != MAGIC {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "contacts magic mismatch"));
    }
    if bytes[4] != VERSION {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "contacts version mismatch"));
    }
    let count = u32::from_be_bytes([bytes[5], bytes[6], bytes[7], bytes[8]]);
    let mut p = 9usize;
    let mut contacts = Vec::with_capacity(count as usize);
    for _ in 0..count {
        if p + 16 + 2 > bytes.len() {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "truncated contact"));
        }
        let mut pid = [0u8; 16];
        pid.copy_from_slice(&bytes[p..p + 16]);
        p += 16;
        let name_len = u16::from_be_bytes([bytes[p], bytes[p + 1]]) as usize;
        p += 2;
        if p + name_len > bytes.len() {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "name overflow"));
        }
        let name = std::str::from_utf8(&bytes[p..p + name_len])
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "name not utf-8"))?
            .to_string();
        p += name_len;
        if p + 32 + 1 > bytes.len() {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "truncated pubkey"));
        }
        let mut pk = [0u8; 32];
        pk.copy_from_slice(&bytes[p..p + 32]);
        p += 32;
        let has_addr = bytes[p];
        p += 1;
        let last_addr = if has_addr == 1 {
            if p + 6 > bytes.len() {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "truncated addr"));
            }
            let port = u16::from_be_bytes([bytes[p], bytes[p + 1]]);
            p += 2;
            let ip = Ipv4Addr::new(bytes[p], bytes[p + 1], bytes[p + 2], bytes[p + 3]);
            p += 4;
            Some(SocketAddr::V4(SocketAddrV4::new(ip, port)))
        } else {
            None
        };
        if p + 8 + 1 > bytes.len() {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "truncated tail"));
        }
        let ts = u64::from_be_bytes([
            bytes[p],
            bytes[p + 1],
            bytes[p + 2],
            bytes[p + 3],
            bytes[p + 4],
            bytes[p + 5],
            bytes[p + 6],
            bytes[p + 7],
        ]);
        p += 8;
        let trusted = bytes[p] != 0;
        p += 1;
        contacts.push(Contact {
            peer_id: pid,
            name,
            public_key: pk,
            last_addr,
            last_seen_unix: ts,
            trusted,
        });
    }
    Ok(PeerDb { contacts })
}

fn save(path: &std::path::Path, db: &PeerDb) -> io::Result<()> {
    let mut buf = Vec::new();
    buf.extend_from_slice(MAGIC);
    buf.push(VERSION);
    buf.extend_from_slice(&(db.contacts.len() as u32).to_be_bytes());
    for c in &db.contacts {
        if c.name.len() > NAME_MAX {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "name too long"));
        }
        buf.extend_from_slice(&c.peer_id);
        buf.extend_from_slice(&(c.name.len() as u16).to_be_bytes());
        buf.extend_from_slice(c.name.as_bytes());
        buf.extend_from_slice(&c.public_key);
        match c.last_addr {
            Some(SocketAddr::V4(v4)) => {
                buf.push(1);
                buf.extend_from_slice(&v4.port().to_be_bytes());
                buf.extend_from_slice(&v4.ip().octets());
            }
            _ => buf.push(0),
        }
        buf.extend_from_slice(&c.last_seen_unix.to_be_bytes());
        buf.push(if c.trusted { 1 } else { 0 });
    }
    std::fs::write(path, &buf)
}