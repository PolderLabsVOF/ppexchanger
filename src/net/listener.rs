//! TCP listener that accepts inbound peers, runs the responder handshake on
//! each new connection, and forwards the resulting `Session<TcpStream>` to a
//! caller-supplied channel.

use crate::crypto::Keypair;
use crate::net::handshake::run_responder;
use crate::net::session::Session;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::mpsc;
use std::sync::Arc;

/// What the listener hands back to the caller for each accepted peer.
pub struct AcceptedPeer {
    pub remote_addr: SocketAddr,
    pub remote_static: [u8; 32],
    pub remote_fingerprint: String,
    pub session: Session<TcpStream>,
}

impl AcceptedPeer {
    /// Convenience: turn an accepted peer into an `Event::PeerConnected` and a
    /// `PeerSession` for downstream handling.
    pub fn into_event(
        self,
    ) -> (
        crate::events::Event,
        crate::events::PeerSession,
    ) {
        let peer_id = peer_id_from_pubkey(&self.remote_static);
        let event = crate::events::Event::PeerConnected {
            peer_id,
            name: format!("peer@{}", self.remote_addr),
            fingerprint: self.remote_fingerprint.clone(),
            trusted: false,
            addr: self.remote_addr,
        };
        let session = crate::events::PeerSession {
            peer_id,
            name: format!("peer@{}", self.remote_addr),
            fingerprint: self.remote_fingerprint,
            last_addr: self.remote_addr,
            session: self.session,
        };
        (event, session)
    }
}

/// Deterministic 16-byte peer_id derived from the first 8 bytes of
/// SHA-256(pubkey). Avoids needing a separate random id exchanged out of band.
pub fn peer_id_from_pubkey(pubkey: &[u8; 32]) -> [u8; 16] {
    use sha2::{Digest, Sha256};
    let h = Sha256::digest(pubkey);
    let mut out = [0u8; 16];
    out.copy_from_slice(&h[..16]);
    out
}

/// Bind on `0.0.0.0:port` (use port `0` for ephemeral) and return a
/// non-blocking listener. Non-blocking lets the accept loop also check
/// a stop flag between attempts so shutdown doesn't hang on a parked
/// `accept()` call.
pub fn bind(port: u16) -> std::io::Result<TcpListener> {
    let l = TcpListener::bind(("0.0.0.0", port))?;
    l.set_nonblocking(true)?;
    Ok(l)
}

/// Run the accept loop on `listener`. For each new connection, spawn a
/// thread that performs the responder handshake and sends the resulting
/// `AcceptedPeer` down `tx`. Errors are silently dropped (logged nowhere
/// since we have no logging crate) — caller decides whether to log them.
pub fn run(listener: TcpListener, static_kp: Arc<Keypair>, tx: mpsc::Sender<AcceptedPeer>) {
    loop {
        match listener.accept() {
            Ok((stream, addr)) => {
                let kp = Arc::clone(&static_kp);
                let tx2 = tx.clone();
                std::thread::spawn(move || {
                    let mut s = stream;
                    match run_responder(&mut s, &kp) {
                        Ok(res) => {
                            let session =
                                Session::new(s, res.send_key, res.recv_key, res.remote_static);
                            let _ = tx2.send(AcceptedPeer {
                                remote_addr: addr,
                                remote_static: res.remote_static,
                                remote_fingerprint: res.remote_fingerprint,
                                session,
                            });
                        }
                        Err(_e) => {
                            // Handshake failed; drop the connection.
                        }
                    }
                });
            }
            Err(_) => {
                // Accept error: brief sleep + retry so we don't spin on a
                // transient condition (e.g. file-descriptor exhaustion).
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
        }
    }
}
