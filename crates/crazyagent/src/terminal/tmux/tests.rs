use super::*;
use crate::terminal::PASTE_CLOSE;
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

/// V243-PASTE-CHUNKS: typed bytes go to tmux as `send-keys -H` commands of at most
/// `SEND_KEYS_BYTES_MAX` bytes for the one target, every byte once and in order, and no
/// command for no bytes, so a paste never holds the server in one long command parse.
#[test]
fn typed_bytes_go_as_send_keys_commands_of_at_most_256_bytes_in_order() {
    assert!(send_keys_lines("%7", &[]).is_empty());
    for length in [1, 255, 256, 257, 512, 1000, INPUT_BYTES_MAX] {
        let bytes: Vec<u8> = (0..length).map(|at| (at * 7 % 256) as u8).collect();
        let runs = typed_runs(&send_keys_lines("%7", &bytes));
        assert_eq!(
            runs.len(),
            length.div_ceil(SEND_KEYS_BYTES_MAX),
            "{length} bytes"
        );
        assert!(
            runs.iter().all(|run| run.len() <= SEND_KEYS_BYTES_MAX),
            "{length} bytes"
        );
        assert!(runs.iter().all(|run| !run.is_empty()), "{length} bytes");
        assert_eq!(
            runs.concat(),
            bytes,
            "{length} bytes arrive once each, in order"
        );
    }
}

/// #359 r1 F1, RV-DS241-69 one layer down: no `send-keys` run ends inside a UTF-8
/// character or inside the closing paste marker that ends the input, which `paste()` and
/// the TUI keep whole for a program that reads them per read.
#[test]
fn a_send_keys_run_never_cuts_a_character_or_the_paste_end_marker() {
    let character = "\u{1f600}".as_bytes();
    let lengths = [
        INPUT_BYTES_MAX,
        2 * SEND_KEYS_BYTES_MAX + 3,
        5 * SEND_KEYS_BYTES_MAX + 1,
    ];
    for length in lengths {
        for at in SEND_KEYS_BYTES_MAX - 3..SEND_KEYS_BYTES_MAX {
            let mut bytes = vec![b'x'; length - PASTE_CLOSE.len()];
            bytes[at..at + character.len()].copy_from_slice(character);
            bytes.extend_from_slice(PASTE_CLOSE);
            let runs = typed_runs(&send_keys_lines("%7", &bytes));
            let case = format!("{length} bytes, a character at {at}");
            assert_eq!(runs.concat(), bytes, "{case}");
            assert!(
                runs.iter().all(|run| run.len() <= SEND_KEYS_BYTES_MAX),
                "{case}"
            );
            assert!(
                runs.iter().all(|run| std::str::from_utf8(run).is_ok()),
                "{case}: a run cuts the character"
            );
            assert!(
                runs.last().is_some_and(|run| run.ends_with(PASTE_CLOSE)),
                "{case}: a run cuts the marker"
            );
        }
    }
}

/// #359 r2: a paste end marker inside the input, as the desktop sends one when keys typed
/// during a send join the paste's batch (`...\x1b[201~\r`), stays whole in one run too.
#[test]
fn a_send_keys_run_never_cuts_a_paste_end_marker_inside_the_input() {
    let mut inputs: Vec<Vec<u8>> = (SEND_KEYS_BYTES_MAX - 5..SEND_KEYS_BYTES_MAX)
        .map(|at| {
            [
                &vec![b'x'; at][..],
                PASTE_CLOSE,
                &b"\r"[..],
                &[b'y'; 300][..],
            ]
            .concat()
        })
        .collect();
    inputs.push(
        [
            &[b'x'; 252][..],
            PASTE_CLOSE,
            &[b'y'; 250][..],
            PASTE_CLOSE,
            &b"\r"[..],
        ]
        .concat(),
    );
    for bytes in inputs {
        let runs = typed_runs(&send_keys_lines("%7", &bytes));
        assert_eq!(runs.concat(), bytes);
        assert!(runs.iter().all(|run| run.len() <= SEND_KEYS_BYTES_MAX));
        let ends: Vec<usize> = runs
            .iter()
            .scan(0, |end, run| {
                *end += run.len();
                Some(*end)
            })
            .collect();
        for marker in (0..bytes.len()).filter(|&at| bytes[at..].starts_with(PASTE_CLOSE)) {
            assert!(
                !ends
                    .iter()
                    .any(|&end| marker < end && end < marker + PASTE_CLOSE.len()),
                "{} bytes: a run cuts the marker at {marker}",
                bytes.len()
            );
        }
    }
}

/// #359 r2, rv-code2 r2 F1: the desktop joins whole events into one input, and no
/// `send-keys` run ends inside one of them, so an app never reads half an escape sequence
/// and takes its lone ESC for the Escape key. V243-PASTE-SS3-X10-UTF8: that holds for an SS3
/// key whose final is a UTF-8 character, and for an X10 report past column or row 95, whose
/// three bytes xterm.js sends raw, one byte per value up to 0xFF and never as UTF-8 (#398
/// r1 F1), after any count of typed bytes.
#[test]
fn a_send_keys_run_never_cuts_an_escape_sequence_of_a_batched_input() {
    let text = |times: usize| vec![&b"x"[..]; times];
    let mut cases: Vec<(&str, Vec<&[u8]>)> = vec![
        ("wheel reports", vec![&b"\x1b[<65;40;12M"[..]; 341]),
        ("held arrow keys", vec![&b"\x1b[A"[..]; 300]),
        ("held application arrow keys", vec![&b"\x1bOA"[..]; 300]),
        ("alt keys", [text(1), vec![&b"\x1bx"[..]; 300]].concat()),
        ("x10 mouse reports", vec![&b"\x1b[M`!!"[..]; 100]),
        (
            "ss3 keys with a utf-8 final",
            [text(1), vec![&b"\x1bO\xc3\xa9"[..]; 100]].concat(),
        ),
        (
            "osc replies",
            vec![&b"\x1b]11;rgb:1e1e/1e1e/1e1e\x07"[..]; 20],
        ),
        ("dcs replies", vec![&b"\x1bP1$r0;1m\x1b\\"[..]; 40]),
        (
            "a paste, then Enter and a key",
            [
                vec![&b"\x1b[200~"[..]],
                text(245),
                vec![PASTE_CLOSE, &b"\r"[..], &b"\x1b[B"[..]],
            ]
            .concat(),
        ),
    ];
    for (case, report) in [
        ("raw x10 wheel past column 95", &b"\x1b[M`\x84\x34"[..]),
        (
            "raw x10 click past column and row 95",
            &b"\x1b[M \x84\x98"[..],
        ),
        ("raw x10 release past column 95", &b"\x1b[M#\xa2\x48"[..]),
    ] {
        for typed in 0..12 {
            cases.push((case, [text(typed), vec![report; 200]].concat()));
        }
    }
    for (case, events) in cases {
        let bytes = events.concat();
        let bounds: Vec<usize> = events
            .iter()
            .scan(0, |end, event| {
                *end += event.len();
                Some(*end)
            })
            .collect();
        let runs = typed_runs(&send_keys_lines("%7", &bytes));
        assert_eq!(runs.concat(), bytes, "{case}");
        assert!(
            runs.iter().all(|run| run.len() <= SEND_KEYS_BYTES_MAX),
            "{case}"
        );
        let mut end = 0;
        for run in &runs {
            end += run.len();
            assert!(
                bounds.contains(&end),
                "{case}: a run ends inside an event at {end}"
            );
        }
    }
}

/// #359 r2: a run moves its end back at most `RUN_CUT_BACK_BYTES_MAX` bytes to keep an escape
/// sequence whole. A window title that starts further back than that is the one sequence a
/// run cuts, at the run size.
#[test]
fn a_send_keys_run_cuts_only_a_sequence_that_starts_past_its_back_off() {
    let title = [&b"\x1b]0;"[..], &[b't'; 200][..], &b"\x07"[..]].concat();
    for (before, first) in [(200, 200), (150, SEND_KEYS_BYTES_MAX)] {
        let bytes = [&vec![b'x'; before][..], &title, &b"\x1b[A"[..]].concat();
        let runs = typed_runs(&send_keys_lines("%7", &bytes));
        assert_eq!(runs.concat(), bytes, "a title after {before} bytes");
        assert_eq!(runs[0].len(), first, "a title after {before} bytes");
    }
}

/// Reads typed bytes back out of `send-keys -H` commands for `%7`. Input: the commands.
/// Output: each command's bytes, in order.
fn typed_runs(lines: &[String]) -> Vec<Vec<u8>> {
    lines
        .iter()
        .map(|line| {
            let keys = line
                .strip_prefix("send-keys -t %7 -H ")
                .expect("send-keys -H for %7");
            keys.split(' ')
                .map(|key| u8::from_str_radix(key, 16).unwrap())
                .collect()
        })
        .collect()
}

/// V243-PASTE-CHUNKS: a full input request sent into a real pane arrives whole and in order,
/// now through one paste buffer, read back from what the pane's app received.
#[cfg(target_os = "macos")]
#[test]
fn a_full_input_request_reaches_the_pane_whole_and_in_order_across_its_commands() {
    let home = tempfile::tempdir().unwrap();
    let (host, _server, agent, received) = raw_cat_pane(home.path(), false);
    let bytes: Vec<u8> = (0..INPUT_BYTES_MAX)
        .map(|at| b' ' + (at * 7 % 95) as u8)
        .collect();
    host.input(&agent, &bytes).unwrap();
    assert_eq!(received_bytes(&received, bytes.len()), bytes);
}

/// V243-PASTE-BUFFER: a request longer than one `send-keys` run is pasted through a tmux
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

/// V243-PASTE-BUFFER: every one of the 255 non-NUL bytes round-trips through the
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

/// V243-PASTE-BUFFER: the engine pins `set-clipboard external` and `get-clipboard off` after
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

/// V243-PASTE-BUFFER: a full request whose `set-buffer` line is the longest one (every byte
/// value once, then only bytes that need an escape, `$HOME` among them) is pasted to an
/// agent's window target and reaches its pane byte-exact, with no buffer left behind.
#[cfg(target_os = "macos")]
#[test]
fn a_full_request_of_every_byte_value_is_pasted_byte_exact_with_no_buffer_left() {
    let home = tempfile::tempdir().unwrap();
    let (host, _server, agent, received) = raw_cat_pane(home.path(), false);
    let escaped: Vec<u8> = (1..=255u8)
        .filter(|byte| !byte.is_ascii_alphanumeric())
        .collect();
    let mut bytes: Vec<u8> = (1..=255u8).collect();
    bytes.extend_from_slice(b"$HOME ${HOME} ~ #{pane_id}");
    while bytes.len() < INPUT_BYTES_MAX {
        bytes.push(escaped[bytes.len() % escaped.len()]);
    }
    assert!(pastes(&bytes));
    host.input(&agent, &bytes).unwrap();
    assert_eq!(received_bytes(&received, bytes.len()), bytes);
    assert_eq!(buffers_left(&host), 0);
}

/// V243-PASTE-BUFFER: a full request holding NULs is typed, not pasted, and still reaches
/// the pane byte-exact.
#[cfg(target_os = "macos")]
#[test]
fn a_request_holding_a_nul_is_typed_byte_exact() {
    let home = tempfile::tempdir().unwrap();
    let (host, _server, agent, received) = raw_cat_pane(home.path(), false);
    let bytes: Vec<u8> = (0..INPUT_BYTES_MAX)
        .map(|at| {
            if at % 1000 == 999 {
                0
            } else {
                b' ' + (at * 7 % 95) as u8
            }
        })
        .collect();
    assert!(!pastes(&bytes));
    host.input(&agent, &bytes).unwrap();
    assert_eq!(received_bytes(&received, bytes.len()), bytes);
}

/// V243-PASTE-BUFFER: a 200 KB paste, sent as the app sends it in full input requests, each
/// pasted through its own buffer, reaches the pane byte-exact with no buffer left behind.
#[cfg(target_os = "macos")]
#[test]
fn a_200_kb_paste_in_full_requests_arrives_byte_exact_with_no_buffer_left() {
    let home = tempfile::tempdir().unwrap();
    let (host, _server, agent, received) = raw_cat_pane(home.path(), false);
    let bytes: Vec<u8> = (0..50 * INPUT_BYTES_MAX)
        .map(|at| 1 + (at * 13 % 255) as u8)
        .collect();
    assert_eq!(bytes.len(), 204_800);
    for request in bytes.chunks(INPUT_BYTES_MAX) {
        assert!(pastes(request));
        host.input(&agent, request).unwrap();
    }
    assert_eq!(received_bytes(&received, bytes.len()), bytes);
    assert_eq!(buffers_left(&host), 0);
}

/// V243-PASTE-BUFFER (h4 22:08): while a paste's named buffer waits between `set-buffer`
/// and `paste-buffer`, a pane's OSC 52 clipboard query gets no reply even with
/// `set-clipboard on`, since tmux answers that query only from an unnamed buffer; the same
/// query does get an unnamed buffer back, which shows the row would see a leak.
#[cfg(target_os = "macos")]
#[test]
fn a_pending_named_paste_buffer_is_not_read_by_a_panes_osc_52_query() {
    let home = tempfile::tempdir().unwrap();
    let (host, _server, agent, received) = raw_cat_pane(home.path(), true);
    let control = host.live_control().unwrap();
    let target = host.agent_target(&control, &agent).unwrap();
    control.run("set-option -s set-clipboard on").unwrap();
    control.run("set-option -s get-clipboard buffer").unwrap();
    let lines = paste_buffer_lines(&target, "crazyagent-paste-osc52-1", b"secret $HOME");
    assert_eq!(
        lines.len(),
        2,
        "a paste is one set-buffer and one paste-buffer"
    );
    assert!(lines[0].starts_with("set-buffer -b crazyagent-paste-osc52-1 -- "));
    control.run(&lines[0]).unwrap();
    assert_eq!(osc52_query(home.path(), &received, 1), 0);
    control.run("set-buffer -- \"control\"").unwrap();
    assert_eq!(osc52_query(home.path(), &received, 2), 1);
    let reply = b"\x1b]52;c;Y29udHJvbA==";
    assert!(
        std::fs::read(&received)
            .unwrap_or_default()
            .windows(reply.len())
            .any(|window| window == reply),
        "an unnamed buffer never came back to the same query"
    );
}

/// V243-PASTE-BUFFER (h4 MUX-RESULTS): a pane's OSC 52 query reads an unnamed buffer with
/// `set-clipboard on` and `get-clipboard buffer`, but no longer once the engine's clipboard
/// pins are applied, nor when `set-clipboard on` comes back after them, since
/// `get-clipboard off` still refuses it.
#[cfg(target_os = "macos")]
#[test]
fn the_clipboard_pins_stop_a_panes_osc_52_query_reading_an_unnamed_buffer() {
    let home = tempfile::tempdir().unwrap();
    let (host, _server, _agent, received) = raw_cat_pane(home.path(), true);
    let control = host.live_control().unwrap();
    control.run("set-option -s set-clipboard on").unwrap();
    control.run("set-option -s get-clipboard buffer").unwrap();
    control.run("set-buffer -- \"control\"").unwrap();
    assert_eq!(
        osc52_query(home.path(), &received, 1),
        1,
        "the unnamed buffer never came back before the pins"
    );
    let pins: Vec<String> = server_options(false)
        .into_iter()
        .filter(|option| option.contains("clipboard"))
        .collect();
    for pin in &pins {
        control.run(pin).unwrap();
    }
    assert_eq!(
        osc52_query(home.path(), &received, 2),
        1,
        "a query read the buffer through the pins"
    );
    control.run("set-option -s set-clipboard on").unwrap();
    assert_eq!(
        osc52_query(home.path(), &received, 3),
        1,
        "get-clipboard off let a query read the buffer"
    );
    assert_eq!(pins.len(), 2);
}

/// A real tmux server whose one agent pane runs `cat` on a raw terminal, writing what it
/// receives to a file. With `osc52_queries`, the pane also prints an OSC 52 clipboard query
/// and a device attributes query each time a `go` file appears in the home folder and
/// removes it, for at most 3000 polls and while `cat` lives. Takes the test's home folder and that choice. Returns the host,
/// the guard that stops the server, the agent and the file.
#[cfg(target_os = "macos")]
fn raw_cat_pane(home: &Path, osc52_queries: bool) -> (Arc<Tmux>, Server, AgentId, PathBuf) {
    cat_pane(home, osc52_queries, false)
}

/// The pane of `raw_cat_pane`, which with `bracketed` first turns bracketed paste on, as an
/// agent app that reads pastes does. Takes the home folder, the OSC 52 choice and that one.
/// Returns as `raw_cat_pane`.
#[cfg(target_os = "macos")]
fn cat_pane(
    home: &Path,
    osc52_queries: bool,
    bracketed: bool,
) -> (Arc<Tmux>, Server, AgentId, PathBuf) {
    use std::os::unix::fs::PermissionsExt;
    let folder = home.join("terminal");
    std::fs::create_dir(&folder).unwrap();
    std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o700)).unwrap();
    let tmux = installed(home).expect("tmux is required for this test");
    let host = Arc::new(Tmux::open(Some(tmux.clone()), &folder));
    let server = Server {
        tmux,
        socket: host.socket(),
    };
    let ready = home.join("ready");
    let received = home.join("received");
    let queries = if osc52_queries {
        format!(
            "( n=0; while [ $n -lt 3000 ] && kill -0 $$ 2>/dev/null; do \
             if [ -e '{go}' ]; then rm -f '{go}'; printf '\\033]52;c;?\\007\\033[c'; fi; \
             n=$((n + 1)); sleep 0.02; done ) & ",
            go = home.join("go").display()
        )
    } else {
        String::new()
    };
    let bracket = if bracketed {
        "printf '\\033[?2004h'; "
    } else {
        ""
    };
    let script = format!(
        "stty raw -echo; {bracket}: > '{}'; {queries}exec /bin/cat > '{}'",
        ready.display(),
        received.display()
    );
    let agent = AgentId::team(1).unwrap();
    let command = InteractiveCommand {
        argv: vec!["/bin/sh".into(), "-c".into(), script],
        cwd: home.to_path_buf(),
        env: Vec::new(),
        transcript: crate::harnesses::interactive::Transcript::Unavailable,
    };
    host.spawn(&agent, &command).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while !ready.exists() {
        assert!(
            Instant::now() < deadline,
            "the pane never set its terminal raw"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    (host, server, agent, received)
}

/// Waits until a file holds at least `length` bytes, for at most ten seconds. Takes the file
/// and the length. Returns the file's bytes.
#[cfg(target_os = "macos")]
fn received_bytes(file: &Path, length: usize) -> Vec<u8> {
    let deadline = Instant::now() + Duration::from_secs(10);
    while std::fs::metadata(file).map_or(0, |found| found.len()) < length as u64 {
        assert!(
            Instant::now() < deadline,
            "the pane never received every typed byte"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    std::fs::read(file).unwrap()
}

/// Makes the pane of `raw_cat_pane` with OSC 52 queries print one clipboard query and one
/// device attributes query, which tmux always answers, after any clipboard reply, since it
/// answers a pane's queries in order; then waits, for at most ten seconds, until that answer
/// is the `queries`-th in the pane's file, so no clipboard reply can still be on its way.
/// Takes the test's home folder, the pane's file and the queries so far, this one included.
/// Returns the OSC 52 replies the pane received.
#[cfg(target_os = "macos")]
fn osc52_query(home: &Path, received: &Path, queries: usize) -> usize {
    let count = |bytes: &[u8], pattern: &[u8]| -> usize {
        bytes
            .windows(pattern.len())
            .filter(|window| *window == pattern)
            .count()
    };
    let go = home.join("go");
    assert!(queries >= 1);
    assert!(!go.exists());
    let before = std::fs::read(received).unwrap_or_default();
    assert_eq!(count(&before, b"\x1b[?"), queries - 1);
    std::fs::write(&go, b"").unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let bytes = std::fs::read(received).unwrap_or_default();
        if count(&bytes, b"\x1b[?") == queries {
            return count(&bytes, b"\x1b]52;");
        }
        assert!(
            Instant::now() < deadline,
            "query {queries} got no device attributes answer"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// The number of tmux buffers on a host's server now. Takes the host. Returns the count.
#[cfg(target_os = "macos")]
fn buffers_left(host: &Tmux) -> usize {
    let control = host.live_control().unwrap();
    let names = control
        .ask(&format!("list-buffers -F '#{{buffer_name}}{FORMAT_END}'"))
        .unwrap();
    assert!(names.iter().all(|name| !name.is_empty()));
    names.len()
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

/// R1: the attach and the paste check read `#{bracket_paste_flag}`, which tmux has from 3.7
/// on. An older tmux answers it empty, so every attach ran out of time; such a tmux is too
/// old for agent terminals.
#[test]
fn only_tmux_three_seven_and_later_is_supported() {
    for (version, expected) in [
        ("3.8", Some(true)),
        ("3.7", Some(true)),
        ("3.7c", Some(true)),
        ("next-3.8", Some(true)),
        ("10.0", Some(true)),
        ("3.6b", Some(false)),
        ("next-3.6", Some(false)),
        ("3.4", Some(false)),
        ("3.3a", Some(false)),
        ("3.2a", Some(false)),
        ("3.1c", Some(false)),
        ("2.9", Some(false)),
        ("master", None),
        ("", None),
    ] {
        assert_eq!(supported(version), expected, "{version}");
    }
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
    let found = located(&[b"24 312 %2 @2 8 0 80 1 0 0 0 0 0 0 0 end".to_vec()]).unwrap();
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
        located(&[b"24 0 %2 @2 8 0 80 0 1 1 0 1 0 1 0 end".to_vec()])
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
        located(&[b"24 0 %2 @2 8 0 80 0 0 1 1   1  end".to_vec()])
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
        "24 {} %2 @2 8 0 80 1 0 0 0 0 0 0 0{FORMAT_END}",
        u64::from(SCROLLBACK_LINES) * 3
    );
    assert_eq!(
        located(&[full.into_bytes()]).unwrap().history,
        HISTORY_ROWS_MAX,
        "history past what the capture asks for is clamped to it"
    );
    assert!(located(&[b"24 312 %2 @2 8 0 80 end".to_vec()]).is_none());
    assert!(located(&[b"24 312 %2 @2 8 0 80 1 0 end".to_vec()]).is_none());
    assert!(located(&[b"24 312 %2 @2 8 0 80 1 x 0 0 0 0 0 0 end".to_vec()]).is_none());
    assert!(located(&[b"24 -4 %2 @2 8 0 80 1 0 0 0 0 0 0 0 end".to_vec()]).is_none());
    assert!(located(&[b"24 312 %2 @2 8 0 80 1 0 0 end".to_vec()]).is_none());
    assert!(located(&[b"24 312 %2 @2 8 0 80 1 0 2 0 0 0 0 0 end".to_vec()]).is_none());
    assert!(located(&[b"24 312 %2 @2 8 0 80 1 0 0 0 0 0 0 0 0 end".to_vec()]).is_none());
    assert!(located(&[b"24 0   0 0 80 0 0 0 0 0 0 0 0 end".to_vec()]).is_none());
    assert!(located(&[]).is_none());
    assert!(
        located(&[b"24 312 %2 @2 8 0 80 1 0 0 0 0 0 0 0".to_vec()]).is_none(),
        "a line without the end mark is one tmux cut short"
    );
    assert!(located(&[b"24 312 %1 @1 0 ".to_vec()]).is_none());
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
    let kept = kept_pane(lead.clone(), None).unwrap();
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
        kept_pane(plain("/private/tmp/other.sb"), None).unwrap(),
        kept_pane(lead.clone(), Some(Path::new("/private/tmp/state"))).unwrap(),
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

/// DEC-158: a kept pane's keeper takes the engine's freeze record as the word before its
/// `sandbox-exec`, so a kept freezable pane passes the pane gate with no variable of the
/// engine's own; a record that is not one absolute line of at most 1001 bytes is refused.
#[cfg(target_os = "macos")]
#[test]
fn a_kept_pane_hands_its_keeper_the_freeze_record_as_an_argument() {
    use crate::sandbox::{KEEPER, PANE_SHIM, SANDBOX_EXEC, kept_pane};
    let agent = AgentId::team(4).unwrap();
    let plain: Vec<String> = [
        SANDBOX_EXEC,
        "-f",
        "/private/tmp/agent.sb",
        "/usr/local/bin/claude",
    ]
    .map(String::from)
    .to_vec();
    let state = Path::new("/private/tmp/team/state");
    let kept = kept_pane(plain.clone(), Some(state)).unwrap();
    assert_eq!(kept.len(), KEEPER.len() + 1 + PANE_SHIM.len() + plain.len());
    assert_eq!(kept[KEEPER.len()], "/private/tmp/team/state");
    assert_eq!(kept[KEEPER.len() + 1..KEEPER.len() + 4], plain[..3]);
    assert_eq!(kept.last(), plain.last());
    let command = InteractiveCommand {
        argv: kept,
        cwd: PathBuf::from("/private/tmp/project"),
        env: vec![],
        transcript: crate::harnesses::interactive::Transcript::Unavailable,
    };
    assert!(spawn_line(&agent, &command).is_ok());
    let longest = format!("/{}", "a".repeat(1000));
    assert!(kept_pane(plain.clone(), Some(Path::new(&longest))).is_some());
    let longer = format!("/{}", "a".repeat(1001));
    for refused in [
        "private/tmp/state",
        "/private/tmp/a\nb",
        "/",
        longer.as_str(),
    ] {
        assert_eq!(
            kept_pane(plain.clone(), Some(Path::new(refused))),
            None,
            "{refused:?}"
        );
    }
}

/// DEC-158: a kept pane's keeper takes the engine's freeze record as an argument, so the
/// pane variable gate stays as it was and still refuses a pane command that carries the
/// keeper's own state variable.
#[cfg(target_os = "macos")]
#[test]
fn a_pane_command_with_the_keeper_state_variable_is_still_refused() {
    let agent = AgentId::team(4).unwrap();
    let command = InteractiveCommand {
        argv: vec!["/usr/local/bin/claude".into()],
        cwd: PathBuf::from("/private/tmp/project"),
        env: vec![(
            crate::sandbox::KEEPER_STATE.into(),
            "/private/tmp/state".into(),
        )],
        transcript: crate::harnesses::interactive::Transcript::Unavailable,
    };
    assert_eq!(
        spawn_line(&agent, &command).map_err(|error| error.0),
        Err("terminal_command_invalid".to_owned())
    );
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

struct Server {
    tmux: PathBuf,
    socket: PathBuf,
}

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

/// Counts the pane listings a host's held control client ran. Input: the host. Output: the
/// count, zero when no control client is held.
#[cfg(target_os = "macos")]
fn listed(host: &Tmux) -> usize {
    host.control.lock().unwrap().as_ref().map_or(0, |control| {
        control.listed.load(std::sync::atomic::Ordering::Relaxed)
    })
}

/// v242 typing C (BRIEF-typing-path): a pane view types its keys straight into the pane it
/// was attached to, with no pane listing per key while the control client that found that
/// pane still serves it; a pane its attach did not find never takes the view's keys, and a
/// pane found by another connection is typed into only after the usual lookup.
#[cfg(target_os = "macos")]
#[test]
fn a_pane_view_types_keys_into_its_attached_pane_with_no_pane_listing() {
    use std::os::unix::fs::PermissionsExt;
    const KEYS: usize = 20;
    let home = tempfile::tempdir().unwrap();
    let folder = home.path().join("terminal");
    std::fs::create_dir(&folder).unwrap();
    std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o700)).unwrap();
    let tmux = installed(home.path()).expect("tmux is required for this test");
    let host = Arc::new(Tmux::open(Some(tmux.clone()), &folder));
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
    host.spawn(&other, &shell("echo other-lead-screen; exec /bin/cat"))
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
    let elsewhere = client(&["list-panes", "-t", &target(&other), "-F", "#{pane_id}"]);
    assert!(pane_id_valid(&teammate), "{teammate}");
    assert!(pane_id_valid(&elsewhere), "{elsewhere}");
    assert_ne!(teammate, elsewhere);
    let shared: Arc<dyn Host> = host.clone();
    let shows = |terminal: &dyn Host, agent: &AgentId, wanted: &str| -> String {
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            let screen =
                String::from_utf8_lossy(&terminal.snapshot(agent).unwrap().screen).into_owned();
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
    let view = crate::terminal::pane_view(shared.clone(), &teammate).unwrap();
    shows(&*view, &lead, "teammate-pane-ready");
    let attached = view.attach(&lead).unwrap();
    assert!(
        shared.attach(&lead).unwrap().at.is_none(),
        "an own view on the engine's server keeps typing by the agent's name"
    );

    let typed = "q".repeat(KEYS);
    let before = listed(&host);
    for _ in 0..KEYS {
        crate::terminal::type_into(&*view, attached.at.as_ref(), &lead, b"q").unwrap();
    }
    crate::terminal::type_into(&*view, attached.at.as_ref(), &lead, b"\r").unwrap();
    assert_eq!(
        listed(&host),
        before,
        "{KEYS} keys typed into an attached pane view listed no panes"
    );
    shows(&*view, &lead, &typed);
    let own = shows(&*shared, &lead, "lead-own-screen");
    assert!(
        !own.contains(&typed),
        "the keys reached the teammate's pane only: {own}"
    );

    let at = attached
        .at
        .clone()
        .expect("a pane view's stream names the pane it was attached to");
    assert_eq!(at.pane, teammate);
    let forged = crate::terminal::PaneAt {
        pane: elsewhere.clone(),
        serial: at.serial,
    };
    crate::terminal::type_into(&*view, Some(&forged), &lead, b"forged-pane\r").unwrap();
    shows(&*view, &lead, "forged-pane");
    let stale = crate::terminal::PaneAt {
        pane: teammate.clone(),
        serial: at.serial.wrapping_add(1),
    };
    let before = listed(&host);
    crate::terminal::type_into(&*view, Some(&stale), &lead, b"stale-serial\r").unwrap();
    assert!(
        listed(&host) > before,
        "a pane found by another connection is typed into only after the lookup"
    );
    shows(&*view, &lead, "stale-serial");
    let seen = shows(&*shared, &other, "other-lead-screen");
    assert!(
        !seen.contains("forged-pane"),
        "a pane the view was not attached to never took its keys: {seen}"
    );
    assert!(
        !seen.contains("stale-serial") && !seen.contains(&typed),
        "no key of the view reached another agent's pane: {seen}"
    );

    let chain = crate::terminal::tmux_host(home.path(), Some(tmux.clone()));
    let routed = crate::terminal::pane_view(chain, &teammate).unwrap();
    let through = routed
        .attach(&lead)
        .unwrap()
        .at
        .expect("a pane view on the engine's host chain names the pane it was attached to");
    assert_eq!(through.pane, teammate);
    assert_ne!(
        through.serial, at.serial,
        "each host keeps its own control client"
    );
    assert_eq!(
        routed
            .input_at(&lead, &through, b"chain-typed\r")
            .map_err(|problem| problem.0),
        Ok(true),
        "the engine's host chain types straight into the attached pane"
    );
    shows(&*view, &lead, "chain-typed");
    assert_eq!(
        routed
            .input_at(&lead, &at, b"other-client\r")
            .map_err(|problem| problem.0),
        Ok(false),
        "a pane found by another control client is refused on the fast path"
    );
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

/// A tmux program answering as the stand-in server a lead swapped in for its own would: it
/// reports the lowest version the host takes ([`VERSION_MIN`]), its session lists `agent` and
/// every pane it reports has `pid` as its first process. Takes the folder it goes in, the
/// agent and the pid. Returns the program.
#[cfg(target_os = "macos")]
fn forging_tmux(folder: &Path, agent: &AgentId, pid: impl std::fmt::Display) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let program = folder.join("forging-tmux");
    let (major, minor) = VERSION_MIN;
    let script = format!(
        r#"#!/bin/sh
printf '%%begin 1 0 1\n%%end 1 0 1\n'
number=1
while IFS= read -r line; do
  printf '%%begin 1 %s 1\n' "$number"
  case "$line" in
    *'#{{version}}'*) printf '{major}.{minor} end\n' ;;
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

/// Writes an `sh` stand-in for tmux that adds a line to `ran` each time it starts, then does
/// `rest`. Takes the folder, the counting file and the rest of the script. Returns the tmux
/// as the resolver gives it.
fn counted_tmux(
    folder: &std::path::Path,
    ran: &std::path::Path,
    rest: &str,
) -> crate::tools::Resolved {
    use std::os::unix::fs::PermissionsExt;
    let program = folder.join(crate::tools::BUNDLED_TMUX);
    let script = format!("#!/bin/sh\necho ran >> '{}'\n{rest}\n", ran.display());
    std::fs::write(&program, script).unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
    crate::tools::Resolved {
        entry: program.clone(),
        program,
        kind: crate::tools::Kind::Native,
    }
}

/// How many times a [`counted_tmux`] started. Takes its counting file. Returns the count.
fn started(ran: &std::path::Path) -> usize {
    std::fs::read_to_string(ran).map_or(0, |text| text.lines().count())
}

#[test]
fn a_tmux_whose_version_read_runs_out_of_time_is_not_read_again_while_the_file_is_unchanged() {
    let folder = tempfile::tempdir().unwrap();
    let ran = folder.path().join("ran.txt");
    let tmux = counted_tmux(folder.path(), &ran, "exec /bin/sleep 30");
    let bound = std::time::Duration::from_secs(1);
    assert!(
        runs_within(&tmux, bound),
        "a read that runs out of time counts as running"
    );
    let first = started(&ran);
    assert!(first <= 1);
    let again = std::time::Instant::now();
    assert!(runs_within(&tmux, bound));
    assert_eq!(
        started(&ran),
        first,
        "every terminal start waited the whole bound again for the same file"
    );
    assert!(again.elapsed() < bound);
}

#[test]
fn a_tmux_whose_version_read_fails_is_asked_again_so_a_fix_outside_the_file_counts() {
    let folder = tempfile::tempdir().unwrap();
    let ran = folder.path().join("ran.txt");
    let tmux = counted_tmux(folder.path(), &ran, "exit 1");
    assert!(!runs(&tmux));
    assert_eq!(started(&ran), 1);
    assert!(!runs(&tmux));
    assert_eq!(started(&ran), 2, "a failed read is not kept");
}

/// Starts a test's own tmux host in a private folder with one sleeping agent per id.
/// Takes the home folder and the agent ids. Returns the host and its server, which ends
/// the server when dropped.
#[cfg(target_os = "macos")]
fn host_with_agents(home: &Path, ids: &[i64]) -> (Tmux, Server) {
    use std::os::unix::fs::PermissionsExt;
    let folder = home.join("terminal");
    std::fs::create_dir(&folder).unwrap();
    std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o700)).unwrap();
    let tmux = installed(home).expect("tmux is required for this test");
    let host = Tmux::open(Some(tmux.clone()), &folder);
    let server = Server {
        tmux,
        socket: host.socket(),
    };
    for id in ids {
        let command = InteractiveCommand {
            argv: vec!["/bin/sh".into(), "-c".into(), "exec /bin/sleep 60".into()],
            cwd: home.to_path_buf(),
            env: Vec::new(),
            transcript: crate::harnesses::interactive::Transcript::Unavailable,
        };
        host.spawn(&AgentId::team(*id).unwrap(), &command).unwrap();
    }
    (host, server)
}

/// Live views count per pane, not per tmux server: eight views of one agent leave room for
/// a view of another agent on the engine's same server, as a nine-tile wall needs, while a
/// ninth view of that same pane is still refused (RV-DS241-96).
#[cfg(target_os = "macos")]
#[test]
fn views_count_per_pane_so_eight_of_one_agent_leave_room_for_another() {
    let home = tempfile::tempdir().unwrap();
    let (host, _server) = host_with_agents(home.path(), &[1, 2]);
    let (one, two) = (AgentId::team(1).unwrap(), AgentId::team(2).unwrap());
    let views: Vec<TerminalStream> = (0..VIEWERS_MAX)
        .map(|_| host.attach(&one).unwrap())
        .collect();
    assert_eq!(
        host.attach(&one).err().map(|problem| problem.0),
        Some("terminal_viewers_full".to_owned())
    );
    let other = host.attach(&two);
    assert!(other.is_ok(), "{:?}", other.err());
    drop(views);
}

/// A view of a teammate pane ends once that pane closes while its window stays, so a closed
/// pane keeps no frozen view holding a view slot (RV-DS241-98).
#[cfg(target_os = "macos")]
#[test]
fn a_view_of_a_closed_pane_ends_though_its_window_stays() {
    let home = tempfile::tempdir().unwrap();
    let (host, server) = host_with_agents(home.path(), &[1]);
    let lead = AgentId::team(1).unwrap();
    let kept = host.panes(&lead).unwrap()[0].id.clone();
    split(&server, &kept, "exec /bin/sleep 60");
    let teammate = host
        .panes(&lead)
        .unwrap()
        .into_iter()
        .find(|pane| pane.id != kept)
        .unwrap()
        .id;
    let mut view = host.attach_pane(&lead, &teammate).unwrap();
    host.kill_pane(&lead, &teammate).unwrap();
    let (done, ended) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        while view.next_blocking().is_some() {}
        let _ = done.send(());
    });
    assert!(
        ended.recv_timeout(Duration::from_secs(10)).is_ok(),
        "the closed pane's view never ended"
    );
    assert_eq!(host.panes(&lead).unwrap().len(), 1);
}

const MARKER: &str = "5f0c1d2e3a4b59687766554433221100";

const LOCATED: &[u8] = b"2 0 %9 @1 0 1 80 0 0 0 0 0 0 0 0 end";

/// Feeds the control reader `text` as tmux's output, from a file through `cat`, so a line
/// longer than the argument limit reaches it whole. Takes the text and the reader's state.
fn read_text(text: &[u8], state: &Mutex<State>) {
    let folder = tempfile::tempdir().unwrap();
    let file = folder.path().join("output");
    std::fs::write(&file, text).unwrap();
    let mut cat = Command::new("/bin/cat");
    cat.arg(&file)
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = crate::spawn_gate::spawn(&mut cat).unwrap();
    let output = child.stdout.take().unwrap();
    read(output, state);
    let _ = child.wait();
}

/// An attach waiting at the front of the reader's queue. Takes what its located reply read,
/// if it came yet, and whether it opens a view. Returns the request and its answer.
fn attach(
    located: Option<&[u8]>,
    view: bool,
) -> (Pending, Receiver<Result<TerminalStream, HostError>>) {
    let (reply, answer) = sync_channel(1);
    let pending = Pending::Attach {
        agent: AgentId::team(1).unwrap(),
        located: located.map(|line| vec![line.to_vec()]),
        history: 0,
        view,
        reply,
        marker: MARKER.as_bytes().to_vec(),
    };
    (pending, answer)
}

/// What an attach answered: the size of the screen it opened, or the code it failed with,
/// none while it has no answer. Takes its answer.
fn attached(
    answer: &Receiver<Result<TerminalStream, HostError>>,
) -> Option<Result<(u16, u16), String>> {
    answer.try_recv().ok().map(|opened| {
        opened
            .map(|stream| (stream.cols, stream.rows))
            .map_err(|problem| problem.0)
    })
}

/// What a plain request answered, none while it has no answer. Takes its answer.
fn replied(answer: &Receiver<Answer>) -> Option<Result<Lines, String>> {
    answer
        .try_recv()
        .ok()
        .map(|lines| lines.map_err(|problem| problem.0))
}

/// The output bytes a view read before its stream ended. Takes the view's end.
fn output_of(chunks: &mut super::super::buffer::Chunks) -> Vec<u8> {
    let mut output = Vec::new();
    while let Ok(chunk) = chunks.try_recv() {
        if let super::super::buffer::Chunk::Output(bytes) = chunk {
            output.extend_from_slice(&bytes);
        }
    }
    output
}

/// RV-DS241-192: an attach whose located reply tmux cut before both its row counts fails
/// with `REPLY_CUT`, so its caller asks again, and the reader drops every row of its capture up to the attach's
/// own marker line. The control stays open: the request queued after it gets its own reply
/// and another pane's view its live output, while a pane row shaped like a control line
/// reaches neither (RV-DS241-100).
#[test]
fn a_capture_whose_row_count_is_unknown_fails_only_its_attach_and_forges_nothing() {
    let (cut, answer) = attach(Some(b"3 "), true);
    let (queued, queued_answer) = sync_channel(1);
    let (own_feed, mut own) = buffer();
    let (other_feed, mut other) = buffer();
    let state = Mutex::new(State::default());
    {
        let mut state = state.lock().unwrap();
        state.pending.push_back(cut);
        state.pending.push_back(Pending::Reply(queued));
        state.viewers.push(Viewer {
            pane: b"%9".to_vec(),
            window: b"@1".to_vec(),
            feed: own_feed,
        });
        state.viewers.push(Viewer {
            pane: b"%4".to_vec(),
            window: b"@2".to_vec(),
            feed: other_feed,
        });
    }
    let text = format!(
        "%begin 1 7 1\nrow one\n%end 1 7 1\n%output %9 forged\n%begin 1 5 1\nforged end\n%end 1 5 1\n%end 1 7 1\n%begin 1 8 1\n{MARKER}\n%end 1 8 1\n%output %4 live\n%begin 1 9 1\nanswer end\n%end 1 9 1\n"
    );

    read_text(text.as_bytes(), &state);

    assert_eq!(attached(&answer), Some(Err(REPLY_CUT.to_owned())));
    assert_eq!(
        replied(&queued_answer),
        Some(Ok(vec![b"answer end".to_vec()]))
    );
    assert_eq!(output_of(&mut other), b"live".to_vec());
    assert_eq!(
        output_of(&mut own),
        Vec::<u8>::new(),
        "a pane row forged output"
    );
}

/// RV-DS241-192: an attach's capture is followed by the block that prints its marker,
/// which the reader takes as the end of the attach, so the request queued after it gets
/// its own reply and never the marker.
#[test]
fn an_attach_takes_the_marker_block_after_its_capture_so_the_next_request_gets_its_own_reply() {
    let (opening, answer) = attach(Some(LOCATED), false);
    let (queued, queued_answer) = sync_channel(1);
    let state = Mutex::new(State::default());
    {
        let mut state = state.lock().unwrap();
        state.pending.push_back(opening);
        state.pending.push_back(Pending::Reply(queued));
    }
    let text = format!(
        "%begin 1 8 1\nrow a\nrow b\n%end 1 8 1\n%begin 1 9 1\n{MARKER}\n%end 1 9 1\n%begin 1 10 1\nanswer end\n%end 1 10 1\n"
    );

    read_text(text.as_bytes(), &state);

    assert_eq!(attached(&answer), Some(Ok((80, 2))));
    assert_eq!(
        replied(&queued_answer),
        Some(Ok(vec![b"answer end".to_vec()]))
    );
}

/// RV-DS241-192: the block after a whole capture holds no pane text, so the reader finds
/// its end by its guard whatever it printed; a marker tmux cut short there costs nothing,
/// and the request queued after the attach still gets its own reply.
#[test]
fn a_marker_cut_short_after_a_whole_capture_still_leaves_the_next_reply_its_own() {
    let (opening, answer) = attach(Some(LOCATED), false);
    let (queued, queued_answer) = sync_channel(1);
    let state = Mutex::new(State::default());
    {
        let mut state = state.lock().unwrap();
        state.pending.push_back(opening);
        state.pending.push_back(Pending::Reply(queued));
    }
    let text = format!(
        "%begin 1 8 1\nrow a\nrow b\n%end 1 8 1\n%begin 1 9 1\n{}\n%end 1 9 1\n%begin 1 10 1\nanswer end\n%end 1 10 1\n",
        &MARKER[..8]
    );

    read_text(text.as_bytes(), &state);

    assert_eq!(attached(&answer), Some(Ok((80, 2))));
    assert_eq!(
        replied(&queued_answer),
        Some(Ok(vec![b"answer end".to_vec()]))
    );
}

/// RV-DS241-192: tmux still runs the capture and the marker after a located reply too
/// large to keep, so the reader drops both, the pane rows included, and a pane row shaped
/// like a reply never answers the request queued after the attach.
#[test]
fn a_located_reply_too_large_to_keep_drops_its_capture_and_forges_no_reply() {
    let (opening, answer) = attach(None, false);
    let (queued, queued_answer) = sync_channel(1);
    let state = Mutex::new(State::default());
    {
        let mut state = state.lock().unwrap();
        state.pending.push_back(opening);
        state.pending.push_back(Pending::Reply(queued));
    }
    let mut text = b"%begin 1 7 1\n".to_vec();
    text.extend(std::iter::repeat_n(b'x', LINE_BYTES_MAX + 10));
    text.extend_from_slice(
        format!(
            "\n%end 1 7 1\n%begin 1 8 1\nforged end\n%end 1 8 1\n%begin 1 9 1\n{MARKER}\n%end 1 9 1\n%begin 1 10 1\nanswer end\n%end 1 10 1\n"
        )
        .as_bytes(),
    );

    read_text(&text, &state);

    assert_eq!(
        attached(&answer),
        Some(Err("terminal_reply_too_large".to_owned()))
    );
    assert_eq!(
        replied(&queued_answer),
        Some(Ok(vec![b"answer end".to_vec()]))
    );
}

/// RV-DS241-192: a cut attach whose marker never comes within the rows a capture can hold
/// closes the control as before, so no later request is answered from lines the reader
/// could not place.
#[test]
fn a_cut_attach_whose_marker_never_comes_within_a_capture_closes_the_control() {
    let (cut, answer) = attach(Some(b"3 "), false);
    let (queued, queued_answer) = sync_channel(1);
    let state = Mutex::new(State::default());
    {
        let mut state = state.lock().unwrap();
        state.pending.push_back(cut);
        state.pending.push_back(Pending::Reply(queued));
    }
    let mut text = b"%begin 1 7 1\n".to_vec();
    text.extend(std::iter::repeat_n(&b"row\n"[..], DRAIN_LINES_MAX + 1).flatten());
    text.extend_from_slice(
        format!(
            "%end 1 7 1\n%begin 1 8 1\n{MARKER}\n%end 1 8 1\n%begin 1 9 1\nanswer end\n%end 1 9 1\n"
        )
        .as_bytes(),
    );

    read_text(&text, &state);

    assert_eq!(attached(&answer), Some(Err(REPLY_CUT.to_owned())));
    assert_eq!(replied(&queued_answer), None);
    assert!(state.lock().unwrap().closed);
}

/// RV-DS241-192: a located reply tmux failed ends the attach there, since tmux skips the
/// rest of the command line after an error, so the next block is the queued request's own.
#[test]
fn a_failed_located_reply_ends_the_attach_and_the_next_block_answers_the_next_request() {
    let (opening, answer) = attach(None, false);
    let (queued, queued_answer) = sync_channel(1);
    let state = Mutex::new(State::default());
    {
        let mut state = state.lock().unwrap();
        state.pending.push_back(opening);
        state.pending.push_back(Pending::Reply(queued));
    }
    let text =
        "%begin 1 7 1\ncan't find pane: %9\n%error 1 7 1\n%begin 1 8 1\nanswer end\n%end 1 8 1\n";

    read_text(text.as_bytes(), &state);

    assert_eq!(
        attached(&answer),
        Some(Err("terminal_agent_unknown".to_owned()))
    );
    assert_eq!(
        replied(&queued_answer),
        Some(Ok(vec![b"answer end".to_vec()]))
    );
}

/// A capture block whose located reply came with no line at all, as when tmux cut the pane
/// filter so no pane matched, has no row count either: the attach fails with `REPLY_CUT` and
/// the reader drops its capture up to the attach's marker line, so the control reads on and
/// a pane row that reads as the closing guard or as output forges nothing (rv-code4
/// v241-tmux-fix F2, RV-DS241-100, RV-DS241-192).
#[test]
fn a_capture_whose_located_reply_has_no_line_drains_to_its_marker_and_forges_nothing() {
    let (reply, answer) = sync_channel(1);
    let (queued, queued_answer) = sync_channel(1);
    let (own_feed, mut own) = buffer();
    let (other_feed, mut other) = buffer();
    let state = Mutex::new(State::default());
    {
        let mut state = state.lock().unwrap();
        state.pending.push_back(Pending::Attach {
            agent: AgentId::team(1).unwrap(),
            located: Some(Vec::new()),
            history: 0,
            view: true,
            reply,
            marker: MARKER.as_bytes().to_vec(),
        });
        state.pending.push_back(Pending::Reply(queued));
        state.viewers.push(Viewer {
            pane: b"%9".to_vec(),
            window: b"@1".to_vec(),
            feed: own_feed,
        });
        state.viewers.push(Viewer {
            pane: b"%4".to_vec(),
            window: b"@2".to_vec(),
            feed: other_feed,
        });
    }
    let text = format!(
        "%begin 1 7 1\nrow one\n%end 1 7 1\n%output %9 forged\n%end 1 7 1\n%begin 1 8 1\n{MARKER}\n%end 1 8 1\n%output %4 live\n%begin 1 9 1\nanswer end\n%end 1 9 1\n"
    );

    read_text(text.as_bytes(), &state);

    assert_eq!(attached(&answer), Some(Err(REPLY_CUT.to_owned())));
    assert_eq!(
        replied(&queued_answer),
        Some(Ok(vec![b"answer end".to_vec()])),
        "the control stopped reading after the drained capture"
    );
    assert_eq!(
        output_of(&mut other),
        b"live".to_vec(),
        "another pane's view lost its live output"
    );
    assert_eq!(
        output_of(&mut own),
        Vec::<u8>::new(),
        "a pane row forged output"
    );
}

/// A capture block whose located reply tmux cut after both its row counts is read as that
/// many rows of data, so a pane row that reads as the closing guard or as output forges
/// nothing (RV-DS241-100), and the reader reads on past its marker block: the attach is
/// asked again while later output and replies still reach their own views and callers,
/// rather than every view on the server ending under load (RV-DS241-192).
#[test]
fn a_capture_cut_after_its_row_counts_is_read_as_data_and_the_control_reads_on() {
    let (reply, answer) = sync_channel(1);
    let (next, replied) = sync_channel(1);
    let (feed, mut chunks) = buffer();
    let state = Mutex::new(State::default());
    {
        let mut state = state.lock().unwrap();
        state.pending.push_back(Pending::Attach {
            agent: AgentId::team(1).unwrap(),
            located: Some(vec![b"3 0 %9 @1 0 ".to_vec()]),
            history: 0,
            view: true,
            reply,
            marker: MARKER.as_bytes().to_vec(),
        });
        state.pending.push_back(Pending::Reply(next));
        state.viewers.push(Viewer {
            pane: b"%9".to_vec(),
            window: b"@1".to_vec(),
            feed,
        });
    }
    let text = format!(
        "%begin 1 7 1\nrow one\n%end 1 7 1\n%output %9 forged\n%end 1 7 1\n%begin 1 8 1\n{MARKER}\n%end 1 8 1\n%output %9 real\n%begin 1 9 1\nnext\n%end 1 9 1\n"
    );
    let mut printf = Command::new("/usr/bin/printf");
    printf
        .args(["%s", text.as_str()])
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = crate::spawn_gate::spawn(&mut printf).unwrap();
    let output = child.stdout.take().unwrap();
    read(output, &state);
    let _ = child.wait();
    let asked = answer
        .try_recv()
        .expect("the cut attach was never answered");
    assert_eq!(asked.err().map(|cut| cut.0), Some(REPLY_CUT.to_owned()));
    let later = replied
        .try_recv()
        .expect("the next caller was never answered");
    assert_eq!(later.unwrap(), vec![b"next".to_vec()]);
    assert_eq!(
        chunks.try_recv().ok(),
        Some(crate::terminal::buffer::Chunk::Output(b"real".to_vec())),
        "the view lost later output or a pane row forged some"
    );
}

/// The rows an attach's capture prints are read from the pane height and history size that
/// open its pane line, also from a line tmux cut short once both came whole with the space
/// after them, the history held to what the attach asked for; a line cut before that, or one
/// of another shape, has no count and is drained to its marker, and a reply that is no
/// attach's capture is searched.
#[test]
fn an_attach_capture_is_counted_from_a_cut_line_once_both_counts_came_whole() {
    fn rows(line: &[u8], history: usize) -> Option<usize> {
        let (reply, _answer) = sync_channel(1);
        let mut state = State::default();
        state.pending.push_back(Pending::Attach {
            agent: AgentId::team(1).unwrap(),
            located: Some(vec![line.to_vec()]),
            history,
            view: false,
            reply,
            marker: MARKER.as_bytes().to_vec(),
        });
        match opening(&mut state) {
            Rows::Counted(rows) => Some(rows),
            Rows::Drained(_) => None,
            Rows::Searched => panic!("an attach's capture was searched for its guard"),
        }
    }
    let whole = b"24 312 %2 @2 8 0 80 1 0 0 0 0 0 0 0 end";
    assert_eq!(rows(whole, HISTORY_ROWS_MAX), Some(24 + 312));
    assert_eq!(rows(whole, 100), Some(24 + 100));
    assert_eq!(rows(b"24 312 %2 @2 8 ", 0), Some(24));
    assert_eq!(rows(b"24 312 ", HISTORY_ROWS_MAX), Some(24 + 312));
    assert_eq!(rows(b"24 312", HISTORY_ROWS_MAX), None);
    assert_eq!(rows(b"24 ", HISTORY_ROWS_MAX), None);
    assert_eq!(rows(b"", HISTORY_ROWS_MAX), None);
    assert_eq!(rows(b"%2 @2 8 0 80 24 1 0 312 ", HISTORY_ROWS_MAX), None);
    let deep = format!("24 {} %2 ", u64::from(SCROLLBACK_LINES) * 3);
    assert_eq!(
        rows(deep.as_bytes(), HISTORY_ROWS_MAX),
        Some(24 + HISTORY_ROWS_MAX)
    );
    let (reply, _answer) = sync_channel(1);
    let mut state = State::default();
    state.pending.push_back(Pending::Reply(reply));
    assert!(matches!(opening(&mut state), Rows::Searched));
}

/// A window layout names every pane it holds, in leaf cells at any depth of splits, and a
/// layout that is not read names none, so it ends no view.
#[test]
fn a_layout_names_the_panes_of_every_leaf_cell() {
    let ids = |layout: &str| {
        layout_panes(layout.as_bytes())
            .into_iter()
            .map(|pane| String::from_utf8(pane).unwrap())
            .collect::<Vec<_>>()
    };
    assert_eq!(ids("c0de,80x24,0,0,7"), ["%7"]);
    assert_eq!(
        ids("b25f,80x24,0,0{40x24,0,0,1,39x24,41,0,2}"),
        ["%1", "%2"]
    );
    assert_eq!(
        ids("a1b2,80x24,0,0[80x12,0,0,3,80x11,0,13{40x11,0,13,4,39x11,41,13,5}]"),
        ["%3", "%4", "%5"]
    );
    assert!(ids("").is_empty());
    assert!(ids("not a layout").is_empty());
}

/// B6b (Q4 option A): a native lead's server keeper on an engine key relay also holds a
/// lifeline from the engine, its standard input, so the teammates it keeps end when the
/// engine dies, a crash included, not only at the deadline. Asked to by its variable, a
/// keeper whose input closes kills its command from outside and exits.
#[cfg(target_os = "macos")]
#[test]
fn a_keeper_on_a_lifeline_ends_its_command_once_the_engine_side_closes() {
    use std::os::unix::process::CommandExt;
    let folder = tempfile::tempdir().unwrap();
    let started = folder.path().join("started");
    let mut keeper = crate::spawn_gate::spawn(
        std::process::Command::new(crate::sandbox::KEEPER[0])
            .args(&crate::sandbox::KEEPER[1..])
            .args(["/bin/sh", "-c", "echo $$ > \"$0\"; exec /bin/sleep 600"])
            .arg(&started)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("CRAZYAGENT_KEEPER_LIFELINE", "0")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0),
    )
    .unwrap();
    let until = Instant::now() + Duration::from_secs(10);
    let command = loop {
        let text = std::fs::read_to_string(&started).unwrap_or_default();
        let pid = text.trim().parse::<i32>().ok();
        if let Some(pid) = pid.and_then(rustix::process::Pid::from_raw) {
            break pid;
        }
        if Instant::now() >= until {
            let _ = keeper.kill();
            let _ = keeper.wait();
            panic!("the keeper starts its command");
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    drop(keeper.stdin.take());
    let until = Instant::now() + Duration::from_secs(15);
    let status = loop {
        match keeper.try_wait().unwrap() {
            Some(status) => break Some(status),
            None if Instant::now() >= until => break None,
            None => std::thread::sleep(Duration::from_millis(100)),
        }
    };
    let gone = || rustix::process::test_kill_process(command).is_err();
    let until = Instant::now() + Duration::from_secs(10);
    while !gone() && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(100));
    }
    let ended = gone();
    if status.is_none() {
        let _ = keeper.kill();
        let _ = keeper.wait();
    }
    if !ended {
        let _ = rustix::process::kill_process(command, rustix::process::Signal::KILL);
    }
    assert!(status.is_some(), "the keeper exits on its lifeline");
    assert!(ended, "the command the keeper kept ends with it");
}

/// B6b: a keeper not asked to watch a lifeline, as every pane keeper and every server keeper
/// of a lead off a key relay, never reads its input, so an input closed from the start, the
/// `/dev/null` the engine gives it, leaves the keeper and its command running.
#[cfg(target_os = "macos")]
#[test]
fn a_keeper_without_a_lifeline_keeps_its_command_whatever_its_input() {
    use std::os::unix::process::CommandExt;
    let folder = tempfile::tempdir().unwrap();
    let started = folder.path().join("started");
    let mut keeper = crate::spawn_gate::spawn(
        std::process::Command::new(crate::sandbox::KEEPER[0])
            .args(&crate::sandbox::KEEPER[1..])
            .args(["/bin/sh", "-c", "echo $$ > \"$0\"; exec /bin/sleep 600"])
            .arg(&started)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0),
    )
    .unwrap();
    let until = Instant::now() + Duration::from_secs(10);
    let command = loop {
        let text = std::fs::read_to_string(&started).unwrap_or_default();
        let pid = text.trim().parse::<i32>().ok();
        if let Some(pid) = pid.and_then(rustix::process::Pid::from_raw) {
            break pid;
        }
        if Instant::now() >= until {
            let _ = keeper.kill();
            let _ = keeper.wait();
            panic!("the keeper starts its command");
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    std::thread::sleep(Duration::from_secs(3));
    let kept = keeper.try_wait().unwrap().is_none();
    let running = rustix::process::test_kill_process(command).is_ok();
    let _ = keeper.kill();
    let _ = keeper.wait();
    let _ = rustix::process::kill_process(command, rustix::process::Signal::KILL);
    assert!(kept, "the keeper runs on");
    assert!(running, "its command runs on");
}

/// B6b: the keeper's lifeline variable is the engine's own, so the pane variable gate refuses
/// a pane command that carries it, as it refuses the keeper's state variable.
#[cfg(target_os = "macos")]
#[test]
fn a_pane_command_with_the_keeper_lifeline_variable_is_refused() {
    let agent = AgentId::team(4).unwrap();
    let command = InteractiveCommand {
        argv: vec!["/usr/local/bin/claude".into()],
        cwd: PathBuf::from("/private/tmp/project"),
        env: vec![(crate::sandbox::KEEPER_LIFELINE.into(), "0".into())],
        transcript: crate::harnesses::interactive::Transcript::Unavailable,
    };
    assert_eq!(
        spawn_line(&agent, &command).map_err(|error| error.0),
        Err("terminal_command_invalid".to_owned())
    );
}

/// Decodes the one double-quoted argument of a `set-buffer` command back to its bytes: ASCII
/// letters and digits as they are, every other byte from its `\ooo` escape. Takes the
/// command. Returns the bytes.
fn set_buffer_bytes(line: &str) -> Vec<u8> {
    let quoted = line
        .split_once(" -- \"")
        .and_then(|(_, rest)| rest.strip_suffix('"'))
        .expect("set-buffer with one double-quoted argument");
    let mut decoded = Vec::new();
    let mut bytes = quoted.bytes();
    while let Some(byte) = bytes.next() {
        if byte.is_ascii_alphanumeric() {
            decoded.push(byte);
            continue;
        }
        assert_eq!(byte, b'\\', "a raw {byte:#04x} inside the quotes");
        let digits: Vec<u8> = bytes.by_ref().take(3).collect();
        assert_eq!(digits.len(), 3);
        decoded.push(u8::from_str_radix(std::str::from_utf8(&digits).unwrap(), 8).unwrap());
    }
    decoded
}

/// Slice C (DEC-203 T7): an owner paste goes to tmux as one `set-buffer` per piece of at most
/// `INPUT_BYTES_MAX` bytes, each piece after the first appended, then one `paste-buffer -d
/// -p -r -S`, which brackets the whole only when the pane asked; the pieces decode back to
/// the keys in order.
#[test]
fn an_owner_paste_is_set_in_appended_pieces_then_pasted_once_with_p() {
    let keys: Vec<u8> = (0..2 * INPUT_BYTES_MAX + 1)
        .map(|at| b' ' + (at % 95) as u8)
        .collect();
    let lines = owner_paste_lines("%7", "crazyagent-paste-1-3", &keys);
    assert_eq!(lines.len(), 4);
    assert!(lines[0].starts_with("set-buffer -b crazyagent-paste-1-3 -- \""));
    assert!(lines[1].starts_with("set-buffer -a -b crazyagent-paste-1-3 -- \""));
    assert!(lines[2].starts_with("set-buffer -a -b crazyagent-paste-1-3 -- \""));
    assert_eq!(
        lines[3],
        "paste-buffer -d -p -r -S -b crazyagent-paste-1-3 -t %7"
    );
    let decoded: Vec<u8> = lines[..3]
        .iter()
        .flat_map(|line| set_buffer_bytes(line))
        .collect();
    assert_eq!(decoded, keys);
}

/// Slice C (DEC-203 T7): a 200 KB owner paste into a pane whose app turned bracketed paste
/// on arrives once inside one bracket pair, byte-exact; into a pane that did not, it arrives
/// unwrapped; no buffer is left behind either way.
#[cfg(target_os = "macos")]
#[test]
fn an_owner_paste_is_bracketed_once_only_when_the_pane_asked() {
    for bracketed in [true, false] {
        let home = tempfile::tempdir().unwrap();
        let (host, _server, agent, received) = cat_pane(home.path(), false, bracketed);
        let deadline = Instant::now() + Duration::from_secs(10);
        while host.input_live(&agent).unwrap() != bracketed {
            assert!(
                Instant::now() < deadline,
                "the pane never set its paste mode"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        let keys: Vec<u8> = (0..50 * INPUT_BYTES_MAX + 7)
            .map(|at| b' ' + (at * 7 % 95) as u8)
            .collect();
        host.paste(&agent, &keys).unwrap();
        let mut expected = Vec::new();
        if bracketed {
            expected.extend_from_slice(b"\x1b[200~");
        }
        expected.extend_from_slice(&keys);
        if bracketed {
            expected.extend_from_slice(PASTE_CLOSE);
        }
        assert_eq!(
            received_bytes(&received, expected.len()),
            expected,
            "bracketed paste {bracketed}"
        );
        assert_eq!(buffers_left(&host), 0);
    }
}

/// #477 r2 / #483: tmux 3.8 answers `display-message -t` for a gone window with success and
/// the current pane's values, so a terminal whose window is gone, on the engine's own server
/// where no lookup runs first, must read as `terminal_agent_unknown`, never as that other
/// pane's paste mode or activity; a live terminal still reads its own.
#[test]
fn a_gone_window_reads_as_unknown_not_as_the_current_pane() {
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
        argv: vec!["/bin/sleep".into(), "60".into()],
        cwd: home.path().to_path_buf(),
        env: Vec::new(),
        transcript: crate::harnesses::interactive::Transcript::Unavailable,
    };
    host.spawn(&agent, &command).unwrap();
    assert_eq!(host.input_live(&agent).map_err(|error| error.0), Ok(false));
    assert!(host.quiet(&agent).unwrap().is_some());
    let gone = AgentId::team(9).unwrap();
    assert_eq!(
        host.input_live(&gone).map_err(|error| error.0),
        Err("terminal_agent_unknown".to_owned())
    );
    assert_eq!(
        host.quiet(&gone).map_err(|error| error.0),
        Err("terminal_agent_unknown".to_owned())
    );
}
