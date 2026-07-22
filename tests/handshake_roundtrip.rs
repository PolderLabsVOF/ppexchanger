//! End-to-end handshake test using two real TCP sockets over loopback.
//!
//! Validates that:
//!   1. both sides complete the 3-message handshake without error,
//!   2. both sides derive IDENTICAL 32-byte send/recv keys,
//!   3. the static public keys are exchanged in full (so fingerprints match).

use lanchat::crypto::Keypair;
use lanchat::net::handshake::{run_initiator, run_responder};
use std::net::{TcpListener, TcpStream};
use std::thread;

#[test]
fn handshake_completes_and_keys_match() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();

    let alice = Keypair::generate();
    let bob = Keypair::generate();

    let alice_pub = alice.public_bytes();
    let bob_pub = bob.public_bytes();

    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        run_responder(&mut stream, &bob)
    });

    let mut client = TcpStream::connect(addr).unwrap();
    let res_init = run_initiator(&mut client, &alice).unwrap();
    let res_resp = server.join().unwrap().unwrap();

    assert_eq!(res_init.remote_static, bob_pub);
    assert_eq!(res_resp.remote_static, alice_pub);
    // Fingerprints must match the *corresponding* remote static keys —
    // not each other (each side's "remote" is the other party).
    let fp_init = lanchat::protocol::fingerprint(&bob_pub);
    let fp_resp = lanchat::protocol::fingerprint(&alice_pub);
    assert_eq!(res_init.remote_fingerprint, fp_init);
    assert_eq!(res_resp.remote_fingerprint, fp_resp);

    // Both sides must hold identical session keys — initiator's send == responder's recv
    // and vice versa.
    assert_eq!(
        res_init.send_key.as_slice(),
        res_resp.recv_key.as_slice(),
        "initiator send_key must equal responder recv_key"
    );
    assert_eq!(
        res_init.recv_key.as_slice(),
        res_resp.send_key.as_slice(),
        "initiator recv_key must equal responder send_key"
    );
}