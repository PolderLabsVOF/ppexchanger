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
const POPUP_H: u16 = 24;

pub fn render(f: &mut Frame, theme: &super::theme::Theme, glyphs: &super::theme::Glyphs) {
    let area = f.area();
    let popup = centered(area);
    f.render_widget(Clear, popup);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(ratatui::widgets::BorderType::Rounded)
        .title(Span::styled(
            format!(" {} ppexchanger — shortcuts ", glyphs.cursor),
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ))
        .border_style(Style::default().fg(theme.border_active));
    let lines = vec![
        Line::from(Span::styled(
            "Navigation",
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from("  Tab         cycle focus (sidebar <-> chat)"),
        Line::from("  Click       select peers, focus chat, use buttons"),
        Line::from("  Up/Down     in sidebar: move selection"),
        Line::from("               in empty input: history recall"),
        Line::from("  PageUp/Dn   scroll chat scrollback"),
        Line::from(""),
        Line::from(Span::styled(
            "Actions",
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from("  Enter       send message"),
        Line::from(format!(
            "  @<name> ..  route to peer by name ({})",
            glyphs.arrow
        )),
        Line::from("  Ctrl-N      start a new chat with selected peer"),
        Line::from("  Ctrl-T      toggle trust on selected peer"),
        Line::from("  Ctrl-R      revoke selected peer"),
        Line::from("  Ctrl-B      toggle the peer sidebar"),
        Line::from("  Ctrl-P      open the peer picker overlay"),
        Line::from("  Ctrl-L      clear input"),
        Line::from("  Ctrl-,      open settings"),
        Line::from("  Esc         cancel / clear"),
        Line::from("  Ctrl-C / Q  quit"),
        Line::from("  ?           toggle this help"),
        Line::from("  Pending     click / Enter to accept, Esc to decline"),
        Line::from(""),
        Line::from(Span::styled(
            "Commands",
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from("  /discover   find peers on the local network"),
        Line::from("  /send       send the typed path as a file to the active peer"),
        Line::from("  /paste-image  send clipboard image (PNG/JPEG) with preview"),
        Line::from("  /theme      cycle theme (default/solarized/monochrome/neon/amber)"),
        Line::from("  /settings   open settings dialog"),
        Line::from("  /trust <n>  trust a peer by name"),
        Line::from("  /revoke <n> revoke a peer's trust"),
    ];
    let para = Paragraph::new(lines)
        .block(block)
        .style(Style::default().fg(theme.fg).bg(theme.bg));
    f.render_widget(para, popup);
}

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
