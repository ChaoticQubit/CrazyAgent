use super::*;
use crate::engine::owner::native_auth::Command;
use crate::engine::owner::{NativeAuthMethod, NativeAuthQuery, NativeAuthStart, random_id};
use crate::protocol::SessionId;

const REPROOF_WAIT: Duration = Duration::from_secs(20);
const REPROOF_POLL: Duration = Duration::from_millis(100);

impl OwnerRuntimeHandle {
    /// Re-proves, one at a time, each native sign-in an earlier engine run proved, with the
    /// official non-interactive account check in its profile under engine custody, so a
    /// restart keeps an owner signed in. Input: none for every connection, or one master
    /// session for its connection only. Output: none; every check settles in storage, and a
    /// check that cannot run is logged by code.
    pub(in crate::engine) async fn reprove_sign_ins(&self, session: Option<SessionId>) {
        let _serial = self.authority.reproof.lock().await;
        let Some(admission) = self.authority.standing_admission() else {
            return;
        };
        let starts = match self
            .engine
            .native_auth_job(|reply| Job::NativeAuthStale(self.identity(), session, reply))
            .await
        {
            Ok(starts) => starts,
            Err(error) => {
                eprintln!(
                    "native_auth_reproof_deferred cause={}",
                    plain_cause(&error, "native_auth_reproof_error_redacted")
                );
                return;
            }
        };
        for start in starts {
            if !self.reprovable(start.method) {
                continue;
            }
            if let Err(error) = self.reprove(&admission, start).await {
                eprintln!(
                    "native_auth_reproof_unsettled cause={}",
                    plain_cause(&error, "native_auth_reproof_error_redacted")
                );
            }
        }
    }

    /// Starts the engine-start re-proof of every earlier sign-in as its own task, after
    /// every executable is registered. Input: none. Output: the task, which the engine
    /// aborts when it stops.
    pub(in crate::engine) fn reprove_in_background(&self) -> tokio::task::JoinHandle<()> {
        let handle = OwnerRuntimeHandle {
            engine: self.engine.clone(),
            authority: self.authority.clone(),
        };
        tokio::spawn(async move { handle.reprove_sign_ins(None).await })
    }

    fn reprovable(&self, method: NativeAuthMethod) -> bool {
        let Ok(registration) = self.authority.native_auth_host.lock() else {
            return false;
        };
        !registration.disabled
            && registration.executable.is_some()
            && match method {
                NativeAuthMethod::ClaudeExisting => registration.claude_executable.is_some(),
                NativeAuthMethod::CodexExisting => registration.codex_executable.is_some(),
                _ => false,
            }
    }

    /// Re-proves one earlier sign-in, waiting for the account-check host to be free first
    /// and again until the finished check has released the profile, and checking again when
    /// the host was busy or could not run the check or the profile was still held, all
    /// within one bound (LB-26, LB-30). Inputs: the owner's standing admission and the
    /// check. Output: the settled phase, or the code.
    async fn reprove(
        &self,
        admission: &Admission,
        start: NativeAuthStart,
    ) -> Result<NativeAuthPhase, EngineError> {
        assert!(matches!(
            start.method,
            NativeAuthMethod::ClaudeExisting | NativeAuthMethod::CodexExisting
        ));
        let deadline = Instant::now() + REPROOF_WAIT;
        let mut attempt = start;
        loop {
            self.native_auth_idle_by(deadline).await?;
            let settled = self.check_once(admission, &attempt, deadline).await;
            if !unrun(&settled) {
                if settled.is_ok() {
                    self.native_auth_idle_by(deadline).await?;
                }
                return settled;
            }
            if Instant::now() >= deadline || !self.reprovable(attempt.method) {
                return settled;
            }
            tokio::time::sleep(REPROOF_POLL).await;
            attempt = NativeAuthStart {
                request_id: RequestId::new(random_id()?)
                    .map_err(|_| failure("owner_identity_encoding"))?,
                ..attempt
            };
        }
    }

    /// Starts one account check and follows it until it settles or the bound passes.
    /// Inputs: the admission, the check and the bound. Output: the settled phase, with
    /// `native_auth_host_unavailable` mapped to an error, or the refusal code.
    async fn check_once(
        &self,
        admission: &Admission,
        start: &NativeAuthStart,
        deadline: Instant,
    ) -> Result<NativeAuthPhase, EngineError> {
        let query = NativeAuthQuery {
            request_id: RequestId::new(random_id()?)
                .map_err(|_| failure("owner_identity_encoding"))?,
            scope: start.scope.clone(),
            operation_id: start.request_id.clone(),
        };
        let mut reply = self
            .native_auth_command(admission.clone(), Command::Start(start.clone()))
            .await?;
        while reply.fact.phase.unfinished() {
            if Instant::now() >= deadline {
                return Err(failure("native_auth_reproof_deadline"));
            }
            tokio::time::sleep(REPROOF_POLL).await;
            reply = self
                .native_auth_command(admission.clone(), Command::Read(query.clone()))
                .await?;
        }
        assert_eq!(reply.fact.operation_id, query.operation_id);
        if reply.fact.phase == NativeAuthPhase::Unavailable
            && reply.fact.code.as_deref() == Some("native_auth_host_unavailable")
        {
            return Err(failure("native_auth_host_unavailable"));
        }
        Ok(reply.fact.phase)
    }

    /// Waits until the account-check host holds no operation but the finished one it keeps
    /// to show. Input: the bound. Output: success, or `native_auth_reproof_deadline`.
    async fn native_auth_idle_by(&self, deadline: Instant) -> Result<(), EngineError> {
        while !self.authority.native_auth_idle() {
            if Instant::now() >= deadline {
                return Err(failure("native_auth_reproof_deadline"));
            }
            tokio::time::sleep(REPROOF_POLL).await;
        }
        Ok(())
    }
}

/// Tells whether a check failed only because the host was busy or could not run it, or
/// another holder still had the profile, which says nothing about the account (LB-30).
/// Input: the check's result. Output: true to check again.
fn unrun(settled: &Result<NativeAuthPhase, EngineError>) -> bool {
    settled.as_ref().is_err_and(|error| {
        matches!(
            error.0.as_str(),
            "native_auth_host_busy" | "native_auth_host_unavailable" | "native_profile_in_use"
        )
    })
}

impl Authority {
    /// Tells whether the account-check host can take a new operation: it holds none, or
    /// only the finished one it keeps to show. Input: none. Output: true when idle.
    fn native_auth_idle(&self) -> bool {
        self.native_auth
            .lock()
            .is_ok_and(|current| current.as_ref().is_none_or(NativeAuthSlot::cached))
    }

    /// The admission of the first owner registration this engine run holds active: the
    /// owner's standing grant an engine-started account check runs under. Input: none.
    /// Output: that admission, or none when no owner is registered.
    fn standing_admission(&self) -> Option<Admission> {
        let slots = self.slots.lock().ok()?;
        slots
            .iter()
            .find(|slot| slot.registration.active)
            .map(|slot| slot.registration.admission.clone())
    }
}
