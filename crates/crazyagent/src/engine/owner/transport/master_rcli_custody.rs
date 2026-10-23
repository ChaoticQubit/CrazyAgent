use super::{EngineError, cleanup_uncertain, failure};
use crate::engine::bootstrap::NativeProfileUse;
use crate::protocol::RequestId;
use crate::rcli_host::{RcliMasterHost, RcliMasterLaunch, RcliMasterObservation};

pub(super) struct RcliCustody {
    host: Option<RcliMasterHost>,
    profile: Option<NativeProfileUse>,
    uncertain: bool,
}

impl RcliCustody {
    pub(super) fn launch(
        launch: RcliMasterLaunch,
        profile: NativeProfileUse,
    ) -> Result<Self, EngineError> {
        let mut custody = Self { host: None, profile: Some(profile), uncertain: false };
        match RcliMasterHost::launch(launch) {
            Ok(host) => {
                custody.host = Some(host);
                assert!(custody.profile.is_some());
                Ok(custody)
            }
            Err(error) => {
                custody.uncertain = true;
                Err(EngineError(format!("master_runtime_cleanup_uncertain:{}", error.0)))
            }
        }
    }

    pub(super) fn profile(&self) -> &NativeProfileUse {
        self.profile.as_ref().expect("retained RCLI execution custody")
    }

    pub(super) fn poll(&mut self) -> Result<Option<RcliMasterObservation>, EngineError> {
        let observed = self.host.as_mut().expect("retained RCLI host").poll();
        observed.map_err(|error| self.observe_error(EngineError(error.0)))
    }

    pub(super) fn prompt(&mut self, turn_id: RequestId, text: &str) -> Result<(), EngineError> {
        let result = self.host.as_mut().expect("retained RCLI host").prompt(turn_id, text);
        result.map_err(|error| self.observe_error(EngineError(error.0)))
    }

    fn observe_error(&mut self, error: EngineError) -> EngineError {
        self.uncertain |= cleanup_uncertain(&error);
        error
    }

    pub(super) fn retire(&mut self, cancel: Option<&RequestId>) -> Result<(), EngineError> {
        if self.uncertain {
            return Err(failure("master_runtime_cleanup_uncertain:rcli_master_cleanup_incomplete"));
        }
        let host = self.host.as_mut().expect("retained RCLI host");
        if let Some(turn_id) = cancel
            && let Err(error) = host.cancel(turn_id)
            && cleanup_uncertain(&EngineError(error.0.clone()))
        {
            self.uncertain = true;
            return Err(EngineError(format!("master_runtime_cleanup_uncertain:{}", error.0)));
        }
        if let Err(error) = host.shutdown() {
            self.uncertain = true;
            return Err(EngineError(format!("master_runtime_cleanup_uncertain:{}", error.0)));
        }
        self.host.take();
        assert!(!self.uncertain);
        assert!(self.host.is_none());
        Ok(())
    }
}

impl Drop for RcliCustody {
    fn drop(&mut self) {
        if !self.uncertain
            && let Some(host) = self.host.as_mut()
        {
            self.uncertain = host.shutdown().is_err();
        }
        if self.uncertain {
            let retained = (self.host.take(), self.profile.take());
            std::mem::forget(retained);
        }
    }
}
