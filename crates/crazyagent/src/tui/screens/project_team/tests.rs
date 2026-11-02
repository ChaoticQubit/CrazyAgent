use super::*;
use crate::client_controller::OwnerController;
use crate::coord::settings::{CoordSettings, RoleSkills};
use crate::engine::Client;
use crate::engine::bootstrap::EngineService;
use crate::engine::owner::{OwnerClientKind, OwnerResponse};
use crate::project_setup::{HarnessSlot, Platform, ProjectSetup, TeamPlan};
use crate::protocol::{CredentialRef, ProjectSpec};
use crate::tui::screens::new_project_frames::compose;
use crate::tui::screens::new_project_view::{Screen, tab_strip};
use crate::tui::screens::project::Tab;
use crossterm::event::KeyModifiers;
use std::os::unix::fs::PermissionsExt;

const NOW: i64 = 1_793_000_000_000;
const MINUTE: i64 = 60_000;

fn project() -> ProjectId {
    ProjectId::new("tidepool").unwrap()
}

fn press(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn accounts() -> Vec<AccountRow> {
    [
        ("acct-001", "ChatGPT Pro - main"),
        ("acct-002", "Claude Max - work"),
    ]
    .into_iter()
    .map(|(id, name)| AccountRow {
        id: CredentialRef::new(id).unwrap(),
        name: name.into(),
        provider: String::new(),
        status: String::new(),
    })
    .collect()
}

fn agent(
    id: i64,
    role: AgentRole,
    harness: &str,
    status: AgentStatus,
    task: Option<i64>,
) -> TeamAgent {
    TeamAgent {
        id,
        role,
        label: role.agent_label(id),
        harness: harness.into(),
        tools: true,
        account: Some(
            if harness == "codex" {
                "acct-001"
            } else {
                "acct-002"
            }
            .into(),
        ),
        worktree: Some(format!(
            "/Users/mira/CrazyAgent/Projects/tidepool/worktrees/agent-{id}"
        )),
        status,
        status_label: status.label().into(),
        task,
        turns: 1,
        problem: None,
        process_group: None,
        created_ms: NOW - 20 * MINUTE,
        heartbeat_ms: NOW,
        stopped_ms: None,
        persona: None,
        handle: None,
        finished: false,
        wakeable: false,
        wake_note: None,
        parent: None,
        owner_stopped: false,
        teammates: Vec::new(),
        pane: None,
        hookless: false,
    }
}

fn task(id: i64, source: TaskSource, prompt: &str, status: TaskStatus) -> TeamTask {
    TeamTask {
        id,
        source,
        harness: "claude-code".into(),
        tools: true,
        prompt: prompt.into(),
        status,
        status_label: status.label().into(),
        agent: None,
        attempts: 1,
        result: None,
        problem: None,
        delivered: false,
        output_bytes: 0,
        created_ms: NOW - 15 * MINUTE,
        started_ms: None,
        finished_ms: None,
        parent: None,
    }
}

fn team() -> TeamStatus {
    let mut charts = task(
        11,
        TaskSource::Master,
        "Build the charts component\nUse the readings API.",
        TaskStatus::Running,
    );
    charts.agent = Some(2);
    charts.started_ms = Some(NOW - 6 * MINUTE);
    let mut api = task(
        12,
        TaskSource::Owner,
        "Write the readings API",
        TaskStatus::Queued,
    );
    api.harness = "codex".into();
    let mut setup = task(
        10,
        TaskSource::Master,
        "Set up the project",
        TaskStatus::Done,
    );
    setup.started_ms = Some(NOW - 14 * MINUTE);
    setup.finished_ms = Some(NOW - 9 * MINUTE);
    setup.result = Some("Created the Next.js app and the Postgres schema.".into());
    let mut stopped = agent(4, AgentRole::Sub, "claude-code", AgentStatus::Stopped, None);
    stopped.stopped_ms = Some(NOW - 9 * MINUTE);
    TeamStatus {
        project_id: project(),
        keep_warm: false,
        turn_limit_secs: None,
        session_limit_secs: None,
        idle_stop_secs: None,
        on_harness_switch: crate::team::SwitchMode::Carry,
        sub_agent_depth: crate::team::DEPTH_DEFAULT,
        child_landing: crate::team::ChildLanding::Lead,
        max_harnesses: None,
        max_teammates: None,
        token_budget: None,
        budget_used: None,
        budget_reached: false,
        spread_accounts: false,
        show_finished: false,
        agent_branches: crate::team::BranchMode::Named,
        master_branch: None,
        land_on: None,
        background_at_end: crate::team::BackgroundAtEnd::Wait,
        ask_before_merge: false,
        new_folder_files_max: crate::team::NEW_FOLDER_FILES_DEFAULT,
        coordination: None,
        live_switch: None,
        paused: false,
        live_master: None,
        live_problem: None,
        min_agents: 1,
        max_agents: 4,
        harnesses: vec![
            TeamHarness {
                harness: "codex".into(),
                role: AgentRole::Master,
                agents_max: 1,
                tools: true,
                live: 1,
                delegation: Some(DelegationPath::Reply),
                delegation_label: Some(DelegationPath::Reply.label().into()),
                runnable: true,
            },
            TeamHarness {
                harness: "claude-code".into(),
                role: AgentRole::Sub,
                agents_max: 2,
                tools: true,
                live: 1,
                delegation: None,
                delegation_label: None,
                runnable: true,
            },
        ],
        agents: vec![
            agent(1, AgentRole::Master, "codex", AgentStatus::Idle, None),
            agent(
                2,
                AgentRole::Sub,
                "claude-code",
                AgentStatus::Working,
                Some(11),
            ),
            stopped,
        ],
        tasks: vec![setup, charts, api],
        messages: Vec::new(),
        plan: Vec::new(),
        events: Default::default(),
        partial: false,
    }
}

fn loaded() -> TeamView {
    let mut view = TeamView::new(project());
    let query = view.next_query(NOW).unwrap();
    view.apply(&query, Ok(SetupView::Team(Box::new(team()))));
    view
}

fn frame(view: &TeamView, width: u16, height: u16) -> Vec<String> {
    let look = Look::plain();
    let columns = usize::from(width);
    let items: Vec<(String, String)> = Tab::ALL
        .iter()
        .map(|tab| {
            (
                format!("{} {}", look.glyph(tab.glyph()), tab.label()),
                String::new(),
            )
        })
        .collect();
    let mut rows: Vec<Row> = tab_strip(columns, &items, Tab::Agents as usize, &look).into();
    let room = usize::from(height) - 4 - 2;
    let (lines, help) = view.rows(columns - 4, room, &accounts(), &look);
    for line in lines.into_iter().take(room) {
        rows.push(Row::of("  ", Style::new()).cat(line).pad(columns));
    }
    let screen = Screen {
        rows,
        cursor: None,
        crumbs: vec!["Home".into(), "Tidepool".into(), "Agents".into()],
        help,
        status: None,
    };
    let text = compose(&screen, width, height, "", &look);
    let lines: Vec<String> = text.lines().map(str::to_owned).collect();
    assert_eq!(lines.len(), usize::from(height));
    lines
}

fn find<'a>(rows: &'a [String], needle: &str) -> &'a str {
    rows.iter()
        .find(|row| row.contains(needle))
        .unwrap_or_else(|| panic!("no row has {needle:?}:\n{}", rows.join("\n")))
}

fn typed(view: &mut TeamView, text: &str) {
    for character in text.chars() {
        assert_eq!(
            view.key(press(KeyCode::Char(character))),
            TeamReply::Handled
        );
    }
}

async fn read_team(controller: &mut OwnerController, view: &mut TeamView, now: i64) {
    let query = view.next_query(now).unwrap();
    let read = controller
        .team_status(project())
        .await
        .map(|status| SetupView::Team(Box::new(status)))
        .map_err(|error| error.0);
    view.apply(&query, read);
}

#[test]
fn the_team_draws_agents_and_tasks_at_120_and_80_columns() {
    let view = loaded();
    let wide = frame(&view, 120, 36);
    assert!(find(&wide, "Team").contains("1 working"));
    assert!(find(&wide, "Team").contains("Keep warm  Off"));
    assert!(
        wide.iter()
            .any(|row| row.contains("Codex leads. Claude Code x2. Up to 4 agents at once."))
    );
    let master = find(&wide, "> Master");
    assert!(master.contains("Codex") && master.contains("ChatGPT Pro - main"));
    assert!(master.contains("Waiting for work"));
    let sub = find(&wide, "Sub-agent 2");
    assert!(sub.contains("Claude Max - work"));
    assert!(sub.contains("Working  Build the charts component"));
    assert!(find(&wide, "Sub-agent 4").contains("Stopped"));
    let charts = find(&wide, "    Build the charts component");
    assert!(charts.contains("Master") && charts.contains("Running  6 min"));
    assert!(find(&wide, "Write the readings API").contains("You"));
    assert!(find(&wide, "Set up the project").contains("Done"));
    assert!(
        wide.iter()
            .position(|row| row.contains("Write the readings API"))
            < wide
                .iter()
                .position(|row| row.contains("Set up the project")),
        "unfinished tasks come before finished ones"
    );
    assert!(
        wide.iter()
            .any(|row| row.contains("Account       ChatGPT Pro - main"))
    );
    assert!(find(&wide, "Tools").contains("On, inside the project folder"));
    assert!(wide.last().unwrap().contains("x Cancel task"));
    assert!(
        wide.iter().all(|row| !row.contains("acct-")),
        "account ids never show"
    );

    let narrow = frame(&view, 80, 24);
    let master = find(&narrow, "> Master");
    assert!(master.contains("Codex") && !master.contains("ChatGPT Pro"));
    assert!(
        narrow
            .iter()
            .any(|row| row.contains("Account       ChatGPT Pro - main"))
    );
    assert!(narrow.iter().all(|row| !row.contains("acct-")));
    assert!(narrow.iter().all(|row| row.chars().count() <= 80));
}

#[test]
fn every_team_setting_shows_at_80_columns_on_whole_rows_after_the_counts() {
    use crate::team::events::WaitingAgent;
    let settings = [
        "Keep warm  Off",
        "Task  Off",
        "Session  Off",
        "Idle stop  Off",
        "On harness switch  Carry over",
    ];
    let mut alerted = team();
    alerted.events.waiting = vec![WaitingAgent {
        agent: 2,
        text: "Do you want to proceed?".into(),
    }];
    alerted.events.paused = true;
    let mut busy = TeamView::new(project());
    let query = busy.next_query(NOW).unwrap();
    busy.apply(&query, Ok(SetupView::Team(Box::new(alerted))));
    for (view, counts) in [
        (loaded(), "Team  1 working  1 waiting"),
        (
            busy,
            "Team  1 working  1 waiting  1 waiting for an answer  auto-wake paused",
        ),
    ] {
        let rows = frame(&view, 80, 24);
        assert!(find(&rows, "Team").contains(counts), "{}", rows.join("\n"));
        for setting in settings {
            assert_eq!(
                rows.iter().filter(|row| row.contains(setting)).count(),
                1,
                "{setting:?} shows once, whole:\n{}",
                rows.join("\n")
            );
        }
        assert!(rows.iter().all(|row| row.chars().count() <= 80));
    }
}

#[test]
fn a_sub_agent_has_one_name_in_the_team_list_and_the_team_graph() {
    let graph = crate::team::graph(&team(), &Default::default());
    let working = graph.nodes.iter().find(|node| node.id == "team-2").unwrap();
    assert_eq!(working.label, "Sub-agent 2");
    let mut view = loaded();
    let rows = frame(&view, 120, 36);
    assert!(find(&rows, "Working  Build the charts component").contains("Sub-agent 2"));
    assert!(find(&rows, "Sub-agent 4").contains("Stopped"));
    assert_eq!(view.key(press(KeyCode::Tab)), TeamReply::Handled);
    let rows = frame(&view, 120, 36);
    assert!(
        rows.iter()
            .any(|row| row.contains("Agent         Sub-agent 2"))
    );
}

#[test]
fn a_named_sub_agent_shows_its_persona_and_role_in_the_list_panel_and_graph() {
    let mut status = team();
    status.agents[1].persona = Some("Ada".into());
    status.agents[1].handle = Some("charts".into());
    status.agents[1].label = "Ada (@charts)".into();
    let graph = crate::team::graph(&status, &Default::default());
    let named = graph.nodes.iter().find(|node| node.id == "team-2").unwrap();
    assert_eq!(named.label, "Ada (@charts)");
    let mut view = TeamView::new(project());
    let query = view.next_query(NOW).unwrap();
    view.apply(&query, Ok(SetupView::Team(Box::new(status))));
    let rows = frame(&view, 120, 36);
    assert!(find(&rows, "Ada (@charts)").contains("Working  Build the charts component"));
    view.key(press(KeyCode::Down));
    let rows = frame(&view, 120, 36);
    assert!(
        rows.iter()
            .any(|row| row.contains("Ada (@charts) \u{b7} Claude Code \u{b7} working")),
        "{}",
        rows.join("\n")
    );
    assert_eq!(view.key(press(KeyCode::Tab)), TeamReply::Handled);
    let rows = frame(&view, 120, 36);
    assert!(
        rows.iter()
            .any(|row| row.contains("Agent         Ada (@charts)"))
    );
}

#[test]
fn every_sub_agent_row_shows_its_current_task_or_its_last_one() {
    let mut status = team();
    status.tasks[0].agent = Some(4);
    let mut view = TeamView::new(project());
    let query = view.next_query(NOW).unwrap();
    view.apply(&query, Ok(SetupView::Team(Box::new(status))));
    let rows = frame(&view, 120, 36);
    assert!(find(&rows, "Sub-agent 2").contains("Working  Build the charts component"));
    assert!(
        find(&rows, "Sub-agent 4").contains("Stopped  Last task: Set up the project"),
        "{}",
        rows.join("\n")
    );
    view.key(press(KeyCode::Down));
    view.key(press(KeyCode::Down));
    assert_eq!(view.agent_cursor, 2);
    let rows = frame(&view, 120, 36);
    assert!(
        rows.iter()
            .any(|row| row.contains("Last task     Set up the project")),
        "{}",
        rows.join("\n")
    );
}

#[test]
fn the_panel_follows_the_focused_task() {
    let mut view = loaded();
    assert_eq!(view.key(press(KeyCode::Tab)), TeamReply::Handled);
    assert_eq!(view.focus, TeamFocus::Tasks);
    let rows = frame(&view, 80, 24);
    assert!(find(&rows, "Task from the master").contains("running 6 min"));
    assert!(find(&rows, "Use the readings API.").starts_with("  "));
    assert!(
        rows.iter()
            .any(|row| row.contains("Agent         Sub-agent 2"))
    );
    view.key(press(KeyCode::Down));
    view.key(press(KeyCode::Down));
    let rows = frame(&view, 80, 24);
    assert!(find(&rows, "Result").contains("Created the Next.js app"));
    view.key(press(KeyCode::Down));
    assert_eq!(view.task_cursor, 2, "the cursor stops at the last task");
}

#[test]
fn cancel_asks_first_then_sends_one_change() {
    let mut view = loaded();
    view.key(press(KeyCode::Down));
    assert_eq!(view.key(press(KeyCode::Char('x'))), TeamReply::Handled);
    assert_eq!(
        view.confirm,
        Some(11),
        "x on a working agent cancels its task"
    );
    let rows = frame(&view, 80, 24);
    assert!(find(&rows, "Cancel \"Build the charts component\"?").contains("y Cancel task"));
    assert!(rows.last().unwrap().contains("n Keep"));
    assert_eq!(view.key(press(KeyCode::Char('n'))), TeamReply::Handled);
    assert_eq!(view.confirm, None);
    view.key(press(KeyCode::Char('x')));
    let reply = view.key(press(KeyCode::Char('y')));
    let TeamReply::Change(action) = reply else {
        panic!("a cancel change expected, got {reply:?}");
    };
    assert_eq!(
        action,
        TeamAction::Cancel {
            project_id: project(),
            task: Some(11)
        }
    );
    assert!(view.busy());
    view.key(press(KeyCode::Char('x')));
    let reply = view.key(press(KeyCode::Char('y')));
    assert_eq!(reply, TeamReply::Handled, "one change at a time");
    assert!(view.notice.as_ref().unwrap().error);
    view.apply(
        &SetupQuery::TeamChange {
            action,
            generation: 7,
        },
        Ok(SetupView::Changed(OwnerResult::Applied)),
    );
    assert!(!view.busy());
    assert_eq!(
        view.notice,
        Some(Notice::plain("Stopping \"Build the charts component\"."))
    );
    assert!(
        view.next_query(NOW + 1).is_some(),
        "a change reads the team again at once"
    );
}

#[test]
fn nothing_to_cancel_on_an_idle_agent_or_a_finished_task() {
    let mut view = loaded();
    view.key(press(KeyCode::Char('x')));
    assert_eq!(view.confirm, None);
    assert_eq!(
        view.notice,
        Some(Notice::plain("There is no running or waiting task here."))
    );
    view.key(press(KeyCode::Tab));
    view.key(press(KeyCode::Down));
    view.key(press(KeyCode::Down));
    view.key(press(KeyCode::Char('x')));
    assert_eq!(view.confirm, None, "a done task cannot be cancelled");
}

#[test]
fn a_refused_change_shows_the_plain_reason() {
    let mut view = loaded();
    view.key(press(KeyCode::Tab));
    view.key(press(KeyCode::Down));
    view.key(press(KeyCode::Char('x')));
    let TeamReply::Change(action) = view.key(press(KeyCode::Char('y'))) else {
        panic!("a cancel change expected");
    };
    assert_eq!(
        action,
        TeamAction::Cancel {
            project_id: project(),
            task: Some(12)
        }
    );
    view.apply(
        &SetupQuery::TeamChange {
            action,
            generation: 8,
        },
        Ok(SetupView::Changed(OwnerResult::Rejected {
            code: "team_task_missing".into(),
        })),
    );
    assert_eq!(
        view.notice,
        Some(Notice::problem("That task is gone already."))
    );
    assert!(!view.busy());
    let rows = frame(&view, 80, 24);
    assert!(
        rows.iter()
            .any(|row| row.contains("That task is gone already."))
    );
}

#[test]
fn keep_warm_turns_on_and_off() {
    let mut view = TeamView::new(project());
    assert_eq!(view.key(press(KeyCode::Char('w'))), TeamReply::Handled);
    assert_eq!(
        view.notice,
        Some(Notice::plain("The team is still loading."))
    );
    let mut view = loaded();
    let reply = view.key(press(KeyCode::Char('w')));
    assert_eq!(
        reply,
        TeamReply::Change(TeamAction::KeepWarm {
            project_id: project(),
            on: true
        })
    );
    let TeamReply::Change(action) = reply else {
        unreachable!();
    };
    view.apply(
        &SetupQuery::TeamChange {
            action,
            generation: 9,
        },
        Ok(SetupView::Changed(OwnerResult::Applied)),
    );
    assert_eq!(
        view.notice,
        Some(Notice::plain("Keeping 1 agent ready between tasks."))
    );
    let mut warm = team();
    warm.keep_warm = true;
    let query = view.next_query(NOW + 10).unwrap();
    view.apply(&query, Ok(SetupView::Team(Box::new(warm))));
    assert!(find(&frame(&view, 80, 24), "Team").contains("Keep warm  On"));
    assert_eq!(
        view.key(press(KeyCode::Char('w'))),
        TeamReply::Change(TeamAction::KeepWarm {
            project_id: project(),
            on: false
        })
    );
}

#[test]
fn reads_are_paced_and_other_projects_are_ignored() {
    let mut view = TeamView::new(project());
    assert!(
        frame(&view, 80, 24)
            .iter()
            .any(|row| row.contains("Loading..."))
    );
    let query = view.next_query(NOW).unwrap();
    assert_eq!(query, SetupQuery::Team { project: project() });
    assert!(view.next_query(NOW + 1).is_none(), "one read at a time");
    view.forget_reads();
    let query = view.next_query(NOW + 2).unwrap();
    view.apply(
        &SetupQuery::Team {
            project: ProjectId::new("ledgerline").unwrap(),
        },
        Ok(SetupView::Team(Box::new(team()))),
    );
    assert!(view.status.is_none(), "another project's team is ignored");
    view.apply(&query, Err("team_engine_not_ready".into()));
    assert_eq!(
        view.notice,
        Some(Notice::problem(
            "The engine is still starting. Try again in a moment."
        ))
    );
    assert!(
        view.next_query(NOW + 3).is_none(),
        "a failed read waits for the next period"
    );
    assert!(view.next_query(NOW + 2 + REFRESH_MS).is_some());
    assert_eq!(view.key(press(KeyCode::Char('q'))), TeamReply::Ignored);
}

#[test]
fn lists_longer_than_the_room_scroll_with_the_cursor() {
    let mut status = team();
    for id in 20..40 {
        status.tasks.push(task(
            id,
            TaskSource::Master,
            &format!("Task number {id}"),
            TaskStatus::Queued,
        ));
    }
    let mut view = TeamView::new(project());
    let query = view.next_query(NOW).unwrap();
    view.apply(&query, Ok(SetupView::Team(Box::new(status))));
    view.key(press(KeyCode::Tab));
    for _ in 0..21 {
        view.key(press(KeyCode::Down));
    }
    let rows = frame(&view, 80, 24);
    assert!(find(&rows, "> ").contains("Task number 39"));
    assert!(rows.iter().all(|row| !row.contains("Task number 20")));
    assert_eq!(window(0, 0, 3), 0);
    assert_eq!(window(9, 10, 4), 6);
    assert_eq!(window(2, 10, 4), 0);
}

#[test]
#[should_panic(expected = "assertion failed")]
fn a_window_must_hold_at_least_one_row() {
    window(3, 3, 0);
}

#[test]
#[should_panic(expected = "assertion `left == right` failed")]
fn a_team_answer_for_another_project_is_a_bug() {
    let mut view = TeamView::new(ProjectId::new("ledgerline").unwrap());
    let query = view.next_query(NOW).unwrap();
    view.apply(&query, Ok(SetupView::Team(Box::new(team()))));
}

#[test]
fn titles_and_plain_reasons() {
    assert_eq!(
        title("\n  Fix the login bug  \nDetails"),
        "Fix the login bug"
    );
    assert_eq!(title(" \n "), "Untitled task");
    assert_eq!(
        team_plain("team_state_not_private"),
        "The engine cannot read the team safely right now."
    );
    assert_eq!(
        team_plain("something_else"),
        plain_problem("something_else")
    );
    assert_eq!(short_path("/a/b/worktrees/agent-3/"), "worktrees/agent-3");
    assert_eq!(
        account_name(Some("acct-404"), &accounts()),
        "A pool account"
    );
    assert_eq!(account_name(None, &accounts()), "-");
}

#[tokio::test]
async fn a_real_engine_team_draws_and_keeps_warm() {
    let root = tempfile::Builder::new()
        .prefix("ca-tui-team-")
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let (_stop, stopped) = tokio::sync::watch::channel(false);
    let config = crate::tui::test_home::engine_config();
    let _service = EngineService::start(root.path(), "e.sock", &config, &stopped)
        .await
        .unwrap();
    let ordinary = Client::new(
        root.path().join("e.sock"),
        crate::tui::test_home::ENGINE_WAIT,
    )
    .unwrap();
    let (_, mut controller) = OwnerController::attach_configured(ordinary, OwnerClientKind::Tui)
        .await
        .unwrap();
    let _home = crate::tui::test_home::projects_home(&mut controller, 1).await;
    let created = controller
        .setup_project(
            project(),
            ProjectSpec {
                name: "Tidepool".into(),
                repositories: vec![],
            },
            ProjectSetup {
                about: "Live dashboard for sensor buoys.".into(),
                tech_stack: vec![],
                platforms: vec![Platform::Website],
                research: vec![],
                pool: None,
                team: None,
                documents: vec![],
            },
            1,
        )
        .await
        .unwrap();
    assert!(matches!(
        created,
        OwnerResponse::Applied { .. } | OwnerResponse::Committed { .. }
    ));

    let mut view = TeamView::new(project());
    let query = view.next_query(NOW).unwrap();
    let read = controller
        .team_status(project())
        .await
        .map(|status| SetupView::Team(Box::new(status)))
        .map_err(|error| error.0);
    view.apply(&query, read);
    let rows = frame(&view, 120, 36);
    assert!(find(&rows, "Team").contains("Keep warm  Off"));
    assert!(rows.iter().any(|row| row.contains("Codex leads.")));
    assert!(
        rows.iter()
            .any(|row| row.contains("No agents are running."))
    );
    assert!(rows.iter().any(|row| row.contains("No tasks yet.")));
    frame(&view, 80, 24);

    let TeamReply::Change(action) = view.key(press(KeyCode::Char('w'))) else {
        panic!("keep warm must ask the engine");
    };
    let response = controller.team_action(action.clone(), 2).await.unwrap();
    let result = match response {
        OwnerResponse::Applied { operation } | OwnerResponse::Committed { operation } => {
            SetupView::Changed(operation.result)
        }
        other => panic!("team change not applied: {other:?}"),
    };
    view.apply(
        &SetupQuery::TeamChange {
            action,
            generation: 2,
        },
        Ok(result),
    );
    assert!(!view.notice.as_ref().unwrap().error, "{:?}", view.notice);
    let query = view.next_query(NOW + 1).unwrap();
    let read = controller
        .team_status(project())
        .await
        .map(|status| SetupView::Team(Box::new(status)))
        .map_err(|error| error.0);
    view.apply(&query, read);
    assert!(view.status.as_ref().unwrap().keep_warm);
    assert!(find(&frame(&view, 80, 24), "Team").contains("Keep warm  On"));
}

#[test]
fn a_new_task_goes_to_the_chosen_sub_agent() {
    let mut view = loaded();
    assert!(!view.typing());
    assert_eq!(view.key(press(KeyCode::Char('n'))), TeamReply::Handled);
    assert!(view.typing());
    let wide = frame(&view, 120, 36);
    assert!(find(&wide, "New task from you").contains("For  < Any sub-agent >"));
    assert!(
        wide.iter()
            .any(|row| row.contains("[ What should the sub-agent do?"))
    );
    assert!(wide.last().unwrap().contains("Enter Hand over"));
    assert!(wide.last().unwrap().contains("Tab Sub-agent"));

    view.key(press(KeyCode::Tab));
    assert_eq!(
        view.draft.as_ref().unwrap().harness.as_deref(),
        Some("claude-code")
    );
    assert!(find(&frame(&view, 120, 36), "New task from you").contains("< Claude Code >"));
    view.key(press(KeyCode::Tab));
    assert_eq!(
        view.draft.as_ref().unwrap().harness,
        None,
        "Tab comes back round to any sub-agent"
    );
    view.key(press(KeyCode::BackTab));
    assert_eq!(
        view.draft.as_ref().unwrap().harness.as_deref(),
        Some("claude-code")
    );

    typed(&mut view, "Fix the chart legend");
    assert_eq!(
        view.draft.as_ref().unwrap().input.value(),
        "Fix the chart legend"
    );
    assert_eq!(view.confirm, None, "x types a letter while writing");
    assert!(!view.busy(), "letters never send a change while writing");
    view.key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::CONTROL));
    assert!(view.paste("Keep the colours\tthe same."));
    let (lines, _, cursor) = view.draw(116, 30, &accounts(), &Look::plain());
    let (row, column) = cursor.expect("the composer places the cursor");
    assert!(
        lines[row]
            .text()
            .starts_with("[ Keep the colours the same."),
        "{:?}",
        lines[row].text()
    );
    assert_eq!(column, 2 + "Keep the colours the same.".len());

    let narrow = frame(&view, 80, 24);
    assert!(
        narrow
            .iter()
            .any(|row| row.contains("[ Fix the chart legend"))
    );
    assert!(
        narrow
            .iter()
            .any(|row| row.contains("[ Keep the colours the same."))
    );
    assert!(narrow.iter().all(|row| row.chars().count() <= 80));

    let reply = view.key(press(KeyCode::Enter));
    let TeamReply::Change(action) = reply else {
        panic!("a delegate change expected, got {reply:?}");
    };
    assert_eq!(
        action,
        TeamAction::Delegate {
            project_id: project(),
            harness: Some("claude-code".into()),
            prompt: "Fix the chart legend\nKeep the colours the same.".into(),
        }
    );
    assert!(view.busy());
    assert!(view.typing(), "the text stays until the engine takes it");
    assert_eq!(
        view.key(press(KeyCode::Enter)),
        TeamReply::Handled,
        "one change at a time"
    );
    assert!(view.notice.as_ref().unwrap().error);
    view.apply(
        &SetupQuery::TeamChange {
            action,
            generation: 9,
        },
        Ok(SetupView::Changed(OwnerResult::Applied)),
    );
    assert!(!view.typing());
    assert!(!view.busy());
    assert_eq!(
        view.notice,
        Some(Notice::plain(
            "Handed \"Fix the chart legend\" to Claude Code."
        ))
    );
    assert!(
        view.next_query(NOW + 1).is_some(),
        "a new task reads the team again at once"
    );
}

#[test]
fn an_empty_task_is_not_sent_and_esc_closes_the_composer() {
    let mut view = loaded();
    view.key(press(KeyCode::Char('n')));
    assert_eq!(view.key(press(KeyCode::Enter)), TeamReply::Handled);
    assert_eq!(
        view.notice,
        Some(Notice::plain("Write what the sub-agent should do first."))
    );
    typed(&mut view, "   ");
    assert_eq!(view.key(press(KeyCode::Enter)), TeamReply::Handled);
    assert!(!view.busy());
    assert!(
        frame(&view, 80, 24)
            .iter()
            .any(|row| row.contains("Write what the sub-agent should do first."))
    );
    assert_eq!(view.key(press(KeyCode::Esc)), TeamReply::Handled);
    assert!(!view.typing());
    assert!(!view.paste("ignored"), "no composer takes no paste");
    let rows = frame(&view, 120, 36);
    assert!(rows.iter().all(|row| !row.contains("New task from you")));
    assert!(rows.last().unwrap().contains("n New task"));
}

#[test]
fn a_refused_task_keeps_its_text_and_shows_why() {
    let mut view = loaded();
    view.key(press(KeyCode::Char('n')));
    assert!(view.paste("Write the alerts page"));
    let TeamReply::Change(action) = view.key(press(KeyCode::Enter)) else {
        panic!("a delegate change expected");
    };
    assert_eq!(
        action,
        TeamAction::Delegate {
            project_id: project(),
            harness: None,
            prompt: "Write the alerts page".into(),
        }
    );
    view.apply(
        &SetupQuery::TeamChange {
            action,
            generation: 10,
        },
        Ok(SetupView::Changed(OwnerResult::Rejected {
            code: "team_queue_full".into(),
        })),
    );
    assert!(view.typing());
    assert!(!view.busy());
    assert_eq!(
        view.draft.as_ref().unwrap().input.value(),
        "Write the alerts page"
    );
    assert_eq!(
        view.notice,
        Some(Notice::problem(
            "Too much work is waiting already. Try again when some finishes."
        ))
    );
    let rows = frame(&view, 80, 24);
    assert!(
        rows.iter()
            .any(|row| row.contains("[ Write the alerts page"))
    );
    assert!(
        rows.iter()
            .any(|row| row.contains("Too much work is waiting already."))
    );
    assert_eq!(
        team_plain("team_has_no_sub_agents"),
        "This team has no sub-agents to hand work to."
    );
    assert_eq!(
        team_plain("team_harness_not_in_plan"),
        "That harness is no longer part of this team."
    );
}

#[test]
fn a_team_without_a_runnable_sub_agent_opens_no_composer() {
    let mut view = TeamView::new(project());
    view.key(press(KeyCode::Char('n')));
    assert!(!view.typing());
    assert_eq!(
        view.notice,
        Some(Notice::plain("The team is still loading."))
    );
    let mut status = team();
    status.harnesses[1].runnable = false;
    let query = view.next_query(NOW).unwrap();
    view.apply(&query, Ok(SetupView::Team(Box::new(status))));
    view.key(press(KeyCode::Char('n')));
    assert!(!view.typing());
    assert_eq!(
        view.notice,
        Some(Notice::plain(
            "This team has no sub-agent that can run yet."
        ))
    );
}

#[tokio::test]
async fn a_real_engine_takes_a_task_from_the_owner() {
    let root = tempfile::Builder::new()
        .prefix("ca-tui-team-task-")
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let (_stop, stopped) = tokio::sync::watch::channel(false);
    let config = crate::tui::test_home::engine_config();
    let _service = EngineService::start(root.path(), "e.sock", &config, &stopped)
        .await
        .unwrap();
    let ordinary = Client::new(
        root.path().join("e.sock"),
        crate::tui::test_home::ENGINE_WAIT,
    )
    .unwrap();
    let (_, mut controller) = OwnerController::attach_configured(ordinary, OwnerClientKind::Tui)
        .await
        .unwrap();
    let _home = crate::tui::test_home::projects_home(&mut controller, 1).await;
    let created = controller
        .setup_project(
            project(),
            ProjectSpec {
                name: "Tidepool".into(),
                repositories: vec![],
            },
            ProjectSetup {
                about: "Live dashboard for sensor buoys.".into(),
                tech_stack: vec![],
                platforms: vec![Platform::Website],
                research: vec![],
                pool: None,
                team: Some(TeamPlan {
                    master: "codex".into(),
                    master_tools: true,
                    sub_harnesses: vec![HarnessSlot {
                        harness: "claude-code".into(),
                        agents: 1,
                        tools: true,
                    }],
                    min_agents: 1,
                    max_agents: 2,
                }),
                documents: vec![],
            },
            1,
        )
        .await
        .unwrap();
    assert!(matches!(
        created,
        OwnerResponse::Applied { .. } | OwnerResponse::Committed { .. }
    ));

    let mut view = TeamView::new(project());
    read_team(&mut controller, &mut view, NOW).await;
    assert!(
        frame(&view, 120, 36)
            .iter()
            .any(|row| row.contains("Codex leads. Claude Code x1."))
    );
    view.key(press(KeyCode::Char('n')));
    assert!(view.typing(), "{:?}", view.notice);
    assert!(view.paste("Write the readings API"));
    view.key(press(KeyCode::Tab));
    let TeamReply::Change(action) = view.key(press(KeyCode::Enter)) else {
        panic!("a delegate change expected");
    };
    let response = controller.team_action(action.clone(), 2).await.unwrap();
    let result = match response {
        OwnerResponse::Applied { operation } | OwnerResponse::Committed { operation } => {
            SetupView::Changed(operation.result)
        }
        other => panic!("team change not applied: {other:?}"),
    };
    view.apply(
        &SetupQuery::TeamChange {
            action,
            generation: 2,
        },
        Ok(result),
    );
    assert!(!view.typing(), "{:?}", view.notice);
    assert_eq!(
        view.notice,
        Some(Notice::plain(
            "Handed \"Write the readings API\" to Claude Code."
        ))
    );

    read_team(&mut controller, &mut view, NOW + 1).await;
    let tasks = view.tasks();
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].source, TaskSource::Owner);
    assert_eq!(tasks[0].harness, "claude-code");
    assert_eq!(
        tasks[0].status,
        TaskStatus::NeedsPool,
        "without a pool the task waits and nothing starts"
    );
    let row = find(&frame(&view, 120, 36), "Needs a pool").to_owned();
    assert!(row.contains("Write the readings API") && row.contains("You"));
    assert!(find(&frame(&view, 80, 24), "Needs a pool").contains("Write the readings API"));
}

#[test]
fn a_waiting_sub_agent_a_paused_wake_and_a_new_notification_show_and_s_marks_it_seen() {
    use crate::team::events::{Notification, NotificationKind, WaitingAgent};
    let mut status = team();
    status.events.waiting = vec![WaitingAgent {
        agent: 2,
        text: "Do you want to proceed? | 1. Yes | 2. No".into(),
    }];
    status.events.paused = true;
    status.events.notifications = [(8, None), (7, None), (5, Some(NOW))]
        .into_iter()
        .map(|(id, seen_ms)| Notification {
            id,
            project_id: project(),
            kind: NotificationKind::Escalation,
            text: format!("Pick the database {id}"),
            created_ms: NOW,
            seen_ms,
        })
        .collect();
    let mut view = TeamView::new(project());
    let query = view.next_query(NOW).unwrap();
    view.apply(&query, Ok(SetupView::Team(Box::new(status))));
    view.key(press(KeyCode::Down));
    let rows = frame(&view, 120, 36);
    let summary = find(&rows, "Team");
    assert!(summary.contains("1 waiting for an answer"), "{summary}");
    assert!(summary.contains("auto-wake paused"), "{summary}");
    assert!(find(&rows, "Asks:").contains("Do you want to proceed? | 1. Yes | 2. No"));
    assert!(rows.iter().any(|row| row.contains("Pick the database 8")));
    assert!(view.help().contains(&("s", "Seen")));
    assert_eq!(
        view.key(press(KeyCode::Char('s'))),
        TeamReply::Change(TeamAction::Seen {
            project_id: project(),
            upto: 8
        })
    );
    let mut quiet = loaded();
    assert_eq!(quiet.key(press(KeyCode::Char('s'))), TeamReply::Handled);
    assert_eq!(
        quiet.notice,
        Some(Notice::plain("There are no new notifications."))
    );
}

#[test]
fn a_opens_the_notifications_newest_first_with_their_age_and_s_marks_them_seen() {
    use crate::team::events::{Notification, NotificationKind};
    let mut status = team();
    status.events.notifications = [
        (9, None, 3 * MINUTE),
        (6, None, 120 * MINUTE),
        (4, Some(NOW), 3 * 24 * 60 * MINUTE),
    ]
    .into_iter()
    .map(|(id, seen_ms, age)| Notification {
        id,
        project_id: project(),
        kind: if id == 6 {
            NotificationKind::WakePaused
        } else {
            NotificationKind::Escalation
        },
        text: format!("Pick the database {id}\nsecond line"),
        created_ms: NOW - age,
        seen_ms,
    })
    .collect();
    let mut view = TeamView::new(project());
    let query = view.next_query(NOW).unwrap();
    view.apply(&query, Ok(SetupView::Team(Box::new(status))));
    assert!(view.help().contains(&("a", "Notifications")));
    let closed = frame(&view, 120, 36);
    assert!(!closed.iter().any(|row| row.contains("Pick the database 4")));

    assert_eq!(view.key(press(KeyCode::Char('a'))), TeamReply::Handled);
    for (width, height) in [(120, 36), (80, 24)] {
        let rows = frame(&view, width, height);
        assert!(find(&rows, "Notifications").contains("2 new"), "{rows:?}");
        let at: Vec<usize> = [9, 6, 4]
            .iter()
            .map(|id| {
                let needle = format!("Pick the database {id}");
                assert_eq!(
                    rows.iter().filter(|row| row.contains(&needle)).count(),
                    1,
                    "{needle} shows once"
                );
                rows.iter().position(|row| row.contains(&needle)).unwrap()
            })
            .collect();
        assert!(at[0] < at[1] && at[1] < at[2], "newest first: {at:?}");
        let newest = find(&rows, "Pick the database 9");
        assert!(
            newest.contains("New") && newest.contains("3 min ago"),
            "{newest}"
        );
        assert!(find(&rows, "Pick the database 6").contains("2 h ago"));
        let seen = find(&rows, "Pick the database 4");
        assert!(
            seen.contains("Seen") && seen.contains("3 days ago"),
            "{seen}"
        );
        assert!(!rows.iter().any(|row| row.contains("second line")));
    }
    assert_eq!(view.help(), vec![("s", "Seen"), ("Esc", "Close")]);
    assert_eq!(
        view.key(press(KeyCode::Char('s'))),
        TeamReply::Change(TeamAction::Seen {
            project_id: project(),
            upto: 9
        })
    );
    assert_eq!(view.key(press(KeyCode::Esc)), TeamReply::Handled);
    assert!(
        !frame(&view, 120, 36)
            .iter()
            .any(|row| row.contains("Pick the database 4"))
    );
    assert!(view.help().contains(&("e", "Wake rules")));

    let mut quiet = loaded();
    assert!(!quiet.help().contains(&("a", "Notifications")));
    assert_eq!(quiet.key(press(KeyCode::Char('a'))), TeamReply::Handled);
    assert!(find(&frame(&quiet, 80, 24), "No notifications yet.").contains("No notifications"));
    assert_eq!(quiet.help(), vec![("Esc", "Close")]);
}

#[test]
fn a_long_notification_list_stays_inside_a_short_window() {
    use crate::team::events::{Notification, NotificationKind};
    let mut status = team();
    status.events.notifications = (1..=20)
        .rev()
        .map(|id| Notification {
            id,
            project_id: project(),
            kind: NotificationKind::Escalation,
            text: format!("Question number {id:02}"),
            created_ms: NOW - id * MINUTE,
            seen_ms: None,
        })
        .collect();
    let mut view = TeamView::new(project());
    let query = view.next_query(NOW).unwrap();
    view.apply(&query, Ok(SetupView::Team(Box::new(status))));
    view.key(press(KeyCode::Char('a')));
    let room = 24 - 4 - 2;
    let look = Look::plain();
    let (lines, _) = view.rows(76, room, &accounts(), &look);
    assert!(lines.len() <= room, "{} rows for {room}", lines.len());
    let text: Vec<String> = lines.iter().map(Row::text).collect();
    assert!(text.iter().any(|row| row.contains("Question number 20")));
    let more = text
        .iter()
        .find(|row| row.contains(" older"))
        .unwrap_or_else(|| panic!("{text:?}"));
    let shown = text
        .iter()
        .filter(|row| row.contains("Question number"))
        .count();
    assert!(more.contains(&format!("{} older", 20 - shown)), "{more}");
}

async fn wake_change(
    view: &mut TeamView,
    controller: &mut OwnerController,
    key: KeyCode,
    now: i64,
) {
    let TeamReply::Change(action) = view.key(press(key)) else {
        panic!("the key must ask the engine");
    };
    let generation = u64::try_from(now - NOW).unwrap() + 2;
    let response = controller
        .team_action(action.clone(), generation)
        .await
        .unwrap();
    let result = match response {
        OwnerResponse::Applied { operation } | OwnerResponse::Committed { operation } => {
            SetupView::Changed(operation.result)
        }
        other => panic!("wake change not applied: {other:?}"),
    };
    view.apply(&SetupQuery::TeamChange { action, generation }, Ok(result));
    assert!(!view.notice.as_ref().unwrap().error, "{:?}", view.notice);
    read_team(controller, view, now).await;
}

#[tokio::test]
async fn a_real_engine_keeps_the_wake_rules_the_owner_sets_at_each_level() {
    let root = tempfile::Builder::new()
        .prefix("ca-tui-wake-")
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let (_stop, stopped) = tokio::sync::watch::channel(false);
    let config = crate::tui::test_home::engine_config();
    let _service = EngineService::start(root.path(), "e.sock", &config, &stopped)
        .await
        .unwrap();
    let ordinary = Client::new(
        root.path().join("e.sock"),
        crate::tui::test_home::ENGINE_WAIT,
    )
    .unwrap();
    let (_, mut controller) = OwnerController::attach_configured(ordinary, OwnerClientKind::Tui)
        .await
        .unwrap();
    let _home = crate::tui::test_home::projects_home(&mut controller, 1).await;
    controller
        .setup_project(
            project(),
            ProjectSpec {
                name: "Tidepool".into(),
                repositories: vec![],
            },
            ProjectSetup {
                about: "Live dashboard for sensor buoys.".into(),
                tech_stack: vec![],
                platforms: vec![Platform::Website],
                research: vec![],
                pool: None,
                team: None,
                documents: vec![],
            },
            1,
        )
        .await
        .unwrap();
    let mut view = TeamView::new(project());
    read_team(&mut controller, &mut view, NOW).await;
    assert_eq!(view.key(press(KeyCode::Char('e'))), TeamReply::Handled);
    let rows = frame(&view, 120, 36);
    assert!(find(&rows, "Wake the master for this project").contains("Uses the app rule"));
    assert!(
        rows.iter()
            .any(|row| row.contains("[ ] A sub-agent reports progress"))
    );
    assert!(
        rows.iter()
            .any(|row| row.contains("Wakes an hour before pausing  30"))
    );

    for _ in 0..4 {
        view.key(press(KeyCode::Down));
    }
    wake_change(&mut view, &mut controller, KeyCode::Char(' '), NOW + 1).await;
    let wake = &view.status.as_ref().unwrap().events.wake;
    assert_eq!(
        wake.project,
        Some(WakeRule {
            progress: true,
            ..WakeRule::default()
        })
    );
    assert!(find(&frame(&view, 120, 36), "Wake the master for this project").contains("Set here"));

    assert_eq!(view.key(press(KeyCode::Tab)), TeamReply::Handled);
    view.key(press(KeyCode::Down));
    wake_change(&mut view, &mut controller, KeyCode::Right, NOW + 2).await;
    assert_eq!(
        controller.team_events().await.unwrap().rule,
        Some(WakeRule {
            hourly_cap: 31,
            ..WakeRule::default()
        })
    );
    assert!(find(&frame(&view, 120, 36), "Wake the master for every project").contains("Set here"));
    wake_change(&mut view, &mut controller, KeyCode::Char('r'), NOW + 3).await;
    assert_eq!(controller.team_events().await.unwrap().rule, None);
    assert_eq!(view.status.as_ref().unwrap().events.wake.app, None);
    assert_eq!(view.key(press(KeyCode::Char('r'))), TeamReply::Handled);
    assert_eq!(
        view.notice,
        Some(Notice::plain("This level already uses the rule above it."))
    );
    assert_eq!(view.key(press(KeyCode::Esc)), TeamReply::Handled);
    assert!(view.wake.is_none());
}

#[test]
fn the_task_and_session_limits_are_off_until_set_and_step_through_the_choices() {
    let mut view = TeamView::new(project());
    assert_eq!(view.key(press(KeyCode::Char('t'))), TeamReply::Handled);
    let mut view = loaded();
    assert!(
        find(&frame(&view, 120, 24), "Team").contains("Task  Off  Session  Off  Idle stop  Off")
    );
    let reply = view.key(press(KeyCode::Char('t')));
    assert_eq!(
        reply,
        TeamReply::Change(TeamAction::TimeLimits {
            project_id: project(),
            turn_secs: Some(1800),
            session_secs: None,
            idle_secs: None
        })
    );
    let TeamReply::Change(action) = reply else {
        unreachable!();
    };
    view.apply(
        &SetupQuery::TeamChange {
            action,
            generation: 9,
        },
        Ok(SetupView::Changed(OwnerResult::Applied)),
    );
    assert_eq!(
        view.notice,
        Some(Notice::plain(
            "Each task may now run up to 30 min, each agent session as long as it needs."
        ))
    );
    let mut limited = team();
    limited.turn_limit_secs = Some(28_800);
    limited.session_limit_secs = Some(28_800);
    let query = view.next_query(NOW + 10).unwrap();
    view.apply(&query, Ok(SetupView::Team(Box::new(limited))));
    assert!(find(&frame(&view, 120, 24), "Team").contains("Task  8 h  Session  8 h"));
    assert_eq!(
        view.key(press(KeyCode::Char('l'))),
        TeamReply::Change(TeamAction::TimeLimits {
            project_id: project(),
            turn_secs: Some(28_800),
            session_secs: None,
            idle_secs: None
        }),
        "past 8 h the session limit goes Off; an Off session holds any task limit"
    );
    let mut view = TeamView::new(project());
    let mut limited = team();
    limited.turn_limit_secs = Some(28_800);
    limited.session_limit_secs = Some(28_800);
    let query = view.next_query(NOW).unwrap();
    view.apply(&query, Ok(SetupView::Team(Box::new(limited))));
    assert_eq!(
        view.key(press(KeyCode::Char('t'))),
        TeamReply::Change(TeamAction::TimeLimits {
            project_id: project(),
            turn_secs: None,
            session_secs: Some(28_800),
            idle_secs: None
        }),
        "a task limit turned Off leaves the session limit"
    );
    let mut view = loaded();
    assert_eq!(
        view.key(press(KeyCode::Char('l'))),
        TeamReply::Change(TeamAction::TimeLimits {
            project_id: project(),
            turn_secs: None,
            session_secs: Some(1800),
            idle_secs: None
        }),
        "a session limit needs no task limit"
    );
    let mut idle = team();
    idle.idle_stop_secs = Some(crate::team::IDLE_STOP_DEFAULT_SECS);
    let mut view = TeamView::new(project());
    let query = view.next_query(NOW).unwrap();
    view.apply(&query, Ok(SetupView::Team(Box::new(idle))));
    assert!(find(&frame(&view, 120, 24), "Team").contains("Idle stop  10 min"));
    let reply = view.key(press(KeyCode::Char('i')));
    assert_eq!(
        reply,
        TeamReply::Change(TeamAction::TimeLimits {
            project_id: project(),
            turn_secs: None,
            session_secs: None,
            idle_secs: Some(3600)
        }),
        "the idle stop steps from 10 min to 1 h and leaves the task and session limits"
    );
    let TeamReply::Change(action) = reply else {
        unreachable!();
    };
    view.apply(
        &SetupQuery::TeamChange {
            action,
            generation: 9,
        },
        Ok(SetupView::Changed(OwnerResult::Applied)),
    );
    assert_eq!(
        view.notice,
        Some(Notice::plain(
            "An idle sub-agent now stops after 1 h. An idle master stops 15 min after its work is done."
        ))
    );
    let mut idle = team();
    idle.idle_stop_secs = Some(3600);
    let mut view = TeamView::new(project());
    let query = view.next_query(NOW).unwrap();
    view.apply(&query, Ok(SetupView::Team(Box::new(idle))));
    assert_eq!(
        view.key(press(KeyCode::Char('i'))),
        TeamReply::Change(TeamAction::TimeLimits {
            project_id: project(),
            turn_secs: None,
            session_secs: None,
            idle_secs: None
        }),
        "past 1 h the idle stop goes Off"
    );
}

#[test]
fn the_harness_switch_setting_flips_between_carry_over_and_start_fresh() {
    let mut view = TeamView::new(project());
    assert_eq!(view.key(press(KeyCode::Char('h'))), TeamReply::Handled);
    assert_eq!(
        view.notice,
        Some(Notice::plain("The team is still loading."))
    );
    let mut view = loaded();
    assert!(find(&frame(&view, 120, 36), "Team").contains("On harness switch  Carry over"));
    let reply = view.key(press(KeyCode::Char('h')));
    assert_eq!(
        reply,
        TeamReply::Change(TeamAction::OnHarnessSwitch {
            project_id: project(),
            mode: crate::team::SwitchMode::Fresh,
        })
    );
    let TeamReply::Change(action) = reply else {
        unreachable!();
    };
    view.apply(
        &SetupQuery::TeamChange {
            action,
            generation: 9,
        },
        Ok(SetupView::Changed(OwnerResult::Applied)),
    );
    assert_eq!(
        view.notice,
        Some(Notice::plain(
            "When the master harness changes, the new harness starts fresh."
        ))
    );
    let mut fresh = team();
    fresh.on_harness_switch = crate::team::SwitchMode::Fresh;
    let query = view.next_query(NOW + 10).unwrap();
    view.apply(&query, Ok(SetupView::Team(Box::new(fresh))));
    assert!(find(&frame(&view, 120, 36), "Team").contains("On harness switch  Start fresh"));
    assert_eq!(
        view.key(press(KeyCode::Char('h'))),
        TeamReply::Change(TeamAction::OnHarnessSwitch {
            project_id: project(),
            mode: crate::team::SwitchMode::Carry,
        })
    );
}

/// A team view the engine shortened to fit one reply says older history is hidden.
#[test]
fn a_shortened_team_view_says_older_history_is_hidden() {
    let view = loaded();
    assert!(!find(&frame(&view, 160, 24), "Team").contains("older history hidden"));
    let mut partial = team();
    partial.partial = true;
    let mut view = TeamView::new(project());
    let query = view.next_query(NOW).unwrap();
    view.apply(&query, Ok(SetupView::Team(Box::new(partial))));
    assert!(find(&frame(&view, 160, 24), "Team").contains("older history hidden"));
}

#[test]
fn finished_agents_wait_in_finished_and_a_lead_names_the_tasks_it_handed_out() {
    let read = |status: TeamStatus| {
        let mut view = TeamView::new(project());
        let query = view.next_query(NOW).unwrap();
        view.apply(&query, Ok(SetupView::Team(Box::new(status))));
        frame(&view, 120, 36)
    };
    let mut status = team();
    status.agents[1].label = "Grace (@lead-b)".into();
    status.agents[2].finished = true;
    let mut added = task(13, TaskSource::Master, "Add add(a, b)", TaskStatus::Queued);
    added.parent = Some(2);
    let mut merged = task(
        14,
        TaskSource::Master,
        "Merge the calc work",
        TaskStatus::Queued,
    );
    merged.parent = Some(99);
    status.tasks.push(added);
    status.tasks.push(merged);

    let hidden = read(status.clone());
    assert!(
        hidden.iter().all(|row| !row.contains("Sub-agent 4")),
        "a finished agent leaves the list under Hide:\n{}",
        hidden.join("\n")
    );
    assert!(find(&hidden, "1 finished agent is in Finished").contains("press Esc, then f."));
    assert!(find(&hidden, "Add add(a, b)").contains("Grace (@lead-b)"));
    assert!(find(&hidden, "Merge the calc work").contains("A lead"));
    assert!(find(&hidden, "    Build the charts component").contains("Master"));
    assert!(find(&hidden, "Write the readings API").contains("You"));

    status.show_finished = true;
    let shown = read(status);
    assert!(find(&shown, "Sub-agent 4").contains("Stopped"));
    assert!(
        shown.iter().all(|row| !row.contains("in Finished")),
        "Show keeps finished agents in the list with no pointer"
    );
}

fn shows(view: &TeamView, setting: &str) -> bool {
    frame(view, 120, 48).iter().any(|row| row.contains(setting))
}

fn applied(view: &mut TeamView, reply: TeamReply) {
    let TeamReply::Change(action) = reply else {
        panic!("no change was sent");
    };
    view.apply(
        &SetupQuery::TeamChange {
            action,
            generation: 9,
        },
        Ok(SetupView::Changed(OwnerResult::Applied)),
    );
}

fn reread(view: &mut TeamView, status: TeamStatus, now: i64) {
    let query = view.next_query(now).unwrap();
    view.apply(&query, Ok(SetupView::Team(Box::new(status))));
}

#[test]
fn agent_branches_flip_between_a_named_branch_and_a_detached_head() {
    let mut view = TeamView::new(project());
    assert_eq!(view.key(press(KeyCode::Char('b'))), TeamReply::Handled);
    assert_eq!(
        view.notice,
        Some(Notice::plain("The team is still loading."))
    );
    let mut view = loaded();
    assert!(view.help().contains(&("b", "Agent branches")));
    assert!(shows(&view, "Agent branches  Named branch"));
    let reply = view.key(press(KeyCode::Char('b')));
    assert_eq!(
        reply,
        TeamReply::Change(TeamAction::AgentBranches {
            project_id: project(),
            mode: crate::team::BranchMode::Detached,
        })
    );
    assert_eq!(view.key(press(KeyCode::Char('b'))), TeamReply::Handled);
    assert_eq!(
        view.notice,
        Some(Notice::problem(
            "Still saving the last change. Try again in a moment."
        ))
    );
    applied(&mut view, reply);
    assert_eq!(
        view.notice,
        Some(Notice::plain("New agents now work on a detached HEAD."))
    );
    let mut detached = team();
    detached.agent_branches = crate::team::BranchMode::Detached;
    reread(&mut view, detached, NOW + 10);
    assert!(shows(&view, "Agent branches  Detached"));
    let reply = view.key(press(KeyCode::Char('b')));
    assert_eq!(
        reply,
        TeamReply::Change(TeamAction::AgentBranches {
            project_id: project(),
            mode: crate::team::BranchMode::Named,
        })
    );
    applied(&mut view, reply);
    assert_eq!(
        view.notice,
        Some(Notice::plain("Each new agent now works on its own branch."))
    );
}

#[test]
fn m_edits_the_masters_branch_and_an_empty_name_goes_back_to_the_default() {
    let mut view = TeamView::new(project());
    assert_eq!(view.key(press(KeyCode::Char('m'))), TeamReply::Handled);
    assert_eq!(
        view.notice,
        Some(Notice::plain("The team is still loading."))
    );
    assert!(!view.typing());
    let mut view = loaded();
    assert!(view.help().contains(&("m", "Master's branch")));
    assert!(shows(&view, "Master's branch  Repository default"));
    assert_eq!(view.key(press(KeyCode::Char('m'))), TeamReply::Handled);
    assert!(view.typing());
    assert_eq!(view.help(), vec![("Enter", "Save"), ("Esc", "Close")]);
    assert!(shows(&view, "[ Repository default"));
    typed(&mut view, "release");
    assert!(view.paste("/2.0"));
    let reply = view.key(press(KeyCode::Enter));
    assert_eq!(
        reply,
        TeamReply::Change(TeamAction::MasterBranch {
            project_id: project(),
            branch: Some("release/2.0".into()),
        })
    );
    assert_eq!(view.key(press(KeyCode::Enter)), TeamReply::Handled);
    assert_eq!(
        view.notice,
        Some(Notice::problem(
            "Still saving the last change. Try again in a moment."
        ))
    );
    applied(&mut view, reply);
    assert!(!view.typing());
    assert_eq!(
        view.notice,
        Some(Notice::plain("The master now works on release/2.0."))
    );
    let mut chosen = team();
    chosen.master_branch = Some("release/2.0".into());
    reread(&mut view, chosen, NOW + 10);
    assert!(shows(&view, "Master's branch  release/2.0"));

    assert_eq!(view.key(press(KeyCode::Char('m'))), TeamReply::Handled);
    assert_eq!(
        view.branch_edit.as_ref().map(|edit| edit.input.value()),
        Some("release/2.0")
    );
    for _ in "release/2.0".chars() {
        assert_eq!(view.key(press(KeyCode::Backspace)), TeamReply::Handled);
    }
    typed(&mut view, "  ");
    let reply = view.key(press(KeyCode::Enter));
    assert_eq!(
        reply,
        TeamReply::Change(TeamAction::MasterBranch {
            project_id: project(),
            branch: None,
        })
    );
    let TeamReply::Change(action) = reply else {
        unreachable!();
    };
    view.apply(
        &SetupQuery::TeamChange {
            action,
            generation: 10,
        },
        Ok(SetupView::Changed(OwnerResult::Rejected {
            code: "team_master_branch_invalid".into(),
        })),
    );
    assert!(view.typing(), "a refused name stays in the editor");
    assert_eq!(
        view.notice,
        Some(Notice::problem(
            "Type a branch name such as main or release/2.0, or leave it empty for the repository default."
        ))
    );
    let reply = view.key(press(KeyCode::Enter));
    applied(&mut view, reply);
    assert_eq!(
        view.notice,
        Some(Notice::plain(
            "The master now works on the repository's default branch."
        ))
    );

    assert_eq!(view.key(press(KeyCode::Char('m'))), TeamReply::Handled);
    typed(&mut view, "dev");
    assert_eq!(view.key(press(KeyCode::Esc)), TeamReply::Handled);
    assert!(!view.typing());
    assert!(view.branch_edit.is_none());
    assert!(view.help().contains(&("g", "Ask before merge")));
}

#[test]
fn shift_l_edits_the_land_on_branch_and_an_empty_name_goes_back_to_the_masters_branch() {
    let mut view = TeamView::new(project());
    assert_eq!(view.key(press(KeyCode::Char('L'))), TeamReply::Handled);
    assert_eq!(
        view.notice,
        Some(Notice::plain("The team is still loading."))
    );
    assert!(!view.typing());
    let mut view = loaded();
    assert!(view.help().contains(&("L", "Land on")));
    assert!(shows(&view, "Land on  Master's branch"));
    assert_eq!(view.key(press(KeyCode::Char('L'))), TeamReply::Handled);
    assert!(view.typing());
    assert_eq!(view.help(), vec![("Enter", "Save"), ("Esc", "Close")]);
    assert!(shows(&view, "[ The master's branch"));
    let rows = frame(&view, 120, 48);
    assert!(rows.iter().all(|row| row.chars().count() <= 120));
    let words = rows
        .join(" ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    assert!(
        words.contains(
            "It can be a branch checked out nowhere: then your project folder's files stay as they are."
        ),
        "{}",
        rows.join("\n")
    );
    typed(&mut view, "sta");
    assert!(view.paste("ge"));
    let reply = view.key(press(KeyCode::Enter));
    assert_eq!(
        reply,
        TeamReply::Change(TeamAction::LandOn {
            project_id: project(),
            branch: Some("stage".into()),
        })
    );
    applied(&mut view, reply);
    assert!(!view.typing());
    assert_eq!(view.notice, Some(Notice::plain("Work now lands on stage.")));
    let mut chosen = team();
    chosen.land_on = Some("stage".into());
    reread(&mut view, chosen, NOW + 10);
    assert!(shows(&view, "Land on  stage"));
    assert!(shows(&view, "Master's branch  Repository default"));

    assert_eq!(view.key(press(KeyCode::Char('L'))), TeamReply::Handled);
    assert_eq!(
        view.branch_edit
            .as_ref()
            .map(|edit| (edit.setting, edit.input.value())),
        Some((BranchSetting::LandOn, "stage"))
    );
    for _ in "stage".chars() {
        assert_eq!(view.key(press(KeyCode::Backspace)), TeamReply::Handled);
    }
    let reply = view.key(press(KeyCode::Enter));
    assert_eq!(
        reply,
        TeamReply::Change(TeamAction::LandOn {
            project_id: project(),
            branch: None,
        })
    );
    let TeamReply::Change(action) = reply else {
        unreachable!();
    };
    view.apply(
        &SetupQuery::TeamChange {
            action,
            generation: 10,
        },
        Ok(SetupView::Changed(OwnerResult::Rejected {
            code: "team_land_on_invalid".into(),
        })),
    );
    assert!(view.typing(), "a refused name stays in the editor");
    assert_eq!(
        view.notice,
        Some(Notice::problem(
            "Type a branch name such as stage or release/2.0, or leave it empty for the master's branch."
        ))
    );
    let reply = view.key(press(KeyCode::Enter));
    applied(&mut view, reply);
    assert!(!view.typing());
    assert_eq!(
        view.notice,
        Some(Notice::plain("Work now lands on the master's branch."))
    );

    assert_eq!(view.key(press(KeyCode::Char('m'))), TeamReply::Handled);
    assert_eq!(
        view.branch_edit.as_ref().map(|edit| edit.setting),
        Some(BranchSetting::Master),
        "m still opens the Master's branch"
    );
}

#[test]
fn g_turns_ask_before_merge_on_and_off() {
    let mut view = TeamView::new(project());
    assert_eq!(view.key(press(KeyCode::Char('g'))), TeamReply::Handled);
    assert_eq!(
        view.notice,
        Some(Notice::plain("The team is still loading."))
    );
    let mut view = loaded();
    assert!(view.help().contains(&("g", "Ask before merge")));
    assert!(shows(&view, "Ask before merge  Off"));
    let reply = view.key(press(KeyCode::Char('g')));
    assert_eq!(
        reply,
        TeamReply::Change(TeamAction::AskBeforeMerge {
            project_id: project(),
            on: true,
        })
    );
    applied(&mut view, reply);
    assert_eq!(
        view.notice,
        Some(Notice::plain(
            "The master now asks you before merging a harness."
        ))
    );
    let mut asking = team();
    asking.ask_before_merge = true;
    reread(&mut view, asking, NOW + 10);
    assert!(shows(&view, "Ask before merge  On"));
    let reply = view.key(press(KeyCode::Char('g')));
    assert_eq!(
        reply,
        TeamReply::Change(TeamAction::AskBeforeMerge {
            project_id: project(),
            on: false,
        })
    );
    applied(&mut view, reply);
    assert_eq!(
        view.notice,
        Some(Notice::plain(
            "The master now merges a finished harness without asking."
        ))
    );
}

async fn coord_send(
    view: &mut TeamView,
    controller: &mut OwnerController,
    reply: TeamReply,
    generation: u64,
) -> TeamAction {
    let TeamReply::Change(action) = reply else {
        panic!("the key must ask the engine, got {reply:?}");
    };
    let response = controller
        .team_action(action.clone(), generation)
        .await
        .unwrap();
    let result = match response {
        OwnerResponse::Applied { operation } | OwnerResponse::Committed { operation } => {
            SetupView::Changed(operation.result)
        }
        other => panic!("coordination change not applied: {other:?}"),
    };
    view.apply(
        &SetupQuery::TeamChange {
            action: action.clone(),
            generation,
        },
        Ok(result),
    );
    assert!(!view.notice.as_ref().unwrap().error, "{:?}", view.notice);
    read_team(controller, view, NOW + i64::try_from(generation).unwrap()).await;
    action
}

fn coordination_of(view: &TeamView) -> CoordinationView {
    view.status.as_ref().unwrap().coordination.clone().unwrap()
}

fn harness_row(view: &TeamView, harness: HarnessKind) -> (SubagentMode, Delivery) {
    let row = coordination_of(view)
        .harnesses
        .into_iter()
        .find(|row| row.harness == harness)
        .unwrap();
    (row.mode, row.delivery)
}

fn clear_counted(view: &mut TeamView) {
    let typed = view
        .coord
        .as_ref()
        .and_then(|edit| edit.input.as_ref())
        .unwrap()
        .value()
        .chars()
        .count();
    for _ in 0..typed {
        assert_eq!(view.key(press(KeyCode::Backspace)), TeamReply::Handled);
    }
}

#[tokio::test]
async fn a_real_engine_keeps_the_coordination_the_owner_sets_in_the_editor() {
    let mut old = loaded();
    assert_eq!(old.key(press(KeyCode::Char('c'))), TeamReply::Handled);
    assert_eq!(
        old.notice,
        Some(Notice::plain(
            "This engine does not report coordination settings yet."
        ))
    );
    assert!(old.coord.is_none());

    let root = tempfile::Builder::new()
        .prefix("ca-tui-coord-")
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let (_stop, stopped) = tokio::sync::watch::channel(false);
    let config = crate::tui::test_home::engine_config();
    let _service = EngineService::start(root.path(), "e.sock", &config, &stopped)
        .await
        .unwrap();
    let ordinary = Client::new(
        root.path().join("e.sock"),
        crate::tui::test_home::ENGINE_WAIT,
    )
    .unwrap();
    let (_, mut controller) = OwnerController::attach_configured(ordinary, OwnerClientKind::Tui)
        .await
        .unwrap();
    let _home = crate::tui::test_home::projects_home(&mut controller, 1).await;
    controller
        .setup_project(
            project(),
            ProjectSpec {
                name: "Tidepool".into(),
                repositories: vec![],
            },
            ProjectSetup {
                about: "Live dashboard for sensor buoys.".into(),
                tech_stack: vec![],
                platforms: vec![Platform::Website],
                research: vec![],
                pool: None,
                team: None,
                documents: vec![],
            },
            1,
        )
        .await
        .unwrap();

    let mut view = TeamView::new(project());
    assert_eq!(view.key(press(KeyCode::Char('c'))), TeamReply::Handled);
    assert_eq!(
        view.notice,
        Some(Notice::plain("The team is still loading."))
    );
    read_team(&mut controller, &mut view, NOW).await;
    let (_, help) = view.rows(116, 30, &accounts(), &Look::plain());
    assert!(help.contains(&("c", "Coordination")), "{help:?}");
    assert_eq!(coordination_of(&view).settings, CoordSettings::default());
    assert_eq!(view.key(press(KeyCode::Char('c'))), TeamReply::Handled);
    assert_eq!(view.coord.as_ref().unwrap().row, 0);
    assert!(!view.typing());

    let reply = view.key(press(KeyCode::Right));
    coord_send(&mut view, &mut controller, reply, 2).await;
    assert_eq!(
        harness_row(&view, HarnessKind::ClaudeCode).0,
        SubagentMode::Native
    );

    view.key(press(KeyCode::Down));
    view.key(press(KeyCode::Down));
    assert_eq!(view.key(press(KeyCode::Right)), TeamReply::Handled);
    assert!(
        view.notice
            .as_ref()
            .unwrap()
            .text
            .contains("has no native teammates"),
        "{:?}",
        view.notice
    );
    assert!(!view.busy(), "a refused mode sends nothing");

    view.key(press(KeyCode::Down));
    let reply = view.key(press(KeyCode::Left));
    coord_send(&mut view, &mut controller, reply, 3).await;
    assert_eq!(
        harness_row(&view, HarnessKind::Codex),
        (SubagentMode::AppSpawned, Delivery::TypingOnly)
    );

    for _ in 0..4 {
        view.key(press(KeyCode::Down));
    }
    assert_eq!(view.coord.as_ref().unwrap().row, 7);
    for (generation, overlaps) in [
        (4, Overlaps::Block),
        (5, Overlaps::Off),
        (6, Overlaps::Warn),
    ] {
        let reply = view.key(press(KeyCode::Right));
        coord_send(&mut view, &mut controller, reply, generation).await;
        assert_eq!(coordination_of(&view).settings.overlaps, overlaps);
    }

    view.key(press(KeyCode::Down));
    view.key(press(KeyCode::Down));
    assert_eq!(view.coord.as_ref().unwrap().row, 9);
    assert_eq!(view.key(press(KeyCode::Enter)), TeamReply::Handled);
    assert!(view.typing());
    clear_counted(&mut view);
    typed(&mut view, "bad name!");
    assert_eq!(view.key(press(KeyCode::Enter)), TeamReply::Handled);
    assert_eq!(
        view.notice,
        Some(Notice::problem(team_plain("coord_settings_invalid")))
    );
    assert!(view.typing(), "a refused list stays open for editing");
    clear_counted(&mut view);
    typed(&mut view, "a, a b*");
    let reply = view.key(press(KeyCode::Enter));
    coord_send(&mut view, &mut controller, reply, 7).await;
    assert!(!view.typing(), "an applied list closes the input");
    assert_eq!(coordination_of(&view).settings.counted_tools, ["a", "b*"]);

    let reply = view.key(press(KeyCode::Char('r')));
    coord_send(&mut view, &mut controller, reply, 8).await;
    assert_eq!(
        coordination_of(&view).settings.counted_tools,
        COUNTED_TOOLS_DEFAULT.map(str::to_owned).to_vec()
    );
    assert_eq!(view.key(press(KeyCode::Esc)), TeamReply::Handled);
    assert!(view.coord.is_none());
}

/// DEC-111 R3 through a real engine: the coordination editor lists Master, Lead and Worker
/// skills on All approved skills; Space on Worker opens its names, a name no skill can have
/// is refused, Enter saves the names as Chosen skills, an empty list gives Lead no skills,
/// and Space returns Worker to All approved skills.
#[tokio::test]
async fn skills_per_role_are_set_in_the_coordination_editor_through_a_real_engine() {
    let root = tempfile::Builder::new()
        .prefix("ca-tui-roles-")
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let (_stop, stopped) = tokio::sync::watch::channel(false);
    let config = crate::tui::test_home::engine_config();
    let _service = EngineService::start(root.path(), "e.sock", &config, &stopped)
        .await
        .unwrap();
    let ordinary = Client::new(
        root.path().join("e.sock"),
        crate::tui::test_home::ENGINE_WAIT,
    )
    .unwrap();
    let (_, mut controller) = OwnerController::attach_configured(ordinary, OwnerClientKind::Tui)
        .await
        .unwrap();
    let _home = crate::tui::test_home::projects_home(&mut controller, 1).await;
    controller
        .setup_project(
            project(),
            ProjectSpec {
                name: "Tidepool".into(),
                repositories: vec![],
            },
            ProjectSetup {
                about: "Live dashboard for sensor buoys.".into(),
                tech_stack: vec![],
                platforms: vec![Platform::Website],
                research: vec![],
                pool: None,
                team: None,
                documents: vec![],
            },
            1,
        )
        .await
        .unwrap();

    let mut view = TeamView::new(project());
    read_team(&mut controller, &mut view, NOW).await;
    assert_eq!(view.key(press(KeyCode::Char('c'))), TeamReply::Handled);
    let shown = frame(&view, 120, 40);
    for role in ["Master skills", "Lead skills", "Worker skills"] {
        assert!(find(&shown, role).contains("All approved skills"), "{role}");
    }
    for _ in 0..12 {
        assert_eq!(view.key(press(KeyCode::Down)), TeamReply::Handled);
    }
    assert_eq!(view.coord.as_ref().unwrap().row, 12);
    let (_, help) = view.rows(116, 40, &accounts(), &Look::plain());
    assert!(
        help.contains(&("Space", "All or chosen")) && help.contains(&("Enter", "Edit skills")),
        "{help:?}"
    );

    assert_eq!(view.key(press(KeyCode::Char(' '))), TeamReply::Handled);
    assert!(
        view.typing(),
        "Space on All approved skills opens the names"
    );
    assert!(!view.busy(), "opening the names sends nothing");
    typed(&mut view, "Bad Name");
    assert_eq!(view.key(press(KeyCode::Enter)), TeamReply::Handled);
    assert_eq!(
        view.notice,
        Some(Notice::problem(team_plain("coord_settings_invalid")))
    );
    assert!(view.typing(), "refused names stay open for editing");
    clear_counted(&mut view);
    typed(&mut view, "style-guide, release-notes style-guide");
    let reply = view.key(press(KeyCode::Enter));
    coord_send(&mut view, &mut controller, reply, 2).await;
    assert!(!view.typing(), "applied names close the input");
    assert_eq!(
        coordination_of(&view).settings.role_skills,
        RoleSkills {
            worker: Some(vec!["style-guide".into(), "release-notes".into()]),
            ..RoleSkills::default()
        }
    );
    assert_eq!(
        view.notice,
        Some(Notice::plain("Worker skills: style-guide, release-notes."))
    );
    assert!(
        find(&frame(&view, 120, 40), "Worker skills")
            .contains("Chosen skills: style-guide, release-notes")
    );

    assert_eq!(view.key(press(KeyCode::Up)), TeamReply::Handled);
    assert_eq!(view.key(press(KeyCode::Enter)), TeamReply::Handled);
    assert!(view.typing());
    let reply = view.key(press(KeyCode::Enter));
    coord_send(&mut view, &mut controller, reply, 3).await;
    assert_eq!(
        coordination_of(&view).settings.role_skills.lead,
        Some(Vec::new())
    );
    assert!(find(&frame(&view, 120, 40), "Lead skills").contains("Chosen skills: none"));

    assert_eq!(view.key(press(KeyCode::Down)), TeamReply::Handled);
    let reply = view.key(press(KeyCode::Char(' ')));
    coord_send(&mut view, &mut controller, reply, 4).await;
    assert_eq!(
        coordination_of(&view).settings.role_skills,
        RoleSkills {
            lead: Some(Vec::new()),
            ..RoleSkills::default()
        }
    );
    assert_eq!(
        view.notice,
        Some(Notice::plain("Worker skills: all approved skills."))
    );
    assert!(find(&frame(&view, 120, 40), "Worker skills").contains("All approved skills"));
}

/// Workflow nudges show Off in the coordination editor of a project with no saved settings,
/// read from a real engine database, and Space on the row steps through every option
/// (DEC-111 R4): each press sends its change, the engine's answer names the new option and
/// the row shows the stored one.
#[test]
fn workflow_nudges_show_off_by_default_and_space_steps_through_every_option() {
    let folder = tempfile::tempdir().unwrap();
    let connection = rusqlite::Connection::open(folder.path().join("engine.db")).unwrap();
    crate::storage::initialize_authority_schemas(&connection).unwrap();
    let coordination = crate::team::coordination(&connection, &project()).unwrap();
    let mut view = TeamView::new(project());
    let query = view.next_query(NOW).unwrap();
    let mut stored = team();
    stored.coordination = Some(coordination.clone());
    view.apply(&query, Ok(SetupView::Team(Box::new(stored.clone()))));
    assert_eq!(view.key(press(KeyCode::Char('c'))), TeamReply::Handled);
    let nudges = |view: &TeamView| find(&frame(view, 120, 40), "Workflow nudges").to_owned();
    assert!(nudges(&view).contains("Off"), "{}", nudges(&view));
    for _ in 0..8 {
        assert_eq!(view.key(press(KeyCode::Down)), TeamReply::Handled);
    }
    let mut at = NOW;
    for (next, words) in [
        (Nudges::TenMinutes, "Every 10 minutes"),
        (Nudges::FirstEdit, "First edit without a graph query"),
        (Nudges::Off, "Off"),
    ] {
        let mut settings = coordination.settings.clone();
        settings.nudges = next;
        let reply = view.key(press(KeyCode::Char(' ')));
        assert_eq!(
            reply,
            TeamReply::Change(TeamAction::CoordSettings {
                project_id: project(),
                settings: settings.clone(),
            })
        );
        stored.coordination.as_mut().unwrap().settings = settings;
        at += 60_000;
        assert_eq!(
            settled(&mut view, reply, &stored, at),
            Some(Notice::plain(format!(
                "Workflow nudges: {}.",
                words.to_lowercase()
            )))
        );
        assert!(nudges(&view).contains(words), "{}", nudges(&view));
    }
}

/// Sends one settings key's change as the engine answers it: applied, then a fresh team read
/// with the stored choice. Takes the view, the reply, the stored team and the read time.
/// Returns the notice the applied change left.
fn settled(view: &mut TeamView, reply: TeamReply, stored: &TeamStatus, at: i64) -> Option<Notice> {
    let TeamReply::Change(action) = reply else {
        panic!("a settings key sends a change: {reply:?}");
    };
    view.apply(
        &SetupQuery::TeamChange {
            action,
            generation: 9,
        },
        Ok(SetupView::Changed(OwnerResult::Applied)),
    );
    let notice = view.notice.clone();
    let query = view.next_query(at).unwrap();
    view.apply(&query, Ok(SetupView::Team(Box::new(stored.clone()))));
    notice
}

/// Sub-agent depth steps through every choice (3 after the default 2, then 1, then 2) and
/// Worker lands flips between both choices (N1, N2): each key sends its change, a second key
/// waits for the first, the engine's answer says what the choice means and the team rows show
/// the stored choice.
#[test]
fn sub_agent_depth_and_worker_landing_step_through_every_choice() {
    let mut loading = TeamView::new(project());
    for code in ['d', 'k'] {
        assert_eq!(loading.key(press(KeyCode::Char(code))), TeamReply::Handled);
        assert_eq!(
            loading.notice,
            Some(Notice::plain("The team is still loading."))
        );
    }
    let shows =
        |view: &TeamView, text: &str| frame(view, 120, 36).iter().any(|row| row.contains(text));
    let mut view = loaded();
    assert!(shows(&view, "Sub-agent depth  2"));
    assert!(shows(&view, "Worker lands  When the lead accepts"));
    let mut stored = team();
    let mut at = NOW;
    for (depth, note) in [
        (
            3,
            "Workers can now lead workers of their own, three levels deep.",
        ),
        (1, "Only the master starts sub-agents now."),
        (
            2,
            "The master's sub-agents can now lead workers of their own.",
        ),
    ] {
        let reply = view.key(press(KeyCode::Char('d')));
        assert_eq!(
            reply,
            TeamReply::Change(TeamAction::SubAgentDepth {
                project_id: project(),
                depth,
            })
        );
        assert_eq!(
            view.key(press(KeyCode::Char('d'))),
            TeamReply::Handled,
            "a second press waits for the first change"
        );
        stored.sub_agent_depth = depth;
        at += 60_000;
        assert_eq!(
            settled(&mut view, reply, &stored, at),
            Some(Notice::plain(note))
        );
        assert!(
            shows(&view, &format!("Sub-agent depth  {depth}")),
            "{depth}"
        );
    }
    for (mode, note, words) in [
        (
            crate::team::ChildLanding::Owner,
            "A lead's worker now lands only when you accept it.",
            "Only when I accept",
        ),
        (
            crate::team::ChildLanding::Lead,
            "A lead's worker now lands on the lead's branch when the lead accepts it.",
            "When the lead accepts",
        ),
    ] {
        let reply = view.key(press(KeyCode::Char('k')));
        assert_eq!(
            reply,
            TeamReply::Change(TeamAction::ChildLanding {
                project_id: project(),
                mode,
            })
        );
        stored.child_landing = mode;
        at += 60_000;
        assert_eq!(
            settled(&mut view, reply, &stored, at),
            Some(Notice::plain(note))
        );
        assert!(shows(&view, &format!("Worker lands  {words}")), "{words}");
    }
}

/// Max harnesses (v), Max teammates (u) and Token budget (T) each step through every choice
/// and back to No limit or Off, and Spread accounts (P) turns on and off (S4, DEC-104 K8):
/// each key sends its change, the engine's answer says the new choice and the team rows
/// show what the engine stored.
#[test]
fn caps_budget_and_spread_step_through_every_choice() {
    let shows =
        |view: &TeamView, text: &str| frame(view, 120, 36).iter().any(|row| row.contains(text));
    let mut view = loaded();
    let mut stored = team();
    let mut at = NOW;
    for (key, label, done) in [
        ('v', "Max harnesses", "Max harnesses is now"),
        ('u', "Max teammates", "Max teammates per harness is now"),
    ] {
        assert!(shows(&view, &format!("{label}  No limit")), "{label}");
        for max in crate::team::CAP_CHOICES
            .iter()
            .copied()
            .map(Some)
            .chain([None])
        {
            let reply = view.key(press(KeyCode::Char(key)));
            let wanted = if key == 'v' {
                TeamAction::MaxHarnesses {
                    project_id: project(),
                    max,
                }
            } else {
                TeamAction::MaxTeammates {
                    project_id: project(),
                    max,
                }
            };
            assert_eq!(reply, TeamReply::Change(wanted), "{label} {max:?}");
            if key == 'v' {
                stored.max_harnesses = max;
            } else {
                stored.max_teammates = max;
            }
            at += 60_000;
            assert_eq!(
                settled(&mut view, reply, &stored, at),
                Some(Notice::plain(format!("{done} {}.", cap_words(max))))
            );
            assert!(
                shows(&view, &format!("{label}  {}", cap_words(max))),
                "{label} {max:?}"
            );
        }
    }

    assert!(shows(&view, "Token budget  Off"));
    for tokens in crate::team::BUDGET_CHOICES
        .iter()
        .copied()
        .map(Some)
        .chain([None])
    {
        let reply = view.key(press(KeyCode::Char('T')));
        assert_eq!(
            reply,
            TeamReply::Change(TeamAction::TokenBudget {
                project_id: project(),
                tokens,
            }),
            "{tokens:?}"
        );
        stored.token_budget = tokens;
        stored.budget_used = tokens.map(|_| 0);
        at += 60_000;
        let done = match tokens {
            Some(tokens) => format!(
                "The team now stops after {} from now.",
                crate::team::token_words(tokens)
            ),
            None => "The token budget is off.".to_owned(),
        };
        assert_eq!(
            settled(&mut view, reply, &stored, at),
            Some(Notice::plain(done))
        );
        assert!(
            shows(
                &view,
                &format!("Token budget  {}", budget_words(tokens, Some(0)))
            ),
            "{tokens:?}"
        );
    }

    assert!(shows(&view, "Spread accounts  Off"));
    for (on, done, words) in [
        (
            true,
            "New agents now prefer a pool account no running agent of this project uses.",
            "On",
        ),
        (
            false,
            "New agents now take the account your account strategy picks.",
            "Off",
        ),
    ] {
        let reply = view.key(press(KeyCode::Char('P')));
        assert_eq!(
            reply,
            TeamReply::Change(TeamAction::SpreadAccounts {
                project_id: project(),
                on,
            })
        );
        stored.spread_accounts = on;
        at += 60_000;
        assert_eq!(
            settled(&mut view, reply, &stored, at),
            Some(Notice::plain(done))
        );
        assert!(shows(&view, &format!("Spread accounts  {words}")), "{on}");
    }
}

/// F moves New folder limit from the shown choice to the next, through every choice and
/// back to the first; each choice shows on the team screen and its saved change says what
/// is now refused (fix/changes-vs-owner-branch DEC-107 rule 3).
#[test]
fn f_moves_the_new_folder_limit_through_every_choice() {
    let mut view = TeamView::new(project());
    assert_eq!(view.key(press(KeyCode::Char('F'))), TeamReply::Handled);
    let choices = crate::team::NEW_FOLDER_FILES_CHOICES;
    for (at, shown) in choices.iter().enumerate() {
        let next = choices[(at + 1) % choices.len()];
        let mut status = team();
        status.new_folder_files_max = *shown;
        let mut view = TeamView::new(project());
        let query = view.next_query(NOW).unwrap();
        view.apply(&query, Ok(SetupView::Team(Box::new(status))));
        let rows = frame(&view, 120, 40);
        assert!(
            rows.iter()
                .any(|row| row.contains(&format!("New folder limit  {shown} files"))),
            "{}",
            rows.join("\n")
        );
        let reply = view.key(press(KeyCode::Char('F')));
        assert_eq!(
            reply,
            TeamReply::Change(TeamAction::NewFolderLimit {
                project_id: project(),
                files: next
            })
        );
        let TeamReply::Change(action) = reply else {
            unreachable!();
        };
        view.apply(
            &SetupQuery::TeamChange {
                action,
                generation: 9,
            },
            Ok(SetupView::Changed(OwnerResult::Applied)),
        );
        assert_eq!(
            view.notice,
            Some(Notice::plain(format!(
                "A change set adding more than {next} files in one new folder is now refused."
            )))
        );
    }
}

/// B steps Background commands at task end from Wait to a 2 min grace time, to Stop and
/// back to Wait, and G steps the grace time through every choice, starting a grace time
/// from another choice (DEC-111 Q13): each key sends its change, the engine's answer says
/// the new choice and the team rows show what the engine stored. A grace time the engine
/// refuses has its own plain text.
#[test]
fn shift_b_steps_background_commands_and_shift_g_steps_the_grace_time() {
    use crate::team::BackgroundAtEnd;
    let mut view = TeamView::new(project());
    for key in ['B', 'G'] {
        assert_eq!(view.key(press(KeyCode::Char(key))), TeamReply::Handled);
        assert_eq!(
            view.notice,
            Some(Notice::plain("The team is still loading."))
        );
    }
    let mut view = loaded();
    assert!(view.help().contains(&("B", "Background commands")));
    assert!(view.help().contains(&("G", "Grace time")));
    assert!(shows(&view, "Background commands  Wait"));
    let grace = |secs: u32, span: &str| {
        (
            BackgroundAtEnd::Grace { secs },
            format!("A task's background commands now get {span} after its turn ends, then stop."),
            format!("Background commands  Stop after {span}"),
        )
    };
    let stop = (
        BackgroundAtEnd::Stop,
        "A task's background commands now stop when its turn ends, and its agent is told so."
            .to_owned(),
        "Background commands  Stop at turn end".to_owned(),
    );
    let wait = (
        BackgroundAtEnd::Wait,
        "A task now waits for the commands its agent started in the background, up to its task limit."
            .to_owned(),
        "Background commands  Wait".to_owned(),
    );
    let mut stored = team();
    let mut at = NOW;
    for (key, (choice, done, shown)) in [
        ('B', grace(120, "2 min")),
        ('G', grace(300, "5 min")),
        ('B', stop.clone()),
        ('G', grace(120, "2 min")),
        ('B', stop),
        ('B', wait),
    ] {
        let reply = view.key(press(KeyCode::Char(key)));
        assert_eq!(
            reply,
            TeamReply::Change(TeamAction::BackgroundAtEnd {
                project_id: project(),
                choice,
            }),
            "{key} {choice:?}"
        );
        applied(&mut view, reply);
        assert_eq!(view.notice, Some(Notice::plain(done)));
        stored.background_at_end = choice;
        at += 60_000;
        reread(&mut view, stored.clone(), at);
        assert!(shows(&view, &shown), "{shown}");
    }

    let choices = [10, 30, 60, 120, 300, 600, 1800, 3600];
    for (index, secs) in choices.iter().copied().enumerate() {
        let mut status = team();
        status.background_at_end = BackgroundAtEnd::Grace { secs };
        let mut view = TeamView::new(project());
        let query = view.next_query(NOW).unwrap();
        view.apply(&query, Ok(SetupView::Team(Box::new(status))));
        let next = choices[(index + 1) % choices.len()];
        assert_eq!(
            view.key(press(KeyCode::Char('G'))),
            TeamReply::Change(TeamAction::BackgroundAtEnd {
                project_id: project(),
                choice: BackgroundAtEnd::Grace { secs: next },
            }),
            "{secs}"
        );
    }

    let mut view = loaded();
    let TeamReply::Change(action) = view.key(press(KeyCode::Char('G'))) else {
        panic!("G sends a grace time");
    };
    view.apply(
        &SetupQuery::TeamChange {
            action,
            generation: 9,
        },
        Ok(SetupView::Changed(OwnerResult::Rejected {
            code: "team_background_grace_invalid".into(),
        })),
    );
    assert_eq!(
        view.notice,
        Some(Notice::problem("Pick a grace time from 10 s to 1 h."))
    );
}
