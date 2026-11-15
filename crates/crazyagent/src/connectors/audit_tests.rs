use super::*;
use crate::connectors::settings::{self, ConnectorSettings};
use crate::connectors::tests::case;

const DAY: i64 = 24 * 60 * 60 * 1000;
const AGENT: &str = "Agent 1";

fn call(connector: &str, project: &str, launch: &str, started_ms: i64) -> CallRow {
    CallRow {
        launch: launch.into(),
        connector: connector.into(),
        tool: "list_issues".into(),
        kind: CallKind::Read,
        sent: true,
        project: project.into(),
        agent: AGENT.into(),
        started_ms,
        ms: 340,
        bytes_in: 120,
        bytes_out: 2048,
        outcome: "ok".into(),
    }
}

fn refused(launch: &str, started_ms: i64) -> CallRow {
    CallRow {
        tool: String::new(),
        kind: CallKind::Unknown,
        sent: false,
        outcome: "connector_tool_unknown".into(),
        ms: 0,
        bytes_out: 0,
        ..call("board", "alpha", launch, started_ms)
    }
}

fn name_project(db: &Connection, id: &str, name: &str) {
    let changed = db
        .execute(
            "UPDATE projects SET body=json_set(body, '$.spec.name', ?2) WHERE id=?1",
            params![id, name],
        )
        .unwrap();
    assert_eq!(changed, 1);
}

fn keep(db: &Connection, keep_log: KeepLog) {
    let (stored, version) = settings::load(db).unwrap();
    let saved = settings::save(db, &ConnectorSettings { keep_log, ..stored }, version).unwrap();
    assert!(saved.is_ok());
}

fn count(db: &Connection, filter: &str) -> i64 {
    db.query_row(
        &format!("SELECT COUNT(*) FROM connector_calls WHERE {filter}"),
        [],
        |row| row.get(0),
    )
    .unwrap()
}

#[test]
fn the_activity_view_lists_calls_newest_first_with_project_names_and_filters_by_connector_and_project()
 {
    let case = case();
    name_project(&case.db, "alpha", "Plant shop");
    let now = crate::semantic::now_ms();
    for (connector, project, at) in [
        ("board", "alpha", now - 4_000),
        ("desk", "beta", now - 3_000),
        ("board", "beta", now - 2_000),
        ("board", "gone", now - 1_000),
    ] {
        record(
            &case.db,
            &call(connector, project, "l1", at),
            KeepLog::Days90,
        )
        .unwrap();
    }

    let all = view(&case.db, None, None).unwrap();
    let order: Vec<(&str, &str, &str)> = all
        .calls
        .iter()
        .map(|call| {
            (
                call.connector.as_str(),
                call.project.as_str(),
                call.project_name.as_str(),
            )
        })
        .collect();
    assert_eq!(
        order,
        [
            ("board", "gone", "gone"),
            ("board", "beta", "beta"),
            ("desk", "beta", "beta"),
            ("board", "alpha", "Plant shop"),
        ]
    );
    let first = &all.calls[3];
    assert_eq!(
        (
            first.tool.as_str(),
            first.kind,
            first.agent.as_str(),
            first.started_ms,
            first.ms,
            first.bytes_in,
            first.bytes_out,
            first.outcome.as_str(),
            first.outcome_text.as_str(),
        ),
        (
            "list_issues",
            CallKind::Read,
            AGENT,
            now - 4_000,
            340,
            120,
            2048,
            "ok",
            "OK"
        )
    );
    assert!(!all.more);
    assert_eq!(all.connectors, ["board", "desk"]);
    let projects: Vec<(&str, &str)> = all
        .projects
        .iter()
        .map(|choice| (choice.id.as_str(), choice.name.as_str()))
        .collect();
    assert_eq!(
        projects,
        [("beta", "beta"), ("gone", "gone"), ("alpha", "Plant shop")]
    );
    assert_eq!(
        (all.connector.as_deref(), all.project.as_deref()),
        (None, None)
    );
    assert_eq!((all.settings, all.version), (ConnectorSettings::DEFAULT, 0));

    let board = view(&case.db, Some("board"), None).unwrap();
    assert_eq!(board.calls.len(), 3);
    assert!(board.calls.iter().all(|call| call.connector == "board"));
    assert_eq!(
        board.connectors,
        ["board", "desk"],
        "choices ignore the filter"
    );
    assert_eq!(board.connector.as_deref(), Some("board"));

    let one = view(&case.db, Some("board"), Some("beta")).unwrap();
    assert_eq!(one.calls.len(), 1);
    assert_eq!(one.calls[0].started_ms, now - 2_000);
    assert_eq!(one.project.as_deref(), Some("beta"));

    let gone = view(&case.db, None, Some("gone")).unwrap();
    assert_eq!(gone.calls.len(), 1, "a project that is gone still filters");
    assert_eq!(gone.calls[0].project_name, "gone");

    let nobody = view(&case.db, Some("nobody"), None).unwrap();
    assert!(nobody.calls.is_empty() && !nobody.more);
}

#[test]
fn the_activity_view_gives_the_engine_sentence_for_an_outcome_and_cleans_hidden_text() {
    let case = case();
    let now = crate::semantic::now_ms();
    let mut write = call("board", "alpha", "l1", now - 2_000);
    write.tool = "create\u{202e}_issue\u{200b}".into();
    write.kind = CallKind::Write;
    write.sent = false;
    write.agent = "Agent\u{7} 1\n".into();
    write.outcome = "connector_writes_not_yet".into();
    record(&case.db, &write, KeepLog::Days90).unwrap();
    let mut odd = call("board", "alpha", "l1", now - 1_000);
    odd.outcome = "zzz_code_from_a_later_build".into();
    record(&case.db, &odd, KeepLog::Days90).unwrap();

    let shown = view(&case.db, None, None).unwrap();
    assert_eq!(shown.calls.len(), 2);
    assert_eq!(shown.calls[0].outcome_text, "zzz_code_from_a_later_build");
    let write = &shown.calls[1];
    assert_eq!(write.tool, "create_issue");
    assert_eq!(write.agent, "Agent 1");
    assert_eq!(write.kind, CallKind::Write);
    assert_eq!(write.outcome, "connector_writes_not_yet");
    assert_eq!(
        write.outcome_text,
        "This tool can change things at the vendor; writes arrive with the approval step."
    );
}

#[test]
fn the_activity_view_holds_only_calls_inside_the_keep_time() {
    let case = case();
    let now = crate::semantic::now_ms();
    record(
        &case.db,
        &call("board", "alpha", "l1", now - 10 * DAY),
        KeepLog::Days90,
    )
    .unwrap();
    record(
        &case.db,
        &call("board", "alpha", "l1", now - DAY),
        KeepLog::Days90,
    )
    .unwrap();
    assert_eq!(view(&case.db, None, None).unwrap().calls.len(), 2);

    keep(&case.db, KeepLog::Days7);
    let shown = view(&case.db, None, None).unwrap();
    assert_eq!(shown.calls.len(), 1);
    assert_eq!(shown.calls[0].started_ms, now - DAY);
    assert_eq!(shown.settings.keep_log, KeepLog::Days7);
    assert_eq!(shown.version, 1);

    prune(&case.db, KeepLog::Days7).unwrap();
    assert_eq!(
        count(&case.db, "1=1"),
        1,
        "the prune drops what the read hid"
    );
}

#[test]
fn a_full_log_at_the_longest_lengths_still_fits_one_owner_frame() {
    let case = case();
    record(&case.db, &call("board", "alpha", "l1", 1), KeepLog::Days90).unwrap();
    let now = crate::semantic::now_ms();
    case.db
        .execute(
            "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 300)
             INSERT INTO connector_calls
                 (launch, connector, tool, kind, sent, project, agent, started_ms, ms,
                  bytes_in, bytes_out, outcome)
             SELECT replace(hex(zeroblob(32)), '00', 'l'),
                    'c' || replace(hex(zeroblob(36)), '00', 'c') || printf('%03d', i % 40),
                    replace(hex(zeroblob(128)), '00', '\u{1d538}'),
                    'read', 1,
                    printf('%03d', i % 40) || replace(hex(zeroblob(125)), '00', 'p'),
                    replace(hex(zeroblob(128)), '00', '\u{1d538}'),
                    ?1, 1, 1, 1,
                    replace(hex(zeroblob(61)), '00', 'x') || printf('%03d', i % 40)
             FROM n",
            [now],
        )
        .unwrap();
    assert_eq!(count(&case.db, "length(CAST(project AS BLOB)) = 128"), 300);

    let full = view(&case.db, None, None).unwrap();
    assert!(full.more, "more calls matched than the view holds");
    assert!(!full.calls.is_empty() && full.calls.len() <= VIEW_CALLS_MAX);
    assert!(full.connectors.len() <= VIEW_CHOICES_MAX);
    assert!(full.projects.len() <= VIEW_CHOICES_MAX && full.projects.len() < 40);
    let bytes = serde_json::to_vec(&full).unwrap().len();
    assert!(
        bytes + 1024 <= crate::protocol::FRAME_BYTES_MAX,
        "{bytes} bytes leave no room in one frame"
    );
}

#[test]
fn a_project_id_longer_than_a_filter_takes_is_listed_but_never_offered_as_a_choice() {
    let case = case();
    let now = crate::semantic::now_ms();
    let long = "p".repeat(PROJECT_ID_BYTES_MAX + 1);
    record(
        &case.db,
        &call("board", &long, "l1", now - 2_000),
        KeepLog::Days90,
    )
    .unwrap();
    record(
        &case.db,
        &call("board", "alpha", "l1", now - 1_000),
        KeepLog::Days90,
    )
    .unwrap();

    let shown = view(&case.db, None, None).unwrap();
    assert_eq!(shown.calls.len(), 2);
    assert_eq!(shown.calls[1].project, long);
    let ids: Vec<&str> = shown
        .projects
        .iter()
        .map(|choice| choice.id.as_str())
        .collect();
    assert_eq!(ids, ["alpha"]);
}

#[test]
fn one_launchs_flood_of_calls_that_never_reached_a_vendor_leaves_every_sent_call_in_place() {
    let case = case();
    let now = crate::semantic::now_ms();
    record(
        &case.db,
        &call("board", "alpha", "a", now - 5_000),
        KeepLog::Days90,
    )
    .unwrap();
    record(
        &case.db,
        &call("desk", "beta", "b", now - 4_000),
        KeepLog::Days90,
    )
    .unwrap();
    record(&case.db, &refused("b", now - 3_000), KeepLog::Days90).unwrap();
    let mut list = call("board", "alpha", "a", now - 2_000);
    list.kind = CallKind::List;
    list.tool = String::new();
    list.sent = false;
    record(&case.db, &list, KeepLog::Days90).unwrap();

    for at in 0..QUIET_ROWS_PER_LAUNCH_MAX as i64 + 50 {
        record(&case.db, &refused("a", now + at), KeepLog::Days90).unwrap();
    }
    assert_eq!(
        count(&case.db, "launch='a' AND sent=0"),
        QUIET_ROWS_PER_LAUNCH_MAX as i64
    );
    assert_eq!(count(&case.db, "launch='a' AND sent=1"), 1);
    assert_eq!(
        count(&case.db, "launch='b'"),
        2,
        "another launch keeps every row"
    );
    assert_eq!(
        count(&case.db, "launch='a' AND kind='list'"),
        0,
        "the oldest calls that never reached a vendor go first"
    );
    let sent = view(&case.db, None, None).unwrap();
    assert_eq!(sent.calls.len(), VIEW_CALLS_MAX);
    assert!(sent.more);
    let board = recent(&case.db, READ_MAX).unwrap();
    assert!(
        board
            .iter()
            .all(|row| !row.sent || row.launch == "a" || row.launch == "b")
    );
}
