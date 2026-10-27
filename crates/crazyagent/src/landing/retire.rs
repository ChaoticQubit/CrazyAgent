use super::AgentKind;
use super::flow::{check_places, hold, holds_only, open_agent, shared};
use super::git::failure;
use super::store;
use crate::engine::EngineError;
use rusqlite::{Connection, params};
use std::path::Path;

const REMOVE_OUTPUT_BYTES_MAX: usize = 4096;

fn unavailable(_: impl std::fmt::Display) -> EngineError {
    failure("changes_unavailable")
}

/// Says why an agent worktree cannot be removed now, as a plain code, checking the store
/// before git: an open change set, a discarded one still restorable or work in flight, a
/// worktree the repository does not record as its own, a HEAD that is not the commit the
/// worktree's next change set would open above, or uncommitted or untracked files.
/// Inputs: the store, the three paths and the time now. Output: None when it can go.
pub(super) fn blocker(
    db: &Connection,
    repository: &Path,
    worktree: &Path,
    git_dir: &Path,
    now: u64,
) -> Result<Option<&'static str>, EngineError> {
    store::initialize(db)?;
    let Some(text) = worktree.to_str() else {
        return Ok(Some("cleanup_worktree_invalid"));
    };
    if store::open_for(db, text)?.is_some() {
        return Ok(Some("cleanup_changes_open"));
    }
    let held: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM landing_change_sets WHERE worktree=?1 AND (intent IS NOT NULL
             OR (state='discarded' AND pruned=0 AND restorable_until_ms>?2))",
            params![text, i64::try_from(now).map_err(unavailable)?],
            |row| row.get(0),
        )
        .map_err(unavailable)?;
    if held > 0 {
        return Ok(Some("cleanup_changes_restorable"));
    }
    if check_places(repository, worktree, git_dir).is_err() {
        return Ok(Some("cleanup_worktree_invalid"));
    }
    let agent = match open_agent(db, AgentKind::Sub, repository, worktree, git_dir) {
        Err(refused) if refused.0 == "changes_workspace_invalid" => {
            return Ok(Some("cleanup_worktree_invalid"));
        }
        opened => opened?,
    };
    let head = match agent.head() {
        Ok(head) => Some(head),
        Err(refused) if refused.0 == "changes_workspace_invalid" => {
            return Ok(Some("cleanup_worktree_invalid"));
        }
        Err(_) => None,
    };
    match (head, store::base_of(db, text)?) {
        (Some(head), Some(base)) if head == base => {
            Ok((!holds_only(&agent.repo, &head)?).then_some("cleanup_worktree_dirty"))
        }
        _ => Ok(Some("cleanup_commits_unlanded")),
    }
}

/// Removes an agent worktree with git, never forcing it, while holding the repository so
/// no landing step runs on it meanwhile, after checking `blocker` again under that hold,
/// then forgets its base. Inputs: the store, the three paths and the time now.
/// Output: Ok once removed, or the plain code that kept it.
pub(super) fn remove(
    db: &Connection,
    repository: &Path,
    worktree: &Path,
    git_dir: &Path,
    now: u64,
) -> Result<Result<(), &'static str>, EngineError> {
    let _held = hold(repository);
    if let Some(code) = blocker(db, repository, worktree, git_dir, now)? {
        return Ok(Err(code));
    }
    let path = worktree
        .to_str()
        .ok_or_else(|| failure("changes_git_failed"))?;
    let removed = shared(repository)?.exec(
        &["worktree", "remove", "--", path],
        None,
        &[],
        REMOVE_OUTPUT_BYTES_MAX,
    )?;
    if removed.code != 0 {
        return Ok(Err("cleanup_worktree_dirty"));
    }
    if std::fs::symlink_metadata(worktree).is_ok() || std::fs::symlink_metadata(git_dir).is_ok() {
        return Err(failure("changes_git_failed"));
    }
    db.execute("DELETE FROM landing_bases WHERE worktree=?1", [path])
        .map_err(unavailable)?;
    Ok(Ok(()))
}
