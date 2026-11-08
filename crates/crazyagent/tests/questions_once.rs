#![cfg(target_os = "macos")]

use crazyagent::client_controller::OwnerController;
use crazyagent::engine::owner::OwnerClientKind;
use crazyagent::project_setup::{HarnessSlot, StoredSetup, TeamPlan};
use crazyagent::protocol::{Outcome, ProjectId, ProjectSpec, StoredProject};
use crazyagent::questions::{Answer, Ask, Asker, Settlement};
use crazyagent::team::TeamAction;
use crazyagent::terminal::tmux::{SOCKET, Tmux, installed};
use crazyagent::terminal::{AgentId, FOLDER, Host};
use rusqlite::{Connection, OptionalExtension};
use std::fs;
use std::io::{BufRead, BufReader, Read};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const PROJECT: &str = "project-questions";
const WAIT: Duration = Duration::from_secs(90);
const WATCHED: Duration = Duration::from_secs(3);
const ATTACH_TIME: Duration = Duration::from_secs(10);
const READY_TIME: Duration = Duration::from_secs(30);
const STOP_TIME: Duration = Duration::from_secs(40);
const STDERR_BYTES_MAX: u64 = 65536;

const CATALOG: &str = "version = 1\ntimeout_ms = 5000\nallowed_destinations = []\n\n[model_access]\naccounts = [\"local-account\"]\n\n[[model_access.connections]]\nid = \"local-connection\"\nprovider = \"openai-compatible\"\nharness = \"rcli\"\nprotocol = \"openai-chat\"\naccount_ref = \"local-account\"\nendpoint = { kind = \"http\", url = \"http://127.0.0.1:11434/v1\" }\nmodel = { id = \"qwen2.5:7b\", author = \"openai-compatible\" }\nauth = { method = \"none\" }\n\n[[model_access.pools]]\nid = \"local-pool\"\nmembers = [\"local-connection\"]\nprojects = [\"project-questions\"]\nroles = [\"project_leader\"]\nfallback_allowed = false\n";

const LIVE_CODEX: &str = r#"#!/bin/sh
d="$CODEX_HOME/sessions/2026/10/28"
/bin/mkdir -p "$d" || exit 1
f="$d/rollout-2026-10-28T00-00-00-$$.jsonl"
printf '{"type":"session_meta","payload":{"cwd":"%s"}}\n' "$(/bin/pwd -P)" > "$f"
printf '\033[?2004hCodex stand-in ready\n'
e=$(printf '\033')
open=0
while IFS= read -r line; do
  printf '%s\n' "$line" >> "$CODEX_HOME/heard.log"
  case "$line" in
    *"$e[201~"*) open=0 ;;
    *"$e[200~"*) open=1; continue ;;
    *) [ "$open" = 1 ] && continue ;;
  esac
  printf '{"type":"event_msg","payload":{"type":"task_started"}}\n' >> "$f"
  case "$line" in
    *"hold the turn"*) while [ ! -e "$CODEX_HOME/release" ]; do /bin/sleep 0.2; done ;;
  esac
  /bin/sleep 0.3
  printf '{"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"Done."}]}}\n' >> "$f"
  printf '{"type":"event_msg","payload":{"type":"task_complete"}}\n' >> "$f"
done
"#;

/// The engine's private tmux server. The live master's pane outlives a killed engine, so
/// dropping this ends the server and every pane left in it, also when the test fails.
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

/// The real `crazyagent engine` process on a private root with writable settings and a
/// codex executable, so it runs its team runtime as the desktop app starts it.
struct Engine {
    child: Child,
    stderr: Option<std::thread::JoinHandle<String>>,
}

impl Engine {
    /// Starts the engine and waits for its `ready` line. Inputs: the engine root and the
    /// codex executable. Output: the running engine, or a panic with its stderr.
    fn start(runtime: &Path, codex: &Path) -> Self {
        let mut child = crazyagent::spawn_gate::spawn(
            Command::new(env!("CARGO_BIN_EXE_crazyagent"))
                .arg("engine")
                .arg(runtime)
                .arg(runtime.join("global.toml"))
                .arg(runtime.join("project.toml"))
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

    /// Kills the engine with SIGKILL, as a crash or a forced quit ends it, so nothing it
    /// would do on a clean stop runs. Output: none once the process is gone.
    fn kill(mut self) {
        self.child.kill().unwrap();
        let status = self.child.wait().unwrap();
        assert!(!status.success(), "the engine was killed, not stopped");
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

impl Drop for Engine {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

/// A private engine root under the real temporary folder, short enough for the engine's
/// sockets, holding the local pool as the global settings and the codex stand-in the live
/// master runs. Output: the folder, removed when dropped, and the stand-in's path.
fn runtime() -> (tempfile::TempDir, PathBuf) {
    let folder = tempfile::Builder::new()
        .prefix("ca-qq-")
        .tempdir_in(fs::canonicalize("/tmp").unwrap())
        .unwrap();
    let root = folder.path().canonicalize().unwrap();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
    for (name, text) in [("global.toml", CATALOG), ("project.toml", "")] {
        fs::write(root.join(name), text).unwrap();
        fs::set_permissions(root.join(name), fs::Permissions::from_mode(0o600)).unwrap();
    }
    let bin = root.join("bin");
    fs::create_dir(&bin).unwrap();
    fs::set_permissions(&bin, fs::Permissions::from_mode(0o700)).unwrap();
    let codex = bin.join("codex");
    fs::write(&codex, LIVE_CODEX).unwrap();
    fs::set_permissions(&codex, fs::Permissions::from_mode(0o700)).unwrap();
    (folder, codex)
}

/// Adds the project and its team setup to the running engine's database, as the setup flow
/// stores them: a tools-off codex master and one codex sub-agent slot. Inputs: the database
/// and the engine root. Output: the setup.
fn project(database: &Connection, root: &Path) -> StoredSetup {
    let folder = root.join("projects").join(PROJECT);
    fs::create_dir_all(folder.join("context")).unwrap();
    fs::write(
        folder.join("context").join("brief.md"),
        "# Ship day\n\nA release that waits for the owner's call.\n",
    )
    .unwrap();
    let folder = folder.canonicalize().unwrap();
    let setup = StoredSetup {
        project_id: ProjectId::new(PROJECT).unwrap(),
        name: "Ship day".into(),
        about: "A release that waits for the owner's call.".into(),
        tech_stack: vec![],
        platforms: vec![],
        research: vec![],
        pool: None,
        team: TeamPlan {
            master: "codex".into(),
            master_tools: false,
            sub_harnesses: vec![HarnessSlot {
                harness: "codex".into(),
                agents: 1,
                tools: false,
            }],
            min_agents: 1,
            max_agents: 2,
        },
        documents: vec![],
        folder: folder.to_str().unwrap().into(),
        created_ms: 1,
        revision: 1,
    };
    let row = StoredProject {
        project_id: setup.project_id.clone(),
        spec: ProjectSpec {
            name: setup.name.clone(),
            repositories: vec![],
        },
    };
    database
        .execute(
            "INSERT INTO projects VALUES (?1, ?2)",
            rusqlite::params![PROJECT, serde_json::to_string(&row).unwrap()],
        )
        .unwrap();
    database
        .execute(
            "INSERT INTO project_setups(project, body) VALUES (?1, ?2)",
            rusqlite::params![PROJECT, serde_json::to_string(&setup).unwrap()],
        )
        .unwrap();
    setup
}

/// Applies one owner team action in its own transaction, as the owner socket does.
/// Inputs: the database, the engine root and the action. Output: none; the action applied.
fn act(database: &mut Connection, root: &Path, action: TeamAction) {
    let access =
        crazyagent::config::resolve_config(&root.join("global.toml"), &root.join("project.toml"))
            .unwrap()
            .settings
            .model_access
            .unwrap();
    let tx = database.transaction().unwrap();
    assert_eq!(
        crazyagent::team::execute(&tx, &access, &action).unwrap(),
        Outcome::Applied,
        "{action:?}"
    );
    tx.commit().unwrap();
}

/// Reads the live master's turn count and whether a turn is open. Input: the database.
/// Output: both, or none before the live switch has a row.
fn live_turns(database: &Connection) -> Option<(i64, bool)> {
    database
        .query_row(
            "SELECT turns,turn_open FROM team_live_masters WHERE project=?1",
            [PROJECT],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .unwrap()
}

/// Reads the team agent the live master switch runs. Input: the database. Output: the
/// agent, or none before the runtime started one.
fn live_agent(database: &Connection) -> Option<i64> {
    database
        .query_row(
            "SELECT agent FROM team_live_masters WHERE project=?1",
            [PROJECT],
            |row| row.get(0),
        )
        .optional()
        .unwrap()
        .flatten()
}

fn state(database: &Connection, message: i64) -> String {
    database
        .query_row(
            "SELECT state FROM team_messages WHERE id=?1",
            [message],
            |row| row.get(0),
        )
        .unwrap()
}

/// Counts how many times the live master's terminal took the answer in. Inputs: the log of
/// every line the master read and the answer's marker. Output: the count, 0 with no log.
fn heard(log: &Path, marker: &str) -> usize {
    fs::read(log)
        .map(|bytes| String::from_utf8_lossy(&bytes).matches(marker).count())
        .unwrap_or(0)
}

fn until(what: &str, done: impl Fn() -> bool) {
    let deadline = Instant::now() + WAIT;
    while !done() {
        assert!(Instant::now() < deadline, "{what}");
        std::thread::sleep(Duration::from_millis(250));
    }
}

/// An owner answer the engine settled while the live master was busy waits as one owner
/// message; the engine is killed before it types it, and the restarted engine types it
/// into the master exactly once and never queues it again. The master starts quiet, its
/// instructions read as system context with nothing typed, so its first turn is the owner's.
#[test]
fn an_answer_settled_before_an_engine_kill_reaches_the_live_master_once_after_the_restart() {
    let (_folder, codex) = runtime();
    let root = codex.parent().unwrap().parent().unwrap().to_path_buf();
    let tmux = installed(&root).expect("tmux 3.x must be installed on this machine");
    let _server = Server {
        tmux: tmux.clone(),
        socket: root.join(FOLDER).join(SOCKET),
    };
    let engine = Engine::start(&root, &codex);
    let mut database = Connection::open(root.join("engine.db")).unwrap();
    database.busy_timeout(Duration::from_secs(5)).unwrap();
    let setup = project(&database, &root);
    let project = setup.project_id.clone();
    let version = crazyagent::team::settings_version(&database, &project).unwrap();
    act(
        &mut database,
        &root,
        TeamAction::LiveMaster {
            project_id: project.clone(),
            on: true,
            version,
        },
    );
    let host = Tmux::open(Some(tmux.clone()), &root.join(FOLDER));
    until("the live master's pane runs", || {
        live_agent(&database).is_some_and(|agent| {
            host.alive()
                .is_ok_and(|alive| alive.contains(&AgentId::team(agent).unwrap()))
        })
    });
    let master: i64 = database
        .query_row(
            "SELECT id FROM team_agents WHERE project=?1 AND role='master'",
            [PROJECT],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(live_agent(&database), Some(master));
    let home = crazyagent::team::runtime::agent_home(&root.join("team"), master);
    let log = home.join("heard.log");
    std::thread::sleep(WATCHED);
    assert_eq!(
        live_turns(&database),
        Some((0, false)),
        "the master starts quiet"
    );
    assert!(
        !log.exists(),
        "nothing is typed into the master at its start"
    );

    let question = crazyagent::questions::ask(
        &database,
        &project,
        &Asker::Master(master),
        &Ask {
            question: "Ship on Friday?".into(),
            options: vec![],
            recommended: Some("Ship it".into()),
            context: None,
        },
    )
    .unwrap();
    host.input(&AgentId::team(master).unwrap(), b"hold the turn\r")
        .unwrap();
    until("the owner's own turn keeps the master busy", || {
        live_turns(&database).is_some_and(|(_, open)| open)
    });

    let marker = format!(
        "Ship it on Friday {}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let settled = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let mut owner = OwnerController::attach(&root, OwnerClientKind::Tui, ATTACH_TIME)
                .await
                .unwrap();
            let settled = owner
                .questions_answer(question, 0, Answer::Text(marker.clone()))
                .await;
            owner.detach();
            settled
        });
    assert_eq!(settled.unwrap(), Settlement::Settled);
    let message: i64 = database
        .query_row(
            "SELECT message FROM questions_log WHERE id=?1",
            [i64::try_from(question).unwrap()],
            |row| row.get(0),
        )
        .unwrap();
    let watched = Instant::now() + WATCHED;
    while Instant::now() < watched {
        assert_eq!(
            state(&database, message),
            "pending",
            "the busy master is not typed into"
        );
        std::thread::sleep(Duration::from_millis(250));
    }
    assert_eq!(heard(&log, &marker), 0, "settled, not yet delivered");

    engine.kill();
    assert!(
        host.alive()
            .unwrap()
            .contains(&AgentId::team(master).unwrap()),
        "the live master's pane outlives the killed engine"
    );
    assert_eq!(state(&database, message), "pending");
    fs::write(home.join("release"), b"").unwrap();

    let engine = Engine::start(&root, &codex);
    until(
        "the restarted engine types the answer into the master",
        || state(&database, message) == "answered",
    );
    std::thread::sleep(WATCHED);
    assert_eq!(
        heard(&log, &marker),
        1,
        "{}",
        String::from_utf8_lossy(&fs::read(&log).unwrap())
    );
    assert_eq!(state(&database, message), "answered");
    let queued: i64 = database
        .query_row(
            "SELECT COUNT(*) FROM team_messages WHERE project=?1 AND instr(text, ?2) > 0",
            rusqlite::params![PROJECT, marker],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(queued, 1, "the answer was queued once");
    let (status, answer): (String, String) = database
        .query_row(
            "SELECT status,answer FROM questions_log WHERE id=?1",
            [i64::try_from(question).unwrap()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(
        (status.as_str(), answer.as_str()),
        ("answered", marker.as_str())
    );
    engine.stop();
}
