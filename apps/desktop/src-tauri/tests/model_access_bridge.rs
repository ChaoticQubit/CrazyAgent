use crazyagent::config::{
    AccountPool, ModelAuthentication, ModelConnection, ModelEndpoint, ModelReference,
};
use crazyagent::config::{ModelAccessEdit, resolve_config};
use crazyagent::engine::bootstrap::EngineService;
use crazyagent::engine::owner::ModelAccessCollection;
use crazyagent::protocol::CredentialRef;
use crazyagent_desktop::bridge::IntentReceipt;
use crazyagent_desktop::bridge::{ActionOutcome, NativeBridge};
use std::os::unix::fs::PermissionsExt;
use std::time::Duration;

type TestResult<T> = Result<T, Box<dyn std::error::Error>>;

struct Harness {
    root: tempfile::TempDir,
    service: EngineService,
    bridge: NativeBridge,
}

impl Harness {
    async fn start(writable: bool) -> TestResult<Self> {
        let root = tempfile::Builder::new()
            .prefix("ca-dma-")
            .tempdir_in("/private/tmp")?;
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700))?;
        let global = root.path().join("global.toml");
        std::fs::write(
            &global,
            "version  = 1\n\n[knowledge]\ntext_bytes_max = 2048\n",
        )?;
        std::fs::set_permissions(&global, std::fs::Permissions::from_mode(0o600))?;
        let project = root.path().join("project.toml");
        let (_stop, stopped) = tokio::sync::watch::channel(false);
        let service = if writable {
            EngineService::start_with_settings_sources(
                root.path(),
                "e.sock",
                &global,
                &project,
                &stopped,
            )
            .await?
        } else {
            EngineService::start(
                root.path(),
                "e.sock",
                &resolve_config(&global, &project)?,
                &stopped,
            )
            .await?
        };
        let mut bridge = NativeBridge::new(Some(root.path().join("e.sock")));
        bridge.connect().await?;
        Ok(Self {
            root,
            service,
            bridge,
        })
    }

    async fn close(mut self) {
        let cleanup = tokio::time::timeout(Duration::from_secs(5), self.service.shutdown()).await;
        assert!(
            matches!(cleanup, Ok(Ok(()))),
            "actual owner cleanup required"
        );
    }

    async fn submit(
        &mut self,
        edit: ModelAccessEdit,
        generation: u64,
    ) -> TestResult<IntentReceipt> {
        let (collection, id) = match &edit {
            ModelAccessEdit::AddAccount { .. } => (ModelAccessCollection::Accounts, None),
            ModelAccessEdit::RemoveAccount { account_ref } => {
                (ModelAccessCollection::Accounts, Some(account_ref.as_str()))
            }
            ModelAccessEdit::CreateConnection { .. } => (ModelAccessCollection::Connections, None),
            ModelAccessEdit::ReplaceConnection { connection } => (
                ModelAccessCollection::Connections,
                Some(connection.id.as_str()),
            ),
            ModelAccessEdit::RemoveConnection { id } => {
                (ModelAccessCollection::Connections, Some(id.as_str()))
            }
            ModelAccessEdit::CreatePool { .. } => (ModelAccessCollection::Pools, None),
            ModelAccessEdit::ReplacePool { pool } => {
                (ModelAccessCollection::Pools, Some(pool.id.as_str()))
            }
            ModelAccessEdit::RemovePool { id } => (ModelAccessCollection::Pools, Some(id.as_str())),
            ModelAccessEdit::AddConnectedAccount { .. } => {
                (ModelAccessCollection::Connections, None)
            }
            ModelAccessEdit::RemoveConnectedAccount { id } => {
                (ModelAccessCollection::Connections, Some(id.as_str()))
            }
        };
        let ticket = if let Some(id) = id {
            self.bridge
                .inspect_model_access(collection, id)
                .await?
                .ticket
        } else {
            self.bridge
                .model_access_page(collection, false)
                .await?
                .ticket
        };
        Ok(self
            .bridge
            .edit_model_access(&ticket, edit, generation)
            .await?)
    }
}

fn reference(id: &str) -> CredentialRef {
    CredentialRef::new(id).unwrap()
}

fn connection() -> ModelConnection {
    ModelConnection {
        id: reference("connection"),
        provider: reference("provider"),
        harness: reference("harness"),
        protocol: reference("openai"),
        account_ref: reference("account"),
        endpoint: ModelEndpoint::Http {
            url: "https://example.test/v1".into(),
        },
        model: ModelReference {
            id: "author/model".into(),
            author: reference("author"),
        },
        auth: ModelAuthentication::ApiKey {
            credential_ref: reference("stored-reference"),
        },
    }
}

fn applied(receipt: &IntentReceipt) -> TestResult<()> {
    if receipt.outcome != ActionOutcome::Applied || receipt.pending {
        return Err(format!("actual publication required: {receipt:?}").into());
    }
    Ok(())
}

#[tokio::test]
async fn desktop_all_eight_edits_preserve_real_metadata_and_enforce_dependencies() {
    use ModelAccessEdit::*;
    let mut h = Harness::start(true).await.unwrap();
    let result: TestResult<_> = async {
        applied(
            &h.submit(
                AddAccount {
                    account_ref: reference("account"),
                },
                1,
            )
            .await?,
        )?;
        applied(
            &h.submit(
                CreateConnection {
                    connection: connection(),
                },
                2,
            )
            .await?,
        )?;
        let mut replacement = connection();
        replacement.model.id = "author/replacement".into();
        applied(
            &h.submit(
                ReplaceConnection {
                    connection: replacement.clone(),
                },
                3,
            )
            .await?,
        )?;
        let detail = h
            .bridge
            .inspect_model_access(ModelAccessCollection::Connections, "connection")
            .await?;
        if !serde_json::to_string(&detail)?.contains("author/replacement") {
            return Err("full actual replacement must be visible".into());
        }
        let pool = AccountPool {
            id: reference("pool"),
            name: None,
            members: vec![reference("connection")],
            projects: vec![reference("project")],
            roles: vec![reference("builder")],
            fallback_allowed: false,
            provider_quota_policy: crazyagent::config::ProviderQuotaPolicy::BoundedAttempt,
        };
        applied(&h.submit(CreatePool { pool: pool.clone() }, 4).await?)?;
        let rejected = h
            .submit(
                RemoveConnection {
                    id: reference("connection"),
                },
                5,
            )
            .await?;
        if rejected.outcome != ActionOutcome::Rejected
            || rejected.pending
            || rejected.rejection != Some("invalid_model_access")
        {
            return Err(format!("real dependency rejection required: {rejected:?}").into());
        }
        let in_use = h
            .submit(
                RemovePool {
                    id: reference("pool"),
                },
                6,
            )
            .await?;
        if in_use.outcome != ActionOutcome::Rejected || in_use.rejection != Some("pool_in_use") {
            return Err(format!("an assigned pool must not be removed: {in_use:?}").into());
        }
        let mut pool = pool;
        pool.roles = vec![reference("reviewer")];
        pool.projects = vec![];
        pool.fallback_allowed = true;
        applied(&h.submit(ReplacePool { pool }, 7).await?)?;
        let actual = resolve_config(
            &h.root.path().join("global.toml"),
            &h.root.path().join("project.toml"),
        )?
        .settings
        .model_access
        .ok_or("real catalog missing")?;
        if actual.connections != vec![replacement]
            || !actual.pools[0].fallback_allowed
            || actual.pools[0].roles[0].as_str() != "reviewer"
        {
            return Err("actual pool/connection replacement differs".into());
        }
        applied(
            &h.submit(
                RemovePool {
                    id: reference("pool"),
                },
                8,
            )
            .await?,
        )?;
        applied(
            &h.submit(
                RemoveConnection {
                    id: reference("connection"),
                },
                9,
            )
            .await?,
        )?;
        applied(
            &h.submit(
                RemoveAccount {
                    account_ref: reference("account"),
                },
                10,
            )
            .await?,
        )?;
        let text = std::fs::read_to_string(h.root.path().join("global.toml"))?;
        if !text.starts_with("version  = 1") || !text.contains("text_bytes_max = 2048") {
            return Err("unrelated settings formatting lost".into());
        }
        let page = h
            .bridge
            .model_access_page(ModelAccessCollection::Accounts, false)
            .await?;
        if !page.items.is_empty() || page.revision != "8" {
            return Err("eight actual changes required".into());
        }
        Ok(())
    }
    .await;
    h.close().await;
    result.unwrap();
}

#[tokio::test]
async fn desktop_read_only_stale_and_wrong_record_tickets_never_publish() {
    let mut h = Harness::start(false).await.unwrap();
    let result: TestResult<_> = async {
        let page = h
            .bridge
            .model_access_page(ModelAccessCollection::Accounts, false)
            .await?;
        if page.writable {
            return Err("read-only host must remain read-only".into());
        }
        let error = h
            .bridge
            .edit_model_access(
                &page.ticket,
                ModelAccessEdit::AddAccount {
                    account_ref: reference("denied"),
                },
                1,
            )
            .await
            .unwrap_err();
        if error.code != "settings_read_only" || h.bridge.status().pending {
            return Err("read-only request must not prepare".into());
        }
        Ok(())
    }
    .await;
    h.close().await;
    result.unwrap();
    let mut h = Harness::start(true).await.unwrap();
    let result: TestResult<_> = async {
        let old = h
            .bridge
            .model_access_page(ModelAccessCollection::Accounts, false)
            .await?;
        h.bridge
            .model_access_page(ModelAccessCollection::Accounts, false)
            .await?;
        let error = h
            .bridge
            .edit_model_access(
                &old.ticket,
                ModelAccessEdit::AddAccount {
                    account_ref: reference("stale"),
                },
                1,
            )
            .await
            .unwrap_err();
        if error.code != "stale_model_access" {
            return Err("refresh must expire ticket".into());
        }
        applied(
            &h.submit(
                ModelAccessEdit::AddAccount {
                    account_ref: reference("account"),
                },
                2,
            )
            .await?,
        )?;
        let detail = h
            .bridge
            .inspect_model_access(ModelAccessCollection::Accounts, "account")
            .await?;
        let error = h
            .bridge
            .edit_model_access(
                &detail.ticket,
                ModelAccessEdit::RemoveAccount {
                    account_ref: reference("other"),
                },
                3,
            )
            .await
            .unwrap_err();
        if error.code != "stale_model_access" {
            return Err("uninspected identity was admitted".into());
        }
        h.bridge.connect().await?;
        let error = h
            .bridge
            .edit_model_access(
                &detail.ticket,
                ModelAccessEdit::RemoveAccount {
                    account_ref: reference("account"),
                },
                4,
            )
            .await
            .unwrap_err();
        if error.code != "stale_model_access" || h.bridge.status().pending {
            return Err("reconnect must expire ticket".into());
        }
        Ok(())
    }
    .await;
    h.close().await;
    result.unwrap();
}

#[tokio::test]
async fn desktop_reports_actual_source_conflict_without_losing_the_engine_reason() {
    let mut h = Harness::start(true).await.unwrap();
    let result: TestResult<_> = async {
        let page = h
            .bridge
            .model_access_page(ModelAccessCollection::Accounts, false)
            .await?;
        let path = h.root.path().join("global.toml");
        let original = std::fs::read(&path)?;
        std::fs::write(&path, "version = 1\n[knowledge]\ntext_bytes_max = 4096\n")?;
        let receipt = h
            .bridge
            .edit_model_access(
                &page.ticket,
                ModelAccessEdit::AddAccount {
                    account_ref: reference("conflicted"),
                },
                1,
            )
            .await?;
        if receipt.outcome != ActionOutcome::Rejected
            || receipt.pending
            || receipt.rejection != Some("settings_source_conflict")
        {
            return Err(format!("actual source-conflict reason required: {receipt:?}").into());
        }
        if std::fs::read_to_string(&path)?.contains("conflicted") {
            return Err("conflicted edit was published".into());
        }
        std::fs::write(&path, original)?;
        applied(
            &h.submit(
                ModelAccessEdit::AddAccount {
                    account_ref: reference("recovered"),
                },
                2,
            )
            .await?,
        )?;
        Ok(())
    }
    .await;
    h.close().await;
    result.unwrap();
}

fn sqlite(root: &std::path::Path, sql: &str) -> TestResult<String> {
    let result = crazyagent::spawn_gate::output(
        std::process::Command::new("/usr/bin/sqlite3")
            .arg(root.join("engine.db"))
            .arg(sql)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped()),
    )?;
    if !result.status.success() {
        return Err("actual SQLite command failed".into());
    }
    Ok(String::from_utf8(result.stdout)?.trim().into())
}

#[tokio::test]
async fn desktop_cancelled_metadata_keeps_exact_intent_across_navigation_reconnect_and_retry() {
    let mut h = Harness::start(true).await.unwrap();
    let result: TestResult<_> = async {
        let page = h
            .bridge
            .model_access_page(ModelAccessCollection::Accounts, false)
            .await?;
        let edit = ModelAccessEdit::AddAccount {
            account_ref: reference("cancelled"),
        };
        let suspended = {
            use std::future::Future;
            let mut send = Box::pin(h.bridge.edit_model_access(&page.ticket, edit.clone(), 41));
            std::future::poll_fn(|context| {
                std::task::Poll::Ready(send.as_mut().poll(context).is_pending())
            })
            .await
        };
        if !suspended {
            return Err("actual owner I/O did not suspend".into());
        }
        let original = serde_json::to_string(&h.bridge.pending_intent()?)?;
        h.bridge.projects(false).await?;
        if h.bridge.reconcile().await.unwrap_err().code != "unexpected_pending_intent" {
            return Err("legacy baseline recovery dispatched metadata".into());
        }
        h.bridge.connect().await?;
        if original != serde_json::to_string(&h.bridge.pending_intent()?)? {
            return Err("reconnect changed exact retained metadata".into());
        }
        let page = h
            .bridge
            .model_access_page(ModelAccessCollection::Accounts, false)
            .await?;
        let error = h
            .bridge
            .edit_model_access(
                &page.ticket,
                ModelAccessEdit::AddAccount {
                    account_ref: reference("other"),
                },
                42,
            )
            .await
            .unwrap_err();
        if error.code != "pending_intent" {
            return Err("pending edit was replaced".into());
        }
        let receipt = h.bridge.reconcile_intent().await?;
        let receipt = if receipt.pending {
            h.bridge.retry_intent().await?
        } else {
            receipt
        };
        applied(&receipt)?;
        if receipt.intent.generation != "41" || receipt.intent.model_access != Some(edit) {
            return Err("settled intent differs from exact submitted metadata".into());
        }
        let page = h
            .bridge
            .model_access_page(ModelAccessCollection::Accounts, false)
            .await?;
        if page.items.len() != 1 || page.items[0].id().as_str() != "cancelled" {
            return Err("actual retained edit missing or duplicated".into());
        }
        if sqlite(h.root.path(), "SELECT COUNT(*) FROM owner_operations;")? != "1" {
            return Err("exact edit must record once".into());
        }
        Ok(())
    }
    .await;
    h.close().await;
    result.unwrap();
}

#[tokio::test]
async fn desktop_unknown_publication_recovers_real_journal_before_and_after_rename() {
    for before_rename in [true, false] {
        let mut h = Harness::start(true).await.unwrap();
        let result: TestResult<_> = async {
            let page = h.bridge.model_access_page(ModelAccessCollection::Accounts, false).await?;
            let trigger = if before_rename {
                "CREATE TRIGGER publication_abort BEFORE UPDATE ON settings_publication BEGIN SELECT RAISE(ABORT, 'actual_journal_abort'); END;"
            } else {
                "CREATE TRIGGER publication_abort BEFORE INSERT ON owner_operations BEGIN SELECT RAISE(ABORT, 'actual_ledger_abort'); END;"
            };
            sqlite(h.root.path(), trigger)?;
            let edit = ModelAccessEdit::AddAccount { account_ref: reference("recovered") };
            let receipt = h.bridge.edit_model_access(&page.ticket, edit.clone(), 77).await?;
            if receipt.outcome != ActionOutcome::Unknown || !receipt.pending { return Err(format!("actual interrupted settings journal required: {receipt:?}").into()); }
            let original = serde_json::to_string(&h.bridge.pending_intent()?)?;
            if sqlite(h.root.path(), "SELECT COUNT(*) FROM settings_publication;")? != "1" { return Err("durable publication journal missing".into()); }
            let bytes = std::fs::read_to_string(h.root.path().join("global.toml"))?;
            if bytes.contains("recovered") == before_rename { return Err("actual rename boundary differs".into()); }
            let receipt = h.bridge.reconcile_intent().await?;
            if receipt.outcome != ActionOutcome::Unknown || !receipt.pending { return Err("unrecovered journal must remain unknown".into()); }
            h.service.shutdown().await?;
            sqlite(h.root.path(), "DROP TRIGGER publication_abort;")?;
            let (_stop, stopped) = tokio::sync::watch::channel(false);
            h.service = EngineService::start_with_settings_sources(h.root.path(), "e.sock", &h.root.path().join("global.toml"), &h.root.path().join("project.toml"), &stopped).await?;
            h.bridge.connect().await?;
            if original != serde_json::to_string(&h.bridge.pending_intent()?)? { return Err("journal recovery replaced the pending identity".into()); }
            let receipt = h.bridge.reconcile_intent().await?;
            applied(&receipt)?;
            if receipt.intent.generation != "77" || receipt.intent.model_access != Some(edit) { return Err("journal recovery settled another edit".into()); }
            if sqlite(h.root.path(), "SELECT COUNT(*) FROM owner_operations;")? != "1" || sqlite(h.root.path(), "SELECT COUNT(*) FROM settings_publication;")? != "0" { return Err("recovery must finalize exactly one operation and clear journal".into()); }
            let page = h.bridge.model_access_page(ModelAccessCollection::Accounts, false).await?;
            if page.items.len() != 1 || page.items[0].id().as_str() != "recovered" { return Err("recovered metadata missing".into()); }
            Ok(())
        }.await;
        h.close().await;
        result.unwrap();
    }
}

#[tokio::test]
async fn desktop_metadata_pages_remain_bounded_and_refuse_stale_owner_cursors() {
    let mut h = Harness::start(true).await.unwrap();
    let result: TestResult<_> = async {
        for i in 0..18 {
            applied(
                &h.submit(
                    ModelAccessEdit::AddAccount {
                        account_ref: reference(&format!("account-{i:02}")),
                    },
                    i + 1,
                )
                .await?,
            )?;
        }
        let page = h
            .bridge
            .model_access_page(ModelAccessCollection::Accounts, false)
            .await?;
        if page.items.len() != 16 || !page.has_more || serde_json::to_vec(&page)?.len() > 65536 {
            return Err("bounded first metadata page required".into());
        }
        let json = serde_json::to_string(&page)?;
        for field in ["db_instance", "owner_id", "engine_epoch", "owner_epoch"] {
            if json.contains(field) {
                return Err("native identity leaked into frontend page".into());
            }
        }
        let page = h
            .bridge
            .model_access_page(ModelAccessCollection::Accounts, true)
            .await?;
        if page.items.len() != 2 || page.has_more {
            return Err("actual next page required".into());
        }
        h.bridge
            .model_access_page(ModelAccessCollection::Accounts, false)
            .await?;
        let mut other = NativeBridge::new(Some(h.root.path().join("e.sock")));
        other.connect().await?;
        let basis = other
            .model_access_page(ModelAccessCollection::Accounts, false)
            .await?;
        applied(
            &other
                .edit_model_access(
                    &basis.ticket,
                    ModelAccessEdit::AddAccount {
                        account_ref: reference("another-owner"),
                    },
                    1,
                )
                .await?,
        )?;
        if h.bridge
            .model_access_page(ModelAccessCollection::Accounts, true)
            .await
            .unwrap_err()
            .code
            != "resync_required"
        {
            return Err("stale owner cursor accepted".into());
        }
        let fresh = h
            .bridge
            .model_access_page(ModelAccessCollection::Accounts, false)
            .await?;
        let detail = h
            .bridge
            .inspect_model_access(ModelAccessCollection::Accounts, "account-00")
            .await?;
        h.bridge
            .inspect_model_access(ModelAccessCollection::Accounts, "missing")
            .await
            .unwrap_err();
        if h.bridge
            .edit_model_access(
                &detail.ticket,
                ModelAccessEdit::RemoveAccount {
                    account_ref: reference("account-00"),
                },
                99,
            )
            .await
            .unwrap_err()
            .code
            != "stale_model_access"
        {
            return Err("failed inspection left old authority active".into());
        }
        if fresh.revision != "19" {
            return Err("actual independent revision required".into());
        }
        Ok(())
    }
    .await;
    h.close().await;
    result.unwrap();
}

#[tokio::test]
async fn desktop_metadata_uses_the_actual_owner_and_durable_settings_source() {
    let root = tempfile::Builder::new()
        .prefix("ca-dmodel-")
        .tempdir_in("/private/tmp")
        .unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let global = root.path().join("global.toml");
    let project = root.path().join("project.toml");
    let (_stop, stopped) = tokio::sync::watch::channel(false);
    let mut service = EngineService::start_with_settings_sources(
        root.path(),
        "e.sock",
        &global,
        &project,
        &stopped,
    )
    .await
    .unwrap();
    let result = tokio::time::timeout(Duration::from_secs(20), async {
        let mut bridge = NativeBridge::new(Some(root.path().join("e.sock")));
        assert!(bridge.connect().await.unwrap().connected);
        let page = bridge
            .model_access_page(ModelAccessCollection::Accounts, false)
            .await
            .unwrap();
        assert!(page.fresh && page.writable && page.items.is_empty());
        let receipt = bridge
            .edit_model_access(
                &page.ticket,
                ModelAccessEdit::AddAccount {
                    account_ref: CredentialRef::new("desktop-account").unwrap(),
                },
                1,
            )
            .await
            .unwrap();
        assert_eq!(receipt.outcome, ActionOutcome::Applied);
        assert!(!receipt.pending);
        assert_eq!(receipt.intent.kind, "model_access");
        let page = bridge
            .model_access_page(ModelAccessCollection::Accounts, false)
            .await
            .unwrap();
        assert_eq!(page.items.len(), 1);
        assert_eq!(page.items[0].id().as_str(), "desktop-account");
        let config = resolve_config(&global, &project).unwrap();
        assert_eq!(
            config.settings.model_access.unwrap().accounts[0].as_str(),
            "desktop-account"
        );
        assert!(bridge.pending_intent().unwrap().is_none());
    })
    .await;
    let cleanup = tokio::time::timeout(Duration::from_secs(5), service.shutdown()).await;
    assert!(
        matches!(cleanup, Ok(Ok(()))),
        "actual owner cleanup required"
    );
    result.unwrap();
}
