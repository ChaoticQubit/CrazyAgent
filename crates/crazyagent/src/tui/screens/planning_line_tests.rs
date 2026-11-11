use super::new_project_view::Look;
use super::planning_line::{PLAN_NOW, Planning, planning, planning_key, planning_value};
use crate::project_setup::{ResearchJob, ResearchKind, ResearchStatus, SetupAction};
use crate::protocol::ProjectId;
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
    assert_eq!(
        planning_key(&running, &project, KeyCode::Char('r')),
        Some(Err("Only a failed or stopped plan can run again."))
    );

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
