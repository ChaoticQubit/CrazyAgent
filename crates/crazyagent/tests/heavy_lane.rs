#![cfg(unix)]

use crazyagent::coord::lane::{FOLDER, Holder, LANE_FAILED, tick, view};
use crazyagent::coord::settings::{CoordSettings, HeavyLane, SharedResource};
use crazyagent::protocol::ProjectId;
use rusqlite::Connection;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const PROJECT: &str = "lane-binary";
const TICK: Duration = Duration::from_millis(100);
const DEADLINE: Duration = Duration::from_secs(60);

fn project() -> ProjectId {
    ProjectId::new(PROJECT).unwrap()
}

fn db() -> Connection {
    let connection = Connection::open_in_memory().unwrap();
    crazyagent::coord::ensure(&connection).unwrap();
    connection
}

/// Makes an agent's temporary folder with its lane folder inside, as the engine lays them
/// out. Takes the test root and the agent. Returns the temporary folder and the holder.
fn agent(root: &Path, id: i64) -> (PathBuf, Holder) {
    let tmp = root.join(format!("agent-{id}")).join("tmp");
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(tmp.join(FOLDER))
        .unwrap();
    (
        tmp.clone(),
        Holder {
            agent: id,
            folder: tmp.join(FOLDER),
        },
    )
}

/// Starts the real `crazyagent lane` with the agent's lane folder, as its Heavy commands
/// section writes it, and a `TMPDIR` elsewhere, as an agent's short scratch folder is.
/// Takes the agent's temporary folder and the arguments after `--folder`. Returns the child
/// with stderr piped.
fn lane(tmp: &Path, arguments: &[&str]) -> Child {
    lane_in(Some(&tmp.join(FOLDER)), tmp.parent().unwrap(), arguments)
}

/// Starts the real `crazyagent lane`, with `--folder` when given. Takes the folder, the
/// `TMPDIR` and the arguments after `--folder`. Returns the child with stderr piped.
fn lane_in(folder: Option<&Path>, scratch: &Path, arguments: &[&str]) -> Child {
    let mut command = Command::new(env!("CARGO_BIN_EXE_crazyagent"));
    command.arg("lane");
    if let Some(folder) = folder {
        command.arg("--folder").arg(folder);
    }
    command
        .args(arguments)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("TMPDIR", scratch)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    crazyagent::spawn_gate::spawn(&mut command).unwrap()
}

fn now_ms() -> i64 {
    i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap()
}

/// Ticks the lane as the team runtime does until every child ended or `DEADLINE` passed.
/// Takes the database, the holders, the settings and the children. Returns each exit code.
fn tick_until_done(
    connection: &Connection,
    holders: &[Holder],
    settings: &CoordSettings,
    children: &mut [Child],
) -> Vec<i32> {
    let started = Instant::now();
    let mut codes: Vec<Option<i32>> = vec![None; children.len()];
    while codes.iter().any(Option::is_none) {
        assert!(
            started.elapsed() < DEADLINE,
            "the lane commands did not end"
        );
        tick(connection, &project(), holders, settings, now_ms()).unwrap();
        for (child, code) in children.iter_mut().zip(codes.iter_mut()) {
            if code.is_none()
                && let Some(status) = child.try_wait().unwrap()
            {
                *code = Some(status.code().unwrap_or(-1));
            }
        }
        std::thread::sleep(TICK);
    }
    codes.into_iter().map(Option::unwrap).collect()
}

fn stderr(child: Child) -> String {
    String::from_utf8(child.wait_with_output().unwrap().stderr).unwrap()
}

#[test]
fn lane_one_runs_two_heavy_commands_one_after_the_other() {
    let root = tempfile::tempdir().unwrap();
    let connection = db();
    let (a_tmp, a) = agent(root.path(), 1);
    let (b_tmp, b) = agent(root.path(), 2);
    let log = root.path().join("log");
    let script = |name: &str| {
        format!(
            "echo start-{name} >> '{}'; sleep 1; echo end-{name} >> '{}'",
            log.display(),
            log.display()
        )
    };
    let mut children = vec![
        lane(
            &a_tmp,
            &["--label", "suite a", "--", "/bin/sh", "-c", &script("a")],
        ),
        lane(
            &b_tmp,
            &["--label", "suite b", "--", "/bin/sh", "-c", &script("b")],
        ),
    ];
    let codes = tick_until_done(
        &connection,
        &[a, b],
        &CoordSettings::default(),
        &mut children,
    );
    assert_eq!(codes, vec![0, 0]);
    let lines: Vec<String> = std::fs::read_to_string(&log)
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect();
    assert_eq!(lines.len(), 4, "{lines:?}");
    let first = lines[0].trim_start_matches("start-").to_owned();
    let second = if first == "a" { "b" } else { "a" };
    assert_eq!(
        lines,
        vec![
            format!("start-{first}"),
            format!("end-{first}"),
            format!("start-{second}"),
            format!("end-{second}"),
        ]
    );
}

#[test]
fn lane_no_limit_runs_heavy_commands_together() {
    let root = tempfile::tempdir().unwrap();
    let connection = db();
    let (a_tmp, a) = agent(root.path(), 1);
    let (b_tmp, b) = agent(root.path(), 2);
    let log = root.path().join("log");
    let script = |name: &str| {
        format!(
            "echo start-{name} >> '{}'; sleep 2; echo end-{name} >> '{}'",
            log.display(),
            log.display()
        )
    };
    let mut children = vec![
        lane(&a_tmp, &["--", "/bin/sh", "-c", &script("a")]),
        lane(&b_tmp, &["--", "/bin/sh", "-c", &script("b")]),
    ];
    let settings = CoordSettings {
        heavy_lane: HeavyLane::NoLimit,
        ..CoordSettings::default()
    };
    assert_eq!(
        tick_until_done(&connection, &[a, b], &settings, &mut children),
        vec![0, 0]
    );
    let text = std::fs::read_to_string(&log).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert!(
        lines[0].starts_with("start-") && lines[1].starts_with("start-"),
        "{lines:?}"
    );
}

#[test]
fn a_shared_resource_holds_a_second_command_that_names_it() {
    let root = tempfile::tempdir().unwrap();
    let connection = db();
    let (a_tmp, a) = agent(root.path(), 1);
    let (b_tmp, b) = agent(root.path(), 2);
    let log = root.path().join("log");
    let script = |name: &str| {
        format!(
            "echo start-{name} >> '{}'; sleep 1; echo end-{name} >> '{}'",
            log.display(),
            log.display()
        )
    };
    let settings = CoordSettings {
        heavy_lane: HeavyLane::NoLimit,
        shared_resources: vec![SharedResource {
            name: "ollama".into(),
            slots: 1,
        }],
        ..CoordSettings::default()
    };
    let mut children = vec![
        lane(
            &a_tmp,
            &["--uses", "ollama", "--", "/bin/sh", "-c", &script("a")],
        ),
        lane(
            &b_tmp,
            &["--uses", "ollama", "--", "/bin/sh", "-c", &script("b")],
        ),
    ];
    assert_eq!(
        tick_until_done(&connection, &[a, b], &settings, &mut children),
        vec![0, 0]
    );
    let text = std::fs::read_to_string(&log).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert!(
        lines[0].starts_with("start-") && lines[1].starts_with("end-"),
        "{lines:?}"
    );
}

#[test]
fn a_lane_command_outside_an_agent_runs_at_once_with_its_exit_code() {
    let root = tempfile::tempdir().unwrap();
    let child = lane_in(None, root.path(), &["--", "/bin/sh", "-c", "exit 7"]);
    let output = child.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(7));
    let text = String::from_utf8(output.stderr).unwrap();
    assert!(text.contains("runs at once"), "{text}");
}

#[test]
fn a_lane_command_naming_an_unknown_resource_does_not_run() {
    let root = tempfile::tempdir().unwrap();
    let connection = db();
    let (tmp, holder) = agent(root.path(), 1);
    let marker = root.path().join("ran");
    let mut children = vec![lane(
        &tmp,
        &[
            "--uses",
            "ollama",
            "--",
            "/usr/bin/touch",
            marker.to_str().unwrap(),
        ],
    )];
    let codes = tick_until_done(
        &connection,
        &[holder],
        &CoordSettings::default(),
        &mut children,
    );
    assert_eq!(codes, vec![LANE_FAILED]);
    assert!(!marker.exists());
    let text = stderr(children.remove(0));
    assert!(text.contains("ollama"), "{text}");
}

#[test]
fn a_terminated_lane_command_stops_its_command_and_frees_its_slot() {
    let root = tempfile::tempdir().unwrap();
    let connection = db();
    let (tmp, holder) = agent(root.path(), 1);
    let marker = root.path().join("started");
    let script = format!("echo started > '{}'; exec sleep 30", marker.display());
    let child = lane(&tmp, &["--", "/bin/sh", "-c", &script]);
    let holders = [holder];
    let settings = CoordSettings::default();
    let started = Instant::now();
    while !marker.exists() {
        assert!(started.elapsed() < DEADLINE, "the command never started");
        tick(&connection, &project(), &holders, &settings, now_ms()).unwrap();
        std::thread::sleep(TICK);
    }
    assert_eq!(view(&connection, &project()).unwrap().running.len(), 1);
    let pid = rustix::process::Pid::from_raw(i32::try_from(child.id()).unwrap()).unwrap();
    rustix::process::kill_process(pid, rustix::process::Signal::TERM).unwrap();
    let mut children = vec![child];
    let codes = tick_until_done(&connection, &holders, &settings, &mut children);
    assert_eq!(codes, vec![128 + 15]);
    tick(&connection, &project(), &holders, &settings, now_ms()).unwrap();
    let shown = view(&connection, &project()).unwrap();
    assert!(shown.running.is_empty() && shown.waiting.is_empty());
}
