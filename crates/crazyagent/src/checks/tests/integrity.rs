use super::*;
use crate::checks::markers::{
    NODE_ONLY_CALL, NODE_ONLY_OPTION, NODE_SKIP_CALL, NODE_SKIP_OPTION, NODE_TODO_CALL,
    NODE_TODO_OPTION, NODE_XDESCRIBE_CALL, NODE_XIT_CALL, NODE_XTEST_CALL, PYTHON_EXPECTED_FAILURE,
    PYTHON_IMPORTORSKIP, PYTHON_SKIP, PYTHON_SKIP_CALL, PYTHON_SKIP_EXCEPTION, PYTHON_SKIP_TEST,
    PYTHON_SKIPIF, PYTHON_UNITTEST_SKIP, PYTHON_UNITTEST_SKIP_IF, PYTHON_UNITTEST_SKIP_UNLESS,
    PYTHON_XFAIL, RUST_IGNORE, RUST_SHOULD_PANIC,
};
use crate::checks::{CheckKind, CheckSpec, NetworkMode, ProfileId, ReportFormat, Trigger};
use crate::landing::{AgentKind, ChangeAgent, FileMark, TurnEnd, TurnOutcome, Workspace};
use std::collections::BTreeMap;
use std::path::Path;
use std::process::{Command, Stdio};
use trackwright_core::model::{Actor as CoreActor, ProjectKey};
use trackwright_core::runs::{
    self, NewResult, PolicyRef, RunEnd, RunFinish, RunRecord, RunStart, SourceRef, Toolchain,
    ToolchainFamily,
};
use trackwright_core::tests_model::CaseKind as CoreKind;

const TRACKER_PROJECT: i64 = 1;
const PROFILE: &str = "5e1d3a0c9b8f7e6d5c4b3a291807f6e5d4c3b2a19087f6e5d4c3b2a190807f6e";
const MATH_BASE: &str = "#[test]
fn adds() {
    assert_eq!(1 + 1, 2);
}

#[test]
fn subtracts() {
    assert_eq!(2 - 1, 1);
}
";

/// One owner repository whose first commit holds the base files, an agent worktree added
/// from it whose turn wrote the edits, the change set that turn recorded in a real engine
/// store, and the trackwright run tables beside it.
struct Case {
    _tmp: tempfile::TempDir,
    db: Connection,
    id: ChangeSetId,
    base: String,
    head: String,
    base_tree: String,
}

/// Runs git as the owner would in a test: an isolated home, no system config, in `dir`.
/// Inputs: the owner home, the folder and the arguments. Output: standard output.
fn git_in(home: &Path, dir: &Path, args: &[&str]) -> String {
    let output = crate::spawn_gate::output(
        Command::new("/usr/bin/git")
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", home)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("LC_ALL", "C")
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
    String::from_utf8(output.stdout).unwrap()
}

/// Builds the trackwright and tracker tables with the tracker's own migration, so every
/// column, CHECK, foreign key and trigger is the real one, and creates core project 1 keyed
/// `CA` as the owner. Input: the engine store.
fn tracker_tables(db: &Connection) {
    crate::tracker::initialize(db).unwrap();
    let tx = db.unchecked_transaction().unwrap();
    trackwright_core::store::create_project(
        &tx,
        &crate::tracker::owner(),
        &ProjectKey::new("CA").unwrap(),
        "Project one",
        1,
    )
    .unwrap();
    tx.commit().unwrap();
    let project: i64 = db
        .query_row("SELECT id FROM tk_projects WHERE key = 'CA'", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(project, TRACKER_PROJECT);
}

impl Case {
    /// Builds the case. Inputs: the files of the owner's first commit, and the agent's
    /// edits, each a path with its new text or None to delete it. Output: the case.
    fn new(base: &[(&str, &str)], edits: &[(&str, Option<&str>)]) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(tmp.path()).unwrap();
        let (repo, home, engine) = (root.join("repo"), root.join("home"), root.join("engine"));
        for folder in [&repo, &home, &engine] {
            std::fs::create_dir(folder).unwrap();
        }
        let owner = |args: &[&str]| git_in(&home, &repo, args);
        owner(&["init", "-q", "--template=", "-b", "main"]);
        owner(&["config", "user.name", "Repo Owner"]);
        owner(&["config", "user.email", "owner@example.invalid"]);
        std::fs::write(repo.join("README"), "one\n").unwrap();
        for (path, text) in base {
            let file = repo.join(path);
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(file, text).unwrap();
        }
        owner(&["add", "-A"]);
        owner(&["commit", "-q", "-m", "Start"]);
        let worktree = root.join("agent-7");
        owner(&[
            "worktree",
            "add",
            "-q",
            "--detach",
            worktree.to_str().unwrap(),
            "HEAD",
        ]);
        for (path, text) in edits {
            let file = worktree.join(path);
            match text {
                Some(text) => {
                    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
                    std::fs::write(file, text).unwrap();
                }
                None => std::fs::remove_file(file).unwrap(),
            }
        }
        let db = landing::open(&engine).unwrap();
        let workspace = Workspace {
            project: ProjectId::new("project-1").unwrap(),
            agent: ChangeAgent {
                kind: AgentKind::Sub,
                id: "7".into(),
                label: "Ada (@builder) \u{b7} Codex".into(),
            },
            repository: repo.clone(),
            worktree,
            git_dir: repo.join(".git/worktrees/agent-7"),
        };
        let end = TurnEnd {
            outcome: TurnOutcome::Succeeded,
            turn: "turn-1".into(),
            prompt_line: "Change the tests".into(),
        };
        let id = landing::record_turn_end(&db, &workspace, &end)
            .unwrap()
            .expect("the turn recorded a change set");
        let (_, view) = landing::change_set(&db, &id).unwrap().unwrap();
        let base_tree = owner(&["rev-parse", &format!("{}^{{tree}}", view.base)])
            .trim()
            .to_owned();
        tracker_tables(&db);
        Self {
            _tmp: tmp,
            db,
            id,
            base: view.base,
            head: view.head,
            base_tree,
        }
    }

    /// Assesses one run of the change set's head. Inputs: the check, the protected paths,
    /// the settings and what the check reported. Output: the block or the refusal.
    fn assess(
        &self,
        check: &AdmittedCheck,
        protected: &[String],
        settings: &ChecksSettings,
        reported: &Reported<'_>,
    ) -> Result<Integrity, EngineError> {
        let subject = Subject {
            change_set: &self.id,
            head: &self.head,
            tracker_project: TRACKER_PROJECT,
            check,
            protected_paths: protected,
            settings,
        };
        assess(&self.db, &subject, reported)
    }

    /// Records one finished run with one passed result per name, the first failed when the
    /// verdict is failed. Inputs: the commit, the check id, the verdict, the finish time,
    /// the names and the block. Output: the run row id.
    fn seed_run(
        &self,
        commit: &str,
        check: &str,
        verdict: &str,
        finished_ms: i64,
        names: &[&str],
        integrity: Option<&str>,
    ) -> i64 {
        let results: Vec<(&str, &str)> = names
            .iter()
            .enumerate()
            .map(|(index, name)| {
                let outcome = if index == 0 && verdict == "failed" {
                    "failed"
                } else {
                    "passed"
                };
                (*name, outcome)
            })
            .collect();
        self.seed_outcomes(commit, check, verdict, finished_ms, &results, integrity)
    }

    /// Records one finished run as the engine's recorder would: its source on a commit, its
    /// policy naming the check, its results with their outcomes and its integrity block.
    /// Inputs: the commit, the check id, the verdict, the finish time, each result name
    /// with its outcome, and the block. Output: the run row id.
    fn seed_outcomes(
        &self,
        commit: &str,
        check: &str,
        verdict: &str,
        finished_ms: i64,
        results: &[(&str, &str)],
        integrity: Option<&str>,
    ) -> i64 {
        let number: i64 = self
            .db
            .query_row(
                "SELECT COALESCE(MAX(number), 0) + 1 FROM tk_runs WHERE project = ?1",
                params![TRACKER_PROJECT],
                |row| row.get(0),
            )
            .unwrap();
        let source = serde_json::json!({
            "kind": "git", "repository": "repo", "commit": commit,
            "tree": self.base_tree, "base": null, "change_set": null,
        });
        let policy = serde_json::json!({
            "profile": PROFILE, "profile_approved_ms": 1, "check": check, "kind": "unit",
            "argv": ["cargo", "test"], "toolchain": {"family": "rust", "version": "rustc 1.98.1"},
            "sandbox": PROFILE, "network": "off", "timeout_s": 600, "environment": PROFILE,
        });
        let count = |outcome: &str| {
            i64::try_from(results.iter().filter(|(_, got)| *got == outcome).count()).unwrap()
        };
        let total = i64::try_from(results.len()).unwrap();
        self.db
            .execute(
                "INSERT INTO tk_runs(project, number, title, state, verdict, source_json,
                     policy_json, integrity_json, started_ms, finished_ms, recorded_by, total,
                     passed, failed, skipped, errored, version)
                 VALUES (?1, ?2, ?3, 'finished', ?4, ?5, ?6, ?7, ?8, ?8, ?9, ?10, ?11, ?12,
                     ?13, ?14, 1)",
                params![
                    TRACKER_PROJECT,
                    number,
                    check,
                    verdict,
                    source.to_string(),
                    policy.to_string(),
                    integrity,
                    finished_ms,
                    ENGINE_ACTOR,
                    total,
                    count("passed"),
                    count("failed"),
                    count("skipped") + count("not_run"),
                    count("errored")
                ],
            )
            .unwrap();
        let run = self.db.last_insert_rowid();
        for (index, (name, outcome)) in results.iter().enumerate() {
            self.db
                .execute(
                    "INSERT INTO tk_results(run_id, position, name, outcome) VALUES (?1, ?2, ?3, ?4)",
                    params![run, i64::try_from(index + 1).unwrap(), name, outcome],
                )
                .unwrap();
        }
        run
    }

    /// The short id `CA-R<n>` trackwright shows for a run row. Input: the row id.
    /// Output: the short id.
    fn short(&self, run: i64) -> String {
        let number: i64 = self
            .db
            .query_row(
                "SELECT number FROM tk_runs WHERE id = ?1",
                params![run],
                |row| row.get(0),
            )
            .unwrap();
        format!("CA-R{number}")
    }
}

fn check(id: &str, holdout: bool) -> AdmittedCheck {
    AdmittedCheck {
        profile: ProfileId::new(PROFILE).unwrap(),
        approved_ms: 1,
        spec: CheckSpec {
            id: id.into(),
            kind: CheckKind::Unit,
            toolchain: None,
            argv: vec!["cargo".into(), "test".into()],
            cwd: ".".into(),
            format: ReportFormat::Libtest,
            report: None,
            timeout_s: 600,
            network: NetworkMode::Off,
            when: vec![Trigger::BeforeAccept],
            required: true,
            protected: false,
        },
        holdout: holdout
            .then(|| BTreeMap::from([("tests/holdout.rs".to_owned(), PROFILE.to_owned())])),
    }
}

fn passed<'a>(names: &[&'a str]) -> Reported<'a> {
    reported(
        &names
            .iter()
            .map(|name| (*name, ResultOutcome::Passed))
            .collect::<Vec<_>>(),
    )
}

fn reported<'a>(results: &[(&'a str, ResultOutcome)]) -> Reported<'a> {
    Reported {
        results: results.to_vec(),
        build_failed: false,
        exit: Some(0),
    }
}

fn with(settings: impl FnOnce(&mut ChecksSettings)) -> ChecksSettings {
    let mut chosen = ChecksSettings::default();
    settings(&mut chosen);
    chosen
}

fn skip(path: &str, line: u32, marker: &str) -> NewSkip {
    NewSkip {
        path: path.into(),
        line,
        marker: marker.into(),
    }
}

fn changed(path: &str, old_path: Option<&str>, status: FileStatus) -> ChangedFile {
    ChangedFile {
        path: path.into(),
        old_path: old_path.map(str::to_owned),
        status,
        kind: FileKind::Text,
        marks: Vec::new(),
    }
}

#[test]
fn a_change_set_that_deletes_a_test_records_the_removed_name() {
    let head = "#[test]\nfn adds() {\n    assert_eq!(1 + 1, 2);\n}\n";
    let case = Case::new(
        &[("tests/math.rs", MATH_BASE)],
        &[("tests/math.rs", Some(head))],
    );
    let baseline = case.seed_run(
        &case.base,
        "unit",
        "passed",
        100,
        &["math::adds", "math::subtracts"],
        None,
    );
    case.seed_run(&case.base, "unit", "failed", 200, &["math::adds"], None);
    case.seed_run(&case.base, "lint", "passed", 300, &["lint::all"], None);
    case.seed_run(&case.head, "unit", "passed", 400, &["math::adds"], None);
    let integrity = case
        .assess(
            &check("unit", false),
            &[],
            &ChecksSettings::default(),
            &passed(&["math::adds"]),
        )
        .unwrap();
    assert_eq!(
        integrity.inventory,
        Inventory {
            baseline_run: Some(case.short(baseline)),
            removed: vec!["math::subtracts".into()],
            removed_total: 1,
        }
    );
    assert_eq!(integrity.executed, 1);
    assert!(integrity.build_ok);
    assert!(!integrity.holdouts_ran);
    let block = serde_json::to_value(&integrity).unwrap();
    assert_eq!(
        block["inventory"]["removed"],
        serde_json::json!(["math::subtracts"])
    );
    assert_eq!(
        block["inventory"]["baseline_run"],
        serde_json::json!("CA-R1")
    );
}

#[test]
fn the_inventory_reads_a_baseline_the_trackwright_recorder_wrote_into_the_migrated_tables() {
    let head = "#[test]\nfn adds() {\n    assert_eq!(1 + 1, 2);\n}\n";
    let case = Case::new(
        &[("tests/math.rs", MATH_BASE)],
        &[("tests/math.rs", Some(head))],
    );
    let engine = CoreActor::new(ENGINE_ACTOR).unwrap();
    let tx = case.db.unchecked_transaction().unwrap();
    let started = runs::start(
        &tx,
        &engine,
        &RunStart {
            project: ProjectKey::new("CA").unwrap(),
            title: "unit".into(),
            plan: None,
            batch: None,
            source: SourceRef::Git {
                repository: "project-1".into(),
                commit: case.base.clone(),
                tree: case.base_tree.clone(),
                base: None,
                change_set: None,
            },
            policy: PolicyRef {
                profile: PROFILE.into(),
                profile_approved_ms: 1,
                check: "unit".into(),
                kind: CoreKind::Unit,
                argv: vec!["cargo".into(), "test".into()],
                toolchain: Toolchain {
                    family: ToolchainFamily::Rust,
                    version: "rustc 1.98.1".into(),
                },
                sandbox: PROFILE.into(),
                network: runs::NetworkMode::Off,
                timeout_s: 600,
                environment: PROFILE.into(),
            },
        },
        100,
    )
    .unwrap();
    let result = |name: &str| NewResult {
        name: name.into(),
        outcome: runs::ResultOutcome::Passed,
        duration_ms: Some(1),
        message: None,
        artifact: None,
    };
    let recorded = runs::record(
        &tx,
        &engine,
        &RunRecord {
            id: started.id.clone(),
            version: started.version,
            results: vec![result("math::adds"), result("math::subtracts")],
        },
        110,
    )
    .unwrap();
    let finished = runs::finish(
        &tx,
        &engine,
        &RunFinish {
            id: recorded.id.clone(),
            version: recorded.version,
            end: RunEnd::Completed,
            exit_code: Some(0),
            reason: None,
            integrity: None,
        },
        120,
    )
    .unwrap();
    tx.commit().unwrap();
    assert_eq!(finished.verdict, Some(runs::Verdict::Passed));
    let integrity = case
        .assess(
            &check("unit", false),
            &[],
            &ChecksSettings::default(),
            &passed(&["math::adds"]),
        )
        .unwrap();
    assert_eq!(
        integrity.inventory,
        Inventory {
            baseline_run: Some(String::from(finished.id)),
            removed: vec!["math::subtracts".into()],
            removed_total: 1,
        }
    );
    let rewritten = case
        .db
        .execute("UPDATE tk_results SET outcome = 'failed'", [])
        .unwrap_err();
    assert!(rewritten.to_string().contains("append-only"), "{rewritten}");
}

#[test]
fn the_integrity_test_tables_carry_the_checks_the_hand_copy_lacked() {
    let case = Case::new(
        &[("tests/math.rs", MATH_BASE)],
        &[("tests/math.rs", Some("#[test]\nfn adds() {}\n"))],
    );
    let run = case.seed_run(&case.base, "unit", "passed", 100, &["math::adds"], None);

    for change in [
        "finished_ms = NULL",
        "version = 0",
        "number = 0",
        "passed = -1",
    ] {
        let refused = case
            .db
            .execute(
                &format!("UPDATE tk_runs SET {change} WHERE id = ?1"),
                params![run],
            )
            .unwrap_err();
        assert!(
            refused.to_string().contains("CHECK constraint failed"),
            "{change}: {refused}"
        );
    }
}

#[test]
fn run_base_compares_with_the_latest_finished_base_run_and_off_compares_nothing() {
    let case = Case::new(
        &[("tests/math.rs", MATH_BASE)],
        &[("tests/math.rs", Some("#[test]\nfn adds() {}\n"))],
    );
    case.seed_run(
        &case.base,
        "unit",
        "passed",
        100,
        &["math::adds", "math::subtracts"],
        None,
    );
    let base_run = case.seed_run(
        &case.base,
        "unit",
        "failed",
        200,
        &["math::adds", "math::multiplies"],
        None,
    );
    let run_base = with(|settings| settings.inventory = InventoryMode::RunBase);
    let integrity = case
        .assess(
            &check("unit", false),
            &[],
            &run_base,
            &passed(&["math::adds"]),
        )
        .unwrap();
    assert_eq!(integrity.inventory.baseline_run, Some(case.short(base_run)));
    assert_eq!(case.short(base_run), "CA-R2");
    assert_eq!(integrity.inventory.removed, ["math::multiplies"]);
    let off = with(|settings| settings.inventory = InventoryMode::Off);
    let integrity = case
        .assess(&check("unit", false), &[], &off, &passed(&["math::adds"]))
        .unwrap();
    assert_eq!(
        integrity.inventory,
        Inventory {
            baseline_run: None,
            removed: Vec::new(),
            removed_total: 0,
        }
    );
}

#[test]
fn a_build_failure_is_not_build_ok_and_never_counts_every_test_as_removed() {
    let case = Case::new(
        &[("tests/math.rs", MATH_BASE)],
        &[("tests/math.rs", Some("#[test]\nfn adds() { nope }\n"))],
    );
    case.seed_run(
        &case.base,
        "unit",
        "passed",
        100,
        &["math::adds", "math::subtracts"],
        None,
    );
    let compile_error = Reported {
        results: Vec::new(),
        build_failed: true,
        exit: Some(101),
    };
    let integrity = case
        .assess(
            &check("unit", false),
            &[],
            &ChecksSettings::default(),
            &compile_error,
        )
        .unwrap();
    assert!(!integrity.build_ok);
    assert_eq!(integrity.inventory.baseline_run, None);
    assert_eq!(integrity.inventory.removed_total, 0);
    let silent = Reported {
        build_failed: false,
        ..compile_error.clone()
    };
    assert!(!silent.build_ok());
    let failing_tests = Reported {
        results: vec![("math::adds", ResultOutcome::Failed)],
        build_failed: false,
        exit: Some(101),
    };
    assert!(failing_tests.build_ok());
    let killed = Reported {
        exit: None,
        ..silent
    };
    assert!(killed.build_ok());
}

#[test]
fn an_added_ignore_attribute_is_flagged_for_rust() {
    let head = "#[test]
fn adds() {
    assert_eq!(1 + 1, 2);
}

#[test]
#[ignore]
fn subtracts() {
    assert_eq!(2 - 1, 1);
}
";
    let case = Case::new(
        &[("tests/math.rs", MATH_BASE)],
        &[("tests/math.rs", Some(head))],
    );
    let integrity = case
        .assess(
            &check("unit", false),
            &[],
            &ChecksSettings::default(),
            &passed(&["math::adds"]),
        )
        .unwrap();
    assert_eq!(integrity.new_skips, [skip("tests/math.rs", 7, RUST_IGNORE)]);
    assert_eq!(refusal(&integrity, &ChecksSettings::default()), None);
}

#[test]
fn an_added_skip_is_flagged_for_node() {
    let base = "import test from 'node:test';

test('adds', () => {});
test('subtracts', () => {});
test('multiplies', () => {});
";
    let head = "import test from 'node:test';

test.skip('adds', () => {});
test('subtracts', { skip: true }, () => {});
test.todo('multiplies');
";
    let case = Case::new(
        &[("tests/app.test.mjs", base)],
        &[("tests/app.test.mjs", Some(head))],
    );
    let integrity = case
        .assess(
            &check("unit", false),
            &[],
            &ChecksSettings::default(),
            &passed(&["adds"]),
        )
        .unwrap();
    assert_eq!(
        integrity.new_skips,
        [
            skip("tests/app.test.mjs", 3, NODE_SKIP_CALL),
            skip("tests/app.test.mjs", 4, NODE_SKIP_OPTION),
            skip("tests/app.test.mjs", 5, NODE_TODO_CALL),
        ]
    );
}

#[test]
fn an_added_skip_is_flagged_for_python() {
    let base = "import pytest


def test_adds():
    assert 1 + 1 == 2


def test_subtracts():
    assert 2 - 1 == 1


def test_multiplies():
    assert 2 * 2 == 4
";
    let head = "import pytest


@pytest.mark.skip(reason=\"later\")
def test_adds():
    assert 1 + 1 == 2


@pytest.mark.skipif(True, reason=\"later\")
def test_subtracts():
    assert 2 - 1 == 1


@pytest.mark.xfail
def test_multiplies():
    assert 2 * 2 == 4
";
    let case = Case::new(
        &[("tests/test_app.py", base)],
        &[("tests/test_app.py", Some(head))],
    );
    let integrity = case
        .assess(
            &check("unit", false),
            &[],
            &ChecksSettings::default(),
            &passed(&["test_app.py::test_adds"]),
        )
        .unwrap();
    assert_eq!(
        integrity.new_skips,
        [
            skip("tests/test_app.py", 4, PYTHON_SKIP),
            skip("tests/test_app.py", 9, PYTHON_SKIPIF),
            skip("tests/test_app.py", 14, PYTHON_XFAIL),
        ]
    );
}

#[test]
fn a_marker_inside_a_string_or_a_removed_line_is_not_flagged() {
    let rust_base = "#[test]
#[ignore]
fn old() {}

#[test]
#[ignore]
fn kept() {}
";
    let rust_head = "const NOTE: &str = \"#[ignore]\";
const RAW: &str = r#\"#[ignore] \"quoted\" \"#;
// #[ignore]
/* #[ignore] */

#[test]
#[ignore]
fn kept() {}
";
    let node_head = "const a = \"test.skip(\";
const b = `{ skip: true }
.todo(`;
// test.todo('x')
/* .skip( */
test('adds', () => {});
";
    let python_head = "NOTE = \"@pytest.mark.skip\"
DOC = \"\"\"
@pytest.mark.xfail
skipif
\"\"\"
# @pytest.mark.skip


def test_adds():
    assert True
";
    let case = Case::new(
        &[
            ("tests/notes.rs", rust_base),
            ("tests/app.test.mjs", "test('adds', () => {});\n"),
            ("tests/test_app.py", "def test_adds():\n    assert True\n"),
        ],
        &[
            ("tests/notes.rs", Some(rust_head)),
            ("tests/app.test.mjs", Some(node_head)),
            ("tests/test_app.py", Some(python_head)),
        ],
    );
    let refuse = with(|settings| settings.skip_markers = SkipMarkers::Refuse);
    let integrity = case
        .assess(&check("unit", false), &[], &refuse, &passed(&["kept"]))
        .unwrap();
    assert_eq!(integrity.new_skips, Vec::<NewSkip>::new());
    assert_eq!(refusal(&integrity, &refuse), None);
}

#[test]
fn skip_markers_refuse_refuses_and_off_reads_nothing() {
    let head = "#[test]\n#[ignore]\nfn adds() {}\n";
    let case = Case::new(
        &[("tests/math.rs", "#[test]\nfn adds() {}\n")],
        &[("tests/math.rs", Some(head))],
    );
    let refuse = with(|settings| settings.skip_markers = SkipMarkers::Refuse);
    let integrity = case
        .assess(&check("unit", false), &[], &refuse, &passed(&["adds"]))
        .unwrap();
    assert_eq!(integrity.new_skips, [skip("tests/math.rs", 2, RUST_IGNORE)]);
    assert_eq!(refusal(&integrity, &refuse), Some("checks_new_skips"));
    let off = with(|settings| settings.skip_markers = SkipMarkers::Off);
    let integrity = case
        .assess(&check("unit", false), &[], &off, &passed(&["adds"]))
        .unwrap();
    assert_eq!(integrity.new_skips, Vec::<NewSkip>::new());
    assert_eq!(refusal(&integrity, &off), None);
}

#[test]
fn a_test_file_git_reads_as_binary_is_unscanned_and_refused_under_refuse() {
    let case = Case::new(
        &[("tests/math.rs", "#[test]\nfn adds() {}\n")],
        &[(
            "tests/math.rs",
            Some("const HIDE: &str = \"\0\";\n#[test]\n#[ignore]\nfn adds() {}\n"),
        )],
    );
    let refuse = with(|settings| settings.skip_markers = SkipMarkers::Refuse);
    let integrity = case
        .assess(&check("unit", false), &[], &refuse, &passed(&["adds"]))
        .unwrap();
    assert_eq!(integrity.new_skips, [skip("tests/math.rs", 1, UNSCANNED)]);
    assert_eq!(refusal(&integrity, &refuse), Some("checks_new_skips"));
}

#[test]
fn touching_a_protected_path_refuses_under_refuse_and_flags_under_flag() {
    let case = Case::new(
        &[
            ("tests/acceptance/flow.rs", "#[test]\nfn flow() {}\n"),
            ("src/lib.rs", "pub fn one() -> u8 { 1 }\n"),
        ],
        &[
            (
                "tests/acceptance/flow.rs",
                Some("#[test]\nfn flow() { assert!(true) }\n"),
            ),
            ("src/lib.rs", Some("pub fn one() -> u8 { 2 }\n")),
        ],
    );
    let protected = vec!["tests/acceptance/".to_owned()];
    let refuse = ChecksSettings::default();
    assert_eq!(refuse.protected_change, ProtectedChange::Refuse);
    let integrity = case
        .assess(
            &check("unit", false),
            &protected,
            &refuse,
            &passed(&["flow"]),
        )
        .unwrap();
    assert_eq!(integrity.protected_touched, ["tests/acceptance/flow.rs"]);
    assert_eq!(
        refusal(&integrity, &refuse),
        Some("checks_protected_change")
    );
    let flag = with(|settings| settings.protected_change = ProtectedChange::Flag);
    let integrity = case
        .assess(&check("unit", false), &protected, &flag, &passed(&["flow"]))
        .unwrap();
    assert_eq!(integrity.protected_touched, ["tests/acceptance/flow.rs"]);
    assert_eq!(refusal(&integrity, &flag), None);
    let elsewhere = vec!["tests/other/".to_owned()];
    let integrity = case
        .assess(
            &check("unit", false),
            &elsewhere,
            &refuse,
            &passed(&["flow"]),
        )
        .unwrap();
    assert_eq!(integrity.protected_touched, Vec::<String>::new());
    assert_eq!(refusal(&integrity, &refuse), None);
}

#[test]
fn a_protected_path_counts_in_any_case_behind_attributes_parents_and_renames() {
    let entries = vec![
        "tests/acceptance/".to_owned(),
        "fixtures/golden.json".to_owned(),
    ];
    let mut attributes = changed("tests/.gitattributes", None, FileStatus::Added);
    attributes.marks.push(FileMark::Attributes);
    let files = [
        changed("TESTS/Acceptance/extra.rs", None, FileStatus::Added),
        changed(
            "tests/accep\u{200C}tance/hidden.rs",
            None,
            FileStatus::Added,
        ),
        changed(
            "tests/moved.rs",
            Some("tests/acceptance/flow.rs"),
            FileStatus::Renamed,
        ),
        changed("fixtures", None, FileStatus::Added),
        changed(".gitattributes", None, FileStatus::Modified),
        attributes,
        changed("fixtures/golden.json", None, FileStatus::Deleted),
        changed("tests/acceptance2/near.rs", None, FileStatus::Added),
        changed("docs/.gitattributes", None, FileStatus::Added),
        changed("tests/x.gitattributes", None, FileStatus::Added),
        changed("fixtures/golden.json.bak", None, FileStatus::Added),
    ];
    assert_eq!(
        protected(&files, &entries),
        [
            ".gitattributes",
            "TESTS/Acceptance/extra.rs",
            "fixtures",
            "fixtures/golden.json",
            "tests/.gitattributes",
            "tests/acceptance/flow.rs",
            "tests/accep\u{FFFD}tance/hidden.rs",
        ]
    );
}

#[test]
fn an_agent_sees_a_holdout_run_as_outcome_only() {
    let case = Case::new(
        &[("tests/math.rs", MATH_BASE)],
        &[("tests/math.rs", Some("#[test]\nfn adds() {}\n"))],
    );
    let holdout = case
        .assess(
            &check("hidden", true),
            &[],
            &ChecksSettings::default(),
            &passed(&["holdout::guards_the_secret"]),
        )
        .unwrap();
    assert!(holdout.holdouts_ran);
    let ordinary = case
        .assess(
            &check("unit", false),
            &[],
            &ChecksSettings::default(),
            &passed(&["math::adds"]),
        )
        .unwrap();
    assert!(!ordinary.holdouts_ran);
    let holdout_run = case.seed_run(
        &case.head,
        "hidden",
        "failed",
        100,
        &["holdout::guards_the_secret"],
        Some(&serde_json::to_string(&holdout).unwrap()),
    );
    let ordinary_run = case.seed_run(
        &case.head,
        "unit",
        "passed",
        200,
        &["math::adds"],
        Some(&serde_json::to_string(&ordinary).unwrap()),
    );
    let unassessed_run = case.seed_run(&case.head, "unit", "passed", 300, &["math::adds"], None);
    let outcome = sight(&case.db, holdout_run, Reader::Agent).unwrap();
    assert_eq!(
        outcome,
        RunSight::OutcomeOnly {
            run: holdout_run,
            state: "finished".into(),
            verdict: Some("failed".into()),
        }
    );
    assert!(
        !serde_json::to_string(&outcome)
            .unwrap()
            .contains("guards_the_secret")
    );
    assert_eq!(
        sight(&case.db, holdout_run, Reader::Owner).unwrap(),
        RunSight::Full
    );
    assert_eq!(
        sight(&case.db, ordinary_run, Reader::Agent).unwrap(),
        RunSight::Full
    );
    assert!(matches!(
        sight(&case.db, unassessed_run, Reader::Agent).unwrap(),
        RunSight::OutcomeOnly { .. }
    ));
    assert_eq!(
        sight(&case.db, unassessed_run + 1, Reader::Owner),
        Err(EngineError("checks_run_not_found".into()))
    );
}

#[test]
fn a_change_set_that_moved_past_the_snapshot_head_is_stale() {
    let case = Case::new(
        &[("tests/math.rs", MATH_BASE)],
        &[("tests/math.rs", Some("#[test]\nfn adds() {}\n"))],
    );
    let check = check("unit", false);
    let settings = ChecksSettings::default();
    let subject = Subject {
        change_set: &case.id,
        head: &case.base,
        tracker_project: TRACKER_PROJECT,
        check: &check,
        protected_paths: &[],
        settings: &settings,
    };
    assert_eq!(
        assess(&case.db, &subject, &passed(&["math::adds"])),
        Err(EngineError("checks_stale".into()))
    );
}

#[test]
fn the_block_stays_within_its_bound_with_long_names_and_counts_every_removal() {
    let case = Case::new(
        &[("tests/math.rs", MATH_BASE)],
        &[("tests/math.rs", Some("#[test]\nfn adds() {}\n"))],
    );
    let long: Vec<String> = (0..120)
        .map(|index| format!("suite::{index:03}::{}", "x".repeat(300)))
        .collect();
    let mut names: Vec<&str> = long.iter().map(String::as_str).collect();
    names.push("math::adds");
    case.seed_run(&case.base, "unit", "passed", 100, &names, None);
    let integrity = case
        .assess(
            &check("unit", false),
            &[],
            &ChecksSettings::default(),
            &passed(&["math::adds"]),
        )
        .unwrap();
    assert_eq!(integrity.inventory.removed_total, 120);
    assert!(!integrity.inventory.removed.is_empty());
    assert!(integrity.inventory.removed.len() < LIST_MAX);
    assert!(integrity.inventory.removed.iter().all(|name| {
        name.len() <= TEXT_BYTES_MAX && name.ends_with("...") && name.starts_with("suite::")
    }));
    assert!(serde_json::to_vec(&integrity).unwrap().len() <= INTEGRITY_BYTES_MAX);
}

#[test]
fn rust_lexing_keeps_real_markers_beside_char_literals_lifetimes_and_nested_comments() {
    let text = r##"fn quote() -> char { '"' }
#[ignore]
fn after_a_quote_char() {}
fn lifetime<'a>(text: &'a str) -> &'a str { text }
#[cfg_attr(unix, ignore)]
fn conditional() {}
/* outer /* inner */ #[ignore] still a comment */
fn escaped() -> char { '\'' }
# [ ignore ]
fn spaced() {}
"##;
    let found = markers::added(Language::Rust, "", text).expect("every literal closes");
    let lines: Vec<u32> = found.iter().map(|marker| marker.line).collect();
    assert_eq!(lines, [2, 5, 9]);
    assert!(found.iter().all(|marker| marker.marker == RUST_IGNORE));
    assert_eq!(Language::of_path("tests/A.RS"), Some(Language::Rust));
    assert_eq!(Language::of_path("web/app.spec.TSX"), Some(Language::Node));
    assert_eq!(Language::of_path("tests/test_a.py"), Some(Language::Python));
    assert_eq!(Language::of_path("README"), None);
}

#[test]
fn a_test_the_baseline_executed_that_this_run_skips_is_a_new_skip() {
    let case = Case::new(
        &[("tests/math.rs", MATH_BASE)],
        &[("tests/math.rs", Some("#[test]\nfn adds() {}\n"))],
    );
    case.seed_outcomes(
        &case.base,
        "unit",
        "passed",
        100,
        &[
            ("math::adds", "passed"),
            ("math::subtracts", "passed"),
            ("math::divides", "failed"),
            ("math::slow", "skipped"),
        ],
        None,
    );
    let now = reported(&[
        ("math::adds", ResultOutcome::Passed),
        ("math::subtracts", ResultOutcome::Skipped),
        ("math::divides", ResultOutcome::NotRun),
        ("math::slow", ResultOutcome::Skipped),
        ("math::later", ResultOutcome::NotRun),
    ]);
    let flag = ChecksSettings::default();
    let integrity = case
        .assess(&check("unit", false), &[], &flag, &now)
        .unwrap();
    assert_eq!(
        integrity.new_skips,
        [
            skip("math::divides", 1, SKIPPED_IN_RUN),
            skip("math::subtracts", 1, SKIPPED_IN_RUN),
        ]
    );
    assert_eq!(integrity.inventory.removed_total, 0);
    assert_eq!(integrity.executed, 1);
    assert_eq!(refusal(&integrity, &flag), None);
    let refuse = with(|settings| settings.skip_markers = SkipMarkers::Refuse);
    let integrity = case
        .assess(&check("unit", false), &[], &refuse, &now)
        .unwrap();
    assert_eq!(refusal(&integrity, &refuse), Some("checks_new_skips"));
    let off = with(|settings| settings.skip_markers = SkipMarkers::Off);
    let integrity = case.assess(&check("unit", false), &[], &off, &now).unwrap();
    assert_eq!(integrity.new_skips, Vec::<NewSkip>::new());
    let no_baseline = with(|settings| settings.inventory = InventoryMode::Off);
    let integrity = case
        .assess(&check("unit", false), &[], &no_baseline, &now)
        .unwrap();
    assert_eq!(integrity.new_skips, Vec::<NewSkip>::new());
}

#[test]
fn runtime_and_alias_skips_are_markers_and_should_panic_is_flagged_for_rust() {
    let lines = |language: Language, text: &str| -> Vec<(u32, &'static str)> {
        markers::added(language, "", text)
            .expect("every literal closes")
            .into_iter()
            .map(|marker| (marker.line, marker.marker))
            .collect()
    };
    let python = "import pytest
import unittest


def test_adds():
    pytest.skip(\"later\")


class Suite(unittest.TestCase):
    @unittest.skip(\"later\")
    def test_one(self):
        pass

    @unittest.skipIf(True, \"later\")
    def test_two(self):
        pass

    @unittest.skipUnless(False, \"later\")
    def test_three(self):
        pass

    @unittest.expectedFailure
    def test_four(self):
        self.skipTest(\"later\")


numpy = pytest.importorskip(\"numpy\")
";
    assert_eq!(
        lines(Language::Python, python),
        [
            (6, PYTHON_SKIP_CALL),
            (10, PYTHON_UNITTEST_SKIP),
            (14, PYTHON_UNITTEST_SKIP_IF),
            (18, PYTHON_UNITTEST_SKIP_UNLESS),
            (22, PYTHON_EXPECTED_FAILURE),
            (24, PYTHON_SKIP_TEST),
            (27, PYTHON_IMPORTORSKIP),
        ]
    );
    let node = "import { test, describe, it } from 'node:test';

test('adds', { todo: true }, () => {});
xit('subtracts', () => {});
xdescribe('suite', () => {});
xtest('multiplies', () => {});
test.only('divides', () => {});
test('powers', { timeout: 5, only: true }, () => {});
";
    assert_eq!(
        lines(Language::Node, node),
        [
            (3, NODE_TODO_OPTION),
            (4, NODE_XIT_CALL),
            (5, NODE_XDESCRIBE_CALL),
            (6, NODE_XTEST_CALL),
            (7, NODE_ONLY_CALL),
            (8, NODE_ONLY_OPTION),
        ]
    );
    let rust = "#[test]\n#[should_panic(expected = \"boom\")]\nfn adds() {}\n";
    assert_eq!(lines(Language::Rust, rust), [(2, RUST_SHOULD_PANIC)]);
    let python_base = "@pytest.mark.skip\ndef test_a():\n    pass\n";
    assert_eq!(
        markers::added(Language::Python, python_base, python_base),
        Some(Vec::new())
    );
}

#[test]
fn a_js_regex_literal_with_a_quote_or_backtick_does_not_hide_later_markers() {
    let text = "const a = /`/;
test.skip('one', () => {});
const b = /`/;
const q = /'[\"/]'/g;
test.skip('two', () => {});
const c = x / y; test.skip('three'); const d = z / w;
if (/\\/\\*/.test(s)) test.skip('four');
function f() { return /`/; }
test.skip('five');
const e = (a) / 2; test.skip('six'); const g = (b) / 3;
";
    let found = markers::added(Language::Node, "", text).expect("every literal closes");
    let lines: Vec<u32> = found.iter().map(|marker| marker.line).collect();
    assert_eq!(lines, [2, 5, 6, 7, 9, 10]);
    assert!(found.iter().all(|marker| marker.marker == NODE_SKIP_CALL));
}

#[test]
fn an_unterminated_literal_leaves_the_head_unscanned() {
    assert_eq!(
        markers::added(Language::Node, "", "const t = `open\ntest.skip('x');\n"),
        None
    );
    assert_eq!(
        markers::added(
            Language::Rust,
            "",
            "/* never closed\n#[ignore]\nfn a() {}\n"
        ),
        None
    );
    assert_eq!(
        markers::added(Language::Rust, "", "const R: &str = r#\"open\n#[ignore]\n"),
        None
    );
    assert_eq!(
        markers::added(Language::Python, "", "DOC = \"\"\"\n@pytest.mark.skip\n"),
        None
    );
    let closed_by_end = markers::added(Language::Rust, "", "#[ignore]\nfn a() {}\n// end")
        .expect("a line comment may end the text");
    assert_eq!(closed_by_end.len(), 1);
    let open_base = markers::added(Language::Node, "const t = `open", "test.skip('x');\n")
        .expect("only the head must close");
    assert_eq!(open_base.len(), 1);
    let case = Case::new(
        &[("tests/app.test.mjs", "test('adds', () => {});\n")],
        &[(
            "tests/app.test.mjs",
            Some("const t = `open\ntest.skip('adds', () => {});\n"),
        )],
    );
    let refuse = with(|settings| settings.skip_markers = SkipMarkers::Refuse);
    let integrity = case
        .assess(&check("unit", false), &[], &refuse, &passed(&["adds"]))
        .unwrap();
    assert_eq!(
        integrity.new_skips,
        [skip("tests/app.test.mjs", 1, UNSCANNED)]
    );
    assert_eq!(refusal(&integrity, &refuse), Some("checks_new_skips"));
}

#[test]
fn recorded_text_holds_no_line_break_or_hidden_character() {
    assert_eq!(
        recorded("tests/a\nb\u{200B}c\u{202E}d\te\r.rs"),
        "tests/a\u{FFFD}b\u{FFFD}c\u{FFFD}d\u{FFFD}e\u{FFFD}.rs"
    );
    assert_eq!(recorded("plain/path.rs"), "plain/path.rs");
    let cut = recorded(&format!("{}\u{e9}\u{e9}b", "a".repeat(196)));
    assert!(cut.len() <= TEXT_BYTES_MAX);
    assert!(cut.ends_with("..."));
    assert!(cut.starts_with(&"a".repeat(196)));
}

#[test]
fn a_quote_inside_an_interpolation_does_not_hide_later_markers() {
    let lines = |language: Language, text: &str| -> Vec<(u32, &'static str)> {
        markers::added(language, "", text)
            .expect("every literal closes")
            .into_iter()
            .map(|marker| (marker.line, marker.marker))
            .collect()
    };
    let node = r##"const s = `${'`'}`; test.skip('one', () => {});
const t = `${items.join(', ')} and ${a ? "b" : 'c'} ${`${x}`}`;
test.skip('two');
const u = `\${'` + "`";
test.skip('three');
"##;
    assert_eq!(
        lines(Language::Node, node),
        [
            (1, NODE_SKIP_CALL),
            (3, NODE_SKIP_CALL),
            (5, NODE_SKIP_CALL)
        ]
    );
    let python = r##"s = f"{'"'}"; pytest.skip("one")
label = f"{d['a']} {x:>{width}} {{braces}} {y!r:'^10}"
pytest.skip("two")
raw = rf'{d["k"]}' + b'\'' + F'''{"""x"""}'''
pytest.skip("three")
"##;
    assert_eq!(
        lines(Language::Python, python),
        [
            (1, PYTHON_SKIP_CALL),
            (3, PYTHON_SKIP_CALL),
            (5, PYTHON_SKIP_CALL)
        ]
    );
}

#[test]
fn a_string_open_at_its_line_end_or_nested_past_the_bound_leaves_the_head_unscanned() {
    assert_eq!(
        markers::added(Language::Node, "", "const a = 'open\ntest.skip('x');\n"),
        None
    );
    assert_eq!(
        markers::added(Language::Python, "", "A = \"open\npytest.skip(\"x\")\n"),
        None
    );
    assert_eq!(
        markers::added(Language::Node, "", "const s = `${'`}`;\ntest.skip('x');\n"),
        None
    );
    assert_eq!(
        markers::added(
            Language::Python,
            "",
            "s = f\"{'open}\"\npytest.skip(\"x\")\n"
        ),
        None
    );
    let deep_node = format!(
        "const s = {}x{};\ntest.skip('x');\n",
        "`${".repeat(40),
        "}`".repeat(40)
    );
    assert_eq!(markers::added(Language::Node, "", &deep_node), None);
    let deep_python = format!(
        "s = {}x{}\npytest.skip('x')\n",
        "f'{".repeat(40),
        "}'".repeat(40)
    );
    assert_eq!(markers::added(Language::Python, "", &deep_python), None);
}

#[test]
fn a_raised_skip_test_exception_is_a_python_marker_and_its_import_is_not() {
    let text = "import unittest
from unittest import SkipTest


def test_adds():
    raise unittest.SkipTest(\"later\")


def test_subtracts():
    raise SkipTest
";
    let found: Vec<(u32, &str)> = markers::added(Language::Python, "", text)
        .expect("every literal closes")
        .into_iter()
        .map(|marker| (marker.line, marker.marker))
        .collect();
    assert_eq!(
        found,
        [(6, PYTHON_SKIP_EXCEPTION), (10, PYTHON_SKIP_EXCEPTION)]
    );
}
