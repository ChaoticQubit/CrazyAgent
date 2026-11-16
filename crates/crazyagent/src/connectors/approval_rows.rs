use super::*;
use crate::connectors::approvals::{self, State};
use crate::connectors::gateway::{GatewayLaunch, end, prepare};
use crate::connectors::grants::Role;
use crate::connectors::settings::Wait;

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
    approvals::waiting(&bench.case.db, approvals::WAITING_MAX)
        .unwrap()
        .unwrap_or_default()
        .into_iter()
        .map(|found| found.id)
        .collect()
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

/// Runs the connector server over the requests with the read wait given while this thread
/// answers as the engine, and hands each write that starts waiting for the owner to
/// `on_waiting` once, by its approval id. Output: the server's replies.
fn talk_with(
    bench: &Bench,
    launch: &GatewayLaunch,
    requests: &[Value],
    wait: Duration,
    mut on_waiting: impl FnMut(&Bench, i64),
) -> Vec<Value> {
    let input = lines(requests);
    let folder = launch.folder.clone();
    let server = std::thread::spawn(move || {
        let mut output = Vec::new();
        shim::serve_with(std::io::Cursor::new(input), &mut output, &folder, wait).unwrap();
        output
    });
    let mut handed = std::collections::BTreeSet::new();
    let deadline = Instant::now() + 4 * WAIT;
    while !server.is_finished() {
        assert!(Instant::now() < deadline, "the server did not finish");
        bench.gateway.answer_pending();
        for id in waiting_ids(bench) {
            if handed.insert(id) {
                on_waiting(bench, id);
            }
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    parsed(&server.join().unwrap())
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
        asked.push(approvals::get(&bench.case.db, id).unwrap().unwrap());
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
        let view = approvals::get(db, id).unwrap().unwrap();
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
    let input = lines(&[call(1, "board__create_issue", json!({}))]);
    let folder = launch.folder.clone();
    let server = std::thread::spawn(move || {
        let mut output = Vec::new();
        let wait = Duration::from_secs(2);
        shim::serve_with(std::io::Cursor::new(input), &mut output, &folder, wait).unwrap();
        output
    });
    let deadline = Instant::now() + WAIT;
    let id = loop {
        assert!(Instant::now() < deadline, "the write never waited");
        bench.gateway.answer_pending();
        if let Some(&id) = waiting_ids(&bench).first() {
            break id;
        }
        std::thread::sleep(Duration::from_millis(5));
    };
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
    let replies = parsed(&server.join().unwrap());
    let (text, error) = shown(&replies[0]);
    assert!(error);
    assert!(text.starts_with("connector_gateway_unavailable"), "{text}");
    assert!(vendor_calls(&vendor).is_empty());
    let left = pending_notices(&mut bench);
    let kinds: Vec<&str> = left.iter().map(|notice| notice.0.name()).collect();
    assert!(!kinds.contains(&"connector_write"), "{kinds:?}");
}
