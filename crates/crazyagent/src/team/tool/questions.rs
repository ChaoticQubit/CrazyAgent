use super::{ASK_TOOL, CALLS_PER_TICK_MAX, Call, RECORD_TOOL, REPLY_WAIT};
use crate::engine::EngineError;
use crate::protocol::{ProjectId, SessionId};
use crate::questions::{Asker, Settlement};
use rusqlite::Connection;
use serde_json::{Value, json};
use std::io::{BufRead, Read, Write};
use std::path::{Path, PathBuf};

pub const ASK_DROP_FILE: &str = "crazyagent-questions.jsonl";
pub const AGENT_SERVER_COMMAND: &str = "team-mcp-agent";
pub const ASKS_PER_REPLY_MAX: usize = 8;
pub const ASK_DROP_BYTES_MAX: u64 = ASKS_PER_REPLY_MAX as u64 * super::CALL_BYTES_MAX;
const ASK_LOGGED: &str = "Recorded. When your reply ends, the engine logs this question in the project's questions and notifies the owner, unless 8 of your questions are still open. Their answer or dismissal comes back to you once on a later turn, so do not ask it again.";
const RECORD_TAKEN: &str = "Noted. When your reply ends, the engine records the owner's message as the answer, taking the words from that message and never from yours, if the question is still open and the message is the owner's.";
const ASK_ONLY: &str = "Not sent: this tool only takes a question or a recorded answer.";
const ASKS_FULL: &str = "Not sent: the engine takes at most 8 question calls from one reply. Make the next one in a later reply.";
const ASK_UNWRITABLE: &str = "Not sent: the engine's question record could not be written.";
const AGENT_ASKS_ONLY: &str = "Not sent: this server only asks the owner questions.";
const UNREAD: &str = "The engine could not read this call. Call the tool again.";

pub const NOTES_BYTES_MAX: usize = 8192;
pub const OPEN_LINE_BYTES_MAX: usize = 4096;
const NOTE_PREFIX: &str = "Engine: ";

const _: () = assert!(ASKS_PER_REPLY_MAX <= CALLS_PER_TICK_MAX);
const _: () = assert!(ASKS_PER_REPLY_MAX == 8);
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

/// The `record_answer` tool's listing, as the live master's list shows it. Output: the
/// tool's JSON entry, named `RECORD_TOOL`.
pub(super) fn record_listing() -> Value {
    let listed = super::record_tool();
    assert_eq!(listed["name"], RECORD_TOOL);
    listed
}

/// The record of the questions a drop-file master asked during one reply, beside its
/// delegation drop file in the same private temporary folder. Input: the drop file.
/// Output: the question record's path.
pub fn ask_file(drop: &Path) -> PathBuf {
    drop.with_file_name(ASK_DROP_FILE)
}

/// Answers one `tools/call` of the `ask` or `record_answer` tool on the drop-file server:
/// the call is checked as the live server checks it and recorded for the engine, which
/// takes it when the reply ends. Inputs: the call's params and the absolute drop file.
/// Output: none for another tool, else the text the master reads and whether it is an error.
pub(super) fn drop_call(params: Option<&Value>, drop: &Path) -> Option<(String, bool)> {
    let params = params?;
    let name = params.get("name")?.as_str()?;
    if name != ASK_TOOL && name != RECORD_TOOL {
        return None;
    }
    Some(match super::live_call(name, params.get("arguments"))? {
        Err(refused) => (refused.to_owned(), true),
        Ok(call @ (Call::Ask { .. } | Call::RecordAnswer { .. })) => {
            match record_ask(drop, &call) {
                Ok(true) if matches!(call, Call::Ask { .. }) => (ASK_LOGGED.to_owned(), false),
                Ok(true) => (RECORD_TAKEN.to_owned(), false),
                Ok(false) => (ASKS_FULL.to_owned(), true),
                Err(_) => (ASK_UNWRITABLE.to_owned(), true),
            }
        }
        Ok(_) => (ASK_ONLY.to_owned(), true),
    })
}

fn record_ask(drop: &Path, call: &Call) -> std::io::Result<bool> {
    assert!(matches!(call, Call::Ask { .. } | Call::RecordAnswer { .. }));
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

/// Takes the question calls, asks and recorded answers, a drop-file master made during one
/// reply, and clears the record. Each is checked again, since the master's side could have
/// written the file. Input: the master's delegation drop file; a link or other non-file is
/// never read. Output: at most `ASKS_PER_REPLY_MAX` calls in call order.
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
        .filter(|call| matches!(call, Call::Ask { .. } | Call::RecordAnswer { .. }))
        .take(ASKS_PER_REPLY_MAX)
        .collect();
    assert!(found.len() <= ASKS_PER_REPLY_MAX);
    found
}

/// Writes the engine line that reminds a drop-file or conversation master of its questions
/// still open for the owner and, when the turn carries an owner message, gives that
/// message's number for `record_answer`. Inputs: the project's pending questions, the master
/// as asker and the owner message number. Output: the line and a blank line, or nothing
/// when the master has no open question.
pub fn open_line(
    pending: &[crate::questions::Question],
    asker: &Asker,
    message: Option<u64>,
) -> String {
    let name = asker.name();
    let mine: Vec<String> = pending
        .iter()
        .filter(|question| question.asker == name)
        .take(crate::questions::PENDING_PER_ASKER_MAX as usize)
        .map(|question| format!("{} {}", question.id, quoted(&question.question)))
        .collect();
    if mine.is_empty() {
        return String::new();
    }
    let mut line = format!(
        "{NOTE_PREFIX}Your questions still open for the owner: {}.",
        mine.join("; ")
    );
    if let Some(message) = message {
        line.push_str(&format!(
            " The owner's message below is message {message}. If it answers one of them, record it with {RECORD_TOOL}: the question's number, version 0 and owner_turn {message}."
        ));
    }
    line.push_str("\n\n");
    assert!(line.len() <= OPEN_LINE_BYTES_MAX);
    line
}

/// Logs and records what a drop-file team master asked of the owner during one reply, as
/// that master's own: each question is logged, and each recorded answer is taken from the
/// owner message the master named. Inputs: the engine connection, the project, the master's
/// team agent id and its drop file. Output: one sentence per call the store refused, for
/// the master's next turn, at most `ASKS_PER_REPLY_MAX`.
pub fn apply_master_calls(
    db: &Connection,
    project: &ProjectId,
    master: i64,
    drop: &Path,
) -> Vec<String> {
    let asker = Asker::Master(master);
    applied(take_asks(drop), |call| match call {
        Call::Ask { .. } => logged_ask(db, project, &asker, call),
        Call::RecordAnswer {
            question,
            version,
            owner_turn,
        } => not_recorded(
            question,
            version,
            crate::questions::record_from_chat(db, project, master, question, version, owner_turn),
        ),
        _ => unreachable!("take_asks returns only question calls"),
    })
}

/// Logs and records what a conversation master asked of the owner during one turn, under
/// its session. Answers are recorded first, and only from the owner's message of that turn,
/// so no question is answered by a message older than itself. Inputs: the engine
/// connection, the project, the session, the turn's owner message number and text when it
/// has one, and the drop file. Output: as `apply_master_calls`.
pub fn apply_session_calls(
    db: &Connection,
    project: &ProjectId,
    session: &SessionId,
    owner: Option<(u64, &str)>,
    drop: &Path,
) -> Vec<String> {
    let asker = Asker::Session(session.clone());
    let (mut calls, asks): (Vec<Call>, Vec<Call>) = take_asks(drop)
        .into_iter()
        .partition(|call| matches!(call, Call::RecordAnswer { .. }));
    calls.extend(asks);
    applied(calls, |call| match call {
        Call::Ask { .. } => logged_ask(db, project, &asker, call),
        Call::RecordAnswer {
            question,
            version,
            owner_turn,
        } => match owner {
            Some((number, text)) if u64::try_from(owner_turn).ok() == Some(number) => not_recorded(
                question,
                version,
                crate::questions::record_from_session(
                    db, project, session, question, version, text,
                ),
            ),
            _ => Some(format!(
                "Your record of question {question} was not taken: message {owner_turn} is not the owner's message of that turn."
            )),
        },
        _ => unreachable!("take_asks returns only question calls"),
    })
}

fn applied(calls: Vec<Call>, apply: impl FnMut(Call) -> Option<String>) -> Vec<String> {
    assert!(calls.len() <= ASKS_PER_REPLY_MAX);
    let refused: Vec<String> = calls.into_iter().filter_map(apply).collect();
    assert!(refused.len() <= ASKS_PER_REPLY_MAX);
    refused
}

fn logged_ask(db: &Connection, project: &ProjectId, asker: &Asker, call: Call) -> Option<String> {
    let Call::Ask {
        question,
        options,
        recommended,
        context,
    } = call
    else {
        unreachable!("only a question is logged");
    };
    let ask = crate::questions::Ask {
        question,
        options,
        recommended,
        context,
    };
    let problem = crate::questions::ask(db, project, asker, &ask).err()?;
    let why = match problem.0.as_str() {
        "questions_asker_full" => "8 of your questions are still open",
        "questions_full" => "64 questions of this project are still open",
        "questions_ask_invalid" | "questions_text_invalid" => {
            "it is not one plain question with at most 6 one-line options and a recommended answer among them"
        }
        _ => "the questions log could not be written",
    };
    Some(format!(
        "Your question {} was not logged for the owner: {why}.",
        quoted(&ask.question)
    ))
}

fn not_recorded(
    question: u64,
    version: u64,
    outcome: Result<Settlement, EngineError>,
) -> Option<String> {
    let why = match outcome {
        Ok(Settlement::Settled) => return None,
        Ok(Settlement::Stale) => {
            format!("it is already answered or dismissed, or changed since version {version}")
        }
        Err(problem) => match problem.0.as_str() {
            "questions_not_found" => "you asked no such question in this project".to_owned(),
            "questions_owner_turn_invalid" => {
                "that message is not the owner's, or came before the question".to_owned()
            }
            "questions_answer_invalid" | "questions_text_invalid" => {
                "the owner's message cannot be stored as an answer".to_owned()
            }
            _ => "the questions log could not be written".to_owned(),
        },
    };
    Some(format!(
        "Your record of question {question} was not taken: {why}."
    ))
}

/// Quotes a master's own words on one line as a JSON string, at most 200 bytes.
fn quoted(text: &str) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    serde_json::to_string(&crate::team::bound_text(&flat, 200))
        .expect("a string always encodes as JSON")
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
                "Question {id} is logged and the owner is notified. Your master sees it too, and CrazyAgent types the owner's answer or dismissal into your terminal once, so do not ask it again."
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
