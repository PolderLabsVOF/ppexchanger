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
use ratatui::widgets::{Block, Borders, BorderType, Clear, Paragraph, Wrap};
use ratatui::Frame;

const POPUP_W: u16 = 60;
const POPUP_H: u16 = 9;

/// What the user has chosen, if anything. `None` = awaiting decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Pending,
    Accepted,
    Rejected,
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
            Style::default().fg(theme.accent).add_modifier(Modifier::BOLD),
        ));

    let size = human_size(state.offer.size);
    let body = format!("{} wants to send:", state.from_name);
    let file_line = format!("  {}  ({})", state.offer.name, size);

    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(Span::styled(
        body,
        Style::default().fg(theme.fg).bg(theme.bg),
    )));
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        file_line,
        Style::default().fg(theme.peer_text).bg(theme.bg),
    )));
    lines.push(Line::from(""));

    let hint = match state.decision {
        Decision::Pending => "[Enter] accept   [Esc] reject",
        Decision::Accepted => "accepted — receiving…",
        Decision::Rejected => "rejected",
    };
    lines.push(Line::from(Span::styled(
        hint,
        Style::default().fg(theme.info).bg(theme.bg),
    )));

    let para = Paragraph::new(lines)
        .block(block)
        .wrap(Wrap { trim: false })
        .style(Style::default().fg(theme.fg).bg(theme.bg));
    f.render_widget(para, popup);
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
