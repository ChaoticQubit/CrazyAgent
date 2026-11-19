use super::*;
use crate::engine::owner::native_auth::Command;
use crate::protocol::CredentialRef;
use crate::storage::narrowing::{Change, Intent, IntentId, MAX_INTENTS, NarrowingAsk};
use std::collections::VecDeque;

const DRAIN_FIRST_WAIT: Duration = Duration::from_millis(100);
const DRAIN_LONGEST_WAIT: Duration = Duration::from_secs(2);

#[cfg(all(test, unix))]
static DRAIN_HELD: std::sync::Mutex<Vec<PathBuf>> = std::sync::Mutex::new(Vec::new());

/// Holds the drainer of the engine at `root` before it lands its next change, until the
/// returned guard drops, so a row orders an owner change before a drained one. Keyed by the
/// root, so the drainers of other rows run on. Input: the engine root. Output: the guard.
#[cfg(all(test, unix))]
pub(super) fn hold_drain(root: &Path) -> DrainHold {
    let mut held = DRAIN_HELD.lock().unwrap();
    assert!(!held.iter().any(|other| other == root));
    held.push(root.to_path_buf());
    DrainHold(root.to_path_buf())
}

/// A hold from [`hold_drain`]; dropping it lets that drainer go on.
#[cfg(all(test, unix))]
pub(super) struct DrainHold(PathBuf);

#[cfg(all(test, unix))]
impl Drop for DrainHold {
    fn drop(&mut self) {
        let mut held = DRAIN_HELD
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let before = held.len();
        held.retain(|other| *other != self.0);
        assert_eq!(held.len() + 1, before);
    }
}

/// Waits while a row holds the drainer of the engine at `root`, polling every 20 ms.
/// Input: the engine root. Output: none once no hold names it.
#[cfg(all(test, unix))]
async fn wait_while_drain_held(root: &Path) {
    while DRAIN_HELD
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .iter()
        .any(|other| other == root)
    {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// A narrowing change whose submit storage refused while the engine runs, kept on disk and
/// queued for the one drainer, which submits the same job again until storage takes it. Each
/// carries its intent's name: storage ends a revocation's, an account change's and a detach's
/// by it, and runs a sign-out or a detach only while its intent is kept, so a newer owner
/// sign-in or attach is never undone.
#[derive(Clone)]
pub(super) enum Pending {
    Revoke {
        control: OwnerControl,
        id: IntentId,
    },
    Logout {
        admission: Admission,
        logout: NativeAuthLogout,
        id: IntentId,
    },
    Account {
        admission: Admission,
        change: AccountChange,
        id: IntentId,
    },
    Secret {
        server: String,
        name: String,
        id: IntentId,
    },
}

/// The account change a kept removal or key replace asks storage for again: the account, and
/// for a key replace its key reference.
#[derive(Clone)]
pub(super) enum AccountChange {
    Remove(CredentialRef),
    KeyReplaced(CredentialRef, CredentialRef),
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

/// Whether storage took a drained job, logging a refusal it answered, which keeps the
/// intent for the next open. Input: the answer. Output: the verdict.
fn drain_taken<T>(answer: &Result<T, EngineError>) -> bool {
    if let Err(refused) = answer
        && !unsubmitted(answer)
    {
        eprintln!("narrowing_drain_answered cause={}", refused.0);
    }
    !unsubmitted(answer)
}

/// Whether an answer is a refusal the owner hears by name, so the kept sign-out it answers
/// ends: a native-auth refusal every client shows, or a busy profile the owner reads as
/// recovery required. Pending, outcome unknown and storage's own answers are not.
/// Input: the answer. Output: the verdict.
fn refused_by_name<T>(answer: &Result<T, EngineError>) -> bool {
    answer.as_ref().is_err_and(|refused| {
        refused.0 == "native_auth_host_busy"
            || (!crate::storage::storage_answer(&refused.0)
                && crate::engine::owner::native_auth::public_error(&refused.0))
    })
}

impl OwnerRuntimeHandle {
    /// Keeps one narrowing change on disk before its job is submitted, so a lost job is
    /// applied later. Input: the change. Output: the intent's name, or none when it could not
    /// be kept or no store exists (a read-only engine, a platform with no store), which is
    /// logged; the change is then answered as before this store existed.
    pub(super) async fn keep_narrowing(&self, change: Change) -> Option<IntentId> {
        let Some(intents) = self.engine.storage.intents.clone() else {
            eprintln!("narrowing_intent_unwritten cause=no_store");
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

    /// Ends a kept sign-out when the answer the owner is about to hear is storage's refusal by
    /// name, since the owner then knows nothing changed; any other answer keeps it for the
    /// drainer or the next open. Inputs: the intent and the answer. Output: none; a removal
    /// that fails is logged, and the sign-out then replays at the next open.
    pub(super) async fn end_refused_sign_out<T>(
        &self,
        id: &IntentId,
        answer: &Result<T, EngineError>,
    ) {
        if !refused_by_name(answer) {
            return;
        }
        self.end_kept(id).await;
    }

    /// Ends a kept narrowing change whose refusal the owner is about to hear by name, since
    /// the owner then knows nothing changed. Input: the intent. Output: none; a removal that
    /// fails is logged, and the change then replays at the next open.
    pub(super) async fn end_kept(&self, id: &IntentId) {
        let Some(intents) = self.engine.storage.intents.clone() else {
            return;
        };
        let name = id.as_str().to_owned();
        let id = id.clone();
        match tokio::task::spawn_blocking(move || intents.remove(&id)).await {
            Ok(Ok(())) => {}
            Ok(Err(left)) => eprintln!("narrowing_intent_unremoved id={name} cause={}", left.0),
            Err(_) => eprintln!("narrowing_intent_unremoved id={name} cause=remover_lost"),
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
            #[cfg(all(test, unix))]
            wait_while_drain_held(&self.authority.root.path).await;
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
                Pending::Logout {
                    admission,
                    logout,
                    id,
                } => {
                    let answer = self
                        .native_auth_submit(
                            admission.clone(),
                            Command::Logout(logout.clone()),
                            crate::storage::NativeAuthAsk::Drained(id.clone()),
                        )
                        .await;
                    drain_taken(&answer)
                }
                Pending::Account {
                    admission,
                    change,
                    id,
                } => {
                    let ask = Some(NarrowingAsk::Drained(id.clone()));
                    let command = match change {
                        AccountChange::Remove(account) => {
                            crate::storage::accounts::Command::Remove(account.clone(), ask)
                        }
                        AccountChange::KeyReplaced(account, credential) => {
                            crate::storage::accounts::Command::KeyReplaced(
                                account.clone(),
                                credential.clone(),
                                ask,
                            )
                        }
                    };
                    let answer = self
                        .engine
                        .native_auth_job(|reply| Job::Accounts(admission.clone(), command, reply))
                        .await;
                    drain_taken(&answer)
                }
                Pending::Secret { server, name, id } => {
                    let detach = crate::storage::plugins::Command::DetachSecret {
                        id: server.clone(),
                        name: name.clone(),
                        ask: Some(NarrowingAsk::Drained(id.clone())),
                    };
                    let answer = self
                        .engine
                        .settled_job(|reply| Job::Plugins(detach, reply), |_| Ok(()))
                        .await;
                    drain_taken(&answer)
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

    /// Whether the drainer has nothing left to land and no task running, for a row that
    /// waits for a drained change before it reads storage. Output: the verdict.
    #[cfg(all(test, unix))]
    pub(super) fn narrowing_drain_idle(&self) -> bool {
        self.authority
            .drain
            .lock()
            .is_ok_and(|drain| drain.pending.is_empty() && !drain.running)
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
