use super::*;
use crate::engine::bootstrap::NativeProfileUsePurpose;
use crate::engine::owner::native_auth::Command;
use crate::storage::native_auth::{NativeAuthDispatch, NativeAuthWork};
use rustix::fs::{FlockOperation, Mode, OFlags, flock, open};
use std::fs::File;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::time::Duration;

const CATALOG: &str = r#"
timeout_ms = 1000
[model_access]
accounts = ["account-a", "account-z"]
[[model_access.connections]]
id = "connection-a"
provider = "provider-a"
harness = "codex"
protocol = "native"
account_ref = "account-a"
endpoint = { kind = "native" }
model = { id = "model-a", author = "author-a" }
auth = { method = "native_managed", profile_ref = "profile-a" }
[[model_access.connections]]
id = "connection-alias"
provider = "provider-a"
harness = "codex"
protocol = "native"
account_ref = "account-a"
endpoint = { kind = "native" }
model = { id = "model-alias", author = "author-a" }
auth = { method = "native_managed", profile_ref = "profile-a" }
[[model_access.connections]]
id = "connection-z"
provider = "provider-z"
harness = "claude"
protocol = "native"
account_ref = "account-z"
endpoint = { kind = "native" }
model = { id = "model-z", author = "author-z" }
auth = { method = "native_managed", profile_ref = "profile-z" }
[[model_access.pools]]
id = "pool-a"
members = ["connection-a", "connection-alias", "connection-z"]
projects = ["project-a"]
roles = ["leader"]
fallback_allowed = false
"#;

type OperationRow = (String, i64, String, Vec<u8>, Vec<u8>);
type FenceRow = (String, String, i64, String, String, String, String, i64);

#[derive(Debug, PartialEq, Eq)]
struct Snapshot {
    operations: Vec<OperationRow>,
    fences: Vec<FenceRow>,
}

struct CustodyCase {
    db: Connection,
    _lock: super::EngineLock,
    custody: Result<NativeProfileCustody, EngineError>,
    sources: settings::SettingsSources,
    config: EffectiveConfig,
    started: Started,
    path: PathBuf,
    global: PathBuf,
    root: tempfile::TempDir,
}

fn request(value: &str) -> RequestId {
    RequestId::new(value).unwrap()
}

fn reference(value: &str) -> CredentialRef {
    CredentialRef::new(value).unwrap()
}

fn private_root() -> tempfile::TempDir {
    let root = tempfile::Builder::new()
        .prefix("ca-custody-db-")
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    root
}

fn catalog(root: &Path) -> PathBuf {
    let path = root.join("g.toml");
    std::fs::write(&path, CATALOG).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    path
}

fn scope(config: &EffectiveConfig, host: &HostStamp, hash: String) -> NativeAuthScope {
    NativeAuthScope {
        expected_database_instance: host.db_instance.clone(),
        connection: reference("connection-a"),
        expected_config_id: config.id.clone(),
        expected_source_hash: hash,
    }
}

fn start(scope: NativeAuthScope, id: &str) -> Command {
    Command::Start(NativeAuthStart {
        request_id: request(id),
        scope,
        method: NativeAuthMethod::CodexBrowser,
    })
}

fn snapshot(path: &Path) -> Snapshot {
    let db = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )
    .unwrap();
    db.busy_timeout(Duration::from_secs(1)).unwrap();
    db.execute_batch("BEGIN").unwrap();
    let operations = db
        .prepare("SELECT id,generation,phase,request,fact FROM native_auth_operations ORDER BY id")
        .unwrap()
        .query_map([], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
            ))
        })
        .unwrap()
        .collect::<Result<Vec<OperationRow>, _>>()
        .unwrap();
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
        .collect::<Result<Vec<FenceRow>, _>>()
        .unwrap();
    Snapshot { operations, fences }
}

impl CustodyCase {
    fn new() -> Self {
        let root = private_root();
        let global = catalog(root.path());
        let sources =
            settings::SettingsSources::open(&global, &root.path().join("p.toml")).unwrap();
        let config = sources.config().unwrap();
        let path = root.path().join("engine.db");
        let OpenedDatabase {
            mut connection,
            lock,
            custody,
        } = open_actor_database(&path, false, None).unwrap();
        let started = owner_start(&mut connection).unwrap();
        Self {
            db: connection,
            _lock: lock,
            custody,
            sources,
            config,
            started,
            path,
            global,
            root,
        }
    }

    fn command(&self, id: &str) -> Command {
        start(
            scope(
                &self.config,
                &self.started.host,
                settings::verified_source_hash(&self.config, Some(&self.sources)).unwrap(),
            ),
            id,
        )
    }

    fn begin(&mut self, command: Command) -> NativeAuthBegin {
        native_auth::begin_native_auth(
            &mut self.db,
            &self.config,
            Some(&self.sources),
            &mut self.custody,
            &self.started.owners[0].admission,
            command,
        )
        .unwrap()
    }

    fn work(&mut self, id: &str) -> (NativeAuthFact, NativeAuthWork) {
        let begun = self.begin(self.command(id));
        assert!(self.db.is_autocommit());
        assert_eq!(begun.reply.fact.phase, NativeAuthPhase::Starting);
        assert!(begun.reply.challenge.is_none());
        assert!(begun.reply.account.is_none());
        let work = begun.work.unwrap();
        assert_eq!(work.fact(), &begun.reply.fact);
        (begun.reply.fact, work)
    }

    fn dispatch(&mut self, work: NativeAuthWork) -> Result<NativeAuthDispatch, EngineError> {
        native_auth::dispatch_native_auth(
            &mut self.db,
            &self.config,
            Some(&self.sources),
            &mut self.custody,
            work,
        )
    }

    fn check(&mut self, dispatch: &NativeAuthDispatch) -> Result<NativeAuthFact, EngineError> {
        native_auth::check_native_auth_custody(
            &mut self.db,
            &self.config,
            Some(&self.sources),
            dispatch,
        )
    }

    fn snapshot(&self) -> Snapshot {
        snapshot(&self.path)
    }

    fn manifest(&self) -> PathBuf {
        self.root
            .path()
            .join("native-profiles")
            .join(format!("{:x}", Sha256::digest(b"profile-a")))
            .join("identity.json")
    }

    fn probe(&self) -> File {
        File::from(
            open(
                self.manifest(),
                OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .unwrap(),
        )
    }

    fn agent(
        &self,
        fact: &NativeAuthFact,
        name: &str,
    ) -> crate::engine::bootstrap::NativeProfileUse {
        NativeProfileCustody::capture(&self.path, &std::fs::metadata(&self.path).unwrap())
            .unwrap()
            .acquire_use(
                &self.started.host,
                &fact.binding,
                &request(name),
                1,
                NativeProfileUsePurpose::Execution,
            )
            .unwrap()
    }

    fn recover(&mut self, ended: Option<&NativeAuthFact>) -> Option<NativeAuthFact> {
        native_auth::recover_native_auth(&mut self.db, &mut self.custody, &self.started.host, ended)
            .unwrap()
    }

    fn end_custody(&mut self, dispatch: &NativeAuthDispatch) -> Option<NativeAuthFact> {
        native_auth::lose_native_auth_custody(
            &mut self.db,
            &self.config,
            Some(&self.sources),
            dispatch,
        )
        .unwrap();
        self.recover(Some(dispatch.fact()))
    }

    fn revoke_origin(&mut self) {
        let control = OwnerControl {
            operation_id: request("revoke-custody-origin"),
            target: OwnerClientKind::Tui,
            expected_epoch: self.started.owners[0].admission.owner_epoch,
            decision: OwnerDecision::Revoke,
        };
        let (reply, _) = owner_control(&mut self.db, &self.started.host, &control).unwrap();
        assert!(!reply.active);
    }
}

#[test]
fn native_auth_custody_dispatch_requires_genuine_committed_starting_and_fence() {
    trait NonClone<A> {
        fn evidence() {}
    }
    impl<T: ?Sized> NonClone<()> for T {}
    impl<T: Clone> NonClone<u8> for T {}
    let _ = <NativeAuthDispatch as NonClone<_>>::evidence;
    let mut case = CustodyCase::new();
    let (fact, work) = case.work("dispatch-committed");
    let before = case.snapshot();
    assert_eq!(before.operations.len(), 1);
    assert_eq!(before.fences.len(), 1);
    assert_eq!(before.operations[0].2, "starting");
    assert_eq!(before.fences[0].1, fact.operation_id.as_str());
    assert_eq!(before.fences[0].2, i64::try_from(fact.generation).unwrap());
    assert_eq!(
        before.fences[0].6,
        case.started.owners[0].admission.owner_id
    );
    let dispatch = case.dispatch(work).unwrap();
    assert_eq!(dispatch.fact(), &fact);
    assert_eq!(case.check(&dispatch).unwrap(), fact);
    assert_eq!(case.snapshot(), before);
    assert!(case.db.is_autocommit());
}

#[test]
fn native_auth_custody_dispatch_exact_replay_never_returns_second_work() {
    let mut case = CustodyCase::new();
    let command = case.command("dispatch-replay");
    let begun = case.begin(command.clone());
    let fact = begun.reply.fact;
    let dispatch = case.dispatch(begun.work.unwrap()).unwrap();
    let before = case.snapshot();
    let replay = case.begin(command);
    assert!(replay.work.is_none());
    assert_eq!(replay.reply.fact, fact);
    assert_eq!(case.check(&dispatch).unwrap(), fact);
    assert_eq!(case.snapshot(), before);
}

#[test]
fn native_auth_custody_dispatch_alias_and_other_profile_cannot_queue() {
    let mut case = CustodyCase::new();
    let (_, work) = case.work("one-custody-flow");
    let dispatch = case.dispatch(work).unwrap();
    let before = case.snapshot();
    for connection in ["connection-alias", "connection-z"] {
        let mut command = case.command("no-second-custody-flow");
        let Command::Start(ref mut request) = command else {
            unreachable!()
        };
        request.scope.connection = reference(connection);
        if connection == "connection-z" {
            request.method = NativeAuthMethod::ClaudeOfficial;
        }
        let rejected = native_auth::begin_native_auth(
            &mut case.db,
            &case.config,
            Some(&case.sources),
            &mut case.custody,
            &case.started.owners[0].admission,
            command,
        );
        assert!(rejected.is_err());
        assert_eq!(case.snapshot(), before);
    }
    assert!(case.check(&dispatch).is_ok());
    let unused = case.root.path().join("native-profiles");
    assert!(
        !unused
            .join(format!("{:x}", Sha256::digest(b"profile-z")))
            .exists()
    );
}

#[test]
fn native_auth_custody_dispatch_rejects_revoked_original_owner() {
    let mut case = CustodyCase::new();
    let (_, work) = case.work("revoked-before-dispatch");
    case.revoke_origin();
    let before = case.snapshot();
    assert!(case.dispatch(work).is_err());
    assert_eq!(case.snapshot(), before);
    assert_eq!(before.fences.len(), 1);
}

#[test]
fn native_auth_custody_check_rejects_revoked_original_owner() {
    let mut case = CustodyCase::new();
    let (_, work) = case.work("revoked-after-dispatch");
    let dispatch = case.dispatch(work).unwrap();
    case.revoke_origin();
    let before = case.snapshot();
    assert!(case.check(&dispatch).is_err());
    assert_eq!(case.snapshot(), before);
}

#[test]
fn native_auth_custody_check_rejects_another_still_active_fence_owner() {
    let mut case = CustodyCase::new();
    let (_, work) = case.work("substituted-owner");
    let dispatch = case.dispatch(work).unwrap();
    let other = &case.started.owners[1].admission;
    assert_ne!(other.owner_id, case.started.owners[0].admission.owner_id);
    case.db
        .execute(
            "UPDATE native_auth_fences SET owner_id=?1,owner_epoch=?2",
            params![other.owner_id, other.owner_epoch],
        )
        .unwrap();
    let before = case.snapshot();
    assert!(case.check(&dispatch).is_err());
    assert_eq!(case.snapshot(), before);
}

#[test]
fn native_auth_custody_dispatch_rejects_changed_source_bytes() {
    let mut case = CustodyCase::new();
    let (_, work) = case.work("changed-before-dispatch");
    std::fs::write(&case.global, format!("{CATALOG}\n")).unwrap();
    let before = case.snapshot();
    assert!(case.dispatch(work).is_err());
    assert_eq!(case.snapshot(), before);
}

#[test]
fn native_auth_custody_check_rejects_source_and_config_changes() {
    for change in ["source", "config"] {
        let mut case = CustodyCase::new();
        let (_, work) = case.work("changed-after-dispatch");
        let dispatch = case.dispatch(work).unwrap();
        if change == "source" {
            std::fs::write(&case.global, format!("{CATALOG}\n")).unwrap();
        } else {
            std::fs::write(&case.global, CATALOG.replace("model-a", "changed-model")).unwrap();
            case.config =
                crate::config::resolve_config(&case.global, &case.root.path().join("p.toml"))
                    .unwrap();
            assert_ne!(case.config.id, dispatch.fact().binding.config_id);
        }
        let before = case.snapshot();
        assert!(case.check(&dispatch).is_err(), "accepted {change} change");
        assert_eq!(case.snapshot(), before);
    }
}

#[test]
fn native_auth_custody_check_rejects_each_changed_durable_scope_join() {
    let updates = [
        "UPDATE native_auth_fences SET generation=generation+1",
        "UPDATE native_auth_fences SET owner_epoch=owner_epoch+1",
        "UPDATE native_auth_fences SET db_instance=lower(hex(randomblob(32)))",
        "UPDATE native_auth_fences SET engine_epoch=lower(hex(randomblob(32)))",
        "UPDATE native_auth_fences SET custody_id=lower(hex(randomblob(32)))",
        "UPDATE native_auth_fences SET profile_key=lower(hex(randomblob(32)))",
    ];
    for sql in updates {
        let mut case = CustodyCase::new();
        let (_, work) = case.work("changed-fence-join");
        let dispatch = case.dispatch(work).unwrap();
        assert_eq!(case.db.execute(sql, []).unwrap(), 1);
        let before = case.snapshot();
        assert!(case.check(&dispatch).is_err(), "accepted {sql}");
        assert_eq!(case.snapshot(), before);
    }
}

#[test]
fn native_auth_custody_check_rejects_dispatch_from_a_different_database() {
    let mut first = CustodyCase::new();
    let mut second = CustodyCase::new();
    let (_, first_work) = first.work("same-operation-id");
    let (_, second_work) = second.work("same-operation-id");
    let dispatch = first.dispatch(first_work).unwrap();
    let second_dispatch = second.dispatch(second_work).unwrap();
    let before = second.snapshot();
    assert_ne!(
        dispatch.fact().binding.database_instance,
        second_dispatch.fact().binding.database_instance
    );
    assert!(second.check(&dispatch).is_err());
    assert_eq!(second.snapshot(), before);
    assert!(second.check(&second_dispatch).is_ok());
}

#[test]
fn native_auth_custody_check_rejects_stale_engine_incarnation() {
    let mut case = CustodyCase::new();
    let (_, work) = case.work("stale-engine-dispatch");
    let dispatch = case.dispatch(work).unwrap();
    owner_stop(&mut case.db, &case.started.host).unwrap();
    let restarted = owner_start(&mut case.db).unwrap();
    assert_ne!(restarted.host.engine_epoch, case.started.host.engine_epoch);
    case.started = restarted;
    let before = case.snapshot();
    assert!(case.check(&dispatch).is_err());
    assert_eq!(case.snapshot(), before);
}

#[test]
fn native_auth_custody_dispatch_revalidates_actual_manifest_descriptor() {
    let mut case = CustodyCase::new();
    let (_, work) = case.work("replaced-before-dispatch");
    let manifest = case.manifest();
    let bytes = std::fs::read(&manifest).unwrap();
    std::fs::rename(&manifest, manifest.with_extension("retained")).unwrap();
    std::fs::write(&manifest, bytes).unwrap();
    std::fs::set_permissions(&manifest, std::fs::Permissions::from_mode(0o600)).unwrap();
    let before = case.snapshot();
    assert!(case.dispatch(work).is_err());
    assert_eq!(case.snapshot(), before);
}

#[test]
fn native_auth_custody_check_keeps_exact_generation_while_real_guard_is_held() {
    let mut case = CustodyCase::new();
    let (fact, work) = case.work("generation-guard-join");
    let dispatch = case.dispatch(work).unwrap();
    let mut independent =
        NativeProfileCustody::capture(&case.path, &std::fs::metadata(&case.path).unwrap()).unwrap();
    let guard = independent
        .acquire_use(
            &case.started.host,
            &fact.binding,
            &fact.operation_id,
            fact.generation,
            NativeProfileUsePurpose::Authentication,
        )
        .unwrap();
    let probe = case.probe();
    assert!(flock(&probe, FlockOperation::NonBlockingLockExclusive).is_err());
    let before = case.snapshot();
    assert_eq!(case.check(&dispatch).unwrap(), fact);
    assert_eq!(case.snapshot(), before);
    guard
        .revalidate(
            &case.started.host,
            &fact.binding,
            &fact.operation_id,
            fact.generation,
            NativeProfileUsePurpose::Authentication,
        )
        .unwrap();
    drop(guard);
    flock(&probe, FlockOperation::NonBlockingLockExclusive).unwrap();
    assert_eq!(case.snapshot(), before);
}

#[test]
fn native_auth_custody_check_observes_actual_committed_cancellation() {
    let mut case = CustodyCase::new();
    let (fact, work) = case.work("cancel-retained-custody");
    let dispatch = case.dispatch(work).unwrap();
    let Command::Start(start) = case.command("cancel-scope") else {
        unreachable!()
    };
    let reply = native_auth::cancel_native_auth(
        &mut case.db,
        &case.config,
        Some(&case.sources),
        &case.started.owners[0].admission,
        &NativeAuthCancel {
            request_id: request("cancel-custody"),
            scope: start.scope,
            operation_id: fact.operation_id.clone(),
        },
        &fact,
    )
    .unwrap();
    assert_eq!(reply.fact.phase, NativeAuthPhase::Cancelling);
    let before = case.snapshot();
    assert_eq!(case.check(&dispatch).unwrap(), reply.fact);
    assert_eq!(case.snapshot(), before);
    assert_eq!(before.fences.len(), 1);
}

#[test]
fn native_auth_custody_dropping_dispatch_never_retires_a_free_physical_lock() {
    let mut case = CustodyCase::new();
    let (_, work) = case.work("drop-dispatch");
    let dispatch = case.dispatch(work).unwrap();
    let before = case.snapshot();
    drop(dispatch);
    let probe = case.probe();
    flock(&probe, FlockOperation::NonBlockingLockExclusive).unwrap();
    drop(probe);
    assert_eq!(case.snapshot(), before);
    let mut alias = case.command("drop-cannot-authorize-alias");
    let Command::Start(ref mut start) = alias else {
        unreachable!()
    };
    start.scope.connection = reference("connection-alias");
    assert!(
        native_auth::begin_native_auth(
            &mut case.db,
            &case.config,
            Some(&case.sources),
            &mut case.custody,
            &case.started.owners[0].admission,
            alias,
        )
        .is_err()
    );
    assert_eq!(case.snapshot(), before);
}

#[test]
fn native_auth_custody_reopen_preserves_dispatch_fence_and_reports_uncertainty() {
    let mut case = CustodyCase::new();
    let (fact, work) = case.work("reopen-dispatch");
    let dispatch = case.dispatch(work).unwrap();
    let before = case.snapshot();
    drop(dispatch);
    let CustodyCase {
        db,
        _lock: lock,
        custody,
        sources,
        config,
        path,
        global: _,
        root: _root,
        started: _,
    } = case;
    drop(db);
    drop(custody);
    drop(lock);
    let OpenedDatabase {
        mut connection,
        lock: _reopened_lock,
        custody: _reopened_custody,
    } = open_actor_database(&path, false, None).unwrap();
    let restarted = owner_start(&mut connection).unwrap();
    let observed = native_auth::read_native_auth(
        &mut connection,
        &config,
        Some(&sources),
        &restarted.owners[0].admission,
        &NativeAuthQuery {
            request_id: request("read-reopened-dispatch"),
            scope: scope(
                &config,
                &restarted.host,
                settings::verified_source_hash(&config, Some(&sources)).unwrap(),
            ),
            operation_id: fact.operation_id,
        },
    )
    .unwrap();
    assert_eq!(observed.fact.phase, NativeAuthPhase::Uncertain);
    assert_eq!(snapshot(&path).fences, before.fences);
}

#[tokio::test]
async fn native_auth_custody_dropped_dispatch_ack_keeps_real_actor_fence() {
    let root = private_root();
    let global = catalog(root.path());
    let path = root.path().join("engine.db");
    let project = root.path().join("p.toml");
    let (storage, config) = Storage::open_with_settings_sources(&path, &global, &project)
        .await
        .unwrap();
    let (send, receive) = oneshot::channel();
    storage
        .sender
        .try_send(Job::OwnerStart(send))
        .unwrap_or_else(|_| panic!("start queue"));
    let started = tokio::time::timeout(Duration::from_secs(3), receive)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let admission = started.owners[0].admission.clone();
    let (send, receive) = oneshot::channel();
    storage
        .sender
        .try_send(Job::OwnerRead(
            admission.clone(),
            OwnerRead::ModelAccessRecord {
                collection: ModelAccessCollection::Connections,
                id: reference("connection-a"),
            },
            send,
        ))
        .unwrap_or_else(|_| panic!("source queue"));
    let state = tokio::time::timeout(Duration::from_secs(3), receive)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let OwnerData::ModelAccessRecord { source_hash, .. } = state.data else {
        panic!("actual actor did not return source identity")
    };
    let scope = scope(&config, &started.host, source_hash);
    let (send, receive) = oneshot::channel();
    storage
        .sender
        .try_send(Job::NativeAuth(
            admission.clone(),
            start(scope.clone(), "lost-dispatch-ack"),
            send,
        ))
        .unwrap_or_else(|_| panic!("begin queue"));
    let begun = tokio::time::timeout(Duration::from_secs(3), receive)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let before = snapshot(&path);
    let (send, receive) = oneshot::channel();
    drop(receive);
    storage
        .sender
        .try_send(Job::NativeAuthDispatch(Box::new(begun.work.unwrap()), send))
        .unwrap_or_else(|_| panic!("dispatch queue"));
    let (send, receive) = oneshot::channel();
    storage
        .sender
        .try_send(Job::NativeAuth(
            admission,
            Command::Read(NativeAuthQuery {
                request_id: request("dispatch-ack-barrier"),
                scope,
                operation_id: begun.reply.fact.operation_id,
            }),
            send,
        ))
        .unwrap_or_else(|_| panic!("barrier queue"));
    let result = tokio::time::timeout(Duration::from_secs(3), receive).await;
    let after = snapshot(&path);
    let (send, receive) = oneshot::channel();
    storage
        .sender
        .try_send(Job::Close(send))
        .unwrap_or_else(|_| panic!("close queue"));
    tokio::time::timeout(Duration::from_secs(3), receive)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let reply = result.unwrap().unwrap().unwrap();
    assert!(reply.work.is_none());
    assert_eq!(after.fences, before.fences);
    assert_eq!(after.operations.len(), 1);
    assert_eq!(after.operations[0].1, before.operations[0].1);
}

fn interrupted(settled: Option<NativeAuthFact>) -> Option<(NativeAuthPhase, Option<String>)> {
    settled.map(|fact| (fact.phase, fact.code))
}

#[test]
fn native_auth_custody_that_ended_while_an_agent_uses_the_account_is_settled_at_once() {
    let mut case = CustodyCase::new();
    let (fact, work) = case.work("ended-beside-agent");
    let dispatch = case.dispatch(work).unwrap();
    let agent = case.agent(&fact, "agent-beside-ended-custody");
    assert_eq!(
        interrupted(case.end_custody(&dispatch)),
        Some((
            NativeAuthPhase::Failed,
            Some("native_auth_interrupted".to_owned())
        ))
    );
    assert!(case.snapshot().fences.is_empty());
    drop(agent);
}

async fn ask<T>(storage: &Storage, job: impl FnOnce(oneshot::Sender<T>) -> Job) -> T {
    let (send, receive) = oneshot::channel();
    storage
        .sender
        .try_send(job(send))
        .unwrap_or_else(|_| panic!("storage queue"));
    tokio::time::timeout(Duration::from_secs(3), receive)
        .await
        .unwrap()
        .unwrap()
}

#[tokio::test]
async fn native_auth_custody_that_ended_during_a_renewal_is_settled_by_a_later_read_once_it_ends() {
    let root = private_root();
    let global = catalog(root.path());
    let path = root.path().join("engine.db");
    let (storage, config) =
        Storage::open_with_settings_sources(&path, &global, &root.path().join("p.toml"))
            .await
            .unwrap();
    let started = ask(&storage, Job::OwnerStart).await.unwrap();
    let admission = started.owners[0].admission.clone();
    let state = ask(&storage, |send| {
        Job::OwnerRead(
            admission.clone(),
            OwnerRead::ModelAccessRecord {
                collection: ModelAccessCollection::Connections,
                id: reference("connection-a"),
            },
            send,
        )
    })
    .await
    .unwrap();
    let OwnerData::ModelAccessRecord { source_hash, .. } = state.data else {
        panic!("actual actor did not return source identity")
    };
    let scope = scope(&config, &started.host, source_hash);
    let begun = ask(&storage, |send| {
        Job::NativeAuth(
            admission.clone(),
            start(scope.clone(), "ended-during-renewal"),
            send,
        )
    })
    .await
    .unwrap();
    let fact = begun.reply.fact.clone();
    let dispatch = Arc::new(
        ask(&storage, |send| {
            Job::NativeAuthDispatch(Box::new(begun.work.unwrap()), send)
        })
        .await
        .unwrap(),
    );
    let agent = NativeProfileCustody::capture(&path, &std::fs::metadata(&path).unwrap())
        .unwrap()
        .acquire_use(
            &started.host,
            &fact.binding,
            &request("agent-renewing"),
            1,
            NativeProfileUsePurpose::Execution,
        )
        .unwrap();
    let renewal = agent.hold_renewal(Duration::ZERO).unwrap();
    let lost = ask(&storage, |send| {
        Job::NativeAuthCustodyLost(dispatch.clone(), send)
    })
    .await
    .unwrap();
    assert_eq!(lost.phase, NativeAuthPhase::Uncertain);
    let released = ask(&storage, |send| {
        Job::NativeAuthCustodyReleased(dispatch.clone(), send)
    })
    .await
    .unwrap();
    assert_eq!(released, None);
    let read = |name: &str| {
        let query = NativeAuthQuery {
            request_id: request(name),
            scope: scope.clone(),
            operation_id: fact.operation_id.clone(),
        };
        let admission = admission.clone();
        move |send: oneshot::Sender<Result<NativeAuthBegin, EngineError>>| {
            Job::NativeAuth(admission, Command::Read(query), send)
        }
    };
    let during = ask(&storage, read("read-during-renewal")).await.unwrap();
    assert_eq!(during.reply.fact.phase, NativeAuthPhase::Uncertain);
    assert_eq!(snapshot(&path).fences.len(), 1);
    drop(renewal);
    let after = ask(&storage, read("read-after-renewal")).await.unwrap();
    assert_eq!(
        (after.reply.fact.phase, after.reply.fact.code),
        (
            NativeAuthPhase::Failed,
            Some("native_auth_interrupted".to_owned())
        )
    );
    assert!(snapshot(&path).fences.is_empty());
    drop(agent);
    ask(&storage, Job::Close).await.unwrap();
}
