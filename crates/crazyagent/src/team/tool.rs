use super::{DELEGATIONS_PER_REPLY_MAX, Delegation, TASK_PROMPT_BYTES_MAX, delegation, names};
use crate::config::EffectiveConfig;
use crate::engine::EngineError;
use crate::memory::library::{
    AGENT_SUGGESTIONS_MAX, AGENT_VERSIONS_MAX, AgentMemory, AgentWrite, NOTE_BYTES_MAX,
    agent_memories, agent_remember,
};
use crate::protocol::{ProjectId, RequestId};
use crate::storage::versioned::LastWriter;
use crate::terminal::AgentId;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior};
#[cfg(unix)]
use rustix::fs::{AtFlags, FileType, Mode, OFlags};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::fs::File;
use std::io::{BufRead, Read, Write};
use std::path::Path;

pub const SERVER_NAME: &str = "crazyagent-team";
pub const SERVER_COMMAND: &str = "team-mcp";
pub const TOOL_NAME: &str = "delegate";
pub const DROP_FILE: &str = "crazyagent-delegations.jsonl";
pub const DROP_BYTES_MAX: u64 =
    (DELEGATIONS_PER_REPLY_MAX * (TASK_PROMPT_BYTES_MAX * 2 + 256)) as u64;
const REQUEST_BYTES_MAX: usize = 1024 * 1024;
const PROTOCOL_VERSIONS: [&str; 3] = ["2024-11-05", "2025-03-26", "2025-06-18"];
const RECORDED: &str = "Recorded. When your reply ends, the engine checks this task against the team plan and hands it to a sub-agent.";
const INVALID: &str = "Not recorded: give a task of plain text up to 4000 bytes and, if you choose them, a harness name from the team plan and a short role such as onboarding-fix.";
const ROLE_BYTES_MAX: usize = 64;
const FULL: &str = "Not recorded: the engine takes at most 8 tasks from one reply.";
const UNWRITABLE: &str = "Not recorded: the engine's task record could not be written.";

const _: () = assert!(DROP_BYTES_MAX as usize >= DELEGATIONS_PER_REPLY_MAX * TASK_PROMPT_BYTES_MAX);
const _: () = assert!(TASK_PROMPT_BYTES_MAX == 4000 && DELEGATIONS_PER_REPLY_MAX == 8);

pub const LIVE_SERVER_COMMAND: &str = "team-mcp-live";
pub const MESSAGE_TOOL: &str = "message";
pub const ANSWER_TOOL: &str = "answer";
pub const STATUS_TOOL: &str = "status";
pub const CHANGES_TOOL: &str = "changes";
pub const ESCALATE_TOOL: &str = "escalate";
pub const MEMORIES_TOOL: &str = "memories";
pub const REMEMBER_TOOL: &str = "remember";
pub const MAILBOX_FOLDER: &str = "crazyagent-team-calls";
pub const CALLS_PER_TICK_MAX: usize = 8;
pub const CALLS_PENDING_MAX: usize = 16;
pub const REPLY_TEXT_MAX: usize = 32 * 1024;
const CALL_BYTES_MAX: u64 = 16 * 1024;
const REPLY_BYTES_MAX: u64 = 2 * REPLY_TEXT_MAX as u64 + 64;
const MAILBOX_SCAN_MAX: usize = 256;
const AGENT_NAME_BYTES_MAX: usize = 64;
const CALL_SUFFIX: &str = ".call";
const TAKEN_SUFFIX: &str = ".taken";
const REPLY_SUFFIX: &str = ".reply";
const PART_SUFFIX: &str = ".part";
const REPLY_WAIT: std::time::Duration = std::time::Duration::from_secs(60);
const REPLY_POLL: std::time::Duration = std::time::Duration::from_millis(50);
const REPLY_STALE: std::time::Duration = std::time::Duration::from_secs(10 * 60);
const LIVE_INVALID: &str = "Not handed off: give a task of plain text up to 4000 bytes and, if you choose one, a harness name from the team plan.";
const ROLE_INVALID: &str = "Not handed off: give a short role of letters and digits up to 64 bytes, such as onboarding-fix.";
const ESCALATE_INVALID: &str = "Not sent: give the owner one plain question of up to 2000 bytes.";
const TYPED_INVALID: &str = "Not sent: name the sub-agent and give plain text up to 4000 bytes.";
const BUSY: &str = "Not sent: 16 team calls already wait for the engine. Try again in a moment.";
const UNSENT: &str = "Not sent: the call could not be written for the engine.";
const NOT_TAKEN: &str =
    "Not sent: the engine did not take the call within a minute, so nothing was done.";
const NOT_ANSWERED: &str = "The engine took the call but did not answer within a minute. Check status() before trying again.";
const MEMORY_INVALID: &str = "Not saved: give one plain memory of up to 2000 bytes and, to change a memory, both its id and the version you read from memories().";
const MEMORY_UNWIRED: &str = "Not saved: this engine does not take memories from its master yet.";
const NO_MEMORIES: &str = "This project has no memories yet. Suggest one with remember().";
const MEMORIES_HEADER: &str = "This project's memories. To change a suggested one, call remember() with its id and the version shown:";
const MEMORIES_MORE_BYTES: usize = 64;

const _: () = assert!(REPLY_BYTES_MAX < REQUEST_BYTES_MAX as u64);
const _: () = assert!(CALLS_PER_TICK_MAX <= CALLS_PENDING_MAX);
const _: () = assert!(super::events::ESCALATION_BYTES_MAX == 2000);

/// One team tool call a live master made, as its server writes it into the mailbox for the
/// engine to answer: `delegate` admits a task now, `message` and `answer` type into a
/// sub-agent's terminal, `status` and `changes` read the team and its pending change sets,
/// `escalate` raises an app notification for a decision only the owner makes, `memories`
/// reads the project's memories with their versions and `remember` suggests one or changes
/// a suggestion at the version it names.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "tool", rename_all = "snake_case", deny_unknown_fields)]
pub enum Call {
    Delegate {
        task: String,
        harness: Option<String>,
        role: Option<String>,
    },
    Message {
        agent: String,
        text: String,
    },
    Answer {
        agent: String,
        text: String,
    },
    Status,
    Changes,
    Escalate {
        text: String,
    },
    Memories,
    Remember {
        text: String,
        memory: Option<String>,
        version: Option<u64>,
    },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Recorded {
    harness: Option<String>,
    task: String,
    #[serde(default)]
    role: Option<String>,
}

/// Serves the master's `delegate` tool as an MCP server over stdio until its client closes input.
/// Takes the client's input and output and the absolute drop file in the master's private temporary folder.
/// Returns an input or output failure; each call only records a proposal, which the engine admits after the turn.
pub fn serve(input: impl BufRead, output: impl Write, drop: &Path) -> std::io::Result<()> {
    if !drop.is_absolute() {
        return Err(std::io::Error::other(
            "the drop file must be an absolute path",
        ));
    }
    rpc(input, output, |request| answer(request, drop))
}

/// Serves a live master's team tools as an MCP server over stdio until its client closes
/// input: each call goes into the mailbox folder in the master's private temporary folder
/// and its result is the engine's answer, so `delegate` takes effect at once.
/// Takes the client's input and output and the absolute mailbox folder the engine made.
/// Returns an input or output failure, or a refusal when the mailbox is not a plain folder.
pub fn serve_live(input: impl BufRead, output: impl Write, mailbox: &Path) -> std::io::Result<()> {
    let plain = std::fs::symlink_metadata(mailbox).is_ok_and(|metadata| metadata.is_dir());
    if !mailbox.is_absolute() || !plain {
        return Err(std::io::Error::other(
            "the mailbox must be an absolute plain folder",
        ));
    }
    rpc(input, output, |request| answer_live(request, mailbox))
}

fn rpc(
    mut input: impl BufRead,
    mut output: impl Write,
    mut answer: impl FnMut(&Value) -> Option<Value>,
) -> std::io::Result<()> {
    loop {
        let mut line = Vec::new();
        let read = input
            .by_ref()
            .take(REQUEST_BYTES_MAX as u64 + 1)
            .read_until(b'\n', &mut line)?;
        if read == 0 {
            return Ok(());
        }
        if line.len() > REQUEST_BYTES_MAX {
            reply(
                &mut output,
                &failure(Value::Null, -32600, "Request too large"),
            )?;
            return Ok(());
        }
        let text = line.trim_ascii();
        if text.is_empty() {
            continue;
        }
        let answered = match serde_json::from_slice::<Value>(text) {
            Ok(request) => answer(&request),
            Err(_) => Some(failure(Value::Null, -32700, "Parse error")),
        };
        if let Some(answered) = answered {
            reply(&mut output, &answered)?;
        }
    }
}

fn reply(output: &mut impl Write, message: &Value) -> std::io::Result<()> {
    let mut line = serde_json::to_vec(message).map_err(std::io::Error::other)?;
    line.push(b'\n');
    output.write_all(&line)?;
    output.flush()
}

fn failure(id: Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

fn initialized(params: Option<&Value>) -> Value {
    let asked = params
        .and_then(|params| params.get("protocolVersion"))
        .and_then(Value::as_str);
    let version = asked
        .filter(|asked| PROTOCOL_VERSIONS.contains(asked))
        .unwrap_or(PROTOCOL_VERSIONS[PROTOCOL_VERSIONS.len() - 1]);
    json!({
        "protocolVersion": version,
        "capabilities": {"tools": {}},
        "serverInfo": {"name": SERVER_NAME, "version": env!("CARGO_PKG_VERSION")},
    })
}

fn tool_result(text: &str, error: bool) -> Value {
    json!({"content": [{"type": "text", "text": text}], "isError": error})
}

fn answer(request: &Value, drop: &Path) -> Option<Value> {
    let method = request.get("method")?.as_str();
    let id = request.get("id")?.clone();
    let params = request.get("params");
    let result = match method {
        Some("initialize") => initialized(params),
        Some("ping") => json!({}),
        Some("tools/list") => json!({"tools": [{
            "name": TOOL_NAME,
            "description": "Hand one task to a sub-agent of this project's team. The engine checks it against the team plan when your reply ends, runs it in its own git worktree and sends you the result.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "task": {"type": "string", "description": "What the sub-agent should do, in plain words."},
                    "harness": {"type": "string", "description": "Optional sub-agent harness from the team plan, such as codex or claude-code."},
                    "role": {"type": "string", "description": "Optional short name for this job in kebab case, such as onboarding-fix. The sub-agent is then addressed as @onboarding-fix."},
                },
                "required": ["task"],
                "additionalProperties": false,
            },
        }]}),
        Some("tools/call") => match call(params, drop) {
            Some((text, error)) => tool_result(text, error),
            None => return Some(failure(id, -32602, "Unknown tool")),
        },
        _ => return Some(failure(id, -32601, "Method not found")),
    };
    Some(json!({"jsonrpc": "2.0", "id": id, "result": result}))
}

fn live_tools() -> Value {
    let agent = json!({"type": "string", "description": "The sub-agent: its @role handle, its persona name or its number."});
    let text = json!({"type": "string", "description": "What to type into the sub-agent's terminal, in plain words."});
    let none = json!({"type": "object", "properties": {}, "additionalProperties": false});
    json!({"tools": [
        {
            "name": TOOL_NAME,
            "description": "Hand one task to a sub-agent of this project's team now. The engine checks it against the team plan, runs it in its own git worktree and tells you when it ends.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "task": {"type": "string", "description": "What the sub-agent should do, in plain words."},
                    "harness": {"type": "string", "description": "Optional sub-agent harness from the team plan, such as codex or claude-code."},
                    "role": {"type": "string", "description": "Optional short role handle for the sub-agent's job, such as @onboarding-fix."},
                },
                "required": ["task"],
                "additionalProperties": false,
            },
        },
        {
            "name": MESSAGE_TOOL,
            "description": "Type a message into one sub-agent's terminal, such as a correction or more detail.",
            "inputSchema": {"type": "object", "properties": {"agent": agent, "text": text}, "required": ["agent", "text"], "additionalProperties": false},
        },
        {
            "name": ANSWER_TOOL,
            "description": "Answer the question a sub-agent is waiting on; the answer is typed into its terminal.",
            "inputSchema": {"type": "object", "properties": {"agent": agent, "text": text}, "required": ["agent", "text"], "additionalProperties": false},
        },
        {
            "name": STATUS_TOOL,
            "description": "Read the team: each sub-agent, its job and state, and the tasks waiting or running.",
            "inputSchema": none,
        },
        {
            "name": CHANGES_TOOL,
            "description": "List the change sets waiting for the owner's review in Changes.",
            "inputSchema": none,
        },
        {
            "name": ESCALATE_TOOL,
            "description": "Ask the owner for a decision only they make: security, destructive or irreversible actions, spending, product or scope. The owner gets an app notification; wait for their answer in this terminal.",
            "inputSchema": {"type": "object", "properties": {"text": {"type": "string", "description": "The decision the owner should make, in one plain question."}}, "required": ["text"], "additionalProperties": false},
        },
        {
            "name": MEMORIES_TOOL,
            "description": "Read this project's memories, each with its id, its version and whether the owner reviewed it.",
            "inputSchema": none,
        },
        {
            "name": REMEMBER_TOOL,
            "description": "Suggest a memory the owner keeps or rejects, or change a suggested one by naming its id and the version you read. A change to an older version is refused as stale: read memories() again and redo it on the current text. Agents never change a memory the owner reviewed.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "text": {"type": "string", "description": "The memory, in plain words, up to 2000 bytes."},
                    "memory": {"type": "string", "description": "The id of the suggested memory to change, from memories()."},
                    "version": {"type": "integer", "minimum": 1, "description": "The version of that memory you read, from memories()."},
                },
                "required": ["text"],
                "additionalProperties": false,
            },
        },
    ]})
}

fn answer_live(request: &Value, mailbox: &Path) -> Option<Value> {
    let method = request.get("method")?.as_str();
    let id = request.get("id")?.clone();
    let params = request.get("params");
    let result = match method {
        Some("initialize") => initialized(params),
        Some("ping") => json!({}),
        Some("tools/list") => live_tools(),
        Some("tools/call") => {
            let name = params
                .and_then(|params| params.get("name"))
                .and_then(Value::as_str);
            let arguments = params.and_then(|params| params.get("arguments"));
            match name.map(|name| live_call(name, arguments)) {
                Some(Some(Ok(call))) => {
                    let (text, error) = ask(mailbox, &call, REPLY_WAIT);
                    tool_result(&text, error)
                }
                Some(Some(Err(refused))) => tool_result(refused, true),
                _ => return Some(failure(id, -32602, "Unknown tool")),
            }
        }
        _ => return Some(failure(id, -32601, "Method not found")),
    };
    Some(json!({"jsonrpc": "2.0", "id": id, "result": result}))
}

/// Reads one live tool call from its MCP name and arguments, checking every bound the
/// engine checks again. Takes the tool name and arguments. Returns none for an unknown
/// tool, or the call, or the plain sentence the master reads when an argument is refused.
pub fn live_call(name: &str, arguments: Option<&Value>) -> Option<Result<Call, &'static str>> {
    let argument = |key: &str| {
        arguments
            .and_then(|arguments| arguments.get(key))
            .and_then(Value::as_str)
    };
    let typed = |agent: Option<&str>, text: Option<&str>| match (agent, text) {
        (Some(agent), Some(text))
            if agent_name_valid(agent) && super::text_valid(text, TASK_PROMPT_BYTES_MAX) =>
        {
            Ok((agent.trim().to_owned(), text.trim().to_owned()))
        }
        _ => Err(TYPED_INVALID),
    };
    Some(match name {
        TOOL_NAME => {
            let Some(proposed) = delegation(
                argument("harness").unwrap_or_default(),
                argument("task").unwrap_or_default(),
            ) else {
                return Some(Err(LIVE_INVALID));
            };
            let role = match argument("role").map(role_handle) {
                Some(Err(())) => return Some(Err(ROLE_INVALID)),
                Some(Ok(role)) => role,
                None => None,
            };
            Ok(Call::Delegate {
                task: proposed.prompt,
                harness: proposed.harness,
                role,
            })
        }
        MESSAGE_TOOL => typed(argument("agent"), argument("text"))
            .map(|(agent, text)| Call::Message { agent, text }),
        ANSWER_TOOL => typed(argument("agent"), argument("text"))
            .map(|(agent, text)| Call::Answer { agent, text }),
        STATUS_TOOL => Ok(Call::Status),
        CHANGES_TOOL => Ok(Call::Changes),
        ESCALATE_TOOL => match argument("text") {
            Some(text) if super::text_valid(text, super::events::ESCALATION_BYTES_MAX) => {
                Ok(Call::Escalate {
                    text: text.trim().to_owned(),
                })
            }
            _ => Err(ESCALATE_INVALID),
        },
        MEMORIES_TOOL => Ok(Call::Memories),
        REMEMBER_TOOL => remember_call(arguments).ok_or(MEMORY_INVALID),
        _ => return None,
    })
}

fn remember_call(arguments: Option<&Value>) -> Option<Call> {
    let field = |key: &str| {
        arguments
            .and_then(|arguments| arguments.get(key))
            .filter(|value| !value.is_null())
    };
    let text = field("text")?
        .as_str()
        .filter(|text| super::text_valid(text, NOTE_BYTES_MAX))?;
    let memory = match field("memory") {
        None => None,
        Some(memory) => Some(
            memory
                .as_str()
                .filter(|memory| RequestId::new(*memory).is_ok())?
                .to_owned(),
        ),
    };
    let version = match field("version") {
        None => None,
        Some(version) => Some(version.as_u64().filter(|version| *version >= 1)?),
    };
    if memory.is_some() != version.is_some() {
        return None;
    }
    Some(Call::Remember {
        text: text.trim().to_owned(),
        memory,
        version,
    })
}

/// Checks a call the engine read from a mailbox, which the agent's side could have written.
/// Takes the call. Returns it, or the plain sentence it was refused with.
pub fn checked(call: Call) -> Result<Call, &'static str> {
    let (name, arguments) = match &call {
        Call::Delegate {
            task,
            harness,
            role,
        } => (
            TOOL_NAME,
            json!({"task": task, "harness": harness, "role": role}),
        ),
        Call::Message { agent, text } => (MESSAGE_TOOL, json!({"agent": agent, "text": text})),
        Call::Answer { agent, text } => (ANSWER_TOOL, json!({"agent": agent, "text": text})),
        Call::Status => (STATUS_TOOL, json!({})),
        Call::Changes => (CHANGES_TOOL, json!({})),
        Call::Escalate { text } => (ESCALATE_TOOL, json!({"text": text})),
        Call::Memories => (MEMORIES_TOOL, json!({})),
        Call::Remember {
            text,
            memory,
            version,
        } => (
            REMEMBER_TOOL,
            json!({"text": text, "memory": memory, "version": version}),
        ),
    };
    let again = live_call(name, Some(&arguments)).expect("every call names a live tool")?;
    if again != call {
        return Err(LIVE_INVALID);
    }
    Ok(call)
}

/// Answers a live master's `memories` call with the project's memories, each with the id
/// and version a change names, within `REPLY_TEXT_MAX`. Takes the engine database and the
/// project. Returns the text the master reads and whether it is an error.
pub fn answer_memories(connection: &mut Connection, project: &ProjectId) -> (String, bool) {
    let listed = connection
        .transaction()
        .map_err(|problem| EngineError(problem.to_string()))
        .and_then(|tx| agent_memories(&tx, project));
    match listed {
        Ok(memories) => (memories_text(&memories), false),
        Err(problem) => (
            format!(
                "The memories could not be read ({}).",
                super::bound_text(&problem.0, 120)
            ),
            true,
        ),
    }
}

/// Answers a live master's `remember` call: saves a suggestion, or the next version of one
/// when the call names the version the master read, in one immediate transaction that
/// commits only a saved write. A stale write tells the master to re-read and redo with the
/// current version; agents can never force one. Takes the engine database, the project, the
/// engine settings (none when the engine gave none), the memory and version read and the
/// text. Returns the text the master reads and whether it is an error.
pub fn answer_remember(
    connection: &mut Connection,
    project: &ProjectId,
    config: Option<&EffectiveConfig>,
    memory: Option<&str>,
    version: Option<u64>,
    text: &str,
) -> (String, bool) {
    let Some(config) = config else {
        return (MEMORY_UNWIRED.to_owned(), true);
    };
    let read = match (memory.map(RequestId::new), version) {
        (None, None) => None,
        (Some(Ok(memory)), Some(version)) if version >= 1 => Some((memory, version)),
        _ => return (MEMORY_INVALID.to_owned(), true),
    };
    let written = remember_once(connection, project, config, read.as_ref(), text);
    match written {
        Ok(written) => remembered_text(&written),
        Err(problem) => (
            format!(
                "Not saved: the memory could not be written ({}).",
                super::bound_text(&problem.0, 120)
            ),
            true,
        ),
    }
}

fn remember_once(
    connection: &mut Connection,
    project: &ProjectId,
    config: &EffectiveConfig,
    read: Option<&(RequestId, u64)>,
    text: &str,
) -> Result<AgentWrite, EngineError> {
    let failed = |problem: rusqlite::Error| EngineError(problem.to_string());
    let tx = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(failed)?;
    let master: Option<i64> = tx
        .query_row(
            "SELECT agent FROM team_live_masters WHERE project=?1 AND live=1",
            [project.as_str()],
            |row| row.get(0),
        )
        .optional()
        .map_err(failed)?
        .flatten();
    let Some(agent) = master.and_then(|master| AgentId::team(master).ok()) else {
        return Ok(AgentWrite::Refused {
            code: "memory_agent_missing".into(),
        });
    };
    let revision: i64 = tx
        .query_row("SELECT revision FROM state WHERE id=1", [], |row| {
            row.get(0)
        })
        .map_err(failed)?;
    let revision = u64::try_from(revision).map_err(|problem| EngineError(problem.to_string()))?;
    let written = match agent_remember(
        &tx,
        config,
        project,
        &agent,
        read.map(|(memory, version)| (memory, *version)),
        text,
        revision,
    ) {
        Err(problem) if problem.0 == "knowledge_record_capacity" => {
            AgentWrite::Refused { code: problem.0 }
        }
        written => written?,
    };
    if matches!(written, AgentWrite::Saved { .. }) {
        tx.commit().map_err(failed)?;
    }
    Ok(written)
}

fn memories_text(memories: &[AgentMemory]) -> String {
    if memories.is_empty() {
        return NO_MEMORIES.to_owned();
    }
    let mut text = MEMORIES_HEADER.to_owned();
    for (shown, memory) in memories.iter().enumerate() {
        let line = format!(
            "\n- {} version {} ({}): {}",
            memory.id.as_str(),
            memory.version,
            if memory.reviewed {
                "reviewed by the owner"
            } else {
                "suggested"
            },
            memory.text
        );
        if text.len() + line.len() + MEMORIES_MORE_BYTES > REPLY_TEXT_MAX {
            text.push_str(&format!("\n{} more not shown.", memories.len() - shown));
            break;
        }
        text.push_str(&line);
    }
    assert!(text.len() <= REPLY_TEXT_MAX);
    text
}

fn remembered_text(written: &AgentWrite) -> (String, bool) {
    match written {
        AgentWrite::Saved { id, version } => (
            format!(
                "Saved memory {} at version {version} as a suggestion the owner keeps or rejects. Name version {version} to change it.",
                id.as_str()
            ),
            false,
        ),
        AgentWrite::Stale { id, stale } => (
            format!(
                "Stale: re-read and redo. Memory {} is at version {} now, changed by {} since version {} you read. Call memories(), then redo your change on the current text naming version {}.",
                id.as_str(),
                stale.current,
                writer_text(&stale.writer),
                stale.version,
                stale.current
            ),
            true,
        ),
        AgentWrite::Reviewed { id, version } => (
            format!(
                "Not saved: the owner reviewed memory {} at version {version}, and agents do not change a memory the owner reviewed. Suggest a new memory with remember() if it still matters.",
                id.as_str()
            ),
            true,
        ),
        AgentWrite::Refused { code } => (
            format!(
                "Not saved: {}.",
                match code.as_str() {
                    "memory_missing" =>
                        "this project has no memory with that id; read the ids with memories()"
                            .to_owned(),
                    "memory_duplicate" => "you already suggested this memory".to_owned(),
                    "memory_text_invalid" => "give one plain memory of up to 2000 bytes".to_owned(),
                    "memory_source_removed" =>
                        "the owner removed what this memory came from".to_owned(),
                    "memory_agent_missing" =>
                        "only this project's live master saves memories".to_owned(),
                    "memory_suggestions_full" => format!(
                        "this project already has {AGENT_SUGGESTIONS_MAX} suggested memories the owner has not reviewed; change one of them or wait for the owner to keep or reject them"
                    ),
                    "memory_versions_full" => format!(
                        "this suggestion already has {AGENT_VERSIONS_MAX} versions; wait for the owner to review it or suggest a new memory"
                    ),
                    "knowledge_record_capacity" =>
                        "the engine's memory is full until the owner deletes memories".to_owned(),
                    other => other.to_owned(),
                }
            ),
            true,
        ),
    }
}

fn writer_text(writer: &LastWriter) -> String {
    match writer {
        LastWriter::Owner => "the owner".to_owned(),
        LastWriter::Agent(agent) => format!("agent {}", agent.as_str()),
        LastWriter::Nobody => "another writer".to_owned(),
    }
}

fn agent_name_valid(agent: &str) -> bool {
    let agent = agent.trim();
    !agent.is_empty() && agent.len() <= AGENT_NAME_BYTES_MAX && !agent.chars().any(char::is_control)
}

/// Reads the role a master gave a job by the same rule as the reply-turn `delegate` tool:
/// at most `ROLE_BYTES_MAX` bytes, turned into a handle by `names::handle`.
/// Takes the text, `@onboarding-fix` or `Onboarding Fix`. Returns the bare handle, none
/// for blank text, or an error when no handle is left or the text is too long.
fn role_handle(text: &str) -> Result<Option<String>, ()> {
    let text = text.trim();
    if text.is_empty() {
        return Ok(None);
    }
    if text.len() > ROLE_BYTES_MAX {
        return Err(());
    }
    names::handle(text).map(Some).ok_or(())
}

/// Writes one call into the mailbox under a part name and renames it into place, so the
/// engine never takes half a call, then waits for the engine's answer. A call still in the
/// mailbox at the deadline is withdrawn, so the engine never runs it; one the engine has
/// claimed is reported as taken but unanswered. Takes the mailbox, the call and the wait.
/// Returns the answer text and whether it is an error.
fn ask(mailbox: &Path, call: &Call, wait: std::time::Duration) -> (String, bool) {
    let pending = calls(mailbox, CALL_SUFFIX).len();
    if pending >= CALLS_PENDING_MAX {
        return (BUSY.into(), true);
    }
    let Ok(id) = call_id() else {
        return (UNSENT.into(), true);
    };
    let Ok(mut line) = serde_json::to_vec(call) else {
        return (UNSENT.into(), true);
    };
    line.push(b'\n');
    let part = mailbox.join(format!("{id}{PART_SUFFIX}"));
    let request = mailbox.join(format!("{id}{CALL_SUFFIX}"));
    if create_private(&part)
        .and_then(|mut file| file.write_all(&line))
        .and_then(|()| std::fs::rename(&part, &request))
        .is_err()
    {
        let _ = std::fs::remove_file(&part);
        return (UNSENT.into(), true);
    }
    let answered = mailbox.join(format!("{id}{REPLY_SUFFIX}"));
    let deadline = std::time::Instant::now() + wait;
    while std::time::Instant::now() < deadline {
        if let Some(found) = read_reply(&answered) {
            let _ = std::fs::remove_file(&answered);
            return found;
        }
        std::thread::sleep(REPLY_POLL);
    }
    if std::fs::remove_file(&request).is_ok() {
        return (NOT_TAKEN.into(), true);
    }
    (NOT_ANSWERED.into(), true)
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Reply {
    text: String,
    error: bool,
}

fn read_reply(path: &Path) -> Option<(String, bool)> {
    let file = open_recorded(path).ok()?;
    if !file.metadata().is_ok_and(|metadata| metadata.is_file()) {
        return None;
    }
    let mut bytes = Vec::new();
    (&file).take(REPLY_BYTES_MAX).read_to_end(&mut bytes).ok()?;
    let reply: Reply = serde_json::from_slice(&bytes).ok()?;
    Some((reply.text, reply.error))
}

fn call_id() -> std::io::Result<String> {
    let mut bytes = [0_u8; 16];
    getrandom::fill(&mut bytes).map_err(|_| std::io::Error::other("no random bytes"))?;
    let id: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    assert!(call_id_valid(&id));
    Ok(id)
}

fn call_id_valid(id: &str) -> bool {
    id.len() == 32
        && id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Lists the mailbox entries with one suffix whose names carry a valid call id, oldest
/// first, never more than `MAILBOX_SCAN_MAX` entries looked at; a link at the mailbox's
/// own name is never followed.
fn calls(mailbox: &Path, suffix: &str) -> Vec<(std::time::SystemTime, String)> {
    Mailbox::open(mailbox)
        .map(|mailbox| mailbox.entries(suffix))
        .unwrap_or_default()
}

/// A live master's mailbox folder, opened once without following a link; every listing,
/// claim, answer and removal then goes through this handle, so a link the master swaps in
/// for the folder's path afterwards is never followed.
#[cfg(unix)]
struct Mailbox(rustix::fd::OwnedFd);

/// A live master's mailbox where no directory-relative no-follow open exists; it cannot be
/// opened, so every use is unreachable by construction.
#[cfg(not(unix))]
enum Mailbox {}

#[cfg(unix)]
impl Mailbox {
    fn folder_flags() -> OFlags {
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK
    }

    /// Opens a mailbox folder, refusing a link at its own name. Takes its path. Returns
    /// the handle or the I/O error.
    fn open(path: &Path) -> std::io::Result<Self> {
        Ok(Self(rustix::fs::open(
            path,
            Self::folder_flags(),
            Mode::empty(),
        )?))
    }

    /// Opens a live master's mailbox for the engine from the folder above the master's
    /// temporary folder, refusing a link at the temporary folder or the mailbox: the master
    /// can replace both. Takes the mailbox path. Returns the handle or the I/O error.
    fn open_for_engine(path: &Path) -> std::io::Result<Self> {
        let refused = || std::io::Error::other("the mailbox is not inside a folder");
        let mailbox = path.file_name().ok_or_else(refused)?;
        let holder = path.parent().ok_or_else(refused)?;
        let held = holder.file_name().ok_or_else(refused)?;
        let above = holder.parent().ok_or_else(refused)?;
        let flags = Self::folder_flags();
        let above = rustix::fs::open(above, flags, Mode::empty())?;
        let holder = rustix::fs::openat(&above, held, flags, Mode::empty())?;
        Ok(Self(rustix::fs::openat(
            &holder,
            mailbox,
            flags,
            Mode::empty(),
        )?))
    }

    /// Lists the entries with one suffix whose names carry a valid call id and that are
    /// plain files, oldest first, never more than `MAILBOX_SCAN_MAX` entries looked at.
    fn entries(&self, suffix: &str) -> Vec<(std::time::SystemTime, String)> {
        let Ok(listed) = rustix::fs::Dir::read_from(&self.0) else {
            return Vec::new();
        };
        let mut found: Vec<(std::time::SystemTime, String)> = listed
            .flatten()
            .filter(|entry| !matches!(entry.file_name().to_bytes(), b"." | b".."))
            .take(MAILBOX_SCAN_MAX)
            .filter_map(|entry| {
                let name = entry.file_name().to_str().ok()?;
                let id = name.strip_suffix(suffix)?;
                if !call_id_valid(id) {
                    return None;
                }
                let stat =
                    rustix::fs::statat(&self.0, entry.file_name(), AtFlags::SYMLINK_NOFOLLOW)
                        .ok()?;
                (FileType::from_raw_mode(stat.st_mode) == FileType::RegularFile)
                    .then(|| (modified(&stat), id.to_owned()))
            })
            .collect();
        found.sort();
        found
    }

    /// Creates a new entry 0600 for writing, failing when anything, a link included, is
    /// there. Takes its name. Returns the open file or the I/O error.
    fn create(&self, name: &str) -> std::io::Result<File> {
        Ok(File::from(rustix::fs::openat(
            &self.0,
            name,
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::RUSR | Mode::WUSR,
        )?))
    }

    /// Opens an entry to read without following a link or blocking on a FIFO. Takes its
    /// name. Returns the open file or the I/O error.
    fn read(&self, name: &str) -> std::io::Result<File> {
        Ok(File::from(rustix::fs::openat(
            &self.0,
            name,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::empty(),
        )?))
    }

    /// Renames an entry within the mailbox; a link is moved, never followed. Takes both
    /// names. Returns the I/O error.
    fn rename(&self, from: &str, to: &str) -> std::io::Result<()> {
        rustix::fs::renameat(&self.0, from, &self.0, to).map_err(std::io::Error::from)
    }

    /// Removes an entry; a link is removed, never followed. Takes its name. Returns the
    /// I/O error.
    fn remove(&self, name: &str) -> std::io::Result<()> {
        rustix::fs::unlinkat(&self.0, name, AtFlags::empty()).map_err(std::io::Error::from)
    }
}

/// Refuses a mailbox where no directory-relative no-follow open exists: both opens return
/// an `Unsupported` error reading `team_mailbox_unavailable`, and the other operations
/// cannot be reached.
#[cfg(not(unix))]
impl Mailbox {
    fn open(_path: &Path) -> std::io::Result<Self> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "team_mailbox_unavailable",
        ))
    }

    fn open_for_engine(path: &Path) -> std::io::Result<Self> {
        Self::open(path)
    }

    fn entries(&self, _suffix: &str) -> Vec<(std::time::SystemTime, String)> {
        match *self {}
    }

    fn create(&self, _name: &str) -> std::io::Result<File> {
        match *self {}
    }

    fn read(&self, _name: &str) -> std::io::Result<File> {
        match *self {}
    }

    fn rename(&self, _from: &str, _to: &str) -> std::io::Result<()> {
        match *self {}
    }

    fn remove(&self, _name: &str) -> std::io::Result<()> {
        match *self {}
    }
}

/// Reads an entry's last change time from its status, the epoch when it does not fit.
/// Takes the status. Returns the time.
#[cfg(unix)]
fn modified(stat: &rustix::fs::Stat) -> std::time::SystemTime {
    let seconds = u64::try_from(stat.st_mtime).ok();
    let nanos = u32::try_from(stat.st_mtime_nsec)
        .ok()
        .filter(|nanos| *nanos < 1_000_000_000);
    seconds
        .zip(nanos)
        .and_then(|(seconds, nanos)| {
            std::time::UNIX_EPOCH.checked_add(std::time::Duration::new(seconds, nanos))
        })
        .unwrap_or(std::time::UNIX_EPOCH)
}

/// Takes the oldest calls in a live master's mailbox, each claimed by renaming it before
/// it is read, so a call withdrawn at its deadline is never run, and removes answers and
/// claims left for `REPLY_STALE`. Links and other non-files, the mailbox and the master's
/// temporary folder included, are never followed; a call past `CALL_BYTES_MAX` is removed
/// unread. Takes the mailbox and how many to take. Returns each call id with its call, or
/// none when the file did not hold a valid call.
pub fn take_calls(mailbox: &Path, max: usize) -> Vec<(String, Option<Call>)> {
    let Ok(mailbox) = Mailbox::open_for_engine(mailbox) else {
        return Vec::new();
    };
    let now = std::time::SystemTime::now();
    for suffix in [REPLY_SUFFIX, TAKEN_SUFFIX] {
        for (modified, id) in mailbox.entries(suffix) {
            if now
                .duration_since(modified)
                .is_ok_and(|age| age >= REPLY_STALE)
            {
                let _ = mailbox.remove(&format!("{id}{suffix}"));
            }
        }
    }
    let mut taken = Vec::new();
    for (_, id) in mailbox.entries(CALL_SUFFIX).into_iter().take(max) {
        let claimed = format!("{id}{TAKEN_SUFFIX}");
        if mailbox
            .rename(&format!("{id}{CALL_SUFFIX}"), &claimed)
            .is_err()
        {
            continue;
        }
        let call = mailbox.read(&claimed).ok().and_then(|file| {
            let metadata = file.metadata().ok()?;
            if !metadata.is_file() || metadata.len() > CALL_BYTES_MAX {
                return None;
            }
            let mut bytes = Vec::new();
            (&file).take(CALL_BYTES_MAX).read_to_end(&mut bytes).ok()?;
            serde_json::from_slice::<Call>(bytes.trim_ascii()).ok()
        });
        let _ = mailbox.remove(&claimed);
        taken.push((id, call));
    }
    assert!(taken.len() <= max);
    taken
}

/// Answers one call in the mailbox: the answer is written beside it under a part name and
/// renamed into place, so the server never reads half an answer. Takes the mailbox, the
/// call id `take_calls` returned, the text, bounded to `REPLY_TEXT_MAX`, and whether it is
/// an error. Returns the I/O failure; a planted part file, or a link at the mailbox or the
/// master's temporary folder, makes the answer fail, never land elsewhere.
pub fn answer_call(mailbox: &Path, id: &str, text: &str, error: bool) -> std::io::Result<()> {
    assert!(call_id_valid(id));
    let reply = Reply {
        text: super::bound_text(text, REPLY_TEXT_MAX),
        error,
    };
    let bytes = serde_json::to_vec(&reply).map_err(std::io::Error::other)?;
    assert!(bytes.len() as u64 <= REPLY_BYTES_MAX);
    let mailbox = Mailbox::open_for_engine(mailbox)?;
    let part = format!("{id}{PART_SUFFIX}");
    let written = mailbox
        .create(&part)
        .and_then(|mut file| file.write_all(&bytes));
    if let Err(problem) = written {
        let _ = mailbox.remove(&part);
        return Err(problem);
    }
    mailbox.rename(&part, &format!("{id}{REPLY_SUFFIX}"))
}

/// Creates a new file 0600 for writing, failing when anything, a link included, is there.
/// Takes its path. Returns the open file or the I/O error.
#[cfg(unix)]
fn create_private(path: &Path) -> std::io::Result<File> {
    Ok(File::from(rustix::fs::open(
        path,
        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::RUSR | Mode::WUSR,
    )?))
}

/// Creates a new file for writing with the private folder's inherited permissions,
/// failing when anything, a reparse point included, is there.
/// Takes its path. Returns the open file or the I/O error.
#[cfg(windows)]
fn create_private(path: &Path) -> std::io::Result<File> {
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
}

fn call(params: Option<&Value>, drop: &Path) -> Option<(&'static str, bool)> {
    if params?.get("name")?.as_str()? != TOOL_NAME {
        return None;
    }
    let argument = |name: &str| {
        params
            .and_then(|params| params.get("arguments"))
            .and_then(|arguments| arguments.get(name))
            .and_then(Value::as_str)
            .unwrap_or_default()
    };
    let role = argument("role").trim();
    let handle = names::handle(role);
    if role.len() > ROLE_BYTES_MAX || (!role.is_empty() && handle.is_none()) {
        return Some((INVALID, true));
    }
    let Some(mut proposed) = delegation(argument("harness"), argument("task")) else {
        return Some((INVALID, true));
    };
    proposed.role = handle;
    Some(match record(drop, &proposed) {
        Ok(true) => (RECORDED, false),
        Ok(false) => (FULL, true),
        Err(_) => (UNWRITABLE, true),
    })
}

/// Opens the drop file to read and append, creating it 0600, never through a link.
/// Takes its path. Returns the open file or the I/O error.
#[cfg(unix)]
fn open_drop(drop: &Path) -> std::io::Result<File> {
    Ok(File::from(rustix::fs::open(
        drop,
        OFlags::RDWR | OFlags::CREATE | OFlags::APPEND | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::RUSR | Mode::WUSR,
    )?))
}

/// Opens the drop file to read and append, creating it with the private folder's
/// inherited permissions, as the reparse point itself and never a link's target.
/// Takes its path. Returns the open file or the I/O error.
#[cfg(windows)]
fn open_drop(drop: &Path) -> std::io::Result<File> {
    use std::os::windows::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .read(true)
        .append(true)
        .create(true)
        .custom_flags(crate::platform::FILE_FLAG_OPEN_REPARSE_POINT)
        .open(drop)
}

/// Opens the drop file to read without following a link or blocking on a FIFO.
/// Takes its path. Returns the open file or the I/O error.
#[cfg(unix)]
fn open_recorded(drop: &Path) -> std::io::Result<File> {
    Ok(File::from(rustix::fs::open(
        drop,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    )?))
}

/// Opens the drop file to read as the reparse point itself, never a link's target.
/// Takes its path. Returns the open file or the I/O error.
#[cfg(windows)]
fn open_recorded(drop: &Path) -> std::io::Result<File> {
    use std::os::windows::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(crate::platform::FILE_FLAG_OPEN_REPARSE_POINT)
        .open(drop)
}

fn record(drop: &Path, proposed: &Delegation) -> std::io::Result<bool> {
    let mut file = open_drop(drop)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(std::io::Error::other("the drop file is not a regular file"));
    }
    let mut recorded = Vec::new();
    (&file).take(DROP_BYTES_MAX).read_to_end(&mut recorded)?;
    let mut line = serde_json::to_vec(
        &json!({"harness": proposed.harness, "task": proposed.prompt, "role": proposed.role}),
    )
    .map_err(std::io::Error::other)?;
    line.push(b'\n');
    let calls = recorded.iter().filter(|byte| **byte == b'\n').count();
    if calls >= DELEGATIONS_PER_REPLY_MAX || metadata.len() + line.len() as u64 > DROP_BYTES_MAX {
        return Ok(false);
    }
    file.write_all(&line)?;
    Ok(true)
}

/// Takes the tasks a master proposed with its `delegate` tool during one turn, and clears the record.
/// Takes the drop file in the master's private temporary folder; a link or other non-file is never read.
/// Returns at most eight valid proposals in call order, none when there is no record.
pub fn take(drop: &Path) -> Vec<Delegation> {
    let opened = open_recorded(drop);
    clear(drop);
    let Ok(file) = opened else {
        return Vec::new();
    };
    if !file.metadata().is_ok_and(|metadata| metadata.is_file()) {
        return Vec::new();
    }
    let mut text = Vec::new();
    if (&file).take(DROP_BYTES_MAX).read_to_end(&mut text).is_err() {
        return Vec::new();
    }
    let found: Vec<Delegation> = text
        .split(|byte| *byte == b'\n')
        .filter_map(|line| serde_json::from_slice::<Recorded>(line).ok())
        .filter_map(|recorded| {
            let mut found = delegation(
                recorded.harness.as_deref().unwrap_or_default(),
                &recorded.task,
            )?;
            found.role = recorded.role.as_deref().and_then(names::handle);
            Some(found)
        })
        .take(DELEGATIONS_PER_REPLY_MAX)
        .collect();
    assert!(found.len() <= DELEGATIONS_PER_REPLY_MAX);
    found
}

/// Removes a master's drop file so a new turn starts with no recorded proposal.
/// Takes the drop file path; a link there is removed, never followed.
/// Returns nothing; a missing file is already clear.
pub fn clear(drop: &Path) {
    if std::fs::symlink_metadata(drop).is_ok_and(|metadata| !metadata.is_dir()) {
        let _ = std::fs::remove_file(drop);
    }
}

#[cfg(all(test, unix))]
#[path = "tool/tests.rs"]
mod tests;

#[cfg(all(test, unix))]
#[path = "tool/stale_tests.rs"]
mod stale_tests;
