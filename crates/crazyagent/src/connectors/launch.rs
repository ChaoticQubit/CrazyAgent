use crate::connectors::gateway::GatewayLaunch;
use crate::connectors::settings::NativeConnectors;
use crate::engine::EngineError;
use crate::plugins::{LaunchServer, ServerSpec};
use std::path::Path;

/// The name of the one server entry a launch with a granted connector gets.
pub const SERVER_NAME: &str = "ca-connectors";

/// What one launch tells its harness about the connectors of the harness's own account:
/// whether Claude Code's claude.ai connectors and Codex's apps are turned off. Both false
/// leave the harness as it is.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct NativeSwitch {
    pub claude_ai_off: bool,
    pub codex_apps_off: bool,
}

/// The server entry that starts a launch's connector server: the engine executable given
/// `connector-mcp` and the launch folder, with no secret and no environment. Inputs: the
/// absolute engine executable and the launch. Output: the entry, or
/// `connector_request_invalid` for a path that is not absolute plain text.
pub fn entry(executable: &Path, launch: &GatewayLaunch) -> Result<LaunchServer, EngineError> {
    let _ = (executable, launch);
    Ok(LaunchServer {
        name: SERVER_NAME.into(),
        spec: ServerSpec::Stdio {
            command: String::new(),
            args: Vec::new(),
        },
        secrets: Vec::new(),
    })
}

/// The switch one harness gets for the owner's Native setting: Off turns Claude Code's
/// claude.ai connectors or Codex's apps off; On, and any other harness, leave them as the
/// harness has them. Inputs: the harness id and the setting. Output: the switch.
pub fn native_switch(harness: &str, native: NativeConnectors) -> NativeSwitch {
    let _ = (harness, native);
    NativeSwitch::default()
}

#[cfg(all(test, unix))]
#[path = "launch_tests.rs"]
mod tests;
