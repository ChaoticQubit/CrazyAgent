use super::*;
use crate::config::ModelAccessEdit;
use crate::engine::EngineError;
use crate::engine::owner::ModelAccessCollection;
use crate::protocol::{Outcome, StoredProject};
use crate::tui::seam::{ProjectCard, ShellEvent, ShellRequest};

const PROJECTS_MAX: usize = 200;

impl Connection {
    /// Queues one shell request on the paired owner worker.
    /// Input: the request. Output: queued, or why not while another request
    /// is pending.
    pub(in crate::tui) fn shell(&mut self, request: ShellRequest) -> Result<(), String> {
        self.owner_job(Job::Shell(Box::new(request)))
    }
}

impl NativeOwner {
    /// Runs one shell request through the owner controller; an engine
    /// transport failure detaches the trusted connection so the shell
    /// reconnects.
    /// Input: the request. Output: the event with data or a bounded plain
    /// problem code.
    pub(super) async fn shell(&mut self, request: ShellRequest) -> ShellEvent {
        let mut lost = false;
        let event = self.shell_event(request, &mut lost).await;
        if lost {
            self.detach();
        }
        event
    }

    async fn shell_event(&mut self, request: ShellRequest, lost: &mut bool) -> ShellEvent {
        match request {
            ShellRequest::Profile => ShellEvent::Profile(match self.ready_controller() {
                Ok(controller) => controller
                    .owner_profile()
                    .await
                    .map_err(|error| transport(&error, lost)),
                Err(error) => Err(error),
            }),
            ShellRequest::SaveProfile {
                profile,
                generation,
            } => ShellEvent::ProfileSaved(match self.ready_controller() {
                Ok(controller) => match controller.save_owner_profile(profile, generation).await {
                    Ok(OwnerResponse::Applied { .. } | OwnerResponse::Committed { .. }) => Ok(()),
                    Ok(OwnerResponse::Rejected { code }) => Err(bounded_problem(&code)),
                    Ok(_) => Err("profile_outcome_unknown".into()),
                    Err(error) => Err(transport(&error, lost)),
                },
                Err(error) => Err(error),
            }),
            ShellRequest::Pools => ShellEvent::Pools(match self.ready_controller() {
                Ok(controller) => controller
                    .pool_summaries()
                    .await
                    .map_err(|error| transport(&error, lost)),
                Err(error) => Err(error),
            }),
            ShellRequest::Projects => ShellEvent::Projects(self.project_cards(lost).await),
            ShellRequest::EditPool { edit, generation } => {
                ShellEvent::PoolEdited(self.edit_pool(edit, generation, lost).await)
            }
        }
    }

    async fn edit_pool(
        &mut self,
        edit: ModelAccessEdit,
        generation: u64,
        lost: &mut bool,
    ) -> Result<(), String> {
        let controller = self.ready_controller()?;
        if controller.pending().is_some() {
            return Err("controller_pending_intent".into());
        }
        let fresh = controller
            .read(OwnerRead::ModelAccessPage {
                collection: ModelAccessCollection::Pools,
                cursor: None,
                limit: 1,
            })
            .await
            .map_err(|error| transport(&error, lost))?;
        let OwnerData::ModelAccessPage {
            source_hash,
            writable,
            ..
        } = fresh.data
        else {
            return Err("pool_read_failed".into());
        };
        if !writable {
            return Err("model_access_read_only".into());
        }
        controller
            .begin_mutation(
                OwnerMutation::ModelAccess {
                    expected_config_id: fresh.config_id,
                    expected_source_hash: source_hash,
                    edit,
                },
                generation,
            )
            .map_err(|error| bounded_problem(&error.0))?;
        match controller.send_pending().await {
            Ok(OwnerResponse::Applied { .. } | OwnerResponse::Committed { .. }) => Ok(()),
            Ok(OwnerResponse::Rejected { code }) => Err(bounded_problem(&code)),
            Ok(_) => Err("pool_outcome_unknown".into()),
            Err(error) => Err(transport(&error, lost)),
        }
    }

    fn ready_controller(&mut self) -> Result<&mut OwnerController, String> {
        if !self.ready {
            return Err("trusted_connection_unavailable".into());
        }
        self.controller
            .as_mut()
            .ok_or_else(|| "trusted_connection_unavailable".into())
    }

    async fn project_cards(&mut self, lost: &mut bool) -> Result<Vec<ProjectCard>, String> {
        let reply = self
            .ordinary()
            .request(WireRequest {
                protocol_version: PROTOCOL_VERSION,
                client_id: ClientId::new("tui-home").map_err(|error| error.to_string())?,
                request_id: RequestId::new(
                    crate::engine::owner::random_id().map_err(|error| error.to_string())?,
                )
                .map_err(|error| error.to_string())?,
                expected_revision: Cursor::new(0),
                command: Command::Snapshot,
            })
            .await
            .map_err(|error| {
                *lost = true;
                bounded_problem(&error.to_string())
            })?;
        let Outcome::Snapshot { projects, .. } = reply.outcome else {
            return Err("projects_read_failed".into());
        };
        let projects: Vec<StoredProject> = projects.into_iter().take(PROJECTS_MAX).collect();
        let controller = self.ready_controller()?;
        let pools = controller
            .pool_summaries()
            .await
            .map_err(|error| transport(&error, lost))?;
        let mut cards = Vec::with_capacity(projects.len());
        for project in &projects {
            let setup = controller
                .project_setup(project.project_id.clone())
                .await
                .map_err(|error| transport(&error, lost))?;
            let jobs = if setup.is_some() {
                controller
                    .research_jobs(project.project_id.clone())
                    .await
                    .map_err(|error| transport(&error, lost))?
            } else {
                Vec::new()
            };
            cards.push(ProjectCard::build(project, setup.as_ref(), &jobs, &pools));
        }
        cards.sort_by(|left, right| {
            right
                .created_ms
                .cmp(&left.created_ms)
                .then_with(|| left.name.cmp(&right.name))
        });
        assert!(cards.len() <= PROJECTS_MAX);
        Ok(cards)
    }
}

fn transport(error: &EngineError, lost: &mut bool) -> String {
    *lost = true;
    bounded_problem(&error.0)
}
