use super::store::{
    Change, agent_in, check_text, failure, master_of, party_columns, party_in, state,
};
use super::{
    AgentId, BODY_BYTES_MAX, CLAIMS_PER_WORK_MAX, Finish, Overlap, Party, TITLE_BYTES_MAX,
    Transfer, WORKS_LISTED_MAX, Work, WorkId, WorkState,
};
use crate::engine::EngineError;
use crate::protocol::ProjectId;
use rusqlite::{Connection, OptionalExtension, params};

/// The parts of a unit of work a transition decides on.
struct WorkRow {
    project: ProjectId,
    owner: Option<AgentId>,
    state: WorkState,
    version: i64,
}

/// Starts a new unit of work owned by an agent, Active at version 1, with its claims
/// (Concord's start_work): the declared files, folders, globs, `module:` and `domain:`
/// labels. Overlaps with other agents' active claims come back at once for both sides
/// to hear; `breadth` gives the soft "split this" note. Takes the database, the project,
/// the agent, the title, the claims and the time in ms. Returns the work and the
/// overlaps, or a plain refusal for a stopped agent or invalid input. Under Wait a start on
/// a file another agent's work holds starts nothing: it records the wait and is refused
/// with `coord_claim_waits`.
pub fn start_work(
    db: &Connection,
    project: &ProjectId,
    agent: AgentId,
    title: String,
    claims: Vec<String>,
    now: i64,
) -> Result<(WorkId, Vec<Overlap>), EngineError> {
    check_text(&title, TITLE_BYTES_MAX)?;
    let claims = super::claims::normalize_all(&claims)?;
    let change = Change::begin(db)?;
    let tx = change.db();
    if !agent_in(tx, project, agent)?.live() {
        return Err(failure("coord_agent_stopped"));
    }
    tx.execute(
        "INSERT INTO coord_work(project,title,owner_agent,state,version,created,updated)
         VALUES (?1,?2,?3,'active',1,?4,?4)",
        params![project.as_str(), title, agent, now],
    )
    .map_err(state)?;
    let work = tx.last_insert_rowid();
    audit(tx, project, work, Party::Agent(agent), "start", 0, 1, now)?;
    super::claims::add(tx, project, agent, work, &claims, now)?;
    let overlaps = super::claims::overlaps_for(tx, project, agent, work, now)?;
    if waits(tx, project)?
        && let Some(held) = super::claims::waits_on(&overlaps)
    {
        let (holder, file) = (held.theirs.work, held.mine.pattern.clone());
        drop(change);
        super::claims::wait_for(db, project, agent, holder, &file, now)?;
        return Err(failure("coord_claim_waits"));
    }
    super::presence::seen(tx, agent, now)?;
    change.commit()?;
    Ok((work, overlaps))
}

/// Says whether the project's Overlapping edits option is Wait. A store failure fails;
/// settings that cannot be read count as the default, as the start's answer reads them.
/// Takes the database and the project. Returns whether starts wait.
fn waits(db: &Connection, project: &ProjectId) -> Result<bool, EngineError> {
    match super::settings::read(db, project) {
        Ok(settings) => Ok(settings.overlaps == super::settings::Overlaps::Wait),
        Err(problem) if problem.0.starts_with("coord_state_unavailable") => Err(problem),
        Err(_) => Ok(false),
    }
}

/// Changes who owns a unit of work (Concord's transfer verbs), first transition wins:
/// Assign gives Open, Declined or the owner's own work to a running agent; the owner
/// Accepts or Declines what was assigned; Release gives it back as Open and Handoff
/// passes it on, both dropping the owner's claims; Reopen brings ended work back, its
/// claims no longer waiting on a change set, so a set landing later frees none of the
/// reopened work's later edits. The owner, the engine, the master and the owner's lead
/// may Assign, Release and Reopen too.
/// Takes the database, the work, the version the caller read, the change, who acts and
/// the time in ms. Returns the new version, `coord_work_stale`, `coord_transfer_invalid`
/// for the state or `coord_transfer_refused` for who acts.
pub fn transfer(
    db: &Connection,
    work: WorkId,
    version: i64,
    action: Transfer,
    by: Party,
    now: i64,
) -> Result<i64, EngineError> {
    let change = Change::begin(db)?;
    let tx = change.db();
    let row = current(tx, work, version)?;
    party_in(tx, &row.project, by)?;
    let owns = row.owner.is_some_and(|owner| by == Party::Agent(owner));
    let supervises = |agent: Option<AgentId>| -> Result<bool, EngineError> {
        supervisor(tx, &row.project, by, agent)
    };
    use WorkState::{Accepted, Active, Assigned, Closed, Complete, Declined, Open, ReviewReady};
    let (state, owner, name) = match action {
        Transfer::Assign { to } => {
            if !agent_in(tx, &row.project, to)?.live() {
                return Err(failure("coord_agent_stopped"));
            }
            if !matches!(row.state, Open | Declined | Accepted | Active) || row.owner == Some(to) {
                return Err(failure("coord_transfer_invalid"));
            }
            if !(owns || supervises(Some(to))? || supervises(row.owner)?) {
                return Err(failure("coord_transfer_refused"));
            }
            (Assigned, Some(to), "assign")
        }
        Transfer::Accept | Transfer::Decline => {
            if row.state != Assigned {
                return Err(failure("coord_transfer_invalid"));
            }
            if !owns {
                return Err(failure("coord_transfer_refused"));
            }
            match action {
                Transfer::Accept => (Accepted, row.owner, "accept"),
                _ => (Declined, row.owner, "decline"),
            }
        }
        Transfer::Release => {
            if !matches!(row.state, Accepted | Active) {
                return Err(failure("coord_transfer_invalid"));
            }
            if !(owns || supervises(row.owner)?) {
                return Err(failure("coord_transfer_refused"));
            }
            (Open, None, "release")
        }
        Transfer::Handoff { to } => {
            if !agent_in(tx, &row.project, to)?.live() {
                return Err(failure("coord_agent_stopped"));
            }
            if !matches!(row.state, Accepted | Active) || row.owner == Some(to) {
                return Err(failure("coord_transfer_invalid"));
            }
            if !owns {
                return Err(failure("coord_transfer_refused"));
            }
            (Assigned, Some(to), "handoff")
        }
        Transfer::Reopen => {
            if !matches!(row.state, ReviewReady | Complete | Closed | Declined) {
                return Err(failure("coord_transfer_invalid"));
            }
            if !(owns || supervises(row.owner)?) {
                return Err(failure("coord_transfer_refused"));
            }
            let state = if row.owner.is_some() { Active } else { Open };
            (state, row.owner, "reopen")
        }
    };
    if let Some(previous) = row.owner
        && owner != Some(previous)
    {
        super::claims::release_claims(tx, previous, work, now)?;
    }
    if action == Transfer::Reopen {
        tx.execute(
            "DELETE FROM coord_held WHERE claim IN (SELECT id FROM coord_claims WHERE work=?1)",
            [work],
        )
        .map_err(super::store::state)?;
    }
    let next = advance(tx, work, &row, state, owner, now)?;
    audit(tx, &row.project, work, by, name, version, next, now)?;
    if let Party::Agent(agent) = by {
        super::presence::seen(tx, agent, now)?;
    }
    change.commit()?;
    Ok(next)
}

/// Ends a unit of work with evidence (Concord's finish_work): its owner marks Accepted or
/// Active work ReviewReady, Complete or Closed, and work waiting for review is Completed
/// or Closed by its owner or whoever supervises it. A master's claims are released at
/// every end; any other agent's hold at every end until the change set carrying its edits
/// lands or is rejected, or its turn ends with nothing to land (OS-8, DEC-117 Q7).
/// Checks that belong to a role, such as a lead's unmerged sub-agent branches, run
/// before this. Takes the database, the work, the version the caller read, the outcome,
/// the evidence, who finishes and the time in ms. Returns the new version or a plain
/// refusal.
pub fn finish(
    db: &Connection,
    work: WorkId,
    version: i64,
    outcome: Finish,
    evidence: String,
    by: AgentId,
    now: i64,
) -> Result<i64, EngineError> {
    check_text(&evidence, BODY_BYTES_MAX)?;
    let change = Change::begin(db)?;
    let tx = change.db();
    let row = current(tx, work, version)?;
    agent_in(tx, &row.project, by)?;
    let owns = row.owner == Some(by);
    let allowed = match row.state {
        WorkState::Accepted | WorkState::Active => owns,
        WorkState::ReviewReady => {
            outcome != Finish::ReviewReady
                && (owns || supervisor(tx, &row.project, Party::Agent(by), row.owner)?)
        }
        _ => return Err(failure("coord_transfer_invalid")),
    };
    if !allowed {
        return Err(failure("coord_transfer_refused"));
    }
    let (ended, name) = match outcome {
        Finish::ReviewReady => (WorkState::ReviewReady, "finish_review_ready"),
        Finish::Complete => (WorkState::Complete, "finish_complete"),
        Finish::Closed => (WorkState::Closed, "finish_closed"),
    };
    if let Some(owner) = row.owner
        && super::store::agent(tx, owner)?.is_none_or(|found| found.master())
    {
        super::claims::release_claims(tx, owner, work, now)?;
    }
    let next = advance(tx, work, &row, ended, row.owner, now)?;
    let changed = tx
        .execute(
            "UPDATE coord_work SET evidence=?2 WHERE id=?1",
            params![work, evidence],
        )
        .map_err(state)?;
    assert_eq!(changed, 1);
    audit(
        tx,
        &row.project,
        work,
        Party::Agent(by),
        name,
        version,
        next,
        now,
    )?;
    super::presence::seen(tx, by, now)?;
    change.commit()?;
    Ok(next)
}

/// Lists a project's unfinished work, everything not Complete, Closed or Declined, most
/// recently changed first, each with its claims that still hold. Takes the database, the
/// project and the most to return, capped at `WORKS_LISTED_MAX`. Returns the work.
pub fn works(db: &Connection, project: &ProjectId, limit: usize) -> Result<Vec<Work>, EngineError> {
    let limit = i64::try_from(limit.min(WORKS_LISTED_MAX)).map_err(state)?;
    let mut statement = db
        .prepare(
            "SELECT id FROM coord_work WHERE project=?1
             AND state NOT IN ('complete','closed','declined')
             ORDER BY updated DESC, id DESC LIMIT ?2",
        )
        .map_err(state)?;
    let ids = statement
        .query_map(params![project.as_str(), limit], |row| {
            row.get::<_, WorkId>(0)
        })
        .map_err(state)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(state)?;
    let mut found = Vec::with_capacity(ids.len());
    for id in ids {
        found.push(work(db, id)?);
    }
    assert!(found.len() <= WORKS_LISTED_MAX);
    Ok(found)
}

/// Reads one unit of work with its claims that still hold. Takes the database and the
/// work. Returns it, or `coord_work_unknown`.
pub fn work(db: &Connection, id: WorkId) -> Result<Work, EngineError> {
    let row: Option<(String, Option<AgentId>, String, i64)> = db
        .query_row(
            "SELECT title,owner_agent,state,version FROM coord_work WHERE id=?1",
            [id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()
        .map_err(state)?;
    let Some((title, owner, text, version)) = row else {
        return Err(failure("coord_work_unknown"));
    };
    let mut statement = db
        .prepare(
            "SELECT pattern FROM coord_claims WHERE work=?1 AND released IS NULL
             ORDER BY id LIMIT ?2",
        )
        .map_err(state)?;
    let limit = i64::try_from(CLAIMS_PER_WORK_MAX).map_err(state)?;
    let claims = statement
        .query_map(params![id, limit], |row| row.get::<_, String>(0))
        .map_err(state)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(state)?;
    Ok(Work {
        id,
        title,
        owner,
        state: WorkState::parse(&text).ok_or_else(|| failure("coord_state_unavailable"))?,
        version,
        claims,
    })
}

/// Reads a unit of work for a transition at the version the caller read. Returns it,
/// `coord_work_unknown`, or `coord_work_stale` when another transition came first.
fn current(tx: &Connection, work: WorkId, version: i64) -> Result<WorkRow, EngineError> {
    let row: Option<(String, Option<AgentId>, String, i64)> = tx
        .query_row(
            "SELECT project,owner_agent,state,version FROM coord_work WHERE id=?1",
            [work],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()
        .map_err(state)?;
    let Some((project, owner, text, stored)) = row else {
        return Err(failure("coord_work_unknown"));
    };
    if stored != version {
        return Err(failure("coord_work_stale"));
    }
    Ok(WorkRow {
        project: ProjectId::new(project).map_err(|_| failure("coord_state_unavailable"))?,
        owner,
        state: WorkState::parse(&text).ok_or_else(|| failure("coord_state_unavailable"))?,
        version: stored,
    })
}

/// Stores a transition: the new state and owner at the next version, only while the
/// version is still the one read. Returns the next version.
fn advance(
    tx: &Connection,
    work: WorkId,
    row: &WorkRow,
    next_state: WorkState,
    owner: Option<AgentId>,
    now: i64,
) -> Result<i64, EngineError> {
    let next = row
        .version
        .checked_add(1)
        .ok_or_else(|| failure("coord_work_stale"))?;
    let changed = tx
        .execute(
            "UPDATE coord_work SET state=?2, owner_agent=?3, version=?4, updated=?5
             WHERE id=?1 AND version=?6",
            params![work, next_state.as_str(), owner, next, now, row.version],
        )
        .map_err(state)?;
    assert_eq!(changed, 1);
    Ok(next)
}

/// Says whether a party oversees an agent's work: the owner, the engine, the project's
/// master, or the agent's lead. Takes the transaction, the project, the party and the
/// agent, None for work nobody owns, which only the owner, engine and master oversee.
fn supervisor(
    tx: &Connection,
    project: &ProjectId,
    by: Party,
    agent: Option<AgentId>,
) -> Result<bool, EngineError> {
    let Party::Agent(by) = by else {
        return Ok(true);
    };
    if master_of(tx, project)? == Some(by) {
        return Ok(true);
    }
    match agent {
        Some(agent) => Ok(agent_in(tx, project, agent)?.parent == Some(by)),
        None => Ok(false),
    }
}

/// Appends one ownership change to the audit trail.
#[allow(clippy::too_many_arguments)]
fn audit(
    tx: &Connection,
    project: &ProjectId,
    work: WorkId,
    by: Party,
    action: &str,
    from: i64,
    to: i64,
    now: i64,
) -> Result<(), EngineError> {
    assert!(to == from + 1);
    let (kind, agent) = party_columns(by);
    tx.execute(
        "INSERT INTO coord_audit(project,work,by_kind,by_agent,action,from_version,to_version,at)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
        params![project.as_str(), work, kind, agent, action, from, to, now],
    )
    .map_err(state)?;
    Ok(())
}
