//! Outbound dial: connect to a peer's TCP endpoint and complete the
//! initiator handshake, returning a ready-to-use `Session`.

use crate::crypto::Keypair;
use crate::net::handshake::run_initiator;
use crate::net::session::Session;
use std::net::{SocketAddr, TcpStream};

pub fn dial(addr: SocketAddr, static_kp: &Keypair) -> std::io::Result<Session<TcpStream>> {
    let mut stream = TcpStream::connect(addr)?;
    let res = run_initiator(&mut stream, static_kp)?;
    Ok(Session::new(
        stream,
        res.send_key,
        res.recv_key,
        res.remote_static,
    ))
}
