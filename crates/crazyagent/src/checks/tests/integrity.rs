use super::*;
use crate::checks::markers::{
    NODE_SKIP_CALL, NODE_SKIP_OPTION, NODE_TODO_CALL, PYTHON_SKIP, PYTHON_SKIPIF, PYTHON_XFAIL,
    RUST_IGNORE,
};
use crate::checks::{CheckKind, CheckSpec, NetworkMode, ProfileId, ReportFormat, Trigger};
use crate::landing::{AgentKind, ChangeAgent, FileMark, TurnEnd, TurnOutcome, Workspace};
use std::collections::BTreeMap;
use std::path::Path;
use std::process::{Command, Stdio};

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

/// Creates the trackwright run and result tables with the columns of the test management
/// contract's migration step 2, until `trackwright-core` is a dependency of this crate and
/// its migration makes them. Input: the engine store.
fn tracker_tables(db: &Connection) {
    db.execute_batch(
        "CREATE TABLE tk_runs(
            id INTEGER PRIMARY KEY, project INTEGER NOT NULL, number INTEGER NOT NULL,
            plan_id INTEGER, batch TEXT, title TEXT NOT NULL,
            state TEXT NOT NULL CHECK (state IN ('running', 'finished', 'interrupted')),
            verdict TEXT CHECK (verdict IN
                ('passed', 'failed', 'no_tests', 'build_failed', 'unknown', 'unavailable')),
            reason TEXT, source_json TEXT NOT NULL, policy_json TEXT NOT NULL,
            integrity_json TEXT, started_ms INTEGER NOT NULL, finished_ms INTEGER,
            recorded_by TEXT NOT NULL, total INTEGER NOT NULL, passed INTEGER NOT NULL,
            failed INTEGER NOT NULL, skipped INTEGER NOT NULL, errored INTEGER NOT NULL,
            version INTEGER NOT NULL, UNIQUE (project, number));
        CREATE TABLE tk_results(
            id INTEGER PRIMARY KEY, run_id INTEGER NOT NULL REFERENCES tk_runs(id),
            position INTEGER NOT NULL CHECK (position >= 1), case_id INTEGER,
            name TEXT NOT NULL,
            outcome TEXT NOT NULL CHECK (outcome IN
                ('passed', 'failed', 'skipped', 'errored', 'not_run')),
            duration_ms INTEGER, message TEXT, artifact TEXT,
            UNIQUE (run_id, name), UNIQUE (run_id, position));",
    )
    .unwrap();
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

    /// Records one finished run as the engine's recorder would: its source on a commit, its
    /// policy naming the check, one passed result per name (the first failed when the
    /// verdict is failed) and its integrity block. Inputs: the commit, the check id, the
    /// verdict, the finish time, the names and the block. Output: the run row id.
    fn seed_run(
        &self,
        commit: &str,
        check: &str,
        verdict: &str,
        finished_ms: i64,
        names: &[&str],
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
        let failed = i64::from(verdict == "failed");
        let total = i64::try_from(names.len()).unwrap();
        self.db
            .execute(
                "INSERT INTO tk_runs(project, number, title, state, verdict, source_json,
                     policy_json, integrity_json, started_ms, finished_ms, recorded_by, total,
                     passed, failed, skipped, errored, version)
                 VALUES (?1, ?2, ?3, 'finished', ?4, ?5, ?6, ?7, ?8, ?8, ?9, ?10, ?11, ?12,
                     0, 0, 1)",
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
                    total - failed,
                    failed
                ],
            )
            .unwrap();
        let run = self.db.last_insert_rowid();
        for (index, name) in names.iter().enumerate() {
            let outcome = if index == 0 && failed == 1 {
                "failed"
            } else {
                "passed"
            };
            self.db
                .execute(
                    "INSERT INTO tk_results(run_id, position, name, outcome) VALUES (?1, ?2, ?3, ?4)",
                    params![run, i64::try_from(index + 1).unwrap(), name, outcome],
                )
                .unwrap();
        }
        run
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
    Reported {
        names: names.to_vec(),
        executed: u32::try_from(names.len()).unwrap(),
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
            baseline_run: Some(baseline),
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
        serde_json::json!(baseline)
    );
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
    assert_eq!(integrity.inventory.baseline_run, Some(base_run));
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
        names: Vec::new(),
        executed: 0,
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
        names: vec!["math::adds"],
        executed: 1,
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
    assert_eq!(integrity.new_skips, [skip("tests/math.rs", 0, UNSCANNED)]);
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
            "tests/accep\u{200C}tance/hidden.rs",
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
    let found = markers::added(Language::Rust, "", text);
    let lines: Vec<u32> = found.iter().map(|marker| marker.line).collect();
    assert_eq!(lines, [2, 5, 9]);
    assert!(found.iter().all(|marker| marker.marker == RUST_IGNORE));
    assert_eq!(Language::of_path("tests/A.RS"), Some(Language::Rust));
    assert_eq!(Language::of_path("web/app.spec.TSX"), Some(Language::Node));
    assert_eq!(Language::of_path("tests/test_a.py"), Some(Language::Python));
    assert_eq!(Language::of_path("README"), None);
}
