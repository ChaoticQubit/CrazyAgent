use super::*;
use crate::engine::owner::ScorecardSaved;
use crate::scorecard::{Scorecard, Settings};

impl OwnerController {
    fn scorecard_client(&self) -> Result<&OwnerClient, EngineError> {
        self.client
            .as_deref()
            .ok_or_else(|| EngineError("controller_disconnected".into()))
    }

    /// Reads the go/no-go scorecard in the reader's local days. Input: the reader's UTC
    /// offset in minutes. Output: the scorecard, or a `SCORECARD_CODES` refusal.
    pub async fn scorecard_report(&self, offset_minutes: i32) -> Result<Scorecard, EngineError> {
        self.scorecard_client()?
            .scorecard_report(offset_minutes)
            .await
    }

    /// Saves the scorecard settings, naming the version the owner read. Inputs: the
    /// settings and the version. Output: Saved or Stale, or a `SCORECARD_CODES` refusal.
    pub async fn scorecard_set(
        &self,
        settings: Settings,
        version: u64,
    ) -> Result<ScorecardSaved, EngineError> {
        self.scorecard_client()?
            .scorecard_set(settings, version)
            .await
    }
}
