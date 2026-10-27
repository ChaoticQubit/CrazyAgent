use super::*;
use crate::usage::report::UsageReport;

impl OwnerController {
    fn usage_client(&self) -> Result<&OwnerClient, EngineError> {
        self.client
            .as_deref()
            .ok_or_else(|| EngineError("controller_disconnected".into()))
    }

    /// Reads the usage summary for the range starting at `since_ms`, None for all time.
    /// Inputs: the range start and the viewer's offset east of UTC in minutes. Output:
    /// totals, groups, local days, lines changed and every account's limits.
    pub async fn usage_report(
        &self,
        since_ms: Option<i64>,
        offset_minutes: i32,
    ) -> Result<UsageReport, EngineError> {
        self.usage_client()?
            .usage_report(since_ms, offset_minutes)
            .await
    }

    /// Asks the engine to check every signed-in account's limits now.
    pub async fn usage_refresh(&self) -> Result<(), EngineError> {
        self.usage_client()?.usage_refresh().await
    }
}
