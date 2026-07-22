//! Internal mpsc bus between the network worker thread and the UI thread.
//!
//! `Event` flows network → UI. `Action` flows UI → network. Two channels,
//! one in each direction, connected by a `Bus`.

use crate::net::session::Session;
use std::net::{SocketAddr, TcpStream};
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
}

#[derive(Debug)]
pub enum Action {
    SendText { to: PeerId, body: String },
    Connect { addr: SocketAddr, name_hint: String, public_key: [u8; 32] },
    Disconnect { peer_id: PeerId },
    Trust { peer_id: PeerId },
    Revoke { peer_id: PeerId },
    Quit,
}

pub struct Bus {
    pub tx_events: Sender<Event>,
    pub rx_events: Receiver<Event>,
    pub tx_actions: Sender<Action>,
    pub rx_actions: Receiver<Action>,
}

impl Bus {
    pub fn new() -> Self {
        let (tx_events, rx_events) = mpsc::channel();
        let (tx_actions, rx_actions) = mpsc::channel();
        Self {
            tx_events,
            rx_events,
            tx_actions,
            rx_actions,
        }
    }
}

impl Default for Bus {
    fn default() -> Self {
        Self::new()
    }
}