use super::*;
use crazyagent::accounts::{AccountDraft, AccountView, ProviderEntry};
use crazyagent::engine::EngineError;
use crazyagent::engine::owner::{AccountCheck, AccountPage, AccountSecret};
use crazyagent::protocol::CredentialRef;

const ACCOUNT_CODES: &[&str] = &[
    "not_connected",
    "owner_grant_stale_or_revoked",
    "account_name_invalid",
    "account_url_invalid",
    "account_url_required",
    "account_url_not_allowed",
    "account_url_unreachable",
    "account_local_url_required",
    "account_model_required",
    "account_model_invalid",
    "account_model_not_allowed",
    "account_models_unavailable",
    "account_key_required",
    "account_key_not_allowed",
    "account_key_invalid",
    "account_key_store_unavailable",
    "account_key_store_unsafe",
    "account_method_unavailable",
    "account_identity_unavailable",
    "account_not_found",
    "account_exists",
    "account_reply_invalid",
    "account_reply_too_large",
    "accounts_page_limit",
    "accounts_cursor_stale",
    "accounts_reply_bounds",
    "accounts_readonly",
    "connection_exists",
    "invalid_model_access",
    "settings_read_only",
    "settings_source_conflict",
    "settings_publication_pending",
    "stale_revision",
    "stale_config",
];

fn account_error(error: EngineError) -> BridgeError {
    let code = if error.0 == "controller_disconnected" {
        "not_connected"
    } else {
        error.0.as_str()
    };
    failure(
        ACCOUNT_CODES
            .iter()
            .copied()
            .find(|known| *known == code)
            .unwrap_or("accounts_outcome_unknown"),
    )
}

fn account_id(id: &str) -> Result<CredentialRef, BridgeError> {
    CredentialRef::new(id).map_err(|_| failure("account_not_found"))
}

impl NativeBridge {
    fn accounts(&self) -> Result<&OwnerController, BridgeError> {
        if !self.status().connected {
            return Err(failure("not_connected"));
        }
        self.controller
            .as_ref()
            .ok_or_else(|| failure("not_connected"))
    }

    /// Reads the provider catalog for the add-account menu.
    pub async fn account_catalog(&self) -> Result<Vec<ProviderEntry>, BridgeError> {
        self.accounts()?
            .account_catalog()
            .await
            .map_err(account_error)
    }

    /// Reads one page of accounts. Inputs: previous page's last id (or none) and page size.
    pub async fn list_accounts(
        &self,
        after: Option<String>,
        limit: u16,
    ) -> Result<AccountPage, BridgeError> {
        let after = after.as_deref().map(account_id).transpose()?;
        self.accounts()?
            .list_accounts(after, limit)
            .await
            .map_err(account_error)
    }

    /// Adds one account; the key goes to the engine and is not retained by the bridge.
    pub async fn add_account(
        &self,
        draft: AccountDraft,
        key: Option<String>,
    ) -> Result<AccountView, BridgeError> {
        self.allow_new_auth()?;
        let key = key
            .filter(|value| !value.is_empty())
            .map(|value| AccountSecret::new(value.into_bytes()))
            .transpose()
            .map_err(|_| failure("account_key_invalid"))?;
        self.accounts()?
            .add_account(draft, key)
            .await
            .map_err(account_error)
    }

    /// Renames one account. Inputs: account id and the new name.
    pub async fn rename_account(&self, id: &str, name: String) -> Result<AccountView, BridgeError> {
        self.allow_new_auth()?;
        self.accounts()?
            .rename_account(account_id(id)?, name)
            .await
            .map_err(account_error)
    }

    /// Removes one account that is in no pool. Input: account id.
    pub async fn remove_account(&self, id: &str) -> Result<(), BridgeError> {
        self.allow_new_auth()?;
        self.accounts()?
            .remove_account(account_id(id)?)
            .await
            .map_err(account_error)
    }

    /// Checks one account now. Input: account id. Output: fresh status and listed models.
    pub async fn check_account(&self, id: &str) -> Result<AccountCheck, BridgeError> {
        self.accounts()?
            .check_account(account_id(id)?)
            .await
            .map_err(account_error)
    }

    /// Lists models on a local model server. Input: loopback http address.
    pub async fn local_models(&self, url: String) -> Result<Vec<String>, BridgeError> {
        self.accounts()?
            .local_models(url)
            .await
            .map_err(account_error)
    }
}
