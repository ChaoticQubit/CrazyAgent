//! Write approvals: a tool call that may change something at a vendor waits here for the
//! owner's Allow or Deny, as one Action notice, by the write setting and the wait setting.
//! A project's standing allow, under Ask once per project and connector, is kept here too.

use crate::connectors::settings::{Wait, WritePolicy};
use crate::credentials::SecretString;
use crate::engine::EngineError;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// The most bytes an approval's preview of a call holds.
pub const PREVIEW_BYTES_MAX: usize = 480;
/// The most writes of one launch that may wait for the owner at once.
pub const WAITING_PER_LAUNCH_MAX: usize = 4;
/// The most writes that may wait for the owner at once, across every launch.
pub const WAITING_MAX: usize = 64;

/// Where an approval stands: waiting for the owner, or answered by an Allow, a Deny or the
/// end of its wait.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Waiting,
    Allowed,
    Denied,
    Expired,
}

/// The owner's answer to one waiting write.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Answer {
    Allow,
    Deny,
}

/// One write call that asks the owner: the launch, project and agent it came from, the
/// connector by name and title, the vendor tool and the call's preview.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingWrite {
    pub launch: String,
    pub project: String,
    pub agent: String,
    pub connector: String,
    pub title: String,
    pub tool: String,
    pub preview: String,
}

/// One approval as the owner's approval page shows it: what asks, where, the preview,
/// where it stands, and its times in milliseconds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApprovalView {
    pub id: i64,
    pub project: String,
    pub project_name: String,
    pub agent: String,
    pub connector: String,
    pub title: String,
    pub tool: String,
    pub preview: String,
    pub state: State,
    pub created_ms: i64,
    pub expires_ms: i64,
    pub answered_ms: Option<i64>,
}

/// The connectors a project's owner allowed to write without asking again, by name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AllowedWrites {
    pub project: String,
    pub connectors: Vec<String>,
}

/// One waiting approval as its notice names it: its id, the project's id and shown name,
/// the connector's title and the vendor tool.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Waiting {
    pub id: i64,
    pub project: String,
    pub project_name: String,
    pub title: String,
    pub tool: String,
}

fn not_yet() -> EngineError {
    EngineError("connector_approvals_not_yet".into())
}

/// Makes the approval tables. Input: the engine database. Output: Ok, or
/// `connectors_unavailable`.
pub fn initialize(_db: &Connection) -> Result<(), EngineError> {
    Err(not_yet())
}

/// The preview of one write call. Inputs: the vendor tool, the call's arguments and the
/// connector's secret forms. Output: the preview, at most `PREVIEW_BYTES_MAX` bytes.
pub fn preview(_tool: &str, _arguments: &Map<String, Value>, _secrets: &[SecretString]) -> String {
    String::new()
}

/// Records one write that waits for the owner. Inputs: the database, the write, the wait
/// and the time now. Output: the approval's id, or a refusal.
pub fn ask(
    _db: &Connection,
    _write: &PendingWrite,
    _wait: Wait,
    _now_ms: i64,
) -> Result<i64, EngineError> {
    Err(not_yet())
}

/// Answers one waiting write. Inputs: the database, the id, the answer, the write setting
/// and the time now. Output: Ok, or a refusal.
pub fn answer(
    _db: &Connection,
    _id: i64,
    _answer: Answer,
    _write: WritePolicy,
    _now_ms: i64,
) -> Result<(), EngineError> {
    Err(not_yet())
}

/// Where one approval stands. Inputs: the database and the id. Output: its state, or none.
pub fn state(_db: &Connection, _id: i64) -> Result<Option<State>, EngineError> {
    Err(not_yet())
}

/// One approval as the owner's page shows it. Inputs: the database and the id. Output:
/// the approval, or none.
pub fn get(_db: &Connection, _id: i64) -> Result<Option<ApprovalView>, EngineError> {
    Err(not_yet())
}

/// Ends the waits that ran out. Inputs: the database and the time now. Output: how many.
pub fn expire(_db: &Connection, _now_ms: i64) -> Result<usize, EngineError> {
    Err(not_yet())
}

/// Ends the given waits. Inputs: the database, the ids and the time now. Output: how many.
pub fn expire_ids(_db: &Connection, _ids: &[i64], _now_ms: i64) -> Result<usize, EngineError> {
    Err(not_yet())
}

/// Ends every wait. Inputs: the database and the time now. Output: how many.
pub fn expire_waiting(_db: &Connection, _now_ms: i64) -> Result<usize, EngineError> {
    Err(not_yet())
}

/// Whether a project's owner allowed a connector to write without asking again. Inputs:
/// the database, the project and the connector. Output: the answer.
pub fn writes_allowed(
    _db: &Connection,
    _project: &str,
    _connector: &str,
) -> Result<bool, EngineError> {
    Err(not_yet())
}

/// The connectors a project may write with without asking again. Inputs: the database and
/// the project. Output: them, by name.
pub fn allowed_writes(_db: &Connection, _project: &str) -> Result<AllowedWrites, EngineError> {
    Err(not_yet())
}

/// Takes back a project's standing allow for one connector. Inputs: the database, the
/// project and the connector. Output: whether there was one.
pub fn revoke_writes(
    _db: &Connection,
    _project: &str,
    _connector: &str,
) -> Result<bool, EngineError> {
    Err(not_yet())
}

/// The waiting approvals for their notices. Inputs: the database and the most to read.
/// Output: them in id order, or none before any approval was asked.
pub fn waiting(_db: &Connection, _max: usize) -> Result<Option<Vec<Waiting>>, EngineError> {
    Err(not_yet())
}

#[cfg(all(test, unix))]
#[path = "approvals_tests.rs"]
mod tests;
