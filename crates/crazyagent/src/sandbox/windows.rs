use super::{Sandbox, SandboxError};
use std::process::Command;

/// Why every Windows launch is refused for now, in plain words.
pub const APPCONTAINER_MISSING: &str = "Windows agents need an AppContainer boundary, which this build does not have yet, so no agent starts unconfined";

/// The Windows boundary for project agents. It refuses every launch as unavailable until
/// the AppContainer launch with a job object exists: AppContainer needs Win32 calls that
/// are unsafe in Rust, the crate forbids unsafe code, and no agent may start without a
/// boundary.
#[derive(Debug, Default)]
pub struct WindowsSandbox;

impl Sandbox for WindowsSandbox {
    fn wrap(&self, _harness: &Command) -> Result<Command, SandboxError> {
        Err(SandboxError::Unavailable(APPCONTAINER_MISSING.into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_refuses_every_launch_until_the_appcontainer_boundary_exists() {
        let seam: &dyn Sandbox = &WindowsSandbox;
        let mut harness = Command::new(r"C:\Windows\System32\cmd.exe");
        harness.arg("/C").arg("exit 0");
        match seam.wrap(&harness) {
            Err(SandboxError::Unavailable(reason)) => assert_eq!(reason, APPCONTAINER_MISSING),
            other => panic!("expected an unavailable boundary, got {other:?}"),
        }
    }
}
