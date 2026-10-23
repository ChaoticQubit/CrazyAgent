use super::{BridgeError, NativeBridge, bounded, failure, native_random_id};
use crazyagent::config::{
    AccountPool, ModelAuthentication, ModelConnection, ModelEndpoint, ModelReference,
    ProviderQuotaPolicy,
};
use crazyagent::engine::EngineError;
use crazyagent::engine::owner::{
    AccountAuthCapabilitiesReply, AccountProfileOutcome, AccountProfileReply,
    AccountProfileRequest, AccountSecret, ModelAccessCollection,
};
use crazyagent::protocol::{CredentialRef, RequestId};
use serde::Serialize;
use std::time::Duration;

#[derive(Debug, Serialize)]
pub struct ConfiguredAccountPlan {
    pub provider: String,
    pub label: String,
    pub connection: ModelConnection,
    pub pool: AccountPool,
}

#[derive(Debug, Serialize)]
pub struct AccountProfileView {
    pub ticket: String,
    pub generation: String,
    pub reply: AccountProfileReply,
    pub pending: bool,
}

pub(super) struct Operation {
    request: AccountProfileRequest,
    connection: ModelConnection,
    ticket: String,
    generation: u64,
    pending: bool,
}

impl NativeBridge {
    pub async fn prepare_configured_account(
        &mut self,
        provider: &str,
        label: &str,
        endpoint: &str,
        model: &str,
    ) -> Result<ConfiguredAccountPlan, BridgeError> {
        if !self.status().connected {
            return Err(failure("not_connected"));
        }
        self.allow_new_auth()?;
        if self.status().pending {
            return Err(failure("pending_intent"));
        }
        validate_inputs(provider, label, endpoint, model)?;
        let page = self
            .model_access_page(ModelAccessCollection::Accounts, false)
            .await?;
        if !page.fresh || !page.writable {
            return Err(failure("settings_read_only"));
        }
        assert!(self.controller.is_some());
        assert!(!self.status().pending);
        configured_plan(provider, label, endpoint, model, &native_random_id()?)
    }

    pub async fn account_auth_capabilities(
        &mut self,
        ticket: &str,
    ) -> Result<AccountAuthCapabilitiesReply, BridgeError> {
        let (scope, connection) = self.selected_model_connection(ticket)?;
        let request = AccountProfileRequest {
            request_id: profile_id()?,
            scope,
        };
        let reply = self
            .controller()?
            .read_account_auth_capabilities(request.clone())
            .await
            .map_err(profile_error)?;
        if reply.request_id != request.request_id
            || reply.connection != connection
            || reply.config_id != request.scope.expected_config_id
            || reply.source_hash != request.scope.expected_source_hash
            || reply.database_instance != request.scope.expected_database_instance
        {
            return Err(failure("account_profile_identity_mismatch"));
        }
        assert_eq!(reply.connection.id, request.scope.connection);
        assert_eq!(reply.request_id, request.request_id);
        bounded(reply)
    }

    pub async fn configure_account_profile(
        &mut self,
        ticket: &str,
        secret: Option<AccountSecret>,
        generation: u64,
    ) -> Result<AccountProfileView, BridgeError> {
        self.allow_new_auth()?;
        if generation == 0 || generation > 9_007_199_254_740_991 {
            return Err(failure("draft_generation_limit"));
        }
        let (scope, connection) = self.selected_model_connection(ticket)?;
        validate_profile_connection(&connection, secret.is_some())?;
        let request = AccountProfileRequest {
            request_id: profile_id()?,
            scope,
        };
        self.account_profile = Some(Operation {
            request: request.clone(),
            connection,
            ticket: ticket.into(),
            generation,
            pending: true,
        });
        assert!(self.profile_pending());
        assert_eq!(
            self.account_profile
                .as_ref()
                .expect("retained profile")
                .request,
            request
        );
        let result = tokio::time::timeout(
            Duration::from_secs(10),
            self.controller()?
                .configure_account_profile(request, secret, generation),
        )
        .await;
        match result {
            Ok(result) => self.finish_account_profile(result),
            Err(_) => Err(failure("account_profile_outcome_unknown_read_same_request")),
        }
    }

    pub async fn read_account_profile(&mut self) -> Result<AccountProfileView, BridgeError> {
        let request = self
            .account_profile
            .as_ref()
            .ok_or_else(|| failure("account_profile_operation_missing"))?
            .request
            .clone();
        let result = self.controller()?.read_account_profile(request).await;
        self.finish_account_profile(result)
    }

    pub(super) fn profile_pending(&self) -> bool {
        self.account_profile
            .as_ref()
            .is_some_and(|operation| operation.pending)
            || self
                .controller
                .as_ref()
                .is_some_and(|controller| controller.pending_account_profile().is_some())
    }

    fn finish_account_profile(
        &mut self,
        result: Result<AccountProfileReply, EngineError>,
    ) -> Result<AccountProfileView, BridgeError> {
        let controller = self
            .controller
            .as_ref()
            .ok_or_else(|| failure("not_connected"))?;
        let pending = controller.pending_account_profile().is_some();
        let identity = controller.identity();
        let operation = self
            .account_profile
            .as_mut()
            .ok_or_else(|| failure("account_profile_operation_missing"))?;
        operation.pending = pending;
        let reply = result.map_err(profile_error)?;
        let scope = &operation.request.scope;
        if reply.request_id != operation.request.request_id
            || reply.database_instance != scope.expected_database_instance
            || reply.database_instance != identity.database_instance()
            || reply.engine_epoch != identity.engine_epoch()
        {
            operation.pending = true;
            return Err(failure("account_profile_identity_mismatch"));
        }
        if let AccountProfileOutcome::ConfiguredOnly { receipt, .. } = &reply.outcome {
            if receipt.connection != operation.connection
                || receipt.config_id != scope.expected_config_id
                || receipt.source_hash != scope.expected_source_hash
                || receipt.custody_id.is_empty()
                || receipt.profile_generation == 0
                || receipt.profile_config_sha256.len() != 64
            {
                operation.pending = true;
                return Err(failure("account_profile_identity_mismatch"));
            }
        }
        assert_eq!(reply.request_id, operation.request.request_id);
        assert_eq!(operation.connection.id, scope.connection);
        bounded(AccountProfileView {
            ticket: operation.ticket.clone(),
            generation: operation.generation.to_string(),
            reply,
            pending: operation.pending,
        })
    }
}

fn validate_inputs(
    provider: &str,
    label: &str,
    endpoint: &str,
    model: &str,
) -> Result<(), BridgeError> {
    if !matches!(provider, "api_key" | "local") {
        return Err(failure("invalid_account_provider"));
    }
    if label.trim().len() > 96 || unsafe_text(label) {
        return Err(failure("invalid_account_label"));
    }
    if model.trim().is_empty() || model.trim().len() > 256 || unsafe_text(model) {
        return Err(failure("invalid_account_model"));
    }
    validate_endpoint(endpoint, provider == "local")
}

fn unsafe_text(value: &str) -> bool {
    value.chars().any(|value| {
        value.is_control()
            || matches!(value,
        '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
    })
}

fn validate_endpoint(value: &str, local: bool) -> Result<(), BridgeError> {
    if value.len() > 2048
        || value
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || c == '\\')
    {
        return Err(failure("invalid_account_endpoint"));
    }
    let url = tauri::Url::parse(value).map_err(|_| failure("invalid_account_endpoint"))?;
    if !matches!(url.scheme(), "http" | "https")
        || !url.has_host()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(failure("invalid_account_endpoint"));
    }
    if local {
        let host = url
            .host_str()
            .ok_or_else(|| failure("invalid_account_endpoint"))?;
        let loopback = host == "localhost"
            || host
                .trim_matches(['[', ']'])
                .parse::<std::net::IpAddr>()
                .is_ok_and(|address| address.is_loopback());
        if url.scheme() != "http" || !loopback {
            return Err(failure("invalid_account_endpoint"));
        }
    }
    Ok(())
}

fn configured_plan(
    provider: &str,
    label: &str,
    endpoint: &str,
    model: &str,
    nonce: &str,
) -> Result<ConfiguredAccountPlan, BridgeError> {
    let reference = |kind: &str| {
        CredentialRef::new(format!("rcli-{kind}-{nonce}"))
            .map_err(|_| failure("account_identity_unavailable"))
    };
    let author = reference_value("openai")?;
    let connection = ModelConnection {
        id: reference("connection")?,
        provider: author.clone(),
        harness: reference_value("rcli")?,
        protocol: reference_value("openai-compatible")?,
        account_ref: reference("account")?,
        endpoint: ModelEndpoint::Http {
            url: endpoint.into(),
        },
        model: ModelReference {
            id: model.trim().into(),
            author,
        },
        auth: if provider == "local" {
            ModelAuthentication::None {}
        } else {
            ModelAuthentication::ApiKey {
                credential_ref: reference("profile")?,
            }
        },
    };
    let pool = AccountPool {
        id: reference("pool")?,
        name: None,
        members: vec![connection.id.clone()],
        projects: Vec::new(),
        roles: Vec::new(),
        fallback_allowed: false,
        provider_quota_policy: ProviderQuotaPolicy::BoundedAttempt,
    };
    assert_eq!(nonce.len(), 64);
    assert_eq!(pool.members, vec![connection.id.clone()]);
    assert!(pool.projects.is_empty() && pool.roles.is_empty());
    assert_ne!(connection.id, connection.account_ref);
    bounded(ConfiguredAccountPlan {
        provider: provider.into(),
        label: label.trim().into(),
        connection,
        pool,
    })
}

fn reference_value(value: &str) -> Result<CredentialRef, BridgeError> {
    CredentialRef::new(value).map_err(|_| failure("account_identity_unavailable"))
}

fn validate_profile_connection(
    connection: &ModelConnection,
    keyed: bool,
) -> Result<(), BridgeError> {
    if connection.harness.as_str() != "rcli" {
        return Err(failure("account_profile_harness_unsupported"));
    }
    let local = match &connection.auth {
        ModelAuthentication::None {} if !keyed => true,
        ModelAuthentication::ApiKey { .. } if keyed => false,
        _ => return Err(failure("account_profile_auth_unsupported")),
    };
    let ModelEndpoint::Http { url } = &connection.endpoint else {
        return Err(failure("invalid_account_endpoint"));
    };
    validate_endpoint(url, local)
}

fn profile_id() -> Result<RequestId, BridgeError> {
    RequestId::new(native_random_id()?).map_err(|_| failure("account_identity_unavailable"))
}

fn profile_error(error: EngineError) -> BridgeError {
    failure(match error.0.as_str() {
        "controller_disconnected" => "not_connected",
        "owner_revoked" => "owner_revoked",
        "account_secret_invalid" => "account_secret_invalid",
        "account_profile_config_changed" => "account_profile_config_changed",
        "account_profile_connection_changed" => "account_profile_connection_changed",
        "account_profile_auth_unsupported" => "account_profile_auth_unsupported",
        "account_profile_harness_unsupported" => "account_profile_harness_unsupported",
        "account_profile_endpoint_unsupported" => "invalid_account_endpoint",
        "native_auth_host_unavailable" => "native_auth_host_unavailable",
        "native_profile_in_use" => "native_profile_in_use",
        "account_profile_outcome_unknown" => "account_profile_outcome_unknown_read_same_request",
        "account_profile_request_conflict" => "account_profile_request_conflict",
        "account_profile_record_invalid" => "account_profile_record_invalid",
        "account_profile_secret_required_or_unexpected" => {
            "account_profile_secret_required_or_unexpected"
        }
        "account_profile_operations_full" => "account_profile_operations_full",
        "account_profile_reservation_missing" => "account_profile_reservation_missing",
        "account_profile_actor_changed" => "account_profile_actor_changed",
        "account_profile_config_invalid" => "account_profile_config_invalid",
        "rcli_private_key_writer_unavailable" => "rcli_private_key_writer_unavailable",
        "rcli_profile_binding_conflict" => "rcli_profile_binding_conflict",
        "rcli_profile_config_changed" => "rcli_profile_config_changed",
        "rcli_profile_config_unsafe" => "rcli_profile_config_unsafe",
        "rcli_profile_configuration_invalid" => "rcli_profile_configuration_invalid",
        "rcli_profile_home_unsafe" => "rcli_profile_home_unsafe",
        "rcli_profile_identity_changed" => "rcli_profile_identity_changed",
        "rcli_profile_not_configurable" => "rcli_profile_not_configurable",
        "account_profile_outcome_unknown_read_same_request" => {
            "account_profile_outcome_unknown_read_same_request"
        }
        _ => {
            let native = super::native_auth::auth_error(error);
            return if native.code == "native_auth_outcome_unknown_read_same_operation" {
                failure("account_profile_rejected_read_same_request")
            } else {
                native
            };
        }
    })
}
