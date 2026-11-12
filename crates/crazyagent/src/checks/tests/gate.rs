use crate::checks::gate::{
    self, FAILED, GateNote, NEW_SKIPS, NOT_RUN, PROTECTED_CHANGE, STALE, TESTS_REMOVED,
};
use crate::checks::queue::{self, JobState};
use crate::checks::record;
use crate::checks::settings::{self, GateMode, ProtectedChange, RemovedTests, SkipMarkers};
use crate::checks::{self, Actor, AdmittedProfile, ChecksChoice, Scope, Trigger};
use crate::landing::{
    self, AcceptOutcome, AgentKind, ChangeAgent, ChangeSetId, ChangeState, TurnEnd, TurnOutcome,
    Workspace,
};
use crate::projects_home::CRAZYAGENT_DIRECTORY;
use crate::protocol::ProjectId;
use crate::tracker::{self, Host};
use rusqlite::{Connection, Transaction, TransactionBehavior};
use serde_json::{Value, json};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use trackwright_core::model::{Actor as CoreActor, ProjectKey, ShortId};
use trackwright_core::ops;
use trackwright_core::requests::Request;

const NOW: i64 = 1_800_000_000_000;

/// One owner repository on `main` attached to the tracker as `CA`, with a `release` branch
/// checked out nowhere, one agent worktree recorded as the engine records it, an isolated
/// home for the test's own git, and an engine runtime with the real engine schema.
struct Case {
    _temp: tempfile::TempDir,
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

fn owner() -> CoreActor {
    CoreActor::new("owner").unwrap()
}

/// One check of a profile as the owner writes it: the id, the script it runs with `sh`
/// and whether the gate requires it, reporting by exit code with network off.
fn check(id: &str, script: &str, required: bool) -> Value {
    json!({
        "id": id,
        "kind": "unit",
        "toolchain": null,
        "argv": ["sh", script],
        "cwd": ".",
        "format": "exit_code",
        "report": null,
        "timeout_s": 600,
        "network": "off",
        "when": ["before_accept", "on_request"],
        "required": required,
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
        git(&home, &repo, &["branch", "release"]);
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
        tracker::attach(&tx, &project, &ProjectKey::new("CA").unwrap(), "One", NOW).unwrap();
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
            repo,
            home,
            engine,
            db,
            project,
            workspace,
        }
    }

    /// Writes files into the agent worktree and records a turn end, which commits them
    /// into the worktree's open change set. Inputs: the turn name and the files. Output:
    /// the change set.
    fn turn(&self, turn: &str, files: &[(&str, &str)]) -> ChangeSetId {
        for (path, text) in files {
            write(&self.workspace.worktree.join(path), text);
        }
        let end = TurnEnd {
            outcome: TurnOutcome::Succeeded,
            turn: turn.into(),
            prompt_line: "Change the fixture".into(),
        };
        landing::record_turn_end(&self.db, &self.workspace, &end)
            .unwrap()
            .unwrap()
    }

    /// Proposes and approves a profile as the owner. Inputs: the checks and the
    /// protected paths. Output: the admitted profile.
    fn approve(&self, checks: Value, protected: &[&str]) -> AdmittedProfile {
        let body = json!({
            "version": 1,
            "checks": checks,
            "holdouts": [],
            "protected_paths": protected
        });
        let proposal = checks::propose(
            &self.db,
            &self.project,
            &serde_json::to_vec(&body).unwrap(),
            Actor::Owner,
        )
        .unwrap();
        self.approve_again(&proposal.id)
    }

    fn approve_again(&self, id: &crate::checks::ProfileId) -> AdmittedProfile {
        checks::approve(&self.db, &self.project, id, Actor::Owner).unwrap()
    }

    /// Runs every check of the current profile on a change set as the owner asks, to
    /// their records. Input: the change set.
    fn run(&self, id: &ChangeSetId) {
        queue::submit(&self.db, id, Trigger::OnRequest, Actor::Owner, &[]).unwrap();
        let mut ran = 0;
        while let Some(job) = queue::run_next(&self.db, &self.engine).unwrap() {
            assert_eq!(job.state, JobState::Recorded, "{job:?}");
            ran += 1;
        }
        assert!(ran > 0);
    }

    /// Saves the project's checks settings as the owner. Input: what to change.
    fn set(&self, change: impl FnOnce(&mut ChecksChoice)) {
        let scope = Scope::Project(self.project.clone());
        let (mut choice, version) = settings::read(&self.db, &scope).unwrap();
        change(&mut choice);
        settings::write(&self.db, &scope, &choice, version, Actor::Owner).unwrap();
    }

    fn gate(&self, mode: GateMode) {
        self.set(|choice| choice.gate = Some(mode));
    }

    fn head(&self, id: &ChangeSetId) -> String {
        landing::change_set(&self.db, id).unwrap().unwrap().1.head
    }

    fn state(&self, id: &ChangeSetId) -> ChangeState {
        landing::change_set(&self.db, id).unwrap().unwrap().1.state
    }

    fn tip(&self, branch: &str) -> String {
        git(&self.home, &self.repo, &["rev-parse", branch])
    }

    /// Accepts a change set at the head the owner reviewed onto the owner's branch.
    fn accept(&self, id: &ChangeSetId) -> Result<AcceptOutcome, String> {
        let head = self.head(id);
        landing::accept(
            &self.db,
            &self.home,
            id,
            &head,
            None,
            crate::activity::AcceptPlace::Changes,
        )
        .map_err(|error| error.0)
    }

    fn accept_into(&self, id: &ChangeSetId, branch: &str) -> Result<AcceptOutcome, String> {
        let head = self.head(id);
        landing::accept_into(
            &self.db,
            &self.home,
            id,
            &head,
            None,
            branch,
            crate::activity::AcceptPlace::Changes,
        )
        .map_err(|error| error.0)
    }

    /// An owner ticket in `CA` linked to a change set. Input: the change set. Output: the
    /// ticket.
    fn ticket_on(&self, id: &ChangeSetId) -> ShortId {
        let tx = Transaction::new_unchecked(&self.db, TransactionBehavior::Immediate).unwrap();
        let call = |op: &str, args: Value| -> Value {
            let request = Request::parse(op, &serde_json::to_vec(&args).unwrap()).unwrap();
            let reply: Value =
                serde_json::from_str(ops::dispatch(&tx, &Host, &owner(), request).json()).unwrap();
            assert_eq!(reply["ok"], true, "{op}: {reply}");
            reply
        };
        let created = call(
            "create",
            json!({"project": "CA", "title": "Land the change"}),
        );
        let ticket = created["data"]["id"].as_str().unwrap().to_owned();
        call(
            "link",
            json!({"id": ticket, "kind": "change_set", "target": id.as_str()}),
        );
        tx.commit().unwrap();
        ShortId::parse(&ticket).unwrap()
    }

    fn notes(&self, id: &ChangeSetId) -> Vec<GateNote> {
        gate::notes(&self.db, id)
            .unwrap()
            .into_iter()
            .map(|recorded| recorded.note)
            .collect()
    }

    /// Sets the inventory of the integrity block of a change set's newest run of one check
    /// to the tests it no longer reports against that run as baseline, as the assessment
    /// records a deleted test, or removes the block. Inputs: the change set, the check and
    /// the removed names, None to remove the block.
    fn removed(&self, id: &ChangeSetId, check: &str, names: Option<&[&str]>) {
        let run = record::latest(&self.db, id)
            .unwrap()
            .into_iter()
            .find(|run| run.check == check)
            .unwrap();
        let block = names.map(|names| {
            let stored: String = self
                .db
                .query_row(
                    "SELECT integrity_json FROM tk_runs WHERE id = ?1",
                    [run.run],
                    |row| row.get(0),
                )
                .unwrap();
            let mut block: Value = serde_json::from_str(&stored).unwrap();
            block["inventory"] = json!({
                "baseline_run": run.id.to_string(),
                "removed": names,
                "removed_total": names.len(),
            });
            block.to_string()
        });
        let changed = self
            .db
            .execute(
                "UPDATE tk_runs SET integrity_json = ?1 WHERE id = ?2",
                rusqlite::params![block, run.run],
            )
            .unwrap();
        assert_eq!(changed, 1);
    }

    /// Accepts a change set at its reviewed head onto the owner's branch, running `between`
    /// once its files are written and before the branch moves. Inputs: the change set and
    /// what to change then. Output: the outcome, or the refusal code.
    fn accept_between(
        &self,
        id: &ChangeSetId,
        between: &dyn Fn(),
    ) -> Result<AcceptOutcome, String> {
        let head = self.head(id);
        landing::accept_between(&self.db, &self.home, id, &head, between).map_err(|error| error.0)
    }

    fn untouched(&self, main: &str) {
        assert_eq!(self.tip("main"), main);
        assert_eq!(git(&self.home, &self.repo, &["status", "--porcelain"]), "");
    }
}

fn landed(outcome: Result<AcceptOutcome, String>) -> String {
    match outcome {
        Ok(AcceptOutcome::Landed { commit, .. }) => commit,
        other => panic!("expected a landed accept, got {other:?}"),
    }
}

fn codes(note: &GateNote) -> Vec<(&str, Option<&str>)> {
    note.gaps
        .iter()
        .map(|gap| (gap.code.as_str(), gap.check.as_deref()))
        .collect()
}

#[test]
fn require_refuses_accept_when_the_passing_run_covered_another_tree() {
    let case = Case::new();
    let id = case.turn("turn-1", &[("pass.sh", "exit 0\n")]);
    case.approve(json!([check("unit", "pass.sh", true)]), &[]);
    case.run(&id);
    let covered = case.head(&id);
    case.gate(GateMode::Require);
    assert_eq!(case.turn("turn-2", &[("notes.txt", "later\n")]), id);
    assert_ne!(case.head(&id), covered);
    let run = record::latest(&case.db, &id).unwrap().remove(0);
    assert_eq!(run.commit, covered);
    let (main, release) = (case.tip("main"), case.tip("release"));

    assert_eq!(case.accept(&id).unwrap_err(), STALE);
    assert_eq!(case.accept_into(&id, "release").unwrap_err(), STALE);
    assert_eq!(case.tip("main"), main);
    assert_eq!(case.tip("release"), release);
    assert_eq!(case.state(&id), ChangeState::Pending);
    let note = gate::assess(&case.db, &id, &case.head(&id)).unwrap();
    assert_eq!(codes(&note), [(STALE, Some("unit"))]);
    assert_eq!(
        note.gaps[0].run.as_deref(),
        Some(run.id.to_string().as_str())
    );
    assert!(case.notes(&id).is_empty());
    assert_eq!(
        gate::admit(&case.db, &id, &covered).unwrap_err().0,
        "changes_stale"
    );
}

#[test]
fn require_accepts_when_every_required_check_passed_on_the_reviewed_head() {
    let case = Case::new();
    let id = case.turn(
        "turn-1",
        &[("pass.sh", "exit 0\n"), ("fail.sh", "exit 1\n")],
    );
    case.approve(
        json!([
            check("unit", "pass.sh", true),
            check("lint", "fail.sh", false)
        ]),
        &[],
    );
    case.run(&id);
    case.gate(GateMode::Require);
    let main = case.tip("main");

    let note = gate::admit(&case.db, &id, &case.head(&id)).unwrap();
    assert_eq!(note.gate, GateMode::Require);
    assert!(note.gaps.is_empty(), "{note:?}");
    let commit = landed(case.accept(&id));
    assert_ne!(commit, main);
    assert_eq!(case.tip("main"), commit);
    assert_eq!(case.state(&id), ChangeState::Accepted);
    assert!(case.notes(&id).is_empty());
}

#[test]
fn accept_into_is_gated_like_accept() {
    let case = Case::new();
    let id = case.turn("turn-1", &[("pass.sh", "exit 0\n")]);
    case.approve(json!([check("unit", "pass.sh", true)]), &[]);
    case.gate(GateMode::Require);
    let release = case.tip("release");

    assert_eq!(case.accept_into(&id, "release").unwrap_err(), NOT_RUN);
    assert_eq!(case.tip("release"), release);
    case.run(&id);
    let commit = landed(case.accept_into(&id, "release"));
    assert_eq!(case.tip("release"), commit);
}

#[test]
fn require_refuses_with_no_profile_no_run_or_a_failed_run() {
    let case = Case::new();
    let id = case.turn(
        "turn-1",
        &[("pass.sh", "exit 0\n"), ("fail.sh", "exit 1\n")],
    );
    case.gate(GateMode::Require);
    assert_eq!(case.accept(&id).unwrap_err(), NOT_RUN);
    let note = gate::assess(&case.db, &id, &case.head(&id)).unwrap();
    assert_eq!(note.profile, None);
    assert_eq!(codes(&note), [(NOT_RUN, None)]);

    case.approve(
        json!([
            check("unit", "pass.sh", true),
            check("e2e", "fail.sh", true)
        ]),
        &[],
    );
    assert_eq!(case.accept(&id).unwrap_err(), NOT_RUN);
    case.run(&id);
    assert_eq!(case.accept(&id).unwrap_err(), FAILED);
    let note = gate::assess(&case.db, &id, &case.head(&id)).unwrap();
    assert_eq!(codes(&note), [(FAILED, Some("e2e"))]);
    assert_eq!(case.state(&id), ChangeState::Pending);
}

#[test]
fn warn_accepts_and_records_the_gap() {
    let case = Case::new();
    let id = case.turn("turn-1", &[("fail.sh", "exit 1\n")]);
    case.approve(json!([check("unit", "fail.sh", true)]), &[]);
    case.run(&id);
    let ticket = case.ticket_on(&id);
    let head = case.head(&id);
    let run = record::latest(&case.db, &id).unwrap().remove(0);
    assert_eq!(
        settings::effective(&case.db, &case.project).unwrap().gate,
        GateMode::Warn
    );

    landed(case.accept(&id));
    assert_eq!(case.state(&id), ChangeState::Accepted);
    let notes = gate::notes(&case.db, &id).unwrap();
    assert_eq!(notes.len(), 1);
    let recorded = &notes[0];
    assert_eq!(recorded.change_set, id);
    assert_eq!(recorded.tickets, [ticket.to_string()]);
    assert_eq!(recorded.note.gate, GateMode::Warn);
    assert_eq!(recorded.note.head, head);
    assert_eq!(codes(&recorded.note), [(FAILED, Some("unit"))]);
    assert_eq!(
        recorded.note.gaps[0].run.as_deref(),
        Some(run.id.to_string().as_str())
    );
    assert_eq!(gate::ticket_notes(&case.db, &ticket).unwrap(), notes);
}

#[test]
fn warn_with_no_profile_records_that_no_check_ran() {
    let case = Case::new();
    let id = case.turn("turn-1", &[("notes.txt", "one\n")]);
    landed(case.accept(&id));
    let notes = case.notes(&id);
    assert_eq!(notes.len(), 1);
    assert_eq!(notes[0].profile, None);
    assert_eq!(codes(&notes[0]), [(NOT_RUN, None)]);
}

#[test]
fn gate_off_checks_nothing_and_records_nothing() {
    let case = Case::new();
    let id = case.turn(
        "turn-1",
        &[
            ("fail.sh", "exit 1\n"),
            ("tests/acceptance/flow.sh", "exit 0\n"),
        ],
    );
    case.approve(
        json!([check("unit", "fail.sh", true)]),
        &["tests/acceptance/"],
    );
    case.run(&id);
    case.set(|choice| {
        choice.gate = Some(GateMode::Off);
        choice.protected_change = Some(ProtectedChange::Refuse);
        choice.skip_markers = Some(SkipMarkers::Refuse);
    });
    let note = gate::admit(&case.db, &id, &case.head(&id)).unwrap();
    assert_eq!(note.gate, GateMode::Off);
    assert!(note.gaps.is_empty());
    landed(case.accept(&id));
    assert!(case.notes(&id).is_empty());
}

#[test]
fn a_profile_approved_after_the_run_makes_the_run_stale() {
    let case = Case::new();
    let id = case.turn("turn-1", &[("pass.sh", "exit 0\n")]);
    let first = case.approve(json!([check("unit", "pass.sh", true)]), &[]);
    case.run(&id);
    case.gate(GateMode::Require);
    let second = case.approve(
        json!([
            check("unit", "pass.sh", true),
            check("lint", "pass.sh", false)
        ]),
        &[],
    );
    assert_ne!(second.id, first.id);
    assert_eq!(case.accept(&id).unwrap_err(), STALE);

    let again = case.approve_again(&first.id);
    assert_eq!(again.id, first.id);
    assert!(again.approved_ms > first.approved_ms);
    let run = record::latest(&case.db, &id).unwrap().remove(0);
    assert_eq!(run.profile, first.id);
    assert_eq!(run.profile_approved_ms, first.approved_ms);
    assert_eq!(case.accept(&id).unwrap_err(), STALE);

    case.run(&id);
    landed(case.accept(&id));
}

#[test]
fn protected_change_refuse_blocks_accept_even_with_passing_runs() {
    let case = Case::new();
    let id = case.turn(
        "turn-1",
        &[
            ("pass.sh", "exit 0\n"),
            ("TESTS/Acceptance/flow.sh", "exit 0\n"),
        ],
    );
    case.approve(
        json!([check("unit", "pass.sh", true)]),
        &["tests/acceptance/"],
    );
    case.run(&id);
    let main = case.tip("main");
    for mode in [GateMode::Require, GateMode::Warn] {
        case.gate(mode);
        assert_eq!(case.accept(&id).unwrap_err(), PROTECTED_CHANGE, "{mode:?}");
        assert_eq!(case.tip("main"), main);
    }
    let note = gate::assess(&case.db, &id, &case.head(&id)).unwrap();
    assert_eq!(codes(&note), [(PROTECTED_CHANGE, None)]);
    assert_eq!(note.gaps[0].paths, ["TESTS/Acceptance/flow.sh"]);
    assert!(case.notes(&id).is_empty());

    case.set(|choice| {
        choice.gate = Some(GateMode::Require);
        choice.protected_change = Some(ProtectedChange::Flag);
    });
    landed(case.accept(&id));
}

#[test]
fn skip_markers_refuse_blocks_accept_when_the_change_adds_a_skip() {
    let case = Case::new();
    let id = case.turn(
        "turn-1",
        &[
            ("pass.sh", "exit 0\n"),
            ("tests/math.rs", "#[test]\n#[ignore]\nfn adds() {}\n"),
        ],
    );
    case.approve(json!([check("unit", "pass.sh", true)]), &[]);
    case.set(|choice| choice.skip_markers = Some(SkipMarkers::Off));
    case.run(&id);
    case.set(|choice| {
        choice.gate = Some(GateMode::Warn);
        choice.skip_markers = Some(SkipMarkers::Refuse);
    });
    let main = case.tip("main");

    assert_eq!(case.accept(&id).unwrap_err(), NEW_SKIPS);
    assert_eq!(case.tip("main"), main);
    let note = gate::assess(&case.db, &id, &case.head(&id)).unwrap();
    assert_eq!(codes(&note), [(NEW_SKIPS, None)]);
    assert_eq!(note.gaps[0].paths, ["tests/math.rs"]);

    case.set(|choice| {
        choice.gate = Some(GateMode::Require);
        choice.skip_markers = Some(SkipMarkers::Flag);
    });
    landed(case.accept(&id));
}

#[test]
fn require_refuses_when_the_profile_requires_no_check() {
    let case = Case::new();
    let id = case.turn("turn-1", &[("pass.sh", "exit 0\n")]);
    let profile = case.approve(json!([check("lint", "pass.sh", false)]), &[]);
    case.gate(GateMode::Require);
    let main = case.tip("main");

    assert_eq!(case.accept(&id).unwrap_err(), NOT_RUN);
    case.run(&id);
    assert_eq!(case.accept(&id).unwrap_err(), NOT_RUN);
    case.untouched(&main);
    let note = gate::assess(&case.db, &id, &case.head(&id)).unwrap();
    assert_eq!(note.profile, Some(profile.id));
    assert_eq!(codes(&note), [(NOT_RUN, None)]);

    case.gate(GateMode::Warn);
    landed(case.accept(&id));
    let notes = case.notes(&id);
    assert_eq!(notes.len(), 1);
    assert_eq!(codes(&notes[0]), [(NOT_RUN, None)]);
}

#[test]
fn a_profile_approved_while_accept_runs_refuses_before_the_branch_moves() {
    let case = Case::new();
    let id = case.turn("turn-1", &[("pass.sh", "exit 0\n")]);
    case.approve(json!([check("unit", "pass.sh", true)]), &[]);
    case.run(&id);
    case.gate(GateMode::Require);
    let main = case.tip("main");
    assert!(
        gate::admit(&case.db, &id, &case.head(&id))
            .unwrap()
            .gaps
            .is_empty()
    );

    let refused = case.accept_between(&id, &|| {
        case.approve(
            json!([
                check("unit", "pass.sh", true),
                check("lint", "pass.sh", false)
            ]),
            &[],
        );
    });
    assert_eq!(refused.unwrap_err(), STALE);
    case.untouched(&main);
    assert!(!case.repo.join("pass.sh").exists());
    assert_eq!(case.state(&id), ChangeState::Pending);

    case.run(&id);
    let commit = landed(case.accept(&id));
    assert_eq!(case.tip("main"), commit);
}

#[test]
fn the_gate_set_to_require_while_accept_runs_refuses_before_the_branch_moves() {
    let case = Case::new();
    let id = case.turn("turn-1", &[("notes.txt", "one\n")]);
    let main = case.tip("main");

    let refused = case.accept_between(&id, &|| case.gate(GateMode::Require));
    assert_eq!(refused.unwrap_err(), NOT_RUN);
    case.untouched(&main);
    assert!(!case.repo.join("notes.txt").exists());
    assert_eq!(case.state(&id), ChangeState::Pending);
}

#[test]
fn removed_tests_flag_refuses_under_require_and_is_kept_in_the_note_under_warn() {
    let case = Case::new();
    let id = case.turn("turn-1", &[("pass.sh", "exit 0\n")]);
    case.approve(json!([check("unit", "pass.sh", true)]), &[]);
    case.run(&id);
    case.removed(&id, "unit", Some(&["math::subtracts"]));
    assert_eq!(
        settings::effective(&case.db, &case.project)
            .unwrap()
            .removed_tests,
        RemovedTests::Flag
    );
    case.gate(GateMode::Require);
    let main = case.tip("main");

    assert_eq!(case.accept(&id).unwrap_err(), TESTS_REMOVED);
    assert_eq!(case.accept_into(&id, "release").unwrap_err(), TESTS_REMOVED);
    case.untouched(&main);
    let run = record::latest(&case.db, &id).unwrap().remove(0);
    let note = gate::assess(&case.db, &id, &case.head(&id)).unwrap();
    assert_eq!(note.removed_tests, RemovedTests::Flag);
    assert_eq!(codes(&note), [(TESTS_REMOVED, Some("unit"))]);
    assert_eq!(note.gaps[0].paths, ["math::subtracts"]);
    assert_eq!(note.gaps[0].paths_total, 1);
    assert_eq!(
        note.gaps[0].run.as_deref(),
        Some(run.id.to_string().as_str())
    );

    case.gate(GateMode::Warn);
    landed(case.accept(&id));
    let notes = case.notes(&id);
    assert_eq!(notes.len(), 1);
    assert_eq!(codes(&notes[0]), [(TESTS_REMOVED, Some("unit"))]);
}

#[test]
fn removed_tests_refuse_blocks_accept_under_warn() {
    let case = Case::new();
    let id = case.turn("turn-1", &[("pass.sh", "exit 0\n")]);
    case.approve(json!([check("unit", "pass.sh", true)]), &[]);
    case.run(&id);
    case.removed(&id, "unit", Some(&["math::subtracts", "math::divides"]));
    case.set(|choice| choice.removed_tests = Some(RemovedTests::Refuse));
    let main = case.tip("main");

    assert_eq!(case.accept(&id).unwrap_err(), TESTS_REMOVED);
    case.untouched(&main);
    assert!(case.notes(&id).is_empty());
    let note = gate::assess(&case.db, &id, &case.head(&id)).unwrap();
    assert_eq!(note.gate, GateMode::Warn);
    assert_eq!(note.removed_tests, RemovedTests::Refuse);
    assert_eq!(note.gaps[0].paths, ["math::divides", "math::subtracts"]);
    assert_eq!(note.gaps[0].paths_total, 2);

    case.removed(&id, "unit", Some(&[]));
    landed(case.accept(&id));
    assert!(case.notes(&id).is_empty());
}

#[test]
fn a_passing_run_without_an_integrity_block_counts_as_removing_tests() {
    let case = Case::new();
    let id = case.turn("turn-1", &[("pass.sh", "exit 0\n")]);
    case.approve(json!([check("unit", "pass.sh", true)]), &[]);
    case.run(&id);
    case.removed(&id, "unit", None);
    case.gate(GateMode::Require);

    assert_eq!(case.accept(&id).unwrap_err(), TESTS_REMOVED);
    let note = gate::assess(&case.db, &id, &case.head(&id)).unwrap();
    assert_eq!(codes(&note), [(TESTS_REMOVED, Some("unit"))]);
    assert!(note.gaps[0].paths.is_empty());
    assert_eq!(note.gaps[0].paths_total, 0);
}

#[test]
fn removed_tests_off_leaves_removed_tests_out_of_the_gate() {
    let case = Case::new();
    let id = case.turn("turn-1", &[("pass.sh", "exit 0\n")]);
    case.approve(json!([check("unit", "pass.sh", true)]), &[]);
    case.run(&id);
    case.removed(&id, "unit", Some(&["math::subtracts"]));
    case.set(|choice| {
        choice.gate = Some(GateMode::Require);
        choice.removed_tests = Some(RemovedTests::Off);
    });

    let note = gate::assess(&case.db, &id, &case.head(&id)).unwrap();
    assert_eq!(note.removed_tests, RemovedTests::Off);
    assert!(note.gaps.is_empty(), "{note:?}");
    case.removed(&id, "unit", None);
    let note = gate::assess(&case.db, &id, &case.head(&id)).unwrap();
    assert!(note.gaps.is_empty(), "{note:?}");
    landed(case.accept(&id));
    assert!(case.notes(&id).is_empty());
}
