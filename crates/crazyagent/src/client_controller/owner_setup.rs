use super::*;
use crate::owner_profile::OwnerProfile;
use crate::pools::PoolSummary;

impl OwnerController {
    /// Reads the owner's first-run profile from the engine.
    /// Input: none. Output: the saved profile, or None before the first save,
    /// which clients use to show the first-run page.
    pub async fn owner_profile(&mut self) -> Result<Option<OwnerProfile>, EngineError> {
        match self.read(OwnerRead::OwnerProfile).await?.data {
            OwnerData::OwnerProfile { profile } => Ok(profile),
            _ => Err(EngineError("controller_read_response".into())),
        }
    }

    /// Saves or updates the owner profile as one retained owner mutation.
    /// Input: the full profile and a nonzero client generation. Output: the
    /// engine response; a refused profile comes back as a rejected result.
    pub async fn save_owner_profile(
        &mut self,
        profile: OwnerProfile,
        generation: u64,
    ) -> Result<OwnerResponse, EngineError> {
        profile.validate()?;
        if self.pending.is_some() || self.pending_account_profile.is_some() {
            return Err(EngineError("controller_pending_intent".into()));
        }
        self.read(OwnerRead::OwnerProfile).await?;
        self.begin_mutation(OwnerMutation::SaveOwnerProfile { profile }, generation)?;
        self.send_pending().await
    }

    /// Reads the plain summary of every pool, following engine pages.
    /// Input: none. Output: each pool's name, counts such as "2 ChatGPT, 1 Claude"
    /// and member status, in catalog order.
    pub async fn pool_summaries(&mut self) -> Result<Vec<PoolSummary>, EngineError> {
        let mut pools = Vec::new();
        let mut offset = 0u8;
        loop {
            let OwnerData::PoolSummaries { pools: page, next_offset } =
                self.read(OwnerRead::PoolSummaries { offset }).await?.data
            else {
                return Err(EngineError("controller_read_response".into()));
            };
            pools.extend(page);
            match next_offset {
                Some(next) if next > offset && pools.len() <= 64 => offset = next,
                Some(_) => return Err(EngineError("controller_read_response".into())),
                None => break,
            }
        }
        assert!(pools.len() <= 64);
        Ok(pools)
    }
}
