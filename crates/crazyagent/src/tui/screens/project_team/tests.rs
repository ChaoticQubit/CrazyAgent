use super::*;
use crate::client_controller::OwnerController;
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
        paused: false,
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
    let sub = find(&wide, "Sub-agent 1");
    assert!(sub.contains("Claude Max - work"));
    assert!(sub.contains("Working  Build the charts component"));
    assert!(find(&wide, "Sub-agent 2").contains("Stopped"));
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
fn the_panel_follows_the_focused_task() {
    let mut view = loaded();
    assert_eq!(view.key(press(KeyCode::Tab)), TeamReply::Handled);
    assert_eq!(view.focus, TeamFocus::Tasks);
    let rows = frame(&view, 80, 24);
    assert!(find(&rows, "Task from the master").contains("running 6 min"));
    assert!(find(&rows, "Use the readings API.").starts_with("  "));
    assert!(
        rows.iter()
            .any(|row| row.contains("Agent         Sub-agent 1"))
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
