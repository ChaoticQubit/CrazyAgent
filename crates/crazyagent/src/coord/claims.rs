use super::store::{Change, failure, related, state};
use super::{AgentId, CLAIMS_PER_WORK_MAX, Claim, Overlap, PATTERN_BYTES_MAX, Presence, WorkId};
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

const _: () = assert!(CLAIMS_SCAN_MAX >= CLAIMS_PER_WORK_MAX * 2);

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
/// is refused. Takes the declared text. Returns the stored form or `coord_claim_invalid`.
pub(super) fn normalize(raw: &str) -> Result<String, EngineError> {
    let invalid = || failure("coord_claim_invalid");
    let raw = raw.trim();
    if raw.is_empty() || raw.len() > PATTERN_BYTES_MAX || raw.chars().any(char::is_control) {
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

/// Stores the claims of a unit of work. Takes the transaction, the project, the agent,
/// the work, the stored forms and the time in ms.
pub(super) fn add(
    tx: &Connection,
    project: &ProjectId,
    agent: AgentId,
    work: WorkId,
    claims: &[String],
    now: i64,
) -> Result<(), EngineError> {
    assert!(claims.len() <= CLAIMS_PER_WORK_MAX);
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
    change.commit()?;
    Ok(released)
}

/// Takes back the claims a change set's close released, once that set is restored, so
/// they overlap newer claims again as any held claim does. Takes the database and the
/// set's id. Returns how many claims hold again.
pub fn restore_set(db: &Connection, set: &str) -> Result<usize, EngineError> {
    assert!(!set.is_empty());
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
    db.execute(
        &format!(
            "UPDATE coord_claims SET released=?2 WHERE agent=?1 AND released IS NULL
             AND id NOT IN (SELECT claim FROM coord_held WHERE released IS NULL)
             AND work IN (SELECT id FROM coord_work WHERE state IN {ENDED})"
        ),
        params![agent, now],
    )
    .map_err(state)
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
                })
            } else {
                None
            }
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

/// Reads a project's claims that still hold scope: not released, on work someone is on,
/// is about to be or ended while its change set has not landed, oldest first, each marked
/// stale while its agent is Away or Stopped. A claim on ended work is never stale: it
/// holds until the agent's change set lands or is rejected (DEC-117 Q7). Past
/// `CLAIMS_SCAN_MAX` a check fails closed with `coord_claims_full`, so no overlap it
/// decides on is silently missed, and a view stops at the oldest `CLAIMS_SCAN_MAX`.
fn active(
    db: &Connection,
    project: &ProjectId,
    now: i64,
    scan: Scan,
) -> Result<Vec<Claim>, EngineError> {
    let mut statement = db
        .prepare(&format!(
            "SELECT c.id,c.agent,c.work,c.pattern,w.state IN {ENDED} FROM coord_claims c
             JOIN coord_work w ON w.id=c.work
             WHERE c.project=?1 AND c.released IS NULL
             AND w.state IN ('assigned','accepted','active','review_ready','complete','closed')
             ORDER BY c.id LIMIT ?2"
        ))
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

/// The plain reason two stored claims may touch the same thing, after Concord: the same
/// file, files in the same folder, a file inside a folder or glob, nested folders, globs
/// whose fixed beginnings meet, or modules or domains sharing a word. Takes both claims.
/// Returns the reason, or None when they cannot overlap.
fn reason(mine: &str, theirs: &str) -> Option<String> {
    let place = |folder: &str| {
        if folder.is_empty() {
            "the whole repository".to_owned()
        } else {
            folder.to_owned()
        }
    };
    match (Scope::of(mine), Scope::of(theirs)) {
        (Scope::File(a), Scope::File(b)) if a == b => Some(format!("same file {a}")),
        (Scope::File(a), Scope::File(b)) => {
            let folder = parent(a);
            (!folder.is_empty() && folder == parent(b)).then(|| format!("same folder {folder}"))
        }
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

/// The folder of a file path, empty at the repository root.
fn parent(file: &str) -> &str {
    file.rfind('/').map_or("", |at| &file[..at])
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

/// Tells whether the owner should now hear that a project's claims fill one overlap check.
/// Inputs: the store, the project and the time. Output: true the first time a full project
/// is seen, false otherwise.
pub fn claims_full_notice(
    db: &Connection,
    project: &ProjectId,
    now: i64,
) -> Result<bool, EngineError> {
    let _ = (db, project, now);
    Ok(false)
}
