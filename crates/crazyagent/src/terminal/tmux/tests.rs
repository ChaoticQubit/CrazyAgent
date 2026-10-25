use super::*;
use std::path::PathBuf;

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
fn only_tmux_three_two_and_later_is_supported() {
    for (version, expected) in [
        ("3.8", Some(true)),
        ("3.2", Some(true)),
        ("3.3a", Some(true)),
        ("next-3.6", Some(true)),
        ("10.0", Some(true)),
        ("3.1c", Some(false)),
        ("2.9", Some(false)),
        ("master", None),
        ("", None),
    ] {
        assert_eq!(supported(version), expected, "{version}");
    }
}

#[test]
fn the_attach_screen_keeps_written_trailing_cells_from_tmux_three_four() {
    for (version, expected) in [
        ("3.2", CAPTURE_TRIMMED),
        ("3.3a", CAPTURE_TRIMMED),
        ("3.4", CAPTURE_WRITTEN),
        ("3.8", CAPTURE_WRITTEN),
        ("next-3.6", CAPTURE_WRITTEN),
        ("10.0", CAPTURE_WRITTEN),
    ] {
        assert_eq!(capture_flags(version), Some(expected), "{version}");
    }
    assert_eq!(capture_flags("3.1c"), None);
    assert_eq!(capture_flags("master"), None);
    assert_eq!(CAPTURE_WRITTEN, "-p -e -N -T");
    assert_eq!(CAPTURE_TRIMMED, "-p -e");
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
    };
    assert!(
        render(&[b"x".to_vec()], 80, 24, (0, 0), both).starts_with(b"\x1b[?2004h\x1b[?1h\x1b[0m")
    );
    let paste = Modes {
        bracketed_paste: true,
        cursor_keys: false,
    };
    assert!(render(&[], 80, 24, (0, 0), paste).starts_with(b"\x1b[?2004h\x1b[0m"));
    assert!(render(&[], 80, 24, (0, 0), Modes::default()).starts_with(b"\x1b[0m"));
}

#[test]
fn an_attach_reads_the_pane_its_window_cursor_and_size() {
    let found = located(&[b"%2 @2 8 0 80 24 1 0".to_vec()]).unwrap();
    assert_eq!(
        (
            found.pane,
            found.window,
            found.cols,
            found.rows,
            found.cursor
        ),
        (b"%2".to_vec(), b"@2".to_vec(), 80, 24, (8, 0))
    );
    assert_eq!(
        found.modes,
        Modes {
            bracketed_paste: true,
            cursor_keys: false
        }
    );
    assert!(located(&[b"%2 @2 8 0 80 24".to_vec()]).is_none());
    assert!(located(&[b"%2 @2 8 0 80 24 1 x".to_vec()]).is_none());
    assert!(located(&[b"  0 0 80 24 0 0".to_vec()]).is_none());
    assert!(located(&[]).is_none());
}

#[test]
fn spawn_commands_go_through_env_with_every_argument_quoted() {
    let agent = AgentId::team(4).unwrap();
    let command = InteractiveCommand {
        argv: vec!["/bin/sh".into(), "-c".into(), "echo \"$FOO\"".into()],
        cwd: PathBuf::from("/private/tmp/project one"),
        env: vec![("FOO".into(), "bar baz".into())],
        transcript: crate::harnesses::interactive::Transcript::Unavailable,
    };
    assert_eq!(
        spawn_line(&agent, &command).unwrap(),
        "new-window -d -P -F '#{pane_id} #{pane_pid}' -n team-4 -c \"/private/tmp/project one\" -t crazyagent: -- /usr/bin/env -i \"TERM=xterm-256color\" \"COLORTERM=truecolor\" \"FOO=bar baz\" \"/bin/sh\" \"-c\" \"echo \\\"\\$FOO\\\"\""
    );
    let mut own_terminal = command.clone();
    own_terminal.env.insert(0, ("TERM".into(), "dumb".into()));
    own_terminal
        .env
        .insert(1, ("COLORTERM".into(), "24bit".into()));
    assert!(
        spawn_line(&agent, &own_terminal).unwrap().contains(
            " -- /usr/bin/env -i \"TERM=dumb\" \"COLORTERM=24bit\" \"FOO=bar baz\" \"/bin/sh\""
        ),
        "a command's own terminal settings replace the defaults"
    );
    let refused = |change: fn(&mut InteractiveCommand)| {
        let mut bad = command.clone();
        change(&mut bad);
        spawn_line(&agent, &bad).unwrap_err().0
    };
    let changes: [fn(&mut InteractiveCommand); 12] = [
        |c| c.argv.clear(),
        |c| c.argv[0] = "-i".into(),
        |c| c.argv[0] = "A=b".into(),
        |c| c.argv.push("nul\0".into()),
        |c| c.cwd = PathBuf::from("relative"),
        |c| c.cwd = PathBuf::from("/tmp/#{pane_id}"),
        |c| c.env.push(("1BAD".into(), "x".into())),
        |c| c.env.push(("BAD-NAME".into(), "x".into())),
        |c| c.env.push(("GH_TOKEN".into(), "planted".into())),
        |c| {
            c.env
                .push(("SSH_AUTH_SOCK".into(), "/tmp/agent.sock".into()))
        },
        |c| {
            c.env
                .push(("ANTHROPIC_API_KEY".into(), "sk-ant-planted".into()))
        },
        |c| c.argv.push("x".repeat(COMMAND_BYTES_MAX)),
    ];
    for change in changes {
        assert_eq!(refused(change), "terminal_command_invalid");
    }
}

#[test]
fn show_environment_replies_name_only_the_variables_still_set() {
    let reply: Vec<Vec<u8>> = [
        "PATH=/usr/bin:/bin",
        "-SSH_AUTH_SOCK",
        "GH_TOKEN=planted=with=equals",
        "EMPTY=",
        "no-equals",
    ]
    .iter()
    .map(|line| line.as_bytes().to_vec())
    .collect();
    assert_eq!(variable_names(&reply), ["PATH", "GH_TOKEN", "EMPTY"]);
    assert!(variable_names(&[b"-REMOVED".to_vec()]).is_empty());
}
