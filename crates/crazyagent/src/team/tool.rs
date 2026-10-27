use super::{DELEGATIONS_PER_REPLY_MAX, Delegation, TASK_PROMPT_BYTES_MAX, delegation, names};
#[cfg(unix)]
use rustix::fs::{Mode, OFlags};
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
pub const MAILBOX_FOLDER: &str = "crazyagent-team-calls";
pub const CALLS_PER_TICK_MAX: usize = 8;
pub const CALLS_PENDING_MAX: usize = 16;
pub const REPLY_TEXT_MAX: usize = 32 * 1024;
const CALL_BYTES_MAX: u64 = 16 * 1024;
const REPLY_BYTES_MAX: u64 = 2 * REPLY_TEXT_MAX as u64 + 64;
const MAILBOX_SCAN_MAX: usize = 256;
const AGENT_NAME_BYTES_MAX: usize = 64;
const ROLE_BYTES_MAX: usize = 40;
const CALL_SUFFIX: &str = ".call";
const REPLY_SUFFIX: &str = ".reply";
const PART_SUFFIX: &str = ".part";
const REPLY_WAIT: std::time::Duration = std::time::Duration::from_secs(60);
const REPLY_POLL: std::time::Duration = std::time::Duration::from_millis(50);
const REPLY_STALE: std::time::Duration = std::time::Duration::from_secs(10 * 60);
const LIVE_INVALID: &str = "Not handed off: give a task of plain text up to 4000 bytes and, if you choose one, a harness name from the team plan.";
const ROLE_INVALID: &str = "Not handed off: a role is 2 to 40 lowercase letters, digits and hyphens, such as @onboarding-fix.";
const TYPED_INVALID: &str = "Not sent: name the sub-agent and give plain text up to 4000 bytes.";
const BUSY: &str = "Not sent: 16 team calls already wait for the engine. Try again in a moment.";
const UNSENT: &str = "Not sent: the call could not be written for the engine.";
const NOT_TAKEN: &str =
    "Not sent: the engine did not take the call within a minute, so nothing was done.";
const NOT_ANSWERED: &str = "The engine took the call but did not answer within a minute. Check status() before trying again.";

const _: () = assert!(REPLY_BYTES_MAX < REQUEST_BYTES_MAX as u64);
const _: () = assert!(CALLS_PER_TICK_MAX <= CALLS_PENDING_MAX);

/// One team tool call a live master made, as its server writes it into the mailbox for the
/// engine to answer: `delegate` admits a task now, `message` and `answer` type into a
/// sub-agent's terminal, `status` and `changes` read the team and its pending change sets.
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
                    let (text, error) = ask(mailbox, &call);
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
                Some(None) => return Some(Err(ROLE_INVALID)),
                Some(Some(role)) => Some(role),
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
        _ => return None,
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
    };
    let again = live_call(name, Some(&arguments)).expect("every call names a live tool")?;
    if again != call {
        return Err(LIVE_INVALID);
    }
    Ok(call)
}

fn agent_name_valid(agent: &str) -> bool {
    let agent = agent.trim();
    !agent.is_empty() && agent.len() <= AGENT_NAME_BYTES_MAX && !agent.chars().any(char::is_control)
}

/// Reads a role handle as a master wrote it, `@onboarding-fix` or `onboarding-fix`.
/// Takes the text. Returns the handle with its `@`, or none when it is not 2 to 40
/// lowercase letters, digits and single inner hyphens.
pub fn role_handle(text: &str) -> Option<String> {
    let bare = text.trim().strip_prefix('@').unwrap_or(text.trim());
    let fits = (2..=ROLE_BYTES_MAX).contains(&bare.len())
        && bare
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !bare.starts_with('-')
        && !bare.ends_with('-')
        && !bare.contains("--");
    fits.then(|| format!("@{bare}"))
}

/// Writes one call into the mailbox and waits for the engine's answer.
/// Takes the mailbox and the call. Returns the answer text and whether it is an error.
fn ask(mailbox: &Path, call: &Call) -> (String, bool) {
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
    let request = mailbox.join(format!("{id}{CALL_SUFFIX}"));
    if create_private(&request)
        .and_then(|mut file| file.write_all(&line))
        .is_err()
    {
        let _ = std::fs::remove_file(&request);
        return (UNSENT.into(), true);
    }
    let answered = mailbox.join(format!("{id}{REPLY_SUFFIX}"));
    let deadline = std::time::Instant::now() + REPLY_WAIT;
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
/// first, never more than `MAILBOX_SCAN_MAX` entries looked at.
fn calls(mailbox: &Path, suffix: &str) -> Vec<(std::time::SystemTime, String)> {
    let Ok(entries) = std::fs::read_dir(mailbox) else {
        return Vec::new();
    };
    let mut found: Vec<(std::time::SystemTime, String)> = entries
        .take(MAILBOX_SCAN_MAX)
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            let id = name.strip_suffix(suffix)?;
            let metadata = entry.metadata().ok()?;
            (call_id_valid(id) && metadata.is_file()).then(|| {
                (
                    metadata.modified().unwrap_or(std::time::UNIX_EPOCH),
                    id.to_owned(),
                )
            })
        })
        .collect();
    found.sort();
    found
}

/// Takes the oldest calls a live master's server left in its mailbox, removing each one
/// read, and removes answers nobody collected for `REPLY_STALE`. Links and other non-files
/// are never read; a call past `CALL_BYTES_MAX` is removed unread. Takes the mailbox and
/// how many to take.
/// Returns each call id with its call, or none when the file did not hold a valid call.
pub fn take_calls(mailbox: &Path, max: usize) -> Vec<(String, Option<Call>)> {
    if !std::fs::symlink_metadata(mailbox).is_ok_and(|metadata| metadata.is_dir()) {
        return Vec::new();
    }
    let now = std::time::SystemTime::now();
    for (modified, id) in calls(mailbox, REPLY_SUFFIX) {
        if now
            .duration_since(modified)
            .is_ok_and(|age| age >= REPLY_STALE)
        {
            let _ = std::fs::remove_file(mailbox.join(format!("{id}{REPLY_SUFFIX}")));
        }
    }
    let mut taken = Vec::new();
    for (_, id) in calls(mailbox, CALL_SUFFIX).into_iter().take(max) {
        let path = mailbox.join(format!("{id}{CALL_SUFFIX}"));
        let call = open_recorded(&path).ok().and_then(|file| {
            let metadata = file.metadata().ok()?;
            if !metadata.is_file() || metadata.len() > CALL_BYTES_MAX {
                return None;
            }
            let mut bytes = Vec::new();
            (&file).take(CALL_BYTES_MAX).read_to_end(&mut bytes).ok()?;
            serde_json::from_slice::<Call>(bytes.trim_ascii()).ok()
        });
        let _ = std::fs::remove_file(&path);
        taken.push((id, call));
    }
    assert!(taken.len() <= max);
    taken
}

/// Answers one call in the mailbox: the answer is written beside it under a part name and
/// renamed into place, so the server never reads half an answer. Takes the mailbox, the
/// call id `take_calls` returned, the text, bounded to `REPLY_TEXT_MAX`, and whether it is
/// an error. Returns the I/O failure; a planted part file makes the answer fail, never land
/// elsewhere.
pub fn answer_call(mailbox: &Path, id: &str, text: &str, error: bool) -> std::io::Result<()> {
    assert!(call_id_valid(id));
    let reply = Reply {
        text: super::bound_text(text, REPLY_TEXT_MAX),
        error,
    };
    let bytes = serde_json::to_vec(&reply).map_err(std::io::Error::other)?;
    assert!(bytes.len() as u64 <= REPLY_BYTES_MAX);
    let part = mailbox.join(format!("{id}{PART_SUFFIX}"));
    let written = create_private(&part).and_then(|mut file| file.write_all(&bytes));
    if let Err(problem) = written {
        let _ = std::fs::remove_file(&part);
        return Err(problem);
    }
    std::fs::rename(&part, mailbox.join(format!("{id}{REPLY_SUFFIX}")))
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
