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
//!   * `Block::bordered().border_type(Double)` for the modal frame
//!   * `Clear` for the modal background fill

use crate::tui::config::UiConfig;
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
const POPUP_W: u16 = 64;
const POPUP_H: u16 = 20;

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
    About,
}

impl Tab {
    pub const ALL: [Tab; 3] = [Tab::Display, Tab::Input, Tab::About];

    pub fn label(self) -> &'static str {
        match self {
            Tab::Display => " Display ",
            Tab::Input => " Input ",
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
    /// Set when a mutation occurred; cleared on save. Mirrors the
    /// `apply-on-change` pattern `/theme` uses for the legacy code path.
    pub dirty: bool,
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
            dirty: false,
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
            Tab::Display => 3, // theme, footer, scrollback
            Tab::Input => 1,   // mouse
            Tab::About => 4,   // version, fingerprint, config path, received dir
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
        .border_type(BorderType::Double)
        .border_style(Style::default().fg(theme.border_active))
        .title(Line::from(ratatui::text::Span::styled(
            " settings ",
            Style::default().fg(theme.accent).add_modifier(Modifier::BOLD),
        )));

    // Split into tab strip + table + footer hint.
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // tabs
            Constraint::Min(3),    // table
            Constraint::Length(1), // footer hint
        ])
        .split(popup);

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
        Tab::Display => " ←/→ change   Enter cycle theme   Esc save & close ",
        Tab::Input => " ←/→ toggle   Esc save & close ",
        Tab::About => " Esc close ",
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
                    .style(header_label_style.clone()),
                mk(
                    "Show footer",
                    if cfg.show_footer { "on" } else { "off" }.to_string(),
                    "Enter toggles",
                ),
                mk(
                    "Scrollback",
                    format!("{} lines", cfg.scrollback),
                    "←/→ ±100",
                ),
            ];
            (rows, widths)
        }
        Tab::Input => {
            let rows = vec![mk(
                "Mouse capture",
                if cfg.mouse { "on" } else { "off" }.to_string(),
                "Enter toggles (effective next launch)",
            )];
            (rows, widths)
        }
        Tab::About => {
            // About rows are read-only — no hint column.
            let rows: Vec<Row<'static>> = vec![
                Row::new(vec![
                    Cell::from("Version").style(label_style),
                    Cell::from(version.to_string()).style(value_style),
                    Cell::from(""),
                ]),
                Row::new(vec![
                    Cell::from("Fingerprint").style(label_style),
                    Cell::from(short_fp(fingerprint)).style(value_style),
                    Cell::from(""),
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
            ];
            (rows, widths)
        }
    }
}

fn short_fp(s: &str) -> String {
    if s.len() <= 12 {
        s.to_string()
    } else {
        format!("{}…", &s[..12])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        s.tab = Tab::Display; // 3 rows
        s.selected = 0;
        s.move_selection(-1);
        assert_eq!(s.selected, 2); // wrapped to last
        s.move_selection(2);
        assert_eq!(s.selected, 1);
    }

    #[test]
    fn switch_tab_clamps_selection_to_row_count() {
        let mut s = SettingsState::new(&mk_cfg());
        s.tab = Tab::Display;
        s.selected = 2; // row 2 of 3 (valid)
        s.switch_tab(Tab::Input); // only 1 row, must clamp
        assert_eq!(s.selected, 0);
        s.switch_tab(Tab::About); // 4 rows
        assert_eq!(s.selected, 0);
    }

    #[test]
    fn rows_in_tab_matches_plan() {
        let mut s = SettingsState::new(&mk_cfg());
        s.tab = Tab::Display;
        assert_eq!(s.rows_in_tab(), 3);
        s.tab = Tab::Input;
        assert_eq!(s.rows_in_tab(), 1);
        s.tab = Tab::About;
        assert_eq!(s.rows_in_tab(), 4);
    }
}
