use super::{BridgeError, IntentReceipt, NativeBridge, bounded, failure};
use crazyagent::engine::owner::OwnerMutation;
use crazyagent::protocol::ProjectId;
use crazyagent::team::{TeamAction, TeamStatus};

fn read_failure(code: &str) -> BridgeError {
    failure(match code {
        "project_setup_missing" => "project_setup_missing",
        "project_missing" => "project_missing",
        _ => "owner_read_failed",
    })
}

fn project(id: &str) -> Result<ProjectId, BridgeError> {
    ProjectId::new(id).map_err(|_| failure("invalid_project"))
}

impl NativeBridge {
    /// Reads a project's team: harness plan, agents, tasks and the master conversation.
    /// Takes the project id.
    /// Returns the bounded team status or a plain failure code.
    pub async fn team_status(&mut self, id: &str) -> Result<TeamStatus, BridgeError> {
        let project_id = project(id)?;
        let status = self
            .controller()?
            .team_status(project_id)
            .await
            .map_err(|e| read_failure(&e.0))?;
        bounded(status)
    }

    /// Sends one message from the owner to the project's master agent.
    /// Takes the project id, the message and the client generation.
    /// Returns the actual commit receipt.
    pub async fn team_message(
        &mut self,
        id: &str,
        text: String,
        generation: u64,
    ) -> Result<IntentReceipt, BridgeError> {
        let project_id = project(id)?;
        self.team_action(TeamAction::Message { project_id, text }, generation)
            .await
    }

    /// Hands one task straight to a sub-agent when the master cannot delegate.
    /// Takes the project id, an optional harness of the team plan, the task and the client generation.
    /// Returns the actual commit receipt; a harness outside the plan is a plain rejection.
    pub async fn team_delegate(
        &mut self,
        id: &str,
        harness: Option<String>,
        prompt: String,
        generation: u64,
    ) -> Result<IntentReceipt, BridgeError> {
        let project_id = project(id)?;
        let action = TeamAction::Delegate {
            project_id,
            harness,
            prompt,
        };
        self.team_action(action, generation).await
    }

    /// Cancels one team task, or every unfinished task when no task is named.
    /// Takes the project id, the optional task id and the client generation.
    /// Returns the actual commit receipt; running tasks stop with their process group.
    pub async fn team_cancel(
        &mut self,
        id: &str,
        task: Option<i64>,
        generation: u64,
    ) -> Result<IntentReceipt, BridgeError> {
        let project_id = project(id)?;
        self.team_action(TeamAction::Cancel { project_id, task }, generation)
            .await
    }

    /// Turns keeping the plan's minimum sub-agents running on or off.
    /// Takes the project id, the switch and the client generation.
    /// Returns the actual commit receipt.
    pub async fn team_keep_warm(
        &mut self,
        id: &str,
        on: bool,
        generation: u64,
    ) -> Result<IntentReceipt, BridgeError> {
        let project_id = project(id)?;
        self.team_action(TeamAction::KeepWarm { project_id, on }, generation)
            .await
    }

    /// Pauses every agent of a project's team, or resumes it.
    /// Takes the project id, true to pause or false to resume, and the client generation.
    /// Returns the actual commit receipt.
    pub async fn team_pause(
        &mut self,
        id: &str,
        paused: bool,
        generation: u64,
    ) -> Result<IntentReceipt, BridgeError> {
        let project_id = project(id)?;
        let action = if paused {
            TeamAction::Pause { project_id }
        } else {
            TeamAction::Resume { project_id }
        };
        self.team_action(action, generation).await
    }

    async fn team_action(
        &mut self,
        action: TeamAction,
        generation: u64,
    ) -> Result<IntentReceipt, BridgeError> {
        self.ready_for_intent(generation)?;
        action
            .validate()
            .map_err(|_| failure("team_action_invalid"))?;
        self.controller()?
            .team_status(action.project().clone())
            .await
            .map_err(|e| read_failure(&e.0))?;
        self.controller()?
            .begin_mutation(OwnerMutation::Team { action }, generation)
            .map_err(|_| failure("owner_not_ready"))?;
        self.deliver_intent(false).await
    }
}
