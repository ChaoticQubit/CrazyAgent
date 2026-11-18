use super::*;

fn attached(output: &[u8]) -> Pane {
    let mut pane = Pane::new(7, true);
    assert!(pane.on_attached(20, 4));
    assert!(pane.on_output(output));
    pane
}

#[test]
fn a_pane_opened_with_keys_off_sends_nothing_from_its_first_key_click_or_paste() {
    let mut pane = Pane::new(5, false);
    assert!(pane.on_attached(20, 4));
    assert!(pane.on_output(b"\x1b[?1000h\x1b[?1006h"));
    assert!(pane.mouse_mode());
    assert_eq!(pane.apply(Input::Text(b"y\r".to_vec())), Action::Nothing);
    let up = Input::Key {
        key: Named::Up,
        mods: 0,
    };
    assert_eq!(pane.apply(up), Action::Nothing);
    let click = Input::Press {
        cell: (1, 1),
        option: false,
        clicks: 1,
    };
    assert_eq!(pane.apply(click), Action::Local);
    let paste = Input::Paste { text: "y".into() };
    assert_eq!(pane.apply(paste), Action::Nothing);
    assert_eq!(pane.seed("y"), Err("native_terminal_keys_off"));
}

#[test]
fn a_seed_flattens_line_breaks_unless_the_pane_reads_pastes_bracketed() {
    let mut early = Pane::new(6, true);
    assert_eq!(early.seed("a"), Err("native_terminal_view_unknown"));
    let mut pane = attached(b"");
    let flat = Action::Paste {
        text: "a b c d".into(),
    };
    assert_eq!(pane.seed("a\r\n\r\nb\rc\nd"), Ok(flat));
    let Ok(Action::Paste { text }) = pane.seed("a\r\x0b\x0c\u{85}\u{2028}b\n\r") else {
        panic!("a seed with odd line breaks is a paste");
    };
    let keys = crazyagent::terminal::paste_keys(&text);
    assert!(!keys.contains(&b'\r') && !keys.contains(&b'\n'), "{keys:?}");
    assert_eq!(pane.seed(""), Ok(Action::Nothing));
    let over = "x".repeat(PASTE_BYTES_MAX + 1);
    assert_eq!(pane.seed(&over), Err("native_terminal_paste_too_large"));
    let full = "x".repeat(PASTE_BYTES_MAX);
    assert!(matches!(pane.seed(&full), Ok(Action::Paste { .. })));
    assert!(pane.on_output(b"\x1b[?2004h"));
    let kept = Action::Paste {
        text: "a\r\nb".into(),
    };
    assert_eq!(pane.seed("a\r\nb"), Ok(kept));
}

#[test]
fn named_keys_follow_cursor_mode() {
    assert_eq!(named_bytes(Named::Up, 0, false), b"\x1b[A");
    assert_eq!(named_bytes(Named::Up, 0, true), b"\x1bOA");
    assert_eq!(named_bytes(Named::Home, 0, true), b"\x1bOH");
    assert_eq!(named_bytes(Named::BackTab, 0, true), b"\x1b[Z");
    assert_eq!(named_bytes(Named::Delete, 0, false), b"\x1b[3~");
}

#[test]
fn modified_and_function_keys_match_the_xterm_panel() {
    assert_eq!(named_bytes(Named::Left, ALT, true), b"\x1b[1;3D");
    assert_eq!(
        named_bytes(Named::End, SHIFT | CONTROL, false),
        b"\x1b[1;6F"
    );
    assert_eq!(named_bytes(Named::Delete, SHIFT, false), b"\x1b[3;2~");
    assert_eq!(named_bytes(Named::PageUp, ALT, false), b"\x1b[5~");
    assert_eq!(named_bytes(Named::PageDown, CONTROL, false), b"\x1b[6;5~");
    assert_eq!(named_bytes(Named::Insert, 0, false), b"\x1b[2~");
    assert_eq!(named_bytes(Named::F(1), 0, false), b"\x1bOP");
    assert_eq!(named_bytes(Named::F(4), SHIFT, false), b"\x1b[1;2S");
    assert_eq!(named_bytes(Named::F(5), 0, false), b"\x1b[15~");
    assert_eq!(named_bytes(Named::F(11), 0, false), b"\x1b[23~");
    assert_eq!(named_bytes(Named::F(12), CONTROL, false), b"\x1b[24;5~");
}

#[test]
fn nothing_applies_before_the_attach_and_sizes_are_bounded() {
    let mut pane = Pane::new(1, true);
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
        pane.apply(Input::Key {
            key: Named::Up,
            mods: 0
        }),
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
            option: false,
            clicks: 1
        }),
        Action::Input {
            bytes: codec::sgr_button(0, 19, 3, true),
            wheel: false
        }
    );
    assert_eq!(
        pane.apply(Input::Press {
            cell: (1, 1),
            option: true,
            clicks: 1
        }),
        Action::Local
    );
}

#[test]
fn the_wheel_on_the_alternate_screen_sends_one_arrow_by_cursor_mode() {
    let mut pane = attached(b"\x1b[?1049h");
    let wheel = |lines| Input::Wheel {
        lines,
        cell: (0, 0),
    };
    assert_eq!(
        pane.apply(wheel(-3)),
        Action::Input {
            bytes: b"\x1b[B".to_vec(),
            wheel: true
        }
    );
    assert!(pane.on_output(b"\x1b[?1h"));
    assert_eq!(
        pane.apply(wheel(2)),
        Action::Input {
            bytes: b"\x1bOA".to_vec(),
            wheel: true
        }
    );
}

#[test]
fn with_keys_off_the_wheel_still_reaches_the_agent_but_presses_select() {
    let mut pane = attached(b"\x1b[?1000h\x1b[?1006h");
    pane.set_keys(false);
    assert_eq!(
        pane.apply(Input::Wheel {
            lines: 4,
            cell: (0, 0)
        }),
        Action::Input {
            bytes: codec::sgr_wheel(true, 0, 0),
            wheel: true
        }
    );
    let press = Input::Press {
        cell: (0, 0),
        option: false,
        clicks: 1,
    };
    assert_eq!(pane.apply(press), Action::Local);
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
            option: false,
            clicks: 1
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
    let mut pane = Pane::new(3, true);
    assert_eq!(pane.resize((80, 24)), None);
    assert!(pane.on_attached(80, 24));
    assert_eq!(pane.resize((80, 24)), None);
    assert_eq!(pane.resize((90, 30)), Some((90, 30)));
}

#[test]
fn clicks_select_a_word_or_a_line_and_select_all_takes_everything() {
    let mut pane = attached(b"hello world\r\nnext");
    let press = |clicks| Input::Press {
        cell: (7, 0),
        option: false,
        clicks,
    };
    assert_eq!(pane.apply(press(2)), Action::Local);
    assert_eq!(pane.copy(false).as_deref(), Ok("world"));
    assert_eq!(pane.apply(press(3)), Action::Local);
    assert_eq!(
        pane.copy(false).as_deref().map(str::trim_end),
        Ok("hello world")
    );
    assert!(pane.select_all());
    let all = pane.copy(false).expect("all selected");
    assert!(all.contains("hello world") && all.contains("next"));
}

#[test]
fn shift_page_keys_scroll_locally_and_others_go_to_the_agent() {
    let mut pane = attached(&b"row\r\n".repeat(40));
    let page = |key, mods| Input::Key { key, mods };
    assert_eq!(pane.apply(page(Named::PageUp, SHIFT)), Action::Local);
    assert!(pane.behind());
    assert_eq!(pane.apply(page(Named::PageDown, SHIFT)), Action::Local);
    assert!(!pane.behind());
    assert_eq!(
        pane.apply(page(Named::PageUp, 0)),
        Action::Input {
            bytes: b"\x1b[5~".to_vec(),
            wheel: false
        }
    );
}
