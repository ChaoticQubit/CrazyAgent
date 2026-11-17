use crate::engine::EngineError;
use crate::engine::owner::failure;
use crate::state_log::replay::{self, Clock, Logged};
use crate::state_log::{Aggregate, Appender, BODY_BYTES_MAX, Chunk, Draft, SNAPSHOT_CHUNK};

mod kept;

use kept::{
    CHANGE_SETS, KEPT_TABLES, ROW_PARTS_MAX, RULE_REQUESTS, VERIFICATION_TABLES, keep_row,
    kept_rows, release_row, restore_kept, upsert_kept,
};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde::{Deserialize, Serialize};

/// The body schema of every landing event.
pub const SCHEMA: u16 = 1;
/// An agent worktree's task branch stored (`landing_agent_branches`).
pub const AGENT_BRANCH_SET: &str = "AgentBranchSet";
/// An agent worktree's task branch forgotten.
pub const AGENT_BRANCH_FORGOTTEN: &str = "AgentBranchForgotten";
/// The commit a worktree's next change set opens above stored (`landing_bases`).
pub const BASE_SET: &str = "BaseSet";
/// The base of a gone worktree forgotten (`landing_bases`).
pub const BASE_FORGOTTEN: &str = "BaseForgotten";
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
/// A change set saved, its summary or its landing result stored (`landing_change_sets`,
/// `landing_change_set_numbers`): the stored row is an owner text, the body its part digests.
pub const CHANGE_SET_SAVED: &str = "ChangeSetSaved";
/// A GitHub rule request written (`github_rule_requests`): the stored row is an owner text,
/// the body its part digests (QUESTION-1120 A).
pub const RULE_REQUEST_SAVED: &str = "RuleRequestSaved";
/// A verification baseline, candidate or attempt written: the stored row is an owner text,
/// the body its table, id, project and part digests (QUESTION-1132 A).
pub const VERIFICATION_SAVED: &str = "VerificationSaved";
/// A verification candidate, attempt or baseline the 128-row cap pruned: the body its table
/// and id (QUESTION-1132 A).
pub const VERIFICATION_FORGOTTEN: &str = "VerificationForgotten";
/// A Not now to a project's publish prompt, kept with the time it asks again
/// (`github_publish_prompt`).
pub const PUBLISH_PROMPT_SNOOZED: &str = "PublishPromptSnoozed";
/// A Don't ask again to a project's publish prompt.
pub const PUBLISH_PROMPT_NEVER: &str = "PublishPromptNever";
/// Every landing event kind.
pub const KINDS: [&str; 17] = [
    AGENT_BRANCH_SET,
    AGENT_BRANCH_FORGOTTEN,
    BASE_SET,
    BASE_FORGOTTEN,
    OWNER_EDIT_KEPT,
    OWNER_EDITS_SETTLED,
    OWNER_EDITS_DROPPED,
    OWNER_EDITS_FORGOTTEN,
    WORKTREE_OWNER_EDITS_FORGOTTEN,
    CHANGE_SET_NUMBERED,
    BASELINE_APPROVED,
    PUBLISH_PROMPT_SNOOZED,
    PUBLISH_PROMPT_NEVER,
    CHANGE_SET_SAVED,
    RULE_REQUEST_SAVED,
    VERIFICATION_SAVED,
    VERIFICATION_FORGOTTEN,
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

/// A change set's stored row: its id, project and number (none for a set an engine without
/// numbers stored) and the digests of the row's cells, kept as an owner text in 1 to 64 parts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Saved {
    pub change_set: String,
    pub project: String,
    pub row: Vec<crate::owner_texts::TextDigest>,
    pub number: Option<u64>,
}

/// A GitHub rule request's stored row: its id and project and the digests of the row's
/// cells, kept as an owner text in parts (QUESTION-1120 A).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Requested {
    pub request: u64,
    pub project: String,
    pub row: Vec<crate::owner_texts::TextDigest>,
}

/// A verification table's stored row: the table, the row's id, its project (a baseline's
/// only, as only that table has one) and the digests of the row's cells, kept as an owner
/// text in parts (QUESTION-1132 A).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Verified {
    pub table: String,
    pub id: String,
    pub project: Option<String>,
    pub row: Vec<crate::owner_texts::TextDigest>,
}

/// A verification table's row the 128-row cap pruned: the table and the row's id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Pruned {
    pub table: String,
    pub id: String,
}

/// Whether a verification body names one of the three tables and an id that table's rows
/// take: a baseline id for baselines, a request id for candidates and attempts. Inputs:
/// the table and the id. Output: the answer.
fn verification_key(table: &str, id: &str) -> bool {
    match VERIFICATION_TABLES.iter().position(|known| *known == table) {
        Some(0) => crate::protocol::BaselineId::new(id).is_ok(),
        Some(_) => crate::protocol::RequestId::new(id).is_ok(),
        None => false,
    }
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
    BaseForgotten(Worktree),
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
    ChangeSetSaved(Saved),
    RuleRequestSaved(Requested),
    VerificationSaved(Verified),
    VerificationForgotten(Pruned),
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
            Self::AgentBranchForgotten(gone) | Self::BaseForgotten(gone) => {
                absolute(&gone.worktree)
            }
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
            Self::RuleRequestSaved(requested) => {
                (1..=ROW_PARTS_MAX).contains(&requested.row.len())
                    && i64::try_from(requested.request).is_ok_and(|request| request >= 1)
                    && crate::protocol::ProjectId::new(requested.project.as_str()).is_ok()
            }
            Self::ChangeSetSaved(saved) => {
                (1..=ROW_PARTS_MAX).contains(&saved.row.len())
                    && change_set_id(&saved.change_set)
                    && crate::protocol::ProjectId::new(saved.project.as_str()).is_ok()
                    && saved
                        .number
                        .is_none_or(|number| number >= 1 && i64::try_from(number).is_ok())
            }
            Self::VerificationSaved(saved) => {
                (1..=ROW_PARTS_MAX).contains(&saved.row.len())
                    && verification_key(&saved.table, &saved.id)
                    && saved.project.is_some() == (saved.table == VERIFICATION_TABLES[0])
                    && saved
                        .project
                        .as_deref()
                        .is_none_or(|project| crate::protocol::ProjectId::new(project).is_ok())
            }
            Self::VerificationForgotten(gone) => verification_key(&gone.table, &gone.id),
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
            | Self::BaseForgotten(_)
            | Self::ChangeSetNumbered(_)
            | Self::BaselineApproved(_)
            | Self::ChangeSetSaved(_)
            | Self::RuleRequestSaved(_)
            | Self::VerificationSaved(_)
            | Self::VerificationForgotten(_) => None,
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
            | Self::BaseForgotten(_)
            | Self::OwnerEditKept(_)
            | Self::ChangeSetNumbered(_)
            | Self::BaselineApproved(_)
            | Self::PublishPromptSnoozed(_)
            | Self::PublishPromptNever(_)
            | Self::ChangeSetSaved(_)
            | Self::RuleRequestSaved(_)
            | Self::VerificationSaved(_)
            | Self::VerificationForgotten(_) => Some(1),
            Self::OwnerEditsSettled(_) | Self::OwnerEditsDropped(_) => Some(PATHS_PER_WORKTREE_MAX),
            Self::OwnerEditsForgotten(_) | Self::WorktreeOwnerEditsForgotten(_) => None,
        }
    }

    /// The change's kind. Input: none. Output: one of [`KINDS`].
    pub fn kind(&self) -> &'static str {
        let kind = match self {
            Self::AgentBranchSet(_) => AGENT_BRANCH_SET,
            Self::AgentBranchForgotten(_) => AGENT_BRANCH_FORGOTTEN,
            Self::BaseForgotten(_) => BASE_FORGOTTEN,
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
            Self::ChangeSetSaved(_) => CHANGE_SET_SAVED,
            Self::RuleRequestSaved(_) => RULE_REQUEST_SAVED,
            Self::VerificationSaved(_) => VERIFICATION_SAVED,
            Self::VerificationForgotten(_) => VERIFICATION_FORGOTTEN,
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
            Self::AgentBranchForgotten(gone) | Self::BaseForgotten(gone) => {
                serde_json::to_vec(gone)
            }
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
            Self::ChangeSetSaved(saved) => serde_json::to_vec(saved),
            Self::RuleRequestSaved(requested) => serde_json::to_vec(requested),
            Self::VerificationSaved(saved) => serde_json::to_vec(saved),
            Self::VerificationForgotten(gone) => serde_json::to_vec(gone),
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
            BASE_FORGOTTEN => serde_json::from_slice(body).map(Self::BaseForgotten),
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
            CHANGE_SET_SAVED => serde_json::from_slice(body).map(Self::ChangeSetSaved),
            RULE_REQUEST_SAVED => serde_json::from_slice(body).map(Self::RuleRequestSaved),
            VERIFICATION_SAVED => serde_json::from_slice(body).map(Self::VerificationSaved),
            VERIFICATION_FORGOTTEN => serde_json::from_slice(body).map(Self::VerificationForgotten),
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
    record_all_at(tx, next, std::slice::from_ref(change))
}

/// Logs a write group inside an owner request whose next revision is already signed, as
/// [`record_at`] logs one change. Inputs: the request's transaction, its next revision and
/// the changes, at least one and at most `TRANSACTION_EVENTS_MAX`. Output: the last event's
/// seq, `state_log_revision_stale`, or the refusal.
fn record_all_at(tx: &Transaction<'_>, next: u64, changes: &[Change]) -> Result<u64, EngineError> {
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
    let seq = append_all(&mut appender, changes)?;
    assert_eq!(
        appender.revision(),
        next,
        "the change is logged at the request's revision"
    );
    Ok(seq)
}

/// One verification row an owner request wrote or the 128-row cap pruned: the table, one of
/// [`VERIFICATION_TABLES`], the row's id and whether it was pruned (QUESTION-1132 A).
pub(crate) type VerificationWrite = (&'static str, String, bool);

/// Logs the verification rows an applied owner request wrote and pruned at its signed next
/// revision, in write order: a written row kept as an owner text and logged as
/// `VerificationSaved`, a pruned one released and logged as `VerificationForgotten`. Inputs:
/// the request's transaction, its next revision and the writes, at least one. Output: the
/// last event's seq, `landing_log_row` for a written row the store lacks, or the refusal.
pub(crate) fn record_verification_at(
    tx: &Transaction<'_>,
    next: u64,
    writes: &[VerificationWrite],
) -> Result<u64, EngineError> {
    assert!(
        !writes.is_empty(),
        "an applied request logs the rows it wrote"
    );
    let mut changes = Vec::with_capacity(writes.len());
    for (table, id, pruned) in writes {
        assert!(VERIFICATION_TABLES.contains(table), "a verification table");
        let key = serde_json::json!(id);
        let change = if *pruned {
            release_row(tx, table, &key)?;
            Change::VerificationForgotten(Pruned {
                table: (*table).to_owned(),
                id: id.clone(),
            })
        } else {
            let (project, row) =
                keep_row(tx, table, &key)?.ok_or_else(|| failure("landing_log_row"))?;
            Change::VerificationSaved(Verified {
                table: (*table).to_owned(),
                id: id.clone(),
                project,
                row,
            })
        };
        changes.push(change);
    }
    assert_eq!(changes.len(), writes.len());
    record_all_at(tx, next, &changes)
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
/// commits. A writer inside a transaction logs through [`record`], [`record_at`] or the
/// open's proof instead, so this one called inside a caller's transaction is refused rather
/// than left without its event. Inputs: the connection and the change. Output: Ok, or the
/// refusal (`landing_log_unavailable` inside a transaction), which writes nothing.
pub(crate) fn write(db: &Connection, change: &Change) -> Result<(), EngineError> {
    within(db, |tx| {
        if forgets_nothing(tx, change)? {
            return Ok(());
        }
        record(tx, change).map(drop)
    })
}

/// Whether a change forgets a worktree's rows when none is left (rv-sec3 #415 F1): such a
/// forget changes nothing, so it logs nothing, and a retire after a Delete everything
/// names no forgotten path again. Inputs: the transaction and the change. Output: the
/// answer, or `changes_unavailable`.
fn forgets_nothing(tx: &Transaction<'_>, change: &Change) -> Result<bool, EngineError> {
    let (table, waiting, gone) = match change {
        Change::AgentBranchForgotten(gone) => ("landing_agent_branches", false, gone),
        Change::BaseForgotten(gone) => ("landing_bases", false, gone),
        Change::OwnerEditsDropped(gone) => ("landing_owner_edits", true, gone),
        Change::WorktreeOwnerEditsForgotten(gone) => ("landing_owner_edits", false, gone),
        _ => return Ok(false),
    };
    assert!(WORKTREE_TABLES.contains(&table));
    assert!(!waiting || table == "landing_owner_edits");
    if !has(tx, table)? {
        return Ok(true);
    }
    let only = if waiting { " AND change_set=''" } else { "" };
    let held: bool = tx
        .query_row(
            &format!("SELECT EXISTS(SELECT 1 FROM {table} WHERE worktree=?1{only})"),
            [&gone.worktree],
            |row| row.get(0),
        )
        .map_err(|_| failure("changes_unavailable"))?;
    Ok(!held)
}

/// Runs one landing write group in a transaction of its own, as [`write`] does: the group
/// writes and records through the transaction, which then commits. Refused inside a
/// caller's transaction. Inputs: the connection and the group. Output: the group's value,
/// or the refusal (`landing_log_unavailable` or the group's) with nothing written.
pub(crate) fn within<T>(
    db: &Connection,
    group: impl FnOnce(&Transaction<'_>) -> Result<T, EngineError>,
) -> Result<T, EngineError> {
    if !db.is_autocommit() {
        return Err(failure("landing_log_unavailable"));
    }
    let tx = Transaction::new_unchecked(db, TransactionBehavior::Immediate)
        .map_err(|_| failure("landing_log_unavailable"))?;
    let value = group(&tx)?;
    tx.commit()
        .map_err(|_| failure("landing_log_unavailable"))?;
    assert!(db.is_autocommit(), "the landing write committed");
    Ok(value)
}

/// Logs a change set's stored row in the writer's transaction (QUESTION-1056 A): its cells
/// by column name are kept as an owner text under the set's row holder, which releases the
/// text an earlier save kept, and `ChangeSetSaved` carries the digest and the set's number.
/// Inputs: the transaction and the set's id. Output: whether the set has a row to log, or
/// the refusal.
pub(crate) fn record_row(tx: &Transaction<'_>, id: &str) -> Result<bool, EngineError> {
    let Some((project, parts)) = keep_row(tx, CHANGE_SETS, &serde_json::json!(id))? else {
        return Ok(false);
    };
    let project = project.ok_or_else(|| failure("landing_log_row"))?;
    let refused = || failure("landing_log_row");
    let number: Option<i64> = tx
        .query_row(
            "SELECT number FROM landing_change_set_numbers WHERE change_set=?1",
            [id],
            |row| row.get(0),
        )
        .optional()
        .map_err(|_| failure("changes_unavailable"))?;
    let number = number
        .map(u64::try_from)
        .transpose()
        .map_err(|_| refused())?;
    let saved = Saved {
        change_set: id.to_owned(),
        project,
        row: parts,
        number,
    };
    record(tx, &Change::ChangeSetSaved(saved))?;
    assert!(!tx.is_autocommit());
    Ok(true)
}

/// The rule request a writer stored, logged as `RuleRequestSaved` in the writer's
/// transaction (QUESTION-1120 A). Inputs: the transaction and the request's id. Output:
/// whether the request has a row to log, or the refusal.
pub(crate) fn record_request(tx: &Transaction<'_>, id: i64) -> Result<bool, EngineError> {
    let Some(change) = request_change(tx, id)? else {
        return Ok(false);
    };
    record(tx, &change)?;
    assert!(!tx.is_autocommit());
    Ok(true)
}

/// The rule requests the storage open ended or marked told, logged through the open's
/// proof (QUESTION-1120 A); none logs nothing and moves no revision. Inputs: the open's
/// transaction, its proof and the requests' ids. Output: Ok, or the refusal.
pub(crate) fn record_requests_opening(
    tx: &Transaction<'_>,
    opening: crate::storage::opening::Opening,
    ids: &[i64],
) -> Result<(), EngineError> {
    let mut changes = Vec::with_capacity(ids.len());
    for id in ids {
        changes.extend(request_change(tx, *id)?);
    }
    if changes.is_empty() {
        return Ok(());
    }
    let mut appender = Appender::opening(tx, opening)?;
    for group in changes.chunks(crate::state_log::TRANSACTION_EVENTS_MAX) {
        append_all(&mut appender, group)?;
    }
    assert!(appender.revision() > 0, "the opening moved the revision");
    assert!(changes.len() <= ids.len());
    Ok(())
}

/// One rule request's stored row as its logged change, the row kept in parts. Inputs: the
/// transaction and the id. Output: the change, none without the row, or the refusal.
fn request_change(tx: &Transaction<'_>, id: i64) -> Result<Option<Change>, EngineError> {
    let Some((project, row)) = keep_row(tx, RULE_REQUESTS, &serde_json::json!(id))? else {
        return Ok(None);
    };
    let project = project.ok_or_else(|| failure("landing_log_row"))?;
    let request = u64::try_from(id).map_err(|_| failure("landing_log_row"))?;
    assert!(request >= 1, "a rule request id is positive");
    Ok(Some(Change::RuleRequestSaved(Requested {
        request,
        project,
        row,
    })))
}

/// Applies a logged change set save: its kept row text upserted by column name, and its
/// number. A save whose text a later one released is skipped (superseded). Inputs: the
/// connection and the save. Output: Ok, `landing_log_row`, `landing_log_row_cell` or the
/// store's refusal.
fn apply_saved(db: &Connection, saved: &Saved) -> Result<(), EngineError> {
    let key = serde_json::json!(saved.change_set);
    if !upsert_kept(
        db,
        CHANGE_SETS,
        &key,
        Some(saved.project.as_str()),
        &saved.row,
    )? {
        return Ok(());
    }
    if let Some(number) = saved.number {
        let number = i64::try_from(number).map_err(|_| failure("landing_log_row"))?;
        db.execute(
            "INSERT INTO landing_change_set_numbers(change_set,project,number) \
             VALUES (?1,?2,?3) ON CONFLICT(change_set) DO NOTHING",
            params![saved.change_set, saved.project, number],
        )
        .map_err(|_| failure("changes_unavailable"))?;
    }
    Ok(())
}

/// Applies one landing change to its table with the same statements live and in replay,
/// within the change's row bound. Inputs: the connection and the change. Output: Ok, or the
/// table's refusal.
pub fn apply(db: &Connection, change: &Change) -> Result<(), EngineError> {
    if let Change::ChangeSetSaved(saved) = change {
        return apply_saved(db, saved);
    }
    if let Change::RuleRequestSaved(requested) = change {
        let key = serde_json::json!(requested.request);
        let project = Some(requested.project.as_str());
        return upsert_kept(db, RULE_REQUESTS, &key, project, &requested.row).map(drop);
    }
    if let Change::VerificationSaved(_) | Change::VerificationForgotten(_) = change {
        return apply_verification(db, change);
    }
    let unavailable = |error: rusqlite::Error| store_refusal(change, &error);
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

/// Applies a logged verification row write: a save upserts the row from its kept text,
/// skipped when a later write released it, and a prune deletes the row. Inputs: the
/// connection and the change. Output: Ok, `landing_log_row` for a table that is not one of
/// the three, or the store's refusal with its text, as verification refused before the log.
fn apply_verification(db: &Connection, change: &Change) -> Result<(), EngineError> {
    let (name, id) = match change {
        Change::VerificationSaved(saved) => (&saved.table, &saved.id),
        Change::VerificationForgotten(gone) => (&gone.table, &gone.id),
        _ => unreachable!("apply hands over only a verification change"),
    };
    let table = VERIFICATION_TABLES
        .into_iter()
        .find(|table| *table == name.as_str())
        .ok_or_else(|| failure("landing_log_row"))?;
    let key = serde_json::json!(id);
    if let Change::VerificationSaved(saved) = change {
        return upsert_kept(db, table, &key, saved.project.as_deref(), &saved.row).map(drop);
    }
    let deleted = db
        .execute(&format!("DELETE FROM {table} WHERE id=?1"), [id])
        .map_err(|error| EngineError(error.to_string()))?;
    assert!(deleted <= 1, "a prune deletes at most its row");
    assert!(VERIFICATION_TABLES.contains(&table));
    Ok(())
}

/// The refusal of a change whose table write the store failed: an approval keeps the store's
/// error text, as verification refused before the log, and every other landing change the
/// fixed `changes_unavailable` its readers match. Inputs: the change and the store error.
/// Output: the refusal.
fn store_refusal(change: &Change, error: &rusqlite::Error) -> EngineError {
    let refusal = match change {
        Change::BaselineApproved(_) => EngineError(error.to_string()),
        _ => failure("changes_unavailable"),
    };
    assert!(!refusal.0.is_empty(), "a refusal names its cause");
    refusal
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
        Change::BaseForgotten(gone) => db.execute(
            "DELETE FROM landing_bases WHERE worktree=?1",
            [&gone.worktree],
        ),
        Change::BaseSet(set) => db.execute(
            "INSERT OR REPLACE INTO landing_bases(worktree,base) VALUES (?1,?2)",
            params![set.worktree, set.base],
        ),
        Change::OwnerEditKept(kept) => db.execute(
            "INSERT INTO landing_owner_edits(worktree, path, at_ms) VALUES (?1,?2,?3) \
             ON CONFLICT(worktree, change_set, path) DO UPDATE SET at_ms=excluded.at_ms",
            params![kept.worktree, kept.path, stored(kept.at_ms)?],
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
            params![set.change_set, set.project, stored(set.number)?],
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
        Change::ChangeSetSaved(_) | Change::RuleRequestSaved(_) => unreachable!("apply keeps it"),
        Change::VerificationSaved(_) | Change::VerificationForgotten(_) => unreachable!("apply"),
    }
}

/// A u64 body value as SQLite stores it; a closed body holds it within i64, so the refusal
/// is never reached from a parsed or built change. Input: the value. Output: the value, or
/// a conversion failure.
fn stored(value: u64) -> rusqlite::Result<i64> {
    i64::try_from(value).map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))
}

/// The tables on the landing log, each with its columns in schema order and, for a table its
/// module makes on first use, its schema: emptied before a replay and carried whole by a
/// snapshot run.
const TABLES: [(&str, &str, Option<&str>); 6] = [
    ("landing_agent_branches", "worktree,repository,branch", None),
    ("landing_bases", "worktree,base", None),
    (
        "landing_owner_edits",
        "worktree,path,at_ms,change_set",
        Some(crate::landing::owner_edit::SCHEMA),
    ),
    (
        "landing_change_set_numbers",
        "change_set,project,number",
        None,
    ),
    ("verification_current", "project,baseline", None),
    (
        "github_publish_prompt",
        "project,snoozed_until,never",
        Some(crate::github::PROMPT_SCHEMA),
    ),
];

/// The most rows of one table a snapshot run carries, as L6's bound.
const SNAPSHOT_ROWS_MAX: usize = 65_536;

/// Room in one chunk body for its envelope (aggregate, table and seq), as L6's.
const CHUNK_ENVELOPE_BYTES: usize = 1_024;

/// The first snapshot run of the landing tables, for the schema step that puts landing on
/// the log (step 15): rows written before the log reach replay through it, through the
/// open's proof. A store without the `state` table or with no landing row gets no run, so
/// it moves no revision. Inputs: the step's transaction and the open's proof. Output:
/// whether a run was appended, or the log's refusal.
#[cfg(any(unix, test))]
pub(crate) fn first_run(
    tx: &Transaction<'_>,
    opening: crate::storage::opening::Opening,
) -> Result<bool, EngineError> {
    if !has(tx, "state")? {
        return Ok(false);
    }
    let mut rows: i64 = 0;
    let tables = TABLES.map(|(table, _, _)| table);
    for table in KEPT_TABLES.into_iter().chain(tables) {
        if has(tx, table)? {
            let count: i64 = tx
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                    row.get(0)
                })
                .map_err(|_| failure("changes_unavailable"))?;
            rows = rows.saturating_add(count);
        }
    }
    if rows == 0 {
        return Ok(false);
    }
    let at_ms = opening.at_ms();
    let mut appender = Appender::opening(tx, opening)?;
    let done = snapshot(&mut appender, at_ms)?;
    assert!(done > 0, "a run closes at a seq");
    assert!(rows > 0);
    Ok(true)
}

/// Appends a snapshot run of every landing table as it stands, each in chunks that fit one
/// event body and at least one chunk each (a table the store lacks is one empty chunk), so
/// replay of a run always clears and fills them all. The kept tables lead, each row as its
/// id, project and part digests, the row kept as an owner text. Inputs: the appender and the
/// writer's clock. Output: the seq of the run's closing event, or `landing_log_snapshot_rows`,
/// `landing_log_snapshot_cell`, `landing_log_row_too_large` or the log's refusal.
pub fn snapshot(appender: &mut Appender<'_, '_>, at_ms: u64) -> Result<u64, EngineError> {
    let mut chunks = Vec::new();
    for table in KEPT_TABLES {
        chunks.extend(pack(table, appender.project(|tx| kept_rows(tx, table))?)?);
    }
    for (table, columns, _) in TABLES {
        let rows = appender.project(|tx| table_rows(tx, table, columns))?;
        chunks.extend(pack(table, rows)?);
    }
    assert!(
        chunks.len() >= KEPT_TABLES.len() + TABLES.len(),
        "one chunk or more per table"
    );
    assert_eq!(chunks[0].table, CHANGE_SETS);
    appender.snapshot(Aggregate::Landing, at_ms, chunks)
}

/// Every row of one landing table in column order, each as a JSON array of its cells; a
/// table the store lacks has none. Inputs: the connection, the table and its columns.
/// Output: the rows, `landing_log_snapshot_rows` past [`SNAPSHOT_ROWS_MAX`], or
/// `landing_log_snapshot_cell` for a cell a snapshot does not carry.
fn table_rows(
    db: &Connection,
    table: &str,
    columns: &str,
) -> Result<Vec<serde_json::Value>, EngineError> {
    if !has(db, table)? {
        return Ok(Vec::new());
    }
    let width = columns.split(',').count();
    let unavailable = |_| failure("changes_unavailable");
    let mut statement = db
        .prepare(&format!(
            "SELECT {columns} FROM {table} ORDER BY {columns} LIMIT ?1"
        ))
        .map_err(unavailable)?;
    let limit =
        i64::try_from(SNAPSHOT_ROWS_MAX + 1).map_err(|_| failure("landing_log_snapshot_rows"))?;
    let stored = statement
        .query_map([limit], |row| {
            (0..width)
                .map(|column| row.get::<_, rusqlite::types::Value>(column))
                .collect::<rusqlite::Result<Vec<_>>>()
        })
        .map_err(unavailable)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(unavailable)?;
    if stored.len() > SNAPSHOT_ROWS_MAX {
        return Err(failure("landing_log_snapshot_rows"));
    }
    let rows = stored
        .into_iter()
        .map(|row| {
            let cells = row.into_iter().map(cell).collect::<Result<Vec<_>, _>>()?;
            Ok(serde_json::Value::Array(cells))
        })
        .collect::<Result<Vec<_>, EngineError>>()?;
    assert!((2..=4).contains(&width));
    assert!(
        rows.iter()
            .all(|row| row.as_array().is_some_and(|row| row.len() == width))
    );
    Ok(rows)
}

/// One stored cell as a snapshot carries it. Input: the cell. Output: its text, integer or
/// null, or `landing_log_snapshot_cell` for a real or a blob, which no landing column holds.
fn cell(value: rusqlite::types::Value) -> Result<serde_json::Value, EngineError> {
    use rusqlite::types::Value as Stored;
    match value {
        Stored::Null => Ok(serde_json::Value::Null),
        Stored::Integer(number) => Ok(serde_json::Value::from(number)),
        Stored::Text(text) => Ok(serde_json::Value::String(text)),
        Stored::Real(_) | Stored::Blob(_) => Err(failure("landing_log_snapshot_cell")),
    }
}

/// Splits one table's rows, in order, into chunks whose rows fit one event body with room
/// for the chunk's envelope; a table with no rows is one empty chunk. Inputs: the table and
/// its rows. Output: the chunks, or `landing_log_row_too_large`.
fn pack(table: &str, rows: Vec<serde_json::Value>) -> Result<Vec<Chunk>, EngineError> {
    assert!(rows.len() <= SNAPSHOT_ROWS_MAX);
    let budget = BODY_BYTES_MAX - CHUNK_ENVELOPE_BYTES;
    let total = rows.len();
    let empty = || Chunk {
        table: table.into(),
        rows: Vec::new(),
    };
    let mut chunks = vec![empty()];
    let mut used = 0;
    for row in rows {
        let bytes = serde_json::to_vec(&row)
            .map_err(|_| failure("landing_log_row_too_large"))?
            .len()
            + 1;
        if bytes > budget {
            return Err(failure("landing_log_row_too_large"));
        }
        if used + bytes > budget {
            chunks.push(empty());
            used = 0;
        }
        used += bytes;
        if let Some(chunk) = chunks.last_mut() {
            chunk.rows.push(row);
        }
    }
    assert_eq!(
        chunks.iter().map(|chunk| chunk.rows.len()).sum::<usize>(),
        total
    );
    assert!(chunks.iter().all(|chunk| chunk.table == table));
    Ok(chunks)
}

/// Inserts one snapshot chunk's rows into its landing table, making first a table its module
/// makes on first use. Inputs: the connection and the chunk. Output: Ok, or
/// `landing_log_chunk_invalid` for an unknown table, a row that is not that table's cells,
/// or a row the table refuses.
fn restore(db: &Connection, chunk: &Chunk) -> Result<(), EngineError> {
    if KEPT_TABLES.contains(&chunk.table.as_str()) {
        return restore_kept(db, chunk);
    }
    let invalid = || failure("landing_log_chunk_invalid");
    let (table, columns, schema) = TABLES
        .into_iter()
        .find(|(table, _, _)| *table == chunk.table)
        .ok_or_else(invalid)?;
    if chunk.rows.len() > SNAPSHOT_ROWS_MAX {
        return Err(invalid());
    }
    if chunk.rows.is_empty() {
        return Ok(());
    }
    if let Some(schema) = schema {
        db.execute_batch(schema).map_err(|_| invalid())?;
    }
    let width = columns.split(',').count();
    let marks = (1..=width)
        .map(|index| format!("?{index}"))
        .collect::<Vec<_>>();
    let mut statement = db
        .prepare(&format!(
            "INSERT INTO {table} ({columns}) VALUES ({})",
            marks.join(",")
        ))
        .map_err(|_| invalid())?;
    for row in &chunk.rows {
        let values = row
            .as_array()
            .filter(|values| values.len() == width)
            .ok_or_else(invalid)?;
        let cells = values
            .iter()
            .map(|value| match value {
                serde_json::Value::Null => Some(rusqlite::types::Value::Null),
                serde_json::Value::Number(number) => {
                    number.as_i64().map(rusqlite::types::Value::Integer)
                }
                serde_json::Value::String(text) => Some(rusqlite::types::Value::Text(text.clone())),
                _ => None,
            })
            .collect::<Option<Vec<_>>>()
            .ok_or_else(invalid)?;
        statement
            .execute(rusqlite::params_from_iter(cells))
            .map_err(|_| invalid())?;
    }
    assert!((2..=4).contains(&width));
    assert!(chunk.rows.len() <= SNAPSHOT_ROWS_MAX);
    Ok(())
}

/// The landing kinds whose body names a project by the key `project`.
pub(crate) const PROJECT_KINDS: [&str; 7] = [
    CHANGE_SET_NUMBERED,
    BASELINE_APPROVED,
    PUBLISH_PROMPT_SNOOZED,
    PUBLISH_PROMPT_NEVER,
    CHANGE_SET_SAVED,
    RULE_REQUEST_SAVED,
    VERIFICATION_SAVED,
];

/// Tombstones every logged landing event of a project, for Create demo's clear of an earlier
/// id, which deletes its landing rows in the same transaction (QUESTION-1046 A, as L7 ide and
/// #348 admissions); snapshot runs keep theirs until compaction (L1b). Inputs: the demo
/// transaction, the project and the owner fence. Output: how many were tombstoned, or the
/// refusal.
pub(crate) fn forget_project(
    tx: &Transaction<'_>,
    project: &str,
    fence: &crate::state_log::fence::Fence,
) -> Result<usize, EngineError> {
    assert!(!tx.is_autocommit(), "the redaction is part of the clear");
    let redacted = replay::redact_matching(
        tx,
        &[Aggregate::Landing],
        Some(PROJECT_KINDS.as_slice()),
        "project",
        project,
        0,
        fence,
    )?;
    assert!(PROJECT_KINDS.iter().all(|kind| KINDS.contains(kind)));
    Ok(redacted)
}

/// The landing kinds whose body names an agent worktree by the key `worktree`, the ones a
/// Delete everything forgets by worktree (rv-sec3 #415 F1).
pub(crate) const WORKTREE_KINDS: [&str; 8] = [
    AGENT_BRANCH_SET,
    AGENT_BRANCH_FORGOTTEN,
    BASE_SET,
    BASE_FORGOTTEN,
    OWNER_EDIT_KEPT,
    OWNER_EDITS_SETTLED,
    OWNER_EDITS_DROPPED,
    WORKTREE_OWNER_EDITS_FORGOTTEN,
];

/// The landing tables keyed by an agent worktree, whose rows a Delete everything forgets.
const WORKTREE_TABLES: [&str; 3] = [
    "landing_agent_branches",
    "landing_bases",
    "landing_owner_edits",
];

/// The most agent worktrees one Delete everything forgets.
const WORKTREES_MAX: usize = 65_536;

/// The agent worktrees only one project names, in its agent rows (a cleaned-up agent's
/// too), its team worktree rows or its change sets, read before a Delete everything deletes
/// them; a path another project still names is left out, so its rows and events stay.
/// Inputs: the delete's transaction and the project. Output: the paths, or
/// `changes_unavailable` past [`WORKTREES_MAX`].
pub(crate) fn project_worktrees(
    tx: &Transaction<'_>,
    project: &str,
) -> Result<Vec<String>, EngineError> {
    let mut sources = Vec::new();
    for table in ["team_agents", "team_worktrees", "landing_change_sets"] {
        if has(tx, table)? {
            sources.push(table);
        }
    }
    if sources.is_empty() {
        return Ok(Vec::new());
    }
    let ours: Vec<String> = sources
        .iter()
        .map(|table| format!("SELECT worktree FROM {table} WHERE project=?1 AND worktree<>''"))
        .collect();
    let mut sql = ours.join(" UNION ");
    for table in &sources {
        sql.push_str(&format!(
            " EXCEPT SELECT worktree FROM {table} WHERE project<>?1"
        ));
    }
    sql.push_str(&format!(" LIMIT {}", WORKTREES_MAX + 1));
    let unavailable = |_| failure("changes_unavailable");
    let mut statement = tx.prepare(&sql).map_err(unavailable)?;
    let worktrees = statement
        .query_map([project], |row| row.get::<_, String>(0))
        .map_err(unavailable)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(unavailable)?;
    if worktrees.len() > WORKTREES_MAX {
        return Err(failure("changes_unavailable"));
    }
    assert!(sources.len() <= 3);
    Ok(worktrees)
}

/// Forgets a deleted project's agent worktrees (rv-sec3 #415 F1): tombstones each event of
/// [`WORKTREE_KINDS`] naming one, in one pass over the log, and deletes their rows in the
/// same transaction, so a landing rebuild equals the live tables; snapshot runs keep theirs
/// until compaction (L1b). Inputs: the transaction, the worktrees and the owner fence.
/// Output: how many events were tombstoned, or the refusal.
pub(crate) fn forget_worktrees(
    tx: &Transaction<'_>,
    worktrees: &[String],
    fence: &crate::state_log::fence::Fence,
) -> Result<usize, EngineError> {
    assert!(!tx.is_autocommit(), "the redaction is part of the delete");
    assert!(worktrees.len() <= WORKTREES_MAX);
    if worktrees.is_empty() {
        return Ok(0);
    }
    let named: std::collections::BTreeSet<&str> = worktrees.iter().map(String::as_str).collect();
    let mut found = Vec::new();
    replay::replay(tx, Aggregate::Landing, 0, |event| {
        if WORKTREE_KINDS.contains(&event.kind.as_str()) {
            let body: serde_json::Map<String, serde_json::Value> =
                serde_json::from_slice(&event.body).map_err(|_| failure("changes_unavailable"))?;
            let worktree = body.get("worktree").and_then(serde_json::Value::as_str);
            if worktree.is_some_and(|worktree| named.contains(worktree)) {
                found.push(event.seq);
            }
        }
        Ok(())
    })?;
    for seq in &found {
        replay::redact(tx, *seq, fence)?;
    }
    for table in WORKTREE_TABLES {
        if has(tx, table)? {
            let sql = format!("DELETE FROM {table} WHERE worktree=?1");
            for worktree in &named {
                tx.execute(&sql, [worktree])
                    .map_err(|_| failure("changes_unavailable"))?;
            }
        }
    }
    assert!(WORKTREE_KINDS.iter().all(|kind| KINDS.contains(kind)));
    Ok(found.len())
}

/// The most project removals a rebuild reads, as L6's bound.
const REMOVALS_MAX: usize = 65_536;

/// Rebuilds the tables on the landing log from it: empties each one the store has, then
/// replays every landing event. A snapshot run holds every row of them all, so its first
/// chunk empties them again; a chunk opens a run unless it follows another chunk at the
/// previous seq, as L6's rebuild reads runs. A logged project removal forgets its project's
/// rule requests, and a Delete its publish prompt answer, at its place in the log, as its
/// projection does (L6 Q10). Input: the store. Output: the last landing seq read, or the
/// refusal.
pub fn rebuild(db: &Connection) -> Result<u64, EngineError> {
    let mut removals = removals(db)?.into_iter().peekable();
    clear(db)?;
    let mut chunk_seq = 0;
    let last = replay::replay(db, Aggregate::Landing, 0, |event| {
        while let Some((_, removal)) = removals.next_if(|(seq, _)| *seq < event.seq) {
            forget_removed(db, &removal)?;
        }
        if event.kind != SNAPSHOT_CHUNK {
            return apply(db, &Change::parse(event)?);
        }
        if chunk_seq == 0 || event.seq != chunk_seq + 1 {
            clear(db)?;
        }
        chunk_seq = event.seq;
        restore(db, &replay::chunk(event)?)
    })?;
    for (_, removal) in removals {
        forget_removed(db, &removal)?;
    }
    assert!(
        last >= chunk_seq,
        "replay reads past every chunk it applied"
    );
    Ok(last)
}

/// Empties each table on the landing log the store has. Input: the store. Output: Ok, or
/// `changes_unavailable`.
fn clear(db: &Connection) -> Result<(), EngineError> {
    let tables = TABLES.map(|(table, _, _)| table);
    for table in tables.into_iter().chain(KEPT_TABLES) {
        if has(db, table)? {
            db.execute(&format!("DELETE FROM {table}"), [])
                .map_err(|_| failure("changes_unavailable"))?;
        }
    }
    Ok(())
}

/// Whether the store has a table of that name. Inputs: the store and the name. Output: the
/// answer, or `changes_unavailable`.
fn has(db: &Connection, table: &str) -> Result<bool, EngineError> {
    db.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name=?1)",
        [table],
        |row| row.get(0),
    )
    .map_err(|_| failure("changes_unavailable"))
}

/// Forgets what a logged project removal's projection forgets of the landing tables: the
/// project's rule requests for a Remove and a Delete alike, and its publish prompt answer
/// for a Delete. Inputs: the store and the removal. Output: Ok, or the store's refusal.
fn forget_removed(
    db: &Connection,
    removal: &crate::project_setup::removed::event::ProjectRemoved,
) -> Result<(), EngineError> {
    crate::github::proposals::forget_project(db, &removal.project)?;
    if !removal.kept {
        crate::github::forget_prompt(db, &removal.project)?;
    }
    Ok(())
}

/// Each logged project removal, Remove from dashboard or Delete, as its seq and body, in
/// log order. Input: the store. Output: the removals, `landing_log_removals` past
/// [`REMOVALS_MAX`], or `landing_log_event` for a removal body that is not one.
fn removals(
    db: &Connection,
) -> Result<Vec<(u64, crate::project_setup::removed::event::ProjectRemoved)>, EngineError> {
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
        found.push((logged.seq, removal));
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

/// Seeds a test store as [`seed`] does and hands it back, for a fixture that opens its store
/// in one expression. Input: the store. Output: the same store.
#[cfg(test)]
pub(crate) fn seeded(db: Connection) -> Connection {
    let host = seed(&db);
    assert_eq!(host.db_instance.len(), 64);
    db
}
