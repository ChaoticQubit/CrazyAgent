use super::TurnEnd;
use super::managed_agent::{AGENT, ManagedAgent, PROJECT, SignedIn, claude};
use crate::coord::relay::{self, INBOX, Layout, MARK, SHOWN};
use crate::coord::{MessageId, MessageKind, NewMessage, Party, Receipt};
use crate::protocol::ProjectId;
use crate::team::now_ms;
use crate::usage::capture::Format;
use serde_json::Value;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::Duration;

const TURN: Duration = Duration::from_secs(180);
const KIND_CHARS_MAX: usize = 64;
const CODEWORD: &str = "WREN-5521";
const PLAIN_TURN: &str = "Reply with exactly the word ONE and nothing else. Do not use any tool.";

/// SEC-LOWS-3: a real Claude Code teammate on a managed account, launched the engine's way
/// with the relay plugin, takes a waiting letter only through its Stop hook's `decision:
/// block` in a turn with no tool call. The engine marks the letter shown, then read once the
/// session answers after it, and never hands it again in a later turn. The row prints the
/// record kinds that carried the letter's mark, kind names only.
#[test]
#[ignore = "live qualification: needs CRAZYAGENT_MANAGED_CLAUDE_PROFILE"]
fn a_real_claude_agent_takes_a_letter_by_its_stop_hook_once_and_reads_it_after_its_answer() {
    let engine = engine_executable();
    let mut signed_in = SignedIn::claude();
    let mut agent = ManagedAgent::new("claude-code", &claude());
    agent.runtime = agent.runtime.with_delegation_tool(&engine).unwrap();
    let project = ProjectId::new(PROJECT).unwrap();
    let mailbox = agent.runtime.mailbox(AGENT);

    let (_, end, cause) = agent.run_turn(
        signed_in.acquire("claude-live-stop-hook-first"),
        PLAIN_TURN,
        TURN,
    );
    println!("stop hook row: first turn end={end:?} cause={cause:?}");
    assert_eq!(end, TurnEnd::Success, "{cause:?}");
    assert!(
        relay::installed(&agent.home),
        "the engine launch never installed the relay plugin"
    );

    let id = crate::coord::send(
        &agent.runtime.connection,
        NewMessage {
            project: project.clone(),
            from: Party::Owner,
            to: Party::Agent(AGENT),
            kind: MessageKind::Note,
            question: None,
            work: None,
            reply_to: None,
            body: format!("Reply with exactly the codeword {CODEWORD} and nothing else."),
            idempotency_key: None,
        },
        now_ms(),
    )
    .unwrap();
    let stem = relay::letter_stem(id).unwrap();
    let letter = mailbox.join(format!("{stem}{INBOX}"));
    let taken = mailbox.join(format!("{stem}{SHOWN}"));
    serve(&agent, &project, &mailbox, false);
    assert!(is_file(&letter), "the relay posted no letter");
    assert_eq!(receipt(&agent, id), Receipt::Sent);

    let (text, end, cause) = agent.run_turn(
        signed_in.acquire("claude-live-stop-hook-second"),
        PLAIN_TURN,
        TURN,
    );
    println!(
        "stop hook row: letter turn end={end:?} cause={cause:?} answered={}",
        text.contains(CODEWORD)
    );
    assert_eq!(end, TurnEnd::Success, "{cause:?}");
    assert!(!is_file(&letter), "the session never took the letter");
    assert!(is_file(&taken), "the taken letter is not marked shown");
    let seen = marked(&agent.home, id);
    println!(
        "stop hook row: record kinds carrying the mark={:?} tool calls before it={:?}",
        seen.kinds, seen.tools_before
    );
    assert_eq!(
        seen.tools_before,
        Some(0),
        "the letter reached the session after a tool call or not at all, so not by its Stop hook"
    );

    serve(&agent, &project, &mailbox, false);
    assert_eq!(receipt(&agent, id), Receipt::Delivered);
    assert!(!is_file(&taken));
    serve(&agent, &project, &mailbox, true);
    assert_eq!(
        receipt(&agent, id),
        Receipt::Read,
        "no answer followed the mark in a record kind the relay reads: {:?}",
        seen.kinds
    );

    let (_, end, cause) = agent.run_turn(
        signed_in.acquire("claude-live-stop-hook-third"),
        PLAIN_TURN,
        TURN,
    );
    println!("stop hook row: later turn end={end:?} cause={cause:?}");
    assert_eq!(end, TurnEnd::Success, "{cause:?}");
    serve(&agent, &project, &mailbox, true);
    assert_eq!(receipt(&agent, id), Receipt::Read);
    assert!(!is_file(&letter) && !is_file(&taken));
    let later = marked(&agent.home, id);
    assert_eq!(
        later.kinds.len(),
        seen.kinds.len(),
        "the letter was handed again: {:?}",
        later.kinds
    );
}

/// The built engine executable the relay hooks run: `CRAZYAGENT_ENGINE`, or the
/// `crazyagent` binary beside this test's build folder. Output: its canonical path, or a
/// panic with the plain BLOCKED message when it was not built.
fn engine_executable() -> PathBuf {
    std::env::var_os("CRAZYAGENT_ENGINE")
        .map(PathBuf::from)
        .or_else(|| {
            let deps = std::env::current_exe().ok()?;
            Some(deps.parent()?.parent()?.join("crazyagent"))
        })
        .and_then(|path| path.canonicalize().ok())
        .filter(|path| path.is_file())
        .unwrap_or_else(|| {
            panic!(
                "BLOCKED: the engine executable is not built; run cargo build -p crazyagent --bin crazyagent"
            )
        })
}

/// Runs one relay step for the agent the way the runtime's tick does for a lead with no
/// native team. Inputs: the agent, its project, its mailbox and whether to read receipts.
fn serve(agent: &ManagedAgent, project: &ProjectId, mailbox: &Path, receipts: bool) {
    relay::serve(
        &agent.runtime.connection,
        project,
        AGENT,
        &agent.home,
        mailbox,
        Layout::Single,
        receipts,
        now_ms(),
    )
    .unwrap();
}

/// The receipt the engine holds for a message. Inputs: the agent and the message id.
/// Output: the receipt.
fn receipt(agent: &ManagedAgent, id: MessageId) -> Receipt {
    crate::coord::message(&agent.runtime.connection, id)
        .unwrap()
        .receipt
}

/// Whether a path is a plain file, never following a link. Input: the path. Output: the
/// answer.
fn is_file(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|found| found.is_file())
}

/// What the agent's Claude session records show of one letter: the kind of each record
/// that carries its mark, and how many tool calls its session made before the first such
/// record that is not the session's own answer.
struct Marked {
    kinds: Vec<String>,
    tools_before: Option<usize>,
}

/// Reads the agent home's Claude session records the way the relay does, never following a
/// link, for the records carrying message `id`'s mark. Inputs: the home and the id. Output:
/// the kinds seen, as `type` or `type/attachment type`, and the tool calls before the first.
fn marked(home: &Path, id: MessageId) -> Marked {
    let mark = format!("{MARK}{id}]");
    let mut found = Marked {
        kinds: Vec::new(),
        tools_before: None,
    };
    for path in crate::usage::capture::records(home, Format::Claude).0 {
        let Some((file, _, _)) = crate::usage::capture::open_record(&path) else {
            continue;
        };
        let mut tools = 0;
        for line in BufReader::new(file).lines().map_while(Result::ok) {
            let Ok(record) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            let kind = kind_name(&record["type"]);
            if kind == "assistant"
                && let Some(parts) = record["message"]["content"].as_array()
            {
                tools += parts
                    .iter()
                    .filter(|part| part["type"] == "tool_use")
                    .count();
            }
            if !line.contains(&mark) {
                continue;
            }
            match record["attachment"]["type"] {
                Value::Null => found.kinds.push(kind.to_owned()),
                ref inner => found.kinds.push(format!("{kind}/{}", kind_name(inner))),
            }
            if kind != "assistant" && found.tools_before.is_none() {
                found.tools_before = Some(tools);
            }
        }
    }
    found
}

/// A record kind fit to print: a short name of lower-case letters, `_` and `-`, else
/// `other`, so no record text reaches the row's output. Input: the JSON value. Output: the
/// name.
fn kind_name(value: &Value) -> &str {
    value
        .as_str()
        .filter(|kind| {
            !kind.is_empty()
                && kind.len() <= KIND_CHARS_MAX
                && kind
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte == b'_' || byte == b'-')
        })
        .unwrap_or("other")
}
