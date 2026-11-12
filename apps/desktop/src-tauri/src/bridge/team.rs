use super::{BridgeError, IntentReceipt, NativeBridge, bounded, caused, failure};
use crazyagent::coord::settings::{CoordSettings, Delivery, HarnessKind, SubagentMode};
use crazyagent::engine::owner::OwnerMutation;
use crazyagent::protocol::ProjectId;
use crazyagent::team::agent_messages::{AgentMessages, Cursor, MessageSettings};
use crazyagent::team::events::{AppEvents, RuleTarget, WakeRule};
use crazyagent::team::{
    BackgroundAtEnd, BranchMode, CatchUpMain, ChildLanding, MasterIdleStop, MasterLands, ReuseIdle,
    SwitchApply, SwitchMode, TeamAction, TeamStatus, TerminalsPerWindow,
};

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

    /// Reads one page of a project sub-agent's Messages timeline, newest first: its tasks,
    /// what the master and the owner said to it and its events, each with the speaker the
    /// engine stored. Takes the project id, the agent, the cursor of the newest entry not
    /// wanted and the page size, none for the project's setting. Returns the bounded page or a
    /// plain failure code.
    pub async fn team_agent_messages(
        &self,
        id: &str,
        agent: i64,
        before: Option<Cursor>,
        limit: Option<u16>,
    ) -> Result<AgentMessages, BridgeError> {
        let project_id = project(id)?;
        let messages = self
            .reader()?
            .team_agent_messages(project_id, agent, before, limit)
            .await
            .map_err(|e| read_failure(&e.0))?;
        bounded(messages)
    }

    /// Stores a project's Messages settings whole: keeping the master's messages, the kept
    /// count, progress events, how results show, the owner's own items and the page size.
    /// Takes the project id, the settings and the client generation. Returns the actual
    /// commit receipt; a value outside the choices is refused as `team_action_invalid` with
    /// `team_message_settings_invalid` behind it.
    pub async fn team_message_settings(
        &mut self,
        id: &str,
        settings: MessageSettings,
        generation: u64,
    ) -> Result<IntentReceipt, BridgeError> {
        let project_id = project(id)?;
        self.team_action(
            TeamAction::MessageSettings {
                project_id,
                settings,
            },
            generation,
        )
        .await
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
    /// Takes the level, the rule or none to fall back to the level above, the version the
    /// screen showed and the generation. Returns the actual commit receipt; a cap over the
    /// bound and a rule changed since are plain rejections.
    pub async fn team_wake_rule(
        &mut self,
        target: RuleTarget,
        rule: Option<WakeRule>,
        version: u64,
        generation: u64,
    ) -> Result<IntentReceipt, BridgeError> {
        self.team_action(
            TeamAction::Wake {
                target,
                rule,
                version,
            },
            generation,
        )
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
    /// Takes the project id, the switch, the team settings version the client read and the
    /// client generation. Returns the actual commit receipt; a save from an older read is
    /// refused as `stale_revision`.
    pub async fn team_keep_warm(
        &mut self,
        id: &str,
        on: bool,
        version: u64,
        generation: u64,
    ) -> Result<IntentReceipt, BridgeError> {
        let project_id = project(id)?;
        let action = TeamAction::KeepWarm {
            project_id,
            on,
            version,
        };
        self.team_action(action, generation).await
    }

    /// Sets the project team's task and session time limits and its sub-agent idle stop, in
    /// seconds from the listed choices, none for no limit. Takes the project id, the three
    /// limits, the team settings version the client read and the client generation. Returns
    /// the actual commit receipt; a value outside the choices, or a session limit below a set
    /// task limit, is refused as `team_action_invalid` with `team_time_limits_invalid` behind
    /// it, and a save from an older read as `stale_revision`.
    pub async fn team_time_limits(
        &mut self,
        id: &str,
        turn_secs: Option<u32>,
        session_secs: Option<u32>,
        idle_secs: Option<u32>,
        version: u64,
        generation: u64,
    ) -> Result<IntentReceipt, BridgeError> {
        let project_id = project(id)?;
        let action = TeamAction::TimeLimits {
            project_id,
            turn_secs,
            session_secs,
            idle_secs,
            version,
        };
        self.team_action(action, generation).await
    }

    /// Sets what the live master does with the conversation when the master harness changes:
    /// `carry` hands the new harness the earlier turns and open tasks, `fresh` starts it
    /// without them. Takes the project id, the mode, the team settings version the client read
    /// and the client generation. Returns the actual commit receipt, `team_action_invalid` for
    /// any other mode, or `stale_revision` for a save from an older read.
    pub async fn team_on_harness_switch(
        &mut self,
        id: &str,
        mode: &str,
        version: u64,
        generation: u64,
    ) -> Result<IntentReceipt, BridgeError> {
        let project_id = project(id)?;
        let mode = SwitchMode::named(mode).ok_or_else(|| failure("team_action_invalid"))?;
        let action = TeamAction::OnHarnessSwitch {
            project_id,
            mode,
            version,
        };
        self.team_action(action, generation).await
    }

    /// Sets when a saved master harness change reaches the running live master: `when_idle`
    /// switches it once it is idle, `next_start` keeps it on its harness until it starts
    /// again. Takes the project id, the choice, the team settings version the client read and
    /// the client generation. Returns the actual commit receipt, `team_action_invalid` for any
    /// other choice, or `stale_revision` for a save from an older read.
    pub async fn team_apply_harness_switch(
        &mut self,
        id: &str,
        mode: &str,
        version: u64,
        generation: u64,
    ) -> Result<IntentReceipt, BridgeError> {
        let project_id = project(id)?;
        let mode = SwitchApply::named(mode).ok_or_else(|| failure("team_action_invalid"))?;
        let action = TeamAction::ApplyHarnessSwitch {
            project_id,
            mode,
            version,
        };
        self.team_action(action, generation).await
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
    /// only in their Finished fold-out. Takes the project id, true for Show, the team settings
    /// version the client read and the client generation. Returns the actual commit receipt;
    /// a save from an older read is refused as `stale_revision`.
    pub async fn team_finished_agents(
        &mut self,
        id: &str,
        show: bool,
        version: u64,
        generation: u64,
    ) -> Result<IntentReceipt, BridgeError> {
        let project_id = project(id)?;
        let action = TeamAction::FinishedAgents {
            project_id,
            show,
            version,
        };
        self.team_action(action, generation).await
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

    /// Sets the branch a project's team work lands on (DEC-111 Q1): the master's merges,
    /// an Accept with no branch named and the measuring of change sets, or clears it back
    /// to the Master's branch. Takes the project id, the branch name, None for the Master's
    /// branch, and the client generation. Returns the actual commit receipt; the engine
    /// refuses a name git cannot hold.
    pub async fn team_land_on(
        &mut self,
        id: &str,
        branch: Option<String>,
        generation: u64,
    ) -> Result<IntentReceipt, BridgeError> {
        let project_id = project(id)?;
        let branch = branch
            .map(|branch| branch.trim().to_owned())
            .filter(|branch| !branch.is_empty());
        self.team_action(TeamAction::LandOn { project_id, branch }, generation)
            .await
    }

    /// Sets what a project's tasks do at their turn end while commands their agents started
    /// in the background still run (DEC-111 Q13): wait, a grace time, or stop. Takes the
    /// project id, the choice and the client generation. Returns the actual commit receipt;
    /// the engine refuses a grace time outside 10 to 3600 s.
    pub async fn team_background(
        &mut self,
        id: &str,
        choice: BackgroundAtEnd,
        generation: u64,
    ) -> Result<IntentReceipt, BridgeError> {
        let project_id = project(id)?;
        self.team_action(
            TeamAction::BackgroundAtEnd { project_id, choice },
            generation,
        )
        .await
    }

    /// Turns a project's "Master accepts change sets" setting on or
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

    /// Sets what a Full-access master does with its own conflict fixes: merge them into main
    /// itself, or leave them waiting for the owner's Accept. Takes the project id, the choice
    /// and the client generation. Returns the actual commit receipt.
    pub async fn team_master_lands(
        &mut self,
        id: &str,
        mode: MasterLands,
        generation: u64,
    ) -> Result<IntentReceipt, BridgeError> {
        let project_id = project(id)?;
        self.team_action(TeamAction::MasterLands { project_id, mode }, generation)
            .await
    }

    /// Turns a project's "Re-ask a sub-agent's empty reply" setting on or off: on, a
    /// sub-agent whose task ends with no reply text is asked once more for its reply.
    /// Takes the project id, the switch and the client generation. Returns the actual
    /// commit receipt.
    pub async fn team_reask_empty_reply(
        &mut self,
        id: &str,
        on: bool,
        generation: u64,
    ) -> Result<IntentReceipt, BridgeError> {
        let project_id = project(id)?;
        self.team_action(TeamAction::ReaskEmptyReply { project_id, on }, generation)
            .await
    }

    /// Saves a project's "End a turn after done" choice (F15): how long a sub-agent's task
    /// turn may stay quiet after its agent reported done before the engine ends it, in
    /// seconds, none for Off. Takes the project id, the choice and the client generation.
    /// Returns the actual commit receipt.
    pub async fn team_done_quiet(
        &mut self,
        id: &str,
        secs: Option<u32>,
        generation: u64,
    ) -> Result<IntentReceipt, BridgeError> {
        let project_id = project(id)?;
        self.team_action(TeamAction::DoneQuiet { project_id, secs }, generation)
            .await
    }

    /// Saves how many terminals each of a project's terminal wall windows holds: 4, 6 or 9.
    /// Takes the project id, the choice and the client generation. Returns the actual commit
    /// receipt.
    pub async fn team_terminals_per_window(
        &mut self,
        id: &str,
        per_window: TerminalsPerWindow,
        generation: u64,
    ) -> Result<IntentReceipt, BridgeError> {
        let project_id = project(id)?;
        self.team_action(
            TeamAction::TerminalsPerWindow {
                project_id,
                per_window,
            },
            generation,
        )
        .await
    }

    /// Reads how many terminals each of a project's terminal wall windows holds, the one
    /// team setting a wall window reads. Takes the project id. Returns the choice.
    pub async fn terminals_per_window(&self, id: &str) -> Result<TerminalsPerWindow, BridgeError> {
        Ok(self.team_status(id).await?.terminals_per_window)
    }

    /// Sets a project's "Catch up with main" setting: `on` lets every agent merge
    /// `crazyagent/main` into its own branch and tells it to, `off` makes the git the app
    /// writes for an agent refuse that merge, except a master at Full access, a guide and
    /// not a lock, since landed work can still come in another way (OS-8). Takes the project id, the mode and the client generation. Returns the
    /// actual commit receipt, or `team_action_invalid` for any other mode.
    pub async fn team_catch_up_main(
        &mut self,
        id: &str,
        mode: &str,
        generation: u64,
    ) -> Result<IntentReceipt, BridgeError> {
        let project_id = project(id)?;
        let mode = CatchUpMain::named(mode).ok_or_else(|| failure("team_action_invalid"))?;
        self.team_action(TeamAction::CatchUpMain { project_id, mode }, generation)
            .await
    }

    /// Saves a project's "Stop the master when idle after" choice: `off`, `fifteen_minutes`
    /// or `one_hour` (DEC-117 Q4). Takes the project id, the choice and the client
    /// generation. Returns the actual commit receipt, or `team_action_invalid` for any other
    /// choice.
    pub async fn team_master_idle_stop(
        &mut self,
        id: &str,
        choice: &str,
        generation: u64,
    ) -> Result<IntentReceipt, BridgeError> {
        let project_id = project(id)?;
        let choice = MasterIdleStop::named(choice).ok_or_else(|| failure("team_action_invalid"))?;
        self.team_action(
            TeamAction::MasterIdleStop { project_id, choice },
            generation,
        )
        .await
    }

    /// Saves a project's "Live terminal" setting: on lets its master run in a live terminal
    /// (DEC-117 Q4). Takes the project id, the setting and the client generation. Returns the
    /// actual commit receipt.
    pub async fn team_master_live_terminal(
        &mut self,
        id: &str,
        on: bool,
        generation: u64,
    ) -> Result<IntentReceipt, BridgeError> {
        let project_id = project(id)?;
        self.team_action(
            TeamAction::MasterLiveTerminal { project_id, on },
            generation,
        )
        .await
    }

    /// Sets a project's "Reuse idle agents" setting: `off` starts a new agent for each new
    /// task, `on` lets an idle agent of the same lead take it (HB-S5-A). Takes the project
    /// id, the mode and the client generation. Returns the actual commit receipt, or
    /// `team_action_invalid` for any other mode.
    pub async fn team_reuse_idle(
        &mut self,
        id: &str,
        mode: &str,
        generation: u64,
    ) -> Result<IntentReceipt, BridgeError> {
        let project_id = project(id)?;
        let mode = ReuseIdle::named(mode).ok_or_else(|| failure("team_action_invalid"))?;
        self.team_action(TeamAction::ReuseIdle { project_id, mode }, generation)
            .await
    }

    /// Sets the most files one new top-level folder, a folder the owner's branch does not
    /// have, may gain in a project's change set before the engine refuses the set (DEC-107).
    /// Takes the project id, the limit and the client generation. Returns the actual commit
    /// receipt.
    pub async fn team_new_folder_limit(
        &mut self,
        id: &str,
        files: u32,
        generation: u64,
    ) -> Result<IntentReceipt, BridgeError> {
        let project_id = project(id)?;
        self.team_action(TeamAction::NewFolderLimit { project_id, files }, generation)
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
    /// app running in a terminal the owner types into. Takes the project id, the switch, the
    /// team settings version the client read and the client generation. Returns the actual
    /// commit receipt; a save from an older read is refused as `stale_revision`.
    pub async fn team_live_master(
        &mut self,
        id: &str,
        on: bool,
        version: u64,
        generation: u64,
    ) -> Result<IntentReceipt, BridgeError> {
        let project_id = project(id)?;
        let action = TeamAction::LiveMaster {
            project_id,
            on,
            version,
        };
        self.team_action(action, generation).await
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

    /// Stops one harness, a sub-agent the master started, with the teammates in its
    /// terminal; it stays stopped across restarts until the owner wakes it. Takes the project
    /// id, the team agent id and the client generation. Returns the actual commit receipt; the
    /// master is a plain rejection, it stops with the whole team.
    pub async fn team_stop_agent(
        &mut self,
        id: &str,
        agent: i64,
        generation: u64,
    ) -> Result<IntentReceipt, BridgeError> {
        let project_id = project(id)?;
        self.team_action(TeamAction::StopAgent { project_id, agent }, generation)
            .await
    }

    /// Stops one teammate a harness split off in its terminal, and tells the harness. Takes
    /// the project id, the harness agent id, the teammate's pane id and the client generation.
    /// Returns the actual commit receipt; a teammate that already ended is a plain rejection.
    pub async fn team_stop_teammate(
        &mut self,
        id: &str,
        agent: i64,
        pane: String,
        generation: u64,
    ) -> Result<IntentReceipt, BridgeError> {
        let project_id = project(id)?;
        self.team_action(
            TeamAction::StopTeammate {
                project_id,
                agent,
                pane,
            },
            generation,
        )
        .await
    }

    /// Sets Max harnesses per project: how many sub-agents the master's work runs at once,
    /// None for No limit. Takes the project id, the cap and the client generation. Returns the
    /// actual commit receipt.
    pub async fn team_max_harnesses(
        &mut self,
        id: &str,
        max: Option<u8>,
        generation: u64,
    ) -> Result<IntentReceipt, BridgeError> {
        let project_id = project(id)?;
        self.team_action(TeamAction::MaxHarnesses { project_id, max }, generation)
            .await
    }

    /// Sets Max teammates per harness, None for No limit. Takes the project id, the cap and
    /// the client generation. Returns the actual commit receipt.
    pub async fn team_max_teammates(
        &mut self,
        id: &str,
        max: Option<u8>,
        generation: u64,
    ) -> Result<IntentReceipt, BridgeError> {
        let project_id = project(id)?;
        self.team_action(TeamAction::MaxTeammates { project_id, max }, generation)
            .await
    }

    /// Sets Sub-agent rounds per message, None for Until the goal is done (F09). Takes the
    /// project id, the choice and the client generation. Returns the actual commit receipt.
    pub async fn team_master_rounds(
        &mut self,
        id: &str,
        max: Option<u8>,
        generation: u64,
    ) -> Result<IntentReceipt, BridgeError> {
        let project_id = project(id)?;
        self.team_action(TeamAction::MasterRounds { project_id, max }, generation)
            .await
    }

    /// Sets Master run time in seconds, None for No limit (F01). Takes the project id, the
    /// choice and the client generation. Returns the actual commit receipt.
    pub async fn team_master_run_time(
        &mut self,
        id: &str,
        secs: Option<u32>,
        generation: u64,
    ) -> Result<IntentReceipt, BridgeError> {
        let project_id = project(id)?;
        self.team_action(TeamAction::MasterRunTime { project_id, secs }, generation)
            .await
    }

    /// Sets the project's Token budget: once its agents use that many tokens from now, the
    /// engine stops the whole team and notifies the owner. None turns it off. Takes the
    /// project id, the budget and the client generation. Returns the actual commit receipt.
    pub async fn team_token_budget(
        &mut self,
        id: &str,
        tokens: Option<u64>,
        generation: u64,
    ) -> Result<IntentReceipt, BridgeError> {
        let project_id = project(id)?;
        self.team_action(TeamAction::TokenBudget { project_id, tokens }, generation)
            .await
    }

    /// Turns the project's Spread harnesses across accounts on or off: on, a new or woken
    /// agent prefers a pool account no running agent of the project uses. Takes the project
    /// id, the switch and the client generation. Returns the actual commit receipt.
    pub async fn team_spread_accounts(
        &mut self,
        id: &str,
        on: bool,
        generation: u64,
    ) -> Result<IntentReceipt, BridgeError> {
        let project_id = project(id)?;
        self.team_action(TeamAction::SpreadAccounts { project_id, on }, generation)
            .await
    }

    pub(super) async fn team_action(
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
