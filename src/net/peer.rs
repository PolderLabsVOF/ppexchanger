//! Outbound peer connector + per-peer session driver.
//!
//! `dial(addr, static_kp)` completes the Noise_XX handshake and returns a
//! fresh `Session<TcpStream>`. Callers hand the session off to
//! `spawn_session_driver`, which:
//!   * owns the `Session` exclusively
//!   * polls inbound frames with `try_recv` (50ms tick) and posts them to
//!     the bus as `Event::TextMessage` / `Event::DecryptFailed`
//!   * drains `outbound_rx` between polls and writes them via `Session::send`
//!   * posts `Event::PeerGone` when either side closes or AEAD fails
//!   * posts `RegistryMsg::Unregister` on exit so the action consumer
//!     drops the outbound sender from its registry
//!
//! `connect()` is the convenience entry point: dial + spawn + return a
//! `Sender<FrameBody>` that the action thread uses for outbound messages.

use crate::crypto::Keypair;
use crate::events::{DiscoveredPeer, Event, FileOffer, InboundFileEvent, PeerId, RegistryMsg};
use crate::net::handshake::run_initiator;
use crate::net::listener::peer_id_from_pubkey;
use crate::net::session::Session;
use crate::protocol::{fingerprint as pubkey_fingerprint, FrameBody};
use std::net::{SocketAddr, TcpStream};
use std::sync::mpsc;
use std::time::Duration;

/// Keep a failed LAN dial responsive enough for discovery's reverse-connect
/// fallback instead of leaving the UI waiting on the operating system's TCP
/// timeout (which can be a minute or more).
const DIAL_TIMEOUT: Duration = Duration::from_secs(2);
const DIAL_ATTEMPTS: usize = 3;

/// Connect to a peer via TCP + Noise_XX. Returns the freshly minted session.
pub fn dial(addr: SocketAddr, static_kp: &Keypair) -> std::io::Result<Session<TcpStream>> {
    let mut stream = None;
    let mut last_error = None;
    for attempt in 0..DIAL_ATTEMPTS {
        match TcpStream::connect_timeout(&addr, DIAL_TIMEOUT) {
            Ok(s) => {
                stream = Some(s);
                break;
            }
            Err(e) => {
                last_error = Some(e);
                if attempt + 1 < DIAL_ATTEMPTS {
                    std::thread::sleep(Duration::from_millis(150));
                }
            }
        }
    }
    let mut s = stream.ok_or_else(|| {
        last_error.unwrap_or_else(|| std::io::Error::other("connection attempts exhausted"))
    })?;
    s.set_nodelay(true)?;
    s.set_read_timeout(Some(DIAL_TIMEOUT))?;
    s.set_write_timeout(Some(DIAL_TIMEOUT))?;
    let res = run_initiator(&mut s, static_kp)
        .map_err(|e| std::io::Error::other(format!("handshake failed: {:?}", e)))?;
    let sess = Session::new(s, res.send_key, res.recv_key, res.remote_static);
    Ok(sess)
}

/// Connect to a peer + spawn the per-connection driver thread. The
/// outbound sender is registered with the action consumer before
/// `connect` returns, so callers can immediately route outbound messages.
///
/// Returns:
///   * `peer_id` derived from the peer's static pubkey
///   * `discovered` summary (name hint, addr, fingerprint) for the UI
pub fn connect(
    addr: SocketAddr,
    name_hint: Option<String>,
    static_kp: &Keypair,
    tx: mpsc::Sender<Event>,
    tx_inbound: mpsc::Sender<InboundFileEvent>,
    reg_tx: mpsc::Sender<RegistryMsg>,
    local_name: String,
    local_hostname: String,
) -> Option<(PeerId, DiscoveredPeer)> {
    let sess = match dial(addr, static_kp) {
        Ok(s) => s,
        Err(e) => {
            let _ = tx.send(Event::Info(format!("could not connect to {}: {}", addr, e)));
            return None;
        }
    };
    let peer_id = peer_id_from_pubkey(&sess.remote_static);
    let fingerprint = pubkey_fingerprint(&sess.remote_static);
    let display_name = name_hint.clone().unwrap_or_else(|| format!("peer@{}", addr));
    let discovered = DiscoveredPeer {
        name: Some(display_name.clone()),
        hostname: None,
        addr,
        fingerprint: Some(fingerprint.clone()),
        reverse: None,
    };
    let (outbound_tx, outbound_rx) = mpsc::channel::<FrameBody>();
    let _ = reg_tx.send(RegistryMsg::Register {
        peer_id,
        name: display_name,
        sender: outbound_tx,
    });
    let reg_tx_for_driver = reg_tx.clone();
    let tx_inbound_for_driver = tx_inbound.clone();
    spawn_session_driver_with_reg(
        sess,
        peer_id,
        fingerprint,
        outbound_rx,
        tx,
        tx_inbound_for_driver,
        Some(reg_tx_for_driver),
        local_name,
        local_hostname,
    );
    let _ = reg_tx; // keep alive until end of fn (for clarity)
    Some((peer_id, discovered))
}

/// Spawn the per-connection driver thread for an already-handshaked session.
/// Used by the inbound listener path (which produces the session from the
/// responder side) and by `connect` for outbound sessions.
pub fn spawn_session_driver(
    sess: Session<TcpStream>,
    peer_id: PeerId,
    fingerprint: String,
    outbound_rx: mpsc::Receiver<FrameBody>,
    tx: mpsc::Sender<Event>,
    tx_inbound: mpsc::Sender<InboundFileEvent>,
    local_name: String,
    local_hostname: String,
) {
    spawn_session_driver_with_reg(
        sess,
        peer_id,
        fingerprint,
        outbound_rx,
        tx,
        tx_inbound,
        None,
        local_name,
        local_hostname,
    )
}

/// Variant of `spawn_session_driver` that also accepts a registry channel.
/// On exit (peer gone or AEAD failure) the driver posts `Unregister` so
/// the action consumer can drop the outbound sender.
pub fn spawn_session_driver_with_reg(
    mut sess: Session<TcpStream>,
    peer_id: PeerId,
    fingerprint: String,
    outbound_rx: mpsc::Receiver<FrameBody>,
    tx: mpsc::Sender<Event>,
    tx_inbound: mpsc::Sender<InboundFileEvent>,
    reg_tx: Option<mpsc::Sender<RegistryMsg>>,
    local_name: String,
    local_hostname: String,
) {
    std::thread::spawn(move || {
        let mut hello_sent = false;
        let mut display = fingerprint.clone();
        let exit = |tx: &mpsc::Sender<Event>, reg_tx: &Option<mpsc::Sender<RegistryMsg>>, name: &str| {
            let _ = tx.send(Event::PeerGone {
                peer_id,
                name: name.to_string(),
            });
            if let Some(r) = reg_tx {
                let _ = r.send(RegistryMsg::Unregister { peer_id });
            }
        };
        loop {
            if !hello_sent {
                if let Err(e) = sess.send(&FrameBody::Hello { name: local_name.clone(), hostname: local_hostname.clone() }) {
                    let _ = tx.send(Event::Info(format!("session hello send failed for {}: {}", display, e)));
                    exit(&tx, &reg_tx, &display);
                    return;
                }
                hello_sent = true;
            }
            // 1) Drain outbound queue.
            loop {
                match outbound_rx.try_recv() {
                    Ok(body) => {
                        if let Err(e) = sess.send(&body) {
                            let _ = tx.send(Event::Info(format!("session send failed for {}: {}", display, e)));
                            exit(&tx, &reg_tx, &display);
                            return;
                        }
                    }
                    Err(mpsc::TryRecvError::Empty) => break,
                    Err(mpsc::TryRecvError::Disconnected) => {
                        let _ = sess.send(&FrameBody::Bye);
                        exit(&tx, &reg_tx, &display);
                        return;
                    }
                }
            }
            // 2) Poll inbound.
            match sess.try_recv() {
                Ok(Some(frame)) => {
                    match frame.body {
                        FrameBody::Hello { name, hostname } => {
                            let display_name = if hostname.is_empty() { name } else { format!("{} ({})", name, hostname) };
                            display = display_name.clone();
                            let _ = tx.send(Event::Info(format!("authenticated peer identity: {}", display_name)));
                            let _ = tx.send(Event::PeerNamed { peer_id, name: display_name });
                        }
                        FrameBody::Text(s) => {
                            let _ = tx.send(Event::TextMessage {
                                from_peer: peer_id,
                                from_name: display.clone(),
                                body: s,
                            });
                        }
                        FrameBody::Bye => {
                            exit(&tx, &reg_tx, &display);
                            return;
                        }
                        // File-* frames are routed over the inbound-file
                        // channel so the action thread owns the per-peer
                        // transfer state. The driver only decodes and
                        // forwards — no state lives here.
                        FrameBody::FileOffer { id, name, size, mime } => {
                            let _ = tx_inbound.send(InboundFileEvent::Offer {
                                peer: peer_id,
                                offer: FileOffer { id, name, size, mime },
                            });
                        }
                        FrameBody::FileAccept { id } => {
                            let _ = tx_inbound.send(InboundFileEvent::Accept {
                                peer: peer_id,
                                id,
                            });
                        }
                        FrameBody::FileReject { id } => {
                            let _ = tx_inbound.send(InboundFileEvent::Reject {
                                peer: peer_id,
                                id,
                            });
                        }
                        FrameBody::FileChunk { id, offset, data } => {
                            let _ = tx_inbound.send(InboundFileEvent::Chunk {
                                peer: peer_id,
                                id,
                                offset,
                                data,
                            });
                        }
                        FrameBody::FileDone { id } => {
                            let _ = tx_inbound.send(InboundFileEvent::Done {
                                peer: peer_id,
                                id,
                            });
                        }
                    }
                }
                Ok(None) => continue,
                Err(e) => {
                    let _ = tx.send(Event::Info(format!("session receive failed for {}: {}", display, e)));
                    exit(&tx, &reg_tx, &display);
                    return;
                }
            }
        }
    });
}
