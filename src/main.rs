//! `lanchat` CLI entrypoint.
//!
//! Modes:
//!   * `--help`              print usage
//!   * `--version`           print version
//!   * `--gen-identity`      generate (or rotate) identity and exit
//!   * `--name <name>`       override display name for this run
//!   * `--port <port>`       override TCP listen port (default: 0 = ephemeral)
//!   * no flags              start the TUI

use lanchat::config::identity_path;
use lanchat::events::{Action, Bus, Event, PeerId};
use lanchat::identity::load_or_create;
use lanchat::net::discovery::Discovery;
use lanchat::net::listener::{self, AcceptedPeer};
use lanchat::net::peer;
use lanchat::net::session::Session;
use lanchat::peerdb::PeerDb;
use lanchat::protocol::{fingerprint as pubkey_fingerprint, Beacon};
use lanchat::tui::{self, UiState};
use std::collections::HashMap;
use std::net::{SocketAddr, TcpListener};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

const VERSION: &str = env!("CARGO_PKG_VERSION");

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let mut name: Option<String> = None;
    let mut port: u16 = 0;
    let mut mode = Mode::Tui;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--help" | "-h" => {
                print_help();
                return;
            }
            "--version" | "-V" => {
                println!("lanchat {}", VERSION);
                return;
            }
            "--name" => {
                i += 1;
                if i >= args.len() {
                    eprintln!("--name requires an argument");
                    std::process::exit(2);
                }
                name = Some(args[i].clone());
            }
            "--port" => {
                i += 1;
                if i >= args.len() {
                    eprintln!("--port requires an argument");
                    std::process::exit(2);
                }
                port = match args[i].parse() {
                    Ok(p) => p,
                    Err(_) => {
                        eprintln!("invalid --port value");
                        std::process::exit(2);
                    }
                };
            }
            "--gen-identity" => mode = Mode::GenIdentity,
            other => {
                eprintln!("unknown argument: {}", other);
                print_help();
                std::process::exit(2);
            }
        }
        i += 1;
    }
    run(mode, name, port);
}

enum Mode {
    Tui,
    GenIdentity,
}

fn print_help() {
    println!(
        "lanchat {version} — fully-local LAN P2P encrypted terminal messenger\n\
         \n\
         USAGE:\n  lanchat [--name <name>] [--port <port>]\n  lanchat --gen-identity\n  lanchat --help | --version\n\
         \n\
         OPTIONS:\n  --name <name>     display name (overrides stored)\n  --port <port>     TCP listen port (0 = ephemeral)\n  --gen-identity    generate a new identity and exit\n  --help, -h        print this help\n  --version, -V     print version",
        version = VERSION
    );
}

fn run(mode: Mode, name: Option<String>, port: u16) {
    let id = load_or_create(name).unwrap_or_else(|e| {
        eprintln!("failed to load identity: {}", e);
        std::process::exit(1);
    });
    match mode {
        Mode::GenIdentity => {
            println!(
                "identity ready\n  peer_id: {}\n  public_key: {}\n  fingerprint: {}\n  file: {}",
                hex(&id.peer_id),
                hex(&id.keypair.public_bytes()),
                pubkey_fingerprint(&id.keypair.public_bytes()),
                identity_path().unwrap().display()
            );
            return;
        }
        Mode::Tui => {}
    }
    start_tui(id, port);
}

fn hex(b: &[u8]) -> String {
    let mut s = String::with_capacity(b.len() * 2);
    for x in b {
        s.push_str(&format!("{:02x}", x));
    }
    s
}

fn start_tui(id: lanchat::identity::Identity, port: u16) {
    let bus = Bus::new();
    let state = Arc::new(Mutex::new(UiState::from_identity(&id)));

    // Load persistent contacts and seed the UI.
    let mut db = PeerDb::load_or_default().unwrap_or_default();
    {
        let mut s = state.lock().unwrap();
        tui::merge_contacts(&mut s, &db);
        s.status = format!(
            "identity: {} ({})",
            id.name,
            pubkey_fingerprint(&id.keypair.public_bytes())
        );
    }

    // Bind TCP listener on the requested port.
    let listener = match listener::bind(port) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("listener bind failed: {}", e);
            std::process::exit(1);
        }
    };
    let bound_port = listener.local_addr().map(|a| a.port()).unwrap_or(0);
    bus.tx_events
        .send(Event::Info(format!("listening on 0.0.0.0:{}", bound_port)))
        .ok();

    // Bind UDP discovery socket.
    let discovery = match Discovery::bind(0) {
        Ok(d) => Arc::new(d),
        Err(e) => {
            eprintln!("multicast bind failed: {}", e);
            std::process::exit(1);
        }
    };

    let stop = Arc::new(AtomicBool::new(false));

    // Build the announce beacon before we move keypair out of `id`.
    let announce_beacon = make_beacon(&id, bound_port);

    // Wrap the static keypair in Arc so listener/handshake threads can share
    // it without cloning the inner struct (Keypair is intentionally not Clone).
    let static_kp: Arc<lanchat::crypto::Keypair> = Arc::new(id.keypair);

    // Listener thread: accepts inbound TCP, runs responder handshake, posts
    // AcceptedPeer to the bus.
    let listener_t = {
        let tx = bus.tx_events.clone();
        let kp = Arc::clone(&static_kp);
        let stop2 = Arc::clone(&stop);
        thread::spawn(move || {
            let _ = stop2; // currently unused — listener runs until drop
            loop {
                match listener.accept() {
                    Ok((stream, addr)) => {
                        let kp2 = Arc::clone(&kp);
                        let tx2 = tx.clone();
                        thread::spawn(move || {
                            let mut s = stream;
                            match lanchat::net::handshake::run_responder(&mut s, &kp2) {
                                Ok(res) => {
                                    let session = Session::new(
                                        s,
                                        res.send_key,
                                        res.recv_key,
                                        res.remote_static,
                                    );
                                    let _ = tx2.send(Event::Info(format!(
                                        "inbound peer from {} (fp {})",
                                        addr, res.remote_fingerprint
                                    )));
                                    let (event, _peer_session) = AcceptedPeer {
                                        remote_addr: addr,
                                        remote_static: res.remote_static,
                                        remote_fingerprint: res.remote_fingerprint.clone(),
                                        session,
                                    }
                                    .into_event();
                                    let _ = tx2.send(event);
                                }
                                Err(_e) => {}
                            }
                        });
                    }
                    Err(_) => {
                        thread::sleep(Duration::from_millis(50));
                    }
                }
            }
        })
    };

    // Announcer thread.
    let ann_disc = Arc::clone(&discovery);
    let ann_stop = Arc::clone(&stop);
    let announce_thread = {
        thread::spawn(move || {
            let _ = ann_disc.announce_loop(announce_beacon, &ann_stop);
        })
    };

    // Receiver thread.
    let recv_disc = Arc::clone(&discovery);
    let recv_stop = Arc::clone(&stop);
    let recv_bus_tx = bus.tx_events.clone();
    let recv_thread = thread::spawn(move || {
        while !recv_stop.load(Ordering::Relaxed) {
            if let Ok(Some((src, b))) = recv_disc.recv_beacon() {
                if b.peer_id == id.peer_id {
                    continue;
                }
                // Trust the beacon's source address for the TCP port it announced.
                let tcp_addr: SocketAddr = (src.ip(), b.tcp_port).into();
                let _ = recv_bus_tx.send(Event::PeerSeen {
                    peer_id: b.peer_id,
                    name: b.name,
                    public_key: b.public_key,
                    fingerprint: pubkey_fingerprint(&b.public_key),
                    addr: tcp_addr,
                });
            }
        }
    });

    // Action consumer thread.
    let act_stop = Arc::clone(&stop);
    let act_bus_tx = bus.tx_events.clone();
    let act_bus_rx = bus.rx_actions; // moved in
    let act_state = Arc::clone(&state);
    let act_thread = {
        let kp = Arc::clone(&static_kp);
        thread::spawn(move || {
            let mut _known: HashMap<PeerId, SocketAddr> = HashMap::new();
            while !act_stop.load(Ordering::Relaxed) {
                match act_bus_rx.recv_timeout(Duration::from_millis(200)) {
                    Ok(Action::Connect {
                        addr,
                        name_hint,
                        public_key: _,
                    }) => {
                        if let Ok(sess) = peer::dial(addr, &kp) {
                            let fp = pubkey_fingerprint(&sess.remote_static);
                            let peer_id = lanchat::net::listener::peer_id_from_pubkey(&sess.remote_static);
                            let _ = act_bus_tx.send(Event::PeerConnected {
                                peer_id,
                                name: name_hint,
                                fingerprint: fp,
                                trusted: false,
                                addr,
                            });
                            spawn_session_reader(sess, addr, act_bus_tx.clone());
                        }
                    }
                    Ok(Action::Trust { peer_id }) => {
                        let mut s = act_state.lock().unwrap();
                        if let Some(p) = s.peers.iter_mut().find(|p| p.peer_id == peer_id) {
                            p.trusted = true;
                        }
                        let mut db = PeerDb::default();
                        db.trust(&peer_id);
                        let _ = db.save();
                    }
                    Ok(Action::Disconnect { peer_id: _ }) => {
                        // For v1, peer disconnects are handled via PeerGone events;
                        // we don't actively close sessions here yet.
                    }
                    Ok(Action::Revoke { peer_id }) => {
                        let mut s = act_state.lock().unwrap();
                        s.peers.retain(|p| p.peer_id != peer_id);
                        let mut db = PeerDb::default();
                        db.revoke(&peer_id);
                        let _ = db.save();
                    }
                    Ok(Action::SendText { to, body }) => {
                        let s = act_state.lock().unwrap();
                        let _ = (to, body, s);
                    }
                    Ok(Action::Quit) => {
                        act_stop.store(true, Ordering::Relaxed);
                        break;
                    }
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
                    Err(_) => break,
                }
            }
        })
    };

    // TUI loop.
    let _guard = tui::TuiGuard::new().unwrap();
    let mut terminal = tui::enter_terminal().unwrap();
    let mut editor = lanchat::tui::input::LineEditor::new();

    loop {
        {
            let mut s = state.lock().unwrap();
            tui::drain_events(&bus.rx_events, &mut s);
        }
        {
            let s = state.lock().unwrap();
            if let Err(e) = tui::render(&mut terminal, &s) {
                eprintln!("render error: {}", e);
                break;
            }
        }
        if crossterm::event::poll(Duration::from_millis(150)).unwrap_or(false) {
            if let Ok(ev) = crossterm::event::read() {
                if let Some(text) = editor.on_key(&ev) {
                    if text == "\x03" {
                        let _ = bus.tx_actions.send(Action::Quit);
                        break;
                    }
                    if text.starts_with('/') {
                        handle_command(&text, &bus.tx_events, &bus.tx_actions, &state, &mut db);
                    } else if !text.is_empty() {
                        let target = {
                            let s = state.lock().unwrap();
                            s.peers
                                .iter()
                                .find(|p| p.state == tui::PeerState::Connected)
                                .map(|p| p.peer_id)
                        };
                        if let Some(to) = target {
                            let _ = bus.tx_actions.send(Action::SendText { to, body: text });
                        }
                    }
                }
                let mut s = state.lock().unwrap();
                s.status = format!("> {}", editor.as_str());
            }
        }
        if stop.load(Ordering::Relaxed) {
            break;
        }
    }

    stop.store(true, Ordering::Relaxed);
    let _ = announce_thread.join();
    let _ = recv_thread.join();
    let _ = listener_t.join();
    let _ = act_thread.join();
}

fn make_beacon(id: &lanchat::identity::Identity, tcp_port: u16) -> Beacon {
    Beacon {
        peer_id: id.peer_id,
        public_key: id.keypair.public_bytes(),
        tcp_port,
        name: id.name.clone(),
    }
}

fn spawn_session_reader(
    mut sess: Session<std::net::TcpStream>,
    addr: SocketAddr,
    tx: std::sync::mpsc::Sender<Event>,
) {
    use lanchat::protocol::FrameBody;
    let peer_id = lanchat::net::listener::peer_id_from_pubkey(&sess.remote_static);
    thread::spawn(move || loop {
        match sess.recv() {
            Ok(frame) => {
                let body = match frame.body {
                    FrameBody::Text(s) => s,
                    FrameBody::Bye => {
                        let _ = tx.send(Event::PeerGone {
                            peer_id,
                            name: format!("peer@{}", addr),
                        });
                        break;
                    }
                };
                let _ = tx.send(Event::TextMessage {
                    from_peer: peer_id,
                    from_name: format!("peer@{}", addr),
                    body,
                });
            }
            Err(_) => {
                let _ = tx.send(Event::PeerGone {
                    peer_id,
                    name: format!("peer@{}", addr),
                });
                break;
            }
        }
    });
}

fn handle_command(
    line: &str,
    tx_events: &std::sync::mpsc::Sender<Event>,
    tx_actions: &std::sync::mpsc::Sender<Action>,
    state: &Arc<Mutex<UiState>>,
    db: &mut PeerDb,
) {
    let mut it = line.split_whitespace();
    let cmd = it.next().unwrap_or("");
    match cmd {
        "/peers" => {
            let s = state.lock().unwrap();
            for p in &s.peers {
                let _ = tx_events.send(Event::Info(format!(
                    "{} {} fp={} state={:?}",
                    p.name,
                    if p.trusted { "(trusted)" } else { "(untrusted)" },
                    p.fingerprint,
                    p.state
                )));
            }
        }
        "/trust" => {
            if let Some(name) = it.next() {
                let pid = {
                    let s = state.lock().unwrap();
                    s.peers.iter().find(|p| p.name == name).map(|p| p.peer_id)
                };
                if let Some(pid) = pid {
                    let _ = tx_actions.send(Action::Trust { peer_id: pid });
                    let _ = db.save();
                }
            }
        }
        "/revoke" => {
            if let Some(name) = it.next() {
                let pid = {
                    let s = state.lock().unwrap();
                    s.peers.iter().find(|p| p.name == name).map(|p| p.peer_id)
                };
                if let Some(pid) = pid {
                    let _ = tx_actions.send(Action::Revoke { peer_id: pid });
                    let _ = db.save();
                }
            }
        }
        "/quit" => {
            let _ = tx_actions.send(Action::Quit);
        }
        _ => {
            let _ = tx_events.send(Event::Info(format!("unknown command: {}", cmd)));
        }
    }
}