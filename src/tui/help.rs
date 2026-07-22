//! Help overlay rendered as a centered floating block when the user presses
//! `?`. Static text — no interaction beyond pressing `?` or Esc to dismiss.

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::Frame;

/// Width / height of the help popup. Picked to fit comfortably in a 80x24
/// terminal with room to spare on each side.
const POPUP_W: u16 = 56;
const POPUP_H: u16 = 18;

pub fn render(f: &mut Frame, theme: &super::theme::Theme, glyphs: &super::theme::Glyphs) {
    let area = f.area();
    let popup = centered(area);
    f.render_widget(Clear, popup);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(ratatui::widgets::BorderType::Rounded)
        .title(Span::styled(
            format!(" {} lanchat — shortcuts ", glyphs.cursor),
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ))
        .border_style(Style::default().fg(theme.border_active));
    let lines = vec![
        Line::from(Span::styled(
            "Navigation",
            Style::default().fg(theme.accent).add_modifier(Modifier::BOLD),
        )),
        Line::from("  Tab         cycle focus (sidebar <-> chat)"),
        Line::from("  Up/Down     in sidebar: move selection"),
        Line::from("               in empty input: history recall"),
        Line::from("  PageUp/Dn   scroll chat scrollback"),
        Line::from(""),
        Line::from(Span::styled(
            "Actions",
            Style::default().fg(theme.accent).add_modifier(Modifier::BOLD),
        )),
        Line::from("  Enter       send message"),
        Line::from(format!("  @<name> ..  route to peer by name ({})", glyphs.arrow)),
        Line::from("  Ctrl-N      start a new chat with selected peer"),
        Line::from("  Ctrl-T      toggle trust on selected peer"),
        Line::from("  Ctrl-R      revoke selected peer"),
        Line::from("  Ctrl-L      clear input"),
        Line::from("  Esc         cancel / clear"),
        Line::from("  Ctrl-C / Q  quit"),
        Line::from("  ?           toggle this help"),
    ];
    let para = Paragraph::new(lines)
        .block(block)
        .style(Style::default().fg(theme.fg).bg(theme.bg));
    f.render_widget(para, popup);
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