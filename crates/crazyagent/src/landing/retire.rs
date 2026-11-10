use super::AgentKind;
use super::clone;
use super::flow::{
    Agent, agent_branch, check_places, clone_branch, drop_branch, hold, holds_only_except,
    linked_git_dir, open_agent, set_engine_ref, shared,
};
use super::git::{Repo, failure};
use super::store;
use crate::agent_git::{self, Layout};
use crate::engine::EngineError;
use rusqlite::{Connection, params};
use std::path::{Path, PathBuf};

const REMOVE_OUTPUT_BYTES_MAX: usize = 4096;
const TIP_LISTING_BYTES_MAX: usize = 64 * 1024 * 1024;

fn unavailable(_: impl std::fmt::Display) -> EngineError {
    failure("changes_unavailable")
}

fn invalid() -> EngineError {
    failure("changes_workspace_invalid")
}

/// What the engine's own fetch read from an agent's clone: its branch tip with each
/// teammate branch's tip, or the plain code that keeps the clone. None for a worktree the
/// engine did not record as a clone.
type Fetched = Option<Result<(String, Vec<(String, String)>), &'static str>>;

/// Reads the branch tip of an agent's own clone through the engine's fetch into the owner
/// repository (`clone::fetch`, upload-pack `--strict`), so no git ever runs in the clone
/// under the agent's config, before any hold is taken, so a slow agent-sized fetch never
/// stalls landing (DEC102-F2). A clone git cannot fetch from keeps only this agent, as
/// `cleanup_worktree_unreadable`. A harness's teammate branches are fetched the same way
/// (`clone::fetch_teammates`), so cleanup can tell whether their work is in the task
/// branch. Inputs: the store, the owner checkout root, the worktree and its git folder.
/// Output: the tips or the code, or None for a linked worktree.
fn fetch_clone(
    db: &Connection,
    repository: &Path,
    worktree: &Path,
    git_dir: &Path,
) -> Result<Fetched, EngineError> {
    if git_dir != agent_git::git_dir(worktree) {
        return Ok(None);
    }
    if check_places(repository, worktree, git_dir).is_err() {
        return Ok(Some(Err("cleanup_worktree_invalid")));
    }
    let shared = shared(repository)?;
    let branch = clone_branch(db, worktree)?;
    let bounds = &crate::worktrees::TREE_BOUNDS;
    let fetched = clone::fetch(&shared, repository, worktree, &branch, bounds).and_then(|tip| {
        clone::fetch_teammates(&shared, repository, worktree, &branch, bounds)
            .map(|teammates| (tip, teammates))
    });
    match fetched {
        Ok(tips) => Ok(Some(Ok(tips))),
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

/// Says whether a harness's teammates (DEC-100) left nothing that is not in its task
/// branch, so removing the clone loses no work: every teammate branch's tip, and every
/// teammate worktree's commit, is in the task branch tip, and every teammate worktree
/// holds just its commit with nothing uncommitted (`clone_holds_only`). The teammate
/// worktrees are read as data (`clone::teammates`) and their commits from what the
/// engine's fetch carried, so no git runs in the clone. Inputs: the owner checkout root,
/// the clone, its task branch, its tip and the fetched teammate branch tips. Output: None
/// when nothing would be lost, else `cleanup_refs_unlanded` for work not in the task
/// branch, `cleanup_worktree_dirty` for uncommitted work, or `cleanup_worktree_unreadable`.
fn teammates_landed(
    repository: &Path,
    worktree: &Path,
    branch: &str,
    tip: &str,
    tips: &[(String, String)],
) -> Result<Option<&'static str>, EngineError> {
    let teammates = match clone::teammates(worktree, branch) {
        Ok(teammates) => teammates,
        Err(_) => return Ok(Some("cleanup_worktree_unreadable")),
    };
    if teammates.is_empty() && tips.is_empty() {
        return Ok(None);
    }
    let shared = shared(repository)?;
    let landed = |commit: &str| shared.test(&["merge-base", "--is-ancestor", commit, tip]);
    for (_, teammate_tip) in tips {
        if !landed(teammate_tip)? {
            return Ok(Some("cleanup_refs_unlanded"));
        }
    }
    for teammate in &teammates {
        let commit = match &teammate.head {
            clone::TeammateHead::Branch(name) => tips
                .iter()
                .find(|(fetched, _)| fetched == name)
                .map(|(_, commit)| commit.clone()),
            clone::TeammateHead::Commit(id) => shared.commit_of(id)?,
        };
        let Some(commit) = commit else {
            return Ok(Some("cleanup_refs_unlanded"));
        };
        if !landed(&commit)? {
            return Ok(Some("cleanup_refs_unlanded"));
        }
        match clone_holds_only(repository, &teammate.folder, &commit) {
            Ok(true) => {}
            Ok(false) => return Ok(Some("cleanup_worktree_dirty")),
            Err(refused)
                if matches!(
                    refused.0.as_str(),
                    "changes_git_failed" | "changes_too_large"
                ) =>
            {
                return Ok(Some("cleanup_worktree_unreadable"));
            }
            Err(refused) => return Err(refused),
        }
    }
    Ok(None)
}

/// Names the native teammates of a harness that work in their own worktree on their own
/// branch (DEC-111 Q17, OP-9b): each `<worktree>/.worktrees/<name>` its clone's git, or for
/// a linked worktree the project repository's git, records checked out on
/// `<task branch>--<name>`, read as data with no git run. Inputs: the store, the worktree
/// and its git folder. Output: the names, or the code that kept them from being read.
pub(super) fn own_teammates(
    db: &Connection,
    worktree: &Path,
    git_dir: &Path,
) -> Result<Vec<String>, EngineError> {
    let (branch, teammates) = if git_dir == agent_git::git_dir(worktree) {
        let branch = clone_branch(db, worktree)?;
        let teammates = clone::teammates(worktree, &branch)?;
        (branch, teammates)
    } else {
        let repository = linked_repository(git_dir)?;
        if check_places(repository, worktree, git_dir)? != Layout::Linked {
            return Ok(Vec::new());
        }
        let branch = agent_branch(db, worktree, Layout::Linked)?.ok_or_else(invalid)?;
        let teammates = clone::linked_teammates(repository, worktree, &branch)?;
        (branch, teammates)
    };
    let named = teammates
        .into_iter()
        .filter_map(|teammate| {
            let name = teammate.folder.file_name()?.to_str()?.to_owned();
            match &teammate.head {
                clone::TeammateHead::Branch(head)
                    if agent_git::is_teammates_own_branch(&branch, &name, head) =>
                {
                    Some(name)
                }
                _ => None,
            }
        })
        .collect();
    Ok(named)
}

/// The linked worktree and git folder of one native teammate of a linked lead that works in
/// its own worktree on its own branch (OP-9b), read as data with no git run. Inputs: the
/// store, the lead's worktree and git folder and the teammate's name. Output: the
/// teammate's worktree and git folder, None for a lead in its own clone or a teammate with
/// no worktree on its own branch, or the code that kept them from being read.
pub(super) fn linked_teammate_place(
    db: &Connection,
    worktree: &Path,
    git_dir: &Path,
    name: &str,
) -> Result<Option<(PathBuf, PathBuf)>, EngineError> {
    if git_dir == agent_git::git_dir(worktree) {
        return Ok(None);
    }
    let repository = linked_repository(git_dir)?;
    if check_places(repository, worktree, git_dir)? != Layout::Linked {
        return Ok(None);
    }
    let branch = agent_branch(db, worktree, Layout::Linked)?.ok_or_else(invalid)?;
    let folder = worktree.join(agent_git::TEAMMATES_FOLDER).join(name);
    let own = clone::linked_teammates(repository, worktree, &branch)?
        .into_iter()
        .any(|teammate| {
            teammate.folder == folder
                && matches!(&teammate.head, clone::TeammateHead::Branch(head)
                    if agent_git::is_teammates_own_branch(&branch, name, head))
        });
    if !own {
        return Ok(None);
    }
    let own_git = linked_git_dir(&folder)?;
    if check_places(repository, &folder, &own_git)? != Layout::Linked {
        return Err(invalid());
    }
    Ok(Some((folder, own_git)))
}

/// The owner checkout root a linked worktree's git folder sits in,
/// `<repository>/.git/worktrees/<record>`, read from the path alone. Input: the git
/// folder. Output: the root, or `changes_workspace_invalid` for any other path.
pub(super) fn linked_repository(git_dir: &Path) -> Result<&Path, EngineError> {
    let records = git_dir.parent().ok_or_else(invalid)?;
    let metadata = records.parent().ok_or_else(invalid)?;
    if records.file_name() != Some(std::ffi::OsStr::new("worktrees"))
        || metadata.file_name() != Some(std::ffi::OsStr::new(".git"))
    {
        return Err(invalid());
    }
    metadata.parent().ok_or_else(invalid)
}

/// Names the teammate branches and teammate worktrees of a harness's clone whose work is
/// not in its task branch yet (DEC-104), by the fetch and ancestry test of
/// `teammates_landed`, listing each one instead of stopping at the first. Inputs: the
/// store, the owner checkout root, the worktree and its git folder. Output: the names,
/// none for a linked worktree or a clone without teammates, or the code that kept the
/// clone from being read.
pub(super) fn unlanded_teammates(
    db: &Connection,
    repository: &Path,
    worktree: &Path,
    git_dir: &Path,
) -> Result<Result<Vec<String>, &'static str>, EngineError> {
    let (tip, tips) = match fetch_clone(db, repository, worktree, git_dir)? {
        None => return Ok(Ok(Vec::new())),
        Some(Err(code)) => return Ok(Err(code)),
        Some(Ok(fetched)) => fetched,
    };
    let branch = clone_branch(db, worktree)?;
    let Ok(teammates) = clone::teammates(worktree, &branch) else {
        return Ok(Err("cleanup_worktree_unreadable"));
    };
    if teammates.is_empty() && tips.is_empty() {
        return Ok(Ok(Vec::new()));
    }
    let shared = shared(repository)?;
    let tip = tip.as_str();
    let landed = |commit: &str| shared.test(&["merge-base", "--is-ancestor", commit, tip]);
    let mut unlanded = Vec::new();
    for (name, teammate_tip) in &tips {
        if !landed(teammate_tip)? {
            unlanded.push(name.clone());
        }
    }
    for teammate in &teammates {
        let commit = match &teammate.head {
            clone::TeammateHead::Branch(name)
                if tips.iter().any(|(fetched, _)| fetched == name) =>
            {
                continue;
            }
            clone::TeammateHead::Branch(_) => None,
            clone::TeammateHead::Commit(id) => shared.commit_of(id)?,
        };
        let held = match commit {
            Some(commit) => landed(&commit)?,
            None => false,
        };
        if !held {
            let folder = teammate
                .folder
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            unlanded.push(format!("{}/{folder}", agent_git::TEAMMATES_FOLDER));
        }
    }
    assert!(unlanded.len() <= tips.len() + teammates.len());
    Ok(Ok(unlanded))
}

/// Says whether an agent's own clone holds exactly its branch tip, never reading the
/// clone's git folder: the owner repository's git, with the clone as its work tree and a
/// private index in the owner's git folder filled from the tip the engine fetched, runs
/// the same checks as for a linked worktree (`holds_only`) once `sizes_match` found no
/// file of another size, so the clone's config, hooks and index are never read. The
/// teammates' worktree folder is left out of its untracked files, since `teammates_landed`
/// checks it, and so is the code graph's copy unless the tip tracks it (`graph_excludes`).
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
        let teammates = format!("/{}/", agent_git::TEAMMATES_FOLDER);
        let mut excluded = vec![teammates.as_str()];
        excluded.extend_from_slice(graph_excludes(&repo, tip)?);
        holds_only_except(&repo, tip, &excluded)
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
/// `TREE_BOUNDS`. A harness's teammates (DEC-100) go with it only when their work is in
/// its branch (`teammates_landed`). Inputs: the store, the three paths and the time now.
/// Output: None when it can go.
pub(super) fn blocker(
    db: &Connection,
    repository: &Path,
    worktree: &Path,
    git_dir: &Path,
    now: u64,
) -> Result<Option<&'static str>, EngineError> {
    let fetched = fetch_clone(db, repository, worktree, git_dir)?;
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
    if held_sets(db, text, now)? {
        return Ok(Some("cleanup_changes_restorable"));
    }
    let layout = match check_places(repository, worktree, git_dir) {
        Ok(layout) => layout,
        Err(_) => return Ok(Some("cleanup_worktree_invalid")),
    };
    if layout == Layout::Clone {
        let (tip, teammate_tips) = match fetched {
            Some(Ok(tips)) => tips,
            Some(Err(code)) => return Ok(Some(code)),
            None => return Ok(Some("cleanup_worktree_invalid")),
        };
        if store::base_of(db, text)?.as_deref() != Some(tip.as_str()) {
            return Ok(Some("cleanup_commits_unlanded"));
        }
        let branch = clone_branch(db, worktree)?;
        match clone::other_refs(worktree, &branch, true) {
            Ok(other) if other.is_empty() => {}
            Ok(_) => return Ok(Some("cleanup_refs_unlanded")),
            Err(_) => return Ok(Some("cleanup_worktree_unreadable")),
        }
        if let Some(code) = teammates_landed(repository, worktree, &branch, &tip, &teammate_tips)? {
            return Ok(Some(code));
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
    if let Some(head) = &head
        && let Some(code) = linked_teammates_landed(db, repository, worktree, head, now)?
    {
        return Ok(Some(code));
    }
    match (head, store::base_of(db, text)?) {
        (Some(head), Some(base)) if head == base => {
            let held = graph_excludes(&agent.repo, &head)
                .and_then(|excluded| holds_only_except(&agent.repo, &head, excluded));
            match held {
                Err(refused) if refused.0 == "changes_git_failed" => {
                    Ok(Some("cleanup_worktree_unreadable"))
                }
                held => Ok((!held?).then_some("cleanup_worktree_dirty")),
            }
        }
        _ => Ok(Some("cleanup_commits_unlanded")),
    }
}

/// Tells whether a worktree's change sets still hold work the owner can get back: one with
/// an owner action in flight, or one discarded and still restorable. Inputs: the store,
/// the worktree path and the time now. Output: the answer.
fn held_sets(db: &Connection, worktree: &str, now: u64) -> Result<bool, EngineError> {
    let held: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM landing_change_sets WHERE worktree=?1 AND (intent IS NOT NULL
             OR (state='discarded' AND pruned=0 AND restorable_until_ms>?2))",
            params![worktree, i64::try_from(now).map_err(unavailable)?],
            |row| row.get(0),
        )
        .map_err(unavailable)?;
    Ok(held > 0)
}

/// Says whether a linked lead's worktree can go without losing its native teammates' work
/// (OP9B-S1), whatever ignore rule hides its teammates' folder from the lead's own git:
/// each teammate worktree has no open or restorable change set, stands at the base its
/// last landed set left or, never recorded, at a commit in the lead's head, and holds
/// nothing uncommitted. The folder is read as data and each teammate worktree with the
/// engine's own git. Inputs: the store, the owner checkout root, the lead's worktree, its
/// head and the time now. Output: None when nothing would be lost, else the cleanup code.
fn linked_teammates_landed(
    db: &Connection,
    repository: &Path,
    worktree: &Path,
    lead_head: &str,
    now: u64,
) -> Result<Option<&'static str>, EngineError> {
    let Some(branch) = agent_branch(db, worktree, Layout::Linked)? else {
        return Ok(Some("cleanup_worktree_invalid"));
    };
    let Ok(teammates) = clone::linked_teammates(repository, worktree, &branch) else {
        return Ok(Some("cleanup_worktree_unreadable"));
    };
    for teammate in &teammates {
        let Some(text) = teammate.folder.to_str() else {
            return Ok(Some("cleanup_worktree_invalid"));
        };
        if store::open_for(db, text)?.is_some() {
            return Ok(Some("cleanup_changes_open"));
        }
        if held_sets(db, text, now)? {
            return Ok(Some("cleanup_changes_restorable"));
        }
        let Ok(own_git) = linked_git_dir(&teammate.folder) else {
            return Ok(Some("cleanup_worktree_unreadable"));
        };
        let (agent, head) =
            match open_agent(db, AgentKind::Sub, repository, &teammate.folder, &own_git)
                .and_then(|agent| agent.head().map(|head| (agent, head)))
            {
                Ok(opened) => opened,
                Err(_) => return Ok(Some("cleanup_worktree_unreadable")),
            };
        let landed = match store::base_of(db, text)? {
            Some(base) => base == head,
            None => shared(repository)?.test(&["merge-base", "--is-ancestor", &head, lead_head])?,
        };
        if !landed {
            return Ok(Some("cleanup_commits_unlanded"));
        }
        let held = graph_excludes(&agent.repo, &head)
            .and_then(|excluded| holds_only_except(&agent.repo, &head, excluded));
        match held {
            Ok(true) => {}
            Ok(false) => return Ok(Some("cleanup_worktree_dirty")),
            Err(_) => return Ok(Some("cleanup_worktree_unreadable")),
        }
    }
    Ok(None)
}

/// The untracked paths a worktree check leaves out for the code graph's copy (DEC-123): its
/// folder, unless the commit tracks one, since then files there are the agent's work like any
/// other and an untracked one left among them keeps the agent, as the removal's git does
/// (rv-code L2). Inputs: the git to read with and the commit. Output: the paths left out.
fn graph_excludes(repo: &Repo, commit: &str) -> Result<&'static [&'static str], EngineError> {
    let name = agent_git::GRAPH_EXCLUDE.trim_matches('/');
    assert!(!name.is_empty() && !name.contains('/'));
    let tracked = repo.capture(
        &["ls-tree", "-z", "--name-only", commit, "--", name],
        REMOVE_OUTPUT_BYTES_MAX,
    )?;
    let excluded: &'static [&'static str] = if tracked.is_empty() {
        &[agent_git::GRAPH_EXCLUDE]
    } else {
        &[]
    };
    assert!(excluded.len() <= 1);
    Ok(excluded)
}

/// Deletes the code graph's copy at the top of a linked worktree whose commit holds none, so
/// git, which never removes a worktree with untracked files unforced, removes the rest; the
/// copy is never the agent's work (DEC-123). A link there goes as a link, never followed, and
/// a folder goes without following a link inside it. Inputs: the agent's git and its
/// worktree. Output: None once no copy is left, or `cleanup_worktree_dirty` when it stays.
fn drop_graph_copy(agent: &Agent, worktree: &Path) -> Result<Option<&'static str>, EngineError> {
    let name = agent_git::GRAPH_EXCLUDE.trim_matches('/');
    assert!(!name.is_empty() && !name.contains('/'));
    let copy = worktree.join(name);
    let Ok(found) = std::fs::symlink_metadata(&copy) else {
        return Ok(None);
    };
    let tracked = agent.repo.capture(
        &["ls-tree", "-z", "--name-only", "HEAD", "--", name],
        REMOVE_OUTPUT_BYTES_MAX,
    )?;
    if !tracked.is_empty() {
        return Ok(None);
    }
    let deleted = if found.is_dir() {
        std::fs::remove_dir_all(&copy)
    } else {
        std::fs::remove_file(&copy)
    };
    Ok(deleted.is_err().then_some("cleanup_worktree_dirty"))
}

/// Removes an agent worktree with git, never forcing it, while holding the repository so
/// no landing step runs on it meanwhile, after checking `blocker` again under that hold,
/// then forgets its base and its owner edits. An agent's own clone is fetched before the
/// hold and removed as a folder with no git, never following a link out of it, since git
/// keeps no record of it in the owner repository. Inputs: the store, the three paths and
/// the time now. Output: Ok once removed, `cleanup_worktree_leftover` with the base and
/// owner edits forgotten when git deleted its record of the worktree, or a clone's folder
/// was deleted, only part way, or the plain code that kept it.
pub(super) fn remove(
    db: &Connection,
    repository: &Path,
    worktree: &Path,
    git_dir: &Path,
    now: u64,
) -> Result<Result<(), &'static str>, EngineError> {
    let fetched = fetch_clone(db, repository, worktree, git_dir)?;
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
        store::forget_agent_branch(db, path)?;
        super::owner_edit::logged("forget", super::owner_edit::forget_worktree(db, worktree));
        return Ok(removed.map_err(|_| "cleanup_worktree_leftover"));
    }
    let agent = open_agent(db, AgentKind::Sub, repository, worktree, git_dir)?;
    let named = match agent.branch() {
        Some(branch) => Some((branch.to_owned(), agent.head()?)),
        None => None,
    };
    if let Some(code) = drop_graph_copy(&agent, worktree)? {
        return Ok(Err(code));
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
        super::owner_edit::logged("forget", super::owner_edit::forget_worktree(db, worktree));
        return Ok(Err("cleanup_worktree_leftover"));
    }
    if std::fs::symlink_metadata(worktree).is_ok() || std::fs::symlink_metadata(git_dir).is_ok() {
        return Err(failure("changes_git_failed"));
    }
    db.execute("DELETE FROM landing_bases WHERE worktree=?1", [path])
        .map_err(unavailable)?;
    super::owner_edit::logged("forget", super::owner_edit::forget_worktree(db, worktree));
    store::forget_agent_branch(db, path)?;
    if let Some((branch, head)) = named
        && !drop_branch(repository, &branch, &head)?
    {
        eprintln!("landing_branch_kept branch={branch}");
    }
    Ok(Ok(()))
}
