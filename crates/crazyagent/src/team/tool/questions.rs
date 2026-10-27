use super::{ASK_TOOL, CALLS_PER_TICK_MAX, Call, REPLY_WAIT};
use crate::engine::EngineError;
use serde_json::{Value, json};
use std::io::{BufRead, Read, Write};
use std::path::{Path, PathBuf};

pub const ASK_DROP_FILE: &str = "crazyagent-questions.jsonl";
pub const AGENT_SERVER_COMMAND: &str = "team-mcp-agent";
pub const ASKS_PER_REPLY_MAX: usize = 4;
pub const ASK_DROP_BYTES_MAX: u64 = ASKS_PER_REPLY_MAX as u64 * super::CALL_BYTES_MAX;
const ASK_LOGGED: &str = "Recorded. When your reply ends, the engine logs this question in the project's questions and notifies the owner. Their answer or dismissal comes back to you once on a later turn, so do not ask it again.";
const ASK_ONLY: &str = "Not asked: this tool only takes a question.";
const ASKS_FULL: &str = "Not asked: the engine takes at most 4 questions from one reply. Ask the next one after the owner answers.";
const ASK_UNWRITABLE: &str = "Not asked: the engine's question record could not be written.";
const AGENT_ASKS_ONLY: &str = "Not sent: this server only asks the owner questions.";
const UNREAD: &str = "The engine could not read this call. Call the tool again.";

pub const NOTES_BYTES_MAX: usize = 8192;
const NOTE_PREFIX: &str = "Engine: ";

const _: () = assert!(ASKS_PER_REPLY_MAX <= CALLS_PER_TICK_MAX);
const _: () = assert!(NOTES_BYTES_MAX >= 2 * crate::questions::ANSWER_BYTES_MAX);

/// The ask tool's listing, the same description and input schema the live master's list
/// shows. Output: the tool's JSON entry, named `ASK_TOOL`.
pub(super) fn ask_listing() -> Value {
    let listed = super::ask_tool();
    assert_eq!(listed["name"], ASK_TOOL);
    listed
}

/// Writes how the owner settled a conversation master's questions as engine lines for the
/// head of its next prompt, oldest first, while they fit in `NOTES_BYTES_MAX`; the rest
/// wait for a later turn. Input: the notes from the questions log. Output: the lines,
/// each note followed by a blank line, and the ids of the notes they carry.
pub fn note_lines(notes: &[crate::questions::Note]) -> (String, Vec<u64>) {
    let mut lines = String::new();
    let mut carried = Vec::new();
    for note in notes {
        let added = NOTE_PREFIX.len() + note.text.len() + 2;
        if lines.len() + added > NOTES_BYTES_MAX {
            break;
        }
        lines.push_str(NOTE_PREFIX);
        lines.push_str(&note.text);
        lines.push_str("\n\n");
        carried.push(note.question);
    }
    assert!(lines.len() <= NOTES_BYTES_MAX);
    assert!(carried.len() <= notes.len());
    (lines, carried)
}

/// The record of the questions a drop-file master asked during one reply, beside its
/// delegation drop file in the same private temporary folder. Input: the drop file.
/// Output: the question record's path.
pub fn ask_file(drop: &Path) -> PathBuf {
    drop.with_file_name(ASK_DROP_FILE)
}

/// Answers one `tools/call` of the ask tool on the drop-file server: the call is checked
/// as the live server checks it and recorded for the engine, which logs it when the reply
/// ends. Inputs: the call's params and the absolute drop file. Output: none for another
/// tool, else the text the master reads and whether it is an error.
pub(super) fn drop_call(params: Option<&Value>, drop: &Path) -> Option<(String, bool)> {
    let params = params?;
    if params.get("name")?.as_str()? != ASK_TOOL {
        return None;
    }
    Some(match super::live_call(ASK_TOOL, params.get("arguments"))? {
        Err(refused) => (refused.to_owned(), true),
        Ok(call @ Call::Ask { .. }) => match record_ask(drop, &call) {
            Ok(true) => (ASK_LOGGED.to_owned(), false),
            Ok(false) => (ASKS_FULL.to_owned(), true),
            Err(_) => (ASK_UNWRITABLE.to_owned(), true),
        },
        Ok(_) => (ASK_ONLY.to_owned(), true),
    })
}

fn record_ask(drop: &Path, call: &Call) -> std::io::Result<bool> {
    assert!(matches!(call, Call::Ask { .. }));
    let mut file = super::open_drop(&ask_file(drop))?;
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(std::io::Error::other(
            "the question record is not a regular file",
        ));
    }
    let mut recorded = Vec::new();
    (&file)
        .take(ASK_DROP_BYTES_MAX)
        .read_to_end(&mut recorded)?;
    let mut line = serde_json::to_vec(call).map_err(std::io::Error::other)?;
    line.push(b'\n');
    let asked = recorded.iter().filter(|byte| **byte == b'\n').count();
    if asked >= ASKS_PER_REPLY_MAX || metadata.len() + line.len() as u64 > ASK_DROP_BYTES_MAX {
        return Ok(false);
    }
    file.write_all(&line)?;
    Ok(true)
}

/// Takes the questions a drop-file master recorded during one reply, and clears the
/// record. Each is checked again, since the master's side could have written the file.
/// Input: the master's delegation drop file; a link or other non-file is never read.
/// Output: at most `ASKS_PER_REPLY_MAX` questions in call order.
pub fn take_asks(drop: &Path) -> Vec<Call> {
    let path = ask_file(drop);
    let opened = super::open_recorded(&path);
    clear_asks(drop);
    let Ok(file) = opened else {
        return Vec::new();
    };
    if !file.metadata().is_ok_and(|metadata| metadata.is_file()) {
        return Vec::new();
    }
    let mut text = Vec::new();
    if (&file)
        .take(ASK_DROP_BYTES_MAX)
        .read_to_end(&mut text)
        .is_err()
    {
        return Vec::new();
    }
    let found: Vec<Call> = text
        .split(|byte| *byte == b'\n')
        .filter_map(|line| serde_json::from_slice::<Call>(line).ok())
        .filter_map(|call| super::checked(call).ok())
        .filter(|call| matches!(call, Call::Ask { .. }))
        .take(ASKS_PER_REPLY_MAX)
        .collect();
    assert!(found.len() <= ASKS_PER_REPLY_MAX);
    found
}

/// Removes a drop-file master's question record so a new turn starts with none.
/// Input: the master's delegation drop file. Output: nothing; a missing record is clear.
pub fn clear_asks(drop: &Path) {
    super::clear(&ask_file(drop));
}

/// Serves a sub-agent's ask tool as an MCP server over stdio until its client closes
/// input: each question goes into the mailbox folder in the agent's private temporary
/// folder and its result is the engine's answer, the question's id. Inputs: the client's
/// input and output and the absolute mailbox folder the engine made. Output: an input or
/// output failure, or a refusal when the mailbox is not a plain folder.
pub fn serve_agent(input: impl BufRead, output: impl Write, mailbox: &Path) -> std::io::Result<()> {
    let plain = std::fs::symlink_metadata(mailbox).is_ok_and(|metadata| metadata.is_dir());
    if !mailbox.is_absolute() || !plain {
        return Err(std::io::Error::other(
            "the mailbox must be an absolute plain folder",
        ));
    }
    super::rpc(input, output, |request| answer_agent(request, mailbox))
}

fn answer_agent(request: &Value, mailbox: &Path) -> Option<Value> {
    let method = request.get("method")?.as_str();
    let id = request.get("id")?.clone();
    let params = request.get("params");
    let result = match method {
        Some("initialize") => super::initialized(params),
        Some("ping") => json!({}),
        Some("tools/list") => json!({"tools": [ask_listing()]}),
        Some("tools/call") => {
            let name = params
                .and_then(|params| params.get("name"))
                .and_then(Value::as_str);
            if name != Some(ASK_TOOL) {
                return Some(super::failure(id, -32602, "Unknown tool"));
            }
            let arguments = params.and_then(|params| params.get("arguments"));
            match super::live_call(ASK_TOOL, arguments) {
                Some(Ok(call @ Call::Ask { .. })) => {
                    let (text, error) = super::ask(mailbox, &call, REPLY_WAIT);
                    super::tool_result(&text, error)
                }
                Some(Ok(_)) => super::tool_result(AGENT_ASKS_ONLY, true),
                Some(Err(refused)) => super::tool_result(refused, true),
                None => return Some(super::failure(id, -32602, "Unknown tool")),
            }
        }
        _ => return Some(super::failure(id, -32601, "Method not found")),
    };
    Some(json!({"jsonrpc": "2.0", "id": id, "result": result}))
}

/// Answers the questions a sub-agent's ask server left in its mailbox, at most
/// `CALLS_PER_TICK_MAX` per call, each checked again before it is logged. Only questions
/// are taken: any other call is refused. Every call is answered, a refused log included.
/// Inputs: the mailbox and the step that logs one question for this sub-agent and returns
/// its id or the store's refusal. Output: how many calls were answered.
pub fn serve_agent_calls(
    mailbox: &Path,
    mut log: impl FnMut(&crate::questions::Ask) -> Result<u64, EngineError>,
) -> usize {
    let taken = super::take_calls(mailbox, CALLS_PER_TICK_MAX);
    for (id, call) in &taken {
        let (text, error) = match call.clone().map(super::checked) {
            None => (UNREAD.to_owned(), true),
            Some(Err(refused)) => (refused.to_owned(), true),
            Some(Ok(Call::Ask {
                question,
                options,
                recommended,
                context,
            })) => {
                let ask = crate::questions::Ask {
                    question,
                    options,
                    recommended,
                    context,
                };
                logged(log(&ask))
            }
            Some(Ok(_)) => (AGENT_ASKS_ONLY.to_owned(), true),
        };
        let _ = super::answer_call(mailbox, id, &text, error);
    }
    taken.len()
}

/// The sentence a sub-agent reads once its question was logged, or why it was not.
/// Input: the store's outcome. Output: the text and whether it is an error.
fn logged(outcome: Result<u64, EngineError>) -> (String, bool) {
    let refused = |text: &str| (text.to_owned(), true);
    match outcome {
        Ok(id) => (
            format!(
                "Question {id} is logged and the owner is notified. Your master sees it too, and the owner's answer or dismissal reaches you once through your master, so do not ask it again."
            ),
            false,
        ),
        Err(problem) => match problem.0.as_str() {
            "questions_asker_full" => refused(
                "Not asked: 8 of your questions are still open. Wait for the owner's answers before asking more.",
            ),
            "questions_full" => refused(
                "Not asked: 64 questions of this project are still open. Wait for the owner's answers before asking more.",
            ),
            "questions_ask_invalid" | "questions_text_invalid" => (
                format!(
                    "Not asked: give one plain question, at most {} distinct one-line options, and a recommended answer that is one of them.",
                    crate::questions::OPTIONS_MAX
                ),
                true,
            ),
            _ => {
                refused("Not asked: the questions log could not be written. Try again in a moment.")
            }
        },
    }
}

#[cfg(all(test, unix))]
#[path = "questions/tests.rs"]
mod tests;
