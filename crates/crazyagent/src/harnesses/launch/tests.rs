use super::{AccountBinding, Driver, LaunchEvent, LaunchLimits, LaunchRequest, rate_limited};
use serde_json::{Value, json};
use std::path::PathBuf;

fn claude_driver() -> Driver {
    let request = LaunchRequest {
        harness: "claude".into(),
        executable: PathBuf::from("/usr/bin/true"),
        working_dir: std::env::temp_dir(),
        account: AccountBinding::Url {
            home: std::env::temp_dir().join("launch-test-home"),
            base_url: "http://127.0.0.1:11434".into(),
            key: None,
        },
        model: None,
        tools: false,
        sandbox_profile: None,
        limits: LaunchLimits::default(),
    };
    let (mut driver, startup) = Driver::new(&request).unwrap();
    assert!(startup.is_empty());
    driver
        .send("Reply with one short greeting.")
        .unwrap()
        .unwrap();
    let (events, _) = driver
        .receive(&line(json!({
            "type":"system", "subtype":"init", "uuid":"init-1",
            "session_id":"native-session", "model":"observed-model",
            "claude_code_version":"2.1.309", "cwd":"/worktree",
            "tools":[], "mcp_servers":[], "apiKeySource":"none",
            "permissionMode":"default", "slash_commands":[],
            "output_style":"default", "skills":[], "plugins":[],
            "capabilities":[]
        })))
        .unwrap();
    assert!(matches!(events.as_slice(), [LaunchEvent::Ready { .. }]));
    driver
}

fn line(value: Value) -> Vec<u8> {
    serde_json::to_vec(&value).unwrap()
}

fn turn_errors(events: &[LaunchEvent]) -> Vec<bool> {
    events
        .iter()
        .filter_map(|event| match event {
            LaunchEvent::TurnError { rate_limited } => Some(*rate_limited),
            _ => None,
        })
        .collect()
}

#[test]
fn rate_limit_wording_is_recognized_and_other_errors_are_not() {
    for message in [
        "Rate limit reached for requests",
        "usage_limit_reached",
        "You exceeded your current quota",
        "unexpected status 429",
        "Too Many Requests",
    ] {
        assert!(rate_limited(message), "{message}");
    }
    for message in ["stream disconnected", "model not found", "invalid_request"] {
        assert!(!rate_limited(message), "{message}");
    }
}

#[test]
fn claude_rate_limit_errors_and_rejections_report_a_rate_limited_turn_error() {
    let mut driver = claude_driver();
    let (events, _) = driver
        .receive(&line(json!({
            "type":"assistant", "uuid":"assistant-1", "session_id":"native-session",
            "parent_tool_use_id":null, "error":"rate_limit",
            "message":{"id":"message-1","type":"message","role":"assistant",
                "model":"observed-model","content":[],"stop_reason":"end_turn",
                "usage":{"input_tokens":1,"output_tokens":0}}
        })))
        .unwrap();
    assert_eq!(turn_errors(&events), [true]);
    let (events, _) = driver
        .receive(&line(json!({
            "type":"rate_limit_event", "uuid":"rate-1", "session_id":"native-session",
            "rate_limit_info":{"status":"rejected"}
        })))
        .unwrap();
    assert_eq!(turn_errors(&events), [true]);
    let (events, _) = driver
        .receive(&line(json!({
            "type":"rate_limit_event", "uuid":"rate-2", "session_id":"native-session",
            "rate_limit_info":{"status":"allowed_warning"}
        })))
        .unwrap();
    assert!(turn_errors(&events).is_empty());
}

#[test]
fn claude_assistant_errors_other_than_rate_limits_are_not_rate_limited() {
    let mut driver = claude_driver();
    let (events, _) = driver
        .receive(&line(json!({
            "type":"assistant", "uuid":"assistant-1", "session_id":"native-session",
            "parent_tool_use_id":null, "error":"invalid_request",
            "message":{"id":"message-1","type":"message","role":"assistant",
                "model":"observed-model","content":[],"stop_reason":"end_turn",
                "usage":{"input_tokens":1,"output_tokens":0}}
        })))
        .unwrap();
    assert_eq!(turn_errors(&events), [false]);
}
