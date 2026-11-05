use super::*;
use crate::checks::integrity::{self, Reader, RunSight};
use crate::checks::queue::{self, Job};
use crate::checks::{self as engine_checks, AdmittedProfile, Trigger};
use crate::landing::{self, AgentKind, ChangeAgent, TurnEnd, TurnOutcome, Workspace};
use crate::projects_home::CRAZYAGENT_DIRECTORY;
use crate::team::tool::{
    CALLS_PER_TICK_MAX, CHECKS_RUN_INVALID, MAILBOX_FOLDER, answer_call, checked, serve_live,
    take_calls,
};
use crate::tracker;
use rusqlite::{Transaction, TransactionBehavior};
use serde_json::json;
use std::collections::BTreeSet;
use std::fs;
use std::io::Cursor;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use trackwright_core::model::{Actor as CoreActor, ProjectKey};
use trackwright_core::tests_model::{CaseKind, CaseStatus};
use trackwright_core::tests_store::{self, NewCase};

const NOW: i64 = 1_800_000_000_000;
const SERVED_WITHIN: Duration = Duration::from_secs(60);
const ORACLE_FAILS: &str = "printf 'running 1 test\\ntest oracle::answers ... FAILED\\n\\nfailures:\\n\\n---- oracle::answers stdout ----\\nexpected 42, got 41\\n\\n\\nfailures:\\n    oracle::answers\\n\\ntest result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s\\n\\n'\nexit 101\n";

/// One owner repository attached to the tracker as `CA` in project `project-1`, an
/// isolated home for the test's own git, and an engine runtime holding the real engine
/// schema, the change set store and the checks folder. Team agents get worktrees of the
/// repository as the engine adds them.
struct Team {
    _temp: tempfile::TempDir,
    root: PathBuf,
    repo: PathBuf,
    home: PathBuf,
    engine: PathBuf,
    db: Connection,
    project: ProjectId,
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

/// One check of a profile as the owner writes it: the id, the argv and how it reports,
/// run on request from the snapshot root with network off and no toolchain.
fn check(id: &str, argv: &[&str], format: &str) -> Value {
    json!({
        "id": id,
        "kind": "unit",
        "toolchain": null,
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

impl Team {
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
        let db = landing::open(&engine).unwrap();
        crate::storage::initialize_authority_schemas(&db).unwrap();
        let project = ProjectId::new("project-1").unwrap();
        let folder = root.join("project-1");
        fs::create_dir_all(folder.join(CRAZYAGENT_DIRECTORY)).unwrap();
        crate::ide::workspace::tests::add_project(&db, project.as_str(), &folder);
        let tx = Transaction::new_unchecked(&db, TransactionBehavior::Immediate).unwrap();
        tracker::attach(&tx, &project, &key(), "One", NOW).unwrap();
        tx.commit().unwrap();
        Self {
            _temp: temp,
            root,
            repo,
            home,
            engine,
            db,
            project,
        }
    }

    /// Adds a worktree for one team sub-agent as the engine does, writes files into it and
    /// records the turn end, which commits them into the agent's open change set. Inputs:
    /// the team agent id and the files. Output: the change set.
    fn change_set(&self, agent: i64, files: &[(&str, &str)]) -> ChangeSetId {
        let name = format!("agent-{agent}");
        let worktree = self.root.join(&name);
        git(
            &self.home,
            &self.repo,
            &[
                "worktree",
                "add",
                "-q",
                "--detach",
                worktree.to_str().unwrap(),
                "HEAD",
            ],
        );
        let workspace = Workspace {
            project: self.project.clone(),
            agent: ChangeAgent {
                kind: AgentKind::Sub,
                id: agent.to_string(),
                label: format!("Agent {agent}"),
            },
            repository: self.repo.clone(),
            worktree,
            git_dir: self.repo.join(".git/worktrees").join(&name),
        };
        landing::record_worktree_start(
            &self.db,
            &workspace.repository,
            &workspace.worktree,
            &workspace.git_dir,
        )
        .unwrap();
        for (path, text) in files {
            write(&workspace.worktree.join(path), text);
        }
        let turn = TurnEnd {
            outcome: TurnOutcome::Succeeded,
            turn: "turn-1".into(),
            prompt_line: "Add the check fixture".into(),
        };
        let id = landing::record_turn_end(&self.db, &workspace, &turn)
            .unwrap()
            .unwrap();
        let (_, view) = landing::change_set(&self.db, &id).unwrap().unwrap();
        assert_eq!(view.agent.id, agent.to_string());
        id
    }

    /// Proposes and approves a profile holding the checks and holdouts as the owner.
    /// Inputs: the checks and the holdouts. Output: the admitted profile.
    fn approve(&self, checks: Value, holdouts: Value) -> AdmittedProfile {
        let body =
            json!({"version": 1, "checks": checks, "holdouts": holdouts, "protected_paths": []});
        let proposal = engine_checks::propose(
            &self.db,
            &self.project,
            &serde_json::to_vec(&body).unwrap(),
            Actor::Owner,
        )
        .unwrap();
        engine_checks::approve(&self.db, &self.project, &proposal.id, Actor::Owner).unwrap()
    }

    /// Makes the calls through the real live team server over its mailbox, one after the
    /// other as an agent's harness does, while the engine side here takes each call,
    /// checks it again and answers it as `caller`, never starting runners. Inputs: the
    /// caller the engine names and each call's tool and arguments. Output: each call's
    /// result text and whether it is an error, in order.
    fn call(&self, caller: Actor, calls: &[(&str, Value)]) -> Vec<(String, bool)> {
        let folder = tempfile::tempdir_in(&self.root).unwrap();
        let mailbox = folder.path().join(MAILBOX_FOLDER);
        private(&mailbox);
        let mut input = Vec::new();
        for (index, (tool, arguments)) in calls.iter().enumerate() {
            let line = json!({"jsonrpc": "2.0", "id": index + 1, "method": "tools/call",
                "params": {"name": tool, "arguments": arguments}});
            input.extend(serde_json::to_vec(&line).unwrap());
            input.push(b'\n');
        }
        let served = mailbox.clone();
        let server = std::thread::spawn(move || {
            let mut output = Vec::new();
            serve_live(Cursor::new(input), &mut output, &served, &[]).unwrap();
            output
        });
        let started = Instant::now();
        while !server.is_finished() {
            assert!(started.elapsed() < SERVED_WITHIN, "the server never ended");
            for (id, call, _) in take_calls(&mailbox, CALLS_PER_TICK_MAX) {
                let (text, error) = match call.map(checked) {
                    Some(Ok(call)) => {
                        let answered = answer(&self.db, &self.project, caller, call);
                        assert!(answered.queued.is_none() || !answered.error);
                        (answered.text, answered.error)
                    }
                    Some(Err(refused)) => (refused.to_owned(), true),
                    None => ("unread".to_owned(), true),
                };
                answer_call(&mailbox, &id, &text, error).unwrap();
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        let output = server.join().unwrap();
        let answers: Vec<(String, bool)> = output
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
            .map(|line| {
                let answer: Value = serde_json::from_slice(line).unwrap();
                let result = &answer["result"];
                (
                    result["content"][0]["text"].as_str().unwrap().to_owned(),
                    result["isError"].as_bool().unwrap(),
                )
            })
            .collect();
        assert_eq!(answers.len(), calls.len(), "{answers:?}");
        answers
    }

    fn jobs(&self, id: &ChangeSetId) -> Vec<Job> {
        queue::jobs(&self.db, id).unwrap()
    }

    /// Queues every check of the current profile on a change set as the owner asks and
    /// runs the batch to its records here. Input: the change set. Output: the jobs as they
    /// ended.
    fn run_as_owner(&self, id: &ChangeSetId) -> Vec<Job> {
        let batch = queue::submit(&self.db, id, Trigger::OnRequest, Actor::Owner, &[]).unwrap();
        let mut ended = Vec::new();
        while let Some(job) = queue::run_next(&self.db, &self.engine).unwrap() {
            assert_eq!(job.batch, batch);
            ended.push(job);
        }
        assert!(!ended.is_empty());
        ended
    }
}

fn batches(jobs: &[Job]) -> usize {
    jobs.iter()
        .map(|job| job.batch.clone())
        .collect::<BTreeSet<_>>()
        .len()
}

#[test]
fn an_agent_cannot_run_checks_on_another_agents_change_set() {
    let team = Team::new();
    let own = team.change_set(7, &[("seven.txt", "seven\n")]);
    let other = team.change_set(8, &[("eight.txt", "eight\n")]);
    team.approve(json!([check("unit", &["true"], "exit_code")]), json!([]));

    let answers = team.call(
        Actor::Agent(7),
        &[
            (CHECKS_RUN_TOOL, json!({"change_set": other.as_str()})),
            (CHECKS_RUN_TOOL, json!({"checks": ["unit"], "agent": "8"})),
            (CHECKS_RUN_TOOL, json!({"checks": ["unit"]})),
        ],
    );
    assert_eq!(answers[0], (CHECKS_RUN_INVALID.to_owned(), true));
    assert_eq!(answers[1], (CHECKS_RUN_INVALID.to_owned(), true));
    assert!(!answers[2].1, "{answers:?}");
    assert!(team.jobs(&other).is_empty(), "{:?}", team.jobs(&other));
    let own_jobs = team.jobs(&own);
    assert_eq!(own_jobs.len(), 1, "{own_jobs:?}");
    assert_eq!(own_jobs[0].actor, Actor::Agent(7));
    assert_eq!(own_jobs[0].trigger, Trigger::OnRequest);
    assert_eq!(own_jobs[0].check, "unit");

    let answers = team.call(
        Actor::Agent(9),
        &[
            (CHECKS_RUN_TOOL, json!({})),
            (CHECKS_RESULTS_TOOL, json!({})),
        ],
    );
    assert!(answers.iter().all(|(_, error)| *error), "{answers:?}");
    assert!(
        answers
            .iter()
            .all(|(text, _)| !text.contains(other.as_str()) && !text.contains(own.as_str())),
        "{answers:?}"
    );
    assert!(team.jobs(&other).is_empty());
    assert_eq!(team.jobs(&own).len(), 1);

    let answers = team.call(Actor::Agent(8), &[(CHECKS_RUN_TOOL, json!({}))]);
    assert!(!answers[0].1, "{answers:?}");
    assert_eq!(team.jobs(&own).len(), 1);
    let other_jobs = team.jobs(&other);
    assert_eq!(other_jobs.len(), 1, "{other_jobs:?}");
    assert_eq!(other_jobs[0].actor, Actor::Agent(8));
}

#[test]
fn the_seventh_agent_request_in_an_hour_is_refused_at_the_default() {
    let team = Team::new();
    let own = team.change_set(7, &[("seven.txt", "seven\n")]);
    let other = team.change_set(8, &[("eight.txt", "eight\n")]);
    let ids: Vec<String> = (1..=7).map(|n| format!("c{n}")).collect();
    team.approve(
        Value::Array(
            ids.iter()
                .map(|id| check(id, &["true"], "exit_code"))
                .collect(),
        ),
        json!([]),
    );
    let settings = engine_checks::settings::effective(&team.db, &team.project).unwrap();
    assert_eq!(settings.agent_requests, 6);

    let asks: Vec<(&str, Value)> = ids
        .iter()
        .map(|id| (CHECKS_RUN_TOOL, json!({"checks": [id]})))
        .collect();
    let answers = team.call(Actor::Agent(7), &asks);
    assert!(answers[..6].iter().all(|(_, error)| !error), "{answers:?}");
    let (refused, error) = &answers[6];
    assert!(*error, "{refused}");
    assert!(refused.contains("6 check requests per hour"), "{refused}");
    assert_eq!(batches(&team.jobs(&own)), 6);
    assert!(team.jobs(&own).iter().all(|job| job.check != "c7"));

    let answers = team.call(
        Actor::Agent(8),
        &[(CHECKS_RUN_TOOL, json!({"checks": ["c7"]}))],
    );
    assert!(
        !answers[0].1,
        "another agent keeps its own hour: {answers:?}"
    );
    assert_eq!(batches(&team.jobs(&other)), 1);

    queue::submit(
        &team.db,
        &own,
        Trigger::OnRequest,
        Actor::Owner,
        &["c7".to_owned()],
    )
    .unwrap();
    assert_eq!(batches(&team.jobs(&own)), 7);
}

#[test]
fn an_agent_cannot_name_a_check_the_profile_does_not_run_on_request() {
    let team = Team::new();
    let own = team.change_set(7, &[("seven.txt", "seven\n")]);
    let mut accept_only = check("e2e", &["true"], "exit_code");
    accept_only["when"] = json!(["before_accept"]);
    accept_only["network"] = json!("remotes");
    let mut turn_only = check("turn", &["true"], "exit_code");
    turn_only["when"] = json!(["turn_end"]);
    team.approve(
        json!([
            check("unit", &["true"], "exit_code"),
            accept_only,
            turn_only
        ]),
        json!([]),
    );

    let answers = team.call(
        Actor::Agent(7),
        &[
            (CHECKS_RUN_TOOL, json!({"checks": ["e2e"]})),
            (CHECKS_RUN_TOOL, json!({"checks": ["unit", "turn"]})),
        ],
    );
    for (text, error) in &answers {
        assert!(*error, "{answers:?}");
        assert!(text.contains("not on request"), "{text}");
    }
    assert!(team.jobs(&own).is_empty(), "{:?}", team.jobs(&own));
    for actor in [Actor::Agent(7), Actor::Master(1)] {
        let refused = queue::submit(
            &team.db,
            &own,
            Trigger::OnRequest,
            actor,
            &["e2e".to_owned()],
        );
        assert_eq!(refused.unwrap_err().0, "checks_not_on_request", "{actor:?}");
    }
    assert!(team.jobs(&own).is_empty());

    let answers = team.call(Actor::Agent(7), &[(CHECKS_RUN_TOOL, json!({}))]);
    assert!(!answers[0].1, "{answers:?}");
    let jobs = team.jobs(&own);
    assert_eq!(
        jobs.iter()
            .map(|job| job.check.as_str())
            .collect::<Vec<_>>(),
        ["unit"],
        "leaving checks out asks only for the on-request ones: {jobs:?}"
    );

    queue::submit(
        &team.db,
        &own,
        Trigger::OnRequest,
        Actor::Owner,
        &["e2e".to_owned()],
    )
    .unwrap();
    let jobs = team.jobs(&own);
    assert!(
        jobs.iter()
            .any(|job| job.check == "e2e" && job.actor == Actor::Owner),
        "the owner still names any check: {jobs:?}"
    );
}

#[test]
fn checks_results_hides_holdout_detail_from_an_agent() {
    let team = Team::new();
    let own = team.change_set(7, &[("oracle.sh", ORACLE_FAILS)]);
    let digest = engine_checks::store_holdout_file(
        &team.db,
        &team.project,
        ORACLE_FAILS.as_bytes(),
        Actor::Owner,
    )
    .unwrap();
    let tx = Transaction::new_unchecked(&team.db, TransactionBehavior::Immediate).unwrap();
    tests_store::create_case(
        &tx,
        &CoreActor::new("owner").unwrap(),
        &key(),
        &NewCase {
            title: "The hidden oracle answers".into(),
            kind: CaseKind::Unit,
            stable_name: Some("oracle::answers".into()),
            steps: Vec::new(),
            protected: true,
            status: CaseStatus::Ready,
        },
        NOW,
    )
    .unwrap();
    tx.commit().unwrap();
    team.approve(
        json!([check("visible", &["sh", "oracle.sh"], "libtest")]),
        json!([{
            "check": check("sealed-oracle", &["sh", "sealed.sh"], "libtest"),
            "files": {"sealed.sh": digest}
        }]),
    );
    assert_eq!(team.run_as_owner(&own).len(), 2);
    let latest = crate::checks::record::latest(&team.db, &own).unwrap();
    assert_eq!(latest.len(), 2, "{latest:?}");
    let sealed = latest.iter().find(|run| run.holdout).unwrap();
    assert_eq!(
        integrity::sight(&team.db, sealed.run, Reader::Owner).unwrap(),
        RunSight::Full
    );
    assert!(matches!(
        integrity::sight(&team.db, sealed.run, Reader::Agent).unwrap(),
        RunSight::OutcomeOnly { .. }
    ));

    let answers = team.call(Actor::Agent(7), &[(CHECKS_RESULTS_TOOL, json!({}))]);
    let (text, error) = &answers[0];
    assert!(!error, "{text}");
    assert!(text.contains("visible"), "{text}");
    assert!(text.contains("oracle::answers"), "{text}");
    assert!(text.contains("holdout"), "{text}");
    assert!(text.contains("failed"), "{text}");
    for hidden in [
        "sealed",
        "expected 42, got 41",
        &digest,
        sealed.id.to_string().as_str(),
    ] {
        assert!(
            !text.contains(hidden),
            "{hidden:?} reached the agent: {text}"
        );
    }

    team.change_set(8, &[("eight.txt", "eight\n")]);
    let answers = team.call(Actor::Agent(8), &[(CHECKS_RESULTS_TOOL, json!({}))]);
    assert!(!answers[0].1, "{answers:?}");
    for shown in ["visible", "oracle::answers", "holdout"] {
        assert!(!answers[0].0.contains(shown), "{answers:?}");
    }
}
