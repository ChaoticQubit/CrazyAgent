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
    serve(input, &mut output, drop, None, &[]).unwrap();
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
    assert_eq!(tools.len(), 3);
    assert_eq!(tools[0]["name"], TOOL_NAME);
    assert_eq!(tools[0]["inputSchema"]["required"], json!(["task"]));
    assert_eq!(tools[1], ask_tool());
    assert_eq!(tools[2], record_tool());
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

/// A master that replaces its private temporary folder with a link to another folder gets
/// nothing read from or removed in that folder by the engine; a real folder still works.
#[test]
fn a_swapped_private_folder_is_never_followed_by_the_engine() {
    let directory = tempfile::tempdir().unwrap();
    let agent = directory.path().join("agent-1");
    let elsewhere = directory.path().join("owner-folder");
    std::fs::create_dir(&agent).unwrap();
    std::fs::create_dir(&elsewhere).unwrap();
    let line = "{\"harness\":null,\"task\":\"Read the owner file\"}\n";
    let outside = elsewhere.join(DROP_FILE);
    std::fs::write(&outside, line).unwrap();
    let tmp = agent.join("tmp");
    symlink(&elsewhere, &tmp).unwrap();
    let drop = tmp.join(DROP_FILE);

    assert!(
        take(&drop).is_empty(),
        "the engine does not read through a swapped folder"
    );
    clear(&drop);
    assert_eq!(
        std::fs::read_to_string(&outside).unwrap(),
        line,
        "the file in the linked folder is neither read nor removed"
    );

    std::fs::remove_file(&tmp).unwrap();
    std::fs::create_dir(&tmp).unwrap();
    std::fs::write(&drop, line).unwrap();
    assert_eq!(
        take(&drop),
        [Delegation {
            harness: None,
            prompt: "Read the owner file".into(),
            role: None,
        }]
    );
    assert!(
        std::fs::symlink_metadata(&drop).is_err(),
        "taking clears the record"
    );
    assert_eq!(std::fs::read_to_string(&outside).unwrap(), line);
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
            Path::new("relative.jsonl"),
            None,
            &[]
        )
        .is_err()
    );
    assert!(output.is_empty());
}

#[test]
fn live_calls_keep_only_bounded_plain_arguments_and_a_valid_role() {
    let spawn = |arguments: Value| live_call(SPAWN_TOOL, Some(&arguments)).unwrap();
    assert_eq!(
        spawn(json!({"task": " Write page.md ", "harness": "Codex", "role": "@page-writer"})),
        Ok(Call::Spawn {
            task: "Write page.md".into(),
            harness: Some("codex".into()),
            role: Some("page-writer".into()),
            mode: None,
        })
    );
    assert_eq!(
        spawn(json!({"task": "Write page.md", "role": "  "})),
        Ok(Call::Spawn {
            task: "Write page.md".into(),
            harness: None,
            role: None,
            mode: None,
        })
    );
    for role in ["@", "???", &"r".repeat(65)] {
        assert_eq!(
            spawn(json!({"task": "Write page.md", "role": role})),
            Err(ROLE_INVALID),
            "{role}"
        );
    }
    assert_eq!(spawn(json!({"task": "   "})), Err(SPAWN_INVALID));
    assert_eq!(
        spawn(json!({"task": "x".repeat(TASK_PROMPT_BYTES_MAX + 1)})),
        Err(SPAWN_INVALID)
    );
    let update = |arguments: Value| live_call(UPDATE_WORK_TOOL, Some(&arguments)).unwrap();
    assert_eq!(
        update(json!({"to": " @page-writer ", "text": "Use the old header.\n"})),
        Ok(Call::UpdateWork {
            kind: Update::Message,
            text: "Use the old header.".into(),
            to: Some("@page-writer".into()),
            question_for: None,
            severity: None,
            reply_to: None,
            work: None,
            key: None,
        })
    );
    assert_eq!(
        update(json!({"for": "me", "severity": "high", "text": "May Ada force-push main?"})),
        Ok(Call::UpdateWork {
            kind: Update::Question,
            text: "May Ada force-push main?".into(),
            to: None,
            question_for: Some(QuestionFor::Me),
            severity: Some(Severity::High),
            reply_to: None,
            work: None,
            key: None,
        })
    );
    assert_eq!(
        update(json!({"reply_to": 7, "text": "Yes."})),
        Ok(Call::UpdateWork {
            kind: Update::Reply,
            text: "Yes.".into(),
            to: None,
            question_for: None,
            severity: None,
            reply_to: Some(7),
            work: None,
            key: None,
        })
    );
    for (arguments, refused) in [
        (json!({"to": "@a", "text": "bell \u{7}"}), UPDATE_INVALID),
        (json!({"to": "a".repeat(65), "text": "hi"}), UPDATE_INVALID),
        (json!({"text": "hi"}), MESSAGE_INVALID),
        (json!({"for": "me", "text": "hi"}), QUESTION_INVALID),
        (
            json!({"for": "me", "severity": "low", "to": "Ada", "text": "hi"}),
            QUESTION_INVALID,
        ),
        (
            json!({"reply_to": 3, "to": "Ada", "text": "hi"}),
            REPLY_INVALID,
        ),
        (
            json!({"to": "Ada", "key": "a b", "text": "hi"}),
            KEY_INVALID,
        ),
    ] {
        assert_eq!(update(arguments.clone()), Err(refused), "{arguments}");
    }
    assert_eq!(
        live_call(
            START_WORK_TOOL,
            Some(&json!({"title": " Fix the footer ", "claims": ["src/page.md"]}))
        )
        .unwrap(),
        Ok(Call::StartWork {
            title: "Fix the footer".into(),
            claims: vec!["src/page.md".into()],
        })
    );
    assert_eq!(
        live_call(START_WORK_TOOL, Some(&json!({"title": "a\nb"}))).unwrap(),
        Err(START_INVALID)
    );
    assert_eq!(
        live_call(
            TRANSFER_WORK_TOOL,
            Some(&json!({"work": 2, "version": 3, "action": "assign"}))
        )
        .unwrap(),
        Err(TRANSFER_INVALID)
    );
    assert_eq!(
        live_call(
            FINISH_WORK_TOOL,
            Some(
                &json!({"work": 2, "version": 3, "outcome": "complete", "evidence": "Tests pass."})
            )
        )
        .unwrap(),
        Ok(Call::FinishWork {
            work: 2,
            version: 3,
            outcome: crate::coord::Finish::Complete,
            evidence: "Tests pass.".into(),
        })
    );
    assert_eq!(
        live_call(STOP_TOOL, Some(&json!({"agent": " Ada "}))).unwrap(),
        Ok(Call::Stop {
            agent: "Ada".into()
        })
    );
    assert_eq!(
        live_call(MERGE_TOOL, Some(&json!({}))).unwrap(),
        Err(AGENT_INVALID)
    );
    assert_eq!(
        live_call(INSPECT_WORK_TOOL, None).unwrap(),
        Ok(Call::InspectWork)
    );
    assert_eq!(live_call("shell", None), None);
    assert_eq!(live_call(TOOL_NAME, None), None);
}

#[test]
fn the_engine_checks_again_every_call_read_from_a_mailbox() {
    assert_eq!(checked(Call::InspectWork), Ok(Call::InspectWork));
    let planted = Call::Spawn {
        task: "Write page.md".into(),
        harness: Some("Codex".into()),
        role: None,
        mode: None,
    };
    assert_eq!(checked(planted), Err(CALL_INVALID));
    let planted = Call::UpdateWork {
        kind: Update::Message,
        text: "x".repeat(TASK_PROMPT_BYTES_MAX + 1),
        to: Some("@a".into()),
        question_for: None,
        severity: None,
        reply_to: None,
        work: None,
        key: None,
    };
    assert!(checked(planted).is_err());
    let planted = Call::Spawn {
        task: "Write page.md".into(),
        harness: None,
        role: Some("Page".into()),
        mode: None,
    };
    assert_eq!(checked(planted), Err(CALL_INVALID));
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
        b"{\"tool\":\"inspect_work\"}\n",
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
    std::fs::write(mailbox.join("notes.call"), b"{\"tool\":\"inspect_work\"}").unwrap();
    std::fs::write(
        mailbox.join(format!("{}{CALL_SUFFIX}", "3".repeat(32))),
        vec![b' '; CALL_BYTES_MAX as usize + 1],
    )
    .unwrap();

    let taken = take_calls(&mailbox, CALLS_PER_TICK_MAX);
    assert_eq!(taken.len(), 3, "{taken:?}");
    assert_eq!(
        taken[0],
        (
            first.clone(),
            Some(Call::InspectWork),
            Some(call_digest(b"{\"tool\":\"inspect_work\"}\n"))
        )
    );
    assert_eq!(taken[1], (second.clone(), None, None));
    assert_eq!(taken[2], ("3".repeat(32), None, None));
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
    assert!(
        !mailbox
            .join(format!("{first}{ANSWER_PART_SUFFIX}"))
            .exists()
    );
    let long = "é".repeat(REPLY_TEXT_MAX);
    answer_call(&mailbox, &second, &long, true).unwrap();
    let (text, error) = read_reply(&mailbox.join(format!("{second}{REPLY_SUFFIX}"))).unwrap();
    assert!(error && text.len() <= REPLY_TEXT_MAX && text.chars().all(|c| c == 'é'));

    let planted = "4".repeat(32);
    symlink(
        &secret,
        mailbox.join(format!("{planted}{ANSWER_PART_SUFFIX}")),
    )
    .unwrap();
    assert!(answer_call(&mailbox, &planted, "never", false).is_err());
    assert_eq!(std::fs::read(&secret).unwrap(), b"{\"tool\":\"changes\"}");
}

#[test]
fn the_engine_follows_no_link_swapped_in_for_the_mailbox_or_the_folder_holding_it() {
    let directory = tempfile::tempdir().unwrap();
    let base = directory.path().canonicalize().unwrap();
    let id = "5".repeat(32);
    let stale = "6".repeat(32);
    let old = std::time::SystemTime::now() - REPLY_STALE - std::time::Duration::from_secs(60);
    let outside = |name: &str| {
        let folder = base.join(name);
        std::fs::create_dir(&folder).unwrap();
        std::fs::write(
            folder.join(format!("{id}{CALL_SUFFIX}")),
            b"{\"tool\":\"inspect_work\"}\n",
        )
        .unwrap();
        let reply = folder.join(format!("{stale}{REPLY_SUFFIX}"));
        std::fs::write(&reply, b"{}").unwrap();
        std::fs::File::options()
            .write(true)
            .open(&reply)
            .unwrap()
            .set_modified(old)
            .unwrap();
        folder
    };
    let untouched = |folder: &Path| {
        let mut names: Vec<String> = std::fs::read_dir(folder)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        assert_eq!(
            names,
            [
                format!("{id}{CALL_SUFFIX}"),
                format!("{stale}{REPLY_SUFFIX}")
            ]
        );
    };
    let tmp = base.join("agent").join("tmp");
    std::fs::create_dir_all(&tmp).unwrap();
    let mailbox = tmp.join(MAILBOX_FOLDER);

    let swapped = outside("swapped-mailbox");
    symlink(&swapped, &mailbox).unwrap();
    assert!(take_calls(&mailbox, CALLS_PER_TICK_MAX).is_empty());
    assert!(answer_call(&mailbox, &id, "Taken.", false).is_err());
    untouched(&swapped);

    std::fs::remove_file(&mailbox).unwrap();
    std::fs::remove_dir(&tmp).unwrap();
    let held = base.join("swapped-tmp");
    std::fs::create_dir(&held).unwrap();
    let inner = outside(format!("swapped-tmp/{MAILBOX_FOLDER}").as_str());
    symlink(&held, &tmp).unwrap();
    assert!(take_calls(&mailbox, CALLS_PER_TICK_MAX).is_empty());
    assert!(answer_call(&mailbox, &id, "Taken.", false).is_err());
    untouched(&inner);

    std::fs::remove_file(&tmp).unwrap();
    std::fs::create_dir(&tmp).unwrap();
    std::fs::rename(&inner, &mailbox).unwrap();
    assert_eq!(
        take_calls(&mailbox, CALLS_PER_TICK_MAX),
        vec![(
            id.clone(),
            Some(Call::InspectWork),
            Some(call_digest(b"{\"tool\":\"inspect_work\"}\n"))
        )]
    );
    assert!(
        !mailbox.join(format!("{stale}{REPLY_SUFFIX}")).exists(),
        "the real mailbox's stale answer is removed"
    );
    answer_call(&mailbox, &id, "Taken.", false).unwrap();
    assert_eq!(
        read_reply(&mailbox.join(format!("{id}{REPLY_SUFFIX}"))),
        Some(("Taken.".into(), false))
    );
}

#[test]
fn the_engine_never_takes_a_call_its_server_is_still_writing() {
    let directory = tempfile::tempdir().unwrap();
    let mailbox = directory
        .path()
        .canonicalize()
        .unwrap()
        .join(MAILBOX_FOLDER);
    std::fs::create_dir(&mailbox).unwrap();
    let askers: Vec<_> = (0..8)
        .map(|_| {
            let mailbox = mailbox.clone();
            std::thread::spawn(move || {
                (0..16)
                    .map(|_| ask(&mailbox, &Call::InspectWork, REPLY_WAIT))
                    .collect::<Vec<_>>()
            })
        })
        .collect();
    let mut unread = 0;
    while askers.iter().any(|asker| !asker.is_finished()) {
        for (id, call, _) in take_calls(&mailbox, CALLS_PER_TICK_MAX) {
            unread += usize::from(call.is_none());
            answer_call(&mailbox, &id, "Taken.", call.is_none()).unwrap();
        }
    }
    let answers: Vec<(String, bool)> = askers
        .into_iter()
        .flat_map(|asker| asker.join().unwrap())
        .collect();
    assert_eq!(unread, 0, "calls were taken before they were written whole");
    assert_eq!(answers.len(), 8 * 16);
    assert!(
        answers
            .iter()
            .all(|answer| *answer == ("Taken.".to_owned(), false)),
        "{answers:?}"
    );
    assert_eq!(std::fs::read_dir(&mailbox).unwrap().count(), 0);
}

#[test]
fn the_live_server_needs_an_absolute_plain_mailbox_and_lists_the_tools_left_on() {
    let directory = tempfile::tempdir().unwrap();
    let base = directory.path().canonicalize().unwrap();
    let list = requests(&[json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"})]);
    assert!(serve_live(list.clone(), Vec::new(), Path::new("relative"), &[]).is_err());
    let linked = base.join("linked");
    symlink(&base, &linked).unwrap();
    assert!(serve_live(list.clone(), Vec::new(), &linked, &[]).is_err());
    let mut output = Vec::new();
    serve_live(list, &mut output, &base, &[STOP_TOOL.to_owned()]).unwrap();
    let answer: Value = serde_json::from_slice(output.trim_ascii()).unwrap();
    let names: Vec<&str> = answer["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
    let left_on: Vec<&str> = TEAM_TOOLS
        .into_iter()
        .filter(|name| *name != STOP_TOOL)
        .chain(QUESTION_TOOLS)
        .collect();
    assert_eq!(names, left_on);
    let tools = answer["result"]["tools"].as_array().unwrap();
    let ask = left_on.len() - QUESTION_TOOLS.len();
    assert_eq!(tools[ask], ask_tool());
    assert_eq!(tools[ask + 1], record_tool());
    assert_eq!(tools[ask + 2], withdraw_tool());
    assert_eq!(
        tools[ask]["inputSchema"]["properties"]["options"]["maxItems"],
        crate::questions::OPTIONS_MAX
    );
}

#[test]
fn the_engine_runs_only_a_call_it_claimed_and_a_claimed_call_is_never_reported_untaken() {
    let directory = tempfile::tempdir().unwrap();
    let mailbox = directory.path().canonicalize().unwrap();
    let blocked = "c".repeat(32);
    std::fs::write(
        mailbox.join(format!("{blocked}{CALL_SUFFIX}")),
        br#"{"tool":"inspect_work"}"#,
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
            &Call::InspectWork,
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

#[test]
fn ask_takes_a_bounded_question_and_record_answer_only_numbers_and_the_engine_checks_both_again() {
    let ask = |arguments: Value| live_call(ASK_TOOL, Some(&arguments)).unwrap();
    let record = |arguments: Value| live_call(RECORD_TOOL, Some(&arguments)).unwrap();
    let asked = Call::Ask {
        question: "Ship V2 on Friday?".into(),
        kind: crate::questions::QuestionKind::Single,
        options: vec!["Yes".into(), "No, wait a week".into()],
        recommended: Some("Yes".into()),
        context: Some("One flaky test left.".into()),
        replaces: None,
    };
    assert_eq!(
        ask(json!({
            "question": " Ship V2 on Friday? ",
            "options": ["Yes", " No, wait a week "],
            "recommended": "Yes",
            "context": "One flaky test left.",
        })),
        Ok(asked.clone())
    );
    assert_eq!(checked(asked.clone()), Ok(asked));
    let six: Vec<String> = (1..=6).map(|n| format!("option {n}")).collect();
    let widest =
        ask(json!({"question": "Pick one?", "options": six.clone(), "recommended": "option 1"}));
    assert!(matches!(widest, Ok(Call::Ask { ref options, .. }) if options == &six));
    let bare = Call::Ask {
        question: "Which name?".into(),
        kind: crate::questions::QuestionKind::Text,
        options: vec![],
        recommended: Some("CrazyAgent, it is known.".into()),
        context: None,
        replaces: None,
    };
    assert_eq!(
        ask(
            json!({"question": "Which name?", "options": null, "recommended": "CrazyAgent, it is known.", "context": null})
        ),
        Ok(bare.clone())
    );
    assert_eq!(checked(bare.clone()), Ok(bare));
    let recorded = Call::RecordAnswer {
        question: 7,
        version: 0,
        owner_turn: 31,
    };
    assert_eq!(
        record(json!({"question": 7, "version": 0, "owner_turn": 31})),
        Ok(recorded.clone())
    );
    assert_eq!(checked(recorded.clone()), Ok(recorded));

    for arguments in [
        json!({}),
        json!({"question": "  "}),
        json!({"question": "x".repeat(crate::questions::QUESTION_BYTES_MAX + 1)}),
        json!({"question": "Pick?", "options": ["a", "b", "c", "d", "e", "f", "g"]}),
        json!({"question": "Pick?", "options": ["a", "a"]}),
        json!({"question": "Pick?", "options": ["two\nlines"]}),
        json!({"question": "Pick?", "options": [1]}),
        json!({"question": "Pick?", "options": "a"}),
        json!({"question": "Pick?", "recommended": 3}),
        json!({"question": "Pick?", "options": ["a", "b"], "recommended": "c"}),
        json!({"question": "Pick?", "options": ["a"], "recommended": "a"}),
        json!({"question": "Pick?", "options": ["a", "b"]}),
        json!({"question": "Pick?", "recommended": "r".repeat(crate::questions::RECOMMENDED_BYTES_MAX + 1)}),
        json!({"question": "Pick \u{202e}one?"}),
        json!({"question": "Pick?", "context": "zero\u{200b}width"}),
        json!({"question": "Pick?", "answer": "yes"}),
        json!({"question": "Pick?", "shell": "rm -rf /"}),
        json!({"id": 7, "answer": "yes"}),
    ] {
        assert_eq!(ask(arguments.clone()), Err(ASK_INVALID), "{arguments}");
    }
    for rule in [
        "2 to 6 distinct one-line options",
        "one of them as recommended",
    ] {
        assert!(
            ASK_INVALID.contains(rule),
            "the refusal names the rule: {rule}"
        );
    }
    for arguments in [
        json!({}),
        json!({"question": 7, "version": 0}),
        json!({"question": 0, "version": 0, "owner_turn": 31}),
        json!({"question": 7, "version": -1, "owner_turn": 31}),
        json!({"question": 7, "version": 0, "owner_turn": 0}),
        json!({"question": "7", "version": 0, "owner_turn": 31}),
        json!({"question": 7, "version": 0, "owner_turn": 31, "answer": "Friday works."}),
        json!({"question": 7, "version": 0, "owner_turn": u64::MAX}),
    ] {
        assert_eq!(
            record(arguments.clone()),
            Err(RECORD_INVALID),
            "{arguments}"
        );
    }
    let planted = Call::Ask {
        question: " untrimmed ".into(),
        kind: crate::questions::QuestionKind::Text,
        options: vec![],
        recommended: None,
        context: None,
        replaces: None,
    };
    assert!(checked(planted).is_err());
    let planted = Call::RecordAnswer {
        question: 7,
        version: 0,
        owner_turn: -1,
    };
    assert_eq!(checked(planted), Err(RECORD_INVALID));
}

/// F12, F14: `record_answer` takes either an owner message number or an answer typed in the
/// master's terminal, never both; `withdraw_question` takes a question and its version; `ask`
/// may name the old question it replaces; the engine reads each the same way again.
#[test]
fn record_answer_takes_a_terminal_answer_withdraw_takes_two_numbers_and_ask_names_what_it_replaces()
{
    let record = |arguments: Value| live_call(RECORD_TOOL, Some(&arguments)).unwrap();
    let withdraw = |arguments: Value| live_call(WITHDRAW_TOOL, Some(&arguments)).unwrap();
    let ask = |arguments: Value| live_call(ASK_TOOL, Some(&arguments)).unwrap();
    let relayed = Call::RelayAnswer {
        question: 7,
        version: 2,
        answer: "Friday works.".into(),
    };
    assert_eq!(
        record(json!({"question": 7, "version": 2, "answer": "  Friday works. "})),
        Ok(relayed.clone())
    );
    assert_eq!(checked(relayed.clone()), Ok(relayed));
    for arguments in [
        json!({"question": 7, "version": 0, "answer": "   "}),
        json!({"question": 7, "version": 0, "answer": 5}),
        json!({"question": 7, "answer": "Yes"}),
        json!({"question": 7, "version": 0, "answer": "y".repeat(crate::questions::ANSWER_BYTES_MAX + 1)}),
    ] {
        assert_eq!(
            record(arguments.clone()),
            Err(RECORD_INVALID),
            "{arguments}"
        );
    }
    assert_eq!(
        checked(Call::RelayAnswer {
            question: 7,
            version: 0,
            answer: " padded ".into(),
        }),
        Err(CALL_INVALID)
    );

    let withdrawn = Call::Withdraw {
        question: 7,
        version: 1,
    };
    assert_eq!(
        withdraw(json!({"question": 7, "version": 1})),
        Ok(withdrawn.clone())
    );
    assert_eq!(checked(withdrawn.clone()), Ok(withdrawn.clone()));
    assert_eq!(withdrawn.name(), WITHDRAW_TOOL);
    for arguments in [
        json!({}),
        json!({"question": 7}),
        json!({"question": 0, "version": 0}),
        json!({"question": 7, "version": -1}),
        json!({"question": 7, "version": 0, "reason": "done"}),
    ] {
        assert_eq!(
            withdraw(arguments.clone()),
            Err(WITHDRAW_INVALID),
            "{arguments}"
        );
    }

    let again = ask(
        json!({"question": "Pick?", "kind": "single", "options": ["a", "b"], "recommended": "a", "replaces": 9}),
    );
    assert!(
        matches!(
            &again,
            Ok(Call::Ask {
                replaces: Some(9),
                ..
            })
        ),
        "{again:?}"
    );
    let again = again.unwrap();
    assert_eq!(checked(again.clone()), Ok(again));
    assert!(matches!(
        ask(json!({"question": "Pick?", "replaces": null})),
        Ok(Call::Ask { replaces: None, .. })
    ));
    for replaces in [json!(0), json!(-3), json!("9"), json!(1.5)] {
        assert_eq!(
            ask(json!({"question": "Pick?", "replaces": replaces.clone()})),
            Err(ASK_INVALID),
            "{replaces}"
        );
    }
}

/// S1: `sessions` takes plain words up to `QUERY_BYTES_MAX` bytes or nothing; `session` takes
/// a plain id and a message number given as a number, as text or left out, and the engine
/// reads both the same way again from a mailbox.
#[test]
fn past_conversation_calls_keep_plain_ids_bounded_words_and_any_offset_form() {
    let sessions = |arguments: Value| live_call(SESSIONS_TOOL, Some(&arguments));
    assert_eq!(
        sessions(json!({"query": "  zebra plan "})),
        Some(Ok(Call::Sessions {
            query: "zebra plan".into()
        }))
    );
    assert_eq!(
        live_call(SESSIONS_TOOL, None),
        Some(Ok(Call::Sessions {
            query: String::new()
        }))
    );
    assert_eq!(
        sessions(json!({"query": "x".repeat(super::super::history::QUERY_BYTES_MAX)})),
        Some(Ok(Call::Sessions {
            query: "x".repeat(super::super::history::QUERY_BYTES_MAX)
        }))
    );
    for refused in [
        json!({"query": "x".repeat(super::super::history::QUERY_BYTES_MAX + 1)}),
        json!({"query": "zebra\u{7}"}),
        json!({"query": "line\nbreak"}),
    ] {
        assert_eq!(sessions(refused), Some(Err(SESSIONS_INVALID)));
    }

    let session = |arguments: Value| live_call(SESSION_TOOL, Some(&arguments));
    let read = |offset| {
        Some(Ok(Call::Session {
            id: "agent-3/rollout-1".into(),
            offset,
        }))
    };
    assert_eq!(
        session(json!({"id": " agent-3/rollout-1 ", "offset": 7})),
        read(7)
    );
    assert_eq!(
        session(json!({"id": "agent-3/rollout-1", "offset": " 12 "})),
        read(12)
    );
    assert_eq!(session(json!({"id": "agent-3/rollout-1"})), read(0));
    assert_eq!(
        session(json!({"id": "agent-3/rollout-1", "offset": null})),
        read(0)
    );
    assert_eq!(
        session(json!({"id": "agent-3/rollout-1", "offset": super::super::history::OFFSET_MAX})),
        read(super::super::history::OFFSET_MAX)
    );
    for refused in [
        json!({"id": "agent-3/../../etc/passwd"}),
        json!({"id": "x".repeat(super::super::history::ID_BYTES_MAX + 1)}),
        json!({"id": "agent-3/roll\u{1b}out"}),
        json!({"id": "agent 3"}),
        json!({"offset": 1}),
        json!({"id": "agent-3/rollout-1", "offset": -1}),
        json!({"id": "agent-3/rollout-1", "offset": "seven"}),
        json!({"id": "agent-3/rollout-1", "offset": super::super::history::OFFSET_MAX + 1}),
    ] {
        assert_eq!(
            session(refused.clone()),
            Some(Err(SESSION_INVALID)),
            "{refused}"
        );
    }

    for call in [
        Call::Sessions {
            query: "zebra plan".into(),
        },
        Call::Session {
            id: "session/abc".into(),
            offset: 40,
        },
    ] {
        assert_eq!(checked(call.clone()), Ok(call));
    }
    assert_eq!(
        checked(Call::Session {
            id: "agent-3/../x".into(),
            offset: 0,
        }),
        Err(SESSION_INVALID)
    );
    assert_eq!(
        checked(Call::Sessions {
            query: "a\u{0}b".into()
        }),
        Err(SESSIONS_INVALID)
    );
}

#[test]
fn a_spawn_names_its_lead_s_mode_only_as_native_or_app_spawned() {
    use crate::coord::settings::SubagentMode;
    let spawn = |arguments: Value| live_call(SPAWN_TOOL, Some(&arguments)).unwrap();
    let mode = |given: Value| match spawn(json!({"task": "Lead the docs", "mode": given})) {
        Ok(Call::Spawn { mode, .. }) => Ok(mode),
        Ok(other) => panic!("not a spawn: {other:?}"),
        Err(refused) => Err(refused),
    };
    assert_eq!(mode(json!("native")), Ok(Some(SubagentMode::Native)));
    assert_eq!(
        mode(json!("app_spawned")),
        Ok(Some(SubagentMode::AppSpawned))
    );
    assert_eq!(
        mode(json!(" App-Spawned ")),
        Ok(Some(SubagentMode::AppSpawned))
    );
    assert_eq!(mode(json!("")), Ok(None));
    assert_eq!(mode(Value::Null), Ok(None));
    assert_eq!(
        spawn(json!({"task": "Lead the docs"})),
        Ok(Call::Spawn {
            task: "Lead the docs".into(),
            harness: None,
            role: None,
            mode: None,
        })
    );
    for refused in [json!("teams"), json!(3), json!(["native"])] {
        assert_eq!(mode(refused), Err(MODE_INVALID));
    }
    let planted = Call::Spawn {
        task: "Lead the docs".into(),
        harness: Some("claude-code".into()),
        role: None,
        mode: Some(SubagentMode::Native),
    };
    assert_eq!(checked(planted.clone()), Ok(planted));
    let line =
        br#"{"tool":"spawn","task":"Lead the docs","harness":null,"role":null,"mode":"teams"}"#;
    assert_eq!(read_call(line), None);
    let schema = &spec(SPAWN_TOOL)["inputSchema"]["properties"]["mode"];
    assert_eq!(schema["enum"], json!(["native", "app_spawned"]));
    assert!(
        schema["description"]
            .as_str()
            .unwrap()
            .starts_with("Master only, fixed for the lead's life")
    );
}

#[test]
fn update_work_says_a_reply_answers_a_question_or_work_and_a_note_takes_none() {
    let tool = spec(UPDATE_WORK_TOOL);
    assert!(
        tool["description"]
            .as_str()
            .unwrap()
            .contains("reply answers a question or a message (work for you) whose number is in reply_to. A note takes no reply: to answer one, send kind note with to its sender."),
        "{tool}"
    );
    let properties = &tool["inputSchema"]["properties"];
    assert_eq!(
        properties["kind"]["description"],
        "message when left out, question with for, reply with reply_to to a question or work for you, never to a note."
    );
    assert_eq!(
        properties["reply_to"]["description"],
        "The question or message (work for you) this answers or passes on, or the message a note or message refers to."
    );
}

/// A call file carries the call alone, and the engine reads back the digest of its exact
/// bytes; a pane any writer planted beside a call is dropped unread (DEC-111 Q18).
#[test]
fn a_call_carries_no_pane_and_a_planted_pane_is_dropped_unread() {
    let directory = tempfile::tempdir().unwrap();
    let mailbox = directory.path().canonicalize().unwrap();
    let asked = mailbox.clone();
    let asker = std::thread::spawn(move || ask(&asked, &Call::InspectWork, REPLY_WAIT));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let id = loop {
        if let Some((_, id)) = calls(&mailbox, CALL_SUFFIX).pop() {
            break id;
        }
        assert!(std::time::Instant::now() < deadline, "no call was written");
        std::thread::sleep(std::time::Duration::from_millis(10));
    };
    let bytes = std::fs::read(mailbox.join(format!("{id}{CALL_SUFFIX}"))).unwrap();
    let written: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(written, json!({"tool": "inspect_work"}));
    assert_eq!(
        take_calls(&mailbox, CALLS_PER_TICK_MAX),
        vec![(
            id.clone(),
            Some(Call::InspectWork),
            Some(call_digest(&bytes))
        )]
    );
    answer_call(&mailbox, &id, "Taken.", false).unwrap();
    assert_eq!(asker.join().unwrap(), ("Taken.".to_owned(), false));

    let planted = [
        br#"{"tool":"inspect_work"}"#.as_slice(),
        br#"{"tool":"inspect_work","pane":3}"#.as_slice(),
        br#"{"tool":"inspect_work","pane":"%x1"}"#.as_slice(),
        br#"{"tool":"inspect_work","pane":null}"#.as_slice(),
        br#"{"tool":"inspect_work","pane":"%1;rm"}"#.as_slice(),
    ];
    for (index, bytes) in planted.into_iter().enumerate() {
        let id = format!("{index}").repeat(32);
        std::fs::write(mailbox.join(format!("{id}{CALL_SUFFIX}")), bytes).unwrap();
        assert_eq!(
            take_calls(&mailbox, CALLS_PER_TICK_MAX),
            vec![(id, Some(Call::InspectWork), Some(call_digest(bytes)))],
            "{}",
            String::from_utf8_lossy(bytes)
        );
    }
    assert_ne!(
        call_digest(br#"{"tool":"inspect_work"}"#),
        call_digest(br#"{"tool":"inspect_work","pane":"%7"}"#)
    );
    assert_eq!(
        read_call(br#"{"tool":"inspect_work","pane":"%7"}"#),
        Some(Call::InspectWork)
    );
}

#[test]
fn a_switched_off_tool_is_refused_by_its_server_and_never_reaches_the_mailbox() {
    let directory = tempfile::tempdir().unwrap();
    let mailbox = directory.path().canonicalize().unwrap();
    let input = requests(&[json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/call",
        "params": {"name": STOP_TOOL, "arguments": {"agent": "3"}}
    })]);
    let mut output = Vec::new();
    serve_live(input, &mut output, &mailbox, &[STOP_TOOL.to_owned()]).unwrap();
    let answer: Value = serde_json::from_slice(output.trim_ascii()).unwrap();
    assert_eq!(answer["result"]["isError"], json!(true), "{answer}");
    assert_eq!(
        answer["result"]["content"][0]["text"],
        json!(SWITCHED_OFF),
        "{answer}"
    );
    assert_eq!(std::fs::read_dir(&mailbox).unwrap().count(), 0);
}

#[test]
fn only_known_team_tools_up_to_their_count_may_be_switched_off() {
    let names =
        |list: &[&str]| -> Vec<String> { list.iter().map(|name| (*name).to_owned()).collect() };
    assert!(known_tools(&[]));
    assert!(known_tools(&names(&TEAM_TOOLS)));
    assert!(known_tools(&names(&[STOP_TOOL, MERGE_TOOL])));
    assert!(!known_tools(&names(&["status"])));
    assert!(!known_tools(&names(&[STOP_TOOL, "Stop"])));
    assert!(known_tools(&names(&QUESTION_TOOLS)));
    assert!(known_tools(&names(&[STOP_TOOL; 15])));
    assert!(!known_tools(&names(&[STOP_TOOL; 16])));
}
/// The words a tool name, description or schema field of the owner's Messages timeline read
/// or its settings write would carry, lowercased.
const OWNER_ONLY: [&str; 12] = [
    "team_agent_messages",
    "teamagentmessages",
    "agent_messages",
    "agentmessages",
    "message_settings",
    "messagesettings",
    "timeline",
    "keep_master",
    "kept_per_project",
    "progress_shown",
    "owner_items",
    "page_size",
];

/// The names a master could try for a timeline read or a settings write.
const OWNER_ONLY_CALLS: [&str; 7] = [
    "team_agent_messages",
    "TeamAgentMessages",
    "agent_messages",
    "message_settings",
    "MessageSettings",
    "timeline",
    "messages",
];

/// MSG-SEC-1: the agents' Messages timeline is the owner's alone. Neither team tool server, the
/// `delegate` server a master gets nor the live master's, lists a tool, a description word or
/// a schema field of a timeline read or a settings write; a call by any such name is an
/// unknown tool to both, the live call reader takes none of them, so nothing reaches the
/// mailbox, and a call planted in the mailbox under such a name is never read as a call.
#[test]
fn neither_team_tool_server_offers_the_owner_s_messages_timeline_or_its_settings() {
    let directory = tempfile::tempdir().unwrap();
    let base = directory.path().canonicalize().unwrap();
    let drop = base.join(DROP_FILE);
    let mailbox = base.join(MAILBOX_FOLDER);
    std::fs::create_dir(&mailbox).unwrap();
    let arguments = json!({"project_id": "p", "agent": "codex-1", "text": "Show me", "settings": {"page_size": 25}});
    let mut lines = vec![json!({"jsonrpc": "2.0", "id": 0, "method": "tools/list", "params": {}})];
    for (id, name) in (1u64..).zip(OWNER_ONLY_CALLS) {
        lines.push(json!({"jsonrpc": "2.0", "id": id, "method": "tools/call",
            "params": {"name": name, "arguments": arguments}}));
    }
    let delegate = by_id(&served(requests(&lines), &drop));
    let mut output = Vec::new();
    serve_live(requests(&lines), &mut output, &mailbox, &[]).unwrap();
    let live: Vec<Value> = output
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_slice(line).unwrap())
        .collect();
    let live = by_id(&live);
    for (server, answers) in [("delegate", &delegate), ("live", &live)] {
        let tools = &answers["0"]["result"]["tools"];
        assert!(!tools.as_array().unwrap().is_empty(), "{server}");
        let listed = tools.to_string().to_lowercase();
        for word in OWNER_ONLY {
            assert!(!listed.contains(word), "{server} lists {word}: {listed}");
        }
        for id in 1..=OWNER_ONLY_CALLS.len() {
            assert_eq!(
                answers[&id.to_string()]["error"]["code"],
                -32602,
                "{server} answers {} as a tool",
                OWNER_ONLY_CALLS[id - 1]
            );
        }
    }
    for name in OWNER_ONLY_CALLS {
        assert!(live_call(name, Some(&arguments)).is_none(), "{name}");
    }
    assert!(
        std::fs::read_dir(&mailbox).unwrap().next().is_none(),
        "no timeline or settings call reached the engine"
    );
    for (number, name) in OWNER_ONLY_CALLS.iter().enumerate() {
        std::fs::write(
            mailbox.join(format!("{number:032x}.call")),
            json!({"tool": name, "agent": "codex-1", "settings": {"page_size": 25}}).to_string(),
        )
        .unwrap();
    }
    let taken = take_calls(&mailbox, CALLS_PER_TICK_MAX);
    assert_eq!(taken.len(), OWNER_ONLY_CALLS.len());
    assert!(
        taken.iter().all(|(_, call, _)| call.is_none()),
        "a planted timeline or settings call was read as a call: {taken:?}"
    );
}

/// `log_activity` (OS-5) is listed with its goal, line and kind, left out when the owner
/// switched it off, and keeps a call only with a goal of 1 to 120 bytes, one line of 1 to
/// 600 bytes and a kind of phase or test, refusing anything else with a plain sentence.
#[test]
fn log_activity_is_listed_unless_switched_off_and_keeps_only_a_bounded_call() {
    let directory = tempfile::tempdir().unwrap();
    let base = directory.path().canonicalize().unwrap();
    let listed = |off: &[String]| -> Vec<Value> {
        let list = requests(&[json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"})]);
        let mut output = Vec::new();
        serve_live(list, &mut output, &base, off).unwrap();
        let answer: Value = serde_json::from_slice(output.trim_ascii()).unwrap();
        answer["result"]["tools"].as_array().unwrap().clone()
    };
    let on = listed(&[]);
    let entry = on.iter().find(|tool| tool["name"] == "log_activity");
    assert!(entry.is_some(), "log_activity is listed while it is on");
    let schema = &entry.unwrap()["inputSchema"];
    assert_eq!(schema["required"], json!(["goal", "text", "kind"]));
    assert_eq!(
        schema["properties"]["kind"]["enum"],
        json!(["phase", "test"])
    );
    assert_eq!(schema["additionalProperties"], json!(false));
    let off = listed(&["log_activity".to_owned()]);
    assert!(off.iter().all(|tool| tool["name"] != "log_activity"));
    assert_eq!(off.len() + 1, on.len());

    let call = |goal: &str, text: &str, kind: &str| {
        live_call(
            "log_activity",
            Some(&json!({"goal": goal, "text": text, "kind": kind})),
        )
    };
    let kept = call(&"g".repeat(120), &"t".repeat(600), "test");
    assert!(
        matches!(kept, Some(Ok(_))),
        "a goal of 120 bytes and a line of 600 are kept: {kept:?}"
    );
    let kept = kept.unwrap().unwrap();
    assert_eq!(kept.name(), "log_activity");
    assert_eq!(checked(kept.clone()), Ok(kept));
    let long_goal = "g".repeat(121);
    let long_text = "t".repeat(601);
    for (goal, text, kind) in [
        ("", "Started the build", "phase"),
        ("   ", "Started the build", "phase"),
        (long_goal.as_str(), "Started the build", "phase"),
        ("Build the service", "", "phase"),
        ("Build the service", long_text.as_str(), "phase"),
        ("Build the service", "Started\nthe build", "phase"),
        ("Build\nthe service", "Started the build", "phase"),
        ("Build the service", "Started the build", "review"),
    ] {
        let refused = call(goal, text, kind);
        assert!(
            matches!(refused, Some(Err(_))),
            "{goal:?} {text:?} {kind:?} is refused: {refused:?}"
        );
    }
    let no_kind = live_call(
        "log_activity",
        Some(&json!({"goal": "Build the service", "text": "Started the build"})),
    );
    assert!(matches!(no_kind, Some(Err(_))), "{no_kind:?}");
}

/// SECAUDIT coord-delivery-raw-body-forges-owner-line r2: a work title reaches other agents
/// in the engine's overlap note, so one holding any of the seven line breaks is refused.
#[test]
fn a_work_title_holding_any_line_break_is_refused_by_name() {
    for line_break in [
        '\n', '\r', '\u{0b}', '\u{0c}', '\u{85}', '\u{2028}', '\u{2029}',
    ] {
        assert_eq!(
            live_call(
                START_WORK_TOOL,
                Some(&json!({"title": format!("a{line_break}b"), "claims": ["src/page.md"]}))
            )
            .unwrap(),
            Err(START_INVALID),
            "{line_break:?}"
        );
    }
}

#[test]
fn an_ask_call_with_a_multi_kind_reads_as_a_multi_question() {
    let call = ask_call(Some(&json!({
        "question": "Which platforms ship in V1?", "kind": "multi",
        "options": ["Web", "Desktop", "Mobile"], "recommended": "Web"
    })));
    assert!(matches!(
        call,
        Some(Call::Ask {
            kind: crate::questions::QuestionKind::Multi,
            ..
        })
    ));
    let call = call.unwrap();
    assert_eq!(
        checked(call.clone()),
        Ok(call),
        "the engine reads the kind back"
    );
}

#[test]
fn an_ask_call_with_options_and_no_recommended_one_is_refused() {
    let call = ask_call(Some(&json!({
        "question": "Which database?", "options": ["Postgres", "SQLite"]
    })));
    assert!(call.is_none());
}

#[test]
fn an_ask_call_names_single_or_text_and_a_kind_that_does_not_fit_is_refused() {
    use crate::questions::QuestionKind;
    let kind_of = |arguments: Value| match ask_call(Some(&arguments)) {
        Some(Call::Ask { kind, .. }) => Some(kind),
        _ => None,
    };
    let options = json!(["Postgres", "SQLite"]);
    assert_eq!(
        kind_of(json!({"question": "Which?", "options": options, "recommended": "SQLite"})),
        Some(QuestionKind::Single),
        "options without a kind read as single"
    );
    assert_eq!(
        kind_of(
            json!({"question": "Which?", "kind": "single", "options": options, "recommended": "SQLite"})
        ),
        Some(QuestionKind::Single)
    );
    assert_eq!(
        kind_of(json!({"question": "Which name?", "kind": "text", "recommended": "Atlas"})),
        Some(QuestionKind::Text)
    );
    assert_eq!(
        kind_of(json!({"question": "Which name?", "kind": null})),
        Some(QuestionKind::Text),
        "no options and no kind read as text"
    );
    for refused in [
        json!({"question": "Which?", "kind": "text", "options": options, "recommended": "SQLite"}),
        json!({"question": "Which?", "kind": "multi"}),
        json!({"question": "Which?", "kind": "single", "options": ["Postgres"], "recommended": "Postgres"}),
        json!({"question": "Which?", "kind": "", "options": options, "recommended": "SQLite"}),
        json!({"question": "Which?", "kind": "choice", "options": options, "recommended": "SQLite"}),
        json!({"question": "Which?", "kind": 1, "options": options, "recommended": "SQLite"}),
    ] {
        assert_eq!(kind_of(refused.clone()), None, "{refused}");
    }
    let schema = ask_tool();
    assert_eq!(
        schema["inputSchema"]["properties"]["kind"]["enum"],
        json!(["single", "multi", "text"])
    );
}
