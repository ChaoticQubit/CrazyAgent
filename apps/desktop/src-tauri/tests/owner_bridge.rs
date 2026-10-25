#![cfg(unix)]

use crazyagent::client_controller::OwnerController;
use crazyagent::config::resolve_config;
use crazyagent::engine::Client;
use crazyagent::engine::bootstrap::EngineService;
use crazyagent::engine::owner::{
    OwnerClientKind, OwnerData, OwnerMutation, OwnerSummary, OwnerView,
};
use crazyagent::protocol::*;
use crazyagent::verification::*;
use crazyagent_desktop::bridge::NativeBridge;
use std::os::unix::fs::PermissionsExt;
use std::time::Duration;

#[tokio::test]
async fn metadata_reads_expire_prior_approval_inspections_even_when_the_read_fails() {
    use crazyagent::engine::owner::ModelAccessCollection;
    let root = tempfile::Builder::new()
        .prefix("ca-dmeta-approval-")
        .tempdir_in("/tmp")
        .unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let config = resolve_config(&root.path().join("g"), &root.path().join("p")).unwrap();
    let (_stop, stopped) = tokio::sync::watch::channel(false);
    let mut service = EngineService::start(root.path(), "e.sock", &config, &stopped)
        .await
        .unwrap();
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let (initial, _) = inspect_fixture(root.path()).await?;
        let mut bridge = NativeBridge::new(Some(root.path().join("e.sock")));
        bridge.connect().await?;
        let inspected = bridge.inspect(&initial.id).await?;
        bridge.model_access_page(ModelAccessCollection::Accounts, false).await?;
        if !matches!(bridge.approve(&inspected.ticket).await, Err(error) if error.code == "stale_selection") {
            return Err("metadata navigation must expire approval inspection".into());
        }
        let inspected = bridge.inspect(&initial.id).await?;
        bridge.inspect_model_access(ModelAccessCollection::Accounts, "missing").await.unwrap_err();
        if !matches!(bridge.approve(&inspected.ticket).await, Err(error) if error.code == "stale_selection") || bridge.status().pending {
            return Err("failed metadata inspection must expire approval authority".into());
        }
        Ok(())
    }.await;
    let cleanup = service.shutdown().await;
    assert!(cleanup.is_ok(), "real engine cleanup failed");
    result.unwrap();
}

fn baseline() -> BaselineDraft {
    BaselineDraft {
        version: 1,
        project_id: ProjectId::new("desktop-project").unwrap(),
        origin: BaselineOrigin::HumanAuthored,
        requirements: "Inspect <script>untrusted text</script> as literal text".into(),
        checks: vec![
            CheckSpec {
                id: RequestId::new("code").unwrap(),
                kind: CheckKind::Code,
                minimum_risk: Risk::Low,
                content: b"desktop behavior".to_vec(),
            },
            CheckSpec {
                id: RequestId::new("integrity").unwrap(),
                kind: CheckKind::TestIntegrity,
                minimum_risk: Risk::Low,
                content: b"test integrity".to_vec(),
            },
        ],
        rules: vec![],
        retry_limit: 0,
    }
}

#[tokio::test]
async fn inspection_reads_the_real_paired_owner_without_exposing_native_identity() {
    let root = tempfile::Builder::new()
        .prefix("ca-desktop-")
        .tempdir_in("/tmp")
        .unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let config = resolve_config(&root.path().join("g"), &root.path().join("p")).unwrap();
    let (_stop, stopped) = tokio::sync::watch::channel(false);
    let mut service = EngineService::start(root.path(), "e.sock", &config, &stopped)
        .await
        .unwrap();
    let result = inspect_fixture(root.path()).await;
    let cleanup = service.shutdown().await;
    assert!(cleanup.is_ok(), "real engine cleanup failed");
    let (detail, serialized) = result.unwrap();
    assert_eq!(detail.requirements, baseline().requirements);
    assert!(!detail.approved);
    assert!(!detail.revoked);
    assert!(!serialized.contains("db_instance"));
    assert!(!serialized.contains("engine_epoch"));
    assert!(!serialized.contains("owner_id"));
    assert!(!serialized.contains("owner.sock"));
    assert!(!serialized.contains(root.path().to_str().unwrap()));
}

async fn inspect_fixture(
    root: &std::path::Path,
) -> Result<(crazyagent_desktop::bridge::BaselineDetail, String), Box<dyn std::error::Error>> {
    let socket = root.join("e.sock");
    let client = Client::new(socket.clone(), Duration::from_secs(2))?;
    let (ordinary, mut control) =
        OwnerController::attach_configured(client, OwnerClientKind::Tui).await?;
    let created = ordinary
        .request(WireRequest {
            protocol_version: PROTOCOL_VERSION,
            client_id: ClientId::new("desktop-test")?,
            request_id: RequestId::new("desktop-project")?,
            expected_revision: Cursor::new(0),
            command: Command::CreateProject {
                project_id: ProjectId::new("desktop-project")?,
                spec: ProjectSpec {
                    name: "Desktop fixture".into(),
                    repositories: vec![],
                },
            },
        })
        .await?;
    if !matches!(created.outcome, Outcome::Applied) {
        return Err("actual project creation required".into());
    }
    control
        .read_page(OwnerView::Baselines, None, Some(16))
        .await?;
    control.begin_mutation(OwnerMutation::ProposeBaseline { draft: baseline() }, 1)?;
    let submitted = control.send_pending().await?;
    if !matches!(
        submitted,
        crazyagent::engine::owner::OwnerResponse::Applied {
            operation: crazyagent::engine::owner::OwnerApplied {
                result: crazyagent::engine::owner::OwnerResult::Applied,
                ..
            }
        }
    ) {
        return Err("actual baseline proposal required".into());
    }
    let page = control
        .read_page(OwnerView::Baselines, None, Some(16))
        .await?;
    let OwnerData::Page { items, .. } = page.data else {
        return Err("actual baseline page required".into());
    };
    if items.len() != 1 {
        return Err("one actual baseline required".into());
    }
    let OwnerSummary::Baseline { baseline_id, .. } = &items[0] else {
        return Err("actual baseline required".into());
    };
    let mut bridge = NativeBridge::new(Some(socket));
    bridge.connect().await?;
    let rows = bridge.baselines(false).await?;
    if rows.items.len() != 1 || rows.items[0].id != baseline_id.as_str() {
        return Err("actual desktop baseline required".into());
    }
    let detail = bridge.inspect(baseline_id.as_str()).await?;
    let serialized = serde_json::to_string(&detail)?;
    Ok((detail, serialized))
}

#[tokio::test]
async fn baseline_decisions_change_real_owner_state_and_preserve_revoked_history() {
    let root = tempfile::Builder::new()
        .prefix("ca-desktop-")
        .tempdir_in("/tmp")
        .unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let config = resolve_config(&root.path().join("g"), &root.path().join("p")).unwrap();
    let (_stop, stopped) = tokio::sync::watch::channel(false);
    let mut service = EngineService::start(root.path(), "e.sock", &config, &stopped)
        .await
        .unwrap();
    let result = decision_fixture(root.path(), false).await;
    let cleanup = service.shutdown().await;
    assert!(cleanup.is_ok(), "real engine cleanup failed");
    result.unwrap();
}

#[tokio::test]
async fn identical_reselection_and_refresh_invalidate_earlier_inspection_tickets() {
    let root = tempfile::Builder::new()
        .prefix("ca-desktop-")
        .tempdir_in("/tmp")
        .unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let config = resolve_config(&root.path().join("g"), &root.path().join("p")).unwrap();
    let (_stop, stopped) = tokio::sync::watch::channel(false);
    let mut service = EngineService::start(root.path(), "e.sock", &config, &stopped)
        .await
        .unwrap();
    let result = decision_fixture(root.path(), true).await;
    let cleanup = service.shutdown().await;
    assert!(cleanup.is_ok(), "real engine cleanup failed");
    result.unwrap();
}

async fn decision_fixture(
    root: &std::path::Path,
    invalidation: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    use crazyagent::engine::owner::OwnerRead;
    use crazyagent_desktop::bridge::ActionOutcome;

    let (initial, _) = inspect_fixture(root).await?;
    let mut bridge = NativeBridge::new(Some(root.join("e.sock")));
    bridge.connect().await?;
    let first = bridge.inspect(&initial.id).await?;
    if invalidation {
        let replacement = bridge.inspect(&initial.id).await?;
        if first.ticket == replacement.ticket {
            return Err("identical selection must receive a distinct generation".into());
        }
        let refused = bridge.approve(&first.ticket).await;
        if !matches!(refused, Err(error) if error.code == "stale_selection") {
            return Err("old selection ticket must be refused".into());
        }
        bridge.baselines(false).await?;
        let refused = bridge.approve(&replacement.ticket).await;
        if !matches!(refused, Err(error) if error.code == "stale_selection") {
            return Err("refresh must invalidate inspection ticket".into());
        }
        if bridge.status().pending {
            return Err("invalid selection must not create a pending mutation".into());
        }
        return Ok(());
    }
    let approval = bridge.approve(&first.ticket).await?;
    if approval.outcome != ActionOutcome::Applied || approval.pending {
        return Err("actual approval receipt required".into());
    }
    let client = Client::new(root.join("e.sock"), Duration::from_secs(2))?;
    let (_, mut observer) =
        OwnerController::attach_configured(client, OwnerClientKind::Tui).await?;
    let read = observer
        .read(OwnerRead::Baseline {
            baseline_id: BaselineId::new(&initial.id)?,
        })
        .await?;
    if !matches!(
        read.data,
        OwnerData::Baseline {
            approved: true,
            revoked: false,
            ..
        }
    ) {
        return Err("independent owner must observe actual approval".into());
    }
    let inspected = bridge.inspect(&initial.id).await?;
    let revocation = bridge.revoke(&inspected.ticket).await?;
    if revocation.outcome != ActionOutcome::Applied || revocation.pending {
        return Err("actual revocation receipt required".into());
    }
    let read = observer
        .read(OwnerRead::Baseline {
            baseline_id: BaselineId::new(&initial.id)?,
        })
        .await?;
    if !matches!(
        read.data,
        OwnerData::Baseline {
            approved: true,
            revoked: true,
            ..
        }
    ) {
        return Err("revocation must preserve actual approval history".into());
    }
    Ok(())
}

#[tokio::test]
async fn stale_decision_reports_recorded_rejection_and_requires_fresh_inspection() {
    let root = tempfile::Builder::new()
        .prefix("ca-desktop-")
        .tempdir_in("/tmp")
        .unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let config = resolve_config(&root.path().join("g"), &root.path().join("p")).unwrap();
    let (_stop, stopped) = tokio::sync::watch::channel(false);
    let mut service = EngineService::start(root.path(), "e.sock", &config, &stopped)
        .await
        .unwrap();
    let result = stale_fixture(root.path()).await;
    let cleanup = service.shutdown().await;
    assert!(cleanup.is_ok(), "real engine cleanup failed");
    result.unwrap();
}

async fn stale_fixture(root: &std::path::Path) -> Result<(), Box<dyn std::error::Error>> {
    use crazyagent_desktop::bridge::ActionOutcome;

    let (initial, _) = inspect_fixture(root).await?;
    let mut bridge = NativeBridge::new(Some(root.join("e.sock")));
    bridge.connect().await?;
    let inspected = bridge.inspect(&initial.id).await?;
    let client = Client::new(root.join("e.sock"), Duration::from_secs(2))?;
    let (_, mut other) = OwnerController::attach_configured(client, OwnerClientKind::Tui).await?;
    other
        .read_page(OwnerView::Baselines, None, Some(16))
        .await?;
    let mut changed = baseline();
    changed
        .requirements
        .push_str(" with an independently changed revision");
    other.begin_mutation(OwnerMutation::ProposeBaseline { draft: changed }, 1)?;
    let reply = other.send_pending().await?;
    if !matches!(
        reply,
        crazyagent::engine::owner::OwnerResponse::Applied {
            operation: crazyagent::engine::owner::OwnerApplied {
                result: crazyagent::engine::owner::OwnerResult::Applied,
                ..
            }
        }
    ) {
        return Err("independent real revision advance required".into());
    }
    let rejected = bridge.approve(&inspected.ticket).await?;
    if rejected.outcome != ActionOutcome::Rejected || rejected.pending {
        return Err("recorded stale rejection must settle without claiming an effect".into());
    }
    if !matches!(bridge.reconcile().await, Err(error) if error.code == "no_pending_intent") {
        return Err("a completed rejection cannot invent pending reconciliation".into());
    }
    if !matches!(bridge.retry().await, Err(error) if error.code == "no_pending_intent") {
        return Err("a completed rejection cannot silently replay".into());
    }
    if !matches!(bridge.approve(&inspected.ticket).await, Err(error) if error.code == "stale_selection")
    {
        return Err("a submitted inspection ticket cannot create a replacement intent".into());
    }
    let fresh = bridge.inspect(&initial.id).await?;
    if fresh.approved || fresh.revoked {
        return Err("stale decision must not change baseline state".into());
    }
    let explicit = bridge.approve(&fresh.ticket).await?;
    if explicit.outcome != ActionOutcome::Applied || explicit.pending {
        return Err("an explicit freshly inspected decision must use its own real receipt".into());
    }
    Ok(())
}

#[tokio::test]
async fn actual_desktop_revocation_blocks_decision_without_revoking_tui() {
    let root = tempfile::Builder::new()
        .prefix("ca-desktop-")
        .tempdir_in("/tmp")
        .unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let config = resolve_config(&root.path().join("g"), &root.path().join("p")).unwrap();
    let (_stop, stopped) = tokio::sync::watch::channel(false);
    let mut service = EngineService::start(root.path(), "e.sock", &config, &stopped)
        .await
        .unwrap();
    let result = match service.owner_runtime() {
        Some(runtime) => revocation_fixture(root.path(), runtime).await,
        None => Err("actual retained owner runtime required".into()),
    };
    let cleanup = service.shutdown().await;
    assert!(cleanup.is_ok(), "real engine cleanup failed");
    result.unwrap();
}

async fn revocation_fixture(
    root: &std::path::Path,
    runtime: &crazyagent::engine::owner::OwnerRuntimeHandle,
) -> Result<(), Box<dyn std::error::Error>> {
    use crazyagent::engine::owner::{OwnerControl, OwnerDecision, OwnerRead};
    use crazyagent_desktop::bridge::ActionOutcome;

    let (initial, _) = inspect_fixture(root).await?;
    let mut bridge = NativeBridge::new(Some(root.join("e.sock")));
    bridge.connect().await?;
    let inspected = bridge.inspect(&initial.id).await?;
    let tui_epoch = runtime.epoch(OwnerClientKind::Tui)?;
    let revoked = runtime
        .control(OwnerControl {
            operation_id: RequestId::new("desktop-revoke")?,
            target: OwnerClientKind::Desktop,
            expected_epoch: runtime.epoch(OwnerClientKind::Desktop)?,
            decision: OwnerDecision::Revoke,
        })
        .await?;
    if revoked.active || runtime.epoch(OwnerClientKind::Tui)? != tui_epoch {
        return Err("actual Desktop-only revocation required".into());
    }
    let denied = bridge.approve(&inspected.ticket).await?;
    if !denied.pending
        || !matches!(
            denied.outcome,
            ActionOutcome::Rejected | ActionOutcome::Uncertain
        )
    {
        return Err("the revoked actual client must not report a successful decision".into());
    }
    let client = Client::new(root.join("e.sock"), Duration::from_secs(2))?;
    let (_, mut observer) =
        OwnerController::attach_configured(client, OwnerClientKind::Tui).await?;
    let read = observer
        .read(OwnerRead::Baseline {
            baseline_id: BaselineId::new(&initial.id)?,
        })
        .await?;
    if !matches!(
        read.data,
        OwnerData::Baseline {
            approved: false,
            revoked: false,
            ..
        }
    ) {
        return Err("TUI must retain actual access and observe no approval effect".into());
    }
    let active = runtime
        .control(OwnerControl {
            operation_id: RequestId::new("desktop-reactivate")?,
            target: OwnerClientKind::Desktop,
            expected_epoch: runtime.epoch(OwnerClientKind::Desktop)?,
            decision: OwnerDecision::Reactivate,
        })
        .await?;
    if !active.active || active.owner_epoch <= revoked.owner_epoch {
        return Err("actual new-epoch reactivation required".into());
    }
    if !bridge.connect().await?.pending {
        return Err("reconnect must retain the original decision".into());
    }
    let barrier = bridge.reconcile().await?;
    if barrier.outcome != ActionOutcome::NotRecordedAtBarrier || !barrier.pending {
        return Err(
            "revocation recovery cannot invent nonapplication or a replacement intent".into(),
        );
    }
    Ok(())
}

#[tokio::test]
async fn cancellation_retains_prepared_intent_for_real_reconciliation_and_exact_retry() {
    let root = tempfile::Builder::new()
        .prefix("ca-desktop-")
        .tempdir_in("/tmp")
        .unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let config = resolve_config(&root.path().join("g"), &root.path().join("p")).unwrap();
    let (_stop, stopped) = tokio::sync::watch::channel(false);
    let mut service = EngineService::start(root.path(), "e.sock", &config, &stopped)
        .await
        .unwrap();
    let result = cancellation_fixture(root.path()).await;
    let cleanup = service.shutdown().await;
    assert!(cleanup.is_ok(), "real engine cleanup failed");
    result.unwrap();
}

async fn cancellation_fixture(root: &std::path::Path) -> Result<(), Box<dyn std::error::Error>> {
    use crazyagent::engine::owner::OwnerRead;
    use crazyagent_desktop::bridge::ActionOutcome;
    use std::future::{Future, poll_fn};
    use std::task::Poll;

    let (initial, _) = inspect_fixture(root).await?;
    let mut bridge = NativeBridge::new(Some(root.join("e.sock")));
    bridge.connect().await?;
    let detail = bridge.inspect(&initial.id).await?;
    let first = {
        let mut actual = Box::pin(bridge.approve(&detail.ticket));
        poll_fn(|context| Poll::Ready(actual.as_mut().poll(context))).await
    };
    if first.is_ready() || !bridge.status().pending {
        return Err("actual cancellation must leave prepared pending intent".into());
    }
    let replacement = bridge.inspect(&initial.id).await?;
    if !matches!(bridge.revoke(&replacement.ticket).await, Err(error) if error.code == "pending_intent")
    {
        return Err("a replacement cannot overwrite cancelled delivery intent".into());
    }
    let barrier = bridge.reconcile().await?;
    if barrier.outcome != ActionOutcome::NotRecordedAtBarrier || !barrier.pending {
        return Err("actual barrier absence must retain the cancelled exact request".into());
    }
    let receipt = bridge.retry().await?;
    if receipt.outcome != ActionOutcome::Applied || receipt.pending {
        return Err("explicit exact retry must obtain its real receipt".into());
    }
    let client = Client::new(root.join("e.sock"), Duration::from_secs(2))?;
    let (_, mut observer) =
        OwnerController::attach_configured(client, OwnerClientKind::Tui).await?;
    let read = observer
        .read(OwnerRead::Baseline {
            baseline_id: BaselineId::new(&initial.id)?,
        })
        .await?;
    if !matches!(
        read.data,
        OwnerData::Baseline {
            approved: true,
            revoked: false,
            ..
        }
    ) {
        return Err("independent real read must observe the retried decision".into());
    }
    Ok(())
}

#[tokio::test]
async fn pending_decision_refuses_a_different_real_database_at_the_same_configured_path() {
    let root = tempfile::Builder::new()
        .prefix("ca-desktop-")
        .tempdir_in("/tmp")
        .unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let result = database_replacement_fixture(root.path()).await;
    result.unwrap();
}

async fn database_replacement_fixture(
    root: &std::path::Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let active = root.join("active");
    std::fs::create_dir(&active)?;
    std::fs::set_permissions(&active, std::fs::Permissions::from_mode(0o700))?;
    let config = resolve_config(&root.join("g"), &root.join("p"))?;
    let (_stop, stopped) = tokio::sync::watch::channel(false);
    let mut first = EngineService::start(&active, "e.sock", &config, &stopped).await?;
    let prepared = match first.owner_runtime() {
        Some(runtime) => revoked_pending_fixture(&active, runtime).await,
        None => Err("actual retained owner runtime required".into()),
    };
    let cleanup = first.shutdown().await;
    cleanup?;
    let mut bridge = prepared?;
    std::fs::rename(&active, root.join("retained-first-database"))?;
    std::fs::create_dir(&active)?;
    std::fs::set_permissions(&active, std::fs::Permissions::from_mode(0o700))?;
    let mut second = EngineService::start(&active, "e.sock", &config, &stopped).await?;
    let reconnect = bridge.connect().await;
    let status = bridge.status();
    let cleanup = second.shutdown().await;
    cleanup?;
    if !matches!(reconnect, Err(error) if error.code == "owner_reconnect_failed") {
        return Err("pending intent must refuse the actual different database".into());
    }
    if !status.pending || status.connected {
        return Err("database refusal must retain pending and clear connection admission".into());
    }
    Ok(())
}

async fn revoked_pending_fixture(
    root: &std::path::Path,
    runtime: &crazyagent::engine::owner::OwnerRuntimeHandle,
) -> Result<NativeBridge, Box<dyn std::error::Error>> {
    use crazyagent::engine::owner::{OwnerControl, OwnerDecision};
    use crazyagent_desktop::bridge::ActionOutcome;

    let (initial, _) = inspect_fixture(root).await?;
    let mut bridge = NativeBridge::new(Some(root.join("e.sock")));
    bridge.connect().await?;
    let detail = bridge.inspect(&initial.id).await?;
    let revoked = runtime
        .control(OwnerControl {
            operation_id: RequestId::new("desktop-database-fence")?,
            target: OwnerClientKind::Desktop,
            expected_epoch: runtime.epoch(OwnerClientKind::Desktop)?,
            decision: OwnerDecision::Revoke,
        })
        .await?;
    if revoked.active {
        return Err("actual Desktop revocation required before pending fixture".into());
    }
    let receipt = bridge.approve(&detail.ticket).await?;
    if !receipt.pending
        || !matches!(
            receipt.outcome,
            ActionOutcome::Rejected | ActionOutcome::Uncertain
        )
    {
        return Err("actual revoked delivery must leave unresolved intent".into());
    }
    Ok(bridge)
}
