use super::*;
use crate::scheduling::{
    ACCOUNT_PLANS_MAX, PoolRanking, SCOPED_STRATEGIES_MAX, Schedule, ScheduleChange, ScheduleCursor,
};

const SCHEDULE_ROWS_MAX: usize = SCOPED_STRATEGIES_MAX * 2 + ACCOUNT_PLANS_MAX;

impl OwnerController {
    /// Reads the owner's account scheduling choices, following engine pages, so a schedule
    /// larger than one reply is read whole: the app strategy, pool strategies, project
    /// overrides and account plans, with the version of the first page, so a change made
    /// while the later pages were read leaves the next owner change stale, never applied.
    /// Returns the stored schedule, or the engine's plain rejection code.
    pub async fn account_schedule(&mut self) -> Result<Schedule, EngineError> {
        let mut schedule = Schedule::default();
        let mut after: Option<ScheduleCursor> = None;
        loop {
            let OwnerData::AccountSchedule {
                schedule: page,
                next,
            } = self
                .read(OwnerRead::AccountSchedule {
                    after: after.clone(),
                })
                .await?
                .data
            else {
                return Err(EngineError("controller_read_response".into()));
            };
            if after.is_none() {
                schedule.version = page.version;
            }
            schedule.strategy = page.strategy;
            schedule.pools.extend(page.pools);
            schedule.projects.extend(page.projects);
            schedule.accounts.extend(page.accounts);
            if schedule.pools.len() + schedule.projects.len() + schedule.accounts.len()
                > SCHEDULE_ROWS_MAX
            {
                return Err(EngineError("controller_read_response".into()));
            }
            match next {
                Some(next) if after.as_ref().is_none_or(|after| &next > after) => {
                    after = Some(next);
                }
                Some(_) => return Err(EngineError("controller_read_response".into())),
                None => break,
            }
        }
        assert!(
            schedule.pools.len() + schedule.projects.len() + schedule.accounts.len()
                <= SCHEDULE_ROWS_MAX
        );
        Ok(schedule)
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

    /// Sends one owner scheduling change, such as a pool strategy or an account's rank. Its
    /// freshness read is the small owner profile, never the whole schedule, so a schedule
    /// larger than one reply can still be changed and shrunk.
    /// Takes the change, the schedule version the screen showed and the client generation.
    /// Returns the actual owner response; a refused change carries a plain code,
    /// `stale_revision` once the schedule changed elsewhere.
    pub async fn set_account_schedule(
        &mut self,
        change: ScheduleChange,
        version: u64,
        generation: u64,
    ) -> Result<OwnerResponse, EngineError> {
        change.validate()?;
        self.owner_profile().await?;
        self.begin_mutation(
            OwnerMutation::AccountSchedule { change, version },
            generation,
        )?;
        self.send_pending().await
    }
}
