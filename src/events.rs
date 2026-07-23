//! Internal mpsc bus between the network worker thread and the UI thread.
//!
//! `Event` flows network → UI. `Action` flows UI → network. Two channels,
//! one in each direction, connected by a `Bus`.
//!
//! A third channel (`tx_inbound_files` / `rx_inbound_files`) carries
//! `InboundFileEvent` — the per-connection driver forwards `FileOffer`,
//! `FileChunk`, etc. straight to the action thread, which owns the
//! per-peer transfer state. This keeps the UI thread out of the file
//! data path.

pub use crate::protocol::{FileId, FrameBody};
use crate::net::session::Session;
use std::net::{SocketAddr, TcpStream};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};

pub type PeerId = [u8; 16];

/// One open peer session, owned by the network thread. The UI thread never
/// touches the underlying socket — it sends `Action::SendText` and the
/// network thread forwards to the `Session`.
pub struct PeerSession {
    pub peer_id: PeerId,
    pub name: String,
    pub fingerprint: String,
    pub last_addr: SocketAddr,
    pub session: Session<TcpStream>,
}

#[derive(Debug)]
pub enum Event {
    /// A peer announced themselves on the multicast group.
    PeerSeen {
        peer_id: PeerId,
        name: String,
        public_key: [u8; 32],
        fingerprint: String,
        addr: SocketAddr,
    },
    /// We have a fresh encrypted session with a peer (either inbound or outbound).
    PeerConnected {
        peer_id: PeerId,
        name: String,
        fingerprint: String,
        trusted: bool,
        addr: SocketAddr,
    },
    /// An inbound chat message.
    TextMessage {
        from_peer: PeerId,
        from_name: String,
        body: String,
    },
    /// Decryption failed for a peer's frame — usually means their pubkey changed.
    DecryptFailed { peer_id: PeerId, from_name: String },
    /// A peer's TCP connection dropped.
    PeerGone { peer_id: PeerId, name: String },
    /// Free-form status string the UI should display.
    Info(String),
    /// One scan method finished — its findings land in the discovery modal.
    DiscoveryUpdate {
        method: String,
        peers: Vec<DiscoveredPeer>,
    },
    /// All `/discover` methods have completed.
    DiscoveryFinished,
    /// An inbound file offer landed; the user must accept or reject it
    /// via `Action::AcceptFile` / `Action::RejectFile`.
    FileOffer {
        from_peer: PeerId,
        from_name: String,
        offer: FileOffer,
    },
    /// Receiver finished writing an inbound file. `saved_to` is the
    /// absolute path on disk.
    FileReceived {
        from_peer: PeerId,
        from_name: String,
        name: String,
        bytes: u64,
        saved_to: PathBuf,
    },
    /// A transfer (outbound or inbound) was aborted. `reason` is
    /// human-readable; `partial` is set to the on-disk path if any
    /// bytes were written (`.partial` suffix for incomplete files).
    FileAborted {
        from_peer: PeerId,
        from_name: String,
        name: String,
        reason: String,
        partial: Option<PathBuf>,
    },
}

/// One file offer carried over the wire. Mirrors the encoded payload
/// of `FrameBody::FileOffer`. `mime` may be absent when the sender
/// didn't supply one.
#[derive(Debug, Clone)]
pub struct FileOffer {
    pub id: FileId,
    pub name: String,
    pub size: u64,
    pub mime: Option<String>,
}

/// One peer discovered by a scan. Mirrors the public struct in
/// `tui::discovery_popup` so the network thread can construct it directly.
#[derive(Debug, Clone)]
pub struct DiscoveredPeer {
    pub name: Option<String>,
    pub addr: SocketAddr,
    pub fingerprint: Option<String>,
}

#[derive(Debug)]
pub enum Action {
    SendText { to: PeerId, body: String },
    /// Send a file at `path` to a peer. The action thread opens the
    /// file, generates a `FileId`, sends `FileOffer`, and waits for
    /// `FileAccept` / `FileReject` before streaming chunks.
    SendFile { to: PeerId, path: PathBuf },
    /// Accept an inbound file offer. The action thread creates the
    /// destination file under `<config_dir>/received/` and replies
    /// with a `FileAccept` frame.
    AcceptFile { from_peer: PeerId, id: FileId },
    /// Reject an inbound file offer. Sends `FileReject` and drops the
    /// pending transfer state.
    RejectFile { from_peer: PeerId, id: FileId },
    Connect { addr: SocketAddr, name_hint: String, public_key: [u8; 32] },
    Disconnect { peer_id: PeerId },
    Trust { peer_id: PeerId },
    Revoke { peer_id: PeerId },
    Quit,
}

/// Inbound file-event traffic: the per-connection driver forwards
/// these straight to the action thread, bypassing the UI. The action
/// thread owns the per-peer inbound transfer state and writes chunks
/// to disk as they arrive.
#[derive(Debug)]
pub enum InboundFileEvent {
    Offer { peer: PeerId, offer: FileOffer },
    Accept { peer: PeerId, id: FileId },
    Reject { peer: PeerId, id: FileId },
    Chunk { peer: PeerId, id: FileId, offset: u64, data: Vec<u8> },
    Done { peer: PeerId, id: FileId },
}

/// Per-peer outbound-sender registration. The per-connection session
/// driver registers its `mpsc::Sender<FrameBody>` on startup so the
/// action consumer thread can route `Action::SendText` through it, and
/// unregisters on exit so a stale entry doesn't outlive the session.
pub enum RegistryMsg {
    Register {
        peer_id: PeerId,
        name: String,
        sender: Sender<FrameBody>,
    },
    Unregister {
        peer_id: PeerId,
    },
}

pub struct Bus {
    pub tx_events: Sender<Event>,
    pub rx_events: Receiver<Event>,
    pub tx_actions: Sender<Action>,
    pub rx_actions: Receiver<Action>,
    /// Network → action thread. Drivers forward inbound file frames
    /// (Offer/Chunk/Done) here. The action thread is the sole owner
    /// of inbound transfer state.
    pub tx_inbound_files: Sender<InboundFileEvent>,
    pub rx_inbound_files: Receiver<InboundFileEvent>,
}

impl Bus {
    pub fn new() -> Self {
        let (tx_events, rx_events) = mpsc::channel();
        let (tx_actions, rx_actions) = mpsc::channel();
        let (tx_inbound_files, rx_inbound_files) = mpsc::channel();
        Self {
            tx_events,
            rx_events,
            tx_actions,
            rx_actions,
            tx_inbound_files,
            rx_inbound_files,
        }
    }
}

impl Default for Bus {
    fn default() -> Self {
        Self::new()
    }
}