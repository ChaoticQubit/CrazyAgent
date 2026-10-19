use crazyagent::config::{EffectiveConfig, resolve_config};
use crazyagent::coordination::{OwnerAction, PlanAuthority, Role, WorkItem, WorkPlan};
use crazyagent::engine::Client;
use crazyagent::engine::bootstrap::EngineService;
use crazyagent::engine::owner::{
    OwnerClient, OwnerClientKind, OwnerData, OwnerMutation, OwnerOperation, OwnerRead,
    OwnerReadReply, OwnerRequest, OwnerResponse, OwnerResult,
};
use crazyagent::protocol::*;
use crazyagent::routing::{ResourceBudget, RouteRequest};
use crazyagent::verification::{
    BaselineDraft, BaselineOrigin, CandidateSnapshot, CheckKind, CheckSpec, Risk,
};
use crazyagent::worker_dispatch::{AttemptRef, DeniedReason, DispatchState, DispatchView};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs;
use std::io::Read;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command as ProcessCommand, ExitStatus, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};
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
projects = ["project-z"]
roles = ["worker"]
fallback_allowed = false
"#;

struct OwnedProcess {
    child: Child,
    stdout: PathBuf,
    stderr: PathBuf,
}

impl OwnedProcess {
    fn start(command: &mut ProcessCommand, root: &Path, label: &str) -> Self {
        let stdout = root.join(format!("{label}.stdout"));
        let stderr = root.join(format!("{label}.stderr"));
        let log = |path: &Path| fs::OpenOptions::new()
            .create_new(true).write(true).mode(0o600).open(path).unwrap();
        let child = command.stdin(Stdio::null())
            .stdout(log(&stdout)).stderr(log(&stderr)).spawn().unwrap();
        eprintln!("tui-dispatch owned {label} pid={}", child.id());
        Self { child, stdout, stderr }
    }

    fn bounded(&self) {
        for path in [&self.stdout, &self.stderr] {
            assert!(fs::metadata(path).unwrap().len() <= 1_048_576,
                "owned stream exceeded one MiB: {path:?}");
        }
    }

    async fn wait(&mut self, seconds: u64) -> ExitStatus {
        let deadline = Instant::now() + Duration::from_secs(seconds);
        loop {
            self.bounded();
            if let Some(status) = self.child.try_wait().unwrap() {
                return status;
            }
            assert!(Instant::now() < deadline, "owned child deadline: {}", self.child.id());
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    fn stop(&mut self) -> bool {
        if self.child.try_wait().unwrap().is_some() {
            return true;
        }
        let pid = rustix::process::Pid::from_raw(i32::try_from(self.child.id()).unwrap()).unwrap();
        let _ = rustix::process::kill_process(pid, rustix::process::Signal::TERM);
        let deadline = Instant::now() + Duration::from_secs(8);
        while Instant::now() < deadline {
            if self.child.try_wait().unwrap().is_some() {
                eprintln!("tui-dispatch owned child reaped pid={}", self.child.id());
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let _ = self.child.kill();
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            if self.child.try_wait().unwrap().is_some() {
                eprintln!("tui-dispatch forced child reap pid={}", self.child.id());
                return false;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        eprintln!("tui-dispatch child NOT reaped pid={}", self.child.id());
        false
    }
}

impl Drop for OwnedProcess {
    fn drop(&mut self) {
        let graceful = self.stop();
        assert!(graceful || std::thread::panicking(), "owned cleanup required force or failed");
    }
}

fn bounded_bytes(path: &Path) -> Vec<u8> {
    let mut bytes = Vec::new();
    fs::File::open(path).unwrap().take(1_048_577).read_to_end(&mut bytes).unwrap();
    assert!(bytes.len() <= 1_048_576, "retained evidence exceeded one MiB: {path:?}");
    bytes
}

async fn git(root: &Path, label: &str, args: &[&str]) -> String {
    let mut command = ProcessCommand::new("/usr/bin/git");
    command.env_clear().env("PATH", "/usr/bin:/bin")
        .env("GIT_CONFIG_NOSYSTEM", "1").env("GIT_CONFIG_GLOBAL", "/dev/null")
        .args(["-c", "core.hooksPath=/dev/null", "-c", "commit.gpgsign=false", "-C"])
        .arg(root.join("repository")).args(args);
    let mut child = OwnedProcess::start(&mut command, root, label);
    assert!(child.wait(5).await.success(), "private Git fixture failed: {args:?}");
    String::from_utf8(bounded_bytes(&child.stdout)).unwrap()
}

async fn acceptance(root: &Path) -> (CandidateSnapshot, BaselineDraft) {
    fs::create_dir(root.join("repository")).unwrap();
    git(root, "git-init", &["init", "-b", "main"]).await;
    let tracked = b"fn sum(a: i32, b: i32) -> i32 { a + b }\n";
    fs::write(root.join("repository/tracked.rs"), tracked).unwrap();
    git(root, "git-add", &["add", "tracked.rs"]).await;
    git(root, "git-commit", &["-c", "user.name=Dispatch Test", "-c",
        "user.email=dispatch@invalid", "commit", "-m", "actual baseline"]).await;
    let head = git(root, "git-head", &["rev-parse", "HEAD"]).await;
    let snapshot = CandidateSnapshot {
        files: BTreeMap::from([("tracked.rs".into(), tracked.to_vec())]),
        head_digest: format!("{:x}", Sha256::digest(head.trim().as_bytes())),
        target_digest: format!("{:x}", Sha256::digest(tracked)),
    };
    let draft = BaselineDraft {
        version: 1, project_id: ProjectId::new("project-z").unwrap(),
        origin: BaselineOrigin::Imported, requirements: "Preserve sum and acceptance".into(),
        checks: vec![
            CheckSpec { id: RequestId::new("sum").unwrap(), kind: CheckKind::Code,
                minimum_risk: Risk::Low, content: b"assert_eq!(sum(2, 3), 5)".to_vec() },
            CheckSpec { id: RequestId::new("integrity").unwrap(), kind: CheckKind::TestIntegrity,
                minimum_risk: Risk::Low, content: b"reject changed expected sum".to_vec() },
        ],
        rules: vec![], retry_limit: 0,
    };
    (snapshot, draft)
}

struct Setup {
    client: Client,
    owner: OwnerClient,
    revision: Cursor,
    database: String,
}

impl Setup {
    async fn connect(root: &Path) -> Self {
        let (client, owner, identity) = Client::new(root.join("e.sock"), Duration::from_secs(2))
            .unwrap().with_owner(OwnerClientKind::Tui).await.unwrap();
        Self { client, owner, revision: Cursor::new(0),
            database: identity.database_instance().to_owned() }
    }

    async fn command(&mut self, id: &str, command: Command) {
        let reply = self.client.request(WireRequest {
            protocol_version: PROTOCOL_VERSION,
            client_id: ClientId::new("tui-dispatch-fixture").unwrap(),
            request_id: RequestId::new(id).unwrap(),
            expected_revision: self.revision.clone(), command,
        }).await.unwrap();
        assert_eq!(reply.outcome, Outcome::Applied, "actual setup command {id}");
        assert!(reply.revision.value() > self.revision.value());
        self.revision = reply.revision;
    }

    async fn mutate(&mut self, id: &str, mutation: OwnerMutation) -> OwnerResult {
        let reply = self.owner.request(OwnerRequest {
            protocol_version: PROTOCOL_VERSION, request_id: RequestId::new(id).unwrap(),
            expected_revision: self.revision.clone(), operation: OwnerOperation::Apply { mutation },
        }).await.unwrap();
        let OwnerResponse::Applied { operation } = reply.response else {
            panic!("actual owner mutation {id} failed: {reply:?}");
        };
        self.revision = operation.current_revision;
        operation.result
    }

    async fn read(&self, id: &str, query: OwnerRead) -> OwnerReadReply {
        let reply = self.owner.request(OwnerRequest {
            protocol_version: PROTOCOL_VERSION, request_id: RequestId::new(id).unwrap(),
            expected_revision: self.revision.clone(), operation: OwnerOperation::Read { query },
        }).await.unwrap();
        let OwnerResponse::Read { state } = reply.response else {
            panic!("actual owner read {id} failed: {reply:?}");
        };
        state
    }
}

fn item(id: &str, session: &str, scope: &ExecutionContext) -> WorkItem {
    WorkItem {
        id: TaskId::new(id).unwrap(), session_id: SessionId::new(session).unwrap(),
        role: Role::Worker, native_descendant: None,
        pool_ref: Some(CredentialRef::new("worker-pool").unwrap()),
        route: RouteRequest {
            preferred_target: None, required_target: Some(CredentialRef::new("profile-a").unwrap()),
            required_capabilities: vec!["coding".into()],
            account_ref: CredentialRef::new("account-a").unwrap(),
            budget_ref: scope.budget_ref.clone(), resources: ResourceBudget::unit(),
        },
    }
}

async fn provision(root: &Path, config: &EffectiveConfig, setup: &mut Setup) -> WorkPlan {
    let (snapshot, draft) = acceptance(root).await;
    let repository = root.join("repository").to_str().unwrap().to_owned();
    for (id, name, repositories) in [
        ("project-a", "Other project", vec![]),
        ("project-z", "Dispatch target", vec![repository.clone()]),
    ] {
        setup.command(id, Command::CreateProject {
            project_id: ProjectId::new(id).unwrap(), spec: ProjectSpec { name: name.into(), repositories },
        }).await;
    }
    setup.command("admit", Command::AdmitRepository {
        project_id: draft.project_id.clone(), repository,
    }).await;
    let baseline_id = draft.identity().unwrap();
    assert_eq!(setup.mutate("propose-baseline", OwnerMutation::ProposeBaseline { draft }).await,
        OwnerResult::Applied);
    assert_eq!(setup.mutate("approve-baseline", OwnerMutation::ApproveBaseline {
        baseline_id: baseline_id.clone(),
    }).await, OwnerResult::Applied);
    let mut scope = ExecutionContext {
        project_id: ProjectId::new("project-z").unwrap(), task_id: TaskId::new("task-a").unwrap(),
        config_id: config.id.clone(), snapshot_id: snapshot.identity().unwrap(), baseline_id,
        allowed_destinations: config.settings.allowed_destinations.clone(),
        budget_ref: CredentialRef::new("budget-a").unwrap(),
    };
    setup.command("pin-other", Command::PinTask { context: scope.clone() }).await;
    scope.task_id = TaskId::new("task-z").unwrap();
    setup.command("pin-target", Command::PinTask { context: scope.clone() }).await;
    WorkPlan {
        id: PlanId::new("dispatch-plan").unwrap(), config_id: config.id.clone(),
        tasks: vec![item("worker-a", "session-a", &scope), item("worker-z", "session-z", &scope)],
        scope, dependencies: vec![], authority: PlanAuthority::Human,
    }
}

async fn reserve(root: &Path, setup: &mut Setup, plan: &WorkPlan) -> AttemptRef {
    setup.command("propose-plan", Command::ProposePlan { plan: plan.clone() }).await;
    for (id, action) in [
        ("approve-plan", OwnerAction::ApprovePlan { plan_id: plan.id.clone() }),
        ("reserve-target", OwnerAction::Reserve {
            plan_id: plan.id.clone(), item_id: TaskId::new("worker-z").unwrap(),
        }),
    ] {
        assert_eq!(setup.mutate(id, OwnerMutation::Coordination { action }).await, OwnerResult::Applied);
    }
    let stored = setup.read("reserved-plan", OwnerRead::CoordinationState {
        plan_id: plan.id.clone(),
    }).await;
    let OwnerData::Domain { outcome } = stored.data else {
        panic!("actual reserved plan required");
    };
    let Outcome::Coordination { state } = *outcome else {
        panic!("actual coordination outcome required");
    };
    assert_eq!(&state.plan, plan);
    assert_eq!(state.reservations.len(), 1);
    let target = state.plan.tasks.iter().find(|item| item.id.as_str() == "worker-z").unwrap();
    assert_eq!(state.plan.tasks.iter().position(|item| item.id == target.id), Some(1));
    assert_ne!(target.id, plan.scope.task_id);
    assert!(!state.reservations.contains_key(&TaskId::new("worker-a").unwrap()));
    let reservation = &state.reservations[&target.id];
    let binding = reservation.binding.as_ref().unwrap();
    assert_eq!(binding.task_id, target.id);
    assert_eq!(binding.connection.id.as_str(), "connection-a");
    let attempt = AttemptRef { plan_id: plan.id.clone(), item_id: target.id.clone(),
        reservation_cursor: reservation.cursor.clone() };
    setup.command("allocate-target", Command::AllocateWorktree {
        task_id: plan.scope.task_id.clone(), repository: root.join("repository").to_str().unwrap().into(),
        owner_session: binding.session_id.clone(), native_descendant: None,
    }).await;
    attempt
}

async fn deliver(
    root: &Path, service: &Arc<Mutex<EngineService>>, setup: &mut Setup, attempt: &AttemptRef,
) -> (OwnedProcess, OwnerReadReply, DispatchView, PathBuf) {
    let prepared = service.lock().await.prepare_runner(attempt).await.unwrap();
    let runner_root = prepared.root().to_owned();
    let mut command = ProcessCommand::new(env!("CARGO_BIN_EXE_crazyagent-runner"));
    command.arg("serve").arg(&runner_root);
    let mut runner = OwnedProcess::start(&mut command, root, "runner");
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        runner.bounded();
        assert!(runner.child.try_wait().unwrap().is_none(), "runner exited before attachment");
        if let Ok(ready) = service.lock().await.attach_runner(&prepared).await {
            assert_eq!(&ready.attempt, attempt);
            assert_eq!(ready.database_instance, setup.database);
            break;
        }
        assert!(Instant::now() < deadline, "actual runner attachment deadline");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let result = setup.mutate("prepare-dispatch", OwnerMutation::PrepareWorkerDispatch {
        attempt: attempt.clone(), repository_slot: 0,
    }).await;
    let OwnerResult::DispatchPrepared { dispatch_id } = result else {
        panic!("actual dispatch preparation receipt required: {result:?}");
    };
    let read = setup.read("denied-dispatch", OwnerRead::WorkerDispatch { dispatch_id }).await;
    let OwnerData::WorkerDispatch { dispatch: Some(view) } = &read.data else {
        panic!("actual retained dispatch required: {read:?}");
    };
    assert_eq!(view.state, DispatchState::DeliveredDenied);
    assert_eq!(&view.attempt, attempt);
    let receipt = view.receipt.as_ref().unwrap();
    assert_eq!(receipt.reason, DeniedReason::NativeRuntimeAdmissionUnavailable);
    assert_eq!(receipt.dispatch_id, view.dispatch_id);
    assert_eq!(receipt.attempt, *attempt);
    assert_eq!(receipt.database_instance, setup.database);
    assert_eq!(receipt.delivery_generation, view.delivery_generation);
    assert_eq!(Some(&receipt.offer_digest), view.offer_digest.as_ref());
    assert!(receipt.controller_generation > 0 && receipt.journal_sequence > 0);
    assert!(receipt.native_identity.is_none());
    let view = view.as_ref().clone();
    (runner, read, view, runner_root)
}

async fn inspect(root: &Path, read: &OwnerReadReply, view: &DispatchView) {
    let expected = root.join("expected.json");
    fs::write(&expected, serde_json::to_vec(&json!({
        "project_id": "project-z", "project_name": "Dispatch target", "task_id": "task-z",
        "other_project": "project-a", "other_task": "task-a", "plan_id": "dispatch-plan",
        "read": read, "dispatch": view,
    })).unwrap()).unwrap();
    let mut command = ProcessCommand::new("python3");
    command.env_remove("PYTHONOPTIMIZE").env("PYTHONDONTWRITEBYTECODE", "1")
        .arg(concat!(env!("CARGO_MANIFEST_DIR"), "/../../scripts/qualification/tui_dispatch.py"))
        .arg(env!("CARGO_BIN_EXE_crazyagent")).arg(root.join("e.sock")).arg(&expected);
    let mut driver = OwnedProcess::start(&mut command, root, "pty-driver");
    let status = driver.wait(40).await;
    assert!(status.success(), "actual dispatch PTY failed: stdout={} stderr={}",
        String::from_utf8_lossy(&bounded_bytes(&driver.stdout)),
        String::from_utf8_lossy(&bounded_bytes(&driver.stderr)));
    let cleanup: serde_json::Value = serde_json::from_slice(&bounded_bytes(&root.join("cleanup.json"))).unwrap();
    assert_eq!(cleanup["tui_status"], 0);
    assert_eq!(cleanup["supervisor_status"], 0);
    assert_eq!(cleanup["terminal_restored"], true);
    assert!(cleanup["pid"].as_u64().unwrap() > 0);
    assert!(cleanup["supervisor_pid"].as_u64().unwrap() > 0);
    assert!(!bounded_bytes(&root.join("tui.pty")).is_empty());
}

async fn exercise(root: PathBuf, config: EffectiveConfig, service: Arc<Mutex<EngineService>>) {
    let mut setup = Setup::connect(&root).await;
    let plan = provision(&root, &config, &mut setup).await;
    let attempt = reserve(&root, &mut setup, &plan).await;
    let (mut runner, before, view, runner_root) = deliver(&root, &service, &mut setup, &attempt).await;
    let journal = bounded_bytes(&runner_root.join("control.log"));
    let repository = bounded_bytes(&root.join("repository/tracked.rs"));
    inspect(&root, &before, &view).await;
    let after = setup.read("after-pty", OwnerRead::WorkerDispatch {
        dispatch_id: view.dispatch_id.clone(),
    }).await;
    assert_eq!(after, before, "TUI inspection/reconnect must not mutate dispatch or owner revision");
    assert_eq!(bounded_bytes(&runner_root.join("control.log")), journal,
        "TUI readback must not deliver, reconcile, cancel or launch a worker");
    assert_eq!(bounded_bytes(&root.join("repository/tracked.rs")), repository);
    runner.bounded();
    assert!(runner.stop(), "actual runner cleanup must be graceful and reaped");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn real_tui_inspects_exact_reserved_denial_after_task_selection_and_owner_reconnect() {
    let root = tempfile::Builder::new().prefix("ca-tui-d-").tempdir_in("/private/tmp").unwrap();
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let root = root.keep();
    eprintln!("tui-dispatch retained evidence root={}", root.display());
    fs::write(root.join("global.toml"), CATALOG).unwrap();
    let config = resolve_config(&root.join("global.toml"), &root.join("project.toml")).unwrap();
    let (_stop, stopped) = watch::channel(false);
    let service = Arc::new(Mutex::new(tokio::time::timeout(Duration::from_secs(5),
        EngineService::start(&root, "e.sock", &config, &stopped)).await.unwrap().unwrap()));
    let mut task = tokio::spawn(exercise(root.clone(), config, service.clone()));
    let (outcome, timed_out) = match tokio::time::timeout(Duration::from_secs(65), &mut task).await {
        Ok(outcome) => (outcome, false),
        Err(_) => { task.abort(); (task.await, true) }
    };
    let cleanup = tokio::time::timeout(Duration::from_secs(5), async {
        service.lock().await.shutdown().await
    }).await;
    eprintln!("tui-dispatch engine_closed={} timed_out={timed_out} root={}",
        matches!(&cleanup, Ok(Ok(()))), root.display());
    assert!(matches!(&cleanup, Ok(Ok(()))), "actual EngineService cleanup failed: {cleanup:?}");
    assert!(!timed_out, "dispatch inspection case exceeded 65 seconds");
    if let Err(error) = outcome {
        if error.is_panic() { std::panic::resume_unwind(error.into_panic()); }
        panic!("actual dispatch PTY task cancelled: {error}");
    }
}
