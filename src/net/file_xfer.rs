//! File-transfer state machines.
//!
//! Two state machines live in the action thread:
//!
//! * **Outbound** — owned by `OutboundMap<FileId, OutboundTransfer>`.
//!   Created on `Action::SendFile`, progresses through
//!   `AwaitingAccept → Sending → Done` as frames go out via the
//!   registered per-peer `Sender<FrameBody>`.
//!
//! * **Inbound** — owned by `InboundMap<FileId, InboundTransfer>`.
//!   Created on `InboundFileEvent::Offer`, transitions to `Receiving`
//!   on `Action::AcceptFile`, writes chunks to disk as they arrive,
//!   finalises on `InboundFileEvent::Done`.
//!
//! Both stay small. They hold an open file handle and the byte
//! counters; everything else is a `FrameBody` flowing through the
//! already-registered per-peer sender (outbound) or an
//! `InboundFileEvent` flowing through the action-thread receiver
//! (inbound). The UI thread never sees transfer data.

use crate::config::config_dir;
use crate::events::{FileOffer, FileId, PeerId};
use crate::protocol::FrameBody;
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

/// Max bytes per chunk. Must match `protocol::FILE_CHUNK_MAX_DATA` —
/// the protocol caps each frame at 32 KiB of payload data.
pub const CHUNK_DATA: usize = 32 * 1024;

/// How long we wait for a peer to accept a `FileOffer` before giving
/// up and aborting the transfer.
const OFFER_TIMEOUT: Duration = Duration::from_secs(30);

// ---------------------------------------------------------------------------
// Outbound state machine
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OutboundPhase {
    AwaitingAccept,
    Sending,
    Done,
}

pub struct OutboundTransfer {
    pub peer: PeerId,
    pub to_name: String,
    /// What we announced.
    pub offer: FileOffer,
    /// Absolute path on disk we are streaming from.
    pub path: PathBuf,
    /// Open read handle. Held for the lifetime of the transfer.
    file: File,
    /// Bytes successfully handed to the session driver.
    bytes_sent: u64,
    phase: OutboundPhase,
    started_at: Instant,
}

impl OutboundTransfer {
    pub fn open(peer: PeerId, to_name: String, path: PathBuf) -> std::io::Result<Self> {
        let meta = fs::metadata(&path)?;
        if !meta.is_file() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "not a regular file",
            ));
        }
        let size = meta.len();
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("file")
            .to_string();
        let id = FileId::random();
        let offer = FileOffer {
            id,
            name,
            size,
            mime: None,
        };
        let file = File::open(&path)?;
        Ok(Self {
            peer,
            to_name,
            offer,
            path,
            file,
            bytes_sent: 0,
            phase: OutboundPhase::AwaitingAccept,
            started_at: Instant::now(),
        })
    }

    pub fn id(&self) -> FileId {
        self.offer.id
    }

    pub fn offer(&self) -> &FileOffer {
        &self.offer
    }

    pub fn timed_out(&self) -> bool {
        self.phase == OutboundPhase::AwaitingAccept
            && self.started_at.elapsed() > OFFER_TIMEOUT
    }

    pub fn mark_accepted(&mut self) {
        if self.phase == OutboundPhase::AwaitingAccept {
            self.phase = OutboundPhase::Sending;
        }
    }

    /// Stream one chunk out via `tx`. Returns the outcome so the
    /// action thread can detect completion vs. error vs. nothing-to-do.
    pub fn step(&mut self, tx: &Sender<FrameBody>) -> std::io::Result<Option<ChunkOutcome>> {
        if self.phase != OutboundPhase::Sending {
            return Ok(None);
        }
        if self.bytes_sent >= self.offer.size {
            self.phase = OutboundPhase::Done;
            let _ = tx.send(FrameBody::FileDone { id: self.offer.id });
            return Ok(Some(ChunkOutcome::Complete));
        }
        let mut buf = vec![0u8; CHUNK_DATA];
        let n = self.file.read(&mut buf)?;
        if n == 0 {
            return Ok(Some(ChunkOutcome::Error("file shrunk during send")));
        }
        buf.truncate(n);
        let offset = self.bytes_sent;
        let id = self.offer.id;
        tx.send(FrameBody::FileChunk {
            id,
            offset,
            data: buf,
        })
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::BrokenPipe, "session gone"))?;
        self.bytes_sent += n as u64;
        Ok(Some(ChunkOutcome::Advanced))
    }

    pub fn into_aborted(self, reason: &str) -> AbortInfo {
        AbortInfo {
            from_peer: self.peer,
            from_name: self.to_name,
            name: self.offer.name,
            reason: reason.to_string(),
        }
    }
}

pub enum ChunkOutcome {
    Advanced,
    Complete,
    Error(&'static str),
}

#[derive(Debug)]
pub struct AbortInfo {
    pub from_peer: PeerId,
    pub from_name: String,
    pub name: String,
    pub reason: String,
}

pub struct OutboundMap {
    by_id: HashMap<FileId, OutboundTransfer>,
}

impl OutboundMap {
    pub fn new() -> Self {
        Self {
            by_id: HashMap::new(),
        }
    }

    pub fn insert(&mut self, t: OutboundTransfer) {
        self.by_id.insert(t.offer.id, t);
    }

    pub fn accept(&mut self, id: FileId) -> bool {
        if let Some(t) = self.by_id.get_mut(&id) {
            t.mark_accepted();
            true
        } else {
            false
        }
    }

    pub fn reject(&mut self, id: FileId) -> Option<AbortInfo> {
        self.by_id.remove(&id).map(|t| t.into_aborted("peer rejected"))
    }

    pub fn tick_timeouts(&mut self) -> Vec<AbortInfo> {
        let expired: Vec<FileId> = self
            .by_id
            .iter()
            .filter(|(_, t)| t.timed_out())
            .map(|(id, _)| *id)
            .collect();
        let mut aborted = Vec::new();
        for id in expired {
            if let Some(t) = self.by_id.remove(&id) {
                aborted.push(t.into_aborted("peer did not respond"));
            }
        }
        aborted
    }

    pub fn step_all<F>(&mut self, mut pick_tx: F) -> Vec<StepResult>
    where
        F: FnMut(PeerId) -> Option<Sender<FrameBody>>,
    {
        let mut out = Vec::new();
        let ids: Vec<FileId> = self.by_id.keys().copied().collect();
        for id in ids {
            let peer = match self.by_id.get(&id) {
                Some(t) => t.peer,
                None => continue,
            };
            let tx = match pick_tx(peer) {
                Some(t) => t,
                None => continue,
            };
            let mut t = match self.by_id.remove(&id) {
                Some(t) => t,
                None => continue,
            };
            match t.step(&tx) {
                Ok(Some(ChunkOutcome::Advanced)) => {
                    self.by_id.insert(id, t);
                }
                Ok(Some(ChunkOutcome::Complete)) => {
                    out.push(StepResult::Completed {
                        peer: t.peer,
                        to_name: t.to_name.clone(),
                        name: t.offer.name.clone(),
                        bytes: t.bytes_sent,
                    });
                }
                Ok(Some(ChunkOutcome::Error(reason))) => {
                    out.push(StepResult::Aborted(t.into_aborted(reason)));
                }
                Ok(None) => {
                    // Still awaiting accept — put it back.
                    self.by_id.insert(id, t);
                }
                Err(e) => {
                    out.push(StepResult::Aborted(t.into_aborted(&format!("io: {}", e))));
                }
            }
        }
        out
    }

    pub fn remove_for_peer(&mut self, peer: PeerId) -> Vec<AbortInfo> {
        let ids: Vec<FileId> = self
            .by_id
            .iter()
            .filter(|(_, t)| t.peer == peer)
            .map(|(id, _)| *id)
            .collect();
        let mut aborted = Vec::new();
        for id in ids {
            if let Some(t) = self.by_id.remove(&id) {
                aborted.push(t.into_aborted("peer disconnected"));
            }
        }
        aborted
    }
}

pub enum StepResult {
    Completed {
        peer: PeerId,
        to_name: String,
        name: String,
        bytes: u64,
    },
    Aborted(AbortInfo),
}

// ---------------------------------------------------------------------------
// Inbound state machine
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InboundPhase {
    Pending,
    Receiving,
    Done,
}

pub struct InboundTransfer {
    pub peer: PeerId,
    pub from_name: String,
    pub offer: FileOffer,
    file: Option<File>,
    bytes_written: u64,
    path: PathBuf,
    phase: InboundPhase,
}

impl InboundTransfer {
    pub fn new(peer: PeerId, from_name: String, offer: FileOffer) -> Self {
        Self {
            peer,
            from_name,
            offer,
            file: None,
            bytes_written: 0,
            path: PathBuf::new(),
            phase: InboundPhase::Pending,
        }
    }

    pub fn id(&self) -> FileId {
        self.offer.id
    }

    pub fn is_pending(&self) -> bool {
        self.phase == InboundPhase::Pending
    }

    pub fn accept(&mut self) -> std::io::Result<()> {
        if self.phase != InboundPhase::Pending {
            return Ok(());
        }
        let dir = received_dir()?;
        self.path = dir.join(format!(
            "{}-{}",
            self.offer.id.to_hex(),
            sanitize(&self.offer.name)
        ));
        let file = File::create(&self.path)?;
        self.file = Some(file);
        self.phase = InboundPhase::Receiving;
        Ok(())
    }

    pub fn write_chunk(&mut self, offset: u64, data: &[u8]) -> std::io::Result<()> {
        if self.phase != InboundPhase::Receiving {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "transfer not in receiving state",
            ));
        }
        let file = self
            .file
            .as_mut()
            .expect("file handle must exist in Receiving");
        if offset != self.bytes_written {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!(
                    "out-of-order chunk: expected offset {}, got {}",
                    self.bytes_written, offset
                ),
            ));
        }
        file.write_all(data)?;
        self.bytes_written += data.len() as u64;
        Ok(())
    }

    pub fn finalize(&mut self, expected_size: u64) -> Result<InboundInfo, InboundError> {
        if self.phase != InboundPhase::Receiving {
            return Err(InboundError::WrongPhase);
        }
        if expected_size != self.offer.size || self.bytes_written != self.offer.size {
            return Err(InboundError::SizeMismatch {
                expected: self.offer.size,
                got: self.bytes_written,
            });
        }
        let file = self.file.as_mut().expect("file in Receiving");
        file.flush().map_err(|e| InboundError::Io(format!("flush: {}", e)))?;
        file.sync_all().map_err(|e| InboundError::Io(format!("sync: {}", e)))?;
        self.file = None;
        self.phase = InboundPhase::Done;
        Ok(InboundInfo {
            peer: self.peer,
            from_name: self.from_name.clone(),
            name: self.offer.name.clone(),
            bytes: self.bytes_written,
            path: self.path.clone(),
        })
    }

    pub fn abort(mut self, reason: &str) -> InboundAbort {
        if let Some(mut f) = self.file.take() {
            let _ = f.flush();
        }
        let partial = if self.bytes_written > 0 && self.path.exists() {
            let partial = self.path.with_extension(format!(
                "{}.partial",
                self.path
                    .extension()
                    .and_then(|e| e.to_str())
                    .unwrap_or("")
            ));
            let _ = fs::rename(&self.path, &partial);
            Some(partial)
        } else {
            None
        };
        InboundAbort {
            peer: self.peer,
            from_name: self.from_name,
            name: self.offer.name,
            reason: reason.to_string(),
            partial,
        }
    }
}

#[derive(Debug)]
pub struct InboundInfo {
    pub peer: PeerId,
    pub from_name: String,
    pub name: String,
    pub bytes: u64,
    pub path: PathBuf,
}

#[derive(Debug)]
pub struct InboundAbort {
    pub peer: PeerId,
    pub from_name: String,
    pub name: String,
    pub reason: String,
    pub partial: Option<PathBuf>,
}

#[derive(Debug)]
pub enum InboundError {
    WrongPhase,
    SizeMismatch { expected: u64, got: u64 },
    Io(String),
}

pub struct InboundMap {
    by_id: HashMap<FileId, InboundTransfer>,
}

impl InboundMap {
    pub fn new() -> Self {
        Self {
            by_id: HashMap::new(),
        }
    }

    /// Insert a fresh inbound transfer. Returns `false` if an entry
    /// for this `id` already exists — duplicate offers are dropped
    /// silently (first wins).
    pub fn offer(&mut self, t: InboundTransfer) -> bool {
        if self.by_id.contains_key(&t.offer.id) {
            return false;
        }
        self.by_id.insert(t.offer.id, t);
        true
    }

    pub fn accept(&mut self, id: FileId) -> std::io::Result<Option<FileOffer>> {
        if let Some(t) = self.by_id.get_mut(&id) {
            t.accept()?;
            Ok(Some(t.offer.clone()))
        } else {
            Ok(None)
        }
    }

    pub fn write_chunk(&mut self, id: FileId, offset: u64, data: Vec<u8>) -> WriteOutcome {
        let Some(t) = self.by_id.get_mut(&id) else {
            return WriteOutcome::Unknown;
        };
        match t.write_chunk(offset, &data) {
            Ok(()) => WriteOutcome::Ok,
            Err(e) => WriteOutcome::Error(e.to_string()),
        }
    }

    pub fn finalize(&mut self, id: FileId, expected_size: u64) -> FinalizeOutcome {
        let Some(mut t) = self.by_id.remove(&id) else {
            return FinalizeOutcome::Unknown;
        };
        match t.finalize(expected_size) {
            Ok(info) => FinalizeOutcome::Done(info),
            Err(e) => FinalizeOutcome::Failed(e),
        }
    }

    pub fn reject(&mut self, id: FileId) -> Option<FileOffer> {
        self.by_id.remove(&id).map(|t| t.offer)
    }

    /// Return the offer's announced size without removing the entry.
    /// Used by the action thread when handling `InboundFileEvent::Done`
    /// to learn the expected size before calling `finalize()`.
    pub fn offer_size(&self, id: &FileId) -> Option<u64> {
        self.by_id.get(id).map(|t| t.offer.size)
    }

    pub fn remove_for_peer(&mut self, peer: PeerId) -> Vec<InboundAbort> {
        let ids: Vec<FileId> = self
            .by_id
            .iter()
            .filter(|(_, t)| t.peer == peer)
            .map(|(id, _)| *id)
            .collect();
        let mut aborted = Vec::new();
        for id in ids {
            if let Some(t) = self.by_id.remove(&id) {
                aborted.push(t.abort("peer disconnected"));
            }
        }
        aborted
    }
}

pub enum WriteOutcome {
    Ok,
    Unknown,
    Error(String),
}

pub enum FinalizeOutcome {
    Done(InboundInfo),
    Unknown,
    Failed(InboundError),
}

// ---------------------------------------------------------------------------
// Path helpers
// ---------------------------------------------------------------------------

/// Returns `<config_dir>/received/`, creating it on first call.
pub fn received_dir() -> std::io::Result<PathBuf> {
    let dir = config_dir()?.join("received");
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Strip path separators, control characters, and leading
/// dots/spaces from `name`. The result is a single filename safe
/// to drop under `<config_dir>/received/<id>-<safe>`.
fn sanitize(name: &str) -> String {
    let mut s: String = name
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '\0' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect();
    // Strip leading dots, spaces, and any underscores they exposed so
    // path-traversal attempts like "../etc/passwd" and "../foo" land as
    // innocuous names. Repeat until stable.
    while let Some(c) = s.chars().next() {
        if c == '.' || c == ' ' || c == '_' {
            s.remove(0);
        } else {
            break;
        }
    }
    if s.is_empty() {
        s.push_str("file");
    }
    s.truncate(200);
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "lanchat-fxfer-test-{}-{}",
            std::process::id(),
            rand_u64()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn rand_u64() -> u64 {
        use rand_core::{OsRng, RngCore};
        let mut b = [0u8; 8];
        OsRng.fill_bytes(&mut b);
        u64::from_le_bytes(b)
    }

    #[test]
    fn sanitize_strips_separators_and_dots() {
        assert_eq!(sanitize("report.pdf"), "report.pdf");
        assert_eq!(sanitize("../etc/passwd"), "etc_passwd");
        assert_eq!(sanitize(".."), "file");
        assert_eq!(sanitize(" "), "file");
        assert_eq!(sanitize("foo\0bar"), "foo_bar");
        assert_eq!(sanitize("a:b\\c/d"), "a_b_c_d");
    }

    #[test]
    fn outbound_open_rejects_directory() {
        let dir = tmp();
        assert!(OutboundTransfer::open([0u8; 16], "x".into(), dir).is_err());
    }

    #[test]
    fn outbound_open_succeeds_for_regular_file() {
        let dir = tmp();
        let p = dir.join("hello.txt");
        fs::write(&p, b"hi").unwrap();
        let t = OutboundTransfer::open([1u8; 16], "peer".into(), p).unwrap();
        assert_eq!(t.offer.name, "hello.txt");
        assert_eq!(t.offer.size, 2);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn inbound_offer_then_accept_writes_chunks() {
        let dir = tmp();
        // Override HOME so received_dir() resolves to our tmp.
        // We can't actually override HOME per-test on all platforms
        // without env mutation; instead, test the lower-level
        // write_chunk logic directly.
        let id = FileId::random();
        let offer = FileOffer {
            id,
            name: "blob.bin".into(),
            size: 6,
            mime: None,
        };
        let mut t = InboundTransfer::new([2u8; 16], "alice".into(), offer.clone());

        // Manually create a file in the temp dir + move it to
        // `t.path` so we can skip the received_dir() bootstrap.
        let dest = dir.join("inbound-test.bin");
        t.path = dest.clone();
        t.file = Some(File::create(&dest).unwrap());
        t.phase = InboundPhase::Receiving;

        t.write_chunk(0, b"abc").unwrap();
        t.write_chunk(3, b"def").unwrap();
        let info = t.finalize(6).unwrap();
        assert_eq!(info.bytes, 6);
        let on_disk = fs::read(&dest).unwrap();
        assert_eq!(on_disk, b"abcdef");

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn inbound_out_of_order_chunk_errors() {
        let dir = tmp();
        let dest = dir.join("ooo.bin");
        let mut t = InboundTransfer::new(
            [3u8; 16],
            "alice".into(),
            FileOffer {
                id: FileId::random(),
                name: "x".into(),
                size: 10,
                mime: None,
            },
        );
        t.path = dest.clone();
        t.file = Some(File::create(&dest).unwrap());
        t.phase = InboundPhase::Receiving;
        t.write_chunk(0, b"abcd").unwrap();
        // Skipping offset 4 → out-of-order.
        assert!(t.write_chunk(8, b"efgh").is_err());
        let _ = fs::remove_dir_all(dir);
    }
}