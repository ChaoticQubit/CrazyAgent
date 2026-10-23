use crazyagent::config::{EffectiveConfig, resolve_config};
use crazyagent::coordination::{OwnerAction, PlanAuthority, Role, WorkItem, WorkPlan};
use crazyagent::engine::Client;
use crazyagent::engine::bootstrap::EngineService;
use crazyagent::engine::owner::{
    OwnerClient, OwnerClientKind, OwnerMutation, OwnerOperation, OwnerRead, OwnerRequest,
    OwnerResponse, OwnerResult, RunnerClient,
};
use crazyagent::protocol::*;
use crazyagent::routing::{ResourceBudget, RouteRequest};
use crazyagent::verification::{
    BaselineDraft, BaselineOrigin, CandidateSnapshot, CheckKind, CheckSpec, Risk,
};
use crazyagent::worker_dispatch::{AttemptRef, DeniedReceipt, RunnerReady, dispatch_id};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs;
use std::future::Future;
use std::io::{Read, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command as ProcessCommand, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;
use tokio::sync::{Mutex, watch};

const CATALOG: &str = r#"
allowed_destinations = ["profile-a"]
[routing]
approved_accounts = ["account-a"]
[model_access]
accounts = ["account-a"]
[[model_access.connections]]
id = "connection-a"
provider = "local-service"
harness = "rcli"
protocol = "openai-compatible"
account_ref = "account-a"
endpoint = { kind = "http", url = "http://127.0.0.1:11434/v1" }
model = { id = "example/model", author = "independent-author" }
auth = { method = "none" }
[[model_access.pools]]
id = "worker-pool"
members = ["connection-a"]
projects = ["project-a"]
roles = ["worker"]
fallback_allowed = false
"#;

async fn exchange(root: &Path, command: Value) -> std::io::Result<Value> {
    tokio::time::timeout(Duration::from_secs(3), async {
        let mut stream = UnixStream::connect(root.join("control.sock")).await?;
        let request = json!({"protocol_version": PROTOCOL_VERSION, "command": command});
        stream.write_all(&encode_frame(&request).unwrap()).await?;
        let mut header = [0; 4];
        stream.read_exact(&mut header).await?;
        let length = u32::from_be_bytes(header) as usize;
        assert!(length > 0 && length <= FRAME_BYTES_MAX - 4);
        let mut bytes = vec![0; length];
        stream.read_exact(&mut bytes).await?;
        serde_json::from_slice(&bytes).map_err(std::io::Error::other)
    })
    .await
    .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "runner control deadline"))?
}

struct Runner {
    root: PathBuf,
    process: Child,
}

impl Runner {
    fn spawn(root: &Path) -> Self {
        let stderr = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(root.join("stderr.log"))
            .unwrap();
        let process = ProcessCommand::new(env!("CARGO_BIN_EXE_crazyagent-runner"))
            .arg("serve")
            .arg(root)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(stderr)
            .spawn()
            .unwrap();
        Self {
            root: root.to_owned(),
            process,
        }
    }

    async fn start(root: &Path) -> Self {
        let mut runner = Self::spawn(root);
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            assert!(
                runner.process.try_wait().unwrap().is_none(),
                "runner exited before ready"
            );
            if let Ok(status) = exchange(root, json!({"kind":"status"})).await {
                assert_eq!(status["kind"], "status");
                assert_eq!(status["identity"]["pid"], runner.process.id());
                return runner;
            }
            assert!(Instant::now() < deadline, "runner readiness deadline");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    async fn status(&self) -> Value {
        exchange(&self.root, json!({"kind":"status"}))
            .await
            .unwrap()
    }

    fn signal(&self, signal: &str) {
        assert!(matches!(signal, "-STOP" | "-CONT"));
        let mut process = ProcessCommand::new("/bin/kill")
            .arg(signal)
            .arg(self.process.id().to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        let result = loop {
            if let Some(status) = process.try_wait().unwrap() {
                break Some(status);
            }
            if Instant::now() >= deadline {
                break None;
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        if result.is_none() {
            process.kill().unwrap();
            let deadline = Instant::now() + Duration::from_secs(2);
            while process.try_wait().unwrap().is_none() {
                assert!(Instant::now() < deadline, "signal helper reap deadline");
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        assert!(
            result.is_some_and(|status| status.success()),
            "actual runner signal failed"
        );
    }

    fn stop(&mut self) {
        if self.process.try_wait().unwrap().is_none() {
            let killed = self.process.kill();
            assert!(killed.is_ok() || self.process.try_wait().unwrap().is_some());
        }
        let deadline = Instant::now() + Duration::from_secs(2);
        while self.process.try_wait().unwrap().is_none() {
            assert!(Instant::now() < deadline, "runner reap deadline");
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

impl Drop for Runner {
    fn drop(&mut self) {
        self.stop();
    }
}

fn private_root() -> tempfile::TempDir {
    let root = tempfile::Builder::new()
        .prefix("ca-rd-")
        .tempdir_in("/private/tmp")
        .unwrap();
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
    root
}

fn journal(root: &Path) -> Vec<Value> {
    let bytes = fs::read(root.join("control.log")).unwrap();
    assert!(bytes.len() <= 1_048_576);
    let mut offset = 0;
    let mut records = Vec::new();
    while offset < bytes.len() {
        let length = u32::from_be_bytes(bytes[offset..offset + 4].try_into().unwrap()) as usize;
        assert!(length > 0 && length <= FRAME_BYTES_MAX - 4);
        offset += 4;
        records.push(serde_json::from_slice(&bytes[offset..offset + length]).unwrap());
        offset += length;
    }
    assert_eq!(offset, bytes.len());
    records
}

#[tokio::test]
async fn identifier_hints_require_actual_engine_authentication() {
    let root = private_root();
    let runner = Runner::start(root.path()).await;
    let before = fs::read(root.path().join("control.log")).unwrap();
    for kind in ["deliver", "reconcile"] {
        let reply = exchange(root.path(), json!({"kind":kind, "dispatch_id":"unknown"}))
            .await
            .unwrap();
        assert_eq!(
            reply,
            json!({"kind":"error", "code":"engine_registration_unavailable"})
        );
    }
    let status = exchange(
        root.path(),
        json!({"kind":"dispatch_status", "dispatch_id":"unknown"}),
    )
    .await
    .unwrap();
    assert_eq!(status["kind"], "dispatch_status");
    assert_eq!(status["state"], "not_found");
    assert!(status["receipt"].is_null() && status["fence"].is_null());
    assert_eq!(fs::read(root.path().join("control.log")).unwrap(), before);
    assert_eq!(runner.status().await["operation_count"], 0);
    assert!(runner.status().await["native_identity"].is_null());
}

#[tokio::test]
async fn control_hints_cannot_supply_an_offer_or_native_action() {
    let root = private_root();
    let runner = Runner::start(root.path()).await;
    let before = fs::read(root.path().join("control.log")).unwrap();
    for command in [
        json!({"kind":"deliver", "dispatch_id":"unknown", "offer":{}}),
        json!({"kind":"reconcile", "dispatch_id":"unknown", "receipt":{}}),
        json!({"kind":"dispatch_status", "dispatch_id":"unknown", "action":{}}),
    ] {
        assert_eq!(
            exchange(root.path(), command).await.unwrap()["code"],
            "invalid_request"
        );
    }
    assert_eq!(fs::read(root.path().join("control.log")).unwrap(), before);
    assert_eq!(runner.status().await["native_admission"], "unavailable");
}

fn wire(id: &str, revision: u64, command: Command) -> WireRequest {
    WireRequest {
        protocol_version: PROTOCOL_VERSION,
        client_id: ClientId::new("runner-dispatch-test").unwrap(),
        request_id: RequestId::new(id).unwrap(),
        expected_revision: Cursor::new(revision),
        command,
    }
}

fn owner_request(id: &str, revision: u64, operation: OwnerOperation) -> OwnerRequest {
    OwnerRequest {
        protocol_version: PROTOCOL_VERSION,
        request_id: RequestId::new(id).unwrap(),
        expected_revision: Cursor::new(revision),
        operation,
    }
}

fn git(root: &Path, repository: &Path, label: &str, args: &[&str]) -> String {
    let output_path = root.join(format!("git-{label}.out"));
    let error_path = root.join(format!("git-{label}.err"));
    let mut process = ProcessCommand::new("/usr/bin/git")
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .args([
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "commit.gpgsign=false",
            "-C",
        ])
        .arg(repository)
        .args(args)
        .stdin(Stdio::null())
        .stdout(fs::File::create(&output_path).unwrap())
        .stderr(fs::File::create(error_path).unwrap())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    let result = loop {
        if let Some(status) = process.try_wait().unwrap() {
            break Some(status);
        }
        if Instant::now() >= deadline {
            break None;
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    if result.is_none() {
        process.kill().unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while process.try_wait().unwrap().is_none() {
            assert!(Instant::now() < deadline, "fixture Git reap deadline");
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    assert!(
        result.is_some_and(|status| status.success()),
        "actual Git fixture failed: {args:?}"
    );
    let mut output = Vec::new();
    fs::File::open(output_path)
        .unwrap()
        .take(65537)
        .read_to_end(&mut output)
        .unwrap();
    assert!(output.len() <= 65536);
    String::from_utf8(output).unwrap()
}

struct Context {
    root: PathBuf,
    config: EffectiveConfig,
    service: Arc<Mutex<EngineService>>,
}

async fn with_engine<F, T>(case: F)
where
    F: FnOnce(Context) -> T,
    T: Future<Output = ()> + Send + 'static,
{
    let root = private_root();
    let global = root.path().join("global.toml");
    fs::write(&global, CATALOG).unwrap();
    let config = resolve_config(&global, &root.path().join("project.toml")).unwrap();
    let (_stop, stopped) = watch::channel(false);
    let service = Arc::new(Mutex::new(
        EngineService::start(root.path(), "e.sock", &config, &stopped)
            .await
            .unwrap(),
    ));
    let context = Context {
        root: root.path().to_owned(),
        config,
        service: service.clone(),
    };
    let mut task = tokio::spawn(case(context));
    let (outcome, timed_out) = match tokio::time::timeout(Duration::from_secs(45), &mut task).await
    {
        Ok(outcome) => (outcome, false),
        Err(_) => {
            task.abort();
            (task.await, true)
        }
    };
    let cleanup = tokio::time::timeout(Duration::from_secs(5), async {
        service.lock().await.shutdown().await
    })
    .await;
    let closed = matches!(&cleanup, Ok(Ok(())));
    if timed_out || outcome.is_err() || !closed {
        eprintln!("runner-dispatch retained root={}", root.keep().display());
    }
    assert!(closed, "actual engine cleanup failed: {cleanup:?}");
    assert!(!timed_out, "actual dispatch case exceeded45s");
    if let Err(error) = outcome {
        if error.is_panic() {
            std::panic::resume_unwind(error.into_panic());
        }
        panic!("actual dispatch task cancelled: {error}");
    }
}

impl Context {
    async fn prepare_delivery(&self, paused: bool) -> (Runner, RequestId, RunnerReady) {
        let repository = self.root.join("repository");
        fs::create_dir(&repository).unwrap();
        git(&self.root, &repository, "init", &["init", "-b", "main"]);
        let tracked = b"fn sum(a: i32, b: i32) -> i32 { a + b }\n";
        fs::write(repository.join("tracked.rs"), tracked).unwrap();
        git(&self.root, &repository, "add", &["add", "tracked.rs"]);
        git(
            &self.root,
            &repository,
            "commit",
            &[
                "-c",
                "user.name=Dispatch Test",
                "-c",
                "user.email=dispatch@invalid",
                "commit",
                "-m",
                "actual baseline",
            ],
        );
        let head = git(&self.root, &repository, "head", &["rev-parse", "HEAD"]);
        let snapshot = CandidateSnapshot {
            files: BTreeMap::from([("tracked.rs".into(), tracked.to_vec())]),
            head_digest: format!("{:x}", Sha256::digest(head.trim().as_bytes())),
            target_digest: format!("{:x}", Sha256::digest(tracked)),
        };
        let draft = BaselineDraft {
            version: 1,
            project_id: ProjectId::new("project-a").unwrap(),
            origin: BaselineOrigin::Imported,
            requirements: "Preserve committed sum and checks".into(),
            checks: vec![
                CheckSpec {
                    id: RequestId::new("sum").unwrap(),
                    kind: CheckKind::Code,
                    minimum_risk: Risk::Low,
                    content: b"assert_eq!(sum(2, 3), 5)".to_vec(),
                },
                CheckSpec {
                    id: RequestId::new("integrity").unwrap(),
                    kind: CheckKind::TestIntegrity,
                    minimum_risk: Risk::Low,
                    content: b"reject changed expected sum".to_vec(),
                },
            ],
            rules: vec![],
            retry_limit: 0,
        };
        let baseline_id = draft.identity().unwrap();
        let scope = ExecutionContext {
            project_id: draft.project_id.clone(),
            task_id: TaskId::new("registered-parent").unwrap(),
            config_id: self.config.id.clone(),
            snapshot_id: snapshot.identity().unwrap(),
            baseline_id: baseline_id.clone(),
            allowed_destinations: self.config.settings.allowed_destinations.clone(),
            budget_ref: CredentialRef::new("budget-a").unwrap(),
        };
        let plan = WorkPlan {
            id: PlanId::new("runner-plan").unwrap(),
            config_id: self.config.id.clone(),
            scope: scope.clone(),
            tasks: vec![WorkItem {
                id: TaskId::new("worker-item").unwrap(),
                session_id: SessionId::new("worker-session").unwrap(),
                role: Role::Worker,
                native_descendant: None,
                pool_ref: Some(CredentialRef::new("worker-pool").unwrap()),
                route: RouteRequest {
                    preferred_target: None,
                    required_target: Some(CredentialRef::new("profile-a").unwrap()),
                    required_capabilities: vec!["coding".into()],
                    account_ref: CredentialRef::new("account-a").unwrap(),
                    budget_ref: scope.budget_ref.clone(),
                    resources: ResourceBudget::unit(),
                },
            }],
            dependencies: vec![],
            authority: PlanAuthority::Human,
        };
        assert_ne!(plan.scope.task_id, plan.tasks[0].id);
        let (client, owner, engine) = Client::new(self.root.join("e.sock"), Duration::from_secs(2))
            .unwrap()
            .with_owner(OwnerClientKind::Tui)
            .await
            .unwrap();
        let mut revision = 0;
        for (index, command) in [
            Command::CreateProject {
                project_id: scope.project_id.clone(),
                spec: ProjectSpec {
                    name: "Real runner dispatch".into(),
                    repositories: vec![repository.to_str().unwrap().into()],
                },
            },
            Command::AdmitRepository {
                project_id: scope.project_id.clone(),
                repository: repository.to_str().unwrap().into(),
            },
        ]
        .into_iter()
        .enumerate()
        {
            let reply = client
                .request(wire(&format!("repository-{index}"), revision, command))
                .await
                .unwrap();
            assert_eq!(reply.outcome, Outcome::Applied);
            revision = reply.revision.value();
        }
        for (index, mutation) in [
            OwnerMutation::ProposeBaseline { draft },
            OwnerMutation::ApproveBaseline { baseline_id },
        ]
        .into_iter()
        .enumerate()
        {
            let reply = owner
                .request(owner_request(
                    &format!("baseline-{index}"),
                    revision,
                    OwnerOperation::Apply { mutation },
                ))
                .await
                .unwrap();
            let OwnerResponse::Applied { operation } = reply.response else {
                panic!("real baseline mutation required");
            };
            assert_eq!(operation.result, OwnerResult::Applied);
            revision = operation.current_revision.value();
        }
        for (index, command) in [
            Command::PinTask {
                context: scope.clone(),
            },
            Command::ProposePlan { plan: plan.clone() },
        ]
        .into_iter()
        .enumerate()
        {
            let reply = client
                .request(wire(&format!("context-{index}"), revision, command))
                .await
                .unwrap();
            assert_eq!(reply.outcome, Outcome::Applied);
            revision = reply.revision.value();
        }
        for (index, action) in [
            OwnerAction::ApprovePlan {
                plan_id: plan.id.clone(),
            },
            OwnerAction::Reserve {
                plan_id: plan.id.clone(),
                item_id: plan.tasks[0].id.clone(),
            },
        ]
        .into_iter()
        .enumerate()
        {
            let reply = owner
                .request(owner_request(
                    &format!("reserve-{index}"),
                    revision,
                    OwnerOperation::Apply {
                        mutation: OwnerMutation::Coordination { action },
                    },
                ))
                .await
                .unwrap();
            let OwnerResponse::Applied { operation } = reply.response else {
                panic!("real reservation required");
            };
            assert_eq!(operation.result, OwnerResult::Applied);
            revision = operation.current_revision.value();
        }
        let reply = client
            .request(wire(
                "coordination",
                0,
                Command::CoordinationState {
                    plan_id: plan.id.clone(),
                },
            ))
            .await
            .unwrap();
        let Outcome::Coordination { state } = reply.outcome else {
            panic!("stored reservation required");
        };
        let reservation = &state.reservations[&plan.tasks[0].id];
        let binding = reservation.binding.as_ref().unwrap();
        let attempt = AttemptRef {
            plan_id: plan.id.clone(),
            item_id: binding.task_id.clone(),
            reservation_cursor: reservation.cursor.clone(),
        };
        let allocated = client
            .request(wire(
                "allocate",
                revision,
                Command::AllocateWorktree {
                    task_id: scope.task_id,
                    repository: repository.to_str().unwrap().into(),
                    owner_session: binding.session_id.clone(),
                    native_descendant: None,
                },
            ))
            .await
            .unwrap();
        assert_eq!(allocated.outcome, Outcome::Applied);
        revision = allocated.revision.value();
        let prepared = self
            .service
            .lock()
            .await
            .prepare_runner(&attempt)
            .await
            .unwrap();
        let runner = Runner::start(prepared.root()).await;
        let ready = self
            .service
            .lock()
            .await
            .attach_runner(&prepared)
            .await
            .unwrap();
        if paused {
            runner.signal("-STOP");
        }
        let dispatch = dispatch_id(engine.database_instance(), &attempt).unwrap();
        owner
            .request(owner_request(
                "prepare",
                revision,
                OwnerOperation::Apply {
                    mutation: OwnerMutation::PrepareWorkerDispatch {
                        attempt,
                        repository_slot: 0,
                    },
                },
            ))
            .await
            .unwrap();
        (runner, dispatch, ready)
    }

    async fn delivered(&self) -> (Runner, RequestId) {
        let (runner, dispatch, _) = self.prepare_delivery(false).await;
        let actual = exchange(
            &runner.root,
            json!({"kind":"dispatch_status", "dispatch_id":dispatch}),
        )
        .await
        .unwrap();
        assert_eq!(
            actual["state"], "denied",
            "actual consumed denial required: {actual}"
        );
        let receipt: DeniedReceipt = serde_json::from_value(actual["receipt"].clone()).unwrap();
        assert_eq!(receipt.dispatch_id, dispatch);
        assert!(receipt.native_identity.is_none());
        assert_eq!(
            receipt.reason,
            crazyagent::worker_dispatch::DeniedReason::NativeRuntimeAdmissionUnavailable
        );
        (runner, dispatch)
    }
}

#[tokio::test]
async fn actual_engine_fence_prevents_unconsumed_generation_delivery() {
    with_engine(|context| async move {
        let (runner, dispatch, ready) = context.prepare_delivery(true).await;
        let client = RunnerClient::new(
            &runner.root,
            &ready.registration_id,
            &ready.boot,
            Duration::from_secs(2),
        )
        .unwrap();
        assert_eq!(client.ready().await.unwrap(), ready);
        let order = client.reconcile(&dispatch).await.unwrap();
        assert_eq!(order.dispatch_id, dispatch);
        assert_eq!(order.delivery_generation, 1);
        assert!(order.offer_digest.is_none());
        runner.signal("-CONT");
        let barrier = exchange(
            &runner.root,
            json!({"kind":"reconcile", "dispatch_id":dispatch}),
        )
        .await
        .unwrap();
        assert_eq!(barrier["kind"], "dispatch_status");
        assert_eq!(barrier["state"], "not_recorded_at_barrier");
        assert!(barrier["receipt"].is_null());
        assert_eq!(
            barrier["fence"]["outcome"]["kind"],
            "not_recorded_at_barrier"
        );
        assert!(barrier["fence"]["journal_sequence"].as_u64().unwrap() > 0);
        let old_offer = client.offer(&dispatch, 1).await;
        assert!(
            old_offer.is_err(),
            "retired Engine generation must refuse an actual Offer"
        );
        let before = fs::read(runner.root.join("control.log")).unwrap();
        let delayed = exchange(
            &runner.root,
            json!({"kind":"deliver", "dispatch_id":dispatch}),
        )
        .await
        .unwrap();
        assert_eq!(delayed["kind"], "error");
        assert_eq!(fs::read(runner.root.join("control.log")).unwrap(), before);
        assert_eq!(runner.status().await["operation_count"], 0);
        let records = journal(&runner.root);
        assert_eq!(
            records
                .iter()
                .filter(|record| record["record"] == "dispatch_fence")
                .count(),
            1
        );
        assert_eq!(
            records
                .iter()
                .filter(|record| record["record"] == "dispatch_denied")
                .count(),
            0
        );
        assert!(runner.status().await["native_identity"].is_null());
    })
    .await;
}

#[tokio::test]
async fn actual_denial_replays_fences_and_restores_without_native_identity() {
    with_engine(|context| async move {
        let (mut runner, dispatch) = context.delivered().await;
        let command = json!({"kind":"dispatch_status", "dispatch_id":dispatch});
        let initial = exchange(&runner.root, command.clone()).await.unwrap();
        let receipt = initial["receipt"].clone();
        assert!(receipt["journal_sequence"].as_u64().unwrap() > 0);
        assert!(receipt["controller_generation"].as_u64().unwrap() > 0);
        let replay = exchange(
            &runner.root,
            json!({"kind":"deliver", "dispatch_id":dispatch}),
        )
        .await
        .unwrap();
        assert_eq!(replay["receipt"], receipt);
        assert_eq!(runner.status().await["operation_count"], 1);
        let (owner, _) =
            OwnerClient::discover(&context.root, OwnerClientKind::Tui, Duration::from_secs(2))
                .await
                .unwrap();
        let current = owner
            .request(owner_request(
                "retirement-revision",
                0,
                OwnerOperation::Read {
                    query: OwnerRead::Capabilities,
                },
            ))
            .await
            .unwrap();
        let OwnerResponse::Read { state } = current.response else {
            panic!("current authenticated owner revision required");
        };
        let retired = owner
            .request(owner_request(
                "retire-consumed",
                state.revision.value(),
                OwnerOperation::Apply {
                    mutation: OwnerMutation::ReconcileWorkerDispatch {
                        dispatch_id: dispatch.clone(),
                    },
                },
            ))
            .await
            .unwrap();
        let OwnerResponse::Applied { operation } = retired.response else {
            panic!("actual authenticated retirement required");
        };
        assert_eq!(
            operation.result,
            OwnerResult::DispatchPrepared {
                dispatch_id: dispatch.clone()
            }
        );
        assert!(!operation.historical_replay);
        let fence = exchange(
            &runner.root,
            json!({"kind":"reconcile", "dispatch_id":dispatch}),
        )
        .await
        .unwrap();
        assert_eq!(fence["kind"], "dispatch_status");
        assert_eq!(fence["receipt"], receipt);
        assert_eq!(fence["fence"]["outcome"]["kind"], "denied");
        assert_eq!(fence["fence"]["outcome"]["receipt"], receipt);
        let fenced_bytes = fs::read(runner.root.join("control.log")).unwrap();
        assert_eq!(
            exchange(
                &runner.root,
                json!({"kind":"reconcile", "dispatch_id":dispatch})
            )
            .await
            .unwrap(),
            fence
        );
        assert_eq!(
            fs::read(runner.root.join("control.log")).unwrap(),
            fenced_bytes
        );
        let records = journal(&runner.root);
        assert_eq!(
            records
                .iter()
                .filter(|record| record["record"] == "dispatch_denied")
                .count(),
            1
        );
        assert_eq!(
            records
                .iter()
                .filter(|record| record["record"] == "dispatch_fence")
                .count(),
            1
        );
        let sequence = receipt["journal_sequence"].as_u64().unwrap() as usize;
        assert_eq!(records[sequence - 1]["receipt"], receipt);
        let old_identity = runner.status().await["identity"].clone();
        runner.stop();
        let replacement = Runner::start(&runner.root).await;
        assert_ne!(replacement.status().await["identity"], old_identity);
        assert_eq!(exchange(&replacement.root, command).await.unwrap(), fence);
        assert_eq!(replacement.status().await["operation_count"], 1);
        assert!(replacement.status().await["native_identity"].is_null());
        assert_eq!(
            replacement.status().await["native_admission"],
            "unavailable"
        );
    })
    .await;
}

#[tokio::test]
async fn actual_denial_shares_legacy_operation_capacity_and_torn_history_is_rejected() {
    with_engine(|context| async move {
        let (mut runner, dispatch) = context.delivered().await;
        let identity = runner.status().await["identity"]["runner_id"].clone();
        let generation = runner.status().await["generation"].clone();
        assert_eq!(exchange(&runner.root, json!({"kind":"claim", "runner_id":identity, "expected_generation":generation, "controller_epoch":"legacy-after-denial"})).await.unwrap()["kind"], "claimed");
        for index in 0..127 {
            let reply = exchange(&runner.root, json!({"kind":"operate", "runner_id":identity, "controller_epoch":"legacy-after-denial", "operation_id":format!("legacy-{index}"), "action":{"kind":"launch", "session_id":"unused", "harness":"rcli", "profile_ref":"unadmitted", "worktree_lease":"unused"}})).await.unwrap();
            assert_eq!(reply["reason"], "native_runtime_admission_unavailable");
            assert!(reply["native_identity"].is_null());
        }
        assert_eq!(runner.status().await["operation_count"], 128);
        let full = fs::read(runner.root.join("control.log")).unwrap();
        let refused = exchange(&runner.root, json!({"kind":"operate", "runner_id":identity, "controller_epoch":"legacy-after-denial", "operation_id":"overflow", "action":{"kind":"launch", "session_id":"unused", "harness":"codex", "profile_ref":"unadmitted", "worktree_lease":"unused"}})).await.unwrap();
        assert_eq!(refused["code"], "operation_capacity");
        assert_eq!(fs::read(runner.root.join("control.log")).unwrap(), full);
        let receipt = exchange(&runner.root, json!({"kind":"dispatch_status", "dispatch_id":dispatch})).await.unwrap()["receipt"].clone();
        runner.stop();
        let mut replacement = Runner::start(&runner.root).await;
        assert_eq!(replacement.status().await["operation_count"], 128);
        assert_eq!(exchange(&replacement.root, json!({"kind":"dispatch_status", "dispatch_id":dispatch})).await.unwrap()["receipt"], receipt);
        replacement.stop();
        let mut file = fs::OpenOptions::new().append(true).open(replacement.root.join("control.log")).unwrap();
        file.write_all(&[0, 0, 0]).unwrap(); file.sync_all().unwrap();
        let corrupted = fs::read(replacement.root.join("control.log")).unwrap();
        let mut rejected = Runner::spawn(&replacement.root);
        let deadline = Instant::now() + Duration::from_secs(5);
        let status = loop {
            if let Some(status) = rejected.process.try_wait().unwrap() { break status; }
            assert!(Instant::now() < deadline, "corrupt journal refusal deadline");
            tokio::time::sleep(Duration::from_millis(10)).await;
        };
        assert!(!status.success());
        assert_eq!(fs::read(rejected.root.join("control.log")).unwrap(), corrupted);
    }).await;
}
