use super::*;
use crate::recovery::RunState;
use rusqlite::params;
use std::collections::BTreeMap;

const PROJECT: &str = "banner-project";
const STOPPED_MS: i64 = 1_800_000_000_000;

fn root() -> (tempfile::TempDir, PathBuf) {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    (directory, root)
}

/// Sends one dismiss through this protocol's request parsing and handler, as an owner
/// client's frame body. Inputs: the engine root, the kind and number. Output: Ok once
/// dismissed, or the refusal code.
fn dismiss(root: &Path, kind: ActionKind, id: i64) -> Result<(), EngineError> {
    let body = serde_json::json!({
        "recovery": {"kind": "dismiss", "action_kind": kind, "id": id}
    });
    let bytes = serde_json::to_vec(&body).unwrap();
    assert!(matches!(protocol(&bytes).unwrap(), Protocol::Recovery));
    let reply = run(root, parse(&bytes)?)?;
    assert!(matches!(reply, Reply::Dismissed));
    Ok(())
}

#[test]
fn the_owner_dismisses_a_notice_once_and_never_an_intent_still_checked() {
    let (_directory, root) = root();
    let ledger = ledger::open(&root).unwrap();
    let checked = ledger::begin(
        &ledger,
        ActionKind::AgentSpawn,
        "team-3",
        "{\"team_agent\":3,\"pane\":true}",
    )
    .unwrap();
    let notice = ledger::begin(
        &ledger,
        ActionKind::AgentSpawn,
        "team-4",
        "{\"team_agent\":4,\"pane\":false,\"unknown\":true}",
    )
    .unwrap();
    let accept = ledger::begin(
        &ledger,
        ActionKind::LandingAccept,
        "set-1",
        "{\"project\":\"p\",\"change_set\":\"set-1\",\"landed\":false}",
    )
    .unwrap();

    let refused = [
        dismiss(&root, ActionKind::AgentSpawn, checked.get())
            .unwrap_err()
            .0,
        dismiss(&root, ActionKind::WorktreeCreate, notice.get())
            .unwrap_err()
            .0,
    ];
    assert_eq!(refused, ["ledger_not_notice", "ledger_missing"]);
    dismiss(&root, ActionKind::AgentSpawn, notice.get()).unwrap();
    dismiss(&root, ActionKind::LandingAccept, accept.get()).unwrap();
    let again = dismiss(&root, ActionKind::AgentSpawn, notice.get())
        .unwrap_err()
        .0;
    assert_eq!(again, "ledger_finished");

    let left = ledger::pending(&ledger, ActionKind::AgentSpawn).unwrap();
    assert_eq!(
        left.iter().map(|action| action.id).collect::<Vec<_>>(),
        vec![checked]
    );
    assert!(
        ledger::pending(&ledger, ActionKind::LandingAccept)
            .unwrap()
            .is_empty()
    );
    let outcome: String = ledger
        .query_row(
            "SELECT outcome FROM ledger_actions WHERE id=?1",
            [accept.get()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(outcome, "failed");
    for code in refused.iter().chain([&again]) {
        assert!(RECOVERY_CODES.contains(&code.as_str()), "{code}");
    }
}

/// Sends one Recovery request through this protocol's parsing and handler, as an owner
/// client's frame body. Inputs: the engine root and the request. Output: the reply or the
/// refusal code.
fn ask(root: &Path, request: serde_json::Value) -> Result<Reply, EngineError> {
    let bytes = serde_json::to_vec(&serde_json::json!({ "recovery": request })).unwrap();
    assert!(matches!(protocol(&bytes).unwrap(), Protocol::Recovery));
    run(root, parse(&bytes)?)
}

fn banner_of(root: &Path, project: &str) -> RecoveryBanner {
    match ask(
        root,
        serde_json::json!({"kind": "list", "project": project}),
    )
    .unwrap()
    {
        Reply::Listed { banner } => banner,
        other => panic!("not a banner: {other:?}"),
    }
}

fn retry(root: &Path, agent: i64) -> Result<(), EngineError> {
    match ask(root, serde_json::json!({"kind": "retry", "agent": agent}))? {
        Reply::Retried => Ok(()),
        other => panic!("not a retry: {other:?}"),
    }
}

/// Adds one team agent in a turn, as the team runtime keeps it while the turn runs.
/// Inputs: the engine database, the project, the role and what the turn was asked.
/// Output: the agent id.
fn working(db: &Connection, project: &str, role: &str, asked: &str) -> i64 {
    db.execute(
        "INSERT INTO team_agents(project,role,harness,tools,status,turns,created_ms,heartbeat_ms)
         VALUES (?1,?2,'codex',0,'working',2,1,?3)",
        params![project, role, STOPPED_MS],
    )
    .unwrap();
    let agent = db.last_insert_rowid();
    db.execute(
        "INSERT INTO team_tasks(project,source,harness,tools,prompt,status,agent,created_ms,started_ms)
         VALUES (?1,'master','codex',0,?2,'running',?3,1,1)",
        params![project, asked, agent],
    )
    .unwrap();
    let task = db.last_insert_rowid();
    db.execute(
        "UPDATE team_agents SET task=?2 WHERE id=?1",
        params![agent, task],
    )
    .unwrap();
    agent
}

/// Records an engine stop that cut every working agent short with no live pane left, and
/// the next start's scan of it. Inputs: the engine database and root. Output: each
/// agent's run id.
fn stop_and_scan(db: &Connection, root: &Path) -> BTreeMap<i64, i64> {
    recovery::capture(db, Some(STOPPED_MS)).unwrap();
    db.execute(
        "UPDATE team_agents SET status='stopped' WHERE status='working'",
        [],
    )
    .unwrap();
    recovery::scan(root)
        .unwrap()
        .into_iter()
        .map(|run| (run.agent, run.id))
        .collect()
}

fn states(banner: &RecoveryBanner) -> Vec<(i64, RunShown, Option<&str>)> {
    banner
        .runs
        .iter()
        .map(|run| (run.agent, run.state, run.reason.as_deref()))
        .collect()
}

#[test]
fn the_banner_lists_open_resumed_and_failed_runs_with_notices_and_retry_waits_again() {
    let (_directory, root) = root();
    let db = ledger::open(&root).unwrap();
    crate::team::initialize(&db).unwrap();
    let project = ProjectId::new(PROJECT).unwrap();
    let master = working(&db, PROJECT, "master", "Ship the page");
    let open = working(&db, PROJECT, "sub", "Plan the page\nThen build it");
    let failed = working(&db, PROJECT, "sub", "Write the tests");
    let resumed = working(&db, PROJECT, "sub", "Fix the header");
    let elsewhere = working(&db, "other-project", "sub", "Not this one");
    let persona = names::assign_persona(&db, &project, open).unwrap();
    let runs = stop_and_scan(&db, &root);
    assert_eq!(runs.len(), 5);
    let mark = |agent: i64, from: RunState, to: RunState, reason: Option<&str>| {
        assert!(recovery::mark(&db, runs[&agent], from, to, reason).unwrap());
    };
    mark(master, RunState::Interrupted, RunState::Resuming, None);
    mark(master, RunState::Resuming, RunState::Resumed, None);
    mark(
        failed,
        RunState::Interrupted,
        RunState::Failed,
        Some("The account is unavailable."),
    );
    mark(resumed, RunState::Interrupted, RunState::Resuming, None);
    mark(resumed, RunState::Resuming, RunState::Resumed, None);

    let checked = ledger::begin(
        &db,
        ActionKind::AgentSpawn,
        &format!("team-{failed}"),
        &format!("{{\"team_agent\":{failed},\"pane\":true}}"),
    )
    .unwrap();
    let start = ledger::begin(
        &db,
        ActionKind::AgentSpawn,
        &format!("team-{open}"),
        &format!("{{\"team_agent\":{open},\"pane\":true,\"unknown\":true}}"),
    )
    .unwrap();
    let other_start = ledger::begin(
        &db,
        ActionKind::AgentSpawn,
        &format!("team-{elsewhere}"),
        &format!("{{\"team_agent\":{elsewhere},\"pane\":false,\"unknown\":true}}"),
    )
    .unwrap();
    let landed = ledger::begin(
        &db,
        ActionKind::LandingAccept,
        "set-1",
        &format!("{{\"project\":\"{PROJECT}\",\"change_set\":\"set-1\",\"landed\":true}}"),
    )
    .unwrap();
    let other_set = ledger::begin(
        &db,
        ActionKind::LandingAccept,
        "set-2",
        "{\"project\":\"other-project\",\"change_set\":\"set-2\",\"landed\":false}",
    )
    .unwrap();

    let banner = banner_of(&root, PROJECT);
    assert_eq!(
        states(&banner),
        [
            (open, RunShown::Interrupted, None),
            (
                failed,
                RunShown::Failed,
                Some("The account is unavailable.")
            ),
            (resumed, RunShown::Resumed, None),
        ],
        "the master resumed before the oldest open run, so it is not listed again"
    );
    let first = &banner.runs[0];
    assert_eq!(first.label, persona);
    assert_eq!(first.role, AgentRole::Sub);
    assert_eq!(first.asked.as_deref(), Some("Plan the page"));
    assert_eq!(
        (first.at_ms, first.turn, first.paused),
        (STOPPED_MS, 3, false)
    );
    assert_eq!(banner.runs[1].label, format!("Sub-agent {failed}"));
    assert_eq!(
        banner
            .notices
            .iter()
            .map(|notice| (
                notice.action_kind,
                notice.id,
                notice.agent,
                notice.label.as_deref(),
                notice.change_set.as_deref(),
                notice.shown
            ))
            .collect::<Vec<_>>(),
        [
            (
                ActionKind::AgentSpawn,
                start.get(),
                Some(open),
                Some(persona.as_str()),
                None,
                NoticeShown::Unknown
            ),
            (
                ActionKind::LandingAccept,
                landed.get(),
                None,
                None,
                Some("set-1"),
                NoticeShown::Landed
            ),
        ],
        "a start a check still settles and another project's notices are not listed"
    );
    assert_eq!(banner.more, 0);
    let other = banner_of(&root, "other-project");
    assert_eq!(states(&other), [(elsewhere, RunShown::Interrupted, None)]);
    assert_eq!(
        other.notices.iter().map(|n| n.id).collect::<Vec<_>>(),
        [other_start.get(), other_set.get()]
    );
    assert_eq!(other.notices[1].shown, NoticeShown::Dropped);

    retry(&root, failed).unwrap();
    assert_eq!(
        states(&banner_of(&root, PROJECT))[1],
        (failed, RunShown::Interrupted, None),
        "a retried run waits to resume again without its old reason"
    );
    assert_eq!(retry(&root, failed).unwrap_err().0, "recovery_not_failed");
    assert_eq!(retry(&root, 0).unwrap_err().0, "recovery_request_invalid");

    dismiss(&root, ActionKind::AgentSpawn, start.get()).unwrap();
    dismiss(&root, ActionKind::LandingAccept, landed.get()).unwrap();
    for agent in [open, failed] {
        mark(agent, RunState::Interrupted, RunState::Resuming, None);
        mark(agent, RunState::Resuming, RunState::Resumed, None);
    }
    let cleared = banner_of(&root, PROJECT);
    assert_eq!(
        cleared,
        RecoveryBanner::default(),
        "everything resumed or dismissed leaves no banner"
    );
    assert_eq!(
        ledger::pending(&db, ActionKind::AgentSpawn)
            .unwrap()
            .iter()
            .map(|action| action.id)
            .collect::<Vec<_>>(),
        [checked, other_start]
    );
}

#[test]
fn a_banner_beyond_its_bounds_lists_the_oldest_notices_and_counts_the_rest() {
    let (_directory, root) = root();
    let db = ledger::open(&root).unwrap();
    crate::team::initialize(&db).unwrap();
    let extra = 3;
    let ids: Vec<i64> = (0..BANNER_NOTICES_MAX + extra)
        .map(|index| {
            let set = format!("set-{index}");
            ledger::begin(
                &db,
                ActionKind::LandingAccept,
                &set,
                &serde_json::json!({"project": PROJECT, "change_set": set, "landed": false})
                    .to_string(),
            )
            .unwrap()
            .get()
        })
        .collect();
    let banner = banner_of(&root, PROJECT);
    assert!(banner.runs.is_empty());
    assert_eq!(
        banner.notices.iter().map(|n| n.id).collect::<Vec<_>>(),
        ids[..BANNER_NOTICES_MAX]
    );
    assert_eq!(banner.more, u32::try_from(extra).unwrap());
    let frame = encode_frame(&Reply::Listed { banner }).unwrap();
    assert!(frame.len() <= crate::protocol::FRAME_BYTES_MAX + 4);
}

#[test]
fn malformed_or_oversized_recovery_requests_are_refused_before_the_ledger() {
    for body in [
        r#"{"recovery":{"kind":"dismiss","action_kind":"agent_spawn"}}"#,
        r#"{"recovery":{"kind":"dismiss","action_kind":"spawn","id":1}}"#,
        r#"{"recovery":{"kind":"dismiss","action_kind":"agent_spawn","id":1,"outcome":"done"}}"#,
        r#"{"recovery":{"kind":"retry","id":1}}"#,
        r#"{"recovery":{"kind":"dismiss","action_kind":"agent_spawn","id":"1"}}"#,
        r#"{"recovery":{"kind":"list"}}"#,
        r#"{"recovery":{"kind":"list","project":"../escape"}}"#,
        r#"{"recovery":{"kind":"retry","agent":"3"}}"#,
        r#"{"recovery":{"kind":"retry","agent":3,"project":"p"}}"#,
    ] {
        assert_eq!(
            parse(body.as_bytes()).unwrap_err().0,
            "recovery_request_invalid",
            "{body}"
        );
    }
    let long = format!(
        r#"{{"recovery":{{"kind":"dismiss","action_kind":"agent_spawn","id":1}},"pad":"{}"}}"#,
        "x".repeat(REQUEST_BYTES_MAX)
    );
    assert_eq!(
        parse(long.as_bytes()).unwrap_err().0,
        "recovery_request_bounds"
    );
}

#[test]
fn recovery_codes_cover_every_refusal_of_the_protocol_and_stay_sorted() {
    let source = include_str!("recovery.rs");
    let found: Vec<&str> = ["failure(\"", "code: \""]
        .into_iter()
        .flat_map(|opening| source.split(opening).skip(1))
        .filter_map(|rest| rest.split('"').next())
        .collect();
    assert!(found.len() >= 7, "{found:?}");
    for code in found {
        assert!(
            RECOVERY_CODES.contains(&code),
            "{code} is not in RECOVERY_CODES"
        );
    }
    let mut sorted = RECOVERY_CODES.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(sorted, RECOVERY_CODES);
}
