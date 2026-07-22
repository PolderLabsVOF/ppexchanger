//! Line editor + shortcut dispatch.
//!
//! The editor handles character entry, backspace, and Enter; arrow keys move
//! the cursor within the buffer (no in-buffer cursor stored yet — Up/Down
//! scroll message history when the buffer is empty, otherwise they let the
//! terminal handle the keys normally). Ctrl-C clears the buffer and emits a
//! sentinel that the main loop turns into `Action::Quit`.
//!
//! Other shortcuts (Tab focus, Ctrl-N new chat, Ctrl-T trust, Ctrl-R revoke,
//! Ctrl-Q quit, Ctrl-L clear, Esc cancel, PageUp/PageDown scrollback, ? help)
//! are emitted as `EditorEvent` values so the main loop can decide which ones
//! need an action vs. a UI-only effect.

use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};

#[derive(Debug, PartialEq, Eq)]
pub enum EditorEvent {
    /// User pressed Enter with non-empty buffer.
    Submit(String),
    /// Ctrl-C — main loop should quit.
    Cancel,
    /// Tab — cycle focus between sidebar and chat.
    FocusNext,
    /// Up arrow with empty buffer — recall previous input from history.
    HistoryPrev,
    /// Down arrow with empty buffer — recall next input from history.
    HistoryNext,
    /// Up/Down with non-empty buffer — ignored (let terminal handle).
    None,
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
                _ => {}
            }
        }
        if *modifiers == KeyModifiers::NONE {
            match code {
                KeyCode::Tab => return EditorEvent::FocusNext,
                KeyCode::Esc => {
                    self.clear();
                    return EditorEvent::Clear;
                }
                KeyCode::BackTab => return EditorEvent::FocusNext,
                KeyCode::PageUp => return EditorEvent::PageUp,
                KeyCode::PageDown => return EditorEvent::PageDown,
                KeyCode::Char('?')
                    if self.buffer.is_empty() && !self.history_idx.is_some() =>
                {
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
                    EditorEvent::None
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
                    EditorEvent::None
                }
            }
            KeyCode::Down => {
                if self.buffer.is_empty() {
                    self.recall_history(1)
                } else {
                    EditorEvent::None
                }
            }
            KeyCode::Char(c) => {
                self.buffer.push(*c);
                EditorEvent::Edited
            }
            _ => EditorEvent::None,
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
}