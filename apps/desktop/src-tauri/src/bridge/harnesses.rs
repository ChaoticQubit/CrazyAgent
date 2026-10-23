use super::{BridgeError, NativeBridge, bounded, failure};
use crazyagent::harnesses::HarnessEntry;

impl NativeBridge {
    /// Lists every harness the engine knows with its status on this machine.
    /// Input: none. Output: catalog entries in owner order, each with a plain
    /// status such as "Ready", "Not installed" or "Limited control: <reason>".
    pub async fn harness_catalog(&mut self) -> Result<Vec<HarnessEntry>, BridgeError> {
        if !self.status().connected {
            return Err(failure("not_connected"));
        }
        let entries = self
            .controller()?
            .harness_catalog()
            .await
            .map_err(|_| failure("owner_read_failed"))?;
        assert!(!entries.is_empty());
        bounded(entries)
    }
}
