use super::*;
use crate::engine::owner::{ModelAccessCollection, Started};
use crate::master_session::MasterSelection;
use crate::master_sessions::{MasterSessionSummary, ResumePlan, SESSIONS_LISTED_MAX};
use crate::protocol::{CredentialRef, ProjectSpec};
use crate::routing::{ResourceBudget, RouteRequest};
use crate::storage::{Job, Storage};
use std::os::unix::fs::PermissionsExt;
use tokio::sync::oneshot;

const CONFIG: &str = r#"version = 1
timeout_ms = 2000
allowed_destinations = ["session-profile"]
[routing]
approved_accounts = ["session-account", "native-account"]
[model_access]
accounts = ["session-account", "native-account"]
[[model_access.connections]]
id = "session-connection"
provider = "local-provider"
harness = "rcli"
protocol = "openai-chat"
account_ref = "session-account"
endpoint = { kind = "http", url = "http://127.0.0.1:11434/v1" }
model = { id = "local-model", author = "local-author" }
auth = { method = "none" }
[[model_access.connections]]
id = "native-connection"
provider = "anthropic"
harness = "claude-code"
protocol = "native"
account_ref = "native-account"
endpoint = { kind = "native" }
model = { id = "configured-claude-model", author = "anthropic" }
auth = { method = "native_managed", profile_ref = "native-profile" }
[[model_access.pools]]
id = "session-pool"
members = ["session-connection"]
projects = ["session-project"]
roles = ["project_leader"]
fallback_allowed = false
[[model_access.pools]]
id = "native-pool"
members = ["native-connection"]
projects = ["session-project"]
roles = ["project_leader"]
fallback_allowed = false
"#;

struct Case {
    storage: Storage,
    started: Started,
    project: ProjectId,
    root: tempfile::TempDir,
}

fn reference(value: &str) -> CredentialRef {
    CredentialRef::new(value).unwrap()
}

async fn start(root: &Path) -> (Storage, Started) {
    let (storage, _) = Storage::open_with_settings_sources(
        &root.join("engine.db"),
        &root.join("global.toml"),
        &root.join("project.toml"),
    )
    .await
    .unwrap();
    let (send, receive) = oneshot::channel();
    storage
        .sender
        .try_send(Job::OwnerStart(send))
        .unwrap_or_else(|_| panic!("owner start queue unavailable"));
    let started = receive.await.unwrap().unwrap();
    (storage, started)
}

impl Case {
    async fn new() -> Self {
        let root = tempfile::Builder::new()
            .prefix("ca-ses-")
            .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
            .unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let global = root.path().join("global.toml");
        std::fs::write(&global, CONFIG).unwrap();
        std::fs::set_permissions(&global, std::fs::Permissions::from_mode(0o600)).unwrap();
        let repository = root.path().join("repository");
        std::fs::create_dir(&repository).unwrap();
        std::fs::write(repository.join("README.md"), "Session repository\n").unwrap();
        let (storage, started) = start(root.path()).await;
        let case = Self {
            storage,
            started,
            project: ProjectId::new("session-project").unwrap(),
            root,
        };
        let created = case
            .apply(
                "create-session-project",
                OwnerMutation::CreateProject {
                    project_id: case.project.clone(),
                    spec: ProjectSpec {
                        name: "Session project".into(),
                        repositories: vec![repository.to_str().unwrap().into()],
                    },
                },
            )
            .await;
        assert_eq!(created, OwnerResult::Applied);
        case
    }

    async fn read(&self, query: OwnerRead) -> OwnerReadReply {
        let (send, receive) = oneshot::channel();
        self.storage
            .sender
            .try_send(Job::OwnerRead(
                self.started.owners[0].admission.clone(),
                query,
                send,
            ))
            .unwrap_or_else(|_| panic!("owner read queue unavailable"));
        receive.await.unwrap().unwrap()
    }

    async fn apply(&self, id: &str, mutation: OwnerMutation) -> OwnerResult {
        let basis = self.read(OwnerRead::Capabilities).await;
        let (send, receive) = oneshot::channel();
        self.storage
            .sender
            .try_send(Job::OwnerApply(
                self.started.owners[0].admission.clone(),
                OwnerRequest {
                    protocol_version: PROTOCOL_VERSION,
                    request_id: RequestId::new(id).unwrap(),
                    expected_revision: basis.revision,
                    operation: OwnerOperation::Apply { mutation },
                },
                send,
            ))
            .unwrap_or_else(|_| panic!("owner apply queue unavailable"));
        receive.await.unwrap().unwrap().result
    }

    async fn master(&self, id: &str, action: MasterOwnerAction) -> OwnerResult {
        self.apply(id, OwnerMutation::ProjectMaster { action })
            .await
    }

    async fn basis(&self) -> (ConfigId, String, Box<MasterSelection>) {
        self.basis_for(MasterBackend::Rcli).await
    }

    async fn basis_for(&self, backend: MasterBackend) -> (ConfigId, String, Box<MasterSelection>) {
        let (pool, account) = match backend {
            MasterBackend::Rcli => ("session-pool", "session-account"),
            MasterBackend::Codex | MasterBackend::Claude => ("native-pool", "native-account"),
        };
        let basis = self
            .read(OwnerRead::ModelAccessPage {
                collection: ModelAccessCollection::Pools,
                cursor: None,
                limit: 1,
            })
            .await;
        let OwnerData::ModelAccessPage { source_hash, .. } = basis.data else {
            panic!("model source basis missing");
        };
        let target = reference("session-profile");
        let selection = MasterSelection {
            backend,
            pool_ref: reference(pool),
            target_profile_ref: target.clone(),
            route: RouteRequest {
                preferred_target: Some(target.clone()),
                required_target: Some(target),
                required_capabilities: vec![],
                account_ref: reference(account),
                budget_ref: reference("session-budget"),
                resources: ResourceBudget::unit(),
            },
        };
        (basis.config_id, source_hash, Box::new(selection))
    }

    async fn open(&self, id: &str) -> OwnerResult {
        let (expected_config_id, expected_source_hash, selection) = self.basis().await;
        self.master(
            id,
            MasterOwnerAction::Open {
                project_id: self.project.clone(),
                expected_config_id,
                expected_source_hash,
                selection,
            },
        )
        .await
    }

    async fn renew(&self, id: &str, cancel_running: bool) -> OwnerResult {
        self.renew_with(id, cancel_running, MasterBackend::Rcli)
            .await
    }

    async fn renew_with(
        &self,
        id: &str,
        cancel_running: bool,
        backend: MasterBackend,
    ) -> OwnerResult {
        let (expected_config_id, expected_source_hash, selection) = self.basis_for(backend).await;
        self.master(
            id,
            MasterOwnerAction::NewSession {
                project_id: self.project.clone(),
                expected_config_id,
                expected_source_hash,
                selection,
                cancel_running,
            },
        )
        .await
    }

    async fn resume(&self, id: &str, session: &SessionId, cancel_running: bool) -> OwnerResult {
        self.master(
            id,
            MasterOwnerAction::Resume {
                project_id: self.project.clone(),
                session_id: session.clone(),
                cancel_running,
            },
        )
        .await
    }

    async fn submit(&self, id: &str, session: &SessionId, text: &str) -> OwnerResult {
        self.master(
            id,
            MasterOwnerAction::Submit {
                project_id: self.project.clone(),
                session_id: session.clone(),
                text: text.into(),
            },
        )
        .await
    }

    async fn shown(&self) -> MasterSessionView {
        let state = self
            .read(OwnerRead::ProjectMaster {
                project_id: self.project.clone(),
            })
            .await;
        let OwnerData::ProjectMaster {
            session: Some(session),
        } = state.data
        else {
            panic!("a master session is shown");
        };
        *session
    }

    async fn sessions(&self) -> Vec<MasterSessionSummary> {
        let state = self
            .read(OwnerRead::ProjectMasterSessions {
                project_id: self.project.clone(),
            })
            .await;
        let OwnerData::ProjectMasterSessions { sessions } = state.data else {
            panic!("the session list is read");
        };
        sessions
    }

    async fn turns(&self, session: &SessionId) -> Vec<MasterTurnView> {
        let state = self
            .read(OwnerRead::ProjectMasterTurns {
                project_id: self.project.clone(),
                session_id: session.clone(),
                cursor: None,
                limit: 32,
            })
            .await;
        let OwnerData::ProjectMasterTurns { page } = state.data else {
            panic!("the session's turns are read");
        };
        page.turns
    }

    async fn reopen(self) -> Self {
        self.reopen_with(None).await
    }

    async fn reopen_with(self, settings: Option<&str>) -> Self {
        let project = self.project.clone();
        let root = self.close().await;
        if let Some(settings) = settings {
            let global = root.path().join("global.toml");
            std::fs::write(&global, settings).unwrap();
            std::fs::set_permissions(&global, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        let (storage, started) = start(root.path()).await;
        Self {
            storage,
            started,
            project,
            root,
        }
    }

    async fn close(self) -> tempfile::TempDir {
        let (send, receive) = oneshot::channel();
        self.storage
            .sender
            .try_send(Job::OwnerStop(self.started.host, send))
            .unwrap_or_else(|_| panic!("owner stop queue unavailable"));
        receive.await.unwrap().unwrap();
        let (send, receive) = oneshot::channel();
        self.storage
            .sender
            .try_send(Job::Close(send))
            .unwrap_or_else(|_| panic!("storage close queue unavailable"));
        receive.await.unwrap().unwrap();
        self.root
    }

    fn db(&self) -> Connection {
        let db = Connection::open(self.root.path().join("engine.db")).unwrap();
        db.busy_timeout(std::time::Duration::from_secs(2)).unwrap();
        db
    }

    fn phases(&self) -> Vec<(String, String)> {
        let db = self.db();
        let mut statement = db
            .prepare("SELECT id,phase FROM master_sessions WHERE project=?1 ORDER BY created")
            .unwrap();
        statement
            .query_map([self.project.as_str()], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    }
}

fn snapshot(case: &Case) -> Vec<(String, String, i64, String)> {
    let db = case.db();
    let mut statement = db
        .prepare("SELECT id,phase,version,body FROM master_sessions ORDER BY created")
        .unwrap();
    statement
        .query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

fn live_count(phases: &[(String, String)]) -> usize {
    phases
        .iter()
        .filter(|(_, phase)| phase == "configured")
        .count()
}

#[tokio::test]
async fn new_session_parks_the_live_one_and_opens_a_fresh_one_atomically() {
    let case = Case::new().await;
    assert_eq!(case.open("open-first").await, OwnerResult::Applied);
    let first = case.shown().await;
    assert_eq!(
        case.submit(
            "first-message",
            &first.session_id,
            "Plan the onboarding page"
        )
        .await,
        OwnerResult::Applied
    );
    let before = case.phases();
    let refused = case.renew("renew-unconfirmed", false).await;
    assert_eq!(
        refused,
        OwnerResult::Rejected {
            code: "master_turn_running".into()
        }
    );
    assert_eq!(
        case.phases(),
        before,
        "a refused New session changes nothing"
    );
    let queued = case.turns(&first.session_id).await;
    assert_eq!(queued.len(), 1);
    assert_eq!(queued[0].phase, MasterTurnPhase::Queued);

    assert_eq!(
        case.renew("renew-confirmed", true).await,
        OwnerResult::Applied
    );
    let phases = case.phases();
    assert_eq!(phases.len(), 2);
    assert_eq!(live_count(&phases), 1);
    assert_eq!(
        phases[0],
        (first.session_id.as_str().into(), "parked".into())
    );
    assert_eq!(phases[1].1, "configured");
    let second = case.shown().await;
    assert_eq!(second.session_id.as_str(), phases[1].0);
    assert_eq!(second.phase, MasterSessionPhase::Configured);
    assert_eq!(second.selection, first.selection);
    let cancelled = case.turns(&first.session_id).await;
    assert_eq!(cancelled[0].phase, MasterTurnPhase::Cancelled);
    assert_eq!(cancelled[0].reason, Some(MasterReason::Cancelled));

    let listed = case.sessions().await;
    assert_eq!(listed.len(), 2);
    assert_eq!(listed[0].session_id, second.session_id);
    assert_eq!(listed[0].phase, MasterSessionPhase::Configured);
    assert_eq!(listed[0].turns, 0);
    assert!(listed[0].started_ms.is_some());
    assert_eq!(listed[1].session_id, first.session_id);
    assert_eq!(listed[1].phase, MasterSessionPhase::Parked);
    assert_eq!(listed[1].turns, 1);
    assert_eq!(
        listed[1].first_line.as_deref(),
        Some("Plan the onboarding page")
    );
    assert_eq!(
        listed[1].last_line.as_deref(),
        Some("Plan the onboarding page")
    );
    assert_eq!(listed[1].results_waiting, 0);

    let again = case.open("open-while-live").await;
    assert_eq!(
        again,
        OwnerResult::Rejected {
            code: "master_session_active".into()
        },
        "Open never parks a live session; only New session does"
    );
    case.close().await;
}

#[tokio::test]
async fn resume_swaps_the_live_session_atomically_and_keeps_every_turn() {
    let case = Case::new().await;
    assert_eq!(case.open("open-first").await, OwnerResult::Applied);
    let first = case.shown().await;
    assert_eq!(
        case.master(
            "close-first",
            MasterOwnerAction::Close {
                project_id: case.project.clone(),
                session_id: first.session_id.clone(),
            },
        )
        .await,
        OwnerResult::Applied
    );
    let ended = case.shown().await;
    assert_eq!(ended.session_id, first.session_id);
    assert_eq!(
        ended.phase,
        MasterSessionPhase::Parked,
        "End conversation parks, so the session stays resumable"
    );
    assert_eq!(
        case.renew("renew-after-end", false).await,
        OwnerResult::Applied
    );
    let second = case.shown().await;
    assert_ne!(second.session_id, first.session_id);
    assert_eq!(
        case.submit(
            "second-message",
            &second.session_id,
            "Write the release notes"
        )
        .await,
        OwnerResult::Applied
    );

    let before = case.phases();
    assert_eq!(
        case.resume("resume-unconfirmed", &first.session_id, false)
            .await,
        OwnerResult::Rejected {
            code: "master_turn_running".into()
        }
    );
    assert_eq!(case.phases(), before);
    assert_eq!(
        case.resume("resume-confirmed", &first.session_id, true)
            .await,
        OwnerResult::Applied
    );
    let resumed = case.shown().await;
    assert_eq!(resumed.session_id, first.session_id);
    assert_eq!(resumed.phase, MasterSessionPhase::Configured);
    assert_eq!(resumed.current_turn, None);
    let phases = case.phases();
    assert_eq!(live_count(&phases), 1);
    assert_eq!(
        phases[1],
        (second.session_id.as_str().into(), "parked".into())
    );
    let kept = case.turns(&second.session_id).await;
    assert_eq!(kept.len(), 1, "the parked session keeps its turn");
    assert_eq!(kept[0].phase, MasterTurnPhase::Cancelled);

    assert_eq!(
        case.resume("resume-live", &first.session_id, false).await,
        OwnerResult::Rejected {
            code: "master_session_live".into()
        }
    );
    assert_eq!(
        case.submit("resumed-message", &first.session_id, "Continue the plan")
            .await,
        OwnerResult::Applied,
        "a resumed session takes new messages"
    );
    let listed = case.sessions().await;
    assert_eq!(listed[0].session_id, first.session_id);
    assert_eq!(listed[0].turns, 1);
    assert_eq!(listed[1].session_id, second.session_id);
    assert_eq!(
        listed[1].first_line.as_deref(),
        Some("Write the release notes")
    );

    let db = case.db();
    let ResumePlan::Fresh { seed, note } = crate::master_sessions::resume_harness(
        &db,
        &second.project_id,
        &second.session_id,
        "codex",
        "conn-1",
    )
    .unwrap() else {
        panic!("a session with earlier turns and no kept conversation starts fresh");
    };
    assert!(seed.contains("Owner: Write the release notes"), "{seed}");
    assert!(seed.contains("(No reply was received.)"), "{seed}");
    assert_eq!(note, crate::master_sessions::FRESH_NOTE);
    case.close().await;
}

#[tokio::test]
async fn the_session_list_is_bounded_and_newest_activity_first() {
    let case = Case::new().await;
    assert_eq!(case.open("open-0").await, OwnerResult::Applied);
    let total = SESSIONS_LISTED_MAX + 2;
    for index in 1..total {
        assert_eq!(
            case.renew(&format!("renew-{index}"), false).await,
            OwnerResult::Applied,
            "renew {index}"
        );
    }
    let phases = case.phases();
    assert_eq!(phases.len(), total);
    assert_eq!(live_count(&phases), 1);
    let oldest = SessionId::new(phases[0].0.clone()).unwrap();
    assert_eq!(
        case.resume("resume-oldest", &oldest, false).await,
        OwnerResult::Applied
    );
    let listed = case.sessions().await;
    assert_eq!(listed.len(), SESSIONS_LISTED_MAX);
    assert_eq!(listed[0].session_id, oldest, "the live session comes first");
    assert_eq!(listed[0].phase, MasterSessionPhase::Configured);
    assert_eq!(
        listed[1].session_id.as_str(),
        phases[total - 1].0,
        "the session parked by the resume was active last"
    );
    for pair in listed[1..].windows(2) {
        assert!(
            (pair[0].last_activity.value(), pair[0].created.value())
                > (pair[1].last_activity.value(), pair[1].created.value())
        );
    }
    assert!(
        listed
            .iter()
            .skip(1)
            .all(|summary| summary.phase == MasterSessionPhase::Parked)
    );
    assert!(serde_json::to_vec(&listed).unwrap().len() <= crate::master_sessions::LIST_BYTES_MAX);
    case.close().await;
}

#[tokio::test]
async fn results_of_a_parked_session_wait_for_it_and_its_agents_are_its_own() {
    let case = Case::new().await;
    assert_eq!(case.open("open-first").await, OwnerResult::Applied);
    let first = case.shown().await;
    assert_eq!(
        case.renew("renew-second", false).await,
        OwnerResult::Applied
    );
    let second = case.shown().await;
    let db = case.db();
    db.execute(
        "INSERT INTO team_agents(project,role,harness,tools,status,created_ms,heartbeat_ms,session) VALUES (?1,'sub','codex',0,'idle',1,1,?2)",
        params![case.project.as_str(), first.session_id.as_str()],
    )
    .unwrap();
    let agent = db.last_insert_rowid();
    crate::team::names::assign_persona(&db, &case.project, agent).unwrap();
    for (prompt, status, delivered) in [
        ("Draft the copy", "done", 0),
        ("Check the links", "failed", 0),
        ("Old step", "done", 1),
        ("Still running", "running", 0),
    ] {
        db.execute(
            "INSERT INTO team_tasks(project,source,harness,tools,prompt,status,agent,result,delivered,created_ms,finished_ms,session) VALUES (?1,'master','codex',0,?2,?3,?4,'copy drafted',?5,1,2,?6)",
            params![case.project.as_str(), prompt, status, agent, delivered, first.session_id.as_str()],
        )
        .unwrap();
    }
    let listed = case.sessions().await;
    let parked = listed
        .iter()
        .find(|summary| summary.session_id == first.session_id)
        .unwrap();
    assert_eq!(parked.results_waiting, 2);
    assert_eq!(parked.agents.len(), 1);
    let live = listed
        .iter()
        .find(|summary| summary.session_id == second.session_id)
        .unwrap();
    assert_eq!(live.results_waiting, 0);
    assert!(live.agents.is_empty());

    let state = crate::master_sessions::session_state(&db, &case.project, &first.session_id)
        .unwrap()
        .unwrap();
    assert!(state.contains(&parked.agents[0]), "{state}");
    assert!(state.contains("Last result: copy drafted"), "{state}");
    assert!(state.contains("Still running"), "{state}");
    assert_eq!(
        crate::master_sessions::session_state(&db, &case.project, &second.session_id)
            .unwrap()
            .map(|state| state.contains(&parked.agents[0])),
        Some(false),
        "another session's agents are not listed as its own"
    );
    case.close().await;
}

#[tokio::test]
async fn upgrading_a_version_3_database_makes_ended_sessions_resumable() {
    let case = Case::new().await;
    assert_eq!(case.open("open-zero").await, OwnerResult::Applied);
    assert_eq!(
        case.renew_with("open-first", false, MasterBackend::Claude)
            .await,
        OwnerResult::Applied
    );
    let first = case.shown().await;
    assert_eq!(first.selection.backend, MasterBackend::Claude);
    assert_eq!(
        case.submit(
            "first-message",
            &first.session_id,
            "Sketch the settings page"
        )
        .await,
        OwnerResult::Applied
    );
    assert_eq!(case.renew("renew-second", true).await, OwnerResult::Applied);
    let second = case.shown().await;
    let root = case.close().await;
    let db = Connection::open(root.path().join("engine.db")).unwrap();
    db.execute_batch(&format!(
        "ALTER TABLE master_sessions RENAME TO master_sessions_now;
         {UNPARKED_SESSIONS};
         INSERT INTO master_sessions SELECT id,project,created,version,
             CASE phase WHEN 'parked' THEN 'closed' ELSE phase END,
             CASE phase WHEN 'parked' THEN json_set(body,'$.phase','closed') ELSE body END
             FROM master_sessions_now;
         DROP TABLE master_sessions_now;
         {CURRENT}; {LATEST};
         ALTER TABLE team_tasks DROP COLUMN session;
         ALTER TABLE team_agents DROP COLUMN session;
         PRAGMA user_version = 3;"
    ))
    .unwrap();
    let closed: String = db
        .query_row(
            "SELECT phase FROM master_sessions WHERE id=?1",
            [first.session_id.as_str()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(closed, "closed");
    drop(db);

    let (storage, started) = start(root.path()).await;
    let case = Case {
        storage,
        started,
        project: ProjectId::new("session-project").unwrap(),
        root,
    };
    let db = case.db();
    let version: i64 = db
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, 4);
    let schema: String = db
        .query_row(
            "SELECT sql FROM sqlite_schema WHERE name='master_sessions'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(schema, SESSIONS);
    for table in ["team_tasks", "team_agents"] {
        let present: bool = db
            .query_row(
                &format!(
                    "SELECT EXISTS(SELECT 1 FROM pragma_table_info('{table}') WHERE name='session')"
                ),
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(present, "{table} records its owning session");
    }
    assert!(
        root_backups(&case)
            .iter()
            .any(|name| name.starts_with("3-to-4-")),
        "the upgrade kept a backup"
    );
    drop(db);
    let phases = case.phases();
    assert_eq!(phases.len(), 3);
    assert_eq!(phases[0].1, "parked");
    assert_eq!(
        phases[1],
        (first.session_id.as_str().into(), "parked".into())
    );
    assert_eq!(
        phases[2],
        (second.session_id.as_str().into(), "configured".into())
    );
    assert_eq!(
        case.resume("resume-upgraded", &first.session_id, false)
            .await,
        OwnerResult::Applied
    );
    let resumed = case.shown().await;
    assert_eq!(resumed.session_id, first.session_id);
    assert_eq!(resumed.phase, MasterSessionPhase::Configured);
    assert_eq!(case.turns(&first.session_id).await.len(), 1);
    case.close().await;
}

fn root_backups(case: &Case) -> Vec<String> {
    std::fs::read_dir(case.root.path().join(crate::storage::migrations::BACKUPS))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_str().unwrap().to_owned())
        .collect()
}

fn git(folder: &Path, args: &[&str]) -> String {
    let output = std::process::Command::new("git")
        .args([
            "-c",
            "user.name=Session Test",
            "-c",
            "user.email=session@test.invalid",
        ])
        .args(args)
        .current_dir(folder)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

#[tokio::test]
async fn parking_switching_and_new_sessions_never_touch_a_dirty_master_worktree() {
    let case = Case::new().await;
    let repository = case.root.path().join("repository");
    git(&repository, &["init", "-q", "-b", "main"]);
    git(&repository, &["add", "README.md"]);
    git(&repository, &["commit", "-q", "-m", "start"]);
    let worktree = case.root.path().join("master-worktree");
    git(
        &repository,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "master-work",
            worktree.to_str().unwrap(),
        ],
    );
    std::fs::write(worktree.join("draft.md"), "Unsaved master draft\n").unwrap();
    std::fs::write(worktree.join("README.md"), "Edited by the master\n").unwrap();
    let dirty = git(&worktree, &["status", "--porcelain"]);
    assert!(
        dirty.contains("draft.md") && dirty.contains("README.md"),
        "{dirty}"
    );

    assert_eq!(case.open("open-dirty").await, OwnerResult::Applied);
    let first = case.shown().await;
    assert_eq!(case.renew("renew-dirty", false).await, OwnerResult::Applied);
    let second = case.shown().await;
    assert_ne!(second.session_id, first.session_id);
    assert_eq!(
        case.resume("resume-dirty", &first.session_id, false).await,
        OwnerResult::Applied
    );
    assert_eq!(
        case.master(
            "park-dirty",
            MasterOwnerAction::Close {
                project_id: case.project.clone(),
                session_id: first.session_id.clone(),
            },
        )
        .await,
        OwnerResult::Applied
    );
    assert_eq!(live_count(&case.phases()), 0);

    assert_eq!(
        std::fs::read_to_string(worktree.join("draft.md")).unwrap(),
        "Unsaved master draft\n"
    );
    assert_eq!(
        std::fs::read_to_string(worktree.join("README.md")).unwrap(),
        "Edited by the master\n"
    );
    assert_eq!(git(&worktree, &["status", "--porcelain"]), dirty);
    let listed = git(&repository, &["worktree", "list", "--porcelain"]);
    assert!(listed.contains(worktree.to_str().unwrap()), "{listed}");
    case.close().await;
}

#[tokio::test]
async fn resume_refuses_a_session_the_current_settings_or_engine_run_no_longer_admit() {
    let case = Case::new().await;
    assert_eq!(case.open("open-rcli").await, OwnerResult::Applied);
    let rcli = case.shown().await;
    assert_eq!(
        case.renew_with("renew-native", false, MasterBackend::Claude)
            .await,
        OwnerResult::Applied
    );
    let native = case.shown().await;
    assert_eq!(case.renew("renew-last", false).await, OwnerResult::Applied);
    let case = case.reopen().await;

    let before = snapshot(&case);
    assert_eq!(
        case.resume("resume-stale-rcli", &rcli.session_id, false)
            .await,
        OwnerResult::Rejected {
            code: "master_source_conflict".into()
        },
        "an RCLI session cannot move to a new engine run, so a send would refuse it too"
    );
    assert_eq!(snapshot(&case), before, "a refused Resume changes nothing");

    assert_eq!(
        case.resume("resume-native", &native.session_id, false)
            .await,
        OwnerResult::Applied,
        "a native session moves to the new engine run as a send does"
    );
    assert_eq!(case.shown().await.session_id, native.session_id);
    assert_eq!(
        case.renew_with("renew-after-native", false, MasterBackend::Claude)
            .await,
        OwnerResult::Applied
    );

    let changed = CONFIG.replace("configured-claude-model", "replaced-claude-model");
    assert_ne!(changed, CONFIG);
    let case = case.reopen_with(Some(&changed)).await;
    let before = snapshot(&case);
    assert_eq!(
        case.resume("resume-out-of-scope", &native.session_id, false)
            .await,
        OwnerResult::Rejected {
            code: "master_source_conflict".into()
        },
        "the session's own pool changed, so it no longer runs under the current settings"
    );
    assert_eq!(snapshot(&case), before, "a refused Resume changes nothing");
    case.close().await;
}

#[tokio::test]
async fn a_parked_session_s_owner_messages_are_read_back_for_its_turns_only() {
    let case = Case::new().await;
    assert_eq!(case.open("open-history").await, OwnerResult::Applied);
    let first = case.shown().await;
    assert_eq!(
        case.submit("history-message", &first.session_id, "Plan the first slice")
            .await,
        OwnerResult::Applied
    );
    assert_eq!(
        case.renew("renew-history", true).await,
        OwnerResult::Applied
    );
    let second = case.shown().await;
    assert_eq!(
        case.submit("other-message", &second.session_id, "Add a dark theme")
            .await,
        OwnerResult::Applied
    );
    let own = case.turns(&first.session_id).await[0].input_cursor.clone();
    let other = case.turns(&second.session_id).await[0].input_cursor.clone();
    let read = |cursors: Vec<Cursor>| {
        case.read(OwnerRead::ProjectMasterInputs {
            project_id: case.project.clone(),
            session_id: first.session_id.clone(),
            cursors,
        })
    };
    let OwnerData::ProjectMasterInputs { entries } =
        read(vec![other.clone(), own.clone()]).await.data
    else {
        panic!("the owner messages are read");
    };
    assert_eq!(
        entries.len(),
        1,
        "another session's message is not this session's history"
    );
    assert_eq!(entries[0].event_cursor, own);
    assert_eq!(entries[0].text, "Plan the first slice");

    let too_many = vec![own.clone(); usize::from(crate::conversation::PAGE_ITEMS_MAX) + 1];
    let (send, receive) = oneshot::channel();
    case.storage
        .sender
        .try_send(Job::OwnerRead(
            case.started.owners[0].admission.clone(),
            OwnerRead::ProjectMasterInputs {
                project_id: case.project.clone(),
                session_id: first.session_id.clone(),
                cursors: too_many,
            },
            send,
        ))
        .unwrap_or_else(|_| panic!("owner read queue unavailable"));
    assert_eq!(
        receive.await.unwrap().unwrap_err().0,
        "master_inputs_bounds"
    );
    case.close().await;
}
