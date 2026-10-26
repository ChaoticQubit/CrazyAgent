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
use crate::storage::{self, OpenedDatabase, harness_groups, native_auth, settings};
use crate::worktrees::{self, LeaseState, WorktreeLease};
use rusqlite::Connection;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
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
    _lock: storage::EngineLock,
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
        } = storage::open_actor_database(&path, false, None).unwrap();
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

    /// Stops this engine run and starts the next on the same data root, the way an app
    /// update does: the database is reopened, the worktrees are reconciled and the owner
    /// starts under a new engine epoch. Input: the case. Output: the case on the new run.
    fn restart(self) -> Self {
        let Case {
            db,
            _lock,
            custody,
            sources,
            config,
            started,
            session,
            repository,
            root,
        } = self;
        drop(custody);
        drop(db);
        drop(_lock);
        let OpenedDatabase {
            mut connection,
            lock,
            custody,
        } = storage::open_actor_database(&root.path().join("engine.db"), false, None).unwrap();
        let restarted = storage::owner_start(&mut connection).unwrap();
        assert_ne!(restarted.host.engine_epoch, started.host.engine_epoch);
        Self {
            db: connection,
            _lock: lock,
            custody,
            sources,
            config,
            started: restarted,
            session,
            repository,
            root,
        }
    }

    /// Gives the session its conversation worktree through the real lease primitives.
    /// Input: the case. Output: none; the session holds one Ready lease.
    fn ready_lease(&mut self) {
        let repository = self.repository.to_str().unwrap().to_owned();
        let tx = self.db.transaction().unwrap();
        assert_eq!(
            worktrees::admit(&tx, &self.session.project_id, &repository).unwrap(),
            Outcome::Applied
        );
        assert_eq!(
            worktrees::reserve_master_bootstrap(
                &tx,
                self.root.path(),
                &self.session.project_id,
                &self.session.bootstrap_task_id,
                &self.session.session_id,
                &repository
            )
            .unwrap(),
            Outcome::Applied
        );
        tx.commit().unwrap();
        worktrees::provision(&self.db, &self.session.bootstrap_task_id, &repository).unwrap();
        assert_eq!(self.lease_state(), LeaseState::Ready);
    }

    fn lease_state(&self) -> LeaseState {
        let leases = leases(&self.db, &self.session);
        assert_eq!(leases.len(), 1);
        leases[0].state.clone()
    }

    /// Sends one owner message to the open session on the current run.
    /// Input: the request id. Output: the owner result of the send.
    fn submit(&mut self, id: &str) -> OwnerResult {
        let action = MasterOwnerAction::Submit {
            project_id: self.session.project_id.clone(),
            session_id: self.session.session_id.clone(),
            text: "Continue after the restart.".into(),
        };
        self.act(id, action)
    }

    /// Cancels the open session's current turn on the current run.
    /// Inputs: the request id and the turn. Output: the owner result of the cancel.
    fn cancel(&mut self, id: &str, turn: &str) -> OwnerResult {
        let action = MasterOwnerAction::Cancel {
            project_id: self.session.project_id.clone(),
            session_id: self.session.session_id.clone(),
            turn_id: RequestId::new(turn).unwrap(),
        };
        self.act(id, action)
    }

    /// Applies one owner master action through the real owner path.
    /// Inputs: the request id and the action. Output: the owner result.
    fn act(&mut self, id: &str, action: MasterOwnerAction) -> OwnerResult {
        let request = OwnerRequest {
            protocol_version: PROTOCOL_VERSION,
            request_id: RequestId::new(id).unwrap(),
            expected_revision: Cursor::new(
                storage::owner_revision(&self.db.transaction().unwrap()).unwrap(),
            ),
            operation: OwnerOperation::Apply {
                mutation: OwnerMutation::ProjectMaster { action },
            },
        };
        storage::master_session::apply(
            &mut self.db,
            &self.config,
            Some(&self.sources),
            &mut self.custody,
            &self.started.host,
            &self.started.owners[0].admission,
            &request,
        )
        .unwrap()
        .result
    }

    /// Announces a turn's harness launch before any child starts, as profile acquisition
    /// does. Input: the turn. Output: none; the launch is recorded with an unknown group.
    fn launching(&mut self, turn: &str) {
        let tx = self.db.transaction().unwrap();
        harness_groups::launching(
            &tx,
            &self.session.session_id,
            &RequestId::new(turn).unwrap(),
        )
        .unwrap();
        tx.commit().unwrap();
    }

    /// Announces a turn's launch and records the group its harness started in.
    /// Inputs: the turn and group. Output: none; the group is recorded.
    fn record_child(&mut self, turn: &str, group: i32) {
        self.launching(turn);
        let tx = self.db.transaction().unwrap();
        harness_groups::record(
            &tx,
            &self.session.session_id,
            &RequestId::new(turn).unwrap(),
            group,
        )
        .unwrap();
        tx.commit().unwrap();
    }

    fn children(&self) -> i64 {
        self.db
            .query_row(
                "SELECT COUNT(*) FROM harness_groups WHERE session=?1",
                [self.session.session_id.as_str()],
                |row| row.get(0),
            )
            .unwrap()
    }
}

/// Starts a real `sleep` leading its own process group, the way the engine starts a
/// master harness. Input: none. Output: the child and its group.
fn harness_group() -> (std::process::Child, i32) {
    let mut command = Process::new("/bin/sleep");
    command
        .arg("30")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .process_group(0);
    let child = crate::spawn_gate::spawn(&mut command).unwrap();
    let group = i32::try_from(child.id()).unwrap();
    (child, group)
}

fn end(mut child: std::process::Child) {
    let _ = child.kill();
    child.wait().unwrap();
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
fn a_master_lease_whose_worktree_gains_a_config_worktree_is_no_longer_ready() {
    let mut case = Case::new();
    let repository = case.repository.to_str().unwrap();
    let tx = case.db.transaction().unwrap();
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
    tx.commit().unwrap();
    worktrees::provision(&case.db, &case.session.bootstrap_task_id, repository).unwrap();
    let ready = leases(&case.db, &case.session);
    assert_eq!(ready[0].state, LeaseState::Ready);
    let pointer = std::fs::read_to_string(Path::new(&ready[0].path).join(".git")).unwrap();
    let metadata = PathBuf::from(
        pointer
            .strip_prefix("gitdir: ")
            .and_then(|path| path.strip_suffix('\n'))
            .unwrap(),
    );
    assert!(metadata.starts_with(case.repository.join(".git/worktrees")));
    std::fs::write(
        metadata.join("config.worktree"),
        b"[core]\n\thooksPath = /private/tmp\n",
    )
    .unwrap();
    assert_eq!(
        leases(&case.db, &case.session)[0].state,
        LeaseState::Uncertain
    );
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

#[test]
fn a_send_after_a_restart_returns_a_worktree_no_harness_ran_in_to_ready() {
    let mut case = Case::new();
    case.ready_lease();
    let mut case = case.restart();
    assert_eq!(case.lease_state(), LeaseState::RecoveryRequired);
    assert_eq!(case.submit("send-after-restart"), OwnerResult::Applied);
    assert_eq!(case.lease_state(), LeaseState::Ready);
}

#[test]
fn a_send_after_a_restart_returns_the_worktree_once_the_recorded_harness_group_is_gone() {
    let mut case = Case::new();
    case.ready_lease();
    let (child, group) = harness_group();
    case.record_child("earlier-turn", group);
    end(child);
    let mut case = case.restart();
    assert_eq!(case.children(), 1);
    assert_eq!(case.submit("send-after-gone"), OwnerResult::Applied);
    assert_eq!(case.lease_state(), LeaseState::Ready);
    assert_eq!(case.children(), 0);
}

#[test]
fn a_send_after_a_restart_while_a_recorded_harness_group_runs_asks_for_a_new_conversation() {
    let mut case = Case::new();
    case.ready_lease();
    let (child, group) = harness_group();
    case.record_child("earlier-turn", group);
    let mut case = case.restart();
    let sent = case.submit("send-while-live");
    let state = case.lease_state();
    let children = case.children();
    end(child);
    assert_eq!(
        sent,
        OwnerResult::Rejected {
            code: "master_source_conflict".into()
        }
    );
    assert_eq!(state, LeaseState::RecoveryRequired);
    assert_eq!(children, 1);
}

#[test]
fn a_send_after_a_restart_when_a_launch_never_recorded_its_group_asks_for_a_new_conversation() {
    let mut case = Case::new();
    case.ready_lease();
    case.launching("launch-without-group");
    let mut case = case.restart();
    assert_eq!(
        case.submit("send-after-unrecorded"),
        OwnerResult::Rejected {
            code: "master_source_conflict".into()
        }
    );
    assert_eq!(case.lease_state(), LeaseState::RecoveryRequired);
    assert_eq!(case.children(), 1);
}

#[test]
fn a_group_recorded_for_a_turn_that_never_announced_its_launch_is_refused() {
    let mut case = Case::new();
    let (child, group) = harness_group();
    let tx = case.db.transaction().unwrap();
    let recorded = harness_groups::record(
        &tx,
        &case.session.session_id,
        &RequestId::new("unannounced-turn").unwrap(),
        group,
    );
    drop(tx);
    end(child);
    assert_eq!(recorded.unwrap_err().0, "harness_group_unannounced");
    assert_eq!(case.children(), 0);
}

#[test]
fn a_later_launch_forgets_earlier_groups_that_are_gone_and_keeps_live_ones() {
    let mut case = Case::new();
    let (gone, gone_group) = harness_group();
    case.record_child("earlier-turn", gone_group);
    end(gone);
    let (live, live_group) = harness_group();
    case.record_child("later-turn", live_group);
    let left = case.children();
    let (third, third_group) = harness_group();
    case.record_child("third-turn", third_group);
    let after = case.children();
    end(live);
    end(third);
    assert_eq!(left, 1);
    assert_eq!(after, 2);
}

#[test]
fn a_session_with_turns_from_before_launch_records_counts_as_launched_with_an_unknown_group() {
    let mut case = Case::new();
    assert_eq!(case.submit("turn-before-records"), OwnerResult::Applied);
    assert_eq!(
        case.cancel("cancel-before-records", "turn-before-records"),
        OwnerResult::Applied
    );
    case.db.execute_batch("DROP TABLE harness_groups").unwrap();
    harness_groups::initialize(&case.db).unwrap();
    let unknown: i64 = case
        .db
        .query_row(
            "SELECT COUNT(*) FROM harness_groups WHERE session=?1 AND turn='turn-before-records' AND process_group IS NULL",
            [case.session.session_id.as_str()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(unknown, 1);
    let tx = case.db.transaction().unwrap();
    assert!(!harness_groups::quiescent(&tx, &case.session.session_id).unwrap());
}
