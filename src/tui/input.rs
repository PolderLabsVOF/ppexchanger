//! Minimal line editor used by the TUI: a `String` buffer that responds to
//! key events. Plain ASCII / UTF-8 passthrough; no arrow-key history yet.
//!
//! ponytail: arrow-key history can be added by storing a `VecDeque<String>`
//! and rotating on Up/Down. Keeping it out for v1 — every keystroke is one
//! syscall and the chat app has nothing more to navigate.

use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};

#[derive(Default)]
pub struct LineEditor {
    pub buffer: String,
}

impl LineEditor {
    pub fn new() -> Self {
        Self::default()
    }

    /// Handle one key event. Returns `Some(text)` when the user pressed
    /// Enter (and clears the buffer). Returns `None` otherwise.
    pub fn on_key(&mut self, ev: &Event) -> Option<String> {
        if let Event::Key(KeyEvent { code, modifiers, .. }) = ev {
            // Ctrl-C: signal quit by returning a sentinel. Callers map this to Quit.
            if *modifiers == KeyModifiers::CONTROL && *code == KeyCode::Char('c') {
                self.buffer.clear();
                return Some("\x03".into());
            }
            match code {
                KeyCode::Enter => {
                    let out = std::mem::take(&mut self.buffer);
                    return Some(out);
                }
                KeyCode::Backspace => {
                    self.buffer.pop();
                }
                KeyCode::Char(c) => {
                    self.buffer.push(*c);
                }
                _ => {}
            }
        }
        None
    }

    pub fn as_str(&self) -> &str {
        &self.buffer
    }
}