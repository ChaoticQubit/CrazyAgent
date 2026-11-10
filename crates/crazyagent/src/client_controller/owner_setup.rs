use super::*;
use crate::cleanup_settings::CleanupSettings;
use crate::owner_profile::OwnerProfile;
use crate::pools::PoolSummary;
use crate::sandbox_settings::SandboxSettings;

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

    /// Reads the owner's sandbox settings from the engine. Input: none. Output: the
    /// stored settings, or the defaults before the first save.
    pub async fn sandbox_settings(&mut self) -> Result<SandboxSettings, EngineError> {
        match self.read(OwnerRead::SandboxSettings).await?.data {
            OwnerData::SandboxSettings { settings } => Ok(settings),
            _ => Err(EngineError("controller_read_response".into())),
        }
    }

    /// Saves the owner's sandbox settings as one retained owner mutation. Input: the full
    /// settings and a nonzero client generation. Output: the engine response.
    pub async fn set_sandbox_settings(
        &mut self,
        settings: SandboxSettings,
        generation: u64,
    ) -> Result<OwnerResponse, EngineError> {
        if self.pending.is_some() || self.pending_account_profile.is_some() {
            return Err(EngineError("controller_pending_intent".into()));
        }
        self.read(OwnerRead::SandboxSettings).await?;
        self.begin_mutation(OwnerMutation::SetSandboxSettings { settings }, generation)?;
        self.send_pending().await
    }

    /// Reads when the engine removes finished agents' worktrees on its own. Input: none.
    /// Output: the stored setting, or the default before the first save.
    pub async fn cleanup_settings(&mut self) -> Result<CleanupSettings, EngineError> {
        match self.read(OwnerRead::CleanupSettings).await?.data {
            OwnerData::CleanupSettings { settings } => Ok(settings),
            _ => Err(EngineError("controller_read_response".into())),
        }
    }

    /// Saves when the engine removes finished agents' worktrees on its own, as one retained
    /// owner mutation. Input: the full setting and a nonzero client generation. Output: the
    /// engine response.
    pub async fn set_cleanup_settings(
        &mut self,
        settings: CleanupSettings,
        generation: u64,
    ) -> Result<OwnerResponse, EngineError> {
        if self.pending.is_some() || self.pending_account_profile.is_some() {
            return Err(EngineError("controller_pending_intent".into()));
        }
        self.read(OwnerRead::CleanupSettings).await?;
        self.begin_mutation(OwnerMutation::SetCleanupSettings { settings }, generation)?;
        self.send_pending().await
    }

    /// Reads the plain summary of every pool, following engine pages.
    /// Input: none. Output: each pool's name, counts such as "2 ChatGPT, 1 Claude"
    /// and member status, in catalog order.
    pub async fn pool_summaries(&mut self) -> Result<Vec<PoolSummary>, EngineError> {
        let mut pools = Vec::new();
        let mut offset = 0u8;
        loop {
            let OwnerData::PoolSummaries {
                pools: page,
                next_offset,
            } = self.read(OwnerRead::PoolSummaries { offset }).await?.data
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
