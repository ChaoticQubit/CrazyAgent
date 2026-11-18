use super::pane::{ALT, CONTROL, Input, Named, SHIFT};

/// One key press as AppKit reports it: its characters with and without modifiers (the
/// latter still shifted) and the modifier keys held. Command keys never come here; the
/// app menu and the view's key equivalents take them.
#[derive(Clone, Copy, Debug)]
pub struct Press<'a> {
    pub chars: &'a str,
    pub bare: &'a str,
    pub shift: bool,
    pub option: bool,
    pub control: bool,
}

impl Press<'_> {
    fn mods(&self) -> u8 {
        let mods = (u8::from(self.shift) * SHIFT)
            | (u8::from(self.option) * ALT)
            | (u8::from(self.control) * CONTROL);
        assert!(mods <= SHIFT | ALT | CONTROL);
        mods
    }
}

/// The named key an AppKit function-key character stands for, from the private-use range.
fn named(bare: &str) -> Option<Named> {
    let key = match bare.chars().next()? {
        '\u{F700}' => Named::Up,
        '\u{F701}' => Named::Down,
        '\u{F702}' => Named::Left,
        '\u{F703}' => Named::Right,
        c @ '\u{F704}'..='\u{F70F}' => Named::F(u8::try_from(u32::from(c) - 0xF703).ok()?),
        '\u{F727}' => Named::Insert,
        '\u{F728}' => Named::Delete,
        '\u{F729}' => Named::Home,
        '\u{F72B}' => Named::End,
        '\u{F72C}' => Named::PageUp,
        '\u{F72D}' => Named::PageDown,
        _ => return None,
    };
    assert!(!matches!(key, Named::F(n) if !(1..=12).contains(&n)));
    Some(key)
}

/// The byte Control makes with a bare key, as xterm sends it: letters, space, the digits 3
/// to 8, the brackets, `@` and `_`; none for any other key.
fn control_byte(bare: &str) -> Option<u8> {
    let [byte] = bare.as_bytes() else {
        return None;
    };
    let out = match byte {
        b'a'..=b'z' | b'A'..=b'Z' => byte.to_ascii_lowercase() - b'a' + 1,
        b' ' | b'@' => 0,
        b'3'..=b'7' => byte - b'3' + 0x1b,
        b'8' => 0x7f,
        b'[' => 0x1b,
        b'\\' => 0x1c,
        b']' => 0x1d,
        b'_' => 0x1f,
        _ => return None,
    };
    assert!(out < 0x20 || out == 0x7f);
    Some(out)
}

/// The bytes Enter, Backspace, Tab and Escape send, by their modifiers, as the xterm panel
/// sends them: Shift+Enter alone types a new line (Ctrl+J), keypad Enter is Enter, and
/// Option as Meta puts an Escape before the key.
fn edit_key(press: &Press<'_>) -> Option<Vec<u8>> {
    let alt = press.option;
    let enter = press.chars == "\r" || (press.chars == "\u{3}" && press.bare == "\u{3}");
    let bytes = match press.chars {
        _ if enter && press.shift && !alt && !press.control => b"\n".to_vec(),
        _ if enter => [&b"\x1b"[..usize::from(alt)], b"\r"].concat(),
        "\u{7f}" | "\u{8}" => {
            let erase = if press.control { 0x08 } else { 0x7f };
            [&b"\x1b"[..usize::from(alt)], &[erase][..]].concat()
        }
        "\u{1b}" => [&b"\x1b"[..usize::from(alt)], b"\x1b"].concat(),
        "\t" if !press.shift => b"\t".to_vec(),
        "\u{19}" => b"\x1b[Z".to_vec(),
        _ => return None,
    };
    assert!(!bytes.is_empty() && bytes.len() <= 3);
    Some(bytes)
}

/// The input one key press makes in the agent's terminal, matching the xterm panel with
/// Option as Meta: named keys carry their modifiers, Control maps a bare key to its control
/// byte, Option puts an Escape before the bare key, and plain text is its UTF-8 bytes.
/// Input: the press. Output: the input, or none for a key that sends nothing.
pub fn key_input(press: &Press<'_>) -> Option<Input> {
    if let Some(key) = named(press.bare) {
        let refused = key == Named::Insert && (press.shift || press.control);
        return (!refused).then_some(Input::Key {
            key,
            mods: press.mods(),
        });
    }
    if let Some(bytes) = edit_key(press) {
        return Some(Input::Text(bytes));
    }
    let private = press
        .chars
        .chars()
        .any(|c| ('\u{F700}'..='\u{F8FF}').contains(&c));
    if private || press.bare.is_empty() {
        return None;
    }
    let bytes = match (press.control, press.option) {
        (true, false) if press.shift => match press.bare {
            "_" | "@" => vec![control_byte(press.bare)?],
            _ => return None,
        },
        (true, false) => vec![control_byte(press.bare)?],
        (true, true)
            if press.bare == " " || press.bare.bytes().all(|b| b.is_ascii_alphabetic()) =>
        {
            vec![0x1b, control_byte(press.bare)?]
        }
        (true, true) => [b"\x1b", press.bare.as_bytes()].concat(),
        (false, true) => [b"\x1b", press.bare.as_bytes()].concat(),
        (false, false) if press.chars.is_empty() => return None,
        (false, false) => press.chars.as_bytes().to_vec(),
    };
    assert!(!bytes.is_empty());
    Some(Input::Text(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press<'a>(chars: &'a str, bare: &'a str, mods: u8) -> Press<'a> {
        Press {
            chars,
            bare,
            shift: mods & SHIFT != 0,
            option: mods & ALT != 0,
            control: mods & CONTROL != 0,
        }
    }

    fn text(chars: &str, bare: &str, mods: u8) -> Option<Vec<u8>> {
        match key_input(&press(chars, bare, mods)) {
            Some(Input::Text(bytes)) => Some(bytes),
            None => None,
            Some(other) => panic!("text expected, got {other:?}"),
        }
    }

    #[test]
    fn enter_keys_match_the_panel() {
        assert_eq!(text("\r", "\r", 0).as_deref(), Some(&b"\r"[..]));
        assert_eq!(text("\r", "\r", SHIFT).as_deref(), Some(&b"\n"[..]));
        assert_eq!(
            text("\r", "\r", SHIFT | CONTROL).as_deref(),
            Some(&b"\r"[..])
        );
        assert_eq!(text("\r", "\r", ALT).as_deref(), Some(&b"\x1b\r"[..]));
        assert_eq!(text("\u{3}", "\u{3}", 0).as_deref(), Some(&b"\r"[..]));
        assert_eq!(text("\u{3}", "\u{3}", SHIFT).as_deref(), Some(&b"\n"[..]));
        assert_eq!(text("\u{3}", "c", CONTROL).as_deref(), Some(&b"\x03"[..]));
    }

    #[test]
    fn edit_keys_follow_their_modifiers() {
        assert_eq!(text("\u{7f}", "\u{7f}", 0).as_deref(), Some(&b"\x7f"[..]));
        assert_eq!(
            text("\u{7f}", "\u{7f}", CONTROL).as_deref(),
            Some(&b"\x08"[..])
        );
        assert_eq!(
            text("\u{7f}", "\u{7f}", ALT).as_deref(),
            Some(&b"\x1b\x7f"[..])
        );
        assert_eq!(
            text("\u{1b}", "\u{1b}", ALT).as_deref(),
            Some(&b"\x1b\x1b"[..])
        );
        assert_eq!(text("\t", "\t", 0).as_deref(), Some(&b"\t"[..]));
        assert_eq!(text("\u{19}", "\t", SHIFT).as_deref(), Some(&b"\x1b[Z"[..]));
    }

    #[test]
    fn control_and_option_make_xterm_bytes() {
        assert_eq!(text("\u{1}", "a", CONTROL).as_deref(), Some(&b"\x01"[..]));
        assert_eq!(text("\0", " ", CONTROL).as_deref(), Some(&b"\0"[..]));
        assert_eq!(text("3", "3", CONTROL).as_deref(), Some(&b"\x1b"[..]));
        assert_eq!(text("8", "8", CONTROL).as_deref(), Some(&b"\x7f"[..]));
        assert_eq!(
            text("\u{1f}", "_", CONTROL | SHIFT).as_deref(),
            Some(&b"\x1f"[..])
        );
        assert_eq!(text("\u{1}", "A", CONTROL | SHIFT), None);
        assert_eq!(text("å", "a", ALT).as_deref(), Some(&b"\x1ba"[..]));
        assert_eq!(text("Å", "A", ALT | SHIFT).as_deref(), Some(&b"\x1bA"[..]));
        assert_eq!(
            text("\u{1}", "a", ALT | CONTROL).as_deref(),
            Some(&b"\x1b\x01"[..])
        );
        assert_eq!(
            text("3", "3", ALT | CONTROL).as_deref(),
            Some(&b"\x1b3"[..])
        );
        assert_eq!(text("2", "2", CONTROL), None);
        assert_eq!(text("é", "é", 0).as_deref(), Some("é".as_bytes()));
        assert_eq!(text("", "", 0), None);
    }

    #[test]
    fn named_keys_carry_their_modifiers() {
        let key = |bare: &str, mods| key_input(&press(bare, bare, mods));
        assert_eq!(
            key("\u{F700}", 0),
            Some(Input::Key {
                key: Named::Up,
                mods: 0
            })
        );
        assert_eq!(
            key("\u{F702}", ALT),
            Some(Input::Key {
                key: Named::Left,
                mods: ALT
            })
        );
        assert_eq!(
            key("\u{F704}", 0),
            Some(Input::Key {
                key: Named::F(1),
                mods: 0
            })
        );
        assert_eq!(
            key("\u{F70F}", SHIFT),
            Some(Input::Key {
                key: Named::F(12),
                mods: SHIFT
            })
        );
        assert_eq!(key("\u{F727}", SHIFT), None);
        assert_eq!(key("\u{F710}", 0), None);
    }
}
