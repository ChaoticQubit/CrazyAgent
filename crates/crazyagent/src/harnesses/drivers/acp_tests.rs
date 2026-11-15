use super::*;
use crate::adapters::{FIELD_BYTES_MAX, LINE_BYTES_MAX};
use serde_json::{Value, json};
use std::path::Path;

const CWD: &str = "/work/project";
const SESSION: &str = "sess_1";

fn frame_of(value: Value) -> Vec<u8> {
    let mut bytes = serde_json::to_vec(&value).unwrap();
    bytes.push(b'\n');
    bytes
}

fn parsed(bytes: &[u8]) -> Value {
    assert_eq!(bytes.last(), Some(&b'\n'), "every frame ends its line");
    assert_eq!(bytes.iter().filter(|byte| **byte == b'\n').count(), 1);
    serde_json::from_slice(&bytes[..bytes.len() - 1]).unwrap()
}

fn reply(id: u64, result: Value) -> Vec<u8> {
    frame_of(json!({"jsonrpc": "2.0", "id": id, "result": result}))
}

fn refusal(id: u64, code: i64, message: &str) -> Vec<u8> {
    frame_of(json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}}))
}

fn update(session: &str, update: Value) -> Vec<u8> {
    frame_of(json!({
        "jsonrpc": "2.0",
        "method": "session/update",
        "params": {"sessionId": session, "update": update}
    }))
}

fn chunk(text: &str) -> Value {
    json!({"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": text}})
}

fn initialized(capabilities: Value) -> Vec<u8> {
    reply(
        0,
        json!({"protocolVersion": 1, "agentCapabilities": capabilities, "authMethods": []}),
    )
}

fn permission(id: Value, kinds: &[&str]) -> Vec<u8> {
    let options: Vec<Value> = kinds
        .iter()
        .map(|kind| json!({"optionId": kind.replace('_', "-"), "name": kind, "kind": kind}))
        .collect();
    frame_of(json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "session/request_permission",
        "params": {"sessionId": SESSION, "toolCall": {"toolCallId": "call_1"}, "options": options}
    }))
}

fn code(result: Result<impl std::fmt::Debug, LaunchError>) -> String {
    result.unwrap_err().0
}

fn events_of(acp: &mut Acp, line: &[u8]) -> Vec<LaunchEvent> {
    let (events, writes) = acp.receive(line).unwrap();
    assert!(writes.is_empty(), "{writes:?}");
    events
}

fn none(events: Vec<LaunchEvent>) {
    assert!(events.is_empty(), "{events:?}");
}

/// A conversation whose session `sess_1` is open: `initialize` and `session/new` answered.
fn ready(tools: bool) -> Acp {
    let (mut acp, start) = Acp::new(Path::new(CWD), tools, None).unwrap();
    assert_eq!(parsed(&start)["method"], "initialize");
    let (events, writes) = acp.receive(&initialized(json!({}))).unwrap();
    assert!(events.is_empty());
    assert_eq!(writes.len(), 1);
    let (events, writes) = acp
        .receive(&reply(1, json!({"sessionId": SESSION})))
        .unwrap();
    assert_eq!(
        events,
        [LaunchEvent::Ready {
            native_session: Some(SESSION.into())
        }]
    );
    assert!(writes.is_empty());
    assert_eq!(acp.phase(), Phase::Idle);
    acp
}

/// A conversation in its first turn, prompt id 2.
fn prompting(tools: bool) -> Acp {
    let mut acp = ready(tools);
    let sent = acp.prompt("hello").unwrap().unwrap();
    assert_eq!(parsed(&sent)["id"], 2);
    assert_eq!(acp.phase(), Phase::Prompting);
    acp
}

/// DEC-180 Task 10: the client starts with `initialize`, offering no file or terminal
/// service, then opens a new session in the working folder with no MCP server, and the
/// session id the agent names is the launch's native session.
#[test]
fn the_client_starts_with_initialize_then_opens_a_new_session_in_the_working_folder() {
    let (mut acp, start) = Acp::new(Path::new(CWD), true, None).unwrap();
    assert_eq!(
        parsed(&start),
        json!({
            "jsonrpc": "2.0",
            "id": 0,
            "method": "initialize",
            "params": {
                "protocolVersion": 1,
                "clientCapabilities": {
                    "fs": {"readTextFile": false, "writeTextFile": false},
                    "terminal": false
                }
            }
        })
    );
    assert_eq!(acp.phase(), Phase::Initializing);
    assert_eq!(acp.session(), None);
    let (events, writes) = acp
        .receive(&initialized(json!({"loadSession": true})))
        .unwrap();
    assert!(events.is_empty());
    assert_eq!(
        writes.iter().map(|sent| parsed(sent)).collect::<Vec<_>>(),
        [json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "session/new",
            "params": {"cwd": CWD, "mcpServers": []}
        })]
    );
    assert_eq!(acp.phase(), Phase::Opening);
    assert_eq!(
        events_of(&mut acp, &reply(1, json!({"sessionId": SESSION}))),
        [LaunchEvent::Ready {
            native_session: Some(SESSION.into())
        }]
    );
    assert_eq!(acp.session(), Some(SESSION));

    assert_eq!(
        code(Acp::new(Path::new("work/project"), true, None)),
        "launch_working_dir_unavailable"
    );
    let (mut newer, _) = Acp::new(Path::new(CWD), true, None).unwrap();
    assert_eq!(
        code(newer.receive(&reply(
            0,
            json!({"protocolVersion": 2, "agentCapabilities": {}})
        ))),
        "launch_protocol_failed"
    );
    let (mut refused, _) = Acp::new(Path::new(CWD), true, None).unwrap();
    assert_eq!(
        code(refused.receive(&refusal(0, -32603, "Internal error"))),
        "launch_request_failed"
    );
    let (mut nameless, _) = Acp::new(Path::new(CWD), true, None).unwrap();
    nameless.receive(&initialized(json!({}))).unwrap();
    assert_eq!(
        code(nameless.receive(&reply(1, json!({"sessionId": ""})))),
        "launch_protocol_failed"
    );
}

/// DEC-180 Task 10: a prompt sent before the session opens is held and sent the moment it
/// opens; one turn streams the agent's text and ends with `end_turn`; a second prompt during
/// a turn, or a second held one, is busy.
#[test]
fn a_prompt_sent_before_the_session_opens_is_held_and_one_turn_streams_text_to_its_end() {
    let (mut acp, _) = Acp::new(Path::new(CWD), true, None).unwrap();
    assert_eq!(acp.prompt("say hi").unwrap(), None);
    assert_eq!(code(acp.prompt("again")), "launch_busy");
    acp.receive(&initialized(json!({}))).unwrap();
    let (events, writes) = acp
        .receive(&reply(1, json!({"sessionId": SESSION})))
        .unwrap();
    assert_eq!(
        events,
        [LaunchEvent::Ready {
            native_session: Some(SESSION.into())
        }]
    );
    assert_eq!(
        writes.iter().map(|sent| parsed(sent)).collect::<Vec<_>>(),
        [json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "session/prompt",
            "params": {"sessionId": SESSION, "prompt": [{"type": "text", "text": "say hi"}]}
        })]
    );
    assert_eq!(acp.phase(), Phase::Prompting);
    assert_eq!(code(acp.prompt("during")), "launch_busy");
    assert_eq!(
        events_of(&mut acp, &update(SESSION, chunk("hi"))),
        [LaunchEvent::Text { text: "hi".into() }]
    );
    none(events_of(&mut acp, &update(SESSION, chunk(""))));
    assert_eq!(
        events_of(&mut acp, &reply(2, json!({"stopReason": "end_turn"}))),
        [LaunchEvent::TurnFinished { success: true }]
    );
    assert_eq!(acp.phase(), Phase::Idle);
    let next = acp.prompt("next").unwrap().unwrap();
    assert_eq!(parsed(&next)["id"], 3);
}

/// DEC-180 Task 10: a tool call names its tool by its title; its later updates, and a tool
/// call with no title, add no event.
#[test]
fn a_tool_call_names_its_tool_and_its_later_updates_add_nothing() {
    let mut acp = prompting(true);
    let call = json!({
        "sessionUpdate": "tool_call",
        "toolCallId": "call_1",
        "title": "Read",
        "status": "pending"
    });
    assert_eq!(
        events_of(&mut acp, &update(SESSION, call)),
        [LaunchEvent::ToolUse {
            name: "Read".into()
        }]
    );
    let later = json!({
        "sessionUpdate": "tool_call_update",
        "toolCallId": "call_1",
        "status": "completed"
    });
    none(events_of(&mut acp, &update(SESSION, later)));
    let untitled = json!({"sessionUpdate": "tool_call", "toolCallId": "call_2"});
    none(events_of(&mut acp, &update(SESSION, untitled)));
}

/// DEC-180 Task 10, QUESTION-16:32 Q2: a `usage_update` says how full the context is, not
/// what the turn spent, so it is read and gives no usage event; an ACP run's usage reads
/// "not reported".
#[test]
fn a_usage_update_is_read_but_counts_no_spend() {
    let mut acp = prompting(true);
    let usage = json!({
        "sessionUpdate": "usage_update",
        "used": 53000,
        "size": 200000,
        "cost": {"amount": 0.045, "currency": "USD"}
    });
    none(events_of(&mut acp, &update(SESSION, usage)));
    let finished = events_of(&mut acp, &reply(2, json!({"stopReason": "end_turn"})));
    assert_eq!(finished, [LaunchEvent::TurnFinished { success: true }]);
    assert!(
        !finished
            .iter()
            .any(|event| matches!(event, LaunchEvent::Usage { .. }))
    );
}

fn answered(acp: &mut Acp, request: &[u8]) -> (Value, Vec<LaunchEvent>) {
    let (events, writes) = acp.receive(request).unwrap();
    assert_eq!(writes.len(), 1, "one answer per request");
    (parsed(&writes[0]), events)
}

/// DEC-180 Task 10, QUESTION-16:32 Q3: with tools on a permission is allowed once, with tools
/// off it is rejected once and the turn notes the denial, and the agent's request id comes
/// back as it was sent, text or number.
#[test]
fn a_permission_is_allowed_once_with_tools_on_and_rejected_with_tools_off() {
    let kinds = ["allow_always", "allow_once", "reject_once", "reject_always"];
    let mut on = prompting(true);
    let (answer, events) = answered(&mut on, &permission(json!("perm-1"), &kinds));
    assert_eq!(
        answer,
        json!({
            "jsonrpc": "2.0",
            "id": "perm-1",
            "result": {"outcome": {"outcome": "selected", "optionId": "allow-once"}}
        })
    );
    none(events);

    let mut off = prompting(false);
    let (answer, events) = answered(&mut off, &permission(json!(7), &kinds));
    assert_eq!(answer["id"], json!(7));
    assert_eq!(
        answer["result"]["outcome"],
        json!({"outcome": "selected", "optionId": "reject-once"})
    );
    assert_eq!(events, [LaunchEvent::ApprovalDenied]);
}

/// DEC-180 Task 10, QUESTION-16:32 Q3: the client never picks `allow_always`, so the agent
/// remembers no allow for a later tools-off run on the same home; with no option that fits,
/// or after the turn was cancelled, the answer is the `cancelled` outcome; a request id that
/// is neither text nor a number fails the launch.
#[test]
fn a_permission_never_remembers_an_allow_and_is_cancelled_when_no_option_fits() {
    let mut always = prompting(true);
    let (answer, events) = answered(
        &mut always,
        &permission(json!("p"), &["allow_always", "reject_always"]),
    );
    assert_eq!(
        answer["result"]["outcome"],
        json!({"outcome": "selected", "optionId": "reject-always"})
    );
    assert_eq!(events, [LaunchEvent::ApprovalDenied]);

    let (answer, events) = answered(&mut always, &permission(json!("q"), &[]));
    assert_eq!(answer["result"]["outcome"], json!({"outcome": "cancelled"}));
    assert_eq!(events, [LaunchEvent::ApprovalDenied]);

    let (answer, _) = answered(&mut always, &permission(json!("r"), &["allow_always"]));
    assert_eq!(answer["result"]["outcome"], json!({"outcome": "cancelled"}));

    always.cancel().unwrap().unwrap();
    let (answer, events) = answered(&mut always, &permission(json!("s"), &["allow_once"]));
    assert_eq!(answer["result"]["outcome"], json!({"outcome": "cancelled"}));
    assert_eq!(events, [LaunchEvent::ApprovalDenied]);

    let mut odd = prompting(true);
    assert_eq!(
        code(odd.receive(&permission(json!({"id": 1}), &["allow_once"]))),
        "launch_protocol_failed"
    );
    assert_eq!(odd.phase(), Phase::Failed);
}

/// DEC-180 Task 10: cancel sends `session/cancel` for the open session; the agent's
/// `cancelled` stop reason ends the turn as interrupted; cancel with no turn, or twice, is
/// refused; a cancel asked while the prompt is held goes out right after the prompt.
#[test]
fn cancel_sends_session_cancel_and_a_cancelled_stop_reason_ends_the_turn_as_interrupted() {
    let mut acp = prompting(true);
    let sent = acp.cancel().unwrap().unwrap();
    assert_eq!(
        parsed(&sent),
        json!({"jsonrpc": "2.0", "method": "session/cancel", "params": {"sessionId": SESSION}})
    );
    assert_eq!(code(acp.cancel()), "launch_not_running");
    assert_eq!(
        events_of(&mut acp, &update(SESSION, chunk("late"))),
        [LaunchEvent::Text {
            text: "late".into()
        }]
    );
    assert_eq!(
        events_of(&mut acp, &reply(2, json!({"stopReason": "cancelled"}))),
        [
            LaunchEvent::Interrupted,
            LaunchEvent::TurnFinished { success: false }
        ]
    );
    assert_eq!(code(acp.cancel()), "launch_not_running");
    assert_eq!(code(ready(true).cancel()), "launch_not_running");

    let (mut held, _) = Acp::new(Path::new(CWD), true, None).unwrap();
    assert_eq!(code(held.cancel()), "launch_not_running");
    assert_eq!(held.prompt("held").unwrap(), None);
    assert_eq!(held.cancel().unwrap(), None);
    held.receive(&initialized(json!({}))).unwrap();
    let (_, writes) = held
        .receive(&reply(1, json!({"sessionId": SESSION})))
        .unwrap();
    let methods: Vec<Value> = writes
        .iter()
        .map(|sent| parsed(sent)["method"].clone())
        .collect();
    assert_eq!(methods, [json!("session/prompt"), json!("session/cancel")]);
}

/// DEC-180 Task 10: every stop reason other than `end_turn` ends the turn unsuccessfully, an
/// error reply to the prompt is a turn error that says whether the account hit a limit, and
/// a reply with no stop reason fails the launch.
#[test]
fn other_stop_reasons_and_an_error_reply_end_the_turn_unsuccessfully() {
    for reason in [
        "max_tokens",
        "max_turn_requests",
        "refusal",
        "something_new",
    ] {
        let mut acp = prompting(true);
        assert_eq!(
            events_of(&mut acp, &reply(2, json!({"stopReason": reason}))),
            [LaunchEvent::TurnFinished { success: false }],
            "{reason}"
        );
    }
    for (message, limited) in [
        ("Rate limit reached for requests", true),
        ("Bad things", false),
    ] {
        let mut acp = prompting(true);
        assert_eq!(
            events_of(&mut acp, &refusal(2, -32603, message)),
            [
                LaunchEvent::TurnError {
                    rate_limited: limited
                },
                LaunchEvent::TurnFinished { success: false }
            ],
            "{message}"
        );
        assert_eq!(acp.phase(), Phase::Idle);
    }
    let mut bare = prompting(true);
    assert_eq!(
        code(bare.receive(&reply(2, json!({})))),
        "launch_protocol_failed"
    );
}

/// DEC-180 Task 10: a session to continue is resumed with `session/resume` when the agent
/// offers it, else loaded with `session/load`, whose replayed history gives no turn event; an
/// agent that offers neither refuses the resume, and a resume id that is not plain is refused
/// before anything is sent.
#[test]
fn a_resume_uses_session_resume_or_load_as_the_agent_offers_and_ignores_the_replayed_history() {
    let old = "sess_old";
    let (mut resumed, _) = Acp::new(Path::new(CWD), true, Some(old)).unwrap();
    let (_, writes) = resumed
        .receive(&initialized(
            json!({"loadSession": true, "sessionCapabilities": {"resume": {}}}),
        ))
        .unwrap();
    assert_eq!(
        parsed(&writes[0]),
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "session/resume",
            "params": {"cwd": CWD, "mcpServers": [], "sessionId": old}
        })
    );
    assert_eq!(
        events_of(&mut resumed, &reply(1, json!({}))),
        [LaunchEvent::Ready {
            native_session: Some(old.into())
        }]
    );

    let (mut loaded, _) = Acp::new(Path::new(CWD), true, Some(old)).unwrap();
    let (_, writes) = loaded
        .receive(&initialized(json!({"loadSession": true})))
        .unwrap();
    assert_eq!(parsed(&writes[0])["method"], "session/load");
    for history in [
        json!({"sessionUpdate": "user_message_chunk", "content": {"type": "text", "text": "old"}}),
        chunk("old reply"),
        json!({"sessionUpdate": "tool_call", "toolCallId": "old_call", "title": "Read"}),
    ] {
        none(events_of(&mut loaded, &update(old, history)));
    }
    assert_eq!(
        events_of(&mut loaded, &reply(1, json!({}))),
        [LaunchEvent::Ready {
            native_session: Some(old.into())
        }]
    );
    assert_eq!(loaded.session(), Some(old));

    let (mut neither, _) = Acp::new(Path::new(CWD), true, Some(old)).unwrap();
    assert_eq!(
        code(neither.receive(&initialized(json!({"loadSession": false})))),
        "launch_resume_unsupported"
    );
    let long = "x".repeat(ID_BYTES_MAX + 1);
    for bad in ["", "line\nbreak", long.as_str()] {
        assert_eq!(
            code(Acp::new(Path::new(CWD), true, Some(bad))),
            "launch_identity",
            "{bad:?}"
        );
    }
}

/// DEC-180 Task 10: an agent that answers `auth_required` when the session opens needs a
/// sign-in of its own, which the engine never gives, so the launch is refused as no
/// compatible account; another refusal is a failed request; either way the conversation
/// stays failed.
#[test]
fn an_agent_that_asks_for_a_sign_in_is_refused_as_no_compatible_account() {
    for resume in [None, Some("sess_old")] {
        let (mut acp, _) = Acp::new(Path::new(CWD), true, resume).unwrap();
        acp.receive(&initialized(json!({"loadSession": true})))
            .unwrap();
        assert_eq!(
            code(acp.receive(&refusal(1, AUTH_REQUIRED, "Authentication required"))),
            "launch_account_unsupported",
            "{resume:?}"
        );
        assert_eq!(acp.phase(), Phase::Failed);
        assert_eq!(
            code(acp.receive(&update(SESSION, chunk("after")))),
            "launch_protocol_failed"
        );
    }
    let (mut other, _) = Acp::new(Path::new(CWD), true, None).unwrap();
    other.receive(&initialized(json!({}))).unwrap();
    assert_eq!(
        code(other.receive(&refusal(1, -32602, "Invalid params"))),
        "launch_request_failed"
    );
}

/// DEC-180 Task 10: a line longer than `LINE_BYTES_MAX` or one that is not a JSON-RPC 2.0
/// object fails the launch; a text chunk longer than `FIELD_BYTES_MAX` is clipped, not
/// refused; a line may end in a carriage return.
#[test]
fn a_line_over_the_bound_fails_the_launch_and_a_long_text_is_clipped() {
    let mut long = prompting(true);
    let text = "a".repeat(FIELD_BYTES_MAX + 100);
    let events = events_of(&mut long, &update(SESSION, chunk(&text)));
    let [LaunchEvent::Text { text: kept }] = events.as_slice() else {
        panic!("one text event, not {events:?}");
    };
    assert!(kept.len() <= FIELD_BYTES_MAX);
    assert!(
        kept.ends_with("more bytes not shown]"),
        "{}",
        &kept[kept.len() - 40..]
    );
    let mut crlf = update(SESSION, chunk("crlf"));
    crlf.insert(crlf.len() - 1, b'\r');
    assert_eq!(
        events_of(&mut long, &crlf),
        [LaunchEvent::Text {
            text: "crlf".into()
        }]
    );

    let mut over = prompting(true);
    let mut huge = vec![b' '; LINE_BYTES_MAX + 1];
    huge[0] = b'{';
    huge[LINE_BYTES_MAX] = b'}';
    assert_eq!(code(over.receive(&huge)), "launch_protocol_failed");
    assert_eq!(over.phase(), Phase::Failed);

    let lines: [&[u8]; 5] = [
        b"not json\n",
        b"[1,2]\n",
        br#"{"jsonrpc":"1.0","method":"session/update","params":{}}"#,
        br#"{"jsonrpc":"2.0"}"#,
        br#"{"jsonrpc":"2.0","method":5,"id":2,"result":{"stopReason":"end_turn"}}"#,
    ];
    for line in lines {
        let mut acp = prompting(true);
        assert_eq!(
            code(acp.receive(line)),
            "launch_protocol_failed",
            "{}",
            String::from_utf8_lossy(line)
        );
    }
}

/// DEC-180 Task 10: the client serves no file, terminal or other request of the agent and
/// answers each with method not found; it reads updates only for its own session and only
/// during a turn; a response to no request of its own fails the launch.
#[test]
fn the_client_offers_no_service_and_reads_only_its_own_session() {
    let mut acp = prompting(true);
    for method in ["fs/read_text_file", "fs/write_text_file", "terminal/create"] {
        let request = frame_of(json!({
            "jsonrpc": "2.0",
            "id": 9,
            "method": method,
            "params": {"sessionId": SESSION, "path": "/etc/passwd"}
        }));
        let (answer, events) = answered(&mut acp, &request);
        assert_eq!(
            answer,
            json!({
                "jsonrpc": "2.0",
                "id": 9,
                "error": {"code": METHOD_NOT_FOUND, "message": "Method not found"}
            }),
            "{method}"
        );
        none(events);
    }
    none(events_of(&mut acp, &update("sess_other", chunk("x"))));
    let notice = frame_of(json!({"jsonrpc": "2.0", "method": "x/notice", "params": {}}));
    none(events_of(&mut acp, &notice));
    assert_eq!(
        events_of(&mut acp, &reply(2, json!({"stopReason": "end_turn"}))),
        [LaunchEvent::TurnFinished { success: true }]
    );
    none(events_of(&mut acp, &update(SESSION, chunk("idle"))));
    assert_eq!(
        code(acp.receive(&reply(99, json!({})))),
        "launch_protocol_failed"
    );

    let mut stale = prompting(true);
    assert_eq!(
        code(stale.receive(&frame_of(
            json!({"jsonrpc": "2.0", "id": "2", "result": {}})
        ))),
        "launch_protocol_failed"
    );
}
