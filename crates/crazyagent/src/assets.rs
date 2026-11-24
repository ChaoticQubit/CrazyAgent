use crate::config::EffectiveConfig;
use crate::engine::EngineError;
use crate::memory::{error, principal, reject};
use crate::protocol::*;
use kernel_plugin_host::assets as kernel;
use rusqlite::{Connection, Transaction};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AssetScope {
    Global,
    Project(ProjectId),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AssetKind {
    Rules,
    Skill,
    ConfigurationText,
    NativePlugin,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssetSource {
    pub id: RequestId,
    pub name: String,
    pub scope: AssetScope,
    pub kind: AssetKind,
    pub path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssetContent {
    pub source: AssetSource,
    pub content_hash: String,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssetSnapshot {
    pub id: SnapshotId,
    pub project_id: ProjectId,
    pub config_id: ConfigId,
    pub content_hash: String,
    pub assets: Vec<AssetContent>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "operation", deny_unknown_fields)]
pub enum AssetAction {
    Admit {
        source: AssetSource,
    },
    Revoke {
        source_id: RequestId,
    },
    Snapshot {
        principal_id: RequestId,
        snapshot_id: SnapshotId,
    },
    Read {
        principal_id: RequestId,
        snapshot_id: SnapshotId,
    },
}

pub(crate) fn initialize(connection: &Connection) -> Result<(), EngineError> {
    kernel::initialize(connection).map_err(error)
}

pub(crate) fn is_mutation(action: &AssetAction) -> bool {
    !matches!(action, AssetAction::Read { .. })
}

/// Runs one asset action in a savepoint, so an action that does not apply leaves no row
/// (v2.5 L8 E5). Inputs: the transaction, the config and the action. Output: the action's
/// outcome, or the store's refusal.
pub(crate) fn execute(
    tx: &Transaction<'_>,
    config: &EffectiveConfig,
    action: &AssetAction,
) -> Result<Outcome, EngineError> {
    crate::memory::log::atomic(tx, || act(tx, config, action))
}

/// Resolves the engine facts an asset action needs and runs it in the plugin host: the
/// project's existence, the principal, its policy config and task config, and the limits.
/// Inputs: the transaction, the config and the action. Output: the action's outcome.
fn act(
    tx: &Transaction<'_>,
    config: &EffectiveConfig,
    action: &AssetAction,
) -> Result<Outcome, EngineError> {
    match action {
        AssetAction::Admit { source } => {
            let project_exists = match &source.scope {
                AssetScope::Project(project) => tx
                    .query_row(
                        "SELECT EXISTS(SELECT 1 FROM projects WHERE id=?1)",
                        [project.as_str()],
                        |r| r.get(0),
                    )
                    .map_err(error)?,
                AssetScope::Global => true,
            };
            let facts = kernel::AdmitFacts {
                project_exists,
                asset_bytes_max: config.settings.knowledge.asset_bytes_max,
                asset_sources_max: config.settings.knowledge.asset_sources_max,
            };
            applied(kernel::admit(tx, &facts, &reshape(source)?))
        }
        AssetAction::Revoke { source_id } => applied(kernel::revoke(tx, source_id.as_str())),
        AssetAction::Snapshot {
            principal_id,
            snapshot_id,
        } => {
            let p = match principal(tx, principal_id) {
                Ok(p) => p,
                Err(e) => return reject(&e.0),
            };
            let policy = crate::memory::policy(tx, &p, config)?;
            let facts = kernel::SnapshotFacts {
                project_id: p.project_id.as_str(),
                config_id: policy.id.as_str(),
                asset_bytes_max: policy.settings.knowledge.asset_bytes_max,
                records_max: policy.settings.knowledge.records_max,
                frame_bytes_max: FRAME_BYTES_MAX,
                same_files: crate::file_identity::same_files,
            };
            applied(kernel::snapshot(tx, &facts, snapshot_id.as_str()))
        }
        AssetAction::Read {
            principal_id,
            snapshot_id,
        } => {
            let p = match principal(tx, principal_id) {
                Ok(p) => p,
                Err(e) => return reject(&e.0),
            };
            let context = crate::storage::task_context(tx, &p.task_id)?
                .ok_or_else(|| error("task_missing"))?;
            let facts = kernel::ReadFacts {
                project_id: p.project_id.as_str(),
                task_config_id: context.config_id.as_str(),
            };
            match kernel::read(tx, &facts, snapshot_id.as_str()) {
                Ok(snapshot) => Ok(Outcome::Assets {
                    snapshot: Box::new(reshape(&snapshot)?),
                }),
                Err(failure) => refused(failure),
            }
        }
    }
}

/// Converts between a wire asset type and its plugin host twin through their one serde
/// shape, so wire ids are validated as when a stored body is read. Input: the value.
/// Output: the twin, or the decoding error.
fn reshape<T: Serialize, U: DeserializeOwned>(value: &T) -> Result<U, EngineError> {
    serde_json::from_value(serde_json::to_value(value).map_err(error)?).map_err(error)
}

/// The outcome of a plugin host action that returns nothing. Input: its result. Output:
/// `Applied`, the refusal, or the store's failure.
fn applied(result: Result<(), kernel::Failure>) -> Result<Outcome, EngineError> {
    match result {
        Ok(()) => Ok(Outcome::Applied),
        Err(failure) => refused(failure),
    }
}

/// A plugin host failure as the engine reports it: a refusal as `Rejected`, a store
/// fault as an error. Input: the failure. Output: the outcome or the error.
fn refused(failure: kernel::Failure) -> Result<Outcome, EngineError> {
    match failure {
        kernel::Failure::Refused(code) => reject(&code),
        kernel::Failure::Failed(text) => Err(error(text)),
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests;
