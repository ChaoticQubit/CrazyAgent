#![cfg(unix)]

mod e2e_support;

use crazyagent::harnesses::drivers::jsonl_turn::{Config, JsonlTurn};
use crazyagent::harnesses::launch::LaunchEvent;
use crazyagent::usage::capture::Tokens;
use e2e_support::harness_standin::{self, Ended, Standin};

const SESSION: &str = r#"{"type":"session","id":"s-77"}"#;
const TEXT: &str = r#"{"type":"text","delta":"hi"}"#;
const TOOL: &str = r#"{"type":"tool","name":"Read"}"#;
const DONE: &str = r#"{"type":"done","ok":true,"usage":{"input":12,"output":3}}"#;
const DEBUG: &str = r#"{"type":"debug","note":"no rule reads this"}"#;

/// Joins lines, each with its newline. Input: the lines. Output: the text.
fn lines(items: &[&str]) -> String {
    items.iter().map(|line| format!("{line}\n")).collect()
}

/// Starts a conversation on the stand-in's own manifest, its home as the working folder.
/// Inputs: the stand-in and the tools switch. Output: the conversation.
fn begin(standin: &Standin, tools: bool) -> JsonlTurn {
    let config = Config::read(&harness_standin::manifest("jsonl-turn")).unwrap();
    let home = standin.home();
    let (turns, ready) = JsonlTurn::new(config, &home, &home, Some("m-1"), tools, None).unwrap();
    assert_eq!(
        ready,
        [LaunchEvent::Ready {
            native_session: None
        }]
    );
    turns
}

/// Feeds a finished turn's output to the conversation and ends the turn by its exit.
/// Inputs: the conversation and the stand-in's end. Output: the turn's events.
fn read_turn(turns: &mut JsonlTurn, ended: &Ended) -> Vec<LaunchEvent> {
    let mut events = Vec::new();
    for line in ended.stdout.split_inclusive(|byte| *byte == b'\n') {
        events.extend(turns.receive(line).unwrap());
    }
    events.extend(turns.exited(ended.status.code()).unwrap());
    events
}

/// DEC-180 Task 11, on the Task 9 stand-in: one process per turn. The first turn's output
/// gives its facts by the manifest's rules, passing over a line that is not JSON and one no
/// rule reads; the session id it reports reaches the second turn's arguments.
#[test]
fn a_jsonl_turn_harness_runs_one_process_per_turn_and_resumes_its_session() {
    let standin = Standin::new("jsonl-turn");
    standin.put(
        "turn-1.out",
        lines(&[SESSION, "progress 50%", TEXT, DEBUG, TOOL, DONE]),
    );
    standin.put("turn-2.out", lines(&[TEXT]));
    let mut turns = begin(&standin, true);

    let first = turns.start("first").unwrap();
    let ended = standin.run(&first, b"");
    assert_eq!(ended.status.code(), Some(0));
    assert_eq!(
        read_turn(&mut turns, &ended),
        [
            LaunchEvent::Text { text: "hi".into() },
            LaunchEvent::ToolUse {
                name: "Read".into()
            },
            LaunchEvent::Usage {
                tokens: Tokens {
                    input: 12,
                    output: 3,
                    cache_read: 0,
                    cache_write: 0,
                },
            },
            LaunchEvent::TurnFinished { success: true },
        ]
    );
    assert_eq!(turns.session(), Some("s-77"));
    assert_eq!(standin.argv(1), first);
    assert_eq!(standin.log("prompt-1"), "first");

    let second = turns.start("second").unwrap();
    assert_eq!(second[second.len() - 2..], ["--resume", "s-77"]);
    let ended = standin.run(&second, b"");
    assert_eq!(
        read_turn(&mut turns, &ended),
        [
            LaunchEvent::Text { text: "hi".into() },
            LaunchEvent::TurnFinished { success: true },
        ]
    );
    assert_eq!(standin.argv(2), second);
    assert_eq!(standin.log("session-2"), "s-77\n");
    assert_eq!(standin.log("prompt-2"), "second");
}

/// DEC-180 Task 11, on the Task 9 stand-in: a turn whose process exits with a code other
/// than 0 ends with a turn error and an unsuccessful end, after the text it gave.
#[test]
fn a_jsonl_turn_that_exits_with_an_error_code_ends_with_a_turn_error() {
    let standin = Standin::new("jsonl-turn");
    standin.put("turn.out", lines(&[TEXT]));
    standin.put("turn.exit", "3\n");
    let mut turns = begin(&standin, true);
    let argv = turns.start("fail").unwrap();
    let ended = standin.run(&argv, b"");
    assert_eq!(ended.status.code(), Some(3));
    assert_eq!(
        read_turn(&mut turns, &ended),
        [
            LaunchEvent::Text { text: "hi".into() },
            LaunchEvent::TurnError {
                rate_limited: false
            },
            LaunchEvent::TurnFinished { success: false },
        ]
    );
}

/// DEC-180 Task 11, on the Task 9 stand-in: an interrupted turn's process is stopped by a
/// signal to its group, and the turn ends interrupted with nothing it had left to say.
#[test]
fn an_interrupted_jsonl_turn_is_stopped_by_a_signal_and_ends_interrupted() {
    let standin = Standin::new("jsonl-turn");
    standin.put(
        "turn.out",
        lines(&[
            TEXT,
            "@standin sleep 30",
            r#"{"type":"text","delta":"never"}"#,
        ]),
    );
    let mut turns = begin(&standin, true);
    let argv = turns.start("long job").unwrap();
    let mut live = standin.start(&argv);
    let first = live.line().expect("the turn's first line");
    assert_eq!(
        turns.receive(first.as_bytes()).unwrap(),
        [LaunchEvent::Text { text: "hi".into() }]
    );
    turns.interrupt().unwrap();
    assert!(live.signal("TERM"));
    let ended = live.finish();
    assert_eq!(ended.status.code(), None);
    let events = read_turn(&mut turns, &ended);
    assert_eq!(
        events,
        [
            LaunchEvent::Interrupted,
            LaunchEvent::TurnFinished { success: false },
        ]
    );
}

/// DEC-180 Task 11, on the Task 9 stand-in: with tools off each turn's arguments end with the
/// manifest's `tools_off` switch, as the stand-in logged them.
#[test]
fn a_jsonl_turn_with_tools_off_passes_the_manifest_switch() {
    let standin = Standin::new("jsonl-turn");
    standin.put("turn.out", lines(&[DONE]));
    let mut turns = begin(&standin, false);
    let argv = turns.start("no tools").unwrap();
    assert_eq!(argv.last().map(String::as_str), Some("--no-tools"));
    let ended = standin.run(&argv, b"");
    assert_eq!(ended.status.code(), Some(0));
    assert_eq!(standin.argv(1), argv);
}
