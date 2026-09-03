//! Modal popup for the `/settings` command (Ctrl-,).
//!
//! Three tabs (Display / Input / About) showing live toggles for the UI
//! config. Selecting a row flips a bit; the caller pulls the dirty state
//! out and persists. We deliberately keep `UiConfig` as the single source
//! of truth — this module is a UI view over it, not a separate store.
//!
//! Widgets in active rotation here:
//!   * `Tabs` for sub-navigation
//!   * `Table` + `TableState` for the toggle rows
//!   * `Block::bordered().border_type(Rounded)` for the modal frame
//!   * `Clear` for the modal background fill

use crate::tui::config::{StatusFormat, UiConfig};
use crate::tui::theme::{Glyphs, Theme, ThemeName};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{
    Block, BorderType, Borders, Cell, Clear, Paragraph, Row, Table, TableState, Tabs, Wrap,
};
use ratatui::Frame;

/// Width + height of the modal. Same dimensions as discovery so the
/// hit-test rectangle stays predictable for mouse users.
const POPUP_W: u16 = 72;
const POPUP_H: u16 = 22;

/// Logical order of themes in the cycle. Matches `Theme::by_name`'s
/// supported set, with amber slotted at the end so a fresh install
/// (`theme = "default"` in config) still resolves to the classic look.
pub const THEME_CHOICES: &[ThemeName] = &[
    ThemeName::Default,
    ThemeName::Solarized,
    ThemeName::Monochrome,
    ThemeName::Neon,
    ThemeName::Amber,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Display,
    Input,
    Behavior,
    About,
}

/// Click targets exposed to the event loop. Keeping geometry here ensures
/// mouse selection follows the same layout as the rendered settings panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseTarget {
    Tab(Tab),
    Row(usize),
    Close,
}

impl Tab {
    pub const ALL: [Tab; 4] = [Tab::Display, Tab::Input, Tab::Behavior, Tab::About];

    pub fn label(self) -> &'static str {
        match self {
            Tab::Display => " Display ",
            Tab::Input => " Input ",
            Tab::Behavior => " Behavior ",
            Tab::About => " About ",
        }
    }

    pub fn idx(self) -> usize {
        Self::ALL.iter().position(|t| *t == self).unwrap_or(0)
    }

    pub fn next_tab(self) -> Tab {
        let i = self.idx();
        Tab::ALL[(i + 1) % Tab::ALL.len()]
    }

    pub fn prev_tab(self) -> Tab {
        let i = self.idx();
        Tab::ALL[(i + Tab::ALL.len() - 1) % Tab::ALL.len()]
    }
}

/// State machine for the settings modal. Persists nothing by itself — the
/// caller is responsible for mutating the live `UiConfig` and writing it
/// to disk on close.
#[derive(Debug, Clone)]
pub struct SettingsState {
    pub tab: Tab,
    /// Cursor row within the active tab. Always < the row count of that
    /// tab; the renderer clamps before drawing.
    pub selected: usize,
    pub theme_idx: usize,
    /// True when the user selected the "reset to defaults" row and the
    /// next Enter is the confirm. Reset on any other key so a casual
    /// scroll into the row doesn't arm the action.
    pub confirm_reset: bool,
    /// Set when a mutation occurred; cleared on save. Mirrors the
    /// `apply-on-change` pattern `/theme` uses for the legacy code path.
    pub dirty: bool,
    /// Editable local display name. This persists to the identity record,
    /// rather than `config.toml`, when the dialog closes.
    pub name_draft: String,
    pub editing_name: bool,
    /// Runtime-only data the popup can't derive from `UiConfig`: build
    /// target triple, connected peer count, last-seen timestamp. The
    /// caller (the main loop) refreshes these on every render frame.
    pub extra: SettingsExtras,
}

impl SettingsState {
    pub fn new(cfg: &UiConfig) -> Self {
        let theme_idx = THEME_CHOICES
            .iter()
            .position(|t| *t == cfg.theme)
            .unwrap_or(0);
        Self {
            tab: Tab::Display,
            selected: 0,
            theme_idx,
            confirm_reset: false,
            dirty: false,
            name_draft: String::new(),
            editing_name: false,
            extra: SettingsExtras::default(),
        }
    }

    /// Cycle to the next theme. Returns the new theme name.
    pub fn cycle_theme(&mut self, delta: i32) -> ThemeName {
        let n = THEME_CHOICES.len() as i32;
        let cur = self.theme_idx as i32;
        let next = ((cur + delta).rem_euclid(n)) as usize;
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

    pub fn toggle_auto_trust_seen(&mut self, cfg: &mut UiConfig) {
        cfg.auto_trust_seen = !cfg.auto_trust_seen;
        self.dirty = true;
    }

    /// Cycle through the three status formats. Returns the new value.
    pub fn cycle_status_format(&mut self, cfg: &mut UiConfig) -> StatusFormat {
        cfg.status_format = match cfg.status_format {
            StatusFormat::NameOnly => StatusFormat::NameAddr,
            StatusFormat::NameAddr => StatusFormat::Off,
            StatusFormat::Off => StatusFormat::NameOnly,
        };
        self.dirty = true;
        cfg.status_format
    }

    /// Restore every persisted field to the default. Caller is
    /// responsible for surfacing the confirm prompt — this is the
    /// "Y pressed" action, not the "row selected" one.
    pub fn reset_to_defaults(&mut self, cfg: &mut UiConfig) {
        *cfg = UiConfig::default();
        // The in-memory theme_idx mirror still points at the old
        // selection; resync so the next render shows the default theme
        // label, not a stale lookup.
        self.theme_idx = cfg.theme as usize;
        self.dirty = true;
    }

    /// Adjust scrollback by `delta` (typically ±100). Clamped at the
    /// parser level (16..50_000) so the input is always valid.
    pub fn bump_scrollback(&mut self, cfg: &mut UiConfig, delta: i32) {
        let cur = cfg.scrollback as i32;
        let next = (cur + delta).clamp(16, crate::tui::config::MAX_SCROLLBACK as i32) as usize;
        if next != cfg.scrollback {
            cfg.scrollback = next;
            self.dirty = true;
        }
    }

    pub fn rows_in_tab(&self) -> usize {
        match self.tab {
            Tab::Display => 5,    // theme, footer, scrollback, notify_sound, auto_trust_seen
            Tab::Input => 3,      // mouse, status_format, reset to defaults; (fingerprint copy, custom name) deferred — see ponytail below
            Tab::Behavior => 4,   // auto-trust, notify, status format, custom display name
            Tab::About => 8,      // version, build target, fingerprint (grouped), config path, received dir, peer count, last-seen, custom-name summary
        }
    }

    pub fn selected(&self) -> usize {
        self.selected.min(self.rows_in_tab().saturating_sub(1))
    }

    pub fn move_selection(&mut self, delta: i32) {
        let n = self.rows_in_tab() as i32;
        if n == 0 {
            return;
        }
        let cur = self.selected as i32;
        let next = ((cur + delta).rem_euclid(n)) as usize;
        self.selected = next;
    }

    pub fn switch_tab(&mut self, tab: Tab) {
        self.tab = tab;
        self.selected = self.selected();
    }
}

/// Bounding rect the modal draws over. Mirrors `discovery_popup::centered`
/// — same constants produce the same rect, which keeps mouse hit-tests
/// consistent.
pub fn centered(area: Rect) -> Rect {
    let w = POPUP_W.min(area.width);
    let h = POPUP_H.min(area.height);
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

pub fn mouse_target(area: Rect, col: u16, row: u16, state: &SettingsState) -> Option<MouseTarget> {
    let popup = centered(area);
    if col < popup.x || col >= popup.right() || row < popup.y || row >= popup.bottom() {
        return None;
    }
    let inner = Block::default().borders(Borders::ALL).inner(popup);
    if row == inner.y {
        let tab_width = (inner.width / Tab::ALL.len() as u16).max(1);
        let idx = ((col.saturating_sub(inner.x)) / tab_width) as usize;
        return Tab::ALL.get(idx.min(Tab::ALL.len() - 1)).copied().map(MouseTarget::Tab);
    }
    if row == inner.bottom().saturating_sub(1) {
        return Some(MouseTarget::Close);
    }
    // The table header consumes the first table line; settings rows start
    // immediately below it and are all one terminal row high.
    let first_row = inner.y.saturating_add(2);
    if row >= first_row {
        let idx = (row - first_row) as usize;
        if idx < state.rows_in_tab() {
            return Some(MouseTarget::Row(idx));
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
    let area = f.area();
    let popup = centered(area);
    f.render_widget(Clear, popup);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme.border_active))
        .title(Line::from(ratatui::text::Span::styled(
            " settings ",
            Style::default().fg(theme.accent).add_modifier(Modifier::BOLD),
        )));

    // Keep content inside the rounded frame rather than drawing through it.
    let inner = block.inner(popup);
    // Split into tab strip + table + footer hint.
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // tabs
            Constraint::Min(3),    // table
            Constraint::Length(1), // footer hint
        ])
        .split(inner);

    // Tab strip — three labels with the active one in accent.
    let tab_titles: Vec<Line> = Tab::ALL
        .iter()
        .map(|t| Line::from(ratatui::text::Span::raw(t.label())))
        .collect();
    let tabs = Tabs::new(tab_titles)
        .select(state.tab.idx())
        .style(Style::default().fg(theme.border_inactive).bg(theme.bg))
        .highlight_style(
            Style::default()
                .fg(theme.accent)
                .bg(theme.bg)
                .add_modifier(Modifier::BOLD),
        )
        .divider(ratatui::symbols::DOT);
    f.render_widget(tabs, chunks[0]);

    // Table body for the active tab.
    let (rows, widths) = rows_for_tab(
        state,
        cfg,
        theme,
        version,
        fingerprint,
        config_path,
        received_dir,
    );
    let header = Row::new(vec![
        Cell::from("setting"),
        Cell::from("value"),
        Cell::from(""),
    ])
    .style(
        Style::default()
            .fg(theme.accent)
            .bg(theme.bg)
            .add_modifier(Modifier::BOLD),
    );
    let table = Table::new(rows, widths)
        .header(header)
        .column_spacing(2)
        .style(Style::default().fg(theme.fg).bg(theme.bg))
        .row_highlight_style(
            Style::default()
                .fg(theme.accent)
                .bg(theme.bg)
                .add_modifier(Modifier::REVERSED),
        )
        .highlight_symbol(">");
    let mut ts = TableState::default();
    ts.select(Some(state.selected()));
    f.render_stateful_widget(table, chunks[1], &mut ts);

    // Footer hint.
    let hint = match state.tab {
        Tab::Display => " click a row to change · click here or Esc to save & close ",
        Tab::Input => " click a row to change · click here or Esc to save & close ",
        Tab::Behavior => " click a row to change · click here or Esc to save & close ",
        Tab::About => " click here or Esc to close ",
    };
    f.render_widget(
        Paragraph::new(Line::from(ratatui::text::Span::styled(
            hint,
            Style::default().fg(theme.info).bg(theme.bg),
        )))
        .wrap(Wrap { trim: false })
        .style(Style::default().bg(theme.bg)),
        chunks[2],
    );

    // Outer frame drawn last so the border sits on top of the tabs/table.
    f.render_widget(block, popup);
}

fn rows_for_tab(
    state: &SettingsState,
    cfg: &UiConfig,
    theme: &Theme,
    version: &str,
    fingerprint: &str,
    config_path: &str,
    received_dir: &str,
) -> (Vec<Row<'static>>, Vec<Constraint>) {
    let widths = vec![
        Constraint::Length(16),
        Constraint::Length(28),
        Constraint::Min(4),
    ];
    let label_style = Style::default().fg(theme.fg).bg(theme.bg);
    let value_style = label_style;
    let hint_style = Style::default().fg(theme.info).bg(theme.bg);
    let header_label_style = Style::default()
        .fg(theme.accent)
        .bg(theme.bg)
        .add_modifier(Modifier::BOLD);

    let mk = |label: &str, value: String, hint: &str| -> Row<'static> {
        Row::new(vec![
            Cell::from(label.to_string()).style(label_style),
            Cell::from(value).style(value_style),
            Cell::from(hint.to_string()).style(hint_style),
        ])
    };

    match state.tab {
        Tab::Display => {
            let theme_name = THEME_CHOICES[state.theme_idx.min(THEME_CHOICES.len() - 1)].as_str();
            let rows = vec![
                mk("Theme", theme_name.to_string(), "←/→ cycles")
                    .style(header_label_style),
                mk(
                    "Show footer",
                    if cfg.show_footer { "on" } else { "off" }.to_string(),
                    "Enter toggles (live)",
                ),
                mk(
                    "Scrollback",
                    format!("{} lines", cfg.scrollback),
                    "←/→ ±100 (live)",
                ),
                mk(
                    "Notify sound",
                    if cfg.notify_sound { "on" } else { "off" }.to_string(),
                    "Enter toggles (live)",
                ),
                mk(
                    "Auto-trust new",
                    if cfg.auto_trust_seen { "on" } else { "off" }.to_string(),
                    "Enter toggles (live)",
                ),
            ];
            (rows, widths)
        }
        Tab::Input => {
            let widget = cfg.status_format.as_str();
            let rows = vec![
                mk(
                    "Mouse capture",
                    if cfg.mouse { "on" } else { "off" }.to_string(),
                    "Enter toggles (live)",
                ),
                mk(
                    "Status line",
                    widget.to_string(),
                    "←/→ cycles",
                ),
                mk(
                    "Reset to defaults",
                    if state.confirm_reset { "Y to confirm" } else { "—" }.to_string(),
                    if state.confirm_reset { "any other key cancels" } else { "Enter arms" },
                ),
            ];
            (rows, widths)
        }
        Tab::Behavior => {
            let rows = vec![
                mk(
                    "Notify sound",
                    if cfg.notify_sound { "on" } else { "off" }.to_string(),
                    "Enter toggles (live)",
                ),
                mk(
                    "Auto-trust seen",
                    if cfg.auto_trust_seen { "on" } else { "off" }.to_string(),
                    "Enter toggles (live)",
                ),
                mk(
                    "Status line",
                    cfg.status_format.as_str().to_string(),
                    "←/→ cycles",
                ),
                mk(
                    "Display name",
                    if state.editing_name {
                        format!("{}▏", state.name_draft)
                    } else {
                        state.name_draft.clone()
                    },
                    if state.editing_name { "type · Enter saves" } else { "Enter to edit" },
                ),
            ];
            (rows, widths)
        }
        Tab::About => {
            // About rows are read-only — no hint column. 8 rows.
            let rows: Vec<Row<'static>> = vec![
                Row::new(vec![
                    Cell::from("Version").style(label_style),
                    Cell::from(version.to_string()).style(value_style),
                    Cell::from(""),
                ]),
                Row::new(vec![
                    Cell::from("Build target").style(label_style),
                    Cell::from(state.extra.build_target.to_string()).style(value_style),
                    Cell::from(""),
                ]),
                Row::new(vec![
                    Cell::from("Fingerprint").style(label_style),
                    Cell::from(grouped_fp(fingerprint)).style(value_style),
                    Cell::from("copy on tab Input"),
                ]),
                Row::new(vec![
                    Cell::from("Config path").style(label_style),
                    Cell::from(config_path.to_string()).style(value_style),
                    Cell::from(""),
                ]),
                Row::new(vec![
                    Cell::from("Received dir").style(label_style),
                    Cell::from(received_dir.to_string()).style(value_style),
                    Cell::from(""),
                ]),
                Row::new(vec![
                    Cell::from("Connected peers").style(label_style),
                    Cell::from(state.extra.connected_count.to_string()).style(value_style),
                    Cell::from(""),
                ]),
                Row::new(vec![
                    Cell::from("Last-seen").style(label_style),
                    Cell::from(format_last_seen(state.extra.last_seen_unix)).style(value_style),
                    Cell::from(""),
                ]),
                Row::new(vec![
                    Cell::from("Display name").style(label_style),
                    Cell::from(state.name_draft.clone()).style(value_style),
                    Cell::from(""),
                ]),
            ];
            (rows, widths)
        }
    }
}

/// Runtime data the settings popup needs but isn't part of `UiConfig`:
/// build target triple, current connected peer count, last-seen epoch.
#[derive(Debug, Clone, Default)]
pub struct SettingsExtras {
    pub build_target: &'static str,
    pub connected_count: usize,
    pub last_seen_unix: u64,
}

/// Format the hex fingerprint as `xx:xx:xx:xx:xx:xx:xx:xx:xx:xx:xx:xx:xx:xx:xx:xx`
/// (16 groups of 2 hex chars separated by colons). Truncates or pads
/// gracefully so a short input doesn't blow up.
fn grouped_fp(s: &str) -> String {
    let hex_chars: String = s.chars().filter(|c| c.is_ascii_hexdigit()).collect();
    let groups: Vec<String> = (0..16)
        .map(|i| {
            let start = i * 2;
            let end = start + 2;
            if end <= hex_chars.len() {
                hex_chars[start..end].to_string()
            } else if start < hex_chars.len() {
                hex_chars[start..].to_string()
            } else {
                "00".to_string()
            }
        })
        .collect();
    groups.join(":")
}

/// Format a unix timestamp as `HH:MM:SS` (UTC). Empty when `0` (i.e. never).
fn format_last_seen(unix: u64) -> String {
    if unix == 0 {
        return "never".to_string();
    }
    let secs = (unix % 86_400) as u32;
    let h = secs / 3600;
    let m = (secs % 3600) / 60;
    let s = secs % 60;
    format!("{:02}:{:02}:{:02}", h, m, s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::config::DEFAULT_SCROLLBACK;

    fn mk_cfg() -> UiConfig {
        UiConfig::default()
    }

    #[test]
    fn settings_state_defaults_to_display_tab_first_row() {
        let s = SettingsState::new(&mk_cfg());
        assert_eq!(s.tab, Tab::Display);
        assert_eq!(s.selected, 0);
        assert!(!s.dirty);
    }

    #[test]
    fn cycle_theme_wraps_and_marks_dirty() {
        let mut s = SettingsState::new(&mk_cfg());
        let initial = s.theme_idx;
        let next = s.cycle_theme(1);
        assert_eq!(s.theme_idx, (initial + 1) % THEME_CHOICES.len());
        assert_eq!(next.as_str(), THEME_CHOICES[s.theme_idx].as_str());
        assert!(s.dirty);
    }

    #[test]
    fn cycle_theme_backwards_wraps_to_end() {
        let mut s = SettingsState::new(&mk_cfg());
        s.theme_idx = 0;
        let _ = s.cycle_theme(-1);
        assert_eq!(s.theme_idx, THEME_CHOICES.len() - 1);
    }

    #[test]
    fn bump_scrollback_clamps() {
        let mut s = SettingsState::new(&mk_cfg());
        let mut cfg = mk_cfg();
        cfg.scrollback = 100;
        s.bump_scrollback(&mut cfg, -99); // 100 - 99 = 1, clamp to 16
        assert_eq!(cfg.scrollback, 16);
        s.bump_scrollback(&mut cfg, 50_000); // 16 + 50_000 > 50_000
        assert_eq!(cfg.scrollback, crate::tui::config::MAX_SCROLLBACK);
    }

    #[test]
    fn toggle_footer_and_mouse_mark_dirty() {
        let mut s = SettingsState::new(&mk_cfg());
        let mut cfg = mk_cfg();
        let initial = cfg.show_footer;
        s.toggle_footer(&mut cfg);
        assert_eq!(cfg.show_footer, !initial);
        assert!(s.dirty);
        let initial_mouse = cfg.mouse;
        s.toggle_mouse(&mut cfg);
        assert_eq!(cfg.mouse, !initial_mouse);
    }

    #[test]
    fn move_selection_wraps_within_tab() {
        let mut s = SettingsState::new(&mk_cfg());
        s.tab = Tab::Display; // 5 rows after v0.5.0
        s.selected = 0;
        s.move_selection(-1);
        assert_eq!(s.selected, 4); // wrapped to last
        s.move_selection(2);
        assert_eq!(s.selected, 1);
    }

    #[test]
    fn switch_tab_clamps_selection_to_row_count() {
        let mut s = SettingsState::new(&mk_cfg());
        s.tab = Tab::Display;
        s.selected = 4; // row 4 of 5 (valid)
        s.switch_tab(Tab::Input); // 3 rows, must clamp to 2
        assert_eq!(s.selected, 2);
        s.switch_tab(Tab::Behavior); // 4 rows, must clamp
        assert_eq!(s.selected, 2);
        s.switch_tab(Tab::About); // 8 rows
        assert_eq!(s.selected, 2);
    }

    #[test]
    fn rows_in_tab_matches_plan() {
        let mut s = SettingsState::new(&mk_cfg());
        s.tab = Tab::Display;
        assert_eq!(s.rows_in_tab(), 5);
        s.tab = Tab::Input;
        assert_eq!(s.rows_in_tab(), 3);
        s.tab = Tab::Behavior;
        assert_eq!(s.rows_in_tab(), 4);
        s.tab = Tab::About;
        assert_eq!(s.rows_in_tab(), 8);
    }

    #[test]
    fn mouse_target_selects_tabs_rows_and_footer() {
        let mut state = SettingsState::new(&mk_cfg());
        let area = Rect::new(0, 0, 100, 30);
        let popup = centered(area);
        let inner = Block::default().borders(Borders::ALL).inner(popup);
        assert_eq!(
            mouse_target(area, inner.x + inner.width / 2, inner.y, &state),
            Some(MouseTarget::Tab(Tab::Behavior))
        );
        state.tab = Tab::Behavior;
        assert_eq!(
            mouse_target(area, inner.x + 2, inner.y + 3, &state),
            Some(MouseTarget::Row(1))
        );
        assert_eq!(
            mouse_target(area, inner.x + 2, inner.bottom() - 1, &state),
            Some(MouseTarget::Close)
        );
    }

    #[test]
    fn toggle_notify_sound_and_auto_trust_mark_dirty() {
        let mut s = SettingsState::new(&mk_cfg());
        let mut cfg = mk_cfg();
        assert!(!cfg.notify_sound);
        s.toggle_notify_sound(&mut cfg);
        assert!(cfg.notify_sound);
        assert!(s.dirty);
        assert!(!cfg.auto_trust_seen);
        s.toggle_auto_trust_seen(&mut cfg);
        assert!(cfg.auto_trust_seen);
    }

    #[test]
    fn reset_to_defaults_restores_every_field() {
        // Set every field to a non-default value, call reset, expect
        // values to match UiConfig::default() exactly.
        let mut s = SettingsState::new(&mk_cfg());
        let mut cfg = mk_cfg();
        // Cycle theme to something non-default.
        let _ = s.cycle_theme(1);
        cfg.theme = THEME_CHOICES[s.theme_idx];
        cfg.mouse = false;
        cfg.show_footer = false;
        cfg.scrollback = 200;
        cfg.notify_sound = true;
        cfg.auto_trust_seen = true;
        cfg.status_format = StatusFormat::Off;
        assert!(s.dirty);

        s.reset_to_defaults(&mut cfg);

        let def = UiConfig::default();
        assert_eq!(cfg.theme, def.theme);
        assert!(cfg.mouse);
        assert!(cfg.show_footer);
        assert_eq!(cfg.scrollback, DEFAULT_SCROLLBACK);
        assert!(!cfg.notify_sound);
        assert!(!cfg.auto_trust_seen);
        assert_eq!(cfg.status_format, StatusFormat::NameOnly);
        assert!(s.dirty);
        // theme_idx was resynced
        assert_eq!(s.theme_idx, def.theme as usize);
    }

    #[test]
    fn cycle_status_format_cycles_through_three() {
        let mut s = SettingsState::new(&mk_cfg());
        let mut cfg = mk_cfg();
        assert_eq!(cfg.status_format, StatusFormat::NameOnly);
        s.cycle_status_format(&mut cfg);
        assert_eq!(cfg.status_format, StatusFormat::NameAddr);
        s.cycle_status_format(&mut cfg);
        assert_eq!(cfg.status_format, StatusFormat::Off);
        s.cycle_status_format(&mut cfg);
        assert_eq!(cfg.status_format, StatusFormat::NameOnly);
    }

    #[test]
    fn grouped_fp_formats_16_groups() {
        let fp = "0123456789abcdef0123456789abcdef";
        let g = grouped_fp(fp);
        // 16 groups of 2 hex chars separated by 15 colons.
        assert_eq!(g.split(':').count(), 16);
        assert_eq!(g, "01:23:45:67:89:ab:cd:ef:01:23:45:67:89:ab:cd:ef");
    }

    #[test]
    fn grouped_fp_tolerates_short_input() {
        let g = grouped_fp("ab");
        assert_eq!(g, "ab:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00");
    }

    #[test]
    fn grouped_fp_strips_non_hex() {
        // The colons are not hex digits, so they're stripped; the digits
        // group together in the same casing the input had.
        let g = grouped_fp("AB:CD:EF:12:34:56:78:90:AB:CD:EF:12:34:56:78:90");
        assert_eq!(g, "AB:CD:EF:12:34:56:78:90:AB:CD:EF:12:34:56:78:90");
    }

    #[test]
    fn format_last_seen_zero_is_never() {
        assert_eq!(format_last_seen(0), "never");
    }

    #[test]
    fn format_last_seen_formats_hms() {
        // 13:08:15 UTC = 13*3600 + 8*60 + 15 = 47295 seconds since midnight.
        assert_eq!(format_last_seen(47295), "13:08:15");
    }
}
