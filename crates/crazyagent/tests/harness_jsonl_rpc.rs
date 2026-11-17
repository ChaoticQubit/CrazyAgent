#![cfg(unix)]

mod e2e_support;

use crazyagent::harnesses::drivers::jsonl_rpc::{Config, JsonlRpc};
use crazyagent::harnesses::drivers::jsonl_turn::Phase;
use crazyagent::harnesses::launch::LaunchEvent;
use e2e_support::harness_standin::{self, Live, Standin};
use serde_json::Value;

const SESSION: &str = r#"{"type":"session","id":"s-77"}"#;
const TEXT: &str = r#"{"type":"text","delta":"hi"}"#;
const TOOL: &str = r#"{"type":"tool","name":"Bash"}"#;
const DONE: &str = r#"{"type":"done","ok":true}"#;
const STOPPED: &str = r#"{"type":"stopped"}"#;
const NEVER: &str = r#"{"type":"text","delta":"never"}"#;

/// Joins lines, each with its newline. Input: the lines. Output: the text.
fn lines(items: &[&str]) -> String {
    items.iter().map(|line| format!("{line}\n")).collect()
}

/// One jsonl-rpc conversation driving a running stand-in: what the driver frames goes to
/// the stand-in's stdin, and each line the stand-in prints goes to the driver.
struct Client {
    live: Live,
    rpc: JsonlRpc,
    events: Vec<LaunchEvent>,
}

impl Client {
    /// Starts the stand-in with the driver's arguments, on the stand-in's own manifest, its
    /// home as the working folder. Inputs: the stand-in, the tools switch and the session to
    /// continue. Output: the client, its `Ready` taken.
    fn start(standin: &Standin, tools: bool, resume: Option<&str>) -> Self {
        let config = Config::read(&harness_standin::manifest("jsonl-rpc")).unwrap();
        let home = standin.home();
        let (rpc, ready) = JsonlRpc::new(config, &home, &home, Some("m-1"), tools, resume).unwrap();
        assert_eq!(
            ready,
            [LaunchEvent::Ready {
                native_session: resume.map(str::to_owned)
            }]
        );
        let live = standin.start(rpc.argv());
        Self {
            live,
            rpc,
            events: Vec::new(),
        }
    }

    /// Feeds the stand-in's lines to the driver and writes back what it answers until the
    /// events so far hold `count` turn ends. Input: the count. Output: false when the
    /// stand-in's output ended first.
    fn until_finished(&mut self, count: usize) -> bool {
        while finished(&self.events) < count {
            let Some(line) = self.live.line() else {
                return false;
            };
            let (events, writes) = self.rpc.receive(line.as_bytes()).unwrap();
            for bytes in writes {
                self.live.write(&bytes);
            }
            self.events.extend(events);
        }
        true
    }

    /// Reads lines until the last event is `event`. Input: the event.
    fn until_event(&mut self, event: &LaunchEvent) {
        while self.events.last() != Some(event) {
            let line = self.live.line().expect("the stand-in's output ended early");
            let (events, writes) = self.rpc.receive(line.as_bytes()).unwrap();
            assert!(writes.is_empty());
            self.events.extend(events);
        }
    }

    /// Sends one prompt. Input: the prompt.
    fn prompt(&mut self, text: &str) {
        let frame = self.rpc.prompt(text).unwrap();
        self.live.write(&frame);
    }
}

fn finished(events: &[LaunchEvent]) -> usize {
    events
        .iter()
        .filter(|event| matches!(event, LaunchEvent::TurnFinished { .. }))
        .count()
}

fn text() -> LaunchEvent {
    LaunchEvent::Text { text: "hi".into() }
}

/// The prompts the stand-in read on its first run, from each frame's `message`.
fn prompts(standin: &Standin) -> Vec<String> {
    standin
        .log("stdin-1")
        .lines()
        .filter_map(|line| {
            let value: Value = serde_json::from_str(line).ok()?;
            (value["type"] == "prompt").then(|| value["message"].as_str().unwrap().to_owned())
        })
        .collect()
}

/// DEC-180 Task 12, on the Task 9 stand-in: one long-lived process takes each prompt as a
/// framed JSON line; the session id its first line reports is kept, and two turns end by
/// their finished lines with the process still running.
#[test]
fn a_jsonl_rpc_harness_runs_two_turns_in_one_process() {
    let standin = Standin::new("jsonl-rpc");
    standin.put("ready.out", lines(&[SESSION]));
    standin.put("turn-1.out", lines(&[TEXT, DONE]));
    standin.put("turn-2.out", lines(&[TEXT, DONE]));
    let mut client = Client::start(&standin, true, None);
    client.prompt("first \"one\"");
    assert!(client.until_finished(1));
    assert_eq!(client.rpc.session(), Some("s-77"));
    client.prompt("second\nline");
    assert!(client.until_finished(2));
    assert_eq!(
        client.events,
        [
            text(),
            LaunchEvent::TurnFinished { success: true },
            text(),
            LaunchEvent::TurnFinished { success: true },
        ]
    );
    assert_eq!(client.rpc.phase(), Phase::Idle);
    client.live.close();
    let ended = client.live.finish();
    assert_eq!(ended.status.code(), Some(0));
    assert_eq!(prompts(&standin), ["first \"one\"", "second\nline"]);
    assert_eq!(standin.log("runs"), "1\n");
}

/// DEC-180 Task 12, on the Task 9 stand-in: an interrupt mid-turn writes the interrupt line,
/// the stand-in stops the turn before its last line, and the turn ends as interrupted.
#[test]
fn a_jsonl_rpc_turn_interrupted_mid_turn_ends_interrupted() {
    let standin = Standin::new("jsonl-rpc");
    standin.put("turn-1.out", lines(&[TEXT, "@standin wait", NEVER]));
    standin.put("interrupt.match", "\"abort\"");
    standin.put("interrupt.out", lines(&[STOPPED]));
    let mut client = Client::start(&standin, true, None);
    client.prompt("long job");
    client.until_event(&text());
    let abort = client.rpc.interrupt().unwrap();
    client.live.write(&abort);
    assert!(client.until_finished(1));
    assert_eq!(
        client.events,
        [
            text(),
            LaunchEvent::Interrupted,
            LaunchEvent::TurnFinished { success: false },
        ]
    );
    client.live.close();
    let ended = client.live.finish();
    assert_eq!(ended.status.code(), Some(0));
    assert!(!String::from_utf8_lossy(&ended.stdout).contains("never"));
}

/// DEC-180 Task 12, on the Task 9 stand-in: a process that dies mid-turn leaves the turn
/// unfinished, with only the text it gave before; the launch session then reports the exit,
/// as it does for every driver.
#[test]
fn a_jsonl_rpc_harness_that_dies_mid_turn_leaves_the_turn_unfinished() {
    let standin = Standin::new("jsonl-rpc");
    standin.put("turn-1.out", lines(&[TEXT, "@standin exit 9", NEVER]));
    let mut client = Client::start(&standin, true, None);
    client.prompt("die");
    assert!(!client.until_finished(1));
    assert_eq!(client.events, [text()]);
    assert_eq!(client.rpc.phase(), Phase::Running);
    let ended = client.live.finish();
    assert_eq!(ended.status.code(), Some(9));
    assert!(!ended.killed);
}

/// DEC-180 Task 12 and rv-sa241 L1, on the Task 9 stand-in: with tools off and a session to
/// continue, the process starts with the resume and tools-off arguments; a tool it reports
/// is a breach whose interrupt line the client writes back, the turn the stand-in then stops
/// ends unsuccessfully with no `Interrupted`, and the process takes no further turn.
#[test]
fn a_jsonl_rpc_breach_of_tools_off_writes_the_interrupt_and_ends_the_turn() {
    let standin = Standin::new("jsonl-rpc");
    standin.put("turn-1.out", lines(&[TOOL, "@standin wait", NEVER]));
    standin.put("interrupt.match", "\"abort\"");
    standin.put("interrupt.out", lines(&[STOPPED]));
    let mut client = Client::start(&standin, false, Some("s-5"));
    let argv = client.rpc.argv().to_vec();
    assert_eq!(argv[argv.len() - 3..], ["--session", "s-5", "--no-tools"]);
    client.prompt("use a tool");
    assert!(client.until_finished(1));
    assert_eq!(
        client.events,
        [
            LaunchEvent::ApprovalDenied,
            LaunchEvent::TurnFinished { success: false },
        ]
    );
    assert!(!client.rpc.breached());
    assert_eq!(client.rpc.phase(), Phase::Failed);
    assert_eq!(
        client.rpc.prompt("again").unwrap_err().0,
        "launch_protocol_failed"
    );
    client.live.close();
    let ended = client.live.finish();
    assert_eq!(ended.status.code(), Some(0));
    assert_eq!(standin.argv(1), argv);
    assert_eq!(standin.log("session-1"), "s-5\n");
    assert!(standin.log("stdin-1").ends_with("{\"type\":\"abort\"}\n"));
}
