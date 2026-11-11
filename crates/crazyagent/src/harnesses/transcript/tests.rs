use super::*;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;

const SHORT_LINES_READ_TIME: Duration = Duration::from_secs(10);
const IDLE_POLLS: usize = 1000;
const IDLE_POLLS_TIME: Duration = Duration::from_secs(2);
const PARKED_LINE_WAIT: Duration = Duration::from_secs(60);

struct Case {
    root: tempfile::TempDir,
}

impl Case {
    fn new() -> Self {
        let root = tempfile::Builder::new()
            .prefix("ca-transcript-")
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

    fn claude(&self) -> (PathBuf, TranscriptReader) {
        let file = self.path().join("session.jsonl");
        let reader = TranscriptReader::new(
            Transcript::ClaudeSession { file: file.clone() },
            &self.work(),
            SystemTime::now(),
        );
        (file, reader)
    }
}

fn append(path: &Path, text: &str) {
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .unwrap();
    file.write_all(text.as_bytes()).unwrap();
}

fn text(value: &str) -> TranscriptEvent {
    TranscriptEvent::Text {
        text: value.to_owned(),
    }
}

#[test]
fn claude_turn_reads_prompt_text_tools_and_finish() {
    let case = Case::new();
    let (file, mut reader) = case.claude();
    assert_eq!(reader.read().unwrap(), Vec::new());
    append(
        &file,
        concat!(
            r#"{"type":"user","message":{"role":"user","content":"Fix the build"}}"#,
            "\n",
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Looking"},{"type":"tool_use","name":"Bash"}],"stop_reason":"tool_use"}}"#,
            "\n",
            r#"{"type":"user","message":{"content":[{"type":"tool_result","content":"ok"}]}}"#,
            "\n",
        ),
    );
    assert_eq!(
        reader.read().unwrap(),
        vec![
            TranscriptEvent::TurnStarted,
            text("Looking\n"),
            TranscriptEvent::ToolUse {
                name: "Bash".to_owned()
            },
        ]
    );
    append(
        &file,
        r#"{"type":"assistant","message":{"content":[{"type":"text","text":"DELEGATE codex: run tests\n"}],"stop_reason":"end_turn"}}"#,
    );
    assert_eq!(reader.read().unwrap(), Vec::new());
    append(
        &file,
        concat!(
            "\n",
            r#"{"type":"system","subtype":"turn_duration","durationMs":12}"#,
            "\n"
        ),
    );
    assert_eq!(
        reader.read().unwrap(),
        vec![
            text("DELEGATE codex: run tests\n"),
            TranscriptEvent::TurnFinished { success: true },
        ]
    );
}

#[test]
fn claude_meta_lines_and_unknown_lines_are_ignored() {
    let case = Case::new();
    let (file, mut reader) = case.claude();
    append(
        &file,
        concat!(
            r#"{"type":"user","isMeta":true,"message":{"content":"<command-name>/clear</command-name>"}}"#,
            "\n",
            "not json\n",
            r#"{"type":"attachment","attachment":{"type":"file"}}"#,
            "\n",
            r#"{"type":"system","subtype":"turn_duration"}"#,
            "\n",
        ),
    );
    assert_eq!(reader.read().unwrap(), Vec::new());
}

/// OB-6 merge-up with OB-1: the typed lines read for a live turn's words are the prompts
/// that start a turn and no other user line, so a line that starts no turn is never read
/// as the words of the turn after it. Each typed line comes with the offset just past it.
#[test]
fn claude_typed_lines_past_an_offset_are_only_the_prompts_that_start_a_turn() {
    let case = Case::new();
    let file = case.path().join("typed.jsonl");
    let summary = r#"{"type":"user","isCompactSummary":true,"message":{"content":"This session is being continued from a previous conversation."}}"#;
    let sidechain =
        r#"{"type":"user","isSidechain":true,"message":{"content":"Search the repository"}}"#;
    let first = r#"{"type":"user","message":{"content":"Plan the launch"}}"#;
    let second = r#"{"type":"user","message":{"content":"Ship it"}}"#;
    append(
        &file,
        &format!("{summary}\n{first}\n{sidechain}\n{second}\n"),
    );
    let first_end = u64::try_from(summary.len() + 1 + first.len() + 1).unwrap();
    let length = std::fs::metadata(&file).unwrap().len();

    let all = typed_past("claude-code", &file, 0).unwrap();
    let rest = typed_past("claude-code", &file, first_end).unwrap();

    assert_eq!(
        all,
        (
            vec![
                (first_end, "Plan the launch".to_owned()),
                (length, "Ship it".to_owned()),
            ],
            length,
        )
    );
    assert_eq!(rest, (vec![(length, "Ship it".to_owned())], length));
}

/// OB6R2-M1: typed lines are read on to the record's end, however much a turn wrote
/// between two of them, here one 5 MiB tool result that is longer than any one read.
#[test]
fn claude_typed_lines_are_read_to_the_record_s_end_past_a_turn_of_megabytes() {
    let case = Case::new();
    let file = case.path().join("long-turn.jsonl");
    let first = r#"{"type":"user","message":{"content":"Plan the launch"}}"#;
    let result = format!(
        r#"{{"type":"user","message":{{"content":[{{"type":"tool_result","tool_use_id":"t1","content":"{}"}}]}}}}"#,
        "x".repeat(5 * 1024 * 1024)
    );
    let second = r#"{"type":"user","message":{"content":"Ship it"}}"#;
    append(&file, &format!("{first}\n{result}\n{second}\n"));
    let first_end = u64::try_from(first.len() + 1).unwrap();
    let length = std::fs::metadata(&file).unwrap().len();

    let all = typed_past("claude-code", &file, 0).unwrap();
    let rest = typed_past("claude-code", &file, first_end).unwrap();
    let none = typed_past("claude-code", &file, length).unwrap();

    assert_eq!(
        all,
        (
            vec![
                (first_end, "Plan the launch".to_owned()),
                (length, "Ship it".to_owned()),
            ],
            length,
        )
    );
    assert_eq!(rest, (vec![(length, "Ship it".to_owned())], length));
    assert_eq!(none, (Vec::new(), length));
}

/// OB6R2-M1: a record with no typed line past the offset is read to its last finished
/// line, which is where a turn with no typed words is bound, and a line the harness has
/// not finished is left for the next read.
#[test]
fn claude_a_record_with_no_typed_line_reads_to_its_last_finished_line() {
    let case = Case::new();
    let file = case.path().join("no-typed.jsonl");
    let result = format!(
        r#"{{"type":"user","message":{{"content":[{{"type":"tool_result","tool_use_id":"t1","content":"{}"}}]}}}}"#,
        "x".repeat(5 * 1024 * 1024)
    );
    append(&file, &format!("{result}\n"));
    let finished = std::fs::metadata(&file).unwrap().len();
    append(&file, r#"{"type":"user","message":{"content":"Half a li"#);

    let read = typed_past("claude-code", &file, 0).unwrap();

    assert_eq!(read, (Vec::new(), finished));
}

/// OB6R2-M1: a line longer than `LINE_BYTES_MAX`, which the turn reader refuses too, is
/// passed over without being held in memory, and the typed line after it is read.
#[test]
fn claude_a_line_longer_than_the_reader_takes_is_passed_over_not_read() {
    let case = Case::new();
    let file = case.path().join("long-line.jsonl");
    let long = format!(
        r#"{{"type":"user","message":{{"content":"{}"}}}}"#,
        "x".repeat(LINE_BYTES_MAX)
    );
    let typed = r#"{"type":"user","message":{"content":"Ship it"}}"#;
    append(&file, &format!("{long}\n{typed}\n"));
    let length = std::fs::metadata(&file).unwrap().len();

    let read = typed_past("claude-code", &file, 0).unwrap();

    assert_eq!(read, (vec![(length, "Ship it".to_owned())], length));
}

/// A Claude Code user line that is no prompt to the model starts no turn, whatever text it
/// carries, and the prompt typed after it starts one as before (OB-1: such a line opened a
/// live master's turn that nothing closed). One shape per entry, each on a fresh record.
#[test]
fn claude_user_lines_that_are_no_prompt_start_no_turn() {
    let shapes = [
        (
            "compact summary",
            r#"{"type":"user","isCompactSummary":true,"message":{"content":"This session is being continued from a previous conversation."}}"#,
        ),
        (
            "transcript only",
            r#"{"type":"user","isVisibleInTranscriptOnly":true,"message":{"content":"Shown in the transcript only"}}"#,
        ),
        (
            "sub-agent line",
            r#"{"type":"user","isSidechain":true,"message":{"content":"Search the repository"}}"#,
        ),
        (
            "tool result with text",
            r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":"ok"},{"type":"text","text":"Continue"}]}}"#,
        ),
        (
            "local command",
            r#"{"type":"user","message":{"content":"<command-name>/model</command-name>\n<command-message>model</command-message>\n<command-args></command-args>"}}"#,
        ),
        (
            "command message",
            r#"{"type":"user","message":{"content":"<command-message>review is running</command-message>\n<command-name>/review</command-name>"}}"#,
        ),
        (
            "local command output",
            r#"{"type":"user","message":{"content":"<local-command-stdout>Set model to opus</local-command-stdout>"}}"#,
        ),
        (
            "local command error",
            r#"{"type":"user","message":{"content":"<local-command-stderr>Unknown model</local-command-stderr>"}}"#,
        ),
        (
            "local command caveat",
            r#"{"type":"user","message":{"content":"<local-command-caveat>The messages below were generated by local commands.</local-command-caveat>"}}"#,
        ),
        (
            "bash mode input",
            r#"{"type":"user","message":{"content":"<bash-input>ls</bash-input>"}}"#,
        ),
        (
            "bash mode output",
            r#"{"type":"user","message":{"content":"<bash-stdout>a.txt</bash-stdout><bash-stderr></bash-stderr>"}}"#,
        ),
        (
            "bash mode error",
            r#"{"type":"user","message":{"content":"<bash-stderr>ls: no such file</bash-stderr>"}}"#,
        ),
        (
            "reminder",
            r#"{"type":"user","message":{"content":"<system-reminder>Plan mode is on</system-reminder>"}}"#,
        ),
        (
            "prompt hook",
            r#"{"type":"user","message":{"content":"<user-prompt-submit-hook>blocked</user-prompt-submit-hook>"}}"#,
        ),
        (
            "text block after blank space",
            r#"{"type":"user","message":{"content":[{"type":"text","text":"\n  <local-command-stdout>ok</local-command-stdout>"}]}}"#,
        ),
        (
            "blank text",
            r#"{"type":"user","message":{"content":"   "}}"#,
        ),
    ];
    for (name, line) in shapes {
        let case = Case::new();
        let (file, mut reader) = case.claude();
        append(&file, &format!("{line}\n"));
        assert_eq!(reader.read().unwrap(), Vec::new(), "{name}");
        append(
            &file,
            concat!(
                r#"{"type":"user","message":{"content":"Fix the build"}}"#,
                "\n"
            ),
        );
        assert_eq!(
            reader.read().unwrap(),
            vec![TranscriptEvent::TurnStarted],
            "{name}"
        );
    }
}

/// A typed command that runs a prompt is no prompt line itself, yet the model answers it:
/// the turn starts at the model's first line and ends like any other, so a live master that
/// runs a skill is still followed. A command line that carries an API error ends the same way.
#[test]
fn claude_a_typed_command_the_model_answers_is_a_turn_from_its_first_line() {
    let case = Case::new();
    let (file, mut reader) = case.claude();
    append(
        &file,
        concat!(
            r#"{"type":"user","message":{"content":"<command-message>review is running</command-message>\n<command-name>/review</command-name>"}}"#,
            "\n",
            r#"{"type":"user","isMeta":true,"message":{"content":[{"type":"text","text":"Review the open change"}]}}"#,
            "\n",
        ),
    );
    assert_eq!(reader.read().unwrap(), Vec::new());
    append(
        &file,
        concat!(
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Reviewing"},{"type":"tool_use","name":"Bash"}],"stop_reason":"tool_use"}}"#,
            "\n",
            r#"{"type":"user","message":{"content":[{"type":"tool_result","content":"ok"}]}}"#,
            "\n",
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Done"}],"stop_reason":"end_turn"}}"#,
            "\n",
        ),
    );
    assert_eq!(
        reader.read().unwrap(),
        vec![
            TranscriptEvent::TurnStarted,
            text("Reviewing\n"),
            TranscriptEvent::ToolUse {
                name: "Bash".to_owned()
            },
            text("Done\n"),
            TranscriptEvent::TurnFinished { success: true },
        ]
    );
    append(
        &file,
        concat!(
            r#"{"type":"user","message":{"content":"<command-message>review is running</command-message>\n<command-name>/review</command-name>"}}"#,
            "\n",
            r#"{"type":"assistant","isApiErrorMessage":true,"error":"rate_limit","message":{"content":[{"type":"text","text":"API Error"}]}}"#,
            "\n",
        ),
    );
    assert_eq!(
        reader.read().unwrap(),
        vec![
            TranscriptEvent::TurnStarted,
            TranscriptEvent::TurnError { rate_limited: true },
            TranscriptEvent::TurnFinished { success: false },
        ]
    );
}

/// A local command the model never answers leaves no turn open and none pending: its lines
/// and a later turn duration say nothing, and the next prompt starts exactly one turn.
#[test]
fn claude_a_local_command_the_model_never_answers_leaves_no_turn_open() {
    let case = Case::new();
    let (file, mut reader) = case.claude();
    append(
        &file,
        concat!(
            r#"{"type":"user","message":{"content":"<command-name>/model</command-name>\n<command-message>model</command-message>\n<command-args></command-args>"}}"#,
            "\n",
            r#"{"type":"user","message":{"content":"<local-command-stdout>Set model to opus</local-command-stdout>"}}"#,
            "\n",
            r#"{"type":"system","subtype":"turn_duration"}"#,
            "\n",
        ),
    );
    assert_eq!(reader.read().unwrap(), Vec::new());
    append(
        &file,
        concat!(
            r#"{"type":"user","message":{"content":"Fix the build"}}"#,
            "\n",
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Fixed"}],"stop_reason":"end_turn"}}"#,
            "\n",
        ),
    );
    assert_eq!(
        reader.read().unwrap(),
        vec![
            TranscriptEvent::TurnStarted,
            text("Fixed\n"),
            TranscriptEvent::TurnFinished { success: true },
        ]
    );
}

/// Only the model's answer to a line that was no prompt starts a turn by itself: a reply
/// with no such line before it starts none, as before, and neither does a sub-agent's reply
/// after one. A prompt that carries a reminder block beside its words is still a prompt.
#[test]
fn claude_a_reply_starts_a_turn_only_after_a_line_that_was_no_prompt() {
    let case = Case::new();
    let (file, mut reader) = case.claude();
    append(
        &file,
        concat!(
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Late"}],"stop_reason":"end_turn"}}"#,
            "\n",
            r#"{"type":"user","message":{"content":"<bash-input>ls</bash-input>"}}"#,
            "\n",
            r#"{"type":"assistant","isSidechain":true,"message":{"content":[{"type":"text","text":"Sub"}],"stop_reason":"end_turn"}}"#,
            "\n",
        ),
    );
    assert_eq!(reader.read().unwrap(), vec![text("Late\n"), text("Sub\n")]);
    append(
        &file,
        concat!(
            r#"{"type":"user","message":{"content":[{"type":"text","text":"<system-reminder>Plan mode is on</system-reminder>"},{"type":"text","text":"Fix the build"}]}}"#,
            "\n",
        ),
    );
    assert_eq!(reader.read().unwrap(), vec![TranscriptEvent::TurnStarted]);
}

/// OB-1 r1: a Claude Code turn dropped as none is open no more, so a typed command the
/// model answers after it is a turn from the model's first line, started once; and the
/// model at work in the dropped turn itself, with a tool or with text, starts it again.
#[test]
fn claude_a_dropped_turn_is_open_no_more_and_starts_again_with_the_model_s_work() {
    let prompt = concat!(
        r#"{"type":"user","message":{"content":"Fix the build"}}"#,
        "\n"
    );
    let reply = concat!(
        r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Reviewed"}],"stop_reason":"end_turn"}}"#,
        "\n"
    );
    let ended = vec![
        TranscriptEvent::TurnStarted,
        text("Reviewed\n"),
        TranscriptEvent::TurnFinished { success: true },
    ];

    let case = Case::new();
    let (file, mut reader) = case.claude();
    append(&file, prompt);
    assert_eq!(reader.read().unwrap(), vec![TranscriptEvent::TurnStarted]);
    reader.drop_turn();
    append(
        &file,
        concat!(
            r#"{"type":"user","message":{"content":"<command-message>review</command-message>\n<command-name>/review</command-name>"}}"#,
            "\n",
            r#"{"type":"user","isMeta":true,"message":{"content":[{"type":"text","text":"Review the open change."}]}}"#,
            "\n",
        ),
    );
    assert_eq!(reader.read().unwrap(), Vec::new());
    append(&file, reply);
    assert_eq!(reader.read().unwrap(), ended);

    let case = Case::new();
    let (file, mut reader) = case.claude();
    append(&file, prompt);
    assert_eq!(reader.read().unwrap(), vec![TranscriptEvent::TurnStarted]);
    reader.drop_turn();
    append(
        &file,
        concat!(
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Bash"}],"stop_reason":"tool_use"}}"#,
            "\n",
        ),
    );
    assert_eq!(
        reader.read().unwrap(),
        vec![
            TranscriptEvent::TurnStarted,
            TranscriptEvent::ToolUse {
                name: "Bash".to_owned()
            },
        ]
    );
    append(&file, reply);
    assert_eq!(reader.read().unwrap(), ended[1..]);

    let case = Case::new();
    let (file, mut reader) = case.claude();
    append(&file, prompt);
    assert_eq!(reader.read().unwrap(), vec![TranscriptEvent::TurnStarted]);
    reader.drop_turn();
    append(&file, reply);
    assert_eq!(reader.read().unwrap(), ended);
    append(&file, reply);
    assert_eq!(
        reader.read().unwrap(),
        vec![text("Reviewed\n")],
        "only a dropped turn starts again by the model's work"
    );
}

/// OB-1 r1: a Codex turn dropped as none gives no end when its end comes alone, and the
/// model's text in it starts it again so its end is given.
#[test]
fn codex_a_dropped_turn_gives_no_lone_end_and_starts_again_with_the_model_s_text() {
    let case = Case::new();
    let sessions = case.path().join("sessions");
    let mut reader = TranscriptReader::new(
        Transcript::CodexRollouts {
            sessions: sessions.clone(),
        },
        &case.work(),
        SystemTime::now(),
    );
    let path = rollout(
        &sessions,
        "2026/10/23",
        "rollout-dropped.jsonl",
        &case.work(),
        &[r#"{"type":"event_msg","payload":{"type":"task_started"}}"#],
    );
    assert_eq!(reader.read().unwrap(), vec![TranscriptEvent::TurnStarted]);
    reader.drop_turn();
    let complete = concat!(
        r#"{"type":"event_msg","payload":{"type":"task_complete"}}"#,
        "\n"
    );
    append(&path, complete);
    assert_eq!(reader.read().unwrap(), Vec::new());
    append(
        &path,
        concat!(
            r#"{"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"Done"}]}}"#,
            "\n",
        ),
    );
    append(&path, complete);
    assert_eq!(
        reader.read().unwrap(),
        vec![
            TranscriptEvent::TurnStarted,
            text("Done\n"),
            TranscriptEvent::TurnFinished { success: true },
        ]
    );
}

/// Only an open turn can be dropped: dropping with none open is a caller's mistake.
#[test]
#[should_panic(expected = "assertion failed: self.in_turn")]
fn dropping_a_turn_with_none_open_panics() {
    let case = Case::new();
    let (_, mut reader) = case.claude();
    reader.drop_turn();
}

#[test]
fn claude_interrupt_and_api_errors_end_the_turn() {
    let case = Case::new();
    let (file, mut reader) = case.claude();
    append(
        &file,
        concat!(
            r#"{"type":"user","message":{"content":"Long job"}}"#,
            "\n",
            r#"{"type":"user","message":{"content":[{"type":"text","text":"[Request interrupted by user]"}]}}"#,
            "\n",
            r#"{"type":"user","message":{"content":"Again"}}"#,
            "\n",
            r#"{"type":"assistant","isApiErrorMessage":true,"error":"rate_limit","message":{"content":[{"type":"text","text":"API Error"}]}}"#,
            "\n",
            r#"{"type":"user","message":{"content":"Once more"}}"#,
            "\n",
            r#"{"type":"assistant","isApiErrorMessage":true,"error":"server_error","message":{"content":[]}}"#,
            "\n",
        ),
    );
    assert_eq!(
        reader.read().unwrap(),
        vec![
            TranscriptEvent::TurnStarted,
            TranscriptEvent::Interrupted,
            TranscriptEvent::TurnFinished { success: false },
            TranscriptEvent::TurnStarted,
            TranscriptEvent::TurnError { rate_limited: true },
            TranscriptEvent::TurnFinished { success: false },
            TranscriptEvent::TurnStarted,
            TranscriptEvent::TurnError {
                rate_limited: false
            },
            TranscriptEvent::TurnFinished { success: false },
        ]
    );
}

#[test]
fn claude_finish_after_end_turn_is_reported_once() {
    let case = Case::new();
    let (file, mut reader) = case.claude();
    append(
        &file,
        concat!(
            r#"{"type":"user","message":{"content":"Hi"}}"#,
            "\n",
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Hello"}],"stop_reason":"end_turn"}}"#,
            "\n",
            r#"{"type":"system","subtype":"turn_duration"}"#,
            "\n",
        ),
    );
    assert_eq!(
        reader.read().unwrap(),
        vec![
            TranscriptEvent::TurnStarted,
            text("Hello\n"),
            TranscriptEvent::TurnFinished { success: true },
        ]
    );
}

/// A Bash call Claude Code runs in the background reports its task id, and the task
/// notification that later starts a turn reports each id it names as ended. An id with a
/// control character is not kept. The record shapes are copied from a real lead's transcript.
#[test]
fn claude_background_commands_are_followed_by_task_id() {
    let case = Case::new();
    let (file, mut reader) = case.claude();
    append(
        &file,
        concat!(
            r#"{"type":"user","message":{"content":"Run the check"}}"#,
            "\n",
            r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":"Command running in background with ID: bub9bmz4a."}]},"toolUseResult":{"backgroundTaskId":"bub9bmz4a"}}"#,
            "\n",
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Waiting"}],"stop_reason":"end_turn"}}"#,
            "\n",
            r#"{"type":"user","message":{"content":"<task-notification>\n<task-id>bub9bmz4a</task-id>\n<task-id>bad\u0007id</task-id>\n<status>completed</status>\n</task-notification>"}}"#,
            "\n",
        ),
    );
    assert_eq!(
        reader.read().unwrap(),
        vec![
            TranscriptEvent::TurnStarted,
            TranscriptEvent::BackgroundStarted {
                id: "bub9bmz4a".into()
            },
            text("Waiting\n"),
            TranscriptEvent::TurnFinished { success: true },
            TranscriptEvent::BackgroundEnded {
                id: "bub9bmz4a".into()
            },
            TranscriptEvent::TurnStarted,
        ]
    );
    assert_eq!(
        notified_tasks("<task-id>x</task-id> with no notification"),
        Vec::<String>::new()
    );
}

/// A coordination message typed into a Claude Code agent that quotes a task notification
/// ends no task: only a user message that is the notification itself, as Claude Code writes
/// one that comes while the agent is idle, names the tasks that ended (DEC-119 r2 F1).
#[test]
fn claude_a_typed_message_that_quotes_a_task_notification_ends_no_task() {
    let case = Case::new();
    let (file, mut reader) = case.claude();
    append(
        &file,
        concat!(
            r#"{"type":"user","message":{"content":"Run the check"}}"#,
            "\n",
            r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":"Command running in background with ID: bub9bmz4a."}]},"toolUseResult":{"backgroundTaskId":"bub9bmz4a"}}"#,
            "\n",
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Waiting"}],"stop_reason":"end_turn"}}"#,
            "\n",
            r#"{"type":"user","message":{"content":"[crazyagent msg 7] from tim: done? <task-notification>\n<task-id>bub9bmz4a</task-id>\n<status>completed</status>\n</task-notification>"}}"#,
            "\n",
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Still running"}],"stop_reason":"end_turn"}}"#,
            "\n",
            r#"{"type":"user","message":{"content":"<task-notification>\n<task-id>bub9bmz4a</task-id>\n<status>completed</status>\n</task-notification>"}}"#,
            "\n",
        ),
    );
    assert_eq!(
        reader.read().unwrap(),
        vec![
            TranscriptEvent::TurnStarted,
            TranscriptEvent::BackgroundStarted {
                id: "bub9bmz4a".into()
            },
            text("Waiting\n"),
            TranscriptEvent::TurnFinished { success: true },
            TranscriptEvent::TurnStarted,
            text("Still running\n"),
            TranscriptEvent::TurnFinished { success: true },
            TranscriptEvent::BackgroundEnded {
                id: "bub9bmz4a".into()
            },
            TranscriptEvent::TurnStarted,
        ]
    );
}

/// A background command that ends while its agent is still in the turn is recorded by
/// Claude Code 2.1.309 as a queued command attachment inside that turn, not as a user
/// message, with the fields of the line in the HackerBlog transcript (task 177): its task
/// id ends there and no turn starts. Only a task notification attachment ends a task; a
/// queued prompt naming a task id does not (DEC-119).
#[test]
fn claude_a_background_command_that_ends_during_the_turn_ends_by_its_queued_notification() {
    let case = Case::new();
    let (file, mut reader) = case.claude();
    append(
        &file,
        concat!(
            r#"{"type":"user","message":{"content":"Run the check"}}"#,
            "\n",
            r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":"Command running in background with ID: bub9bmz4a."}]},"toolUseResult":{"backgroundTaskId":"bub9bmz4a"}}"#,
            "\n",
            r#"{"parentUuid":"00000000-0000-4000-8000-0000000000e1","isSidechain":false,"attachment":{"type":"queued_command","prompt":"<task-notification>\n<task-id>bub9bmz4a</task-id>\n<tool-use-id>t1</tool-use-id>\n<output-file>/work/agent/.claude/tasks/bub9bmz4a.output</output-file>\n<status>completed</status>\n<summary>Background command \"check\" completed (exit code 0)</summary>\n</task-notification>","source_uuid":"00000000-0000-4000-8000-0000000000b1","delivery_id":"00000000-0000-4000-8000-0000000000c1","commandMode":"task-notification","origin":{"kind":"task-notification","producer":"session-task"},"runId":"00000000-0000-4000-8000-0000000000d1","timestamp":"2026-11-07T18:52:01.000Z"},"type":"attachment","uuid":"00000000-0000-4000-8000-0000000000f1","timestamp":"2026-11-07T18:52:01.000Z","rendered":[{"type":"user","message":{"role":"user","content":"<task-notification>\n<task-id>bub9bmz4a</task-id>\n<tool-use-id>t1</tool-use-id>\n<output-file>/work/agent/.claude/tasks/bub9bmz4a.output</output-file>\n<status>completed</status>\n<summary>Background command \"check\" completed (exit code 0)</summary>\n</task-notification>"}}],"renderedInHumanTurn":[{"content":"<task-notification>\n<task-id>bub9bmz4a</task-id>\n<tool-use-id>t1</tool-use-id>\n<output-file>/work/agent/.claude/tasks/bub9bmz4a.output</output-file>\n<status>completed</status>\n<summary>Background command \"check\" completed (exit code 0)</summary>\n</task-notification>"}],"renderedRole":"user","session_id":"00000000-0000-4000-8000-0000000000a1","userType":"external","entrypoint":"cli","cwd":"/work/agent","sessionId":"00000000-0000-4000-8000-0000000000a1","version":"2.1.309","gitBranch":"agent/check"}"#,
            "\n",
            r#"{"parentUuid":"00000000-0000-4000-8000-0000000000e2","isSidechain":false,"attachment":{"type":"queued_command","prompt":"<task-notification>\n<task-id>other1</task-id>\n</task-notification>","source_uuid":"00000000-0000-4000-8000-0000000000b2","delivery_id":"00000000-0000-4000-8000-0000000000c2","commandMode":"prompt","origin":{"kind":"user"},"runId":"00000000-0000-4000-8000-0000000000d2","timestamp":"2026-11-07T18:52:02.000Z"},"type":"attachment","uuid":"00000000-0000-4000-8000-0000000000f2","timestamp":"2026-11-07T18:52:02.000Z","rendered":[{"type":"user","message":{"role":"user","content":"<task-notification>\n<task-id>other1</task-id>\n</task-notification>"}}],"renderedInHumanTurn":[{"content":"<task-notification>\n<task-id>other1</task-id>\n</task-notification>"}],"renderedRole":"user","session_id":"00000000-0000-4000-8000-0000000000a1","userType":"external","entrypoint":"cli","cwd":"/work/agent","sessionId":"00000000-0000-4000-8000-0000000000a1","version":"2.1.309","gitBranch":"agent/check"}"#,
            "\n",
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Check passed"}],"stop_reason":"end_turn"}}"#,
            "\n",
        ),
    );
    assert_eq!(
        reader.read().unwrap(),
        vec![
            TranscriptEvent::TurnStarted,
            TranscriptEvent::BackgroundStarted {
                id: "bub9bmz4a".into()
            },
            TranscriptEvent::BackgroundEnded {
                id: "bub9bmz4a".into()
            },
            text("Check passed\n"),
            TranscriptEvent::TurnFinished { success: true },
        ]
    );
}

/// A background command the agent stops itself with TaskStop gets no task notification, so
/// the stop's tool result, which names the task, ends it (w-review LTK-H1, the record shape
/// a real Claude Code session wrote on 2026-10-29).
#[test]
fn claude_a_background_command_the_agent_stops_ends_by_its_task_id() {
    let case = Case::new();
    let (file, mut reader) = case.claude();
    append(
        &file,
        concat!(
            r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":"Command running in background with ID: b20nv1bx6."}]},"toolUseResult":{"backgroundTaskId":"b20nv1bx6"}}"#,
            "\n",
            r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t2","content":"Successfully stopped task: b20nv1bx6 (npm run dev)"}]},"toolUseResult":{"message":"Successfully stopped task: b20nv1bx6 (npm run dev)","task_id":"b20nv1bx6","task_type":"local_bash"}}"#,
            "\n",
        ),
    );
    assert_eq!(
        reader.read().unwrap(),
        vec![
            TranscriptEvent::BackgroundStarted {
                id: "b20nv1bx6".into()
            },
            TranscriptEvent::BackgroundEnded {
                id: "b20nv1bx6".into()
            },
        ]
    );
}

/// A background command Claude Code ends with its caller's final response sends no
/// notification, so it is not followed: an id waited on would never end (HB-BEFR).
#[test]
fn claude_a_background_command_that_ends_with_the_final_response_is_not_followed() {
    let case = Case::new();
    let (file, mut reader) = case.claude();
    append(
        &file,
        concat!(
            r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":"Command running in background with ID: b30aa."}]},"toolUseResult":{"backgroundTaskId":"b30aa","backgroundEndsWithFinalResponse":true}}"#,
            "\n",
            r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t2","content":"Command running in background with ID: b30ab."}]},"toolUseResult":{"backgroundTaskId":"b30ab","backgroundEndsWithFinalResponse":false}}"#,
            "\n",
        ),
    );
    assert_eq!(
        reader.read().unwrap(),
        vec![TranscriptEvent::BackgroundStarted { id: "b30ab".into() }]
    );
}

/// A background command the agent read as settled with GetTask ends by that snapshot, since
/// Claude Code then withdraws its notification; a snapshot still working ends nothing
/// (HB-TASKGET).
#[test]
fn claude_a_get_task_snapshot_of_a_settled_command_ends_it() {
    let case = Case::new();
    let (file, mut reader) = case.claude();
    append(
        &file,
        concat!(
            r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":"Command running in background with ID: b40aa."}]},"toolUseResult":{"backgroundTaskId":"b40aa"}}"#,
            "\n",
            r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t2","content":"Command running in background with ID: b40ab."}]},"toolUseResult":{"backgroundTaskId":"b40ab"}}"#,
            "\n",
            r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t3","content":"{\"taskId\":\"b40aa\"}"}]},"toolUseResult":{"taskId":"b40aa","status":"working","statusMessage":"Still running.","createdAt":"2026-11-10T04:00:00Z","lastUpdatedAt":"2026-11-10T04:00:00Z"}}"#,
            "\n",
            r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t4","content":"{\"taskId\":\"b40aa\"}"}]},"toolUseResult":{"taskId":"b40aa","status":"completed","statusMessage":"Done.","createdAt":"2026-11-10T04:00:00Z","lastUpdatedAt":"2026-11-10T04:01:00Z"}}"#,
            "\n",
            r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t5","content":"{\"taskId\":\"b40ab\"}"}]},"toolUseResult":{"taskId":"b40ab","status":"cancelled","statusMessage":"Stopped.","createdAt":"2026-11-10T04:00:00Z","lastUpdatedAt":"2026-11-10T04:01:00Z"}}"#,
            "\n",
        ),
    );
    assert_eq!(
        reader.read().unwrap(),
        vec![
            TranscriptEvent::BackgroundStarted { id: "b40aa".into() },
            TranscriptEvent::BackgroundStarted { id: "b40ab".into() },
            TranscriptEvent::BackgroundEnded { id: "b40aa".into() },
            TranscriptEvent::BackgroundEnded { id: "b40ab".into() },
        ]
    );
}

#[test]
fn claude_transcript_behind_a_symlink_is_not_followed() {
    let case = Case::new();
    let real = case.path().join("real.jsonl");
    append(
        &real,
        concat!(r#"{"type":"user","message":{"content":"Hi"}}"#, "\n"),
    );
    let (file, mut reader) = case.claude();
    std::os::unix::fs::symlink(&real, &file).unwrap();
    assert_eq!(reader.read().unwrap(), Vec::new());
}

#[test]
fn line_above_the_limit_is_refused() {
    let case = Case::new();
    let (file, mut reader) = case.claude();
    append(&file, &"x".repeat(LINE_BYTES_MAX + 1));
    let mut outcome = Ok(Vec::new());
    for _ in 0..(LINE_BYTES_MAX / READ_BYTES_MAX + 2) {
        outcome = reader.read();
        if outcome.is_err() {
            break;
        }
    }
    assert_eq!(
        outcome,
        Err(LaunchError(
            "launch_transcript_line_too_long".to_owned(),
            None
        ))
    );
}

fn rollout(sessions: &Path, day: &str, name: &str, cwd: &Path, lines: &[&str]) -> PathBuf {
    let folder = sessions.join(day);
    std::fs::create_dir_all(&folder).unwrap();
    let path = folder.join(name);
    let meta = serde_json::json!({
        "timestamp": "2026-10-23T22:00:00Z",
        "type": "session_meta",
        "payload": {"id": "s", "cwd": cwd.to_string_lossy()}
    });
    let mut body = format!("{meta}\n");
    for line in lines {
        body.push_str(line);
        body.push('\n');
    }
    std::fs::write(&path, body).unwrap();
    path
}

#[test]
fn codex_rollout_of_this_working_folder_is_found_and_read() {
    let case = Case::new();
    let sessions = case.path().join("sessions");
    let since = SystemTime::now();
    let other = case.path().join("other");
    rollout(
        &sessions,
        "2026/10/23",
        "rollout-other.jsonl",
        &other,
        &[r#"{"type":"event_msg","payload":{"type":"task_started"}}"#],
    );
    let mut reader = TranscriptReader::new(
        Transcript::CodexRollouts {
            sessions: sessions.clone(),
        },
        &case.work(),
        since,
    );
    assert_eq!(reader.read().unwrap(), Vec::new());
    let path = rollout(
        &sessions,
        "2026/10/23",
        "rollout-mine.jsonl",
        &case.work(),
        &[
            r#"{"type":"event_msg","payload":{"type":"task_started"}}"#,
            r#"{"type":"response_item","payload":{"type":"reasoning","summary":[]}}"#,
            r#"{"type":"response_item","payload":{"type":"function_call","name":"shell"}}"#,
            r#"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"Fix it"}]}}"#,
            r#"{"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"Done"}]}}"#,
            r#"{"type":"event_msg","payload":{"type":"task_complete","last_agent_message":"Done"}}"#,
        ],
    );
    assert_eq!(
        reader.read().unwrap(),
        vec![
            TranscriptEvent::TurnStarted,
            TranscriptEvent::ToolUse {
                name: "shell".to_owned()
            },
            text("Done\n"),
            TranscriptEvent::TurnFinished { success: true },
        ]
    );
    append(
        &path,
        concat!(
            r#"{"type":"event_msg","payload":{"type":"task_started"}}"#,
            "\n",
            r#"{"type":"event_msg","payload":{"type":"error","message":"You've hit your usage limit"}}"#,
            "\n",
            r#"{"type":"event_msg","payload":{"type":"turn_aborted","reason":"interrupted"}}"#,
            "\n",
        ),
    );
    assert_eq!(
        reader.read().unwrap(),
        vec![
            TranscriptEvent::TurnStarted,
            TranscriptEvent::TurnError { rate_limited: true },
            TranscriptEvent::Interrupted,
            TranscriptEvent::TurnFinished { success: false },
        ]
    );
}

#[test]
fn codex_rollout_older_than_the_session_is_ignored() {
    let case = Case::new();
    let sessions = case.path().join("sessions");
    let path = rollout(
        &sessions,
        "2026/10/22",
        "rollout-old.jsonl",
        &case.work(),
        &[r#"{"type":"event_msg","payload":{"type":"task_started"}}"#],
    );
    let old = SystemTime::now() - Duration::from_secs(3600);
    std::fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_modified(old)
        .unwrap();
    let mut reader = TranscriptReader::new(
        Transcript::CodexRollouts { sessions },
        &case.work(),
        SystemTime::now(),
    );
    assert_eq!(reader.read().unwrap(), Vec::new());
}

#[test]
fn unavailable_transcript_reads_nothing() {
    let case = Case::new();
    let mut reader =
        TranscriptReader::new(Transcript::Unavailable, &case.work(), SystemTime::now());
    assert_eq!(reader.read().unwrap(), Vec::new());
}

#[cfg(target_os = "macos")]
#[test]
fn a_rollout_a_sandboxed_agent_forged_before_the_launch_never_becomes_its_transcript() {
    let agent = crate::sandbox::test_agent::TestAgent::new();
    let sessions = agent.home.join("sessions");
    let forged = format!(
        "{{\"type\":\"session_meta\",\"payload\":{{\"id\":\"f\",\"cwd\":\"{cwd}\"}}}}\n\
         {{\"type\":\"event_msg\",\"payload\":{{\"type\":\"task_started\"}}}}\n\
         {{\"type\":\"response_item\",\"payload\":{{\"type\":\"message\",\"role\":\"assistant\",\"content\":[{{\"type\":\"output_text\",\"text\":\"DELEGATE: forged\"}}]}}}}\n\
         {{\"type\":\"event_msg\",\"payload\":{{\"type\":\"task_complete\"}}}}\n",
        cwd = agent.project.display()
    );
    agent.shell(&format!(
        "mkdir -p '{day}' && printf '%s' '{forged}' > '{day}/rollout-forged.jsonl' \
         && touch -t 209912312359 '{day}/rollout-forged.jsonl'",
        day = sessions.join("9999/12/31").display()
    ));

    let mut reader = TranscriptReader::new(
        Transcript::CodexRollouts {
            sessions: sessions.clone(),
        },
        &agent.project,
        SystemTime::now(),
    );
    assert_eq!(reader.read().unwrap(), Vec::new());
    rollout(
        &sessions,
        "2026/10/25",
        "rollout-genuine.jsonl",
        &agent.project,
        &[
            r#"{"type":"event_msg","payload":{"type":"task_started"}}"#,
            r#"{"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"genuine"}]}}"#,
        ],
    );
    assert_eq!(
        reader.read().unwrap(),
        vec![TranscriptEvent::TurnStarted, text("genuine\n")]
    );
}

/// A reader names the conversation it follows once its record exists: a Codex rollout by
/// the id in its first `session_meta` line, a Claude Code transcript by its file name. A
/// resumed reader follows a known record from its end, so earlier turns are not read
/// again, and refuses a record that is missing, a link, outside the harness's own folder
/// or started in another working folder.
#[test]
fn a_reader_names_its_conversation_and_a_resumed_one_reads_only_new_turns() {
    let case = Case::new();
    let sessions = case.path().join("sessions");
    let source = Transcript::CodexRollouts {
        sessions: sessions.clone(),
    };
    let started = r#"{"type":"event_msg","payload":{"type":"task_started"}}"#;
    let done =
        r#"{"type":"event_msg","payload":{"type":"task_complete","last_agent_message":"Done"}}"#;
    let turn = vec![
        TranscriptEvent::TurnStarted,
        TranscriptEvent::TurnFinished { success: true },
    ];
    let mut reader = TranscriptReader::new(source.clone(), &case.work(), SystemTime::now());
    assert_eq!(reader.conversation(), None);
    let path = rollout(
        &sessions,
        "2026/10/23",
        "rollout-mine.jsonl",
        &case.work(),
        &[started, done],
    );
    assert_eq!(reader.read().unwrap(), turn);
    assert_eq!(reader.conversation(), Some(("s".to_owned(), path.clone())));

    let mut resumed = TranscriptReader::resumed(source.clone(), &case.work(), &path, None).unwrap();
    assert_eq!(resumed.conversation(), Some(("s".to_owned(), path.clone())));
    assert_eq!(resumed.read().unwrap(), Vec::new());
    append(&path, &format!("{started}\n{done}\n"));
    assert_eq!(resumed.read().unwrap(), turn);

    let other = rollout(
        &sessions,
        "2026/10/23",
        "rollout-other.jsonl",
        &case.path().join("other"),
        &[started],
    );
    let outside = case.path().join("rollout-copy.jsonl");
    std::fs::copy(&path, &outside).unwrap();
    let link = sessions.join("2026/10/23/rollout-link.jsonl");
    std::os::unix::fs::symlink(&path, &link).unwrap();
    std::os::unix::fs::symlink(sessions.join("2026/10/23"), sessions.join("2026/10/24")).unwrap();
    for refused in [
        other,
        outside,
        link,
        sessions.join("2026/10/24/rollout-mine.jsonl"),
        sessions.join("2026/10/23/../23/rollout-mine.jsonl"),
        sessions.join("2026/10/23/rollout-gone.jsonl"),
    ] {
        assert_eq!(
            TranscriptReader::resumed(source.clone(), &case.work(), &refused, None).err(),
            Some(LaunchError("launch_resume_unavailable".to_owned(), None)),
            "{}",
            refused.display()
        );
    }

    let (file, mut reader) = case.claude();
    assert_eq!(reader.conversation(), None);
    let asked = r#"{"type":"user","message":{"role":"user","content":"Fix the build"}}"#;
    append(&file, &format!("{asked}\n"));
    assert_eq!(reader.read().unwrap(), vec![TranscriptEvent::TurnStarted]);
    assert_eq!(
        reader.conversation(),
        Some(("session".to_owned(), file.clone()))
    );
    let claude = Transcript::ClaudeSession { file: file.clone() };
    let mut resumed = TranscriptReader::resumed(claude.clone(), &case.work(), &file, None).unwrap();
    assert_eq!(resumed.read().unwrap(), Vec::new());
    append(&file, &format!("{asked}\n"));
    assert_eq!(resumed.read().unwrap(), vec![TranscriptEvent::TurnStarted]);
    assert_eq!(
        TranscriptReader::resumed(
            claude.clone(),
            &case.work(),
            &case.path().join("other.jsonl"),
            None
        )
        .err(),
        Some(LaunchError("launch_resume_unavailable".to_owned(), None))
    );
    let middle = resumed.start();
    assert!(middle > 0);
    let mut again =
        TranscriptReader::resumed(claude.clone(), &case.work(), &file, Some(middle)).unwrap();
    assert_eq!(again.start(), middle);
    assert_eq!(again.read().unwrap(), vec![TranscriptEvent::TurnStarted]);
    let past = std::fs::metadata(&file).unwrap().len() + 1;
    assert_eq!(
        TranscriptReader::resumed(claude, &case.work(), &file, Some(past)).err(),
        Some(LaunchError("launch_resume_unavailable".to_owned(), None))
    );
}

/// The typed messages and replies of a conversation are read from its record for a fresh
/// conversation to carry on, without tool output, interrupts, meta lines or the harness's
/// own context blocks, and nothing is read through a link.
#[test]
fn a_conversation_record_gives_its_typed_messages_and_replies() {
    let case = Case::new();
    let sessions = case.path().join("sessions");
    let path = rollout(
        &sessions,
        "2026/10/23",
        "rollout-mine.jsonl",
        &case.work(),
        &[
            r#"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"<environment_context>cwd</environment_context>"}]}}"#,
            r#"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"Write the release notes"}]}}"#,
            r#"{"type":"response_item","payload":{"type":"function_call","name":"shell"}}"#,
            r#"{"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"Notes drafted"}]}}"#,
            r#"{"type":"response_item","payload":{"type":"message","role":"developer","content":[{"type":"input_text","text":"rules"}]}}"#,
        ],
    );
    assert_eq!(
        conversation_messages("codex", &path),
        vec![
            (Speaker::Input, "Write the release notes".to_owned()),
            (Speaker::Agent, "Notes drafted".to_owned()),
        ]
    );
    let (file, _) = case.claude();
    append(
        &file,
        concat!(
            r#"{"type":"user","isMeta":true,"message":{"content":"meta"}}"#,
            "\n",
            r#"{"type":"user","message":{"content":"Fix the build"}}"#,
            "\n",
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Looking"},{"type":"tool_use","name":"Bash"}]}}"#,
            "\n",
            r#"{"type":"user","message":{"content":[{"type":"tool_result","content":"ok"}]}}"#,
            "\n",
            r#"{"type":"user","message":{"content":[{"type":"text","text":"[Request interrupted by user]"}]}}"#,
            "\n",
        ),
    );
    assert_eq!(
        conversation_messages("claude-code", &file),
        vec![
            (Speaker::Input, "Fix the build".to_owned()),
            (Speaker::Agent, "Looking".to_owned()),
        ]
    );
    let link = case.path().join("link.jsonl");
    std::os::unix::fs::symlink(&file, &link).unwrap();
    assert!(conversation_messages("claude-code", &link).is_empty());
    assert!(conversation_messages("codex", &case.path().join("gone.jsonl")).is_empty());
}

/// A FIFO where a transcript is expected is never opened for a blocking read: a pane's
/// reader, a resumed reader and the excerpt reader all return at once and take nothing.
#[test]
fn a_fifo_in_place_of_a_transcript_never_blocks_a_reader() {
    let case = Case::new();
    let (file, reader) = case.claude();
    let made = crate::spawn_gate::status(
        std::process::Command::new("/usr/bin/mkfifo")
            .arg(&file)
            .env_clear()
            .stdin(std::process::Stdio::null()),
    )
    .unwrap();
    assert!(made.success());
    let work = case.work();
    let (sent, got) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut reader = reader;
        let read = reader.read();
        let resumed = TranscriptReader::resumed(
            Transcript::ClaudeSession { file: file.clone() },
            &work,
            &file,
            None,
        )
        .err();
        let messages = conversation_messages("claude-code", &file);
        let _ = sent.send((read, reader.conversation(), resumed, messages));
    });
    let (read, named, resumed, messages) = got
        .recv_timeout(Duration::from_secs(10))
        .expect("a FIFO must not block a transcript reader");
    assert_eq!(read, Ok(Vec::new()));
    assert_eq!(named, None);
    assert_eq!(
        resumed,
        Some(LaunchError("launch_resume_unavailable".to_owned(), None))
    );
    assert!(messages.is_empty());
}

/// A record an agent fills with a million empty lines in its own home is read in one pass:
/// each read drains the lines it used once, not once per line, so the engine thread that
/// polls the record is never stalled, and the line after them is still read.
#[test]
fn a_read_of_a_million_short_lines_drains_them_in_one_pass() {
    let case = Case::new();
    let (file, mut reader) = case.claude();
    let prompt = r#"{"type":"user","message":{"role":"user","content":"Fix the build"}}"#;
    append(&file, &"\n".repeat(READ_BYTES_MAX - prompt.len() - 1));
    append(&file, &format!("{prompt}\n"));
    let (sent, read) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let started = std::time::Instant::now();
        let events = reader.read();
        let _ = sent.send((events, started.elapsed()));
    });
    let (events, took) = read
        .recv_timeout(SHORT_LINES_READ_TIME)
        .expect("a read of short lines stalls the engine thread");
    assert_eq!(events.unwrap(), vec![TranscriptEvent::TurnStarted]);
    assert!(took < SHORT_LINES_READ_TIME, "{took:?}");
}

/// A long line the harness has not finished is scanned once: a poll that finds no new
/// bytes does not rescan it, so a thousand idle polls of a 15 MiB partial line cost the
/// engine thread almost nothing, and the line still ends where the next newline arrives.
#[test]
fn a_parked_partial_line_is_not_rescanned_by_polls_without_new_bytes() {
    let case = Case::new();
    let (file, mut reader) = case.claude();
    let parked = LINE_BYTES_MAX - READ_BYTES_MAX;
    append(&file, &"x".repeat(parked));
    let (sent, read) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        while reader.bytes() < parked as u64 {
            assert_eq!(reader.read().unwrap(), Vec::new());
        }
        let started = std::time::Instant::now();
        for _ in 0..IDLE_POLLS {
            assert_eq!(reader.read().unwrap(), Vec::new());
        }
        let _ = sent.send((reader, started.elapsed()));
    });
    let (mut reader, took) = read
        .recv_timeout(PARKED_LINE_WAIT)
        .expect("idle polls rescan the parked line");
    assert!(
        took < IDLE_POLLS_TIME,
        "{IDLE_POLLS} idle polls took {took:?}"
    );
    let prompt = r#"{"type":"user","message":{"role":"user","content":"Fix the build"}}"#;
    append(&file, &format!("\n{prompt}\n"));
    assert_eq!(reader.read().unwrap(), vec![TranscriptEvent::TurnStarted]);
}
/// OP-25 OP25-2: a tool input nested as deep as a record line allows is formatted only up
/// to the bound the session search keeps, so it costs about what a flat input of the same
/// size costs, and its text is cut at that bound.
#[test]
fn a_deeply_nested_tool_input_is_formatted_only_up_to_its_bound() {
    let filler = "word ".repeat(200 * 1024);
    let line = |depth: usize| {
        let mut input = Value::String(filler.clone());
        for _ in 0..depth {
            input = serde_json::json!({ "a": input });
        }
        serde_json::json!({"type": "assistant", "message": {"role": "assistant", "content": [
            {"type": "tool_use", "id": "toolu_0D", "name": "Deep", "input": input}]}})
        .to_string()
    };
    let fastest = |line: &str| {
        (0..7)
            .map(|_| {
                let started = std::time::Instant::now();
                let (speaker, text) = record_message("claude-code", line.as_bytes()).unwrap();
                let took = started.elapsed();
                assert_eq!(speaker, "agent");
                assert!(text.len() <= TOOL_TEXT_BYTES_MAX + 64, "{}", text.len());
                assert!(text.ends_with("..."), "{}", &text[text.len() - 20..]);
                took
            })
            .min()
            .unwrap()
    };
    let flat = line(1);
    let deep = line(120);
    assert!(
        deep.len() <= crate::team::history::LINE_BYTES_MAX,
        "{}",
        deep.len()
    );
    let (flat_took, deep_took) = (fastest(&flat), fastest(&deep));
    assert!(
        deep_took <= flat_took * 3 + Duration::from_millis(1),
        "deep {deep_took:?}, flat {flat_took:?}"
    );
}

/// A Codex 0.164 command still running when exec_command yields is reported by its session
/// id, from the "Process running with session ID N" line of its tool output (PA-10), in the
/// text and the content-list output shapes; its end is the CommandExecution item Codex
/// writes once the process exits. A running item, an item without an id, an id that is not
/// a number and a poll of a command that already exited report nothing.
#[test]
fn codex_background_commands_are_followed_by_session_id() {
    use serde_json::json;
    let output = |output: serde_json::Value| json!({"type":"response_item","payload":{"type":"function_call_output","call_id":"c1","output":output}});
    let ended = |status: &str, id: serde_json::Value| {
        json!({"type":"event_msg","payload":{"type":"item_completed","thread_id":"t","turn_id":"u",
            "item":{"type":"CommandExecution","id":"c1","process_id":id,"command":["sleep","9"],
                "cwd":"/w","parsed_cmd":[],"source":"agent","status":status,"exit_code":0}}})
    };
    let started = |id: &str| TranscriptEvent::BackgroundStarted { id: id.into() };
    assert_eq!(
        codex_facts(&output(json!(
            "Chunk ID: 1\nWall time: 1.0002 seconds\nProcess running with session ID 4821\nOutput:\nserving"
        ))),
        vec![started("4821")]
    );
    assert_eq!(
        codex_facts(&output(json!([
            {"type":"input_text","text":"Wall time: 2.0 seconds\nProcess running with session ID 77"}
        ]))),
        vec![started("77")]
    );
    assert_eq!(
        codex_facts(&output(
            json!({"content":"Process running with session ID 9"})
        )),
        vec![started("9")]
    );
    for quiet in [
        output(json!(
            "Wall time: 1.0 seconds\nProcess exited with code 0\nOutput:\n"
        )),
        output(json!("Process running with session ID 48\u{7}21")),
        output(json!("Process running with session ID abc")),
        output(json!("echo Process running with session ID 5")),
        ended("in_progress", json!("4821")),
        ended("completed", serde_json::Value::Null),
        ended("completed", json!("x1")),
    ] {
        assert_eq!(codex_facts(&quiet), Vec::new(), "{quiet}");
    }
    for status in ["completed", "failed", "declined"] {
        assert_eq!(
            codex_facts(&ended(status, json!("4821"))),
            vec![TranscriptEvent::BackgroundEnded { id: "4821".into() }]
        );
    }
}

/// Both harnesses with tools report their background commands; only Claude Code starts a
/// turn of its own when one ends, so a Codex session must end a held turn itself (PA-10).
#[test]
fn codex_and_claude_records_report_background_commands() {
    let case = Case::new();
    let (_, claude) = case.claude();
    assert!(claude.reports_background());
    assert!(claude.notifies_background_end());
    let codex = TranscriptReader::new(
        Transcript::CodexRollouts {
            sessions: case.path(),
        },
        &case.work(),
        SystemTime::now(),
    );
    assert!(codex.reports_background());
    assert!(!codex.notifies_background_end());
    let none = TranscriptReader::new(Transcript::Unavailable, &case.work(), SystemTime::now());
    assert!(!none.reports_background());
    assert!(!none.notifies_background_end());
}

/// Background commands at task end maps to every harness kind (PA-10, DEC-141): a Claude
/// Code and a Codex record both report the commands their agent starts in the background,
/// so Wait, a grace time and Stop act the same on both; rcli runs no team agent and has no
/// tools, which the setting says. The match names every kind, so a new one fails to build
/// until it has a mapping.
#[test]
fn every_harness_kind_maps_background_commands_at_task_end() {
    use crate::coord::settings::HarnessKind;
    let case = Case::new();
    for kind in HarnessKind::ALL {
        match kind {
            HarnessKind::ClaudeCode => {
                let (_, reader) = case.claude();
                assert!(reader.reports_background());
            }
            HarnessKind::Codex => {
                let reader = TranscriptReader::new(
                    Transcript::CodexRollouts {
                        sessions: case.path(),
                    },
                    &case.work(),
                    SystemTime::now(),
                );
                assert!(reader.reports_background());
            }
            HarnessKind::Rcli => assert_eq!(crate::team::launch_harness("rcli"), None),
        }
    }
}
