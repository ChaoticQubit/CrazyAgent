#![cfg(unix)]

mod e2e_support;

use crazyagent::harnesses::drivers::acp::Acp;
use crazyagent::harnesses::launch::LaunchEvent;
use e2e_support::harness_standin::{Live, Standin};
use serde_json::Value;

const SESSION: &str = "sess_standin";
const CHUNK: &str = r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"sess_standin","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"hi"}}}}"#;
const TOOL: &str = r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"sess_standin","update":{"sessionUpdate":"tool_call","toolCallId":"call_1","title":"Read","status":"pending"}}}"#;
const USAGE: &str = r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"sess_standin","update":{"sessionUpdate":"usage_update","used":120,"size":200000}}}"#;
const PERMISSION: &str = r#"{"jsonrpc":"2.0","id":"perm-1","method":"session/request_permission","params":{"sessionId":"sess_standin","toolCall":{"toolCallId":"call_1"},"options":[{"optionId":"allow-always","name":"Always","kind":"allow_always"},{"optionId":"allow-once","name":"Allow","kind":"allow_once"}]}}"#;
const TOOL_DONE: &str = r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"sess_standin","update":{"sessionUpdate":"tool_call_update","toolCallId":"call_1","status":"completed"}}}"#;
const NEVER: &str = r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"sess_standin","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"never"}}}}"#;
const HISTORY: &str = r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"sess_standin","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"old reply"}}}}"#;
const AUTH_REQUIRED: &str = r#"{"code":-32000,"message":"Authentication required"}"#;

/// Joins lines, each with its newline. Input: the lines. Output: the text.
fn lines(items: &[&str]) -> String {
    items.iter().map(|line| format!("{line}\n")).collect()
}

/// The JSON-RPC methods the client wrote to the stand-in on its first run, in order.
fn methods(standin: &Standin) -> Vec<String> {
    standin
        .log("stdin-1")
        .lines()
        .filter_map(|line| {
            let value: Value = serde_json::from_str(line).ok()?;
            value.get("method")?.as_str().map(str::to_owned)
        })
        .collect()
}

/// One ACP client driving a running stand-in: what the client writes goes to the stand-in's
/// stdin, and each line the stand-in prints goes to the client.
struct Client {
    live: Live,
    acp: Acp,
    events: Vec<LaunchEvent>,
}

impl Client {
    /// Starts the stand-in and writes the client's `initialize`. Inputs: the stand-in, the
    /// tools switch and the session to continue. Output: the client.
    fn start(standin: &Standin, tools: bool, resume: Option<&str>) -> Self {
        let mut live = standin.start(&standin.at_home(&[]));
        let (acp, first) = Acp::new(&standin.home(), tools, resume).unwrap();
        live.write(&first);
        Self {
            live,
            acp,
            events: Vec::new(),
        }
    }

    /// Feeds the stand-in's lines to the client and writes back what it answers until the
    /// events so far hold `done`. Input: the condition. Output: false when the stand-in's
    /// output ended first.
    fn until(&mut self, done: impl Fn(&[LaunchEvent]) -> bool) -> bool {
        while !done(&self.events) {
            let Some(line) = self.live.line() else {
                return false;
            };
            let (events, writes) = self.acp.receive(line.as_bytes()).unwrap();
            for bytes in writes {
                self.live.write(&bytes);
            }
            self.events.extend(events);
        }
        true
    }

    /// Sends one prompt, or holds it until the session is open. Input: the prompt.
    fn prompt(&mut self, text: &str) {
        if let Some(bytes) = self.acp.prompt(text).unwrap() {
            self.live.write(&bytes);
        }
    }
}

fn finished(events: &[LaunchEvent]) -> usize {
    events
        .iter()
        .filter(|event| matches!(event, LaunchEvent::TurnFinished { .. }))
        .count()
}

fn ready() -> LaunchEvent {
    LaunchEvent::Ready {
        native_session: Some(SESSION.into()),
    }
}

/// DEC-180 Task 10, on the Task 9 stand-in: an ACP agent starts, opens a session, and one
/// turn gives its text and its tool; its usage update gives no event (QUESTION-16:32 Q2).
#[test]
fn an_acp_agent_starts_and_runs_one_turn_with_a_tool_and_a_usage_update() {
    let standin = Standin::new("acp");
    standin.put("session-id", SESSION);
    standin.put("turn-1.out", lines(&[CHUNK, TOOL, USAGE]));
    let mut client = Client::start(&standin, true, None);
    client.prompt("say hi");
    assert!(client.until(|events| finished(events) == 1));
    assert_eq!(
        client.events,
        [
            ready(),
            LaunchEvent::Text { text: "hi".into() },
            LaunchEvent::ToolUse {
                name: "Read".into()
            },
            LaunchEvent::TurnFinished { success: true },
        ]
    );
    assert_eq!(client.acp.session(), Some(SESSION));
    client.live.close();
    let ended = client.live.finish();
    assert_eq!(ended.status.code(), Some(0));
    assert_eq!(
        methods(&standin),
        ["initialize", "session/new", "session/prompt"]
    );
}

/// DEC-180 Task 10, on the Task 9 stand-in: a permission asked mid-turn is answered allow
/// once with tools on, never allow always, and the turn goes on; a turn cancelled while the
/// agent works ends as interrupted, and nothing the agent had left to say comes after.
#[test]
fn an_acp_agent_asks_permission_and_a_cancelled_turn_ends_interrupted() {
    let standin = Standin::new("acp");
    standin.put("session-id", SESSION);
    standin.put(
        "turn-1.out",
        lines(&[TOOL, PERMISSION, "@standin wait", TOOL_DONE]),
    );
    standin.put("turn-2.out", lines(&[CHUNK, "@standin wait", NEVER]));
    let mut client = Client::start(&standin, true, None);
    client.prompt("use a tool");
    assert!(client.until(|events| finished(events) == 1));
    assert_eq!(
        client.events[1..],
        [
            LaunchEvent::ToolUse {
                name: "Read".into()
            },
            LaunchEvent::TurnFinished { success: true },
        ]
    );
    let answer = standin
        .log("stdin-1")
        .lines()
        .find(|line| line.contains("perm-1"))
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .expect("the permission was answered");
    assert_eq!(answer["result"]["outcome"]["optionId"], "allow-once");

    client.prompt("cancel me");
    assert!(client.until(|events| events.last() == Some(&LaunchEvent::Text { text: "hi".into() })));
    let cancel = client.acp.cancel().unwrap().unwrap();
    client.live.write(&cancel);
    assert!(client.until(|events| finished(events) == 2));
    assert_eq!(
        client.events[3..],
        [
            LaunchEvent::Text { text: "hi".into() },
            LaunchEvent::Interrupted,
            LaunchEvent::TurnFinished { success: false },
        ]
    );
    client.live.close();
    let ended = client.live.finish();
    assert_eq!(ended.status.code(), Some(0));
    assert!(!String::from_utf8_lossy(&ended.stdout).contains("never"));
}

/// DEC-180 Task 10, on the Task 9 stand-in: an agent that answers `auth_required` when the
/// session opens is refused as no compatible account, and no prompt is sent to it.
#[test]
fn an_acp_agent_that_needs_a_sign_in_is_refused_as_no_compatible_account() {
    let standin = Standin::new("acp");
    standin.put("acp-session-new.error", AUTH_REQUIRED);
    let mut client = Client::start(&standin, true, None);
    client.prompt("never sent");
    let initialized = client.live.line().unwrap();
    let (_, writes) = client.acp.receive(initialized.as_bytes()).unwrap();
    for bytes in writes {
        client.live.write(&bytes);
    }
    let refused = client.live.line().unwrap();
    let error = client.acp.receive(refused.as_bytes()).unwrap_err();
    assert_eq!(error.0, "launch_account_unsupported");
    client.live.close();
    let ended = client.live.finish();
    assert_eq!(ended.status.code(), Some(0));
    assert_eq!(methods(&standin), ["initialize", "session/new"]);
}

/// DEC-180 Task 10, on the Task 9 stand-in: a session continued on an agent that offers
/// `loadSession` is loaded, and the history the agent replays gives no turn event.
#[test]
fn an_acp_agent_loads_a_session_and_its_replayed_history_makes_no_turn_event() {
    let standin = Standin::new("acp");
    standin.put("acp-session-load.out", lines(&[HISTORY, TOOL]));
    let mut client = Client::start(&standin, true, Some(SESSION));
    assert!(client.until(|events| !events.is_empty()));
    assert_eq!(client.events, [ready()]);
    client.live.close();
    let ended = client.live.finish();
    assert_eq!(ended.status.code(), Some(0));
    assert_eq!(methods(&standin), ["initialize", "session/load"]);
}

/// DEC-180 Task 10, on the Task 9 stand-in: an agent that dies mid-turn leaves the turn
/// unfinished, with only the text it gave before; the launch session then reports the exit,
/// as it does for every driver.
#[test]
fn an_acp_agent_that_dies_mid_turn_leaves_the_turn_unfinished() {
    let standin = Standin::new("acp");
    standin.put("session-id", SESSION);
    standin.put("turn-1.out", lines(&[CHUNK, "@standin exit 9", NEVER]));
    let mut client = Client::start(&standin, true, None);
    client.prompt("die");
    assert!(!client.until(|events| finished(events) == 1));
    assert_eq!(
        client.events,
        [ready(), LaunchEvent::Text { text: "hi".into() }]
    );
    let ended = client.live.finish();
    assert_eq!(ended.status.code(), Some(9));
    assert!(!ended.killed);
}
