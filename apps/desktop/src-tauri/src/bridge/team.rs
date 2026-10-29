use super::{BridgeError, IntentReceipt, NativeBridge, bounded, caused, failure};
use crazyagent::coord::settings::{CoordSettings, Delivery, HarnessKind, SubagentMode};
use crazyagent::engine::owner::OwnerMutation;
use crazyagent::protocol::ProjectId;
use crazyagent::team::events::{AppEvents, RuleTarget, WakeRule};
use crazyagent::team::{BranchMode, ChildLanding, SwitchMode, TeamAction, TeamStatus};

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
    pub async fn team_status(&self, id: &str) -> Result<TeamStatus, BridgeError> {
        let project_id = project(id)?;
        let status = self
            .reader()?
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

    /// Sets the project team's task and session time limits and its sub-agent idle stop, in
    /// seconds from the listed choices, none for no limit. Takes the project id, the three
    /// limits and the client generation. Returns the actual commit receipt; a value outside
    /// the choices, or a session limit below a set task limit, is refused as
    /// `team_action_invalid` with `team_time_limits_invalid` behind it.
    pub async fn team_time_limits(
        &mut self,
        id: &str,
        turn_secs: Option<u32>,
        session_secs: Option<u32>,
        idle_secs: Option<u32>,
        generation: u64,
    ) -> Result<IntentReceipt, BridgeError> {
        let project_id = project(id)?;
        let action = TeamAction::TimeLimits {
            project_id,
            turn_secs,
            session_secs,
            idle_secs,
        };
        self.team_action(action, generation).await
    }

    /// Sets what the live master does with the conversation when the master harness changes:
    /// `carry` hands the new harness the earlier turns and open tasks, `fresh` starts it
    /// without them. Takes the project id, the mode and the client generation. Returns the
    /// actual commit receipt, or `team_action_invalid` for any other mode.
    pub async fn team_on_harness_switch(
        &mut self,
        id: &str,
        mode: &str,
        generation: u64,
    ) -> Result<IntentReceipt, BridgeError> {
        let project_id = project(id)?;
        let mode = SwitchMode::named(mode).ok_or_else(|| failure("team_action_invalid"))?;
        self.team_action(TeamAction::OnHarnessSwitch { project_id, mode }, generation)
            .await
    }

    /// Sets how deep the project's sub-agents may start workers of their own: 1 lets only
    /// the master start sub-agents, 2 lets its sub-agents lead workers, 3 lets those workers
    /// lead too. Takes the project id, the depth and the client generation. Returns the actual
    /// commit receipt, or the engine's refusal for any other depth.
    pub async fn team_sub_agent_depth(
        &mut self,
        id: &str,
        depth: u8,
        generation: u64,
    ) -> Result<IntentReceipt, BridgeError> {
        let project_id = project(id)?;
        self.team_action(TeamAction::SubAgentDepth { project_id, depth }, generation)
            .await
    }

    /// Sets where a lead's worker lands its change set: `lead` when the lead accepts it onto
    /// its own branch, `owner` only when the owner accepts it. Takes the project id, the mode
    /// and the client generation. Returns the actual commit receipt, or
    /// `team_action_invalid` for any other mode.
    pub async fn team_child_landing(
        &mut self,
        id: &str,
        mode: &str,
        generation: u64,
    ) -> Result<IntentReceipt, BridgeError> {
        let project_id = project(id)?;
        let mode = ChildLanding::named(mode).ok_or_else(|| failure("team_action_invalid"))?;
        self.team_action(TeamAction::ChildLanding { project_id, mode }, generation)
            .await
    }

    /// Sets where the project's finished agents show: in the agents list and team graph, or
    /// only in their Finished fold-out. Takes the project id, true for Show and the client
    /// generation. Returns the actual commit receipt.
    pub async fn team_finished_agents(
        &mut self,
        id: &str,
        show: bool,
        generation: u64,
    ) -> Result<IntentReceipt, BridgeError> {
        let project_id = project(id)?;
        self.team_action(TeamAction::FinishedAgents { project_id, show }, generation)
            .await
    }

    /// Sets where the project's new sub-agents commit: `named`, each on its own branch
    /// `crazyagent/agent-<id>`, or `detached`, the older detached HEAD. Takes the project
    /// id, the mode and the client generation. Returns the actual commit receipt, or
    /// `team_action_invalid` for any other mode.
    pub async fn team_agent_branches(
        &mut self,
        id: &str,
        mode: &str,
        generation: u64,
    ) -> Result<IntentReceipt, BridgeError> {
        let project_id = project(id)?;
        let mode = BranchMode::named(mode).ok_or_else(|| failure("team_action_invalid"))?;
        self.team_action(TeamAction::AgentBranches { project_id, mode }, generation)
            .await
    }

    /// Sets the branch a project's master works on and merges finished harnesses into
    /// (DEC-100), or clears the choice back to the repository's default branch. Takes the
    /// project id, the branch name, None for the default, and the client generation.
    /// Returns the actual commit receipt; the engine refuses a name git cannot hold.
    pub async fn team_master_branch(
        &mut self,
        id: &str,
        branch: Option<String>,
        generation: u64,
    ) -> Result<IntentReceipt, BridgeError> {
        let project_id = project(id)?;
        let branch = branch
            .map(|branch| branch.trim().to_owned())
            .filter(|branch| !branch.is_empty());
        self.team_action(TeamAction::MasterBranch { project_id, branch }, generation)
            .await
    }

    /// Turns a project's "Ask me before merging into the master's branch" setting on or
    /// off: on, the master's merge of a finished harness waits for the owner's Accept.
    /// Takes the project id, the switch and the client generation. Returns the actual
    /// commit receipt.
    pub async fn team_ask_before_merge(
        &mut self,
        id: &str,
        on: bool,
        generation: u64,
    ) -> Result<IntentReceipt, BridgeError> {
        let project_id = project(id)?;
        self.team_action(TeamAction::AskBeforeMerge { project_id, on }, generation)
            .await
    }

    /// Saves a project's coordination settings: where sub-agent questions go, what overlapping
    /// claims do, workflow nudges and the counted tools. Takes the project id, the settings and
    /// the client generation. Returns the actual commit receipt.
    pub async fn team_coordination(
        &mut self,
        id: &str,
        settings: CoordSettings,
        generation: u64,
    ) -> Result<IntentReceipt, BridgeError> {
        let project_id = project(id)?;
        self.team_action(
            TeamAction::CoordSettings {
                project_id,
                settings,
            },
            generation,
        )
        .await
    }

    /// Sets how one harness kind's agents get sub-agents in a project: app-spawned, or native
    /// teammates where the harness has them. Takes the project id, the harness kind, the mode
    /// and the client generation. Returns the actual commit receipt.
    pub async fn team_harness_mode(
        &mut self,
        id: &str,
        harness: HarnessKind,
        mode: SubagentMode,
        generation: u64,
    ) -> Result<IntentReceipt, BridgeError> {
        let project_id = project(id)?;
        self.team_action(
            TeamAction::HarnessMode {
                project_id,
                harness,
                mode,
            },
            generation,
        )
        .await
    }

    /// Sets how messages reach one harness kind's agents in a project: auto, native only or
    /// typing only. Takes the project id, the harness kind, the delivery and the client
    /// generation. Returns the actual commit receipt.
    pub async fn team_harness_delivery(
        &mut self,
        id: &str,
        harness: HarnessKind,
        delivery: Delivery,
        generation: u64,
    ) -> Result<IntentReceipt, BridgeError> {
        let project_id = project(id)?;
        self.team_action(
            TeamAction::HarnessDelivery {
                project_id,
                harness,
                delivery,
            },
            generation,
        )
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

    /// Wakes a finished sub-agent (DEC-102) as a team action by its id, which the team runtime
    /// serves wherever team agents run: the same agent comes back under its own name, worktree
    /// and harness session. Takes the project id, the team agent id and the client generation.
    /// Returns the actual commit receipt; a running agent, a cleaned-up worktree or a system
    /// where team agents cannot run is a plain rejection.
    pub async fn team_wake_agent(
        &mut self,
        id: &str,
        agent: i64,
        generation: u64,
    ) -> Result<IntentReceipt, BridgeError> {
        let project_id = project(id)?;
        self.team_action(TeamAction::WakeAgent { project_id, agent }, generation)
            .await
    }

    async fn team_action(
        &mut self,
        action: TeamAction,
        generation: u64,
    ) -> Result<IntentReceipt, BridgeError> {
        self.ready_for_intent(generation)?;
        action
            .validate()
            .map_err(|error| caused("team_action_invalid", &error.0))?;
        if let Some(project_id) = action.project() {
            self.controller()?
                .project_setup(project_id.clone())
                .await
                .map_err(|e| read_failure(&e.0))?;
        }
        self.controller()?
            .begin_mutation(OwnerMutation::Team { action }, generation)
            .map_err(|error| caused("owner_not_ready", &error.0))?;
        self.deliver_intent(false).await
    }
}
