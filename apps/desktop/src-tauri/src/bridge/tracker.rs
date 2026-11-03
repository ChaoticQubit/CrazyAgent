use super::*;
use crazyagent::engine::EngineError;
use crazyagent::engine::owner::{IDE_CODES, OwnerClient, TRACKER_CODES, TrackedProject};
use crazyagent::ide::bridge::reveal::RevealTarget;
use crazyagent::landing::ChangeSetId;
use crazyagent::protocol::ProjectId;
use std::sync::Arc;
use trackwright_core::model::{OpKind, ProjectKey};

fn known(codes: &[&'static str], fallback: &'static str, error: EngineError) -> BridgeError {
    if error.0 == "controller_disconnected" {
        return failure("not_connected");
    }
    caused(
        codes
            .iter()
            .copied()
            .find(|code| *code == error.0)
            .unwrap_or(fallback),
        &error.0,
    )
}

/// The bridge failure for an engine tracker error: a code `TRACKER_CODES` lists reaches the
/// window as itself, anything else is an unknown outcome with the engine code behind it.
/// Input: the engine error. Output: the bridge failure.
pub fn tracker_error(error: EngineError) -> BridgeError {
    known(TRACKER_CODES, "tracker_outcome_unknown", error)
}

/// The bridge failure for an Open in IDE error: a code `IDE_CODES` lists reaches the window
/// as itself, anything else is an unknown outcome with the engine code behind it. Input:
/// the engine error. Output: the bridge failure.
pub fn reveal_error(error: EngineError) -> BridgeError {
    known(IDE_CODES, "ide_outcome_unknown", error)
}

/// Reads the window's tracker operation: one of the names schema v1 lists, with its input
/// as one JSON object passed on unchanged for the engine to check. Inputs: the name and
/// the input. Output: the operation and the input, or `unknown_op` or `args_invalid`.
pub fn operation(
    op: &str,
    args: serde_json::Value,
) -> Result<(OpKind, serde_json::Value), BridgeError> {
    let kind = OpKind::parse(op).ok_or_else(|| failure("unknown_op"))?;
    if !args.is_object() {
        return Err(failure("args_invalid"));
    }
    assert_eq!(kind.as_str(), op);
    Ok((kind, args))
}

/// Tells whether the window's operation is one of schema v1's writes, so the command takes
/// the bridge's write lock. Input: the name. Output: false for a read or an unknown name.
pub fn writes(op: &str) -> bool {
    OpKind::parse(op).is_some_and(OpKind::is_write)
}

fn project(id: String, missing: &'static str) -> Result<ProjectId, BridgeError> {
    ProjectId::new(id).map_err(|_| failure(missing))
}

impl NativeBridge {
    fn tracker_client(&self) -> Result<Arc<OwnerClient>, BridgeError> {
        if !self.status().connected {
            return Err(failure("not_connected"));
        }
        self.controller
            .as_ref()
            .ok_or_else(|| failure("not_connected"))?
            .shared_client()
            .map_err(tracker_error)
    }

    /// Runs one schema v1 tracker operation as the owner, run without the bridge lock.
    /// Inputs: the operation's name and its input object. Output: the core's reply
    /// envelope; a refused operation is an envelope with `ok` false.
    pub fn tracker_op(
        &self,
        op: String,
        args: serde_json::Value,
    ) -> Result<Unlocked<serde_json::Value>, BridgeError> {
        let (kind, args) = operation(&op, args)?;
        let client = self.tracker_client()?;
        Ok(Box::pin(async move {
            client.tracker_op(kind, args).await.map_err(tracker_error)
        }))
    }

    /// Lists the projects that have tickets, run without the bridge lock. Output: each
    /// engine project with its tracker key and name.
    pub fn tracker_projects(&self) -> Result<Unlocked<Vec<TrackedProject>>, BridgeError> {
        let client = self.tracker_client()?;
        Ok(Box::pin(async move {
            client.tracker_projects().await.map_err(tracker_error)
        }))
    }

    /// Gives a project its tickets under the key the owner chose, run without the bridge
    /// lock. Inputs: the project id, the key and the name. Output: the tracked project, or
    /// `project_key_invalid` before the engine is asked.
    pub fn tracker_attach(
        &self,
        project_id: String,
        key: String,
        name: String,
    ) -> Result<Unlocked<TrackedProject>, BridgeError> {
        let project = project(project_id, "tracker_project_missing")?;
        let key = ProjectKey::new(&key).map_err(|_| failure("project_key_invalid"))?;
        let client = self.tracker_client()?;
        Ok(Box::pin(async move {
            client
                .tracker_attach(project, key, name)
                .await
                .map_err(tracker_error)
        }))
    }

    /// Shows a ticket's linked change set in the project's open desktop IDE, run without
    /// the bridge lock. Inputs: the project id and the change set id. Output: Ok once the
    /// IDE showed it; `changes_not_found` for a malformed id before the engine is asked,
    /// `ide_not_running` when no IDE is open for the project.
    pub fn tracker_open_change_set(
        &self,
        project_id: String,
        change_set: String,
    ) -> Result<Unlocked<()>, BridgeError> {
        let id = ChangeSetId::new(change_set).map_err(|_| failure("changes_not_found"))?;
        let project = project(project_id, "ide_project_unknown")?;
        let client = self.tracker_client()?;
        Ok(Box::pin(async move {
            client
                .ide_reveal(project, RevealTarget::ChangeSet { id, path: None })
                .await
                .map(|_| ())
                .map_err(reveal_error)
        }))
    }
}
