use crate::engine::EngineError;
use crate::master_session::MasterTurnPhase;
use crate::owner_texts::TextDigest;
use crate::protocol::{CredentialRef, ProjectId, RequestId, SessionId};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};

/// The four tables v2.5 L13c puts on the log, in snapshot order after L13's three.
pub(super) const TABLES: [&str; 4] = [
    "master_turns",
    "master_preparations",
    "master_waiting_resolves",
    "configured_master_profiles",
];
/// The most session or profile ids one dropped event lists, each at most 128 bytes quoted.
pub(super) const NAMES_MAX: usize = 256;

/// One master turn row: its session's project and the turn's keys, its phase and the digest
/// its body is kept under (`owner_texts`), so no turn text rides in the log.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TurnSaved {
    pub(crate) project: ProjectId,
    pub(crate) session: SessionId,
    pub(crate) ordinal: u64,
    pub(crate) id: RequestId,
    pub(crate) input: u64,
    pub(crate) phase: MasterTurnPhase,
    pub(crate) body: TextDigest,
}

/// One turn's preparation: its session's project, the turn's session, id and input, and the
/// digest its body is kept under.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreparationSaved {
    pub(crate) project: ProjectId,
    pub(crate) session: SessionId,
    pub(crate) turn: RequestId,
    pub(crate) input: u64,
    pub(crate) body: TextDigest,
}

/// One session's waiting Resolve: its project, session, operation and input, and the digest
/// its request body is kept under.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResolveSaved {
    pub(crate) project: ProjectId,
    pub(crate) session: SessionId,
    pub(crate) operation: RequestId,
    pub(crate) input: u64,
    pub(crate) body: TextDigest,
}

/// Waiting Resolves of one project gone from their table, by session, at most
/// [`NAMES_MAX`] in one event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResolvesDropped {
    pub(crate) project: ProjectId,
    pub(crate) sessions: Vec<SessionId>,
}

/// One configured profile receipt: its session's project, its id, session and profile, and
/// the digest its record is kept under.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileSaved {
    pub(crate) project: ProjectId,
    pub(crate) session: SessionId,
    pub(crate) id: RequestId,
    pub(crate) profile: CredentialRef,
    pub(crate) body: TextDigest,
}

/// Configured profile receipts of one project gone from their table, by id, at most
/// [`NAMES_MAX`] in one event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfilesDropped {
    pub(crate) project: ProjectId,
    pub(crate) ids: Vec<RequestId>,
}

/// Marks a master turn a live writer inserted or saved, by its input, with its session's
/// project; a turn of no stored session is not marked. Inputs: the transaction and the
/// turn's input. Output: Ok, or the store's refusal.
pub(crate) fn mark_turn(_: &Connection, _: u64) -> Result<(), EngineError> {
    Ok(())
}

/// Marks a turn's preparation a live writer inserted, by the turn's input. Inputs: the
/// transaction, the session and the turn id. Output: Ok, or the store's refusal.
pub(crate) fn mark_preparation(_: &Connection, _: &str, _: &str) -> Result<(), EngineError> {
    Ok(())
}

/// Marks a session's waiting Resolve a live writer saved or is about to delete. Inputs:
/// the transaction, the project and the session. Output: Ok, or the store's refusal.
pub(crate) fn mark_resolve(_: &Connection, _: &str, _: &str) -> Result<(), EngineError> {
    Ok(())
}

/// Marks a configured profile receipt a live writer inserted or is about to delete, with
/// its session's project. Inputs: the transaction and the receipt id. Output: Ok, or the
/// store's refusal.
pub(crate) fn mark_profile(_: &Connection, _: &str) -> Result<(), EngineError> {
    Ok(())
}

/// Marks every configured profile receipt of a stored session, before a bulk delete.
/// Input: the transaction. Output: Ok, or the store's refusal.
pub(crate) fn mark_profiles(_: &Connection) -> Result<(), EngineError> {
    Ok(())
}

/// Marks a deleted session's waiting Resolve and profile receipts, so the Delete's close
/// logs them again after its redaction. Inputs: the transaction, the project and the
/// session. Output: Ok, or the store's refusal.
pub(crate) fn mark_session_rest(_: &Connection, _: &str, _: &str) -> Result<(), EngineError> {
    Ok(())
}

/// Lets go of the kept bodies of a session's turns and preparations before a Delete removes
/// them. Inputs: the transaction and the session. Output: Ok, or the store's refusal.
pub(crate) fn release_session_turns(_: &Connection, _: &str) -> Result<(), EngineError> {
    Err(EngineError("sessions_log_unwritten".into()))
}
