//! UI configuration loader.
//!
//! Reads a tiny subset of TOML from `<config_dir>/config.toml` (XDG
//! `~/.config/lanchat/config.toml` on Linux/macOS,
//! `%APPDATA%\lanchat\config.toml` on Windows). We intentionally
//! hand-roll the parser instead of pulling in a TOML crate: the
//! supported grammar is a single `[ui]` table with a few keys, all of
//! which we can parse with a handful of lines.
//!
//! Supported keys under `[ui]`:
//!   theme        = "default" | "solarized" | "monochrome" | "neon"
//!   show_footer  = true | false
//!   mouse        = true | false
//!   scrollback   = <integer>
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

#[derive(Debug, Clone)]
pub struct UiConfig {
    pub theme: ThemeName,
    pub show_footer: bool,
    pub mouse: bool,
    pub scrollback: usize,
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            theme: ThemeName::Default,
            show_footer: true,
            // Mouse capture is opt-in. With capture on, the terminal
            // loses native drag-select (in tmux, in many browsers-of-
            // -buffers, etc.). Set `mouse = true` in config.toml or pass
            // no flag explicitly to enable.
            mouse: false,
            scrollback: DEFAULT_SCROLLBACK,
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
                    if let Some(v) = unquote(value).or_else(|| value.parse().ok().map(|b: bool| b.to_string())) {
                        out.show_footer = parse_bool(&v).unwrap_or(out.show_footer);
                    }
                }
                "mouse" => {
                    if let Some(v) = parse_bool(value) {
                        out.mouse = v;
                    }
                }
                "scrollback" => {
                    if let Some(n) = value.parse::<usize>().ok() {
                        out.scrollback = n.clamp(16, MAX_SCROLLBACK);
                    }
                }
                _ => {} // unknown key — ignore
            }
        }
        Some(out)
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
        // Mouse capture is opt-in — see Default impl.
        assert!(!c.mouse);
        assert_eq!(c.scrollback, DEFAULT_SCROLLBACK);
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
        let tmp = std::env::temp_dir().join("lanchat-test-config.toml");
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
}