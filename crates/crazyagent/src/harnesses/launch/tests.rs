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
        unconfined: false,
        git_identity: None,
        limits: LaunchLimits::default(),
    };
    let (mut driver, startup) = Driver::new(&request, false).unwrap();
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

#[cfg(target_os = "macos")]
struct TeamLaunch {
    _directory: tempfile::TempDir,
    request: LaunchRequest,
    work: PathBuf,
}

#[cfg(target_os = "macos")]
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
            unconfined: false,
            git_identity: None,
            limits: LaunchLimits::default(),
        },
        work,
    }
}

/// macOS only: an agent launch needs Seatbelt, so every other system refuses it with
/// `launch_sandbox_unavailable`, which `harnesses::launch::tests` asserts in its own row.
#[cfg(target_os = "macos")]
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

/// macOS only: an agent launch needs Seatbelt, so every other system refuses it with
/// `launch_sandbox_unavailable`, which `harnesses::launch::tests` asserts in its own row.
#[cfg(target_os = "macos")]
#[test]
fn a_tools_on_claude_team_agent_waits_long_enough_for_its_mcp_servers_to_connect() {
    let connect_limit = |command: &std::process::Command| {
        command
            .get_envs()
            .find(|(name, _)| *name == "MCP_TIMEOUT")
            .and_then(|(_, value)| value.map(std::ffi::OsStr::to_owned))
    };
    let mut off = team_launch("claude-code");
    off.request.tools = false;
    assert_eq!(connect_limit(&super::command(&off.request).unwrap()), None);
    let on = team_launch("claude-code");
    assert_eq!(
        connect_limit(&super::command(&on.request).unwrap()),
        Some("60000".into()),
        "Claude Code's own 30 s connect limit drops context-mode on a busy machine"
    );
}

/// macOS only: no agent launches elsewhere (`launch_sandbox_unavailable`), so a Linux build
/// host installs no Codex whose reading of a team agent's configuration could be checked.
#[cfg(target_os = "macos")]
#[test]
fn the_installed_codex_reads_a_tools_on_team_agent_with_its_own_sub_agents_off() {
    let launch = team_launch("codex");
    let mut features = std::process::Command::new(&launch.request.executable);
    features
        .args(
            super::codex_configuration(&launch.request, false, None)
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

#[cfg(target_os = "macos")]
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

#[cfg(target_os = "macos")]
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

/// macOS only: an agent launch needs Seatbelt, so every other system refuses it with
/// `launch_sandbox_unavailable`, which `harnesses::launch::tests` asserts in its own row.
#[cfg(target_os = "macos")]
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
    session.stop().unwrap();
}

#[cfg(target_os = "macos")]
const USAGE_RESULT_HARNESS: &str = r#"#!/bin/sh
IFS= read -r turn
printf '%s\n' '{"type":"system","subtype":"init","uuid":"init-1","session_id":"native-session","model":"observed-model","capabilities":[]}'
printf '%s\n' '{"type":"assistant","uuid":"step-1","session_id":"native-session","parent_tool_use_id":null,"message":{"id":"message-1","type":"message","role":"assistant","model":"observed-model","content":[{"type":"text","text":"DONE"}]}}'
printf '%s\n' '{"type":"result","subtype":"success","uuid":"result-1","session_id":"native-session","is_error":false,"result":"DONE","usage":{"input_tokens":12,"output_tokens":30,"cache_read_input_tokens":900,"cache_creation_input_tokens":40}}'
IFS= read -r turn
printf '%s\n' '{"type":"assistant","uuid":"step-2","session_id":"native-session","parent_tool_use_id":null,"message":{"id":"message-2","type":"message","role":"assistant","model":"observed-model","content":[{"type":"text","text":"QUIET"}]}}'
printf '%s\n' '{"type":"result","subtype":"success","uuid":"result-2","session_id":"native-session","is_error":false,"result":"QUIET"}'
/bin/cat > /dev/null
"#;

/// macOS only: an agent launch needs Seatbelt, so every other system refuses it with
/// `launch_sandbox_unavailable`, which `harnesses::launch::tests` asserts in its own row.
#[cfg(target_os = "macos")]
#[test]
fn a_piped_claude_turn_reports_the_usage_its_result_carries_before_it_finishes() {
    use std::os::unix::fs::PermissionsExt;
    let mut launch = team_launch("claude-code");
    let harness = launch.work.parent().unwrap().join("usage-harness");
    std::fs::write(&harness, USAGE_RESULT_HARNESS).unwrap();
    std::fs::set_permissions(&harness, std::fs::Permissions::from_mode(0o700)).unwrap();
    launch.request.executable = harness;
    launch.request.tools = false;
    let mut session = super::Session::start(launch.request).unwrap();
    let first = turn(&mut session, "Finish the check.");
    let reported = crate::usage::capture::Tokens {
        input: 12,
        output: 30,
        cache_read: 900,
        cache_write: 40,
    };
    assert_eq!(
        first[first.len() - 2..],
        [
            LaunchEvent::Usage { tokens: reported },
            LaunchEvent::TurnFinished { success: true }
        ],
        "{first:?}"
    );
    let second = turn(&mut session, "Say nothing about usage.");
    assert!(
        !second
            .iter()
            .any(|event| matches!(event, LaunchEvent::Usage { .. })),
        "a result without usage reports none: {second:?}"
    );
    session.stop().unwrap();
}

fn codex_driver_in_a_turn() -> Driver {
    codex_driver_in_a_turn_keeping(false)
}

fn codex_driver_in_a_turn_keeping(native_threads: bool) -> Driver {
    let working_dir = std::env::temp_dir();
    let cwd = working_dir.to_str().unwrap().to_owned();
    let request = LaunchRequest {
        harness: "codex".into(),
        executable: PathBuf::from("/usr/bin/true"),
        working_dir,
        account: AccountBinding::Url {
            home: std::env::temp_dir().join("launch-test-home"),
            base_url: "http://127.0.0.1:11434".into(),
            key: None,
        },
        model: None,
        tools: native_threads,
        sandbox_profile: None,
        unconfined: false,
        git_identity: None,
        limits: LaunchLimits::default(),
    };
    let (approval, sandbox) = if native_threads {
        ("never", json!({"type":"dangerFullAccess"}))
    } else {
        (
            "on-request",
            json!({"type":"readOnly","networkAccess":false}),
        )
    };
    let (mut driver, startup) = Driver::new(&request, native_threads).unwrap();
    assert_eq!(startup.len(), 1);
    assert!(driver.send("Reply with OK.").unwrap().is_none());
    let (_, writes) = driver
        .receive(&line(json!({"id":1,"result":{
            "userAgent":"unit-agent", "codexHome":"/unit/codex",
            "platformFamily":"unix", "platformOs":"macos"
        }})))
        .unwrap();
    assert_eq!(writes.len(), 2);
    let (events, writes) = driver
        .receive(&line(json!({"id":2,"result":{
            "thread":{
                "id":"own-thread", "cliVersion":"0.164.0", "createdAt":1, "updatedAt":1,
                "cwd":cwd, "ephemeral":false, "modelProvider":"unit-provider", "preview":"",
                "projectId":null, "sessionId":"own-session", "source":"appServer",
                "status":{"type":"idle"}, "turns":[]
            },
            "cwd":cwd, "model":"unit-model", "modelProvider":"unit-provider",
            "approvalPolicy":approval, "approvalsReviewer":"user",
            "sandbox":sandbox
        }})))
        .unwrap();
    assert!(matches!(events.as_slice(), [LaunchEvent::Ready { .. }]));
    assert_eq!(writes.len(), 1);
    driver
        .receive(&line(json!({"id":3,"result":{"turn":{
            "id":"own-turn", "rootTurnId":"own-turn", "items":[], "status":"inProgress"
        }}})))
        .unwrap();
    driver
}

#[test]
fn a_team_codex_agent_interrupts_a_codex_native_sub_agent_and_finishes_its_turn() {
    let mut driver = codex_driver_in_a_turn();
    let (events, writes) = driver
        .receive(&line(json!({"method":"turn/started","params":{
            "threadId":"child-thread",
            "turn":{"id":"child-turn","rootTurnId":"own-turn","items":[],"status":"inProgress"}
        }})))
        .unwrap();
    assert!(events.is_empty());
    assert_eq!(writes.len(), 1);
    assert_eq!(
        serde_json::from_slice::<Value>(&writes[0]).unwrap(),
        json!({"id":4,"method":"turn/interrupt","params":{
            "threadId":"child-thread","turnId":"child-turn"
        }})
    );
    let (events, writes) = driver
        .receive(&line(json!({"method":"item/agentMessage/delta","params":{
            "threadId":"child-thread", "turnId":"child-turn", "itemId":"child-item",
            "delta":"child text"
        }})))
        .unwrap();
    assert!(events.is_empty() && writes.is_empty());
    let (events, _) = driver
        .receive(&line(json!({"method":"turn/completed","params":{
            "threadId":"own-thread",
            "turn":{"id":"own-turn","items":[],"status":"completed"}
        }})))
        .unwrap();
    assert!(
        events
            .iter()
            .any(|event| matches!(event, LaunchEvent::TurnFinished { success: true }))
    );
}

/// PA-7: a native Codex lead's sub-agent threads run on; the lead's own turn still ends
/// on its own `turn/completed`, whatever its children send.
#[test]
fn a_native_codex_lead_keeps_its_codex_sub_agents_running_and_finishes_its_turn() {
    let mut driver = codex_driver_in_a_turn_keeping(true);
    for frame in [
        json!({"method":"turn/started","params":{
            "threadId":"child-thread",
            "turn":{"id":"child-turn","rootTurnId":"own-turn","items":[],"status":"inProgress"}
        }}),
        json!({"method":"item/agentMessage/delta","params":{
            "threadId":"child-thread", "turnId":"child-turn", "itemId":"child-item",
            "delta":"child text"
        }}),
        json!({"method":"turn/completed","params":{
            "threadId":"child-thread",
            "turn":{"id":"child-turn","items":[],"status":"completed"}
        }}),
    ] {
        let (events, writes) = driver.receive(&line(frame)).unwrap();
        assert!(events.is_empty(), "{events:?}");
        assert!(
            writes.is_empty(),
            "a native lead never interrupts its sub-agents"
        );
    }
    let (events, writes) = driver
        .receive(&line(json!({"method":"turn/completed","params":{
            "threadId":"own-thread",
            "turn":{"id":"own-turn","items":[],"status":"completed"}
        }})))
        .unwrap();
    assert!(writes.is_empty());
    assert!(
        events
            .iter()
            .any(|event| matches!(event, LaunchEvent::TurnFinished { success: true }))
    );
}

/// macOS only: an agent launch needs Seatbelt, so every other system refuses it with
/// `launch_sandbox_unavailable`, which `harnesses::launch::tests` asserts in its own row.
#[cfg(target_os = "macos")]
#[test]
fn a_team_agent_commits_as_the_owner_and_its_claude_settings_turn_attribution_off() {
    use std::os::unix::fs::PermissionsExt;
    use std::process::{Command, Stdio};
    let directory = tempfile::Builder::new()
        .prefix("ca-identity-")
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    let base = directory.path().canonicalize().unwrap();
    let [repo, owner, home, work] = ["repo", "owner", "home", "work"].map(|name| base.join(name));
    for folder in [&repo, &owner, &home, &work] {
        std::fs::create_dir(folder).unwrap();
        std::fs::set_permissions(folder, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    std::fs::write(
        owner.join(".gitconfig"),
        "[user]\n\tname = Repo Owner\n\temail = owner@example.invalid\n",
    )
    .unwrap();
    let initialized = crate::spawn_gate::status(
        Command::new("/usr/bin/git")
            .env_clear()
            .env("HOME", &owner)
            .args(["init", "-q", "--template="])
            .arg(&repo),
    )
    .unwrap();
    assert!(initialized.success());
    let profile = base.join("project.sb");
    std::fs::write(&profile, "(version 1)\n(allow default)\n").unwrap();
    let (name, email) = crate::landing::owner_git_identity(&repo, &owner).unwrap();
    assert!(super::GitIdentity::new("Bad <name>".into(), email.clone()).is_err());
    assert!(super::GitIdentity::new(name.clone(), "two\nlines".into()).is_err());
    assert!(super::GitIdentity::new(" ".into(), email.clone()).is_err());
    let request = LaunchRequest {
        harness: "claude-code".into(),
        executable: PathBuf::from("/usr/bin/true"),
        working_dir: work.clone(),
        account: AccountBinding::Subscription { home },
        model: None,
        tools: true,
        sandbox_profile: Some(profile),
        unconfined: false,
        git_identity: Some(super::GitIdentity::new(name, email).unwrap()),
        limits: LaunchLimits::default(),
    };
    let command = super::command(&request).unwrap();
    let arguments: Vec<String> = command
        .get_args()
        .map(|argument| argument.to_str().unwrap().to_owned())
        .collect();
    let at = arguments
        .iter()
        .position(|argument| argument == "--settings")
        .unwrap();
    let settings: Value = serde_json::from_str(&arguments[at + 1]).unwrap();
    assert_eq!(settings["attribution"], json!({"commit": "", "pr": ""}));
    assert_eq!(settings["includeCoAuthoredBy"], json!(false));
    let env: Vec<(std::ffi::OsString, std::ffi::OsString)> = command
        .get_envs()
        .filter_map(|(name, value)| Some((name.to_owned(), value?.to_owned())))
        .collect();
    let git = |args: &[&str]| -> String {
        let output = crate::spawn_gate::output(
            Command::new("/usr/bin/git")
                .env_clear()
                .envs(env.iter().map(|(name, value)| (name, value)))
                .current_dir(&work)
                .args(args)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped()),
        )
        .unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    };
    git(&["init", "-q", "--template="]);
    std::fs::write(work.join("file.txt"), "agent work\n").unwrap();
    git(&["add", "file.txt"]);
    git(&["commit", "-q", "-m", "Agent work"]);
    assert_eq!(
        git(&["log", "-1", "--format=%an|%ae|%cn|%ce"]),
        "Repo Owner|owner@example.invalid|Repo Owner|owner@example.invalid\n"
    );
}

/// Off macOS no agent launch is built, since agents run under Seatbelt alone in this
/// version: a request that is valid in every other way, its sandbox profile named, is
/// refused with `launch_sandbox_unavailable` and nothing is written into the agent's home.
#[cfg(all(unix, not(target_os = "macos")))]
#[test]
fn a_launch_off_macos_is_refused_with_the_sandbox_code() {
    use std::os::unix::fs::PermissionsExt;
    let directory = tempfile::Builder::new()
        .prefix("ca-refused-launch-")
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    let base = directory.path().canonicalize().unwrap();
    let [home, work] = ["home", "work"].map(|name| base.join(name));
    for folder in [&home, &work] {
        std::fs::create_dir(folder).unwrap();
        std::fs::set_permissions(folder, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let profile = base.join("project.sb");
    std::fs::write(&profile, "(version 1)\n(allow default)\n").unwrap();
    for (harness, tools) in [
        ("claude-code", true),
        ("claude-code", false),
        ("codex", true),
    ] {
        let request = LaunchRequest {
            harness: harness.into(),
            executable: PathBuf::from("/usr/bin/true"),
            working_dir: work.clone(),
            account: AccountBinding::Subscription { home: home.clone() },
            model: None,
            tools,
            sandbox_profile: Some(profile.clone()),
            unconfined: false,
            git_identity: None,
            limits: LaunchLimits::default(),
        };
        assert_eq!(
            super::command(&request).unwrap_err(),
            super::LaunchError("launch_sandbox_unavailable".into(), None),
            "{harness} with tools {tools}"
        );
    }
    assert!(std::fs::read_dir(&home).unwrap().next().is_none());
}

/// An npm-built harness is a script whose first line asks `env` for its interpreter, and a
/// tools-off launch runs with PATH /usr/bin:/bin, where `env` finds no Node from nvm, volta
/// or Homebrew. The headless launch starts the interpreter the tool resolver finds, by its
/// absolute file, then the script's real file, and PATH stays exactly the tools-off value.
/// The stand-in asks for `sh`, which the resolver finds in the system folders on every Mac.
#[cfg(target_os = "macos")]
#[test]
fn an_env_script_harness_starts_headless_through_its_resolved_interpreter_with_tools_off() {
    use std::os::unix::fs::PermissionsExt;
    let directory = tempfile::Builder::new()
        .prefix("ca-env-script-")
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    let base = directory.path().canonicalize().unwrap();
    let [home, work] = ["home", "work"].map(|name| base.join(name));
    for folder in [&home, &work] {
        std::fs::create_dir(folder).unwrap();
        std::fs::set_permissions(folder, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let profile = base.join("project.sb");
    std::fs::write(&profile, "(version 1)\n(allow default)\n").unwrap();
    let install = tempfile::tempdir().unwrap();
    let script = install
        .path()
        .canonicalize()
        .unwrap()
        .join("lib/node_modules/@anthropic-ai/claude-code/cli.js");
    std::fs::create_dir_all(script.parent().unwrap()).unwrap();
    std::fs::write(&script, "#!/usr/bin/env sh\nexit 0\n").unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    let request = LaunchRequest {
        harness: "claude-code".into(),
        executable: script.clone(),
        working_dir: work.clone(),
        account: AccountBinding::Subscription { home: home.clone() },
        model: None,
        tools: false,
        sandbox_profile: Some(profile.clone()),
        unconfined: false,
        git_identity: None,
        limits: LaunchLimits::default(),
    };
    let command = super::command(&request).unwrap();
    let interpreter =
        crate::tools::resolve("sh", &crate::tools::Places::from_environment(None)).unwrap();
    assert!(interpreter.program.is_absolute());
    let args: Vec<&std::ffi::OsStr> = command.get_args().collect();
    let at = args
        .iter()
        .position(|argument| *argument == interpreter.program.as_os_str())
        .expect("the resolved interpreter starts the harness");
    assert_eq!(args.get(at + 1), Some(&script.as_os_str()));
    let path = command
        .get_envs()
        .find(|(name, _)| *name == "PATH")
        .and_then(|(_, value)| value)
        .unwrap();
    assert_eq!(path, super::TOOLS_OFF_PATH);
}

#[cfg(target_os = "macos")]
#[test]
fn a_launch_path_starts_with_the_agent_git_only_while_the_temp_folder_holds_it() {
    use crate::sandbox::toolchains::{self, ToolHomes};
    use std::os::unix::fs::PermissionsExt;
    let directory = tempfile::Builder::new()
        .prefix("ca-agent-git-")
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    let base = directory.path().canonicalize().unwrap();
    let [home, work] = ["home", "work"].map(|name| base.join(name));
    for folder in [&home, &work] {
        std::fs::create_dir(folder).unwrap();
        std::fs::set_permissions(folder, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let profile = base.join("project.sb");
    std::fs::write(&profile, "(version 1)\n(allow default)\n").unwrap();
    let path_of = |tools: bool| -> String {
        let request = LaunchRequest {
            harness: "claude-code".into(),
            executable: PathBuf::from("/usr/bin/true"),
            working_dir: work.clone(),
            account: AccountBinding::Subscription { home: home.clone() },
            model: None,
            tools,
            sandbox_profile: Some(profile.clone()),
            unconfined: false,
            git_identity: None,
            limits: LaunchLimits::default(),
        };
        let command = super::command(&request).unwrap();
        command
            .get_envs()
            .find(|(name, _)| *name == "PATH")
            .and_then(|(_, value)| value)
            .unwrap()
            .to_str()
            .unwrap()
            .to_owned()
    };
    let tools_on = toolchains::agent_path(&ToolHomes::owner().unwrap(), super::TOOLS_ON_PATH);
    assert_eq!(path_of(true), tools_on);
    assert_eq!(path_of(false), super::TOOLS_OFF_PATH);
    let tmp = toolchains::make_tmp(&home).unwrap();
    let bin = tmp.join("bin");
    assert_eq!(path_of(true), format!("{}:{tools_on}", bin.display()));
    assert_eq!(
        path_of(false),
        format!("{}:{}", bin.display(), super::TOOLS_OFF_PATH)
    );
    toolchains::remove_tmp(&home).unwrap();
    assert_eq!(path_of(false), super::TOOLS_OFF_PATH);
}

/// Starts `program` with `args` as the leader of a process group of its own. Inputs: the
/// program and its arguments. Output: the child and its group as a pid.
#[cfg(unix)]
fn group_leader(program: &str, args: &[&str]) -> (std::process::Child, rustix::process::Pid) {
    use std::os::unix::process::CommandExt;
    let child = crate::spawn_gate::spawn(
        std::process::Command::new(program)
            .args(args)
            .process_group(0)
            .env_clear()
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null()),
    )
    .unwrap();
    let group = rustix::process::Pid::from_child(&child);
    (child, group)
}

/// CB-2 one signal: a pane group that already answered ESRCH gets no KILL from the stop
/// that ends it, since its number may lead a new group by then. macOS answers EPERM for a
/// group whose last member it just reaped, so the test first waits for ESRCH.
#[cfg(unix)]
#[test]
fn a_pane_group_that_answered_esrch_is_never_signalled() {
    let (mut ended, group) = group_leader("/usr/bin/true", &[]);
    assert!(ended.wait().unwrap().success());
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    while rustix::process::test_kill_process_group(group) != Err(rustix::io::Errno::SRCH) {
        assert!(
            std::time::Instant::now() < deadline,
            "the reaped group answers ESRCH"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let calls = std::cell::RefCell::new(Vec::new());

    let gone = super::end_pane_group_with(
        super::Ending::claim(ended.id()),
        std::time::Duration::from_secs(1),
        || {
            let answer = rustix::process::test_kill_process_group(group);
            calls.borrow_mut().push(("test", answer));
            answer != Err(rustix::io::Errno::SRCH)
        },
        || calls.borrow_mut().push(("kill", Ok(()))),
    );
    assert!(gone, "a group that answered ESRCH is gone");
    assert_eq!(
        calls.into_inner(),
        [("test", Err(rustix::io::Errno::SRCH))],
        "the gone group is tested once and never signalled"
    );
}

/// CB-2 one signal: a stop that finds its pane's group held by another path, as the
/// terminal host's close of that pane, only watches the group and never signals it, so the
/// group is signalled from one path only. Once the hold is gone a stop ends the group.
#[cfg(unix)]
#[test]
fn a_pane_group_another_path_holds_is_only_watched() {
    use std::os::unix::process::ExitStatusExt;
    let (mut survivor, group) = group_leader("/bin/sleep", &["120"]);
    let id = survivor.id();
    let other = super::Ending::claim(id).expect("no path holds a new group");
    assert!(
        super::Ending::claim(id).is_none(),
        "a held group has one holder"
    );
    assert!(
        super::Ending::claim(id).is_none(),
        "a refused claim leaves the holder's hold in place"
    );
    let reaped = std::thread::spawn(move || survivor.wait());
    let kills = std::cell::Cell::new(0);

    let gone = super::end_pane_group_with(
        None,
        std::time::Duration::from_millis(300),
        || rustix::process::test_kill_process_group(group) != Err(rustix::io::Errno::SRCH),
        || kills.set(kills.get() + 1),
    );
    drop(other);
    let ended = super::end_pane_group(id, super::Ending::claim(id));
    if !ended {
        let _ = rustix::process::kill_process_group(group, rustix::process::Signal::KILL);
    }
    let status = reaped.join().unwrap().unwrap();
    assert!(!gone, "the held group still runs");
    assert_eq!(
        kills.get(),
        0,
        "a stop never signals a group another path holds"
    );
    assert!(
        ended,
        "a hold freed before its group ended leaves the group claimable, and a stop ends it"
    );
    assert_eq!(status.signal(), Some(9), "the group ended by KILL");
}

/// CB-2 r1 F1: once a holder saw its group answer ESRCH, the number stays refused for a
/// while, so no later path, such as a session that notices its pane gone, claims and
/// signals a number a new group may lead by then.
#[cfg(unix)]
#[test]
fn a_group_its_holder_saw_end_stays_refused_to_later_paths() {
    use std::os::unix::process::ExitStatusExt;
    let (mut survivor, _) = group_leader("/bin/sleep", &["120"]);
    let id = survivor.id();
    let held = super::Ending::claim(id).expect("no path holds a new group");
    let reaped = std::thread::spawn(move || survivor.wait());

    let ended = super::end_pane_group(id, Some(held));
    let later = super::Ending::claim(id);
    let status = reaped.join().unwrap().unwrap();
    assert!(ended, "the holder ends its group");
    assert_eq!(status.signal(), Some(9), "the group ended by KILL");
    assert!(
        later.is_none(),
        "a number whose group just ended is refused to a later path"
    );
}

/// CB-2 r1 F1: a path that has no hold, because another path held the group or just ended
/// it, only watches the group for the wait and never claims or signals it itself.
#[cfg(unix)]
#[test]
fn a_path_without_a_hold_only_watches_and_never_claims() {
    use std::os::unix::process::ExitStatusExt;
    let (mut survivor, group) = group_leader("/bin/sleep", &["120"]);
    let id = survivor.id();
    let reaped = std::thread::spawn(move || survivor.wait());
    let started = std::time::Instant::now();

    let ended = super::end_pane_group(id, None);
    let watched = started.elapsed();
    if rustix::process::test_kill_process_group(group) != Err(rustix::io::Errno::SRCH) {
        let _ = rustix::process::kill_process_group(group, rustix::process::Signal::KILL);
    }
    let status = reaped.join().unwrap().unwrap();
    assert!(!ended, "a path without a hold never ends the group itself");
    assert!(
        watched >= super::GROUP_EXIT_WAIT,
        "it watched the whole wait: {watched:?}"
    );
    assert_eq!(
        status.signal(),
        Some(9),
        "the group ran until the test ended it"
    );
}

/// Collects what an output reader hands the session until it ends, a skipped line as
/// `skipped` and the end as `closed`. Takes the receiver. Returns the items in order.
#[cfg(unix)]
fn handed(lines: std::sync::mpsc::Receiver<super::Line>) -> Vec<String> {
    lines
        .iter()
        .map(|line| match line {
            super::Line::Bytes(bytes) => String::from_utf8(bytes).unwrap(),
            super::Line::Skipped => "skipped".to_owned(),
            super::Line::Closed => "closed".to_owned(),
        })
        .collect()
}

/// Owner 11-11, no output cap: a harness stdout line longer than `LINE_BYTES_MAX` is passed
/// over to its newline and reported once as skipped, the line after it still arrives, and
/// output past the old 8 MiB total never ends the session.
#[cfg(unix)]
#[test]
fn a_stdout_line_past_the_line_bound_is_skipped_and_the_next_line_arrives() {
    let mut child = crate::spawn_gate::spawn(
        std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(format!(
                "head -c {} /dev/zero | tr '\\0' x; printf '\\nnext\\n'",
                2 * super::LINE_BYTES_MAX + 1
            ))
            .stdout(std::process::Stdio::piped()),
    )
    .unwrap();
    let stdout = child.stdout.take().unwrap();
    let (sender, lines) = std::sync::mpsc::sync_channel(super::LINES_AHEAD_MAX);
    let reader = std::thread::spawn(move || super::forward(stdout, sender));
    let got = handed(lines);
    reader.join().unwrap();
    assert!(child.wait().unwrap().success());
    assert_eq!(got, ["skipped", "next\n", "closed"]);
}

/// Owner 11-11: a durable harness output file with a line longer than `LINE_BYTES_MAX` is
/// read on past it, the line after it arrives, and a file that ends inside a skipped line
/// hands none of its rest.
#[cfg(unix)]
#[test]
fn a_durable_output_line_past_the_line_bound_is_skipped_and_never_handed_in_part() {
    let folder = tempfile::tempdir().unwrap();
    let long = "x".repeat(2 * super::LINE_BYTES_MAX + 1);
    for (body, want) in [
        (
            format!("{long}\nnext\n"),
            vec!["skipped", "next\n", "closed"],
        ),
        (long.clone(), vec!["skipped", "closed"]),
    ] {
        let path = folder.path().join("output");
        std::fs::write(&path, &body).unwrap();
        let output = std::fs::File::open(&path).unwrap();
        let probe = std::fs::File::open(&path).unwrap();
        let (sender, lines) = std::sync::mpsc::sync_channel(super::LINES_AHEAD_MAX);
        let reader = std::thread::spawn(move || super::tail(output, probe, sender));
        let got = handed(lines);
        reader.join().unwrap();
        assert_eq!(got, want, "{} bytes", body.len());
    }
}

/// L1C r1 F2: a durable output log the session has read to its end past
/// `DURABLE_OUTPUT_KEPT_MAX` is emptied while its harness still holds it, and the session
/// reads on from the start, where the harness's next appends land; the agent is never
/// stopped and the log on disk stays bounded.
#[cfg(unix)]
#[test]
fn a_durable_output_log_read_past_its_bound_is_emptied_and_read_on() {
    use std::io::Write;
    let folder = tempfile::tempdir().unwrap();
    let path = folder.path().join("output.log");
    let mut writer = std::fs::OpenOptions::new()
        .append(true)
        .create_new(true)
        .open(&path)
        .unwrap();
    rustix::fs::flock(
        &writer,
        rustix::fs::FlockOperation::NonBlockingLockExclusive,
    )
    .unwrap();
    writer.set_len(super::DURABLE_OUTPUT_KEPT_MAX + 1).unwrap();
    writer.write_all(b"\n").unwrap();
    let output = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
        .unwrap();
    let probe = std::fs::File::open(&path).unwrap();
    let (sender, lines) = std::sync::mpsc::sync_channel(super::LINES_AHEAD_MAX);
    let reader = std::thread::spawn(move || super::tail(output, probe, sender));
    let until = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while std::fs::metadata(&path).unwrap().len() > super::DURABLE_OUTPUT_KEPT_MAX {
        assert!(
            std::time::Instant::now() < until,
            "the log was never emptied"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert_eq!(std::fs::metadata(&path).unwrap().len(), 0);
    writer.write_all(b"next\n").unwrap();
    assert_eq!(std::fs::metadata(&path).unwrap().len(), 5);
    std::thread::sleep(std::time::Duration::from_millis(200));
    drop(writer);
    let got = handed(lines);
    reader.join().unwrap();
    assert_eq!(got, ["skipped", "next\n", "closed"]);
}

/// L1C r1 F2: the watcher of a reattached agent, whose log nothing reads, empties the log
/// once it passes `DURABLE_OUTPUT_KEPT_MAX`; a log within the bound and a path that now
/// names another file are left alone.
#[cfg(unix)]
#[test]
fn an_unread_durable_output_log_past_its_bound_is_emptied_only_when_it_is_the_probed_file() {
    let folder = tempfile::tempdir().unwrap();
    let path = folder.path().join("output.log");
    let log = std::fs::File::create(&path).unwrap();
    log.set_len(super::DURABLE_OUTPUT_KEPT_MAX).unwrap();
    let probe = std::fs::File::open(&path).unwrap();
    assert!(!super::trim_unread_output(&path, &probe));
    assert_eq!(
        std::fs::metadata(&path).unwrap().len(),
        super::DURABLE_OUTPUT_KEPT_MAX
    );

    log.set_len(super::DURABLE_OUTPUT_KEPT_MAX + 1).unwrap();
    let moved = folder.path().join("moved.log");
    std::fs::rename(&path, &moved).unwrap();
    std::fs::write(&path, b"other\n").unwrap();
    assert!(!super::trim_unread_output(&path, &probe));
    assert_eq!(std::fs::read(&path).unwrap(), b"other\n");
    assert_eq!(
        std::fs::metadata(&moved).unwrap().len(),
        super::DURABLE_OUTPUT_KEPT_MAX + 1
    );

    std::fs::remove_file(&path).unwrap();
    std::fs::rename(&moved, &path).unwrap();
    assert!(super::trim_unread_output(&path, &probe));
    assert_eq!(std::fs::metadata(&path).unwrap().len(), 0);
}
