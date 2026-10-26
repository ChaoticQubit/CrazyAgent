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

struct TeamLaunch {
    _directory: tempfile::TempDir,
    request: LaunchRequest,
    work: PathBuf,
}

fn team_launch(harness: &str) -> TeamLaunch {
    use std::os::unix::fs::PermissionsExt;
    let entry = crate::harnesses::catalog(&crate::harnesses::Search::from_environment())
        .into_iter()
        .find(|entry| entry.id == harness)
        .and_then(|entry| entry.executable)
        .unwrap_or_else(|| panic!("installed {harness}"));
    let directory = tempfile::Builder::new()
        .prefix("ca-team-launch-")
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    let base = directory.path().canonicalize().unwrap();
    let home = base.join("home");
    let work = base.join("work");
    for folder in [&home, &work] {
        std::fs::create_dir(folder).unwrap();
        std::fs::set_permissions(folder, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let profile = base.join("project.sb");
    std::fs::write(&profile, "(version 1)\n(allow default)\n").unwrap();
    TeamLaunch {
        _directory: directory,
        request: LaunchRequest {
            harness: harness.into(),
            executable: std::fs::canonicalize(entry).unwrap(),
            working_dir: work.clone(),
            account: AccountBinding::Subscription { home },
            model: None,
            tools: true,
            sandbox_profile: Some(profile),
            limits: LaunchLimits::default(),
        },
        work,
    }
}

#[test]
fn a_real_tools_on_claude_team_agent_keeps_its_default_tools_but_no_built_in_sub_agent() {
    use std::io::{BufRead, BufReader, Write};
    use std::process::Stdio;
    let background_off = |command: &std::process::Command| {
        command.get_envs().any(|(name, value)| {
            name == "CLAUDE_CODE_DISABLE_BACKGROUND_TASKS" && value == Some("1".as_ref())
        })
    };
    let mut off = team_launch("claude-code");
    off.request.tools = false;
    assert!(!background_off(&super::command(&off.request).unwrap()));
    let launch = team_launch("claude-code");
    let mut command = super::command(&launch.request).unwrap();
    assert!(background_off(&command));
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = crate::spawn_gate::spawn(&mut command).unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"hi\"}}\n")
        .unwrap();
    let stdout = child.stdout.take().unwrap();
    let (send, lines) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if send.send(line).is_err() {
                break;
            }
        }
    });
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    let tools: Vec<String> = loop {
        let line = lines
            .recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
            .expect("Claude Code names its tools when its session starts");
        let Ok(event) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if event["type"] == "system" && event["subtype"] == "init" {
            break serde_json::from_value(event["tools"].clone()).unwrap();
        }
    };
    let _ = child.kill();
    child.wait().unwrap();
    for kept in ["Bash", "Edit", "Read", "Write"] {
        assert!(tools.iter().any(|tool| tool == kept), "{kept}: {tools:?}");
    }
    for built_in in ["Agent", "Task", "Workflow", "TeamCreate"] {
        assert!(
            !tools.iter().any(|tool| tool == built_in),
            "{built_in}: {tools:?}"
        );
    }
}

#[test]
fn the_installed_codex_reads_a_tools_on_team_agent_with_its_own_sub_agents_off() {
    let launch = team_launch("codex");
    let mut features = std::process::Command::new(&launch.request.executable);
    features
        .args(
            super::codex_configuration(&launch.request, false)
                .iter()
                .flat_map(|setting| ["-c", setting]),
        )
        .args(["features", "list"])
        .env_clear()
        .env("HOME", launch.request.account.home())
        .env("CODEX_HOME", launch.request.account.home())
        .env("PATH", "/usr/bin:/bin")
        .current_dir(&launch.work)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let output = crate::spawn_gate::output(&mut features).unwrap();
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let state = |name: &str| {
        text.lines().find_map(|line| {
            let columns: Vec<&str> = line.split_whitespace().collect();
            (columns.first() == Some(&name))
                .then(|| columns.last().copied())
                .flatten()
        })
    };
    assert_eq!(state("apps"), Some("false"), "{text}");
    assert_eq!(state("multi_agent"), Some("false"), "{text}");
    assert_eq!(state("multi_agent_v2"), Some("false"), "{text}");
    assert_eq!(state("shell_tool"), Some("true"), "{text}");
}

const LATE_RESULT_HARNESS: &str = r#"#!/bin/sh
IFS= read -r turn
printf '%s\n' '{"type":"system","subtype":"init","uuid":"init-1","session_id":"native-session","model":"observed-model","capabilities":[]}'
printf '%s\n' '{"type":"assistant","uuid":"step-1","session_id":"native-session","parent_tool_use_id":null,"message":{"id":"message-1","type":"message","role":"assistant","model":"observed-model","content":[{"type":"text","text":"FIRST"}]}}'
printf '%s\n' '{"type":"result","subtype":"success","uuid":"result-1","session_id":"native-session","is_error":false,"result":"FIRST"}'
printf '%s\n' '{"type":"system","subtype":"task_notification","uuid":"task-2","session_id":"native-session","task_id":"b1","run_id":"r1","tool_use_id":"tool-1","status":"completed","output_file":"","summary":"sleep 30 finished"}'
printf '%s\n' '{"type":"assistant","uuid":"step-2","session_id":"native-session","parent_tool_use_id":null,"message":{"id":"message-2","type":"message","role":"assistant","model":"observed-model","content":[{"type":"text","text":"LATE"}]}}'
printf '%s\n' '{"type":"result","subtype":"success","uuid":"result-2","session_id":"native-session","is_error":false,"result":"LATE"}'
IFS= read -r turn
printf '%s\n' '{"type":"assistant","uuid":"step-3","session_id":"native-session","parent_tool_use_id":null,"message":{"id":"message-3","type":"message","role":"assistant","model":"observed-model","content":[{"type":"text","text":"SECOND"}]}}'
printf '%s\n' '{"type":"result","subtype":"success","uuid":"result-3","session_id":"native-session","is_error":false,"result":"SECOND"}'
/bin/cat > /dev/null
"#;

fn turn(session: &mut super::Session, prompt: &str) -> Vec<LaunchEvent> {
    session.send(prompt).unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    let mut events = Vec::new();
    while !events
        .iter()
        .any(|event| matches!(event, LaunchEvent::TurnFinished { .. }))
    {
        assert!(std::time::Instant::now() < deadline, "{events:?}");
        if let Some(event) = session
            .next_event(std::time::Duration::from_millis(200))
            .unwrap()
        {
            events.push(event);
        }
    }
    events
}

#[test]
fn a_team_claude_agent_counts_and_drops_lines_after_its_result_and_takes_the_next_turn() {
    use std::os::unix::fs::PermissionsExt;
    let mut launch = team_launch("claude-code");
    let harness = launch.work.parent().unwrap().join("late-harness");
    std::fs::write(&harness, LATE_RESULT_HARNESS).unwrap();
    std::fs::set_permissions(&harness, std::fs::Permissions::from_mode(0o700)).unwrap();
    launch.request.executable = harness;
    launch.request.tools = false;
    let mut session = super::Session::start(launch.request).unwrap();
    let first = turn(&mut session, "Start the background check.");
    assert!(first.contains(&LaunchEvent::Text {
        text: "FIRST".into()
    }));
    assert_eq!(
        first.last(),
        Some(&LaunchEvent::TurnFinished { success: true })
    );
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    let mut late = Vec::new();
    while late.len() < 3 {
        assert!(std::time::Instant::now() < deadline, "{late:?}");
        if let Some(event) = session
            .next_event(std::time::Duration::from_millis(200))
            .unwrap()
        {
            late.push(event);
        }
    }
    assert_eq!(
        late,
        [1, 2, 3].map(|lines| LaunchEvent::LateOutput { lines }),
        "the lines after the result are counted and dropped, never decoded"
    );
    let second = turn(&mut session, "What did the check say?");
    assert_eq!(
        second,
        [
            LaunchEvent::Text {
                text: "SECOND".into()
            },
            LaunchEvent::TurnFinished { success: true }
        ]
    );
    session.stop();
}
