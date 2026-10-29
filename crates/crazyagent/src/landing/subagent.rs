use super::flow::{check_places, hold, shared, tip};
use super::git::{failure, object_id};
use super::{LeadGit, MergeOutcome, clone, store};
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
