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

/// Whether the job is still at work: downloading, waiting for agents to stop, copying,
/// moving folders to the Trash, clearing or building a New Project. Takes the job. Returns
/// true while it runs.
pub(in crate::tui) fn running(job: &JobView) -> bool {
    matches!(
        job.state,
        JobState::Downloading { .. }
            | JobState::Stopping { .. }
            | JobState::Copying { .. }
            | JobState::Trashing { .. }
            | JobState::Clearing
            | JobState::Building { .. }
    )
}

/// The verb a job's kind is named by while it runs. Takes the kind. Returns the verb.
fn verb(kind: JobKind) -> &'static str {
    match kind {
        JobKind::Add => "Adding",
        JobKind::MoveProject => "Moving",
        JobKind::Delete => "Deleting",
        JobKind::Setup => "Setting up",
    }
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
            format!(
                "{} {} is running. Wait for it to finish on Home.",
                verb(job.kind),
                name(job)
            )
        }
        None => copy("job_busy"),
    };
    assert!(line.ends_with("Wait for it to finish on Home."));
    line
}

/// The line for a refused job request: a busy refusal names the job that runs, any other
/// refusal says its own sentence, and a code the engine sent empty or unknown says the plain
/// fallback. Takes the code and the job read. Returns the line.
pub(in crate::tui) fn refusal(code: &str, held: Option<&JobView>) -> String {
    match code {
        "job_busy" => busy(held),
        code => copy(code),
    }
}

/// The folder's own name, or the whole path when it has none; a delete's or a New Project's
/// project name whole.
pub(in crate::tui) fn name(job: &JobView) -> String {
    if matches!(job.kind, JobKind::Delete | JobKind::Setup) {
        return job.source.clone();
    }
    let path = std::path::Path::new(&job.source);
    let name = path.file_name().map_or_else(
        || job.source.clone(),
        |name| name.to_string_lossy().into_owned(),
    );
    assert!(!name.is_empty() || job.source.is_empty());
    name
}

/// What the job does now or how it ended, with the folder's name and the counts as files
/// land, in the desktop's words; a count the engine sent past its total is left out, as it
/// reads as no count. Takes the job. Returns one sentence.
pub(in crate::tui) fn line(job: &JobView) -> String {
    let name = name(job);
    if running(job) && job.stopping {
        return format!("Stopping {name}...");
    }
    if job.kind == JobKind::Delete {
        return delete_line(job, &name);
    }
    if job.kind == JobKind::Setup {
        return setup_line(job, &name);
    }
    let (verb, done) = match job.mode {
        AddMode::Move => ("Moving", "moved"),
        AddMode::Copy => ("Copying", "copied"),
    };
    match &job.state {
        JobState::Downloading { progress } => {
            if progress.total == 0 || progress.done > progress.total {
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
        JobState::Trashing { .. } | JobState::CutShort { .. } => delete_line(job, &name),
        JobState::Building { .. } => setup_line(job, &name),
    }
}

/// What a New Project does now or how it ended, with the project's name and how many of its
/// documents it copied. Takes the New Project and its name. Returns one sentence.
fn setup_line(job: &JobView, name: &str) -> String {
    match &job.state {
        JobState::Building { done, total } if *total > 0 && done <= total => {
            let noun = if *total == 1 { "document" } else { "documents" };
            format!("Setting up {name}: {done} of {total} {noun} copied")
        }
        JobState::Done => format!("{name} is set up in your projects folder."),
        JobState::Cancelled { .. } => {
            format!("Setting up {name} stopped. Nothing was kept.")
        }
        JobState::Failed { code, .. } => format!("{name}: {}", copy(code)),
        JobState::Building { .. }
        | JobState::Downloading { .. }
        | JobState::Stopping { .. }
        | JobState::Copying { .. }
        | JobState::Clearing
        | JobState::Trashing { .. }
        | JobState::NeedsAnswer { .. }
        | JobState::CutShort { .. } => format!("Setting up {name}..."),
    }
}

/// What a delete does now or how it ended, with the project's name and how many of its
/// folders reached the Trash, in the desktop's words. Takes the delete and its name. Returns
/// one sentence.
fn delete_line(job: &JobView, name: &str) -> String {
    let folders = |total: u64| if total == 1 { "folder" } else { "folders" };
    match &job.state {
        JobState::Stopping { agents_left } => {
            format!("Waiting for {} of {name} to stop...", agents(*agents_left))
        }
        JobState::Trashing { done, total } if done <= total => {
            format!(
                "Moving {name} to the Trash: {done} of {total} {}",
                folders(*total)
            )
        }
        JobState::Trashing { .. } => format!("Moving {name} to the Trash..."),
        JobState::Clearing => format!("Removing {name} from CrazyAgent..."),
        JobState::Done => format!("{name} is deleted. Its files are in the Trash."),
        JobState::Cancelled { .. } => {
            format!("Deleting {name} stopped. Nothing was moved to the Trash.")
        }
        JobState::Failed { code, .. } => format!("{name}: {}", copy(code)),
        JobState::CutShort { trashed, total } if trashed <= total => {
            format!(
                "Deleting {name} was cut short with {trashed} of {total} {} in the Trash. Finish delete or keep the project.",
                folders(*total)
            )
        }
        JobState::CutShort { .. } => {
            format!("Deleting {name} was cut short. Finish delete or keep the project.")
        }
        JobState::Downloading { .. }
        | JobState::Copying { .. }
        | JobState::NeedsAnswer { .. }
        | JobState::Building { .. } => format!("Deleting {name}..."),
    }
}

/// What the TUI offers for the job: Stop while it downloads, waits for agents or copies,
/// nothing while it clears or a delete moves folders to the Trash, Retry only when the folder
/// did not land (Finish delete for a delete cut short), Answer for a soft refusal, Dismiss once
/// it ended. Takes the job. Returns the actions, the one Enter does first.
pub(in crate::tui) fn actions(job: &JobView) -> Vec<JobAction> {
    let offered = match &job.state {
        JobState::Downloading { .. }
        | JobState::Stopping { .. }
        | JobState::Copying { .. }
        | JobState::Building { .. }
            if job.stopping =>
        {
            vec![]
        }
        JobState::Downloading { .. }
        | JobState::Stopping { .. }
        | JobState::Copying { .. }
        | JobState::Building { .. } => vec![JobAction::Stop],
        JobState::Clearing | JobState::Trashing { .. } => vec![],
        JobState::Done => vec![JobAction::Dismiss],
        JobState::Failed {
            phase: Phase::Clearing,
            ..
        } => vec![JobAction::Dismiss],
        JobState::Failed { .. } | JobState::Cancelled { .. } | JobState::CutShort { .. } => {
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
/// with the folder's name cut short so the crumbs keep their place; a count past its total
/// is left out. Takes a running job and the ellipsis. Returns the words.
pub(in crate::tui) fn brief(job: &JobView, ellipsis: &str) -> String {
    assert!(running(job), "an ended job has no brief: {:?}", job.state);
    let name = cut(&name(job), BRIEF_NAME_CELLS_MAX, ellipsis);
    let verb = verb(job.kind);
    let brief = match &job.state {
        _ if job.stopping => format!("Stopping {name}..."),
        JobState::Downloading { progress }
            if progress.total > 0 && progress.done <= progress.total =>
        {
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
        JobState::Copying { .. } if job.kind == JobKind::Delete => format!("{verb} {name}..."),
        JobState::Copying { copied, .. } if *copied > 0 => format!("{verb} {name}: {copied} files"),
        JobState::Copying { .. } => format!("{verb} {name}..."),
        JobState::Trashing { done, total } if done <= total => {
            format!("{verb} {name}: {done} of {total}")
        }
        JobState::Trashing { .. } => format!("{verb} {name}..."),
        JobState::Building { done, total } if *total > 0 && done <= total => {
            format!("{verb} {name}: {done} of {total}")
        }
        JobState::Building { .. } => format!("{verb} {name}..."),
        _ => format!("Finishing {name}..."),
    };
    assert!(brief.contains(&name));
    brief
}

/// The word Home's job card shows beside the folder's name, in the desktop's words, with the
/// status kind that colors it. Takes the job. Returns the kind and the word.
pub(in crate::tui) fn badge(job: &JobView) -> (Status, &'static str) {
    let moving = job.kind == JobKind::MoveProject;
    let deleting = job.kind == JobKind::Delete;
    let building = job.kind == JobKind::Setup;
    let badge = match &job.state {
        JobState::Building { .. } => (Status::Active, "Setting up"),
        JobState::Downloading { .. } => (Status::Active, "Downloading"),
        JobState::Stopping { .. } => (Status::Active, "Waiting"),
        JobState::Copying { .. } | JobState::Trashing { .. } if deleting => {
            (Status::Active, "Deleting")
        }
        JobState::Copying { .. } if moving => (Status::Active, "Moving"),
        JobState::Copying { .. } => (Status::Active, "Adding"),
        JobState::Clearing | JobState::Trashing { .. } => (Status::Active, "Finishing"),
        JobState::Done if deleting => (Status::Ok, "Deleted"),
        JobState::Done if moving => (Status::Ok, "Moved"),
        JobState::Done if building => (Status::Ok, "Set up"),
        JobState::Done => (Status::Ok, "Added"),
        JobState::CutShort { .. } => (Status::Needs, "Cut short"),
        JobState::Failed { .. } => (Status::Fail, "Failed"),
        JobState::Cancelled { .. } => (Status::Off, "Stopped"),
        JobState::NeedsAnswer { .. } => (Status::Needs, "Needs answer"),
    };
    assert_eq!(running(job), badge.0 == Status::Active);
    badge
}

/// What Enter and d do on Home's job card, as the card's last line. Takes the job. Returns
/// the line; a job that offers nothing yet asks the owner to wait, and a delete moving its
/// folders to the Trash says it can't stop now.
pub(in crate::tui) fn next(job: &JobView) -> &'static str {
    let next = match actions(job).as_slice() {
        [] if matches!(job.state, JobState::Trashing { .. }) && !job.stopping => {
            "It can't stop now. Wait for it to finish."
        }
        [] => "Wait for it to finish.",
        [JobAction::Retry, JobAction::Dismiss]
            if matches!(job.state, JobState::CutShort { .. }) =>
        {
            "Enter finishes the delete. d keeps the project."
        }
        [JobAction::Stop] => "Enter stops it.",
        [JobAction::Dismiss] => "Enter dismisses it.",
        [JobAction::Retry, JobAction::Dismiss] => "Enter tries again. d dismisses it.",
        [JobAction::Answer, JobAction::Dismiss] => "Enter answers. d dismisses it.",
        other => panic!("no job offers {other:?}"),
    };
    assert!(next.ends_with('.'));
    next
}

/// The hint word for one job action, the desktop's button word: Finish delete and Keep the
/// project for a delete cut short. Takes the job and the action. Returns the word.
pub(in crate::tui) fn label(job: &JobView, action: JobAction) -> &'static str {
    let cut = matches!(job.state, JobState::CutShort { .. });
    match action {
        JobAction::Retry if cut => "Finish delete",
        JobAction::Dismiss if cut => "Keep the project",
        JobAction::Stop => "Stop",
        JobAction::Retry => "Try again",
        JobAction::Answer => "Answer",
        JobAction::Dismiss => "Dismiss",
    }
}
