use super::{
    EngineError, OwnerApplied, OwnerMutation, OwnerOperation, OwnerReply, OwnerRequest,
    OwnerResponse, OwnerResult, OwnerRuntimeHandle, failure,
};
use crate::master_session::{MasterOwnerAction, TURNS_MAX};
use crate::protocol::{ProjectId, RequestId, SessionId};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::task::{Id, JoinError, JoinSet};
use tokio::time::{Instant, timeout_at};

const SHUTDOWN_GRACE: Duration = Duration::from_secs(1);
const DRIVE_ATTEMPTS: u32 = 6;
const DRIVE_PAUSE: Duration = Duration::from_millis(750);
pub(super) const UNKNOWN_OUTCOME: &str = "owner_outcome_unknown_reconcile_same_identity";

struct Dispatch {
    project_id: ProjectId,
    session_id: SessionId,
    turn_id: RequestId,
    task: Id,
    finished: bool,
}

/// The master turn drivers this engine run started, at most `TURNS_MAX` remembered. A
/// finished driver is joined and its result recorded as soon as the next turn arrives, so
/// only live drivers count against the bound; finished entries stay, oldest evicted
/// first, so a repeated commit of a recent turn never starts a second driver.
pub(super) struct MasterDispatch {
    entries: Vec<Dispatch>,
    tasks: JoinSet<Result<(), EngineError>>,
    closing: Option<Instant>,
    stopping: Arc<AtomicBool>,
    uncertain: bool,
    first_error: Option<String>,
}

impl MasterDispatch {
    #[cfg(unix)]
    pub(super) fn new() -> Self {
        let dispatcher = Self {
            entries: Vec::new(),
            tasks: JoinSet::new(),
            closing: None,
            stopping: Arc::new(AtomicBool::new(false)),
            uncertain: false,
            first_error: None,
        };
        dispatcher.assert_consistent();
        dispatcher
    }

    /// Checks, before storage commits a master Submit, that its driver could start now,
    /// so a refusal reaches the owner as a request that changed nothing. Finished drivers
    /// are reaped first. Input: the request; any other request passes. Output: Ok, or
    /// `master_dispatch_stopped` or `master_dispatch_capacity`.
    pub(super) fn room(&mut self, request: &OwnerRequest) -> Result<(), EngineError> {
        self.assert_consistent();
        if !matches!(
            &request.operation,
            OwnerOperation::Apply {
                mutation: OwnerMutation::ProjectMaster {
                    action: MasterOwnerAction::Submit { .. }
                }
            }
        ) {
            return Ok(());
        }
        self.ready()
    }

    /// Checks that one more driver could start now, after reaping finished ones. Output:
    /// Ok, or `master_dispatch_stopped` once shutdown began, or `master_dispatch_capacity`
    /// while `TURNS_MAX` drivers run.
    pub(super) fn ready(&mut self) -> Result<(), EngineError> {
        if self.closing.is_some() {
            return Err(failure("master_dispatch_stopped"));
        }
        self.reap();
        if self.entries.len() == TURNS_MAX && self.entries.iter().all(|entry| !entry.finished) {
            return Err(failure("master_dispatch_capacity"));
        }
        self.assert_consistent();
        Ok(())
    }

    /// Starts the driver of a master turn the engine created from a waiting resolve
    /// request (DEC-095), after `ready` passed under the same lock. Inputs: the runtime
    /// handle, the turn's project, session and id. Output: true when a driver started,
    /// false when this turn already had one, or the dispatch refusal.
    pub(super) fn start(
        &mut self,
        runtime: OwnerRuntimeHandle,
        project_id: &ProjectId,
        session_id: &SessionId,
        turn_id: &RequestId,
    ) -> Result<bool, EngineError> {
        self.ready()?;
        self.spawn(runtime, project_id, session_id, turn_id)
    }

    pub(super) fn committed(
        &mut self,
        runtime: OwnerRuntimeHandle,
        request: &OwnerRequest,
        reply: &OwnerReply,
    ) -> Result<bool, EngineError> {
        self.assert_consistent();
        if self.closing.is_some() {
            return Err(failure("master_dispatch_stopped"));
        }
        request.validate()?;
        if reply.request_id != request.request_id {
            return Err(failure("master_dispatch_binding_mismatch"));
        }
        let OwnerOperation::Apply {
            mutation:
                OwnerMutation::ProjectMaster {
                    action:
                        MasterOwnerAction::Submit {
                            project_id,
                            session_id,
                            ..
                        },
                },
        } = &request.operation
        else {
            return Ok(false);
        };
        let OwnerResponse::Applied { operation } = &reply.response else {
            return Ok(false);
        };
        if operation.historical_replay || operation.result != OwnerResult::Applied {
            return Ok(false);
        }
        validate_commit(&runtime, request, operation)?;
        self.reap();
        self.spawn(runtime, project_id, session_id, &request.request_id)
    }

    /// Spawns the driver of one master turn unless this run already started one for it.
    /// After its turn settles the driver starts and drives the session's waiting resolve
    /// turn, if any, so Ask agent to update runs right after the turn it waited for.
    /// Inputs: the runtime handle and the turn's project, session and id. Output: true
    /// when a driver started, or the capacity refusal.
    fn spawn(
        &mut self,
        runtime: OwnerRuntimeHandle,
        project_id: &ProjectId,
        session_id: &SessionId,
        turn_id: &RequestId,
    ) -> Result<bool, EngineError> {
        assert!(self.closing.is_none());
        if self.entries.iter().any(|entry| {
            entry.project_id == *project_id
                && entry.session_id == *session_id
                && entry.turn_id == *turn_id
        }) {
            return Ok(false);
        }
        if self.entries.len() == TURNS_MAX {
            let oldest = self
                .entries
                .iter()
                .position(|entry| entry.finished)
                .ok_or_else(|| failure("master_dispatch_capacity"))?;
            self.entries.remove(oldest);
        }
        let executor = tokio::runtime::Handle::try_current()
            .map_err(|_| failure("master_dispatch_runtime_unavailable"))?;
        let project = project_id.clone();
        let session = session_id.clone();
        let mut turn = turn_id.clone();
        let stopping = self.stopping.clone();
        let task = self.tasks.spawn_on(
            async move {
                let mut settled = Ok(());
                for _ in 0..TURNS_MAX {
                    let result = drive_until_settled(
                        || async {
                            runtime
                                .drive_master_turn(&project, &session, &turn)
                                .await
                                .map(|_| ())
                        },
                        DRIVE_PAUSE,
                    )
                    .await;
                    if let Err(error) = &result {
                        eprintln!("{}", super::master_turn_diagnostic(error));
                        if std::env::var_os("CRAZYAGENT_DEBUG_CUSTODIAN").is_some() {
                            eprintln!("master_turn_debug_cause={}", error.0.escape_debug());
                        }
                    }
                    settled = settled.and(result);
                    if stopping.load(Ordering::SeqCst) {
                        break;
                    }
                    match runtime.start_waiting_resolve(&project, &session).await {
                        Ok(Some(next)) => turn = next,
                        Ok(None) => break,
                        Err(error) => {
                            eprintln!("master_resolve_not_started cause={}", error.0);
                            settled = settled.and(Err(error));
                            break;
                        }
                    }
                }
                settled
            },
            &executor,
        );
        self.entries.push(Dispatch {
            project_id: project_id.clone(),
            session_id: session_id.clone(),
            turn_id: turn_id.clone(),
            task: task.id(),
            finished: false,
        });
        self.assert_consistent();
        Ok(true)
    }

    pub(super) async fn shutdown(&mut self) -> Result<(), EngineError> {
        self.assert_consistent();
        self.stopping.store(true, Ordering::SeqCst);
        let deadline = *self
            .closing
            .get_or_insert_with(|| Instant::now() + SHUTDOWN_GRACE);
        while !self.tasks.is_empty() {
            match timeout_at(deadline, self.tasks.join_next_with_id()).await {
                Ok(Some(joined)) => self.joined(joined),
                Ok(None) => break,
                Err(_) => {
                    self.uncertain = true;
                    self.tasks.abort_all();
                    while let Some(joined) = self.tasks.join_next_with_id().await {
                        self.joined(joined);
                    }
                }
            }
        }
        assert!(self.tasks.is_empty());
        assert!(self.entries.iter().all(|entry| entry.finished));
        self.assert_consistent();
        if self.uncertain {
            Err(failure("master_dispatch_cleanup_uncertain"))
        } else if let Some(error) = &self.first_error {
            Err(EngineError(error.clone()))
        } else {
            Ok(())
        }
    }

    fn reap(&mut self) {
        while let Some(joined) = self.tasks.try_join_next_with_id() {
            self.joined(joined);
        }
    }

    fn joined(&mut self, joined: Result<(Id, Result<(), EngineError>), JoinError>) {
        let (id, result) = match joined {
            Ok((id, result)) => (id, Ok(result)),
            Err(error) => (error.id(), Err(error)),
        };
        let entry = self
            .entries
            .iter_mut()
            .find(|entry| entry.task == id)
            .expect("every master driver task keeps its entry until it is joined");
        assert!(!entry.finished);
        entry.finished = true;
        match result {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                self.first_error.get_or_insert(error.0);
            }
            Err(_) => self.uncertain = true,
        }
    }

    fn assert_consistent(&self) {
        assert!(self.entries.len() <= TURNS_MAX);
        assert_eq!(
            self.entries.iter().filter(|entry| !entry.finished).count(),
            self.tasks.len()
        );
    }
}

/// Drives one master turn again under the same identity while the storage actor reports
/// an unknown outcome, which asks the caller to reconcile by repeating the same request.
/// Inputs: the drive step and the base pause, which grows linearly per attempt. Output:
/// the first settled result, or the unknown outcome after DRIVE_ATTEMPTS tries.
pub(super) async fn drive_until_settled<F, Fut>(
    mut drive: F,
    pause: Duration,
) -> Result<(), EngineError>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<(), EngineError>>,
{
    let mut attempt = 1;
    loop {
        let result = drive().await;
        match result {
            Err(error) if error.0 == UNKNOWN_OUTCOME && attempt < DRIVE_ATTEMPTS => {
                tokio::time::sleep(pause * attempt).await;
                attempt += 1;
            }
            settled => {
                assert!(attempt <= DRIVE_ATTEMPTS);
                return settled;
            }
        }
    }
}

fn validate_commit(
    runtime: &OwnerRuntimeHandle,
    request: &OwnerRequest,
    operation: &OwnerApplied,
) -> Result<(), EngineError> {
    let provenance = &operation.provenance;
    let authority = &runtime.authority;
    if !authority.usable.load(Ordering::SeqCst) {
        return Err(failure("master_dispatch_owner_unavailable"));
    }
    if provenance.origin != "native_owner"
        || provenance.engine_epoch != authority.host.engine_epoch
        || provenance.operation_id != request.request_id
        || provenance.request_hash != request.mutation_hash()?
        || provenance.revision_before != request.expected_revision
        || provenance.revision_after.value() <= provenance.revision_before.value()
        || operation.current_revision != provenance.revision_after
        || provenance.event_cursor.as_ref() != Some(&provenance.revision_after)
    {
        return Err(failure("master_dispatch_binding_mismatch"));
    }
    let slots = authority
        .slots
        .lock()
        .map_err(|_| failure("master_dispatch_owner_unavailable"))?;
    let admitted = slots.iter().any(|slot| {
        let admission = &slot.registration.admission;
        slot.registration.active
            && admission.db_instance == authority.host.db_instance
            && admission.engine_epoch == authority.host.engine_epoch
            && admission.owner_id == provenance.owner_id
            && admission.owner_epoch == provenance.admitted_epoch
            && admission.owner_epoch == provenance.resulting_epoch
    });
    if !admitted {
        return Err(failure("master_dispatch_binding_mismatch"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU32;

    #[tokio::test]
    async fn unknown_outcome_drives_the_same_turn_again_until_it_settles() {
        let calls = AtomicU32::new(0);
        let result = drive_until_settled(
            || async {
                if calls.fetch_add(1, Ordering::SeqCst) < 2 {
                    Err(failure("owner_outcome_unknown_reconcile_same_identity"))
                } else {
                    Ok(())
                }
            },
            Duration::from_millis(1),
        )
        .await;
        assert_eq!(result, Ok(()));
        assert_eq!(calls.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn other_failures_and_exhausted_retries_are_returned() {
        let calls = AtomicU32::new(0);
        let result = drive_until_settled(
            || async {
                calls.fetch_add(1, Ordering::SeqCst);
                Err::<(), _>(failure("master_repository_missing"))
            },
            Duration::from_millis(1),
        )
        .await;
        assert_eq!(result, Err(failure("master_repository_missing")));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        let calls = AtomicU32::new(0);
        let result = drive_until_settled(
            || async {
                calls.fetch_add(1, Ordering::SeqCst);
                Err::<(), _>(failure("owner_outcome_unknown_reconcile_same_identity"))
            },
            Duration::from_millis(1),
        )
        .await;
        assert_eq!(
            result,
            Err(failure("owner_outcome_unknown_reconcile_same_identity"))
        );
        assert_eq!(calls.load(Ordering::SeqCst), DRIVE_ATTEMPTS);
    }
}
