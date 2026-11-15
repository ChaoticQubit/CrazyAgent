use super::intake::copy;
use crate::projects_home::AddMode;
use crate::projects_home::job::{JobKind, JobState, JobView, Phase};
use crate::tui::glyphs::Status;
use crate::tui::text::cut;

const BRIEF_NAME_CELLS_MAX: usize = 16;

#[cfg(test)]
mod tests;

/// What the TUI offers for the engine's job: stop it, start it again, answer its soft refusal,
/// or forget it once it ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::tui) enum JobAction {
    Stop,
    Retry,
    Answer,
    Dismiss,
}

/// Whether the job is still at work: downloading, waiting for agents to stop, copying or
/// clearing. Takes the job. Returns true while it runs.
pub(in crate::tui) fn running(job: &JobView) -> bool {
    matches!(
        job.state,
        JobState::Downloading { .. }
            | JobState::Stopping { .. }
            | JobState::Copying { .. }
            | JobState::Clearing
    )
}

/// The few words for a count of agents still stopping. Takes the count. Returns the words.
fn agents(left: u64) -> String {
    match left {
        1 => "1 agent".to_owned(),
        left => format!("{left} agents"),
    }
}

/// The line for a start the engine refused `job_busy`: names the job that runs, as the client
/// last read it, or says another job runs when that read found none running. Takes the job
/// read. Returns the line.
pub(in crate::tui) fn busy(held: Option<&JobView>) -> String {
    let line = match held.filter(|job| running(job)) {
        Some(job) => {
            let verb = match job.kind {
                JobKind::Add => "Adding",
                JobKind::MoveProject => "Moving",
            };
            format!(
                "{verb} {} is running. Wait for it to finish on Home.",
                name(job)
            )
        }
        None => copy("job_busy"),
    };
    assert!(line.ends_with("Wait for it to finish on Home."));
    line
}

/// The line for a refused job request: a busy refusal names the job that runs, any other
/// refusal says its own sentence. Takes the code and the job read. Returns the line.
pub(in crate::tui) fn refusal(code: &str, held: Option<&JobView>) -> String {
    assert!(!code.is_empty());
    match code {
        "job_busy" => busy(held),
        code => copy(code),
    }
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

/// What the job does now or how it ended, with the folder's name and the counts as files
/// land, in the desktop's words. Takes the job. Returns one sentence.
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
        JobState::Stopping { agents_left } => {
            format!("Waiting for {} of {name} to stop...", agents(*agents_left))
        }
        JobState::Copying { checked, .. } if *checked > 0 => {
            format!("Checking the copy of {name}: {checked} files checked")
        }
        JobState::Copying { copied, .. } if *copied > 0 => {
            format!("{verb} {name}: {copied} files copied")
        }
        JobState::Copying { .. } => format!("{verb} {name}..."),
        JobState::Clearing => format!("Removing {name} from its old place..."),
        JobState::Done if job.kind == JobKind::MoveProject => {
            format!("{name} is now in your projects folder. Start the team again from Team.")
        }
        JobState::Done => format!("{name} is in your projects folder. Set it up from Home."),
        JobState::Cancelled { .. } if job.kind == JobKind::MoveProject => {
            format!("Moving {name} stopped. Nothing was moved or copied.")
        }
        JobState::Cancelled { .. } => {
            format!("Adding {name} stopped. Nothing was moved or copied.")
        }
        JobState::Failed { code, .. } => format!("{name}: {}", copy(code)),
        JobState::NeedsAnswer { .. } => {
            format!("{name} needs your answer before it is {done}.")
        }
    }
}

/// What the TUI offers for the job: Stop while it downloads, waits for agents or copies,
/// nothing while it clears, Retry only when the folder did not land, Answer for a soft
/// refusal, Dismiss once it ended. Takes the job. Returns the actions, the one Enter does
/// first.
pub(in crate::tui) fn actions(job: &JobView) -> Vec<JobAction> {
    let offered = match &job.state {
        JobState::Downloading { .. } | JobState::Stopping { .. } | JobState::Copying { .. }
            if job.stopping =>
        {
            vec![]
        }
        JobState::Downloading { .. } | JobState::Stopping { .. } | JobState::Copying { .. } => {
            vec![JobAction::Stop]
        }
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
    let verb = match job.kind {
        JobKind::Add => "Adding",
        JobKind::MoveProject => "Moving",
    };
    let brief = match &job.state {
        _ if job.stopping => format!("Stopping {name}..."),
        JobState::Downloading { progress } if progress.total > 0 => {
            format!(
                "Downloading {name}: {} of {}",
                progress.done, progress.total
            )
        }
        JobState::Downloading { .. } => format!("Downloading {name}..."),
        JobState::Stopping { agents_left } => {
            format!("Stopping {name}: {}", agents(*agents_left))
        }
        JobState::Copying { checked, .. } if *checked > 0 => {
            format!("Checking {name}: {checked} files")
        }
        JobState::Copying { copied, .. } if *copied > 0 => format!("{verb} {name}: {copied} files"),
        JobState::Copying { .. } => format!("{verb} {name}..."),
        _ => format!("Finishing {name}..."),
    };
    assert!(brief.contains(&name));
    brief
}

/// The word Home's job card shows beside the folder's name, in the desktop's words, with the
/// status kind that colors it. Takes the job. Returns the kind and the word.
pub(in crate::tui) fn badge(job: &JobView) -> (Status, &'static str) {
    let moving = job.kind == JobKind::MoveProject;
    let badge = match &job.state {
        JobState::Downloading { .. } => (Status::Active, "Downloading"),
        JobState::Stopping { .. } => (Status::Active, "Waiting"),
        JobState::Copying { .. } if moving => (Status::Active, "Moving"),
        JobState::Copying { .. } => (Status::Active, "Adding"),
        JobState::Clearing => (Status::Active, "Finishing"),
        JobState::Done if moving => (Status::Ok, "Moved"),
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
        other => panic!("no job offers {other:?}"),
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
