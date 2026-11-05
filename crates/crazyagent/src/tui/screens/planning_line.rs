use super::new_project_view::{Look, Row};
use crate::project_setup::{ResearchJob, ResearchKind, ResearchStatus, SetupAction};
use crate::protocol::ProjectId;
use crossterm::event::KeyCode;

/// Where a project's roadmap planning stands: not planned, waiting for research, planning,
/// done, or failed or stopped, with the planning job it reads from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::tui) enum Planning {
    Idle,
    Waiting,
    Active {
        job: i64,
        status: ResearchStatus,
    },
    Done {
        job: i64,
    },
    Failed {
        job: i64,
        cancelled: bool,
        reason: Option<String>,
    },
}

const ACTIVE: [ResearchStatus; 3] = [
    ResearchStatus::Queued,
    ResearchStatus::NeedsPool,
    ResearchStatus::Running,
];

/// The words of the action that queues a planning job for any project.
pub(in crate::tui) const PLAN_NOW: &str = "Plan roadmap now";

/// Reads the planning state from the project's jobs. The newest planning job decides; the
/// engine holds a waiting one back while any research job of the project waits or runs.
/// Takes the jobs. Returns the state.
pub(in crate::tui) fn planning(jobs: &[ResearchJob]) -> Planning {
    let newest = jobs
        .iter()
        .filter(|job| job.kind == ResearchKind::RoadmapPlanning)
        .max_by_key(|job| job.id);
    let research_active = jobs
        .iter()
        .any(|job| job.kind != ResearchKind::RoadmapPlanning && ACTIVE.contains(&job.status));
    match newest {
        None => Planning::Idle,
        Some(job)
            if job.status != ResearchStatus::Running
                && ACTIVE.contains(&job.status)
                && research_active =>
        {
            Planning::Waiting
        }
        Some(job) if ACTIVE.contains(&job.status) => Planning::Active {
            job: job.id,
            status: job.status,
        },
        Some(job) if job.status == ResearchStatus::Done => Planning::Done { job: job.id },
        Some(job) => Planning::Failed {
            job: job.id,
            cancelled: job.status == ResearchStatus::Cancelled,
            reason: job
                .error
                .as_deref()
                .map(str::trim)
                .filter(|reason| !reason.is_empty())
                .map(str::to_owned),
        },
    }
}

/// Draws the planning state as one value: the status word, what the job reports, and the key
/// of the one action. Takes the state and the look. Returns the row, without a label.
pub(in crate::tui) fn planning_value(state: &Planning, look: &Look) -> Row {
    let plan = format!("   p {PLAN_NOW}");
    match state {
        Planning::Idle => Row::of("Not planned", look.fg).push(plan, look.muted),
        Planning::Waiting => Row::of("Waiting for research", look.muted),
        Planning::Active { status, .. } => Row::of(
            match status {
                ResearchStatus::Queued => "Planning, waiting to start",
                ResearchStatus::NeedsPool => "Planning, waiting for a pool",
                _ => "Planning",
            },
            look.running,
        ),
        Planning::Done { .. } => Row::of("Done, in Context", look.success).push(plan, look.muted),
        Planning::Failed {
            cancelled: true, ..
        } => Row::of("Stopped", look.muted).push("   r Retry", look.muted),
        Planning::Failed { reason, .. } => Row::of(
            format!(
                "! Failed: {}",
                reason
                    .as_deref()
                    .unwrap_or("planning stopped with an error")
            ),
            look.warning,
        )
        .push("   r Retry", look.muted),
    }
}

/// Answers the planning keys of the Overview and Settings tabs: `p` plans a roadmap now
/// unless a job waits or runs, and `r` runs a failed or stopped planning job again. Takes the
/// state, the project and the key. Returns the change to send, a plain line when the state
/// offers no such action, or none for any other key.
pub(in crate::tui) fn planning_key(
    state: &Planning,
    project: &ProjectId,
    key: KeyCode,
) -> Option<Result<SetupAction, &'static str>> {
    match (key, state) {
        (KeyCode::Char('p'), Planning::Idle | Planning::Done { .. }) => {
            Some(Ok(SetupAction::PlanRoadmap {
                project_id: project.clone(),
            }))
        }
        (KeyCode::Char('p'), Planning::Waiting) => {
            Some(Err("The roadmap is planned once research finishes."))
        }
        (KeyCode::Char('p'), Planning::Active { .. }) => Some(Err(
            "A roadmap is already being planned for this project. Wait for it to finish.",
        )),
        (KeyCode::Char('r'), Planning::Failed { job, .. }) => {
            Some(Ok(SetupAction::RetryResearch {
                project_id: project.clone(),
                job: *job,
            }))
        }
        (KeyCode::Char('r'), _) => Some(Err("Only a failed or stopped plan can run again.")),
        _ => None,
    }
}
