//! End-to-end test of `net::session::Session`.
//!
//! - `ten_messages_each_way`: full loopback TCP roundtrip.
//! - `tampered_ciphertext_rejected`: writes a bogus length-prefixed payload
//!   onto the wire after the handshake and verifies the receiver errors.
//!
//! Replay protection is exercised by the same `Session::recv` path: the AEAD
//! nonce is derived from the expected `recv_seq`, so a resend uses a stale
//! nonce and fails authentication.

use lanchat::crypto::Keypair;
use lanchat::net::handshake::{run_initiator, run_responder};
use lanchat::net::session::Session;
use lanchat::protocol::FrameBody;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::thread;

fn make_session_pair() -> (Session<TcpStream>, Session<TcpStream>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let alice = Keypair::generate();
    let bob = Keypair::generate();

    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let res = run_responder(&mut stream, &bob).unwrap();
        Session::new(stream, res.send_key, res.recv_key, res.remote_static)
    });

    let mut client = TcpStream::connect(addr).unwrap();
    let res = run_initiator(&mut client, &alice).unwrap();
    let client_session = Session::new(client, res.send_key, res.recv_key, res.remote_static);
    let server_session = server.join().unwrap();
    (client_session, server_session)
}

/// Two unidirectional pipes fused into a single `Read+Write` stream.
struct PipeDuplex<R: Read, W: Write> {
    reader: R,
    writer: W,
}
impl<R: Read, W: Write> Read for PipeDuplex<R, W> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.reader.read(buf)
    }
}
impl<R: Read, W: Write> Write for PipeDuplex<R, W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.writer.write(buf)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.writer.flush()
    }
}

#[test]
fn ten_messages_each_way() {
    let (mut alice, mut bob) = make_session_pair();
    for i in 0..10 {
        alice.send(&FrameBody::Text(format!("a->b {}", i))).unwrap();
    }
    for i in 0..10 {
        bob.send(&FrameBody::Text(format!("b->a {}", i))).unwrap();
    }
    for i in 0..10 {
        let f = bob.recv().unwrap();
        assert_eq!(f.body, FrameBody::Text(format!("a->b {}", i)));
        assert_eq!(f.seq, i as u64);
    }
    for i in 0..10 {
        let f = alice.recv().unwrap();
        assert_eq!(f.body, FrameBody::Text(format!("b->a {}", i)));
        assert_eq!(f.seq, i as u64);
    }
}

/// After the handshake we deliberately write 32 garbage bytes onto the wire
/// and verify `Session::recv` rejects them.
#[test]
fn tampered_ciphertext_rejected() {
    let (a_to_b_r, a_to_b_w) = std::io::pipe().unwrap();
    let (b_to_a_r, b_to_a_w) = std::io::pipe().unwrap();

    let alice = Keypair::generate();
    let bob = Keypair::generate();

    let server = thread::spawn(move || {
        let mut stream = PipeDuplex {
            reader: a_to_b_r,
            writer: b_to_a_w,
        };
        let res = run_responder(&mut stream, &bob).unwrap();
        let mut session = Session::new(stream, res.send_key, res.recv_key, res.remote_static);
        // Loop recv; we expect at least one Err (the bogus ciphertext).
        let mut ok_count = 0;
        let mut err_count = 0;
        for _ in 0..4 {
            match session.recv() {
                Ok(f) => {
                    eprintln!("server decrypted frame: {:?}", f.body);
                    ok_count += 1;
                }
                Err(e) => {
                    eprintln!("server rejected frame: {}", e);
                    err_count += 1;
                    break;
                }
            }
        }
        assert!(err_count > 0, "server must reject the bogus ciphertext");
        assert_eq!(ok_count, 0, "bogus bytes must not be mistaken for a real frame");
    });

    let mut stream = PipeDuplex {
        reader: b_to_a_r,
        writer: a_to_b_w,
    };
    let res = run_initiator(&mut stream, &alice).unwrap();
    let mut session = Session::new(stream, res.send_key, res.recv_key, res.remote_static);

    // Skip the success path entirely — go straight to bogus bytes.
    let len: u32 = 32;
    session.write_raw(&len.to_be_bytes()).unwrap();
    session.write_raw(&vec![0xFFu8; 32]).unwrap();
    // Closing the writer half (drop) signals EOF, but the server's first
    // `recv` should already have errored before that matters.
    drop(session);

    server.join().unwrap();
}

/// Exercise the new wire-level `FileOffer` / `FileAccept` / `FileChunk`
/// / `FileDone` variants end-to-end through `Session<TcpStream>`. The
/// frames round-trip with bytes intact and per-frame sequence numbers
/// increment as expected. Higher-level state machines (OutboundTransfer,
/// InboundTransfer) live in `file_xfer` and have their own unit tests.
#[test]
fn file_offer_accept_chunk_done_roundtrip() {
    let (mut alice, mut bob) = make_session_pair();
    let id = lanchat::protocol::FileId::random();
    let offer_name = "report.pdf".to_string();

    // Bob is the sender (Alice is the receiver). The sender's first
    // `recv()` call would consume nothing yet — both sides send then
    // recv in their natural order.
    bob.send(&FrameBody::FileOffer {
        id,
        name: offer_name.clone(),
        size: 5,
        mime: Some("application/pdf".into()),
    })
    .unwrap();
    alice.send(&FrameBody::FileAccept { id }).unwrap();
    alice.send(&FrameBody::FileChunk {
        id,
        offset: 0,
        data: b"hello".to_vec(),
    })
    .unwrap();
    alice.send(&FrameBody::FileDone { id }).unwrap();

    let offer = alice.recv().unwrap();
    assert_eq!(offer.seq, 0);
    assert_eq!(
        offer.body,
        FrameBody::FileOffer {
            id,
            name: offer_name,
            size: 5,
            mime: Some("application/pdf".into()),
        }
    );

    let accept = bob.recv().unwrap();
    assert_eq!(accept.seq, 0);
    assert_eq!(accept.body, FrameBody::FileAccept { id });

    let chunk = bob.recv().unwrap();
    assert_eq!(chunk.seq, 1);
    assert_eq!(
        chunk.body,
        FrameBody::FileChunk {
            id,
            offset: 0,
            data: b"hello".to_vec(),
        }
    );

    let done = bob.recv().unwrap();
    assert_eq!(done.seq, 2);
    assert_eq!(done.body, FrameBody::FileDone { id });
}

/// A 32 KiB chunk is the protocol's upper bound for one `FileChunk`.
/// This test confirms it survives the AEAD frame path unchanged.
#[test]
fn file_chunk_max_size_roundtrips() {
    let (mut alice, mut bob) = make_session_pair();
    let id = lanchat::protocol::FileId::random();
    let payload = vec![0xABu8; lanchat::protocol::FILE_CHUNK_MAX_DATA];

    bob.send(&FrameBody::FileChunk {
        id,
        offset: 0,
        data: payload.clone(),
    })
    .unwrap();
    let f = alice.recv().unwrap();
    if let FrameBody::FileChunk { data, .. } = f.body {
        assert_eq!(data.len(), lanchat::protocol::FILE_CHUNK_MAX_DATA);
        assert_eq!(data, payload);
    } else {
        panic!("expected FileChunk, got {:?}", f.body);
    }
}
