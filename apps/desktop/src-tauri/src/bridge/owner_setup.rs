use super::{BridgeError, IntentReceipt, NativeBridge, bounded, caused, failure};
use crazyagent::cleanup_settings::CleanupSettings;
use crazyagent::engine::owner::{OwnerMutation, OwnerRead};
use crazyagent::learning::{LearningProgress, MarkStep};
use crazyagent::master_frame_limit::MasterFrameSettings;
use crazyagent::master_step_limit::MasterStepSettings;
use crazyagent::master_time_limit::MasterTimeSettings;
use crazyagent::owner_profile::OwnerProfile;
use crazyagent::pools::PoolSummary;
use crazyagent::provider_retry::{ProviderRetry, ProviderRetryView};
use crazyagent::quit_settings::QuitSettings;
use crazyagent::sandbox_settings::SandboxSettings;

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
            .map_err(|error| caused("owner_read_failed", &error.0))?;
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
        self.allow_change()?;
        if self.blocked() {
            return Err(failure("pending_intent"));
        }
        if generation == 0 || generation > 9_007_199_254_740_991 {
            return Err(failure("draft_generation_limit"));
        }
        profile
            .validate()
            .map_err(|error| caused(profile_code(&error.0), &error.0))?;
        self.controller()?
            .read(OwnerRead::OwnerProfile)
            .await
            .map_err(|error| caused("owner_read_failed", &error.0))?;
        self.controller()?
            .begin_mutation(OwnerMutation::SaveOwnerProfile { profile }, generation)
            .map_err(|error| caused("owner_not_ready", &error.0))?;
        assert!(self.status().pending);
        self.deliver_intent(false).await
    }

    /// Reads the owner's sandbox settings. Input: none. Output: the stored settings, or the
    /// defaults before the first save.
    pub async fn sandbox_settings(&mut self) -> Result<SandboxSettings, BridgeError> {
        if !self.status().connected {
            return Err(failure("not_connected"));
        }
        self.controller()?
            .sandbox_settings()
            .await
            .map_err(|error| caused("owner_read_failed", &error.0))
    }

    /// Saves the owner's sandbox settings as one retained owner intent. Inputs: the full
    /// settings and a fresh draft generation. Output: the intent receipt.
    pub async fn set_sandbox_settings(
        &mut self,
        settings: SandboxSettings,
        generation: u64,
    ) -> Result<IntentReceipt, BridgeError> {
        self.allow_new_auth().await?;
        if self.blocked() {
            return Err(failure("pending_intent"));
        }
        if generation == 0 || generation > 9_007_199_254_740_991 {
            return Err(failure("draft_generation_limit"));
        }
        self.controller()?
            .read(OwnerRead::SandboxSettings)
            .await
            .map_err(|error| caused("owner_read_failed", &error.0))?;
        self.controller()?
            .begin_mutation(OwnerMutation::SetSandboxSettings { settings }, generation)
            .map_err(|error| caused("owner_not_ready", &error.0))?;
        assert!(self.status().pending);
        self.deliver_intent(false).await
    }

    /// Reads when the engine removes finished agents' worktrees on its own. Input: none.
    /// Output: the stored setting, or the default before the first save.
    pub async fn cleanup_settings(&mut self) -> Result<CleanupSettings, BridgeError> {
        if !self.status().connected {
            return Err(failure("not_connected"));
        }
        self.controller()?
            .cleanup_settings()
            .await
            .map_err(|error| caused("owner_read_failed", &error.0))
    }

    /// Saves when the engine removes finished agents' worktrees on its own, as one retained
    /// owner intent. Inputs: the full setting and a fresh draft generation. Output: the
    /// intent receipt.
    pub async fn set_cleanup_settings(
        &mut self,
        settings: CleanupSettings,
        generation: u64,
    ) -> Result<IntentReceipt, BridgeError> {
        self.allow_new_auth().await?;
        if self.blocked() {
            return Err(failure("pending_intent"));
        }
        if generation == 0 || generation > 9_007_199_254_740_991 {
            return Err(failure("draft_generation_limit"));
        }
        self.controller()?
            .read(OwnerRead::CleanupSettings)
            .await
            .map_err(|error| caused("owner_read_failed", &error.0))?;
        self.controller()?
            .begin_mutation(OwnerMutation::SetCleanupSettings { settings }, generation)
            .map_err(|error| caused("owner_not_ready", &error.0))?;
        assert!(self.status().pending);
        self.deliver_intent(false).await
    }

    /// Reads what quitting does to running team agents and how long the ones left running
    /// wait for the engine. Input: none. Output: the stored setting, or the default before the
    /// first save.
    pub async fn quit_settings(&mut self) -> Result<QuitSettings, BridgeError> {
        if !self.status().connected {
            return Err(failure("not_connected"));
        }
        self.controller()?
            .quit_settings()
            .await
            .map_err(|error| caused("owner_read_failed", &error.0))
    }

    /// Saves what quitting does to running team agents and how long the ones left running
    /// wait, as one retained owner intent. Inputs: the full setting and a fresh draft
    /// generation. Output: the intent receipt.
    pub async fn set_quit_settings(
        &mut self,
        settings: QuitSettings,
        generation: u64,
    ) -> Result<IntentReceipt, BridgeError> {
        self.allow_new_auth().await?;
        if self.blocked() {
            return Err(failure("pending_intent"));
        }
        if generation == 0 || generation > 9_007_199_254_740_991 {
            return Err(failure("draft_generation_limit"));
        }
        self.controller()?
            .read(OwnerRead::QuitSettings)
            .await
            .map_err(|error| caused("owner_read_failed", &error.0))?;
        self.controller()?
            .begin_mutation(OwnerMutation::SetQuitSettings { settings }, generation)
            .map_err(|error| caused("owner_not_ready", &error.0))?;
        assert!(self.status().pending);
        self.deliver_intent(false).await
    }

    /// Reads what harnesses do when a model provider can't be reached: the chosen option and
    /// the engine's line for every option and harness. Input: none. Output: the view.
    pub async fn provider_retry(&mut self) -> Result<ProviderRetryView, BridgeError> {
        if !self.status().connected {
            return Err(failure("not_connected"));
        }
        self.controller()?
            .provider_retry()
            .await
            .map_err(|error| caused("owner_read_failed", &error.0))
    }

    /// Saves what harnesses do when a model provider can't be reached, as one retained owner
    /// intent. Inputs: the option and a fresh draft generation. Output: the intent receipt.
    pub async fn set_provider_retry(
        &mut self,
        choice: ProviderRetry,
        generation: u64,
    ) -> Result<IntentReceipt, BridgeError> {
        self.allow_new_auth().await?;
        if self.blocked() {
            return Err(failure("pending_intent"));
        }
        if generation == 0 || generation > 9_007_199_254_740_991 {
            return Err(failure("draft_generation_limit"));
        }
        self.controller()?
            .read(OwnerRead::ProviderRetry)
            .await
            .map_err(|error| caused("owner_read_failed", &error.0))?;
        self.controller()?
            .begin_mutation(OwnerMutation::SetProviderRetry { choice }, generation)
            .map_err(|error| caused("owner_not_ready", &error.0))?;
        assert!(self.status().pending);
        self.deliver_intent(false).await
    }

    /// Reads the Learn CrazyAgent steps: their words, how many engine rows or marks show each
    /// done, and whether the owner skipped the guide. Input: none. Output: the progress.
    pub async fn learning_progress(&mut self) -> Result<LearningProgress, BridgeError> {
        if !self.status().connected {
            return Err(failure("not_connected"));
        }
        self.controller()?
            .learning_progress()
            .await
            .map_err(|error| caused("owner_read_failed", &error.0))
    }

    /// Sets or clears one learning mark as a retained owner intent: a page opened, or the
    /// guide skipped or shown again. Sends nothing when the engine already holds it that way.
    /// Inputs: the mark, whether it is set and a fresh draft generation. Output: None when
    /// nothing was sent, else the intent receipt.
    pub async fn set_learning_mark(
        &mut self,
        step: MarkStep,
        on: bool,
        generation: u64,
    ) -> Result<Option<IntentReceipt>, BridgeError> {
        self.allow_new_auth().await?;
        if self.blocked() {
            return Err(failure("pending_intent"));
        }
        if generation == 0 || generation > 9_007_199_254_740_991 {
            return Err(failure("draft_generation_limit"));
        }
        let progress = self
            .controller()?
            .learning_progress()
            .await
            .map_err(|error| caused("owner_read_failed", &error.0))?;
        if progress.marked(step) == on {
            return Ok(None);
        }
        self.controller()?
            .begin_mutation(OwnerMutation::SetLearningMark { step, on }, generation)
            .map_err(|error| caused("owner_not_ready", &error.0))?;
        assert!(self.status().pending);
        self.deliver_intent(false).await.map(Some)
    }

    /// Reads how many steps a master with its tools on may take for one message. Input:
    /// none. Output: the stored setting, or the default before the first save.
    pub async fn master_step_settings(&mut self) -> Result<MasterStepSettings, BridgeError> {
        if !self.status().connected {
            return Err(failure("not_connected"));
        }
        self.controller()?
            .master_step_settings()
            .await
            .map_err(|error| caused("owner_read_failed", &error.0))
    }

    /// Saves how many steps a master with its tools on may take for one message, as one
    /// retained owner intent. Inputs: the full setting and a fresh draft generation.
    /// Output: the intent receipt.
    pub async fn set_master_step_settings(
        &mut self,
        settings: MasterStepSettings,
        generation: u64,
    ) -> Result<IntentReceipt, BridgeError> {
        self.allow_new_auth().await?;
        if self.blocked() {
            return Err(failure("pending_intent"));
        }
        if generation == 0 || generation > 9_007_199_254_740_991 {
            return Err(failure("draft_generation_limit"));
        }
        self.controller()?
            .read(OwnerRead::MasterStepSettings)
            .await
            .map_err(|error| caused("owner_read_failed", &error.0))?;
        self.controller()?
            .begin_mutation(
                OwnerMutation::SetMasterStepSettings { settings },
                generation,
            )
            .map_err(|error| caused("owner_not_ready", &error.0))?;
        assert!(self.status().pending);
        self.deliver_intent(false).await
    }

    /// Reads how long a master may work on one message before it stops. Input: none.
    /// Output: the stored setting, or the default before the first save.
    pub async fn master_time_settings(&mut self) -> Result<MasterTimeSettings, BridgeError> {
        if !self.status().connected {
            return Err(failure("not_connected"));
        }
        self.controller()?
            .master_time_settings()
            .await
            .map_err(|error| caused("owner_read_failed", &error.0))
    }

    /// Saves how long a master may work on one message before it stops, as one retained
    /// owner intent. Inputs: the full setting and a fresh draft generation. Output: the
    /// intent receipt.
    pub async fn set_master_time_settings(
        &mut self,
        settings: MasterTimeSettings,
        generation: u64,
    ) -> Result<IntentReceipt, BridgeError> {
        self.allow_new_auth().await?;
        if self.blocked() {
            return Err(failure("pending_intent"));
        }
        if generation == 0 || generation > 9_007_199_254_740_991 {
            return Err(failure("draft_generation_limit"));
        }
        self.controller()?
            .read(OwnerRead::MasterTimeSettings)
            .await
            .map_err(|error| caused("owner_read_failed", &error.0))?;
        self.controller()?
            .begin_mutation(
                OwnerMutation::SetMasterTimeSettings { settings },
                generation,
            )
            .map_err(|error| caused("owner_not_ready", &error.0))?;
        assert!(self.status().pending);
        self.deliver_intent(false).await
    }

    /// Reads the largest one message the engine writes to a configured master's harness.
    /// Input: none. Output: the stored setting, or the default before the first save.
    pub async fn master_frame_settings(&mut self) -> Result<MasterFrameSettings, BridgeError> {
        if !self.status().connected {
            return Err(failure("not_connected"));
        }
        self.controller()?
            .master_frame_settings()
            .await
            .map_err(|error| caused("owner_read_failed", &error.0))
    }

    /// Saves the largest one message the engine writes to a configured master's harness, as
    /// one retained owner intent. Inputs: the full setting and a fresh draft generation.
    /// Output: the intent receipt.
    pub async fn set_master_frame_settings(
        &mut self,
        settings: MasterFrameSettings,
        generation: u64,
    ) -> Result<IntentReceipt, BridgeError> {
        self.allow_new_auth().await?;
        if self.blocked() {
            return Err(failure("pending_intent"));
        }
        if generation == 0 || generation > 9_007_199_254_740_991 {
            return Err(failure("draft_generation_limit"));
        }
        self.controller()?
            .read(OwnerRead::MasterFrameSettings)
            .await
            .map_err(|error| caused("owner_read_failed", &error.0))?;
        self.controller()?
            .begin_mutation(
                OwnerMutation::SetMasterFrameSettings { settings },
                generation,
            )
            .map_err(|error| caused("owner_not_ready", &error.0))?;
        assert!(self.status().pending);
        self.deliver_intent(false).await
    }

    /// Reads the plain pool list with each member's status.
    /// Input: none. Output: every pool's name, counts such as "2 ChatGPT, 1 Claude"
    /// and member status words.
    pub async fn pool_summaries(&self) -> Result<Vec<PoolSummary>, BridgeError> {
        if !self.status().connected {
            return Err(failure("not_connected"));
        }
        let pools = self
            .reader()?
            .pool_summaries()
            .await
            .map_err(|error| caused("owner_read_failed", &error.0))?;
        assert!(pools.len() <= 64);
        Ok(pools)
    }
}
