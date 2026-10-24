use super::{BridgeError, NativeBridge, failure};
use crazyagent::engine::EngineError;
use crazyagent::setup::SetupStatus;

const CODES: [&str; 9] = [
    "setup_busy",
    "setup_readonly",
    "setup_store_unavailable",
    "setup_owner_unavailable",
    "setup_outcome_unknown",
    "pack_unknown",
    "pack_required",
    "pack_not_installed",
    "pack_name_taken",
];

fn setup_failure(error: EngineError) -> BridgeError {
    failure(
        CODES
            .iter()
            .find(|code| **code == error.0)
            .copied()
            .unwrap_or("setup_failed"),
    )
}

impl NativeBridge {
    fn setup_ready(&self) -> Result<(), BridgeError> {
        if !self.status().connected {
            return Err(failure("not_connected"));
        }
        Ok(())
    }

    /// Reads the setup screen: tmux, uv and Graphify with their states, each default-pack
    /// entry with its source, license and state, and whether setup is running.
    pub async fn setup_status(&mut self) -> Result<SetupStatus, BridgeError> {
        self.setup_ready()?;
        self.controller()?
            .setup_status()
            .await
            .map_err(setup_failure)
    }

    /// Starts setup with the ticked default-pack entries; ticked MCP servers count as
    /// approved by the owner. Input: the ticked entry ids. Output: the starting status.
    pub async fn run_setup(&mut self, pack: Vec<String>) -> Result<SetupStatus, BridgeError> {
        self.setup_ready()?;
        self.controller()?
            .run_setup(pack)
            .await
            .map_err(setup_failure)
    }

    /// Switches one installed default-pack entry on or off. Inputs: its id and the state.
    pub async fn set_pack_entry(
        &mut self,
        id: String,
        on: bool,
    ) -> Result<SetupStatus, BridgeError> {
        self.setup_ready()?;
        self.controller()?
            .set_pack_entry(id, on)
            .await
            .map_err(setup_failure)
    }

    /// Removes one installed default-pack entry and everything it added. Input: its id.
    pub async fn remove_pack_entry(&mut self, id: String) -> Result<SetupStatus, BridgeError> {
        self.setup_ready()?;
        self.controller()?
            .remove_pack_entry(id)
            .await
            .map_err(setup_failure)
    }
}
