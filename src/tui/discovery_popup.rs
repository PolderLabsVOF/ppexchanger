//! Modal popup for the `/discover` command.
//!
//! Mirrors the help overlay but with a live-updating list of peers found by
//! each scan method. The popup dismisses on Esc or any Enter; the
//! underlying scan keeps running until `running == false`.
//!
//! A second view (`render_map`) replaces the list with a Canvas widget
//! that plots peers by IP-address. x = last IPv4 octet (0..256), y =
//! /24 bucket derived from the first three octets. Marker::Braille
//! gives sub-cell resolution so even one peer reads as a clear dot.

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    canvas::{Canvas, Line as CanvasLine, Points},
    Block, BorderType, Borders, Clear, Paragraph, Wrap,
};
use ratatui::{symbols, Frame};

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

/// Map view of discovered peers — Canvas widget with `Marker::Braille`
/// for sub-cell dots. x axis = IPv4 last-octet (0..=255); y axis =
/// derived /24 prefix hash so different subnets stack vertically.
/// The 256×256 space fits a /16 comfortably; multi-segment LANs would
/// just share the same y bucket, which is fine for a "where on the LAN"
/// view.
pub fn render_map(
    f: &mut Frame,
    theme: &super::theme::Theme,
    glyphs: &super::theme::Glyphs,
    state: &super::DiscoveryState,
) {
    let area = f.area();
    let popup = centered(area);
    f.render_widget(Clear, popup);

    let title = if state.running {
        format!(" {} map (scanning…) ", glyphs.dot_connected)
    } else {
        format!(" {} peer map ", glyphs.dot_seen)
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Double)
        .border_style(Style::default().fg(theme.border_active))
        .title(Span::styled(
            title,
            Style::default().fg(theme.accent).add_modifier(Modifier::BOLD),
        ));

    // Body: split into a Canvas plot + a footer hint line.
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(5), Constraint::Length(1)])
        .split(popup);

    // Gather every peer's IPv4 address into a (x, y, color) tuple.
    // Color is the theme's peer_text by default; trusted peers take
    // accent. We hash the /24 prefix into a 0..16 y bucket so the map
    // has stable layout across scans.
    let mut dots: Vec<(f64, f64, ratatui::style::Color)> = Vec::new();
    for method in &state.results {
        for p in &method.peers {
            if let std::net::SocketAddr::V4(v4) = p.addr {
                let octets = v4.ip().octets();
                let x = octets[3] as f64;
                let y_bucket = ((octets[0] as usize)
                    ^ ((octets[1] as usize) << 1)
                    ^ ((octets[2] as usize) << 2))
                    % 16;
                let y = y_bucket as f64;
                let _ = octets;
                let color = if method.name.contains("multicast") {
                    theme.accent // multicast beacons are typically trusted discovery
                } else {
                    theme.peer_text
                };
                dots.push((x, y, color));
            }
        }
    }

    let canvas = Canvas::default()
        .background_color(theme.bg)
        .x_bounds([0.0, 256.0])
        .y_bounds([0.0, 16.0])
        .marker(symbols::Marker::Braille)
        .paint(|ctx| {
            // Subtle baseline grid so the user can read coordinates.
            for y in (0..=16).step_by(4) {
                ctx.draw(&CanvasLine {
                    x1: 0.0,
                    y1: y as f64,
                    x2: 256.0,
                    y2: y as f64,
                    color: theme.border_inactive,
                });
            }
            // Plot each peer as a single braille point.
            for (x, y, color) in &dots {
                ctx.draw(&Points {
                    coords: &[(*x, *y)],
                    color: *color,
                });
            }
            // If nothing yet, drop a "?" at the center so the map is
            // never empty (helpful while a scan is still running).
            if dots.is_empty() {
                ctx.draw(&Points {
                    coords: &[(128.0, 8.0)],
                    color: theme.untrusted_mark,
                });
            }
        });
    f.render_widget(canvas, chunks[0]);

    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            " press 1 for list view · Esc close ",
            Style::default().fg(theme.info).bg(theme.bg),
        )))
        .style(Style::default().bg(theme.bg)),
        chunks[1],
    );

    // Outer frame last.
    f.render_widget(block, popup);
}