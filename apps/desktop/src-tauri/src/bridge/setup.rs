use super::{BridgeError, IntentReceipt, NativeBridge, failure};
use crazyagent::engine::EngineError;
use crazyagent::engine::owner::OwnerMutation;
use crazyagent::setup::SetupStatus;

const CODES: [&str; 3] = [
    "setup_busy",
    "setup_store_unavailable",
    "setup_outcome_unknown",
];

fn setup_failure(error: EngineError) -> BridgeError {
    failure(
        CODES
            .iter()
            .find(|code| **code == error.0)
            .copied()
            .unwrap_or("owner_read_failed"),
    )
}

impl NativeBridge {
    fn setup_ready(&self) -> Result<(), BridgeError> {
        if !self.status().connected {
            return Err(failure("not_connected"));
        }
        Ok(())
    }

    async fn setup_intent(
        &mut self,
        mutation: OwnerMutation,
        generation: u64,
    ) -> Result<IntentReceipt, BridgeError> {
        self.setup_ready()?;
        self.allow_new_auth()?;
        if self.status().pending {
            return Err(failure("pending_intent"));
        }
        if generation == 0 || generation > 9_007_199_254_740_991 {
            return Err(failure("draft_generation_limit"));
        }
        self.controller()?
            .setup_status()
            .await
            .map_err(setup_failure)?;
        self.controller()?
            .begin_mutation(mutation, generation)
            .map_err(|_| failure("owner_not_ready"))?;
        assert!(self.status().pending);
        self.deliver_intent(false).await
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

    /// Starts setup with the ticked default-pack entries as one retained owner intent;
    /// ticked MCP servers count as approved by the owner.
    /// Inputs: the ticked entry ids and a fresh draft generation. Output: the receipt.
    pub async fn run_setup(
        &mut self,
        pack: Vec<String>,
        generation: u64,
    ) -> Result<IntentReceipt, BridgeError> {
        self.setup_intent(OwnerMutation::RunSetup { pack }, generation)
            .await
    }

    /// Switches one installed default-pack entry on or off as one retained owner intent.
    /// Inputs: its id, the state and a fresh draft generation. Output: the receipt.
    pub async fn set_pack_entry(
        &mut self,
        id: String,
        on: bool,
        generation: u64,
    ) -> Result<IntentReceipt, BridgeError> {
        self.setup_intent(OwnerMutation::SetPackEntry { id, on }, generation)
            .await
    }

    /// Removes one installed default-pack entry and everything it added, as one retained
    /// owner intent. Inputs: its id and a fresh draft generation. Output: the receipt.
    pub async fn remove_pack_entry(
        &mut self,
        id: String,
        generation: u64,
    ) -> Result<IntentReceipt, BridgeError> {
        self.setup_intent(OwnerMutation::RemovePackEntry { id }, generation)
            .await
    }
}
