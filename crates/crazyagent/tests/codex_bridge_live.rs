#![cfg(target_os = "macos")]

use crazyagent::coord::adoption::Flag;
use crazyagent::coord::delivery::codex::{self, Bridged, CONTROL_SOCKET};
use crazyagent::coord::delivery::{Deliveries, NOTICES_KEY, RETRY_MS, Reach};
use crazyagent::coord::{MessageId, MessageKind, NewMessage, Party};
use crazyagent::harnesses::interactive::{Run, command};
use crazyagent::harnesses::launch::{AccountBinding, LaunchLimits, LaunchRequest};
use crazyagent::harnesses::{HarnessStatus, Search, catalog};
use crazyagent::protocol::ProjectId;
use crazyagent::sandbox::{ProjectSandbox, SandboxNetwork, SandboxRemote};
use crazyagent::team::runtime::agent_home;
use crazyagent::terminal::tmux::{SOCKET, Tmux, installed};
use crazyagent::terminal::{AgentId, FOLDER, Host};
use std::collections::BTreeSet;
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const OLLAMA_OPENAI: &str = "http://127.0.0.1:11434/v1";
const LOCAL_MODEL: &str = "qwen2.5:0.5b";
const SESSION: &str = "0199a0b0-0000-4000-8000-000000000001";
const GREETING: &str = "Reply with one short greeting.";
const LONG_TURN: &str =
    "Write the numbers from 1 to 3000 in words, one per line, with no other text.";
const STEER: &str = "Stop counting now and reply with the single word steered.";
const NOTICE: &str = "Owner notice: the owner woke worker 7 again in its own worktree.";
const PROJECT: &str = "bridge-live";
const START: Duration = Duration::from_secs(60);
const TURN: Duration = Duration::from_secs(400);

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

/// Makes a private folder for one part of the agent's world.
/// Input: the path. Output: the canonical path, created with mode 0700.
fn private(path: &Path) -> PathBuf {
    std::fs::create_dir_all(path).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
    path.canonicalize().unwrap()
}

/// Finds the installed Codex and checks it is a release the bridge was checked against.
/// Input: nothing. Output: the canonical executable; panics when Codex is absent or is
/// not one of `codex::VERSIONS`.
fn installed_codex() -> PathBuf {
    let entries = catalog(&Search::from_environment());
    let entry = entries.iter().find(|entry| entry.id == "codex").unwrap();
    assert_eq!(
        entry.status,
        HarnessStatus::Ready,
        "UNAVAILABLE: codex must be installed for the live bridge check"
    );
    let executable = std::fs::canonicalize(entry.executable.as_ref().unwrap()).unwrap();
    let output = crazyagent::spawn_gate::output(
        Command::new(&executable)
            .arg("--version")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()),
    )
    .unwrap();
    let version = String::from_utf8(output.stdout).unwrap();
    assert!(
        codex::VERSIONS
            .iter()
            .any(|(major, minor)| version.contains(&format!(" {major}.{minor}."))),
        "UNAVAILABLE: the bridge was not checked against {version}"
    );
    executable
}

/// Tells whether any Codex record under the agent home holds a text.
/// Input: the agent's `CODEX_HOME` and the text. Output: whether a rollout contains it.
fn recorded(home: &Path, text: &str) -> bool {
    let mut folders = vec![home.join("sessions")];
    while let Some(folder) = folders.pop() {
        let Ok(entries) = std::fs::read_dir(&folder) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let kind = entry.file_type().unwrap();
            if kind.is_dir() {
                folders.push(path);
            } else if kind.is_file()
                && std::fs::read_to_string(&path).is_ok_and(|record| record.contains(text))
            {
                return true;
            }
        }
    }
    false
}

/// Waits for a condition on the agent home, failing with what was awaited.
/// Input: how long, what is awaited and the check. Output: nothing; panics on timeout.
fn until(limit: Duration, what: &str, done: impl Fn() -> bool) {
    let deadline = Instant::now() + limit;
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Launches a real Codex app with the engine's own command in tmux, inside the real project
/// sandbox with only its control socket granted, and waits for its app server's socket.
/// Input: the private base folder, the agent's private `CODEX_HOME` and its terminal id.
/// Output: the tmux server guard and host; panics when the app never binds its socket.
fn start_app(base: &Path, home: &Path, agent: &AgentId) -> (Server, Tmux) {
    let project = private(&base.join("project"));
    let tmp = private(&base.join("tmp"));
    let executable = installed_codex();
    let socket = codex::socket(home).unwrap();
    codex::prepare_folder().unwrap();
    let sandbox = ProjectSandbox::new(
        &project,
        home,
        &tmp,
        SandboxNetwork::Only(vec![SandboxRemote::for_endpoint(OLLAMA_OPENAI).unwrap()]),
    )
    .unwrap()
    .allow_read(executable.parent().unwrap())
    .unwrap()
    .allow_unix_socket(&socket)
    .unwrap();
    let profile = base.join("agent.sb");
    sandbox.install(&profile).unwrap();
    let request = LaunchRequest {
        harness: "codex".into(),
        executable,
        working_dir: project,
        account: AccountBinding::Url {
            home: home.to_path_buf(),
            base_url: OLLAMA_OPENAI.into(),
            key: None,
        },
        model: Some(LOCAL_MODEL.into()),
        tools: false,
        sandbox_profile: Some(profile),
        unconfined: false,
        git_identity: None,
        limits: LaunchLimits::default(),
    };
    let built = command(
        &request,
        None,
        &Run {
            session_id: SESSION,
            key_file: None,
            resume: false,
        },
    )
    .unwrap();

    let tmux = installed(base).expect("tmux must be installed on this machine");
    let folder = base.join(FOLDER);
    let server = Server {
        tmux: tmux.clone(),
        socket: folder.join(SOCKET),
    };
    let host = Tmux::open(Some(tmux), &folder);
    host.spawn(agent, &built).unwrap();
    until(START, "the app server's control socket", || {
        std::fs::metadata(home.join(CONTROL_SOCKET))
            .is_ok_and(|socket| socket.file_type().is_socket())
    });
    (server, host)
}

/// Starts the app's first turn through its app server once it takes one, and waits for
/// the reply. Input: the agent's `CODEX_HOME`. Output: nothing; panics on timeout.
fn greet(home: &Path) {
    let deadline = Instant::now() + START;
    loop {
        match codex::deliver(home, GREETING, true) {
            Bridged::Delivered => break,
            Bridged::Unavailable(reason) => {
                assert!(
                    Instant::now() < deadline,
                    "the idle app never took a turn through its app server: {reason}"
                );
                std::thread::sleep(Duration::from_millis(500));
            }
        }
    }
    until(TURN, "the delivered greeting and its reply", || {
        recorded(home, GREETING) && recorded(home, "\"role\":\"assistant\"")
    });
}

/// The time now in ms since the Unix epoch. Input: nothing. Output: the time.
fn now_ms() -> i64 {
    i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap()
}

/// K2b live: a real Codex app launched by the engine's own command, inside the real project
/// sandbox with only its control socket granted, starts its app server through the bridge
/// and attaches to it. `codex::deliver` then starts a turn while the app is idle and
/// steers the running turn while it works. Needs Codex 0.164 and a local Ollama model.
#[test]
fn a_real_codex_app_takes_coord_messages_through_its_app_server_idle_and_busy() {
    let root = tempfile::Builder::new()
        .prefix("ca-bridge-")
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    let base = private(root.path());
    let home = private(&base.join("home"));
    let agent = AgentId::team(1).unwrap();
    let (_server, host) = start_app(&base, &home, &agent);
    greet(&home);

    assert_eq!(codex::deliver(&home, LONG_TURN, true), Bridged::Delivered);
    until(TURN, "the long turn to start", || {
        recorded(&home, LONG_TURN)
    });
    assert_eq!(codex::deliver(&home, STEER, true), Bridged::Delivered);
    until(TURN, "the steer to reach the running turn", || {
        recorded(&home, STEER)
    });
    host.kill(&agent).unwrap();
}

/// owner-wake r2 OWAKE2-M1 live: the team's delivery tick steers a working Codex agent's
/// waiting notes into its running turn through the bridge, an owner notice alone and an
/// adoption nudge alike, so neither waits for the agent's next task. Needs Codex 0.164 and
/// a local Ollama model.
#[test]
fn a_working_codex_agent_gets_an_owner_notice_and_a_nudge_steered_by_the_delivery_tick() {
    let root = tempfile::Builder::new()
        .prefix("ca-bridge-")
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    let base = private(root.path());
    let state = private(&base.join("state"));
    let db = rusqlite::Connection::open(base.join("engine.db")).unwrap();
    crazyagent::team::initialize(&db).unwrap();
    crazyagent::coord::ensure(&db).unwrap();
    db.execute_batch(
        "CREATE TABLE IF NOT EXISTS projects (id TEXT PRIMARY KEY, body TEXT NOT NULL);",
    )
    .unwrap();
    db.execute("INSERT INTO projects(id,body) VALUES (?1,'{}')", [PROJECT])
        .unwrap();
    db.execute(
        "INSERT INTO team_agents(project,role,harness,tools,status,created_ms,heartbeat_ms)
         VALUES (?1,'sub','codex',0,'working',1,1)",
        [PROJECT],
    )
    .unwrap();
    let id = db.last_insert_rowid();
    let home = private(&agent_home(&state, id));
    assert_eq!(home, agent_home(&state, id));
    let agent = AgentId::team(id).unwrap();
    let (_server, host) = start_app(&base, &home, &agent);
    greet(&home);
    assert_eq!(codex::deliver(&home, LONG_TURN, true), Bridged::Delivered);
    until(TURN, "the long turn to start", || {
        recorded(&home, LONG_TURN)
    });

    let project = ProjectId::new(PROJECT).unwrap();
    let send = |body: &str, key: String| -> MessageId {
        crazyagent::coord::send(
            &db,
            NewMessage {
                project: project.clone(),
                from: Party::Engine,
                to: Party::Agent(id),
                kind: MessageKind::Note,
                question: None,
                work: None,
                reply_to: None,
                body: body.to_owned(),
                idempotency_key: Some(key),
            },
            now_ms(),
        )
        .unwrap()
    };
    let receipt = |message: MessageId| -> String {
        db.query_row(
            "SELECT receipt FROM coord_messages WHERE id=?1",
            [message],
            |row| row.get(0),
        )
        .unwrap()
    };
    let awake = BTreeSet::new();
    let reach = Reach {
        host: Some(&host as &dyn Host),
        owner: None,
        state: &state,
        asleep: &awake,
    };
    let mut deliveries = Deliveries::default();
    let notice = send(NOTICE, format!("{NOTICES_KEY}{id}"));
    deliveries.tick(&db, &project, now_ms(), &reach).unwrap();
    let reminder = Flag::EditsWithoutWork.reminder();
    let nudge = send(reminder, format!("adoption-edits_without_start_work-{id}"));
    deliveries
        .tick(&db, &project, now_ms() + RETRY_MS, &reach)
        .unwrap();
    assert_eq!(
        (receipt(notice), receipt(nudge)),
        ("delivered".to_owned(), "delivered".to_owned()),
        "the working agent's notice and nudge went through the bridge at once"
    );
    until(
        TURN,
        "the steered notice and nudge in the agent's records",
        || recorded(&home, NOTICE) && recorded(&home, reminder),
    );
    host.kill(&agent).unwrap();
}
