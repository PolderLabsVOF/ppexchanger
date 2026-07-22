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
