use super::*;

#[test]
fn tmux_output_octal_escapes_decode_to_the_exact_bytes() {
    assert_eq!(
        decode(b"\\033[31mred\\033[0m \xe2\x9c\x93 back\\134slash\\015\\012"),
        b"\x1b[31mred\x1b[0m \xe2\x9c\x93 back\\slash\r\n"
    );
    assert_eq!(decode(b"sh-3.2$ "), b"sh-3.2$ ");
    assert_eq!(decode(b"\\9 \\12 \\777"), b"\\9 \\12 \\777");
    assert_eq!(decode(b""), b"");
}

/// A request longer than one `send-keys` run is pasted through a tmux
/// buffer unless it holds a NUL, which would end the buffer; a request of at most one run,
/// or one holding a NUL, is typed as before.
#[test]
fn a_request_is_pasted_only_above_one_run_and_without_a_nul() {
    for length in [
        1,
        SEND_KEYS_BYTES_MAX - 1,
        SEND_KEYS_BYTES_MAX,
        SEND_KEYS_BYTES_MAX + 1,
        2 * SEND_KEYS_BYTES_MAX,
        INPUT_BYTES_MAX,
    ] {
        let bytes = vec![b'a'; length];
        assert_eq!(
            pastes(&bytes),
            length > SEND_KEYS_BYTES_MAX,
            "{length} bytes"
        );
        for at in [0, length / 2, length - 1] {
            let mut nul = bytes.clone();
            nul[at] = 0;
            assert!(!pastes(&nul), "{length} bytes with a NUL at {at}");
        }
    }
}

/// Every one of the 255 non-NUL bytes round-trips through the
/// `set-buffer` quoting, which writes only ASCII letters, digits and `\ooo` escapes inside
/// its double quotes, so tmux's parser expands no `$HOME`, `~` or `#{...}`; `paste-buffer`
/// then pastes that buffer unchanged into the target and deletes it.
#[test]
fn paste_quoting_round_trips_every_non_nul_byte_and_never_expands_a_dollar() {
    let mut bytes: Vec<u8> = (1..=255u8).collect();
    bytes.extend_from_slice(b"$HOME ${HOME} ~ #{pane_id} \\ \" ' ; {");
    let lines = paste_buffer_lines("%7", "crazyagent-paste-1-2", &bytes);
    assert_eq!(lines.len(), 2);
    assert_eq!(
        lines[1],
        "paste-buffer -d -r -S -b crazyagent-paste-1-2 -t %7"
    );
    let quoted = lines[0]
        .strip_prefix("set-buffer -b crazyagent-paste-1-2 -- \"")
        .and_then(|rest| rest.strip_suffix('"'))
        .expect("set-buffer with one double-quoted argument");
    assert!(!quoted.contains('$'));
    let mut decoded = Vec::new();
    let mut chars = quoted.bytes();
    while let Some(byte) = chars.next() {
        if byte.is_ascii_alphanumeric() {
            decoded.push(byte);
            continue;
        }
        assert_eq!(byte, b'\\', "a raw {byte:#04x} inside the quotes");
        let digits: Vec<u8> = chars.by_ref().take(3).collect();
        assert_eq!(digits.len(), 3);
        assert!(digits.iter().all(|digit| (b'0'..=b'7').contains(digit)));
        let text = std::str::from_utf8(&digits).unwrap();
        decoded.push(u8::from_str_radix(text, 8).unwrap());
    }
    assert_eq!(decoded, bytes);
}

/// The engine pins `set-clipboard external` and `get-clipboard off` after
/// today's server options, which stay the same and in the same order, on the engine's server
/// and on a confined lead's.
#[test]
fn the_server_options_are_todays_then_the_two_clipboard_pins() {
    for confined in [false, true] {
        let mut today = vec![
            format!("set-option -g default-terminal {TERMINAL_TYPE}"),
            "set-option -gw allow-rename off".to_owned(),
            "set-option -gw automatic-rename off".to_owned(),
            "set-option -gw remain-on-exit off".to_owned(),
            format!("set-option -g history-limit {SCROLLBACK_LINES}"),
        ];
        if confined {
            today.push(format!("set-option -g default-shell {LEAD_SHELL}"));
        }
        let options = server_options(confined);
        assert_eq!(options[..today.len()], today[..]);
        assert_eq!(
            options[today.len()..],
            [
                "set-option -sq set-clipboard external",
                "set-option -sq get-clipboard off"
            ]
        );
    }
}

#[test]
fn quoted_arguments_keep_every_character_inside_one_tmux_word() {
    assert_eq!(
        quote("a\"b\\c$HOME\nline2\ttab #x ~y'z"),
        "\"a\\\"b\\\\c\\$HOME\\012line2\\011tab #x ~y'z\""
    );
    assert_eq!(quote(""), "\"\"");
    assert_eq!(quote("\u{e9}\u{7f}"), "\"\u{e9}\\177\"");
    assert!(!quote("one\ntwo\r").contains(['\n', '\r']));
}

#[test]
fn the_attach_screen_redraws_every_row_and_puts_the_cursor_back() {
    let screen = render(
        &[b"ready$ echo hi".to_vec(), b"hi".to_vec(), Vec::new()],
        80,
        2,
        (7, 1),
        Modes::default(),
    );
    assert_eq!(
        screen,
        b"\x1b[0m\x1b[H\x1b[2Jready$ echo hi\r\nhi\x1b[0m\x1b[2;8H".to_vec(),
        "rows past the terminal height are dropped"
    );
    assert!(render(&[], 80, 24, (500, 500), Modes::default()).ends_with(b"\x1b[24;80H"));
}

#[test]
fn the_attach_screen_first_turns_on_the_input_modes_the_app_set() {
    let both = Modes {
        bracketed_paste: true,
        cursor_keys: true,
        ..Modes::default()
    };
    assert!(
        render(&[b"x".to_vec()], 80, 24, (0, 0), both).starts_with(b"\x1b[?2004h\x1b[?1h\x1b[0m")
    );
    let paste = Modes {
        bracketed_paste: true,
        ..Modes::default()
    };
    assert!(render(&[], 80, 24, (0, 0), paste).starts_with(b"\x1b[?2004h\x1b[0m"));
    assert!(render(&[], 80, 24, (0, 0), Modes::default()).starts_with(b"\x1b[0m"));
}

#[test]
fn the_attach_history_fills_a_fresh_view_scrollback_and_leaves_the_screen_to_redraw() {
    assert!(scrollback(&[], 24).is_empty());
    let history = scrollback(&[b"one".to_vec(), b"\x1b[1mtwo".to_vec()], 3);
    assert_eq!(
        history,
        b"\x1b[0m\x1b[H\x1b[2Jone\r\n\x1b[1mtwo\r\n\x1b[0m\n\n".to_vec()
    );
    for count in [1_usize, 2, 3, 4, 40] {
        let lines: Vec<Vec<u8>> = (1..=count)
            .map(|line| format!("old {line}").into_bytes())
            .collect();
        let screen = render(
            &[b"now 1".to_vec(), b"now 2".to_vec(), b"now 3".to_vec()],
            20,
            3,
            (5, 2),
            Modes::default(),
        );
        let mut parser = vt100::Parser::new(3, 20, 100);
        parser.process(&scrollback(&lines, 3));
        parser.process(&screen);
        let shown: Vec<String> = parser.screen().rows(0, 20).collect();
        assert_eq!(shown, ["now 1", "now 2", "now 3"], "{count} history rows");
        parser.set_scrollback(usize::MAX);
        assert_eq!(
            parser.screen().scrollback(),
            count,
            "every history row and nothing else is in the scrollback"
        );
        let window = count.min(3);
        parser.set_scrollback(window);
        let top = format!("old {}", count - window + 1);
        assert_eq!(
            parser.screen().rows(0, 20).next().unwrap(),
            top,
            "{count} history rows"
        );
    }
}
