use super::*;
use crazyagent::engine::EngineError;
use crazyagent::engine::owner::{AgentInstructions, OwnerClient, PROCESS_CODES};
use crazyagent::protocol::ProjectId;
use std::sync::Arc;

fn process_error(error: EngineError) -> BridgeError {
    if error.0 == "controller_disconnected" {
        return failure("not_connected");
    }
    caused(
        PROCESS_CODES
            .iter()
            .copied()
            .find(|known| *known == error.0)
            .unwrap_or("process_outcome_unknown"),
        &error.0,
    )
}

fn project(project_id: Option<String>) -> Result<Option<ProjectId>, BridgeError> {
    project_id
        .map(|id| ProjectId::new(id).map_err(|_| failure("process_project_unknown")))
        .transpose()
}

impl NativeBridge {
    fn process_client(&self) -> Result<Arc<OwnerClient>, BridgeError> {
        if !self.status().connected {
            return Err(failure("not_connected"));
        }
        self.controller
            .as_ref()
            .ok_or_else(|| failure("not_connected"))?
            .shared_client()
            .map_err(process_error)
    }

    /// Reads the owner's instructions, run without the bridge lock. Input: none for the
    /// app-wide text, or the project id. Output: the text, empty when none is saved.
    pub fn process_text(
        &self,
        project_id: Option<String>,
    ) -> Result<Unlocked<String>, BridgeError> {
        let project = project(project_id)?;
        let client = self.process_client()?;
        Ok(Box::pin(async move {
            client.process_text(project).await.map_err(process_error)
        }))
    }

    /// Saves the owner's instructions whole, run without the bridge lock. Inputs: none for
    /// the app-wide text, or the project id, and the text. Output: unit once stored.
    pub fn save_process_text(
        &self,
        project_id: Option<String>,
        text: String,
    ) -> Result<Unlocked<()>, BridgeError> {
        let project = project(project_id)?;
        let client = self.process_client()?;
        Ok(Box::pin(async move {
            client
                .save_process_text(project, text)
                .await
                .map_err(process_error)
        }))
    }

    /// Reads the owner's `~/.claude/CLAUDE.md` for a preview, run without the bridge lock.
    /// Only the owner's Import click calls this. Output: the text.
    pub fn import_global_instructions(&self) -> Result<Unlocked<String>, BridgeError> {
        let client = self.process_client()?;
        Ok(Box::pin(async move {
            client
                .import_global_instructions()
                .await
                .map_err(process_error)
        }))
    }

    /// Reads the CrazyAgent default process for a preview, run without the bridge lock.
    pub fn default_process(&self) -> Result<Unlocked<String>, BridgeError> {
        let client = self.process_client()?;
        Ok(Box::pin(async move {
            client.default_process().await.map_err(process_error)
        }))
    }

    /// Reads what one team agent received at its last launch, run without the bridge lock.
    /// Inputs: the project id and the agent id, or none for the project's newest master.
    /// Output: the read-only view.
    pub fn agent_instructions(
        &self,
        project_id: String,
        agent: Option<i64>,
    ) -> Result<Unlocked<AgentInstructions>, BridgeError> {
        let project =
            project(Some(project_id))?.ok_or_else(|| failure("process_project_unknown"))?;
        let client = self.process_client()?;
        Ok(Box::pin(async move {
            client
                .agent_instructions(project, agent)
                .await
                .map_err(process_error)
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every Process code the engine answers reaches the window as itself, so the owner
    /// reads its sentence; anything else is an unknown outcome with the engine code behind.
    #[test]
    fn engine_process_codes_pass_through_unchanged() {
        for code in PROCESS_CODES {
            assert_eq!(process_error(EngineError((*code).into())).code, *code);
        }
        assert_eq!(
            process_error(EngineError("controller_disconnected".into())).code,
            "not_connected"
        );
        let unlisted = process_error(EngineError("process_state_torn".into()));
        assert_eq!(unlisted.code, "process_outcome_unknown");
        assert_eq!(unlisted.cause.as_deref(), Some("process_state_torn"));
        let raw = process_error(EngineError("open /Users/mira/notes: denied".into()));
        assert_eq!(raw.code, "process_outcome_unknown");
        assert_eq!(
            raw.cause.as_deref(),
            Some(crazyagent::protocol::TEXT_WITHHELD)
        );
        assert_eq!(
            project(Some("not an id".into())).unwrap_err().code,
            "process_project_unknown"
        );
        assert_eq!(project(None).unwrap(), None);
    }
}
