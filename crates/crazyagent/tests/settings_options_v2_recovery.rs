#![cfg(target_os = "macos")]

mod e2e_support;

use crazyagent::client_controller::OwnerController;
use crazyagent::engine::owner::{
    NoticeShown, OwnerClientKind, OwnerResult, RecoveryBanner, RunShown,
};
use crazyagent::ledger::{self, ActionKind};
use crazyagent::project_setup::{Platform, ProjectSetup, ResearchKind};
use crazyagent::protocol::{ProjectId, ProjectSpec};
use crazyagent::terminal::FOLDER;
use crazyagent::terminal::tmux::{SOCKET, installed};
use e2e_support::{Owner, result, scratch};
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use std::fs;
use std::io::{BufRead, BufReader, Read};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const PROJECT: &str = "v2-recovery";
const ASKED: &str = "Write the buoy alarm tests";
const MISSING: &str = "its session record is missing";
const WAITS: &str = "Its task or message waits for the owner's retry";
const LET_GO: &str = "Its task or message was let go";
const WAIT: Duration = Duration::from_secs(60);
const ATTACH_TIME: Duration = Duration::from_secs(10);
const READY_TIME: Duration = Duration::from_secs(30);
const STOP_TIME: Duration = Duration::from_secs(40);
const STDERR_BYTES_MAX: u64 = 65536;
const STAND_IN: &str = "#!/bin/sh\nwhile IFS= read -r line; do printf '%s\\n' \"$line\" >> \"$CODEX_HOME/heard.log\"; done\n";

/// The engine's private tmux server, ended when the test ends, also when it fails.
struct Server {
    tmux: PathBuf,
    socket: PathBuf,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = crazyagent::spawn_gate::status(
            Command::new(&self.tmux)
                .arg("-S")
                .arg(&self.socket)
                .arg("kill-server")
                .env_clear()
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null()),
        );
    }
}

/// The real `crazyagent engine` process with writable settings and a codex executable, so
/// it runs its team runtime and resumes interrupted runs as the desktop app starts it.
struct TeamEngine {
    child: Child,
    stderr: Option<std::thread::JoinHandle<String>>,
}

impl TeamEngine {
    /// Starts the engine and waits for its `ready` line. Inputs: the engine root and the
    /// codex executable. Output: the running engine, or a panic with its stderr.
    fn start(root: &Path, codex: &Path) -> Self {
        let mut child = crazyagent::spawn_gate::spawn(
            Command::new(env!("CARGO_BIN_EXE_crazyagent"))
                .arg("engine")
                .arg(root)
                .arg(root.join("global.toml"))
                .arg(root.join("project.toml"))
                .arg("engine.sock")
                .arg("--writable-settings")
                .arg("--codex-executable")
                .arg(codex)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped()),
        )
        .unwrap();
        let stdout = child.stdout.take().unwrap();
        let (send, ready) = mpsc::sync_channel(1);
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            let mut line = String::new();
            let _ = reader.read_line(&mut line);
            let _ = send.send(line);
            let _ = std::io::copy(&mut reader, &mut std::io::sink());
        });
        let mut stderr = child.stderr.take().unwrap();
        let stderr = std::thread::spawn(move || {
            let mut kept = Vec::new();
            let _ = (&mut stderr).take(STDERR_BYTES_MAX).read_to_end(&mut kept);
            let _ = std::io::copy(&mut stderr, &mut std::io::sink());
            String::from_utf8_lossy(&kept).into_owned()
        });
        let line = ready.recv_timeout(READY_TIME).unwrap_or_default();
        let mut engine = Self {
            child,
            stderr: Some(stderr),
        };
        if line != "ready\n" {
            let _ = engine.child.kill();
            let _ = engine.child.wait();
            panic!("engine not ready: {line:?} {}", engine.stderr_text());
        }
        engine
    }

    fn stderr_text(&mut self) -> String {
        self.stderr
            .take()
            .map(|handle| handle.join().unwrap_or_default())
            .unwrap_or_default()
    }

    /// Stops the engine with SIGTERM as the desktop app does and checks it exits cleanly.
    fn stop(mut self) {
        let signalled = crazyagent::spawn_gate::status(Command::new("/bin/kill").args([
            "-s",
            "TERM",
            &self.child.id().to_string(),
        ]))
        .unwrap();
        assert!(signalled.success());
        let deadline = Instant::now() + STOP_TIME;
        let status = loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                break status;
            }
            assert!(Instant::now() < deadline, "the engine did not stop");
            std::thread::sleep(Duration::from_millis(20));
        };
        let stderr = self.stderr_text();
        assert!(status.success(), "{stderr}");
    }
}

impl Drop for TeamEngine {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

fn now_ms() -> i64 {
    i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap()
}

/// A private engine root holding the settings files and a codex stand-in that only logs
/// what it reads; no run here reaches it. Output: the folder, removed when dropped, the
/// canonical root and the stand-in's path.
fn runtime() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let folder = scratch("ca-v2opt-rec-");
    let root = folder.path().canonicalize().unwrap();
    for (name, text) in [
        ("global.toml", "version = 1\ntimeout_ms = 5000\n"),
        ("project.toml", ""),
    ] {
        fs::write(root.join(name), text).unwrap();
        fs::set_permissions(root.join(name), fs::Permissions::from_mode(0o600)).unwrap();
    }
    let bin = root.join("bin");
    fs::create_dir(&bin).unwrap();
    fs::set_permissions(&bin, fs::Permissions::from_mode(0o700)).unwrap();
    let codex = bin.join("codex");
    fs::write(&codex, STAND_IN).unwrap();
    fs::set_permissions(&codex, fs::Permissions::from_mode(0o700)).unwrap();
    (folder, root, codex)
}

/// Every log the codex stand-in wrote below the engine root, found by walking at most
/// `FOLDERS_MAX` folders without following links. Input: the root. Output: the logs.
fn heard(root: &Path) -> Vec<PathBuf> {
    const FOLDERS_MAX: usize = 4096;
    let mut found = Vec::new();
    let mut folders = vec![root.to_path_buf()];
    let mut seen = 0;
    while let Some(folder) = folders.pop() {
        seen += 1;
        assert!(
            seen <= FOLDERS_MAX,
            "the engine root is larger than expected"
        );
        let Ok(entries) = fs::read_dir(&folder) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() {
                folders.push(entry.path());
            } else if entry.file_name() == "heard.log" {
                found.push(entry.path());
            }
        }
    }
    found
}

/// Picks a projects home and sets up the project through the owner protocol, as New
/// project does. Inputs: the attached owner and the home folder.
async fn create(owner: &mut OwnerController, home: &Path) {
    let set = owner
        .set_projects_home(home.to_str().unwrap().into(), 1)
        .await
        .unwrap();
    assert_eq!(result(set), OwnerResult::Applied);
    let created = owner
        .setup_project(
            ProjectId::new(PROJECT).unwrap(),
            ProjectSpec {
                name: "Buoy recovery".into(),
                repositories: vec![],
            },
            ProjectSetup {
                about: "Watches sensor buoys and raises alerts.".into(),
                tech_stack: vec!["rust".into()],
                platforms: vec![Platform::Website],
                research: vec![ResearchKind::MarketResearch],
                pool: None,
                team: None,
                documents: vec![],
            },
            2,
        )
        .await
        .unwrap();
    assert_eq!(result(created), OwnerResult::Applied);
}

/// Leaves one sub-agent marked in a turn with its running task, as an engine crash leaves
/// them, in the stopped engine's database. Input: the engine root. Output: the agent id.
fn crashed_in_a_turn(root: &Path) -> i64 {
    let db = Connection::open(root.join("engine.db")).unwrap();
    let at = now_ms();
    db.execute(
        "INSERT INTO team_agents(project,role,harness,tools,status,turns,created_ms,heartbeat_ms)
         VALUES (?1,'sub','codex',0,'working',1,?2,?2)",
        params![PROJECT, at],
    )
    .unwrap();
    let agent = db.last_insert_rowid();
    db.execute(
        "INSERT INTO team_tasks(project,source,harness,tools,prompt,status,agent,created_ms,started_ms)
         VALUES (?1,'master','codex',0,?2,'running',?3,?4,?4)",
        params![PROJECT, ASKED, agent, at],
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

/// The run of one agent as the engine stores it: its state and whether the owner retried
/// it, read without writing. Inputs: the engine root and the agent.
fn stored_run(root: &Path, agent: i64) -> Option<(String, bool)> {
    let db = Connection::open_with_flags(root.join("engine.db"), OpenFlags::SQLITE_OPEN_READ_ONLY)
        .unwrap();
    db.query_row(
        "SELECT state, retried FROM recovery_runs WHERE agent=?1 ORDER BY id DESC LIMIT 1",
        [agent],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )
    .optional()
    .unwrap()
}

/// Counts the project notes the engine wrote that contain a text. Inputs: the engine root
/// and the text.
fn notes(root: &Path, text: &str) -> i64 {
    let db = Connection::open_with_flags(root.join("engine.db"), OpenFlags::SQLITE_OPEN_READ_ONLY)
        .unwrap();
    db.query_row(
        "SELECT count(*) FROM team_messages WHERE project=?1 AND instr(text, ?2) > 0",
        params![PROJECT, text],
        |row| row.get(0),
    )
    .unwrap()
}

/// Reads the project's banner through the owner protocol until the check holds.
async fn banner_until(
    owner: &OwnerController,
    what: &str,
    want: impl Fn(&RecoveryBanner) -> bool,
) -> RecoveryBanner {
    let project = ProjectId::new(PROJECT).unwrap();
    let deadline = Instant::now() + WAIT;
    loop {
        let banner = owner.recovery_banner(project.clone()).await.unwrap();
        if want(&banner) {
            return banner;
        }
        assert!(Instant::now() < deadline, "{what}: {banner:?}");
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn recovery_retry_resumes_the_failed_run_again_and_lets_its_task_go_when_it_fails_twice() {
    let (_folder, root, codex) = runtime();
    let _server = installed(&root).map(|tmux| Server {
        tmux,
        socket: root.join(FOLDER).join(SOCKET),
    });
    let disk = scratch("ca-v2opt-rec-home-");
    let home = disk.path().join("Projects");
    fs::create_dir(&home).unwrap();

    let engine = TeamEngine::start(&root, &codex);
    let mut owner = OwnerController::attach(&root, OwnerClientKind::Tui, ATTACH_TIME)
        .await
        .unwrap();
    create(&mut owner, &home).await;
    drop(owner);
    engine.stop();
    let agent = crashed_in_a_turn(&root);

    let engine = TeamEngine::start(&root, &codex);
    let owner = OwnerController::attach(&root, OwnerClientKind::Tui, ATTACH_TIME)
        .await
        .unwrap();
    let banner = banner_until(
        &owner,
        "the resume fails and waits for the owner",
        |banner| {
            banner
                .runs
                .iter()
                .any(|run| run.agent == agent && run.state == RunShown::Failed)
        },
    )
    .await;
    let run = banner.runs.iter().find(|run| run.agent == agent).unwrap();
    assert_eq!(run.reason.as_deref(), Some(MISSING));
    assert_eq!(run.asked.as_deref(), Some(ASKED));
    assert_eq!(stored_run(&root, agent), Some(("failed".into(), false)));
    assert_eq!(notes(&root, WAITS), 1, "the project is told once");

    owner.recovery_retry(agent).await.unwrap();
    banner_until(
        &owner,
        "the retried resume fails and lets the task go",
        |banner| banner.runs.is_empty(),
    )
    .await;
    assert_eq!(
        stored_run(&root, agent),
        Some(("failed".into(), true)),
        "Retry made the engine resume the run once more"
    );
    assert_eq!(
        notes(&root, LET_GO),
        1,
        "the second failure lets the task go"
    );
    assert_eq!(notes(&root, WAITS), 1);
    assert_eq!(
        heard(&root),
        Vec::<PathBuf>::new(),
        "neither resume reached the codex stand-in"
    );
    drop(owner);
    engine.stop();
}

#[tokio::test(flavor = "multi_thread")]
async fn recovery_dismiss_closes_the_notice_once_and_it_stays_closed_after_a_restart() {
    let mut engine = e2e_support::Engine::start("ca-v2opt-dismiss-");
    let mut owner = Owner::attach(&engine).await;
    let disk = scratch("ca-v2opt-dismiss-home-");
    let home = disk.path().join("Projects");
    fs::create_dir(&home).unwrap();
    assert_eq!(owner.set_home(&home).await, OwnerResult::Applied);
    assert_eq!(
        owner.create_project(PROJECT, "Buoy recovery").await,
        OwnerResult::Applied
    );
    let project = ProjectId::new(PROJECT).unwrap();

    let db = Connection::open(engine.path().join("engine.db")).unwrap();
    db.busy_timeout(Duration::from_secs(5)).unwrap();
    db.execute(
        "INSERT INTO team_agents(project,role,harness,tools,status,created_ms,heartbeat_ms)
         VALUES (?1,'sub','codex',0,'stopped',?2,?2)",
        params![PROJECT, now_ms()],
    )
    .unwrap();
    let agent = db.last_insert_rowid();
    drop(db);
    let ledger_db = ledger::open(engine.path()).unwrap();
    let notice = ledger::begin(
        &ledger_db,
        ActionKind::AgentSpawn,
        &format!("team-{agent}"),
        &format!("{{\"team_agent\":{agent},\"pane\":false,\"unknown\":true}}"),
    )
    .unwrap();
    drop(ledger_db);

    let banner = owner
        .controller
        .recovery_banner(project.clone())
        .await
        .unwrap();
    assert_eq!(banner.notices.len(), 1, "{banner:?}");
    let shown = &banner.notices[0];
    assert_eq!(
        (shown.action_kind, shown.id, shown.agent, shown.shown),
        (
            ActionKind::AgentSpawn,
            notice.get(),
            Some(agent),
            NoticeShown::Unknown
        )
    );

    owner
        .controller
        .recovery_dismiss(ActionKind::AgentSpawn, notice.get())
        .await
        .unwrap();
    let banner = owner
        .controller
        .recovery_banner(project.clone())
        .await
        .unwrap();
    assert!(banner.notices.is_empty(), "{banner:?}");
    let db = Connection::open_with_flags(
        engine.path().join("engine.db"),
        OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let outcome: Option<String> = db
        .query_row(
            "SELECT outcome FROM ledger_actions WHERE id=?1",
            [notice.get()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(outcome.as_deref(), Some("unknown"), "the notice is closed");
    drop(db);

    engine.restart();
    let owner = Owner::attach(&engine).await;
    let banner = owner.controller.recovery_banner(project).await.unwrap();
    assert!(
        banner.notices.is_empty(),
        "dismissed stays dismissed: {banner:?}"
    );
    let again = owner
        .controller
        .recovery_dismiss(ActionKind::AgentSpawn, notice.get())
        .await
        .expect_err("a closed notice is not dismissed twice");
    assert_eq!(again.0, "ledger_finished");
}
