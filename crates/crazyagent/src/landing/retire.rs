use super::AgentKind;
use super::clone;
use super::flow::{check_places, hold, holds_only, open_agent, set_engine_ref, shared};
use super::git::{Repo, failure};
use super::store;
use crate::agent_git::{self, Layout};
use crate::engine::EngineError;
use rusqlite::{Connection, params};
use std::path::Path;

const REMOVE_OUTPUT_BYTES_MAX: usize = 4096;
const TIP_LISTING_BYTES_MAX: usize = 64 * 1024 * 1024;

fn unavailable(_: impl std::fmt::Display) -> EngineError {
    failure("changes_unavailable")
}

/// What the engine's own fetch read from an agent's clone: its branch tip, or the plain
/// code that keeps the clone. None for a worktree the engine did not record as a clone.
type Fetched = Option<Result<String, &'static str>>;

/// Reads the branch tip of an agent's own clone through the engine's fetch into the owner
/// repository (`clone::fetch`, upload-pack `--strict`), so no git ever runs in the clone
/// under the agent's config, before any hold is taken, so a slow agent-sized fetch never
/// stalls landing (DEC102-F2). A clone git cannot fetch from keeps only this agent, as
/// `cleanup_worktree_unreadable`. Inputs: the owner checkout root, the worktree and its
/// git folder. Output: the tip or the code, or None for a linked worktree.
fn fetch_clone(repository: &Path, worktree: &Path, git_dir: &Path) -> Result<Fetched, EngineError> {
    if git_dir != agent_git::git_dir(worktree) {
        return Ok(None);
    }
    if check_places(repository, worktree, git_dir).is_err() {
        return Ok(Some(Err("cleanup_worktree_invalid")));
    }
    let shared = shared(repository)?;
    match clone::fetch(
        &shared,
        repository,
        worktree,
        &crate::worktrees::TREE_BOUNDS,
    ) {
        Ok(tip) => Ok(Some(Ok(tip))),
        Err(refused) if refused.0 == "changes_workspace_invalid" => {
            Ok(Some(Err("cleanup_worktree_invalid")))
        }
        Err(refused)
            if matches!(
                refused.0.as_str(),
                "changes_agent_branch_invalid" | "changes_too_large" | "changes_git_failed"
            ) =>
        {
            Ok(Some(Err("cleanup_worktree_unreadable")))
        }
        Err(refused) => Err(refused),
    }
}

/// Says whether an agent's own clone holds exactly its branch tip, never reading the
/// clone's git folder: the owner repository's git, with the clone as its work tree and a
/// private index in the owner's git folder filled from the tip the engine fetched, runs
/// the same checks as for a linked worktree (`holds_only`) once `sizes_match` found no
/// file of another size, so the clone's config, hooks and index are never read.
/// Inputs: the owner checkout root, the clone and the tip. Output: true when it holds
/// nothing else.
fn clone_holds_only(repository: &Path, worktree: &Path, tip: &str) -> Result<bool, EngineError> {
    let metadata = repository.join(".git");
    let folder = metadata.join(format!(
        "crazyagent-clean-{}",
        &crate::engine::owner::random_id()?[..24]
    ));
    std::fs::create_dir(&folder).map_err(|_| failure("changes_git_failed"))?;
    let held = Repo::open(repository, &metadata, Some(worktree)).and_then(|repo| {
        if !sizes_match(&repo, worktree, tip)? {
            return Ok(false);
        }
        let repo = repo.private_index(&folder.join("index"));
        repo.run(&["read-tree", tip])?;
        holds_only(&repo, tip)
    });
    let _ = std::fs::remove_dir_all(&folder);
    held
}

/// Compares each file and symlink of a tip with the clone's work tree by kind and size
/// alone, read as metadata without following a link at the path, before git hashes any
/// of them (CCS-1): a private index `read-tree` fills holds no sizes, so git would hash
/// every tracked file in full under the repository hold, however large the agent made
/// it. Inputs: the owner repository's git, the clone and the tip. Output: false at the
/// first one that differs or is missing, true when all match, so git then hashes at most
/// the tip's own bytes, else `changes_too_large` past `TREE_BOUNDS`.
fn sizes_match(repo: &Repo, worktree: &Path, tip: &str) -> Result<bool, EngineError> {
    use std::os::unix::ffi::OsStrExt;
    let listed = repo.capture(&["ls-tree", "-r", "-z", "-l", tip], TIP_LISTING_BYTES_MAX)?;
    let bounds = &crate::worktrees::TREE_BOUNDS;
    let (mut files, mut bytes) = (0_usize, 0_u64);
    for entry in listed
        .split(|byte| *byte == 0)
        .filter(|entry| !entry.is_empty())
    {
        let tab = entry
            .iter()
            .position(|byte| *byte == b'\t')
            .ok_or_else(|| failure("changes_git_failed"))?;
        let header =
            std::str::from_utf8(&entry[..tab]).map_err(|_| failure("changes_git_failed"))?;
        let fields: Vec<&str> = header.split_whitespace().collect();
        let [mode, kind, _, size] = fields[..] else {
            return Err(failure("changes_git_failed"));
        };
        if kind != "blob" {
            continue;
        }
        let size: u64 = size.parse().map_err(|_| failure("changes_git_failed"))?;
        files += 1;
        bytes = bytes.saturating_add(size);
        if files > bounds.files || bytes > bounds.bytes {
            return Err(failure("changes_too_large"));
        }
        let path = worktree.join(std::ffi::OsStr::from_bytes(&entry[tab + 1..]));
        let Ok(found) = std::fs::symlink_metadata(&path) else {
            return Ok(false);
        };
        let same_kind = if mode == "120000" {
            found.file_type().is_symlink()
        } else {
            found.is_file()
        };
        if !same_kind || found.len() != size {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Says why an agent worktree cannot be removed now, as a plain code, checking the store
/// before git: an open change set, a discarded one still restorable or work in flight, a
/// worktree the repository does not record as its own, a HEAD that is not the commit the
/// worktree's next change set would open above, uncommitted or untracked files, or a
/// worktree git cannot read, such as one a stale lock holds, which keeps only this agent.
/// An agent's own clone is never opened: its branch, read by the engine's fetch, must be
/// the commit its next change set would open above, else `cleanup_commits_unlanded`, it
/// must hold no other ref, such as a stash, branch or tag (`clone::other_refs`), else
/// `cleanup_refs_unlanded`, or `cleanup_worktree_unreadable` for ref data it cannot read,
/// and its files must hold just that commit (`clone_holds_only`), checked by size first
/// (`sizes_match`), else `cleanup_worktree_dirty`, or `cleanup_worktree_unreadable` past
/// `TREE_BOUNDS`. Inputs: the store, the three paths and the time now.
/// Output: None when it can go.
pub(super) fn blocker(
    db: &Connection,
    repository: &Path,
    worktree: &Path,
    git_dir: &Path,
    now: u64,
) -> Result<Option<&'static str>, EngineError> {
    let fetched = fetch_clone(repository, worktree, git_dir)?;
    check(db, repository, worktree, git_dir, now, fetched)
}

/// Runs `blocker`'s checks with a clone's branch tip already fetched. Inputs: the store,
/// the three paths, the time now and the fetch. Output: None when the worktree can go.
fn check(
    db: &Connection,
    repository: &Path,
    worktree: &Path,
    git_dir: &Path,
    now: u64,
    fetched: Fetched,
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
    let layout = match check_places(repository, worktree, git_dir) {
        Ok(layout) => layout,
        Err(_) => return Ok(Some("cleanup_worktree_invalid")),
    };
    if layout == Layout::Clone {
        let tip = match fetched {
            Some(Ok(tip)) => tip,
            Some(Err(code)) => return Ok(Some(code)),
            None => return Ok(Some("cleanup_worktree_invalid")),
        };
        if store::base_of(db, text)?.as_deref() != Some(tip.as_str()) {
            return Ok(Some("cleanup_commits_unlanded"));
        }
        match clone::other_refs(worktree) {
            Ok(other) if other.is_empty() => {}
            Ok(_) => return Ok(Some("cleanup_refs_unlanded")),
            Err(_) => return Ok(Some("cleanup_worktree_unreadable")),
        }
        return match clone_holds_only(repository, worktree, &tip) {
            Err(refused)
                if matches!(
                    refused.0.as_str(),
                    "changes_git_failed" | "changes_too_large"
                ) =>
            {
                Ok(Some("cleanup_worktree_unreadable"))
            }
            held => Ok((!held?).then_some("cleanup_worktree_dirty")),
        };
    }
    let agent = match open_agent(db, AgentKind::Sub, repository, worktree, git_dir) {
        Err(refused) if refused.0 == "changes_workspace_invalid" => {
            return Ok(Some("cleanup_worktree_invalid"));
        }
        Err(refused) if refused.0 == "changes_git_failed" => {
            return Ok(Some("cleanup_worktree_unreadable"));
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
        (Some(head), Some(base)) if head == base => match holds_only(&agent.repo, &head) {
            Err(refused) if refused.0 == "changes_git_failed" => {
                Ok(Some("cleanup_worktree_unreadable"))
            }
            held => Ok((!held?).then_some("cleanup_worktree_dirty")),
        },
        _ => Ok(Some("cleanup_commits_unlanded")),
    }
}

/// Removes an agent worktree with git, never forcing it, while holding the repository so
/// no landing step runs on it meanwhile, after checking `blocker` again under that hold,
/// then forgets its base. An agent's own clone is fetched before the hold and removed as a
/// folder with no git, never following a link out of it, since git keeps no record of it
/// in the owner repository. Inputs: the store, the three paths and the time now.
/// Output: Ok once removed, `cleanup_worktree_leftover` with the base forgotten when git
/// deleted its record of the worktree, or a clone's folder was deleted, only part way, or
/// the plain code that kept it.
pub(super) fn remove(
    db: &Connection,
    repository: &Path,
    worktree: &Path,
    git_dir: &Path,
    now: u64,
) -> Result<Result<(), &'static str>, EngineError> {
    let fetched = fetch_clone(repository, worktree, git_dir)?;
    let in_clone = fetched.is_some();
    let _held = hold(repository);
    if let Some(code) = check(db, repository, worktree, git_dir, now, fetched)? {
        return Ok(Err(code));
    }
    let path = worktree
        .to_str()
        .ok_or_else(|| failure("changes_git_failed"))?;
    if in_clone {
        set_engine_ref(
            &shared(repository)?,
            repository,
            &clone::base_ref(worktree)?,
            None,
        )?;
        let removed = std::fs::remove_dir_all(worktree);
        db.execute("DELETE FROM landing_bases WHERE worktree=?1", [path])
            .map_err(unavailable)?;
        return Ok(removed.map_err(|_| "cleanup_worktree_leftover"));
    }
    let removed = shared(repository)?.exec(
        &["worktree", "remove", "--", path],
        None,
        &[],
        REMOVE_OUTPUT_BYTES_MAX,
    )?;
    if removed.code != 0 {
        let unregistered = matches!(
            std::fs::symlink_metadata(git_dir),
            Err(problem) if problem.kind() == std::io::ErrorKind::NotFound
        );
        if !unregistered {
            return Ok(Err("cleanup_worktree_dirty"));
        }
        db.execute("DELETE FROM landing_bases WHERE worktree=?1", [path])
            .map_err(unavailable)?;
        return Ok(Err("cleanup_worktree_leftover"));
    }
    if std::fs::symlink_metadata(worktree).is_ok() || std::fs::symlink_metadata(git_dir).is_ok() {
        return Err(failure("changes_git_failed"));
    }
    db.execute("DELETE FROM landing_bases WHERE worktree=?1", [path])
        .map_err(unavailable)?;
    Ok(Ok(()))
}
