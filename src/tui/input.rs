//! Line editor + shortcut dispatch.
//!
//! The editor handles character entry, backspace, and Enter; arrow keys move
//! the cursor within the buffer (no in-buffer cursor stored yet — Up/Down
//! scroll message history when the buffer is empty, otherwise they let the
//! terminal handle the keys normally). Ctrl-C clears the buffer and emits a
//! sentinel that the main loop turns into `Action::Quit`.
//!
//! Other shortcuts (Tab focus, Ctrl-N new chat, Ctrl-T trust, Ctrl-R revoke,
//! Ctrl-Q quit, Ctrl-L clear, Esc cancel, PageUp/PageDown scrollback, ? help,
//! Ctrl-B toggle sidebar, Ctrl-P open peer picker) are emitted as
//! `EditorEvent` values so the main loop can decide which ones need an
//! action vs. a UI-only effect.

use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};

#[derive(Debug, PartialEq, Eq)]
pub enum EditorEvent {
    /// User pressed Enter with non-empty buffer.
    Submit(String),
    /// Enter on an empty composer. The main loop uses this to open the
    /// currently highlighted peer when the sidebar has focus.
    ActivateSelection,
    /// Ctrl-C — main loop should quit.
    Cancel,
    /// Tab — cycle focus between sidebar and chat.
    FocusNext,
    /// Up arrow with empty buffer — recall previous input from history.
    HistoryPrev,
    /// Down arrow with empty buffer — recall next input from history.
    HistoryNext,
    /// No editor action was needed for the key event.
    None,
    /// Up/Down while the peers pane has focus. Kept separate from command
    /// history so a non-empty composer never gets mutated by navigation.
    PeerPrev,
    PeerNext,
    /// Esc — cancel current input.
    Clear,
    /// Ctrl-L — clear input buffer.
    ClearInput,
    /// Ctrl-Q — quit immediately.
    Quit,
    /// Ctrl-N — open "new chat" prompt (focuses peer input).
    NewChat,
    /// Ctrl-T — toggle trust on selected peer.
    ToggleTrust,
    /// Ctrl-R — revoke selected peer.
    RevokePeer,
    /// PageUp — scroll chat back.
    PageUp,
    /// PageDown — scroll chat forward.
    PageDown,
    /// `?` — toggle help overlay.
    ToggleHelp,
    /// `Ctrl-,` — open the settings popup.
    OpenSettings,
    /// Ctrl-B — toggle the sidebar visibility. UI-only; the render pass
    /// reads `state.sidebar_hidden` to decide whether to allocate room
    /// for it.
    ToggleSidebar,
    /// Ctrl-P — open the temporary peer picker overlay. Used in narrow
    /// terminals where the sidebar is collapsed by default.
    OpenPeerPicker,
    /// A click landed on one of the top menu buttons. The menu is the
    /// only entry point that needs the closure payload (so the handler
    /// in main.rs can route Settings → open_settings, Quit → quit,
    /// etc.) without growing the variant enum.
    MenuAction(crate::tui::MenuAction),
    /// A printable character was added (or backspace).
    Edited,
}

#[derive(Default)]
pub struct LineEditor {
    pub buffer: String,
    /// Last N submitted lines, newest at back.
    history: VecDeque<String>,
    /// Current history-cursor position. `None` means we're typing fresh.
    history_idx: Option<usize>,
}

use std::collections::VecDeque;

const HISTORY_CAP: usize = 64;

/// Commands surfaced by the inline command palette. Keep this catalogue next
/// to completion so the preview and Tab behavior can never drift apart.
pub const COMMANDS: &[(&str, &str)] = &[
    ("/discover", "find peers on the local network"),
    ("/map", "toggle the discovery peer map"),
    ("/peers", "show known peers"),
    ("/trust", "trust a peer: /trust <name>"),
    ("/revoke", "revoke a peer: /revoke <name>"),
    ("/theme", "change the color theme"),
    ("/send", "send a file: /send <path>"),
    ("/settings", "open settings"),
    ("/clear", "clear all chat messages"),
    ("/quit", "quit ppexchanger"),
];

pub fn command_matches(input: &str) -> Vec<(&'static str, &'static str)> {
    let query = input.split_whitespace().next().unwrap_or(input);
    COMMANDS
        .iter()
        .copied()
        .filter(|(command, _)| command.starts_with(query))
        .collect()
}

/// Hard cap on a single pasted payload, in bytes. 1 MiB is well above
/// any sane paste (a screenshot's worth of text, a config file dump)
/// but small enough that a "I pasted an entire log file by accident"
/// doesn't OOM the UI thread. If the cap is hit, the paste is dropped
/// entirely — the editor stays usable.
const PASTE_MAX: usize = 1024 * 1024;

impl LineEditor {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn as_str(&self) -> &str {
        &self.buffer
    }

    pub fn clear(&mut self) {
        self.buffer.clear();
        self.history_idx = None;
    }

    /// Append a pasted string to the buffer. Called by the main loop on
    /// `Event::Paste(String)`. Returns `EditorEvent::Edited` so the
    /// status line redraws. If the paste would overflow `PASTE_MAX`,
    /// the entire paste is dropped and the existing buffer is left
    /// untouched — the editor never blocks on user input.
    pub fn on_paste(&mut self, text: &str) -> EditorEvent {
        if text.len() > PASTE_MAX || self.buffer.len() + text.len() > PASTE_MAX {
            return EditorEvent::None;
        }
        self.buffer.push_str(text);
        EditorEvent::Edited
    }

    /// Handle one key event. Returns the editor event describing what
    /// happened; the caller decides which events become UI side-effects vs.
    /// `Action` messages on the bus.
    pub fn on_key(&mut self, ev: &Event) -> EditorEvent {
        let Event::Key(KeyEvent {
            code,
            modifiers,
            kind,
            ..
        }) = ev
        else {
            return EditorEvent::None;
        };
        // Ignore key-release events so a held key doesn't double-fire.
        if !matches!(kind, crossterm::event::KeyEventKind::Press) {
            return EditorEvent::None;
        }

        // Ctrl-modified shortcuts first.
        // Terminals commonly encode Ctrl+Backspace as Ctrl-H (ASCII
        // backspace), so handle both representations before the printable
        // character branch below. Ctrl-W is accepted as the familiar shell
        // word-delete alternative as well.
        if modifiers.contains(KeyModifiers::CONTROL)
            && matches!(code, KeyCode::Backspace | KeyCode::Char('h') | KeyCode::Char('w'))
        {
            self.delete_word_backwards();
            return EditorEvent::Edited;
        }
        if *modifiers == KeyModifiers::CONTROL {
            match code {
                KeyCode::Char('c') => {
                    self.clear();
                    return EditorEvent::Cancel;
                }
                KeyCode::Char('l') => {
                    self.clear();
                    return EditorEvent::ClearInput;
                }
                KeyCode::Char('q') => return EditorEvent::Quit,
                KeyCode::Char('n') => return EditorEvent::NewChat,
                KeyCode::Char('t') => return EditorEvent::ToggleTrust,
                KeyCode::Char('r') => return EditorEvent::RevokePeer,
                KeyCode::Char(',') => return EditorEvent::OpenSettings,
                KeyCode::Char('b') => return EditorEvent::ToggleSidebar,
                KeyCode::Char('p') => return EditorEvent::OpenPeerPicker,
                _ => {}
            }
        }
        if *modifiers == KeyModifiers::NONE {
            match code {
                KeyCode::Tab if self.buffer.starts_with('/') => return self.complete_command(),
                KeyCode::Tab => return EditorEvent::FocusNext,
                KeyCode::Esc => {
                    self.clear();
                    return EditorEvent::Clear;
                }
                KeyCode::BackTab => return EditorEvent::FocusNext,
                KeyCode::PageUp => return EditorEvent::PageUp,
                KeyCode::PageDown => return EditorEvent::PageDown,
                KeyCode::Char('?') if self.buffer.is_empty() && self.history_idx.is_none() => {
                    return EditorEvent::ToggleHelp;
                }
                _ => {}
            }
        }

        match code {
            KeyCode::Enter => {
                let out = std::mem::take(&mut self.buffer);
                self.history_idx = None;
                if !out.is_empty() {
                    self.push_history(&out);
                }
                if out.is_empty() {
                    EditorEvent::ActivateSelection
                } else {
                    EditorEvent::Submit(out)
                }
            }
            KeyCode::Backspace => {
                self.buffer.pop();
                EditorEvent::Edited
            }
            KeyCode::Up => {
                if self.buffer.is_empty() {
                    self.recall_history(-1)
                } else {
                    EditorEvent::PeerPrev
                }
            }
            KeyCode::Down => {
                if self.buffer.is_empty() {
                    self.recall_history(1)
                } else {
                    EditorEvent::PeerNext
                }
            }
            KeyCode::Char(c) => {
                self.buffer.push(*c);
                EditorEvent::Edited
            }
            _ => EditorEvent::None,
        }
    }

    /// Delete the word immediately before the insertion point. The editor
    /// currently appends at the end of the buffer, so this operates from the
    /// end while respecting UTF-8 character boundaries.
    fn delete_word_backwards(&mut self) {
        while self.buffer.chars().last().is_some_and(char::is_whitespace) {
            self.buffer.pop();
        }
        while self
            .buffer
            .chars()
            .last()
            .is_some_and(|character| !character.is_whitespace())
        {
            self.buffer.pop();
        }
    }

    fn push_history(&mut self, line: &str) {
        // Skip if identical to last entry — avoids spamming history with
        // repeated sends.
        if self.history.back().map(|s| s.as_str()) == Some(line) {
            return;
        }
        if self.history.len() == HISTORY_CAP {
            self.history.pop_front();
        }
        self.history.push_back(line.to_string());
    }

    fn complete_command(&mut self) -> EditorEvent {
        let command_end = self
            .buffer
            .find(char::is_whitespace)
            .unwrap_or(self.buffer.len());
        let query = self.buffer[..command_end].to_string();
        let Some((command, _)) = command_matches(&query).first().copied() else {
            return EditorEvent::None;
        };
        let suffix = self.buffer[command_end..].to_string();
        self.buffer = format!("{}{}", command, suffix);
        if suffix.is_empty() && command != query {
            self.buffer.push(' ');
        }
        EditorEvent::Edited
    }

    /// Step the history cursor. `delta == -1` is older, `+1` is newer.
    fn recall_history(&mut self, delta: i32) -> EditorEvent {
        if self.history.is_empty() {
            return EditorEvent::None;
        }
        let next = match self.history_idx {
            None if delta < 0 => (self.history.len() as i32 - 1) as usize,
            None => return EditorEvent::None,
            Some(i) => (i as i32 + delta).clamp(0, self.history.len() as i32 - 1) as usize,
        };
        self.history_idx = Some(next);
        self.buffer = self.history[next].clone();
        EditorEvent::HistoryPrev
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyEventKind;

    #[allow(dead_code)] // helper used by future paste-handling tests
    fn paste_event(text: &str) -> Event {
        Event::Paste(text.to_string())
    }

    fn press(code: KeyCode, modifiers: KeyModifiers) -> Event {
        Event::Key(KeyEvent {
            code,
            modifiers,
            kind: KeyEventKind::Press,
            state: crossterm::event::KeyEventState::NONE,
        })
    }

    #[test]
    fn enter_submits_and_remembers_history() {
        let mut ed = LineEditor::new();
        ed.on_key(&press(KeyCode::Char('h'), KeyModifiers::NONE));
        ed.on_key(&press(KeyCode::Char('i'), KeyModifiers::NONE));
        let ev = ed.on_key(&press(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(ev, EditorEvent::Submit("hi".into()));
        assert!(ed.buffer.is_empty());

        // Up arrow on empty buffer recalls last history entry.
        let _ = ed.on_key(&press(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(ed.buffer, "hi");
    }

    #[test]
    fn empty_enter_activates_peer_selection() {
        let mut ed = LineEditor::new();
        assert_eq!(
            ed.on_key(&press(KeyCode::Enter, KeyModifiers::NONE)),
            EditorEvent::ActivateSelection
        );
    }

    #[test]
    fn arrow_navigation_does_not_mutate_nonempty_composer() {
        let mut ed = LineEditor::new();
        ed.buffer = "draft".into();
        assert_eq!(
            ed.on_key(&press(KeyCode::Up, KeyModifiers::NONE)),
            EditorEvent::PeerPrev
        );
        assert_eq!(ed.buffer, "draft");
    }

    #[test]
    fn ctrl_c_cancels() {
        let mut ed = LineEditor::new();
        ed.on_key(&press(KeyCode::Char('x'), KeyModifiers::NONE));
        let ev = ed.on_key(&press(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert_eq!(ev, EditorEvent::Cancel);
        assert!(ed.buffer.is_empty());
    }

    #[test]
    fn tab_focuses_next() {
        let mut ed = LineEditor::new();
        let ev = ed.on_key(&press(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(ev, EditorEvent::FocusNext);
    }

    #[test]
    fn tab_completes_a_slash_command() {
        let mut ed = LineEditor::new();
        ed.buffer = "/disc".into();
        assert_eq!(ed.on_key(&press(KeyCode::Tab, KeyModifiers::NONE)), EditorEvent::Edited);
        assert_eq!(ed.buffer, "/discover ");
        assert_eq!(command_matches("/se")[0].0, "/send");
    }

    #[test]
    fn pageup_pagedown_pass_through() {
        let mut ed = LineEditor::new();
        assert_eq!(
            ed.on_key(&press(KeyCode::PageUp, KeyModifiers::NONE)),
            EditorEvent::PageUp
        );
        assert_eq!(
            ed.on_key(&press(KeyCode::PageDown, KeyModifiers::NONE)),
            EditorEvent::PageDown
        );
    }

    #[test]
    fn question_mark_toggles_help_only_when_empty() {
        let mut ed = LineEditor::new();
        assert_eq!(
            ed.on_key(&press(KeyCode::Char('?'), KeyModifiers::NONE)),
            EditorEvent::ToggleHelp
        );
        let mut ed = LineEditor::new();
        ed.on_key(&press(KeyCode::Char('a'), KeyModifiers::NONE));
        let ev = ed.on_key(&press(KeyCode::Char('?'), KeyModifiers::NONE));
        // '?' is appended normally because the buffer is non-empty.
        assert_eq!(ev, EditorEvent::Edited);
        assert_eq!(ed.buffer, "a?");
    }

    #[test]
    fn ctrl_shortcuts() {
        let mut ed = LineEditor::new();
        assert_eq!(
            ed.on_key(&press(KeyCode::Char('q'), KeyModifiers::CONTROL)),
            EditorEvent::Quit
        );
        assert_eq!(
            ed.on_key(&press(KeyCode::Char('n'), KeyModifiers::CONTROL)),
            EditorEvent::NewChat
        );
        assert_eq!(
            ed.on_key(&press(KeyCode::Char('t'), KeyModifiers::CONTROL)),
            EditorEvent::ToggleTrust
        );
    }

    #[test]
    fn paste_appends_to_buffer() {
        let mut ed = LineEditor::new();
        ed.on_key(&press(KeyCode::Char('h'), KeyModifiers::NONE));
        assert_eq!(ed.on_paste("ello world"), EditorEvent::Edited);
        assert_eq!(ed.as_str(), "hello world");
    }

    #[test]
    fn paste_drops_over_cap() {
        let mut ed = LineEditor::new();
        // A paste >1 MiB is dropped without disturbing existing buffer.
        let huge = "x".repeat(PASTE_MAX + 1);
        let ev = ed.on_paste(&huge);
        assert_eq!(ev, EditorEvent::None);
        assert!(ed.as_str().is_empty());
    }

    #[test]
    fn paste_drops_when_buffer_would_overflow() {
        let mut ed = LineEditor::new();
        // Pre-fill close to the cap, then paste something that pushes over.
        let prefix = "a".repeat(PASTE_MAX - 10);
        ed.buffer.push_str(&prefix);
        let ev = ed.on_paste("bbbbbbbbbbbbb"); // +13 bytes, overflows
        assert_eq!(ev, EditorEvent::None);
        // Buffer is untouched (no partial state).
        assert_eq!(ed.buffer.len(), PASTE_MAX - 10);
    }

    #[test]
    fn ctrl_backspace_deletes_a_word_without_inserting_h() {
        let mut ed = LineEditor::new();
        ed.buffer = "hello world".into();
        assert_eq!(
            ed.on_key(&press(KeyCode::Backspace, KeyModifiers::CONTROL)),
            EditorEvent::Edited
        );
        assert_eq!(ed.buffer, "hello ");

        // Some terminals report Ctrl+Backspace as Ctrl-H.
        assert_eq!(
            ed.on_key(&press(KeyCode::Char('h'), KeyModifiers::CONTROL)),
            EditorEvent::Edited
        );
        assert!(ed.buffer.is_empty());
    }

    #[test]
    fn ctrl_b_toggles_sidebar() {
        let mut ed = LineEditor::new();
        assert_eq!(
            ed.on_key(&press(KeyCode::Char('b'), KeyModifiers::CONTROL)),
            EditorEvent::ToggleSidebar
        );
        // Buffer is untouched — sidebar toggle is a UI-only effect.
        assert!(ed.buffer.is_empty());
    }

    #[test]
    fn ctrl_p_opens_peer_picker() {
        let mut ed = LineEditor::new();
        assert_eq!(
            ed.on_key(&press(KeyCode::Char('p'), KeyModifiers::CONTROL)),
            EditorEvent::OpenPeerPicker
        );
        assert!(ed.buffer.is_empty());
    }
}
