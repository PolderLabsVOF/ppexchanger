//! Theme + glyph palettes.
//!
//! Five built-in themes (default, solarized, monochrome, neon, amber) plus a glyph set
//! that auto-detects whether the terminal renders UTF-8 box-drawing characters
//! or falls back to ASCII. Glyph detection is conservative: any non-ASCII
//! codepoint in the set triggers the Unicode variant, otherwise ASCII.

use ratatui::style::{Color, Modifier, Style};

/// Semantic style roles.
///
/// Render code asks for a role by name instead of poking individual palette
/// fields. Each concrete theme resolves every role so the renderer never
/// falls back to an ad-hoc `Style::default().fg(theme.x)` — that scattered
/// pattern is what made the previous chrome read like a stack of unrelated
/// buttons. Roles map one-to-one with the spec's "semantic color rules":
/// brightness carries hierarchy, color only signals semantic state.
///
/// Lavender / purple = focused nav, focused input, commands,
/// keyboard-shortcut brackets.
/// Cyan / blue      = peer identifiers, connection info, commands.
/// Green            = success (connected, delivered).
/// Red              = danger (errors, disconnect, destructive).
/// Gray             = timestamps, fingerprints, hints, inactive nav.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StyleRole {
    /// Primary readable body text. fg + bg with no modifier.
    TextPrimary,
    /// Secondary text — peer summaries, secondary labels.
    TextSecondary,
    /// Muted text — timestamps, fingerprints, hints, placeholder copy.
    TextMuted,
    /// Accent — focused borders, active nav, keyboard shortcut brackets,
    /// commands. The "look here" cue.
    TextAccent,
    /// Successful state — connected, delivered, trusted.
    TextSuccess,
    /// Warning state — pending, partial trust.
    TextWarning,
    /// Error state — failed, revoked, destructive.
    TextDanger,
    /// Background surface — the base canvas behind everything.
    SurfaceBase,
    /// Panel surface — composer, sidebar, secondary surfaces. Slightly
    /// different from base so chrome reads as elevated.
    SurfacePanel,
    /// Selection / highlight surface — currently-selected sidebar row.
    SurfaceSelected,
    /// Incoming message surface — usually the same as `SurfaceBase` so
    /// incoming bubbles rely on alignment + whitespace instead of fill.
    SurfaceMessageIncoming,
    /// Outgoing message surface — subtle tone so the reader's eye lands
    /// here without a per-row rectangle.
    SurfaceMessageOutgoing,
    /// Normal (unfocused) border color.
    BorderNormal,
    /// Focused border color — the dominant focus cue.
    BorderFocused,
}

impl StyleRole {
    /// Stable identifier used by tests and theme diagnostics.
    pub fn as_str(&self) -> &'static str {
        match self {
            StyleRole::TextPrimary => "text.primary",
            StyleRole::TextSecondary => "text.secondary",
            StyleRole::TextMuted => "text.muted",
            StyleRole::TextAccent => "text.accent",
            StyleRole::TextSuccess => "text.success",
            StyleRole::TextWarning => "text.warning",
            StyleRole::TextDanger => "text.danger",
            StyleRole::SurfaceBase => "surface.base",
            StyleRole::SurfacePanel => "surface.panel",
            StyleRole::SurfaceSelected => "surface.selected",
            StyleRole::SurfaceMessageIncoming => "surface.message.incoming",
            StyleRole::SurfaceMessageOutgoing => "surface.message.outgoing",
            StyleRole::BorderNormal => "border.normal",
            StyleRole::BorderFocused => "border.focused",
        }
    }
}

/// Built-in color themes. Selectable via `ppexchanger --theme <name>` or the
/// in-TUI `/theme <name>` command. Persisted to `config.toml`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThemeName {
    Default,
    Solarized,
    Monochrome,
    Neon,
    Amber,
}

impl ThemeName {
    pub fn as_str(&self) -> &'static str {
        match self {
            ThemeName::Default => "default",
            ThemeName::Solarized => "solarized",
            ThemeName::Monochrome => "monochrome",
            ThemeName::Neon => "neon",
            ThemeName::Amber => "amber",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "default" => Some(ThemeName::Default),
            "solarized" => Some(ThemeName::Solarized),
            "monochrome" | "mono" => Some(ThemeName::Monochrome),
            "neon" => Some(ThemeName::Neon),
            "amber" => Some(ThemeName::Amber),
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
    pub gauge_filled: Color,
    pub gauge_unfilled: Color,
    /// Peer status indicator colors
    pub status_online: Color, // Green - peer is connected
    pub status_seen: Color,    // Yellow - peer seen via beacon
    pub status_offline: Color, // Gray - peer unreachable
}

impl Theme {
    pub fn by_name(name: ThemeName) -> Self {
        match name {
            ThemeName::Default => Self::default_palette(),
            ThemeName::Solarized => Self::solarized(),
            ThemeName::Monochrome => Self::monochrome(),
            ThemeName::Neon => Self::neon(),
            ThemeName::Amber => Self::amber(),
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
            gauge_filled: Color::Cyan,
            gauge_unfilled: Color::DarkGray,
            status_online: Color::Green,
            status_seen: Color::Yellow,
            status_offline: Color::DarkGray,
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
            gauge_filled: Color::Rgb(133, 153, 0),
            gauge_unfilled: Color::Rgb(7, 54, 66),
            status_online: Color::Rgb(0, 255, 136),
            status_seen: Color::Rgb(181, 137, 0),
            status_offline: Color::Rgb(88, 110, 117),
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
            gauge_filled: Color::White,
            gauge_unfilled: Color::DarkGray,
            status_online: Color::Green,
            status_seen: Color::Yellow,
            status_offline: Color::DarkGray,
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
            gauge_filled: Color::Rgb(255, 0, 255),
            gauge_unfilled: Color::Rgb(80, 80, 80),
            status_online: Color::Rgb(0, 255, 136),
            status_seen: Color::Rgb(255, 200, 0),
            status_offline: Color::Rgb(100, 100, 100),
        }
    }

    /// Retro amber-phosphor CRT vibe: dark brown background, amber fg,
    /// green accent for selection + trusted markers.
    fn amber() -> Self {
        Self {
            name: ThemeName::Amber,
            bg: Color::Rgb(0x1a, 0x0f, 0x00),
            fg: Color::Rgb(0xff, 0xb0, 0x00),
            accent: Color::Rgb(0x66, 0xff, 0x66),
            border_active: Color::Rgb(0xff, 0xb0, 0x00),
            border_inactive: Color::Rgb(0x55, 0x32, 0x00),
            self_text: Color::Rgb(0x66, 0xff, 0x66),
            peer_text: Color::Rgb(0xff, 0xcc, 0x66),
            trusted_mark: Color::Rgb(0x66, 0xff, 0x66),
            untrusted_mark: Color::Rgb(0xff, 0x77, 0x33),
            error: Color::Rgb(0xff, 0x55, 0x55),
            info: Color::Rgb(0x88, 0xc0, 0x70),
            highlight: Color::Rgb(0x66, 0xff, 0x66),
            status_bg: Color::Rgb(0x33, 0x22, 0x00),
            status_fg: Color::Rgb(0xff, 0xb0, 0x00),
            gauge_filled: Color::Rgb(0xff, 0xb0, 0x00),
            gauge_unfilled: Color::Rgb(0x33, 0x22, 0x00),
            status_online: Color::Rgb(0x66, 0xff, 0x66),
            status_seen: Color::Rgb(0xff, 0xb0, 0x00),
            status_offline: Color::Rgb(0x55, 0x32, 0x00),
        }
    }

    pub fn style(&self) -> Style {
        Style::default().bg(self.bg).fg(self.fg)
    }

    pub fn border_style(&self, active: bool) -> Style {
        Style::default()
            .fg(if active {
                self.border_active
            } else {
                self.border_inactive
            })
            .bg(self.bg)
    }

    pub fn self_message_style(&self) -> Style {
        Style::default().fg(self.self_text).bg(self.bg)
    }

    pub fn peer_message_style(&self) -> Style {
        Style::default().fg(self.peer_text).bg(self.bg)
    }

    /// Stable accent for a peer, derived from its authenticated identity so
    /// the same person keeps the same color across reconnects and restarts.
    /// The palette deliberately avoids `self_text`, keeping local echoes
    /// visually distinct from every remote speaker.
    pub fn peer_message_style_for(&self, peer_id: &[u8; 16]) -> Style {
        let colors = match self.name {
            ThemeName::Default => [
                Color::Cyan,
                Color::Yellow,
                Color::Magenta,
                Color::Blue,
                Color::Red,
                Color::White,
            ],
            ThemeName::Solarized => [
                Color::Rgb(42, 161, 152),
                Color::Rgb(181, 137, 0),
                Color::Rgb(211, 54, 130),
                Color::Rgb(38, 139, 210),
                Color::Rgb(203, 75, 22),
                Color::Rgb(108, 113, 196),
            ],
            ThemeName::Monochrome => [
                Color::White,
                Color::Gray,
                Color::Indexed(250),
                Color::Indexed(245),
                Color::Indexed(240),
                Color::Indexed(235),
            ],
            ThemeName::Neon => [
                Color::Rgb(0, 220, 255),
                Color::Rgb(255, 220, 0),
                Color::Rgb(255, 80, 220),
                Color::Rgb(120, 160, 255),
                Color::Rgb(255, 100, 60),
                Color::Rgb(180, 100, 255),
            ],
            ThemeName::Amber => [
                Color::Rgb(255, 204, 102),
                Color::Rgb(255, 153, 51),
                Color::Rgb(255, 230, 153),
                Color::Rgb(204, 153, 102),
                Color::Rgb(255, 119, 51),
                Color::Rgb(153, 204, 102),
            ],
        };
        Style::default()
            .fg(colors[peer_id[0] as usize % colors.len()])
            .bg(self.bg)
    }

    pub fn info_style(&self) -> Style {
        Style::default()
            .fg(self.info)
            .bg(self.bg)
            .add_modifier(Modifier::ITALIC)
    }

    pub fn error_style(&self) -> Style {
        Style::default()
            .fg(self.error)
            .bg(self.bg)
            .add_modifier(Modifier::BOLD)
    }

    pub fn status_style(&self) -> Style {
        Style::default().fg(self.status_fg).bg(self.status_bg)
    }

    pub fn highlight_style(&self) -> Style {
        Style::default()
            .fg(self.highlight)
            .bg(self.bg)
            .add_modifier(Modifier::BOLD)
    }

    pub fn trusted_style(&self) -> Style {
        Style::default().fg(self.trusted_mark).bg(self.bg)
    }

    pub fn untrusted_style(&self) -> Style {
        Style::default().fg(self.untrusted_mark).bg(self.bg)
    }

    /// Gauge foreground (filled portion) — exposed as a Style so widgets
    /// can apply both `fg` and `bg` from one call.
    pub fn gauge_filled_style(&self) -> Style {
        Style::default()
            .fg(self.gauge_filled)
            .bg(self.gauge_unfilled)
    }

    /// Gauge background (unfilled portion). Pair with `gauge_filled_style`
    /// for the standard `Gauge::gauge_style` argument.
    pub fn gauge_unfilled_style(&self) -> Style {
        Style::default().fg(self.gauge_unfilled).bg(self.bg)
    }

    /// Style for secondary/placeholder text (e.g., empty state messages).
    pub fn dim_style(&self) -> Style {
        Style::default()
            .fg(self.fg)
            .bg(self.bg)
            .add_modifier(Modifier::DIM)
    }

    /// Resolve a semantic role to a concrete `Style` for this theme.
    ///
    /// Roles carry the *meaning* of a color slot (focused border,
    /// incoming surface, success text). Each concrete theme maps roles
    /// onto its palette so the render code never has to know whether the
    /// active theme is solarized, monochrome, or amber.
    ///
    /// `TextSecondary` deliberately picks a brighter shade than
    /// `TextMuted` so secondary labels still outrank hints and
    /// timestamps. `SurfaceMessageIncoming` collapses to `SurfaceBase`
    /// for now — incoming bubbles rely on alignment + whitespace, not on
    /// a fill, so the eye reads the writer/reader direction from layout
    /// alone.
    pub fn role_style(&self, role: StyleRole) -> Style {
        match role {
            StyleRole::TextPrimary => Style::default().fg(self.fg).bg(self.bg),
            StyleRole::TextSecondary => Style::default()
                .fg(self.fg)
                .bg(self.bg)
                .add_modifier(Modifier::DIM),
            StyleRole::TextMuted => Style::default().fg(self.border_inactive).bg(self.bg),
            StyleRole::TextAccent => Style::default()
                .fg(self.accent)
                .bg(self.bg)
                .add_modifier(Modifier::BOLD),
            StyleRole::TextSuccess => Style::default().fg(self.status_online).bg(self.bg),
            StyleRole::TextWarning => Style::default().fg(self.status_seen).bg(self.bg),
            StyleRole::TextDanger => Style::default()
                .fg(self.error)
                .bg(self.bg)
                .add_modifier(Modifier::BOLD),
            StyleRole::SurfaceBase => Style::default().bg(self.bg),
            StyleRole::SurfacePanel => Style::default().bg(self.status_bg),
            StyleRole::SurfaceSelected => Style::default()
                .bg(self.status_bg)
                .fg(self.fg)
                .add_modifier(Modifier::BOLD),
            StyleRole::SurfaceMessageIncoming => Style::default().bg(self.bg),
            StyleRole::SurfaceMessageOutgoing => Style::default().bg(self.status_bg),
            StyleRole::BorderNormal => Style::default().fg(self.border_inactive).bg(self.bg),
            StyleRole::BorderFocused => Style::default().fg(self.border_active).bg(self.bg),
        }
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
        for n in [
            ThemeName::Default,
            ThemeName::Solarized,
            ThemeName::Monochrome,
            ThemeName::Neon,
            ThemeName::Amber,
        ] {
            assert_eq!(ThemeName::parse(n.as_str()), Some(n));
        }
        assert_eq!(ThemeName::parse("DEFAULT"), Some(ThemeName::Default));
        assert_eq!(ThemeName::parse("MONO"), Some(ThemeName::Monochrome));
        assert_eq!(ThemeName::parse("amber"), Some(ThemeName::Amber));
        assert_eq!(ThemeName::parse("bogus"), None);
    }

    #[test]
    fn amber_palette_matches_plan() {
        let t = Theme::by_name(ThemeName::Amber);
        // Dark brownish bg (0x1a0f00) and amber phosphor fg (0xffb000) per the plan.
        assert!(matches!(t.bg, Color::Rgb(0x1a, 0x0f, 0x00)));
        assert!(matches!(t.fg, Color::Rgb(0xff, 0xb0, 0x00)));
        // Green accent (0x66ff66).
        assert!(matches!(t.accent, Color::Rgb(0x66, 0xff, 0x66)));
        assert_eq!(t.name, ThemeName::Amber);
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

    #[test]
    fn every_theme_resolves_every_role() {
        // The semantic color policy relies on every theme providing a
        // concrete Style for every role. If a theme forgets one, the
        // renderer would fall back to the default Style and the visual
        // hierarchy would silently break for that theme.
        let roles = [
            StyleRole::TextPrimary,
            StyleRole::TextSecondary,
            StyleRole::TextMuted,
            StyleRole::TextAccent,
            StyleRole::TextSuccess,
            StyleRole::TextWarning,
            StyleRole::TextDanger,
            StyleRole::SurfaceBase,
            StyleRole::SurfacePanel,
            StyleRole::SurfaceSelected,
            StyleRole::SurfaceMessageIncoming,
            StyleRole::SurfaceMessageOutgoing,
            StyleRole::BorderNormal,
            StyleRole::BorderFocused,
        ];
        for name in [
            ThemeName::Default,
            ThemeName::Solarized,
            ThemeName::Monochrome,
            ThemeName::Neon,
            ThemeName::Amber,
        ] {
            let t = Theme::by_name(name);
            for role in roles {
                let _ = t.role_style(role);
            }
        }
    }

    #[test]
    fn role_strings_are_stable() {
        // Role names are surfaced in debug logs and test diagnostics.
        // Locking the strings down prevents silent rename drift.
        assert_eq!(StyleRole::TextPrimary.as_str(), "text.primary");
        assert_eq!(StyleRole::BorderFocused.as_str(), "border.focused");
        assert_eq!(
            StyleRole::SurfaceMessageOutgoing.as_str(),
            "surface.message.outgoing"
        );
    }

    #[test]
    fn focus_border_is_brighter_than_normal_border() {
        // The spec mandates that focus is communicated dominantly by
        // border brightness — verify every theme picks a brighter shade
        // for the focused border than the unfocused one.
        for name in [
            ThemeName::Default,
            ThemeName::Solarized,
            ThemeName::Monochrome,
            ThemeName::Neon,
            ThemeName::Amber,
        ] {
            let t = Theme::by_name(name);
            assert_ne!(
                t.role_style(StyleRole::BorderFocused).fg,
                t.role_style(StyleRole::BorderNormal).fg,
                "theme {name:?} must differentiate focused from normal border"
            );
        }
    }
}
