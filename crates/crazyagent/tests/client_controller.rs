use crazyagent::engine::Client;
use crazyagent::engine::bootstrap::EngineService;
use crazyagent::engine::owner::{
    OwnerClient, OwnerClientKind, OwnerData, OwnerMutation, OwnerOperation, OwnerRead,
    OwnerRequest, OwnerResponse, OwnerResult, OwnerView,
};
use crazyagent::protocol::*;
use crazyagent::verification::*;
use rustix::process::{Pid, Signal, WaitOptions, kill_process, waitpid};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::Path;
use std::process::{Child, Command as ProcessCommand, Stdio};
use std::time::{Duration, Instant};

struct OwnedCli {
    child: Child,
}

impl OwnedCli {
    fn start(root: &Path) -> Self {
        let child = ProcessCommand::new(env!("CARGO_BIN_EXE_crazyagent"))
            .args(["engine"])
            .arg(root)
            .arg(root.join("global.toml"))
            .arg(root.join("project.toml"))
            .arg("engine.sock")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        Self { child }
    }

    fn pid(&self) -> Pid {
        Pid::from_raw(self.child.id().try_into().unwrap()).unwrap()
    }

    async fn ready(&mut self, root: &Path) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            assert!(
                self.child.try_wait().unwrap().is_none(),
                "CLI exited before readiness"
            );
            if OwnerClient::discover(root, OwnerClientKind::Desktop, Duration::from_millis(200))
                .await
                .is_ok()
            {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "authenticated readiness deadline"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    fn pause(&mut self) {
        kill_process(self.pid(), Signal::STOP).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if let Some((pid, status)) = waitpid(
                Some(self.pid()),
                WaitOptions::UNTRACED | WaitOptions::NOHANG,
            )
            .unwrap()
            {
                assert_eq!(pid, self.pid());
                assert!(status.stopped(), "owned CLI did not stop");
                return;
            }
            assert!(Instant::now() < deadline, "owned child stop deadline");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn resume(&self) {
        kill_process(self.pid(), Signal::CONT).unwrap();
    }

    fn finish(&mut self) -> bool {
        if self.child.try_wait().ok().flatten().is_some() {
            return true;
        }
        let _ = kill_process(self.pid(), Signal::CONT);
        let _ = kill_process(self.pid(), Signal::TERM);
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            if self.child.try_wait().ok().flatten().is_some() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        let _ = self.child.kill();
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            if self.child.try_wait().ok().flatten().is_some() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        false
    }
}

impl Drop for OwnedCli {
    fn drop(&mut self) {
        if !self.finish() {
            eprintln!("owned CLI cleanup deadline");
        }
    }
}

fn request(id: &str, revision: Cursor, operation: OwnerOperation) -> OwnerRequest {
    OwnerRequest {
        protocol_version: PROTOCOL_VERSION,
        request_id: RequestId::new(id).unwrap(),
        expected_revision: revision,
        operation,
    }
}

fn draft() -> BaselineDraft {
    BaselineDraft {
        version: 1,
        project_id: ProjectId::new("project").unwrap(),
        origin: BaselineOrigin::Imported,
        requirements: "Actual queued late delivery fixture".into(),
        checks: vec![
            CheckSpec {
                id: RequestId::new("code").unwrap(),
                kind: CheckKind::Code,
                minimum_risk: Risk::Low,
                content: b"real code".to_vec(),
            },
            CheckSpec {
                id: RequestId::new("integrity").unwrap(),
                kind: CheckKind::TestIntegrity,
                minimum_risk: Risk::Low,
                content: b"real integrity".to_vec(),
            },
        ],
        rules: vec![],
        retry_limit: 0,
    }
}

fn stamp(path: &Path) -> (u64, u64, u32) {
    let m = std::fs::symlink_metadata(path).unwrap();
    (m.dev(), m.ino(), m.mode())
}

async fn page(client: &OwnerClient, id: &str) -> crazyagent::engine::owner::OwnerReadReply {
    let response = client
        .request(request(
            id,
            Cursor::new(0),
            OwnerOperation::Read {
                query: OwnerRead::Page {
                    view: OwnerView::Baselines,
                    cursor: None,
                    limit: 16,
                },
            },
        ))
        .await
        .unwrap()
        .response;
    let OwnerResponse::Read { state } = response else {
        panic!("real page read required: {response:?}");
    };
    state
}

#[tokio::test]
async fn paused_owned_cli_queues_late_delivery_with_real_receipt_and_one_effect() {
    let root = tempfile::Builder::new()
        .prefix("ca-ctl-")
        .tempdir_in("/private/tmp")
        .unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut process = OwnedCli::start(root.path());
    process.ready(root.path()).await;
    let ordinary = Client::new(root.path().join("engine.sock"), Duration::from_secs(2)).unwrap();
    let seed = ordinary
        .request(WireRequest {
            protocol_version: PROTOCOL_VERSION,
            client_id: ClientId::new("controller-fixture").unwrap(),
            request_id: RequestId::new("create-project").unwrap(),
            expected_revision: Cursor::new(0),
            command: Command::CreateProject {
                project_id: ProjectId::new("project").unwrap(),
                spec: ProjectSpec {
                    name: "Controller fixture".into(),
                    repositories: vec![],
                },
            },
        })
        .await
        .unwrap();
    assert!(matches!(seed.outcome, Outcome::Applied), "{seed:?}");
    let (client, identity) = OwnerClient::discover(
        root.path(),
        OwnerClientKind::Desktop,
        Duration::from_millis(200),
    )
    .await
    .unwrap();
    let (observer, observed_identity) = OwnerClient::discover(
        root.path(),
        OwnerClientKind::Desktop,
        Duration::from_secs(2),
    )
    .await
    .unwrap();
    assert_eq!(identity, observed_identity);
    let before = page(&observer, "before").await;
    assert!(matches!(&before.data, OwnerData::Page { items, .. } if items.is_empty()));
    let pending = request(
        "queued-late-delivery",
        before.revision,
        OwnerOperation::Apply {
            mutation: OwnerMutation::ProposeBaseline { draft: draft() },
        },
    );
    let hash = pending.mutation_hash().unwrap();
    let locator: serde_json::Value =
        serde_json::from_slice(&std::fs::read(root.path().join("owner-current")).unwrap()).unwrap();
    let generation = root.path().join(locator["generation"].as_str().unwrap());
    let socket = generation.join("owner.sock");
    let metadata = (stamp(root.path()), stamp(&generation), stamp(&socket));
    println!(
        "native identity db={} epoch={} metadata={metadata:?}",
        identity.database_instance(),
        identity.engine_epoch()
    );
    process.pause();
    let result = client.request(pending.clone()).await;
    assert!(result.is_err(), "paused owner unexpectedly replied");
    println!("paused request outcome={}", result.unwrap_err());
    assert_eq!(
        metadata,
        (stamp(root.path()), stamp(&generation), stamp(&socket))
    );
    process.resume();
    let deadline = Instant::now() + Duration::from_secs(5);
    let committed = loop {
        let reply = observer
            .request(request(
                "observe-late-receipt",
                Cursor::new(0),
                OwnerOperation::Reconcile {
                    operation_id: pending.request_id.clone(),
                    expected_hash: Some(hash.clone()),
                },
            ))
            .await
            .unwrap();
        match reply.response {
            OwnerResponse::Committed { operation } => break operation,
            OwnerResponse::NotRecordedAtBarrier { .. } if Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            response => panic!("queued late delivery fixture blocker: {response:?}"),
        }
    };
    assert!(
        matches!(committed.result, OwnerResult::Applied),
        "{committed:?}"
    );
    let after = page(&observer, "after").await;
    assert!(matches!(&after.data, OwnerData::Page { items, .. } if items.len() == 1));
    let replay = observer.request(pending).await.unwrap();
    let OwnerResponse::Applied { operation } = replay.response else {
        panic!("real replay receipt required");
    };
    assert_eq!(operation.result, committed.result);
    assert_eq!(operation.provenance, committed.provenance);
    assert!(operation.historical_replay);
    let repeated = page(&observer, "after-retry").await;
    assert!(matches!(&repeated.data, OwnerData::Page { items, .. } if items.len() == 1));
    println!("actual late receipt, independent baseline count=1, exact retry count=1");
    assert!(process.finish(), "owned CLI cleanup failed");
}

#[tokio::test]
async fn bootstrap_retains_real_host_control_until_shutdown() {
    let root = tempfile::Builder::new()
        .prefix("ca-ctl-")
        .tempdir_in("/private/tmp")
        .unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let config =
        crazyagent::config::resolve_config(&root.path().join("g"), &root.path().join("p")).unwrap();
    let (_stop, stopped) = tokio::sync::watch::channel(false);
    let mut service = EngineService::start(root.path(), "e.sock", &config, &stopped)
        .await
        .unwrap();
    let discovered = OwnerClient::discover(
        root.path(),
        OwnerClientKind::Desktop,
        Duration::from_secs(2),
    )
    .await;
    let retained = service.owner_runtime().is_some();
    let closed = service.shutdown().await;
    let absent_after_close = service.owner_runtime().is_none();
    closed.unwrap();
    assert!(
        discovered.is_ok(),
        "actual discovery must precede retention assertion"
    );
    assert!(
        retained,
        "real bootstrap must retain its actual native host handle"
    );
    assert!(absent_after_close);
}

#[tokio::test]
async fn real_host_revoke_reactivate_keeps_other_kind_live() {
    use crazyagent::engine::owner::{OwnerControl, OwnerDecision};
    let root = tempfile::Builder::new()
        .prefix("ca-ctl-")
        .tempdir_in("/private/tmp")
        .unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let config =
        crazyagent::config::resolve_config(&root.path().join("g"), &root.path().join("p")).unwrap();
    let (_stop, stopped) = tokio::sync::watch::channel(false);
    let mut service = EngineService::start(root.path(), "e.sock", &config, &stopped)
        .await
        .unwrap();
    let observed: Result<_, crazyagent::engine::EngineError> = async {
        let (desktop, identity) = OwnerClient::discover(
            root.path(),
            OwnerClientKind::Desktop,
            Duration::from_secs(2),
        )
        .await?;
        let (tui, _) =
            OwnerClient::discover(root.path(), OwnerClientKind::Tui, Duration::from_secs(2))
                .await?;
        let host = service
            .owner_runtime()
            .ok_or_else(|| crazyagent::engine::EngineError("missing runtime".into()))?;
        let epoch = host.epoch(OwnerClientKind::Desktop)?;
        let control = OwnerControl {
            operation_id: RequestId::new("revoke-desktop").unwrap(),
            target: OwnerClientKind::Desktop,
            expected_epoch: epoch,
            decision: OwnerDecision::Revoke,
        };
        let revoked = host.control(control.clone()).await?;
        let historical = host.reconcile_control(control).await?;
        let old_denied = desktop
            .request(request(
                "old-denied",
                Cursor::new(0),
                OwnerOperation::Read {
                    query: OwnerRead::Capabilities,
                },
            ))
            .await
            .is_err();
        let discovery_denied = OwnerClient::discover(
            root.path(),
            OwnerClientKind::Desktop,
            Duration::from_secs(2),
        )
        .await
        .is_err();
        let other_live = tui
            .request(request(
                "other-live",
                Cursor::new(0),
                OwnerOperation::Read {
                    query: OwnerRead::Capabilities,
                },
            ))
            .await?;
        let reactivated = host
            .control(OwnerControl {
                operation_id: RequestId::new("reactivate-desktop").unwrap(),
                target: OwnerClientKind::Desktop,
                expected_epoch: host.epoch(OwnerClientKind::Desktop)?,
                decision: OwnerDecision::Reactivate,
            })
            .await?;
        let current_epoch = host.epoch(OwnerClientKind::Desktop)?;
        let (_, current_identity) = OwnerClient::discover(
            root.path(),
            OwnerClientKind::Desktop,
            Duration::from_secs(2),
        )
        .await?;
        Ok((
            revoked,
            historical,
            old_denied,
            discovery_denied,
            other_live,
            reactivated,
            current_epoch,
            identity,
            current_identity,
        ))
    }
    .await;
    let closed = service.shutdown().await;
    let absent = service.owner_runtime().is_none();
    closed.unwrap();
    let (
        revoked,
        historical,
        old_denied,
        discovery_denied,
        other_live,
        reactivated,
        epoch,
        identity,
        current_identity,
    ) = observed.unwrap();
    assert!(!revoked.active);
    assert_eq!(historical, Some(revoked.clone()));
    assert!(old_denied);
    assert!(discovery_denied);
    assert!(matches!(other_live.response, OwnerResponse::Read { .. }));
    assert!(reactivated.active);
    assert!(epoch > revoked.owner_epoch);
    assert_eq!(epoch, reactivated.owner_epoch);
    assert_eq!(identity, current_identity);
    assert!(absent);
}

#[tokio::test]
async fn owned_paused_child_is_reaped_during_panic_cleanup() {
    let root = tempfile::Builder::new()
        .prefix("ca-ctl-")
        .tempdir_in("/private/tmp")
        .unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut process = OwnedCli::start(root.path());
    process.ready(root.path()).await;
    let pid = process.pid();
    let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
        process.pause();
        panic!("exercise owned paused child unwinding");
    }));
    assert!(panicked.is_err());
    assert_eq!(
        waitpid(Some(pid), WaitOptions::NOHANG).unwrap_err(),
        rustix::io::Errno::CHILD
    );
}

#[tokio::test]
async fn controller_failed_read_preserves_stale_bounded_state() {
    use crazyagent::client_controller::OwnerController;
    use crazyagent::engine::owner::{OwnerControl, OwnerDecision};
    let root = tempfile::Builder::new()
        .prefix("ca-ctl-")
        .tempdir_in("/private/tmp")
        .unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let config =
        crazyagent::config::resolve_config(&root.path().join("g"), &root.path().join("p")).unwrap();
    let (_stop, stopped) = tokio::sync::watch::channel(false);
    let mut service = EngineService::start(root.path(), "e.sock", &config, &stopped)
        .await
        .unwrap();
    let observed: Result<_, crazyagent::engine::EngineError> = async {
        let mut controller = OwnerController::attach(
            root.path(),
            OwnerClientKind::Desktop,
            Duration::from_secs(2),
        )
        .await?;
        let first = controller
            .read_page(OwnerView::Projects, None, None)
            .await?;
        let host = service
            .owner_runtime()
            .ok_or_else(|| crazyagent::engine::EngineError("missing host".into()))?;
        host.control(OwnerControl {
            operation_id: RequestId::new("revoke-read").unwrap(),
            target: OwnerClientKind::Desktop,
            expected_epoch: host.epoch(OwnerClientKind::Desktop)?,
            decision: OwnerDecision::Revoke,
        })
        .await?;
        let denied = controller
            .read_page(OwnerView::Projects, None, None)
            .await
            .is_err();
        Ok((first, denied, controller.view().cloned()))
    }
    .await;
    service.shutdown().await.unwrap();
    let (first, denied, view) = observed.unwrap();
    assert!(denied);
    let view = view.unwrap();
    assert_eq!(view.state, first);
    assert!(
        !view.fresh,
        "a failed real read must invalidate cached freshness"
    );
}

#[tokio::test]
async fn controller_reads_both_kinds_and_rejects_out_of_bounds_or_wrong_root() {
    use crazyagent::client_controller::OwnerController;
    let root = tempfile::Builder::new()
        .prefix("ca-ctl-")
        .tempdir_in("/private/tmp")
        .unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let config =
        crazyagent::config::resolve_config(&root.path().join("g"), &root.path().join("p")).unwrap();
    let (_stop, stopped) = tokio::sync::watch::channel(false);
    let mut service = EngineService::start(root.path(), "e.sock", &config, &stopped)
        .await
        .unwrap();
    let observed: Result<_, crazyagent::engine::EngineError> = async {
        let mut states = Vec::new();
        for kind in [OwnerClientKind::Tui, OwnerClientKind::Desktop] {
            let mut controller =
                OwnerController::attach(root.path(), kind, Duration::from_secs(2)).await?;
            let state = controller
                .read_page(OwnerView::Projects, None, Some(32))
                .await?;
            let bounds = controller
                .read_page(OwnerView::Projects, None, Some(33))
                .await
                .is_err();
            let zero = controller
                .read_page(OwnerView::Projects, None, Some(0))
                .await
                .is_err();
            states.push((state, bounds, zero));
        }
        let missing = root.path().join("missing");
        let wrong_root =
            OwnerController::attach(&missing, OwnerClientKind::Desktop, Duration::from_secs(2))
                .await
                .is_err();
        Ok((states, wrong_root))
    }
    .await;
    service.shutdown().await.unwrap();
    let (states, wrong_root) = observed.unwrap();
    assert!(wrong_root);
    for (state, bounds, zero) in states {
        assert!(matches!(state.data, OwnerData::Page { .. }));
        assert!(bounds && zero);
    }
}

async fn seed_project(root: &Path) -> Result<(), crazyagent::engine::EngineError> {
    let client = Client::new(root.join("engine.sock"), Duration::from_secs(2))?;
    let reply = client
        .request(WireRequest {
            protocol_version: PROTOCOL_VERSION,
            client_id: ClientId::new("controller-seed").unwrap(),
            request_id: RequestId::new("create-project").unwrap(),
            expected_revision: Cursor::new(0),
            command: Command::CreateProject {
                project_id: ProjectId::new("project").unwrap(),
                spec: ProjectSpec {
                    name: "Real controller project".into(),
                    repositories: vec![],
                },
            },
        })
        .await?;
    if reply.outcome != Outcome::Applied {
        return Err(crazyagent::engine::EngineError(format!(
            "seed failed: {:?}",
            reply.outcome
        )));
    }
    Ok(())
}

async fn real_receipt(
    client: &OwnerClient,
    original: &OwnerRequest,
) -> crazyagent::engine::owner::OwnerApplied {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let reply = client
            .request(request(
                "real-receipt",
                Cursor::new(0),
                OwnerOperation::Reconcile {
                    operation_id: original.request_id.clone(),
                    expected_hash: Some(original.mutation_hash().unwrap()),
                },
            ))
            .await
            .unwrap();
        match reply.response {
            OwnerResponse::Committed { operation } => return operation,
            OwnerResponse::NotRecordedAtBarrier { .. } if Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            response => panic!("actual receipt fixture blocker: {response:?}"),
        }
    }
}

#[tokio::test]
async fn controller_cancellation_retains_exact_intent_before_await() {
    use crazyagent::client_controller::OwnerController;
    let root = tempfile::Builder::new()
        .prefix("ca-ctl-")
        .tempdir_in("/private/tmp")
        .unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut process = OwnedCli::start(root.path());
    process.ready(root.path()).await;
    seed_project(root.path()).await.unwrap();
    let mut controller = OwnerController::attach(
        root.path(),
        OwnerClientKind::Desktop,
        Duration::from_secs(2),
    )
    .await
    .unwrap();
    controller
        .read_page(OwnerView::Baselines, None, None)
        .await
        .unwrap();
    let submitted = controller
        .begin_mutation(OwnerMutation::ProposeBaseline { draft: draft() }, 7)
        .unwrap()
        .clone();
    let (observer, _) = OwnerClient::discover(
        root.path(),
        OwnerClientKind::Desktop,
        Duration::from_secs(2),
    )
    .await
    .unwrap();
    process.pause();
    let cancelled = tokio::time::timeout(Duration::from_millis(100), controller.send_pending())
        .await
        .is_err();
    let pending_after = controller.pending().cloned();
    let blocked = controller
        .begin_mutation(OwnerMutation::ProposeBaseline { draft: draft() }, 8)
        .is_err();
    process.resume();
    let receipt = real_receipt(&observer, submitted.request()).await;
    let observed = page(&observer, "independent-effect").await;
    assert!(process.finish());
    assert!(cancelled);
    assert!(matches!(receipt.result, OwnerResult::Applied));
    assert!(matches!(observed.data, OwnerData::Page { items, .. } if items.len() == 1));
    let pending_after =
        pending_after.expect("cancellation must retain the exact pending request before awaiting");
    assert_eq!(pending_after.request(), submitted.request());
    assert_eq!(pending_after.hash(), submitted.hash());
    assert_eq!(pending_after.generation(), 7);
    assert!(pending_after.uncertain());
    assert!(
        blocked,
        "identical replacement generation must not displace submitted intent"
    );
}

#[tokio::test]
async fn controller_refuses_different_database_with_pending_intent() {
    use crazyagent::client_controller::OwnerController;
    let first = tempfile::Builder::new()
        .prefix("ca-ctl-")
        .tempdir_in("/private/tmp")
        .unwrap();
    let second = tempfile::Builder::new()
        .prefix("ca-ctl-")
        .tempdir_in("/private/tmp")
        .unwrap();
    for root in [first.path(), second.path()] {
        std::fs::set_permissions(root, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let mut first_cli = OwnedCli::start(first.path());
    first_cli.ready(first.path()).await;
    let mut second_cli = OwnedCli::start(second.path());
    second_cli.ready(second.path()).await;
    let mut controller = OwnerController::attach(
        first.path(),
        OwnerClientKind::Desktop,
        Duration::from_secs(2),
    )
    .await
    .unwrap();
    controller
        .read_page(OwnerView::Projects, None, None)
        .await
        .unwrap();
    let identity = controller.identity().clone();
    let pending = controller
        .begin_mutation(OwnerMutation::ProposeBaseline { draft: draft() }, 9)
        .unwrap()
        .clone();
    let refused = controller.reattach(second.path()).await.is_err();
    let unchanged_identity = *controller.identity() == identity;
    let unchanged_pending = controller.pending() == Some(&pending);
    let cannot_send = if refused {
        controller.retry_pending().await.is_err()
    } else {
        false
    };
    assert!(first_cli.finish() && second_cli.finish());
    assert!(
        refused,
        "pending intent must refuse a different database before installing the client"
    );
    assert!(unchanged_identity && unchanged_pending && cannot_send);
}

#[tokio::test]
async fn controller_reconcile_barrier_retains_then_real_receipt_settles() {
    use crazyagent::client_controller::OwnerController;
    let root = tempfile::Builder::new()
        .prefix("ca-ctl-")
        .tempdir_in("/private/tmp")
        .unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut process = OwnedCli::start(root.path());
    process.ready(root.path()).await;
    seed_project(root.path()).await.unwrap();
    let mut controller = OwnerController::attach(
        root.path(),
        OwnerClientKind::Desktop,
        Duration::from_millis(200),
    )
    .await
    .unwrap();
    controller
        .read_page(OwnerView::Baselines, None, None)
        .await
        .unwrap();
    let submitted = controller
        .begin_mutation(OwnerMutation::ProposeBaseline { draft: draft() }, 12)
        .unwrap()
        .clone();
    let barrier = controller.reconcile_pending().await.unwrap();
    let barrier_retained = controller.pending() == Some(&submitted);
    let (observer, _) = OwnerClient::discover(
        root.path(),
        OwnerClientKind::Desktop,
        Duration::from_secs(2),
    )
    .await
    .unwrap();
    process.pause();
    let timeout = controller.send_pending().await.is_err();
    process.resume();
    let original = real_receipt(&observer, submitted.request()).await;
    let reconciled = controller.reconcile_pending().await.unwrap();
    let settled = controller.pending().is_none();
    let after = page(&observer, "settled-effect").await;
    assert!(process.finish());
    assert!(matches!(
        barrier,
        OwnerResponse::NotRecordedAtBarrier { .. }
    ));
    assert!(barrier_retained && timeout);
    let OwnerResponse::Committed { operation } = reconciled else {
        panic!("actual committed receipt required");
    };
    assert_eq!(operation.result, original.result);
    assert_eq!(operation.provenance, original.provenance);
    assert!(matches!(after.data, OwnerData::Page { items, .. } if items.len() == 1));
    assert!(
        settled,
        "actual correlated receipt must settle the pending intent"
    );
}

async fn add_project(root: &Path, name: &str) -> Result<(), crazyagent::engine::EngineError> {
    let client = Client::new(root.join("engine.sock"), Duration::from_secs(2))?;
    let state = client
        .request(WireRequest {
            protocol_version: PROTOCOL_VERSION,
            client_id: ClientId::new("controller-seed").unwrap(),
            request_id: RequestId::new(format!("snapshot-{name}")).unwrap(),
            expected_revision: Cursor::new(0),
            command: Command::Snapshot,
        })
        .await?;
    let reply = client
        .request(WireRequest {
            protocol_version: PROTOCOL_VERSION,
            client_id: ClientId::new("controller-seed").unwrap(),
            request_id: RequestId::new(format!("create-{name}")).unwrap(),
            expected_revision: state.revision,
            command: Command::CreateProject {
                project_id: ProjectId::new(name).unwrap(),
                spec: ProjectSpec {
                    name: name.into(),
                    repositories: vec![],
                },
            },
        })
        .await?;
    if reply.outcome != Outcome::Applied {
        return Err(crazyagent::engine::EngineError(format!(
            "create failed: {:?}",
            reply.outcome
        )));
    }
    Ok(())
}

#[tokio::test]
async fn controller_exact_retry_retains_submitted_generation_and_owner_isolation() {
    use crazyagent::client_controller::OwnerController;
    let root = tempfile::Builder::new()
        .prefix("ca-ctl-")
        .tempdir_in("/private/tmp")
        .unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut process = OwnedCli::start(root.path());
    process.ready(root.path()).await;
    seed_project(root.path()).await.unwrap();
    let mut controller = OwnerController::attach(
        root.path(),
        OwnerClientKind::Tui,
        Duration::from_millis(200),
    )
    .await
    .unwrap();
    controller
        .read_page(OwnerView::Baselines, None, None)
        .await
        .unwrap();
    let actual_draft = draft();
    let baseline_id = actual_draft.identity().unwrap();
    let original = controller
        .begin_mutation(
            OwnerMutation::ProposeBaseline {
                draft: actual_draft.clone(),
            },
            41,
        )
        .unwrap()
        .clone();
    process.pause();
    let timeout = controller.send_pending().await.is_err();
    let mut edited = actual_draft.clone();
    edited.requirements = "Edited while original delivery is unknown".into();
    let edited_blocked = controller
        .begin_mutation(OwnerMutation::ProposeBaseline { draft: edited }, 42)
        .is_err();
    let identical_blocked = controller
        .begin_mutation(
            OwnerMutation::ProposeBaseline {
                draft: actual_draft.clone(),
            },
            43,
        )
        .is_err();
    let retained = controller.pending().cloned().unwrap();
    process.resume();
    let (observer, _) =
        OwnerClient::discover(root.path(), OwnerClientKind::Tui, Duration::from_secs(2))
            .await
            .unwrap();
    let committed = real_receipt(&observer, original.request()).await;
    let (other, _) = OwnerClient::discover(
        root.path(),
        OwnerClientKind::Desktop,
        Duration::from_secs(2),
    )
    .await
    .unwrap();
    let foreign = other
        .request(request(
            "foreign-reconcile",
            Cursor::new(0),
            OwnerOperation::Reconcile {
                operation_id: original.request().request_id.clone(),
                expected_hash: Some(original.hash().into()),
            },
        ))
        .await
        .unwrap();
    let retried = controller.retry_pending().await.unwrap();
    let settled = controller.pending().is_none();
    let actual = controller
        .read(OwnerRead::Baseline { baseline_id })
        .await
        .unwrap();
    let count = page(&observer, "retry-count").await;
    assert!(process.finish());
    assert!(timeout && edited_blocked && identical_blocked);
    assert_eq!(retained.request(), original.request());
    assert_eq!(retained.hash(), original.hash());
    assert_eq!(retained.generation(), 41);
    assert!(matches!(
        foreign.response,
        OwnerResponse::NotRecordedAtBarrier { .. }
    ));
    let OwnerResponse::Applied { operation } = retried else {
        panic!("actual exact replay required");
    };
    assert!(operation.historical_replay);
    assert_eq!(operation.provenance, committed.provenance);
    assert_eq!(operation.result, committed.result);
    assert!(settled);
    assert!(matches!(actual.data, OwnerData::Baseline { draft, .. } if draft == actual_draft));
    assert!(matches!(count.data, OwnerData::Page { items, .. } if items.len() == 1));
}

#[tokio::test]
async fn controller_epoch_reconnect_invalidates_cursor_and_preserves_known_pending() {
    use crazyagent::client_controller::OwnerController;
    let root = tempfile::Builder::new()
        .prefix("ca-ctl-")
        .tempdir_in("/private/tmp")
        .unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut first = OwnedCli::start(root.path());
    first.ready(root.path()).await;
    seed_project(root.path()).await.unwrap();
    add_project(root.path(), "project-two").await.unwrap();
    let mut controller = OwnerController::attach(
        root.path(),
        OwnerClientKind::Desktop,
        Duration::from_millis(200),
    )
    .await
    .unwrap();
    let first_page = controller
        .read_page(OwnerView::Projects, None, Some(1))
        .await
        .unwrap();
    let OwnerData::Page {
        next: Some(cursor), ..
    } = first_page.data
    else {
        panic!("actual page cursor required");
    };
    let original_identity = controller.identity().clone();
    let pending = controller
        .begin_mutation(OwnerMutation::ProposeBaseline { draft: draft() }, 44)
        .unwrap()
        .clone();
    let (observer, _) = OwnerClient::discover(
        root.path(),
        OwnerClientKind::Desktop,
        Duration::from_secs(2),
    )
    .await
    .unwrap();
    first.pause();
    let timeout = controller.send_pending().await.is_err();
    first.resume();
    let original = real_receipt(&observer, pending.request()).await;
    controller.detach();
    let detached_stale = !controller.view().unwrap().fresh;
    assert!(first.finish());
    let mut second = OwnedCli::start(root.path());
    second.ready(root.path()).await;
    controller.reattach(root.path()).await.unwrap();
    let current_identity = controller.identity().clone();
    let stale = !controller.view().unwrap().fresh;
    let preserved = controller.pending().unwrap().request() == pending.request();
    let cursor_reply = controller
        .read_page(OwnerView::Projects, Some(cursor), Some(1))
        .await
        .unwrap();
    let cursor_stale = !controller.view().unwrap().fresh;
    let receipt = controller.reconcile_pending().await.unwrap();
    let fresh = controller
        .read_page(OwnerView::Projects, None, None)
        .await
        .unwrap();
    let fresh_state = controller.view().unwrap().fresh;
    let count = controller
        .read_page(OwnerView::Baselines, None, None)
        .await
        .unwrap();
    assert!(second.finish());
    assert!(timeout && detached_stale && stale && preserved && cursor_stale);
    assert_eq!(
        original_identity.database_instance(),
        current_identity.database_instance()
    );
    assert_ne!(
        original_identity.engine_epoch(),
        current_identity.engine_epoch()
    );
    assert!(matches!(cursor_reply.data, OwnerData::ResyncRequired));
    let OwnerResponse::Committed { operation } = receipt else {
        panic!("old receipt required");
    };
    assert_eq!(operation.provenance, original.provenance);
    assert!(fresh_state);
    assert!(matches!(fresh.data, OwnerData::Page { items, .. } if items.len() == 2));
    assert!(matches!(count.data, OwnerData::Page { items, .. } if items.len() == 1));
}

#[tokio::test]
async fn controller_stale_decision_is_inner_rejected_not_success() {
    use crazyagent::client_controller::OwnerController;
    let root = tempfile::Builder::new()
        .prefix("ca-ctl-")
        .tempdir_in("/private/tmp")
        .unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut process = OwnedCli::start(root.path());
    process.ready(root.path()).await;
    seed_project(root.path()).await.unwrap();
    let mut controller = OwnerController::attach(
        root.path(),
        OwnerClientKind::Desktop,
        Duration::from_secs(2),
    )
    .await
    .unwrap();
    let state = controller
        .read_page(OwnerView::Baselines, None, None)
        .await
        .unwrap();
    let expected = state.revision;
    let config_identity = state.config_id;
    let pending = controller
        .begin_mutation(OwnerMutation::ProposeBaseline { draft: draft() }, 45)
        .unwrap()
        .clone();
    add_project(root.path(), "intervening-project")
        .await
        .unwrap();
    let result = controller.send_pending().await.unwrap();
    let cleared = controller.pending().is_none();
    let stale = !controller.view().unwrap().fresh;
    let cannot_rebase = controller
        .begin_mutation(OwnerMutation::ProposeBaseline { draft: draft() }, 46)
        .is_err();
    let count = controller
        .read_page(OwnerView::Baselines, None, None)
        .await
        .unwrap();
    assert!(process.finish());
    assert_eq!(pending.request().expected_revision, expected);
    assert_eq!(count.config_id, config_identity);
    let OwnerResponse::Applied { operation } = result else {
        panic!("stored actual outcome required");
    };
    assert!(matches!(operation.result, OwnerResult::Rejected { code } if code == "stale_revision"));
    assert_eq!(operation.provenance.request_hash, pending.hash());
    assert!(cleared && stale && cannot_rebase);
    assert!(matches!(count.data, OwnerData::Page { items, .. } if items.is_empty()));
}

async fn trusted_seed_command(
    engine: &crazyagent::engine::Engine,
    revision: &mut Cursor,
    id: &str,
    command: Command,
) -> Result<(), crazyagent::engine::EngineError> {
    let reply = engine
        .apply(WireRequest {
            protocol_version: PROTOCOL_VERSION,
            client_id: ClientId::new("trusted-fixture-host").unwrap(),
            request_id: RequestId::new(id).unwrap(),
            expected_revision: revision.clone(),
            command,
        })
        .await?;
    if reply.outcome != Outcome::Applied {
        return Err(crazyagent::engine::EngineError(format!(
            "trusted fixture setup failed: {:?}",
            reply.outcome
        )));
    }
    *revision = reply.revision;
    Ok(())
}

#[tokio::test]
async fn controller_preserves_actual_unavailable_verification_outcome() {
    use crazyagent::client_controller::OwnerController;
    let root = tempfile::Builder::new()
        .prefix("ca-ctl-")
        .tempdir_in("/private/tmp")
        .unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let config =
        crazyagent::config::resolve_config(&root.path().join("g"), &root.path().join("p")).unwrap();
    let engine = crazyagent::engine::Engine::open(&root.path().join("engine.db"), config.clone())
        .await
        .unwrap();
    let seeded: Result<(), crazyagent::engine::EngineError> = async {
        let mut revision = Cursor::new(0);
        let baseline = draft();
        let baseline_id = baseline.identity().unwrap();
        trusted_seed_command(
            &engine,
            &mut revision,
            "project",
            Command::CreateProject {
                project_id: baseline.project_id.clone(),
                spec: ProjectSpec {
                    name: "Real unavailable fixture".into(),
                    repositories: vec![],
                },
            },
        )
        .await?;
        trusted_seed_command(
            &engine,
            &mut revision,
            "baseline",
            Command::ProposeBaseline { draft: baseline },
        )
        .await?;
        trusted_seed_command(
            &engine,
            &mut revision,
            "approve",
            Command::OwnerVerification {
                action: OwnerVerification::Approve {
                    baseline_id: baseline_id.clone(),
                },
            },
        )
        .await?;
        let snapshot = CandidateSnapshot {
            files: std::collections::BTreeMap::from([(
                "src/main.rs".into(),
                b"fn main() {}".to_vec(),
            )]),
            head_digest: "a".repeat(64),
            target_digest: "b".repeat(64),
        };
        let context = ExecutionContext {
            project_id: ProjectId::new("project").unwrap(),
            task_id: TaskId::new("task").unwrap(),
            config_id: config.id.clone(),
            snapshot_id: snapshot.identity().unwrap(),
            baseline_id,
            allowed_destinations: vec![],
            budget_ref: CredentialRef::new("budget").unwrap(),
        };
        trusted_seed_command(&engine, &mut revision, "pin", Command::PinTask { context }).await?;
        trusted_seed_command(
            &engine,
            &mut revision,
            "register",
            Command::OwnerVerification {
                action: OwnerVerification::Register {
                    id: RequestId::new("candidate").unwrap(),
                    task_id: TaskId::new("task").unwrap(),
                    snapshot,
                    environment_id: EnvironmentId::new("environment").unwrap(),
                },
            },
        )
        .await?;
        Ok(())
    }
    .await;
    engine.close().await.unwrap();
    drop(engine);
    seeded.unwrap();
    let (_stop, stopped) = tokio::sync::watch::channel(false);
    let mut service = EngineService::start(root.path(), "e.sock", &config, &stopped)
        .await
        .unwrap();
    let observed: Result<_, crazyagent::engine::EngineError> = async {
        let mut controller = OwnerController::attach(
            root.path(),
            OwnerClientKind::Desktop,
            Duration::from_secs(2),
        )
        .await?;
        let candidate_id = RequestId::new("candidate").unwrap();
        controller
            .read(OwnerRead::Inspect {
                candidate_id: candidate_id.clone(),
            })
            .await?;
        controller.begin_mutation(
            OwnerMutation::FullSuite {
                candidate_id: candidate_id.clone(),
            },
            51,
        )?;
        let full = controller.send_pending().await?;
        controller
            .read(OwnerRead::Inspect {
                candidate_id: candidate_id.clone(),
            })
            .await?;
        controller.begin_mutation(
            OwnerMutation::Verify {
                candidate_id: candidate_id.clone(),
                attempt_id: RequestId::new("attempt").unwrap(),
            },
            52,
        )?;
        let verified = controller.send_pending().await?;
        let settled = controller.pending().is_none();
        let state = controller.read(OwnerRead::Inspect { candidate_id }).await?;
        Ok((full, verified, settled, state))
    }
    .await;
    service.shutdown().await.unwrap();
    let (full, verified, settled, state) = observed.unwrap();
    assert!(
        matches!(full, OwnerResponse::Applied { operation } if operation.result == OwnerResult::Applied)
    );
    assert!(
        matches!(verified, OwnerResponse::Applied { operation } if matches!(&operation.result, OwnerResult::VerificationUnavailable { attempt_id } if attempt_id.as_str() == "attempt"))
    );
    assert!(settled);
    let OwnerData::Domain { outcome } = state.data else {
        panic!("real inspected domain required");
    };
    let Outcome::Verification { state } = *outcome else {
        panic!("actual verification state required");
    };
    assert_eq!(state.attempts.len(), 1);
    assert!(!state.attempts[0].1.outcomes.is_empty());
    assert!(
        state.attempts[0]
            .1
            .outcomes
            .values()
            .all(|v| *v == CheckOutcome::Unavailable)
    );
    assert!(state.attempts[0].1.accepts(&state.current).is_err());
}
