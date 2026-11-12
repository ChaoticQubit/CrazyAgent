use super::*;
use std::path::PathBuf;

#[cfg(target_os = "macos")]
#[path = "../../../tests/pgid_support/mod.rs"]
mod pgid_support;

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
fn the_attach_screen_puts_a_full_screen_app_back_on_its_alternate_screen_with_its_mouse_modes() {
    let full_screen = Modes {
        bracketed_paste: true,
        alternate_screen: true,
        mouse_button: true,
        mouse_sgr: true,
        ..Modes::default()
    };
    assert!(
        render(&[b"x".to_vec()], 80, 24, (0, 0), full_screen)
            .starts_with(b"\x1b[?1049h\x1b[?2004h\x1b[?1002h\x1b[?1006h\x1b[0m\x1b[H\x1b[2Jx"),
        "the screen is drawn on the alternate screen, with the wheel reported to the app"
    );
    let every = Modes {
        mouse_standard: true,
        mouse_all: true,
        mouse_utf8: true,
        ..Modes::default()
    };
    assert!(
        render(&[], 80, 24, (0, 0), every).starts_with(b"\x1b[?1000h\x1b[?1003h\x1b[?1005h\x1b[0m")
    );
}

#[test]
fn an_attach_reads_the_pane_its_window_cursor_and_size() {
    let found = located(&[b"%2 @2 8 0 80 24 1 0 312 0 0 0 0 0 0 end".to_vec()]).unwrap();
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
            ..Modes::default()
        }
    );
    assert_eq!(found.history, 312);
    assert_eq!(
        located(&[b"%2 @2 8 0 80 24 0 1 0 1 0 1 0 1 0 end".to_vec()])
            .unwrap()
            .modes,
        Modes {
            cursor_keys: true,
            alternate_screen: true,
            mouse_button: true,
            mouse_sgr: true,
            ..Modes::default()
        }
    );
    assert_eq!(
        located(&[b"%2 @2 8 0 80 24 0 0 0 1 1   1  end".to_vec()])
            .unwrap()
            .modes,
        Modes {
            alternate_screen: true,
            mouse_standard: true,
            mouse_sgr: true,
            ..Modes::default()
        },
        "a mode flag an older tmux leaves empty reads as off"
    );
    let full = format!(
        "%2 @2 8 0 80 24 1 0 {} 0 0 0 0 0 0{FORMAT_END}",
        u64::from(SCROLLBACK_LINES) * 3
    );
    assert_eq!(
        located(&[full.into_bytes()]).unwrap().history,
        HISTORY_ROWS_MAX,
        "history past what the capture asks for is clamped to it"
    );
    assert!(located(&[b"%2 @2 8 0 80 24 end".to_vec()]).is_none());
    assert!(located(&[b"%2 @2 8 0 80 24 1 0 end".to_vec()]).is_none());
    assert!(located(&[b"%2 @2 8 0 80 24 1 x 0 end".to_vec()]).is_none());
    assert!(located(&[b"%2 @2 8 0 80 24 1 0 -4 end".to_vec()]).is_none());
    assert!(located(&[b"%2 @2 8 0 80 24 1 0 312 end".to_vec()]).is_none());
    assert!(located(&[b"%2 @2 8 0 80 24 1 0 312 2 0 0 0 0 0 end".to_vec()]).is_none());
    assert!(located(&[b"%2 @2 8 0 80 24 1 0 312 0 0 0 0 0 0 0 end".to_vec()]).is_none());
    assert!(located(&[b"  0 0 80 24 0 0 0 end".to_vec()]).is_none());
    assert!(located(&[]).is_none());
    assert!(
        located(&[b"%2 @2 8 0 80 24 1 0 312 0 0 0 0 0 0".to_vec()]).is_none(),
        "a line without the end mark is one tmux cut short"
    );
    assert!(located(&[b"%1 @1 0 ".to_vec()]).is_none());
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

/// A native lead's own server takes the lead's command back out of the engine's own keeper
/// and pane shim, which cannot run inside the server's sandbox, and checks what is left as
/// strictly as a plain command; any other wrapper, keeper or shim text is refused.
#[cfg(target_os = "macos")]
#[test]
fn a_confined_lead_line_takes_only_the_engines_own_keeper_back_out() {
    use crate::sandbox::{KEEPER, PANE_SHIM, SANDBOX_EXEC, kept_pane};
    let agent = AgentId::team(4).unwrap();
    let profile = Path::new("/private/tmp/lead.sb");
    let plain = |profile: &str| -> Vec<String> {
        [
            SANDBOX_EXEC,
            "-f",
            profile,
            "/usr/local/bin/claude",
            "--resume",
        ]
        .map(String::from)
        .to_vec()
    };
    let command = |argv: Vec<String>| InteractiveCommand {
        argv,
        cwd: PathBuf::from("/private/tmp/project"),
        env: vec![],
        transcript: crate::harnesses::interactive::Transcript::Unavailable,
    };
    let line = |argv: Vec<String>| confined_line(&agent, &command(argv), profile, &[]);
    let lead = plain("/private/tmp/lead.sb");
    let kept = kept_pane(lead.clone()).unwrap();
    assert_eq!(
        line(kept.clone()).map_err(|error| error.0),
        Ok(line(lead.clone()).unwrap()),
        "the lead command the engine kept is checked as the plain one"
    );

    let shim = KEEPER.len() + 3;
    let mut keeper_text = kept.clone();
    keeper_text[KEEPER.len() - 2].push(' ');
    let mut shim_text = kept.clone();
    shim_text[shim + PANE_SHIM.len() - 2].push(' ');
    let mut no_shim = kept.clone();
    no_shim.drain(shim..shim + PANE_SHIM.len());
    let mut not_sandboxed = kept.clone();
    not_sandboxed[KEEPER.len()] = "/bin/sh".into();
    let mut wrapped = vec![
        "/usr/bin/perl".to_owned(),
        "-e".into(),
        "exec @ARGV".into(),
        "--".into(),
    ];
    wrapped.extend(lead.clone());
    let refused = [
        kept_pane(plain("/private/tmp/other.sb")).unwrap(),
        keeper_text,
        shim_text,
        no_shim,
        not_sandboxed,
        wrapped,
        kept[..KEEPER.len()]
            .iter()
            .chain(&kept[shim..])
            .cloned()
            .collect(),
    ];
    for argv in refused {
        assert_eq!(
            line(argv.clone()).unwrap_err().0,
            "terminal_command_invalid",
            "accepted {argv:?}"
        );
    }
}

/// Keeper r1 KP-L3: a pane's keeper is a perl outside the sandbox, so no pane command may
/// carry a variable that makes perl load code from elsewhere, in any letter case.
#[test]
fn a_pane_command_with_a_perl_load_variable_is_refused() {
    let agent = AgentId::team(4).unwrap();
    let command = |name: &str| InteractiveCommand {
        argv: vec!["/usr/local/bin/claude".into()],
        cwd: PathBuf::from("/private/tmp/project"),
        env: vec![(name.into(), "/private/tmp/planted".into())],
        transcript: crate::harnesses::interactive::Transcript::Unavailable,
    };
    assert!(spawn_line(&agent, &command("TERM")).is_ok());
    for name in ["PERL5LIB", "PERL5OPT", "PERLLIB", "PERL5DB", "perl5opt"] {
        assert_eq!(
            spawn_line(&agent, &command(name)).map_err(|error| error.0),
            Err("terminal_command_invalid".to_owned()),
            "{name}"
        );
    }
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
        "new-window -d -P -F '#{pane_id} #{pane_pid} end' -n team-4 -c \"/private/tmp/project one\" -t crazyagent: -- /usr/bin/env -i \"TERM=xterm-256color\" \"COLORTERM=truecolor\" \"FOO=bar baz\" \"/bin/sh\" \"-c\" \"echo \\\"\\$FOO\\\"\""
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

#[cfg(target_os = "macos")]
struct Server {
    tmux: PathBuf,
    socket: PathBuf,
}

#[cfg(target_os = "macos")]
impl Drop for Server {
    fn drop(&mut self) {
        let _ = crate::spawn_gate::status(
            Command::new(&self.tmux)
                .arg("-S")
                .arg(&self.socket)
                .arg("kill-server")
                .env_clear()
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null()),
        );
    }
}

#[cfg(target_os = "macos")]
#[test]
fn an_owner_whose_shell_is_csh_keeps_the_private_session_and_its_agents() {
    use std::os::unix::fs::PermissionsExt;
    let home = tempfile::tempdir().unwrap();
    let folder = home.path().join("terminal");
    std::fs::create_dir(&folder).unwrap();
    std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o700)).unwrap();
    let tmux = installed(home.path()).expect("tmux is required for this test");
    let config = home.path().join("owner.conf");
    std::fs::write(&config, "set-option -g default-shell /bin/csh\n").unwrap();
    let host = Tmux::open(Some(tmux.clone()), &folder);
    let _server = Server {
        tmux: tmux.clone(),
        socket: host.socket(),
    };
    let started = crate::spawn_gate::status(
        Command::new(&tmux)
            .args(["-u", "-f"])
            .arg(&config)
            .arg("-S")
            .arg(host.socket())
            .args(["new-session", "-d", "-s", "owner", "/bin/sleep", "60"])
            .env_clear()
            .env("PATH", SERVER_PATH)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null()),
    )
    .unwrap();
    assert!(started.success());
    let shell = crate::spawn_gate::output(
        Command::new(&tmux)
            .arg("-S")
            .arg(host.socket())
            .args(["show-options", "-gv", "default-shell"])
            .env_clear()
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null()),
    )
    .unwrap();
    assert_eq!(String::from_utf8_lossy(&shell.stdout).trim(), "/bin/csh");

    assert_eq!(host.alive().unwrap(), Vec::<AgentId>::new());
    let agent = AgentId::team(1).unwrap();
    let command = InteractiveCommand {
        argv: vec!["/bin/sleep".into(), "30".into()],
        cwd: home.path().to_path_buf(),
        env: Vec::new(),
        transcript: crate::harnesses::interactive::Transcript::Unavailable,
    };
    host.spawn(&agent, &command).unwrap();
    std::thread::sleep(Duration::from_secs(3));
    let windows = crate::spawn_gate::output(
        Command::new(&tmux)
            .arg("-S")
            .arg(host.socket())
            .args(["list-windows", "-t", SESSION, "-F", "#{window_name}"])
            .env_clear()
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null()),
    )
    .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&windows.stdout)
            .lines()
            .collect::<Vec<_>>(),
        vec![HOLDER, "team-1"]
    );
    assert_eq!(host.alive().unwrap(), vec![agent]);
}

#[cfg(target_os = "macos")]
#[test]
fn an_attach_to_a_full_screen_app_reports_the_wheel_to_it_on_its_alternate_screen() {
    use std::os::unix::fs::PermissionsExt;
    let home = tempfile::tempdir().unwrap();
    let folder = home.path().join("terminal");
    std::fs::create_dir(&folder).unwrap();
    std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o700)).unwrap();
    let tmux = installed(home.path()).expect("tmux is required for this test");
    let host = Tmux::open(Some(tmux.clone()), &folder);
    let _server = Server {
        tmux,
        socket: host.socket(),
    };
    let agent = AgentId::team(1).unwrap();
    let command = InteractiveCommand {
        argv: vec![
            "/bin/sh".into(),
            "-c".into(),
            "printf 'before\\n\\033[?1049h\\033[?1002h\\033[?1006h\\033[Hfull screen'; exec /bin/sleep 30"
                .into(),
        ],
        cwd: home.path().to_path_buf(),
        env: Vec::new(),
        transcript: crate::harnesses::interactive::Transcript::Unavailable,
    };
    host.spawn(&agent, &command).unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    let stream = loop {
        let stream = host.attach(&agent).unwrap();
        if String::from_utf8_lossy(&stream.screen).contains("full screen") {
            break stream;
        }
        assert!(
            Instant::now() < deadline,
            "the app drew its full screen in time"
        );
        std::thread::sleep(Duration::from_millis(100));
    };
    let mut parser = vt100::Parser::new(stream.rows, stream.cols, SCROLLBACK_LINES as usize);
    parser.process(&stream.history);
    parser.process(&stream.screen);
    let screen = parser.screen();
    assert!(
        screen.alternate_screen(),
        "the view draws the app on its alternate screen, which has no history to scroll"
    );
    assert_eq!(
        (
            screen.mouse_protocol_mode(),
            screen.mouse_protocol_encoding()
        ),
        (
            vt100::MouseProtocolMode::ButtonMotion,
            vt100::MouseProtocolEncoding::Sgr
        ),
        "the view reports the wheel to the app in the app's own encoding"
    );
    assert!(screen.contents().starts_with("full screen"));
}

#[cfg(target_os = "macos")]
#[test]
fn an_attach_puts_the_pane_history_into_the_view_scrollback() {
    use std::os::unix::fs::PermissionsExt;
    let home = tempfile::tempdir().unwrap();
    let folder = home.path().join("terminal");
    std::fs::create_dir(&folder).unwrap();
    std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o700)).unwrap();
    let tmux = installed(home.path()).expect("tmux is required for this test");
    let host = Tmux::open(Some(tmux.clone()), &folder);
    let _server = Server {
        tmux,
        socket: host.socket(),
    };
    let agent = AgentId::team(1).unwrap();
    let command = InteractiveCommand {
        argv: vec![
            "/bin/sh".into(),
            "-c".into(),
            "/usr/bin/seq 1 300; exec /bin/sleep 30".into(),
        ],
        cwd: home.path().to_path_buf(),
        env: Vec::new(),
        transcript: crate::harnesses::interactive::Transcript::Unavailable,
    };
    host.spawn(&agent, &command).unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    let stream = loop {
        let stream = host.attach(&agent).unwrap();
        if String::from_utf8_lossy(&stream.screen).contains("300") {
            break stream;
        }
        assert!(
            Instant::now() < deadline,
            "the pane printed its lines in time"
        );
        std::thread::sleep(Duration::from_millis(100));
    };
    let (cols, rows) = (stream.cols, stream.rows);
    let mut parser = vt100::Parser::new(rows, cols, SCROLLBACK_LINES as usize);
    parser.process(&stream.history);
    parser.process(&stream.screen);
    let shown: Vec<String> = parser.screen().rows(0, cols).collect();
    let last = usize::from(rows) - 2;
    assert_eq!(shown[last], "300", "the screen ends with the newest line");
    assert_eq!(shown[0], (301 - usize::from(rows) + 1).to_string());
    let history = 301 - usize::from(rows);
    parser.set_scrollback(usize::MAX);
    assert_eq!(parser.screen().scrollback(), history);
    parser.set_scrollback(usize::from(rows));
    let window: Vec<String> = parser.screen().rows(0, cols).collect();
    let expected: Vec<String> = (history - usize::from(rows) + 1..=history)
        .map(|line| line.to_string())
        .collect();
    assert_eq!(
        window, expected,
        "the scrollback ends where the screen begins"
    );
    assert!(
        stream
            .history
            .starts_with(b"\x1b[0m\x1b[H\x1b[2J1\r\n2\r\n3\r\n"),
        "the history starts at the pane's first line"
    );
}

#[cfg(target_os = "macos")]
struct Stalled(rustix::process::Pid);

#[cfg(target_os = "macos")]
impl Drop for Stalled {
    fn drop(&mut self) {
        let _ = rustix::process::kill_process(self.0, rustix::process::Signal::CONT);
    }
}

/// tmux stops expanding a format once it has run for 100 ms of wall time and answers with
/// the text so far as a success, which a server stalled on a loaded machine reaches: an
/// attach then read `%1 @1 0 ` and failed, ending the agent. The real server is stalled
/// again and again while callers attach, list the live agents and start new ones: every
/// call still succeeds with the whole answer.
#[cfg(target_os = "macos")]
#[test]
fn a_reply_tmux_cut_short_while_its_server_stalls_is_asked_again() {
    use std::os::unix::fs::PermissionsExt;
    use std::sync::atomic::{AtomicBool, Ordering};
    const STALLS: usize = 60;
    const CALLERS: usize = 4;
    const STARTED: i64 = 6;
    let home = tempfile::tempdir().unwrap();
    let folder = home.path().join("terminal");
    std::fs::create_dir(&folder).unwrap();
    std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o700)).unwrap();
    let tmux = installed(home.path()).expect("tmux is required for this test");
    let host = Arc::new(Tmux::open(Some(tmux.clone()), &folder));
    let _server = Server {
        tmux: tmux.clone(),
        socket: host.socket(),
    };
    let command = InteractiveCommand {
        argv: vec!["/bin/sleep".into(), "60".into()],
        cwd: home.path().to_path_buf(),
        env: Vec::new(),
        transcript: crate::harnesses::interactive::Transcript::Unavailable,
    };
    let agent = AgentId::team(1).unwrap();
    host.spawn(&agent, &command).unwrap();
    let pid = crate::spawn_gate::output(
        Command::new(&tmux)
            .arg("-S")
            .arg(host.socket())
            .args(["display-message", "-p", "#{pid}"])
            .env_clear()
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null()),
    )
    .unwrap();
    let server = String::from_utf8_lossy(&pid.stdout)
        .trim()
        .parse::<i32>()
        .ok()
        .and_then(rustix::process::Pid::from_raw)
        .unwrap();
    let stalling = Arc::new(AtomicBool::new(true));
    let mut callers: Vec<_> = (0..CALLERS)
        .map(|_| {
            let (host, agent, stalling) = (host.clone(), agent.clone(), stalling.clone());
            std::thread::spawn(move || {
                let mut problems = Vec::new();
                let mut asked = 0;
                while stalling.load(Ordering::SeqCst) {
                    if let Err(problem) = host.screen(&agent) {
                        problems.push(format!("screen: {}", problem.0));
                    }
                    match host.alive() {
                        Ok(alive) if alive.contains(&agent) => {}
                        other => problems.push(format!("alive: {other:?}")),
                    }
                    asked += 1;
                }
                (asked, problems)
            })
        })
        .collect();
    let starter = {
        let (host, command, stalling) = (host.clone(), command.clone(), stalling.clone());
        std::thread::spawn(move || {
            let mut problems = Vec::new();
            for id in 2..=STARTED {
                let started = AgentId::team(id).unwrap();
                match host.spawn(&started, &command) {
                    Ok(pane) if pane.agent == started && pane.pid > 0 => {}
                    other => problems.push(format!("spawn {id}: {other:?}")),
                }
                std::thread::sleep(Duration::from_millis(400));
            }
            while stalling.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(20));
            }
            (0, problems)
        })
    };
    callers.push(starter);
    {
        let _stalled = Stalled(server);
        for _ in 0..STALLS {
            rustix::process::kill_process(server, rustix::process::Signal::STOP).unwrap();
            std::thread::sleep(Duration::from_millis(150));
            rustix::process::kill_process(server, rustix::process::Signal::CONT).unwrap();
            std::thread::sleep(Duration::from_millis(30));
        }
    }
    stalling.store(false, Ordering::SeqCst);
    let mut asked = 0;
    let mut problems = Vec::new();
    for caller in callers {
        let (count, found) = caller.join().unwrap();
        asked += count;
        problems.extend(found);
    }
    assert!(asked >= STALLS, "the callers kept asking: {asked}");
    assert_eq!(problems, Vec::<String>::new());
    let all: Vec<AgentId> = (1..=STARTED).map(|id| AgentId::team(id).unwrap()).collect();
    assert_eq!(host.alive().unwrap(), all);
}

/// Splits a pane of a test's own private tmux server, as a native harness splits its window
/// for a teammate. Takes the server, the pane to split and the new pane's shell command.
/// Panics unless tmux splits it.
#[cfg(target_os = "macos")]
fn split(server: &Server, pane: &str, command: &str) {
    let status = crate::spawn_gate::status(
        Command::new(&server.tmux)
            .arg("-S")
            .arg(&server.socket)
            .args(["split-window", "-d", "-t", pane, command])
            .env_clear()
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null()),
    )
    .unwrap();
    assert!(status.success(), "tmux split the pane");
}

/// A harness's teammate pane is listed under its agent and ended on its own (S4): `panes`
/// lists the panes of the agent's window, and `kill_pane` ends only the named one while it
/// refuses an invalid id, another agent's pane, an unknown pane and the window's last pane.
#[cfg(target_os = "macos")]
#[test]
fn kill_pane_ends_only_that_pane_and_refuses_the_last_and_a_foreign_one() {
    use std::os::unix::fs::PermissionsExt;
    let home = tempfile::tempdir().unwrap();
    let folder = home.path().join("terminal");
    std::fs::create_dir(&folder).unwrap();
    std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o700)).unwrap();
    let tmux = installed(home.path()).expect("tmux is required for this test");
    let host = Tmux::open(Some(tmux.clone()), &folder);
    let server = Server {
        tmux,
        socket: host.socket(),
    };
    let sleeper = || InteractiveCommand {
        argv: vec!["/bin/sh".into(), "-c".into(), "exec /bin/sleep 60".into()],
        cwd: home.path().to_path_buf(),
        env: Vec::new(),
        transcript: crate::harnesses::interactive::Transcript::Unavailable,
    };
    let lead = AgentId::team(1).unwrap();
    let other = AgentId::team(2).unwrap();
    host.spawn(&lead, &sleeper()).unwrap();
    host.spawn(&other, &sleeper()).unwrap();
    let first = host.panes(&lead).unwrap();
    assert_eq!(first.len(), 1, "{first:?}");
    let kept = first[0].id.clone();
    assert!(pane_id_valid(&kept), "{kept}");
    split(&server, &kept, "exec /bin/sleep 60");
    let panes = host.panes(&lead).unwrap();
    assert_eq!(panes.len(), 2, "{panes:?}");
    assert!(
        panes.iter().all(|pane| !pane.dead && pane.pid > 0),
        "{panes:?}"
    );
    let teammate = panes
        .iter()
        .find(|pane| pane.id != kept)
        .unwrap()
        .id
        .clone();
    let foreign = host.panes(&other).unwrap()[0].id.clone();
    for (pane, why) in [
        ("pane-1", "an invalid id"),
        (foreign.as_str(), "another agent's pane"),
        ("%999999", "an unknown pane"),
    ] {
        assert_eq!(
            host.kill_pane(&lead, pane).unwrap_err().0,
            "terminal_pane_unknown",
            "{why}"
        );
    }
    assert_eq!(host.panes(&other).unwrap().len(), 1);
    assert_eq!(host.panes(&lead).unwrap().len(), 2);

    host.kill_pane(&lead, &teammate).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    let left = loop {
        let left = host.panes(&lead).unwrap();
        if left.len() == 1 {
            break left;
        }
        assert!(
            Instant::now() < deadline,
            "the teammate pane ended: {left:?}"
        );
        std::thread::sleep(Duration::from_millis(50));
    };
    assert_eq!(left[0].id, kept, "the lead's own pane stays");
    assert_eq!(
        host.kill_pane(&lead, &kept).unwrap_err().0,
        "terminal_pane_unknown",
        "the window's last pane is not a teammate"
    );
    assert_eq!(host.panes(&lead).unwrap().len(), 1);
    assert_eq!(host.panes(&other).unwrap().len(), 1);
}

/// A process group a test read from a pane, killed when the test ends unless the test saw
/// it gone, so a failing check leaves no stand-in process running and a group number that
/// answered ESRCH is never signalled (r1 SG-L1).
#[cfg(target_os = "macos")]
struct Group(rustix::process::Pid, std::cell::Cell<bool>);

#[cfg(target_os = "macos")]
impl Drop for Group {
    fn drop(&mut self) {
        if !self.1.get() {
            let _ = rustix::process::kill_process_group(self.0, rustix::process::Signal::KILL);
        }
    }
}

/// The shell line of a pane whose first process keeps in its group a child that ignores the
/// hangup tmux sends a closing pane, as a busy Codex app-server outlives its bridge pane.
/// The child writes its pid to `started` once it ignores the hangup. Takes that file, whose
/// path holds no shell syntax. Returns the line.
#[cfg(target_os = "macos")]
fn hangup_ignored(started: &Path) -> String {
    let path = started.to_str().unwrap();
    assert!(
        path.bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"/._-".contains(&byte)),
        "{path}"
    );
    format!(
        "/bin/sh -c 'trap \"\" HUP; echo $$ > {path}; exec /bin/sleep 120' & exec /bin/sleep 120"
    )
}

/// An agent command that runs the `hangup_ignored` line. Takes its working folder and the
/// child's pid file. Returns the command.
#[cfg(target_os = "macos")]
fn hangup_ignored_agent(cwd: &Path, started: &Path) -> InteractiveCommand {
    InteractiveCommand {
        argv: vec!["/bin/sh".into(), "-c".into(), hangup_ignored(started)],
        cwd: cwd.to_path_buf(),
        env: Vec::new(),
        transcript: crate::harnesses::interactive::Transcript::Unavailable,
    }
}

/// Waits for the child of a `hangup_ignored` pane and checks that it is in the group the
/// pane's first process leads, once `pgid_support::pane_group` sees that process lead it.
/// Takes the pane's first process id and the child's pid file. Returns the group, killed
/// when dropped, and the child.
#[cfg(target_os = "macos")]
fn left_in_group(pane: u32, started: &Path) -> (Group, rustix::process::Pid) {
    let leader = pgid_support::pane_group(pane);
    let group = Group(leader, std::cell::Cell::new(false));
    let deadline = Instant::now() + Duration::from_secs(10);
    let child = loop {
        let written = std::fs::read_to_string(started).ok();
        if let Some(child) = written
            .and_then(|text| text.trim().parse::<i32>().ok())
            .and_then(rustix::process::Pid::from_raw)
        {
            break child;
        }
        assert!(Instant::now() < deadline, "the pane started its child");
        std::thread::sleep(Duration::from_millis(20));
    };
    assert_eq!(
        rustix::process::getpgid(Some(child)),
        Ok(leader),
        "the child stays in the pane's group"
    );
    (group, child)
}

/// Waits for at most ten seconds until neither the group nor its child is left; macOS
/// answers EPERM for a group whose last member it just reaped before it answers ESRCH.
/// A gone group is marked so its guard leaves it alone. Takes both. Returns whether both
/// are gone.
#[cfg(target_os = "macos")]
fn gone(group: &Group, child: rustix::process::Pid) -> bool {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if rustix::process::test_kill_process_group(group.0) == Err(rustix::io::Errno::SRCH)
            && rustix::process::test_kill_process(child) == Err(rustix::io::Errno::SRCH)
        {
            group.1.set(true);
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// A stopped agent leaves nothing of its pane running (FINDING bridge orphan): a child the
/// agent's app started in the pane's group, which ignores the hangup of the closing pane as
/// a busy Codex app-server does, is gone with the whole group once `kill` returns.
#[cfg(target_os = "macos")]
#[test]
fn kill_ends_every_process_in_the_agent_pane_group() {
    use std::os::unix::fs::PermissionsExt;
    let home = tempfile::tempdir().unwrap();
    let folder = home.path().join("terminal");
    std::fs::create_dir(&folder).unwrap();
    std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o700)).unwrap();
    let tmux = installed(home.path()).expect("tmux is required for this test");
    let host = Tmux::open(Some(tmux.clone()), &folder);
    let _server = Server {
        tmux,
        socket: host.socket(),
    };
    let started = home.path().join("child.pid");
    let agent = AgentId::team(1).unwrap();
    host.spawn(&agent, &hangup_ignored_agent(home.path(), &started))
        .unwrap();
    let pane = host.panes(&agent).unwrap().remove(0);
    let (group, child) = left_in_group(pane.pid, &started);

    host.kill(&agent).unwrap();
    assert!(gone(&group, child), "the agent's pane group is gone");
    assert_eq!(host.alive().unwrap(), Vec::<AgentId>::new());
}

/// An ended teammate pane leaves nothing of its group running: `kill_pane` ends the
/// hangup-ignoring child of that pane and keeps the lead's own pane.
#[cfg(target_os = "macos")]
#[test]
fn kill_pane_ends_every_process_in_that_pane_group() {
    use std::os::unix::fs::PermissionsExt;
    let home = tempfile::tempdir().unwrap();
    let folder = home.path().join("terminal");
    std::fs::create_dir(&folder).unwrap();
    std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o700)).unwrap();
    let tmux = installed(home.path()).expect("tmux is required for this test");
    let host = Tmux::open(Some(tmux.clone()), &folder);
    let server = Server {
        tmux,
        socket: host.socket(),
    };
    let lead = AgentId::team(1).unwrap();
    let sleeper = InteractiveCommand {
        argv: vec!["/bin/sh".into(), "-c".into(), "exec /bin/sleep 60".into()],
        cwd: home.path().to_path_buf(),
        env: Vec::new(),
        transcript: crate::harnesses::interactive::Transcript::Unavailable,
    };
    host.spawn(&lead, &sleeper).unwrap();
    let kept = host.panes(&lead).unwrap().remove(0).id;
    let started = home.path().join("child.pid");
    split(&server, &kept, &hangup_ignored(&started));
    let teammate = host
        .panes(&lead)
        .unwrap()
        .into_iter()
        .find(|pane| pane.id != kept)
        .unwrap();
    let (group, child) = left_in_group(teammate.pid, &started);

    host.kill_pane(&lead, &teammate.id).unwrap();
    assert!(gone(&group, child), "the teammate pane's group is gone");
    let left = host.panes(&lead).unwrap();
    assert_eq!(left.len(), 1, "{left:?}");
    assert_eq!(left[0].id, kept, "the lead's own pane stays");
}

/// Closing the server leaves nothing of any of its panes running: a window that holds no
/// agent, which `close` ends only through the server's end, has its hangup-ignoring child
/// gone with its group once `close` returns.
#[cfg(target_os = "macos")]
#[test]
fn close_ends_every_process_in_the_server_pane_groups() {
    use std::os::unix::fs::PermissionsExt;
    let home = tempfile::tempdir().unwrap();
    let folder = home.path().join("terminal");
    std::fs::create_dir(&folder).unwrap();
    std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o700)).unwrap();
    let tmux = installed(home.path()).expect("tmux is required for this test");
    let host = Tmux::open(Some(tmux.clone()), &folder);
    let server = Server {
        tmux,
        socket: host.socket(),
    };
    let agent = AgentId::team(1).unwrap();
    let sleeper = InteractiveCommand {
        argv: vec!["/bin/sh".into(), "-c".into(), "exec /bin/sleep 60".into()],
        cwd: home.path().to_path_buf(),
        env: Vec::new(),
        transcript: crate::harnesses::interactive::Transcript::Unavailable,
    };
    host.spawn(&agent, &sleeper).unwrap();
    let started = home.path().join("child.pid");
    let opened = crate::spawn_gate::output(
        Command::new(&server.tmux)
            .arg("-S")
            .arg(&server.socket)
            .args(["new-window", "-d", "-P", "-F", "#{pane_pid}", "-t"])
            .arg(format!("{SESSION}:"))
            .args(["-n", "_outside", &hangup_ignored(&started)])
            .env_clear()
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null()),
    )
    .unwrap();
    assert!(opened.status.success(), "tmux opened the outside window");
    let pane = String::from_utf8_lossy(&opened.stdout)
        .trim()
        .parse::<u32>()
        .unwrap();
    let (group, child) = left_in_group(pane, &started);
    assert_eq!(
        host.alive().unwrap(),
        vec![agent],
        "the window holds no agent"
    );

    host.close(&[]).unwrap();
    assert!(gone(&group, child), "the outside pane's group is gone");
}

/// A group that once answered ESRCH is never tested or signalled again (r1 SG-L1): its
/// number is free, and a new group may take it while another group of the same stop still
/// ends. One real group ended and was reaped before the call, one real group lives until
/// its KILL.
#[cfg(target_os = "macos")]
#[test]
fn a_group_once_gone_is_never_tested_or_signalled_again() {
    use std::os::unix::process::{CommandExt, ExitStatusExt};
    let mut ended = crate::spawn_gate::spawn(
        Command::new("/usr/bin/true")
            .process_group(0)
            .env_clear()
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null()),
    )
    .unwrap();
    let ended_group = rustix::process::Pid::from_child(&ended);
    assert!(ended.wait().unwrap().success());
    let mut survivor = crate::spawn_gate::spawn(
        Command::new("/bin/sleep")
            .arg("120")
            .process_group(0)
            .env_clear()
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null()),
    )
    .unwrap();
    let survivor_group = rustix::process::Pid::from_child(&survivor);
    let guard = Group(survivor_group, std::cell::Cell::new(false));
    let held = [ended.id(), survivor.id()]
        .map(|group| Ending::claim(group).expect("no path holds a new group"));
    let reaped = std::thread::spawn(move || survivor.wait());
    let calls = std::cell::RefCell::new(Vec::new());

    end_groups_with(
        held.into(),
        |group| {
            let answer = rustix::process::test_kill_process_group(group);
            calls.borrow_mut().push(("test", group, answer));
            answer != Err(rustix::io::Errno::SRCH)
        },
        |group| {
            calls.borrow_mut().push(("kill", group, Ok(())));
            let _ = rustix::process::kill_process_group(group, rustix::process::Signal::KILL);
        },
    );
    let calls = calls.into_inner();
    let of_ended: Vec<_> = calls
        .iter()
        .filter(|(_, group, _)| *group == ended_group)
        .collect();
    assert_eq!(
        of_ended,
        [&("test", ended_group, Err(rustix::io::Errno::SRCH))],
        "the ended group is tested once and left alone"
    );
    assert!(
        calls
            .iter()
            .any(|(kind, group, _)| *kind == "kill" && *group == survivor_group),
        "the live group gets its KILL"
    );
    let status = reaped.join().unwrap().unwrap();
    assert_eq!(status.signal(), Some(9), "the live group ended by KILL");
    guard.1.set(true);
}

/// Stops in a row do not each wait out the hangup grace (r1 SG-L2): the team tick ends the
/// panes of three agents whose children ignore the hangup in less than one grace, and every
/// group is still ended.
#[cfg(target_os = "macos")]
#[test]
fn stops_in_a_row_wait_less_than_one_hangup_grace() {
    use std::os::unix::fs::PermissionsExt;
    let home = tempfile::tempdir().unwrap();
    let folder = home.path().join("terminal");
    std::fs::create_dir(&folder).unwrap();
    std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o700)).unwrap();
    let tmux = installed(home.path()).expect("tmux is required for this test");
    let host = Tmux::open(Some(tmux.clone()), &folder);
    let _server = Server {
        tmux,
        socket: host.socket(),
    };
    let mut started = Vec::new();
    for id in 1..=3 {
        let agent = AgentId::team(id).unwrap();
        let pid_file = home.path().join(format!("child-{id}.pid"));
        host.spawn(&agent, &hangup_ignored_agent(home.path(), &pid_file))
            .unwrap();
        let pane = host.panes(&agent).unwrap().remove(0);
        started.push((agent, left_in_group(pane.pid, &pid_file)));
    }

    let begun = Instant::now();
    for (agent, _) in &started {
        host.kill(agent).unwrap();
    }
    let took = begun.elapsed();
    assert!(
        took < HANGUP_GRACE,
        "three stops took {took:?}, at least one hangup grace {HANGUP_GRACE:?}"
    );
    for (agent, (group, child)) in &started {
        assert!(gone(group, *child), "the group of {agent:?} is gone");
    }
}

/// Closing the server waits about one hangup grace for all its panes, not one per pane
/// (r1 SG-L2), and returns only once every group is gone, as the engine's shutdown needs.
#[cfg(target_os = "macos")]
#[test]
fn close_waits_one_hangup_grace_for_all_its_panes() {
    use std::os::unix::fs::PermissionsExt;
    let home = tempfile::tempdir().unwrap();
    let folder = home.path().join("terminal");
    std::fs::create_dir(&folder).unwrap();
    std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o700)).unwrap();
    let tmux = installed(home.path()).expect("tmux is required for this test");
    let host = Tmux::open(Some(tmux.clone()), &folder);
    let _server = Server {
        tmux,
        socket: host.socket(),
    };
    let mut started = Vec::new();
    for id in 1..=3 {
        let agent = AgentId::team(id).unwrap();
        let pid_file = home.path().join(format!("child-{id}.pid"));
        host.spawn(&agent, &hangup_ignored_agent(home.path(), &pid_file))
            .unwrap();
        let pane = host.panes(&agent).unwrap().remove(0);
        started.push(left_in_group(pane.pid, &pid_file));
    }

    let begun = Instant::now();
    host.close(&[]).unwrap();
    let took = begun.elapsed();
    assert!(
        took < HANGUP_GRACE + GROUP_KILL_WAIT,
        "closing three panes took {took:?}, more than one grace and kill wait"
    );
    for (group, child) in &started {
        assert_eq!(
            rustix::process::test_kill_process_group(group.0),
            Err(rustix::io::Errno::SRCH),
            "every group is gone once close returns"
        );
        assert_eq!(
            rustix::process::test_kill_process(*child),
            Err(rustix::io::Errno::SRCH)
        );
        group.1.set(true);
    }
}

/// CB-2 one signal: a close leaves a pane group another path holds to that path, as the
/// stop that ends the pane holds its group from before the host's kill, so the host never
/// signals that group as well. The holder then ends it.
#[cfg(target_os = "macos")]
#[test]
fn a_close_leaves_a_pane_group_another_path_holds_to_that_path() {
    use std::os::unix::fs::PermissionsExt;
    let home = tempfile::tempdir().unwrap();
    let folder = home.path().join("terminal");
    std::fs::create_dir(&folder).unwrap();
    std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o700)).unwrap();
    let tmux = installed(home.path()).expect("tmux is required for this test");
    let host = Tmux::open(Some(tmux.clone()), &folder);
    let _server = Server {
        tmux,
        socket: host.socket(),
    };
    let started = home.path().join("child.pid");
    let agent = AgentId::team(1).unwrap();
    host.spawn(&agent, &hangup_ignored_agent(home.path(), &started))
        .unwrap();
    let pane = host.panes(&agent).unwrap().remove(0);
    let (group, child) = left_in_group(pane.pid, &started);
    let held = crate::harnesses::launch::Ending::claim(pane.pid)
        .expect("no path holds the new pane's group");

    host.kill(&agent).unwrap();
    host.join_reapers();
    let survived = rustix::process::test_kill_process(child).is_ok();
    let ended = crate::harnesses::launch::end_pane_group(pane.pid, Some(held));
    assert!(
        survived,
        "the host signalled a pane group another path holds"
    );
    assert!(ended, "the holder ends the group");
    assert!(gone(&group, child), "the pane's group is gone");
}

#[cfg(target_os = "macos")]
#[test]
fn a_pane_view_shows_and_types_into_only_the_teammate_pane_its_lead_window_lists() {
    use std::os::unix::fs::PermissionsExt;
    let home = tempfile::tempdir().unwrap();
    let folder = home.path().join("terminal");
    std::fs::create_dir(&folder).unwrap();
    std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o700)).unwrap();
    let tmux = installed(home.path()).expect("tmux is required for this test");
    let host = Tmux::open(Some(tmux.clone()), &folder);
    let socket = host.socket();
    let _server = Server {
        tmux: tmux.clone(),
        socket: socket.clone(),
    };
    let shell = |script: &str| InteractiveCommand {
        argv: vec!["/bin/sh".into(), "-c".into(), script.into()],
        cwd: home.path().to_path_buf(),
        env: Vec::new(),
        transcript: crate::harnesses::interactive::Transcript::Unavailable,
    };
    let lead = AgentId::team(1).unwrap();
    let other = AgentId::team(2).unwrap();
    host.spawn(&lead, &shell("echo lead-own-screen; exec /bin/cat"))
        .unwrap();
    host.spawn(&other, &shell("echo other-lead-screen; exec /bin/sleep 30"))
        .unwrap();
    let client = |args: &[&str]| {
        let output = crate::spawn_gate::output(
            Command::new(&tmux)
                .arg("-S")
                .arg(&socket)
                .args(args)
                .env_clear()
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped()),
        )
        .unwrap();
        assert!(
            output.status.success(),
            "tmux {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    };
    let teammate = client(&[
        "split-window",
        "-d",
        "-P",
        "-F",
        "#{pane_id}",
        "-t",
        &target(&lead),
        "--",
        "/bin/sh",
        "-c",
        "echo teammate-pane-ready; exec /bin/cat",
    ]);
    assert!(
        pane_id_valid(&teammate),
        "tmux names the split pane: {teammate}"
    );
    let elsewhere = client(&["list-panes", "-t", &target(&other), "-F", "#{pane_id}"]);
    assert!(
        pane_id_valid(&elsewhere) && elsewhere != teammate,
        "{elsewhere}"
    );
    let host: Arc<dyn Host> = Arc::new(host);
    let shows = |terminal: &dyn Host, wanted: &str| -> String {
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            let screen =
                String::from_utf8_lossy(&terminal.attach(&lead).unwrap().screen).into_owned();
            if screen.contains(wanted) {
                return screen;
            }
            assert!(
                Instant::now() < deadline,
                "the pane showed {wanted} in time: {screen}"
            );
            std::thread::sleep(Duration::from_millis(100));
        }
    };
    let view = crate::terminal::pane_view(host.clone(), &teammate).unwrap();
    let screen = shows(&*view, "teammate-pane-ready");
    assert!(
        !screen.contains("lead-own-screen"),
        "the view shows the teammate's pane, not its lead's: {screen}"
    );
    view.input(&lead, b"typed-to-teammate\r").unwrap();
    shows(&*view, "typed-to-teammate");
    let own = shows(&*host, "lead-own-screen");
    assert!(
        !own.contains("typed-to-teammate") && !own.contains("teammate-pane-ready"),
        "typing went to the teammate's pane only, and the lead's view stays its own: {own}"
    );
    let unknown = Err("terminal_pane_unknown".to_owned());
    assert_eq!(
        crate::terminal::pane_view(host.clone(), "lead")
            .map(drop)
            .map_err(|problem| problem.0),
        unknown,
        "an invalid pane id is refused before any call"
    );
    for pane in [elsewhere.as_str(), "%99999"] {
        let view = crate::terminal::pane_view(host.clone(), pane).unwrap();
        assert_eq!(
            view.attach(&lead).map(drop).map_err(|problem| problem.0),
            unknown,
            "a pane its lead's window does not list is never attached: {pane}"
        );
        assert_eq!(
            view.snapshot(&lead).map(drop).map_err(|problem| problem.0),
            unknown,
            "a pane its lead's window does not list is never read: {pane}"
        );
        assert_eq!(
            view.input(&lead, b"typed-to-other\r")
                .map_err(|problem| problem.0),
            unknown,
            "a pane its lead's window does not list is never typed into: {pane}"
        );
        assert_eq!(
            view.input_live(&lead)
                .map(drop)
                .map_err(|problem| problem.0),
            unknown,
            "a pane its lead's window does not list is never checked for live input: {pane}"
        );
    }
    let deadline = Instant::now() + Duration::from_secs(1);
    loop {
        let screen = String::from_utf8_lossy(&host.attach(&other).unwrap().screen).into_owned();
        assert!(
            !screen.contains("typed-to-other"),
            "no refused typing reached the other lead's pane: {screen}"
        );
        if Instant::now() >= deadline && screen.contains("other-lead-screen") {
            break;
        }
        assert!(
            Instant::now() < deadline + Duration::from_secs(14),
            "the other lead's pane showed its own screen in time: {screen}"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Says whether a host still holds a control connection that reads. Input: the host.
/// Output: true while its held control has not seen its server end.
#[cfg(target_os = "macos")]
fn holds_reading_control(host: &Tmux) -> bool {
    host.control
        .lock()
        .unwrap()
        .as_ref()
        .is_some_and(|control| !control.closed())
}

/// Gives the inode of the file at a host's socket path, which changes when another server
/// binds the path. Input: the host. Output: the inode, or none when no file is there.
#[cfg(target_os = "macos")]
fn socket_inode(host: &Tmux) -> Option<u64> {
    use std::os::unix::fs::MetadataExt;
    std::fs::symlink_metadata(host.socket())
        .ok()
        .map(|metadata| metadata.ino())
}

/// Starts a private host with one agent in a fresh 0700 folder, then ends its server from
/// outside with kill-server, as another tmux client or a crash would. Takes the temp home
/// and the agent. Returns the host, still holding the control it read with, its server and
/// the inode of the socket that server bound.
#[cfg(target_os = "macos")]
fn ended_from_outside(home: &Path, agent: &AgentId) -> (Tmux, Server, u64) {
    use std::os::unix::fs::PermissionsExt;
    let folder = home.join("terminal");
    std::fs::create_dir(&folder).unwrap();
    std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o700)).unwrap();
    let tmux = installed(home).expect("tmux is required for this test");
    let host = Tmux::open(Some(tmux.clone()), &folder);
    let server = Server {
        tmux: tmux.clone(),
        socket: host.socket(),
    };
    let command = InteractiveCommand {
        argv: vec!["/bin/sleep".into(), "60".into()],
        cwd: home.to_path_buf(),
        env: Vec::new(),
        transcript: crate::harnesses::interactive::Transcript::Unavailable,
    };
    host.spawn(agent, &command).unwrap();
    assert_eq!(host.alive().unwrap(), vec![agent.clone()]);
    assert!(
        holds_reading_control(&host),
        "the host holds a reading control"
    );
    let bound = socket_inode(&host).expect("the server bound its socket");
    let ended = crate::spawn_gate::status(
        Command::new(&tmux)
            .arg("-S")
            .arg(host.socket())
            .arg("kill-server")
            .env_clear()
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null()),
    )
    .unwrap();
    assert!(ended.success(), "tmux ended the server");
    (host, server, bound)
}

/// Starts a host whose holder reads its deadline every loop and ends its server after two
/// passed reads, on a fresh server in `home` running one agent (FBTMUX-P1). Takes the home
/// and the agent. Returns the host and its server.
#[cfg(target_os = "macos")]
fn fast_deadline(home: &Path, agent: &AgentId) -> (Tmux, Server) {
    use std::os::unix::fs::PermissionsExt;
    let folder = home.join("terminal");
    std::fs::create_dir(&folder).unwrap();
    std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o700)).unwrap();
    let tmux = installed(home).expect("tmux is required for this test");
    let host = Tmux::open(Some(tmux.clone()), &folder).with_deadline_checks(1, 2);
    let server = Server {
        tmux,
        socket: host.socket(),
    };
    let command = InteractiveCommand {
        argv: vec!["/bin/sleep".into(), "120".into()],
        cwd: home.to_path_buf(),
        env: Vec::new(),
        transcript: crate::harnesses::interactive::Transcript::Unavailable,
    };
    host.spawn(agent, &command).unwrap();
    assert_eq!(host.alive().unwrap(), vec![agent.clone()]);
    (host, server)
}

/// FBTMUX-P1: a server's holder ends it with its agents once the deadline the engine set
/// has passed, while a server renewed ahead in time keeps running and one no engine ever
/// renewed keeps today's bound, its socket alone.
#[cfg(target_os = "macos")]
#[test]
fn the_holder_ends_a_server_only_once_its_renewed_deadline_passed() {
    let agent = AgentId::team(1).unwrap();
    let (never, kept, ending) = (
        tempfile::tempdir().unwrap(),
        tempfile::tempdir().unwrap(),
        tempfile::tempdir().unwrap(),
    );
    let (unrenewed, _unrenewed) = fast_deadline(never.path(), &agent);
    let (renewed, _renewed) = fast_deadline(kept.path(), &agent);
    let (late, _late) = fast_deadline(ending.path(), &agent);
    let ahead = || SystemTime::now() + Duration::from_secs(3);
    late.renew(ahead()).unwrap();
    let listens = |host: &Tmux| crate::spawn_gate::connect_unix_std(host.socket()).is_ok();
    let until = Instant::now() + Duration::from_secs(30);
    while listens(&late) {
        assert!(
            Instant::now() < until,
            "the holder ends a server past its deadline"
        );
        renewed.renew(ahead()).unwrap();
        std::thread::sleep(Duration::from_millis(500));
    }
    assert_eq!(late.alive().unwrap(), Vec::<AgentId>::new());
    let held = Instant::now() + Duration::from_secs(8);
    while Instant::now() < held {
        renewed.renew(ahead()).unwrap();
        std::thread::sleep(Duration::from_millis(500));
    }
    assert_eq!(renewed.alive().unwrap(), vec![agent.clone()]);
    assert_eq!(unrenewed.alive().unwrap(), vec![agent]);
}

/// A server ended from outside leaves its socket file behind. Once its control saw it end
/// and nothing listens, the host has no live agents, forgets the socket and starts no server.
#[cfg(target_os = "macos")]
#[test]
fn a_server_ended_from_outside_has_no_live_agents_and_is_not_started_again() {
    let home = tempfile::tempdir().unwrap();
    let agent = AgentId::team(1).unwrap();
    let (host, _server, _) = ended_from_outside(home.path(), &agent);
    let until = Instant::now() + REPLY_TIME;
    while holds_reading_control(&host) || crate::spawn_gate::connect_unix_std(host.socket()).is_ok()
    {
        assert!(
            Instant::now() < until,
            "the server ended and its control saw it"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(host.running(), "tmux leaves its socket file behind");
    assert_eq!(host.alive().unwrap(), Vec::<AgentId>::new());
    assert!(
        !host.running(),
        "the ended server's socket file is forgotten"
    );
    assert!(
        crate::spawn_gate::connect_unix_std(host.socket()).is_err(),
        "no server was started again"
    );
}

/// Asks for the live agents at once after an outside kill-server, while the held control may
/// still read, the server may still be ending, or both are gone. Every round has no live
/// agents, never a closed or unavailable terminal, and no other server binds the socket
/// path: it is gone or still the file the ended server bound.
#[cfg(target_os = "macos")]
#[test]
fn a_server_ended_from_outside_has_no_live_agents_while_its_control_still_reads() {
    const ROUNDS: usize = 20;
    let mut answers = Vec::new();
    for _ in 0..ROUNDS {
        let home = tempfile::tempdir().unwrap();
        let agent = AgentId::team(1).unwrap();
        let (host, _server, bound) = ended_from_outside(home.path(), &agent);
        let answer = host.alive().map_err(|error| error.0);
        let rebound = socket_inode(&host).is_some_and(|inode| inode != bound);
        answers.push((answer, rebound));
    }
    assert_eq!(answers, vec![(Ok(Vec::new()), false); ROUNDS]);
}

#[cfg(target_os = "macos")]
const PROBE_FOLDER: &str = "CRAZYAGENT_TMUX_PROBE_FOLDER";
#[cfg(target_os = "macos")]
const PROBE_HELPER: &str = "terminal::tmux::tests::probing_a_live_server_out_of_descriptors";

/// A probe of a live server that fails without a refusal keeps the server's socket. A helper
/// process that can open no more descriptors asks a fresh host for the live agents: it is
/// told the terminal is unavailable, and the server keeps its socket and its agent.
#[cfg(target_os = "macos")]
#[test]
fn a_live_server_keeps_its_socket_when_a_probe_of_it_fails_without_a_refusal() {
    use std::os::unix::fs::PermissionsExt;
    let home = tempfile::tempdir().unwrap();
    let folder = home.path().join("terminal");
    std::fs::create_dir(&folder).unwrap();
    std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o700)).unwrap();
    let tmux = installed(home.path()).expect("tmux is required for this test");
    let host = Tmux::open(Some(tmux.clone()), &folder);
    let _server = Server {
        tmux: tmux.clone(),
        socket: host.socket(),
    };
    let agent = AgentId::team(1).unwrap();
    let command = InteractiveCommand {
        argv: vec!["/bin/sleep".into(), "60".into()],
        cwd: home.path().to_path_buf(),
        env: Vec::new(),
        transcript: crate::harnesses::interactive::Transcript::Unavailable,
    };
    host.spawn(&agent, &command).unwrap();
    let bound = socket_inode(&host).expect("the server bound its socket");
    let ran = crate::spawn_gate::status(
        Command::new(std::env::current_exe().unwrap())
            .args(["--exact", PROBE_HELPER, "--ignored", "--test-threads", "1"])
            .env(PROBE_FOLDER, &folder)
            .stdin(Stdio::null())
            .stdout(Stdio::null()),
    )
    .unwrap();
    assert!(ran.success(), "the probing helper ran");
    assert_eq!(
        std::fs::read_to_string(home.path().join("answer")).unwrap(),
        r#"Err("terminal_unavailable")"#
    );
    assert_eq!(
        socket_inode(&host),
        Some(bound),
        "the live server keeps its socket"
    );
    assert_eq!(
        Tmux::open(Some(tmux), &folder).alive().unwrap(),
        vec![agent]
    );
}

/// Opens descriptors until the process can open no more, then asks a fresh host for the
/// live agents of the server in the folder `PROBE_FOLDER` names, and writes the answer
/// beside that folder. Run only by the probe descriptor test as its helper process.
#[cfg(target_os = "macos")]
#[test]
#[ignore = "run only by the probe descriptor test as its helper process"]
fn probing_a_live_server_out_of_descriptors() {
    use rustix::process::{Resource, Rlimit, getrlimit, setrlimit};
    let Some(folder) = std::env::var_os(PROBE_FOLDER).map(PathBuf::from) else {
        return;
    };
    let home = folder.parent().unwrap().to_path_buf();
    let host = Tmux::open(installed(&home), &folder);
    let limit = getrlimit(Resource::Nofile);
    setrlimit(
        Resource::Nofile,
        Rlimit {
            current: Some(128),
            maximum: limit.maximum,
        },
    )
    .unwrap();
    let mut held = Vec::new();
    let full = loop {
        match std::fs::File::open("/dev/null") {
            Ok(file) => held.push(file),
            Err(error) => break error,
        }
        assert!(held.len() <= 128, "the lowered limit bounds the open files");
    };
    assert_eq!(
        rustix::io::Errno::from_io_error(&full),
        Some(rustix::io::Errno::MFILE)
    );
    let answer = host.alive().map_err(|error| error.0);
    drop(held);
    std::fs::write(home.join("answer"), format!("{answer:?}")).unwrap();
}

/// A tmux program answering as the stand-in server a lead swapped in for its own would: its
/// session lists `agent` and every pane it reports has `pid` as its first process. Takes the
/// folder it goes in, the agent and the pid. Returns the program.
#[cfg(target_os = "macos")]
fn forging_tmux(folder: &Path, agent: &AgentId, pid: impl std::fmt::Display) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let program = folder.join("forging-tmux");
    let script = format!(
        r#"#!/bin/sh
printf '%%begin 1 0 1\n%%end 1 0 1\n'
number=1
while IFS= read -r line; do
  printf '%%begin 1 %s 1\n' "$number"
  case "$line" in
    *'#{{version}}'*) printf '3.5 end\n' ;;
    *list-windows*) printf '{agent} end\n' ;;
    *list-panes*) printf '%%1 {pid} 0 end\n' ;;
    *new-window*) printf '%%2 {pid} end\n' ;;
  esac
  printf '%%end 1 %s 1\n' "$number"
  number=$((number + 1))
done
"#
    );
    std::fs::write(&program, script).unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
    program
}

/// A host whose socket has a listener in this test process and whose tmux program forges
/// every reply: the swap a native lead can make at its own socket path.
#[cfg(target_os = "macos")]
struct StandIn {
    home: tempfile::TempDir,
    _listener: std::os::unix::net::UnixListener,
    address: PathBuf,
    agent: AgentId,
    host: Tmux,
}

#[cfg(target_os = "macos")]
impl StandIn {
    /// Opens the host and binds the stand-in listener. Takes the pid its replies name.
    /// Returns the stand-in, which this process listens for.
    fn naming(pid: impl std::fmt::Display) -> Self {
        use std::os::unix::fs::PermissionsExt;
        let home = tempfile::tempdir().unwrap();
        let folder = home.path().join("terminal");
        std::fs::create_dir(&folder).unwrap();
        std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o700)).unwrap();
        let agent = AgentId::team(1).unwrap();
        let host = Tmux::open(Some(forging_tmux(home.path(), &agent, pid)), &folder);
        let address = host.address().unwrap();
        let listener = crate::spawn_gate::bind_unix_std(&address).unwrap();
        Self {
            home,
            _listener: listener,
            address,
            agent,
            host,
        }
    }
}

/// Starts a process leading its own group under a shell that stays, so neither this test nor
/// a stand-in listening in it is its parent. Returns the shell and the group, killed when
/// dropped.
#[cfg(target_os = "macos")]
fn foreign_group() -> (Child, Group) {
    let mut shell = crate::spawn_gate::spawn(
        Command::new("/bin/sh")
            .args([
                "-c",
                "/usr/bin/perl -e 'setpgrp(0, 0); sleep 120' & echo $!; wait",
            ])
            .env_clear()
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null()),
    )
    .unwrap();
    let mut line = String::new();
    BufReader::new(shell.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    let leader = pgid_support::pane_group(line.trim().parse().unwrap());
    (shell, Group(leader, std::cell::Cell::new(false)))
}

/// Starts a process of this test leading its own group, so a stand-in listening in this test
/// process is its parent. Returns the process and the group, killed when dropped.
#[cfg(target_os = "macos")]
fn listeners_group() -> (Child, Group) {
    use std::os::unix::process::CommandExt;
    let child = crate::spawn_gate::spawn(
        Command::new("/bin/sleep")
            .arg("120")
            .process_group(0)
            .env_clear()
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null()),
    )
    .unwrap();
    let leader = pgid_support::pane_group(child.id());
    (child, Group(leader, std::cell::Cell::new(false)))
}

#[cfg(target_os = "macos")]
fn answers(group: &Group) -> bool {
    rustix::process::test_kill_process_group(group.0).is_ok()
}

#[cfg(target_os = "macos")]
#[test]
fn a_kill_leaves_alone_a_process_a_stand_in_listener_names_but_did_not_start() {
    let (mut shell, group) = foreign_group();
    let stand_in = StandIn::naming(group.0.as_raw_nonzero());

    stand_in.host.kill(&stand_in.agent).unwrap();
    stand_in.host.join_reapers();

    assert!(
        answers(&group),
        "the engine killed a group the stand-in only named"
    );
    drop(group);
    let _ = shell.wait();
}

#[cfg(target_os = "macos")]
#[test]
fn a_left_pane_cleanup_leaves_alone_a_process_a_stand_in_listener_names_but_did_not_start() {
    let (mut shell, group) = foreign_group();
    let stand_in = StandIn::naming(group.0.as_raw_nonzero());

    let ended =
        crate::harnesses::terminal_session::end_left_pane(&stand_in.host, &stand_in.agent, "test")
            .map_err(|problem| problem.0);

    assert!(
        answers(&group),
        "the cleanup signalled a process the stand-in only named"
    );
    assert_eq!(
        ended,
        Ok(false),
        "a pane no server vouched for stays uncertain"
    );
    drop(group);
    let _ = shell.wait();
}

#[cfg(target_os = "macos")]
#[test]
fn a_spawn_refuses_a_first_process_the_listener_did_not_start() {
    let (mut shell, group) = foreign_group();
    let stand_in = StandIn::naming(group.0.as_raw_nonzero());
    let command = hangup_ignored_agent(stand_in.home.path(), &stand_in.home.path().join("unused"));

    let spawned = stand_in
        .host
        .spawn(&AgentId::team(2).unwrap(), &command)
        .map(|pane| pane.pid)
        .map_err(|problem| problem.0);

    assert_eq!(spawned, Err("terminal_command_failed".to_owned()));
    drop(group);
    let _ = shell.wait();
}

#[cfg(target_os = "macos")]
#[test]
fn a_reply_naming_a_pid_no_process_can_have_names_none() {
    let stand_in = StandIn::naming(u32::MAX);

    assert_eq!(
        stand_in
            .host
            .pane_pid(&stand_in.agent)
            .map_err(|problem| problem.0),
        Ok(None)
    );
    assert_eq!(
        crate::harnesses::terminal_session::end_left_pane(&stand_in.host, &stand_in.agent, "test",)
            .map_err(|problem| problem.0),
        Ok(false)
    );
}

#[cfg(target_os = "macos")]
#[test]
fn a_first_process_the_listener_started_is_named() {
    let (mut child, group) = listeners_group();
    let stand_in = StandIn::naming(group.0.as_raw_nonzero());

    assert_eq!(
        stand_in
            .host
            .pane_pid(&stand_in.agent)
            .map_err(|problem| problem.0),
        Ok(Some(child.id()))
    );
    drop(group);
    let _ = child.wait();
}

#[cfg(target_os = "macos")]
#[test]
fn a_first_process_is_not_named_once_the_listener_is_gone() {
    let (mut child, group) = listeners_group();
    let stand_in = StandIn::naming(group.0.as_raw_nonzero());
    assert_eq!(
        stand_in
            .host
            .pane_pid(&stand_in.agent)
            .map_err(|problem| problem.0),
        Ok(Some(child.id()))
    );

    drop(stand_in._listener);

    assert_eq!(
        stand_in
            .host
            .pane_pid(&stand_in.agent)
            .map_err(|problem| problem.0),
        Ok(None),
        "a socket nothing listens at vouched for a process"
    );
    drop(group);
    let _ = child.wait();
}

#[cfg(target_os = "macos")]
#[test]
fn the_server_pid_comes_only_from_a_listener_bound_at_the_socket_itself() {
    let stand_in = StandIn::naming(2);
    assert_eq!(stand_in.host.server_pid(), Some(std::process::id()));
    let other = stand_in.home.path().join("other.sock");
    let _other = crate::spawn_gate::bind_unix_std(&other).unwrap();

    std::fs::remove_file(&stand_in.address).unwrap();
    std::os::unix::fs::symlink(&other, &stand_in.address).unwrap();

    assert_eq!(
        stand_in.host.server_pid(),
        None,
        "a link at the socket path redirected the probe to another listener"
    );
}
