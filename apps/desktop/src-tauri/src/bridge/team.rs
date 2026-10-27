use super::{BridgeError, IntentReceipt, NativeBridge, bounded, caused, failure};
use crazyagent::engine::owner::OwnerMutation;
use crazyagent::protocol::ProjectId;
use crazyagent::team::events::{AppEvents, RuleTarget, WakeRule};
use crazyagent::team::{TeamAction, TeamStatus};

pub(super) fn read_failure(code: &str) -> BridgeError {
    caused(
        match code {
            "project_setup_missing" => "project_setup_missing",
            "project_missing" => "project_missing",
            _ => "owner_read_failed",
        },
        code,
    )
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

    /// Reads the app's own wake rule and every project's newest owner notifications.
    /// Returns the bounded view with the unseen count for the badge, or a plain failure code.
    pub async fn team_events(&mut self) -> Result<AppEvents, BridgeError> {
        let events = self
            .controller()?
            .team_events()
            .await
            .map_err(|e| read_failure(&e.0))?;
        bounded(events)
    }

    /// Sets or clears the wake rule of one level: the app default, a project or one agent.
    /// Takes the level, the rule or none to fall back to the level above, and the generation.
    /// Returns the actual commit receipt; a cap over the bound is a plain rejection.
    pub async fn team_wake_rule(
        &mut self,
        target: RuleTarget,
        rule: Option<WakeRule>,
        generation: u64,
    ) -> Result<IntentReceipt, BridgeError> {
        self.team_action(TeamAction::Wake { target, rule }, generation)
            .await
    }

    /// Marks a project's owner notifications up to one id as seen, so they leave the badge.
    /// Takes the project id, the newest seen notification id and the client generation.
    /// Returns the actual commit receipt.
    pub async fn team_seen(
        &mut self,
        id: &str,
        upto: i64,
        generation: u64,
    ) -> Result<IntentReceipt, BridgeError> {
        let project_id = project(id)?;
        self.team_action(TeamAction::Seen { project_id, upto }, generation)
            .await
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

    /// Turns a project's live master on or off: on, the engine keeps the master harness's own
    /// app running in a terminal the owner types into. Takes the project id, the switch and
    /// the client generation. Returns the actual commit receipt.
    pub async fn team_live_master(
        &mut self,
        id: &str,
        on: bool,
        generation: u64,
    ) -> Result<IntentReceipt, BridgeError> {
        let project_id = project(id)?;
        self.team_action(TeamAction::LiveMaster { project_id, on }, generation)
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

    /// Hands off one sub-agent task the master proposed in a reply but the engine has not created.
    /// Takes the project id, the master reply id, the step number and the client generation.
    /// Returns the actual commit receipt.
    pub async fn team_hand_off(
        &mut self,
        id: &str,
        reply: i64,
        step: u8,
        generation: u64,
    ) -> Result<IntentReceipt, BridgeError> {
        let project_id = project(id)?;
        self.team_action(
            TeamAction::HandOff {
                project_id,
                reply,
                step,
            },
            generation,
        )
        .await
    }

    /// Renames a live sub-agent's role handle, the `@name` the owner and master address it by.
    /// Takes the project id, the team agent id, the new kebab-case handle and the client generation.
    /// Returns the actual commit receipt; a taken or malformed handle is a plain rejection.
    pub async fn team_rename(
        &mut self,
        id: &str,
        agent: i64,
        handle: String,
        generation: u64,
    ) -> Result<IntentReceipt, BridgeError> {
        let project_id = project(id)?;
        let action = TeamAction::Rename {
            project_id,
            agent,
            handle,
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
        if let Some(project_id) = action.project() {
            self.controller()?
                .team_status(project_id.clone())
                .await
                .map_err(|e| read_failure(&e.0))?;
        }
        self.controller()?
            .begin_mutation(OwnerMutation::Team { action }, generation)
            .map_err(|error| caused("owner_not_ready", &error.0))?;
        self.deliver_intent(false).await
    }
}
