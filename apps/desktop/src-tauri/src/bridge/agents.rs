use super::*;
use crazyagent::agents::{AccountUse, AgentView, Holder};
use crazyagent::engine::EngineError;
use crazyagent::engine::owner::{AGENT_CONTROL_CODES, OwnerClient, RELEASE_WAIT};
use crazyagent::master_session::MasterOwnerAction;
use crazyagent::protocol::{CredentialRef, ProjectId};
use std::sync::Arc;

const BRIDGE_CODES: &[&str] = &["not_connected", "agent_launch_failed"];

fn agent_error(error: EngineError) -> BridgeError {
    let code = match error.0.as_str() {
        "controller_disconnected" => "not_connected",
        launch if launch.starts_with("launch_") => "agent_launch_failed",
        code => code,
    };
    failure(
        BRIDGE_CODES
            .iter()
            .chain(AGENT_CONTROL_CODES)
            .copied()
            .find(|known| *known == code)
            .unwrap_or("agents_outcome_unknown"),
    )
}

fn account(account_id: String) -> Result<CredentialRef, BridgeError> {
    CredentialRef::new(account_id).map_err(|_| failure("agent_account_invalid"))
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

    /// Reads everything that holds one account's profile, so a refused sign-out can name
    /// it. Input: the account id. Output: its running agents and other holders.
    pub async fn account_use(&self, account_id: String) -> Result<AccountUse, BridgeError> {
        self.agent_controls()?
            .account_use(account(account_id)?)
            .await
            .map_err(agent_error)
    }

    /// The first step of releasing an account, run without the bridge lock: stops its
    /// running agents, waiting for their hosts, then reads its other holders.
    /// Input: the account id. Output: the holders still to end.
    pub fn stop_account_agents(
        &self,
        account_id: String,
    ) -> Result<Unlocked<Vec<Holder>>, BridgeError> {
        let account = account(account_id)?;
        let client = self.agent_client()?;
        Ok(Box::pin(async move {
            client
                .stop_agents_using(account.clone())
                .await
                .map_err(agent_error)?;
            client.account_holders(account).await.map_err(agent_error)
        }))
    }

    /// The second step of releasing an account: ends each holder through the owner's own
    /// intent, as the owner's other cancels are delivered. A research job is cancelled
    /// and a master conversation is closed; one that already ended is refused, which is
    /// not an error. Inputs: the holders and a fresh draft generation. Output: Ok once
    /// each was answered, or `pending_intent` while one may still be recorded.
    pub async fn end_holders(
        &mut self,
        holders: Vec<Holder>,
        generation: u64,
    ) -> Result<(), BridgeError> {
        for holder in holders {
            let receipt = match holder {
                Holder::Research { project, job, .. } => {
                    self.cancel_research(project.as_str(), job, generation)
                        .await?
                }
                Holder::Master { project, session } => {
                    self.ready_for_intent(generation)?;
                    self.controller()?
                        .read(OwnerRead::ProjectMaster {
                            project_id: project.clone(),
                        })
                        .await
                        .map_err(|_| failure("owner_read_failed"))?;
                    self.controller()?
                        .begin_mutation(
                            OwnerMutation::ProjectMaster {
                                action: MasterOwnerAction::Close {
                                    project_id: project,
                                    session_id: session,
                                },
                            },
                            generation,
                        )
                        .map_err(|_| failure("owner_not_ready"))?;
                    self.deliver_intent(false).await?
                }
            };
            if !matches!(
                receipt.outcome,
                ActionOutcome::Applied | ActionOutcome::Rejected
            ) {
                return Err(failure("pending_intent"));
            }
        }
        Ok(())
    }

    /// The last step of releasing an account, run without the bridge lock: waits until
    /// nothing holds it, at most `RELEASE_WAIT`. Input: the account id. Output: what still
    /// holds it, nothing once all have ended.
    pub fn await_account_free(
        &self,
        account_id: String,
    ) -> Result<Unlocked<AccountUse>, BridgeError> {
        let account = account(account_id)?;
        let client = self.agent_client()?;
        Ok(Box::pin(async move {
            client
                .await_account_free(account, RELEASE_WAIT)
                .await
                .map_err(agent_error)
        }))
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
        for code in AGENT_CONTROL_CODES {
            assert_eq!(agent_error(EngineError((*code).into())).code, *code);
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
