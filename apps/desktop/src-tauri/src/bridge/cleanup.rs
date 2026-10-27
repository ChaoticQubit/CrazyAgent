use super::*;
use crazyagent::engine::EngineError;
use crazyagent::engine::owner::{CLEANUP_CODES, OwnerClient};
use crazyagent::protocol::ProjectId;
use crazyagent::team::cleanup::FinishedAgent;
use std::sync::Arc;

fn cleanup_error(error: EngineError) -> BridgeError {
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

    /// Removes the worktrees and folders of the finished agents the owner confirmed, run
    /// without the bridge lock; the engine checks each one again first. Inputs: the
    /// project id and the agents. Output: the agents removed, or the code that kept them.
    pub fn remove_finished_agents(
        &self,
        project_id: String,
        agents: Vec<i64>,
    ) -> Result<Unlocked<Vec<i64>>, BridgeError> {
        let project = project(project_id)?;
        let client = self.cleanup_client()?;
        Ok(Box::pin(async move {
            let removal = client
                .remove_finished_agents(project, agents)
                .await
                .map_err(cleanup_error)?;
            match removal.kept {
                None => Ok(removal.agents),
                Some(code) => Err(cleanup_error(EngineError(code))),
            }
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
