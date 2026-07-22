//! ratatui-driven terminal UI.
//!
//! Layout:
//!   ╭─ lanchat ─ alice ────────────────────────────────────────────╮
//!   │ Peers (n)       │ alice: hi                                    │
//!   │  bob  trusted   │ bob:  yo                                     │
//!   │  carol pending  │ alice: how r u?                              │
//!   ├─────────────────┴──────────────────────────────────────────────┤
//!   │ > typing...                                                    │
//!   ╰────────────────────────────────────────────────────────────────╯
//!
//! The theme + glyph palettes live in `theme.rs`. The hand-rolled TOML
//! config reader lives in `config.rs`. The keyboard overlay lives in
//! `help.rs`. This module owns the shared `UiState` and the main `render`
//! pass.

pub mod config;
pub mod discovery_popup;
pub mod help;
pub mod input;
pub mod theme;

pub use config::{UiConfig, DEFAULT_SCROLLBACK, MAX_SCROLLBACK};
pub use input::{EditorEvent, LineEditor};
pub use theme::{detect_glyphs, Glyphs, Theme, ThemeName};

use crate::events::{Event, PeerId};
use crate::identity::Identity;
use crate::peerdb::{Contact, PeerDb};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, BorderType, List, ListItem, Paragraph, Wrap};
use ratatui::Terminal;
use std::collections::VecDeque;
use std::io::{stdout, Stdout};
use std::time::{SystemTime, UNIX_EPOCH};

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
    /// Bounded message ring; older entries are dropped when full.
    pub messages: VecDeque<UiMessage>,
    pub status: String,
    pub selected_peer: usize,
    pub focus: Focus,
    pub show_help: bool,
    /// Modal state for `/discover`. `None` means the modal is closed; the
    /// popup renders the in-progress scan results when present.
    pub discovery: Option<DiscoveryState>,
    /// How many lines back from the latest message we're scrolled. `0` =
    /// pinned to bottom (latest).
    pub scroll: usize,
    pub max_scrollback: usize,
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
}

#[derive(Debug, Clone)]
pub struct DiscoveryMethod {
    pub name: String,
    pub peers: Vec<DiscoveredPeer>,
}

#[derive(Debug, Clone)]
pub struct DiscoveredPeer {
    pub name: Option<String>,
    pub addr: std::net::SocketAddr,
    pub fingerprint: Option<String>,
}

#[derive(Clone)]
pub struct UiPeer {
    pub peer_id: PeerId,
    pub name: String,
    pub fingerprint: String,
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
    pub from_name: String,
    pub body: String,
    pub outgoing: bool,
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
            messages: VecDeque::new(),
            status: "starting…".into(),
            selected_peer: 0,
            focus: Focus::Chat,
            show_help: false,
            discovery: None,
            scroll: 0,
            max_scrollback: DEFAULT_SCROLLBACK,
        }
    }

    pub fn apply(&mut self, ev: &Event) {
        match ev {
            Event::PeerSeen {
                peer_id,
                name,
                fingerprint,
                addr,
                ..
            } => {
                if let Some(p) = self.peers.iter_mut().find(|p| &p.peer_id == peer_id) {
                    p.name = name.clone();
                    p.fingerprint = fingerprint.clone();
                } else {
                    self.peers.push(UiPeer {
                        peer_id: *peer_id,
                        name: name.clone(),
                        fingerprint: fingerprint.clone(),
                        trusted: false,
                        state: PeerState::Seen,
                    });
                }
                let _ = addr;
            }
            Event::PeerConnected {
                peer_id,
                name,
                fingerprint,
                trusted,
                ..
            } => {
                if let Some(p) = self.peers.iter_mut().find(|p| &p.peer_id == peer_id) {
                    p.name = name.clone();
                    p.fingerprint = fingerprint.clone();
                    p.trusted = *trusted;
                    p.state = PeerState::Connected;
                } else {
                    self.peers.push(UiPeer {
                        peer_id: *peer_id,
                        name: name.clone(),
                        fingerprint: fingerprint.clone(),
                        trusted: *trusted,
                        state: PeerState::Connected,
                    });
                }
            }
            Event::TextMessage {
                from_peer,
                from_name,
                body,
            } => {
                self.push_message(UiMessage {
                    from_name: from_name.clone(),
                    body: body.clone(),
                    outgoing: false,
                    ts_unix: now_unix(),
                });
                let _ = from_peer;
            }
            Event::DecryptFailed { from_name, .. } => {
                self.push_message(UiMessage {
                    from_name: "[decrypt]".into(),
                    body: format!("failed to decrypt message from {}", from_name),
                    outgoing: false,
                    ts_unix: now_unix(),
                });
            }
            Event::PeerGone { name, .. } => {
                self.push_message(UiMessage {
                    from_name: "[net]".into(),
                    body: format!("{} disconnected", name),
                    outgoing: false,
                    ts_unix: now_unix(),
                });
                if let Some(p) = self.peers.iter_mut().find(|p| &p.name == name) {
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
                            addr: p.addr,
                            fingerprint: p.fingerprint.clone(),
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
                }
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
        });
    }

    pub fn close_discovery(&mut self) {
        self.discovery = None;
    }
}

impl UiState {
    fn push_message(&mut self, m: UiMessage) {
        self.messages.push_back(m);
        while self.messages.len() > self.max_scrollback {
            self.messages.pop_front();
        }
        // Any new message resets the scroll anchor — we always show the
        // latest by default.
        self.scroll = 0;
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

/// Initialize the terminal: enter raw mode + alt-screen + hide cursor.
/// Returns the `Terminal` plus a guard struct that restores state on drop.
pub fn enter_terminal() -> std::io::Result<Terminal<CrosstermBackend<Stdout>>> {
    use crossterm::terminal::{EnterAlternateScreen, SetTitle};
    crossterm::terminal::enable_raw_mode()?;
    let mut out = stdout();
    crossterm::execute!(out, EnterAlternateScreen, SetTitle("lanchat"))?;
    let backend = CrosstermBackend::new(out);
    Ok(Terminal::new(backend)?)
}

/// Restore the terminal to its previous state. Safe to call multiple times.
pub struct TuiGuard {
    active: bool,
}
impl TuiGuard {
    pub fn new() -> std::io::Result<Self> {
        Ok(Self { active: true })
    }
}
impl Drop for TuiGuard {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        use crossterm::terminal::LeaveAlternateScreen;
        // crossterm 0.28 dropped the typed `ShowCursor` command; emit the raw
        // escape sequence instead. DCS show-cursor = ESC [ ? 25 h.
        let _ = crossterm::execute!(stdout(), LeaveAlternateScreen);
        let _ = std::io::Write::write_all(&mut stdout(), b"\x1B[?25h");
        let _ = crossterm::terminal::disable_raw_mode();
        self.active = false;
    }
}

/// Render one frame using the supplied theme + glyph palette.
pub fn render(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    state: &UiState,
    theme: &Theme,
    glyphs: &Glyphs,
) -> std::io::Result<()> {
    terminal.draw(|f| {
        let area = f.area();

        // Outer root: take the full area. Two vertical bands — body + footer.
        let outer = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(3), Constraint::Length(3)])
            .split(area);
        let body = outer[0];
        let footer = outer[1];

        // Body: sidebar | chat.
        let cols = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(24), Constraint::Min(10)])
            .split(body);
        let sidebar_area = cols[0];
        let chat_area = cols[1];

        draw_sidebar(f, sidebar_area, state, theme, glyphs);
        draw_chat(f, chat_area, state, theme, glyphs);
        draw_footer(f, footer, state, theme, glyphs);

        if state.show_help {
            help::render(f, theme, glyphs);
        }
        if let Some(d) = &state.discovery {
            discovery_popup::render(f, theme, glyphs, d);
        }
    })?;
    Ok(())
}

fn draw_sidebar(f: &mut Frame, area: Rect, state: &UiState, theme: &Theme, glyphs: &Glyphs) {
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
            format!(" {} Peers ({}) ", glyphs.cursor, state.peers.len()),
            Style::default().fg(theme.accent).add_modifier(Modifier::BOLD),
        ));

    let items: Vec<ListItem> = state
        .peers
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let dot = match p.state {
                PeerState::Connected => glyphs.dot_connected,
                PeerState::Seen => glyphs.dot_seen,
                PeerState::Gone => glyphs.dot_gone,
            };
            let trust = if p.trusted { glyphs.trusted } else { glyphs.untrusted };
            let style = if p.trusted {
                theme.trusted_style()
            } else {
                theme.untrusted_style()
            };
            let name_style = if p.state == PeerState::Connected {
                theme.self_message_style()
            } else {
                theme.peer_message_style()
            };
            let label = if i == state.selected_peer {
                Line::from(vec![
                    Span::styled(
                        format!("{} {} {}", dot, trust, p.name),
                        name_style.add_modifier(Modifier::BOLD),
                    ),
                ])
            } else {
                Line::from(vec![
                    Span::styled(format!("{} {} ", dot, trust), style),
                    Span::styled(p.name.clone(), name_style),
                ])
            };
            ListItem::new(label)
        })
        .collect();

    f.render_widget(List::new(items).block(block), area);
}

fn draw_chat(f: &mut Frame, area: Rect, state: &UiState, theme: &Theme, glyphs: &Glyphs) {
    let active = state.focus == Focus::Chat;
    let selected_name = state.selected().map(|p| p.name.clone()).unwrap_or_default();
    let title = if selected_name.is_empty() {
        format!(" {} lanchat — {} ", glyphs.cursor, state.self_name)
    } else {
        format!(
            " {} {} {} {} ",
            glyphs.cursor,
            state.self_name,
            glyphs.arrow,
            selected_name
        )
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(theme.border_style(active))
        .title(Span::styled(title, Style::default().fg(theme.accent).add_modifier(Modifier::BOLD)));

    // Apply scroll: when scrolled back, show messages ending at
    // `len - scroll`. Slice to whatever fits in the area.
    let total = state.messages.len();
    let visible_n = (area.height as usize).saturating_sub(2); // minus borders
    let end = total.saturating_sub(state.scroll);
    let start = end.saturating_sub(visible_n);
    let visible: Vec<Line> = state
        .messages
        .iter()
        .skip(start)
        .take(end - start)
        .map(|m| {
            let who_style = if m.outgoing {
                theme.self_message_style()
            } else {
                theme.peer_message_style()
            };
            let who = if m.outgoing {
                state.self_name.clone()
            } else {
                m.from_name.clone()
            };
            Line::from(vec![
                Span::styled(format!("{}: ", who), who_style.add_modifier(Modifier::BOLD)),
                Span::styled(m.body.clone(), Style::default().fg(theme.fg).bg(theme.bg)),
            ])
        })
        .collect();

    let para = Paragraph::new(visible)
        .block(block)
        .wrap(Wrap { trim: false })
        .style(Style::default().fg(theme.fg).bg(theme.bg));
    f.render_widget(para, area);
}

fn draw_footer(f: &mut Frame, area: Rect, state: &UiState, theme: &Theme, glyphs: &Glyphs) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(theme.border_style(state.focus == Focus::Chat))
        .title(Span::styled(
            format!(" {} message ", glyphs.arrow),
            Style::default().fg(theme.accent),
        ));
    // The prompt character reflects focus.
    let prefix = match state.focus {
        Focus::Sidebar => format!("[{}] ", glyphs.cursor),
        Focus::Chat => format!("{} ", glyphs.arrow),
    };
    let line = Line::from(vec![
        Span::styled(prefix, theme.highlight_style()),
        Span::styled(state.status.clone(), Style::default().fg(theme.fg).bg(theme.status_bg)),
    ]);
    f.render_widget(Paragraph::new(line).block(block), area);
}

/// Drain all pending events from the receiver into the shared state.
pub fn drain_events(rx: &std::sync::mpsc::Receiver<Event>, state: &mut UiState) {
    while let Ok(ev) = rx.try_recv() {
        state.apply(&ev);
    }
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
        if db.by_peer_id(&p.peer_id).is_none() {
            let c = Contact {
                peer_id: p.peer_id,
                name: p.name.clone(),
                public_key: [0u8; 32], // filled by main when known
                last_addr: None,
                last_seen_unix: now,
                trusted: p.trusted,
            };
            db.upsert(c);
        }
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
        };
        let mut s = UiState::from_identity(&id);
        let ev = Event::PeerSeen {
            peer_id: [1u8; 16],
            name: "bob".into(),
            public_key: [0u8; 32],
            fingerprint: "deadbeef00000000".into(),
            addr: "127.0.0.1:1".parse().unwrap(),
        };
        s.apply(&ev);
        assert_eq!(s.peers.len(), 1);
        assert_eq!(s.peers[0].name, "bob");
    }

    #[test]
    fn apply_text_message_appends() {
        let id = Identity {
            peer_id: [0u8; 16],
            keypair: crate::crypto::Keypair::generate(),
            name: "alice".into(),
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
    fn cycle_focus_toggles() {
        let id = Identity {
            peer_id: [0u8; 16],
            keypair: crate::crypto::Keypair::generate(),
            name: "alice".into(),
        };
        let mut s = UiState::from_identity(&id);
        assert_eq!(s.focus, Focus::Chat);
        s.cycle_focus();
        assert_eq!(s.focus, Focus::Sidebar);
        s.cycle_focus();
        assert_eq!(s.focus, Focus::Chat);
    }

    #[test]
    fn discovery_lifecycle() {
        let id = Identity {
            peer_id: [0u8; 16],
            keypair: crate::crypto::Keypair::generate(),
            name: "alice".into(),
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
                addr: "10.0.0.2:7777".parse().unwrap(),
                fingerprint: Some("abcd".into()),
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
    fn discovery_update_replaces_existing_method_results() {
        let id = Identity {
            peer_id: [0u8; 16],
            keypair: crate::crypto::Keypair::generate(),
            name: "alice".into(),
        };
        let mut s = UiState::from_identity(&id);
        s.start_discovery();
        s.apply(&Event::DiscoveryUpdate {
            method: "TCP subnet scan".into(),
            peers: vec![crate::events::DiscoveredPeer {
                name: None,
                addr: "10.0.0.3:7777".parse().unwrap(),
                fingerprint: None,
            }],
        });
        s.apply(&Event::DiscoveryUpdate {
            method: "TCP subnet scan".into(),
            peers: vec![
                crate::events::DiscoveredPeer {
                    name: None,
                    addr: "10.0.0.3:7777".parse().unwrap(),
                    fingerprint: None,
                },
                crate::events::DiscoveredPeer {
                    name: None,
                    addr: "10.0.0.4:7777".parse().unwrap(),
                    fingerprint: None,
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
        };
        let mut s = UiState::from_identity(&id);
        let mk = |pid: u8, name: &str, state: PeerState| UiPeer {
            peer_id: [pid; 16],
            name: name.into(),
            fingerprint: String::new(),
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
        };
        let mut s = UiState::from_identity(&id);
        let mk = |pid: u8, name: &str, state: PeerState| UiPeer {
            peer_id: [pid; 16],
            name: name.into(),
            fingerprint: String::new(),
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
}