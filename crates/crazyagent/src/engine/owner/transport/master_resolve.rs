use super::master_runtime::when_storage_frees;
use super::*;
use crate::landing::ChangeSetId;
use crate::master_session::MasterOwnerAction;
use crate::protocol::{ProjectId, SessionId};
use crate::storage::{MasterRuntimeCommand, MasterRuntimeResult};

const STALE_ATTEMPTS: u32 = 3;

impl OwnerRuntimeHandle {
    /// Starts a conversation master's resolve turn right after Ask agent to update merged
    /// the owner's branch into its worktree (DEC-095): the owner's Resolve operation
    /// records the request, then the turn starts now, or after the master's running turn
    /// ends. Inputs: the owner packet and admission, the engine root and the change set.
    /// Output: Ok, also for a team agent's set, or a code listed in `CHANGES_CODES`.
    pub(super) async fn resolve_after_update(
        &self,
        packet: &[u8; 37],
        admission: &Admission,
        root: PathBuf,
        id: &ChangeSetId,
    ) -> Result<(), EngineError> {
        let set = id.clone();
        let master = tokio::task::spawn_blocking(move || {
            crate::landing_turns::master_of(&crate::landing::open(&root)?, &set)
        })
        .await
        .map_err(|_| failure("changes_unavailable"))??;
        let Some((project_id, session_id)) = master else {
            return Ok(());
        };
        self.ask_master_to_resolve(packet, admission, &project_id, &session_id, id)
            .await
            .map_err(|error| {
                if super::changes::CHANGES_CODES.contains(&error.0.as_str()) {
                    error
                } else {
                    failure("changes_outcome_unknown")
                }
            })?;
        when_storage_frees(|| self.start_waiting_now(&project_id, &session_id))
            .await
            .map_err(|error| {
                eprintln!("master_resolve_not_started cause={}", error.0);
                failure("changes_master_not_started")
            })
    }

    /// Commits the owner's Resolve operation for a master's updating change set, at the
    /// current owner revision, again when another owner write moved it first. Inputs: the
    /// owner packet and admission, the project, session and change set. Output: Ok once
    /// applied, or the refusal code.
    async fn ask_master_to_resolve(
        &self,
        packet: &[u8; 37],
        admission: &Admission,
        project_id: &ProjectId,
        session_id: &SessionId,
        id: &ChangeSetId,
    ) -> Result<(), EngineError> {
        for _ in 0..STALE_ATTEMPTS {
            let reply = {
                let _barrier = self.engine.owner_egress.write().await;
                if &self.authority.authenticate(packet)? != admission {
                    return Err(failure("owner_grant_stale_or_revoked"));
                }
                let current = self
                    .engine
                    .owner_job(|reply| {
                        Job::OwnerRead(admission.clone(), OwnerRead::Capabilities, reply)
                    })
                    .await?;
                let request = OwnerRequest {
                    protocol_version: PROTOCOL_VERSION,
                    request_id: RequestId::new(format!(
                        "resolve-{}",
                        crate::engine::owner::random_id()?
                    ))
                    .map_err(|_| failure("owner_identity_encoding"))?,
                    expected_revision: current.revision,
                    operation: OwnerOperation::Apply {
                        mutation: OwnerMutation::ProjectMaster {
                            action: MasterOwnerAction::Resolve {
                                project_id: project_id.clone(),
                                session_id: session_id.clone(),
                                change_set: id.clone(),
                            },
                        },
                    },
                };
                self.engine
                    .dispatch_owner(admission.clone(), request, None)
                    .await?
            };
            let OwnerResponse::Applied { operation } = reply.response else {
                return Err(failure("changes_outcome_unknown"));
            };
            match operation.result {
                OwnerResult::Applied => return Ok(()),
                OwnerResult::Rejected { code } if code == "stale_revision" => {}
                OwnerResult::Rejected { code } if code.starts_with("changes_") => {
                    return Err(EngineError(code));
                }
                OwnerResult::Rejected { .. } => return Err(failure("changes_master_closed")),
                _ => return Err(failure("changes_outcome_unknown")),
            }
        }
        Err(failure("owner_busy"))
    }

    /// Creates the session's waiting resolve turn when no turn is outstanding and starts
    /// its driver, holding the owner barrier and then the dispatcher in the order owner
    /// commits take them. Inputs: the project and session. Output: Ok, also when the
    /// turn still waits for a running one or no driver can start now.
    async fn start_waiting_now(
        &self,
        project_id: &ProjectId,
        session_id: &SessionId,
    ) -> Result<(), EngineError> {
        let _barrier = self.engine.owner_egress.write().await;
        let mut dispatch = self.authority.master_dispatch.lock().await;
        if let Err(error) = dispatch.ready() {
            eprintln!("master_resolve_waiting cause={}", error.0);
            return Ok(());
        }
        let command = MasterRuntimeCommand::StartWaiting {
            host: self.identity(),
            project_id: project_id.clone(),
            session_id: session_id.clone(),
        };
        let turn = match self
            .engine
            .owner_job(|reply| Job::MasterRuntime(Box::new(command), reply))
            .await?
        {
            MasterRuntimeResult::Turn(turn) => turn,
            MasterRuntimeResult::Waiting => return Ok(()),
            _ => return Err(failure("master_runtime_result")),
        };
        let runtime = OwnerRuntimeHandle {
            engine: self.engine.clone(),
            authority: self.authority.clone(),
        };
        dispatch.start(runtime, project_id, session_id, &turn.id)?;
        Ok(())
    }

    /// Creates the session's waiting resolve turn once the turn it waited for settled, for
    /// the driver that drove that turn to drive it next. Inputs: the project and session.
    /// Output: the new turn's id, or None when nothing waits or a turn is outstanding.
    pub(super) async fn start_waiting_resolve(
        &self,
        project_id: &ProjectId,
        session_id: &SessionId,
    ) -> Result<Option<RequestId>, EngineError> {
        let started = when_storage_frees(|| {
            self.master_runtime_command(MasterRuntimeCommand::StartWaiting {
                host: self.identity(),
                project_id: project_id.clone(),
                session_id: session_id.clone(),
            })
        })
        .await?;
        match started {
            MasterRuntimeResult::Turn(turn) => {
                assert_eq!(&turn.session_id, session_id);
                Ok(Some(turn.id))
            }
            MasterRuntimeResult::Waiting => Ok(None),
            _ => Err(failure("master_runtime_result")),
        }
    }
}
