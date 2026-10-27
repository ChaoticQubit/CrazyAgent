use crate::engine::EngineError;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::time::Duration;

mod store;
#[cfg(test)]
mod tests;

/// The most bytes an action key may hold.
pub const KEY_BYTES_MAX: usize = 512;
/// The most bytes an action payload may hold.
pub const PAYLOAD_BYTES_MAX: usize = 16 * 1024;
/// The most actions that may be pending at once, across every kind.
pub const PENDING_MAX: usize = 512;
/// How many finished actions the ledger keeps, newest first; older ones are pruned.
pub const FINISHED_KEPT: usize = 1024;

/// The engine actions the ledger records before their effect, so a restart can tell
/// whether each one happened. On the owner wire each is its stored snake_case name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionKind {
    Message,
    Dispatch,
    WorktreeCreate,
    AgentSpawn,
    LandingAccept,
}

/// The id the ledger gave one action when its intent was recorded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ActionId(i64);

/// How a recorded action ended. `Failed` means its effect did not happen; `Unknown`
/// closes an action whose effect could not be told, once the owner has been shown it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Done,
    Failed,
    Unknown,
}

/// One recorded action: its id, kind, the key naming what it acts on, the data its
/// check needs and when its intent was recorded, in milliseconds since the epoch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Action {
    pub id: ActionId,
    pub kind: ActionKind,
    pub key: String,
    pub payload: String,
    pub started_ms: i64,
}

/// What a check of real state says about a pending action after a restart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resolution {
    Done,
    NotStarted,
    Unknown,
}

/// Reads real state to tell whether a pending action's effect happened. Input: the
/// action. Output: Done, NotStarted, or Unknown when real state cannot tell.
pub trait Check {
    fn check(&self, action: &Action) -> Resolution;
}

/// The pending actions of one kind after a check. Finished: those whose effect happened,
/// and those that never started, which the caller completes as new actions. Still pending:
/// those whose outcome is unknown, which are shown to the owner and never replayed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    pub done: Vec<Action>,
    pub not_started: Vec<Action>,
    pub unknown: Vec<Action>,
}

impl ActionId {
    /// The number the ledger stores for this action, which the owner names to dismiss it.
    pub fn get(self) -> i64 {
        self.0
    }
}

impl ActionKind {
    fn name(self) -> &'static str {
        match self {
            Self::Message => "message",
            Self::Dispatch => "dispatch",
            Self::WorktreeCreate => "worktree_create",
            Self::AgentSpawn => "agent_spawn",
            Self::LandingAccept => "landing_accept",
        }
    }
}

impl Outcome {
    fn name(self) -> &'static str {
        match self {
            Self::Done => "done",
            Self::Failed => "failed",
            Self::Unknown => "unknown",
        }
    }
}

fn failure(code: &str) -> EngineError {
    EngineError(code.to_owned())
}

fn storage(_: impl std::fmt::Display) -> EngineError {
    failure("ledger_unavailable")
}

/// Opens the action ledger in the engine database, adding its table when missing.
/// Input: the engine root holding `engine.db`. Output: the connection.
pub fn open(runtime: &Path) -> Result<Connection, EngineError> {
    if !runtime.is_absolute() {
        return Err(failure("ledger_unavailable"));
    }
    let db = Connection::open(runtime.join("engine.db")).map_err(storage)?;
    db.busy_timeout(Duration::from_secs(5)).map_err(storage)?;
    initialize(&db)?;
    Ok(db)
}

/// Adds the ledger table to a connection of the engine database when missing, for a
/// component that already holds one. Input: the connection. Output: done, or
/// `ledger_unavailable`.
pub fn initialize(db: &Connection) -> Result<(), EngineError> {
    store::initialize(db)
}

/// Records the intent of an action before its effect runs. Inputs: the ledger, the kind,
/// a key of at most KEY_BYTES_MAX bytes with no control characters naming what it acts
/// on, and a payload of at most PAYLOAD_BYTES_MAX bytes. Output: the new action id, or
/// `ledger_pending` while an action of that kind and key is pending, `ledger_full` at
/// PENDING_MAX pending actions, or `ledger_invalid`.
pub fn begin(
    db: &Connection,
    kind: ActionKind,
    key: &str,
    payload: &str,
) -> Result<ActionId, EngineError> {
    if key.trim().is_empty()
        || key.len() > KEY_BYTES_MAX
        || key.chars().any(char::is_control)
        || payload.len() > PAYLOAD_BYTES_MAX
    {
        return Err(failure("ledger_invalid"));
    }
    store::insert(db, kind, key, payload, crate::team::now_ms())
}

/// Records how a pending action ended and prunes finished actions beyond FINISHED_KEPT.
/// Inputs: the ledger, the action id and its outcome. Output: Ok, or `ledger_finished`
/// when it already ended, or `ledger_missing` when no such action is kept.
pub fn finish(db: &Connection, id: ActionId, outcome: Outcome) -> Result<(), EngineError> {
    store::finish(db, None, id, outcome, crate::team::now_ms())
}

/// What the owner is shown for a pending action that waits for the owner, not a check: a
/// notice whose payload holds `"unknown": true` shows Unknown, and an interrupted Accept
/// listed with `"landed"` shows Done when it landed and Failed when it was dropped.
/// Input: the action. Output: that outcome, none for an intent a check still settles.
pub fn notice(action: &Action) -> Option<Outcome> {
    let payload = serde_json::from_str::<serde_json::Value>(&action.payload).ok()?;
    if payload.get("unknown") == Some(&serde_json::Value::Bool(true)) {
        return Some(Outcome::Unknown);
    }
    match (action.kind, payload.get("landed")?.as_bool()?) {
        (ActionKind::LandingAccept, true) => Some(Outcome::Done),
        (ActionKind::LandingAccept, false) => Some(Outcome::Failed),
        _ => None,
    }
}

/// Closes one notice once the owner dismisses it, with the outcome `notice` says the owner
/// was shown. Inputs: the ledger, and the kind and number the owner names. Output: that
/// outcome, or `ledger_not_notice` for an intent a check still settles, `ledger_finished`
/// when it already ended, or `ledger_missing` when no action of that kind has that number.
pub fn dismiss(db: &Connection, kind: ActionKind, id: i64) -> Result<Outcome, EngineError> {
    let Some(action) = pending(db, kind)?
        .into_iter()
        .find(|action| action.id.0 == id)
    else {
        return Err(store::ended(db, Some(kind), ActionId(id))?);
    };
    let shown = notice(&action).ok_or_else(|| failure("ledger_not_notice"))?;
    store::finish(db, Some(kind), action.id, shown, crate::team::now_ms())?;
    Ok(shown)
}

/// Replaces the payload of a pending action, so its check can keep what it found, such as
/// an outcome no later start can tell either. Inputs: the ledger, the action id and a
/// payload of at most PAYLOAD_BYTES_MAX bytes. Output: Ok, or `ledger_invalid`,
/// `ledger_finished` when it already ended, `ledger_missing` or `ledger_unavailable`.
pub fn note(db: &Connection, id: ActionId, payload: &str) -> Result<(), EngineError> {
    if payload.len() > PAYLOAD_BYTES_MAX {
        return Err(failure("ledger_invalid"));
    }
    store::replace(db, id, payload)
}

/// The pending actions of one kind, oldest first: intents recorded with no outcome, whose
/// effect may or may not have happened. Inputs: the ledger and the kind. Output: at most
/// PENDING_MAX actions.
pub fn pending(db: &Connection, kind: ActionKind) -> Result<Vec<Action>, EngineError> {
    store::pending(db, kind)
}

/// The pending action of one kind and key, if any. Inputs: the ledger, the kind and the
/// key. Output: the action, or none when nothing of that key is pending.
pub fn pending_key(
    db: &Connection,
    kind: ActionKind,
    key: &str,
) -> Result<Option<Action>, EngineError> {
    store::pending_key(db, kind, key)
}

/// Checks every pending action of one kind against real state: a Done action is finished
/// as done, a NotStarted one as failed, since its effect never happened and the caller
/// completes it as a new action, and an Unknown one stays pending, so each start shows it
/// to the owner again until `finish(Outcome::Unknown)` closes it; it is never returned for
/// replay. Call it inside the transaction that acts on the lists, so a crash cannot finish
/// an action and lose what the caller must do with it. Inputs: the ledger, the kind and
/// the check. Output: the checked actions by resolution.
pub fn resolve(
    db: &Connection,
    kind: ActionKind,
    check: &impl Check,
) -> Result<Resolved, EngineError> {
    let mut resolved = Resolved {
        done: Vec::new(),
        not_started: Vec::new(),
        unknown: Vec::new(),
    };
    for action in pending(db, kind)? {
        let (outcome, list) = match check.check(&action) {
            Resolution::Done => (Outcome::Done, &mut resolved.done),
            Resolution::NotStarted => (Outcome::Failed, &mut resolved.not_started),
            Resolution::Unknown => {
                resolved.unknown.push(action);
                continue;
            }
        };
        finish(db, action.id, outcome)?;
        list.push(action);
    }
    Ok(resolved)
}
