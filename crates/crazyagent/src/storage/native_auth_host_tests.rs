use super::*;
use crate::engine::bootstrap::NativeProfileUsePurpose;
use crate::engine::owner::native_auth::Command;
use crate::storage::native_auth::{NativeAuthBegin, NativeAuthHostUpdate, NativeAuthWork};
use rustix::fs::{FlockOperation, Mode, OFlags, flock, open};
use std::fs::File;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

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

#[derive(Debug, PartialEq, Eq)]
struct StoredOperation {
    id: String,
    generation: i64,
    phase: String,
    request: Vec<u8>,
    fact: Vec<u8>,
}

#[derive(Debug, PartialEq, Eq)]
struct StoredFence {
    profile_key: String,
    operation_id: String,
    generation: i64,
    db_instance: String,
    engine_epoch: String,
    custody_id: String,
}

struct HostCase {
    db: Connection,
    lock: File,
    custody: Result<NativeProfileCustody, EngineError>,
    sources: settings::SettingsSources,
    config: EffectiveConfig,
    started: Started,
    path: PathBuf,
    global: PathBuf,
    root: tempfile::TempDir,
}

fn reference(value: &str) -> CredentialRef {
    CredentialRef::new(value).unwrap()
}

fn request_id(value: &str) -> RequestId {
    RequestId::new(value).unwrap()
}

fn assert_code<T>(result: Result<T, EngineError>, code: &str) {
    match result {
        Ok(_) => panic!("expected {code}"),
        Err(error) => assert_eq!(error.0, code),
    }
}

fn fresh_work(begin: NativeAuthBegin) -> (NativeAuthFact, NativeAuthWork) {
    trait NonCloneEvidence<A> {
        fn evidence() {}
    }
    impl<T: ?Sized> NonCloneEvidence<()> for T {}
    impl<T: Clone> NonCloneEvidence<u8> for T {}
    let _ = <NativeAuthWork as NonCloneEvidence<_>>::evidence;
    assert_eq!(begin.reply.fact.phase, NativeAuthPhase::Starting);
    assert!(begin.reply.challenge.is_none());
    assert!(begin.reply.account.is_none());
    let work = begin.work.expect("one committed work value");
    assert_eq!(work.fact(), &begin.reply.fact);
    (begin.reply.fact, work)
}

impl HostCase {
    fn new() -> Self {
        let root = tempfile::Builder::new()
            .prefix("ca-host-storage-")
            .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
            .unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let global = root.path().join("g.toml");
        std::fs::write(&global, CATALOG).unwrap();
        std::fs::set_permissions(&global, std::fs::Permissions::from_mode(0o600)).unwrap();
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
        assert!(connection.is_autocommit());
        assert!(custody.is_ok());
        Self {
            db: connection,
            lock,
            custody,
            sources,
            config,
            started,
            path,
            global,
            root,
        }
    }

    fn scope(&self, connection: &str) -> NativeAuthScope {
        NativeAuthScope {
            expected_database_instance: self.started.host.db_instance.clone(),
            connection: reference(connection),
            expected_config_id: self.config.id.clone(),
            expected_source_hash: settings::verified_source_hash(&self.config, Some(&self.sources))
                .unwrap(),
        }
    }

    fn start(&self, id: &str, connection: &str) -> Command {
        Command::Start(NativeAuthStart {
            request_id: request_id(id),
            scope: self.scope(connection),
            method: if connection == "connection-z" {
                NativeAuthMethod::ClaudeOfficial
            } else {
                NativeAuthMethod::CodexBrowser
            },
        })
    }

    fn logout(&self, id: &str, connection: &str) -> Command {
        Command::Logout(NativeAuthLogout {
            request_id: request_id(id),
            scope: self.scope(connection),
        })
    }

    fn begin(&mut self, command: Command) -> Result<NativeAuthBegin, EngineError> {
        native_auth::begin_native_auth(
            &mut self.db,
            &self.config,
            Some(&self.sources),
            &mut self.custody,
            &self.started.owners[0].admission,
            command,
        )
    }

    fn query(fact: &NativeAuthFact) -> NativeAuthQuery {
        NativeAuthQuery {
            request_id: request_id("read-host-operation"),
            scope: NativeAuthScope {
                expected_database_instance: fact.binding.database_instance.clone(),
                connection: fact.binding.connection.id.clone(),
                expected_config_id: fact.binding.config_id.clone(),
                expected_source_hash: fact.binding.source_hash.clone(),
            },
            operation_id: fact.operation_id.clone(),
        }
    }

    fn read(&mut self, fact: &NativeAuthFact) -> Result<NativeAuthReply, EngineError> {
        native_auth::read_native_auth(
            &mut self.db,
            &self.config,
            Some(&self.sources),
            &self.started.owners[0].admission,
            &Self::query(fact),
        )
    }

    fn cancel(&mut self, fact: &NativeAuthFact) -> Result<NativeAuthReply, EngineError> {
        let query = Self::query(fact);
        native_auth::cancel_native_auth(
            &mut self.db,
            &self.config,
            Some(&self.sources),
            &self.started.owners[0].admission,
            &NativeAuthCancel {
                request_id: request_id("cancel-host-operation"),
                scope: query.scope,
                operation_id: query.operation_id,
            },
            fact,
        )
    }

    fn publish(
        &mut self,
        fact: &NativeAuthFact,
        update: NativeAuthHostUpdate,
    ) -> Result<NativeAuthFact, EngineError> {
        native_auth::publish_native_auth(
            &mut self.db,
            &self.config,
            Some(&self.sources),
            &self.started.host,
            fact,
            update,
        )
    }

    fn finish(&mut self, work: NativeAuthWork) -> Result<NativeAuthReply, EngineError> {
        native_auth::finish_native_auth_without_effect(
            &mut self.db,
            &self.config,
            Some(&self.sources),
            &mut self.custody,
            &self.started.owners[0].admission,
            work,
        )
    }

    fn observer(&self) -> Connection {
        Connection::open_with_flags(
            &self.path,
            OpenFlags::SQLITE_OPEN_READ_ONLY
                | OpenFlags::SQLITE_OPEN_NO_MUTEX
                | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )
        .unwrap()
    }

    fn snapshot(&self) -> (Vec<StoredOperation>, Vec<StoredFence>) {
        let observer = self.observer();
        let operations = observer
            .prepare(
                "SELECT id,generation,phase,request,fact FROM native_auth_operations ORDER BY id",
            )
            .unwrap()
            .query_map([], |row| {
                Ok(StoredOperation {
                    id: row.get(0)?,
                    generation: row.get(1)?,
                    phase: row.get(2)?,
                    request: row.get(3)?,
                    fact: row.get(4)?,
                })
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        let fences = observer
            .prepare("SELECT profile_key,operation_id,generation,db_instance,engine_epoch,custody_id FROM native_auth_fences ORDER BY profile_key")
            .unwrap()
            .query_map([], |row| {
                Ok(StoredFence {
                    profile_key: row.get(0)?,
                    operation_id: row.get(1)?,
                    generation: row.get(2)?,
                    db_instance: row.get(3)?,
                    engine_epoch: row.get(4)?,
                    custody_id: row.get(5)?,
                })
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        (operations, fences)
    }

    fn profile(&self, profile: &str) -> PathBuf {
        self.root
            .path()
            .join("native-profiles")
            .join(format!("{:x}", Sha256::digest(profile.as_bytes())))
    }

    fn reopen(self) -> Self {
        let Self {
            db,
            lock,
            custody,
            sources,
            config,
            started,
            path,
            global,
            root,
        } = self;
        let old_epoch = started.host.engine_epoch;
        drop(db);
        drop(custody);
        drop(lock);
        let OpenedDatabase {
            mut connection,
            lock,
            custody,
        } = open_actor_database(&path, false, None).unwrap();
        let started = owner_start(&mut connection).unwrap();
        assert_ne!(started.host.engine_epoch, old_epoch);
        assert!(connection.is_autocommit());
        Self {
            db: connection,
            lock,
            custody,
            sources,
            config,
            started,
            path,
            global,
            root,
        }
    }
}

fn lock_probe(case: &HostCase) -> File {
    File::from(
        open(
            case.profile("profile-a").join("identity.json"),
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
            Mode::empty(),
        )
        .unwrap(),
    )
}

#[test]
fn native_effect_requires_committed_starting_and_fence() {
    let mut case = HostCase::new();
    assert!(case.snapshot().0.is_empty());
    assert!(case.snapshot().1.is_empty());
    let (fact, work) = fresh_work(
        case.begin(case.start("committed-start", "connection-a"))
            .unwrap(),
    );
    assert!(case.db.is_autocommit());
    let (operations, fences) = case.snapshot();
    assert_eq!(operations.len(), 1);
    assert_eq!(fences.len(), 1);
    assert_eq!(operations[0].phase, "starting");
    assert_eq!(owner_auth::decode_fact(&operations[0].fact).unwrap(), fact);
    assert_eq!(fences[0].operation_id, fact.operation_id.as_str());
    assert_eq!(
        fences[0].generation,
        i64::try_from(fact.generation).unwrap()
    );
    assert_eq!(fences[0].db_instance, fact.binding.database_instance);
    assert_eq!(fences[0].engine_epoch, fact.binding.engine_epoch);
    assert_eq!(fences[0].custody_id, fact.binding.custody_id);
    assert_eq!(
        fences[0].profile_key,
        format!("{:x}", Sha256::digest(b"profile-a"))
    );
    assert_eq!(work.fact(), &fact);
}

#[test]
fn logout_requires_committed_starting_and_fence() {
    let mut case = HostCase::new();
    let (fact, work) = fresh_work(
        case.begin(case.logout("committed-logout", "connection-a"))
            .unwrap(),
    );
    assert_eq!(fact.action, NativeAuthAction::Logout);
    assert!(case.db.is_autocommit());
    let (operations, fences) = case.snapshot();
    assert_eq!(operations[0].phase, "starting");
    assert_eq!(fences.len(), 1);
    assert_eq!(fences[0].operation_id, work.fact().operation_id.as_str());
}

#[test]
fn failed_fence_insert_rolls_back_starting_and_returns_no_work() {
    let mut case = HostCase::new();
    case.db.execute_batch("CREATE TRIGGER deny_host_fence BEFORE INSERT ON native_auth_fences BEGIN SELECT RAISE(ABORT,'denied_fence'); END;").unwrap();
    let before = case.snapshot();
    assert!(
        case.begin(case.start("reject-fence", "connection-a"))
            .is_err()
    );
    assert!(case.db.is_autocommit());
    assert_eq!(case.snapshot(), before);
}

#[test]
fn failed_intent_insert_never_leaves_a_fence() {
    let mut case = HostCase::new();
    case.db.execute_batch("CREATE TRIGGER deny_host_intent BEFORE INSERT ON native_auth_operations BEGIN SELECT RAISE(ABORT,'denied_intent'); END;").unwrap();
    let before = case.snapshot();
    assert!(
        case.begin(case.logout("reject-intent", "connection-a"))
            .is_err()
    );
    assert!(case.db.is_autocommit());
    assert_eq!(case.snapshot(), before);
}

#[test]
fn exact_request_replay_returns_no_second_work_while_busy() {
    let mut case = HostCase::new();
    let (fact, work) = fresh_work(
        case.begin(case.start("same-request", "connection-a"))
            .unwrap(),
    );
    let before = case.snapshot();
    for _ in 0..2 {
        let replay = case
            .begin(case.start("same-request", "connection-a"))
            .unwrap();
        assert_eq!(replay.reply.fact, fact);
        assert!(replay.work.is_none());
        assert_eq!(case.snapshot(), before);
    }
    assert_eq!(work.fact().generation, fact.generation);
}

#[test]
fn changed_known_request_conflicts_before_busy_or_preparation() {
    let mut case = HostCase::new();
    let (_fact, _work) = fresh_work(
        case.begin(case.start("known-conflict", "connection-a"))
            .unwrap(),
    );
    let before = case.snapshot();
    assert_code(
        case.begin(case.logout("known-conflict", "connection-a")),
        "native_auth_operation_conflict",
    );
    assert_code(
        case.begin(case.start("known-conflict", "connection-z")),
        "native_auth_operation_conflict",
    );
    assert!(!case.profile("profile-z").exists());
    assert_eq!(case.snapshot(), before);
}

#[test]
fn one_active_flow_rejects_another_profile_without_a_queue_or_preparation() {
    let mut case = HostCase::new();
    let (_fact, _work) = fresh_work(case.begin(case.start("active-a", "connection-a")).unwrap());
    let before = case.snapshot();
    assert_code(
        case.begin(case.start("busy-z", "connection-z")),
        "native_auth_host_busy",
    );
    assert_code(
        case.begin(case.logout("busy-z-logout", "connection-z")),
        "native_auth_host_busy",
    );
    assert!(!case.profile("profile-z").exists());
    assert_eq!(case.snapshot(), before);
}

#[test]
fn compatible_alias_start_and_logout_share_the_physical_fence() {
    for logout in [false, true] {
        let mut case = HostCase::new();
        let (_fact, _work) = fresh_work(
            case.begin(case.start("physical-a", "connection-a"))
                .unwrap(),
        );
        let before = case.snapshot();
        let command = if logout {
            case.logout("alias-operation", "connection-alias")
        } else {
            case.start("alias-operation", "connection-alias")
        };
        assert_code(case.begin(command), "native_auth_recovery_required");
        assert_eq!(case.snapshot(), before);
        assert_eq!(case.snapshot().1.len(), 1);
    }
}

#[test]
fn capacity_precedes_new_preparation_and_preserves_known_replay() {
    let mut case = HostCase::new();
    let mut first = None;
    for index in 0..1023 {
        let (fact, work) = fresh_work(
            case.begin(case.start(&format!("capacity-{index}"), "connection-a"))
                .unwrap(),
        );
        let finished = case.finish(work).unwrap();
        assert_eq!(finished.fact.phase, NativeAuthPhase::Unavailable);
        assert_eq!(finished.fact.generation, fact.generation);
        if index == 0 {
            first = Some(finished.fact);
        }
    }
    let (active, _work) = fresh_work(
        case.begin(case.start("capacity-1023", "connection-a"))
            .unwrap(),
    );
    let before = case.snapshot();
    assert_eq!(before.0.len(), 1024);
    assert_eq!(before.1.len(), 1);
    let replay = case
        .begin(case.start("capacity-0", "connection-a"))
        .unwrap();
    assert_eq!(replay.reply.fact, first.unwrap());
    assert!(replay.work.is_none());
    let replay = case
        .begin(case.start("capacity-1023", "connection-a"))
        .unwrap();
    assert_eq!(replay.reply.fact, active);
    assert!(replay.work.is_none());
    assert_code(
        case.begin(case.logout("capacity-0", "connection-a")),
        "native_auth_operation_conflict",
    );
    assert_code(
        case.begin(case.start("over-capacity", "connection-z")),
        "native_auth_operation_capacity",
    );
    assert_code(
        case.begin(case.logout("over-capacity-logout", "connection-z")),
        "native_auth_operation_capacity",
    );
    assert!(!case.profile("profile-z").exists());
    assert_eq!(case.snapshot(), before);
}

#[test]
fn generation_is_shared_by_intent_fence_work_and_actual_guard() {
    let mut case = HostCase::new();
    let (first, work) = fresh_work(
        case.begin(case.start("generation-1", "connection-a"))
            .unwrap(),
    );
    assert_eq!(first.generation, 1);
    let guard = case
        .custody
        .as_mut()
        .unwrap()
        .acquire_use(
            &case.started.host,
            &first.binding,
            &first.operation_id,
            first.generation,
            NativeProfileUsePurpose::Authentication,
        )
        .unwrap();
    guard
        .revalidate(
            &case.started.host,
            &first.binding,
            &first.operation_id,
            work.fact().generation,
            NativeProfileUsePurpose::Authentication,
        )
        .unwrap();
    drop(guard);
    case.finish(work).unwrap();
    let (second, _work) = fresh_work(
        case.begin(case.start("generation-2", "connection-a"))
            .unwrap(),
    );
    assert_eq!(second.generation, first.generation + 1);
    let (operations, fences) = case.snapshot();
    assert_eq!(operations.len(), 2);
    assert_eq!(
        fences[0].generation,
        i64::try_from(second.generation).unwrap()
    );
}

#[test]
fn stale_generation_cannot_publish_or_change_the_fence() {
    let mut case = HostCase::new();
    let (mut stale, _work) = fresh_work(
        case.begin(case.start("stale-publish", "connection-a"))
            .unwrap(),
    );
    stale.generation += 1;
    let before = case.snapshot();
    assert!(
        case.publish(&stale, NativeAuthHostUpdate::Unavailable)
            .is_err()
    );
    assert_eq!(case.snapshot(), before);
    assert!(case.db.is_autocommit());
}

#[test]
fn stale_generation_cannot_cancel_or_change_the_fence() {
    let mut case = HostCase::new();
    let (mut stale, _work) = fresh_work(
        case.begin(case.start("stale-cancel", "connection-a"))
            .unwrap(),
    );
    stale.generation += 1;
    let before = case.snapshot();
    assert!(case.cancel(&stale).is_err());
    assert_eq!(case.snapshot(), before);
    assert!(case.db.is_autocommit());
}

#[test]
fn wrong_operation_or_full_binding_cannot_publish_or_cancel() {
    for field in ["operation", "database", "custody", "connection", "revision"] {
        let mut case = HostCase::new();
        let (mut wrong, _work) = fresh_work(
            case.begin(case.start("wrong-identity", "connection-a"))
                .unwrap(),
        );
        match field {
            "operation" => wrong.operation_id = request_id("another-operation"),
            "database" => wrong.binding.database_instance = "f".repeat(64),
            "custody" => wrong.binding.custody_id = "e".repeat(64),
            "connection" => {
                wrong.binding.connection =
                    native_profile_connection(&case.config, &reference("connection-alias"))
                        .unwrap()
                        .clone()
            }
            "revision" => {
                wrong.binding.at_revision = Cursor::new(wrong.binding.at_revision.value() + 1)
            }
            _ => unreachable!(),
        }
        let before = case.snapshot();
        assert!(
            case.publish(&wrong, NativeAuthHostUpdate::Unavailable)
                .is_err(),
            "{field}"
        );
        assert!(case.cancel(&wrong).is_err(), "{field}");
        assert_eq!(case.snapshot(), before, "{field}");
    }
}

#[test]
fn changed_effective_config_blocks_publication_and_cancellation() {
    let mut case = HostCase::new();
    let (fact, _work) = fresh_work(
        case.begin(case.start("changed-config", "connection-a"))
            .unwrap(),
    );
    std::fs::write(
        &case.global,
        CATALOG.replace("timeout_ms = 1000", "timeout_ms = 1001"),
    )
    .unwrap();
    case.config = case.sources.config().unwrap();
    assert_ne!(case.config.id, fact.binding.config_id);
    let before = case.snapshot();
    assert!(
        case.publish(&fact, NativeAuthHostUpdate::Unavailable)
            .is_err()
    );
    assert!(case.cancel(&fact).is_err());
    assert_eq!(case.snapshot(), before);
}

#[test]
fn changed_source_with_same_config_blocks_publication_and_cancellation() {
    let mut case = HostCase::new();
    let (fact, _work) = fresh_work(
        case.begin(case.start("changed-source", "connection-a"))
            .unwrap(),
    );
    std::fs::write(&case.global, format!("\n{CATALOG}")).unwrap();
    assert_eq!(case.sources.config().unwrap().id, fact.binding.config_id);
    assert_ne!(
        settings::verified_source_hash(&case.config, Some(&case.sources)).unwrap(),
        fact.binding.source_hash
    );
    let before = case.snapshot();
    assert!(
        case.publish(&fact, NativeAuthHostUpdate::Unavailable)
            .is_err()
    );
    assert!(case.cancel(&fact).is_err());
    assert_eq!(case.snapshot(), before);
}

#[test]
fn new_engine_epoch_cannot_publish_or_cancel_the_old_generation() {
    let mut case = HostCase::new();
    let (fact, _work) = fresh_work(case.begin(case.start("old-epoch", "connection-a")).unwrap());
    let old_host = case.started.host.clone();
    owner_stop(&mut case.db, &old_host).unwrap();
    case.started = owner_start(&mut case.db).unwrap();
    assert_ne!(case.started.host.engine_epoch, fact.binding.engine_epoch);
    let before = case.snapshot();
    assert!(
        case.publish(&fact, NativeAuthHostUpdate::Unavailable)
            .is_err()
    );
    assert!(
        native_auth::publish_native_auth(
            &mut case.db,
            &case.config,
            Some(&case.sources),
            &old_host,
            &fact,
            NativeAuthHostUpdate::Unavailable
        )
        .is_err()
    );
    assert!(case.cancel(&fact).is_err());
    assert_eq!(case.snapshot(), before);
}

#[test]
fn revoked_origin_owner_blocks_publication_and_cancellation() {
    let mut case = HostCase::new();
    let (fact, _work) = fresh_work(
        case.begin(case.start("revoked-origin", "connection-a"))
            .unwrap(),
    );
    let control = OwnerControl {
        operation_id: request_id("revoke-host-owner"),
        target: OwnerClientKind::Tui,
        expected_epoch: case.started.owners[0].admission.owner_epoch,
        decision: OwnerDecision::Revoke,
    };
    owner_control(&mut case.db, &case.started.host, &control).unwrap();
    let before = case.snapshot();
    assert!(
        case.publish(&fact, NativeAuthHostUpdate::Unavailable)
            .is_err()
    );
    assert!(case.cancel(&fact).is_err());
    assert_eq!(case.snapshot(), before);
}

#[test]
fn cancellation_commits_cancelling_before_effects_and_keeps_the_fence() {
    let mut case = HostCase::new();
    let (fact, _work) = fresh_work(
        case.begin(case.start("actual-cancel-intent", "connection-a"))
            .unwrap(),
    );
    let fence = case.snapshot().1;
    let cancelled = case.cancel(&fact).unwrap();
    assert_eq!(cancelled.fact.phase, NativeAuthPhase::Cancelling);
    assert_eq!(cancelled.fact.operation_id, fact.operation_id);
    assert_eq!(cancelled.fact.generation, fact.generation);
    assert!(cancelled.challenge.is_none());
    assert!(cancelled.account.is_none());
    assert!(case.db.is_autocommit());
    let before = case.snapshot();
    assert_eq!(before.0[0].phase, "cancelling");
    assert_eq!(before.1, fence);
    assert_eq!(case.read(&fact).unwrap().fact, cancelled.fact);
    assert_eq!(case.cancel(&fact).unwrap().fact, cancelled.fact);
    assert_eq!(case.snapshot(), before);
}

#[test]
fn custodian_uncertainty_keeps_fence_after_the_real_lock_is_free() {
    let mut case = HostCase::new();
    let (fact, work) = fresh_work(
        case.begin(case.start("lost-custody", "connection-a"))
            .unwrap(),
    );
    let guard = case
        .custody
        .as_mut()
        .unwrap()
        .acquire_use(
            &case.started.host,
            &fact.binding,
            &fact.operation_id,
            fact.generation,
            NativeProfileUsePurpose::Authentication,
        )
        .unwrap();
    let probe = lock_probe(&case);
    assert!(flock(&probe, FlockOperation::NonBlockingLockExclusive).is_err());
    let uncertain = case
        .publish(&fact, NativeAuthHostUpdate::CustodyUncertain)
        .unwrap();
    assert_eq!(uncertain.phase, NativeAuthPhase::Uncertain);
    drop(guard);
    flock(&probe, FlockOperation::NonBlockingLockExclusive).unwrap();
    drop(probe);
    drop(work);
    let before = case.snapshot();
    assert_eq!(before.1.len(), 1);
    assert_code(
        case.begin(case.start("must-remain-fenced", "connection-alias")),
        "native_auth_recovery_required",
    );
    assert_eq!(case.snapshot(), before);
}

#[test]
fn database_reopen_keeps_old_work_fenced_while_held_and_the_next_begin_recovers_it() {
    let mut case = HostCase::new();
    let (fact, work) = fresh_work(
        case.begin(case.start("restart-fence", "connection-a"))
            .unwrap(),
    );
    let before = case.snapshot();
    drop(work);
    let mut case = case.reopen();
    let observed = case.read(&fact).unwrap();
    assert_eq!(observed.fact.phase, NativeAuthPhase::Uncertain);
    assert_eq!(observed.fact.generation, fact.generation);
    assert_eq!(case.snapshot().1, before.1);
    let probe = lock_probe(&case);
    flock(&probe, FlockOperation::NonBlockingLockExclusive).unwrap();
    let held = case.snapshot();
    assert_code(
        case.begin(case.start("held-elsewhere", "connection-z")),
        "native_auth_host_busy",
    );
    assert_eq!(case.snapshot(), held);
    drop(probe);
    let (elsewhere, work) = fresh_work(
        case.begin(case.start("released-elsewhere", "connection-z"))
            .unwrap(),
    );
    let recovered = case.read(&fact).unwrap().fact;
    assert_eq!(
        (recovered.phase, recovered.code.as_deref()),
        (NativeAuthPhase::Failed, Some("native_auth_interrupted"))
    );
    assert_eq!(recovered.generation, fact.generation);
    let fences = case.snapshot().1;
    assert_eq!(fences.len(), 1);
    assert_eq!(fences[0].operation_id, elsewhere.operation_id.as_str());
    case.finish(work).unwrap();
    fresh_work(
        case.begin(case.start("released-here", "connection-a"))
            .unwrap(),
    );
}

#[test]
fn a_terminal_unavailable_phase_alone_cannot_clear_the_fence() {
    let mut case = HostCase::new();
    let (fact, _work) = fresh_work(
        case.begin(case.start("terminal-with-custody", "connection-a"))
            .unwrap(),
    );
    let before_fence = case.snapshot().1;
    let unavailable = case
        .publish(&fact, NativeAuthHostUpdate::Unavailable)
        .unwrap();
    assert_eq!(unavailable.phase, NativeAuthPhase::Unavailable);
    assert_eq!(case.snapshot().1, before_fence);
    assert_code(
        case.begin(case.logout("terminal-is-not-retired", "connection-alias")),
        "native_auth_recovery_required",
    );
    assert_eq!(case.snapshot().1.len(), 1);
}

#[test]
fn consumed_never_dispatched_work_can_retire_its_no_effect_fence() {
    let mut case = HostCase::new();
    let (fact, work) = fresh_work(
        case.begin(case.start("no-effect-start", "connection-a"))
            .unwrap(),
    );
    let reply = case.finish(work).unwrap();
    assert_eq!(reply.fact.phase, NativeAuthPhase::Unavailable);
    assert_eq!(reply.fact.operation_id, fact.operation_id);
    assert_eq!(reply.fact.generation, fact.generation);
    assert!(reply.account.is_none());
    assert!(reply.challenge.is_none());
    assert!(case.db.is_autocommit());
    let (operations, fences) = case.snapshot();
    assert_eq!(operations.len(), 1);
    assert!(fences.is_empty());
    assert_eq!(case.read(&fact).unwrap().fact, reply.fact);
    let replay = case
        .begin(case.start("no-effect-start", "connection-a"))
        .unwrap();
    assert!(replay.work.is_none());
    assert_eq!(replay.reply.fact, reply.fact);
    let (next, _work) = fresh_work(
        case.begin(case.start("after-no-effect", "connection-a"))
            .unwrap(),
    );
    assert_eq!(next.generation, fact.generation + 1);
}

#[test]
fn no_effect_retirement_rejects_a_still_held_actual_use_guard() {
    let mut case = HostCase::new();
    let (fact, work) = fresh_work(
        case.begin(case.start("held-no-effect", "connection-a"))
            .unwrap(),
    );
    let guard = case
        .custody
        .as_mut()
        .unwrap()
        .acquire_use(
            &case.started.host,
            &fact.binding,
            &fact.operation_id,
            fact.generation,
            NativeProfileUsePurpose::Authentication,
        )
        .unwrap();
    let before = case.snapshot();
    assert!(case.finish(work).is_err());
    assert_eq!(case.snapshot(), before);
    drop(guard);
    assert_eq!(case.snapshot().1.len(), 1);
}

#[test]
fn no_effect_retirement_revalidates_the_retained_manifest_identity() {
    let mut case = HostCase::new();
    let (_fact, work) = fresh_work(
        case.begin(case.start("changed-manifest", "connection-a"))
            .unwrap(),
    );
    let manifest = case.profile("profile-a").join("identity.json");
    let retained = File::open(&manifest).unwrap();
    let identity = retained.metadata().unwrap().ino();
    std::fs::rename(&manifest, manifest.with_file_name("retained-identity.json")).unwrap();
    assert_eq!(retained.metadata().unwrap().ino(), identity);
    let before = case.snapshot();
    assert!(case.finish(work).is_err());
    assert_eq!(case.snapshot(), before);
}

#[test]
fn custody_uncertainty_invalidates_a_remaining_no_effect_work_value() {
    let mut case = HostCase::new();
    let (fact, work) = fresh_work(
        case.begin(case.start("uncertain-no-effect", "connection-a"))
            .unwrap(),
    );
    case.publish(&fact, NativeAuthHostUpdate::CustodyUncertain)
        .unwrap();
    let before = case.snapshot();
    assert!(case.finish(work).is_err());
    assert_eq!(case.snapshot(), before);
    assert_eq!(before.1.len(), 1);
}

#[test]
fn failed_no_effect_retirement_rolls_back_terminal_fact_and_fence() {
    let mut case = HostCase::new();
    let (_fact, work) = fresh_work(
        case.begin(case.start("retirement-rollback", "connection-a"))
            .unwrap(),
    );
    case.db.execute_batch("CREATE TRIGGER deny_host_retirement BEFORE DELETE ON native_auth_fences BEGIN SELECT RAISE(ABORT,'denied_retirement'); END;").unwrap();
    let before = case.snapshot();
    assert!(case.finish(work).is_err());
    assert!(case.db.is_autocommit());
    assert_eq!(case.snapshot(), before);
}

#[test]
fn logout_no_effect_retirement_preserves_its_real_intent_and_replay() {
    let mut case = HostCase::new();
    let (fact, work) = fresh_work(
        case.begin(case.logout("no-effect-logout", "connection-a"))
            .unwrap(),
    );
    let finished = case.finish(work).unwrap();
    assert_eq!(finished.fact.action, NativeAuthAction::Logout);
    assert_eq!(finished.fact.phase, NativeAuthPhase::Unavailable);
    assert_eq!(finished.fact.generation, fact.generation);
    assert!(case.snapshot().1.is_empty());
    let before = case.snapshot();
    let replay = case
        .begin(case.logout("no-effect-logout", "connection-a"))
        .unwrap();
    assert!(replay.work.is_none());
    assert_eq!(replay.reply.fact, finished.fact);
    assert_eq!(case.snapshot(), before);
}
