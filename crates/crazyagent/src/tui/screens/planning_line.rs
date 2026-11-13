use super::new_project_view::{Look, Row};
use crate::project_setup::{ResearchJob, ResearchKind, ResearchStatus, SetupAction};
use crate::protocol::ProjectId;
use crate::roadmap_status::{ItemStatus, Refresh, RoadmapStatus};
use crate::tui::client::roadmap::{RoadmapEvent, RoadmapQuery, RoadmapReply};
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
/// unless a job waits or runs, and `r` runs a failed or stopped planning job again; in any
/// other state `r` belongs to the roadmap status line. Takes the state, the project and the
/// key. Returns the change to send, a plain line when the state offers no such action, or
/// none for any other key.
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
        _ => None,
    }
}

const POLL_ACTIVE_MS: i64 = 5_000;
const POLL_IDLE_MS: i64 = 60_000;
const STATUS_WORDS: [(ItemStatus, &str); 4] = [
    (ItemStatus::Tested, "tested"),
    (ItemStatus::Done, "done"),
    (ItemStatus::InProgress, "in progress"),
    (ItemStatus::Pending, "pending"),
];
/// What `r` says when the roadmap status is switched off for the project.
pub(in crate::tui) const STATUS_OFF: &str = "Roadmap status is off in Project settings.";
/// What `r` says while a check already runs.
pub(in crate::tui) const STATUS_CHECKING: &str = "The roadmap status is being checked now.";

const _: () = assert!(POLL_ACTIVE_MS > 0 && POLL_ACTIVE_MS < POLL_IDLE_MS);

/// The roadmap status line of a project page: the status last read, why the last request
/// failed, the request in flight, a Refresh the owner asked for and when the status was
/// last read.
#[derive(Debug)]
pub(in crate::tui) struct StatusLine {
    project: ProjectId,
    status: Option<RoadmapStatus>,
    problem: Option<String>,
    sent: Option<RoadmapQuery>,
    queued: Option<RoadmapQuery>,
    read_ms: Option<i64>,
}

impl StatusLine {
    /// The line of one project before anything is read. Takes the project.
    pub(in crate::tui) fn new(project: ProjectId) -> Self {
        Self {
            project,
            status: None,
            problem: None,
            sent: None,
            queued: None,
            read_ms: None,
        }
    }

    /// The next request to send, one at a time: a Refresh the owner asked for, else a read
    /// when none was made or the last one is older than its period, the short one while a
    /// check runs or is due. Takes now. Returns the request, or none.
    pub(in crate::tui) fn next_query(&mut self, now_ms: i64) -> Option<RoadmapQuery> {
        if self.sent.is_some() {
            return None;
        }
        let query = match self.queued.take() {
            Some(asked) => asked,
            None => {
                let active = match &self.status {
                    Some(status) => status.running || status.stale,
                    None => self.problem.is_none(),
                };
                let period = if active { POLL_ACTIVE_MS } else { POLL_IDLE_MS };
                let due = match self.read_ms {
                    None => true,
                    Some(at) => now_ms.saturating_sub(at) >= period,
                };
                if !due {
                    return None;
                }
                self.read_ms = Some(now_ms);
                RoadmapQuery::Read {
                    project: self.project.clone(),
                }
            }
        };
        self.sent = Some(query.clone());
        Some(query)
    }

    /// Takes the reply to the request in flight: a status replaces the one shown, a Refresh
    /// that was taken or refused reads the status again at once. Takes the event; a reply to
    /// any other request changes nothing.
    pub(in crate::tui) fn apply(&mut self, event: &RoadmapEvent) {
        if self.sent.as_ref() != Some(&event.query) {
            return;
        }
        self.sent = None;
        match (&event.query, &event.result) {
            (_, Ok(RoadmapReply::Status(status))) => {
                self.status = Some((**status).clone());
                self.problem = None;
            }
            (_, Ok(RoadmapReply::Done)) => self.read_ms = None,
            (query, Err(code)) => {
                self.problem = Some(match code.as_str() {
                    "roadmap_status_off" => STATUS_OFF.to_owned(),
                    other => super::project::plain_problem(other),
                });
                if matches!(query, RoadmapQuery::Refresh { .. }) {
                    self.read_ms = None;
                }
            }
        }
    }

    /// Answers `r`: queues a check now unless the setting is Off or a check already runs.
    /// Returns unit once queued, or the plain line saying why not.
    pub(in crate::tui) fn refresh(&mut self) -> Result<(), &'static str> {
        match &self.status {
            Some(status) if status.refresh == Refresh::Off => Err(STATUS_OFF),
            Some(status) if status.running => Err(STATUS_CHECKING),
            _ => {
                self.queued = Some(RoadmapQuery::Refresh {
                    project: self.project.clone(),
                });
                Ok(())
            }
        }
    }

    /// Draws the status as one value: the counts, when it was checked or that a check runs,
    /// why the last check failed, and the Refresh key when it can be pressed. Takes the look
    /// and now. Returns the row, without a label.
    pub(in crate::tui) fn value(&self, look: &Look, now_ms: i64) -> Row {
        let Some(status) = &self.status else {
            return match &self.problem {
                Some(problem) => Row::of(format!("! {problem}"), look.warning),
                None => Row::of("Loading...", look.muted),
            };
        };
        if status.refresh == Refresh::Off {
            return Row::of("Off", look.muted);
        }
        let mut row = Row::of(status_counts(status), look.fg);
        row = match (status.running, status.checked_ms) {
            (true, _) => row.push("   Checking now", look.running),
            (false, Some(at)) => row.push(format!("   Checked {}", ago(now_ms, at)), look.muted),
            (false, None) => row,
        };
        if let Some(problem) = &status.problem {
            row = if status.running || status.stale {
                row.push(format!("   {problem}"), look.muted)
            } else {
                row.push(format!("   ! {problem}"), look.warning)
            };
        } else if let Some(problem) = &self.problem {
            row = row.push(format!("   ! {problem}"), look.warning);
        }
        if status.running {
            row
        } else {
            row.push("   r Refresh", look.muted)
        }
    }
}

/// The counts of a status in the order the views use: tested, done, in progress, pending;
/// "Not checked yet" before the first check and "No roadmap items" for an empty roadmap.
/// Takes the status. Returns the words.
pub(in crate::tui) fn status_counts(status: &RoadmapStatus) -> String {
    if status.items.is_empty() {
        return match status.checked_ms {
            Some(_) => "No roadmap items".into(),
            None => "Not checked yet".into(),
        };
    }
    let words: Vec<String> = STATUS_WORDS
        .iter()
        .map(|(kind, word)| {
            let count = status
                .items
                .iter()
                .filter(|state| state.status == *kind)
                .count();
            format!("{count} {word}")
        })
        .collect();
    assert_eq!(words.len(), STATUS_WORDS.len());
    words.join(", ")
}

/// How long ago a time was, in whole minutes, hours or days. Takes now and the time.
/// Returns the words, "just now" under a minute or for a time ahead of now.
fn ago(now_ms: i64, at_ms: i64) -> String {
    let minutes = now_ms.saturating_sub(at_ms).max(0) / 60_000;
    match minutes {
        0 => "just now".into(),
        1..60 => format!("{minutes} min ago"),
        60..2880 => format!("{} h ago", minutes / 60),
        _ => format!("{} d ago", minutes / 1440),
    }
}

/// Whether `r` asks for a roadmap status check in this planning state: always, except
/// where it runs a failed or stopped plan again. Takes the state and the key.
pub(in crate::tui) fn refreshes_status(state: &Planning, key: KeyCode) -> bool {
    key == KeyCode::Char('r') && !matches!(state, Planning::Failed { .. })
}
