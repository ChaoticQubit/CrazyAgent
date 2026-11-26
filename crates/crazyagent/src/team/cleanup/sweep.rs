use super::*;

/// What one pass of the engine's own cleanup did: the agents whose worktree it removed, in
/// id order, the id the next pass starts after, 0 once this pass read the last finished agent
/// so agents a check keeps never hold the others back, one line for each agent the engine
/// could not check or remove, and one for each agent its check let through but its removal
/// then kept, with the plain code that kept it, at most `CANDIDATES_MAX` lines in all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Swept {
    pub removed: Vec<i64>,
    pub after: i64,
    pub failures: Vec<String>,
    pub kept: Vec<String>,
}

/// Removes, on the engine's own schedule, the worktrees of finished agents in every project
/// the owner's setting lets go: past its wait since the agent finished, an agent with no
/// finish time counting as past it, each one checked and removed as Clean up does, one at a
/// time, so one a check keeps never stops the others. Nothing goes when only the owner's
/// Clean up removes. A pass reads at most `CANDIDATES_MAX` agents with an id above `after`
/// and removes at most `RECLAIMABLE_MAX`. Inputs: the store, the engine root, the setting,
/// where the last pass stopped and the time now. Output: what this pass did.
pub fn sweep(
    db: &Connection,
    runtime: &Path,
    settings: &CleanupSettings,
    after: i64,
    now_ms: i64,
) -> Result<Swept, EngineError> {
    assert!(after >= 0);
    let Some(wait_ms) = settings.wait_ms() else {
        return Ok(Swept {
            removed: Vec::new(),
            after: 0,
            failures: Vec::new(),
            kept: Vec::new(),
        });
    };
    let now = landing_time(now_ms)?;
    let candidates: Vec<(i64, String)> = {
        let _running = running();
        initialize(db)?;
        let mut statement = db
            .prepare(
                "SELECT id,project FROM team_agents AS agent WHERE id>?1
                 AND status IN ('stopped','failed')
                 AND (stopped_ms IS NULL OR stopped_ms<=?2)
                 AND (id IN (SELECT agent FROM team_worktrees WHERE project=agent.project)
                      OR id IN (SELECT agent FROM team_retiring WHERE project=agent.project))
                 ORDER BY id LIMIT ?3",
            )
            .map_err(unavailable)?;
        statement
            .query_map(
                params![after, now_ms.saturating_sub(wait_ms), CANDIDATES_MAX as i64],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(unavailable)?
            .collect::<Result<_, _>>()
            .map_err(unavailable)?
    };
    let mut swept = Swept {
        removed: Vec::new(),
        after: 0,
        failures: Vec::new(),
        kept: Vec::new(),
    };
    for (id, project) in &candidates {
        if swept.removed.len() == RECLAIMABLE_MAX {
            break;
        }
        assert!(*id > swept.after);
        swept.after = *id;
        if let Err(problem) = sweep_one(db, runtime, project, *id, now, now_ms, &mut swept) {
            swept.failures.push(format!(
                "agent={id} cause={}",
                crate::protocol::plain_code(&problem.0)
            ));
        }
    }
    if candidates.len() < CANDIDATES_MAX
        && candidates.last().map(|last| last.0) == Some(swept.after)
    {
        swept.after = 0;
    }
    assert!(swept.removed.len() <= RECLAIMABLE_MAX);
    assert!(candidates.len() <= CANDIDATES_MAX);
    assert!(swept.removed.len() + swept.kept.len() + swept.failures.len() <= candidates.len());
    Ok(swept)
}

/// Checks one agent as Clean up would and removes its worktree when nothing keeps it, then
/// notes the files git ignores that went with it. Inputs: the store, its project, the agent,
/// the time now in both forms and the pass it adds to: the agent once removed, or the code
/// its removal kept it with. Output: Ok whether it went or stayed, or the problem that
/// stopped the check or the note.
fn sweep_one(
    db: &Connection,
    runtime: &Path,
    project: &str,
    id: i64,
    now: u64,
    now_ms: i64,
    swept: &mut Swept,
) -> Result<(), EngineError> {
    let project = ProjectId::new(project).map_err(unavailable)?;
    let checked = {
        let _running = running();
        check(db, &project, id, now)?
    };
    let Ok(target) = checked else {
        return Ok(());
    };
    let held = held_ignored(db, &target);
    let removal = remove(db, runtime, &project, &[id], now_ms)?;
    match removal.kept {
        None => {
            assert_eq!(removal.agents, [id]);
            swept.removed.push(id);
            if let Some(held) = held {
                note_swept(db, &project, id, &held)?;
            }
        }
        Some(code) => {
            assert!(removal.agents.is_empty());
            swept.kept.push(format!(
                "agent={id} kept={}",
                crate::protocol::plain_code(&code)
            ));
        }
    }
    Ok(())
}

/// The note the sweep writes when a worktree it removed held files git ignores, with the
/// agent's name for `{agent}` and the bounded list of entries for `{paths}`.
const SWEPT_IGNORED: &str =
    "The worktree of {agent} was removed; the files git ignores in it went with it: {paths}.";

const LABEL_BYTES_MAX: usize = 120;

const PATH_BYTES_MAX: usize = 120;

const TAIL_BYTES_MAX: usize = 32;

/// The name the owner's lists give a finished agent: a sub-agent whose name a running agent
/// now carries is named by its number alone. Inputs: the store, the project and the agent.
/// Output: the name.
pub(super) fn listed_label(
    db: &Connection,
    project: &ProjectId,
    agent: &TeamAgent,
) -> Result<String, EngineError> {
    if agent.role == AgentRole::Sub
        && super::super::events::name_reaches_running(db, project, agent.id)?
    {
        return Ok(AgentRole::Sub.agent_label(agent.id));
    }
    Ok(agent.label.clone())
}

/// The files git ignores in a worktree the sweep is about to remove: the first entries and
/// how many there are in all, None past what git may print. A listing git refuses gives no
/// note rather than keeping the worktree, since ignored files never keep one. Inputs: the
/// store and the checked target. Output: the entries, or None.
#[cfg(unix)]
fn held_ignored(db: &Connection, target: &Target) -> Option<(Vec<String>, Option<usize>)> {
    if target.worktree_removed() {
        return None;
    }
    let place = &target.place;
    crate::landing::retire::ignored::ignored(db, &place.repository, &place.worktree)
        .ok()
        .flatten()
        .map(|held| (held.paths, held.total))
}

/// No ignored listing where the landing checks are not built. Output: None.
#[cfg(not(unix))]
fn held_ignored(_: &Connection, _: &Target) -> Option<(Vec<String>, Option<usize>)> {
    None
}

/// Writes the note naming the files git ignores that went with a swept worktree, in the
/// project's team conversation where the owner reads what the engine did. Inputs: the
/// store, the project, the agent and the entries. Output: done, or the store's failure.
fn note_swept(
    db: &Connection,
    project: &ProjectId,
    id: i64,
    held: &(Vec<String>, Option<usize>),
) -> Result<(), EngineError> {
    let agent = db
        .query_row(
            &format!("SELECT {AGENT_COLUMNS} FROM team_agents WHERE id=?1 AND project=?2"),
            params![id, project.as_str()],
            agent_row,
        )
        .map_err(unavailable)?;
    let label = crate::team::bound_text(&listed_label(db, project, &agent)?, LABEL_BYTES_MAX);
    let (head, rest) = SWEPT_IGNORED.split_once("{agent}").unwrap();
    let (middle, tail) = rest.split_once("{paths}").unwrap();
    let frame = head.len() + label.len() + middle.len() + tail.len();
    let list = named(held, crate::team::NOTE_BYTES_MAX - frame);
    let text = format!("{head}{label}{middle}{list}{tail}");
    assert!(text.len() <= crate::team::NOTE_BYTES_MAX);
    let tx = Transaction::new_unchecked(db, TransactionBehavior::Immediate).map_err(unavailable)?;
    let _ = crate::team::append_message(
        &tx,
        project,
        crate::team::MessageAuthor::Engine,
        &text,
        crate::team::MessageState::Note,
        crate::team::now_ms(),
    )?;
    tx.commit().map_err(unavailable)
}

/// As many of the listed entries as fit in `room` bytes, each with control characters
/// replaced and cut to `PATH_BYTES_MAX`, then how many more there are. Inputs: the entries
/// with their count and the room. Output: the list, naming at least one entry.
fn named((paths, total): &(Vec<String>, Option<usize>), room: usize) -> String {
    assert!(!paths.is_empty());
    assert!(room >= PATH_BYTES_MAX + TAIL_BYTES_MAX);
    let mut line = String::new();
    let mut shown = 0_usize;
    for path in paths {
        let name =
            crate::team::bound_text(&path.replace(char::is_control, "\u{fffd}"), PATH_BYTES_MAX);
        let gap = if shown == 0 { "" } else { ", " };
        if line.len() + gap.len() + name.len() + TAIL_BYTES_MAX > room {
            break;
        }
        line.push_str(gap);
        line.push_str(&name);
        shown += 1;
    }
    assert!(shown > 0);
    match *total {
        Some(total) if total > shown => line.push_str(&format!(" and {} more", total - shown)),
        Some(_) => {}
        None => line.push_str(" and more"),
    }
    assert!(line.len() <= room);
    line
}
