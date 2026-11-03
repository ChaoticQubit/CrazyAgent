use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{ChildStdin, Command, Output, Stdio};
use std::sync::mpsc::{Receiver, channel};
use std::thread;
use std::time::{Duration, Instant};
use tempfile::TempDir;
use trackwright_core::catalog::catalog;

const BIN: &str = env!("CARGO_BIN_EXE_trackwright");
const REPLY_WAIT: Duration = Duration::from_secs(30);
const REPLY_LINES_MAX: usize = 64;

/// Runs the built binary to completion in a folder with no trackwright variables set.
/// Inputs: folder, arguments. Output: the result.
fn run(cwd: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(cwd)
        .env_remove("TRACKWRIGHT_DB")
        .env_remove("TRACKWRIGHT_ACTOR")
        .output()
        .expect("the built trackwright binary runs")
}

/// Sends one JSON-RPC request line and waits, bounded, for the next reply line.
/// Inputs: the server's stdin, its reply lines, the request. Output: the reply.
fn call(stdin: &mut ChildStdin, replies: &Receiver<String>, request: Value) -> Value {
    writeln!(stdin, "{request}").expect("the server reads stdin");
    stdin.flush().expect("the server reads stdin");
    let line = replies
        .recv_timeout(REPLY_WAIT)
        .expect("the server replies within 30 s");
    let reply: Value = serde_json::from_str(&line).expect("each reply is one JSON line");
    assert_eq!(reply["jsonrpc"], "2.0", "{reply}");
    assert_eq!(reply["id"], request["id"], "{reply}");
    reply
}

/// Reads the tracker envelope a `tools/call` reply carries as JSON text. Input: the reply.
/// Output: the envelope.
fn tool_envelope(reply: &Value) -> Value {
    let text = reply["result"]["content"][0]["text"]
        .as_str()
        .expect("a tool reply carries the envelope as text");
    let envelope: Value = serde_json::from_str(text).expect("the text is the envelope");
    assert_eq!(envelope["api"], 1, "{envelope}");
    envelope
}

#[test]
fn the_built_binary_serves_mcp_over_stdio() {
    let root = TempDir::new().unwrap();
    let init = run(
        root.path(),
        &["init", "--key", "CA", "--actor", "owner", "--json"],
    );
    assert_eq!(init.status.code(), Some(0), "{init:?}");
    let mut server = Command::new(BIN)
        .args(["mcp", "--actor", "agent:one"])
        .current_dir(root.path())
        .env_remove("TRACKWRIGHT_DB")
        .env_remove("TRACKWRIGHT_ACTOR")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("the built trackwright binary starts");
    let mut stdin = server.stdin.take().unwrap();
    let stdout = server.stdout.take().unwrap();
    let (sender, replies) = channel();
    let reader = thread::spawn(move || {
        for line in BufReader::new(stdout).lines().take(REPLY_LINES_MAX) {
            let Ok(line) = line else { break };
            if sender.send(line).is_err() {
                break;
            }
        }
    });

    let initialize = call(
        &mut stdin,
        &replies,
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": {"name": "mcp-binary-test", "version": "1"}
        }}),
    );
    assert_eq!(initialize["result"]["protocolVersion"], "2025-06-18");
    writeln!(
        stdin,
        "{}",
        json!({"jsonrpc": "2.0", "method": "notifications/initialized"})
    )
    .unwrap();

    let listed = call(
        &mut stdin,
        &replies,
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
    );
    let tools: BTreeSet<&str> = listed["result"]["tools"]
        .as_array()
        .expect("tools/list replies a tool array")
        .iter()
        .map(|tool| tool["name"].as_str().expect("every tool has a name"))
        .collect();
    let ops: BTreeSet<&str> = catalog().iter().map(|spec| spec.kind.as_str()).collect();
    assert_eq!(tools, ops);

    let created = call(
        &mut stdin,
        &replies,
        json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {
            "name": "create",
            "arguments": {"project": "CA", "title": "Made over MCP"}
        }}),
    );
    let envelope = tool_envelope(&created);
    assert_eq!(envelope["ok"], true, "{envelope}");
    assert_eq!(envelope["data"]["id"], "CA-1");

    drop(stdin);
    let started = Instant::now();
    let status = loop {
        if let Some(status) = server.try_wait().unwrap() {
            break status;
        }
        if started.elapsed() > REPLY_WAIT {
            server.kill().unwrap();
            panic!("the server did not exit within 30 s of stdin closing");
        }
        thread::sleep(Duration::from_millis(50));
    };
    reader.join().unwrap();
    assert!(status.success(), "{status:?}");

    let shown = run(
        root.path(),
        &["show", "--id", "CA-1", "--actor", "owner", "--json"],
    );
    assert_eq!(shown.status.code(), Some(0), "{shown:?}");
    let shown: Value = serde_json::from_slice(&shown.stdout).unwrap();
    assert_eq!(shown["data"]["ticket"]["title"], "Made over MCP");
    assert_eq!(shown["data"]["ticket"]["created_by"], "agent:one");
}
