mod routes;

use crazyagent::agent_git::Layout;
use crazyagent::coord::settings::SkillRole;
use crazyagent::coord::{MessageKind, NewMessage, Party, QuestionFor, Severity};
use crazyagent::landing::{AgentKind, ChangeAgent, TurnEnd, TurnOutcome, Workspace};
use crazyagent::protocol::ProjectId;
use crazyagent_desktop::bridge::{BridgeError, BridgeQueue, NativeBridge};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use std::ffi::OsString;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::{RwLockReadGuard, RwLockWriteGuard};

const HEAD_BYTES_MAX: usize = 16 * 1024;
const BODY_BYTES_MAX: usize = 4 * 1024 * 1024;
const TOKEN_BYTES: usize = 32;
const RECORD_MODE: &str = "record-turn-end";
const ASK_MODE: &str = "ask-owner";
const SKILLS_MODE: &str = "role-skills";
const RECORD_ARGS_MAX: usize = 10;
const IDE_OPEN: &str = "e2e_ide_open";

/// The one bridge every route shares, as the app's `NativeState` does.
type Shared = Arc<BridgeQueue>;

/// What a routed command answers the page's invoke with: its value, its own failure,
/// Tauri's refusal of its arguments, or no such command.
enum Routed {
    Value(Value),
    Failure(Value),
    Invalid(String),
    Unknown,
}

/// Takes the shared bridge alone through the same bounded queue the app's commands use.
/// Input: the shared bridge. Output: its guard, or `busy` past the queue's bounds.
async fn lock(state: &Shared) -> Result<RwLockWriteGuard<'_, NativeBridge>, BridgeError> {
    state.enter().await
}

/// Shares the bridge with other plain reads through the same bounded queue the app's
/// reads use. Input: the shared bridge. Output: its guard, or `busy` past the queue's bounds.
async fn read(state: &Shared) -> Result<RwLockReadGuard<'_, NativeBridge>, BridgeError> {
    state.read().await
}

/// Reads a command's camelCase arguments the way Tauri does, refusing ones that do not fit.
/// Input: the invoke arguments. Output: the typed arguments or the refusal to send back.
fn parse<T: DeserializeOwned>(args: Value) -> Result<T, Routed> {
    serde_json::from_value(args).map_err(|error| Routed::Invalid(format!("invalid args: {error}")))
}

/// Turns a command result into its answer. Input: the result. Output: the value or the
/// serialized failure, as Tauri resolves or rejects the invoke.
fn routed<T: Serialize>(result: Result<T, BridgeError>) -> Routed {
    let encoded = match result {
        Ok(value) => serde_json::to_value(value).map(Routed::Value),
        Err(failure) => serde_json::to_value(failure).map(Routed::Failure),
    };
    encoded.unwrap_or_else(|error| Routed::Invalid(format!("unencodable reply: {error}")))
}

/// Reads one HTTP request from the test's file server: `POST /invoke` with the run's
/// bearer token and a JSON body of at most 4 MiB. Input: the stream and the token.
/// Output: the body, or the refusal reason.
async fn request(stream: &mut TcpStream, token: &str) -> Result<Vec<u8>, &'static str> {
    let mut buffer = Vec::new();
    let end = loop {
        if let Some(end) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
            break end;
        }
        if buffer.len() > HEAD_BYTES_MAX {
            return Err("head too large");
        }
        let mut chunk = [0u8; 4096];
        let read = stream.read(&mut chunk).await.map_err(|_| "read failed")?;
        if read == 0 {
            return Err("closed early");
        }
        buffer.extend_from_slice(&chunk[..read]);
    };
    let head = std::str::from_utf8(&buffer[..end]).map_err(|_| "head not text")?;
    let mut lines = head.split("\r\n");
    if lines.next() != Some("POST /invoke HTTP/1.1") {
        return Err("not an invoke");
    }
    let mut length = None;
    let mut authorized = false;
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            return Err("bad header");
        };
        match name.trim().to_ascii_lowercase().as_str() {
            "content-length" => length = value.trim().parse::<usize>().ok(),
            "authorization" => authorized = value.trim() == format!("Bearer {token}"),
            _ => {}
        }
    }
    if !authorized {
        return Err("unauthorized");
    }
    let length = length.ok_or("no length")?;
    if length > BODY_BYTES_MAX {
        return Err("body too large");
    }
    let mut body = buffer[end + 4..].to_vec();
    if body.len() > length {
        return Err("body longer than its length");
    }
    let start = body.len();
    body.resize(length, 0);
    stream
        .read_exact(&mut body[start..])
        .await
        .map_err(|_| "body cut short")?;
    assert_eq!(body.len(), length);
    Ok(body)
}

/// Opens a project's IDE server through the bridge as the app's `desktop_ide_open` does,
/// without the native webview it would show it in, and answers where the IDE is reached,
/// so the end-to-end test loads the same workbench in a browser. Inputs: the shared bridge
/// and `{ projectId }`. Output: the opened server, or its refusal.
async fn ide_open(state: &Shared, args: Value) -> Routed {
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    struct Args {
        project_id: String,
    }
    let Args { project_id } = match parse(args) {
        Ok(args) => args,
        Err(invalid) => return invalid,
    };
    let result: Result<_, BridgeError> = async {
        let project = crazyagent_desktop::bridge::ide::ide_project(&project_id)?;
        let call = read(state).await?.ide_open(project)?;
        call.await
    }
    .await;
    routed(result)
}

/// Serves one connection: routes its invoke on the shared bridge and writes the answer.
/// Input: the stream, the shared bridge and the token. Output: nothing; failures close it.
async fn serve(mut stream: TcpStream, state: Shared, token: Arc<String>) {
    let (status, answer) = match request(&mut stream, &token).await {
        Err(reason) => ("400 Bad Request", json!({ "refused": reason })),
        Ok(body) => match serde_json::from_slice::<Value>(&body) {
            Ok(Value::Object(mut call)) => match call.remove("cmd") {
                Some(Value::String(command)) => {
                    let args = call.remove("args").unwrap_or_else(|| json!({}));
                    let routed = if command == IDE_OPEN {
                        ide_open(&state, args).await
                    } else {
                        routes::route(&state, &command, args).await
                    };
                    let answer = match routed {
                        Routed::Value(value) => json!({ "value": value }),
                        Routed::Failure(failure) => json!({ "failure": failure }),
                        Routed::Invalid(message) => json!({ "invalid": message }),
                        Routed::Unknown => json!({ "unknown": command }),
                    };
                    ("200 OK", answer)
                }
                _ => ("400 Bad Request", json!({ "refused": "no command" })),
            },
            _ => (
                "400 Bad Request",
                json!({ "refused": "body not an object" }),
            ),
        },
    };
    let body = answer.to_string();
    let reply = format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(reply.as_bytes()).await;
    let _ = stream.shutdown().await;
}

/// Ends one sub-agent turn in a worktree the way the team runtime does, through the
/// engine's own `landing_turns::record` on its database, so the end-to-end test reviews a
/// real worktree edit and a set that conflicts with main goes back to its agent with its
/// resolve task (OS-8). Inputs: engine root, project, agent id and label, owner checkout,
/// agent worktree, its git folder and the prompt line. Output: the change set id or `none`.
fn record_turn_end(args: &[OsString]) -> Result<String, String> {
    let [
        root,
        project,
        agent,
        label,
        repository,
        worktree,
        git_dir,
        prompt,
    ] = args
    else {
        return Err(format!(
            "usage: desktop-e2e-bridge {RECORD_MODE} <engine root> <project> <agent> <label> <repository> <worktree> <git dir> <prompt>"
        ));
    };
    let text = |value: &OsString| {
        value
            .to_str()
            .map(str::to_owned)
            .ok_or_else(|| "arguments must be UTF-8".to_owned())
    };
    let agent = text(agent)?;
    let workspace = Workspace {
        project: ProjectId::new(text(project)?).map_err(|_| "invalid project id".to_owned())?,
        agent: ChangeAgent {
            kind: AgentKind::Sub,
            id: agent.clone(),
            label: text(label)?,
        },
        repository: PathBuf::from(repository),
        worktree: PathBuf::from(worktree),
        git_dir: PathBuf::from(git_dir),
    };
    let turn = TurnEnd {
        outcome: TurnOutcome::Succeeded,
        turn: format!("turn-{agent}"),
        prompt_line: text(prompt)?,
    };
    let db = crazyagent::landing::open(Path::new(root)).map_err(|error| error.0)?;
    let recorded =
        crazyagent::landing_turns::record(&db, &workspace, &turn).map_err(|error| error.0)?;
    Ok(match recorded {
        crazyagent::landing_turns::Landed::Nothing => "none".to_owned(),
        crazyagent::landing_turns::Landed::Added(id)
        | crazyagent::landing_turns::Landed::Waiting(id) => String::from(id),
    })
}

/// Puts one question from a team agent to the owner the way the agent's `update_work` call
/// does, through the engine's own `coord::send` on its database, so the end-to-end test
/// answers a question the engine really holds. Inputs: engine root, project, asking agent
/// id, severity (high, normal or low) and the question of at most `coord::BODY_BYTES_MAX`
/// bytes. Output: the question's message id.
fn ask_owner(args: &[OsString]) -> Result<String, String> {
    let [root, project, agent, severity, question] = args else {
        return Err(format!(
            "usage: desktop-e2e-bridge {ASK_MODE} <engine root> <project> <agent> <severity> <question>"
        ));
    };
    let text = |value: &OsString| {
        value
            .to_str()
            .map(str::to_owned)
            .ok_or_else(|| "arguments must be UTF-8".to_owned())
    };
    let agent: i64 = text(agent)?
        .parse()
        .ok()
        .filter(|id| *id > 0)
        .ok_or_else(|| "invalid agent id".to_owned())?;
    let severity = Severity::parse(&text(severity)?)
        .ok_or_else(|| "severity is high, normal or low".to_owned())?;
    let body = text(question)?;
    if body.trim().is_empty() || body.len() > crazyagent::coord::BODY_BYTES_MAX {
        return Err(format!(
            "the question is 1 to {} bytes",
            crazyagent::coord::BODY_BYTES_MAX
        ));
    }
    let message = NewMessage {
        project: ProjectId::new(text(project)?).map_err(|_| "invalid project id".to_owned())?,
        from: Party::Agent(agent),
        to: Party::Owner,
        kind: MessageKind::Question,
        question: Some((QuestionFor::Me, severity)),
        work: None,
        reply_to: None,
        body,
        idempotency_key: None,
    };
    let since = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| "the clock is before 1970".to_owned())?;
    let now =
        i64::try_from(since.as_millis()).map_err(|_| "the clock is out of range".to_owned())?;
    let db = crazyagent::landing::open(Path::new(root)).map_err(|error| error.0)?;
    let id = crazyagent::coord::send(&db, message, now).map_err(|error| error.0)?;
    assert!(id > 0);
    Ok(id.to_string())
}

/// Installs what one team role's agent is given for a project the way a team launch does,
/// through the engine's own `launch_context::for_team_role` on its database and a Claude Code
/// install with tools on, so the end-to-end test reads the skills the owner's choice gives
/// that role (DEC-111 R3). Inputs: engine root, project, role (master, lead or worker) and an
/// empty private home folder. Output: the installed skill names in order, comma-separated,
/// or `none`.
fn role_skills(args: &[OsString]) -> Result<String, String> {
    let [root, project, role, home] = args else {
        return Err(format!(
            "usage: desktop-e2e-bridge {SKILLS_MODE} <engine root> <project> <master|lead|worker> <home>"
        ));
    };
    let text = |value: &OsString| {
        value
            .to_str()
            .map(str::to_owned)
            .ok_or_else(|| "arguments must be UTF-8".to_owned())
    };
    let role = match text(role)?.as_str() {
        "master" => SkillRole::Master,
        "lead" => SkillRole::Lead,
        "worker" => SkillRole::Worker,
        _ => return Err("the role is master, lead or worker".to_owned()),
    };
    let project = ProjectId::new(text(project)?).map_err(|_| "invalid project id".to_owned())?;
    let db = crazyagent::landing::open(Path::new(root)).map_err(|error| error.0)?;
    let context = crazyagent::launch_context::for_team_role(&db, &project, Layout::Linked, role)
        .map_err(|error| error.0)?;
    let home = Path::new(home);
    let installed = context
        .install("claude-code", home, true)
        .map_err(|error| error.0)?;
    let folder = home.join("crazyagent/plugin/skills");
    let mut names = Vec::new();
    if folder.is_dir() {
        for entry in std::fs::read_dir(&folder).map_err(|error| error.to_string())? {
            let name = entry.map_err(|error| error.to_string())?.file_name();
            names.push(
                name.into_string()
                    .map_err(|_| "a skill folder name is not UTF-8".to_owned())?,
            );
        }
    }
    drop(installed);
    assert!(names.len() <= context.plugins().skills.len());
    names.sort();
    Ok(match names.is_empty() {
        true => "none".to_owned(),
        false => names.join(","),
    })
}

/// Serves the app's `desktop_*` commands to the end-to-end test on 127.0.0.1 for as long as
/// stdin stays open; with `record-turn-end` first, records one ended agent turn, with
/// `ask-owner` first, puts one agent's question to the owner, and with `role-skills` first,
/// installs one team role's skills. Input: the engine socket path. Output: prints
/// `ready <port> <token>` once listening, or the recorded change set, the question's id or
/// the installed skills; exits non-zero when it cannot.
#[tokio::main]
async fn main() -> std::process::ExitCode {
    let all: Vec<OsString> = std::env::args_os().skip(1).take(RECORD_ARGS_MAX).collect();
    if all.first().is_some_and(|mode| mode == ASK_MODE) {
        return match ask_owner(&all[1..]) {
            Ok(id) => {
                println!("{id}");
                std::process::ExitCode::SUCCESS
            }
            Err(error) => {
                eprintln!("{ASK_MODE}: {error}");
                std::process::ExitCode::FAILURE
            }
        };
    }
    if all.first().is_some_and(|mode| mode == SKILLS_MODE) {
        return match role_skills(&all[1..]) {
            Ok(names) => {
                println!("{names}");
                std::process::ExitCode::SUCCESS
            }
            Err(error) => {
                eprintln!("{SKILLS_MODE}: {error}");
                std::process::ExitCode::FAILURE
            }
        };
    }
    if all.first().is_some_and(|mode| mode == RECORD_MODE) {
        return match record_turn_end(&all[1..]) {
            Ok(recorded) => {
                println!("{recorded}");
                std::process::ExitCode::SUCCESS
            }
            Err(error) => {
                eprintln!("record-turn-end: {error}");
                std::process::ExitCode::FAILURE
            }
        };
    }
    let mut args = all.into_iter();
    let (Some(socket), None) = (args.next(), args.next()) else {
        eprintln!("usage: desktop-e2e-bridge <engine socket>");
        return std::process::ExitCode::FAILURE;
    };
    let mut random = [0u8; TOKEN_BYTES];
    if getrandom::fill(&mut random).is_err() {
        eprintln!("no random source");
        return std::process::ExitCode::FAILURE;
    }
    let token: String = random.iter().map(|byte| format!("{byte:02x}")).collect();
    assert_eq!(token.len(), TOKEN_BYTES * 2);
    let listener = match crazyagent::spawn_gate::bind_tcp("127.0.0.1:0").await {
        Ok(listener) => listener,
        Err(_) => {
            eprintln!("cannot listen on 127.0.0.1");
            return std::process::ExitCode::FAILURE;
        }
    };
    let Ok(address) = listener.local_addr() else {
        eprintln!("no listening address");
        return std::process::ExitCode::FAILURE;
    };
    assert!(address.ip().is_loopback());
    let state: Shared = Arc::new(BridgeQueue::new(NativeBridge::new(Some(PathBuf::from(
        socket,
    )))));
    let token = Arc::new(token);
    std::thread::spawn(|| {
        let _ = std::io::copy(&mut std::io::stdin(), &mut std::io::sink());
        std::process::exit(0);
    });
    println!("ready {} {token}", address.port());
    if std::io::stdout().flush().is_err() {
        return std::process::ExitCode::FAILURE;
    }
    loop {
        let Ok((stream, peer)) = crazyagent::spawn_gate::accept_tcp(&listener).await else {
            continue;
        };
        if !peer.ip().is_loopback() {
            continue;
        }
        tokio::spawn(serve(stream, Arc::clone(&state), Arc::clone(&token)));
    }
}
