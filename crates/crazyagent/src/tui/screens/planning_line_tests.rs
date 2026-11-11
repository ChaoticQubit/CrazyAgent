use super::new_project_view::Look;
use super::planning_line::{
    PLAN_NOW, Planning, STATUS_CHECKING, STATUS_OFF, StatusLine, planning, planning_key,
    planning_value, refreshes_status, status_counts,
};
use crate::project_setup::{ResearchJob, ResearchKind, ResearchStatus, SetupAction};
use crate::protocol::ProjectId;
use crate::roadmap_status::{ItemState, ItemStatus, Refresh, RoadmapItem, RoadmapStatus};
use crate::tui::client::roadmap::{RoadmapEvent, RoadmapQuery, RoadmapReply};
use crossterm::event::KeyCode;

fn job(id: i64, kind: ResearchKind, status: ResearchStatus, error: Option<&str>) -> ResearchJob {
    ResearchJob {
        id,
        project_id: ProjectId::new("notes-cli").unwrap(),
        kind,
        kind_label: kind.label().into(),
        status,
        status_label: status.label().into(),
        report_path: kind.report_path(),
        harness: "codex".into(),
        tools: true,
        attempts: 1,
        claimer: None,
        error: error.map(str::to_owned),
        cancel_requested: false,
        output_bytes: 0,
        created_ms: 1_793_000_000_000,
        started_ms: Some(1_793_000_001_000),
        heartbeat_ms: None,
        finished_ms: Some(1_793_000_002_000),
    }
}

#[test]
fn planning_line_shows_failed_with_retry() {
    let project = ProjectId::new("notes-cli").unwrap();
    let jobs = vec![
        job(1, ResearchKind::MarketResearch, ResearchStatus::Done, None),
        job(2, ResearchKind::RoadmapPlanning, ResearchStatus::Done, None),
        job(
            3,
            ResearchKind::RoadmapPlanning,
            ResearchStatus::Failed,
            Some("the harness stopped before it wrote the roadmap"),
        ),
    ];
    let state = planning(&jobs);
    assert_eq!(
        state,
        Planning::Failed {
            job: 3,
            cancelled: false,
            reason: Some("the harness stopped before it wrote the roadmap".into()),
        },
        "the newest planning job decides"
    );
    let line = planning_value(&state, &Look::plain()).text();
    assert_eq!(
        line,
        "! Failed: the harness stopped before it wrote the roadmap   r Retry"
    );
    assert_eq!(
        planning_key(&state, &project, KeyCode::Char('r')),
        Some(Ok(SetupAction::RetryResearch {
            project_id: project.clone(),
            job: 3,
        }))
    );
    assert_eq!(planning_key(&state, &project, KeyCode::Char('p')), None);

    let stopped = planning(&[job(
        4,
        ResearchKind::RoadmapPlanning,
        ResearchStatus::Cancelled,
        None,
    )]);
    assert_eq!(
        planning_value(&stopped, &Look::plain()).text(),
        "Stopped   r Retry"
    );

    let running = planning(&[job(
        5,
        ResearchKind::RoadmapPlanning,
        ResearchStatus::Running,
        None,
    )]);
    assert_eq!(planning_value(&running, &Look::plain()).text(), "Planning");
    assert_eq!(
        planning_key(&running, &project, KeyCode::Char('p')),
        Some(Err(
            "A roadmap is already being planned for this project. Wait for it to finish."
        ))
    );
    assert_eq!(planning_key(&running, &project, KeyCode::Char('r')), None);
    assert!(refreshes_status(&running, KeyCode::Char('r')));
    assert!(!refreshes_status(&state, KeyCode::Char('r')));
    assert!(!refreshes_status(&running, KeyCode::Char('p')));

    let waiting = planning(&[
        job(
            6,
            ResearchKind::MarketResearch,
            ResearchStatus::Running,
            None,
        ),
        job(
            7,
            ResearchKind::RoadmapPlanning,
            ResearchStatus::Queued,
            None,
        ),
    ]);
    assert_eq!(
        waiting,
        Planning::Waiting,
        "the engine holds planning back while research runs"
    );
    assert_eq!(
        planning_value(&waiting, &Look::plain()).text(),
        "Waiting for research"
    );

    let idle = planning(&[job(
        8,
        ResearchKind::MarketResearch,
        ResearchStatus::Running,
        None,
    )]);
    assert_eq!(idle, Planning::Idle);
    assert_eq!(
        planning_value(&idle, &Look::plain()).text(),
        format!("Not planned   p {PLAN_NOW}")
    );
    assert_eq!(
        planning_key(&idle, &project, KeyCode::Char('p')),
        Some(Ok(SetupAction::PlanRoadmap {
            project_id: project.clone(),
        }))
    );
    assert_eq!(planning_key(&idle, &project, KeyCode::Char('x')), None);
}

#[test]
fn planning_asks_before_it_can_replace_the_roadmap() {
    use super::project::{ProjectReply, ProjectScreen};
    use crate::project_setup::{ContextItem, ContextKind, ProjectContext};
    use crossterm::event::{KeyEvent, KeyModifiers};

    let project = ProjectId::new("notes-cli").unwrap();
    let press = |screen: &mut ProjectScreen, code: KeyCode| {
        screen.key(KeyEvent::new(code, KeyModifiers::NONE))
    };
    let context = |items: Vec<ContextItem>| ProjectContext {
        project_id: project.clone(),
        folder: "/Users/mira/Projects/notes-cli".into(),
        items,
    };
    let brief = ContextItem {
        path: "context/brief.md".into(),
        kind: ContextKind::Brief,
        bytes: 10,
        linked: false,
    };
    let plan = SetupAction::PlanRoadmap {
        project_id: project.clone(),
    };

    let mut screen = ProjectScreen::new(project.clone(), "Notes".into(), None);
    screen.jobs = Some(vec![]);
    screen.context = Some(context(vec![brief.clone()]));
    assert_eq!(
        press(&mut screen, KeyCode::Char('p')),
        ProjectReply::Change(Box::new(plan.clone())),
        "nothing to replace, so it plans at once"
    );

    screen.context = Some(context(vec![
        brief.clone(),
        ContextItem {
            path: "repo/docs/roadmap.md".into(),
            kind: ContextKind::Plan,
            bytes: 10,
            linked: true,
        },
    ]));
    assert_eq!(
        press(&mut screen, KeyCode::Char('p')),
        ProjectReply::Change(Box::new(plan.clone())),
        "a roadmap linked from the repo is never written by planning"
    );

    screen.context = Some(context(vec![
        brief,
        ContextItem {
            path: "context/roadmap.md".into(),
            kind: ContextKind::Plan,
            bytes: 10,
            linked: false,
        },
    ]));
    assert_eq!(
        press(&mut screen, KeyCode::Char('p')),
        ProjectReply::Handled
    );
    assert_eq!(
        screen.confirm_plan,
        Some(plan.clone()),
        "a roadmap the owner wrote by hand"
    );
    let drawn = super::project_view::project_screen(&mut screen, 80, 24, &Look::plain());
    let text: String = drawn
        .rows
        .iter()
        .map(|row| row.text())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text.contains("Replace the roadmap?"), "{text}");
    assert_eq!(
        press(&mut screen, KeyCode::Char('n')),
        ProjectReply::Handled
    );
    assert_eq!(screen.confirm_plan, None);

    screen.context = None;
    screen.jobs = Some(vec![job(
        9,
        ResearchKind::RoadmapPlanning,
        ResearchStatus::Done,
        None,
    )]);
    assert_eq!(
        press(&mut screen, KeyCode::Char('p')),
        ProjectReply::Handled
    );
    assert_eq!(
        press(&mut screen, KeyCode::Char('y')),
        ProjectReply::Change(Box::new(plan)),
        "a plan that ran before asks, then plans on yes"
    );

    screen.jobs = Some(vec![job(
        10,
        ResearchKind::RoadmapPlanning,
        ResearchStatus::Failed,
        None,
    )]);
    assert_eq!(
        press(&mut screen, KeyCode::Char('r')),
        ProjectReply::Handled
    );
    assert_eq!(
        screen.confirm_plan,
        Some(SetupAction::RetryResearch {
            project_id: project,
            job: 10,
        }),
        "Retry asks too"
    );
}

const NOW: i64 = 1_800_000_000_000;

fn alpha() -> ProjectId {
    ProjectId::new("alpha").unwrap()
}

/// A status checked a minute before `NOW` with one item per given state.
fn checked(states: &[ItemStatus]) -> RoadmapStatus {
    RoadmapStatus {
        items: states
            .iter()
            .map(|status| ItemState {
                item: RoadmapItem {
                    section: "V1".into(),
                    text: "Sign in".into(),
                },
                status: *status,
                evidence: String::new(),
            })
            .collect(),
        checked_ms: Some(NOW - 60_000),
        running: false,
        stale: false,
        refresh: Refresh::AfterLanding,
        source: Some("docs/plan.md".into()),
        problem: None,
    }
}

/// Sends the line's next request and answers it. Returns the request sent.
fn exchange(line: &mut StatusLine, now: i64, result: Result<RoadmapReply, String>) -> RoadmapQuery {
    let query = line.next_query(now).expect("a request is due");
    line.apply(&RoadmapEvent {
        query: query.clone(),
        result,
    });
    query
}

fn shown(line: &StatusLine) -> String {
    line.value(&Look::plain(), NOW).text()
}

#[test]
fn the_status_line_shows_the_counts_when_it_was_checked_and_the_refresh_key() {
    let mut line = StatusLine::new(alpha());
    assert_eq!(shown(&line), "Loading...");
    let states = [
        ItemStatus::Tested,
        ItemStatus::InProgress,
        ItemStatus::Pending,
    ];
    let read = line.next_query(NOW).unwrap();
    assert_eq!(read, RoadmapQuery::Read { project: alpha() });
    assert_eq!(line.next_query(NOW), None, "one request at a time");
    line.apply(&RoadmapEvent {
        query: read,
        result: Ok(RoadmapReply::Status(Box::new(checked(&states)))),
    });
    assert_eq!(
        shown(&line),
        "1 tested, 0 done, 1 in progress, 1 pending   Checked 1 min ago   r Refresh"
    );
    let mut never = checked(&[]);
    never.checked_ms = None;
    assert_eq!(status_counts(&never), "Not checked yet");
    assert_eq!(status_counts(&checked(&[])), "No roadmap items");
}

#[test]
fn the_status_line_is_read_again_slowly_when_idle_and_quickly_while_a_check_runs() {
    let mut line = StatusLine::new(alpha());
    let idle = checked(&[ItemStatus::Done]);
    exchange(&mut line, NOW, Ok(RoadmapReply::Status(Box::new(idle))));
    assert_eq!(line.next_query(NOW + 59_999), None);
    let mut running = checked(&[ItemStatus::Done]);
    running.running = true;
    let reply = Ok(RoadmapReply::Status(Box::new(running)));
    assert!(matches!(
        exchange(&mut line, NOW + 60_000, reply),
        RoadmapQuery::Read { .. }
    ));
    assert_eq!(
        shown(&line),
        "0 tested, 1 done, 0 in progress, 0 pending   Checking now"
    );
    assert_eq!(line.refresh(), Err(STATUS_CHECKING));
    assert_eq!(line.next_query(NOW + 64_999), None);
    assert!(line.next_query(NOW + 65_000).is_some());
}

#[test]
fn r_asks_for_a_check_then_reads_the_status_again_and_off_refuses_it() {
    let mut line = StatusLine::new(alpha());
    let idle = checked(&[ItemStatus::Pending]);
    exchange(&mut line, NOW, Ok(RoadmapReply::Status(Box::new(idle))));
    assert_eq!(line.refresh(), Ok(()));
    let asked = exchange(&mut line, NOW + 1, Ok(RoadmapReply::Done));
    assert_eq!(asked, RoadmapQuery::Refresh { project: alpha() });
    let mut off = checked(&[ItemStatus::Pending]);
    off.refresh = Refresh::Off;
    let reply = Ok(RoadmapReply::Status(Box::new(off)));
    assert!(
        matches!(
            exchange(&mut line, NOW + 2, reply),
            RoadmapQuery::Read { .. }
        ),
        "a Refresh that was taken reads the status again at once"
    );
    assert_eq!(shown(&line), "Off");
    assert_eq!(line.refresh(), Err(STATUS_OFF));
    assert_eq!(line.next_query(NOW + 3), None, "Off queues no check");

    let mut refused = StatusLine::new(alpha());
    assert_eq!(refused.refresh(), Ok(()));
    exchange(&mut refused, NOW, Err("roadmap_status_off".into()));
    assert_eq!(shown(&refused), format!("! {STATUS_OFF}"));
}
