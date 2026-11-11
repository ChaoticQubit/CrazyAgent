use crate::engine::EngineError;
use crate::ledger::{self, Action, ActionKind, Outcome};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::path::{Component, Path, PathBuf};

const ENTRIES_SCANNED_MAX: usize = 1024;
const POINTER_BYTES_MAX: u64 = 4096;

/// What the ledger keeps about one team agent worktree the engine adds: the agent, the
/// owner's checkout root and the worktree, both absolute, and whether it is a notice: an
/// add a check could not settle, kept pending for the owner and never checked again.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WorktreeIntent {
    team_agent: i64,
    repository: PathBuf,
    worktree: PathBuf,
    #[serde(default)]
    unknown: bool,
}

/// What the ledger keeps about one team agent harness the engine starts: the agent,
/// whether it runs in a terminal pane a later engine can see or on pipes, and whether it
/// is a notice: a start a check could not settle, kept pending for the owner and never
/// checked again.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SpawnIntent {
    team_agent: i64,
    pane: bool,
    #[serde(default)]
    unknown: bool,
}

fn key(agent: i64) -> String {
    format!("team-{agent}")
}

fn unavailable(_: rusqlite::Error) -> EngineError {
    EngineError("ledger_unavailable".to_owned())
}

/// Records that the engine is about to add a team agent's worktree, before git runs, so a
/// stop part way leaves an intent the next start settles. Inputs: the engine root, the
/// agent, the owner's checkout root and the worktree, which must be the agent's own
/// `worktrees/agent-<id>` folder in it. Output: Ok, or the plain reason the worktree must
/// not be added, such as an earlier add of it still unsettled (`ledger_pending`).
pub(super) fn begin_worktree(
    root: &Path,
    agent: i64,
    repository: &Path,
    worktree: &Path,
) -> Result<(), String> {
    let intent = WorktreeIntent {
        team_agent: agent,
        repository: repository.to_path_buf(),
        worktree: worktree.to_path_buf(),
        unknown: false,
    };
    assert!(
        placed(&intent),
        "an agent worktree sits at worktrees/agent-<id> in its checkout"
    );
    begin(
        root,
        ActionKind::WorktreeCreate,
        agent,
        serde_json::to_string(&intent),
        [
            "the agent worktree could not be recorded",
            "An earlier add of this agent's worktree is still being checked; it settles at the next engine start",
            "An earlier add of this agent's worktree has an unknown outcome. Check its folder, then dismiss it in Recovery to start the agent again",
        ],
    )
}

/// Records that the engine is about to start a team agent's harness, before anything can
/// start it, so a stop part way leaves an intent the next start settles, and a second
/// start of the agent is refused while it is pending. Inputs: the engine root, the agent
/// and whether it runs in a pane. Output: Ok, or the plain reason it must not start.
pub(super) fn begin_spawn(root: &Path, agent: i64, pane: bool) -> Result<(), String> {
    let intent = SpawnIntent {
        team_agent: agent,
        pane,
        unknown: false,
    };
    begin(
        root,
        ActionKind::AgentSpawn,
        agent,
        serde_json::to_string(&intent),
        [
            "the agent's start could not be recorded",
            "An earlier start of this agent is still being checked; it settles once agent terminals are available",
            "An earlier start of this agent has an unknown outcome. Check the agent, then dismiss it in Recovery to start it again",
        ],
    )
}

/// Records one intent under the agent's key. Inputs: the engine root, the kind, the agent,
/// the payload, and the owner sentences for a ledger failure, for an earlier intent of the
/// agent a check still settles, and for an earlier one left as a notice the owner
/// dismisses. Output: Ok, or the sentence with its plain code.
fn begin(
    root: &Path,
    kind: ActionKind,
    agent: i64,
    payload: serde_json::Result<String>,
    [failed, checking, noticed]: [&str; 3],
) -> Result<(), String> {
    let payload = payload.map_err(|_| super::with_code(failed, Some("ledger_invalid")))?;
    let ledger =
        ledger::open(root).map_err(|problem| super::with_code(failed, Some(&problem.0)))?;
    let Err(problem) = ledger::begin(&ledger, kind, &key(agent), &payload) else {
        return Ok(());
    };
    if problem.0 != "ledger_pending" {
        return Err(super::with_code(failed, Some(&problem.0)));
    }
    let notice = ledger::pending(&ledger, kind).is_ok_and(|actions| {
        actions
            .iter()
            .any(|action| action.key == key(agent) && ledger::notice(action).is_some())
    });
    let sentence = if notice { noticed } else { checking };
    Err(super::with_code(sentence, Some(&problem.0)))
}

/// Ends the pending start of a team agent's harness: Done once its worker is ready (a
/// pane's record stored), Failed when its worker ended before that. An agent with no
/// pending start, such as one taken over after a restart, is left alone, and so is a
/// notice; a ledger that cannot be written logs its plain code. Inputs: the engine root,
/// the agent and outcome.
pub(super) fn spawned(root: &Path, agent: i64, outcome: Outcome) {
    logged(ledger::open(root).and_then(|ledger| finish_spawn(&ledger, agent, outcome)));
}

/// Settles what one launch left pending once it returns: the agent's worktree intent as
/// `settle_worktrees` does, and its start as Failed when no worker runs the agent.
/// Inputs: the team database, the engine root, the agent and whether a worker runs it.
pub(super) fn settle(team: &Connection, root: &Path, agent: i64, running: bool) {
    logged(ledger::open(root).and_then(|ledger| {
        settle_worktrees(&ledger, team, Some(agent))?;
        if running {
            Ok(())
        } else {
            finish_spawn(&ledger, agent, Outcome::Failed)
        }
    }));
}

/// Settles the intents an engine stop left pending that need no terminal: every agent
/// worktree intent as `settle_worktrees` does, and every start of a piped agent becomes a
/// notice, since no pid of it was kept and its harness may still run unseen. Pane starts
/// wait for `recover_panes`. Inputs: the team database and the engine root.
pub(super) fn recover(team: &Connection, root: &Path) {
    logged(ledger::open(root).and_then(|ledger| {
        settle_worktrees(&ledger, team, None)?;
        for action in ledger::pending(&ledger, ActionKind::AgentSpawn)? {
            if let Some(intent) = spawn_intent(&action).filter(|intent| !intent.pane) {
                notice(
                    &ledger,
                    &action,
                    &SpawnIntent {
                        unknown: true,
                        ..intent
                    },
                )?;
            }
        }
        Ok(())
    }));
}

/// Settles the pane starts an engine stop left pending against the stored pane records
/// and the live panes: Done when its record was stored, Failed when no pane is left, and a
/// notice when its pane is alive without one, which the caller then ends so it never runs
/// beside a second start. Inputs: the engine root, the records and the live panes.
pub(super) fn recover_panes<R, P>(
    root: &Path,
    records: &BTreeMap<i64, R>,
    alive: &BTreeMap<i64, P>,
) {
    logged(ledger::open(root).and_then(|ledger| {
        for action in ledger::pending(&ledger, ActionKind::AgentSpawn)? {
            let Some(intent) = spawn_intent(&action).filter(|intent| intent.pane) else {
                continue;
            };
            if records.contains_key(&intent.team_agent) {
                ledger::finish(&ledger, action.id, Outcome::Done)?;
            } else if alive.contains_key(&intent.team_agent) {
                notice(
                    &ledger,
                    &action,
                    &SpawnIntent {
                        unknown: true,
                        ..intent
                    },
                )?;
            } else {
                ledger::finish(&ledger, action.id, Outcome::Failed)?;
            }
        }
        Ok(())
    }));
}

/// Replaces a pending intent a check could not settle with a notice under the same kind
/// and key, in one transaction: the intent ends Unknown and the notice stays pending, so
/// the owner is shown it and the agent's next start is refused until the owner dismisses
/// it with `ledger::dismiss`. Inputs: the ledger, the intent and the notice to keep.
/// Output: Ok, or the ledger's failure, which leaves the intent pending as it was.
fn notice(ledger: &Connection, action: &Action, kept: &impl Serialize) -> Result<(), EngineError> {
    let payload =
        serde_json::to_string(kept).map_err(|_| EngineError("ledger_invalid".to_owned()))?;
    let held = ledger.unchecked_transaction().map_err(unavailable)?;
    ledger::finish(&held, action.id, Outcome::Unknown)?;
    ledger::begin(&held, action.kind, &action.key, &payload)?;
    held.commit().map_err(unavailable)
}

fn finish_spawn(ledger: &Connection, agent: i64, outcome: Outcome) -> Result<(), EngineError> {
    for action in ledger::pending(ledger, ActionKind::AgentSpawn)? {
        if spawn_intent(&action).is_some_and(|intent| intent.team_agent == agent) {
            ledger::finish(ledger, action.id, outcome)?;
        }
    }
    Ok(())
}

/// Settles pending agent worktree intents, one agent's or all: Done when `team_worktrees`
/// records the worktree for its agent; otherwise the add never finished and nothing ran in
/// it, so its partial worktree is removed and the intent Failed, or the intent becomes a
/// notice when what is there is not plainly that worktree, which is then left as it is.
/// Notices and intents another part of the engine wrote are left alone. Inputs: the
/// ledger, the team database and the agent, none for all. Output: Ok, or the ledger's or
/// database's failure.
fn settle_worktrees(
    ledger: &Connection,
    team: &Connection,
    only: Option<i64>,
) -> Result<(), EngineError> {
    for action in ledger::pending(ledger, ActionKind::WorktreeCreate)? {
        let Some(intent) = worktree_intent(&action) else {
            continue;
        };
        if only.is_some_and(|agent| agent != intent.team_agent) {
            continue;
        }
        if recorded(team, &intent)? {
            ledger::finish(ledger, action.id, Outcome::Done)?;
            continue;
        }
        match remove(&intent) {
            Ok(()) => ledger::finish(ledger, action.id, Outcome::Failed)?,
            Err(code) => {
                eprintln!("team_worktree_left cause={code}");
                notice(
                    ledger,
                    &action,
                    &WorktreeIntent {
                        unknown: true,
                        ..intent
                    },
                )?;
            }
        }
    }
    Ok(())
}

/// Reads a pending worktree intent the engine still checks. Input: the action. Output: the
/// intent, none for a notice or a payload another part of the engine wrote.
fn worktree_intent(action: &Action) -> Option<WorktreeIntent> {
    serde_json::from_str::<WorktreeIntent>(&action.payload)
        .ok()
        .filter(|intent| action.key == key(intent.team_agent) && !intent.unknown)
}

/// Reads a pending start intent the engine still checks. Input: the action. Output: the
/// intent, none for a notice or a payload another part of the engine wrote.
fn spawn_intent(action: &Action) -> Option<SpawnIntent> {
    serde_json::from_str::<SpawnIntent>(&action.payload)
        .ok()
        .filter(|intent| action.key == key(intent.team_agent) && !intent.unknown)
}

/// Tells whether the runtime recorded this worktree for its agent, which it does only once
/// the add finished. Inputs: the team database and the intent. Output: the answer.
fn recorded(team: &Connection, intent: &WorktreeIntent) -> Result<bool, EngineError> {
    let (Some(repository), Some(worktree)) = (intent.repository.to_str(), intent.worktree.to_str())
    else {
        return Ok(false);
    };
    team.query_row(
        "SELECT EXISTS(SELECT 1 FROM team_worktrees WHERE agent=?1 AND repository=?2 AND worktree=?3)",
        params![intent.team_agent, repository, worktree],
        |row| row.get(0),
    )
    .map_err(super::error)
}

fn plain(path: &Path) -> bool {
    path.is_absolute()
        && path.components().all(|part| {
            matches!(
                part,
                Component::Prefix(_) | Component::RootDir | Component::Normal(_)
            )
        })
}

/// Tells whether an intent names a team agent's own worktree: plain absolute paths, the
/// worktree at `worktrees/agent-<id>` inside the checkout. Input: the intent.
fn placed(intent: &WorktreeIntent) -> bool {
    let name = format!("agent-{}", intent.team_agent);
    intent.team_agent > 0
        && plain(&intent.repository)
        && plain(&intent.worktree)
        && intent.worktree.starts_with(&intent.repository)
        && intent.worktree.file_name() == Some(OsStr::new(&name))
        && intent.worktree.parent().and_then(Path::file_name) == Some(OsStr::new("worktrees"))
}

/// Removes the worktree an unfinished add left, without git, so nothing the repository
/// configures runs: the worktree folder when it is empty or its `.git` file points to its
/// own entry under the checkout's `.git/worktrees`, then that entry, found by its `gitdir`
/// link back to the worktree. Anything else there is left. Input: the intent.
/// Output: Ok once neither is left, or the plain code of what kept them.
fn remove(intent: &WorktreeIntent) -> Result<(), &'static str> {
    if !placed(intent) {
        return Err("team_worktree_invalid");
    }
    let git = intent.repository.join(".git");
    if !std::fs::symlink_metadata(&git).is_ok_and(|meta| meta.is_dir()) {
        return Err("team_worktree_repository");
    }
    let entries = git.join("worktrees");
    let back = format!("{}\n", intent.worktree.join(".git").display());
    let mut entry = None;
    match std::fs::symlink_metadata(&intent.worktree) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return Err("team_worktree_unreadable"),
        Ok(meta) if !meta.is_dir() => return Err("team_worktree_not_folder"),
        Ok(_) => {
            match small(&intent.worktree.join(".git")) {
                Some(pointer) => {
                    let named = pointer
                        .strip_prefix("gitdir: ")
                        .and_then(|path| path.strip_suffix('\n'))
                        .map(PathBuf::from)
                        .filter(|path| {
                            plain(path)
                                && path.parent() == Some(entries.as_path())
                                && small(&path.join("gitdir")).as_deref() == Some(back.as_str())
                        });
                    entry = Some(named.ok_or("team_worktree_foreign")?);
                }
                None if empty(&intent.worktree) => {}
                None => return Err("team_worktree_foreign"),
            }
            std::fs::remove_dir_all(&intent.worktree).map_err(|_| "team_worktree_unremovable")?;
        }
    }
    let entry = match entry {
        Some(entry) => Some(entry),
        None => linked(&entries, &back)?,
    };
    if let Some(entry) = entry {
        assert!(entry.starts_with(&entries) && entry != entries);
        std::fs::remove_dir_all(&entry).map_err(|_| "team_worktree_unremovable")?;
    }
    Ok(())
}

/// Finds the checkout's worktree entry whose `gitdir` file links back to `back`, reading
/// at most `ENTRIES_SCANNED_MAX` entries. Inputs: the `.git/worktrees` folder and the link.
/// Output: the entry, none when no entry links back, or the plain code that stopped it.
fn linked(entries: &Path, back: &str) -> Result<Option<PathBuf>, &'static str> {
    let mut listing = match std::fs::read_dir(entries) {
        Ok(listing) => listing,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err("team_worktree_unreadable"),
    };
    for _ in 0..ENTRIES_SCANNED_MAX {
        let Some(found) = listing.next() else {
            return Ok(None);
        };
        let path = found.map_err(|_| "team_worktree_unreadable")?.path();
        if std::fs::symlink_metadata(&path).is_ok_and(|meta| meta.is_dir())
            && small(&path.join("gitdir")).as_deref() == Some(back)
        {
            return Ok(Some(path));
        }
    }
    Err("team_worktree_entries_many")
}

/// Reads a small plain file such as a worktree's `.git` pointer or `gitdir` link through
/// one open that never follows a link, checking the opened file itself, so nothing swapped
/// in after a check is read. Input: the path. Output: the text, none when it is not such
/// a file.
fn small(path: &Path) -> Option<String> {
    crate::platform::read_plain_file(path, POINTER_BYTES_MAX)
}

fn empty(folder: &Path) -> bool {
    std::fs::read_dir(folder).is_ok_and(|mut entries| entries.next().is_none())
}

fn logged(result: Result<(), EngineError>) {
    if let Err(problem) = result {
        eprintln!(
            "team_ledger_failed cause={}",
            crate::protocol::plain_code(&problem.0)
        );
    }
}

#[cfg(all(test, unix))]
#[path = "spawn_ledger/tests.rs"]
mod tests;
