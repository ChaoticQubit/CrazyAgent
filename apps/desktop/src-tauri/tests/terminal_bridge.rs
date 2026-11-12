#![cfg(unix)]

mod engine_support;

use crazyagent::engine::bootstrap::EngineService;
use crazyagent::project_setup::{HarnessSlot, ProjectSetup, TeamPlan};
use crazyagent::protocol::ProjectSpec;
use crazyagent::team::{EdgeState, NodeKind, TaskSource};
use crazyagent::terminal::{
    AgentId, INPUT_BYTES_MAX, PROMPT_BYTES_MAX, SIZE_MAX, SIZE_MIN, TerminalFrame, TerminalRequest,
};
use crazyagent_desktop::bridge::terminal::{
    Terminals, VIEWERS_MAX, agent, channel_message, graph_shows, input_request, pane, plain_code,
    request_valid,
};
use crazyagent_desktop::bridge::{ActionOutcome, NativeBridge};
use std::error::Error;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tauri::ipc::InvokeResponseBody;

const COMMANDS: [&str; 6] = [
    "desktop_team_graph",
    "desktop_terminal_attach",
    "desktop_terminal_input",
    "desktop_terminal_prompt",
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

const WALL_COMMANDS: [&str; 12] = [
    "desktop_terminal_attach",
    "desktop_terminal_input",
    "desktop_terminal_prompt",
    "desktop_terminal_resize",
    "desktop_terminal_detach",
    "desktop_team_graph",
    "desktop_team_stop_agent",
    "desktop_team_stop_teammate",
    "desktop_terminals_per_window",
    "desktop_extend_terminal_windows",
    "desktop_focus_terminal_window",
    "desktop_close_project_walls",
];

const PROJECT_WALL_COMMANDS: [&str; 7] = [
    "desktop_team_graph",
    "desktop_team_stop_agent",
    "desktop_team_stop_teammate",
    "desktop_terminals_per_window",
    "desktop_extend_terminal_windows",
    "desktop_focus_terminal_window",
    "desktop_close_project_walls",
];

const WALL_GUARD: &str = "terminal_window(&webview)?;";
const PROJECT_WALL_GUARD: &str = "terminal_window_for(&webview, &id)?;";

/// Every `#[tauri::command]` in lib.rs with its name and body. Takes nothing. Returns the
/// pairs in source order.
fn command_bodies() -> Vec<(&'static str, &'static str)> {
    include_str!("../src/lib.rs")
        .split("#[tauri::command]")
        .skip(1)
        .map(|body| {
            let name = body
                .split("fn ")
                .nth(1)
                .and_then(|rest| rest.split(['(', '<']).next())
                .unwrap();
            (name, body)
        })
        .collect()
}

/// Terminal wall windows reach only the terminal, graph, Stop, Terminals per window and
/// window growth commands: their capability grants exactly those to `terminals-*` windows,
/// the app loads it beside the main window's, and in the bindings exactly those commands
/// admit a wall window while every other command keeps the main window guard. Each one that
/// names a project admits a wall window only for its own project, and a wall attaches only to
/// a terminal that project's graph shows.
#[test]
fn wall_windows_reach_only_the_terminal_graph_stop_and_setting_read_commands() {
    use serde_json::{Value, json};
    use std::collections::BTreeSet;
    let read = |text: &str| -> Value { serde_json::from_str(text).unwrap() };
    let wall = read(include_str!("../capabilities/terminal-wall.json"));
    assert_eq!(wall["windows"], json!(["terminals-*"]));
    assert_eq!(wall["webviews"], json!(["terminals-*"]));
    assert_eq!(wall["local"], json!(true));
    let granted: BTreeSet<String> = wall["permissions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|permission| permission.as_str().unwrap().to_owned())
        .collect();
    let allowed: BTreeSet<String> = WALL_COMMANDS
        .iter()
        .map(|command| format!("allow-{}", command.replace('_', "-")))
        .collect();
    assert_eq!(granted, allowed);
    let main = read(include_str!("../capabilities/main-window.json"));
    assert!(
        main.get("windows").is_none(),
        "the capability names the main webview alone, so no child webview of the window matches it"
    );
    assert_eq!(main["webviews"], json!(["main"]));
    let conf = read(include_str!("../tauri.conf.json"));
    assert_eq!(
        conf["app"]["security"]["capabilities"],
        json!(["main-window", "terminal-wall"])
    );
    let commands = command_bodies();
    assert!(commands.len() > 100, "{}", commands.len());
    let wall_guarded = |body: &str| body.contains(WALL_GUARD) || body.contains(PROJECT_WALL_GUARD);
    let admitted: BTreeSet<&str> = commands
        .iter()
        .filter(|(_, body)| wall_guarded(body))
        .map(|(name, _)| *name)
        .collect();
    assert_eq!(admitted, WALL_COMMANDS.into_iter().collect());
    for (name, body) in &commands {
        assert_ne!(
            wall_guarded(body),
            body.contains("main_window(&webview)?;"),
            "{name} has exactly one of the two window guards"
        );
    }
    for (name, body) in &commands {
        if PROJECT_WALL_COMMANDS.contains(name) {
            let start = body.split_once('{').map(|(_, rest)| rest.trim_start());
            assert!(
                start.is_some_and(|rest| rest.starts_with(PROJECT_WALL_GUARD)),
                "{name} admits a wall window only for its own project"
            );
        }
    }
    let attach = commands
        .iter()
        .find(|(name, _)| *name == "desktop_terminal_attach")
        .unwrap()
        .1;
    assert!(attach.contains("id: Option<String>"), "{attach}");
    assert!(attach.contains(PROJECT_WALL_GUARD), "{attach}");
    assert!(
        attach.contains("bridge::terminal::graph_shows(&graph, &agent)"),
        "{attach}"
    );
}

/// A wall window opens at an address naming its project, page and project name, and moves
/// the owner only through `wall_focus`: to the main window on its project's Agents tab or to
/// another wall window of its own project, never from the main window.
#[test]
fn a_wall_window_opens_at_its_named_address_and_moves_only_through_wall_focus() {
    let commands = command_bodies();
    let focus = commands
        .iter()
        .find(|(name, _)| *name == "desktop_focus_terminal_window")
        .map_or("", |(_, body)| *body);
    assert!(
        focus.contains("bridge::terminal::wall_focus(webview.label(), &project, &target)?"),
        "{focus}"
    );
    assert!(!focus.contains("main_window(&webview)?;"), "{focus}");
    let bindings = include_str!("../src/lib.rs");
    assert!(
        bindings.contains("bridge::terminal::wall_url(project, page, name)"),
        "build_wall opens the named address"
    );
    assert!(
        !bindings.contains("format!(\"index.html#/terminals/"),
        "no wall address is built by hand"
    );
}

/// The wall windows do not outlive what they show: closing the main window closes every
/// wall window, and after a project removal the engine applied the main window asks a
/// main-only command to close that project's walls, so the removal stays a plain bridge call.
#[test]
fn closing_main_or_removing_a_project_closes_the_wall_windows() {
    let lib = include_str!("../src/lib.rs");
    let setup = lib.split(".setup(|app| {").nth(1).unwrap();
    let setup = setup.split("#[cfg(feature").next().unwrap();
    assert!(setup.contains("main.on_window_event("), "{setup}");
    assert!(setup.contains("tauri::WindowEvent::Destroyed"), "{setup}");
    assert!(setup.contains("close_walls(&handle, None)"), "{setup}");
    let close = command_bodies()
        .into_iter()
        .find(|(name, _)| *name == "desktop_close_project_walls")
        .map(|(_, body)| body)
        .unwrap_or_default();
    assert!(close.contains("app: AppHandle"), "{close}");
    assert!(
        close.contains(PROJECT_WALL_GUARD) && close.contains("close_walls(&app, Some(&project))"),
        "{close}"
    );
    let build = include_str!("../build.rs");
    assert_eq!(build.matches("\"desktop_close_project_walls\"").count(), 1);
    let capability = include_str!("../capabilities/main-window.json");
    assert_eq!(
        capability
            .matches("\"allow-desktop-close-project-walls\"")
            .count(),
        1
    );
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
fn teammate_pane_ids_from_the_window_are_checked_before_the_engine_sees_them() {
    assert_eq!(pane("%12").unwrap(), "%12");
    for bad in [
        "%x",
        "",
        "12",
        "%",
        "%-1",
        "% 1",
        "%1\n",
        "%1;kill",
        "..%1",
        &format!("%{}", "9".repeat(40)),
    ] {
        assert_eq!(
            pane(bad).unwrap_err().code,
            "terminal_pane_invalid",
            "{bad:?}"
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
fn only_input_prompts_and_resize_inside_the_engine_limits_are_sent() {
    let input = |len: usize| TerminalRequest::Input {
        bytes: vec![b'a'; len],
    };
    assert!(request_valid(&input(1)));
    assert!(request_valid(&input(INPUT_BYTES_MAX)));
    assert!(!request_valid(&input(0)));
    assert!(!request_valid(&input(INPUT_BYTES_MAX + 1)));
    let scroll = |len: usize| input_request(vec![b'a'; len], true);
    assert_eq!(
        scroll(1),
        TerminalRequest::Scroll { bytes: vec![b'a'] },
        "wheel keys go as Scroll, which the engine never counts as the owner typing"
    );
    assert_eq!(input_request(vec![b'a'], false), input(1));
    assert!(request_valid(&scroll(INPUT_BYTES_MAX)));
    assert!(!request_valid(&scroll(0)));
    assert!(!request_valid(&scroll(INPUT_BYTES_MAX + 1)));
    let resize = |cols, rows| TerminalRequest::Resize { cols, rows };
    assert!(request_valid(&resize(SIZE_MIN, SIZE_MAX)));
    assert!(request_valid(&resize(80, 24)));
    assert!(!request_valid(&resize(SIZE_MIN - 1, 24)));
    assert!(!request_valid(&resize(80, SIZE_MAX + 1)));
    assert!(!request_valid(&TerminalRequest::Detach));
    let prompt = |text: String| TerminalRequest::Prompt { text };
    assert!(request_valid(&prompt("run the tests".into())));
    assert!(request_valid(&prompt("x".repeat(PROMPT_BYTES_MAX))));
    assert!(!request_valid(&prompt(" \n\t ".into())));
    assert!(!request_valid(&prompt("x".repeat(PROMPT_BYTES_MAX + 1))));
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
    let terminals = Arc::new(Terminals::default());
    let team = AgentId::team(4).unwrap();
    terminals.page_started("main");
    terminals.room("main", &team).unwrap();
    let typed = terminals
        .send(
            "main",
            &team,
            TerminalRequest::Input {
                bytes: b"ls\r".to_vec(),
            },
        )
        .await;
    assert_eq!(typed.unwrap_err().code, "terminal_not_attached");
    let empty = terminals
        .send("main", &team, TerminalRequest::Input { bytes: vec![] })
        .await;
    assert_eq!(empty.unwrap_err().code, "terminal_request_invalid");
    let resized = terminals
        .send("main", &team, TerminalRequest::Resize { cols: 0, rows: 24 })
        .await;
    assert_eq!(resized.unwrap_err().code, "terminal_request_invalid");
    terminals.detach("main", &team, u64::MAX).unwrap();
    terminals.detach("main", &team, u64::MAX).unwrap();
}

#[test]
fn a_view_holds_its_place_from_the_room_check_until_it_attaches_or_gives_up() {
    let terminals = Arc::new(Terminals::default());
    terminals.page_started("main");
    let team = |n| AgentId::team(n).unwrap();
    let past = VIEWERS_MAX as i64 + 1;
    let mut held: Vec<_> = (1..=VIEWERS_MAX as i64)
        .map(|n| terminals.room("main", &team(n)))
        .collect();
    assert!(held.iter().all(Result::is_ok));
    assert_eq!(
        terminals.room("main", &team(past)).err().map(|e| e.code),
        Some("terminal_viewers_full"),
        "a view still opening its connection keeps its place, so one past the window's views is refused before it opens one"
    );
    assert!(
        terminals.room("main", &team(1)).is_ok(),
        "a second view of the same agent replaces the first"
    );
    held.truncate(VIEWERS_MAX - 1);
    assert!(
        terminals.room("main", &team(past)).is_ok(),
        "a view that gave up before attaching frees its place"
    );
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
    assert_eq!(
        graph.nodes.len(),
        1,
        "a new team is its master alone: {graph:?}"
    );
    assert!(graph.nodes.iter().all(|n| !n.kind.agent()));
    assert!(graph.edges.is_empty());
    let mut shown = graph.clone();
    shown.nodes[0].terminal = Some(AgentId::team(4)?);
    assert!(graph_shows(&shown, &AgentId::team(4)?));
    assert!(!graph_shows(&shown, &AgentId::team(5)?));
    assert!(!graph_shows(&graph, &AgentId::team(4)?));

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
    assert_eq!(
        edge.to, masters[0].id,
        "a task no agent took yet waits on the master"
    );
    assert_eq!(edge.source, TaskSource::Owner);
    assert_eq!(edge.state, EdgeState::Sent);
    assert_eq!(edge.text, "Write chores.md");
    assert_eq!(edge.reply, None);

    let unknown = bridge
        .open_terminal(AgentId::team(99)?, None)
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
