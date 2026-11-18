use super::*;
use crate::engine::owner::native_auth::Command;
use crate::storage::narrowing::{Change, Intent, IntentId, MAX_INTENTS};
use std::collections::VecDeque;

const DRAIN_FIRST_WAIT: Duration = Duration::from_millis(100);
const DRAIN_LONGEST_WAIT: Duration = Duration::from_secs(2);

/// A narrowing change whose submit storage refused while the engine runs, kept on disk and
/// queued for the one drainer, which submits the same job again until storage takes it. A
/// revocation carries its intent's name; storage ends a sign-out's intent by its request id.
#[derive(Clone)]
pub(super) enum Pending {
    Revoke {
        control: OwnerControl,
        id: IntentId,
    },
    Logout {
        admission: Admission,
        logout: NativeAuthLogout,
    },
}

/// The drainer's queue, in the order the changes were made, and its one task, which runs
/// while the queue holds a change and stops with the engine.
#[derive(Default)]
pub(super) struct Drain {
    pending: VecDeque<Pending>,
    task: Option<tokio::task::JoinHandle<()>>,
    running: bool,
}

/// Whether an answer means storage never took the job: its owner barrier or its queue
/// refused it, or storage withdrew it, so the drainer submits it again later. Input: the
/// answer. Output: the verdict.
fn unsubmitted<T>(answer: &Result<T, EngineError>) -> bool {
    answer.as_ref().is_err_and(|refused| {
        crate::storage::not_submitted(&refused.0) || refused.0 == "owner_control_not_submitted"
    })
}

impl OwnerRuntimeHandle {
    /// Keeps one narrowing change on disk before its job is submitted, so a lost job is
    /// applied later. Input: the change. Output: the intent's name, or none when it could not
    /// be kept, which is logged; the change is then answered as before this store existed.
    pub(super) async fn keep_narrowing(&self, change: Change) -> Option<IntentId> {
        let Some(intents) = self.engine.storage.intents.clone() else {
            eprintln!("narrowing_intent_unwritten cause=readonly");
            return None;
        };
        let intent = Intent {
            db_instance: self.authority.host.db_instance.clone(),
            change,
        };
        match tokio::task::spawn_blocking(move || intents.write(&intent)).await {
            Ok(Ok(id)) => Some(id),
            Ok(Err(refused)) => {
                eprintln!("narrowing_intent_unwritten cause={}", refused.0);
                None
            }
            Err(_) => {
                eprintln!("narrowing_intent_unwritten cause=writer_lost");
                None
            }
        }
    }

    /// Answers a kept narrowing change whose submit storage refused: it is queued for the
    /// drainer and pending, never 'nothing changed'. Inputs: the answer and the change, when
    /// kept. Output: the answer, `access_change_pending` for a refused kept change.
    pub(super) fn pending_unless_taken<T>(
        &self,
        answer: Result<T, EngineError>,
        pending: Option<Pending>,
    ) -> Result<T, EngineError> {
        match pending {
            Some(pending) if unsubmitted(&answer) => {
                self.drain_later(pending);
                Err(failure(crate::storage::ACCESS_CHANGE_PENDING))
            }
            _ => answer,
        }
    }

    /// Queues one refused narrowing change for the drainer and starts the drainer when none
    /// runs. A queue at `MAX_INTENTS` takes no more: the change stays on disk for the next
    /// open, which is logged. Input: the change. Output: none.
    fn drain_later(&self, pending: Pending) {
        let Ok(mut drain) = self.authority.drain.lock() else {
            eprintln!("narrowing_drain_unavailable");
            return;
        };
        if drain.pending.len() >= MAX_INTENTS {
            eprintln!("narrowing_drain_bounded pending={}", drain.pending.len());
            return;
        }
        drain.pending.push_back(pending);
        assert!(drain.pending.len() <= MAX_INTENTS);
        if drain.running {
            return;
        }
        drain.running = true;
        let handle = OwnerRuntimeHandle {
            engine: self.engine.clone(),
            authority: self.authority.clone(),
        };
        drain.task = Some(tokio::spawn(async move { handle.drain_all().await }));
    }

    /// The drainer: lands each queued change in turn, the oldest first, one at a time, and
    /// ends once the queue is empty. Output: none; it is stopped with the engine.
    async fn drain_all(self) {
        loop {
            let next = {
                let Ok(mut drain) = self.authority.drain.lock() else {
                    eprintln!("narrowing_drain_unavailable");
                    return;
                };
                match drain.pending.front().cloned() {
                    Some(next) => next,
                    None => {
                        drain.running = false;
                        return;
                    }
                }
            };
            self.land(next).await;
            if let Ok(mut drain) = self.authority.drain.lock() {
                drain.pending.pop_front();
            }
        }
    }

    /// Submits one change again until storage takes it, waiting 100 ms at first and twice as
    /// long each time, at most 2 s. Input: the change. Output: none once storage took it;
    /// what storage then does with its intent is storage's call.
    async fn land(&self, pending: Pending) {
        let mut wait = DRAIN_FIRST_WAIT;
        loop {
            let taken = match &pending {
                Pending::Revoke { control, id } => self.land_revocation(control, id).await,
                Pending::Logout { admission, logout } => {
                    let answer = self
                        .native_auth_submit(admission.clone(), Command::Logout(logout.clone()))
                        .await;
                    !unsubmitted(&answer)
                }
            };
            if taken {
                return;
            }
            tokio::time::sleep(wait).await;
            wait = (wait * 2).min(DRAIN_LONGEST_WAIT);
            assert!(wait <= DRAIN_LONGEST_WAIT);
        }
    }

    /// Submits one refused revocation again under the owner barrier, with its intent, so
    /// storage ends the intent once it commits. Inputs: the revocation and its intent.
    /// Output: whether storage took the job.
    async fn land_revocation(&self, control: &OwnerControl, id: &IntentId) -> bool {
        assert_eq!(control.decision, OwnerDecision::Revoke);
        let Ok(barrier) =
            tokio::time::timeout(self.engine.timeout, self.engine.owner_egress.write()).await
        else {
            return false;
        };
        let submitted = self.authority.next_control();
        let authority = self.authority.clone();
        let answer = self
            .engine
            .settled_owner_job(
                |reply| {
                    Job::OwnerControl(
                        self.authority.host.clone(),
                        control.clone(),
                        Some(id.clone()),
                        reply,
                    )
                },
                move |stored| {
                    let (result, registration) = stored?;
                    authority
                        .publish(registration, submitted)
                        .map_err(|_| failure("owner_control_committed_publication_unavailable"))?;
                    Ok(result)
                },
            )
            .await;
        drop(barrier);
        if let Err(refused) = &answer
            && !unsubmitted(&answer)
        {
            eprintln!("narrowing_drain_answered cause={}", refused.0);
        }
        !unsubmitted(&answer)
    }

    /// Stops the drainer with the engine: what it had not landed stays on disk for the next
    /// open, which is logged. Output: none once its task ended.
    pub(in crate::engine) async fn stop_narrowing_drain(&self) {
        let task = match self.authority.drain.lock() {
            Ok(mut drain) => {
                if !drain.pending.is_empty() {
                    eprintln!("narrowing_drain_stopped pending={}", drain.pending.len());
                }
                drain.running = false;
                drain.pending.clear();
                drain.task.take()
            }
            Err(_) => None,
        };
        if let Some(task) = task {
            task.abort();
            let _ = task.await;
        }
    }

    /// Stops the drainer without waiting, when the engine is dropped without a shutdown.
    /// Output: none.
    pub(in crate::engine) fn abort_narrowing_drain(&self) {
        if let Ok(mut drain) = self.authority.drain.lock()
            && let Some(task) = drain.task.take()
        {
            task.abort();
        }
    }
}
