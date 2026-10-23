use super::*;
use crate::engine::owner::{
    AccountAuthCapabilitiesReply, AccountProfileOutcome, AccountProfileReply,
    AccountProfileRequest, AccountSecret,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingAccountProfile {
    request: AccountProfileRequest,
    pub(super) identity: OwnerConnectionIdentity,
    generation: u64,
    uncertain: bool,
}

impl PendingAccountProfile {
    pub fn request(&self) -> &AccountProfileRequest {
        &self.request
    }
    pub fn generation(&self) -> u64 {
        self.generation
    }
    pub fn uncertain(&self) -> bool {
        self.uncertain
    }
}

impl OwnerController {
    pub fn pending_account_profile(&self) -> Option<&PendingAccountProfile> {
        self.pending_account_profile.as_ref()
    }

    pub async fn configure_account_profile(
        &mut self,
        request: AccountProfileRequest,
        secret: Option<AccountSecret>,
        generation: u64,
    ) -> Result<AccountProfileReply, EngineError> {
        if self.pending.is_some() || self.pending_account_profile.is_some() {
            return Err(EngineError("controller_pending_intent".into()));
        }
        if generation == 0 {
            return Err(EngineError("controller_not_ready".into()));
        }
        if request.scope.expected_database_instance != self.identity.database_instance() {
            return Err(EngineError("controller_pending_database_mismatch".into()));
        }
        let client = self
            .client
            .as_ref()
            .ok_or_else(|| EngineError("controller_disconnected".into()))?;
        self.pending_account_profile = Some(PendingAccountProfile {
            request: request.clone(),
            identity: self.identity.clone(),
            generation,
            uncertain: true,
        });
        if let Some(view) = &mut self.view {
            view.fresh = false;
        }
        let result = client
            .configure_account_profile_private(request, secret)
            .await;
        match result {
            Ok(result) => self.settle_account_profile(result.reply, result.settled),
            Err(failure) => {
                if !failure.may_have_effects {
                    self.pending_account_profile = None;
                }
                Err(failure.error)
            }
        }
    }

    pub async fn read_account_profile(
        &mut self,
        request: AccountProfileRequest,
    ) -> Result<AccountProfileReply, EngineError> {
        if let Some(pending) = &self.pending_account_profile {
            if pending.request != request {
                return Err(EngineError(
                    "controller_account_profile_request_mismatch".into(),
                ));
            }
            if pending.identity.database_instance() != self.identity.database_instance() {
                return Err(EngineError("controller_pending_database_mismatch".into()));
            }
        }
        let client = self
            .client
            .as_ref()
            .ok_or_else(|| EngineError("controller_disconnected".into()))?;
        let result = client.read_account_profile_private(request).await?;
        self.settle_account_profile(result.reply, result.settled)
    }

    pub async fn read_account_auth_capabilities(
        &mut self,
        request: AccountProfileRequest,
    ) -> Result<AccountAuthCapabilitiesReply, EngineError> {
        if request.scope.expected_database_instance != self.identity.database_instance() {
            return Err(EngineError("controller_pending_database_mismatch".into()));
        }
        let reply = self
            .client
            .as_ref()
            .ok_or_else(|| EngineError("controller_disconnected".into()))?
            .read_account_auth_capabilities(request)
            .await?;
        if reply.database_instance != self.identity.database_instance()
            || reply.engine_epoch != self.identity.engine_epoch()
        {
            return Err(EngineError(
                "controller_account_profile_identity_mismatch".into(),
            ));
        }
        Ok(reply)
    }

    fn settle_account_profile(
        &mut self,
        reply: AccountProfileReply,
        settled: bool,
    ) -> Result<AccountProfileReply, EngineError> {
        if reply.database_instance != self.identity.database_instance()
            || reply.engine_epoch != self.identity.engine_epoch()
        {
            return Err(EngineError(
                "controller_account_profile_identity_mismatch".into(),
            ));
        }
        if let Some(pending) = &self.pending_account_profile {
            if reply.request_id != pending.request.request_id {
                return Err(EngineError(
                    "controller_account_profile_request_mismatch".into(),
                ));
            }
            let recorded = matches!(reply.outcome, AccountProfileOutcome::ConfiguredOnly { .. });
            if recorded || settled {
                self.pending_account_profile = None;
            }
        }
        Ok(reply)
    }
}
