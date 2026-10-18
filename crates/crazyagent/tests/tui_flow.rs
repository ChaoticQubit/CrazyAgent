use crazyagent::protocol::{Cursor, Outcome, RequestId, WireReply};
use crazyagent::tui::app::{App, Focus, UiEvent};
use crazyagent::tui::views::safe_text;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[test]
fn optimized_python_qualification_refuses_before_argument_parsing() {
    let output = Command::new("python3")
        .arg("-O")
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../scripts/qualification/tui_flow.py"
        ))
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert_eq!(
        String::from_utf8(output.stderr).unwrap(),
        "TUI qualification requires Python assertions enabled\n"
    );
}

#[test]
fn real_engine_and_tui_complete_the_local_pty_flow() {
    let output = Command::new("python3")
        .env_remove("PYTHONOPTIMIZE")
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../scripts/qualification/tui_flow.py"
        ))
        .arg(env!("CARGO_BIN_EXE_crazyagent"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn cli(arguments: &[&str]) -> std::process::Output {
    let root = tempfile::tempdir().unwrap();
    let stdout = std::fs::File::create(root.path().join("stdout")).unwrap();
    let stderr = std::fs::File::create(root.path().join("stderr")).unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_crazyagent"))
        .args(arguments)
        .stdin(Stdio::null())
        .stdout(stdout)
        .stderr(stderr)
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return std::process::Output {
                status,
                stdout: std::fs::read(root.path().join("stdout")).unwrap(),
                stderr: std::fs::read(root.path().join("stderr")).unwrap(),
            };
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("TUI CLI failed to exit within five seconds");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn main_help_advertises_the_real_tui_entrypoint() {
    let output = cli(&["--help"]);
    assert!(output.status.success());
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains("crazyagent tui SOCKET"), "{text}");
}

#[test]
fn tui_help_is_available_without_a_terminal_or_engine() {
    let output = cli(&["tui", "--help"]);
    assert!(output.status.success(), "{:?}", output);
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains("crazyagent tui SOCKET"), "{text}");
}

#[test]
fn tui_rejects_a_pipe_without_changing_terminal_modes() {
    let output = cli(&["tui", "/unavailable/engine.sock"]);
    assert!(!output.status.success());
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("requires terminal")
    );
    assert!(!output.stdout.contains(&0x1b));
}

#[test]
fn drafts_and_scroll_remain_bounded_under_untrusted_input() {
    let mut app = App::new();
    for _ in 0..2000 {
        app.reduce(UiEvent::Character('界')).unwrap();
    }
    for character in ['\0', '\u{1b}', '\u{9b}', '\r', '\n'] {
        app.reduce(UiEvent::Character(character)).unwrap();
    }
    assert_eq!(app.draft().len(), 4095);
    assert!(!app.draft().chars().any(char::is_control));
    app.reduce(UiEvent::Scroll(i16::MAX)).unwrap();
    app.reduce(UiEvent::Scroll(i16::MAX)).unwrap();
    app.reduce(UiEvent::Scroll(i16::MAX)).unwrap();
    assert_eq!(app.scroll, u16::MAX);
    app.reduce(UiEvent::Resize(u16::MAX, u16::MAX)).unwrap();
    assert_eq!(app.viewport(), (1000, 1000));
}

#[test]
fn project_creation_queues_exactly_one_typed_command() {
    let mut app = App::new();
    app.reduce(UiEvent::FocusNext).unwrap();
    app.reduce(UiEvent::BeginProject).unwrap();
    for character in "界-project".chars() {
        app.reduce(UiEvent::Character(character)).unwrap();
    }
    app.reduce(UiEvent::Submit).unwrap();
    assert!(app.reduce(UiEvent::Submit).is_err());
    let Some(crazyagent::protocol::Command::CreateProject { spec, .. }) = app.take_command() else {
        panic!("expected the user's project creation");
    };
    assert_eq!(spec.name, "界-project");
    assert!(spec.repositories.is_empty());
    assert!(app.take_command().is_none());
}

#[test]
fn central_conversation_retains_draft_and_never_forges_a_worker_command() {
    let mut app = App::new();
    app.reduce(UiEvent::Character('界')).unwrap();
    app.reduce(UiEvent::Submit).unwrap();
    assert_eq!(app.draft(), "界");
    assert!(!app.has_command());
    assert!(app.status.contains("owner API"));
    for _ in 0..7 {
        app.reduce(UiEvent::FocusNext).unwrap();
    }
    assert_eq!(app.focus(), Focus::Conversation);
}

#[test]
fn cancelling_project_creation_preserves_the_leader_draft() {
    let mut app = App::new();
    app.reduce(UiEvent::Character('界')).unwrap();
    app.reduce(UiEvent::FocusNext).unwrap();
    app.reduce(UiEvent::BeginProject).unwrap();
    app.reduce(UiEvent::Character('P')).unwrap();
    app.reduce(UiEvent::Cancel).unwrap();
    assert_eq!(app.draft(), "界");
    assert!(!app.has_command());
}

#[test]
fn focus_returns_to_the_single_central_conversation() {
    let mut app = App::new();
    assert_eq!(app.focus(), Focus::Conversation);
    app.reduce(UiEvent::FocusNext).unwrap();
    assert_eq!(app.focus(), Focus::Projects);
    app.reduce(UiEvent::FocusPrevious).unwrap();
    assert_eq!(app.focus(), Focus::Conversation);
}

#[test]
fn draft_backspace_preserves_unicode_boundaries() {
    let mut app = App::new();
    app.reduce(UiEvent::Character('界')).unwrap();
    app.reduce(UiEvent::Character('🦀')).unwrap();
    assert_eq!(app.draft(), "界🦀");
    app.reduce(UiEvent::Backspace).unwrap();
    assert_eq!(app.draft(), "界");
}

#[test]
fn zero_size_resize_is_a_valid_terminal_event() {
    let mut app = App::new();
    app.reduce(UiEvent::Resize(0, 0)).unwrap();
    assert_eq!(app.viewport(), (0, 0));
    app.reduce(UiEvent::Resize(80, 24)).unwrap();
    assert_eq!(app.viewport(), (80, 24));
}

#[test]
fn late_reply_cannot_regress_the_visible_revision() {
    let mut app = App::new();
    app.apply_reply(WireReply {
        request_id: RequestId::new("newer-read").unwrap(),
        revision: Cursor::new(9),
        outcome: Outcome::Snapshot {
            projects: vec![],
            tasks: vec![],
        },
    })
    .unwrap();
    app.apply_reply(WireReply {
        request_id: RequestId::new("older-read").unwrap(),
        revision: Cursor::new(2),
        outcome: Outcome::Snapshot {
            projects: vec![],
            tasks: vec![],
        },
    })
    .unwrap();
    assert_eq!(app.revision(), Cursor::new(9));
}

#[test]
fn transcript_data_cannot_emit_terminal_controls() {
    let rendered = safe_text("界\u{1b}]52;c;c2VjcmV0\u{7}\u{9b}31m\r\u{0}text", 4096);
    assert!(rendered.starts_with('界'));
    assert!(rendered.ends_with("text"));
    assert!(!rendered.contains('\u{1b}'));
    assert!(!rendered.contains('\u{9b}'));
    assert!(!rendered.contains('\u{7}'));
    assert!(!rendered.contains('\r'));
    assert!(!rendered.contains('\u{0}'));
}

#[test]
fn transcript_limit_never_splits_a_unicode_scalar() {
    let rendered = safe_text("界🦀tail", 6);
    assert_eq!(rendered, "界");
    assert!(rendered.len() <= 6);
}

fn wire(
    id: &str,
    revision: u64,
    command: crazyagent::protocol::Command,
) -> crazyagent::protocol::WireRequest {
    crazyagent::protocol::WireRequest {
        protocol_version: crazyagent::protocol::PROTOCOL_VERSION,
        client_id: crazyagent::protocol::ClientId::new("tui-data-check").unwrap(),
        request_id: RequestId::new(id).unwrap(),
        expected_revision: Cursor::new(revision),
        command,
    }
}

#[tokio::test]
async fn project_completion_matches_actual_request_and_submitted_generation() {
    use crazyagent::engine::Engine;
    use crazyagent::protocol::Command as EngineCommand;
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let config =
        crazyagent::config::resolve_config(&root.path().join("g"), &root.path().join("p")).unwrap();
    let engine = Engine::open(&root.path().join("db"), config).await.unwrap();
    let mut revision = 0;
    for (index, mode) in ["unchanged", "edited", "identical-replacement", "rejected"]
        .into_iter()
        .enumerate()
    {
        let mut app = App::new();
        app.reduce(UiEvent::FocusNext).unwrap();
        app.reduce(UiEvent::BeginProject).unwrap();
        app.reduce(UiEvent::Character('A')).unwrap();
        app.reduce(UiEvent::Submit).unwrap();
        let command = app.take_command().unwrap();
        let request_id = format!("generation-{index}");
        app.bind_project_request(RequestId::new(&request_id).unwrap());
        let reply = engine
            .apply(wire(
                &request_id,
                if mode == "rejected" {
                    revision + 1
                } else {
                    revision
                },
                command,
            ))
            .await
            .unwrap();
        revision = reply.revision.value();
        if mode == "edited" {
            app.reduce(UiEvent::Character('B')).unwrap();
        }
        if mode == "identical-replacement" {
            app.reduce(UiEvent::Cancel).unwrap();
            app.reduce(UiEvent::BeginProject).unwrap();
            app.reduce(UiEvent::Character('A')).unwrap();
        }
        app.apply_reply(reply.clone()).unwrap();
        if mode == "unchanged" {
            assert!(!app.is_creating_project());
            assert!(app.draft().is_empty());
        } else {
            assert!(app.is_creating_project());
            assert_eq!(app.draft(), if mode == "edited" { "AB" } else { "A" });
        }
        if !app.is_creating_project() {
            app.reduce(UiEvent::BeginProject).unwrap();
            app.reduce(UiEvent::Character('B')).unwrap();
        }
        app.reduce(UiEvent::Submit).unwrap();
        let next = app.take_command().unwrap();
        let next_id = format!("next-{index}");
        app.bind_project_request(RequestId::new(&next_id).unwrap());
        let before = app.draft().to_owned();
        app.apply_reply(reply).unwrap();
        assert_eq!(app.draft(), before);
        assert!(app.reduce(UiEvent::Submit).is_err());
        let next_reply = engine.apply(wire(&next_id, revision, next)).await.unwrap();
        revision = next_reply.revision.value();
        app.apply_reply(next_reply).unwrap();
        assert!(!app.is_creating_project());
    }
    assert!(matches!(
        engine
            .apply(wire("read", 0, EngineCommand::Snapshot))
            .await
            .unwrap()
            .outcome,
        Outcome::Snapshot { .. }
    ));
    engine.close().await.unwrap();
}

#[tokio::test]
async fn real_wire_data_preserves_selection_and_scopes_configuration() {
    use crazyagent::engine::{Client, Engine, Server};
    use crazyagent::protocol::{
        BaselineId, Command as EngineCommand, CredentialRef, ExecutionContext, ProjectId,
        ProjectSpec, SnapshotId, TaskId,
    };
    use std::os::unix::fs::PermissionsExt;

    let root = tempfile::tempdir().unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::write(root.path().join("project.toml"), "timeout_ms = 1500").unwrap();
    let config = crazyagent::config::resolve_config(
        &root.path().join("global.toml"),
        &root.path().join("project.toml"),
    )
    .unwrap();
    let engine = std::sync::Arc::new(
        Engine::open(&root.path().join("engine.db"), config.clone())
            .await
            .unwrap(),
    );
    let socket = root.path().join("engine.sock");
    let server = Server::bind(&socket, engine.clone(), &config).unwrap();
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let service = tokio::spawn(server.run(stopped));
    let client = Client::new(socket, Duration::from_secs(1)).unwrap();
    for (index, name) in ["a", "z"].into_iter().enumerate() {
        let reply = client
            .request(wire(
                &format!("project-{name}"),
                index as u64,
                EngineCommand::CreateProject {
                    project_id: ProjectId::new(name).unwrap(),
                    spec: ProjectSpec {
                        name: format!("{name}-real-project"),
                        repositories: Vec::new(),
                    },
                },
            ))
            .await
            .unwrap();
        assert_eq!(reply.outcome, Outcome::Applied);
    }
    let context = ExecutionContext {
        project_id: ProjectId::new("z").unwrap(),
        task_id: TaskId::new("z-task").unwrap(),
        config_id: config.id.clone(),
        snapshot_id: SnapshotId::new("source-snapshot").unwrap(),
        baseline_id: BaselineId::new("unexecuted-baseline").unwrap(),
        allowed_destinations: Vec::new(),
        budget_ref: CredentialRef::new("unspent-budget-reference").unwrap(),
    };
    let reply = client
        .request(wire("pin", 2, EngineCommand::PinTask { context }))
        .await
        .unwrap();
    assert_eq!(reply.outcome, Outcome::Applied);
    let mut app = App::new();
    app.apply_reply(
        client
            .request(wire("snapshot", 0, EngineCommand::Snapshot))
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(app.selected_project().unwrap().project_id.as_str(), "a");
    app.reduce(UiEvent::SelectionNext).unwrap();
    assert_eq!(app.selected_task().unwrap().task_id.as_str(), "z-task");
    app.reduce(UiEvent::FocusNext).unwrap();
    app.reduce(UiEvent::FocusNext).unwrap();
    app.apply_reply(
        client
            .request(wire("configuration", 0, app.refresh_command()))
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(app.config.as_ref().unwrap().id, config.id);
    let rendered = crazyagent::tui::views::body(&app);
    assert!(
        rendered.contains("timeout_ms = 1500 | Project"),
        "{rendered}"
    );
    app.apply_reply(
        client
            .request(wire("refresh", 0, EngineCommand::Snapshot))
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(app.selected_project().unwrap().project_id.as_str(), "z");
    app.reduce(UiEvent::SelectionPrevious).unwrap();
    assert!(app.config.is_none());
    assert!(app.selected_task().is_none());
    assert!(!crazyagent::tui::views::body(&app).contains("timeout_ms = 1500"));
    while app.focus() != Focus::Conversation {
        app.reduce(UiEvent::FocusNext).unwrap();
    }
    app.reduce(UiEvent::Character('界')).unwrap();
    app.reduce(UiEvent::FocusNext).unwrap();
    app.reduce(UiEvent::BeginProject).unwrap();
    app.reduce(UiEvent::Character('P')).unwrap();
    app.reduce(UiEvent::Submit).unwrap();
    let command = app.take_command().unwrap();
    app.bind_project_request(RequestId::new("preserve-draft").unwrap());
    let created = client
        .request(wire("preserve-draft", app.revision().value(), command))
        .await
        .unwrap();
    assert_eq!(created.outcome, Outcome::Applied);
    app.apply_reply(created).unwrap();
    assert_eq!(app.draft(), "界");
    assert!(!app.is_creating_project());
    stop.send(()).unwrap();
    service.await.unwrap().unwrap();
    engine.close().await.unwrap();
}
