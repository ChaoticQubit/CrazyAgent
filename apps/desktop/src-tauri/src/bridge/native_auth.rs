use super::{BridgeError, NativeBridge, bounded, failure};
use crazyagent::config::ModelConnection;
use crazyagent::engine::EngineError;
use crazyagent::engine::owner::{
    NativeAuthAction, NativeAuthCancel, NativeAuthLogout, NativeAuthMethod, NativeAuthPhase,
    NativeAuthQuery, NativeAuthReply, NativeAuthScope, NativeAuthStart,
};
use crazyagent::protocol::RequestId;
use serde::Serialize;

mod claude;
mod presentation;

#[derive(Serialize)]
pub struct NativeAuthView {
    pub ticket: String,
    pub request_id: String,
    pub operation_id: String,
    pub database_instance: String,
    pub engine_epoch: String,
    pub connection: String,
    pub config_id: String,
    pub source_hash: String,
    pub generation: String,
    pub action: NativeAuthAction,
    pub phase: NativeAuthPhase,
    pub code: Option<String>,
    pub challenge: Option<presentation::Challenge>,
    pub account: Option<presentation::Account>,
    pub display_error: Option<&'static str>,
}

pub(super) struct Operation {
    original: Original,
    connection: ModelConnection,
    request_id: RequestId,
    generation: Option<u64>,
    unresolved: bool,
    claude_prompt: Option<(u64, String)>,
    claude_input_attempted: bool,
}

enum Original {
    Start(NativeAuthStart),
    Logout(NativeAuthLogout),
}

impl Original {
    fn scope(&self) -> &NativeAuthScope {
        match self {
            Self::Start(value) => &value.scope,
            Self::Logout(value) => &value.scope,
        }
    }

    fn id(&self) -> &RequestId {
        match self {
            Self::Start(value) => &value.request_id,
            Self::Logout(value) => &value.request_id,
        }
    }

    fn action(&self) -> NativeAuthAction {
        match self {
            Self::Start(value) => NativeAuthAction::SignIn {
                method: value.method,
            },
            Self::Logout(_) => NativeAuthAction::Logout,
        }
    }
}

impl NativeBridge {
    pub async fn start_native_auth(
        &mut self,
        ticket: &str,
        method: NativeAuthMethod,
    ) -> Result<NativeAuthView, BridgeError> {
        let (scope, connection) = self.native_auth_selection(ticket)?;
        self.allow_new_auth()?;
        let valid = match method {
            NativeAuthMethod::CodexBrowser
            | NativeAuthMethod::CodexDeviceCode
            | NativeAuthMethod::CodexExisting => "codex",
            NativeAuthMethod::ClaudeOfficial => "claude",
        };
        if connection.harness.as_str() != valid {
            return Err(failure("native_auth_method_mismatch"));
        }
        let request = NativeAuthStart {
            request_id: auth_id()?,
            scope,
            method,
        };
        self.retain_auth(Original::Start(request.clone()), connection);
        let result = self.controller()?.start_native_auth(request).await;
        self.finish_auth(ticket, result)
    }

    pub async fn read_native_auth(&mut self, ticket: &str) -> Result<NativeAuthView, BridgeError> {
        let (scope, _) = self.native_auth_selection(ticket)?;
        let operation_id = self.auth_operation(&scope)?;
        let request = NativeAuthQuery {
            request_id: self.auth_request_id()?,
            scope,
            operation_id,
        };
        let result = self.controller()?.read_native_auth(request).await;
        self.finish_auth(ticket, result)
    }

    pub async fn cancel_native_auth(
        &mut self,
        ticket: &str,
    ) -> Result<NativeAuthView, BridgeError> {
        let (scope, _) = self.native_auth_selection(ticket)?;
        let operation_id = self.auth_operation(&scope)?;
        let request = NativeAuthCancel {
            request_id: self.auth_request_id()?,
            scope,
            operation_id,
        };
        let result = self.controller()?.cancel_native_auth(request).await;
        self.finish_auth(ticket, result)
    }

    pub async fn logout_native_auth(
        &mut self,
        ticket: &str,
    ) -> Result<NativeAuthView, BridgeError> {
        let (scope, connection) = self.native_auth_selection(ticket)?;
        self.allow_new_auth()?;
        let request = NativeAuthLogout {
            request_id: auth_id()?,
            scope,
        };
        self.retain_auth(Original::Logout(request.clone()), connection);
        let result = self.controller()?.logout_native_auth(request).await;
        self.finish_auth(ticket, result)
    }

    pub(super) fn allow_new_auth(&self) -> Result<(), BridgeError> {
        if self.profile_pending() {
            return Err(failure("account_profile_recovery_required"));
        }
        if self
            .native_auth
            .as_ref()
            .is_some_and(|value| value.unresolved)
        {
            return Err(failure("native_auth_recovery_required"));
        }
        Ok(())
    }

    fn retain_auth(&mut self, original: Original, connection: ModelConnection) {
        let request_id = original.id().clone();
        self.native_auth = Some(Operation {
            original,
            connection,
            request_id,
            generation: None,
            unresolved: true,
            claude_prompt: None,
            claude_input_attempted: false,
        });
        let operation = self.native_auth.as_ref().expect("retained auth operation");
        assert_eq!(operation.original.id(), &operation.request_id);
        assert!(operation.unresolved);
    }

    fn auth_operation(&mut self, scope: &NativeAuthScope) -> Result<RequestId, BridgeError> {
        let operation = self
            .native_auth
            .as_mut()
            .ok_or_else(|| failure("native_auth_operation_missing"))?;
        let original = operation.original.scope();
        if original.expected_database_instance != scope.expected_database_instance {
            operation.unresolved = false;
            return Err(failure("native_auth_database_mismatch"));
        }
        if original.connection != scope.connection {
            return Err(failure("native_auth_operation_scope_conflict"));
        }
        if original != scope {
            operation.unresolved = false;
            return Err(failure("native_profile_source_conflict"));
        }
        Ok(operation.original.id().clone())
    }

    fn auth_request_id(&mut self) -> Result<RequestId, BridgeError> {
        let request = auth_id()?;
        let operation = self
            .native_auth
            .as_mut()
            .ok_or_else(|| failure("native_auth_operation_missing"))?;
        operation.request_id = request.clone();
        operation.claude_prompt = None;
        assert_ne!(operation.original.id(), &request);
        Ok(request)
    }

    fn finish_auth(
        &mut self,
        ticket: &str,
        result: Result<NativeAuthReply, EngineError>,
    ) -> Result<NativeAuthView, BridgeError> {
        if let Some(operation) = self.native_auth.as_mut() {
            operation.claude_prompt = None;
        }
        let reply = match result {
            Ok(reply) => reply,
            Err(error) => {
                let error = auth_error(error);
                if let Some(operation) = self.native_auth.as_mut() {
                    if error.code == "native_auth_outcome_unknown_read_same_operation" {
                        operation.unresolved = true;
                    } else if operation.generation.is_none()
                        || matches!(
                            error.code,
                            "native_auth_not_active"
                                | "native_auth_operation_missing"
                                | "native_auth_database_mismatch"
                                | "native_profile_source_conflict"
                        )
                    {
                        operation.unresolved = false;
                    }
                }
                return Err(self.auth_failure(error));
            }
        };
        if let Err(error) = self.validate_auth_reply(&reply) {
            if let Some(operation) = self.native_auth.as_mut() {
                operation.unresolved = true;
            }
            return Err(self.auth_failure(error));
        }
        let operation = self.native_auth.as_mut().expect("validated auth operation");
        operation.generation = Some(reply.fact.generation);
        operation.unresolved = reply.fact.phase.unfinished();
        let view = presentation::project(ticket, reply)?;
        if view.display_error.is_none()
            && let Some(presentation::Challenge::ClaudeOfficial {
                accepts_code: true,
                prompt_id: Some(prompt),
                ..
            }) = &view.challenge
        {
            operation.claude_prompt = Some((
                prompt
                    .parse()
                    .map_err(|_| failure("native_auth_cannot_display_safely"))?,
                view.engine_epoch.clone(),
            ));
        }
        Ok(view)
    }

    fn auth_failure(&mut self, error: BridgeError) -> BridgeError {
        if matches!(
            error.code,
            "not_connected"
                | "owner_revoked"
                | "native_auth_outcome_unknown_read_same_operation"
                | "native_auth_claude_input_outcome_unknown"
        ) {
            if let Some(controller) = self.controller.as_mut() {
                controller.detach();
            }
            self.ordinary = None;
            self.model_access = None;
            self.model_access_next = None;
        }
        error
    }

    fn validate_auth_reply(&self, reply: &NativeAuthReply) -> Result<(), BridgeError> {
        let operation = self
            .native_auth
            .as_ref()
            .ok_or_else(|| failure("native_auth_operation_missing"))?;
        let identity = self
            .controller
            .as_ref()
            .ok_or_else(|| failure("not_connected"))?
            .identity();
        let scope = operation.original.scope();
        let fact = &reply.fact;
        if reply.request_id != operation.request_id
            || &fact.operation_id != operation.original.id()
            || reply.database_instance != identity.database_instance()
            || reply.engine_epoch != identity.engine_epoch()
            || reply.database_instance != scope.expected_database_instance
            || fact.action != operation.original.action()
            || fact.binding.database_instance != scope.expected_database_instance
            || fact.binding.config_id != scope.expected_config_id
            || fact.binding.source_hash != scope.expected_source_hash
            || fact.binding.connection != operation.connection
            || fact.generation == 0
            || operation
                .generation
                .is_some_and(|generation| generation != fact.generation)
        {
            return Err(failure("native_auth_outcome_unknown_read_same_operation"));
        }
        Ok(())
    }
}

fn auth_id() -> Result<RequestId, BridgeError> {
    let id = super::native_random_id().map_err(|_| failure("native_auth_identity_unavailable"))?;
    RequestId::new(id).map_err(|_| failure("native_auth_identity_unavailable"))
}

pub(super) fn auth_error(error: EngineError) -> BridgeError {
    failure(match error.0.as_str() {
        "controller_disconnected" => "not_connected",
        "native_auth_database_mismatch" => "native_auth_database_mismatch",
        "native_auth_method_mismatch" => "native_auth_method_mismatch",
        "native_auth_operation_conflict" => "native_auth_operation_conflict",
        "native_auth_operation_scope_conflict" => "native_auth_operation_scope_conflict",
        "native_auth_operation_missing" => "native_auth_operation_missing",
        "native_auth_operation_capacity" => "native_auth_operation_capacity",
        "native_auth_not_active" => "native_auth_not_active",
        "native_auth_recovery_required" => "native_auth_recovery_required",
        "native_auth_readonly" => "native_auth_readonly",
        "native_profile_source_invalid" => "native_profile_source_invalid",
        "native_profile_source_conflict" => "native_profile_source_conflict",
        "native_profile_connection_missing" => "native_profile_connection_missing",
        "native_profile_not_managed" => "native_profile_not_managed",
        "native_profile_binding_conflict" => "native_profile_binding_conflict",
        "native_profile_custody_unsafe" => "native_profile_custody_unsafe",
        "native_profile_incomplete" => "native_profile_incomplete",
        "native_profile_identity_bounds" => "native_profile_identity_bounds",
        "native_profile_identity_changed" => "native_profile_identity_changed",
        "native_profile_identity_invalid" => "native_profile_identity_invalid",
        "native_profile_capacity" => "native_profile_capacity",
        "native_profile_new_home_not_empty" => "native_profile_new_home_not_empty",
        "native_profile_in_use" => "native_profile_in_use",
        "native_profile_lock_unavailable" => "native_profile_lock_unavailable",
        "owner_grant_stale_or_revoked" => "owner_revoked",
        "owner_revoked" => "owner_revoked",
        "owner_queue_not_submitted" => "owner_queue_not_submitted",
        _ => "native_auth_outcome_unknown_read_same_operation",
    })
}
