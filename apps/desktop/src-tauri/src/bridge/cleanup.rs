use super::*;
use crazyagent::engine::EngineError;
use crazyagent::engine::owner::{CLEANUP_CODES, OwnerClient};
use crazyagent::protocol::ProjectId;
use crazyagent::team::cleanup::{FinishedAgent, Removal, RemovalPreview};
use std::sync::Arc;

pub(super) fn cleanup_error(error: EngineError) -> BridgeError {
    if error.0 == "controller_disconnected" {
        return failure("not_connected");
    }
    caused(
        CLEANUP_CODES
            .iter()
            .copied()
            .find(|known| *known == error.0)
            .unwrap_or("cleanup_outcome_unknown"),
        &error.0,
    )
}

/// The removal the window gets: the agents removed, the listed code of the one kept, as
/// [`cleanup_error`] passes codes, and the folder too deep to remove. Input: the engine's
/// removal. Output: the window's.
fn window_removal(removal: Removal) -> Removal {
    Removal {
        kept: removal
            .kept
            .map(|code| cleanup_error(EngineError(code)).code.to_owned()),
        agents: removal.agents,
        folder: removal.folder,
    }
}

fn project(id: String) -> Result<ProjectId, BridgeError> {
    ProjectId::new(id).map_err(|_| failure("cleanup_request_invalid"))
}

impl NativeBridge {
    fn cleanup_client(&self) -> Result<Arc<OwnerClient>, BridgeError> {
        if !self.status().connected {
            return Err(failure("not_connected"));
        }
        self.controller
            .as_ref()
            .ok_or_else(|| failure("not_connected"))?
            .shared_client()
            .map_err(cleanup_error)
    }

    /// Reads a project's finished agents whose worktree and folder can be removed, newest
    /// first with their size on disk, run without the bridge lock. Input: the project id.
    /// Output: the agents; `cleanup_request_invalid` for a project id that is not one.
    pub fn finished_agents(
        &self,
        project_id: String,
    ) -> Result<Unlocked<Vec<FinishedAgent>>, BridgeError> {
        let project = project(project_id)?;
        let client = self.cleanup_client()?;
        Ok(Box::pin(async move {
            client.finished_agents(project).await.map_err(cleanup_error)
        }))
    }

    /// Reads what removing a project would do, run without the bridge lock: its name, the
    /// folders Delete everything would move to the Trash, whether it may, and what would be
    /// lost. Input: the project id. Output: the engine's preview; nothing changes.
    pub fn removal_preview(
        &self,
        project_id: String,
    ) -> Result<Unlocked<RemovalPreview>, BridgeError> {
        let project = project(project_id)?;
        let client = self.cleanup_client()?;
        Ok(Box::pin(async move {
            client.removal_preview(project).await.map_err(cleanup_error)
        }))
    }

    /// Removes the worktrees and folders of the finished agents the owner confirmed, run
    /// without the bridge lock; the engine checks each one again first. Inputs: the
    /// project id and the agents. Output: the agents removed with the listed code of the one
    /// kept when the removal stopped part way, or the code that kept them all.
    pub fn remove_finished_agents(
        &self,
        project_id: String,
        agents: Vec<i64>,
    ) -> Result<Unlocked<Removal>, BridgeError> {
        let project = project(project_id)?;
        let client = self.cleanup_client()?;
        Ok(Box::pin(async move {
            let removal = client
                .remove_finished_agents(project, agents)
                .await
                .map_err(cleanup_error)?;
            Ok(window_removal(removal))
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A removal kept for a folder too deep to remove reaches the window with that folder,
    /// also when it removed no agent; a removal kept for any other code carries none.
    #[test]
    fn a_too_deep_removal_reaches_the_window_with_its_folder() {
        for agents in [Vec::new(), vec![3]] {
            let deep = Removal {
                agents,
                kept: Some("cleanup_folder_too_deep".into()),
                folder: Some("/runtime/tool-cache/ab12/agent-1".into()),
            };
            assert_eq!(window_removal(deep.clone()), deep);
        }
        let dirty = Removal {
            agents: vec![3],
            kept: Some("cleanup_worktree_dirty".into()),
            folder: None,
        };
        assert_eq!(window_removal(dirty.clone()), dirty);
    }

    /// Every cleanup code the engine answers reaches the window as itself; anything else
    /// is an unknown outcome with the engine code behind it and raw engine text withheld.
    #[test]
    fn engine_cleanup_codes_pass_through_unchanged() {
        for code in CLEANUP_CODES {
            assert_eq!(cleanup_error(EngineError((*code).into())).code, *code);
        }
        assert_eq!(
            cleanup_error(EngineError("controller_disconnected".into())).code,
            "not_connected"
        );
        let unlisted = cleanup_error(EngineError("cleanup_state_torn".into()));
        assert_eq!(unlisted.code, "cleanup_outcome_unknown");
        assert_eq!(unlisted.cause.as_deref(), Some("cleanup_state_torn"));
        let raw = cleanup_error(EngineError("remove /Users/mira/worktree: denied".into()));
        assert_eq!(raw.code, "cleanup_outcome_unknown");
        assert_eq!(
            raw.cause.as_deref(),
            Some(crazyagent::protocol::TEXT_WITHHELD)
        );
        assert_eq!(
            project("../secret".into()).unwrap_err().code,
            "cleanup_request_invalid"
        );
    }
}
