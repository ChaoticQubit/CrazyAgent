use super::{BridgeError, IntentReceipt, NativeBridge, bounded, caused, failure};
use crazyagent::engine::owner::{OwnerMutation, OwnerRead};
use crazyagent::keep_awake::{KeepAwakeAction, KeepAwakeView, Want};

/// The highest draft generation the window may send, the largest integer JavaScript holds
/// exactly.
const GENERATION_MAX: u64 = 9_007_199_254_740_991;

impl NativeBridge {
    /// Reads Keep awake (DEC-167) for the top bar's control, its panel and Settings: the
    /// settings, what is held and why, the lid support, the platform and the count of
    /// working agents. Input: none. Output: the view as the engine last evaluated it, or
    /// `not_connected` without an engine.
    pub async fn keep_awake_read(&self) -> Result<KeepAwakeView, BridgeError> {
        if !self.status().connected {
            return Err(failure("not_connected"));
        }
        let view = self
            .reader()?
            .keep_awake()
            .await
            .map_err(|error| caused("owner_read_failed", &error.0))?;
        assert!(view.state.ended.is_none() || view.state.want == Want::Nothing);
        assert!(view.state.ends_at_ms.is_none() || view.state.want != Want::Nothing);
        bounded(view)
    }

    /// Sends one Keep awake change as a retained owner intent: start a timer, move the
    /// running one later, end the hold, or save the settings. Inputs: the change and a
    /// fresh draft generation. Output: the intent receipt, or `keep_awake_minutes_invalid`
    /// for minutes outside 1 to 1440; the engine refuses any other change in the receipt.
    pub async fn keep_awake_act(
        &mut self,
        action: KeepAwakeAction,
        generation: u64,
    ) -> Result<IntentReceipt, BridgeError> {
        self.allow_new_auth().await?;
        if self.blocked() {
            return Err(failure("pending_intent"));
        }
        if generation == 0 || generation > GENERATION_MAX {
            return Err(failure("draft_generation_limit"));
        }
        action
            .validate()
            .map_err(|_| failure("keep_awake_minutes_invalid"))?;
        self.controller()?
            .read(OwnerRead::KeepAwake)
            .await
            .map_err(|error| caused("owner_read_failed", &error.0))?;
        self.controller()?
            .begin_mutation(OwnerMutation::KeepAwake { action }, generation)
            .map_err(|error| caused("owner_not_ready", &error.0))?;
        assert!(self.status().pending);
        self.deliver_intent(false).await
    }
}
