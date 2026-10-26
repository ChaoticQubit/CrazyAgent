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
                eprintln!("native_auth_reproof_deferred cause={}", error.0);
                return;
            }
        };
        for start in starts {
            if !self.reprovable(start.method) {
                continue;
            }
            if let Err(error) = self.reprove(&admission, start).await {
                eprintln!("native_auth_reproof_unsettled cause={}", error.0);
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

    async fn reprove(
        &self,
        admission: &Admission,
        start: NativeAuthStart,
    ) -> Result<NativeAuthPhase, EngineError> {
        assert!(matches!(
            start.method,
            NativeAuthMethod::ClaudeExisting | NativeAuthMethod::CodexExisting
        ));
        let query = NativeAuthQuery {
            request_id: RequestId::new(random_id()?)
                .map_err(|_| failure("owner_identity_encoding"))?,
            scope: start.scope.clone(),
            operation_id: start.request_id.clone(),
        };
        let deadline = Instant::now() + REPROOF_WAIT;
        let mut reply = self
            .native_auth_command(admission.clone(), Command::Start(start))
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
        Ok(reply.fact.phase)
    }
}

impl Authority {
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
