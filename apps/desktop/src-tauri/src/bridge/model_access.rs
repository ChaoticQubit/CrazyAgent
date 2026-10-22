use super::*;
use crazyagent::config::{ModelAccessEdit, ModelAuthentication, ModelConnection, ModelEndpoint};
use crazyagent::engine::owner::{ModelAccessCollection, ModelAccessRecord, NativeAuthScope};
use crazyagent::engine::owner::{ModelAccessCursor, OwnerReadReply};
use crazyagent::protocol::CredentialRef;

pub(super) struct Selection {
    generation: u64,
    state: OwnerReadReply,
}

#[derive(Debug, Serialize)]
pub struct ModelAccessPage {
    pub ticket: String,
    pub collection: ModelAccessCollection,
    pub revision: String,
    pub config_id: String,
    pub source_hash: String,
    pub writable: bool,
    pub fresh: bool,
    pub items: Vec<ModelAccessRecord>,
    pub has_more: bool,
}

#[derive(Debug, Serialize)]
pub struct ModelAccessDetail {
    pub ticket: String,
    pub collection: ModelAccessCollection,
    pub revision: String,
    pub config_id: String,
    pub source_hash: String,
    pub writable: bool,
    pub fresh: bool,
    pub record: ModelAccessRecord,
}

impl NativeBridge {
    pub(super) fn native_auth_selection(
        &self,
        ticket: &str,
    ) -> Result<(NativeAuthScope, ModelConnection), BridgeError> {
        let (scope, connection) = self.selected_model_connection(ticket)?;
        if connection.protocol.as_str() != "native"
            || !matches!(connection.endpoint, ModelEndpoint::Native {})
            || !matches!(connection.auth, ModelAuthentication::NativeManaged { .. })
            || !matches!(connection.harness.as_str(), "codex" | "claude")
        {
            return Err(failure("native_profile_not_managed"));
        }
        Ok((scope, connection))
    }

    pub(super) fn selected_model_connection(
        &self,
        ticket: &str,
    ) -> Result<(NativeAuthScope, ModelConnection), BridgeError> {
        if !self.status().connected {
            return Err(failure("not_connected"));
        }
        if self.status().pending {
            return Err(failure("pending_intent"));
        }
        let selected = self
            .model_access
            .as_ref()
            .filter(|value| ticket.len() <= 20 && ticket == value.generation.to_string())
            .ok_or_else(|| failure("stale_model_access"))?;
        let controller = self
            .controller
            .as_ref()
            .ok_or_else(|| failure("not_connected"))?;
        if !controller
            .view()
            .is_some_and(|value| value.fresh && value.state == selected.state)
        {
            return Err(failure("stale_model_access"));
        }
        let OwnerData::ModelAccessRecord {
            source_hash,
            record: ModelAccessRecord::Connection { connection },
            ..
        } = &selected.state.data
        else {
            return Err(failure("stale_model_access"));
        };
        Ok((
            NativeAuthScope {
                expected_database_instance: controller.identity().database_instance().into(),
                connection: connection.id.clone(),
                expected_config_id: selected.state.config_id.clone(),
                expected_source_hash: source_hash.clone(),
            },
            connection.clone(),
        ))
    }

    pub async fn model_access_page(
        &mut self,
        collection: ModelAccessCollection,
        next: bool,
    ) -> Result<ModelAccessPage, BridgeError> {
        self.selected = None;
        self.model_access = None;
        let cursor: Option<ModelAccessCursor> = if next {
            Some(
                self.model_access_next
                    .take()
                    .filter(|cursor| cursor.collection == collection)
                    .ok_or_else(|| failure("no_next_page"))?,
            )
        } else {
            self.model_access_next = None;
            None
        };
        let state = self
            .read_model_access(OwnerRead::ModelAccessPage {
                collection,
                cursor,
                limit: PAGE_SIZE,
            })
            .await?;
        let OwnerData::ModelAccessPage {
            collection: actual,
            source_hash,
            writable,
            items,
            next,
        } = &state.data
        else {
            return Err(failure("resync_required"));
        };
        if *actual != collection
            || items.len() > usize::from(PAGE_SIZE)
            || items
                .iter()
                .any(|item| record_collection(item) != collection)
        {
            return Err(failure("unexpected_reply"));
        }
        let page = bounded(ModelAccessPage {
            ticket: self.generation.to_string(),
            collection,
            revision: state.revision.value().to_string(),
            config_id: state.config_id.as_str().into(),
            source_hash: source_hash.clone(),
            writable: *writable,
            fresh: true,
            items: items.clone(),
            has_more: next.is_some(),
        })?;
        self.model_access_next = next.clone();
        self.model_access = Some(Selection {
            generation: self.generation,
            state,
        });
        assert!(page.items.len() <= usize::from(PAGE_SIZE));
        assert!(page.fresh);
        Ok(page)
    }

    pub async fn inspect_model_access(
        &mut self,
        collection: ModelAccessCollection,
        id: &str,
    ) -> Result<ModelAccessDetail, BridgeError> {
        self.selected = None;
        self.model_access = None;
        let id = CredentialRef::new(id).map_err(|_| failure("invalid_model_access"))?;
        let state = self
            .read_model_access(OwnerRead::ModelAccessRecord {
                collection,
                id: id.clone(),
            })
            .await?;
        let OwnerData::ModelAccessRecord {
            source_hash,
            writable,
            record,
        } = &state.data
        else {
            return Err(failure("resync_required"));
        };
        if record.id() != &id || record_collection(record) != collection {
            return Err(failure("unexpected_reply"));
        }
        let detail = bounded(ModelAccessDetail {
            ticket: self.generation.to_string(),
            collection,
            revision: state.revision.value().to_string(),
            config_id: state.config_id.as_str().into(),
            source_hash: source_hash.clone(),
            writable: *writable,
            fresh: true,
            record: record.clone(),
        })?;
        self.model_access = Some(Selection {
            generation: self.generation,
            state,
        });
        assert_eq!(detail.record.id(), &id);
        assert!(detail.fresh);
        Ok(detail)
    }

    async fn read_model_access(&mut self, query: OwnerRead) -> Result<OwnerReadReply, BridgeError> {
        self.generation = self
            .generation
            .checked_add(1)
            .ok_or_else(|| failure("selection_limit"))?;
        if !self.status().connected {
            return Err(failure("not_connected"));
        }
        let state = self.controller()?.read(query).await.map_err(|error| {
            failure(match error.0.as_str() {
                "settings_source_conflict" => "settings_source_conflict",
                "settings_publication_pending" => "settings_publication_pending",
                "model_access_record_not_found" => "model_access_record_not_found",
                "owner_revoked" => "owner_revoked",
                _ => "owner_read_failed",
            })
        })?;
        assert!(self.generation > 0);
        assert!(self.controller()?.view().is_some());
        Ok(state)
    }

    pub async fn edit_model_access(
        &mut self,
        ticket: &str,
        edit: ModelAccessEdit,
        generation: u64,
    ) -> Result<IntentReceipt, BridgeError> {
        self.allow_new_auth()?;
        if self.status().pending {
            return Err(failure("pending_intent"));
        }
        if generation == 0 || generation > 9_007_199_254_740_991 {
            return Err(failure("draft_generation_limit"));
        }
        edit.validate()
            .map_err(|_| failure("invalid_model_access"))?;
        if let Some(selected) = &self.model_access {
            assert!(
                selected.generation > 0,
                "metadata selection requires an advanced generation"
            );
            assert!(
                matches!(
                    selected.state.data,
                    OwnerData::ModelAccessPage { .. } | OwnerData::ModelAccessRecord { .. }
                ),
                "metadata selection requires an actual metadata read"
            );
        }
        let selected = self
            .model_access
            .as_ref()
            .filter(|selected| ticket.len() <= 20 && ticket == selected.generation.to_string())
            .ok_or_else(|| failure("stale_model_access"))?;
        let view = self
            .controller
            .as_ref()
            .and_then(|controller| controller.view());
        if !self.status().connected
            || !view.is_some_and(|view| view.fresh && view.state == selected.state)
        {
            return Err(failure("stale_model_access"));
        }
        let (source_hash, writable) = match &selected.state.data {
            OwnerData::ModelAccessPage {
                collection,
                source_hash,
                writable,
                ..
            } if matches!(
                (&edit, collection),
                (
                    ModelAccessEdit::AddAccount { .. },
                    ModelAccessCollection::Accounts
                ) | (
                    ModelAccessEdit::CreateConnection { .. },
                    ModelAccessCollection::Connections
                ) | (
                    ModelAccessEdit::CreatePool { .. },
                    ModelAccessCollection::Pools
                )
            ) =>
            {
                (source_hash, writable)
            }
            OwnerData::ModelAccessRecord {
                record,
                source_hash,
                writable,
            } if edit_targets_record(&edit, record) => (source_hash, writable),
            _ => return Err(failure("stale_model_access")),
        };
        if !writable {
            return Err(failure("settings_read_only"));
        }
        let mutation = OwnerMutation::ModelAccess {
            expected_config_id: selected.state.config_id.clone(),
            expected_source_hash: source_hash.clone(),
            edit,
        };
        self.controller()?
            .begin_mutation(mutation, generation)
            .map_err(|_| failure("owner_not_ready"))?;
        self.model_access = None;
        self.model_access_next = None;
        assert!(self.status().pending);
        assert!(
            self.pending_intent()?
                .is_some_and(|intent| intent.kind == "model_access")
        );
        self.deliver_intent(false).await
    }
}

fn record_collection(record: &ModelAccessRecord) -> ModelAccessCollection {
    match record {
        ModelAccessRecord::Account { .. } => ModelAccessCollection::Accounts,
        ModelAccessRecord::Connection { .. } => ModelAccessCollection::Connections,
        ModelAccessRecord::Pool { .. } => ModelAccessCollection::Pools,
    }
}

fn edit_targets_record(edit: &ModelAccessEdit, record: &ModelAccessRecord) -> bool {
    match (edit, record) {
        (
            ModelAccessEdit::RemoveAccount { account_ref },
            ModelAccessRecord::Account {
                account_ref: actual,
            },
        ) => account_ref == actual,
        (
            ModelAccessEdit::ReplaceConnection { connection },
            ModelAccessRecord::Connection { connection: actual },
        ) => connection.id == actual.id,
        (
            ModelAccessEdit::RemoveConnection { id },
            ModelAccessRecord::Connection { connection },
        ) => id == &connection.id,
        (ModelAccessEdit::ReplacePool { pool }, ModelAccessRecord::Pool { pool: actual }) => {
            pool.id == actual.id
        }
        (ModelAccessEdit::RemovePool { id }, ModelAccessRecord::Pool { pool }) => id == &pool.id,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crazyagent::engine::bootstrap::EngineService;
    use std::os::unix::fs::PermissionsExt;

    async fn actual_read_bridge() -> (tempfile::TempDir, NativeBridge) {
        let root = tempfile::Builder::new()
            .prefix("ca-dma-invariant-")
            .tempdir_in("/private/tmp")
            .unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let (_stop, stopped) = tokio::sync::watch::channel(false);
        let mut service = EngineService::start_with_settings_sources(
            root.path(),
            "e.sock",
            &root.path().join("g"),
            &root.path().join("p"),
            &stopped,
        )
        .await
        .unwrap();
        let mut bridge = NativeBridge::new(Some(root.path().join("e.sock")));
        bridge.connect().await.unwrap();
        bridge
            .model_access_page(ModelAccessCollection::Accounts, false)
            .await
            .unwrap();
        bridge.baselines(false).await.unwrap();
        service.shutdown().await.unwrap();
        (root, bridge)
    }

    #[tokio::test]
    #[should_panic(expected = "metadata selection requires an advanced generation")]
    async fn internal_metadata_selection_generation_asserts_actual_read_corruption() {
        let (_root, mut bridge) = actual_read_bridge().await;
        bridge.model_access.as_mut().unwrap().generation = 0;
        let _ = bridge
            .edit_model_access(
                "0",
                ModelAccessEdit::AddAccount {
                    account_ref: CredentialRef::new("account").unwrap(),
                },
                1,
            )
            .await;
    }

    #[tokio::test]
    #[should_panic(expected = "metadata selection requires an actual metadata read")]
    async fn internal_metadata_selection_kind_asserts_cross_view_corruption() {
        let (_root, mut bridge) = actual_read_bridge().await;
        let actual_baselines = bridge
            .controller
            .as_ref()
            .unwrap()
            .view()
            .unwrap()
            .state
            .clone();
        let selected = bridge.model_access.as_mut().unwrap();
        let ticket = selected.generation.to_string();
        selected.state = actual_baselines;
        let _ = bridge
            .edit_model_access(
                &ticket,
                ModelAccessEdit::AddAccount {
                    account_ref: CredentialRef::new("account").unwrap(),
                },
                1,
            )
            .await;
    }
}
