#![cfg(unix)]

mod e2e_support;

use crazyagent::client_controller::OwnerController;
use crazyagent::config::{ModelAccess, resolve_config};
use crazyagent::engine::owner::{OwnerClientKind, OwnerResult};
use crazyagent::memory::{KnowledgeEntry, ReviewDecision};
use crazyagent::project_setup::load_setup;
use crazyagent::protocol::ProjectId;
use crazyagent::team::master::{Answering, serve_calls, set_live};
use crazyagent::team::runtime::{ConfigSource, agent_home};
use crazyagent::team::tool::{LIVE_SERVER_COMMAND, MAILBOX_FOLDER, MEMORIES_TOOL, REMEMBER_TOOL};
use e2e_support::{ENGINE, Engine, Owner, profile, rejected, result, scratch};
use rusqlite::{Connection, params};
use serde_json::{Value, json};
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const ATTACH_TIME: Duration = Duration::from_secs(10);
const TURN_TIME: Duration = Duration::from_secs(60);
const PROJECT: &str = "memory-shop";
const AGENT_RULE: &str = "Agent suggestion: run the linter before each commit.";
const AGENT_STYLE: &str = "Agent suggestion: name test files after the module.";
const OWNER_RULE: &str = "Run the linter and the tests before each commit.";
const AGENT_REDO: &str = "Agent change: run only the linter before each commit.";
const AGENT_STYLE_CHANGE: &str = "Agent change: name test files after the feature.";
const OWNER_STYLE: &str = "Name test files after the module they test.";
const AGENT_LATE: &str = "Agent change after the owner's edit: name test files freely.";

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

fn tool_call(id: u64, name: &str, arguments: Value) -> String {
    let call = json!({"jsonrpc": "2.0", "id": id, "method": "tools/call",
        "params": {"name": name, "arguments": arguments}});
    format!("{call}\n")
}

fn remember(id: u64, text: &str, read: Option<(&str, u64)>) -> String {
    let arguments = match read {
        None => json!({"text": text}),
        Some((memory, version)) => json!({"text": text, "memory": memory, "version": version}),
    };
    tool_call(id, REMEMBER_TOOL, arguments)
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
    let folder = agent_home(&root.join("team"), master)
        .parent()
        .unwrap()
        .to_path_buf();
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

    let (global, settings) = (root.join("global.toml"), root.join("project.toml"));
    let config: ConfigSource = Box::new(move || resolve_config(&global, &settings).ok());
    assert!(config().is_some(), "the engine's settings resolve");
    let access = ModelAccess::default();
    let answering = Answering {
        access: &access,
        stored: &stored,
        landing: root,
        events: None,
        host: None,
        config: Some(&config),
    };
    let deadline = Instant::now() + TURN_TIME;
    while server.try_wait().unwrap().is_none() {
        assert!(Instant::now() < deadline, "the master's turn never ended");
        serve_calls(&mut connection, &answering, &mailbox).unwrap();
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

fn saved(answer: &(String, bool), version: u64) -> String {
    let (text, error) = answer;
    assert!(!error, "{text}");
    let id = text
        .split_whitespace()
        .nth(2)
        .unwrap_or_default()
        .to_owned();
    assert_eq!(
        text,
        &format!(
            "Saved memory {id} at version {version} as a suggestion the owner keeps or rejects. Name version {version} to change it."
        )
    );
    id
}

fn stale(id: &str, read: u64, current: u64) -> (String, bool) {
    (
        format!(
            "Stale: re-read and redo. Memory {id} is at version {current} now, changed by the owner since version {read} you read. Call memories(), then redo your change on the current text naming version {current}."
        ),
        true,
    )
}

fn held(id: &str, version: u64) -> (String, bool) {
    (
        format!(
            "Not saved: the owner reviewed memory {id} at version {version}, and agents do not change a memory the owner reviewed. Suggest a new memory with remember() if it still matters."
        ),
        true,
    )
}

fn entry<'a>(entries: &'a [KnowledgeEntry], id: &str) -> &'a KnowledgeEntry {
    entries
        .iter()
        .find(|entry| entry.id.as_str() == id)
        .unwrap_or_else(|| {
            let listed: Vec<&str> = entries.iter().map(|entry| entry.id.as_str()).collect();
            panic!("memory {id} is not listed: {listed:?}")
        })
}

fn shown(entry: &KnowledgeEntry) -> (u64, &str, &str, bool) {
    (
        entry.version,
        entry.text.as_str(),
        entry.curator_session.as_str(),
        entry.human_reviewed,
    )
}

async fn desktop(engine: &Engine) -> OwnerController {
    OwnerController::attach(engine.path(), OwnerClientKind::Desktop, ATTACH_TIME)
        .await
        .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_owner_s_memory_edit_wins_over_the_live_master_s_and_the_master_cannot_undo_it() {
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
    let project = ProjectId::new(PROJECT).unwrap();
    drop(tui);
    engine.stop();

    let master = add_master(&engine);
    let curator = format!("team-{master}");
    let suggested = master_turn(
        &engine,
        master,
        &[
            remember(2, AGENT_RULE, None),
            remember(3, AGENT_STYLE, None),
        ],
    );
    let rule = saved(&suggested[0], 1);
    let style = saved(&suggested[1], 1);
    assert_ne!(rule, style);

    engine.launch();
    let mut tui = Owner::attach(&engine).await;
    let mut desk = desktop(&engine).await;
    let opened = desk.project_memories(project.clone(), None).await.unwrap();
    let style_opened = entry(&opened, &style).version;
    assert_eq!(
        shown(entry(&opened, &style)),
        (1, AGENT_STYLE, curator.as_str(), false),
        "the desktop shows the master's suggestion as the master's"
    );
    let listed = tui
        .controller
        .project_memories(project.clone(), None)
        .await
        .unwrap();
    assert_eq!(
        shown(entry(&listed, &rule)),
        (1, AGENT_RULE, curator.as_str(), false)
    );
    let generation = tui.next();
    assert_eq!(
        result(
            tui.controller
                .review_memory(
                    project.clone(),
                    entry(&listed, &rule).id.clone(),
                    1,
                    ReviewDecision::Edit {
                        text: OWNER_RULE.into()
                    },
                    generation,
                )
                .await
                .unwrap()
        ),
        OwnerResult::Applied,
        "the owner edits the master's suggestion from the TUI"
    );
    drop(desk);
    drop(tui);
    engine.stop();

    let refused = master_turn(
        &engine,
        master,
        &[
            remember(2, AGENT_REDO, Some((&rule, 1))),
            remember(3, AGENT_REDO, Some((&rule, 2))),
            remember(4, AGENT_STYLE_CHANGE, Some((&style, 1))),
        ],
    );
    assert_eq!(
        refused[0],
        stale(&rule, 1, 2),
        "the master's change from before the owner's edit is refused as stale, naming the owner"
    );
    assert_eq!(
        refused[1],
        held(&rule, 2),
        "after re-reading, the master still cannot change what the owner reviewed"
    );
    assert_eq!(saved(&refused[2], 2), style);

    engine.launch();
    let mut desk = desktop(&engine).await;
    assert_eq!(
        result(
            desk.review_memory(
                project.clone(),
                entry(&opened, &style).id.clone(),
                style_opened,
                ReviewDecision::Edit {
                    text: OWNER_STYLE.into()
                },
                1,
            )
            .await
            .unwrap()
        ),
        rejected("knowledge_version_conflict"),
        "the desktop screen opened before the master's change is refused, not merged"
    );
    let reread = desk.project_memories(project.clone(), None).await.unwrap();
    assert_eq!(
        shown(entry(&reread, &style)),
        (2, AGENT_STYLE_CHANGE, curator.as_str(), false),
        "the refused edit changed nothing; the owner reads the master's version 2"
    );
    assert_eq!(
        (
            entry(&reread, &rule).version,
            entry(&reread, &rule).text.as_str(),
            entry(&reread, &rule).human_reviewed
        ),
        (2, OWNER_RULE, true),
        "the master's refused calls left the owner's edit in place"
    );
    assert_eq!(
        result(
            desk.review_memory(
                project.clone(),
                entry(&reread, &style).id.clone(),
                2,
                ReviewDecision::Edit {
                    text: OWNER_STYLE.into()
                },
                2,
            )
            .await
            .unwrap()
        ),
        OwnerResult::Applied,
        "the owner's edit on the re-read version replaces the master's"
    );
    drop(desk);
    engine.stop();

    let late = master_turn(
        &engine,
        master,
        &[
            remember(2, AGENT_LATE, Some((&style, 2))),
            remember(3, AGENT_LATE, Some((&style, 3))),
            tool_call(4, MEMORIES_TOOL, json!({})),
        ],
    );
    assert_eq!(late[0], stale(&style, 2, 3));
    assert_eq!(late[1], held(&style, 3));
    let (memories, error) = &late[2];
    assert!(!error, "{memories}");
    for line in [
        format!("- {rule} version 2 (reviewed by the owner): {OWNER_RULE}"),
        format!("- {style} version 3 (reviewed by the owner): {OWNER_STYLE}"),
    ] {
        assert!(
            memories.lines().any(|listed| listed == line),
            "the master reads the owner's text: {memories}"
        );
    }

    engine.launch();
    let mut tui = Owner::attach(&engine).await;
    let last = tui
        .controller
        .project_memories(project.clone(), None)
        .await
        .unwrap();
    assert_eq!(last.len(), 2, "the master's refused calls added no memory");
    for (id, version, text) in [(&rule, 2, OWNER_RULE), (&style, 3, OWNER_STYLE)] {
        let kept = entry(&last, id);
        assert_eq!(
            (kept.version, kept.text.as_str(), kept.human_reviewed),
            (version, text, true),
            "the owner's edit of {id} survives the master's later turns and the restarts"
        );
    }
    drop(tui);
    engine.stop();
}
