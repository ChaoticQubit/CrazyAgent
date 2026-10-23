use super::*;
use std::collections::BTreeMap;
use std::io::Cursor;
use std::os::unix::fs::{PermissionsExt, symlink};

fn requests(lines: &[Value]) -> Cursor<Vec<u8>> {
    let mut input = Vec::new();
    for line in lines {
        input.extend(serde_json::to_vec(line).unwrap());
        input.push(b'\n');
    }
    Cursor::new(input)
}

fn call_line(id: u64, arguments: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "method": "tools/call",
        "params": {"name": TOOL_NAME, "arguments": arguments}})
}

fn served(input: Cursor<Vec<u8>>, drop: &Path) -> Vec<Value> {
    let mut output = Vec::new();
    serve(input, &mut output, drop).unwrap();
    output
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_slice(line).unwrap())
        .collect()
}

fn by_id(answers: &[Value]) -> BTreeMap<String, Value> {
    answers
        .iter()
        .map(|answer| (answer["id"].to_string(), answer.clone()))
        .collect()
}

#[test]
fn the_delegate_tool_records_proposals_that_the_engine_takes_once() {
    let directory = tempfile::tempdir().unwrap();
    let drop = directory.path().join(DROP_FILE);
    let mut input = requests(&[
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
            "protocolVersion": "2025-06-18", "capabilities": {},
            "clientInfo": {"name": "codex", "version": "1"}}}),
        json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}}),
        call_line(3, json!({"task": "Write apple.txt"})),
        call_line(
            4,
            json!({"harness": "Claude-Code", "task": " Review the notes "}),
        ),
        call_line(5, json!({"task": "   "})),
        json!({"jsonrpc": "2.0", "id": 6, "method": "tools/call",
            "params": {"name": "shell", "arguments": {}}}),
        json!({"jsonrpc": "2.0", "id": 7, "method": "resources/list"}),
        json!({"jsonrpc": "2.0", "id": 8, "method": "ping"}),
    ])
    .into_inner();
    input.extend(b"not json\n");
    let answers = served(Cursor::new(input), &drop);
    assert_eq!(
        answers.len(),
        9,
        "a notification gets no answer: {answers:?}"
    );
    let answers = by_id(&answers);
    assert_eq!(answers["1"]["result"]["protocolVersion"], "2025-06-18");
    assert_eq!(answers["1"]["result"]["serverInfo"]["name"], SERVER_NAME);
    assert!(answers["1"]["result"]["capabilities"]["tools"].is_object());
    let tools = answers["2"]["result"]["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0]["name"], TOOL_NAME);
    assert_eq!(tools[0]["inputSchema"]["required"], json!(["task"]));
    for id in ["3", "4"] {
        assert_eq!(answers[id]["result"]["isError"], false, "{id}");
        assert_eq!(answers[id]["result"]["content"][0]["text"], RECORDED);
    }
    assert_eq!(answers["5"]["result"]["isError"], true);
    assert_eq!(answers["5"]["result"]["content"][0]["text"], INVALID);
    assert_eq!(answers["6"]["error"]["code"], -32602);
    assert_eq!(answers["7"]["error"]["code"], -32601);
    assert_eq!(answers["8"]["result"], json!({}));
    assert_eq!(answers["null"]["error"]["code"], -32700);

    let mode = std::fs::metadata(&drop).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o600);
    assert_eq!(
        take(&drop),
        [
            Delegation {
                harness: None,
                prompt: "Write apple.txt".into(),
            },
            Delegation {
                harness: Some("claude-code".into()),
                prompt: "Review the notes".into(),
            },
        ]
    );
    assert!(!drop.exists(), "taking the proposals clears the record");
    assert!(take(&drop).is_empty(), "a proposal is taken once");
}

#[test]
fn the_tool_records_no_more_tasks_than_one_reply_may_hand_off() {
    let directory = tempfile::tempdir().unwrap();
    let drop = directory.path().join(DROP_FILE);
    let calls: Vec<Value> = (1..=DELEGATIONS_PER_REPLY_MAX as u64 + 1)
        .map(|id| call_line(id, json!({"task": format!("Write file {id}")})))
        .collect();
    let answers = served(requests(&calls), &drop);
    assert_eq!(answers.len(), DELEGATIONS_PER_REPLY_MAX + 1);
    let refused: Vec<&Value> = answers
        .iter()
        .filter(|answer| answer["result"]["isError"] == true)
        .collect();
    assert_eq!(refused.len(), 1);
    assert_eq!(refused[0]["id"], DELEGATIONS_PER_REPLY_MAX as u64 + 1);
    assert_eq!(refused[0]["result"]["content"][0]["text"], FULL);
    assert_eq!(take(&drop).len(), DELEGATIONS_PER_REPLY_MAX);
}

#[test]
fn a_linked_record_is_never_followed_by_the_tool_or_the_engine() {
    let directory = tempfile::tempdir().unwrap();
    let elsewhere = directory.path().join("engine-file");
    let line = "{\"harness\":null,\"task\":\"Read the engine file\"}\n";
    std::fs::write(&elsewhere, line).unwrap();
    let drop = directory.path().join(DROP_FILE);
    symlink(&elsewhere, &drop).unwrap();

    let answers = served(
        requests(&[call_line(1, json!({"task": "Write a.txt"}))]),
        &drop,
    );
    assert_eq!(answers[0]["result"]["isError"], true);
    assert_eq!(answers[0]["result"]["content"][0]["text"], UNWRITABLE);
    assert!(
        take(&drop).is_empty(),
        "the engine does not read through a link"
    );
    assert!(
        std::fs::symlink_metadata(&drop).is_err(),
        "the link is removed"
    );
    assert_eq!(
        std::fs::read_to_string(&elsewhere).unwrap(),
        line,
        "the linked file is untouched"
    );
}

#[test]
fn the_engine_keeps_only_valid_recorded_proposals() {
    let directory = tempfile::tempdir().unwrap();
    let drop = directory.path().join(DROP_FILE);
    std::fs::write(
        &drop,
        concat!(
            "{\"harness\":\"codex\",\"task\":\"Write a.txt\"}\n",
            "{\"task\":\"Write b.txt\",\"extra\":1}\n",
            "{\"harness\":\"Bad Name\",\"task\":\"Write c.txt\"}\n",
            "{\"harness\":null,\"task\":\"\"}\n",
            "garbage\n",
            "{\"task\":\"Write d.txt\"}"
        ),
    )
    .unwrap();
    assert_eq!(
        take(&drop),
        [
            Delegation {
                harness: Some("codex".into()),
                prompt: "Write a.txt".into(),
            },
            Delegation {
                harness: None,
                prompt: "Write d.txt".into(),
            },
        ]
    );
}

#[test]
fn the_server_needs_an_absolute_drop_file() {
    let mut output = Vec::new();
    assert!(
        serve(
            Cursor::new(Vec::new()),
            &mut output,
            Path::new("relative.jsonl")
        )
        .is_err()
    );
    assert!(output.is_empty());
}
