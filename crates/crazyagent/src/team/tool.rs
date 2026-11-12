use super::{DELEGATIONS_PER_REPLY_MAX, Delegation, TASK_PROMPT_BYTES_MAX, delegation, names};
use crate::coord::{Finish, QuestionFor, Severity};
use crate::questions::QuestionKind;
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
/// The engine subcommand that serves a sub-agent's team tools; it lists the same tools as
/// the live master's server, and the engine answers each by the caller's place in the team.
pub const AGENT_SERVER_COMMAND: &str = "team-mcp-agent";
pub const START_WORK_TOOL: &str = "start_work";
pub const INSPECT_WORK_TOOL: &str = "inspect_work";
pub const UPDATE_WORK_TOOL: &str = "update_work";
pub const TRANSFER_WORK_TOOL: &str = "transfer_work";
pub const FINISH_WORK_TOOL: &str = "finish_work";
pub const SPAWN_TOOL: &str = "spawn";
pub const STOP_TOOL: &str = "stop";
/// The tool that merges a finished agent's work into its lead's branch: a harness's task
/// branch into the Master's branch for the master (DEC-100), a worker's work into the
/// lead's own branch for a lead.
pub const MERGE_TOOL: &str = "merge";
pub const REMEMBER_TOOL: &str = "remember";
pub const RECALL_TOOL: &str = "recall";
pub const SESSIONS_TOOL: &str = "sessions";
pub const SESSION_TOOL: &str = "session";
/// The tool an agent logs a new phase of its work or a finished test run with, to the
/// project activity the owner reads on Overview (OS-5).
pub const ACTIVITY_TOOL: &str = "log_activity";
/// The team tools a live master and a sub-agent with tools get (DEC-104), in the order of
/// `coord::tool_switches::BUILTIN_TOOLS`, which the owner switches each one by.
pub const TEAM_TOOLS: [&str; 13] = [
    START_WORK_TOOL,
    INSPECT_WORK_TOOL,
    UPDATE_WORK_TOOL,
    TRANSFER_WORK_TOOL,
    FINISH_WORK_TOOL,
    SPAWN_TOOL,
    STOP_TOOL,
    MERGE_TOOL,
    REMEMBER_TOOL,
    RECALL_TOOL,
    SESSIONS_TOOL,
    SESSION_TOOL,
    ACTIVITY_TOOL,
];
/// The team tools a master without its live terminal gets beside `delegate`, answered by the
/// engine through its mailbox like a live master's.
pub const MEMORY_TOOLS: [&str; 2] = [REMEMBER_TOOL, RECALL_TOOL];
/// The team tools only an agent that leads others uses, left out of the list of a
/// sub-agent the owner's Sub-agent depth setting keeps from leading.
pub const LEAD_TOOLS: [&str; 3] = [SPAWN_TOOL, STOP_TOOL, MERGE_TOOL];
pub const ASK_TOOL: &str = "ask";
pub const RECORD_TOOL: &str = "record_answer";
pub const REQUEST_ACCESS_TOOL: &str = "request_access";
/// The owner's questions tools (DEC-099 A) a server lists after `TEAM_TOOLS`: `ask` logs a
/// question for the owner, `record_answer` records the owner's chat message that answered
/// one of the master's own and `request_access` asks the owner for one narrow extra the
/// sandbox blocks (F08). A sub-agent's server leaves `record_answer` out.
pub const QUESTION_TOOLS: [&str; 3] = [ASK_TOOL, RECORD_TOOL, REQUEST_ACCESS_TOOL];
pub const MAILBOX_FOLDER: &str = "crazyagent-team-calls";
pub const CALLS_PER_TICK_MAX: usize = 8;
pub const CALLS_PENDING_MAX: usize = 16;
pub const REPLY_TEXT_MAX: usize = 32 * 1024;
const CALL_BYTES_MAX: u64 = 16 * 1024;
const REPLY_BYTES_MAX: u64 = 2 * REPLY_TEXT_MAX as u64 + 64;
#[cfg(unix)]
const MAILBOX_SCAN_MAX: usize = 256;
const AGENT_NAME_BYTES_MAX: usize = 64;
const CALL_SUFFIX: &str = ".call";
const TAKEN_SUFFIX: &str = ".taken";
pub(crate) const REPLY_SUFFIX: &str = ".reply";
const PART_SUFFIX: &str = ".part";
pub(crate) const ANSWER_PART_SUFFIX: &str = ".reply-part";
const PANE_KEY: &str = "pane";
const REPLY_WAIT: std::time::Duration = std::time::Duration::from_secs(60);
const REPLY_POLL: std::time::Duration = std::time::Duration::from_millis(50);
const REPLY_STALE: std::time::Duration = std::time::Duration::from_secs(10 * 60);
const SPAWN_INVALID: &str = "Not started: give a task of plain text up to 4000 bytes and, if you choose one, a harness name such as codex or claude-code.";
const MODE_INVALID: &str =
    "Not started: mode is native or app_spawned, or left out for the owner's setting.";
const ROLE_INVALID: &str =
    "Not started: give a short role of letters and digits up to 64 bytes, such as onboarding-fix.";
const START_INVALID: &str = "Not started: give a title of one plain line up to 200 bytes and at most 32 claims, each a file, folder/, glob, module:<name> or domain:<name> of up to 512 bytes.";
const UPDATE_INVALID: &str = "Not sent: give plain text up to 4000 bytes; to names an agent by its name, @role handle or number, or master or lead.";
const QUESTION_INVALID: &str = "Not sent: a question needs for (me, master or lead) and severity (high, normal or low), and no to: its for tag decides who gets it.";
const REPLY_INVALID: &str =
    "Not sent: a reply names the message it answers in reply_to, and no to, for or severity.";
const MESSAGE_INVALID: &str = "Not sent: a message or note names who gets it in to, and carries no for or severity; ask a question with kind question.";
const KEY_INVALID: &str = "Not sent: a key is up to 128 letters, digits, '-', '_', '.' or ':'.";
const TRANSFER_INVALID: &str = "Not changed: give the work number, the version inspect_work shows and an action (assign, accept, decline, release, handoff or reopen); assign and handoff also name the agent in to.";
const FINISH_INVALID: &str = "Not finished: give the work number, the version inspect_work shows, an outcome (review_ready, complete or closed) and evidence of plain text up to 4000 bytes.";
const AGENT_INVALID: &str =
    "Not done: name the agent by its persona name, its @role handle or its number.";
const SESSIONS_INVALID: &str =
    "Not searched: give plain words up to 256 bytes, or nothing to list the conversations.";
const SESSION_INVALID: &str =
    "Not read: give a conversation id from sessions() and a message number from 0.";
const REMEMBER_INVALID: &str =
    "Not saved: give one plain fact of up to 2000 bytes, and a scope of project or app.";
const RECALL_INVALID: &str = "Not searched: give plain words on one line of up to 100 bytes, or none for the newest memories.";
const ACTIVITY_INVALID: &str = "Not logged: give the goal on one plain line of up to 120 bytes, one plain line of up to 600 bytes saying what happened, and a kind of phase or test.";
const CALL_INVALID: &str = "Not done: the engine could not read this call. Call the tool again.";
const SWITCHED_OFF: &str = "Not done: the owner switched this tool off.";
const ASK_INVALID: &str = "Not asked: give one plain question of up to 2048 bytes with no hidden characters. For kind single or multi give 2 to 6 distinct one-line options of up to 200 bytes and one of them as recommended; for kind text give no options. Context may be up to 4096 bytes.";
const RECORD_INVALID: &str = "Not recorded: give the question number, the version you read (0 while it waits) and the number of the owner's chat message that answers it.";
const ASK_KEYS: [&str; 5] = ["question", "kind", "options", "recommended", "context"];
const REQUEST_INVALID: &str = "Not requested: give kind socket with the socket's full path, kind port with one port from 1 to 49151, or kind temp_folder, and a reason in plain words of up to 2048 bytes.";
const REQUEST_KEYS: [&str; 4] = ["kind", "path", "port", "reason"];
const REQUEST_REASON_BYTES_MAX: usize = 2048;
const RECORD_KEYS: [&str; 3] = ["question", "version", "owner_turn"];
const BUSY: &str = "Not sent: 16 team calls already wait for the engine. Try again in a moment.";
const UNSENT: &str = "Not sent: the call could not be written for the engine.";
const NOT_TAKEN: &str =
    "Not sent: the engine did not take the call within a minute, so nothing was done.";
#[cfg(unix)]
const UNNAMED: &str =
    "Not sent: the engine could not tell which agent made the call. Try again in a moment.";
const NOT_ANSWERED: &str = "The engine took the call but did not answer within a minute. Check inspect_work() before trying again.";

const _: () = assert!(REPLY_BYTES_MAX < REQUEST_BYTES_MAX as u64);
const _: () = assert!(CALLS_PER_TICK_MAX <= CALLS_PENDING_MAX);
const _: () = assert!(crate::coord::KEY_BYTES_MAX == 128);
const _: () = {
    let builtin = crate::coord::tool_switches::BUILTIN_TOOLS;
    assert!(builtin.len() == TEAM_TOOLS.len());
    let mut at = 0;
    while at < TEAM_TOOLS.len() {
        assert!(same_name(TEAM_TOOLS[at], builtin[at].0));
        at += 1;
    }
};

/// Compares two tool names byte by byte where a const check needs it. Takes both names.
/// Returns whether they are the same.
const fn same_name(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut at = 0;
    while at < a.len() {
        if a[at] != b[at] {
            return false;
        }
        at += 1;
    }
    true
}
const _: () = assert!(
    2 * (crate::questions::QUESTION_BYTES_MAX
        + crate::questions::OPTIONS_MAX * crate::questions::OPTION_BYTES_MAX
        + crate::questions::RECOMMENDED_BYTES_MAX
        + crate::questions::CONTEXT_BYTES_MAX)
        + 256
        <= CALL_BYTES_MAX as usize
);

/// One team tool call a live master or a sub-agent made (DEC-104), as its server writes it
/// into the mailbox for the engine to answer by the caller's place in the team: the work
/// verbs `start_work`, `inspect_work`, `update_work`, `transfer_work` and `finish_work`,
/// the lead verbs `spawn`, `stop` and `merge`, and `sessions`, `session`, `remember` and
/// `recall` over past conversations and saved memory, and `ask` and `record_answer` for
/// the owner's questions log (DEC-099 A). The engine names the caller from the mailbox it
/// read the call from, never from the call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "tool", rename_all = "snake_case", deny_unknown_fields)]
pub enum Call {
    StartWork {
        title: String,
        claims: Vec<String>,
    },
    InspectWork,
    UpdateWork {
        kind: Update,
        text: String,
        to: Option<String>,
        #[serde(rename = "for")]
        question_for: Option<QuestionFor>,
        severity: Option<Severity>,
        reply_to: Option<i64>,
        work: Option<i64>,
        key: Option<String>,
    },
    TransferWork {
        work: i64,
        version: i64,
        action: Handover,
        to: Option<String>,
    },
    FinishWork {
        work: i64,
        version: i64,
        outcome: Finish,
        evidence: String,
    },
    Spawn {
        task: String,
        harness: Option<String>,
        role: Option<String>,
        mode: Option<crate::coord::settings::SubagentMode>,
    },
    Stop {
        agent: String,
    },
    Merge {
        agent: String,
    },
    Sessions {
        query: String,
    },
    Session {
        id: String,
        offset: u64,
    },
    Remember {
        text: String,
        scope: MemoryReach,
    },
    Recall {
        query: String,
    },
    LogActivity {
        goal: String,
        text: String,
        kind: LogKind,
    },
    Ask {
        question: String,
        kind: QuestionKind,
        options: Vec<String>,
        recommended: Option<String>,
        context: Option<String>,
    },
    RecordAnswer {
        question: u64,
        version: u64,
        owner_turn: i64,
    },
    RequestAccess {
        grant: crate::sandbox::policy::AccessGrant,
        reason: String,
    },
}

impl Call {
    /// The tool this call came from, the name the owner's tool switches use. Takes the
    /// call. Returns one of `TEAM_TOOLS` or `QUESTION_TOOLS`.
    pub fn name(&self) -> &'static str {
        let name = match self {
            Self::StartWork { .. } => START_WORK_TOOL,
            Self::InspectWork => INSPECT_WORK_TOOL,
            Self::UpdateWork { .. } => UPDATE_WORK_TOOL,
            Self::TransferWork { .. } => TRANSFER_WORK_TOOL,
            Self::FinishWork { .. } => FINISH_WORK_TOOL,
            Self::Spawn { .. } => SPAWN_TOOL,
            Self::Stop { .. } => STOP_TOOL,
            Self::Merge { .. } => MERGE_TOOL,
            Self::Sessions { .. } => SESSIONS_TOOL,
            Self::Session { .. } => SESSION_TOOL,
            Self::Remember { .. } => REMEMBER_TOOL,
            Self::Recall { .. } => RECALL_TOOL,
            Self::LogActivity { .. } => ACTIVITY_TOOL,
            Self::Ask { .. } => ASK_TOOL,
            Self::RecordAnswer { .. } => RECORD_TOOL,
            Self::RequestAccess { .. } => REQUEST_ACCESS_TOOL,
        };
        assert!(TEAM_TOOLS.contains(&name) || QUESTION_TOOLS.contains(&name));
        name
    }
}

/// What an `update_work` call sends: a message that asks something of its recipient, a
/// note that asks nothing, a tagged question (Q14b) or a reply to an earlier message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Update {
    Message,
    Note,
    Question,
    Reply,
}

/// The ownership change a `transfer_work` call asks for; assign and handoff also name the
/// agent the work goes to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Handover {
    Assign,
    Accept,
    Decline,
    Release,
    Handoff,
    Reopen,
}

/// What a `log_activity` call logs: a new phase of the work or a finished test run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LogKind {
    Phase,
    Test,
}

/// Where a memory an agent saves applies: its own project, or every project of the app.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryReach {
    Project,
    App,
}

impl MemoryReach {
    fn as_str(self) -> &'static str {
        match self {
            Self::Project => "project",
            Self::App => "app",
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Recorded {
    harness: Option<String>,
    task: String,
    #[serde(default)]
    role: Option<String>,
}

/// Serves the master's `delegate` and `ask` tools as an MCP server over stdio until its client
/// closes input, and with a mailbox also its `MEMORY_TOOLS` less the ones switched off, each
/// answered by the engine through the mailbox. Takes the client's input and output, the absolute
/// drop file in the master's private temporary folder, the mailbox folder the engine made for a
/// team master without its live terminal, and the tools left out. Returns an input or output
/// failure, or a refusal when the mailbox is not a plain folder; a `delegate` or `ask` call only
/// records a proposal or question, which the engine takes after the turn.
pub fn serve(
    input: impl BufRead,
    output: impl Write,
    drop: &Path,
    mailbox: Option<&Path>,
    off: &[String],
) -> std::io::Result<()> {
    if !drop.is_absolute() {
        return Err(std::io::Error::other(
            "the drop file must be an absolute path",
        ));
    }
    if let Some(mailbox) = mailbox
        && (!mailbox.is_absolute()
            || !std::fs::symlink_metadata(mailbox).is_ok_and(|metadata| metadata.is_dir()))
    {
        return Err(std::io::Error::other(
            "the mailbox must be an absolute plain folder",
        ));
    }
    rpc(input, output, |request| answer(request, drop, mailbox, off))
}

/// Serves a live master's or a sub-agent's team tools, `TEAM_TOOLS` less the ones switched
/// off, as an MCP server over stdio until its client closes input: each call goes into the
/// mailbox folder in the agent's private temporary folder, and its result is the engine's
/// answer. A native lead's servers first have the engine name each call by their process
/// (DEC-111 Q18); nothing the server or its environment says names the caller. Takes the
/// client's input and output, the absolute mailbox folder the engine made and the tools
/// left out. Returns an input or output failure, or a refusal when the mailbox is not a
/// plain folder.
pub fn serve_live(
    input: impl BufRead,
    output: impl Write,
    mailbox: &Path,
    off: &[String],
) -> std::io::Result<()> {
    let plain = std::fs::symlink_metadata(mailbox).is_ok_and(|metadata| metadata.is_dir());
    if !mailbox.is_absolute() || !plain {
        return Err(std::io::Error::other(
            "the mailbox must be an absolute plain folder",
        ));
    }
    rpc(input, output, |request| answer_live(request, mailbox, off))
}

/// Says whether every name is one of the team tools, as the engine subcommands check the
/// tools they are told to leave out. Takes the names. Returns the answer.
pub fn known_tools(names: &[String]) -> bool {
    names.len() <= TEAM_TOOLS.len() + QUESTION_TOOLS.len()
        && names.iter().all(|name| {
            TEAM_TOOLS.contains(&name.as_str()) || QUESTION_TOOLS.contains(&name.as_str())
        })
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

fn answer(request: &Value, drop: &Path, mailbox: Option<&Path>, off: &[String]) -> Option<Value> {
    let method = request.get("method")?.as_str();
    let id = request.get("id")?.clone();
    let params = request.get("params");
    let name = params
        .and_then(|params| params.get("name"))
        .and_then(Value::as_str);
    let result = match method {
        Some("initialize") => initialized(params),
        Some("ping") => json!({}),
        Some("tools/list") => {
            let mut tools = vec![json!({
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
            })];
            tools.extend([
                questions::ask_listing(),
                questions::record_listing(),
                questions::request_listing(),
            ]);
            if mailbox.is_some() {
                tools.extend(
                    MEMORY_TOOLS
                        .iter()
                        .filter(|name| !off.iter().any(|tool| tool == *name))
                        .map(|name| spec(name)),
                );
            }
            assert!(tools.len() <= 4 + MEMORY_TOOLS.len());
            json!({ "tools": tools })
        }
        Some("tools/call") => match (mailbox, name) {
            (Some(mailbox), Some(name)) if MEMORY_TOOLS.contains(&name) => {
                match mailed(mailbox, name, params, off) {
                    Some(result) => result,
                    None => return Some(failure(id, -32602, "Unknown tool")),
                }
            }
            _ => match call(params, drop)
                .map(|(text, error)| (text.to_owned(), error))
                .or_else(|| questions::drop_call(params, drop))
            {
                Some((text, error)) => tool_result(&text, error),
                None => return Some(failure(id, -32602, "Unknown tool")),
            },
        },
        _ => return Some(failure(id, -32601, "Method not found")),
    };
    Some(json!({"jsonrpc": "2.0", "id": id, "result": result}))
}

/// Answers one team tool call through the mailbox: refused when switched off or when an
/// argument fails its bound, else written for the engine and answered with its reply. Takes
/// the mailbox, the tool name, the call's parameters and the tools left out. Returns the
/// `tools/call` result, or none for an unknown tool.
fn mailed(mailbox: &Path, name: &str, params: Option<&Value>, off: &[String]) -> Option<Value> {
    if off.iter().any(|tool| tool == name) {
        return Some(tool_result(SWITCHED_OFF, true));
    }
    let arguments = params.and_then(|params| params.get("arguments"));
    Some(match live_call(name, arguments)? {
        Ok(call) => {
            let (text, error) = ask(mailbox, &call, REPLY_WAIT);
            tool_result(&text, error)
        }
        Err(refused) => tool_result(refused, true),
    })
}

/// The team tools a server lists, in `BUILTIN_TOOLS` order, without the ones left out.
/// Takes the tools left out. Returns the MCP `tools/list` result.
fn team_tools(off: &[String]) -> Value {
    let on = |name: &str| !off.iter().any(|tool| tool == name);
    let mut tools: Vec<Value> = TEAM_TOOLS
        .iter()
        .filter(|name| on(name))
        .map(|name| spec(name))
        .collect();
    if on(ASK_TOOL) {
        tools.push(ask_tool());
    }
    if on(RECORD_TOOL) {
        tools.push(record_tool());
    }
    if on(REQUEST_ACCESS_TOOL) {
        tools.push(request_access_tool());
    }
    assert!(tools.len() <= TEAM_TOOLS.len() + QUESTION_TOOLS.len());
    json!({ "tools": tools })
}

/// One team tool's MCP description and input schema. Takes one of `TEAM_TOOLS`. Returns
/// its `tools/list` entry.
fn spec(name: &str) -> Value {
    let agent = json!({"type": "string", "description": "The agent: its persona name, its @role handle or its number."});
    let work = json!({"type": "integer", "minimum": 1, "description": "The work number start_work or inspect_work gave."});
    let version = json!({"type": "integer", "minimum": 1, "description": "The work's version inspect_work shows; a stale one is refused so the first change wins."});
    let none = json!({"type": "object", "properties": {}, "additionalProperties": false});
    let (description, schema) = match name {
        START_WORK_TOOL => (
            "Start a unit of work before you edit, with a short title, and claim what it touches: each claim a file, a folder ending in /, a glob, module:<name> or domain:<name>. The engine tells you and the other agents when a claim overlaps theirs and notes a claim too broad to share. Returns the work number and version.",
            json!({"type": "object", "properties": {
                "title": {"type": "string", "description": "What the work is, on one line of up to 200 bytes."},
                "claims": {"type": "array", "maxItems": 32, "items": {"type": "string"}, "description": "What the work touches, such as src/login.rs, docs/, src/**/*.ts or module:auth."},
            }, "required": ["title"], "additionalProperties": false}),
        ),
        INSPECT_WORK_TOOL => (
            "Read your work with its number and version, the messages and questions waiting for you (reading them here marks them read), what you sent and how far each got, and claims that overlap yours. A lead or the master also reads the agents it leads, their work and the change sets waiting.",
            none,
        ),
        UPDATE_WORK_TOOL => (
            "Send to another agent of this project. kind message asks the agent in to for something, note tells it something, question asks with for (me for the owner, master, or lead) and severity (high, normal or low) and the engine routes it, reply answers a question or a message (work for you) whose number is in reply_to. A note takes no reply: to answer one, send kind note with to its sender. A for me question reaches the owner only. Pass a question on up the chain as a question whose reply_to names it, at the same or a higher severity. A message to a finished agent you lead wakes it with your text.",
            json!({"type": "object", "properties": {
                "text": {"type": "string", "description": "What you send, in plain words, up to 4000 bytes."},
                "kind": {"type": "string", "enum": ["message", "note", "question", "reply"], "description": "message when left out, question with for, reply with reply_to to a question or work for you, never to a note."},
                "to": {"type": "string", "description": "For a message or note: the agent's persona name, @role handle or number, or master, or lead for your own lead."},
                "for": {"type": "string", "enum": ["me", "master", "lead"], "description": "For a question: me for the owner, master, or lead for your own lead."},
                "severity": {"type": "string", "enum": ["high", "normal", "low"], "description": "For a question: how urgent it is. It never changes who gets it."},
                "reply_to": {"type": "integer", "minimum": 1, "description": "The question or message (work for you) this answers or passes on, or the message a note or message refers to."},
                "work": {"type": "integer", "minimum": 1, "description": "The work number this is about."},
                "key": {"type": "string", "description": "Optional key so a retried send is stored once."},
            }, "required": ["text"], "additionalProperties": false}),
        ),
        TRANSFER_WORK_TOOL => (
            "Change who owns a unit of work: assign or handoff it to the agent in to, accept or decline work assigned to you, release your own, or reopen finished work. The master and your lead may assign and reopen your work.",
            json!({"type": "object", "properties": {
                "work": work,
                "version": version,
                "action": {"type": "string", "enum": ["assign", "accept", "decline", "release", "handoff", "reopen"]},
                "to": agent,
            }, "required": ["work", "version", "action"], "additionalProperties": false}),
        ),
        FINISH_WORK_TOOL => (
            "Finish a unit of work with evidence, such as the commands you ran and what they showed. review_ready asks your lead or the master to check it; complete and closed end it and keep its claims until the change set carrying the work lands, or its turn ends with nothing to land. A lead cannot complete its work while a teammate's or sub-agent's branch is not merged into its own; this names them.",
            json!({"type": "object", "properties": {
                "work": work,
                "version": version,
                "outcome": {"type": "string", "enum": ["review_ready", "complete", "closed"]},
                "evidence": {"type": "string", "description": "What shows the work is done, in plain words, up to 4000 bytes."},
            }, "required": ["work", "version", "outcome", "evidence"], "additionalProperties": false}),
        ),
        SPAWN_TOOL => (
            "Start an agent for one task in its own worktree and branch: the master starts a lead, and a lead starts a sub-agent of its own within the owner's Sub-agent depth setting. The engine tells you when it ends.",
            json!({"type": "object", "properties": {
                "task": {"type": "string", "description": "What the agent should do, in plain words."},
                "harness": {"type": "string", "description": "Optional harness, such as codex or claude-code."},
                "role": {"type": "string", "description": "Optional short role such as onboarding-fix; the agent is then @onboarding-fix."},
                "mode": {"type": "string", "enum": ["native", "app_spawned"], "description": "Master only, fixed for the lead's life: native (its sub-agents are its own Claude Code teammates) or app_spawned (it starts them with spawn); left out, the owner's setting for that harness."},
            }, "required": ["task"], "additionalProperties": false}),
        ),
        STOP_TOOL => (
            "Stop an agent you lead: the master may stop any agent, a lead only its own sub-agents. Its unfinished tasks fail, and you or anyone above you can wake it again.",
            json!({"type": "object", "properties": {"agent": agent}, "required": ["agent"], "additionalProperties": false}),
        ),
        MERGE_TOOL => (
            "Merge a finished agent's work locally, never pushed: the master merges a lead's task branch into the Master's branch, and a lead merges one of its own sub-agents' work into its own branch. The owner's settings may keep it for the owner's Accept in Changes instead; then this says so.",
            json!({"type": "object", "properties": {"agent": agent}, "required": ["agent"], "additionalProperties": false}),
        ),
        ACTIVITY_TOOL => (
            "Log to the project activity the owner reads on Overview: a new phase of the work or a finished test run, with the goal it serves and one line saying what happened.",
            json!({"type": "object", "properties": {
                "goal": {"type": "string", "description": "The goal the work serves, such as its work title, on one line of up to 120 bytes."},
                "text": {"type": "string", "description": "One line of up to 600 bytes saying what happened."},
                "kind": {"type": "string", "enum": ["phase", "test"], "description": "phase for a new phase of the work, test for a finished test run."},
            }, "required": ["goal", "text", "kind"], "additionalProperties": false}),
        ),
        SESSIONS_TOOL => (
            "Search every past master and sub-agent conversation of this project, of any age: each hit gives the conversation id, the message number and an excerpt. With no query it lists the conversations, newest first.",
            json!({"type": "object", "properties": {"query": {"type": "string", "description": "Words that must all appear in one message; leave empty to list the conversations."}}, "additionalProperties": false}),
        ),
        SESSION_TOOL => (
            "Read one past conversation of this project from a message number on, one page at a time; the page ends with the number to read next.",
            json!({"type": "object", "properties": {"id": {"type": "string", "description": "The conversation id that sessions gave."}, "offset": {"type": "integer", "minimum": 0, "description": "The message number to start from; 0 when left out."}}, "required": ["id"], "additionalProperties": false}),
        ),
        REMEMBER_TOOL => (
            "Save one fact, decision or lesson that agents should know in later sessions. Scope project keeps it to this project; app shares it with every project. The owner sees it with your name in the Memory tab, can delete it, and may have to approve it first.",
            json!({"type": "object", "properties": {"text": {"type": "string", "description": "The memory in plain words, up to 2000 bytes."}, "scope": {"type": "string", "enum": ["project", "app"], "description": "project (the default) or app."}}, "required": ["text"], "additionalProperties": false}),
        ),
        RECALL_TOOL => (
            "Search the saved memories of this project and the app-wide ones, newest first. Leave the query empty for the newest.",
            json!({"type": "object", "properties": {"query": {"type": "string", "description": "Words the memory holds, on one line of up to 100 bytes."}}, "additionalProperties": false}),
        ),
        other => unreachable!("{other} is not a team tool"),
    };
    json!({"name": name, "description": description, "inputSchema": schema})
}

/// The MCP entry of the `record_answer` tool, the same for every master that is given it.
/// Output: the tool's name, description and input schema.
pub fn record_tool() -> Value {
    json!({
        "name": RECORD_TOOL,
        "description": "Record that the owner answered one of your own questions in this chat instead of in the Questions view. Name the owner's message; the engine takes the answer from that message, never from your words, and takes the question off the owner's list.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "question": {"type": "integer", "description": "The question number the ask tool gave you."},
                "version": {"type": "integer", "description": "The version you read: 0 while the question waits."},
                "owner_turn": {"type": "integer", "description": "The number of the owner's chat message that answers it."},
            },
            "required": ["question", "version", "owner_turn"],
            "additionalProperties": false,
        },
    })
}

/// The MCP entry of the `request_access` tool (F08), the same for every master and agent
/// that is given it: one socket, one port on this computer or the owner's temp folder, with
/// the reason the owner reads. Output: the tool's name, description and input schema.
pub fn request_access_tool() -> Value {
    json!({
        "name": REQUEST_ACCESS_TOOL,
        "description": "Ask the owner to allow one narrow extra your sandbox blocks: connecting to one unix socket, connecting to one TCP port on this computer, or using the owner's own temp folder as yours. The engine checks it and raises a question the owner answers in the Questions view; if allowed it joins the project's Agent access and agents restart at their next pause to get it. Use it instead of working around the sandbox. Container, VM, key and password agent sockets are never allowed, nor ports from 49152 up.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "kind": {"type": "string", "enum": ["socket", "port", "temp_folder"], "description": "socket: one unix socket by its full path. port: one TCP port on this computer. temp_folder: the owner's own temp folder."},
                "path": {"type": "string", "description": "For kind socket only: the socket's full path."},
                "port": {"type": "integer", "minimum": 1, "maximum": 49_151, "description": "For kind port only: the port."},
                "reason": {"type": "string", "description": "Why you need it, in one or two plain sentences the owner reads."},
            },
            "required": ["kind", "reason"],
            "additionalProperties": false,
        },
    })
}

/// The MCP entry of the `ask` tool, the same for every master and agent that is given it.
/// Output: the tool's name, description and input schema.
pub fn ask_tool() -> Value {
    json!({
        "name": ASK_TOOL,
        "description": "Ask the owner a question and log it in the project's questions. This is the only way to ask the owner anything, grill-me questions included: one question at a time, with the answer you recommend. Prefer single or multi with a recommended option; use text only when no short list of answers fits. The owner is notified and the tool replies with the question number. The owner's answer or dismissal comes to you once, as a message.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "question": {"type": "string", "description": "One plain question for the owner."},
                "kind": {"type": "string", "enum": ["single", "multi", "text"], "description": "single: the owner picks one option. multi: the owner picks any. text: only when the answer is genuinely free text. single and multi need 2 to 6 options and one recommended option; the owner can always add a comment."},
                "options": {"type": "array", "items": {"type": "string"}, "maxItems": crate::questions::OPTIONS_MAX, "description": "2 to 6 short one-line answers the owner can pick from, for kind single or multi."},
                "recommended": {"type": "string", "description": "The answer you recommend: one of the options, or a short answer for kind text."},
                "context": {"type": "string", "description": "What the owner needs to know to answer."},
            },
            "required": ["question"],
            "additionalProperties": false,
        },
    })
}

fn answer_live(request: &Value, mailbox: &Path, off: &[String]) -> Option<Value> {
    let method = request.get("method")?.as_str();
    let id = request.get("id")?.clone();
    let params = request.get("params");
    let result = match method {
        Some("initialize") => initialized(params),
        Some("ping") => json!({}),
        Some("tools/list") => team_tools(off),
        Some("tools/call") => {
            let name = params
                .and_then(|params| params.get("name"))
                .and_then(Value::as_str);
            match name.and_then(|name| mailed(mailbox, name, params, off)) {
                Some(result) => result,
                None => return Some(failure(id, -32602, "Unknown tool")),
            }
        }
        _ => return Some(failure(id, -32601, "Method not found")),
    };
    Some(json!({"jsonrpc": "2.0", "id": id, "result": result}))
}

/// Reads one team tool call from its MCP name and arguments, checking every bound the
/// engine checks again. Takes the tool name and arguments. Returns none for an unknown
/// tool, or the call, or the plain sentence the agent reads when an argument is refused.
pub fn live_call(name: &str, arguments: Option<&Value>) -> Option<Result<Call, &'static str>> {
    let argument = |key: &str| {
        arguments
            .and_then(|arguments| arguments.get(key))
            .and_then(Value::as_str)
    };
    let number = |key: &str| match arguments.and_then(|arguments| arguments.get(key)) {
        None | Some(Value::Null) => Some(None),
        Some(value) => value
            .as_i64()
            .or_else(|| value.as_str().and_then(|text| text.trim().parse().ok()))
            .filter(|number| *number > 0)
            .map(Some),
    };
    Some(match name {
        START_WORK_TOOL => {
            let claims = match arguments.and_then(|arguments| arguments.get("claims")) {
                None | Some(Value::Null) => Some(Vec::new()),
                Some(Value::String(one)) => claim(one).map(|one| vec![one]),
                Some(Value::Array(items)) if items.len() <= crate::coord::CLAIMS_PER_WORK_MAX => {
                    items
                        .iter()
                        .map(|item| item.as_str().and_then(claim))
                        .collect()
                }
                Some(_) => None,
            };
            match (argument("title").map(str::trim), claims) {
                (Some(title), Some(claims))
                    if super::text_valid(title, crate::coord::TITLE_BYTES_MAX)
                        && !title.contains(|c: char| super::events::line_break(c) || c == '\t') =>
                {
                    Ok(Call::StartWork {
                        title: title.to_owned(),
                        claims,
                    })
                }
                _ => Err(START_INVALID),
            }
        }
        INSPECT_WORK_TOOL => Ok(Call::InspectWork),
        UPDATE_WORK_TOOL => {
            let to = match argument("to").map(str::trim) {
                None | Some("") => Some(None),
                Some(to) if agent_name_valid(to) => Some(Some(to.to_owned())),
                Some(_) => None,
            };
            let key = match argument("key").map(str::trim) {
                None | Some("") => None,
                Some(key) if key_valid(key) => Some(key.to_owned()),
                Some(_) => return Some(Err(KEY_INVALID)),
            };
            let (
                Some(text),
                Some(to),
                Some(question_for),
                Some(severity),
                Some(reply_to),
                Some(work),
                Some(kind),
            ) = (
                argument("text")
                    .filter(|text| super::text_valid(text, crate::coord::BODY_BYTES_MAX)),
                to,
                named::<QuestionFor>(arguments, "for"),
                named::<Severity>(arguments, "severity"),
                number("reply_to"),
                number("work"),
                named::<Update>(arguments, "kind"),
            )
            else {
                return Some(Err(UPDATE_INVALID));
            };
            let kind = kind.unwrap_or(match (question_for, reply_to) {
                (Some(_), _) => Update::Question,
                (None, Some(_)) => Update::Reply,
                (None, None) => Update::Message,
            });
            let tagged = question_for.is_some() || severity.is_some();
            let refused = match kind {
                Update::Question
                    if question_for.is_none() || severity.is_none() || to.is_some() =>
                {
                    Some(QUESTION_INVALID)
                }
                Update::Reply if reply_to.is_none() || to.is_some() || tagged => {
                    Some(REPLY_INVALID)
                }
                Update::Message | Update::Note if to.is_none() || tagged => Some(MESSAGE_INVALID),
                _ => None,
            };
            if let Some(refused) = refused {
                return Some(Err(refused));
            }
            Ok(Call::UpdateWork {
                kind,
                text: text.trim().to_owned(),
                to,
                question_for,
                severity,
                reply_to,
                work,
                key,
            })
        }
        TRANSFER_WORK_TOOL => {
            let to = match argument("to").map(str::trim) {
                None | Some("") => Some(None),
                Some(to) if agent_name_valid(to) => Some(Some(to.to_owned())),
                Some(_) => None,
            };
            match (
                number("work"),
                number("version"),
                named::<Handover>(arguments, "action"),
                to,
            ) {
                (Some(Some(work)), Some(Some(version)), Some(Some(action)), Some(to))
                    if matches!(action, Handover::Assign | Handover::Handoff) == to.is_some() =>
                {
                    Ok(Call::TransferWork {
                        work,
                        version,
                        action,
                        to,
                    })
                }
                _ => Err(TRANSFER_INVALID),
            }
        }
        FINISH_WORK_TOOL => match (
            number("work"),
            number("version"),
            named::<Finish>(arguments, "outcome"),
            argument("evidence"),
        ) {
            (Some(Some(work)), Some(Some(version)), Some(Some(outcome)), Some(evidence))
                if super::text_valid(evidence, crate::coord::BODY_BYTES_MAX) =>
            {
                Ok(Call::FinishWork {
                    work,
                    version,
                    outcome,
                    evidence: evidence.trim().to_owned(),
                })
            }
            _ => Err(FINISH_INVALID),
        },
        SPAWN_TOOL => {
            let Some(proposed) = delegation(
                argument("harness").unwrap_or_default(),
                argument("task").unwrap_or_default(),
            ) else {
                return Some(Err(SPAWN_INVALID));
            };
            let role = match argument("role").map(role_handle) {
                Some(Err(())) => return Some(Err(ROLE_INVALID)),
                Some(Ok(role)) => role,
                None => None,
            };
            let Some(mode) = named(arguments, "mode") else {
                return Some(Err(MODE_INVALID));
            };
            Ok(Call::Spawn {
                task: proposed.prompt,
                harness: proposed.harness,
                role,
                mode,
            })
        }
        STOP_TOOL | MERGE_TOOL => match argument("agent") {
            Some(agent) if agent_name_valid(agent) => {
                let agent = agent.trim().to_owned();
                Ok(if name == STOP_TOOL {
                    Call::Stop { agent }
                } else {
                    Call::Merge { agent }
                })
            }
            _ => Err(AGENT_INVALID),
        },
        SESSIONS_TOOL => match argument("query").unwrap_or_default() {
            query
                if query.len() <= super::history::QUERY_BYTES_MAX
                    && !query.chars().any(char::is_control) =>
            {
                Ok(Call::Sessions {
                    query: query.trim().to_owned(),
                })
            }
            _ => Err(SESSIONS_INVALID),
        },
        SESSION_TOOL => {
            let offset = match arguments.and_then(|arguments| arguments.get("offset")) {
                None | Some(Value::Null) => Some(0),
                Some(value) => value
                    .as_u64()
                    .or_else(|| value.as_str().and_then(|text| text.trim().parse().ok())),
            };
            match (argument("id").map(str::trim), offset) {
                (Some(id), Some(offset))
                    if super::history::id_valid(id) && offset <= super::history::OFFSET_MAX =>
                {
                    Ok(Call::Session {
                        id: id.to_owned(),
                        offset,
                    })
                }
                _ => Err(SESSION_INVALID),
            }
        }
        REMEMBER_TOOL => {
            let scope = match argument("scope").map(str::trim) {
                None | Some("" | "project") => Some(MemoryReach::Project),
                Some("app") => Some(MemoryReach::App),
                Some(_) => None,
            };
            match (argument("text"), scope) {
                (Some(text), Some(scope))
                    if super::text_valid(text, crate::memory::library::NOTE_BYTES_MAX) =>
                {
                    Ok(Call::Remember {
                        text: text.trim().to_owned(),
                        scope,
                    })
                }
                _ => Err(REMEMBER_INVALID),
            }
        }
        RECALL_TOOL => match argument("query").unwrap_or_default() {
            query
                if query.len() <= crate::memory::library::SEARCH_BYTES_MAX
                    && !query.chars().any(char::is_control) =>
            {
                Ok(Call::Recall {
                    query: query.trim().to_owned(),
                })
            }
            _ => Err(RECALL_INVALID),
        },
        ACTIVITY_TOOL => {
            let line = |key: &str, max: usize| {
                argument(key)
                    .filter(|line| {
                        !line.trim().is_empty()
                            && line.len() <= max
                            && !line.chars().any(char::is_control)
                    })
                    .map(|line| line.trim().to_owned())
            };
            let kind = match argument("kind").map(str::trim) {
                Some("phase") => Some(LogKind::Phase),
                Some("test") => Some(LogKind::Test),
                _ => None,
            };
            match (
                line("goal", crate::activity::GOAL_BYTES_MAX),
                line("text", crate::activity::TEXT_BYTES_MAX),
                kind,
            ) {
                (Some(goal), Some(text), Some(kind)) => Ok(Call::LogActivity { goal, text, kind }),
                _ => Err(ACTIVITY_INVALID),
            }
        }
        ASK_TOOL => ask_call(arguments).ok_or(ASK_INVALID),
        RECORD_TOOL => record_call(arguments).ok_or(RECORD_INVALID),
        REQUEST_ACCESS_TOOL => request_call(arguments).ok_or(REQUEST_INVALID),
        _ => return None,
    })
}

/// Reads one `ask` call: a question with its kind, options, recommendation and context,
/// each checked as the questions log checks it. A call without a kind is single when it has
/// options and text when it has none. Takes the arguments. Returns the call, or none when a
/// part is missing, unknown or outside its bounds.
fn ask_call(arguments: Option<&Value>) -> Option<Call> {
    let arguments = arguments?.as_object()?;
    if arguments
        .keys()
        .any(|key| !ASK_KEYS.contains(&key.as_str()))
    {
        return None;
    }
    let text = |key: &str| match arguments.get(key) {
        None | Some(Value::Null) => Some(None),
        Some(Value::String(text)) => Some(Some(text.clone())),
        Some(_) => None,
    };
    let options = match arguments.get("options") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(items)) if items.len() <= crate::questions::OPTIONS_MAX => items
            .iter()
            .map(|item| item.as_str().map(str::to_owned))
            .collect::<Option<_>>()?,
        Some(_) => return None,
    };
    let kind = match arguments.get("kind") {
        None | Some(Value::Null) => QuestionKind::stored("", options.len())?,
        Some(Value::String(name)) if !name.is_empty() => QuestionKind::stored(name, options.len())?,
        Some(_) => return None,
    };
    let ask = crate::questions::Ask {
        question: text("question")??,
        kind,
        options,
        recommended: text("recommended")?,
        context: text("context")?,
    }
    .checked()
    .ok()?;
    Some(Call::Ask {
        question: ask.question,
        kind: ask.kind,
        options: ask.options,
        recommended: ask.recommended,
        context: ask.context,
    })
}

/// Reads one `request_access` call: a kind with only the field it needs, a plain grant and
/// a reason of up to `REQUEST_REASON_BYTES_MAX` bytes; the engine checks the grant itself
/// and plains the reason when it raises the question. Takes the arguments. Returns the
/// call, or none when a part is missing, extra or outside its bounds.
fn request_call(arguments: Option<&Value>) -> Option<Call> {
    use crate::sandbox::policy::AccessGrant;
    let arguments = arguments?.as_object()?;
    if arguments
        .keys()
        .any(|key| !REQUEST_KEYS.contains(&key.as_str()))
    {
        return None;
    }
    let reason = arguments.get("reason")?.as_str()?;
    if reason.trim().is_empty() || reason.len() > REQUEST_REASON_BYTES_MAX {
        return None;
    }
    let has = |key: &str| arguments.contains_key(key);
    let grant = match arguments.get("kind")?.as_str()? {
        "socket" if !has("port") => AccessGrant::Socket {
            path: std::path::PathBuf::from(arguments.get("path")?.as_str()?),
        },
        "port" if !has("path") => AccessGrant::Port {
            port: u16::try_from(arguments.get("port")?.as_u64()?).ok()?,
        },
        "temp_folder" if !has("path") && !has("port") => AccessGrant::TempFolder,
        _ => return None,
    };
    if !grant.plain() {
        return None;
    }
    Some(Call::RequestAccess {
        grant,
        reason: reason.to_owned(),
    })
}

/// Reads one `record_answer` call: the question number, the version the master read and
/// the owner's chat message number. Takes the arguments. Returns the call, or none when a
/// part is missing, unknown or not a number in range.
fn record_call(arguments: Option<&Value>) -> Option<Call> {
    let arguments = arguments?.as_object()?;
    if arguments
        .keys()
        .any(|key| !RECORD_KEYS.contains(&key.as_str()))
    {
        return None;
    }
    let number = |key: &str| arguments.get(key).and_then(Value::as_u64);
    let question = number("question").filter(|id| *id > 0)?;
    let version = number("version")?;
    let owner_turn = number("owner_turn")
        .and_then(|turn| i64::try_from(turn).ok())
        .filter(|turn| *turn > 0)?;
    Some(Call::RecordAnswer {
        question,
        version,
        owner_turn,
    })
}

/// Checks a call the engine read from a mailbox, which the agent's side could have written.
/// Takes the call. Returns it, or the plain sentence it was refused with.
pub fn checked(call: Call) -> Result<Call, &'static str> {
    let arguments = match &call {
        Call::StartWork { title, claims } => json!({"title": title, "claims": claims}),
        Call::InspectWork => json!({}),
        Call::UpdateWork {
            kind,
            text,
            to,
            question_for,
            severity,
            reply_to,
            work,
            key,
        } => json!({
            "kind": kind, "text": text, "to": to, "for": question_for, "severity": severity,
            "reply_to": reply_to, "work": work, "key": key,
        }),
        Call::TransferWork {
            work,
            version,
            action,
            to,
        } => json!({"work": work, "version": version, "action": action, "to": to}),
        Call::FinishWork {
            work,
            version,
            outcome,
            evidence,
        } => json!({"work": work, "version": version, "outcome": outcome, "evidence": evidence}),
        Call::Spawn {
            task,
            harness,
            role,
            mode,
        } => json!({"task": task, "harness": harness, "role": role, "mode": mode}),
        Call::Stop { agent } | Call::Merge { agent } => json!({"agent": agent}),
        Call::Sessions { query } => json!({"query": query}),
        Call::Session { id, offset } => json!({"id": id, "offset": offset}),
        Call::Remember { text, scope } => json!({"text": text, "scope": scope.as_str()}),
        Call::Recall { query } => json!({"query": query}),
        Call::LogActivity { goal, text, kind } => {
            json!({"goal": goal, "text": text, "kind": kind})
        }
        Call::Ask {
            question,
            kind,
            options,
            recommended,
            context,
        } => {
            json!({"question": question, "kind": kind.as_str(), "options": options, "recommended": recommended, "context": context})
        }
        Call::RecordAnswer {
            question,
            version,
            owner_turn,
        } => json!({"question": question, "version": version, "owner_turn": owner_turn}),
        Call::RequestAccess { grant, reason } => {
            let mut arguments = serde_json::to_value(grant).map_err(|_| CALL_INVALID)?;
            arguments["reason"] = json!(reason);
            arguments
        }
    };
    let again = live_call(call.name(), Some(&arguments)).expect("every call names a team tool")?;
    if again != call {
        return Err(CALL_INVALID);
    }
    Ok(call)
}

/// Reads one of a call's fixed choices, such as a kind, tag, action or outcome, in any
/// case and with `-` for `_`. Takes the arguments and the key. Returns None for a choice
/// the tool does not offer, else the choice, None when left out or blank.
fn named<T: serde::de::DeserializeOwned>(
    arguments: Option<&Value>,
    key: &str,
) -> Option<Option<T>> {
    match arguments.and_then(|arguments| arguments.get(key)) {
        None | Some(Value::Null) => Some(None),
        Some(Value::String(text)) if text.trim().is_empty() => Some(None),
        Some(Value::String(text)) => serde_json::from_value(Value::String(
            text.trim().to_ascii_lowercase().replace('-', "_"),
        ))
        .ok()
        .map(Some),
        Some(_) => None,
    }
}

/// Reads one claim of `start_work` before the engine normalizes it: plain text on one
/// line of up to `PATTERN_BYTES_MAX` bytes. Takes the text. Returns it trimmed, or None.
fn claim(text: &str) -> Option<String> {
    let text = text.trim();
    (!text.is_empty()
        && text.len() <= crate::coord::PATTERN_BYTES_MAX
        && !text.chars().any(char::is_control))
    .then(|| text.to_owned())
}

/// Says whether an idempotency key is 1 to `KEY_BYTES_MAX` letters, digits, `-`, `_`, `.`
/// or `:`, as the coordination store keeps it. Takes the key. Returns the answer.
fn key_valid(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= crate::coord::KEY_BYTES_MAX
        && key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b':'))
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
/// claimed is reported as taken but unanswered. Before writing, a native lead's server has
/// the engine name the call by the call id and the digest of its bytes, and sends nothing
/// the engine did not name. Takes the mailbox, the call and the wait. Returns the answer
/// text and whether it is an error.
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
    #[cfg(unix)]
    {
        if !super::caller::name_call(mailbox, &id, &call_digest(&line)) {
            return (UNNAMED.into(), true);
        }
    }
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

pub(crate) fn call_id() -> std::io::Result<String> {
    let mut bytes = [0_u8; 16];
    getrandom::fill(&mut bytes).map_err(|_| std::io::Error::other("no random bytes"))?;
    let id: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    assert!(call_id_valid(&id));
    Ok(id)
}

pub(crate) fn call_id_valid(id: &str) -> bool {
    id.len() == 32
        && id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Says whether a mailbox holds a call the engine has not taken yet, so a master turn asks
/// storage to answer only when there is something to answer. A link at the mailbox's own
/// name is never followed. Takes the mailbox. Returns the answer.
pub fn calls_waiting(mailbox: &Path) -> bool {
    !calls(mailbox, CALL_SUFFIX).is_empty()
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
pub(crate) struct Mailbox(rustix::fd::OwnedFd);

/// A live master's mailbox where no directory-relative no-follow open exists; it cannot be
/// opened, so every use is unreachable by construction.
#[cfg(not(unix))]
pub(crate) enum Mailbox {}

#[cfg(unix)]
impl Mailbox {
    fn folder_flags() -> OFlags {
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK
    }

    /// Opens a mailbox folder, refusing a link at its own name. Takes its path. Returns
    /// the handle or the I/O error.
    pub(crate) fn open(path: &Path) -> std::io::Result<Self> {
        Ok(Self(rustix::fs::open(
            path,
            Self::folder_flags(),
            Mode::empty(),
        )?))
    }

    /// Opens a live master's mailbox for the engine from the folder above the master's
    /// temporary folder, refusing a link at the temporary folder or the mailbox: the master
    /// can replace both. Takes the mailbox path. Returns the handle or the I/O error.
    pub(crate) fn open_for_engine(path: &Path) -> std::io::Result<Self> {
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

    /// Opens a folder inside the mailbox, refusing a link or anything but a folder at its
    /// name. Takes its name. Returns the handle or the I/O error.
    pub(crate) fn folder(&self, name: &str) -> std::io::Result<Self> {
        Ok(Self(rustix::fs::openat(
            &self.0,
            name,
            Self::folder_flags(),
            Mode::empty(),
        )?))
    }

    /// Makes a folder inside the mailbox that only this user can enter, keeping whatever
    /// already sits at its name, which [`Self::folder`] then refuses unless it is a folder.
    /// Takes its name. Returns the I/O error.
    pub(crate) fn make_folder(&self, name: &str) -> std::io::Result<()> {
        match rustix::fs::mkdirat(&self.0, name, Mode::RWXU) {
            Ok(()) | Err(rustix::io::Errno::EXIST) => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    /// Lists the entries with one suffix whose names carry a valid call id and that are
    /// plain files, oldest first, never more than `MAILBOX_SCAN_MAX` entries looked at.
    pub(crate) fn entries(&self, suffix: &str) -> Vec<(std::time::SystemTime, String)> {
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
    pub(crate) fn create(&self, name: &str) -> std::io::Result<File> {
        Ok(File::from(rustix::fs::openat(
            &self.0,
            name,
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::RUSR | Mode::WUSR,
        )?))
    }

    /// Opens an entry to append to, creating it 0600 when missing, without following a link
    /// or blocking on a FIFO. Takes its name. Returns the open file or the I/O error.
    pub(crate) fn append(&self, name: &str) -> std::io::Result<File> {
        Ok(File::from(rustix::fs::openat(
            &self.0,
            name,
            OFlags::WRONLY
                | OFlags::CREATE
                | OFlags::APPEND
                | OFlags::NOFOLLOW
                | OFlags::NONBLOCK
                | OFlags::CLOEXEC,
            Mode::RUSR | Mode::WUSR,
        )?))
    }

    /// Opens an entry to read without following a link or blocking on a FIFO. Takes its
    /// name. Returns the open file or the I/O error.
    pub(crate) fn read(&self, name: &str) -> std::io::Result<File> {
        Ok(File::from(rustix::fs::openat(
            &self.0,
            name,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::empty(),
        )?))
    }

    /// Renames an entry within the mailbox; a link is moved, never followed. Takes both
    /// names. Returns the I/O error.
    pub(crate) fn rename(&self, from: &str, to: &str) -> std::io::Result<()> {
        rustix::fs::renameat(&self.0, from, &self.0, to).map_err(std::io::Error::from)
    }

    /// Removes an entry; a link is removed, never followed. Takes its name. Returns the
    /// I/O error.
    pub(crate) fn remove(&self, name: &str) -> std::io::Result<()> {
        rustix::fs::unlinkat(&self.0, name, AtFlags::empty()).map_err(std::io::Error::from)
    }
}

/// Refuses a mailbox where no directory-relative no-follow open exists: both opens return
/// an `Unsupported` error reading `team_mailbox_unavailable`, and the other operations
/// cannot be reached.
#[cfg(not(unix))]
impl Mailbox {
    pub(crate) fn open(_path: &Path) -> std::io::Result<Self> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "team_mailbox_unavailable",
        ))
    }

    pub(crate) fn open_for_engine(path: &Path) -> std::io::Result<Self> {
        Self::open(path)
    }

    pub(crate) fn folder(&self, _name: &str) -> std::io::Result<Self> {
        match *self {}
    }

    pub(crate) fn make_folder(&self, _name: &str) -> std::io::Result<()> {
        match *self {}
    }

    pub(crate) fn entries(&self, _suffix: &str) -> Vec<(std::time::SystemTime, String)> {
        match *self {}
    }

    pub(crate) fn create(&self, _name: &str) -> std::io::Result<File> {
        match *self {}
    }

    pub(crate) fn append(&self, _name: &str) -> std::io::Result<File> {
        match *self {}
    }

    pub(crate) fn read(&self, _name: &str) -> std::io::Result<File> {
        match *self {}
    }

    pub(crate) fn rename(&self, _from: &str, _to: &str) -> std::io::Result<()> {
        match *self {}
    }

    pub(crate) fn remove(&self, _name: &str) -> std::io::Result<()> {
        match *self {}
    }
}

/// Reads an entry's last change time from its status, the epoch when it does not fit.
/// Takes the status. Returns the time.
#[cfg(unix)]
pub(super) fn modified(stat: &rustix::fs::Stat) -> std::time::SystemTime {
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
/// none when the file did not hold a valid call, and the digest of the bytes read, which
/// the engine checks against the name it gave the call.
pub fn take_calls(
    mailbox: &Path,
    max: usize,
) -> Vec<(String, Option<Call>, Option<[u8; CALL_DIGEST_BYTES]>)> {
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
            Some((read_call(bytes.trim_ascii())?, call_digest(&bytes)))
        });
        let _ = mailbox.remove(&claimed);
        taken.push(match call {
            Some((call, digest)) => (id, Some(call), Some(digest)),
            None => (id, None, None),
        });
    }
    assert!(taken.len() <= max);
    taken
}

/// Reads one call file. A pane an older server wrote beside the call is dropped unread:
/// the engine names a caller only by its process (DEC-111 Q18). Takes the bytes. Returns
/// the call, or none for anything else.
fn read_call(bytes: &[u8]) -> Option<Call> {
    let mut value: Value = serde_json::from_slice(bytes).ok()?;
    value.as_object_mut()?.remove(PANE_KEY);
    serde_json::from_value::<Call>(value).ok()
}

/// The bytes of a call digest: SHA-256.
pub const CALL_DIGEST_BYTES: usize = 32;

/// The SHA-256 of a call file's exact bytes, which binds the engine's name for a call to
/// the call it names. Takes the bytes. Returns the digest.
pub fn call_digest(bytes: &[u8]) -> [u8; CALL_DIGEST_BYTES] {
    use sha2::Digest as _;
    sha2::Sha256::digest(bytes).into()
}

/// Answers one call in the mailbox: the answer is written beside it under a part name no
/// agent of a shared mailbox can make, and renamed into place, so the server never reads
/// half an answer. Takes the mailbox, the
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
    let part = format!("{id}{ANSWER_PART_SUFFIX}");
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
/// Takes the drop file in the master's private temporary folder; a link or other non-file is never
/// read, nor a file reached through a link the master put in place of that folder.
/// Returns at most eight valid proposals in call order, none when there is no record.
pub fn take(drop: &Path) -> Vec<Delegation> {
    let opened = open_taken(drop);
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
/// Takes the drop file path; a link there is removed, never followed, and nothing is removed
/// through a link the master put in place of its private temporary folder.
/// Returns nothing; a missing file is already clear.
pub fn clear(drop: &Path) {
    remove_drop(drop);
}

/// Opens the master's private temporary folder that holds its drop file, for the engine,
/// refusing a link at that folder or at the agent folder above it, which the master can
/// replace. Takes the drop file path. Returns the folder handle and the file's name.
#[cfg(unix)]
fn drop_folder(drop: &Path) -> std::io::Result<(Mailbox, &str)> {
    let refused = || std::io::Error::other("the drop file is not inside a folder");
    let name = drop
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(refused)?;
    let folder = drop.parent().ok_or_else(refused)?;
    Ok((Mailbox::open_for_engine(folder)?, name))
}

/// Opens the drop file for the engine through its folder's handle, without following a link
/// or blocking on a FIFO. Takes its path. Returns the open file or the I/O error.
#[cfg(unix)]
fn open_taken(drop: &Path) -> std::io::Result<File> {
    let (folder, name) = drop_folder(drop)?;
    folder.read(name)
}

/// Opens the drop file for the engine as the reparse point itself, never a link's target.
/// Takes its path. Returns the open file or the I/O error.
#[cfg(windows)]
fn open_taken(drop: &Path) -> std::io::Result<File> {
    open_recorded(drop)
}

/// Removes the drop file through its folder's handle; a link there is removed, never
/// followed, and a folder of that name stays. Takes its path.
#[cfg(unix)]
fn remove_drop(drop: &Path) {
    if let Ok((folder, name)) = drop_folder(drop) {
        let _ = folder.remove(name);
    }
}

/// Removes the drop file by its path unless a folder is there; a link is removed, never
/// followed. Takes its path.
#[cfg(windows)]
fn remove_drop(drop: &Path) {
    if std::fs::symlink_metadata(drop).is_ok_and(|metadata| !metadata.is_dir()) {
        let _ = std::fs::remove_file(drop);
    }
}

pub mod questions;

#[cfg(all(test, unix))]
#[path = "tool/tests.rs"]
mod tests;
