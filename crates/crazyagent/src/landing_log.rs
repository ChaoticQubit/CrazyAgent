use crate::engine::EngineError;
use crate::engine::owner::failure;
use crate::state_log::replay::{self, Clock, Logged};
use crate::state_log::{Aggregate, Appender, Draft};
use rusqlite::{Connection, Transaction, TransactionBehavior, params};
use serde::{Deserialize, Serialize};

/// The body schema of every landing event.
pub const SCHEMA: u16 = 1;
/// An agent worktree's task branch stored (`landing_agent_branches`).
pub const AGENT_BRANCH_SET: &str = "AgentBranchSet";
/// An agent worktree's task branch forgotten.
pub const AGENT_BRANCH_FORGOTTEN: &str = "AgentBranchForgotten";
/// The commit a worktree's next change set opens above stored (`landing_bases`).
pub const BASE_SET: &str = "BaseSet";
/// An owner edit kept waiting in an agent worktree for its next turn end
/// (`landing_owner_edits`).
pub const OWNER_EDIT_KEPT: &str = "OwnerEditKept";
/// A worktree's waiting owner edits settled on the change set its turn end recorded.
pub const OWNER_EDITS_SETTLED: &str = "OwnerEditsSettled";
/// A worktree's waiting owner edits dropped by a turn end that left no change set open.
pub const OWNER_EDITS_DROPPED: &str = "OwnerEditsDropped";
/// The owner edits settled on a change set forgotten once no review can show them.
pub const OWNER_EDITS_FORGOTTEN: &str = "OwnerEditsForgotten";
/// Every owner edit of a removed worktree forgotten.
pub const WORKTREE_OWNER_EDITS_FORGOTTEN: &str = "WorktreeOwnerEditsForgotten";
/// A change set stored before numbers given the next number of its project
/// (`landing_change_set_numbers`), by the numbering pass at the owner upkeep.
pub const CHANGE_SET_NUMBERED: &str = "ChangeSetNumbered";
/// An owner approval made a baseline its project's current one (`verification_current`).
pub const BASELINE_APPROVED: &str = "BaselineApproved";
/// A Not now to a project's publish prompt, kept with the time it asks again
/// (`github_publish_prompt`).
pub const PUBLISH_PROMPT_SNOOZED: &str = "PublishPromptSnoozed";
/// A Don't ask again to a project's publish prompt.
pub const PUBLISH_PROMPT_NEVER: &str = "PublishPromptNever";
/// Every landing event kind.
pub const KINDS: [&str; 12] = [
    AGENT_BRANCH_SET,
    AGENT_BRANCH_FORGOTTEN,
    BASE_SET,
    OWNER_EDIT_KEPT,
    OWNER_EDITS_SETTLED,
    OWNER_EDITS_DROPPED,
    OWNER_EDITS_FORGOTTEN,
    WORKTREE_OWNER_EDITS_FORGOTTEN,
    CHANGE_SET_NUMBERED,
    BASELINE_APPROVED,
    PUBLISH_PROMPT_SNOOZED,
    PUBLISH_PROMPT_NEVER,
];

/// Whether a body path is absolute: `/`, then names `owner_edit::check_path` accepts, so
/// no free text rides in it (v2.5 L6 Q11). Input: the path. Output: the answer.
fn absolute(path: &str) -> bool {
    path.strip_prefix('/')
        .is_some_and(|rest| crate::landing::owner_edit::check_path(rest).is_ok())
}

/// The task branch the engine named for an agent worktree: both paths absolute and the
/// branch one `agent_git::is_agent_branch` accepts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentBranch {
    pub worktree: String,
    pub repository: String,
    pub branch: String,
}

/// The commit a worktree's next change set opens above: an absolute worktree and a 40 or 64
/// lowercase hex object id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Base {
    pub worktree: String,
    pub base: String,
}

/// Whether a text is only lowercase hex digits. Input: the text. Output: the answer.
fn lower_hex(id: &str) -> bool {
    id.bytes()
        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// Whether a text is a git object id: 40 or 64 lowercase hex digits. Input: the text.
/// Output: the answer.
fn object_id(id: &str) -> bool {
    matches!(id.len(), 40 | 64) && lower_hex(id)
}

/// Whether a text is a change set id: 32 lowercase hex digits, as `ChangeSetId::new`
/// checks. Input: the text. Output: the answer.
fn change_set_id(id: &str) -> bool {
    id.len() == 32 && lower_hex(id)
}

/// An owner edit kept waiting: an absolute worktree, a path `owner_edit::check_path`
/// accepts and the edit's time in ms, at most `i64::MAX`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OwnerEdit {
    pub worktree: String,
    pub path: String,
    pub at_ms: u64,
}

/// A worktree's waiting owner edits settled on a change set: an absolute worktree and a
/// change set id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settled {
    pub worktree: String,
    pub change_set: String,
}

/// A change set's number in its project: a change set id, a project id `ProjectId::new`
/// accepts and a number from 1 within `i64`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Numbered {
    pub change_set: String,
    pub project: String,
    pub number: u64,
}

/// An approved baseline made its project's current one: a project id and a baseline id,
/// each one the protocol's identity accepts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Approved {
    pub project: String,
    pub baseline: String,
}

/// A Not now to a project's publish prompt: the project id and the time in ms it asks again,
/// from 0.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Snoozed {
    pub project: String,
    pub until_ms: i64,
}

/// A project an event is about, its id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Project {
    pub project: String,
}

/// A change set an event is about, its id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChangeSet {
    pub change_set: String,
}

/// A worktree an event is about, an absolute path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Worktree {
    pub worktree: String,
}

/// One landing change, the event live writes append and replay applies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    AgentBranchSet(AgentBranch),
    AgentBranchForgotten(Worktree),
    BaseSet(Base),
    OwnerEditKept(OwnerEdit),
    OwnerEditsSettled(Settled),
    OwnerEditsDropped(Worktree),
    OwnerEditsForgotten(ChangeSet),
    WorktreeOwnerEditsForgotten(Worktree),
    ChangeSetNumbered(Numbered),
    BaselineApproved(Approved),
    PublishPromptSnoozed(Snoozed),
    PublishPromptNever(Project),
}

impl Change {
    /// Whether every field is closed: absolute worktrees, plain relative paths, an agent
    /// branch, object and change set ids, a time in `i64`. Input: none. Output: the answer.
    fn closed(&self) -> bool {
        match self {
            Self::AgentBranchSet(set) => {
                absolute(&set.worktree)
                    && absolute(&set.repository)
                    && crate::agent_git::is_agent_branch(&set.branch)
            }
            Self::AgentBranchForgotten(gone) => absolute(&gone.worktree),
            Self::BaseSet(set) => absolute(&set.worktree) && object_id(&set.base),
            Self::OwnerEditKept(kept) => {
                absolute(&kept.worktree)
                    && crate::landing::owner_edit::check_path(&kept.path).is_ok()
                    && i64::try_from(kept.at_ms).is_ok()
            }
            Self::OwnerEditsSettled(set) => {
                absolute(&set.worktree) && change_set_id(&set.change_set)
            }
            Self::OwnerEditsDropped(gone) | Self::WorktreeOwnerEditsForgotten(gone) => {
                absolute(&gone.worktree)
            }
            Self::OwnerEditsForgotten(gone) => change_set_id(&gone.change_set),
            Self::ChangeSetNumbered(set) => {
                change_set_id(&set.change_set)
                    && crate::protocol::ProjectId::new(set.project.as_str()).is_ok()
                    && set.number >= 1
                    && i64::try_from(set.number).is_ok()
            }
            Self::BaselineApproved(approved) => {
                crate::protocol::ProjectId::new(approved.project.as_str()).is_ok()
                    && crate::protocol::BaselineId::new(approved.baseline.as_str()).is_ok()
            }
            Self::PublishPromptSnoozed(snoozed) => {
                crate::protocol::ProjectId::new(snoozed.project.as_str()).is_ok()
                    && snoozed.until_ms >= 0
            }
            Self::PublishPromptNever(never) => {
                crate::protocol::ProjectId::new(never.project.as_str()).is_ok()
            }
        }
    }

    /// The schema of a table the change writes that its module makes on first use, which
    /// apply makes too, so a replay into a store without it holds. Input: none. Output: the
    /// schema, or None for a table the store's open makes.
    fn schema(&self) -> Option<&'static str> {
        match self {
            Self::OwnerEditKept(_)
            | Self::OwnerEditsSettled(_)
            | Self::OwnerEditsDropped(_)
            | Self::OwnerEditsForgotten(_)
            | Self::WorktreeOwnerEditsForgotten(_) => Some(crate::landing::owner_edit::SCHEMA),
            Self::PublishPromptSnoozed(_) | Self::PublishPromptNever(_) => {
                Some(crate::github::PROMPT_SCHEMA)
            }
            Self::AgentBranchSet(_)
            | Self::AgentBranchForgotten(_)
            | Self::BaseSet(_)
            | Self::ChangeSetNumbered(_)
            | Self::BaselineApproved(_) => None,
        }
    }

    /// The most rows the change may touch: one for a single-row write, the waiting bound
    /// for a settle or drop, none for a forget, which spans a change set or worktree.
    /// Input: none. Output: the bound.
    fn rows_max(&self) -> Option<i64> {
        use crate::landing::owner_edit::PATHS_PER_WORKTREE_MAX;
        match self {
            Self::AgentBranchSet(_)
            | Self::AgentBranchForgotten(_)
            | Self::BaseSet(_)
            | Self::OwnerEditKept(_)
            | Self::ChangeSetNumbered(_)
            | Self::BaselineApproved(_)
            | Self::PublishPromptSnoozed(_)
            | Self::PublishPromptNever(_) => Some(1),
            Self::OwnerEditsSettled(_) | Self::OwnerEditsDropped(_) => Some(PATHS_PER_WORKTREE_MAX),
            Self::OwnerEditsForgotten(_) | Self::WorktreeOwnerEditsForgotten(_) => None,
        }
    }

    /// The change's kind. Input: none. Output: one of [`KINDS`].
    pub fn kind(&self) -> &'static str {
        let kind = match self {
            Self::AgentBranchSet(_) => AGENT_BRANCH_SET,
            Self::AgentBranchForgotten(_) => AGENT_BRANCH_FORGOTTEN,
            Self::BaseSet(_) => BASE_SET,
            Self::OwnerEditKept(_) => OWNER_EDIT_KEPT,
            Self::OwnerEditsSettled(_) => OWNER_EDITS_SETTLED,
            Self::OwnerEditsDropped(_) => OWNER_EDITS_DROPPED,
            Self::OwnerEditsForgotten(_) => OWNER_EDITS_FORGOTTEN,
            Self::WorktreeOwnerEditsForgotten(_) => WORKTREE_OWNER_EDITS_FORGOTTEN,
            Self::ChangeSetNumbered(_) => CHANGE_SET_NUMBERED,
            Self::BaselineApproved(_) => BASELINE_APPROVED,
            Self::PublishPromptSnoozed(_) => PUBLISH_PROMPT_SNOOZED,
            Self::PublishPromptNever(_) => PUBLISH_PROMPT_NEVER,
        };
        assert!(KINDS.contains(&kind), "the landing kind {kind} is in KINDS");
        kind
    }

    /// The event body, the change's fields as one JSON object. Input: none. Output: the
    /// bytes, or `landing_log_body` for a field that is not closed.
    pub fn body(&self) -> Result<Vec<u8>, EngineError> {
        if !self.closed() {
            return Err(failure("landing_log_body"));
        }
        let body = match self {
            Self::AgentBranchSet(set) => serde_json::to_vec(set),
            Self::AgentBranchForgotten(gone) => serde_json::to_vec(gone),
            Self::BaseSet(set) => serde_json::to_vec(set),
            Self::OwnerEditKept(kept) => serde_json::to_vec(kept),
            Self::OwnerEditsSettled(set) => serde_json::to_vec(set),
            Self::OwnerEditsDropped(gone) | Self::WorktreeOwnerEditsForgotten(gone) => {
                serde_json::to_vec(gone)
            }
            Self::OwnerEditsForgotten(gone) => serde_json::to_vec(gone),
            Self::ChangeSetNumbered(set) => serde_json::to_vec(set),
            Self::BaselineApproved(approved) => serde_json::to_vec(approved),
            Self::PublishPromptSnoozed(snoozed) => serde_json::to_vec(snoozed),
            Self::PublishPromptNever(never) => serde_json::to_vec(never),
        };
        body.map_err(|_| failure("landing_log_body"))
    }

    /// Reads a logged landing event back as its change: the aggregate landing, the schema
    /// [`SCHEMA`], the body exactly the kind's fields and every field closed. Input: the
    /// event. Output: the change, or `landing_log_event`.
    pub fn parse(event: &Logged) -> Result<Self, EngineError> {
        let refused = || failure("landing_log_event");
        if event.aggregate != Aggregate::Landing || event.schema != SCHEMA {
            return Err(refused());
        }
        let body = event.body.as_slice();
        let change = match event.kind.as_str() {
            AGENT_BRANCH_SET => serde_json::from_slice(body).map(Self::AgentBranchSet),
            AGENT_BRANCH_FORGOTTEN => serde_json::from_slice(body).map(Self::AgentBranchForgotten),
            BASE_SET => serde_json::from_slice(body).map(Self::BaseSet),
            OWNER_EDIT_KEPT => serde_json::from_slice(body).map(Self::OwnerEditKept),
            OWNER_EDITS_SETTLED => serde_json::from_slice(body).map(Self::OwnerEditsSettled),
            OWNER_EDITS_DROPPED => serde_json::from_slice(body).map(Self::OwnerEditsDropped),
            OWNER_EDITS_FORGOTTEN => serde_json::from_slice(body).map(Self::OwnerEditsForgotten),
            WORKTREE_OWNER_EDITS_FORGOTTEN => {
                serde_json::from_slice(body).map(Self::WorktreeOwnerEditsForgotten)
            }
            CHANGE_SET_NUMBERED => serde_json::from_slice(body).map(Self::ChangeSetNumbered),
            BASELINE_APPROVED => serde_json::from_slice(body).map(Self::BaselineApproved),
            PUBLISH_PROMPT_SNOOZED => serde_json::from_slice(body).map(Self::PublishPromptSnoozed),
            PUBLISH_PROMPT_NEVER => serde_json::from_slice(body).map(Self::PublishPromptNever),
            _ => return Err(refused()),
        };
        let change = change.map_err(|_| refused())?;
        if !change.closed() {
            return Err(refused());
        }
        Ok(change)
    }
}

/// Logs one landing change in the caller's transaction: appends it at a revision of its own
/// behind the store's engine host (L6 `store_fence`), then applies it through the
/// projection. The caller commits. Inputs: the transaction and the change. Output: the
/// event's seq, or the fence's, the log's or the table's refusal.
pub(crate) fn record(tx: &Transaction<'_>, change: &Change) -> Result<u64, EngineError> {
    record_all(tx, std::slice::from_ref(change))
}

/// Logs one write group of landing changes in the caller's transaction at one revision of
/// its own behind the store's engine host. The caller commits. Inputs: the transaction and
/// the changes, at least one and at most `TRANSACTION_EVENTS_MAX`. Output: the last event's
/// seq, or the first refusal, which the caller rolls back.
pub(crate) fn record_all(tx: &Transaction<'_>, changes: &[Change]) -> Result<u64, EngineError> {
    let fence = crate::worktrees::log::store_fence(tx)?;
    let mut appender = Appender::own(tx, fence)?;
    let seq = append_all(&mut appender, changes)?;
    assert!(appender.revision() > 0, "own moved the revision");
    Ok(seq)
}

/// Logs one landing change inside an owner request whose next revision is already signed:
/// moves `state` from one before it to it, then appends and applies the change there behind
/// the store's engine host, so the request's own `events` row at next covers it (as L6
/// `record_at`, DEC 250-L9-LANDING C4). Inputs: the request's transaction, its next revision
/// and the change. Output: the event's seq, `state_log_revision_stale`, or the refusal.
pub(crate) fn record_at(
    tx: &Transaction<'_>,
    next: u64,
    change: &Change,
) -> Result<u64, EngineError> {
    let stale = || failure("state_log_revision_stale");
    let before = next.checked_sub(1).ok_or_else(stale)?;
    let next_signed = i64::try_from(next).map_err(|_| stale())?;
    let moved = tx
        .execute(
            "UPDATE state SET revision=?1 WHERE id=1 AND revision=?2",
            params![next_signed, i64::try_from(before).map_err(|_| stale())?],
        )
        .map_err(|_| failure("landing_log_unavailable"))?;
    if moved != 1 {
        return Err(stale());
    }
    let fence = crate::worktrees::log::store_fence(tx)?;
    let mut appender = Appender::owner(tx, next, fence)?;
    let seq = append_all(&mut appender, std::slice::from_ref(change))?;
    assert_eq!(
        appender.revision(),
        next,
        "the change is logged at the request's revision"
    );
    Ok(seq)
}

/// Appends and applies a write group in order: every body is checked first, then each
/// change is appended and applied through the appender's projection. Inputs: the appender
/// and the changes, at least one and at most `TRANSACTION_EVENTS_MAX`. Output: the last
/// event's seq, or the first refusal.
fn append_all(appender: &mut Appender<'_, '_>, changes: &[Change]) -> Result<u64, EngineError> {
    assert!(
        !changes.is_empty(),
        "a write group logs at least one change"
    );
    assert!(changes.len() <= crate::state_log::TRANSACTION_EVENTS_MAX);
    let bodies = changes
        .iter()
        .map(Change::body)
        .collect::<Result<Vec<_>, _>>()?;
    let mut last = 0;
    for (change, body) in changes.iter().zip(&bodies) {
        last = appender.append(&Draft {
            aggregate: Aggregate::Landing,
            kind: change.kind(),
            schema: SCHEMA,
            at_ms: Clock::System.now_ms(),
            body,
        })?;
        appender.project(|tx| apply(tx, change))?;
    }
    assert!(last > 0, "a recorded landing event has a seq");
    Ok(last)
}

/// Writes one landing change in a transaction of its own: opens it, records the event and
/// commits. Every landing writer runs outside a transaction (none runs inside the open's
/// sweep), so one called inside a caller's transaction is refused rather than left without
/// its event. Inputs: the connection and the change. Output: Ok, or the refusal
/// (`landing_log_unavailable` inside a transaction), which writes nothing.
pub(crate) fn write(db: &Connection, change: &Change) -> Result<(), EngineError> {
    if !db.is_autocommit() {
        return Err(failure("landing_log_unavailable"));
    }
    let tx = Transaction::new_unchecked(db, TransactionBehavior::Immediate)
        .map_err(|_| failure("landing_log_unavailable"))?;
    record(&tx, change)?;
    tx.commit()
        .map_err(|_| failure("landing_log_unavailable"))?;
    assert!(db.is_autocommit(), "the landing write committed");
    Ok(())
}

/// Applies one landing change to its table with the same statements live and in replay,
/// within the change's row bound. Inputs: the connection and the change. Output: Ok, or the
/// table's refusal.
pub fn apply(db: &Connection, change: &Change) -> Result<(), EngineError> {
    let unavailable = |_| failure("changes_unavailable");
    if let Some(schema) = change.schema() {
        db.execute_batch(schema).map_err(unavailable)?;
    }
    let changed = statements(db, change).map_err(unavailable)?;
    let changed = i64::try_from(changed).map_err(|_| failure("changes_unavailable"))?;
    let bound = change.rows_max();
    assert!(
        bound.is_none_or(|max| changed <= max),
        "a landing change stays in its row bound"
    );
    Ok(())
}

/// Runs one landing change's statements. Inputs: the connection and the change. Output:
/// the rows the last statement changed, or the database's refusal.
fn statements(db: &Connection, change: &Change) -> rusqlite::Result<usize> {
    match change {
        Change::AgentBranchSet(set) => db.execute(
            "INSERT OR REPLACE INTO landing_agent_branches(worktree,repository,branch) \
             VALUES (?1,?2,?3)",
            params![set.worktree, set.repository, set.branch],
        ),
        Change::AgentBranchForgotten(gone) => db.execute(
            "DELETE FROM landing_agent_branches WHERE worktree=?1",
            [&gone.worktree],
        ),
        Change::BaseSet(set) => db.execute(
            "INSERT OR REPLACE INTO landing_bases(worktree,base) VALUES (?1,?2)",
            params![set.worktree, set.base],
        ),
        Change::OwnerEditKept(kept) => db.execute(
            "INSERT INTO landing_owner_edits(worktree, path, at_ms) VALUES (?1,?2,?3) \
             ON CONFLICT(worktree, change_set, path) DO UPDATE SET at_ms=excluded.at_ms",
            params![kept.worktree, kept.path, kept.at_ms],
        ),
        Change::OwnerEditsSettled(set) => db.execute(
            "UPDATE OR REPLACE landing_owner_edits SET change_set=?2 \
             WHERE worktree=?1 AND change_set=''",
            params![set.worktree, set.change_set],
        ),
        Change::OwnerEditsDropped(gone) => db.execute(
            "DELETE FROM landing_owner_edits WHERE worktree=?1 AND change_set=''",
            [&gone.worktree],
        ),
        Change::OwnerEditsForgotten(gone) => db.execute(
            "DELETE FROM landing_owner_edits WHERE change_set=?1",
            [&gone.change_set],
        ),
        Change::WorktreeOwnerEditsForgotten(gone) => db.execute(
            "DELETE FROM landing_owner_edits WHERE worktree=?1",
            [&gone.worktree],
        ),
        Change::ChangeSetNumbered(set) => db.execute(
            "INSERT INTO landing_change_set_numbers(change_set,project,number) VALUES (?1,?2,?3)",
            params![set.change_set, set.project, set.number],
        ),
        Change::BaselineApproved(approved) => db
            .execute(
                "UPDATE verification_baselines SET approved=1 WHERE id=?1",
                [&approved.baseline],
            )
            .and_then(|_| {
                db.execute(
                    "INSERT INTO verification_current VALUES (?1,?2) \
                     ON CONFLICT(project) DO UPDATE SET baseline=excluded.baseline",
                    params![approved.project, approved.baseline],
                )
            }),
        Change::PublishPromptSnoozed(snoozed) => db.execute(
            "INSERT INTO github_publish_prompt(project, snoozed_until, never) VALUES (?1, ?2, 0) \
             ON CONFLICT(project) DO UPDATE SET snoozed_until = excluded.snoozed_until",
            params![snoozed.project, snoozed.until_ms],
        ),
        Change::PublishPromptNever(never) => db.execute(
            "INSERT INTO github_publish_prompt(project, snoozed_until, never) VALUES (?1, NULL, 1) \
             ON CONFLICT(project) DO UPDATE SET never = 1",
            [&never.project],
        ),
    }
}

/// The tables the landing log rebuilds, emptied before its replay.
const TABLES: [&str; 6] = [
    "landing_agent_branches",
    "landing_bases",
    "landing_owner_edits",
    "landing_change_set_numbers",
    "verification_current",
    "github_publish_prompt",
];

/// The first snapshot run of the landing tables, for the schema step that puts landing on
/// the log (step 14): rows written before the log reach replay through it, through the
/// open's proof. A store without the `state` table or with no landing row gets no run, so
/// it moves no revision. Inputs: the step's transaction and the open's proof. Output:
/// whether a run was appended, or the log's refusal.
#[cfg(any(unix, test))]
pub(crate) fn first_run(
    tx: &Transaction<'_>,
    opening: crate::storage::opening::Opening,
) -> Result<bool, EngineError> {
    let _ = (tx, opening);
    Ok(false)
}

/// The most project removals a rebuild reads, as L6's bound.
const REMOVALS_MAX: usize = 65_536;

/// Rebuilds the tables on the landing log from it: empties each one the store has, then
/// replays every landing event; a logged project Delete forgets its project's publish prompt
/// answer at its place in the log, as its projection does (L6 Q10). Input: the store.
/// Output: the last landing seq read, or the refusal.
pub fn rebuild(db: &Connection) -> Result<u64, EngineError> {
    let mut deletes = deletes(db)?.into_iter().peekable();
    clear(db)?;
    let last = replay::replay(db, Aggregate::Landing, 0, |event| {
        while let Some((_, project)) = deletes.next_if(|(seq, _)| *seq < event.seq) {
            crate::github::forget_prompt(db, &project)?;
        }
        apply(db, &Change::parse(event)?)
    })?;
    for (_, project) in deletes {
        crate::github::forget_prompt(db, &project)?;
    }
    Ok(last)
}

/// Empties each table on the landing log the store has. Input: the store. Output: Ok, or
/// `changes_unavailable`.
fn clear(db: &Connection) -> Result<(), EngineError> {
    let unavailable = |_| failure("changes_unavailable");
    for table in TABLES {
        let kept: bool = db
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name=?1)",
                [table],
                |row| row.get(0),
            )
            .map_err(unavailable)?;
        if kept {
            db.execute(&format!("DELETE FROM {table}"), [])
                .map_err(unavailable)?;
        }
    }
    Ok(())
}

/// Each logged project Delete, a removal that forgets the project's notes, as its seq and
/// project, in log order. Input: the store. Output: the deletes, `landing_log_removals` past
/// [`REMOVALS_MAX`] removals, or `landing_log_event` for a removal body that is not one.
fn deletes(db: &Connection) -> Result<Vec<(u64, crate::protocol::ProjectId)>, EngineError> {
    use crate::project_setup::removed::event as removed;
    let mut found = Vec::new();
    let mut read = 0;
    replay::replay(db, Aggregate::Projects, 0, |logged| {
        if logged.kind != removed::KIND {
            return Ok(());
        }
        read += 1;
        if read > REMOVALS_MAX {
            return Err(failure("landing_log_removals"));
        }
        let removal: removed::ProjectRemoved =
            serde_json::from_slice(&logged.body).map_err(|_| failure("landing_log_event"))?;
        if !removal.kept {
            found.push((logged.seq, removal.project));
        }
        Ok(())
    })?;
    assert!(found.len() <= REMOVALS_MAX);
    assert!(found.windows(2).all(|pair| pair[0].0 < pair[1].0));
    Ok(found)
}

/// Seeds a test store for logged landing writes, as ide::log::seed does. Input: the store.
/// Output: the running host.
#[cfg(test)]
pub(crate) fn seed(db: &Connection) -> crate::engine::owner::HostStamp {
    crate::ide::log::seed(db)
}
