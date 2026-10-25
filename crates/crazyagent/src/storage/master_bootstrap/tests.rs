use crate::config::EffectiveConfig;
use crate::engine::owner::native_auth::Command as AuthCommand;
use crate::engine::owner::{
    NativeAuthFact, NativeAuthMethod, NativeAuthPhase, NativeAuthScope, NativeAuthStart, OwnerData,
    OwnerMutation, OwnerOperation, OwnerRead, OwnerRequest, OwnerResult, Started,
};
use crate::master_session::{MasterBackend, MasterOwnerAction, MasterSelection, MasterSessionView};
use crate::protocol::{
    Command, CredentialRef, Cursor, Outcome, PROTOCOL_VERSION, ProjectId, ProjectSpec, RequestId,
};
use crate::routing::{ResourceBudget, RouteRequest};
use crate::storage::{self, OpenedDatabase, native_auth, settings};
use crate::worktrees::{self, LeaseState, WorktreeLease};
use rusqlite::Connection;
use std::fs::File;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command as Process;

const CATALOG: &str = r#"version = 1
allowed_destinations = ["connection"]
[routing]
approved_accounts = ["account"]
task_capacity = { slots=1, cpu_millis=1000, memory_mib=512, processes=4, output_bytes=1048576, wall_time_ms=60000, spend_microunits=0 }
[model_access]
accounts = ["account"]
[[model_access.connections]]
id = "connection"
provider = "openai"
harness = "codex"
protocol = "native"
account_ref = "account"
endpoint = { kind="native" }
model = { id="configured-model", author="openai" }
auth = { method="native_managed", profile_ref="profile" }
[[model_access.pools]]
id = "pool"
members = ["connection"]
projects = ["project"]
roles = ["project_leader"]
fallback_allowed = false
"#;

fn reference(value: &str) -> CredentialRef {
    CredentialRef::new(value).unwrap()
}

fn git(repository: &Path, args: &[&str]) {
    let output = crate::spawn_gate::output(
        Process::new("/usr/bin/git")
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_TERMINAL_PROMPT", "0")
            .args(["-c", "core.hooksPath=/dev/null", "-C"])
            .arg(repository)
            .args(args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped()),
    )
    .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert!(output.stdout.len() <= 8192);
}

fn repository(root: &Path) -> PathBuf {
    let repository = root.join("repository");
    std::fs::create_dir(&repository).unwrap();
    std::fs::set_permissions(&repository, std::fs::Permissions::from_mode(0o700)).unwrap();
    git(&repository, &["init", "-b", "main"]);
    std::fs::write(repository.join("tracked"), b"committed\n").unwrap();
    git(&repository, &["add", "tracked"]);
    git(
        &repository,
        &[
            "-c",
            "user.name=Bootstrap Test",
            "-c",
            "user.email=bootstrap@invalid",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-m",
            "base",
        ],
    );
    assert!(repository.join(".git").is_dir());
    assert!(repository.join("tracked").is_file());
    repository
}

fn selection() -> MasterSelection {
    MasterSelection {
        backend: MasterBackend::Codex,
        pool_ref: reference("pool"),
        target_profile_ref: reference("connection"),
        route: RouteRequest {
            preferred_target: Some(reference("connection")),
            required_target: Some(reference("connection")),
            required_capabilities: vec![],
            account_ref: reference("account"),
            budget_ref: reference("conversation-budget"),
            resources: ResourceBudget::unit(),
        },
    }
}

struct Case {
    db: Connection,
    _lock: File,
    custody: Result<crate::engine::bootstrap::NativeProfileCustody, crate::engine::EngineError>,
    sources: settings::SettingsSources,
    config: EffectiveConfig,
    started: Started,
    session: MasterSessionView,
    repository: PathBuf,
    root: tempfile::TempDir,
}

impl Case {
    fn new() -> Self {
        let root = tempfile::Builder::new()
            .prefix("ca-mbp-")
            .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
            .unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let repository = repository(root.path());
        let global = root.path().join("global.toml");
        std::fs::write(&global, CATALOG).unwrap();
        std::fs::set_permissions(&global, std::fs::Permissions::from_mode(0o600)).unwrap();
        let sources =
            settings::SettingsSources::open(&global, &root.path().join("project.toml")).unwrap();
        let config = sources.config().unwrap();
        let path = root.path().join("engine.db");
        let OpenedDatabase {
            mut connection,
            lock,
            mut custody,
        } = storage::open_actor_database(&path, false).unwrap();
        let started = storage::owner_start(&mut connection).unwrap();
        let session = open(
            &mut connection,
            &config,
            &sources,
            &path,
            &started,
            &repository,
            &mut custody,
        );
        assert_eq!(session.owner.engine_epoch, started.host.engine_epoch);
        assert!(connection.is_autocommit());
        Self {
            db: connection,
            _lock: lock,
            custody,
            sources,
            config,
            started,
            session,
            repository,
            root,
        }
    }

    fn starting_auth(&mut self) -> NativeAuthFact {
        let begin = native_auth::begin_native_auth(
            &mut self.db,
            &self.config,
            Some(&self.sources),
            &mut self.custody,
            &self.started.owners[0].admission,
            AuthCommand::Start(NativeAuthStart {
                request_id: RequestId::new("actual-auth-start").unwrap(),
                scope: NativeAuthScope {
                    expected_database_instance: self.started.host.db_instance.clone(),
                    connection: reference("connection"),
                    expected_config_id: self.config.id.clone(),
                    expected_source_hash: self.session.source_hash.clone(),
                },
                method: NativeAuthMethod::CodexBrowser,
            }),
        )
        .unwrap();
        assert_eq!(begin.reply.fact.phase, NativeAuthPhase::Starting);
        assert!(begin.work.is_some());
        begin.reply.fact
    }
}

fn open(
    db: &mut Connection,
    config: &EffectiveConfig,
    sources: &settings::SettingsSources,
    path: &Path,
    started: &Started,
    repository: &Path,
    custody: &mut Result<
        crate::engine::bootstrap::NativeProfileCustody,
        crate::engine::EngineError,
    >,
) -> MasterSessionView {
    let project = ProjectId::new("project").unwrap();
    let request = OwnerRequest {
        protocol_version: PROTOCOL_VERSION,
        request_id: RequestId::new("actual-project-create").unwrap(),
        expected_revision: Cursor::new(
            storage::owner_revision(&db.transaction().unwrap()).unwrap(),
        ),
        operation: OwnerOperation::Apply {
            mutation: OwnerMutation::CreateProject {
                project_id: project.clone(),
                spec: ProjectSpec {
                    name: "Planning".into(),
                    repositories: vec![repository.to_str().unwrap().into()],
                },
            },
        },
    };
    let created =
        storage::owner_apply(db, config, path, &started.owners[0].admission, &request).unwrap();
    assert_eq!(created.result, OwnerResult::Applied);
    let request = OwnerRequest {
        protocol_version: PROTOCOL_VERSION,
        request_id: RequestId::new("actual-master-open").unwrap(),
        expected_revision: created.current_revision,
        operation: OwnerOperation::Apply {
            mutation: OwnerMutation::ProjectMaster {
                action: MasterOwnerAction::Open {
                    project_id: project.clone(),
                    expected_config_id: config.id.clone(),
                    expected_source_hash: settings::verified_source_hash(config, Some(sources))
                        .unwrap(),
                    selection: Box::new(selection()),
                },
            },
        },
    };
    let opened = storage::master_session::apply(
        db,
        config,
        Some(sources),
        custody,
        &started.host,
        &started.owners[0].admission,
        &request,
    )
    .unwrap();
    assert_eq!(opened.result, OwnerResult::Applied);
    let reply = storage::master_session::read(
        db,
        config,
        Some(sources),
        &started.owners[0].admission,
        &OwnerRead::ProjectMaster {
            project_id: project,
        },
    )
    .unwrap();
    let OwnerData::ProjectMaster {
        session: Some(session),
    } = reply.data
    else {
        panic!("actual master Open must produce a persisted session");
    };
    let session = *session;
    assert_eq!(session.owner, opened.provenance);
    session
}

fn leases(db: &Connection, session: &MasterSessionView) -> Vec<WorktreeLease> {
    let Outcome::Worktrees { leases } = worktrees::leases(db, &session.bootstrap_task_id).unwrap()
    else {
        panic!("actual lease query must return leases");
    };
    leases
}

#[test]
fn master_lease_primitive_materializes_committed_source_without_task_authority() {
    let mut case = Case::new();
    std::fs::write(case.repository.join("tracked"), b"owner dirty\n").unwrap();
    let repository = case.repository.to_str().unwrap();
    let tx = case.db.transaction().unwrap();
    let ordinary = Command::AllocateWorktree {
        task_id: case.session.bootstrap_task_id.clone(),
        repository: repository.into(),
        owner_session: case.session.session_id.clone(),
        native_descendant: None,
    };
    assert_eq!(
        worktrees::reserve(&tx, case.root.path(), &ordinary).unwrap(),
        Outcome::Rejected {
            code: "task_missing".into()
        }
    );
    assert_eq!(
        worktrees::admit(&tx, &case.session.project_id, repository).unwrap(),
        Outcome::Applied
    );
    assert_eq!(
        worktrees::reserve_master_bootstrap(
            &tx,
            case.root.path(),
            &case.session.project_id,
            &case.session.bootstrap_task_id,
            &case.session.session_id,
            repository
        )
        .unwrap(),
        Outcome::Applied
    );
    let reserved = leases(&tx, &case.session);
    assert_eq!(reserved.len(), 1);
    assert_eq!(reserved[0].state, LeaseState::Reserved);
    assert_eq!(reserved[0].owner_session, case.session.session_id);
    tx.commit().unwrap();
    worktrees::provision(&case.db, &case.session.bootstrap_task_id, repository).unwrap();
    let ready = leases(&case.db, &case.session);
    assert_eq!(ready.len(), 1);
    assert_eq!(ready[0].state, LeaseState::Ready);
    assert_eq!(ready[0].native_descendant, None);
    assert_eq!(
        std::fs::read(Path::new(&ready[0].path).join("tracked")).unwrap(),
        b"committed\n"
    );
    assert_eq!(
        std::fs::read(case.repository.join("tracked")).unwrap(),
        b"owner dirty\n"
    );
    let mode = std::fs::symlink_metadata(&ready[0].path)
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(
        mode & 0o077,
        0,
        "master worktree root must be private for the RCLI host cwd"
    );
    let tasks: i64 = case
        .db
        .query_row("SELECT COUNT(*) FROM tasks", [], |row| row.get(0))
        .unwrap();
    let auth: i64 = case
        .db
        .query_row("SELECT COUNT(*) FROM native_auth_operations", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(tasks, 0);
    assert_eq!(auth, 0);
}

#[test]
fn official_auth_start_does_not_authorize_bootstrap_receipt_or_lease() {
    let mut case = Case::new();
    let fact = case.starting_auth();
    let tx = case.db.transaction().unwrap();
    let error = super::reserve(
        &tx,
        case.root.path(),
        &case.session,
        case.repository.to_str().unwrap(),
        &fact,
    )
    .unwrap_err();
    assert_eq!(error.0, "master_bootstrap_authentication_stale");
    let error = super::ready(&tx, &case.session, &fact).unwrap_err();
    assert_eq!(error.0, "master_bootstrap_authentication_stale");
    assert!(leases(&tx, &case.session).is_empty());
    let receipts: i64 = tx
        .query_row("SELECT COUNT(*) FROM bootstrap_workspaces", [], |row| {
            row.get(0)
        })
        .unwrap();
    let tasks: i64 = tx
        .query_row("SELECT COUNT(*) FROM tasks", [], |row| row.get(0))
        .unwrap();
    assert_eq!(receipts, 0);
    assert_eq!(tasks, 0);
}

#[test]
fn bootstrap_initializer_rejects_conflicting_schema() {
    let root = tempfile::tempdir_in(std::fs::canonicalize("/tmp").unwrap()).unwrap();
    let db = Connection::open(root.path().join("conflict.db")).unwrap();
    db.execute_batch("CREATE TABLE bootstrap_workspaces (body TEXT)")
        .unwrap();
    let error = super::initialize(&db).unwrap_err();
    assert_eq!(error.0, "master_bootstrap_schema_conflict");
    let count: i64 = db
        .query_row("SELECT COUNT(*) FROM bootstrap_workspaces", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(count, 0);
}

#[test]
#[should_panic(expected = "bootstrap reservation requires an active transaction")]
fn bootstrap_reservation_asserts_its_transaction_invariant() {
    let mut case = Case::new();
    let fact = case.starting_auth();
    let tx = case.db.transaction().unwrap();
    tx.execute_batch("COMMIT").unwrap();
    let _ = super::reserve(
        &tx,
        case.root.path(),
        &case.session,
        case.repository.to_str().unwrap(),
        &fact,
    );
}
