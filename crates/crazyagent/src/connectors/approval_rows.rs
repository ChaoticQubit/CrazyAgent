use super::*;
use crate::connectors::approvals::{self, State};
use crate::connectors::gateway::{GatewayLaunch, end, prepare};
use crate::connectors::grants::Role;
use crate::connectors::settings::Wait;
use std::io::Write;
use std::sync::{Arc, Mutex};

const OTHER: &str = "beta";

fn call(id: u64, name: &str, arguments: Value) -> Value {
    request(
        id,
        "tools/call",
        json!({"name": name, "arguments": arguments}),
    )
}

/// A bench whose `board` lists the read `list_issues` and the write `create_issue`, with
/// one launch of it. Output: the stand-in vendor, the bench and the launch.
fn write_bench() -> (Vendor, Bench, GatewayLaunch) {
    let vendor = serving(vec![read_tool("list_issues"), write_tool("create_issue")]);
    let (bench, launch) = board_bench(&vendor);
    (vendor, bench, launch)
}

/// Saves the approval wait given and keeps every other connector setting as it was.
fn set_wait(bench: &Bench, wait: Wait) {
    let db = &bench.case.db;
    let (saved, version) = settings::load(db).unwrap();
    let changed = ConnectorSettings { wait, ..saved };
    settings::save(db, &changed, version).unwrap().unwrap();
}

/// The owner answers one waiting write as the owner's page does: under the write setting
/// saved now, at the time now.
fn owner_answers(bench: &Bench, id: i64, answer: approvals::Answer) {
    let (saved, _) = settings::load(&bench.case.db).unwrap();
    let now = crate::semantic::now_ms();
    approvals::answer(&bench.case.db, id, answer, saved.write, now).unwrap();
}

/// The writes waiting for the owner now, by approval id.
fn waiting_ids(bench: &Bench) -> Vec<i64> {
    let now = crate::semantic::now_ms();
    approvals::waiting(&bench.case.db, approvals::WAITING_MAX, now)
        .unwrap()
        .unwrap_or_default()
        .into_iter()
        .map(|found| found.id)
        .collect()
}

/// One approval as the owner's page reads it now. Inputs: the database and the id. Output:
/// the approval.
fn viewed(db: &rusqlite::Connection, id: i64) -> approvals::ApprovalView {
    approvals::get(db, id, crate::semantic::now_ms())
        .unwrap()
        .unwrap()
}

/// How many write calls the activity log holds with the outcome given, sent or not.
fn writes(bench: &Bench, outcome: &str, sent: bool) -> i64 {
    bench
        .case
        .db
        .query_row(
            "SELECT COUNT(*) FROM connector_calls WHERE kind='write' AND outcome=?1 AND sent=?2",
            rusqlite::params![outcome, sent],
            |row| row.get(0),
        )
        .unwrap()
}

/// Whether the vendor got a call to the tool given.
fn sent_to(vendor: &Vendor, tool: &str) -> bool {
    vendor_calls(vendor).iter().any(|seen| seen.tool() == tool)
}

fn by_id(replies: &[Value], id: u64) -> &Value {
    replies
        .iter()
        .find(|reply| reply["id"] == json!(id))
        .unwrap_or_else(|| panic!("no reply to {id}: {replies:?}"))
}

/// A connector server's output, kept where the row reads it while the server runs.
#[derive(Clone, Default)]
struct Collected(Arc<Mutex<Vec<u8>>>);

impl Write for Collected {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// A connector server on its own thread whose input stays open, as a live harness keeps
/// it, until the row closes it.
struct Open {
    input: Option<std::io::PipeWriter>,
    output: Collected,
    server: std::thread::JoinHandle<()>,
}

impl Open {
    /// Starts the server on the launch with the read wait given. Output: the server.
    fn start(launch: &GatewayLaunch, wait: Duration) -> Self {
        let (reader, writer) = std::io::pipe().unwrap();
        let output = Collected::default();
        let folder = launch.folder.clone();
        let written = output.clone();
        let server = std::thread::spawn(move || {
            let input = std::io::BufReader::new(reader);
            shim::serve_with(input, written, &folder, wait).unwrap();
        });
        Self {
            input: Some(writer),
            output,
            server,
        }
    }

    fn send(&mut self, requests: &[Value]) {
        let input = self.input.as_mut().expect("the input is open");
        input.write_all(&lines(requests)).unwrap();
    }

    fn replies(&self) -> Vec<Value> {
        parsed(&self.output.0.lock().unwrap())
    }

    /// Closes the input and waits at most `within` for the server to end. Input: the most
    /// to wait. Output: every reply the server wrote.
    fn finish(mut self, within: Duration) -> Vec<Value> {
        self.input = None;
        let deadline = Instant::now() + within;
        while !self.server.is_finished() {
            assert!(
                Instant::now() < deadline,
                "the server kept waiting after its input closed"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        self.server.join().unwrap();
        self.replies()
    }
}

/// Answers as the engine until `found` gives a value, within `WAIT`. Inputs: the bench,
/// what the row waits for and the check. Output: the value.
fn until<T>(bench: &Bench, what: &str, mut found: impl FnMut(&Bench) -> Option<T>) -> T {
    let deadline = Instant::now() + WAIT;
    loop {
        bench.gateway.answer_pending();
        if let Some(value) = found(bench) {
            return value;
        }
        assert!(Instant::now() < deadline, "{what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Runs the connector server over the requests with the read wait given while this thread
/// answers as the engine, and hands each write that starts waiting for the owner to
/// `on_waiting` once, by its approval id. The input closes once every request with an id
/// has its reply. Output: the server's replies.
fn talk_with(
    bench: &Bench,
    launch: &GatewayLaunch,
    requests: &[Value],
    wait: Duration,
    mut on_waiting: impl FnMut(&Bench, i64),
) -> Vec<Value> {
    let asked = requests
        .iter()
        .filter(|request| request.get("id").is_some())
        .count();
    let mut server = Open::start(launch, wait);
    server.send(requests);
    let mut handed = std::collections::BTreeSet::new();
    let deadline = Instant::now() + 4 * WAIT;
    while server.replies().len() < asked {
        assert!(
            Instant::now() < deadline,
            "the server did not answer every call"
        );
        bench.gateway.answer_pending();
        for id in waiting_ids(bench) {
            if handed.insert(id) {
                on_waiting(bench, id);
            }
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    server.finish(WAIT)
}

/// DEC-168 Task 12: under Ask every time a write call waits for the owner, the vendor sees
/// nothing until Allow, and after Allow the call runs once and is logged once as sent.
#[test]
fn a_write_waits_for_the_owner_and_runs_only_after_allow() {
    let (vendor, bench, launch) = write_bench();
    let mut asked = Vec::new();
    let write = call(1, "board__create_issue", json!({"title": "Fix login"}));
    let replies = talk_with(&bench, &launch, &[write], WAIT, |bench, id| {
        assert!(vendor_calls(&vendor).is_empty(), "the vendor saw it first");
        asked.push(viewed(&bench.case.db, id));
        owner_answers(bench, id, approvals::Answer::Allow);
    });
    assert_eq!(asked.len(), 1, "{asked:?}");
    let view = &asked[0];
    assert_eq!(view.state, State::Waiting);
    assert_eq!((&*view.project, &*view.agent), (PROJECT, AGENT));
    let what = (&*view.connector, &*view.title, &*view.tool);
    assert_eq!(what, ("board", "Board", "create_issue"));
    let preview = &view.preview;
    assert!(preview.starts_with("create_issue"), "{preview}");
    assert!(preview.contains("Fix login"), "{preview}");
    let (text, error) = shown(&replies[0]);
    assert!(!error, "{text}");
    assert!(text.ends_with("ran create_issue"), "{text}");
    assert_eq!(vendor_calls(&vendor).len(), 1);
    let state = approvals::state(&bench.case.db, view.id).unwrap();
    assert_eq!(state, Some(State::Allowed));
    assert_eq!(writes(&bench, "ok", true), 1);
}

/// DEC-168 Task 12: a Deny answers the agent with a plain tool error naming the refusal,
/// the vendor never sees the call, and the log keeps it as a write never sent.
#[test]
fn deny_returns_a_plain_tool_error_and_the_vendor_never_sees_the_call() {
    let (vendor, bench, launch) = write_bench();
    let write = call(1, "board__create_issue", json!({"title": "Fix login"}));
    let replies = talk_with(&bench, &launch, &[write], WAIT, |bench, id| {
        owner_answers(bench, id, approvals::Answer::Deny);
    });
    let (text, error) = shown(&replies[0]);
    assert!(error);
    assert!(text.starts_with("connector_write_denied: "), "{text}");
    let plain = crate::connectors::plain_problem("connector_write_denied");
    assert!(text.ends_with(plain.unwrap()), "{text}");
    assert!(vendor_calls(&vendor).is_empty());
    assert_eq!(writes(&bench, "connector_write_denied", false), 1);
}

/// DEC-168 Task 12: under Ask once per project and connector, one Allow lets later writes
/// of that connector in that project run without asking, and a launch of another project
/// still asks.
#[test]
fn ask_once_allows_later_writes_in_that_project_only() {
    let (vendor, bench, launch) = write_bench();
    set_write(&bench, WritePolicy::AskOncePerProject);
    let mut asked = 0;
    let first = call(1, "board__create_issue", json!({"title": "one"}));
    let replies = talk_with(&bench, &launch, &[first], WAIT, |bench, id| {
        asked += 1;
        owner_answers(bench, id, approvals::Answer::Allow);
    });
    assert!(!shown(&replies[0]).1, "{}", replies[0]);
    assert_eq!(asked, 1);
    let later = [
        call(1, "board__create_issue", json!({"title": "two"})),
        call(2, "board__create_issue", json!({"title": "three"})),
    ];
    let replies = talk_with(&bench, &launch, &later, WAIT, |_, id| {
        panic!("approval {id} asked again in the same project");
    });
    assert_eq!(replies.len(), 2);
    for reply in &replies {
        assert!(!shown(reply).1, "{reply}");
    }
    assert_eq!(vendor_calls(&vendor).len(), 3);

    grants::set(
        &bench.case.db,
        &bench.case.root,
        OTHER,
        "board",
        true,
        Who::Both,
        2,
    )
    .unwrap();
    let other = prepare(
        &bench.case.db,
        &bench.case.root,
        OTHER,
        Role::Agent { tools_on: true },
        AGENT,
    )
    .unwrap()
    .unwrap();
    let mut asked_other = Vec::new();
    let write = call(1, "board__create_issue", json!({"title": "four"}));
    let replies = talk_with(&bench, &other, &[write], WAIT, |bench, id| {
        asked_other.push(id);
        owner_answers(bench, id, approvals::Answer::Deny);
    });
    assert_eq!(asked_other.len(), 1);
    assert!(shown(&replies[0]).1, "{}", replies[0]);
    assert_eq!(vendor_calls(&vendor).len(), 3);
    let allowed = approvals::allowed_writes(&bench.case.db, PROJECT).unwrap();
    assert_eq!(allowed.connectors, ["board"]);
    let allowed = approvals::allowed_writes(&bench.case.db, OTHER).unwrap();
    assert!(allowed.connectors.is_empty());
}

/// DEC-168 addendum 14 Q5: under Read-only no write tool is listed and a write call is
/// refused without asking; under Ask every time the write tools are listed as writes and
/// a call to one waits for the owner.
#[test]
fn read_only_lists_no_write_tool_and_ask_lists_them_and_holds_the_call() {
    let (vendor, bench, launch) = write_bench();
    set_write(&bench, WritePolicy::ReadOnly);
    assert_eq!(listed(&bench, &launch), ["board__list_issues"]);
    let (text, error) = called(&bench, &launch, "board__create_issue", json!({}));
    assert!(error);
    assert!(text.starts_with("connector_writes_off"), "{text}");
    assert!(waiting_ids(&bench).is_empty());

    set_write(&bench, WritePolicy::AskEveryTime);
    let tools = tools_of(&bench, &launch);
    let mut names: Vec<String> = tools
        .iter()
        .map(|entry| entry["name"].as_str().unwrap().to_owned())
        .collect();
    names.sort();
    assert_eq!(names, ["board__create_issue", "board__list_issues"]);
    for entry in &tools {
        let read = entry["name"] == "board__list_issues";
        let hint = &entry["annotations"]["readOnlyHint"];
        assert_eq!(hint, &json!(read), "{entry}");
    }
    let mut asked = Vec::new();
    let write = call(1, "board__create_issue", json!({}));
    let replies = talk_with(&bench, &launch, &[write], WAIT, |bench, id| {
        assert!(vendor_calls(&vendor).is_empty(), "the vendor saw it first");
        asked.push(id);
        owner_answers(bench, id, approvals::Answer::Deny);
    });
    assert_eq!(asked.len(), 1);
    let (text, error) = shown(&replies[0]);
    assert!(error);
    assert!(text.starts_with("connector_write_denied"), "{text}");
    assert!(vendor_calls(&vendor).is_empty());
}

/// DEC-168 Task 12: a held write that nobody answers ends at the wait setting and not
/// before, answers the agent that it expired, and never reaches the vendor.
#[test]
fn a_held_write_expires_at_the_wait_setting_and_says_so() {
    let (vendor, bench, launch) = write_bench();
    set_wait(&bench, Wait::Minutes2);
    let write = call(1, "board__create_issue", json!({}));
    let replies = talk_with(&bench, &launch, &[write], WAIT, |bench, id| {
        let db = &bench.case.db;
        let view = viewed(db, id);
        assert_eq!(view.expires_ms - view.created_ms, 2 * 60 * 1000);
        assert_eq!(bench.gateway.settle_at(db, view.expires_ms - 1), 0);
        assert_eq!(approvals::state(db, id).unwrap(), Some(State::Waiting));
        assert_eq!(bench.gateway.settle_at(db, view.expires_ms), 1);
        assert_eq!(approvals::state(db, id).unwrap(), Some(State::Expired));
    });
    let (text, error) = shown(&replies[0]);
    assert!(error);
    assert!(text.starts_with("connector_write_expired"), "{text}");
    assert!(vendor_calls(&vendor).is_empty());
    assert_eq!(writes(&bench, "connector_write_expired", false), 1);
}

/// DEC-168 r2 row 13: while writes are not Read-only, a connector's write tools come from
/// its normal endpoint and an allowed write runs there while reads stay on the read-only
/// endpoint; Read-only sends everything back to the read-only endpoint.
#[test]
fn allowing_a_write_moves_that_connector_to_its_normal_endpoint_and_read_only_moves_it_back() {
    let vendor = Vendor::start(|seen, _, stream| {
        let reply = legacy(seen, None, "2025-06-18").unwrap_or_else(|| match seen.rpc() {
            "tools/list" if seen.path == "/read-only" => {
                result(seen, json!({"tools": [read_tool("list_issues")]}))
            }
            "tools/list" => result(
                seen,
                json!({"tools": [read_tool("list_issues"), write_tool("create_issue")]}),
            ),
            "tools/call" => result(seen, text_result(&format!("ran {}", seen.tool()))),
            other => panic!("{other}"),
        });
        send(stream, &reply);
    });
    let bench = bench();
    let mut board = vendor.endpoint();
    board.read_only = ReadOnly::Url(
        url::Url::parse(&format!("http://127.0.0.1:{}/read-only", vendor.port)).unwrap(),
    );
    bench.gateway.stand_in("board", board);
    grant(&bench, "board", true);
    let launch = launch(&bench);
    let mut names = listed(&bench, &launch);
    names.sort();
    assert_eq!(names, ["board__create_issue", "board__list_issues"]);
    let write = call(1, "board__create_issue", json!({}));
    let replies = talk_with(&bench, &launch, &[write], WAIT, |bench, id| {
        owner_answers(bench, id, approvals::Answer::Allow);
    });
    assert!(!shown(&replies[0]).1, "{}", replies[0]);
    let (text, error) = called(&bench, &launch, "board__list_issues", json!({}));
    assert!(!error, "{text}");
    let sent: Vec<(String, String)> = vendor_calls(&vendor)
        .iter()
        .map(|seen| (seen.tool().to_owned(), seen.path.clone()))
        .collect();
    assert_eq!(
        sent,
        [
            ("create_issue".to_owned(), "/mcp".to_owned()),
            ("list_issues".to_owned(), "/read-only".to_owned()),
        ]
    );

    set_write(&bench, WritePolicy::ReadOnly);
    let before = vendor.seen().len();
    assert_eq!(listed(&bench, &launch), ["board__list_issues"]);
    let (text, error) = called(&bench, &launch, "board__create_issue", json!({}));
    assert!(error);
    assert!(text.starts_with("connector_writes_off"), "{text}");
    let after = vendor.seen();
    assert!(after.len() > before);
    for request in &after[before..] {
        assert_eq!(request.path, "/read-only");
    }
}

/// DEC-168 addendum 14: an Allow runs a held write only after its checks pass again; a
/// grant turned off or writes set to Read-only while it waited refuse it, and the vendor
/// never sees it.
#[test]
fn a_held_write_whose_grant_or_write_setting_changed_is_refused_and_never_sent() {
    let (vendor, bench, launch) = write_bench();
    let write = call(1, "board__create_issue", json!({}));
    let replies = talk_with(&bench, &launch, &[write.clone()], WAIT, |bench, id| {
        grant(bench, "board", false);
        owner_answers(bench, id, approvals::Answer::Allow);
    });
    let (text, error) = shown(&replies[0]);
    assert!(error);
    assert!(text.starts_with("connector_not_granted"), "{text}");

    grant(&bench, "board", true);
    let replies = talk_with(&bench, &launch, &[write], WAIT, |bench, id| {
        set_write(bench, WritePolicy::ReadOnly);
        owner_answers(bench, id, approvals::Answer::Allow);
    });
    let (text, error) = shown(&replies[0]);
    assert!(error);
    assert!(text.starts_with("connector_writes_off"), "{text}");
    assert!(vendor_calls(&vendor).is_empty());
}

/// DEC-168 addendum 14 Q1: the connector server answers calls side by side, so a read
/// sent after a held write reaches the vendor and is answered while the write still waits.
#[test]
fn a_held_write_never_holds_up_a_read_through_the_same_server() {
    let (vendor, bench, launch) = write_bench();
    let requests = [
        call(1, "board__create_issue", json!({})),
        call(2, "board__list_issues", json!({})),
    ];
    let replies = talk_with(&bench, &launch, &requests, WAIT, |bench, id| {
        let deadline = Instant::now() + WAIT;
        while !sent_to(&vendor, "list_issues") {
            assert!(Instant::now() < deadline, "the read waited for the write");
            bench.gateway.answer_pending();
            std::thread::sleep(Duration::from_millis(5));
        }
        let db = &bench.case.db;
        assert_eq!(approvals::state(db, id).unwrap(), Some(State::Waiting));
        owner_answers(bench, id, approvals::Answer::Deny);
    });
    assert_eq!(replies.len(), 2);
    let (text, error) = shown(by_id(&replies, 2));
    assert!(!error, "{text}");
    let (text, error) = shown(by_id(&replies, 1));
    assert!(error);
    assert!(text.starts_with("connector_write_denied"), "{text}");
    assert!(!sent_to(&vendor, "create_issue"));
}

/// DEC-168 addendum 14 Q1: while the engine holds a write for the owner, the connector
/// server waits past its read wait, and the owner's later Allow still reaches the agent.
#[test]
fn a_held_write_outlasts_the_read_wait_until_the_owner_answers() {
    let (vendor, bench, launch) = write_bench();
    let wait = Duration::from_millis(300);
    let write = call(1, "board__create_issue", json!({}));
    let replies = talk_with(&bench, &launch, &[write], wait, |bench, id| {
        std::thread::sleep(3 * wait);
        owner_answers(bench, id, approvals::Answer::Allow);
    });
    let (text, error) = shown(&replies[0]);
    assert!(!error, "{text}");
    assert!(text.ends_with("ran create_issue"), "{text}");
    assert_eq!(vendor_calls(&vendor).len(), 1);
}

/// DEC-168 addendum 14: a launch that ends while its write waits expires that approval
/// and leaves no notice waiting; the server gives up at its read wait and the vendor never
/// sees the call.
#[test]
fn an_ended_launch_expires_its_held_writes() {
    let (vendor, mut bench, launch) = write_bench();
    let mut server = Open::start(&launch, Duration::from_secs(2));
    server.send(&[call(1, "board__create_issue", json!({}))]);
    let id = until(&bench, "the write never waited", |bench| {
        waiting_ids(bench).first().copied()
    });
    let name = launch
        .folder
        .file_name()
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned();
    end(&bench.case.db, &bench.case.root, &name).unwrap();
    bench.gateway.answer_pending();
    let db = &bench.case.db;
    assert_eq!(approvals::state(db, id).unwrap(), Some(State::Expired));
    let deadline = Instant::now() + WAIT;
    while server.replies().is_empty() {
        assert!(Instant::now() < deadline, "the write was never answered");
        std::thread::sleep(Duration::from_millis(5));
    }
    let replies = server.finish(WAIT);
    let (text, error) = shown(&replies[0]);
    assert!(error);
    assert!(text.starts_with("connector_gateway_unavailable"), "{text}");
    assert!(vendor_calls(&vendor).is_empty());
    let left = pending_notices(&mut bench);
    let kinds: Vec<&str> = left.iter().map(|notice| notice.0.name()).collect();
    assert!(!kinds.contains(&"connector_write"), "{kinds:?}");
}

/// Answers as the engine until the approval given has ended as expired.
fn until_dropped(bench: &Bench, id: i64) {
    until(bench, "the held write was never dropped", |bench| {
        let state = approvals::state(&bench.case.db, id).unwrap();
        (state == Some(State::Expired)).then_some(())
    });
}

/// DEC-168 Task 12, leader 20:55: a held write that its client cancels (a timeout or a
/// stop) is dropped at once. Its approval ends, the server sends no reply for it, and an
/// Allow after that never reaches the vendor, so the write never runs after the agent was
/// told it failed.
#[test]
fn a_cancelled_held_write_is_dropped_and_a_later_allow_never_reaches_the_vendor() {
    let (vendor, bench, launch) = write_bench();
    let mut server = Open::start(&launch, WAIT);
    server.send(&[call(7, "board__create_issue", json!({"title": "Fix login"}))]);
    let id = until(&bench, "the write never waited", |bench| {
        waiting_ids(bench).first().copied()
    });
    let cancel = json!({
        "jsonrpc": "2.0",
        "method": "notifications/cancelled",
        "params": {"requestId": 7, "reason": "timed out"},
    });
    server.send(&[cancel]);
    until_dropped(&bench, id);
    let db = &bench.case.db;
    let (saved, _) = settings::load(db).unwrap();
    let now = crate::semantic::now_ms();
    let late = approvals::answer(db, id, approvals::Answer::Allow, saved.write, now);
    assert_eq!(late.unwrap_err().0, approvals::CLOSED);
    for _ in 0..3 {
        bench.gateway.answer_pending();
    }
    assert_eq!(server.finish(WAIT), Vec::<Value>::new());
    assert!(!sent_to(&vendor, "create_issue"));
    assert_eq!(writes(&bench, "connector_write_expired", false), 1);
}

/// DEC-168 Task 12, leader 20:55: when the harness closes the server's input (its turn
/// ended), a held write is dropped. The server ends at once instead of waiting for the
/// owner, the approval ends, and the vendor never sees the write.
#[test]
fn a_held_write_is_dropped_when_the_server_input_closes_and_the_server_ends_at_once() {
    let (vendor, bench, launch) = write_bench();
    let mut server = Open::start(&launch, Duration::from_millis(300));
    server.send(&[call(3, "board__create_issue", json!({}))]);
    let id = until(&bench, "the write never waited", |bench| {
        waiting_ids(bench).first().copied()
    });
    let replies = server.finish(Duration::from_secs(5));
    assert_eq!(replies, Vec::<Value>::new());
    until_dropped(&bench, id);
    for _ in 0..3 {
        bench.gateway.answer_pending();
    }
    assert!(!sent_to(&vendor, "create_issue"));
}

/// Hands the engine one write call as a connector server does, holding the lock on the
/// call's alive mark, as a server that is still waiting holds it. Inputs: the launch's
/// mailbox and the call id. Output: the alive mark, whose lock goes when it is dropped.
fn handed_write(mailbox: &crate::team::tool::Mailbox, id: &str) -> std::fs::File {
    let alive = mailbox
        .create(&format!("{id}{}", shim::ALIVE_SUFFIX))
        .unwrap();
    alive.lock().unwrap();
    let ask = json!({"op": "call", "tool": "board__create_issue", "arguments": {}});
    let part = format!("{id}{}", shim::PART_SUFFIX);
    let mut file = mailbox.create(&part).unwrap();
    file.write_all(&lines(&[ask])).unwrap();
    mailbox
        .rename(&part, &format!("{id}{}", shim::CALL_SUFFIX))
        .unwrap();
    alive
}

/// DEC-168 Task 12, leader 20:55: a held write whose server died without a word (its lock
/// on the call's alive mark gone) is dropped while it waits, and also when the owner's Allow
/// came first, so the engine never runs a write nobody waits for.
#[test]
fn a_held_write_whose_server_died_is_dropped_even_after_an_allow() {
    let (vendor, bench, launch) = write_bench();
    let mailbox = crate::team::tool::Mailbox::open(&launch.folder.join(shim::MAILBOX)).unwrap();
    let first = crate::team::tool::call_id().unwrap();
    let alive = handed_write(&mailbox, &first);
    let id = until(&bench, "the write never waited", |bench| {
        waiting_ids(bench).first().copied()
    });
    drop(alive);
    until_dropped(&bench, id);

    let second = crate::team::tool::call_id().unwrap();
    let alive = handed_write(&mailbox, &second);
    let later = until(&bench, "the second write never waited", |bench| {
        waiting_ids(bench).into_iter().find(|found| *found != id)
    });
    owner_answers(&bench, later, approvals::Answer::Allow);
    drop(alive);
    for _ in 0..5 {
        bench.gateway.answer_pending();
    }
    assert!(!sent_to(&vendor, "create_issue"));
    assert_eq!(writes(&bench, "connector_write_expired", false), 2);
    let reply = format!("{second}{}", shim::REPLY_SUFFIX);
    assert!(
        mailbox.read(&reply).is_err(),
        "no answer for a server that is gone"
    );
}

/// DEC-168 Task 12, sec r1 F2 and r2 F2: the owner's approval shows every argument the held
/// write will send, a long text and a nested object whole, a tag-block payload as escapes in
/// a view marked as holding hidden characters, a secret form withheld, and the vendor gets
/// those arguments as sent after the Allow.
#[test]
fn the_owner_sees_every_argument_a_held_write_sends_with_secret_forms_withheld() {
    let (vendor, bench, launch) = write_bench();
    let long = "Notes for the team ".repeat(20);
    let fields = json!({
        "title": "Weekly summary",
        "description": long,
        "sign": "ok\u{e0041}\u{e0042}",
        "extra": {"note": format!("key {KEY}"), "items": [1, 2, 3]},
    });
    let write = call(1, "board__create_issue", fields);
    let mut seen = None;
    let replies = talk_with(&bench, &launch, &[write], WAIT, |bench, id| {
        let view = viewed(&bench.case.db, id);
        assert!(view.hidden, "the view is marked");
        seen = Some((id, view.arguments));
        owner_answers(bench, id, approvals::Answer::Allow);
    });
    let (id, text) = seen.expect("the write waited for the owner");
    let calls = vendor_calls(&vendor);
    let sent = &calls[0].body["params"]["arguments"];
    assert_eq!(sent["sign"], json!("ok\u{e0041}\u{e0042}"));
    assert!(!text.is_empty(), "the approval shows the arguments");
    assert!(!text.contains(KEY), "{text}");
    let shown_to_owner: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(shown_to_owner["description"], json!(long));
    assert_eq!(shown_to_owner["extra"]["items"], json!([1, 2, 3]));
    assert_eq!(shown_to_owner["extra"]["note"], json!("[withheld]"));
    let sign = json!("ok\\u{E0041}\\u{E0042}");
    assert_eq!(shown_to_owner["sign"], sign);
    let (_, error) = shown(&replies[0]);
    assert!(!error);
    assert!(sent_to(&vendor, "create_issue"));
    let answered = viewed(&bench.case.db, id);
    assert_eq!(answered.arguments, "");
}

/// DEC-168 Task 12, sec r1 F1: an allowed write that could no longer finish before its
/// server stops waiting (its age plus one connector wait and one reply wait reaching
/// `HELD_WAIT`) is answered as expired and never reaches the vendor.
#[test]
fn an_allowed_write_too_late_to_finish_before_its_server_gives_up_is_never_sent() {
    let (vendor, bench, launch) = write_bench();
    let write = call(1, "board__create_issue", json!({"title": "Fix login"}));
    let replies = talk_with(&bench, &launch, &[write], WAIT, |bench, id| {
        owner_answers(bench, id, approvals::Answer::Allow);
        let db = &bench.case.db;
        let view = viewed(db, id);
        let room = shim::HELD_WAIT - ALLOW_BUDGET;
        let late = view.created_ms + i64::try_from(room.as_millis()).unwrap();
        assert_eq!(bench.gateway.settle_at(db, late), 1);
    });
    let (text, error) = shown(&replies[0]);
    assert!(error, "{text}");
    assert!(text.starts_with("connector_write_expired"), "{text}");
    assert!(!sent_to(&vendor, "create_issue"));
    assert_eq!(writes(&bench, "connector_write_expired", false), 1);
}

/// The calls of the launch given that the engine marks as held for the owner, by call id.
fn held_calls(launch: &GatewayLaunch) -> Vec<String> {
    let mailbox = crate::team::tool::Mailbox::open(&launch.folder.join(shim::MAILBOX)).unwrap();
    let held = mailbox.entries(shim::HELD_SUFFIX);
    held.into_iter().map(|(_, id)| id).collect()
}

/// Makes the launch read on this connection fail, or read again, with a temporary table of
/// the same name that hides the real one, while every approval still reads and every launch
/// record stays. Inputs: the database and whether the read fails.
fn launch_read_fails(db: &rusqlite::Connection, fails: bool) {
    let sql = if fails {
        "CREATE TEMP TABLE connector_launches (unread INTEGER)"
    } else {
        "DROP TABLE temp.connector_launches"
    };
    db.execute_batch(sql).unwrap();
}

/// DEC-168 Task 12, code r1 CT12-1: a settle whose launch read fails settles nothing, so a
/// held write stays waiting, parked and marked held. Once the read works again, an Allow
/// runs it once and its server gets the answer.
#[test]
fn a_failed_launch_read_settles_nothing_and_the_held_write_runs_after_a_later_allow() {
    let (vendor, bench, launch) = write_bench();
    let mut server = Open::start(&launch, WAIT);
    server.send(&[call(1, "board__create_issue", json!({"title": "Fix login"}))]);
    let id = until(&bench, "the write never waited", |bench| {
        waiting_ids(bench).first().copied()
    });
    let db = &bench.case.db;
    let held = held_calls(&launch);
    assert_eq!(held.len(), 1, "{held:?}");
    launch_read_fails(db, true);
    assert_eq!(bench.gateway.settle_at(db, crate::semantic::now_ms()), 0);
    assert_eq!(approvals::state(db, id).unwrap(), Some(State::Waiting));
    assert_eq!(locked(&bench.gateway.parked).len(), 1);
    assert_eq!(held_calls(&launch), held);
    launch_read_fails(db, false);
    owner_answers(&bench, id, approvals::Answer::Allow);
    assert_eq!(bench.gateway.settle_at(db, crate::semantic::now_ms()), 1);
    let deadline = Instant::now() + WAIT;
    while server.replies().is_empty() {
        assert!(
            Instant::now() < deadline,
            "the allowed write was never answered"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    let replies = server.finish(WAIT);
    let (text, error) = shown(&replies[0]);
    assert!(!error, "{text}");
    assert!(text.ends_with("ran create_issue"), "{text}");
    assert_eq!(vendor_calls(&vendor).len(), 1);
    assert_eq!(writes(&bench, "ok", true), 1);
}

/// A bench like [`write_bench`] whose vendor takes `slow` to answer each tool call.
/// Input: how long a call takes. Output: the stand-in vendor, the bench and the launch.
fn slow_write_bench(slow: Duration) -> (Vendor, Bench, GatewayLaunch) {
    let tools = vec![read_tool("list_issues"), write_tool("create_issue")];
    let vendor = Vendor::start(move |seen, _, stream| {
        let reply = legacy(seen, None, "2025-06-18").unwrap_or_else(|| match seen.rpc() {
            "tools/list" => result(seen, json!({"tools": tools})),
            "tools/call" => {
                std::thread::sleep(slow);
                result(seen, text_result(&format!("ran {}", seen.tool())))
            }
            other => panic!("{other}"),
        });
        send(stream, &reply);
    });
    let (bench, launch) = board_bench(&vendor);
    (vendor, bench, launch)
}

/// DEC-168 Task 12, sec r2 F1: the late check counts the time a settle pass has already
/// spent. When the first allowed write runs long, a second write allowed in the same pass
/// that could no longer finish before its server gives up is answered expired, never sent.
#[test]
fn a_write_allowed_behind_a_slow_one_in_the_same_settle_is_never_sent_too_late() {
    let slow = Duration::from_secs(2);
    let (vendor, bench, launch) = slow_write_bench(slow);
    let mut server = Open::start(&launch, WAIT);
    server.send(&[
        call(1, "board__create_issue", json!({"title": "First"})),
        call(2, "board__create_issue", json!({"title": "Second"})),
    ]);
    let ids = until(&bench, "both writes never waited", |bench| {
        let ids = waiting_ids(bench);
        (ids.len() == 2).then_some(ids)
    });
    for id in &ids {
        owner_answers(&bench, *id, approvals::Answer::Allow);
    }
    let db = &bench.case.db;
    let first = viewed(db, ids[0]);
    let room = i64::try_from((shim::HELD_WAIT - ALLOW_BUDGET).as_millis()).unwrap();
    let margin = i64::try_from((slow / 2).as_millis()).unwrap();
    let now = first.created_ms + room - margin;
    assert_eq!(bench.gateway.settle_at(db, now), 2);
    let deadline = Instant::now() + WAIT;
    while server.replies().len() < 2 {
        assert!(Instant::now() < deadline, "a write was never answered");
        std::thread::sleep(Duration::from_millis(5));
    }
    let replies = server.finish(WAIT);
    let texts: Vec<String> = [1, 2].map(|id| shown(by_id(&replies, id)).0).to_vec();
    let expired = texts
        .iter()
        .filter(|text| text.starts_with("connector_write_expired"))
        .count();
    let ran = texts
        .iter()
        .filter(|text| text.ends_with("ran create_issue"))
        .count();
    assert_eq!((expired, ran), (1, 1), "{texts:?}");
    assert_eq!(vendor_calls(&vendor).len(), 1);
    assert_eq!(writes(&bench, "ok", true), 1);
    assert_eq!(writes(&bench, "connector_write_expired", false), 1);
}
