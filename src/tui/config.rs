//! UI configuration loader.
//!
//! Reads a tiny subset of TOML from `<config_dir>/config.toml` (XDG
//! `~/.config/ppexchanger/config.toml` on Linux/macOS,
//! `%APPDATA%\ppexchanger\config.toml` on Windows). We intentionally
//! hand-roll the parser instead of pulling in a TOML crate: the
//! supported grammar is a single `[ui]` table with a few keys, all of
//! which we can parse with a handful of lines.
//!
//! Supported keys under `[ui]`:
//!   theme                  = "default" | "solarized" | "monochrome" | "neon" | "amber"
//!   show_footer            = true | false
//!   mouse                  = true | false
//!   scrollback             = <integer>
//!   notify_sound           = true | false
//!   auto_trust_seen        = true | false
//!   status_format          = "name" | "name+addr" | "off"
//!   narrow_sidebar_below   = <integer>   # column width that triggers Ctrl+B collapse
//!   min_conversation_width = <integer>   # minimum chat pane width before sidebar hides
//!
//! Lines starting with `#` are comments. Unknown keys are silently ignored.
//! Missing file → defaults.

use crate::tui::theme::ThemeName;
use std::fs;
use std::path::Path;

/// Default number of chat messages retained in the scrollback ring buffer.
pub const DEFAULT_SCROLLBACK: usize = 500;
/// Hard cap so a misconfigured file can't request an unbounded buffer.
pub const MAX_SCROLLBACK: usize = 50_000;
/// Terminal column count below which the sidebar should collapse to keep
/// the chat pane usable. Users can still toggle manually with Ctrl+B.
pub const DEFAULT_NARROW_SIDEBAR_BELOW: u16 = 80;
/// Minimum column count the chat pane should retain when the sidebar is
/// visible. Below this the sidebar hides by default regardless of toggle.
pub const DEFAULT_MIN_CONVERSATION_WIDTH: u16 = 40;

/// What the footer status line shows. `Off` hides it entirely; the other
/// two modes are obvious from the name. Persisted as a string in TOML.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum StatusFormat {
    #[default]
    NameOnly,
    NameAddr,
    Off,
}

impl StatusFormat {
    pub fn as_str(self) -> &'static str {
        match self {
            StatusFormat::NameOnly => "name",
            StatusFormat::NameAddr => "name+addr",
            StatusFormat::Off => "off",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "name" => Some(StatusFormat::NameOnly),
            "name+addr" | "name_addr" | "name-addr" => Some(StatusFormat::NameAddr),
            "off" => Some(StatusFormat::Off),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct UiConfig {
    pub theme: ThemeName,
    pub show_footer: bool,
    pub mouse: bool,
    pub scrollback: usize,
    /// Emit a terminal bell on inbound messages. Off by default — most
    /// people don't want their terminal beeping during a quiet chat.
    pub notify_sound: bool,
    pub desktop_notifications: bool,
    pub image_previews: bool,
    /// Mark every newly-discovered peer as trusted instead of untrusted.
    /// Off by default; turning it on is a security regression unless the
    /// network is fully isolated (e.g. a single-room LAN party).
    pub auto_trust_seen: bool,
    /// What the footer status line renders. See `StatusFormat`.
    pub status_format: StatusFormat,
    /// Terminal column width below which the sidebar collapses by default.
    /// The user can still toggle it with Ctrl+B.
    pub narrow_sidebar_below: u16,
    /// Minimum column count reserved for the chat pane when the sidebar is
    /// visible. Below this the layout engine hides the sidebar.
    pub min_conversation_width: u16,
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            theme: ThemeName::Default,
            show_footer: true,
            // Mouse reporting keeps pane clicks and wheel scrolling usable.
            // Native terminal selection remains available through the
            // terminal's Shift-drag bypass; use --no-mouse for pure native
            // selection mode.
            mouse: true,
            scrollback: DEFAULT_SCROLLBACK,
            notify_sound: false,
            desktop_notifications: true,
            image_previews: true,
            auto_trust_seen: false,
            status_format: StatusFormat::NameOnly,
            narrow_sidebar_below: DEFAULT_NARROW_SIDEBAR_BELOW,
            min_conversation_width: DEFAULT_MIN_CONVERSATION_WIDTH,
        }
    }
}

impl UiConfig {
    pub fn load_or_default(path: &Path) -> Self {
        let text = match fs::read_to_string(path) {
            Ok(t) => t,
            Err(_) => return Self::default(),
        };
        Self::parse(&text).unwrap_or_default()
    }

    /// Parse a config string. Returns `None` if the input is structurally
    /// invalid (unterminated string, unclosed table) so callers fall back to
    /// defaults rather than panicking on a hand-edited file.
    pub fn parse(input: &str) -> Option<Self> {
        let mut out = Self::default();
        let mut in_ui = false;
        for raw in input.lines() {
            let line = strip_comment(raw).trim();
            if line.is_empty() {
                continue;
            }
            if line.starts_with('[') && line.ends_with(']') {
                in_ui = line == "[ui]";
                continue;
            }
            if !in_ui {
                continue;
            }
            let (key, value) = line.split_once('=')?;
            let key = key.trim();
            let value = value.trim();
            match key {
                "theme" => {
                    if let Some(v) = unquote(value) {
                        if let Some(t) = ThemeName::parse(&v) {
                            out.theme = t;
                        }
                    }
                }
                "show_footer" => {
                    if let Some(v) =
                        unquote(value).or_else(|| value.parse().ok().map(|b: bool| b.to_string()))
                    {
                        out.show_footer = parse_bool(&v).unwrap_or(out.show_footer);
                    }
                }
                "desktop_notifications" => {
                    if let Some(v) = parse_bool(value) {
                        out.desktop_notifications = v;
                    }
                }
                "image_previews" => {
                    if let Some(v) = parse_bool(value) {
                        out.image_previews = v;
                    }
                }
                "mouse" => {
                    if let Some(v) = parse_bool(value) {
                        out.mouse = v;
                    }
                }
                "scrollback" => {
                    if let Ok(n) = value.parse::<usize>() {
                        out.scrollback = n.clamp(16, MAX_SCROLLBACK);
                    }
                }
                "notify_sound" => {
                    if let Some(v) = parse_bool(value) {
                        out.notify_sound = v;
                    }
                }
                "auto_trust_seen" => {
                    if let Some(v) = parse_bool(value) {
                        out.auto_trust_seen = v;
                    }
                }
                "status_format" => {
                    if let Some(v) = unquote(value) {
                        if let Some(f) = StatusFormat::parse(&v) {
                            out.status_format = f;
                        }
                    }
                }
                "narrow_sidebar_below" => {
                    if let Ok(n) = value.parse::<u16>() {
                        out.narrow_sidebar_below = n.clamp(40, 240);
                    }
                }
                "min_conversation_width" => {
                    if let Ok(n) = value.parse::<u16>() {
                        out.min_conversation_width = n.clamp(20, 200);
                    }
                }
                _ => {} // unknown key — ignore
            }
        }
        Some(out)
    }

    /// Emit the canonical TOML form. Used by both `/theme` and the
    /// reset-to-defaults flow. The header comment notes the file is
    /// auto-generated so the user knows hand-edits above the `[ui]`
    /// header survive a rewrite.
    pub fn to_toml(&self) -> String {
        let mut out =
            String::from("# ppexchanger UI config — generated, edits preserved on next /theme\n");
        out.push_str("[ui]\n");
        out.push_str(&format!("theme = \"{}\"\n", self.theme.as_str()));
        out.push_str(&format!("show_footer = {}\n", self.show_footer));
        out.push_str(&format!("mouse = {}\n", self.mouse));
        out.push_str(&format!("scrollback = {}\n", self.scrollback));
        out.push_str(&format!("notify_sound = {}\n", self.notify_sound));
        out.push_str(&format!(
            "desktop_notifications = {}\n",
            self.desktop_notifications
        ));
        out.push_str(&format!("image_previews = {}\n", self.image_previews));
        out.push_str(&format!("auto_trust_seen = {}\n", self.auto_trust_seen));
        out.push_str(&format!(
            "status_format = \"{}\"\n",
            self.status_format.as_str()
        ));
        out.push_str(&format!(
            "narrow_sidebar_below = {}\n",
            self.narrow_sidebar_below
        ));
        out.push_str(&format!(
            "min_conversation_width = {}\n",
            self.min_conversation_width
        ));
        out
    }
}

fn strip_comment(line: &str) -> &str {
    // Comments are `#` to end-of-line, but `#` inside a quoted value must be
    // preserved. We only strip when the `#` appears outside quotes.
    let bytes = line.as_bytes();
    let mut in_quote = false;
    for (i, &b) in bytes.iter().enumerate() {
        match b {
            b'"' => in_quote = !in_quote,
            b'#' if !in_quote => return &line[..i],
            _ => {}
        }
    }
    line
}

fn unquote(s: &str) -> Option<String> {
    let s = s.trim();
    if s.starts_with('"') && s.ends_with('"') && s.len() >= 2 {
        Some(s[1..s.len() - 1].to_string())
    } else {
        None
    }
}

fn parse_bool(s: &str) -> Option<bool> {
    let s = s.trim().to_ascii_lowercase();
    match s.as_str() {
        "true" | "yes" | "on" | "1" => Some(true),
        "false" | "no" | "off" | "0" => Some(false),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_when_empty() {
        let c = UiConfig::parse("").unwrap();
        assert_eq!(c.theme, ThemeName::Default);
        assert!(c.show_footer);
        // Mouse interaction is enabled by default; Shift-drag remains the
        // terminal-native selection escape hatch.
        assert!(c.mouse);
        assert_eq!(c.scrollback, DEFAULT_SCROLLBACK);
        // v0.5.0 settings: all the new toggles default to off / conservative.
        assert!(!c.notify_sound);
        assert!(!c.auto_trust_seen);
        assert_eq!(c.status_format, StatusFormat::NameOnly);
    }

    #[test]
    fn parses_ui_block() {
        let toml = r#"
            # this is a comment
            [ui]
            theme = "neon"
            show_footer = false
            mouse = off
            scrollback = 1024
        "#;
        let c = UiConfig::parse(toml).unwrap();
        assert_eq!(c.theme, ThemeName::Neon);
        assert!(!c.show_footer);
        assert!(!c.mouse);
        assert_eq!(c.scrollback, 1024);
    }

    #[test]
    fn ignores_unknown_keys_and_other_tables() {
        let toml = r#"
            [net]
            something = "else"

            [ui]
            theme = "solarized"
            unknown_key = 42
        "#;
        let c = UiConfig::parse(toml).unwrap();
        assert_eq!(c.theme, ThemeName::Solarized);
    }

    #[test]
    fn clamps_scrollback() {
        let c = UiConfig::parse("[ui]\nscrollback = 5\n").unwrap();
        assert_eq!(c.scrollback, 16);
        let c = UiConfig::parse("[ui]\nscrollback = 999999\n").unwrap();
        assert_eq!(c.scrollback, MAX_SCROLLBACK);
    }

    #[test]
    fn comment_with_hash_inside_quoted_value_is_preserved() {
        // The hash inside the quoted theme value must NOT strip the rest;
        // the outer comment is dropped. Since the inner value isn't a known
        // theme, the field stays at its default.
        let toml = r#"[ui]
            theme = "so#larized"   # tail comment
        "#;
        let c = UiConfig::parse(toml).unwrap();
        assert_eq!(c.theme, ThemeName::Default);
    }

    #[test]
    fn roundtrip_load_or_default() {
        // Write a config with the same shape our main() emitter produces,
        // parse it back, and verify every field survived.
        let tmp = std::env::temp_dir().join("ppexchanger-test-config.toml");
        let _ = std::fs::remove_file(&tmp);
        if let Some(p) = tmp.parent() {
            std::fs::create_dir_all(p).ok();
        }
        let body = "\
# generated\n\
[ui]\n\
theme = \"neon\"\n\
show_footer = false\n\
mouse = false\n\
scrollback = 1024\n";
        std::fs::write(&tmp, body).unwrap();
        let loaded = UiConfig::load_or_default(&tmp);
        assert_eq!(loaded.theme, ThemeName::Neon);
        assert!(!loaded.show_footer);
        assert!(!loaded.mouse);
        assert_eq!(loaded.scrollback, 1024);
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn parses_new_v050_fields() {
        let toml = r#"
            [ui]
            notify_sound = true
            auto_trust_seen = yes
            status_format = "name+addr"
        "#;
        let c = UiConfig::parse(toml).unwrap();
        assert!(c.notify_sound);
        assert!(c.auto_trust_seen);
        assert_eq!(c.status_format, StatusFormat::NameAddr);
    }

    #[test]
    fn parses_status_format_off_and_name_only() {
        let c = UiConfig::parse("[ui]\nstatus_format = \"off\"\n").unwrap();
        assert_eq!(c.status_format, StatusFormat::Off);
        let c = UiConfig::parse("[ui]\nstatus_format = \"name\"\n").unwrap();
        assert_eq!(c.status_format, StatusFormat::NameOnly);
    }

    #[test]
    fn status_format_unknown_falls_back_to_default() {
        // An unknown string is ignored — the field stays at the default.
        let c = UiConfig::parse("[ui]\nstatus_format = \"bogus\"\n").unwrap();
        assert_eq!(c.status_format, StatusFormat::NameOnly);
    }

    #[test]
    fn to_toml_roundtrip_through_parse() {
        let original = UiConfig {
            theme: ThemeName::Solarized,
            show_footer: false,
            mouse: false,
            scrollback: 2048,
            notify_sound: true,
            desktop_notifications: false,
            image_previews: false,
            auto_trust_seen: false,
            status_format: StatusFormat::NameAddr,
            narrow_sidebar_below: 72,
            min_conversation_width: 32,
        };
        let toml = original.to_toml();
        let parsed = UiConfig::parse(&toml).expect("self-emitted TOML must parse");
        assert_eq!(parsed.desktop_notifications, original.desktop_notifications);
        assert_eq!(parsed.image_previews, original.image_previews);
        assert_eq!(parsed.theme, original.theme);
        assert_eq!(parsed.show_footer, original.show_footer);
        assert_eq!(parsed.mouse, original.mouse);
        assert_eq!(parsed.scrollback, original.scrollback);
        assert_eq!(parsed.notify_sound, original.notify_sound);
        assert_eq!(parsed.auto_trust_seen, original.auto_trust_seen);
        assert_eq!(parsed.status_format, original.status_format);
        assert_eq!(parsed.narrow_sidebar_below, original.narrow_sidebar_below);
        assert_eq!(
            parsed.min_conversation_width,
            original.min_conversation_width
        );
    }

    #[test]
    fn parses_responsive_layout_keys() {
        let toml = r#"
            [ui]
            narrow_sidebar_below = 72
            min_conversation_width = 32
        "#;
        let c = UiConfig::parse(toml).unwrap();
        assert_eq!(c.narrow_sidebar_below, 72);
        assert_eq!(c.min_conversation_width, 32);
    }

    #[test]
    fn responsive_keys_clamps_out_of_range_values() {
        // Anything outside the safety band falls back to the closest
        // sane value — a misconfigured config file must not silently
        // request a zero-width chat pane.
        let c = UiConfig::parse("[ui]\nnarrow_sidebar_below = 0\n").unwrap();
        assert_eq!(c.narrow_sidebar_below, 40);
        let c = UiConfig::parse("[ui]\nnarrow_sidebar_below = 500\n").unwrap();
        assert_eq!(c.narrow_sidebar_below, 240);
        let c = UiConfig::parse("[ui]\nmin_conversation_width = 0\n").unwrap();
        assert_eq!(c.min_conversation_width, 20);
    }

    #[test]
    fn responsive_keys_default_when_absent() {
        let c = UiConfig::parse("[ui]\ntheme = \"neon\"\n").unwrap();
        assert_eq!(c.narrow_sidebar_below, DEFAULT_NARROW_SIDEBAR_BELOW);
        assert_eq!(c.min_conversation_width, DEFAULT_MIN_CONVERSATION_WIDTH);
    }
}
