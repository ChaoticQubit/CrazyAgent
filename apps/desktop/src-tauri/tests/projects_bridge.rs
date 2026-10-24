use crazyagent::client_controller::OwnerController;
use crazyagent::config::resolve_config;
use crazyagent::engine::bootstrap::EngineService;
use crazyagent::engine::owner::{
    OwnerClientKind, OwnerData, OwnerMutation, OwnerOperation, OwnerRequest, OwnerView,
};
use crazyagent::protocol::{
    Cursor, ExecutionContext, PROTOCOL_VERSION, ProjectSpec, RequestId, StoredProject,
};
use crazyagent_desktop::bridge::NativeBridge;
use serde_json::json;
use std::error::Error;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::Duration;

#[test]
fn project_creation_has_one_named_native_command_and_local_main_window_permission() {
    let build = include_str!("../build.rs");
    let bindings = include_str!("../src/lib.rs");
    let capability: serde_json::Value =
        serde_json::from_str(include_str!("../capabilities/main-window.json")).unwrap();
    assert_eq!(
        build.matches("\"desktop_create_project\"").count(),
        1,
        "the creation operation needs exactly one named build permission"
    );
    assert!(bindings.contains("async fn desktop_create_project("));
    assert_eq!(
        bindings
            .lines()
            .filter(|line| line.trim() == "desktop_create_project,")
            .count(),
        1
    );
    assert_eq!(capability["local"], true);
    assert_eq!(capability["windows"], json!(["main"]));
    assert_eq!(capability["webviews"], json!(["main"]));
    let permissions = capability["permissions"].as_array().unwrap();
    let handlers: Vec<&str> = bindings
        .split("generate_handler![")
        .nth(1)
        .and_then(|rest| rest.split(']').next())
        .unwrap()
        .split(',')
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .collect();
    assert!(handlers.len() >= 34);
    assert_eq!(permissions.len(), handlers.len());
    for handler in &handlers {
        assert_eq!(
            build.matches(&format!("\"{handler}\"")).count(),
            1,
            "{handler} needs one build permission"
        );
        let allowed = format!("allow-{}", handler.replace('_', "-"));
        assert_eq!(
            permissions
                .iter()
                .filter(|permission| **permission == allowed.as_str())
                .count(),
            1,
            "{handler} needs one main-window permission"
        );
    }
    assert_eq!(
        permissions
            .iter()
            .filter(|permission| **permission == "allow-desktop-create-project")
            .count(),
        1
    );
    assert!(capability.get("remote").is_none());
}

#[test]
fn desktop_dependency_accepts_the_exact_closed_project_creation_contract() {
    let value = json!({
        "kind": "CreateProject",
        "project_id": "native-generated-project",
        "spec": {"name": "Declared project", "repositories": ["/not-inspected/repository"]}
    });
    let mutation: OwnerMutation = serde_json::from_value(value.clone())
        .expect("root must integrate the accepted closed core creation variant");
    assert_eq!(serde_json::to_value(&mutation).unwrap(), value);
    let request = OwnerRequest {
        protocol_version: PROTOCOL_VERSION,
        request_id: RequestId::new("native-generated-operation").unwrap(),
        expected_revision: Cursor::new(0),
        operation: OwnerOperation::Apply { mutation },
    };
    assert!(request.validate().is_ok());
    let mut forged = value;
    forged["owner_id"] = json!("browser-cannot-supply-authority");
    assert!(serde_json::from_value::<OwnerMutation>(forged).is_err());
}

#[tokio::test]
async fn unseeded_native_bridge_reads_empty_projects_without_creating_work() {
    let root = tempfile::Builder::new()
        .prefix("ca-project-red-")
        .tempdir_in("/tmp")
        .unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let config = resolve_config(&root.path().join("g"), &root.path().join("p")).unwrap();
    let (_cancel, stopped) = tokio::sync::watch::channel(false);
    let mut service = EngineService::start(root.path(), "e.sock", &config, &stopped)
        .await
        .unwrap();
    let result = tokio::time::timeout(Duration::from_secs(15), empty_bridge(root.path())).await;
    let cleanup = tokio::time::timeout(Duration::from_secs(5), service.shutdown()).await;
    assert!(
        matches!(cleanup, Ok(Ok(()))),
        "actual engine shutdown failed"
    );
    result.unwrap().unwrap();
}

async fn empty_bridge(root: &Path) -> Result<(), Box<dyn Error>> {
    let mut bridge = NativeBridge::new(Some(root.join("e.sock")));
    assert!(!bridge.status().connected);
    assert!(bridge.pending_intent()?.is_none());
    let status = bridge.connect().await?;
    assert!(status.connected);
    assert!(!status.pending);
    let projects = bridge.projects(false).await?;
    assert!(projects.fresh);
    assert!(projects.items.is_empty());
    assert!(!projects.has_more);
    let mut observer =
        OwnerController::attach(root, OwnerClientKind::Tui, Duration::from_secs(2)).await?;
    let tasks = observer.read_page(OwnerView::Tasks, None, Some(1)).await?;
    let OwnerData::Page { items, next } = tasks.data else {
        return Err("actual empty task page required".into());
    };
    assert!(items.is_empty());
    assert!(next.is_none());
    assert!(bridge.pending_intent()?.is_none());
    Ok(())
}

#[tokio::test]
async fn real_creation_first_brief_and_restart_preserve_exact_spec_without_work() {
    let root = private_root();
    let config = resolve_config(&root.path().join("g"), &root.path().join("p")).unwrap();
    let (_cancel, stopped) = tokio::sync::watch::channel(false);
    let mut service = EngineService::start(root.path(), "e.sock", &config, &stopped)
        .await
        .unwrap();
    let result =
        tokio::time::timeout(Duration::from_secs(20), create_first_brief(root.path())).await;
    let cleanup = tokio::time::timeout(Duration::from_secs(5), service.shutdown()).await;
    assert!(matches!(cleanup, Ok(Ok(()))));
    let (id, spec) = result.unwrap().unwrap();
    let mut restarted = EngineService::start(root.path(), "e.sock", &config, &stopped)
        .await
        .unwrap();
    let result = tokio::time::timeout(Duration::from_secs(15), async {
        let mut bridge = connected_bridge(root.path()).await?;
        let page = bridge.projects(false).await?;
        assert_eq!(page.items.len(), 1);
        assert_eq!(page.items[0].id, id);
        assert_eq!(page.items[0].repository_count, 2);
        let selected = bridge.open_project(&id).await?;
        let brief = selected.brief.ok_or("persisted first brief missing")?;
        assert_eq!(brief.version.as_deref(), Some("1"));
        assert_eq!(brief.text, "Actual first brief, with no execution.");
        let (projects, tasks) = actual_snapshot(root.path()).await?;
        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].spec, spec);
        assert!(tasks.is_empty());
        Ok::<_, Box<dyn Error>>(())
    })
    .await;
    let cleanup = tokio::time::timeout(Duration::from_secs(5), restarted.shutdown()).await;
    assert!(matches!(cleanup, Ok(Ok(()))));
    result.unwrap().unwrap();
}

#[tokio::test]
async fn cancelled_creation_replays_exact_native_identity_after_engine_restart() {
    use crazyagent::engine::Client;
    use crazyagent_desktop::bridge::ActionOutcome;
    use std::future::Future;

    let root = private_root();
    let config = resolve_config(&root.path().join("g"), &root.path().join("p")).unwrap();
    let (_cancel, stopped) = tokio::sync::watch::channel(false);
    let mut service = EngineService::start(root.path(), "e.sock", &config, &stopped)
        .await
        .unwrap();
    let mut bridge = connected_bridge(root.path()).await.unwrap();
    bridge.projects(false).await.unwrap();
    let (observer, _) = OwnerController::attach_configured(
        Client::new(root.path().join("e.sock"), Duration::from_secs(2)).unwrap(),
        OwnerClientKind::Tui,
    )
    .await
    .unwrap();
    let (projects, tasks) = snapshot_from(&observer).await.unwrap();
    assert!(projects.is_empty());
    assert!(tasks.is_empty());
    let spec = declaration(root.path(), "Exact original");
    let suspended = {
        let mut request = Box::pin(bridge.create_project(spec.clone(), 41));
        std::future::poll_fn(|context| {
            std::task::Poll::Ready(request.as_mut().poll(context).is_pending())
        })
        .await
    };
    let original = bridge.pending_intent().unwrap().unwrap();
    assert!(
        suspended,
        "real owner I/O must suspend after intent retention"
    );
    assert_eq!(original.generation, "41");
    assert_eq!(original.project_spec.as_ref(), Some(&spec));
    assert_eq!(original.operation_id.len(), 64);
    assert_ne!(
        original.project_id.as_deref(),
        Some(original.operation_id.as_str())
    );
    let replacement = bridge
        .create_project(declaration(root.path(), "Replacement"), 42)
        .await;
    assert_eq!(replacement.unwrap_err().code, "pending_intent");
    assert_eq!(
        serde_json::to_value(bridge.pending_intent().unwrap()).unwrap(),
        serde_json::to_value(Some(&original)).unwrap()
    );
    let committed_without_ack = tokio::time::timeout(Duration::from_secs(15), async {
        let mut request = Box::pin(bridge.retry_intent());
        for _ in 0..8 {
            let suspended = std::future::poll_fn(|context| {
                std::task::Poll::Ready(request.as_mut().poll(context).is_pending())
            })
            .await;
            if !suspended {
                return Err::<bool, Box<dyn Error>>(
                    "real delivery did not retain an unconsumed acknowledgement".into(),
                );
            }
            let (projects, tasks) = snapshot_from(&observer).await?;
            assert!(tasks.is_empty());
            if projects.iter().any(|project| project.spec == spec) {
                assert_eq!(projects.len(), 1);
                assert_eq!(
                    projects[0].project_id.as_str(),
                    original.project_id.as_deref().unwrap()
                );
                return Ok(true);
            }
        }
        Ok(false)
    })
    .await
    .unwrap()
    .unwrap();
    assert!(
        committed_without_ack,
        "the real engine must commit before acknowledgement is dropped"
    );
    assert_eq!(
        serde_json::to_value(bridge.pending_intent().unwrap()).unwrap(),
        serde_json::to_value(Some(&original)).unwrap()
    );
    drop(observer);
    let cleanup = tokio::time::timeout(Duration::from_secs(5), service.shutdown()).await;
    assert!(matches!(cleanup, Ok(Ok(()))));
    let mut restarted = EngineService::start(root.path(), "e.sock", &config, &stopped)
        .await
        .unwrap();
    let result = tokio::time::timeout(Duration::from_secs(20), async {
        bridge.connect().await?;
        assert_eq!(
            serde_json::to_value(bridge.pending_intent()?)?,
            serde_json::to_value(Some(&original))?
        );
        let reconciled = bridge.reconcile_intent().await?;
        assert_eq!(
            serde_json::to_value(&reconciled.intent)?,
            serde_json::to_value(&original)?
        );
        assert!(
            !reconciled.pending,
            "the real committed operation must reconcile after restart"
        );
        let receipt = reconciled;
        assert_eq!(receipt.outcome, ActionOutcome::Applied);
        assert!(!receipt.pending);
        assert_eq!(
            serde_json::to_value(&receipt.intent)?,
            serde_json::to_value(&original)?
        );
        assert!(bridge.pending_intent()?.is_none());
        let (projects, tasks) = actual_snapshot(root.path()).await?;
        assert_eq!(projects.len(), 1);
        assert_eq!(
            projects[0].project_id.as_str(),
            original.project_id.as_deref().unwrap()
        );
        assert_eq!(projects[0].spec, spec);
        assert!(tasks.is_empty());
        assert_eq!(
            bridge.retry_intent().await.unwrap_err().code,
            "no_pending_intent"
        );
        Ok::<_, Box<dyn Error>>(())
    })
    .await;
    let cleanup = tokio::time::timeout(Duration::from_secs(5), restarted.shutdown()).await;
    assert!(matches!(cleanup, Ok(Ok(()))));
    result.unwrap().unwrap();
}

#[tokio::test]
async fn native_creation_bounds_and_revoked_owner_cannot_create_a_project() {
    use crazyagent::engine::owner::{OwnerControl, OwnerDecision};
    use crazyagent_desktop::bridge::ActionOutcome;

    let root = private_root();
    let config = resolve_config(&root.path().join("g"), &root.path().join("p")).unwrap();
    let (_cancel, stopped) = tokio::sync::watch::channel(false);
    let mut service = EngineService::start(root.path(), "e.sock", &config, &stopped)
        .await
        .unwrap();
    let result = tokio::time::timeout(Duration::from_secs(20), async {
        let mut bridge = connected_bridge(root.path()).await?;
        bridge.projects(false).await?;
        for spec in [
            ProjectSpec {
                name: "".into(),
                repositories: vec!["/repo".into()],
            },
            ProjectSpec {
                name: "é".repeat(129),
                repositories: vec!["/repo".into()],
            },
            ProjectSpec {
                name: "control\u{85}".into(),
                repositories: vec!["/repo".into()],
            },
            ProjectSpec {
                name: "Empty".into(),
                repositories: vec![],
            },
            ProjectSpec {
                name: "Relative".into(),
                repositories: vec!["relative".into()],
            },
            ProjectSpec {
                name: "URL".into(),
                repositories: vec!["https://host/repo".into()],
            },
            ProjectSpec {
                name: "Control".into(),
                repositories: vec!["/repo\n".into()],
            },
            ProjectSpec {
                name: "Long".into(),
                repositories: vec![format!("/{}", "x".repeat(4096))],
            },
            ProjectSpec {
                name: "Count".into(),
                repositories: vec!["/repo".into(); 65],
            },
            ProjectSpec {
                name: "Frame".into(),
                repositories: vec![format!("/{}", "x".repeat(4095)); 64],
            },
        ] {
            assert!(bridge.create_project(spec, 1).await.is_err());
            assert!(bridge.pending_intent()?.is_none());
        }
        for generation in [0, 9_007_199_254_740_992] {
            assert_eq!(
                bridge
                    .create_project(declaration(root.path(), "Generation"), generation)
                    .await
                    .unwrap_err()
                    .code,
                "draft_generation_limit"
            );
            assert!(bridge.pending_intent()?.is_none());
        }
        assert!(actual_snapshot(root.path()).await?.0.is_empty());
        let host = service
            .owner_runtime()
            .ok_or("actual owner runtime missing")?;
        let revoked = host
            .control(OwnerControl {
                operation_id: RequestId::new("project-revoke")?,
                target: OwnerClientKind::Desktop,
                expected_epoch: host.epoch(OwnerClientKind::Desktop)?,
                decision: OwnerDecision::Revoke,
            })
            .await?;
        assert!(!revoked.active);
        let spec = declaration(root.path(), "Revoked");
        let denied = bridge.create_project(spec.clone(), 10).await?;
        assert_eq!(denied.outcome, ActionOutcome::Uncertain);
        assert_eq!(denied.rejection, None);
        assert!(actual_snapshot(root.path()).await?.0.is_empty());
        assert!(actual_snapshot(root.path()).await?.1.is_empty());
        assert!(denied.pending);
        assert!(!bridge.status().connected);
        assert!(bridge.status().pending);
        assert_eq!(denied.intent.kind, "create_project");
        assert_eq!(denied.intent.generation, "10");
        assert_eq!(denied.intent.project_spec.as_ref(), Some(&spec));
        assert_eq!(denied.intent.operation_id.len(), 64);
        let project_id = denied
            .intent
            .project_id
            .as_deref()
            .ok_or("native project identity missing")?;
        assert_eq!(project_id.len(), 72);
        assert_ne!(project_id, denied.intent.operation_id.as_str());
        assert_eq!(
            serde_json::to_value(bridge.pending_intent()?)?,
            serde_json::to_value(Some(&denied.intent))?
        );
        for repository in &spec.repositories {
            assert!(!Path::new(repository).exists());
        }
        assert!(!root.path().join("g").exists());
        assert!(!root.path().join("p").exists());
        assert_eq!(
            bridge
                .pending_intent()?
                .unwrap()
                .project_spec
                .as_ref()
                .unwrap()
                .name,
            "Revoked"
        );
        Ok::<_, Box<dyn Error>>(())
    })
    .await;
    let cleanup = tokio::time::timeout(Duration::from_secs(5), service.shutdown()).await;
    assert!(matches!(cleanup, Ok(Ok(()))));
    result.unwrap().unwrap();
}

#[tokio::test]
async fn receipt_identity_opens_creation_outside_first_project_page() {
    let root = private_root();
    let config = resolve_config(&root.path().join("g"), &root.path().join("p")).unwrap();
    let (_cancel, stopped) = tokio::sync::watch::channel(false);
    let mut service = EngineService::start(root.path(), "e.sock", &config, &stopped)
        .await
        .unwrap();
    let result = tokio::time::timeout(Duration::from_secs(30), async {
        let mut bridge = connected_bridge(root.path()).await?;
        let mut receipts = Vec::new();
        for generation in 1..=17 {
            bridge.projects(false).await?;
            let receipt = bridge
                .create_project(
                    declaration(root.path(), &format!("Project {generation}")),
                    generation,
                )
                .await?;
            assert_eq!(
                receipt.outcome,
                crazyagent_desktop::bridge::ActionOutcome::Applied
            );
            receipts.push(receipt);
        }
        let first = bridge.projects(false).await?;
        assert_eq!(first.items.len(), 16);
        assert!(first.has_more);
        let receipt = receipts
            .iter()
            .find(|receipt| {
                !first
                    .items
                    .iter()
                    .any(|row| Some(row.id.as_str()) == receipt.intent.project_id.as_deref())
            })
            .ok_or("one real receipt must fall outside the first page")?;
        let id = receipt.intent.project_id.as_deref().unwrap();
        let selected = bridge.open_project(id).await?;
        assert_eq!(selected.project_id, id);
        assert!(selected.brief.is_none());
        let (projects, tasks) = actual_snapshot(root.path()).await?;
        assert_eq!(projects.len(), 17);
        assert!(tasks.is_empty());
        Ok::<_, Box<dyn Error>>(())
    })
    .await;
    let cleanup = tokio::time::timeout(Duration::from_secs(5), service.shutdown()).await;
    assert!(matches!(cleanup, Ok(Ok(()))));
    result.unwrap().unwrap();
}

#[tokio::test]
async fn native_creation_accepts_utf8_and_repository_count_boundaries_without_inspection() {
    let root = private_root();
    let config = resolve_config(&root.path().join("g"), &root.path().join("p")).unwrap();
    let (_cancel, stopped) = tokio::sync::watch::channel(false);
    let mut service = EngineService::start(root.path(), "e.sock", &config, &stopped)
        .await
        .unwrap();
    let result = tokio::time::timeout(Duration::from_secs(20), async {
        let mut bridge = connected_bridge(root.path()).await?;
        let utf8 = ProjectSpec {
            name: "é".repeat(128),
            repositories: vec![format!("/{}a", "é".repeat(2047))],
        };
        let count = ProjectSpec {
            name: "Sixty-four declarations".into(),
            repositories: vec!["/not-inspected/repository".into(); 64],
        };
        for (generation, spec) in [(1, utf8.clone()), (2, count.clone())] {
            bridge.projects(false).await?;
            let receipt = bridge.create_project(spec.clone(), generation).await?;
            assert_eq!(
                receipt.outcome,
                crazyagent_desktop::bridge::ActionOutcome::Applied
            );
            assert_eq!(receipt.intent.project_spec.as_ref(), Some(&spec));
            assert!(!receipt.pending);
        }
        let (projects, tasks) = actual_snapshot(root.path()).await?;
        assert_eq!(projects.len(), 2);
        assert!(projects.iter().any(|project| project.spec == utf8));
        assert!(projects.iter().any(|project| project.spec == count));
        assert!(tasks.is_empty());
        let page = bridge.projects(false).await?;
        assert!(page.items.iter().any(|row| row.repository_count == 64));
        Ok::<_, Box<dyn Error>>(())
    })
    .await;
    let cleanup = tokio::time::timeout(Duration::from_secs(5), service.shutdown()).await;
    assert!(matches!(cleanup, Ok(Ok(()))));
    result.unwrap().unwrap();
}

fn private_root() -> tempfile::TempDir {
    let root = tempfile::Builder::new()
        .prefix("ca-project-")
        .tempdir_in("/tmp")
        .unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    root
}

fn declaration(root: &Path, name: &str) -> ProjectSpec {
    ProjectSpec {
        name: name.into(),
        repositories: vec![
            root.join("absent-one").display().to_string(),
            root.join("absent-two").display().to_string(),
        ],
    }
}

async fn connected_bridge(root: &Path) -> Result<NativeBridge, Box<dyn Error>> {
    let mut bridge = NativeBridge::new(Some(root.join("e.sock")));
    assert!(bridge.connect().await?.connected);
    Ok(bridge)
}

async fn actual_snapshot(
    root: &Path,
) -> Result<(Vec<StoredProject>, Vec<ExecutionContext>), Box<dyn Error>> {
    use crazyagent::engine::Client;

    let (ordinary, _observer) = OwnerController::attach_configured(
        Client::new(root.join("e.sock"), Duration::from_secs(2))?,
        OwnerClientKind::Tui,
    )
    .await?;
    snapshot_from(&ordinary).await
}

async fn snapshot_from(
    ordinary: &crazyagent::engine::Client,
) -> Result<(Vec<StoredProject>, Vec<ExecutionContext>), Box<dyn Error>> {
    use crazyagent::protocol::{ClientId, Command, Outcome, WireRequest};

    let reply = ordinary
        .request(WireRequest {
            protocol_version: PROTOCOL_VERSION,
            client_id: ClientId::new("project-creation-readback")?,
            request_id: RequestId::new("project-creation-snapshot")?,
            expected_revision: Cursor::new(0),
            command: Command::Snapshot,
        })
        .await?;
    let Outcome::Snapshot { projects, tasks } = reply.outcome else {
        return Err("real stored snapshot required".into());
    };
    Ok((projects, tasks))
}

async fn create_first_brief(root: &Path) -> Result<(String, ProjectSpec), Box<dyn Error>> {
    use crazyagent::engine::owner::OwnerRead;
    use crazyagent::protocol::ProjectId;
    use crazyagent_desktop::bridge::ActionOutcome;

    let mut bridge = connected_bridge(root).await?;
    assert!(bridge.projects(false).await?.items.is_empty());
    let spec = declaration(root, " Exact declaration ");
    let receipt = bridge.create_project(spec.clone(), 1).await?;
    assert_eq!(receipt.outcome, ActionOutcome::Applied);
    assert!(!receipt.pending);
    assert_eq!(receipt.intent.kind, "create_project");
    assert_eq!(receipt.intent.project_spec.as_ref(), Some(&spec));
    let id = receipt
        .intent
        .project_id
        .ok_or("native project identity missing")?;
    assert!(!root.join("absent-one").exists());
    assert!(!root.join("absent-two").exists());
    assert!(!root.join("g").exists());
    assert!(!root.join("p").exists());
    let selected = bridge.open_project(&id).await?;
    assert!(selected.brief.is_none());
    assert!(bridge.history(false).await?.entries.is_empty());
    let brief = bridge
        .replace_brief(
            &selected.ticket,
            "Actual first brief, with no execution.",
            2,
        )
        .await?;
    assert_eq!(brief.outcome, ActionOutcome::Applied);
    assert!(!brief.pending);
    let mut observer =
        OwnerController::attach(root, OwnerClientKind::Tui, Duration::from_secs(2)).await?;
    let current = observer
        .read(OwnerRead::CurrentBrief {
            project_id: ProjectId::new(&id)?,
        })
        .await?;
    let OwnerData::CurrentBrief {
        brief: Some(brief), ..
    } = current.data
    else {
        return Err("real other-client first brief required".into());
    };
    assert_eq!(brief.text, "Actual first brief, with no execution.");
    let baselines = observer
        .read_page(OwnerView::Baselines, None, Some(1))
        .await?;
    let OwnerData::Page { items, .. } = baselines.data else {
        return Err("real baseline page required".into());
    };
    assert!(items.is_empty());
    let (projects, tasks) = actual_snapshot(root).await?;
    assert_eq!(projects.len(), 1);
    assert_eq!(projects[0].spec, spec);
    assert!(tasks.is_empty());
    Ok((id, spec))
}
