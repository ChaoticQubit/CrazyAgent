use super::*;
use crate::engine::bootstrap::{EngineService, runner_binding};
use crate::engine::native_auth_host::RegisteredNativeAuthHost;
use crate::engine::owner::native_auth::Command;
use crate::storage::{NativeAuthRetirementCheckpoint, NativeAuthWork};
use rusqlite::{Connection, OpenFlags};
use sha2::{Digest, Sha256};
use std::future::Future;
use std::os::unix::fs::PermissionsExt;
use std::process::{Child, Command as ProcessCommand, Stdio};
use std::task::{Context, Poll, Waker};

const CATALOG: &str = r#"
timeout_ms = 1000
[model_access]
accounts = ["account-a"]
[[model_access.connections]]
id = "connection-a"
provider = "provider-a"
harness = "codex"
protocol = "native"
account_ref = "account-a"
endpoint = { kind = "native" }
model = { id = "model-a", author = "author-a" }
auth = { method = "native_managed", profile_ref = "profile-a" }
[[model_access.pools]]
id = "pool-a"
members = ["connection-a"]
projects = ["project-a"]
roles = ["leader"]
fallback_allowed = false
"#;

type FenceRow = (String, String, i64, String, String, String, String, i64);

#[derive(Debug, PartialEq, Eq)]
struct Snapshot {
    facts: Vec<NativeAuthFact>,
    fences: Vec<FenceRow>,
}

struct LifetimeCase {
    root: PathBuf,
    config: crate::config::EffectiveConfig,
    source_hash: String,
    host: OwnerRuntimeHandle,
    service: tokio::sync::Mutex<Option<EngineService>>,
}

struct TestChild(Child);

impl TestChild {
    async fn kill_and_reap(&mut self) {
        if self.0.try_wait().unwrap().is_some() {
            return;
        }
        self.0.kill().unwrap();
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if self.0.try_wait().unwrap().is_some() {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }
}

impl Drop for TestChild {
    fn drop(&mut self) {
        if matches!(self.0.try_wait(), Ok(Some(_))) {
            return;
        }
        let _ = self.0.kill();
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while std::time::Instant::now() < deadline {
            if matches!(self.0.try_wait(), Ok(Some(_))) {
                return;
            }
            std::thread::yield_now();
        }
        eprintln!(
            "test direct-child cleanup uncertain for PID {}",
            self.0.id()
        );
    }
}

fn request(value: &str) -> RequestId {
    RequestId::new(value).unwrap()
}

fn executable() -> PathBuf {
    let path = std::env::var_os("CRAZYAGENT_TEST_NATIVE_AUTH_HOST")
        .map(PathBuf::from)
        .expect("root must supply the source-matched product custodian executable path");
    assert!(path.is_absolute());
    path
}

fn snapshot(path: &Path) -> Snapshot {
    let db = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )
    .unwrap();
    db.busy_timeout(Duration::from_secs(1)).unwrap();
    db.execute_batch("BEGIN").unwrap();
    let facts = db
        .prepare("SELECT fact FROM native_auth_operations ORDER BY id")
        .unwrap()
        .query_map([], |row| row.get::<_, Vec<u8>>(0))
        .unwrap()
        .map(|row| crate::engine::owner::native_auth::decode_fact(&row.unwrap()).unwrap())
        .collect();
    let fences = db
        .prepare(
            "SELECT profile_key,operation_id,generation,db_instance,engine_epoch,custody_id,
             owner_id,owner_epoch FROM native_auth_fences ORDER BY profile_key",
        )
        .unwrap()
        .query_map([], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
                row.get(6)?,
                row.get(7)?,
            ))
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    Snapshot { facts, fences }
}

async fn lifetime_case<F, T>(case: F)
where
    F: FnOnce(Arc<LifetimeCase>) -> T,
    T: Future<Output = ()> + Send + 'static,
{
    let root = tempfile::Builder::new()
        .prefix("ca-dlf-")
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let global = root.path().join("g.toml");
    std::fs::write(&global, CATALOG).unwrap();
    std::fs::set_permissions(&global, std::fs::Permissions::from_mode(0o600)).unwrap();
    let project = root.path().join("p.toml");
    let config = crate::config::resolve_config(&global, &project).unwrap();
    let (_stop, stopped) = tokio::sync::watch::channel(false);
    let service = EngineService::start_with_settings_sources(
        root.path(),
        "e.sock",
        &global,
        &project,
        &stopped,
    )
    .await
    .unwrap();
    let handle = service.owner_runtime().unwrap();
    let client = OwnerClient::attach(
        &handle.authority.root.path,
        OwnerClientKind::Tui,
        Duration::from_secs(2),
    )
    .unwrap();
    let response = client
        .request(OwnerRequest {
            protocol_version: PROTOCOL_VERSION,
            request_id: request("source-for-dispatch-lifetime"),
            expected_revision: Cursor::new(0),
            operation: OwnerOperation::Read {
                query: OwnerRead::ModelAccessRecord {
                    collection: ModelAccessCollection::Connections,
                    id: CredentialRef::new("connection-a").unwrap(),
                },
            },
        })
        .await
        .unwrap();
    let OwnerResponse::Read { state } = response.response else {
        panic!("actual owner did not return model access source")
    };
    let OwnerData::ModelAccessRecord { source_hash, .. } = state.data else {
        panic!("actual owner did not return connection source")
    };
    drop(client);
    let context = Arc::new(LifetimeCase {
        root: root.path().to_path_buf(),
        config,
        source_hash,
        host: OwnerRuntimeHandle {
            engine: handle.engine.clone(),
            authority: handle.authority.clone(),
        },
        service: tokio::sync::Mutex::new(Some(service)),
    });
    let authority = Arc::downgrade(&context.host.authority);
    let mut task = tokio::spawn(case(context.clone()));
    let (outcome, timed_out) = match tokio::time::timeout(Duration::from_secs(30), &mut task).await
    {
        Ok(outcome) => (outcome, false),
        Err(_) => {
            task.abort();
            (task.await, true)
        }
    };
    let cleanup = tokio::time::timeout(Duration::from_secs(8), async {
        context.host.invalidate_native_auth_dispatch()?;
        context.host.drain_native_auth_dispatch().await?;
        if let Some(mut service) = context.service.lock().await.take() {
            service.shutdown().await?;
        }
        Ok::<(), EngineError>(())
    })
    .await;
    drop(context);
    if timed_out || outcome.is_err() || !matches!(&cleanup, Ok(Ok(()))) {
        eprintln!(
            "dispatch lifetime evidence retained at {}",
            root.keep().display()
        );
    }
    assert!(matches!(&cleanup, Ok(Ok(()))), "cleanup: {cleanup:?}");
    assert!(
        authority.upgrade().is_none(),
        "dispatch retained a strong Authority cycle"
    );
    assert!(!timed_out, "dispatch lifetime case exceeded 30 seconds");
    if let Err(error) = outcome {
        if error.is_panic() {
            std::panic::resume_unwind(error.into_panic());
        }
        panic!("dispatch lifetime task: {error}");
    }
}

impl LifetimeCase {
    fn client(&self, kind: OwnerClientKind) -> OwnerClient {
        OwnerClient::attach(&self.host.authority.root.path, kind, Duration::from_secs(2)).unwrap()
    }

    fn admission(&self) -> Admission {
        let client = self.client(OwnerClientKind::Tui);
        let mut packet = [0; 37];
        packet[..4].copy_from_slice(b"CAO1");
        packet[5..].copy_from_slice(&client.token);
        self.host.authority.authenticate(&packet).unwrap()
    }

    fn start(&self, id: &str) -> NativeAuthStart {
        NativeAuthStart {
            request_id: request(id),
            scope: NativeAuthScope {
                expected_database_instance: self.host.identity().db_instance,
                connection: CredentialRef::new("connection-a").unwrap(),
                expected_config_id: self.config.id.clone(),
                expected_source_hash: self.source_hash.clone(),
            },
            method: NativeAuthMethod::CodexBrowser,
        }
    }

    fn query(&self, fact: &NativeAuthFact, id: &str) -> NativeAuthQuery {
        NativeAuthQuery {
            request_id: request(id),
            scope: self.start("query-scope").scope,
            operation_id: fact.operation_id.clone(),
        }
    }

    fn register(&self) {
        self.host
            .register_native_auth_host(RegisteredNativeAuthHost::capture(&executable()).unwrap())
            .unwrap();
    }

    async fn work(&self, id: &str) -> (NativeAuthFact, NativeAuthWork) {
        let begun = self
            .host
            .engine
            .native_auth_job(|reply| {
                Job::NativeAuth(self.admission(), Command::Start(self.start(id)), reply)
            })
            .await
            .unwrap();
        assert_eq!(begun.reply.fact.phase, NativeAuthPhase::Starting);
        assert!(begun.reply.account.is_none());
        assert!(begun.reply.challenge.is_none());
        (begun.reply.fact, begun.work.unwrap())
    }

    fn snapshot(&self) -> Snapshot {
        snapshot(&self.root.join("engine.db"))
    }

    fn retain(&self, work: NativeAuthWork) {
        self.host
            .retain_native_auth_dispatch(self.admission(), work)
            .unwrap_or_else(|(error, _work)| panic!("dispatch rejected genuine work: {error}"));
    }

    fn retained_pid(&self) -> rustix::process::Pid {
        let slot = self.host.authority.native_auth.lock().unwrap();
        let slot = slot
            .as_ref()
            .expect("actual dispatch slot must remain owned");
        assert!(
            slot.dispatch_task.is_some(),
            "actual dispatcher JoinHandle is absent"
        );
        let child = slot
            .child
            .as_ref()
            .expect("actual custodian Child is absent");
        rustix::process::Pid::from_raw(i32::try_from(child.id()).unwrap()).unwrap()
    }

    fn probe(&self) -> File {
        File::from(
            rustix::fs::open(
                self.root
                    .join("native-profiles")
                    .join(format!("{:x}", Sha256::digest(b"profile-a")))
                    .join("identity.json"),
                OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .unwrap(),
        )
    }

    async fn held(&self, id: &str) -> (NativeAuthFact, NativeAuthRetirementCheckpoint) {
        self.register();
        let (fact, work) = self.work(id).await;
        assert_eq!(self.snapshot().fences.len(), 1);
        let mut checkpoint =
            NativeAuthRetirementCheckpoint::arm(&fact, Duration::from_secs(2)).unwrap();
        self.retain(work);
        tokio::time::timeout(Duration::from_secs(10), checkpoint.reached())
            .await
            .unwrap()
            .unwrap();
        rustix::process::test_kill_process(self.retained_pid()).unwrap();
        assert!(
            rustix::fs::flock(
                self.probe(),
                rustix::fs::FlockOperation::NonBlockingLockExclusive
            )
            .is_err()
        );
        assert_eq!(self.snapshot().fences.len(), 1);
        (fact, checkpoint)
    }

    async fn drain(&self) {
        tokio::time::timeout(
            Duration::from_secs(8),
            self.host.drain_native_auth_dispatch(),
        )
        .await
        .unwrap()
        .unwrap();
    }

    async fn committed(&self, fact: &NativeAuthFact) -> NativeAuthFact {
        let reply = self
            .client(OwnerClientKind::Tui)
            .read_native_auth(self.query(fact, "observe-real-retirement-commit"))
            .await
            .unwrap();
        assert_eq!(reply.fact.operation_id, fact.operation_id);
        assert_eq!(reply.fact.generation, fact.generation);
        assert_eq!(reply.fact.phase, NativeAuthPhase::Unavailable);
        assert_eq!(
            reply.fact.code.as_deref(),
            Some("native_auth_host_unavailable")
        );
        assert!(reply.account.is_none());
        assert!(reply.challenge.is_none());
        assert!(self.snapshot().fences.is_empty());
        reply.fact
    }

    fn interrupted(&self, fact: &NativeAuthFact) {
        let after = self.snapshot();
        assert!(after.fences.is_empty(), "{after:?}");
        assert_eq!(after.facts.len(), 1);
        let settled = &after.facts[0];
        assert_eq!(
            (&settled.operation_id, settled.generation),
            (&fact.operation_id, fact.generation)
        );
        assert_eq!(
            (settled.phase, settled.code.as_deref()),
            (NativeAuthPhase::Failed, Some("native_auth_interrupted"))
        );
    }

    async fn revoke(&self, kind: OwnerClientKind, id: &str) {
        self.host
            .control(OwnerControl {
                operation_id: request(id),
                target: kind,
                expected_epoch: self.host.epoch(kind).unwrap(),
                decision: OwnerDecision::Revoke,
            })
            .await
            .unwrap();
    }

    async fn wait_invalidated(&self) {
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let live = self
                    .host
                    .authority
                    .native_auth
                    .lock()
                    .unwrap()
                    .as_ref()
                    .is_some_and(|slot| slot.session_is_live());
                if !live {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }
}

fn assert_reaped(pid: rustix::process::Pid) {
    let observed = rustix::process::waitpid(Some(pid), rustix::process::WaitOptions::NOHANG);
    assert!(
        matches!(&observed, Err(rustix::io::Errno::CHILD)),
        "retained direct child was not already reaped by its actual owner: {observed:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_auth_dispatch_lifetime_unregistered_embedded_host_is_never_dispatched() {
    lifetime_case(|case| async move {
        let reply = case
            .client(OwnerClientKind::Tui)
            .start_native_auth(case.start("unregistered-embedded-host"))
            .await
            .unwrap();
        assert_eq!(reply.fact.phase, NativeAuthPhase::Unavailable);
        assert_eq!(
            reply.fact.code.as_deref(),
            Some("native_auth_host_unavailable")
        );
        assert!(reply.account.is_none());
        assert!(reply.challenge.is_none());
        assert!(case.snapshot().fences.is_empty());
        assert!(case.host.authority.native_auth.lock().unwrap().is_none());
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_auth_dispatch_lifetime_runtime_dir_joins_actual_generation_and_engine() {
    lifetime_case(|case| async move {
        let root = case.host.native_auth_owner_runtime_dir().unwrap();
        assert_eq!(root, case.root);
        assert_ne!(root, case.host.authority.root.path);
        let (generation, identity, binding) = runner_binding(&root).unwrap();
        assert_eq!(generation, case.host.authority.root.path);
        assert_eq!(
            identity.database_instance(),
            case.host.identity().db_instance
        );
        assert_eq!(identity.engine_epoch(), case.host.identity().engine_epoch);
        binding.verify().unwrap();
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_auth_dispatch_lifetime_runtime_dir_rejects_replaced_generation() {
    lifetime_case(|case| async move {
        let generation = case.host.authority.root.path.clone();
        let moved = case.root.join("moved-owner-generation");
        std::fs::rename(&generation, &moved).unwrap();
        std::fs::create_dir(&generation).unwrap();
        std::fs::set_permissions(&generation, std::fs::Permissions::from_mode(0o700)).unwrap();
        let observed = case.host.native_auth_owner_runtime_dir();
        std::fs::remove_dir(&generation).unwrap();
        std::fs::rename(&moved, &generation).unwrap();
        assert!(observed.is_err());
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_auth_dispatch_lifetime_registration_is_single_and_frozen() {
    lifetime_case(|case| async move {
        case.register();
        assert!(
            case.host
                .register_native_auth_host(
                    RegisteredNativeAuthHost::capture(&executable()).unwrap(),
                )
                .is_err()
        );
        assert!(case.snapshot().facts.is_empty());
        assert!(case.host.authority.native_auth.lock().unwrap().is_none());
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_auth_dispatch_lifetime_registered_owner_start_replays_one_committed_operation() {
    lifetime_case(|case| async move {
        case.register();
        let client = case.client(OwnerClientKind::Tui);
        let begun = client
            .start_native_auth(case.start("registered-real-owner-start"))
            .await
            .unwrap();
        assert_eq!(begun.fact.phase, NativeAuthPhase::Starting);
        assert!(begun.account.is_none());
        assert!(begun.challenge.is_none());
        case.drain().await;
        let terminal = case.committed(&begun.fact).await;
        let replay = client
            .start_native_auth(case.start("registered-real-owner-start"))
            .await
            .unwrap();
        assert_eq!(replay.fact, terminal);
        assert_eq!(case.snapshot().facts.len(), 1);
        assert!(case.host.authority.native_auth.lock().unwrap().is_none());
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_auth_dispatch_lifetime_actual_child_and_task_survive_retirement_commit() {
    lifetime_case(|case| async move {
        let (fact, mut checkpoint) = case.held("child-through-commit").await;
        let pid = case.retained_pid();
        rustix::process::kill_process(pid, rustix::process::Signal::STOP).unwrap();
        checkpoint.release().unwrap();
        case.committed(&fact).await;
        assert_eq!(case.retained_pid(), pid);
        rustix::process::test_kill_process(pid).unwrap();
        rustix::process::kill_process(pid, rustix::process::Signal::CONT).unwrap();
        case.drain().await;
        assert_reaped(pid);
        rustix::fs::flock(
            case.probe(),
            rustix::fs::FlockOperation::NonBlockingLockExclusive,
        )
        .unwrap();
        assert!(case.host.authority.native_auth.lock().unwrap().is_none());
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_auth_dispatch_lifetime_dropped_request_keeps_actual_child_and_exact_replay() {
    lifetime_case(|case| async move {
        case.register();
        let (fact, work) = case.work("dropped-request-retained-work").await;
        let mut checkpoint =
            NativeAuthRetirementCheckpoint::arm(&fact, Duration::from_secs(2)).unwrap();
        case.retain(work);
        tokio::time::timeout(Duration::from_secs(10), checkpoint.reached())
            .await
            .unwrap()
            .unwrap();
        let pid = case.retained_pid();
        {
            let requester = OwnerRuntimeHandle {
                engine: case.host.engine.clone(),
                authority: case.host.authority.clone(),
            };
            let client = case.client(OwnerClientKind::Tui);
            let pending = client.start_native_auth(case.start("dropped-request-retained-work"));
            tokio::pin!(pending);
            let mut context = Context::from_waker(Waker::noop());
            let observed = pending.as_mut().poll(&mut context);
            assert!(
                matches!(&observed, Poll::Pending),
                "request poll: {observed:?}"
            );
            drop(requester);
        }
        assert_eq!(case.retained_pid(), pid);
        rustix::process::test_kill_process(pid).unwrap();
        assert_eq!(case.snapshot().fences.len(), 1);
        checkpoint.release().unwrap();
        case.drain().await;
        assert_reaped(pid);
        let terminal = case.committed(&fact).await;
        let replay = case
            .client(OwnerClientKind::Tui)
            .start_native_auth(case.start("dropped-request-retained-work"))
            .await
            .unwrap();
        assert_eq!(replay.fact, terminal);
        assert_eq!(case.snapshot().facts.len(), 1);
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_auth_dispatch_lifetime_invalidate_rolls_back_retirement_and_releases_the_reaped_fence()
 {
    lifetime_case(|case| async move {
        let (fact, mut checkpoint) = case.held("invalidate-before-commit").await;
        let pid = case.retained_pid();
        case.host.invalidate_native_auth_dispatch().unwrap();
        case.host.invalidate_native_auth_dispatch().unwrap();
        checkpoint.release().unwrap();
        case.drain().await;
        assert_reaped(pid);
        case.interrupted(&fact);
        rustix::fs::flock(
            case.probe(),
            rustix::fs::FlockOperation::NonBlockingLockExclusive,
        )
        .unwrap();
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_auth_dispatch_lifetime_service_shutdown_reaps_child_and_releases_its_fence() {
    lifetime_case(|case| async move {
        let (fact, mut checkpoint) = case.held("shutdown-before-commit").await;
        let pid = case.retained_pid();
        let mut service = case.service.lock().await.take().unwrap();
        let shutdown = tokio::spawn(async move { service.shutdown().await });
        case.wait_invalidated().await;
        checkpoint.release().unwrap();
        tokio::time::timeout(Duration::from_secs(8), shutdown)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_reaped(pid);
        case.interrupted(&fact);
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_auth_dispatch_lifetime_service_drop_performs_bounded_direct_child_cleanup() {
    lifetime_case(|case| async move {
        let (_, mut checkpoint) = case.held("drop-before-commit").await;
        let before = case.snapshot();
        let pid = case.retained_pid();
        let service = case.service.lock().await.take().unwrap();
        let dropped = tokio::spawn(async move { drop(service) });
        case.wait_invalidated().await;
        checkpoint.release().unwrap();
        tokio::time::timeout(Duration::from_secs(5), dropped)
            .await
            .unwrap()
            .unwrap();
        assert_reaped(pid);
        assert_eq!(case.snapshot().fences, before.fences);
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_auth_dispatch_lifetime_original_owner_revoke_reaps_retained_committed_child() {
    lifetime_case(|case| async move {
        let (fact, mut checkpoint) = case.held("revoke-retained-child").await;
        let pid = case.retained_pid();
        rustix::process::kill_process(pid, rustix::process::Signal::STOP).unwrap();
        checkpoint.release().unwrap();
        case.committed(&fact).await;
        case.revoke(OwnerClientKind::Tui, "revoke-original-dispatch-owner")
            .await;
        case.drain().await;
        assert_reaped(pid);
        assert!(case.snapshot().fences.is_empty());
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_auth_dispatch_lifetime_unrelated_owner_revoke_keeps_actual_child() {
    lifetime_case(|case| async move {
        let (fact, mut checkpoint) = case.held("unrelated-owner-retained-child").await;
        let pid = case.retained_pid();
        rustix::process::kill_process(pid, rustix::process::Signal::STOP).unwrap();
        checkpoint.release().unwrap();
        case.committed(&fact).await;
        case.revoke(OwnerClientKind::Desktop, "revoke-unrelated-desktop-owner")
            .await;
        assert_eq!(case.retained_pid(), pid);
        rustix::process::test_kill_process(pid).unwrap();
        rustix::process::kill_process(pid, rustix::process::Signal::CONT).unwrap();
        case.drain().await;
        assert_reaped(pid);
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_auth_dispatch_lifetime_revoked_handoff_returns_untouched_committed_work() {
    lifetime_case(|case| async move {
        case.register();
        let admission = case.admission();
        let (_, work) = case.work("revoked-before-synchronous-handoff").await;
        let before = case.snapshot();
        case.revoke(OwnerClientKind::Tui, "revoke-before-handoff")
            .await;
        let work = match case.host.retain_native_auth_dispatch(admission, work) {
            Err((_error, work)) => work,
            Ok(()) => panic!("revoked owner handed off actual committed work"),
        };
        assert_eq!(case.snapshot().fences, before.fences);
        assert!(case.host.authority.native_auth.lock().unwrap().is_none());
        drop(work);
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_auth_dispatch_lifetime_registration_after_committed_work_is_rejected() {
    lifetime_case(|case| async move {
        let reply = case
            .client(OwnerClientKind::Tui)
            .start_native_auth(case.start("registration-after-work"))
            .await
            .unwrap();
        assert_eq!(reply.fact.phase, NativeAuthPhase::Unavailable);
        let before = case.snapshot();
        assert!(
            case.host
                .register_native_auth_host(
                    RegisteredNativeAuthHost::capture(&executable()).unwrap(),
                )
                .is_err()
        );
        assert_eq!(case.snapshot(), before);
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_auth_dispatch_lifetime_real_spawn_failure_releases_the_dispatched_fence() {
    lifetime_case(|case| async move {
        let path = case.root.join("non-executable-image");
        let mut image = vec![0xce, 0xfa, 0xed, 0xfe, 18, 0, 0, 0, 0, 0, 0, 0, 2, 0, 0, 0];
        image.resize(28, 0);
        std::fs::write(&path, image).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        case.host
            .register_native_auth_host(RegisteredNativeAuthHost::capture(&path).unwrap())
            .unwrap();
        let (fact, work) = case.work("actual-os-spawn-failure").await;
        assert!(crate::spawn_gate::spawn(&mut ProcessCommand::new(&path)).is_err());
        case.retain(work);
        let drained = tokio::time::timeout(
            Duration::from_secs(8),
            case.host.drain_native_auth_dispatch(),
        )
        .await
        .unwrap();
        assert!(drained.is_ok(), "drain: {drained:?}");
        case.interrupted(&fact);
        assert!(case.host.authority.native_auth.lock().unwrap().is_none());
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_auth_dispatch_lifetime_real_broken_bootstrap_pipe_releases_the_fence() {
    lifetime_case(|case| async move {
        let (fact, work) = case.work("actual-bootstrap-pipe-failure").await;
        let launch = case.host.prepare_native_auth_custody(work).await.unwrap();
        let mut child = TestChild(
            crate::spawn_gate::spawn(
                ProcessCommand::new("/bin/cat")
                    .stdin(Stdio::piped())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null()),
            )
            .unwrap(),
        );
        let input = child.0.stdin.take().unwrap();
        child.kill_and_reap().await;
        assert!(launch.write_bootstrap(input).is_err());
        case.host.invalidate_native_auth_dispatch().unwrap();
        case.drain().await;
        case.interrupted(&fact);
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_auth_dispatch_lifetime_real_custodian_attach_failure_releases_the_fence() {
    lifetime_case(|case| async move {
        let (fact, work) = case.work("actual-custodian-attach-failure").await;
        let launch = case.host.prepare_native_auth_custody(work).await.unwrap();
        let missing = case.root.join("empty-unregistered-runtime");
        std::fs::create_dir(&missing).unwrap();
        std::fs::set_permissions(&missing, std::fs::Permissions::from_mode(0o700)).unwrap();
        let registered = RegisteredNativeAuthHost::capture(&executable()).unwrap();
        let mut child = TestChild(
            crate::spawn_gate::spawn(
                ProcessCommand::new(registered.verified_path().unwrap())
                    .arg("native-auth-custodian")
                    .arg(&missing)
                    .stdin(Stdio::piped())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null()),
            )
            .unwrap(),
        );
        launch
            .write_bootstrap(child.0.stdin.take().unwrap())
            .unwrap();
        let exited = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Some(status) = child.0.try_wait().unwrap() {
                    break status;
                }
                tokio::task::yield_now().await;
            }
        })
        .await;
        if exited.is_err() {
            child.kill_and_reap().await;
        }
        assert!(!exited.unwrap().success());
        case.host.invalidate_native_auth_dispatch().unwrap();
        case.drain().await;
        case.interrupted(&fact);
    })
    .await;
}
