use super::intake::copy;
use crate::projects_home::AddMode;
use crate::projects_home::job::{JobState, JobView, Phase};

/// What the TUI offers for the engine's intake job: stop it, start it again, answer its soft
/// refusal in Add folder, or forget it once it ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::tui) enum JobAction {
    Stop,
    Retry,
    Answer,
    Dismiss,
}

/// Whether the intake job is still at work: downloading, copying or clearing.
/// Takes the job. Returns true while it runs.
pub(in crate::tui) fn running(job: &JobView) -> bool {
    let _ = job;
    false
}

/// What the intake job does now or how it ended, with the folder's name and the counts as
/// files land, in the desktop's words. Takes the job. Returns one sentence.
pub(in crate::tui) fn line(job: &JobView) -> String {
    let _ = (job, copy, AddMode::Move, Phase::Copying);
    String::new()
}

/// What the TUI offers for the job: Stop while it downloads or copies, nothing while it
/// clears, Retry only when the folder did not land, Answer for a soft refusal, Dismiss once
/// it ended. Takes the job. Returns the actions, the one Enter does first.
pub(in crate::tui) fn actions(job: &JobView) -> Vec<JobAction> {
    let _ = job;
    Vec::new()
}

/// The sentence for a job that just ended, said once: only when the read before showed the
/// same job running. Takes the read before and the read now. Returns the job's line, or None.
pub(in crate::tui) fn end_line(before: Option<&JobView>, after: Option<&JobView>) -> Option<String> {
    let _ = (before, after, JobState::Done);
    None
}
