use super::*;
#[cfg(unix)]
use crate::engine::bootstrap::EngineService;
use crate::recovery::RunState;
use rusqlite::params;
use std::collections::BTreeMap;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

const PROJECT: &str = "banner-project";
const STOPPED_MS: i64 = 1_800_000_000_000;

#[cfg(unix)]
const OWNER_WAIT_MS: u64 = 5000;
#[cfg(unix)]
const EXCHANGE_MS: u64 = 1000;
#[cfg(unix)]
const IN_FLIGHT: Duration = Duration::from_millis(300);
#[cfg(unix)]
const PAST_EXCHANGE: Duration = Duration::from_millis(1500);
#[cfg(unix)]
const BARRIER_FREED: Duration = Duration::from_secs(10);
#[cfg(unix)]
const FILL_WRITES_MAX: usize = 4096;

/// A real engine with an owner client and the engine root its action ledger lives in.
#[cfg(unix)]
struct Running {
    root: tempfile::TempDir,
    service: EngineService,
    _stop: tokio::sync::watch::Sender<bool>,
    client: OwnerClient,
    engine_root: PathBuf,
}

#[cfg(unix)]
impl Running {
    async fn start() -> Self {
        Self::start_with(OWNER_WAIT_MS).await
    }

    /// Starts the engine with one owner exchange timeout. Input: the timeout in
    /// milliseconds. Output: the running engine.
    async fn start_with(timeout_ms: u64) -> Self {
        let root = tempfile::Builder::new()
            .prefix("ca-recovery-")
            .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
            .unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let global = root.path().join("global.toml");
        std::fs::write(&global, format!("version = 1\ntimeout_ms = {timeout_ms}\n")).unwrap();
        std::fs::set_permissions(&global, std::fs::Permissions::from_mode(0o600)).unwrap();
        let (stop, stopped) = tokio::sync::watch::channel(false);
        let service = EngineService::start_with_settings_sources(
            root.path(),
            "e.sock",
            &global,
            &root.path().join("project.toml"),
            &stopped,
        )
        .await
        .unwrap();
        let client =
            OwnerClient::discover(root.path(), OwnerClientKind::Tui, Duration::from_secs(20))
                .await
                .unwrap()
                .0;
        let engine_root =
            super::changes::engine_root(&service.owner_runtime().unwrap().authority).unwrap();
        Self {
            root,
            service,
            _stop: stop,
            client,
            engine_root,
        }
    }

    fn runtime(&self) -> &OwnerRuntimeHandle {
        self.service.owner_runtime().unwrap()
    }

    /// The handshake packet the owner client sends. Output: the packet.
    fn packet(&self) -> [u8; 37] {
        let mut packet = [0; 37];
        packet[..4].copy_from_slice(b"CAO1");
        packet[5..].copy_from_slice(&self.client.token);
        packet
    }

    async fn stop(mut self) {
        drop(self.client);
        tokio::time::timeout(Duration::from_secs(10), self.service.shutdown())
            .await
            .unwrap()
            .unwrap();
        drop(self.root);
    }
}

/// Records one agent start whose outcome is unknown, as a notice the owner may dismiss.
/// Inputs: the ledger and the team agent. Output: the notice's id.
#[cfg(unix)]
fn notice(ledger: &rusqlite::Connection, team_agent: u32) -> ledger::ActionId {
    ledger::begin(
        ledger,
        ActionKind::AgentSpawn,
        &format!("team-{team_agent}"),
        &format!("{{\"team_agent\":{team_agent},\"pane\":false,\"unknown\":true}}"),
    )
    .unwrap()
}

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

/// Settles one interrupted run as its resume does: resuming, then resumed. Inputs: the
/// engine database and the run.
fn resume(db: &Connection, run: i64) {
    assert!(recovery::mark(db, run, RunState::Interrupted, RunState::Resuming, None).unwrap());
    assert!(recovery::mark(db, run, RunState::Resuming, RunState::Resumed, None).unwrap());
}

/// Fails one run's resume with the reason the banner shows. Inputs: the engine database and
/// the run.
fn fail(db: &Connection, run: i64) {
    assert!(
        recovery::mark(
            db,
            run,
            RunState::Interrupted,
            RunState::Failed,
            Some("The account is unavailable."),
        )
        .unwrap()
    );
}

/// A failed run that still holds its task waits for the owner's retry however many newer
/// runs resumed after it: more resumed runs than a project keeps neither push it out of the
/// banner nor take its retry away, while an older run whose retried resume failed again and
/// let its task go waits for nothing and is not listed ahead of it.
#[test]
fn an_old_failed_run_that_holds_its_task_stays_in_the_banner_behind_any_number_of_resumed_runs() {
    let (_directory, root) = root();
    let db = ledger::open(&root).unwrap();
    crate::team::initialize(&db).unwrap();
    let project = ProjectId::new(PROJECT).unwrap();
    let gave_up = working(&db, PROJECT, "sub", "Tag the release");
    let given_up = stop_and_scan(&db, &root)[&gave_up];
    let failed = working(&db, PROJECT, "sub", "Write the tests");
    let held = stop_and_scan(&db, &root)[&failed];
    assert!(given_up < held);
    fail(&db, given_up);
    retry(&root, gave_up).unwrap();
    fail(&db, given_up);
    let task: i64 = db
        .query_row(
            "SELECT task FROM recovery_runs WHERE id=?1",
            [given_up],
            |row| row.get(0),
        )
        .unwrap();
    recovery::release(&db, recovery::HeldItem::Task(task)).unwrap();
    fail(&db, held);

    let resumed: Vec<i64> = (0..=recovery::RUNS_PER_PROJECT_MAX)
        .map(|index| {
            let agent = working(&db, PROJECT, "sub", &format!("Fix bug {index}"));
            resume(&db, stop_and_scan(&db, &root)[&agent]);
            agent
        })
        .collect();
    assert!(recovery::scan(&root).unwrap().is_empty());
    let kept: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM recovery_runs WHERE project=?1 AND state='resumed'",
            [PROJECT],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        kept,
        i64::try_from(recovery::RUNS_PER_PROJECT_MAX).unwrap(),
        "the project keeps only its newest resumed runs"
    );

    let listed = recovery::list(&root, &project).unwrap();
    assert_eq!(
        listed.waiting.iter().map(|run| run.id).collect::<Vec<_>>(),
        [held]
    );
    assert_eq!(listed.more, 0);
    assert!(listed.waiting[0].last_turn.task.is_some());
    let banner = banner_of(&root, PROJECT);
    let newest = &resumed[resumed.len() - (BANNER_RUNS_MAX - 1)..];
    assert_eq!(
        states(&banner),
        std::iter::once((
            failed,
            RunShown::Failed,
            Some("The account is unavailable.")
        ))
        .chain(newest.iter().map(|&agent| (agent, RunShown::Resumed, None)))
        .collect::<Vec<_>>(),
        "the old failed run leads and the newest resumed runs fill the rest"
    );
    assert_eq!(banner.more, 0);

    retry(&root, failed).unwrap();
    assert_eq!(
        states(&banner_of(&root, PROJECT))[0],
        (failed, RunShown::Interrupted, None),
        "its retry still reaches it"
    );
}

/// More runs wait than one listing reads: the banner shows the oldest and counts every
/// other waiting run, the ones the listing left out included.
#[test]
fn every_waiting_run_past_the_banner_is_counted_even_past_what_one_listing_reads() {
    let (_directory, root) = root();
    let db = ledger::open(&root).unwrap();
    crate::team::initialize(&db).unwrap();
    let project = ProjectId::new(PROJECT).unwrap();
    let extra = 3;
    let waiting = recovery::RUNS_PER_PROJECT_MAX + extra;
    for index in 0..waiting {
        working(&db, PROJECT, "sub", &format!("Task {index}"));
    }
    let runs = stop_and_scan(&db, &root);
    assert_eq!(runs.len(), waiting);
    let mut by_age: Vec<(i64, i64)> = runs.iter().map(|(&agent, &run)| (run, agent)).collect();
    by_age.sort_unstable();

    let listed = recovery::list(&root, &project).unwrap();
    assert_eq!(
        listed.waiting.iter().map(|run| run.id).collect::<Vec<_>>(),
        by_age[..recovery::RUNS_PER_PROJECT_MAX]
            .iter()
            .map(|&(run, _)| run)
            .collect::<Vec<_>>(),
        "a listing reads the oldest waiting runs"
    );
    assert_eq!(listed.more, extra);
    assert!(listed.settled.is_empty());
    let banner = banner_of(&root, PROJECT);
    assert_eq!(
        states(&banner),
        by_age[..BANNER_RUNS_MAX]
            .iter()
            .map(|&(_, agent)| (agent, RunShown::Interrupted, None))
            .collect::<Vec<_>>()
    );
    assert_eq!(
        banner.more,
        u32::try_from(waiting - BANNER_RUNS_MAX).unwrap()
    );
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

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_dismissal_holds_the_owner_barrier_until_it_commits_and_a_revoked_client_dismisses_none()
{
    let running = Running::start().await;
    let held = ledger::open(&running.engine_root).unwrap();
    let first = notice(&held, 4);
    let second = notice(&held, 5);
    held.execute_batch("BEGIN IMMEDIATE").unwrap();
    let mut dismissal = Box::pin(
        running
            .client
            .recovery_dismiss(ActionKind::AgentSpawn, first.get()),
    );
    assert!(
        tokio::time::timeout(IN_FLIGHT, &mut dismissal)
            .await
            .is_err(),
        "the dismissal waits behind the held write"
    );
    assert!(
        running.runtime().engine.owner_egress.try_write().is_err(),
        "the dismissal holds the owner barrier while it waits to commit"
    );
    held.execute_batch("ROLLBACK").unwrap();
    dismissal
        .await
        .unwrap_or_else(|error| panic!("the held dismissal answered {}", error.0));

    let runtime = running.runtime();
    let revoked = runtime
        .control(OwnerControl {
            operation_id: RequestId::new("revoke-before-recovery").unwrap(),
            target: OwnerClientKind::Tui,
            expected_epoch: runtime.epoch(OwnerClientKind::Tui).unwrap(),
            decision: OwnerDecision::Revoke,
        })
        .await
        .unwrap();
    assert!(!revoked.active);
    assert!(
        running
            .client
            .recovery_dismiss(ActionKind::AgentSpawn, second.get())
            .await
            .is_err(),
        "a revoked client dismisses nothing"
    );
    let left = ledger::pending(&held, ActionKind::AgentSpawn).unwrap();
    assert_eq!(
        left.iter().map(|action| action.id).collect::<Vec<_>>(),
        vec![second]
    );
    drop(held);
    running.stop().await;
}

/// The owner's half of a connected socket pair, with the client's half kept open.
/// Output: the server stream and the client stream.
#[cfg(unix)]
fn connection() -> (UnixStream, UnixStream) {
    let (server, client) = crate::spawn_gate::unix_pair_std().unwrap();
    for end in [&server, &client] {
        end.set_nonblocking(true).unwrap();
    }
    (
        UnixStream::from_std(server).unwrap(),
        UnixStream::from_std(client).unwrap(),
    )
}

#[cfg(unix)]
fn dismissal_body(id: ledger::ActionId) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "recovery": {"kind": "dismiss", "action_kind": "agent_spawn", "id": id.get()}
    }))
    .unwrap()
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_dismissal_slower_than_the_owner_exchange_deadline_still_answers_with_its_outcome() {
    let running = Running::start_with(EXCHANGE_MS).await;
    let held = ledger::open(&running.engine_root).unwrap();
    let shown = notice(&held, 4);
    held.execute_batch("BEGIN IMMEDIATE").unwrap();
    let mut dismissal = Box::pin(
        running
            .client
            .recovery_dismiss(ActionKind::AgentSpawn, shown.get()),
    );
    assert!(PAST_EXCHANGE > Duration::from_millis(EXCHANGE_MS));
    assert!(
        tokio::time::timeout(PAST_EXCHANGE, &mut dismissal)
            .await
            .is_err(),
        "the dismissal still waits past the owner exchange deadline"
    );
    held.execute_batch("ROLLBACK").unwrap();
    dismissal
        .await
        .unwrap_or_else(|error| panic!("the slow dismissal answered {}", error.0));
    assert!(
        ledger::pending(&held, ActionKind::AgentSpawn)
            .unwrap()
            .is_empty()
    );
    drop(held);
    running.stop().await;
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_dismissal_cut_short_keeps_the_owner_barrier_until_it_commits_so_a_revocation_returns_after_it()
 {
    let running = Running::start().await;
    let held = ledger::open(&running.engine_root).unwrap();
    let shown = notice(&held, 4);
    let runtime = running.runtime();
    let packet = running.packet();
    let admission = runtime.authority.authenticate(&packet).unwrap();
    let (mut server, _client) = connection();
    let body = dismissal_body(shown);
    held.execute_batch("BEGIN IMMEDIATE").unwrap();
    let served = serve(
        &mut server,
        &runtime.engine,
        &runtime.authority,
        &packet,
        &admission,
        &body,
    );
    assert!(
        tokio::time::timeout(IN_FLIGHT, served).await.is_err(),
        "the dismissal is cut short while it waits behind the held write"
    );
    assert!(
        runtime.engine.owner_egress.try_write().is_err(),
        "the dismissal still owns the owner barrier after its request is cut"
    );
    let mut revoke = Box::pin(runtime.control(OwnerControl {
        operation_id: RequestId::new("revoke-during-cut-recovery").unwrap(),
        target: OwnerClientKind::Tui,
        expected_epoch: runtime.epoch(OwnerClientKind::Tui).unwrap(),
        decision: OwnerDecision::Revoke,
    }));
    assert!(
        tokio::time::timeout(IN_FLIGHT, &mut revoke).await.is_err(),
        "the revocation waits for the dismissal it was ordered after"
    );
    held.execute_batch("ROLLBACK").unwrap();
    let revoked = tokio::time::timeout(BARRIER_FREED, revoke)
        .await
        .expect("the revocation runs once the dismissal commits")
        .unwrap();
    assert!(!revoked.active);
    assert!(
        ledger::pending(&held, ActionKind::AgentSpawn)
            .unwrap()
            .is_empty(),
        "the dismissal committed before the revocation returned"
    );
    assert!(runtime.engine.owner_egress.try_write().is_ok());
    drop(server);
    drop(held);
    running.stop().await;
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_request_that_cannot_take_the_owner_barrier_within_the_engine_timeout_is_refused_busy() {
    let running = Running::start_with(EXCHANGE_MS).await;
    let held = ledger::open(&running.engine_root).unwrap();
    let shown = notice(&held, 4);
    let runtime = running.runtime();
    let packet = running.packet();
    let admission = runtime.authority.authenticate(&packet).unwrap();
    let (mut server, mut client) = connection();
    let body = dismissal_body(shown);
    let barrier = runtime.engine.owner_egress.write().await;
    let served = tokio::time::timeout(
        Duration::from_millis(EXCHANGE_MS) + REPLY_WRITE_TIME + IN_FLIGHT,
        serve(
            &mut server,
            &runtime.engine,
            &runtime.authority,
            &packet,
            &admission,
            &body,
        ),
    )
    .await
    .expect("the barrier wait is bounded by the engine timeout");
    drop(barrier);
    served.unwrap();
    let frame = read_frame(&mut client).await.unwrap();
    let reply: Reply = serde_json::from_slice(&frame[4..]).unwrap();
    assert!(
        matches!(&reply, Reply::Rejected { code } if code == "owner_busy"),
        "{reply:?}"
    );
    assert_eq!(
        ledger::pending(&held, ActionKind::AgentSpawn)
            .unwrap()
            .iter()
            .map(|action| action.id)
            .collect::<Vec<_>>(),
        vec![shown],
        "a busy refusal dismisses nothing"
    );
    drop(held);
    running.stop().await;
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_client_that_stops_reading_holds_the_owner_barrier_no_longer_than_the_reply_bound() {
    let running = Running::start().await;
    let ledger = ledger::open(&running.engine_root).unwrap();
    let shown = notice(&ledger, 4);
    let runtime = running.runtime();
    let packet = running.packet();
    let admission = runtime.authority.authenticate(&packet).unwrap();
    let (mut server, client) = crate::spawn_gate::unix_pair_std().unwrap();
    for end in [&server, &client] {
        end.set_nonblocking(true).unwrap();
    }
    nix::sys::socket::setsockopt(&server, nix::sys::socket::sockopt::SndBuf, &1024usize).unwrap();
    nix::sys::socket::setsockopt(&client, nix::sys::socket::sockopt::RcvBuf, &1024usize).unwrap();
    let mut writes = 0;
    for chunk in [&[0; 1024][..], &[0; 1][..]] {
        loop {
            match std::io::Write::write(&mut server, chunk) {
                Ok(_) => writes += 1,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(error) => panic!("filling the socket failed: {error}"),
            }
            assert!(writes < FILL_WRITES_MAX, "the socket never filled");
        }
    }
    assert!(writes > 0, "the client's socket took no bytes");
    let mut server = UnixStream::from_std(server).unwrap();
    let _client = UnixStream::from_std(client).unwrap();
    let body = serde_json::to_vec(&serde_json::json!({
        "recovery": {"kind": "dismiss", "action_kind": "agent_spawn", "id": shown.get()}
    }))
    .unwrap();

    let served = tokio::time::timeout(
        REPLY_WRITE_TIME + Duration::from_secs(5),
        serve(
            &mut server,
            &runtime.engine,
            &runtime.authority,
            &packet,
            &admission,
            &body,
        ),
    )
    .await
    .expect("a client that stops reading is dropped once the reply bound passes");
    assert_eq!(served.unwrap_err().0, "recovery_outcome_unknown");
    assert!(
        runtime.engine.owner_egress.try_write().is_ok(),
        "the owner barrier is free once the stalled reply is dropped"
    );
    assert!(
        ledger::pending(&ledger, ActionKind::AgentSpawn)
            .unwrap()
            .is_empty(),
        "the dismissal committed before its reply was written"
    );
    drop(server);
    drop(ledger);
    running.stop().await;
}
