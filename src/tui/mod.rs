//! ratatui-driven terminal UI.
//!
//! Layout:
//!   ┌─ lanchat ─ alice ─────────────────────────────────────────────┐
//!   │ Peers (n)       │ alice: hi                                    │
//!   │  bob  trusted   │ bob:  yo                                     │
//!   │  carol pending  │ alice: how r u?                              │
//!   ├─────────────────┴──────────────────────────────────────────────┤
//!   │ > typing...                                                    │
//!   └────────────────────────────────────────────────────────────────┘

pub mod input;

use crate::events::{Action, Bus, Event, PeerId};
use crate::identity::Identity;
use crate::peerdb::{Contact, PeerDb};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph};
use ratatui::Terminal;
use std::collections::VecDeque;
use std::io::{stdout, Stdout};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

/// Shared UI state the render loop reads. Owned by the UI thread; the network
/// thread only ever sends immutable events over the bus.
pub struct UiState {
    pub self_name: String,
    pub self_fingerprint: String,
    pub self_peer_id: PeerId,
    pub peers: Vec<UiPeer>,
    pub messages: VecDeque<UiMessage>,
    pub status: String,
    pub selected_peer: usize,
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
                self.messages.push_back(UiMessage {
                    from_name: from_name.clone(),
                    body: body.clone(),
                    outgoing: false,
                    ts_unix: now_unix(),
                });
                let _ = from_peer;
            }
            Event::DecryptFailed { from_name, .. } => {
                self.messages.push_back(UiMessage {
                    from_name: "[decrypt]".into(),
                    body: format!("failed to decrypt message from {}", from_name),
                    outgoing: false,
                    ts_unix: now_unix(),
                });
            }
            Event::PeerGone { name, .. } => {
                self.messages.push_back(UiMessage {
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
            _ => {}
        }
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

/// Render one frame.
pub fn render(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    state: &UiState,
) -> std::io::Result<()> {
    terminal.draw(|f| {
        let area = f.area();
        let cols = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(22), Constraint::Min(10)])
            .split(area);
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(3),
                Constraint::Length(3),
            ])
            .split(area);
        let _ = cols;

        let peers: Vec<ListItem> = state
            .peers
            .iter()
            .map(|p| {
                let tag = match p.state {
                    PeerState::Seen => "·",
                    PeerState::Connected => "*",
                    PeerState::Gone => "x",
                };
                let trust = if p.trusted { "T" } else { " " };
                ListItem::new(Line::from(vec![
                    Span::raw(format!("{} {} ", tag, trust)),
                    Span::styled(p.name.clone(), Style::default().fg(Color::Cyan)),
                ]))
            })
            .collect();
        let peers_block = Block::default()
            .borders(Borders::ALL)
            .title(format!("Peers ({})", state.peers.len()));
        f.render_widget(
            List::new(peers).block(peers_block),
            Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Length(22), Constraint::Min(10)])
                .split(rows[0])[0],
        );

        let mut lines: Vec<Line> = Vec::with_capacity(state.messages.len());
        for m in state.messages.iter() {
            let who = if m.outgoing {
                state.self_name.clone()
            } else {
                m.from_name.clone()
            };
            lines.push(Line::from(format!("{}: {}", who, m.body)));
        }
        let chat_block = Block::default()
            .borders(Borders::ALL)
            .title(format!("lanchat — {}", state.self_name));
        f.render_widget(
            Paragraph::new(lines).block(chat_block),
            Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Length(22), Constraint::Min(10)])
                .split(rows[0])[1],
        );

        let input_block = Block::default().borders(Borders::ALL).title("> type and press Enter");
        f.render_widget(
            Paragraph::new(state.status.clone()).block(input_block),
            rows[1],
        );
    })?;
    Ok(())
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
}