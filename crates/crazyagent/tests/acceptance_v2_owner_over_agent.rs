#![cfg(unix)]

mod e2e_support;

use crazyagent::config::ModelAccess;
use crazyagent::engine::owner::OwnerResult;
use crazyagent::memory::library::{
    AgentWrites, LibraryAction, MemoryOrigin, MemoryScope, SavedMemory,
};
use crazyagent::project_setup::load_setup;
use crazyagent::protocol::ProjectId;
use crazyagent::team::master::{Answering, serve_calls, set_live};
use crazyagent::team::runtime::agent_home;
use crazyagent::team::tool::{LIVE_SERVER_COMMAND, MAILBOX_FOLDER, REMEMBER_TOOL};
use e2e_support::{ENGINE, Engine, Owner, profile, result, scratch};
use rusqlite::{Connection, params};
use serde_json::{Value, json};
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const TURN_TIME: Duration = Duration::from_secs(60);
const PROJECT: &str = "memory-shop";
const MASTER: &str = "Master";
const OWNER_RULE: &str = "Run the linter and the tests before each commit.";
const AGENT_RULE: &str = "Agent suggestion: run only the linter before each commit.";
const AGENT_STYLE: &str = "Agent suggestion: name test files after the feature.";
const SAVED: &str = "Saved to memory. recall finds it now, and agents started from now on get it.";
const HELD: &str =
    "Saved for the owner's approval. Agents get it once the owner approves it in the Memory tab.";
const DUPLICATE: &str = "Not saved: This memory is already saved.";

fn now_ms() -> i64 {
    i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap()
}

fn database(engine: &Engine) -> Connection {
    let connection = Connection::open(engine.path().join("engine.db")).unwrap();
    connection.busy_timeout(Duration::from_secs(5)).unwrap();
    connection
}

fn add_master(engine: &Engine) -> i64 {
    let connection = database(engine);
    crazyagent::team::initialize(&connection).unwrap();
    let now = now_ms();
    connection
        .execute(
            "INSERT INTO team_agents(project, role, harness, tools, status, created_ms, heartbeat_ms, stopped_ms)
             VALUES (?1, 'master', 'claude-code', 1, 'stopped', ?2, ?2, ?2)",
            params![PROJECT, now],
        )
        .unwrap();
    connection.last_insert_rowid()
}

fn private_folder(path: &Path) {
    std::fs::create_dir_all(path).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
}

fn remember(id: u64, text: &str) -> String {
    let call = json!({"jsonrpc": "2.0", "id": id, "method": "tools/call",
        "params": {"name": REMEMBER_TOOL, "arguments": {"text": text, "scope": "project"}}});
    format!("{call}\n")
}

/// Runs one turn of the project's live master while the engine process is stopped: the real
/// `team-mcp-live` server takes the master's calls and the engine's own `serve_calls`
/// answers them on the engine database, the live switch on for that turn only.
/// Inputs: the stopped engine, the master agent and the calls numbered from 2.
/// Output: each answer's text and error flag, in call order.
fn master_turn(engine: &Engine, master: i64, calls: &[String]) -> Vec<(String, bool)> {
    let root = engine.path();
    let project = ProjectId::new(PROJECT).unwrap();
    let mut connection = database(engine);
    crazyagent::team::master::initialize(&connection).unwrap();
    set_live(&connection, &project, true).unwrap();
    assert_eq!(
        connection
            .execute(
                "UPDATE team_live_masters SET agent=?1 WHERE project=?2",
                params![master, PROJECT],
            )
            .unwrap(),
        1
    );
    let stored = {
        let tx = connection.unchecked_transaction().unwrap();
        load_setup(&tx, &project)
            .unwrap()
            .expect("the owner created the project")
    };
    let state = root.join("team");
    let folder = agent_home(&state, master).parent().unwrap().to_path_buf();
    let mailbox = folder.join("tmp").join(MAILBOX_FOLDER);
    for path in [folder.clone(), folder.join("tmp"), mailbox.clone()] {
        private_folder(&path);
    }

    let mut server = crazyagent::spawn_gate::spawn(
        Command::new(ENGINE)
            .arg(LIVE_SERVER_COMMAND)
            .arg(&mailbox)
            .env_clear()
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null()),
    )
    .unwrap();
    let mut requests = vec![
        format!(
            "{}\n",
            json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
                "protocolVersion": "2025-06-18", "capabilities": {},
                "clientInfo": {"name": "acceptance", "version": "1"}}})
        ),
        "{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n".to_owned(),
    ];
    requests.extend_from_slice(calls);
    let mut input = server.stdin.take().unwrap();
    input.write_all(requests.concat().as_bytes()).unwrap();
    drop(input);
    let mut stdout = server.stdout.take().unwrap();
    let reader = std::thread::spawn(move || {
        let mut output = String::new();
        stdout.read_to_string(&mut output).unwrap();
        output
    });

    let access = ModelAccess::default();
    let agents = state.join("agents");
    let answering = Answering {
        access: &access,
        stored: &stored,
        landing: root,
        agents: &agents,
        events: None,
        host: None,
    };
    let deadline = Instant::now() + TURN_TIME;
    while server.try_wait().unwrap().is_none() {
        assert!(Instant::now() < deadline, "the master's turn never ended");
        serve_calls(&mut connection, &answering, master, &mailbox).unwrap();
        std::thread::sleep(Duration::from_millis(20));
    }
    set_live(&connection, &project, false).unwrap();
    connection
        .execute(
            "UPDATE team_live_masters SET agent=NULL WHERE project=?1",
            [PROJECT],
        )
        .unwrap();

    let output = reader.join().unwrap();
    let answers: Vec<Value> = output
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(answers.len(), calls.len() + 1, "{output}");
    (2..2 + calls.len() as u64)
        .map(|id| {
            let found = answers
                .iter()
                .find(|answer| answer["id"] == id)
                .unwrap_or_else(|| panic!("no answer {id}:\n{output}"));
            (
                found["result"]["content"][0]["text"]
                    .as_str()
                    .unwrap()
                    .to_owned(),
                found["result"]["isError"].as_bool().unwrap(),
            )
        })
        .collect()
}

fn answer(text: &str, error: bool) -> (String, bool) {
    (text.to_owned(), error)
}

async fn listed(tui: &mut Owner, scope: &MemoryScope) -> Vec<SavedMemory> {
    let page = tui
        .controller
        .memories(scope.clone(), None, None)
        .await
        .unwrap();
    assert_eq!(
        page.next, None,
        "one page holds every memory of the project"
    );
    page.items
}

fn origin(memories: &[SavedMemory], text: &str) -> MemoryOrigin {
    let found: Vec<&SavedMemory> = memories
        .iter()
        .filter(|memory| memory.text == text)
        .collect();
    assert_eq!(found.len(), 1, "{text} is saved once: {memories:#?}");
    found[0].origin.clone()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_owner_s_memory_stays_as_the_owner_saved_it_whatever_the_live_master_remembers() {
    let mut engine = Engine::start("ca-av2-memory-");
    let mut tui = Owner::attach(&engine).await;
    assert_eq!(tui.save_profile(profile()).await, OwnerResult::Applied);
    let disk = scratch("ca-av2-memory-home-");
    let home = disk.path().join("Projects");
    std::fs::create_dir(&home).unwrap();
    assert_eq!(tui.set_home(&home).await, OwnerResult::Applied);
    assert_eq!(
        tui.create_project(PROJECT, "Memory shop").await,
        OwnerResult::Applied
    );
    let scope = MemoryScope::Project {
        project_id: ProjectId::new(PROJECT).unwrap(),
    };
    let generation = tui.next();
    assert_eq!(
        result(
            tui.controller
                .add_memory(scope.clone(), OWNER_RULE.into(), generation)
                .await
                .unwrap()
        ),
        OwnerResult::Applied,
        "the owner saves the memory from the TUI"
    );
    let first = listed(&mut tui, &scope).await;
    assert_eq!(first.len(), 1, "{first:#?}");
    let owners = first[0].clone();
    assert_eq!(
        (owners.text.as_str(), &owners.origin),
        (OWNER_RULE, &MemoryOrigin::Written)
    );
    drop(tui);
    engine.stop();

    let master = add_master(&engine);
    let saved = master_turn(
        &engine,
        master,
        &[remember(2, OWNER_RULE), remember(3, AGENT_RULE)],
    );
    assert_eq!(
        saved,
        [answer(DUPLICATE, true), answer(SAVED, false)],
        "the master cannot save the owner's memory again as its own, and its other text is a new memory"
    );

    engine.launch();
    let mut tui = Owner::attach(&engine).await;
    let generation = tui.next();
    assert_eq!(
        result(
            tui.controller
                .change_memory(
                    LibraryAction::AgentWrites {
                        writes: AgentWrites::Ask
                    },
                    generation,
                )
                .await
                .unwrap()
        ),
        OwnerResult::Applied,
        "the owner turns on Ask me first for agent memory writes"
    );
    drop(tui);
    engine.stop();

    let held = master_turn(
        &engine,
        master,
        &[
            remember(2, OWNER_RULE),
            remember(3, AGENT_RULE),
            remember(4, AGENT_STYLE),
        ],
    );
    assert_eq!(
        held,
        [
            answer(DUPLICATE, true),
            answer(DUPLICATE, true),
            answer(HELD, false)
        ],
        "under Ask me first the owner's memory is still refused and the master's new text waits for approval"
    );

    engine.launch();
    let mut tui = Owner::attach(&engine).await;
    let last = listed(&mut tui, &scope).await;
    assert_eq!(last.len(), 3, "{last:#?}");
    let kept = last
        .iter()
        .find(|memory| memory.id == owners.id)
        .expect("the owner's memory is still listed");
    assert_eq!(
        kept, &owners,
        "the owner's memory keeps its id, place, order, origin and text after the master's turns and the restarts"
    );
    assert_eq!(origin(&last, OWNER_RULE), MemoryOrigin::Written);
    assert_eq!(
        origin(&last, AGENT_RULE),
        MemoryOrigin::Agent {
            agent: MASTER.into()
        }
    );
    assert_eq!(
        origin(&last, AGENT_STYLE),
        MemoryOrigin::Requested {
            agent: MASTER.into()
        }
    );
    drop(tui);
    engine.stop();
}
