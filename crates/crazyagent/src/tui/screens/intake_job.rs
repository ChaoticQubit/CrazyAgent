use super::intake::copy;
use crate::projects_home::AddMode;
use crate::projects_home::job::{JobState, JobView, Phase};
use crate::tui::glyphs::Status;
use crate::tui::text::cut;

const BRIEF_NAME_CELLS_MAX: usize = 16;

#[cfg(test)]
mod tests;

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
    matches!(
        job.state,
        JobState::Downloading { .. } | JobState::Copying { .. } | JobState::Clearing
    )
}

/// The folder's own name, or the whole path when it has none.
pub(in crate::tui) fn name(job: &JobView) -> String {
    let path = std::path::Path::new(&job.source);
    let name = path.file_name().map_or_else(
        || job.source.clone(),
        |name| name.to_string_lossy().into_owned(),
    );
    assert!(!name.is_empty() || job.source.is_empty());
    name
}

/// What the intake job does now or how it ended, with the folder's name and the counts as
/// files land, in the desktop's words. Takes the job. Returns one sentence.
pub(in crate::tui) fn line(job: &JobView) -> String {
    let name = name(job);
    if running(job) && job.stopping {
        return format!("Stopping {name}...");
    }
    let (verb, done) = match job.mode {
        AddMode::Move => ("Moving", "moved"),
        AddMode::Copy => ("Copying", "copied"),
    };
    match &job.state {
        JobState::Downloading { progress } => {
            assert!(progress.done <= progress.total);
            if progress.total == 0 {
                format!("Downloading {name} from iCloud...")
            } else {
                let (done, total) = (progress.done, progress.total);
                format!("Downloading {name} from iCloud: {done} of {total} files")
            }
        }
        JobState::Copying { checked, .. } if *checked > 0 => {
            format!("Checking the copy of {name}: {checked} files checked")
        }
        JobState::Copying { copied, .. } if *copied > 0 => {
            format!("{verb} {name}: {copied} files copied")
        }
        JobState::Copying { .. } => format!("{verb} {name}..."),
        JobState::Clearing => format!("Removing {name} from its old place..."),
        JobState::Done => format!("{name} is in your projects folder. Set it up from Home."),
        JobState::Cancelled { .. } => {
            format!("Adding {name} stopped. Nothing was moved or copied.")
        }
        JobState::Failed { code, .. } => format!("{name}: {}", copy(code)),
        JobState::NeedsAnswer { .. } => {
            format!("{name} needs your answer before it is {done}.")
        }
    }
}

/// What the TUI offers for the job: Stop while it downloads or copies, nothing while it
/// clears, Retry only when the folder did not land, Answer for a soft refusal, Dismiss once
/// it ended. Takes the job. Returns the actions, the one Enter does first.
pub(in crate::tui) fn actions(job: &JobView) -> Vec<JobAction> {
    let offered = match &job.state {
        JobState::Downloading { .. } | JobState::Copying { .. } if job.stopping => vec![],
        JobState::Downloading { .. } | JobState::Copying { .. } => vec![JobAction::Stop],
        JobState::Clearing => vec![],
        JobState::Done => vec![JobAction::Dismiss],
        JobState::Failed {
            phase: Phase::Clearing,
            ..
        } => vec![JobAction::Dismiss],
        JobState::Failed { .. } | JobState::Cancelled { .. } => {
            vec![JobAction::Retry, JobAction::Dismiss]
        }
        JobState::NeedsAnswer { .. } => vec![JobAction::Answer, JobAction::Dismiss],
    };
    assert!(running(job) || offered.contains(&JobAction::Dismiss));
    assert!(!running(job) || !offered.contains(&JobAction::Dismiss));
    offered
}

/// The sentence for a job that just ended, said once: only when the read before showed the
/// same job running. Takes the read before and the read now. Returns the job's line, or None.
pub(in crate::tui) fn end_line(
    before: Option<&JobView>,
    after: Option<&JobView>,
) -> Option<String> {
    let (before, after) = (before?, after?);
    let ended = before.id == after.id && running(before) && !running(after);
    let said = ended.then(|| line(after));
    assert!(said.as_ref().is_none_or(|text| !text.is_empty()));
    said
}

/// The running job's few words for the header on every screen: what it does and its count,
/// with the folder's name cut short so the crumbs keep their place. Takes a running job and
/// the ellipsis. Returns the words.
pub(in crate::tui) fn brief(job: &JobView, ellipsis: &str) -> String {
    assert!(running(job), "an ended job has no brief: {:?}", job.state);
    let name = cut(&name(job), BRIEF_NAME_CELLS_MAX, ellipsis);
    let brief = match &job.state {
        _ if job.stopping => format!("Stopping {name}..."),
        JobState::Downloading { progress } if progress.total > 0 => {
            format!(
                "Downloading {name}: {} of {}",
                progress.done, progress.total
            )
        }
        JobState::Downloading { .. } => format!("Downloading {name}..."),
        JobState::Copying { checked, .. } if *checked > 0 => {
            format!("Checking {name}: {checked} files")
        }
        JobState::Copying { copied, .. } if *copied > 0 => format!("Adding {name}: {copied} files"),
        JobState::Copying { .. } => format!("Adding {name}..."),
        _ => format!("Finishing {name}..."),
    };
    assert!(brief.contains(&name));
    brief
}

/// The word Home's job card shows beside the folder's name, in the desktop's words, with the
/// status kind that colors it. Takes the job. Returns the kind and the word.
pub(in crate::tui) fn badge(job: &JobView) -> (Status, &'static str) {
    let badge = match &job.state {
        JobState::Downloading { .. } => (Status::Active, "Downloading"),
        JobState::Copying { .. } => (Status::Active, "Adding"),
        JobState::Clearing => (Status::Active, "Finishing"),
        JobState::Done => (Status::Ok, "Added"),
        JobState::Failed { .. } => (Status::Fail, "Failed"),
        JobState::Cancelled { .. } => (Status::Off, "Stopped"),
        JobState::NeedsAnswer { .. } => (Status::Needs, "Needs answer"),
    };
    assert_eq!(running(job), badge.0 == Status::Active);
    badge
}

/// What Enter and d do on Home's job card, as the card's last line. Takes the job. Returns
/// the line; a job that offers nothing yet asks the owner to wait.
pub(in crate::tui) fn next(job: &JobView) -> &'static str {
    let next = match actions(job).as_slice() {
        [] => "Wait for it to finish.",
        [JobAction::Stop] => "Enter stops it.",
        [JobAction::Dismiss] => "Enter dismisses it.",
        [JobAction::Retry, JobAction::Dismiss] => "Enter tries again. d dismisses it.",
        [JobAction::Answer, JobAction::Dismiss] => "Enter answers. d dismisses it.",
        other => panic!("no intake job offers {other:?}"),
    };
    assert!(next.ends_with('.'));
    next
}

/// The hint word for one job action, the desktop's button word. Takes the action. Returns
/// the word.
pub(in crate::tui) fn label(action: JobAction) -> &'static str {
    match action {
        JobAction::Stop => "Stop",
        JobAction::Retry => "Try again",
        JobAction::Answer => "Answer",
        JobAction::Dismiss => "Dismiss",
    }
}
