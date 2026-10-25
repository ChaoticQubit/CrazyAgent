use super::*;
use crate::accounts::{AccountDraft, AccountView, ProviderEntry};
use crate::engine::owner::{AccountCheck, AccountPage, AccountSecret};
use crate::protocol::CredentialRef;

impl OwnerController {
    fn accounts_client(&self) -> Result<&OwnerClient, EngineError> {
        self.client
            .as_ref()
            .ok_or_else(|| EngineError("controller_disconnected".into()))
    }

    /// Reads the provider catalog for the add-account menu. Output: providers and methods.
    pub async fn account_catalog(&self) -> Result<Vec<ProviderEntry>, EngineError> {
        self.accounts_client()?.account_catalog().await
    }

    /// Reads one page of accounts with plain statuses. Inputs: previous `next` and page size.
    pub async fn list_accounts(
        &self,
        after: Option<CredentialRef>,
        limit: u16,
    ) -> Result<AccountPage, EngineError> {
        self.accounts_client()?.list_accounts(after, limit).await
    }

    /// Adds one account; a key is handed to the engine and never kept here. Output: account.
    pub async fn add_account(
        &self,
        draft: AccountDraft,
        key: Option<AccountSecret>,
    ) -> Result<AccountView, EngineError> {
        self.accounts_client()?.add_account(draft, key).await
    }

    /// Renames one account. Inputs: account id and new name. Output: the renamed account.
    pub async fn rename_account(
        &self,
        id: CredentialRef,
        name: String,
    ) -> Result<AccountView, EngineError> {
        self.accounts_client()?.rename_account(id, name).await
    }

    /// Removes one account that is in no pool, and its saved key. Input: account id.
    pub async fn remove_account(&self, id: CredentialRef) -> Result<(), EngineError> {
        self.accounts_client()?.remove_account(id).await
    }

    /// Replaces the saved key of one account in place; the key goes to the engine and is
    /// never kept here. Inputs: account id and the new key. Output: the same account.
    pub async fn replace_account_key(
        &self,
        id: CredentialRef,
        key: AccountSecret,
    ) -> Result<AccountView, EngineError> {
        self.accounts_client()?.replace_account_key(id, key).await
    }

    /// Checks one account now and returns its fresh status and listed models.
    pub async fn check_account(&self, id: CredentialRef) -> Result<AccountCheck, EngineError> {
        self.accounts_client()?.check_account(id).await
    }

    /// Lists models pulled on a local model server. Input: loopback http address.
    pub async fn local_models(&self, url: String) -> Result<Vec<String>, EngineError> {
        self.accounts_client()?.local_models(url).await
    }
}
