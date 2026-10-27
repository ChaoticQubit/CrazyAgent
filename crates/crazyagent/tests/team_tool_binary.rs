use crazyagent::plugins::check::{CHECK_TIME, check_stdio};
use crazyagent::team::Delegation;
use crazyagent::team::tool::{
    ANSWER_TOOL, CALLS_PER_TICK_MAX, CHANGES_TOOL, Call, DROP_FILE, LIVE_SERVER_COMMAND,
    MAILBOX_FOLDER, MESSAGE_TOOL, SERVER_COMMAND, STATUS_TOOL, TOOL_NAME, answer_call, take,
    take_calls,
};
use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[test]
fn the_engine_binary_serves_the_delegate_tool_to_an_mcp_client() {
    let directory = tempfile::tempdir().unwrap();
    let record = directory.path().canonicalize().unwrap().join(DROP_FILE);
    let engine = env!("CARGO_BIN_EXE_crazyagent");
    let arguments = [SERVER_COMMAND.to_owned(), record.display().to_string()];
    assert_eq!(
        check_stdio(engine, &arguments, &[], CHECK_TIME).unwrap(),
        [TOOL_NAME],
        "the engine's own MCP check sees exactly the delegate tool"
    );

    let mut server = crazyagent::spawn_gate::spawn(
        Command::new(engine)
            .args(&arguments)
            .env_clear()
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null()),
    )
    .unwrap();
    let mut input = server.stdin.take().unwrap();
    input
        .write_all(
            concat!(
                "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{\"protocolVersion\":\"2025-06-18\",\"capabilities\":{},\"clientInfo\":{\"name\":\"test\",\"version\":\"1\"}}}\n",
                "{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n",
                "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/call\",\"params\":{\"name\":\"delegate\",\"arguments\":{\"harness\":\"codex\",\"task\":\"Write apple.txt\"}}}\n"
            )
            .as_bytes(),
        )
        .unwrap();
    drop(input);
    let mut output = String::new();
    server
        .stdout
        .take()
        .unwrap()
        .read_to_string(&mut output)
        .unwrap();
    assert!(server.wait().unwrap().success(), "{output}");
    let answers: Vec<serde_json::Value> = output
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(answers.len(), 2, "{output}");
    assert_eq!(answers[1]["id"], 2);
    assert_eq!(answers[1]["result"]["isError"], false, "{output}");
    assert_eq!(
        take(&record),
        [Delegation {
            harness: Some("codex".into()),
            prompt: "Write apple.txt".into(),
        }]
    );

    let wrong = crazyagent::spawn_gate::output(
        Command::new(engine)
            .arg(SERVER_COMMAND)
            .stdin(Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped()),
    )
    .unwrap();
    assert!(!wrong.status.success());
    assert!(String::from_utf8_lossy(&wrong.stderr).contains("usage: crazyagent team-mcp"));
}

#[test]
fn the_engine_binary_serves_the_live_team_tools_with_the_engine_answer() {
    let directory = tempfile::tempdir().unwrap();
    let mailbox = directory
        .path()
        .canonicalize()
        .unwrap()
        .join(MAILBOX_FOLDER);
    std::fs::create_dir(&mailbox).unwrap();
    let engine = env!("CARGO_BIN_EXE_crazyagent");
    let arguments = [
        LIVE_SERVER_COMMAND.to_owned(),
        mailbox.display().to_string(),
    ];
    assert_eq!(
        check_stdio(engine, &arguments, &[], CHECK_TIME).unwrap(),
        [
            TOOL_NAME,
            MESSAGE_TOOL,
            ANSWER_TOOL,
            STATUS_TOOL,
            CHANGES_TOOL
        ],
    );

    let mut server = crazyagent::spawn_gate::spawn(
        Command::new(engine)
            .args(&arguments)
            .env_clear()
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null()),
    )
    .unwrap();
    let mut input = server.stdin.take().unwrap();
    input
        .write_all(
            concat!(
                "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{\"protocolVersion\":\"2025-06-18\",\"capabilities\":{},\"clientInfo\":{\"name\":\"test\",\"version\":\"1\"}}}\n",
                "{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n",
                "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/call\",\"params\":{\"name\":\"delegate\",\"arguments\":{\"harness\":\"codex\",\"task\":\"Write apple.txt\",\"role\":\"@apple-writer\"}}}\n",
                "{\"jsonrpc\":\"2.0\",\"id\":3,\"method\":\"tools/call\",\"params\":{\"name\":\"message\",\"arguments\":{\"agent\":\"@apple-writer\",\"text\":\"\\u0007\"}}}\n",
                "{\"jsonrpc\":\"2.0\",\"id\":4,\"method\":\"tools/call\",\"params\":{\"name\":\"status\",\"arguments\":{}}}\n"
            )
            .as_bytes(),
        )
        .unwrap();
    drop(input);
    let answered = [
        (
            Call::Delegate {
                task: "Write apple.txt".into(),
                harness: Some("codex".into()),
                role: Some("@apple-writer".into()),
            },
            "Task 7 is queued for codex.",
            false,
        ),
        (Call::Status, "No sub-agent is working.", true),
    ];
    for (expected, text, error) in answered {
        let deadline = Instant::now() + Duration::from_secs(20);
        let taken = loop {
            let taken = take_calls(&mailbox, CALLS_PER_TICK_MAX);
            if !taken.is_empty() || Instant::now() >= deadline {
                break taken;
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        assert_eq!(taken.len(), 1, "one call waits at a time: {taken:?}");
        assert_eq!(taken[0].1.as_ref(), Some(&expected));
        answer_call(&mailbox, &taken[0].0, text, error).unwrap();
    }
    let mut output = String::new();
    server
        .stdout
        .take()
        .unwrap()
        .read_to_string(&mut output)
        .unwrap();
    assert!(server.wait().unwrap().success(), "{output}");
    let answers: Vec<serde_json::Value> = output
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(answers.len(), 4, "{output}");
    assert_eq!(answers[1]["result"]["isError"], false, "{output}");
    assert_eq!(
        answers[1]["result"]["content"][0]["text"],
        "Task 7 is queued for codex."
    );
    assert_eq!(answers[2]["id"], 3);
    assert_eq!(answers[2]["result"]["isError"], true, "{output}");
    assert!(
        answers[2]["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .starts_with("Not sent"),
        "{output}"
    );
    assert_eq!(answers[3]["result"]["isError"], true);
    assert_eq!(
        answers[3]["result"]["content"][0]["text"],
        "No sub-agent is working."
    );
    assert_eq!(
        std::fs::read_dir(&mailbox).unwrap().count(),
        0,
        "every call and answer was taken"
    );

    let wrong = crazyagent::spawn_gate::output(
        Command::new(engine)
            .arg(LIVE_SERVER_COMMAND)
            .arg(directory.path().join("missing"))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()),
    )
    .unwrap();
    assert!(!wrong.status.success());
}
