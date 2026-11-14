use super::*;
use crate::protocol::{CredentialRef, ProjectId};
use crate::usage::money::MoneyChoice;
use crate::usage::report::UsageReport;
use crate::usage::spend::SpendChoice;

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

    /// Reads every saved account's free resets, credits and spend as stored; `detail` only
    /// while Pools is open. Input: whether Pools is open. Output: one entry per account.
    pub async fn usage_extras(
        &self,
        detail: bool,
    ) -> Result<Vec<crate::engine::owner::AccountExtras>, EngineError> {
        self.usage_client()?.usage_extras(detail).await
    }

    /// Asks the engine to check every signed-in account's limits now.
    pub async fn usage_refresh(&self) -> Result<(), EngineError> {
        self.usage_client()?.usage_refresh().await
    }

    /// Stores one of the owner's dollars and Metrics page choices and keeps the other two.
    /// Input: the choice and its value. Output: done.
    pub async fn usage_set_money_one(&self, choice: MoneyChoice) -> Result<(), EngineError> {
        self.usage_client()?.usage_set_money_one(choice).await
    }

    /// Asks the engine to spend one free reset on an account; its outcome appears in the
    /// report's spend view. Inputs: the account and whether the owner confirmed this spend.
    /// Output: done once queued, `usage_spend_off` or `usage_spend_unconfirmed`.
    pub async fn usage_spend(
        &self,
        account: CredentialRef,
        confirmed: bool,
    ) -> Result<(), EngineError> {
        self.usage_client()?.usage_spend(account, confirmed).await
    }

    /// Stores one of the owner's Pools settings and keeps the other two. Input: the setting
    /// and its value. Output: done.
    pub async fn usage_set_spend_one(&self, choice: SpendChoice) -> Result<(), EngineError> {
        self.usage_client()?.usage_set_spend_one(choice).await
    }

    /// Applies the owner's choice of the recount, once. Input: whether to use the new numbers.
    /// Output: done, or `usage_recount_not_ready`.
    pub async fn usage_decide_recount(&self, use_new: bool) -> Result<(), EngineError> {
        self.usage_client()?.usage_decide_recount(use_new).await
    }
}
