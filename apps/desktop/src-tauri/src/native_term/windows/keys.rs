use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    VK_A, VK_BACK, VK_C, VK_DELETE, VK_DOWN, VK_END, VK_ESCAPE, VK_F1, VK_F4, VK_F12, VK_HOME,
    VK_INSERT, VK_LEFT, VK_NEXT, VK_PRIOR, VK_RETURN, VK_RIGHT, VK_SPACE, VK_TAB, VK_UP, VK_V,
    VK_Z,
};

use super::super::event::ViewEvent;
use super::super::keymap::{self, Press};
use super::super::pane::{Input, Named};

/// The modifier keys held at a key press, as the window procedure reads them with
/// GetKeyState; `win` is either Windows key.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Mods {
    pub shift: bool,
    pub control: bool,
    pub alt: bool,
    pub win: bool,
}

/// The character message TranslateMessage queued behind a key-down, as PeekMessageW finds
/// it: none, a character (WM_CHAR or WM_SYSCHAR) with its UTF-16 unit, or a dead key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Next {
    Nothing,
    Char(u16),
    Dead,
}

/// What one key-down does: the event it sends, if any, and whether the character message
/// queued behind it is dropped. No event and no swallow leaves the key to the character
/// path and the system's default handling.
#[derive(Debug, PartialEq)]
pub struct Down {
    pub event: Option<ViewEvent>,
    pub swallow: bool,
}

/// What one character message does: a text input, a high surrogate held for its pair, or
/// nothing.
#[derive(Debug, PartialEq)]
pub enum Typed {
    Text(Input),
    Held(u16),
    Dropped,
}

/// The press keymap reads for a key with the held modifiers; its characters are also its
/// bare key. Inputs: the characters and the modifiers. Output: the press.
fn press(chars: &str, mods: Mods) -> Press<'_> {
    Press {
        chars,
        bare: chars,
        shift: mods.shift,
        option: mods.alt,
        control: mods.control,
    }
}

/// The named key a VK stands for, Shift+Tab as Back Tab, decided before any text so the
/// keypad's navigation keys and Delete never type a character. Inputs: the VK and the
/// modifiers. Output: the named key, or none.
fn named(vk: u16, mods: Mods) -> Option<Named> {
    let named = match vk {
        VK_UP => Named::Up,
        VK_DOWN => Named::Down,
        VK_LEFT => Named::Left,
        VK_RIGHT => Named::Right,
        VK_HOME => Named::Home,
        VK_END => Named::End,
        VK_PRIOR => Named::PageUp,
        VK_NEXT => Named::PageDown,
        VK_INSERT => Named::Insert,
        VK_DELETE => Named::Delete,
        VK_TAB if mods.shift => Named::BackTab,
        VK_F1..=VK_F12 => Named::F(u8::try_from(vk - VK_F1 + 1).ok()?),
        _ => return None,
    };
    assert!(!matches!(named, Named::F(n) if !(1..=12).contains(&n)));
    Some(named)
}

/// The characters keymap reads for Enter, Backspace, Tab and Escape, as the macOS view
/// reports them: Backspace as DEL. Input: the VK. Output: the characters, or none.
fn edit(vk: u16) -> Option<&'static str> {
    let chars = match vk {
        VK_RETURN => "\r",
        VK_BACK => "\u{7f}",
        VK_TAB => "\t",
        VK_ESCAPE => "\u{1b}",
        _ => return None,
    };
    assert_eq!(chars.len(), 1);
    Some(chars)
}

/// The view command a copy or paste chord makes, by the panel's rules off a Mac: Ctrl+C
/// and Ctrl+Shift+C copy while text is selected, else send ^C; Ctrl+V, Ctrl+Shift+V and
/// Shift+Insert paste. Inputs: the VK and the modifiers. Output: the command, or none.
fn chord(vk: u16, mods: Mods) -> Option<ViewEvent> {
    if mods.alt {
        return None;
    }
    let event = match (vk, mods.control, mods.shift) {
        (VK_C, true, _) => ViewEvent::CopyKey(Input::Text(vec![0x03])),
        (VK_V, true, _) | (VK_INSERT, false, true) => ViewEvent::Paste,
        _ => return None,
    };
    assert!(matches!(event, ViewEvent::CopyKey(_) | ViewEvent::Paste));
    Some(event)
}

/// The key's own character for a Control or Alt chord: a letter from its VK, so Ctrl+C
/// still interrupts on a Cyrillic or Greek layout, upper case with Shift; else the
/// character the key types with Shift alone. Inputs: the VK, Shift and that character.
/// Output: the character, or none.
fn own(vk: u16, shift: bool, bare: Option<char>) -> Option<char> {
    if !(VK_A..=VK_Z).contains(&vk) {
        return bare.filter(|c| !c.is_control());
    }
    let letter = char::from(u8::try_from(vk).ok()?);
    assert!(letter.is_ascii_uppercase());
    Some(if shift {
        letter
    } else {
        letter.to_ascii_lowercase()
    })
}

/// Whether a UTF-16 unit is a character that types, not a control code.
fn printable(unit: u16) -> bool {
    unit >= 0x20 && unit != 0x7f
}

/// What a key-down makes, by the xterm rules of `keymap` with Alt as Meta: a Windows key
/// chord sends nothing; Alt+F4 and Alt+Space stay the system's; the copy and paste chords,
/// named keys and Enter, Backspace, Tab and Escape send on the key-down; a Ctrl+Alt key
/// that types a character is AltGr text and goes to the character path, as does a key with
/// no Ctrl or Alt; any other Ctrl or Alt chord sends from the key's own character. Inputs:
/// the VK, the modifiers, the queued character message and the key's Shift-only character.
/// Output: the event and whether the queued character is dropped.
pub fn down(vk: u16, mods: Mods, next: Next, bare: Option<char>) -> Down {
    let _ = (vk, mods, next, bare);
    unimplemented!("H2 red")
}

/// What a character message types: one character as its UTF-8 bytes, a surrogate pair
/// joined, Enter, Tab and Escape kept; a lone surrogate or any other control code types
/// nothing. Inputs: the UTF-16 unit and the high surrogate held from the last message.
/// Output: the text, the unit to hold, or nothing.
pub fn typed(unit: u16, held: Option<u16>) -> Typed {
    let _ = (unit, held);
    unimplemented!("H2 red")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native_term::pane::{ALT, CONTROL, SHIFT};
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        VK_F5, VK_OEM_4, VK_OEM_MINUS, VK_Q, VK_SHIFT, VK_X,
    };

    const NONE: Mods = Mods {
        shift: false,
        control: false,
        alt: false,
        win: false,
    };
    const SHIFTED: Mods = Mods {
        shift: true,
        ..NONE
    };
    const CTRL: Mods = Mods {
        control: true,
        ..NONE
    };
    const ALTED: Mods = Mods { alt: true, ..NONE };
    const CTRL_SHIFT: Mods = Mods {
        shift: true,
        control: true,
        ..NONE
    };
    const CTRL_ALT: Mods = Mods {
        control: true,
        alt: true,
        ..NONE
    };

    fn sent(vk: u16, mods: Mods, next: Next) -> Option<ViewEvent> {
        down(vk, mods, next, None).event
    }

    fn text(bytes: &[u8]) -> Option<ViewEvent> {
        Some(ViewEvent::Key(Input::Text(bytes.to_vec())))
    }

    fn key(key: Named, mods: u8) -> Option<ViewEvent> {
        Some(ViewEvent::Key(Input::Key { key, mods }))
    }

    #[test]
    fn named_keys_carry_their_modifier_bits_and_send_on_the_key_down() {
        assert_eq!(sent(VK_UP, NONE, Next::Nothing), key(Named::Up, 0));
        assert_eq!(sent(VK_UP, CTRL, Next::Nothing), key(Named::Up, CONTROL));
        assert_eq!(
            sent(VK_PRIOR, SHIFTED, Next::Nothing),
            key(Named::PageUp, SHIFT)
        );
        assert_eq!(sent(VK_NEXT, NONE, Next::Nothing), key(Named::PageDown, 0));
        assert_eq!(sent(VK_F1, NONE, Next::Nothing), key(Named::F(1), 0));
        assert_eq!(sent(VK_F5, ALTED, Next::Nothing), key(Named::F(5), ALT));
        assert_eq!(sent(VK_F12, NONE, Next::Nothing), key(Named::F(12), 0));
        assert_eq!(sent(VK_DELETE, NONE, Next::Nothing), key(Named::Delete, 0));
        assert_eq!(sent(VK_INSERT, NONE, Next::Nothing), key(Named::Insert, 0));
        assert_eq!(sent(VK_INSERT, CTRL, Next::Nothing), None);
        assert_eq!(
            sent(VK_TAB, SHIFTED, Next::Char(9)),
            key(Named::BackTab, SHIFT)
        );
        assert!(down(VK_TAB, SHIFTED, Next::Char(9), None).swallow);
        let f13 = down(VK_F12 + 1, NONE, Next::Nothing, None);
        assert_eq!(
            f13,
            Down {
                event: None,
                swallow: false
            }
        );
    }

    #[test]
    fn edit_keys_follow_the_panel_and_drop_their_character_message() {
        assert_eq!(sent(VK_RETURN, NONE, Next::Char(13)), text(b"\r"));
        assert_eq!(sent(VK_RETURN, SHIFTED, Next::Char(13)), text(b"\n"));
        assert_eq!(sent(VK_RETURN, ALTED, Next::Char(13)), text(b"\x1b\r"));
        assert_eq!(sent(VK_BACK, NONE, Next::Char(8)), text(b"\x7f"));
        assert_eq!(sent(VK_BACK, CTRL, Next::Char(0x7f)), text(b"\x08"));
        assert_eq!(sent(VK_TAB, NONE, Next::Char(9)), text(b"\t"));
        assert_eq!(sent(VK_ESCAPE, NONE, Next::Char(0x1b)), text(b"\x1b"));
        for vk in [VK_RETURN, VK_BACK, VK_TAB, VK_ESCAPE] {
            assert!(down(vk, NONE, Next::Char(13), None).swallow, "{vk}");
            assert!(!down(vk, NONE, Next::Nothing, None).swallow, "{vk}");
        }
    }

    #[test]
    fn control_and_alt_chords_send_from_the_keys_own_character() {
        assert_eq!(sent(VK_A, CTRL, Next::Char(1)), text(b"\x01"));
        assert_eq!(sent(VK_A, CTRL_ALT, Next::Nothing), text(b"\x1b\x01"));
        assert_eq!(
            sent(VK_A, ALTED, Next::Char(u16::from(b'a'))),
            text(b"\x1ba")
        );
        let shifted = down(VK_A, CTRL_SHIFT, Next::Char(1), None);
        assert_eq!(
            shifted,
            Down {
                event: None,
                swallow: true
            }
        );
        let underscore = down(VK_OEM_MINUS, CTRL_SHIFT, Next::Char(0x1f), Some('_'));
        assert_eq!(underscore.event, text(b"\x1f"));
        assert_eq!(
            down(VK_SPACE, CTRL, Next::Char(32), Some(' ')).event,
            text(b"\0")
        );
        assert_eq!(
            down(VK_OEM_4, CTRL, Next::Char(0x1b), Some('[')).event,
            text(b"\x1b")
        );
        let cyrillic = down(VK_X, CTRL, Next::Char(0x18), Some('ч'));
        assert_eq!(cyrillic.event, text(b"\x18"));
        assert_eq!(down(VK_SHIFT, CTRL, Next::Nothing, None).event, None);
    }

    #[test]
    fn altgr_text_and_plain_keys_go_to_the_character_path() {
        let pass = Down {
            event: None,
            swallow: false,
        };
        assert_eq!(
            down(VK_Q, CTRL_ALT, Next::Char(u16::from(b'@')), Some('q')),
            pass
        );
        assert_eq!(
            down(VK_A, NONE, Next::Char(u16::from(b'a')), Some('a')),
            pass
        );
        assert_eq!(
            down(VK_A, SHIFTED, Next::Char(u16::from(b'A')), Some('A')),
            pass
        );
        assert_eq!(
            down(VK_Q, CTRL_ALT, Next::Dead, Some('q')).event,
            text(b"\x1b\x11")
        );
    }

    #[test]
    fn windows_key_chords_send_nothing_and_alt_f4_and_alt_space_stay_the_systems() {
        let win = Mods { win: true, ..NONE };
        assert_eq!(
            down(VK_A, win, Next::Nothing, None),
            Down {
                event: None,
                swallow: false
            }
        );
        assert_eq!(down(VK_UP, win, Next::Nothing, None).event, None);
        let pass = Down {
            event: None,
            swallow: false,
        };
        assert_eq!(down(VK_F4, ALTED, Next::Nothing, None), pass);
        assert_eq!(down(VK_SPACE, ALTED, Next::Char(32), Some(' ')), pass);
    }

    #[test]
    fn copy_and_paste_chords_follow_the_panel_off_a_mac() {
        let copy = Some(ViewEvent::CopyKey(Input::Text(vec![0x03])));
        assert_eq!(sent(VK_C, CTRL, Next::Char(3)), copy);
        assert_eq!(sent(VK_C, CTRL_SHIFT, Next::Char(3)), copy);
        assert!(down(VK_C, CTRL, Next::Char(3), None).swallow);
        assert_eq!(sent(VK_V, CTRL, Next::Char(0x16)), Some(ViewEvent::Paste));
        assert_eq!(
            sent(VK_V, CTRL_SHIFT, Next::Char(0x16)),
            Some(ViewEvent::Paste)
        );
        assert_eq!(
            sent(VK_INSERT, SHIFTED, Next::Nothing),
            Some(ViewEvent::Paste)
        );
        assert_eq!(sent(VK_C, CTRL_ALT, Next::Nothing), text(b"\x1b\x03"));
    }

    #[test]
    fn the_character_path_types_one_character_and_joins_surrogate_pairs() {
        let typed_text = |s: &str| Typed::Text(Input::Text(s.as_bytes().to_vec()));
        assert_eq!(typed(u16::from(b'a'), None), typed_text("a"));
        assert_eq!(typed(0xE9, None), typed_text("é"));
        assert_eq!(typed(0x304B, None), typed_text("か"));
        assert_eq!(typed(0xD83D, None), Typed::Held(0xD83D));
        assert_eq!(typed(0xDE0A, Some(0xD83D)), typed_text("😊"));
        assert_eq!(typed(0xDE0A, None), Typed::Dropped);
        assert_eq!(typed(0x304B, Some(0xD83D)), typed_text("か"));
        assert_eq!(typed(13, None), typed_text("\r"));
        assert_eq!(typed(8, None), Typed::Dropped);
        assert_eq!(typed(0x7f, None), Typed::Dropped);
        assert_eq!(typed(3, None), Typed::Dropped);
    }
}
