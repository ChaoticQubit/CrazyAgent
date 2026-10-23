use crazyagent::adapters::codex::{
    AuthAccount, AuthControl, DEVELOPER_INSTRUCTIONS_BYTES_MAX, FRAME_BYTES_MAX, State,
};
use crazyagent::adapters::events::{EventKind, TokenUsage, TurnOutcome};
use crazyagent::adapters::{NativeAction, NativeHarness};
use crazyagent::engine::codex_host::{CodexHostBatch, CodexHostProtocol};
use crazyagent::protocol::{CredentialRef, RequestId, SessionId};
use serde_json::{Value, json};
use std::path::Path;

fn action() -> NativeAction {
    NativeAction::Launch {
        session_id: SessionId::new("unit-local-session").unwrap(),
        harness: NativeHarness::Codex,
        profile_ref: CredentialRef::new("unit-selected-profile").unwrap(),
        worktree_lease: RequestId::new("unit-owned-lease").unwrap(),
    }
}

fn protocol() -> CodexHostProtocol {
    CodexHostProtocol::new(
        action(),
        CredentialRef::new("unit-selected-account").unwrap(),
    )
    .unwrap()
}

fn wire(value: Value) -> Vec<u8> {
    let mut bytes = serde_json::to_vec(&value).unwrap();
    bytes.push(b'\n');
    bytes
}

fn message(bytes: &[u8]) -> Value {
    assert_eq!(bytes.last(), Some(&b'\n'));
    assert_eq!(bytes.iter().filter(|byte| **byte == b'\n').count(), 1);
    serde_json::from_slice(bytes).unwrap()
}

fn reply(protocol: &mut CodexHostProtocol, id: i64, result: Value) -> CodexHostBatch {
    protocol
        .receive(&wire(json!({"id":id,"result":result})))
        .unwrap()
}

fn initialized() -> CodexHostProtocol {
    let mut host = protocol();
    assert_eq!(message(&host.initialize().unwrap())["id"], 1);
    let observed = reply(
        &mut host,
        1,
        json!({
            "userAgent":"unit-wire-input", "codexHome":"/unit/profile",
            "platformFamily":"unix", "platformOs":"macos"
        }),
    );
    assert_eq!(observed.events.len(), 1);
    assert_eq!(observed.events[0].kind, EventKind::ProtocolInitialized);
    assert_eq!(observed.outbound.len(), 1);
    assert_eq!(
        message(&observed.outbound[0]),
        json!({"method":"initialized"})
    );
    assert!(observed.auth.is_empty());
    assert_eq!(host.state(), State::Ready);
    host
}

fn idle() -> CodexHostProtocol {
    let mut host = initialized();
    let outgoing = message(&host.start_thread(Path::new("/unit/project")).unwrap());
    let thread = json!({
        "id":"unit-native-thread", "cliVersion":"0.163.0", "createdAt":1,
        "updatedAt":1, "cwd":"/unit/project", "ephemeral":false,
        "modelProvider":"unit-provider-input", "preview":"", "projectId":null,
        "sessionId":"unit-unprojected-session", "source":"appServer",
        "status":{"type":"idle"}, "turns":[]
    });
    let batch = reply(
        &mut host,
        outgoing["id"].as_i64().unwrap(),
        json!({
            "thread":thread, "cwd":"/unit/project", "model":"unit-model-input",
            "modelProvider":"unit-provider-input", "approvalPolicy":"on-request",
            "approvalsReviewer":"user", "sandbox":{"type":"readOnly","networkAccess":false}
        }),
    );
    assert_eq!(batch.events.len(), 1);
    assert_eq!(batch.events[0].kind, EventKind::ThreadStarted);
    assert!(batch.events[0].request_id.is_none());
    assert_eq!(host.state(), State::Idle);
    host
}

fn active() -> (CodexHostProtocol, RequestId) {
    let mut host = idle();
    let request = RequestId::new("unit-local-turn").unwrap();
    let outgoing = message(&host.start_turn(request.clone(), "unit prompt").unwrap());
    let batch = reply(
        &mut host,
        outgoing["id"].as_i64().unwrap(),
        json!({"turn":{"id":"unit-native-turn","items":[],"status":"inProgress"}}),
    );
    assert_eq!(batch.events.len(), 1);
    assert_eq!(batch.events[0].kind, EventKind::TurnStarted);
    assert_eq!(batch.events[0].request_id, Some(request.clone()));
    assert_eq!(host.state(), State::Active);
    (host, request)
}

fn completed(status: &str) -> Vec<u8> {
    wire(json!({
        "method":"turn/completed", "params":{
            "threadId":"unit-native-thread",
            "turn":{"id":"unit-native-turn","items":[],"status":status}
        }
    }))
}

#[test]
fn codex_host_initialization_waits_for_complete_frame_before_initialized_notification() {
    let mut host = protocol();
    let request = message(&host.initialize().unwrap());
    assert_eq!(request["method"], "initialize");
    assert_eq!(request["params"]["capabilities"]["experimentalApi"], false);
    let bytes = wire(json!({"id":1,"result":{
        "userAgent":"unit-wire-input","codexHome":"/unit/profile",
        "platformFamily":"unix","platformOs":"macos"
    }}));
    let partial = host.receive(&bytes[..bytes.len() - 1]).unwrap();
    assert!(partial.events.is_empty());
    assert!(partial.outbound.is_empty());
    assert!(partial.auth.is_empty());
    assert_eq!(host.state(), State::Initializing);
    let complete = host.receive(&bytes[bytes.len() - 1..]).unwrap();
    assert_eq!(complete.events[0].kind, EventKind::ProtocolInitialized);
    assert!(complete.events[0].request_id.is_none());
    assert!(complete.events[0].native_session_id.is_none());
    assert_eq!(
        message(&complete.outbound[0]),
        json!({"method":"initialized"})
    );
    assert_eq!(host.state(), State::Ready);
}

#[test]
fn codex_host_thread_start_preserves_read_only_sandbox_and_exact_project_cwd() {
    let mut host = initialized();
    let outgoing = message(
        &host
            .start_thread(Path::new("/unit/declared-project"))
            .unwrap(),
    );
    assert_eq!(outgoing["method"], "thread/start");
    assert_eq!(outgoing["params"]["cwd"], "/unit/declared-project");
    assert_eq!(outgoing["params"]["approvalPolicy"], "on-request");
    assert_eq!(outgoing["params"]["approvalsReviewer"], "user");
    assert_eq!(outgoing["params"]["sandbox"], "read-only");
    assert_eq!(host.state(), State::ThreadStarting);
}

#[test]
fn codex_host_master_thread_carries_instructions_on_stdin_with_the_same_read_only_policy() {
    let project = Path::new("/unit/declared-project");
    let instructions = "# Project context from CrazyAgent\n\nThe codename is \"BLUE-HERON-77\".\n";
    let mut host = initialized();
    let outgoing = message(&host.start_instructed_thread(project, instructions).unwrap());
    assert_eq!(outgoing["method"], "thread/start");
    assert_eq!(outgoing["params"]["developerInstructions"], instructions);
    assert_eq!(outgoing["params"]["cwd"], "/unit/declared-project");
    assert_eq!(outgoing["params"]["approvalPolicy"], "on-request");
    assert_eq!(outgoing["params"]["approvalsReviewer"], "user");
    assert_eq!(outgoing["params"]["sandbox"], "read-only");
    assert_eq!(host.state(), State::ThreadStarting);

    let mut plain = initialized();
    let outgoing = message(&plain.start_thread(project).unwrap());
    assert!(outgoing["params"].get("developerInstructions").is_none());

    let oversized = "x".repeat(DEVELOPER_INSTRUCTIONS_BYTES_MAX + 1);
    for refused in ["", "  \n", "before\0after", oversized.as_str()] {
        let mut host = initialized();
        assert!(host.start_instructed_thread(project, refused).is_err());
        assert_eq!(host.state(), State::Ready);
    }
    let largest = "x".repeat(DEVELOPER_INSTRUCTIONS_BYTES_MAX);
    let mut host = initialized();
    let outgoing = message(&host.start_instructed_thread(project, &largest).unwrap());
    assert_eq!(
        outgoing["params"]["developerInstructions"],
        largest.as_str()
    );
}

#[test]
fn codex_host_account_read_uses_nonrefresh_rpc_separate_from_normalized_events() {
    let mut host = initialized();
    let outgoing = message(&host.read_account().unwrap());
    assert_eq!(outgoing["method"], "account/read");
    assert_eq!(outgoing["params"]["refreshToken"], false);
    let batch = reply(
        &mut host,
        outgoing["id"].as_i64().unwrap(),
        json!({"account":{"type":"chatgpt","email":null,"planType":"plus"},
            "requiresOpenaiAuth":true}),
    );
    assert!(batch.events.is_empty());
    assert!(batch.outbound.is_empty());
    assert!(matches!(&batch.auth[..], [AuthControl::AccountRead {
        account: Some(AuthAccount::Chatgpt { email: None, plan_type }),
        requires_openai_auth: true, ..
    }] if plan_type == "plus"));
    assert_eq!(host.state(), State::Ready);
}

#[test]
fn codex_host_missing_account_and_false_auth_requirement_do_not_become_readiness() {
    let mut host = initialized();
    let outgoing = message(&host.read_account().unwrap());
    let batch = reply(
        &mut host,
        outgoing["id"].as_i64().unwrap(),
        json!({"account":null,"requiresOpenaiAuth":false}),
    );
    assert!(matches!(
        &batch.auth[..],
        [AuthControl::AccountRead {
            account: None,
            requires_openai_auth: false,
            ..
        }]
    ));
    assert!(batch.events.is_empty());
    assert!(batch.outbound.is_empty());
}

#[test]
fn codex_host_sensitive_account_failure_stays_out_of_normalized_event_and_debug_output() {
    let mut host = initialized();
    let outgoing = message(&host.read_account().unwrap());
    let batch = host
        .receive(&wire(json!({
            "id":outgoing["id"], "error":{"code":-32000,
                "message":"unit-private-email@example.invalid unit-private-token"}
        })))
        .unwrap();
    assert!(batch.events.is_empty());
    assert!(matches!(
        &batch.auth[..],
        [AuthControl::RequestFailed {
            method: "account/read",
            code: -32000,
            ..
        }]
    ));
    let debug = format!("{:?}", batch.auth);
    assert!(!debug.contains("unit-private-email"));
    assert!(!debug.contains("unit-private-token"));
}

#[test]
fn codex_host_coalesced_delta_and_completion_keep_exact_dispatch_and_native_provenance() {
    let (mut host, request) = active();
    let mut bytes = wire(json!({"method":"item/agentMessage/delta","params":{
        "threadId":"unit-native-thread","turnId":"unit-native-turn",
        "itemId":"unit-native-item","delta":"unit answer"
    }}));
    bytes.extend(completed("completed"));
    let batch = host.receive(&bytes).unwrap();
    assert_eq!(batch.events.len(), 2);
    for event in &batch.events {
        assert_eq!(event.request_id, Some(request.clone()));
        assert_eq!(
            event.provenance.account_ref.as_str(),
            "unit-selected-account"
        );
        assert_eq!(
            event.provenance.profile_ref.as_str(),
            "unit-selected-profile"
        );
        assert_eq!(event.provenance.worktree_lease.as_str(), "unit-owned-lease");
        assert_eq!(
            event.native_thread_id.as_deref(),
            Some("unit-native-thread")
        );
        assert_eq!(event.native_turn_id.as_deref(), Some("unit-native-turn"));
        assert!(event.native_session_id.is_none());
        assert!(event.native_event_id.is_none());
    }
    assert!(
        matches!(&batch.events[0].kind, EventKind::AssistantDelta { text, .. }
        if text == "unit answer")
    );
    assert!(matches!(&batch.events[1].kind, EventKind::TurnFinished {
        outcome: TurnOutcome::Success, result: None, usage, estimated_cost_usd: None, ..
    } if usage == &TokenUsage::default()));
    assert_eq!(host.state(), State::Idle);
}

#[test]
fn codex_host_finished_correlation_clears_before_unsolicited_notice_and_next_dispatch() {
    let (mut host, request) = active();
    let terminal = host.receive(&completed("completed")).unwrap();
    assert_eq!(terminal.events[0].request_id, Some(request));
    let notice = host
        .receive(&wire(json!({"method":"unit/unknown","params":{}})))
        .unwrap();
    assert!(notice.events[0].request_id.is_none());
    let next = RequestId::new("unit-second-local-turn").unwrap();
    let outgoing = message(&host.start_turn(next.clone(), "unit second prompt").unwrap());
    let batch = reply(
        &mut host,
        outgoing["id"].as_i64().unwrap(),
        json!({
            "turn":{"id":"unit-second-native-turn","items":[],"status":"inProgress"}
        }),
    );
    assert_eq!(batch.events[0].request_id, Some(next));
    assert_eq!(
        batch.events[0].native_turn_id.as_deref(),
        Some("unit-second-native-turn")
    );
}

#[test]
fn codex_host_rejected_busy_dispatch_cannot_replace_current_turn_correlation() {
    let (mut host, request) = active();
    assert!(
        host.start_turn(RequestId::new("unit-rejected-turn").unwrap(), "other")
            .is_err()
    );
    let batch = host.receive(&completed("completed")).unwrap();
    assert_eq!(batch.events[0].request_id, Some(request));
    assert_eq!(host.state(), State::Idle);
}

#[test]
fn codex_host_invalid_prompt_does_not_reserve_a_request_or_poison_next_valid_dispatch() {
    let mut host = idle();
    assert!(
        host.start_turn(RequestId::new("unit-empty-turn").unwrap(), " ")
            .is_err()
    );
    let request = RequestId::new("unit-valid-after-empty").unwrap();
    let outgoing = message(
        &host
            .start_turn(request.clone(), "unit valid prompt")
            .unwrap(),
    );
    let batch = reply(
        &mut host,
        outgoing["id"].as_i64().unwrap(),
        json!({
            "turn":{"id":"unit-native-turn","items":[],"status":"inProgress"}
        }),
    );
    assert_eq!(batch.events[0].request_id, Some(request));
}

#[test]
fn codex_host_interrupt_acknowledgement_never_completes_or_releases_turn_correlation() {
    let (mut host, request) = active();
    let outgoing = message(&host.interrupt().unwrap());
    assert_eq!(outgoing["method"], "turn/interrupt");
    assert_eq!(outgoing["params"]["turnId"], "unit-native-turn");
    let ack = reply(&mut host, outgoing["id"].as_i64().unwrap(), json!({}));
    assert_eq!(ack.events[0].kind, EventKind::InterruptAcknowledged);
    assert_eq!(ack.events[0].request_id, Some(request.clone()));
    assert_eq!(host.state(), State::Active);
    let terminal = host.receive(&completed("interrupted")).unwrap();
    assert_eq!(terminal.events[0].request_id, Some(request));
    assert!(matches!(&terminal.events[0].kind, EventKind::TurnFinished {
        outcome: TurnOutcome::Incomplete, terminal_reason: Some(reason), ..
    } if reason == "interrupted"));
    assert_eq!(host.state(), State::Idle);
}

#[test]
fn codex_host_turn_start_rpc_error_is_correlated_without_invented_terminal_success() {
    let mut host = idle();
    let request = RequestId::new("unit-failed-start").unwrap();
    let outgoing = message(&host.start_turn(request.clone(), "unit prompt").unwrap());
    let batch = host
        .receive(&wire(json!({"id":outgoing["id"],"error":{
            "code":-32000,"message":"unit model unavailable"
        }})))
        .unwrap();
    assert_eq!(batch.events.len(), 1);
    assert_eq!(batch.events[0].request_id, Some(request));
    assert!(matches!(&batch.events[0].kind, EventKind::RequestFailed {
        method, code: -32000, ..
    } if method == "turn/start"));
    assert_eq!(host.state(), State::Idle);
    let notice = host
        .receive(&wire(json!({"method":"unit/after-error","params":{}})))
        .unwrap();
    assert!(notice.events[0].request_id.is_none());
}

#[test]
fn codex_host_retryable_turn_error_remains_nonterminal_until_native_failed_completion() {
    let (mut host, request) = active();
    let batch = host
        .receive(&wire(json!({"method":"error","params":{
            "threadId":"unit-native-thread","turnId":"unit-native-turn",
            "error":{"message":"unit transient failure"},"willRetry":true
        }})))
        .unwrap();
    assert_eq!(batch.events[0].request_id, Some(request.clone()));
    assert!(matches!(
        &batch.events[0].kind,
        EventKind::TurnError {
            will_retry: true,
            ..
        }
    ));
    assert_eq!(host.state(), State::Active);
    let terminal = host.receive(&completed("failed")).unwrap();
    assert_eq!(terminal.events[0].request_id, Some(request));
    assert!(matches!(
        &terminal.events[0].kind,
        EventKind::TurnFinished {
            outcome: TurnOutcome::Failed,
            ..
        }
    ));
}

#[test]
fn codex_host_approval_request_is_observed_and_declined_through_supported_rpc() {
    let (mut host, request) = active();
    let batch = host
        .receive(&wire(json!({
            "id":"unit-approval","method":"item/commandExecution/requestApproval","params":{
                "threadId":"unit-native-thread","turnId":"unit-native-turn",
                "itemId":"unit-command","startedAtMs":1
            }
        })))
        .unwrap();
    assert_eq!(batch.events[0].request_id, Some(request));
    assert!(matches!(
        &batch.events[0].kind,
        EventKind::ApprovalRequired { .. }
    ));
    assert_eq!(batch.outbound.len(), 1);
    assert_eq!(
        message(&batch.outbound[0]),
        json!({
            "id":"unit-approval","result":{"decision":"decline"}
        })
    );
    assert_eq!(host.state(), State::Active);
}

#[test]
fn codex_host_stale_native_thread_and_oversize_frame_fail_without_normalized_success() {
    let (mut host, _) = active();
    assert!(
        host.receive(&wire(json!({"method":"item/agentMessage/delta","params":{
            "threadId":"unit-wrong-thread","turnId":"unit-native-turn",
            "itemId":"unit-native-item","delta":"unit stale data"
        }})))
        .is_err()
    );
    assert_eq!(host.state(), State::Failed);
    let mut other = protocol();
    other.initialize().unwrap();
    assert!(other.receive(&vec![b'x'; FRAME_BYTES_MAX + 1]).is_err());
    assert_eq!(other.state(), State::Failed);
}

#[test]
fn codex_host_eof_with_partial_frame_reports_disconnection_without_completion_or_identity() {
    let (mut host, request) = active();
    let partial = host.receive(b"{\"method\":\"turn/completed\"").unwrap();
    assert!(partial.events.is_empty());
    let event = host.disconnect();
    assert_eq!(event.kind, EventKind::Disconnected);
    assert_eq!(event.request_id, Some(request));
    assert!(event.native_session_id.is_none());
    assert!(event.native_turn_id.is_none());
    assert_eq!(host.state(), State::Failed);
}

#[test]
fn codex_host_noncodex_or_nonlaunch_provenance_is_rejected() {
    let mut wrong = action();
    let NativeAction::Launch { harness, .. } = &mut wrong else {
        unreachable!()
    };
    *harness = NativeHarness::Claude;
    assert!(CodexHostProtocol::new(wrong, CredentialRef::new("unit-account").unwrap()).is_err());
    let cancel = NativeAction::CancelChild {
        session_id: SessionId::new("unit-local-session").unwrap(),
        native_child_id: SessionId::new("unit-child-input").unwrap(),
    };
    assert!(CodexHostProtocol::new(cancel, CredentialRef::new("unit-account").unwrap()).is_err());
}
