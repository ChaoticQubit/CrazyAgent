use super::*;
use crate::research::runner::{
    RUN_NAME_USED, ReadOnlyEnding, ReadOnlyRun, ResearchAccount, ResearchRunner, RunError,
};
use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// What a check says when its agent ran out of time.
pub const TIMED_OUT: &str = "The check ran out of time";
/// What a check says when the project's hidden folders hold tracked files or cannot be read.
pub const HIDDEN_UNREADABLE: &str = "The project's hidden folders could not be checked";

/// The engine log code of a finished run whose folder or profile could not be removed.
const RUN_LEFT: &str = "roadmap_status_run_left";
/// How often a running check asks the store whether it still holds its claim; the runner
/// polls its stop ten times as often.
const CLAIM_POLL: Duration = Duration::from_secs(1);

/// How long a finished run's folder stays before it is removed. The usage upkeep counts a
/// run's tokens from the records in its home once every `SCAN_EVERY`, so the home must
/// outlive the run by several passes or the last turn's tokens are never counted.
pub(crate) const RUN_KEPT: Duration = Duration::from_secs(60);

const _: () = assert!(RUN_KEPT.as_secs() >= 6 * crate::usage::service::SCAN_EVERY.as_secs());

/// What a finished status run left in the research state: its folder, which holds the
/// prompt with the evidence pack and the agent's home, its Seatbelt profile and when the
/// run ended. The research loop removes both once [`RunLeft::due`] says so.
#[derive(Debug)]
pub(crate) struct RunLeft {
    folder: PathBuf,
    profile: PathBuf,
    ended: Instant,
}

impl RunLeft {
    /// Names what one run left. Inputs: the run's folder, whose name starts with
    /// `RUN_NAME_HEAD`, its profile file and when the run ended. Output: the record.
    pub(crate) fn at(folder: PathBuf, profile: PathBuf, ended: Instant) -> Self {
        assert!(folder.is_absolute() && profile.is_absolute());
        assert!(
            folder
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with(RUN_NAME_HEAD))
        );
        Self {
            folder,
            profile,
            ended,
        }
    }

    /// Whether the run ended at least [`RUN_KEPT`] ago, so the usage upkeep has counted it.
    pub(crate) fn due(&self) -> bool {
        self.ended.elapsed() >= RUN_KEPT
    }

    /// Removes the folder and the profile. A part that is already gone is fine; one that
    /// cannot be removed is logged by count and stays until Delete everything.
    pub(crate) fn remove(self) {
        assert!(self.folder.is_absolute() && self.profile.is_absolute());
        let left = [
            std::fs::remove_dir_all(&self.folder),
            std::fs::remove_file(&self.profile),
        ]
        .into_iter()
        .filter_map(Result::err)
        .filter(|error| error.kind() != std::io::ErrorKind::NotFound)
        .count();
        if left > 0 {
            eprintln!("{RUN_LEFT} parts={left}");
        }
    }
}

/// The most entries of the research agents folder one start sweep reads.
#[cfg(unix)]
const SWEPT_MAX: usize = 4096;

/// Removes every status run folder, with its profile, that an engine before 2.4.1 left in
/// the research state: named [`RUN_NAME_HEAD`], the project id and the start time, with no
/// run-id hash, a shape no run of this engine makes and Delete everything no longer
/// matches (RV-DS241-45 r2). It runs at engine start, before any status run, and reads at
/// most [`SWEPT_MAX`] entries. Input: the absolute research state folder. Output: how many
/// runs it removed.
#[cfg(unix)]
pub(crate) fn sweep_unhashed(state: &Path) -> usize {
    assert!(state.is_absolute());
    let Ok(listing) = std::fs::read_dir(state.join("agents")) else {
        return 0;
    };
    let mut swept = 0;
    for entry in listing.take(SWEPT_MAX).flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if !name.starts_with(RUN_NAME_HEAD) || hashed(name) {
            continue;
        }
        let profile = state.join("profiles").join(format!("{name}.sb"));
        RunLeft::at(entry.path(), profile, Instant::now()).remove();
        swept += 1;
    }
    assert!(swept <= SWEPT_MAX);
    swept
}

/// Whether a status run name ends as this engine writes it: `-`, the run-id hash in
/// lowercase hex, `-` and the start time in plain digits. Input: the name. Output: the
/// answer.
#[cfg(unix)]
fn hashed(name: &str) -> bool {
    let Some((stem, time)) = name.rsplit_once('-') else {
        return false;
    };
    let Some((_, hash)) = stem.rsplit_once('-') else {
        return false;
    };
    (1..=RUN_TIME_DIGITS_MAX).contains(&time.len())
        && time.bytes().all(|byte| byte.is_ascii_digit())
        && hash.len() == 2 * RUN_ID_HASH_BYTES
        && hash
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// How a check ended without a status: with words for the owner, or too early to say, so it
/// runs again later.
pub(super) enum Unchecked {
    Failed(String),
    Later,
}

/// Runs one status check `tick` claimed to its end and stores how it ended: the item states,
/// the words of its failure, or a new try after `RETRY_MS` when the engine is stopping or
/// the account is busy. A result that cannot be stored leaves the run open; `tick` closes it
/// with `UNFINISHED` once its time is over. Inputs: the runner, the project, the claim the
/// run holds, the project's folder and the account to run on. Output: what the run left
/// in the research state for the caller to remove later, if it left anything.
pub(crate) fn check(
    runner: &ResearchRunner,
    project: &ProjectId,
    claim: Claim,
    root: &Path,
    account: &ResearchAccount,
) -> Option<RunLeft> {
    assert!(root.is_absolute());
    let db = runner
        .database()
        .parent()
        .and_then(|engine| crate::questions::open(engine).ok())?;
    let mut left = None;
    let outcome = answer(runner, &db, project, claim, root, account, &mut left);
    let now = crate::team::now_ms();
    let _ = match &outcome {
        Ok((states, source)) => finish(&db, project, claim, Ok((states.as_slice(), source)), now),
        Err(Unchecked::Failed(words)) => finish(&db, project, claim, Err(words), now),
        Err(Unchecked::Later) => requeue(&db, project, claim, now.saturating_add(RETRY_MS)),
    };
    left
}

/// Asks the status agent about one project: prepares the items and the evidence, runs the
/// agent with tools off on the project's account and reads its reply. The agent is stopped
/// once the run no longer holds its claim. Inputs: the runner, the engine connection, the
/// project, the claim, its folder, the account and where to put what the run left. Output:
/// the item states and the roadmap file they came from, or how the check ended without them.
pub(super) fn answer(
    runner: &ResearchRunner,
    db: &Connection,
    project: &ProjectId,
    claim: Claim,
    root: &Path,
    account: &ResearchAccount,
    left: &mut Option<RunLeft>,
) -> Result<(Vec<ItemState>, String), Unchecked> {
    assert!(left.is_none());
    if let Some(unreached) = runner.unreached(account) {
        return Err(Unchecked::Failed(unreached.line().to_owned()));
    }
    let failed = |words: &str| Unchecked::Failed(words.to_owned());
    let hidden = crate::sandbox::hidden_paths::load(db, project.as_str())
        .map_err(|_| failed(HIDDEN_UNREADABLE))?;
    crate::sandbox::hidden_paths::none_tracked(root, &hidden)
        .map_err(|_| failed(HIDDEN_UNREADABLE))?;
    let check = prepare(db, project, root).map_err(failed)?;
    let name = run_name(project, crate::team::now_ms());
    let run = ReadOnlyRun {
        name: &name,
        agent: "roadmap-status",
        project: Some(project),
        folder: root,
        prompt: &check.prompt,
        account,
        wall_time: AGENT_TIME,
        output_tokens: Some(OUTPUT_TOKENS),
    };
    let asked_at = Cell::new(Instant::now());
    let lost = Cell::new(false);
    let stop = || {
        if !lost.get() && asked_at.get().elapsed() >= CLAIM_POLL {
            asked_at.set(Instant::now());
            lost.set(!held(db, project, claim));
        }
        lost.get() || runner.is_stopping()
    };
    let (ending, kept) = asked(runner, &run, &stop);
    *left = kept;
    match ending {
        Ok(ReadOnlyEnding::Answered(text)) => parse_reply(&text, &check.items)
            .map(|states| (states, check.source))
            .map_err(failed),
        Ok(ReadOnlyEnding::Failed(reason)) => Err(Unchecked::Failed(reason)),
        Ok(ReadOnlyEnding::TimedOut) => Err(failed(TIMED_OUT)),
        Ok(ReadOnlyEnding::Stopped) | Err(RunError::Busy) => Err(Unchecked::Later),
        Err(error) => Err(Unchecked::Failed(error.to_string())),
    }
}

/// Runs the status agent and names what the run left in the research state, its folder
/// and its Seatbelt profile, for removal once the usage upkeep has counted the run. A run
/// whose end is uncertain names nothing, since its agent may still use them, and a name
/// that was already taken is not this run's to remove. Inputs: the runner, the run and
/// `stop`. Output: how the run ended and what it left.
pub(super) fn asked(
    runner: &ResearchRunner,
    run: &ReadOnlyRun<'_>,
    stop: &dyn Fn() -> bool,
) -> (Result<ReadOnlyEnding, RunError>, Option<RunLeft>) {
    assert!(run.name.starts_with(RUN_NAME_HEAD));
    assert!(run.name.len() <= RUN_NAME_BYTES_MAX);
    let folder = runner.state().join("agents").join(run.name);
    let profile = runner
        .state()
        .join("profiles")
        .join(format!("{}.sb", run.name));
    let fresh = std::fs::symlink_metadata(&folder).is_err();
    let ending = runner.run_read_only(run, stop);
    let taken = matches!(&ending, Err(RunError::Invalid(reason)) if reason == RUN_NAME_USED);
    let left = (fresh && !taken && !matches!(ending, Err(RunError::Uncertain(_))))
        .then(|| RunLeft::at(folder, profile, Instant::now()));
    (ending, left)
}

/// The agent state name of one status run: its project and when it started, so no run
/// reuses the folder of an earlier one. Inputs: the project and now. Output: the name,
/// ASCII letters, digits, `-` and `_` only, which `names_run` reads back as this project's.
pub(super) fn run_name(project: &ProjectId, now_ms: i64) -> String {
    let name = format!("{}-{}", run_stem(project), now_ms.max(0));
    assert!(!name.is_empty() && name.len() <= RUN_NAME_BYTES_MAX);
    assert!(names_run(project, &name));
    name
}
