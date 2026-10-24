use super::*;
use crate::engine::owner::OwnerClient;
use crate::setup::SetupStatus;

impl OwnerController {
    fn setup_client(&self) -> Result<&OwnerClient, EngineError> {
        self.client
            .as_ref()
            .ok_or_else(|| EngineError("controller_disconnected".into()))
    }

    /// Reads the setup screen: each dependency with its state, each default-pack entry
    /// with its source, license and state, and whether setup is running.
    pub async fn setup_status(&self) -> Result<SetupStatus, EngineError> {
        self.setup_client()?.setup_status().await
    }

    /// Starts setup with the ticked default-pack entries; continuing past the ticked MCP
    /// servers records the owner's approval of them. Input: the ticked entry ids.
    /// Output: the status as the run starts.
    pub async fn run_setup(&self, pack: Vec<String>) -> Result<SetupStatus, EngineError> {
        self.setup_client()?.run_setup(pack).await
    }

    /// Switches one installed default-pack entry on or off. Inputs: its id and the state.
    pub async fn set_pack_entry(&self, id: String, on: bool) -> Result<SetupStatus, EngineError> {
        self.setup_client()?.set_pack_entry(id, on).await
    }

    /// Removes one installed default-pack entry and everything it added. Input: its id.
    pub async fn remove_pack_entry(&self, id: String) -> Result<SetupStatus, EngineError> {
        self.setup_client()?.remove_pack_entry(id).await
    }
}
