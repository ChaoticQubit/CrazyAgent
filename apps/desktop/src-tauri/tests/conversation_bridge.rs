#![cfg(unix)]

use crazyagent::client_controller::OwnerController;
use crazyagent::config::resolve_config;
use crazyagent::conversation::{ConversationContent, ConversationMutation};
use crazyagent::engine::Client;
use crazyagent::engine::bootstrap::EngineService;
use crazyagent::engine::owner::{
    OwnerClientKind, OwnerData, OwnerMutation, OwnerRead, OwnerResponse, OwnerResult,
};
use crazyagent::protocol::*;
use crazyagent_desktop::bridge::{ActionOutcome, NativeBridge};
use std::error::Error;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::Duration;

type TestResult<T> = Result<T, Box<dyn Error>>;

#[tokio::test]
async fn cancelled_message_keeps_exact_pending_across_projects_and_legacy_recovery() {
    let root = tempfile::Builder::new()
        .prefix("ca-dpend-")
        .tempdir_in("/tmp")
        .unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let config = resolve_config(&root.path().join("g"), &root.path().join("p")).unwrap();
    let (_cancel, stopped) = tokio::sync::watch::channel(false);
    let mut service = EngineService::start(root.path(), "e.sock", &config, &stopped)
        .await
        .unwrap();
    let result = tokio::time::timeout(Duration::from_secs(20), pending_case(root.path())).await;
    let cleanup = tokio::time::timeout(Duration::from_secs(5), service.shutdown()).await;
    assert!(
        matches!(cleanup, Ok(Ok(()))),
        "actual engine cleanup failed"
    );
    let (count, beta, generation) = result.unwrap().unwrap();
    assert_eq!(count, 1);
    assert_eq!(beta, 0);
    assert_eq!(generation, "41");
}

async fn pending_case(root: &Path) -> TestResult<(usize, usize, String)> {
    let (mut bridge, mut observer) = pair(root).await?;
    let selected = bridge.open_project("alpha").await?;
    let cancelled = {
        use std::future::Future;
        let mut request =
            Box::pin(bridge.send_message(&selected.ticket, "exact cancelled alpha message", 41));
        std::future::poll_fn(|context| {
            std::task::Poll::Ready(request.as_mut().poll(context).is_pending())
        })
        .await
    };
    if !cancelled {
        return Err("fixture did not suspend actual pending I/O".into());
    }
    let original = serde_json::to_string(&bridge.pending_intent()?)?;
    let beta = bridge.open_project("beta").await?;
    if bridge
        .send_message(&beta.ticket, "must not replace alpha intent", 42)
        .await
        .err()
        .ok_or("conflicting intent was admitted")?
        .code
        != "pending_intent"
    {
        return Err("conflicting intent was not blocked".into());
    }
    if bridge
        .reconcile()
        .await
        .err()
        .ok_or("legacy recovery dispatched another target kind")?
        .code
        != "unexpected_pending_intent"
    {
        return Err("legacy recovery dispatched another target kind".into());
    }
    if serde_json::to_string(&bridge.pending_intent()?)? != original {
        return Err("original target/generation changed".into());
    }
    let reconciled = bridge.reconcile_intent().await?;
    let settled = if reconciled.pending {
        bridge.retry_intent().await?
    } else {
        reconciled
    };
    if settled.outcome != ActionOutcome::Applied
        || settled.pending
        || settled.intent.kind != "message"
        || settled.intent.project_id.as_deref() != Some("alpha")
    {
        return Err("actual original receipt required".into());
    }
    let read = observer
        .read(OwnerRead::ConversationHistory {
            project_id: ProjectId::new("alpha")?,
            cursor: None,
            limit: 32,
        })
        .await?;
    let OwnerData::ConversationHistory { page } = read.data else {
        return Err("actual alpha history required".into());
    };
    if page
        .entries
        .iter()
        .any(|entry| entry.text != "exact cancelled alpha message")
    {
        return Err("different payload was recorded".into());
    }
    let read = observer
        .read(OwnerRead::ConversationHistory {
            project_id: ProjectId::new("beta")?,
            cursor: None,
            limit: 32,
        })
        .await?;
    let OwnerData::ConversationHistory { page: beta_page } = read.data else {
        return Err("actual beta history required".into());
    };
    Ok((
        page.entries.len(),
        beta_page.entries.len(),
        settled.intent.generation,
    ))
}

#[tokio::test]
async fn full_text_pagination_invalidates_old_cursor_after_actual_revision_change() {
    let root = tempfile::Builder::new()
        .prefix("ca-dpage-")
        .tempdir_in("/tmp")
        .unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let config = resolve_config(&root.path().join("g"), &root.path().join("p")).unwrap();
    let (_cancel, stopped) = tokio::sync::watch::channel(false);
    let mut service = EngineService::start(root.path(), "e.sock", &config, &stopped)
        .await
        .unwrap();
    let result = tokio::time::timeout(Duration::from_secs(20), pagination_case(root.path())).await;
    let cleanup = tokio::time::timeout(Duration::from_secs(5), service.shutdown()).await;
    assert!(
        matches!(cleanup, Ok(Ok(()))),
        "actual engine cleanup failed"
    );
    let (full, second, stale) = result.unwrap().unwrap();
    assert_eq!(full, "\u{1}".repeat(8192));
    assert_eq!(second, "second entry");
    assert_eq!(stale, "resync_required");
}

async fn pagination_case(root: &Path) -> TestResult<(String, String, &'static str)> {
    let (mut bridge, mut observer) = pair(root).await?;
    let selected = bridge.open_project("alpha").await?;
    let first = bridge
        .send_message(&selected.ticket, &"\u{1}".repeat(8192), 1)
        .await?;
    if first.outcome != ActionOutcome::Applied {
        return Err("actual first message required".into());
    }
    let selected = bridge.open_project("alpha").await?;
    bridge
        .send_message(&selected.ticket, "second entry", 2)
        .await?;
    bridge.open_project("alpha").await?;
    let first = bridge.history(false).await?;
    if !first.has_more || first.entries.len() != 1 || serde_json::to_vec(&first)?.len() > 65536 {
        return Err("one bounded complete entry required".into());
    }
    let second = bridge.history(true).await?;
    if second.has_more || second.entries.len() != 1 {
        return Err("actual terminal history page required".into());
    }
    bridge.history(false).await?;
    observer
        .read(OwnerRead::CurrentBrief {
            project_id: ProjectId::new("alpha")?,
        })
        .await?;
    observer.begin_mutation(
        OwnerMutation::Conversation {
            mutation: ConversationMutation::AppendMessage {
                project_id: ProjectId::new("alpha")?,
                text: "independent new revision".into(),
            },
        },
        9,
    )?;
    observer.send_pending().await?;
    let stale = bridge
        .history(true)
        .await
        .err()
        .ok_or("stale cursor was accepted")?
        .code;
    Ok((
        first.entries[0].text.clone(),
        second.entries[0].text.clone(),
        stale,
    ))
}

#[tokio::test]
async fn actual_desktop_revocation_denies_old_bridge_and_reactivation_requires_new_inspection() {
    use crazyagent::engine::owner::{OwnerControl, OwnerDecision};
    let root = tempfile::Builder::new()
        .prefix("ca-drevoke-")
        .tempdir_in("/tmp")
        .unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let config = resolve_config(&root.path().join("g"), &root.path().join("p")).unwrap();
    let (_cancel, stopped) = tokio::sync::watch::channel(false);
    let mut service = EngineService::start(root.path(), "e.sock", &config, &stopped)
        .await
        .unwrap();
    let result: TestResult<_> = async {
        let (mut bridge, mut observer) = pair(root.path()).await?;
        let selected = bridge.open_project("alpha").await?;
        let host = service.owner_runtime().ok_or("actual runtime missing")?;
        let epoch = host.epoch(OwnerClientKind::Desktop)?;
        let revoked = host
            .control(OwnerControl {
                operation_id: RequestId::new("native-desktop-revoke")?,
                target: OwnerClientKind::Desktop,
                expected_epoch: epoch,
                decision: OwnerDecision::Revoke,
            })
            .await?;
        if revoked.active {
            return Err("actual desktop still active".into());
        }
        let denied = bridge.history(false).await.is_err();
        observer
            .read(OwnerRead::CurrentBrief {
                project_id: ProjectId::new("alpha")?,
            })
            .await?;
        let reactivated = host
            .control(OwnerControl {
                operation_id: RequestId::new("native-desktop-reactivate")?,
                target: OwnerClientKind::Desktop,
                expected_epoch: revoked.owner_epoch,
                decision: OwnerDecision::Reactivate,
            })
            .await?;
        bridge.connect().await?;
        let stale = bridge
            .send_message(&selected.ticket, "old selection must be denied", 1)
            .await
            .err()
            .ok_or("old selection was admitted")?
            .code;
        let fresh = bridge.open_project("alpha").await?;
        let receipt = bridge
            .send_message(&fresh.ticket, "fresh reactivated message", 2)
            .await?;
        Ok((
            denied,
            reactivated.active,
            reactivated.owner_epoch > revoked.owner_epoch,
            stale,
            receipt.outcome,
        ))
    }
    .await;
    let cleanup = tokio::time::timeout(Duration::from_secs(5), service.shutdown()).await;
    assert!(
        matches!(cleanup, Ok(Ok(()))),
        "actual engine cleanup failed"
    );
    let observed = result.unwrap();
    assert_eq!(
        observed,
        (true, true, true, "stale_selection", ActionOutcome::Applied)
    );
}

#[tokio::test]
async fn desktop_message_is_durable_without_implicitly_replacing_the_brief() {
    let root = tempfile::Builder::new()
        .prefix("ca-dconv-")
        .tempdir_in("/tmp")
        .unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let config = resolve_config(&root.path().join("g"), &root.path().join("p")).unwrap();
    let (_cancel, stopped) = tokio::sync::watch::channel(false);
    let mut service = EngineService::start(root.path(), "e.sock", &config, &stopped)
        .await
        .unwrap();
    let result = tokio::time::timeout(Duration::from_secs(15), message_case(root.path())).await;
    let cleanup = tokio::time::timeout(Duration::from_secs(5), service.shutdown()).await;
    assert!(
        matches!(cleanup, Ok(Ok(()))),
        "actual engine cleanup failed"
    );
    let (projects, history, current_brief, projected) = result.unwrap().unwrap();
    assert_eq!(projects, vec!["alpha", "beta"]);
    assert_eq!(
        history,
        "<script>literal owner message</script>\nsecond line"
    );
    assert!(current_brief.is_none());
    for forbidden in [
        "owner_id",
        "db_instance",
        "engine_epoch",
        "request_hash",
        "owner.sock",
    ] {
        assert!(!projected.contains(forbidden));
    }
}

async fn pair(root: &Path) -> TestResult<(NativeBridge, OwnerController)> {
    let socket = root.join("e.sock");
    let (ordinary, mut observer) = OwnerController::attach_configured(
        Client::new(socket.clone(), Duration::from_secs(2))?,
        OwnerClientKind::Tui,
    )
    .await?;
    for name in ["alpha", "beta"] {
        let revision = observer.read(OwnerRead::Capabilities).await?.revision;
        let reply = ordinary
            .request(WireRequest {
                protocol_version: PROTOCOL_VERSION,
                client_id: ClientId::new("desktop-conversation-fixture")?,
                request_id: RequestId::new(name)?,
                expected_revision: revision,
                command: Command::CreateProject {
                    project_id: ProjectId::new(name)?,
                    spec: ProjectSpec {
                        name: name.into(),
                        repositories: vec![],
                    },
                },
            })
            .await?;
        if reply.outcome != Outcome::Applied {
            return Err("actual project creation required".into());
        }
    }
    let mut bridge = NativeBridge::new(Some(socket));
    bridge.connect().await?;
    Ok((bridge, observer))
}

async fn message_case(root: &Path) -> TestResult<(Vec<String>, String, Option<String>, String)> {
    let (mut bridge, mut observer) = pair(root).await?;
    let projects = bridge.projects(false).await?;
    let names = projects.items.iter().map(|row| row.id.clone()).collect();
    let selected = bridge.open_project("alpha").await?;
    if selected.brief.is_some() {
        return Err("actual initial brief must be absent".into());
    }
    let text = "<script>literal owner message</script>\nsecond line";
    let receipt = bridge.send_message(&selected.ticket, text, 1).await?;
    if receipt.outcome != ActionOutcome::Applied
        || receipt.pending
        || receipt.intent.project_id.as_deref() != Some("alpha")
    {
        return Err("actual recorded alpha message receipt required".into());
    }
    let state = observer
        .read(OwnerRead::ConversationHistory {
            project_id: ProjectId::new("alpha")?,
            cursor: None,
            limit: 1,
        })
        .await?;
    let OwnerData::ConversationHistory { page } = state.data else {
        return Err("actual history required".into());
    };
    if page.entries.len() != 1 || page.entries[0].content != ConversationContent::Message {
        return Err("one actual owner message required".into());
    }
    let brief = observer
        .read(OwnerRead::CurrentBrief {
            project_id: ProjectId::new("alpha")?,
        })
        .await?;
    let OwnerData::CurrentBrief { brief, .. } = brief.data else {
        return Err("actual current brief required".into());
    };
    bridge.open_project("alpha").await?;
    let displayed = bridge.history(false).await?;
    if displayed.entries.len() != 1 || displayed.entries[0].text != text || !displayed.fresh {
        return Err("complete actual desktop history required".into());
    }
    let projected = serde_json::to_string(&displayed)?;
    Ok((
        names,
        page.entries[0].text.clone(),
        brief.map(|value| value.text),
        projected,
    ))
}

#[tokio::test]
async fn desktop_brief_replacement_is_explicit_and_rejects_the_inspected_old_version() {
    let root = tempfile::Builder::new()
        .prefix("ca-dbrief-")
        .tempdir_in("/tmp")
        .unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let config = resolve_config(&root.path().join("g"), &root.path().join("p")).unwrap();
    let (_cancel, stopped) = tokio::sync::watch::channel(false);
    let mut service = EngineService::start(root.path(), "e.sock", &config, &stopped)
        .await
        .unwrap();
    let result = tokio::time::timeout(Duration::from_secs(15), conflict_case(root.path())).await;
    let cleanup = tokio::time::timeout(Duration::from_secs(5), service.shutdown()).await;
    assert!(
        matches!(cleanup, Ok(Ok(()))),
        "actual engine cleanup failed"
    );
    let (rejection, retained, final_text) = result.unwrap().unwrap();
    assert_eq!(rejection, Some("stale_brief_version".into()));
    assert_eq!(retained, "concurrent actual brief");
    assert_eq!(final_text, "explicit fresh replacement");
}

async fn conflict_case(root: &Path) -> TestResult<(Option<String>, String, String)> {
    let (mut bridge, mut observer) = pair(root).await?;
    let selected = bridge.open_project("alpha").await?;
    observer
        .read(OwnerRead::CurrentBrief {
            project_id: ProjectId::new("alpha")?,
        })
        .await?;
    observer.begin_mutation(
        OwnerMutation::Conversation {
            mutation: ConversationMutation::ReplaceBrief {
                project_id: ProjectId::new("alpha")?,
                expected_version: 0,
                text: "concurrent actual brief".into(),
            },
        },
        1,
    )?;
    let response = observer.send_pending().await?;
    let OwnerResponse::Applied { operation } = response else {
        return Err("actual concurrent brief receipt required".into());
    };
    if operation.result != OwnerResult::Applied {
        return Err("actual concurrent brief effect required".into());
    }
    bridge.history(false).await?;
    let rejected = bridge
        .replace_brief(
            &selected.ticket,
            "must not replace the concurrent version",
            2,
        )
        .await?;
    if rejected.outcome != ActionOutcome::Rejected || rejected.pending {
        return Err("actual recorded version rejection required".into());
    }
    let current = bridge.open_project("alpha").await?;
    let retained = current
        .brief
        .as_ref()
        .ok_or("actual retained brief required")?
        .text
        .clone();
    let applied = bridge
        .replace_brief(&current.ticket, "explicit fresh replacement", 3)
        .await?;
    if applied.outcome != ActionOutcome::Applied || applied.pending {
        return Err("explicit fresh replacement must be recorded".into());
    }
    let final_read = observer
        .read(OwnerRead::CurrentBrief {
            project_id: ProjectId::new("alpha")?,
        })
        .await?;
    let OwnerData::CurrentBrief {
        brief: Some(final_brief),
        ..
    } = final_read.data
    else {
        return Err("actual final brief required".into());
    };
    Ok((
        rejected.rejection.map(str::to_owned),
        retained,
        final_brief.text,
    ))
}
