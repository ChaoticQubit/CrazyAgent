#![cfg(unix)]

mod engine_support;

use crazyagent::engine::bootstrap::EngineService;
use crazyagent::project_setup::{HarnessSlot, ProjectSetup, TeamPlan};
use crazyagent::protocol::ProjectSpec;
use crazyagent::team::{EdgeState, NodeKind, TaskSource};
use crazyagent::terminal::{
    AgentId, INPUT_BYTES_MAX, SIZE_MAX, SIZE_MIN, TerminalFrame, TerminalRequest,
};
use crazyagent_desktop::bridge::terminal::{
    Terminals, agent, channel_message, plain_code, request_valid,
};
use crazyagent_desktop::bridge::{ActionOutcome, NativeBridge};
use std::error::Error;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::Duration;
use tauri::ipc::InvokeResponseBody;

const COMMANDS: [&str; 5] = [
    "desktop_team_graph",
    "desktop_terminal_attach",
    "desktop_terminal_input",
    "desktop_terminal_resize",
    "desktop_terminal_detach",
];

#[test]
fn graph_and_terminal_commands_are_registered_in_all_four_places() {
    let build = include_str!("../build.rs");
    let bindings = include_str!("../src/lib.rs");
    let capability = include_str!("../capabilities/main-window.json");
    let client = include_str!("../../src/engine-client.ts");
    for command in COMMANDS {
        assert_eq!(
            build.matches(&format!("\"{command}\"")).count(),
            1,
            "{command} build"
        );
        assert!(
            bindings.contains(&format!("fn {command}(")),
            "{command} binding"
        );
        assert_eq!(
            bindings
                .lines()
                .filter(|line| line.trim() == format!("{command},"))
                .count(),
            1,
            "{command} handler"
        );
        let permission = format!("\"allow-{}\"", command.replace('_', "-"));
        assert_eq!(
            capability.matches(&permission).count(),
            1,
            "{command} capability"
        );
        assert_eq!(
            client.matches(&format!("\"{command}\"")).count(),
            1,
            "{command} client"
        );
    }
}

#[test]
fn terminal_names_from_the_window_are_checked_before_use() {
    assert_eq!(agent("team-3").unwrap().as_str(), "team-3");
    for bad in [
        "",
        "Team-3",
        "team 3",
        "-team",
        "team-",
        "../team-3",
        &"a".repeat(65),
    ] {
        assert_eq!(
            agent(bad).unwrap_err().code,
            "terminal_agent_invalid",
            "{bad}"
        );
    }
}

#[test]
fn only_plain_engine_codes_reach_the_window() {
    assert_eq!(
        plain_code("terminal_agent_unknown", "x"),
        "terminal_agent_unknown"
    );
    assert_eq!(plain_code("exited", "x"), "exited");
    for raw in [
        "",
        "Exited",
        "pane %3 died",
        "code-1",
        "a\nb",
        &"a".repeat(65),
    ] {
        assert_eq!(
            plain_code(raw, "terminal_closed"),
            "terminal_closed",
            "{raw:?}"
        );
    }
}

#[test]
fn only_input_and_resize_inside_the_engine_limits_are_sent() {
    let input = |len: usize| TerminalRequest::Input {
        bytes: vec![b'a'; len],
    };
    assert!(request_valid(&input(1)));
    assert!(request_valid(&input(INPUT_BYTES_MAX)));
    assert!(!request_valid(&input(0)));
    assert!(!request_valid(&input(INPUT_BYTES_MAX + 1)));
    let resize = |cols, rows| TerminalRequest::Resize { cols, rows };
    assert!(request_valid(&resize(SIZE_MIN, SIZE_MAX)));
    assert!(request_valid(&resize(80, 24)));
    assert!(!request_valid(&resize(SIZE_MIN - 1, 24)));
    assert!(!request_valid(&resize(80, SIZE_MAX + 1)));
    assert!(!request_valid(&TerminalRequest::Detach));
    let attach = TerminalRequest::Attach {
        agent_id: AgentId::team(1).unwrap(),
    };
    assert!(!request_valid(&attach));
}

fn text(message: &InvokeResponseBody) -> &str {
    match message {
        InvokeResponseBody::Json(text) => text,
        InvokeResponseBody::Raw(_) => panic!("expected a control frame"),
    }
}

#[test]
fn output_goes_to_the_window_as_raw_bytes_and_control_frames_as_json() {
    let team = AgentId::team(2).unwrap();
    let screen = b"\x1b[1mcodex\x1b[0m ready\r\n".to_vec();
    let (message, last) = channel_message(
        &team,
        TerminalFrame::Output {
            bytes: screen.clone(),
        },
    );
    assert!(matches!(message, InvokeResponseBody::Raw(ref bytes) if *bytes == screen));
    assert!(!last);

    let (message, last) = channel_message(
        &team,
        TerminalFrame::Attached {
            agent_id: team.clone(),
            cols: 120,
            rows: 32,
        },
    );
    assert_eq!(
        text(&message),
        r#"{"kind":"attached","agent_id":"team-2","cols":120,"rows":32}"#
    );
    assert!(!last);

    let (message, last) = channel_message(
        &team,
        TerminalFrame::Attached {
            agent_id: AgentId::team(9).unwrap(),
            cols: 80,
            rows: 24,
        },
    );
    assert_eq!(
        text(&message),
        r#"{"kind":"closed","reason":"terminal_agent_mismatch"}"#
    );
    assert!(last);

    let (message, last) = channel_message(
        &team,
        TerminalFrame::Closed {
            reason: "pane exited with /Users/someone/secret".into(),
        },
    );
    assert_eq!(
        text(&message),
        r#"{"kind":"closed","reason":"terminal_closed"}"#
    );
    assert!(last);

    let (message, last) = channel_message(
        &team,
        TerminalFrame::Rejected {
            code: "terminal_agent_unknown".into(),
        },
    );
    assert_eq!(
        text(&message),
        r#"{"kind":"rejected","code":"terminal_agent_unknown"}"#
    );
    assert!(last);
}

#[tokio::test]
async fn typing_into_a_terminal_without_a_view_is_refused_and_detach_is_idempotent() {
    let terminals = Terminals::default();
    let team = AgentId::team(4).unwrap();
    terminals.room(&team).unwrap();
    let typed = terminals
        .send(
            &team,
            TerminalRequest::Input {
                bytes: b"ls\r".to_vec(),
            },
        )
        .await;
    assert_eq!(typed.unwrap_err().code, "terminal_not_attached");
    let empty = terminals
        .send(&team, TerminalRequest::Input { bytes: vec![] })
        .await;
    assert_eq!(empty.unwrap_err().code, "terminal_request_invalid");
    let resized = terminals
        .send(&team, TerminalRequest::Resize { cols: 0, rows: 24 })
        .await;
    assert_eq!(resized.unwrap_err().code, "terminal_request_invalid");
    terminals.detach(&team).unwrap();
    terminals.detach(&team).unwrap();
}

fn setup() -> ProjectSetup {
    ProjectSetup {
        about: "Splits chores between helpers.".into(),
        tech_stack: vec!["rust".into()],
        platforms: vec![],
        research: vec![],
        pool: None,
        team: Some(TeamPlan {
            master: "codex".into(),
            master_tools: false,
            sub_harnesses: vec![HarnessSlot {
                harness: "codex".into(),
                agents: 2,
                tools: true,
            }],
            min_agents: 1,
            max_agents: 3,
        }),
        documents: vec![],
    }
}

async fn flow(root: &Path) -> Result<(), Box<dyn Error>> {
    let mut bridge = NativeBridge::new(Some(root.join("e.sock")));
    assert_eq!(
        bridge.team_graph("chores").await.unwrap_err().code,
        "not_connected"
    );
    assert!(bridge.connect().await?.connected);
    let _home = engine_support::pick_home(&mut bridge, 1).await;
    let spec = ProjectSpec {
        name: "Chores".into(),
        repositories: vec![],
    };
    let created = bridge.setup_project(spec, setup(), 1).await?;
    assert!(
        matches!(created.outcome, ActionOutcome::Applied),
        "{created:?}"
    );
    let id = created.intent.project_id.clone().unwrap();

    let graph = bridge.team_graph(&id).await?;
    assert_eq!(graph.project_id.as_str(), id);
    let masters: Vec<_> = graph
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Master)
        .collect();
    assert_eq!(masters.len(), 1);
    assert_eq!(masters[0].harness, "codex");
    assert_eq!(masters[0].parent, None);
    let groups: Vec<_> = graph
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::HarnessGroup)
        .collect();
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].harness, "codex");
    assert_eq!(groups[0].parent.as_deref(), Some(masters[0].id.as_str()));
    assert!(groups[0].agents.is_some());
    assert!(graph.nodes.iter().all(|n| n.kind != NodeKind::Agent));
    assert!(graph.edges.is_empty());

    let handed = bridge
        .team_delegate(&id, Some("codex".into()), "Write chores.md".into(), 2)
        .await?;
    assert!(
        matches!(handed.outcome, ActionOutcome::Applied),
        "{handed:?}"
    );
    let graph = bridge.team_graph(&id).await?;
    assert_eq!(graph.edges.len(), 1);
    let edge = &graph.edges[0];
    assert_eq!(edge.from, masters[0].id);
    assert_eq!(edge.to, groups[0].id);
    assert_eq!(edge.source, TaskSource::Owner);
    assert_eq!(edge.state, EdgeState::Sent);
    assert_eq!(edge.text, "Write chores.md");
    assert_eq!(edge.reply, None);

    let unknown = bridge
        .open_terminal(AgentId::team(99)?)
        .await
        .err()
        .map(|e| e.code);
    assert_eq!(unknown, Some("terminal_agent_unknown"));
    assert_eq!(
        bridge.team_graph("absent-project").await.unwrap_err().code,
        "project_setup_missing"
    );
    assert_eq!(
        bridge.team_graph("Not A Project").await.unwrap_err().code,
        "invalid_project"
    );
    Ok(())
}

#[tokio::test]
async fn desktop_reads_the_team_graph_through_the_real_engine() {
    let root = tempfile::Builder::new()
        .prefix("ca-graph-desk-")
        .tempdir_in("/tmp")
        .unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let config = engine_support::engine_config(root.path());
    let (_cancel, stopped) = tokio::sync::watch::channel(false);
    let mut service = EngineService::start(root.path(), "e.sock", &config, &stopped)
        .await
        .unwrap();
    let result = tokio::time::timeout(Duration::from_secs(20), flow(root.path())).await;
    let cleanup = tokio::time::timeout(Duration::from_secs(5), service.shutdown()).await;
    assert!(matches!(cleanup, Ok(Ok(()))));
    result.unwrap().unwrap();
}
