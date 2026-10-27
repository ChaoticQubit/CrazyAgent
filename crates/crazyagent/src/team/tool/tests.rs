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
                role: None,
            },
            Delegation {
                harness: Some("claude-code".into()),
                prompt: "Review the notes".into(),
                role: None,
            },
        ]
    );
    assert!(!drop.exists(), "taking the proposals clears the record");
    assert!(take(&drop).is_empty(), "a proposal is taken once");
}

#[test]
fn the_delegate_tool_keeps_the_role_the_master_gives_a_job_as_a_handle() {
    let directory = tempfile::tempdir().unwrap();
    let drop = directory.path().join(DROP_FILE);
    let answers = by_id(&served(
        requests(&[
            json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": {}}),
            call_line(
                2,
                json!({"task": "Fix onboarding", "role": "Onboarding Fix"}),
            ),
            call_line(3, json!({"task": "Write docs", "role": "@docs"})),
            call_line(4, json!({"task": "No role", "role": "  "})),
            call_line(5, json!({"task": "Bad role", "role": "???"})),
            call_line(6, json!({"task": "Long role", "role": "a".repeat(65)})),
        ]),
        &drop,
    ));
    let schema = &answers["1"]["result"]["tools"][0]["inputSchema"];
    assert_eq!(schema["properties"]["role"]["type"], "string");
    assert_eq!(schema["required"], json!(["task"]));
    for id in ["2", "3", "4"] {
        assert_eq!(answers[id]["result"]["isError"], false, "{id}");
    }
    for id in ["5", "6"] {
        assert_eq!(answers[id]["result"]["isError"], true, "{id}");
        assert_eq!(answers[id]["result"]["content"][0]["text"], INVALID);
    }
    assert_eq!(
        take(&drop),
        [
            Delegation {
                harness: None,
                prompt: "Fix onboarding".into(),
                role: Some("onboarding-fix".into()),
            },
            Delegation {
                harness: None,
                prompt: "Write docs".into(),
                role: Some("docs".into()),
            },
            Delegation {
                harness: None,
                prompt: "No role".into(),
                role: None,
            },
        ]
    );
    std::fs::write(
        &drop,
        "{\"harness\":null,\"task\":\"Edited\",\"role\":\"Not A Handle!!\"}\n{\"harness\":null,\"task\":\"Odd\",\"role\":\"!!!\"}\n",
    )
    .unwrap();
    assert_eq!(
        take(&drop)
            .into_iter()
            .map(|found| found.role)
            .collect::<Vec<_>>(),
        [Some("not-a-handle".into()), None],
        "a role read back from the record is a handle again or nothing"
    );
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
                role: None,
            },
            Delegation {
                harness: None,
                prompt: "Write d.txt".into(),
                role: None,
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

#[test]
fn live_calls_keep_only_bounded_plain_arguments_and_a_valid_role() {
    let delegate = |arguments: Value| live_call(TOOL_NAME, Some(&arguments)).unwrap();
    assert_eq!(
        delegate(json!({"task": " Write page.md ", "harness": "Codex", "role": "@page-writer"})),
        Ok(Call::Delegate {
            task: "Write page.md".into(),
            harness: Some("codex".into()),
            role: Some("page-writer".into()),
        })
    );
    assert_eq!(
        delegate(json!({"task": "Write page.md", "role": "Page Writer"})),
        Ok(Call::Delegate {
            task: "Write page.md".into(),
            harness: None,
            role: Some("page-writer".into()),
        })
    );
    assert_eq!(
        delegate(json!({"task": "Write page.md", "role": "  "})),
        Ok(Call::Delegate {
            task: "Write page.md".into(),
            harness: None,
            role: None,
        })
    );
    for role in ["@", "???", &"r".repeat(65)] {
        assert_eq!(
            delegate(json!({"task": "Write page.md", "role": role})),
            Err(ROLE_INVALID),
            "{role}"
        );
    }
    assert_eq!(delegate(json!({"task": "   "})), Err(LIVE_INVALID));
    assert_eq!(
        delegate(json!({"task": "x".repeat(TASK_PROMPT_BYTES_MAX + 1)})),
        Err(LIVE_INVALID)
    );
    assert_eq!(
        live_call(
            MESSAGE_TOOL,
            Some(&json!({"agent": " @page-writer ", "text": "Use the old header.\n"}))
        )
        .unwrap(),
        Ok(Call::Message {
            agent: "@page-writer".into(),
            text: "Use the old header.".into(),
        })
    );
    for arguments in [
        json!({"agent": "@a", "text": "bell \u{7}"}),
        json!({"agent": "", "text": "hi"}),
        json!({"agent": "a".repeat(65), "text": "hi"}),
        json!({"text": "hi"}),
    ] {
        assert_eq!(
            live_call(ANSWER_TOOL, Some(&arguments)).unwrap(),
            Err(TYPED_INVALID),
            "{arguments}"
        );
    }
    assert_eq!(
        live_call(
            ESCALATE_TOOL,
            Some(&json!({"text": " Ada wants to force-push main. Allow it? "}))
        )
        .unwrap(),
        Ok(Call::Escalate {
            text: "Ada wants to force-push main. Allow it?".into(),
        })
    );
    for arguments in [
        json!({"text": "  "}),
        json!({"text": "x".repeat(super::super::events::ESCALATION_BYTES_MAX + 1)}),
        json!({"text": "bell \u{7}"}),
        json!({}),
    ] {
        assert_eq!(
            live_call(ESCALATE_TOOL, Some(&arguments)).unwrap(),
            Err(ESCALATE_INVALID),
            "{arguments}"
        );
    }
    assert_eq!(live_call(STATUS_TOOL, None).unwrap(), Ok(Call::Status));
    assert_eq!(
        live_call(CHANGES_TOOL, Some(&json!({}))).unwrap(),
        Ok(Call::Changes)
    );
    assert_eq!(live_call("shell", None), None);
}

#[test]
fn the_engine_checks_again_every_call_read_from_a_mailbox() {
    assert_eq!(checked(Call::Status), Ok(Call::Status));
    let planted = Call::Delegate {
        task: "Write page.md".into(),
        harness: Some("Codex".into()),
        role: None,
    };
    assert_eq!(checked(planted), Err(LIVE_INVALID));
    let planted = Call::Message {
        agent: "@a".into(),
        text: "x".repeat(TASK_PROMPT_BYTES_MAX + 1),
    };
    assert_eq!(checked(planted), Err(TYPED_INVALID));
    let planted = Call::Delegate {
        task: "Write page.md".into(),
        harness: None,
        role: Some("Page".into()),
    };
    assert_eq!(checked(planted), Err(LIVE_INVALID));
}

#[test]
fn the_mailbox_takes_calls_in_order_answers_them_whole_and_never_follows_a_link() {
    let directory = tempfile::tempdir().unwrap();
    let mailbox = directory
        .path()
        .canonicalize()
        .unwrap()
        .join(MAILBOX_FOLDER);
    std::fs::create_dir(&mailbox).unwrap();
    let first = "0".repeat(32);
    let second = "1".repeat(32);
    std::fs::write(
        mailbox.join(format!("{first}{CALL_SUFFIX}")),
        b"{\"tool\":\"status\"}\n",
    )
    .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(20));
    std::fs::write(mailbox.join(format!("{second}{CALL_SUFFIX}")), b"not json").unwrap();
    let secret = directory.path().join("secret.json");
    std::fs::write(&secret, b"{\"tool\":\"changes\"}").unwrap();
    symlink(
        &secret,
        mailbox.join(format!("{}{CALL_SUFFIX}", "2".repeat(32))),
    )
    .unwrap();
    std::fs::write(mailbox.join("notes.call"), b"{\"tool\":\"status\"}").unwrap();
    std::fs::write(
        mailbox.join(format!("{}{CALL_SUFFIX}", "3".repeat(32))),
        vec![b' '; CALL_BYTES_MAX as usize + 1],
    )
    .unwrap();

    let taken = take_calls(&mailbox, CALLS_PER_TICK_MAX);
    assert_eq!(taken.len(), 3, "{taken:?}");
    assert_eq!(taken[0], (first.clone(), Some(Call::Status)));
    assert_eq!(taken[1], (second.clone(), None));
    assert_eq!(taken[2], ("3".repeat(32), None));
    assert!(take_calls(&mailbox, CALLS_PER_TICK_MAX).is_empty());
    assert!(
        std::fs::symlink_metadata(mailbox.join(format!("{}{CALL_SUFFIX}", "2".repeat(32)))).is_ok()
    );
    assert!(mailbox.join("notes.call").is_file());
    assert_eq!(std::fs::read(&secret).unwrap(), b"{\"tool\":\"changes\"}");

    answer_call(&mailbox, &first, "Two agents are working.", false).unwrap();
    assert_eq!(
        read_reply(&mailbox.join(format!("{first}{REPLY_SUFFIX}"))),
        Some(("Two agents are working.".into(), false))
    );
    assert!(!mailbox.join(format!("{first}{PART_SUFFIX}")).exists());
    let long = "é".repeat(REPLY_TEXT_MAX);
    answer_call(&mailbox, &second, &long, true).unwrap();
    let (text, error) = read_reply(&mailbox.join(format!("{second}{REPLY_SUFFIX}"))).unwrap();
    assert!(error && text.len() <= REPLY_TEXT_MAX && text.chars().all(|c| c == 'é'));

    let planted = "4".repeat(32);
    symlink(&secret, mailbox.join(format!("{planted}{PART_SUFFIX}"))).unwrap();
    assert!(answer_call(&mailbox, &planted, "never", false).is_err());
    assert_eq!(std::fs::read(&secret).unwrap(), b"{\"tool\":\"changes\"}");
}

#[test]
fn the_live_server_needs_an_absolute_plain_mailbox_and_lists_six_tools() {
    let directory = tempfile::tempdir().unwrap();
    let base = directory.path().canonicalize().unwrap();
    let list = requests(&[json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"})]);
    assert!(serve_live(list.clone(), Vec::new(), Path::new("relative")).is_err());
    let linked = base.join("linked");
    symlink(&base, &linked).unwrap();
    assert!(serve_live(list.clone(), Vec::new(), &linked).is_err());
    let mut output = Vec::new();
    serve_live(list, &mut output, &base).unwrap();
    let answer: Value = serde_json::from_slice(output.trim_ascii()).unwrap();
    let names: Vec<&str> = answer["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        [
            TOOL_NAME,
            MESSAGE_TOOL,
            ANSWER_TOOL,
            STATUS_TOOL,
            CHANGES_TOOL,
            ESCALATE_TOOL
        ]
    );
}

#[test]
fn the_engine_runs_only_a_call_it_claimed_and_a_claimed_call_is_never_reported_untaken() {
    let directory = tempfile::tempdir().unwrap();
    let mailbox = directory.path().canonicalize().unwrap();
    let blocked = "c".repeat(32);
    std::fs::write(
        mailbox.join(format!("{blocked}{CALL_SUFFIX}")),
        br#"{"tool":"status"}"#,
    )
    .unwrap();
    std::fs::create_dir(mailbox.join(format!("{blocked}{TAKEN_SUFFIX}"))).unwrap();
    assert!(take_calls(&mailbox, CALLS_PER_TICK_MAX).is_empty());
    assert!(mailbox.join(format!("{blocked}{CALL_SUFFIX}")).is_file());
    std::fs::remove_file(mailbox.join(format!("{blocked}{CALL_SUFFIX}"))).unwrap();
    std::fs::remove_dir(mailbox.join(format!("{blocked}{TAKEN_SUFFIX}"))).unwrap();

    let served = mailbox.clone();
    let server = std::thread::spawn(move || {
        ask(
            &served,
            &Call::Status,
            std::time::Duration::from_millis(800),
        )
    });
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let id = loop {
        if let Some((_, id)) = calls(&mailbox, CALL_SUFFIX).pop() {
            break id;
        }
        assert!(std::time::Instant::now() < deadline, "no call was written");
        std::thread::sleep(std::time::Duration::from_millis(10));
    };
    std::fs::rename(
        mailbox.join(format!("{id}{CALL_SUFFIX}")),
        mailbox.join(format!("{id}{TAKEN_SUFFIX}")),
    )
    .unwrap();
    assert_eq!(server.join().unwrap(), (NOT_ANSWERED.to_owned(), true));
}
