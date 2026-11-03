use crate::engine::EngineError;
use crate::protocol::ProjectId;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub mod profile;
pub mod runner;
pub mod settings;
pub mod snapshot;

pub use profile::{
    AdmittedCheck, AdmittedProfile, CheckKind, CheckProfile, CheckSpec, HoldoutSpec, NetworkMode,
    ProfileId, ReportFormat, Trigger,
};
pub use settings::{ChecksChoice, ChecksSettings, Scope};

/// A refused checks request: one plain code such as `checks_profile_invalid`, never protected
/// check material.
pub type CheckRefusal = EngineError;

/// Who acts on check profiles and settings, as the engine sets it from the connection that
/// asked, never from the request: the owner, or a team master or sub-agent by its team agent
/// id. Stored as `owner`, `master:<id>` and `agent:<id>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Actor {
    Owner,
    Master(i64),
    Agent(i64),
}

/// Where a stored profile stands in its project: proposed and never approved, the current
/// one, approved once but replaced, or revoked by the owner.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProfileState {
    Proposed,
    Current,
    Approved,
    Revoked,
}

/// One stored profile as the owner reviews it, with who proposed it and when it was last
/// approved or revoked.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredProfile {
    pub id: ProfileId,
    pub project: ProjectId,
    pub profile: CheckProfile,
    pub proposed_by: String,
    pub proposed_ms: i64,
    pub approved_ms: Option<i64>,
    pub revoked_ms: Option<i64>,
    pub state: ProfileState,
}

/// The outcome of a proposal: the profile's identity, and the owner question it raised when
/// a master or agent proposed it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Proposal {
    pub id: ProfileId,
    pub question: Option<u64>,
}

/// Creates the check profile, holdout file and settings tables. Input: the engine
/// connection. Output: Ok, or the storage error.
pub fn initialize(connection: &Connection) -> Result<(), EngineError> {
    let _ = connection;
    todo!()
}

/// Stores a proposed profile by content. A master's or agent's proposal of a profile not yet
/// approved raises one owner question; the owner's own proposal raises none. Inputs: the
/// engine connection, the project, the profile JSON and the actor. Output: the proposal, or
/// `checks_profile_invalid`, `checks_holdout_missing`, `checks_profile_revoked`,
/// `checks_project_missing`, a questions refusal, or `checks_unavailable`.
pub fn propose(
    db: &Connection,
    project: &ProjectId,
    body: &[u8],
    actor: Actor,
) -> Result<Proposal, CheckRefusal> {
    let _ = (db, project, body, actor);
    todo!()
}

/// Approves a stored profile as the project's current one, as a new admission. Approving the
/// current profile again changes nothing. Inputs: the engine connection, the project, the
/// profile id and the actor. Output: the admitted profile, or `checks_owner_only`,
/// `checks_profile_unknown` or `checks_unavailable`.
pub fn approve(
    db: &Connection,
    project: &ProjectId,
    id: &ProfileId,
    actor: Actor,
) -> Result<AdmittedProfile, CheckRefusal> {
    let _ = (db, project, id, actor);
    todo!()
}

/// Revokes a stored profile; a revoked current profile leaves the project with none.
/// Inputs: the engine connection, the project, the profile id and the actor. Output: Ok, or
/// `checks_owner_only`, `checks_profile_unknown` or `checks_unavailable`.
pub fn revoke(
    db: &Connection,
    project: &ProjectId,
    id: &ProfileId,
    actor: Actor,
) -> Result<(), CheckRefusal> {
    let _ = (db, project, id, actor);
    todo!()
}

/// Reads one stored profile. Inputs: the engine connection, the project and the profile id.
/// Output: the profile, none when the project holds no such profile, or `checks_unavailable`.
pub fn show(
    db: &Connection,
    project: &ProjectId,
    id: &ProfileId,
) -> Result<Option<StoredProfile>, CheckRefusal> {
    let _ = (db, project, id);
    todo!()
}

/// The project's current admitted profile. Inputs: the engine connection or a transaction
/// on it, and the project. Output: the profile, none when the owner approved none or revoked
/// the current one, or `checks_unavailable`.
pub fn current_profile(
    db: &Connection,
    project: &ProjectId,
) -> Result<Option<AdmittedProfile>, CheckRefusal> {
    let _ = (db, project);
    todo!()
}

/// Stores one holdout file the owner gives, by content. Inputs: the engine connection, the
/// project, the bytes and the actor. Output: the bytes' SHA-256 hex digest a profile names,
/// or `checks_owner_only`, `checks_holdout_invalid`, `checks_holdouts_full` or
/// `checks_unavailable`.
pub fn store_holdout_file(
    db: &Connection,
    project: &ProjectId,
    bytes: &[u8],
    actor: Actor,
) -> Result<String, CheckRefusal> {
    let _ = (db, project, bytes, actor);
    todo!()
}

/// The bytes of an admitted holdout's files, to copy into its run's snapshot. Inputs: the
/// engine connection, the project and the check. Output: each relative path with its bytes,
/// empty for an ordinary check, or `checks_holdout_missing` or `checks_unavailable`.
pub fn holdout_files(
    db: &Connection,
    project: &ProjectId,
    check: &AdmittedCheck,
) -> Result<BTreeMap<String, Vec<u8>>, CheckRefusal> {
    let _ = (db, project, check);
    todo!()
}

#[cfg(all(test, target_os = "macos"))]
#[path = "checks/tests/runner.rs"]
mod runner_tests;
