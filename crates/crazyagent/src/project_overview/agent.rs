use super::{
    Competitors, Ended, RUN_TIME, Research, TREE_PATHS_MAX, brief, competitors, finish, needs_home,
    report_text, requeue,
};
use crate::project_setup::ResearchKind;
use crate::project_setup::links;
use crate::protocol::ProjectId;
use crate::research::runner::{
    ReadOnlyEnding, ReadOnlyRun, ResearchAccount, ResearchRunner, RunError, unreached_provider_here,
};
use crate::roadmap_status::Claim;
use rusqlite::Connection;
use std::path::Path;
use std::time::Duration;

/// What a writing run says when its agent ran out of time.
pub const TIMED_OUT: &str = "The refresh ran out of time";
/// What a writing run says when the project's hidden folders hold tracked files or cannot
/// be read.
pub const HIDDEN_UNREADABLE: &str = "The project's hidden folders could not be checked";
/// What a writing run says when the project's files and documents could not be read.
pub const EVIDENCE_UNREADABLE: &str = "Could not read the project's files and documents";
/// The most output tokens one writing run may spend.
pub const OUTPUT_TOKENS: u32 = 16 * 1024;
/// How long after a run that could not start (the engine is stopping, the account is busy)
/// the next try is due.
pub const RETRY_MS: i64 = 60 * 1000;

const PREPARE_SECS: u64 = 90;
const AGENT_TIME: Duration = Duration::from_secs(RUN_TIME.as_secs() - PREPARE_SECS);
const RUN_NAME_BYTES_MAX: usize = 96;
const RUN_NAME_HEAD: &str = "overview-";

const _: () = assert!(RUN_TIME.as_secs() >= 4 * PREPARE_SECS && RETRY_MS > 0);
const _: () = assert!(RETRY_MS < crate::roadmap_status::QUIET_MS);

/// How a run ended without a reply: with words for the owner, with nothing to do, or too
/// early to say, so it runs again later.
pub(super) enum Unwritten {
    Failed(String),
    Skipped,
    Later,
}

/// Runs one writing run `tick` claimed to its end and stores how it ended: the tabs of the
/// reply, the words of its failure, nothing for a project that cannot hold tab files, or a
/// new try after `RETRY_MS` when the engine is stopping or the account is busy. A result
/// that cannot be stored leaves the run open; `tick` closes it once its time is over.
/// Inputs: the runner, the project, the claim the run holds, the project's folder and the
/// account to run on.
pub(crate) fn write(
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
        Ok((reply, with_competitors)) => {
            let ended = Ended::Reply {
                reply: reply.as_str(),
                with_competitors: *with_competitors,
            };
            finish(&db, project, root, claim, ended, now)
        }
        Err(Unwritten::Failed(words)) => {
            finish(&db, project, root, claim, Ended::Failed(words), now)
        }
        Err(Unwritten::Skipped) => finish(&db, project, root, claim, Ended::Skipped, now),
        Err(Unwritten::Later) => requeue(&db, project, claim, now.saturating_add(RETRY_MS)),
    };
}

/// The competitor research a run is shown: the report of the newest finished competitor
/// research job, read from the project's state folder where the runner writes it, else
/// the competitor research files the owner linked, each read with no link followed. A
/// report that went away since it was listed gives way to the linked files. The evidence
/// takes at most `RESEARCH_BYTES_MAX` of them. Inputs: the connection, the project and
/// its folder. Output: the files, or none when the store cannot be read.
pub(super) fn research(db: &Connection, project: &ProjectId, root: &Path) -> Option<Vec<Research>> {
    let linked = || -> Option<Vec<Research>> {
        let files = links::research(db, project, root).ok()?;
        Some(
            files
                .into_iter()
                .filter(|(kind, _)| *kind == ResearchKind::CompetitorResearch)
                .map(|(_, file)| file)
                .collect(),
        )
    };
    let found = match competitors(db, project, root).ok()? {
        Competitors::Missing => Vec::new(),
        Competitors::Report { path } => match report_text(root, &path) {
            Some(text) => vec![Research { path, text }],
            None => linked().unwrap_or_default(),
        },
        Competitors::Linked { .. } => linked()?,
    };
    assert!(found.len() <= links::LINKS_MAX);
    Some(found)
}

/// Asks the writing agent about one project: checks the project can hold tab files and
/// hides nothing tracked, gathers the research and the tracked paths, and runs the agent
/// with tools off on the project's account. Inputs: the runner, the engine connection, the
/// project, its folder and the account. Output: the reply and whether competitor research
/// was in its evidence, or how the run ended without one.
pub(super) fn answer(
    runner: &ResearchRunner,
    db: &Connection,
    project: &ProjectId,
    root: &Path,
    account: &ResearchAccount,
) -> Result<(String, bool), Unwritten> {
    let failed = |words: &str| Unwritten::Failed(words.to_owned());
    if needs_home(root) {
        return Err(Unwritten::Skipped);
    }
    if let Some(line) = unreached_provider_here(account) {
        return Err(Unwritten::Failed(line));
    }
    let hidden = crate::sandbox::hidden_paths::load(db, project.as_str())
        .map_err(|_| failed(HIDDEN_UNREADABLE))?;
    crate::sandbox::hidden_paths::none_tracked(root, &hidden)
        .map_err(|_| failed(HIDDEN_UNREADABLE))?;
    let research = research(db, project, root).ok_or_else(|| failed(EVIDENCE_UNREADABLE))?;
    let tree = if root.join(".git").exists() {
        crate::landing::evidence::tracked_paths(root, |_| true, TREE_PATHS_MAX)
            .map_err(|_| failed(EVIDENCE_UNREADABLE))?
    } else {
        Vec::new()
    };
    let now = crate::team::now_ms();
    let (prompt, with_competitors) =
        brief(db, project, root, &research, &tree, now).map_err(|_| failed(EVIDENCE_UNREADABLE))?;
    let name = run_name(project, now);
    let run = ReadOnlyRun {
        name: &name,
        agent: "overview",
        project: Some(project),
        folder: root,
        prompt: &prompt,
        account,
        wall_time: AGENT_TIME,
        output_tokens: Some(OUTPUT_TOKENS),
    };
    match runner.run_read_only(&run, &|| runner.is_stopping()) {
        Ok(ReadOnlyEnding::Answered(text)) => Ok((text, with_competitors)),
        Ok(ReadOnlyEnding::Failed(reason)) => Err(Unwritten::Failed(reason)),
        Ok(ReadOnlyEnding::TimedOut) => Err(failed(TIMED_OUT)),
        Ok(ReadOnlyEnding::Stopped) | Err(RunError::Busy) => Err(Unwritten::Later),
        Err(error) => Err(Unwritten::Failed(error.to_string())),
    }
}

/// The agent state name of one writing run: its project and when it started, so no run
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
