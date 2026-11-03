use ratatui::Terminal;
use ratatui::backend::TestBackend;
use serde_json::{Value, json};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use tempfile::TempDir;
use trackwright_cli::tui::Local;
use trackwright_cli::views::coverage::Coverage;
use trackwright_cli::views::runs::RunDetail;
use trackwright_cli::views::{Client, Theme};
use trackwright_core::coverage::CoverageOf;
use trackwright_core::criteria;
use trackwright_core::model::{Actor, OpKind, ProjectKey, ShortId};
use trackwright_core::standalone::Standalone;
use trackwright_core::tests_model::ALLURE_RESULT_BYTES_MAX;

const BIN: &str = env!("CARGO_BIN_EXE_trackwright");
const JUNIT: &str = "node-test.junit.xml";
const LIBTEST_BUILD_FAILED: &str = "libtest-build-failed.txt";
const CARGO_TEST: &[&str] = &["cargo", "test", "--locked"];

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../trackwright-core/tests/fixtures/results")
        .join(name)
}

fn run(cwd: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(cwd)
        .env_remove("TRACKWRIGHT_DB")
        .env_remove("TRACKWRIGHT_ACTOR")
        .output()
        .expect("the built trackwright binary runs")
}

fn envelope(output: &Output) -> Value {
    let text = std::str::from_utf8(&output.stdout).expect("stdout is UTF-8");
    assert_eq!(text.lines().count(), 1, "--json prints one line: {text}");
    let reply: Value = serde_json::from_str(text).expect("stdout is one JSON envelope");
    assert_eq!(reply["api"], 1, "{reply}");
    reply
}

fn owner(cwd: &Path, args: &[&str]) -> Output {
    run(cwd, &[args, &["--actor", "owner", "--json"]].concat())
}

fn initialized() -> TempDir {
    let folder = tempfile::tempdir().expect("a temporary folder");
    let output = owner(folder.path(), &["init", "--key", "TW"]);
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    folder
}

fn runs_of(cwd: &Path) -> Vec<Value> {
    let output = owner(cwd, &["run_list", "--project", "TW"]);
    let reply = envelope(&output);
    assert_eq!(reply["ok"], true, "{reply}");
    reply["data"]["runs"]
        .as_array()
        .expect("run_list replies its runs")
        .clone()
}

#[test]
fn the_built_binary_imports_a_junit_file_and_shows_the_run_in_json() {
    let folder = initialized();
    let report = folder.path().join(JUNIT);
    fs::copy(fixture(JUNIT), &report).expect("the real report copies");
    let path = report.to_str().expect("a UTF-8 path");
    let imported = owner(
        folder.path(),
        &[
            "run",
            "import",
            "--project",
            "TW",
            "--format",
            "junit",
            "--file",
            path,
        ],
    );
    assert_eq!(imported.status.code(), Some(0), "{imported:?}");
    let reply = envelope(&imported);
    assert_eq!(reply["ok"], true, "{reply}");
    assert_eq!(reply["data"]["run"], "TW-R1");
    assert_eq!(reply["data"]["recorded"], 3);
    assert_eq!(reply["data"]["verdict"], "failed");

    let shown = owner(folder.path(), &["run_show", "--id", "TW-R1"]);
    assert_eq!(shown.status.code(), Some(0), "{shown:?}");
    let reply = envelope(&shown);
    assert_eq!(reply["ok"], true, "{reply}");
    let data = &reply["data"];
    assert_eq!(data["run"]["state"], "finished");
    assert_eq!(data["run"]["verdict"], "failed");
    assert_eq!(data["source"], json!({"kind": "declared", "ref": JUNIT}));
    let outcomes: Vec<(&str, &str)> = data["results"]
        .as_array()
        .expect("run_show replies results")
        .iter()
        .map(|row| {
            (
                row["name"].as_str().expect("a result name"),
                row["outcome"].as_str().expect("an outcome"),
            )
        })
        .collect();
    assert_eq!(
        outcomes,
        [
            ("test.adds two numbers", "passed"),
            ("test.reports a wrong sum", "failed"),
            ("test.waits for a fixture", "skipped"),
        ]
    );
    assert_eq!(data["unlinked_total"], 3, "no case has these stable names");
}

#[test]
fn run_import_refuses_a_symlinked_file() {
    let folder = initialized();
    let report = folder.path().join(JUNIT);
    fs::copy(fixture(JUNIT), &report).expect("the real report copies");
    let link = folder.path().join("linked.junit.xml");
    std::os::unix::fs::symlink(&report, &link).expect("a symlink to the report");
    let path = link.to_str().expect("a UTF-8 path");
    let imported = owner(
        folder.path(),
        &[
            "run",
            "import",
            "--project",
            "TW",
            "--format",
            "junit",
            "--file",
            path,
        ],
    );
    assert_eq!(imported.status.code(), Some(1), "{imported:?}");
    let reply = envelope(&imported);
    assert_eq!(reply["ok"], false);
    assert_eq!(reply["code"], "import_file_refused", "{reply}");
    assert!(
        runs_of(folder.path()).is_empty(),
        "a refused import starts no run"
    );
}

#[test]
fn run_import_refuses_a_report_over_its_format_bound() {
    let folder = initialized();
    let report = folder.path().join("big-result.json");
    let padding = " ".repeat(ALLURE_RESULT_BYTES_MAX);
    fs::write(&report, format!("{{\"status\":\"passed\"}}{padding}")).expect("a big file");
    let path = report.to_str().expect("a UTF-8 path");
    let imported = owner(
        folder.path(),
        &[
            "run",
            "import",
            "--project",
            "TW",
            "--format",
            "allure",
            "--file",
            path,
        ],
    );
    assert_eq!(imported.status.code(), Some(1), "{imported:?}");
    let reply = envelope(&imported);
    assert_eq!(reply["code"], "import_file_refused", "{reply}");
    assert!(
        runs_of(folder.path()).is_empty(),
        "a refused import starts no run"
    );
}

#[test]
fn run_import_into_a_running_run_refuses_a_build_failed_report() {
    let folder = initialized();
    let source = json!({"kind": "declared", "ref": "ci"}).to_string();
    let policy = run_start_input(CARGO_TEST)["policy"].to_string();
    let started = owner(
        folder.path(),
        &[
            "run_start",
            "--project",
            "TW",
            "--title",
            "ci",
            "--source",
            &source,
            "--policy",
            &policy,
        ],
    );
    let reply = envelope(&started);
    assert_eq!(reply["ok"], true, "{reply}");
    assert_eq!(reply["data"]["run"]["id"], "TW-R1");
    let report = folder.path().join(LIBTEST_BUILD_FAILED);
    fs::copy(fixture(LIBTEST_BUILD_FAILED), &report).expect("the real report copies");
    let path = report.to_str().expect("a UTF-8 path");
    let imported = owner(
        folder.path(),
        &[
            "run", "import", "--run", "TW-R1", "--format", "libtest", "--file", path,
        ],
    );
    assert_eq!(imported.status.code(), Some(1), "{imported:?}");
    let reply = envelope(&imported);
    assert_eq!(
        reply["ok"], false,
        "a build failure never imports as ok: {reply}"
    );
    assert_eq!(reply["code"], "import_report_refused", "{reply}");
    let message = reply["message"].as_str().expect("a refusal message");
    assert!(message.contains("build_failed"), "{message}");

    let shown = envelope(&owner(folder.path(), &["run_show", "--id", "TW-R1"]));
    assert_eq!(shown["data"]["run"]["state"], "running", "{shown}");
    assert_eq!(
        shown["data"]["run"]["totals"]["total"], 0,
        "nothing was recorded"
    );
}

fn local(folder: &TempDir) -> Local {
    let path = folder.path().join("tracker.db");
    let mut store = trackwright_core::store::Store::open(&path).expect("the store opens");
    let tx = store.transaction().expect("a transaction");
    let owner = Actor::new("owner").expect("an actor");
    let key = ProjectKey::new("TW").expect("a key");
    trackwright_core::store::create_project(&tx, &owner, &key, "Trackwright", 1)
        .expect("the project creates");
    tx.commit().expect("the setup commits");
    Local::open(&path, owner).expect("the client opens")
}

fn add_criteria(folder: &TempDir, id: &str, texts: &[&str]) {
    let path = folder.path().join("tracker.db");
    let mut store = trackwright_core::store::Store::open(&path).expect("the store opens");
    let tx = store.transaction().expect("a transaction");
    let owner = Actor::new("owner").expect("an actor");
    let id = ShortId::parse(id).expect("a ticket id");
    for text in texts {
        criteria::add_criterion(&tx, &Standalone, &owner, &id, text, 2)
            .expect("the criterion adds");
    }
    tx.commit().expect("the criteria commit");
}

fn data(client: &mut dyn Client, op: OpKind, input: Value) -> Value {
    let reply = client.call(op, input.clone());
    assert_eq!(reply["ok"], true, "{} {input} replied {reply}", op.as_str());
    reply["data"].clone()
}

fn hex(digit: char, len: usize) -> String {
    std::iter::repeat_n(digit, len).collect()
}

fn run_start_input(argv: &[&str]) -> Value {
    json!({
        "project": "TW",
        "title": "unit on CS-12",
        "batch": "trigger-7",
        "source": {
            "kind": "git",
            "repository": "trackwright",
            "commit": hex('a', 40),
            "tree": hex('b', 40),
            "base": hex('c', 40),
            "change_set": "CS-12",
        },
        "policy": {
            "profile": hex('d', 64),
            "profile_approved_ms": 1_000,
            "check": "unit",
            "kind": "unit",
            "argv": argv,
            "toolchain": {"family": "rust", "version": "rustc 1.98.1"},
            "sandbox": hex('e', 64),
            "network": "off",
            "timeout_s": 1_800,
            "environment": hex('f', 64),
        },
    })
}

fn seed_run(client: &mut dyn Client, argv: &[&str]) {
    data(
        client,
        OpKind::CaseCreate,
        json!({
            "project": "TW",
            "title": "Adds two numbers",
            "kind": "unit",
            "stable_name": "sum::adds_two_numbers",
        }),
    );
    let started = data(client, OpKind::RunStart, run_start_input(argv));
    assert_eq!(started["run"]["id"], "TW-R1");
    let recorded = data(
        client,
        OpKind::RunRecord,
        json!({
            "id": "TW-R1",
            "version": started["run"]["version"],
            "results": [
                {"name": "sum::adds_two_numbers", "outcome": "passed", "duration_ms": 4},
                {
                    "name": "sum::overflows",
                    "outcome": "failed",
                    "message": "attempt to add with overflow",
                },
            ],
        }),
    );
    data(
        client,
        OpKind::RunFinish,
        json!({
            "id": "TW-R1",
            "version": recorded["run"]["version"],
            "end": "completed",
            "exit_code": 101,
            "integrity": {
                "executed": 2,
                "inventory": {"baseline_run": null, "removed": [], "removed_total": 0},
                "new_skips": [],
                "protected_touched": [],
                "holdouts_ran": true,
                "build_ok": true,
            },
        }),
    );
}

fn screen(width: u16, height: u16, draw: impl FnOnce(&mut ratatui::Frame<'_>)) -> Vec<String> {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("a test terminal");
    terminal.draw(draw).expect("the frame draws");
    let buffer = terminal.backend().buffer().clone();
    (0..height)
        .map(|y| (0..width).map(|x| buffer[(x, y)].symbol()).collect())
        .collect()
}

fn check(name: &str, actual: &[String]) {
    if let Ok(folder) = std::env::var("TRACKWRIGHT_FRAMES_OUT") {
        fs::create_dir_all(&folder).expect("the frames folder");
        fs::write(format!("{folder}/{name}.txt"), actual.join("\n") + "\n").expect("a frame");
    }
    let path = format!(
        "{}/tests/frames/tests/{name}.txt",
        env!("CARGO_MANIFEST_DIR")
    );
    let text = fs::read_to_string(&path).unwrap_or_else(|_| panic!("missing frame {path}"));
    let expected: Vec<&str> = text.lines().collect();
    let wrong: Vec<String> = expected
        .iter()
        .zip(actual)
        .enumerate()
        .filter(|(_, (want, got))| **want != got.as_str())
        .map(|(row, (want, got))| format!("row {row}\n want |{want}|\n  got |{got}|"))
        .collect();
    assert_eq!(expected.len(), actual.len(), "{name}: row count");
    assert!(wrong.is_empty(), "{name}:\n{}", wrong.join("\n"));
}

#[test]
fn the_run_view_renders_the_binding_block_at_80_columns() {
    let folder = tempfile::tempdir().expect("a temporary folder");
    let mut client = local(&folder);
    seed_run(&mut client, CARGO_TEST);
    let detail = RunDetail::load("TW-R1", &mut client).expect("the run loads");
    let frame = screen(80, 24, |frame| {
        let area = frame.area();
        detail.render(area, frame.buffer_mut(), &Theme::plain());
    });
    check("run-80", &frame);
}

#[test]
fn the_run_view_widens_the_result_names_at_120_columns() {
    let folder = tempfile::tempdir().expect("a temporary folder");
    let mut client = local(&folder);
    seed_run(&mut client, CARGO_TEST);
    let detail = RunDetail::load("TW-R1", &mut client).expect("the run loads");
    let frame = screen(120, 24, |frame| {
        let area = frame.area();
        detail.render(area, frame.buffer_mut(), &Theme::plain());
    });
    check("run-120", &frame);
}

#[test]
fn the_coverage_view_marks_uncovered_criteria() {
    let folder = tempfile::tempdir().expect("a temporary folder");
    let mut client = local(&folder);
    let ticket = data(
        &mut client,
        OpKind::Create,
        json!({"project": "TW", "title": "Sums stay exact"}),
    );
    assert_eq!(ticket["id"], "TW-1");
    add_criteria(&folder, "TW-1", &["Two numbers add", "Overflow is refused"]);
    data(
        &mut client,
        OpKind::CaseCreate,
        json!({"project": "TW", "title": "Adds two numbers", "kind": "unit"}),
    );
    data(
        &mut client,
        OpKind::CaseLink,
        json!({"id": "TW-T1", "version": 1, "ticket": "TW-1", "position": 1}),
    );
    let of = CoverageOf::Ticket(ShortId::parse("TW-1").expect("a ticket id"));
    let key = ProjectKey::new("TW").expect("a key");
    let view = Coverage::load(&key, &of, &mut client).expect("coverage loads");
    let frame = screen(80, 16, |frame| {
        let area = frame.area();
        view.render(area, frame.buffer_mut(), &Theme::plain());
    });
    let row = |text: &str| {
        frame
            .iter()
            .find(|line| line.contains(text))
            .unwrap_or_else(|| panic!("a row shows {text}: {frame:#?}"))
            .clone()
    };
    assert!(row("Two numbers add").contains("1 case"), "{frame:#?}");
    assert!(!row("Two numbers add").contains("uncovered"), "{frame:#?}");
    assert!(
        row("Overflow is refused").contains("uncovered"),
        "{frame:#?}"
    );
    assert!(row("TW-T1").contains("no result"), "{frame:#?}");
    assert!(
        frame[0].contains("1 of 2 uncovered"),
        "the title counts the uncovered criteria: {frame:#?}"
    );
}

#[test]
fn the_binding_block_quotes_arguments_that_hold_spaces_or_quotes() {
    let folder = tempfile::tempdir().expect("a temporary folder");
    let mut client = local(&folder);
    seed_run(&mut client, &["sh", "-c", "echo it's done"]);
    let detail = RunDetail::load("TW-R1", &mut client).expect("the run loads");
    let frame = screen(120, 24, |frame| {
        let area = frame.area();
        detail.render(area, frame.buffer_mut(), &Theme::plain());
    });
    let command = frame
        .iter()
        .find(|line| line.contains("Command"))
        .unwrap_or_else(|| panic!("a command row: {frame:#?}"));
    assert_eq!(
        command.trim_end(),
        r"   Command      sh -c 'echo it'\''s done'",
        "one argument with a space stays one quoted argument"
    );
}
