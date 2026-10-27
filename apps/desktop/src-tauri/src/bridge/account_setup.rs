use super::*;
use crazyagent::config::{
    AccountPool, ModelAuthentication, ModelConnection, ModelEndpoint, ModelReference,
    ProviderQuotaPolicy,
};
use crazyagent::engine::owner::ModelAccessCollection;
use crazyagent::protocol::CredentialRef;

#[derive(Debug, Serialize)]
pub struct ManagedAccountPlan {
    pub provider: String,
    pub label: String,
    pub connection: ModelConnection,
    pub pool: AccountPool,
}

impl NativeBridge {
    pub async fn prepare_managed_account(
        &mut self,
        provider: &str,
        label: &str,
    ) -> Result<ManagedAccountPlan, BridgeError> {
        if !self.status().connected {
            return Err(failure("not_connected"));
        }
        self.allow_new_auth()?;
        if self.blocked() {
            return Err(failure("pending_intent"));
        }
        let author = match provider {
            "codex" => "openai",
            "claude" => "anthropic",
            _ => return Err(failure("invalid_account_provider")),
        };
        if label.chars().any(|value| {
            value.is_control()
                || matches!(
                    value,
                    '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}'
                        | '\u{2066}'..='\u{2069}'
                )
        }) || label.trim().len() > 96
        {
            return Err(failure("invalid_account_label"));
        }
        let page = self
            .model_access_page(ModelAccessCollection::Accounts, false)
            .await?;
        if !page.fresh || !page.writable {
            return Err(failure("settings_read_only"));
        }
        let nonce = native_random_id().map_err(|_| failure("account_identity_unavailable"))?;
        let reference = |kind: &str| {
            CredentialRef::new(format!("{provider}-{kind}-{nonce}"))
                .map_err(|_| failure("account_identity_unavailable"))
        };
        let connection = ModelConnection {
            id: reference("connection")?,
            provider: CredentialRef::new(author)
                .map_err(|_| failure("invalid_account_provider"))?,
            harness: CredentialRef::new(provider)
                .map_err(|_| failure("invalid_account_provider"))?,
            protocol: CredentialRef::new("native")
                .map_err(|_| failure("invalid_account_provider"))?,
            account_ref: reference("account")?,
            endpoint: ModelEndpoint::Native {},
            model: ModelReference {
                id: "native-default".into(),
                author: CredentialRef::new(author)
                    .map_err(|_| failure("invalid_account_provider"))?,
            },
            auth: ModelAuthentication::NativeManaged {
                profile_ref: reference("profile")?,
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
        let plan = bounded(ManagedAccountPlan {
            provider: provider.into(),
            label: label.trim().into(),
            connection,
            pool,
        })?;
        assert_eq!(plan.pool.members, vec![plan.connection.id.clone()]);
        assert!(plan.pool.projects.is_empty() && plan.pool.roles.is_empty());
        assert!(!self.status().pending);
        Ok(plan)
    }
}
