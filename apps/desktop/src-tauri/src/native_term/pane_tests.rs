use super::*;

fn attached(output: &[u8]) -> Pane {
    let mut pane = Pane::new(7);
    assert!(pane.on_attached(20, 4));
    assert!(pane.on_output(output));
    pane
}

#[test]
fn named_keys_follow_cursor_mode() {
    assert_eq!(named_bytes(Named::Up, false), b"\x1b[A");
    assert_eq!(named_bytes(Named::Up, true), b"\x1bOA");
    assert_eq!(named_bytes(Named::BackTab, true), b"\x1b[Z");
    assert_eq!(named_bytes(Named::Delete, false), b"\x1b[3~");
}

#[test]
fn nothing_applies_before_the_attach_and_sizes_are_bounded() {
    let mut pane = Pane::new(1);
    assert_eq!(pane.apply(Input::Text(b"a".to_vec())), Action::Nothing);
    assert!(!pane.on_output(b"x"));
    assert!(!pane.on_attached(1, 24));
    assert!(!pane.on_attached(80, 1001));
    assert!(pane.on_attached(80, 24));
    assert!(pane.ready());
}

#[test]
fn typed_text_and_keys_ask_for_bytes_that_are_not_wheel() {
    let mut pane = attached(b"");
    assert_eq!(
        pane.apply(Input::Text(b"ls\r".to_vec())),
        Action::Input {
            bytes: b"ls\r".to_vec(),
            wheel: false
        }
    );
    assert!(pane.on_output(b"\x1b[?1h"));
    assert_eq!(
        pane.apply(Input::Key(Named::Up)),
        Action::Input {
            bytes: b"\x1bOA".to_vec(),
            wheel: false
        }
    );
    assert_eq!(pane.apply(Input::Text(Vec::new())), Action::Nothing);
}

#[test]
fn keys_off_types_and_pastes_nothing_but_still_scrolls() {
    let mut pane = attached(&b"line\r\n".repeat(30));
    pane.set_keys(false);
    assert_eq!(pane.apply(Input::Text(b"a".to_vec())), Action::Nothing);
    assert_eq!(
        pane.apply(Input::Paste { text: "a".into() }),
        Action::Nothing
    );
    assert_eq!(
        pane.apply(Input::Wheel {
            lines: 2,
            cell: (0, 0)
        }),
        Action::Local
    );
    assert!(pane.behind());
    pane.set_keys(true);
    assert!(matches!(
        pane.apply(Input::Text(b"a".to_vec())),
        Action::Input { .. }
    ));
    assert!(!pane.behind());
}

#[test]
fn the_wheel_in_mouse_mode_asks_for_wheel_reports() {
    let mut pane = attached(b"\x1b[?1000h\x1b[?1006h");
    assert!(pane.mouse_mode());
    assert_eq!(
        pane.apply(Input::Wheel {
            lines: 1,
            cell: (2, 3)
        }),
        Action::Input {
            bytes: codec::sgr_wheel(true, 2, 3),
            wheel: true
        }
    );
    assert_eq!(
        pane.apply(Input::Press {
            cell: (99, 99),
            option: false
        }),
        Action::Input {
            bytes: codec::sgr_button(0, 19, 3, true),
            wheel: false
        }
    );
    assert_eq!(
        pane.apply(Input::Press {
            cell: (1, 1),
            option: true
        }),
        Action::Local
    );
}

#[test]
fn the_wheel_on_the_alternate_screen_sends_arrows_as_wheel_keys() {
    let mut pane = attached(b"\x1b[?1049h\x1b[?1007h");
    assert_eq!(
        pane.apply(Input::Wheel {
            lines: -2,
            cell: (0, 0)
        }),
        Action::Input {
            bytes: b"\x1bOB\x1bOB".to_vec(),
            wheel: true
        }
    );
}

#[test]
fn the_wheel_scrolls_locally_and_jump_returns_to_the_bottom() {
    let mut pane = attached(&b"row\r\n".repeat(40));
    assert!(!pane.behind());
    assert_eq!(
        pane.apply(Input::Wheel {
            lines: 3,
            cell: (0, 0)
        }),
        Action::Local
    );
    assert!(pane.behind());
    assert!(pane.jump());
    assert!(!pane.behind());
    assert!(!pane.jump());
}

#[test]
fn a_paste_passes_the_owner_text_unchanged_for_the_engine_to_clean() {
    let mut pane = attached(b"");
    let text = "a\x1b[201~b\r\nc".to_owned();
    assert_eq!(
        pane.apply(Input::Paste { text: text.clone() }),
        Action::Paste { text }
    );
    assert_eq!(
        pane.apply(Input::Paste {
            text: String::new()
        }),
        Action::Nothing
    );
}

#[test]
fn copy_takes_the_selection_or_the_whole_output() {
    let mut pane = attached(b"hello world\r\nnext");
    assert_eq!(pane.copy(false), Err("native_terminal_copy_empty"));
    assert_eq!(
        pane.apply(Input::Press {
            cell: (0, 0),
            option: false
        }),
        Action::Local
    );
    assert_eq!(
        pane.apply(Input::Drag {
            cell: (4, 0),
            option: false
        }),
        Action::Local
    );
    assert_eq!(pane.copy(false).as_deref(), Ok("hello"));
    let all = pane.copy(true).expect("whole output");
    assert!(all.contains("hello world") && all.contains("next"));
}

#[test]
fn resize_reports_only_a_change() {
    let mut pane = Pane::new(3);
    assert_eq!(pane.resize((80, 24)), None);
    assert!(pane.on_attached(80, 24));
    assert_eq!(pane.resize((80, 24)), None);
    assert_eq!(pane.resize((90, 30)), Some((90, 30)));
}
