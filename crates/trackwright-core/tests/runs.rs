use rusqlite::{Connection, Transaction};
use std::path::Path;
use trackwright_core::events;
use trackwright_core::host::{Authority, Sight};
use trackwright_core::model::{Actor, ProjectKey, codes};
use trackwright_core::runs::{
    self, IntegrityRef, Inventory, NetworkMode, NewResult, NewSkip, PolicyRef, ResultOutcome,
    RunCancel, RunEnd, RunFinish, RunList, RunRecord, RunRetention, RunShow, RunStart, RunState,
    RunView, RunWriters, Sighted, SourceRef, StopReason, TestId, TestObject, Toolchain,
    ToolchainFamily, Verdict, codes as run_codes,
};
use trackwright_core::schema;
use trackwright_core::standalone::Standalone;
use trackwright_core::store::{self, Store, StoreError};
use trackwright_core::tests_model::{
    CaseKind, CaseStatus, RESULT_MESSAGE_BYTES_MAX, RESULT_ROWS_PER_RECORD_MAX,
    RUNS_RETAINED_PER_PROJECT_MAX,
};
use trackwright_core::tests_store::{self, NewCase};

const DAY_MS: i64 = 24 * 60 * 60 * 1_000;

fn opened(dir: &Path) -> Store {
    Store::open(&dir.join("tracker").join("tracker.db")).unwrap()
}

fn actor(name: &str) -> Actor {
    Actor::new(name).unwrap()
}

fn key() -> ProjectKey {
    ProjectKey::new("CA").unwrap()
}

fn project(tx: &Transaction<'_>) {
    store::create_project(tx, &actor("owner"), &key(), "CrazyAgent", 1_000).unwrap();
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

fn set_policy(tx: &Transaction<'_>, policy: &str, value: &str) {
    tx.execute(
        "INSERT INTO tk_policy(project, key, value_json, version)
         SELECT id, ?1, ?2, 1 FROM tk_projects WHERE key = 'CA'
         ON CONFLICT(project, key) DO UPDATE
         SET value_json = excluded.value_json, version = version + 1",
        (policy, serde_json::to_string(value).unwrap()),
    )
    .unwrap();
}

fn hex(digit: char, len: usize) -> String {
    std::iter::repeat_n(digit, len).collect()
}

fn source() -> SourceRef {
    SourceRef::Git {
        repository: "crazyagent".to_owned(),
        commit: hex('a', 40),
        tree: hex('b', 40),
        base: Some(hex('c', 40)),
        change_set: Some("CS-12".to_owned()),
    }
}

fn policy(kind: CaseKind) -> PolicyRef {
    PolicyRef {
        profile: hex('d', 64),
        profile_approved_ms: 1_000,
        check: "unit".to_owned(),
        kind,
        argv: vec!["cargo".to_owned(), "test".to_owned(), "--locked".to_owned()],
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

fn start_of(kind: CaseKind) -> RunStart {
    RunStart {
        project: key(),
        title: "unit on CS-12".to_owned(),
        plan: None,
        batch: Some("trigger-7".to_owned()),
        source: source(),
        policy: policy(kind),
    }
}

fn started(tx: &Transaction<'_>, writer: &Actor, now_ms: i64) -> RunView {
    runs::start(tx, writer, &start_of(CaseKind::Unit), now_ms).unwrap()
}

fn result(name: &str, outcome: ResultOutcome) -> NewResult {
    NewResult {
        name: name.to_owned(),
        outcome,
        duration_ms: Some(12),
        message: None,
        artifact: None,
    }
}

fn record_of(run: &RunView, results: Vec<NewResult>) -> RunRecord {
    RunRecord {
        id: run.id.clone(),
        version: run.version,
        results,
    }
}

fn recorded(
    tx: &Transaction<'_>,
    writer: &Actor,
    run: &RunView,
    results: &[(&str, ResultOutcome)],
    now_ms: i64,
) -> RunView {
    let rows = results
        .iter()
        .map(|(name, outcome)| result(name, *outcome))
        .collect();
    runs::record(tx, writer, &record_of(run, rows), now_ms).unwrap()
}

fn finish_of(run: &RunView, end: RunEnd, exit_code: Option<i32>) -> RunFinish {
    RunFinish {
        id: run.id.clone(),
        version: run.version,
        end,
        exit_code,
        reason: None,
        integrity: None,
    }
}

fn finished(
    tx: &Transaction<'_>,
    writer: &Actor,
    run: &RunView,
    exit_code: Option<i32>,
    now_ms: i64,
) -> RunView {
    let finish = finish_of(run, RunEnd::Completed, exit_code);
    runs::finish(tx, writer, &finish, now_ms).unwrap()
}

fn list_of(verdict: Option<Verdict>) -> RunList {
    RunList {
        project: key(),
        plan: None,
        batch: None,
        verdict,
        commit: None,
        before: None,
    }
}

fn run_id(text: &str) -> TestId {
    TestId::parse_of(text, TestObject::Run).unwrap()
}

fn run_numbers(tx: &Transaction<'_>) -> Vec<i64> {
    let mut statement = tx
        .prepare("SELECT number FROM tk_runs ORDER BY number")
        .unwrap();
    statement
        .query_map([], |row| row.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

#[test]
fn a_finished_run_with_zero_executed_results_is_no_tests_and_never_passed() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = opened(dir.path());
    let tx = store.transaction().unwrap();
    project(&tx);
    let engine = actor("engine:checks");

    let empty = started(&tx, &engine, 2_000);
    assert_eq!(empty.id.to_string(), "CA-R1");
    assert_eq!(empty.state, RunState::Running);
    assert_eq!(empty.verdict, None);
    let empty = finished(&tx, &engine, &empty, Some(0), 2_001);
    assert_eq!(empty.state, RunState::Finished);
    assert_eq!(empty.verdict, Some(Verdict::NoTests));
    assert_eq!(empty.totals.total, 0);
    assert_eq!(empty.finished_ms, Some(2_001));

    let idle = started(&tx, &engine, 2_002);
    let idle = recorded(
        &tx,
        &engine,
        &idle,
        &[
            ("idle::skipped", ResultOutcome::Skipped),
            ("idle::not_run", ResultOutcome::NotRun),
        ],
        2_003,
    );
    let idle = finished(&tx, &engine, &idle, Some(0), 2_004);
    assert_eq!(idle.verdict, Some(Verdict::NoTests));
    assert_eq!(idle.totals.total, 2);
    assert_eq!(idle.totals.skipped, 1);
    assert_eq!(idle.totals.passed, 0);
    assert_eq!(idle.totals.failed, 0);

    let passed = runs::list(&tx, &list_of(Some(Verdict::Passed))).unwrap();
    assert!(passed.rows.is_empty());
    let no_tests = runs::list(&tx, &list_of(Some(Verdict::NoTests))).unwrap();
    let ids: Vec<String> = no_tests.rows.iter().map(|row| row.id.to_string()).collect();
    assert_eq!(ids, ["CA-R2", "CA-R1"]);

    for verdict in Verdict::ALL {
        assert_eq!(
            verdict.satisfies(),
            *verdict == Verdict::Passed,
            "{verdict}"
        );
    }
    let shown = runs::show(
        &tx,
        &RunShow {
            id: idle.id.clone(),
            after: None,
        },
    )
    .unwrap();
    assert_eq!(shown.run.verdict, Some(Verdict::NoTests));
    assert!(!shown.run.verdict.unwrap().satisfies());
}

#[test]
fn a_run_records_results_per_case_and_replays_from_events() {
    let dir = tempfile::tempdir().unwrap();
    let engine = actor("engine:checks");
    let owner = actor("owner");
    {
        let mut store = opened(dir.path());
        let tx = store.transaction().unwrap();
        project(&tx);
        let first = tests_store::create_case(
            &tx,
            &owner,
            &key(),
            &NewCase {
                title: "Store runs".to_owned(),
                kind: CaseKind::Unit,
                stable_name: Some("runs::store_runs".to_owned()),
                steps: Vec::new(),
                protected: false,
                status: CaseStatus::Ready,
            },
            1_500,
        )
        .unwrap();
        let second = tests_store::create_case(
            &tx,
            &owner,
            &key(),
            &NewCase {
                title: "Replay runs".to_owned(),
                kind: CaseKind::Unit,
                stable_name: Some("runs::replay_runs".to_owned()),
                steps: Vec::new(),
                protected: false,
                status: CaseStatus::Ready,
            },
            1_501,
        )
        .unwrap();
        assert_eq!(first.short_id.to_string(), "CA-T1");
        assert_eq!(second.short_id.to_string(), "CA-T2");

        let run = started(&tx, &engine, 2_000);
        let mut failing = result("runs::replay_runs", ResultOutcome::Failed);
        failing.message = Some("assertion failed: left == right".to_owned());
        failing.artifact = Some("target/report.xml".to_owned());
        let run = runs::record(
            &tx,
            &engine,
            &record_of(
                &run,
                vec![result("runs::store_runs", ResultOutcome::Passed), failing],
            ),
            2_001,
        )
        .unwrap();
        let run = recorded(
            &tx,
            &engine,
            &run,
            &[("runs::unmatched", ResultOutcome::Skipped)],
            2_002,
        );
        assert_eq!(run.totals.total, 3);
        let run = finished(&tx, &engine, &run, Some(101), 2_003);
        assert_eq!(run.verdict, Some(Verdict::Failed));

        let stopped = started(&tx, &engine, 2_004);
        let stopped = recorded(
            &tx,
            &engine,
            &stopped,
            &[("runs::store_runs", ResultOutcome::Passed)],
            2_005,
        );
        let cancel = RunCancel {
            id: stopped.id.clone(),
            version: stopped.version,
            reason: StopReason::TimedOut,
        };
        runs::cancel(&tx, &engine, &cancel, 2_006).unwrap();

        let shown = runs::show(
            &tx,
            &RunShow {
                id: run.id.clone(),
                after: None,
            },
        )
        .unwrap();
        let cases: Vec<Option<String>> = shown
            .results
            .iter()
            .map(|row| row.case.as_ref().map(ToString::to_string))
            .collect();
        assert_eq!(
            cases,
            [Some("CA-T1".to_owned()), Some("CA-T2".to_owned()), None]
        );
        let positions: Vec<u32> = shown.results.iter().map(|row| row.position).collect();
        assert_eq!(positions, [1, 2, 3]);
        assert_eq!(shown.unlinked, ["runs::unmatched"]);
        assert_eq!(shown.unlinked_total, 1);
        assert_eq!(shown.cursor, None);
        assert_eq!(
            shown.results[1].message.as_deref(),
            Some("assertion failed: left == right")
        );

        let mut statement = tx
            .prepare("SELECT op FROM tk_events WHERE op LIKE 'run.%' ORDER BY id")
            .unwrap();
        let ops: Vec<String> = statement
            .query_map([], |row| row.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        drop(statement);
        assert_eq!(
            ops,
            [
                runs::RUN_START,
                runs::RUN_RECORD,
                runs::RUN_RECORD,
                runs::RUN_FINISH,
                runs::RUN_START,
                runs::RUN_RECORD,
                runs::RUN_CANCEL,
            ]
        );
        assert_eq!(
            runs::stored(&tx, &key()).unwrap(),
            runs::replay(&tx, &key()).unwrap()
        );
        assert_eq!(
            store::state(&tx, &key()).unwrap(),
            events::replay(&tx, &key()).unwrap()
        );
        tx.commit().unwrap();
    }
    let mut store = opened(dir.path());
    let tx = store.transaction().unwrap();
    let current = runs::stored(&tx, &key()).unwrap();
    assert_eq!(current.len(), 2);
    assert_eq!(current[0].results.len(), 3);
    assert_eq!(current[1].run.state, RunState::Interrupted);
    assert_eq!(current[1].run.reason.as_deref(), Some("timed_out"));
    assert_eq!(current[1].run.verdict, None);
    assert_eq!(current, runs::replay(&tx, &key()).unwrap());
}

#[test]
fn any_failed_or_errored_result_makes_the_run_failed() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = opened(dir.path());
    let tx = store.transaction().unwrap();
    project(&tx);
    let engine = actor("engine:checks");

    let one_failed = started(&tx, &engine, 2_000);
    let one_failed = recorded(
        &tx,
        &engine,
        &one_failed,
        &[
            ("a::passes", ResultOutcome::Passed),
            ("a::fails", ResultOutcome::Failed),
        ],
        2_001,
    );
    let one_failed = finished(&tx, &engine, &one_failed, Some(101), 2_002);
    assert_eq!(one_failed.verdict, Some(Verdict::Failed));
    assert_eq!(one_failed.totals.failed, 1);
    assert_eq!(one_failed.totals.passed, 1);

    let one_errored = started(&tx, &engine, 2_003);
    let one_errored = recorded(
        &tx,
        &engine,
        &one_errored,
        &[
            ("b::passes", ResultOutcome::Passed),
            ("b::breaks", ResultOutcome::Errored),
        ],
        2_004,
    );
    let one_errored = finished(&tx, &engine, &one_errored, Some(1), 2_005);
    assert_eq!(one_errored.verdict, Some(Verdict::Failed));
    assert_eq!(one_errored.totals.errored, 1);

    let no_exit = started(&tx, &engine, 2_006);
    let no_exit = recorded(
        &tx,
        &engine,
        &no_exit,
        &[("c::fails", ResultOutcome::Failed)],
        2_007,
    );
    let no_exit = finished(&tx, &engine, &no_exit, None, 2_008);
    assert_eq!(no_exit.verdict, Some(Verdict::Failed));

    let clean = started(&tx, &engine, 2_009);
    let clean = recorded(
        &tx,
        &engine,
        &clean,
        &[("d::passes", ResultOutcome::Passed)],
        2_010,
    );
    let clean = finished(&tx, &engine, &clean, Some(0), 2_011);
    assert_eq!(clean.verdict, Some(Verdict::Passed));
    assert!(clean.verdict.unwrap().satisfies());
}

#[test]
fn an_exit_status_that_contradicts_the_results_is_unknown() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = opened(dir.path());
    let tx = store.transaction().unwrap();
    project(&tx);
    let engine = actor("engine:checks");

    let quiet_failure = started(&tx, &engine, 2_000);
    let quiet_failure = recorded(
        &tx,
        &engine,
        &quiet_failure,
        &[("a::fails", ResultOutcome::Failed)],
        2_001,
    );
    let quiet_failure = finished(&tx, &engine, &quiet_failure, Some(0), 2_002);
    assert_eq!(quiet_failure.verdict, Some(Verdict::Unknown));

    let loud_pass = started(&tx, &engine, 2_003);
    let loud_pass = recorded(
        &tx,
        &engine,
        &loud_pass,
        &[("b::passes", ResultOutcome::Passed)],
        2_004,
    );
    let loud_pass = finished(&tx, &engine, &loud_pass, Some(101), 2_005);
    assert_eq!(loud_pass.verdict, Some(Verdict::Unknown));

    let unreadable = started(&tx, &engine, 2_006);
    let unreadable = recorded(
        &tx,
        &engine,
        &unreadable,
        &[("c::passes", ResultOutcome::Passed)],
        2_007,
    );
    let finish = finish_of(&unreadable, RunEnd::ReportUnreadable, Some(0));
    let unreadable = runs::finish(&tx, &engine, &finish, 2_008).unwrap();
    assert_eq!(unreadable.verdict, Some(Verdict::Unknown));
    assert!(!unreadable.verdict.unwrap().satisfies());
}

#[test]
fn a_build_that_failed_before_tests_ran_is_build_failed_not_failed() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = opened(dir.path());
    let tx = store.transaction().unwrap();
    project(&tx);
    let engine = actor("engine:checks");

    let silent = started(&tx, &engine, 2_000);
    let silent = finished(&tx, &engine, &silent, Some(101), 2_001);
    assert_eq!(silent.verdict, Some(Verdict::BuildFailed));

    let reported = started(&tx, &engine, 2_002);
    let finish = finish_of(&reported, RunEnd::BuildFailed, Some(101));
    let reported = runs::finish(&tx, &engine, &finish, 2_003).unwrap();
    assert_eq!(reported.verdict, Some(Verdict::BuildFailed));
    assert_ne!(reported.verdict, Some(Verdict::Failed));

    let with_results = started(&tx, &engine, 2_004);
    let with_results = recorded(
        &tx,
        &engine,
        &with_results,
        &[("a::passes", ResultOutcome::Passed)],
        2_005,
    );
    let finish = finish_of(&with_results, RunEnd::BuildFailed, Some(101));
    assert_eq!(
        refused(runs::finish(&tx, &engine, &finish, 2_006)),
        run_codes::RUN_FINISH_INVALID
    );
}

#[test]
fn a_run_that_could_not_start_is_unavailable_with_its_reason() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = opened(dir.path());
    let tx = store.transaction().unwrap();
    project(&tx);
    let engine = actor("engine:checks");

    let run = started(&tx, &engine, 2_000);
    let silent = finish_of(&run, RunEnd::NotStarted, None);
    assert_eq!(
        refused(runs::finish(&tx, &engine, &silent, 2_001)),
        run_codes::RUN_FINISH_INVALID
    );
    let mut missing = finish_of(&run, RunEnd::NotStarted, None);
    missing.reason = Some("toolchain missing: rustc".to_owned());
    let run = runs::finish(&tx, &engine, &missing, 2_002).unwrap();
    assert_eq!(run.state, RunState::Finished);
    assert_eq!(run.verdict, Some(Verdict::Unavailable));
    assert_eq!(run.reason.as_deref(), Some("toolchain missing: rustc"));

    let ran = started(&tx, &engine, 2_003);
    let ran = recorded(
        &tx,
        &engine,
        &ran,
        &[("a::passes", ResultOutcome::Passed)],
        2_004,
    );
    let mut claimed = finish_of(&ran, RunEnd::NotStarted, None);
    claimed.reason = Some("sandbox".to_owned());
    assert_eq!(
        refused(runs::finish(&tx, &engine, &claimed, 2_005)),
        run_codes::RUN_FINISH_INVALID
    );
}

#[test]
fn a_cancelled_or_timed_out_run_is_interrupted_without_a_verdict() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = opened(dir.path());
    let tx = store.transaction().unwrap();
    project(&tx);
    let engine = actor("engine:checks");

    for (index, reason) in [StopReason::Cancelled, StopReason::TimedOut]
        .into_iter()
        .enumerate()
    {
        let now_ms = 2_000 + 10 * index as i64;
        let run = started(&tx, &engine, now_ms);
        let run = recorded(
            &tx,
            &engine,
            &run,
            &[("a::passes", ResultOutcome::Passed)],
            now_ms + 1,
        );
        let cancel = RunCancel {
            id: run.id.clone(),
            version: run.version,
            reason,
        };
        let run = runs::cancel(&tx, &engine, &cancel, now_ms + 2).unwrap();
        assert_eq!(run.state, RunState::Interrupted);
        assert_eq!(run.verdict, None);
        assert_eq!(run.reason.as_deref(), Some(reason.as_str()));
        assert_eq!(run.finished_ms, Some(now_ms + 2));
    }
    let listed = runs::list(&tx, &list_of(Some(Verdict::Passed))).unwrap();
    assert!(listed.rows.is_empty());
}

#[test]
fn a_result_after_finish_is_refused_and_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = opened(dir.path());
    let tx = store.transaction().unwrap();
    project(&tx);
    let engine = actor("engine:checks");

    let run = started(&tx, &engine, 2_000);
    let run = recorded(
        &tx,
        &engine,
        &run,
        &[("a::passes", ResultOutcome::Passed)],
        2_001,
    );
    let stale = record_of(&run, vec![result("a::late", ResultOutcome::Passed)]);
    let run = finished(&tx, &engine, &run, Some(0), 2_002);
    assert_eq!(run.verdict, Some(Verdict::Passed));

    let results_before = count(&tx, "tk_results");
    let events_before = count(&tx, "tk_events");
    let late = record_of(&run, vec![result("a::late", ResultOutcome::Failed)]);
    assert_eq!(
        refused(runs::record(&tx, &engine, &late, 2_003)),
        run_codes::RUN_CLOSED
    );
    assert_eq!(
        refused(runs::record(&tx, &engine, &stale, 2_003)),
        codes::STALE_VERSION
    );
    let again = finish_of(&run, RunEnd::Completed, Some(1));
    assert_eq!(
        refused(runs::finish(&tx, &engine, &again, 2_004)),
        run_codes::RUN_CLOSED
    );
    let cancel = RunCancel {
        id: run.id.clone(),
        version: run.version,
        reason: StopReason::Cancelled,
    };
    assert_eq!(
        refused(runs::cancel(&tx, &engine, &cancel, 2_005)),
        run_codes::RUN_CLOSED
    );

    assert_eq!(count(&tx, "tk_results"), results_before);
    assert_eq!(count(&tx, "tk_events"), events_before);
    let shown = runs::show(
        &tx,
        &RunShow {
            id: run.id.clone(),
            after: None,
        },
    )
    .unwrap();
    assert_eq!(shown.run, run);
    assert_eq!(shown.results.len(), 1);

    let rewrite = tx.execute("UPDATE tk_results SET outcome = 'failed'", []);
    assert!(rewrite.is_err(), "tk_results rows are never rewritten");
    let unknown = RunRecord {
        id: run_id("CA-R99"),
        version: 1,
        results: vec![result("a::ghost", ResultOutcome::Passed)],
    };
    assert_eq!(
        refused(runs::record(&tx, &engine, &unknown, 2_006)),
        run_codes::RUN_UNKNOWN
    );
    assert_eq!(count(&tx, "tk_results"), results_before);
}

#[test]
fn a_bad_batch_of_results_is_refused_whole() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = opened(dir.path());
    let tx = store.transaction().unwrap();
    project(&tx);
    let engine = actor("engine:checks");
    let run = started(&tx, &engine, 2_000);
    let run = recorded(
        &tx,
        &engine,
        &run,
        &[("a::first", ResultOutcome::Passed)],
        2_001,
    );
    let results_before = count(&tx, "tk_results");
    let events_before = count(&tx, "tk_events");

    let over: Vec<NewResult> = (0..=RESULT_ROWS_PER_RECORD_MAX)
        .map(|index| result(&format!("a::case_{index}"), ResultOutcome::Passed))
        .collect();
    let batches = [
        (over, run_codes::RESULTS_TOO_MANY),
        (
            vec![
                result("a::new", ResultOutcome::Passed),
                result("a::new", ResultOutcome::Failed),
            ],
            run_codes::RESULT_NAME_REPEATED,
        ),
        (
            vec![
                result("a::new", ResultOutcome::Passed),
                result("a::first", ResultOutcome::Failed),
            ],
            run_codes::RESULT_NAME_REPEATED,
        ),
        (
            vec![result("a::\u{202e}evil", ResultOutcome::Passed)],
            codes::TEXT_HIDDEN_CHARACTERS,
        ),
        (
            vec![result(&"n".repeat(513), ResultOutcome::Passed)],
            codes::TEXT_TOO_LONG,
        ),
        (
            vec![result("a::two\nlines", ResultOutcome::Passed)],
            codes::TEXT_LINE_BREAK,
        ),
        (vec![result(" ", ResultOutcome::Passed)], codes::TEXT_EMPTY),
        (Vec::new(), run_codes::RESULTS_EMPTY),
    ];
    for (rows, code) in batches {
        assert_eq!(
            refused(runs::record(&tx, &engine, &record_of(&run, rows), 2_002)),
            code
        );
    }
    let mut hidden_message = result("a::new", ResultOutcome::Failed);
    hidden_message.message = Some("left\u{200b}right".to_owned());
    assert_eq!(
        refused(runs::record(
            &tx,
            &engine,
            &record_of(&run, vec![hidden_message]),
            2_003
        )),
        codes::TEXT_HIDDEN_CHARACTERS
    );
    assert_eq!(count(&tx, "tk_results"), results_before);
    assert_eq!(count(&tx, "tk_events"), events_before);
}

#[test]
fn a_long_message_keeps_its_tail_marked_elided() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = opened(dir.path());
    let tx = store.transaction().unwrap();
    project(&tx);
    let engine = actor("engine:checks");
    let run = started(&tx, &engine, 2_000);

    let head = "h".repeat(RESULT_MESSAGE_BYTES_MAX);
    let tail = "panicked at tests/runs.rs:42: the decisive line";
    let mut long = result("a::fails", ResultOutcome::Failed);
    long.message = Some(format!("{head}\n{tail}"));
    let mut exact = result("a::exact", ResultOutcome::Failed);
    exact.message = Some("é".repeat(RESULT_MESSAGE_BYTES_MAX / 2));
    let run = runs::record(&tx, &engine, &record_of(&run, vec![long, exact]), 2_001).unwrap();

    let shown = runs::show(
        &tx,
        &RunShow {
            id: run.id.clone(),
            after: None,
        },
    )
    .unwrap();
    let stored: Vec<String> = {
        let mut statement = tx
            .prepare("SELECT message FROM tk_results ORDER BY position")
            .unwrap();
        statement
            .query_map([], |row| row.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    };
    assert!(stored[0].starts_with(runs::ELIDED_MARK));
    assert!(stored[0].ends_with(tail));
    assert!(stored[0].len() <= RESULT_MESSAGE_BYTES_MAX);
    assert_eq!(stored[1], "é".repeat(RESULT_MESSAGE_BYTES_MAX / 2));
    assert!(shown.results[0].message.as_deref().unwrap().ends_with(tail));
}

#[test]
fn run_writers_engine_only_refuses_an_agent_and_a_master() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = opened(dir.path());
    let tx = store.transaction().unwrap();
    project(&tx);
    let engine = actor("engine:checks");
    let agent = actor("agent:w1");
    let master = actor("master:s1");
    let owner = actor("owner");

    let by_agent = started(&tx, &agent, 2_000);
    assert_eq!(by_agent.recorded_by, agent);

    set_policy(&tx, "run_writers", RunWriters::EngineOnly.as_str());
    let runs_before = count(&tx, "tk_runs");
    let events_before = count(&tx, "tk_events");
    for writer in [&agent, &master, &owner, &actor("engine:mapping")] {
        assert_eq!(
            refused(runs::start(&tx, writer, &start_of(CaseKind::Unit), 2_001)),
            run_codes::RUN_WRITER_REFUSED,
            "{writer}"
        );
    }
    assert_eq!(count(&tx, "tk_runs"), runs_before);

    let run = started(&tx, &engine, 2_002);
    for writer in [&agent, &master] {
        let rows = vec![result("a::forged", ResultOutcome::Passed)];
        assert_eq!(
            refused(runs::record(&tx, writer, &record_of(&run, rows), 2_003)),
            run_codes::RUN_WRITER_REFUSED
        );
        let finish = finish_of(&run, RunEnd::Completed, Some(0));
        assert_eq!(
            refused(runs::finish(&tx, writer, &finish, 2_004)),
            run_codes::RUN_WRITER_REFUSED
        );
        let cancel = RunCancel {
            id: run.id.clone(),
            version: run.version,
            reason: StopReason::Cancelled,
        };
        assert_eq!(
            refused(runs::cancel(&tx, writer, &cancel, 2_005)),
            run_codes::RUN_WRITER_REFUSED
        );
    }
    let finish = finish_of(&by_agent, RunEnd::Completed, Some(0));
    assert_eq!(
        refused(runs::finish(&tx, &agent, &finish, 2_006)),
        run_codes::RUN_WRITER_REFUSED
    );
    assert_eq!(count(&tx, "tk_events"), events_before + 1);

    let run = recorded(
        &tx,
        &engine,
        &run,
        &[("a::passes", ResultOutcome::Passed)],
        2_007,
    );
    assert_eq!(
        finished(&tx, &engine, &run, Some(0), 2_008).verdict,
        Some(Verdict::Passed)
    );

    let manual = runs::start(&tx, &owner, &start_of(CaseKind::Manual), 2_009).unwrap();
    assert_eq!(manual.policy.kind, CaseKind::Manual);
    let manual = recorded(
        &tx,
        &owner,
        &manual,
        &[("manual::login", ResultOutcome::Passed)],
        2_010,
    );
    assert_eq!(
        finished(&tx, &owner, &manual, None, 2_011).verdict,
        Some(Verdict::Passed)
    );
    for writer in [&agent, &engine] {
        assert_eq!(
            refused(runs::start(&tx, writer, &start_of(CaseKind::Manual), 2_012)),
            run_codes::RUN_WRITER_REFUSED,
            "{writer}"
        );
    }

    set_policy(&tx, "run_writers", RunWriters::Any.as_str());
    assert_eq!(started(&tx, &master, 2_013).recorded_by, master);
}

#[test]
fn a_bad_source_or_policy_shape_is_refused_plainly() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = opened(dir.path());
    let tx = store.transaction().unwrap();
    project(&tx);
    let engine = actor("engine:checks");

    let good_source = serde_json::to_string(&source()).unwrap();
    assert_eq!(SourceRef::parse(&good_source).unwrap(), source());
    let declared = SourceRef::parse(r#"{"kind":"declared","ref":"nightly 2026-11-02"}"#).unwrap();
    assert_eq!(
        declared,
        SourceRef::Declared {
            reference: "nightly 2026-11-02".to_owned()
        }
    );
    let commit = hex('a', 40);
    let bad_sources = [
        r#"{"kind":"svn","ref":"r12"}"#.to_owned(),
        r#"{"kind":"declared","ref":"x","extra":1}"#.to_owned(),
        r#"{"kind":"declared"}"#.to_owned(),
        format!(
            r#"{{"kind":"git","repository":"r","commit":"{commit}","base":null,"change_set":null}}"#
        ),
        good_source.replace(&hex('a', 40), &hex('a', 39)),
        good_source.replace(&hex('b', 40), &hex('B', 40)),
        good_source.replace(&hex('c', 40), &hex('g', 40)),
        good_source.replace("crazyagent", &"r".repeat(201)),
        good_source.replace("crazyagent", "crazy\\u202eagent"),
        r#"{"kind":"declared","ref":""}"#.to_owned(),
        format!(r#"{{"kind":"declared","ref":"{}"}}"#, "x".repeat(513)),
        "[]".to_owned(),
        "{".to_owned(),
    ];
    for text in &bad_sources {
        let refusal = SourceRef::parse(text).expect_err(text);
        assert_eq!(refusal.code, run_codes::RUN_SOURCE_INVALID, "{text}");
        assert!(!refusal.message.is_empty());
    }

    let good_policy = serde_json::to_string(&policy(CaseKind::Unit)).unwrap();
    assert_eq!(
        PolicyRef::parse(&good_policy).unwrap(),
        policy(CaseKind::Unit)
    );
    let long_argv: Vec<String> = (0..9).map(|_| "a".repeat(500)).collect();
    let mut over_bytes = policy(CaseKind::Unit);
    over_bytes.argv = long_argv;
    let bad_policies = [
        good_policy.replace(&hex('d', 64), &hex('d', 63)),
        good_policy.replace(&hex('e', 64), &hex('z', 64)),
        good_policy.replace(&hex('f', 64), &hex('F', 64)),
        good_policy.replace("\"off\"", "\"anywhere\""),
        good_policy.replace("\"rust\"", "\"cobol\""),
        good_policy.replace("\"unit\",\"kind\"", "\"unit\",\"extra\":1,\"kind\""),
        good_policy.replace("1800", "7201"),
        good_policy.replace("\"--locked\"", "\"--lo\\u200bcked\""),
        serde_json::to_string(&over_bytes).unwrap(),
    ];
    for text in &bad_policies {
        let refusal = PolicyRef::parse(text).expect_err(text);
        assert_eq!(refusal.code, run_codes::RUN_POLICY_INVALID, "{text}");
    }
    let mut many_argv = policy(CaseKind::Unit);
    many_argv.argv = (0..33).map(|index| format!("a{index}")).collect();
    let mut long_item = policy(CaseKind::Unit);
    long_item.argv = vec!["a".repeat(513)];
    let mut empty_check = policy(CaseKind::Unit);
    empty_check.check = String::new();

    let runs_before = count(&tx, "tk_runs");
    let events_before = count(&tx, "tk_events");
    for bad in [many_argv, long_item, empty_check, over_bytes] {
        let mut start = start_of(CaseKind::Unit);
        start.policy = bad;
        assert_eq!(
            refused(runs::start(&tx, &engine, &start, 2_000)),
            run_codes::RUN_POLICY_INVALID
        );
    }
    let mut short_commit = start_of(CaseKind::Unit);
    short_commit.source = SourceRef::Git {
        repository: "crazyagent".to_owned(),
        commit: hex('a', 12),
        tree: hex('b', 40),
        base: None,
        change_set: None,
    };
    assert_eq!(
        refused(runs::start(&tx, &engine, &short_commit, 2_000)),
        run_codes::RUN_SOURCE_INVALID
    );
    let mut bad_batch = start_of(CaseKind::Unit);
    bad_batch.batch = Some("trigger 7".to_owned());
    assert_eq!(
        refused(runs::start(&tx, &engine, &bad_batch, 2_000)),
        run_codes::RUN_BATCH_INVALID
    );
    let mut no_plan = start_of(CaseKind::Unit);
    no_plan.plan = Some(TestId::parse_of("CA-P9", TestObject::Plan).unwrap());
    assert_eq!(
        refused(runs::start(&tx, &engine, &no_plan, 2_000)),
        run_codes::PLAN_UNKNOWN
    );
    assert_eq!(count(&tx, "tk_runs"), runs_before);
    assert_eq!(count(&tx, "tk_events"), events_before);

    let run = started(&tx, &engine, 2_001);
    let integrity = IntegrityRef {
        executed: 1,
        inventory: Inventory {
            baseline_run: Some(run_id("CA-R1")),
            removed: vec!["a::gone".to_owned()],
            removed_total: 1,
        },
        new_skips: vec![NewSkip {
            path: "tests/a.rs".to_owned(),
            line: 7,
            marker: "#[ignore]".to_owned(),
        }],
        protected_touched: Vec::new(),
        holdouts_ran: false,
        build_ok: true,
    };
    let good_integrity = serde_json::to_string(&integrity).unwrap();
    assert_eq!(IntegrityRef::parse(&good_integrity).unwrap(), integrity);
    let mut too_few = integrity.clone();
    too_few.inventory.removed_total = 0;
    let mut too_many = integrity.clone();
    too_many.new_skips = vec![integrity.new_skips[0].clone(); 51];
    let mut no_line = integrity.clone();
    no_line.new_skips[0].line = 0;
    for bad in [too_few, too_many, no_line] {
        let mut finish = finish_of(&run, RunEnd::Completed, Some(0));
        finish.integrity = Some(bad);
        assert_eq!(
            refused(runs::finish(&tx, &engine, &finish, 2_002)),
            run_codes::RUN_INTEGRITY_INVALID
        );
    }
    let unknown_field = good_integrity.replace("\"executed\"", "\"extra\":1,\"executed\"");
    assert_eq!(
        IntegrityRef::parse(&unknown_field).unwrap_err().code,
        run_codes::RUN_INTEGRITY_INVALID
    );
    let mut finish = finish_of(&run, RunEnd::Completed, Some(0));
    finish.integrity = Some(integrity.clone());
    let run = runs::finish(&tx, &engine, &finish, 2_003).unwrap();
    assert_eq!(run.integrity, Some(integrity));
}

fn evidence_on(tx: &Transaction<'_>, ticket: &trackwright_core::model::ShortId, reference: &str) {
    store::add_criterion(tx, &actor("owner"), ticket, "runs prove it", 3_000).unwrap();
    let criterion_id: i64 = tx
        .query_row("SELECT max(id) FROM tk_criteria", [], |row| row.get(0))
        .unwrap();
    tx.execute(
        "INSERT INTO tk_checks(criterion, kind, ref) VALUES (?1, 'ci', ?2)",
        (criterion_id, reference),
    )
    .unwrap();
    tx.execute(
        "INSERT INTO tk_evidence(check_id, result, ref, recorded_by, recorded_ms)
         VALUES (last_insert_rowid(), 'pass', ?1, 'ci', 3001)",
        [reference],
    )
    .unwrap();
}

#[test]
fn retention_never_prunes_a_run_that_evidence_refers_to() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = opened(dir.path());
    let tx = store.transaction().unwrap();
    project(&tx);
    let engine = actor("engine:checks");
    let owner = actor("owner");
    let ticket = store::create_ticket(
        &tx,
        &owner,
        &key(),
        &store::NewTicket {
            title: "Prove runs".to_owned(),
            body: String::new(),
            kind: trackwright_core::model::TicketKind::Feature,
            status: trackwright_core::model::TicketStatus::Backlog,
            priority: trackwright_core::model::Priority::new(2).unwrap(),
        },
        1_100,
    )
    .unwrap();
    tests_store::create_case(
        &tx,
        &owner,
        &key(),
        &NewCase {
            title: "Latest result".to_owned(),
            kind: CaseKind::Unit,
            stable_name: Some("keep::latest".to_owned()),
            steps: Vec::new(),
            protected: false,
            status: CaseStatus::Ready,
        },
        1_200,
    )
    .unwrap();
    set_policy(&tx, "run_retention", RunRetention::PruneAge.as_str());

    let old_ms = 10_000;
    for number in 1..=5 {
        let run = started(&tx, &engine, old_ms + number);
        let name = if number == 4 {
            "keep::latest"
        } else {
            "other::name"
        };
        let run = recorded(
            &tx,
            &engine,
            &run,
            &[(name, ResultOutcome::Passed)],
            old_ms + number,
        );
        finished(&tx, &engine, &run, Some(0), old_ms + number);
    }
    let running = started(&tx, &engine, old_ms + 6);
    evidence_on(&tx, &ticket.short_id, "CA-R1");
    store::add_link(&tx, &owner, &ticket.short_id, "run", "CA-R2", 3_002).unwrap();
    assert_eq!(run_numbers(&tx), [1, 2, 3, 4, 5, 6]);

    let now_ms = old_ms + 91 * DAY_MS;
    let newest = started(&tx, &engine, now_ms);
    assert_eq!(newest.id.to_string(), "CA-R7");
    assert_eq!(run_numbers(&tx), [1, 2, 4, 6, 7]);
    let pruned: i64 = tx
        .query_row(
            "SELECT count(*) FROM tk_events WHERE op = ?1",
            [runs::RUN_PRUNE],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(pruned, 2);
    let orphans: i64 = tx
        .query_row(
            "SELECT count(*) FROM tk_results WHERE run_id NOT IN (SELECT id FROM tk_runs)",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(orphans, 0);
    assert_eq!(
        runs::stored(&tx, &key()).unwrap(),
        runs::replay(&tx, &key()).unwrap()
    );
    assert_eq!(
        refused(runs::show(
            &tx,
            &RunShow {
                id: run_id("CA-R3"),
                after: None
            }
        )),
        run_codes::RUN_UNKNOWN
    );
    assert_eq!(running.state, RunState::Running);

    set_policy(&tx, "run_retention", RunRetention::KeepAll.as_str());
    let old = started(&tx, &engine, old_ms + 7);
    let old = recorded(
        &tx,
        &engine,
        &old,
        &[("other::name", ResultOutcome::Passed)],
        old_ms + 7,
    );
    finished(&tx, &engine, &old, Some(0), old_ms + 7);
    assert_eq!(started(&tx, &engine, now_ms + 1).id.to_string(), "CA-R9");
    assert_eq!(run_numbers(&tx), [1, 2, 4, 6, 7, 8, 9]);
}

#[test]
fn retention_past_the_count_prunes_the_oldest_unreferenced_runs_first() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = opened(dir.path());
    let tx = store.transaction().unwrap();
    project(&tx);
    let engine = actor("engine:checks");
    let template = started(&tx, &engine, 2_000);
    let template = finished(&tx, &engine, &template, Some(0), 2_001);
    assert_eq!(template.verdict, Some(Verdict::NoTests));
    let filler = RUNS_RETAINED_PER_PROJECT_MAX as i64 + 2;
    tx.execute(
        "WITH RECURSIVE n(i) AS (SELECT 2 UNION ALL SELECT i + 1 FROM n WHERE i < ?1)
         INSERT INTO tk_runs(project, number, plan_id, batch, title, state, verdict, reason,
             source_json, policy_json, integrity_json, started_ms, finished_ms, recorded_by,
             total, passed, failed, skipped, errored, version)
         SELECT r.project, n.i, NULL, r.batch, r.title, r.state, r.verdict, NULL,
             r.source_json, r.policy_json, NULL, 2000 + n.i, 2000 + n.i, r.recorded_by,
             0, 0, 0, 0, 0, 1
         FROM n, tk_runs r WHERE r.number = 1",
        [filler],
    )
    .unwrap();
    assert_eq!(count(&tx, "tk_runs"), filler);

    let newest = started(&tx, &engine, 10_000_000);
    assert_eq!(newest.id.to_string(), format!("CA-R{}", filler + 1));
    assert_eq!(count(&tx, "tk_runs"), RUNS_RETAINED_PER_PROJECT_MAX as i64);
    let numbers = run_numbers(&tx);
    assert_eq!(numbers[0], 4);
    assert_eq!(*numbers.last().unwrap(), filler + 1);
}

#[test]
fn every_run_statement_prepares_against_a_database_holding_only_the_core_schema() {
    let dir = tempfile::tempdir().unwrap();
    let mut connection = Connection::open(dir.path().join("core.db")).unwrap();
    let tx = connection.transaction().unwrap();
    schema::migrate(&tx).unwrap();
    for statement in runs::statements() {
        tx.prepare(statement)
            .unwrap_or_else(|error| panic!("{statement}: {error}"));
    }
    assert!(runs::statements().len() >= 10);
}

#[test]
fn a_write_that_fails_part_way_leaves_no_rows() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = opened(dir.path());
    let tx = store.transaction().unwrap();
    project(&tx);
    let engine = actor("engine:checks");
    let run = started(&tx, &engine, 2_000);
    tx.execute_batch(
        "CREATE TRIGGER abort_result BEFORE INSERT ON tk_results
             WHEN NEW.name = 'a::aborts'
             BEGIN SELECT RAISE(ABORT, 'result insert aborted'); END;
         CREATE TRIGGER abort_event BEFORE INSERT ON tk_events
             WHEN NEW.op IN ('run.start', 'run.finish')
             BEGIN SELECT RAISE(ABORT, 'event insert aborted'); END;",
    )
    .unwrap();
    let runs_before = count(&tx, "tk_runs");
    let events_before = count(&tx, "tk_events");

    let rows = vec![
        result("a::lands_first", ResultOutcome::Passed),
        result("a::aborts", ResultOutcome::Passed),
    ];
    let failed = runs::record(&tx, &engine, &record_of(&run, rows), 2_001);
    assert!(matches!(failed, Err(StoreError::Sqlite(_))), "{failed:?}");
    assert_eq!(count(&tx, "tk_results"), 0);

    let failed = runs::start(&tx, &engine, &start_of(CaseKind::Unit), 2_002);
    assert!(matches!(failed, Err(StoreError::Sqlite(_))), "{failed:?}");
    assert_eq!(count(&tx, "tk_runs"), runs_before);

    let finish = finish_of(&run, RunEnd::Completed, Some(0));
    let failed = runs::finish(&tx, &engine, &finish, 2_003);
    assert!(matches!(failed, Err(StoreError::Sqlite(_))), "{failed:?}");

    assert_eq!(count(&tx, "tk_events"), events_before);
    tx.commit().unwrap();
    let tx = store.transaction().unwrap();
    assert_eq!(count(&tx, "tk_results"), 0);
    assert_eq!(count(&tx, "tk_runs"), runs_before);
    let shown = runs::show(
        &tx,
        &RunShow {
            id: run.id.clone(),
            after: None,
        },
    )
    .unwrap();
    assert_eq!(shown.run.state, RunState::Running);
    assert_eq!(shown.run.version, run.version);
    assert_eq!(shown.run.totals.total, 0);
}

#[test]
fn a_refused_record_keeps_the_run_running_and_an_unreadable_report_is_unknown() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = opened(dir.path());
    let tx = store.transaction().unwrap();
    project(&tx);
    let engine = actor("engine:checks");
    let run = started(&tx, &engine, 2_000);
    let run = recorded(
        &tx,
        &engine,
        &run,
        &[("a::first", ResultOutcome::Passed)],
        2_001,
    );
    let events_before = count(&tx, "tk_events");

    let rows = vec![
        result("a::second", ResultOutcome::Passed),
        result("a::\u{7}bell", ResultOutcome::Failed),
    ];
    let code = refused(runs::record(&tx, &engine, &record_of(&run, rows), 2_002));
    assert_eq!(code, codes::TEXT_HIDDEN_CHARACTERS);
    assert_eq!(count(&tx, "tk_results"), 1);
    assert_eq!(count(&tx, "tk_events"), events_before);
    let shown = runs::show(
        &tx,
        &RunShow {
            id: run.id.clone(),
            after: None,
        },
    )
    .unwrap();
    assert_eq!(shown.run.state, RunState::Running);
    assert_eq!(shown.run.version, run.version);

    let why = "result 2 of the report has a hidden character in its name";
    let mut finish = finish_of(&run, RunEnd::ReportUnreadable, Some(1));
    finish.reason = Some(why.to_owned());
    let unknown = runs::finish(&tx, &engine, &finish, 2_003).unwrap();
    assert_eq!(unknown.state, RunState::Finished);
    assert_eq!(unknown.verdict, Some(Verdict::Unknown));
    assert_eq!(unknown.reason.as_deref(), Some(why));
    assert_eq!(unknown.totals.total, 1);
    assert!(!unknown.verdict.unwrap().satisfies());

    let other = started(&tx, &engine, 2_004);
    let mut broken = finish_of(&other, RunEnd::ReportUnreadable, None);
    broken.reason = Some("two\nlines".to_owned());
    assert_eq!(
        refused(runs::finish(&tx, &engine, &broken, 2_005)),
        codes::TEXT_LINE_BREAK
    );
    let mut completed = finish_of(&other, RunEnd::Completed, Some(0));
    completed.reason = Some(why.to_owned());
    assert_eq!(
        refused(runs::finish(&tx, &engine, &completed, 2_006)),
        run_codes::RUN_FINISH_INVALID
    );
}

#[test]
fn the_standalone_host_shows_every_actor_each_run_in_full() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = opened(dir.path());
    let tx = store.transaction().unwrap();
    project(&tx);
    let engine = actor("engine:checks");
    let run = started(&tx, &engine, 2_000);
    let run = recorded(
        &tx,
        &engine,
        &run,
        &[("hidden::answers", ResultOutcome::Failed)],
        2_001,
    );
    let run = finished(&tx, &engine, &run, Some(101), 2_002);
    assert_eq!(run.verdict, Some(Verdict::Failed));
    let show = RunShow {
        id: run.id.clone(),
        after: None,
    };
    for reader in ["agent:1", "owner"] {
        let reader = actor(reader);
        let sight = Standalone.run_sight(&tx, &key(), &reader, &run.id).unwrap();
        assert_eq!(sight, Sight::Full);
        assert_eq!(
            runs::show_as(&tx, &Standalone, &reader, &show).unwrap(),
            Sighted::Full(Box::new(runs::show(&tx, &show).unwrap()))
        );
        assert_eq!(
            runs::list_as(&tx, &Standalone, &reader, &list_of(None)).unwrap(),
            runs::list(&tx, &list_of(None)).unwrap()
        );
    }
}
