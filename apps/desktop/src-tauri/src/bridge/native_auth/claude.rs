use super::{BridgeError, NativeBridge, NativeAuthQuery, auth_id, failure};
use crazyagent::engine::owner::{AccountSecret, ClaudeAuthInput, NativeAuthAction, NativeAuthMethod};

impl NativeBridge {
    pub fn claude_auth_secret(code: String) -> Result<AccountSecret, BridgeError> {
        let mut bytes = code.into_bytes();
        let separator = bytes.iter().position(|byte| *byte == b'#');
        let valid = bytes.len() <= 8192
            && bytes.iter().all(u8::is_ascii_graphic)
            && separator.is_some_and(|index| index > 0 && index + 1 < bytes.len())
            && bytes.iter().filter(|byte| **byte == b'#').count() == 1;
        if !valid {
            bytes.fill(0);
            return Err(failure("native_auth_claude_input_invalid"));
        }
        AccountSecret::new(bytes).map_err(|_| failure("native_auth_claude_input_invalid"))
    }

    pub async fn submit_claude_auth_code(
        &mut self,
        ticket: &str,
        secret: AccountSecret,
    ) -> Result<(), BridgeError> {
        if self.profile_pending() {
            return Err(failure("account_profile_recovery_required"));
        }
        let (scope, _) = self.native_auth_selection(ticket)?;
        let operation_id = self.auth_operation(&scope)?;
        let epoch = self.controller()?.identity().engine_epoch().to_owned();
        let operation = self.native_auth.as_mut()
            .ok_or_else(|| failure("native_auth_operation_missing"))?;
        let (prompt_id, observed_epoch) = operation.claude_prompt.as_ref()
            .ok_or_else(|| failure("native_auth_claude_input_not_pending"))?;
        if operation.claude_input_attempted || observed_epoch != &epoch || *prompt_id != 1
            || operation.original.action() != (NativeAuthAction::SignIn { method: NativeAuthMethod::ClaudeOfficial })
        {
            return Err(failure("native_auth_claude_input_not_pending"));
        }
        let generation = operation.generation
            .ok_or_else(|| failure("native_auth_claude_input_not_pending"))?;
        let request_id = auth_id()?;
        let request = ClaudeAuthInput {
            query: NativeAuthQuery { request_id: request_id.clone(), scope, operation_id },
            generation,
            prompt_id: *prompt_id,
        };
        operation.claude_input_attempted = true;
        operation.claude_prompt = None;
        operation.request_id = request_id;
        assert!(operation.claude_input_attempted && operation.claude_prompt.is_none());
        assert_ne!(operation.original.id(), &operation.request_id);
        let result = self.controller()?.submit_claude_auth_code(request, secret).await;
        match result {
            Ok(()) => Ok(()),
            Err(error) => {
                let error = failure(match error.0.as_str() {
                    "native_auth_claude_input_invalid" => "native_auth_claude_input_invalid",
                    "native_auth_claude_input_not_pending" => "native_auth_claude_input_not_pending",
                    "controller_pending_intent" => "pending_intent",
                    "controller_disconnected" => "not_connected",
                    "owner_grant_stale_or_revoked" | "owner_revoked" => "owner_revoked",
                    _ => "native_auth_claude_input_outcome_unknown",
                });
                Err(self.auth_failure(error))
            }
        }
    }
}
