use super::{ASK_TOOL, CALLS_PER_TICK_MAX, Call, RECORD_TOOL, REQUEST_ACCESS_TOOL, WITHDRAW_TOOL};
use crate::engine::EngineError;
use crate::protocol::{ProjectId, SessionId};
use crate::questions::{Asker, Settlement};
use crate::sandbox::grants::{self, Requested};
use crate::sandbox::policy::AccessGrant;
use rusqlite::Connection;
use serde_json::Value;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

pub const ASK_DROP_FILE: &str = "crazyagent-questions.jsonl";
pub const ASKS_PER_REPLY_MAX: usize = 8;
pub const ASK_DROP_BYTES_MAX: u64 = ASKS_PER_REPLY_MAX as u64 * super::CALL_BYTES_MAX;
const ASK_LOGGED: &str = "Recorded. When your reply ends, the engine logs this question in the project's questions and notifies the owner, unless 8 of your questions are still open. Their answer or dismissal comes back to you once on a later turn, so do not ask it again.";
const RECORD_TAKEN: &str = "Noted. When your reply ends, the engine records the owner's message as the answer, taking the words from that message and never from yours, if the question is still open and the message is the owner's.";
const REQUEST_TAKEN: &str = "Recorded. When your reply ends, the engine checks the request and raises it as a question in the owner's Questions view. If the owner allows it, it joins the project's Agent access and you get it at your next start; do not ask again.";
const REQUEST_HELD: &str = "The project already allows that, so nothing was asked. If your sandbox still blocks it, you get it at your next start.";
const WITHDRAW_TAKEN: &str = "Noted. When your reply ends, the engine takes the question off the owner's list if it is still open and was asked by you or by a sub-agent of this project.";
const RELAY_LIVE_ONLY: &str = "Not sent: only a master in the owner's terminal records an answer as its own words. Name the owner's message with owner_turn instead.";
const ASK_ONLY: &str = "Not sent: this tool only takes a question, a recorded answer, a withdrawal or an access request.";
const ASKS_FULL: &str = "Not sent: the engine takes at most 8 question calls from one reply. Make the next one in a later reply.";
const ASK_UNWRITABLE: &str = "Not sent: the engine's question record could not be written.";
const AGENT_ASKS_ONLY: &str = "Not recorded: the owner answers a sub-agent's questions in the Questions view, so only the master records an answer from chat.";
const AGENT_WITHDRAW_REFUSED: &str = "Not withdrawn: only the master takes a question off the owner's list. Tell your master the question is no longer needed.";

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

/// The `withdraw_question` tool's listing, as the live master's list shows it (F12).
/// Output: the tool's JSON entry, named `WITHDRAW_TOOL`.
pub(super) fn withdraw_listing() -> Value {
    let listed = super::withdraw_tool();
    assert_eq!(listed["name"], WITHDRAW_TOOL);
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

/// The `request_access` tool's listing, as the live master's list shows it. Output: the
/// tool's JSON entry, named `REQUEST_ACCESS_TOOL`.
pub(super) fn request_listing() -> Value {
    let listed = super::request_access_tool();
    assert_eq!(listed["name"], REQUEST_ACCESS_TOOL);
    listed
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

/// Answers one `tools/call` of the `ask`, `record_answer`, `withdraw_question` or
/// `request_access` tool on the drop-file server: the call is checked as the live server checks it and recorded for
/// the engine, which takes it when the reply ends. An answer given as the master's own
/// words is refused, since a drop-file master has no terminal the owner types into. Inputs:
/// the call's params and the absolute drop file. Output: none for another tool, else the
/// text the master reads and whether it is an error.
pub(super) fn drop_call(params: Option<&Value>, drop: &Path) -> Option<(String, bool)> {
    let params = params?;
    let name = params.get("name")?.as_str()?;
    if ![ASK_TOOL, RECORD_TOOL, WITHDRAW_TOOL, REQUEST_ACCESS_TOOL].contains(&name) {
        return None;
    }
    Some(match super::live_call(name, params.get("arguments"))? {
        Err(refused) => (refused.to_owned(), true),
        Ok(Call::RelayAnswer { .. }) => (RELAY_LIVE_ONLY.to_owned(), true),
        Ok(call) if taken(&call) => match record_ask(drop, &call) {
            Ok(true) => (
                match call {
                    Call::Ask { .. } => ASK_LOGGED,
                    Call::Withdraw { .. } => WITHDRAW_TAKEN,
                    Call::RequestAccess { .. } => REQUEST_TAKEN,
                    _ => RECORD_TAKEN,
                }
                .to_owned(),
                false,
            ),
            Ok(false) => (ASKS_FULL.to_owned(), true),
            Err(_) => (ASK_UNWRITABLE.to_owned(), true),
        },
        Ok(_) => (ASK_ONLY.to_owned(), true),
    })
}

/// Says whether a question call is one the engine takes from a drop-file or conversation
/// master's record: an ask, an answer recorded from the owner's message, a withdrawal or an
/// access request. Input: the call. Output: the answer.
fn taken(call: &Call) -> bool {
    matches!(
        call,
        Call::Ask { .. }
            | Call::RecordAnswer { .. }
            | Call::Withdraw { .. }
            | Call::RequestAccess { .. }
    )
}

fn record_ask(drop: &Path, call: &Call) -> std::io::Result<bool> {
    assert!(taken(call));
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

/// Takes the question calls, asks, recorded answers and withdrawals, a drop-file master made during one
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
        .filter(taken)
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
        Call::RequestAccess { grant, reason } => not_raised(db, project, &asker, &grant, &reason),
        Call::RecordAnswer {
            question,
            version,
            owner_turn,
        } => not_recorded(
            question,
            version,
            crate::questions::record_from_chat(db, project, master, question, version, owner_turn),
        ),
        Call::Withdraw { question, version } => not_withdrawn(
            question,
            version,
            crate::questions::withdraw(db, project, &asker, question, version),
        ),
        _ => unreachable!("take_asks returns only question calls"),
    })
}

/// Logs and records what a conversation master asked of the owner during one turn, under
/// its session. Answers are recorded first, and only from the owner's message of that turn,
/// so no question is answered by a message older than itself, and that message answers at
/// most one question, as an owner turn of a team master does. Inputs: the engine
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
    let mut used = false;
    applied(calls, |call| match call {
        Call::Ask { .. } => logged_ask(db, project, &asker, call),
        Call::RequestAccess { grant, reason } => not_raised(db, project, &asker, &grant, &reason),
        Call::RecordAnswer {
            question,
            version,
            owner_turn,
        } => match owner {
            Some((number, _)) if used && u64::try_from(owner_turn).ok() == Some(number) => {
                Some(format!(
                    "Your record of question {question} was not taken: message {owner_turn} already answered another question."
                ))
            }
            Some((number, text)) if u64::try_from(owner_turn).ok() == Some(number) => {
                let outcome = crate::questions::record_from_session(
                    db, project, session, question, version, text,
                );
                used = matches!(outcome, Ok(Settlement::Settled));
                not_recorded(question, version, outcome)
            }
            _ => Some(format!(
                "Your record of question {question} was not taken: message {owner_turn} is not the owner's message of that turn."
            )),
        },
        Call::Withdraw { question, version } => not_withdrawn(
            question,
            version,
            crate::questions::withdraw(db, project, &asker, question, version),
        ),
        _ => unreachable!("take_asks returns only question calls"),
    })
}

/// Raises a drop-file or conversation master's access request when its reply ends, as
/// [`request_reply`] does. Inputs: the engine connection, the project, the master as
/// asker, the grant and its reason. Output: none once asked, else the sentence for its
/// next turn.
fn not_raised(
    db: &Connection,
    project: &ProjectId,
    asker: &Asker,
    grant: &AccessGrant,
    reason: &str,
) -> Option<String> {
    match grants::request(db, project, asker, grant, reason) {
        Ok(Requested::Asked(_)) => None,
        Ok(Requested::AlreadyAllowed) => Some(format!(
            "Your access request for {}: {REQUEST_HELD}",
            grant.label()
        )),
        Err(code) => Some(format!(
            "Your access request for {} was not raised: {}",
            grant.label(),
            grants::refusal_text(&code)
        )),
    }
}

/// Raises one access request (F08) for the owner and says how it went: the grant is
/// checked as a save checks it, a grant the project already allows asks nothing, and the
/// reason is plained and quoted in the owner's question. Inputs: the engine connection, the
/// project, who asked, the grant and the reason. Output: the sentence the caller reads and
/// whether it is an error.
pub(in crate::team) fn request_reply(
    db: &Connection,
    project: &ProjectId,
    asker: &Asker,
    grant: &AccessGrant,
    reason: &str,
) -> (String, bool) {
    match grants::request(db, project, asker, grant, reason) {
        Ok(Requested::Asked(question)) => (
            format!(
                "Access request raised as question {question} in the owner's Questions view. If the owner allows it, it joins this project's Agent access and you restart at your next pause with it; you are not told otherwise, so do not ask again."
            ),
            false,
        ),
        Ok(Requested::AlreadyAllowed) => (REQUEST_HELD.to_owned(), false),
        Err(code) => (
            format!("Not requested: {}", grants::refusal_text(&code)),
            true,
        ),
    }
}

fn applied(calls: Vec<Call>, apply: impl FnMut(Call) -> Option<String>) -> Vec<String> {
    assert!(calls.len() <= ASKS_PER_REPLY_MAX);
    let refused: Vec<String> = calls.into_iter().filter_map(apply).collect();
    assert!(refused.len() <= ASKS_PER_REPLY_MAX);
    refused
}

/// Logs one question a drop-file or conversation master asked, or asks it in place of an
/// old free-text one when it names one to replace (F14). Inputs: the engine connection, the
/// project, the master as asker and its `ask` call. Output: none when it was logged, else
/// the sentence for the master's next turn.
fn logged_ask(db: &Connection, project: &ProjectId, asker: &Asker, call: Call) -> Option<String> {
    let (ask, replaces) = split_ask(call);
    let asked = match replaces {
        Some(old) => crate::questions::reask(db, project, asker, &ask, old),
        None => crate::questions::ask(db, project, asker, &ask),
    };
    let problem = asked.err()?;
    let why = match problem.0.as_str() {
        "questions_asker_full" => "8 of your questions are still open",
        "questions_full" => "64 questions of this project are still open",
        "questions_replace_invalid" => {
            "the question it replaces is not an open free-text question of this project asked by you or by an engine step, or the new one is not of kind single or multi"
        }
        "questions_ask_invalid" | "questions_text_invalid" => {
            "it is not one plain question of kind single or multi with 2 to 6 one-line options and one of them recommended, or of kind text with no options"
        }
        _ => "the questions log could not be written",
    };
    Some(format!(
        "Your question {} was not logged for the owner: {why}.",
        quoted(&ask.question)
    ))
}

/// Splits a checked `ask` call into the question for the store and the number of the old
/// question it replaces, if any. Input: the call, which must be an ask. Output: both parts.
fn split_ask(call: Call) -> (crate::questions::Ask, Option<u64>) {
    let Call::Ask {
        question,
        kind,
        options,
        recommended,
        context,
        replaces,
    } = call
    else {
        unreachable!("only a question is logged");
    };
    let ask = crate::questions::Ask {
        question,
        kind,
        options,
        recommended,
        context,
    };
    (ask, replaces)
}

/// The sentence a drop-file or conversation master reads on a later turn when its
/// withdrawal was not taken. Inputs: the question, the version it read and the store's
/// outcome. Output: none when the question was withdrawn, else the sentence.
fn not_withdrawn(
    question: u64,
    version: u64,
    outcome: Result<Settlement, EngineError>,
) -> Option<String> {
    let why = match outcome {
        Ok(Settlement::Settled) => return None,
        Ok(Settlement::Stale) => {
            format!("it is already answered or dismissed, or changed since version {version}")
        }
        Err(problem) if problem.0 == "questions_not_found" => {
            "it is not an open question of this project asked by you or by a sub-agent".to_owned()
        }
        Err(_) => "the questions log could not be written".to_owned(),
    };
    Some(format!(
        "Your withdrawal of question {question} was not taken: {why}."
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
                "that message is not the owner's, came before the question, or already answered another question".to_owned()
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

/// Logs the question a sub-agent asked through the `ask` tool of its team server, with the
/// Logs a sub-agent's `ask` with it as the asker, in place of an old free-text question of
/// its own when it names one, raises its `request_access` as [`request_reply`] does and
/// refuses its `record_answer` and `withdraw_question`. Inputs: the engine connection, the
/// project, the sub-agent's team id and its checked question call. Output: the sentence
/// the sub-agent reads and whether it is an error.
pub(in crate::team) fn agent_ask(
    db: &Connection,
    project: &ProjectId,
    agent: i64,
    call: Call,
) -> (String, bool) {
    match call {
        Call::Ask { .. } => {
            let (ask, replaces) = split_ask(call);
            let asker = Asker::Agent(agent);
            logged(match replaces {
                Some(old) => crate::questions::reask(db, project, &asker, &ask, old),
                None => crate::questions::ask(db, project, &asker, &ask),
            })
        }
        Call::RecordAnswer { .. } | Call::RelayAnswer { .. } => (AGENT_ASKS_ONLY.to_owned(), true),
        Call::Withdraw { .. } => (AGENT_WITHDRAW_REFUSED.to_owned(), true),
        Call::RequestAccess { grant, reason } => {
            request_reply(db, project, &Asker::Agent(agent), &grant, &reason)
        }
        _ => unreachable!("only question calls reach agent_ask"),
    }
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
            "questions_replace_invalid" => refused(
                "Not asked: the question it replaces is not an open free-text question you asked in this project, or the new one is not of kind single or multi.",
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
