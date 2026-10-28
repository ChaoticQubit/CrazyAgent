#![cfg(unix)]

mod engine_support;

use crazyagent::engine::bootstrap::EngineService;
use crazyagent::project_setup::{HarnessSlot, ProjectSetup, TeamPlan};
use crazyagent::protocol::ProjectSpec;
use crazyagent::team::{MessageAuthor, MessageState, TaskSource, TaskStatus};
use crazyagent_desktop::bridge::{ActionOutcome, NativeBridge};
use std::error::Error;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

const COMMANDS: [&str; 10] = [
    "desktop_team_status",
    "desktop_team_message",
    "desktop_team_delegate",
    "desktop_team_cancel",
    "desktop_team_keep_warm",
    "desktop_team_time_limits",
    "desktop_team_live_master",
    "desktop_team_pause",
    "desktop_team_hand_off",
    "desktop_team_rename",
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
    assert_eq!(
        refused.rejection.as_deref(),
        Some("team_harness_not_in_plan")
    );
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
    assert_eq!(missing.rejection.as_deref(), Some("team_task_missing"));
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
    let team = bridge.team_status(&id).await?;
    assert!(!team.paused);
    assert!(team.plan.is_empty(), "no master reply proposed steps yet");
    let owner_message = team.messages[0].id;
    let not_a_plan = bridge.team_hand_off(&id, owner_message, 0, 10).await?;
    assert_eq!(
        not_a_plan.rejection.as_deref(),
        Some("team_plan_step_missing")
    );
    assert_eq!(
        bridge
            .team_hand_off(&id, owner_message, 8, 11)
            .await
            .unwrap_err()
            .code,
        "team_action_invalid"
    );
    let gone = bridge.team_rename(&id, 99, "docs-pass".into(), 12).await?;
    assert_eq!(gone.rejection.as_deref(), Some("team_agent_unknown"));
    let malformed = bridge.team_rename(&id, 99, "Docs Pass".into(), 13).await?;
    assert_eq!(malformed.rejection.as_deref(), Some("team_handle_invalid"));
    assert_eq!(
        bridge
            .team_rename(&id, 0, "docs".into(), 14)
            .await
            .unwrap_err()
            .code,
        "team_action_invalid"
    );

    let team = bridge.team_status(&id).await?;
    assert_eq!(
        (
            team.turn_limit_secs,
            team.session_limit_secs,
            team.idle_stop_secs
        ),
        (None, None, Some(600)),
        "no time limit and a 10 minute idle stop until the owner sets them"
    );
    let limited = bridge
        .team_time_limits(&id, Some(3600), Some(14_400), None, 15)
        .await?;
    assert!(
        matches!(limited.outcome, ActionOutcome::Applied),
        "{limited:?}"
    );
    let team = bridge.team_status(&id).await?;
    assert_eq!(
        (
            team.turn_limit_secs,
            team.session_limit_secs,
            team.idle_stop_secs
        ),
        (Some(3600), Some(14_400), None),
        "an idle stop of None is Off"
    );
    assert_eq!(
        bridge
            .team_time_limits(&id, Some(7200), Some(3600), Some(600), 16)
            .await
            .unwrap_err()
            .code,
        "team_action_invalid",
        "a session limit below the task limit is refused"
    );
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

/// Fills one project's team with history no single owner reply can carry whole: 120 long
/// finished tasks, 80 stopped sub-agents with long problems and 25 long escalations, written
/// by the real sqlite3 tool into the engine database. Takes the engine root and the project.
fn crowd(root: &Path, project: &str) {
    assert!(
        project
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-')
    );
    let sql = format!(
        ".timeout 10000
WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM n WHERE i<120)
INSERT INTO team_tasks(project,source,harness,tools,prompt,status,attempts,result,delivered,created_ms,started_ms,finished_ms)
SELECT '{project}','owner','codex',0,i||' '||hex(zeroblob(2000)),'done',1,hex(zeroblob(2000)),1,i,i,i FROM n;
WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM n WHERE i<80)
INSERT INTO team_agents(project,role,harness,tools,status,error,created_ms,heartbeat_ms,stopped_ms)
SELECT '{project}','sub','codex',0,'stopped',hex(zeroblob(1000)),i,i,i FROM n;
WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM n WHERE i<25)
INSERT INTO team_notifications(project,kind,text,created_ms)
SELECT '{project}','escalation',hex(zeroblob(1000)),i FROM n;
SELECT count(*) FROM team_tasks WHERE project='{project}';
"
    );
    let mut child = Command::new("/usr/bin/sqlite3")
        .arg(root.join("engine.db"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(sql.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "120");
}

async fn crowded(root: &Path) -> Result<(), Box<dyn Error>> {
    let mut bridge = NativeBridge::new(Some(root.join("e.sock")));
    assert!(bridge.connect().await?.connected);
    let _home = engine_support::pick_home(&mut bridge, 1).await;
    let spec = ProjectSpec {
        name: "Chores".into(),
        repositories: vec![],
    };
    let created = bridge.setup_project(spec, setup(), 2).await?;
    assert!(
        matches!(created.outcome, ActionOutcome::Applied),
        "{created:?}"
    );
    let id = created.intent.project_id.clone().unwrap();
    crowd(root, &id);
    let team = bridge.team_status(&id).await?;
    assert!(team.partial, "the read left older history out to fit");
    let paused = bridge.team_pause(&id, true, 3).await?;
    assert!(
        matches!(paused.outcome, ActionOutcome::Applied),
        "{paused:?}"
    );
    let cancelled = bridge.team_cancel(&id, None, 4).await?;
    assert!(
        matches!(cancelled.outcome, ActionOutcome::Applied),
        "{cancelled:?}"
    );
    let live = bridge.team_live_master(&id, false, 5).await?;
    assert!(matches!(live.outcome, ActionOutcome::Applied), "{live:?}");
    assert!(bridge.team_status(&id).await?.paused);
    Ok(())
}

/// A team whose full status passes one owner reply still reads in the desktop, and Pause,
/// Cancel and live master off still go through to the real engine.
#[tokio::test]
async fn desktop_pauses_and_cancels_a_team_whose_full_status_passes_one_reply() {
    let root = tempfile::Builder::new()
        .prefix("ca-team-crowd-")
        .tempdir_in("/tmp")
        .unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let config = engine_support::engine_config(root.path());
    let (_cancel, stopped) = tokio::sync::watch::channel(false);
    let mut service = EngineService::start(root.path(), "e.sock", &config, &stopped)
        .await
        .unwrap();
    let result = tokio::time::timeout(Duration::from_secs(30), crowded(root.path())).await;
    let cleanup = tokio::time::timeout(Duration::from_secs(5), service.shutdown()).await;
    assert!(matches!(cleanup, Ok(Ok(()))));
    result.unwrap().unwrap();
}
