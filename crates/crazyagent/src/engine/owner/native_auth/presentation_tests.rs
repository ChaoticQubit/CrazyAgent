use super::*;
use crate::adapters::{claude, codex};
use serde_json::{Value, json};

const URL: &str = "https://auth.example.invalid/login?state=owner-secret-703";
const EMAIL: &str = "owner-secret-703@example.invalid";

fn fixture() -> (Value, Command) {
    let scope = NativeAuthScope {
        expected_database_instance: "a".repeat(64),
        connection: CredentialRef::new("connection").unwrap(),
        expected_config_id: ConfigId::new("e".repeat(64)).unwrap(),
        expected_source_hash: "c".repeat(64),
    };
    let command = Command::Start(NativeAuthStart {
        request_id: RequestId::new("wire-operation").unwrap(),
        scope: scope.clone(),
        method: NativeAuthMethod::CodexBrowser,
    });
    let fact = json!({
        "operation_id": "wire-operation",
        "action": {"kind": "sign_in", "method": "codex_browser"},
        "binding": {
            "database_instance": scope.expected_database_instance,
            "engine_epoch": "b".repeat(64),
            "config_id": scope.expected_config_id,
            "source_hash": scope.expected_source_hash,
            "at_revision": Cursor::new(0),
            "connection": {
                "id": "connection", "provider": "provider", "harness": "codex",
                "protocol": "native", "account_ref": "account",
                "endpoint": {"kind": "native"},
                "model": {"id": "model", "author": "author"},
                "auth": {"method": "native_managed", "profile_ref": "profile"}
            },
            "custody_id": "d".repeat(64)
        },
        "generation": 1, "phase": "unavailable", "code": "native_auth_host_unavailable"
    });
    let fact: NativeAuthFact = serde_json::from_value(fact).unwrap();
    let value = json!({
        "status": "fact", "request_id": "wire-operation",
        "database_instance": "a".repeat(64), "engine_epoch": "b".repeat(64),
        "fact": fact
    });
    (value, command)
}

fn browser() -> (Value, Command) {
    let (mut value, command) = fixture();
    value["fact"]["phase"] = json!("awaiting_owner");
    value["fact"]["code"] = Value::Null;
    value["presentation"] = json!({
        "challenge": {"kind": "codex_browser", "login_id": "login-703", "auth_url": URL},
        "account": null
    });
    (value, command)
}

fn decode(value: &Value, command: &Command) -> Result<NativeAuthReply, EngineError> {
    decode_reply(&serde_json::to_vec(value).unwrap(), command)
}

fn roundtrip(value: &Value, command: &Command) -> NativeAuthReply {
    let reply = decode(value, command).unwrap();
    let frame = encode_reply(Ok(reply), command.request_id().clone()).unwrap();
    assert_eq!(
        u32::from_be_bytes(frame[..4].try_into().unwrap()) as usize,
        frame.len() - 4
    );
    decode_reply(&frame[4..], command).unwrap()
}

fn account(harness: &str, account: Value, logged_in: bool) -> (Value, Command) {
    let (mut value, mut command) = fixture();
    value["fact"]["phase"] = json!(if logged_in {
        "authenticated_observed"
    } else {
        "unauthenticated_observed"
    });
    value["fact"]["code"] = Value::Null;
    value["fact"]["binding"]["connection"]["harness"] = json!(harness);
    if harness == "claude" {
        value["fact"]["action"]["method"] = json!("claude_official");
        let Command::Start(start) = &mut command else {
            panic!("start fixture")
        };
        start.method = NativeAuthMethod::ClaudeOfficial;
    }
    value["presentation"] = json!({"challenge": null, "account": account});
    (value, command)
}

fn claude_account(logged_in: bool) -> Value {
    json!({
        "kind": "claude", "logged_in": logged_in,
        "auth_method": "future-owner-secret-703", "api_provider": "firstParty",
        "config_directory": "synthetic-private-profile",
        "email": {"state": "missing"}, "org_id": {"state": "null"},
        "org_name": {"state": "value", "text": "owner-secret-703-org"},
        "subscription_type": {"state": "value", "text": "owner-secret-703-subscription"}
    })
}

#[test]
fn legacy_fact_wire_stays_without_presentation() {
    let (value, command) = fixture();
    let reply = decode(&value, &command).unwrap();
    assert!(reply.challenge.is_none() && reply.account.is_none());
    let frame = encode_reply(Ok(reply), command.request_id().clone()).unwrap();
    let encoded: Value = serde_json::from_slice(&frame[4..]).unwrap();
    assert!(encoded.get("presentation").is_none());
    assert_eq!(encoded, value);
}

#[test]
fn browser_challenge_roundtrips_only_through_owner_wire() {
    let (value, command) = browser();
    let reply = roundtrip(&value, &command);
    let challenge = reply.challenge.as_ref().unwrap();
    let codex::LoginChallenge::Browser { login_id, auth_url } = challenge.codex() else {
        panic!("browser challenge expected")
    };
    assert_eq!(login_id, "login-703");
    assert_eq!(auth_url.expose(), URL);
    assert!(reply.account.is_none());
    assert!(!format!("{reply:?}").contains("owner-secret-703"));
    assert!(
        !String::from_utf8(encode_fact(&reply.fact).unwrap())
            .unwrap()
            .contains(URL)
    );
}

#[test]
fn device_code_roundtrips_with_exact_method() {
    let (mut value, mut command) = browser();
    value["fact"]["action"]["method"] = json!("codex_device_code");
    let Command::Start(start) = &mut command else {
        panic!("start fixture")
    };
    start.method = NativeAuthMethod::CodexDeviceCode;
    value["presentation"]["challenge"] = json!({
        "kind": "codex_device_code", "login_id": "device-703",
        "user_code": "SECRET-703", "verification_url": URL
    });
    let reply = roundtrip(&value, &command);
    let codex::LoginChallenge::DeviceCode {
        login_id,
        user_code,
        verification_url,
    } = reply.challenge.as_ref().unwrap().codex()
    else {
        panic!("device challenge expected")
    };
    assert_eq!(login_id, "device-703");
    assert_eq!(user_code.expose(), "SECRET-703");
    assert_eq!(verification_url.expose(), URL);
    assert!(!format!("{reply:?}").contains("SECRET-703"));
}

#[test]
fn codex_observed_account_kinds_remain_distinct() {
    let cases = [
        json!({"kind": "codex_api_key"}),
        json!({"kind": "codex_chatgpt", "email": EMAIL, "plan_type": "plus"}),
        json!({"kind": "codex_chatgpt", "email": null, "plan_type": "unknown"}),
        json!({"kind": "codex_amazon_bedrock", "uses_codex_managed_credentials": false}),
    ];
    for (index, wire) in cases.into_iter().enumerate() {
        let (value, command) = account("codex", wire, true);
        let reply = roundtrip(&value, &command);
        match (index, reply.account.as_ref().unwrap()) {
            (0, NativeAuthAccount::Codex(codex::AuthAccount::ApiKey)) => {}
            (1, NativeAuthAccount::Codex(codex::AuthAccount::Chatgpt { email, plan_type })) => {
                assert_eq!(email.as_ref().unwrap().expose(), EMAIL);
                assert_eq!(plan_type, "plus");
            }
            (2, NativeAuthAccount::Codex(codex::AuthAccount::Chatgpt { email, plan_type })) => {
                assert!(email.is_none());
                assert_eq!(plan_type, "unknown");
            }
            (
                3,
                NativeAuthAccount::Codex(codex::AuthAccount::AmazonBedrock {
                    uses_codex_managed_credentials,
                }),
            ) => assert!(!uses_codex_managed_credentials),
            _ => panic!("observed account kind changed"),
        }
        assert!(!format!("{reply:?}").contains(EMAIL));
    }
}

#[test]
fn claude_preserves_unknown_method_and_missing_null_value() {
    let (value, command) = account("claude", claude_account(true), true);
    let reply = roundtrip(&value, &command);
    let NativeAuthAccount::Claude(status) = reply.account.as_ref().unwrap() else {
        panic!("Claude account expected")
    };
    assert!(status.logged_in);
    let claude::AuthMethod::Unknown(method) = &status.auth_method else {
        panic!("unknown method must remain unknown")
    };
    assert_eq!(method.as_str(), "future-owner-secret-703");
    assert!(matches!(status.email, claude::AuthStatusField::Missing));
    assert!(matches!(status.org_id, claude::AuthStatusField::Null));
    let claude::AuthStatusField::Value(org) = &status.org_name else {
        panic!("org value expected")
    };
    assert_eq!(org.as_str(), "owner-secret-703-org");
    assert!(!format!("{reply:?}").contains("owner-secret-703"));
}

#[test]
fn claude_logged_out_observation_is_not_authenticated() {
    let (mut value, start) = account("claude", claude_account(false), false);
    let command = Command::Logout(NativeAuthLogout {
        request_id: start.request_id().clone(),
        scope: start.scope().clone(),
    });
    value["fact"]["action"] = json!({"kind": "logout"});
    let reply = roundtrip(&value, &command);
    assert_eq!(reply.fact.phase, NativeAuthPhase::UnauthenticatedObserved);
    let NativeAuthAccount::Claude(status) = reply.account.unwrap() else {
        panic!("Claude account expected")
    };
    assert!(!status.logged_in);
}

#[test]
fn read_preserves_operation_join_with_a_distinct_request_id() {
    let (mut value, start) = browser();
    let command = Command::Read(NativeAuthQuery {
        request_id: RequestId::new("wire-read").unwrap(),
        scope: start.scope().clone(),
        operation_id: start.operation_id().clone(),
    });
    value["request_id"] = json!("wire-read");
    let reply = roundtrip(&value, &command);
    assert_eq!(reply.request_id, *command.request_id());
    assert_eq!(reply.fact.operation_id, *command.operation_id());
    assert!(reply.challenge.is_some());
}

#[test]
fn challenge_rejects_wrong_phase_method_harness_or_epoch() {
    let (value, command) = browser();
    decode(&value, &command).unwrap();
    let paths = [
        ("/fact/phase", json!("checking_account")),
        ("/fact/action/method", json!("codex_device_code")),
        ("/fact/binding/connection/harness", json!("claude")),
        ("/fact/binding/engine_epoch", json!("f".repeat(64))),
        ("/fact/binding/connection/protocol", json!("openai")),
    ];
    for (path, replacement) in paths {
        let mut changed = value.clone();
        *changed.pointer_mut(path).unwrap() = replacement;
        assert!(decode(&changed, &command).is_err(), "{path}");
    }
}

#[test]
fn challenge_and_account_cannot_coexist() {
    let (mut value, command) = browser();
    decode(&value, &command).unwrap();
    value["presentation"]["account"] = json!({"kind": "codex_api_key"});
    assert!(decode(&value, &command).is_err());
}

#[test]
fn account_rejects_harness_phase_and_logged_in_mismatch() {
    let (value, command) = account("claude", claude_account(true), true);
    decode(&value, &command).unwrap();
    let paths = [
        ("/fact/phase", json!("unavailable")),
        ("/fact/binding/connection/harness", json!("codex")),
        ("/fact/binding/engine_epoch", json!("f".repeat(64))),
        ("/presentation/account/logged_in", json!(false)),
    ];
    for (path, replacement) in paths {
        let mut changed = value.clone();
        *changed.pointer_mut(path).unwrap() = replacement;
        assert!(decode(&changed, &command).is_err(), "{path}");
    }
}

#[test]
fn presentation_keeps_existing_request_and_source_joins() {
    let (value, command) = browser();
    decode(&value, &command).unwrap();
    let paths = [
        ("/request_id", json!("different-request")),
        ("/database_instance", json!("f".repeat(64))),
        ("/fact/operation_id", json!("different-operation")),
        ("/fact/binding/connection/id", json!("different-connection")),
        ("/fact/binding/source_hash", json!("f".repeat(64))),
        ("/fact/binding/config_id", json!("f".repeat(64))),
        ("/fact/generation", json!(0)),
    ];
    for (path, replacement) in paths {
        let mut changed = value.clone();
        *changed.pointer_mut(path).unwrap() = replacement;
        assert!(decode(&changed, &command).is_err(), "{path}");
    }
}

#[test]
fn presentation_rejects_unknown_fields_and_duplicate_keys() {
    let (value, command) = browser();
    decode(&value, &command).unwrap();
    for path in ["/presentation", "/presentation/challenge"] {
        let mut changed = value.clone();
        changed.pointer_mut(path).unwrap()["unexpected"] = json!("owner-secret-703");
        assert!(decode(&changed, &command).is_err());
    }
    let body = serde_json::to_string(&value).unwrap();
    let duplicate = body.replacen("{", "{\"status\":\"fact\",", 1);
    assert!(decode_reply(duplicate.as_bytes(), &command).is_err());
    let duplicate = body.replacen(
        "\"login_id\":\"login-703\"",
        "\"login_id\":\"duplicate\",\"login_id\":\"login-703\"",
        1,
    );
    assert_ne!(duplicate, body);
    assert!(decode_reply(duplicate.as_bytes(), &command).is_err());
}

#[test]
fn presentation_rejects_invalid_urls_and_text_bounds() {
    let (value, command) = browser();
    decode(&value, &command).unwrap();
    for url in [
        "javascript:owner-secret-703".into(),
        "https://bad host/".into(),
        "x".repeat(2049),
    ] {
        let mut changed = value.clone();
        changed["presentation"]["challenge"]["auth_url"] = json!(url);
        assert!(decode(&changed, &command).is_err());
    }
    let (mut value, command) = account(
        "codex",
        json!({"kind": "codex_chatgpt", "email": EMAIL, "plan_type": "plus"}),
        true,
    );
    decode(&value, &command).unwrap();
    value["presentation"]["account"]["email"] = json!("x".repeat(321));
    assert!(decode(&value, &command).is_err());
}

#[test]
fn reply_body_limit_applies_before_json_decoding() {
    let (value, command) = fixture();
    let body = serde_json::to_vec(&value).unwrap();
    decode_reply(&body, &command).unwrap();
    let mut oversized = vec![b' '; 65537];
    oversized.extend_from_slice(&body);
    assert!(decode_reply(&oversized, &command).is_err());
}

#[test]
fn encoder_revalidates_typed_presentation_without_echoing_secrets() {
    let (value, command) = browser();
    let mut reply = decode(&value, &command).unwrap();
    reply.fact.binding.engine_epoch = "f".repeat(64);
    let error = encode_reply(Ok(reply), command.request_id().clone()).unwrap_err();
    assert!(!format!("{error:?}").contains("owner-secret-703"));
}

#[test]
fn request_identity_postcondition_is_trippable() {
    let (value, command) = fixture();
    let reply = decode(&value, &command).unwrap();
    let result = std::panic::catch_unwind(|| {
        encode_reply(Ok(reply), RequestId::new("wrong-local-request").unwrap())
    });
    assert!(result.is_err());
}
