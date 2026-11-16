use crate::connectors::gateway::GatewayLaunch;
use crate::connectors::settings::NativeConnectors;
use crate::connectors::shim;
use crate::engine::EngineError;
use crate::plugins::{LaunchServer, ServerSpec};
use std::path::Path;
use std::time::Duration;

/// The name of the one server entry a launch with a granted connector gets.
pub const SERVER_NAME: &str = "ca-connectors";
/// How long a harness waits for one call to the connector server: past the longest
/// approval wait and the connector server's own wait for a held write, so the owner's
/// answer reaches the agent before its harness gives up.
pub const HARNESS_WAIT: Duration = Duration::from_secs(40 * 60);
/// How long a harness waits for the connector server to start and answer its handshake.
pub const STARTUP_WAIT: Duration = Duration::from_secs(30);

const _: () = assert!(HARNESS_WAIT.as_secs() > shim::HELD_WAIT.as_secs());
const _: () = assert!(STARTUP_WAIT.as_secs() >= 10);

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
    let plain = |path: &Path| {
        path.is_absolute()
            .then(|| path.to_str())
            .flatten()
            .filter(|text| !text.chars().any(char::is_control))
            .map(str::to_owned)
    };
    let (Some(command), Some(folder)) = (plain(executable), plain(&launch.folder)) else {
        return Err(EngineError("connector_request_invalid".into()));
    };
    Ok(LaunchServer {
        name: SERVER_NAME.into(),
        spec: ServerSpec::Stdio {
            command,
            args: vec![shim::SERVER_COMMAND.into(), folder],
        },
        secrets: Vec::new(),
    })
}

/// The switch one harness gets for the owner's Native setting: Off turns Claude Code's
/// claude.ai connectors or Codex's apps off; On, and any other harness, leave them as the
/// harness has them. Inputs: the harness id and the setting. Output: the switch.
pub fn native_switch(harness: &str, native: NativeConnectors) -> NativeSwitch {
    if native != NativeConnectors::Off {
        return NativeSwitch::default();
    }
    NativeSwitch {
        claude_ai_off: harness == "claude-code",
        codex_apps_off: harness == "codex",
    }
}

#[cfg(all(test, unix))]
#[path = "launch_tests.rs"]
mod tests;
