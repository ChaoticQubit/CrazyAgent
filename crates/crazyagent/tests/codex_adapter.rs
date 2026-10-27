use crazyagent::adapters::LINE_BYTES_MAX;
use crazyagent::adapters::codex::{
    ApprovalKind, CodexProtocol, Event, FRAME_BYTES_MAX, ProtocolError, State, TurnStatus, WireId,
};
use serde_json::{Value, json};
use std::path::Path;

fn wire(value: Value) -> Vec<u8> {
    let mut bytes = serde_json::to_vec(&value).unwrap();
    bytes.push(b'\n');
    bytes
}

fn request(bytes: &[u8]) -> Value {
    assert_eq!(bytes.last(), Some(&b'\n'));
    assert_eq!(bytes.iter().filter(|b| **b == b'\n').count(), 1);
    serde_json::from_slice(bytes).unwrap()
}

fn reply(protocol: &mut CodexProtocol, id: i64, result: Value) -> Vec<Event> {
    protocol
        .receive(&wire(json!({"id":id,"result":result})))
        .unwrap()
}

fn initialized() -> CodexProtocol {
    let mut protocol = CodexProtocol::new();
    assert_eq!(request(&protocol.initialize().unwrap())["id"], 1);
    assert_eq!(
        reply(
            &mut protocol,
            1,
            json!({
                "userAgent":"unit-wire-data", "codexHome":"/unit/codex",
                "platformFamily":"unix", "platformOs":"macos"
            })
        ),
        vec![Event::Initialized]
    );
    assert_eq!(protocol.state(), State::Initialized);
    assert_eq!(
        request(&protocol.initialized().unwrap()),
        json!({"method":"initialized"})
    );
    assert_eq!(protocol.state(), State::Ready);
    protocol
}

fn thread() -> Value {
    json!({
        "id":"thread-unit", "cliVersion":"0.163.0", "createdAt":1,
        "updatedAt":1, "cwd":"/unit/worktree", "ephemeral":false,
        "modelProvider":"unit-provider-data", "preview":"", "projectId":null,
        "sessionId":"session-unit", "source":"appServer", "status":{"type":"idle"},
        "turns":[]
    })
}

fn thread_result() -> Value {
    json!({
        "thread":thread(), "cwd":"/unit/worktree", "model":"unit-model-data",
        "modelProvider":"unit-provider-data", "approvalPolicy":"on-request",
        "approvalsReviewer":"user", "sandbox":{"type":"readOnly","networkAccess":false}
    })
}

fn idle() -> CodexProtocol {
    let mut protocol = initialized();
    let outgoing = request(&protocol.start_thread(Path::new("/unit/worktree")).unwrap());
    assert_eq!(outgoing["id"], 2);
    assert_eq!(
        reply(&mut protocol, 2, thread_result()),
        vec![Event::ThreadStarted {
            thread_id: "thread-unit".into()
        }]
    );
    assert_eq!(protocol.state(), State::Idle);
    protocol
}

fn turn(id: &str, status: &str) -> Value {
    json!({"id":id,"items":[],"status":status})
}

fn notification(method: &str, turn_id: &str, status: &str) -> Vec<u8> {
    wire(json!({"method":method,"params":{"threadId":"thread-unit","turn":turn(turn_id,status)}}))
}

fn active() -> CodexProtocol {
    let mut protocol = idle();
    assert_eq!(
        request(&protocol.start_turn("unit input").unwrap())["id"],
        3
    );
    assert_eq!(
        reply(
            &mut protocol,
            3,
            json!({"turn":turn("turn-unit","inProgress")})
        ),
        vec![Event::TurnStarted {
            turn_id: "turn-unit".into()
        }]
    );
    assert_eq!(protocol.state(), State::Active);
    protocol
}

fn approval(id: Value, method: &str) -> Vec<u8> {
    let mut params = json!({
        "threadId":"thread-unit", "turnId":"turn-unit", "itemId":"item-unit",
        "startedAtMs":1
    });
    if method == "item/permissions/requestApproval" {
        params["cwd"] = json!("/unit/worktree");
        params["permissions"] = json!({"network":{"enabled":true}});
    }
    wire(json!({"id":id,"method":method,"params":params}))
}

#[test]
fn initialize_uses_stable_framing_and_explicit_nonexperimental_capabilities() {
    let mut protocol = CodexProtocol::new();
    let value = request(&protocol.initialize().unwrap());
    assert_eq!(value["id"], 1);
    assert_eq!(value["method"], "initialize");
    assert_eq!(value["params"]["clientInfo"]["name"], "crazyagent");
    assert_eq!(value["params"]["capabilities"]["experimentalApi"], false);
    assert_eq!(value["params"]["capabilities"]["requestAttestation"], false);
    assert!(value.get("jsonrpc").is_none());
    assert_eq!(protocol.state(), State::Initializing);
    assert_eq!(protocol.initialize(), Err(ProtocolError::InvalidState));
    assert_eq!(protocol.initialized(), Err(ProtocolError::InvalidState));
}

#[test]
fn initialization_requires_a_correlated_complete_reply_before_thread_dispatch() {
    let mut protocol = CodexProtocol::new();
    protocol.initialize().unwrap();
    assert_eq!(
        protocol.start_thread(Path::new("/unit/worktree")),
        Err(ProtocolError::InvalidState)
    );
    assert_eq!(
        protocol.receive(&wire(json!({"id":2,"result":{}}))),
        Err(ProtocolError::StaleMessage)
    );
    assert_eq!(protocol.state(), State::Failed);
    assert_eq!(protocol.initialize(), Err(ProtocolError::InvalidState));
}

#[test]
fn incomplete_initialize_payload_is_rejected() {
    let mut protocol = CodexProtocol::new();
    protocol.initialize().unwrap();
    assert_eq!(
        protocol.receive(&wire(json!({"id":1,"result":{"userAgent":"unit"}}))),
        Err(ProtocolError::InvalidMessage)
    );
    assert_eq!(protocol.state(), State::Failed);
}

#[test]
fn correlated_remote_failure_retires_identity_without_claiming_success() {
    let mut protocol = CodexProtocol::new();
    protocol.initialize().unwrap();
    assert_eq!(
        protocol
            .receive(&wire(
                json!({"id":1,"error":{"code":-32000,"message":"unit failure"}})
            ))
            .unwrap(),
        vec![Event::RequestFailed {
            id: 1,
            method: "initialize",
            code: -32000,
            message: "unit failure".into()
        }]
    );
    assert_eq!(protocol.state(), State::New);
    assert_eq!(request(&protocol.initialize().unwrap())["id"], 2);
    assert_eq!(
        protocol.receive(&wire(json!({"id":1,"result":{}}))),
        Err(ProtocolError::StaleMessage)
    );
}

#[test]
fn decoder_preserves_fragmented_utf8_and_multiple_lines() {
    let mut protocol = active();
    let bytes = wire(json!({"method":"item/agentMessage/delta","params":{
        "threadId":"thread-unit","turnId":"turn-unit","itemId":"item-unit","delta":"a\n🦀"
    }}));
    let split = bytes.iter().position(|b| *b == 0xf0).unwrap() + 2;
    assert!(protocol.receive(&bytes[..split]).unwrap().is_empty());
    let mut tail = bytes[split..].to_vec();
    tail.extend(wire(
        json!({"method":"warning","params":{"message":"unit"}}),
    ));
    assert_eq!(
        protocol.receive(&tail).unwrap(),
        vec![
            Event::AgentMessageDelta {
                turn_id: "turn-unit".into(),
                item_id: "item-unit".into(),
                delta: "a\n🦀".into()
            },
            Event::UnsupportedNotification {
                method: "warning".into()
            }
        ]
    );
}

#[test]
fn decoder_enforces_frame_chunk_and_event_bounds() {
    let mut protocol = CodexProtocol::new();
    assert!(
        protocol
            .receive(&vec![b' '; LINE_BYTES_MAX])
            .unwrap()
            .is_empty()
    );
    assert_eq!(protocol.receive(b" "), Err(ProtocolError::FrameTooLarge));
    assert_eq!(protocol.state(), State::Failed);
    let mut protocol = CodexProtocol::new();
    assert_eq!(
        protocol.receive(&vec![b' '; LINE_BYTES_MAX + 2]),
        Err(ProtocolError::FrameTooLarge)
    );
    let mut protocol = initialized();
    let bytes = wire(json!({"method":"warning","params":{}})).repeat(129);
    assert_eq!(protocol.receive(&bytes), Err(ProtocolError::Capacity));
    assert_eq!(protocol.state(), State::Failed);
}

#[test]
fn malformed_ambiguous_duplicate_and_versioned_envelopes_fail_closed() {
    for bytes in [
        b"\n".as_slice(),
        b"[]\n",
        b"{\"id\":1,\"id\":1,\"result\":{}}\n",
        b"{\"id\":1,\"result\":{},\"error\":null}\n",
        b"{\"id\":1,\"result\":{},\"method\":\"warning\"}\n",
        b"{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{}}\n",
        b"{\"id\":null,\"method\":\"warning\"}\n",
        b"\xff\n",
    ] {
        let mut protocol = CodexProtocol::new();
        protocol.initialize().unwrap();
        assert_eq!(protocol.receive(bytes), Err(ProtocolError::InvalidMessage));
        assert_eq!(protocol.state(), State::Failed);
    }
}

#[test]
fn eof_never_invents_a_completed_or_recoverable_turn() {
    let mut protocol = active();
    assert!(protocol.receive(b"{\"method\":").unwrap().is_empty());
    assert_eq!(protocol.finish(), Err(ProtocolError::Disconnected));
    assert_eq!(protocol.state(), State::Failed);
    assert_eq!(protocol.interrupt(), Err(ProtocolError::InvalidState));
}

#[test]
fn thread_request_has_explicit_restrictive_policy_and_validated_cwd() {
    let mut protocol = initialized();
    assert_eq!(
        protocol.start_thread(Path::new("relative")),
        Err(ProtocolError::InvalidInput)
    );
    assert_eq!(
        protocol.start_thread(Path::new("/unit/../escape")),
        Err(ProtocolError::InvalidInput)
    );
    let value = request(&protocol.start_thread(Path::new("/unit/worktree")).unwrap());
    assert_eq!(
        value,
        json!({"id":2,"method":"thread/start","params":{
            "cwd":"/unit/worktree","approvalPolicy":"on-request","approvalsReviewer":"user","sandbox":"read-only"
        }})
    );
    assert_eq!(protocol.state(), State::ThreadStarting);
}

#[test]
fn effective_thread_policy_must_match_the_restrictive_request() {
    for (key, value) in [
        ("approvalPolicy", json!("never")),
        ("approvalsReviewer", json!("auto_review")),
        ("sandbox", json!({"type":"dangerFullAccess"})),
        ("sandbox", json!({"type":"readOnly","networkAccess":true})),
        ("cwd", json!("/different")),
    ] {
        let mut protocol = initialized();
        protocol.start_thread(Path::new("/unit/worktree")).unwrap();
        let mut result = thread_result();
        result[key] = value;
        assert_eq!(
            protocol.receive(&wire(json!({"id":2,"result":result}))),
            Err(ProtocolError::InvalidMessage)
        );
        assert_eq!(protocol.state(), State::Failed);
    }
}

#[test]
fn thread_notification_before_reply_is_correlated_without_finishing_the_request() {
    let mut protocol = initialized();
    protocol.start_thread(Path::new("/unit/worktree")).unwrap();
    assert_eq!(
        protocol
            .receive(&wire(
                json!({"method":"thread/started","params":{"thread":thread()}})
            ))
            .unwrap(),
        vec![Event::ThreadStarted {
            thread_id: "thread-unit".into()
        }]
    );
    assert_eq!(
        protocol.start_turn("unit"),
        Err(ProtocolError::InvalidState)
    );
    assert!(reply(&mut protocol, 2, thread_result()).is_empty());
    assert_eq!(protocol.state(), State::Idle);
}

#[test]
fn thread_notification_after_reply_is_the_same_thread_and_keeps_the_protocol_idle() {
    let mut protocol = idle();
    assert!(
        protocol
            .receive(&wire(
                json!({"method":"thread/started","params":{"thread":thread()}})
            ))
            .unwrap()
            .is_empty()
    );
    assert_eq!(protocol.state(), State::Idle);
    let mut other = thread();
    other["id"] = json!("thread-other");
    assert_eq!(
        protocol.receive(&wire(
            json!({"method":"thread/started","params":{"thread":other}})
        )),
        Err(ProtocolError::StaleMessage)
    );
}

#[test]
fn turn_request_preserves_text_without_permission_overrides() {
    let mut protocol = idle();
    assert_eq!(protocol.start_turn(""), Err(ProtocolError::InvalidInput));
    assert_eq!(
        protocol.start_turn(&"x".repeat(FRAME_BYTES_MAX)),
        Err(ProtocolError::FrameTooLarge)
    );
    let value = request(&protocol.start_turn("unit\n🦀").unwrap());
    assert_eq!(
        value,
        json!({"id":3,"method":"turn/start","params":{
            "threadId":"thread-unit","input":[{"type":"text","text":"unit\n🦀","text_elements":[]}]
        }})
    );
    assert_eq!(protocol.state(), State::TurnStarting);
}

#[test]
fn turn_notifications_before_reply_cannot_be_rolled_back_by_acknowledgement() {
    let mut protocol = idle();
    protocol.start_turn("unit").unwrap();
    assert_eq!(
        protocol
            .receive(&notification("turn/started", "turn-unit", "inProgress"))
            .unwrap(),
        vec![Event::TurnStarted {
            turn_id: "turn-unit".into()
        }]
    );
    assert_eq!(
        protocol
            .receive(&notification("turn/completed", "turn-unit", "completed"))
            .unwrap(),
        vec![Event::TurnCompleted {
            turn_id: "turn-unit".into(),
            status: TurnStatus::Completed
        }]
    );
    assert_eq!(protocol.state(), State::Idle);
    assert_eq!(
        protocol.start_turn("next"),
        Err(ProtocolError::InvalidState)
    );
    assert!(
        reply(
            &mut protocol,
            3,
            json!({"turn":turn("turn-unit","inProgress")})
        )
        .is_empty()
    );
    assert_eq!(protocol.state(), State::Idle);
    assert_eq!(request(&protocol.start_turn("next").unwrap())["id"], 4);
    assert_eq!(
        protocol.receive(&notification("turn/started", "turn-unit", "inProgress")),
        Err(ProtocolError::StaleMessage)
    );
}

#[test]
fn interrupt_acknowledgement_does_not_claim_the_turn_has_stopped() {
    let mut protocol = active();
    assert_eq!(
        request(&protocol.interrupt().unwrap()),
        json!({"id":4,"method":"turn/interrupt","params":{"threadId":"thread-unit","turnId":"turn-unit"}})
    );
    assert_eq!(
        reply(&mut protocol, 4, json!({})),
        vec![Event::InterruptAcknowledged {
            turn_id: "turn-unit".into()
        }]
    );
    assert_eq!(protocol.state(), State::Active);
    assert_eq!(
        protocol
            .receive(&notification("turn/completed", "turn-unit", "interrupted"))
            .unwrap(),
        vec![Event::TurnCompleted {
            turn_id: "turn-unit".into(),
            status: TurnStatus::Interrupted
        }]
    );
    assert_eq!(protocol.state(), State::Idle);
    assert_eq!(protocol.interrupt(), Err(ProtocolError::InvalidState));
}

#[test]
fn completion_before_interrupt_reply_is_preserved() {
    let mut protocol = active();
    protocol.interrupt().unwrap();
    protocol
        .receive(&notification("turn/completed", "turn-unit", "interrupted"))
        .unwrap();
    assert_eq!(
        reply(&mut protocol, 4, json!({})),
        vec![Event::InterruptAcknowledged {
            turn_id: "turn-unit".into()
        }]
    );
    assert_eq!(protocol.state(), State::Idle);
}

#[test]
fn error_notification_reports_retry_without_a_terminal_transition() {
    let mut protocol = active();
    assert_eq!(protocol.receive(&wire(json!({"method":"error","params":{
        "threadId":"thread-unit","turnId":"turn-unit","error":{"message":"unit error"},"willRetry":false
    }}))).unwrap(), vec![Event::TurnError { turn_id:"turn-unit".into(), message:"unit error".into(), will_retry:false }]);
    assert_eq!(protocol.state(), State::Active);
    assert_eq!(
        protocol
            .receive(&notification("turn/completed", "turn-unit", "failed"))
            .unwrap(),
        vec![Event::TurnCompleted {
            turn_id: "turn-unit".into(),
            status: TurnStatus::Failed
        }]
    );
}

#[test]
fn approvals_preserve_native_identity_and_require_explicit_denial() {
    for (method, kind) in [
        (
            "item/commandExecution/requestApproval",
            ApprovalKind::Command,
        ),
        ("item/fileChange/requestApproval", ApprovalKind::FileChange),
        (
            "item/permissions/requestApproval",
            ApprovalKind::Permissions,
        ),
    ] {
        let mut protocol = active();
        let input = approval(json!("approval-unit"), method);
        let events = protocol.receive(&input).unwrap();
        assert!(
            matches!(&events[0],Event::ApprovalRequired { id:WireId::Text(id),kind:found,params } if id=="approval-unit" && *found==kind && params["turnId"]=="turn-unit")
        );
        assert_eq!(protocol.state(), State::Active);
        let response = request(
            &protocol
                .deny_request(&WireId::Text("approval-unit".into()))
                .unwrap(),
        );
        if kind == ApprovalKind::Permissions {
            assert_eq!(
                response,
                json!({"id":"approval-unit","result":{"permissions":{},"scope":"turn"}})
            );
        } else {
            assert_eq!(
                response,
                json!({"id":"approval-unit","result":{"decision":"decline"}})
            );
        }
        assert_eq!(
            protocol.deny_request(&WireId::Text("approval-unit".into())),
            Err(ProtocolError::StaleMessage)
        );
        assert_eq!(protocol.receive(&input), Err(ProtocolError::StaleMessage));
    }
}

#[test]
fn numeric_server_ids_have_a_separate_direction_from_client_correlation() {
    let mut protocol = active();
    protocol.interrupt().unwrap();
    protocol
        .receive(&approval(json!(4), "item/fileChange/requestApproval"))
        .unwrap();
    assert_eq!(
        request(&protocol.deny_request(&WireId::Number(4)).unwrap())["id"],
        4
    );
    assert_eq!(
        reply(&mut protocol, 4, json!({})),
        vec![Event::InterruptAcknowledged {
            turn_id: "turn-unit".into()
        }]
    );
}

#[test]
fn pending_approvals_are_bounded_and_outlive_turn_completion_until_declined() {
    let mut protocol = active();
    for id in 0..16 {
        protocol
            .receive(&approval(json!(id), "item/fileChange/requestApproval"))
            .unwrap();
    }
    assert_eq!(
        protocol.receive(&approval(json!(16), "item/fileChange/requestApproval")),
        Err(ProtocolError::Capacity)
    );
    assert_eq!(protocol.state(), State::Failed);
    let mut protocol = active();
    protocol
        .receive(&approval(json!("unit"), "item/fileChange/requestApproval"))
        .unwrap();
    protocol
        .receive(&notification("turn/completed", "turn-unit", "completed"))
        .unwrap();
    assert_eq!(
        protocol.start_turn("next"),
        Err(ProtocolError::InvalidState)
    );
    assert_eq!(
        request(&protocol.deny_request(&WireId::Text("unit".into())).unwrap()),
        json!({"id":"unit","result":{"decision":"decline"}})
    );
    assert_eq!(
        protocol.deny_request(&WireId::Text("unit".into())),
        Err(ProtocolError::StaleMessage)
    );
    assert_eq!(request(&protocol.start_turn("next").unwrap())["id"], 4);
}

#[test]
fn requests_read_together_with_their_turn_end_are_still_declined_by_their_own_protocol() {
    let mut protocol = active();
    let mut input = approval(json!(7), "item/commandExecution/requestApproval");
    input.extend(wire(json!({
        "id":8, "method":"item/tool/requestUserInput", "params":{
            "threadId":"thread-unit", "turnId":"turn-unit", "itemId":"item-input",
            "isBlocking":false,
            "questions":[{"id":"scope","header":"Scope","question":"Which work?"}]
        }
    })));
    input.extend(notification("turn/completed", "turn-unit", "completed"));
    let events = protocol.receive(&input).unwrap();
    assert_eq!(events.len(), 3);
    assert!(matches!(
        &events[0],
        Event::ApprovalRequired {
            id: WireId::Number(7),
            kind: ApprovalKind::Command,
            ..
        }
    ));
    assert_eq!(
        events[1],
        Event::UnsupportedRequest {
            id: WireId::Number(8),
            method: "item/tool/requestUserInput".into()
        }
    );
    assert_eq!(
        events[2],
        Event::TurnCompleted {
            turn_id: "turn-unit".into(),
            status: TurnStatus::Completed
        }
    );
    assert_eq!(protocol.state(), State::Idle);
    assert_eq!(
        request(&protocol.deny_request(&WireId::Number(7)).unwrap()),
        json!({"id":7,"result":{"decision":"decline"}})
    );
    assert_eq!(
        protocol.start_turn("next"),
        Err(ProtocolError::InvalidState)
    );
    assert_eq!(
        request(&protocol.deny_request(&WireId::Number(8)).unwrap()),
        json!({"id":8,"error":{"code":-32601,"message":"unsupported native request"}})
    );
    assert_eq!(request(&protocol.start_turn("next").unwrap())["id"], 4);
}

fn child_turn(thread: &str, turn: &str, root: &str, status: &str) -> Vec<u8> {
    let method = if status == "inProgress" {
        "turn/started"
    } else {
        "turn/completed"
    };
    wire(json!({"method":method,"params":{"threadId":thread,"turn":{
        "id":turn, "rootTurnId":root, "items":[], "itemsView":"notLoaded", "status":status,
        "error":null, "startedAt":1, "completedAt":null, "durationMs":null
    }}}))
}

#[test]
fn a_codex_native_sub_agent_is_interrupted_and_its_frames_never_reach_the_turn() {
    let mut protocol = active();
    let mut input = wire(json!({"method":"item/started","params":{
        "item":{"type":"subAgentActivity","model":"unit-model","reasoningEffort":"low",
            "id":"call-unit","kind":"started","agentThreadId":"child-unit","agentPath":"/root/agent_1"},
        "threadId":"thread-unit", "turnId":"turn-unit", "startedAtMs":1
    }}));
    input.extend(child_turn(
        "child-unit",
        "child-turn",
        "turn-unit",
        "inProgress",
    ));
    input.extend(wire(json!({"method":"item/agentMessage/delta","params":{
        "threadId":"child-unit", "turnId":"child-turn", "itemId":"child-item", "delta":"child text"
    }})));
    input.extend(wire(json!({"method":"error","params":{
        "error":{"message":"child failure"}, "willRetry":false,
        "threadId":"child-unit", "turnId":"child-turn"
    }})));
    assert_eq!(
        protocol.receive(&input).unwrap(),
        vec![
            Event::UnsupportedNotification {
                method: "item/started".into()
            },
            Event::NativeSubAgentStarted {
                thread_id: "child-unit".into(),
                turn_id: "child-turn".into()
            }
        ]
    );
    assert_eq!(protocol.state(), State::Active);
    assert_eq!(
        request(
            &protocol
                .interrupt_child("child-unit", "child-turn")
                .unwrap()
        ),
        json!({"id":4,"method":"turn/interrupt","params":{"threadId":"child-unit","turnId":"child-turn"}})
    );
    assert_eq!(
        protocol.interrupt_child("other-unit", "other-turn"),
        Err(ProtocolError::StaleMessage)
    );
    assert!(reply(&mut protocol, 4, json!({})).is_empty());
    assert!(
        protocol
            .receive(&child_turn(
                "child-unit",
                "child-turn",
                "turn-unit",
                "interrupted"
            ))
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        protocol
            .receive(&child_turn(
                "grandchild-unit",
                "grandchild-turn",
                "turn-unit",
                "inProgress"
            ))
            .unwrap(),
        vec![Event::NativeSubAgentStarted {
            thread_id: "grandchild-unit".into(),
            turn_id: "grandchild-turn".into()
        }]
    );
    assert_eq!(
        request(
            &protocol
                .interrupt_child("grandchild-unit", "grandchild-turn")
                .unwrap()
        )["id"],
        5
    );
    assert_eq!(
        protocol
            .receive(&wire(
                json!({"id":5,"error":{"code":-32600,"message":"no active turn"}})
            ))
            .unwrap(),
        Vec::new()
    );
    assert_eq!(
        protocol
            .receive(&wire(json!({"method":"item/agentMessage/delta","params":{
                "threadId":"thread-unit", "turnId":"turn-unit", "itemId":"own-item", "delta":"own text"
            }})))
            .unwrap(),
        vec![Event::AgentMessageDelta {
            turn_id: "turn-unit".into(),
            item_id: "own-item".into(),
            delta: "own text".into()
        }]
    );
    assert_eq!(
        protocol
            .receive(&notification("turn/completed", "turn-unit", "completed"))
            .unwrap(),
        vec![Event::TurnCompleted {
            turn_id: "turn-unit".into(),
            status: TurnStatus::Completed
        }]
    );
    assert!(
        protocol
            .receive(&child_turn(
                "child-unit",
                "child-turn",
                "turn-unit",
                "completed"
            ))
            .unwrap()
            .is_empty()
    );
    assert_eq!(protocol.state(), State::Idle);
}

#[test]
fn a_foreign_turn_that_did_not_start_from_this_turn_is_still_refused() {
    let mut protocol = active();
    assert_eq!(
        protocol.receive(&child_turn(
            "other-unit",
            "other-turn",
            "other-root",
            "inProgress"
        )),
        Err(ProtocolError::StaleMessage)
    );
    assert_eq!(protocol.state(), State::Failed);
}

#[test]
fn stale_mismatched_and_malformed_turn_events_are_rejected() {
    for value in [
        json!({"method":"turn/completed","params":{"threadId":"other","turn":turn("turn-unit","completed")}}),
        json!({"method":"turn/completed","params":{"threadId":"thread-unit","turn":turn("other","completed")}}),
        json!({"method":"turn/completed","params":{"threadId":"thread-unit","turn":turn("turn-unit","inProgress")}}),
        json!({"method":"turn/started","params":{"threadId":"thread-unit","turn":{"id":"turn-unit","status":"inProgress"}}}),
    ] {
        let mut protocol = active();
        assert!(protocol.receive(&wire(value)).is_err());
        assert_eq!(protocol.state(), State::Failed);
    }
}

#[test]
fn unsupported_requests_are_visible_and_only_receive_method_not_found() {
    let mut protocol = active();
    assert_eq!(
        protocol
            .receive(&wire(
                json!({"id":"child-unit","method":"thread/child/cancel","params":{}})
            ))
            .unwrap(),
        vec![Event::UnsupportedRequest {
            id: WireId::Text("child-unit".into()),
            method: "thread/child/cancel".into()
        }]
    );
    assert_eq!(
        request(
            &protocol
                .deny_request(&WireId::Text("child-unit".into()))
                .unwrap()
        ),
        json!({"id":"child-unit","error":{"code":-32601,"message":"unsupported native request"}})
    );
    assert_eq!(protocol.state(), State::Active);
}

#[test]
fn server_ids_and_permission_context_are_validated_at_the_boundary() {
    for value in [
        json!({"id":"","method":"item/fileChange/requestApproval","params":{}}),
        json!({"id":1.5,"method":"item/fileChange/requestApproval","params":{}}),
        json!({"id":9223372036854775808u64,"method":"item/fileChange/requestApproval","params":{}}),
        json!({"id":1,"method":"item/fileChange/requestApproval","params":{"threadId":"other","turnId":"turn-unit","itemId":"item","startedAtMs":1}}),
        json!({"id":1,"method":"item/permissions/requestApproval","params":{"threadId":"thread-unit","turnId":"turn-unit","itemId":"item","startedAtMs":1}}),
    ] {
        let mut protocol = active();
        assert!(protocol.receive(&wire(value)).is_err());
        assert_eq!(protocol.state(), State::Failed);
    }
}

#[test]
fn terminal_reply_after_terminal_notification_is_only_an_acknowledgement() {
    let mut protocol = idle();
    protocol.start_turn("unit").unwrap();
    protocol
        .receive(&notification("turn/started", "turn-unit", "inProgress"))
        .unwrap();
    protocol
        .receive(&notification("turn/completed", "turn-unit", "completed"))
        .unwrap();
    assert!(
        reply(
            &mut protocol,
            3,
            json!({"turn":turn("turn-unit","completed")})
        )
        .is_empty()
    );
    assert_eq!(protocol.state(), State::Idle);
}

#[test]
fn unsupported_request_while_idle_is_deniable_and_blocks_new_dispatch() {
    let mut protocol = active();
    protocol
        .receive(&notification("turn/completed", "turn-unit", "completed"))
        .unwrap();
    assert_eq!(
        protocol
            .receive(&wire(
                json!({"id":"idle-unit","method":"account/chatgptAuthTokens/refresh","params":{}})
            ))
            .unwrap(),
        vec![Event::UnsupportedRequest {
            id: WireId::Text("idle-unit".into()),
            method: "account/chatgptAuthTokens/refresh".into()
        }]
    );
    assert_eq!(
        protocol.start_turn("next"),
        Err(ProtocolError::InvalidState)
    );
    assert_eq!(
        request(
            &protocol
                .deny_request(&WireId::Text("idle-unit".into()))
                .unwrap()
        )["error"]["code"],
        -32601
    );
    assert_eq!(request(&protocol.start_turn("next").unwrap())["id"], 4);
}
