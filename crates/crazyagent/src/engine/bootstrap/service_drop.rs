use super::*;

impl Drop for EngineService {
    /// Stops native sign-in dispatch and reaps its custodian child when the service is
    /// dropped without a completed shutdown, so no custodian outlives its engine.
    /// Inputs: the service being dropped. Output: none; an unconfirmed cleanup is logged.
    fn drop(&mut self) {
        if let Some(owner) = self.owner_runtime.as_ref() {
            owner.abort_narrowing_drain();
        }
        if let Some(owner) = self.owner_runtime.as_ref()
            && let Err(error) = owner.invalidate_native_auth_dispatch()
        {
            eprintln!(
                "native_auth_dispatch_drop_cleanup_uncertain cause={}",
                error.0
            );
        }
    }
}
