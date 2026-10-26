use super::*;

#[path = "../../native_auth_host/claude_auth/channel.rs"]
pub(crate) mod claude;

#[cfg(all(test, unix))]
#[path = "native_auth_channel/rcli_registry_tests.rs"]
mod rcli_registry_tests;
#[path = "native_auth_channel/reproof.rs"]
mod reproof;
#[cfg(unix)]
use crate::engine::bootstrap::NativeProfileUse;
use crate::engine::bootstrap::runner_binding;
#[cfg(unix)]
use crate::engine::bootstrap::{NativeBinding, NativeProfileUsePurpose};
use crate::engine::native_auth_host::RegisteredNativeAuthHost;
#[cfg(unix)]
use crate::engine::native_auth_host::custody::Retirement;
use crate::engine::native_auth_host::custody::cleanup_direct_child;
use crate::engine::owner::native_auth::{CodexAuthObservation, apply_codex_presentation};
use crate::storage::{NativeAuthDispatch, NativeAuthWork};
use serde::{Deserialize, Serialize};
use std::future::Future;
use std::pin::Pin;
use std::process::Child;
#[cfg(unix)]
use std::process::ExitStatus;
use std::sync::Weak;
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::Poll;

const READ_BOUND: Duration = Duration::from_secs(10);
const CLEANUP_BOUND: Duration = Duration::from_secs(2);

fn operation_bound(fact: &NativeAuthFact) -> Duration {
    match fact.action {
        NativeAuthAction::SignIn { .. } => Duration::from_secs(600),
        NativeAuthAction::Logout => READ_BOUND,
    }
}

#[cfg(unix)]
fn purpose(fact: &NativeAuthFact) -> NativeProfileUsePurpose {
    match fact.action {
        NativeAuthAction::SignIn { .. } => NativeProfileUsePurpose::Authentication,
        NativeAuthAction::Logout => NativeProfileUsePurpose::Logout,
    }
}

#[cfg(unix)]
fn fact_host(fact: &NativeAuthFact) -> HostStamp {
    HostStamp {
        db_instance: fact.binding.database_instance.clone(),
        engine_epoch: fact.binding.engine_epoch.clone(),
    }
}

pub(super) struct NativeAuthSlot {
    pub(super) token: [u8; 32],
    fact: NativeAuthFact,
    admission: Admission,
    dispatch: Option<Arc<NativeAuthDispatch>>,
    attached: bool,
    live: Arc<AtomicBool>,
    deadline: Instant,
    expired: bool,
    supervisor: Option<tokio::task::AbortHandle>,
    pub(super) dispatch_task: Option<tokio::task::JoinHandle<Result<(), EngineError>>>,
    pub(super) child: Option<Child>,
    retained: bool,
    cleaning: bool,
    committed: bool,
    progress: Option<CodexProgress>,
    claude_progress: Option<claude::ClaudeProgress>,
}

struct CodexProgress {
    fact: NativeAuthFact,
    login_id: Option<String>,
    request_id: i64,
    challenge: Option<crate::adapters::codex::LoginChallenge>,
    account: Option<crate::adapters::codex::AuthAccount>,
}

impl NativeAuthSlot {
    fn cached(&self) -> bool {
        self.committed
            && (self.progress.is_some() || self.claude_progress.is_some())
            && self.dispatch_task.is_none()
            && self.child.is_none()
            && !self.cleaning
    }
}

#[derive(Default)]
pub(super) struct NativeAuthHostRegistration {
    executable: Option<Arc<RegisteredNativeAuthHost>>,
    codex_executable: Option<Arc<RegisteredNativeAuthHost>>,
    claude_executable: Option<Arc<RegisteredNativeAuthHost>>,
    rcli_master_executable: Option<Arc<RegisteredNativeAuthHost>>,
    rcli_private_key_executable: Option<Arc<RegisteredNativeAuthHost>>,
    frozen: bool,
    disabled: bool,
}

pub(crate) struct NativeAuthDispatchHost {
    #[cfg(unix)]
    engine: Arc<Engine>,
    #[cfg(unix)]
    authority: Weak<Authority>,
    #[cfg(unix)]
    fact: NativeAuthFact,
    #[cfg(unix)]
    admission: Admission,
}

#[cfg(unix)]
impl NativeAuthDispatchHost {
    fn authority(&self) -> Result<Arc<Authority>, EngineError> {
        let authority = self
            .authority
            .upgrade()
            .ok_or_else(|| failure("owner_runtime_stale"))?;
        authority.verify_native_auth_admission(&self.admission, &self.fact)?;
        {
            let current = authority
                .native_auth
                .lock()
                .map_err(|_| failure("native_auth_recovery_required"))?;
            let slot = current
                .as_ref()
                .ok_or_else(|| failure("native_auth_recovery_required"))?;
            if slot.fact != self.fact || slot.admission != self.admission || slot.expired {
                return Err(failure("native_auth_recovery_required"));
            }
        }
        Ok(authority)
    }

    pub(crate) fn owner_runtime_dir(&self) -> Result<PathBuf, EngineError> {
        self.authority()?.native_auth_owner_runtime_dir()
    }

    pub(crate) fn registered_executable(
        &self,
    ) -> Result<Arc<RegisteredNativeAuthHost>, EngineError> {
        let authority = self.authority()?;
        let executable = authority
            .native_auth_host
            .lock()
            .map_err(|_| failure("native_auth_recovery_required"))?
            .executable
            .clone()
            .ok_or_else(|| failure("native_auth_host_unavailable"))?;
        executable.verified_path()?;
        Ok(executable)
    }

    pub(crate) fn registered_codex_executable(
        &self,
    ) -> Result<Option<Arc<RegisteredNativeAuthHost>>, EngineError> {
        let authority = self.authority()?;
        let executable = {
            let registration = authority
                .native_auth_host
                .lock()
                .map_err(|_| failure("native_auth_recovery_required"))?;
            if registration.disabled {
                return Err(failure("native_auth_host_unavailable"));
            }
            registration.codex_executable.clone()
        };
        if let Some(executable) = executable.as_ref() {
            executable.verified_path()?;
        }
        Ok(executable)
    }

    pub(crate) fn registered_claude_executable(
        &self,
    ) -> Result<Option<Arc<RegisteredNativeAuthHost>>, EngineError> {
        let authority = self.authority()?;
        let executable = {
            let registration = authority
                .native_auth_host
                .lock()
                .map_err(|_| failure("native_auth_recovery_required"))?;
            if registration.disabled {
                return Err(failure("native_auth_host_unavailable"));
            }
            registration.claude_executable.clone()
        };
        if let Some(executable) = executable.as_ref() {
            executable.verified_path()?;
        }
        Ok(executable)
    }

    pub(crate) async fn prepare_native_auth_custody(
        &self,
        work: NativeAuthWork,
    ) -> Result<NativeAuthCustodyLaunch, EngineError> {
        if work.fact() != &self.fact {
            return Err(failure("native_auth_recovery_required"));
        }
        drop(self.authority()?);
        let dispatch = self
            .engine
            .native_auth_job(|reply| Job::NativeAuthDispatch(Box::new(work), reply))
            .await?;
        self.authority()?.reserve_native_auth(dispatch, true)
    }

    pub(crate) fn retain_child(&self, child: Child) -> Result<(), (EngineError, Child)> {
        let authority = match self.authority() {
            Ok(authority) => authority,
            Err(error) => return Err((error, child)),
        };
        let mut current = match authority.native_auth.lock() {
            Ok(current) => current,
            Err(_) => return Err((failure("native_auth_recovery_required"), child)),
        };
        let Some(slot) = current.as_mut() else {
            return Err((failure("native_auth_recovery_required"), child));
        };
        if slot.fact != self.fact
            || slot.admission != self.admission
            || slot.expired
            || slot.dispatch.is_none()
            || slot.child.is_some()
            || slot.cleaning
        {
            return Err((failure("native_auth_recovery_required"), child));
        }
        slot.child = Some(child);
        assert!(slot.child.is_some() && slot.dispatch_task.is_some());
        Ok(())
    }

    pub(crate) fn try_wait_child(&self) -> Result<Option<ExitStatus>, EngineError> {
        let authority = self
            .authority
            .upgrade()
            .ok_or_else(|| failure("owner_runtime_stale"))?;
        let mut current = authority
            .native_auth
            .lock()
            .map_err(|_| failure("native_auth_recovery_required"))?;
        let slot = current
            .as_mut()
            .ok_or_else(|| failure("native_auth_recovery_required"))?;
        if slot.fact != self.fact || slot.admission != self.admission || slot.cleaning {
            return Err(failure("native_auth_recovery_required"));
        }
        slot.child
            .as_mut()
            .ok_or_else(|| failure("native_auth_recovery_required"))?
            .try_wait()
            .map_err(io_error)
    }
}

impl NativeAuthSlot {
    #[cfg(unix)]
    pub(super) fn session_is_live(&self) -> bool {
        self.live.load(Ordering::SeqCst)
    }
}

#[cfg(unix)]
pub(crate) struct NativeAuthCustodyLaunch {
    token: [u8; 32],
    authority: Weak<Authority>,
    delivered: bool,
}

#[cfg(unix)]
impl NativeAuthCustodyLaunch {
    pub(crate) fn write_bootstrap(
        mut self,
        mut input: std::process::ChildStdin,
    ) -> Result<(), EngineError> {
        use std::io::Write;
        self.delivered = true;
        input.write_all(&self.token).map_err(io_error)?;
        drop(input);
        Ok(())
    }
}

#[cfg(unix)]
impl Drop for NativeAuthCustodyLaunch {
    fn drop(&mut self) {
        if !self.delivered
            && let Some(authority) = self.authority.upgrade()
            && let Ok(mut slot) = authority.native_auth.lock()
            && let Some(slot) = slot.as_mut()
            && !slot.attached
            && bool::from(slot.token.ct_eq(&self.token))
        {
            slot.attached = true;
            slot.live.store(false, Ordering::SeqCst);
        }
        self.token.fill(0);
    }
}

#[cfg(unix)]
pub(crate) enum NativeAuthCustodyControl {
    Continue,
    Cancel,
}

pub(crate) struct NativeAuthCustodyRetirement {
    dispatch: Arc<NativeAuthDispatch>,
    live: Arc<AtomicBool>,
    after_child_reaped: bool,
}

impl NativeAuthCustodyRetirement {
    pub(crate) fn after_child_reaped(&self) -> bool {
        self.after_child_reaped
    }
    pub(crate) fn dispatch(&self) -> &NativeAuthDispatch {
        &self.dispatch
    }
    pub(crate) fn session_is_live(&self) -> bool {
        self.live.load(Ordering::SeqCst)
    }
    pub(crate) fn validate_session(&self) -> Result<(), EngineError> {
        if !self.session_is_live() {
            return Err(failure("native_auth_recovery_required"));
        }
        Ok(())
    }
}

pub(crate) struct NativeAuthCustodyObservation {
    dispatch: Arc<NativeAuthDispatch>,
    live: Arc<AtomicBool>,
    event: claude::CustodyEvent,
}

impl NativeAuthCustodyObservation {
    pub(crate) fn dispatch(&self) -> &NativeAuthDispatch {
        &self.dispatch
    }

    pub(crate) fn validate_session(&self) -> Result<(), EngineError> {
        if !self.live.load(Ordering::SeqCst) {
            return Err(failure("native_auth_recovery_required"));
        }
        Ok(())
    }
}

impl OwnerRuntimeHandle {
    pub(crate) fn register_native_auth_host(
        &self,
        host: RegisteredNativeAuthHost,
    ) -> Result<(), EngineError> {
        if !self.authority.usable.load(Ordering::SeqCst) {
            return Err(failure("owner_runtime_stale"));
        }
        self.native_auth_owner_runtime_dir()?;
        host.verified_path()?;
        let mut registration = self
            .authority
            .native_auth_host
            .lock()
            .map_err(|_| failure("native_auth_recovery_required"))?;
        if registration.frozen || registration.executable.is_some() {
            return Err(failure("native_auth_host_registration_frozen"));
        }
        registration.executable = Some(Arc::new(host));
        Ok(())
    }

    pub(crate) fn register_codex_executable(
        &self,
        host: RegisteredNativeAuthHost,
    ) -> Result<(), EngineError> {
        if !self.authority.usable.load(Ordering::SeqCst) {
            return Err(failure("owner_runtime_stale"));
        }
        self.native_auth_owner_runtime_dir()?;
        host.verified_path()?;
        let mut registration = self
            .authority
            .native_auth_host
            .lock()
            .map_err(|_| failure("native_auth_recovery_required"))?;
        if registration.frozen || registration.disabled || registration.codex_executable.is_some() {
            return Err(failure("native_auth_host_registration_frozen"));
        }
        registration.codex_executable = Some(Arc::new(host));
        Ok(())
    }

    pub(crate) fn register_claude_executable(
        &self,
        host: RegisteredNativeAuthHost,
    ) -> Result<(), EngineError> {
        if !self.authority.usable.load(Ordering::SeqCst) {
            return Err(failure("owner_runtime_stale"));
        }
        self.native_auth_owner_runtime_dir()?;
        host.verified_path()?;
        let mut registration = self
            .authority
            .native_auth_host
            .lock()
            .map_err(|_| failure("native_auth_recovery_required"))?;
        if registration.frozen || registration.disabled || registration.claude_executable.is_some()
        {
            return Err(failure("native_auth_host_registration_frozen"));
        }
        registration.claude_executable = Some(Arc::new(host));
        Ok(())
    }

    pub(crate) fn register_rcli_master_executable(
        &self,
        host: RegisteredNativeAuthHost,
    ) -> Result<(), EngineError> {
        if !self.authority.usable.load(Ordering::SeqCst) {
            return Err(failure("owner_runtime_stale"));
        }
        self.native_auth_owner_runtime_dir()?;
        host.verified_path()?;
        let mut registration = self
            .authority
            .native_auth_host
            .lock()
            .map_err(|_| failure("native_auth_recovery_required"))?;
        if registration.frozen
            || registration.disabled
            || registration.rcli_master_executable.is_some()
        {
            return Err(failure("native_auth_host_registration_frozen"));
        }
        registration.rcli_master_executable = Some(Arc::new(host));
        Ok(())
    }

    pub(crate) fn register_rcli_private_key_executable(
        &self,
        host: RegisteredNativeAuthHost,
    ) -> Result<(), EngineError> {
        if !self.authority.usable.load(Ordering::SeqCst) {
            return Err(failure("owner_runtime_stale"));
        }
        self.native_auth_owner_runtime_dir()?;
        host.verified_path()?;
        let mut registration = self
            .authority
            .native_auth_host
            .lock()
            .map_err(|_| failure("native_auth_recovery_required"))?;
        if registration.frozen
            || registration.disabled
            || registration.rcli_private_key_executable.is_some()
        {
            return Err(failure("native_auth_host_registration_frozen"));
        }
        registration.rcli_private_key_executable = Some(Arc::new(host));
        Ok(())
    }

    pub(crate) fn registered_codex_executable(
        &self,
    ) -> Result<Arc<RegisteredNativeAuthHost>, EngineError> {
        if !self.authority.usable.load(Ordering::SeqCst) {
            return Err(failure("owner_runtime_stale"));
        }
        self.native_auth_owner_runtime_dir()?;
        let executable = {
            let registration = self
                .authority
                .native_auth_host
                .lock()
                .map_err(|_| failure("native_auth_recovery_required"))?;
            if registration.disabled {
                return Err(failure("native_auth_host_unavailable"));
            }
            registration
                .codex_executable
                .clone()
                .ok_or_else(|| failure("native_auth_host_unavailable"))?
        };
        executable.verified_path()?;
        Ok(executable)
    }

    pub(crate) fn registered_claude_executable(
        &self,
    ) -> Result<Arc<RegisteredNativeAuthHost>, EngineError> {
        if !self.authority.usable.load(Ordering::SeqCst) {
            return Err(failure("owner_runtime_stale"));
        }
        self.native_auth_owner_runtime_dir()?;
        let executable = {
            let registration = self
                .authority
                .native_auth_host
                .lock()
                .map_err(|_| failure("native_auth_recovery_required"))?;
            if registration.disabled {
                return Err(failure("native_auth_host_unavailable"));
            }
            registration
                .claude_executable
                .clone()
                .ok_or_else(|| failure("native_auth_host_unavailable"))?
        };
        executable.verified_path()?;
        Ok(executable)
    }

    pub(crate) fn registered_rcli_master_executable(
        &self,
    ) -> Result<Arc<RegisteredNativeAuthHost>, EngineError> {
        if !self.authority.usable.load(Ordering::SeqCst) {
            return Err(failure("owner_runtime_stale"));
        }
        self.native_auth_owner_runtime_dir()?;
        let executable = {
            let registration = self
                .authority
                .native_auth_host
                .lock()
                .map_err(|_| failure("native_auth_recovery_required"))?;
            if registration.disabled {
                return Err(failure("native_auth_host_unavailable"));
            }
            registration
                .rcli_master_executable
                .clone()
                .ok_or_else(|| failure("native_auth_host_unavailable"))?
        };
        executable.verified_path()?;
        Ok(executable)
    }

    pub(crate) fn native_auth_owner_runtime_dir(&self) -> Result<PathBuf, EngineError> {
        self.authority.native_auth_owner_runtime_dir()
    }

    pub(crate) fn registered_rcli_private_key_executable(
        &self,
    ) -> Result<Arc<RegisteredNativeAuthHost>, EngineError> {
        if !self.authority.usable.load(Ordering::SeqCst) {
            return Err(failure("owner_runtime_stale"));
        }
        self.native_auth_owner_runtime_dir()?;
        let executable = {
            let registration = self
                .authority
                .native_auth_host
                .lock()
                .map_err(|_| failure("native_auth_recovery_required"))?;
            if registration.disabled {
                return Err(failure("native_auth_host_unavailable"));
            }
            registration
                .rcli_private_key_executable
                .clone()
                .ok_or_else(|| failure("native_auth_host_unavailable"))?
        };
        executable.verified_path()?;
        Ok(executable)
    }

    pub(crate) fn retain_native_auth_dispatch(
        &self,
        admission: Admission,
        work: NativeAuthWork,
    ) -> Result<(), (EngineError, Box<NativeAuthWork>)> {
        let runtime = match tokio::runtime::Handle::try_current() {
            Ok(runtime) => runtime,
            Err(_) => return Err((failure("native_auth_host_unavailable"), Box::new(work))),
        };
        let mut registration = match self.authority.native_auth_host.lock() {
            Ok(registration) => registration,
            Err(_) => return Err((failure("native_auth_recovery_required"), Box::new(work))),
        };
        registration.frozen = true;
        if registration.disabled || registration.executable.is_none() {
            return Err((failure("native_auth_host_unavailable"), Box::new(work)));
        }
        if let Err(error) = self
            .authority
            .verify_native_auth_admission(&admission, work.fact())
        {
            return Err((error, Box::new(work)));
        }
        let mut current = match self.authority.native_auth.lock() {
            Ok(current) => current,
            Err(_) => return Err((failure("native_auth_recovery_required"), Box::new(work))),
        };
        if current.as_ref().is_some_and(NativeAuthSlot::cached) {
            current.take();
        }
        if current.is_some() {
            return Err((failure("native_auth_host_busy"), Box::new(work)));
        }
        let fact = work.fact().clone();
        let host = NativeAuthDispatchHost {
            #[cfg(unix)]
            engine: self.engine.clone(),
            #[cfg(unix)]
            authority: Arc::downgrade(&self.authority),
            #[cfg(unix)]
            fact: fact.clone(),
            #[cfg(unix)]
            admission: admission.clone(),
        };
        *current = Some(NativeAuthSlot {
            token: [0; 32],
            deadline: Instant::now() + operation_bound(&fact),
            fact,
            admission,
            dispatch: None,
            attached: true,
            live: Arc::new(AtomicBool::new(false)),
            expired: false,
            supervisor: None,
            dispatch_task: Some(
                runtime.spawn(crate::engine::native_auth_host::dispatch(work, host)),
            ),
            child: None,
            retained: true,
            cleaning: false,
            committed: false,
            progress: None,
            claude_progress: None,
        });
        assert!(current.as_ref().is_some_and(|slot| {
            slot.dispatch.is_none() && slot.child.is_none() && slot.dispatch_task.is_some()
        }));
        drop(current);
        drop(registration);
        self.authority.native_auth_changed.notify_one();
        Ok(())
    }

    pub(crate) fn invalidate_native_auth_dispatch(&self) -> Result<(), EngineError> {
        let mut registration = self
            .authority
            .native_auth_host
            .lock()
            .map_err(|_| failure("native_auth_recovery_required"))?;
        registration.disabled = true;
        registration.frozen = true;
        self.authority.invalidate_native_auth_dispatch(None)?;
        drop(registration);
        self.authority.cleanup_native_auth_child()
    }

    pub(crate) async fn drain_native_auth_dispatch(&self) -> Result<(), EngineError> {
        self.authority
            .drain_native_auth_dispatch(&self.engine)
            .await
    }

    #[cfg(all(test, unix))]
    pub(crate) async fn prepare_native_auth_custody(
        &self,
        work: NativeAuthWork,
    ) -> Result<NativeAuthCustodyLaunch, EngineError> {
        self.authority
            .native_auth_host
            .lock()
            .map_err(|_| failure("native_auth_recovery_required"))?
            .frozen = true;
        let dispatch = self
            .engine
            .native_auth_job(|reply| Job::NativeAuthDispatch(Box::new(work), reply))
            .await?;
        self.authority.reserve_native_auth(dispatch, false)
    }

    pub(super) async fn native_auth_command(
        &self,
        admission: Admission,
        command: crate::engine::owner::native_auth::Command,
    ) -> Result<NativeAuthReply, EngineError> {
        command.validate()?;
        if matches!(
            &command,
            crate::engine::owner::native_auth::Command::Start(_)
                | crate::engine::owner::native_auth::Command::Logout(_)
        ) {
            self.authority
                .native_auth_host
                .lock()
                .map_err(|_| failure("native_auth_recovery_required"))?
                .frozen = true;
        }
        let begun = self
            .engine
            .native_auth_job(|reply| Job::NativeAuth(admission.clone(), command, reply))
            .await?;
        match begun.work {
            None => {
                let mut reply = begun.reply;
                self.authority.native_auth_presentation(&mut reply)?;
                Ok(reply)
            }
            Some(work) => match self.retain_native_auth_dispatch(admission.clone(), work) {
                Ok(()) => Ok(begun.reply),
                Err((_error, work)) => self
                    .engine
                    .native_auth_job(|reply| Job::NativeAuthFinish(admission, work, reply))
                    .await
                    .map_err(|error| {
                        if error.0 == "owner_queue_not_submitted" {
                            crate::engine::owner::native_auth::outcome_unknown()
                        } else {
                            error
                        }
                    }),
            },
        }
    }
}

impl Authority {
    fn native_auth_presentation(&self, reply: &mut NativeAuthReply) -> Result<(), EngineError> {
        let current = self
            .native_auth
            .lock()
            .map_err(|_| failure("native_auth_recovery_required"))?;
        let Some(slot) = current.as_ref().filter(|slot| !slot.expired) else {
            return Ok(());
        };
        if self.claude_presentation(slot, reply)? {
            return Ok(());
        }
        let Some(progress) = slot
            .progress
            .as_ref()
            .filter(|progress| progress.fact == reply.fact)
        else {
            return Ok(());
        };
        self.verify_native_auth_origin(
            slot.dispatch
                .as_ref()
                .ok_or_else(|| failure("native_auth_recovery_required"))?,
        )?;
        apply_codex_presentation(reply, progress.challenge.clone(), progress.account.clone())
    }

    fn native_auth_owner_runtime_dir(&self) -> Result<PathBuf, EngineError> {
        if !self.usable.load(Ordering::SeqCst) {
            return Err(failure("owner_runtime_stale"));
        }
        self.root.verify()?;
        let parent = self
            .root
            .path
            .parent()
            .ok_or_else(|| failure("owner_runtime_stale"))?;
        let (generation, identity, binding) = runner_binding(parent)?;
        if generation != self.root.path
            || identity.database_instance() != self.host.db_instance
            || identity.engine_epoch() != self.host.engine_epoch
        {
            return Err(failure("owner_runtime_stale"));
        }
        binding.verify()?;
        Ok(parent.to_path_buf())
    }

    pub(super) fn can_start_native_auth(&self, packet: &[u8; 37]) -> Result<(), EngineError> {
        if &packet[..4] != b"CAN1" || packet[4] != 0 {
            return Err(failure("native_auth_authentication_failed"));
        }
        let current = self
            .native_auth
            .lock()
            .map_err(|_| failure("native_auth_recovery_required"))?;
        let slot = current
            .as_ref()
            .ok_or_else(|| failure("native_auth_authentication_failed"))?;
        if slot.attached
            || slot.expired
            || Instant::now() >= slot.deadline
            || !bool::from(slot.token.ct_eq(&packet[5..]))
        {
            return Err(failure("native_auth_authentication_failed"));
        }
        self.verify_native_auth_origin(
            slot.dispatch
                .as_deref()
                .ok_or_else(|| failure("native_auth_authentication_failed"))?,
        )
    }

    pub(super) fn track_native_auth(&self, packet: &[u8; 37], task: tokio::task::AbortHandle) {
        if let Ok(mut current) = self.native_auth.lock()
            && let Some(slot) = current.as_mut()
            && bool::from(slot.token.ct_eq(&packet[5..]))
            && slot.supervisor.is_none()
        {
            slot.supervisor = Some(task);
        } else {
            task.abort();
        }
    }

    pub(super) fn native_auth_deadline(&self) -> Option<Instant> {
        self.native_auth.lock().ok().and_then(|current| {
            current
                .as_ref()
                .filter(|slot| !slot.expired && !slot.cached())
                .map(|slot| slot.deadline)
        })
    }

    pub(super) async fn expire_native_auth(&self, engine: &Engine) {
        let dispatch = {
            let Ok(mut current) = self.native_auth.lock() else {
                return;
            };
            let Some(slot) = current.as_mut() else {
                return;
            };
            if slot.expired || Instant::now() < slot.deadline {
                return;
            }
            slot.expired = true;
            slot.attached = true;
            slot.live.store(false, Ordering::SeqCst);
            slot.dispatch.clone()
        };
        if self.invalidate_native_auth_dispatch(None).is_err() {
            eprintln!("native_auth_dispatch_cleanup_uncertain");
        }
        if let Some(dispatch) = dispatch {
            let _ = engine
                .native_auth_job(|reply| Job::NativeAuthCustodyLost(dispatch, reply))
                .await;
        }
    }

    fn verify_native_auth_origin(&self, dispatch: &NativeAuthDispatch) -> Result<(), EngineError> {
        self.verify_native_auth_admission(dispatch.admission(), dispatch.fact())
    }

    fn verify_native_auth_admission(
        &self,
        admission: &Admission,
        fact: &NativeAuthFact,
    ) -> Result<(), EngineError> {
        if !self.usable.load(Ordering::SeqCst)
            || self.host.db_instance != fact.binding.database_instance
            || self.host.engine_epoch != fact.binding.engine_epoch
        {
            return Err(failure("owner_runtime_stale"));
        }
        let slots = self
            .slots
            .lock()
            .map_err(|_| failure("owner_authority_unavailable"))?;
        let (index, slot) = slots
            .iter()
            .enumerate()
            .find(|(_, slot)| slot.registration.active && &slot.registration.admission == admission)
            .ok_or_else(|| failure("owner_grant_stale_or_revoked"))?;
        self.root
            .verify_key(KEYS[index], &slot.file, slot.identity)?;
        let mut actual = [0; 32];
        slot.file.read_exact_at(&mut actual, 0).map_err(io_error)?;
        if !bool::from(slot.token.ct_eq(&actual)) || self.root.socket_identity()? != self.socket {
            return Err(failure("owner_capability_content_changed"));
        }
        Ok(())
    }

    #[cfg(unix)]
    fn reserve_native_auth(
        self: &Arc<Self>,
        dispatch: NativeAuthDispatch,
        preparing: bool,
    ) -> Result<NativeAuthCustodyLaunch, EngineError> {
        self.verify_native_auth_origin(&dispatch)?;
        let mut current = self
            .native_auth
            .lock()
            .map_err(|_| failure("native_auth_recovery_required"))?;
        if !preparing && current.as_ref().is_some_and(NativeAuthSlot::cached) {
            current.take();
        }
        if preparing {
            let slot = current
                .as_mut()
                .ok_or_else(|| failure("native_auth_recovery_required"))?;
            if slot.expired
                || slot.fact != *dispatch.fact()
                || &slot.admission != dispatch.admission()
                || slot.dispatch.is_some()
                || slot.dispatch_task.is_none()
            {
                return Err(failure("native_auth_recovery_required"));
            }
        } else if current.is_some() {
            return Err(failure("native_auth_host_busy"));
        }
        let mut token = [0; 32];
        getrandom::fill(&mut token).map_err(|_| failure("owner_entropy_unavailable"))?;
        if let Some(slot) = current.as_mut() {
            slot.token = token;
            slot.dispatch = Some(Arc::new(dispatch));
            slot.attached = false;
        } else {
            *current = Some(NativeAuthSlot {
                token,
                fact: dispatch.fact().clone(),
                admission: dispatch.admission().clone(),
                deadline: Instant::now() + operation_bound(dispatch.fact()),
                dispatch: Some(Arc::new(dispatch)),
                attached: false,
                live: Arc::new(AtomicBool::new(false)),
                expired: false,
                supervisor: None,
                dispatch_task: None,
                child: None,
                retained: false,
                cleaning: false,
                committed: false,
                progress: None,
                claude_progress: None,
            });
        }
        assert!(
            current
                .as_ref()
                .is_some_and(|slot| !slot.attached && !slot.session_is_live())
        );
        Ok(NativeAuthCustodyLaunch {
            token,
            authority: Arc::downgrade(self),
            delivered: false,
        })
    }

    fn attach_native_auth(&self, packet: &[u8; 37]) -> Result<Session, EngineError> {
        if &packet[..4] != b"CAN1" || packet[4] != 0 {
            return Err(failure("native_auth_authentication_failed"));
        }
        let mut current = self
            .native_auth
            .lock()
            .map_err(|_| failure("native_auth_recovery_required"))?;
        let slot = current
            .as_mut()
            .ok_or_else(|| failure("native_auth_authentication_failed"))?;
        if slot.attached
            || slot.expired
            || Instant::now() >= slot.deadline
            || !bool::from(slot.token.ct_eq(&packet[5..]))
        {
            return Err(failure("native_auth_authentication_failed"));
        }
        let dispatch = slot
            .dispatch
            .as_ref()
            .ok_or_else(|| failure("native_auth_authentication_failed"))?;
        self.verify_native_auth_origin(dispatch)?;
        slot.attached = true;
        slot.live.store(true, Ordering::SeqCst);
        Ok(Session {
            dispatch: dispatch.clone(),
            live: slot.live.clone(),
            deadline: slot.deadline,
            authority: Weak::new(),
        })
    }

    pub(super) fn stop_native_auth(&self) -> Result<(), EngineError> {
        self.invalidate_native_auth_dispatch(None)
    }

    pub(super) fn invalidate_native_auth_dispatch(
        &self,
        admission: Option<&Admission>,
    ) -> Result<(), EngineError> {
        {
            let mut current = self
                .native_auth
                .lock()
                .map_err(|_| failure("native_auth_recovery_required"))?;
            let Some(slot) = current.as_mut() else {
                return Ok(());
            };
            if admission.is_some_and(|admission| admission != &slot.admission) {
                return Ok(());
            }
            slot.expired = true;
            slot.attached = true;
            slot.live.store(false, Ordering::SeqCst);
            slot.progress = None;
            slot.claude_progress = None;
            if let Some(task) = &slot.supervisor {
                task.abort();
            }
            if let Some(task) = &slot.dispatch_task {
                task.abort();
            }
        }
        self.native_auth_changed.notify_waiters();
        Ok(())
    }

    pub(super) fn native_auth_dispatch_pending(&self) -> bool {
        self.native_auth
            .lock()
            .map(|current| {
                current
                    .as_ref()
                    .is_some_and(|slot| slot.dispatch_task.is_some())
            })
            .unwrap_or(false)
    }

    pub(super) async fn native_auth_dispatch_finished(&self) -> Result<(), EngineError> {
        std::future::poll_fn(|context| {
            let mut current = match self.native_auth.lock() {
                Ok(current) => current,
                Err(_) => return Poll::Ready(Err(failure("native_auth_recovery_required"))),
            };
            let Some(task) = current
                .as_mut()
                .and_then(|slot| slot.dispatch_task.as_mut())
            else {
                return Poll::Ready(Ok(()));
            };
            let Poll::Ready(result) = Pin::new(task).poll(context) else {
                return Poll::Pending;
            };
            current.as_mut().unwrap().dispatch_task.take();
            self.native_auth_changed.notify_waiters();
            Poll::Ready(match result {
                Ok(result) => result,
                Err(error) if error.is_cancelled() => Ok(()),
                Err(error) => Err(io_error(error)),
            })
        })
        .await
    }

    fn cleanup_native_auth_child(&self) -> Result<(), EngineError> {
        let (fact, mut child) = {
            let mut current = self
                .native_auth
                .lock()
                .map_err(|_| failure("native_auth_recovery_required"))?;
            let Some(slot) = current.as_mut() else {
                return Ok(());
            };
            if slot.cleaning {
                return Ok(());
            }
            if slot.child.is_none() || (!slot.expired && slot.dispatch_task.is_some()) {
                return Ok(());
            }
            slot.cleaning = true;
            (slot.fact.clone(), slot.child.take().unwrap())
        };
        let result = cleanup_direct_child(&mut child, std::time::Instant::now() + CLEANUP_BOUND);
        let mut current = self
            .native_auth
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let slot = current
            .as_mut()
            .expect("native auth cleanup retains its slot");
        assert_eq!(slot.fact, fact);
        assert!(slot.cleaning && slot.child.is_none());
        slot.cleaning = false;
        let result = match result {
            Ok(Some(_)) => Ok(()),
            Ok(None) => {
                slot.child = Some(child);
                Err(failure("native_auth_dispatch_cleanup_uncertain"))
            }
            Err(error) => {
                slot.child = Some(child);
                Err(io_error(error))
            }
        };
        drop(current);
        self.native_auth_changed.notify_waiters();
        result
    }

    pub(super) async fn drain_native_auth_dispatch(
        &self,
        engine: &Engine,
    ) -> Result<(), EngineError> {
        let mut task_error = None;
        let mut cleanup_deadline = None;
        loop {
            let changed = self.native_auth_changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            let (pending, cleaning, deadline) = {
                let current = self
                    .native_auth
                    .lock()
                    .map_err(|_| failure("native_auth_recovery_required"))?;
                let Some(slot) = current.as_ref() else {
                    return task_error.map_or(Ok(()), Err);
                };
                (
                    slot.dispatch_task.is_some(),
                    slot.cleaning,
                    if slot.expired {
                        *cleanup_deadline.get_or_insert_with(|| Instant::now() + CLEANUP_BOUND)
                    } else {
                        slot.deadline + CLEANUP_BOUND
                    },
                )
            };
            if pending {
                tokio::select! {
                    _ = &mut changed => continue,
                    result = tokio::time::timeout_at(deadline, self.native_auth_dispatch_finished()) => {
                        match result {
                            Ok(Ok(())) => {}
                            Ok(Err(error)) => task_error = Some(error),
                            Err(_) => return Err(failure("native_auth_dispatch_join_uncertain")),
                        }
                    }
                }
                continue;
            }
            if cleaning {
                tokio::time::timeout_at(deadline, &mut changed)
                    .await
                    .map_err(|_| failure("native_auth_dispatch_cleanup_uncertain"))?;
                continue;
            }
            self.cleanup_native_auth_child()?;
            let dispatch = {
                let mut current = self
                    .native_auth
                    .lock()
                    .map_err(|_| failure("native_auth_recovery_required"))?;
                let Some(slot) = current.as_ref() else {
                    return task_error.map_or(Ok(()), Err);
                };
                if slot.dispatch_task.is_some() || slot.cleaning {
                    drop(current);
                    continue;
                }
                if slot.child.is_some() {
                    return Err(failure("native_auth_dispatch_cleanup_uncertain"));
                }
                if slot.cached() && !slot.expired {
                    return task_error.map_or(Ok(()), Err);
                }
                if !slot.retained && !slot.expired {
                    return task_error.map_or(Ok(()), Err);
                }
                let Some(dispatch) = (!slot.committed).then(|| slot.dispatch.clone()).flatten()
                else {
                    current.take();
                    drop(current);
                    self.native_auth_changed.notify_waiters();
                    return task_error.map_or(Ok(()), Err);
                };
                current.as_mut().unwrap().cleaning = true;
                dispatch
            };
            let released = engine
                .native_auth_job(|reply| Job::NativeAuthCustodyReleased(dispatch, reply))
                .await;
            {
                let mut current = self
                    .native_auth
                    .lock()
                    .unwrap_or_else(|error| error.into_inner());
                assert!(current.as_ref().is_some_and(|slot| slot.cleaning
                    && slot.child.is_none()
                    && slot.dispatch_task.is_none()));
                current.take();
            }
            self.native_auth_changed.notify_waiters();
            if let Err(error) = released
                && custody_release_undelivered(&error)
            {
                return Err(error);
            }
            return task_error.map_or(Ok(()), Err);
        }
    }
}

/// Tells whether releasing an ended dispatch may not have reached the storage actor.
/// Input: the error from that job. Output: true when the job was not queued or its
/// reply never arrived. False when storage answered with a refusal, which means the
/// dispatch scope already changed and its durable fence stays in place for recovery.
fn custody_release_undelivered(error: &EngineError) -> bool {
    error.0 == "owner_queue_not_submitted"
        || *error == crate::engine::owner::native_auth::outcome_unknown()
}

impl Drop for Authority {
    fn drop(&mut self) {
        let current = self
            .native_auth
            .get_mut()
            .unwrap_or_else(|error| error.into_inner());
        if let Some(slot) = current.as_mut() {
            slot.live.store(false, Ordering::SeqCst);
            if let Some(task) = &slot.supervisor {
                task.abort();
            }
            if let Some(task) = &slot.dispatch_task {
                task.abort();
            }
            if let Some(child) = slot.child.as_mut()
                && !matches!(
                    cleanup_direct_child(child, std::time::Instant::now() + CLEANUP_BOUND),
                    Ok(Some(_))
                )
            {
                eprintln!("native_auth_dispatch_drop_cleanup_uncertain");
            }
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum AttachKind {
    Attach,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Attach {
    protocol_version: u16,
    kind: AttachKind,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum GrantKind {
    Grant,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Grant {
    protocol_version: u16,
    kind: GrantKind,
    fact: NativeAuthFact,
}

#[derive(Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum RequestKind {
    Check,
    RetireNoEffect,
    ObserveCodex,
    ObserveClaude,
    TakeClaudeInput,
    RetireAfterChildReaped,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    protocol_version: u16,
    operation_id: RequestId,
    generation: u64,
    seq: u64,
    kind: RequestKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    observation: Option<CodexAuthObservation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    claude_observation:
        Option<crate::engine::native_auth_host::claude_auth::types::ClaudeAuthObservation>,
}

#[derive(Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum ReplyKind {
    Continue,
    Cancel,
    Retired,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Reply {
    protocol_version: u16,
    seq: u64,
    kind: ReplyKind,
    fact: NativeAuthFact,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    claude_input: bool,
}

struct Session {
    dispatch: Arc<NativeAuthDispatch>,
    live: Arc<AtomicBool>,
    deadline: Instant,
    authority: Weak<Authority>,
}

impl Session {
    fn validate(&self) -> Result<Arc<Authority>, EngineError> {
        if !self.live.load(Ordering::SeqCst) || Instant::now() >= self.deadline {
            return Err(failure("native_auth_recovery_required"));
        }
        let authority = self
            .authority
            .upgrade()
            .ok_or_else(|| failure("owner_runtime_stale"))?;
        authority.verify_native_auth_origin(&self.dispatch)?;
        Ok(authority)
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.live.store(false, Ordering::SeqCst);
    }
}

pub(super) async fn serve(
    mut stream: UnixStream,
    packet: [u8; 37],
    engine: Arc<Engine>,
    authority: Weak<Authority>,
    timeout: Duration,
    attach_deadline: Instant,
) {
    let attached = async {
        let owner = authority
            .upgrade()
            .ok_or_else(|| failure("owner_runtime_stale"))?;
        let mut session = owner.attach_native_auth(&packet)?;
        session.authority = authority;
        drop(owner);
        let frame = read_frame(&mut stream).await?;
        let attach: Attach = serde_json::from_slice(&frame[4..])
            .map_err(|_| failure("native_auth_frame_invalid"))?;
        if attach.protocol_version != PROTOCOL_VERSION {
            return Err(failure("native_auth_protocol_version"));
        }
        let fact = check(&engine, &session).await?;
        let frame = encode_frame(&Grant {
            protocol_version: PROTOCOL_VERSION,
            kind: GrantKind::Grant,
            fact,
        })
        .map_err(io_error)?;
        stream.write_all(&frame).await.map_err(io_error)?;
        Ok::<_, EngineError>(session)
    };
    let Ok(Ok(session)) = tokio::time::timeout_at(attach_deadline, attached).await else {
        return;
    };
    let retired = tokio::time::timeout_at(
        session.deadline,
        serve_session(&mut stream, &engine, &session, timeout),
    )
    .await;
    session.live.store(false, Ordering::SeqCst);
    if !matches!(retired, Ok(Ok(()))) {
        if std::env::var_os("CRAZYAGENT_DEBUG_CUSTODIAN").is_some() {
            let cause = match &retired {
                Ok(Err(error)) => error.0.escape_debug().to_string(),
                _ => "native_auth_session_deadline".into(),
            };
            eprintln!("native_auth_custody_session_failed cause={cause}");
        }
        let _ = engine
            .native_auth_job(|reply| Job::NativeAuthCustodyLost(session.dispatch.clone(), reply))
            .await;
    }
    let _ = stream.shutdown().await;
}

async fn check(engine: &Engine, session: &Session) -> Result<NativeAuthFact, EngineError> {
    session.validate()?;
    let fact = engine
        .native_auth_job(|reply| Job::NativeAuthCustodyCheck(session.dispatch.clone(), reply))
        .await?;
    session.validate()?;
    Ok(fact)
}

async fn read_custody_frame(
    reader: &mut crate::platform::unix_socket::ReadHalf<'_>,
    timeout: Duration,
) -> Result<(Vec<u8>, Instant), EngineError> {
    let first = [reader.read_u8().await.map_err(io_error)?];
    let deadline = Instant::now() + timeout;
    let mut input = tokio::io::AsyncReadExt::chain(first.as_slice(), reader);
    let frame = tokio::time::timeout_at(deadline, read_frame(&mut input))
        .await
        .map_err(|_| failure("native_auth_channel_timeout"))??;
    Ok((frame, deadline))
}

impl Session {
    fn request(&self, frame: &[u8], expected: &mut u64) -> Result<Request, EngineError> {
        let request: Request = serde_json::from_slice(&frame[4..])
            .map_err(|_| failure("native_auth_frame_invalid"))?;
        let fact = self.dispatch.fact();
        if request.protocol_version != PROTOCOL_VERSION
            || request.operation_id != fact.operation_id
            || request.generation != fact.generation
            || request.seq != *expected
            || (request.kind == RequestKind::ObserveCodex) != request.observation.is_some()
            || (request.kind == RequestKind::ObserveClaude) != request.claude_observation.is_some()
            || (request.kind == RequestKind::TakeClaudeInput
                && fact.binding.connection.harness.as_str() != "claude")
        {
            return Err(failure("native_auth_frame_scope"));
        }
        *expected = expected
            .checked_add(1)
            .ok_or_else(|| failure("native_auth_sequence_bounds"))?;
        Ok(request)
    }

    fn committed(&self, after_child_reaped: bool) -> Result<(), EngineError> {
        let owner = self
            .authority
            .upgrade()
            .ok_or_else(|| failure("owner_runtime_stale"))?;
        let mut current = owner
            .native_auth
            .lock()
            .map_err(|_| failure("native_auth_recovery_required"))?;
        if let Some(slot) = current.as_mut()
            && slot
                .dispatch
                .as_ref()
                .is_some_and(|dispatch| Arc::ptr_eq(dispatch, &self.dispatch))
        {
            slot.committed = true;
            if !after_child_reaped {
                slot.progress = None;
                slot.claude_progress = None;
            }
            if !slot.retained
                && slot.dispatch_task.is_none()
                && slot.child.is_none()
                && !slot.cleaning
                && slot.progress.is_none()
                && slot.claude_progress.is_none()
            {
                current.take();
            }
        }
        Ok(())
    }

    fn validate_observation(&self, event: &CodexAuthObservation) -> Result<(), EngineError> {
        event.validate()?;
        let owner = self.validate()?;
        let current = owner
            .native_auth
            .lock()
            .map_err(|_| failure("native_auth_recovery_required"))?;
        let slot = current
            .as_ref()
            .filter(|slot| {
                slot.dispatch
                    .as_ref()
                    .is_some_and(|dispatch| Arc::ptr_eq(dispatch, &self.dispatch))
            })
            .ok_or_else(|| failure("native_auth_recovery_required"))?;
        if event.request_id().is_some_and(|id| {
            id <= slot
                .progress
                .as_ref()
                .map_or(0, |progress| progress.request_id)
        }) {
            return Err(failure("native_auth_observation_order"));
        }
        match event {
            CodexAuthObservation::LoginStarted { .. }
                if slot
                    .progress
                    .as_ref()
                    .is_some_and(|progress| progress.login_id.is_some()) =>
            {
                return Err(failure("native_auth_observation_order"));
            }
            CodexAuthObservation::LoginCompleted { login_id, .. }
                if login_id.is_none()
                    || login_id.as_deref()
                        != slot
                            .progress
                            .as_ref()
                            .and_then(|progress| progress.login_id.as_deref()) =>
            {
                return Err(failure("native_auth_observation_scope"));
            }
            _ => {}
        }
        Ok(())
    }

    fn record_observation(
        &self,
        event: &CodexAuthObservation,
        fact: NativeAuthFact,
    ) -> Result<(), EngineError> {
        let owner = self.validate()?;
        let mut current = owner
            .native_auth
            .lock()
            .map_err(|_| failure("native_auth_recovery_required"))?;
        let slot = current
            .as_mut()
            .filter(|slot| {
                slot.dispatch
                    .as_ref()
                    .is_some_and(|dispatch| Arc::ptr_eq(dispatch, &self.dispatch))
            })
            .ok_or_else(|| failure("native_auth_recovery_required"))?;
        let login_id = match event {
            CodexAuthObservation::LoginStarted { login_id, .. } => Some(login_id.clone()),
            _ => slot
                .progress
                .as_ref()
                .and_then(|progress| progress.login_id.clone()),
        };
        let request_id = event.request_id().unwrap_or_else(|| {
            slot.progress
                .as_ref()
                .map_or(0, |progress| progress.request_id)
        });
        let challenge = if fact.phase == NativeAuthPhase::AwaitingOwner {
            event.challenge()?
        } else {
            None
        };
        let account = if fact.phase == NativeAuthPhase::AuthenticatedObserved {
            event.account()?
        } else {
            None
        };
        slot.progress = Some(CodexProgress {
            fact,
            login_id,
            request_id,
            challenge,
            account,
        });
        assert!(slot.progress.is_some());
        assert!(!slot.committed);
        Ok(())
    }
}

async fn custody_reply(
    engine: &Engine,
    session: &Session,
    request: &Request,
    reader: &mut crate::platform::unix_socket::ReadHalf<'_>,
) -> Result<Reply, EngineError> {
    session.validate()?;
    let retirement = matches!(
        request.kind,
        RequestKind::RetireNoEffect | RequestKind::RetireAfterChildReaped
    );
    if let Some(event) = &request.observation {
        session.validate_observation(event)?;
    }
    if let Some(event) = &request.claude_observation {
        session.validate_claude(event)?;
    }
    let job = async {
        if retirement {
            engine
                .native_auth_job(|reply| {
                    Job::NativeAuthCustodyRetire(
                        NativeAuthCustodyRetirement {
                            dispatch: session.dispatch.clone(),
                            live: session.live.clone(),
                            after_child_reaped: request.kind == RequestKind::RetireAfterChildReaped,
                        },
                        reply,
                    )
                })
                .await
        } else if let Some(event) = &request.observation {
            engine
                .native_auth_job(|reply| {
                    Job::NativeAuthCustodyObserve(
                        NativeAuthCustodyObservation {
                            dispatch: session.dispatch.clone(),
                            live: session.live.clone(),
                            event: claude::CustodyEvent::Codex(event.clone()),
                        },
                        reply,
                    )
                })
                .await
        } else if let Some(event) = &request.claude_observation {
            engine
                .native_auth_job(|reply| {
                    Job::NativeAuthCustodyObserve(
                        NativeAuthCustodyObservation {
                            dispatch: session.dispatch.clone(),
                            live: session.live.clone(),
                            event: claude::CustodyEvent::Claude(event.clone()),
                        },
                        reply,
                    )
                })
                .await
        } else {
            check(engine, session).await
        }
    };
    let fact = tokio::select! {
        biased;
        _ = reader.read_u8() => {
            session.live.store(false, Ordering::SeqCst);
            return Err(failure("native_auth_channel_lost"));
        }
        fact = job => fact?,
    };
    let kind = if retirement {
        session.committed(request.kind == RequestKind::RetireAfterChildReaped)?;
        ReplyKind::Retired
    } else {
        session.validate()?;
        if let Some(event) = &request.observation {
            session.record_observation(event, fact.clone())?;
        }
        if let Some(event) = &request.claude_observation {
            session.record_claude(event, fact.clone())?;
        }
        if matches!(
            fact.phase,
            NativeAuthPhase::Cancelling | NativeAuthPhase::Cancelled
        ) {
            ReplyKind::Cancel
        } else {
            ReplyKind::Continue
        }
    };
    Ok(Reply {
        protocol_version: PROTOCOL_VERSION,
        seq: request.seq,
        kind,
        fact,
        claude_input: false,
    })
}

async fn serve_session(
    stream: &mut UnixStream,
    engine: &Engine,
    session: &Session,
    timeout: Duration,
) -> Result<(), EngineError> {
    let (mut reader, mut writer) = stream.split();
    let mut expected = 1_u64;
    loop {
        let (frame, deadline) = read_custody_frame(&mut reader, timeout).await?;
        let request = session.request(&frame, &mut expected)?;
        let retirement = matches!(
            request.kind,
            RequestKind::RetireNoEffect | RequestKind::RetireAfterChildReaped
        );
        let exchange = async {
            let mut reply = custody_reply(engine, session, &request, &mut reader).await?;
            let secret = if request.kind == RequestKind::TakeClaudeInput {
                session.take_claude_code(&reply.fact)?
            } else {
                None
            };
            reply.claude_input = secret.is_some();
            let frame = encode_frame(&reply).map_err(io_error)?;
            writer.write_all(&frame).await.map_err(io_error)?;
            if let Some(secret) = secret {
                claude::write_secret(&mut writer, secret).await?;
            }
            Ok::<(), EngineError>(())
        };
        tokio::time::timeout_at(deadline, exchange)
            .await
            .map_err(|_| failure("native_auth_channel_timeout"))??;
        if retirement {
            return Ok(());
        }
    }
}

#[cfg(unix)]
pub(crate) struct NativeAuthCustodyChannel {
    stream: Option<UnixStream>,
    binding: NativeBinding,
    root: Root,
    socket: Identity,
    fact: NativeAuthFact,
    next_seq: u64,
    deadline: Instant,
    outstanding: bool,
    sealed: bool,
}

#[cfg(unix)]
impl NativeAuthCustodyChannel {
    pub(crate) async fn connect(input: &Path, token: [u8; 32]) -> Result<Self, EngineError> {
        let entered = Instant::now();
        tokio::time::timeout(READ_BOUND, Self::connect_pinned(input, token, entered))
            .await
            .map_err(|_| failure("native_auth_channel_timeout"))?
    }

    async fn connect_pinned(
        input: &Path,
        mut token: [u8; 32],
        entered: Instant,
    ) -> Result<Self, EngineError> {
        let (generation, host, binding) = runner_binding(input)?;
        let root = Root::open(&generation)?;
        let socket = root.socket_identity()?;
        let mut stream = crate::spawn_gate::connect_unix(root.path.join(SOCKET))
            .await
            .map_err(io_error)?;
        verify_peer(&stream)?;
        binding.verify()?;
        if root.socket_identity()? != socket {
            return Err(failure("owner_socket_replaced"));
        }
        let mut packet = [0; 37];
        packet[..4].copy_from_slice(b"CAN1");
        packet[5..].copy_from_slice(&token);
        token.fill(0);
        stream.write_all(&packet).await.map_err(io_error)?;
        packet.fill(0);
        let frame = encode_frame(&Attach {
            protocol_version: PROTOCOL_VERSION,
            kind: AttachKind::Attach,
        })
        .map_err(io_error)?;
        stream.write_all(&frame).await.map_err(io_error)?;
        let frame = read_frame(&mut stream).await?;
        let grant: Grant = serde_json::from_slice(&frame[4..])
            .map_err(|_| failure("native_auth_grant_invalid"))?;
        Self::validate_grant(&grant, &host)?;
        binding.verify()?;
        if root.socket_identity()? != socket {
            return Err(failure("owner_socket_replaced"));
        }
        let deadline = entered + operation_bound(&grant.fact);
        Ok(Self {
            stream: Some(stream),
            binding,
            root,
            socket,
            fact: grant.fact,
            next_seq: 1,
            deadline,
            outstanding: false,
            sealed: false,
        })
    }

    fn validate_grant(
        grant: &Grant,
        host: &crate::engine::bootstrap::OwnerConnectionIdentity,
    ) -> Result<(), EngineError> {
        if grant.protocol_version != PROTOCOL_VERSION
            || grant.fact.generation == 0
            || grant.fact.binding.database_instance != host.database_instance()
            || grant.fact.binding.engine_epoch != host.engine_epoch()
            || !matches!(
                grant.fact.phase,
                NativeAuthPhase::Starting | NativeAuthPhase::Cancelling
            )
        {
            return Err(failure("native_auth_grant_invalid"));
        }
        Ok(())
    }

    pub(crate) fn fact(&self) -> &NativeAuthFact {
        &self.fact
    }

    fn verify(&self) -> Result<(), EngineError> {
        self.binding.verify()?;
        if Instant::now() >= self.deadline || self.root.socket_identity()? != self.socket {
            return Err(failure("native_auth_channel_lost"));
        }
        Ok(())
    }

    fn close(&mut self) {
        self.sealed = true;
        self.stream.take();
    }

    fn guard_or_close(&mut self, guard: &NativeProfileUse) -> Result<(), EngineError> {
        if let Err(error) = self.verify_guard(guard) {
            self.close();
            return Err(error);
        }
        Ok(())
    }

    pub(crate) async fn revalidate(
        &mut self,
        guard: &NativeProfileUse,
    ) -> Result<NativeAuthCustodyControl, EngineError> {
        if self.sealed {
            return Err(failure("native_auth_recovery_required"));
        }
        self.guard_or_close(guard)?;
        let reply = self.exchange(RequestKind::Check, None).await?;
        self.guard_or_close(guard)?;
        match reply.kind {
            ReplyKind::Continue => Ok(NativeAuthCustodyControl::Continue),
            ReplyKind::Cancel => Ok(NativeAuthCustodyControl::Cancel),
            ReplyKind::Retired => Err(failure("native_auth_reply_invalid")),
        }
    }

    pub(crate) async fn observe_codex(
        &mut self,
        guard: &NativeProfileUse,
        event: crate::adapters::codex::AuthControl,
    ) -> Result<NativeAuthCustodyControl, EngineError> {
        if self.sealed {
            return Err(failure("native_auth_recovery_required"));
        }
        self.guard_or_close(guard)?;
        let observation = CodexAuthObservation::from_native(event)?;
        let reply = self
            .exchange(RequestKind::ObserveCodex, Some(observation))
            .await?;
        self.guard_or_close(guard)?;
        match reply.kind {
            ReplyKind::Continue => Ok(NativeAuthCustodyControl::Continue),
            ReplyKind::Cancel => Ok(NativeAuthCustodyControl::Cancel),
            ReplyKind::Retired => Err(failure("native_auth_reply_invalid")),
        }
    }

    fn verify_guard(&self, guard: &NativeProfileUse) -> Result<(), EngineError> {
        self.verify()?;
        guard.revalidate(
            &fact_host(&self.fact),
            &self.fact.binding,
            &self.fact.operation_id,
            self.fact.generation,
            purpose(&self.fact),
        )
    }

    pub(in crate::engine) async fn retire(
        &mut self,
        retirement: Retirement,
    ) -> Result<NativeAuthFact, EngineError> {
        if self.sealed {
            return Err(failure("native_auth_recovery_required"));
        }
        self.sealed = true;
        self.retirement_or_close(&retirement)?;
        let reply = self.exchange(RequestKind::RetireNoEffect, None).await?;
        self.retirement_or_close(&retirement)?;
        if reply.kind != ReplyKind::Retired || reply.fact.phase != NativeAuthPhase::Unavailable {
            self.close();
            return Err(failure("native_auth_reply_invalid"));
        }
        self.close();
        drop(retirement);
        Ok(reply.fact)
    }

    pub(in crate::engine) async fn retire_after_child_reaped(
        &mut self,
        retirement: Retirement,
    ) -> Result<NativeAuthFact, EngineError> {
        if self.sealed {
            return Err(failure("native_auth_recovery_required"));
        }
        self.sealed = true;
        self.retirement_after_child_or_close(&retirement)?;
        let reply = self
            .exchange(RequestKind::RetireAfterChildReaped, None)
            .await?;
        self.retirement_after_child_or_close(&retirement)?;
        if reply.kind != ReplyKind::Retired
            || !matches!(
                reply.fact.phase,
                NativeAuthPhase::AuthenticatedObserved
                    | NativeAuthPhase::UnauthenticatedObserved
                    | NativeAuthPhase::Failed
                    | NativeAuthPhase::Cancelled
            )
        {
            self.close();
            return Err(failure("native_auth_reply_invalid"));
        }
        self.close();
        drop(retirement);
        Ok(reply.fact)
    }

    fn retirement_after_child_or_close(
        &mut self,
        retirement: &Retirement,
    ) -> Result<(), EngineError> {
        if let Err(error) = retirement.revalidate_after_child_reaped(
            &fact_host(&self.fact),
            &self.fact.binding,
            &self.fact.operation_id,
            self.fact.generation,
            purpose(&self.fact),
        ) {
            self.close();
            return Err(error);
        }
        Ok(())
    }

    fn retirement_or_close(&mut self, retirement: &Retirement) -> Result<(), EngineError> {
        if let Err(error) = retirement.revalidate(
            &fact_host(&self.fact),
            &self.fact.binding,
            &self.fact.operation_id,
            self.fact.generation,
            purpose(&self.fact),
        ) {
            self.close();
            return Err(error);
        }
        Ok(())
    }

    fn next_request(
        &mut self,
        kind: RequestKind,
        observation: Option<CodexAuthObservation>,
    ) -> Result<Request, EngineError> {
        if self.outstanding {
            return Err(failure("native_auth_recovery_required"));
        }
        self.verify()?;
        self.outstanding = true;
        let seq = self.next_seq;
        self.next_seq = seq
            .checked_add(1)
            .ok_or_else(|| failure("native_auth_sequence_bounds"))?;
        Ok(Request {
            protocol_version: PROTOCOL_VERSION,
            operation_id: self.fact.operation_id.clone(),
            generation: self.fact.generation,
            seq,
            kind,
            observation,
            claude_observation: None,
        })
    }

    fn validate_reply(&self, request: &Request, reply: &Reply) -> Result<(), EngineError> {
        if reply.protocol_version != PROTOCOL_VERSION
            || reply.seq != request.seq
            || reply.fact.operation_id != self.fact.operation_id
            || reply.fact.generation != self.fact.generation
            || reply.fact.binding != self.fact.binding
            || reply.fact.action != self.fact.action
            || (reply.claude_input
                && (request.kind != RequestKind::TakeClaudeInput
                    || reply.kind != ReplyKind::Continue))
        {
            return Err(failure("native_auth_reply_invalid"));
        }
        let valid = match reply.kind {
            ReplyKind::Continue => {
                matches!(
                    request.kind,
                    RequestKind::Check
                        | RequestKind::ObserveCodex
                        | RequestKind::ObserveClaude
                        | RequestKind::TakeClaudeInput
                ) && matches!(
                    reply.fact.phase,
                    NativeAuthPhase::Starting
                        | NativeAuthPhase::AwaitingOwner
                        | NativeAuthPhase::CheckingAccount
                        | NativeAuthPhase::AuthenticatedObserved
                        | NativeAuthPhase::UnauthenticatedObserved
                        | NativeAuthPhase::Failed
                )
            }
            ReplyKind::Cancel => {
                matches!(
                    request.kind,
                    RequestKind::Check
                        | RequestKind::ObserveCodex
                        | RequestKind::ObserveClaude
                        | RequestKind::TakeClaudeInput
                ) && matches!(
                    reply.fact.phase,
                    NativeAuthPhase::Cancelling | NativeAuthPhase::Cancelled
                )
            }
            ReplyKind::Retired => {
                (request.kind == RequestKind::RetireNoEffect
                    && reply.fact.phase == NativeAuthPhase::Unavailable)
                    || (request.kind == RequestKind::RetireAfterChildReaped
                        && matches!(
                            reply.fact.phase,
                            NativeAuthPhase::AuthenticatedObserved
                                | NativeAuthPhase::UnauthenticatedObserved
                                | NativeAuthPhase::Failed
                                | NativeAuthPhase::Cancelled
                        ))
            }
        };
        if !valid {
            return Err(failure("native_auth_reply_invalid"));
        }
        Ok(())
    }

    async fn exchange(
        &mut self,
        kind: RequestKind,
        observation: Option<CodexAuthObservation>,
    ) -> Result<Reply, EngineError> {
        let request = match self.next_request(kind, observation) {
            Ok(request) => request,
            Err(error) => {
                self.close();
                return Err(error);
            }
        };
        self.exchange_request(request).await
    }

    async fn exchange_request(&mut self, request: Request) -> Result<Reply, EngineError> {
        let result =
            tokio::time::timeout_at(self.deadline.min(Instant::now() + READ_BOUND), async {
                let stream = self
                    .stream
                    .as_mut()
                    .ok_or_else(|| failure("native_auth_channel_lost"))?;
                stream
                    .write_all(&encode_frame(&request).map_err(io_error)?)
                    .await
                    .map_err(io_error)?;
                let frame = read_frame(stream).await?;
                let reply: Reply = serde_json::from_slice(&frame[4..])
                    .map_err(|_| failure("native_auth_reply_invalid"))?;
                self.validate_reply(&request, &reply)?;
                Ok(reply)
            })
            .await
            .map_err(|_| failure("native_auth_channel_timeout"))
            .and_then(|result| result);
        if result.is_err() {
            self.close();
        }
        self.outstanding = false;
        if let Err(error) = self.verify() {
            self.close();
            return Err(error);
        }
        if let Ok(reply) = &result {
            self.fact = reply.fact.clone();
        }
        result
    }
}

#[cfg(all(test, unix))]
mod auth_observation_tests {
    use super::*;
    use crate::adapters::codex::{AuthControl, CodexProtocol, Event, LoginChallenge};
    use crate::engine::bootstrap::EngineService;
    use crate::engine::native_auth_host::custody::CustodySession;
    use crate::engine::owner::native_auth::{Command, NativeAuthAccount};
    use rusqlite::{Connection, OpenFlags};
    use serde_json::{Value, json};
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::process::CommandExt;
    use std::process::{Command as ProcessCommand, Stdio};

    const URL: &str = "https://auth.openai.com/authorize?state=observation-private-state";
    const DEVICE_URL: &str = "https://auth.openai.com/device/observation-private-state";
    const USER_CODE: &str = "OBSERVATION-PRIVATE-CODE";
    const EMAIL: &str = "observation-private@example.test";
    const ERROR: &str = "observation-private-native-error";
    const CATALOG: &str = r#"
timeout_ms = 1000
[model_access]
accounts = ["account-a"]
[[model_access.connections]]
id = "connection-a"
provider = "provider-a"
harness = "codex"
protocol = "native"
account_ref = "account-a"
endpoint = { kind = "native" }
model = { id = "model-a", author = "author-a" }
auth = { method = "native_managed", profile_ref = "profile-a" }
[[model_access.pools]]
id = "pool-a"
members = ["connection-a"]
projects = ["project-a"]
roles = ["leader"]
fallback_allowed = false
"#;

    struct Case {
        root: tempfile::TempDir,
        service: EngineService,
        client: OwnerClient,
        channel: NativeAuthCustodyChannel,
        guard: Option<NativeProfileUse>,
        scope: NativeAuthScope,
        fact: NativeAuthFact,
    }

    fn id(value: &str) -> RequestId {
        RequestId::new(value).unwrap()
    }

    fn protocol() -> CodexProtocol {
        let mut protocol = CodexProtocol::new();
        protocol.initialize().unwrap();
        let frame = json!({"id":1,"result":{
            "userAgent":"observation-unit-input", "codexHome":"/unit/empty-profile",
            "platformFamily":"unix", "platformOs":"macos"
        }});
        let bytes = format!("{frame}\n");
        assert_eq!(
            protocol.receive(bytes.as_bytes()).unwrap(),
            vec![Event::Initialized]
        );
        protocol.initialized().unwrap();
        protocol
    }

    fn control(protocol: &mut CodexProtocol, frame: Value) -> AuthControl {
        let bytes = format!("{frame}\n");
        let mut events = protocol.receive(bytes.as_bytes()).unwrap();
        assert_eq!(events.len(), 1);
        match events.remove(0) {
            Event::AuthControl(event) => event,
            _ => panic!("actual parser did not produce an authentication control"),
        }
    }

    fn challenge(protocol: &mut CodexProtocol, device: bool) -> AuthControl {
        let result = if device {
            protocol.start_device_code_login().unwrap();
            json!({"type":"chatgptDeviceCode", "loginId":"observed-login",
                "userCode":USER_CODE, "verificationUrl":DEVICE_URL})
        } else {
            protocol.start_chatgpt_login().unwrap();
            json!({"type":"chatgpt", "loginId":"observed-login", "authUrl":URL})
        };
        control(protocol, json!({"id":2,"result":result}))
    }

    impl Case {
        async fn open(action: NativeAuthAction) -> Self {
            Self::open_with(CATALOG, action).await
        }

        async fn open_with(catalog: &str, action: NativeAuthAction) -> Self {
            let root = tempfile::Builder::new()
                .prefix("ca-auth-observe-")
                .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
                .unwrap();
            std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
            let global = root.path().join("g.toml");
            std::fs::write(&global, catalog).unwrap();
            std::fs::set_permissions(&global, std::fs::Permissions::from_mode(0o600)).unwrap();
            let project = root.path().join("p.toml");
            let config = crate::config::resolve_config(&global, &project).unwrap();
            let (_stop, stopped) = tokio::sync::watch::channel(false);
            let service = EngineService::start_with_settings_sources(
                root.path(),
                "e.sock",
                &global,
                &project,
                &stopped,
            )
            .await
            .unwrap();
            let host = service.owner_runtime().unwrap();
            let client = OwnerClient::attach(
                &host.authority.root.path,
                OwnerClientKind::Tui,
                Duration::from_secs(2),
            )
            .unwrap();
            let response = client
                .request(OwnerRequest {
                    protocol_version: PROTOCOL_VERSION,
                    request_id: id("observation-source"),
                    expected_revision: Cursor::new(0),
                    operation: OwnerOperation::Read {
                        query: OwnerRead::ModelAccessRecord {
                            collection: ModelAccessCollection::Connections,
                            id: CredentialRef::new("connection-a").unwrap(),
                        },
                    },
                })
                .await
                .unwrap();
            let OwnerResponse::Read { state } = response.response else {
                panic!("actual owner did not return a source record")
            };
            let OwnerData::ModelAccessRecord { source_hash, .. } = state.data else {
                panic!("actual owner did not return source identity")
            };
            let scope = NativeAuthScope {
                expected_database_instance: host.identity().db_instance,
                connection: CredentialRef::new("connection-a").unwrap(),
                expected_config_id: config.id,
                expected_source_hash: source_hash,
            };
            Self::begin(root, service, client, scope, action).await
        }

        async fn begin(
            root: tempfile::TempDir,
            service: EngineService,
            client: OwnerClient,
            scope: NativeAuthScope,
            action: NativeAuthAction,
        ) -> Self {
            let host = service.owner_runtime().unwrap();
            let mut packet = [0; 37];
            packet[..4].copy_from_slice(b"CAO1");
            packet[5..].copy_from_slice(&client.token);
            let admission = host.authority.authenticate(&packet).unwrap();
            let command = match action {
                NativeAuthAction::SignIn { method } => Command::Start(NativeAuthStart {
                    request_id: id("observed-operation"),
                    scope: scope.clone(),
                    method,
                }),
                NativeAuthAction::Logout => Command::Logout(NativeAuthLogout {
                    request_id: id("observed-operation"),
                    scope: scope.clone(),
                }),
            };
            let begun = host
                .engine
                .native_auth_job(|reply| Job::NativeAuth(admission, command, reply))
                .await
                .unwrap();
            assert_eq!(begun.reply.fact.phase, NativeAuthPhase::Starting);
            let fact = begun.reply.fact;
            let _launch = host
                .prepare_native_auth_custody(begun.work.unwrap())
                .await
                .unwrap();
            let (_, _, binding) = runner_binding(root.path()).unwrap();
            let mut custody = binding.capture_native_profile_custody().unwrap();
            let guard = custody
                .acquire_use(
                    &host.identity(),
                    &fact.binding,
                    &fact.operation_id,
                    fact.generation,
                    purpose(&fact),
                )
                .unwrap();
            let token = host
                .authority
                .native_auth
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .token;
            let channel = NativeAuthCustodyChannel::connect(root.path(), token)
                .await
                .unwrap();
            Self {
                root,
                service,
                client,
                channel,
                guard: Some(guard),
                scope,
                fact,
            }
        }

        async fn observe(&mut self, event: AuthControl) -> NativeAuthCustodyControl {
            self.channel
                .observe_codex(self.guard.as_ref().unwrap(), event)
                .await
                .unwrap()
        }

        async fn read(&self) -> NativeAuthReply {
            self.client
                .read_native_auth(NativeAuthQuery {
                    request_id: id("read-observed-operation"),
                    scope: self.scope.clone(),
                    operation_id: self.fact.operation_id.clone(),
                })
                .await
                .unwrap()
        }

        fn database(&self) -> Connection {
            let db = Connection::open_with_flags(
                self.root.path().join("engine.db"),
                OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NOFOLLOW,
            )
            .unwrap();
            db.busy_timeout(Duration::from_secs(1)).unwrap();
            db
        }

        fn fence_count(&self) -> i64 {
            self.database()
                .query_row("SELECT count(*) FROM native_auth_fences", [], |row| {
                    row.get(0)
                })
                .unwrap()
        }

        fn durable_payloads_are_redacted(&self) {
            let (request, fact): (Vec<u8>, Vec<u8>) = self
                .database()
                .query_row(
                    "SELECT request,fact FROM native_auth_operations WHERE id=?1",
                    [self.fact.operation_id.as_str()],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .unwrap();
            for secret in [URL, DEVICE_URL, USER_CODE, EMAIL, ERROR] {
                assert!(
                    !request
                        .windows(secret.len())
                        .any(|window| window == secret.as_bytes())
                );
                assert!(
                    !fact
                        .windows(secret.len())
                        .any(|window| window == secret.as_bytes())
                );
            }
        }

        async fn retire(&mut self) -> NativeAuthFact {
            let mut session = CustodySession::new(self.guard.take().unwrap());
            let mut child = crate::spawn_gate::spawn(
                ProcessCommand::new("/bin/cat")
                    .env_clear()
                    .stdin(Stdio::piped())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .process_group(0),
            )
            .unwrap();
            let input = child.stdin.take().unwrap();
            session.retain_child(child).unwrap();
            drop(input);
            tokio::time::timeout(Duration::from_secs(2), async {
                loop {
                    if let Some(status) = session.child_mut().unwrap().try_wait().unwrap() {
                        assert!(status.success());
                        break;
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            let retirement = session.seal_after_child_reaped().unwrap();
            self.channel
                .retire_after_child_reaped(retirement)
                .await
                .unwrap()
        }

        async fn close(mut self) {
            drop(self.channel);
            drop(self.guard.take());
            tokio::time::timeout(Duration::from_secs(5), self.service.shutdown())
                .await
                .unwrap()
                .unwrap();
        }
    }

    #[tokio::test]
    async fn native_auth_observation_browser_account_is_published_through_real_owner() {
        tokio::time::timeout(Duration::from_secs(20), async {
            let mut case = Case::open(NativeAuthAction::SignIn {
                method: NativeAuthMethod::CodexBrowser,
            })
            .await;
            let mut protocol = protocol();
            assert!(matches!(
                case.observe(challenge(&mut protocol, false)).await,
                NativeAuthCustodyControl::Continue
            ));
            let waiting = case.read().await;
            assert_eq!(waiting.fact.phase, NativeAuthPhase::AwaitingOwner);
            assert!(matches!(waiting.challenge.unwrap().codex(),
                LoginChallenge::Browser { auth_url, .. } if auth_url.expose() == URL));
            assert!(waiting.account.is_none());
            case.durable_payloads_are_redacted();
            case.observe(control(
                &mut protocol,
                json!({"method":"account/login/completed",
                "params":{"loginId":"observed-login","success":true}}),
            ))
            .await;
            let checking = case.read().await;
            assert_eq!(checking.fact.phase, NativeAuthPhase::CheckingAccount);
            assert!(checking.account.is_none());
            assert!(checking.challenge.is_none());
            protocol.read_account(false).unwrap();
            case.observe(control(
                &mut protocol,
                json!({"id":3,"result":{
                    "account":{"type":"chatgpt","email":EMAIL,"planType":"plus"},
                    "requiresOpenaiAuth":true
                }}),
            ))
            .await;
            let observed = case.read().await;
            assert_eq!(observed.fact.phase, NativeAuthPhase::AuthenticatedObserved);
            assert!(matches!(&observed.account, Some(NativeAuthAccount::Codex(
                crate::adapters::codex::AuthAccount::Chatgpt { email: Some(email), .. }
            )) if email.expose() == EMAIL));
            assert!(observed.challenge.is_none());
            assert!(!format!("{observed:?}").contains(EMAIL));
            assert_eq!(case.fence_count(), 1);
            let retired = case.retire().await;
            assert_eq!(retired.phase, NativeAuthPhase::AuthenticatedObserved);
            assert_eq!(case.fence_count(), 0);
            let replay = case.read().await;
            assert_eq!(replay.fact, retired);
            assert!(replay.account.is_some());
            case.durable_payloads_are_redacted();
            case.close().await;
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn native_auth_observation_device_challenge_rejects_foreign_generation_guard() {
        tokio::time::timeout(Duration::from_secs(20), async {
            let mut case = Case::open(NativeAuthAction::SignIn {
                method: NativeAuthMethod::CodexDeviceCode,
            })
            .await;
            let mut protocol = protocol();
            case.observe(challenge(&mut protocol, true)).await;
            let waiting = case.read().await;
            assert_eq!(waiting.fact.phase, NativeAuthPhase::AwaitingOwner);
            assert!(matches!(waiting.challenge.unwrap().codex(),
                LoginChallenge::DeviceCode { user_code, verification_url, .. }
                    if user_code.expose() == USER_CODE && verification_url.expose() == DEVICE_URL));
            case.durable_payloads_are_redacted();
            drop(case.guard.take());
            let host = case.service.owner_runtime().unwrap();
            let (_, _, binding) = runner_binding(case.root.path()).unwrap();
            let mut custody = binding.capture_native_profile_custody().unwrap();
            let foreign = custody
                .acquire_use(
                    &host.identity(),
                    &case.fact.binding,
                    &case.fact.operation_id,
                    case.fact.generation + 1,
                    purpose(&case.fact),
                )
                .unwrap();
            let completion = control(
                &mut protocol,
                json!({"method":"account/login/completed",
                "params":{"loginId":"observed-login","success":true}}),
            );
            assert!(
                case.channel
                    .observe_codex(&foreign, completion)
                    .await
                    .is_err()
            );
            assert_eq!(case.fence_count(), 1);
            assert_ne!(
                case.read().await.fact.phase,
                NativeAuthPhase::AuthenticatedObserved
            );
            drop(foreign);
            case.close().await;
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn native_auth_observation_logout_waits_for_account_read_then_retires() {
        tokio::time::timeout(Duration::from_secs(20), async {
            let mut case = Case::open(NativeAuthAction::Logout).await;
            let mut protocol = protocol();
            protocol.logout_account().unwrap();
            case.observe(control(&mut protocol, json!({"id":2,"result":{}})))
                .await;
            let checking = case.read().await;
            assert_eq!(checking.fact.phase, NativeAuthPhase::CheckingAccount);
            assert!(checking.account.is_none());
            assert_eq!(case.fence_count(), 1);
            protocol.read_account(false).unwrap();
            case.observe(control(
                &mut protocol,
                json!({"id":3,"result":{
                    "account":null,"requiresOpenaiAuth":true
                }}),
            ))
            .await;
            let observed = case.read().await;
            assert_eq!(
                observed.fact.phase,
                NativeAuthPhase::UnauthenticatedObserved
            );
            assert!(observed.account.is_none());
            let retired = case.retire().await;
            assert_eq!(retired.phase, NativeAuthPhase::UnauthenticatedObserved);
            assert_eq!(retired.action, NativeAuthAction::Logout);
            assert_eq!(case.fence_count(), 0);
            assert_eq!(case.read().await.fact, retired);
            case.close().await;
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn native_auth_observation_native_failure_is_fixed_code_without_secret_payload() {
        tokio::time::timeout(Duration::from_secs(20), async {
            let mut case = Case::open(NativeAuthAction::SignIn {
                method: NativeAuthMethod::CodexBrowser,
            })
            .await;
            let mut protocol = protocol();
            case.observe(challenge(&mut protocol, false)).await;
            case.observe(control(
                &mut protocol,
                json!({"method":"account/login/completed",
                "params":{"loginId":"observed-login","success":false,"error":ERROR}}),
            ))
            .await;
            let failed = case.read().await;
            assert_eq!(failed.fact.phase, NativeAuthPhase::Failed);
            assert_eq!(
                failed.fact.code.as_deref(),
                Some("native_auth_login_failed")
            );
            assert!(failed.account.is_none());
            assert!(failed.challenge.is_none());
            assert!(!format!("{failed:?}").contains(ERROR));
            case.durable_payloads_are_redacted();
            assert_eq!(case.fence_count(), 1);
            assert_eq!(case.retire().await.phase, NativeAuthPhase::Failed);
            assert_eq!(case.fence_count(), 0);
            case.close().await;
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn native_auth_observation_confirmed_cancel_preserves_real_cancel_intent() {
        tokio::time::timeout(Duration::from_secs(20), async {
            let mut case = Case::open(NativeAuthAction::SignIn {
                method: NativeAuthMethod::CodexBrowser,
            })
            .await;
            let mut protocol = protocol();
            case.observe(challenge(&mut protocol, false)).await;
            let cancelling = case
                .client
                .cancel_native_auth(NativeAuthCancel {
                    request_id: id("cancel-observed-operation"),
                    scope: case.scope.clone(),
                    operation_id: case.fact.operation_id.clone(),
                })
                .await
                .unwrap();
            assert_eq!(cancelling.fact.phase, NativeAuthPhase::Cancelling);
            protocol.cancel_login("observed-login").unwrap();
            assert!(matches!(
                case.observe(control(
                    &mut protocol,
                    json!({
                        "id":3,"result":{"status":"canceled"}
                    })
                ))
                .await,
                NativeAuthCustodyControl::Cancel
            ));
            let cancelled = case.read().await;
            assert_eq!(cancelled.fact.phase, NativeAuthPhase::Cancelled);
            assert!(cancelled.account.is_none());
            assert!(cancelled.challenge.is_none());
            assert_eq!(case.fence_count(), 1);
            assert_eq!(case.retire().await.phase, NativeAuthPhase::Cancelled);
            assert_eq!(case.fence_count(), 0);
            case.close().await;
        })
        .await
        .unwrap();
    }

    const MASTER_CATALOG: &str = r#"
timeout_ms = 2000
allowed_destinations = ["connection-a"]
[routing]
approved_accounts = ["account-a"]
task_capacity = { slots=1, cpu_millis=1000, memory_mib=512, processes=4, output_bytes=1048576, wall_time_ms=60000, spend_microunits=0 }
[model_access]
accounts = ["account-a"]
[[model_access.connections]]
id = "connection-a"
provider = "openai"
harness = "codex"
protocol = "native"
account_ref = "account-a"
endpoint = { kind = "native" }
model = { id = "model-a", author = "openai" }
auth = { method = "native_managed", profile_ref = "profile-a" }
[[model_access.pools]]
id = "pool-a"
members = ["connection-a"]
projects = ["project-a"]
roles = ["project_leader"]
fallback_allowed = false
"#;

    fn master_git(home: &std::path::Path, repository: &std::path::Path, arguments: &[&str]) {
        let output = crate::spawn_gate::output(
            ProcessCommand::new("/usr/bin/git")
                .env_clear()
                .env("PATH", "/usr/bin:/bin")
                .env("HOME", home)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_TERMINAL_PROMPT", "0")
                .args([
                    "-c",
                    "core.hooksPath=/dev/null",
                    "-c",
                    "commit.gpgsign=false",
                ])
                .args(["-c", "user.name=Signed-in master fixture"])
                .args(["-c", "user.email=signed-in-master@example.invalid"])
                .arg("-C")
                .arg(repository)
                .args(arguments)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped()),
        )
        .unwrap();
        assert!(
            output.status.success(),
            "actual Git fixture failed: {output:?}"
        );
    }

    fn master_project() -> crate::protocol::ProjectId {
        crate::protocol::ProjectId::new("project-a").unwrap()
    }

    fn master_selection() -> crate::master_session::MasterSelection {
        let target = CredentialRef::new("connection-a").unwrap();
        crate::master_session::MasterSelection {
            backend: crate::master_session::MasterBackend::Codex,
            pool_ref: CredentialRef::new("pool-a").unwrap(),
            target_profile_ref: target.clone(),
            route: crate::routing::RouteRequest {
                preferred_target: Some(target.clone()),
                required_target: Some(target),
                required_capabilities: vec![],
                account_ref: CredentialRef::new("account-a").unwrap(),
                budget_ref: CredentialRef::new("conversation-budget").unwrap(),
                resources: crate::routing::ResourceBudget::unit(),
            },
        }
    }

    fn admitted(
        result: crate::storage::MasterRuntimeResult,
    ) -> Box<crate::storage::PreparedMasterTurn> {
        match result {
            crate::storage::MasterRuntimeResult::Prepared(prepared) => {
                assert!(prepared.attempt.request.previous_attempt.is_none());
                assert_eq!(prepared.attempt.connection.id.as_str(), "connection-a");
                assert_eq!(prepared.attempt.request.project_id, master_project());
                prepared
            }
            crate::storage::MasterRuntimeResult::Turn(turn) => {
                panic!(
                    "signed-in master turn was not admitted: {:?} {:?}",
                    turn.phase, turn.reason
                )
            }
            _ => panic!("actual master preparation returned an unexpected result"),
        }
    }

    fn blocked(
        result: crate::storage::MasterRuntimeResult,
        reason: crate::master_session::MasterReason,
    ) {
        let crate::storage::MasterRuntimeResult::Turn(turn) = result else {
            panic!("actual master turn was admitted while it had to be blocked");
        };
        assert_eq!(turn.phase, crate::master_session::MasterTurnPhase::Blocked);
        assert_eq!(turn.reason, Some(reason));
    }

    impl Case {
        async fn owner_state(&self, query: OwnerRead) -> crate::engine::owner::OwnerReadReply {
            let reply = self
                .client
                .request(OwnerRequest {
                    protocol_version: PROTOCOL_VERSION,
                    request_id: id("signed-in-master-read"),
                    expected_revision: Cursor::new(0),
                    operation: OwnerOperation::Read { query },
                })
                .await
                .unwrap();
            let OwnerResponse::Read { state } = reply.response else {
                panic!("actual owner read failed")
            };
            state
        }

        async fn owner_apply(
            &self,
            request_id: &str,
            mutation: crate::engine::owner::OwnerMutation,
        ) {
            let host = self.service.owner_runtime().unwrap();
            let revision = self.owner_state(OwnerRead::Capabilities).await.revision;
            let request = OwnerRequest {
                protocol_version: PROTOCOL_VERSION,
                request_id: id(request_id),
                expected_revision: revision,
                operation: OwnerOperation::Apply { mutation },
            };
            request.validate().unwrap();
            let mut packet = [0; 37];
            packet[..4].copy_from_slice(b"CAO1");
            packet[5..].copy_from_slice(&self.client.token);
            let admission = host.authority.authenticate(&packet).unwrap();
            let operation = host
                .engine
                .owner_job(|reply| Job::OwnerApply(admission, request, reply))
                .await
                .unwrap();
            assert_eq!(
                operation.result,
                crate::engine::owner::OwnerResult::Applied,
                "{request_id}"
            );
        }

        async fn model_access_basis(&self) -> (crate::protocol::ConfigId, String) {
            let state = self
                .owner_state(OwnerRead::ModelAccessPage {
                    collection: ModelAccessCollection::Pools,
                    cursor: None,
                    limit: 16,
                })
                .await;
            let OwnerData::ModelAccessPage { source_hash, .. } = state.data else {
                panic!("actual model source basis missing")
            };
            (state.config_id, source_hash)
        }

        async fn edit_settings(&self, request_id: &str, edit: crate::config::ModelAccessEdit) {
            let (config_id, source_hash) = self.model_access_basis().await;
            self.owner_apply(
                request_id,
                crate::engine::owner::OwnerMutation::ModelAccess {
                    expected_config_id: config_id.clone(),
                    expected_source_hash: source_hash,
                    edit,
                },
            )
            .await;
            assert_ne!(self.model_access_basis().await.0, config_id);
        }

        fn configured_connection(&self) -> crate::config::ModelConnection {
            let config = crate::config::resolve_config(
                &self.root.path().join("g.toml"),
                &self.root.path().join("p.toml"),
            )
            .unwrap();
            config.settings.model_access.unwrap().connections[0].clone()
        }

        async fn complete_sign_in(&mut self) {
            let mut protocol = protocol();
            self.observe(challenge(&mut protocol, false)).await;
            self.observe(control(
                &mut protocol,
                json!({"method":"account/login/completed",
                "params":{"loginId":"observed-login","success":true}}),
            ))
            .await;
            protocol.read_account(false).unwrap();
            self.observe(control(
                &mut protocol,
                json!({"id":3,"result":{
                    "account":{"type":"chatgpt","email":EMAIL,"planType":"plus"},
                    "requiresOpenaiAuth":true
                }}),
            ))
            .await;
            assert_eq!(
                self.retire().await.phase,
                NativeAuthPhase::AuthenticatedObserved
            );
            assert_eq!(self.fence_count(), 0);
        }

        async fn open_master(&self) {
            let root = self.root.path();
            let repository = root.join("repository");
            std::fs::create_dir(&repository).unwrap();
            std::fs::set_permissions(&repository, std::fs::Permissions::from_mode(0o700)).unwrap();
            master_git(root, &repository, &["init", "--initial-branch=trunk"]);
            std::fs::write(
                repository.join("README"),
                b"Actual signed-in master workspace.\n",
            )
            .unwrap();
            master_git(root, &repository, &["add", "README"]);
            master_git(
                root,
                &repository,
                &["commit", "-qm", "Initialize signed-in master fixture"],
            );
            self.owner_apply(
                "signed-in-master-project",
                crate::engine::owner::OwnerMutation::CreateProject {
                    project_id: master_project(),
                    spec: crate::protocol::ProjectSpec {
                        name: "Signed-in master".into(),
                        repositories: vec![repository.to_str().unwrap().into()],
                    },
                },
            )
            .await;
            let (config_id, source_hash) = self.model_access_basis().await;
            self.owner_apply(
                "signed-in-master-open",
                crate::engine::owner::OwnerMutation::ProjectMaster {
                    action: crate::master_session::MasterOwnerAction::Open {
                        project_id: master_project(),
                        expected_config_id: config_id,
                        expected_source_hash: source_hash,
                        selection: Box::new(master_selection()),
                    },
                },
            )
            .await;
        }

        async fn prepare_next_turn(&self, request_id: &str) -> crate::storage::MasterRuntimeResult {
            let state = self
                .owner_state(OwnerRead::ProjectMaster {
                    project_id: master_project(),
                })
                .await;
            let OwnerData::ProjectMaster {
                session: Some(session),
            } = state.data
            else {
                panic!("actual master session missing")
            };
            let session = *session;
            self.owner_apply(
                request_id,
                crate::engine::owner::OwnerMutation::ProjectMaster {
                    action: crate::master_session::MasterOwnerAction::Submit {
                        project_id: session.project_id.clone(),
                        session_id: session.session_id.clone(),
                        text: "Describe the project without using tools.".into(),
                    },
                },
            )
            .await;
            let host = self.service.owner_runtime().unwrap();
            host.prepare_master_turn(&session.project_id, &session.session_id, &id(request_id))
                .await
                .unwrap()
        }
    }

    #[tokio::test]
    async fn signed_in_master_survives_an_unrelated_settings_edit() {
        tokio::time::timeout(Duration::from_secs(120), async {
            let mut case = Case::open_with(
                MASTER_CATALOG,
                NativeAuthAction::SignIn {
                    method: NativeAuthMethod::CodexBrowser,
                },
            )
            .await;
            case.complete_sign_in().await;
            case.edit_settings(
                "rename-master-pool",
                crate::config::ModelAccessEdit::ReplacePool {
                    pool: crate::config::AccountPool {
                        id: CredentialRef::new("pool-a").unwrap(),
                        name: Some("Daily work".into()),
                        members: vec![CredentialRef::new("connection-a").unwrap()],
                        projects: vec![CredentialRef::new("project-a").unwrap()],
                        roles: vec![CredentialRef::new("project_leader").unwrap()],
                        fallback_allowed: false,
                        provider_quota_policy: crate::config::ProviderQuotaPolicy::default(),
                    },
                },
            )
            .await;
            case.open_master().await;
            let prepared = admitted(case.prepare_next_turn("first-signed-in-turn").await);
            let host = case.service.owner_runtime().unwrap();
            let profile = host
                .acquire_master_profile(&prepared, &id("first-signed-in-turn"))
                .await
                .unwrap();
            if let Err(error) = host.prepare_master_host(&prepared, &profile) {
                assert_ne!(error.0, "master_runtime_binding");
            }
            drop(profile);
            case.close().await;
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn signed_in_master_is_refused_after_its_connection_changes() {
        tokio::time::timeout(Duration::from_secs(120), async {
            let mut case = Case::open_with(
                MASTER_CATALOG,
                NativeAuthAction::SignIn {
                    method: NativeAuthMethod::CodexBrowser,
                },
            )
            .await;
            case.complete_sign_in().await;
            let mut connection = case.configured_connection();
            connection.model.id = "model-b".into();
            case.edit_settings(
                "change-master-model",
                crate::config::ModelAccessEdit::ReplaceConnection { connection },
            )
            .await;
            case.open_master().await;
            blocked(
                case.prepare_next_turn("turn-after-connection-change").await,
                crate::master_session::MasterReason::ReadinessStale,
            );
            case.close().await;
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn blocked_first_master_turn_does_not_lock_out_the_turn_after_sign_in() {
        tokio::time::timeout(Duration::from_secs(120), async {
            let mut case = Case::open_with(
                MASTER_CATALOG,
                NativeAuthAction::SignIn {
                    method: NativeAuthMethod::CodexBrowser,
                },
            )
            .await;
            case.open_master().await;
            blocked(
                case.prepare_next_turn("turn-before-sign-in").await,
                crate::master_session::MasterReason::ReadinessStale,
            );
            case.complete_sign_in().await;
            admitted(case.prepare_next_turn("turn-after-sign-in").await);
            case.close().await;
        })
        .await
        .unwrap();
    }
}
