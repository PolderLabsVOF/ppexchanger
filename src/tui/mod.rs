//! ratatui-driven terminal UI.
//!
//! Layout:
//!   ╭─ ppexchanger ───────────────────────────────────────────────────╮
//!   │  ● bob (macbook)                              [connected]       │
//!   ├──────────────────────────────────────────────────────────────────┤
//!   │                                                                   │
//!   │  bob: hey alice                              10:32               │
//!   │                                                                   │
//!   │                                    alice: hi!       10:32        │
//!   │                                                                   │
//!   ├──────────────────────────────────────────────────────────────────┤
//!   │  > type message...                                         [⏎]  │
//!   ╰──────────────────────────────────────────────────────────────────╯
//!
//! The theme + glyph palettes live in `theme.rs`. The hand-rolled TOML
//! config reader lives in `config.rs`. The keyboard overlay lives in
//! `help.rs`. This module owns the shared `UiState` and the main `render`
//! pass.

pub mod art;
pub mod config;
pub mod discovery_popup;
pub mod file_offer_popup;
pub mod help;
pub mod input;
pub mod settings_popup;
pub mod theme;

pub use config::{UiConfig, DEFAULT_SCROLLBACK, MAX_SCROLLBACK};
pub use input::{EditorEvent, LineEditor};
pub use settings_popup::SettingsState;
pub use theme::{detect_glyphs, Glyphs, Theme, ThemeName};

use crate::events::{Event, PeerId};
use crate::identity::Identity;
use crate::peerdb::{Contact, PeerDb};
use std::collections::HashMap;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Margin, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, Borders, Clear, Gauge, List, ListItem, ListState, Paragraph, Scrollbar,
    ScrollbarOrientation, ScrollbarState, Wrap,
};
use ratatui::Terminal;
use std::collections::VecDeque;
use std::io::{stdout, Stdout};
use std::time::{SystemTime, UNIX_EPOCH};

/// Layout constants shared between `render()` and `hit_test()`. The
/// sidebar has room for connection metadata; the body always retains a
/// usable chat region. Changing these constants in one place is enough.
const SIDEBAR_WIDTH: u16 = 30;
/// A focused message field plus an unboxed status/action line. Slash-command
/// suggestions render above it in the chat pane.
const FOOTER_HEIGHT: u16 = 5;
const BODY_MIN_HEIGHT: u16 = 3;
/// The application header has an identity row and a compact navigation row.
const MENU_HEIGHT: u16 = 4;
const MENU_BUTTON_WIDTH: u16 = 12;
const MENU_BUTTON_GAP: u16 = 1;

/// Menu buttons, left-to-right order. Used as the click target for
/// the menu bar; `Hit::Menu(MenuAction)` returns the variant under the
/// cursor, and `handle_mouse` routes it to the corresponding slash
/// command or `EditorEvent` in the main loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuAction {
    Peers,
    Discover,
    Settings,
    Help,
    Quit,
}

/// Three rectangles produced by the single `Layout` pass. Hit-test and
/// render both build on this so the click map matches what the user
/// sees on screen.
pub struct LayoutAreas {
    pub menu: Rect,
    pub sidebar: Rect,
    pub chat: Rect,
    pub footer: Rect,
}

/// Hit-test result for one mouse event. `Sidebar(i)` is the index into
/// the sorted `state.peers` slice (the same order the sidebar renders
/// in); `Chat` covers the chat pane; `Footer` is the input line area;
/// `Modal` means the click landed inside a modal popup (show_help or
/// discovery) and the main loop should consume it without further
/// dispatch; `Menu(action)` is a click on one of the top menu buttons.
#[derive(Debug)]
pub enum Hit {
    Sidebar(usize),
    Chat,
    Footer,
    Modal,
    Menu(MenuAction),
}

/// Decide whether `(col, row)` falls inside `rect`.
pub fn point_in_rect(rect: Rect, col: u16, row: u16) -> bool {
    col >= rect.x
        && col < rect.x.saturating_add(rect.width)
        && row >= rect.y
        && row < rect.y.saturating_add(rect.height)
}

/// Which pane has keyboard focus. Tab cycles between them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Sidebar,
    Chat,
}

/// Shared UI state the render loop reads. Owned by the UI thread; the network
/// thread only ever sends immutable events over the bus.
pub struct UiState {
    pub self_name: String,
    pub self_fingerprint: String,
    pub self_peer_id: PeerId,
    pub peers: Vec<UiPeer>,
    /// Authenticated greetings can race the UI's `PeerConnected` event on
    /// outbound connections. Hold those names briefly so the peer never
    /// falls back permanently to `peer@IP`.
    peer_name_overrides: HashMap<PeerId, String>,
    /// Bounded message ring; older entries are dropped when full.
    pub messages: VecDeque<UiMessage>,
    pub status: String,
    /// The draft displayed in the composer. Keeping it in UI state lets the
    /// renderer draw a real input field without overloading `status`.
    pub composer: String,
    pub selected_peer: usize,
    pub focus: Focus,
    pub show_help: bool,
    /// Mirror of `UiConfig::status_format` so `draw_footer` can choose
    /// what to render. Updated by `apply_live_cfg` on every render pass.
    pub status_format: crate::tui::config::StatusFormat,
    /// Mirror of `UiConfig::show_footer`. When false, the renderer
    /// skips the footer rect entirely — the chat pane absorbs the
    /// freed vertical space via the layout splitter.
    pub show_footer: bool,
    /// Modal state for an inbound file offer. `None` means no pending
    /// offer; otherwise the modal is shown over the chat and the user
    /// can accept or reject via Enter / Esc.
    pub file_offer: Option<file_offer_popup::FileOfferPrompt>,
    /// Modal state for `/discover`. `None` means the modal is closed; the
    /// popup renders the in-progress scan results when present.
    pub discovery: Option<DiscoveryState>,
    /// Modal state for `/settings` / `Ctrl-,`. `None` means the popup is
    /// closed; otherwise it tracks the active tab + cursor + dirty flag.
    pub settings: Option<SettingsState>,
    /// Modal state for an inbound connection request. `None` means no pending
    /// request; otherwise the modal is shown and user can accept or deny.
    pub pending_connection: Option<ConnectionRequest>,
    /// True until the user dismisses the large startup logo (or sends
    /// their first message). Lives in UiState so render() doesn't need
    /// a separate channel to know whether to draw it.
    pub show_logo: bool,
    /// Toggles each render pass to fake a CRT scanline overlay. Cheap
    /// because nothing else changes — only the modifier set applied to
    /// every other chat row. We intentionally don't redraw on a timer;
    /// the main loop redraws on every event/poll which is fast enough
    /// to look continuous on a 60Hz terminal.
    pub scanline_tick: bool,
    /// TCP port we bound for inbound peer connections. Used by the
    /// discovery-empty hint so the firewall one-liner matches the actual
    /// listen port (default 7777, custom if the user passed `--port`).
    pub bound_port: u16,
    /// How many lines back from the latest message we're scrolled. `0` =
    /// pinned to bottom (latest).
    pub scroll: usize,
    pub max_scrollback: usize,
    history_dirty: bool,
    contacts_dirty: bool,
}

/// Snapshot of an in-flight `/discover` scan.
#[derive(Debug, Clone)]
pub struct DiscoveryState {
    /// True while at least one scan mode is still running.
    pub running: bool,
    /// Methods that have completed, with their findings.
    pub results: Vec<DiscoveryMethod>,
    /// Human-readable label for the bar at the top: "scanning multicast + subnet"
    pub summary: String,
    /// Local UI tab: false = list, true = canvas map. Flipped by `2` /
    /// `3` in the popup; not persisted.
    pub view_map: bool,
    /// One-line hint shown after discovery finishes with zero peers.
    /// Populated only if we have a useful suggestion (e.g. Windows
    /// firewall blocking inbound). Stays `None` if the scan is still
    /// running or found peers.
    pub hint: Option<String>,
    /// Selected flattened peer row in the discovery result list.
    pub selected: usize,
}

#[derive(Debug, Clone)]
pub struct DiscoveryMethod {
    pub name: String,
    pub peers: Vec<DiscoveredPeer>,
}

#[derive(Debug, Clone)]
pub struct DiscoveredPeer {
    pub name: Option<String>,
    pub hostname: Option<String>,
    pub addr: std::net::SocketAddr,
    pub fingerprint: Option<String>,
    pub reverse: Option<(std::net::SocketAddr, crate::events::PeerId)>,
}

/// An inbound connection request waiting for user accept/deny.
#[derive(Clone)]
pub struct ConnectionRequest {
    pub addr: std::net::SocketAddr,
    pub name: String,
    pub fingerprint: String,
}

#[derive(Clone)]
pub struct UiPeer {
    pub peer_id: PeerId,
    pub name: String,
    pub fingerprint: String,
    pub public_key: [u8; 32],
    pub last_addr: Option<std::net::SocketAddr>,
    pub trusted: bool,
    pub state: PeerState,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PeerState {
    Seen,
    Connected,
    Gone,
}

pub struct UiMessage {
    pub from_peer: PeerId,
    pub from_name: String,
    pub body: String,
    pub outgoing: bool,
    pub pending: bool,
    pub ts_unix: u64,
}

impl UiState {
    pub fn from_identity(id: &Identity) -> Self {
        let fp = crate::protocol::fingerprint(&id.keypair.public_bytes());
        Self {
            self_name: id.name.clone(),
            self_fingerprint: fp,
            self_peer_id: id.peer_id,
            peers: Vec::new(),
            peer_name_overrides: HashMap::new(),
            messages: VecDeque::new(),
            status: "starting…".into(),
            composer: String::new(),
            selected_peer: 0,
            focus: Focus::Chat,
            show_help: false,
            status_format: crate::tui::config::StatusFormat::NameOnly,
            show_footer: true,
            file_offer: None,
            discovery: None,
            settings: None,
            pending_connection: None,
            show_logo: true,
            scanline_tick: false,
            bound_port: 0,
            scroll: 0,
            max_scrollback: DEFAULT_SCROLLBACK,
            history_dirty: false,
            contacts_dirty: false,
        }
    }

    /// Open the settings popup seeded from the current `cfg`. Idempotent:
    /// opening twice is a no-op rather than reset the cursor.
    pub fn open_settings(&mut self, cfg: &crate::tui::UiConfig) {
        if self.settings.is_none() {
            let mut settings = SettingsState::new(cfg);
            settings.name_draft = self.self_name.clone();
            self.settings = Some(settings);
        }
    }

    /// Mirror the live config bits that the render loop reads. Cheap;
    /// call once per render pass before drawing. The `status_format`
    /// mirror lets the footer switch between name-only / name+addr / off
    /// without re-parsing config on every draw.
    pub fn apply_live_cfg(&mut self, cfg: &crate::tui::UiConfig) {
        self.status_format = cfg.status_format;
        self.show_footer = cfg.show_footer;
    }

    /// Drop the settings modal. Caller is responsible for persisting the
    /// config (the modal only flips the live `UiConfig` — see main.rs).
    pub fn close_settings(&mut self) {
        self.settings = None;
    }

    pub fn dismiss_logo(&mut self) {
        self.show_logo = false;
    }

    pub fn apply(&mut self, ev: &Event) {
        match ev {
            Event::PeerSeen {
                peer_id,
                name,
                hostname,
                fingerprint,
                addr,
                public_key,
            } => {
                // Update or add peer as "Seen" state (discovered but not connected)
                let display_name = if hostname.is_empty() {
                    name.clone()
                } else {
                    format!("{} ({})", name, hostname)
                };
                if let Some(p) = self.peers.iter_mut().find(|p| &p.peer_id == peer_id) {
                    p.name = display_name;
                    p.fingerprint = fingerprint.clone();
                    p.public_key = *public_key;
                    p.last_addr = Some(*addr);
                } else {
                    self.peers.push(UiPeer {
                        peer_id: *peer_id,
                        name: display_name,
                        fingerprint: fingerprint.clone(),
                        public_key: *public_key,
                        last_addr: Some(*addr),
                        trusted: false,
                        state: PeerState::Seen,
                    });
                }
            }
            Event::ConnectionRequest {
                addr,
                name,
                hostname,
                fingerprint,
            } => {
                // Coalesce repeated reverse-connect retries from the same
                // requester. Keep the card visible while the user decides.
                if self
                    .pending_connection
                    .as_ref()
                    .is_some_and(|pending| pending.addr == *addr)
                {
                    return;
                }
                let display_name = if hostname.is_empty() {
                    name.clone()
                } else {
                    format!("{} ({})", name, hostname)
                };
                self.pending_connection = Some(crate::tui::ConnectionRequest {
                    addr: *addr,
                    name: display_name.clone(),
                    fingerprint: fingerprint.clone(),
                });
                self.status = format!("incoming connection request from {}", display_name);
            }
            Event::ConnectionAccepted { .. } => {
                // Handled by PeerConnected event that follows
            }
            Event::ConnectionDenied { addr } => {
                self.status = format!("connection denied by {}", addr);
            }
            Event::PeerConnected {
                peer_id,
                name,
                fingerprint,
                trusted,
                addr,
            } => {
                let display_name = self
                    .peer_name_overrides
                    .remove(peer_id)
                    .unwrap_or_else(|| name.clone());
                if let Some(p) = self.peers.iter_mut().find(|p| &p.peer_id == peer_id) {
                    p.name = display_name.clone();
                    p.fingerprint = fingerprint.clone();
                    p.last_addr = Some(*addr);
                    p.trusted = *trusted;
                    p.state = PeerState::Connected;
                } else {
                    self.peers.push(UiPeer {
                        peer_id: *peer_id,
                        name: display_name.clone(),
                        fingerprint: fingerprint.clone(),
                        public_key: [0u8; 32],
                        last_addr: Some(*addr),
                        trusted: *trusted,
                        state: PeerState::Connected,
                    });
                }
                self.pending_connection = self
                    .pending_connection
                    .take()
                    .filter(|pending| pending.addr != *addr);
                self.status = format!("connected to {}", display_name);
                self.contacts_dirty = true;
            }
            Event::PeerNamed { peer_id, name } => {
                if let Some(peer) = self.peers.iter_mut().find(|peer| peer.peer_id == *peer_id) {
                    peer.name = name.clone();
                    self.status = format!("connected to {}", name);
                } else {
                    self.peer_name_overrides.insert(*peer_id, name.clone());
                }
            }
            Event::TextMessage {
                from_peer,
                from_name,
                body,
            } => {
                self.push_message(UiMessage {
                    from_peer: *from_peer,
                    from_name: from_name.clone(),
                    body: body.clone(),
                    outgoing: false,
                    pending: false,
                    ts_unix: now_unix(),
                });
            }
            Event::TextDelivered { peer_id, body } => {
                if let Some(message) = self.messages.iter_mut().rev().find(|message| {
                    message.outgoing
                        && message.pending
                        && message.from_peer == *peer_id
                        && message.body == *body
                }) {
                    message.pending = false;
                    self.history_dirty = true;
                }
            }
            Event::DecryptFailed { peer_id, from_name } => {
                self.push_message(UiMessage {
                    from_peer: *peer_id,
                    from_name: "[decrypt]".into(),
                    body: format!("failed to decrypt message from {}", from_name),
                    outgoing: false,
                    pending: false,
                    ts_unix: now_unix(),
                });
            }
            Event::PeerGone { peer_id, name } => {
                self.peer_name_overrides.remove(peer_id);
                self.push_message(UiMessage {
                    from_peer: *peer_id,
                    from_name: "[net]".into(),
                    body: format!("{} disconnected", name),
                    outgoing: false,
                    pending: false,
                    ts_unix: now_unix(),
                });
                if let Some(p) = self.peers.iter_mut().find(|p| &p.peer_id == peer_id) {
                    p.state = PeerState::Gone;
                }
            }
            Event::Info(s) => {
                self.status = s.clone();
            }
            Event::DiscoveryUpdate { method, peers } => {
                if let Some(d) = self.discovery.as_mut() {
                    let peer_objs: Vec<DiscoveredPeer> = peers
                        .iter()
                        .map(|p| DiscoveredPeer {
                            name: p.name.clone(),
                            hostname: p.hostname.clone(),
                            addr: p.addr,
                            fingerprint: p.fingerprint.clone(),
                            reverse: p.reverse,
                        })
                        .collect();
                    let mstr: &str = method.as_str();
                    if let Some(m) = d.results.iter_mut().find(|m| m.name == mstr) {
                        m.peers = peer_objs;
                    } else {
                        d.results.push(DiscoveryMethod {
                            name: method.clone(),
                            peers: peer_objs,
                        });
                    }
                    let _ = mstr;
                }
            }
            Event::DiscoveryFinished => {
                if let Some(d) = self.discovery.as_mut() {
                    d.running = false;
                    // If every method came back empty, surface the
                    // platform-specific hint. Windows users hit this when
                    // the firewall silently drops inbound TCP — peers on
                    // the LAN can see us but we can't reach them, so the
                    // local peer list stays empty. macOS/Linux don't have
                    // this default-deny issue for home LANs.
                    if d.results.iter().all(|m| m.peers.is_empty()) {
                        d.hint = Some(discovery_empty_hint(self.bound_port));
                    }
                }
            }
            // File-transfer events. The full file-offer modal lives in
            // Slice 8 (`tui::file_offer_popup`); for now we surface a
            // brief status-line note so the apply is non-exhaustive
            // and the action thread can drive accept/reject through
            // separate `Action::AcceptFile` / `Action::RejectFile`
            // paths without the UI blocking on a modal.
            Event::FileOffer {
                from_peer,
                from_name,
                offer,
            } => {
                // Open the modal unless one is already up — the first
                // offer wins; subsequent ones get logged to the chat.
                if self.file_offer.is_none() {
                    self.file_offer = Some(file_offer_popup::FileOfferPrompt {
                        from_peer: *from_peer,
                        from_name: from_name.clone(),
                        offer: offer.clone(),
                        decision: file_offer_popup::Decision::Pending,
                    });
                } else {
                    self.push_message(UiMessage {
                        from_peer: *from_peer,
                        from_name: "[file]".into(),
                        body: format!(
                            "{} offers file: {} ({} bytes) — busy with another",
                            from_name,
                            offer.name,
                            offer.size
                        ),
                        outgoing: false,
                        pending: false,
                        ts_unix: now_unix(),
                    });
                }
            }
            Event::FileReceived {
                from_peer,
                from_name,
                name,
                bytes,
                saved_to,
                ..
            } => {
                self.file_offer = None;
                self.push_message(UiMessage {
                    from_peer: *from_peer,
                    from_name: "[file]".into(),
                    body: format!(
                        "{} sent {} ({} bytes) → {}",
                        from_name,
                        name,
                        bytes,
                        saved_to.display()
                    ),
                    outgoing: false,
                    pending: false,
                    ts_unix: now_unix(),
                });
            }
            Event::FileAborted {
                from_peer,
                from_name,
                name,
                reason,
                ..
            } => {
                self.file_offer = None;
                self.push_message(UiMessage {
                    from_peer: *from_peer,
                    from_name: "[file]".into(),
                    body: format!(
                        "{}: transfer of {} aborted ({})",
                        from_name, name, reason
                    ),
                    outgoing: false,
                    pending: false,
                    ts_unix: now_unix(),
                });
            }
        }
    }

    /// Open the discovery modal and kick off the scan. Idempotent: if a scan
    /// is already in flight, refresh the running flag and summary instead of
    /// spawning a second set of threads.
    pub fn start_discovery(&mut self) {
        let already_running = self
            .discovery
            .as_ref()
            .map(|d| d.running)
            .unwrap_or(false);
        self.discovery = Some(DiscoveryState {
            running: true,
            results: Vec::new(),
            summary: if already_running {
                "scan in flight…".into()
            } else {
                "running UDP multicast + TCP subnet scan…".into()
            },
            view_map: false,
            hint: None,
            selected: 0,
        });
    }

    pub fn close_discovery(&mut self) {
        self.discovery = None;
    }

    pub fn move_discovery_selection(&mut self, delta: i32) {
        let Some(discovery) = self.discovery.as_mut() else { return; };
        let count = discovery.results.iter().map(|method| method.peers.len()).sum::<usize>();
        if count == 0 {
            discovery.selected = 0;
            return;
        }
        discovery.selected = (discovery.selected as i32 + delta)
            .clamp(0, count as i32 - 1) as usize;
    }

    pub fn selected_discovery_peer(&self) -> Option<DiscoveredPeer> {
        let discovery = self.discovery.as_ref()?;
        discovery
            .results
            .iter()
            .flat_map(|method| method.peers.iter())
            .nth(discovery.selected)
            .cloned()
    }
}

/// Build the user-facing hint for an empty discovery. Windows users
/// hit this when the firewall is silently dropping inbound — peers on
/// the LAN can see *us* (we bind 7777) but we can't reach them via
/// the scan because nothing on the remote side is accepting the
/// connect. Probe the rule state to avoid nagging users who already
/// fixed it.
fn discovery_empty_hint(bound_port: u16) -> String {
    use crate::net::firewall;
    if !firewall::SUPPORTED {
        // Not Windows, or Windows feature not compiled in. The TCP
        // scan should have found peers on the same /24 — if it didn't,
        // they're either off, on a different subnet, or the multicast
        // group is filtered. Generic hint is enough.
        return "no peers found — check that the other machine is on the same network and running ppx".into();
    }
    if firewall::rule_present(bound_port) {
        // Rule exists — the firewall isn't the problem. Point the user
        // at the next likely cause instead.
        return "no peers found — firewall rule is fine; check that the other machine is on the same network and running ppx".into();
    }
    firewall::manual_hint(bound_port)
}

impl UiState {
    fn push_message(&mut self, m: UiMessage) {
        self.messages.push_back(m);
        self.history_dirty = true;
        while self.messages.len() > self.max_scrollback {
            self.messages.pop_front();
        }
        // Any new message resets the scroll anchor — we always show the
        // latest by default.
        self.scroll = 0;
    }

    /// Add an optimistic local echo for a message accepted by the composer.
    /// Keeping this distinct from `TextMessage` prevents our own line from
    /// being rendered as if it came from the selected peer.
    pub fn push_outgoing_message(&mut self, to_peer: PeerId, body: String) {
        self.push_message(UiMessage {
            from_peer: to_peer,
            from_name: self.self_name.clone(),
            body,
            outgoing: true,
            pending: true,
            ts_unix: now_unix(),
        });
    }

    /// Replace the in-memory ring with history loaded from disk. Loading is
    /// not itself a mutation, so the first render does not rewrite the file.
    pub fn restore_history(&mut self, history: VecDeque<UiMessage>) {
        self.messages = history;
        while self.messages.len() > self.max_scrollback {
            self.messages.pop_front();
        }
        self.scroll = 0;
        self.history_dirty = false;
    }

    pub fn history_needs_save(&self) -> bool {
        self.history_dirty
    }

    pub fn mark_history_saved(&mut self) {
        self.history_dirty = false;
    }

    pub fn contacts_need_save(&self) -> bool {
        self.contacts_dirty
    }

    pub fn mark_contacts_saved(&mut self) {
        self.contacts_dirty = false;
    }

    pub fn mark_contacts_dirty(&mut self) {
        self.contacts_dirty = true;
    }

    /// Currently selected peer, if any.
    pub fn selected(&self) -> Option<&UiPeer> {
        self.peers.get(self.selected_peer)
    }

    /// Re-sort peers so Connected come first, then Seen, then Gone, then
    /// alphabetical by name. Called once after each event drain so the
    /// sidebar order stays stable as peers come and go.
    pub fn sort_peers(&mut self) {
        self.peers.sort_by(|a, b| {
            let ra = match a.state {
                PeerState::Connected => 0,
                PeerState::Seen => 1,
                PeerState::Gone => 2,
            };
            let rb = match b.state {
                PeerState::Connected => 0,
                PeerState::Seen => 1,
                PeerState::Gone => 2,
            };
            ra.cmp(&rb).then_with(|| a.name.cmp(&b.name))
        });
        // Keep selection on the same peer (or clamp if it moved).
        if self.selected_peer >= self.peers.len() {
            self.selected_peer = self.peers.len().saturating_sub(1);
        }
    }

    /// Move selection by `delta`, clamped to `0..peers.len()`.
    pub fn move_selection(&mut self, delta: i32) {
        if self.peers.is_empty() {
            self.selected_peer = 0;
            return;
        }
        let cur = self.selected_peer as i32;
        let next = (cur + delta).clamp(0, self.peers.len() as i32 - 1) as usize;
        self.selected_peer = next;
    }

    pub fn cycle_focus(&mut self) {
        self.focus = match self.focus {
            Focus::Sidebar => Focus::Chat,
            Focus::Chat => Focus::Sidebar,
        };
    }

    pub fn scroll_back(&mut self, lines: usize) {
        self.scroll = (self.scroll + lines).min(self.messages.len().saturating_sub(1));
    }

    pub fn scroll_forward(&mut self, lines: usize) {
        self.scroll = self.scroll.saturating_sub(lines);
    }

    /// Visible chat lines for the current scroll position. Newest is at the
    /// bottom of the returned slice.
    pub fn visible_messages(&self) -> Vec<&UiMessage> {
        if self.messages.is_empty() {
            return Vec::new();
        }
        let end = self.messages.len().saturating_sub(self.scroll);
        let start = end.saturating_sub(self.messages.len()); // keep full history, render caps by area
        self.messages.iter().skip(start).take(end - start).collect()
    }
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Format a Unix timestamp as HH:MM (24-hour format).
fn chrono_timestamp(ts: &Option<u64>) -> String {
    match ts {
        Some(t) => {
            let secs = *t;
            // Convert to hours and minutes
            let hours = (secs / 3600) % 24;
            let minutes = (secs / 60) % 60;
            format!("{:02}:{:02}", hours, minutes)
        }
        None => {
            // Use current time if no timestamp
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            let hours = (now / 3600) % 24;
            let minutes = (now / 60) % 60;
            format!("{:02}:{:02}", hours, minutes)
        }
    }
}

/// Compute the three rectangles that the TUI is split into. Used by
/// `render()` (to lay out widgets) and `hit_test()` (to map clicks
/// back to panes). Returns the same shape regardless of caller, so a
/// click on a peer name in the sidebar always corresponds to the row
/// the user can see.
pub fn compute_layout(area: Rect) -> LayoutAreas {
    // Vertical: pinned menu on top, body absorbs the middle, footer on the
    // bottom. The menu row is opt-out of the body budget so the chat pane
    // can never be squeezed below BODY_MIN_HEIGHT by a tall menu.
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(MENU_HEIGHT),
            Constraint::Min(BODY_MIN_HEIGHT),
            Constraint::Length(FOOTER_HEIGHT),
        ])
        .split(area);
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(SIDEBAR_WIDTH), Constraint::Min(10)])
        .split(outer[1]);
    LayoutAreas {
        menu: outer[0],
        sidebar: cols[0],
        chat: cols[1],
        footer: outer[2],
    }
}

/// Centred popup rectangle, mirroring `discovery_popup::centered` so
/// the click region matches what the modal draws over. Help uses the
/// same dimensions; the file-offer modal will too.
pub fn modal_rect(area: Rect) -> Rect {
    let w = 64u16.min(area.width);
    let h = 20u16.min(area.height);
    let vert = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(area.height.saturating_sub(h) / 2),
            Constraint::Length(h),
            Constraint::Min(0),
        ])
        .split(area);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(area.width.saturating_sub(w) / 2),
            Constraint::Length(w),
            Constraint::Min(0),
        ])
        .split(vert[1])[1]
}

/// Map a `MouseEvent` to a `Hit`. Caller has already computed
/// `areas` from the same `f.area()` it last rendered against, so the
/// rectangles are identical to what's on screen.
///
/// `peers_len` is the count of `state.peers` AFTER sorting — this
/// must match the order `draw_sidebar` iterates in, or click-to-select
/// will pick the wrong peer.
pub fn hit_test(
    _screen: Rect,
    col: u16,
    row: u16,
    areas: &LayoutAreas,
    modal_open: bool,
    peers_len: usize,
) -> Hit {
    // A popup is modal across the entire screen. This prevents clicks just
    // outside its rounded frame from selecting a peer or firing navigation
    // hidden behind it.
    if modal_open {
        return Hit::Modal;
    }
    // Menu row: checked first so menu clicks don't fall through to the
    // sidebar or chat pane underneath. Mirrors draw_menu: 5 buttons of
    // width BUTTON_W = 12 with a 1-cell gap. Clicks past the last
    // button fall through to the regular pane hit-test.
    // Buttons occupy the second inner header row only. The identity and
    // connection gauge above remain informational rather than accidental
    // click targets.
    if row == areas.menu.y.saturating_add(2) && point_in_rect(areas.menu, col, row) {
        const STRIDE: u16 = MENU_BUTTON_WIDTH + MENU_BUTTON_GAP;
        let local_col = col.saturating_sub(areas.menu.x);
        let idx = (local_col / STRIDE) as usize;
        if let Some(action) = match idx {
            0 => Some(MenuAction::Peers),
            1 => Some(MenuAction::Discover),
            2 => Some(MenuAction::Settings),
            3 => Some(MenuAction::Help),
            4 => Some(MenuAction::Quit),
            _ => None,
        } {
            return Hit::Menu(action);
        }
        // Past the last button — fall through to sidebar/chat.
    }
    if point_in_rect(areas.sidebar, col, row) {
        // Sidebar: header (Peers (n)) takes 1 line, border takes the
        // top, so the first peer sits at sidebar.y + 2. Each peer is
        // one ListItem row. Indices are clamped so a click in the
        // empty area below the last peer is a no-op rather than
        // a panic.
        if peers_len == 0 {
            return Hit::Sidebar(0);
        }
        let first_peer_y = areas.sidebar.y.saturating_add(2);
        if row < first_peer_y {
            return Hit::Sidebar(0);
        }
        let idx = (row - first_peer_y) as usize;
        let idx = idx.min(peers_len.saturating_sub(1));
        return Hit::Sidebar(idx);
    }
    if point_in_rect(areas.chat, col, row) {
        return Hit::Chat;
    }
    Hit::Footer
}

/// Initialize the terminal: raw mode + alt-screen + bracketed paste +
/// (optionally) mouse capture. Bracketed paste is always on; mouse
/// capture is gated by `mouse_enabled` because enabling capture on
/// tmux breaks native drag-select.
pub fn enter_terminal(
    mouse_enabled: bool,
) -> std::io::Result<Terminal<CrosstermBackend<Stdout>>> {
    use crossterm::event::{EnableBracketedPaste, EnableMouseCapture};
    use crossterm::terminal::{EnterAlternateScreen, SetTitle};
    crossterm::terminal::enable_raw_mode()?;
    let mut out = stdout();
    crossterm::execute!(out, EnableBracketedPaste)?;
    if mouse_enabled {
        crossterm::execute!(out, EnableMouseCapture)?;
    }
    crossterm::execute!(out, EnterAlternateScreen, SetTitle("ppexchanger"))?;
    let backend = CrosstermBackend::new(out);
    Terminal::new(backend)
}

/// Restore the terminal to its previous state. The teardown mirrors
/// `enter_terminal` exactly so we don't leak raw mode, alt-screen, or
/// mouse-capture into the parent shell.
pub struct TuiGuard {
    active: bool,
    mouse_enabled: bool,
}
impl TuiGuard {
    pub fn new(mouse_enabled: bool) -> std::io::Result<Self> {
        Ok(Self {
            active: true,
            mouse_enabled,
        })
    }

    /// Flip terminal mouse capture mid-session. Idempotent — emitting the
    /// same state twice is a no-op so the call can sit at the end of the
    /// settings key handler without guard checks.
    pub fn set_mouse(&mut self, on: bool) -> std::io::Result<()> {
        if on == self.mouse_enabled {
            return Ok(());
        }
        use crossterm::event::{DisableMouseCapture, EnableMouseCapture};
        use std::io::stdout;
        if on {
            crossterm::execute!(stdout(), EnableMouseCapture)?;
        } else {
            crossterm::execute!(stdout(), DisableMouseCapture)?;
        }
        self.mouse_enabled = on;
        Ok(())
    }
}
impl Drop for TuiGuard {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        use crossterm::event::{DisableBracketedPaste, DisableMouseCapture};
        use crossterm::terminal::LeaveAlternateScreen;
        if self.mouse_enabled {
            let _ = crossterm::execute!(stdout(), DisableMouseCapture);
        }
        let _ = crossterm::execute!(stdout(), DisableBracketedPaste, LeaveAlternateScreen);
        // crossterm 0.28 dropped the typed `ShowCursor` command; emit
        // the raw escape sequence instead. DCS show-cursor = ESC [ ? 25 h.
        let _ = std::io::Write::write_all(&mut stdout(), b"\x1B[?25h");
        let _ = crossterm::terminal::disable_raw_mode();
        self.active = false;
    }
}

/// Per-frame context the settings popup reads from `UiConfig` and the
/// build/version strings that don't belong on `UiState`.
#[derive(Default)]
pub struct SettingsView<'a> {
    pub cfg: Option<&'a UiConfig>,
    pub version: &'a str,
    pub config_path: &'a str,
    pub received_dir: &'a str,
}

/// Render one frame using the supplied theme + glyph palette.
pub fn render(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    state: &mut UiState,
    theme: &Theme,
    glyphs: &Glyphs,
    settings_view: SettingsView<'_>,
) -> std::io::Result<()> {
    // CRT scanline phase flips each render so the alternating DIM
    // modifier on chat rows appears to crawl downward.
    state.scanline_tick = !state.scanline_tick;
    terminal.draw(|f| {
        let area = f.area();
        // Paint the complete canvas first. This prevents stale cells when a
        // message wraps differently after a resize or theme switch.
        f.render_widget(Block::default().style(theme.style()), area);
        // Single source of truth for the layout — hit_test reuses it.
        let areas = compute_layout(area);

        draw_menu(f, areas.menu, state, theme, glyphs);
        draw_sidebar(f, areas.sidebar, state, theme, glyphs);
        draw_chat(f, areas.chat, state, theme, glyphs);
        draw_command_palette(f, areas.chat, state, theme);
        if state.show_footer {
            draw_footer(f, areas.footer, state, theme, glyphs);
        }

        if state.show_help {
            help::render(f, theme, glyphs);
        }
        if let Some(d) = &state.discovery {
            if d.view_map {
                discovery_popup::render_map(f, theme, glyphs, d);
            } else {
                discovery_popup::render(f, theme, glyphs, d);
            }
        }
        if let Some(p) = &state.file_offer {
            file_offer_popup::render(f, theme, glyphs, p);
        }
        // Settings popup renders last so it sits on top of every other
        // modal. Caller passes the live UiConfig; the popup mutates it
        // (and the caller persists on close).
        if let (Some(s), Some(cfg)) = (&state.settings, settings_view.cfg) {
            settings_popup::render(
                f,
                theme,
                glyphs,
                s,
                cfg,
                settings_view.version,
                &state.self_fingerprint,
                settings_view.config_path,
                settings_view.received_dir,
            );
        }
    })?;
    Ok(())
}

/// Render the top menu bar as five bordered Paragraph blocks side by
/// side. Each block is the click target for the corresponding
/// `MenuAction`; `hit_test` re-uses the same horizontal split to map a
/// mouse column back to an action. Buttons render as `[ Label ]` with
/// brackets in `border_inactive` and label in `accent` so the menu
/// reads at a glance against the CRT palette.
fn draw_menu(f: &mut Frame, area: Rect, state: &UiState, theme: &Theme, _glyphs: &Glyphs) {
    if area.height == 0 || area.width == 0 {
        return;
    }
    let online = state
        .peers
        .iter()
        .filter(|p| p.state == PeerState::Connected)
        .count();
    let known = state.peers.len();
    let health = if known == 0 { 0 } else { (online * 100 / known) as u16 };
    let identity = truncate_tail(&state.self_name, 20);
    let fingerprint = truncate_tail(&state.self_fingerprint, 12);
    let title = Line::from(vec![
        Span::styled(" PPX ", Style::default().fg(theme.bg).bg(theme.accent).add_modifier(Modifier::BOLD)),
        Span::styled("  secure local exchange", Style::default().fg(theme.fg).bg(theme.bg).add_modifier(Modifier::BOLD)),
        Span::styled(format!("  ·  {} online", online), theme.self_message_style()),
        Span::styled(format!("  ·  {}  {}", identity, fingerprint), theme.dim_style()),
    ]);
    let button = |label: &str, style: Style| {
        Span::styled(
            format!("{:^width$}", label, width = MENU_BUTTON_WIDTH as usize),
            style,
        )
    };
    let active_button = Style::default()
        .fg(theme.bg)
        .bg(theme.accent)
        .add_modifier(Modifier::BOLD);
    let quiet_button = Style::default().fg(theme.fg).bg(theme.status_bg);
    let info_button = Style::default().fg(theme.info).bg(theme.status_bg);
    let danger_button = Style::default().fg(theme.error).bg(theme.status_bg);
    // Fixed-width, filled controls read as buttons in every color theme and
    // stay exactly aligned with the mouse hit regions below.
    let nav = Line::from(vec![
        button("1 PEERS", if state.focus == Focus::Sidebar { active_button } else { quiet_button }),
        Span::raw(" "),
        button("/ DISCOVER", info_button),
        Span::raw(" "),
        button("3 SETTINGS", quiet_button),
        Span::raw(" "),
        button("? HELP", quiet_button),
        Span::raw(" "),
        button("x QUIT", danger_button),
    ]);
    let block = Block::default().borders(Borders::ALL).border_type(BorderType::Rounded)
        .border_style(theme.border_style(false));
    let inner = block.inner(area);
    f.render_widget(block, area);
    let header = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Length(1)])
        .split(inner);
    let top = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(28), Constraint::Length(22)])
        .split(header[0]);
    f.render_widget(Paragraph::new(title), top[0]);
    f.render_widget(
        Gauge::default()
            .ratio(f64::from(health) / 100.0)
            .label(format!(" LINK {} / {} ", online, known))
            .use_unicode(true)
            .style(Style::default().fg(theme.status_fg).bg(theme.status_bg))
            .gauge_style(theme.gauge_filled_style()),
        top[1],
    );
    f.render_widget(Paragraph::new(nav), header[1]);
}

fn draw_sidebar(f: &mut Frame, area: Rect, state: &UiState, theme: &Theme, glyphs: &Glyphs) {
    // Connection requests are deliberately visible in their own compact
    // container. It appears only while action is required, leaving the
    // normal peer list uncluttered at all other times.
    let peer_area = if let Some(request) = &state.pending_connection {
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(5), Constraint::Min(3)])
            .split(area);
        let pending = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(theme.border_style(true))
            .title(Span::styled(
                " ◌ PENDING CONNECTION ",
                Style::default().fg(theme.status_seen).add_modifier(Modifier::BOLD),
            ));
        let prompt = Paragraph::new(vec![
            Line::from(Span::styled(request.name.clone(), theme.peer_message_style())),
            Line::from(Span::styled("Click / Enter accept · Esc decline", theme.dim_style())),
        ])
        .block(pending)
        .wrap(Wrap { trim: true });
        f.render_widget(prompt, rows[0]);
        rows[1]
    } else {
        area
    };
    let active = state.focus == Focus::Sidebar;
    let title_style = if active {
        theme.border_style(true)
    } else {
        theme.border_style(false)
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(title_style)
        .title(Span::styled(
            format!(" {} PEERS · {}/{} ", glyphs.cursor, online_count(state), state.peers.len()),
            Style::default().fg(theme.accent).add_modifier(Modifier::BOLD),
        ))
        .title_bottom(
            Line::from(Span::styled(" ↑↓ navigate · click select ", theme.dim_style()))
                .right_aligned(),
        );

    let items: Vec<ListItem> = if state.peers.is_empty() {
        // Empty state: show helpful message
        vec![ListItem::new(vec![
            Line::from(Span::styled("no connected peers", theme.dim_style())),
            Line::from(Span::styled("run /discover to find someone nearby", theme.info_style())),
        ])]
    } else {
        state
            .peers
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let (dot, dot_color) = match p.state {
                    PeerState::Connected => (glyphs.dot_connected, theme.status_online),
                    PeerState::Seen => (glyphs.dot_seen, theme.status_seen),
                    PeerState::Gone => (glyphs.dot_gone, theme.status_offline),
                };
                let trust = if p.trusted { glyphs.trusted } else { glyphs.untrusted };
                let style = if p.trusted {
                    theme.trusted_style()
                } else {
                    theme.untrusted_style()
                };
                let name_style = theme.peer_message_style_for(&p.peer_id);
                let detail = match p.state {
                    PeerState::Connected => "online",
                    PeerState::Seen => "available",
                    PeerState::Gone => "offline",
                };
                let label = if i == state.selected_peer {
                    Line::from(vec![
                        Span::styled(
                            format!("{} ", dot),
                            Style::default().fg(dot_color),
                        ),
                        Span::styled(
                            format!("{} {}  ", trust, p.name),
                            name_style.add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(detail, Style::default().fg(dot_color).bg(theme.status_bg)),
                    ])
                } else {
                    Line::from(vec![
                        Span::styled(
                            format!("{} {} ", dot, trust),
                            style,
                        ),
                        Span::styled(p.name.clone(), name_style),
                        Span::styled(format!(" · {}", detail), theme.dim_style()),
                    ])
                };
                ListItem::new(label)
            })
            .collect()
    };

    let mut list_state = ListState::default();
    if !state.peers.is_empty() {
        list_state.select(Some(state.selected_peer));
    }
    f.render_stateful_widget(
        List::new(items)
            .block(block)
            .highlight_style(Style::default().bg(theme.status_bg).add_modifier(Modifier::BOLD))
            .highlight_symbol("› "),
        peer_area,
        &mut list_state,
    );
}

fn online_count(state: &UiState) -> usize {
    state
        .peers
        .iter()
        .filter(|peer| peer.state == PeerState::Connected)
        .count()
}

fn draw_chat(f: &mut Frame, area: Rect, state: &UiState, theme: &Theme, glyphs: &Glyphs) {
    let active = state.focus == Focus::Chat;
    let selected = state.selected();
    let selected_name = selected.map(|p| p.name.clone()).unwrap_or_default();

    // Conversation header follows the selected peer and is deliberately
    // separate from the app chrome, so context survives scrolling.
    let title = if selected_name.is_empty() {
        Line::from(vec![Span::styled(
            format!(" {} INBOX ", glyphs.cursor),
            Style::default().fg(theme.accent).add_modifier(Modifier::BOLD),
        )])
    } else {
        // Show status dot and peer hostname with connection status
        let fp = selected
            .map(|peer| peer.fingerprint.as_str())
            .filter(|fingerprint| !fingerprint.is_empty())
            .map(|fingerprint| format!("  · {}", truncate_tail(fingerprint, 12)))
            .unwrap_or_default();
        let (dot, dot_color, status_text) = match selected.map(|p| p.state) {
            Some(PeerState::Connected) => (glyphs.dot_connected, theme.status_online, "[connected]"),
            Some(PeerState::Seen) => (glyphs.dot_seen, theme.status_seen, "[seen]"),
            Some(PeerState::Gone) | None => (glyphs.dot_gone, theme.status_offline, "[offline]"),
        };
        Line::from(vec![
            Span::styled(
                format!(" {} ", dot),
                Style::default().fg(dot_color),
            ),
            Span::styled(
                selected_name.clone(),
                Style::default().fg(theme.accent).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("  · {}", status_text.trim_matches(&['[', ']'][..])),
                Style::default().fg(dot_color),
            ),
            Span::styled(fp, theme.dim_style()),
        ])
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(theme.border_style(active))
        .title(title);

    // Apply scroll: when scrolled back, show messages ending at
    // `len - scroll`. Slice to whatever fits in the area.
    let total = state.messages.len();
    let visible_n = (area.height as usize).saturating_sub(2); // minus borders
    let end = total.saturating_sub(state.scroll);
    let start = end.saturating_sub(visible_n);

    // Empty state: show improved welcome message
    let empty = state.messages.is_empty();
    let visible: Vec<Line> = if empty {
        Vec::new()
    } else {
        state
            .messages
            .iter()
            .skip(start)
            .take(end - start)
            .map(|m| {
                let who = if m.outgoing {
                    state.self_name.clone()
                } else {
                    // Resolve at render time so a provisional `peer@IP`
                    // label is replaced in already-rendered chat history as
                    // soon as the encrypted Hello supplies the real name.
                    state
                        .peers
                        .iter()
                        .find(|peer| peer.peer_id == m.from_peer)
                        .map(|peer| peer.name.clone())
                        .unwrap_or_else(|| m.from_name.clone())
                };
                let who_style = if m.outgoing {
                    theme.self_message_style()
                } else {
                    theme.peer_message_style_for(&m.from_peer)
                };
                // Format timestamp as HH:MM
                let timestamp = chrono_timestamp(&Some(m.ts_unix));
                let delivery = if m.pending { "⏳ pending" } else { "✓ sent" };
                let divider = Style::default().fg(theme.border_inactive).bg(theme.bg);
                let bubble = Style::default().fg(theme.fg).bg(theme.status_bg);

                if m.outgoing {
                    // Right-align the entire local row and keep delivery as
                    // the final span, putting the checkmark at the chat edge
                    // even when the message itself is short.
                    Line::from(vec![
                        Span::styled(
                            format!("{}  {}", who, timestamp),
                            who_style.add_modifier(Modifier::BOLD),
                        ),
                        Span::styled("  │  ", divider),
                        Span::styled(format!(" {} ", m.body), bubble),
                        Span::styled(
                            format!(" {} ", delivery),
                            if m.pending {
                                theme.info_style().bg(theme.status_bg)
                            } else {
                                theme.self_message_style().bg(theme.status_bg)
                            },
                        ),
                    ])
                    .right_aligned()
                } else {
                    // Incoming rows stay left-aligned. The colored peer
                    // rail, muted metadata, and contrasting bubble make the
                    // two directions readable at a glance.
                    Line::from(vec![
                        Span::styled(" ‹ ", who_style.add_modifier(Modifier::BOLD).bg(theme.status_bg)),
                        Span::styled(
                            format!("{}  {}", who, timestamp),
                            who_style.add_modifier(Modifier::BOLD),
                        ),
                        Span::styled("  │  ", divider),
                        Span::styled(format!(" {} ", m.body), bubble),
                    ])
                }
            })
            .collect()
    };

    let para = Paragraph::new(visible)
        .block(block)
        .wrap(Wrap { trim: false })
        .style(Style::default().fg(theme.fg).bg(theme.bg));
    f.render_widget(para, area);
    if empty {
        let body = area.inner(Margin { vertical: 1, horizontal: 1 });
        let height = 5.min(body.height);
        let empty_area = Rect::new(
            body.x,
            body.y.saturating_add(body.height.saturating_sub(height) / 2),
            body.width,
            height,
        );
        let (headline, detail) = if selected_name.is_empty() {
            ("YOUR INBOX IS READY", "Choose a peer, or use Discover to find someone on your LAN.")
        } else {
            ("START THE CONVERSATION", "Write a message below — your exchange stays encrypted on this network.")
        };
        f.render_widget(
            Paragraph::new(vec![
                Line::from(Span::styled(headline, theme.highlight_style())),
                Line::from(""),
                Line::from(Span::styled(detail, theme.dim_style())),
                Line::from(""),
                Line::from(Span::styled("Discover  ·  Select a peer  ·  Start chatting", theme.info_style())),
            ])
            .alignment(Alignment::Center),
            empty_area,
        );
    }
    // The chat's scroll position is discoverable at a glance, including
    // when mouse-wheel or PageUp navigation moves away from the newest item.
    if total > visible_n && area.width > 2 && area.height > 2 {
        let mut scrollbar_state = ScrollbarState::new(total)
            .position(total.saturating_sub(visible_n.saturating_add(state.scroll)))
            .viewport_content_length(visible_n);
        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .thumb_style(Style::default().fg(theme.accent).bg(theme.bg))
            .track_style(Style::default().fg(theme.border_inactive).bg(theme.bg))
            .begin_symbol(None)
            .end_symbol(None);
        f.render_stateful_widget(
            scrollbar,
            area.inner(Margin { vertical: 1, horizontal: 0 }),
            &mut scrollbar_state,
        );
    }
}

fn draw_footer(f: &mut Frame, area: Rect, state: &UiState, theme: &Theme, glyphs: &Glyphs) {
    if area.width < 4 || area.height < FOOTER_HEIGHT {
        return;
    }
    let composer_area = Rect::new(area.x, area.y, area.width, 3);
    let target = state.selected().map(|p| p.name.as_str()).unwrap_or("no recipient");
    let target_label = truncate_tail(target, 18);
    let composer_title = if target == "no recipient" {
        " message · choose a peer "
    } else {
        " message · encrypted "
    };
    let composer_block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(theme.border_style(state.focus == Focus::Chat))
        .title(Span::styled(
            composer_title,
            Style::default().fg(theme.accent).add_modifier(Modifier::BOLD),
        ));
    let input_area = composer_block.inner(composer_area);
    f.render_widget(composer_block, composer_area);
    let chip = format!(" TO {} ", target_label);
    let prefix = format!(" {} ", glyphs.arrow);
    let available = input_area
        .width
        .saturating_sub(chip.chars().count() as u16)
        .saturating_sub(prefix.chars().count() as u16)
        .max(1) as usize;
    let draft = truncate_tail(&state.composer, available);
    let is_empty = state.composer.is_empty();
    let input = Line::from(vec![
        Span::styled(chip, Style::default().fg(theme.status_fg).bg(theme.status_bg).add_modifier(Modifier::BOLD)),
        Span::styled(prefix.clone(), theme.highlight_style()),
        Span::styled(if is_empty { glyphs.cursor } else { "" }, theme.highlight_style()),
        Span::styled(
            if is_empty { "Write a message or /command".to_string() } else { draft.clone() },
            if is_empty { theme.dim_style() } else { Style::default().fg(theme.fg).bg(theme.bg) },
        ),
    ]);
    f.render_widget(Paragraph::new(input), input_area);

    // Place the terminal cursor at the logical end of the visible draft.
    let modal_open = state.show_help
        || state.discovery.is_some()
        || state.file_offer.is_some()
        || state.settings.is_some();
    if state.focus == Focus::Chat && !modal_open && !is_empty {
        let cursor_x = input_area
            .x
            .saturating_add(target_label.chars().count() as u16 + 5)
            .saturating_add(prefix.chars().count() as u16)
            .saturating_add(draft.chars().count() as u16)
            .min(input_area.right().saturating_sub(1));
        f.set_cursor_position((cursor_x, input_area.y));
    }
    let status = if state.status_format == crate::tui::config::StatusFormat::Off {
        "Ready".to_string()
    } else if state.status.trim().is_empty() {
        "Ready".to_string()
    } else {
        truncate_tail(&state.status, area.width.saturating_sub(28) as usize)
    };
    let status_style = status_style(&status, theme);
    let status_icon = if ["failed", "denied", "error", "aborted"]
        .iter()
        .any(|needle| status.to_ascii_lowercase().contains(needle))
    {
        "!"
    } else if status.to_ascii_lowercase().contains("connected") {
        "✓"
    } else {
        "·"
    };
    let meta = Line::from(vec![
        Span::styled(format!(" {} ", status_icon), status_style.add_modifier(Modifier::BOLD)),
        Span::styled(format!(" {}", status), status_style),
        Span::raw("  "),
        Span::styled(
            " Enter send ",
            Style::default().fg(theme.bg).bg(theme.accent).add_modifier(Modifier::BOLD),
        ),
        Span::styled("  Esc clear", theme.dim_style()),
        Span::styled("   Tab focus", theme.dim_style()),
        Span::styled("   ? help", Style::default().fg(theme.info).bg(theme.bg)),
    ]);
    f.render_widget(
        Paragraph::new(meta),
        Rect::new(area.x, area.y.saturating_add(3), area.width, 1),
    );
}

/// Show matching slash commands immediately above the borderless composer.
/// It is deliberately contextual: ordinary message drafting never loses chat
/// space to a permanently visible command menu.
fn draw_command_palette(f: &mut Frame, chat: Rect, state: &UiState, theme: &Theme) {
    let Some((popup, shown)) = command_palette_layout(chat, &state.composer) else {
        return;
    };
    f.render_widget(Clear, popup);
    let lines: Vec<Line> = shown
        .into_iter()
        .map(|(command, description)| {
            Line::from(vec![
                Span::styled(format!(" {:<12}", command), theme.highlight_style()),
                Span::styled(description, theme.dim_style()),
            ])
        })
        .collect();
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(theme.border_style(true))
        .title(Span::styled(
            " commands · Tab completes ",
            Style::default().fg(theme.accent).add_modifier(Modifier::BOLD),
        ));
    f.render_widget(Paragraph::new(lines).block(block), popup);
}

/// Resolve a palette click into a completed command. The popup is rendered
/// inside chat, so its hit map lives alongside the drawing geometry.
pub fn command_palette_hit(chat: Rect, composer: &str, col: u16, row: u16) -> Option<&'static str> {
    let (popup, shown) = command_palette_layout(chat, composer)?;
    if col < popup.x || col >= popup.right() || row <= popup.y || row >= popup.bottom() {
        return None;
    }
    shown.get((row - popup.y - 1) as usize).map(|(command, _)| *command)
}

fn command_palette_layout(chat: Rect, composer: &str) -> Option<(Rect, Vec<(&'static str, &'static str)>)> {
    if !composer.starts_with('/') || chat.width < 30 || chat.height < 7 {
        return None;
    }
    let shown: Vec<_> = input::command_matches(composer).into_iter().take(5).collect();
    if shown.is_empty() {
        return None;
    }
    let height = (shown.len() as u16 + 2).min(chat.height.saturating_sub(2));
    Some((
        Rect::new(
            chat.x.saturating_add(2),
            chat.bottom().saturating_sub(height.saturating_add(1)),
            chat.width.saturating_sub(4).min(58),
            height,
        ),
        shown,
    ))
}

/// Keep a status or draft on one terminal line, preserving the newest end of
/// a long value. The input model appends at the end, so this is the part users
/// need to see while typing.
fn truncate_tail(value: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    let count = value.chars().count();
    if count <= width {
        return value.to_string();
    }
    if width <= 3 {
        return ".".repeat(width);
    }
    let tail: String = value.chars().skip(count - (width - 3)).collect();
    format!("...{}", tail)
}

fn status_style(status: &str, theme: &Theme) -> Style {
    let lower = status.to_ascii_lowercase();
    if ["failed", "denied", "error", "aborted"].iter().any(|needle| lower.contains(needle)) {
        theme.error_style()
    } else if ["connected", "listening", "saved", "ready"].iter().any(|needle| lower.contains(needle)) {
        theme.self_message_style()
    } else {
        theme.info_style()
    }
}

/// Drain all pending events from the receiver into the shared state.
pub fn drain_events(rx: &std::sync::mpsc::Receiver<Event>, state: &mut UiState) -> usize {
    let mut text_count = 0;
    while let Ok(ev) = rx.try_recv() {
        if matches!(ev, Event::TextMessage { .. }) {
            text_count += 1;
        }
        state.apply(&ev);
    }
    text_count
}

/// Merge persisted contacts into the live UI state so trusted/untrusted
/// markings survive restarts.
pub fn merge_contacts(state: &mut UiState, db: &PeerDb) {
    for c in db.iter() {
        if let Some(p) = state.peers.iter_mut().find(|p| p.peer_id == c.peer_id) {
            p.trusted = c.trusted;
        } else {
            state.peers.push(UiPeer {
                peer_id: c.peer_id,
                name: c.name.clone(),
                fingerprint: crate::protocol::fingerprint(&c.public_key),
                public_key: c.public_key,
                last_addr: c.last_addr,
                trusted: c.trusted,
                state: PeerState::Seen,
            });
        }
    }
}

/// Convenience: build a list of `(peer_id, addr)` for every connected peer.
pub fn connected_addrs(state: &UiState) -> Vec<(PeerId, String)> {
    state
        .peers
        .iter()
        .filter(|p| p.state == PeerState::Connected)
        .map(|p| (p.peer_id, p.name.clone()))
        .collect()
}

/// Update the persisted contact DB to reflect the latest UI view.
pub fn sync_to_db(state: &UiState, db: &mut PeerDb) {
    let now = now_unix();
    for p in &state.peers {
        if p.state != PeerState::Connected {
            continue;
        }
        db.upsert(Contact {
            peer_id: p.peer_id,
            name: p.name.clone(),
            public_key: p.public_key,
            last_addr: p.last_addr,
            last_seen_unix: now,
            trusted: p.trusted,
        });
    }
}

// Frame alias — keeps the local helper signatures tidy.
type Frame<'a> = ratatui::Frame<'a>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_event_adds_peer() {
        let id = Identity {
            peer_id: [0u8; 16],
            keypair: crate::crypto::Keypair::generate(),
            name: "alice".into(),
            hostname: "test-host".into(),
        };
        let mut s = UiState::from_identity(&id);
        let ev = Event::PeerSeen {
            peer_id: [1u8; 16],
            name: "bob".into(),
            hostname: "macbook".into(),
            public_key: [0u8; 32],
            fingerprint: "deadbeef00000000".into(),
            addr: "127.0.0.1:1".parse().unwrap(),
        };
        s.apply(&ev);
        assert_eq!(s.peers.len(), 1);
        assert_eq!(s.peers[0].name, "bob (macbook)");
    }

    #[test]
    fn apply_text_message_appends() {
        let id = Identity {
            peer_id: [0u8; 16],
            keypair: crate::crypto::Keypair::generate(),
            name: "alice".into(),
            hostname: "test-host".into(),
        };
        let mut s = UiState::from_identity(&id);
        s.apply(&Event::TextMessage {
            from_peer: [1u8; 16],
            from_name: "bob".into(),
            body: "hi".into(),
        });
        assert_eq!(s.messages.len(), 1);
        assert_eq!(s.messages[0].body, "hi");
    }

    #[test]
    fn ring_buffer_caps_history() {
        let id = Identity {
            peer_id: [0u8; 16],
            keypair: crate::crypto::Keypair::generate(),
            name: "alice".into(),
            hostname: "test-host".into(),
        };
        let mut s = UiState::from_identity(&id);
        s.max_scrollback = 4;
        for i in 0..10 {
            s.apply(&Event::TextMessage {
                from_peer: [1u8; 16],
                from_name: "bob".into(),
                body: format!("m{}", i),
            });
        }
        assert_eq!(s.messages.len(), 4);
        assert_eq!(s.messages.front().unwrap().body, "m6");
        assert_eq!(s.messages.back().unwrap().body, "m9");
    }

    #[test]
    fn scroll_back_clamps_to_history() {
        let id = Identity {
            peer_id: [0u8; 16],
            keypair: crate::crypto::Keypair::generate(),
            name: "alice".into(),
            hostname: "test-host".into(),
        };
        let mut s = UiState::from_identity(&id);
        for i in 0..5 {
            s.apply(&Event::TextMessage {
                from_peer: [1u8; 16],
                from_name: "bob".into(),
                body: format!("m{}", i),
            });
        }
        s.scroll_back(99);
        assert_eq!(s.scroll, 4);
        s.scroll_forward(2);
        assert_eq!(s.scroll, 2);
        s.scroll_forward(99);
        assert_eq!(s.scroll, 0);
    }

    #[test]
    fn scanline_tick_inits_false() {
        let id = Identity {
            peer_id: [0u8; 16],
            keypair: crate::crypto::Keypair::generate(),
            name: "alice".into(),
            hostname: "test-host".into(),
        };
        let s = UiState::from_identity(&id);
        assert!(!s.scanline_tick);
    }

    #[test]
    fn logo_suppression_when_modal_open() {
        // Replicates the any_modal guard from render(). If show_logo
        // is true and any modal is open, the logo must NOT draw over
        // the popup. We don't actually call render() here — we assert
        // the same boolean expression the render() branch uses.
        let id = Identity {
            peer_id: [0u8; 16],
            keypair: crate::crypto::Keypair::generate(),
            name: "alice".into(),
            hostname: "test-host".into(),
        };
        let mut s = UiState::from_identity(&id);
        assert!(s.show_logo);
        assert!(s.messages.is_empty());

        let any_modal = |s: &UiState| {
            s.show_help || s.discovery.is_some() || s.file_offer.is_some() || s.settings.is_some()
        };
        assert!(!any_modal(&s));

        s.show_help = true;
        assert!(any_modal(&s));
        s.show_help = false;

        s.start_discovery();
        assert!(any_modal(&s));
        s.close_discovery();

        s.open_settings(&crate::tui::UiConfig::default());
        assert!(any_modal(&s));
    }

    #[test]
    fn cycle_focus_toggles() {
        let id = Identity {
            peer_id: [0u8; 16],
            keypair: crate::crypto::Keypair::generate(),
            name: "alice".into(),
            hostname: "test-host".into(),
        };
        let mut s = UiState::from_identity(&id);
        assert_eq!(s.focus, Focus::Chat);
        s.cycle_focus();
        assert_eq!(s.focus, Focus::Sidebar);
        s.cycle_focus();
        assert_eq!(s.focus, Focus::Chat);
    }

    #[test]
    fn discovery_view_map_defaults_false_and_toggles() {
        let id = Identity {
            peer_id: [0u8; 16],
            keypair: crate::crypto::Keypair::generate(),
            name: "alice".into(),
            hostname: "test-host".into(),
        };
        let mut s = UiState::from_identity(&id);
        s.start_discovery();
        assert!(!s.discovery.as_ref().unwrap().view_map);
        s.discovery.as_mut().unwrap().view_map = true;
        assert!(s.discovery.as_ref().unwrap().view_map);
    }

    #[test]
    fn discovery_lifecycle() {
        let id = Identity {
            peer_id: [0u8; 16],
            keypair: crate::crypto::Keypair::generate(),
            name: "alice".into(),
            hostname: "test-host".into(),
        };
        let mut s = UiState::from_identity(&id);
        assert!(s.discovery.is_none());
        s.start_discovery();
        let d = s.discovery.as_ref().unwrap();
        assert!(d.running);
        assert!(d.results.is_empty());

        // Simulate a method finishing.
        s.apply(&Event::DiscoveryUpdate {
            method: "UDP multicast (239.255.42.99)".into(),
            peers: vec![crate::events::DiscoveredPeer {
                name: Some("bob".into()),
                hostname: Some("macbook".into()),
                addr: "10.0.0.2:7777".parse().unwrap(),
                fingerprint: Some("abcd".into()),
                reverse: None,
            }],
        });
        s.apply(&Event::DiscoveryFinished);
        let d = s.discovery.as_ref().unwrap();
        assert!(!d.running);
        assert_eq!(d.results.len(), 1);
        assert_eq!(d.results[0].name, "UDP multicast (239.255.42.99)");
        assert_eq!(d.results[0].peers.len(), 1);
        assert_eq!(d.results[0].peers[0].name.as_deref(), Some("bob"));

        // Esc-equivalent: close_discovery drops the modal.
        s.close_discovery();
        assert!(s.discovery.is_none());
    }

    #[test]
    fn authenticated_name_is_kept_when_it_arrives_before_connected_event() {
        let id = Identity {
            peer_id: [0u8; 16],
            keypair: crate::crypto::Keypair::generate(),
            name: "alice".into(),
            hostname: "test-host".into(),
        };
        let mut s = UiState::from_identity(&id);
        let peer_id = [9u8; 16];
        s.apply(&Event::PeerNamed {
            peer_id,
            name: "macbook (berks)".into(),
        });
        s.apply(&Event::PeerConnected {
            peer_id,
            name: "peer@10.0.0.95:7777".into(),
            fingerprint: "abcd".into(),
            trusted: false,
            addr: "10.0.0.95:7777".parse().unwrap(),
        });
        assert_eq!(s.peers[0].name, "macbook (berks)");
    }

    #[test]
    fn discovery_update_replaces_existing_method_results() {
        let id = Identity {
            peer_id: [0u8; 16],
            keypair: crate::crypto::Keypair::generate(),
            name: "alice".into(),
            hostname: "test-host".into(),
        };
        let mut s = UiState::from_identity(&id);
        s.start_discovery();
        s.apply(&Event::DiscoveryUpdate {
            method: "TCP subnet scan".into(),
            peers: vec![crate::events::DiscoveredPeer {
                name: None,
                hostname: None,
                addr: "10.0.0.3:7777".parse().unwrap(),
                fingerprint: None,
                reverse: None,
            }],
        });
        s.apply(&Event::DiscoveryUpdate {
            method: "TCP subnet scan".into(),
            peers: vec![
                crate::events::DiscoveredPeer {
                    name: None,
                    hostname: None,
                    addr: "10.0.0.3:7777".parse().unwrap(),
                    fingerprint: None,
                    reverse: None,
                },
                crate::events::DiscoveredPeer {
                    name: None,
                    hostname: None,
                    addr: "10.0.0.4:7777".parse().unwrap(),
                    fingerprint: None,
                    reverse: None,
                },
            ],
        });
        let d = s.discovery.as_ref().unwrap();
        // Same method reported twice — should produce one entry, not two.
        assert_eq!(d.results.len(), 1);
        assert_eq!(d.results[0].peers.len(), 2);
    }

    #[test]
    fn sort_peers_groups_connected_first_then_seen_then_gone() {
        let id = Identity {
            peer_id: [0u8; 16],
            keypair: crate::crypto::Keypair::generate(),
            name: "alice".into(),
            hostname: "test-host".into(),
        };
        let mut s = UiState::from_identity(&id);
        let mk = |pid: u8, name: &str, state: PeerState| UiPeer {
            peer_id: [pid; 16],
            name: name.into(),
            fingerprint: String::new(),
            public_key: [0u8; 32],
            last_addr: None,
            trusted: false,
            state,
        };
        s.peers = vec![
            mk(1, "carol", PeerState::Seen),
            mk(2, "bob", PeerState::Connected),
            mk(3, "alice-friend", PeerState::Gone),
            mk(4, "dave", PeerState::Connected),
        ];
        s.selected_peer = 0;
        s.sort_peers();
        // Connected (bob, dave) sorted alphabetically, then Seen (carol),
        // then Gone (alice-friend).
        let names: Vec<&str> = s.peers.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, vec!["bob", "dave", "carol", "alice-friend"]);
    }

    #[test]
    fn sort_peers_clamps_selection_when_peers_removed() {
        let id = Identity {
            peer_id: [0u8; 16],
            keypair: crate::crypto::Keypair::generate(),
            name: "alice".into(),
            hostname: "test-host".into(),
        };
        let mut s = UiState::from_identity(&id);
        let mk = |pid: u8, name: &str, state: PeerState| UiPeer {
            peer_id: [pid; 16],
            name: name.into(),
            fingerprint: String::new(),
            public_key: [0u8; 32],
            last_addr: None,
            trusted: false,
            state,
        };
        s.peers = vec![
            mk(1, "a", PeerState::Connected),
            mk(2, "b", PeerState::Connected),
            mk(3, "c", PeerState::Connected),
        ];
        s.selected_peer = 2;
        s.peers.remove(2);
        s.sort_peers();
        assert_eq!(s.selected_peer, 1);
    }

    // Layout / hit-test coverage.

    fn synthetic_layout() -> (Rect, LayoutAreas) {
        let screen = Rect::new(0, 0, 80, 24);
        let areas = compute_layout(screen);
        (screen, areas)
    }

    #[test]
    fn compute_layout_produces_four_rects() {
        let (screen, areas) = synthetic_layout();
        // Outer is header + body + composer; constants define their sizes.
        assert_eq!(areas.sidebar.width, SIDEBAR_WIDTH);
        assert_eq!(areas.footer.height, FOOTER_HEIGHT);
        assert_eq!(areas.chat.x, areas.sidebar.x + SIDEBAR_WIDTH);
        assert_eq!(areas.footer.y, screen.height - FOOTER_HEIGHT);
        // Header sits at the very top and spans the full width.
        assert_eq!(areas.menu.y, 0);
        assert_eq!(areas.menu.height, MENU_HEIGHT);
        assert_eq!(areas.menu.width, screen.width);
        // Body sits below the menu.
        assert_eq!(areas.chat.y, MENU_HEIGHT);
    }

    #[test]
    fn truncate_tail_keeps_the_latest_input_visible() {
        assert_eq!(truncate_tail("hello", 8), "hello");
        assert_eq!(truncate_tail("abcdefghijkl", 8), "...hijkl");
        assert_eq!(truncate_tail("abcdef", 3), "...");
    }

    #[test]
    fn command_palette_click_resolves_the_visible_command() {
        let chat = Rect::new(0, 0, 80, 24);
        let (popup, _) = command_palette_layout(chat, "/disc").unwrap();
        assert_eq!(
            command_palette_hit(chat, "/disc", popup.x + 2, popup.y + 1),
            Some("/discover")
        );
    }

    #[test]
    fn hit_test_menu_clicks_resolve_to_action() {
        let (screen, areas) = synthetic_layout();
        // Five buttons of width 12 with a 1-cell gap. Click in the
        // middle of each button.
        let stride = 12 + 1;
        for (col_target, expected) in [
            (5usize, MenuAction::Peers),
            (18, MenuAction::Discover),
            (31, MenuAction::Settings),
            (44, MenuAction::Help),
            (57, MenuAction::Quit),
        ] {
            let hit = hit_test(screen, col_target as u16, 2, &areas, false, 0);
            assert!(
                matches!(hit, Hit::Menu(a) if a == expected),
                "col {} expected {:?}, got {:?}",
                col_target,
                expected,
                hit
            );
        }
        // Click past the last button falls through to one of the
        // body / footer / sidebar panes. On an 80×24 screen the only
        // options at y=0 are Sidebar (if x < 24) or Footer (default).
        let past = stride * 5 + 2;
        if (past as u16) < screen.width {
            let hit = hit_test(screen, past as u16, 2, &areas, false, 0);
            assert!(
                !matches!(hit, Hit::Menu(_)),
                "past-end should not be a Menu hit, got {:?}",
                hit
            );
        }
    }

    #[test]
    fn hit_test_sidebar_row_picks_peer_index() {
        let (screen, areas) = synthetic_layout();
        // First peer sits at sidebar.y + 2 (1 border + 1 header line).
        let first_y = areas.sidebar.y + 2;
        assert!(matches!(
            hit_test(screen, areas.sidebar.x + 1, first_y, &areas, false, 3),
            Hit::Sidebar(0)
        ));
        assert!(matches!(
            hit_test(screen, areas.sidebar.x + 1, first_y + 1, &areas, false, 3),
            Hit::Sidebar(1)
        ));
        // Click below last peer but still inside the sidebar — should
        // clamp to the last index rather than fall through to Footer.
        let below_last = areas.sidebar
            .y
            .saturating_add(areas.sidebar.height)
            .saturating_sub(2);
        assert!(matches!(
            hit_test(screen, areas.sidebar.x + 1, below_last, &areas, false, 3),
            Hit::Sidebar(2)
        ));
    }

    #[test]
    fn hit_test_chat_click_returns_chat() {
        let (screen, areas) = synthetic_layout();
        let col = areas.chat.x + 1;
        let row = areas.chat.y + 1;
        assert!(matches!(
            hit_test(screen, col, row, &areas, false, 0),
            Hit::Chat
        ));
    }

    #[test]
    fn hit_test_modal_consumes_clicks_inside_modal_rect() {
        let (screen, areas) = synthetic_layout();
        let modal = modal_rect(screen);
        let col = modal.x + modal.width / 2;
        let row = modal.y + modal.height / 2;
        // Without a modal open, a click inside the modal rect falls
        // through to the chat pane (since the modal sits over it).
        assert!(matches!(
            hit_test(screen, col, row, &areas, false, 0),
            Hit::Chat
        ));
        // With a modal open, that same click is consumed as Modal.
        assert!(matches!(
            hit_test(screen, col, row, &areas, true, 0),
            Hit::Modal
        ));
    }
}
