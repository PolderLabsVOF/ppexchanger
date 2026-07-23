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
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, BorderType, List, ListItem, Paragraph, Wrap};
use ratatui::Terminal;
use std::collections::VecDeque;
use std::io::{stdout, Stdout};
use std::time::{SystemTime, UNIX_EPOCH};

/// Layout constants shared between `render()` and `hit_test()`. The
/// sidebar column is 24 cells wide; the body row is at least 3 cells
/// tall. Changing these constants in one place is enough.
const SIDEBAR_WIDTH: u16 = 24;
const FOOTER_HEIGHT: u16 = 3;
const BODY_MIN_HEIGHT: u16 = 3;

/// Three rectangles produced by the single `Layout` pass. Hit-test and
/// render both build on this so the click map matches what the user
/// sees on screen.
pub struct LayoutAreas {
    pub sidebar: Rect,
    pub chat: Rect,
    pub footer: Rect,
}

/// Hit-test result for one mouse event. `Sidebar(i)` is the index into
/// the sorted `state.peers` slice (the same order the sidebar renders
/// in); `Chat` covers the chat pane; `Footer` is the input line area;
/// `Modal` means the click landed inside a modal popup (show_help or
/// discovery) and the main loop should consume it without further
/// dispatch.
#[derive(Debug)]
pub enum Hit {
    Sidebar(usize),
    Chat,
    Footer,
    Modal,
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
    /// Bounded message ring; older entries are dropped when full.
    pub messages: VecDeque<UiMessage>,
    pub status: String,
    pub selected_peer: usize,
    pub focus: Focus,
    pub show_help: bool,
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
    /// Local UI tab: false = list, true = canvas map. Flipped by `2` /
    /// `3` in the popup; not persisted.
    pub view_map: bool,
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
            file_offer: None,
            discovery: None,
            settings: None,
            show_logo: true,
            scanline_tick: false,
            scroll: 0,
            max_scrollback: DEFAULT_SCROLLBACK,
        }
    }

    /// Open the settings popup seeded from the current `cfg`. Idempotent:
    /// opening twice is a no-op rather than reset the cursor.
    pub fn open_settings(&mut self, cfg: &crate::tui::UiConfig) {
        if self.settings.is_none() {
            self.settings = Some(SettingsState::new(cfg));
        }
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
                        from_name: "[file]".into(),
                        body: format!(
                            "{} offers file: {} ({} bytes) — busy with another",
                            from_name,
                            offer.name,
                            offer.size
                        ),
                        outgoing: false,
                        ts_unix: now_unix(),
                    });
                }
            }
            Event::FileReceived {
                from_name,
                name,
                bytes,
                saved_to,
                ..
            } => {
                self.file_offer = None;
                self.push_message(UiMessage {
                    from_name: "[file]".into(),
                    body: format!(
                        "{} sent {} ({} bytes) → {}",
                        from_name,
                        name,
                        bytes,
                        saved_to.display()
                    ),
                    outgoing: false,
                    ts_unix: now_unix(),
                });
            }
            Event::FileAborted {
                from_name,
                name,
                reason,
                ..
            } => {
                self.file_offer = None;
                self.push_message(UiMessage {
                    from_name: "[file]".into(),
                    body: format!(
                        "{}: transfer of {} aborted ({})",
                        from_name, name, reason
                    ),
                    outgoing: false,
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

/// Compute the three rectangles that the TUI is split into. Used by
/// `render()` (to lay out widgets) and `hit_test()` (to map clicks
/// back to panes). Returns the same shape regardless of caller, so a
/// click on a peer name in the sidebar always corresponds to the row
/// the user can see.
pub fn compute_layout(area: Rect) -> LayoutAreas {
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(BODY_MIN_HEIGHT), Constraint::Length(FOOTER_HEIGHT)])
        .split(area);
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(SIDEBAR_WIDTH), Constraint::Min(10)])
        .split(outer[0]);
    LayoutAreas {
        sidebar: cols[0],
        chat: cols[1],
        footer: outer[1],
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
    screen: Rect,
    col: u16,
    row: u16,
    areas: &LayoutAreas,
    modal_open: bool,
    peers_len: usize,
) -> Hit {
    // Modals always win — they draw over the centre, so any click in
    // that rect must NOT fall through to the chat pane.
    if modal_open && point_in_rect(modal_rect(screen), col, row) {
        return Hit::Modal;
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
    crossterm::execute!(out, EnterAlternateScreen, SetTitle("lanchat"))?;
    let backend = CrosstermBackend::new(out);
    Ok(Terminal::new(backend)?)
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
        // Single source of truth for the layout — hit_test reuses it.
        let areas = compute_layout(area);

        draw_sidebar(f, areas.sidebar, state, theme, glyphs);
        draw_chat(f, areas.chat, state, theme, glyphs);
        draw_footer(f, areas.footer, state, theme, glyphs);

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
        // Startup logo: only on a fresh session and only when the chat
        // pane is empty. Dismissed by sending a message or hitting Esc.
        if state.show_logo && state.messages.is_empty() {
            art::render(f, areas.chat, art::LogoKind::Large, theme);
        }
        // Per-pane background gradient. Cheap overlay: an empty row
        // painted with alternating accent / status_bg colors that
        // shifts each frame via `scanline_tick`. Reads as a soft
        // scan / gradient without the CPU cost of per-pixel blending.
        draw_gradient_overlay(f, areas.sidebar, theme, state.scanline_tick);
        draw_gradient_overlay(f, areas.chat, theme, state.scanline_tick);
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

/// Paint a soft horizontal sweep across the pane interior. The sweep
/// shifts one column per render (toggled by `tick`) so it reads as a
/// gentle gradient rather than a static stripe. We render an empty
/// `Paragraph` row over the bg-colored Block; ratatui composites the
/// row's `bg` over whatever was below, giving the moving-band effect.
///
/// Implementation is cheap: a single-line Paragraph of `width` spans,
/// each painted with one of three palette tones (bg → status_bg → accent).
fn draw_gradient_overlay(f: &mut Frame, area: Rect, theme: &Theme, tick: bool) {
    if area.width < 3 || area.height < 3 {
        return;
    }
    // Only apply inside the borders (shrink by 1 row top + bottom, 1 col
    // left + right). The bands shift on tick.
    let inner = Rect {
        x: area.x + 1,
        y: area.y + 1,
        width: area.width.saturating_sub(2),
        height: area.height.saturating_sub(2),
    };
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    let mut spans: Vec<Span> = Vec::with_capacity(inner.width as usize);
    for col in 0..inner.width {
        // Phase cycle is 4 cells wide; tick adds 1 cell offset so the
        // band appears to drift horizontally.
        let phase = ((col + (if tick { 1 } else { 0 })) % 4) as u8;
        let color = match phase {
            0 => theme.bg,
            1 => theme.status_bg,
            2 => theme.status_bg,
            _ => theme.accent, // single accent column reads as a moving "highlight"
        };
        spans.push(Span::styled(" ", Style::default().bg(color).fg(color)));
    }
    // Limit to one row per overlay call so the cost is bounded. We pick
    // the middle row of the pane — at the bottom of the title strip and
    // above the content — so the band is visible without obscuring text.
    let row = inner.y + inner.height / 2;
    f.render_widget(
        Paragraph::new(Line::from(spans)).style(Style::default().bg(theme.bg)),
        Rect::new(inner.x, row, inner.width, 1),
    );
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
    let dim_phase = state.scanline_tick; // flips each frame for CRT effect
    let visible: Vec<Line> = state
        .messages
        .iter()
        .skip(start)
        .take(end - start)
        .enumerate()
        .map(|(i, m)| {
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
            let mut body_style =
                Style::default().fg(theme.fg).bg(theme.bg);
            // CRT scanline: every other row gets a DIM modifier so the
            // text appears to scan, alternating each frame via
            // `scanline_tick`. The offset by `dim_phase` makes the
            // "band" crawl down the pane.
            if dim_phase ^ (i % 2 == 1) {
                body_style = body_style.add_modifier(Modifier::DIM);
            }
            Line::from(vec![
                Span::styled(format!("{}: ", who), who_style.add_modifier(Modifier::BOLD)),
                Span::styled(m.body.clone(), body_style),
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
    fn scanline_tick_inits_false() {
        let id = Identity {
            peer_id: [0u8; 16],
            keypair: crate::crypto::Keypair::generate(),
            name: "alice".into(),
        };
        let s = UiState::from_identity(&id);
        assert!(!s.scanline_tick);
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
    fn discovery_view_map_defaults_false_and_toggles() {
        let id = Identity {
            peer_id: [0u8; 16],
            keypair: crate::crypto::Keypair::generate(),
            name: "alice".into(),
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

    // Layout / hit-test coverage.

    fn synthetic_layout() -> (Rect, LayoutAreas) {
        let screen = Rect::new(0, 0, 80, 24);
        let areas = compute_layout(screen);
        (screen, areas)
    }

    #[test]
    fn compute_layout_produces_three_rects() {
        let (screen, areas) = synthetic_layout();
        // Outer is body + 3-tall footer; sidebar is 24 wide.
        assert_eq!(areas.sidebar.width, SIDEBAR_WIDTH);
        assert_eq!(areas.footer.height, FOOTER_HEIGHT);
        assert_eq!(areas.chat.x, areas.sidebar.x + SIDEBAR_WIDTH);
        assert_eq!(areas.footer.y, screen.height - FOOTER_HEIGHT);
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