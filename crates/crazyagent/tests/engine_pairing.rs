#![cfg(unix)]

use crazyagent::client_controller::OwnerController;
use crazyagent::config::resolve_config;
use crazyagent::engine::bootstrap::EngineService;
use crazyagent::engine::owner::{
    OwnerClient, OwnerClientKind, OwnerData, OwnerMutation, OwnerSummary, OwnerView,
};
use crazyagent::engine::{Client, Engine, EngineError, Server};
use crazyagent::protocol::*;
use std::future::Future;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::Poll;
use std::time::Duration;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

struct Running {
    stop: Option<oneshot::Sender<()>>,
    task: Option<JoinHandle<Result<(), EngineError>>>,
    closed: Arc<AtomicBool>,
}

impl Running {
    async fn service(root: Arc<tempfile::TempDir>, anchor: &Path) -> Self {
        let config = resolve_config(&anchor.join("g"), &anchor.join("p")).unwrap();
        let (_stop, stopped) = tokio::sync::watch::channel(false);
        let mut service = EngineService::start(anchor, "e.sock", &config, &stopped)
            .await
            .unwrap();
        let (stop, done) = oneshot::channel();
        let closed = Arc::new(AtomicBool::new(false));
        let observed = closed.clone();
        let task = tokio::spawn(async move {
            let _ = done.await;
            let result = service.shutdown().await;
            observed.store(result.is_ok(), Ordering::SeqCst);
            drop(root);
            result
        });
        Self {
            stop: Some(stop),
            task: Some(task),
            closed,
        }
    }

    async fn ordinary(root: Arc<tempfile::TempDir>, socket: &Path) -> Self {
        let config = resolve_config(&root.path().join("g"), &root.path().join("p")).unwrap();
        let engine = Arc::new(
            Engine::open(&root.path().join("other.db"), config.clone())
                .await
                .unwrap(),
        );
        let server = Server::bind(socket, engine.clone(), &config).unwrap();
        let (stop, done) = oneshot::channel();
        let closed = Arc::new(AtomicBool::new(false));
        let observed = closed.clone();
        let task = tokio::spawn(async move {
            let served = server.run(done).await;
            let storage = engine.close().await;
            let result = served.and(storage);
            observed.store(result.is_ok(), Ordering::SeqCst);
            drop(root);
            result
        });
        Self {
            stop: Some(stop),
            task: Some(task),
            closed,
        }
    }

    async fn finish(mut self) {
        self.stop.take().unwrap().send(()).unwrap();
        let mut task = self.task.take().unwrap();
        match tokio::time::timeout(Duration::from_secs(5), &mut task).await {
            Ok(result) => result.unwrap().unwrap(),
            Err(_) => {
                task.abort();
                let _ = tokio::time::timeout(Duration::from_secs(1), task).await;
                panic!("owned native cleanup exceeded five seconds");
            }
        }
        assert!(self.closed.load(Ordering::SeqCst));
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
    }
}

fn root() -> Arc<tempfile::TempDir> {
    let root = tempfile::Builder::new()
        .prefix("ca-pair-")
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    Arc::new(root)
}

fn client(socket: PathBuf) -> Client {
    Client::new(socket, Duration::from_secs(2)).unwrap()
}

fn request(id: &str, revision: Cursor, command: Command) -> WireRequest {
    WireRequest {
        protocol_version: PROTOCOL_VERSION,
        client_id: ClientId::new("native-pairing").unwrap(),
        request_id: RequestId::new(id).unwrap(),
        expected_revision: revision,
        command,
    }
}

fn create(id: &str, revision: Cursor) -> WireRequest {
    request(
        id,
        revision,
        Command::CreateProject {
            project_id: ProjectId::new(id).unwrap(),
            spec: ProjectSpec {
                name: id.into(),
                repositories: vec![],
            },
        },
    )
}

async fn projects(controller: &mut OwnerController) -> Result<Vec<String>, EngineError> {
    let reply = controller
        .read_page(OwnerView::Projects, None, None)
        .await?;
    match reply.data {
        OwnerData::Page { items, .. } => Ok(items
            .into_iter()
            .map(|item| match item {
                OwnerSummary::Project { project_id, .. } => project_id.as_str().to_owned(),
                _ => panic!("project read returned another view"),
            })
            .collect()),
        _ => Err(EngineError("expected actual project page".into())),
    }
}

#[tokio::test]
async fn configured_controller_pairs_both_kinds_and_preserves_legacy_paths() {
    let root = root();
    let running = Running::service(root.clone(), root.path()).await;
    let observed: Result<_, EngineError> = async {
        let mut identities = Vec::new();
        let mut effects = Vec::new();
        for (kind, id) in [
            (OwnerClientKind::Tui, "paired-tui"),
            (OwnerClientKind::Desktop, "paired-desktop"),
        ] {
            let (ordinary, mut controller) =
                OwnerController::attach_configured(client(root.path().join("e.sock")), kind)
                    .await?;
            let (_, direct) =
                OwnerClient::discover(root.path(), kind, Duration::from_secs(2)).await?;
            let snapshot = ordinary
                .request(request(
                    &format!("snapshot-{id}"),
                    Cursor::new(0),
                    Command::Snapshot,
                ))
                .await?;
            let effect = ordinary.request(create(id, snapshot.revision)).await?;
            let names = projects(&mut controller).await?;
            identities.push(controller.identity().clone() == direct);
            effects.push(effect.outcome == Outcome::Applied && names.contains(&id.into()));
        }
        let legacy = client(root.path().join("e.sock"))
            .request(request("legacy", Cursor::new(0), Command::Snapshot))
            .await?;
        Ok((identities, effects, legacy.outcome))
    }
    .await;
    running.finish().await;
    let (identities, effects, legacy) = observed.unwrap();
    assert_eq!(identities, vec![true, true]);
    assert_eq!(effects, vec![true, true]);
    assert!(matches!(legacy, Outcome::Snapshot { .. }));
    assert!(!root.path().join("e.sock").exists());
    assert!(!root.path().join("owner-current").exists());
}

#[tokio::test]
async fn configured_admission_rejects_another_real_socket_in_the_same_parent() {
    let root = root();
    let other = crate::root();
    let running = Running::service(root.clone(), root.path()).await;
    let extra = Running::ordinary(other, &root.path().join("other.sock")).await;
    let ordinary = client(root.path().join("other.sock"));
    let real = ordinary
        .request(request("other-live", Cursor::new(0), Command::Snapshot))
        .await;
    let raw = client(root.path().join("other.sock"))
        .with_owner(OwnerClientKind::Desktop)
        .await;
    let controlled = OwnerController::attach_configured(ordinary, OwnerClientKind::Desktop).await;
    extra.finish().await;
    running.finish().await;
    assert!(matches!(real.unwrap().outcome, Outcome::Snapshot { .. }));
    assert_eq!(raw.err().unwrap().0, "owner_ordinary_socket_mismatch");
    assert_eq!(
        controlled.err().unwrap().0,
        "owner_ordinary_socket_mismatch"
    );
}

#[tokio::test]
async fn retained_socket_refuses_a_real_replacement_without_wrong_engine_effect() {
    let root = root();
    let other = crate::root();
    let running = Running::service(root.clone(), root.path()).await;
    let socket = root.path().join("e.sock");
    let held = root.path().join("held.sock");
    let (ordinary, mut controller) =
        OwnerController::attach_configured(client(socket.clone()), OwnerClientKind::Desktop)
            .await
            .unwrap();
    std::fs::rename(&socket, &held).unwrap();
    let extra = Running::ordinary(other, &socket).await;
    let denied = ordinary
        .request(create("wrong-engine", Cursor::new(0)))
        .await;
    let wrong_state = client(socket.clone())
        .request(request("wrong-state", Cursor::new(0), Command::Snapshot))
        .await;
    extra.finish().await;
    std::fs::rename(&held, &socket).unwrap();
    let correct_state = projects(&mut controller).await;
    running.finish().await;
    assert_eq!(denied.err().unwrap().0, "owner_endpoint_replaced_or_unsafe");
    assert!(matches!(
        wrong_state.unwrap().outcome,
        Outcome::Snapshot { projects, .. } if projects.is_empty()
    ));
    assert!(correct_state.unwrap().is_empty());
}

#[tokio::test]
async fn retained_locator_rejects_atomic_replacement_and_allows_explicit_rebinding() {
    let root = root();
    let running = Running::service(root.clone(), root.path()).await;
    let locator = root.path().join("owner-current");
    let held = root.path().join("held-locator");
    let bytes = std::fs::read(&locator).unwrap();
    let (ordinary, mut controller) = OwnerController::attach_configured(
        client(root.path().join("e.sock")),
        OwnerClientKind::Tui,
    )
    .await
    .unwrap();
    std::fs::rename(&locator, &held).unwrap();
    std::fs::write(&locator, &bytes).unwrap();
    std::fs::set_permissions(&locator, std::fs::Permissions::from_mode(0o600)).unwrap();
    let denied = ordinary.request(create("blocked", Cursor::new(0))).await;
    let rebound = controller
        .reattach_configured(client(root.path().join("e.sock")))
        .await;
    let effect = match rebound {
        Ok(bound) => bound.request(create("rebound", Cursor::new(0))).await,
        Err(error) => Err(error),
    };
    let state = projects(&mut controller).await;
    std::fs::remove_file(&locator).unwrap();
    std::fs::rename(&held, &locator).unwrap();
    running.finish().await;
    assert_eq!(denied.err().unwrap().0, "owner_locator_replaced_or_unsafe");
    assert_eq!(effect.unwrap().outcome, Outcome::Applied);
    assert_eq!(state.unwrap(), vec!["rebound"]);
}

#[tokio::test]
async fn retained_locator_bytes_are_checked_after_connect_before_request_write() {
    let root = root();
    let running = Running::service(root.clone(), root.path()).await;
    let locator = root.path().join("owner-current");
    let bytes = std::fs::read(&locator).unwrap();
    let original = std::fs::metadata(&locator).unwrap();
    let (ordinary, mut controller) = OwnerController::attach_configured(
        client(root.path().join("e.sock")),
        OwnerClientKind::Tui,
    )
    .await
    .unwrap();
    let epoch = controller.identity().engine_epoch().as_bytes();
    let position = bytes.windows(epoch.len()).position(|w| w == epoch).unwrap();
    let mut edited = bytes.clone();
    edited[position] = if edited[position] == b'a' { b'b' } else { b'a' };
    let mut sending = Box::pin(ordinary.request(create("before-write", Cursor::new(0))));
    let pending =
        std::future::poll_fn(|cx| Poll::Ready(matches!(sending.as_mut().poll(cx), Poll::Pending)))
            .await;
    std::fs::write(&locator, &edited).unwrap();
    let changed = std::fs::metadata(&locator).unwrap();
    let denied = sending.await;
    std::fs::write(&locator, &bytes).unwrap();
    let state = projects(&mut controller).await;
    running.finish().await;
    assert!(
        pending,
        "actual connect must yield before the tested mutation"
    );
    assert_eq!(
        (original.dev(), original.ino()),
        (changed.dev(), changed.ino())
    );
    assert_eq!(bytes.len(), edited.len());
    assert_eq!(denied.err().unwrap().0, "owner_locator_replaced_or_unsafe");
    assert!(state.unwrap().is_empty());
}

#[tokio::test]
async fn native_parent_alias_is_supported_and_retargeting_is_fenced() {
    let root = root();
    let other = crate::root();
    let anchor = root.path().join("anchor");
    let replacement = other.path().join("anchor");
    for path in [&anchor, &replacement] {
        std::fs::create_dir(path).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let alias = root.path().join("alias");
    std::os::unix::fs::symlink(root.path(), &alias).unwrap();
    let running = Running::service(root.clone(), &anchor).await;
    let observed: Result<_, EngineError> = async {
        let (ordinary, mut controller) = OwnerController::attach_configured(
            client(alias.join("anchor/e.sock")),
            OwnerClientKind::Desktop,
        )
        .await?;
        let (_, identity) =
            OwnerClient::discover(&anchor, OwnerClientKind::Desktop, Duration::from_secs(2))
                .await?;
        let paired = controller.identity() == &identity;
        let first = ordinary
            .request(create("alias-project", Cursor::new(0)))
            .await?;
        std::fs::remove_file(&alias).unwrap();
        std::os::unix::fs::symlink(other.path(), &alias).unwrap();
        let refused = ordinary.request(create("retargeted", first.revision)).await;
        let names = projects(&mut controller).await?;
        Ok((paired, first.outcome, refused, names))
    }
    .await;
    running.finish().await;
    let (paired, first, refused, names) = observed.unwrap();
    assert!(paired);
    assert_eq!(first, Outcome::Applied);
    assert_eq!(refused.err().unwrap().0, "owner_anchor_replaced_or_unsafe");
    assert_eq!(names, vec!["alias-project"]);
}

#[tokio::test]
async fn actual_restart_requires_rebinding_and_retains_pending_controller_intent() {
    let root = root();
    let running = Running::service(root.clone(), root.path()).await;
    let socket = root.path().join("e.sock");
    let (ordinary, mut controller) =
        OwnerController::attach_configured(client(socket.clone()), OwnerClientKind::Desktop)
            .await
            .unwrap();
    let old = controller.identity().clone();
    let before_socket = std::fs::metadata(&socket).unwrap();
    let before_locator = std::fs::metadata(root.path().join("owner-current")).unwrap();
    let first = ordinary
        .request(create("project", Cursor::new(0)))
        .await
        .unwrap();
    projects(&mut controller).await.unwrap();
    controller
        .begin_mutation(
            OwnerMutation::ApproveBaseline {
                baseline_id: BaselineId::new("not-created").unwrap(),
            },
            23,
        )
        .unwrap();
    let pending = controller.pending().unwrap().clone();
    running.finish().await;
    let restarted = Running::service(root.clone(), root.path()).await;
    let after_socket = std::fs::metadata(&socket).unwrap();
    let after_locator = std::fs::metadata(root.path().join("owner-current")).unwrap();
    eprintln!(
        "actual restart socket dev/inode {:?} -> {:?}; locator {:?} -> {:?}",
        (before_socket.dev(), before_socket.ino()),
        (after_socket.dev(), after_socket.ino()),
        (before_locator.dev(), before_locator.ino()),
        (after_locator.dev(), after_locator.ino())
    );
    let denied = ordinary.request(create("old-client", first.revision)).await;
    let rebound = controller.reattach_configured(client(socket)).await;
    let current = controller.identity().clone();
    let retained = controller.pending() == Some(&pending);
    let stale = controller.view().is_some_and(|v| !v.fresh);
    let observed: Result<_, EngineError> = async {
        let bound = rebound?;
        let (_, direct) = OwnerClient::discover(
            root.path(),
            OwnerClientKind::Desktop,
            Duration::from_secs(2),
        )
        .await?;
        let snapshot = bound
            .request(request("after-restart", Cursor::new(0), Command::Snapshot))
            .await?;
        let effect = bound
            .request(create("new-client", snapshot.revision))
            .await?;
        let names = projects(&mut controller).await?;
        Ok((direct, effect.outcome, names))
    }
    .await;
    restarted.finish().await;
    let (direct, effect, names) = observed.unwrap();
    assert!(denied.is_err());
    assert_eq!(old.database_instance(), current.database_instance());
    assert_ne!(old.engine_epoch(), current.engine_epoch());
    assert_eq!(current, direct);
    assert!(retained && stale);
    assert_eq!(effect, Outcome::Applied);
    assert_eq!(names, vec!["new-client", "project"]);
}

#[tokio::test]
async fn configured_reconnect_preserves_pending_on_other_database_and_bad_socket() {
    let root = root();
    let other = crate::root();
    let running = Running::service(root.clone(), root.path()).await;
    let second = Running::service(other.clone(), other.path()).await;
    let (ordinary, mut controller) = OwnerController::attach_configured(
        client(root.path().join("e.sock")),
        OwnerClientKind::Tui,
    )
    .await
    .unwrap();
    let observed: Result<_, EngineError> = async {
        projects(&mut controller).await?;
        controller.begin_mutation(
            OwnerMutation::ApproveBaseline {
                baseline_id: BaselineId::new("not-created").unwrap(),
            },
            24,
        )?;
        let pending = controller.pending().unwrap().clone();
        let old = controller.identity().clone();
        let denied = controller
            .reattach_configured(client(other.path().join("e.sock")))
            .await;
        let disconnected = controller.read_page(OwnerView::Projects, None, None).await;
        let fixed = controller.pending() == Some(&pending)
            && controller.identity() == &old
            && controller.view().is_some_and(|v| !v.fresh);
        let bad = controller
            .reattach_configured(client(root.path().join("missing.sock")))
            .await;
        let retained = controller.pending() == Some(&pending);
        let restored = controller.reattach_configured(ordinary).await?;
        let actual = restored
            .request(request("restored", Cursor::new(0), Command::Snapshot))
            .await?;
        Ok((denied, disconnected, fixed, bad, retained, actual.outcome))
    }
    .await;
    second.finish().await;
    running.finish().await;
    let (denied, disconnected, fixed, bad, retained, actual) = observed.unwrap();
    assert_eq!(
        denied.err().unwrap().0,
        "controller_pending_database_mismatch"
    );
    assert_eq!(disconnected.err().unwrap().0, "controller_disconnected");
    assert!(fixed && retained);
    assert_eq!(bad.err().unwrap().0, "owner_ordinary_socket_mismatch");
    assert!(matches!(actual, Outcome::Snapshot { .. }));
}

#[tokio::test]
async fn owned_service_cleanup_completes_after_fixture_panic() {
    let root = root();
    let running = Running::service(root.clone(), root.path()).await;
    let closed = running.closed.clone();
    let failed = tokio::spawn(async move {
        let _owned = running;
        panic!("exercise native fixture panic cleanup");
    })
    .await;
    let cleanup = tokio::time::timeout(Duration::from_secs(5), async {
        while !closed.load(Ordering::SeqCst) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    assert!(failed.unwrap_err().is_panic());
    assert!(cleanup.is_ok());
    assert!(!root.path().join("e.sock").exists());
    assert!(!root.path().join("owner-current").exists());
}

#[tokio::test]
async fn configured_controller_reconnect_uses_its_fixed_actually_revoked_kind() {
    use crazyagent::engine::owner::{OwnerControl, OwnerDecision};
    let root = root();
    let config = resolve_config(&root.path().join("g"), &root.path().join("p")).unwrap();
    let (_stop, stopped) = tokio::sync::watch::channel(false);
    let mut service = EngineService::start(root.path(), "e.sock", &config, &stopped)
        .await
        .unwrap();
    let observed: Result<_, EngineError> = async {
        let (_, mut tui) = OwnerController::attach_configured(
            client(root.path().join("e.sock")),
            OwnerClientKind::Tui,
        )
        .await?;
        let (_, mut desktop) = OwnerController::attach_configured(
            client(root.path().join("e.sock")),
            OwnerClientKind::Desktop,
        )
        .await?;
        projects(&mut tui).await?;
        let host = service
            .owner_runtime()
            .ok_or_else(|| EngineError("host absent".into()))?;
        let epoch = host.epoch(OwnerClientKind::Tui)?;
        let control = OwnerControl {
            operation_id: RequestId::new("pairing-revoke-tui").unwrap(),
            target: OwnerClientKind::Tui,
            expected_epoch: epoch,
            decision: OwnerDecision::Revoke,
        };
        let revoked = host.control(control.clone()).await?;
        let historical = host.reconcile_control(control).await?;
        let old_denied = tui
            .read_page(OwnerView::Projects, None, None)
            .await
            .is_err();
        let reconnect_denied = tui
            .reattach_configured(client(root.path().join("e.sock")))
            .await
            .is_err();
        let stale = tui.view().is_some_and(|v| !v.fresh);
        let other_live = projects(&mut desktop).await?;
        let activated = host
            .control(OwnerControl {
                operation_id: RequestId::new("pairing-reactivate-tui").unwrap(),
                target: OwnerClientKind::Tui,
                expected_epoch: host.epoch(OwnerClientKind::Tui)?,
                decision: OwnerDecision::Reactivate,
            })
            .await?;
        let bound = tui
            .reattach_configured(client(root.path().join("e.sock")))
            .await?;
        let effect = bound.request(create("reactivated", Cursor::new(0))).await?;
        let seen = projects(&mut tui).await?;
        Ok((
            revoked,
            historical,
            old_denied,
            reconnect_denied,
            stale,
            other_live,
            activated,
            effect.outcome,
            seen,
        ))
    }
    .await;
    let closed = tokio::time::timeout(Duration::from_secs(5), service.shutdown()).await;
    let absent = service.owner_runtime().is_none();
    closed.unwrap().unwrap();
    let (
        revoked,
        historical,
        old_denied,
        reconnect_denied,
        stale,
        other_live,
        activated,
        effect,
        seen,
    ) = observed.unwrap();
    assert!(!revoked.active);
    assert_eq!(historical, Some(revoked.clone()));
    assert!(old_denied && reconnect_denied && stale);
    assert!(other_live.is_empty());
    assert!(activated.active);
    assert_ne!(revoked.owner_epoch, activated.owner_epoch);
    assert_eq!(effect, Outcome::Applied);
    assert_eq!(seen, vec!["reactivated"]);
    assert!(absent);
    assert!(!root.path().join("owner-current").exists());
}

#[tokio::test]
async fn configured_reconnect_without_pending_clears_the_old_database_view() {
    let root = root();
    let other = crate::root();
    let running = Running::service(root.clone(), root.path()).await;
    let second = Running::service(other.clone(), other.path()).await;
    let observed: Result<_, EngineError> = async {
        let (ordinary, mut controller) = OwnerController::attach_configured(
            client(root.path().join("e.sock")),
            OwnerClientKind::Desktop,
        )
        .await?;
        ordinary.request(create("original", Cursor::new(0))).await?;
        let original = projects(&mut controller).await?;
        let old = controller.identity().clone();
        let bound = controller
            .reattach_configured(client(other.path().join("e.sock")))
            .await?;
        let cleared = controller.view().is_none() && controller.pending().is_none();
        let changed = controller.identity().database_instance() != old.database_instance();
        let effect = bound.request(create("replacement", Cursor::new(0))).await?;
        let names = projects(&mut controller).await?;
        Ok((original, cleared, changed, effect.outcome, names))
    }
    .await;
    second.finish().await;
    running.finish().await;
    let (original, cleared, changed, effect, names) = observed.unwrap();
    assert_eq!(original, vec!["original"]);
    assert!(cleared && changed);
    assert_eq!(effect, Outcome::Applied);
    assert_eq!(names, vec!["replacement"]);
}
