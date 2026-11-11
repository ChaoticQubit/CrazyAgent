use super::*;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

struct Case {
    root: tempfile::TempDir,
}

impl Case {
    fn new() -> Self {
        let root = tempfile::Builder::new()
            .prefix("ca-pending-")
            .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
            .unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::create_dir(root.path().join("work")).unwrap();
        Self { root }
    }

    fn path(&self) -> PathBuf {
        self.root.path().canonicalize().unwrap()
    }

    fn work(&self) -> PathBuf {
        self.path().join("work")
    }

    fn sessions(&self) -> PathBuf {
        self.path().join("sessions")
    }
}

fn write(path: &Path, lines: &[String]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut body = String::new();
    for line in lines {
        body.push_str(line);
        body.push('\n');
    }
    std::fs::write(path, body).unwrap();
}

fn rollout(sessions: &Path, name: &str, cwd: &Path, lines: &[String]) -> PathBuf {
    let path = sessions.join("2026/10/27").join(name);
    let meta = serde_json::json!({
        "timestamp": "2026-10-27T12:00:00Z",
        "type": "session_meta",
        "payload": {"id": "s", "cwd": cwd.to_string_lossy()}
    });
    let mut all = vec![meta.to_string()];
    all.extend(lines.iter().cloned());
    write(&path, &all);
    path
}

fn command(tool: &str, summary: &str) -> PendingCommand {
    PendingCommand {
        tool: tool.to_owned(),
        summary: summary.to_owned(),
    }
}

fn prompt(text: &str) -> String {
    serde_json::json!({"type": "user", "message": {"role": "user", "content": text}}).to_string()
}

fn tool_use(id: &str, name: &str, input: Value) -> String {
    serde_json::json!({
        "type": "assistant",
        "message": {
            "role": "assistant",
            "content": [{"type": "tool_use", "id": id, "name": name, "input": input}],
            "stop_reason": "tool_use"
        }
    })
    .to_string()
}

fn tool_result(id: &str, error: bool) -> String {
    serde_json::json!({
        "type": "user",
        "message": {
            "role": "user",
            "content": [{"tool_use_id": id, "type": "tool_result", "content": "ok", "is_error": error}]
        }
    })
    .to_string()
}

fn started() -> String {
    r#"{"type":"event_msg","payload":{"type":"task_started"}}"#.to_owned()
}

fn function_call(id: &str, name: &str, arguments: Value) -> String {
    serde_json::json!({
        "type": "response_item",
        "payload": {
            "type": "function_call",
            "name": name,
            "arguments": arguments.to_string(),
            "call_id": id
        }
    })
    .to_string()
}

fn function_output(id: &str) -> String {
    serde_json::json!({
        "type": "response_item",
        "payload": {"type": "function_call_output", "call_id": id, "output": "done"}
    })
    .to_string()
}

#[test]
fn claude_tool_use_without_result_is_pending() {
    let case = Case::new();
    let file = case.path().join("session.jsonl");
    let source = RecordSource::ClaudeSession { file: file.clone() };
    write(
        &file,
        &[
            prompt("Build and ship"),
            tool_use(
                "toolu_01",
                "Read",
                serde_json::json!({"file_path": "/w/Cargo.toml"}),
            ),
            tool_result("toolu_01", false),
            serde_json::json!({
                "type": "assistant",
                "message": {
                    "role": "assistant",
                    "content": [
                        {"type": "text", "text": "Building now"},
                        {"type": "tool_use", "id": "toolu_02", "name": "Bash",
                         "input": {"command": "cargo build --release", "description": "Build"}},
                        {"type": "tool_use", "id": "toolu_03", "name": "Edit",
                         "input": {"file_path": "/w/src/main.rs", "old_string": "a", "new_string": "b"}}
                    ],
                    "stop_reason": "tool_use"
                }
            })
            .to_string(),
            serde_json::json!({
                "type": "user",
                "isMeta": true,
                "message": {"role": "user", "content": "<system-reminder>context</system-reminder>"}
            })
            .to_string(),
            serde_json::json!({
                "type": "user",
                "isSidechain": true,
                "message": {"role": "user", "content": [{"type": "text", "text": "Sub-agent prompt"}]}
            })
            .to_string(),
        ],
    );
    assert_eq!(
        unknown(&source, &case.work(), &file),
        vec![
            command("Bash", "cargo build --release"),
            command("Edit", "/w/src/main.rs"),
        ]
    );
}

#[test]
fn codex_function_call_without_output_is_pending() {
    let case = Case::new();
    let sessions = case.sessions();
    let source = RecordSource::CodexRollouts {
        sessions: sessions.clone(),
    };
    let path = rollout(
        &sessions,
        "rollout-mine.jsonl",
        &case.work(),
        &[
            started(),
            function_call(
                "call_1",
                "shell",
                serde_json::json!({"command": ["bash", "-lc", "ls"]}),
            ),
            function_output("call_1"),
            function_call(
                "call_2",
                "shell_command",
                serde_json::json!({"command": "git push origin feat", "workdir": "/w"}),
            ),
            serde_json::json!({
                "type": "response_item",
                "payload": {
                    "type": "custom_tool_call",
                    "status": "completed",
                    "call_id": "call_3",
                    "name": "apply_patch",
                    "input": "*** Begin Patch\n*** Update File: src/lib.rs\n@@\n-a\n+b\n*** End Patch\n"
                }
            })
            .to_string(),
            serde_json::json!({
                "type": "response_item",
                "payload": {
                    "type": "local_shell_call",
                    "call_id": "call_4",
                    "status": "in_progress",
                    "action": {"type": "exec", "command": ["make", "deploy"]}
                }
            })
            .to_string(),
            r#"{"type":"response_item","payload":{"type":"function_call","name":"exec_command","arguments":{"cmd":"npm run deploy"},"call_id":"call_5"}}"#.to_owned(),
            r#"{"type":"response_item","payload":{"type":"web_search_call","status":"completed","action":{"type":"search","query":"x"}}}"#.to_owned(),
            r#"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"also run the tests"}]}}"#.to_owned(),
        ],
    );
    assert_eq!(
        unknown(&source, &case.work(), &path),
        vec![
            command("shell_command", "git push origin feat"),
            command(
                "apply_patch",
                "*** Begin Patch *** Update File: src/lib.rs @@ -a +b *** End Patch"
            ),
            command("local_shell", "make deploy"),
            command("exec_command", "npm run deploy"),
        ]
    );
}

#[test]
fn completed_calls_are_not_pending() {
    let case = Case::new();
    let file = case.path().join("session.jsonl");
    let source = RecordSource::ClaudeSession { file: file.clone() };
    write(
        &file,
        &[
            prompt("Clean up"),
            tool_use(
                "toolu_01",
                "Bash",
                serde_json::json!({"command": "rm -rf build"}),
            ),
            prompt("[Request interrupted by user for tool use]"),
            prompt("Try again"),
            tool_use(
                "toolu_02",
                "Bash",
                serde_json::json!({"command": "rm -rf build"}),
            ),
            tool_result("toolu_02", true),
            tool_use(
                "toolu_03",
                "Bash",
                serde_json::json!({"command": "cargo clean"}),
            ),
            tool_result("toolu_03", false),
            serde_json::json!({
                "type": "assistant",
                "message": {"role": "assistant", "content": [{"type": "text", "text": "Clean"}], "stop_reason": "end_turn"}
            })
            .to_string(),
        ],
    );
    assert_eq!(unknown(&source, &case.work(), &file), Vec::new());

    let sessions = case.sessions();
    let source = RecordSource::CodexRollouts {
        sessions: sessions.clone(),
    };
    let path = rollout(
        &sessions,
        "rollout-mine.jsonl",
        &case.work(),
        &[
            started(),
            function_call("call_1", "shell", serde_json::json!({"command": ["ls"]})),
            r#"{"type":"event_msg","payload":{"type":"turn_aborted","reason":"interrupted"}}"#
                .to_owned(),
            started(),
            function_call("call_2", "shell", serde_json::json!({"command": ["ls"]})),
            function_output("call_2"),
            serde_json::json!({
                "type": "response_item",
                "payload": {"type": "custom_tool_call", "call_id": "call_3", "name": "apply_patch", "input": "*** Begin Patch"}
            })
            .to_string(),
            serde_json::json!({
                "type": "response_item",
                "payload": {"type": "custom_tool_call_output", "call_id": "call_3", "output": "Success"}
            })
            .to_string(),
            r#"{"type":"event_msg","payload":{"type":"task_complete","last_agent_message":"Done"}}"#
                .to_owned(),
        ],
    );
    assert_eq!(unknown(&source, &case.work(), &path), Vec::new());
}

#[test]
fn a_record_elsewhere_of_another_folder_or_behind_a_link_is_not_read() {
    let case = Case::new();
    let sessions = case.sessions();
    let codex = RecordSource::CodexRollouts {
        sessions: sessions.clone(),
    };
    let lines = [
        started(),
        function_call("call_1", "shell", serde_json::json!({"command": ["ls"]})),
    ];
    let mine = rollout(&sessions, "rollout-mine.jsonl", &case.work(), &lines);
    assert_eq!(
        unknown(&codex, &case.work(), &mine),
        vec![command("shell", "ls")]
    );
    let other = rollout(
        &sessions,
        "rollout-other.jsonl",
        &case.path().join("other"),
        &lines,
    );
    assert_eq!(unknown(&codex, &case.work(), &other), Vec::new());
    let loose = rollout(
        &case.path().join("loose"),
        "rollout-loose.jsonl",
        &case.work(),
        &lines,
    );
    assert_eq!(unknown(&codex, &case.work(), &loose), Vec::new());
    let around = sessions
        .join("..")
        .join("loose/2026/10/27/rollout-loose.jsonl");
    assert_eq!(unknown(&codex, &case.work(), &around), Vec::new());
    let linked = sessions.join("2026/10/27/rollout-linked.jsonl");
    std::os::unix::fs::symlink(&mine, &linked).unwrap();
    assert_eq!(unknown(&codex, &case.work(), &linked), Vec::new());

    let file = case.path().join("session.jsonl");
    write(
        &file,
        &[
            prompt("Go"),
            tool_use("toolu_01", "Bash", serde_json::json!({"command": "ls"})),
        ],
    );
    let claude = RecordSource::ClaudeSession { file: file.clone() };
    assert_eq!(
        unknown(&claude, &case.work(), &file),
        vec![command("Bash", "ls")]
    );
    let elsewhere = case.path().join("elsewhere.jsonl");
    std::fs::copy(&file, &elsewhere).unwrap();
    assert_eq!(unknown(&claude, &case.work(), &elsewhere), Vec::new());
    let link = case.path().join("link.jsonl");
    std::os::unix::fs::symlink(&file, &link).unwrap();
    let behind = RecordSource::ClaudeSession { file: link.clone() };
    assert_eq!(unknown(&behind, &case.work(), &link), Vec::new());
    assert_eq!(
        unknown(&claude, &case.work(), &case.path().join("missing.jsonl")),
        Vec::new()
    );
}

#[test]
fn summaries_are_one_plain_line_of_at_most_200_bytes() {
    let case = Case::new();
    let file = case.path().join("session.jsonl");
    let source = RecordSource::ClaudeSession { file: file.clone() };
    let long = format!("x {}", "\u{e9}".repeat(300));
    write(
        &file,
        &[
            prompt("Go"),
            tool_use(
                "toolu_01",
                "Bash\u{7}",
                serde_json::json!({"command": "echo one\n\u{1b}[201~echo two\tthree"}),
            ),
            tool_use("toolu_02", "Bash", serde_json::json!({"command": long})),
            tool_use(
                "toolu_03",
                "TodoWrite",
                serde_json::json!({"todos": [{"content": "ship"}]}),
            ),
            tool_use(
                "toolu_04",
                &"T".repeat(100),
                serde_json::json!({"command": "\n\t"}),
            ),
        ],
    );
    let found = unknown(&source, &case.work(), &file);
    assert_eq!(
        found,
        vec![
            command("Bash", "echo one [201~echo two three"),
            command("Bash", &format!("x {}...", "\u{e9}".repeat(97))),
            command("TodoWrite", r#"{"todos":[{"content":"ship"}]}"#),
            command(&format!("{}...", "T".repeat(TOOL_BYTES_MAX - 3)), ""),
        ]
    );
    for pending in &found {
        assert!(pending.summary.len() <= SUMMARY_BYTES_MAX);
        assert!(pending.tool.len() <= TOOL_BYTES_MAX);
        assert!(!pending.summary.chars().any(char::is_control));
        assert!(!pending.tool.chars().any(char::is_control));
    }
}

#[test]
fn at_most_the_newest_sixteen_pending_commands_are_named() {
    let case = Case::new();
    let file = case.path().join("session.jsonl");
    let source = RecordSource::ClaudeSession { file: file.clone() };
    let mut lines = vec![prompt("Go")];
    for at in 1..=20 {
        lines.push(tool_use(
            &format!("toolu_{at}"),
            "Bash",
            serde_json::json!({"command": format!("echo {at}")}),
        ));
    }
    lines.push(tool_use(
        "toolu_20",
        "Bash",
        serde_json::json!({"command": "echo again"}),
    ));
    write(&file, &lines);
    let found = unknown(&source, &case.work(), &file);
    assert_eq!(PENDING_MAX, 16);
    assert_eq!(
        found,
        (5..=20)
            .map(|at| command("Bash", &format!("echo {at}")))
            .collect::<Vec<_>>()
    );
}
