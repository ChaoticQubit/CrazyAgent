use super::store::{Change, failure, related, state};
use super::{
    AGENT_CLAIMS_MAX, AgentId, CLAIMS_PER_WORK_MAX, Claim, MessageKind, NewMessage, Overlap,
    PATTERN_BYTES_MAX, Party, Presence, WorkId,
};
use crate::engine::EngineError;
use crate::protocol::ProjectId;
use rusqlite::{Connection, params};
use std::collections::HashMap;

/// The most active claims of a project one overlap check reads; past it the check fails
/// closed with `coord_claims_full`, never a silent miss.
const CLAIMS_SCAN_MAX: usize = 2048;
/// The most change sets one agent's held claims wait on that one turn end checks.
const HELD_SETS_MAX: usize = 16;
/// The work states whose claims wait for a change set rather than for the agent.
const ENDED: &str = "('review_ready','complete','closed')";
/// The work states whose claims still hold scope: someone is on it, is about to be, or it
/// ended while its change set has not landed.
const HOLDING: &str = "('assigned','accepted','active','review_ready','complete','closed')";

/// What one read of a project's claims does past `CLAIMS_SCAN_MAX`: a check that decides
/// an overlap fails closed with `coord_claims_full`; a view shows the oldest claims.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Scan {
    Check,
    View,
}
/// Concord's soft breadth limits: more path claims, modules or domains than these in
/// one unit of work get a "split this" note.
const PATHS_SOFT_MAX: usize = 5;
const MODULES_SOFT_MAX: usize = 3;
const DOMAINS_SOFT_MAX: usize = 2;
/// The stored form of a claim on the whole repository.
const ROOT: &str = "**";
/// The most files one agent waits for under Wait; a newer wait drops its oldest.
const WAITS_PER_AGENT_MAX: usize = 8;
/// The most waits one wake settles; the rest settle at the next release or sweep.
const WAKES_MAX: usize = 64;
/// The most agents one cycle check under Wait walks; a longer chain counts as a cycle.
pub(super) const WAIT_WALK_MAX: usize = 64;
/// The most wait cycles one notice read tells the owner of; the rest are told next time.
const CYCLE_NOTICES_MAX: usize = 16;

const _: () = assert!(CLAIMS_SCAN_MAX >= CLAIMS_PER_WORK_MAX * 2);
const _: () = assert!(AGENT_CLAIMS_MAX >= CLAIMS_PER_WORK_MAX);
const _: () = assert!(CLAIMS_SCAN_MAX >= AGENT_CLAIMS_MAX * 16);

/// What one claim covers, read from its stored form.
enum Scope<'a> {
    File(&'a str),
    Folder(&'a str),
    Glob(&'a str),
    Label { module: bool, tokens: Vec<String> },
}

impl<'a> Scope<'a> {
    fn of(pattern: &'a str) -> Self {
        if let Some(label) = pattern.strip_prefix("module:") {
            return Self::Label {
                module: true,
                tokens: tokens(label),
            };
        }
        if let Some(label) = pattern.strip_prefix("domain:") {
            return Self::Label {
                module: false,
                tokens: tokens(label),
            };
        }
        if pattern == ROOT {
            return Self::Folder("");
        }
        if pattern.contains(['*', '?']) {
            return Self::Glob(pattern);
        }
        if pattern.ends_with('/') {
            return Self::Folder(pattern);
        }
        Self::File(pattern)
    }

    /// Says whether this scope covers one repository file path.
    fn covers(&self, file: &str) -> bool {
        match self {
            Self::File(path) => *path == file,
            Self::Folder(folder) => file.starts_with(folder),
            Self::Glob(glob) => glob_match(glob.as_bytes(), file.as_bytes()),
            Self::Label { .. } => false,
        }
    }
}

/// Puts a declared claim into its one stored form, so `./src/a.rs` and `src/a.rs` are the
/// same claim: `module:<label>` and `domain:<label>` stay labels; a path loses `./`, `.`
/// and empty segments and keeps a trailing `/` for a folder; `.` or `**` alone claims the
/// whole repository. An absolute path, a `..` segment, a backslash or a control character
/// or a line break of any kind is refused. Takes the declared text. Returns the stored
/// form or `coord_claim_invalid`.
pub(super) fn normalize(raw: &str) -> Result<String, EngineError> {
    let invalid = || failure("coord_claim_invalid");
    let raw = raw.trim();
    if raw.is_empty()
        || raw.len() > PATTERN_BYTES_MAX
        || raw
            .chars()
            .any(|c| c.is_control() || crate::team::events::line_break(c))
    {
        return Err(invalid());
    }
    for prefix in ["module:", "domain:"] {
        if let Some(label) = raw.strip_prefix(prefix) {
            let label = label.trim();
            if tokens(label).is_empty() {
                return Err(invalid());
            }
            return Ok(format!("{prefix}{label}"));
        }
    }
    if raw.starts_with('/') || raw.contains('\\') {
        return Err(invalid());
    }
    let mut segments = Vec::new();
    for segment in raw.split('/') {
        match segment {
            "" | "." => {}
            ".." => return Err(invalid()),
            segment => segments.push(segment),
        }
    }
    let joined = segments.join("/");
    let stored = match joined.as_str() {
        "" | ROOT => ROOT.to_owned(),
        _ if raw.ends_with('/') => format!("{joined}/"),
        _ => joined,
    };
    assert!(stored.len() <= PATTERN_BYTES_MAX + 1);
    Ok(stored)
}

/// Normalizes the claims of one unit of work and drops repeats, keeping their order.
/// Takes the declared claims. Returns the stored forms, `coord_claims_too_many` past
/// `CLAIMS_PER_WORK_MAX`, or `coord_claim_invalid`.
pub(super) fn normalize_all(raw: &[String]) -> Result<Vec<String>, EngineError> {
    if raw.len() > CLAIMS_PER_WORK_MAX {
        return Err(failure("coord_claims_too_many"));
    }
    let mut stored: Vec<String> = Vec::with_capacity(raw.len());
    for claim in raw {
        let claim = normalize(claim)?;
        if !stored.contains(&claim) {
            stored.push(claim);
        }
    }
    Ok(stored)
}

/// The soft "split this" reasons for a unit of work's claims (Concord): a claim on the
/// whole repository or every top-level file, more than 5 paths, 3 modules or 2 domains.
/// Never a refusal of the claims themselves. Takes the declared claims. Returns the
/// reasons, empty when the scope is fine, or `coord_claim_invalid`.
pub fn breadth(claims: &[String]) -> Result<Vec<String>, EngineError> {
    let stored = normalize_all(claims)?;
    let mut reasons = Vec::new();
    if stored.iter().any(|claim| claim == ROOT || claim == "*") {
        reasons.push("a claim covers the whole repository".to_owned());
    }
    let (mut paths, mut modules, mut domains) = (0, 0, 0);
    for claim in &stored {
        match Scope::of(claim) {
            Scope::Label { module: true, .. } => modules += 1,
            Scope::Label { module: false, .. } => domains += 1,
            _ => paths += 1,
        }
    }
    for (count, limit, what) in [
        (paths, PATHS_SOFT_MAX, "files or folders"),
        (modules, MODULES_SOFT_MAX, "modules"),
        (domains, DOMAINS_SOFT_MAX, "domains"),
    ] {
        if count > limit {
            reasons.push(format!("{count} {what} (split above {limit})"));
        }
    }
    Ok(reasons)
}

/// The folder and glob claims of a unit of work, each in its stored form with the fixed
/// beginning every path it covers starts with, so a caller can count the files one claim
/// covers for the "split this" note. The whole repository is left out, since `breadth`
/// notes it already. Takes the declared claims. Returns the pairs or `coord_claim_invalid`.
pub fn path_scopes(claims: &[String]) -> Result<Vec<(String, String)>, EngineError> {
    Ok(normalize_all(claims)?
        .into_iter()
        .filter_map(|claim| {
            let prefix = match Scope::of(&claim) {
                Scope::Folder(folder) if !folder.is_empty() => folder.to_owned(),
                Scope::Glob(glob) => fixed(glob).to_owned(),
                _ => return None,
            };
            Some((claim, prefix))
        })
        .collect())
}

/// Says whether a stored file, folder or glob claim covers one repository path; a label
/// covers none. Takes the stored claim and the path. Returns the answer.
pub fn covers(claim: &str, path: &str) -> bool {
    Scope::of(claim).covers(path)
}

/// Stores the claims of a unit of work, refused with `coord_agent_claims_full` when they
/// would take the agent past `AGENT_CLAIMS_MAX` live claims, so one agent's flood stops
/// only that agent. Takes the transaction, the project, the agent, the work, the stored
/// forms and the time in ms.
pub(super) fn add(
    tx: &Connection,
    project: &ProjectId,
    agent: AgentId,
    work: WorkId,
    claims: &[String],
    now: i64,
) -> Result<(), EngineError> {
    assert!(claims.len() <= CLAIMS_PER_WORK_MAX);
    if claims.is_empty() {
        return Ok(());
    }
    let limit = i64::try_from(AGENT_CLAIMS_MAX + 1).map_err(state)?;
    let live: i64 = tx
        .query_row(
            &format!(
                "SELECT COUNT(*) FROM (SELECT 1 FROM coord_claims c JOIN coord_work w ON w.id=c.work
                 WHERE c.project=?1 AND c.agent=?2 AND c.released IS NULL AND w.state IN {HOLDING}
                 LIMIT ?3)"
            ),
            params![project.as_str(), agent, limit],
            |row| row.get(0),
        )
        .map_err(state)?;
    let live = usize::try_from(live).map_err(state)?;
    assert!(live <= AGENT_CLAIMS_MAX + 1);
    if live + claims.len() > AGENT_CLAIMS_MAX {
        return Err(failure("coord_agent_claims_full"));
    }
    for claim in claims {
        tx.execute(
            "INSERT INTO coord_claims(project,agent,work,pattern,created) VALUES (?1,?2,?3,?4,?5)",
            params![project.as_str(), agent, work, claim, now],
        )
        .map_err(state)?;
    }
    Ok(())
}

/// Releases an agent's claims on one unit of work, which then overlap nothing. Takes the
/// database, the agent, the work and the time in ms. Returns Ok.
pub fn release_claims(
    db: &Connection,
    agent: AgentId,
    work: WorkId,
    now: i64,
) -> Result<(), EngineError> {
    db.execute(
        "UPDATE coord_claims SET released=?3 WHERE agent=?1 AND work=?2 AND released IS NULL",
        params![agent, work, now],
    )
    .map_err(state)?;
    wake_waiters(db, now)?;
    Ok(())
}

/// Holds an agent's claims on its ended work for the change set that carries its edits, so
/// they keep overlapping until that set lands or is discarded (OS-8, DEC-117 Q7). A claim
/// already held for a set keeps that set. Takes the database, the agent and the open
/// change set's id. Returns how many claims it newly held.
pub fn hold_for_set(db: &Connection, agent: AgentId, set: &str) -> Result<usize, EngineError> {
    assert!(!set.is_empty());
    db.execute(
        &format!(
            "INSERT OR IGNORE INTO coord_held(claim,project,change_set)
             SELECT c.id,c.project,?2 FROM coord_claims c JOIN coord_work w ON w.id=c.work
             WHERE c.agent=?1 AND c.released IS NULL AND w.state IN {ENDED}"
        ),
        params![agent, set],
    )
    .map_err(state)
}

/// Releases the claims one closed change set held on still-ended work, and marks those
/// holds released at that time so a restore can take back exactly them. Claims of work
/// reopened since, or held for another set, stay. Takes the database, the set's id and the
/// time in ms. Returns how many claims it released.
pub fn release_set(db: &Connection, set: &str, now: i64) -> Result<usize, EngineError> {
    assert!(!set.is_empty());
    let change = Change::begin(db)?;
    let tx = change.db();
    let marked = tx
        .execute(
            &format!(
                "UPDATE coord_held SET released=?2 WHERE change_set=?1 AND released IS NULL
                 AND claim IN (SELECT c.id FROM coord_claims c JOIN coord_work w ON w.id=c.work
                               WHERE c.released IS NULL AND w.state IN {ENDED})"
            ),
            params![set, now],
        )
        .map_err(state)?;
    let released = tx
        .execute(
            "UPDATE coord_claims SET released=?2 WHERE released IS NULL
             AND id IN (SELECT claim FROM coord_held WHERE change_set=?1 AND released=?2)",
            params![set, now],
        )
        .map_err(state)?;
    assert_eq!(
        marked, released,
        "a set's release frees exactly the claims it marked"
    );
    wake_waiters(tx, now)?;
    change.commit()?;
    Ok(released)
}

/// Takes back the claims a change set's close released, once that set is restored, so
/// they overlap newer claims again as any held claim does; a set with no released hold to
/// take back or unmark writes nothing. Takes the database and the set's id. Returns how many
/// claims hold again.
pub fn restore_set(db: &Connection, set: &str) -> Result<usize, EngineError> {
    assert!(!set.is_empty());
    let due: bool = db
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM coord_held h JOIN coord_claims c ON c.id=h.claim
             WHERE h.change_set=?1 AND h.released IS NOT NULL
             AND (c.released IS NULL OR h.released=c.released))",
            [set],
            |row| row.get(0),
        )
        .map_err(state)?;
    if !due {
        return Ok(0);
    }
    let change = Change::begin(db)?;
    let tx = change.db();
    let restored = tx
        .execute(
            "UPDATE coord_claims SET released=NULL WHERE id IN
             (SELECT h.claim FROM coord_held h WHERE h.change_set=?1
              AND h.released IS NOT NULL AND h.released=coord_claims.released)",
            [set],
        )
        .map_err(state)?;
    let unmarked = tx
        .execute(
            "UPDATE coord_held SET released=NULL WHERE change_set=?1 AND released IS NOT NULL
             AND claim IN (SELECT id FROM coord_claims WHERE released IS NULL)",
            [set],
        )
        .map_err(state)?;
    assert_eq!(
        restored, unmarked,
        "a restore takes back exactly the claims its set released"
    );
    change.commit()?;
    Ok(restored)
}

/// Releases an agent's claims on ended work no change set holds, at a turn end that leaves
/// nothing to land, so such claims never hold forever. Takes the database, the agent and
/// the time in ms. Returns how many claims it released.
pub fn release_unheld(db: &Connection, agent: AgentId, now: i64) -> Result<usize, EngineError> {
    let released = db
        .execute(
            &format!(
                "UPDATE coord_claims SET released=?2 WHERE agent=?1 AND released IS NULL
             AND id NOT IN (SELECT claim FROM coord_held WHERE released IS NULL)
             AND work IN (SELECT id FROM coord_work WHERE state IN {ENDED})"
            ),
            params![agent, now],
        )
        .map_err(state)?;
    wake_waiters(db, now)?;
    Ok(released)
}

/// The change sets an agent's still-held claims wait on, at most `HELD_SETS_MAX`, so a
/// turn end can release those of a set that closed while its release failed. Takes the
/// database and the agent. Returns the set ids.
pub fn held_sets(db: &Connection, agent: AgentId) -> Result<Vec<String>, EngineError> {
    let mut statement = db
        .prepare(
            "SELECT DISTINCT h.change_set FROM coord_held h JOIN coord_claims c ON c.id=h.claim
             WHERE c.agent=?1 AND c.released IS NULL AND h.released IS NULL LIMIT ?2",
        )
        .map_err(state)?;
    let limit = i64::try_from(HELD_SETS_MAX).map_err(state)?;
    let sets = statement
        .query_map(params![agent, limit], |row| row.get::<_, String>(0))
        .map_err(state)?
        .collect::<Result<Vec<String>, _>>()
        .map_err(state)?;
    assert!(sets.len() <= HELD_SETS_MAX);
    Ok(sets)
}

/// The change sets any agent's still-held claims in a project wait on, at most `most`, so
/// the engine's sweep can release those of a set that closed while its release failed and
/// whose agent never has another turn end. Takes the database, the project and the most
/// sets to read. Returns the set ids.
pub fn held_sets_in(
    db: &Connection,
    project: &ProjectId,
    most: usize,
) -> Result<Vec<String>, EngineError> {
    let mut statement = db
        .prepare(
            "SELECT DISTINCT h.change_set FROM coord_claims c JOIN coord_held h ON h.claim=c.id
             WHERE c.project=?1 AND c.released IS NULL AND h.released IS NULL LIMIT ?2",
        )
        .map_err(state)?;
    let limit = i64::try_from(most).map_err(state)?;
    let sets = statement
        .query_map(params![project.as_str(), limit], |row| {
            row.get::<_, String>(0)
        })
        .map_err(state)?
        .collect::<Result<Vec<String>, _>>()
        .map_err(state)?;
    assert!(sets.len() <= most);
    Ok(sets)
}

/// The stopped or failed agents of a project whose claims on ended work no change set
/// holds, at most `HELD_SETS_MAX`, so the engine's sweep holds or releases them although
/// no turn end of theirs will. Takes the database and the project. Returns the agents.
pub fn stopped_with_unheld(
    db: &Connection,
    project: &ProjectId,
) -> Result<Vec<AgentId>, EngineError> {
    let mut statement = db
        .prepare(&format!(
            "SELECT DISTINCT c.agent FROM coord_claims c JOIN coord_work w ON w.id=c.work
             JOIN team_agents a ON a.id=c.agent
             WHERE c.project=?1 AND c.released IS NULL AND w.state IN {ENDED}
             AND a.status IN ('stopped','failed')
             AND NOT EXISTS (SELECT 1 FROM coord_held h WHERE h.claim=c.id AND h.released IS NULL)
             LIMIT ?2"
        ))
        .map_err(state)?;
    let limit = i64::try_from(HELD_SETS_MAX).map_err(state)?;
    let agents = statement
        .query_map(params![project.as_str(), limit], |row| {
            row.get::<_, AgentId>(0)
        })
        .map_err(state)?
        .collect::<Result<Vec<AgentId>, _>>()
        .map_err(state)?;
    assert!(agents.len() <= HELD_SETS_MAX);
    Ok(agents)
}

/// The other agents' active claims that cover one file, for the edit-time check that
/// warns or blocks under the "Overlapping edits" setting. A lead's and its sub-agent's
/// claims never count against each other. Takes the database, the project, the editing
/// agent and the file path relative to the repository root. Returns the claims, each
/// marked stale while its agent is away, or `coord_claim_invalid` for a path that is
/// not one plain file.
pub fn overlapping(
    db: &Connection,
    project: &ProjectId,
    agent: AgentId,
    path: &str,
) -> Result<Vec<Claim>, EngineError> {
    let file = normalize(path)?;
    if !matches!(Scope::of(&file), Scope::File(_)) {
        return Err(failure("coord_claim_invalid"));
    }
    let now = crate::team::now_ms();
    let mut kin = Kin::new(db);
    let mut found = Vec::new();
    for claim in active(db, project, now, Scan::Check)? {
        if claim.agent != agent
            && Scope::of(&claim.pattern).covers(&file)
            && !kin.related(agent, claim.agent)?
        {
            found.push(claim);
        }
    }
    Ok(found)
}

/// The overlaps of one unit of work's claims with every other agent's active claims, for
/// `start_work` to report to both sides. Takes the transaction, the project, the agent,
/// the work and the time in ms. Returns the overlaps.
pub(super) fn overlaps_for(
    tx: &Connection,
    project: &ProjectId,
    agent: AgentId,
    work: WorkId,
    now: i64,
) -> Result<Vec<Overlap>, EngineError> {
    let claims = active(tx, project, now, Scan::Check)?;
    let (mine, others): (Vec<Claim>, Vec<Claim>) = claims
        .into_iter()
        .partition(|claim| claim.agent == agent && claim.work == work);
    let mut kin = Kin::new(tx);
    let mut found = Vec::new();
    for theirs in others.iter().filter(|claim| claim.agent != agent) {
        if kin.related(agent, theirs.agent)? {
            continue;
        }
        for claim in &mine {
            if let Some(reason) = reason(&claim.pattern, &theirs.pattern) {
                found.push(Overlap {
                    mine: claim.clone(),
                    theirs: theirs.clone(),
                    reasons: vec![reason],
                    left_out: false,
                });
            }
        }
    }
    Ok(found)
}

/// Every overlap between active claims of different, unrelated agents of a project, each
/// pair once, for the graph's red lines and an agent's inbox, read from the oldest
/// `CLAIMS_SCAN_MAX` active claims. Takes the database, the project and the time in ms.
/// Returns the overlaps.
pub(super) fn all_overlaps(
    db: &Connection,
    project: &ProjectId,
    now: i64,
) -> Result<Vec<Overlap>, EngineError> {
    let claims = active(db, project, now, Scan::View)?;
    let mut kin = Kin::new(db);
    let mut found = Vec::new();
    for (index, mine) in claims.iter().enumerate() {
        for theirs in &claims[index + 1..] {
            if mine.agent == theirs.agent || kin.related(mine.agent, theirs.agent)? {
                continue;
            }
            if let Some(reason) = reason(&mine.pattern, &theirs.pattern) {
                found.push(Overlap {
                    mine: mine.clone(),
                    theirs: theirs.clone(),
                    reasons: vec![reason],
                    left_out: false,
                });
            }
        }
    }
    Ok(found)
}

/// The overlaps one agent's active claims have with other agents', each turned so
/// `mine` is that agent's claim. Takes the database, the project, the agent and the time
/// in ms. Returns the overlaps.
pub fn overlaps_of(
    db: &Connection,
    project: &ProjectId,
    agent: AgentId,
    now: i64,
) -> Result<Vec<Overlap>, EngineError> {
    Ok(all_overlaps(db, project, now)?
        .into_iter()
        .filter_map(|overlap| {
            if overlap.mine.agent == agent {
                Some(overlap)
            } else if overlap.theirs.agent == agent {
                Some(Overlap {
                    mine: overlap.theirs,
                    theirs: overlap.mine,
                    reasons: overlap.reasons,
                    left_out: overlap.left_out,
                })
            } else {
                None
            }
        })
        .collect())
}

/// The overlap a start under Wait waits on: the first that [`holds`] a file the start
/// claims. Input: the start's overlaps. Output: that overlap, if any.
pub(super) fn waits_on(overlaps: &[Overlap]) -> Option<&Overlap> {
    overlaps.iter().find(|overlap| holds(overlap))
}

/// Says whether an overlap's other claim, not stale, covers a file the start claims, with
/// a file, folder or glob, so Wait holds whatever Block holds. Input: the overlap. Output:
/// the answer.
fn holds(overlap: &Overlap) -> bool {
    !overlap.theirs.stale
        && matches!(Scope::of(&overlap.mine.pattern), Scope::File(file) if covers(&overlap.theirs.pattern, file))
}

/// Leaves out of a start every file claim another agent's live claim [`holds`], when its
/// wait would close a cycle, so each holder keeps editing its file and the starter's edits
/// of it stay refused (OS-8). The overlaps of the files left out stay, marked `left_out`;
/// the other overlaps of those claims go with them. Takes the start's transaction and its
/// overlaps. Returns Ok.
pub(super) fn leave_out(tx: &Connection, overlaps: &mut Vec<Overlap>) -> Result<(), EngineError> {
    let mut gone: Vec<i64> = overlaps
        .iter()
        .filter(|overlap| holds(overlap))
        .map(|overlap| overlap.mine.id)
        .collect();
    gone.sort_unstable();
    gone.dedup();
    for id in &gone {
        tx.execute("DELETE FROM coord_claims WHERE id=?1", params![id])
            .map_err(state)?;
    }
    overlaps.retain(|overlap| !gone.contains(&overlap.mine.id) || holds(overlap));
    for overlap in overlaps.iter_mut() {
        overlap.left_out = gone.contains(&overlap.mine.id);
    }
    Ok(())
}

/// Says whether a start's wait under Wait would close a cycle: the holder, or an agent it
/// waits on in turn, waits on work the starter owns. The walk reads at most
/// `WAIT_WALK_MAX` agents and counts a longer chain as a cycle, so no start waits on a
/// chain never read to its end. Takes the database, the starting agent and the holder.
/// Returns the answer.
pub(super) fn closes_cycle(
    db: &Connection,
    starter: AgentId,
    holder: AgentId,
) -> Result<bool, EngineError> {
    let mut statement = db
        .prepare(
            "SELECT DISTINCT k.owner_agent FROM coord_waits w JOIN coord_work k ON k.id=w.work
             WHERE w.agent=?1 AND k.owner_agent IS NOT NULL LIMIT ?2",
        )
        .map_err(state)?;
    let limit = i64::try_from(WAITS_PER_AGENT_MAX).map_err(state)?;
    let mut seen = vec![holder];
    let mut next = 0;
    while let Some(&at) = seen.get(next) {
        next += 1;
        if at == starter {
            return Ok(true);
        }
        let owners: Vec<AgentId> = statement
            .query_map(params![at, limit], |row| row.get(0))
            .map_err(state)?
            .collect::<Result<_, _>>()
            .map_err(state)?;
        for owner in owners {
            if seen.contains(&owner) {
                continue;
            }
            if seen.len() == WAIT_WALK_MAX {
                return Ok(true);
            }
            seen.push(owner);
        }
    }
    Ok(false)
}

/// Records that a start under Wait went ahead without the held file because its wait would
/// have closed a cycle, once per pair of agents, so [`wait_cycle_notices`] tells the owner
/// of it once. Takes the transaction, the project, the starter, the holder, the file and
/// the time in ms. Returns Ok.
pub(super) fn note_cycle(
    tx: &Connection,
    project: &ProjectId,
    starter: AgentId,
    holder: AgentId,
    file: &str,
    now: i64,
) -> Result<(), EngineError> {
    tx.execute(
        "INSERT OR IGNORE INTO coord_wait_cycles(project,first,second,holder,file,created)
         VALUES (?1,?2,?3,?4,?5,?6)",
        params![
            project.as_str(),
            starter.min(holder),
            starter.max(holder),
            holder,
            file,
            now
        ],
    )
    .map_err(state)?;
    Ok(())
}

/// Records that an agent waits for a file another agent's work holds through its claim
/// `held`, so a release that frees it prompts the agent, keeping its newest
/// `WAITS_PER_AGENT_MAX` waits. Takes the database, the project, the waiting agent, the
/// holding work, the file, the holding claim and the time in ms. Returns Ok.
pub(super) fn wait_for(
    db: &Connection,
    project: &ProjectId,
    agent: AgentId,
    work: WorkId,
    file: &str,
    held: &str,
    now: i64,
) -> Result<(), EngineError> {
    let change = Change::begin(db)?;
    let tx = change.db();
    tx.execute(
        "INSERT OR REPLACE INTO coord_waits(project,agent,work,file,held,created)
         VALUES (?1,?2,?3,?4,?5,?6)",
        params![project.as_str(), agent, work, file, held, now],
    )
    .map_err(state)?;
    let limit = i64::try_from(WAITS_PER_AGENT_MAX).map_err(state)?;
    tx.execute(
        "DELETE FROM coord_waits WHERE agent=?1 AND id NOT IN
         (SELECT id FROM coord_waits WHERE agent=?1 ORDER BY id DESC LIMIT ?2)",
        params![agent, limit],
    )
    .map_err(state)?;
    change.commit()
}

/// An agent's newest wait, for the refusal it reads: the holding work's owner, the work
/// and the file. Takes the database and the agent. Returns the wait, None without one.
pub fn latest_wait(
    db: &Connection,
    agent: AgentId,
) -> Result<Option<(AgentId, WorkId, String)>, EngineError> {
    use rusqlite::OptionalExtension;
    db.query_row(
        "SELECT k.owner_agent,w.work,w.file FROM coord_waits w JOIN coord_work k ON k.id=w.work
         WHERE w.agent=?1 AND k.owner_agent IS NOT NULL ORDER BY w.id DESC LIMIT 1",
        [agent],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )
    .optional()
    .map_err(state)
}

/// Settles the waits whose file is free now: the holding work's claim on it no longer
/// holds, as a start would see it (released, or its agent stopped, failed or away on work
/// it was still on), prompting each waiting agent once with the plan's sentence; a waiting
/// agent that stopped is forgotten unprompted. Each wait goes once settled, so no agent
/// waits for a file forever while the release or the engine's sweep runs; with nothing
/// due it only reads. A prompt the store refuses for a reason other than a store failure
/// drops its wait, so it never blocks the release. Takes the database and the time in ms.
/// Returns how many agents it prompted.
pub fn wake_waiters(db: &Connection, now: i64) -> Result<usize, EngineError> {
    let due: Vec<(i64, String, AgentId, WorkId, String, bool)> = {
        let mut statement = db
            .prepare(&format!(
                "SELECT w.id,w.project,w.agent,w.work,w.file,
                        a.status IS NOT NULL AND a.status NOT IN ('stopped','failed')
                 FROM coord_waits w LEFT JOIN team_agents a ON a.id=w.agent
                 WHERE a.status IS NULL OR a.status IN ('stopped','failed') OR NOT EXISTS
                 (SELECT 1 FROM coord_claims c JOIN coord_work k ON k.id=c.work
                  LEFT JOIN team_agents h ON h.id=c.agent
                  LEFT JOIN coord_activity v ON v.agent=c.agent
                  WHERE c.work=w.work AND c.released IS NULL AND c.pattern=w.held
                  AND k.state IN {HOLDING}
                  AND (k.state IN {ENDED} OR (h.status IS NOT NULL AND h.status NOT IN ('stopped','failed')
                       AND COALESCE(v.last_seen, h.created_ms) > ?2)))
                 ORDER BY w.id LIMIT ?1"
            ))
            .map_err(state)?;
        let limit = i64::try_from(WAKES_MAX).map_err(state)?;
        let away = super::presence::away_at_or_before(now);
        statement
            .query_map(params![limit, away], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            })
            .map_err(state)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(state)?
    };
    assert!(due.len() <= WAKES_MAX);
    if due.is_empty() {
        return Ok(0);
    }
    let change = Change::begin(db)?;
    let tx = change.db();
    let mut prompted = 0;
    for (id, project, agent, work, file, live) in due {
        let settled = tx
            .execute("DELETE FROM coord_waits WHERE id=?1", [id])
            .map_err(state)?;
        if settled == 0 || !live {
            continue;
        }
        let message = NewMessage {
            project: ProjectId::new(&project).map_err(|_| failure("coord_project_unknown"))?,
            from: Party::Engine,
            to: Party::Agent(agent),
            kind: MessageKind::Prompt,
            question: None,
            work: None,
            reply_to: None,
            body: format!("{file} is free now: start your work again."),
            idempotency_key: Some(format!("wait-{work}-{id}")),
        };
        match super::messages::send(tx, message, now) {
            Ok(_) => prompted += 1,
            Err(problem) if problem.0.starts_with("coord_state_unavailable") => {
                return Err(problem);
            }
            Err(_) => {}
        }
    }
    change.commit()?;
    Ok(prompted)
}

/// The waits under Wait that would have closed a cycle and the owner has not heard of yet,
/// each pair once: the agent whose start went ahead without the file, the agent holding it
/// and the file. Marks them told. Takes the database, the project and the time in ms.
/// Returns the pairs.
pub fn wait_cycle_notices(
    db: &Connection,
    project: &ProjectId,
    now: i64,
) -> Result<Vec<(AgentId, AgentId, String)>, EngineError> {
    let limit = i64::try_from(CYCLE_NOTICES_MAX).map_err(state)?;
    let untold: Vec<(AgentId, AgentId, AgentId, String)> = {
        let mut statement = db
            .prepare(
                "SELECT first,second,holder,file FROM coord_wait_cycles WHERE project=?1 AND told IS NULL
                 ORDER BY created,first,second LIMIT ?2",
            )
            .map_err(state)?;
        statement
            .query_map(params![project.as_str(), limit], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
            })
            .map_err(state)?
            .collect::<Result<_, _>>()
            .map_err(state)?
    };
    assert!(untold.len() <= CYCLE_NOTICES_MAX);
    if untold.is_empty() {
        return Ok(Vec::new());
    }
    let change = Change::begin(db)?;
    let tx = change.db();
    for (first, second, _, _) in &untold {
        tx.execute(
            "UPDATE coord_wait_cycles SET told=?4 WHERE project=?1 AND first=?2 AND second=?3",
            params![project.as_str(), first, second, now],
        )
        .map_err(state)?;
    }
    change.commit()?;
    Ok(untold
        .into_iter()
        .map(|(first, second, holder, file)| {
            (if holder == first { second } else { first }, holder, file)
        })
        .collect())
}

/// The lead and sub-agent answers one overlap check already read, so a check over many
/// claims asks the store once per pair of agents.
struct Kin<'a> {
    db: &'a Connection,
    known: HashMap<(AgentId, AgentId), bool>,
}

impl<'a> Kin<'a> {
    fn new(db: &'a Connection) -> Self {
        Self {
            db,
            known: HashMap::new(),
        }
    }

    /// Says whether two agents are lead and sub-agent of each other, as `related` does.
    fn related(&mut self, a: AgentId, b: AgentId) -> Result<bool, EngineError> {
        let pair = (a.min(b), a.max(b));
        if let Some(known) = self.known.get(&pair) {
            return Ok(*known);
        }
        let answer = related(self.db, a, b)?;
        self.known.insert(pair, answer);
        Ok(answer)
    }
}

/// The query over a project's claims one overlap check reads, selecting `columns`, oldest
/// first up to a limit: not released, on work in a `HOLDING` state, and, unless the work
/// ended, of an agent that is not stopped, failed or gone, so claims built up by agents
/// that never run again stop no one. They are kept, not released: a resumed agent keeps
/// its id and its claims count again. Input: the columns. Output: the SQL, taking the
/// project and the limit.
fn scanned(columns: &str) -> String {
    format!(
        "SELECT {columns} FROM coord_claims c JOIN coord_work w ON w.id=c.work
         LEFT JOIN team_agents a ON a.id=c.agent
         WHERE c.project=?1 AND c.released IS NULL AND w.state IN {HOLDING}
         AND (w.state IN {ENDED} OR (a.status IS NOT NULL AND a.status NOT IN ('stopped','failed')))
         ORDER BY c.id LIMIT ?2"
    )
}

/// Reads a project's claims that still hold scope, as [`scanned`] selects them, each
/// marked stale while its agent is Away or Stopped. A claim on ended work is never stale:
/// it holds until the agent's change set lands or is rejected (DEC-117 Q7). Past
/// `CLAIMS_SCAN_MAX` a check fails closed with `coord_claims_full`, so no overlap it
/// decides on is silently missed, and a view stops at the oldest `CLAIMS_SCAN_MAX`.
fn active(
    db: &Connection,
    project: &ProjectId,
    now: i64,
    scan: Scan,
) -> Result<Vec<Claim>, EngineError> {
    let mut statement = db
        .prepare(&scanned(&format!(
            "c.id,c.agent,c.work,c.pattern,w.state IN {ENDED}"
        )))
        .map_err(state)?;
    let limit = i64::try_from(CLAIMS_SCAN_MAX + 1).map_err(state)?;
    let rows = statement
        .query_map(params![project.as_str(), limit], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, bool>(4)?,
            ))
        })
        .map_err(state)?;
    let mut away: HashMap<AgentId, bool> = HashMap::new();
    let mut claims = Vec::new();
    for row in rows {
        let (id, agent, work, pattern, ended) = row.map_err(state)?;
        if claims.len() == CLAIMS_SCAN_MAX {
            if scan == Scan::Check {
                return Err(failure("coord_claims_full"));
            }
            break;
        }
        let stale = !ended
            && match away.get(&agent).copied() {
                Some(known) => known,
                None => {
                    let gone = matches!(
                        super::presence::current(db, agent, now)?,
                        Presence::Away | Presence::Stopped
                    );
                    away.insert(agent, gone);
                    gone
                }
            };
        claims.push(Claim {
            id,
            agent,
            work,
            pattern,
            stale,
        });
    }
    assert!(claims.len() <= CLAIMS_SCAN_MAX);
    Ok(claims)
}

/// The plain reason two stored claims may touch the same thing, after Concord, at file
/// level (F31): the same file, a file inside a folder or glob, nested folders, globs whose
/// fixed beginnings meet, or modules or domains sharing a word. Two different files never
/// overlap, even in one folder. Takes both claims. Returns the reason, or None when they
/// cannot overlap.
fn reason(mine: &str, theirs: &str) -> Option<String> {
    let place = |folder: &str| {
        if folder.is_empty() {
            "the whole repository".to_owned()
        } else {
            folder.to_owned()
        }
    };
    match (Scope::of(mine), Scope::of(theirs)) {
        (Scope::File(a), Scope::File(b)) => (a == b).then(|| format!("same file {a}")),
        (Scope::File(file), other @ (Scope::Folder(_) | Scope::Glob(_)))
        | (other @ (Scope::Folder(_) | Scope::Glob(_)), Scope::File(file)) => {
            other.covers(file).then(|| match other {
                Scope::Folder(folder) => format!("{file} is inside {}", place(folder)),
                Scope::Glob(glob) => format!("{file} matches {glob}"),
                _ => unreachable!(),
            })
        }
        (Scope::Folder(a), Scope::Folder(b)) => (a.starts_with(b) || b.starts_with(a))
            .then(|| format!("{} and {} nest", place(a), place(b))),
        (Scope::Folder(a), Scope::Glob(b))
        | (Scope::Glob(b), Scope::Folder(a))
        | (Scope::Glob(a), Scope::Glob(b)) => {
            let (a_fixed, b_fixed) = (fixed(a), fixed(b));
            (a_fixed.starts_with(b_fixed) || b_fixed.starts_with(a_fixed))
                .then(|| format!("{} and {b} may overlap", place(a)))
        }
        (
            Scope::Label {
                module: a_module,
                tokens: a,
            },
            Scope::Label {
                module: b_module,
                tokens: b,
            },
        ) if a_module == b_module => {
            let shared: Vec<&str> = a
                .iter()
                .filter(|token| b.contains(token))
                .map(String::as_str)
                .collect();
            (!shared.is_empty()).then(|| {
                let what = if a_module { "module" } else { "domain" };
                format!("shared {what} words: {}", shared.join(", "))
            })
        }
        _ => None,
    }
}

/// The folder part of a glob before its first wildcard, which every path it matches
/// starts with.
fn fixed(glob: &str) -> &str {
    let literal = glob.find(['*', '?']).map_or(glob, |at| &glob[..at]);
    literal.rfind('/').map_or("", |at| &literal[..=at])
}

/// Splits a module or domain label into lower-case words, so "Todo Frontend",
/// "todo-frontend" and "frontend/todo" share their words (Concord), sorted and without
/// repeats.
fn tokens(label: &str) -> Vec<String> {
    let mut words: Vec<String> = label
        .to_lowercase()
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(str::to_owned)
        .collect();
    words.sort();
    words.dedup();
    words
}

/// Matches a repository path against a claim glob: `*` and `?` stay inside one folder,
/// `**` crosses folders and `**/` also matches no folder at all. Runs in time
/// proportional to the two lengths multiplied, both bounded by `PATTERN_BYTES_MAX`.
fn glob_match(glob: &[u8], path: &[u8]) -> bool {
    enum Token {
        Byte(u8),
        One,
        Star,
        Any,
        AnyFolders,
    }
    let mut tokens = Vec::with_capacity(glob.len());
    let mut at = 0;
    while at < glob.len() {
        let token = match (glob[at], glob.get(at + 1), glob.get(at + 2)) {
            (b'*', Some(b'*'), Some(b'/')) => {
                at += 3;
                Token::AnyFolders
            }
            (b'*', Some(b'*'), _) => {
                at += 2;
                Token::Any
            }
            (b'*', ..) => {
                at += 1;
                Token::Star
            }
            (b'?', ..) => {
                at += 1;
                Token::One
            }
            (byte, ..) => {
                at += 1;
                Token::Byte(byte)
            }
        };
        tokens.push(token);
    }
    let mut row = vec![false; path.len() + 1];
    row[0] = true;
    for token in &tokens {
        let mut next = vec![false; path.len() + 1];
        let mut reached = false;
        for end in 0..=path.len() {
            next[end] = match token {
                Token::Byte(byte) => end > 0 && row[end - 1] && path[end - 1] == *byte,
                Token::One => end > 0 && row[end - 1] && path[end - 1] != b'/',
                Token::Star => row[end] || (end > 0 && next[end - 1] && path[end - 1] != b'/'),
                Token::Any => row[end] || (end > 0 && next[end - 1]),
                Token::AnyFolders => row[end] || (end > 0 && reached && path[end - 1] == b'/'),
            };
            reached |= row[end];
        }
        row = next;
    }
    row[path.len()]
}

/// Tells whether the owner should now hear that a project's live claims pass what one
/// overlap check reads: once per time it fills, recorded in `coord_claims_full` and cleared
/// once it is no longer full, so a project that fills again is told again. Inputs: the
/// store, the project and the time in ms. Output: true the first time a full project is
/// seen, false otherwise.
pub fn claims_full_notice(
    db: &Connection,
    project: &ProjectId,
    now: i64,
) -> Result<bool, EngineError> {
    let limit = i64::try_from(CLAIMS_SCAN_MAX + 1).map_err(state)?;
    let read: i64 = db
        .query_row(
            &format!("SELECT COUNT(*) FROM ({})", scanned("1")),
            params![project.as_str(), limit],
            |row| row.get(0),
        )
        .map_err(state)?;
    let read = usize::try_from(read).map_err(state)?;
    assert!(read <= CLAIMS_SCAN_MAX + 1);
    if read > CLAIMS_SCAN_MAX {
        let told = db
            .execute(
                "INSERT OR IGNORE INTO coord_claims_full(project,told) VALUES (?1,?2)",
                params![project.as_str(), now],
            )
            .map_err(state)?;
        return Ok(told == 1);
    }
    db.execute(
        "DELETE FROM coord_claims_full WHERE project=?1",
        [project.as_str()],
    )
    .map_err(state)?;
    Ok(false)
}
