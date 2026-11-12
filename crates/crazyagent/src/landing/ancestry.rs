use super::flow::{self, hold_within};
use super::git::{Repo, failure};
use super::{AgentKind, ChangeAgent, Workspace, clone, owner, store, subagent};
use crate::agent_git::Layout;
use crate::engine::EngineError;
use crate::protocol::ProjectId;
use crate::team::LandingMode;
use rusqlite::{Connection, OptionalExtension, params};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The most agent commits one Merge commit landing may carry into the owner's history.
pub(super) const MERGED_COMMITS_MAX: usize = 256;

/// The most bytes of carried commit objects, all together, the check reads.
const MERGED_TEXT_BYTES_MAX: usize = 4 * 1024 * 1024;

/// The most agent worktrees one Accept moves onto the landed tip.
const MOVED_PLACES_MAX: usize = 512;

/// How long a turn end or a task hand-off, both on the team runtime's own thread, waits for
/// landing work that holds the repository before it leaves the move to the next one.
const HOLD_WAIT: Duration = Duration::from_secs(2);

/// The most bytes the commit list of the Merge commit check may print: 257 lines of a
/// commit and its parents.
const LISTED_BYTES_MAX: usize = 256 * 1024;

/// The most bytes one carried commit object may hold.
const COMMIT_BYTES_MAX: usize = 1024 * 1024;

/// One agent worktree the team records. Fields: the agent id and its three paths.
struct Place {
    agent: i64,
    repository: PathBuf,
    worktree: PathBuf,
    git_dir: PathBuf,
}

/// The second parent an Accept gives the commit it lands (DEC-147.6): none under the
/// project's Landing setting Squash, the set's head under Merge commit once every commit
/// the merge would carry into the owner's history passed [`check_carried`]. Inputs: the
/// store, the owner repository and checkout, the project, the tip the commit lands on, the
/// set's head and the owner's identity. Output: the parent, None for a squash, or the
/// refusal.
pub(super) fn merge_parent(
    db: &Connection,
    owner: &Repo,
    checkout: &Path,
    project: &ProjectId,
    tip: &str,
    head: &str,
    identity: (&str, &str),
) -> Result<Option<String>, EngineError> {
    assert!(super::git::is_object_id(tip) && super::git::is_object_id(head));
    match crate::team::landing_mode(db, project)? {
        LandingMode::Squash => Ok(None),
        LandingMode::Merge => {
            check_carried(owner, checkout, tip, head, identity)?;
            Ok(Some(head.to_owned()))
        }
    }
}

/// Checks every commit a Merge commit would make part of the owner's branch, the ones
/// reachable from `head` but not from `tip`, which includes agent commits below a set's
/// measured base that an earlier Squash landing left out of the owner's history. There may
/// be at most [`MERGED_COMMITS_MAX`] and each needs a parent. Each commit object is read
/// whole, so no byte of a message can pass for a separator (L3 r1 F1), and must be UTF-8
/// with no `encoding` other than UTF-8, no hidden text and no control character other than
/// a newline or a tab in any header or message line. Its author and committer must be the
/// owner's identity, which the engine gives agents' commits, or the engine's own (L3-L1).
/// Its tree must pass the landing's own tree checks against its first parent (L3-M1).
/// Inputs: the owner repository and checkout, the tip, the head and the owner's identity.
/// Output: Ok, or `changes_merge_unsafe` or `changes_git_failed`.
pub(super) fn check_carried(
    owner: &Repo,
    checkout: &Path,
    tip: &str,
    head: &str,
    identity: (&str, &str),
) -> Result<(), EngineError> {
    let range = format!("{tip}..{head}");
    let count = format!("--max-count={}", MERGED_COMMITS_MAX + 1);
    let listed = owner.exec(
        &["rev-list", "--parents", &count, &range, "--"],
        None,
        &[],
        LISTED_BYTES_MAX,
    )?;
    if listed.code != 0 || listed.overflow {
        return Err(failure("changes_git_failed"));
    }
    let text = std::str::from_utf8(&listed.stdout).map_err(|_| failure("changes_git_failed"))?;
    let lines: Vec<&str> = text.lines().filter(|line| !line.is_empty()).collect();
    if lines.len() > MERGED_COMMITS_MAX {
        return Err(failure("changes_merge_unsafe"));
    }
    let mut carried: Vec<(&str, &str)> = Vec::with_capacity(lines.len());
    for line in lines {
        let ids: Vec<&str> = line.split(' ').collect();
        if !ids.iter().all(|id| super::git::is_object_id(id)) {
            return Err(failure("changes_git_failed"));
        }
        let &[commit, first, ..] = ids.as_slice() else {
            return Err(failure("changes_merge_unsafe"));
        };
        carried.push((commit, first));
    }
    assert!(carried.len() <= MERGED_COMMITS_MAX);
    let mut read = 0usize;
    for &(commit, _) in &carried {
        let object = owner.exec(&["cat-file", "commit", commit], None, &[], COMMIT_BYTES_MAX)?;
        if object.code != 0 {
            return Err(failure("changes_git_failed"));
        }
        read = read.saturating_add(object.stdout.len());
        if object.overflow || read > MERGED_TEXT_BYTES_MAX {
            return Err(failure("changes_merge_unsafe"));
        }
        let text =
            std::str::from_utf8(&object.stdout).map_err(|_| failure("changes_merge_unsafe"))?;
        if !plain_commit(text, identity) {
            return Err(failure("changes_merge_unsafe"));
        }
    }
    for (commit, parent) in carried {
        check_carried_tree(owner, checkout, parent, commit)?;
    }
    Ok(())
}

/// Tells whether one raw commit object may enter the owner's history under Merge commit:
/// every header and message line is plain text, at most one `author` and one `committer`
/// header, both naming an identity the engine gives, and no `encoding` but UTF-8, so git
/// shows the owner the same text that was checked. Inputs: the object as UTF-8 and the
/// owner's identity. Output: the answer.
fn plain_commit(text: &str, identity: (&str, &str)) -> bool {
    if !plain_text(text) {
        return false;
    }
    let headers = text.split_once("\n\n").map_or(text, |(headers, _)| headers);
    let (mut author, mut committer) = (None, None);
    for line in headers.lines() {
        if let Some(rest) = line.strip_prefix("author ") {
            if author.replace(person(rest)).is_some() {
                return false;
            }
        } else if let Some(rest) = line.strip_prefix("committer ") {
            if committer.replace(person(rest)).is_some() {
                return false;
            }
        } else if let Some(rest) = line.strip_prefix("encoding ") {
            if !matches!(rest.to_ascii_lowercase().as_str(), "utf-8" | "utf8") {
                return false;
            }
        }
    }
    matches!(
        (author, committer),
        (Some(Some(author)), Some(Some(committer)))
            if known_identity(author, identity) && known_identity(committer, identity)
    )
}

/// Reads the name and email of an `author` or `committer` header after its keyword: the
/// name, then the email in angle brackets, then the date. Input: the header's value.
/// Output: the name and email, or None for a malformed value.
fn person(value: &str) -> Option<(&str, &str)> {
    let (name, rest) = value.split_once(" <")?;
    let (email, _) = rest.split_once('>')?;
    Some((name, email))
}

/// Runs the landing's tree checks on one carried commit against its first parent, as
/// [`flow::check_merged`] and [`owner::uses_configured_commands`] check a set's merged
/// tree: a refusal of either becomes `changes_merge_unsafe`. Inputs: the owner repository
/// and checkout, the parent and the commit. Output: Ok, or the refusal.
fn check_carried_tree(
    owner: &Repo,
    checkout: &Path,
    parent: &str,
    commit: &str,
) -> Result<(), EngineError> {
    let unsafe_merge = |error: EngineError| {
        if error.0 == "changes_git_failed" {
            error
        } else {
            failure("changes_merge_unsafe")
        }
    };
    flow::check_merged(owner, checkout, parent, commit, commit).map_err(unsafe_merge)?;
    let paths = super::diff::changed_paths(owner, parent, commit).map_err(unsafe_merge)?;
    if super::owner::uses_configured_commands(owner, &paths, &[commit]).map_err(unsafe_merge)? {
        return Err(failure("changes_merge_unsafe"));
    }
    Ok(())
}

/// Tells whether a carried commit's author or committer is one the engine gives: the
/// owner's identity, which agents' own commits take, or the engine's own (L3-L1). Inputs:
/// the commit's name and email and the owner's identity. Output: the answer.
fn known_identity(person: (&str, &str), owner: (&str, &str)) -> bool {
    person == owner || person == flow::ENGINE
}

/// Tells whether agent-written text may enter the owner's history: no hidden text and no
/// control character but a newline or a tab. Input: the text. Output: the answer.
pub(super) fn plain_text(text: &str) -> bool {
    !crate::memory::hidden_text(text)
        && text
            .chars()
            .all(|c| !c.is_control() || c == '\n' || c == '\t')
}

/// Moves every resting sub-agent of a project onto the tip of its Land on branch after an
/// Accept (DEC-147.6): each one whose row says idle, stopped or failed, in a linked worktree
/// of `repository`, other than `skip`, the accepted set's own, as [`move_place`] says. A
/// busy agent moves at its turn end ([`move_after_turn`]) and an agent's own clone hears of
/// it at its next task ([`tell_main_moved`]). Each one it cannot move is logged by its
/// plain code and skipped. The caller holds the repository's hold. Inputs: the store, the
/// project, the repository and the worktree to skip. Output: the agents moved.
pub(super) fn move_resting(
    db: &Connection,
    project: &ProjectId,
    repository: &Path,
    skip: &Path,
) -> Vec<i64> {
    let places = match resting(db, project, repository) {
        Ok(places) => places,
        Err(error) => {
            log("landing_agents_unread", &error);
            return Vec::new();
        }
    };
    if places.is_empty() {
        return Vec::new();
    }
    let opened = owner::land_on_branch(db, repository, project).and_then(|found| {
        let owner = Repo::open(repository, &repository.join(".git"), None)?;
        Ok(found.map(|(_, tip)| (owner, tip)))
    });
    let (owner, tip) = match opened {
        Ok(Some(found)) => found,
        Ok(None) => return Vec::new(),
        Err(error) => {
            log("landing_main_unresolved", &error);
            return Vec::new();
        }
    };
    let mut moved = Vec::new();
    for place in places.iter().filter(|place| place.worktree != skip) {
        match move_place(db, project, &owner, place, &tip) {
            Ok(true) => moved.push(place.agent),
            Ok(false) => {}
            Err(error) => log("landing_agent_not_moved", &error),
        }
    }
    assert!(moved.len() <= MOVED_PLACES_MAX);
    moved
}

/// Moves the agent of a turn that just ended onto the tip of its Land on branch, as
/// [`move_place`] says, once its turn left nothing pending (DEC-147.6). Only a sub-agent's
/// linked worktree moves; anything else, or a hold not free within [`HOLD_WAIT`], is left.
/// Inputs: the store and the turn's workspace. Output: whether it moved.
pub(super) fn move_after_turn(db: &Connection, workspace: &Workspace) -> Result<bool, EngineError> {
    if workspace.agent.kind != AgentKind::Sub {
        return Ok(false);
    }
    let Ok(agent) = workspace.agent.id.parse::<i64>() else {
        return Ok(false);
    };
    let Some(_held) = hold_within(&workspace.repository, HOLD_WAIT) else {
        return Ok(false);
    };
    let Some((_, tip)) = owner::land_on_branch(db, &workspace.repository, &workspace.project)?
    else {
        return Ok(false);
    };
    let owner = Repo::open(
        &workspace.repository,
        &workspace.repository.join(".git"),
        None,
    )?;
    let place = Place {
        agent,
        repository: workspace.repository.clone(),
        worktree: workspace.worktree.clone(),
        git_dir: workspace.git_dir.clone(),
    };
    move_place(db, &workspace.project, &owner, &place, &tip)
}

/// Moves one sub-agent's linked worktree from its HEAD to `tip` when that is safe: it
/// holds no open change set, lands on the Land on branch rather than a lead's, holds
/// nothing but its HEAD, and its HEAD carries no work the tip lacks, being the commit its
/// next change set opens above, the head of a landed set, or already in the tip's history.
/// The move is [`flow::reset_agent`], which never writes over an edit, and the worktree's
/// next change set then opens above the tip. An own clone is never touched (CC-N1). The
/// caller holds the repository's hold. Inputs: the store, the project, the owner
/// repository, the place and the tip. Output: whether it moved.
fn move_place(
    db: &Connection,
    project: &ProjectId,
    owner: &Repo,
    place: &Place,
    tip: &str,
) -> Result<bool, EngineError> {
    assert!(super::git::is_object_id(tip));
    if flow::check_places(&place.repository, &place.worktree, &place.git_dir)? != Layout::Linked {
        return Ok(false);
    }
    let worktree = place
        .worktree
        .to_str()
        .ok_or_else(|| failure("changes_workspace_invalid"))?;
    if store::open_for(db, worktree)?.is_some() || leads_elsewhere(db, project, place)? {
        return Ok(false);
    }
    let agent = flow::open_agent(
        db,
        AgentKind::Sub,
        &place.repository,
        &place.worktree,
        &place.git_dir,
    )?;
    let head = agent.head()?;
    if head == tip {
        return Ok(false);
    }
    if !carries_only_landed(db, project, owner, place, worktree, &head, tip)?
        || !flow::holds_only(&agent.repo, &head)?
    {
        return Ok(false);
    }
    flow::verify_objects(&place.repository, &[&head], &[tip])?;
    if !flow::reset_agent(&agent, &head, tip)? {
        return Ok(false);
    }
    flow::set_base(db, &place.repository, &place.worktree, &place.git_dir, tip)?;
    Ok(true)
}

/// Tells whether a worktree's HEAD carries only work its Land on branch already has: it is
/// the commit the worktree's next change set opens above, the head of a set the engine
/// landed, or an ancestor of the tip. Inputs: the store, the project, the owner repository,
/// the place, its path text, the HEAD and the tip. Output: the answer.
fn carries_only_landed(
    db: &Connection,
    project: &ProjectId,
    owner: &Repo,
    place: &Place,
    worktree: &str,
    head: &str,
    tip: &str,
) -> Result<bool, EngineError> {
    if store::base_of(db, worktree)?.as_deref() == Some(head) {
        return Ok(true);
    }
    if store::landed_head(db, project, &place.repository, head)?.is_some() {
        return Ok(true);
    }
    owner.test(&["merge-base", "--is-ancestor", head, tip])
}

/// Tells whether an agent lands on its lead's branch while the owner's Lands setting is
/// Lead, so the Land on tip is not where it moves. Inputs: the store, the project and the
/// place. Output: the answer.
fn leads_elsewhere(
    db: &Connection,
    project: &ProjectId,
    place: &Place,
) -> Result<bool, EngineError> {
    let agent = ChangeAgent {
        kind: AgentKind::Sub,
        id: place.agent.to_string(),
        label: String::new(),
    };
    Ok(subagent::lead_of(db, project, &agent, &place.repository)?.is_some())
}

/// Tells a sub-agent working in its own clone, as its next task is handed over, that main
/// moved on (DEC-147.6, CC-N1): when it holds no open change set, does not land on a
/// lead's branch, and the commit its next change set opens above is not the Land on tip
/// nor the head whose landing [`owner::tell_landed`] already named with that tip. The
/// clone is given the tip as `crazyagent/main` first, then one Note keyed by agent and
/// tip. Inputs: the store, the project and the agent. Output: whether it was told.
pub(super) fn tell_main_moved(
    db: &Connection,
    project: &ProjectId,
    agent: i64,
) -> Result<bool, EngineError> {
    let Some(place) = place_of(db, project, agent)? else {
        return Ok(false);
    };
    if flow::check_places(&place.repository, &place.worktree, &place.git_dir)? != Layout::Clone {
        return Ok(false);
    }
    let worktree = place
        .worktree
        .to_str()
        .ok_or_else(|| failure("changes_workspace_invalid"))?;
    if store::open_for(db, worktree)?.is_some() || leads_elsewhere(db, project, &place)? {
        return Ok(false);
    }
    let Some(base) = store::base_of(db, worktree)? else {
        return Ok(false);
    };
    let Some(_held) = hold_within(&place.repository, HOLD_WAIT) else {
        return Ok(false);
    };
    let Some((_, tip)) = owner::land_on_branch(db, &place.repository, project)? else {
        return Ok(false);
    };
    if base == tip
        || store::landed_head(db, project, &place.repository, &base)?.as_deref() == Some(&tip)
    {
        return Ok(false);
    }
    clone::give_main(&place.repository, &place.worktree, &tip)?;
    let note = crate::coord::NewMessage {
        project: project.clone(),
        from: crate::coord::Party::Engine,
        to: crate::coord::Party::Agent(agent),
        kind: crate::coord::MessageKind::Note,
        question: None,
        work: None,
        reply_to: None,
        body: main_moved_text(&tip, &base),
        idempotency_key: Some(format!("main-moved-{agent}-{}", short(&tip))),
    };
    crate::coord::send(db, note, crate::team::now_ms())?;
    Ok(true)
}

/// The Note an own-clone sub-agent gets when main moved on (DEC-147.6, leader approved): the
/// tip, and the commit its next work opens above, so `git log <base>..HEAD` tells whether it
/// has commits of its own, under Squash too. Inputs: the tip and the base. Output: the text.
pub(super) fn main_moved_text(tip: &str, base: &str) -> String {
    format!(
        "Main moved on to {}. If git log {}..HEAD shows no commits, start your next work from main: git reset --keep crazyagent/main.",
        short(tip),
        short(base)
    )
}

/// The first 12 characters of a commit id. Input: the id. Output: the short id.
fn short(id: &str) -> &str {
    id.get(..12).unwrap_or(id)
}

/// The worktree the team records for one sub-agent of a project. Inputs: the store, the
/// project and the agent. Output: the place, None when there is none or no team tables.
fn place_of(
    db: &Connection,
    project: &ProjectId,
    agent: i64,
) -> Result<Option<Place>, EngineError> {
    if !team_tables(db)? {
        return Ok(None);
    }
    db.query_row(
        "SELECT w.agent,w.repository,w.worktree,w.git_dir FROM team_worktrees w
         JOIN team_agents a ON a.id=w.agent AND a.project=w.project
         WHERE w.agent=?1 AND w.project=?2 AND a.role='sub'",
        params![agent, project.as_str()],
        place_row,
    )
    .optional()
    .map_err(|_| failure("changes_unavailable"))
}

/// The worktrees of a project's resting sub-agents in one repository, idle, stopped or
/// failed, at most [`MOVED_PLACES_MAX`]. Inputs: the store, the project and the repository.
/// Output: the places, none for a store with no team tables.
fn resting(
    db: &Connection,
    project: &ProjectId,
    repository: &Path,
) -> Result<Vec<Place>, EngineError> {
    if !team_tables(db)? {
        return Ok(Vec::new());
    }
    let unavailable = |_| failure("changes_unavailable");
    let repository = repository
        .to_str()
        .ok_or_else(|| failure("changes_workspace_invalid"))?;
    let limit = i64::try_from(MOVED_PLACES_MAX).map_err(|_| failure("changes_unavailable"))?;
    let mut statement = db
        .prepare(
            "SELECT w.agent,w.repository,w.worktree,w.git_dir FROM team_worktrees w
             JOIN team_agents a ON a.id=w.agent AND a.project=w.project
             WHERE w.project=?1 AND w.repository=?2 AND a.role='sub'
             AND a.status IN ('idle','stopped','failed') ORDER BY w.agent LIMIT ?3",
        )
        .map_err(unavailable)?;
    let places = statement
        .query_map(params![project.as_str(), repository, limit], place_row)
        .map_err(unavailable)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(unavailable)?;
    assert!(places.len() <= MOVED_PLACES_MAX);
    Ok(places)
}

/// Reads one place from a row of agent, repository, worktree and git folder. Input: the
/// row. Output: the place.
fn place_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Place> {
    Ok(Place {
        agent: row.get(0)?,
        repository: PathBuf::from(row.get::<_, String>(1)?),
        worktree: PathBuf::from(row.get::<_, String>(2)?),
        git_dir: PathBuf::from(row.get::<_, String>(3)?),
    })
}

/// Tells whether the store keeps the team tables the moves read. Input: the store.
/// Output: the answer.
fn team_tables(db: &Connection) -> Result<bool, EngineError> {
    db.query_row(
        "SELECT COUNT(*)=2 FROM sqlite_schema WHERE type='table'
         AND name IN ('team_worktrees','team_agents')",
        [],
        |row| row.get(0),
    )
    .map_err(|_| failure("changes_unavailable"))
}

/// Logs a move step that failed by its plain code. Inputs: the event and the failure.
fn log(event: &str, error: &EngineError) {
    eprintln!("{event} code={}", crate::protocol::plain_code(&error.0));
}
