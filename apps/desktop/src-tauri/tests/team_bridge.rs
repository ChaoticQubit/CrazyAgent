use crazyagent::config::resolve_config;
use crazyagent::engine::bootstrap::EngineService;
use crazyagent::project_setup::{HarnessSlot, ProjectSetup, TeamPlan};
use crazyagent::protocol::ProjectSpec;
use crazyagent::team::{MessageAuthor, MessageState, TaskSource, TaskStatus};
use crazyagent_desktop::bridge::{ActionOutcome, NativeBridge};
use std::error::Error;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::Duration;

const COMMANDS: [&str; 6] = [
    "desktop_team_status",
    "desktop_team_message",
    "desktop_team_delegate",
    "desktop_team_cancel",
    "desktop_team_keep_warm",
    "desktop_team_pause",
];

#[test]
fn team_commands_are_registered_in_all_four_places() {
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
            bindings.contains(&format!("async fn {command}(")),
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
        bridge.team_status("chores").await.unwrap_err().code,
        "not_connected"
    );
    assert!(bridge.connect().await?.connected);
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

    let team = bridge.team_status(&id).await?;
    assert_eq!((team.min_agents, team.max_agents), (1, 3));
    assert!(!team.keep_warm);
    assert!(!team.paused);
    assert_eq!(
        team.harnesses
            .iter()
            .map(|h| (h.harness.as_str(), h.agents_max, h.tools))
            .collect::<Vec<_>>(),
        [("codex", 1, false), ("codex", 2, true)]
    );
    assert!(team.tasks.is_empty() && team.agents.is_empty());

    let sent = bridge
        .team_message(&id, "Split the chores list.".into(), 2)
        .await?;
    assert!(matches!(sent.outcome, ActionOutcome::Applied), "{sent:?}");
    assert_eq!(sent.intent.kind, "team");
    let handed = bridge
        .team_delegate(&id, Some("codex".into()), "Write chores.md".into(), 3)
        .await?;
    assert!(
        matches!(handed.outcome, ActionOutcome::Applied),
        "{handed:?}"
    );
    let refused = bridge
        .team_delegate(&id, Some("gemini".into()), "Nope".into(), 4)
        .await?;
    assert_eq!(refused.rejection, Some("team_harness_not_in_plan"));
    let warm = bridge.team_keep_warm(&id, true, 5).await?;
    assert!(matches!(warm.outcome, ActionOutcome::Applied), "{warm:?}");

    let team = bridge.team_status(&id).await?;
    assert!(team.keep_warm);
    assert_eq!(
        team.messages
            .iter()
            .map(|m| (m.author, m.state, m.text.as_str()))
            .collect::<Vec<_>>(),
        [(
            MessageAuthor::Owner,
            MessageState::Pending,
            "Split the chores list."
        )]
    );
    assert_eq!(team.tasks.len(), 1);
    let task = &team.tasks[0];
    assert_eq!(
        (
            task.source,
            task.status,
            task.status_label.as_str(),
            task.tools
        ),
        (
            TaskSource::Owner,
            TaskStatus::NeedsPool,
            "Needs a pool",
            true
        )
    );

    let cancelled = bridge.team_cancel(&id, Some(task.id), 6).await?;
    assert!(
        matches!(cancelled.outcome, ActionOutcome::Applied),
        "{cancelled:?}"
    );
    let missing = bridge.team_cancel(&id, Some(task.id + 50), 7).await?;
    assert_eq!(missing.rejection, Some("team_task_missing"));
    assert_eq!(
        bridge.team_status(&id).await?.tasks[0].status,
        TaskStatus::Cancelled
    );
    let paused = bridge.team_pause(&id, true, 8).await?;
    assert!(
        matches!(paused.outcome, ActionOutcome::Applied),
        "{paused:?}"
    );
    assert!(bridge.team_status(&id).await?.paused);
    let resumed = bridge.team_pause(&id, false, 9).await?;
    assert!(
        matches!(resumed.outcome, ActionOutcome::Applied),
        "{resumed:?}"
    );
    assert!(!bridge.team_status(&id).await?.paused);
    assert_eq!(
        bridge.team_status("absent-project").await.unwrap_err().code,
        "project_setup_missing"
    );
    assert!(bridge.pending_intent()?.is_none());
    Ok(())
}

#[tokio::test]
async fn desktop_reads_the_team_and_sends_team_actions_through_the_real_engine() {
    let root = tempfile::Builder::new()
        .prefix("ca-team-desk-")
        .tempdir_in("/tmp")
        .unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let config = resolve_config(&root.path().join("g"), &root.path().join("p")).unwrap();
    let (_cancel, stopped) = tokio::sync::watch::channel(false);
    let mut service = EngineService::start(root.path(), "e.sock", &config, &stopped)
        .await
        .unwrap();
    let result = tokio::time::timeout(Duration::from_secs(20), flow(root.path())).await;
    let cleanup = tokio::time::timeout(Duration::from_secs(5), service.shutdown()).await;
    assert!(matches!(cleanup, Ok(Ok(()))));
    result.unwrap().unwrap();
}
