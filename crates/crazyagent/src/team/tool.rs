use super::{DELEGATIONS_PER_REPLY_MAX, Delegation, TASK_PROMPT_BYTES_MAX, delegation};
use rustix::fs::{Mode, OFlags};
use serde::Deserialize;
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
const RECORDED: &str = "Recorded. When your reply ends, the engine checks this task against the team plan and hands it to a sub-agent; the result comes back to you.";
const INVALID: &str = "Not recorded: give a task of plain text up to 4000 bytes and, if you choose one, a harness name from the team plan.";
const FULL: &str = "Not recorded: the engine takes at most 8 tasks from one reply.";
const UNWRITABLE: &str = "Not recorded: the engine's task record could not be written.";

const _: () = assert!(DROP_BYTES_MAX as usize >= DELEGATIONS_PER_REPLY_MAX * TASK_PROMPT_BYTES_MAX);
const _: () = assert!(TASK_PROMPT_BYTES_MAX == 4000 && DELEGATIONS_PER_REPLY_MAX == 8);

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Recorded {
    harness: Option<String>,
    task: String,
}

/// Serves the master's `delegate` tool as an MCP server over stdio until its client closes input.
/// Takes the client's input and output and the absolute drop file in the master's private temporary folder.
/// Returns an input or output failure; each call only records a proposal, which the engine admits after the turn.
pub fn serve(mut input: impl BufRead, mut output: impl Write, drop: &Path) -> std::io::Result<()> {
    if !drop.is_absolute() {
        return Err(std::io::Error::other(
            "the drop file must be an absolute path",
        ));
    }
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
        let answer = match serde_json::from_slice::<Value>(text) {
            Ok(request) => answer(&request, drop),
            Err(_) => Some(failure(Value::Null, -32700, "Parse error")),
        };
        if let Some(answer) = answer {
            reply(&mut output, &answer)?;
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

fn answer(request: &Value, drop: &Path) -> Option<Value> {
    let method = request.get("method")?.as_str();
    let id = request.get("id")?.clone();
    let params = request.get("params");
    let result = match method {
        Some("initialize") => {
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
        Some("ping") => json!({}),
        Some("tools/list") => json!({"tools": [{
            "name": TOOL_NAME,
            "description": "Hand one task to a sub-agent of this project's team. The engine checks it against the team plan when your reply ends, runs it in its own git worktree and sends you the result.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "task": {"type": "string", "description": "What the sub-agent should do, in plain words."},
                    "harness": {"type": "string", "description": "Optional sub-agent harness from the team plan, such as codex or claude-code."},
                },
                "required": ["task"],
                "additionalProperties": false,
            },
        }]}),
        Some("tools/call") => match call(params, drop) {
            Some((text, error)) => json!({
                "content": [{"type": "text", "text": text}],
                "isError": error,
            }),
            None => return Some(failure(id, -32602, "Unknown tool")),
        },
        _ => return Some(failure(id, -32601, "Method not found")),
    };
    Some(json!({"jsonrpc": "2.0", "id": id, "result": result}))
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
    let Some(proposed) = delegation(argument("harness"), argument("task")) else {
        return Some((INVALID, true));
    };
    Some(match record(drop, &proposed) {
        Ok(true) => (RECORDED, false),
        Ok(false) => (FULL, true),
        Err(_) => (UNWRITABLE, true),
    })
}

fn record(drop: &Path, proposed: &Delegation) -> std::io::Result<bool> {
    let mut file = File::from(rustix::fs::open(
        drop,
        OFlags::RDWR | OFlags::CREATE | OFlags::APPEND | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::RUSR | Mode::WUSR,
    )?);
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(std::io::Error::other("the drop file is not a regular file"));
    }
    let mut recorded = Vec::new();
    (&file).take(DROP_BYTES_MAX).read_to_end(&mut recorded)?;
    let mut line =
        serde_json::to_vec(&json!({"harness": proposed.harness, "task": proposed.prompt}))
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
    let opened = rustix::fs::open(
        drop,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map(File::from);
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
            delegation(
                recorded.harness.as_deref().unwrap_or_default(),
                &recorded.task,
            )
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

#[cfg(test)]
#[path = "tool/tests.rs"]
mod tests;
