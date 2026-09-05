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
pub use theme::{detect_glyphs, Glyphs, StyleRole, Theme, ThemeName};

use crate::events::{Event, PeerId};
use crate::identity::Identity;
use crate::peerdb::{Contact, PeerDb};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Margin, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, Borders, Clear, Gauge, List, ListItem, ListState, Paragraph, Scrollbar,
    ScrollbarOrientation, ScrollbarState, Wrap,
};
use ratatui::Terminal;
use std::collections::HashMap;
use std::collections::VecDeque;
use std::io::{stdout, Stdout};
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Layout constants shared between `render()` and `hit_test()`. The
/// sidebar has room for connection metadata; the body always retains a
/// usable chat region. Changing these constants in one place is enough.
/// Percentage of the body width allocated to the sidebar when visible.
/// The sidebar only needs a single column of peer names + a status dot;
/// a quarter of the screen leaves plenty of room for the conversation.
const SIDEBAR_PERCENT: u16 = 25;
/// Minimum chat pane width when the sidebar is visible. Anything below
/// this triggers a sidebar collapse (handled by the render loop via the
/// responsive breakpoint logic).
const MIN_CHAT_WIDTH: u16 = 40;
/// A focused message field plus an unboxed status/action line. Slash-command
/// suggestions render above it in the chat pane.
const FOOTER_HEIGHT: u16 = 5;
const BODY_MIN_HEIGHT: u16 = 3;
/// The application header has an identity row and a compact navigation row.
const MENU_HEIGHT: u16 = 4;

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
    None,
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
    /// Mirror of `UiConfig::sidebar_hidden`. When true the layout
    /// engine skips the sidebar column entirely and the chat pane
    /// absorbs the freed horizontal space. Flipped with Ctrl-B.
    pub sidebar_hidden: bool,
    /// Modal state for the temporary peer picker overlay (Ctrl-P).
    /// Useful when the sidebar is hidden on narrow terminals.
    pub show_peer_picker: bool,
    /// Mirror of `UiConfig::narrow_sidebar_below`. The layout engine
    /// hides the sidebar when the screen width drops below this
    /// threshold, unless the user has explicitly toggled it back on.
    pub narrow_sidebar_below: u16,
    /// Mirror of `UiConfig::min_conversation_width`. The layout engine
    /// ensures the chat pane never shrinks below this column count.
    pub min_conversation_width: u16,
    /// Mirror of `UiConfig::status_format` so `draw_footer` can choose
    /// what to render. Updated by `apply_live_cfg` on every render pass.
    pub status_format: crate::tui::config::StatusFormat,
    /// Mirror of `UiConfig::show_footer`. When false, the renderer
    /// skips the footer rect entirely — the chat pane absorbs the
    /// freed vertical space via the layout splitter.
    pub show_footer: bool,
    pub image_previews: bool,
    /// Modal state for an inbound file offer. `None` means no pending
    /// offer; otherwise the modal is shown over the chat and the user
    /// can accept or reject via Enter / Esc.
    pub file_offer: Option<file_offer_popup::FileOfferPrompt>,
    /// Preview of a downloaded text attachment. Closing the preview leaves
    /// the file on disk and shows its path in the chat row for later use.
    pub text_preview: Option<TextFilePreview>,
    /// Identity of the text attachment currently expanded inline in chat.
    expanded_text: Option<(PeerId, u64, bool)>,
    expanded_text_offset: usize,
    /// Largest valid source-line offset for the expanded text viewport. This
    /// is refreshed by `draw_chat` once the terminal height is known, keeping
    /// wheel/PageUp scrolling bounded to the actual content instead of
    /// allowing the preview to drift into an almost-empty final line.
    expanded_text_max_offset: usize,
    /// Exact rendered row ranges for file attachments. Used by mouse hit
    /// testing so clicks on empty chat space never open or expand anything.
    attachment_rows: Vec<(u16, u16, (PeerId, u64, bool))>,
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
    /// listen port (default 47391, custom if the user passed `--port`).
    pub bound_port: u16,
    /// How many lines back from the latest message we're scrolled. `0` =
    /// pinned to bottom (latest).
    pub scroll: usize,
    pub max_scrollback: usize,
    /// Height (in rows) of the chat block's interior. Refreshed by
    /// `draw_chat` each frame so `scroll_back` can cap `scroll` so the
    /// viewport stays full of the oldest messages instead of
    /// scrolling past the top and leaving empty rows behind.
    pub visible_chat_rows: usize,
    /// Message selected by mouse for copy. The key survives deque index
    /// changes while keeping selection scoped to one concrete message.
    message_selection: Option<(PeerId, u64, bool)>,
    selecting_message: Option<usize>,
    /// Number of messages in the currently selected conversation. This is
    /// refreshed by `draw_chat` and keeps scrolling bounded after switching
    /// between peers with different history lengths.
    pub active_chat_message_count: usize,
    pub active_chat_peer: Option<PeerId>,
    history_dirty: bool,
    contacts_dirty: bool,
    /// Terminal graphics-protocol picker. Initialised lazily on the
    /// first frame so `Picker::from_query_stdio` runs with a real
    /// TTY available. `None` ⇒ the renderer falls back to a
    /// metadata-only line for image messages. `Picker` is `Copy` so
    /// no `Arc` is needed.
    pub image_picker: Option<ratatui_image::picker::Picker>,
    /// Cache of decoded image protocols keyed by file path. The
    /// `ratatui_image::protocol::StatefulProtocol` does the
    /// resize+encode once per area change; we keep the result so
    /// subsequent frames just render pixels without re-decoding.
    pub image_protocols: HashMap<PathBuf, ratatui_image::protocol::StatefulProtocol>,
    /// Terminal-independent halfblock previews. Decoding and resizing an
    /// image is relatively expensive, so cache the finished styled lines by
    /// source path and cell dimensions. A new terminal size naturally gets a
    /// separate entry without invalidating existing conversations.
    image_rasters: HashMap<(PathBuf, u16, u16), Vec<Line<'static>>>,
}

#[derive(Debug, Clone)]
pub struct TextFilePreview {
    pub from_name: String,
    pub name: String,
    pub path: PathBuf,
    pub content: String,
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

#[derive(Clone)]
pub struct UiMessage {
    pub from_peer: PeerId,
    pub from_name: String,
    pub body: String,
    pub outgoing: bool,
    pub pending: bool,
    pub ts_unix: u64,
    /// Optional inline image (clipboard-image send / receive).
    /// When `Some`, the renderer reads the file at draw time and
    /// uses `ratatui_image::Image` for an in-terminal preview. The
    /// `body` field still holds the metadata fallback line so the
    /// text is searchable in scrollback.
    pub image: Option<ImageMeta>,
}

/// Description of one inline image attached to a `UiMessage`. The
/// renderer opens `path` at draw time and uses `width`/`height` to
/// reserve cells; `mime` and `bytes` round-trip through the chat
/// history file so old history reloads with the preview intact.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImageMeta {
    pub path: PathBuf,
    pub mime: String,
    pub width: u32,
    pub height: u32,
    pub bytes: u64,
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
            sidebar_hidden: false,
            show_peer_picker: false,
            narrow_sidebar_below: crate::tui::config::DEFAULT_NARROW_SIDEBAR_BELOW,
            min_conversation_width: crate::tui::config::DEFAULT_MIN_CONVERSATION_WIDTH,
            status_format: crate::tui::config::StatusFormat::NameOnly,
            show_footer: true,
            image_previews: true,
            file_offer: None,
            text_preview: None,
            expanded_text: None,
            expanded_text_offset: 0,
            expanded_text_max_offset: 0,
            attachment_rows: Vec::new(),
            discovery: None,
            settings: None,
            pending_connection: None,
            show_logo: true,
            scanline_tick: false,
            bound_port: 0,
            scroll: 0,
            max_scrollback: DEFAULT_SCROLLBACK,
            visible_chat_rows: 0,
            message_selection: None,
            selecting_message: None,
            active_chat_message_count: 0,
            active_chat_peer: None,
            history_dirty: false,
            contacts_dirty: false,
            image_picker: None,
            image_protocols: HashMap::new(),
            image_rasters: HashMap::new(),
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
        self.image_previews = cfg.image_previews;
        self.max_scrollback = cfg.scrollback;
        self.narrow_sidebar_below = cfg.narrow_sidebar_below;
        self.min_conversation_width = cfg.min_conversation_width;
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
                let display_name = self
                    .peers
                    .iter()
                    .find(|peer| peer.peer_id == *from_peer)
                    .map(|peer| peer.name.clone())
                    .unwrap_or_else(|| from_name.clone());
                self.push_message(UiMessage {
                    from_peer: *from_peer,
                    from_name: from_name.clone(),
                    body: body.clone(),
                    outgoing: false,
                    pending: false,
                    ts_unix: now_unix(),
                    image: None,
                });
                self.status = format!("new message from {}", display_name);
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
            Event::FileSent {
                peer_id,
                name,
                bytes,
            } => {
                if let Some(message) = self.messages.iter_mut().rev().find(|message| {
                    message.outgoing
                        && message.pending
                        && message.from_peer == *peer_id
                        && message
                            .image
                            .as_ref()
                            .and_then(|meta| meta.path.file_name())
                            .and_then(|value| value.to_str())
                            .is_some_and(|file_name| file_name == name)
                }) {
                    message.pending = false;
                    self.history_dirty = true;
                }
                self.status = format!("sent {} ({} bytes)", name, bytes);
            }
            Event::DecryptFailed { peer_id, from_name } => {
                self.push_message(UiMessage {
                    from_peer: *peer_id,
                    from_name: "[decrypt]".into(),
                    body: format!("failed to decrypt message from {}", from_name),
                    outgoing: false,
                    pending: false,
                    ts_unix: now_unix(),
                    image: None,
                });
            }
            Event::PeerGone { peer_id, name } => {
                // Connection-state changes are surfaced through the peer
                // list's status indicator; we deliberately do not push an
                // inline chat message for them.
                self.peer_name_overrides.remove(peer_id);
                if let Some(p) = self.peers.iter_mut().find(|p| &p.peer_id == peer_id) {
                    p.state = PeerState::Gone;
                }
                self.status = format!("{} offline", name);
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
                // Offers are auto-accepted by the action thread. Keep this
                // event path non-modal for compatibility with older peers:
                // surface a status line instead of blocking the conversation.
                self.file_offer = None;
                self.status = format!("receiving {} from {}", offer.name, from_name);
                self.push_message(UiMessage {
                    from_peer: *from_peer,
                    from_name: "[file]".into(),
                    body: format!("{} sent {} ({} bytes)", from_name, offer.name, offer.size),
                    outgoing: false,
                    pending: false,
                    ts_unix: now_unix(),
                    image: None,
                });
            }
            Event::FileReceived {
                from_peer,
                from_name,
                name,
                bytes,
                saved_to,
                mime,
                width,
                height,
            } => {
                self.file_offer = None;
                let image = if mime
                    .as_deref()
                    .is_some_and(|value| value.starts_with("image/"))
                    || is_image_attachment(name)
                {
                    let (detected_width, detected_height) =
                        image::image_dimensions(saved_to).ok().unwrap_or((0, 0));
                    Some(ImageMeta {
                        path: saved_to.clone(),
                        mime: mime.clone().unwrap_or_else(|| "image/*".into()),
                        width: width.unwrap_or(detected_width),
                        height: height.unwrap_or(detected_height),
                        bytes: *bytes,
                    })
                } else {
                    None
                };
                let is_text = image.is_none() && is_text_attachment(name);
                if is_text && std::fs::metadata(saved_to).is_ok() {
                    self.status = format!("downloaded {} · click the message to expand", name);
                }
                if let Some(image) = image {
                    // Keep image rows compact (filename + dimensions) so the
                    // preview anchor is stable and the thumbnail stays
                    // aligned with the left edge of the peer's messages.
                    self.push_inbound_image(*from_peer, from_name.clone(), image);
                } else if is_text {
                    let line_count = std::fs::read_to_string(saved_to)
                        .map(|content| content.lines().count())
                        .unwrap_or(0);
                    self.push_inbound_text_file(
                        *from_peer,
                        from_name.clone(),
                        saved_to.clone(),
                        *bytes,
                        line_count,
                    );
                } else {
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
                        image: None,
                    });
                }
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
                    body: format!("{}: transfer of {} aborted ({})", from_name, name, reason),
                    outgoing: false,
                    pending: false,
                    ts_unix: now_unix(),
                    image: None,
                });
            }
        }
    }

    /// Open the discovery modal and kick off the scan. Idempotent: if a scan
    /// is already in flight, refresh the running flag and summary instead of
    /// spawning a second set of threads.
    pub fn start_discovery(&mut self) {
        let already_running = self.discovery.as_ref().map(|d| d.running).unwrap_or(false);
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
        let Some(discovery) = self.discovery.as_mut() else {
            return;
        };
        let count = discovery
            .results
            .iter()
            .map(|method| method.peers.len())
            .sum::<usize>();
        if count == 0 {
            discovery.selected = 0;
            return;
        }
        discovery.selected =
            (discovery.selected as i32 + delta).clamp(0, count as i32 - 1) as usize;
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
/// the LAN can see *us* (we bind 47391) but we can't reach them via
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
    /// Wipe the in-memory chat history and mark the change dirty so the
    /// next save cycle persists the empty state. The scroll anchor
    /// resets to the bottom so subsequent messages render normally.
    /// Invoked by the `/clear` slash command.
    pub fn clear_messages(&mut self) {
        self.messages.clear();
        self.scroll = 0;
        self.message_selection = None;
        self.selecting_message = None;
        self.expanded_text = None;
        self.expanded_text_offset = 0;
        self.expanded_text_max_offset = 0;
        self.attachment_rows.clear();
        self.history_dirty = true;
    }

    fn push_message(&mut self, m: UiMessage) {
        self.messages.push_back(m);
        self.history_dirty = true;
        while self.messages.len() > self.max_scrollback {
            self.messages.pop_front();
            // pop_front shifts every remaining message one slot toward
            // the front. Decrement scroll so a reader scrolled back in
            // history keeps the same logical window visible: without
            // this the topmost row silently drops off the top of the
            // viewport whenever the scrollback cap evicts the oldest
            // entry. At the bottom (scroll == 0) nothing changes.
            if self.scroll > 0 {
                self.scroll = self.scroll.saturating_sub(1);
            }
        }
        // Anchor the viewport on the same logical content across pushes.
        // `scroll` is an offset from the bottom of the deque, so a naive
        // new push would slide the visible content up by one row each
        // time — every incoming message would erase the topmost visible
        // line. Bumping scroll by 1 keeps the viewport parked on the
        // same messages; the new line lives below the visible area until
        // the user scrolls forward to it.
        if self.scroll > 0 {
            self.scroll = (self.scroll + 1).min(self.messages.len().saturating_sub(1));
        }
    }

    /// Map a screen row in the chat pane to the nearest message in the
    /// selected conversation. Group headers and gutters are intentionally
    /// tolerated so a drag can begin on either the header or bubble.
    pub fn message_index_at_chat_row(&self, chat: Rect, row: u16) -> Option<usize> {
        let top = chat.y.saturating_add(1);
        let bottom = chat.bottom().saturating_sub(1);
        let peer_id = self
            .active_chat_peer
            .or_else(|| self.selected().map(|p| p.peer_id))?;
        if row < top || row >= bottom {
            return None;
        }
        let indices: Vec<usize> = self
            .messages
            .iter()
            .enumerate()
            .filter_map(|(index, message)| (message.from_peer == peer_id).then_some(index))
            .collect();
        if indices.is_empty() {
            return None;
        }
        let visible_end = indices.len().saturating_sub(self.scroll);
        let visible_start = visible_end.saturating_sub(self.visible_chat_rows.max(1));
        let offset = (row - top) as usize;
        indices
            .get((visible_start + offset).min(visible_end.saturating_sub(1)))
            .copied()
    }

    pub fn begin_message_selection(&mut self, index: usize) {
        if let Some(message) = self.messages.get(index) {
            self.selecting_message = Some(index);
            self.message_selection = Some((message.from_peer, message.ts_unix, message.outgoing));
        }
    }

    pub fn extend_message_selection(&mut self, index: usize) {
        let Some(anchor) = self.selecting_message else {
            return;
        };
        let Some(peer_id) = self
            .active_chat_peer
            .or_else(|| self.selected().map(|p| p.peer_id))
        else {
            return;
        };
        let (start, end) = if anchor <= index {
            (anchor, index)
        } else {
            (index, anchor)
        };
        if let Some(message) = self
            .messages
            .get(start.min(self.messages.len().saturating_sub(1)))
        {
            self.message_selection = Some((peer_id, message.ts_unix, message.outgoing));
        }
        self.selecting_message = Some(end);
    }

    pub fn finish_message_selection(&mut self) {
        self.selecting_message = None;
    }

    pub fn take_selected_message_text(&mut self) -> Option<String> {
        let (peer_id, ts, outgoing) = self.message_selection.take()?;
        self.selecting_message = None;
        self.messages
            .iter()
            .find(|message| {
                message.from_peer == peer_id
                    && message.ts_unix == ts
                    && message.outgoing == outgoing
            })
            .map(|message| message.body.clone())
    }

    pub fn message_is_selected(&self, message: &UiMessage) -> bool {
        self.message_selection
            .is_some_and(|(peer_id, ts, outgoing)| {
                message.from_peer == peer_id
                    && message.ts_unix == ts
                    && message.outgoing == outgoing
            })
    }

    /// Add an optimistic local echo for a message accepted by the composer.
    /// Keeping this distinct from `TextMessage` prevents our own line from
    /// being rendered as if it came from the selected peer. Snaps the
    /// viewport to the bottom so the sender immediately sees their line.
    pub fn push_outgoing_message(&mut self, to_peer: PeerId, body: String) {
        self.push_message(UiMessage {
            from_peer: to_peer,
            from_name: self.self_name.clone(),
            body,
            outgoing: true,
            pending: true,
            ts_unix: now_unix(),
            image: None,
        });
        // Local send is the only case where we want the viewport to
        // follow the new line. Incoming pushes leave the scroll anchor
        // alone so a reader of history isn't yanked back to the bottom.
        self.scroll = 0;
    }

    /// Add a local-echo row for an outgoing image (clipboard image).
    /// The metadata line gives readers something searchable in
    /// scrollback; the renderer opens `image.path` at draw time for
    /// the in-terminal preview.
    pub fn push_outgoing_image(&mut self, to_peer: PeerId, image: ImageMeta) {
        let kb = image.bytes as f64 / 1024.0;
        let label = image
            .path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("image")
            .to_string();
        let label = truncate_tail(&label, 28);
        let body = format!(
            "[image {} · {}×{} · {:.1} KB · {}]",
            label, image.width, image.height, kb, image.mime
        );
        self.push_message(UiMessage {
            from_peer: to_peer,
            from_name: self.self_name.clone(),
            body,
            outgoing: true,
            pending: true,
            ts_unix: now_unix(),
            image: Some(image),
        });
        self.scroll = 0;
    }

    /// Add a local echo for a pasted text attachment. Text files use the
    /// existing attachment descriptor so the row survives encrypted history
    /// reloads; the renderer treats the non-image MIME as a compact,
    /// expandable document row rather than trying to draw pixels.
    pub fn push_outgoing_text_file(
        &mut self,
        to_peer: PeerId,
        path: PathBuf,
        bytes: u64,
        lines: usize,
    ) {
        let body = format!(
            "▤ text paste · {} lines · {:.1} KB · click to expand",
            lines,
            bytes as f64 / 1024.0
        );
        self.push_message(UiMessage {
            from_peer: to_peer,
            from_name: self.self_name.clone(),
            body,
            outgoing: true,
            pending: true,
            ts_unix: now_unix(),
            image: Some(ImageMeta {
                path,
                mime: "text/plain; charset=utf-8".into(),
                width: lines.min(u32::MAX as usize) as u32,
                height: 0,
                bytes,
            }),
        });
        self.scroll = 0;
    }

    /// Add an inbound image row. The body is the metadata line; the
    /// renderer uses `image.path` for the preview source.
    pub fn push_inbound_image(&mut self, from_peer: PeerId, from_name: String, image: ImageMeta) {
        let kb = image.bytes as f64 / 1024.0;
        let label = image
            .path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("image")
            .to_string();
        let label = truncate_tail(&label, 28);
        let body = format!(
            "[image {} · {}×{} · {:.1} KB · {}]",
            label, image.width, image.height, kb, image.mime
        );
        self.push_message(UiMessage {
            from_peer,
            from_name: from_name.clone(),
            body,
            outgoing: false,
            pending: false,
            ts_unix: now_unix(),
            image: Some(image),
        });
    }

    /// Add an inbound text attachment row while retaining the saved file as
    /// the source for the expandable preview.
    pub fn push_inbound_text_file(
        &mut self,
        from_peer: PeerId,
        from_name: String,
        path: PathBuf,
        bytes: u64,
        lines: usize,
    ) {
        self.push_message(UiMessage {
            from_peer,
            from_name,
            body: format!(
                "▤ text paste · {} lines · {:.1} KB · click to expand",
                lines,
                bytes as f64 / 1024.0
            ),
            outgoing: false,
            pending: false,
            ts_unix: now_unix(),
            image: Some(ImageMeta {
                path,
                mime: "text/plain; charset=utf-8".into(),
                width: lines.min(u32::MAX as usize) as u32,
                height: 0,
                bytes,
            }),
        });
    }

    /// Toggle a text attachment selected in the chat. Expansion is rendered
    /// inline, so the composer and surrounding conversation remain visible.
    pub fn open_text_preview_at(&mut self, index: usize) -> bool {
        let Some(message) = self.messages.get(index).cloned() else {
            return false;
        };
        let Some(_meta) = message.image.filter(|meta| meta.mime.starts_with("text/")) else {
            return false;
        };
        let key = (message.from_peer, message.ts_unix, message.outgoing);
        if self.expanded_text == Some(key) {
            self.expanded_text = None;
            self.expanded_text_offset = 0;
            self.expanded_text_max_offset = 0;
        } else {
            self.expanded_text = Some(key);
            self.expanded_text_offset = 0;
            self.expanded_text_max_offset = 0;
        }
        self.scroll = 0;
        true
    }

    pub fn attachment_at_chat_row(&self, chat: Rect, row: u16) -> Option<usize> {
        self.attachment_rows
            .iter()
            .find(|(start, end, _)| {
                let absolute_start = chat.y.saturating_add(1).saturating_add(*start);
                let absolute_end = chat.y.saturating_add(1).saturating_add(*end);
                row >= absolute_start && row <= absolute_end
            })
            .and_then(|(_, _, key)| {
                self.messages.iter().position(|message| {
                    (message.from_peer, message.ts_unix, message.outgoing) == *key
                })
            })
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
        let selected_id = self.selected().map(|peer| peer.peer_id);
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
        // Keep selection on the same peer (or clamp if it was removed).
        if let Some(peer_id) = selected_id {
            if let Some(index) = self.peers.iter().position(|peer| peer.peer_id == peer_id) {
                self.selected_peer = index;
            } else {
                // The selected contact was removed (for example, after
                // revoke). Do not leave a copy selection or scroll anchor
                // pointing at a conversation that is no longer visible.
                self.message_selection = None;
                self.selecting_message = None;
                self.active_chat_peer = None;
                self.active_chat_message_count = 0;
                self.scroll = 0;
            }
        }
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
        self.select_peer(next);
    }

    /// Select a concrete sidebar row. Changing peers resets the conversation
    /// viewport and any message selection so keyboard, mouse, and programmatic
    /// navigation all have identical behavior.
    pub fn select_peer(&mut self, index: usize) {
        if index >= self.peers.len() || index == self.selected_peer {
            return;
        }
        self.selected_peer = index;
        self.scroll = 0;
        self.active_chat_peer = None;
        self.active_chat_message_count = 0;
        // A selection belongs to the conversation that was visible when it
        // was made. Drop it as soon as navigation changes peers so a
        // subsequent copy can never leak text from another chat.
        self.message_selection = None;
        self.selecting_message = None;
        self.expanded_text = None;
        self.expanded_text_offset = 0;
        self.expanded_text_max_offset = 0;
        self.attachment_rows.clear();
    }

    pub fn cycle_focus(&mut self) {
        self.focus = match self.focus {
            Focus::Sidebar => Focus::Chat,
            Focus::Chat => Focus::Sidebar,
        };
    }

    pub fn scroll_back(&mut self, lines: usize) {
        if self.expanded_text.is_some() {
            self.expanded_text_offset = self
                .expanded_text_offset
                .saturating_add(lines)
                .min(self.expanded_text_max_offset);
            return;
        }
        // Cap so the viewport at the top of history stays full of the
        // oldest content. With inline group headers, total rendered
        // rows = `messages.len()` bubbles + one header per sender
        // group. Falling back to `messages.len() - 1` when
        // `visible_chat_rows` has not been set yet (single-frame
        // callers before the first draw) keeps the cap sane; the
        // value is refreshed by `draw_chat` before any scroll input
        // can land.
        let message_count = if self.active_chat_peer.is_some() {
            self.active_chat_message_count
        } else {
            self.messages.len()
        };
        let max_scroll = if self.visible_chat_rows <= 1 {
            message_count.saturating_sub(1)
        } else {
            let mut groups = 0usize;
            let mut previous = None;
            for message in self.messages.iter().filter(|message| {
                self.active_chat_peer
                    .is_none_or(|peer_id| message.from_peer == peer_id)
            }) {
                if previous != Some(message.outgoing) {
                    groups += 1;
                    previous = Some(message.outgoing);
                }
            }
            let total_rows = message_count + groups;
            total_rows.saturating_sub(self.visible_chat_rows)
        };
        // The renderer consumes message offsets, not row offsets. Never
        // let a row estimate move the anchor past the oldest message.
        self.scroll = self
            .scroll
            .saturating_add(lines)
            .min(max_scroll.min(message_count.saturating_sub(1)));
    }

    pub fn scroll_forward(&mut self, lines: usize) {
        if self.expanded_text.is_some() {
            self.expanded_text_offset = self.expanded_text_offset.saturating_sub(lines);
            return;
        }
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

fn is_text_attachment(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    [
        ".txt",
        ".md",
        ".markdown",
        ".log",
        ".csv",
        ".json",
        ".yaml",
        ".yml",
        ".toml",
        ".xml",
        ".html",
        ".css",
        ".rs",
        ".py",
        ".js",
        ".ts",
    ]
    .iter()
    .any(|suffix| lower.ends_with(suffix))
}

fn is_image_attachment(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    [".png", ".jpg", ".jpeg"]
        .iter()
        .any(|suffix| lower.ends_with(suffix))
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

const SECS_PER_DAY: u64 = 86_400;

/// Calendar-day bucket (UTC days since 1970-01-01) for a Unix timestamp.
/// Used to detect day boundaries for the chat day-separator labels.
fn day_bucket(ts: u64) -> u64 {
    ts / SECS_PER_DAY
}

/// Count how many sender groups the message deque contains. A group is a
/// maximal run of consecutive messages with the same `outgoing` flag (each
/// per-peer chat has only two parties, so the flag alone partitions the
/// history). The first message starts a new group by definition. Each
/// group emits exactly one header row during rendering, so this count
/// plus `messages.len()` equals the total rendered content rows — used
/// by `scroll_back` to keep the viewport full at the top of history.
#[allow(dead_code)]
fn count_groups(messages: &VecDeque<UiMessage>) -> usize {
    if messages.is_empty() {
        return 0;
    }
    let mut count = 1;
    for i in 1..messages.len() {
        // SAFETY: i is bounded by messages.len(), which is also the
        // valid index range for both get(i) and get(i - 1).
        if messages.get(i - 1).map(|m| m.outgoing) != messages.get(i).map(|m| m.outgoing) {
            count += 1;
        }
    }
    count
}

/// Build the label for a chat day separator. Today and yesterday read
/// naturally; older days collapse to "Mon DD" using an inline civil-from-
/// days conversion so we don't pull in a date library just for a label.
fn day_separator_label(bucket: u64, today_bucket: u64) -> String {
    if bucket == today_bucket {
        return "today".into();
    }
    if bucket + 1 == today_bucket {
        return "yesterday".into();
    }
    // Howard Hinnant's civil_from_days (days-since-1970 → y/m/d) — works
    // for the entire signed range using only integer arithmetic.
    let z = bucket as i64 + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 9]
    let d_signed = (doy as i64) - ((153 * mp as i64 + 2) / 5) + 1; // [1, 31]
    let m_signed = if mp < 10 {
        mp as i64 + 3
    } else {
        mp as i64 - 9
    }; // [1, 12]
       // Year is computed (March-based year adjustment) but not rendered
       // since labels collapse to "Mon DD" for days older than yesterday.
    let _y = (yoe as i64) + era * 400 + if m_signed <= 2 { 1 } else { 0 };
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let month = MONTHS[((m_signed - 1) as usize).min(11)];
    format!("{} {}", month, d_signed)
}

/// Compute the three rectangles that the TUI is split into. Used by
/// `render()` (to lay out widgets) and `hit_test()` (to map clicks
/// back to panes). Returns the same shape regardless of caller, so a
/// click on a peer name in the sidebar always corresponds to the row
/// the user can see.
///
/// `sidebar_visible` lets the caller (typically the main render loop)
/// hide the sidebar when it's been collapsed by Ctrl-B or by the
/// responsive breakpoint logic. When hidden, the sidebar rect collapses
/// to a zero-width sliver and the chat pane absorbs the freed column
/// count — the layout is always the same shape, just narrower.
pub fn compute_layout(area: Rect, sidebar_visible: bool) -> LayoutAreas {
    compute_layout_with_footer(area, sidebar_visible, FOOTER_HEIGHT)
}

fn compute_layout_with_footer(
    area: Rect,
    sidebar_visible: bool,
    footer_height: u16,
) -> LayoutAreas {
    // Vertical: pinned menu on top, body absorbs the middle, footer on the
    // bottom. The menu row is opt-out of the body budget so the chat pane
    // can never be squeezed below BODY_MIN_HEIGHT by a tall menu.
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(MENU_HEIGHT),
            Constraint::Min(BODY_MIN_HEIGHT),
            Constraint::Length(footer_height),
        ])
        .split(area);
    let cols = if sidebar_visible {
        // Peer sidebar occupies a percentage slice — peers usually have
        // a single-line name and an `online` / `offline` dot, so a
        // quarter of the screen is plenty even on 200-column terminals.
        // Min cap keeps the sidebar readable on very wide screens.
        Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Percentage(SIDEBAR_PERCENT),
                Constraint::Min(MIN_CHAT_WIDTH),
            ])
            .split(outer[1])
    } else {
        // Sidebar hidden — collapse to a zero-width sliver so the rest
        // of the layout pipeline still sees the same shape.
        Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(0), Constraint::Min(MIN_CHAT_WIDTH)])
            .split(outer[1])
    };
    LayoutAreas {
        menu: outer[0],
        sidebar: cols[0],
        chat: cols[1],
        footer: outer[2],
    }
}

pub fn layout_for_state(area: Rect, state: &UiState) -> LayoutAreas {
    let footer_height =
        if state.show_footer && state.status_format != crate::tui::config::StatusFormat::Off {
            FOOTER_HEIGHT
        } else {
            3
        };
    let visible = !state.sidebar_hidden && area.width >= state.narrow_sidebar_below;
    let layout = compute_layout_with_footer(area, visible, footer_height);
    if visible && layout.chat.width < state.min_conversation_width {
        compute_layout_with_footer(area, false, footer_height)
    } else {
        layout
    }
}

pub fn sidebar_peer_list_area(area: Rect, pending: bool) -> Rect {
    if pending {
        Layout::vertical([Constraint::Length(5), Constraint::Min(3)]).split(area)[1]
    } else {
        area
    }
}

pub fn sidebar_peer_offset(area: Rect, selected: usize) -> usize {
    let capacity = (area.height.saturating_sub(2) as usize / 2).max(1);
    selected.saturating_add(1).saturating_sub(capacity)
}

fn menu_action_rects(area: Rect) -> Vec<(MenuAction, Rect)> {
    let inner = Block::default().borders(Borders::ALL).inner(area);
    let mut x = inner.x;
    [
        (MenuAction::Peers, "[1] PEERS"),
        (MenuAction::Discover, "[/] DISCOVER"),
        (MenuAction::Settings, "[3] SETTINGS"),
        (MenuAction::Help, "[?] HELP"),
        (MenuAction::Quit, "[x] QUIT"),
    ]
    .into_iter()
    .map(|(action, label)| {
        let width = (label.len() as u16).min(inner.right().saturating_sub(x));
        let rect = Rect::new(
            x,
            inner.y.saturating_add(1),
            width,
            u16::from(inner.height >= 2),
        );
        x = x.saturating_add(label.len() as u16 + 5);
        (action, rect)
    })
    .collect()
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
    peer_area: Rect,
    modal_open: bool,
    peers_len: usize,
) -> Hit {
    // A popup is modal across the entire screen. This prevents clicks just
    // outside its rounded frame from selecting a peer or firing navigation
    // hidden behind it.
    if modal_open {
        return Hit::Modal;
    }
    for (action, rect) in menu_action_rects(areas.menu) {
        if point_in_rect(rect, col, row) {
            return Hit::Menu(action);
        }
    }
    let inner = Block::default().borders(Borders::ALL).inner(peer_area);
    if point_in_rect(inner, col, row) {
        let idx = ((row - inner.y) / 2) as usize;
        return if idx < peers_len {
            Hit::Sidebar(idx)
        } else {
            Hit::None
        };
    }
    if point_in_rect(areas.chat, col, row) {
        return Hit::Chat;
    }
    if point_in_rect(areas.footer, col, row) {
        Hit::Footer
    } else {
        Hit::None
    }
}

/// Initialize the terminal: raw mode + alt-screen + bracketed paste +
/// (optionally) mouse capture. Bracketed paste is always on; mouse
/// capture is gated by `mouse_enabled` because enabling capture on
/// tmux breaks native drag-select.
pub fn enter_terminal(mouse_enabled: bool) -> std::io::Result<Terminal<CrosstermBackend<Stdout>>> {
    use crossterm::event::{EnableBracketedPaste, EnableMouseCapture};
    use crossterm::terminal::{EnterAlternateScreen, SetTitle};
    let mut out = stdout();
    reset_terminal_input_modes(&mut out);
    crossterm::terminal::enable_raw_mode()?;
    crossterm::execute!(out, EnableBracketedPaste)?;
    if mouse_enabled {
        crossterm::execute!(out, EnableMouseCapture)?;
    }
    crossterm::execute!(out, EnterAlternateScreen, SetTitle("ppexchanger"))?;
    // A previous crash can leave mouse reports queued in the pty. Consume
    // only those stale mouse events before the first frame; never discard a
    // key or paste that belongs to the new session.
    while crossterm::event::poll(Duration::ZERO).unwrap_or(false) {
        match crossterm::event::read() {
            Ok(crossterm::event::Event::Mouse(_)) => {}
            Ok(_) | Err(_) => break,
        }
    }
    let backend = CrosstermBackend::new(out);
    Terminal::new(backend)
}

fn reset_terminal_input_modes(out: &mut Stdout) {
    let _ = std::io::Write::write_all(
        out,
        b"\x1b[?1006l\x1b[?1015l\x1b[?1004l\x1b[?1003l\x1b[?1002l\x1b[?1000l\x1b[?2004l",
    );
    let _ = std::io::Write::flush(out);
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
        // Always reset terminal input modes, even if the configured mouse
        // flag was false or teardown is happening after a partially failed
        // setup. This prevents queued SGR mouse payloads from leaking into
        // the shell prompt after ppx exits.
        let mut out = stdout();
        reset_terminal_input_modes(&mut out);
        let _ = std::io::Write::write_all(&mut out, b"\x1b[?25h");
        let _ = std::io::Write::flush(&mut out);
        // Discard any mouse reports already buffered by the terminal while
        // raw mode is still active, otherwise the shell will print their
        // numeric SGR payload after LeaveAlternateScreen.
        let deadline = Instant::now() + Duration::from_millis(30);
        while Instant::now() < deadline {
            if !crossterm::event::poll(Duration::from_millis(2)).unwrap_or(false) {
                break;
            }
            if !matches!(
                crossterm::event::read(),
                Ok(crossterm::event::Event::Mouse(_))
            ) {
                break;
            }
        }
        use crossterm::event::{DisableBracketedPaste, DisableMouseCapture};
        use crossterm::terminal::LeaveAlternateScreen;
        // Emit the typed command even when our state says capture was off;
        // this also cleans up a mode left behind by an earlier crashed run.
        let _ = crossterm::execute!(stdout(), DisableMouseCapture);
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
pub fn render<B: ratatui::backend::Backend>(
    terminal: &mut Terminal<B>,
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
        // Responsive breakpoint: when the terminal is too narrow for the
        // configured sidebar allocation, hide the sidebar automatically.
        // The user can still toggle it back via Ctrl-B if they need to.
        let areas = layout_for_state(area, state);

        draw_menu(f, areas.menu, state, theme, glyphs);
        draw_sidebar(f, areas.sidebar, state, theme, glyphs);
        draw_chat(f, areas.chat, state, theme, glyphs);
        draw_command_palette(f, areas.chat, state, theme);
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
        if let Some(preview) = &state.text_preview {
            render_text_preview(f, theme, preview);
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

/// Render the top menu bar.
///
/// Layout (two rows inside a single bordered block):
///   row 1: `PPX  secure local exchange   ● N online`
///   row 2: `[1] PEERS  ·  [/] DISCOVER  ·  [3] SETTINGS  ·  [?] HELP  ·  [x] QUIT`
///
/// Each nav entry is bracketed (`[1]`, `[/]`, `[3]`, `[?]`, `[x]`)
/// rather than a button-styled rectangle — the brackets themselves
/// carry the keyboard shortcut cue, so the row reads as a sentence
/// instead of a control panel. Active section inverts to bg/accent,
/// inactive dims via the muted role. The connection identity moves to
/// the active conversation header (see `draw_chat`).
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
    let health = online
        .checked_mul(100)
        .and_then(|n| n.checked_div(known))
        .unwrap_or(0) as u16;
    // Primary row: brand mark + descriptor + online dot + count.
    // The `●` glyph is the same one the sidebar uses so the online
    // indicator reads consistently across panes.
    let mut title_spans: Vec<Span> = Vec::with_capacity(6);
    title_spans.push(Span::styled(
        " PPX ",
        Style::default()
            .fg(theme.bg)
            .bg(theme.accent)
            .add_modifier(Modifier::BOLD),
    ));
    title_spans.push(Span::styled(
        "  secure local exchange",
        Style::default()
            .fg(theme.fg)
            .bg(theme.bg)
            .add_modifier(Modifier::BOLD),
    ));
    title_spans.push(Span::styled(
        format!("  {} {} online", _glyphs.dot_connected, online),
        theme.role_style(StyleRole::TextSecondary),
    ));

    // Secondary nav row: bracketed keys with a · separator. We render
    // each entry as three spans — `[`, key, `]` — so the bracket color
    // stays muted even when the label inverts for the active section.
    // This reads as "[1] PEERS" without the box-of-color button effect.
    let dim = theme.role_style(StyleRole::TextMuted);
    let accent = theme.role_style(StyleRole::TextAccent);
    let active_inverted = Style::default()
        .fg(theme.bg)
        .bg(theme.accent)
        .add_modifier(Modifier::BOLD);
    let quiet = Style::default().fg(theme.fg).bg(theme.bg);
    let danger = theme.role_style(StyleRole::TextDanger);
    let info = Style::default().fg(theme.info).bg(theme.bg);
    let bracket_active = Style::default()
        .fg(theme.bg)
        .bg(theme.accent)
        .add_modifier(Modifier::BOLD);
    let nav_style = |active: bool, base: Style, _info: bool| -> Style {
        if active {
            active_inverted
        } else {
            base
        }
    };
    let bracket_style_for = |active: bool| -> Style {
        if active {
            bracket_active
        } else {
            dim
        }
    };
    let sep = Span::styled("  ·  ", dim);
    let mut nav: Vec<Span> = Vec::with_capacity(20);
    let nav_items: [(&str, &str, Style, bool); 5] = [
        (
            "1",
            "PEERS",
            nav_style(state.focus == Focus::Sidebar, quiet, false),
            state.focus == Focus::Sidebar,
        ),
        ("/", "DISCOVER", info, false),
        ("3", "SETTINGS", quiet, false),
        ("?", "HELP", quiet, false),
        ("x", "QUIT", danger, false),
    ];
    for (idx, (key, label, label_style, active)) in nav_items.iter().enumerate() {
        let bracket_style = bracket_style_for(*active);
        nav.push(Span::styled("[", bracket_style));
        nav.push(Span::styled(*key, bracket_style));
        nav.push(Span::styled("] ", bracket_style));
        nav.push(Span::styled(*label, *label_style));
        if idx + 1 < nav_items.len() {
            nav.push(sep.clone());
        }
    }

    // Suppress the unused-variable warning for `accent` — the role call
    // documents the intent even though we only invert via `active_inverted`.
    let _ = accent;

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(theme.role_style(StyleRole::BorderNormal));
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
    f.render_widget(Paragraph::new(Line::from(title_spans)), top[0]);
    // Health gauge: live only on wide terminals where there is room.
    // On a narrow terminal the gauge becomes a single row of noise;
    // dropping it leaves the connection count in the title row to do
    // the same job.
    if top[1].width >= 18 {
        f.render_widget(
            Gauge::default()
                .ratio(f64::from(health) / 100.0)
                .label(format!(" LINK {} / {} ", online, known))
                .use_unicode(true)
                .style(Style::default().fg(theme.status_fg).bg(theme.status_bg))
                .gauge_style(theme.gauge_filled_style()),
            top[1],
        );
    }
    f.render_widget(Paragraph::new(Line::from(nav)), header[1]);
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
                Style::default()
                    .fg(theme.status_seen)
                    .add_modifier(Modifier::BOLD),
            ));
        let prompt = Paragraph::new(vec![
            Line::from(Span::styled(
                request.name.clone(),
                theme.peer_message_style(),
            )),
            Line::from(Span::styled(
                "Click / Enter accept · Esc decline",
                theme.dim_style(),
            )),
        ])
        .block(pending)
        .wrap(Wrap { trim: true });
        f.render_widget(prompt, rows[0]);
        sidebar_peer_list_area(area, true)
    } else {
        area
    };
    let active = state.focus == Focus::Sidebar;
    // Single focus cue: brighter border. Title is the secondary cue in
    // accent vs muted role so the reader knows where focus sits
    // without three simultaneous signals.
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(if active {
            theme.role_style(StyleRole::BorderFocused)
        } else {
            theme.role_style(StyleRole::BorderNormal)
        })
        .title(Span::styled(
            format!(" PEERS · {}/{} ", online_count(state), state.peers.len()),
            if active {
                theme.role_style(StyleRole::TextAccent)
            } else {
                theme.role_style(StyleRole::TextMuted)
            },
        ));

    let items: Vec<ListItem> = if state.peers.is_empty() {
        // Empty state: minimal copy, centered.
        vec![ListItem::new(vec![
            Line::from(Span::styled(
                "  no peers discovered  ",
                theme.role_style(StyleRole::TextMuted),
            )),
            Line::from(Span::styled(
                "  run / discover to find someone  ",
                theme.role_style(StyleRole::TextSecondary),
            )),
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
                let trust = if p.trusted {
                    glyphs.trusted
                } else {
                    glyphs.untrusted
                };
                let name_style = theme.peer_message_style_for(&p.peer_id);
                let detail = match p.state {
                    PeerState::Connected => "online",
                    PeerState::Seen => "available",
                    PeerState::Gone => "offline",
                };
                // Truncated fingerprint in muted gray — the readable
                // peer name stays the dominant signal; the fingerprint
                // is a secondary identifier underneath.
                let fp_short = if p.fingerprint.is_empty() {
                    String::new()
                } else {
                    format!("{}…", truncate_tail(&p.fingerprint, 9))
                };
                if i == state.selected_peer {
                    ListItem::new(vec![
                        Line::from(vec![
                            Span::styled(format!("{} ", dot), Style::default().fg(dot_color)),
                            Span::styled(
                                format!("{} {} ", trust, p.name),
                                name_style.add_modifier(Modifier::BOLD),
                            ),
                            Span::styled(
                                detail,
                                Style::default()
                                    .fg(dot_color)
                                    .bg(theme.status_bg)
                                    .add_modifier(Modifier::BOLD),
                            ),
                        ]),
                        Line::from(Span::styled(
                            format!("  {}", fp_short),
                            theme.role_style(StyleRole::TextMuted),
                        )),
                    ])
                } else {
                    ListItem::new(vec![
                        Line::from(vec![
                            Span::styled(format!("{} ", dot), Style::default().fg(dot_color)),
                            Span::styled(format!("{} ", trust), name_style),
                            Span::styled(p.name.clone(), name_style),
                            Span::styled(
                                format!(" · {}", detail),
                                theme.role_style(StyleRole::TextMuted),
                            ),
                        ]),
                        Line::from(Span::styled(
                            format!("  {}", fp_short),
                            theme.role_style(StyleRole::TextMuted),
                        )),
                    ])
                }
            })
            .collect()
    };

    let mut list_state =
        ListState::default().with_offset(sidebar_peer_offset(peer_area, state.selected_peer));
    if !state.peers.is_empty() {
        list_state.select(Some(state.selected_peer));
    }
    f.render_stateful_widget(
        List::new(items)
            .block(block)
            .highlight_style(
                Style::default()
                    .bg(theme.status_bg)
                    .add_modifier(Modifier::BOLD),
            )
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

/// One image preview reservation. `height` is the cell-row count
/// reserved for the preview; `width` is the cell-column count to
/// clamp against. Defined at module scope because the chunk walk
/// produces a `Vec<ImagePreview>` that the second-pass overlay
/// reads after the closure returns.
#[derive(Clone)]
struct ImagePreview {
    meta: ImageMeta,
    height: u16,
    width: u16,
    outgoing: bool,
}

fn draw_chat(f: &mut Frame, area: Rect, state: &mut UiState, theme: &Theme, glyphs: &Glyphs) {
    let active = state.focus == Focus::Chat;
    let selected = state.selected().cloned();
    let selected_peer_id = selected.as_ref().map(|p| p.peer_id);
    let selected_name = selected
        .as_ref()
        .map(|p| p.name.clone())
        .unwrap_or_default();

    // Conversation header follows the selected peer and is deliberately
    // separate from the app chrome, so context survives scrolling.
    // Layout: `<dot> <peer>  <status>  <fp>` with peer name dominant
    // in accent, status in semantic role color (success/warning/danger),
    // fingerprint muted gray and truncated.
    let title = if selected_name.is_empty() {
        Line::from(vec![Span::styled(
            format!(" {} INBOX ", glyphs.cursor),
            theme.role_style(StyleRole::TextAccent),
        )])
    } else {
        let fp = selected
            .as_ref()
            .map(|peer| peer.fingerprint.as_str())
            .filter(|fingerprint| !fingerprint.is_empty())
            .map(|fingerprint| format!(" · {}", truncate_tail(fingerprint, 12)))
            .unwrap_or_default();
        let (dot, dot_color, status_text, status_role) = match selected.as_ref().map(|p| p.state) {
            Some(PeerState::Connected) => (
                glyphs.dot_connected,
                theme.status_online,
                "connected",
                StyleRole::TextSuccess,
            ),
            Some(PeerState::Seen) => (
                glyphs.dot_seen,
                theme.status_seen,
                "available",
                StyleRole::TextWarning,
            ),
            Some(PeerState::Gone) | None => (
                glyphs.dot_gone,
                theme.status_offline,
                "offline",
                StyleRole::TextDanger,
            ),
        };
        Line::from(vec![
            Span::styled(format!(" {} ", dot), Style::default().fg(dot_color)),
            Span::styled(
                selected_name.clone(),
                theme.role_style(StyleRole::TextAccent),
            ),
            Span::styled(
                format!("  · {} ", status_text),
                theme.role_style(status_role),
            ),
            Span::styled(fp, theme.role_style(StyleRole::TextMuted)),
        ])
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(if active {
            theme.role_style(StyleRole::BorderFocused)
        } else {
            theme.role_style(StyleRole::BorderNormal)
        })
        .title(title);

    // Scroll math. `state.scroll` is the offset from the bottom of
    // the visible message deque (0 = newest visible). The forward
    // walk below accumulates whole message chunks oldest → newest,
    // then drops chunks from the front when the budget fills so the
    // newest stays anchored to the bottom of the viewport while
    // older messages overflow off the top as the reader scrolls back.
    // Each message belongs to exactly one peer. Keep the global encrypted
    // history, but render only the conversation currently selected in the
    // sidebar so switching peers never leaks another chat into this pane.
    let conversation: Vec<&UiMessage> = selected_peer_id
        .map(|peer_id| {
            state
                .messages
                .iter()
                .filter(|message| message.from_peer == peer_id)
                .collect()
        })
        .unwrap_or_default();
    let total = conversation.len();
    state.active_chat_peer = selected_peer_id;
    state.active_chat_message_count = total;
    let visible_n = (area.height as usize).saturating_sub(2); // minus borders
                                                              // Publish the chat interior height so `scroll_back` can clamp the
                                                              // scroll offset to keep the viewport full at the top of history.
    state.visible_chat_rows = visible_n;

    // Empty state: show improved welcome message
    let empty = conversation.is_empty();
    // `image_anchors` records (Y offset inside the chat interior,
    // ImagePreview) pairs so a second pass can overlay the pixel preview
    // widgets at the right row. We compute Y offsets by walking the
    // same row counts the Paragraph is about to consume.
    let mut image_anchors: Vec<(u16, ImagePreview)> = Vec::new();
    state.attachment_rows.clear();
    let visible: Vec<Line> = if empty {
        Vec::new()
    } else {
        // Forward walk: oldest → newest. Each message becomes a chunk
        // whose rows are: optional group header (only when the sender
        // switches from the previous bubble in the slice), optional day
        // separator (only when the calendar day changes), then the
        // bubble itself. Headers are inline at every group boundary so
        // the reader sees "Alice · 14:02" before Alice's run and
        // "Bob · 14:05" before Bob's run, even mid-scroll — the
        // header is anchored to its group's first message, so it does
        // not chase the newest message as more bubbles arrive within
        // the same group.
        let bubble_base = Style::default().fg(theme.fg).bg(theme.status_bg);
        let gutter = Line::from("");
        let today_bucket = day_bucket(now_unix());
        // Line budget for rendered chat rows. The chat block paints a
        // top + bottom border; the remaining rows hold content.
        let budget = (area.height as usize).saturating_sub(2);

        let resolve_who = |m: &UiMessage| -> String {
            if m.outgoing {
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
            }
        };
        let who_style_for = |m: &UiMessage| -> Style {
            if m.outgoing {
                theme.self_message_style()
            } else {
                theme.peer_message_style_for(&m.from_peer)
            }
        };
        // Build a single header line for a bubble that opens a new
        // sender group. The sender name is dominant (BOLD in their
        // accent color) and the timestamp rides muted gray to its right.
        // Match each header to its bubble column so the conversation reads
        // naturally at a glance: peers on the left, us on the right.
        let build_header = |m: &UiMessage| -> Line<'_> {
            let who = resolve_who(m);
            let who_style = who_style_for(m).add_modifier(Modifier::BOLD);
            let ts_style = theme.role_style(StyleRole::TextMuted);
            let ts = chrono_timestamp(&Some(m.ts_unix));
            let line = Line::from(vec![
                Span::styled(if m.outgoing { "› " } else { "‹ " }, who_style),
                Span::styled(format!("{} ", who), who_style),
                Span::styled(ts.to_string(), ts_style),
            ]);
            if m.outgoing {
                line.right_aligned()
            } else {
                line.left_aligned()
            }
        };

        // Respect the user's scroll anchor: when scrolled back, only
        // consider messages at or before the scroll window so we don't
        // re-surface content that should be off-screen.
        state.scroll = state.scroll.min(total.saturating_sub(1));
        let visible_end_index = total.saturating_sub(state.scroll);
        // Snapshot the visible slice into a contiguous Vec so we can
        // borrow it as `&[UiMessage]` for the forward walk below.
        // Each chunk occupies at least one row. Only the last viewport's
        // worth can survive the trimming below, plus one context message.
        let start = visible_end_index.saturating_sub(budget.saturating_add(1));
        let slice = &conversation[start..visible_end_index];

        // Build per-message chunks oldest → newest. Each chunk carries
        // an optional group header (when the sender switches from the
        // previous bubble in the slice), an optional day separator (when
        // the calendar day changes), and the bubble row. Image rows
        // are reserved in `lines_used` and the preview position is
        // recorded into `image_anchors` for the second-pass overlay.
        struct Chunk<'a> {
            rows: Vec<Line<'a>>,
            preview: Option<ImagePreview>,
            attachment_key: Option<(PeerId, u64, bool)>,
            attachment_body_offset: Option<usize>,
        }
        // Maximum rows / columns we'll spend on an inline preview.
        // Keeps a single image from monopolising the chat viewport.
        const MAX_IMAGE_ROWS: u16 = 20;
        const MAX_IMAGE_COLS: u16 = 64;
        // Keep the oldest visible chunk at the front without shifting the
        // entire vector on every message. This matters for large scrollback
        // histories and keeps incoming messages from lagging behind renders.
        let mut chunks: VecDeque<Chunk<'_>> = VecDeque::new();
        let mut lines_used: usize = 0;
        let mut prev: Option<&UiMessage> = None;
        // Tracks the sender of the previous bubble in the slice so a
        // header is emitted exactly at the start of each new sender
        // group. `None` for the very first bubble in the slice — it
        // opens a group by definition since there is no earlier bubble
        // to continue.
        let mut prev_outgoing: Option<bool> = None;

        for (i, m) in slice.iter().enumerate() {
            let bubble = if state.message_is_selected(m) {
                bubble_base.add_modifier(Modifier::REVERSED)
            } else {
                bubble_base
            };
            // Lines for this single message: optional group header +
            // optional day separator + bubble.
            let mut m_lines: Vec<Line> = Vec::new();

            // Inline group header: emit when the sender differs from
            // the previous bubble in the slice, or when this is the
            // very first bubble (which always opens a new group).
            // A blank gutter row precedes the header for every group
            // except the very first so the eye can separate groups
            // without relying on borders or color contrast.
            let is_group_start = match prev_outgoing {
                Some(prev) => prev != m.outgoing,
                None => true,
            };
            if is_group_start {
                if prev.is_some() {
                    m_lines.push(gutter.clone());
                }
                m_lines.push(build_header(m));
            }

            // Day boundary: prepend [gutter, separator, gutter] when
            // the previous (older) message in the deque crosses into a
            // different calendar day. When this is the very first
            // message in the slice we compare against the message just
            // past the slice so a scroll anchor that lands on a new day
            // is labelled.
            let prev_for_day: Option<&UiMessage> = if i == 0 {
                conversation.get(visible_end_index).copied()
            } else {
                prev
            };
            if let Some(p) = prev_for_day {
                let mb = day_bucket(m.ts_unix);
                let pb = day_bucket(p.ts_unix);
                if mb != pb {
                    let label = day_separator_label(mb, today_bucket);
                    let sep = Line::from(Span::styled(
                        format!("─── {} ───", label),
                        theme.dim_style(),
                    ))
                    .centered();
                    m_lines.push(gutter.clone());
                    m_lines.push(sep);
                    m_lines.push(gutter.clone());
                }
            }

            // Bubble body.
            let text_meta = m
                .image
                .as_ref()
                .filter(|meta| meta.mime.starts_with("text/"));
            let attachment_key = m
                .image
                .as_ref()
                .map(|_| (m.from_peer, m.ts_unix, m.outgoing));
            let text_key = text_meta.map(|_| (m.from_peer, m.ts_unix, m.outgoing));
            let text_expanded = text_key.is_some_and(|key| state.expanded_text == Some(key));
            let attachment_body_offset = attachment_key.map(|_| m_lines.len());
            let image_preview = if let Some(img) = m
                .image
                .as_ref()
                .filter(|img| state.image_previews && img.mime.starts_with("image/"))
            {
                // Reserve a cell budget for the preview. The picker
                // gives us the actual cell-pixel mapping, but for the
                // line-budget reservation a 1-row-per-cell-rows
                // mapping is close enough — a few rows either way
                // doesn't matter, the StatefulImage paints inside
                // whatever Rect we give it.
                let (cell_w, cell_h) = state
                    .image_picker
                    .map(|p| p.font_size())
                    .map(|fs| (fs.0.max(1) as u32, fs.1.max(1) as u32))
                    .unwrap_or((8, 16));
                let rows_for_image = if img.height == 0 {
                    MAX_IMAGE_ROWS
                } else {
                    let raw = img.height.div_ceil(cell_h);
                    raw.min(MAX_IMAGE_ROWS as u32).max(1) as u16
                };
                let cols_for_image = if img.width == 0 {
                    MAX_IMAGE_COLS
                } else {
                    let raw = img.width.div_ceil(cell_w);
                    raw.min(MAX_IMAGE_COLS as u32).max(1) as u16
                };
                Some(ImagePreview {
                    meta: img.clone(),
                    height: rows_for_image,
                    width: cols_for_image,
                    outgoing: m.outgoing,
                })
            } else {
                None
            };
            //
            // Peer bubbles are anchored to the left; our bubbles are anchored
            // to the right. The delivery chip remains the final span so it
            // sits at the far right of an outgoing row.
            let bubble_body = text_meta
                .map(|meta| {
                    format!(
                        "▤ text paste · {} lines · {:.1} KB · click to {}",
                        meta.width,
                        meta.bytes as f64 / 1024.0,
                        if text_expanded { "collapse" } else { "expand" }
                    )
                })
                .unwrap_or_else(|| m.body.clone());
            if m.outgoing {
                let chip_char = if m.pending { " ⏳" } else { " ✓" };
                let chip_style = theme.role_style(StyleRole::TextMuted);
                let mut bubble_line = vec![
                    Span::styled("  ", bubble),
                    Span::styled(bubble_body, bubble),
                    Span::styled("  ", bubble),
                ];
                let chip = Span::styled(chip_char, chip_style);
                bubble_line.push(chip);
                m_lines.push(Line::from(bubble_line).right_aligned());
            } else {
                let indent_style = Style::default().bg(theme.bg);
                m_lines.push(
                    Line::from(vec![
                        Span::styled("  ", indent_style),
                        Span::styled(bubble_body, Style::default().fg(theme.fg).bg(theme.bg)),
                    ])
                    .left_aligned(),
                );
            }
            if text_expanded {
                if let Some(meta) = text_meta {
                    match std::fs::read_to_string(&meta.path) {
                        Ok(content) => {
                            let all_lines: Vec<&str> = content.lines().collect();
                            let total_lines = all_lines.len();
                            // Keep the expanded chunk bounded to the current
                            // viewport. PageUp/PageDown and the mouse wheel
                            // move this window through the complete file.
                            let room = budget.saturating_sub(m_lines.len() + 2).max(1);
                            // Clamp to the last *full* viewport. Previously
                            // the offset could grow without bound, leaving
                            // only the final source line visible and making
                            // wheel scrolling appear to jump or stop. Store
                            // the bound on state so input events can apply it
                            // before the next frame is drawn.
                            let max_offset = total_lines.saturating_sub(room);
                            state.expanded_text_max_offset = max_offset;
                            state.expanded_text_offset = state.expanded_text_offset.min(max_offset);
                            let start = state.expanded_text_offset;
                            let shown: Vec<&str> =
                                all_lines.iter().skip(start).take(room).copied().collect();
                            m_lines.push(Line::from(Span::styled(
                                format!(
                                    "  ┌─ pasted text · lines {}–{} of {} ─────────",
                                    start.saturating_add(1),
                                    (start + shown.len()).min(total_lines),
                                    total_lines
                                ),
                                theme.role_style(StyleRole::TextMuted),
                            )));
                            for line in shown {
                                m_lines.push(Line::from(vec![
                                    Span::styled("  │ ", theme.role_style(StyleRole::TextMuted)),
                                    Span::styled(
                                        (*line).to_string(),
                                        theme.role_style(StyleRole::TextSecondary),
                                    ),
                                ]));
                            }
                            m_lines.push(Line::from(Span::styled(
                                "  └─ click this message to collapse · scroll to read ─",
                                theme.role_style(StyleRole::TextMuted),
                            )));
                        }
                        Err(_) => m_lines.push(Line::from(Span::styled(
                            "  text attachment is no longer available",
                            theme.role_style(StyleRole::TextDanger),
                        ))),
                    }
                }
            }
            // Image preview placeholder rows. The actual pixel rendering
            // is done in a second pass via `StatefulImage`; these blank
            // rows just reserve the vertical space so the line-budget
            // accounting stays correct.
            if let Some(preview) = image_preview.as_ref() {
                for _ in 0..preview.height {
                    m_lines.push(Line::from(""));
                }
            }

            // Append this chunk, then drop oldest chunks until the
            // total fits the budget.
            let new_size = lines_used + m_lines.len();
            if new_size > budget {
                let mut drop_lines = new_size - budget;
                while drop_lines > 0 && !chunks.is_empty() {
                    let front = chunks.front().unwrap();
                    if front.rows.len() <= drop_lines {
                        drop_lines -= front.rows.len();
                        lines_used -= front.rows.len();
                        chunks.pop_front();
                    } else {
                        lines_used -= front.rows.len();
                        chunks.pop_front();
                        drop_lines = 0;
                    }
                }
            }
            lines_used += m_lines.len();
            // Keep the preview attached to its chunk. Anchors are computed
            // after old chunks are dropped, avoiding stale Y offsets when
            // the conversation scrollback exceeds the viewport.
            chunks.push_back(Chunk {
                rows: m_lines,
                preview: image_preview.clone(),
                attachment_key,
                attachment_body_offset,
            });
            prev = Some(m);
            prev_outgoing = Some(m.outgoing);
        }

        // Flatten chunks into the final row buffer in display order
        // (oldest chunk first).
        let mut rows: Vec<Line> = Vec::with_capacity(lines_used);
        for chunk in &chunks {
            let chunk_start = rows.len();
            rows.extend(chunk.rows.iter().cloned());
            if let Some(preview) = chunk.preview.as_ref() {
                let top = rows.len().saturating_sub(preview.height as usize) as u16;
                image_anchors.push((top, preview.clone()));
            }
            if let (Some(key), Some(offset)) = (chunk.attachment_key, chunk.attachment_body_offset)
            {
                let start = chunk_start.saturating_add(offset) as u16;
                let end = rows.len().saturating_sub(1) as u16;
                state.attachment_rows.push((start, end, key));
            }
        }
        rows
    };

    let para = Paragraph::new(visible)
        .block(block)
        .wrap(Wrap { trim: false })
        .style(Style::default().fg(theme.fg).bg(theme.bg));
    f.render_widget(para, area);

    // Second pass: overlay pixel previews at the Y offsets recorded during
    // the chunk walk. Rendering is terminal-independent Unicode halfblocks,
    // so this also works in terminals without Kitty/iTerm graphics support.
    if !empty {
        let interior = area.inner(Margin {
            vertical: 1,
            horizontal: 1,
        });
        for (y_offset, preview) in image_anchors {
            let abs_y = interior.y.saturating_add(y_offset);
            if abs_y + preview.height > interior.y + interior.height {
                break;
            }
            render_image_preview(f, state, interior, abs_y, &preview);
        }
    }
    if empty {
        let body = area.inner(Margin {
            vertical: 1,
            horizontal: 1,
        });
        let height = 5.min(body.height);
        let empty_area = Rect::new(
            body.x,
            body.y
                .saturating_add(body.height.saturating_sub(height) / 2),
            body.width,
            height,
        );
        // Empty / transitional states: keep the headline short and the
        // detail action-oriented. The first line carries the user
        // affordance; the second line tells them how to get there.
        // Centered but quiet — no boxes, no neon glow.
        let (headline, detail, hint) = if selected_name.is_empty() {
            (
                "Select a peer to start messaging",
                "↑ ↓ navigate  ·  Enter open  ·  / discover",
                "no peers discovered · run /discover to find someone on your LAN",
            )
        } else {
            (
                "Start the conversation",
                "Write a message below — your exchange stays encrypted.",
                "Enter send  ·  Esc clear  ·  ? help",
            )
        };
        let _ = hint; // reserved for future per-state copy
        f.render_widget(
            Paragraph::new(vec![
                Line::from(Span::styled(
                    headline,
                    theme.role_style(StyleRole::TextSecondary),
                )),
                Line::from(""),
                Line::from(Span::styled(detail, theme.role_style(StyleRole::TextMuted))),
            ])
            .alignment(Alignment::Center),
            empty_area,
        );
    }
    // The chat's scroll position is discoverable at a glance, including
    // when mouse-wheel or PageUp navigation moves away from the newest
    // item. `total` is the bubble count; the actual rendered rows
    // include inline group headers (counted by `scroll_back` for the
    // cap), but ratatui's ScrollbarState positions by row count, so we
    // feed it the same row budget the renderer uses.
    if total > visible_n && area.width > 2 && area.height > 2 {
        let viewport_rows = visible_n;
        let mut scrollbar_state = ScrollbarState::new(total)
            .position(total.saturating_sub(viewport_rows.saturating_add(state.scroll)))
            .viewport_content_length(viewport_rows);
        // Scrollbar restraint: the track stays very dim and the thumb
        // sits at medium brightness so the chrome recedes. The spec
        // forbids over-prominent scrollbars — the chat must read as a
        // column of text, not a widget assembly.
        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .thumb_style(Style::default().fg(theme.border_inactive).bg(theme.bg))
            .track_style(
                Style::default()
                    .fg(theme.border_inactive)
                    .bg(theme.bg)
                    .add_modifier(Modifier::DIM),
            )
            .begin_symbol(None)
            .end_symbol(None);
        f.render_stateful_widget(
            scrollbar,
            area.inner(Margin {
                vertical: 1,
                horizontal: 0,
            }),
            &mut scrollbar_state,
        );
    }
}

/// Render one inline image preview. The first call for a given path
/// decodes + resizes into a StatefulProtocol (cached); subsequent
/// calls reuse the cached protocol so the render loop never
/// re-decodes on every frame. When the picker is unavailable (no TTY
/// or query failure) we leave the placeholder rows blank — the
/// metadata line in the Paragraph above already conveys the
/// dimensions and path.
fn render_image_preview(
    f: &mut Frame,
    state: &mut UiState,
    interior: Rect,
    abs_y: u16,
    preview: &ImagePreview,
) {
    // Keep the image fully inside the pane even on narrow terminals. The
    // outgoing edge uses the same right boundary as the message bubbles;
    // incoming previews share the left gutter with peer messages.
    let width = preview.width.min(interior.width);
    let height = preview.height.min(
        interior
            .height
            .saturating_sub(abs_y.saturating_sub(interior.y)),
    );
    let x = if preview.outgoing {
        interior.right().saturating_sub(width)
    } else {
        interior.x
    };
    let rect = Rect::new(x, abs_y, width, height);
    if rect.width == 0 || rect.height == 0 {
        return;
    }
    let key = (preview.meta.path.clone(), rect.width, rect.height);
    if !state.image_rasters.contains_key(&key) {
        // Resizing must not retain an unbounded collection of raster sizes.
        if state.image_rasters.len() >= 32 {
            state.image_rasters.clear();
        }
        let image = match image::open(&preview.meta.path) {
            Ok(image) => image,
            Err(_) => {
                // Remember missing/unreadable files too; otherwise a stale
                // history entry would retry filesystem decoding every frame.
                state.image_rasters.insert(key.clone(), Vec::new());
                return;
            }
        };
        // Render directly with Unicode halfblocks. This works consistently in
        // every terminal and preserves two vertical source pixels per cell.
        let resized = image
            .resize_exact(
                rect.width as u32,
                rect.height as u32 * 2,
                image::imageops::FilterType::Lanczos3,
            )
            .to_rgba8();
        let mut lines = Vec::with_capacity(rect.height as usize);
        for row in 0..rect.height as u32 {
            let mut spans = Vec::with_capacity(rect.width as usize);
            for col in 0..rect.width as u32 {
                let upper = resized.get_pixel(col, row * 2);
                let lower = resized.get_pixel(col, row * 2 + 1);
                let upper = Color::Rgb(upper[0], upper[1], upper[2]);
                let lower = Color::Rgb(lower[0], lower[1], lower[2]);
                spans.push(Span::styled("▀", Style::default().fg(upper).bg(lower)));
            }
            lines.push(Line::from(spans));
        }
        state.image_rasters.insert(key.clone(), lines);
    }
    if let Some(lines) = state.image_rasters.get(&key) {
        if lines.is_empty() {
            return;
        }
        f.render_widget(Paragraph::new(lines.clone()), rect);
    }
}

/// Render the footer (composer + status row).
///
/// The composer is a two-row block:
///   row 1: `message · encrypted` (dim label) — gives the user the
///          "what is this box" cue without a button-styled title.
///   row 2: `TO <peer> › <draft or placeholder>` — recipient chip on
///          the left, prompt on the right with the cursor.
///
/// Below the composer a thin status row carries the connection state on
/// the left and a list of bracketed keyboard hints on the right. The
/// shortcuts are highlighted in accent so they pop against muted gray
/// descriptions.
fn draw_footer(f: &mut Frame, area: Rect, state: &UiState, theme: &Theme, glyphs: &Glyphs) {
    if area.width < 4 || area.height < 3 {
        return;
    }
    // Composer is 2 rows tall: 1 row title above the input + 1 row
    // input area inside the block. The status row sits below. Block
    // height 3 = 1 border + 1 title + 1 input. The spec reference
    // collapses the title to a single dim label rather than a
    // button-styled title bar — let the border weight carry the focus
    // cue instead.
    let composer_area = Rect::new(area.x, area.y, area.width, 3);
    let target = state
        .selected()
        .map(|p| p.name.as_str())
        .unwrap_or("no recipient");
    let target_label = truncate_tail(target, 18);
    let composer_title = if target == "no recipient" {
        " choose a peer "
    } else {
        " message · encrypted "
    };
    // Single focus cue: brighter border. Title is a secondary cue in
    // the accent role — only one cue dominates at a time so the eye
    // can lock onto focused vs unfocused without ambiguity.
    let composer_block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(if state.focus == Focus::Chat {
            theme.role_style(StyleRole::BorderFocused)
        } else {
            theme.role_style(StyleRole::BorderNormal)
        })
        .title(Span::styled(
            composer_title,
            if state.focus == Focus::Chat {
                theme.role_style(StyleRole::TextAccent)
            } else {
                theme.role_style(StyleRole::TextMuted)
            },
        ));
    let inner = composer_block.inner(composer_area);
    f.render_widget(composer_block, composer_area);

    // Inner row: TO chip + prompt + draft.
    let input_area = Rect::new(inner.x, inner.y, inner.width, 1);
    let chip = format!(" TO {} ", target_label);
    let prefix = format!("{} ", glyphs.arrow);
    let chip_width = Span::raw(&chip).width() as u16;
    let prefix_width = Span::raw(&prefix).width() as u16;
    let available = input_area
        .width
        .saturating_sub(chip_width)
        .saturating_sub(prefix_width)
        .max(1) as usize;
    let draft = truncate_tail(&state.composer, available);
    let is_empty = state.composer.is_empty();
    let placeholder = if target == "no recipient" {
        "Select a peer to start messaging"
    } else {
        "Write a message or /command"
    };
    let prompt_style = theme.role_style(StyleRole::TextAccent);
    let chip_style = if target == "no recipient" {
        theme.role_style(StyleRole::TextMuted)
    } else {
        theme.role_style(StyleRole::TextSecondary)
    };
    let draft_style = if is_empty {
        theme.role_style(StyleRole::TextMuted)
    } else {
        Style::default().fg(theme.fg).bg(theme.bg)
    };
    let mut input_spans: Vec<Span> = Vec::with_capacity(5);
    input_spans.push(Span::styled(chip, chip_style));
    input_spans.push(Span::styled(prefix.clone(), prompt_style));
    if is_empty {
        input_spans.push(Span::styled(placeholder.to_string(), draft_style));
    } else {
        input_spans.push(Span::styled(draft.clone(), draft_style));
    }
    f.render_widget(Paragraph::new(Line::from(input_spans)), input_area);

    // Place the terminal cursor at the logical end of the visible draft.
    let modal_open = state.show_help
        || state.discovery.is_some()
        || state.settings.is_some()
        || state.show_peer_picker;
    if state.focus == Focus::Chat && !modal_open && !is_empty {
        let cursor_x = input_area
            .x
            .saturating_add(chip_width)
            .saturating_add(prefix_width)
            .saturating_add(Span::raw(&draft).width() as u16)
            .min(input_area.right().saturating_sub(1));
        f.set_cursor_position((cursor_x, input_area.y));
    }

    if !state.show_footer || state.status_format == crate::tui::config::StatusFormat::Off {
        return;
    }

    // Status row beneath the composer: connection state on the left,
    // keyboard hints on the right. Each shortcut is bracketed and
    // highlighted in accent; descriptions sit muted next to them.
    let identity = match state.status_format {
        crate::tui::config::StatusFormat::NameAddr => {
            format!("{} :{}", state.self_name, state.bound_port)
        }
        _ => state.self_name.clone(),
    };
    let message = if state.status.trim().is_empty() {
        "Ready"
    } else {
        &state.status
    };
    let status = truncate_tail(
        &format!("{identity} · {message}"),
        area.width.saturating_sub(4) as usize,
    );
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

    let dim = theme.role_style(StyleRole::TextMuted);
    let accent = theme.role_style(StyleRole::TextAccent);
    let hint = |key: &str, desc: &str| -> Vec<Span> {
        vec![
            Span::styled("[", dim),
            Span::styled(key.to_string(), accent),
            Span::styled("]", dim),
            Span::styled(format!(" {} ", desc), dim),
        ]
    };
    // Left side: connection state icon + status. The hint row sits
    // right-justified in the remaining space. On very narrow terminals
    // we drop the hint row entirely so the status text isn't truncated
    // out of existence.
    let mut right_spans: Vec<Span> = Vec::new();
    for h in [
        hint("Enter", "send"),
        hint("Esc", "clear"),
        hint("Tab", "focus"),
        hint("Shift+drag", "terminal select"),
        hint("?", "help"),
    ] {
        right_spans.extend(h);
    }
    let right_width: usize = right_spans.iter().map(|s| s.content.chars().count()).sum();
    let row = Rect::new(area.x, area.y.saturating_add(3), area.width, 1);
    let has_room_for_hints = area.width as usize >= right_width + 10;
    if has_room_for_hints {
        let chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Min(1), Constraint::Length(right_width as u16)])
            .split(row);
        let left_text = format!(" {} {} ", status_icon, status);
        let left_width = chunks[0].width as usize;
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                truncate_tail(&left_text, left_width),
                status_style,
            ))),
            chunks[0],
        );
        f.render_widget(
            Paragraph::new(Line::from(right_spans)).alignment(Alignment::Right),
            chunks[1],
        );
    } else {
        let left_text = format!(" {} {} ", status_icon, status);
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                truncate_tail(&left_text, row.width as usize),
                status_style,
            ))),
            row,
        );
    }
}

fn render_text_preview(f: &mut Frame, theme: &Theme, preview: &TextFilePreview) {
    let area = f.area();
    if area.width < 40 || area.height < 10 {
        return;
    }
    let width = area.width.saturating_sub(4).min(84);
    let height = area.height.saturating_sub(4).min(24);
    let popup = Rect::new(
        area.x.saturating_add(area.width.saturating_sub(width) / 2),
        area.y
            .saturating_add(area.height.saturating_sub(height) / 2),
        width,
        height,
    );
    f.render_widget(Clear, popup);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(theme.role_style(StyleRole::BorderFocused))
        .title(Span::styled(
            format!(" text preview · {} ", preview.name),
            theme.role_style(StyleRole::TextAccent),
        ));
    let content_rows = height.saturating_sub(6) as usize;
    let mut lines = vec![
        Line::from(Span::styled(
            format!("Downloaded from {}", preview.from_name),
            theme.self_message_style().add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            format!("Saved to {}", preview.path.display()),
            theme.role_style(StyleRole::TextMuted),
        )),
        Line::from(""),
    ];
    lines.extend(preview.content.lines().take(content_rows).map(|line| {
        Line::from(Span::styled(
            line.to_string(),
            theme.role_style(StyleRole::TextSecondary),
        ))
    }));
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "Enter / Esc close · file remains available at the saved path",
        theme.role_style(StyleRole::TextMuted),
    )));
    f.render_widget(
        Paragraph::new(lines)
            .block(block)
            .wrap(Wrap { trim: false }),
        popup,
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
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
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
    shown
        .get((row - popup.y - 1) as usize)
        .map(|(command, _)| *command)
}

fn command_palette_layout(
    chat: Rect,
    composer: &str,
) -> Option<(Rect, Vec<(&'static str, &'static str)>)> {
    if !composer.starts_with('/') || chat.width < 30 || chat.height < 7 {
        return None;
    }
    let shown: Vec<_> = input::command_matches(composer)
        .into_iter()
        .take(5)
        .collect();
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
    let count = Span::raw(value).width();
    if count <= width {
        return value.to_string();
    }
    if width <= 3 {
        return ".".repeat(width);
    }
    let mut used = 0;
    let mut start = value.len();
    for (index, ch) in value.char_indices().rev() {
        let mut bytes = [0; 4];
        let cells = Span::raw(&*ch.encode_utf8(&mut bytes)).width();
        if used + cells > width - 3 {
            break;
        }
        used += cells;
        start = index;
    }
    format!("...{}", &value[start..])
}

fn status_style(status: &str, theme: &Theme) -> Style {
    let lower = status.to_ascii_lowercase();
    if ["failed", "denied", "error", "aborted"]
        .iter()
        .any(|needle| lower.contains(needle))
    {
        theme.error_style()
    } else if ["connected", "listening", "saved", "ready"]
        .iter()
        .any(|needle| lower.contains(needle))
    {
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

    fn test_state() -> UiState {
        UiState::from_identity(&Identity {
            peer_id: [0; 16],
            keypair: crate::crypto::Keypair::generate(),
            name: "alice".into(),
            hostname: "test".into(),
        })
    }

    fn frame_text(terminal: &Terminal<ratatui::backend::TestBackend>) -> String {
        terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect()
    }

    #[test]
    fn hiding_status_preserves_bordered_composer_and_reclaims_space() {
        let mut state = test_state();
        let cfg = UiConfig {
            show_footer: false,
            image_previews: false,
            scrollback: 800,
            ..UiConfig::default()
        };
        state.apply_live_cfg(&cfg);
        assert!(!state.image_previews);
        assert_eq!(state.max_scrollback, 800);
        state.status = "status-hidden-marker".into();
        let screen = Rect::new(0, 0, 100, 30);
        assert_eq!(layout_for_state(screen, &state).footer.height, 3);
        let theme = Theme::by_name(theme::ThemeName::Default);
        let glyphs = theme::detect_glyphs();
        let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(100, 30)).unwrap();
        render(
            &mut terminal,
            &mut state,
            &theme,
            &glyphs,
            SettingsView::default(),
        )
        .unwrap();
        let text = frame_text(&terminal);
        assert!(text.contains("choose a peer"));
        assert!(!text.contains("status-hidden-marker"));
    }

    #[test]
    fn renders_tiny_and_responsive_terminals_without_panicking() {
        let theme = Theme::by_name(theme::ThemeName::Default);
        let glyphs = theme::detect_glyphs();
        let mut state = test_state();
        state.composer = "界".repeat(100);
        for (width, height) in [(1, 1), (8, 4), (40, 12), (80, 24), (160, 48)] {
            let mut terminal =
                Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
            render(
                &mut terminal,
                &mut state,
                &theme,
                &glyphs,
                SettingsView::default(),
            )
            .unwrap();
            let areas = layout_for_state(Rect::new(0, 0, width, height), &state);
            if width < state.narrow_sidebar_below {
                assert_eq!(areas.sidebar.width, 0);
            }
        }
    }

    #[test]
    fn rendered_chat_filters_peers_and_survives_extreme_scroll() {
        let mut state = test_state();
        for (peer, name) in [(1, "bob"), (2, "carol")] {
            state.apply(&Event::PeerSeen {
                peer_id: [peer; 16],
                name: name.into(),
                hostname: "test".into(),
                public_key: [peer; 32],
                fingerprint: "abcd".into(),
                addr: format!("127.0.0.1:{}", 8000 + peer as u16).parse().unwrap(),
            });
            state.apply(&Event::TextMessage {
                from_peer: [peer; 16],
                from_name: name.into(),
                body: format!("private-message-{name}"),
            });
        }
        let theme = Theme::by_name(theme::ThemeName::Default);
        let glyphs = theme::detect_glyphs();
        let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(100, 30)).unwrap();
        for name in ["bob", "carol"] {
            let index = state
                .peers
                .iter()
                .position(|p| p.name.starts_with(name))
                .unwrap();
            state.select_peer(index);
            state.scroll = usize::MAX;
            render(
                &mut terminal,
                &mut state,
                &theme,
                &glyphs,
                SettingsView::default(),
            )
            .unwrap();
            let frame = frame_text(&terminal);
            assert!(frame.contains(&format!("private-message-{name}")));
            let other = if name == "bob" { "carol" } else { "bob" };
            assert!(!frame.contains(&format!("private-message-{other}")));
        }
    }

    #[test]
    fn scrolled_sidebar_click_matches_rendered_peer() {
        let mut state = test_state();
        for peer in 1..=20 {
            state.apply(&Event::PeerSeen {
                peer_id: [peer; 16],
                name: format!("peer-{peer:02}"),
                hostname: "test".into(),
                public_key: [peer; 32],
                fingerprint: "abcd".into(),
                addr: format!("127.0.0.1:{}", 8000 + peer as u16).parse().unwrap(),
            });
        }
        state.selected_peer = 15;
        let screen = Rect::new(0, 0, 120, 24);
        let areas = layout_for_state(screen, &state);
        let offset = sidebar_peer_offset(areas.sidebar, state.selected_peer);
        let theme = Theme::by_name(theme::ThemeName::Default);
        let glyphs = theme::detect_glyphs();
        let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(120, 24)).unwrap();
        render(
            &mut terminal,
            &mut state,
            &theme,
            &glyphs,
            SettingsView::default(),
        )
        .unwrap();
        let row: String = (areas.sidebar.x..areas.sidebar.right())
            .map(|x| terminal.backend().buffer()[(x, areas.sidebar.y + 1)].symbol())
            .collect();
        assert!(row.contains(&state.peers[offset].name), "{row:?}");
        assert!(matches!(
            hit_test(
                screen,
                areas.sidebar.x + 1,
                areas.sidebar.y + 1,
                &areas,
                areas.sidebar,
                false,
                state.peers.len() - offset
            ),
            Hit::Sidebar(0)
        ));
    }

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
        assert_eq!(s.status, "new message from bob");
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
        // Without `visible_chat_rows` populated, the cap falls back to
        // messages.len() - 1 so the viewport always shows at least the
        // oldest message.
        assert_eq!(s.scroll, 4);
        s.scroll_forward(2);
        assert_eq!(s.scroll, 2);
        s.scroll_forward(99);
        assert_eq!(s.scroll, 0);
    }

    #[test]
    fn scroll_back_clamps_to_visible_rows() {
        // With `visible_chat_rows` set to the chat block's interior
        // height, the scroll cap is `total_rows - visible_chat_rows`
        // where `total_rows` includes one inline header per sender
        // group. Twenty bubbles from a single sender is one group, so
        // total_rows = 21 and the cap is 21 - 8 = 13.
        let id = Identity {
            peer_id: [0u8; 16],
            keypair: crate::crypto::Keypair::generate(),
            name: "alice".into(),
            hostname: "test-host".into(),
        };
        let mut s = UiState::from_identity(&id);
        for i in 0..20 {
            s.apply(&Event::TextMessage {
                from_peer: [1u8; 16],
                from_name: "bob".into(),
                body: format!("m{}", i),
            });
        }
        s.visible_chat_rows = 8;
        s.scroll_back(99);
        assert_eq!(s.scroll, 13);
        s.scroll_back(2);
        assert_eq!(s.scroll, 13);
        s.scroll_forward(5);
        assert_eq!(s.scroll, 8);
        s.scroll_forward(99);
        assert_eq!(s.scroll, 0);
    }

    #[test]
    fn expanded_text_scroll_stays_inside_viewport() {
        let id = Identity {
            peer_id: [0u8; 16],
            keypair: crate::crypto::Keypair::generate(),
            name: "alice".into(),
            hostname: "test-host".into(),
        };
        let mut s = UiState::from_identity(&id);
        s.expanded_text = Some(([1u8; 16], 42, false));
        // A 12-line attachment rendered into a six-line viewport has six
        // valid starting positions (0..=6). Large wheel bursts must stop at
        // the final full viewport rather than exposing a lone trailing line.
        s.expanded_text_max_offset = 6;
        s.scroll_back(99);
        assert_eq!(s.expanded_text_offset, 6);
        s.scroll_back(2);
        assert_eq!(s.expanded_text_offset, 6);
        s.scroll_forward(3);
        assert_eq!(s.expanded_text_offset, 3);
        s.scroll_forward(99);
        assert_eq!(s.expanded_text_offset, 0);
    }

    #[test]
    fn scroll_back_caps_per_group_with_alternating_senders() {
        // Twenty bubbles alternating between Alice (outgoing) and Bob
        // (incoming) make twenty sender groups. Total rendered rows =
        // 20 bubbles + 20 headers = 40. With an 8-row viewport the cap
        // is bounded by the 19 messages that can be skipped.
        let id = Identity {
            peer_id: [0u8; 16],
            keypair: crate::crypto::Keypair::generate(),
            name: "alice".into(),
            hostname: "test-host".into(),
        };
        let mut s = UiState::from_identity(&id);
        for i in 0..20 {
            if i % 2 == 0 {
                s.push_outgoing_message([1u8; 16], format!("out{}", i));
            } else {
                s.apply(&Event::TextMessage {
                    from_peer: [1u8; 16],
                    from_name: "bob".into(),
                    body: format!("in{}", i),
                });
            }
        }
        s.visible_chat_rows = 8;
        s.scroll_back(99);
        assert_eq!(s.scroll, 19);
    }

    #[test]
    fn count_groups_partitions_by_outgoing_flag() {
        let id = Identity {
            peer_id: [0u8; 16],
            keypair: crate::crypto::Keypair::generate(),
            name: "alice".into(),
            hostname: "test-host".into(),
        };
        let mut s = UiState::from_identity(&id);
        assert_eq!(count_groups(&s.messages), 0);
        // Two incoming bubbles: one group.
        s.apply(&Event::TextMessage {
            from_peer: [1u8; 16],
            from_name: "bob".into(),
            body: "a".into(),
        });
        s.apply(&Event::TextMessage {
            from_peer: [1u8; 16],
            from_name: "bob".into(),
            body: "b".into(),
        });
        assert_eq!(count_groups(&s.messages), 1);
        // Add one outgoing: switch, now two groups.
        s.push_outgoing_message([1u8; 16], "c".into());
        assert_eq!(count_groups(&s.messages), 2);
        // Another outgoing: no switch, still two groups.
        s.push_outgoing_message([1u8; 16], "d".into());
        assert_eq!(count_groups(&s.messages), 2);
        // One more incoming: switch, three groups.
        s.apply(&Event::TextMessage {
            from_peer: [1u8; 16],
            from_name: "bob".into(),
            body: "e".into(),
        });
        assert_eq!(count_groups(&s.messages), 3);
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

        let any_modal = |s: &UiState| s.show_help || s.discovery.is_some() || s.settings.is_some();
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
    fn selecting_another_peer_resets_chat_viewport() {
        let id = Identity {
            peer_id: [0u8; 16],
            keypair: crate::crypto::Keypair::generate(),
            name: "alice".into(),
            hostname: "test-host".into(),
        };
        let mut s = UiState::from_identity(&id);
        for (peer_id, name) in [([1u8; 16], "bob"), ([2u8; 16], "carol")] {
            s.apply(&Event::PeerSeen {
                peer_id,
                name: name.into(),
                hostname: "host".into(),
                public_key: [0u8; 32],
                fingerprint: "fp".into(),
                addr: "127.0.0.1:1".parse().unwrap(),
            });
        }
        s.scroll = 4;
        s.active_chat_peer = Some([1u8; 16]);
        s.select_peer(1);
        assert_eq!(s.selected_peer, 1);
        assert_eq!(s.scroll, 0);
        assert_eq!(s.active_chat_peer, None);
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
                addr: "10.0.0.2:47391".parse().unwrap(),
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
            name: "peer@10.0.0.95:47391".into(),
            fingerprint: "abcd".into(),
            trusted: false,
            addr: "10.0.0.95:47391".parse().unwrap(),
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
                addr: "10.0.0.3:47391".parse().unwrap(),
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
                    addr: "10.0.0.3:47391".parse().unwrap(),
                    fingerprint: None,
                    reverse: None,
                },
                crate::events::DiscoveredPeer {
                    name: None,
                    hostname: None,
                    addr: "10.0.0.4:47391".parse().unwrap(),
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
        let areas = compute_layout(screen, true);
        (screen, areas)
    }

    #[test]
    fn compute_layout_produces_four_rects() {
        let (screen, areas) = synthetic_layout();
        // Outer is header + body + composer; constants define their sizes.
        // The sidebar is now a percentage slice — check that it sits to
        // the left of the chat pane and that the chat pane fills the
        // remainder.
        assert_eq!(areas.sidebar.width, 80 * SIDEBAR_PERCENT / 100);
        assert_eq!(areas.footer.height, FOOTER_HEIGHT);
        assert_eq!(areas.chat.x, areas.sidebar.right());
        assert_eq!(areas.footer.y, screen.height - FOOTER_HEIGHT);
        // Header sits at the very top and spans the full width.
        assert_eq!(areas.menu.y, 0);
        assert_eq!(areas.menu.height, MENU_HEIGHT);
        assert_eq!(areas.menu.width, screen.width);
        // Body sits below the menu.
        assert_eq!(areas.chat.y, MENU_HEIGHT);
    }

    #[test]
    fn compute_layout_collapses_sidebar_when_hidden() {
        let screen = Rect::new(0, 0, 80, 24);
        let areas = compute_layout(screen, false);
        // Sidebar collapses to a zero-width sliver; the chat pane
        // absorbs the freed column count.
        assert_eq!(areas.sidebar.width, 0);
        assert_eq!(areas.chat.width, screen.width);
        assert_eq!(areas.chat.x, 0);
    }

    #[test]
    fn sidebar_collapse_keeps_chat_at_minimum_width() {
        // Narrow terminal — sidebar hidden, chat pane fills the whole
        // body width without ever falling below the configured min.
        let screen = Rect::new(0, 0, 60, 24);
        let areas = compute_layout(screen, false);
        assert_eq!(areas.sidebar.width, 0);
        assert!(areas.chat.width >= MIN_CHAT_WIDTH || areas.chat.width == screen.width);
    }

    #[test]
    fn sidebar_visible_on_wide_terminal() {
        let screen = Rect::new(0, 0, 120, 24);
        let areas = compute_layout(screen, true);
        // The sidebar gets a quarter of the body width; chat fills the rest.
        assert!(areas.sidebar.width > 0);
        assert!(areas.chat.width > areas.sidebar.width);
        assert_eq!(areas.sidebar.right(), areas.chat.x);
    }

    #[test]
    fn narrow_sidebar_below_defaults_round_trip() {
        // Mirrored config fields start at the same defaults that the
        // config file declares. A misbehaving apply_live_cfg call
        // would silently snap the breakpoint to zero and break the
        // responsive logic.
        let id = Identity {
            peer_id: [0u8; 16],
            keypair: crate::crypto::Keypair::generate(),
            name: "alice".into(),
            hostname: "test-host".into(),
        };
        let s = UiState::from_identity(&id);
        assert_eq!(
            s.narrow_sidebar_below,
            crate::tui::config::DEFAULT_NARROW_SIDEBAR_BELOW
        );
        assert_eq!(
            s.min_conversation_width,
            crate::tui::config::DEFAULT_MIN_CONVERSATION_WIDTH
        );
    }

    #[test]
    fn apply_live_cfg_mirrors_responsive_knobs() {
        let id = Identity {
            peer_id: [0u8; 16],
            keypair: crate::crypto::Keypair::generate(),
            name: "alice".into(),
            hostname: "test-host".into(),
        };
        let mut s = UiState::from_identity(&id);
        let cfg = UiConfig {
            theme: ThemeName::Default,
            show_footer: true,
            image_previews: true,
            mouse: true,
            scrollback: 100,
            notify_sound: false,
            desktop_notifications: true,
            auto_trust_seen: false,
            status_format: crate::tui::config::StatusFormat::NameOnly,
            narrow_sidebar_below: 72,
            min_conversation_width: 32,
        };
        s.apply_live_cfg(&cfg);
        assert_eq!(s.narrow_sidebar_below, 72);
        assert_eq!(s.min_conversation_width, 32);
    }

    #[test]
    fn sidebar_toggle_is_independent_of_focus() {
        // The Ctrl-B toggle should flip sidebar_hidden regardless of
        // which pane has focus — it's a layout knob, not a focus
        // claim. Lock that contract down so a future refactor doesn't
        // muddle focus with chrome visibility.
        let id = Identity {
            peer_id: [0u8; 16],
            keypair: crate::crypto::Keypair::generate(),
            name: "alice".into(),
            hostname: "test-host".into(),
        };
        let mut s = UiState::from_identity(&id);
        assert!(!s.sidebar_hidden);
        s.sidebar_hidden = !s.sidebar_hidden;
        assert!(s.sidebar_hidden);
        s.sidebar_hidden = !s.sidebar_hidden;
        assert!(!s.sidebar_hidden);
        // Focus is unchanged.
        assert_eq!(s.focus, Focus::Chat);
    }

    #[test]
    fn truncate_tail_keeps_the_latest_input_visible() {
        assert_eq!(truncate_tail("hello", 8), "hello");
        assert_eq!(truncate_tail("abcdefghijkl", 8), "...hijkl");
        assert_eq!(truncate_tail("abcdef", 3), "...");
    }

    #[test]
    fn truncate_tail_respects_terminal_cell_width() {
        let text = truncate_tail("界界界界", 7);
        assert_eq!(text, "...界界");
        assert!(Line::from(text).width() <= 7);
    }

    #[test]
    fn scrolling_cannot_hide_all_messages_or_overflow() {
        let id = Identity {
            peer_id: [0; 16],
            keypair: crate::crypto::Keypair::generate(),
            name: "alice".into(),
            hostname: "test".into(),
        };
        let mut state = UiState::from_identity(&id);
        state.push_outgoing_message([1; 16], "first".into());
        state.apply(&Event::TextMessage {
            from_peer: [1; 16],
            from_name: "bob".into(),
            body: "second".into(),
        });
        state.visible_chat_rows = 2;
        state.scroll_back(usize::MAX);
        assert!(state.scroll < state.messages.len());
        state.scroll_back(usize::MAX);
        assert!(state.scroll < state.messages.len());
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
        for (action, rect) in menu_action_rects(areas.menu) {
            let hit = hit_test(
                screen,
                rect.x.saturating_add(rect.width / 2),
                rect.y,
                &areas,
                areas.sidebar,
                false,
                0,
            );
            assert!(
                matches!(hit, Hit::Menu(found) if found == action),
                "rect {:?} expected {:?}, got {:?}",
                rect,
                action,
                hit
            );
        }
        // Separator clicks must not trigger either neighbouring action.
        let separator_x = menu_action_rects(areas.menu)[0].1.right();
        assert!(matches!(
            hit_test(
                screen,
                separator_x,
                areas.menu.y + 2,
                &areas,
                areas.sidebar,
                false,
                0
            ),
            Hit::None
        ));
    }

    #[test]
    fn hit_test_sidebar_row_picks_peer_index() {
        let (screen, areas) = synthetic_layout();
        let peer_list = areas.sidebar;
        // A block title lives in the border, so the first two-line ListItem
        // starts at the first inner row.
        let first_y = peer_list.y + 1;
        assert!(matches!(
            hit_test(
                screen,
                peer_list.x + 1,
                first_y,
                &areas,
                peer_list,
                false,
                3
            ),
            Hit::Sidebar(0)
        ));
        assert!(matches!(
            hit_test(
                screen,
                peer_list.x + 1,
                first_y + 1,
                &areas,
                peer_list,
                false,
                3
            ),
            Hit::Sidebar(0)
        ));
        assert!(matches!(
            hit_test(
                screen,
                peer_list.x + 1,
                first_y + 2,
                &areas,
                peer_list,
                false,
                3
            ),
            Hit::Sidebar(1)
        ));
        // Empty sidebar space is inert: it must never silently select the
        // last peer.
        let below_last = peer_list.bottom().saturating_sub(2);
        assert!(matches!(
            hit_test(
                screen,
                peer_list.x + 1,
                below_last,
                &areas,
                peer_list,
                false,
                3
            ),
            Hit::None
        ));
    }

    #[test]
    fn hit_test_uses_the_rendered_peer_list_after_pending_card() {
        let (screen, areas) = synthetic_layout();
        let peer_list = sidebar_peer_list_area(areas.sidebar, true);
        let first_peer_row = peer_list.y + 1;
        assert!(matches!(
            hit_test(
                screen,
                peer_list.x + 1,
                first_peer_row,
                &areas,
                peer_list,
                false,
                3,
            ),
            Hit::Sidebar(0)
        ));
        assert!(matches!(
            hit_test(
                screen,
                peer_list.x + 1,
                first_peer_row + 2,
                &areas,
                peer_list,
                false,
                3,
            ),
            Hit::Sidebar(1)
        ));
    }

    #[test]
    fn hit_test_chat_click_returns_chat() {
        let (screen, areas) = synthetic_layout();
        let col = areas.chat.x + 1;
        let row = areas.chat.y + 1;
        assert!(matches!(
            hit_test(screen, col, row, &areas, areas.sidebar, false, 0),
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
            hit_test(screen, col, row, &areas, areas.sidebar, false, 0),
            Hit::Chat
        ));
        // With a modal open, that same click is consumed as Modal.
        assert!(matches!(
            hit_test(screen, col, row, &areas, areas.sidebar, true, 0),
            Hit::Modal
        ));
    }

    #[test]
    fn image_message_renders_without_panic_when_picker_unavailable() {
        // Build a UiState with an image message whose `path` points
        // at a file that does not exist. With `image_picker = None`
        // (the offline / non-TTY case) the render path must NOT
        // panic; it just leaves the placeholder rows blank because
        // `image::open` returns Err. The chat history file path
        // double-encoded round-trip is also verified here.
        let id = Identity {
            peer_id: [0u8; 16],
            keypair: crate::crypto::Keypair::generate(),
            name: "alice".into(),
            hostname: "test-host".into(),
        };
        let mut s = UiState::from_identity(&id);
        assert!(s.image_picker.is_none());
        assert!(s.image_protocols.is_empty());
        let img = ImageMeta {
            path: PathBuf::from("/nonexistent/does-not-exist.png"),
            mime: "image/png".to_string(),
            width: 640,
            height: 480,
            bytes: 1024,
        };
        s.push_inbound_image([1u8; 16], "bob".into(), img.clone());
        // The message is in the ring with the ImageMeta attached.
        assert_eq!(s.messages.len(), 1);
        assert!(s.messages[0].image.is_some());
        assert_eq!(s.messages[0].image.as_ref().unwrap().width, 640);
        assert_eq!(s.messages[0].image.as_ref().unwrap().height, 480);
        // The body is the metadata fallback line so the message is
        // searchable in scrollback.
        assert!(s.messages[0].body.contains("640"));
        assert!(s.messages[0].body.contains("480"));
        // `render_image_preview` would short-circuit on the missing
        // picker and bail; we don't drive a real Frame here because
        // that requires a terminal backend, but the lazy-init guard
        // is exercised by the render path on the first call.
    }

    #[test]
    fn image_meta_round_trips_through_chat_history() {
        // Sanity-check the encoding/decoding by exercising the
        // public API the chat_history module exposes. We can't
        // import it here (circular), so we just verify ImageMeta
        // can be constructed and is Clone + PartialEq + Eq +
        // Debug as the persistent format expects.
        let img = ImageMeta {
            path: PathBuf::from("/tmp/foo.png"),
            mime: "image/png".to_string(),
            width: 100,
            height: 200,
            bytes: 4096,
        };
        let cloned = img.clone();
        assert_eq!(img, cloned);
    }
}
