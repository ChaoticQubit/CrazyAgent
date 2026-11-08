#![cfg(target_os = "macos")]

mod pgid_support;

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
const BUSY_TURN: &str =
    "Write the numbers from 1 to 100 in words, one per line, with no other text.";
const STEER: &str = "Stop counting now and reply with the single word steered.";
const NOTICE: &str = "Owner notice: the owner woke worker 7 again in its own worktree.";
const REQUEUED: &str = "Reply with the single word requeued.";
const PROJECT: &str = "bridge-live";
const START: Duration = Duration::from_secs(60);
const TURN: Duration = Duration::from_secs(400);
const ABORT: Duration = Duration::from_secs(60);
const ESC: &[u8] = b"\x1b";
const GROUP_GONE: Duration = Duration::from_secs(10);
const TURN_EVENTS: [&str; 3] = [
    r#""type":"task_started""#,
    r#""type":"task_complete""#,
    r#""type":"turn_aborted""#,
];

/// A test's private tmux server and the process group of the app's pane, read from tmux
/// once the app started. Dropping it kills that group and waits until it is gone, so the
/// app server the bridge started in the background never outlives the test and goes on
/// with its turn against the shared local model, and then ends the server.
struct Server {
    tmux: PathBuf,
    socket: PathBuf,
    group: Option<rustix::process::Pid>,
}

impl Drop for Server {
    fn drop(&mut self) {
        if let Some(group) = self.group {
            let deadline = Instant::now() + GROUP_GONE;
            while rustix::process::test_kill_process_group(group) != Err(rustix::io::Errno::SRCH)
                && Instant::now() < deadline
            {
                let _ = rustix::process::kill_process_group(group, rustix::process::Signal::KILL);
                std::thread::sleep(Duration::from_millis(10));
            }
        }
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

/// Reads a Codex record under the agent home that holds a text.
/// Input: the agent's `CODEX_HOME` and the text. Output: the first such rollout's content.
fn rollout_with(home: &Path, text: &str) -> Option<String> {
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
                && let Ok(record) = std::fs::read_to_string(&path)
                && record.contains(text)
            {
                return Some(record);
            }
        }
    }
    None
}

/// Tells whether any Codex record under the agent home holds a text.
/// Input: the agent's `CODEX_HOME` and the text. Output: whether a rollout contains it.
fn recorded(home: &Path, text: &str) -> bool {
    rollout_with(home, text).is_some()
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

/// Tells whether steered texts joined the running turn of a prompt: the rollout holding the
/// prompt records each text after it with no turn start, end or abort in between, so no
/// text reached the agent as a new turn. Input: the agent's `CODEX_HOME`, the prompt and the
/// texts. Output: Ok, or the first text outside that turn and the turn event before it.
fn joined(home: &Path, prompt: &str, texts: &[&str]) -> Result<(), String> {
    let record = rollout_with(home, prompt).ok_or_else(|| format!("no record holds {prompt:?}"))?;
    let start = record.find(prompt).unwrap();
    for text in texts {
        let Some(at) = record[start..].find(text) else {
            return Err(format!("{text:?} is not recorded after {prompt:?}"));
        };
        let between = &record[start..start + at];
        if let Some(event) = TURN_EVENTS.iter().find(|event| between.contains(*event)) {
            return Err(format!(
                "{text:?} joined no running turn: {event} lies between {prompt:?} and it"
            ));
        }
    }
    Ok(())
}

/// Waits for the texts steered into a running turn to reach the agent's records.
/// Input: how long, the agent's `CODEX_HOME`, the running turn's prompt and the steered
/// texts. Output: Ok, or on timeout the texts still missing and whether the model's step
/// on the prompt was still running.
fn steered(limit: Duration, home: &Path, prompt: &str, texts: &[&str]) -> Result<(), String> {
    let deadline = Instant::now() + limit;
    loop {
        let missing: Vec<&str> = texts
            .iter()
            .copied()
            .filter(|text| !recorded(home, text))
            .collect();
        if missing.is_empty() {
            return Ok(());
        }
        if Instant::now() >= deadline {
            let step = match rollout_with(home, prompt) {
                None => "never started (the prompt is not recorded)",
                Some(record) => {
                    let start = record.find(prompt).unwrap();
                    if record[start..].contains(r#""role":"assistant""#) {
                        "had ended (an assistant record follows it)"
                    } else {
                        "was still running (no assistant record after it)"
                    }
                }
            };
            return Err(format!(
                "timed out waiting for the steered {missing:?}; the model's step on {prompt:?} {step}"
            ));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Launches a real Codex app with the engine's own command in tmux, inside the real project
/// sandbox with only its control socket granted, and waits for its app server's socket.
/// Input: the private base folder, the agent's private `CODEX_HOME` and its terminal id.
/// Output: the tmux server guard, holding the app pane's process group, and the host;
/// panics when the app never binds its socket.
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
        tools: true,
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
    let mut server = Server {
        tmux: tmux.clone(),
        socket: folder.join(SOCKET),
        group: None,
    };
    let host = Tmux::open(Some(tmux), &folder);
    let pane = host.spawn(agent, &built).unwrap();
    server.group = Some(pgid_support::pane_group(pane.pid));
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
            Bridged::Delivered(_) => break,
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

/// The wait for steered texts, on a fake agent home: on timeout it names each text still
/// missing, and it tells a model step still running on the prompt (only an older assistant
/// record) from one that ended. Needs no Codex and no model.
#[test]
fn a_steer_timeout_names_each_missing_text_and_whether_the_step_still_runs() {
    let root = tempfile::tempdir().unwrap();
    let day = root.path().join("sessions/2026/11/06");
    std::fs::create_dir_all(&day).unwrap();
    let rollout = day.join("rollout.jsonl");
    let line = |role: &str, text: &str| {
        format!(
            "{{\"type\":\"message\",\"role\":\"{role}\",\"content\":[{{\"text\":\"{text}\"}}]}}\n"
        )
    };
    let running = [
        line("user", GREETING),
        line("assistant", "Hello."),
        line("user", BUSY_TURN),
        line("user", NOTICE),
    ]
    .concat();
    std::fs::write(&rollout, &running).unwrap();
    let timeout = steered(Duration::ZERO, root.path(), BUSY_TURN, &[NOTICE, STEER]).unwrap_err();
    assert!(timeout.contains(&format!("{STEER:?}")), "{timeout}");
    assert!(!timeout.contains(&format!("{NOTICE:?}")), "{timeout}");
    assert!(timeout.contains("still running"), "{timeout}");

    std::fs::write(&rollout, running + &line("assistant", "one")).unwrap();
    let timeout = steered(Duration::ZERO, root.path(), BUSY_TURN, &[STEER]).unwrap_err();
    assert!(timeout.contains(&format!("{STEER:?}")), "{timeout}");
    assert!(timeout.contains("ended"), "{timeout}");
    assert!(!timeout.contains("still running"), "{timeout}");
    assert_eq!(
        steered(Duration::ZERO, root.path(), BUSY_TURN, &[NOTICE]),
        Ok(())
    );
}

/// The proof that a steer joined a running turn, on a fake agent home: a text recorded in
/// the prompt's turn joined it, and one recorded after that turn ended, aborted or another
/// began did not. Needs no Codex and no model.
#[test]
fn a_steered_text_joined_the_busy_turn_only_with_no_turn_event_before_it() {
    let root = tempfile::tempdir().unwrap();
    let day = root.path().join("sessions/2026/11/06");
    std::fs::create_dir_all(&day).unwrap();
    let rollout = day.join("rollout.jsonl");
    let user = |text: &str| {
        format!(
            "{{\"type\":\"response_item\",\"payload\":{{\"type\":\"message\",\"role\":\"user\",\"content\":[{{\"text\":\"{text}\"}}]}}}}\n"
        )
    };
    let event = |kind: &str| {
        format!(
            "{{\"type\":\"event_msg\",\"payload\":{{\"type\":\"{kind}\",\"turn_id\":\"t\"}}}}\n"
        )
    };
    let busy = [event("task_started"), user(BUSY_TURN), user(NOTICE)].concat();
    std::fs::write(&rollout, &busy).unwrap();
    assert_eq!(joined(root.path(), BUSY_TURN, &[NOTICE]), Ok(()));
    let missing = joined(root.path(), BUSY_TURN, &[STEER]).unwrap_err();
    assert!(missing.contains("not recorded"), "{missing}");
    for kind in ["task_complete", "turn_aborted", "task_started"] {
        std::fs::write(&rollout, [busy.clone(), event(kind), user(STEER)].concat()).unwrap();
        let stray = joined(root.path(), BUSY_TURN, &[NOTICE, STEER]).unwrap_err();
        assert!(stray.contains(&format!("{STEER:?}")), "{stray}");
        assert!(stray.contains(kind), "{stray}");
    }
}

/// K2b live: a real tools-on Codex app launched by the engine's own command, inside the real
/// project sandbox with only its control socket granted, starts its app server through the
/// bridge and attaches to it (a tools-off app runs without the shared app server and is
/// reached by typing). `codex::deliver` then starts a turn while the app is idle and steers
/// the running turn while it works. Needs Codex 0.164 and a local Ollama model.
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

    assert!(matches!(
        codex::deliver(&home, BUSY_TURN, true),
        Bridged::Delivered(_)
    ));
    until(TURN, "the busy turn to start", || {
        recorded(&home, BUSY_TURN)
    });
    assert!(matches!(
        codex::deliver(&home, STEER, false),
        Bridged::Delivered(_)
    ));
    if let Err(problem) =
        steered(TURN, &home, BUSY_TURN, &[STEER]).and_then(|()| joined(&home, BUSY_TURN, &[STEER]))
    {
        panic!(
            "{problem}; the test folder is kept at {}",
            root.keep().display()
        );
    }
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
    assert!(matches!(
        codex::deliver(&home, BUSY_TURN, true),
        Bridged::Delivered(_)
    ));
    until(TURN, "the busy turn to start", || {
        recorded(&home, BUSY_TURN)
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
        held: &awake,
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
    if let Err(problem) = steered(TURN, &home, BUSY_TURN, &[NOTICE, reminder])
        .and_then(|()| joined(&home, BUSY_TURN, &[NOTICE, reminder]))
    {
        panic!(
            "{problem}; the test folder is kept at {}",
            root.keep().display()
        );
    }
    host.kill(&agent).unwrap();
}

/// b19 P2 live: the delivery tick steers a message into a working Codex agent's busy turn
/// and keeps that turn's id, the message waits there for the model's step to end, and the
/// owner's Esc in the agent's terminal aborts that turn first, so Codex drops it. The
/// agent's receipts read that turn's abort in its rollout and put the message back to sent,
/// and the next tick delivers it once, in a new turn. Needs Codex 0.164 and local Ollama.
#[test]
fn a_steer_a_codex_turn_aborted_by_esc_dropped_goes_back_to_sent_and_comes_once_more() {
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
    let agent = AgentId::team(id).unwrap();
    let (_server, host) = start_app(&base, &home, &agent);
    greet(&home);
    let Bridged::Delivered(Some(busy)) = codex::deliver(&home, BUSY_TURN, true) else {
        panic!("the busy turn started with no turn id named");
    };
    until(TURN, "the busy turn to start", || {
        recorded(&home, BUSY_TURN)
    });

    let project = ProjectId::new(PROJECT).unwrap();
    let message = crazyagent::coord::send(
        &db,
        NewMessage {
            project: project.clone(),
            from: Party::Engine,
            to: Party::Agent(id),
            kind: MessageKind::Note,
            question: None,
            work: None,
            reply_to: None,
            body: REQUEUED.to_owned(),
            idempotency_key: Some(format!("p2-live-steer-{id}")),
        },
        now_ms(),
    )
    .unwrap();
    let receipt = || -> (String, Option<String>) {
        db.query_row(
            "SELECT receipt, turn FROM coord_messages WHERE id=?1",
            [message],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap()
    };
    let awake = BTreeSet::new();
    let reach = Reach {
        host: Some(&host as &dyn Host),
        owner: None,
        state: &state,
        asleep: &awake,
        held: &awake,
    };
    let mut deliveries = Deliveries::default();
    deliveries.tick(&db, &project, now_ms(), &reach).unwrap();
    host.input(&agent, ESC).unwrap();
    assert_eq!(
        receipt(),
        ("delivered".to_owned(), Some(busy.clone())),
        "the tick steered the message into the busy turn and kept that turn"
    );
    let mark = format!("[crazyagent msg {message}]");
    until(ABORT, "the owner's Esc to abort the busy turn", || {
        rollout_with(&home, BUSY_TURN)
            .is_some_and(|record| record.contains(r#""type":"turn_aborted""#))
    });
    let named = format!(r#""turn_id":"{busy}""#);
    assert!(
        rollout_with(&home, BUSY_TURN)
            .unwrap()
            .lines()
            .any(|line| { line.contains(r#""type":"turn_aborted""#) && line.contains(&named) }),
        "the rollout's abort names the busy turn the message went into"
    );
    if recorded(&home, &mark) {
        panic!(
            "the steer reached the rollout before the abort, so this run never saw Codex drop it; the test folder is kept at {}",
            root.keep().display()
        );
    }

    crazyagent::coord::relay::codex_receipts(&db, id, &home, now_ms()).unwrap();
    assert_eq!(
        receipt(),
        ("sent".to_owned(), None),
        "the dropped steer waits for the agent's next turn"
    );
    deliveries
        .tick(&db, &project, now_ms() + RETRY_MS, &reach)
        .unwrap();
    let (again, next) = receipt();
    assert_eq!(
        again, "delivered",
        "the next tick gave the agent the message again"
    );
    assert!(
        next.is_some() && next != Some(busy),
        "the message went into a new turn: {next:?}"
    );
    until(TURN, "the message delivered again", || {
        recorded(&home, &mark)
    });
    let record = rollout_with(&home, &mark).unwrap();
    let lines: Vec<&str> = record.lines().collect();
    let held: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, line)| line.contains(r#""type":"response_item""#) && line.contains(&mark))
        .map(|(at, _)| at)
        .collect();
    assert_eq!(held.len(), 1, "Codex holds the message once: {held:?}");
    let abort = lines
        .iter()
        .position(|line| line.contains(r#""type":"turn_aborted""#))
        .unwrap();
    assert!(
        abort < held[0]
            && lines[abort..held[0]]
                .iter()
                .any(|line| line.contains(r#""type":"task_started""#)),
        "the message came in a turn of its own after the abort"
    );
    host.kill(&agent).unwrap();
}
