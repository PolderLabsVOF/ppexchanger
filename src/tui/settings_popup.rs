//! Structured settings modal for `/settings` (Ctrl-,).
//!
//! The popup is deliberately a view over `UiConfig`: changes apply live and
//! are persisted by the caller when the modal closes. Geometry helpers are
//! shared by rendering and hit testing so mouse targets cannot drift.

use crate::tui::config::{StatusFormat, UiConfig};
use crate::tui::theme::{Glyphs, Theme, ThemeName};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, Borders, Cell, Clear, Paragraph, Row, Table, TableState, Tabs, Wrap,
};
use ratatui::Frame;

const POPUP_W: u16 = 76;
const POPUP_H: u16 = 18;
const DESCRIPTION_HEIGHT: u16 = 3;
const FOOTER_HEIGHT: u16 = 1;
const CLOSE_LABEL: &str = "[ Esc close ]";

pub const THEME_CHOICES: &[ThemeName] = &[
    ThemeName::Default,
    ThemeName::Solarized,
    ThemeName::Monochrome,
    ThemeName::Neon,
    ThemeName::Amber,
];

/// A concrete, user-facing setting. The application uses this instead of
/// fragile row numbers when it applies keyboard and mouse actions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Setting {
    DisplayName,
    Theme,
    Mouse,
    Footer,
    StatusFormat,
    Scrollback,
    SidebarBreakpoint,
    MinChatWidth,
    NotifySound,
    DesktopNotifications,
    ImagePreviews,
    AutoTrust,
    Reset,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Profile,
    Appearance,
    Chat,
    Privacy,
    About,
}

impl Tab {
    pub const ALL: [Tab; 5] = [
        Tab::Profile,
        Tab::Appearance,
        Tab::Chat,
        Tab::Privacy,
        Tab::About,
    ];
    const PROFILE: &[Setting] = &[Setting::DisplayName];
    const APPEARANCE: &[Setting] = &[
        Setting::Theme,
        Setting::Footer,
        Setting::StatusFormat,
        Setting::SidebarBreakpoint,
        Setting::MinChatWidth,
    ];
    const CHAT: &[Setting] = &[
        Setting::Mouse,
        Setting::Scrollback,
        Setting::ImagePreviews,
        Setting::NotifySound,
        Setting::DesktopNotifications,
    ];
    const PRIVACY: &[Setting] = &[Setting::AutoTrust, Setting::Reset];
    const ABOUT: &[Setting] = &[];

    pub fn label(self) -> &'static str {
        match self {
            Tab::Profile => "Profile",
            Tab::Appearance => "Appearance",
            Tab::Chat => "Chat",
            Tab::Privacy => "Privacy",
            Tab::About => "About",
        }
    }

    pub fn settings(self) -> &'static [Setting] {
        match self {
            Tab::Profile => Self::PROFILE,
            Tab::Appearance => Self::APPEARANCE,
            Tab::Chat => Self::CHAT,
            Tab::Privacy => Self::PRIVACY,
            Tab::About => Self::ABOUT,
        }
    }

    pub fn idx(self) -> usize {
        Self::ALL
            .iter()
            .position(|t| *t == self)
            .unwrap_or_default()
    }
    pub fn next_tab(self) -> Self {
        Self::ALL[(self.idx() + 1) % Self::ALL.len()]
    }
    pub fn prev_tab(self) -> Self {
        Self::ALL[(self.idx() + Self::ALL.len() - 1) % Self::ALL.len()]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseTarget {
    Tab(Tab),
    Row(usize),
    Close,
}

#[derive(Debug, Clone)]
pub struct SettingsState {
    pub tab: Tab,
    pub selected: usize,
    pub theme_idx: usize,
    pub confirm_reset: bool,
    pub dirty: bool,
    pub name_draft: String,
    /// Snapshot used by Esc to cancel an inline display-name edit.
    pub name_before_edit: String,
    pub save_error: Option<String>,
    pub editing_name: bool,
    /// Kept for callers that want to expose a viewport; rendering computes a
    /// safe effective offset from the selected row on each terminal size.
    pub row_scroll: usize,
    pub extra: SettingsExtras,
}

impl SettingsState {
    pub fn new(cfg: &UiConfig) -> Self {
        Self {
            tab: Tab::Profile,
            selected: 0,
            theme_idx: THEME_CHOICES
                .iter()
                .position(|t| *t == cfg.theme)
                .unwrap_or_default(),
            confirm_reset: false,
            dirty: false,
            name_draft: String::new(),
            name_before_edit: String::new(),
            save_error: None,
            editing_name: false,
            row_scroll: 0,
            extra: SettingsExtras::default(),
        }
    }

    pub fn cycle_theme(&mut self, delta: i32) -> ThemeName {
        let next =
            ((self.theme_idx as i32 + delta).rem_euclid(THEME_CHOICES.len() as i32)) as usize;
        self.theme_idx = next;
        self.dirty = true;
        THEME_CHOICES[next]
    }
    pub fn toggle_mouse(&mut self, cfg: &mut UiConfig) {
        cfg.mouse = !cfg.mouse;
        self.dirty = true;
    }
    pub fn toggle_footer(&mut self, cfg: &mut UiConfig) {
        cfg.show_footer = !cfg.show_footer;
        self.dirty = true;
    }
    pub fn toggle_notify_sound(&mut self, cfg: &mut UiConfig) {
        cfg.notify_sound = !cfg.notify_sound;
        self.dirty = true;
    }
    pub fn toggle_desktop_notifications(&mut self, cfg: &mut UiConfig) {
        cfg.desktop_notifications = !cfg.desktop_notifications;
        self.dirty = true;
    }
    pub fn toggle_image_previews(&mut self, cfg: &mut UiConfig) {
        cfg.image_previews = !cfg.image_previews;
        self.dirty = true;
    }
    pub fn toggle_auto_trust_seen(&mut self, cfg: &mut UiConfig) {
        cfg.auto_trust_seen = !cfg.auto_trust_seen;
        self.dirty = true;
    }

    pub fn cycle_status_format(&mut self, cfg: &mut UiConfig) -> StatusFormat {
        cfg.status_format = match cfg.status_format {
            StatusFormat::NameOnly => StatusFormat::NameAddr,
            StatusFormat::NameAddr => StatusFormat::Off,
            StatusFormat::Off => StatusFormat::NameOnly,
        };
        self.dirty = true;
        cfg.status_format
    }
    pub fn reset_to_defaults(&mut self, cfg: &mut UiConfig) {
        *cfg = UiConfig::default();
        self.theme_idx = THEME_CHOICES
            .iter()
            .position(|t| *t == cfg.theme)
            .unwrap_or_default();
        self.dirty = true;
    }
    pub fn bump_scrollback(&mut self, cfg: &mut UiConfig, delta: i32) {
        let next = (cfg.scrollback as i32 + delta)
            .clamp(16, crate::tui::config::MAX_SCROLLBACK as i32) as usize;
        if next != cfg.scrollback {
            cfg.scrollback = next;
            self.dirty = true;
        }
    }
    pub fn bump_sidebar_breakpoint(&mut self, cfg: &mut UiConfig, delta: i32) {
        let next = (cfg.narrow_sidebar_below as i32 + delta).clamp(40, 240) as u16;
        if next != cfg.narrow_sidebar_below {
            cfg.narrow_sidebar_below = next;
            self.dirty = true;
        }
    }
    pub fn bump_min_chat_width(&mut self, cfg: &mut UiConfig, delta: i32) {
        let next = (cfg.min_conversation_width as i32 + delta).clamp(20, 200) as u16;
        if next != cfg.min_conversation_width {
            cfg.min_conversation_width = next;
            self.dirty = true;
        }
    }
    pub fn rows_in_tab(&self) -> usize {
        let editable = self.tab.settings().len();
        if editable == 0 {
            8
        } else {
            editable
        }
    }
    pub fn selected(&self) -> usize {
        self.selected.min(self.rows_in_tab().saturating_sub(1))
    }
    pub fn selected_setting(&self) -> Option<Setting> {
        self.tab.settings().get(self.selected()).copied()
    }
    pub fn move_selection(&mut self, delta: i32) {
        self.confirm_reset = false;
        let count = self.rows_in_tab() as i32;
        if count > 0 {
            self.selected = ((self.selected() as i32 + delta).rem_euclid(count)) as usize;
        }
    }
    pub fn switch_tab(&mut self, tab: Tab) {
        self.confirm_reset = false;
        if self.editing_name {
            self.name_draft = self.name_before_edit.clone();
            self.editing_name = false;
        }
        self.tab = tab;
        self.selected = self.selected();
        self.row_scroll = 0;
    }
}

#[derive(Debug, Clone, Copy)]
struct PopupLayout {
    popup: Rect,
    tabs: Rect,
    rows: Rect,
    description: Rect,
    footer: Rect,
}

pub fn centered(area: Rect) -> Rect {
    let width = POPUP_W.min(area.width);
    let height = POPUP_H.min(area.height);
    Rect::new(
        area.x + area.width.saturating_sub(width) / 2,
        area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    )
}

fn popup_layout(area: Rect) -> PopupLayout {
    let popup = centered(area);
    let inner = Block::default().borders(Borders::ALL).inner(popup);
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(DESCRIPTION_HEIGHT.min(inner.height)),
            Constraint::Length(FOOTER_HEIGHT.min(inner.height)),
        ])
        .split(inner);
    PopupLayout {
        popup,
        tabs: chunks[0],
        rows: chunks[1],
        description: chunks[2],
        footer: chunks[3],
    }
}

fn tab_rects(area: Rect) -> impl Iterator<Item = (Tab, Rect)> {
    let mut x = area.x;
    Tab::ALL.into_iter().filter_map(move |tab| {
        let width = tab.label().len() as u16;
        if x >= area.right() {
            return None;
        }
        let rect = Rect::new(x, area.y, width.min(area.right().saturating_sub(x)), 1);
        x = x.saturating_add(width + if area.width < 45 { 1 } else { 3 });
        Some((tab, rect))
    })
}

fn close_rect(footer: Rect) -> Rect {
    Rect::new(
        footer
            .right()
            .saturating_sub(CLOSE_LABEL.len().min(footer.width as usize) as u16),
        footer.y,
        CLOSE_LABEL.len().min(footer.width as usize) as u16,
        footer.height,
    )
}

fn visible_row_range(state: &SettingsState, viewport_rows: usize) -> (usize, usize) {
    let total = state.rows_in_tab();
    let capacity = viewport_rows.max(1);
    let selected = state.selected();
    let start = state
        .row_scroll
        .min(selected)
        .min(total.saturating_sub(capacity));
    let start = if selected >= start + capacity {
        selected + 1 - capacity
    } else {
        start
    };
    (start, (start + capacity).min(total))
}

pub fn mouse_target(area: Rect, col: u16, row: u16, state: &SettingsState) -> Option<MouseTarget> {
    let layout = popup_layout(area);
    if !layout.popup.contains((col, row).into()) {
        return None;
    }
    for (tab, rect) in tab_rects(layout.tabs) {
        if rect.contains((col, row).into()) {
            return Some(MouseTarget::Tab(tab));
        }
    }
    if close_rect(layout.footer).contains((col, row).into()) {
        return Some(MouseTarget::Close);
    }
    if layout.rows.contains((col, row).into()) {
        let (start, end) = visible_row_range(state, layout.rows.height as usize);
        let selected = start + row.saturating_sub(layout.rows.y) as usize;
        if selected < end {
            return Some(MouseTarget::Row(selected));
        }
    }
    None
}

#[allow(clippy::too_many_arguments)]
pub fn render(
    f: &mut Frame,
    theme: &Theme,
    _glyphs: &Glyphs,
    state: &SettingsState,
    cfg: &UiConfig,
    version: &str,
    fingerprint: &str,
    config_path: &str,
    received_dir: &str,
) {
    let layout = popup_layout(f.area());
    f.render_widget(Clear, layout.popup);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme.border_active))
        .title(Line::from(Span::styled(
            " Settings ",
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        )));
    let titles: Vec<Line> = Tab::ALL
        .iter()
        .map(|tab| Line::from(Span::raw(tab.label())))
        .collect();
    f.render_widget(
        Tabs::new(titles)
            .padding("", "")
            .select(state.tab.idx())
            .divider(if layout.tabs.width < 45 { " " } else { " · " })
            .style(Style::default().fg(theme.border_inactive).bg(theme.bg))
            .highlight_style(
                Style::default()
                    .fg(theme.accent)
                    .add_modifier(Modifier::BOLD),
            ),
        layout.tabs,
    );

    let rows = rows_for_tab(
        state,
        cfg,
        theme,
        version,
        fingerprint,
        config_path,
        received_dir,
    );
    let (start, end) = visible_row_range(state, layout.rows.height as usize);
    let mut table_state = TableState::default();
    table_state.select(
        (state.selected() >= start && state.selected() < end).then_some(state.selected() - start),
    );
    let table = Table::new(
        rows.into_iter().skip(start).take(end.saturating_sub(start)),
        if layout.rows.width < 60 {
            [
                Constraint::Percentage(55),
                Constraint::Percentage(45),
                Constraint::Length(0),
            ]
        } else {
            [
                Constraint::Length(23),
                Constraint::Min(12),
                Constraint::Length(16),
            ]
        },
    )
    .column_spacing(1)
    .style(Style::default().fg(theme.fg).bg(theme.bg))
    .row_highlight_style(
        Style::default()
            .fg(theme.accent)
            .add_modifier(Modifier::REVERSED),
    )
    .highlight_symbol("› ");
    f.render_stateful_widget(table, layout.rows, &mut table_state);

    let description = state
        .save_error
        .as_ref()
        .map(|error| {
            Line::from(format!(
                "Could not save: {error}. Fix the problem, then close to retry."
            ))
        })
        .unwrap_or_else(|| {
            if state.tab == Tab::About {
                Line::from(match state.selected() {
                    0 => format!("Version: {version}"),
                    1 => format!("Build target: {}", state.extra.build_target),
                    2 => format!("Fingerprint: {}", grouped_fp(fingerprint)),
                    3 => format!("Configuration: {config_path}"),
                    4 => format!("Received files: {received_dir}"),
                    5 => format!("Connected peers: {}", state.extra.connected_count),
                    6 => format!(
                        "Last seen: {}",
                        format_last_seen(state.extra.last_seen_unix)
                    ),
                    _ => format!("Display name: {}", state.name_draft),
                })
            } else {
                selected_description(state)
            }
        });
    f.render_widget(
        Paragraph::new(description)
            .style(Style::default().fg(theme.info).bg(theme.bg))
            .wrap(Wrap { trim: true }),
        layout.description,
    );
    let footer_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Min(0),
            Constraint::Length(CLOSE_LABEL.len().min(layout.footer.width as usize) as u16),
        ])
        .split(layout.footer);
    f.render_widget(
        Paragraph::new(if footer_chunks[0].width < 52 {
            "Tab sections · ↑↓ select"
        } else {
            "↑↓ select · ←→ change · Tab category · saved on close"
        })
        .style(Style::default().fg(theme.info).bg(theme.bg)),
        footer_chunks[0],
    );
    f.render_widget(
        Paragraph::new(CLOSE_LABEL).style(
            Style::default()
                .fg(theme.accent)
                .bg(theme.bg)
                .add_modifier(Modifier::BOLD),
        ),
        footer_chunks[1],
    );
    f.render_widget(block, layout.popup);
}

fn rows_for_tab(
    state: &SettingsState,
    cfg: &UiConfig,
    theme: &Theme,
    version: &str,
    fingerprint: &str,
    config_path: &str,
    received_dir: &str,
) -> Vec<Row<'static>> {
    let normal = Style::default().fg(theme.fg).bg(theme.bg);
    let value = Style::default().fg(theme.accent).bg(theme.bg);
    let row = |label: &str, value_text: String, action: &str| {
        Row::new(vec![
            Cell::from(label.to_owned()).style(normal),
            Cell::from(value_text).style(value),
            Cell::from(action.to_owned()).style(Style::default().fg(theme.info).bg(theme.bg)),
        ])
    };
    match state.tab {
        Tab::Profile => vec![row(
            "Display name",
            if state.editing_name {
                format!("{}▏", state.name_draft)
            } else {
                state.name_draft.clone()
            },
            if state.editing_name {
                "Enter save"
            } else {
                "Enter edit"
            },
        )],
        Tab::Appearance => vec![
            row(
                "Theme",
                THEME_CHOICES[state.theme_idx.min(THEME_CHOICES.len() - 1)]
                    .as_str()
                    .to_owned(),
                "←→ cycle",
            ),
            row("Status footer", on_off(cfg.show_footer), "Enter toggle"),
            row(
                "Status format",
                match cfg.status_format {
                    StatusFormat::NameOnly => "Name",
                    StatusFormat::NameAddr => "Name + port",
                    StatusFormat::Off => "Hidden",
                }
                .to_owned(),
                "←→ cycle",
            ),
            row(
                "Sidebar collapse",
                format!("{} cols", cfg.narrow_sidebar_below),
                "←→ ±5",
            ),
            row(
                "Minimum chat width",
                format!("{} cols", cfg.min_conversation_width),
                "←→ ±5",
            ),
        ],
        Tab::Chat => vec![
            row("Mouse capture", on_off(cfg.mouse), "Enter toggle"),
            row(
                "Chat history",
                format!("{} messages", cfg.scrollback),
                "←→ ±100",
            ),
            row("Image previews", on_off(cfg.image_previews), "Enter toggle"),
            row("Terminal sound", on_off(cfg.notify_sound), "Enter toggle"),
            row(
                "Desktop alerts",
                on_off(cfg.desktop_notifications),
                "Enter toggle",
            ),
        ],
        Tab::Privacy => vec![
            row(
                "Automatic trust",
                on_off(cfg.auto_trust_seen),
                "Enter toggle",
            ),
            row(
                "Reset settings",
                if state.confirm_reset {
                    "Press Y to confirm".into()
                } else {
                    "—".into()
                },
                if state.confirm_reset {
                    "Y confirm"
                } else {
                    "Enter arm"
                },
            ),
        ],
        Tab::About => vec![
            row("Version", version.to_owned(), ""),
            row("Build target", state.extra.build_target.to_owned(), ""),
            row("Fingerprint", grouped_fp(fingerprint), ""),
            row("Config file", config_path.to_owned(), ""),
            row("Received files", received_dir.to_owned(), ""),
            row(
                "Connected peers",
                state.extra.connected_count.to_string(),
                "",
            ),
            row(
                "Last seen",
                format_last_seen(state.extra.last_seen_unix),
                "",
            ),
            row("Display name", state.name_draft.clone(), ""),
        ],
    }
}
fn on_off(value: bool) -> String {
    if value { "On" } else { "Off" }.to_owned()
}

fn selected_description(state: &SettingsState) -> Line<'static> {
    let text = match state.selected_setting() {
        Some(Setting::DisplayName) => if state.editing_name { "Type your name. Enter saves this field; Esc restores its previous value." } else { "The name other people see in their peer list and chat. Press Enter to edit." },
        Some(Setting::Theme) => "Choose the color palette used by PPX.",
        Some(Setting::Mouse) => "Lets PPX receive clicks and wheel scrolling. Hold Shift while dragging to use your terminal’s native text selection.",
        Some(Setting::Footer) => "Show the compact connection/status line below the composer. The message entry box stays visible.",
        Some(Setting::StatusFormat) => "Choose whether the status footer shows your name, name and listening port, or is hidden.",
        Some(Setting::Scrollback) => "Maximum messages retained across all chats. Lower limits discard older history as new messages arrive.",
        Some(Setting::SidebarBreakpoint) => "Terminal width where PPX automatically collapses the peer sidebar to leave room for chat.",
        Some(Setting::MinChatWidth) => "Minimum chat-pane width kept while the peer sidebar is visible.",
        Some(Setting::NotifySound) => "Play the terminal bell when a new message arrives.",
        Some(Setting::DesktopNotifications) => "Show an operating-system notification when a new message arrives while PPX is open.",
        Some(Setting::ImagePreviews) => "Render received and sent images inline when the terminal supports an image protocol.",
        Some(Setting::AutoTrust) => "Warning: automatically accepts unknown peers discovered on your LAN. Leave off unless you trust that network.",
        Some(Setting::Reset) => if state.confirm_reset { "Reset is armed. Press Y to replace every UI setting with its default, or any other key to cancel." } else { "Restore all UI settings to their defaults. Press Enter, then Y to confirm." },
        None => "Build and connection information. These values are read-only.",
    };
    Line::from(Span::styled(text, Style::default()))
}

#[derive(Debug, Clone, Default)]
pub struct SettingsExtras {
    pub build_target: &'static str,
    pub connected_count: usize,
    pub last_seen_unix: u64,
}

fn grouped_fp(s: &str) -> String {
    let hex: String = s.chars().filter(|c| c.is_ascii_hexdigit()).collect();
    (0..16)
        .map(|i| {
            hex.get(i * 2..i * 2 + 2)
                .unwrap_or_else(|| {
                    if i * 2 < hex.len() {
                        &hex[i * 2..]
                    } else {
                        "00"
                    }
                })
                .to_owned()
        })
        .collect::<Vec<_>>()
        .join(":")
}
fn format_last_seen(unix: u64) -> String {
    if unix == 0 {
        return "never".into();
    }
    let secs = unix % 86_400;
    format!("{:02}:{:02}:{:02}", secs / 3600, secs / 60 % 60, secs % 60)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{backend::TestBackend, Terminal};

    #[test]
    fn categories_expose_the_requested_settings() {
        assert_eq!(
            Tab::ALL,
            [
                Tab::Profile,
                Tab::Appearance,
                Tab::Chat,
                Tab::Privacy,
                Tab::About
            ]
        );
        assert_eq!(Tab::Profile.settings(), &[Setting::DisplayName]);
        assert!(Tab::About.settings().is_empty());
    }
    #[test]
    fn selection_uses_setting_not_a_fragile_row_number() {
        let mut state = SettingsState::new(&UiConfig::default());
        state.switch_tab(Tab::Chat);
        state.selected = 2;
        assert_eq!(state.selected_setting(), Some(Setting::ImagePreviews));
    }
    #[test]
    fn mouse_targets_follow_rendered_tab_row_and_explicit_close() {
        let state = SettingsState::new(&UiConfig::default());
        let area = Rect::new(0, 0, 100, 30);
        let layout = popup_layout(area);
        assert_eq!(
            mouse_target(area, layout.tabs.x, layout.tabs.y, &state),
            Some(MouseTarget::Tab(Tab::Profile))
        );
        assert_eq!(
            mouse_target(area, layout.rows.x, layout.rows.y, &state),
            Some(MouseTarget::Row(0))
        );
        let close = close_rect(layout.footer);
        assert_eq!(
            mouse_target(area, close.x, close.y, &state),
            Some(MouseTarget::Close)
        );
        assert_eq!(
            mouse_target(area, layout.footer.x, layout.footer.y, &state),
            None
        );
    }
    #[test]
    fn small_terminal_keeps_selected_row_visible_and_clickable() {
        let mut state = SettingsState::new(&UiConfig::default());
        state.switch_tab(Tab::Appearance);
        state.selected = 4;
        let area = Rect::new(0, 0, 46, 10);
        let layout = popup_layout(area);
        let (start, end) = visible_row_range(&state, layout.rows.height as usize);
        assert!(start <= 4 && 4 < end);
        let row_y = layout.rows.y + (4 - start) as u16;
        assert_eq!(
            mouse_target(area, layout.rows.x, row_y, &state),
            Some(MouseTarget::Row(4))
        );
    }
    #[test]
    fn popup_renders_all_categories_and_selected_explanation() {
        let cfg = UiConfig::default();
        let mut state = SettingsState::new(&cfg);
        state.switch_tab(Tab::Privacy);
        let theme = Theme::by_name(ThemeName::Default);
        let glyphs = crate::tui::theme::detect_glyphs();
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
        terminal
            .draw(|f| {
                render(
                    f,
                    &theme,
                    &glyphs,
                    &state,
                    &cfg,
                    "0.0.0",
                    "abcd",
                    "/tmp/config",
                    "/tmp/files",
                )
            })
            .unwrap();
        let screen = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(screen.contains("Profile"));
        assert!(screen.contains("Privacy"));
        assert!(screen.contains("Warning: automatically accepts"));
    }
    #[test]
    fn reset_resyncs_theme_and_numeric_controls_clamp() {
        let mut cfg = UiConfig::default();
        let mut state = SettingsState::new(&cfg);
        state.bump_sidebar_breakpoint(&mut cfg, -999);
        state.bump_min_chat_width(&mut cfg, -999);
        assert_eq!(cfg.narrow_sidebar_below, 40);
        assert_eq!(cfg.min_conversation_width, 20);
        state.reset_to_defaults(&mut cfg);
        assert_eq!(state.theme_idx, 0);
    }
}
