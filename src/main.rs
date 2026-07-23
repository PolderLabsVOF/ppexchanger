//! `lanchat` CLI entrypoint.
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

use lanchat::config::{config_dir, identity_path};
use lanchat::events::{Action, Bus, Event, PeerId, RegistryMsg};
use lanchat::identity::load_or_create;
use lanchat::net::discovery::Discovery;
use lanchat::net::listener;
use lanchat::net::peer;
use lanchat::net::session::Session;
use lanchat::peerdb::PeerDb;
use lanchat::protocol::{fingerprint as pubkey_fingerprint, Beacon, FrameBody};
use lanchat::tui::{self, UiConfig, UiState};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

const VERSION: &str = env!("CARGO_PKG_VERSION");

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let mut name: Option<String> = None;
    let mut port: u16 = 0;
    let mut mode = Mode::Tui;
    let mut theme_override: Option<lanchat::tui::ThemeName> = None;
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
            "--theme" => {
                i += 1;
                if i >= args.len() {
                    eprintln!("--theme requires an argument");
                    std::process::exit(2);
                }
                match lanchat::tui::ThemeName::parse(&args[i]) {
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
        "lanchat {version} — fully-local LAN P2P encrypted terminal messenger\n\
         \n\
         USAGE:\n  lanchat [--name <name>] [--port <port>] [--theme <name>] [--config <path>] [--no-mouse]\n  lanchat --gen-identity\n  lanchat --help | --version\n\
         \n\
         OPTIONS:\n  --name <name>     display name (overrides stored)\n  --port <port>     TCP listen port (0 = ephemeral)\n  --theme <name>    default|solarized|monochrome|neon\n  --config <path>   path to config.toml (default: $XDG_CONFIG_HOME/lanchat/config.toml on
                    Linux/macOS, %APPDATA%\\lanchat\\config.toml on Windows)\n  --no-mouse        disable mouse capture\n  --gen-identity    generate a new identity and exit\n  --help, -h        print this help\n  --version, -V     print version",
        version = VERSION
    );
}

fn run(
    mode: Mode,
    name: Option<String>,
    port: u16,
    theme_override: Option<lanchat::tui::ThemeName>,
    config_override: Option<PathBuf>,
    mouse_override: Option<bool>,
) {
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
fn save_ui_config(cfg: &lanchat::tui::UiConfig, path: &std::path::Path) -> std::io::Result<()> {
    let body = format_ui_config(cfg);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, body)
}

fn format_ui_config(cfg: &lanchat::tui::UiConfig) -> String {
    let mut out = String::from("# lanchat UI config — generated, edits preserved on next /theme\n");
    out.push_str("[ui]\n");
    out.push_str(&format!("theme = \"{}\"\n", cfg.theme.as_str()));
    out.push_str(&format!("show_footer = {}\n", cfg.show_footer));
    out.push_str(&format!("mouse = {}\n", cfg.mouse));
    out.push_str(&format!("scrollback = {}\n", cfg.scrollback));
    out
}

fn default_config_path() -> PathBuf {
    config_dir().map(|d| d.join("config.toml")).unwrap_or_default()
}

fn start_tui(
    id: lanchat::identity::Identity,
    port: u16,
    theme_override: Option<lanchat::tui::ThemeName>,
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

    let theme = lanchat::tui::Theme::by_name(ui_cfg.theme);
    let glyphs = lanchat::tui::detect_glyphs();

    let bus = Bus::new();
    let state = Arc::new(Mutex::new({
        let mut s = UiState::from_identity(&id);
        s.max_scrollback = ui_cfg.scrollback;
        s
    }));

    // Load persistent contacts and seed the UI.
    let db = PeerDb::load_or_default().unwrap_or_default();
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
    let static_kp: Arc<lanchat::crypto::Keypair> = Arc::new(id.keypair);

    // Listener thread: accepts inbound TCP, runs responder handshake,
    // hands the outbound sender to the action thread via RegistryMsg,
    // and spawns the per-connection session driver.
    let listener_t = {
        let tx = bus.tx_events.clone();
        let kp = Arc::clone(&static_kp);
        let stop2 = Arc::clone(&stop);
        let reg_tx2 = reg_tx.clone();
        let inbound_tx_for_listener = bus.tx_inbound_files.clone();
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
                                    let peer_id = lanchat::net::listener::peer_id_from_pubkey(
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
                                    );
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
        thread::spawn(move || {
            let mut outbound: HashMap<PeerId, mpsc::Sender<FrameBody>> = HashMap::new();
            let mut peer_names: HashMap<PeerId, String> = HashMap::new();
            let mut outbox: lanchat::net::file_xfer::OutboundMap =
                lanchat::net::file_xfer::OutboundMap::new();
            let mut inbox: lanchat::net::file_xfer::InboundMap =
                lanchat::net::file_xfer::InboundMap::new();
            while !act_stop.load(Ordering::Relaxed) {
                // Poll the action channel with a short timeout so we can
                // also drain the registry + inbound-file channels
                // between bursts.
                match act_bus_rx.recv_timeout(Duration::from_millis(100)) {
                    Ok(Action::Connect {
                        addr,
                        name_hint,
                        public_key: _,
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
                        thread::spawn(move || {
                            if let Some((peer_id, discovered)) = peer::connect(
                                addr,
                                Some(name_hint.clone()),
                                &kp_clone,
                                tx_clone.clone(),
                                tx_inbound_clone,
                                reg_clone,
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
                            }
                        });
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
                        let mut db = PeerDb::default();
                        db.revoke(&peer_id);
                        let _ = db.save();
                        outbound.remove(&peer_id);
                        peer_names.remove(&peer_id);
                    }
                    Ok(Action::SendText { to, body }) => {
                        // Optimistic local echo: render the sent line in
                        // the UI immediately so the user sees feedback.
                        let name = peer_names
                            .get(&to)
                            .cloned()
                            .unwrap_or_else(|| "self".into());
                        {
                            let mut s = act_state.lock().unwrap();
                            s.apply(&Event::TextMessage {
                                from_peer: to,
                                from_name: name,
                                body: body.clone(),
                            });
                        }
                        // Push the actual encrypted frame through the
                        // session driver. If the peer is no longer
                        // registered (disconnected mid-send), drop it.
                        if let Some(tx) = outbound.get(&to) {
                            let _ = tx.send(FrameBody::Text(body));
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
                        match lanchat::net::file_xfer::OutboundTransfer::open(
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
                            outbound.insert(peer_id, sender);
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
                    use lanchat::events::InboundFileEvent;
                    match ev {
                        InboundFileEvent::Offer { peer, offer } => {
                            let from_name = peer_names
                                .get(&peer)
                                .cloned()
                                .unwrap_or_else(|| hex(&peer));
                            let accepted = inbox.offer(
                                lanchat::net::file_xfer::InboundTransfer::new(
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
                            use lanchat::net::file_xfer::WriteOutcome;
                            match inbox.write_chunk(id, offset, data) {
                                WriteOutcome::Error(reason) => {
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
                                _ => {}
                            }
                        }
                        InboundFileEvent::Done { peer, id } => {
                            // FileDone on the wire carries only the
                            // FileId — we trust the offer's announced
                            // size for the size check, and use the
                            // peer name from the registry for the
                            // success event.
                            use lanchat::net::file_xfer::FinalizeOutcome;
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
                    use lanchat::net::file_xfer::StepResult;
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

    // TUI loop.
    let _guard = tui::TuiGuard::new(ui_cfg.mouse).unwrap();
    let mut terminal = tui::enter_terminal(ui_cfg.mouse).unwrap();
    let mut editor = lanchat::tui::LineEditor::new();
    // Active mutable copy of the config — `/theme` updates it, so we can
    // persist on change without re-reading from disk.
    let mut live_cfg = ui_cfg;
    let live_cfg_path = cfg_path;

    loop {
        {
            let mut s = state.lock().unwrap();
            tui::drain_events(&bus.rx_events, &mut s);
            // Stable sidebar ordering: Connected > Seen > Gone, then name.
            s.sort_peers();
        }
        {
            let s = state.lock().unwrap();
            if let Err(e) = tui::render(&mut terminal, &s, &theme, &glyphs) {
                eprintln!("render error: {}", e);
                break;
            }
        }
        if crossterm::event::poll(Duration::from_millis(150)).unwrap_or(false) {
            if let Ok(ev) = crossterm::event::read() {
                match editor.on_key(&ev) {
                    lanchat::tui::EditorEvent::Submit(text) => {
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
                            let _ = bus.tx_actions.send(Action::SendText {
                                to: target,
                                body: strip_routing(&text),
                            });
                        } else {
                            let _ = bus.tx_events.send(Event::Info(
                                "no peer selected or matched".into(),
                            ));
                        }
                    }
                    lanchat::tui::EditorEvent::Cancel => {
                        let _ = bus.tx_actions.send(Action::Quit);
                        break;
                    }
                    lanchat::tui::EditorEvent::Quit => {
                        let _ = bus.tx_actions.send(Action::Quit);
                        break;
                    }
                    lanchat::tui::EditorEvent::FocusNext => {
                        let mut s = state.lock().unwrap();
                        s.cycle_focus();
                    }
                    lanchat::tui::EditorEvent::ToggleTrust => {
                        let pid = {
                            let s = state.lock().unwrap();
                            s.selected().map(|p| p.peer_id)
                        };
                        if let Some(pid) = pid {
                            let _ = bus.tx_actions.send(Action::Trust { peer_id: pid });
                        }
                    }
                    lanchat::tui::EditorEvent::RevokePeer => {
                        let pid = {
                            let s = state.lock().unwrap();
                            s.selected().map(|p| p.peer_id)
                        };
                        if let Some(pid) = pid {
                            let _ = bus.tx_actions.send(Action::Revoke { peer_id: pid });
                        }
                    }
                    lanchat::tui::EditorEvent::NewChat => {
                        // For v1 this is a no-op visual hint; peer selection
                        // is via Up/Down on the sidebar after Tab.
                        let _ = bus.tx_events.send(Event::Info(
                            "use Tab to focus the sidebar, then Up/Down to pick a peer".into(),
                        ));
                    }
                    lanchat::tui::EditorEvent::ToggleHelp => {
                        let mut s = state.lock().unwrap();
                        s.show_help = !s.show_help;
                    }
                    lanchat::tui::EditorEvent::PageUp => {
                        let mut s = state.lock().unwrap();
                        if s.focus == tui::Focus::Chat {
                            s.scroll_back(5);
                        } else {
                            s.move_selection(-1);
                        }
                    }
                    lanchat::tui::EditorEvent::PageDown => {
                        let mut s = state.lock().unwrap();
                        if s.focus == tui::Focus::Chat {
                            s.scroll_forward(5);
                        } else {
                            s.move_selection(1);
                        }
                    }
                    lanchat::tui::EditorEvent::ClearInput
                    | lanchat::tui::EditorEvent::Clear => {
                        // Editor already cleared its buffer. If a modal is
                        // open, Esc also closes it.
                        let mut s = state.lock().unwrap();
                        if s.show_help {
                            s.show_help = false;
                        } else if s.discovery.is_some() {
                            s.close_discovery();
                        }
                    }
                    lanchat::tui::EditorEvent::HistoryPrev
                    | lanchat::tui::EditorEvent::HistoryNext
                    | lanchat::tui::EditorEvent::Edited
                    | lanchat::tui::EditorEvent::None => {}
                }
                let mut s = state.lock().unwrap();
                let prefix = match s.focus {
                    tui::Focus::Sidebar => format!("[sidebar] > {}", editor.as_str()),
                    tui::Focus::Chat => format!("> {}", editor.as_str()),
                };
                s.status = prefix;
            }
        }
        if stop.load(Ordering::Relaxed) {
            break;
        }
    }

    stop.store(true, Ordering::Relaxed);
    // Discovery threads self-terminate on the stop flag; we don't track
    // their handles here. Listener and action-consumer threads are joined.
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

/// Spawn one scan per method, post each result as `Event::DiscoveryUpdate`,
/// then `DiscoveryFinished` once all are in. The UI thread updates the
/// modal state from those events.
fn do_discover(
    beacon: Beacon,
    self_peer_id: lanchat::events::PeerId,
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

    // Method 2: TCP subnet scan. Walks local /24 for hosts accepting TCP on
    // the announced port.
    let tx_tcp = tx.clone();
    let tcp_port = beacon.tcp_port;
    thread::spawn(move || {
        let addrs = match lanchat::net::scan::scan_local_subnet(
            tcp_port,
            lanchat::net::scan::SCAN_HOSTS,
        ) {
            Ok(v) => v,
            Err(e) => {
                let _ = tx_tcp.send(Event::Info(format!("tcp scan failed: {}", e)));
                Vec::new()
            }
        };
        let peers = addrs
            .into_iter()
            .map(|a| lanchat::events::DiscoveredPeer {
                name: None,
                addr: std::net::SocketAddr::V4(a),
                fingerprint: None,
            })
            .collect();
        let _ = tx_tcp.send(Event::DiscoveryUpdate {
            method: format!("TCP subnet scan (port {})", tcp_port),
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
) -> std::io::Result<Vec<lanchat::events::DiscoveredPeer>> {
    let d = Discovery::bind(0)?;
    let _ = d.announce(beacon);
    let deadline = std::time::Instant::now() + window;
    let mut seen: std::collections::HashMap<lanchat::events::PeerId, lanchat::events::DiscoveredPeer> =
        std::collections::HashMap::new();
    while std::time::Instant::now() < deadline && !stop.load(Ordering::Relaxed) {
        if let Ok(Some((src, b))) = d.recv_beacon() {
            if b.peer_id == beacon.peer_id {
                continue;
            }
            let tcp_addr: SocketAddr = (src.ip(), b.tcp_port).into();
            seen.entry(b.peer_id).or_insert(lanchat::events::DiscoveredPeer {
                name: if b.name.is_empty() { None } else { Some(b.name) },
                addr: tcp_addr,
                fingerprint: Some(pubkey_fingerprint(&b.public_key)),
            });
        }
    }
    Ok(seen.into_values().collect())
}

/// Resolve a `@<name> ...` routing prefix. Returns the peer_id targeted by
/// the message. Bare text resolves to the currently-selected peer (first
/// connected peer if none selected).
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
fn handle_command(
    line: &str,
    tx_events: &std::sync::mpsc::Sender<Event>,
    tx_actions: &std::sync::mpsc::Sender<Action>,
    state: &Arc<Mutex<UiState>>,
    cfg: &mut UiConfig,
    cfg_path: &std::path::Path,
    announce_beacon: &lanchat::protocol::Beacon,
    self_peer_id: lanchat::events::PeerId,
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
                        "current theme: {} (available: default, solarized, monochrome, neon)",
                        cfg.theme.as_str()
                    )));
                    return;
                }
            };
            match lanchat::tui::ThemeName::parse(name) {
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
        _ => {
            let _ = tx_events.send(Event::Info(format!("unknown command: {}", cmd)));
        }
    }
}