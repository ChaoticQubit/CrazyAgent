use super::{BridgeError, NativeBridge, bounded, caused, failure};
use crazyagent::harnesses::HarnessEntry;

impl NativeBridge {
    /// Lists every harness the engine knows with its status on this machine, from
    /// the engine's last probe unless `check` asks it to look again now. Input:
    /// whether to probe now. Output: catalog entries in owner order, each with a
    /// plain status such as "Ready", "Not installed" or "Limited control: <reason>".
    pub async fn harness_catalog(&self, check: bool) -> Result<Vec<HarnessEntry>, BridgeError> {
        if !self.status().connected {
            return Err(failure("not_connected"));
        }
        let entries = self
            .reader()?
            .harness_catalog(check)
            .await
            .map_err(|error| caused("owner_read_failed", &error.0))?;
        assert!(!entries.is_empty());
        bounded(entries)
    }
}
