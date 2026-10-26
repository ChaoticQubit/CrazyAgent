use super::*;
use crazyagent::agents::AgentView;
use crazyagent::engine::EngineError;
use crazyagent::engine::owner::OwnerClient;
use crazyagent::protocol::ProjectId;
use std::sync::Arc;

const AGENT_CODES: &[&str] = &[
    "not_connected",
    "owner_grant_stale_or_revoked",
    "owner_socket_replaced",
    "agent_not_found",
    "agent_not_running",
    "agent_busy",
    "agent_not_busy",
    "agent_reconnected_read_only",
    "agent_restart_needs_key",
    "agent_restart_conflict",
    "agent_restart_limit",
    "agent_registry_unavailable",
    "agent_root_unsafe",
    "agent_state_unavailable",
    "agent_thread_unavailable",
    "agent_spec_invalid",
    "agent_account_invalid",
    "agent_launch_failed",
    "agents_request_bounds",
    "agents_request_invalid",
    "agents_reply_bounds",
    "agents_owner_unavailable",
    "agents_unavailable",
];

fn agent_error(error: EngineError) -> BridgeError {
    let code = match error.0.as_str() {
        "controller_disconnected" => "not_connected",
        launch if launch.starts_with("launch_") => "agent_launch_failed",
        code => code,
    };
    failure(
        AGENT_CODES
            .iter()
            .copied()
            .find(|known| *known == code)
            .unwrap_or("agents_outcome_unknown"),
    )
}

impl NativeBridge {
    fn agent_controls(&self) -> Result<&OwnerController, BridgeError> {
        if !self.status().connected {
            return Err(failure("not_connected"));
        }
        self.controller
            .as_ref()
            .ok_or_else(|| failure("not_connected"))
    }

    /// Lists the agents the engine launched, newest first, for one project or for all.
    /// Input: the project id or none. Output: each agent's plain status and controls.
    pub async fn agents(&self, project_id: Option<String>) -> Result<Vec<AgentView>, BridgeError> {
        let project = project_id
            .map(|id| ProjectId::new(id).map_err(|_| failure("agent_not_found")))
            .transpose()?;
        self.agent_controls()?
            .agents(project)
            .await
            .map_err(agent_error)
    }

    fn agent_client(&self) -> Result<Arc<OwnerClient>, BridgeError> {
        self.agent_controls()?.shared_client().map_err(agent_error)
    }

    /// Stops one agent and its process group; other agents keep running. Input: agent id.
    pub fn stop_agent(&self, id: String) -> Result<Unlocked<AgentView>, BridgeError> {
        let client = self.agent_client()?;
        Ok(Box::pin(async move {
            client.stop_agent(id).await.map_err(agent_error)
        }))
    }

    /// Interrupts the current turn of one agent, which keeps running. Input: agent id.
    pub fn interrupt_agent(&self, id: String) -> Result<Unlocked<()>, BridgeError> {
        let client = self.agent_client()?;
        Ok(Box::pin(async move {
            client.interrupt_agent(id).await.map_err(agent_error)
        }))
    }

    /// Restarts one agent with its first task. Input: agent id. Output: the agent.
    pub fn restart_agent(&self, id: String) -> Result<Unlocked<AgentView>, BridgeError> {
        let client = self.agent_client()?;
        Ok(Box::pin(async move {
            client.restart_agent(id).await.map_err(agent_error)
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The engine's own refusals of an agent control reach the window as they are, so the
    /// owner sees why a stop or restart did not happen instead of an unknown outcome.
    #[test]
    fn engine_agent_control_codes_pass_through_unchanged() {
        for code in [
            "agent_hosted",
            "agent_restart_unavailable",
            "agent_identity_unavailable",
            "owner_busy",
            "owner_request_failed",
            "peer_owner_mismatch",
            "owner_root_unavailable",
        ] {
            assert_eq!(agent_error(EngineError(code.into())).code, code);
        }
        assert_eq!(
            agent_error(EngineError("launch_spawn_failed".into())).code,
            "agent_launch_failed"
        );
        assert_eq!(
            agent_error(EngineError("Not A Code".into())).code,
            "agents_outcome_unknown"
        );
    }
}
