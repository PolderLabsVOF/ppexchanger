//! Privacy-preserving TCP rendezvous relay transport.
//!
//! The relay never decrypts or interprets ppx traffic.  Clients first send the
//! fixed registration record `PPXRLY01 || room_id` (8 + 32 bytes).  `room_id`
//! is an opaque SHA-256 digest derived from a high-entropy invitation secret by
//! the client.  The relay holds the first socket for a room and, when a second
//! arrives, replies with one role byte (`1` initiator, `2` responder) then
//! copies bytes in both directions.  A third socket for an active room is
//! rejected.  No registration values or payload bytes are logged or written to
//! disk.

use crate::crypto::{sha256, Keypair};
use crate::net::handshake::{run_initiator, run_responder};
use crate::net::session::Session;
use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::net::{IpAddr, Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

const REG_MAGIC: &[u8; 8] = b"PPXRLY01";
const REG_LEN: usize = 40;
const ROLE_INITIATOR: u8 = 1;
const ROLE_RESPONDER: u8 = 2;
const POLL: Duration = Duration::from_millis(20);
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
const READY_TIMEOUT: Duration = Duration::from_secs(30);
const COPY_BUFFER: usize = 16 * 1024;

/// Resource limits for a self-hosted relay.
#[derive(Clone, Debug)]
pub struct RelayOptions {
    pub max_clients: usize,
    pub max_per_ip: usize,
    pub waiting_timeout: Duration,
    pub idle_timeout: Duration,
}

impl Default for RelayOptions {
    fn default() -> Self {
        Self {
            max_clients: 128,
            max_per_ip: 16,
            waiting_timeout: Duration::from_secs(60),
            idle_timeout: Duration::from_secs(10 * 60),
        }
    }
}

struct Waiting {
    stream: TcpStream,
    ip: IpAddr,
    since: Instant,
}

struct Registration {
    stream: TcpStream,
    ip: IpAddr,
    room: [u8; 32],
}

/// Run a bounded relay until `stop` becomes true.
///
/// The supplied listener is switched to nonblocking mode.  Admission is capped
/// before registration workers are spawned, and every path decrements its
/// admission count.  Relay threads are limited to admitted sockets.
pub fn serve(
    listener: TcpListener,
    options: RelayOptions,
    stop: Arc<AtomicBool>,
) -> io::Result<()> {
    listener.set_nonblocking(true)?;
    let (registered_tx, registered_rx) = mpsc::channel::<Option<Registration>>();
    let (finished_tx, finished_rx) = mpsc::channel::<[u8; 32]>();
    let active = Arc::new(AtomicUsize::new(0));
    let ip_counts = Arc::new(Mutex::new(HashMap::<IpAddr, usize>::new()));
    let mut waiting: HashMap<[u8; 32], Waiting> = HashMap::new();
    let mut paired: HashMap<[u8; 32], ()> = HashMap::new();
    let mut pair_threads: Vec<thread::JoinHandle<()>> = Vec::new();

    while !stop.load(Ordering::Relaxed) {
        expire_waiting(&mut waiting, &ip_counts, &active, options.waiting_timeout);
        while let Ok(room) = finished_rx.try_recv() {
            paired.remove(&room);
        }
        let mut index = 0;
        while index < pair_threads.len() {
            if pair_threads[index].is_finished() {
                let _ = pair_threads.swap_remove(index).join();
            } else {
                index += 1;
            }
        }
        while let Ok(Some(reg)) = registered_rx.try_recv() {
            if paired.contains_key(&reg.room) {
                drop_socket(reg.stream, reg.ip, &ip_counts, &active);
                continue;
            }
            if let Some(first) = waiting.remove(&reg.room) {
                paired.insert(reg.room, ());
                let room = reg.room;
                let first_ip = first.ip;
                let second_ip = reg.ip;
                let active_for_pair = active.clone();
                let counts_for_pair = ip_counts.clone();
                let stop_for_pair = stop.clone();
                let finished_tx = finished_tx.clone();
                let idle_timeout = options.idle_timeout;
                pair_threads.push(thread::spawn(move || {
                    pair_and_forward(first, reg, stop_for_pair, idle_timeout);
                    paired_finished(active_for_pair, counts_for_pair, first_ip, second_ip);
                    let _ = finished_tx.send(room);
                }));
            } else {
                waiting.insert(
                    reg.room,
                    Waiting {
                        stream: reg.stream,
                        ip: reg.ip,
                        since: Instant::now(),
                    },
                );
            }
        }

        match listener.accept() {
            Ok((stream, addr)) => {
                let ip = addr.ip();
                if !try_admit(ip, &active, &ip_counts, &options) {
                    let _ = stream.shutdown(Shutdown::Both);
                    continue;
                }
                let tx = registered_tx.clone();
                let active = active.clone();
                let counts = ip_counts.clone();
                thread::spawn(move || {
                    let result = read_registration(stream, ip).ok();
                    if result.is_none() {
                        release(ip, &active, &counts);
                    }
                    let _ = tx.send(result);
                });
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => thread::sleep(POLL),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }

    for (_, waiting) in waiting {
        let _ = waiting.stream.shutdown(Shutdown::Both);
        release(waiting.ip, &active, &ip_counts);
    }
    // Pair workers use one-second read timeouts and observe `stop`, so joining
    // here bounds shutdown without leaving socket-copy threads behind.
    for worker in pair_threads {
        let _ = worker.join();
    }
    Ok(())
}

fn read_registration(mut stream: TcpStream, ip: IpAddr) -> io::Result<Registration> {
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    let mut record = [0u8; REG_LEN];
    stream.read_exact(&mut record)?;
    if &record[..8] != REG_MAGIC {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid relay registration",
        ));
    }
    let room: [u8; 32] = record[8..]
        .try_into()
        .expect("fixed registration room length");
    Ok(Registration { stream, ip, room })
}

fn try_admit(
    ip: IpAddr,
    active: &AtomicUsize,
    counts: &Mutex<HashMap<IpAddr, usize>>,
    options: &RelayOptions,
) -> bool {
    loop {
        let current = active.load(Ordering::Acquire);
        if current >= options.max_clients {
            return false;
        }
        if active
            .compare_exchange(current, current + 1, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            break;
        }
    }
    let mut counts = counts.lock().expect("relay IP count mutex poisoned");
    let entry = counts.entry(ip).or_default();
    if *entry >= options.max_per_ip {
        active.fetch_sub(1, Ordering::AcqRel);
        return false;
    }
    *entry += 1;
    true
}

fn release(ip: IpAddr, active: &AtomicUsize, counts: &Mutex<HashMap<IpAddr, usize>>) {
    active.fetch_sub(1, Ordering::AcqRel);
    let mut counts = counts.lock().expect("relay IP count mutex poisoned");
    if let Some(value) = counts.get_mut(&ip) {
        *value -= 1;
        if *value == 0 {
            counts.remove(&ip);
        }
    }
}

fn drop_socket(
    stream: TcpStream,
    ip: IpAddr,
    counts: &Mutex<HashMap<IpAddr, usize>>,
    active: &AtomicUsize,
) {
    let _ = stream.shutdown(Shutdown::Both);
    release(ip, active, counts);
}

fn expire_waiting(
    waiting: &mut HashMap<[u8; 32], Waiting>,
    counts: &Mutex<HashMap<IpAddr, usize>>,
    active: &AtomicUsize,
    timeout: Duration,
) {
    let expired: Vec<_> = waiting
        .iter()
        .filter_map(|(room, entry)| (entry.since.elapsed() >= timeout).then_some(*room))
        .collect();
    for room in expired {
        if let Some(entry) = waiting.remove(&room) {
            drop_socket(entry.stream, entry.ip, counts, active);
        }
    }
}

fn paired_finished(
    active: Arc<AtomicUsize>,
    counts: Arc<Mutex<HashMap<IpAddr, usize>>>,
    first_ip: IpAddr,
    second_ip: IpAddr,
) {
    release(first_ip, &active, &counts);
    release(second_ip, &active, &counts);
    // The serve loop removes the active-room guard after this completion.
}

fn pair_and_forward(
    mut first: Waiting,
    mut second: Registration,
    stop: Arc<AtomicBool>,
    idle_timeout: Duration,
) {
    if first.stream.write_all(&[ROLE_INITIATOR]).is_err()
        || second.stream.write_all(&[ROLE_RESPONDER]).is_err()
    {
        let _ = first.stream.shutdown(Shutdown::Both);
        let _ = second.stream.shutdown(Shutdown::Both);
        return;
    }
    let first_read = match first.stream.try_clone() {
        Ok(s) => s,
        Err(_) => return,
    };
    let second_read = match second.stream.try_clone() {
        Ok(s) => s,
        Err(_) => return,
    };
    let first_shutdown = match first.stream.try_clone() {
        Ok(s) => s,
        Err(_) => return,
    };
    let second_shutdown = match second.stream.try_clone() {
        Ok(s) => s,
        Err(_) => return,
    };
    let stop_back = stop.clone();
    let back = thread::spawn(move || {
        copy_until_closed(second_read, first.stream, stop_back, idle_timeout)
    });
    copy_until_closed(first_read, second.stream, stop, idle_timeout);
    let _ = first_shutdown.shutdown(Shutdown::Both);
    let _ = second_shutdown.shutdown(Shutdown::Both);
    let _ = back.join();
}

fn copy_until_closed(
    mut source: TcpStream,
    mut destination: TcpStream,
    stop: Arc<AtomicBool>,
    idle_timeout: Duration,
) {
    let _ = source.set_read_timeout(Some(Duration::from_secs(1)));
    let _ = destination.set_write_timeout(Some(Duration::from_secs(5)));
    let mut buf = [0u8; COPY_BUFFER];
    let mut last_data = Instant::now();
    loop {
        if stop.load(Ordering::Relaxed) || last_data.elapsed() >= idle_timeout {
            break;
        }
        match source.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                if destination.write_all(&buf[..n]).is_err() {
                    break;
                }
                last_data = Instant::now();
            }
            Err(e)
                if e.kind() == io::ErrorKind::WouldBlock || e.kind() == io::ErrorKind::TimedOut => {
            }
            Err(_) => break,
        }
    }
}

/// Connect through a relay and run the regular end-to-end encrypted ppx
/// handshake. `room` is a 32-byte invitation secret, never sent directly.
pub fn connect(
    addr: SocketAddr,
    room: &[u8; 32],
    expected_peer: &[u8; 32],
    local_key: &Keypair,
) -> io::Result<Session<TcpStream>> {
    if expected_peer == &local_key.public_bytes() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "cannot relay-connect to yourself",
        ));
    }
    let mut stream = TcpStream::connect_timeout(&addr, HANDSHAKE_TIMEOUT)?;
    stream.set_nodelay(true)?;
    stream.set_read_timeout(Some(READY_TIMEOUT))?;
    stream.set_write_timeout(Some(HANDSHAKE_TIMEOUT))?;
    let mut route_input = Vec::with_capacity(16 + room.len());
    route_input.extend_from_slice(b"ppexchanger-relay-room-v1");
    route_input.extend_from_slice(room);
    let route = sha256(&route_input);
    stream.write_all(REG_MAGIC)?;
    stream.write_all(&route)?;
    let mut role = [0u8; 1];
    stream.read_exact(&mut role)?;
    stream.set_read_timeout(Some(HANDSHAKE_TIMEOUT))?;
    let result = match role[0] {
        ROLE_INITIATOR => run_initiator(&mut stream, local_key),
        ROLE_RESPONDER => run_responder(&mut stream, local_key),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid relay role",
        )),
    }?;
    if &result.remote_static != expected_peer {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "relay peer identity did not match invitation",
        ));
    }
    Ok(Session::new(
        stream,
        result.send_key,
        result.recv_key,
        result.remote_static,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::FrameBody;

    fn start(
        options: RelayOptions,
    ) -> (
        SocketAddr,
        Arc<AtomicBool>,
        thread::JoinHandle<io::Result<()>>,
    ) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = stop.clone();
        let handle = thread::spawn(move || serve(listener, options, thread_stop));
        (address, stop, handle)
    }

    #[test]
    fn pairs_encrypted_sessions_both_directions() {
        let (addr, stop, handle) = start(RelayOptions::default());
        let a = Keypair::generate();
        let b = Keypair::generate();
        let room = [7; 32];
        let a_public = a.public_bytes();
        let b_public = b.public_bytes();
        let join = thread::spawn(move || connect(addr, &room, &b_public, &a));
        let mut b_session = connect(addr, &room, &a_public, &b).unwrap();
        let mut a_session = join.join().unwrap().unwrap();
        a_session.send(&FrameBody::Text("hello".into())).unwrap();
        assert!(
            matches!(b_session.recv().unwrap().body, FrameBody::Text(ref text) if text == "hello")
        );
        b_session.send(&FrameBody::Text("back".into())).unwrap();
        assert!(
            matches!(a_session.recv().unwrap().body, FrameBody::Text(ref text) if text == "back")
        );
        stop.store(true, Ordering::Relaxed);
        handle.join().unwrap().unwrap();
    }

    #[test]
    fn wrong_expected_key_is_rejected() {
        let (addr, stop, handle) = start(RelayOptions::default());
        let a = Keypair::generate();
        let b = Keypair::generate();
        let room = [3; 32];
        let b_public = b.public_bytes();
        let join = thread::spawn(move || connect(addr, &room, &b_public, &a));
        let wrong = Keypair::generate().public_bytes();
        // The endpoint that pins the wrong identity must refuse the tunnel;
        // the other endpoint cannot learn that decision from a raw relay.
        assert!(connect(addr, &room, &wrong, &b).is_err());
        assert!(join.join().unwrap().is_ok());
        stop.store(true, Ordering::Relaxed);
        handle.join().unwrap().unwrap();
    }

    #[test]
    fn waiting_expiry_releases_admission_capacity() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let client = TcpStream::connect(addr).unwrap();
        let (stream, peer) = listener.accept().unwrap();
        let active = AtomicUsize::new(1);
        let counts = Mutex::new(HashMap::from([(peer.ip(), 1usize)]));
        let mut waiting = HashMap::from([(
            [9; 32],
            Waiting {
                stream,
                ip: peer.ip(),
                since: Instant::now() - Duration::from_secs(1),
            },
        )]);
        expire_waiting(&mut waiting, &counts, &active, Duration::from_millis(1));
        assert!(waiting.is_empty());
        assert_eq!(active.load(Ordering::Relaxed), 0);
        assert!(counts.lock().unwrap().is_empty());
        drop(client);
    }

    #[test]
    fn admission_cap_rejects_extra_waiter() {
        let (addr, stop, handle) = start(RelayOptions {
            max_clients: 1,
            ..RelayOptions::default()
        });
        let mut first = TcpStream::connect(addr).unwrap();
        first.write_all(REG_MAGIC).unwrap();
        first.write_all(&[1; 32]).unwrap();
        thread::sleep(Duration::from_millis(40));
        let mut extra = TcpStream::connect(addr).unwrap();
        extra
            .set_read_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        extra.write_all(REG_MAGIC).unwrap();
        extra.write_all(&[2; 32]).unwrap();
        let mut byte = [0; 1];
        assert_eq!(extra.read(&mut byte).unwrap_or(0), 0);
        stop.store(true, Ordering::Relaxed);
        handle.join().unwrap().unwrap();
    }

    #[test]
    fn malformed_registration_and_shutdown_do_not_hang() {
        let (addr, stop, handle) = start(RelayOptions {
            waiting_timeout: Duration::from_millis(20),
            ..RelayOptions::default()
        });
        let mut raw = TcpStream::connect(addr).unwrap();
        raw.write_all(b"not-a-relay-registration").unwrap();
        thread::sleep(Duration::from_millis(40));
        stop.store(true, Ordering::Relaxed);
        handle.join().unwrap().unwrap();
    }
}
