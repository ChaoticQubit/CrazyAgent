use super::{BridgeError, IntentReceipt, NativeBridge, bounded, failure};
use crazyagent::engine::owner::{OwnerMutation, OwnerRead};
use crazyagent::owner_profile::OwnerProfile;
use crazyagent::pools::PoolSummary;

fn profile_code(code: &str) -> &'static str {
    match code {
        "owner_profile_name" => "owner_profile_name",
        "owner_profile_role" => "owner_profile_role",
        "owner_profile_bio" => "owner_profile_bio",
        "owner_profile_stack" => "owner_profile_stack",
        _ => "owner_profile_bounds",
    }
}

impl NativeBridge {
    /// Reads the owner's first-run profile.
    /// Input: none. Output: the saved profile, or None on first run so the
    /// window can show the first-run page.
    pub async fn owner_profile(&mut self) -> Result<Option<OwnerProfile>, BridgeError> {
        if !self.status().connected {
            return Err(failure("not_connected"));
        }
        let profile = self
            .controller()?
            .owner_profile()
            .await
            .map_err(|_| failure("owner_read_failed"))?;
        bounded(profile)
    }

    /// Saves or updates the owner profile as one retained owner intent.
    /// Input: the full profile and a fresh draft generation. Output: the intent
    /// receipt; a refused field comes back as a plain rejection code.
    pub async fn save_owner_profile(
        &mut self,
        profile: OwnerProfile,
        generation: u64,
    ) -> Result<IntentReceipt, BridgeError> {
        self.allow_new_auth()?;
        if self.status().pending {
            return Err(failure("pending_intent"));
        }
        if generation == 0 || generation > 9_007_199_254_740_991 {
            return Err(failure("draft_generation_limit"));
        }
        profile
            .validate()
            .map_err(|error| failure(profile_code(&error.0)))?;
        self.controller()?
            .read(OwnerRead::OwnerProfile)
            .await
            .map_err(|_| failure("owner_read_failed"))?;
        self.controller()?
            .begin_mutation(OwnerMutation::SaveOwnerProfile { profile }, generation)
            .map_err(|_| failure("owner_not_ready"))?;
        assert!(self.status().pending);
        self.deliver_intent(false).await
    }

    /// Reads the plain pool list with each member's status.
    /// Input: none. Output: every pool's name, counts such as "2 ChatGPT, 1 Claude"
    /// and member status words.
    pub async fn pool_summaries(&mut self) -> Result<Vec<PoolSummary>, BridgeError> {
        if !self.status().connected {
            return Err(failure("not_connected"));
        }
        let pools = self
            .controller()?
            .pool_summaries()
            .await
            .map_err(|_| failure("owner_read_failed"))?;
        assert!(pools.len() <= 64);
        Ok(pools)
    }
}
