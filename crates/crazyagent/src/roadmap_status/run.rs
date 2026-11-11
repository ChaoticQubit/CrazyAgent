use super::*;
use crate::research::runner::{
    ReadOnlyEnding, ReadOnlyRun, ResearchAccount, ResearchRunner, RunError,
};
use std::path::Path;

/// What a check says when its agent ran out of time.
pub const TIMED_OUT: &str = "The check ran out of time";
/// What a check says when the project's hidden folders hold tracked files or cannot be read.
pub const HIDDEN_UNREADABLE: &str = "The project's hidden folders could not be checked";

const RUN_NAME_BYTES_MAX: usize = 96;
const RUN_NAME_HEAD: &str = "roadmap-status-";

/// How a check ended without a status: with words for the owner, or too early to say, so it
/// runs again later.
enum Unchecked {
    Failed(String),
    Later,
}

/// Runs one status check `tick` claimed to its end and stores how it ended: the item states,
/// the words of its failure, or a new try after `RETRY_MS` when the engine is stopping or
/// the account is busy. A result that cannot be stored leaves the run open; `tick` closes it
/// with `UNFINISHED` once its time is over. Inputs: the runner, the project, the claim the
/// run holds, the project's folder and the account to run on.
pub(crate) fn check(
    runner: &ResearchRunner,
    project: &ProjectId,
    claim: Claim,
    root: &Path,
    account: &ResearchAccount,
) {
    assert!(root.is_absolute());
    let Some(db) = runner
        .database()
        .parent()
        .and_then(|engine| crate::questions::open(engine).ok())
    else {
        return;
    };
    let outcome = answer(runner, &db, project, root, account);
    let now = crate::team::now_ms();
    let _ = match &outcome {
        Ok((states, source)) => finish(&db, project, claim, Ok((states.as_slice(), source)), now),
        Err(Unchecked::Failed(words)) => finish(&db, project, claim, Err(words), now),
        Err(Unchecked::Later) => requeue(&db, project, claim, now.saturating_add(RETRY_MS)),
    };
}

/// Asks the status agent about one project: prepares the items and the evidence, runs the
/// agent with tools off on the project's account and reads its reply. Inputs: the runner,
/// the engine connection, the project, its folder and the account. Output: the item states
/// and the roadmap file they came from, or how the check ended without them.
fn answer(
    runner: &ResearchRunner,
    db: &Connection,
    project: &ProjectId,
    root: &Path,
    account: &ResearchAccount,
) -> Result<(Vec<ItemState>, String), Unchecked> {
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
    match runner.run_read_only(&run, &|| runner.is_stopping()) {
        Ok(ReadOnlyEnding::Answered(text)) => parse_reply(&text, &check.items)
            .map(|states| (states, check.source))
            .map_err(failed),
        Ok(ReadOnlyEnding::Failed(reason)) => Err(Unchecked::Failed(reason)),
        Ok(ReadOnlyEnding::TimedOut) => Err(failed(TIMED_OUT)),
        Ok(ReadOnlyEnding::Stopped) | Err(RunError::Busy) => Err(Unchecked::Later),
        Err(error) => Err(Unchecked::Failed(error.to_string())),
    }
}

/// The agent state name of one status run: its project and when it started, so no run
/// reuses the folder of an earlier one; the project part is cut to fit the bound. Inputs:
/// the project and now. Output: the name, ASCII letters, digits, `-` and `_` only.
fn run_name(project: &ProjectId, now_ms: i64) -> String {
    let tail = format!("-{now_ms}");
    let room = RUN_NAME_BYTES_MAX - RUN_NAME_HEAD.len() - tail.len();
    let named: String = project
        .as_str()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .take(room)
        .collect();
    let name = format!("{RUN_NAME_HEAD}{named}{tail}");
    assert!(!name.is_empty() && name.len() <= RUN_NAME_BYTES_MAX);
    assert!(name.is_ascii());
    name
}
