//! Modal popup for an inbound file offer.
//!
//! Mirrors `discovery_popup` — clears the centred rect, draws a small
//! box with the sender, file name, and size, plus an Accept / Reject
//! hint. The popup stays open until the user confirms or dismisses; the
//! underlying `FrameBody` machinery is driven separately by the action
//! thread so the UI never blocks on file I/O.

use crate::events::FileOffer;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph, Wrap};
use ratatui::Frame;

const POPUP_W: u16 = 60;
const POPUP_H: u16 = 15;

/// What the user has chosen, if anything. `None` = awaiting decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Pending,
    Accepted,
    Rejected,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseAction {
    Accept,
    Reject,
}

#[derive(Debug, Clone)]
pub struct FileOfferPrompt {
    pub from_peer: crate::events::PeerId,
    pub from_name: String,
    pub offer: FileOffer,
    pub decision: Decision,
}

pub fn render(
    f: &mut Frame,
    theme: &super::theme::Theme,
    glyphs: &super::theme::Glyphs,
    state: &FileOfferPrompt,
) {
    let area = f.area();
    let popup = centered(area);
    f.render_widget(Clear, popup);

    let title = format!(" {} file offer ", glyphs.dot_connected);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme.border_active))
        .title(Span::styled(
            title,
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ));

    let size = human_size(state.offer.size);
    let is_text = state
        .offer
        .mime
        .as_deref()
        .is_some_and(|mime| mime.starts_with("text/"))
        || state.offer.name.to_ascii_lowercase().ends_with(".txt");
    let body = if is_text {
        format!(
            "{} wants to send a text file (preview after download):",
            state.from_name
        )
    } else {
        format!("{} wants to send:", state.from_name)
    };
    let file_line = format!("  {}  ({})", state.offer.name, size);

    let hint = match state.decision {
        Decision::Pending => Line::from(vec![
            Span::styled(
                if is_text {
                    "  DOWNLOAD (Enter)  "
                } else {
                    "  ACCEPT (Enter)  "
                },
                Style::default()
                    .fg(theme.bg)
                    .bg(theme.accent)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("   "),
            Span::styled(
                "  REJECT (Esc)  ",
                Style::default()
                    .fg(theme.error)
                    .bg(theme.status_bg)
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
        Decision::Accepted => Line::from(Span::styled("accepted — receiving…", theme.info_style())),
        Decision::Rejected => Line::from(Span::styled("rejected", theme.error_style())),
    };
    let mut lines: Vec<Line> = vec![
        Line::from(Span::styled(
            body,
            Style::default().fg(theme.fg).bg(theme.bg),
        )),
        Line::from(""),
        Line::from(Span::styled(
            file_line,
            Style::default().fg(theme.peer_text).bg(theme.bg),
        )),
    ];
    if let Some(preview) = state.offer.preview.as_ref() {
        lines.push(Line::from(Span::styled(
            "Preview",
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        )));
        lines.extend(preview.lines().take(5).map(|line| {
            Line::from(Span::styled(
                format!("  {}", line),
                Style::default().fg(theme.fg).bg(theme.bg),
            ))
        }));
    }
    lines.push(Line::from(""));
    lines.push(hint);

    let para = Paragraph::new(lines)
        .block(block)
        .wrap(Wrap { trim: false })
        .style(Style::default().fg(theme.fg).bg(theme.bg));
    f.render_widget(para, popup);
}

/// The bottom action row is split into two large mouse targets, so the file
/// prompt is not keyboard-only.
pub fn mouse_action(area: Rect, col: u16, row: u16) -> Option<MouseAction> {
    mouse_action_for_preview(area, col, row, None)
}

/// Mouse hit-test that mirrors the rendered preview length for a concrete
/// offer. Keeping this separate preserves the small public helper used by
/// older callers while making preview rows non-clickable.
pub fn mouse_action_for_preview(
    area: Rect,
    col: u16,
    row: u16,
    preview: Option<&str>,
) -> Option<MouseAction> {
    let popup = centered(area);
    let action_offset = preview
        .map(|text| 6 + text.lines().take(5).count() as u16)
        .unwrap_or(5);
    let action_row = popup.y.saturating_add(action_offset);
    if row != action_row || col < popup.x || col >= popup.right() {
        return None;
    }
    if col < popup.x.saturating_add(popup.width / 2) {
        Some(MouseAction::Accept)
    } else {
        Some(MouseAction::Reject)
    }
}

/// Rect of the centred popup, used by `hit_test` to consume clicks.
pub fn rect(area: Rect) -> Rect {
    centered(area)
}

fn centered(area: Rect) -> Rect {
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

fn human_size(bytes: u64) -> String {
    const K: u64 = 1024;
    if bytes < K {
        format!("{} B", bytes)
    } else if bytes < K * K {
        format!("{:.1} KiB", bytes as f64 / K as f64)
    } else if bytes < K * K * K {
        format!("{:.1} MiB", bytes as f64 / (K * K) as f64)
    } else {
        format!("{:.2} GiB", bytes as f64 / (K * K * K) as f64)
    }
}
