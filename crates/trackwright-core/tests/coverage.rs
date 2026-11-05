use rusqlite::{OptionalExtension, Transaction};
use serde_json::json;
use std::path::{Path, PathBuf};
use trackwright_core::coverage::{
    self, CoverageCase, CoverageOf, CoverageQuery, CoverageResult, Finished,
};
use trackwright_core::criteria::{self, NewEvidence};
use trackwright_core::events;
use trackwright_core::host::{
    Authority, CheckView, Evidence, EvidenceState, Host, LinkKind, LinkState, Links, Paths,
    TicketView,
};
use trackwright_core::model::{
    Actor, CheckKind, ClosePolicy, CriterionState, EvidenceResult, OpKind, PolicyKey, Priority,
    ProjectKey, Refusal, ShortId, TicketKind, TicketStatus, codes,
};
use trackwright_core::policy;
use trackwright_core::runs::{
    self, NetworkMode, NewResult, PolicyRef, ResultOutcome, RunEnd, RunFinish, RunRecord, RunStart,
    RunState, SourceRef, TestId, Toolchain, ToolchainFamily, Verdict,
};
use trackwright_core::standalone::Standalone;
use trackwright_core::store::{self, NewTicket, Store, StoreError};
use trackwright_core::tests_model::{
    CaseKind, CaseStatus, CriterionRef, PlanStatus, RunWriters, SkippedCounts, TestEvidence,
    codes as test_codes,
};
use trackwright_core::tests_store::{self, NewCase, PlanFields};

/// A host that keeps each ticket's current source tree in its own table in the tracker's
/// database, read in the core's transaction, and locks `run_writers` to `engine_only`. Its
/// `test` checks follow runs through the core; links, every other check kind and the rest
/// of authority follow the standalone host. It opens no file.
struct TreeHost;

const TREE_TABLE: &str = "CREATE TABLE host_trees(ticket TEXT PRIMARY KEY, tree TEXT NOT NULL)";

impl Links for TreeHost {
    fn kinds(&self) -> &[LinkKind] {
        &[]
    }

    fn resolve(
        &self,
        tx: &Transaction<'_>,
        project: &ProjectKey,
        kind: &str,
        target: &str,
    ) -> Result<LinkState, Refusal> {
        Standalone.resolve(tx, project, kind, target)
    }
}

impl Evidence for TreeHost {
    fn check(
        &self,
        tx: &Transaction<'_>,
        project: &ProjectKey,
        check: &CheckView,
    ) -> Result<EvidenceState, Refusal> {
        if check.kind == CheckKind::Test
            && let Some(state) = coverage::test_check(tx, self, project, check)?
        {
            return Ok(state);
        }
        Standalone.check(tx, project, check)
    }

    fn tracks_source(&self) -> bool {
        true
    }

    fn current_source(
        &self,
        tx: &Transaction<'_>,
        _project: &ProjectKey,
        ticket: &TicketView,
    ) -> Result<Option<SourceRef>, Refusal> {
        let tree: Option<String> = tx
            .query_row(
                "SELECT tree FROM host_trees WHERE ticket = ?1",
                [ticket.id.to_string()],
                |row| row.get(0),
            )
            .optional()
            .map_err(|error| Refusal::host("host_failed", error.to_string()))?;
        Ok(tree.map(|tree| source(&tree)))
    }
}

impl Authority for TreeHost {
    fn actor_may(
        &self,
        tx: &Transaction<'_>,
        project: &ProjectKey,
        actor: &Actor,
        op: OpKind,
        ticket: Option<&TicketView>,
    ) -> Result<(), Refusal> {
        Standalone.actor_may(tx, project, actor, op, ticket)
    }

    fn actors_verified(&self) -> bool {
        true
    }

    fn evidence_may(
        &self,
        tx: &Transaction<'_>,
        project: &ProjectKey,
        actor: &Actor,
        check: &CheckView,
    ) -> Result<(), Refusal> {
        Standalone.evidence_may(tx, project, actor, check)
    }

    fn policy_locked(&self, setting: PolicyKey) -> Option<&'static str> {
        (setting == PolicyKey::RunWriters).then_some(RunWriters::EngineOnly.as_str())
    }
}

impl Paths for TreeHost {
    /// Refuses every file: the coverage tests export and import nothing. Inputs: the
    /// transaction, key, actor and path (unused) and the operation. Output:
    /// `export_path_refused` for an export, `import_path_refused` otherwise.
    fn resolve_path(
        &self,
        _: &Transaction<'_>,
        _: &ProjectKey,
        _: &Actor,
        op: OpKind,
        _: &Path,
    ) -> Result<PathBuf, Refusal> {
        let code = match op {
            OpKind::Export => codes::EXPORT_PATH_REFUSED,
            _ => codes::IMPORT_PATH_REFUSED,
        };
        Err(Refusal::new(code, "the coverage test host opens no file"))
    }
}

fn opened(dir: &Path) -> Store {
    Store::open(&dir.join("tracker").join("tracker.db")).unwrap()
}

fn actor(name: &str) -> Actor {
    Actor::new(name).unwrap()
}

fn owner() -> Actor {
    actor("owner")
}

fn engine() -> Actor {
    actor("engine:checks")
}

fn key() -> ProjectKey {
    ProjectKey::new("CA").unwrap()
}

fn project(tx: &Transaction<'_>) {
    store::create_project(tx, &owner(), &key(), "CrazyAgent", 1_000).unwrap();
}

fn refused<T: std::fmt::Debug>(result: Result<T, StoreError>) -> &'static str {
    match result {
        Err(StoreError::Refused(refusal)) => refusal.code,
        other => panic!("expected a refusal, got {other:?}"),
    }
}

fn count(tx: &Transaction<'_>, table: &str) -> i64 {
    tx.query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
        row.get(0)
    })
    .unwrap()
}

fn hex(digit: char, len: usize) -> String {
    std::iter::repeat_n(digit, len).collect()
}

fn source(tree: &str) -> SourceRef {
    SourceRef::Git {
        repository: "crazyagent".to_owned(),
        commit: hex('a', 40),
        tree: tree.to_owned(),
        base: None,
        change_set: None,
    }
}

fn check_policy() -> PolicyRef {
    PolicyRef {
        profile: hex('d', 64),
        profile_approved_ms: 1_000,
        check: "unit".to_owned(),
        kind: CaseKind::Unit,
        argv: vec!["cargo".to_owned(), "test".to_owned()],
        toolchain: Toolchain {
            family: ToolchainFamily::Rust,
            version: "rustc 1.98.1".to_owned(),
        },
        sandbox: hex('e', 64),
        network: NetworkMode::Off,
        timeout_s: 1_800,
        environment: hex('f', 64),
    }
}

fn ticket(tx: &Transaction<'_>, by: &Actor, title: &str) -> ShortId {
    let new = NewTicket {
        title: title.to_owned(),
        body: String::new(),
        kind: TicketKind::Feature,
        status: TicketStatus::Todo,
        priority: Priority::new(2).unwrap(),
    };
    store::create_ticket(tx, by, &key(), &new, 1_100)
        .unwrap()
        .short_id
}

fn case(tx: &Transaction<'_>, title: &str, stable_name: &str) -> TestId {
    let new = NewCase {
        title: title.to_owned(),
        kind: CaseKind::Unit,
        stable_name: Some(stable_name.to_owned()),
        steps: Vec::new(),
        protected: false,
        status: CaseStatus::Ready,
    };
    tests_store::create_case(tx, &owner(), &key(), &new, 1_150)
        .unwrap()
        .short_id
}

fn criterion(tx: &Transaction<'_>, id: &ShortId, text: &str) -> u32 {
    criteria::add_criterion(tx, &Standalone, &owner(), id, text, 1_200)
        .unwrap()
        .position
}

fn add_test_check(
    tx: &Transaction<'_>,
    host: &impl Host,
    id: &ShortId,
    position: u32,
    case: &TestId,
) {
    criteria::add_check(
        tx,
        host,
        &owner(),
        id,
        position,
        CheckKind::Test,
        &case.to_string(),
        1_210,
    )
    .unwrap();
}

fn state(tx: &Transaction<'_>, id: &ShortId, position: u32) -> CriterionState {
    criteria::criteria(tx, id)
        .unwrap()
        .into_iter()
        .find(|found| found.position == position)
        .unwrap()
        .state
}

fn status(tx: &Transaction<'_>, id: &ShortId) -> TicketStatus {
    store::ticket(tx, id).unwrap().status
}

fn view(id: &ShortId, position: u32, case: &TestId) -> CheckView {
    CheckView {
        ticket: id.clone(),
        criterion: position,
        kind: CheckKind::Test,
        reference: case.to_string(),
    }
}

fn evidence_on(tx: &Transaction<'_>, host: &impl Host, check: &CheckView) -> EvidenceState {
    coverage::test_check(tx, host, &key(), check)
        .unwrap()
        .expect("a case id check follows runs")
}

fn set_policy(tx: &Transaction<'_>, host: &impl Host, setting: PolicyKey, value: &str) {
    let version = policy::describe(tx, &key())
        .unwrap()
        .into_iter()
        .find(|entry| entry.key == setting)
        .unwrap()
        .version;
    let set = policy::set(tx, host, &owner(), &key(), setting, value, version, 1_050).unwrap();
    assert_eq!(
        serde_json::to_value(set).unwrap()[setting.as_str()],
        json!(value)
    );
}

fn started(tx: &Transaction<'_>, tree: &str, now_ms: i64) -> runs::RunView {
    let start = RunStart {
        project: key(),
        title: "unit".to_owned(),
        plan: None,
        batch: None,
        source: source(tree),
        policy: check_policy(),
    };
    runs::start(tx, &engine(), &start, now_ms).unwrap()
}

fn recorded(
    tx: &Transaction<'_>,
    run: runs::RunView,
    results: &[(&str, ResultOutcome)],
    now_ms: i64,
) -> runs::RunView {
    if results.is_empty() {
        return run;
    }
    let record = RunRecord {
        id: run.id.clone(),
        version: run.version,
        results: results
            .iter()
            .map(|(name, outcome)| NewResult {
                name: (*name).to_owned(),
                outcome: *outcome,
                duration_ms: Some(5),
                message: None,
                artifact: None,
            })
            .collect(),
    };
    runs::record(tx, &engine(), &record, now_ms).unwrap()
}

fn finished(
    tx: &Transaction<'_>,
    host: &impl Host,
    run: &runs::RunView,
    end: RunEnd,
    exit_code: Option<i32>,
    now_ms: i64,
) -> Finished {
    let finish = RunFinish {
        id: run.id.clone(),
        version: run.version,
        end,
        exit_code,
        reason: (end == RunEnd::ReportUnreadable).then(|| "the report did not parse".to_owned()),
        integrity: None,
    };
    coverage::finish(tx, host, &engine(), &finish, now_ms).unwrap()
}

fn run_on(
    tx: &Transaction<'_>,
    host: &impl Host,
    tree: &str,
    results: &[(&str, ResultOutcome)],
    exit_code: Option<i32>,
    now_ms: i64,
) -> Finished {
    let run = started(tx, tree, now_ms);
    let run = recorded(tx, run, results, now_ms + 1);
    finished(tx, host, &run, RunEnd::Completed, exit_code, now_ms + 2)
}

fn run(
    tx: &Transaction<'_>,
    results: &[(&str, ResultOutcome)],
    exit_code: Option<i32>,
    now_ms: i64,
) -> Finished {
    run_on(tx, &Standalone, &hex('b', 40), results, exit_code, now_ms)
}

fn query(of: CoverageOf, after: Option<CriterionRef>) -> CoverageQuery {
    CoverageQuery {
        project: key(),
        of,
        after,
    }
}

fn release(tx: &Transaction<'_>, name: &str, tickets: &[&ShortId]) {
    tx.execute(
        "INSERT INTO tk_versions(project, name, position, target_day, status, source, version)
         SELECT id, ?1, 0, NULL, 'active', 'test', 1 FROM tk_projects WHERE key = 'CA'",
        [name],
    )
    .unwrap();
    for id in tickets {
        let changed = tx
            .execute(
                "UPDATE tk_tickets SET version_id = (SELECT id FROM tk_versions WHERE name = ?1)
                 WHERE number = ?2",
                (name, id.number()),
            )
            .unwrap();
        assert_eq!(changed, 1);
    }
}

#[test]
fn a_criterion_closes_when_its_linked_tests_last_passed() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = opened(dir.path());
    let tx = store.transaction().unwrap();
    project(&tx);
    let first = case(&tx, "Store runs", "runs::store");
    let second = case(&tx, "Replay runs", "runs::replay");
    let id = ticket(&tx, &owner(), "Runs are stored and replayed");
    let position = criterion(&tx, &id, "Runs are stored and replayed");
    add_test_check(&tx, &Standalone, &id, position, &first);
    add_test_check(&tx, &Standalone, &id, position, &second);
    assert_eq!(
        policy::load(&tx, &key()).unwrap().close,
        ClosePolicy::Evidence
    );
    assert_eq!(state(&tx, &id, position), CriterionState::Open);

    let failing = run(
        &tx,
        &[
            ("runs::store", ResultOutcome::Passed),
            ("runs::replay", ResultOutcome::Failed),
        ],
        Some(101),
        2_000,
    );
    assert_eq!(failing.run.verdict, Some(Verdict::Failed));
    assert_eq!(failing.tickets, std::slice::from_ref(&id));
    assert_eq!(state(&tx, &id, position), CriterionState::Open);
    assert_eq!(status(&tx, &id), TicketStatus::Todo);
    assert!(evidence_on(&tx, &Standalone, &view(&id, position, &first)).satisfied);
    assert!(!evidence_on(&tx, &Standalone, &view(&id, position, &second)).satisfied);

    let passing = run(
        &tx,
        &[
            ("runs::store", ResultOutcome::Passed),
            ("runs::replay", ResultOutcome::Passed),
        ],
        Some(0),
        3_000,
    );
    assert_eq!(passing.run.verdict, Some(Verdict::Passed));
    assert_eq!(passing.tickets, std::slice::from_ref(&id));
    assert_eq!(state(&tx, &id, position), CriterionState::Met);
    let closed = store::ticket(&tx, &id).unwrap();
    assert_eq!(closed.status, TicketStatus::Done);
    assert_eq!(closed.closed_by, Some(Actor::host("evidence")));
    let reason = closed.close_reason.unwrap();
    assert!(reason.contains(&passing.run.id.to_string()), "{reason}");
    let proven = evidence_on(&tx, &Standalone, &view(&id, position, &second));
    assert!(proven.satisfied);
    assert!(proven.detail.contains(&passing.run.id.to_string()));

    let unrelated = run(
        &tx,
        &[("other::case", ResultOutcome::Passed)],
        Some(0),
        4_000,
    );
    assert_eq!(unrelated.run.verdict, Some(Verdict::Passed));
    assert!(unrelated.tickets.is_empty());
    assert_eq!(
        events::replay(&tx, &key()).unwrap(),
        store::state(&tx, &key()).unwrap()
    );
    assert_eq!(
        runs::stored(&tx, &key()).unwrap(),
        runs::replay(&tx, &key()).unwrap()
    );
    tx.commit().unwrap();
}

#[test]
fn a_no_tests_run_never_satisfies_a_test_check() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = opened(dir.path());
    let tx = store.transaction().unwrap();
    project(&tx);
    let only = case(&tx, "Store runs", "runs::store");
    let id = ticket(&tx, &owner(), "Runs are stored");
    let position = criterion(&tx, &id, "Runs are stored");
    add_test_check(&tx, &Standalone, &id, position, &only);
    set_policy(&tx, &Standalone, PolicyKey::Close, "owner");
    let check = view(&id, position, &only);

    let empty = run(
        &tx,
        &[("runs::store", ResultOutcome::Skipped)],
        Some(0),
        2_000,
    );
    assert_eq!(empty.run.verdict, Some(Verdict::NoTests));
    assert_eq!(empty.tickets, std::slice::from_ref(&id));
    assert_eq!(state(&tx, &id, position), CriterionState::Open);
    assert!(!evidence_on(&tx, &Standalone, &check).satisfied);
    set_policy(&tx, &Standalone, PolicyKey::SkippedCounts, "neutral");
    assert!(!evidence_on(&tx, &Standalone, &check).satisfied);

    let passing = run(
        &tx,
        &[("runs::store", ResultOutcome::Passed)],
        Some(0),
        3_000,
    );
    assert_eq!(passing.tickets, std::slice::from_ref(&id));
    assert_eq!(state(&tx, &id, position), CriterionState::Met);

    let unread = started(&tx, &hex('b', 40), 4_000);
    let unread = recorded(
        &tx,
        unread,
        &[("runs::store", ResultOutcome::Passed)],
        4_001,
    );
    let unread = finished(
        &tx,
        &Standalone,
        &unread,
        RunEnd::ReportUnreadable,
        Some(0),
        4_002,
    );
    assert_eq!(unread.run.verdict, Some(Verdict::Unknown));
    assert_eq!(unread.tickets, std::slice::from_ref(&id));
    assert_eq!(state(&tx, &id, position), CriterionState::Open);
    let detail = evidence_on(&tx, &Standalone, &check);
    assert!(!detail.satisfied);
    assert!(detail.detail.contains(&unread.run.id.to_string()));

    let broken = run(&tx, &[], Some(101), 5_000);
    assert_eq!(broken.run.verdict, Some(Verdict::BuildFailed));
    assert!(broken.tickets.is_empty());
    assert_eq!(state(&tx, &id, position), CriterionState::Open);

    let interrupted = started(&tx, &hex('b', 40), 6_000);
    let interrupted = recorded(
        &tx,
        interrupted,
        &[("runs::store", ResultOutcome::Passed)],
        6_001,
    );
    let cancel = runs::RunCancel {
        id: interrupted.id.clone(),
        version: interrupted.version,
        reason: runs::StopReason::TimedOut,
    };
    runs::cancel(&tx, &engine(), &cancel, 6_002).unwrap();
    assert!(!evidence_on(&tx, &Standalone, &check).satisfied);

    let again = run(
        &tx,
        &[("runs::store", ResultOutcome::Passed)],
        Some(0),
        7_000,
    );
    assert_eq!(again.tickets, std::slice::from_ref(&id));
    assert_eq!(state(&tx, &id, position), CriterionState::Met);
    assert_eq!(status(&tx, &id), TicketStatus::Todo);
}

#[test]
fn current_source_ignores_a_pass_on_another_tree() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = opened(dir.path());
    let tx = store.transaction().unwrap();
    project(&tx);
    tx.execute_batch(TREE_TABLE).unwrap();
    let only = case(&tx, "Store runs", "runs::store");
    let id = ticket(&tx, &owner(), "Runs are stored");
    let position = criterion(&tx, &id, "Runs are stored");
    add_test_check(&tx, &TreeHost, &id, position, &only);
    set_policy(&tx, &TreeHost, PolicyKey::TestEvidence, "current_source");
    assert_eq!(
        policy::load(&tx, &key()).unwrap().test_evidence,
        TestEvidence::CurrentSource
    );
    let check = view(&id, position, &only);

    let unknown = run_on(
        &tx,
        &TreeHost,
        &hex('b', 40),
        &[("runs::store", ResultOutcome::Passed)],
        Some(0),
        2_000,
    );
    assert_eq!(unknown.tickets, std::slice::from_ref(&id));
    assert_eq!(state(&tx, &id, position), CriterionState::Open);
    assert!(!evidence_on(&tx, &TreeHost, &check).satisfied);

    tx.execute(
        "INSERT INTO host_trees(ticket, tree) VALUES (?1, ?2)",
        (id.to_string(), hex('c', 40)),
    )
    .unwrap();
    let elsewhere = run_on(
        &tx,
        &TreeHost,
        &hex('b', 40),
        &[("runs::store", ResultOutcome::Passed)],
        Some(0),
        3_000,
    );
    assert_eq!(elsewhere.run.verdict, Some(Verdict::Passed));
    assert_eq!(elsewhere.tickets, std::slice::from_ref(&id));
    assert_eq!(state(&tx, &id, position), CriterionState::Open);
    assert_eq!(status(&tx, &id), TicketStatus::Todo);
    assert!(!evidence_on(&tx, &TreeHost, &check).satisfied);
    assert!(!evidence_on(&tx, &Standalone, &check).satisfied);

    let here = run_on(
        &tx,
        &TreeHost,
        &hex('c', 40),
        &[("runs::store", ResultOutcome::Passed)],
        Some(0),
        4_000,
    );
    assert_eq!(here.tickets, std::slice::from_ref(&id));
    assert_eq!(state(&tx, &id, position), CriterionState::Met);
    assert_eq!(status(&tx, &id), TicketStatus::Done);
    let proven = evidence_on(&tx, &TreeHost, &check);
    assert!(proven.satisfied);
    assert!(proven.detail.contains(&here.run.id.to_string()));
    assert!(!evidence_on(&tx, &Standalone, &check).satisfied);
}

#[test]
fn current_source_on_the_standalone_host_replies_option_needs_host() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = opened(dir.path());
    let tx = store.transaction().unwrap();
    project(&tx);
    let policies = count(&tx, "tk_policy");
    let written = count(&tx, "tk_events");

    let attempt = policy::set(
        &tx,
        &Standalone,
        &owner(),
        &key(),
        PolicyKey::TestEvidence,
        "current_source",
        0,
        1_050,
    );
    assert_eq!(refused(attempt), test_codes::OPTION_NEEDS_HOST);
    assert_eq!(count(&tx, "tk_policy"), policies);
    assert_eq!(count(&tx, "tk_events"), written);
    assert_eq!(
        policy::load(&tx, &key()).unwrap().test_evidence,
        TestEvidence::Latest
    );

    let settings = policy::describe(&tx, &key()).unwrap();
    let shown = |setting: PolicyKey| {
        settings
            .iter()
            .find(|entry| entry.key == setting)
            .unwrap()
            .clone()
    };
    let evidence = shown(PolicyKey::TestEvidence);
    assert_eq!(evidence.value, json!("latest"));
    assert_eq!(evidence.default, json!("latest"));
    assert_eq!(
        evidence.options,
        [json!("latest"), json!("current_source"), json!("plan")]
    );
    assert_eq!(evidence.version, 0);
    for (setting, default) in [
        (PolicyKey::SkippedCounts, "fail"),
        (PolicyKey::CaseProtectedVisibility, "hidden"),
        (PolicyKey::RunWriters, "any"),
        (PolicyKey::RunRetention, "prune_count"),
    ] {
        assert_eq!(shown(setting).value, json!(default), "{setting}");
        assert_eq!(shown(setting).default, json!(default), "{setting}");
    }

    set_policy(&tx, &Standalone, PolicyKey::TestEvidence, "plan");
    set_policy(&tx, &Standalone, PolicyKey::TestEvidence, "latest");
    set_policy(&tx, &TreeHost, PolicyKey::TestEvidence, "current_source");
    assert_eq!(
        policy::load(&tx, &key()).unwrap().test_evidence,
        TestEvidence::CurrentSource
    );
    assert_eq!(
        events::replay(&tx, &key()).unwrap(),
        store::state(&tx, &key()).unwrap()
    );
}

#[test]
fn a_host_lock_refuses_every_other_value_even_from_the_owner() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = opened(dir.path());
    let tx = store.transaction().unwrap();
    project(&tx);
    let policies = count(&tx, "tk_policy");
    let written = count(&tx, "tk_events");
    let writers = |tx: &Transaction<'_>| policy::load(tx, &key()).unwrap().run_writers;

    let attempt = policy::set(
        &tx,
        &TreeHost,
        &owner(),
        &key(),
        PolicyKey::RunWriters,
        "any",
        0,
        1_050,
    );
    match attempt {
        Err(StoreError::Refused(refusal)) => {
            assert_eq!(refusal.code, codes::POLICY_LOCKED);
            assert!(refusal.message.contains("run_writers"));
            assert!(refusal.message.contains("engine_only"));
        }
        other => panic!("expected policy_locked, got {other:?}"),
    }
    assert_eq!(count(&tx, "tk_policy"), policies);
    assert_eq!(count(&tx, "tk_events"), written);
    assert_eq!(writers(&tx), RunWriters::Any);

    set_policy(&tx, &TreeHost, PolicyKey::RunWriters, "engine_only");
    assert_eq!(writers(&tx), RunWriters::EngineOnly);
    let again = policy::set(
        &tx,
        &TreeHost,
        &owner(),
        &key(),
        PolicyKey::RunWriters,
        "any",
        1,
        1_060,
    );
    assert_eq!(refused(again), codes::POLICY_LOCKED);
    assert_eq!(writers(&tx), RunWriters::EngineOnly);

    set_policy(&tx, &TreeHost, PolicyKey::SkippedCounts, "neutral");
    assert_eq!(
        policy::load(&tx, &key()).unwrap().skipped_counts,
        SkippedCounts::Neutral
    );
    set_policy(&tx, &Standalone, PolicyKey::RunWriters, "any");
    assert_eq!(writers(&tx), RunWriters::Any);
    assert_eq!(
        events::replay(&tx, &key()).unwrap(),
        store::state(&tx, &key()).unwrap()
    );
}

#[test]
fn a_newer_interrupted_run_leaves_the_check_unsatisfied() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = opened(dir.path());
    let tx = store.transaction().unwrap();
    project(&tx);
    let only = case(&tx, "Store runs", "runs::store");
    let id = ticket(&tx, &owner(), "Runs are stored");
    let position = criterion(&tx, &id, "Runs are stored");
    add_test_check(&tx, &Standalone, &id, position, &only);
    set_policy(&tx, &Standalone, PolicyKey::Close, "owner");
    let check = view(&id, position, &only);
    let cancel = |run: &runs::RunView, now_ms: i64| {
        let input = runs::RunCancel {
            id: run.id.clone(),
            version: run.version,
            reason: runs::StopReason::TimedOut,
        };
        coverage::cancel(&tx, &Standalone, &engine(), &input, now_ms).unwrap()
    };

    let passing = run(
        &tx,
        &[("runs::store", ResultOutcome::Passed)],
        Some(0),
        2_000,
    );
    assert_eq!(passing.tickets, std::slice::from_ref(&id));
    assert_eq!(state(&tx, &id, position), CriterionState::Met);

    let empty = cancel(&started(&tx, &hex('b', 40), 3_000), 3_001);
    assert_eq!(empty.run.state, RunState::Interrupted);
    assert!(empty.tickets.is_empty());
    assert_eq!(state(&tx, &id, position), CriterionState::Met);
    assert!(evidence_on(&tx, &Standalone, &check).satisfied);

    let hung = started(&tx, &hex('b', 40), 4_000);
    let hung = recorded(&tx, hung, &[("runs::store", ResultOutcome::Failed)], 4_001);
    let hung = cancel(&hung, 4_002);
    assert_eq!(hung.run.state, RunState::Interrupted);
    assert_eq!(hung.run.verdict, None);
    assert_eq!(hung.tickets, std::slice::from_ref(&id));
    assert_eq!(state(&tx, &id, position), CriterionState::Open);
    let masked = evidence_on(&tx, &Standalone, &check);
    assert!(!masked.satisfied);
    assert!(masked.detail.contains(&hung.run.id.to_string()));
    assert!(masked.detail.contains("interrupted"));

    let page = coverage::coverage(&tx, &query(CoverageOf::Ticket(id.clone()), None)).unwrap();
    assert_eq!(
        page.rows[0].cases[0].latest,
        Some(CoverageResult {
            outcome: ResultOutcome::Passed,
            run: passing.run.id.clone(),
            verdict: Verdict::Passed,
        })
    );
    assert_eq!(
        coverage::coverage_as(
            &tx,
            &Standalone,
            &actor("agent:1"),
            &query(CoverageOf::Ticket(id.clone()), None)
        )
        .unwrap(),
        page,
        "the standalone host gives every actor full sight"
    );

    let again = run(
        &tx,
        &[("runs::store", ResultOutcome::Passed)],
        Some(0),
        5_000,
    );
    assert_eq!(again.tickets, std::slice::from_ref(&id));
    assert_eq!(state(&tx, &id, position), CriterionState::Met);
    assert_eq!(status(&tx, &id), TicketStatus::Todo);
    assert_eq!(
        runs::stored(&tx, &key()).unwrap(),
        runs::replay(&tx, &key()).unwrap()
    );
    assert_eq!(
        events::replay(&tx, &key()).unwrap(),
        store::state(&tx, &key()).unwrap()
    );
}

#[test]
fn a_skipped_linked_case_leaves_the_check_open_under_fail() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = opened(dir.path());
    let tx = store.transaction().unwrap();
    project(&tx);
    let skipped = case(&tx, "Store runs", "runs::store");
    case(&tx, "Replay runs", "runs::replay");
    let id = ticket(&tx, &owner(), "Runs are stored");
    let position = criterion(&tx, &id, "Runs are stored");
    add_test_check(&tx, &Standalone, &id, position, &skipped);
    set_policy(&tx, &Standalone, PolicyKey::Close, "owner");
    assert_eq!(
        policy::load(&tx, &key()).unwrap().skipped_counts,
        SkippedCounts::Fail
    );
    let check = view(&id, position, &skipped);

    let passing = run(
        &tx,
        &[("runs::store", ResultOutcome::Passed)],
        Some(0),
        2_000,
    );
    assert_eq!(state(&tx, &id, position), CriterionState::Met);

    let skipping = run(
        &tx,
        &[
            ("runs::store", ResultOutcome::Skipped),
            ("runs::replay", ResultOutcome::Passed),
        ],
        Some(0),
        3_000,
    );
    assert_eq!(skipping.run.verdict, Some(Verdict::Passed));
    assert_eq!(skipping.tickets, std::slice::from_ref(&id));
    assert_eq!(state(&tx, &id, position), CriterionState::Open);
    let open = evidence_on(&tx, &Standalone, &check);
    assert!(!open.satisfied);
    assert!(open.detail.contains(&skipping.run.id.to_string()));

    set_policy(&tx, &Standalone, PolicyKey::SkippedCounts, "neutral");
    let neutral = evidence_on(&tx, &Standalone, &check);
    assert!(neutral.satisfied);
    assert!(neutral.detail.contains(&passing.run.id.to_string()));
    let not_run = run(
        &tx,
        &[
            ("runs::store", ResultOutcome::NotRun),
            ("runs::replay", ResultOutcome::Passed),
        ],
        Some(0),
        4_000,
    );
    assert_eq!(not_run.tickets, std::slice::from_ref(&id));
    assert_eq!(state(&tx, &id, position), CriterionState::Met);
}

#[test]
fn the_plan_option_counts_only_runs_of_the_release_active_plan() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = opened(dir.path());
    let tx = store.transaction().unwrap();
    project(&tx);
    let only = case(&tx, "Store runs", "runs::store");
    let id = ticket(&tx, &owner(), "Runs are stored");
    let position = criterion(&tx, &id, "Runs are stored");
    add_test_check(&tx, &Standalone, &id, position, &only);
    set_policy(&tx, &Standalone, PolicyKey::Close, "owner");
    set_policy(&tx, &Standalone, PolicyKey::TestEvidence, "plan");
    let check = view(&id, position, &only);

    let unplanned = run(
        &tx,
        &[("runs::store", ResultOutcome::Passed)],
        Some(0),
        2_000,
    );
    assert_eq!(unplanned.tickets, std::slice::from_ref(&id));
    assert_eq!(state(&tx, &id, position), CriterionState::Open);
    assert!(!evidence_on(&tx, &Standalone, &check).satisfied);

    release(&tx, "1.4", &[&id]);
    let fields = PlanFields {
        name: "1.4 acceptance".to_owned(),
        release: Some("1.4".to_owned()),
        cycle: None,
        status: PlanStatus::Active,
    };
    let plan = tests_store::upsert_plan(&tx, &owner(), &key(), None, &fields, 2_500).unwrap();
    assert!(!evidence_on(&tx, &Standalone, &check).satisfied);

    let start = RunStart {
        project: key(),
        title: "1.4 acceptance".to_owned(),
        plan: Some(plan.short_id.clone()),
        batch: None,
        source: source(&hex('b', 40)),
        policy: check_policy(),
    };
    let planned = runs::start(&tx, &engine(), &start, 3_000).unwrap();
    let planned = recorded(
        &tx,
        planned,
        &[("runs::store", ResultOutcome::Passed)],
        3_001,
    );
    let planned = finished(
        &tx,
        &Standalone,
        &planned,
        RunEnd::Completed,
        Some(0),
        3_002,
    );
    assert_eq!(planned.tickets, std::slice::from_ref(&id));
    assert_eq!(state(&tx, &id, position), CriterionState::Met);

    let later = run(
        &tx,
        &[("runs::store", ResultOutcome::Failed)],
        Some(1),
        4_000,
    );
    assert_eq!(later.tickets, std::slice::from_ref(&id));
    assert_eq!(state(&tx, &id, position), CriterionState::Met);
    let proven = evidence_on(&tx, &Standalone, &check);
    assert!(proven.detail.contains(&planned.run.id.to_string()));
}

#[test]
fn recorded_evidence_never_satisfies_a_check_that_names_a_case() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = opened(dir.path());
    let tx = store.transaction().unwrap();
    project(&tx);
    let agent = actor("agent:w1");
    let only = case(&tx, "Store runs", "runs::store");
    let id = ticket(&tx, &agent, "Runs are stored");
    let opened_as = status(&tx, &id);
    assert!(!opened_as.is_closed());
    let named = criteria::add_criterion(&tx, &Standalone, &agent, &id, "Runs are stored", 1_200)
        .unwrap()
        .position;
    criteria::add_check(
        &tx,
        &Standalone,
        &agent,
        &id,
        named,
        CheckKind::Test,
        &only.to_string(),
        1_210,
    )
    .unwrap();
    let missing = criteria::add_criterion(&tx, &Standalone, &agent, &id, "Runs replay", 1_220)
        .unwrap()
        .position;
    criteria::add_check(
        &tx,
        &Standalone,
        &agent,
        &id,
        missing,
        CheckKind::Test,
        "CA-T99",
        1_230,
    )
    .unwrap();

    for (position, check) in [(named, only.to_string()), (missing, "CA-T99".to_owned())] {
        let claim = NewEvidence {
            ticket: id.clone(),
            criterion: position,
            kind: CheckKind::Test,
            check: check.clone(),
            result: EvidenceResult::Pass,
            reference: None,
        };
        let (_, outcome) =
            criteria::record_evidence(&tx, &Standalone, &agent, &claim, 1_300).unwrap();
        assert_eq!(outcome.criterion.state, CriterionState::Open, "{check}");
        assert_eq!(outcome.ticket.status, opened_as, "{check}");
    }
    let gone = coverage::test_check(
        &tx,
        &Standalone,
        &key(),
        &CheckView {
            ticket: id.clone(),
            criterion: missing,
            kind: CheckKind::Test,
            reference: "CA-T99".to_owned(),
        },
    )
    .unwrap()
    .unwrap();
    assert!(!gone.satisfied);
    let free = CheckView {
        ticket: id.clone(),
        criterion: named,
        kind: CheckKind::Test,
        reference: "test:unit".to_owned(),
    };
    assert_eq!(
        coverage::test_check(&tx, &Standalone, &key(), &free).unwrap(),
        None
    );

    let passing = run(
        &tx,
        &[("runs::store", ResultOutcome::Passed)],
        Some(0),
        2_000,
    );
    assert_eq!(passing.tickets, std::slice::from_ref(&id));
    assert_eq!(state(&tx, &id, named), CriterionState::Met);
    assert_eq!(state(&tx, &id, missing), CriterionState::Open);
    assert_eq!(status(&tx, &id), opened_as);
}

#[test]
fn coverage_lists_uncovered_criteria() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = opened(dir.path());
    let tx = store.transaction().unwrap();
    project(&tx);
    let proving = case(&tx, "Store runs", "runs::store");
    let linked = case(&tx, "Replay runs", "runs::replay");
    let id = ticket(&tx, &owner(), "Runs are stored and replayed");
    let checked = criterion(&tx, &id, "Runs are stored");
    let bare = criterion(&tx, &id, "Runs are pruned");
    let mentioned = criterion(&tx, &id, "Runs replay");
    add_test_check(&tx, &Standalone, &id, checked, &proving);
    let mut version = tests_store::case(&tx, &linked).unwrap().version;
    for position in [checked, mentioned] {
        let target = CriterionRef {
            ticket: id.clone(),
            position,
        };
        version = tests_store::link_case(&tx, &owner(), &linked, version, &target, 1_250)
            .unwrap()
            .version;
    }
    let ran = run(
        &tx,
        &[
            ("runs::store", ResultOutcome::Passed),
            ("runs::replay", ResultOutcome::Failed),
        ],
        Some(101),
        2_000,
    );
    assert_eq!(ran.run.verdict, Some(Verdict::Failed));
    let latest = |outcome: ResultOutcome| {
        Some(CoverageResult {
            outcome,
            run: ran.run.id.clone(),
            verdict: Verdict::Failed,
        })
    };

    let page = coverage::coverage(&tx, &query(CoverageOf::Ticket(id.clone()), None)).unwrap();
    assert_eq!(page.criteria_total, 3);
    assert_eq!(page.uncovered_total, 1);
    assert_eq!(page.cursor, None);
    let positions: Vec<u32> = page.rows.iter().map(|row| row.criterion.position).collect();
    assert_eq!(positions, [checked, bare, mentioned]);
    assert!(page.rows.iter().all(|row| row.criterion.ticket == id));
    assert_eq!(page.rows[0].text, "Runs are stored");
    assert_eq!(page.rows[0].state, CriterionState::Met);
    assert_eq!(page.rows[1].state, CriterionState::Open);
    assert_eq!(
        page.rows[0].cases,
        [
            CoverageCase {
                case: proving.clone(),
                title: "Store runs".to_owned(),
                proves: true,
                latest: latest(ResultOutcome::Passed),
            },
            CoverageCase {
                case: linked.clone(),
                title: "Replay runs".to_owned(),
                proves: false,
                latest: latest(ResultOutcome::Failed),
            },
        ]
    );
    assert_eq!(page.rows[0].cases_total, 2);
    assert!(page.rows[1].cases.is_empty());
    assert_eq!(page.rows[1].cases_total, 0);
    assert_eq!(page.rows[2].cases.len(), 1);
    assert_eq!(page.rows[2].cases[0].case, linked);
    assert!(!page.rows[2].cases[0].proves);

    let unknown = ShortId::parse("CA-99").unwrap();
    assert_eq!(
        refused(coverage::coverage(
            &tx,
            &query(CoverageOf::Ticket(unknown), None)
        )),
        codes::TICKET_UNKNOWN
    );
    assert_eq!(
        refused(coverage::coverage(
            &tx,
            &query(CoverageOf::Version("9.9".to_owned()), None)
        )),
        test_codes::RELEASE_UNKNOWN
    );
    let other = ProjectKey::new("CB").unwrap();
    store::create_project(&tx, &owner(), &other, "Other", 1_000).unwrap();
    let theirs = store::create_ticket(
        &tx,
        &owner(),
        &other,
        &NewTicket {
            title: "Theirs".to_owned(),
            body: String::new(),
            kind: TicketKind::Feature,
            status: TicketStatus::Todo,
            priority: Priority::new(2).unwrap(),
        },
        1_100,
    )
    .unwrap()
    .short_id;
    assert_eq!(
        refused(coverage::coverage(
            &tx,
            &query(CoverageOf::Ticket(theirs), None)
        )),
        test_codes::CROSS_PROJECT
    );
}

#[test]
fn coverage_pages_a_release_by_ticket_then_position() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = opened(dir.path());
    let tx = store.transaction().unwrap();
    project(&tx);
    let first = ticket(&tx, &owner(), "First");
    let second = ticket(&tx, &owner(), "Second");
    let outside = ticket(&tx, &owner(), "Outside");
    for (id, total) in [(&first, 32), (&second, 32), (&outside, 1)] {
        for n in 1..=total {
            criterion(&tx, id, &format!("C{n}"));
        }
    }
    release(&tx, "1.4", &[&first, &second]);

    let mut seen = Vec::new();
    let mut after = None;
    for _ in 0..4 {
        let page =
            coverage::coverage(&tx, &query(CoverageOf::Version("1.4".to_owned()), after)).unwrap();
        assert_eq!(page.criteria_total, 64);
        assert_eq!(page.uncovered_total, 64);
        assert!(!page.rows.is_empty());
        assert!(page.rows.len() <= coverage::COVERAGE_PAGE_ROWS);
        let bytes = serde_json::to_vec(&page.rows).unwrap().len();
        assert!(bytes <= coverage::COVERAGE_PAGE_BYTES_MAX);
        seen.extend(page.rows.iter().map(|row| row.criterion.clone()));
        after = page.cursor;
        if after.is_none() {
            break;
        }
        assert_eq!(after.as_ref(), seen.last());
    }
    assert_eq!(after, None);
    assert_eq!(seen.len(), 64);
    let mut sorted = seen.clone();
    sorted.sort_by_key(|row| (row.ticket.number(), row.position));
    sorted.dedup();
    assert_eq!(sorted, seen);
    assert!(seen.iter().all(|row| row.ticket != outside));
}
