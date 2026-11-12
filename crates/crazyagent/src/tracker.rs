use crate::engine::EngineError;
use crate::protocol::ProjectId;
use rusqlite::{OptionalExtension, Transaction, params};
use trackwright_core::model::{Actor, ProjectKey, Refusal};
use trackwright_core::store::{self, StoreError};

pub mod context_import;
pub mod hooks;
pub mod host;
pub(crate) mod render;
pub mod settings;
#[cfg(all(test, unix))]
mod tests;

pub use host::Host;

#[cfg(unix)]
const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS tracker_projects (
    project TEXT PRIMARY KEY NOT NULL,
    key TEXT NOT NULL UNIQUE
);
CREATE TABLE IF NOT EXISTS tracker_settings (
    scope TEXT PRIMARY KEY NOT NULL,
    body TEXT NOT NULL CHECK (length(CAST(body AS BLOB)) BETWEEN 2 AND 1024)
);
CREATE TABLE IF NOT EXISTS tracker_context_imports (
    project TEXT PRIMARY KEY NOT NULL,
    imported_ms INTEGER NOT NULL
);";

const PROJECT_OF_KEY: &str = "SELECT project FROM tracker_projects WHERE key = ?1";
const KEY_OF_PROJECT: &str = "SELECT key FROM tracker_projects WHERE project = ?1";

/// The code a store call fails with when the tracker's tables cannot be read or written.
pub const TRACKER_UNAVAILABLE: &str = "tracker_unavailable";

fn unavailable(_: impl std::fmt::Display) -> EngineError {
    EngineError(TRACKER_UNAVAILABLE.into())
}

/// Turns a core store failure into the engine's error: a refusal keeps its code, anything
/// else is `tracker_unavailable`. Input: the failure. Output: the engine error.
pub(crate) fn engine_error(error: StoreError) -> EngineError {
    match error {
        StoreError::Refused(refusal) => EngineError(refusal.code.into()),
        StoreError::Sqlite(_) | StoreError::Corrupt(_) => unavailable(""),
    }
}

/// Migrates trackwright's tables and the engine's tracker tables into `engine.db`, then
/// writes every attached project's effective policy into the core, so a project attached
/// before a host lock (`run_writers`) takes it; all in one transaction, forward only and
/// idempotent. Input: the engine database, not inside a transaction. Output: Ok once all
/// are current, or `tracker_unavailable`, or the core's `store_schema_newer` for a
/// database a newer tracker wrote.
#[cfg(unix)]
pub(crate) fn initialize(connection: &rusqlite::Connection) -> Result<(), EngineError> {
    assert!(connection.is_autocommit());
    let tx = connection.unchecked_transaction().map_err(unavailable)?;
    trackwright_core::schema::migrate(&tx).map_err(engine_error)?;
    tx.execute_batch(SCHEMA).map_err(unavailable)?;
    settings::sync_attached(&tx, crate::team::now_ms())?;
    tx.commit().map_err(unavailable)
}

/// The engine identity the owner's own writes run as, in the tracker as in the questions
/// and change sets. Output: the actor `owner`.
pub(crate) fn owner() -> Actor {
    Actor::new("owner").expect("owner is a valid actor")
}

/// Gives an engine project its tracker project: creates the core project as the owner,
/// maps the engine project to its key and writes the project's effective ticket settings
/// into the core. Inputs: the transaction, the engine project, the key the owner chose,
/// the project name and the time. Output: Ok, `project_exists` for a key or project
/// already attached, or the core's refusal of the key or name.
pub fn attach(
    tx: &Transaction<'_>,
    project: &ProjectId,
    key: &ProjectKey,
    name: &str,
    now_ms: i64,
) -> Result<(), EngineError> {
    if key_of(tx, project)?.is_some() {
        return Err(EngineError(
            trackwright_core::model::codes::PROJECT_EXISTS.into(),
        ));
    }
    store::create_project(tx, &owner(), key, name, now_ms).map_err(engine_error)?;
    let added = tx
        .execute(
            "INSERT INTO tracker_projects(project, key) VALUES (?1, ?2)",
            params![project.as_str(), key.as_str()],
        )
        .map_err(unavailable)?;
    assert_eq!(added, 1);
    settings::sync(tx, project, now_ms)?;
    assert_eq!(key_of(tx, project)?.as_ref(), Some(key));
    Ok(())
}

/// Finds the tracker key of an engine project. Inputs: the transaction and the project.
/// Output: the key, or None for a project not attached.
pub fn key_of(
    tx: &Transaction<'_>,
    project: &ProjectId,
) -> Result<Option<ProjectKey>, EngineError> {
    let key: Option<String> = tx
        .query_row(KEY_OF_PROJECT, [project.as_str()], |row| row.get(0))
        .optional()
        .map_err(unavailable)?;
    key.map(|key| ProjectKey::new(&key).map_err(|refusal| EngineError(refusal.code.into())))
        .transpose()
}

/// Finds the engine project behind a tracker key, for the host's checks. Inputs: the
/// transaction and the key. Output: the project, or the refusal `project_not_attached`
/// (or `host_failed`) that blocks the operation.
pub(crate) fn project_of(tx: &Transaction<'_>, key: &ProjectKey) -> Result<ProjectId, Refusal> {
    let project: Option<String> = tx
        .query_row(PROJECT_OF_KEY, [key.as_str()], |row| row.get(0))
        .optional()
        .map_err(|_| Refusal::host(host::HOST_FAILED, "the engine could not read its projects"))?;
    let project = project.ok_or_else(|| {
        Refusal::host(
            host::PROJECT_NOT_ATTACHED,
            format!("{} is not a CrazyAgent project", key.as_str()),
        )
    })?;
    ProjectId::new(project)
        .map_err(|_| Refusal::host(host::HOST_FAILED, "the engine stores a bad project id"))
}
