use super::icloud::{Download, Progress};
use super::{ADD_OVERRIDES_MAX, AddMode, AddOverride, Tally};
use crate::engine::EngineError;
use crate::project_setup::ProjectSetup;
use crate::protocol::{ProjectId, ProjectSpec, RequestId};
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex, PoisonError};

/// The soft refusals of an add the owner may go past with an override: a job that ends on
/// one waits for the owner's answer instead of failing.
pub const NEEDS_ANSWER: [&str; 3] = [
    "home_move_worktrees",
    "home_copy_linked_checkout",
    "home_copy_too_large",
];

/// What a job does: adds an existing folder, moves a registered project kept outside the
/// projects home into it, deletes a project with its folders (Delete everything), or builds
/// a New Project in the projects home (DEC-177 addendum 2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobKind {
    Add,
    MoveProject,
    Delete,
    Setup,
}

/// What a client asks a job to do: bring a folder into the projects home by the path the
/// owner gave, bring a registered project in by its id, delete a project, named by the
/// exact name the owner typed, or build a New Project from the owner's answers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum JobSubject {
    Add {
        path: String,
    },
    MoveProject {
        project_id: ProjectId,
    },
    Delete {
        project_id: ProjectId,
        name: String,
    },
    Setup {
        project_id: ProjectId,
        spec: ProjectSpec,
        setup: ProjectSetup,
    },
}

impl JobSubject {
    /// The kind of job this subject starts.
    pub fn kind(&self) -> JobKind {
        match self {
            Self::Add { .. } => JobKind::Add,
            Self::MoveProject { .. } => JobKind::MoveProject,
            Self::Delete { .. } => JobKind::Delete,
            Self::Setup { .. } => JobKind::Setup,
        }
    }
}

/// The part of a job that was running.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Downloading,
    Stopping,
    Copying,
    Clearing,
    Trashing,
    Building,
}

/// What a job is doing, or how it ended. A project move or a delete waits in `Stopping` for
/// the project's agents to stop before anything moves, and reads `Copying` with no counts
/// before it counted them; a delete then moves its folders to the Trash in `Trashing`, and
/// `CutShort` is a delete an engine stop cut short, with how many of its folders reached the
/// Trash. A New Project reads `Building` with how many of its documents it copied once it
/// counted them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum JobState {
    Downloading { progress: Progress },
    Stopping { agents_left: u64 },
    Copying { copied: u64, checked: u64 },
    Clearing,
    Trashing { done: u64, total: u64 },
    Done,
    Failed { code: String, phase: Phase },
    Cancelled { phase: Phase },
    NeedsAnswer { code: String },
    CutShort { trashed: u64, total: u64 },
    Building { done: u64, total: u64 },
}

/// One job as a client sees it: its id, which is the change's request id, its kind, the
/// project a move, a delete or a New Project takes, the folder (for a delete or a New Project
/// the project's name, which labels its end line), the mode (a delete reads Move, unused), the soft
/// refusals it goes past, whether a stop was asked and its state. A client answering a soft
/// refusal starts again with these overrides and the new one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JobView {
    pub id: RequestId,
    pub kind: JobKind,
    pub project_id: Option<ProjectId>,
    pub source: String,
    pub mode: AddMode,
    pub overrides: Vec<AddOverride>,
    pub stopping: bool,
    pub state: JobState,
}

impl JobView {
    /// The subject that starts this job again, for a retry or an answer to its soft refusal.
    /// A delete has none: it starts again only from the remove ask, where the owner types the
    /// exact name (DEC-177 addendum 1, Q1 A); nor has a New Project, whose answers the job does
    /// not keep (addendum 2, Q3 A). Output: the subject, or None for a delete, a New Project or
    /// a view whose kind and project do not match.
    pub fn subject(&self) -> Option<JobSubject> {
        match (self.kind, &self.project_id) {
            (JobKind::Add, None) => Some(JobSubject::Add {
                path: self.source.clone(),
            }),
            (JobKind::MoveProject, Some(project_id)) => Some(JobSubject::MoveProject {
                project_id: project_id.clone(),
            }),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Stage {
    Downloading,
    Copying,
    Ended(JobState),
}

/// One job the engine runs apart from any client: an add of an existing folder, a move of a
/// registered project into the projects home, a Delete everything or a New Project. An add or
/// a move downloads what is only in iCloud, then moves or copies the folder into the projects
/// home, reporting progress as files land and are copied, and stops between files when asked.
/// A delete waits for the project's agents, then moves its folders to the Trash one by one and
/// stops only before the first. A New Project builds its folder, counting its documents, and
/// stops only before its record.
#[derive(Debug)]
pub struct IntakeJob {
    id: RequestId,
    kind: JobKind,
    project: Option<ProjectId>,
    source: String,
    mode: AddMode,
    overrides: Vec<AddOverride>,
    download: Arc<Download>,
    tally: Arc<Tally>,
    stage: Mutex<Stage>,
}

impl IntakeJob {
    /// A job not yet started. Inputs: the change's request id, the kind, the project a move, a
    /// delete or a New Project takes (none for an add), the folder text (a delete's or a New
    /// Project's name), the mode and the overrides, which only an add or a move has. Output:
    /// the job, downloading.
    pub fn new(
        id: RequestId,
        kind: JobKind,
        project: Option<ProjectId>,
        source: String,
        mode: AddMode,
        overrides: Vec<AddOverride>,
    ) -> Self {
        assert_eq!(kind != JobKind::Add, project.is_some());
        assert!(!source.is_empty());
        assert!(overrides.len() <= ADD_OVERRIDES_MAX);
        assert!(matches!(kind, JobKind::Add | JobKind::MoveProject) || overrides.is_empty());
        Self {
            id,
            kind,
            project,
            source,
            mode,
            overrides,
            download: Arc::new(Download::default()),
            tally: Arc::new(Tally::default()),
            stage: Mutex::new(Stage::Downloading),
        }
    }

    /// A job an engine stop cut short, as the engine start found it on disk. Inputs: as
    /// [`IntakeJob::new`] without overrides, and the code it ended with: `done`,
    /// `home_move_source_left` (landed, part of the old folder left),
    /// `move_project_old_folder_left` (a project moved, its old folder left), both read as
    /// failed while clearing, or another failure, read as failed while copying. Output: the
    /// ended job.
    pub fn restored(
        id: RequestId,
        kind: JobKind,
        project: Option<ProjectId>,
        source: String,
        mode: AddMode,
        ended: &str,
    ) -> Self {
        assert!(!ended.is_empty());
        let job = Self::new(id, kind, project, source, mode, Vec::new());
        let state = match ended {
            "done" => JobState::Done,
            "home_move_source_left" | "move_project_old_folder_left" => JobState::Failed {
                code: ended.to_owned(),
                phase: Phase::Clearing,
            },
            code => JobState::Failed {
                code: code.to_owned(),
                phase: Phase::Copying,
            },
        };
        *job.stage.lock().unwrap_or_else(PoisonError::into_inner) = Stage::Ended(state);
        assert!(job.ended());
        job
    }

    /// A Delete everything an engine stop cut short, as the engine start found its mark: its
    /// request, the project and its name, and how many of its folders reached the Trash.
    /// Inputs: those. Output: the ended job, which waits for the owner to finish the delete
    /// from the remove ask or keep the project.
    pub fn cut_short(
        id: RequestId,
        project: ProjectId,
        name: String,
        trashed: u64,
        total: u64,
    ) -> Self {
        assert!(trashed <= total);
        let job = Self::new(
            id,
            JobKind::Delete,
            Some(project),
            name,
            AddMode::Move,
            Vec::new(),
        );
        let state = JobState::CutShort { trashed, total };
        *job.stage.lock().unwrap_or_else(PoisonError::into_inner) = Stage::Ended(state);
        assert!(job.ended());
        job
    }

    /// A New Project an engine stop cut short, as the engine start found its mark: its
    /// request, the project and its name. Inputs: those. Output: the ended job, failed while
    /// building with the interrupted code; the owner starts it again from New Project.
    pub fn setup_interrupted(id: RequestId, project: ProjectId, name: String) -> Self {
        let job = Self::new(
            id,
            JobKind::Setup,
            Some(project),
            name,
            AddMode::Move,
            Vec::new(),
        );
        let state = JobState::Failed {
            code: job.interrupted().to_owned(),
            phase: Phase::Building,
        };
        *job.stage.lock().unwrap_or_else(PoisonError::into_inner) = Stage::Ended(state);
        assert!(job.ended());
        job
    }

    /// The change's request id, which names the job.
    pub fn id(&self) -> &RequestId {
        &self.id
    }

    /// What the job does.
    pub fn kind(&self) -> JobKind {
        self.kind
    }

    /// The project a move, a delete or a New Project takes, or None for an add.
    pub fn project(&self) -> Option<&ProjectId> {
        self.project.as_ref()
    }

    /// The code a job its task left before it ended gets: its kind's interrupted code.
    pub fn interrupted(&self) -> &'static str {
        match self.kind {
            JobKind::Add => "home_add_interrupted",
            JobKind::MoveProject => "move_project_interrupted",
            JobKind::Delete => "project_delete_interrupted",
            JobKind::Setup => "home_new_project_interrupted",
        }
    }

    /// The folder being added or moved: as the owner gave it for an add, the project's real
    /// folder for a move. A delete's or a New Project's is the project's name.
    pub fn source(&self) -> &str {
        &self.source
    }

    /// How the folder comes into the home.
    pub fn mode(&self) -> AddMode {
        self.mode
    }

    /// The soft refusals the owner chose to go past.
    pub fn overrides(&self) -> &[AddOverride] {
        &self.overrides
    }

    /// The job's download, to claim the engine's download slot with.
    pub fn download(&self) -> Arc<Download> {
        Arc::clone(&self.download)
    }

    /// The tally the job's add reports into and obeys, to hand to the thread that copies.
    pub fn tally(&self) -> Arc<Tally> {
        Arc::clone(&self.tally)
    }

    /// Marks the download over and the add started.
    pub fn copying(&self) {
        let mut stage = self.stage.lock().unwrap_or_else(PoisonError::into_inner);
        assert_eq!(
            *stage,
            Stage::Downloading,
            "the add starts once, after the download"
        );
        *stage = Stage::Copying;
    }

    /// Asks the job to stop: the download at its next check, a move's or a delete's wait for
    /// its agents at its next poll, the copy between entries, a New Project between its build
    /// steps. Clearing the old folder never stops, since the copy it follows is complete, a
    /// delete that committed to its first Trash move refuses the stop, and so does a New
    /// Project whose record started. Output: whether the stop was taken.
    pub fn stop(&self) -> bool {
        if !self.tally.stop() {
            assert!(self.tally.trashing().is_some() || self.tally.recording());
            return false;
        }
        self.download.cancel();
        assert!(self.tally.stopped());
        true
    }

    /// Records how the job ended. Input: the result of the download or the add.
    pub fn end(&self, result: Result<(), EngineError>) {
        let mut stage = self.stage.lock().unwrap_or_else(PoisonError::into_inner);
        let phase = match *stage {
            Stage::Downloading | Stage::Copying if self.kind == JobKind::Setup => Phase::Building,
            Stage::Downloading => Phase::Downloading,
            Stage::Copying if self.tally.clearing() => Phase::Clearing,
            Stage::Copying if self.tally.trashing().is_some() => Phase::Trashing,
            Stage::Copying if self.tally.waiting() || self.kind == JobKind::Delete => {
                Phase::Stopping
            }
            Stage::Copying => Phase::Copying,
            Stage::Ended(_) => panic!("a job ends once"),
        };
        *stage = Stage::Ended(ended(phase, self.tally.stopped(), result));
        assert!(matches!(*stage, Stage::Ended(_)));
    }

    /// Whether the job has ended.
    pub fn ended(&self) -> bool {
        let stage = self.stage.lock().unwrap_or_else(PoisonError::into_inner);
        matches!(*stage, Stage::Ended(_))
    }

    /// The job as a client sees it now.
    pub fn view(&self) -> JobView {
        let stage = self.stage.lock().unwrap_or_else(PoisonError::into_inner);
        let state = match &*stage {
            Stage::Downloading | Stage::Copying if self.kind == JobKind::Setup => {
                let (done, total) = self.tally.building();
                JobState::Building { done, total }
            }
            Stage::Downloading => JobState::Downloading {
                progress: self.download.progress(),
            },
            Stage::Copying if self.tally.clearing() => JobState::Clearing,
            Stage::Copying => match (self.tally.waiting_for(), self.tally.trashing()) {
                (_, Some((done, total))) => JobState::Trashing { done, total },
                (Some(agents_left), None) => JobState::Stopping { agents_left },
                (None, None) => JobState::Copying {
                    copied: self.tally.copied(),
                    checked: self.tally.checked(),
                },
            },
            Stage::Ended(state) => state.clone(),
        };
        assert!(!matches!(state, JobState::Stopping { agents_left: 0 }));
        let view = JobView {
            id: self.id.clone(),
            kind: self.kind,
            project_id: self.project.clone(),
            source: self.source.clone(),
            mode: self.mode,
            overrides: self.overrides.clone(),
            stopping: self.tally.stopped(),
            state,
        };
        assert!(view.overrides.len() <= ADD_OVERRIDES_MAX);
        assert_eq!(view.kind != JobKind::Add, view.project_id.is_some());
        assert!(
            view.kind == JobKind::Delete
                || !matches!(
                    view.state,
                    JobState::Trashing { .. } | JobState::CutShort { .. }
                )
        );
        assert!(view.kind == JobKind::Setup || !matches!(view.state, JobState::Building { .. }));
        assert!(
            view.kind != JobKind::Setup
                || matches!(
                    view.state,
                    JobState::Building { .. }
                        | JobState::Done
                        | JobState::Failed { .. }
                        | JobState::Cancelled { .. }
                )
        );
        view
    }
}

/// How a job that reached `phase` ended. A landed folder is done even after a stop; the
/// copy's own `home_add_cancelled`, or any failed download after a stop, is a cancel; a
/// soft refusal waits for the owner's answer. Inputs: the phase, whether a stop was asked
/// and the result. Output: the end state.
fn ended(phase: Phase, stopped: bool, result: Result<(), EngineError>) -> JobState {
    let code = match result {
        Ok(()) => return JobState::Done,
        Err(e) => e.0,
    };
    assert!(!code.is_empty());
    let cancelled = code == "home_add_cancelled" || (stopped && phase == Phase::Downloading);
    if cancelled {
        assert!(phase != Phase::Clearing, "clearing never stops");
        assert!(
            phase != Phase::Trashing,
            "a delete stops only before its first Trash move"
        );
        return JobState::Cancelled { phase };
    }
    if NEEDS_ANSWER.contains(&code.as_str()) {
        return JobState::NeedsAnswer { code };
    }
    JobState::Failed { code, phase }
}

#[cfg(test)]
mod tests;
