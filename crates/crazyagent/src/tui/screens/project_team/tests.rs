use super::*;
use crate::client_controller::OwnerController;
use crate::coord::settings::{ClaudeDisplay, CoordSettings, RoleSkills, TaskStart};
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
        instructions_changed: false,
        message_note: None,
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
        apply_harness_switch: crate::team::SwitchApply::WhenIdle,
        sub_agent_depth: crate::team::DEPTH_DEFAULT,
        child_landing: crate::team::ChildLanding::Lead,
        max_harnesses: None,
        max_teammates: None,
        token_budget: None,
        budget_used: None,
        budget_reached: false,
        spread_accounts: false,
        terminals_per_window: crate::team::TerminalsPerWindow::Six,
        show_finished: false,
        message_settings: crate::team::agent_messages::MessageSettings::default(),
        agent_branches: crate::team::BranchMode::Named,
        master_branch: None,
        land_on: None,
        background_at_end: crate::team::BackgroundAtEnd::Wait,
        ask_before_merge: false,
        master_lands: None,
        access_unreadable: false,
        reask_empty_reply: true,
        master_idle_stop: crate::team::MasterIdleStop::default(),
        master_live_terminal: true,
        catch_up_main: crate::team::CatchUpMain::On,
        reuse_idle: crate::team::ReuseIdle::Off,
        new_folder_files_max: crate::team::NEW_FOLDER_FILES_DEFAULT,
        coordination: None,
        live_switch: None,
        paused: false,
        live_master: None,
        live_problem: None,
        live_idle_stopped: false,
        settings_version: 7,
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
    let items = crate::tui::screens::project_view::tab_items(0, columns, &look);
    let mut rows: Vec<Row> = tab_strip(columns, &items, Tab::Agents as usize, &look).into();
    let room = usize::from(height) - 4 - 2;
    let (lines, help, _) = view.draw(columns - 4, room, &accounts(), &look);
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

/// The words a frame shows in reading order, its rows joined with single spaces and its
/// borders left out, so a text wrapped over several rows reads as one line. Takes the
/// frame's rows. Returns the words.
fn words(rows: &[String]) -> String {
    rows.iter()
        .flat_map(|row| row.split_whitespace())
        .filter(|word| word.chars().any(char::is_alphanumeric))
        .collect::<Vec<_>>()
        .join(" ")
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
fn a_short_window_keeps_the_plan_line_while_a_harness_cannot_run() {
    use crate::team::events::WaitingAgent;
    let mut status = team();
    status.harnesses[1].runnable = false;
    status.events.waiting = vec![WaitingAgent {
        agent: 2,
        text: "Do you want to proceed?".into(),
    }];
    status.events.paused = true;
    let mut view = TeamView::new(project());
    let query = view.next_query(NOW).unwrap();
    view.apply(&query, Ok(SetupView::Team(Box::new(status))));
    let rows = frame(&view, 80, 24);
    assert_eq!(
        rows.iter()
            .filter(|row| row.contains("Claude Code (can't run yet) x2"))
            .count(),
        1,
        "{}",
        rows.join("\n")
    );
    assert!(rows.iter().all(|row| row.chars().count() <= 80));
}

#[test]
fn a_short_window_keeps_the_line_saying_there_is_no_master() {
    use crate::team::events::WaitingAgent;
    let mut status = team();
    status
        .harnesses
        .retain(|harness| harness.role != AgentRole::Master);
    status.events.waiting = vec![WaitingAgent {
        agent: 2,
        text: "Do you want to proceed?".into(),
    }];
    status.events.paused = true;
    let mut view = TeamView::new(project());
    let query = view.next_query(NOW).unwrap();
    view.apply(&query, Ok(SetupView::Team(Box::new(status))));
    let rows = frame(&view, 80, 24);
    assert_eq!(
        rows.iter()
            .filter(|row| row.contains("No master yet."))
            .count(),
        1,
        "{}",
        rows.join("\n")
    );
    assert!(rows.iter().all(|row| row.chars().count() <= 80));
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
            on: true,
            version: 7,
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
    warm.settings_version = 1;
    let query = view.next_query(NOW + 10).unwrap();
    view.apply(&query, Ok(SetupView::Team(Box::new(warm))));
    assert!(find(&frame(&view, 80, 24), "Team").contains("Keep warm  On"));
    assert_eq!(
        view.key(press(KeyCode::Char('w'))),
        TeamReply::Change(TeamAction::KeepWarm {
            project_id: project(),
            on: false,
            version: 1,
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
fn a_refused_agent_git_step_names_the_command_line_tools_fix() {
    assert_eq!(
        team_plain("agent_git_needs_tools"),
        "Agent git needs the Command Line Tools: run xcode-select --install."
    );
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
        account_name(Some("acct-002"), &accounts()),
        "Claude Max - work",
        "a listed account goes by the name Accounts shows (OB-4)"
    );
    assert_eq!(
        account_name(Some("acct-404"), &accounts()),
        "acct-404",
        "an account with no name known falls back to its id"
    );
    assert_eq!(account_name(None, &accounts()), "-");
}

#[test]
fn two_accounts_with_one_name_show_their_ids() {
    use crate::tui::seam::{account_label, account_labels};
    let row = |id: &str, name: &str| AccountRow {
        id: CredentialRef::new(id).unwrap(),
        name: name.into(),
        provider: String::new(),
        status: String::new(),
    };
    let rows = [
        row("claude-account-1a", "Claude"),
        row("claude-account-2b", "Claude"),
        row("chatgpt-account-3c", "Work ChatGPT"),
    ];
    assert_eq!(
        account_name(Some("claude-account-1a"), &rows),
        "Claude (claude-account-1a)"
    );
    assert_eq!(
        account_name(Some("claude-account-2b"), &rows),
        "Claude (claude-account-2b)"
    );
    assert_eq!(
        account_name(Some("chatgpt-account-3c"), &rows),
        "Work ChatGPT",
        "a name used once stays plain"
    );
    assert_eq!(
        account_name(Some("claude-account-2b"), &rows[1..]),
        "Claude"
    );
    assert_eq!(account_label("gone", &rows), None);
    let labels = account_labels(&rows);
    assert_eq!(labels.len(), 3);
    for account in &rows {
        assert_eq!(
            labels.get(account.id.as_str()),
            account_label(account.id.as_str(), &rows).as_ref(),
            "the one-pass words are the single account's words"
        );
    }
    assert!(account_labels(&[]).is_empty());
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

/// Sends the team setting change one key asks for to the real engine and hands the view the
/// result. Takes the view, the controller, the setting key and the generation. Returns the
/// engine's result.
async fn setting_change(
    view: &mut TeamView,
    controller: &mut OwnerController,
    key: char,
    generation: u64,
) -> OwnerResult {
    let TeamReply::Change(action) = view.key(press(KeyCode::Char(key))) else {
        panic!("{key} must ask the engine");
    };
    let response = controller
        .team_action(action.clone(), generation)
        .await
        .unwrap();
    let result = match response {
        OwnerResponse::Applied { operation } | OwnerResponse::Committed { operation } => {
            operation.result
        }
        other => panic!("team change not answered: {other:?}"),
    };
    view.apply(
        &SetupQuery::TeamChange { action, generation },
        Ok(SetupView::Changed(result.clone())),
    );
    result
}

/// The value of the team setting one key changes, as the team status holds it.
/// Takes the status and the key. Returns the value as text.
fn setting(status: &TeamStatus, key: char) -> String {
    match key {
        'w' => status.keep_warm.to_string(),
        't' => format!("{:?}", status.turn_limit_secs),
        'i' => format!("{:?}", status.idle_stop_secs),
        'h' => format!("{:?}", status.on_harness_switch),
        other => panic!("{other} is not a team setting key"),
    }
}

/// Keep warm, the task limit, the idle stop and the harness switch setting saved from a
/// window that read the team before another window changed it twice are refused as stale by
/// the real engine: the newer value survives, the window says so and reads the team again at
/// once, and then shows the newer value.
#[tokio::test]
async fn a_real_engine_refuses_a_team_setting_saved_from_an_older_read() {
    let root = tempfile::Builder::new()
        .prefix("ca-tui-stale-")
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

    let mut generation = 2;
    for key in ['w', 't', 'i', 'h'] {
        let mut mine = TeamView::new(project());
        let mut theirs = TeamView::new(project());
        read_team(&mut controller, &mut mine, NOW).await;
        read_team(&mut controller, &mut theirs, NOW).await;
        let read = mine.status.as_ref().unwrap().settings_version;
        assert_eq!(theirs.status.as_ref().unwrap().settings_version, read);

        assert_eq!(
            setting_change(&mut theirs, &mut controller, key, generation).await,
            OwnerResult::Applied,
            "{key}"
        );
        read_team(&mut controller, &mut theirs, NOW + 1).await;
        let first = setting(theirs.status.as_ref().unwrap(), key);
        assert_eq!(
            setting_change(&mut theirs, &mut controller, key, generation + 1).await,
            OwnerResult::Applied,
            "{key}"
        );
        let newer = controller.team_status(project()).await.unwrap();
        assert_eq!(newer.settings_version, read + 2, "{key}");
        assert_ne!(
            setting(&newer, key),
            first,
            "{key}: the older window's change differs from the newer value"
        );

        assert_eq!(
            setting_change(&mut mine, &mut controller, key, generation + 2).await,
            OwnerResult::Rejected {
                code: crate::storage::versioned::STALE_CODE.into()
            },
            "{key}"
        );
        assert_eq!(
            mine.notice,
            Some(Notice::problem(
                "This changed in another window, so yours was not saved. The saved one shows now."
            )),
            "{key}"
        );
        let kept = controller.team_status(project()).await.unwrap();
        assert_eq!(
            setting(&kept, key),
            setting(&newer, key),
            "{key}: the newer value survives"
        );
        assert_eq!(kept.settings_version, newer.settings_version, "{key}");

        read_team(&mut controller, &mut mine, NOW + 1).await;
        assert_eq!(
            setting(mine.status.as_ref().unwrap(), key),
            setting(&newer, key),
            "{key}: the refused window shows the newer value"
        );
        let TeamReply::Change(again) = mine.key(press(KeyCode::Char(key))) else {
            panic!("{key} must ask the engine");
        };
        let named = match &again {
            TeamAction::KeepWarm { version, .. }
            | TeamAction::TimeLimits { version, .. }
            | TeamAction::OnHarnessSwitch { version, .. } => *version,
            other => panic!("{key} sent {other:?}"),
        };
        assert_eq!(
            named, newer.settings_version,
            "{key}: the next change names the newer version"
        );
        generation += 3;
    }
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
    let (lines, _, _) = view.draw(76, room, &accounts(), &look);
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
fn an_agent_level_the_owner_cleared_sends_the_version_its_reset_left() {
    let switch = |agent: i64, cleared: Vec<crate::team::events::ClearedRule>| {
        let mut view = loaded();
        view.status.as_mut().unwrap().events.wake.cleared = cleared;
        view.wake = Some(WakeEdit {
            level: WakeLevel::Agent(agent),
            agent: Some(agent),
            row: 0,
        });
        let TeamReply::Change(TeamAction::Wake {
            target,
            rule,
            version,
        }) = view.key(press(KeyCode::Char(' ')))
        else {
            panic!("a switch on an agent level asks the engine");
        };
        assert_eq!(
            target,
            RuleTarget::Agent {
                project_id: project(),
                agent
            }
        );
        assert!(rule.is_some());
        version
    };
    let reset = vec![crate::team::events::ClearedRule {
        agent: 2,
        version: 4,
    }];
    assert_eq!(
        switch(2, reset.clone()),
        4,
        "the cleared level's version, not 0, so the engine takes the change"
    );
    assert_eq!(switch(4, reset), 0, "a level never set is at version 0");
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
            idle_secs: None,
            version: 7,
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
            idle_secs: None,
            version: 7,
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
            idle_secs: None,
            version: 7,
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
            idle_secs: None,
            version: 7,
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
            idle_secs: Some(3600),
            version: 7,
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
            idle_secs: None,
            version: 7,
        }),
        "past 1 h the idle stop goes Off"
    );
}

fn entry(
    id: &str,
    name: &str,
    status: crate::harnesses::HarnessStatus,
    team: crate::harnesses::TeamUse,
) -> crate::harnesses::HarnessEntry {
    crate::harnesses::HarnessEntry {
        id: id.into(),
        name: name.into(),
        status_label: String::new(),
        status,
        executable: None,
        headless: String::new(),
        tools: crate::harnesses::ToolControl::Switchable,
        model_selection: false,
        account_kinds: Vec::new(),
        team,
        aliases: Vec::new(),
    }
}

fn catalog() -> Vec<crate::harnesses::HarnessEntry> {
    use crate::harnesses::{HarnessStatus, TeamUse};
    vec![
        entry("codex", "Codex", HarnessStatus::Ready, TeamUse::Available),
        entry(
            "claude",
            "Claude Code",
            HarnessStatus::Ready,
            TeamUse::Available,
        ),
        entry(
            "opencode",
            "OpenCode",
            HarnessStatus::NotInstalled,
            TeamUse::Available,
        ),
        entry(
            "pi",
            "Pi",
            HarnessStatus::Ready,
            TeamUse::Unavailable {
                reason: "No reader for its replies yet.".into(),
            },
        ),
        entry(
            "gemini",
            "Gemini CLI",
            HarnessStatus::Ready,
            TeamUse::MasterOnly {
                reason: "It runs only as the project master.".into(),
            },
        ),
    ]
}

fn stored() -> crate::project_setup::StoredSetup {
    crate::project_setup::StoredSetup {
        project_id: project(),
        name: "Tidepool".into(),
        about: "Tide charts for the harbour.".into(),
        tech_stack: vec!["Next.js".into(), "Postgres".into()],
        platforms: Vec::new(),
        research: Vec::new(),
        pool: None,
        team: TeamPlan {
            master: "codex".into(),
            master_tools: true,
            sub_harnesses: vec![HarnessSlot {
                harness: "claude-code".into(),
                agents: 2,
                tools: true,
            }],
            min_agents: 1,
            max_agents: 4,
        },
        documents: Vec::new(),
        folder: "/Users/mira/CrazyAgent/Projects/tidepool".into(),
        created_ms: NOW - 60 * MINUTE,
        revision: 7,
    }
}

/// Team settings name the saved master harness on the plan line and, after the On harness
/// switch choice, when a saved change reaches the running master; at 80 columns too, where a
/// setting of its own would fold the first settings row away.
#[test]
fn team_settings_name_the_master_harness_and_when_a_change_reaches_the_running_master() {
    let view = loaded();
    for (width, height) in [(160, 36), (80, 24)] {
        let rows = frame(&view, width, height);
        assert!(find(&rows, "leads.").contains("Codex leads."));
        assert!(
            find(&rows, "On harness switch")
                .contains("On harness switch  Carry over, when the master is idle"),
            "{}",
            rows.join("\n")
        );
    }
    let mut later = team();
    later.apply_harness_switch = crate::team::SwitchApply::NextStart;
    let mut view = TeamView::new(project());
    let query = view.next_query(NOW).unwrap();
    view.apply(&query, Ok(SetupView::Team(Box::new(later))));
    assert!(
        find(&frame(&view, 160, 36), "On harness switch")
            .contains("On harness switch  Carry over, at the next start")
    );
    assert!(view.help().contains(&("H", "Master harness")));
    assert!(view.help().contains(&("A", "Apply master change")));
}

/// A flips Apply a master harness change between When the master is idle and At the next
/// start, through the engine's ApplyHarnessSwitch change.
#[test]
fn the_apply_setting_flips_between_when_idle_and_at_the_next_start() {
    let mut view = TeamView::new(project());
    assert_eq!(view.key(press(KeyCode::Char('A'))), TeamReply::Handled);
    assert_eq!(
        view.notice,
        Some(Notice::plain("The team is still loading."))
    );
    let mut view = loaded();
    let reply = view.key(press(KeyCode::Char('A')));
    assert_eq!(
        reply,
        TeamReply::Change(TeamAction::ApplyHarnessSwitch {
            project_id: project(),
            mode: crate::team::SwitchApply::NextStart,
            version: 7,
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
            "A saved master harness change now waits for the master's next start."
        ))
    );
    let mut later = team();
    later.apply_harness_switch = crate::team::SwitchApply::NextStart;
    let query = view.next_query(NOW + 10).unwrap();
    view.apply(&query, Ok(SetupView::Team(Box::new(later))));
    assert_eq!(
        view.key(press(KeyCode::Char('A'))),
        TeamReply::Change(TeamAction::ApplyHarnessSwitch {
            project_id: project(),
            mode: crate::team::SwitchApply::WhenIdle,
            version: 7,
        })
    );
}

/// A sends Apply a master harness change with the team settings version the view read, and
/// waits while the read after its last save is on its way, so the engine refuses a change
/// from an older read as stale instead of letting it undo a newer one (I1).
#[test]
fn the_apply_setting_names_the_settings_version_it_read_and_waits_for_the_read_after_a_save() {
    let mut view = TeamView::new(project());
    let mut read = team();
    read.settings_version = 7;
    reread(&mut view, read, NOW);
    let reply = view.key(press(KeyCode::Char('A')));
    let TeamReply::Change(action) = &reply else {
        panic!("no change was sent: {reply:?}");
    };
    assert_eq!(
        serde_json::to_value(action).unwrap()["version"],
        serde_json::json!(7),
        "the change names the version the view read: {action:?}"
    );
    applied(&mut view, reply);
    assert_eq!(
        view.key(press(KeyCode::Char('A'))),
        TeamReply::Handled,
        "the team read after the save has not landed"
    );
    assert_eq!(
        view.notice,
        Some(Notice::problem(
            "Still saving the last change. Try again in a moment."
        ))
    );
}

/// H opens the master harness picker: it reads the harness catalog once and the stored
/// setup, lists the harnesses that can run the master with the running one marked now,
/// and Enter saves the picked one as the team's master through the project setup at the
/// read revision; the current one saves nothing, a refused save keeps the picker open and
/// reads the setup again, and Esc closes it.
#[test]
fn the_master_harness_picker_saves_the_picked_harness_through_the_project_setup() {
    let mut view = TeamView::new(project());
    assert_eq!(view.key(press(KeyCode::Char('H'))), TeamReply::Handled);
    assert_eq!(
        view.notice,
        Some(Notice::plain("The team is still loading."))
    );
    assert!(!view.wants_catalog());

    let mut view = loaded();
    assert_eq!(view.key(press(KeyCode::Char('H'))), TeamReply::Handled);
    assert!(view.wants_catalog());
    assert!(!view.wants_catalog(), "the catalog is read once");
    assert_eq!(
        view.next_query(NOW + 1),
        Some(SetupQuery::Setup { project: project() })
    );
    assert_eq!(view.next_query(NOW + 2), None, "one setup read at a time");
    let shown = words(&frame(&view, 120, 36));
    assert!(shown.contains("Harness for the master"), "{shown}");
    assert!(shown.contains("Reading the harnesses"), "{shown}");
    assert_eq!(view.key(press(KeyCode::Enter)), TeamReply::Handled);
    assert!(
        view.master_pick.is_some(),
        "nothing to save before the reads"
    );

    view.take_catalog(&Ok(catalog()));
    view.apply(
        &SetupQuery::Setup { project: project() },
        Ok(SetupView::Setup(Some(Box::new(stored())))),
    );
    let rows = frame(&view, 120, 36);
    let at = rows
        .iter()
        .position(|row| row.contains("Harness for the master"))
        .unwrap();
    let picker: Vec<String> = rows[at + 1..]
        .iter()
        .map(|row| {
            row.trim_matches(|c: char| c == ' ' || c == '\u{2502}')
                .to_owned()
        })
        .filter(|row| row.ends_with("now") || ["Claude Code", "Gemini CLI"].contains(&row.as_str()))
        .collect();
    assert_eq!(
        picker,
        ["> Codex  now", "Claude Code", "Gemini CLI"],
        "{}",
        rows.join("\n")
    );
    assert!(
        words(&rows).contains("The running master switches once it is idle."),
        "{}",
        rows.join("\n")
    );

    assert_eq!(view.key(press(KeyCode::Enter)), TeamReply::Handled);
    assert!(view.master_pick.is_none());
    assert_eq!(
        view.notice,
        Some(Notice::plain("The master already uses Codex."))
    );

    assert_eq!(view.key(press(KeyCode::Char('H'))), TeamReply::Handled);
    assert!(!view.wants_catalog(), "the catalog stays read");
    assert_eq!(view.next_query(NOW + 3), None, "the setup stays read");
    assert_eq!(view.key(press(KeyCode::Down)), TeamReply::Handled);
    let reply = view.key(press(KeyCode::Enter));
    let mut team_plan = stored().team;
    team_plan.master = "claude".into();
    assert_eq!(
        reply,
        TeamReply::Save(crate::project_setup::SetupAction::Update {
            project_id: project(),
            kept: None,
            expected_revision: 7,
            name: None,
            setup: ProjectSetup {
                about: "Tide charts for the harbour.".into(),
                tech_stack: vec!["Next.js".into(), "Postgres".into()],
                platforms: Vec::new(),
                research: Vec::new(),
                pool: None,
                team: Some(team_plan),
                documents: Vec::new(),
            },
        })
    );
    let TeamReply::Save(action) = reply else {
        unreachable!();
    };
    assert!(view.busy());
    let mut other = action.clone();
    if let crate::project_setup::SetupAction::Update { name, .. } = &mut other {
        *name = Some("Tidepool two".into());
    }
    view.apply(
        &SetupQuery::Change {
            action: other,
            generation: 3,
        },
        Ok(SetupView::Changed(OwnerResult::Applied)),
    );
    assert!(
        view.busy() && view.master_pick.is_some(),
        "another screen's setup save is not the picker's"
    );
    view.apply(
        &SetupQuery::Change {
            action,
            generation: 4,
        },
        Ok(SetupView::Changed(OwnerResult::Applied)),
    );
    assert!(view.master_pick.is_none());
    assert_eq!(
        view.notice,
        Some(Notice::plain(
            "The master switches to Claude Code once it is idle."
        ))
    );
    assert_eq!(
        view.next_query(NOW + 4),
        Some(SetupQuery::Team { project: project() }),
        "the summary reads the saved master again"
    );

    view.apply(
        &SetupQuery::Team { project: project() },
        Ok(SetupView::Team(Box::new(team()))),
    );
    assert_eq!(view.key(press(KeyCode::Char('H'))), TeamReply::Handled);
    assert_eq!(
        view.next_query(NOW + 5),
        Some(SetupQuery::Setup { project: project() }),
        "a save raises the revision, so the setup is read again"
    );
    view.apply(
        &SetupQuery::Setup { project: project() },
        Ok(SetupView::Setup(Some(Box::new(stored())))),
    );
    assert_eq!(view.key(press(KeyCode::Down)), TeamReply::Handled);
    assert_eq!(view.key(press(KeyCode::Down)), TeamReply::Handled);
    let TeamReply::Save(action) = view.key(press(KeyCode::Enter)) else {
        panic!("Enter on Gemini CLI saves it");
    };
    view.apply(
        &SetupQuery::Change {
            action,
            generation: 5,
        },
        Ok(SetupView::Changed(OwnerResult::Rejected {
            code: "setup_revision_conflict".into(),
        })),
    );
    assert!(
        view.master_pick.is_some(),
        "a refused save keeps the picker"
    );
    assert!(view.notice.as_ref().is_some_and(|notice| notice.error));
    assert_eq!(
        view.next_query(NOW + 6),
        Some(SetupQuery::Setup { project: project() })
    );
    assert_eq!(view.key(press(KeyCode::Esc)), TeamReply::Handled);
    assert!(view.master_pick.is_none());
}

/// A failed setup read under the master harness picker is said once and read again only
/// after the refresh interval, not on every arrival that asks for the next read
/// (rv-code TP1-L1).
#[test]
fn a_failed_setup_read_under_the_master_picker_waits_for_the_refresh_interval() {
    let mut view = loaded();
    assert_eq!(view.key(press(KeyCode::Char('H'))), TeamReply::Handled);
    assert_eq!(
        view.next_query(NOW + 1),
        Some(SetupQuery::Setup { project: project() })
    );
    view.apply(
        &SetupQuery::Setup { project: project() },
        Err("team_engine_not_ready".into()),
    );
    assert_eq!(
        view.notice,
        Some(Notice::problem(
            "The engine is still starting. Try again in a moment."
        ))
    );
    assert!(view.master_pick.is_some(), "the picker shows the problem");
    assert_ne!(
        view.next_query(NOW + 2),
        Some(SetupQuery::Setup { project: project() }),
        "a failed setup read is not sent again at once"
    );
    assert_ne!(
        view.next_query(NOW + REFRESH_MS),
        Some(SetupQuery::Setup { project: project() }),
        "nor before the refresh interval passed"
    );
    assert_eq!(
        view.next_query(NOW + 1 + REFRESH_MS),
        Some(SetupQuery::Setup { project: project() }),
        "the setup is read again after the refresh interval"
    );
}

/// A project without a saved setup closes the master harness picker with the reason, so
/// the picker does not read the missing setup again and again (rv-code TP1-L1).
#[test]
fn a_missing_setup_closes_the_master_picker_with_the_reason() {
    let mut view = loaded();
    assert_eq!(view.key(press(KeyCode::Char('H'))), TeamReply::Handled);
    assert_eq!(
        view.next_query(NOW + 1),
        Some(SetupQuery::Setup { project: project() })
    );
    view.apply(
        &SetupQuery::Setup { project: project() },
        Ok(SetupView::Setup(None)),
    );
    assert!(view.master_pick.is_none(), "the picker closes");
    assert_eq!(
        view.notice,
        Some(Notice::problem(
            "This project has no saved setup, so its master harness cannot change."
        ))
    );
    assert_ne!(
        view.next_query(NOW + 2),
        Some(SetupQuery::Setup { project: project() }),
        "the missing setup is not read again"
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
    assert!(
        find(&frame(&view, 120, 36), "On harness switch")
            .contains("On harness switch  Carry over, when the master is idle")
    );
    let reply = view.key(press(KeyCode::Char('h')));
    assert_eq!(
        reply,
        TeamReply::Change(TeamAction::OnHarnessSwitch {
            project_id: project(),
            mode: crate::team::SwitchMode::Fresh,
            version: 7,
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
    assert!(
        find(&frame(&view, 120, 36), "On harness switch")
            .contains("On harness switch  Start fresh, when the master is idle")
    );
    assert_eq!(
        view.key(press(KeyCode::Char('h'))),
        TeamReply::Change(TeamAction::OnHarnessSwitch {
            project_id: project(),
            mode: crate::team::SwitchMode::Carry,
            version: 7,
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
            "The master now waits for your Accept before merging a harness, and before landing its own conflict fixes."
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

/// I moves "Stop the master when idle after" through 15 min, 1 h and Off and back, each
/// with its plain notice (DEC-117 Q4).
#[test]
fn capital_i_moves_the_master_idle_stop_through_its_choices() {
    use crate::team::MasterIdleStop::{FifteenMinutes, Off, OneHour};
    let mut view = TeamView::new(project());
    assert_eq!(view.key(press(KeyCode::Char('I'))), TeamReply::Handled);
    assert_eq!(
        view.notice,
        Some(Notice::plain("The team is still loading."))
    );
    let mut view = loaded();
    assert!(
        view.help()
            .contains(&("I", "Stop the master when idle after"))
    );
    assert!(shows(&view, "Master idle stop  15 min"));
    let steps = [
        (
            FifteenMinutes,
            OneHour,
            "An idle master now stops after 1 h.",
        ),
        (
            OneHour,
            Off,
            "An idle master now keeps running until the team stops.",
        ),
        (
            Off,
            FifteenMinutes,
            "An idle master now stops after 15 min.",
        ),
    ];
    for (at, next, words) in steps {
        let line = format!("Master idle stop  {}", at.label());
        assert!(shows(&view, &line), "{line}");
        let reply = view.key(press(KeyCode::Char('I')));
        assert_eq!(
            reply,
            TeamReply::Change(TeamAction::MasterIdleStop {
                project_id: project(),
                choice: next,
            })
        );
        applied(&mut view, reply);
        assert_eq!(view.notice, Some(Notice::plain(words)));
        let mut shown = team();
        shown.master_idle_stop = next;
        reread(&mut view, shown, NOW + 10);
    }
}

/// The idle limit's notice says what the master's own choice does (DEC-117 Q4).
#[test]
fn the_idle_limit_notice_says_what_the_masters_own_choice_does() {
    use crate::team::MasterIdleStop::{Off, OneHour};
    let steps = [
        (Off, "An idle master keeps running until the team stops."),
        (OneHour, "An idle master stops 1 h after its work is done."),
    ];
    for (choice, words) in steps {
        let mut chosen = team();
        chosen.master_idle_stop = choice;
        let mut view = TeamView::new(project());
        let query = view.next_query(NOW).unwrap();
        view.apply(&query, Ok(SetupView::Team(Box::new(chosen))));
        let reply = view.key(press(KeyCode::Char('i')));
        applied(&mut view, reply);
        let notice = format!("{:?}", view.notice);
        assert!(notice.contains(words), "{notice}");
    }
}

/// V turns "Live terminal" off and on, each way with its plain notice (DEC-117 Q4).
#[test]
fn capital_v_turns_live_terminal_off_and_on() {
    let mut view = TeamView::new(project());
    assert_eq!(view.key(press(KeyCode::Char('V'))), TeamReply::Handled);
    assert_eq!(
        view.notice,
        Some(Notice::plain("The team is still loading."))
    );
    let mut view = loaded();
    assert!(view.help().contains(&("V", "Live terminal")));
    let steps = [
        (
            "On",
            false,
            "Live terminal is off: the master ends at its next stop and its conversation is the console.",
        ),
        (
            "Off",
            true,
            "Live terminal is on: the master runs in a terminal you see and type into.",
        ),
    ];
    for (at, next, words) in steps {
        let line = format!("Live terminal  {at}");
        assert!(shows(&view, &line), "{line}");
        let reply = view.key(press(KeyCode::Char('V')));
        assert_eq!(
            reply,
            TeamReply::Change(TeamAction::MasterLiveTerminal {
                project_id: project(),
                on: next,
            })
        );
        applied(&mut view, reply);
        assert_eq!(view.notice, Some(Notice::plain(words)));
        let mut shown = team();
        shown.master_live_terminal = next;
        reread(&mut view, shown, NOW + 10);
    }
}

/// A master with no live terminal (RCLI) has nothing for "Live terminal" to turn on or off: V
/// says so and changes nothing, and neither the settings strip nor the help offers it
/// (OWNER-Q4-2-L3).
#[test]
fn capital_v_says_why_for_a_master_with_no_live_terminal() {
    let mut shown = team();
    let master = shown
        .harnesses
        .iter_mut()
        .find(|harness| harness.role == crate::team::AgentRole::Master)
        .expect("the fixture lists a master harness");
    master.harness = "rcli".into();
    let mut view = TeamView::new(project());
    let query = view.next_query(NOW).unwrap();
    view.apply(&query, Ok(SetupView::Team(Box::new(shown))));
    assert_eq!(view.key(press(KeyCode::Char('V'))), TeamReply::Handled);
    assert_eq!(
        view.notice,
        Some(Notice::plain(
            "This master has no live terminal: its conversation is the console."
        ))
    );
    assert!(!shows(&view, "Live terminal"), "the strip leaves it out");
    assert!(!view.help().contains(&("V", "Live terminal")));
}

/// A project with no master yet still stores the Live terminal choice: V sends it, and the
/// settings strip and the help offer it. "No live terminal" is only for a master that lacks
/// one (rv-secreview q4-3 F2).
#[test]
fn capital_v_stores_the_choice_with_no_master_yet() {
    let mut shown = team();
    shown
        .harnesses
        .retain(|harness| harness.role != crate::team::AgentRole::Master);
    let on = !shown.master_live_terminal;
    let mut view = TeamView::new(project());
    let query = view.next_query(NOW).unwrap();
    view.apply(&query, Ok(SetupView::Team(Box::new(shown))));
    assert_eq!(
        view.key(press(KeyCode::Char('V'))),
        TeamReply::Change(TeamAction::MasterLiveTerminal {
            project_id: project(),
            on,
        })
    );
    assert!(shows(&view, "Live terminal"), "the strip offers it");
    assert!(view.help().contains(&("V", "Live terminal")));
}

/// R flips Re-ask empty reply: On is shown for a team that has made no choice, R sends Off
/// and then On, each with its plain notice (DEC-111).
#[test]
fn capital_r_turns_reask_empty_reply_off_and_on() {
    let mut view = TeamView::new(project());
    assert_eq!(view.key(press(KeyCode::Char('R'))), TeamReply::Handled);
    assert_eq!(
        view.notice,
        Some(Notice::plain("The team is still loading."))
    );
    let mut view = loaded();
    assert!(view.help().contains(&("R", "Re-ask empty reply")));
    assert!(shows(&view, "Re-ask empty reply  On"));
    let reply = view.key(press(KeyCode::Char('R')));
    assert_eq!(
        reply,
        TeamReply::Change(TeamAction::ReaskEmptyReply {
            project_id: project(),
            on: false,
        })
    );
    applied(&mut view, reply);
    assert_eq!(
        view.notice,
        Some(Notice::plain(
            "A sub-agent's task with no reply text is now reported without asking again."
        ))
    );
    let mut off = team();
    off.reask_empty_reply = false;
    reread(&mut view, off, NOW + 10);
    assert!(shows(&view, "Re-ask empty reply  Off"));
    let reply = view.key(press(KeyCode::Char('R')));
    assert_eq!(
        reply,
        TeamReply::Change(TeamAction::ReaskEmptyReply {
            project_id: project(),
            on: true,
        })
    );
    applied(&mut view, reply);
    assert_eq!(
        view.notice,
        Some(Notice::plain(
            "A sub-agent that ends a task with no reply text is now asked once more for its reply."
        ))
    );
}

/// OS-8: Catch up with main shows On in the coordination editor of a project that made no
/// choice, on the row under Overlapping edits, and Space flips it Off and back: each press
/// sends the setting's own change, the engine's answer says what agents may now do and the
/// row shows the stored choice.
#[test]
fn catch_up_with_main_shows_on_under_overlapping_edits_and_space_flips_it() {
    let folder = tempfile::tempdir().unwrap();
    let connection = rusqlite::Connection::open(folder.path().join("engine.db")).unwrap();
    crate::storage::initialize_authority_schemas(&connection).unwrap();
    let mut view = TeamView::new(project());
    let query = view.next_query(NOW).unwrap();
    let mut stored = team();
    stored.coordination = Some(crate::team::coordination(&connection, &project()).unwrap());
    stored.catch_up_main = crate::team::catch_up_main(&connection, &project()).unwrap();
    view.apply(&query, Ok(SetupView::Team(Box::new(stored.clone()))));
    assert_eq!(view.key(press(KeyCode::Char('c'))), TeamReply::Handled);
    let shown = frame(&view, 120, 40);
    let at_overlaps = shown
        .iter()
        .position(|row| row.contains("Overlapping edits"))
        .unwrap();
    assert!(
        shown[at_overlaps + 1].contains("Catch up with main")
            && shown[at_overlaps + 1].contains("On"),
        "the row under Overlapping edits shows Catch up with main On:\n{}",
        shown.join("\n")
    );
    let row = |view: &TeamView| find(&frame(view, 120, 40), "Catch up with main").to_owned();
    for _ in 0..9 {
        assert_eq!(view.key(press(KeyCode::Down)), TeamReply::Handled);
    }
    assert_eq!(view.coord.as_ref().unwrap().row, 9);
    let mut at = NOW;
    for (mode, words, done) in [
        (
            CatchUpMain::Off,
            "Off",
            "Catch up with main is off for agents started from now on: the git the app writes for an agent refuses git merge crazyagent/main, except a master at Full access. It is a guide, not a lock: landed work can still come in another way.",
        ),
        (
            CatchUpMain::On,
            "On",
            "Catch up with main is on for agents started from now on: they may git merge crazyagent/main.",
        ),
    ] {
        let reply = view.key(press(KeyCode::Char(' ')));
        assert_eq!(
            reply,
            TeamReply::Change(TeamAction::CatchUpMain {
                project_id: project(),
                mode,
            })
        );
        stored.catch_up_main = mode;
        at += 60_000;
        assert_eq!(
            settled(&mut view, reply, &stored, at),
            Some(Notice::plain(done))
        );
        assert!(row(&view).contains(words), "{}", row(&view));
    }
}

/// The Coordination screen's last row shows the stored Reuse idle agents choice, Off by
/// default, and Space flips it: each press sends the setting's own change, the engine's
/// answer says what a new task now does and the row shows the choice (HB-S5-A).
#[test]
fn reuse_idle_agents_shows_off_by_default_and_space_flips_it() {
    let folder = tempfile::tempdir().unwrap();
    let connection = rusqlite::Connection::open(folder.path().join("engine.db")).unwrap();
    crate::storage::initialize_authority_schemas(&connection).unwrap();
    let mut view = TeamView::new(project());
    let query = view.next_query(NOW).unwrap();
    let mut stored = team();
    stored.coordination = Some(crate::team::coordination(&connection, &project()).unwrap());
    stored.reuse_idle = crate::team::reuse_idle(&connection, &project()).unwrap();
    view.apply(&query, Ok(SetupView::Team(Box::new(stored.clone()))));
    assert_eq!(view.key(press(KeyCode::Char('c'))), TeamReply::Handled);
    for _ in 0..16 {
        assert_eq!(view.key(press(KeyCode::Down)), TeamReply::Handled);
    }
    assert_eq!(view.coord.as_ref().unwrap().row, 16);
    let row = |view: &TeamView| find(&frame(view, 120, 40), "Reuse idle agents").to_owned();
    assert!(row(&view).contains("Off"), "{}", row(&view));
    let mut at = NOW;
    for (mode, words, done) in [
        (
            crate::team::ReuseIdle::On,
            "On",
            "Reuse idle agents is on: an idle agent may take the next task, on the history it already has.",
        ),
        (
            crate::team::ReuseIdle::Off,
            "Off",
            "Reuse idle agents is off: each new task starts a new agent on the current branch.",
        ),
    ] {
        let reply = view.key(press(KeyCode::Char(' ')));
        assert_eq!(
            reply,
            TeamReply::Change(TeamAction::ReuseIdle {
                project_id: project(),
                mode,
            })
        );
        stored.reuse_idle = mode;
        at += 60_000;
        assert_eq!(
            settled(&mut view, reply, &stored, at),
            Some(Notice::plain(done))
        );
        assert!(row(&view).contains(words), "{}", row(&view));
    }
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

    for _ in 0..3 {
        view.key(press(KeyCode::Down));
    }
    assert_eq!(view.coord.as_ref().unwrap().row, 3);
    let reply = view.key(press(KeyCode::Right));
    coord_send(&mut view, &mut controller, reply, 3).await;
    assert_eq!(
        harness_row(&view, HarnessKind::Codex).0,
        SubagentMode::Native
    );
    assert!(
        frame(&view, 116, 30)
            .iter()
            .any(|row| row.contains("Codex sub-agents")
                && row.contains("shown in the lead's terminal only")),
        "the Codex row says where its native sub-agents show"
    );

    view.key(press(KeyCode::Down));
    let reply = view.key(press(KeyCode::Left));
    coord_send(&mut view, &mut controller, reply, 4).await;
    assert_eq!(
        harness_row(&view, HarnessKind::Codex),
        (SubagentMode::Native, Delivery::TypingOnly)
    );

    view.key(press(KeyCode::Down));
    assert_eq!(view.coord.as_ref().unwrap().row, 5);
    assert_eq!(view.key(press(KeyCode::Right)), TeamReply::Handled);
    assert_eq!(
        view.notice,
        Some(Notice::plain(
            crate::coord::settings::RCLI_NATIVE_UNAVAILABLE
        ))
    );
    assert!(!view.busy(), "a refused mode sends nothing");

    for _ in 0..3 {
        view.key(press(KeyCode::Down));
    }
    assert_eq!(view.coord.as_ref().unwrap().row, 8);
    for (generation, overlaps) in [
        (5, Overlaps::Block),
        (6, Overlaps::Off),
        (7, Overlaps::Wait),
        (8, Overlaps::Warn),
    ] {
        let reply = view.key(press(KeyCode::Right));
        coord_send(&mut view, &mut controller, reply, generation).await;
        assert_eq!(coordination_of(&view).settings.overlaps, overlaps);
    }

    for _ in 0..4 {
        view.key(press(KeyCode::Down));
    }
    assert_eq!(view.coord.as_ref().unwrap().row, 12);
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
    coord_send(&mut view, &mut controller, reply, 9).await;
    assert!(!view.typing(), "an applied list closes the input");
    assert_eq!(coordination_of(&view).settings.counted_tools, ["a", "b*"]);

    let reply = view.key(press(KeyCode::Char('r')));
    coord_send(&mut view, &mut controller, reply, 10).await;
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
    for _ in 0..15 {
        assert_eq!(view.key(press(KeyCode::Down)), TeamReply::Handled);
    }
    assert_eq!(view.coord.as_ref().unwrap().row, 15);
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
    for _ in 0..10 {
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

/// T3-15: the Task-start reminder shows On in the coordination editor of a project with no
/// saved settings, read from a real engine database, on the row under Workflow nudges, and
/// Space flips it Off and back: each press sends the whole settings with the other option,
/// the engine's answer names it and the row shows the stored one.
#[test]
fn the_task_start_reminder_shows_on_by_default_and_space_flips_it() {
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
    let shown = frame(&view, 120, 40);
    let at_nudges = shown
        .iter()
        .position(|row| row.contains("Workflow nudges"))
        .unwrap();
    assert!(
        shown[at_nudges + 1].contains("Task-start reminder") && shown[at_nudges + 1].contains("On"),
        "the row under Workflow nudges shows the reminder On:\n{}",
        shown.join("\n")
    );
    let reminder = |view: &TeamView| find(&frame(view, 120, 40), "Task-start reminder").to_owned();
    for _ in 0..11 {
        assert_eq!(view.key(press(KeyCode::Down)), TeamReply::Handled);
    }
    assert_eq!(view.coord.as_ref().unwrap().row, 11);
    let mut at = NOW;
    for (next, words) in [(TaskStart::Off, "Off"), (TaskStart::On, "On")] {
        let mut settings = coordination.settings.clone();
        settings.task_start = next;
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
                "Task-start reminder: {}.",
                words.to_lowercase()
            )))
        );
        assert!(reminder(&view).contains(words), "{}", reminder(&view));
    }
}

/// The Claude Code terminal display shows Classic in the coordination editor of a project
/// with no saved settings, read from a real engine database, on row 2 under the Claude Code
/// rows (N-D9), and Space on the row flips it to Fullscreen and back: each press sends the
/// whole settings with the other display, the engine's answer names it and the row shows
/// the stored one.
#[test]
fn the_claude_code_display_shows_classic_by_default_and_space_flips_it() {
    let folder = tempfile::tempdir().unwrap();
    let connection = rusqlite::Connection::open(folder.path().join("engine.db")).unwrap();
    crate::storage::initialize_authority_schemas(&connection).unwrap();
    let coordination = crate::team::coordination(&connection, &project()).unwrap();
    assert_eq!(coordination.settings.claude_display, ClaudeDisplay::Classic);
    let mut view = TeamView::new(project());
    let query = view.next_query(NOW).unwrap();
    let mut stored = team();
    stored.coordination = Some(coordination.clone());
    view.apply(&query, Ok(SetupView::Team(Box::new(stored.clone()))));
    assert_eq!(view.key(press(KeyCode::Char('c'))), TeamReply::Handled);
    let display =
        |view: &TeamView| find(&frame(view, 120, 40), "Claude Code terminal display").to_owned();
    assert!(display(&view).contains("Classic"), "{}", display(&view));
    for _ in 0..2 {
        assert_eq!(view.key(press(KeyCode::Down)), TeamReply::Handled);
    }
    assert_eq!(view.coord.as_ref().unwrap().row, 2);
    let mut at = NOW;
    for (next, words) in [
        (ClaudeDisplay::Fullscreen, "Fullscreen"),
        (ClaudeDisplay::Classic, "Classic"),
    ] {
        let mut settings = coordination.settings.clone();
        settings.claude_display = next;
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
                "Claude Code terminals now open {words}."
            )))
        );
        assert!(display(&view).contains(words), "{}", display(&view));
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
/// the new choice and which agents it acts on, read whole in an 80 column terminal, and
/// the team rows show what the engine stored. A grace time the engine refuses has its own
/// plain text.
#[test]
fn shift_b_steps_background_commands_and_shift_g_steps_the_grace_time() {
    use crate::team::BackgroundAtEnd;
    const SCOPE: &str = "Wait and a grace time act on Claude Code panes. Other agents run per turn have their background commands stopped when the turn ends, and other kept agents, such as a kept Codex pane, keep theirs whatever the choice.";
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
            format!(
                "A task's background commands now get {span} after its turn ends, then stop. {SCOPE}"
            ),
            format!("Background commands  Stop after {span}"),
        )
    };
    let stop = (
        BackgroundAtEnd::Stop,
        format!(
            "A task's background commands now stop when its turn ends, and its agent is told so. {SCOPE}"
        ),
        "Background commands  Stop at turn end".to_owned(),
    );
    let wait = (
        BackgroundAtEnd::Wait,
        format!(
            "A task now waits for the commands its agent started in the background, up to its task limit. {SCOPE}"
        ),
        "Background commands  Wait".to_owned(),
    );
    let mut stored = team();
    let mut at = NOW;
    for (key, (choice, done, shown)) in [
        ('B', grace(1800, "30 min")),
        ('G', grace(3600, "1 h")),
        ('B', stop.clone()),
        ('G', grace(1800, "30 min")),
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
        let narrow = frame(&view, 80, 30);
        assert!(words(&narrow).contains(SCOPE), "{}", narrow.join("\n"));
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

fn messages_reply(agent: i64) -> SetupView {
    use crate::team::agent_messages::{
        AgentMessage, AgentMessages, EntryKind, MessageSettings, Source, Speaker,
    };
    let entry = |id: i64, ago: i64, from: Speaker, kind: EntryKind, source: Source, text: &str| {
        AgentMessage {
            at_ms: NOW - ago * MINUTE,
            source,
            id,
            from,
            kind,
            text: Some(text.into()),
            task: Some(11),
            state: None,
            result: None,
            result_more: false,
        }
    };
    SetupView::TeamMessages(Box::new(AgentMessages {
        project_id: project(),
        agent,
        entries: vec![
            entry(
                4,
                2,
                Speaker::Agent,
                EntryKind::Question,
                Source::Event,
                "Which chart library?",
            ),
            entry(
                11,
                6,
                Speaker::Master,
                EntryKind::Task,
                Source::Task,
                "Build the charts component",
            ),
        ],
        older: false,
        settings: MessageSettings::default(),
    }))
}

#[test]
fn capital_m_opens_the_focused_sub_agents_messages_and_esc_returns_at_120_and_80_columns() {
    let mut view = loaded();
    assert!(view.help().contains(&("M", "Messages")));
    assert!(view.help().contains(&("o", "Messages settings")));
    view.key(press(KeyCode::Down));
    let label = view.agents()[1].label.clone();
    assert_eq!(view.key(press(KeyCode::Char('M'))), TeamReply::Handled);
    let query = view.next_query(NOW + 1).unwrap();
    assert_eq!(
        query,
        SetupQuery::TeamMessages {
            project: project(),
            agent: 2,
            before: None,
        }
    );
    assert_eq!(view.next_query(NOW + 2), None, "one page read at a time");
    view.apply(&query, Ok(messages_reply(2)));
    for (width, height) in [(120, 36), (80, 24)] {
        let rows = frame(&view, width, height);
        find(&rows, &format!("Messages: {label}"));
        assert!(
            find(&rows, "Master > task: Build the charts component")
                .contains(&format!("  {}  Master", clock(NOW - 6 * MINUTE, 0)))
        );
        assert!(
            find(&rows, "Agent < question: Which chart library?")
                .contains(&format!("> {}  Agent", clock(NOW - 2 * MINUTE, 0)))
        );
        assert!(
            !rows.iter().any(|row| row.contains("Tasks  3")),
            "the list takes the place of the team lists\n{}",
            rows.join("\n")
        );
    }
    assert_eq!(view.help(), [("\u{2191}\u{2193}", "Move"), ("Esc", "Back")]);
    assert_eq!(
        view.key(press(KeyCode::Char('x'))),
        TeamReply::Handled,
        "keys stay in the open list"
    );
    assert!(view.confirm.is_none());
    assert_eq!(view.key(press(KeyCode::Esc)), TeamReply::Handled);
    assert!(view.messages.is_none());
    find(&frame(&view, 120, 36), "Tasks  3");
}

/// B19L merge (18.13 native teammates, b19 Messages): a teammate in the agents list opens
/// its Messages as a sub-agent does, as on the Team graph screen.
#[test]
fn capital_m_opens_a_focused_teammates_messages() {
    let mut view = TeamView::new(project());
    let query = view.next_query(NOW).unwrap();
    let mut status = team();
    status.agents.push(agent(
        7,
        AgentRole::Teammate,
        "claude-code",
        AgentStatus::Working,
        None,
    ));
    view.apply(&query, Ok(SetupView::Team(Box::new(status))));
    let at = view
        .agents()
        .iter()
        .position(|agent| agent.id == 7)
        .unwrap();
    for _ in 0..at {
        view.key(press(KeyCode::Down));
    }

    assert_eq!(view.key(press(KeyCode::Char('M'))), TeamReply::Handled);
    assert_eq!(view.notice, None);
    assert_eq!(
        view.next_query(NOW + 1),
        Some(SetupQuery::TeamMessages {
            project: project(),
            agent: 7,
            before: None,
        })
    );
}

#[test]
fn capital_m_on_the_master_says_where_its_conversation_is_and_on_a_task_asks_for_an_agent() {
    let mut view = loaded();
    assert_eq!(view.key(press(KeyCode::Char('M'))), TeamReply::Handled);
    assert!(view.messages.is_none());
    assert_eq!(view.notice, Some(Notice::plain(MASTER_ELSEWHERE)));
    find(&frame(&view, 120, 36), MASTER_ELSEWHERE);
    view.key(press(KeyCode::Tab));
    view.key(press(KeyCode::Char('M')));
    assert!(view.messages.is_none());
    assert_eq!(
        view.notice,
        Some(Notice::plain(
            "Pick an agent in the agents list to read its messages."
        ))
    );
    assert_eq!(view.next_query(NOW + 1), None, "a refused m reads nothing");
}

fn message_change(view: &mut TeamView, key: KeyCode, settings: MessageSettings, done: &str) {
    let TeamReply::Change(action) = view.key(press(key)) else {
        panic!("a Messages settings change expected: {:?}", view.notice);
    };
    assert_eq!(
        action,
        TeamAction::MessageSettings {
            project_id: project(),
            settings,
        }
    );
    assert_eq!(view.key(press(KeyCode::Char(' '))), TeamReply::Handled);
    assert_eq!(
        view.notice,
        Some(Notice::problem(
            "Still saving the last change. Try again in a moment."
        )),
        "one change at a time"
    );
    view.apply(
        &SetupQuery::TeamChange {
            action,
            generation: 1,
        },
        Ok(SetupView::Changed(OwnerResult::Applied)),
    );
    assert_eq!(view.notice, Some(Notice::plain(done)));
    let now = view.now_ms;
    let query = view
        .next_query(now)
        .expect("the team is read again right after a save");
    let mut read = team();
    read.message_settings = settings;
    view.apply(&query, Ok(SetupView::Team(Box::new(read))));
}

#[test]
fn o_opens_messages_settings_and_every_choice_of_every_setting_is_one_change() {
    let mut waiting = TeamView::new(project());
    assert_eq!(waiting.key(press(KeyCode::Char('o'))), TeamReply::Handled);
    assert!(waiting.message_row.is_none());
    assert_eq!(
        waiting.notice,
        Some(Notice::plain("The team is still loading."))
    );

    let mut view = loaded();
    assert_eq!(view.key(press(KeyCode::Char('o'))), TeamReply::Handled);
    for (width, height) in [(120, 36), (80, 24)] {
        let rows = frame(&view, width, height);
        assert!(
            find(&rows, "Messages settings").contains("For every sub-agent's Messages list here")
        );
        for (label, value) in [
            ("Keep the master's messages to agents", "On"),
            ("Messages kept per project", "2000"),
            ("Progress events", "Shown"),
            ("Task results", "First line, Enter shows all"),
            ("Your own tasks and prompts", "Shown"),
            ("Entries per page", "50"),
        ] {
            assert!(
                find(&rows, label).contains(&format!("{label:<40}{value}")),
                "{label} shows {value}\n{}",
                rows.join("\n")
            );
        }
        assert!(find(&rows, "Keep the master's messages").contains("> Keep"));
    }
    assert_eq!(
        view.help(),
        [
            ("\u{2191}\u{2193}", "Move"),
            ("Space", "Next choice"),
            ("\u{2190}\u{2192}", "Choice"),
            ("Esc", "Close"),
        ]
    );
    let mut saved = MessageSettings {
        keep_master_messages: false,
        ..MessageSettings::default()
    };
    message_change(
        &mut view,
        KeyCode::Char(' '),
        saved,
        "Keep the master's messages to agents: Off.",
    );
    view.key(press(KeyCode::Down));
    for (key, kept) in [
        (KeyCode::Right, 5000),
        (KeyCode::Right, 500),
        (KeyCode::Left, 5000),
    ] {
        saved.kept_per_project = kept;
        message_change(
            &mut view,
            key,
            saved,
            &format!("Messages kept per project: {kept}."),
        );
    }
    view.key(press(KeyCode::Down));
    saved.progress_shown = false;
    message_change(&mut view, KeyCode::Enter, saved, "Progress events: Hidden.");
    view.key(press(KeyCode::Down));
    saved.results = ResultsShown::Full;
    message_change(
        &mut view,
        KeyCode::Char(' '),
        saved,
        "Task results: Full text.",
    );
    view.key(press(KeyCode::Down));
    saved.owner_items_shown = false;
    message_change(
        &mut view,
        KeyCode::Right,
        saved,
        "Your own tasks and prompts: Hidden.",
    );
    view.key(press(KeyCode::Down));
    view.key(press(KeyCode::Down));
    for (key, size) in [
        (KeyCode::Right, 100),
        (KeyCode::Right, 25),
        (KeyCode::Left, 100),
    ] {
        saved.page_size = size;
        message_change(&mut view, key, saved, &format!("Entries per page: {size}."));
    }

    let rows = frame(&view, 80, 24);
    for (label, value) in [
        ("Keep the master's messages to agents", "Off"),
        ("Messages kept per project", "5000"),
        ("Progress events", "Hidden"),
        ("Task results", "Full text"),
        ("Your own tasks and prompts", "Hidden"),
        ("Entries per page", "100"),
    ] {
        assert!(find(&rows, label).contains(&format!("{label:<40}{value}")));
    }
    view.key(press(KeyCode::Up));
    saved.owner_items_shown = true;
    message_change(
        &mut view,
        KeyCode::Char(' '),
        saved,
        "Your own tasks and prompts: Shown.",
    );
    assert_eq!(view.key(press(KeyCode::Esc)), TeamReply::Handled);
    assert!(view.message_row.is_none());
    assert!(
        !frame(&view, 120, 36)
            .iter()
            .any(|row| row.contains("Messages settings"))
    );
}

/// A change made after a save but before the team read that follows it would be built
/// from the older read and undo the save, so it waits for a read sent after the save.
#[test]
fn a_settings_change_waits_for_the_team_read_sent_after_the_last_save() {
    let mut view = loaded();
    assert_eq!(view.key(press(KeyCode::Char('o'))), TeamReply::Handled);
    let TeamReply::Change(action) = view.key(press(KeyCode::Char(' '))) else {
        panic!("a Messages settings change expected: {:?}", view.notice);
    };
    let sent_before = view
        .next_query(NOW + REFRESH_MS)
        .expect("the paced team read is due");
    assert!(matches!(sent_before, SetupQuery::Team { .. }));
    view.apply(
        &SetupQuery::TeamChange {
            action,
            generation: 1,
        },
        Ok(SetupView::Changed(OwnerResult::Applied)),
    );
    let still = Some(Notice::problem(
        "Still saving the last change. Try again in a moment.",
    ));
    view.key(press(KeyCode::Down));
    assert_eq!(
        view.key(press(KeyCode::Char(' '))),
        TeamReply::Handled,
        "a change built before the read after the save would undo the save"
    );
    assert_eq!(view.notice, still);
    view.apply(&sent_before, Ok(SetupView::Team(Box::new(team()))));
    assert_eq!(
        view.key(press(KeyCode::Char(' '))),
        TeamReply::Handled,
        "a read sent before the save still shows the older settings"
    );
    assert_eq!(view.notice, still);
    let sent_after = view
        .next_query(NOW + REFRESH_MS)
        .expect("the read after the save goes at once");
    let saved = MessageSettings {
        keep_master_messages: false,
        ..MessageSettings::default()
    };
    let mut read = team();
    read.message_settings = saved;
    view.apply(&sent_after, Ok(SetupView::Team(Box::new(read))));
    assert_eq!(
        view.key(press(KeyCode::Char(' '))),
        TeamReply::Change(TeamAction::MessageSettings {
            project_id: project(),
            settings: MessageSettings {
                kept_per_project: 5000,
                ..saved
            },
        }),
        "the next change keeps the saved one"
    );
}
/// Stop teammate (S3, feat/harness-comms b07fcf11): p on a harness with no running teammate
/// says so; otherwise p asks which one, the panel numbers the running teammates p then 1 up,
/// oldest first and past finished and stopped ones, a number past them keeps asking, Esc
/// keeps them, and a number sends one StopTeammate for that teammate's pane.
#[test]
fn p_then_a_number_stops_that_running_teammate_of_the_selected_harness() {
    let mut view = loaded();
    view.key(press(KeyCode::Down));
    let lead = view.agents()[1].id;
    assert_eq!(lead, 2);
    assert_eq!(view.key(press(KeyCode::Char('p'))), TeamReply::Handled);
    assert_eq!(view.ask, None);
    assert_eq!(
        view.notice,
        Some(Notice::plain("That harness has no running teammates."))
    );
    let teammate = |pane: &str, ended: bool, stopped: bool, minutes: i64| crate::team::Teammate {
        pane: pane.into(),
        ended,
        stopped,
        seen_ms: NOW - minutes * MINUTE,
    };
    let mut status = team();
    status.agents[1].teammates = vec![
        teammate("%7", false, false, 5),
        teammate("%8", true, false, 4),
        teammate("%9", false, true, 3),
        teammate("%11", false, false, 2),
    ];
    reread(&mut view, status, NOW + MINUTE);
    let rows = frame(&view, 120, 60);
    assert!(find(&rows, "%7").contains("running, p then 1"));
    assert!(find(&rows, "%8").contains("finished"));
    assert!(find(&rows, "%9").contains("stopped"));
    assert!(find(&rows, "%11").contains("running, p then 2"));
    assert_eq!(view.key(press(KeyCode::Char('p'))), TeamReply::Handled);
    assert_eq!(view.ask, Some(Ask::Teammate(lead)));
    assert_eq!(view.help(), vec![("1-9", "Stop teammate"), ("Esc", "Keep")]);
    assert!(find(&frame(&view, 120, 60), "Stop which teammate of").contains("1-2 Teammate"));
    assert_eq!(view.key(press(KeyCode::Char('3'))), TeamReply::Handled);
    assert_eq!(
        view.ask,
        Some(Ask::Teammate(lead)),
        "a number past the running teammates keeps asking"
    );
    assert_eq!(view.key(press(KeyCode::Esc)), TeamReply::Handled);
    assert_eq!(view.ask, None);
    view.key(press(KeyCode::Char('p')));
    let TeamReply::Change(action) = view.key(press(KeyCode::Char('2'))) else {
        panic!("p then 2 sends a change");
    };
    assert_eq!(
        action,
        TeamAction::StopTeammate {
            project_id: project(),
            agent: lead,
            pane: "%11".into(),
        }
    );
    assert_eq!(view.ask, None);
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
            "Stopping that teammate. Its harness is told."
        ))
    );
}

/// The team view's help (S3, feat/harness-comms b07fcf11) names every key once, each with
/// its own label, Stop teammate on p beside Stop agent on z.
#[test]
fn the_team_help_lists_every_key_once_with_stop_teammate_on_p() {
    let help = loaded().help();
    for (at, (key, label)) in help.iter().enumerate() {
        assert!(
            help[..at].iter().all(|(seen, _)| seen != key),
            "{key} is listed twice: {help:?}"
        );
        assert!(
            help[..at].iter().all(|(_, seen)| seen != label),
            "{label} is listed twice: {help:?}"
        );
    }
    for pair in [
        ("p", "Stop teammate"),
        ("z", "Stop agent"),
        ("m", "Master's branch"),
        ("g", "Ask before merge"),
        ("T", "Token budget"),
    ] {
        assert!(help.contains(&pair), "{pair:?} is missing: {help:?}");
    }
}

/// W moves Terminals per window from the shown choice to the next, 4, 6, 9 and back to 4;
/// each choice shows on the team screen and its saved change says what Open all terminals
/// now does (owner 2026-11-07, OS-4).
#[test]
fn w_moves_terminals_per_window_through_four_six_and_nine() {
    use crate::team::TerminalsPerWindow::{Four, Nine, Six};
    for (shown, next, count) in [(Six, Nine, 9), (Nine, Four, 4), (Four, Six, 6)] {
        let mut status = team();
        status.terminals_per_window = shown;
        let mut view = TeamView::new(project());
        let query = view.next_query(NOW).unwrap();
        view.apply(&query, Ok(SetupView::Team(Box::new(status))));
        let rows = frame(&view, 120, 40);
        assert!(
            rows.iter()
                .any(|row| row.contains(&format!("Terminals per window  {}", u8::from(shown)))),
            "{}",
            rows.join("\n")
        );
        let reply = view.key(press(KeyCode::Char('W')));
        assert_eq!(
            reply,
            TeamReply::Change(TeamAction::TerminalsPerWindow {
                project_id: project(),
                per_window: next
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
                "Open all terminals now puts {count} terminals in each window."
            )))
        );
    }
}

/// FBT3-S1: with the project's agent access unreadable, E says so instead of calling the
/// project below Full access.
#[test]
fn e_says_the_agent_access_cannot_be_read_when_it_cannot() {
    let mut view = loaded();
    let mut unreadable = team();
    unreadable.access_unreadable = true;
    reread(&mut view, unreadable, NOW + MINUTE);
    assert_eq!(view.key(press(KeyCode::Char('E'))), TeamReply::Handled);
    assert_eq!(
        view.notice,
        Some(Notice::plain(
            "This project's agent access can't be read, so the master's conflict fixes wait for your Accept."
        ))
    );
}

#[test]
fn e_switches_what_the_master_does_with_its_conflict_fixes_only_at_full_access() {
    use crate::team::MasterLands;
    let row = "When the master resolves conflicts";
    let mut view = loaded();
    assert!(view.help().contains(&("E", "Master merges")));
    assert!(
        !view.help().contains(&("W", "Master merges")),
        "W stays free for Terminals per window"
    );
    assert!(!shows(&view, row), "the row shows only at Full access");
    assert_eq!(view.key(press(KeyCode::Char('E'))), TeamReply::Handled);
    assert_eq!(
        view.notice,
        Some(Notice::plain(
            "The master's own change sets land by themselves only at Full access."
        ))
    );
    let mut full = team();
    full.master_lands = Some(MasterLands::Merges);
    reread(&mut view, full.clone(), NOW + MINUTE);
    assert!(shows(&view, &format!("{row}  Merge into main itself")));
    let terminals = view.key(press(KeyCode::Char('W')));
    assert!(
        matches!(
            terminals,
            TeamReply::Change(TeamAction::TerminalsPerWindow { .. })
        ),
        "W switches Terminals per window, never the master merge setting"
    );
    applied(&mut view, terminals);
    reread(&mut view, full, NOW + MINUTE + 5);
    assert!(shows(&view, &format!("{row}  Merge into main itself")));
    let reply = view.key(press(KeyCode::Char('E')));
    assert_eq!(
        reply,
        TeamReply::Change(TeamAction::MasterLands {
            project_id: project(),
            mode: MasterLands::Waits,
        })
    );
    applied(&mut view, reply);
    assert_eq!(
        view.notice,
        Some(Notice::plain(
            "The master now waits for your Accept on its conflict fixes."
        ))
    );
    let mut waits = team();
    waits.master_lands = Some(MasterLands::Waits);
    reread(&mut view, waits, NOW + MINUTE + 10);
    assert!(shows(&view, &format!("{row}  Wait for my Accept")));
    assert_eq!(
        view.key(press(KeyCode::Char('E'))),
        TeamReply::Change(TeamAction::MasterLands {
            project_id: project(),
            mode: MasterLands::Merges,
        })
    );
}
