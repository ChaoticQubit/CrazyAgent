use super::*;
use crate::keep_awake::{KeepAwakeAction, KeepAwakeView};

impl OwnerController {
    /// Reads Keep awake (DEC-167): the settings, what is held and why, the lid support,
    /// the platform and the running work count. Input: none. Output: the view as the
    /// engine last evaluated it.
    pub async fn keep_awake(&mut self) -> Result<KeepAwakeView, EngineError> {
        match self.read(OwnerRead::KeepAwake).await?.data {
            OwnerData::KeepAwake { view } => Ok(view),
            _ => Err(EngineError("controller_read_response".into())),
        }
    }

    /// Sends one Keep awake change as a retained owner mutation: start a timer, move it,
    /// end the hold, or save the settings. Inputs: the change and a nonzero client
    /// generation. Output: the engine response; a refused change comes back as a rejected
    /// result, and minutes outside 1 to 1440 as `keep_awake_minutes_invalid`.
    pub async fn keep_awake_action(
        &mut self,
        action: KeepAwakeAction,
        generation: u64,
    ) -> Result<OwnerResponse, EngineError> {
        action.validate()?;
        if self.pending.is_some() || self.pending_account_profile.is_some() {
            return Err(EngineError("controller_pending_intent".into()));
        }
        self.read(OwnerRead::KeepAwake).await?;
        self.begin_mutation(OwnerMutation::KeepAwake { action }, generation)?;
        self.send_pending().await
    }
}
