use crazyagent::plugins::check::{CHECK_TIME, check_stdio};
use crazyagent::team::Delegation;
use crazyagent::team::tool::{DROP_FILE, SERVER_COMMAND, TOOL_NAME, take};
use std::io::{Read, Write};
use std::process::{Command, Stdio};

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
            role: None,
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
