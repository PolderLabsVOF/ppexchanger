//! Modal popup for the `/discover` command.
//!
//! Mirrors the help overlay but with a live-updating list of peers found by
//! each scan method. The popup dismisses on Esc or any Enter; the
//! underlying scan keeps running until `running == false`.

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, BorderType, Clear, Paragraph, Wrap};
use ratatui::Frame;

const POPUP_W: u16 = 64;
const POPUP_H: u16 = 20;

pub fn render(
    f: &mut Frame,
    theme: &super::theme::Theme,
    glyphs: &super::theme::Glyphs,
    state: &super::DiscoveryState,
) {
    let area = f.area();
    let popup = centered(area);
    f.render_widget(Clear, popup);

    let title = if state.running {
        format!(" {} discovering… ", glyphs.dot_connected)
    } else {
        format!(" {} discovery results ", glyphs.dot_seen)
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme.border_active))
        .title(Span::styled(
            title,
            Style::default().fg(theme.accent).add_modifier(Modifier::BOLD),
        ));

    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(Span::styled(
        format!(" {}", state.summary),
        theme.info_style(),
    )));
    lines.push(Line::from(""));

    if state.results.is_empty() && state.running {
        lines.push(Line::from(Span::styled(
            "  no peers yet…",
            Style::default().fg(theme.peer_text).bg(theme.bg),
        )));
    }

    for method in &state.results {
        lines.push(Line::from(Span::styled(
            format!(" {}", method.name),
            Style::default().fg(theme.accent).add_modifier(Modifier::BOLD),
        )));
        if method.peers.is_empty() {
            lines.push(Line::from(Span::styled(
                "    (none)",
                Style::default().fg(theme.untrusted_mark).bg(theme.bg),
            )));
        } else {
            for p in &method.peers {
                let label = match (&p.name, &p.fingerprint) {
                    (Some(n), Some(fp)) => format!("    {}  {}  {}", n, p.addr, short_fp(fp)),
                    (Some(n), None) => format!("    {}  {}", n, p.addr),
                    (None, Some(fp)) => format!("    {}  {}", p.addr, short_fp(fp)),
                    (None, None) => format!("    {}", p.addr),
                };
                lines.push(Line::from(Span::styled(
                    label,
                    Style::default().fg(theme.peer_text).bg(theme.bg),
                )));
            }
        }
    }

    if !state.running {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "  press Esc to close",
            Style::default().fg(theme.info).bg(theme.bg),
        )));
    }

    let para = Paragraph::new(lines)
        .block(block)
        .wrap(Wrap { trim: false })
        .style(Style::default().fg(theme.fg).bg(theme.bg));
    f.render_widget(para, popup);
}

fn short_fp(fp: &str) -> String {
    if fp.len() <= 12 {
        fp.to_string()
    } else {
        format!("{}…", &fp[..12])
    }
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