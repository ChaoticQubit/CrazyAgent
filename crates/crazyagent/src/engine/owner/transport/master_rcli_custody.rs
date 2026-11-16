use super::{EngineError, cleanup_uncertain, failure};
use crate::engine::bootstrap::NativeProfileUse;
use crate::protocol::RequestId;
use crate::rcli_host::{RcliMasterHost, RcliMasterLaunch, RcliMasterObservation};
use crate::sandbox::InstalledSandbox;
use std::time::Duration;

pub(super) struct RcliCustody {
    host: Option<RcliMasterHost>,
    profile: Option<NativeProfileUse>,
    uncertain: bool,
}

impl RcliCustody {
    /// Starts the rcli master host and keeps it with its profile. Inputs: the launch, the
    /// sandbox, the profile in use and the host's run clock, none for no master time limit.
    /// Output: the custody, or `master_runtime_cleanup_uncertain` with the host's refusal.
    pub(super) fn launch(
        launch: RcliMasterLaunch,
        sandbox: InstalledSandbox,
        profile: NativeProfileUse,
        run_time: Option<Duration>,
    ) -> Result<Self, EngineError> {
        let mut custody = Self {
            host: None,
            profile: Some(profile),
            uncertain: false,
        };
        match RcliMasterHost::launch(launch, &sandbox, run_time) {
            Ok(host) => {
                custody.host = Some(host);
                assert!(custody.profile.is_some());
                Ok(custody)
            }
            Err(error) => {
                custody.uncertain = true;
                Err(EngineError(format!(
                    "master_runtime_cleanup_uncertain:{}",
                    error.0
                )))
            }
        }
    }

    pub(super) fn profile(&self) -> &NativeProfileUse {
        self.profile
            .as_ref()
            .expect("retained RCLI execution custody")
    }

    /// Names the process group of the RCLI master in custody. Input: the custody.
    /// Output: the group, or none once the host is gone.
    pub(super) fn process_group(&self) -> Option<u32> {
        self.host.as_ref().map(RcliMasterHost::process_group)
    }

    pub(super) fn poll(&mut self) -> Result<Option<RcliMasterObservation>, EngineError> {
        let observed = self.host.as_mut().expect("retained RCLI host").poll();
        observed.map_err(|error| self.observe_error(EngineError(error.0)))
    }

    pub(super) fn prompt(&mut self, turn_id: RequestId, text: &str) -> Result<(), EngineError> {
        let result = self
            .host
            .as_mut()
            .expect("retained RCLI host")
            .prompt(turn_id, text);
        result.map_err(|error| self.observe_error(EngineError(error.0)))
    }

    fn observe_error(&mut self, error: EngineError) -> EngineError {
        self.uncertain |= cleanup_uncertain(&error);
        error
    }

    pub(super) fn retire(&mut self, cancel: Option<&RequestId>) -> Result<(), EngineError> {
        if self.uncertain {
            return Err(failure(
                "master_runtime_cleanup_uncertain:rcli_master_cleanup_incomplete",
            ));
        }
        let host = self.host.as_mut().expect("retained RCLI host");
        if let Some(turn_id) = cancel
            && let Err(error) = host.cancel(turn_id)
            && cleanup_uncertain(&EngineError(error.0.clone()))
        {
            self.uncertain = true;
            return Err(EngineError(format!(
                "master_runtime_cleanup_uncertain:{}",
                error.0
            )));
        }
        self.shut_down()?;
        self.host.take();
        assert!(!self.uncertain);
        assert!(self.host.is_none());
        Ok(())
    }
}

impl RcliCustody {
    /// Shuts the host down. What its stop left running once its group ended
    /// (LINUX-KILL-GROUP r1 F1) is held with the profile, as a Claude host turn's hold keeps
    /// its lease, so the account goes once none run, and the turn ends with that code; any
    /// other failure keeps both as in doubt. Input: the custody. Output: Ok, the leftover
    /// code, or `master_runtime_cleanup_uncertain` with the host's refusal.
    fn shut_down(&mut self) -> Result<(), EngineError> {
        let host = self.host.as_mut().expect("retained RCLI host");
        let shut = host.shutdown();
        if let Some(left) = host.left_code() {
            let profile = self
                .profile
                .take()
                .expect("retained RCLI execution custody");
            let account = profile.account().clone();
            host.hold_leftovers(Some(account), profile);
            assert!(host.left_code().is_none());
            return Err(failure(left));
        }
        shut.map_err(|error| {
            self.uncertain = true;
            EngineError(format!("master_runtime_cleanup_uncertain:{}", error.0))
        })
    }
}

impl Drop for RcliCustody {
    fn drop(&mut self) {
        if !self.uncertain && self.host.is_some() {
            let _ = self.shut_down();
        }
        if self.uncertain {
            if let Some(host) = self.host.as_mut() {
                host.hold_leftovers(None, ());
            }
            let retained = (self.host.take(), self.profile.take());
            std::mem::forget(retained);
        }
    }
}
