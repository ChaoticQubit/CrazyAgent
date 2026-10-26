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

/// The strings are Claude Code 2.1.309's own, from its shipped bundle
/// `/opt/homebrew/Caskroom/claude-code@latest/2.1.309/claude`: the limit-line prefixes and
/// model-credit pattern its `Bii` check uses, whole limit lines, the limit names its
/// `You've hit your ${name}` line takes, and the warning and notice prefixes it keeps
/// apart from limits.
#[test]
fn claude_code_limit_lines_are_rate_limits_and_its_warnings_are_not() {
    let prefixes = [
        "You've hit your",
        "You've reached your",
        "You're out of usage credits",
        "Your org is out of usage \u{b7} add funds to continue",
        "Your org is out of usage \u{b7} contact your admin",
        "Your seat type doesn't include usage credits",
        "Your seat type doesn't include usage",
        "Your usage allocation has been disabled by your admin",
        "Your group's usage limit is set to $0",
        "Fable 5 requires usage credits",
        "You're out of extra usage",
        "Your seat type doesn't include extra usage",
    ];
    let lines = [
        "You've hit your monthly spend limit.",
        "You've hit your channel's monthly spend limit.",
        "You've hit your team's shared budget. /model to switch models.",
        "You've reached your Fable limit.",
        "You've hit your fast limit",
        "Fable 5 requires usage credits.",
        "Sonnet requires usage credits.",
        "API Error: Request rejected (429)",
    ];
    let names = [
        "session limit",
        "weekly limit",
        "Opus limit",
        "Sonnet limit",
        "Fable limit",
        "usage credit limit",
    ];
    for message in prefixes.into_iter().chain(lines) {
        assert!(rate_limited(message), "{message}");
    }
    for name in names {
        let line = format!("You've hit your {name}");
        assert!(rate_limited(&line), "{line}");
    }
    for message in [
        "You've used",
        "You're close to",
        "You're now using usage credits",
        "Context limit reached",
        "API Error: Request was aborted.",
        "API Error: 400 due to tool use concurrency issues.",
        "Claude Opus is not available with the Claude Pro plan.",
        "Sonnet requires usage credits",
    ] {
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
