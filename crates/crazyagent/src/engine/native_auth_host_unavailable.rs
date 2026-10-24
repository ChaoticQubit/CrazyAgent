use crate::engine::EngineError;
use std::path::Path;

#[path = "native_auth_host/claude_auth/types.rs"]
pub(crate) mod claude_auth_types;

pub(in crate::engine) mod claude_auth {
    pub(crate) use super::claude_auth_types as types;
}

/// A registered native sign-in host executable, which cannot exist without the Unix
/// owner, mode and inode checks that pin it. `capture` reports `native_auth_host_unavailable`,
/// so every method below is unreachable by construction.
pub(crate) enum RegisteredNativeAuthHost {}

impl RegisteredNativeAuthHost {
    /// Refuses to pin a native sign-in host executable on this platform.
    /// Takes the executable path, unused. Returns `native_auth_host_unavailable`.
    pub(crate) fn capture(_path: &Path) -> Result<Self, EngineError> {
        Err(EngineError("native_auth_host_unavailable".into()))
    }

    pub(crate) fn verified_path(&self) -> Result<&Path, EngineError> {
        match *self {}
    }

    pub(crate) fn verified_sha256(&self) -> Result<String, EngineError> {
        match *self {}
    }
}

/// Refuses to run native sign-in custody, which needs Unix process groups and fd handoff.
/// Takes the queued work and its dispatch host, both dropped unused.
/// Returns `native_auth_host_unavailable`.
pub(in crate::engine) async fn dispatch(
    _work: crate::storage::NativeAuthWork,
    _host: crate::engine::owner::transport::NativeAuthDispatchHost,
) -> Result<(), EngineError> {
    Err(EngineError("native_auth_host_unavailable".into()))
}

pub(super) mod custody {
    #[cfg(unix)]
    use crate::engine::EngineError;
    #[cfg(unix)]
    use crate::engine::bootstrap::NativeProfileUsePurpose;
    #[cfg(unix)]
    use crate::engine::owner::{HostStamp, NativeProfileBinding};
    #[cfg(unix)]
    use crate::protocol::RequestId;
    use std::io;
    use std::process::{Child, ExitStatus};
    use std::thread;
    use std::time::{Duration, Instant};

    const POLL_INTERVAL: Duration = Duration::from_millis(5);
    const DROP_TIME: Duration = Duration::from_secs(2);

    /// A sealed native sign-in custody session, which cannot exist without native profile
    /// custody. Every method below is unreachable by construction.
    #[cfg(unix)]
    pub(in crate::engine) enum Retirement {}

    #[cfg(unix)]
    impl Retirement {
        #[cfg(unix)]
        pub(in crate::engine) fn revalidate(
            &self,
            _host: &HostStamp,
            _binding: &NativeProfileBinding,
            _operation_id: &RequestId,
            _generation: u64,
            _purpose: NativeProfileUsePurpose,
        ) -> Result<(), EngineError> {
            match *self {}
        }

        pub(in crate::engine) fn revalidate_after_child_reaped(
            &self,
            _host: &HostStamp,
            _binding: &NativeProfileBinding,
            _operation_id: &RequestId,
            _generation: u64,
            _purpose: NativeProfileUsePurpose,
        ) -> Result<(), EngineError> {
            match *self {}
        }
    }

    fn sleep_until(deadline: Instant) {
        let remaining = deadline.saturating_duration_since(Instant::now());
        thread::sleep(POLL_INTERVAL.min(remaining));
    }

    /// Kills one direct child and waits for it until the deadline, at most two seconds,
    /// exactly as on Unix. Takes the child and deadline. Returns its exit status, none
    /// when it is still running at the deadline, or the kill error.
    pub(in crate::engine) fn cleanup_direct_child(
        child: &mut Child,
        deadline: Instant,
    ) -> io::Result<Option<ExitStatus>> {
        if let Some(status) = child.try_wait()? {
            return Ok(Some(status));
        }
        let started = Instant::now();
        if started >= deadline {
            return Ok(None);
        }
        let deadline = deadline.min(started + DROP_TIME);
        assert!(deadline > started);
        assert!(deadline.duration_since(started) <= DROP_TIME);
        if let Err(error) = child.kill() {
            return match child.try_wait()? {
                Some(status) => Ok(Some(status)),
                None => Err(error),
            };
        }
        loop {
            if let Some(status) = child.try_wait()? {
                return Ok(Some(status));
            }
            if Instant::now() >= deadline {
                return Ok(None);
            }
            sleep_until(deadline);
        }
    }
}
