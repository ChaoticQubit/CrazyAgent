use super::*;

impl OwnerRequest {
    pub fn validate(&self) -> Result<(), EngineError> {
        if self.protocol_version != PROTOCOL_VERSION {
            return Err(failure("owner_protocol_version"));
        }
        encode_frame(self).map_err(|_| failure("owner_request_bounds"))?;
        match &self.operation {
            OwnerOperation::Apply {
                mutation: OwnerMutation::ProjectMaster { action },
            } => crate::master_session::validate_action(action)?,
            OwnerOperation::Apply {
                mutation:
                    OwnerMutation::PrepareWorkerDispatch {
                        attempt,
                        repository_slot,
                    },
            } => {
                crate::worker_dispatch::validate_prepare(attempt, *repository_slot)?;
            }
            OwnerOperation::Apply {
                mutation:
                    OwnerMutation::CancelWorkerDispatch { .. }
                    | OwnerMutation::ReconcileWorkerDispatch { .. },
            } => {}
            OwnerOperation::Apply {
                mutation:
                    OwnerMutation::ResumeWorkerDispatch {
                        expected_delivery_generation,
                        ..
                    },
            } => {
                if !(1..crate::worker_dispatch::DISPATCH_GENERATIONS_MAX)
                    .contains(expected_delivery_generation)
                {
                    return Err(failure("dispatch_delivery_generation"));
                }
            }
            OwnerOperation::Apply { mutation } => {
                mutation.command()?;
            }
            OwnerOperation::Read { query } => {
                query.command()?;
                if let OwnerRead::ProjectMasterTurns { cursor, limit, .. } = query {
                    crate::master_session::validate_page(cursor.as_ref(), *limit)?;
                }
                if let OwnerRead::ProjectMasterMessages { before, limit, .. } = query {
                    crate::master_sessions::validate_messages_page(*before, *limit)?;
                }
                if let OwnerRead::ModelAccessPage { limit, cursor, .. } = query {
                    if !(1..=16).contains(limit) {
                        return Err(failure("model_access_page_limit"));
                    }
                    if let Some(cursor) = cursor
                        && (!is_hex_id(&cursor.source_hash)
                            || !is_hex_id(&cursor.owner_id)
                            || !is_hex_id(&cursor.db_instance)
                            || !is_hex_id(&cursor.engine_epoch))
                    {
                        return Err(failure("model_access_cursor"));
                    }
                }
                if let OwnerRead::ConversationHistory {
                    project_id,
                    cursor,
                    limit,
                } = query
                {
                    crate::conversation::validate_history(project_id, cursor.as_ref(), *limit)?;
                }
                if let OwnerRead::Page { limit, cursor, .. } = query {
                    if !(1..=32).contains(limit) {
                        return Err(failure("owner_page_limit"));
                    }
                    if let Some(cursor) = cursor {
                        RequestId::new(cursor.after_id.clone())
                            .map_err(|_| failure("owner_page_cursor"))?;
                    }
                }
            }
            OwnerOperation::Reconcile {
                expected_hash: Some(hash),
                ..
            } if !is_hex_id(hash) => return Err(failure("owner_hash")),
            _ => {}
        }
        Ok(())
    }

    pub fn mutation_hash(&self) -> Result<String, EngineError> {
        self.validate()?;
        if !matches!(self.operation, OwnerOperation::Apply { .. }) {
            return Err(failure("owner_mutation_required"));
        }
        Ok(crate::memory::hash(
            &serde_json::to_vec(self).map_err(|_| failure("owner_request_encoding"))?,
        ))
    }
}
