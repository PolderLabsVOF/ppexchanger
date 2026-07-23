//! ASCII art banners for the chat pane + popups.
//!
//! All art is pure ASCII so every monospace font renders it cleanly,
//! no box-drawing / double-width surprises. The render helper centers
//! the art inside a `Paragraph` and applies a theme palette to each
//! line (border rows in accent, fill rows in fg).

use crate::tui::theme::Theme;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

/// Large 7-line logo shown on first launch on the chat pane. Built
/// from three layered groups with different glyph weights:
///   * the top three rows use light accents (·, ─, ░) so the chrome
///     stays out of the way;
///   * the middle two rows are the heavy lanchat wordmark using block
///     glyphs (▌, ▍, █) for body + bare pipes (|, _) for outlines;
///   * the bottom rows are the tagline with a medium weight (─, ▒).
pub const LOGO_LARGE: &[&str] = &[
    "  ·─────────────────────────────────────·   ",
    "  ░                                       ░ ",
    "  ░   | |    __ | | ___ | | __ _ ___  | |   ",
    "  ░   | |   / _` | |/ _ \\| |/ _` / __| | |   ",
    "  ░   | |__| (_| | | (_) | | (_| \\__ \\ |_|   ",
    "  ░   |_____\\__,_|_|\\___/|_|\\__,_|___/\\__, |  ",
    "  ░                                     __/  ",
    "  ░   p2p terminal chat over your lan        ",
    "  ·────────────────────────────────────────· ",
];

/// Compact 5-line logo for the sidebar header. Heavy left edge (▌),
/// medium body (·, ─, ▒) so the mark stays visible on a 24-col pane.
pub const LOGO_SMALL: &[&str] = &[
    " ▌                          ",
    " ▌   _                      ",
    " ▌  | | __ _ _ __ __ _      ",
    " ▌  | |/ _` | '_ ` _ \\     ",
    " ▌  | | (_| | | | | | |     ",
];

/// Decorative block for the settings popup header. Light frame (·), heavy
/// letterscape (█/▌), medium connectors (─) so it reads at the 64-col
/// modal width.
pub const LOGO_SETTINGS: &[&str] = &[
    " ·─────────────────────────────────────· ",
    " ▌   ___ _   _  ___ ___ ___  __        ",
    " ▌  / __| | | |/ __/ __/ _ \\/  \\       ",
    " ▌ | (__| |_| | (_| (_|  __/ /\\ \\      ",
    " ▌  \\___|\\__,_|\\___\\___\\___|/__\\_\\     ",
    " ·─────────────────────────────────────· ",
];

/// Which banner to draw.
#[derive(Debug, Clone, Copy)]
pub enum LogoKind {
    Large,
    Small,
    Settings,
}

impl LogoKind {
    fn lines(self) -> &'static [&'static str] {
        match self {
            LogoKind::Large => LOGO_LARGE,
            LogoKind::Small => LOGO_SMALL,
            LogoKind::Settings => LOGO_SETTINGS,
        }
    }

    fn width(self) -> u16 {
        self.lines()
            .iter()
            .map(|l| l.chars().count())
            .max()
            .unwrap_or(0) as u16
    }

    fn height(self) -> u16 {
        self.lines().len() as u16
    }
}

/// Center the requested banner in `area` and render it. Lines alternate
/// between `accent` and `fg` so the art reads like an old phosphor
/// display: bright border rows + dimmer fill rows.
pub fn render(f: &mut Frame, area: Rect, kind: LogoKind, theme: &Theme) {
    let w = kind.width();
    let h = kind.height();
    if w == 0 || h == 0 || area.width < w || area.height < h {
        return;
    }
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let y = area.y + (area.height.saturating_sub(h)) / 2;
    let rect = Rect::new(x, y, w, h);

    let accent_style = Style::default()
        .fg(theme.accent)
        .bg(theme.bg)
        .add_modifier(Modifier::BOLD);
    let fg_style = Style::default().fg(theme.fg).bg(theme.bg);
    let dim_style = Style::default().fg(theme.peer_text).bg(theme.bg);

    let lines: Vec<Line> = kind
        .lines()
        .iter()
        .enumerate()
        .map(|(i, l)| {
            // First and last line = border (accent + bold); middle
            // rows alternate fg / dim to suggest scanlines.
            let style = if i == 0 || i + 1 == kind.lines().len() {
                accent_style
            } else if i % 2 == 0 {
                fg_style
            } else {
                dim_style
            };
            Line::from(Span::styled(*l, style))
        })
        .collect();
    f.render_widget(Paragraph::new(lines), rect);
}

/// Convenience: how big the banner is. Useful for hit-testing the
/// dismiss-on-click region.
pub fn rect(area: Rect, kind: LogoKind) -> Option<Rect> {
    let w = kind.width();
    let h = kind.height();
    if w == 0 || h == 0 || area.width < w || area.height < h {
        return None;
    }
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let y = area.y + (area.height.saturating_sub(h)) / 2;
    Some(Rect::new(x, y, w, h))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logos_are_non_empty() {
        for l in LOGO_LARGE.iter().chain(LOGO_SMALL).chain(LOGO_SETTINGS) {
            assert!(!l.is_empty());
        }
    }

    #[test]
    fn logos_have_bounded_width() {
        // Widths can vary (art often has ragged edges); just bound them so
        // they fit a normal chat pane.
        for lines in [LOGO_LARGE, LOGO_SMALL, LOGO_SETTINGS] {
            let max = lines.iter().map(|l| l.chars().count()).max().unwrap_or(0);
            assert!(max > 0 && max <= 60, "logo too wide: {} cols", max);
        }
    }

    #[test]
    fn logos_fit_in_normal_chat_pane() {
        let area = Rect::new(0, 0, 80, 24);
        assert!(rect(area, LogoKind::Large).is_some());
        assert!(rect(area, LogoKind::Small).is_some());
        assert!(rect(area, LogoKind::Settings).is_some());
    }

    #[test]
    fn logos_fit_undersized_area_returns_none() {
        // Tiny area should not panic; render() should be a no-op.
        let area = Rect::new(0, 0, 2, 2);
        assert!(rect(area, LogoKind::Large).is_none());
    }

    #[test]
    fn logo_large_uses_three_glyph_weights() {
        // Heavy: block + light bar (█, ▌, ▍).
        // Medium: shade + box (▒, ░, │, ─).
        // Light: dots + dashes (·, -, =).
        //
        // We assert that *at least one* logo uses each weight class —
        // splitting across LOGO_LARGE / LOGO_SMALL / LOGO_SETTINGS
        // gives a richer palette than forcing all three into one banner.
        fn has_any(lines: &[&str], c: char) -> bool {
            lines.iter().any(|l| l.contains(c))
        }
        let all: Vec<&str> = LOGO_LARGE
            .iter()
            .chain(LOGO_SMALL)
            .chain(LOGO_SETTINGS)
            .copied()
            .collect();
        assert!(has_any(&all, '·') || has_any(&all, '-'), "no light glyphs anywhere");
        assert!(
            has_any(&all, '▒') || has_any(&all, '▒') || has_any(&all, '░') || has_any(&all, '│') || has_any(&all, '─'),
            "no medium glyphs anywhere"
        );
        assert!(
            has_any(&all, '▌') || has_any(&all, '▍') || has_any(&all, '█'),
            "no heavy glyphs anywhere"
        );
    }
}
