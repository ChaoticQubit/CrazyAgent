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

const COMMANDS: [&str; 14] = [
    "desktop_team_status",
    "desktop_team_message",
    "desktop_team_delegate",
    "desktop_team_cancel",
    "desktop_team_keep_warm",
    "desktop_team_time_limits",
    "desktop_team_on_harness_switch",
    "desktop_team_apply_harness_switch",
    "desktop_team_finished_agents",
    "desktop_team_live_master",
    "desktop_team_pause",
    "desktop_team_hand_off",
    "desktop_team_rename",
    "desktop_team_wake_agent",
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
    let version = bridge.team_status(&id).await?.settings_version;
    let warm = bridge.team_keep_warm(&id, true, version, 5).await?;
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
    let read = team.settings_version;
    let limited = bridge
        .team_time_limits(&id, Some(3600), Some(14_400), None, read, 15)
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
    assert_eq!(team.settings_version, read + 1);
    let refused = bridge
        .team_time_limits(
            &id,
            Some(7200),
            Some(3600),
            Some(600),
            team.settings_version,
            16,
        )
        .await
        .unwrap_err();
    assert_eq!(
        (refused.code, refused.cause.as_deref()),
        ("team_action_invalid", Some("team_time_limits_invalid")),
        "a session limit below the task limit is refused and names the engine's reason"
    );
    let stale = bridge
        .team_time_limits(&id, Some(1800), None, Some(120), read, 21)
        .await?;
    assert_eq!(
        stale.rejection.as_deref(),
        Some("stale_revision"),
        "time limits saved from the read before the last change are refused"
    );
    let team = bridge.team_status(&id).await?;
    assert_eq!(
        (
            team.turn_limit_secs,
            team.session_limit_secs,
            team.idle_stop_secs
        ),
        (Some(3600), Some(14_400), None),
        "the newer limits survive a save from an older read"
    );
    assert_eq!(team.settings_version, read + 1);
    assert_eq!(
        bridge.team_status("absent-project").await.unwrap_err().code,
        "project_setup_missing"
    );
    wake(&mut bridge, root, &id).await?;
    assert!(bridge.pending_intent()?.is_none());
    Ok(())
}

fn sqlite(root: &Path, sql: &str) -> Result<String, Box<dyn Error>> {
    let result = crazyagent::spawn_gate::output(
        std::process::Command::new("/usr/bin/sqlite3")
            .arg("-cmd")
            .arg(".timeout 5000")
            .arg(root.join("engine.db"))
            .arg(sql)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped()),
    )?;
    if !result.status.success() {
        return Err(String::from_utf8_lossy(&result.stderr).into_owned().into());
    }
    Ok(String::from_utf8(result.stdout)?.trim().into())
}

/// Presses Wake the way the Finished fold-out does (DEC-102, AVF-5): the bridge sends the team
/// wake by agent id to the real engine, which records it for the team runtime and then shows
/// the agent as waking; a running agent, one whose worktree is gone and an unknown one are
/// refused with their plain codes. Team agents are stored as the runtime leaves them. This
/// engine runs no team runtime (only the `crazyagent` binary starts one), so the recorded
/// Wake stays pending and the waking checks do not depend on any tick.
async fn wake(bridge: &mut NativeBridge, root: &Path, id: &str) -> Result<(), Box<dyn Error>> {
    for (agent, status, turns, worktree) in [
        (501, "stopped", 2, true),
        (502, "idle", 0, true),
        (503, "failed", 1, false),
    ] {
        sqlite(
            root,
            &format!(
                "INSERT INTO team_agents(id,project,role,harness,tools,status,turns,created_ms,heartbeat_ms,stopped_ms) VALUES ({agent},'{id}','sub','codex',1,'{status}',{turns},1,1,2);"
            ),
        )?;
        if worktree {
            sqlite(
                root,
                &format!(
                    "INSERT INTO team_worktrees(agent,project,repository,worktree,git_dir) VALUES ({agent},'{id}','.','worktrees/agent-{agent}','.git');"
                ),
            )?;
        }
    }
    let shown = |team: &crazyagent::team::TeamStatus, agent: i64| {
        let found = team.agents.iter().find(|a| a.id == agent).unwrap();
        (found.finished, found.wakeable, found.wake_note.clone())
    };
    let team = bridge.team_status(id).await?;
    let runs = cfg!(target_os = "macos");
    assert!(shown(&team, 501).0);
    assert_eq!(shown(&team, 501).1, runs, "{team:#?}");
    assert_eq!(
        shown(&team, 502),
        (false, false, None),
        "an agent waiting for its first turn is not finished"
    );
    assert_eq!(
        shown(&team, 503),
        (
            true,
            false,
            Some(
                "Its worktree was cleaned up, so it cannot be woken. Hand its work to a new agent."
                    .into()
            )
        )
    );

    let pressed = bridge.team_wake_agent(id, 501, 15).await?;
    if runs {
        assert!(
            matches!(pressed.outcome, ActionOutcome::Applied),
            "{pressed:?}"
        );
        assert_eq!(
            shown(&bridge.team_status(id).await?, 501),
            (true, false, Some("Waking.".into()))
        );
        let again = bridge.team_wake_agent(id, 501, 16).await?;
        assert!(matches!(again.outcome, ActionOutcome::Applied), "{again:?}");
        assert_eq!(
            sqlite(root, "SELECT COUNT(*) FROM team_wake_requests;")?,
            "1"
        );
    } else {
        assert_eq!(
            pressed.rejection.as_deref(),
            Some("team_agents_unavailable")
        );
    }
    for (agent, generation, code) in [
        (502, 17, "team_agent_running"),
        (503, 18, "team_agent_no_worktree"),
        (999, 19, "team_agent_not_found"),
    ] {
        let refused = bridge.team_wake_agent(id, agent, generation).await?;
        assert_eq!(refused.rejection.as_deref(), Some(code), "{refused:?}");
    }
    assert_eq!(
        bridge.team_wake_agent(id, 0, 20).await.unwrap_err().code,
        "team_action_invalid"
    );
    assert_eq!(
        sqlite(root, "SELECT COUNT(*) FROM team_agents WHERE role='sub';")?,
        "3",
        "a Wake starts no new agent"
    );
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
    let mut child = crazyagent::spawn_gate::spawn(
        Command::new("/usr/bin/sqlite3")
            .arg(root.join("engine.db"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()),
    )
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
    let live = bridge
        .team_live_master(&id, false, team.settings_version, 5)
        .await?;
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

const COORD_COMMANDS: [&str; 6] = [
    "desktop_tool_switches",
    "desktop_set_tool_switch",
    "desktop_restore_tool_switches",
    "desktop_team_coordination",
    "desktop_team_harness_mode",
    "desktop_team_harness_delivery",
];

#[test]
fn tool_switch_and_coordination_commands_are_registered_where_the_web_view_calls_them() {
    let build = include_str!("../build.rs");
    let bindings = include_str!("../src/lib.rs");
    let capability = include_str!("../capabilities/main-window.json");
    let api = include_str!("../../src/api.ts");
    for command in COORD_COMMANDS {
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
        assert!(
            api.contains(&format!("\"{command}\"")),
            "{command} web view call"
        );
    }
}

async fn coordinated(root: &Path) -> Result<(), Box<dyn Error>> {
    use crazyagent::coord::settings::{
        CoordSettings, Delivery, HarnessKind, Nudges, Overlaps, QuestionsTo, RoleSkills,
        SubagentMode, TaskStart, TerminalDisplay,
    };
    use crazyagent::coord::tool_switches::Switch;
    let mut bridge = NativeBridge::new(Some(root.join("e.sock")));
    assert!(bridge.connect().await?.connected);
    let _home = engine_support::pick_home(&mut bridge, 1).await;
    let created = bridge
        .setup_project(
            ProjectSpec {
                name: "Chores".into(),
                repositories: vec![],
            },
            setup(),
            1,
        )
        .await?;
    assert!(
        matches!(created.outcome, ActionOutcome::Applied),
        "{created:?}"
    );
    let id = created.intent.project_id.clone().unwrap();

    let global = bridge.tool_switches(None, None).await?;
    assert!(
        global
            .iter()
            .all(|view| view.builtin && view.global == Switch::On)
    );
    bridge
        .set_tool_switch(None, None, "start_work".into(), Switch::Off)
        .await?;
    let here = bridge
        .set_tool_switch(Some(id.clone()), None, "start_work".into(), Switch::On)
        .await?;
    let start = here.iter().find(|view| view.tool == "start_work").unwrap();
    assert_eq!((start.global, start.here), (Switch::Off, Some(Switch::On)));
    let hidden = bridge
        .set_tool_switch(None, None, "recall".into(), Switch::Hidden)
        .await?;
    assert_eq!(
        hidden
            .iter()
            .find(|view| view.tool == "recall")
            .unwrap()
            .global,
        Switch::Hidden
    );
    let restored = bridge.restore_tool_switches(None, None).await?;
    assert!(restored.iter().all(|view| view.global == Switch::On));
    let project = bridge.tool_switches(Some(id.clone()), None).await?;
    let start = project
        .iter()
        .find(|view| view.tool == "start_work")
        .unwrap();
    assert_eq!(
        (start.global, start.here),
        (Switch::On, Some(Switch::On)),
        "a global restore keeps the project's own switch"
    );
    assert_eq!(
        bridge
            .set_tool_switch(None, None, "nope".into(), Switch::Off)
            .await
            .unwrap_err()
            .code,
        "tool_unknown"
    );

    let defaults = bridge.team_status(&id).await?.coordination.unwrap();
    assert_eq!(defaults.settings, CoordSettings::default());
    let chosen = CoordSettings {
        questions_to: QuestionsTo::Me,
        overlaps: Overlaps::Block,
        nudges: Nudges::TenMinutes,
        counted_tools: vec!["start_work".into(), "ctx_*".into()],
        role_skills: RoleSkills {
            master: None,
            lead: Some(Vec::new()),
            worker: Some(vec!["release-notes".into()]),
        },
        terminal_display: TerminalDisplay::Fullscreen,
        task_start: TaskStart::Off,
    };
    for receipt in [
        bridge.team_coordination(&id, chosen.clone(), 2).await?,
        bridge
            .team_harness_mode(&id, HarnessKind::ClaudeCode, SubagentMode::Native, 3)
            .await?,
        bridge
            .team_harness_delivery(&id, HarnessKind::Codex, Delivery::TypingOnly, 4)
            .await?,
    ] {
        assert!(
            matches!(receipt.outcome, ActionOutcome::Applied),
            "{receipt:?}"
        );
    }
    let refused = bridge
        .team_harness_mode(&id, HarnessKind::Rcli, SubagentMode::Native, 5)
        .await
        .unwrap_err();
    assert_eq!(
        (refused.code, refused.cause.as_deref()),
        ("team_action_invalid", Some("coord_native_unsupported"))
    );
    let saved = bridge.team_status(&id).await?.coordination.unwrap();
    assert_eq!(saved.settings, chosen);
    let row = |harness: HarnessKind| {
        let row = saved
            .harnesses
            .iter()
            .find(|row| row.harness == harness)
            .unwrap();
        (row.mode, row.delivery)
    };
    assert_eq!(
        row(HarnessKind::ClaudeCode),
        (SubagentMode::Native, Delivery::Auto)
    );
    assert_eq!(
        row(HarnessKind::Codex),
        (SubagentMode::AppSpawned, Delivery::TypingOnly)
    );
    assert_eq!(
        row(HarnessKind::Rcli),
        (SubagentMode::AppSpawned, Delivery::Auto)
    );
    assert!(bridge.pending_intent()?.is_none());
    Ok(())
}

#[tokio::test]
async fn desktop_sets_tool_switches_and_coordination_through_the_real_engine() {
    let root = tempfile::Builder::new()
        .prefix("ca-coord-desk-")
        .tempdir_in("/tmp")
        .unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let config = engine_support::engine_config(root.path());
    let (_cancel, stopped) = tokio::sync::watch::channel(false);
    let mut service = EngineService::start(root.path(), "e.sock", &config, &stopped)
        .await
        .unwrap();
    let result = tokio::time::timeout(Duration::from_secs(20), coordinated(root.path())).await;
    let cleanup = tokio::time::timeout(Duration::from_secs(5), service.shutdown()).await;
    assert!(matches!(cleanup, Ok(Ok(()))));
    result.unwrap().unwrap();
}

/// Sends every New folder limit choice through the desktop bridge to the real engine and
/// reads each back in the team status the Team controls show; a count outside the choices
/// is refused and leaves the last choice. Takes the engine root. Returns any failure.
async fn new_folder_limits(root: &Path) -> Result<(), Box<dyn Error>> {
    let mut bridge = NativeBridge::new(Some(root.join("e.sock")));
    assert!(bridge.connect().await?.connected);
    let _home = engine_support::pick_home(&mut bridge, 1).await;
    let spec = ProjectSpec {
        name: "Folders".into(),
        repositories: vec![],
    };
    let created = bridge.setup_project(spec, setup(), 1).await?;
    assert!(
        matches!(created.outcome, ActionOutcome::Applied),
        "{created:?}"
    );
    let id = created.intent.project_id.clone().unwrap();
    assert_eq!(
        bridge.team_status(&id).await?.new_folder_files_max,
        crazyagent::team::NEW_FOLDER_FILES_DEFAULT
    );
    let mut generation = 2;
    for files in crazyagent::team::NEW_FOLDER_FILES_CHOICES {
        let saved = bridge.team_new_folder_limit(&id, files, generation).await?;
        generation += 1;
        assert!(matches!(saved.outcome, ActionOutcome::Applied), "{saved:?}");
        assert_eq!(bridge.team_status(&id).await?.new_folder_files_max, files);
    }
    let refused = bridge
        .team_new_folder_limit(&id, 999, generation)
        .await
        .unwrap_err();
    assert_eq!(
        (refused.code, refused.cause.as_deref()),
        ("team_action_invalid", Some("team_new_folder_limit_invalid")),
        "a count outside the choices is refused and names the engine's reason"
    );
    assert_eq!(
        bridge.team_status(&id).await?.new_folder_files_max,
        crazyagent::team::NEW_FOLDER_FILES_CHOICES[3]
    );
    Ok(())
}

/// Sends every Terminals per window choice through the desktop bridge to the real engine
/// and reads each back the way a terminal wall window reads it and in the team status the
/// Agent settings show; Six until the owner picks another. Takes the engine root. Returns
/// any failure.
async fn terminals_per_window_choices(root: &Path) -> Result<(), Box<dyn Error>> {
    use crazyagent::team::TerminalsPerWindow::{Four, Nine, Six};
    let mut bridge = NativeBridge::new(Some(root.join("e.sock")));
    assert!(bridge.connect().await?.connected);
    let _home = engine_support::pick_home(&mut bridge, 1).await;
    let spec = ProjectSpec {
        name: "Wall".into(),
        repositories: vec![],
    };
    let created = bridge.setup_project(spec, setup(), 1).await?;
    assert!(
        matches!(created.outcome, ActionOutcome::Applied),
        "{created:?}"
    );
    let id = created.intent.project_id.clone().unwrap();
    assert_eq!(bridge.terminals_per_window(&id).await?, Six);
    assert_eq!(bridge.team_status(&id).await?.terminals_per_window, Six);
    for (generation, choice) in (2..).zip([Four, Nine, Six, Nine]) {
        let saved = bridge
            .team_terminals_per_window(&id, choice, generation)
            .await?;
        assert!(matches!(saved.outcome, ActionOutcome::Applied), "{saved:?}");
        assert_eq!(bridge.terminals_per_window(&id).await?, choice);
        assert_eq!(bridge.team_status(&id).await?.terminals_per_window, choice);
    }
    Ok(())
}

/// Every Terminals per window choice the desktop sends is saved, read back by the wall and
/// shown (owner 2026-11-07, OS-4, every-option rule).
#[tokio::test]
async fn desktop_saves_and_reads_every_terminals_per_window_choice() {
    let root = tempfile::Builder::new()
        .prefix("ca-team-wall-")
        .tempdir_in("/tmp")
        .unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let config = engine_support::engine_config(root.path());
    let (_cancel, stopped) = tokio::sync::watch::channel(false);
    let mut service = EngineService::start(root.path(), "e.sock", &config, &stopped)
        .await
        .unwrap();
    let result = tokio::time::timeout(
        Duration::from_secs(20),
        terminals_per_window_choices(root.path()),
    )
    .await;
    let cleanup = tokio::time::timeout(Duration::from_secs(5), service.shutdown()).await;
    assert!(matches!(cleanup, Ok(Ok(()))));
    result.unwrap().unwrap();
}

/// Every New folder limit choice the desktop sends is saved and shown (fix/changes-vs-owner-branch
/// DEC-107 rule 3, every-option rule).
#[tokio::test]
async fn desktop_saves_and_shows_every_new_folder_limit_choice() {
    let root = tempfile::Builder::new()
        .prefix("ca-team-folders-")
        .tempdir_in("/tmp")
        .unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let config = engine_support::engine_config(root.path());
    let (_cancel, stopped) = tokio::sync::watch::channel(false);
    let mut service = EngineService::start(root.path(), "e.sock", &config, &stopped)
        .await
        .unwrap();
    let result =
        tokio::time::timeout(Duration::from_secs(20), new_folder_limits(root.path())).await;
    let cleanup = tokio::time::timeout(Duration::from_secs(5), service.shutdown()).await;
    assert!(matches!(cleanup, Ok(Ok(()))));
    result.unwrap().unwrap();
}
