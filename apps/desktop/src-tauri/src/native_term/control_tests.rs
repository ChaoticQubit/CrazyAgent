use super::*;
use std::path::PathBuf;
use std::sync::mpsc::Receiver;
use std::time::Instant;

const FACTS: &str = "%0 1 80 24 0 0 0 0 1 0 0 0 0";

fn asked(views: &[u64]) -> Mutex<Binds> {
    let mut binds = Binds::default();
    for view in views {
        binds.views.insert(*view, Slot::Asked);
        binds.wants.push_back(Want {
            view: *view,
            kind: WantKind::State,
        });
        binds.wants.push_back(Want {
            view: *view,
            kind: WantKind::Capture,
        });
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
    format!(
        "%begin 1 {tag} 1\n{facts}\n%end 1 {tag} 1\n%begin 1 {} 1\nscreen\n%end 1 {} 1\n",
        tag + 1,
        tag + 1
    ) + &format!("%output %9 view-{view}\n")
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
    let state = parse_state(b"%4 1 80 24 3 5 1 1 1 0 0 0 1").expect("state parses");
    assert_eq!(
        (state.pane.as_str(), state.columns, state.lines),
        ("%4", 80, 24)
    );
    assert!(state.alternate && state.app_cursor && state.mouse[3]);
    assert!(parse_state(b"%4 2 80 24 3 5 1 1 1 0 0 0 1").is_none());
    assert!(parse_state(b"4 1 80 24 3 5 1 1 1 0 0 0 1").is_none());
    assert!(parse_state(b"%4 1 1 24 3 5 1 1 1 0 0 0 1").is_none());
    assert!(parse_state(b"%4 1 80 24 3 24 1 1 1 0 0 0 1").is_none());
}

#[test]
fn restore_replays_modes_and_ends_at_the_cursor() {
    let state = parse_state(b"%1 1 10 3 2 1 0 1 0 1 0 0 1").expect("state parses");
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
         %output %0 in-capture\n%begin 1 318 1\nshown\n%end 1 318 1\n\
         %output %0 late\\015\n%output %3 other\n"
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
    let rows = "row\n".repeat(REPLY_LINES_MAX + 1);
    let stream = format!(
        "%begin 1 1 1\n%error 1 1 1\n%begin 1 2 1\n%end 1 2 1\n\
         %begin 1 3 1\n{split}\n%end 1 3 1\n%begin 1 4 1\n%end 1 4 1\n\
         %begin 1 5 1\n{FACTS}\n%end 1 5 1\n%begin 1 6 1\n%error 1 6 1\n\
         %begin 1 7 1\n{FACTS}\n%end 1 7 1\n%begin 1 8 1\n{rows}%end 1 8 1\n"
    );
    let events = read(&stream, &binds);
    let refused = |view, code| Event::Refused { view, code };
    assert_eq!(
        events,
        vec![
            refused(1, "native_terminal_window_unknown"),
            refused(2, "native_terminal_pane_refused"),
            refused(3, "native_terminal_capture_failed"),
            refused(4, "native_terminal_capture_failed"),
            Event::Closed,
        ]
    );
    assert!(locked(&binds).views.is_empty());
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
        let done = Command::new(&self.tmux)
            .args(["-u", "-f", "/dev/null", "-S"])
            .arg(&self.socket)
            .args(arguments)
            .env_clear()
            .env("PATH", CLIENT_PATH)
            .stdin(Stdio::null())
            .output()
            .expect("tmux runs");
        assert!(done.status.success(), "tmux {arguments:?}: {done:?}");
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = Command::new(&self.tmux)
            .args(["-u", "-f", "/dev/null", "-S"])
            .arg(&self.socket)
            .arg("kill-server")
            .env_clear()
            .stdin(Stdio::null())
            .output();
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
    assert!(control.close(1));
    let _ = quiet(&events, Duration::from_millis(150));
    assert_eq!(quiet(&events, Duration::from_millis(500)), 0);
    server.run(&["kill-server"]);
    assert_eq!(
        next(&events, second, |e| *e == Event::Closed),
        Event::Closed
    );
    assert!(!control.close(1));
}
