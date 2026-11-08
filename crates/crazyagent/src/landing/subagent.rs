use super::flow::{
    agent_branch, check_places, clone_branch, hold, now_ms, pending_ref, set_engine_ref, shared,
    tip,
};
use super::git::{failure, object_id};
use super::{ChangeState, LeadGit, MergeOutcome, clone, store};
use crate::agent_git::{self, Layout};
use crate::engine::EngineError;
use crate::protocol::ProjectId;
use crate::research::runner::AgentGit;
use rusqlite::{Connection, OptionalExtension, params};
use std::path::{Path, PathBuf};

/// The most sub-agents of one lead the engine reads in one call, past the team's
/// running-agent limit, so a full team is never cut short.
const SUB_AGENTS_MAX: usize = 64;
const _: () = assert!(SUB_AGENTS_MAX >= crate::terminal::PANES_PER_PROJECT_MAX);

/// The longest sub-agent name a branch keeps, leaving room for a `-64` suffix inside the
/// 48 bytes `agent_git::teammate_branch` keeps of a name.
const NAME_BYTES_MAX: usize = 40;
const _: () = assert!(NAME_BYTES_MAX + 3 <= 48);
/// The most names one claim tries: the name, then `-2` to `-64`.
const SUFFIXES_MAX: usize = 64;
const LISTING_BYTES_MAX: usize = 4096;
const ID_BYTES_MAX: usize = 256;
const CONFLICTS_BYTES_MAX: usize = 64 * 1024;
const CONFLICTS_SHOWN_MAX: usize = 50;
/// The most levels of workers below one lead whose change sets close with the lead's.
const WORKER_LEVELS_MAX: u8 = 3;
const _: () = assert!(WORKER_LEVELS_MAX >= crate::team::DEPTH_CHOICES[2]);

fn unavailable(_: impl std::fmt::Display) -> EngineError {
    failure("changes_unavailable")
}

/// Where one team agent works, as the engine recorded it when it made the worktree: the
/// owner's checkout root, the worktree and its git metadata folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Place {
    pub(super) repository: PathBuf,
    pub(super) worktree: PathBuf,
    pub(super) git_dir: PathBuf,
}

/// Reads where one team agent of a project works. Inputs: the engine database, the project
/// and the agent. Output: the place, None for an agent with no worktree recorded now.
pub(super) fn place(
    db: &Connection,
    project: &ProjectId,
    agent: i64,
) -> Result<Option<Place>, EngineError> {
    db.query_row(
        "SELECT repository,worktree,git_dir FROM team_worktrees WHERE agent=?1 AND project=?2",
        params![agent, project.as_str()],
        |row| {
            Ok(Place {
                repository: PathBuf::from(row.get::<_, String>(0)?),
                worktree: PathBuf::from(row.get::<_, String>(1)?),
                git_dir: PathBuf::from(row.get::<_, String>(2)?),
            })
        },
    )
    .optional()
    .map_err(unavailable)
}

/// Reads the sub-agents one lead started that still have a worktree, oldest first, each
/// with its id and place. Inputs: the engine database, the project and the lead. Output:
/// at most `SUB_AGENTS_MAX` of them, or `changes_too_large` for more.
pub(super) fn sub_agents(
    db: &Connection,
    project: &ProjectId,
    lead: i64,
) -> Result<Vec<(i64, Place)>, EngineError> {
    let mut statement = db
        .prepare(
            "SELECT a.id,w.repository,w.worktree,w.git_dir FROM team_agents a
             JOIN team_worktrees w ON w.agent=a.id AND w.project=a.project
             WHERE a.project=?1 AND a.parent=?2 AND a.role='sub' ORDER BY a.id LIMIT ?3",
        )
        .map_err(unavailable)?;
    let limit = i64::try_from(SUB_AGENTS_MAX + 1).map_err(unavailable)?;
    let found: Vec<(i64, Place)> = statement
        .query_map(params![project.as_str(), lead, limit], |row| {
            Ok((
                row.get(0)?,
                Place {
                    repository: PathBuf::from(row.get::<_, String>(1)?),
                    worktree: PathBuf::from(row.get::<_, String>(2)?),
                    git_dir: PathBuf::from(row.get::<_, String>(3)?),
                },
            ))
        })
        .map_err(unavailable)?
        .collect::<Result<_, _>>()
        .map_err(unavailable)?;
    if found.len() > SUB_AGENTS_MAX {
        return Err(failure("changes_too_large"));
    }
    Ok(found)
}

/// Closes the open change sets of a lead's sub-agents whose work the task tip of the
/// lead's own clone already holds, however it got there: a merge the lead ran by hand
/// after a conflict counts as the engine's merge does (F3). Each lands as the lead's tip.
/// A lead in a linked worktree never merges by hand, so its workers are left as they are.
/// Inputs: the store, the project and the lead. Output: Ok, or the failure that kept the
/// lead's tip from being read.
pub(super) fn settle(db: &Connection, project: &ProjectId, lead: i64) -> Result<(), EngineError> {
    store::initialize(db)?;
    let mut open = Vec::new();
    for (_, sub) in sub_agents(db, project, lead)? {
        if store::open_for(db, path_text(&sub.worktree)?)?.is_some() {
            open.push(sub);
        }
    }
    if open.is_empty() {
        return Ok(());
    }
    let Some(lead_place) = place(db, project, lead)? else {
        return Ok(());
    };
    let layout = check_places(
        &lead_place.repository,
        &lead_place.worktree,
        &lead_place.git_dir,
    )?;
    if layout != Layout::Clone {
        return Ok(());
    }
    let lead_tip = tip(
        db,
        &lead_place.repository,
        &lead_place.worktree,
        &lead_place.git_dir,
    )?;
    for sub in open
        .iter()
        .filter(|sub| sub.repository == lead_place.repository)
    {
        close_landed(db, sub, &lead_tip, &lead_tip)?;
    }
    Ok(())
}

/// Closes the open change sets of a lead's workers with the lead's own closed change set
/// (DEC-107): when the lead's set lands, a worker set whose work it holds lands with it as
/// Accepted, even when the lead squashed or resolved it by hand (F3); while the owner's
/// Lands setting is Lead, every other worker set opened before the lead's set closed is
/// discarded with it, restorable as an owner's Discard keeps one, since only that lead
/// would have merged it. A worker's own workers close with it the same way. The caller
/// holds [`hold`] on the closed set's repository. Inputs: the store and the lead's closed
/// change set. Output: Ok, or the first failure.
pub(super) fn close_with_lead(
    db: &Connection,
    lead_set: &store::Record,
) -> Result<(), EngineError> {
    close_workers(db, lead_set, WORKER_LEVELS_MAX)
}

/// Closes the workers' sets of one closed change set as [`close_with_lead`] says, then
/// their own workers', at most `levels` deep. Inputs: the store, the closed change set and
/// the levels left. Output: Ok, or the first failure.
fn close_workers(db: &Connection, lead_set: &store::Record, levels: u8) -> Result<(), EngineError> {
    assert!(!lead_set.is_open());
    if levels == 0 || lead_set.agent.kind != super::AgentKind::Sub {
        return Ok(());
    }
    let Ok(lead) = lead_set.agent.id.parse::<i64>() else {
        return Ok(());
    };
    let orphans = leads_merge(db, &lead_set.project)?;
    for (_, sub) in sub_agents(db, &lead_set.project, lead)? {
        if sub.repository != lead_set.repository {
            continue;
        }
        let Some(mut record) = store::open_for(db, path_text(&sub.worktree)?)? else {
            continue;
        };
        if record.intent.is_some() || record.created_ms > lead_set.updated_ms {
            continue;
        }
        let landed = match (lead_set.state, lead_set.landed.as_deref()) {
            (ChangeState::Accepted, Some(landed)) => Some(landed),
            _ => None,
        };
        match landed {
            Some(landed) if covers(&record, &lead_set.head)? => {
                land_covered(db, &mut record, landed)?
            }
            _ if orphans => {
                super::flow::discard_held(db, &mut record, lead_set.updated_ms)?;
            }
            _ => continue,
        }
        close_workers(db, &record, levels - 1)?;
    }
    Ok(())
}

/// Closes the open sets of lead workers whose lead's newest change set closed after they
/// opened, while the lead has none open, as [`close_with_lead`] would have when it closed:
/// sets left open before the engine closed workers with their lead. The caller holds
/// [`hold`] on their repository. Inputs: the store and the open change sets of one
/// repository. Output: Ok, or the first failure.
pub(super) fn close_orphaned(
    db: &Connection,
    ids: &[super::ChangeSetId],
) -> Result<(), EngineError> {
    if !team_kept(db)? {
        return Ok(());
    }
    let mut leads = Vec::new();
    for id in ids {
        let record = store::load(db, id)?;
        if !record.is_open() || record.agent.kind != super::AgentKind::Sub {
            continue;
        }
        let Ok(agent) = record.agent.id.parse::<i64>() else {
            continue;
        };
        let lead: Option<i64> = db
            .query_row(
                "SELECT parent FROM team_agents WHERE id=?1 AND project=?2 AND role='sub'",
                params![agent, record.project.as_str()],
                |row| row.get(0),
            )
            .optional()
            .map_err(unavailable)?
            .flatten();
        if let Some(lead) = lead
            && !leads.contains(&(record.project.clone(), lead))
        {
            leads.push((record.project.clone(), lead));
        }
    }
    for (project, lead) in leads {
        let Some(lead_set) = store::newest_of(db, &project, &lead.to_string())? else {
            continue;
        };
        if !lead_set.is_open() && lead_set.intent.is_none() {
            close_workers(db, &lead_set, WORKER_LEVELS_MAX)?;
        }
    }
    Ok(())
}

/// The lead whose branch a change set's agent lands on while the owner's Lands setting is
/// Lead (DEC-107), with the place the lead works, read from the store only; a native
/// teammate whose turns the engine commits is its lead's worker too (OP9B-L3). Inputs: the
/// store, the project, the change set's agent and its repository. Output: the lead and its
/// place, None for an agent that is not a lead's worker or while Lands is Owner, or
/// `changes_workspace_invalid` for a lead's worker whose lead has no worktree in that
/// repository now, which is never measured against the owner's branch instead.
pub(super) fn lead_of(
    db: &Connection,
    project: &ProjectId,
    agent: &super::ChangeAgent,
    repository: &Path,
) -> Result<Option<(i64, Place)>, EngineError> {
    if agent.kind != super::AgentKind::Sub || !leads_merge(db, project)? {
        return Ok(None);
    }
    let Ok(id) = agent.id.parse::<i64>() else {
        return Ok(None);
    };
    let lead: Option<i64> = db
        .query_row(
            "SELECT parent FROM team_agents WHERE id=?1 AND project=?2 AND role IN ('sub','teammate')",
            params![id, project.as_str()],
            |row| row.get(0),
        )
        .optional()
        .map_err(unavailable)?
        .flatten();
    let Some(lead) = lead else {
        return Ok(None);
    };
    match place(db, project, lead)? {
        Some(lead_place) if lead_place.repository == repository => Ok(Some((lead, lead_place))),
        _ => Err(failure("changes_workspace_invalid")),
    }
}

/// The tip of the branch a lead's worker lands on while the owner's Lands setting is Lead
/// (DEC-107): its lead's branch, read from the lead's linked worktree or fetched from the
/// lead's own clone with the engine's checked fetch. It never takes the repository hold,
/// so a caller reads it before taking the hold. Inputs: the store and the lead's place.
/// Output: the tip.
pub(super) fn read_lead_tip(db: &Connection, lead: &Place) -> Result<String, EngineError> {
    super::flow::read_tip(db, &lead.repository, &lead.worktree, &lead.git_dir)
}

/// The tip a change set's agent lands on when it is a lead's worker, as [`lead_of`] finds
/// the lead and [`read_lead_tip`] reads its branch. Inputs: the store, the project, the
/// change set's agent and its repository. Output: the tip, None for an agent that is not a
/// lead's worker or while Lands is Owner, or the failure that kept the lead's tip unread.
pub(super) fn lead_tip(
    db: &Connection,
    project: &ProjectId,
    agent: &super::ChangeAgent,
    repository: &Path,
) -> Result<Option<String>, EngineError> {
    match lead_of(db, project, agent, repository)? {
        Some((_, lead)) => read_lead_tip(db, &lead).map(Some),
        None => Ok(None),
    }
}

/// The name of the branch a lead works on, for the text its workers read about it, read
/// without git. Inputs: the store and the lead's place. Output: the branch, None when its
/// place or name cannot be read.
pub(super) fn lead_branch(db: &Connection, lead: &Place) -> Option<String> {
    let layout = check_places(&lead.repository, &lead.worktree, &lead.git_dir).ok()?;
    super::flow::agent_branch(db, &lead.worktree, layout)
        .ok()
        .flatten()
}

/// Tells whether a project's lead merges its workers' change sets, so the owner's lists
/// leave them out: the owner's Lands setting is Lead. Inputs: the store and the project.
/// Output: the answer, false where the engine keeps no team tables.
pub(super) fn leads_merge(db: &Connection, project: &ProjectId) -> Result<bool, EngineError> {
    if !team_kept(db)? {
        return Ok(false);
    }
    Ok(crate::team::nesting(db, project)?.lands == crate::team::ChildLanding::Lead)
}

/// Tells whether the store holds the team tables the engine keeps beside change sets.
/// Input: the store. Output: the answer.
fn team_kept(db: &Connection) -> Result<bool, EngineError> {
    let tables: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM sqlite_schema WHERE type='table' AND name IN ('team_agents','team_nesting')",
            [],
            |row| row.get(0),
        )
        .map_err(unavailable)?;
    Ok(tables == 2)
}

/// Tells whether a change set's agent is a native teammate, whose work its lead lands and
/// which takes no resolve task of its own (OP9B-L3). Inputs: the store, the project and the
/// change set's agent. Output: the answer, false where the engine keeps no team tables.
pub(super) fn is_teammate(
    db: &Connection,
    project: &ProjectId,
    agent: &super::ChangeAgent,
) -> Result<bool, EngineError> {
    if agent.kind != super::AgentKind::Sub || !team_kept(db)? {
        return Ok(false);
    }
    let Ok(id) = agent.id.parse::<i64>() else {
        return Ok(false);
    };
    db.query_row(
        "SELECT EXISTS(SELECT 1 FROM team_agents WHERE id=?1 AND project=?2 AND role='teammate')",
        params![id, project.as_str()],
        |row| row.get(0),
    )
    .map_err(unavailable)
}

/// Tells whether a change set's agent is a worker of a lead. Inputs: the store, the
/// project and the change set's agent. Output: the answer.
pub(super) fn is_lead_worker(
    db: &Connection,
    project: &ProjectId,
    agent: &super::ChangeAgent,
) -> Result<bool, EngineError> {
    let Ok(id) = agent.id.parse::<i64>() else {
        return Ok(false);
    };
    if agent.kind != super::AgentKind::Sub {
        return Ok(false);
    }
    db.query_row(
        "SELECT EXISTS(SELECT 1 FROM team_agents WHERE id=?1 AND project=?2 AND role='sub' AND parent IS NOT NULL)",
        params![id, project.as_str()],
        |row| row.get(0),
    )
    .map_err(unavailable)
}

/// Lists the branches of a lead's sub-agents whose work is not yet in the lead's task
/// branch (DEC-104 K5b). For a lead in its own clone, a sub-agent's branch counts as
/// merged once its tip, read through the engine's checked fetch, is an ancestor of the
/// lead's tip. For a lead in a linked worktree, whose workers land as squash commits, it
/// counts once the sub-agent has no open change set. No git runs in any agent's clone.
/// Inputs: the engine database, the project and the lead. Output: the unmerged branches in
/// sub-agent order, or `changes_workspace_invalid` for a lead with no worktree or branch.
pub(super) fn unmerged(
    db: &Connection,
    project: &ProjectId,
    lead: i64,
) -> Result<Vec<String>, EngineError> {
    store::initialize(db)?;
    let lead_place =
        place(db, project, lead)?.ok_or_else(|| failure("changes_workspace_invalid"))?;
    let subs = sub_agents(db, project, lead)?;
    if subs.is_empty() {
        return Ok(Vec::new());
    }
    let layout = check_places(
        &lead_place.repository,
        &lead_place.worktree,
        &lead_place.git_dir,
    )?;
    let lead_tip = match layout {
        Layout::Clone => Some(tip(
            db,
            &lead_place.repository,
            &lead_place.worktree,
            &lead_place.git_dir,
        )?),
        Layout::Linked => None,
    };
    let repository = shared(&lead_place.repository)?;
    let mut unmerged = Vec::new();
    for (_, sub) in &subs {
        if sub.repository != lead_place.repository {
            return Err(failure("changes_workspace_invalid"));
        }
        let branch = super::agent_branch(db, &sub.worktree, &sub.git_dir)?
            .ok_or_else(|| failure("changes_workspace_invalid"))?;
        let landed = match &lead_tip {
            Some(lead_tip) => {
                let sub_tip = tip(db, &sub.repository, &sub.worktree, &sub.git_dir)?;
                repository.test(&["merge-base", "--is-ancestor", &sub_tip, lead_tip])?
            }
            None => store::open_for(db, path_text(&sub.worktree)?)?.is_none(),
        };
        if !landed {
            unmerged.push(branch);
        }
    }
    assert!(unmerged.len() <= subs.len());
    Ok(unmerged)
}

/// The text of a path the engine recorded, which is always UTF-8. Input: the path.
/// Output: the text, or `changes_workspace_invalid`.
pub(super) fn path_text(path: &Path) -> Result<&str, EngineError> {
    path.to_str()
        .ok_or_else(|| failure("changes_workspace_invalid"))
}

/// Names and stores the branch a new sub-agent of a lead works on (DEC-104 K5b),
/// `<lead's task branch>--<name>`, or its first free `-2` to `-64` form when another agent
/// worktree, an owner branch or a branch in the lead's own clone already holds it, so its
/// clone is then made on it. The repository hold keeps two launches from taking one name.
/// Inputs: the store, the owner checkout root, the new worktree, the lead's task branch,
/// the lead's clone (None for a linked worktree) and the name, lower-case letters and
/// digits joined by single hyphens. Output: the branch, None when the lead's branch cannot
/// carry sub-agent branches, or `changes_agent_branch_exists` when every form is taken.
pub(super) fn claim(
    db: &Connection,
    repository: &Path,
    worktree: &Path,
    task: &str,
    lead_clone: Option<&Path>,
    name: &str,
) -> Result<Option<String>, EngineError> {
    store::initialize(db)?;
    let base: String = name.chars().take(NAME_BYTES_MAX).collect();
    let base = base.trim_end_matches('-');
    let carries = |candidate: &str| {
        agent_git::teammate_branch(task, candidate)
            .filter(|branch| agent_git::is_agent_branch(branch))
    };
    if !agent_git::is_agent_branch(task) || carries(base).is_none() {
        return Ok(None);
    }
    let _held = hold(repository);
    let shared = shared(repository)?;
    for suffix in 1..=SUFFIXES_MAX {
        let candidate = match suffix {
            1 => base.to_owned(),
            _ => format!("{base}-{suffix}"),
        };
        let branch = carries(&candidate).ok_or_else(|| failure("changes_agent_branch_invalid"))?;
        let reference = format!("refs/heads/{branch}");
        let nested = shared.capture(
            &[
                "for-each-ref",
                "--count=1",
                "--format=%(refname)",
                &format!("{reference}/"),
            ],
            LISTING_BYTES_MAX,
        )?;
        let in_lead = match lead_clone {
            Some(clone) => clone::has_branch(clone, &branch)?,
            None => false,
        };
        if in_lead
            || !nested.is_empty()
            || store::agent_branch_taken(db, repository, worktree, &branch)?
            || shared.commit_of(&reference)?.is_some()
        {
            continue;
        }
        store::set_agent_branch(db, repository, worktree, &branch)?;
        return Ok(Some(branch));
    }
    Err(failure("changes_agent_branch_exists"))
}

/// Merges one sub-agent's branch into its lead's task branch inside the lead's own clone
/// (DEC-104 K5b, Q32). The sub-agent's tip is read through the engine's checked fetch, so
/// its objects are checked and reach the lead through the owner objects its clone borrows.
/// Every git step in the lead's clone runs with the lead's own rights, under its profile:
/// the branch `<task>--<name>` is created or moved forward there, then merged with
/// `--no-ff`; a conflicting merge is aborted, leaving that branch for the lead to merge by
/// hand. No git runs in the sub-agent's clone. Inputs: the engine database, the project,
/// the lead, the sub-agent and the lead's git. Output: what the merge did, or a plain
/// `changes_*` refusal the lead can act on.
pub(super) fn merge(
    db: &Connection,
    project: &ProjectId,
    lead: i64,
    sub: i64,
    git: &LeadGit<'_>,
) -> Result<MergeOutcome, EngineError> {
    store::initialize(db)?;
    let invalid = || failure("changes_workspace_invalid");
    let parent: Option<Option<i64>> = db
        .query_row(
            "SELECT parent FROM team_agents WHERE id=?1 AND project=?2 AND role='sub'",
            params![sub, project.as_str()],
            |row| row.get(0),
        )
        .optional()
        .map_err(unavailable)?;
    if parent.flatten() != Some(lead) {
        return Err(failure("changes_not_your_sub_agent"));
    }
    let lead_place = place(db, project, lead)?.ok_or_else(invalid)?;
    let sub_place = place(db, project, sub)?.ok_or_else(invalid)?;
    if sub_place.repository != lead_place.repository {
        return Err(invalid());
    }
    let lead_layout = check_places(
        &lead_place.repository,
        &lead_place.worktree,
        &lead_place.git_dir,
    )?;
    if lead_layout != Layout::Clone {
        return Err(failure("changes_lead_not_own_clone"));
    }
    check_places(
        &sub_place.repository,
        &sub_place.worktree,
        &sub_place.git_dir,
    )?;
    let task =
        super::agent_branch(db, &lead_place.worktree, &lead_place.git_dir)?.ok_or_else(invalid)?;
    let own =
        super::agent_branch(db, &sub_place.worktree, &sub_place.git_dir)?.ok_or_else(invalid)?;
    let branch = if agent_git::is_teammate_branch(&task, &own) {
        own
    } else {
        let folder = sub_place
            .worktree
            .file_name()
            .and_then(std::ffi::OsStr::to_str)
            .ok_or_else(invalid)?;
        agent_git::teammate_branch(&task, folder).ok_or_else(invalid)?
    };
    let sub_tip = tip(
        db,
        &sub_place.repository,
        &sub_place.worktree,
        &sub_place.git_dir,
    )?;
    let lead_tip = tip(
        db,
        &lead_place.repository,
        &lead_place.worktree,
        &lead_place.git_dir,
    )?;
    let shared = shared(&lead_place.repository)?;
    if shared.test(&["merge-base", "--is-ancestor", &sub_tip, &lead_tip])? {
        close_landed(db, &sub_place, &sub_tip, &lead_tip)?;
        return Ok(MergeOutcome::AlreadyMerged { branch });
    }
    let home = crate::projects_home::owner_home()?;
    let (name, email) = super::owner_git_identity(&lead_place.repository, &home)?;
    let identity = [
        ("GIT_AUTHOR_NAME", name.as_str()),
        ("GIT_AUTHOR_EMAIL", email.as_str()),
        ("GIT_COMMITTER_NAME", name.as_str()),
        ("GIT_COMMITTER_EMAIL", email.as_str()),
    ];
    let run = |arguments: &[&str], limit: usize| -> Result<AgentGit, EngineError> {
        crate::research::runner::agent_git(
            git.sandbox,
            git.profile,
            &lead_place.worktree,
            arguments,
            &identity,
            limit,
        )
        .map_err(|_| failure("changes_git_failed"))
    };
    let ran = |output: &AgentGit| output.code == Some(0) && !output.overflow;
    let head = run(&["symbolic-ref", "-q", "HEAD"], ID_BYTES_MAX)?;
    if !ran(&head) || head.stdout != format!("refs/heads/{task}\n").as_bytes() {
        return Err(failure("changes_lead_off_task_branch"));
    }
    if run(&["rev-parse", "-q", "--verify", "MERGE_HEAD"], ID_BYTES_MAX)?.code == Some(0) {
        return Err(failure("changes_git_busy"));
    }
    let status = run(
        &[
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=no",
            "--ignore-submodules=all",
        ],
        LISTING_BYTES_MAX,
    )?;
    if status.code != Some(0) {
        return Err(failure("changes_git_failed"));
    }
    if status.overflow || !status.stdout.is_empty() {
        return Err(failure("changes_lead_uncommitted"));
    }
    let reference = format!("refs/heads/{branch}");
    let held = run(
        &[
            "rev-parse",
            "-q",
            "--verify",
            "--end-of-options",
            &format!("{reference}^{{commit}}"),
        ],
        ID_BYTES_MAX,
    )?;
    let old = match held.code {
        Some(0) if !held.overflow => Some(object_id(&held.stdout)?),
        Some(1) => None,
        _ => return Err(failure("changes_git_failed")),
    };
    let expected = match &old {
        Some(old) if *old == sub_tip => None,
        Some(old) => {
            let behind = shared.commit_of(old)?.is_some()
                && shared.test(&["merge-base", "--is-ancestor", old, &sub_tip])?;
            if !behind {
                return Err(failure("changes_agent_branch_exists"));
            }
            Some(old.as_str())
        }
        None => Some(""),
    };
    if let Some(expected) = expected {
        let moved = run(
            &[
                "update-ref",
                "--no-deref",
                "-m",
                "CrazyAgent: sub-agent branch",
                &reference,
                &sub_tip,
                expected,
            ],
            ID_BYTES_MAX,
        )?;
        if !ran(&moved) {
            return Err(failure("changes_git_failed"));
        }
    }
    let message = format!("Merge {branch} into {task}");
    let merged = run(
        &[
            "merge",
            "--no-ff",
            "--no-edit",
            "--no-verify",
            "--no-gpg-sign",
            "--no-autostash",
            "--no-stat",
            "--quiet",
            "-m",
            &message,
            &reference,
        ],
        LISTING_BYTES_MAX,
    )?;
    if merged.code == Some(0) {
        let head = run(&["rev-parse", "--verify", "HEAD^{commit}"], ID_BYTES_MAX)?;
        if !ran(&head) {
            return Err(failure("changes_git_failed"));
        }
        let commit = object_id(&head.stdout)?;
        assert_ne!(commit, sub_tip);
        close_landed(db, &sub_place, &sub_tip, &commit)?;
        return Ok(MergeOutcome::Merged { commit, branch });
    }
    if run(&["rev-parse", "-q", "--verify", "MERGE_HEAD"], ID_BYTES_MAX)?.code != Some(0) {
        return Err(failure("changes_merge_refused"));
    }
    let listed = run(
        &["diff", "--name-only", "--diff-filter=U", "-z"],
        CONFLICTS_BYTES_MAX,
    )?;
    if !ran(&run(&["merge", "--abort"], ID_BYTES_MAX)?) {
        return Err(failure("changes_merge_abort_failed"));
    }
    let files: Vec<String> = listed
        .stdout
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .take(CONFLICTS_SHOWN_MAX)
        .map(|path| super::diff::plain(&String::from_utf8_lossy(path)))
        .collect();
    assert!(files.len() <= CONFLICTS_SHOWN_MAX);
    Ok(MergeOutcome::Conflict { branch, files })
}

/// Makes one native teammate's worktree in its lead's own clone, as
/// [`super::add_teammate_worktree`] says: nothing runs when the clone already records it on
/// the teammate's branch, and the record is read again after git. Inputs: the store, the
/// lead's worktree and git folder, the teammate's name and the lead's git. Output: Ok, or
/// a `changes_*` refusal.
pub(super) fn add_teammate(
    db: &Connection,
    worktree: &Path,
    git_dir: &Path,
    name: &str,
    git: &LeadGit<'_>,
) -> Result<(), EngineError> {
    let invalid = || failure("changes_workspace_invalid");
    if git_dir != agent_git::git_dir(worktree) {
        return Err(failure("changes_lead_not_own_clone"));
    }
    if !agent_git::worktree_name_valid(name) {
        return Err(invalid());
    }
    let task = clone_branch(db, worktree)?;
    let branch = agent_git::teammate_branch(&task, name).ok_or_else(invalid)?;
    let on_branch = || -> Result<bool, EngineError> {
        Ok(clone::teammates(worktree, &task)?.iter().any(|teammate| {
            teammate.folder.file_name() == Some(std::ffi::OsStr::new(name))
                && teammate.head == clone::TeammateHead::Branch(branch.clone())
        }))
    };
    if on_branch()? {
        return Ok(());
    }
    let run = |arguments: &[&str]| -> Result<AgentGit, EngineError> {
        crate::research::runner::agent_git(
            git.sandbox,
            git.profile,
            worktree,
            arguments,
            &[],
            ID_BYTES_MAX,
        )
        .map_err(|_| failure("changes_git_failed"))
    };
    let folder = format!("{}/{name}", agent_git::TEAMMATES_FOLDER);
    let held = run(&[
        "rev-parse",
        "-q",
        "--verify",
        "--end-of-options",
        &format!("refs/heads/{branch}^{{commit}}"),
    ])?;
    let added = match held.code {
        Some(0) => run(&["worktree", "add", "-q", &folder, &branch])?,
        Some(1) => run(&["worktree", "add", "-q", "-b", &branch, &folder, "HEAD"])?,
        _ => return Err(failure("changes_git_failed")),
    };
    if added.code != Some(0) || !on_branch()? {
        return Err(failure("changes_git_failed"));
    }
    Ok(())
}

/// Makes one native teammate's linked worktree inside its linked lead's worktree (OP-9b), as
/// [`super::add_teammate_worktree`] says: `<worktree>/.worktrees/<name>` as a worktree of
/// the project repository on `<task branch>--<name>`, a new branch cut from the lead's
/// HEAD or the one an earlier run left as it stands. Nothing runs when the repository
/// already records it on that branch, and the record is read again after git; the store
/// then keeps that branch as the teammate worktree's own, so its turns record there.
/// Inputs: the store, the owner checkout root, the lead's worktree and git folder, the
/// teammate's name and the lead's teammates' setup git. Output: Ok, or a `changes_*`
/// refusal.
pub(super) fn add_linked_teammate(
    db: &Connection,
    repository: &Path,
    worktree: &Path,
    git_dir: &Path,
    name: &str,
    git: &LeadGit<'_>,
) -> Result<(), EngineError> {
    let invalid = || failure("changes_workspace_invalid");
    if check_places(repository, worktree, git_dir)? != Layout::Linked {
        return Err(invalid());
    }
    if !agent_git::worktree_name_valid(name) {
        return Err(invalid());
    }
    let task = agent_branch(db, worktree, Layout::Linked)?.ok_or_else(invalid)?;
    let branch = agent_git::teammate_branch(&task, name)
        .filter(|branch| agent_git::is_agent_branch(branch))
        .ok_or_else(invalid)?;
    let folder = worktree.join(agent_git::TEAMMATES_FOLDER).join(name);
    let on_branch = || -> Result<bool, EngineError> {
        Ok(clone::linked_teammates(repository, worktree, &task)?
            .iter()
            .any(|teammate| {
                teammate.folder == folder
                    && teammate.head == clone::TeammateHead::Branch(branch.clone())
            }))
    };
    if on_branch()? {
        return store::set_agent_branch(db, repository, &folder, &branch);
    }
    let held = shared(repository)?.exec(
        &[
            "rev-parse",
            "-q",
            "--verify",
            "--end-of-options",
            &format!("refs/heads/{branch}^{{commit}}"),
        ],
        None,
        &[],
        256,
    )?;
    let base = match held.code {
        0 => None,
        1 => Some(tip(db, repository, worktree, git_dir)?),
        _ => return Err(failure("changes_git_failed")),
    };
    crate::research::runner::add_teammate_worktree_at(
        git.sandbox,
        git.profile,
        &folder,
        &branch,
        base.as_deref(),
    )
    .map_err(|_| failure("changes_git_failed"))?;
    if !on_branch()? {
        return Err(failure("changes_git_failed"));
    }
    store::set_agent_branch(db, repository, &folder, &branch)
}

/// Closes a sub-agent's open change set once its lead merged the work it holds, as
/// [`close_covered`] does, under the engine's hold on the repository. Inputs: the store,
/// the sub-agent's place, the tip that was merged and the lead's commit holding it.
/// Output: Ok, closed or left open.
fn close_landed(
    db: &Connection,
    sub: &Place,
    merged: &str,
    landed: &str,
) -> Result<(), EngineError> {
    let _held = hold(&sub.repository);
    close_covered(db, sub, merged, landed)
}

/// Closes a sub-agent's open change set whose head a commit holds, as an Accept closes one
/// it landed, so the owner's Changes no longer offers it: Accepted with `landed` as where
/// it landed, its pending ref dropped and the sub-agent's next change set opening above
/// it. A set with an owner action in flight, or holding work past `covering`, stays open.
/// The caller holds [`hold`] on the sub-agent's repository. Inputs: the store, the
/// sub-agent's place, the commit that holds its work and the commit it landed as.
fn close_covered(
    db: &Connection,
    sub: &Place,
    merged: &str,
    landed: &str,
) -> Result<(), EngineError> {
    let Some(mut record) = store::open_for(db, path_text(&sub.worktree)?)? else {
        return Ok(());
    };
    if record.intent.is_some() || !covers(&record, merged)? {
        return Ok(());
    }
    land_covered(db, &mut record, landed)
}

/// Tells whether a commit holds an open change set's head. Inputs: the change set and the
/// commit. Output: the answer.
pub(super) fn covers(record: &store::Record, merged: &str) -> Result<bool, EngineError> {
    if record.head == merged {
        return Ok(true);
    }
    let shared = shared(&record.repository)?;
    Ok(shared.commit_of(&record.head)?.is_some()
        && shared.test(&["merge-base", "--is-ancestor", &record.head, merged])?)
}

/// Closes an open change set whose work a commit holds as Accepted with `landed` as where
/// it landed, drops its pending ref and has the agent's next change set open above it.
/// The caller holds [`hold`] on its repository. Inputs: the store, the change set and the
/// commit it landed as. Output: Ok once closed.
pub(super) fn land_covered(
    db: &Connection,
    record: &mut store::Record,
    landed: &str,
) -> Result<(), EngineError> {
    assert!(record.is_open() && record.intent.is_none());
    let shared = shared(&record.repository)?;
    record.state = ChangeState::Accepted;
    record.landed = Some(landed.to_owned());
    record.conflicts.clear();
    record.waiting = false;
    record.updated_ms = now_ms()?;
    store::save(db, record)?;
    super::flow::release_agent_claims(db, record);
    set_engine_ref(&shared, &record.repository, &pending_ref(&record.id), None)?;
    store::set_base(db, &record.worktree, &record.head)?;
    assert!(!record.is_open());
    Ok(())
}
