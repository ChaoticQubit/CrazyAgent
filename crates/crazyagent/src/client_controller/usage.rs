use super::*;
use crate::protocol::{CredentialRef, ProjectId};
use crate::usage::money::MoneySettings;
use crate::usage::report::UsageReport;
use crate::usage::spend::SpendSettings;

impl OwnerController {
    fn usage_client(&self) -> Result<&OwnerClient, EngineError> {
        self.client
            .as_deref()
            .ok_or_else(|| EngineError("controller_disconnected".into()))
    }

    /// Reads the usage summary for one project, None for every project, over the range
    /// starting at `since_ms`, None for all time. Inputs: the project, the range start and
    /// the viewer's offset east of UTC in minutes. Output: totals, groups, local days, lines
    /// changed per project and per agent, and every account's limits.
    pub async fn usage_report(
        &self,
        project: Option<ProjectId>,
        since_ms: Option<i64>,
        offset_minutes: i32,
    ) -> Result<UsageReport, EngineError> {
        self.usage_client()?
            .usage_report(project, since_ms, offset_minutes)
            .await
    }

    /// Asks the engine to check every signed-in account's limits now.
    pub async fn usage_refresh(&self) -> Result<(), EngineError> {
        self.usage_client()?.usage_refresh().await
    }

    /// Stores the owner's main number and price source. Input: the settings. Output: done.
    pub async fn usage_set_money(&self, settings: MoneySettings) -> Result<(), EngineError> {
        self.usage_client()?.usage_set_money(settings).await
    }

    /// Asks the engine to spend one free reset on an account; its outcome appears in the
    /// report's spend view. Input: the account. Output: done once queued, or
    /// `usage_spend_off`.
    pub async fn usage_spend(&self, account: CredentialRef) -> Result<(), EngineError> {
        self.usage_client()?.usage_spend(account).await
    }

    /// Stores the owner's spend settings. Input: the settings. Output: done.
    pub async fn usage_set_spend(&self, settings: SpendSettings) -> Result<(), EngineError> {
        self.usage_client()?.usage_set_spend(settings).await
    }

    /// Applies the owner's choice of the recount, once. Input: whether to use the new numbers.
    /// Output: done, or `usage_recount_not_ready`.
    pub async fn usage_decide_recount(&self, use_new: bool) -> Result<(), EngineError> {
        self.usage_client()?.usage_decide_recount(use_new).await
    }
}
