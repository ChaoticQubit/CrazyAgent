use crazyagent::adapters::codex::{
    AuthAccount, AuthControl, CodexProtocol, Event, FRAME_BYTES_MAX, LoginCancelStatus,
    LoginChallenge, ProtocolError, SensitiveAuthText, State, WireId,
};
use crazyagent::adapters::codex_events::{BridgeError, CodexEventBridge};
use crazyagent::adapters::events::{Capability, EventKind, Support};
use crazyagent::adapters::{NativeAction, NativeHarness};
use crazyagent::protocol::{CredentialRef, RequestId, SessionId};
use serde_json::{Value, json};
use std::path::Path;

const LOGIN: &str = "account/login/start";
const COMPLETED: &str = "account/login/completed";
const READ: &str = "account/read";
const CANCEL: &str = "account/login/cancel";
const LOGOUT: &str = "account/logout";
const AUTH_URL: &str = "https://auth.example.test/authorize?state=private-browser-challenge";
const DEVICE_URL: &str = "https://auth.example.test/device/private-device-challenge";
const USER_CODE: &str = "PRIVATE-DEVICE-CODE";
const AUTH_ERROR: &str = "private-auth-error-material";
const EMAIL: &str = "private-account@example.test";

fn wire(value: Value) -> Vec<u8> {
    let mut bytes = serde_json::to_vec(&value).unwrap();
    bytes.push(b'\n');
    bytes
}

fn request(bytes: &[u8]) -> Value {
    assert_eq!(bytes.last(), Some(&b'\n'));
    assert_eq!(bytes.iter().filter(|byte| **byte == b'\n').count(), 1);
    assert!(bytes.len() <= FRAME_BYTES_MAX + 1);
    serde_json::from_slice(bytes).unwrap()
}

fn reply(protocol: &mut CodexProtocol, id: i64, result: Value) -> Vec<Event> {
    protocol
        .receive(&wire(json!({"id":id,"result":result})))
        .unwrap()
}

fn auth(events: Vec<Event>) -> AuthControl {
    assert_eq!(events.len(), 1);
    match events.into_iter().next().unwrap() {
        Event::AuthControl(control) => control,
        event => panic!("expected transient auth control, received {event:?}"),
    }
}

fn ready() -> CodexProtocol {
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
    assert_eq!(
        request(&protocol.initialized().unwrap()),
        json!({"method":"initialized"})
    );
    assert_eq!(protocol.state(), State::Ready);
    protocol
}

fn thread_result() -> Value {
    json!({
        "thread":{
            "id":"thread-unit", "cliVersion":"0.163.0", "createdAt":1,
            "updatedAt":1, "cwd":"/unit/worktree", "ephemeral":false,
            "modelProvider":"unit-provider-data", "preview":"", "projectId":null,
            "sessionId":"session-unit", "source":"appServer", "status":{"type":"idle"},
            "turns":[]
        },
        "cwd":"/unit/worktree", "model":"unit-model-data",
        "modelProvider":"unit-provider-data", "approvalPolicy":"on-request",
        "approvalsReviewer":"user", "sandbox":{"type":"readOnly","networkAccess":false}
    })
}

fn idle() -> CodexProtocol {
    let mut protocol = ready();
    assert_eq!(
        request(&protocol.start_thread(Path::new("/unit/worktree")).unwrap())["id"],
        2
    );
    assert_eq!(
        reply(&mut protocol, 2, thread_result()),
        vec![Event::ThreadStarted {
            thread_id: "thread-unit".into()
        }]
    );
    assert_eq!(protocol.state(), State::Idle);
    protocol
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
            json!({"turn":{"id":"turn-unit","items":[],"status":"inProgress"}})
        ),
        vec![Event::TurnStarted {
            turn_id: "turn-unit".into()
        }]
    );
    assert_eq!(protocol.state(), State::Active);
    protocol
}

fn browser_result(login_id: &str) -> Value {
    json!({"type":"chatgpt","loginId":login_id,"authUrl":AUTH_URL})
}

fn device_result(login_id: &str) -> Value {
    json!({
        "type":"chatgptDeviceCode", "loginId":login_id,
        "userCode":USER_CODE, "verificationUrl":DEVICE_URL
    })
}

fn completion(params: Value) -> Vec<u8> {
    wire(json!({"method":COMPLETED,"params":params}))
}

fn begin_browser(protocol: &mut CodexProtocol, login_id: &str) -> AuthControl {
    let outgoing = request(&protocol.start_chatgpt_login().unwrap());
    let id = outgoing["id"].as_i64().unwrap();
    assert_eq!(
        outgoing,
        json!({"id":id,"method":LOGIN,"params":{"type":"chatgpt"}})
    );
    auth(reply(protocol, id, browser_result(login_id)))
}

fn bridge() -> CodexEventBridge {
    CodexEventBridge::new(
        NativeAction::Launch {
            session_id: SessionId::new("managed-session").unwrap(),
            harness: NativeHarness::Codex,
            profile_ref: CredentialRef::new("native-profile").unwrap(),
            worktree_lease: RequestId::new("owned-lease").unwrap(),
        },
        CredentialRef::new("registered-account").unwrap(),
    )
    .unwrap()
}

fn assert_mutations_blocked(protocol: &mut CodexProtocol, state: State) {
    assert_eq!(
        protocol.start_chatgpt_login(),
        Err(ProtocolError::InvalidState)
    );
    assert_eq!(
        protocol.start_device_code_login(),
        Err(ProtocolError::InvalidState)
    );
    assert_eq!(
        protocol.read_account(true),
        Err(ProtocolError::InvalidState)
    );
    assert_eq!(protocol.logout_account(), Err(ProtocolError::InvalidState));
    assert_eq!(protocol.state(), state);
}

#[test]
fn browser_login_uses_supported_wire_and_distinct_native_login_id() {
    let mut protocol = ready();
    assert_eq!(
        request(&protocol.start_chatgpt_login().unwrap()),
        json!({"id":2,"method":LOGIN,"params":{"type":"chatgpt"}})
    );
    match auth(reply(
        &mut protocol,
        2,
        browser_result("native-browser-login"),
    )) {
        AuthControl::LoginStarted {
            id,
            challenge: LoginChallenge::Browser { login_id, auth_url },
        } => {
            assert_eq!(id, 2);
            assert_eq!(login_id, "native-browser-login");
            assert_eq!(auth_url.expose(), AUTH_URL);
        }
        control => panic!("unexpected auth control {control:?}"),
    }
    assert_eq!(protocol.state(), State::Ready);
}

#[test]
fn device_login_uses_supported_wire_without_browser_or_external_token_parameters() {
    let mut protocol = ready();
    assert_eq!(
        request(&protocol.start_device_code_login().unwrap()),
        json!({"id":2,"method":LOGIN,"params":{"type":"chatgptDeviceCode"}})
    );
    match auth(reply(
        &mut protocol,
        2,
        device_result("native-device-login"),
    )) {
        AuthControl::LoginStarted {
            id,
            challenge:
                LoginChallenge::DeviceCode {
                    login_id,
                    user_code,
                    verification_url,
                },
        } => {
            assert_eq!(id, 2);
            assert_eq!(login_id, "native-device-login");
            assert_eq!(user_code.expose(), USER_CODE);
            assert_eq!(verification_url.expose(), DEVICE_URL);
        }
        control => panic!("unexpected auth control {control:?}"),
    }
    assert_eq!(protocol.state(), State::Ready);
}

#[test]
fn fragmented_challenge_and_coalesced_completion_preserve_wire_order() {
    let mut protocol = ready();
    protocol.start_chatgpt_login().unwrap();
    let challenge = wire(json!({"id":2,"result":browser_result("native-fragmented")}));
    let split = challenge.len() / 2;
    assert!(protocol.receive(&challenge[..split]).unwrap().is_empty());
    let mut remainder = challenge[split..].to_vec();
    remainder.extend(completion(
        json!({"loginId":"native-fragmented","success":true}),
    ));
    let events = protocol.receive(&remainder).unwrap();
    assert_eq!(events.len(), 2);
    assert!(matches!(
        &events[0],
        Event::AuthControl(AuthControl::LoginStarted { id: 2, .. })
    ));
    assert_eq!(
        events[1],
        Event::AuthControl(AuthControl::LoginCompleted {
            login_id: Some("native-fragmented".into()),
            success: true,
            error: None,
            onboarding_entrypoint: None,
        })
    );
    assert_eq!(protocol.state(), State::Ready);
    assert_eq!(
        request(&protocol.start_thread(Path::new("/unit/worktree")).unwrap())["id"],
        3
    );
}

#[test]
fn missing_and_null_completion_ids_remain_uncorrelated_and_do_not_unlock_login() {
    for params in [
        json!({"success":false}),
        json!({"loginId":null,"success":false,"error":null,"onboardingEntrypoint":null}),
    ] {
        let mut protocol = ready();
        begin_browser(&mut protocol, "native-pending");
        assert_eq!(
            auth(protocol.receive(&completion(params)).unwrap()),
            AuthControl::LoginCompleted {
                login_id: None,
                success: false,
                error: None,
                onboarding_entrypoint: None,
            }
        );
        assert_eq!(
            protocol.start_thread(Path::new("/unit/worktree")),
            Err(ProtocolError::InvalidState)
        );
        assert_mutations_blocked(&mut protocol, State::Ready);
        assert!(matches!(
            auth(protocol.receive(&completion(json!({"loginId":"native-pending","success":false}))).unwrap()),
            AuthControl::LoginCompleted { login_id: Some(id), success: false, .. } if id == "native-pending"
        ));
        assert!(protocol.start_thread(Path::new("/unit/worktree")).is_ok());
    }
}

#[test]
fn completion_preserves_success_error_and_supported_onboarding_without_inference() {
    for success in [false, true] {
        let mut protocol = ready();
        begin_browser(&mut protocol, "native-observed");
        let control = auth(
            protocol
                .receive(&completion(json!({
                    "loginId":"native-observed", "success":success,
                    "error":AUTH_ERROR, "onboardingEntrypoint":"life_sciences"
                })))
                .unwrap(),
        );
        match control {
            AuthControl::LoginCompleted {
                login_id,
                success: observed,
                error,
                onboarding_entrypoint,
            } => {
                assert_eq!(login_id.as_deref(), Some("native-observed"));
                assert_eq!(observed, success);
                assert_eq!(error.unwrap().expose(), AUTH_ERROR);
                assert_eq!(onboarding_entrypoint.as_deref(), Some("life_sciences"));
            }
            control => panic!("unexpected auth control {control:?}"),
        }
        assert_eq!(protocol.state(), State::Ready);
    }
}

#[test]
fn account_read_and_refresh_emit_exact_parameters_and_preserve_null_account_truth() {
    for refresh in [false, true] {
        for result in [
            json!({"requiresOpenaiAuth":true}),
            json!({"account":null,"requiresOpenaiAuth":false}),
        ] {
            let mut protocol = ready();
            assert_eq!(
                request(&protocol.read_account(refresh).unwrap()),
                json!({"id":2,"method":READ,"params":{"refreshToken":refresh}})
            );
            assert_eq!(
                auth(reply(&mut protocol, 2, result.clone())),
                AuthControl::AccountRead {
                    id: 2,
                    account: None,
                    requires_openai_auth: result["requiresOpenaiAuth"].as_bool().unwrap(),
                }
            );
            assert_eq!(protocol.state(), State::Ready);
        }
    }
}

#[test]
fn account_read_preserves_supported_account_variants_and_nullable_email() {
    let cases = [
        json!({"type":"apiKey"}),
        json!({"type":"chatgpt","email":EMAIL,"planType":"plus"}),
        json!({"type":"chatgpt","email":null,"planType":"unknown"}),
        json!({"type":"amazonBedrock"}),
        json!({"type":"amazonBedrock","usesCodexManagedCredentials":true}),
    ];
    for account in cases {
        let mut protocol = ready();
        protocol.read_account(false).unwrap();
        match auth(reply(
            &mut protocol,
            2,
            json!({"account":account,"requiresOpenaiAuth":false}),
        )) {
            AuthControl::AccountRead {
                id: 2,
                account: Some(observed),
                requires_openai_auth: false,
            } => match observed {
                AuthAccount::ApiKey => assert_eq!(account["type"], "apiKey"),
                AuthAccount::Chatgpt { email, plan_type } => {
                    assert_eq!(account["type"], "chatgpt");
                    assert_eq!(
                        email.as_ref().map(SensitiveAuthText::expose),
                        account["email"].as_str()
                    );
                    assert_eq!(plan_type, account["planType"].as_str().unwrap());
                }
                AuthAccount::AmazonBedrock {
                    uses_codex_managed_credentials,
                } => {
                    assert_eq!(account["type"], "amazonBedrock");
                    assert_eq!(
                        uses_codex_managed_credentials,
                        account["usesCodexManagedCredentials"]
                            .as_bool()
                            .unwrap_or(false)
                    );
                }
            },
            control => panic!("unexpected auth control {control:?}"),
        }
    }
}

#[test]
fn account_read_accepts_every_pinned_plan_variant_without_promoting_readiness() {
    for plan in [
        "free",
        "go",
        "plus",
        "pro",
        "prolite",
        "promax",
        "team",
        "self_serve_business_prolite",
        "self_serve_business_usage_based",
        "business",
        "ent26",
        "enterprise_cbp_automation",
        "enterprise_cbp_usage_based",
        "enterprise",
        "edu",
        "edu_plus",
        "edu_pro",
        "unknown",
    ] {
        let mut protocol = ready();
        protocol.read_account(false).unwrap();
        match auth(reply(
            &mut protocol,
            2,
            json!({
                "account":{"type":"chatgpt","email":null,"planType":plan},
                "requiresOpenaiAuth":true
            }),
        )) {
            AuthControl::AccountRead {
                account:
                    Some(AuthAccount::Chatgpt {
                        email: None,
                        plan_type,
                    }),
                requires_openai_auth: true,
                ..
            } => assert_eq!(plan_type, plan),
            control => panic!("unexpected auth control {control:?}"),
        }
        assert_eq!(protocol.state(), State::Ready);
    }
}

#[test]
fn cancel_uses_native_login_id_and_preserves_both_supported_statuses() {
    for (status, expected) in [
        ("canceled", LoginCancelStatus::Canceled),
        ("notFound", LoginCancelStatus::NotFound),
    ] {
        let mut protocol = ready();
        begin_browser(&mut protocol, "native-to-cancel");
        assert_eq!(
            request(&protocol.cancel_login("native-to-cancel").unwrap()),
            json!({"id":3,"method":CANCEL,"params":{"loginId":"native-to-cancel"}})
        );
        assert_eq!(
            auth(reply(&mut protocol, 3, json!({"status":status}))),
            AuthControl::LoginCanceled {
                id: 3,
                status: expected
            }
        );
        if status == "notFound" {
            assert_eq!(
                protocol.start_thread(Path::new("/unit/worktree")),
                Err(ProtocolError::InvalidState)
            );
        } else {
            assert!(protocol.start_thread(Path::new("/unit/worktree")).is_ok());
        }
    }
}

#[test]
fn cancel_unknown_id_does_not_invent_a_matching_active_flow() {
    for (status, expected) in [
        ("notFound", LoginCancelStatus::NotFound),
        ("canceled", LoginCancelStatus::Canceled),
    ] {
        let mut protocol = ready();
        begin_browser(&mut protocol, "native-active");
        assert_eq!(
            request(&protocol.cancel_login("native-unknown").unwrap())["params"]["loginId"],
            "native-unknown"
        );
        assert_eq!(
            auth(reply(&mut protocol, 3, json!({"status":status}))),
            AuthControl::LoginCanceled {
                id: 3,
                status: expected
            }
        );
        assert_eq!(
            protocol.start_thread(Path::new("/unit/worktree")),
            Err(ProtocolError::InvalidState)
        );
    }
}

#[test]
fn logout_is_correlated_and_does_not_invent_an_account_or_completion_event() {
    let mut protocol = ready();
    let outgoing = request(&protocol.logout_account().unwrap());
    assert_eq!(outgoing["id"], 2);
    assert_eq!(outgoing["method"], LOGOUT);
    assert!(outgoing.get("params").is_none_or(Value::is_null));
    assert_eq!(
        auth(reply(&mut protocol, 2, json!({}))),
        AuthControl::LoggedOut { id: 2 }
    );
    assert_eq!(protocol.state(), State::Ready);
    assert_eq!(request(&protocol.read_account(false).unwrap())["id"], 3);
    assert_eq!(
        auth(reply(
            &mut protocol,
            3,
            json!({"account":null,"requiresOpenaiAuth":true})
        )),
        AuthControl::AccountRead {
            id: 3,
            account: None,
            requires_openai_auth: true
        }
    );
}

#[test]
fn auth_rpc_errors_use_sensitive_channel_with_exact_method_code_and_correlation() {
    for operation in [LOGIN, READ, CANCEL, LOGOUT] {
        let mut protocol = ready();
        let bytes = match operation {
            LOGIN => protocol.start_chatgpt_login(),
            READ => protocol.read_account(true),
            CANCEL => protocol.cancel_login("native-absent"),
            LOGOUT => protocol.logout_account(),
            _ => unreachable!(),
        }
        .unwrap();
        assert_eq!(request(&bytes)["id"], 2);
        let control = auth(
            protocol
                .receive(&wire(json!({
                    "id":2,"error":{"code":-32001,"message":AUTH_ERROR}
                })))
                .unwrap(),
        );
        match control {
            AuthControl::RequestFailed {
                id,
                method,
                code,
                message,
            } => {
                assert_eq!(id, 2);
                assert_eq!(method, operation);
                assert_eq!(code, -32001);
                assert_eq!(message.expose(), AUTH_ERROR);
                assert!(!format!("{message:?}").contains(AUTH_ERROR));
            }
            control => panic!("unexpected auth control {control:?}"),
        }
        assert_eq!(protocol.state(), State::Ready);
        assert!(protocol.read_account(false).is_ok());
    }
}

#[test]
fn invalid_missing_null_and_string_rpc_ids_fail_without_becoming_login_ids() {
    for id in [
        None,
        Some(json!(null)),
        Some(json!("2")),
        Some(json!(0)),
        Some(json!(-1)),
        Some(json!(3)),
        Some(json!(2.0)),
    ] {
        let mut protocol = ready();
        protocol.start_chatgpt_login().unwrap();
        let mut incoming = json!({"result":browser_result("native-valid")});
        if let Some(id) = id {
            incoming["id"] = id;
        }
        assert!(protocol.receive(&wire(incoming)).is_err());
        assert_eq!(protocol.state(), State::Failed);
    }
}

#[test]
fn unsolicited_duplicate_and_duplicate_key_rpc_responses_fail_closed() {
    let mut protocol = ready();
    assert_eq!(
        protocol.receive(&wire(json!({"id":2,"result":{}}))),
        Err(ProtocolError::StaleMessage)
    );
    assert_eq!(protocol.state(), State::Failed);

    let mut protocol = ready();
    begin_browser(&mut protocol, "native-once");
    assert_eq!(
        protocol.receive(&wire(
            json!({"id":2,"result":browser_result("native-once")})
        )),
        Err(ProtocolError::StaleMessage)
    );
    assert_eq!(protocol.state(), State::Failed);

    let mut protocol = ready();
    protocol.start_chatgpt_login().unwrap();
    assert_eq!(
        protocol.receive(b"{\"id\":2,\"id\":2,\"result\":{\"type\":\"chatgpt\",\"loginId\":\"native-once\",\"authUrl\":\"https://auth.example.test/login\"}}\n"),
        Err(ProtocolError::InvalidMessage)
    );
    assert_eq!(protocol.state(), State::Failed);
}

#[test]
fn invalid_native_login_ids_are_rejected_at_response_and_cancel_boundaries() {
    for login_id in [
        json!(null),
        json!(2),
        json!(""),
        json!(" "),
        json!("bad\nlogin"),
        json!("x".repeat(257)),
    ] {
        let mut protocol = ready();
        protocol.start_chatgpt_login().unwrap();
        let mut result = browser_result("native-valid");
        result["loginId"] = login_id;
        assert_eq!(
            protocol.receive(&wire(json!({"id":2,"result":result}))),
            Err(ProtocolError::InvalidMessage)
        );
        assert_eq!(protocol.state(), State::Failed);
    }
    for login_id in ["", " ", "bad\nlogin", &"x".repeat(257)] {
        let mut protocol = ready();
        assert_eq!(
            protocol.cancel_login(login_id),
            Err(ProtocolError::InvalidInput)
        );
        assert_eq!(protocol.state(), State::Ready);
        assert_eq!(request(&protocol.read_account(false).unwrap())["id"], 2);
    }
}

#[test]
fn missing_challenge_fields_wrong_login_variants_and_malformed_urls_fail_explicitly() {
    for result in [
        json!({"type":"chatgpt","authUrl":AUTH_URL}),
        json!({"type":"chatgpt","loginId":"native-valid"}),
        json!({"type":"chatgpt","loginId":"native-valid","authUrl":null}),
        json!({"type":"chatgpt","loginId":"native-valid","authUrl":"file:///private/auth"}),
        json!({"type":"chatgpt","loginId":"native-valid","authUrl":"https://"}),
        json!({"type":"chatgpt","loginId":"native-valid","authUrl":"https://auth.example.test/\ncode"}),
        json!({"type":"chatgptAuthTokens","accessToken":"synthetic-denied-token","chatgptAccountId":"synthetic-denied-account"}),
        json!({"type":"apiKey"}),
        device_result("native-wrong-method"),
    ] {
        let mut protocol = ready();
        protocol.start_chatgpt_login().unwrap();
        assert_eq!(
            protocol.receive(&wire(json!({"id":2,"result":result}))),
            Err(ProtocolError::InvalidMessage)
        );
        assert_eq!(protocol.state(), State::Failed);
    }
    for missing in ["loginId", "userCode", "verificationUrl"] {
        let mut protocol = ready();
        protocol.start_device_code_login().unwrap();
        let mut result = device_result("native-device");
        result.as_object_mut().unwrap().remove(missing);
        assert_eq!(
            protocol.receive(&wire(json!({"id":2,"result":result}))),
            Err(ProtocolError::InvalidMessage)
        );
    }
}

#[test]
fn malformed_completion_account_cancel_and_logout_results_fail_closed() {
    for params in [
        json!({"loginId":"native-valid"}),
        json!({"loginId":"native-valid","success":null}),
        json!({"loginId":2,"success":true}),
        json!({"loginId":"","success":true}),
        json!({"loginId":"native-valid","success":true,"error":2}),
        json!({"loginId":"native-valid","success":true,"onboardingEntrypoint":"unknown-entrypoint"}),
    ] {
        let mut protocol = ready();
        begin_browser(&mut protocol, "native-valid");
        assert_eq!(
            protocol.receive(&completion(params)),
            Err(ProtocolError::InvalidMessage)
        );
        assert_eq!(protocol.state(), State::Failed);
    }
    for result in [
        json!({"account":null}),
        json!({"requiresOpenaiAuth":null}),
        json!({"account":{"type":"chatgpt","planType":"plus"},"requiresOpenaiAuth":false}),
        json!({"account":{"type":"chatgpt","email":null},"requiresOpenaiAuth":false}),
        json!({"account":{"type":"chatgpt","email":null,"planType":"invented-plan"},"requiresOpenaiAuth":false}),
        json!({"account":{"type":"invented-account"},"requiresOpenaiAuth":false}),
        json!({"account":{"type":"amazonBedrock","usesCodexManagedCredentials":null},"requiresOpenaiAuth":false}),
    ] {
        let mut protocol = ready();
        protocol.read_account(false).unwrap();
        assert_eq!(
            protocol.receive(&wire(json!({"id":2,"result":result}))),
            Err(ProtocolError::InvalidMessage)
        );
    }
    for result in [
        json!({}),
        json!({"status":null}),
        json!({"status":"unknown"}),
    ] {
        let mut protocol = ready();
        protocol.cancel_login("native-valid").unwrap();
        assert_eq!(
            protocol.receive(&wire(json!({"id":2,"result":result}))),
            Err(ProtocolError::InvalidMessage)
        );
    }
    let mut protocol = ready();
    protocol.logout_account().unwrap();
    assert_eq!(
        protocol.receive(&wire(json!({"id":2,"result":null}))),
        Err(ProtocolError::InvalidMessage)
    );
}

#[test]
fn completion_before_start_response_keeps_native_correlation_and_terminal_fact() {
    let mut protocol = ready();
    protocol.start_chatgpt_login().unwrap();
    assert_eq!(
        auth(
            protocol
                .receive(&completion(
                    json!({"loginId":"native-early","success":true})
                ))
                .unwrap()
        ),
        AuthControl::LoginCompleted {
            login_id: Some("native-early".into()),
            success: true,
            error: None,
            onboarding_entrypoint: None,
        }
    );
    assert!(matches!(
        auth(reply(&mut protocol, 2, browser_result("native-early"))),
        AuthControl::LoginStarted { id: 2, challenge: LoginChallenge::Browser { login_id, .. } } if login_id == "native-early"
    ));
    assert!(protocol.start_thread(Path::new("/unit/worktree")).is_ok());
}

#[test]
fn early_completion_with_different_later_login_id_is_never_silently_correlated() {
    let mut protocol = ready();
    protocol.start_chatgpt_login().unwrap();
    protocol
        .receive(&completion(
            json!({"loginId":"native-early","success":true}),
        ))
        .unwrap();
    assert_eq!(
        protocol.receive(&wire(
            json!({"id":2,"result":browser_result("native-other")})
        )),
        Err(ProtocolError::StaleMessage)
    );
    assert_eq!(protocol.state(), State::Failed);

    let mut protocol = ready();
    protocol.start_chatgpt_login().unwrap();
    protocol
        .receive(&completion(
            json!({"loginId":"native-early","success":false}),
        ))
        .unwrap();
    assert_eq!(
        protocol.receive(&completion(
            json!({"loginId":"native-other","success":false})
        )),
        Err(ProtocolError::StaleMessage)
    );
    assert_eq!(protocol.state(), State::Failed);
}

#[test]
fn completion_cancel_races_preserve_both_observed_outcomes() {
    for completion_first in [false, true] {
        let mut protocol = ready();
        begin_browser(&mut protocol, "native-racing");
        protocol.cancel_login("native-racing").unwrap();
        let completed = completion(json!({"loginId":"native-racing","success":true}));
        let canceled = wire(json!({"id":3,"result":{"status":"canceled"}}));
        let mut frames = if completion_first {
            completed.clone()
        } else {
            canceled.clone()
        };
        frames.extend(if completion_first {
            canceled
        } else {
            completed
        });
        let events = protocol.receive(&frames).unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(
            events[usize::from(!completion_first)],
            Event::AuthControl(AuthControl::LoginCompleted {
                login_id: Some("native-racing".into()),
                success: true,
                error: None,
                onboarding_entrypoint: None,
            })
        );
        assert_eq!(
            events[usize::from(completion_first)],
            Event::AuthControl(AuthControl::LoginCanceled {
                id: 3,
                status: LoginCancelStatus::Canceled
            })
        );
        assert_eq!(protocol.state(), State::Ready);
        assert!(protocol.start_thread(Path::new("/unit/worktree")).is_ok());
    }
}

#[test]
fn late_completion_of_canceled_login_does_not_clear_another_live_login() {
    let mut protocol = ready();
    begin_browser(&mut protocol, "native-canceled-old");
    protocol.cancel_login("native-canceled-old").unwrap();
    reply(&mut protocol, 3, json!({"status":"canceled"}));
    begin_browser(&mut protocol, "native-live-new");
    assert!(matches!(
        auth(protocol.receive(&completion(json!({"loginId":"native-canceled-old","success":true}))).unwrap()),
        AuthControl::LoginCompleted { login_id: Some(id), success: true, .. } if id == "native-canceled-old"
    ));
    assert_eq!(
        protocol.start_thread(Path::new("/unit/worktree")),
        Err(ProtocolError::InvalidState)
    );
    assert_mutations_blocked(&mut protocol, State::Ready);
    protocol
        .receive(&completion(
            json!({"loginId":"native-live-new","success":false}),
        ))
        .unwrap();
    assert!(protocol.start_thread(Path::new("/unit/worktree")).is_ok());
}

#[test]
fn unknown_duplicate_completed_and_reused_native_login_ids_fail_closed() {
    let mut protocol = ready();
    assert_eq!(
        protocol.receive(&completion(
            json!({"loginId":"native-unknown","success":true})
        )),
        Err(ProtocolError::StaleMessage)
    );

    let mut protocol = ready();
    begin_browser(&mut protocol, "native-once");
    let completed = completion(json!({"loginId":"native-once","success":false}));
    assert_eq!(protocol.receive(&completed).unwrap().len(), 1);
    assert_eq!(
        protocol.receive(&completed),
        Err(ProtocolError::StaleMessage)
    );
    assert_eq!(protocol.state(), State::Failed);

    let mut protocol = ready();
    begin_browser(&mut protocol, "native-reused");
    protocol
        .receive(&completion(
            json!({"loginId":"native-reused","success":false}),
        ))
        .unwrap();
    assert_eq!(request(&protocol.start_chatgpt_login().unwrap())["id"], 3);
    assert_eq!(
        protocol.receive(&wire(
            json!({"id":3,"result":browser_result("native-reused")})
        )),
        Err(ProtocolError::StaleMessage)
    );
}

#[test]
fn authentication_mutations_cannot_change_credentials_under_thread_or_active_turn() {
    let mut new = CodexProtocol::new();
    assert_mutations_blocked(&mut new, State::New);
    let mut initializing = CodexProtocol::new();
    initializing.initialize().unwrap();
    assert_mutations_blocked(&mut initializing, State::Initializing);
    let mut initialized = CodexProtocol::new();
    initialized.initialize().unwrap();
    reply(
        &mut initialized,
        1,
        json!({
            "userAgent":"unit-wire-data","codexHome":"/unit/codex",
            "platformFamily":"unix","platformOs":"macos"
        }),
    );
    assert_mutations_blocked(&mut initialized, State::Initialized);
    let mut starting_thread = ready();
    starting_thread
        .start_thread(Path::new("/unit/worktree"))
        .unwrap();
    assert_mutations_blocked(&mut starting_thread, State::ThreadStarting);
    let mut idle = idle();
    assert_mutations_blocked(&mut idle, State::Idle);
    idle.start_turn("unit input").unwrap();
    assert_mutations_blocked(&mut idle, State::TurnStarting);
    let mut active = active();
    assert_mutations_blocked(&mut active, State::Active);
    assert_eq!(
        active.cancel_login("native-id"),
        Err(ProtocolError::InvalidState)
    );
}

#[test]
fn cancel_and_read_do_not_bypass_uninitialized_or_pending_thread_phases() {
    let mut new = CodexProtocol::new();
    assert_eq!(
        new.cancel_login("native-id"),
        Err(ProtocolError::InvalidState)
    );
    assert_eq!(new.read_account(false), Err(ProtocolError::InvalidState));
    let mut initializing = CodexProtocol::new();
    initializing.initialize().unwrap();
    assert_eq!(
        initializing.cancel_login("native-id"),
        Err(ProtocolError::InvalidState)
    );
    assert_eq!(
        initializing.read_account(false),
        Err(ProtocolError::InvalidState)
    );
    let mut starting_thread = ready();
    starting_thread
        .start_thread(Path::new("/unit/worktree"))
        .unwrap();
    assert_eq!(
        starting_thread.cancel_login("native-id"),
        Err(ProtocolError::InvalidState)
    );
    assert_eq!(
        starting_thread.read_account(false),
        Err(ProtocolError::InvalidState)
    );
    let mut starting_turn = idle();
    starting_turn.start_turn("unit input").unwrap();
    assert_eq!(
        starting_turn.cancel_login("native-id"),
        Err(ProtocolError::InvalidState)
    );
    assert_eq!(
        starting_turn.read_account(false),
        Err(ProtocolError::InvalidState)
    );
}

#[test]
fn nonrefreshing_read_preserves_ready_idle_and_active_coding_phase() {
    for mut protocol in [ready(), idle(), active()] {
        let state = protocol.state();
        let outgoing = request(&protocol.read_account(false).unwrap());
        let id = outgoing["id"].as_i64().unwrap();
        assert_eq!(outgoing["method"], READ);
        assert_eq!(outgoing["params"], json!({"refreshToken":false}));
        assert_eq!(protocol.state(), state);
        assert_eq!(
            auth(reply(
                &mut protocol,
                id,
                json!({"account":null,"requiresOpenaiAuth":true})
            )),
            AuthControl::AccountRead {
                id,
                account: None,
                requires_openai_auth: true
            }
        );
        assert_eq!(protocol.state(), state);
        if state == State::Active {
            assert_eq!(
                request(&protocol.interrupt().unwrap())["params"]["turnId"],
                "turn-unit"
            );
        }
    }
}

#[test]
fn auth_requests_share_existing_single_pending_rpc_and_login_flow_guards() {
    let mut protocol = ready();
    protocol.start_chatgpt_login().unwrap();
    assert_mutations_blocked(&mut protocol, State::Ready);
    assert_eq!(
        protocol.read_account(false),
        Err(ProtocolError::InvalidState)
    );
    assert_eq!(
        protocol.read_account_rate_limits(),
        Err(ProtocolError::InvalidState)
    );
    assert_eq!(
        protocol.cancel_login("native-pending"),
        Err(ProtocolError::InvalidState)
    );
    assert_eq!(
        protocol.start_thread(Path::new("/unit/worktree")),
        Err(ProtocolError::InvalidState)
    );
    reply(&mut protocol, 2, browser_result("native-pending"));
    assert_mutations_blocked(&mut protocol, State::Ready);
    assert_eq!(
        protocol.start_thread(Path::new("/unit/worktree")),
        Err(ProtocolError::InvalidState)
    );
    assert_eq!(request(&protocol.read_account(false).unwrap())["id"], 3);
    reply(
        &mut protocol,
        3,
        json!({"account":null,"requiresOpenaiAuth":true}),
    );
    assert_eq!(
        request(&protocol.cancel_login("native-pending").unwrap())["id"],
        4
    );
    reply(&mut protocol, 4, json!({"status":"canceled"}));
    assert_eq!(
        request(&protocol.start_device_code_login().unwrap())["id"],
        5
    );
}

#[test]
fn native_login_history_is_bounded_at_sixteen_without_reusing_ids_or_losing_read_access() {
    let mut protocol = ready();
    for index in 0..16 {
        let login_id = format!("native-bounded-{index}");
        begin_browser(&mut protocol, &login_id);
        assert!(matches!(
            auth(
                protocol
                    .receive(&completion(json!({"loginId":login_id,"success":false})))
                    .unwrap()
            ),
            AuthControl::LoginCompleted { success: false, .. }
        ));
    }
    assert_eq!(protocol.start_chatgpt_login(), Err(ProtocolError::Capacity));
    assert_eq!(
        protocol.start_device_code_login(),
        Err(ProtocolError::Capacity)
    );
    assert_eq!(protocol.state(), State::Ready);
    assert_eq!(request(&protocol.read_account(false).unwrap())["id"], 18);
    reply(
        &mut protocol,
        18,
        json!({"account":null,"requiresOpenaiAuth":true}),
    );
    assert_eq!(request(&protocol.logout_account().unwrap())["id"], 19);
    assert_eq!(
        auth(reply(&mut protocol, 19, json!({}))),
        AuthControl::LoggedOut { id: 19 }
    );
    assert_eq!(protocol.start_chatgpt_login(), Err(ProtocolError::Capacity));
    assert_eq!(
        protocol.receive(&completion(
            json!({"loginId":"native-bounded-0","success":false})
        )),
        Err(ProtocolError::StaleMessage)
    );
}

#[test]
fn auth_string_bounds_accept_exact_limits_and_reject_one_byte_over() {
    for (field, maximum) in [("loginId", 256), ("authUrl", 2048)] {
        for overflow in [false, true] {
            let mut protocol = ready();
            protocol.start_chatgpt_login().unwrap();
            let mut result = browser_result("native-bounded");
            let size = maximum + usize::from(overflow);
            result[field] = if field == "authUrl" {
                let prefix = "https://auth.example.test/";
                json!(format!("{prefix}{}", "x".repeat(size - prefix.len())))
            } else {
                json!("x".repeat(size))
            };
            let received = protocol.receive(&wire(json!({"id":2,"result":result})));
            if overflow {
                assert_eq!(received, Err(ProtocolError::InvalidMessage));
            } else {
                assert_eq!(received.unwrap().len(), 1);
            }
        }
    }
    for overflow in [false, true] {
        let mut protocol = ready();
        protocol.start_device_code_login().unwrap();
        let mut result = device_result("native-device-bounded");
        result["userCode"] = json!("x".repeat(256 + usize::from(overflow)));
        let received = protocol.receive(&wire(json!({"id":2,"result":result})));
        if overflow {
            assert_eq!(received, Err(ProtocolError::InvalidMessage));
        } else {
            assert_eq!(received.unwrap().len(), 1);
        }
    }
    for overflow in [false, true] {
        let mut protocol = ready();
        begin_browser(&mut protocol, "native-error-bounded");
        let received = protocol.receive(&completion(json!({
            "loginId":"native-error-bounded", "success":false,
            "error":"x".repeat(4096 + usize::from(overflow))
        })));
        if overflow {
            assert_eq!(received, Err(ProtocolError::InvalidMessage));
        } else {
            assert_eq!(received.unwrap().len(), 1);
        }
    }
}

#[test]
fn email_error_and_device_url_bounds_apply_to_every_sensitive_ingress() {
    let mut protocol = ready();
    protocol.read_account(false).unwrap();
    assert_eq!(
        protocol.receive(&wire(json!({"id":2,"result":{
            "account":{"type":"chatgpt","email":"x".repeat(321),"planType":"plus"},
            "requiresOpenaiAuth":false
        }}))),
        Err(ProtocolError::InvalidMessage)
    );

    let mut protocol = ready();
    protocol.read_account(false).unwrap();
    assert_eq!(
        protocol.receive(&wire(
            json!({"id":2,"error":{"code":-1,"message":"x".repeat(4097)}})
        )),
        Err(ProtocolError::InvalidMessage)
    );

    let mut protocol = ready();
    protocol.start_device_code_login().unwrap();
    let mut result = device_result("native-device-url");
    result["verificationUrl"] = json!(format!("https://auth.example.test/{}", "x".repeat(2049)));
    assert_eq!(
        protocol.receive(&wire(json!({"id":2,"result":result}))),
        Err(ProtocolError::InvalidMessage)
    );
}

#[test]
fn auth_receive_preserves_existing_frame_output_and_buffer_bounds() {
    for (count, expected_error) in [(128, None), (129, Some(ProtocolError::Capacity))] {
        let mut protocol = ready();
        let frame = completion(json!({"loginId":null,"success":false}));
        let bytes = frame.repeat(count);
        assert!(bytes.len() < FRAME_BYTES_MAX);
        let received = protocol.receive(&bytes);
        if let Some(error) = expected_error {
            assert_eq!(received, Err(error));
            assert_eq!(protocol.state(), State::Failed);
        } else {
            let events = received.unwrap();
            assert_eq!(events.len(), count);
            assert!(events.iter().all(|event| matches!(
                event,
                Event::AuthControl(AuthControl::LoginCompleted {
                    login_id: None,
                    success: false,
                    ..
                })
            )));
        }
    }
    let mut protocol = ready();
    protocol.start_chatgpt_login().unwrap();
    assert_eq!(
        protocol.receive(&vec![b'x'; FRAME_BYTES_MAX + 1]),
        Err(ProtocolError::FrameTooLarge)
    );
    assert_eq!(protocol.state(), State::Failed);

    let mut protocol = ready();
    protocol.start_chatgpt_login().unwrap();
    protocol.receive(b"{\"id\":2,\"result\":").unwrap();
    assert_eq!(protocol.finish(), Err(ProtocolError::Disconnected));
    assert_eq!(protocol.state(), State::Failed);
}

#[test]
fn transient_auth_types_do_not_implement_serialize_and_debug_redacts_all_sensitive_values() {
    trait AmbiguousIfSerialize<A> {
        fn marker() {}
    }
    impl<T: ?Sized> AmbiguousIfSerialize<()> for T {}
    struct IfSerialize;
    impl<T: ?Sized + serde::Serialize> AmbiguousIfSerialize<IfSerialize> for T {}
    let _ = <SensitiveAuthText as AmbiguousIfSerialize<_>>::marker;
    let _ = <LoginChallenge as AmbiguousIfSerialize<_>>::marker;
    let _ = <AuthAccount as AmbiguousIfSerialize<_>>::marker;
    let _ = <AuthControl as AmbiguousIfSerialize<_>>::marker;
    let _ = <Event as AmbiguousIfSerialize<_>>::marker;

    let mut protocol = ready();
    let browser = begin_browser(&mut protocol, "native-browser-redacted");
    let completed = auth(
        protocol
            .receive(&completion(json!({
                "loginId":"native-browser-redacted","success":false,"error":AUTH_ERROR
            })))
            .unwrap(),
    );
    let outgoing = request(&protocol.start_device_code_login().unwrap());
    let device = auth(reply(
        &mut protocol,
        outgoing["id"].as_i64().unwrap(),
        device_result("native-device-redacted"),
    ));
    protocol
        .receive(&completion(
            json!({"loginId":"native-device-redacted","success":false}),
        ))
        .unwrap();
    let outgoing = request(&protocol.read_account(false).unwrap());
    let account = auth(reply(
        &mut protocol,
        outgoing["id"].as_i64().unwrap(),
        json!({
            "account":{"type":"chatgpt","email":EMAIL,"planType":"plus"},"requiresOpenaiAuth":false
        }),
    ));
    let outgoing = request(&protocol.logout_account().unwrap());
    let failed = auth(
        protocol
            .receive(&wire(json!({
                "id":outgoing["id"],"error":{"code":-1,"message":AUTH_ERROR}
            })))
            .unwrap(),
    );
    for control in [browser, device, completed, account, failed] {
        let debug = format!(
            "{control:?} {event:?}",
            event = Event::AuthControl(control.clone())
        );
        for sensitive in [AUTH_URL, DEVICE_URL, USER_CODE, AUTH_ERROR, EMAIL] {
            assert!(!debug.contains(sensitive));
        }
    }
}

#[test]
fn every_production_auth_control_is_rejected_by_worker_bridge_without_state_changes() {
    let mut protocol = ready();
    let mut controls = vec![begin_browser(&mut protocol, "native-worker-denied")];
    controls.push(auth(
        protocol
            .receive(&completion(json!({
                "loginId":"native-worker-denied","success":false,"error":AUTH_ERROR
            })))
            .unwrap(),
    ));
    let outgoing = request(&protocol.start_device_code_login().unwrap());
    controls.push(auth(reply(
        &mut protocol,
        outgoing["id"].as_i64().unwrap(),
        device_result("native-device-denied"),
    )));
    let outgoing = request(&protocol.cancel_login("native-device-denied").unwrap());
    controls.push(auth(reply(
        &mut protocol,
        outgoing["id"].as_i64().unwrap(),
        json!({"status":"canceled"}),
    )));
    let outgoing = request(&protocol.read_account(false).unwrap());
    controls.push(auth(reply(
        &mut protocol,
        outgoing["id"].as_i64().unwrap(),
        json!({
            "account":{"type":"chatgpt","email":EMAIL,"planType":"plus"},"requiresOpenaiAuth":false
        }),
    )));
    let outgoing = request(&protocol.logout_account().unwrap());
    controls.push(auth(reply(
        &mut protocol,
        outgoing["id"].as_i64().unwrap(),
        json!({}),
    )));
    let outgoing = request(&protocol.read_account(false).unwrap());
    controls.push(auth(
        protocol
            .receive(&wire(
                json!({"id":outgoing["id"],"error":{"code":-1,"message":AUTH_ERROR}}),
            ))
            .unwrap(),
    ));

    let mut bridge = bridge();
    for control in controls {
        assert_eq!(
            bridge.translate(Event::AuthControl(control), None),
            Err(BridgeError::Unsupported)
        );
    }
    let initialized = bridge.translate(Event::Initialized, None).unwrap();
    assert_eq!(initialized.kind, EventKind::ProtocolInitialized);
    assert_eq!(initialized.native_thread_id, None);
    bridge
        .translate(
            Event::ThreadStarted {
                thread_id: "native-coding-thread".into(),
            },
            None,
        )
        .unwrap();
    let initialized = bridge.translate(Event::Initialized, None).unwrap();
    assert_eq!(
        initialized.native_thread_id.as_deref(),
        Some("native-coding-thread")
    );
    assert_eq!(
        bridge.support(Capability::NativeAdmission),
        Support::Unavailable
    );
}

#[test]
fn worker_bridge_also_rejects_legacy_auth_errors_and_unsupported_auth_method_events() {
    let mut bridge = bridge();
    for method in [LOGIN, COMPLETED, READ, CANCEL, LOGOUT] {
        assert_eq!(
            bridge.translate(
                Event::RequestFailed {
                    id: 2,
                    method,
                    code: -1,
                    message: AUTH_ERROR.into()
                },
                None
            ),
            Err(BridgeError::Unsupported)
        );
        assert_eq!(
            bridge.translate(
                Event::UnsupportedNotification {
                    method: method.into()
                },
                None
            ),
            Err(BridgeError::Unsupported)
        );
        assert_eq!(
            bridge.translate(
                Event::UnsupportedRequest {
                    id: WireId::Number(42),
                    method: method.into()
                },
                None
            ),
            Err(BridgeError::Unsupported)
        );
    }
    assert_eq!(
        bridge
            .translate(Event::Initialized, None)
            .unwrap()
            .native_thread_id,
        None
    );
}
