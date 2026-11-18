use crate::native_term::keymap::{self, Press};
use crate::native_term::pane::{Input, Named};
use gtk::gdk::ModifierType;
use gtk::gdk::keys::Key;
use gtk::gdk::keys::constants as key;

/// The modifiers whose chords belong to the desktop, never to the terminal: Super and
/// Hyper, and Mod4, where X11 keymaps put them.
const DESKTOP_CHORDS: ModifierType = ModifierType::SUPER_MASK
    .union(ModifierType::HYPER_MASK)
    .union(ModifierType::MOD4_MASK);

/// The named key a GDK key stands for, the keypad's navigation keys included; it is
/// decided before any text, so Delete and the keypad keys never type a character. Input:
/// the key. Output: the named key, or none.
fn named(keyval: Key) -> Option<Named> {
    let named = match keyval {
        key::Up | key::KP_Up => Named::Up,
        key::Down | key::KP_Down => Named::Down,
        key::Left | key::KP_Left => Named::Left,
        key::Right | key::KP_Right => Named::Right,
        key::Home | key::KP_Home => Named::Home,
        key::End | key::KP_End => Named::End,
        key::Page_Up | key::KP_Page_Up => Named::PageUp,
        key::Page_Down | key::KP_Page_Down => Named::PageDown,
        key::Insert | key::KP_Insert => Named::Insert,
        key::Delete | key::KP_Delete => Named::Delete,
        other if (*key::F1..=*key::F12).contains(&*other) => {
            Named::F(u8::try_from(*other - *key::F1 + 1).ok()?)
        }
        _ => return None,
    };
    assert!(!matches!(named, Named::F(n) if !(1..=12).contains(&n)));
    Some(named)
}

/// The characters a GDK key types, as the macOS view reads them: Return and keypad Enter
/// as a carriage return, Backspace as DEL, Shift+Tab (ISO_Left_Tab) as the back-tab
/// character U+0019, else the key's own character. Input: the key. Output: the text, or
/// none for a key with no character.
fn text(keyval: Key) -> Option<String> {
    let typed = match keyval {
        key::Return | key::KP_Enter => '\r',
        key::BackSpace => '\u{7f}',
        key::ISO_Left_Tab => '\u{19}',
        other => other.to_unicode()?,
    };
    Some(typed.to_string())
}

/// One keymap entry for a hardware key, as GDK lists them: group, level and keyval.
pub type Entry = (i32, i32, Key);

/// The keyval a Control chord reads on a non-Latin layout, as other GTK terminals map it,
/// so Ctrl+C still interrupts on a Cyrillic or Greek layout: with Control held and a key
/// whose character is not ASCII, the same hardware key's keyval in its lowest Latin
/// group, at the press's Shift level or else level 0. Inputs: the keyval, the modifier
/// state and the key's entries, read only then. Output: the keyval to map.
pub fn control_latin(
    keyval: Key,
    state: ModifierType,
    entries: impl FnOnce() -> Vec<Entry>,
) -> Key {
    let latin = |found: Key| found.to_unicode().is_some_and(|c| c.is_ascii_graphic());
    let non_latin = keyval.to_unicode().is_some_and(|c| !c.is_ascii());
    if !state.contains(ModifierType::CONTROL_MASK) || !non_latin {
        return keyval;
    }
    let entries = entries();
    let at_level = |level: i32| {
        entries
            .iter()
            .filter(|&&(_, at, found)| at == level && latin(found))
            .min_by_key(|&&(group, _, _)| group)
            .map(|&(_, _, found)| found)
    };
    let shifted = i32::from(state.contains(ModifierType::SHIFT_MASK));
    let mapped = at_level(shifted).or_else(|| at_level(0)).unwrap_or(keyval);
    assert!(mapped == keyval || latin(mapped));
    mapped
}

/// The input one GDK key press makes in the agent's terminal, by the xterm rules of
/// `keymap`: Shift, Control and Alt (Mod1) count, lock keys and the input method's forward
/// bit do not, and a Super or Hyper chord sends nothing. Inputs: the key and its modifier
/// state. Output: the input, or none for a key that sends nothing.
pub fn key_input(keyval: Key, state: ModifierType) -> Option<Input> {
    if state.intersects(DESKTOP_CHORDS) {
        return None;
    }
    let shift = state.contains(ModifierType::SHIFT_MASK);
    let control = state.contains(ModifierType::CONTROL_MASK);
    let option = state.contains(ModifierType::MOD1_MASK);
    if let Some(found) = named(keyval) {
        let press = Press {
            chars: "",
            bare: "",
            shift,
            option,
            control,
        };
        return keymap::named_input(found, &press);
    }
    let typed = text(keyval)?;
    assert!(!typed.is_empty());
    keymap::key_input(&Press {
        chars: &typed,
        bare: &typed,
        shift,
        option,
        control,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native_term::pane::{CONTROL, Input, Named, SHIFT};
    use gtk::gdk::ModifierType as M;
    use gtk::gdk::keys::Key;
    use gtk::gdk::keys::constants as k;

    fn text(keyval: Key, state: M) -> Option<Vec<u8>> {
        match key_input(keyval, state) {
            Some(Input::Text(bytes)) => Some(bytes),
            None => None,
            Some(other) => panic!("text expected, got {other:?}"),
        }
    }

    fn named(keyval: Key, state: M) -> Option<(Named, u8)> {
        match key_input(keyval, state) {
            Some(Input::Key { key, mods }) => Some((key, mods)),
            None => None,
            Some(other) => panic!("a named key expected, got {other:?}"),
        }
    }

    #[test]
    fn letters_carry_control_and_alt_as_xterm_bytes() {
        assert_eq!(text(k::a, M::empty()).as_deref(), Some(&b"a"[..]));
        assert_eq!(text(k::A, M::SHIFT_MASK).as_deref(), Some(&b"A"[..]));
        assert_eq!(text(k::a, M::CONTROL_MASK).as_deref(), Some(&b"\x01"[..]));
        assert_eq!(text(k::a, M::MOD1_MASK).as_deref(), Some(&b"\x1ba"[..]));
        assert_eq!(
            text(k::a, M::MOD1_MASK | M::CONTROL_MASK).as_deref(),
            Some(&b"\x1b\x01"[..])
        );
        assert_eq!(
            text(k::underscore, M::CONTROL_MASK | M::SHIFT_MASK).as_deref(),
            Some(&b"\x1f"[..])
        );
        assert_eq!(text(k::A, M::CONTROL_MASK | M::SHIFT_MASK), None);
        assert_eq!(text(k::space, M::CONTROL_MASK).as_deref(), Some(&b"\0"[..]));
        assert_eq!(text(k::eacute, M::empty()).as_deref(), Some("é".as_bytes()));
    }

    #[test]
    fn lock_keys_and_the_input_method_forward_bit_are_not_modifiers() {
        let noise = M::LOCK_MASK | M::MOD2_MASK | M::MODIFIER_RESERVED_25_MASK | M::BUTTON1_MASK;
        assert_eq!(text(k::a, noise).as_deref(), Some(&b"a"[..]));
        assert_eq!(
            text(k::a, noise | M::CONTROL_MASK).as_deref(),
            Some(&b"\x01"[..])
        );
        assert_eq!(named(k::Up, noise), Some((Named::Up, 0)));
        assert_eq!(
            text(k::a, M::MOD1_MASK | M::META_MASK).as_deref(),
            Some(&b"\x1ba"[..])
        );
    }

    #[test]
    fn super_and_hyper_chords_send_nothing() {
        for chord in [M::SUPER_MASK, M::MOD4_MASK, M::HYPER_MASK] {
            assert_eq!(key_input(k::a, chord), None);
            assert_eq!(key_input(k::Up, chord), None);
            assert_eq!(key_input(k::Return, chord), None);
        }
    }

    #[test]
    fn edit_keys_follow_the_panel() {
        assert_eq!(text(k::Return, M::empty()).as_deref(), Some(&b"\r"[..]));
        assert_eq!(text(k::KP_Enter, M::empty()).as_deref(), Some(&b"\r"[..]));
        assert_eq!(text(k::Return, M::SHIFT_MASK).as_deref(), Some(&b"\n"[..]));
        assert_eq!(
            text(k::Return, M::MOD1_MASK).as_deref(),
            Some(&b"\x1b\r"[..])
        );
        assert_eq!(
            text(k::BackSpace, M::empty()).as_deref(),
            Some(&b"\x7f"[..])
        );
        assert_eq!(
            text(k::BackSpace, M::CONTROL_MASK).as_deref(),
            Some(&b"\x08"[..])
        );
        assert_eq!(text(k::Tab, M::empty()).as_deref(), Some(&b"\t"[..]));
        assert_eq!(text(k::Escape, M::empty()).as_deref(), Some(&b"\x1b"[..]));
        assert_eq!(
            text(k::ISO_Left_Tab, M::SHIFT_MASK).as_deref(),
            Some(&b"\x1b[Z"[..])
        );
    }

    #[test]
    fn delete_and_the_keypad_are_named_before_any_text() {
        assert_eq!(named(k::Delete, M::empty()), Some((Named::Delete, 0)));
        assert_eq!(named(k::KP_Delete, M::empty()), Some((Named::Delete, 0)));
        assert_eq!(named(k::KP_Up, M::empty()), Some((Named::Up, 0)));
        assert_eq!(
            named(k::KP_Page_Down, M::CONTROL_MASK),
            Some((Named::PageDown, CONTROL))
        );
        assert_eq!(named(k::Home, M::SHIFT_MASK), Some((Named::Home, SHIFT)));
        assert_eq!(text(k::KP_1, M::MOD2_MASK).as_deref(), Some(&b"1"[..]));
    }

    #[test]
    fn function_keys_are_f1_to_f12_and_insert_with_shift_or_control_sends_nothing() {
        assert_eq!(named(k::F1, M::empty()), Some((Named::F(1), 0)));
        assert_eq!(named(k::F12, M::SHIFT_MASK), Some((Named::F(12), SHIFT)));
        assert_eq!(key_input(k::F13, M::empty()), None);
        assert_eq!(named(k::Insert, M::empty()), Some((Named::Insert, 0)));
        assert_eq!(key_input(k::Insert, M::SHIFT_MASK), None);
        assert_eq!(key_input(k::Insert, M::CONTROL_MASK), None);
        assert_eq!(key_input(k::Shift_L, M::SHIFT_MASK), None);
    }

    /// The C key's keymap entries on a us,ru layout pair, as GDK lists them: group, level
    /// and keyval.
    fn ru_c() -> Vec<(i32, i32, Key)> {
        vec![
            (0, 0, k::c),
            (0, 1, k::C),
            (1, 0, k::Cyrillic_es),
            (1, 1, k::Cyrillic_ES),
        ]
    }

    #[test]
    fn control_on_a_non_latin_layout_uses_the_keys_latin_group() {
        let c = control_latin(k::Cyrillic_es, M::CONTROL_MASK, ru_c);
        assert_eq!(c, k::c);
        assert_eq!(text(c, M::CONTROL_MASK).as_deref(), Some(&b"\x03"[..]));
        assert_eq!(
            control_latin(k::Cyrillic_ES, M::CONTROL_MASK | M::SHIFT_MASK, ru_c),
            k::C
        );
        let greek_c = || vec![(0, 0, k::c), (0, 1, k::C), (1, 0, k::Greek_psi)];
        assert_eq!(control_latin(k::Greek_psi, M::CONTROL_MASK, greek_c), k::c);
        assert_eq!(
            control_latin(k::Cyrillic_es, M::CONTROL_MASK | M::MOD1_MASK, ru_c),
            k::c
        );
    }

    #[test]
    fn without_control_or_a_latin_entry_the_keyval_stays() {
        let none = || -> Vec<(i32, i32, Key)> { panic!("no keymap lookup expected") };
        assert_eq!(
            control_latin(k::Cyrillic_es, M::empty(), none),
            k::Cyrillic_es
        );
        assert_eq!(
            control_latin(k::Cyrillic_es, M::MOD1_MASK, none),
            k::Cyrillic_es
        );
        assert_eq!(control_latin(k::c, M::CONTROL_MASK, none), k::c);
        assert_eq!(control_latin(k::Return, M::CONTROL_MASK, none), k::Return);
        assert_eq!(control_latin(k::Up, M::CONTROL_MASK, none), k::Up);
        let cyrillic_only = || vec![(0, 0, k::Cyrillic_es)];
        assert_eq!(
            control_latin(k::Cyrillic_es, M::CONTROL_MASK, cyrillic_only),
            k::Cyrillic_es
        );
    }
}
