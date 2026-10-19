use crazyagent::adapters::claude::{
    ClaudeProtocol, ProtocolError as ClaudeError, State as ClaudeState,
};
use crazyagent::adapters::codex::{
    CodexProtocol, Event, FRAME_BYTES_MAX, ProtocolError as CodexError, State as CodexState,
    TurnStatus,
};
use crazyagent::adapters::codex_events::{BridgeError, CodexEventBridge};
use crazyagent::adapters::events::{
    Capability, EventKind, HarnessEvent, NativeRequestId, ObservedEvent, RateLimitStatus, Support,
    TurnOutcome,
};
use crazyagent::adapters::{NativeAction, NativeHarness};
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

fn wire(value: Value) -> Vec<u8> {
    let mut bytes = serde_json::to_vec(&value).unwrap();
    bytes.push(b'\n');
    bytes
}

fn request_id(bytes: &[u8]) -> i64 {
    serde_json::from_slice::<Value>(bytes).unwrap()["id"]
        .as_i64()
        .unwrap()
}

fn codex_reply(protocol: &mut CodexProtocol, id: i64, result: Value) -> Vec<Event> {
    protocol
        .receive(&wire(json!({"id":id,"result":result})))
        .unwrap()
}

fn codex_ready() -> CodexProtocol {
    let mut protocol = CodexProtocol::new();
    let initialize = protocol.initialize().unwrap();
    assert_eq!(
        codex_reply(
            &mut protocol,
            request_id(&initialize),
            json!({
                "userAgent":"protocol-test-data", "codexHome":"/protocol-test-data",
                "platformFamily":"unix", "platformOs":"macos"
            }),
        ),
        vec![Event::Initialized]
    );
    protocol.initialized().unwrap();
    protocol
}

fn codex_idle() -> (CodexProtocol, CodexEventBridge) {
    let mut protocol = codex_ready();
    let mut bridge = CodexEventBridge::new(
        action(NativeHarness::Codex),
        CredentialRef::new("pinned-account").unwrap(),
    )
    .unwrap();
    let request = protocol.start_thread(Path::new("/worktree")).unwrap();
    let events = codex_reply(
        &mut protocol,
        request_id(&request),
        json!({
            "thread":{
                "id":"native-thread", "cliVersion":"0.163.0", "createdAt":1,
                "updatedAt":1, "cwd":"/worktree", "ephemeral":false,
                "modelProvider":"protocol-test-data", "preview":"", "projectId":null,
                "sessionId":"unprojected-session", "source":"appServer",
                "status":{"type":"idle"}, "turns":[]
            },
            "cwd":"/worktree", "model":"protocol-test-data",
            "modelProvider":"protocol-test-data", "approvalPolicy":"on-request",
            "approvalsReviewer":"user", "sandbox":{"type":"readOnly","networkAccess":false}
        }),
    );
    assert_eq!(events.len(), 1);
    bridge.translate(events[0].clone(), None).unwrap();
    (protocol, bridge)
}

fn codex_active() -> (CodexProtocol, CodexEventBridge) {
    let (mut protocol, mut bridge) = codex_idle();
    let request = protocol.start_turn("Read the file").unwrap();
    let events = codex_reply(
        &mut protocol,
        request_id(&request),
        json!({"turn":{"id":"native-turn","items":[],"status":"inProgress"}}),
    );
    assert_eq!(events.len(), 1);
    bridge.translate(events[0].clone(), None).unwrap();
    (protocol, bridge)
}

fn codex_read(protocol: &mut CodexProtocol, result: Value) -> (i64, Vec<Event>) {
    let request = protocol.read_account_rate_limits().unwrap();
    let id = request_id(&request);
    (id, codex_reply(protocol, id, result))
}

fn assert_invalid_codex_result(result: Value) {
    let mut protocol = codex_ready();
    let request = protocol.read_account_rate_limits().unwrap();
    assert_eq!(
        protocol.receive(&wire(json!({"id":request_id(&request),"result":result}))),
        Err(CodexError::InvalidMessage)
    );
    assert_eq!(protocol.state(), CodexState::Failed);
}

fn claude_new() -> ClaudeProtocol {
    ClaudeProtocol::new(
        action(NativeHarness::Claude),
        CredentialRef::new("pinned-account").unwrap(),
    )
    .unwrap()
}

fn claude_idle() -> ClaudeProtocol {
    let mut protocol = claude_new();
    let events = protocol
        .receive(&wire(json!({
            "type":"system", "subtype":"init", "uuid":"init-1",
            "session_id":"native-session", "model":"observed-model",
            "claude_code_version":"2.1.301", "cwd":"/worktree", "tools":[],
            "mcp_servers":[], "apiKeySource":"none", "permissionMode":"default",
            "slash_commands":[], "output_style":"default", "skills":[], "plugins":[],
            "capabilities":["interrupt_receipt_v1"]
        })))
        .unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(protocol.state(), ClaudeState::Idle);
    protocol
}

fn rate_frame(uuid: &str, info: Value) -> Value {
    json!({
        "type":"rate_limit_event", "uuid":uuid,
        "session_id":"native-session", "rate_limit_info":info
    })
}

fn claude_rate(protocol: &mut ClaudeProtocol, uuid: &str, info: Value) -> HarnessEvent {
    let mut events = protocol.receive(&wire(rate_frame(uuid, info))).unwrap();
    assert_eq!(events.len(), 1);
    events.pop().unwrap()
}

fn assert_invalid_claude_rate(info: Value, error: ClaudeError) {
    let mut protocol = claude_idle();
    assert_eq!(
        protocol.receive(&wire(rate_frame("rate-invalid", info))),
        Err(error)
    );
    assert_eq!(protocol.state(), ClaudeState::Failed);
}

#[test]
fn codex_read_uses_correlated_passive_request_and_one_pending_slot() {
    let mut protocol = CodexProtocol::new();
    assert_eq!(
        protocol.read_account_rate_limits(),
        Err(CodexError::InvalidState)
    );
    let mut protocol = codex_ready();
    let request = protocol.read_account_rate_limits().unwrap();
    let decoded: Value = serde_json::from_slice(&request).unwrap();
    assert_eq!(decoded["method"], "account/rateLimits/read");
    assert_eq!(
        decoded["params"],
        json!({"excludeResetCreditDetails":true,"supportsLunaReserve":false})
    );
    assert!(request_id(&request) > 0);
    assert_eq!(protocol.state(), CodexState::Ready);
    assert_eq!(
        protocol.read_account_rate_limits(),
        Err(CodexError::InvalidState)
    );
    assert_eq!(
        protocol.start_thread(Path::new("/worktree")),
        Err(CodexError::InvalidState)
    );
    assert_eq!(protocol.state(), CodexState::Ready);
    let events = codex_reply(
        &mut protocol,
        request_id(&request),
        json!({"rateLimits":{}}),
    );
    assert_eq!(events.len(), 1);
    assert!(protocol.start_thread(Path::new("/worktree")).is_ok());
}

#[test]
fn codex_missing_and_null_permission_remain_unknown_but_false_is_observed() {
    let mut protocol = codex_ready();
    let (missing_id, missing) = codex_read(&mut protocol, json!({"rateLimits":{}}));
    assert_eq!(
        missing,
        vec![Event::AccountRateLimitsRead {
            id: missing_id,
            account_id: None,
            ordinary_usage_allowed: None
        }]
    );
    let (null_id, null) = codex_read(
        &mut protocol,
        json!({"rateLimits":{},"accountId":null,"ordinaryUsageAllowed":null}),
    );
    assert_eq!(
        null,
        vec![Event::AccountRateLimitsRead {
            id: null_id,
            account_id: None,
            ordinary_usage_allowed: None
        }]
    );
    let (false_id, denied) = codex_read(
        &mut protocol,
        json!({"rateLimits":{},"accountId":"backend-account","ordinaryUsageAllowed":false}),
    );
    assert_eq!(
        denied,
        vec![Event::AccountRateLimitsRead {
            id: false_id,
            account_id: Some("backend-account".to_owned()),
            ordinary_usage_allowed: Some(false)
        }]
    );
    assert!(missing_id < null_id && null_id < false_id);
    assert_eq!(protocol.state(), CodexState::Ready);
}

#[test]
fn codex_allowed_permission_does_not_require_account_identity() {
    let mut protocol = codex_ready();
    let (id, events) = codex_read(
        &mut protocol,
        json!({"rateLimits":{"primary":{"usedPercent":0,"windowDurationMins":300,
            "resetsAt":2000000000}},"ordinaryUsageAllowed":true}),
    );
    assert_eq!(
        events,
        vec![Event::AccountRateLimitsRead {
            id,
            account_id: None,
            ordinary_usage_allowed: Some(true)
        }]
    );
    assert_eq!(protocol.state(), CodexState::Ready);
}

#[test]
fn codex_account_read_does_not_claim_thread_identity_or_erase_active_turn() {
    let (mut protocol, mut bridge) = codex_active();
    let (_, events) = codex_read(
        &mut protocol,
        json!({"rateLimits":{},"accountId":"backend-account","ordinaryUsageAllowed":true}),
    );
    let observed = bridge.translate(events[0].clone(), None).unwrap();
    assert_eq!(observed.request_id, None);
    assert_eq!(observed.native_session_id, None);
    assert_eq!(observed.native_thread_id, None);
    assert_eq!(observed.native_turn_id, None);
    assert_eq!(observed.native_event_id, None);
    assert_eq!(observed.native_user_message_uuid, None);
    assert_eq!(protocol.state(), CodexState::Active);
    let completion = protocol
        .receive(&wire(json!({
            "method":"turn/completed", "params":{"threadId":"native-thread",
                "turn":{"id":"native-turn","items":[],"status":"completed"}}
        })))
        .unwrap();
    assert_eq!(
        completion,
        vec![Event::TurnCompleted {
            turn_id: "native-turn".to_owned(),
            status: TurnStatus::Completed
        }]
    );
    let finished = bridge.translate(completion[0].clone(), None).unwrap();
    assert_eq!(finished.native_thread_id.as_deref(), Some("native-thread"));
    assert_eq!(finished.native_turn_id.as_deref(), Some("native-turn"));
    assert_eq!(protocol.state(), CodexState::Idle);
}

#[test]
fn codex_bridge_correlates_from_actual_dispatch_map_and_preserves_managed_provenance() {
    let (mut protocol, mut bridge) = codex_idle();
    let request = protocol.read_account_rate_limits().unwrap();
    let id = request_id(&request);
    let mut dispatched = BTreeMap::new();
    let local_id = RequestId::new("account-read-one").unwrap();
    dispatched.insert(id, local_id.clone());
    let events = codex_reply(
        &mut protocol,
        id,
        json!({
            "rateLimits":{}, "accountId":"backend-account", "ordinaryUsageAllowed":false
        }),
    );
    let Event::AccountRateLimitsRead { id: reply_id, .. } = &events[0] else {
        panic!("expected an account read reply");
    };
    let observed = bridge
        .translate(events[0].clone(), dispatched.remove(reply_id))
        .unwrap();
    assert!(dispatched.is_empty());
    assert_eq!(observed.request_id, Some(local_id));
    assert_eq!(
        observed.provenance.session_id,
        SessionId::new("managed-session").unwrap()
    );
    assert_eq!(observed.provenance.harness, NativeHarness::Codex);
    assert_eq!(
        observed.provenance.account_ref,
        CredentialRef::new("pinned-account").unwrap()
    );
    assert_eq!(
        observed.provenance.profile_ref,
        CredentialRef::new("native-profile").unwrap()
    );
    assert_eq!(
        observed.provenance.worktree_lease,
        RequestId::new("owned-lease").unwrap()
    );
    assert_eq!(
        observed.kind,
        EventKind::AccountRateLimitsRead {
            id: NativeRequestId::Number(id),
            account_id: Some("backend-account".to_owned()),
            ordinary_usage_allowed: Some(false)
        }
    );
    assert_eq!(protocol.state(), CodexState::Idle);
}

#[test]
fn codex_fragmented_reply_and_coalesced_notification_keep_distinct_contracts() {
    let mut protocol = codex_ready();
    let request = protocol.read_account_rate_limits().unwrap();
    let id = request_id(&request);
    let bytes = wire(json!({"id":id,"result":{
        "rateLimits":{},"accountId":"backend-account","ordinaryUsageAllowed":false
    }}));
    let split = bytes.len() / 2;
    assert_eq!(
        protocol.receive(&bytes[..split]).unwrap(),
        Vec::<Event>::new()
    );
    assert_eq!(protocol.state(), CodexState::Ready);
    let mut remainder = bytes[split..].to_vec();
    remainder.extend(wire(
        json!({"method":"account/rateLimits/updated","params":{
            "rateLimits":{},"accountId":"uncontracted-account","ordinaryUsageAllowed":true
        }}),
    ));
    assert_eq!(
        protocol.receive(&remainder).unwrap(),
        vec![
            Event::AccountRateLimitsRead {
                id,
                account_id: Some("backend-account".to_owned()),
                ordinary_usage_allowed: Some(false)
            },
            Event::UnsupportedNotification {
                method: "account/rateLimits/updated".to_owned()
            }
        ]
    );
    assert_eq!(protocol.state(), CodexState::Ready);
}

#[test]
fn codex_notification_cannot_satisfy_pending_account_read() {
    let mut protocol = codex_ready();
    let request = protocol.read_account_rate_limits().unwrap();
    assert_eq!(
        protocol
            .receive(&wire(json!({
                "method":"account/rateLimits/updated","params":{"rateLimits":{}}
            })))
            .unwrap(),
        vec![Event::UnsupportedNotification {
            method: "account/rateLimits/updated".to_owned()
        }]
    );
    assert_eq!(
        protocol.read_account_rate_limits(),
        Err(CodexError::InvalidState)
    );
    assert_eq!(
        codex_reply(
            &mut protocol,
            request_id(&request),
            json!({"rateLimits":{}})
        ),
        vec![Event::AccountRateLimitsRead {
            id: request_id(&request),
            account_id: None,
            ordinary_usage_allowed: None
        }]
    );
}

#[test]
fn codex_wrong_numeric_read_id_is_stale() {
    let mut protocol = codex_ready();
    let request = protocol.read_account_rate_limits().unwrap();
    assert_eq!(
        protocol.receive(&wire(json!({
            "id":request_id(&request)+1,"result":{"rateLimits":{}}
        }))),
        Err(CodexError::StaleMessage)
    );
    assert_eq!(protocol.state(), CodexState::Failed);
}

#[test]
fn codex_text_id_does_not_match_numeric_read_id() {
    let mut protocol = codex_ready();
    let request = protocol.read_account_rate_limits().unwrap();
    assert_eq!(
        protocol.receive(&wire(json!({
            "id":request_id(&request).to_string(),"result":{"rateLimits":{}}
        }))),
        Err(CodexError::StaleMessage)
    );
    assert_eq!(protocol.state(), CodexState::Failed);
}

#[test]
fn codex_duplicate_account_reply_cannot_be_replayed() {
    let mut protocol = codex_ready();
    let (id, events) = codex_read(&mut protocol, json!({"rateLimits":{}}));
    assert_eq!(events.len(), 1);
    assert_eq!(
        protocol.receive(&wire(json!({"id":id,"result":{"rateLimits":{}}}))),
        Err(CodexError::StaleMessage)
    );
    assert_eq!(protocol.state(), CodexState::Failed);
}

#[test]
fn codex_read_error_keeps_active_turn_and_original_error_scope() {
    let (mut protocol, _) = codex_active();
    let request = protocol.read_account_rate_limits().unwrap();
    let id = request_id(&request);
    assert_eq!(
        protocol
            .receive(&wire(json!({
                "id":id,"error":{"code":-32000,"message":"usage read unavailable"}
            })))
            .unwrap(),
        vec![Event::RequestFailed {
            id,
            method: "account/rateLimits/read",
            code: -32000,
            message: "usage read unavailable".to_owned()
        }]
    );
    assert_eq!(protocol.state(), CodexState::Active);
    assert!(protocol.interrupt().is_ok());
}

#[test]
fn codex_account_fact_types_are_validated_without_coercion() {
    assert_invalid_codex_result(json!({"rateLimits":{},"ordinaryUsageAllowed":"false"}));
    assert_invalid_codex_result(json!({"rateLimits":{},"ordinaryUsageAllowed":0}));
    assert_invalid_codex_result(json!({"rateLimits":{},"accountId":17}));
    assert_invalid_codex_result(json!({"rateLimits":{},"accountId":{}}));
}

#[test]
fn codex_read_requires_actual_snapshot_envelope() {
    assert_invalid_codex_result(json!({"ordinaryUsageAllowed":true}));
    assert_invalid_codex_result(json!({"rateLimits":null,"ordinaryUsageAllowed":true}));
    assert_invalid_codex_result(json!({"rateLimits":[],"ordinaryUsageAllowed":true}));
}

#[test]
fn codex_account_reply_is_bounded_before_fact_emission() {
    let mut protocol = codex_ready();
    let request = protocol.read_account_rate_limits().unwrap();
    let bytes = wire(json!({"id":request_id(&request),"result":{
        "rateLimits":{},"accountId":"a".repeat(FRAME_BYTES_MAX)
    }}));
    assert_eq!(protocol.receive(&bytes), Err(CodexError::FrameTooLarge));
    assert_eq!(protocol.state(), CodexState::Failed);
}

#[test]
fn codex_bridge_rejects_oversized_typed_account_fact() {
    let (_, mut bridge) = codex_idle();
    assert_eq!(
        bridge.translate(
            Event::AccountRateLimitsRead {
                id: 17,
                account_id: Some("a".repeat(262_145)),
                ordinary_usage_allowed: Some(true)
            },
            None
        ),
        Err(BridgeError::FrameTooLarge)
    );
}

#[test]
fn claude_allowed_rate_fact_preserves_unknowns_and_native_session_provenance() {
    let mut protocol = claude_idle();
    let event = claude_rate(&mut protocol, "rate-allowed", json!({"status":"allowed"}));
    assert_eq!(
        event.kind,
        EventKind::RateLimitObserved {
            status: RateLimitStatus::Allowed,
            resets_at_unix_secs: None,
            rate_limit_type: None,
            utilization: None,
            overage_status: None,
            overage_resets_at_unix_secs: None,
            overage_disabled_reason: None
        }
    );
    assert_eq!(event.request_id, None);
    assert_eq!(event.native_session_id, "native-session");
    assert_eq!(event.native_event_id, "rate-allowed");
    assert_eq!(event.native_user_message_uuid, None);
    assert_eq!(
        event.provenance.account_ref,
        CredentialRef::new("pinned-account").unwrap()
    );
    assert_eq!(
        event.provenance.profile_ref,
        CredentialRef::new("native-profile").unwrap()
    );
    assert_eq!(protocol.state(), ClaudeState::Idle);
    let observed = ObservedEvent::from(event);
    assert_eq!(
        observed.native_session_id.as_deref(),
        Some("native-session")
    );
    assert_eq!(observed.native_event_id.as_deref(), Some("rate-allowed"));
    assert_eq!(observed.native_thread_id, None);
    assert_eq!(observed.native_turn_id, None);
    assert_eq!(observed.request_id, None);
}

#[test]
fn claude_warning_preserves_zero_quantities_and_overage_facts() {
    let mut protocol = claude_idle();
    let event = claude_rate(
        &mut protocol,
        "rate-warning",
        json!({
            "status":"allowed_warning", "resetsAt":0, "rateLimitType":"five_hour",
            "utilization":0.0, "overageStatus":"rejected", "overageResetsAt":0,
            "overageDisabledReason":"out_of_credits"
        }),
    );
    assert_eq!(
        event.kind,
        EventKind::RateLimitObserved {
            status: RateLimitStatus::AllowedWarning,
            resets_at_unix_secs: Some(0),
            rate_limit_type: Some("five_hour".to_owned()),
            utilization: Some(0.0),
            overage_status: Some(RateLimitStatus::Rejected),
            overage_resets_at_unix_secs: Some(0),
            overage_disabled_reason: Some("out_of_credits".to_owned())
        }
    );
    let serialized = serde_json::to_value(event).unwrap();
    assert_eq!(serialized["kind"]["status"], "allowed_warning");
    assert_eq!(serialized["kind"]["overage_status"], "rejected");
    assert_eq!(protocol.state(), ClaudeState::Idle);
}

#[test]
fn claude_rejected_utilization_can_exceed_one_without_becoming_generic_quota() {
    let mut protocol = claude_idle();
    let event = claude_rate(
        &mut protocol,
        "rate-rejected",
        json!({
            "status":"rejected", "utilization":1.25, "rateLimitType":"seven_day",
            "resetsAt":2000000000, "overageStatus":"allowed_warning"
        }),
    );
    assert_eq!(
        event.kind,
        EventKind::RateLimitObserved {
            status: RateLimitStatus::Rejected,
            resets_at_unix_secs: Some(2000000000),
            rate_limit_type: Some("seven_day".to_owned()),
            utilization: Some(1.25),
            overage_status: Some(RateLimitStatus::AllowedWarning),
            overage_resets_at_unix_secs: None,
            overage_disabled_reason: None
        }
    );
    assert_eq!(protocol.state(), ClaudeState::Idle);
}

#[test]
fn claude_session_wide_fact_is_not_attributed_to_pending_prompt() {
    let mut protocol = claude_idle();
    let request = RequestId::new("pending-prompt").unwrap();
    protocol
        .start_turn(request.clone(), "Read the file")
        .unwrap();
    let event = claude_rate(&mut protocol, "rate-active", json!({"status":"allowed"}));
    assert_eq!(event.request_id, None);
    assert_eq!(protocol.state(), ClaudeState::Active);
    let completion = protocol
        .receive(&wire(json!({
            "type":"result", "subtype":"success", "uuid":"result-1",
            "session_id":"native-session", "duration_ms":3, "duration_api_ms":2,
            "num_turns":1, "is_error":false, "result":"done", "stop_reason":"end_turn",
            "usage":{"input_tokens":12,"output_tokens":5,"cache_read_input_tokens":0},
            "total_cost_usd":0.004, "modelUsage":{}, "permission_denials":[]
        })))
        .unwrap();
    assert_eq!(completion.len(), 1);
    assert_eq!(completion[0].request_id, Some(request));
    assert!(matches!(
        completion[0].kind,
        EventKind::TurnFinished {
            outcome: TurnOutcome::Success,
            ..
        }
    ));
    assert_eq!(protocol.state(), ClaudeState::Idle);
}

#[test]
fn claude_fragmented_and_coalesced_limits_preserve_order_without_turn_transition() {
    let mut protocol = claude_idle();
    let first = wire(rate_frame("rate-first", json!({"status":"allowed"})));
    let split = first.len() / 2;
    assert!(protocol.receive(&first[..split]).unwrap().is_empty());
    let mut remaining = first[split..].to_vec();
    remaining.extend(wire(rate_frame(
        "rate-second",
        json!({"status":"rejected"}),
    )));
    let events = protocol.receive(&remaining).unwrap();
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].native_event_id, "rate-first");
    assert_eq!(events[1].native_event_id, "rate-second");
    assert!(matches!(
        events[0].kind,
        EventKind::RateLimitObserved {
            status: RateLimitStatus::Allowed,
            ..
        }
    ));
    assert!(matches!(
        events[1].kind,
        EventKind::RateLimitObserved {
            status: RateLimitStatus::Rejected,
            ..
        }
    ));
    assert_eq!(events[0].request_id, None);
    assert_eq!(events[1].request_id, None);
    assert_eq!(protocol.state(), ClaudeState::Idle);
}

#[test]
fn claude_rate_fact_requires_initialized_native_session() {
    let mut protocol = claude_new();
    assert_eq!(
        protocol.receive(&wire(rate_frame("rate-early", json!({"status":"allowed"})))),
        Err(ClaudeError::InvalidState)
    );
    assert_eq!(protocol.state(), ClaudeState::Failed);
}

#[test]
fn claude_rate_fact_from_different_session_is_stale() {
    let mut protocol = claude_idle();
    let mut value = rate_frame("rate-other", json!({"status":"allowed"}));
    value["session_id"] = json!("different-session");
    assert_eq!(
        protocol.receive(&wire(value)),
        Err(ClaudeError::StaleMessage)
    );
    assert_eq!(protocol.state(), ClaudeState::Failed);
}

#[test]
fn claude_duplicate_rate_event_cannot_be_replayed() {
    let mut protocol = claude_idle();
    let value = rate_frame("rate-duplicate", json!({"status":"allowed"}));
    assert_eq!(protocol.receive(&wire(value.clone())).unwrap().len(), 1);
    assert_eq!(
        protocol.receive(&wire(value)),
        Err(ClaudeError::StaleMessage)
    );
    assert_eq!(protocol.state(), ClaudeState::Failed);
}

#[test]
fn claude_unknown_limit_status_is_explicitly_unsupported() {
    assert_invalid_claude_rate(json!({"status":"future_status"}), ClaudeError::Unsupported);
    assert_invalid_claude_rate(
        json!({"status":"allowed","overageStatus":"future_status"}),
        ClaudeError::Unsupported,
    );
}

#[test]
fn claude_limit_quantities_are_typed_and_not_coerced() {
    assert_invalid_claude_rate(
        json!({"status":"allowed","resetsAt":0.5}),
        ClaudeError::InvalidMessage,
    );
    assert_invalid_claude_rate(
        json!({"status":"allowed","utilization":"0.5"}),
        ClaudeError::InvalidMessage,
    );
    assert_invalid_claude_rate(
        json!({"status":"allowed","overageResetsAt":null}),
        ClaudeError::InvalidMessage,
    );
    assert_invalid_claude_rate(
        json!({"status":"allowed","rateLimitType":17}),
        ClaudeError::InvalidMessage,
    );
}

#[test]
fn claude_rate_fact_requires_structured_info_and_status() {
    assert_invalid_claude_rate(Value::Null, ClaudeError::InvalidMessage);
    assert_invalid_claude_rate(json!({}), ClaudeError::InvalidMessage);
    assert_invalid_claude_rate(json!({"status":true}), ClaudeError::InvalidMessage);
}

#[test]
fn claude_rate_fact_frame_is_bounded_before_emission() {
    let mut protocol = claude_idle();
    let value = rate_frame(
        "rate-large",
        json!({
            "status":"allowed","overageDisabledReason":"x".repeat(1_048_576)
        }),
    );
    assert_eq!(
        protocol.receive(&wire(value)),
        Err(ClaudeError::FrameTooLarge)
    );
    assert_eq!(protocol.state(), ClaudeState::Failed);
}

#[test]
fn auth_progress_and_limit_facts_do_not_enable_native_capabilities() {
    let mut claude = claude_idle();
    let auth = claude
        .receive(&wire(json!({
            "type":"auth_status", "uuid":"auth-status", "session_id":"native-session",
            "isAuthenticating":false, "output":[]
        })))
        .unwrap();
    assert_eq!(auth.len(), 1);
    assert_eq!(
        auth[0].kind,
        EventKind::Unsupported {
            message_type: "auth_status".to_owned(),
            subtype: None
        }
    );
    claude_rate(
        &mut claude,
        "rate-capabilities",
        json!({"status":"allowed"}),
    );
    let (mut codex, mut bridge) = codex_idle();
    let (_, events) = codex_read(
        &mut codex,
        json!({
            "rateLimits":{},"accountId":"backend-account","ordinaryUsageAllowed":true
        }),
    );
    bridge.translate(events[0].clone(), None).unwrap();
    assert_eq!(
        claude.support(Capability::NativeAdmission),
        Support::Unavailable
    );
    assert_eq!(
        claude.support(Capability::CancelChild),
        Support::Unavailable
    );
    assert_eq!(
        claude.support(Capability::LiveRecovery),
        Support::Unavailable
    );
    assert_eq!(
        bridge.support(Capability::NativeAdmission),
        Support::Unavailable
    );
    assert_eq!(
        bridge.support(Capability::CancelChild),
        Support::Unavailable
    );
    assert_eq!(
        bridge.support(Capability::LiveRecovery),
        Support::Unavailable
    );
    assert_eq!(
        claude.support(Capability::StructuredEvents),
        Support::ProtocolOnly
    );
    assert_eq!(
        bridge.support(Capability::StructuredEvents),
        Support::ProtocolOnly
    );
}
