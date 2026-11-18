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
            named(k::ISO_Left_Tab, M::SHIFT_MASK),
            Some((Named::BackTab, SHIFT))
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
}
