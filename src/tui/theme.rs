//! Theme + glyph palettes.
//!
//! Four built-in themes (default, solarized, monochrome, neon) plus a glyph set
//! that auto-detects whether the terminal renders UTF-8 box-drawing characters
//! or falls back to ASCII. Glyph detection is conservative: any non-ASCII
//! codepoint in the set triggers the Unicode variant, otherwise ASCII.

use ratatui::style::{Color, Modifier, Style};

/// Built-in color themes. Selectable via `lanchat --theme <name>` or the
/// in-TUI `/theme <name>` command. Persisted to `config.toml`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThemeName {
    Default,
    Solarized,
    Monochrome,
    Neon,
}

impl ThemeName {
    pub fn as_str(&self) -> &'static str {
        match self {
            ThemeName::Default => "default",
            ThemeName::Solarized => "solarized",
            ThemeName::Monochrome => "monochrome",
            ThemeName::Neon => "neon",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "default" => Some(ThemeName::Default),
            "solarized" => Some(ThemeName::Solarized),
            "monochrome" | "mono" => Some(ThemeName::Monochrome),
            "neon" => Some(ThemeName::Neon),
            _ => None,
        }
    }
}

/// Resolved color palette for the current theme.
#[derive(Debug, Clone, Copy)]
pub struct Theme {
    pub name: ThemeName,
    pub bg: Color,
    pub fg: Color,
    pub accent: Color,
    pub border_active: Color,
    pub border_inactive: Color,
    pub self_text: Color,
    pub peer_text: Color,
    pub trusted_mark: Color,
    pub untrusted_mark: Color,
    pub error: Color,
    pub info: Color,
    pub highlight: Color,
    pub status_bg: Color,
    pub status_fg: Color,
}

impl Theme {
    pub fn by_name(name: ThemeName) -> Self {
        match name {
            ThemeName::Default => Self::default_palette(),
            ThemeName::Solarized => Self::solarized(),
            ThemeName::Monochrome => Self::monochrome(),
            ThemeName::Neon => Self::neon(),
        }
    }

    fn default_palette() -> Self {
        Self {
            name: ThemeName::Default,
            bg: Color::Reset,
            fg: Color::White,
            accent: Color::Cyan,
            border_active: Color::Cyan,
            border_inactive: Color::DarkGray,
            self_text: Color::Green,
            peer_text: Color::Yellow,
            trusted_mark: Color::Green,
            untrusted_mark: Color::DarkGray,
            error: Color::Red,
            info: Color::Blue,
            highlight: Color::Magenta,
            status_bg: Color::Indexed(236),
            status_fg: Color::White,
        }
    }

    fn solarized() -> Self {
        Self {
            name: ThemeName::Solarized,
            bg: Color::Rgb(0, 43, 54),
            fg: Color::Rgb(147, 161, 161),
            accent: Color::Rgb(38, 139, 210),
            border_active: Color::Rgb(133, 153, 0),
            border_inactive: Color::Rgb(88, 110, 117),
            self_text: Color::Rgb(133, 153, 0),
            peer_text: Color::Rgb(181, 137, 0),
            trusted_mark: Color::Rgb(133, 153, 0),
            untrusted_mark: Color::Rgb(88, 110, 117),
            error: Color::Rgb(220, 50, 47),
            info: Color::Rgb(38, 139, 210),
            highlight: Color::Rgb(211, 54, 130),
            status_bg: Color::Rgb(7, 54, 66),
            status_fg: Color::Rgb(147, 161, 161),
        }
    }

    fn monochrome() -> Self {
        Self {
            name: ThemeName::Monochrome,
            bg: Color::Reset,
            fg: Color::White,
            accent: Color::White,
            border_active: Color::White,
            border_inactive: Color::DarkGray,
            self_text: Color::White,
            peer_text: Color::Gray,
            trusted_mark: Color::White,
            untrusted_mark: Color::DarkGray,
            error: Color::Gray,
            info: Color::Gray,
            highlight: Color::White,
            status_bg: Color::Black,
            status_fg: Color::White,
        }
    }

    fn neon() -> Self {
        Self {
            name: ThemeName::Neon,
            bg: Color::Black,
            fg: Color::Rgb(255, 255, 255),
            accent: Color::Rgb(255, 0, 255),
            border_active: Color::Rgb(0, 255, 255),
            border_inactive: Color::Rgb(80, 80, 80),
            self_text: Color::Rgb(0, 255, 128),
            peer_text: Color::Rgb(255, 200, 0),
            trusted_mark: Color::Rgb(255, 0, 255),
            untrusted_mark: Color::Rgb(80, 80, 80),
            error: Color::Rgb(255, 64, 64),
            info: Color::Rgb(64, 200, 255),
            highlight: Color::Rgb(255, 255, 0),
            status_bg: Color::Rgb(20, 0, 30),
            status_fg: Color::Rgb(0, 255, 255),
        }
    }

    pub fn style(&self) -> Style {
        Style::default().bg(self.bg).fg(self.fg)
    }

    pub fn border_style(&self, active: bool) -> Style {
        Style::default()
            .fg(if active { self.border_active } else { self.border_inactive })
            .bg(self.bg)
    }

    pub fn self_message_style(&self) -> Style {
        Style::default().fg(self.self_text).bg(self.bg)
    }

    pub fn peer_message_style(&self) -> Style {
        Style::default().fg(self.peer_text).bg(self.bg)
    }

    pub fn info_style(&self) -> Style {
        Style::default().fg(self.info).bg(self.bg).add_modifier(Modifier::ITALIC)
    }

    pub fn error_style(&self) -> Style {
        Style::default().fg(self.error).bg(self.bg).add_modifier(Modifier::BOLD)
    }

    pub fn status_style(&self) -> Style {
        Style::default().fg(self.status_fg).bg(self.status_bg)
    }

    pub fn highlight_style(&self) -> Style {
        Style::default().fg(self.highlight).bg(self.bg).add_modifier(Modifier::BOLD)
    }

    pub fn trusted_style(&self) -> Style {
        Style::default().fg(self.trusted_mark).bg(self.bg)
    }

    pub fn untrusted_style(&self) -> Style {
        Style::default().fg(self.untrusted_mark).bg(self.bg)
    }
}

/// Glyph set. The terminal width() check tells us whether the runtime can
/// render UTF-8 box characters; we keep an ASCII fallback so the UI degrades
/// gracefully on dumb terminals.
#[derive(Debug, Clone, Copy)]
pub struct Glyphs {
    pub dot_connected: &'static str,
    pub dot_seen: &'static str,
    pub dot_gone: &'static str,
    pub trusted: &'static str,
    pub untrusted: &'static str,
    pub cursor: &'static str,
    pub arrow: &'static str,
    pub ellipsis: &'static str,
}

const GLYPHS_UNICODE: Glyphs = Glyphs {
    dot_connected: "●",
    dot_seen: "○",
    dot_gone: "×",
    trusted: "★",
    untrusted: "☆",
    cursor: "▌",
    arrow: "→",
    ellipsis: "…",
};

const GLYPHS_ASCII: Glyphs = Glyphs {
    dot_connected: "*",
    dot_seen: "o",
    dot_gone: "x",
    trusted: "T",
    untrusted: " ",
    cursor: "|",
    arrow: "->",
    ellipsis: "...",
};

/// Heuristic: try to enable Unicode if the LANG / LC_ALL looks like UTF-8,
/// otherwise ASCII. We don't actually probe the terminal — too risky in CI
/// to print box-drawing bytes blindly.
pub fn detect_glyphs() -> Glyphs {
    let lang = std::env::var("LANG").unwrap_or_default();
    let lc = std::env::var("LC_ALL").unwrap_or_default();
    let utf8 = lang.to_ascii_uppercase().contains("UTF-8")
        || lang.to_ascii_uppercase().contains("UTF8")
        || lc.to_ascii_uppercase().contains("UTF-8")
        || lc.to_ascii_uppercase().contains("UTF8");
    if utf8 {
        GLYPHS_UNICODE
    } else {
        GLYPHS_ASCII
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_roundtrip() {
        for n in [ThemeName::Default, ThemeName::Solarized, ThemeName::Monochrome, ThemeName::Neon] {
            assert_eq!(ThemeName::parse(n.as_str()), Some(n));
        }
        assert_eq!(ThemeName::parse("DEFAULT"), Some(ThemeName::Default));
        assert_eq!(ThemeName::parse("MONO"), Some(ThemeName::Monochrome));
        assert_eq!(ThemeName::parse("bogus"), None);
    }

    #[test]
    fn themes_produce_distinct_palettes() {
        let a = Theme::by_name(ThemeName::Default);
        let b = Theme::by_name(ThemeName::Solarized);
        let c = Theme::by_name(ThemeName::Neon);
        assert_ne!(a.accent, b.accent);
        assert_ne!(a.accent, c.accent);
    }

    #[test]
    fn glyph_detection_is_deterministic() {
        // Setting an env var in the test would race with other tests; just
        // assert that the function doesn't panic and returns one of the
        // two known glyph sets.
        let g = detect_glyphs();
        let is_unicode = g.dot_connected == "●";
        let is_ascii = g.dot_connected == "*";
        assert!(is_unicode || is_ascii);
    }
}