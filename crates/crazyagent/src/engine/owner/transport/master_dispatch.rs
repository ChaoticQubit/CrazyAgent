use super::{
    EngineError, OwnerApplied, OwnerMutation, OwnerOperation, OwnerReply, OwnerRequest,
    OwnerResponse, OwnerResult, OwnerRuntimeHandle, failure,
};
use crate::master_session::{MasterOwnerAction, TURNS_MAX};
use crate::protocol::{ProjectId, RequestId, SessionId};
use std::sync::atomic::Ordering;
use std::time::Duration;
use tokio::task::JoinHandle;
use tokio::time::{Instant, timeout_at};

const SHUTDOWN_GRACE: Duration = Duration::from_secs(1);
const DRIVE_ATTEMPTS: u32 = 6;
const DRIVE_PAUSE: Duration = Duration::from_millis(750);
pub(super) const UNKNOWN_OUTCOME: &str = "owner_outcome_unknown_reconcile_same_identity";

struct Dispatch {
    project_id: ProjectId,
    session_id: SessionId,
    turn_id: RequestId,
    task: Option<JoinHandle<Result<(), EngineError>>>,
}

pub(super) struct MasterDispatch {
    entries: Vec<Dispatch>,
    closing: Option<Instant>,
    uncertain: bool,
    first_error: Option<String>,
}

impl MasterDispatch {
    #[cfg(unix)]
    pub(super) fn new() -> Self {
        let dispatcher = Self {
            entries: Vec::new(),
            closing: None,
            uncertain: false,
            first_error: None,
        };
        dispatcher.assert_consistent();
        dispatcher
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
        if self.entries.iter().any(|entry| {
            entry.project_id == *project_id
                && entry.session_id == *session_id
                && entry.turn_id == request.request_id
        }) {
            return Ok(false);
        }
        if self.entries.len() == TURNS_MAX {
            return Err(failure("master_dispatch_capacity"));
        }
        let executor = tokio::runtime::Handle::try_current()
            .map_err(|_| failure("master_dispatch_runtime_unavailable"))?;
        let project = project_id.clone();
        let session = session_id.clone();
        let turn = request.request_id.clone();
        let task = executor.spawn(async move {
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
                eprintln!(
                    "master_turn_not_completed cause={}",
                    super::native_auth_dispatch_diagnostic(error)
                );
                if std::env::var_os("CRAZYAGENT_DEBUG_CUSTODIAN").is_some() {
                    eprintln!("master_turn_debug_cause={}", error.0.escape_debug());
                }
            }
            result
        });
        self.entries.push(Dispatch {
            project_id: project_id.clone(),
            session_id: session_id.clone(),
            turn_id: request.request_id.clone(),
            task: Some(task),
        });
        self.assert_consistent();
        Ok(true)
    }

    pub(super) async fn shutdown(&mut self) -> Result<(), EngineError> {
        self.assert_consistent();
        let deadline = *self
            .closing
            .get_or_insert_with(|| Instant::now() + SHUTDOWN_GRACE);
        for index in 0..self.entries.len() {
            let Some(task) = self.entries[index].task.as_mut() else {
                continue;
            };
            match timeout_at(deadline, task).await {
                Ok(result) => {
                    drop(self.entries[index].task.take());
                    self.record_join(result);
                }
                Err(_) => {
                    self.uncertain = true;
                    break;
                }
            }
        }
        if self.uncertain {
            self.abort_remaining();
            self.join_remaining().await;
        }
        assert!(self.entries.iter().all(|entry| entry.task.is_none()));
        self.assert_consistent();
        if self.uncertain {
            Err(failure("master_dispatch_cleanup_uncertain"))
        } else if let Some(error) = &self.first_error {
            Err(EngineError(error.clone()))
        } else {
            Ok(())
        }
    }

    fn record_join(&mut self, result: Result<Result<(), EngineError>, tokio::task::JoinError>) {
        assert!(self.closing.is_some());
        match result {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                self.first_error.get_or_insert(error.0);
            }
            Err(_) => self.uncertain = true,
        }
    }

    fn abort_remaining(&self) {
        for entry in &self.entries {
            if let Some(task) = &entry.task {
                task.abort();
            }
        }
    }

    async fn join_remaining(&mut self) {
        assert!(self.closing.is_some());
        assert!(self.uncertain);
        for index in 0..self.entries.len() {
            let Some(task) = self.entries[index].task.as_mut() else {
                continue;
            };
            let result = task.await;
            drop(self.entries[index].task.take());
            self.record_join(result);
        }
        assert!(self.entries.iter().all(|entry| entry.task.is_none()));
    }

    fn assert_consistent(&self) {
        assert!(self.entries.len() <= TURNS_MAX);
        if self.closing.is_none() {
            assert!(!self.uncertain && self.first_error.is_none());
            assert!(self.entries.iter().all(|entry| entry.task.is_some()));
        }
    }
}

impl Drop for MasterDispatch {
    fn drop(&mut self) {
        self.abort_remaining();
    }
}

/// Drives one master turn again under the same identity while the storage actor reports
/// an unknown outcome, which asks the caller to reconcile by repeating the same request.
/// Inputs: the drive step and the base pause, which grows linearly per attempt. Output:
/// the first settled result, or the unknown outcome after DRIVE_ATTEMPTS tries.
async fn drive_until_settled<F, Fut>(mut drive: F, pause: Duration) -> Result<(), EngineError>
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
