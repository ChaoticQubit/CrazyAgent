use crazyagent::config::{EffectiveConfig, resolve_config};
use crazyagent::coordination::{OwnerAction, PlanAuthority, PlanState, Role, WorkItem, WorkPlan};
use crazyagent::engine::bootstrap::EngineService;
use crazyagent::engine::owner::{
    OwnerClient, OwnerClientKind, OwnerMutation, OwnerOperation, OwnerRead, OwnerRequest,
    OwnerResponse, OwnerResult,
};
use crazyagent::engine::{Client, EngineError};
use crazyagent::protocol::*;
use crazyagent::routing::{ResourceBudget, RouteRequest};
use crazyagent::worker_dispatch::{
    AttemptRef, PreparedRunner, RunnerBootIdentity, RunnerReady, RunnerRequest,
};
use serde::Serialize;
use serde_json::Value;
use std::future::Future;
use std::io::{self, Read};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
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

struct Context {
    root: PathBuf,
    config: EffectiveConfig,
    service: Arc<Mutex<EngineService>>,
}

async fn run_case<F, T>(case: F)
where
    F: FnOnce(Context) -> T,
    T: Future<Output = ()> + Send + 'static,
{
    let root = tempfile::Builder::new()
        .prefix("ca-ta-")
        .tempdir_in("/private/tmp")
        .unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    eprintln!("trusted-runner-auth root={}", root.path().display());
    let global = root.path().join("global.toml");
    std::fs::write(&global, CATALOG).unwrap();
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
    let failed = timed_out || outcome.is_err() || !closed;
    eprintln!(
        "trusted-runner-auth cleanup engine_closed={closed} timed_out={timed_out} root={}",
        root.path().display()
    );
    if failed {
        let retained = root.keep();
        eprintln!("trusted-runner-auth retained root={}", retained.display());
    }
    assert!(
        closed,
        "actual EngineService cleanup must finish: {cleanup:?}"
    );
    assert!(!timed_out, "restricted registration case exceeded45s");
    if let Err(error) = outcome {
        if error.is_panic() {
            std::panic::resume_unwind(error.into_panic());
        }
        panic!("restricted registration task was cancelled: {error}");
    }
}

fn wire(id: &str, revision: u64, command: Command) -> WireRequest {
    WireRequest {
        protocol_version: PROTOCOL_VERSION,
        client_id: ClientId::new("trusted-runner-auth-test").unwrap(),
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

impl Context {
    async fn reserve(&self) -> (AttemptRef, Client) {
        let (client, owner, _) = Client::new(self.root.join("e.sock"), Duration::from_secs(2))
            .unwrap()
            .with_owner(OwnerClientKind::Tui)
            .await
            .unwrap();
        let scope = ExecutionContext {
            project_id: ProjectId::new("project-a").unwrap(),
            task_id: TaskId::new("registered-parent").unwrap(),
            config_id: self.config.id.clone(),
            snapshot_id: SnapshotId::new("snapshot-a").unwrap(),
            baseline_id: BaselineId::new("baseline-a").unwrap(),
            allowed_destinations: self.config.settings.allowed_destinations.clone(),
            budget_ref: CredentialRef::new("budget-a").unwrap(),
        };
        let plan = WorkPlan {
            id: PlanId::new("plan-a").unwrap(),
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
        for (index, command) in [
            Command::CreateProject {
                project_id: scope.project_id.clone(),
                spec: ProjectSpec {
                    name: "Restricted registration".into(),
                    repositories: vec![],
                },
            },
            Command::PinTask {
                context: scope.clone(),
            },
            Command::ProposePlan { plan: plan.clone() },
        ]
        .into_iter()
        .enumerate()
        {
            let reply = client
                .request(wire(&format!("setup-{index}"), index as u64, command))
                .await
                .unwrap();
            assert_eq!(reply.outcome, Outcome::Applied);
            assert_eq!(reply.revision.value(), index as u64 + 1);
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
                    &format!("owner-{index}"),
                    index as u64 + 3,
                    OwnerOperation::Apply {
                        mutation: OwnerMutation::Coordination { action },
                    },
                ))
                .await
                .unwrap();
            let OwnerResponse::Applied { operation } = reply.response else {
                panic!("actual authenticated owner mutation required");
            };
            assert_eq!(operation.result, OwnerResult::Applied);
            assert_eq!(operation.current_revision.value(), index as u64 + 4);
        }
        let state = coordination(&client, "reserved").await;
        assert_eq!(state.plan, plan);
        assert_eq!(state.reservations.len(), 1);
        assert_ne!(state.plan.scope.task_id, state.plan.tasks[0].id);
        let reservation = &state.reservations[&state.plan.tasks[0].id];
        let binding = reservation.binding.as_ref().unwrap();
        assert_eq!(binding.connection.id.as_str(), "connection-a");
        assert_eq!(binding.task_id, state.plan.tasks[0].id);
        assert_eq!(binding.attempt_cursor, reservation.cursor);
        (
            AttemptRef {
                plan_id: state.plan.id.clone(),
                item_id: binding.task_id.clone(),
                reservation_cursor: reservation.cursor.clone(),
            },
            client,
        )
    }

    async fn prepared(&self, attempt: &AttemptRef) -> PreparedRunner {
        let actual = self.service.lock().await.prepare_runner(attempt).await;
        assert!(
            actual.is_ok(),
            "missing restricted runner preparation capability: {}",
            actual
                .as_ref()
                .err()
                .map_or("none", |error| error.0.as_str())
        );
        actual.unwrap()
    }
}

async fn coordination(client: &Client, id: &str) -> PlanState {
    let reply = client
        .request(wire(
            id,
            0,
            Command::CoordinationState {
                plan_id: PlanId::new("plan-a").unwrap(),
            },
        ))
        .await
        .unwrap();
    let Outcome::Coordination { state } = reply.outcome else {
        panic!("actual stored plan required");
    };
    *state
}

fn bounded_json(path: &Path) -> Value {
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .unwrap()
        .take(2049)
        .read_to_end(&mut bytes)
        .unwrap();
    assert!(bytes.len() <= 2048, "bootstrap JSON exceeds2048bytes");
    serde_json::from_slice(&bytes).unwrap()
}

fn generation(root: &Path) -> PathBuf {
    let locator = bounded_json(&root.join("owner-current"));
    let generation = locator["generation"].as_str().unwrap();
    assert!(generation.len() == 9 && generation.starts_with('o'));
    assert!(generation[1..].bytes().all(|byte| byte.is_ascii_hexdigit()));
    root.join(generation)
}

fn capability(path: &Path) -> [u8; 32] {
    let file = std::fs::File::from(
        rustix::fs::open(
            path,
            rustix::fs::OFlags::RDONLY
                | rustix::fs::OFlags::NOFOLLOW
                | rustix::fs::OFlags::CLOEXEC
                | rustix::fs::OFlags::NONBLOCK,
            rustix::fs::Mode::empty(),
        )
        .unwrap(),
    );
    let metadata = file.metadata().unwrap();
    assert!(metadata.is_file() && metadata.mode() & 0o777 == 0o600);
    assert!(metadata.nlink() == 1 && metadata.len() == 32);
    let mut bytes = Vec::new();
    file.take(33).read_to_end(&mut bytes).unwrap();
    assert!(bytes.len() == 32, "capability length changed");
    let mut token = [0; 32];
    token.copy_from_slice(&bytes);
    token
}

fn closed(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::UnexpectedEof | io::ErrorKind::ConnectionReset | io::ErrorKind::BrokenPipe
    )
}

async fn exchange<T: Serialize>(
    socket: &Path,
    auth: Option<(&[u8; 4], u8, &[u8; 32])>,
    request: &T,
) -> io::Result<Option<Value>> {
    tokio::time::timeout(Duration::from_secs(2), async {
        let mut stream = UnixStream::connect(socket).await?;
        if let Some((principal, slot, token)) = auth {
            let mut prelude = [0; 37];
            prelude[..4].copy_from_slice(principal);
            prelude[4] = slot;
            prelude[5..].copy_from_slice(token);
            if let Err(error) = stream.write_all(&prelude).await {
                return if closed(&error) { Ok(None) } else { Err(error) };
            }
        }
        let frame = encode_frame(request).map_err(|_| io::Error::other("request frame bounds"))?;
        if let Err(error) = stream.write_all(&frame).await {
            return if closed(&error) { Ok(None) } else { Err(error) };
        }
        let mut header = [0; 4];
        if let Err(error) = stream.read_exact(&mut header).await {
            return if closed(&error) { Ok(None) } else { Err(error) };
        }
        let length = u32::from_be_bytes(header) as usize;
        if length == 0 || length > FRAME_BYTES_MAX - 4 {
            return Err(io::Error::other("reply frame bounds"));
        }
        let mut bytes = vec![0; length];
        stream.read_exact(&mut bytes).await?;
        serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|_| io::Error::other("invalid actual reply"))
    })
    .await
    .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "actual exchange deadline"))?
}

struct OwnedRunner {
    process: Child,
    root: PathBuf,
}

impl OwnedRunner {
    async fn start(prepared: &PreparedRunner) -> Self {
        let stderr = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(prepared.root().join("stderr.log"))
            .unwrap();
        let process = ProcessCommand::new(env!("CARGO_BIN_EXE_crazyagent-runner"))
            .arg("serve")
            .arg(prepared.root())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(stderr)
            .spawn()
            .unwrap();
        let mut runner = Self {
            process,
            root: prepared.root().to_owned(),
        };
        let readiness = tokio::time::timeout(Duration::from_secs(5), async {
            for _ in 0..500 {
                assert!(runner.process.try_wait().unwrap().is_none(), "actual runner exited");
                match exchange(
                    &runner.root.join("control.sock"),
                    None,
                    &serde_json::json!({"protocol_version": crazyagent::protocol::PROTOCOL_VERSION, "command": {"kind": "status"}}),
                )
                .await
                {
                    Ok(Some(status)) => {
                        assert_eq!(status["kind"], "status");
                        assert_eq!(status["identity"]["pid"], runner.process.id());
                        return;
                    }
                    Err(error) if matches!(error.kind(), io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused) => {}
                    _ => panic!("actual runner readiness failed"),
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            panic!("actual runner readiness attempts exhausted");
        })
        .await;
        assert!(readiness.is_ok(), "actual runner readiness deadline");
        runner
    }

    async fn status(&self) -> Value {
        exchange(
            &self.root.join("control.sock"),
            None,
            &serde_json::json!({"protocol_version": crazyagent::protocol::PROTOCOL_VERSION, "command": {"kind": "status"}}),
        )
        .await
        .unwrap()
        .unwrap()
    }

    fn stop(&mut self) -> io::Result<()> {
        if self.process.try_wait()?.is_some() {
            return Ok(());
        }
        if let Err(error) = self.process.kill()
            && self.process.try_wait()?.is_none()
        {
            return Err(error);
        }
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            if self.process.try_wait()?.is_some() {
                eprintln!(
                    "trusted-runner-auth runner_reaped=true pid={}",
                    self.process.id()
                );
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "owned runner reap deadline",
        ))
    }
}

impl Drop for OwnedRunner {
    fn drop(&mut self) {
        if let Err(error) = self.stop() {
            eprintln!(
                "trusted-runner-auth runner_reaped=false pid={} error={error}",
                self.process.id()
            );
            assert!(std::thread::panicking(), "owned runner cleanup failed");
        }
    }
}

#[tokio::test]
async fn restricted_runner_preparation_publishes_scoped_bootstrap() {
    run_case(|context| async move {
        let (attempt, client) = context.reserve().await;
        let before = coordination(&client, "before-prepare").await;
        let prepared = context.prepared(&attempt).await;
        assert_eq!(prepared.attempt(), &attempt);
        assert!(prepared.slot() < 128);
        assert_eq!(prepared.root().parent(), Some(context.root.as_path()));
        let metadata = std::fs::symlink_metadata(prepared.root()).unwrap();
        assert!(metadata.is_dir() && metadata.mode() & 0o777 == 0o700);
        let bootstrap = prepared.root().join("engine-bootstrap");
        let metadata = std::fs::symlink_metadata(&bootstrap).unwrap();
        assert!(metadata.is_file() && metadata.nlink() == 1);
        assert_eq!(metadata.mode() & 0o777, 0o600);
        let data = bounded_json(&bootstrap);
        assert_eq!(data["version"], 1);
        assert_eq!(data["attempt"], serde_json::to_value(&attempt).unwrap());
        assert_eq!(data["registration_id"], prepared.registration_id().as_str());
        assert_eq!(data["slot"], prepared.slot());
        assert!(data.get("token").is_none());
        assert!(data.get("capability").is_none());
        assert_eq!(coordination(&client, "after-prepare").await, before);
        assert!(!prepared.root().join("control.sock").exists());
    })
    .await;
}

#[tokio::test]
async fn actual_runner_ready_binds_the_observed_boot_and_engine() {
    run_case(|context| async move {
        let (attempt, client) = context.reserve().await;
        let before = coordination(&client, "before-ready").await;
        let prepared = context.prepared(&attempt).await;
        let runner = OwnedRunner::start(&prepared).await;
        let observed = runner.status().await;
        let ready = context.service.lock().await.attach_runner(&prepared).await;
        assert!(
            ready.is_ok(),
            "actual restricted Ready capability is required"
        );
        let ready = ready.unwrap();
        let (_, engine) =
            OwnerClient::discover(&context.root, OwnerClientKind::Tui, Duration::from_secs(2))
                .await
                .unwrap();
        assert_eq!(ready.attempt, attempt);
        assert_eq!(ready.registration_id, *prepared.registration_id());
        let observed_boot: RunnerBootIdentity =
            serde_json::from_value(observed["identity"].clone()).unwrap_or_else(|_| {
                panic!("actual runner boot must use the closed identity shape")
            });
        assert_eq!(ready.boot, observed_boot);
        assert_eq!(ready.database_instance, engine.database_instance());
        assert_eq!(ready.engine_epoch, engine.engine_epoch());
        let status = runner.status().await;
        let registered: RunnerReady = serde_json::from_value(status["engine_registration"].clone())
            .unwrap_or_else(|_| {
                panic!("actual runner registration must use the closed Ready shape")
            });
        assert_eq!(registered, ready);
        assert_eq!(status["native_admission"], "unavailable");
        assert!(status["native_identity"].is_null());
        assert_eq!(status["operation_count"], 0);
        assert_eq!(coordination(&client, "after-ready").await, before);
    })
    .await;
}

#[tokio::test]
async fn actual_capabilities_cannot_cross_runner_and_human_principals() {
    run_case(|context| async move {
        let (attempt, client) = context.reserve().await;
        let before = coordination(&client, "before-separation").await;
        let prepared = context.prepared(&attempt).await;
        let runner = OwnedRunner::start(&prepared).await;
        let ready = context
            .service
            .lock()
            .await
            .attach_runner(&prepared)
            .await
            .unwrap();
        let directory = generation(&context.root);
        let socket = directory.join("owner.sock");
        let token = capability(&directory.join(format!("r{:02x}.cap", prepared.slot())));
        let boot: RunnerBootIdentity =
            serde_json::from_value(runner.status().await["identity"].clone()).unwrap();
        let request = RunnerRequest::Ready {
            protocol_version: PROTOCOL_VERSION,
            registration_id: prepared.registration_id().clone(),
            boot,
        };
        let actual = exchange(&socket, Some((b"CAR1", prepared.slot(), &token)), &request)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(actual["kind"], "ready");
        let repeated: RunnerReady = serde_json::from_value(actual["identity"].clone()).unwrap();
        assert_eq!(repeated, ready);
        let owner_read = owner_request(
            "cross-principal",
            0,
            OwnerOperation::Read {
                query: OwnerRead::Capabilities,
            },
        );
        assert!(
            exchange(
                &socket,
                Some((b"CAR1", prepared.slot(), &token)),
                &owner_read
            )
            .await
            .unwrap()
            .is_none()
        );
        for (slot, name, kind) in [
            (0, "owner-tui.cap", OwnerClientKind::Tui),
            (1, "owner-desktop.cap", OwnerClientKind::Desktop),
        ] {
            let human = capability(&directory.join(name));
            assert!(
                exchange(&socket, Some((b"CAR1", prepared.slot(), &human)), &request)
                    .await
                    .unwrap()
                    .is_none()
            );
            assert!(
                exchange(&socket, Some((b"CAO1", slot, &token)), &owner_read)
                    .await
                    .unwrap()
                    .is_none()
            );
            assert!(
                exchange(&socket, Some((b"CAO1", slot, &human)), &request)
                    .await
                    .unwrap()
                    .is_none()
            );
            let (owner, _) = OwnerClient::discover(&context.root, kind, Duration::from_secs(2))
                .await
                .unwrap();
            let reply = owner.request(owner_read.clone()).await.unwrap();
            let OwnerResponse::Read { state } = reply.response else {
                panic!("original authenticated human principal must remain usable");
            };
            let data = serde_json::to_value(state.data).unwrap();
            assert_eq!(data["authenticated_owner_transport"], true);
            assert_eq!(data["native_worker_admission"], false);
        }
        let status = runner.status().await;
        assert_eq!(status["operation_count"], 0);
        assert!(status["native_identity"].is_null());
        assert_eq!(coordination(&client, "after-separation").await, before);
    })
    .await;
}

#[tokio::test]
async fn replaced_prepared_bootstrap_cannot_authenticate_a_runner() {
    run_case(|context| async move {
        let (attempt, client) = context.reserve().await;
        let before = coordination(&client, "before-drift").await;
        let prepared = context.prepared(&attempt).await;
        let runner = OwnedRunner::start(&prepared).await;
        let path = prepared.root().join("engine-bootstrap");
        let original = std::fs::symlink_metadata(&path).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        std::fs::rename(&path, prepared.root().join("engine-bootstrap.original")).unwrap();
        let mut replacement = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
            .unwrap();
        std::io::Write::write_all(&mut replacement, &bytes).unwrap();
        replacement.sync_all().unwrap();
        let changed = replacement.metadata().unwrap();
        assert!(original.dev() != changed.dev() || original.ino() != changed.ino());
        let denied = context.service.lock().await.attach_runner(&prepared).await;
        assert!(
            denied.is_err(),
            "replaced prepared bootstrap must fail closed"
        );
        assert_eq!(
            std::fs::symlink_metadata(&path).unwrap().ino(),
            changed.ino()
        );
        let status = runner.status().await;
        assert_eq!(status["operation_count"], 0);
        assert!(status["native_identity"].is_null());
        assert!(status.get("engine_registration").is_none_or(Value::is_null));
        assert_eq!(coordination(&client, "after-drift").await, before);
    })
    .await;
}

#[tokio::test]
async fn zero_cursor_is_validation_and_cannot_publish_registration() {
    run_case(|context| async move {
        let (mut attempt, client) = context.reserve().await;
        let before = coordination(&client, "before-invalid").await;
        attempt.reservation_cursor = Cursor::new(0);
        let actual = context.service.lock().await.prepare_runner(&attempt).await;
        assert!(
            matches!(actual, Err(EngineError(ref code)) if code == "runner_attempt_cursor_zero")
        );
        assert_eq!(coordination(&client, "after-invalid").await, before);
        assert!(std::fs::read_dir(&context.root).unwrap().all(|entry| {
            let name = entry.unwrap().file_name();
            let name = name.to_string_lossy();
            !(name.len() == 3 && name.starts_with('r'))
        }));
    })
    .await;
}

#[tokio::test]
async fn unregistered_attempt_identity_cannot_publish_a_runner() {
    run_case(|context| async move {
        let (attempt, client) = context.reserve().await;
        let before = coordination(&client, "before-mismatched-attempt").await;
        let mut changed_cursor = attempt.clone();
        changed_cursor.reservation_cursor =
            Cursor::new(attempt.reservation_cursor.value().checked_add(1).unwrap());
        let mut missing_item = attempt.clone();
        missing_item.item_id = TaskId::new("unregistered-worker").unwrap();
        let mut missing_plan = attempt.clone();
        missing_plan.plan_id = PlanId::new("unregistered-plan").unwrap();
        for (invalid, expected) in [
            (changed_cursor, "runner_attempt_mismatch"),
            (missing_item, "runner_work_item_missing"),
            (missing_plan, "runner_plan_missing"),
        ] {
            let result = context.service.lock().await.prepare_runner(&invalid).await;
            assert!(matches!(result, Err(EngineError(ref code)) if code == expected));
        }
        assert_eq!(
            coordination(&client, "after-mismatched-attempt").await,
            before
        );
        assert!(std::fs::read_dir(&context.root).unwrap().all(|entry| {
            let name = entry.unwrap().file_name();
            let name = name.to_string_lossy();
            !(name.len() == 3 && name.starts_with('r'))
        }));
    })
    .await;
}

#[tokio::test]
async fn repeated_preparation_retains_the_exact_unstarted_registration() {
    run_case(|context| async move {
        let (attempt, client) = context.reserve().await;
        let before = coordination(&client, "before-repeat-preparation").await;
        let first = context.prepared(&attempt).await;
        let metadata = std::fs::symlink_metadata(first.root().join("engine-bootstrap")).unwrap();
        let repeated = context
            .service
            .lock()
            .await
            .prepare_runner(&attempt)
            .await
            .unwrap();
        assert_eq!(repeated, first);
        let retained = std::fs::symlink_metadata(repeated.root().join("engine-bootstrap")).unwrap();
        assert_eq!(
            (retained.dev(), retained.ino()),
            (metadata.dev(), metadata.ino())
        );
        assert!(!first.root().join("control.sock").exists());
        assert!(
            !generation(&context.root)
                .join(format!("r{:02x}.cap", first.slot()))
                .exists()
        );
        assert_eq!(
            coordination(&client, "after-repeat-preparation").await,
            before
        );
    })
    .await;
}

#[tokio::test]
async fn ready_rejects_a_different_boot_or_registration_without_poisoning_the_actual_identity() {
    run_case(|context| async move {
        let (attempt, client) = context.reserve().await;
        let before = coordination(&client, "before-ready-identity-rejection").await;
        let prepared = context.prepared(&attempt).await;
        let runner = OwnedRunner::start(&prepared).await;
        let ready = context
            .service
            .lock()
            .await
            .attach_runner(&prepared)
            .await
            .unwrap();
        let directory = generation(&context.root);
        let token = capability(&directory.join(format!("r{:02x}.cap", prepared.slot())));
        let mut changed_boot = ready.boot.clone();
        changed_boot.pid = changed_boot.pid.checked_add(1).unwrap();
        for request in [
            RunnerRequest::Ready {
                protocol_version: PROTOCOL_VERSION,
                registration_id: prepared.registration_id().clone(),
                boot: changed_boot,
            },
            RunnerRequest::Ready {
                protocol_version: PROTOCOL_VERSION,
                registration_id: RequestId::new("different-registration").unwrap(),
                boot: ready.boot.clone(),
            },
        ] {
            assert!(
                exchange(
                    &directory.join("owner.sock"),
                    Some((b"CAR1", prepared.slot(), &token)),
                    &request
                )
                .await
                .unwrap()
                .is_none()
            );
        }
        let actual = exchange(
            &directory.join("owner.sock"),
            Some((b"CAR1", prepared.slot(), &token)),
            &RunnerRequest::Ready {
                protocol_version: PROTOCOL_VERSION,
                registration_id: prepared.registration_id().clone(),
                boot: ready.boot.clone(),
            },
        )
        .await
        .unwrap()
        .unwrap();
        let retained: RunnerReady = serde_json::from_value(actual["identity"].clone()).unwrap();
        assert_eq!(retained, ready);
        let status = runner.status().await;
        assert_eq!(status["operation_count"], 0);
        assert!(status["native_identity"].is_null());
        assert_eq!(
            coordination(&client, "after-ready-identity-rejection").await,
            before
        );
    })
    .await;
}

#[tokio::test]
async fn revoked_plan_cannot_attach_the_observed_runner_or_publish_its_capability() {
    run_case(|context| async move {
        let (attempt, client) = context.reserve().await;
        let prepared = context.prepared(&attempt).await;
        let runner = OwnedRunner::start(&prepared).await;
        let barrier = client.request(wire("before-revoke-for-attach", 0, Command::CoordinationState {
            plan_id: attempt.plan_id.clone(),
        })).await.unwrap();
        let (owner, _) = OwnerClient::discover(&context.root, OwnerClientKind::Tui, Duration::from_secs(2))
            .await.unwrap();
        let revoked = owner.request(owner_request("revoke-before-runner-attach", barrier.revision.value(), OwnerOperation::Apply {
            mutation: OwnerMutation::Coordination {
                action: OwnerAction::RevokePlan { plan_id: attempt.plan_id.clone() },
            },
        })).await.unwrap();
        assert!(matches!(revoked.response, OwnerResponse::Applied { operation } if operation.result == OwnerResult::Applied));
        let denied = context.service.lock().await.attach_runner(&prepared).await;
        assert!(matches!(denied, Err(EngineError(ref code)) if code == "runner_plan_unavailable"));
        let status = runner.status().await;
        assert!(status["engine_registration"].is_null());
        assert_eq!(status["operation_count"], 0);
        assert!(status["native_identity"].is_null());
        assert!(!generation(&context.root).join(format!("r{:02x}.cap", prepared.slot())).exists());
        assert!(coordination(&client, "after-revoked-attach").await.revoked);
    }).await;
}

fn delivery_git(root: &Path, repository: &Path, label: &str, args: &[&str]) -> String {
    let output_path = root.join(format!("git-{label}.stdout"));
    let error_path = root.join(format!("git-{label}.stderr"));
    let output = std::fs::OpenOptions::new()
        .write(true).create_new(true).mode(0o600).open(&output_path).unwrap();
    let error = std::fs::OpenOptions::new()
        .write(true).create_new(true).mode(0o600).open(&error_path).unwrap();
    let mut process = ProcessCommand::new("/usr/bin/git")
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .args(["-c", "core.hooksPath=/dev/null", "-c", "commit.gpgsign=false", "-C"])
        .arg(repository)
        .args(args)
        .stdin(Stdio::null()).stdout(output).stderr(error)
        .spawn().unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    let result = loop {
        match process.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Ok(None) => break Err(io::Error::new(io::ErrorKind::TimedOut, "fixture Git deadline")),
            Err(error) => break Err(error),
        }
    };
    if result.is_err() {
        let _ = process.kill();
        let deadline = Instant::now() + Duration::from_secs(2);
        let reaped = loop {
            match process.try_wait() {
                Ok(Some(_)) => break true,
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                _ => break false,
            }
        };
        eprintln!("trusted-runner-auth fixture_git_reaped={reaped} pid={}", process.id());
        assert!(reaped, "actual fixture Git cleanup failed");
    }
    let status = result.unwrap_or_else(|error| panic!("actual fixture Git failed: {error}; {}", error_path.display()));
    assert!(status.success(), "actual Git {args:?} failed: {status}; {}", error_path.display());
    let mut bytes = Vec::new();
    std::fs::File::open(&output_path).unwrap().take(65537).read_to_end(&mut bytes).unwrap();
    assert!(bytes.len() <= 65536, "fixture Git stdout bound");
    String::from_utf8(bytes).unwrap()
}

#[tokio::test]
async fn engine_owned_attempt_is_consumed_and_denied_by_real_runner() {
    use crazyagent::engine::owner::OwnerData;
    use crazyagent::verification::{BaselineDraft, BaselineOrigin, CandidateSnapshot, CheckKind, CheckSpec, Risk};
    use crazyagent::worker_dispatch::{DeniedReason, DeniedReceipt, DispatchCancellation, DispatchState, dispatch_id};
    use crazyagent::worktrees::LeaseState;
    use sha2::{Digest, Sha256};

    run_case(|context| async move {
        let repository = context.root.join("repository");
        std::fs::create_dir(&repository).unwrap();
        std::fs::set_permissions(&repository, std::fs::Permissions::from_mode(0o700)).unwrap();
        delivery_git(&context.root, &repository, "init", &["init", "-b", "main"]);
        let tracked = b"fn sum(a: i32, b: i32) -> i32 { a + b }\n";
        std::fs::write(repository.join("tracked.rs"), tracked).unwrap();
        delivery_git(&context.root, &repository, "add", &["add", "tracked.rs"]);
        delivery_git(&context.root, &repository, "commit", &[
            "-c", "user.name=Dispatch Test", "-c", "user.email=dispatch@invalid",
            "commit", "-m", "actual delivery baseline",
        ]);
        let head = delivery_git(&context.root, &repository, "head", &["rev-parse", "HEAD"]).trim().to_owned();
        assert!(matches!(head.len(), 40 | 64) && head.bytes().all(|byte| byte.is_ascii_hexdigit()));
        let snapshot = CandidateSnapshot {
            files: std::collections::BTreeMap::from([("tracked.rs".into(), std::fs::read(repository.join("tracked.rs")).unwrap())]),
            head_digest: format!("{:x}", Sha256::digest(head.as_bytes())),
            target_digest: format!("{:x}", Sha256::digest(tracked)),
        };
        let draft = BaselineDraft {
            version: 1,
            project_id: ProjectId::new("project-a").unwrap(),
            origin: BaselineOrigin::Imported,
            requirements: "Preserve the committed sum implementation and its checks".into(),
            checks: vec![
                CheckSpec {
                    id: RequestId::new("sum-check").unwrap(),
                    kind: CheckKind::Code,
                    minimum_risk: Risk::Low,
                    content: b"assert_eq!(sum(2, 3), 5)".to_vec(),
                },
                CheckSpec {
                    id: RequestId::new("integrity-check").unwrap(),
                    kind: CheckKind::TestIntegrity,
                    minimum_risk: Risk::Low,
                    content: b"reject a changed expected sum".to_vec(),
                },
            ],
            rules: vec![],
            retry_limit: 0,
        };
        let baseline_id = draft.identity().unwrap();
        let scope = ExecutionContext {
            project_id: draft.project_id.clone(),
            task_id: TaskId::new("dispatch-parent").unwrap(),
            config_id: context.config.id.clone(),
            snapshot_id: snapshot.identity().unwrap(),
            baseline_id: baseline_id.clone(),
            allowed_destinations: context.config.settings.allowed_destinations.clone(),
            budget_ref: CredentialRef::new("dispatch-budget").unwrap(),
        };
        let plan = WorkPlan {
            id: PlanId::new("dispatch-plan").unwrap(),
            config_id: context.config.id.clone(),
            scope: scope.clone(),
            tasks: vec![WorkItem {
                id: TaskId::new("dispatch-item").unwrap(),
                session_id: SessionId::new("dispatch-session").unwrap(),
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
        let (client, owner, engine) = Client::new(context.root.join("e.sock"), Duration::from_secs(2))
            .unwrap().with_owner(OwnerClientKind::Tui).await.unwrap();
        let mut revision = 0;
        for (index, command) in [
            Command::CreateProject {
                project_id: scope.project_id.clone(),
                spec: ProjectSpec {
                    name: "Real consumed delivery".into(),
                    repositories: vec![repository.to_str().unwrap().into()],
                },
            },
            Command::AdmitRepository {
                project_id: scope.project_id.clone(),
                repository: repository.to_str().unwrap().into(),
            },
        ].into_iter().enumerate() {
            let reply = client.request(wire(&format!("delivery-repository-{index}"), revision, command)).await.unwrap();
            assert_eq!(reply.outcome, Outcome::Applied);
            revision = reply.revision.value();
        }
        for (index, mutation) in [
            OwnerMutation::ProposeBaseline { draft: draft.clone() },
            OwnerMutation::ApproveBaseline { baseline_id: baseline_id.clone() },
        ].into_iter().enumerate() {
            let reply = owner.request(owner_request(&format!("delivery-baseline-{index}"), revision, OwnerOperation::Apply { mutation })).await.unwrap();
            let OwnerResponse::Applied { operation } = reply.response else { panic!("real owner baseline mutation required"); };
            assert_eq!(operation.result, OwnerResult::Applied);
            revision = operation.current_revision.value();
        }
        let baseline = owner.request(owner_request("delivery-baseline-read", 0, OwnerOperation::Read {
            query: OwnerRead::Baseline { baseline_id: baseline_id.clone() },
        })).await.unwrap();
        assert!(matches!(baseline.response, OwnerResponse::Read { state } if
            matches!(&state.data, OwnerData::Baseline { draft: actual, approved: true, revoked: false, current: true } if actual == &draft)));
        for (index, command) in [
            Command::PinTask { context: scope.clone() },
            Command::ProposePlan { plan: plan.clone() },
        ].into_iter().enumerate() {
            let reply = client.request(wire(&format!("delivery-context-{index}"), revision, command)).await.unwrap();
            assert_eq!(reply.outcome, Outcome::Applied);
            revision = reply.revision.value();
        }
        let parent = client.request(wire("delivery-parent-read", 0, Command::TaskContext { task_id: scope.task_id.clone() })).await.unwrap();
        assert_eq!(parent.outcome, Outcome::Task { context: scope.clone() });
        for (index, action) in [
            OwnerAction::ApprovePlan { plan_id: plan.id.clone() },
            OwnerAction::Reserve { plan_id: plan.id.clone(), item_id: plan.tasks[0].id.clone() },
        ].into_iter().enumerate() {
            let reply = owner.request(owner_request(&format!("delivery-reservation-{index}"), revision, OwnerOperation::Apply {
                mutation: OwnerMutation::Coordination { action },
            })).await.unwrap();
            let OwnerResponse::Applied { operation } = reply.response else { panic!("real owner reservation required"); };
            assert_eq!(operation.result, OwnerResult::Applied);
            revision = operation.current_revision.value();
        }
        let reserved_reply = client.request(wire("delivery-reserved-read", 0, Command::CoordinationState {
            plan_id: plan.id.clone(),
        })).await.unwrap();
        let Outcome::Coordination { state } = reserved_reply.outcome else { panic!("actual stored plan required"); };
        let reserved = *state;
        assert_eq!(reserved.plan, plan);
        assert!(reserved.approved && !reserved.revoked);
        assert_ne!(scope.task_id, reserved.plan.tasks[0].id);
        assert_eq!(reserved.reservations.len(), 1);
        let reservation = &reserved.reservations[&plan.tasks[0].id];
        let binding = reservation.binding.as_ref().unwrap();
        assert_eq!(binding.task_id, plan.tasks[0].id);
        assert_eq!(binding.session_id, plan.tasks[0].session_id);
        assert_eq!(binding.config_id, context.config.id);
        assert_eq!(binding.connection.id.as_str(), "connection-a");
        assert_eq!(binding.connection.account_ref.as_str(), "account-a");
        assert_eq!(binding.target_profile_ref.as_str(), "profile-a");
        assert_eq!(binding.attempt_cursor, reservation.cursor);
        let attempt = AttemptRef {
            plan_id: plan.id.clone(),
            item_id: binding.task_id.clone(),
            reservation_cursor: reservation.cursor.clone(),
        };
        let allocated = client.request(wire("delivery-allocate", revision, Command::AllocateWorktree {
            task_id: scope.task_id.clone(),
            repository: repository.to_str().unwrap().into(),
            owner_session: binding.session_id.clone(),
            native_descendant: None,
        })).await.unwrap();
        assert_eq!(allocated.outcome, Outcome::Applied);
        revision = allocated.revision.value();
        let leases = client.request(wire("delivery-lease-read", 0, Command::WorktreeLeases { task_id: scope.task_id.clone() })).await.unwrap();
        let Outcome::Worktrees { leases } = leases.outcome else { panic!("real worktree lease readback required"); };
        assert_eq!(leases.len(), 1);
        let lease = &leases[0];
        assert_eq!(lease.task_id, scope.task_id);
        assert_ne!(lease.task_id, attempt.item_id);
        assert_eq!(lease.owner_session, binding.session_id);
        assert_eq!(lease.repository, repository.to_str().unwrap());
        assert_eq!(lease.state, LeaseState::Ready);
        assert!(lease.native_descendant.is_none());
        assert!(Path::new(&lease.path).is_dir());
        assert_eq!(delivery_git(&context.root, Path::new(&lease.path), "lease-head", &["rev-parse", "HEAD"]).trim(), head);
        assert_eq!(std::fs::read(Path::new(&lease.path).join("tracked.rs")).unwrap(), tracked);
        let prepared = context.prepared(&attempt).await;
        let runner = OwnedRunner::start(&prepared).await;
        let ready = context.service.lock().await.attach_runner(&prepared).await.unwrap();
        assert_eq!(ready.attempt, attempt);
        assert_eq!(ready.registration_id, *prepared.registration_id());
        assert_eq!(ready.database_instance, engine.database_instance());
        assert_eq!(ready.engine_epoch, engine.engine_epoch());
        let before_status = runner.status().await;
        let actual_ready: RunnerReady = serde_json::from_value(before_status["engine_registration"].clone()).unwrap();
        assert_eq!(actual_ready, ready);
        assert_eq!(before_status["identity"]["pid"], runner.process.id());
        assert_eq!(before_status["operation_count"], 0);
        assert_eq!(before_status["native_admission"], "unavailable");
        assert!(before_status["native_identity"].is_null());
        let before_snapshot = client.request(wire("delivery-snapshot-before", 0, Command::Snapshot)).await.unwrap().outcome;
        let dispatch = dispatch_id(engine.database_instance(), &attempt).unwrap();
        let prepared_result = owner.request(owner_request("delivery-prepare", revision, OwnerOperation::Apply {
            mutation: OwnerMutation::PrepareWorkerDispatch { attempt: attempt.clone(), repository_slot: 0 },
        })).await.unwrap();
        let readback = owner.request(owner_request("delivery-read", 0, OwnerOperation::Read {
            query: OwnerRead::WorkerDispatch { dispatch_id: dispatch.clone() },
        })).await.unwrap();
        let retained_runner = exchange(&runner.root.join("control.sock"), None, &serde_json::json!({
            "protocol_version": PROTOCOL_VERSION,
            "command": { "kind": "dispatch_status", "dispatch_id": dispatch },
        })).await.unwrap();
        let retained_receipt = retained_runner.as_ref()
            .and_then(|reply| reply.get("receipt"))
            .map(|receipt| serde_json::from_value::<DeniedReceipt>(receipt.clone()))
            .transpose();
        let view = match &readback.response {
            OwnerResponse::Read { state } => match &state.data {
                OwnerData::WorkerDispatch { dispatch } => dispatch.as_deref(),
                _ => None,
            },
            _ => None,
        };
        assert!(
            matches!((view, &retained_receipt), (Some(view), Ok(Some(receipt))) if
                view.state == DispatchState::DeliveredDenied && view.receipt.as_ref() == Some(receipt)),
            "missing real consumed dispatch receipt: prepare={prepared_result:?}; owner={readback:?}; runner={retained_runner:?}"
        );
        let view = view.unwrap();
        let receipt = retained_receipt.unwrap().unwrap();
        assert_eq!(view.dispatch_id, dispatch);
        assert_eq!(view.attempt, attempt);
        assert_eq!(view.delivery_generation, 1);
        assert_eq!(view.cancellation, DispatchCancellation::None);
        assert_eq!(view.binding_digest, format!("{:x}", Sha256::digest(serde_json::to_vec(binding).unwrap())));
        assert_eq!(view.lease_digest.len(), 64);
        assert!(view.lease_digest.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)));
        assert_eq!(receipt.dispatch_id, dispatch);
        assert_eq!(receipt.attempt, attempt);
        assert_eq!(receipt.delivery_generation, view.delivery_generation);
        assert_eq!(receipt.database_instance, ready.database_instance);
        assert_eq!(receipt.offer_engine_epoch, ready.engine_epoch);
        assert_eq!(receipt.registration_id, ready.registration_id);
        assert_eq!(receipt.runner_boot_id, ready.boot.runner_id);
        assert_eq!(Some(&receipt.offer_digest), view.offer_digest.as_ref());
        assert!(receipt.journal_sequence > 0);
        assert_eq!(receipt.reason, DeniedReason::NativeRuntimeAdmissionUnavailable);
        assert!(receipt.native_identity.is_none());
        assert!(serde_json::to_vec(view).unwrap().len() <= 4096);
        assert!(serde_json::to_vec(&receipt).unwrap().len() <= 2048);
        let after_status = runner.status().await;
        assert_eq!(after_status["operation_count"], 1);
        assert_eq!(after_status["generation"], receipt.controller_generation);
        assert_eq!(after_status["identity"], before_status["identity"]);
        assert_eq!(after_status["native_admission"], "unavailable");
        assert!(after_status["native_identity"].is_null());
        let after_reserved = client.request(wire("delivery-reservation-after", 0, Command::CoordinationState {
            plan_id: plan.id.clone(),
        })).await.unwrap();
        let Outcome::Coordination { state } = after_reserved.outcome else { panic!("actual stored plan required"); };
        assert_eq!(*state, reserved);
        let after_leases = client.request(wire("delivery-lease-after", 0, Command::WorktreeLeases { task_id: scope.task_id.clone() })).await.unwrap();
        assert_eq!(after_leases.outcome, Outcome::Worktrees { leases });
        let after_snapshot = client.request(wire("delivery-snapshot-after", 0, Command::Snapshot)).await.unwrap().outcome;
        assert_eq!(after_snapshot, before_snapshot);
        assert_eq!(std::fs::read(repository.join("tracked.rs")).unwrap(), tracked);
    }).await;
}
