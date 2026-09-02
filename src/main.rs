//! `ppexchanger` CLI entrypoint.
//!
//! Modes:
//!   * `--help`              print usage
//!   * `--version`           print version
//!   * `--gen-identity`      generate (or rotate) identity and exit
//!   * `--name <name>`       override display name for this run
//!   * `--port <port>`       override TCP listen port (default: 0 = ephemeral)
//!   * `--theme <name>`      override theme for this run
//!   * `--config <path>`     override config path
//!   * `--no-mouse`          disable mouse capture
//!   * no flags              start the TUI

use ppexchanger::config::{config_dir, history_path, identity_path};
use ppexchanger::tui::config::StatusFormat;
use ppexchanger::events::{Action, Bus, Event, PeerId, RegistryMsg};
use ppexchanger::identity::load_or_create;
use ppexchanger::net::discovery::Discovery;
use ppexchanger::net::listener;
use ppexchanger::net::peer;
use ppexchanger::net::session::Session;
use ppexchanger::peerdb::PeerDb;
use ppexchanger::protocol::{fingerprint as pubkey_fingerprint, Beacon, FrameBody};
use ppexchanger::tui::{self, PeerState, UiConfig, UiState};
use std::collections::{HashMap, VecDeque};
use std::io::{Read, Write};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

const VERSION: &str = env!("CARGO_PKG_VERSION");

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let mut name: Option<String> = None;
    // Default port is 7777 (the documented multicast port) so two
    // machines running the TUI with no flags can find each other via
    // the TCP subnet scan fallback. `0` is still accepted for ephemeral
    // binding (mostly useful for tests).
    let mut port: u16 = ppexchanger::net::discovery::MULTICAST_PORT;
    let mut mode = Mode::Tui;
    let mut theme_override: Option<ppexchanger::tui::ThemeName> = None;
    let mut config_override: Option<PathBuf> = None;
    let mut mouse_override: Option<bool> = None;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--help" | "-h" => {
                print_help();
                return;
            }
            "--version" | "-V" => {
                println!("ppexchanger {}", VERSION);
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
            "--theme" => {
                i += 1;
                if i >= args.len() {
                    eprintln!("--theme requires an argument");
                    std::process::exit(2);
                }
                match ppexchanger::tui::ThemeName::parse(&args[i]) {
                    Some(t) => theme_override = Some(t),
                    None => {
                        eprintln!("unknown theme: {}", args[i]);
                        std::process::exit(2);
                    }
                }
            }
            "--config" => {
                i += 1;
                if i >= args.len() {
                    eprintln!("--config requires an argument");
                    std::process::exit(2);
                }
                config_override = Some(PathBuf::from(&args[i]));
            }
            "--no-mouse" => mouse_override = Some(false),
            "--gen-identity" => mode = Mode::GenIdentity,
            other => {
                eprintln!("unknown argument: {}", other);
                print_help();
                std::process::exit(2);
            }
        }
        i += 1;
    }
    run(
        mode,
        name,
        port,
        theme_override,
        config_override,
        mouse_override,
    );
}

enum Mode {
    Tui,
    GenIdentity,
}

fn print_help() {
    println!(
        "ppexchanger {version} — fully-local LAN P2P encrypted terminal messenger\n\
         \n\
         USAGE:\n  ppx [--name <name>] [--port <port>] [--theme <name>] [--config <path>] [--no-mouse]\n  ppx --gen-identity\n  ppx --help | --version\n\
         \n\
         OPTIONS:\n  --name <name>     display name (overrides stored)\n  --port <port>     TCP listen port (0 = ephemeral)\n  --theme <name>    default|solarized|monochrome|neon|amber\n  --config <path>   path to config.toml (default: $XDG_CONFIG_HOME/ppexchanger/config.toml on
                    Linux/macOS, %APPDATA%\\ppexchanger\\config.toml on Windows)\n  --no-mouse        disable mouse capture (mouse is ON by default)\n  --gen-identity    generate a new identity and exit\n  --help, -h        print this help\n  --version, -V     print version",
        version = VERSION
    );
}

fn run(
    mode: Mode,
    name: Option<String>,
    port: u16,
    theme_override: Option<ppexchanger::tui::ThemeName>,
    config_override: Option<PathBuf>,
    mouse_override: Option<bool>,
) {
    // First-run migration from the v0.4.x `lanchat/` config dir. Best-effort:
    // a permission error just prints to stderr and we continue with the new
    // (empty) dir. Idempotent — a no-op once the files have been copied.
    if let Ok(true) = ppexchanger::config::migrate_legacy_config() {
        eprintln!(
            "migrated legacy lanchat config → {}",
            ppexchanger::config::config_dir()
                .map(|d| d.display().to_string())
                .unwrap_or_else(|_| "<unknown>".into())
        );
    }
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
    start_tui(id, port, theme_override, config_override, mouse_override);
}

fn hex(b: &[u8]) -> String {
    let mut s = String::with_capacity(b.len() * 2);
    for x in b {
        s.push_str(&format!("{:02x}", x));
    }
    s
}

/// Persist the current UI config to disk. Best-effort: a permission error
/// just posts an Event::Info warning instead of crashing the TUI.
fn save_ui_config(cfg: &ppexchanger::tui::UiConfig, path: &std::path::Path) -> std::io::Result<()> {
    let body = format_ui_config(cfg);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, body)
}

fn format_ui_config(cfg: &ppexchanger::tui::UiConfig) -> String {
    cfg.to_toml()
}

fn default_config_path() -> PathBuf {
    config_dir().map(|d| d.join("config.toml")).unwrap_or_default()
}

fn start_tui(
    id: ppexchanger::identity::Identity,
    port: u16,
    theme_override: Option<ppexchanger::tui::ThemeName>,
    config_override: Option<PathBuf>,
    mouse_override: Option<bool>,
) {
    // Load config: explicit flag > default path > builtin defaults.
    let cfg_path = config_override.unwrap_or_else(default_config_path);
    let mut ui_cfg = UiConfig::load_or_default(&cfg_path);
    if let Some(t) = theme_override {
        ui_cfg.theme = t;
    }
    if let Some(m) = mouse_override {
        ui_cfg.mouse = m;
    }

    let theme = ppexchanger::tui::Theme::by_name(ui_cfg.theme);
    let glyphs = ppexchanger::tui::detect_glyphs();

    let bus = Bus::new();
    let history_path = history_path().ok();
    let history_secret = id.secret_bytes();
    let history_peer_id = id.peer_id;
    let state = Arc::new(Mutex::new({
        let mut s = UiState::from_identity(&id);
        s.max_scrollback = ui_cfg.scrollback;
        s
    }));

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
        if let Some(path) = history_path.as_ref() {
            match ppexchanger::chat_history::load(path, &history_secret, &history_peer_id) {
                Ok(Some(history)) => s.restore_history(history),
                Ok(None) => {}
                Err(error) => {
                    s.status = format!("chat history unavailable: {}", error);
                }
            }
        }
    }
    let initial_pending_text: Vec<(PeerId, String)> = state
        .lock()
        .unwrap()
        .messages
        .iter()
        .filter(|message| message.outgoing && message.pending)
        .map(|message| (message.from_peer, message.body.clone()))
        .collect();

    // Bind TCP listener on the requested port.
    let listener = match listener::bind(port) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("listener bind failed: {}", e);
            std::process::exit(1);
        }
    };
    let bound_port = listener.local_addr().map(|a| a.port()).unwrap_or(0);
    // Plumb the bound port into the UI state so the discovery-empty
    // hint can render the exact firewall one-liner the user needs to
    // run (port must match what `ppx` actually bound).
    state.lock().unwrap().bound_port = bound_port;
    bus.tx_events
        .send(Event::Info(format!("listening on 0.0.0.0:{}", bound_port)))
        .ok();

    // Keep inbound connectivity working on hosts with a default-deny
    // firewall. This runs before the terminal switches to raw mode so an
    // interactive sudo prompt remains visible and usable. The helper is
    // idempotent and never enables the firewall or removes unrelated rules.
    match ppexchanger::net::firewall::ensure_rules(
        bound_port,
        ppexchanger::net::discovery::CONTROL_PORT,
    ) {
        Ok(Some(message)) => {
            let _ = bus.tx_events.send(Event::Info(message));
        }
        Ok(None) => {}
        Err(error) => {
            let _ = bus.tx_events.send(Event::Info(format!(
                "firewall setup skipped: {}",
                error
            )));
        }
    }

    let stop = Arc::new(AtomicBool::new(false));

    // Registry channel: the inbound listener uses it to hand outbound
    // senders (for newly-accepted sessions) over to the action consumer,
    // which owns the registry and routes outbound messages through it.
    let (reg_tx, reg_rx) = mpsc::channel::<RegistryMsg>();

    // Build the announce beacon once; it's reused on every `/discover`.
    let announce_beacon = make_beacon(&id, bound_port);
    // Keep a copy of our peer_id for discovery filtering (so we ignore our
    // own beacon if it loops back).
    let self_peer_id = id.peer_id;

    // Wrap the static keypair in Arc so listener/handshake threads can share
    // it without cloning the inner struct (Keypair is intentionally not Clone).
    let static_kp: Arc<ppexchanger::crypto::Keypair> = Arc::new(id.keypair);
    let local_name = id.name.clone();
    let local_hostname = id.hostname.clone();

    // Announcer thread: continuously broadcasts our presence so peers running
    // `/discover` can find us even if we haven't initiated discovery ourselves.
    // This allows one-sided discovery: only the initiator needs to run `/discover`.
    let announcer_stop = Arc::clone(&stop);
    let announcer_beacon = announce_beacon.clone();
    let announcer_events = bus.tx_events.clone();
    std::thread::spawn(move || {
        let mut d = match ppexchanger::net::discovery::Discovery::bind(
            ppexchanger::net::discovery::CONTROL_PORT,
        ) {
            Ok(d) => d,
            Err(_) => match ppexchanger::net::discovery::Discovery::bind(0) {
                Ok(d) => d,
                Err(e) => {
                    eprintln!("announcer bind failed: {}", e);
                    return;
                }
            },
        };
        let mut beacon = announcer_beacon;
        beacon.control_port = d.local_port().unwrap_or(0);
        while !announcer_stop.load(std::sync::atomic::Ordering::Relaxed) {
            if let Err(e) = d.announce_both(&beacon) {
                eprintln!("announcer error: {}", e);
            }
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
            while std::time::Instant::now() < deadline && !announcer_stop.load(std::sync::atomic::Ordering::Relaxed) {
                if let Ok(Some(addr)) = d.recv_reverse_connect(beacon.peer_id) {
                    let _ = announcer_events.send(Event::Info(format!(
                        "incoming reverse connection request from {}",
                        addr
                    )));
                    let _ = announcer_events.send(Event::ConnectionRequest {
                        addr,
                        name: format!("peer@{}", addr),
                        hostname: String::new(),
                        fingerprint: "pending reverse connection".into(),
                    });
                }
            }
        }
    });

    // Listener thread: accepts inbound TCP, runs responder handshake,
    // hands the outbound sender to the action thread via RegistryMsg,
    // and spawns the per-connection session driver.
    let listener_t = {
        let tx = bus.tx_events.clone();
        let kp = Arc::clone(&static_kp);
        let stop2 = Arc::clone(&stop);
        let reg_tx2 = reg_tx.clone();
        let inbound_tx_for_listener = bus.tx_inbound_files.clone();
        let local_name_for_listener = local_name.clone();
        let local_hostname_for_listener = local_hostname.clone();
        thread::spawn(move || {
            loop {
                if stop2.load(Ordering::Relaxed) {
                    break;
                }
                match listener.accept() {
                    Ok((stream, addr)) => {
                        let kp2 = Arc::clone(&kp);
                        let tx2 = tx.clone();
                        let reg_tx3 = reg_tx2.clone();
                        let inbound_tx_for_driver = inbound_tx_for_listener.clone();
                        let local_name = local_name_for_listener.clone();
                        let local_hostname = local_hostname_for_listener.clone();
                        thread::spawn(move || {
                            let mut s = stream;
                            let _ = s.set_read_timeout(Some(Duration::from_secs(5)));
                            let _ = s.set_write_timeout(Some(Duration::from_secs(5)));
                            let _ = s.set_nodelay(true);
                            // Probe gate: peek for the 4-byte PPXP magic. If it
                            // matches, echo it back and close the probe. The
                            // subnet scanner intentionally sends only this
                            // preamble; attempting Noise after it produces the
                            // misleading "failed to fill whole buffer" log.
                            let mut head = [0u8; 4];
                            if s.read_exact(&mut head).is_err() {
                                return;
                            }
                            if &head == b"PPXP" {
                                let _ = s.write_all(b"PPXP");
                                return;
                            }
                            let prefix = head.to_vec();
                            let mut wrapped = ppexchanger::net::listener::PrefixedStream {
                                head: &prefix,
                                inner: s,
                            };
                            match ppexchanger::net::handshake::run_responder(&mut wrapped, &kp2) {
                                Ok(res) => {
                                    let session = Session::new(
                                        wrapped.inner,
                                        res.send_key,
                                        res.recv_key,
                                        res.remote_static,
                                    );
                                    let peer_id = ppexchanger::net::listener::peer_id_from_pubkey(
                                        &session.remote_static,
                                    );
                                    let fp = res.remote_fingerprint.clone();
                                    let _ = tx2.send(Event::Info(format!(
                                        "inbound peer from {} (fp {})",
                                        addr, fp
                                    )));
                                    let _ = tx2.send(Event::PeerConnected {
                                        peer_id,
                                        name: format!("peer@{}", addr),
                                        fingerprint: fp.clone(),
                                        trusted: false,
                                        addr,
                                    });
                                    let (otx, orx) = mpsc::channel::<FrameBody>();
                                    let _ = reg_tx3.send(RegistryMsg::Register {
                                        peer_id,
                                        name: format!("peer@{}", addr),
                                        sender: otx,
                                    });
                                    let reg_tx4 = reg_tx3.clone();
                                    peer::spawn_session_driver_with_reg(
                                        session,
                                        peer_id,
                                        fp,
                                        orx,
                                        tx2,
                                        inbound_tx_for_driver,
                                        Some(reg_tx4),
                                        local_name,
                                        local_hostname,
                                    );
                                }
                                Err(e) => {
                                    let _ = tx2.send(Event::Info(format!(
                                        "inbound handshake from {} failed: {}",
                                        addr, e
                                    )));
                                }
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

    // No always-on announcer/receiver — discovery runs only when the user
    // enters `/discover`. The thread handles are stored so we can join them
    // at quit time if a scan is still in flight.

    // Action consumer thread. Owns the outbound registry: one
    // `mpsc::Sender<FrameBody>` per live peer session. Inbound listener
    // feeds it `RegistryMsg::Register`; the driver disconnects post
    // `Event::PeerGone`, which we translate to `RegistryMsg::Unregister`.
    // Action::SendText pushes a frame into the registered sender.
    let act_stop = Arc::clone(&stop);
    let act_bus_tx = bus.tx_events.clone();
    let act_bus_rx = bus.rx_actions; // moved in
    let act_inbound_rx = bus.rx_inbound_files; // moved in
    let act_state = Arc::clone(&state);
    let act_thread = {
        let kp = Arc::clone(&static_kp);
        let act_reg_tx = reg_tx.clone();
        let initial_pending_text = initial_pending_text;
        thread::spawn(move || {
            let mut outbound: HashMap<PeerId, mpsc::Sender<FrameBody>> = HashMap::new();
            let mut peer_names: HashMap<PeerId, String> = HashMap::new();
            let mut pending_text: HashMap<PeerId, VecDeque<String>> = HashMap::new();
            for (peer_id, body) in initial_pending_text {
                pending_text.entry(peer_id).or_default().push_back(body);
            }
            let mut outbox: ppexchanger::net::file_xfer::OutboundMap =
                ppexchanger::net::file_xfer::OutboundMap::new();
            let mut inbox: ppexchanger::net::file_xfer::InboundMap =
                ppexchanger::net::file_xfer::InboundMap::new();
            while !act_stop.load(Ordering::Relaxed) {
                // Poll the action channel with a short timeout so we can
                // also drain the registry + inbound-file channels
                // between bursts.
                match act_bus_rx.recv_timeout(Duration::from_millis(100)) {
                    Ok(Action::Connect {
                        addr,
                        name_hint,
                        public_key: _,
                        reverse,
                    }) => {
                        // peer::connect dials, handshakes, spawns the
                        // driver, and registers its outbound sender with
                        // the action consumer's registry before returning.
                        // On any failure it has already posted an Info
                        // event and returns None.
                        let tx_clone = act_bus_tx.clone();
                        let tx_inbound_clone = bus.tx_inbound_files.clone();
                        let kp_clone = Arc::clone(&kp);
                        let reg_clone = act_reg_tx.clone();
                        let fallback_port = bound_port;
                        let local_name_for_connect = local_name.clone();
                        let local_hostname_for_connect = local_hostname.clone();
                        thread::spawn(move || {
                            if let Some((peer_id, discovered)) = peer::connect(
                                addr,
                                Some(name_hint.clone()),
                                &kp_clone,
                                tx_clone.clone(),
                                tx_inbound_clone,
                                reg_clone,
                                local_name_for_connect,
                                local_hostname_for_connect,
                            ) {
                                let _ = tx_clone.send(Event::PeerConnected {
                                    peer_id,
                                    name: discovered
                                        .name
                                        .unwrap_or_else(|| format!("peer@{}", addr)),
                                    fingerprint: discovered
                                        .fingerprint
                                        .unwrap_or_else(|| "?".into()),
                                    trusted: false,
                                    addr,
                                });
                            } else if let Some((control_addr, target_peer_id)) = reverse {
                                // Try the advertised endpoint first, then the
                                // stable control port. This also recovers from
                                // a stale beacon published by an older build.
                                let first = Discovery::request_reverse_connect(control_addr, target_peer_id, fallback_port);
                                let stable = if control_addr.port() != ppexchanger::net::discovery::CONTROL_PORT {
                                    Discovery::request_reverse_connect(
                                        SocketAddr::new(control_addr.ip(), ppexchanger::net::discovery::CONTROL_PORT),
                                        target_peer_id,
                                        fallback_port,
                                    )
                                } else {
                                    Ok(false)
                                };
                                match (first, stable) {
                                    (Ok(true), _) | (_, Ok(true)) => { let _ = tx_clone.send(Event::Info(format!("{} received the reverse-connect request", addr))); }
                                    (Ok(false), Ok(false)) => { let _ = tx_clone.send(Event::Info(format!("{} did not acknowledge the reverse-connect request", addr))); }
                                    (Err(e), _) | (_, Err(e)) => { let _ = tx_clone.send(Event::Info(format!("direct connection and reverse request failed: {}", e))); }
                                }
                            }
                        });
                    }
                    Ok(Action::Trust { peer_id }) => {
                        let mut s = act_state.lock().unwrap();
                        if let Some(p) = s.peers.iter_mut().find(|p| p.peer_id == peer_id) {
                            p.trusted = true;
                        }
                        s.mark_contacts_dirty();
                    }
                    Ok(Action::AcceptConnection { addr: _ }) => {
                        // TODO: implement connection request prompts
                        // For now, auto-accept
                    }
                    Ok(Action::DenyConnection { addr: _ }) => {
                        // TODO: implement connection request prompts
                        // For now, nothing to do
                    }
                    Ok(Action::Disconnect { peer_id }) => {
                        // Drop our reference to the outbound sender; the
                        // driver thread sees the channel close and exits
                        // on its next drain, posting Unregister via the
                        // registry channel as a side-effect.
                        outbound.remove(&peer_id);
                        peer_names.remove(&peer_id);
                    }
                    Ok(Action::Revoke { peer_id }) => {
                        let mut s = act_state.lock().unwrap();
                        s.peers.retain(|p| p.peer_id != peer_id);
                        s.mark_contacts_dirty();
                        outbound.remove(&peer_id);
                        peer_names.remove(&peer_id);
                    }
                    Ok(Action::SendText { to, body }) => {
                        // Optimistic local echo: render the sent line in
                        // the UI immediately so the user sees feedback.
                        {
                            let mut s = act_state.lock().unwrap();
                            s.push_outgoing_message(to, body.clone());
                        }
                        pending_text.entry(to).or_default().push_back(body.clone());
                        // Push immediately when a live driver exists. The
                        // queue remains until the driver confirms the write,
                        // so a broken pipe cannot lose the user's message.
                        if let Some(tx) = outbound.get(&to).cloned() {
                            if tx.send(FrameBody::Text(body)).is_err() {
                                outbound.remove(&to);
                                let _ = act_bus_tx.send(Event::Info(
                                    "peer is offline; message queued for reconnect".into(),
                                ));
                            }
                        } else {
                            let _ = act_bus_tx.send(Event::Info(
                                "peer is offline; message queued for reconnect".into(),
                            ));
                        }
                    }
                    // File actions drive the state machines in
                    // `file_xfer`. SendFile opens the file, sends the
                    // offer, and parks until accept; AcceptFile /
                    // RejectFile route the peer response and create
                    // the destination file on accept.
                    Ok(Action::SendFile { to, path }) => {
                        let to_name = peer_names
                            .get(&to)
                            .cloned()
                            .unwrap_or_else(|| hex(&to));
                        match ppexchanger::net::file_xfer::OutboundTransfer::open(
                            to, to_name, path,
                        ) {
                            Ok(t) => {
                                let id = t.id();
                                let offer = t.offer().clone();
                                if let Some(tx) = outbound.get(&to) {
                                    let _ = tx.send(FrameBody::FileOffer {
                                        id,
                                        name: offer.name.clone(),
                                        size: offer.size,
                                        mime: offer.mime.clone(),
                                    });
                                }
                                outbox.insert(t);
                                let _ = act_bus_tx.send(Event::Info(format!(
                                    "offered {} ({} bytes) to {}",
                                    offer.name,
                                    offer.size,
                                    peer_names
                                        .get(&to)
                                        .cloned()
                                        .unwrap_or_else(|| hex(&to))
                                )));
                            }
                            Err(e) => {
                                let _ = act_bus_tx
                                    .send(Event::Info(format!("open failed: {}", e)));
                            }
                        }
                    }
                    Ok(Action::AcceptFile { from_peer, id }) => {
                        // Reply with FileAccept + create destination
                        // file via the inbound map. The peer name is
                        // patched in from the registry when the offer
                        // was first delivered.
                        match inbox.accept(id) {
                            Ok(Some(offer)) => {
                                if let Some(tx) = outbound.get(&from_peer) {
                                    let _ = tx.send(FrameBody::FileAccept { id });
                                }
                                let from_name = peer_names
                                    .get(&from_peer)
                                    .cloned()
                                    .unwrap_or_else(|| hex(&from_peer));
                                let _ = act_bus_tx.send(Event::FileOffer {
                                    from_peer,
                                    from_name,
                                    offer,
                                });
                            }
                            Ok(None) => {
                                let _ = act_bus_tx.send(Event::Info(format!(
                                    "accept: no pending offer for {}",
                                    id.to_hex()
                                )));
                            }
                            Err(e) => {
                                let _ = act_bus_tx.send(Event::Info(format!(
                                    "accept {}: {}",
                                    id.to_hex(),
                                    e
                                )));
                            }
                        }
                    }
                    Ok(Action::RejectFile { from_peer, id }) => {
                        if inbox.reject(id).is_some() {
                            if let Some(tx) = outbound.get(&from_peer) {
                                let _ = tx.send(FrameBody::FileReject { id });
                            }
                        }
                    }
                    Ok(Action::Quit) => {
                        act_stop.store(true, Ordering::Relaxed);
                        break;
                    }
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                    Err(_) => break,
                }
                // Drain registry messages produced by the inbound
                // listener and by the connect helper.
                while let Ok(msg) = reg_rx.try_recv() {
                    match msg {
                        RegistryMsg::Register {
                            peer_id,
                            name,
                            sender,
                        } => {
                            peer_names.insert(peer_id, name);
                            outbound.insert(peer_id, sender.clone());
                            // Flush messages accumulated while this peer was
                            // offline. Keep each entry until its driver emits
                            // TextDelivered, allowing a failed reconnect to
                            // be retried by the next connection.
                            if let Some(queue) = pending_text.get(&peer_id) {
                                let mut failed = false;
                                for body in queue {
                                    if sender.send(FrameBody::Text(body.clone())).is_err() {
                                        failed = true;
                                        break;
                                    }
                                }
                                if failed {
                                    outbound.remove(&peer_id);
                                }
                            }
                        }
                        RegistryMsg::Rename { peer_id, name } => {
                            peer_names.insert(peer_id, name);
                        }
                        RegistryMsg::TextDelivered { peer_id, body } => {
                            if let Some(queue) = pending_text.get_mut(&peer_id) {
                                if let Some(index) = queue.iter().position(|queued| queued == &body) {
                                    queue.remove(index);
                                }
                                if queue.is_empty() {
                                    pending_text.remove(&peer_id);
                                }
                            }
                        }
                        RegistryMsg::TextSendFailed { peer_id, .. } => {
                            outbound.remove(&peer_id);
                        }
                        RegistryMsg::Unregister { peer_id } => {
                            outbound.remove(&peer_id);
                            peer_names.remove(&peer_id);
                            // Abort any in-flight transfers that depend
                            // on this peer so we don't leak file
                            // handles or leave half-finished chunks.
                            for info in outbox.remove_for_peer(peer_id) {
                                let _ = act_bus_tx.send(Event::FileAborted {
                                    from_peer: info.from_peer,
                                    from_name: info.from_name,
                                    name: info.name,
                                    reason: info.reason,
                                    partial: None,
                                });
                            }
                            for ab in inbox.remove_for_peer(peer_id) {
                                let _ = act_bus_tx.send(Event::FileAborted {
                                    from_peer: ab.peer,
                                    from_name: ab.from_name,
                                    name: ab.name,
                                    reason: ab.reason,
                                    partial: ab.partial,
                                });
                            }
                        }
                    }
                }

                // Drain inbound file events: the per-connection
                // drivers forward FileOffer / FileChunk / FileDone
                // straight here.
                while let Ok(ev) = act_inbound_rx.try_recv() {
                    use ppexchanger::events::InboundFileEvent;
                    match ev {
                        InboundFileEvent::Offer { peer, offer } => {
                            let from_name = peer_names
                                .get(&peer)
                                .cloned()
                                .unwrap_or_else(|| hex(&peer));
                            let accepted = inbox.offer(
                                ppexchanger::net::file_xfer::InboundTransfer::new(
                                    peer, from_name.clone(), offer.clone(),
                                ),
                            );
                            if accepted {
                                let _ = act_bus_tx.send(Event::FileOffer {
                                    from_peer: peer,
                                    from_name,
                                    offer,
                                });
                            }
                        }
                        InboundFileEvent::Accept { peer: _, id } => {
                            outbox.accept(id);
                        }
                        InboundFileEvent::Reject { peer: _, id } => {
                            if let Some(info) = outbox.reject(id) {
                                let _ = act_bus_tx.send(Event::FileAborted {
                                    from_peer: info.from_peer,
                                    from_name: info.from_name,
                                    name: info.name,
                                    reason: info.reason,
                                    partial: None,
                                });
                            }
                        }
                        InboundFileEvent::Chunk { peer: _, id, offset, data } => {
                            use ppexchanger::net::file_xfer::WriteOutcome;
                            if let WriteOutcome::Error(reason) = inbox.write_chunk(id, offset, data) {
                                if let Some(offer) = inbox.reject(id) {
                                    let _ = act_bus_tx.send(Event::FileAborted {
                                        from_peer: [0u8; 16], // patched below
                                        from_name: String::new(),
                                        name: offer.name,
                                        reason,
                                        partial: None,
                                    });
                                }
                            }
                        }
                        InboundFileEvent::Done { peer, id } => {
                            // FileDone on the wire carries only the
                            // FileId — we trust the offer's announced
                            // size for the size check, and use the
                            // peer name from the registry for the
                            // success event.
                            use ppexchanger::net::file_xfer::FinalizeOutcome;
                            let expected_size = inbox.offer_size(&id).unwrap_or(u64::MAX);
                            match inbox.finalize(id, expected_size) {
                                FinalizeOutcome::Done(info) => {
                                    let _ = act_bus_tx.send(Event::FileReceived {
                                        from_peer: info.peer,
                                        from_name: info.from_name,
                                        name: info.name,
                                        bytes: info.bytes,
                                        saved_to: info.path,
                                    });
                                }
                                FinalizeOutcome::Failed(_e) => {
                                    // Surface as Info — the partial
                                    // file was renamed to `.partial`
                                    // by `InboundTransfer::abort`,
                                    // which `finalize` calls on the
                                    // size mismatch path internally.
                                    let _ = act_bus_tx.send(Event::Info(format!(
                                        "inbound transfer failed for peer {} (size mismatch?)",
                                        hex(&peer)
                                    )));
                                }
                                FinalizeOutcome::Unknown => {}
                            }
                        }
                    }
                }

                // Tick outbound transfers: timeouts + one chunk
                // forward per peer. Bounded to one chunk per tick
                // so the action thread stays responsive even with
                // many active transfers.
                for info in outbox.tick_timeouts() {
                    let _ = act_bus_tx.send(Event::FileAborted {
                        from_peer: info.from_peer,
                        from_name: info.from_name,
                        name: info.name,
                        reason: info.reason,
                        partial: None,
                    });
                }
                for result in outbox.step_all(|peer| outbound.get(&peer).cloned()) {
                    use ppexchanger::net::file_xfer::StepResult;
                    match result {
                        StepResult::Completed { peer, to_name, name, bytes } => {
                            let _ = act_bus_tx.send(Event::Info(format!(
                                "sent {} ({} bytes) to {}",
                                name, bytes, to_name
                            )));
                            let _ = peer;
                        }
                        StepResult::Aborted(info) => {
                            let _ = act_bus_tx.send(Event::FileAborted {
                                from_peer: info.from_peer,
                                from_name: info.from_name,
                                name: info.name,
                                reason: info.reason,
                                partial: None,
                            });
                        }
                    }
                }
            }
        })
    };

    // Restore every previously connected contact that has a usable last
    // address. The action thread performs the bounded TCP retries off the UI
    // thread, so a sleeping laptop or stale DHCP lease cannot stall startup.
    let reconnect_targets: Vec<_> = db
        .iter()
        .filter_map(|contact| {
            let addr = contact.last_addr?;
            (addr.port() != 0 && !addr.ip().is_unspecified())
                .then(|| (addr, contact.name.clone(), contact.public_key))
        })
        .collect();
    if !reconnect_targets.is_empty() {
        let _ = bus.tx_events.send(Event::Info(format!(
            "reconnecting to {} saved peer{}…",
            reconnect_targets.len(),
            if reconnect_targets.len() == 1 { "" } else { "s" }
        )));
        for (addr, name, public_key) in reconnect_targets {
            let _ = bus.tx_actions.send(Action::Connect {
                addr,
                name_hint: name,
                public_key,
                reverse: None,
            });
        }
    }

    // TUI loop.
    let mut _guard = tui::TuiGuard::new(ui_cfg.mouse).unwrap();
    let mut terminal = tui::enter_terminal(ui_cfg.mouse).unwrap();
    let mut editor = ppexchanger::tui::LineEditor::new();
    // Active mutable copy of the config — `/theme` updates it, so we can
    // persist on change without re-reading from disk.
    let mut live_cfg = ui_cfg;
    let live_cfg_path = cfg_path;
    let received_dir_str = config_dir()
        .map(|d| d.join("received").display().to_string())
        .unwrap_or_else(|_| "<no config dir>".to_string());
    let mut last_history_save_attempt = Instant::now() - Duration::from_secs(2);
    let mut history_save_backoff = false;

    loop {
        {
            let mut s = state.lock().unwrap();
            let text_count = tui::drain_events(&bus.rx_events, &mut s);
            if s.contacts_need_save() {
                tui::sync_to_db(&s, &mut db);
                match db.save() {
                    Ok(()) => s.mark_contacts_saved(),
                    Err(error) => {
                        s.status = format!("peer list save failed: {}", error);
                    }
                }
            }
            // Persist after each batch of newly-arrived messages. Keep the
            // state lock while serializing so a concurrent optimistic echo
            // cannot be marked clean without being included in the snapshot.
            if history_path.is_some()
                && s.history_needs_save()
                && (!history_save_backoff
                    || last_history_save_attempt.elapsed() >= Duration::from_secs(2))
            {
                last_history_save_attempt = Instant::now();
                let save_result = ppexchanger::chat_history::save(
                    history_path.as_ref().unwrap(),
                    &history_secret,
                    &history_peer_id,
                    &s.messages,
                );
                match save_result {
                    Ok(()) => {
                        s.mark_history_saved();
                        history_save_backoff = false;
                    }
                    Err(error) => {
                        history_save_backoff = true;
                        s.status = format!("chat history save failed: {}", error);
                    }
                }
            }
            // Stable sidebar ordering: Connected > Seen > Gone, then name.
            s.sort_peers();
            // Notify-bell: when a fresh chat message arrived and the
            // user opted in, ring the terminal bell on stderr. Avoids
            // stdout (which the renderer owns). Fits in one byte so
            // doing this inline is cheaper than spawning a notifier.
            if live_cfg.notify_sound && text_count > 0 {
                let _ = std::io::Write::write_all(&mut std::io::stderr(), b"\x07");
            }
            // Auto-trust: when a peer just transitioned to Connected
            // and the user opted in, post a Trust action immediately
            // so the contact DB and UI badge flip in one tick. The
            // action thread re-applies the flag — idempotent because
            // `set_trusted(true)` is a no-op when already set.
            if live_cfg.auto_trust_seen {
                for p in s.peers.iter() {
                    if matches!(p.state, PeerState::Connected) && !p.trusted {
                        let _ = bus.tx_actions.send(Action::Trust {
                            peer_id: p.peer_id,
                        });
                    }
                }
            }
        }
        {
            let mut s = state.lock().unwrap();
            // Mirror live cfg bits that the render loop reads. Cheap;
            // called once per render pass so the footer / settings
            // reflect every change without per-handler plumbing.
            s.apply_live_cfg(&live_cfg);
            let view = tui::SettingsView {
                cfg: Some(&live_cfg),
                version: VERSION,
                config_path: &live_cfg_path.display().to_string(),
                received_dir: &received_dir_str,
            };
            if let Err(e) = tui::render(&mut terminal, &mut s, &theme, &glyphs, view) {
                eprintln!("render error: {}", e);
                break;
            }
        }
        if crossterm::event::poll(Duration::from_millis(150)).unwrap_or(false) {
            if let Ok(ev) = crossterm::event::read() {
                // Mouse + paste never reach on_key: crossterm's on_key
                // returns None for non-Key events, which would silently
                // drop a bracketed paste. Peel them apart here.
                if matches!(ev, crossterm::event::Event::Paste(_)) {
                    if let crossterm::event::Event::Paste(s) = &ev {
                        let _ = editor.on_paste(s);
                    }
                } else if matches!(ev, crossterm::event::Event::Mouse(_)) {
                    if live_cfg.mouse {
                        if let crossterm::event::Event::Mouse(m) = ev {
                            let sz = terminal.size().unwrap_or_default();
                            let rect = ratatui::layout::Rect {
                                x: 0,
                                y: 0,
                                width: sz.width,
                                height: sz.height,
                            };
                            // Modal controls own their mouse interactions.
                            // Handle them before generic pane hit-testing so a
                            // click never leaks through to the chat beneath.
                            if matches!(m.kind, crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left)) {
                                let settings_target = {
                                    let s = state.lock().unwrap();
                                    s.settings.as_ref().and_then(|settings| {
                                        ppexchanger::tui::settings_popup::mouse_target(
                                            rect, m.column, m.row, settings,
                                        )
                                    })
                                };
                                if let Some(target) = settings_target {
                                    use ppexchanger::tui::settings_popup::MouseTarget;
                                    match target {
                                        MouseTarget::Tab(tab) => {
                                            state.lock().unwrap().settings.as_mut().unwrap().switch_tab(tab);
                                        }
                                        MouseTarget::Row(row) => {
                                            let key = crossterm::event::KeyEvent::new(
                                                crossterm::event::KeyCode::Enter,
                                                crossterm::event::KeyModifiers::NONE,
                                            );
                                            let mut s = state.lock().unwrap();
                                            let settings = s.settings.as_mut().unwrap();
                                            settings.selected = row;
                                            route_settings_key(&key, settings, &mut live_cfg, &mut _guard);
                                        }
                                        MouseTarget::Close => {
                                            let name = {
                                                let mut s = state.lock().unwrap();
                                                let name = s.settings.as_ref().unwrap().name_draft.trim().to_string();
                                                if !name.is_empty() {
                                                    s.self_name = name.clone();
                                                }
                                                s.close_settings();
                                                name
                                            };
                                            let _ = save_ui_config(&live_cfg, &live_cfg_path);
                                            if !name.is_empty() {
                                                let _ = ppexchanger::identity::update_name(&name);
                                            }
                                        }
                                    }
                                    continue;
                                }
                                let file_action = {
                                    let s = state.lock().unwrap();
                                    s.file_offer.as_ref().and_then(|_| {
                                        ppexchanger::tui::file_offer_popup::mouse_action(rect, m.column, m.row)
                                    })
                                };
                                if let Some(file_action) = file_action {
                                    let pending = state.lock().unwrap().file_offer.as_ref().map(|p| (p.from_peer, p.offer.id));
                                    if let Some((peer, id)) = pending {
                                        match file_action {
                                            ppexchanger::tui::file_offer_popup::MouseAction::Accept => {
                                                let _ = bus.tx_actions.send(Action::AcceptFile { from_peer: peer, id });
                                            }
                                            ppexchanger::tui::file_offer_popup::MouseAction::Reject => {
                                                let _ = bus.tx_actions.send(Action::RejectFile { from_peer: peer, id });
                                                state.lock().unwrap().file_offer = None;
                                            }
                                        }
                                    }
                                    continue;
                                }
                                let pending_click = {
                                    let s = state.lock().unwrap();
                                    let sidebar = tui::compute_layout(rect).sidebar;
                                    s.pending_connection.as_ref().and_then(|request| {
                                        (m.column >= sidebar.x
                                            && m.column < sidebar.right()
                                            && m.row >= sidebar.y
                                            && m.row < sidebar.y.saturating_add(5))
                                            .then(|| request.clone())
                                    })
                                };
                                if let Some(request) = pending_click {
                                    state.lock().unwrap().pending_connection = None;
                                    state.lock().unwrap().status = format!("accepting connection from {}…", request.name);
                                    let _ = bus.tx_actions.send(Action::Connect {
                                        addr: request.addr,
                                        name_hint: request.name,
                                        public_key: [0; 32],
                                        reverse: None,
                                    });
                                    continue;
                                }
                                let discovered = {
                                    let s = state.lock().unwrap();
                                    s.discovery.as_ref().and_then(|discovery| {
                                        ppexchanger::tui::discovery_popup::peer_at(
                                            rect, m.column, m.row, discovery,
                                        )
                                    })
                                };
                                if let Some(index) = discovered {
                                    let peer = {
                                        let mut s = state.lock().unwrap();
                                        if let Some(discovery) = s.discovery.as_mut() {
                                            discovery.selected = index;
                                        }
                                        let peer = s.selected_discovery_peer();
                                        if let Some(peer) = &peer {
                                            s.status = format!("connecting to {}…", peer.addr);
                                            s.close_discovery();
                                        }
                                        peer
                                    };
                                    if let Some(peer) = peer {
                                        let name = peer.name.unwrap_or_else(|| format!("peer@{}", peer.addr));
                                        let _ = bus.tx_actions.send(Action::Connect {
                                            addr: peer.addr,
                                            name_hint: name,
                                            public_key: [0u8; 32],
                                            reverse: peer.reverse,
                                        });
                                    }
                                    continue;
                                }
                                let dismiss_overlay = {
                                    let s = state.lock().unwrap();
                                    (s.show_help && tui::point_in_rect(tui::help::rect(rect), m.column, m.row))
                                        || (s.discovery.is_some() && tui::point_in_rect(tui::discovery_popup::rect(rect), m.column, m.row))
                                };
                                if dismiss_overlay {
                                    let mut s = state.lock().unwrap();
                                    s.show_help = false;
                                    s.close_discovery();
                                    continue;
                                }
                                let command = {
                                    let s = state.lock().unwrap();
                                    let chat = tui::compute_layout(rect).chat;
                                    tui::command_palette_hit(chat, &s.composer, m.column, m.row)
                                };
                                if let Some(command) = command {
                                    editor.buffer = format!("{} ", command);
                                }
                            }
                            // Menu clicks come back as EditorEvents so
                            // the dispatch arm below can run them
                            // through the same path as Ctrl-, / ? /
                            // Esc — keeping state mutation in one
                            // place.
                            if let Some(ppexchanger::tui::EditorEvent::MenuAction(act)) = handle_mouse(m, &state, rect) {
                                match act {
                                        ppexchanger::tui::MenuAction::Peers => {
                                            state.lock().unwrap().focus = tui::Focus::Sidebar;
                                        }
                                        ppexchanger::tui::MenuAction::Discover => {
                                            state.lock().unwrap().start_discovery();
                                            do_discover(
                                                announce_beacon.clone(),
                                                self_peer_id,
                                                bus.tx_events.clone(),
                                                Arc::clone(&stop),
                                            );
                                        }
                                        ppexchanger::tui::MenuAction::Settings => {
                                            state.lock().unwrap().open_settings(&live_cfg);
                                        }
                                        ppexchanger::tui::MenuAction::Help => {
                                            state.lock().unwrap().show_help = true;
                                        }
                                        ppexchanger::tui::MenuAction::Quit => {
                                            stop.store(true, Ordering::SeqCst);
                                        }
                                    }
                                }
                        }
                    }
                } else if state.lock().unwrap().pending_connection.is_some() {
                    if let crossterm::event::Event::Key(k) = &ev {
                        if k.kind == crossterm::event::KeyEventKind::Press {
                            match k.code {
                                crossterm::event::KeyCode::Enter => {
                                    let request = { state.lock().unwrap().pending_connection.take() };
                                    if let Some(request) = request {
                                        state.lock().unwrap().status = format!("accepting connection from {}…", request.name);
                                        let _ = bus.tx_actions.send(Action::Connect {
                                            addr: request.addr,
                                            name_hint: request.name,
                                            public_key: [0; 32],
                                            reverse: None,
                                        });
                                    }
                                }
                                crossterm::event::KeyCode::Esc => {
                                    let mut s = state.lock().unwrap();
                                    s.pending_connection = None;
                                    s.status = "connection request declined".into();
                                }
                                _ => {}
                            }
                        }
                    }
                } else if state
                    .lock()
                    .unwrap()
                    .settings
                    .as_ref()
                    .is_some()
                {
                    // Settings modal is open — every key event routes here
                    // (including Esc, which closes the modal through
                    // route_settings_key). The editor buffer is frozen.
                    if let crossterm::event::Event::Key(k) = &ev {
                        if k.kind == crossterm::event::KeyEventKind::Press {
                            let (close_after, dirty) = {
                                let mut s = state.lock().unwrap();
                                route_settings_key(
                                    k,
                                    s.settings.as_mut().unwrap(),
                                    &mut live_cfg,
                                    &mut _guard,
                                );
                                let close = k.code == crossterm::event::KeyCode::Esc;
                                (close, s.settings.as_ref().unwrap().dirty)
                            };
                            if close_after {
                                let name_draft = {
                                    let mut s = state.lock().unwrap();
                                    let name = s
                                        .settings
                                        .as_ref()
                                        .map(|settings| settings.name_draft.trim().to_string())
                                        .unwrap_or_default();
                                    if !name.is_empty() && name != s.self_name {
                                        s.self_name = name.clone();
                                    }
                                    s.close_settings();
                                    name
                                };
                                match save_ui_config(&live_cfg, &live_cfg_path) {
                                    Ok(()) => {
                                        let _ = bus.tx_events.send(Event::Info(
                                            "settings saved".into(),
                                        ));
                                    }
                                    Err(e) => {
                                        let _ = bus.tx_events.send(Event::Info(format!(
                                            "settings save failed: {}",
                                            e
                                        )));
                                    }
                                }
                                if !name_draft.is_empty() {
                                    if let Err(e) = ppexchanger::identity::update_name(&name_draft) {
                                        let _ = bus.tx_events.send(Event::Info(format!(
                                            "display name save failed: {}",
                                            e
                                        )));
                                    }
                                }
                            }
                            let _ = dirty;
                        }
                    }
                } else if state.lock().unwrap().discovery.is_some() {
                    // Discovery is a selectable dialog: keyboard users can
                    // navigate results and connect without reaching for the
                    // mouse.
                    if let crossterm::event::Event::Key(k) = &ev {
                        if k.kind == crossterm::event::KeyEventKind::Press {
                            match k.code {
                                crossterm::event::KeyCode::Up | crossterm::event::KeyCode::Char('k') => {
                                    state.lock().unwrap().move_discovery_selection(-1);
                                }
                                crossterm::event::KeyCode::Down | crossterm::event::KeyCode::Char('j') => {
                                    state.lock().unwrap().move_discovery_selection(1);
                                }
                                crossterm::event::KeyCode::Enter => {
                                    let peer = {
                                        let mut s = state.lock().unwrap();
                                        let peer = s.selected_discovery_peer();
                                        if let Some(peer) = &peer {
                                            s.status = format!("connecting to {}…", peer.addr);
                                            s.close_discovery();
                                        }
                                        peer
                                    };
                                    if let Some(peer) = peer {
                                        let name = peer.name.unwrap_or_else(|| format!("peer@{}", peer.addr));
                                        let _ = bus.tx_actions.send(Action::Connect {
                                            addr: peer.addr,
                                            name_hint: name,
                                            public_key: [0u8; 32],
                                            reverse: peer.reverse,
                                        });
                                    }
                                }
                                crossterm::event::KeyCode::Esc => state.lock().unwrap().close_discovery(),
                                _ => {}
                            }
                        }
                    }
                } else {
                    match editor.on_key(&ev) {
                        ppexchanger::tui::EditorEvent::Submit(text) => {
                            if text.starts_with('/') {
                                handle_command(
                                    &text,
                                    &bus.tx_events,
                                    &bus.tx_actions,
                                    &state,
                                    &mut live_cfg,
                                    &live_cfg_path,
                                    &announce_beacon,
                                    self_peer_id,
                                    Arc::clone(&stop),
                                );
                            } else if let Some(target) = resolve_target(&state, &text) {
                                // Auto-detect: if the body (after stripping
                                // any `@<name>` routing prefix) is the
                                // path of an existing regular file, send
                                // it as a FileOffer. Otherwise fall
                                // through to plain text.
                                let body = strip_routing(&text);
                                if let Some(path) = looks_like_existing_file(&body) {
                                    let _ = bus.tx_actions.send(Action::SendFile {
                                        to: target,
                                        path,
                                    });
                                } else {
                                    let _ = bus.tx_actions.send(Action::SendText {
                                        to: target,
                                        body,
                                    });
                                }
                            } else {
                                let _ = bus.tx_events.send(Event::Info(
                                    "no peer selected or matched".into(),
                                ));
                            }
                        }
                    ppexchanger::tui::EditorEvent::Cancel => {
                        let _ = bus.tx_actions.send(Action::Quit);
                        break;
                    }
                    ppexchanger::tui::EditorEvent::Quit => {
                        let _ = bus.tx_actions.send(Action::Quit);
                        break;
                    }
                    ppexchanger::tui::EditorEvent::FocusNext => {
                        let mut s = state.lock().unwrap();
                        s.cycle_focus();
                    }
                    ppexchanger::tui::EditorEvent::ToggleTrust => {
                        let pid = {
                            let s = state.lock().unwrap();
                            s.selected().map(|p| p.peer_id)
                        };
                        if let Some(pid) = pid {
                            let _ = bus.tx_actions.send(Action::Trust { peer_id: pid });
                        }
                    }
                    ppexchanger::tui::EditorEvent::RevokePeer => {
                        let pid = {
                            let s = state.lock().unwrap();
                            s.selected().map(|p| p.peer_id)
                        };
                        if let Some(pid) = pid {
                            let _ = bus.tx_actions.send(Action::Revoke { peer_id: pid });
                        }
                    }
                    ppexchanger::tui::EditorEvent::NewChat => {
                        // For v1 this is a no-op visual hint; peer selection
                        // is via Up/Down on the sidebar after Tab.
                        let _ = bus.tx_events.send(Event::Info(
                            "use Tab to focus the sidebar, then Up/Down to pick a peer".into(),
                        ));
                    }
                    ppexchanger::tui::EditorEvent::ToggleHelp => {
                        let mut s = state.lock().unwrap();
                        s.show_help = !s.show_help;
                    }
                    ppexchanger::tui::EditorEvent::OpenSettings => {
                        let mut s = state.lock().unwrap();
                        s.open_settings(&live_cfg);
                    }
                    ppexchanger::tui::EditorEvent::PageUp => {
                        let mut s = state.lock().unwrap();
                        if s.focus == tui::Focus::Chat {
                            s.scroll_back(5);
                        } else {
                            s.move_selection(-1);
                        }
                    }
                    ppexchanger::tui::EditorEvent::PageDown => {
                        let mut s = state.lock().unwrap();
                        if s.focus == tui::Focus::Chat {
                            s.scroll_forward(5);
                        } else {
                            s.move_selection(1);
                        }
                    }
                    ppexchanger::tui::EditorEvent::ClearInput
                    | ppexchanger::tui::EditorEvent::Clear => {
                        // Editor already cleared its buffer. If a modal is
                        // open, Esc also closes it. The settings popup
                        // routes Esc through `route_settings_key`, so this
                        // branch only fires for help / discovery / logo.
                        let mut s = state.lock().unwrap();
                        if s.show_help {
                            s.show_help = false;
                        } else if s.discovery.is_some() {
                            s.close_discovery();
                        } else {
                            // Fresh session: dismiss the startup logo.
                            s.dismiss_logo();
                        }
                    }
                    ppexchanger::tui::EditorEvent::HistoryPrev
                    | ppexchanger::tui::EditorEvent::HistoryNext
                    | ppexchanger::tui::EditorEvent::Edited
                    | ppexchanger::tui::EditorEvent::MenuAction(_)
                    | ppexchanger::tui::EditorEvent::None => {}
                    }
                    // File-offer modal: Enter accepts, Esc rejects.
                    // Closed by either choice; the FileReceived /
                    // FileAborted event clears `state.file_offer`
                    // independently as a safety net.
                    if let crossterm::event::Event::Key(k) = &ev {
                        if k.kind == crossterm::event::KeyEventKind::Press {
                            let pending = state
                                .lock()
                                .unwrap()
                                .file_offer
                                .as_ref()
                                .map(|p| (p.from_peer, p.offer.id));
                            if let Some((peer, id)) = pending {
                                match k.code {
                                    crossterm::event::KeyCode::Enter => {
                                        let _ = bus.tx_actions.send(Action::AcceptFile {
                                            from_peer: peer,
                                            id,
                                        });
                                    }
                                    crossterm::event::KeyCode::Esc => {
                                        let _ = bus.tx_actions.send(Action::RejectFile {
                                            from_peer: peer,
                                            id,
                                        });
                                        state.lock().unwrap().file_offer = None;
                                    }
                                    _ => {}
                                }
                            }
                        }
                    }
                }
                // The composer is renderer state, while `status` is reserved
                // for network and command feedback. Keeping them separate
                // means an incoming connection status is no longer erased by
                // every keystroke.
                state.lock().unwrap().composer = editor.as_str().to_owned();
            }
        }
        if stop.load(Ordering::Relaxed) {
            break;
        }
    }

    // Flush the final in-memory batch before stopping worker threads. This
    // covers a message received just before the user exits or a save retry
    // that was waiting on the short backoff timer.
    if let Some(path) = history_path.as_ref() {
        let mut s = state.lock().unwrap();
        if s.history_needs_save() {
            if let Err(error) = ppexchanger::chat_history::save(
                path,
                &history_secret,
                &history_peer_id,
                &s.messages,
            ) {
                s.status = format!("chat history save failed: {}", error);
            } else {
                s.mark_history_saved();
            }
        }
    }
    stop.store(true, Ordering::Relaxed);
    // Discovery threads self-terminate on the stop flag; we don't track
    // their handles here. Listener and action-consumer threads are joined.
    let _ = listener_t.join();
    let _ = act_thread.join();
}

fn make_beacon(id: &ppexchanger::identity::Identity, tcp_port: u16) -> Beacon {
    Beacon {
        peer_id: id.peer_id,
        public_key: id.keypair.public_bytes(),
        tcp_port,
        name: id.name.clone(),
        hostname: id.hostname.clone(),
        control_port: 0,
    }
}

/// Spawn one scan per method, post each result as `Event::DiscoveryUpdate`,
/// then `DiscoveryFinished` once all are in. The UI thread updates the
/// modal state from those events.
fn do_discover(
    beacon: Beacon,
    self_peer_id: ppexchanger::events::PeerId,
    tx: std::sync::mpsc::Sender<Event>,
    stop: Arc<AtomicBool>,
) {
    // Method 1: UDP multicast. Send one announce, listen for ~3s, collect
    // unique peer_ids.
    let tx_mc = tx.clone();
    let stop_mc = Arc::clone(&stop);
    let beacon_mc = beacon.clone();
    thread::spawn(move || {
        let peers = match multicast_scan(&beacon_mc, &stop_mc, Duration::from_secs(3)) {
            Ok(v) => v,
            Err(e) => {
                let _ = tx_mc.send(Event::Info(format!("multicast scan failed: {}", e)));
                Vec::new()
            }
        };
        let _ = tx_mc.send(Event::DiscoveryUpdate {
            method: "UDP multicast (239.255.42.99)".into(),
            peers,
        });
        let _ = tx_mc.send(Event::DiscoveryFinished);
    });

    // Method 2: TCP subnet scan. Walks local /24 for hosts accepting TCP.
    // Probe BOTH the local beacon's announced port AND the canonical
    // multicast port (7777). Two peers on different ports can still find
    // each other: the scan covers both the custom port announced in our
    // own beacon and the well-known default a peer might be using.
    let tx_tcp = tx.clone();
    let tcp_port = beacon.tcp_port;
    thread::spawn(move || {
        let mut ports: Vec<u16> = vec![tcp_port];
        let canonical = ppexchanger::net::discovery::MULTICAST_PORT;
        if !ports.contains(&canonical) {
            ports.push(canonical);
        }
        let addrs = match ppexchanger::net::scan::scan_local_subnet_multi_port(
            &ports,
            ppexchanger::net::scan::SCAN_HOSTS,
        ) {
            Ok(v) => v,
            Err(e) => {
                let _ = tx_tcp.send(Event::Info(format!("tcp scan failed: {}", e)));
                Vec::new()
            }
        };
        let peers = addrs
            .into_iter()
            .map(|a| ppexchanger::events::DiscoveredPeer {
                name: None,
                hostname: None,
                addr: std::net::SocketAddr::V4(a),
                fingerprint: None,
                // TCP discovery gives us a concrete peer IP but no beacon
                // id. Keep a targeted reverse-connect fallback available;
                // the receiver accepts the all-zero id only on its own
                // unicast control endpoint.
                reverse: Some((
                    std::net::SocketAddr::V4(std::net::SocketAddrV4::new(
                        *a.ip(),
                        ppexchanger::net::discovery::CONTROL_PORT,
                    )),
                    [0u8; 16],
                )),
            })
            .collect();
        let label = if ports.len() > 1 {
            format!("TCP subnet scan (ports {})", ports.iter().map(|p| p.to_string()).collect::<Vec<_>>().join(", "))
        } else {
            format!("TCP subnet scan (port {})", tcp_port)
        };
        let _ = tx_tcp.send(Event::DiscoveryUpdate {
            method: label,
            peers,
        });
        let _ = tx_tcp.send(Event::DiscoveryFinished);
        let _ = self_peer_id; // referenced for parity with future signed-scan work
    });
    let _ = stop;
}

/// Open a multicast socket briefly, announce + listen, return the unique
/// beacons we observed (filtering out our own peer_id).
fn multicast_scan(
    beacon: &Beacon,
    stop: &Arc<AtomicBool>,
    window: Duration,
) -> std::io::Result<Vec<ppexchanger::events::DiscoveredPeer>> {
    // Beacons are addressed to the well-known multicast UDP port. Binding an
    // ephemeral local port here lets us send an announcement, but guarantees
    // we never receive another machine's reply: UDP delivery is keyed by the
    // destination port. TCP may use 7777 at the same time; it is a separate
    // transport namespace.
    let mut d = Discovery::bind(ppexchanger::net::discovery::MULTICAST_PORT)?;
    let _ = d.announce_both(beacon);
    let deadline = std::time::Instant::now() + window;
    let mut seen: std::collections::HashMap<ppexchanger::events::PeerId, ppexchanger::events::DiscoveredPeer> =
        std::collections::HashMap::new();
    while std::time::Instant::now() < deadline && !stop.load(Ordering::Relaxed) {
        if let Ok(Some((src, b))) = d.recv_beacon() {
            if b.peer_id == beacon.peer_id {
                continue;
            }
            let tcp_addr: SocketAddr = (src.ip(), b.tcp_port).into();
            let discovered = ppexchanger::events::DiscoveredPeer {
                name: if b.name.is_empty() { None } else { Some(b.name) },
                hostname: if b.hostname.is_empty() { None } else { Some(b.hostname) },
                addr: tcp_addr,
                fingerprint: Some(pubkey_fingerprint(&b.public_key)),
                reverse: (b.control_port != 0).then(|| (SocketAddr::new(src.ip(), b.control_port), b.peer_id)),
            };
            seen.entry(b.peer_id)
                .and_modify(|existing| {
                    // A concurrent `/discover` beacon from this peer lacks
                    // its announcer's ephemeral control port. Prefer the
                    // periodic announcer beacon when it arrives later.
                    if existing.reverse.is_none() && discovered.reverse.is_some() {
                        *existing = discovered.clone();
                    }
                })
                .or_insert(discovered);
        }
    }
    Ok(seen.into_values().collect())
}

/// Resolve a `@<name> ...` routing prefix. Returns the peer_id targeted by
/// the message. Bare text resolves to the currently-selected peer (first
/// connected peer if none selected).
///
/// Translate one mouse event into a UI mutation. Reads the current state
/// under the lock and writes it back in one short critical section. Scroll
/// wheels in the chat pane use the same `scroll_back` / `scroll_forward`
/// increments as PageUp/PageDown (5 lines per notch) so the two input
/// modes feel identical.
fn handle_mouse(
    m: crossterm::event::MouseEvent,
    state: &Arc<Mutex<UiState>>,
    size: ratatui::layout::Rect,
) -> Option<ppexchanger::tui::EditorEvent> {
    use crossterm::event::{MouseButton, MouseEventKind};
    let mut s = state.lock().unwrap();
    let areas = tui::compute_layout(size);
    let modal_open = s.file_offer.is_some() || s.show_help || s.discovery.is_some() || s.settings.is_some();
    let hit = tui::hit_test(
        size,
        m.column,
        m.row,
        &areas,
        modal_open,
        s.peers.len(),
    );
    match m.kind {
        MouseEventKind::Down(MouseButton::Left) => match hit {
            tui::Hit::Sidebar(idx) => {
                s.selected_peer = idx;
                s.focus = tui::Focus::Sidebar;
            }
            tui::Hit::Chat => {
                s.focus = tui::Focus::Chat;
            }
            tui::Hit::Footer => {
                // Clicks in the footer (input line) intentionally
                // focus the chat pane so typing works without
                // re-clicking.
                s.focus = tui::Focus::Chat;
            }
            tui::Hit::Modal => {
                // Modal handles its own dispatch (Enter/Esc) — clicks
                // are absorbed for v1.
            }
            tui::Hit::Menu(action) => {
                // Surface the click as an EditorEvent so the main loop
                // can route it through the same arm that handles
                // Ctrl-, / /. Clicks need access to `live_cfg`, `bus`,
                // and `running` — only the event loop has those.
                drop(s);
                return Some(ppexchanger::tui::EditorEvent::MenuAction(action));
            }
        },
        MouseEventKind::ScrollUp if s.focus == tui::Focus::Chat => s.scroll_back(5),
        MouseEventKind::ScrollDown if s.focus == tui::Focus::Chat => s.scroll_forward(5),
        _ => {}
    }
    None
}

/// If `body` is the path of an existing regular file, return its path.
/// Called only on Submit — never per keystroke — so the syscall cost is
/// negligible. The shape test (separator, leading `./`, extension) keeps a
/// stray word like `pdf` from being treated as a filename.
fn looks_like_existing_file(body: &str) -> Option<PathBuf> {
    use std::path::{Component, Path};
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return None;
    }
    let p = Path::new(trimmed);
    let looks_like_path = p.components().count() > 1
        || trimmed.starts_with('~')
        || trimmed.starts_with("./")
        || trimmed.starts_with(".\\")
        || p.extension().is_some_and(|e| !e.is_empty() && e.len() <= 5);
    if !looks_like_path {
        return None;
    }
    if p.components().any(|c| matches!(c, Component::ParentDir)) {
        return None;
    }
    let meta = std::fs::metadata(p).ok()?;
    if !meta.is_file() {
        return None;
    }
    Some(p.to_path_buf())
}


fn resolve_target(state: &Arc<Mutex<UiState>>, text: &str) -> Option<PeerId> {
    let s = state.lock().unwrap();
    let trimmed = text.trim_start();
    if let Some(rest) = trimmed.strip_prefix('@') {
        // @<name> ... → route by exact name match.
        let name = rest.split_whitespace().next().unwrap_or("");
        if name.is_empty() {
            return None;
        }
        return s.peers.iter().find(|p| p.name == name).map(|p| p.peer_id);
    }
    s.peers
        .iter()
        .find(|p| p.state == tui::PeerState::Connected)
        .map(|p| p.peer_id)
}

/// Strip the leading `@<name>` from a routed message, leaving just the body.
fn strip_routing(text: &str) -> String {
    let trimmed = text.trim_start();
    if let Some(rest) = trimmed.strip_prefix('@') {
        // Skip the first whitespace-delimited token (the name).
        let after_name = rest.split_whitespace().skip(1).collect::<Vec<_>>().join(" ");
        return after_name;
    }
    text.to_string()
}

/// `/theme <name>` switches the active theme and persists it; `/peers`,
/// `/trust <name>`, `/revoke <name>`, `/discover`, `/quit` are passthrough
/// commands. `/discover` opens the modal and spawns a UDP multicast scan +
/// a TCP subnet scan; results stream into the modal via `Event::DiscoveryUpdate`.
#[allow(clippy::too_many_arguments)]
fn handle_command(
    line: &str,
    tx_events: &std::sync::mpsc::Sender<Event>,
    tx_actions: &std::sync::mpsc::Sender<Action>,
    state: &Arc<Mutex<UiState>>,
    cfg: &mut UiConfig,
    cfg_path: &std::path::Path,
    announce_beacon: &ppexchanger::protocol::Beacon,
    self_peer_id: ppexchanger::events::PeerId,
    stop: Arc<AtomicBool>,
) {
    let mut it = line.split_whitespace();
    let cmd = it.next().unwrap_or("");
    match cmd {
        "/discover" => {
            {
                let mut s = state.lock().unwrap();
                s.start_discovery();
            }
            do_discover(
                announce_beacon.clone(),
                self_peer_id,
                tx_events.clone(),
                stop,
            );
        }
        "/map" => {
            // Flip the /discover popup to the Canvas-based map view.
            // Idempotent — opening the popup with /discover resets it
            // to list view, so this is the explicit toggle.
            let mut s = state.lock().unwrap();
            if let Some(d) = s.discovery.as_mut() {
                d.view_map = !d.view_map;
            }
        }
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
                }
            }
        }
        "/theme" => {
            let name = match it.next() {
                Some(n) => n,
                None => {
                    let _ = tx_events.send(Event::Info(format!(
                        "current theme: {} (available: default, solarized, monochrome, neon, amber)",
                        cfg.theme.as_str()
                    )));
                    return;
                }
            };
            match ppexchanger::tui::ThemeName::parse(name) {
                Some(t) => {
                    cfg.theme = t;
                    match save_ui_config(cfg, cfg_path) {
                        Ok(()) => {
                            let _ = tx_events.send(Event::Info(format!(
                                "theme set to {} (saved)",
                                t.as_str()
                            )));
                        }
                        Err(e) => {
                            let _ = tx_events.send(Event::Info(format!(
                                "theme set to {} (save failed: {})",
                                t.as_str(),
                                e
                            )));
                        }
                    }
                }
                None => {
                    let _ = tx_events.send(Event::Info(format!("unknown theme: {}", name)));
                }
            }
        }
        "/quit" => {
            let _ = tx_actions.send(Action::Quit);
        }
        "/send" => {
            // Explicit file transfer — bypass auto-detect. Useful when
            // the path has no extension or the user wants unambiguous
            // behaviour.
            let rest: String = it.collect::<Vec<_>>().join(" ");
            let path = PathBuf::from(rest.trim());
            let pid = {
                let s = state.lock().unwrap();
                s.peers
                    .iter()
                    .find(|p| p.state == tui::PeerState::Connected)
                    .map(|p| p.peer_id)
            };
            if let Some(to) = pid {
                let _ = tx_actions.send(Action::SendFile { to, path });
            } else {
                let _ = tx_events.send(Event::Info(
                    "/send: no connected peer selected".into(),
                ));
            }
        }
        "/settings" => {
            let mut s = state.lock().unwrap();
            s.open_settings(cfg);
        }
        _ => {
            let _ = tx_events.send(Event::Info(format!("unknown command: {}", cmd)));
        }
    }
}

/// Translate a single key event into a settings-modal mutation. Caller
/// owns the lock and persists after `close_after` is reported.
fn route_settings_key(
    k: &crossterm::event::KeyEvent,
    st: &mut ppexchanger::tui::SettingsState,
    cfg: &mut ppexchanger::tui::UiConfig,
    guard: &mut ppexchanger::tui::TuiGuard,
) {
    use crossterm::event::{KeyCode, KeyModifiers};
    use ppexchanger::tui::settings_popup::Tab;
    let code = k.code;
    let mods = k.modifiers;
    // The display-name field is a small inline editor inside Settings. It
    // consumes printable keys before the normal dialog shortcuts so typing a
    // name never changes tabs or toggles unrelated options.
    if st.editing_name {
        match code {
            KeyCode::Enter => {
                st.name_draft = st.name_draft.trim().to_string();
                st.editing_name = false;
            }
            KeyCode::Backspace => {
                st.name_draft.pop();
            }
            KeyCode::Char(c) if mods.is_empty() || mods == KeyModifiers::SHIFT => {
                if st.name_draft.len() < 256 {
                    st.name_draft.push(c);
                }
            }
            _ => {}
        }
        return;
    }
    // Auto-cancel a stale reset-confirm whenever the user does anything
    // other than the 'y' that fires it. The earlier guard arm matches
    // 'y' first; everything else falls through and clears the flag.
    if st.confirm_reset && !matches!(code, KeyCode::Char('y') | KeyCode::Char('Y')) {
        st.confirm_reset = false;
    }
    // Number-row tab jump (1/2/3/4) — works without modifier.
    if mods == KeyModifiers::NONE || mods == KeyModifiers::SHIFT {
        match code {
            KeyCode::Char('1') => st.switch_tab(Tab::Display),
            KeyCode::Char('2') => st.switch_tab(Tab::Input),
            KeyCode::Char('3') => st.switch_tab(Tab::Behavior),
            KeyCode::Char('4') => st.switch_tab(Tab::About),
            KeyCode::Left | KeyCode::Char('h') => match st.tab {
                Tab::Display => match st.selected() {
                    0 => {
                        let _ = st.cycle_theme(-1);
                        cfg.theme = ppexchanger::tui::settings_popup::THEME_CHOICES[st.theme_idx];
                    }
                    2 => st.bump_scrollback(cfg, -100),
                    _ => {}
                },
                Tab::Input => match st.selected() {
                    1 => {
                        // Cycle backwards through status formats.
                        let cur = cfg.status_format;
                        let prev = match cur {
                            StatusFormat::NameOnly => StatusFormat::Off,
                            StatusFormat::NameAddr => StatusFormat::NameOnly,
                            StatusFormat::Off => StatusFormat::NameAddr,
                        };
                        cfg.status_format = prev;
                        st.dirty = true;
                    }
                                        _ => { st.toggle_mouse(cfg); let _ = guard.set_mouse(cfg.mouse); }
                },
                Tab::Behavior => if st.selected() == 2 {
                    // Cycle backwards through status formats.
                    let cur = cfg.status_format;
                    let prev = match cur {
                        StatusFormat::NameOnly => StatusFormat::Off,
                        StatusFormat::NameAddr => StatusFormat::NameOnly,
                        StatusFormat::Off => StatusFormat::NameAddr,
                    };
                    cfg.status_format = prev;
                    st.dirty = true;
                },
                Tab::About => {}
            },
            KeyCode::Right | KeyCode::Char('l') => match st.tab {
                Tab::Display => match st.selected() {
                    0 => {
                        let _ = st.cycle_theme(1);
                        cfg.theme = ppexchanger::tui::settings_popup::THEME_CHOICES[st.theme_idx];
                    }
                    2 => st.bump_scrollback(cfg, 100),
                    _ => {}
                },
                Tab::Input => match st.selected() {
                    1 => {
                        let _ = st.cycle_status_format(cfg);
                    }
                                        _ => { st.toggle_mouse(cfg); let _ = guard.set_mouse(cfg.mouse); }
                },
                Tab::Behavior => if st.selected() == 2 {
                    let _ = st.cycle_status_format(cfg);
                },
                Tab::About => {}
            },
            KeyCode::Up | KeyCode::Char('k') => st.move_selection(-1),
            KeyCode::Down | KeyCode::Char('j') => st.move_selection(1),
            // Reset-confirm: when armed, the next Y fires the reset;
            // any other key (including Enter) cancels. Pre-empts the
            // normal Enter handler below so the selected row doesn't
            // also trigger.
            KeyCode::Char('y') | KeyCode::Char('Y') if st.confirm_reset => {
                st.reset_to_defaults(cfg);
                st.confirm_reset = false;
            }
            KeyCode::Enter | KeyCode::Char(' ') => match st.tab {
                Tab::Display => match st.selected() {
                    0 => {
                        let _ = st.cycle_theme(1);
                        cfg.theme = ppexchanger::tui::settings_popup::THEME_CHOICES[st.theme_idx];
                    }
                    1 => st.toggle_footer(cfg),
                    2 => st.bump_scrollback(cfg, 100),
                    3 => st.toggle_notify_sound(cfg),
                    4 => st.toggle_auto_trust_seen(cfg),
                    _ => {}
                },
                Tab::Input => match st.selected() {
                    0 => { st.toggle_mouse(cfg); let _ = guard.set_mouse(cfg.mouse); }
                    1 => {
                        let _ = st.cycle_status_format(cfg);
                    }
                    2 => {
                        // Arm the reset confirm; next 'y' fires, anything
                        // else cancels via the `_ =>` arm in move_selection.
                        st.confirm_reset = true;
                    }
                    _ => {}
                },
                Tab::Behavior => match st.selected() {
                    0 => st.toggle_notify_sound(cfg),
                    1 => st.toggle_auto_trust_seen(cfg),
                    2 => { let _ = st.cycle_status_format(cfg); }
                    3 => st.editing_name = true,
                    _ => {}
                },
                Tab::About => {}
            },
            KeyCode::Tab => st.switch_tab(st.tab.next_tab()),
            KeyCode::BackTab => st.switch_tab(st.tab.prev_tab()),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn looks_like_existing_file_accepts_real_path() {
        let dir = std::env::temp_dir().join(format!(
            "ppx-llf-accept-{}-{}",
            std::process::id(),
            rand_u64()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("foo.txt");
        std::fs::write(&p, b"hi").unwrap();
        let s = p.to_str().unwrap();
        assert_eq!(looks_like_existing_file(s), Some(p.clone()));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn looks_like_existing_file_rejects_missing() {
        assert_eq!(looks_like_existing_file("/no/such/path/abc.bin"), None);
    }

    #[test]
    fn looks_like_existing_file_rejects_bare_word_without_extension() {
        // "pdf" alone — no separator, no leading dot, no extension —
        // must not trigger a metadata() syscall.
        assert_eq!(looks_like_existing_file("pdf"), None);
        assert_eq!(looks_like_existing_file("hello"), None);
    }

    #[test]
    fn looks_like_existing_file_rejects_parent_traversal() {
        assert_eq!(looks_like_existing_file("../etc/passwd"), None);
        assert_eq!(looks_like_existing_file("foo/../../bar"), None);
    }

    #[test]
    fn looks_like_existing_file_rejects_directory() {
        let dir = std::env::temp_dir();
        let s = dir.to_str().unwrap();
        // An existing directory must not be accepted; only regular files.
        assert_eq!(looks_like_existing_file(s), None);
    }

    fn rand_u64() -> u64 {
        use rand_core::{OsRng, RngCore};
        let mut b = [0u8; 8];
        OsRng.fill_bytes(&mut b);
        u64::from_le_bytes(b)
    }
}
