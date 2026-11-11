use super::*;
use crate::master_session::{MasterTurnPage, MasterTurnPhase, MasterTurnView};
use crate::protocol::{ConfigId, Cursor, ProjectId};
use serde_json::json;

fn screen() -> Screen {
    Screen::new(
        Path::new("/w/tree"),
        Path::new("/p/repo"),
        &[PathBuf::from("/p/repo/client-x")],
    )
}

fn tool_use(name: &str, input: Value) -> EventKind {
    EventKind::ToolUse {
        message_id: "message-1".into(),
        parent_tool_use_id: None,
        tool_use_id: "tool-1".into(),
        name: name.into(),
        input,
    }
}

fn notification(method: &str) -> EventKind {
    EventKind::Unsupported {
        message_type: "notification".into(),
        subtype: Some(method.into()),
    }
}

fn turn(ordinal: u64, phase: MasterTurnPhase) -> MasterTurnView {
    MasterTurnView {
        id: RequestId::new(format!("turn-{ordinal}")).unwrap(),
        session_id: SessionId::new("session-1").unwrap(),
        ordinal,
        input_cursor: Cursor::new(ordinal),
        phase,
        reason: None,
        attempt: None,
        runtime: None,
        reply: None,
        accounts: Vec::new(),
        tasks: Vec::new(),
        changes: None,
        engine_request: None,
        activity: Vec::new(),
        detail: None,
        cause: None,
    }
}

#[test]
fn a_file_read_or_edit_names_its_path_below_the_worktree() {
    let screen = screen();
    let label = |name: &str, input: Value| screen.label(&tool_use(name, input));
    assert_eq!(
        label("Read", json!({"file_path": "/w/tree/src/app/page.tsx"})).as_deref(),
        Some("Reading src/app/page.tsx")
    );
    assert_eq!(
        label("Edit", json!({"file_path": "docs/plan.md"})).as_deref(),
        Some("Editing docs/plan.md")
    );
    assert_eq!(
        label(
            "NotebookEdit",
            json!({"notebook_path": "/w/tree/nb/run.ipynb"})
        )
        .as_deref(),
        Some("Editing nb/run.ipynb")
    );
    let longest = format!("{}.md", "a".repeat(PATH_BYTES_MAX - 3));
    assert_eq!(
        label("Write", json!({"file_path": longest})),
        Some(format!("Editing {longest}"))
    );
}

#[test]
fn a_path_that_could_tell_a_secret_or_leave_the_worktree_is_never_named() {
    let screen = screen();
    let refused = [
        json!({"file_path": "/w/tree/.env"}),
        json!({"file_path": "/w/tree/config/credentials.json"}),
        json!({"file_path": "/w/tree/keys/id_rsa_token.txt"}),
        json!({"file_path": "/Users/someone/Library/Application Support/CrazyAgent/x.md"}),
        json!({"file_path": "/w/treehouse/x.md"}),
        json!({"file_path": "/w/tree"}),
        json!({"file_path": "../outside.md"}),
        json!({"file_path": "docs/../../outside.md"}),
        json!({"file_path": "docs//x.md"}),
        json!({"file_path": "docs/./x.md"}),
        json!({"file_path": "/w/tree/.git/config"}),
        json!({"file_path": "/w/tree/sub/.GIT/HEAD"}),
        json!({"file_path": "/w/tree/.CrazyAgent/context/brief.md"}),
        json!({"file_path": "/w/tree/client-x/contract.md"}),
        json!({"file_path": "/w/tree/Client-X/notes/a.md"}),
        json!({"file_path": "/w/tree/docs/bell\u{7}.md"}),
        json!({"file_path": format!("{}.md", "a".repeat(PATH_BYTES_MAX - 2))}),
        json!({"file_path": 7}),
        json!({"omitted_bytes": 90000}),
        json!({}),
    ];
    for input in refused {
        assert_eq!(
            screen.label(&tool_use("Read", input.clone())).as_deref(),
            Some("Reading a file"),
            "{input}"
        );
    }
    assert_eq!(
        screen
            .label(&tool_use(
                "Edit",
                json!({"file_path": "/w/tree/.env.local"})
            ))
            .as_deref(),
        Some("Editing a file")
    );
    let outside = Screen::new(
        Path::new("/w/tree"),
        Path::new("/p/repo"),
        &[PathBuf::from("/elsewhere/folder")],
    );
    assert_eq!(
        outside
            .label(&tool_use("Read", json!({"file_path": "/w/tree/src/x.ts"})))
            .as_deref(),
        Some("Reading a file")
    );
    let whole = Screen::new(
        Path::new("/w/tree"),
        Path::new("/p/repo"),
        &[PathBuf::from("/p/repo")],
    );
    assert_eq!(
        whole
            .label(&tool_use("Read", json!({"file_path": "/w/tree/src/x.ts"})))
            .as_deref(),
        Some("Reading a file")
    );
}

#[test]
fn commands_patterns_urls_text_and_unknown_tools_become_fixed_phrases() {
    let screen = screen();
    let cases = [
        (
            tool_use(
                "Bash",
                json!({"command": "export TOKEN=abc123 && cargo test"}),
            ),
            "Running a command",
        ),
        (
            tool_use(
                "Grep",
                json!({"pattern": "password=", "path": "/w/tree/src"}),
            ),
            "Searching the code",
        ),
        (
            tool_use("Glob", json!({"pattern": "**/*.pem"})),
            "Searching the code",
        ),
        (
            tool_use("WebFetch", json!({"url": "https://x.test/?key=abc123"})),
            "Reading the web",
        ),
        (
            tool_use("Task", json!({"prompt": "read ~/.ssh"})),
            "Starting a sub-agent",
        ),
        (
            tool_use("TodoWrite", json!({"todos": [{"content": "abc123"}]})),
            "Updating its plan",
        ),
        (
            tool_use("mcp__other-server__steal_abc123", json!({})),
            "Using a tool",
        ),
        (
            tool_use("mcp__crazyagent-team__delegate", json!({"task": "abc123"})),
            "Handing a task to the team",
        ),
        (
            tool_use("mcp__crazyagent-team__recall", json!({"query": "abc123"})),
            "Recalling project memory",
        ),
        (
            tool_use("mcp__crazyagent-team__ask", json!({"question": "abc123"})),
            "Asking you a question",
        ),
        (
            EventKind::ToolUse {
                message_id: "message-2".into(),
                parent_tool_use_id: Some("tool-0".into()),
                tool_use_id: "tool-2".into(),
                name: "Read".into(),
                input: json!({"file_path": "/w/tree/src/abc123.ts"}),
            },
            "A sub-agent is working",
        ),
        (
            EventKind::AssistantText {
                message_id: "message-3".into(),
                parent_tool_use_id: Some("tool-0".into()),
                text: "abc123".into(),
            },
            "A sub-agent is working",
        ),
        (
            EventKind::AssistantDelta {
                item_id: "item-1".into(),
                text: "the key is abc123".into(),
            },
            "Writing a reply",
        ),
        (
            EventKind::AssistantText {
                message_id: "message-4".into(),
                parent_tool_use_id: None,
                text: "abc123".into(),
            },
            "Writing a reply",
        ),
        (
            EventKind::TurnError {
                message: "abc123".into(),
                will_retry: true,
            },
            "Retrying after an error",
        ),
    ];
    for (event, expected) in cases {
        let label = screen.label(&event).unwrap();
        assert_eq!(label, expected, "{event:?}");
        assert!(!label.contains("abc123"), "{event:?}");
    }
}

#[test]
fn codex_methods_and_claude_thinking_map_to_fixed_phrases_and_bookkeeping_to_none() {
    let screen = screen();
    for (method, expected) in [
        ("item/commandExecution/outputDelta", "Running a command"),
        ("item/fileChange/outputDelta", "Editing files"),
        ("item/reasoning/summaryTextDelta", "Thinking"),
        ("item/reasoning/textDelta", "Thinking"),
        ("turn/plan/updated", "Updating its plan"),
        ("native_sub_agent", "Starting a sub-agent"),
        ("item/started", "Working"),
        ("item/completed", "Working"),
        ("turn/diff/updated", "Working"),
    ] {
        assert_eq!(
            screen.label(&notification(method)).as_deref(),
            Some(expected),
            "{method}"
        );
    }
    for method in [
        "thread/tokenUsage/updated",
        "account/rateLimits/updated",
        "abc123/secret",
    ] {
        assert_eq!(screen.label(&notification(method)), None, "{method}");
    }
    assert_eq!(
        screen
            .label(&EventKind::Unsupported {
                message_type: "assistant.content".into(),
                subtype: Some("thinking".into()),
            })
            .as_deref(),
        Some("Thinking")
    );
    for event in [
        EventKind::Unsupported {
            message_type: "user".into(),
            subtype: None,
        },
        EventKind::Unsupported {
            message_type: "system".into(),
            subtype: Some("item/started".into()),
        },
        EventKind::TurnStarted,
        EventKind::ThreadStarted,
        EventKind::TurnError {
            message: "abc123".into(),
            will_retry: false,
        },
    ] {
        assert_eq!(screen.label(&event), None, "{event:?}");
    }
}

#[test]
fn a_turn_keeps_its_last_twenty_lines_and_a_repeat_only_moves_its_time() {
    let board = Board::default();
    let session = SessionId::new("session-1").unwrap();
    let turn = RequestId::new("turn-1").unwrap();
    let watch = board.watch(&session, &turn).unwrap();
    for index in 0..25u64 {
        watch.record(format!("Reading src/{index}.ts"), index);
    }
    let lines = board.lines(&session, &turn);
    assert_eq!(lines.len(), LINES_MAX);
    assert_eq!(lines[0].text, "Reading src/5.ts");
    assert_eq!(lines[LINES_MAX - 1].text, "Reading src/24.ts");
    watch.record("Reading src/24.ts".into(), 90);
    let lines = board.lines(&session, &turn);
    assert_eq!(lines.len(), LINES_MAX);
    assert_eq!(lines[LINES_MAX - 1].at_ms, 90);
    watch.record("Writing a reply".into(), 91);
    watch.record("Writing a reply".into(), 95);
    let lines = board.lines(&session, &turn);
    assert_eq!(lines[0].text, "Reading src/6.ts");
    assert_eq!(
        lines[LINES_MAX - 1],
        MasterActivityLine {
            at_ms: 95,
            text: "Writing a reply".into(),
        }
    );
    assert!(
        board
            .lines(&SessionId::new("session-2").unwrap(), &turn)
            .is_empty()
    );
    drop(watch);
    assert!(board.lines(&session, &turn).is_empty());
}

#[test]
fn the_board_refuses_a_turn_past_its_bound_and_frees_a_place_when_a_turn_ends() {
    let board = Board::default();
    let session = SessionId::new("session-1").unwrap();
    let mut watches: Vec<Watch<'_>> = (0..TURNS_MAX)
        .map(|index| {
            board
                .watch(&session, &RequestId::new(format!("turn-{index}")).unwrap())
                .unwrap()
        })
        .collect();
    let late = RequestId::new("turn-late").unwrap();
    assert!(board.watch(&session, &late).is_none());
    watches.pop();
    let watch = board.watch(&session, &late).unwrap();
    watch.record("Thinking".into(), 1);
    assert_eq!(board.lines(&session, &late).len(), 1);
}

#[test]
fn a_read_of_master_turns_gets_lines_on_its_outstanding_turn_only() {
    let board = Board::default();
    let session = SessionId::new("session-1").unwrap();
    let running = turn(2, MasterTurnPhase::Running);
    let settled = turn(1, MasterTurnPhase::Succeeded);
    let watch = board.watch(&session, &running.id).unwrap();
    let ended = board.watch(&session, &settled.id).unwrap();
    watch.record("Reading src/x.ts".into(), 7);
    ended.record("Writing a reply".into(), 3);
    let read = OwnerReadReply {
        revision: Cursor::new(4),
        config_id: ConfigId::new("c".repeat(64)).unwrap(),
        data: OwnerData::ProjectMasterTurns {
            page: MasterTurnPage {
                project_id: ProjectId::new("alpha").unwrap(),
                session_id: session.clone(),
                turns: vec![settled, running],
                next: None,
            },
        },
    };
    let OwnerData::ProjectMasterTurns { page } = with_lines(&board, read).data else {
        panic!("turns read");
    };
    assert!(page.turns[0].activity.is_empty());
    assert_eq!(
        page.turns[1].activity,
        vec![MasterActivityLine {
            at_ms: 7,
            text: "Reading src/x.ts".into(),
        }]
    );
    let other = OwnerReadReply {
        revision: Cursor::new(4),
        config_id: ConfigId::new("c".repeat(64)).unwrap(),
        data: OwnerData::ResyncRequired,
    };
    assert_eq!(with_lines(&board, other.clone()), other);
}
