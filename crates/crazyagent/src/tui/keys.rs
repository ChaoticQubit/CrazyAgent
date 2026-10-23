use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

/// One key press after normalization, the only key type screens see.
/// Input: built from a crossterm event by `Key::from_event`, or from a paste.
/// Output: plain variants; Ctrl letters are lowercase, Shift-arrows are named.
#[derive(Clone, PartialEq, Eq)]
pub enum Key {
    Char(char),
    Ctrl(char),
    Enter,
    Esc,
    Tab,
    BackTab,
    Backspace,
    Delete,
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    PageUp,
    PageDown,
    ShiftLeft,
    ShiftRight,
    Paste(String),
}

impl std::fmt::Debug for Key {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Char(_) => formatter.write_str("Char"),
            Self::Paste(text) => write!(formatter, "Paste({} chars)", text.chars().count()),
            Self::Ctrl(letter) => write!(formatter, "Ctrl({letter})"),
            other => formatter.write_str(match other {
                Self::Enter => "Enter",
                Self::Esc => "Esc",
                Self::Tab => "Tab",
                Self::BackTab => "BackTab",
                Self::Backspace => "Backspace",
                Self::Delete => "Delete",
                Self::Left => "Left",
                Self::Right => "Right",
                Self::Up => "Up",
                Self::Down => "Down",
                Self::Home => "Home",
                Self::End => "End",
                Self::PageUp => "PageUp",
                Self::PageDown => "PageDown",
                Self::ShiftLeft => "ShiftLeft",
                _ => "ShiftRight",
            }),
        }
    }
}

/// An action every screen shares, resolved before the screen sees the key.
/// Input: a key and whether a text field has focus, through `global`.
/// Output: the shared action.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Global {
    Quit,
    GoTo,
    Help,
    PreviousSection,
    NextSection,
}

impl Key {
    /// Normalizes one crossterm key event.
    /// Input: the event. Output: the key, or None for releases and keys the TUI
    /// does not use (function keys, Alt and Super chords).
    pub fn from_event(event: KeyEvent) -> Option<Self> {
        if event.kind == KeyEventKind::Release {
            return None;
        }
        let modifiers = event.modifiers;
        if modifiers.intersects(KeyModifiers::ALT | KeyModifiers::SUPER | KeyModifiers::META) {
            return None;
        }
        if modifiers.contains(KeyModifiers::CONTROL) {
            return match event.code {
                KeyCode::Char(letter) if letter.is_ascii_alphabetic() => {
                    Some(Self::Ctrl(letter.to_ascii_lowercase()))
                }
                _ => None,
            };
        }
        let shift = modifiers.contains(KeyModifiers::SHIFT);
        Some(match event.code {
            KeyCode::Char(character) if !character.is_control() => Self::Char(character),
            KeyCode::Enter => Self::Enter,
            KeyCode::Esc => Self::Esc,
            KeyCode::Tab if shift => Self::BackTab,
            KeyCode::Tab => Self::Tab,
            KeyCode::BackTab => Self::BackTab,
            KeyCode::Backspace => Self::Backspace,
            KeyCode::Delete => Self::Delete,
            KeyCode::Left if shift => Self::ShiftLeft,
            KeyCode::Right if shift => Self::ShiftRight,
            KeyCode::Left => Self::Left,
            KeyCode::Right => Self::Right,
            KeyCode::Up => Self::Up,
            KeyCode::Down => Self::Down,
            KeyCode::Home => Self::Home,
            KeyCode::End => Self::End,
            KeyCode::PageUp => Self::PageUp,
            KeyCode::PageDown => Self::PageDown,
            _ => return None,
        })
    }
}

/// Resolves the keys every screen shares.
/// Inputs: the key and whether a text field has focus. Output: the global
/// action; `?`, `[` and `]` type into a focused field instead.
pub fn global(key: &Key, typing: bool) -> Option<Global> {
    match key {
        Key::Ctrl('q' | 'c') => Some(Global::Quit),
        Key::Ctrl('k') => Some(Global::GoTo),
        Key::ShiftLeft => Some(Global::PreviousSection),
        Key::ShiftRight => Some(Global::NextSection),
        Key::Char('?') if !typing => Some(Global::Help),
        Key::Char('[') if !typing => Some(Global::PreviousSection),
        Key::Char(']') if !typing => Some(Global::NextSection),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(code: KeyCode, modifiers: KeyModifiers) -> Option<Key> {
        Key::from_event(KeyEvent::new(code, modifiers))
    }

    #[test]
    fn events_normalize_to_plain_keys() {
        assert_eq!(press(KeyCode::Char('a'), KeyModifiers::NONE), Some(Key::Char('a')));
        assert_eq!(press(KeyCode::Char('A'), KeyModifiers::SHIFT), Some(Key::Char('A')));
        assert_eq!(press(KeyCode::Char('K'), KeyModifiers::CONTROL), Some(Key::Ctrl('k')));
        assert_eq!(
            press(KeyCode::Char('j'), KeyModifiers::CONTROL),
            Some(Key::Ctrl('j'))
        );
        assert_eq!(press(KeyCode::Left, KeyModifiers::SHIFT), Some(Key::ShiftLeft));
        assert_eq!(press(KeyCode::Right, KeyModifiers::SHIFT), Some(Key::ShiftRight));
        assert_eq!(press(KeyCode::Tab, KeyModifiers::SHIFT), Some(Key::BackTab));
        assert_eq!(press(KeyCode::BackTab, KeyModifiers::SHIFT), Some(Key::BackTab));
        assert_eq!(press(KeyCode::F(2), KeyModifiers::NONE), None);
        assert_eq!(press(KeyCode::Char('x'), KeyModifiers::ALT), None);
        assert_eq!(press(KeyCode::Char('1'), KeyModifiers::CONTROL), None);
        let mut release = KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE);
        release.kind = KeyEventKind::Release;
        assert_eq!(Key::from_event(release), None);
    }

    #[test]
    fn globals_leave_letters_to_focused_fields() {
        assert_eq!(global(&Key::Ctrl('q'), true), Some(Global::Quit));
        assert_eq!(global(&Key::Ctrl('c'), false), Some(Global::Quit));
        assert_eq!(global(&Key::Ctrl('k'), true), Some(Global::GoTo));
        assert_eq!(global(&Key::Char('?'), false), Some(Global::Help));
        assert_eq!(global(&Key::Char('?'), true), None);
        assert_eq!(global(&Key::Char('['), false), Some(Global::PreviousSection));
        assert_eq!(global(&Key::Char(']'), true), None);
        assert_eq!(global(&Key::ShiftRight, true), Some(Global::NextSection));
        assert_eq!(global(&Key::Char('n'), false), None);
        assert_eq!(global(&Key::Esc, false), None);
    }

    #[test]
    fn debug_output_never_shows_typed_text() {
        assert_eq!(format!("{:?}", Key::Char('s')), "Char");
        assert_eq!(
            format!("{:?}", Key::Paste("sk-secret-value".into())),
            "Paste(15 chars)"
        );
    }
}
