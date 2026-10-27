use super::*;
use crate::scheduling::{PoolRanking, Schedule, ScheduleChange};

impl OwnerController {
    /// Reads the owner's account scheduling choices: the app strategy, pool strategies,
    /// project overrides and account plans.
    /// Returns the stored schedule, or the engine's plain rejection code.
    pub async fn account_schedule(&mut self) -> Result<Schedule, EngineError> {
        match self.read(OwnerRead::AccountSchedule).await?.data {
            OwnerData::AccountSchedule { schedule } => Ok(schedule),
            _ => Err(EngineError("controller_read_response".into())),
        }
    }

    /// Reads each pool's account ranking: the order work would try its accounts now and
    /// why each held account waits.
    /// Returns one ranking per pool, or the engine's plain rejection code.
    pub async fn account_ranking(&mut self) -> Result<Vec<PoolRanking>, EngineError> {
        match self.read(OwnerRead::AccountRanking).await?.data {
            OwnerData::AccountRanking { pools } => Ok(pools),
            _ => Err(EngineError("controller_read_response".into())),
        }
    }

    /// Sends one owner scheduling change, such as a pool strategy or an account's rank.
    /// Takes the change and the client generation.
    /// Returns the actual owner response; a refused change carries a plain code.
    pub async fn set_account_schedule(
        &mut self,
        change: ScheduleChange,
        generation: u64,
    ) -> Result<OwnerResponse, EngineError> {
        change.validate()?;
        self.account_schedule().await?;
        self.begin_mutation(OwnerMutation::AccountSchedule { change }, generation)?;
        self.send_pending().await
    }
}
