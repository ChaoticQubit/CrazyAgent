use super::{BridgeError, IntentReceipt, NativeBridge, caused, failure};
use crazyagent::engine::owner::OwnerMutation;
use crazyagent::scheduling::{PoolRanking, Schedule, ScheduleChange};

impl NativeBridge {
    /// Reads the owner's account scheduling choices: the app strategy, pool strategies,
    /// project overrides and each account's rank, reserve and pin.
    /// Returns the stored schedule or a plain failure code.
    pub async fn account_schedule(&mut self) -> Result<Schedule, BridgeError> {
        if !self.status().connected {
            return Err(failure("not_connected"));
        }
        self.controller()?
            .account_schedule()
            .await
            .map_err(|error| caused("owner_read_failed", &error.0))
    }

    /// Reads each pool's account ranking: the order work would try its accounts now and
    /// why each held account waits.
    /// Returns one ranking per pool or a plain failure code.
    pub async fn account_ranking(&mut self) -> Result<Vec<PoolRanking>, BridgeError> {
        if !self.status().connected {
            return Err(failure("not_connected"));
        }
        self.controller()?
            .account_ranking()
            .await
            .map_err(|error| caused("owner_read_failed", &error.0))
    }

    /// Sends one scheduling change, such as a pool strategy or an account's reserve.
    /// Takes the change and the client generation.
    /// Returns the actual commit receipt; a value out of range is a plain failure.
    pub async fn set_account_schedule(
        &mut self,
        change: ScheduleChange,
        generation: u64,
    ) -> Result<IntentReceipt, BridgeError> {
        self.ready_for_intent(generation)?;
        change
            .validate()
            .map_err(|error| caused("account_schedule_invalid", &error.0))?;
        self.account_schedule().await?;
        self.controller()?
            .begin_mutation(OwnerMutation::AccountSchedule { change }, generation)
            .map_err(|error| caused("owner_not_ready", &error.0))?;
        self.deliver_intent(false).await
    }
}
