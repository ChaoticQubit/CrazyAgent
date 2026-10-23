use super::*;
use crate::engine::owner::{AccountProfileOutcome, AccountProfileReply, AccountSecret};
use model_access::account_profile::{public_problem, validate_connection};

impl NativeOwner {
    fn profile_controller(
        &mut self,
        submission: &model_access::ProfileSubmission,
        reconcile: bool,
    ) -> Result<&mut OwnerController, String> {
        if !self.ready {
            return Err("account_profile_owner_changed".into());
        }
        if !reconcile
            && (self.creation_pending.is_some()
                || self.model_pending.is_some()
                || self.conversation_pending.is_some()
                || self.master_pending.is_some())
        {
            return Err("account_profile_pending_intent".into());
        }
        let controller = self
            .controller
            .as_mut()
            .ok_or("account_profile_owner_changed")?;
        if controller.identity().database_instance()
            != submission.basis.identity.database_instance()
            || (!reconcile && controller.identity() != &submission.basis.identity)
            || submission.generation == 0
            || submission.request.scope.expected_database_instance
                != submission.basis.identity.database_instance()
            || submission.request.scope.expected_config_id != submission.basis.config_id
            || submission.request.scope.expected_source_hash != submission.basis.source_hash
            || submission.request.scope.connection != submission.connection.id
        {
            return Err("account_profile_owner_changed".into());
        }
        if !reconcile
            && (controller.pending().is_some() || controller.pending_account_profile().is_some())
        {
            return Err("account_profile_pending_intent".into());
        }
        if let Some(pending) = controller.pending_account_profile()
            && (pending.request() != &submission.request
                || pending.generation() != submission.generation)
        {
            return Err("account_profile_pending_intent".into());
        }
        Ok(controller)
    }

    pub(super) async fn account_profile_configure(
        &mut self,
        submission: model_access::ProfileSubmission,
        secret: Option<AccountSecret>,
    ) -> model_access::Event {
        let result = self.configure_profile(&submission, secret).await;
        let retained = self
            .controller
            .as_ref()
            .is_some_and(|controller| controller.pending_account_profile().is_some());
        model_access::Event::AccountProfile {
            submission,
            result,
            retained,
        }
    }

    async fn configure_profile(
        &mut self,
        submission: &model_access::ProfileSubmission,
        secret: Option<AccountSecret>,
    ) -> Result<Box<AccountProfileReply>, String> {
        validate_connection(&submission.connection)?;
        let controller = self.profile_controller(submission, false)?;
        let fresh = controller
            .read(OwnerRead::ModelAccessRecord {
                collection: crate::engine::owner::ModelAccessCollection::Connections,
                id: submission.connection.id.clone(),
            })
            .await
            .map_err(|error| public_problem(&error.0))?;
        if !submission.basis.writable
            || fresh.revision != submission.basis.revision
            || fresh.config_id != submission.basis.config_id
            || !matches!(&fresh.data, OwnerData::ModelAccessRecord {
                source_hash, writable: true,
                record: crate::engine::owner::ModelAccessRecord::Connection { connection },
            } if source_hash == &submission.basis.source_hash
                && connection == &submission.connection)
        {
            return Err("account_profile_stale_connection".into());
        }
        controller
            .configure_account_profile(submission.request.clone(), secret, submission.generation)
            .await
            .map(Box::new)
            .map_err(|error| public_problem(&error.0).into())
    }

    pub(super) async fn account_profile_read(
        &mut self,
        submission: model_access::ProfileSubmission,
    ) -> model_access::Event {
        let (result, retained) = match self.profile_controller(&submission, true) {
            Ok(controller) => {
                let was_pending = controller.pending_account_profile().is_some();
                let result = controller
                    .read_account_profile(submission.request.clone())
                    .await
                    .map(Box::new)
                    .map_err(|error| public_problem(&error.0).into());
                let still_pending = controller.pending_account_profile().is_some();
                let retained = match &result {
                    Err(_) => true,
                    Ok(reply) => match &reply.outcome {
                        AccountProfileOutcome::NotRecorded => !was_pending || still_pending,
                        AccountProfileOutcome::ConfiguredOnly { .. } => still_pending,
                    },
                };
                (result, retained)
            }
            Err(problem) => (Err(problem), true),
        };
        model_access::Event::AccountProfile {
            submission,
            result,
            retained,
        }
    }

    pub(super) async fn account_profile_capabilities(
        &mut self,
        submission: model_access::ProfileSubmission,
    ) -> model_access::Event {
        let result = match self.profile_controller(&submission, false) {
            Ok(controller) => controller
                .read_account_auth_capabilities(submission.request.clone())
                .await
                .map(Box::new)
                .map_err(|error| public_problem(&error.0).into()),
            Err(problem) => Err(problem),
        };
        model_access::Event::AccountProfileCapabilities { submission, result }
    }
}
