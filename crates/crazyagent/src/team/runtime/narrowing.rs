use super::{
    ACCESS_MASTER_RESTARTED, ACCESS_WIDER_NEXT_START, ACCESS_WIDER_WHEN_IDLE, Narrowed, Order,
    Restart, TeamRuntime, error,
};
use crate::agents::Agents;
use crate::engine::EngineError;
use crate::project_setup::StoredSetup;
use crate::protocol::ProjectId;
use crate::team::{AgentRole, now_ms};
use kernel_sandbox::policy::{AccessApply, AgentAccess, Narrowing};
use kernel_sandbox::toolchains::Toolchain;
use std::collections::BTreeSet;
use std::sync::atomic::Ordering;

impl TeamRuntime {
    /// Stops the project's agents whose running process holds more than the owner saved now.
    /// At a wider access level (AL-2) a kept or live agent stops at once and a per-turn agent
    /// while its turn runs, since an idle one reads the level again in its next grant. An
    /// agent with tools whose profile was made for a wider network than the saved one (CS-2)
    /// stops whether it runs or idles, since its next turn would run in the same profile. With
    /// a toolchain the saved policy no longer allows any agent stops, since its sandbox
    /// profile was built once at its start; a pane whose record kept no toolchains counts as
    /// holding every family. A stopped agent is held, so its task or message waits again
    /// without a spent attempt, and every next start reads the policy again, so it starts
    /// with the new level and network and without that toolchain. Each agent is checked once
    /// per saved policy. An unreadable policy stops every agent holding more than Sandboxed
    /// with no toolchain and, with tools, the None network, and a project folder that
    /// toolchain detection cannot read stops every agent holding a toolchain the owner made no
    /// choice for, each under its own reason. Takes the project's stored setup.
    pub(super) fn retire_narrowed(&mut self, stored: &StoredSetup) {
        let project = &stored.project_id;
        let saved = crate::sandbox::policy_store::load(&self.connection, project.as_str()).ok();
        let stale: Vec<i64> = self
            .workers
            .iter()
            .filter(|(_, w)| &w.project == project && !w.stopping)
            .filter(|(_, w)| saved.is_none() || w.checked != saved)
            .map(|(id, _)| *id)
            .collect();
        if stale.is_empty() {
            return;
        }
        let root = stored
            .project_folder()
            .canonicalize()
            .ok()
            .map(kernel_sandbox::paths::one_spelling);
        for id in stale {
            let worker = self.workers.get_mut(&id).expect("worker exists");
            let running =
                !worker.managed || worker.live || worker.kept_session || worker.busy.is_some();
            let narrowed = kernel_sandbox::policy::narrowing(
                saved.as_ref(),
                root.as_deref(),
                running.then_some(worker.access),
                worker.tools.then_some(worker.network),
                worker.toolchains.as_deref().unwrap_or(&Toolchain::ALL),
            )
            .map(|cause| match cause {
                Narrowing::TurnedOff | Narrowing::NotAllowed if worker.toolchains.is_none() => {
                    Narrowed::Unrecorded
                }
                cause => Narrowed::Saved(cause),
            });
            let revoked = saved
                .as_ref()
                .is_some_and(|saved| saved.revokes(&worker.access_grants));
            let narrowed = narrowed.or(revoked.then_some(Narrowed::Revoked));
            let Some(narrowed) = narrowed else {
                let widened = saved.as_ref().is_some_and(|saved| {
                    saved.widens(
                        root.as_deref(),
                        running.then_some(worker.access),
                        worker.tools.then_some(worker.network),
                        worker.toolchains.as_deref(),
                        &worker.access_grants,
                    )
                });
                if widened && worker.role != AgentRole::Teammate {
                    self.access_restarts.insert(id, project.clone());
                }
                if widened {
                    self.access_widened.insert(project.clone());
                }
                worker.checked = saved.clone();
                continue;
            };
            self.access_restarts.remove(&id);
            worker.stop_for("engine_stop:narrowed");
            worker.held = true;
            worker.narrowed = Some(narrowed);
            worker.cancel.store(true, Ordering::SeqCst);
            let _ = worker.orders.send(Order::Stop);
        }
    }

    /// Applies a wider saved access to running agents under the owner's Apply access changes
    /// choice (F08): one team note per saved widening; under When idle each agent stops at its
    /// next idle point, the live master as a planned restart that resumes its own session,
    /// and under At its next start none stops. Takes the project's stored setup. Returns any
    /// SQLite failure.
    pub(super) fn restart_widened(&mut self, stored: &StoredSetup) -> Result<(), EngineError> {
        let project = &stored.project_id;
        let announce = self.access_widened.remove(project);
        let ids: Vec<i64> = self
            .access_restarts
            .iter()
            .filter(|(_, owner)| *owner == project)
            .map(|(id, _)| *id)
            .collect();
        if !announce && ids.is_empty() {
            return Ok(());
        }
        assert!(ids.len() <= self.access_restarts.len());
        let apply = crate::sandbox::policy_store::load(&self.connection, project.as_str())
            .map(|policy| policy.apply_changes)
            .unwrap_or_default();
        if announce {
            let text = match apply {
                AccessApply::WhenIdle => ACCESS_WIDER_WHEN_IDLE,
                AccessApply::NextStart => ACCESS_WIDER_NEXT_START,
            };
            self.note(project, text)?;
        }
        for id in ids {
            let gone = self.workers.get(&id).is_none_or(|worker| worker.stopping);
            if gone || apply == AccessApply::NextStart {
                self.access_restarts.remove(&id);
                continue;
            }
            let waits = self.workers[&id].kept_session && self.lead_waits(id)?;
            let quiet = self.workers[&id].live
                && super::master::quiet(
                    &self.connection,
                    self.agents.as_deref().map(Agents::typing),
                    project,
                    now_ms(),
                );
            let worker = self.workers.get_mut(&id).expect("worker exists");
            let idle = worker.busy.is_none() && worker.own_turn.is_none() && !waits;
            let stops = match (worker.role, worker.live) {
                (AgentRole::Master, true) => idle && quiet && worker.turn_started.is_none(),
                (AgentRole::Teammate, _) => false,
                _ => idle,
            };
            if !stops {
                continue;
            }
            let live = worker.live;
            worker.stop_for("engine_stop:widened");
            let _ = worker.orders.send(Order::Stop);
            self.access_restarts.remove(&id);
            if live {
                self.master_restarts.insert(project.clone());
                self.note(project, ACCESS_MASTER_RESTARTED)?;
            }
        }
        Ok(())
    }

    /// Stops and holds the project's running agents that can still open a folder the owner
    /// hid after they started, as a narrowed agent stops: each not stopping, in the sandbox
    /// and not a teammate, whose start hid neither the folder nor one above it, a pane from
    /// before the engine kept that list counting as hiding none. Each stays listed for the
    /// team status and the owner's Restart them until restarted or started again. Takes the
    /// project's stored setup. Returns any SQLite failure.
    pub(super) fn mark_seers(&mut self, stored: &StoredSetup) -> Result<(), EngineError> {
        let project = &stored.project_id;
        let saved = crate::sandbox::access_store::hidden::load(&self.connection, project.as_str())
            .map_err(error)?;
        let workers = &self.workers;
        self.hidden_held
            .retain(|id, _| workers.get(id).is_none_or(|worker| worker.stopping));
        let found: Vec<i64> = self
            .workers
            .iter()
            .filter(|(_, w)| &w.project == project && !w.stopping)
            .filter(|(_, w)| w.access != AgentAccess::Full && w.role != AgentRole::Teammate)
            .filter(|(_, w)| !kernel_sandbox::hidden_paths::covers(w.hidden.as_deref(), &saved))
            .map(|(id, _)| *id)
            .collect();
        for id in found {
            let worker = self.workers.get_mut(&id).expect("worker exists");
            worker.stop_for("engine_stop:hidden");
            worker.held = true;
            worker.narrowed = Some(Narrowed::Hidden);
            worker.cancel.store(true, Ordering::SeqCst);
            let _ = worker.orders.send(Order::Stop);
            let held = (project.clone(), worker.role, worker.live);
            self.hidden_held.insert(id, held);
        }
        let seers: BTreeSet<i64> = self
            .hidden_held
            .iter()
            .filter(|(_, (owner, ..))| owner == project)
            .map(|(id, _)| *id)
            .collect();
        assert!(seers.len() <= self.hidden_held.len());
        crate::sandbox::access_store::hidden::set_seers(&self.connection, project.as_str(), &seers)
            .map_err(error)
    }

    /// The projects a tick steps for a restart: those with an agent restart planned, and
    /// those with a listed agent the owner asked to restart, an ask only a step of that
    /// project serves. Takes nothing. Returns the projects, or any SQLite failure.
    pub(super) fn restarting_projects(&self) -> Result<Vec<ProjectId>, EngineError> {
        let asked = crate::sandbox::access_store::hidden::asked_projects(&self.connection)
            .map_err(error)?;
        let mut projects: Vec<ProjectId> = self
            .restarts
            .values()
            .map(|(project, _)| project.clone())
            .collect();
        projects.extend(
            asked
                .into_iter()
                .filter_map(|project| ProjectId::new(project).ok()),
        );
        assert!(projects.len() >= self.restarts.len());
        Ok(projects)
    }

    /// Serves the owner's Restart them (SA-VID14-A): a listed agent still running stops with its
    /// work held, and one the engine stopped whose process ended starts again, a sub-agent in
    /// its own worktree and conversation and the live master as a planned restart, so its next
    /// start hides every saved folder. Takes the project's stored setup. Returns any SQLite failure.
    pub(super) fn restart_seers(&mut self, stored: &StoredSetup) -> Result<(), EngineError> {
        let project = &stored.project_id;
        let asked =
            crate::sandbox::access_store::hidden::restarts(&self.connection, project.as_str())
                .map_err(error)?;
        for id in asked {
            let Some(worker) = self.workers.get_mut(&id) else {
                if let Some((owner, role, live)) = self.hidden_held.get(&id).cloned()
                    && &owner == project
                {
                    self.hidden_held.remove(&id);
                    match (role, live) {
                        (AgentRole::Sub, _) => {
                            self.restarts.insert(id, (owner, Restart::Hidden));
                        }
                        (AgentRole::Master, true) => {
                            self.master_restarts.insert(owner);
                        }
                        _ => {}
                    }
                }
                continue;
            };
            if &worker.project != project || worker.stopping || worker.role == AgentRole::Teammate {
                continue;
            }
            worker.stop_for("engine_stop:hidden");
            worker.held = true;
            worker.cancel.store(true, Ordering::SeqCst);
            let _ = worker.orders.send(Order::Stop);
            match (worker.role, worker.live) {
                (AgentRole::Sub, _) => {
                    worker.restarting = true;
                    self.restarts.insert(id, (project.clone(), Restart::Hidden));
                }
                (AgentRole::Master, true) => {
                    self.master_restarts.insert(project.clone());
                }
                _ => {}
            }
        }
        Ok(())
    }
}
