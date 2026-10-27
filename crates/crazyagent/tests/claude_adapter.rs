use crazyagent::adapters::claude::{ClaudeProtocol, ProtocolError, State};
use crazyagent::adapters::events::{Capability, EventKind, Support, TurnOutcome};
use crazyagent::adapters::{LINE_BYTES_MAX, NativeAction, NativeHarness};
use crazyagent::protocol::{CredentialRef, FRAME_BYTES_MAX, RequestId, SessionId};
use serde_json::{Value, json};

fn launch() -> NativeAction {
    NativeAction::Launch {
        session_id: SessionId::new("managed-session").unwrap(),
        harness: NativeHarness::Claude,
        profile_ref: CredentialRef::new("claude-profile").unwrap(),
        worktree_lease: RequestId::new("worktree-lease").unwrap(),
    }
}

fn protocol() -> ClaudeProtocol {
    ClaudeProtocol::new(launch(), CredentialRef::new("approved-account").unwrap()).unwrap()
}

fn wire(value: Value) -> Vec<u8> {
    let mut bytes = serde_json::to_vec(&value).unwrap();
    bytes.push(b'\n');
    bytes
}

fn init() -> Value {
    json!({
        "type":"system", "subtype":"init", "uuid":"init-1",
        "session_id":"native-session", "model":"observed-model",
        "claude_code_version":"2.1.302", "cwd":"/worktree",
        "tools":[], "mcp_servers":[], "apiKeySource":"none",
        "permissionMode":"default", "slash_commands":[],
        "output_style":"default", "skills":[], "plugins":[],
        "capabilities":["interrupt_receipt_v1","future-native-capability"]
    })
}

fn ready() -> ClaudeProtocol {
    let mut p = protocol();
    p.receive(&wire(init())).unwrap();
    assert_eq!(p.state(), State::Idle);
    p
}

fn active() -> ClaudeProtocol {
    let mut p = ready();
    p.start_turn(RequestId::new("request-one").unwrap(), "Read the file")
        .unwrap();
    assert_eq!(p.state(), State::Active);
    p
}

fn assistant(uuid: &str, content: Value) -> Value {
    json!({
        "type":"assistant", "uuid":uuid, "session_id":"native-session",
        "parent_tool_use_id":null,
        "message":{"id":"message-one","type":"message","role":"assistant",
            "model":"observed-model","content":content,"stop_reason":"end_turn",
            "usage":{"input_tokens":12,"output_tokens":5}}
    })
}

fn result(uuid: &str) -> Value {
    json!({
        "type":"result", "subtype":"success", "uuid":uuid,
        "session_id":"native-session", "duration_ms":3, "duration_api_ms":2,
        "num_turns":1, "is_error":false, "result":"done", "stop_reason":"end_turn",
        "usage":{"input_tokens":12,"output_tokens":5,"cache_read_input_tokens":0},
        "total_cost_usd":0.004, "modelUsage":{}, "permission_denials":[]
    })
}

#[test]
fn initial_prompt_and_observed_init_preserve_all_launch_provenance() {
    let mut p = protocol();
    let outgoing = p
        .start_turn(RequestId::new("request-one").unwrap(), "Read the file")
        .unwrap();
    let message: Value = serde_json::from_slice(&outgoing).unwrap();
    assert_eq!(message["type"], "user");
    assert_eq!(
        message["message"],
        json!({"role":"user","content":"Read the file"})
    );
    assert_eq!(message["parent_tool_use_id"], Value::Null);
    assert!(message.get("session_id").is_none());
    assert!(message.get("account_ref").is_none());
    assert_eq!(p.state(), State::AwaitingInit);
    let events = p.receive(&wire(init())).unwrap();
    assert_eq!(events.len(), 1);
    let event = &events[0];
    assert_eq!(event.provenance.session_id.as_str(), "managed-session");
    assert_eq!(event.provenance.harness, NativeHarness::Claude);
    assert_eq!(event.provenance.account_ref.as_str(), "approved-account");
    assert_eq!(event.provenance.profile_ref.as_str(), "claude-profile");
    assert_eq!(event.provenance.worktree_lease.as_str(), "worktree-lease");
    assert_eq!(event.native_session_id, "native-session");
    assert_eq!(event.request_id.as_ref().unwrap().as_str(), "request-one");
    assert!(
        matches!(&event.kind, EventKind::Initialized { model, capabilities }
        if model == "observed-model" && capabilities.len() == 2)
    );
    assert_eq!(p.state(), State::Active);
}

#[test]
fn fragmented_utf8_and_complete_assistant_blocks_decode_without_duplicate_text() {
    let mut p = active();
    let bytes = wire(assistant(
        "assistant-1",
        json!([{"type":"text","text":"Hello λ"}]),
    ));
    let split = bytes.iter().position(|b| *b == 0xce).unwrap() + 1;
    assert!(p.receive(&bytes[..split]).unwrap().is_empty());
    let events = p.receive(&bytes[split..]).unwrap();
    assert_eq!(events.len(), 1);
    assert!(
        matches!(&events[0].kind, EventKind::AssistantText { message_id, parent_tool_use_id, text }
        if message_id == "message-one" && parent_tool_use_id.is_none() && text == "Hello λ")
    );
    let tool = assistant(
        "assistant-2",
        json!([{"type":"tool_use","id":"tool-one","name":"Read","input":{"file_path":"a.rs"}}]),
    );
    let events = p.receive(&wire(tool)).unwrap();
    assert!(
        matches!(&events[0].kind, EventKind::ToolUse { message_id, tool_use_id, name, input, .. }
        if message_id == "message-one" && tool_use_id == "tool-one" && name == "Read" && input["file_path"] == "a.rs")
    );
}

#[test]
fn subagent_output_retains_parent_identity_without_inventing_a_native_child() {
    let mut p = active();
    let mut value = assistant("child-output", json!([{"type":"text","text":"child text"}]));
    value["parent_tool_use_id"] = json!("parent-tool");
    let events = p.receive(&wire(value)).unwrap();
    assert!(
        matches!(&events[0].kind, EventKind::AssistantText { parent_tool_use_id, .. }
        if parent_tool_use_id.as_deref() == Some("parent-tool"))
    );
    assert_eq!(p.support(Capability::CancelChild), Support::Unavailable);
    assert_eq!(p.support(Capability::NativeAdmission), Support::Unavailable);
    assert_eq!(p.support(Capability::LiveRecovery), Support::Unavailable);
    assert_eq!(
        p.support(Capability::StructuredEvents),
        Support::ProtocolOnly
    );
}

#[test]
fn result_reports_observed_per_turn_usage_and_estimated_cost_without_filling_unknowns() {
    let mut p = active();
    let events = p.receive(&wire(result("result-1"))).unwrap();
    assert!(matches!(&events[0].kind,
        EventKind::TurnFinished { outcome:TurnOutcome::Success, usage, estimated_cost_usd, .. }
        if usage.input_tokens == Some(12) && usage.output_tokens == Some(5)
            && usage.cache_read_input_tokens == Some(0)
            && usage.cache_creation_input_tokens.is_none() && *estimated_cost_usd == Some(0.004)));
    assert_eq!(p.state(), State::Idle);
    let outgoing = p
        .start_turn(RequestId::new("request-two").unwrap(), "Continue")
        .unwrap();
    let value: Value = serde_json::from_slice(&outgoing).unwrap();
    assert_eq!(value["session_id"], "native-session");
    let mut no_usage = result("result-2");
    no_usage.as_object_mut().unwrap().remove("usage");
    no_usage.as_object_mut().unwrap().remove("total_cost_usd");
    let events = p.receive(&wire(no_usage)).unwrap();
    assert_eq!(
        events[0].request_id.as_ref().unwrap().as_str(),
        "request-two"
    );
    assert!(
        matches!(&events[0].kind, EventKind::TurnFinished { usage, estimated_cost_usd, .. }
        if usage.input_tokens.is_none() && usage.output_tokens.is_none() && estimated_cost_usd.is_none())
    );
}

#[test]
fn error_results_and_contradictory_success_are_truthful() {
    let mut p = active();
    let mut value = result("result-error");
    value["subtype"] = json!("error_max_turns");
    value["is_error"] = json!(true);
    value["errors"] = json!(["turn limit"]);
    let events = p.receive(&wire(value)).unwrap();
    assert!(
        matches!(&events[0].kind, EventKind::TurnFinished { outcome:TurnOutcome::Failed, errors, .. }
        if errors == &["turn limit"])
    );
    p.start_turn(RequestId::new("next-request").unwrap(), "Continue")
        .unwrap();
    let mut value = result("result-contradictory");
    value["is_error"] = json!(true);
    let events = p.receive(&wire(value)).unwrap();
    assert!(matches!(
        events[0].kind,
        EventKind::TurnFinished {
            outcome: TurnOutcome::Failed,
            ..
        }
    ));
}

#[test]
fn unknown_and_partial_messages_remain_explicitly_unsupported() {
    let mut p = active();
    let events = p.receive(&wire(json!({"type":"stream_event","uuid":"partial-1",
        "session_id":"native-session","parent_tool_use_id":null,
        "event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hi"}}}))).unwrap();
    assert!(
        matches!(&events[0].kind, EventKind::Unsupported { message_type, subtype }
        if message_type == "stream_event" && subtype.is_none())
    );
    let events = p
        .receive(&wire(json!({"type":"system","subtype":"future",
        "uuid":"future-1","session_id":"native-session"})))
        .unwrap();
    assert!(
        matches!(&events[0].kind, EventKind::Unsupported { subtype, .. }
        if subtype.as_deref() == Some("future"))
    );
    assert_eq!(p.state(), State::Active);
}

#[test]
fn concurrent_and_reused_request_ids_are_rejected_without_mutating_the_turn() {
    let mut p = active();
    assert_eq!(
        p.start_turn(RequestId::new("request-two").unwrap(), "second"),
        Err(ProtocolError::InvalidState)
    );
    p.receive(&wire(result("result-1"))).unwrap();
    assert_eq!(
        p.start_turn(RequestId::new("request-one").unwrap(), "retry"),
        Err(ProtocolError::StaleMessage)
    );
    assert_eq!(p.state(), State::Idle);
    assert!(
        p.start_turn(RequestId::new("request-two").unwrap(), "second")
            .is_ok()
    );
}

#[test]
fn changed_session_and_duplicate_native_event_ids_fail_closed() {
    let mut p = active();
    let bytes = wire(assistant(
        "assistant-1",
        json!([{"type":"text","text":"one"}]),
    ));
    p.receive(&bytes).unwrap();
    assert_eq!(p.receive(&bytes), Err(ProtocolError::StaleMessage));
    assert_eq!(p.state(), State::Failed);
    assert_eq!(
        p.receive(&wire(result("result-1"))),
        Err(ProtocolError::InvalidState)
    );
    let mut p = active();
    let mut value = result("foreign-result");
    value["session_id"] = json!("other-native-session");
    assert_eq!(p.receive(&wire(value)), Err(ProtocolError::StaleMessage));
    assert_eq!(p.state(), State::Failed);
}

#[test]
fn result_before_init_and_assistant_without_an_active_request_are_rejected() {
    let mut p = protocol();
    assert_eq!(
        p.receive(&wire(result("early-result"))),
        Err(ProtocolError::InvalidState)
    );
    let mut p = ready();
    assert_eq!(
        p.receive(&wire(assistant("unsolicited", json!([])))),
        Err(ProtocolError::InvalidState)
    );
}

#[test]
fn system_notice_before_init_is_skipped_and_init_still_binds_the_session() {
    let mut p = protocol();
    let notice = json!({
        "type":"system", "subtype":"ui_invalidate", "event":"ui.render",
        "uuid":"notice-1", "session_id":"native-session"
    });
    assert_eq!(p.receive(&wire(notice.clone())), Ok(Vec::new()));
    assert_eq!(p.state(), State::AwaitingInit);
    assert_eq!(p.receive(&wire(notice)), Err(ProtocolError::StaleMessage));
    let mut p = protocol();
    p.receive(&wire(json!({
        "type":"system", "subtype":"ui_invalidate", "uuid":"notice-2", "session_id":"native-session"
    })))
    .unwrap();
    let events = p.receive(&wire(init())).unwrap();
    assert!(matches!(events[0].kind, EventKind::Initialized { .. }));
    assert_eq!(p.state(), State::Idle);
}

#[test]
fn duplicate_keys_at_every_depth_are_rejected_before_normalization() {
    for bytes in [
        br#"{"type":"result","type":"assistant","uuid":"duplicate","session_id":"native-session"}
"#.as_slice(),
        br#"{"type":"assistant","uuid":"duplicate","session_id":"native-session","parent_tool_use_id":null,"message":{"id":"msg","role":"assistant","content":[{"type":"tool_use","id":"tool","name":"Read","input":{"path":"one","path":"two"}}]}}
"#.as_slice(),
    ] {
        let mut p = active();
        assert_eq!(p.receive(bytes), Err(ProtocolError::InvalidMessage));
        assert_eq!(p.state(), State::Failed);
    }
}

#[test]
fn frame_output_and_event_counts_are_bounded() {
    let mut p = protocol();
    assert_eq!(
        p.receive(&vec![b'x'; LINE_BYTES_MAX + 1]),
        Err(ProtocolError::FrameTooLarge)
    );
    let mut p = ready();
    assert_eq!(
        p.start_turn(
            RequestId::new("large").unwrap(),
            &"x".repeat(FRAME_BYTES_MAX)
        ),
        Err(ProtocolError::FrameTooLarge)
    );
    assert_eq!(p.state(), State::Idle);
    let mut p = active();
    let mut bytes = Vec::new();
    for i in 0..129 {
        bytes.extend(wire(
            json!({"type":"future","uuid":format!("id-{i}"),"session_id":"native-session"}),
        ));
    }
    assert_eq!(p.receive(&bytes), Err(ProtocolError::Capacity));
}

#[test]
fn malformed_usage_and_missing_native_identity_never_become_zero_or_success() {
    for bad in [json!(-1), json!(1.5), json!("5"), json!(null)] {
        let mut p = active();
        let mut value = result("bad-usage");
        value["usage"]["output_tokens"] = bad;
        assert_eq!(p.receive(&wire(value)), Err(ProtocolError::InvalidMessage));
    }
    let mut p = protocol();
    let mut value = init();
    value.as_object_mut().unwrap().remove("session_id");
    assert_eq!(p.receive(&wire(value)), Err(ProtocolError::InvalidMessage));
}

#[test]
fn unsupported_actions_cannot_construct_an_execution_adapter() {
    let action = NativeAction::CancelChild {
        session_id: SessionId::new("managed-session").unwrap(),
        native_child_id: SessionId::new("child").unwrap(),
    };
    assert!(matches!(
        ClaudeProtocol::new(action, CredentialRef::new("account").unwrap()),
        Err(ProtocolError::Unsupported)
    ));
    let mut action = launch();
    if let NativeAction::Launch { harness, .. } = &mut action {
        *harness = NativeHarness::Codex;
    }
    assert!(matches!(
        ClaudeProtocol::new(action, CredentialRef::new("account").unwrap()),
        Err(ProtocolError::Unsupported)
    ));
}

#[test]
fn disconnect_and_truncated_frames_never_claim_native_completion() {
    let mut p = active();
    p.receive(b"{\"type\":").unwrap();
    assert_eq!(p.finish(), Err(ProtocolError::Disconnected));
    assert_eq!(p.state(), State::Failed);
    assert_eq!(p.finish(), Err(ProtocolError::InvalidState));
}

#[test]
fn aborted_success_and_assistant_errors_keep_the_observed_failure_information() {
    let mut p = active();
    let mut value = assistant("assistant-error", json!([]));
    value["error"] = json!("rate_limit");
    let events = p.receive(&wire(value)).unwrap();
    assert!(matches!(&events[0].kind, EventKind::AssistantError { code }
        if code == "rate_limit"));
    let mut value = result("aborted-result");
    value["terminal_reason"] = json!("aborted_streaming");
    let events = p.receive(&wire(value)).unwrap();
    assert!(matches!(&events[0].kind,
        EventKind::TurnFinished { outcome:TurnOutcome::Incomplete, terminal_reason, .. }
        if terminal_reason.as_deref() == Some("aborted_streaming")));
}

#[test]
fn a_harness_with_no_sign_in_is_told_apart_from_a_provider_rejecting_the_sign_in() {
    let mut p = active();
    let mut value = assistant(
        "not-signed-in",
        json!([{"type":"text","text":"Not logged in \u{b7} Please run /login"}]),
    );
    value["error"] = json!("authentication_failed");
    let events = p.receive(&wire(value)).unwrap();
    assert!(matches!(&events[0].kind, EventKind::AssistantError { code }
        if code == "not_signed_in"));
    let mut value = assistant(
        "rejected",
        json!([{"type":"text","text":"Invalid API key \u{b7} Please run /login"}]),
    );
    value["error"] = json!("authentication_failed");
    let events = p.receive(&wire(value)).unwrap();
    assert!(matches!(&events[0].kind, EventKind::AssistantError { code }
        if code == "authentication_failed"));
}

#[test]
fn malformed_and_repeated_initialization_cannot_rebind_native_identity() {
    let mut p = ready();
    let mut value = init();
    value["uuid"] = json!("init-2");
    assert_eq!(p.receive(&wire(value)), Err(ProtocolError::StaleMessage));
    let mut p = protocol();
    let mut value = init();
    value["capabilities"] = json!(["interrupt_receipt_v1", 3]);
    assert_eq!(p.receive(&wire(value)), Err(ProtocolError::InvalidMessage));
}

#[test]
fn every_later_turn_opens_with_an_init_for_the_bound_session() {
    let mut p = active();
    p.receive(&wire(result("result-1"))).unwrap();
    p.start_turn(RequestId::new("request-two").unwrap(), "Again")
        .unwrap();
    let mut again = init();
    again["uuid"] = json!("init-2");
    assert!(p.receive(&wire(again)).unwrap().is_empty());
    assert_eq!(p.state(), State::Active);
    let text = p
        .receive(&wire(assistant(
            "assistant-2",
            json!([{"type":"text","text":"two"}]),
        )))
        .unwrap();
    assert_eq!(text.len(), 1);
    p.receive(&wire(result("result-2"))).unwrap();
    assert_eq!(p.state(), State::Idle);

    let mut p = active();
    let mut foreign = init();
    foreign["uuid"] = json!("init-2");
    foreign["session_id"] = json!("other-native-session");
    assert_eq!(p.receive(&wire(foreign)), Err(ProtocolError::StaleMessage));
    assert_eq!(p.state(), State::Failed);

    let mut p = active();
    let mut malformed = init();
    malformed["uuid"] = json!("init-2");
    malformed["capabilities"] = json!(["interrupt_receipt_v1", 3]);
    assert_eq!(
        p.receive(&wire(malformed)),
        Err(ProtocolError::InvalidMessage)
    );
}

#[test]
fn invalid_prompt_and_output_expansion_leave_no_reserved_request() {
    let mut p = ready();
    for prompt in ["", "  ", "unsafe\0prompt"] {
        assert_eq!(
            p.start_turn(RequestId::new("same-request").unwrap(), prompt),
            Err(ProtocolError::InvalidInput)
        );
        assert_eq!(p.state(), State::Idle);
    }
    let prompt = "\n".repeat(FRAME_BYTES_MAX / 2);
    assert_eq!(
        p.start_turn(RequestId::new("same-request").unwrap(), &prompt),
        Err(ProtocolError::InvalidInput)
    );
    let prompt = format!("x{}", "\n".repeat(FRAME_BYTES_MAX / 2));
    assert_eq!(
        p.start_turn(RequestId::new("same-request").unwrap(), &prompt),
        Err(ProtocolError::FrameTooLarge)
    );
    assert!(
        p.start_turn(RequestId::new("same-request").unwrap(), "valid")
            .is_ok()
    );
}
