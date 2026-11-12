use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

/// One key press after normalization, the only key type screens see.
/// Input: built from a crossterm event by `Key::from_event`, or from a paste.
/// Output: plain variants; Ctrl letters are lowercase, Ctrl-] is `Ctrl(']')`,
/// Shift-arrows are named.
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

/// What Shift-←→ do while the master's terminal has the keys, a choice kept on this
/// computer. Input: picked in Settings, Profile. Output: `SwitchTabs`, the default, leaves
/// the master view for the project's previous or next tab; `SendToAgent` types them in.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ShiftArrows {
    #[default]
    SwitchTabs,
    SendToAgent,
}

impl ShiftArrows {
    /// Every choice with its label, in the order Settings shows them.
    pub const ALL: [(ShiftArrows, &'static str); 2] = [
        (Self::SwitchTabs, "Switch tabs"),
        (Self::SendToAgent, "Send to the agent"),
    ];
}

/// Whether the TUI asks the terminal for the enhanced keyboard protocol, so Shift-Enter
/// reaches it as its own key: `Auto` asks at start and turns the protocol on when the
/// terminal has it; `Off` never asks, and Ctrl-J stays the only new-line key.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ShiftEnter {
    #[default]
    Auto,
    Off,
}

impl ShiftEnter {
    /// Every choice with its label, in the order Settings shows them.
    pub const ALL: [(ShiftEnter, &'static str); 2] = [(Self::Auto, "Auto"), (Self::Off, "Off")];
}

impl Key {
    /// Normalizes one crossterm key event.
    /// Input: the event. Output: the key, or None for releases and keys the TUI
    /// does not use (function keys, Alt and Super chords). Terminals send Ctrl-]
    /// as byte 0x1d, which crossterm reports as Ctrl-5. Shift+Enter, where the
    /// terminal reports it, is Ctrl-J: the new-line key, a line feed to an agent.
    /// Ctrl chords a terminal reports under the enhanced keyboard protocol (Ctrl-Enter,
    /// Ctrl-I, Ctrl-M, Ctrl-Tab, Ctrl-Backspace, Ctrl-Esc, Ctrl-[, Ctrl-3, Ctrl-8) give the
    /// key the same chord gives from a legacy terminal, so the shell and an agent see the
    /// same key and bytes.
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
                    match letter.to_ascii_lowercase() {
                        'i' => Some(Self::Tab),
                        'm' => Some(Self::Enter),
                        letter => Some(Self::Ctrl(letter)),
                    }
                }
                KeyCode::Char(']' | '5') => Some(Self::Ctrl(']')),
                KeyCode::Char('[' | '3') | KeyCode::Esc => Some(Self::Esc),
                KeyCode::Char('8') => Some(Self::Backspace),
                KeyCode::Backspace => Some(Self::Ctrl('h')),
                KeyCode::Enter => Some(Self::Enter),
                KeyCode::Tab => Some(Self::Tab),
                KeyCode::BackTab => Some(Self::BackTab),
                _ => None,
            };
        }
        let shift = modifiers.contains(KeyModifiers::SHIFT);
        Some(match event.code {
            KeyCode::Char(character) if !character.is_control() => Self::Char(character),
            KeyCode::Enter if shift => Self::Ctrl('j'),
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
        assert_eq!(
            press(KeyCode::Char('a'), KeyModifiers::NONE),
            Some(Key::Char('a'))
        );
        assert_eq!(
            press(KeyCode::Char('A'), KeyModifiers::SHIFT),
            Some(Key::Char('A'))
        );
        assert_eq!(
            press(KeyCode::Char('K'), KeyModifiers::CONTROL),
            Some(Key::Ctrl('k'))
        );
        assert_eq!(
            press(KeyCode::Char('j'), KeyModifiers::CONTROL),
            Some(Key::Ctrl('j'))
        );
        assert_eq!(
            press(KeyCode::Left, KeyModifiers::SHIFT),
            Some(Key::ShiftLeft)
        );
        assert_eq!(
            press(KeyCode::Right, KeyModifiers::SHIFT),
            Some(Key::ShiftRight)
        );
        assert_eq!(press(KeyCode::Tab, KeyModifiers::SHIFT), Some(Key::BackTab));
        assert_eq!(
            press(KeyCode::BackTab, KeyModifiers::SHIFT),
            Some(Key::BackTab)
        );
        assert_eq!(press(KeyCode::F(2), KeyModifiers::NONE), None);
        assert_eq!(press(KeyCode::Char('x'), KeyModifiers::ALT), None);
        assert_eq!(press(KeyCode::Char('1'), KeyModifiers::CONTROL), None);
        assert_eq!(
            press(KeyCode::Char(']'), KeyModifiers::CONTROL),
            Some(Key::Ctrl(']'))
        );
        assert_eq!(
            press(KeyCode::Char('5'), KeyModifiers::CONTROL),
            Some(Key::Ctrl(']'))
        );
        let mut release = KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE);
        release.kind = KeyEventKind::Release;
        assert_eq!(Key::from_event(release), None);
    }

    #[test]
    fn shift_enter_is_the_new_line_key_a_terminal_sends_as_a_line_feed() {
        assert_eq!(
            press(KeyCode::Enter, KeyModifiers::SHIFT),
            Some(Key::Ctrl('j')),
            "a terminal reporting Shift+Enter (CSI 13;2u) gets a new line, never a send"
        );
        assert_eq!(press(KeyCode::Enter, KeyModifiers::NONE), Some(Key::Enter));
        let line_feed = crate::tui::terminal_pane::encode(&Key::Ctrl('j'), false, false);
        assert_eq!(
            line_feed, b"\n",
            "the byte Claude Code and Codex read as a new line in the message"
        );
        assert_eq!(
            crate::tui::terminal_pane::encode(&Key::Enter, false, false),
            b"\r"
        );
    }

    /// N-T1 C: a legacy terminal sends Ctrl-I and Ctrl-M as Tab and Enter; under the
    /// enhanced keyboard protocol they arrive as Ctrl chords and still act as Tab and Enter
    /// in the shell, while Ctrl-H and Ctrl-J stay the Ctrl keys they are today.
    #[test]
    fn ctrl_i_and_ctrl_m_from_the_enhanced_keyboard_protocol_act_as_tab_and_enter() {
        let control = KeyModifiers::CONTROL;
        let both = KeyModifiers::CONTROL | KeyModifiers::SHIFT;
        assert_eq!(press(KeyCode::Char('i'), control), Some(Key::Tab));
        assert_eq!(press(KeyCode::Char('I'), both), Some(Key::Tab));
        assert_eq!(press(KeyCode::Char('m'), control), Some(Key::Enter));
        assert_eq!(press(KeyCode::Char('M'), both), Some(Key::Enter));
        assert_eq!(press(KeyCode::Char('h'), control), Some(Key::Ctrl('h')));
        assert_eq!(press(KeyCode::Char('j'), control), Some(Key::Ctrl('j')));
    }

    /// N-T1 C: under the enhanced keyboard protocol (disambiguated escape codes) a terminal
    /// reports Ctrl and Shift chords as CSI u, which crossterm 0.28 gives as the plain key
    /// with the modifier; each reaches an agent's terminal as the bytes the same key sends
    /// from a legacy terminal today.
    #[test]
    fn keys_from_the_enhanced_keyboard_protocol_reach_an_agent_as_legacy_bytes() {
        let control = KeyModifiers::CONTROL;
        let shift = KeyModifiers::SHIFT;
        let none = KeyModifiers::NONE;
        let pairs = [
            (
                "Ctrl-Enter",
                press(KeyCode::Enter, control),
                press(KeyCode::Enter, none),
            ),
            (
                "Ctrl-Shift-Enter",
                press(KeyCode::Enter, control | shift),
                press(KeyCode::Enter, none),
            ),
            (
                "Ctrl-Tab",
                press(KeyCode::Tab, control),
                press(KeyCode::Tab, none),
            ),
            (
                "Ctrl-Shift-Tab",
                press(KeyCode::BackTab, control | shift),
                press(KeyCode::BackTab, shift),
            ),
            (
                "Ctrl-Backspace",
                press(KeyCode::Backspace, control),
                press(KeyCode::Char('h'), control),
            ),
            (
                "Ctrl-Esc",
                press(KeyCode::Esc, control),
                press(KeyCode::Esc, none),
            ),
            (
                "Ctrl-[",
                press(KeyCode::Char('['), control),
                press(KeyCode::Esc, none),
            ),
            (
                "Ctrl-3",
                press(KeyCode::Char('3'), control),
                press(KeyCode::Esc, none),
            ),
            (
                "Ctrl-8",
                press(KeyCode::Char('8'), control),
                press(KeyCode::Backspace, none),
            ),
            (
                "Ctrl-]",
                press(KeyCode::Char(']'), control),
                press(KeyCode::Char('5'), control),
            ),
            (
                "Ctrl-Shift-A",
                press(KeyCode::Char('A'), control | shift),
                press(KeyCode::Char('a'), control),
            ),
            (
                "Shift-Esc",
                press(KeyCode::Esc, shift),
                press(KeyCode::Esc, none),
            ),
            (
                "Shift-Backspace",
                press(KeyCode::Backspace, shift),
                press(KeyCode::Backspace, none),
            ),
        ];
        for (name, enhanced, legacy) in pairs {
            let legacy = legacy.unwrap_or_else(|| panic!("{name}: a legacy terminal sends it"));
            let enhanced =
                enhanced.unwrap_or_else(|| panic!("{name}: dropped under the enhanced protocol"));
            assert_eq!(
                crate::tui::terminal_pane::encode(&enhanced, false, false),
                crate::tui::terminal_pane::encode(&legacy, false, false),
                "{name}"
            );
        }
    }

    #[test]
    fn globals_leave_letters_to_focused_fields() {
        assert_eq!(global(&Key::Ctrl('q'), true), Some(Global::Quit));
        assert_eq!(global(&Key::Ctrl('c'), false), Some(Global::Quit));
        assert_eq!(global(&Key::Ctrl('k'), true), Some(Global::GoTo));
        assert_eq!(global(&Key::Char('?'), false), Some(Global::Help));
        assert_eq!(global(&Key::Char('?'), true), None);
        assert_eq!(
            global(&Key::Char('['), false),
            Some(Global::PreviousSection)
        );
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
