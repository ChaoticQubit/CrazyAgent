use crazyagent::adapters::claude::ClaudeProtocol;
use crazyagent::adapters::codex::{CodexProtocol, Event, ProtocolError, State, WireId};
use crazyagent::adapters::codex_events::{BridgeError, CodexEventBridge, OUTPUT_BYTES_MAX};
use crazyagent::adapters::events::{
    Approval, Capability, EventKind, NativeRequestId, ObservedEvent, Support, TurnOutcome,
};
use crazyagent::adapters::{FIELD_BYTES_MAX, NativeAction, NativeHarness};
use crazyagent::protocol::{CredentialRef, RequestId, SessionId};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::Path;

fn action(harness: NativeHarness) -> NativeAction {
    NativeAction::Launch {
        session_id: SessionId::new("managed-session").unwrap(),
        harness,
        profile_ref: CredentialRef::new("native-profile").unwrap(),
        worktree_lease: RequestId::new("owned-lease").unwrap(),
    }
}

fn bridge() -> CodexEventBridge {
    CodexEventBridge::new(
        action(NativeHarness::Codex),
        CredentialRef::new("pinned-account").unwrap(),
    )
    .unwrap()
}

fn wire(value: Value) -> Vec<u8> {
    let mut bytes = serde_json::to_vec(&value).unwrap();
    bytes.push(b'\n');
    bytes
}

fn reply(protocol: &mut CodexProtocol, id: i64, result: Value) -> Vec<Event> {
    protocol
        .receive(&wire(json!({"id":id,"result":result})))
        .unwrap()
}

fn thread() -> Value {
    json!({
        "id":"native-thread", "cliVersion":"0.163.0", "createdAt":1, "updatedAt":1,
        "cwd":"/worktree", "ephemeral":false, "modelProvider":"protocol-test-data",
        "preview":"", "projectId":null, "sessionId":"unprojected-session",
        "source":"appServer", "status":{"type":"idle"}, "turns":[]
    })
}

fn ready() -> (CodexProtocol, CodexEventBridge) {
    let mut protocol = CodexProtocol::new();
    let mut bridge = bridge();
    protocol.initialize().unwrap();
    let events = reply(
        &mut protocol,
        1,
        json!({
            "userAgent":"protocol-test-data", "codexHome":"/protocol-test-data",
            "platformFamily":"unix", "platformOs":"macos"
        }),
    );
    for event in events {
        bridge.translate(event, None).unwrap();
    }
    protocol.initialized().unwrap();
    protocol.start_thread(Path::new("/worktree")).unwrap();
    let events = reply(
        &mut protocol,
        2,
        json!({
            "thread":thread(), "cwd":"/worktree", "model":"protocol-test-data",
            "modelProvider":"protocol-test-data", "approvalPolicy":"on-request",
            "approvalsReviewer":"user", "sandbox":{"type":"readOnly","networkAccess":false}
        }),
    );
    for event in events {
        bridge.translate(event, None).unwrap();
    }
    (protocol, bridge)
}

fn active() -> (CodexProtocol, CodexEventBridge, RequestId) {
    let (mut protocol, mut bridge) = ready();
    let request = RequestId::new("local-turn-request").unwrap();
    let outgoing = protocol.start_turn("Read the file").unwrap();
    let value: Value = serde_json::from_slice(&outgoing).unwrap();
    let mut mapping = BTreeMap::new();
    mapping.insert(value["id"].as_i64().unwrap(), request.clone());
    for event in reply(
        &mut protocol,
        3,
        json!({"turn":{"id":"native-turn","items":[],"status":"inProgress"}}),
    ) {
        bridge.translate(event, mapping.get(&3).cloned()).unwrap();
    }
    (protocol, bridge, request)
}

fn notice(protocol: &mut CodexProtocol, method: &str, params: Value) -> Vec<Event> {
    protocol
        .receive(&wire(json!({"method":method,"params":params})))
        .unwrap()
}

#[test]
fn initialization_preserves_local_provenance_and_leaves_unreported_native_fields_unknown() {
    let mut protocol = CodexProtocol::new();
    let mut bridge = bridge();
    let outgoing = protocol.initialize().unwrap();
    let sent: Value = serde_json::from_slice(&outgoing).unwrap();
    let local = RequestId::new("initialize-request").unwrap();
    let mapping = BTreeMap::from([(sent["id"].as_i64().unwrap(), local.clone())]);
    let mut events = reply(
        &mut protocol,
        1,
        json!({
            "userAgent":"protocol-test-data", "codexHome":"/protocol-test-data",
            "platformFamily":"unix", "platformOs":"macos"
        }),
    );
    let observed = bridge
        .translate(events.remove(0), mapping.get(&1).cloned())
        .unwrap();
    assert_eq!(observed.provenance.harness, NativeHarness::Codex);
    assert_eq!(observed.provenance.session_id.as_str(), "managed-session");
    assert_eq!(observed.provenance.profile_ref.as_str(), "native-profile");
    assert_eq!(observed.provenance.account_ref.as_str(), "pinned-account");
    assert_eq!(observed.provenance.worktree_lease.as_str(), "owned-lease");
    assert_eq!(observed.request_id, Some(local));
    assert!(observed.native_session_id.is_none());
    assert!(observed.native_thread_id.is_none());
    assert!(observed.native_turn_id.is_none());
    assert!(observed.native_event_id.is_none());
    assert!(observed.native_user_message_uuid.is_none());
    assert_eq!(observed.kind, EventKind::ProtocolInitialized);
}

#[test]
fn thread_and_turn_ids_come_from_parser_events_without_aliasing_native_session() {
    let (mut protocol, mut bridge) = ready();
    protocol.start_turn("Read").unwrap();
    let events = notice(
        &mut protocol,
        "turn/started",
        json!({
            "threadId":"native-thread", "turn":{"id":"native-turn","status":"inProgress","items":[]}
        }),
    );
    let observed = bridge
        .translate(events.into_iter().next().unwrap(), None)
        .unwrap();
    assert_eq!(observed.native_thread_id.as_deref(), Some("native-thread"));
    assert_eq!(observed.native_turn_id.as_deref(), Some("native-turn"));
    assert!(observed.native_session_id.is_none());
    assert!(observed.request_id.is_none());
    assert_eq!(observed.kind, EventKind::TurnStarted);
    assert!(
        reply(
            &mut protocol,
            3,
            json!({"turn":{"id":"native-turn","items":[],"status":"inProgress"}})
        )
        .is_empty()
    );
}

#[test]
fn fragmented_real_delta_stays_a_delta_and_retains_observed_item_and_turn_identity() {
    let (mut protocol, mut bridge, request) = active();
    let bytes = wire(json!({"method":"item/agentMessage/delta","params":{
        "threadId":"native-thread","turnId":"native-turn","itemId":"native-item","delta":"text λ"
    }}));
    let split = bytes.iter().position(|b| *b == 0xce).unwrap() + 1;
    assert!(protocol.receive(&bytes[..split]).unwrap().is_empty());
    let events = protocol.receive(&bytes[split..]).unwrap();
    let observed = bridge
        .translate(events.into_iter().next().unwrap(), Some(request.clone()))
        .unwrap();
    assert_eq!(observed.request_id, Some(request));
    assert_eq!(observed.native_turn_id.as_deref(), Some("native-turn"));
    assert_eq!(
        observed.kind,
        EventKind::AssistantDelta {
            item_id: "native-item".into(),
            text: "text λ".into()
        }
    );
    assert_eq!(protocol.state(), State::Active);
}

#[test]
fn completed_interrupted_and_failed_turns_remain_distinct_with_unknown_usage() {
    for (status, outcome) in [
        ("completed", TurnOutcome::Success),
        ("interrupted", TurnOutcome::Incomplete),
        ("failed", TurnOutcome::Failed),
    ] {
        let (mut protocol, mut bridge, request) = active();
        let events = notice(
            &mut protocol,
            "turn/completed",
            json!({
                "threadId":"native-thread","turn":{"id":"native-turn","items":[],"status":status}
            }),
        );
        let observed = bridge
            .translate(events.into_iter().next().unwrap(), Some(request))
            .unwrap();
        assert_eq!(observed.native_turn_id.as_deref(), Some("native-turn"));
        assert!(matches!(&observed.kind, EventKind::TurnFinished {
            outcome:actual, result, usage, estimated_cost_usd, terminal_reason, ..
        } if *actual == outcome && result.is_none() && estimated_cost_usd.is_none()
            && usage.input_tokens.is_none() && usage.output_tokens.is_none()
            && usage.cache_read_input_tokens.is_none() && usage.cache_creation_input_tokens.is_none()
            && terminal_reason.as_deref() == Some(status)));
        assert_eq!(protocol.state(), State::Idle);
    }
}

#[test]
fn interrupt_acknowledgement_is_not_terminal_completion_or_child_control() {
    let (mut protocol, mut bridge, request) = active();
    protocol.interrupt().unwrap();
    let events = reply(&mut protocol, 4, json!({}));
    let observed = bridge
        .translate(events.into_iter().next().unwrap(), Some(request))
        .unwrap();
    assert_eq!(observed.kind, EventKind::InterruptAcknowledged);
    assert_eq!(observed.native_turn_id.as_deref(), Some("native-turn"));
    assert_eq!(protocol.state(), State::Active);
    assert_eq!(
        bridge.support(Capability::CancelChild),
        Support::Unavailable
    );
    assert_eq!(
        bridge.support(Capability::NativeAdmission),
        Support::Unavailable
    );
    assert_eq!(
        bridge.support(Capability::LiveRecovery),
        Support::Unavailable
    );
    assert_eq!(
        bridge.support(Capability::StructuredEvents),
        Support::ProtocolOnly
    );
}

#[test]
fn request_failure_keeps_actual_wire_correlation_without_guessing_native_turn() {
    let (mut protocol, mut bridge) = ready();
    let outgoing = protocol.start_turn("Read").unwrap();
    let sent: Value = serde_json::from_slice(&outgoing).unwrap();
    let mapping = BTreeMap::from([(
        sent["id"].as_i64().unwrap(),
        RequestId::new("failed-local-request").unwrap(),
    )]);
    let events = protocol
        .receive(&wire(
            json!({"id":3,"error":{"code":-32001,"message":"observed rejection"}}),
        ))
        .unwrap();
    for event in events {
        let Event::RequestFailed { id, .. } = &event else {
            panic!("expected actual correlated request failure")
        };
        let observed = bridge
            .translate(event.clone(), mapping.get(id).cloned())
            .unwrap();
        assert_eq!(
            observed.request_id.as_ref().unwrap().as_str(),
            "failed-local-request"
        );
        assert!(observed.native_turn_id.is_none());
        assert_eq!(
            observed.kind,
            EventKind::RequestFailed {
                id: NativeRequestId::Number(3),
                method: "turn/start".into(),
                code: -32001,
                message: "observed rejection".into()
            }
        );
    }
}

#[test]
fn retryable_turn_error_preserves_native_retry_fact_and_does_not_complete_turn() {
    let (mut protocol, mut bridge, request) = active();
    let events = notice(
        &mut protocol,
        "error",
        json!({
            "threadId":"native-thread","turnId":"native-turn", "error":{"message":"observed retry"},"willRetry":true
        }),
    );
    let observed = bridge
        .translate(events.into_iter().next().unwrap(), Some(request))
        .unwrap();
    assert_eq!(
        observed.kind,
        EventKind::TurnError {
            message: "observed retry".into(),
            will_retry: true
        }
    );
    assert_eq!(protocol.state(), State::Active);
}

#[test]
fn approval_and_unknown_request_ids_keep_number_and_text_distinct_without_authorizing() {
    let (mut protocol, mut bridge, request) = active();
    let params =
        json!({"threadId":"native-thread","turnId":"native-turn","itemId":"item","startedAtMs":1});
    for (id, expected) in [
        (json!(7), NativeRequestId::Number(7)),
        (json!("7"), NativeRequestId::Text("7".into())),
    ] {
        let events = protocol
            .receive(&wire(
                json!({"id":id,"method":"item/commandExecution/requestApproval","params":params}),
            ))
            .unwrap();
        let observed = bridge
            .translate(events.into_iter().next().unwrap(), Some(request.clone()))
            .unwrap();
        assert!(
            matches!(&observed.kind, EventKind::ApprovalRequired { id, approval:Approval::Command, params:actual }
            if *id == expected && *actual == params)
        );
        let typed: WireId = serde_json::from_value(id).unwrap();
        let denial: Value =
            serde_json::from_slice(&protocol.deny_request(&typed).unwrap()).unwrap();
        assert_eq!(denial["result"]["decision"], "decline");
    }
    let events = protocol
        .receive(&wire(
            json!({"id":"future-request","method":"future/method","params":{}}),
        ))
        .unwrap();
    let observed = bridge
        .translate(events.into_iter().next().unwrap(), None)
        .unwrap();
    assert_eq!(
        observed.kind,
        EventKind::UnsupportedRequest {
            id: NativeRequestId::Text("future-request".into()),
            method: "future/method".into()
        }
    );
    assert!(observed.request_id.is_none());
    assert!(observed.native_turn_id.is_none());
}

#[test]
fn unsupported_usage_notification_does_not_fabricate_counters_or_turn_correlation() {
    let (mut protocol, mut bridge, _) = active();
    let events = notice(
        &mut protocol,
        "thread/tokenUsage/updated",
        json!({
            "threadId":"foreign-unvalidated-thread", "turnId":"foreign-unvalidated-turn",
            "tokenUsage":{"inputTokens":23,"outputTokens":7}
        }),
    );
    let observed = bridge
        .translate(events.into_iter().next().unwrap(), None)
        .unwrap();
    assert!(
        matches!(&observed.kind, EventKind::Unsupported { message_type, subtype }
        if message_type == "notification" && subtype.as_deref() == Some("thread/tokenUsage/updated"))
    );
    assert!(observed.native_turn_id.is_none());
    assert!(observed.native_session_id.is_none());
    assert!(observed.request_id.is_none());
    assert_eq!(observed.native_thread_id.as_deref(), Some("native-thread"));
}

#[test]
fn disconnect_is_an_observed_protocol_failure_without_success_or_a_native_event_id() {
    let (mut protocol, bridge, request) = active();
    let error = protocol.finish().unwrap_err();
    assert_eq!(error, ProtocolError::Disconnected);
    let observed = bridge.translate_error(error, Some(request));
    assert_eq!(observed.kind, EventKind::Disconnected);
    assert!(observed.native_event_id.is_none());
    assert!(observed.native_turn_id.is_none());
    assert_eq!(protocol.state(), State::Failed);
}

#[test]
fn malformed_real_frame_error_remains_an_explicit_protocol_failure() {
    let (mut protocol, bridge, _) = active();
    let error = protocol.receive(b"not json\n").unwrap_err();
    let observed = bridge.translate_error(error, None);
    assert_eq!(
        observed.kind,
        EventKind::ProtocolFailure {
            code: "InvalidMessage".into()
        }
    );
    assert!(observed.request_id.is_none());
    assert!(observed.native_turn_id.is_none());
}

#[test]
fn claude_existing_envelope_converts_losslessly_to_the_same_shared_observation() {
    let mut claude = ClaudeProtocol::new(
        action(NativeHarness::Claude),
        CredentialRef::new("claude-account").unwrap(),
    )
    .unwrap();
    claude
        .start_turn(RequestId::new("claude-request").unwrap(), "Read")
        .unwrap();
    let mut events = claude
        .receive(&wire(json!({
            "type":"system","subtype":"init","uuid":"actual-event",
            "session_id":"actual-claude-session","model":"observed-model","capabilities":[]
        })))
        .unwrap();
    let original = events.remove(0);
    let observed: ObservedEvent = original.clone().into();
    assert_eq!(observed.provenance, original.provenance);
    assert_eq!(observed.request_id, original.request_id);
    assert_eq!(observed.kind, original.kind);
    assert_eq!(observed.native_session_id, Some(original.native_session_id));
    assert_eq!(observed.native_event_id, Some(original.native_event_id));
    assert_eq!(
        observed.native_user_message_uuid,
        original.native_user_message_uuid
    );
    assert!(observed.native_thread_id.is_none());
    assert!(observed.native_turn_id.is_none());
}

#[test]
fn wrong_harness_actions_remain_unsupported() {
    assert!(matches!(
        CodexEventBridge::new(
            action(NativeHarness::Claude),
            CredentialRef::new("account").unwrap()
        ),
        Err(BridgeError::Unsupported)
    ));
    assert!(matches!(
        CodexEventBridge::new(
            NativeAction::CancelChild {
                session_id: SessionId::new("managed-session").unwrap(),
                native_child_id: SessionId::new("child").unwrap()
            },
            CredentialRef::new("account").unwrap()
        ),
        Err(BridgeError::Unsupported)
    ));
}

#[test]
fn public_typed_events_cannot_expand_shared_output_without_a_bound() {
    let mut bridge = bridge();
    let event = Event::UnsupportedRequest {
        id: WireId::Text("id".into()),
        method: "x".repeat(OUTPUT_BYTES_MAX),
    };
    assert!(matches!(
        bridge.translate(event, None),
        Err(BridgeError::FrameTooLarge)
    ));
}

#[test]
fn a_codex_message_past_a_field_is_clipped_instead_of_failing_the_turn() {
    let (mut protocol, mut bridge, request) = active();
    let text = "é\u{1b}".repeat(25_000);
    for (method, params) in [
        (
            "item/agentMessage/delta",
            json!({"threadId":"native-thread","turnId":"native-turn","itemId":"item","delta":text}),
        ),
        (
            "error",
            json!({"threadId":"native-thread","turnId":"native-turn","error":{"message":text},"willRetry":true}),
        ),
    ] {
        let events = notice(&mut protocol, method, params);
        let observed = bridge
            .translate(events.into_iter().next().unwrap(), Some(request.clone()))
            .unwrap();
        let clipped = match observed.kind {
            EventKind::AssistantDelta { text, .. } => text,
            EventKind::TurnError { message, .. } => message,
            other => panic!("{other:?}"),
        };
        assert!(clipped.len() <= FIELD_BYTES_MAX);
        let (kept, note) = clipped.rsplit_once('\n').unwrap();
        assert!(text.starts_with(kept));
        assert_eq!(
            note,
            format!("[{} more bytes not shown]", text.len() - kept.len())
        );
        assert_eq!(protocol.state(), State::Active);
    }
}

#[test]
fn a_codex_approval_past_a_field_is_summarized_instead_of_failing_the_turn() {
    let (mut protocol, mut bridge, request) = active();
    let command = format!(
        "cat > note.txt <<'EOF'\n{}EOF",
        "\u{1}line\n".repeat(50_000)
    );
    let params = json!({
        "threadId":"native-thread", "turnId":"native-turn", "itemId":"item", "startedAtMs":1,
        "command":command, "cwd":"/worktree", "reason":"write the note"
    });
    let size = serde_json::to_vec(&params).unwrap().len();
    let events = protocol
        .receive(&wire(
            json!({"id":9,"method":"item/commandExecution/requestApproval","params":params}),
        ))
        .unwrap();
    let observed = bridge
        .translate(events.into_iter().next().unwrap(), Some(request))
        .unwrap();
    assert_eq!(observed.native_turn_id.as_deref(), Some("native-turn"));
    let summary = json!({
        "threadId":"native-thread", "turnId":"native-turn", "itemId":"item", "startedAtMs":1,
        "omitted_bytes":size
    });
    assert!(matches!(
        &observed.kind,
        EventKind::ApprovalRequired { id: NativeRequestId::Number(9), approval: Approval::Command, params }
        if *params == summary
    ));
    let denial: Value =
        serde_json::from_slice(&protocol.deny_request(&WireId::Number(9)).unwrap()).unwrap();
    assert_eq!(denial["result"]["decision"], "decline");
    assert_eq!(protocol.state(), State::Active);
}
