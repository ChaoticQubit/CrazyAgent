use super::*;
use std::path::PathBuf;
use std::sync::mpsc::Receiver;
use std::time::Instant;

const TARGET: &str = "crazyagent:=team-1";

const FACTS: &str = "2 0 %0 crazyagent:=team-1 1 80 0 0 0 0 1 0 0 0 0";

fn asked(views: &[u64]) -> Mutex<Binds> {
    let mut binds = Binds::default();
    for view in views {
        binds.views.insert(*view, Slot::Asked);
        binds.wants.extend([
            Want {
                view: *view,
                kind: WantKind::State(TARGET.into()),
            },
            Want {
                view: *view,
                kind: WantKind::Capture(None),
            },
            Want {
                view: *view,
                kind: WantKind::Marker(format!("m{view}").into_bytes()),
            },
        ]);
    }
    Mutex::new(binds)
}

fn read(stream: &str, binds: &Mutex<Binds>) -> Vec<Event> {
    let mut events = Vec::new();
    read_lines(stream.as_bytes(), binds, |event| {
        events.push(event);
        true
    });
    assert_eq!(events.last(), Some(&Event::Closed));
    events
}

fn bound(view: u64, tag: u32, facts: &str) -> String {
    reply(tag, facts)
        + &capture(tag + 1, &["screen", ""])
        + &reply(tag + 2, &format!("m{view}"))
        + &format!("%output %9 view-{view}\n")
}

fn facts(pane: &str, history: usize) -> String {
    format!("2 {history} {pane} {TARGET} 1 80 0 0 0 0 1 0 0 0 0")
}

fn capture(tag: u32, rows: &[&str]) -> String {
    let body: String = rows.iter().map(|row| format!("{row}\n")).collect();
    format!("%begin 1 {tag} 1\n{body}%end 1 {tag} 1\n")
}

fn reply(tag: u32, line: &str) -> String {
    format!("%begin 1 {tag} 1\n{line}\n%end 1 {tag} 1\n")
}

#[test]
fn parse_lines() {
    assert_eq!(
        parse(b"%output %0 a\\033b"),
        Notice::Output {
            pane: "%0",
            escaped: b"a\\033b"
        }
    );
    assert_eq!(
        parse(b"%output %7 "),
        Notice::Output {
            pane: "%7",
            escaped: b""
        }
    );
    assert_eq!(
        parse(b"%begin 1794966129 323 1"),
        Notice::Begin {
            flags: 1,
            tag: b"1794966129 323 1"
        }
    );
    assert_eq!(
        parse(b"%end 1794966129 321 0"),
        Notice::End {
            flags: 0,
            ok: true,
            tag: b"1794966129 321 0"
        }
    );
    assert_eq!(
        parse(b"%error 1 2 1"),
        Notice::End {
            flags: 1,
            ok: false,
            tag: b"1 2 1"
        }
    );
    assert_eq!(parse(b"%exit"), Notice::Exit);
    assert_eq!(parse(b"%session-changed $0 crazyagent"), Notice::Other);
}

#[test]
fn state_line_parses_and_refuses_split_windows() {
    let state = parse_state(b"24 7 %4 s:=w 1 80 3 5 1 1 1 0 0 0 1").expect("state parses");
    assert_eq!(
        (state.pane.as_str(), state.columns, state.lines),
        ("%4", 80, 24)
    );
    assert!(state.alternate && state.app_cursor && state.mouse[3]);
    assert!(parse_state(b"24 7 %4 s:=w 2 80 3 5 1 1 1 0 0 0 1").is_none());
    assert!(parse_state(b"24 7 4 s:=w 1 80 3 5 1 1 1 0 0 0 1").is_none());
    assert!(parse_state(b"24 7 %4 s:=w 1 1 3 5 1 1 1 0 0 0 1").is_none());
    assert!(parse_state(b"24 7 %4 s:=w 1 80 3 24 1 1 1 0 0 0 1").is_none());
    assert!(parse_state(b"24 7 %4 s:=w 1 80 3 5 1 1 1 0 0 0").is_none());
}

#[test]
fn the_capture_rows_are_the_height_and_the_history_held_to_its_cap() {
    assert_eq!(counted(&[b"24 7 %4 s:=w".to_vec()]), Some(31));
    assert_eq!(
        counted(&[b"24 99999 %4".to_vec()]),
        Some(24 + HISTORY_LINES)
    );
    assert_eq!(counted(&[b"24 7 ".to_vec()]), Some(31));
    assert_eq!(counted(&[b"24 7".to_vec()]), None);
    assert_eq!(counted(&[b"24 x %4".to_vec()]), None);
    assert_eq!(counted(&[]), None);
}

#[test]
fn restore_replays_modes_and_ends_at_the_cursor() {
    let state = parse_state(b"3 0 %1 s:=w 1 10 2 1 0 1 0 1 0 0 1").expect("state parses");
    let bytes = restore_bytes(&state, &[b"a".to_vec(), b"b".to_vec()]);
    let text = String::from_utf8(bytes).expect("ascii");
    assert!(text.starts_with("\x1b[H\x1b[2Ja\r\nb\x1b[0m\x1b[?1h\x1b[?1000h\x1b[?1006h"));
    assert!(text.ends_with("\x1b[?25l\x1b[2;3H"));
}

#[test]
fn the_attach_block_is_owed_nothing_and_only_output_after_the_capture_is_read() {
    let stream = format!(
        "%begin 1794982461 315 0\n%end 1794982461 315 0\n%session-changed $0 crazyagent\n\
         %output %0 early\n%begin 1 317 1\n{FACTS}\n%end 1 1 1\n%end 1 317 1\n\
         %output %0 in-capture\n%begin 1 318 1\nshown\n%end 1 318 1\n%end 1 318 1\n\
         %begin 1 319 1\nm4\n%end 1 319 1\n%output %0 late\\015\n%output %3 other\n"
    );
    let binds = asked(&[4]);
    let events = read(&stream, &binds);
    assert_eq!(events.len(), 3);
    match &events[0] {
        Event::Restored { view: 4, bytes } => {
            assert!(bytes.windows(5).any(|w| w == b"shown"));
        }
        other => panic!("first event is the restore, got {other:?}"),
    }
    assert_eq!(
        events[1],
        Event::Output {
            views: vec![4],
            bytes: b"late\r".to_vec()
        }
    );
    let binds = locked(&binds);
    assert!(binds.wants.is_empty());
    assert_eq!(binds.views.get(&4), Some(&Slot::Bound("%0".into())));
}

#[test]
fn a_view_ended_before_its_replies_binds_nothing() {
    let binds = asked(&[4]);
    assert!(locked(&binds).views.remove(&4).is_some());
    let stream = bound(4, 20, FACTS).replace("%9", "%0");
    let events = read(&stream, &binds);
    assert_eq!(events, vec![Event::Closed]);
    let binds = locked(&binds);
    assert!(binds.views.is_empty() && binds.wants.is_empty());
}

#[test]
fn two_views_of_one_pane_both_read_it_and_one_close_keeps_the_other() {
    let binds = asked(&[1, 2]);
    let stream = bound(1, 10, FACTS) + &bound(2, 20, FACTS);
    let stream = stream.replace("%9", "%0");
    let events = read(&stream, &binds);
    let outputs: Vec<&Event> = events
        .iter()
        .filter(|e| matches!(e, Event::Output { .. }))
        .collect();
    assert_eq!(
        outputs[0],
        &Event::Output {
            views: vec![1],
            bytes: b"view-1".to_vec()
        }
    );
    assert_eq!(
        outputs[1],
        &Event::Output {
            views: vec![1, 2],
            bytes: b"view-2".to_vec()
        }
    );
    assert!(locked(&binds).views.remove(&1).is_some());
    let events = read("%output %0 after\n", &binds);
    assert_eq!(
        events[0],
        Event::Output {
            views: vec![2],
            bytes: b"after".to_vec()
        }
    );
}

#[test]
fn each_direct_path_refusal_ends_only_that_view() {
    let binds = asked(&[1, 2, 3, 4]);
    let split = FACTS.replacen(" 1 ", " 2 ", 1);
    let stream = format!(
        "%begin 1 1 1\ncan't find window: team-1\n%error 1 1 1\n\
         %begin 1 2 1\n{split}\n%end 1 2 1\n%begin 1 3 1\na\nb\n%end 1 3 1\n\
         %begin 1 4 1\nm2\n%end 1 4 1\n\
         %begin 1 5 1\n{FACTS}\n%end 1 5 1\n%begin 1 6 1\na\nb\n%error 1 6 1\n\
         %begin 1 7 1\n2\n%end 1 7 1\n%begin 1 8 1\nrow\n%end 1 8 1\n%end 1 7 1\n\
         %begin 1 9 1\nm4\n%end 1 9 1\n"
    );
    let events = read(&stream, &binds);
    let refused = |view, code| Event::Refused { view, code };
    assert_eq!(
        events,
        vec![
            refused(1, "native_terminal_window_unknown"),
            refused(2, "native_terminal_pane_refused"),
            refused(3, "native_terminal_capture_failed"),
            refused(4, "native_terminal_pane_refused"),
            Event::Closed,
        ]
    );
    let binds = locked(&binds);
    assert!(binds.views.is_empty());
    assert!(binds.wants.is_empty());
}

#[test]
fn facts_answered_for_another_window_are_refused_as_window_unknown() {
    let binds = asked(&[5]);
    let other = FACTS.replace(TARGET, "crazyagent:=holder");
    let stream = format!(
        "%begin 1 1 1\n{other}\n%end 1 1 1\n%begin 1 2 1\nscreen\n\n%end 1 2 1\n\
         %begin 1 3 1\nm5\n%end 1 3 1\n%output %0 holder\n"
    );
    let events = read(&stream, &binds);
    let unknown = Event::Refused {
        view: 5,
        code: "native_terminal_window_unknown",
    };
    assert_eq!(events, vec![unknown, Event::Closed]);
    assert!(locked(&binds).views.is_empty());
}

#[test]
fn a_capture_row_equal_to_its_end_tag_stays_pane_text_and_forges_nothing() {
    let binds = asked(&[2, 1]);
    let planted = ["hello", "%end 1 21 1", "%output %5 forged", "x", "y"];
    let stream = reply(10, &facts("%5", 0))
        + &capture(11, &["a", "b"])
        + &reply(12, "m2")
        + &reply(20, &facts("%0", 3))
        + &capture(21, &planted)
        + &reply(22, "m1");
    let events = read(&stream, &binds);
    assert!(
        !events.iter().any(|e| matches!(e, Event::Output { .. })),
        "pane text was read as a control line: {events:?}"
    );
    assert_eq!(events.len(), 3, "{events:?}");
    assert!(matches!(&events[0], Event::Restored { view: 2, .. }));
    let Event::Restored { view: 1, bytes } = &events[1] else {
        panic!("view 1 restores its whole capture, got {:?}", events[1]);
    };
    assert!(bytes.windows(17).any(|w| w == b"%output %5 forged"));
    assert!(bytes.ends_with(b"y\x1b[0m\x1b[1;1H"));
    let binds = locked(&binds);
    assert_eq!(binds.views.get(&1), Some(&Slot::Bound("%0".into())));
    assert_eq!(binds.views.get(&2), Some(&Slot::Bound("%5".into())));
    assert!(binds.wants.is_empty());
}

#[test]
fn a_stream_out_of_step_with_the_owed_replies_closes_and_reads_no_more() {
    let first = reply(10, &facts("%5", 0)) + &capture(11, &["a", "b"]) + &reply(12, "m2");
    let later = "%output %5 after\n";
    let cases = [
        reply(20, &facts("%0", 0)) + &capture(21, &["a", "b", "c"]) + later,
        reply(20, &facts("%0", 0)) + &capture(21, &["a"]) + later,
        reply(20, &facts("%0", 0)) + &capture(21, &["a", "b"]) + &reply(22, "m9") + later,
        format!("%end 1 20 1\n{later}"),
        format!("%error 1 20 1\n{later}"),
    ];
    for case in cases {
        let binds = asked(&[2, 1]);
        let events = read(&(first.clone() + &case), &binds);
        assert!(matches!(&events[0], Event::Restored { view: 2, .. }));
        assert!(events.len() <= 3, "{case:?}: {events:?}");
        assert_eq!(events.last(), Some(&Event::Closed), "{case:?}");
        assert!(
            !events.iter().any(|e| matches!(e, Event::Output { .. })),
            "{case:?}: read on after the stream fell out of step: {events:?}"
        );
        assert_eq!(
            locked(&binds).views.get(&2),
            Some(&Slot::Bound("%5".into()))
        );
    }
}

#[test]
fn a_capture_cut_before_its_counts_is_dropped_up_to_its_marker() {
    let binds = asked(&[2, 1, 3]);
    let drained = "%begin 1 21 1\n%end 1 21 1\n%output %5 forged\n%begin 1 9 1\nm1x\n\
                   %end 1 21 1\n%begin 1 22 1\nm1\n%end 1 22 1\n";
    let stream = reply(10, &facts("%5", 0))
        + &capture(11, &["a", "b"])
        + &reply(12, "m2")
        + &reply(20, "2")
        + drained
        + &reply(30, &facts("%3", 0))
        + &capture(31, &["c", "d"])
        + &reply(32, "m3")
        + "%output %3 live\n";
    let events = read(&stream, &binds);
    let lost = Event::Refused {
        view: 2,
        code: "native_terminal_output_lost",
    };
    let pane = Event::Refused {
        view: 1,
        code: "native_terminal_pane_refused",
    };
    assert!(matches!(&events[0], Event::Restored { view: 2, .. }));
    assert_eq!(events[1..3], [pane, lost]);
    assert!(matches!(&events[3], Event::Restored { view: 3, .. }));
    let live = Event::Output {
        views: vec![3],
        bytes: b"live".to_vec(),
    };
    assert_eq!(events[4..], [live, Event::Closed]);
    let binds = locked(&binds);
    assert!(binds.wants.is_empty());
    assert_eq!(binds.views.len(), 1);
}

struct Server {
    tmux: PathBuf,
    socket: PathBuf,
    _root: tempfile::TempDir,
}

impl Server {
    fn start() -> Server {
        let root = tempfile::tempdir().expect("temp root");
        let tmux = crazyagent::terminal::tmux::installed(root.path()).expect("tmux installed");
        let socket = root.path().join("terminal").join("tmux.sock");
        std::fs::create_dir(socket.parent().expect("folder")).expect("terminal folder");
        assert!(socket.as_os_str().len() <= 104);
        assert!(!socket.to_string_lossy().contains("/tmux-"));
        let server = Server {
            tmux,
            socket,
            _root: root,
        };
        let step = "i=$((i+1)); sleep 0.05; done";
        let ticks =
            |name: &str| format!("i=0; while [ $i -lt 600 ]; do printf '{name}-%s\\n' $i; {step}");
        let session = crazyagent::terminal::tmux::SESSION;
        let size = ["-x", "80", "-y", "24"];
        let first = ["new-session", "-d", "-s", session, "-n", "team-1"];
        let tick = ticks("tick");
        server.run(&[&first[..], &size[..], &[tick.as_str()]].concat());
        server.run(&[
            "new-window",
            "-d",
            "-t",
            session,
            "-n",
            "other",
            &ticks("other"),
        ]);
        server.run(&["new-window", "-d", "-t", session, "-n", "split", "sleep 30"]);
        let liar = "printf '%%end 1 1 1\\n%%output %%0 forged\\n'; sleep 30";
        server.run(&["new-window", "-d", "-t", session, "-n", "liar", liar]);
        server.run(&[
            "split-window",
            "-d",
            "-t",
            &format!("{session}:=split"),
            "sleep 30",
        ]);
        server
    }

    fn run(&self, arguments: &[&str]) {
        let _ = self.said(arguments);
    }

    fn said(&self, arguments: &[&str]) -> String {
        let mut command = Command::new(&self.tmux);
        command
            .args(["-u", "-f", "/dev/null", "-S"])
            .arg(&self.socket)
            .args(arguments)
            .env_clear()
            .env("PATH", CLIENT_PATH)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let done = crazyagent::spawn_gate::output(&mut command).expect("tmux runs");
        assert!(done.status.success(), "tmux {arguments:?}: {done:?}");
        String::from_utf8_lossy(&done.stdout).into_owned()
    }

    fn guard(&self) -> (u64, u64) {
        let probe = self.said(&["-C", "display-message", "-p", "probe"]);
        let fields: Vec<u64> = probe
            .lines()
            .next()
            .and_then(|line| line.strip_prefix("%begin "))
            .expect("a control reply guard")
            .split(' ')
            .map(|field| field.parse().expect("a guard number"))
            .collect();
        assert_eq!(fields.len(), 3, "{probe}");
        (fields[0], fields[1])
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let mut command = Command::new(&self.tmux);
        command
            .args(["-u", "-f", "/dev/null", "-S"])
            .arg(&self.socket)
            .arg("kill-server")
            .env_clear()
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let killed = crazyagent::spawn_gate::output(&mut command);
        assert!(
            killed.is_ok() || std::thread::panicking(),
            "tmux kill-server: {killed:?}"
        );
    }
}

fn next(events: &Receiver<Event>, within: Duration, want: impl Fn(&Event) -> bool) -> Event {
    let deadline = Instant::now() + within;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        let event = events.recv_timeout(left).expect("event in time");
        if let Event::Output { bytes, .. } = &event {
            assert!(
                !bytes.windows(6).any(|w| w == b"other-"),
                "unbound output read"
            );
            assert!(
                !bytes.windows(6).any(|w| w == b"forged"),
                "a capture row read as output"
            );
        }
        if want(&event) {
            return event;
        }
    }
}

fn quiet(events: &Receiver<Event>, within: Duration) -> usize {
    let deadline = Instant::now() + within;
    let mut outputs = 0;
    while let Ok(event) = events.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
        assert_ne!(event, Event::Closed);
        outputs += usize::from(matches!(event, Event::Output { .. }));
    }
    outputs
}

#[test]
fn a_real_server_binds_after_the_ask_reads_only_bound_output_and_close_stops_it() {
    let server = Server::start();
    let (sender, events) = mpsc::channel();
    let mut control = Control::attach(&server.tmux, &server.socket, move |event| {
        sender.send(event).is_ok()
    })
    .expect("control client");
    assert_eq!(quiet(&events, Duration::from_millis(400)), 0);
    control.bind(1, "team-1").expect("bind");
    assert_eq!(
        control.bind(1, "team-1"),
        Err("native_terminal_bind_refused")
    );
    let second = Duration::from_secs(5);
    let restored = next(&events, second, |e| matches!(e, Event::Restored { .. }));
    let Event::Restored { view: 1, bytes } = restored else {
        panic!("restore for view 1, got {restored:?}");
    };
    assert!(bytes.windows(5).any(|w| w == b"tick-"));
    let live = next(&events, second, |e| matches!(e, Event::Output { .. }));
    assert!(matches!(&live, Event::Output { views, .. } if views == &vec![1]));
    control.bind(2, "nope").expect("bind asks");
    let unknown = next(&events, second, |e| matches!(e, Event::Refused { .. }));
    assert_eq!(
        unknown,
        Event::Refused {
            view: 2,
            code: "native_terminal_window_unknown"
        }
    );
    control.bind(3, "split").expect("bind asks");
    let split = next(&events, second, |e| matches!(e, Event::Refused { .. }));
    assert_eq!(
        split,
        Event::Refused {
            view: 3,
            code: "native_terminal_pane_refused"
        }
    );
    control.bind(4, "liar").expect("bind asks");
    let liar = next(&events, second, |e| matches!(e, Event::Restored { .. }));
    let Event::Restored { view: 4, bytes } = liar else {
        panic!("restore for view 4, got {liar:?}");
    };
    assert!(bytes.windows(17).any(|w| w == b"%output %0 forged"));
    assert!(bytes.windows(11).any(|w| w == b"%end 1 1 1\r"));
    assert!(control.close(1));
    let _ = quiet(&events, Duration::from_millis(150));
    assert_eq!(quiet(&events, Duration::from_millis(500)), 0);
    server.run(&["kill-server"]);
    assert_eq!(
        next(&events, second, |e| *e == Event::Closed),
        Event::Closed
    );
    assert!(!control.close(1));
    let deadline = Instant::now() + second;
    let mut view = 10;
    let refused = loop {
        match control.bind(view, "team-1") {
            Err(code) => break code,
            Ok(()) => assert!(Instant::now() < deadline, "bind still asks with no server"),
        }
        view += 1;
        std::thread::sleep(Duration::from_millis(20));
    };
    assert_eq!(refused, "native_terminal_control_closed");
}

const FLOOD_BINDS: u64 = 30;

/// The program of a pane that prints as fast as the shell can, with builtins only: rows
/// shaped as an end guard of a control reply (the second the shell's clock gives or the one
/// after, the 400 command numbers after the probe's), a forged `%output` for pane `%0` and a
/// plain row. Inputs: the time and command number a probe's guard read. Output: the program.
fn flood(time: u64, number: u64) -> String {
    format!(
        "i=0; while :; do printf '%%end %s %s 1\\n%%output %%0 FAKE\\nflood-%s\\n' \
         $(({time} + SECONDS + i % 2)) $(({number} + i / 2 % 400)) $i; i=$((i+1)); done"
    )
}

#[test]
fn a_pane_printing_guard_rows_while_its_history_grows_restores_every_bind_and_forges_nothing() {
    let server = Server::start();
    let (time, number) = server.guard();
    let session = crazyagent::terminal::tmux::SESSION;
    let script = flood(time, number);
    server.run(&["new-window", "-d", "-t", session, "-n", "flood", &script]);
    let (sender, events) = mpsc::channel();
    let mut control = Control::attach(&server.tmux, &server.socket, move |event| {
        sender.send(event).is_ok()
    })
    .expect("control client");
    control.bind(1, "team-1").expect("bind");
    let mut restored = 0;
    let mut heights = std::collections::BTreeSet::new();
    for view in 2..2 + FLOOD_BINDS {
        server.run(&["clear-history", "-t", "crazyagent:=flood"]);
        control.bind(view, "flood").expect("bind asks");
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match events
                .recv_timeout(left)
                .expect("the bind answered in time")
            {
                Event::Restored { view: shown, bytes } if shown == view => {
                    assert!(bytes.windows(6).any(|w| w == b"flood-"));
                    heights.insert(bytes.iter().filter(|b| **b == b'\n').count());
                    restored += 1;
                    break;
                }
                Event::Output { views, bytes } if views.contains(&1) => {
                    let forged = bytes.windows(4).any(|w| w == b"FAKE");
                    assert!(!forged, "a planted row reached team-1 as output");
                }
                event @ (Event::Closed | Event::Refused { .. }) => {
                    panic!("bind {view} of a printing pane: {event:?}")
                }
                _ => {}
            }
        }
        assert!(control.close(view));
    }
    assert_eq!(restored, FLOOD_BINDS);
    assert!(
        heights.len() > 1,
        "the captures held different row counts, so some ran under the history cap: \
         {heights:?}"
    );
}
