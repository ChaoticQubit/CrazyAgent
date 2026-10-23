use super::*;
use crate::accounts::{AccountDraft, AccountView, ProviderEntry};
use crate::engine::EngineError;
use crate::engine::owner::{AccountCheck, AccountPage, AccountSecret};
use crate::protocol::CredentialRef;

#[cfg_attr(
    not(test),
    expect(dead_code, reason = "built by the Settings > Accounts view")
)]
pub(in crate::tui) enum AccountsRequest {
    Catalog,
    List {
        after: Option<CredentialRef>,
        limit: u16,
    },
    Add {
        draft: AccountDraft,
        key: Option<AccountSecret>,
    },
    Rename {
        id: CredentialRef,
        name: String,
    },
    Remove {
        id: CredentialRef,
    },
    Check {
        id: CredentialRef,
    },
    LocalModels {
        url: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::tui) enum AccountsReply {
    Catalog(Vec<ProviderEntry>),
    Page(AccountPage),
    Account(AccountView),
    Removed(CredentialRef),
    Checked(AccountCheck),
    Models(Vec<String>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::tui) struct AccountsEvent {
    pub result: Result<AccountsReply, String>,
}

impl Connection {
    /// Queues one Settings > Accounts request on the paired owner connection. The reply
    /// arrives later as `OwnerEvent::Accounts`. Input: the request. Output: queued or why not.
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "called by the Settings > Accounts view")
    )]
    pub(in crate::tui) fn accounts(&mut self, request: AccountsRequest) -> Result<(), String> {
        self.owner_job(Job::Accounts(Box::new(request)))
    }
}

impl NativeOwner {
    /// Runs one accounts request through the owner controller and turns any engine error
    /// into its plain code. Input: the request. Output: the reply event.
    pub(super) async fn accounts(&mut self, request: AccountsRequest) -> AccountsEvent {
        let result = async {
            if !self.ready {
                return Err(EngineError("controller_disconnected".into()));
            }
            let controller = self
                .controller
                .as_ref()
                .ok_or_else(|| EngineError("controller_disconnected".into()))?;
            Ok(match request {
                AccountsRequest::Catalog => {
                    AccountsReply::Catalog(controller.account_catalog().await?)
                }
                AccountsRequest::List { after, limit } => {
                    AccountsReply::Page(controller.list_accounts(after, limit).await?)
                }
                AccountsRequest::Add { draft, key } => {
                    AccountsReply::Account(controller.add_account(draft, key).await?)
                }
                AccountsRequest::Rename { id, name } => {
                    AccountsReply::Account(controller.rename_account(id, name).await?)
                }
                AccountsRequest::Remove { id } => {
                    controller.remove_account(id.clone()).await?;
                    AccountsReply::Removed(id)
                }
                AccountsRequest::Check { id } => {
                    AccountsReply::Checked(controller.check_account(id).await?)
                }
                AccountsRequest::LocalModels { url } => {
                    AccountsReply::Models(controller.local_models(url).await?)
                }
            })
        }
        .await;
        AccountsEvent {
            result: result.map_err(|error: EngineError| bounded_problem(&error.0)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::accounts::{Provider, SignInMethod};
    use crate::engine::bootstrap::EngineService;
    use std::os::unix::fs::PermissionsExt;

    async fn owner_event(connection: &mut Connection) -> OwnerEvent {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
        loop {
            if let Some(event) = connection.poll_owner().unwrap() {
                return event;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "owner event timed out"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    async fn reply(
        connection: &mut Connection,
        request: AccountsRequest,
    ) -> Result<AccountsReply, String> {
        connection.accounts(request).unwrap();
        loop {
            if let OwnerEvent::Accounts(event) = owner_event(connection).await {
                return event.result;
            }
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn tui_accounts_requests_reach_the_real_engine() {
        let root = tempfile::Builder::new()
            .prefix("ca-tacct-")
            .tempdir_in("/private/tmp")
            .unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let global = root.path().join("global.toml");
        std::fs::write(&global, "version = 1\ntimeout_ms = 5000\n").unwrap();
        std::fs::set_permissions(&global, std::fs::Permissions::from_mode(0o600)).unwrap();
        let (_stop, stopped) = tokio::sync::watch::channel(false);
        let mut service = EngineService::start_with_settings_sources(
            root.path(),
            "e.sock",
            &global,
            &root.path().join("project.toml"),
            &stopped,
        )
        .await
        .unwrap();
        let mut connection = Connection::connect_paired(
            Client::new(root.path().join("e.sock"), Duration::from_secs(10)).unwrap(),
        )
        .unwrap();
        while !matches!(
            owner_event(&mut connection).await,
            OwnerEvent::Access { ready: true, .. }
        ) {}

        let Ok(AccountsReply::Catalog(catalog)) =
            reply(&mut connection, AccountsRequest::Catalog).await
        else {
            panic!("catalog reply required")
        };
        assert_eq!(catalog[0].name, "ChatGPT / Codex");
        let draft = AccountDraft {
            provider: Provider::Local,
            method: SignInMethod::Url,
            name: "Ollama".into(),
            url: None,
            model: Some("qwen2.5:0.5b".into()),
        };
        let Ok(AccountsReply::Account(local)) =
            reply(&mut connection, AccountsRequest::Add { draft, key: None }).await
        else {
            panic!("added account required")
        };
        assert_eq!(local.url.as_deref(), Some("http://127.0.0.1:11434/v1"));
        assert_eq!(local.status_label, "Signed in");
        let models = reply(
            &mut connection,
            AccountsRequest::LocalModels {
                url: "http://127.0.0.1:11434".into(),
            },
        )
        .await;
        let Ok(AccountsReply::Models(models)) = models else {
            panic!("models required: {models:?}")
        };
        assert!(models.iter().any(|model| model == "qwen2.5:0.5b"));
        let Ok(AccountsReply::Checked(checked)) = reply(
            &mut connection,
            AccountsRequest::Check {
                id: local.id.clone(),
            },
        )
        .await
        else {
            panic!("checked account required")
        };
        assert_eq!(checked.account.status_label, "Signed in");
        assert!(checked.models.iter().any(|model| model == "qwen2.5:0.5b"));
        let Ok(AccountsReply::Account(renamed)) = reply(
            &mut connection,
            AccountsRequest::Rename {
                id: local.id.clone(),
                name: "Laptop".into(),
            },
        )
        .await
        else {
            panic!("renamed account required")
        };
        assert_eq!(renamed.name, "Laptop");
        let Ok(AccountsReply::Page(page)) = reply(
            &mut connection,
            AccountsRequest::List {
                after: None,
                limit: 50,
            },
        )
        .await
        else {
            panic!("page required")
        };
        assert_eq!(page.accounts.len(), 1);
        assert_eq!(
            reply(
                &mut connection,
                AccountsRequest::Remove {
                    id: local.id.clone()
                }
            )
            .await,
            Ok(AccountsReply::Removed(local.id.clone()))
        );
        assert_eq!(
            reply(
                &mut connection,
                AccountsRequest::Remove {
                    id: local.id.clone()
                }
            )
            .await,
            Err("account_not_found".into())
        );
        drop(connection);
        tokio::time::timeout(Duration::from_secs(5), service.shutdown())
            .await
            .unwrap()
            .unwrap();
    }
}
