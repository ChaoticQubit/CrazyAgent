use super::icloud::{Download, Progress};
use super::{ADD_OVERRIDES_MAX, AddMode, AddOverride, Tally};
use crate::engine::EngineError;
use crate::protocol::RequestId;
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex, PoisonError};

/// The soft refusals of an add the owner may go past with an override: a job that ends on
/// one waits for the owner's answer instead of failing.
pub const NEEDS_ANSWER: [&str; 3] = [
    "home_move_worktrees",
    "home_copy_linked_checkout",
    "home_copy_too_large",
];

/// The part of an intake job that was running.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Downloading,
    Copying,
    Clearing,
}

/// What an intake job is doing, or how it ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum JobState {
    Downloading { progress: Progress },
    Copying { copied: u64, checked: u64 },
    Clearing,
    Done,
    Failed { code: String, phase: Phase },
    Cancelled { phase: Phase },
    NeedsAnswer { code: String },
}

/// One intake job as a client sees it: its id, which is the add's request id, the folder,
/// the mode, the soft refusals it goes past, whether a stop was asked and its state. A client
/// answering a soft refusal starts again with these overrides and the new one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JobView {
    pub id: RequestId,
    pub source: String,
    pub mode: AddMode,
    pub overrides: Vec<AddOverride>,
    pub stopping: bool,
    pub state: JobState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Stage {
    Downloading,
    Copying,
    Ended(JobState),
}

/// One add of an existing folder that the engine runs apart from any client: it downloads
/// what is only in iCloud, then moves or copies the folder into the projects home,
/// reporting progress as files land and are copied, and stops between files when asked.
#[derive(Debug)]
pub struct IntakeJob {
    id: RequestId,
    owner_id: String,
    source: String,
    mode: AddMode,
    overrides: Vec<AddOverride>,
    download: Arc<Download>,
    tally: Arc<Tally>,
    stage: Mutex<Stage>,
}

impl IntakeJob {
    /// A job not yet started. Inputs: the add's request id, the owner that asked, the
    /// folder text, the mode and the overrides. Output: the job, downloading.
    pub fn new(
        id: RequestId,
        owner_id: String,
        source: String,
        mode: AddMode,
        overrides: Vec<AddOverride>,
    ) -> Self {
        assert!(!owner_id.is_empty());
        assert!(!source.is_empty());
        assert!(overrides.len() <= ADD_OVERRIDES_MAX);
        Self {
            id,
            owner_id,
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
    /// `home_move_source_left` (landed, part of the old folder left) or another failure,
    /// read as failed while copying. Output: the ended job.
    pub fn restored(
        id: RequestId,
        owner_id: String,
        source: String,
        mode: AddMode,
        ended: &str,
    ) -> Self {
        assert!(!ended.is_empty());
        let job = Self::new(id, owner_id, source, mode, Vec::new());
        let state = match ended {
            "done" => JobState::Done,
            "home_move_source_left" => JobState::Failed {
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

    /// The add's request id, which names the job.
    pub fn id(&self) -> &RequestId {
        &self.id
    }

    /// The owner that started the job.
    pub fn owner_id(&self) -> &str {
        &self.owner_id
    }

    /// The folder being added, as the owner gave it.
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

    /// Asks the job to stop: the download at its next check, the copy between entries.
    /// Clearing the old folder never stops, since the copy it follows is complete.
    pub fn stop(&self) {
        self.download.cancel();
        self.tally.stop();
        assert!(self.tally.stopped());
    }

    /// Records how the job ended. Input: the result of the download or the add.
    pub fn end(&self, result: Result<(), EngineError>) {
        let mut stage = self.stage.lock().unwrap_or_else(PoisonError::into_inner);
        let phase = match *stage {
            Stage::Downloading => Phase::Downloading,
            Stage::Copying if self.tally.clearing() => Phase::Clearing,
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
            Stage::Downloading => JobState::Downloading {
                progress: self.download.progress(),
            },
            Stage::Copying if self.tally.clearing() => JobState::Clearing,
            Stage::Copying => JobState::Copying {
                copied: self.tally.copied(),
                checked: self.tally.checked(),
            },
            Stage::Ended(state) => state.clone(),
        };
        JobView {
            id: self.id.clone(),
            source: self.source.clone(),
            mode: self.mode,
            overrides: Vec::new(),
            stopping: self.tally.stopped(),
            state,
        }
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
        return JobState::Cancelled { phase };
    }
    if NEEDS_ANSWER.contains(&code.as_str()) {
        return JobState::NeedsAnswer { code };
    }
    JobState::Failed { code, phase }
}

#[cfg(test)]
mod tests;
