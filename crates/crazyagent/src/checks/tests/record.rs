use crate::checks::queue::{self, Job, JobState};
use crate::checks::record::{self, CheckRun, Recording};
use crate::checks::runner::{self, RunLimits};
use crate::checks::snapshot::{self, RunKey};
use crate::checks::{self, Actor, AdmittedProfile, NetworkMode, Trigger};
use crate::landing::{self, AgentKind, ChangeAgent, ChangeSetId, TurnEnd, TurnOutcome, Workspace};
use crate::projects_home::CRAZYAGENT_DIRECTORY;
use crate::protocol::ProjectId;
use crate::tracker::{self, Host};
use rusqlite::{Connection, Transaction, TransactionBehavior};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};
use trackwright_core::criteria;
use trackwright_core::host::{Evidence, TicketView};
use trackwright_core::model::{self as core, CheckKind as CoreCheck, ProjectKey, ShortId};
use trackwright_core::ops;
use trackwright_core::requests::Request;
use trackwright_core::runs::{
    self, NewResult, ResultOutcome, RunEnd, RunFinish, RunRecord, RunShow, RunStart, RunState,
    RunView, SourceRef, Verdict,
};
use trackwright_core::store::StoreError;
use trackwright_core::tests_model::{CaseKind, CaseStatus};
use trackwright_core::tests_store::{self, NewCase};

const NOW: i64 = 1_800_000_000_000;

/// One owner repository attached to the tracker as `CA`, with one agent worktree added
/// from it and recorded as the engine records it, an isolated home for the test's own git,
/// and an engine runtime holding the real engine schema, the change set store and the
/// checks folder.
struct Case {
    _temp: tempfile::TempDir,
    root: PathBuf,
    repo: PathBuf,
    home: PathBuf,
    engine: PathBuf,
    db: Connection,
    project: ProjectId,
    workspace: Workspace,
}

fn git(home: &Path, dir: &Path, args: &[&str]) -> String {
    let output = crate::spawn_gate::output(
        Command::new("/usr/bin/git")
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", home)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("LC_ALL", "C")
            .args([
                "-c",
                "core.fsmonitor=false",
                "-c",
                "core.hooksPath=/dev/null",
            ])
            .arg("-C")
            .arg(dir)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()),
    )
    .unwrap();
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

fn private(folder: &Path) {
    fs::create_dir_all(folder).unwrap();
    fs::set_permissions(folder, fs::Permissions::from_mode(0o700)).unwrap();
}

fn write(path: &Path, text: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

fn key() -> ProjectKey {
    ProjectKey::new("CA").unwrap()
}

fn owner() -> core::Actor {
    core::Actor::new("owner").unwrap()
}

/// One check of a profile as the owner writes it: the id, the argv, how it reports and
/// its toolchain, run on request from the snapshot root with network off.
fn check(id: &str, argv: &[&str], format: &str, toolchain: Option<&str>) -> Value {
    json!({
        "id": id,
        "kind": "unit",
        "toolchain": toolchain,
        "argv": argv,
        "cwd": ".",
        "format": format,
        "report": null,
        "timeout_s": 600,
        "network": "off",
        "when": ["on_request"],
        "required": true,
        "protected": false
    })
}

impl Case {
    fn new() -> Self {
        let temp = tempfile::tempdir_in("/tmp").unwrap();
        let root = temp.path().canonicalize().unwrap();
        let (repo, home, engine) = (root.join("repo"), root.join("owner"), root.join("engine"));
        for folder in [&repo, &home, &engine, &engine.join("checks")] {
            private(folder);
        }
        git(&home, &repo, &["init", "-q", "--template=", "-b", "main"]);
        git(&home, &repo, &["config", "user.name", "Repo Owner"]);
        git(
            &home,
            &repo,
            &["config", "user.email", "owner@example.invalid"],
        );
        write(&repo.join("README"), "start\n");
        git(&home, &repo, &["add", "README"]);
        git(&home, &repo, &["commit", "-q", "-m", "Start"]);
        let worktree = root.join("agent-1");
        git(
            &home,
            &repo,
            &[
                "worktree",
                "add",
                "-q",
                "--detach",
                worktree.to_str().unwrap(),
                "HEAD",
            ],
        );
        let db = landing::open(&engine).unwrap();
        crate::storage::initialize_authority_schemas(&db).unwrap();
        let project = ProjectId::new("project-1").unwrap();
        let folder = root.join("project-1");
        fs::create_dir_all(folder.join(CRAZYAGENT_DIRECTORY)).unwrap();
        crate::ide::workspace::tests::add_project(&db, project.as_str(), &folder);
        let tx = Transaction::new_unchecked(&db, TransactionBehavior::Immediate).unwrap();
        tracker::attach(&tx, &project, &key(), "One", NOW).unwrap();
        tx.commit().unwrap();
        let workspace = Workspace {
            project: project.clone(),
            agent: ChangeAgent {
                kind: AgentKind::Sub,
                id: "agent-1".into(),
                label: "Agent 1".into(),
            },
            repository: repo.clone(),
            worktree,
            git_dir: repo.join(".git/worktrees/agent-1"),
        };
        landing::record_worktree_start(
            &db,
            &workspace.repository,
            &workspace.worktree,
            &workspace.git_dir,
        )
        .unwrap();
        Self {
            _temp: temp,
            root,
            repo,
            home,
            engine,
            db,
            project,
            workspace,
        }
    }

    /// Writes files into the agent worktree and records the turn end, which commits them
    /// into the worktree's open change set. Input: the files. Output: the change set.
    fn change_set(&self, files: &[(&str, &str)]) -> ChangeSetId {
        for (path, text) in files {
            write(&self.workspace.worktree.join(path), text);
        }
        let turn = TurnEnd {
            outcome: TurnOutcome::Succeeded,
            turn: "turn-1".into(),
            prompt_line: "Add the check fixture".into(),
        };
        landing::record_turn_end(&self.db, &self.workspace, &turn)
            .unwrap()
            .unwrap()
    }

    /// Proposes and approves a profile holding the checks as the owner. Input: the
    /// checks. Output: the admitted profile.
    fn approve(&self, checks: Value) -> AdmittedProfile {
        let body = json!({"version": 1, "checks": checks, "holdouts": [], "protected_paths": []});
        let proposal = checks::propose(
            &self.db,
            &self.project,
            &serde_json::to_vec(&body).unwrap(),
            Actor::Owner,
        )
        .unwrap();
        checks::approve(&self.db, &self.project, &proposal.id, Actor::Owner).unwrap()
    }

    /// Queues every check of the current profile on a change set as the owner asks and
    /// runs the batch to its records. Input: the change set. Output: the jobs as they ended.
    fn run(&self, id: &ChangeSetId) -> Vec<Job> {
        let batch = queue::submit(&self.db, id, Trigger::OnRequest, Actor::Owner, &[]).unwrap();
        let mut ended = Vec::new();
        while let Some(job) = queue::run_next(&self.db, &self.engine).unwrap() {
            assert_eq!(job.batch, batch);
            ended.push(job);
        }
        assert!(!ended.is_empty());
        ended
    }

    fn head(&self, id: &ChangeSetId) -> String {
        landing::change_set(&self.db, id).unwrap().unwrap().1.head
    }

    fn tree(&self, commit: &str) -> String {
        git(
            &self.home,
            &self.repo,
            &["rev-parse", &format!("{commit}^{{tree}}")],
        )
    }

    fn only(&self, id: &ChangeSetId) -> CheckRun {
        let latest = record::latest(&self.db, id).unwrap();
        assert_eq!(latest.len(), 1, "{latest:?}");
        latest.into_iter().next().unwrap()
    }

    fn view(&self, run: &CheckRun) -> RunView {
        let tx = self.db.unchecked_transaction().unwrap();
        runs::show(
            &tx,
            &RunShow {
                id: run.id.clone(),
                after: None,
            },
        )
        .unwrap()
        .run
    }

    fn count(&self, sql: &str) -> i64 {
        self.db.query_row(sql, [], |row| row.get(0)).unwrap()
    }

    /// An owner ticket in `CA` whose one criterion is proven by a test case named after a
    /// check's result, closed by evidence (the default). Input: the result name. Output:
    /// the ticket.
    fn ticket_on(&self, result: &str) -> ShortId {
        let tx = Transaction::new_unchecked(&self.db, TransactionBehavior::Immediate).unwrap();
        let case = tests_store::create_case(
            &tx,
            &owner(),
            &key(),
            &NewCase {
                title: "The unit check passes".into(),
                kind: CaseKind::Unit,
                stable_name: Some(result.into()),
                steps: Vec::new(),
                protected: false,
                status: CaseStatus::Ready,
            },
            NOW,
        )
        .unwrap();
        let request = Request::parse(
            "create",
            &serde_json::to_vec(&json!({"project": "CA", "title": "Checks pass"})).unwrap(),
        )
        .unwrap();
        let reply: Value =
            serde_json::from_str(ops::dispatch(&tx, &Host, &owner(), request).json()).unwrap();
        assert_eq!(reply["ok"], true, "{reply}");
        let ticket = ShortId::parse(reply["data"]["id"].as_str().unwrap()).unwrap();
        let criterion =
            criteria::add_criterion(&tx, &Host, &owner(), &ticket, "The unit check passed", NOW)
                .unwrap();
        criteria::add_check(
            &tx,
            &Host,
            &owner(),
            &ticket,
            criterion.position,
            CoreCheck::Test,
            &String::from(case.short_id),
            NOW + 1,
        )
        .unwrap();
        tx.commit().unwrap();
        ticket
    }

    fn closed(&self, ticket: &ShortId) -> Option<core::Actor> {
        let tx = self.db.unchecked_transaction().unwrap();
        trackwright_core::store::ticket(&tx, ticket)
            .unwrap()
            .closed_by
    }
}

#[test]
fn a_passing_check_run_records_the_change_set_tree_and_the_approved_profile() {
    let case = Case::new();
    let id = case.change_set(&[("pass.sh", "exit 0\n")]);
    let admitted = case.approve(json!([check(
        "unit",
        &["sh", "pass.sh"],
        "exit_code",
        None
    )]));
    let jobs = case.run(&id);
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].state, JobState::Recorded);
    let run = case.only(&id);
    assert_eq!(jobs[0].run, Some(run.run));
    assert_eq!(run.check, "unit");
    assert_eq!(run.state, RunState::Finished);
    assert_eq!(run.verdict, Some(Verdict::Passed));
    let head = case.head(&id);
    assert_eq!(run.commit, head);
    assert_eq!(run.tree, case.tree(&head));
    assert_eq!(run.profile, admitted.id);
    assert_eq!(run.profile_approved_ms, admitted.approved_ms);
    assert!(!run.holdout);
    let view = case.view(&run);
    assert_eq!(view.batch.as_deref(), Some(jobs[0].batch.as_str()));
    assert_eq!(view.recorded_by.as_str(), runs::ENGINE_ACTOR);
    let base = landing::change_set(&case.db, &id).unwrap().unwrap().1.base;
    assert_eq!(
        view.source,
        SourceRef::Git {
            repository: case.project.as_str().into(),
            commit: head.clone(),
            tree: case.tree(&head),
            base: Some(base),
            change_set: Some(id.as_str().into()),
        }
    );
    assert_eq!(view.policy.profile, admitted.id.as_str());
    assert_eq!(view.policy.profile_approved_ms, admitted.approved_ms);
    assert_eq!(view.policy.check, "unit");
    assert_eq!(view.policy.argv, ["sh", "pass.sh"]);
    assert_eq!(view.policy.network, runs::NetworkMode::Off);
    assert_eq!(view.policy.timeout_s, 600);
    assert_eq!(view.totals.passed, 1);
}

#[test]
fn the_last_passing_run_closes_the_linked_ticket_in_the_same_transaction() {
    let case = Case::new();
    let id = case.change_set(&[("pass.sh", "exit 0\n")]);
    case.approve(json!([check(
        "unit",
        &["sh", "pass.sh"],
        "exit_code",
        None
    )]));
    let ticket = case.ticket_on("unit");
    assert_eq!(case.closed(&ticket), None);
    case.run(&id);
    assert_eq!(case.only(&id).verdict, Some(Verdict::Passed));
    assert_eq!(
        case.closed(&ticket).map(|actor| actor.as_str().to_owned()),
        Some("host:evidence".to_owned())
    );
}

#[test]
fn a_rolled_back_recording_leaves_no_run_and_no_ticket_change() {
    let case = Case::new();
    let id = case.change_set(&[("pass.sh", "exit 0\n")]);
    let admitted = case.approve(json!([check(
        "unit",
        &["sh", "pass.sh"],
        "exit_code",
        None
    )]));
    let ticket = case.ticket_on("unit");
    let check = admitted.checks().into_iter().next().unwrap();
    let snapshot = snapshot::prepare(
        &case.db,
        &case.engine.join("checks"),
        &id,
        &RunKey::new("run-1").unwrap(),
    )
    .unwrap();
    let execution = runner::run(
        &snapshot,
        &check,
        &BTreeMap::new(),
        &RunLimits {
            timeout: Duration::from_secs(120),
            output_cap: runner::OUTPUT_CAP,
            report_cap: runner::REPORT_CAP,
            network_ceiling: NetworkMode::Remotes,
            remotes: Vec::new(),
        },
        &AtomicBool::new(false),
    );
    assert_eq!(execution.exit, Some(0));
    let batch = queue::BatchId::new("b0123456789abcdef").unwrap();
    let events = case.count("SELECT COUNT(*) FROM tk_events");
    let tx = Transaction::new_unchecked(&case.db, TransactionBehavior::Immediate).unwrap();
    let stored = record::store(
        &tx,
        &Host,
        &Recording {
            change_set: &id,
            batch: &batch,
            snapshot: &snapshot,
            check: &check,
            execution: &execution,
            integrity: None,
        },
        NOW + 100,
    )
    .unwrap();
    assert_eq!(stored.verdict, Some(Verdict::Passed));
    assert_eq!(record::latest(&tx, &id).unwrap().len(), 1);
    tx.rollback().unwrap();
    assert_eq!(case.count("SELECT COUNT(*) FROM tk_runs"), 0);
    assert_eq!(case.count("SELECT COUNT(*) FROM tk_results"), 0);
    assert_eq!(case.count("SELECT COUNT(*) FROM tk_events"), events);
    assert!(record::latest(&case.db, &id).unwrap().is_empty());
    assert_eq!(case.closed(&ticket), None);
    snapshot.remove().unwrap();
}

#[test]
fn a_check_that_ran_zero_tests_records_no_tests() {
    let case = Case::new();
    let id = case.change_set(&[(
        "none.sh",
        "printf 'running 0 tests\\n\\ntest result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s\\n\\n'\n",
    )]);
    case.approve(json!([check("unit", &["sh", "none.sh"], "libtest", None)]));
    case.run(&id);
    let run = case.only(&id);
    assert_eq!(run.state, RunState::Finished);
    assert_eq!(run.verdict, Some(Verdict::NoTests));
    let view = case.view(&run);
    assert_eq!(view.totals.executed(), 0);
    assert!(!view.verdict.unwrap().satisfies());
    assert_eq!(view.integrity.map(|integrity| integrity.executed), Some(0));
}

#[test]
fn a_rust_check_that_fails_to_compile_records_build_failed() {
    let case = Case::new();
    let id = case.change_set(&[
        ("rust-toolchain.toml", "[toolchain]\nchannel = \"1.98.1\"\n"),
        (
            "Cargo.toml",
            "[package]\nname = \"broken\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        ),
        (
            "src/lib.rs",
            "pub fn answer() -> u32 {\n    \"forty-two\"\n}\n\n#[test]\nfn answers() {\n    assert_eq!(answer(), 42);\n}\n",
        ),
    ]);
    case.approve(json!([check(
        "unit",
        &["cargo", "test", "--offline"],
        "libtest",
        Some("rust")
    )]));
    case.run(&id);
    let run = case.only(&id);
    assert_eq!(run.state, RunState::Finished);
    assert_eq!(run.verdict, Some(Verdict::BuildFailed), "{run:?}");
    let view = case.view(&run);
    assert!(view.policy.toolchain.version.starts_with("rustc 1.98.1"));
    assert_eq!(
        view.integrity.map(|integrity| integrity.build_ok),
        Some(false)
    );
    assert_eq!(view.totals.failed, 0);
}

#[test]
fn discarding_a_change_set_cancels_its_running_check() {
    let case = Case::new();
    let id = case.change_set(&[("slow.sh", "exec /bin/sleep 120\n")]);
    case.approve(json!([check(
        "unit",
        &["sh", "slow.sh"],
        "exit_code",
        None
    )]));
    queue::submit(&case.db, &id, Trigger::OnRequest, Actor::Owner, &[]).unwrap();
    let head = case.head(&id);
    let started = Instant::now();
    let ended = std::thread::scope(|scope| {
        let worker = scope.spawn(|| {
            let db = landing::open(&case.engine).unwrap();
            queue::run_next(&db, &case.engine)
        });
        let deadline = Instant::now() + Duration::from_secs(60);
        while queue::jobs(&case.db, &id).unwrap()[0].state != JobState::Running {
            assert!(Instant::now() < deadline, "the job never started");
            std::thread::sleep(Duration::from_millis(50));
        }
        std::thread::sleep(Duration::from_secs(2));
        landing::discard(&case.db, &id, &head, NOW as u64 + 1_000).unwrap();
        worker.join().unwrap()
    });
    let job = ended.unwrap().unwrap();
    assert!(started.elapsed() < Duration::from_secs(60));
    assert_eq!(job.state, JobState::Recorded);
    let run = case.only(&id);
    assert_eq!(run.state, RunState::Interrupted);
    assert_eq!(run.verdict, None);
    assert_eq!(case.view(&run).reason.as_deref(), Some("cancelled"));
    assert!(queue::run_next(&case.db, &case.engine).unwrap().is_none());
}

#[test]
fn the_environment_digest_covers_names_and_values_and_the_run_stores_no_value() {
    let case = Case::new();
    let id = case.change_set(&[("pass.sh", "exit 0\n")]);
    case.approve(json!([check(
        "unit",
        &["sh", "pass.sh"],
        "exit_code",
        None
    )]));
    case.run(&id);
    let first = case.view(&case.only(&id));
    case.run(&id);
    let second = case.view(&case.only(&id));
    assert_ne!(first.id, second.id);
    for view in [&first, &second] {
        let digest = &view.policy.environment;
        assert_eq!(digest.len(), 64);
        assert!(
            digest
                .bytes()
                .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
        );
    }
    assert_ne!(
        first.policy.environment, second.policy.environment,
        "each run's HOME differs, so its digest does"
    );
    let checks_folder = case.engine.join("checks");
    let needle = checks_folder.to_str().unwrap();
    for (table, columns) in [
        (
            "tk_runs",
            "source_json || policy_json || coalesce(integrity_json, '') || coalesce(reason, '')",
        ),
        (
            "tk_results",
            "name || coalesce(message, '') || coalesce(artifact, '')",
        ),
        (
            "tk_events",
            "coalesce(before_json, '') || coalesce(after_json, '')",
        ),
        (
            "check_jobs",
            "batch || change_set || check_id || coalesce(reason, '')",
        ),
    ] {
        let sql = format!("SELECT COUNT(*) FROM {table} WHERE instr({columns}, ?1) > 0");
        let found: i64 = case.db.query_row(&sql, [needle], |row| row.get(0)).unwrap();
        assert_eq!(found, 0, "{table} stores an environment value");
    }
    assert!(case.root.starts_with("/private/tmp"));
}

#[test]
fn the_host_names_the_linked_change_set_head_as_the_current_source() {
    let case = Case::new();
    let id = case.change_set(&[("pass.sh", "exit 0\n")]);
    case.approve(json!([check(
        "unit",
        &["sh", "pass.sh"],
        "exit_code",
        None
    )]));
    let ticket = case.ticket_on("unit");
    let tx = Transaction::new_unchecked(&case.db, TransactionBehavior::Immediate).unwrap();
    let args =
        json!({"id": String::from(ticket.clone()), "kind": "change_set", "target": id.as_str()});
    let request = Request::parse("link", &serde_json::to_vec(&args).unwrap()).unwrap();
    let reply: Value =
        serde_json::from_str(ops::dispatch(&tx, &Host, &owner(), request).json()).unwrap();
    assert_eq!(reply["ok"], true, "{reply}");
    let view = TicketView::of(&trackwright_core::store::ticket(&tx, &ticket).unwrap());
    assert!(Host.tracks_source());
    assert_eq!(
        Host.current_source(&tx, &key(), &view).unwrap(),
        None,
        "no check has recorded the head's tree yet"
    );
    tx.commit().unwrap();
    case.run(&id);
    let recorded = case.view(&case.only(&id)).source;
    let tx = case.db.unchecked_transaction().unwrap();
    let current = Host.current_source(&tx, &key(), &view).unwrap();
    assert_eq!(current, Some(recorded));
    let Some(SourceRef::Git { commit, .. }) = current else {
        panic!("the engine records git sources");
    };
    assert_eq!(commit, case.head(&id));
}

/// Writes one passing run as `actor` straight through the core, with the source and policy
/// of a run the engine recorded, as an agent forging a pass would. Inputs: the case, the
/// actor and the engine's run. Output: the forged run, or the core's refusal.
fn forge(case: &Case, actor: &str, like: &RunView) -> Result<RunView, StoreError> {
    let actor = core::Actor::new(actor).unwrap();
    let tx = Transaction::new_unchecked(&case.db, TransactionBehavior::Immediate).unwrap();
    let started = runs::start(
        &tx,
        &actor,
        &RunStart {
            project: key(),
            title: "unit".into(),
            plan: None,
            batch: like.batch.clone(),
            source: like.source.clone(),
            policy: like.policy.clone(),
        },
        NOW + 5_000,
    )?;
    let recorded = runs::record(
        &tx,
        &actor,
        &RunRecord {
            id: started.id.clone(),
            version: started.version,
            results: vec![NewResult {
                name: "unit".into(),
                outcome: ResultOutcome::Passed,
                duration_ms: Some(1),
                message: None,
                artifact: None,
            }],
        },
        NOW + 5_001,
    )?;
    let finished = runs::finish(
        &tx,
        &actor,
        &RunFinish {
            id: recorded.id.clone(),
            version: recorded.version,
            end: RunEnd::Completed,
            exit_code: Some(0),
            reason: None,
            integrity: None,
        },
        NOW + 5_002,
    )?;
    tx.commit().unwrap();
    Ok(finished)
}

#[test]
fn only_the_engine_path_counts_whatever_the_run_writers_policy() {
    let case = Case::new();
    let id = case.change_set(&[("pass.sh", "exit 0\n")]);
    case.approve(json!([check(
        "unit",
        &["sh", "pass.sh"],
        "exit_code",
        None
    )]));
    case.run(&id);
    let engine = case.only(&id);
    let engine_view = case.view(&engine);
    let forged = forge(&case, "agent:1", &engine_view).expect("run_writers defaults to any");
    assert_eq!(forged.verdict, Some(Verdict::Passed));
    assert_eq!(forged.recorded_by.as_str(), "agent:1");
    assert_ne!(forged.id, engine.id);
    assert_eq!(
        case.only(&id),
        engine,
        "a run the engine did not record never counts"
    );
    let tracker_project: i64 = case
        .db
        .query_row("SELECT id FROM tk_projects WHERE key = 'CA'", [], |row| {
            row.get(0)
        })
        .unwrap();
    case.db
        .execute(
            "INSERT INTO tk_policy(project, key, value_json, version)
             VALUES (?1, 'run_writers', '\"engine_only\"', 1)",
            [tracker_project],
        )
        .unwrap();
    for actor in ["agent:1", "owner"] {
        let Err(StoreError::Refused(refusal)) = forge(&case, actor, &engine_view) else {
            panic!("{actor} writes an automated run under engine_only");
        };
        assert_eq!(refusal.code, "run_writer_refused", "{actor}");
    }
    case.run(&id);
    let again = case.only(&id);
    assert_ne!(again.run, engine.run);
    assert_eq!(again.verdict, Some(Verdict::Passed));
    assert_eq!(case.view(&again).recorded_by.as_str(), runs::ENGINE_ACTOR);
}
