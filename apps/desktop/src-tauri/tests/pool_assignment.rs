use crazyagent::client_controller::OwnerController;
use crazyagent::config::{AccountPool, ModelAccessEdit, ProviderQuotaPolicy};
use crazyagent::engine::bootstrap::EngineService;
use crazyagent::engine::owner::{
    ModelAccessCollection, ModelAccessRecord, OwnerClientKind, OwnerData, OwnerRead, OwnerView,
};
use crazyagent::protocol::{CredentialRef, ProjectSpec};
use crazyagent_desktop::bridge::{ActionOutcome, NativeBridge};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::Duration;

#[tokio::test]
async fn catalog_pool_binds_the_exact_native_project_through_replace_pool_and_survives_restart() {
    let root = tempfile::Builder::new()
        .prefix("ca-pool-bind-")
        .tempdir_in("/private/tmp")
        .unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let global = root.path().join("g.toml");
    let project = root.path().join("p.toml");
    std::fs::write(&global, "version = 1\n").unwrap();
    std::fs::set_permissions(&global, std::fs::Permissions::from_mode(0o600)).unwrap();
    let (_stop, stopped) = tokio::sync::watch::channel(false);
    let mut service = EngineService::start_with_settings_sources(
        root.path(), "e.sock", &global, &project, &stopped,
    ).await.unwrap();
    let path = root.path().to_path_buf();
    let mut body = tokio::spawn(async move { bind_existing_pool(&path).await });
    let result = tokio::time::timeout(Duration::from_secs(20), &mut body).await;
    if result.is_err() {
        body.abort();
        let _ = body.await;
    }
    let cleanup = tokio::time::timeout(Duration::from_secs(5), service.shutdown()).await;
    assert!(matches!(cleanup, Ok(Ok(()))), "actual first owner service must stop");
    let (expected, selected) = result.unwrap().unwrap().unwrap();
    let mut restarted = EngineService::start_with_settings_sources(
        root.path(), "e.sock", &global, &project, &stopped,
    ).await.unwrap();
    let path = root.path().to_path_buf();
    let mut body = tokio::spawn(async move {
        let mut bridge = NativeBridge::new(Some(path.join("e.sock")));
        assert!(bridge.connect().await.map_err(|error| error.code.to_owned())?.connected);
        let mut observer = OwnerController::attach(&path, OwnerClientKind::Tui, Duration::from_secs(2))
            .await.map_err(|error| error.to_string())?;
        let actual = read_pool(&mut observer, &expected.id).await?;
        assert_eq!(actual, expected);
        let inspected = bridge.inspect_model_access(ModelAccessCollection::Pools, expected.id.as_str())
            .await.map_err(|error| error.code.to_owned())?;
        assert!(inspected.fresh && inspected.writable);
        assert!(matches!(inspected.record, ModelAccessRecord::Pool { pool } if pool == expected));
        let opened = bridge.open_project(&selected).await.map_err(|error| error.code.to_owned())?;
        assert_eq!(opened.project_id, selected);
        assert!(opened.fresh);
        assert!(opened.brief.is_none());
        assert!(bridge.pending_intent().map_err(|error| error.code.to_owned())?.is_none());
        assert_no_work(&mut observer).await?;
        assert!(!path.join("p.toml").exists());
        for index in 0..2 {
            assert!(!path.join(format!("absent-{index}-a")).exists());
            assert!(!path.join(format!("absent-{index}-b")).exists());
        }
        Ok::<_, String>(())
    });
    let result = tokio::time::timeout(Duration::from_secs(15), &mut body).await;
    if result.is_err() {
        body.abort();
        let _ = body.await;
    }
    let cleanup = tokio::time::timeout(Duration::from_secs(5), restarted.shutdown()).await;
    assert!(matches!(cleanup, Ok(Ok(()))), "actual restarted owner service must stop");
    result.unwrap().unwrap().unwrap();
}

async fn bind_existing_pool(root: &Path) -> Result<(AccountPool, String), String> {
    let mut bridge = NativeBridge::new(Some(root.join("e.sock")));
    assert!(bridge.connect().await.map_err(|error| error.code.to_owned())?.connected);
    let mut observer = OwnerController::attach(root, OwnerClientKind::Tui, Duration::from_secs(2))
        .await.map_err(|error| error.to_string())?;
    assert!(bridge.projects(false).await.map_err(|error| error.code.to_owned())?.items.is_empty());
    let plan = bridge.prepare_managed_account("codex", "Before projects")
        .await.map_err(|error| error.code.to_owned())?;
    let mut catalog = plan.pool;
    catalog.fallback_allowed = true;
    catalog.provider_quota_policy = ProviderQuotaPolicy::Strict;
    assert!(catalog.projects.is_empty() && catalog.roles.is_empty());
    publish(&mut bridge, ModelAccessCollection::Accounts,
        ModelAccessEdit::AddAccount { account_ref: plan.connection.account_ref.clone() }, 1).await?;
    publish(&mut bridge, ModelAccessCollection::Connections,
        ModelAccessEdit::CreateConnection { connection: plan.connection.clone() }, 2).await?;
    publish(&mut bridge, ModelAccessCollection::Pools,
        ModelAccessEdit::CreatePool { pool: catalog.clone() }, 3).await?;
    assert_eq!(read_pool(&mut observer, &catalog.id).await?, catalog);
    assert!(bridge.projects(false).await.map_err(|error| error.code.to_owned())?.items.is_empty());
    let mut ids = Vec::new();
    for index in 0..2 {
        let spec = ProjectSpec {
            name: "Same project name".into(),
            repositories: vec![
                root.join(format!("absent-{index}-a")).to_string_lossy().into_owned(),
                root.join(format!("absent-{index}-b")).to_string_lossy().into_owned(),
            ],
        };
        let receipt = bridge.create_project(spec, 4 + index).await
            .map_err(|error| error.code.to_owned())?;
        assert_eq!(receipt.outcome, ActionOutcome::Applied);
        assert!(!receipt.pending);
        let id = receipt.intent.project_id.ok_or("actual project receipt required")?;
        let opened = bridge.open_project(&id).await.map_err(|error| error.code.to_owned())?;
        assert!(opened.fresh);
        assert_eq!(opened.project_id, id);
        ids.push(id);
    }
    assert_eq!(ids.len(), 2);
    assert_ne!(ids[0], ids[1]);
    let page = bridge.projects(false).await.map_err(|error| error.code.to_owned())?;
    assert!(page.fresh && !page.has_more);
    assert_eq!(page.items.len(), 2);
    assert!(page.items.iter().all(|row| row.name == "Same project name" && row.repository_count == 2));
    let selected = bridge.open_project(&ids[1]).await.map_err(|error| error.code.to_owned())?;
    assert_eq!(selected.project_id, ids[1]);
    let selected_ref = CredentialRef::new(selected.project_id.clone()).map_err(|error| error.to_string())?;
    assert_eq!(read_pool(&mut observer, &catalog.id).await?, catalog,
        "opening project choices must not silently bind a catalog pool");
    let mut prior = catalog.clone();
    prior.projects.push(CredentialRef::new(ids[0].clone()).map_err(|error| error.to_string())?);
    prior.roles.push(CredentialRef::new("builder").map_err(|error| error.to_string())?);
    let detail = bridge.inspect_model_access(ModelAccessCollection::Pools, prior.id.as_str())
        .await.map_err(|error| error.code.to_owned())?;
    let receipt = bridge.edit_model_access(&detail.ticket,
        ModelAccessEdit::ReplacePool { pool: prior.clone() }, 6).await
        .map_err(|error| error.code.to_owned())?;
    assert_eq!(receipt.outcome, ActionOutcome::Applied);
    assert!(!receipt.pending);
    assert_eq!(read_pool(&mut observer, &prior.id).await?, prior);
    let mut expected = prior.clone();
    expected.projects.push(selected_ref.clone());
    expected.roles.push(CredentialRef::new("project_leader").map_err(|error| error.to_string())?);
    let detail = bridge.inspect_model_access(ModelAccessCollection::Pools, prior.id.as_str())
        .await.map_err(|error| error.code.to_owned())?;
    let receipt = bridge.edit_model_access(&detail.ticket,
        ModelAccessEdit::ReplacePool { pool: expected.clone() }, 7).await
        .map_err(|error| error.code.to_owned())?;
    assert_eq!(receipt.outcome, ActionOutcome::Applied);
    assert!(!receipt.pending);
    assert_eq!(receipt.intent.generation, "7");
    assert!(matches!(receipt.intent.model_access, Some(ModelAccessEdit::ReplacePool { pool }) if pool == expected));
    assert_eq!(read_pool(&mut observer, &expected.id).await?, expected);
    assert_eq!(expected.id, catalog.id);
    assert_eq!(expected.members, catalog.members);
    assert_eq!(expected.fallback_allowed, catalog.fallback_allowed);
    assert_eq!(expected.provider_quota_policy, catalog.provider_quota_policy);
    assert_eq!(expected.projects, vec![prior.projects[0].clone(), selected_ref]);
    assert_eq!(expected.roles[0], prior.roles[0]);
    assert_eq!(expected.roles[1].as_str(), "project_leader");
    let inspected = bridge.inspect_model_access(ModelAccessCollection::Pools, expected.id.as_str())
        .await.map_err(|error| error.code.to_owned())?;
    assert!(inspected.fresh && inspected.writable);
    assert!(matches!(inspected.record, ModelAccessRecord::Pool { pool } if pool == expected));
    let persisted = crazyagent::config::resolve_config(&root.join("g.toml"), &root.join("p.toml"))
        .map_err(|error| error.to_string())?;
    assert_eq!(persisted.settings.model_access.unwrap().pools, vec![expected.clone()]);
    assert_no_work(&mut observer).await?;
    assert!(bridge.pending_intent().map_err(|error| error.code.to_owned())?.is_none());
    Ok((expected, selected.project_id))
}

async fn publish(
    bridge: &mut NativeBridge,
    collection: ModelAccessCollection,
    edit: ModelAccessEdit,
    generation: u64,
) -> Result<(), String> {
    let page = bridge.model_access_page(collection, false).await.map_err(|error| error.code.to_owned())?;
    assert!(page.fresh && page.writable);
    let receipt = bridge.edit_model_access(&page.ticket, edit, generation).await
        .map_err(|error| error.code.to_owned())?;
    assert_eq!(receipt.outcome, ActionOutcome::Applied);
    assert!(!receipt.pending);
    Ok(())
}

async fn read_pool(observer: &mut OwnerController, id: &CredentialRef) -> Result<AccountPool, String> {
    let reply = observer.read(OwnerRead::ModelAccessRecord {
        collection: ModelAccessCollection::Pools, id: id.clone(),
    }).await.map_err(|error| error.to_string())?;
    let OwnerData::ModelAccessRecord { record: ModelAccessRecord::Pool { pool }, writable, .. } = reply.data else {
        return Err("actual independent owner pool record required".into());
    };
    assert!(writable);
    assert_eq!(&pool.id, id);
    Ok(pool)
}

async fn assert_no_work(observer: &mut OwnerController) -> Result<(), String> {
    for view in [OwnerView::Tasks, OwnerView::Baselines, OwnerView::Plans, OwnerView::Candidates] {
        let reply = observer.read(OwnerRead::Page { view, cursor: None, limit: 16 })
            .await.map_err(|error| error.to_string())?;
        assert!(matches!(reply.data, OwnerData::Page { items, next } if items.is_empty() && next.is_none()));
    }
    Ok(())
}
